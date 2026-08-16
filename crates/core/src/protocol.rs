//! Wire messages (ADR-009). TCP framing is length-delimited (ADR-008); each frame body is an **app frame**:
//! `protocol_version` (u16 LE) + `message_kind` (u16 LE) + `request_id` (u32 LE) + postcard(`NetMessage`).

use glam::Vec3;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::error::ProtocolError;

/// One wire-protocol revision: what changed and which ADR decided it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ProtocolRevision {
    pub version: u16,
    /// Deciding ADR ("ADR-028"), or `""` for a change with no ADR of its own.
    pub adr: &'static str,
    pub summary: &'static str,
}

/// **The** protocol changelog — the single place a wire revision is recorded.
///
/// [`PROTOCOL_VERSION`] is derived from the last entry, so bumping the version
/// *is* appending a row here: the number can't move without a note saying why,
/// and no other file needs to carry a copy of it. Prose (CLAUDE.md, the ADRs)
/// points at this constant rather than repeating the number — three docs claimed
/// "currently 8" while the wire had already moved to 11.
///
/// Entries start at v8, the oldest revision with a written record (ADR-018);
/// v1–v7 predate the log and no client speaking them exists.
pub const PROTOCOL_HISTORY: &[ProtocolRevision] = &[
    ProtocolRevision {
        version: 8,
        adr: "ADR-018",
        summary: "prim texture surface params (alpha/glow/full-bright/repeats/rotation/offset)",
    },
    ProtocolRevision {
        version: 9,
        adr: "ADR-016",
        summary: "authenticated handshake — ClientHello.auth, ServerHelloAck.principal_id + session_token",
    },
    ProtocolRevision {
        version: 10,
        adr: "ADR-029",
        summary: "geo-anchored prims — PrimDto.geo; CreatePrim/UpdatePrim carry a mandatory GeoAnchor",
    },
    ProtocolRevision {
        version: 11,
        adr: "ADR-028",
        summary: "chat — ChatSend / ChatMessage / ChatSubscribe; Local (proximity), Global and Room scopes",
    },
    ProtocolRevision {
        version: 12,
        adr: "",
        summary: "explicit flight — ClientIntent.flying; the sim falls an avatar that isn't flying",
    },
    ProtocolRevision {
        version: 13,
        adr: "",
        summary: "jump + emotes — ClientIntent.jump, AvatarStateDto.emote; the sim integrates a real gravity arc",
    },
    ProtocolRevision {
        version: 14,
        adr: "",
        summary: "prim scripts + region capacity — Get/Set/PrimScript, ScriptOutput, TouchPrim, RegionCapacity",
    },
    ProtocolRevision {
        version: 15,
        adr: "",
        summary: "proximity voice — VoiceFrame / VoiceData; opaque codec payload fanned out by ChatHub",
    },
    ProtocolRevision {
        version: 16,
        adr: "",
        summary: "moderation — SetBlocked / BlockedList / ReportUser / ReportAck, and DeleteAccount / AccountDeleted",
    },
];

/// Current wire version, sent in every app frame; `decode_app_frame` rejects
/// mismatches (`ProtocolError::UnsupportedVersion`). Derived from
/// [`PROTOCOL_HISTORY`] — append there to bump it.
pub const PROTOCOL_VERSION: u16 = PROTOCOL_HISTORY[PROTOCOL_HISTORY.len() - 1].version;

const APP_HEADER_LEN: usize = 8;

#[repr(u16)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MessageKind {
    ClientHello = 1,
    ServerHelloAck = 2,
    ServerError = 3,
    ClientIntent = 4,
    ObserverUpdate = 5,
    WorldSnapshot = 6,
    PrimRemoved = 7,
    CreatePrim = 8,
    UpdatePrim = 9,
    DeletePrim = 10,
    PrimUpsert = 11,
    TextureCatalogRequest = 12,
    TextureCatalog = 13,
    TextureRequest = 14,
    TextureData = 15,
    TextureNotFound = 16,
    ChatSend = 17,
    ChatMessage = 18,
    ChatSubscribe = 19,
    GetPrimScript = 20,
    SetPrimScript = 21,
    PrimScript = 22,
    ScriptOutput = 23,
    TouchPrim = 24,
    RegionCapacity = 25,
    VoiceFrame = 26,
    VoiceData = 27,
    SetBlocked = 28,
    BlockedList = 29,
    ReportUser = 30,
    ReportAck = 31,
    DeleteAccount = 32,
    AccountDeleted = 33,
}

impl MessageKind {
    #[must_use]
    pub fn from_wire(v: u16) -> Option<Self> {
        match v {
            1 => Some(Self::ClientHello),
            2 => Some(Self::ServerHelloAck),
            3 => Some(Self::ServerError),
            4 => Some(Self::ClientIntent),
            5 => Some(Self::ObserverUpdate),
            6 => Some(Self::WorldSnapshot),
            7 => Some(Self::PrimRemoved),
            8 => Some(Self::CreatePrim),
            9 => Some(Self::UpdatePrim),
            10 => Some(Self::DeletePrim),
            11 => Some(Self::PrimUpsert),
            12 => Some(Self::TextureCatalogRequest),
            13 => Some(Self::TextureCatalog),
            14 => Some(Self::TextureRequest),
            15 => Some(Self::TextureData),
            16 => Some(Self::TextureNotFound),
            17 => Some(Self::ChatSend),
            18 => Some(Self::ChatMessage),
            19 => Some(Self::ChatSubscribe),
            20 => Some(Self::GetPrimScript),
            21 => Some(Self::SetPrimScript),
            22 => Some(Self::PrimScript),
            23 => Some(Self::ScriptOutput),
            24 => Some(Self::TouchPrim),
            25 => Some(Self::RegionCapacity),
            26 => Some(Self::VoiceFrame),
            27 => Some(Self::VoiceData),
            28 => Some(Self::SetBlocked),
            29 => Some(Self::BlockedList),
            30 => Some(Self::ReportUser),
            31 => Some(Self::ReportAck),
            32 => Some(Self::DeleteAccount),
            33 => Some(Self::AccountDeleted),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct TextureEntry {
    pub id: String,
    pub name: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct RegionDto {
    pub id: i64,
    pub name: String,
    pub latitude: f64,
    pub longitude: f64,
    pub tile_x: i64,
    pub tile_y: i64,
    pub tile_z: i64,
    /// Sim-space origin for this region’s tile (matches `simulator` grid layout).
    pub sim_x: f32,
    pub sim_y: f32,
    pub sim_z: f32,
}

/// Per-prim texture surface parameters (Texture tab). Mirrors a subset of the
/// Second Life texture tab: overall transparency, glow, full-bright, and the
/// texture UV transform (repeats per face, flip, rotation, offset).
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq)]
pub struct PrimSurface {
    /// Overall opacity, 0.0 (fully transparent) – 1.0 (opaque). SL "Transparency %" = (1 − alpha) · 100.
    pub alpha: f32,
    /// Surface glow, 0.0 – 1.0. Drives emissive (separate from scene lighting).
    pub glow: f32,
    /// Render fully lit regardless of the day/night sun position (SL "Full Bright").
    pub full_bright: bool,
    /// Texture repeats per face along U / V (tiling count). Default 1.0.
    pub repeat_u: f32,
    pub repeat_v: f32,
    /// Mirror the texture along U / V.
    pub flip_u: bool,
    pub flip_v: bool,
    /// Texture rotation in degrees.
    pub rotation: f32,
    /// Texture offset along U / V (fraction of a repeat).
    pub offset_u: f32,
    pub offset_v: f32,
}

impl Default for PrimSurface {
    fn default() -> Self {
        Self {
            alpha: 1.0,
            glow: 0.0,
            full_bright: false,
            repeat_u: 1.0,
            repeat_v: 1.0,
            flip_u: false,
            flip_v: false,
            rotation: 0.0,
            offset_u: 0.0,
            offset_v: 0.0,
        }
    }
}

/// Per-prim geometry deformation parameters (Object tab): path cut, hollow, and the
/// twist / taper / top-shear / slice "warp" set. Grouped so the online `UpdatePrim`
/// write path can carry them without a 20-field message variant.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq)]
pub struct PrimGeometry {
    pub path_cut_begin: f32,
    pub path_cut_end: f32,
    pub hollow: f32,
    pub twist_begin: f32,
    pub twist_end: f32,
    pub taper_x: f32,
    pub taper_y: f32,
    pub top_shear_x: f32,
    pub top_shear_y: f32,
    pub slice_begin: f32,
    pub slice_end: f32,
}

impl Default for PrimGeometry {
    fn default() -> Self {
        Self {
            path_cut_begin: 0.0,
            path_cut_end: 1.0,
            hollow: 0.0,
            twist_begin: 0.0,
            twist_end: 0.0,
            taper_x: 0.0,
            taper_y: 0.0,
            top_shear_x: 0.0,
            top_shear_y: 0.0,
            slice_begin: 0.0,
            slice_end: 1.0,
        }
    }
}

/// Absolute geographic placement of a prim's horizontal position (ADR-029).
///
/// This is the anchor-independent ground truth: the client derives the
/// render-space `GridCell` + local `Transform` from it via the *current* OSM
/// anchor (`lat_lng_to_world`), so a prim keeps its real-world location across a
/// re-anchor. Altitude is carried separately in `PrimDto::position.y`.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq)]
pub struct GeoAnchor {
    pub latitude: f64,
    pub longitude: f64,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct PrimDto {
    pub id: i64,
    pub region_id: i64,
    pub name: String,
    pub shape: String,
    /// Sim-space position (metres). `position.y` is the authoritative altitude;
    /// horizontal placement (x/z) is derived from `geo` (ADR-029), so x/z here are
    /// only a transient echo of the last render frame.
    pub position: Vec3,
    pub rotation: Vec3,
    pub scale: Vec3,
    pub color: [f32; 3],
    /// Optional texture key from standard library (ADR-017).
    #[serde(default)]
    pub texture_id: Option<String>,
    #[serde(default)]
    pub path_cut_begin: f32,
    #[serde(default = "default_path_cut_end")]
    pub path_cut_end: f32,
    #[serde(default)]
    pub hollow: f32,
    #[serde(default)]
    pub twist_begin: f32,
    #[serde(default)]
    pub twist_end: f32,
    #[serde(default)]
    pub taper_x: f32,
    #[serde(default)]
    pub taper_y: f32,
    #[serde(default)]
    pub top_shear_x: f32,
    #[serde(default)]
    pub top_shear_y: f32,
    #[serde(default)]
    pub slice_begin: f32,
    #[serde(default = "default_slice_end")]
    pub slice_end: f32,
    /// Texture surface params (ADR-017 / protocol v8).
    #[serde(default)]
    pub surface: PrimSurface,
    /// Absolute geographic placement (ADR-029 / protocol v10). Every prim is
    /// geo-anchored — there is no legacy fixed-origin placement.
    pub geo: GeoAnchor,
}

fn default_path_cut_end() -> f32 {
    1.0
}

fn default_slice_end() -> f32 {
    1.0
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct AvatarStateDto {
    pub id: u64,
    pub position: Vec3,
    pub yaw: f32,
    /// An emote this avatar started recently, so a client that just came into
    /// range still sees a wave in progress rather than missing it. The sim clears
    /// it after [`EMOTE_REPLICATE_SECS`]; a receiver plays it once per
    /// `(avatar, sequence)` it has not seen.
    pub emote: Option<EmoteState>,
}

/// A one-shot full-body animation an avatar can play. These are clips the
/// Quaternius "Ultimate Modular" characters already embed — every character glTF
/// carries the same 24 — so an emote costs no asset, only a name for the index.
///
/// Deliberately a closed enum rather than a clip index: the index is a property of
/// the *asset*, and putting it on the wire would freeze the character set. A
/// receiver maps the name to whatever its own models call that motion.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum Emote {
    Wave,
    /// The generic "use the thing in front of me" motion, also played by the
    /// world-interaction prompt.
    Interact,
    Roll,
    Punch,
    Kick,
    /// Play dead. Every set has one, and someone will find it.
    Faint,
}

/// An emote plus the sequence number that makes it re-playable: the same emote
/// twice in a row is two distinct events, and a receiver diffing on the emote
/// alone would show only the first.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct EmoteState {
    pub emote: Emote,
    pub sequence: u32,
}

/// How long an emote stays on an avatar's replicated state. A little longer than
/// the longest clip, so a client that receives its first snapshot mid-wave still
/// plays it, and short enough that it does not replay on a much later snapshot.
pub const EMOTE_REPLICATE_SECS: f32 = 2.5;

/// Longest room name the server will act on.
///
/// `ChatScope::Room` is a `String` off the wire, so its only other bound is
/// `MAX_FRAME` — 32 MiB. The server turns it into a channel key (`format!(
/// "room:{room}")`), clones that per recipient and re-encodes it into every
/// outgoing frame, so a name that size is an allocation multiplier on a message
/// path that runs at tens of frames a second. 64 characters is past any name
/// anyone types and nowhere near the range where the cost matters.
///
/// Not a wire-format change (nothing is added to or removed from a message), so
/// it appends no row to [`PROTOCOL_HISTORY`]: a client that ignores it gets its
/// oversized room send refused, exactly as it gets an over-long chat body
/// truncated today.
pub const MAX_ROOM_LEN: usize = 64;

/// Chat scope a client sends into (ADR-028). The server resolves it to a concrete
/// channel. Geo tiers (country/province/municipality) land with `cartopolis_geo`.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub enum ChatScope {
    /// Proximity chat; `shout` widens the radius. Ephemeral, never stored.
    Local { shout: bool },
    /// Everyone online.
    Global,
    /// A named, opt-in room.
    Room(String),
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
        /// Flight mode. The sim has no gravity of its own: it lets a flying avatar
        /// hover at whatever altitude it reached, and falls one that isn't flying.
        /// Without it, switching flight off in mid-air left the avatar hanging there.
        flying: bool,
        /// Vertical thrust while `flying`; ignored otherwise.
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
    },
    ObserverUpdate {
        position: Vec3,
    },
    WorldSnapshot {
        tick: u64,
        regions: Vec<RegionDto>,
        prims: Vec<PrimDto>,
        avatars: Vec<AvatarStateDto>,
    },
    /// ADR-011 delta v0: remove prim by stable id (server-assigned).
    PrimRemoved {
        id: i64,
    },
    /// ADR-017 / ADR-018: client requests creation of a new prim with full initial state.
    CreatePrim {
        request_id: u32,
        region_id: i64,
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
    },
    /// ADR-017: client requests deletion of a prim by id.
    DeletePrim {
        request_id: u32,
        prim_id: i64,
    },
    /// ADR-017: server broadcasts the full authoritative prim state after a create or update.
    PrimUpsert {
        prim: PrimDto,
    },
    /// Client requests the full texture catalog from the sim.
    TextureCatalogRequest,
    /// Sim responds with the list of available textures.
    TextureCatalog {
        textures: Vec<TextureEntry>,
    },
    /// Client requests raw PNG bytes for a specific texture.
    TextureRequest {
        request_id: u32,
        texture_id: String,
    },
    /// Sim sends raw PNG bytes for the requested texture.
    TextureData {
        request_id: u32,
        texture_id: String,
        png_bytes: Vec<u8>,
    },
    /// Sim signals that the requested texture id does not exist.
    TextureNotFound {
        request_id: u32,
        texture_id: String,
    },
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
    /// ADR-028: client joins (`join = true`) or leaves a room channel.
    ChatSubscribe {
        room: String,
        join: bool,
    },
    /// Fetch a prim's script source for the editor. Scripts are *not* carried in
    /// `WorldSnapshot`: source is authoring data, an order of magnitude larger
    /// than the rest of a `PrimDto`, and it would be re-sent to every observer
    /// 20 times a second for the benefit of the one person editing it.
    GetPrimScript {
        request_id: u32,
        prim_id: i64,
    },
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
    /// A line from a script: `vf.log`, a compile error, or a runtime fault. This
    /// is the script editor's console, and the only channel a broken script has.
    ScriptOutput {
        prim_id: i64,
        level: ScriptLevel,
        text: String,
        at_ms: u64,
    },
    /// Client touched a prim; the server turns it into the script's `on_touch`.
    TouchPrim {
        request_id: u32,
        prim_id: i64,
    },
    /// Region resource budget and current usage. Sent on connect and whenever a
    /// prim or script is added or removed, so the editor can show the headroom
    /// *before* an author hits the wall rather than as a refusal afterwards.
    RegionCapacity {
        region_id: i64,
        prims_used: u32,
        prims_max: u32,
        scripts_used: u32,
        scripts_max: u32,
        /// Per-script memory ceiling in bytes, enforced by the Lua allocator.
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
    BlockedList {
        usernames: Vec<String>,
    },
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
    },
    /// Server confirms a report was filed. `accepted = false` means it was
    /// refused (unknown subject, or reporting yourself) — the client says so
    /// rather than showing a silent success.
    ReportAck {
        request_id: u32,
        accepted: bool,
    },
    /// Delete the signed-in account and everything personal attached to it
    /// (App Store guideline 5.1.1(v): an app with accounts must offer deletion
    /// *within the app*).
    ///
    /// The password is required again even though the connection is already
    /// authenticated: this is the one irreversible thing a session token can ask
    /// for, and a token can be a remembered one from a device somebody else is
    /// holding.
    DeleteAccount {
        request_id: u32,
        password: String,
    },
    /// The account is gone. The server closes the connection immediately after
    /// sending this, so it is both the receipt and the goodbye.
    AccountDeleted,
}

/// Why a person is being reported. A closed list rather than free text: it is
/// what an operator triages by, and it is the vocabulary the report dialog
/// offers. `note` carries anything this does not cover.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum ReportReason {
    /// Targeted abuse of a person.
    Harassment,
    /// Slurs or attacks on a protected group.
    HateSpeech,
    /// Sexual content, or sexual attention that was not asked for.
    SexualContent,
    /// A threat of violence, or content suggesting someone is in danger.
    Threat,
    /// Flooding, advertising, scams.
    Spam,
    /// Anything else; `note` carries it.
    Other,
}

impl ReportReason {
    /// Every reason, in the order the report dialog offers them.
    pub const ALL: &'static [Self] = &[
        Self::Harassment,
        Self::HateSpeech,
        Self::SexualContent,
        Self::Threat,
        Self::Spam,
        Self::Other,
    ];

    /// Short label for the UI and for the operator's report list.
    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            Self::Harassment => "Harassment or bullying",
            Self::HateSpeech => "Hate speech or a slur",
            Self::SexualContent => "Sexual content",
            Self::Threat => "Threat or self-harm",
            Self::Spam => "Spam or a scam",
            Self::Other => "Something else",
        }
    }

    /// Stable string for storage and for the admin API. Parsed back by
    /// [`Self::from_wire`], so these two must move together.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Harassment => "harassment",
            Self::HateSpeech => "hate_speech",
            Self::SexualContent => "sexual_content",
            Self::Threat => "threat",
            Self::Spam => "spam",
            Self::Other => "other",
        }
    }

    /// Inverse of [`Self::as_str`]; an unrecognised string reads as `Other`
    /// rather than dropping a report on the floor.
    #[must_use]
    pub fn from_wire(s: &str) -> Self {
        match s {
            "harassment" => Self::Harassment,
            "hate_speech" => Self::HateSpeech,
            "sexual_content" => Self::SexualContent,
            "threat" => Self::Threat,
            "spam" => Self::Spam,
            _ => Self::Other,
        }
    }
}

/// Longest report note the server will store, in characters.
///
/// Same reasoning as [`MAX_ROOM_LEN`]: the field is a `String` off the wire and
/// its only other bound is `MAX_FRAME`. Long enough for a paragraph explaining
/// what happened, short enough that a report table cannot be used as storage.
pub const MAX_REPORT_NOTE_LEN: usize = 2000;

/// Longest reported-message excerpt the server will store, in characters.
pub const MAX_REPORT_MESSAGE_LEN: usize = 2000;

/// Severity of a [`NetMessage::ScriptOutput`] line.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum ScriptLevel {
    /// `vf.log` from a running script.
    Info,
    /// The host has something to say about the script's lifecycle (compiled,
    /// stopped, out of fuel).
    Notice,
    /// A compile error or a runtime fault. The script is not running.
    Error,
}

#[must_use]
pub fn message_kind(msg: &NetMessage) -> MessageKind {
    match msg {
        NetMessage::ClientHello { .. } => MessageKind::ClientHello,
        NetMessage::ServerHelloAck { .. } => MessageKind::ServerHelloAck,
        NetMessage::ServerError { .. } => MessageKind::ServerError,
        NetMessage::ClientIntent { .. } => MessageKind::ClientIntent,
        NetMessage::ObserverUpdate { .. } => MessageKind::ObserverUpdate,
        NetMessage::WorldSnapshot { .. } => MessageKind::WorldSnapshot,
        NetMessage::PrimRemoved { .. } => MessageKind::PrimRemoved,
        NetMessage::CreatePrim { .. } => MessageKind::CreatePrim,
        NetMessage::UpdatePrim { .. } => MessageKind::UpdatePrim,
        NetMessage::DeletePrim { .. } => MessageKind::DeletePrim,
        NetMessage::PrimUpsert { .. } => MessageKind::PrimUpsert,
        NetMessage::TextureCatalogRequest => MessageKind::TextureCatalogRequest,
        NetMessage::TextureCatalog { .. } => MessageKind::TextureCatalog,
        NetMessage::TextureRequest { .. } => MessageKind::TextureRequest,
        NetMessage::TextureData { .. } => MessageKind::TextureData,
        NetMessage::TextureNotFound { .. } => MessageKind::TextureNotFound,
        NetMessage::ChatSend { .. } => MessageKind::ChatSend,
        NetMessage::ChatMessage { .. } => MessageKind::ChatMessage,
        NetMessage::ChatSubscribe { .. } => MessageKind::ChatSubscribe,
        NetMessage::GetPrimScript { .. } => MessageKind::GetPrimScript,
        NetMessage::SetPrimScript { .. } => MessageKind::SetPrimScript,
        NetMessage::PrimScript { .. } => MessageKind::PrimScript,
        NetMessage::ScriptOutput { .. } => MessageKind::ScriptOutput,
        NetMessage::TouchPrim { .. } => MessageKind::TouchPrim,
        NetMessage::RegionCapacity { .. } => MessageKind::RegionCapacity,
        NetMessage::VoiceFrame { .. } => MessageKind::VoiceFrame,
        NetMessage::VoiceData { .. } => MessageKind::VoiceData,
        NetMessage::SetBlocked { .. } => MessageKind::SetBlocked,
        NetMessage::BlockedList { .. } => MessageKind::BlockedList,
        NetMessage::ReportUser { .. } => MessageKind::ReportUser,
        NetMessage::ReportAck { .. } => MessageKind::ReportAck,
        NetMessage::DeleteAccount { .. } => MessageKind::DeleteAccount,
        NetMessage::AccountDeleted => MessageKind::AccountDeleted,
    }
}

#[must_use]
pub fn message_request_id(msg: &NetMessage) -> u32 {
    match msg {
        NetMessage::ClientIntent { request_id, .. } => *request_id,
        NetMessage::ServerError { request_id, .. } => *request_id,
        NetMessage::CreatePrim { request_id, .. } => *request_id,
        NetMessage::UpdatePrim { request_id, .. } => *request_id,
        NetMessage::DeletePrim { request_id, .. } => *request_id,
        NetMessage::TextureRequest { request_id, .. } => *request_id,
        NetMessage::TextureData { request_id, .. } => *request_id,
        NetMessage::TextureNotFound { request_id, .. } => *request_id,
        NetMessage::ChatSend { request_id, .. } => *request_id,
        NetMessage::GetPrimScript { request_id, .. } => *request_id,
        NetMessage::SetPrimScript { request_id, .. } => *request_id,
        NetMessage::TouchPrim { request_id, .. } => *request_id,
        NetMessage::SetBlocked { request_id, .. } => *request_id,
        NetMessage::ReportUser { request_id, .. } => *request_id,
        NetMessage::ReportAck { request_id, .. } => *request_id,
        NetMessage::DeleteAccount { request_id, .. } => *request_id,
        _ => 0,
    }
}

/// Raw postcard body (no app header). Prefer [`encode_app_frame`] / [`decode_app_frame`] on the wire.
#[must_use]
pub fn encode_message(msg: &NetMessage) -> Result<Vec<u8>, postcard::Error> {
    postcard::to_allocvec(msg)
}

#[must_use]
pub fn decode_message(bytes: &[u8]) -> Result<NetMessage, postcard::Error> {
    postcard::from_bytes(bytes)
}

/// Full ADR-009 app frame: version + kind + request_id + postcard payload.
#[must_use]
pub fn encode_app_frame(msg: &NetMessage) -> Result<Vec<u8>, postcard::Error> {
    let body = encode_message(msg)?;
    let kind = message_kind(msg) as u16;
    let rid = message_request_id(msg);
    let mut out = Vec::with_capacity(APP_HEADER_LEN + body.len());
    out.extend_from_slice(&PROTOCOL_VERSION.to_le_bytes());
    out.extend_from_slice(&kind.to_le_bytes());
    out.extend_from_slice(&rid.to_le_bytes());
    out.extend_from_slice(&body);
    Ok(out)
}

#[must_use]
pub fn decode_app_frame(bytes: &[u8]) -> Result<NetMessage, ProtocolError> {
    if bytes.len() < APP_HEADER_LEN {
        return Err(ProtocolError::FrameTooShort(bytes.len()));
    }
    let ver = u16::from_le_bytes(bytes[0..2].try_into().unwrap());
    if ver != PROTOCOL_VERSION {
        return Err(ProtocolError::UnsupportedVersion(ver));
    }
    let kind_wire = u16::from_le_bytes(bytes[2..4].try_into().unwrap());
    let Some(kind) = MessageKind::from_wire(kind_wire) else {
        return Err(ProtocolError::UnknownMessageKind(kind_wire));
    };
    let _request_id = u32::from_le_bytes(bytes[4..8].try_into().unwrap());
    let msg = decode_message(&bytes[APP_HEADER_LEN..])?;
    if message_kind(&msg) != kind {
        return Err(ProtocolError::KindMismatch {
            header: kind_wire,
            body: message_kind(&msg) as u16,
        });
    }
    Ok(msg)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The changelog is the source of truth for the wire version, so it has to
    /// stay a gapless ascending run — a duplicated or skipped number would let
    /// two different schemas answer to one version.
    #[test]
    fn protocol_history_is_contiguous_and_current() {
        assert!(!PROTOCOL_HISTORY.is_empty());
        for pair in PROTOCOL_HISTORY.windows(2) {
            assert_eq!(
                pair[1].version,
                pair[0].version + 1,
                "gap or duplicate between v{} and v{}",
                pair[0].version,
                pair[1].version
            );
        }
        for rev in PROTOCOL_HISTORY {
            assert!(!rev.summary.is_empty(), "v{} has no summary", rev.version);
        }
        assert_eq!(
            PROTOCOL_VERSION,
            PROTOCOL_HISTORY[PROTOCOL_HISTORY.len() - 1].version
        );
    }

    #[test]
    fn roundtrip_hello_app_frame() {
        let m = NetMessage::ClientHello {
            protocol_version: PROTOCOL_VERSION,
            auth: AuthCredential::Password {
                username: "dev".into(),
                password: "dev".into(),
            },
        };
        let b = encode_app_frame(&m).unwrap();
        let m2 = decode_app_frame(&b).unwrap();
        assert_eq!(m, m2);
    }

    #[test]
    fn roundtrip_hello_ack_app_frame() {
        let m = NetMessage::ServerHelloAck {
            session_id: Uuid::nil(),
            tick_hz: 20.0,
            your_avatar_id: 7,
            principal_id: Uuid::nil(),
            username: "dev".into(),
            session_token: "deadbeef".into(),
            osm_tile_url_template: String::new(),
        };
        let b = encode_app_frame(&m).unwrap();
        let m2 = decode_app_frame(&b).unwrap();
        assert_eq!(m, m2);
    }

    #[test]
    fn roundtrip_snapshot_app_frame() {
        let m = NetMessage::WorldSnapshot {
            tick: 42,
            regions: vec![],
            prims: vec![],
            avatars: vec![],
        };
        let b = encode_app_frame(&m).unwrap();
        let m2 = decode_app_frame(&b).unwrap();
        assert_eq!(m, m2);
    }

    #[test]
    fn roundtrip_create_prim_app_frame() {
        let m = NetMessage::CreatePrim {
            request_id: 123,
            region_id: 1,
            name: "New Prim".into(),
            position: Vec3::new(10.0, 5.0, 20.0),
            rotation: Vec3::ZERO,
            scale: Vec3::ONE,
            color: [0.5, 0.5, 0.5],
            texture_id: None,
            shape: "box".into(),
            surface: PrimSurface::default(),
            geometry: PrimGeometry::default(),
            geo: GeoAnchor {
                latitude: 53.2194,
                longitude: 6.5665,
            },
        };
        let b = encode_app_frame(&m).unwrap();
        let m2 = decode_app_frame(&b).unwrap();
        assert_eq!(m, m2);
    }

    #[test]
    fn roundtrip_update_prim_app_frame() {
        let m = NetMessage::UpdatePrim {
            request_id: 124,
            prim_id: 42,
            position: Vec3::new(10.0, 5.0, 20.0),
            rotation: Vec3::new(0.0, 90.0, 0.0),
            scale: Vec3::new(2.0, 2.0, 2.0),
            color: [1.0, 0.0, 0.0],
            texture_id: Some("brick".into()),
            name: "My Prim".into(),
            surface: PrimSurface::default(),
            geometry: PrimGeometry::default(),
            geo: GeoAnchor {
                latitude: 53.2194,
                longitude: 6.5665,
            },
        };
        let b = encode_app_frame(&m).unwrap();
        let m2 = decode_app_frame(&b).unwrap();
        assert_eq!(m, m2);
    }

    #[test]
    fn roundtrip_chat_app_frames() {
        for m in [
            NetMessage::ChatSend {
                request_id: 7,
                scope: ChatScope::Local { shout: true },
                body: "hi".into(),
            },
            NetMessage::ChatSend {
                request_id: 8,
                scope: ChatScope::Room("helpdesk".into()),
                body: "yo".into(),
            },
            NetMessage::ChatMessage {
                channel: "geo:".into(),
                sender_id: 3,
                sender_name: "dev".into(),
                sent_at_ms: 1234,
                body: "hello".into(),
                distance_m: Some(12.5),
            },
            NetMessage::ChatSubscribe {
                room: "helpdesk".into(),
                join: true,
            },
        ] {
            let b = encode_app_frame(&m).unwrap();
            assert_eq!(decode_app_frame(&b).unwrap(), m);
        }
    }

    #[test]
    fn roundtrip_moderation_app_frames() {
        for m in [
            NetMessage::SetBlocked {
                request_id: 3,
                username: "loud".into(),
                blocked: true,
            },
            NetMessage::BlockedList {
                usernames: vec!["loud".into(), "louder".into()],
            },
            NetMessage::BlockedList { usernames: vec![] },
            NetMessage::ReportUser {
                request_id: 4,
                username: "loud".into(),
                reason: ReportReason::Harassment,
                note: "followed me across three regions".into(),
                message: Some("the line they sent".into()),
            },
            NetMessage::ReportUser {
                request_id: 5,
                username: "loud".into(),
                reason: ReportReason::Other,
                note: String::new(),
                message: None,
            },
            NetMessage::ReportAck {
                request_id: 4,
                accepted: true,
            },
            NetMessage::DeleteAccount {
                request_id: 6,
                password: "hunter2".into(),
            },
            NetMessage::AccountDeleted,
        ] {
            let b = encode_app_frame(&m).unwrap();
            assert_eq!(decode_app_frame(&b).unwrap(), m);
        }
    }

    /// `as_str` is what lands in SQLite and in the admin API, so a reason that
    /// does not survive the trip silently retypes every stored report as `Other`.
    #[test]
    fn every_report_reason_survives_its_stored_form() {
        for r in ReportReason::ALL {
            assert_eq!(ReportReason::from_wire(r.as_str()), *r);
            assert!(!r.label().is_empty());
        }
        assert_eq!(
            ReportReason::from_wire("something we stopped using"),
            ReportReason::Other
        );
    }

    #[test]
    fn roundtrip_voice_app_frames() {
        for m in [
            NetMessage::VoiceFrame {
                scope: ChatScope::Local { shout: false },
                seq: 42,
                payload: vec![0x00, 0xff, 0x7f],
            },
            NetMessage::VoiceFrame {
                scope: ChatScope::Room("standup".into()),
                seq: u32::MAX,
                payload: vec![1; 1500],
            },
            NetMessage::VoiceData {
                channel: "voice:local".into(),
                sender_id: 9,
                seq: 43,
                payload: vec![0xde, 0xad],
                distance_m: Some(31.25),
            },
            NetMessage::VoiceData {
                channel: "room:standup".into(),
                sender_id: 9,
                seq: 44,
                payload: vec![],
                distance_m: None,
            },
        ] {
            let b = encode_app_frame(&m).unwrap();
            assert_eq!(decode_app_frame(&b).unwrap(), m);
        }
    }

    #[test]
    fn roundtrip_delete_prim_app_frame() {
        let m = NetMessage::DeletePrim {
            request_id: 125,
            prim_id: 42,
        };
        let b = encode_app_frame(&m).unwrap();
        let m2 = decode_app_frame(&b).unwrap();
        assert_eq!(m, m2);
    }

    #[test]
    fn roundtrip_prim_upsert_app_frame() {
        let prim = PrimDto {
            id: 42,
            region_id: 1,
            name: "Updated Prim".into(),
            shape: "sphere".into(),
            position: Vec3::new(15.0, 8.0, 25.0),
            rotation: Vec3::new(45.0, 45.0, 0.0),
            scale: Vec3::new(1.5, 1.5, 1.5),
            color: [0.0, 1.0, 0.0],
            texture_id: Some("grass".into()),
            path_cut_begin: 0.0,
            path_cut_end: 1.0,
            hollow: 0.0,
            twist_begin: 0.0,
            twist_end: 0.0,
            taper_x: 0.0,
            taper_y: 0.0,
            top_shear_x: 0.0,
            top_shear_y: 0.0,
            slice_begin: 0.0,
            slice_end: 1.0,
            surface: PrimSurface::default(),
            geo: GeoAnchor {
                latitude: 53.2194,
                longitude: 6.5665,
            },
        };
        let m = NetMessage::PrimUpsert { prim: prim.clone() };
        let b = encode_app_frame(&m).unwrap();
        let m2 = decode_app_frame(&b).unwrap();
        assert_eq!(m, m2);
    }

    /// Every `MessageKind` must survive the `discriminant → from_wire` trip, and
    /// nothing may sit outside the assigned range. This is the guard the
    /// per-message round-trips above can't be: adding a `NetMessage` variant +
    /// kind but forgetting its `from_wire` arm (or mis-numbering it) silently
    /// desyncs a real client. The `ALL.len()` assert is a deliberate tripwire —
    /// a new variant fails here until it's added to this list.
    #[test]
    fn message_kind_wire_is_a_bijection() {
        use MessageKind::*;
        const ALL: [MessageKind; 33] = [
            ClientHello,
            ServerHelloAck,
            ServerError,
            ClientIntent,
            ObserverUpdate,
            WorldSnapshot,
            PrimRemoved,
            CreatePrim,
            UpdatePrim,
            DeletePrim,
            PrimUpsert,
            TextureCatalogRequest,
            TextureCatalog,
            TextureRequest,
            TextureData,
            TextureNotFound,
            ChatSend,
            ChatMessage,
            ChatSubscribe,
            GetPrimScript,
            SetPrimScript,
            PrimScript,
            ScriptOutput,
            TouchPrim,
            RegionCapacity,
            VoiceFrame,
            VoiceData,
            SetBlocked,
            BlockedList,
            ReportUser,
            ReportAck,
            DeleteAccount,
            AccountDeleted,
        ];
        for k in ALL {
            assert_eq!(
                MessageKind::from_wire(k as u16),
                Some(k),
                "discriminant {} lost its from_wire arm",
                k as u16
            );
        }
        // Contiguous from 1, nothing on either side. `ALL` is the list to extend
        // when a kind is added — that is the point of the test, and the reason it
        // is written against a hand-maintained list rather than a derive.
        assert_eq!(MessageKind::from_wire(0), None);
        assert_eq!(MessageKind::from_wire(ALL.len() as u16 + 1), None);
    }

    /// The header guards in `decode_app_frame` — the checks that keep a
    /// version-skewed or corrupt frame from being misread as a valid message.
    /// Untested by the round-trips, and exactly the paths a mismatched client hits.
    #[test]
    fn decode_app_frame_rejects_bad_headers() {
        let good = encode_app_frame(&NetMessage::PrimRemoved { id: 1 }).unwrap();

        // Truncated below the 8-byte app header.
        assert!(matches!(
            decode_app_frame(&good[..4]),
            Err(ProtocolError::FrameTooShort(4))
        ));

        // Wrong protocol version (bytes 0..2).
        let mut wrong_ver = good.clone();
        wrong_ver[0] = wrong_ver[0].wrapping_add(1);
        assert!(matches!(
            decode_app_frame(&wrong_ver),
            Err(ProtocolError::UnsupportedVersion(_))
        ));

        // Unknown message kind (bytes 2..4); 0 is never assigned.
        let mut bad_kind = good.clone();
        bad_kind[2] = 0;
        bad_kind[3] = 0;
        assert!(matches!(
            decode_app_frame(&bad_kind),
            Err(ProtocolError::UnknownMessageKind(0))
        ));
    }
}
