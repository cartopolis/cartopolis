//! The wire's changelog: [`PROTOCOL_HISTORY`] and the version derived from it.

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
    ProtocolRevision {
        version: 17,
        adr: "",
        summary: "avatar teleport — Teleport { position }; the sim moves the avatar there and the next snapshot confirms it",
    },
    ProtocolRevision {
        version: 18,
        adr: "",
        summary: "presence — SetPresence { active }; an idle client stops being sent WorldSnapshots, and chat still reaches it",
    },
    ProtocolRevision {
        version: 19,
        adr: "",
        summary: "sitting — ClientIntent.seated / AvatarStateDto.seated carry the seat as an offset from the avatar's own position",
    },
    ProtocolRevision {
        version: 20,
        adr: "",
        summary: "particles + avatar geo — PrimDto.particles carries an emitter; ClientIntent.geo says where on Earth the avatar is, which is the only frame a script's sensor can measure in",
    },
    ProtocolRevision {
        version: 21,
        adr: "",
        summary: "PrimDto.scripted — whether a prim carries a script. The client cannot derive it (source never travels in a snapshot) and it is what decides whether walking up to a prim offers to press it",
    },
    ProtocolRevision {
        version: 22,
        adr: "",
        summary: "model assets — PrimDto/CreatePrim/UpdatePrim carry an optional `model`, the hash of a glTF binary the sim stores; ServerHelloAck says where to fetch one; ReportUser can name one",
    },
    ProtocolRevision {
        version: 23,
        adr: "",
        summary: "regions retired — RegionDto and WorldSnapshot.regions gone, PrimDto/CreatePrim lose region_id, RegionCapacity becomes the world-wide WorldCapacity, and ObserverUpdate goes with the AoI filter it fed",
    },
    ProtocolRevision {
        version: 24,
        adr: "",
        summary: "AvatarStateDto.velocity — the world velocity the sim integrated this tick, so a receiver can extrapolate another avatar to the present and pick its gait from a speed rather than differencing smoothed positions",
    },
    ProtocolRevision {
        version: 25,
        adr: "",
        summary: "private chats — E2eeRegisterDevice / ClaimKeyPackages / CreateGroup / Send / Sync and their answers; the server orders and relays MLS-sealed bytes it cannot open (docs/notes/encrypted-chat.md)",
    },
    ProtocolRevision {
        version: 26,
        adr: "",
        summary: "calls — CallJoin / CallJoined / CallOffer / CallAnswer / CallLeave / CallRoster: SDP signalling for a private chat's call, whose media goes over UDP to the simulator's forwarder sealed with keys from the chat (docs/notes/calls.md)",
    },
    ProtocolRevision {
        version: 27,
        adr: "",
        summary: "contacts and message requests — ContactsState / RequestContact / AnswerContact / RemoveContact / SetContactSettings / LookupPerson / PersonInfo / GetNearbyPeople / NearbyPeople; a private chat with somebody who is not a contact starts as a request (docs/notes/contacts.md)",
    },
    ProtocolRevision {
        version: 28,
        adr: "",
        summary: "video on a prim — PrimDto.video (a clip in the simulator's video store, when it started, whether it loops) and SetPrimVideo to start or stop one; the clip is fetched over HTTP, not sent on the wire (docs/notes/video-on-a-prim.md)",
    },
    ProtocolRevision {
        version: 29,
        adr: "",
        summary: "ringing a closed app — SetPushEndpoint registers a device's UnifiedPush address; the server wakes it when a call starts in one of its chats, with no content in the push (docs/notes/calls.md)",
    },
    ProtocolRevision {
        version: 30,
        adr: "",
        summary: "where the people in a chat are — WherePeople / PeopleWhere: a private chat's World view asks where its members' avatars are, and hears only about contacts who show that they are online (docs/notes/contacts.md)",
    },
    ProtocolRevision {
        version: 31,
        adr: "",
        summary: "declining a call — CallDecline from the device that declines; CallDeclined to the chat's connected devices, so the caller hears at once instead of ringing out, and the decliner's other devices stop ringing (docs/notes/calls.md)",
    },
    ProtocolRevision {
        version: 32,
        adr: "",
        summary: "VP9 in calls — no message changes, but the app now sends VP9 video (receivers must decode VP8 and VP9, with VP9's own clear SFrame prefix), so a client that decodes only VP8 must not share a call (docs/notes/calls.md)",
    },
    ProtocolRevision {
        version: 33,
        adr: "",
        summary: "prim sound — PrimSound, a one-shot a script played (play_sound), and PrimDto::sound_loop, the loop a prim is playing (loop_sound / stop_sound); sounds are a closed library named by cartopolis_core::SoundName (docs/notes/prim-scripting.md)",
    },
    ProtocolRevision {
        version: 34,
        adr: "",
        summary: "physical prims — PrimDto.physical, SetPrimPhysical to switch it, and PrimMotion: where the simulator's rigid-body step has moved each physical prim, a compact pose + velocity per body instead of a PrimUpsert per tick (docs/notes/physical-prims.md)",
    },
    ProtocolRevision {
        version: 35,
        adr: "",
        summary: "permissions — Permissions, the account's granted permission slugs, sent after the handshake and again whenever its roles change, so a client offers what the server would allow (docs/notes/permissions.md)",
    },
    ProtocolRevision {
        version: 36,
        adr: "",
        summary: "hands and tones — PrimDto.hand_range (a script senses hands within it) and PrimDto.tone; HandPose, where a nearby high-precision client's hand is relative to such a prim, for its script; PrimTone, the synthesised tone a script set (docs/notes/hands-and-tones.md)",
    },
    ProtocolRevision {
        version: 37,
        adr: "",
        summary: "full addresses — ServerHelloAck.server_name; every person in an encrypted chat is name@server (E2eeClaimKeyPackages/E2eeKeyPackages.address, direct_with, E2eeWelcome.members, E2eeDeliver.sender, E2eeInvite.from, and each device's MLS credential), and a chat's id begins with its home server, so chats can later span servers (docs/notes/web-chat.md)",
    },
    ProtocolRevision {
        version: 38,
        adr: "",
        summary: "workspaces — WorkspaceCreate / WorkspaceList / Workspaces / WorkspaceAddMember, and channels: ChannelCreate lists an encrypted chat as a workspace channel, ChannelJoin asks into a public one, ChannelJoinRequest hands that to a member device to add them (docs/notes/web-chat.md)",
    },
    ProtocolRevision {
        version: 39,
        adr: "",
        summary: "communities and every device in every chat — workspaces, channels and rooms are gone (kinds 19 and 75–81 retired, ChatScope::Room removed); Community* and Topic* messages, moderators who delete (E2eeRedacted) and remove (TopicRemovePeople), and E2eeAddDevices, which hands a device in a chat the key packages of member devices that are not in it (docs/notes/communities.md, docs/notes/encrypted-chat.md)",
    },
    ProtocolRevision {
        version: 40,
        adr: "",
        summary: "community pictures, places and invite links — CommunitySettings.picture and .place, CommunityListing.picture, .place and .distance_m, DirectoryOrder::Near with CommunityDirectory.near, and CommunityInviteCreate / CommunityInvite / CommunityInviteLookup / CommunityJoinInvite (docs/notes/communities.md)",
    },
    ProtocolRevision {
        version: 41,
        adr: "",
        summary: "meetings — a call has hosts and settings (CallMember.role, .hand, .held, .sharing, .muted, .recording, .guest, .room; CallRoster.settings, .waiting, .spotlight, .rooms), a waiting room (CallWaiting, Admit / Deny), CallJoin.screen names a third, screen-share track (CallTrack.kind replaces .video), CallControl carries hands, sharing and host actions, CallEnded tells a removed device or everyone at an end-for-all, CallReact / CallReacted relay sealed reactions, CallWatch says whose cameras a device draws, and guests join by link: AuthCredential::Guest, CallInvite / CallInvited, CallKeyDrop / CallKeyDropped (docs/notes/calls.md)",
    },
    ProtocolRevision {
        version: 42,
        adr: "",
        summary: "avatar looks — SetAvatarLook, a client's MakeHuman look (AvatarLook: body and face sliders, skin, eyes, hair, wardrobe), which the server keeps with the account; AvatarLookOf, an avatar's look, sent to everyone when it changes or its owner arrives, and every online look to a client that has just connected (docs/notes/makehuman-avatars.md)",
    },
    ProtocolRevision {
        version: 43,
        adr: "",
        summary: "agent accounts — CallMember.agent, ContactDto.agent and ContactRequestDto.agent say an AI agent speaks for the account, so its badge shows before anybody calls or answers it (docs/mockups/voice-agent/)",
    },
    ProtocolRevision {
        version: 44,
        adr: "",
        summary: "faces — ClientIntent.expression and AvatarStateDto.expression carry an expression (Expression, held like an emote with a sequence) the avatar's face shows for a few seconds; SetFaceFrame / FaceFrameOf relay a face tracked by its owner's camera, 52 unit weights a frame, to the clients within speaking range (docs/notes/avatar-faces.md)",
    },
    ProtocolRevision {
        version: 45,
        adr: "",
        summary: "a more personal look — AvatarLook.skin_tone lightens or darkens the skin, AvatarLook.extras names things worn beyond the outfit (a beard, glasses), and AvatarLook.face carries the body's shape controls as well as the face's (docs/notes/makehuman-avatars.md)",
    },
    ProtocolRevision {
        version: 46,
        adr: "",
        summary: "the avatar comes back where it was — ServerHelloAck.last_place, where the account's avatar stood when its last session ended, kept by the sim per account, which the client puts the avatar back at on every sign-in and on any device (docs/notes/session-modes.md)",
    },
    ProtocolRevision {
        version: 47,
        adr: "",
        summary: "an avatar stands on Earth, and a client is sent what is within its reach — AvatarStateDto.geo and alt_m replace the frame-metre position, Teleport places by geo and alt_m, WorldSnapshot carries avatars only and is assembled per connection from the avatars within reach, things arrive as PrimUpsert when they come within reach and leave with PrimOutOfReach (docs/notes/interest-by-reach.md)",
    },
    ProtocolRevision {
        version: 48,
        adr: "",
        summary: "picking things up — GrabPrim asks to hold a physical prim small and light enough to carry, ReleasePrim throws it or puts it down, and AvatarStateDto.holding says which prim an avatar is carrying; the sim holds it in front of the avatar and it moves through PrimMotion like any other body (docs/notes/physical-prims.md)",
    },
    ProtocolRevision {
        version: 49,
        adr: "",
        summary: "greeting another avatar — OfferGreeting offers a handshake or a high five to an avatar within reach, GreetingOffered tells that avatar (or that the offer is withdrawn), AcceptGreeting takes it, and GreetingStarted tells both the sim has stood them together and started the gesture; Emote gains Handshake and HighFive, which only the sim starts, and Throw, the clip a throw plays (docs/notes/paired-gestures.md)",
    },
    ProtocolRevision {
        version: 50,
        adr: "",
        summary: "following a place for new plans — FollowPlans gives the server the spots this account follows, which it checks the municipality's new notices against and wakes a closed phone for with the push word \"plan\"; a phone says it knows that word by registering its address with the \"plans:\" prefix (docs/notes/plans.md)",
    },
    ProtocolRevision {
        version: 51,
        adr: "",
        summary: "drafts from the design room — PrimDto.draft and CreatePrim.draft: a prim created as a draft is sent only to its owner's connections, is not touched, carried or collided with by anyone else and its script senses nobody until it is placed (POST /api/inventory/drafts/place), which clears the flag and sends it to everyone within reach; and groups — PrimDto.groups is the chain of groups a prim is in, outermost first, set over HTTP (/api/inventory/groups) and sent in its PrimUpsert (docs/notes/design-room.md)",
    },
];

/// Current wire version, sent in every app frame; `decode_app_frame` rejects
/// mismatches (`ProtocolError::UnsupportedVersion`). Derived from
/// [`PROTOCOL_HISTORY`] — append there to bump it.
pub const PROTOCOL_VERSION: u16 = PROTOCOL_HISTORY[PROTOCOL_HISTORY.len() - 1].version;
