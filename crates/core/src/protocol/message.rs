//! [`NetMessage`], the one enum every message on the wire is a variant of.
//! Its variant order is the wire (`wire_shape.rs`).

use glam::Vec3;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use super::*;
use crate::particles::ParticleEffect;
use crate::sounds::PrimSound;
use crate::tone::{HandPose, ToneSpec};

/// Chat scope a client sends into (ADR-028). The server resolves it to a concrete
/// channel. Geo tiers (country/province/municipality) land with `cartopolis_geo`.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub enum ChatScope {
    /// Proximity chat; `shout` widens the radius. Ephemeral, never stored.
    Local { shout: bool },
    /// Everyone online.
    Global,
}

/// Credential presented in the [`NetMessage::ClientHello`] handshake (ADR-016).
/// The server verifies it against its local account store and issues a session.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub enum AuthCredential {
    /// A previously issued session token (silent reconnect).
    Token(String),
    /// Username + password. Verified server-side with argon2; never stored
    /// client-side (persist the returned session token instead).
    Password { username: String, password: String },
    /// A guest with a call link (protocol 41): the link's `code` and the name
    /// they gave. Makes an account that may only join that link's call and
    /// lasts [`CALL_GUEST_HOURS`]; the returned session token reconnects it.
    Guest { code: String, name: String },
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub enum NetMessage {
    ClientHello {
        protocol_version: u16,
        /// ADR-016: credential the server authenticates before any world traffic.
        auth: AuthCredential,
    },
    ServerHelloAck {
        session_id: Uuid,
        tick_hz: f32,
        your_avatar_id: u64,
        /// ADR-016: stable account id bound to this connection.
        principal_id: Uuid,
        /// ADR-016: authoritative account name for this principal (display/UI).
        username: String,
        /// ADR-016: opaque session token to persist for silent reconnect.
        session_token: String,
        /// ADR-014: operator tile URL; `{z}/{x}/{y}` placeholders. Empty = client default.
        #[serde(default)]
        osm_tile_url_template: String,
        /// Where this server's HTTP surface is, for the client to fetch model
        /// assets from (protocol v22). Empty means "work it out from the address
        /// you connected to".
        ///
        /// **The server has to say, because the client cannot derive it.** The
        /// wire arrives over a TCP port or a WebSocket path, and the HTTP listener
        /// is a separate bind that an operator points a reverse proxy at — on the
        /// deployed sim the two are different ports behind different hostnames,
        /// and against a development sim on this machine the HTTP port is whatever
        /// `--http-listen` was given. Guessing produces a client that silently
        /// fetches nothing.
        ///
        /// It is the same shape of answer as `osm_tile_url_template` beside it:
        /// the server telling a client where its services are, rather than the
        /// client assuming.
        #[serde(default)]
        api_base_url: String,
        /// This server's name (protocol 37), the host of its public URL: what
        /// makes `username` a full address (`username@server_name`) and what a
        /// conversation created here begins its id with (`address::group_id`).
        /// Every identity inside an encrypted chat is a full address, so that a
        /// chat can one day reach people on other servers (`crate::address`).
        server_name: String,
        /// Where this account's avatar stood when its last session ended
        /// (protocol 46), or `None` if it has never stood anywhere.
        ///
        /// **The client places the avatar, not the sim.** The sim spawns every
        /// avatar at the origin of its client's map frame, which is wherever that
        /// map happens to be anchored — the last long jump, the last pan at
        /// altitude — so a sign-in used to land the avatar somewhere that looked
        /// random. The sim cannot do better on its own: it does not know the
        /// client's anchor. So it says where, and the client moves the avatar
        /// there the way a teleport does (`session::keep_avatar_across_reconnect`).
        last_place: Option<GeoAnchor>,
    },
    ServerError {
        request_id: u32,
        code: u32,
        message: String,
    },
    ClientIntent {
        request_id: u32,
        move_x: f32,
        move_z: f32,
        /// World yaw replicated to others (travel facing — the direction the avatar
        /// is turned toward). Server applies this instead of `atan2(velocity)`.
        display_yaw: f32,
        /// Avatar flight, from protocol 12 until it was removed on 2026-11-14.
        ///
        /// **Ignored by the sim, and clients send `false`.** The avatar walks,
        /// runs and jumps; an old client that still sets this is simply fallen
        /// by the sim like everyone else, and its flight-speed wish clamps at
        /// `motion::MAX_SPEED`. Kept on the wire because removing a field is a
        /// protocol revision, and these three cost three bytes.
        flying: bool,
        /// Climb and descend for `flying`; ignored by the sim, `false` from
        /// clients, kept for the same reason.
        fly_up: bool,
        fly_down: bool,
        /// One-shot: the client asked to jump this frame. The sim grants it only
        /// from the ground, so this is a *request*, not an impulse — a client that
        /// holds the bit down does not climb.
        jump: bool,
        /// One-shot: an emote the avatar started this frame, replicated to
        /// everyone in range through [`AvatarStateDto::emote`]. `None` on almost
        /// every intent.
        emote: Option<Emote>,
        /// One-shot: an expression the face started this frame (protocol 44),
        /// replicated as [`AvatarStateDto::expression`]; like `emote`.
        expression: Option<Expression>,
        /// Where the avatar is sitting, as metres from its own position — see
        /// [`AvatarStateDto::seated`] for why it is an offset. A *level*, not an
        /// edge: it is set for as long as the sit lasts, so an avatar that comes
        /// into range mid-sit is drawn sitting. The sim clears it the moment the
        /// same intent asks to move.
        seated: Option<[f32; 2]>,
        /// Where this client believes its avatar stands on Earth (protocol v20).
        ///
        /// Since protocol 47 the sim holds the avatar's place itself
        /// ([`AvatarStateDto::geo`]) and moves it by the intent's velocity, so
        /// this is read for one thing: **placing an avatar that has no place
        /// yet.** A connection's avatar is nowhere until its client says where
        /// it is — the first intent carrying a `geo`, or a `Teleport` — and until
        /// then it is in nobody's snapshot and in earshot of nobody, which is how
        /// a web UI that only chats stays out of the world. On a placed avatar
        /// the field is ignored: the authority is the sim's, and a client wanting
        /// to be elsewhere teleports.
        ///
        /// `None` from a client that does not know yet (before the map has
        /// anchored).
        geo: Option<GeoAnchor>,
    },
    /// The avatars within this connection's reach, once a tick (protocol 47:
    /// assembled per connection from the sim's spatial index, so what arrives
    /// is what the receiving avatar could see or hear — not everyone on the
    /// server). Things do not travel in it: a thing arrives as `PrimUpsert`
    /// when it comes within reach and leaves as [`Self::PrimOutOfReach`].
    ///
    /// The first one a connection receives, at the handshake, is empty and says
    /// the connection is live; an avatar absent from a later one has left reach
    /// or the world, and the receiver forgets it.
    WorldSnapshot {
        tick: u64,
        avatars: Vec<AvatarStateDto>,
    },
    /// ADR-011 delta v0: remove prim by stable id (server-assigned).
    PrimRemoved { id: i64 },
    /// A thing left this connection's reach (protocol 47): forget it, as the
    /// receiver forgets an avatar missing from a snapshot. Not a deletion — the
    /// thing still exists and arrives again as `PrimUpsert` when it comes back
    /// within reach — which is why it is not a `PrimRemoved`: a client that
    /// treated the two alike would have no way to tell a thing it may build
    /// against from one that is gone.
    PrimOutOfReach { id: i64 },
    /// ADR-017 / ADR-018: client requests creation of a new prim with full initial state.
    CreatePrim {
        request_id: u32,
        name: String,
        position: Vec3,
        rotation: Vec3,
        scale: Vec3,
        color: [f32; 3],
        texture_id: Option<String>,
        shape: String,
        #[serde(default)]
        surface: PrimSurface,
        #[serde(default)]
        geometry: PrimGeometry,
        /// Absolute geo placement (ADR-029).
        geo: GeoAnchor,
        /// A particle emitter to author onto the prim (protocol v20).
        #[serde(default)]
        particles: Option<ParticleEffect>,
        /// A stored model asset to draw instead of `shape` (protocol v22).
        #[serde(default)]
        model: Option<String>,
        /// Make it a draft only its maker sees (protocol 51): what the design
        /// room builds, until it is placed.
        #[serde(default)]
        draft: bool,
    },
    /// ADR-017: client sends updated prim state (position, rotation, scale, color, texture, name).
    UpdatePrim {
        request_id: u32,
        prim_id: i64,
        position: Vec3,
        rotation: Vec3,
        scale: Vec3,
        color: [f32; 3],
        texture_id: Option<String>,
        name: String,
        /// Texture surface params (ADR-017 / protocol v8).
        #[serde(default)]
        surface: PrimSurface,
        /// Geometry deformation params (ADR-018 / protocol v8).
        #[serde(default)]
        geometry: PrimGeometry,
        /// Absolute geo placement (ADR-029).
        geo: GeoAnchor,
        /// The prim's particle emitter, or `None` to take it off (protocol v20).
        #[serde(default)]
        particles: Option<ParticleEffect>,
        /// The prim's model asset, or `None` to go back to the procedural shape
        /// (protocol v22).
        #[serde(default)]
        model: Option<String>,
    },
    /// ADR-017: client requests deletion of a prim by id.
    DeletePrim { request_id: u32, prim_id: i64 },
    /// ADR-017: server broadcasts the full authoritative prim state after a create or update.
    PrimUpsert { prim: PrimDto },
    /// Client requests the full texture catalog from the sim.
    TextureCatalogRequest,
    /// Sim responds with the list of available textures.
    TextureCatalog { textures: Vec<TextureEntry> },
    /// Client requests raw PNG bytes for a specific texture.
    TextureRequest { request_id: u32, texture_id: String },
    /// Sim sends raw PNG bytes for the requested texture.
    TextureData {
        request_id: u32,
        texture_id: String,
        png_bytes: Vec<u8>,
    },
    /// Sim signals that the requested texture id does not exist.
    TextureNotFound { request_id: u32, texture_id: String },
    /// ADR-028: client sends a chat message in a scope; the server resolves the
    /// concrete channel, applies rate limits, and delivers it.
    ChatSend {
        request_id: u32,
        scope: ChatScope,
        body: String,
    },
    /// ADR-028: server delivers a chat message on a resolved channel. `distance_m`
    /// is `Some` only for `Local` (proximity) messages.
    ChatMessage {
        channel: String,
        sender_id: u64,
        sender_name: String,
        sent_at_ms: u64,
        body: String,
        distance_m: Option<f32>,
    },
    /// Fetch a prim's script source for the editor. Scripts are *not* carried in
    /// `WorldSnapshot`: source is authoring data, an order of magnitude larger
    /// than the rest of a `PrimDto`, and it would be re-sent to every observer
    /// 20 times a second for the benefit of the one person editing it.
    GetPrimScript { request_id: u32, prim_id: i64 },
    /// Store (or, with an empty `source`, remove) a prim's script. The server
    /// compiles it and replies with a [`NetMessage::ScriptOutput`] carrying either
    /// the compile error or a ready line.
    SetPrimScript {
        request_id: u32,
        prim_id: i64,
        source: String,
    },
    /// Server's answer to [`NetMessage::GetPrimScript`], and the broadcast after a
    /// successful `SetPrimScript` so a second editor on the same prim updates.
    PrimScript {
        prim_id: i64,
        source: String,
        /// Whether the host currently holds a live, compiled instance.
        running: bool,
    },
    /// A line from a script: `log`, a compile error, or a runtime fault. This
    /// is the script editor's console, and the only channel a broken script has.
    ScriptOutput {
        prim_id: i64,
        level: ScriptLevel,
        text: String,
        at_ms: u64,
    },
    /// Client touched a prim; the server turns it into the script's `on_touch`.
    TouchPrim { request_id: u32, prim_id: i64 },
    /// The world's resource budget and current usage. Sent on connect and whenever
    /// a prim or script is added or removed, so the editor can show the headroom
    /// *before* an author hits the wall rather than as a refusal afterwards.
    ///
    /// World-wide since protocol 23. It was per-region, on the reasoning that a
    /// region is the unit an operator hands out — but a simulator only ever ran
    /// one, so the ceiling was already the server's, and the limits now live in
    /// `cartopolis.toml` where the rest of an operator's decisions are.
    WorldCapacity {
        prims_used: u32,
        prims_max: u32,
        scripts_used: u32,
        scripts_max: u32,
        /// Per-script memory ceiling in bytes: how far one script may grow the
        /// heap in one slice (`scripting::SCRIPT_MEMORY_BYTES`).
        script_memory_bytes: u32,
    },
    /// Client sends one encoded audio frame into a scope. Voice reuses the chat
    /// scopes and the chat fan-out; the sim never inspects `payload` (Opus today,
    /// but the codec is entirely a client concern).
    ///
    /// Fire-and-forget: no `request_id`, because there is no reply and a late
    /// frame is worth less than a dropped one. `seq` is the sender's own frame
    /// counter, carried so the receiver can order and drop — the eventual
    /// transport is unreliable, unordered datagrams, where arrival order is not
    /// send order, so sequencing has to live in the message rather than be
    /// inferred from the stream.
    VoiceFrame {
        scope: ChatScope,
        seq: u32,
        payload: Vec<u8>,
    },
    /// Server forwards an audio frame to one recipient. `distance_m` is `Some`
    /// only for `Local` (proximity), and is what the client attenuates by.
    ///
    /// Carries no sender name: at ~50 frames/second that string would dominate
    /// the packet, and the client already has names from avatar replication.
    VoiceData {
        channel: String,
        sender_id: u64,
        seq: u32,
        payload: Vec<u8>,
        distance_m: Option<f32>,
    },
    /// Block (`blocked = true`) or unblock a person, by username.
    ///
    /// **By username, not by `sender_id`.** An avatar id is per-connection and is
    /// reused; a block that is worth persisting has to name something that
    /// survives a reconnect, and the account username is the only stable, unique
    /// public identity the wire carries (`accounts.username` is `UNIQUE`, and
    /// `display_name` is a copy of it).
    ///
    /// The server answers with a [`NetMessage::BlockedList`] rather than an ack,
    /// so the client's list is always the server's list — a client that applied
    /// its own edit optimistically would diverge the moment one was refused.
    SetBlocked {
        request_id: u32,
        username: String,
        blocked: bool,
    },
    /// The full set of people this account blocks. Sent once after the handshake
    /// and again after every accepted [`NetMessage::SetBlocked`].
    ///
    /// Whole list, not a delta: it is a handful of short strings, it makes the
    /// login and the update path the same code, and a lost delta would leave a
    /// blocked person audible with nothing to say why.
    BlockedList { usernames: Vec<String> },
    /// Report a person to the operators (App Store guideline 1.2).
    ///
    /// `message` is the offending line as the reporter saw it, which is the only
    /// copy that survives for proximity chat — `Local` is never stored, so
    /// without it a report about the thing that actually happened arrives with no
    /// evidence attached. It is also the *unmasked* text where the filter hit,
    /// which is the point: masking is for the room, not for the operator.
    ReportUser {
        request_id: u32,
        username: String,
        reason: ReportReason,
        /// The reporter's own words. May be empty.
        note: String,
        /// The reported message, if the report was raised from a chat line.
        message: Option<String>,
        /// The model asset the report is about, if it was raised from an object
        /// rather than from a person or a chat line (protocol v22).
        ///
        /// The subject stays a username — the uploader is still who an operator
        /// may need to act on — and this rides alongside so that acting on one
        /// report reaches every prim wearing that file, rather than the single one
        /// the reporter happened to be standing in front of.
        #[serde(default)]
        asset: Option<String>,
    },
    /// Server confirms a report was filed. `accepted = false` means it was
    /// refused (unknown subject, or reporting yourself) — the client says so
    /// rather than showing a silent success.
    ReportAck { request_id: u32, accepted: bool },
    /// Delete the signed-in account and everything personal attached to it
    /// (App Store guideline 5.1.1(v): an app with accounts must offer deletion
    /// *within the app*).
    ///
    /// The password is required again even though the connection is already
    /// authenticated: this is the one irreversible thing a session token can ask
    /// for, and a token can be a remembered one from a device somebody else is
    /// holding.
    DeleteAccount { request_id: u32, password: String },
    /// The account is gone. The server closes the connection immediately after
    /// sending this, so it is both the receipt and the goodbye.
    AccountDeleted,
    /// Client asks to be *placed* — the counterpart of `ClientIntent`, which can
    /// only ever ask to move. A jump to a searched place, a bookmark, a shared
    /// link or a distant GPS fix used to move only the local entity; the next
    /// snapshot pulled the avatar straight back, so online every teleport in
    /// the client silently did not happen. The sim sets the position and clears
    /// the motion state, and the next `WorldSnapshot` is the confirmation —
    /// there is no ack, exactly as with intents.
    ///
    /// `geo` and `alt_m` are the place on Earth [`AvatarStateDto`] answers with
    /// (protocol 47). Trusted like an intent is trusted (the sim has no map to
    /// validate a destination against); a non-finite or off-globe coordinate
    /// discards the whole message, same rule as `apply_intent`.
    Teleport {
        request_id: u32,
        geo: GeoAnchor,
        alt_m: f32,
    },
    /// Client says whether it is being *looked at*.
    ///
    /// `active: false` means the app is off screen (Android's background
    /// connection, `systems::background`) and the server should stop sending it
    /// `WorldSnapshot`s. That is the whole of it: the connection stays open, chat,
    /// voice and every other message still arrive, and the client keeps its
    /// session, its place in the world and its `ChatHub` registration.
    ///
    /// **Why the snapshot and nothing else.** A backgrounded phone draws nothing,
    /// so a world state it cannot render is the one thing on this wire with no
    /// consumer — and it is also by far the largest and the most frequent, at the
    /// tick rate. Dropping it is what makes a held connection cheap enough to
    /// hold; keeping everything else is what makes holding it worth doing, since
    /// the reason to stay connected is to be reachable.
    ///
    /// **The client is trusted here, and the cost of a lie is its own.** A client
    /// that claims to be idle simply stops being told where anybody is; it cannot
    /// use that to see more than it should, and its own avatar keeps being
    /// simulated and broadcast to everyone else exactly as before. So this needs
    /// no validation beyond the flag itself.
    ///
    /// Resuming is the same message with `active: true`, and the next tick's
    /// snapshot is the resynchronisation — there is no catch-up and no ack, for
    /// the same reason `Teleport` has none: the next snapshot is the whole state.
    SetPresence { request_id: u32, active: bool },

    // ── Private chats (protocol 25, `docs/notes/encrypted-chat.md`) ──────────
    //
    // Everything below carries *sealed* bytes. The server is the MLS delivery
    // service: it keeps each device's one-use key packages, an ordered log per
    // chat, and who is in which chat so it knows whom to deliver to. It cannot
    // open any of it, and `cartopolis_simulator` does not link the crate that
    // could. What it does see is who talks to whom, when, and how much.
    /// Client registers this installation's device for its account, and tops
    /// up its stock of key packages (each can be used once, to add this device
    /// to one chat). Idempotent: sent on every connect.
    E2eeRegisterDevice {
        request_id: u32,
        device: Uuid,
        key_packages: Vec<Vec<u8>>,
    },
    /// Server: how many unused key packages this device has left, so the
    /// client knows how many to make next time.
    E2eeDeviceStatus { request_id: u32, key_packages: u32 },
    /// Client asks for one key package per device of `address` — a full
    /// `name@server` (protocol 37) — to add them to a chat. Each package handed
    /// out is deleted: it opens one invitation. Asking for your own address
    /// returns your *other* devices.
    E2eeClaimKeyPackages { request_id: u32, address: String },
    /// Server: the packages, one per device that had one left. A device with
    /// none left is simply absent — it can be added later, when it tops up.
    E2eeKeyPackages {
        request_id: u32,
        address: String,
        packages: Vec<E2eeKeyPackage>,
    },
    /// Client registers a new chat's id with the server, with the sender as
    /// its only member. The id begins with this server's name
    /// (`address::group_id`): a chat's log is ordered by the server it was
    /// created on. `direct_with` is the other person's address for a direct
    /// chat, so the server can refuse a second direct chat between the same
    /// two accounts and point at the first instead.
    E2eeCreateGroup {
        request_id: u32,
        group: Vec<u8>,
        direct_with: Option<String>,
    },
    /// Server: the chat exists. `existing` is set when a direct chat between
    /// these two people was already there; the client then uses that one.
    E2eeGroupCreated {
        request_id: u32,
        group: Vec<u8>,
        existing: Option<Vec<u8>>,
    },
    /// Client appends a sealed entry to a chat's log.
    ///
    /// A `commit` (a membership change) is accepted only if `epoch` is the
    /// chat's current epoch; the second of two simultaneous commits is refused
    /// and its sender retries after catching up. That is the whole of the
    /// server's part in keeping everyone's view of the chat identical.
    E2eeSend {
        request_id: u32,
        group: Vec<u8>,
        epoch: u64,
        commit: bool,
        message: Vec<u8>,
        /// For a commit that adds people: the invitation and whom it is for.
        welcome: Option<E2eeWelcome>,
    },
    /// Server: the entry's place in the log, or why it was refused.
    E2eeSent {
        request_id: u32,
        group: Vec<u8>,
        seq: Option<u64>,
        refused: Option<String>,
    },
    /// Client asks for everything it has not seen: entries after each cursor,
    /// and any invitations waiting for this device. An invitation stops being
    /// resent once its chat appears in `cursors` (the device has joined).
    E2eeSync {
        request_id: u32,
        cursors: Vec<E2eeCursor>,
    },
    /// Server: the end of a sync's replay. Everything before it has been sent.
    E2eeSynced { request_id: u32 },
    /// Server: one log entry, live or replayed. The client skips entries its own
    /// device wrote, and processes the rest strictly in `seq` order.
    E2eeDeliver {
        group: Vec<u8>,
        seq: u64,
        /// The sender's full address.
        sender: String,
        device: Uuid,
        sent_at_ms: u64,
        commit: bool,
        message: Vec<u8>,
    },
    /// Server: an invitation for this device. Join it, then read the chat's log
    /// from after `after`, the commit that added this device.
    E2eeInvite {
        group: Vec<u8>,
        /// The inviter's full address.
        from: String,
        after: u64,
        welcome: Vec<u8>,
    },

    // ── Calls (protocol 26, `docs/notes/calls.md`) ───────────────────────────
    //
    // Signalling only. The media goes over UDP to the simulator's forwarder,
    // sealed on the sending device with a key from the chat's MLS group, so
    // what the server learns here is what it already knew from the chat —
    // who is in it — plus who is in a call and when. Each device keeps two
    // connections to the forwarder, and each is offered by the side that sends
    // on it: `CallJoin` carries the device's offer for what it sends,
    // `CallOffer` the forwarder's for what it will receive.
    /// Client joins (or starts) the call of private chat `group` with the SDP
    /// offer for its sending connection. Only a member of the chat with a
    /// registered device may join.
    CallJoin {
        request_id: u32,
        group: Vec<u8>,
        offer: String,
        /// The media id in `offer` that carries a shared screen (protocol 41).
        /// Offered from the start, like the camera, so sharing later needs no
        /// renegotiation; `None` from a device that cannot share.
        screen: Option<String>,
    },
    /// Server: in the call, with the forwarder's answer. `participant` is this
    /// device's number in the call, which [`NetMessage::CallRoster`] and
    /// [`CallTrack`] name people by.
    CallJoined {
        request_id: u32,
        group: Vec<u8>,
        participant: u64,
        answer: String,
    },
    /// Server: an offer for this device's receiving connection — somebody's
    /// microphone or camera was added or went away. Answered by `CallAnswer`.
    CallOffer {
        group: Vec<u8>,
        sdp: String,
        tracks: Vec<CallTrack>,
    },
    /// Client: the answer to the last `CallOffer`.
    CallAnswer { group: Vec<u8>, sdp: String },
    /// Client leaves the call. Closing the connection leaves it too.
    CallLeave { group: Vec<u8> },
    /// Server: who is in a chat's call, sent to every member's connected
    /// devices whenever it changes. A roster this device is not in is the call
    /// ringing; an empty one is the call over.
    CallRoster {
        group: Vec<u8>,
        members: Vec<CallMember>,
        /// What the hosts allow (protocol 41).
        settings: CallSettings,
        /// Devices waiting to be let in (protocol 41).
        waiting: Vec<CallWaiter>,
        /// Who the hosts show large for everybody.
        spotlight: Option<u64>,
        /// The open breakout rooms, if any.
        rooms: Option<CallRooms>,
    },

    // ── Contacts and message requests (protocol 27, `docs/notes/contacts.md`) ─
    //
    // A contact is somebody you accepted, both ways round. Anybody else reaches
    // you by a *request*: one knock, which you accept, decline or ignore. The
    // server is what refuses — who may send you one, how many a new account may
    // send, and that a private chat with somebody who is not a contact holds a
    // single message until they accept — because a check in the client is a
    // check a modified client skips.
    /// Server: everything about this account's contacts, sent on connect and
    /// again after anything in it changes (including by the other person).
    /// The whole state rather than a delta, like `BlockedList`.
    ContactsState {
        contacts: Vec<ContactDto>,
        /// Requests waiting for this account's answer.
        incoming: Vec<ContactRequestDto>,
        /// Requests this account sent that have not been accepted. A declined
        /// one is listed here exactly as a waiting one is: a decline is silent.
        outgoing: Vec<ContactRequestDto>,
        settings: ContactSettings,
    },
    /// Client asks to become contacts with `username`. `link_code` is the code
    /// from their contact link, which is accepted whatever their setting says.
    /// Answered with a `ContactsState`, or a `ServerError` saying why not.
    RequestContact {
        request_id: u32,
        username: String,
        link_code: Option<String>,
    },
    /// Client accepts or declines a request from `username`.
    AnswerContact {
        request_id: u32,
        username: String,
        accept: bool,
    },
    /// Client stops being contacts with `username`, or withdraws a request it
    /// sent them. Not a block: they can send a request again.
    RemoveContact { request_id: u32, username: String },
    /// Client changes who may reach it. `reset_link` issues a new link code,
    /// so every copy of the old link stops working.
    SetContactSettings {
        request_id: u32,
        policy: RequestPolicy,
        show_online: bool,
        notify_requests: bool,
        separate_new_accounts: bool,
        reset_link: bool,
    },
    /// Client asks about one person: by exact username, or by the avatar it
    /// tapped (the client is never told whose an avatar is until it asks).
    LookupPerson {
        request_id: u32,
        username: Option<String>,
        avatar_id: Option<u64>,
    },
    /// Server: the answer. `person` is `None` when there is nobody to show —
    /// no such account, or one that asked not to be found by name.
    PersonInfo {
        request_id: u32,
        person: Option<PersonDto>,
    },
    /// Client asks who is near its avatar now, for the contacts screen.
    GetNearbyPeople { request_id: u32 },
    /// Server: people near the asker's avatar, nearest first.
    NearbyPeople {
        request_id: u32,
        people: Vec<NearbyPersonDto>,
    },

    // ── Video on a prim (protocol 28, `docs/notes/video-on-a-prim.md`) ───────
    /// Client starts a clip on a prim it may edit (`video: Some`), or stops the
    /// one playing (`None`). The clip must be in the simulator's video store;
    /// the server stamps the start time and answers with the prim's
    /// `PrimUpsert` to everyone, or a `ServerError` to the sender.
    SetPrimVideo {
        request_id: u32,
        prim_id: i64,
        video: Option<SetVideo>,
    },
    /// Client: where to wake this device when it is not connected — a
    /// UnifiedPush endpoint (an `https:` URL its distributor gave it) — or
    /// `None` to stop. The server posts an empty wake-up there when a call
    /// starts in one of the account's chats; nothing about the call is in it.
    SetPushEndpoint { endpoint: Option<String> },

    // ── Where the people in a chat are (protocol 30) ─────────────────────────
    /// Client asks where these people's avatars are, for a private chat's World
    /// view. Asked again every few seconds while that view is open.
    WherePeople {
        request_id: u32,
        usernames: Vec<String>,
    },
    /// Server: the ones the asker may see — contacts who show that they are
    /// online, whose avatar is in the world with a location. Anybody else is
    /// left out, never marked as hidden, so the answer says nothing about them.
    PeopleWhere {
        request_id: u32,
        people: Vec<PersonWhereDto>,
    },
    /// Client: this device declines the call ringing in `group`.
    CallDecline { group: Vec<u8> },
    /// Server: `username` declined the call in `group` — to the chat's
    /// connected devices: the caller's, which stop ringing out, and the
    /// decliner's others, which stop ringing.
    CallDeclined { group: Vec<u8>, username: String },
    /// Server: a script on `prim_id` played a one-shot sound (`play_sound`, protocol
    /// 33). Sent to every connection: the client knows where the prim is in its own
    /// frame and the server does not, so the client measures the distance and
    /// drops it when out of `sound.range_m`. A one-shot is an edge, not a level —
    /// somebody arriving a second later does not hear a bang that already happened.
    PrimSound { prim_id: i64, sound: PrimSound },

    // ── Physical prims (protocol 34, `docs/notes/physical-prims.md`) ─────────
    /// Client switches a prim it may edit between physical (the simulator moves
    /// it: it falls, collides and can be pushed) and not. Its own message, as
    /// `SetPrimVideo` is, because `UpdatePrim` is a full-state edit every tool
    /// sends and a tool that forgot the flag would drop every ball it touched.
    /// Answered with the prim's `PrimUpsert` to everyone, or a `ServerError`.
    SetPrimPhysical {
        request_id: u32,
        prim_id: i64,
        physical: bool,
    },
    /// Server: where the physics step has moved the physical prims that moved
    /// this tick. Sent instead of a `PrimUpsert` per prim per tick, which would
    /// carry every field of the prim and make the client rebuild its mesh twenty
    /// times a second; a body is ~50 bytes. A body that has come to rest is sent
    /// once more with `resting` set, and then not until it moves again.
    PrimMotion { tick: u64, bodies: Vec<PrimBody> },
    /// Client: pick up this physical prim (protocol 48). Anyone may, as anyone
    /// may kick the ball; the sim refuses with a `ServerError` a prim that is not
    /// physical, too big or heavy to carry (`carry::can_carry`), out of reach of
    /// where the sim holds the avatar, or already in somebody's hands. Granted,
    /// it shows as `AvatarStateDto::holding` and the prim follows the avatar.
    GrabPrim { request_id: u32, prim_id: i64 },
    /// Client: let go of whatever this avatar holds (protocol 48) — thrown
    /// forward and up from the hands when `throw`, set down at the feet when
    /// not. No speed on the wire: the sim throws at its own speed along the
    /// avatar's facing, plus the avatar's own velocity.
    ReleasePrim { request_id: u32, throw: bool },
    /// Client: offer `greeting` to the avatar `to` (protocol 49). Refused with
    /// a `ServerError` when `to` is this avatar, is nowhere, is out of
    /// [`crate::greet::GREET_REACH_M`] of where the sim holds this one, either
    /// is seated, or either has blocked the other; rate limited. Granted, `to`
    /// is sent `GreetingOffered`; the offer lapses after
    /// [`crate::greet::OFFER_SECS`], and is withdrawn if the offerer moves.
    OfferGreeting {
        request_id: u32,
        to: u64,
        greeting: crate::greet::Greeting,
    },
    /// Server: the avatar `from` (signed in as `name`) offers `greeting` to
    /// this one — or, with `greeting: None`, no longer does (it moved).
    GreetingOffered {
        from: u64,
        name: String,
        greeting: Option<crate::greet::Greeting>,
    },
    /// Client: take the greeting `from` offered. The sim checks the offer is
    /// still open and the two still within reach, stands them together and
    /// starts it on both; otherwise a `ServerError`.
    AcceptGreeting { request_id: u32, from: u64 },
    /// Server, to both: the greeting has started. Turn to `yaw` (the sim has
    /// turned this avatar there and holds it while the gesture plays) and play
    /// it; `partner` is the other avatar. Everyone else sees it as both
    /// avatars' `emote`, with the same sequence, in the same snapshot.
    GreetingStarted {
        greeting: crate::greet::Greeting,
        partner: u64,
        yaw: f32,
    },

    // ── Following a place for new plans (protocol 50, `docs/notes/plans.md`) ──
    /// Client: the spots this account follows, `[lat, lng]`, replacing what the
    /// server held. The server checks every new notice in the municipality's
    /// feed against them and wakes the account's closed phones for one within
    /// 100 m (`simulator::plans_watch`). Sent once a connection when the list
    /// is not empty, and on every change, so a device that follows nothing
    /// does not clear what another device follows.
    FollowPlans { places: Vec<[f64; 2]> },
    /// Server: what this account may do (`cartopolis_core::permissions`, by
    /// slug) — sent after the handshake and again whenever an operator changes
    /// a role, the account's roles or its admin flag, so a client's buttons
    /// follow the server's own gate without a reconnect. Slugs rather than a
    /// bit set so a permission added later is an unknown string to an older
    /// client, not a misread bit. The server enforces every one of these
    /// itself; this only decides what a client offers.
    Permissions { granted: Vec<String> },

    // ── Hands and tones (protocol 36, `docs/notes/hands-and-tones.md`) ──
    /// Client: where this user's hand is relative to a prim whose script senses
    /// hands, sent while the hand is within the prim's `hand_range` and the user
    /// has high-precision tracking on — at most `tone::HAND_HZ` per prim.
    /// Dropped when it is out of that range. Never relayed: it is input for the
    /// prim's script (`on_hand`), which decides what it means.
    HandPose { pose: HandPose },
    /// Server: the tone a prim's script set (`set_tone`), or `None` when it
    /// stopped (`stop_tone`). Its own message rather than a `PrimUpsert`
    /// because a script following a hand sets it up to twenty times a second,
    /// and a whole prim per change would make every client rebuild its mesh.
    /// Sent to every connection; each measures its own distance to the prim.
    PrimTone {
        prim_id: i64,
        tone: Option<ToneSpec>,
    },

    // ── Communities (protocol 39, `docs/notes/communities.md`) ────────────────
    //
    // A community is a named set of people with a few topics; a topic is an
    // encrypted group chat (the E2ee messages above) that every member is in.
    // The server keeps the community's name, description, topic names and
    // members in the clear so it can list it and route to it; what is said in
    // a topic stays sealed. Being in a community is being in all its topics,
    // so the server, which decides who is a member, also decides who must be
    // added to or taken out of each topic, and hands that work to a device
    // already in it (the server holds no key and cannot do it itself).
    /// Client asks for every community it is in or has asked to join.
    CommunityList { request_id: u32 },
    /// Server: every community this account is in or waiting for, whole. The
    /// answer to every community request, and pushed unasked (`request_id` 0)
    /// whenever one of them changes, so a client never patches its own copy.
    Communities {
        request_id: u32,
        communities: Vec<CommunityDto>,
    },
    /// Client starts a community, as its owner. Topics and people follow as
    /// [`NetMessage::TopicCreate`] and [`NetMessage::CommunityAddMember`].
    CommunityCreate {
        request_id: u32,
        settings: CommunitySettings,
    },
    /// Server: the community exists.
    CommunityCreated { request_id: u32, id: Uuid },
    /// A moderator changes a community's settings.
    CommunityEdit {
        request_id: u32,
        id: Uuid,
        settings: CommunitySettings,
    },
    /// Client searches the listed communities on this server. An empty query
    /// lists the most popular.
    CommunityDirectory {
        request_id: u32,
        query: String,
        order: DirectoryOrder,
        /// Where the asker is (latitude, longitude), for [`DirectoryOrder::Near`].
        near: Option<(f64, f64)>,
    },
    /// Server: the answer to a directory search.
    CommunityListings {
        request_id: u32,
        listings: Vec<CommunityListing>,
    },
    /// Client joins an open community, or asks to join one that asks first,
    /// with an optional line to its moderators.
    CommunityJoin {
        request_id: u32,
        id: Uuid,
        message: String,
    },
    /// Client leaves a community, or withdraws its request to join one. An
    /// owner must hand the community over first.
    CommunityLeave { request_id: u32, id: Uuid },
    /// A moderator lets somebody in, or turns their request down.
    CommunityDecide {
        request_id: u32,
        id: Uuid,
        address: String,
        accept: bool,
    },
    /// A moderator adds somebody (a full address on this server).
    CommunityAddMember {
        request_id: u32,
        id: Uuid,
        address: String,
    },
    /// The owner makes somebody a moderator or a member again, or hands the
    /// community over (`role` Owner: they become owner, the old owner a
    /// moderator).
    CommunitySetRole {
        request_id: u32,
        id: Uuid,
        address: String,
        role: CommunityRole,
    },
    /// A moderator takes somebody out of one topic (`topic`) or the whole
    /// community, and with `block` stops them asking to join again.
    CommunityRemove {
        request_id: u32,
        id: Uuid,
        address: String,
        topic: Option<Vec<u8>>,
        block: bool,
    },
    /// The owner (or a server operator who triages reports) deletes a
    /// community and all its topics.
    CommunityDelete { request_id: u32, id: Uuid },
    /// Client lists a chat it has just registered (`E2eeCreateGroup`) as a
    /// topic of a community it is in. Every other member is then added to it.
    TopicCreate {
        request_id: u32,
        community: Uuid,
        group: Vec<u8>,
        name: String,
    },
    /// A moderator renames, moves or archives a topic. An archived topic keeps
    /// its history and takes no new messages.
    TopicEdit {
        request_id: u32,
        community: Uuid,
        group: Vec<u8>,
        name: String,
        position: u32,
        archived: bool,
    },
    /// Server, to a device in `group`: add these people (one key package per
    /// device, claimed by the server for this) with one commit. They are
    /// members of the topic's community who are not in the topic yet.
    TopicAddPeople {
        group: Vec<u8>,
        people: Vec<E2eeNewDevice>,
    },
    /// Server, to a device in `group`: take every device of these people out
    /// of it with one commit. The server has already stopped delivering to
    /// them; the commit is what stops their keys opening anything new.
    TopicRemovePeople {
        group: Vec<u8>,
        addresses: Vec<String>,
    },
    /// Client: its commit taking `addresses` out of `group` was accepted, so
    /// the server can stop asking.
    TopicRemoved {
        group: Vec<u8>,
        addresses: Vec<String>,
    },
    /// Server, to the devices of somebody taken out of a topic: it is gone.
    /// The device forgets it; `by` is the moderator, or empty when they left.
    TopicLeft {
        group: Vec<u8>,
        community: Uuid,
        by: String,
    },
    /// A moderator deletes a message from a topic of their community. The
    /// server stops delivering it and tells every member to hide it.
    ModerateDelete {
        request_id: u32,
        group: Vec<u8>,
        seq: u64,
    },
    /// Server: entry `seq` of `group` was deleted by a moderator (`by`). Sent
    /// live and, in place of the entry, on every later sync.
    E2eeRedacted {
        group: Vec<u8>,
        seq: u64,
        by: String,
    },
    /// Client reports something said in a community to its moderators. The
    /// line is the reporter's own copy: the server cannot read the topic.
    CommunityReport {
        request_id: u32,
        id: Uuid,
        address: String,
        group: Vec<u8>,
        seq: u64,
        reason: ReportReason,
        note: String,
        line: String,
    },
    /// A moderator closes a report.
    CommunityReportResolve {
        request_id: u32,
        id: Uuid,
        report: i64,
    },
    /// A moderator makes an invite link: anyone holding its code may join,
    /// whatever the community's way in, for [`COMMUNITY_INVITE_DAYS`].
    CommunityInviteCreate { request_id: u32, id: Uuid },
    /// Server: the invite's code, for `https://cartopolis.org/c/<code>`.
    CommunityInvite {
        request_id: u32,
        id: Uuid,
        code: String,
    },
    /// Client looks an invite up before joining with it: the answer is a
    /// [`NetMessage::CommunityListings`] with the one community in it, listed or not.
    CommunityInviteLookup { request_id: u32, code: String },
    /// Client joins with an invite.
    CommunityJoinInvite { request_id: u32, code: String },
    /// Server, to a device already in `group`: these devices belong to its
    /// members but are not in it (signed in after it started, or had no key
    /// package left then). Add them with one commit whose invitation names
    /// them. The server claimed each package for this, so it is used once;
    /// a device that is already in the chat is skipped.
    E2eeAddDevices {
        group: Vec<u8>,
        devices: Vec<E2eeNewDevice>,
    },

    // ── Meetings (protocol 41, `docs/notes/calls.md`) ────────────────────────
    //
    // What a host does is enforced here, at the server, with what it already
    // sees: who is connected and which stream is whose. Held sound and a
    // forbidden screen are dropped at the forwarder, not hidden in the other
    // apps. What people say to each other (reactions) stays sealed.
    /// Client: something about this device's own place in the call (a hand, a
    /// shared screen), or a host's action on the call or on somebody in it.
    /// A host action from somebody who is not a host is refused with a
    /// `ServerError` carrying the request base of the call.
    CallControl { group: Vec<u8>, action: CallAction },
    /// Server, to a device that is no longer in the call because a host
    /// removed it or ended the call for everyone. Its media connections are
    /// already closed.
    CallEnded {
        group: Vec<u8>,
        reason: CallEndReason,
    },
    /// Client: a reaction, sealed with a key from the chat's group for
    /// `epoch` (`cartopolis_calls::keys::seal_event`), relayed to everyone
    /// else in the call. The server learns that somebody reacted, not how.
    CallReact {
        group: Vec<u8>,
        epoch: u64,
        sealed: Vec<u8>,
    },
    /// Server: somebody in the call reacted. `participant` is stamped by the
    /// server, so who sent it cannot be forged.
    CallReacted {
        group: Vec<u8>,
        participant: u64,
        epoch: u64,
        sealed: Vec<u8>,
    },
    /// Client: whose cameras this device draws. `None` is everybody (where a
    /// call starts); the forwarder sends no camera picture to a device that
    /// does not draw it. Shared screens are always sent.
    CallWatch {
        group: Vec<u8>,
        cameras: Option<Vec<u64>>,
    },
    /// Server: the join under `request_id` waits for a host to let this
    /// device in (`CallSettings::members_wait`, or a guest). `participant` is
    /// its number from now on; a host's `Admit` answers it with the
    /// `CallJoined` it was waiting for, a `Deny` with a `ServerError`.
    CallWaiting {
        request_id: u32,
        group: Vec<u8>,
        participant: u64,
    },
    /// Client: make a link a guest can join this chat's call by. Only a member
    /// of the chat may; the answer is [`NetMessage::CallInvited`]. The link's
    /// secret is the asking device's own and never reaches the server.
    CallInvite { group: Vec<u8> },
    /// Server: the code for a guest link to `group`'s call, good until
    /// `expires_ms`. Also what a guest is told right after signing in with
    /// one, so it knows which call it may join.
    CallInvited {
        group: Vec<u8>,
        code: String,
        expires_ms: u64,
    },
    /// Client (a host): the call's key for the guest seated as `participant`,
    /// sealed under the link's secret, which the server does not have
    /// (`docs/notes/calls.md`, "Guests"). Relayed to that guest only.
    CallKeyDrop {
        group: Vec<u8>,
        participant: u64,
        sealed: Vec<u8>,
    },
    /// Server: a host's [`NetMessage::CallKeyDrop`], for this guest.
    CallKeyDropped { group: Vec<u8>, sealed: Vec<u8> },
    /// This client's avatar look (protocol 42), sent once connected and again
    /// whenever its owner saves a new one. The server keeps it with the account
    /// and hands it on as [`NetMessage::AvatarLookOf`]; one it finds
    /// unreasonable ([`AvatarLook::is_reasonable`]) is dropped.
    SetAvatarLook { look: AvatarLook },
    /// How the avatar `avatar_id` looks. Sent to every client when its owner
    /// arrives or changes it, and for every online avatar to a client that has
    /// just connected. Only the avatar id, never the account: who an avatar is
    /// stays something a client has to ask.
    AvatarLookOf { avatar_id: u64, look: AvatarLook },
    /// This client's face as its camera tracks it (protocol 44): the face's
    /// [`FACE_UNITS`] weights, each 0–255 for 0–1, at most [`FACE_FRAME_HZ`]
    /// a second; empty when tracking stops. The server hands it on as
    /// [`NetMessage::FaceFrameOf`] to the clients within speaking range and
    /// keeps nothing. A frame of the wrong length is dropped.
    SetFaceFrame { weights: Vec<u8> },
    /// The tracked face of the avatar `avatar_id`, as its owner sent it.
    FaceFrameOf { avatar_id: u64, weights: Vec<u8> },
}
