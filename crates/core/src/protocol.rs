//! Wire messages (ADR-009). TCP framing is length-delimited (ADR-008); each frame body is an **app frame**:
//! `protocol_version` (u16 LE) + `message_kind` (u16 LE) + `request_id` (u32 LE) + postcard(`NetMessage`).

use glam::Vec3;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::error::ProtocolError;
use crate::particles::ParticleEffect;
use crate::sounds::PrimSound;
use crate::tone::{HandPose, ToneSpec};

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
    // 5 was `ObserverUpdate`, retired with regions in protocol 23: it existed only
    // to aim the per-region area-of-interest filter, and there is no longer a
    // region to include or exclude. The number is left unassigned rather than
    // reused — a stale client sending 5 should be rejected, not misread as
    // something else.
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
    GetPrimScript = 20,
    SetPrimScript = 21,
    PrimScript = 22,
    ScriptOutput = 23,
    TouchPrim = 24,
    WorldCapacity = 25,
    VoiceFrame = 26,
    VoiceData = 27,
    SetBlocked = 28,
    BlockedList = 29,
    ReportUser = 30,
    ReportAck = 31,
    DeleteAccount = 32,
    AccountDeleted = 33,
    Teleport = 34,
    SetPresence = 35,
    E2eeRegisterDevice = 36,
    E2eeDeviceStatus = 37,
    E2eeClaimKeyPackages = 38,
    E2eeKeyPackages = 39,
    E2eeCreateGroup = 40,
    E2eeGroupCreated = 41,
    E2eeSend = 42,
    E2eeSent = 43,
    E2eeSync = 44,
    E2eeSynced = 45,
    E2eeDeliver = 46,
    E2eeInvite = 47,
    CallJoin = 48,
    CallJoined = 49,
    CallOffer = 50,
    CallAnswer = 51,
    CallLeave = 52,
    CallRoster = 53,
    ContactsState = 54,
    RequestContact = 55,
    AnswerContact = 56,
    RemoveContact = 57,
    SetContactSettings = 58,
    LookupPerson = 59,
    PersonInfo = 60,
    GetNearbyPeople = 61,
    NearbyPeople = 62,
    SetPrimVideo = 63,
    SetPushEndpoint = 64,
    WherePeople = 65,
    PeopleWhere = 66,
    CallDecline = 67,
    CallDeclined = 68,
    PrimSound = 69,
    SetPrimPhysical = 70,
    PrimMotion = 71,
    Permissions = 72,
    HandPose = 73,
    PrimTone = 74,
    E2eeAddDevices = 82,
    CommunityList = 83,
    Communities = 84,
    CommunityCreate = 85,
    CommunityCreated = 86,
    CommunityEdit = 87,
    CommunityDirectory = 88,
    CommunityListings = 89,
    CommunityJoin = 90,
    CommunityLeave = 91,
    CommunityDecide = 92,
    CommunityAddMember = 93,
    CommunitySetRole = 94,
    CommunityRemove = 95,
    CommunityDelete = 96,
    TopicCreate = 97,
    TopicEdit = 98,
    TopicAddPeople = 99,
    TopicRemovePeople = 100,
    TopicRemoved = 101,
    TopicLeft = 102,
    ModerateDelete = 103,
    E2eeRedacted = 104,
    CommunityReport = 105,
    CommunityReportResolve = 106,
    CommunityInviteCreate = 107,
    CommunityInvite = 108,
    CommunityInviteLookup = 109,
    CommunityJoinInvite = 110,
}

impl MessageKind {
    #[must_use]
    pub fn from_wire(v: u16) -> Option<Self> {
        match v {
            1 => Some(Self::ClientHello),
            2 => Some(Self::ServerHelloAck),
            3 => Some(Self::ServerError),
            4 => Some(Self::ClientIntent),
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
            20 => Some(Self::GetPrimScript),
            21 => Some(Self::SetPrimScript),
            22 => Some(Self::PrimScript),
            23 => Some(Self::ScriptOutput),
            24 => Some(Self::TouchPrim),
            25 => Some(Self::WorldCapacity),
            26 => Some(Self::VoiceFrame),
            27 => Some(Self::VoiceData),
            28 => Some(Self::SetBlocked),
            29 => Some(Self::BlockedList),
            30 => Some(Self::ReportUser),
            31 => Some(Self::ReportAck),
            32 => Some(Self::DeleteAccount),
            33 => Some(Self::AccountDeleted),
            34 => Some(Self::Teleport),
            35 => Some(Self::SetPresence),
            36 => Some(Self::E2eeRegisterDevice),
            37 => Some(Self::E2eeDeviceStatus),
            38 => Some(Self::E2eeClaimKeyPackages),
            39 => Some(Self::E2eeKeyPackages),
            40 => Some(Self::E2eeCreateGroup),
            41 => Some(Self::E2eeGroupCreated),
            42 => Some(Self::E2eeSend),
            43 => Some(Self::E2eeSent),
            44 => Some(Self::E2eeSync),
            45 => Some(Self::E2eeSynced),
            46 => Some(Self::E2eeDeliver),
            47 => Some(Self::E2eeInvite),
            48 => Some(Self::CallJoin),
            49 => Some(Self::CallJoined),
            50 => Some(Self::CallOffer),
            51 => Some(Self::CallAnswer),
            52 => Some(Self::CallLeave),
            53 => Some(Self::CallRoster),
            54 => Some(Self::ContactsState),
            55 => Some(Self::RequestContact),
            56 => Some(Self::AnswerContact),
            57 => Some(Self::RemoveContact),
            58 => Some(Self::SetContactSettings),
            59 => Some(Self::LookupPerson),
            60 => Some(Self::PersonInfo),
            61 => Some(Self::GetNearbyPeople),
            62 => Some(Self::NearbyPeople),
            63 => Some(Self::SetPrimVideo),
            64 => Some(Self::SetPushEndpoint),
            65 => Some(Self::WherePeople),
            66 => Some(Self::PeopleWhere),
            67 => Some(Self::CallDecline),
            68 => Some(Self::CallDeclined),
            69 => Some(Self::PrimSound),
            70 => Some(Self::SetPrimPhysical),
            71 => Some(Self::PrimMotion),
            72 => Some(Self::Permissions),
            73 => Some(Self::HandPose),
            74 => Some(Self::PrimTone),
            82 => Some(Self::E2eeAddDevices),
            83 => Some(Self::CommunityList),
            84 => Some(Self::Communities),
            85 => Some(Self::CommunityCreate),
            86 => Some(Self::CommunityCreated),
            87 => Some(Self::CommunityEdit),
            88 => Some(Self::CommunityDirectory),
            89 => Some(Self::CommunityListings),
            90 => Some(Self::CommunityJoin),
            91 => Some(Self::CommunityLeave),
            92 => Some(Self::CommunityDecide),
            93 => Some(Self::CommunityAddMember),
            94 => Some(Self::CommunitySetRole),
            95 => Some(Self::CommunityRemove),
            96 => Some(Self::CommunityDelete),
            97 => Some(Self::TopicCreate),
            98 => Some(Self::TopicEdit),
            99 => Some(Self::TopicAddPeople),
            100 => Some(Self::TopicRemovePeople),
            101 => Some(Self::TopicRemoved),
            102 => Some(Self::TopicLeft),
            103 => Some(Self::ModerateDelete),
            104 => Some(Self::E2eeRedacted),
            105 => Some(Self::CommunityReport),
            106 => Some(Self::CommunityReportResolve),
            107 => Some(Self::CommunityInviteCreate),
            108 => Some(Self::CommunityInvite),
            109 => Some(Self::CommunityInviteLookup),
            110 => Some(Self::CommunityJoinInvite),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct TextureEntry {
    pub id: String,
    pub name: String,
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
/// `Default` is null island, and it means "not placed yet" rather than a place —
/// the one thing that reads it is a script's view of a prim before the first tick
/// has filled it in, where every sensor answer is "nobody" and that is correct.
#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize, PartialEq)]
pub struct GeoAnchor {
    pub latitude: f64,
    pub longitude: f64,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct PrimDto {
    pub id: i64,
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
    /// A stored model asset that stands in for [`Self::shape`] (protocol v22).
    ///
    /// The value is an asset hash — the name the simulator stores the glTF
    /// binary under, and the whole of the address: the client fetches
    /// `/api/assets/{model}.glb` and, because a file named by its own content
    /// can never change, caches it forever.
    ///
    /// **A hash and not an inventory item id.** An item is a person's *named
    /// reference* to a file and it can be renamed, moved to a folder or thrown
    /// away; the prim in the street must not go blank when it is. Two prims made
    /// from two people's copies of the same bench also carry the same value here,
    /// which is what lets the client hold one mesh for both.
    ///
    /// Every other field keeps its meaning when this is set — the scale, the
    /// rotation, the geo anchor, the colour tint, the script and the particles
    /// are all about the object, not about how its surface is generated. The
    /// deformation set (hollow, twist, taper) is the exception: it describes the
    /// procedural shape and is ignored.
    #[serde(default)]
    pub model: Option<String>,
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
    /// A particle emitter on this prim, if it has one (protocol v20). `None` is
    /// the overwhelmingly common case and costs one byte.
    #[serde(default)]
    pub particles: Option<ParticleEffect>,
    /// Whether this prim carries a script (protocol v21).
    ///
    /// **The client cannot work this out.** Script source is deliberately kept
    /// out of `WorldSnapshot` — it is an order of magnitude larger than the rest
    /// of a prim and of interest to one person — so "does pressing this do
    /// anything?" has no answer on the client without the server saying. It is
    /// what `systems::interact` gates its prompt on: offering to press every
    /// crate in the city would be noise, and offering to press nothing would mean
    /// a button nobody can find.
    ///
    /// It says a script is *stored*, not that it is running: a source that failed
    /// to compile is stored and faulted, and pressing it does nothing. Reporting
    /// "stored" is the honest half — the client has no business knowing about a
    /// compile error on somebody else's object.
    #[serde(default)]
    pub scripted: bool,
    /// A video playing on the prim's surface (protocol 28,
    /// `docs/notes/video-on-a-prim.md`), or `None`. Set only by
    /// [`NetMessage::SetPrimVideo`] — never by `UpdatePrim`, which is a
    /// full-state edit every tool sends, so that a colour change cannot stop a
    /// video by forgetting to carry it.
    #[serde(default)]
    pub video: Option<PrimVideo>,
    /// The sound this prim is looping (`loop_sound`, protocol 33), or `None`.
    /// A level rather than an event, so somebody walking up to a fountain hears
    /// it. Set only by the prim's script; held in the simulator's memory and
    /// **not persisted** — a restart runs every script's `on_start` again, which
    /// is where a standing loop is started.
    #[serde(default)]
    pub sound_loop: Option<PrimSound>,
    /// The simulator moves this prim: it falls, collides with other prims and
    /// with avatars, and can be pushed and kicked (protocol 34,
    /// `docs/notes/physical-prims.md`). Second Life's *Physical* checkbox. Set
    /// only by [`NetMessage::SetPrimPhysical`] and by a script, never by
    /// `UpdatePrim`; while it is set the prim's pose arrives in
    /// [`NetMessage::PrimMotion`].
    #[serde(default)]
    pub physical: bool,
    /// The prim's script wants the hands within this many metres
    /// (`sense_hands`, protocol 36); 0 when it wants none. Clients read it to
    /// know which prims to report a hand to. Set only by the script and, like
    /// `sound_loop`, not persisted.
    #[serde(default)]
    pub hand_range: f32,
    /// The tone the prim's script is sounding (`set_tone`), or `None`. Here
    /// for a client arriving while it sounds; changes arrive as
    /// [`NetMessage::PrimTone`].
    #[serde(default)]
    pub tone: Option<ToneSpec>,
}

/// A video on a prim: which clip, and when it started, so everybody near it
/// sees the same moment.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PrimVideo {
    /// The clip's content hash in the simulator's video store (`/api/videos`).
    pub asset: String,
    /// When it started, in Unix milliseconds by the simulator's clock. A
    /// client plays from `now - started_at_ms`, so sync is only as good as the
    /// two clocks' agreement — seconds at worst on devices that keep time.
    pub started_at_ms: u64,
    /// Start again at the end, rather than stop on the last picture.
    pub looped: bool,
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
    /// Set while this avatar is sitting on something: metres in world XZ from
    /// [`Self::position`] to where its body is drawn.
    ///
    /// **An offset, not a place**, for two reasons. The sim has no map and no idea
    /// what a bench is, so it cannot hold a seat as a location it could check;
    /// and the sitter's own client measured the seat in the frame its map is
    /// anchored in, which is a *translation* away from every other client's. A
    /// delta survives that translation, a position does not. It is bounded by the
    /// sim ([`MAX_SEAT_OFFSET_M`]) — an avatar may lean out of its authoritative
    /// position by about the width of a bench, not by a street.
    ///
    /// The facing needs no field of its own: a sitter turns to face out of the
    /// seat, and that is already [`Self::yaw`].
    pub seated: Option<[f32; 2]>,
    /// World velocity (m/s) the sim integrated this avatar with on the tick the
    /// snapshot was taken: horizontal travel including the flight lift, plus
    /// the vertical rate of a jump, a fall or a climb. Zero while seated.
    ///
    /// Replicated rather than derived because a receiver cannot derive it
    /// well. Differencing two snapshots is a tick late and noisy at the tick
    /// rate, and differencing the *smoothed* position — which is what the
    /// client did — gets it wrong exactly at a start or a stop, the moments
    /// the gait changes. With it, a receiver draws another avatar where it is
    /// now (`position + velocity × age`) instead of trailing where it was.
    pub velocity: Vec3,
}

/// How far a seat may draw an avatar from the position the sim holds for it.
///
/// A seat is the one thing that separates where an avatar *is* from where it is
/// *drawn*, so it needs a ceiling: without one a client could sit and put its
/// body an arbitrary distance away, which is a way to appear somewhere it is not
/// and to be missed by anything that reasons about proximity. The furthest real
/// seat is the far bench of a picnic table, a little over a metre off, reached
/// from a standing position the colliders keep about 0.6 m short of it.
pub const MAX_SEAT_OFFSET_M: f32 = 2.0;

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
        /// Where the avatar is sitting, as metres from its own position — see
        /// [`AvatarStateDto::seated`] for why it is an offset. A *level*, not an
        /// edge: it is set for as long as the sit lasts, so an avatar that comes
        /// into range mid-sit is drawn sitting. The sim clears it the moment the
        /// same intent asks to move.
        seated: Option<[f32; 2]>,
        /// Where this avatar is on Earth (protocol v20), as its own client
        /// measures it.
        ///
        /// **The sim cannot derive this and it is not a second copy of
        /// `position`.** An avatar's sim-space position is metres in the frame
        /// its own client's map is anchored in, and every client anchors
        /// somewhere else and re-anchors when it teleports — so "is this avatar
        /// near that prim?" is not a question sim space can answer, while a prim
        /// has carried an absolute `GeoAnchor` since ADR-029. This is the other
        /// half of that pair, and `sensor` is what it exists for.
        ///
        /// `None` from a client that does not know yet (before the map has
        /// anchored) — a sensor simply does not see such an avatar, which is the
        /// right answer rather than a guessed position.
        geo: Option<GeoAnchor>,
    },
    WorldSnapshot {
        tick: u64,
        prims: Vec<PrimDto>,
        avatars: Vec<AvatarStateDto>,
    },
    /// ADR-011 delta v0: remove prim by stable id (server-assigned).
    PrimRemoved { id: i64 },
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
    /// `position` is in the same frame `AvatarStateDto` already uses. Trusted like an intent is trusted (the sim has no map to
    /// validate a destination against); non-finite components discard the whole
    /// message, same rule as `apply_intent`.
    Teleport { request_id: u32, position: Vec3 },
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
}

/// How long an invite link works, in days.
pub const COMMUNITY_INVITE_DAYS: u64 = 14;

/// A person's standing in a community ([`CommunityDto`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum CommunityRole {
    /// Started it, or was handed it. Appoints moderators, hands it over,
    /// deletes it. There is exactly one.
    Owner,
    /// Lets people in, adds and removes them, deletes messages, edits the
    /// community and its topics.
    Moderator,
    Member,
}

impl CommunityRole {
    /// Whether this role moderates (the owner does too).
    #[must_use]
    pub fn moderates(self) -> bool {
        matches!(self, Self::Owner | Self::Moderator)
    }
}

/// How somebody gets into a community.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum JoinRule {
    /// Anyone who finds it joins straight away.
    Open,
    /// Anyone may ask; a moderator confirms.
    Ask,
    /// Only people a moderator adds.
    Invite,
}

/// How a directory search is sorted.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum DirectoryOrder {
    /// Most members first.
    Popular,
    /// Newest first.
    New,
    /// Nearest first, to the `near` point of the search; a community pinned
    /// to no place sorts last.
    Near,
}

/// What a community's owner and moderators set.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CommunitySettings {
    pub name: String,
    /// One line about it. May be empty.
    pub about: String,
    pub join: JoinRule,
    /// Shown in the directory. Its name, description, topic names, moderators
    /// and size are then public on this server.
    pub listed: bool,
    /// Whether members, not only moderators, may start topics.
    pub members_start_topics: bool,
    /// Its picture, by its hash in the picture store (`/api/pictures`): a small
    /// JPEG, not sealed, since everybody it is listed to may see it.
    pub picture: Option<String>,
    /// Where it is, for a neighbourhood or a club house: what "Near you" sorts by.
    pub place: Option<CommunityPlace>,
}

/// The place a community is pinned to ([`CommunitySettings::place`]).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CommunityPlace {
    pub latitude: f64,
    pub longitude: f64,
    /// How people know it ("Oosterpoort", "the club house").
    pub name: String,
}

/// One community, as one of its members (or somebody waiting to join) sees
/// it ([`NetMessage::Communities`]).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CommunityDto {
    pub id: Uuid,
    /// The server it lives on, which orders all of its topics.
    pub home: String,
    pub settings: CommunitySettings,
    /// The asking account's role; `None` while its request to join waits.
    pub role: Option<CommunityRole>,
    /// Everybody in it. Empty while waiting.
    pub members: Vec<CommunityMemberDto>,
    /// Its topics, in their order. Empty while waiting.
    pub topics: Vec<TopicDto>,
    /// Requests to join. Only a moderator is sent these.
    pub requests: Vec<JoinRequestDto>,
    /// Open reports. Only a moderator is sent these.
    pub reports: Vec<CommunityReportDto>,
}

/// One member of a community.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CommunityMemberDto {
    /// Full address (`name@server`).
    pub address: String,
    pub role: CommunityRole,
}

/// One topic of a community.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TopicDto {
    /// The encrypted chat it is (`E2eeCreateGroup`'s id).
    pub group: Vec<u8>,
    pub name: String,
    pub position: u32,
    pub archived: bool,
    /// Whether the asking account is in it: false for a moment after joining
    /// the community, until a member's device has added it, and for good
    /// after a moderator took it out of this one topic.
    pub joined: bool,
}

/// Somebody asking to join ([`CommunityDto::requests`]).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct JoinRequestDto {
    pub address: String,
    pub message: String,
    pub requested_ms: u64,
}

/// A report made to a community's moderators.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CommunityReportDto {
    pub id: i64,
    pub reporter: String,
    pub reported: String,
    pub group: Vec<u8>,
    pub seq: u64,
    pub reason: ReportReason,
    pub note: String,
    pub line: String,
    pub created_ms: u64,
}

/// A listed community, as the directory shows it to anybody on the server.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CommunityListing {
    pub id: Uuid,
    pub name: String,
    pub about: String,
    pub join: JoinRule,
    pub members: u32,
    pub topics: Vec<String>,
    /// The owner and the moderators.
    pub moderators: Vec<CommunityMemberDto>,
    /// The asking account's standing: in it, waiting, or neither.
    pub mine: ListingState,
    pub created_ms: u64,
    pub picture: Option<String>,
    pub place: Option<CommunityPlace>,
    /// Metres from the search's `near` point, when it had one and this has a place.
    pub distance_m: Option<f64>,
}

/// Where the asking account stands with a listed community.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum ListingState {
    Outside,
    Asked,
    Member,
    /// Removed and blocked from rejoining, or turned down within the last week.
    Refused,
}

/// A topic's name as it is stored and shown after a `#`: lower case, spaces
/// as dashes, only letters, digits, `-` and `_`. One rule for the server that
/// stores it and the device that labels the chat, so the two always agree.
#[must_use]
pub fn topic_name(name: &str) -> String {
    let mut out = String::new();
    for c in name.trim().trim_start_matches('#').chars() {
        if c.is_alphanumeric() || c == '-' || c == '_' {
            out.extend(c.to_lowercase());
        } else if c.is_whitespace() && !out.ends_with('-') {
            out.push('-');
        }
    }
    out.trim_matches('-').to_string()
}

/// One physical prim's pose and velocity ([`NetMessage::PrimMotion`]).
///
/// The pose is the same pair a prim is always placed by — the geo anchor for
/// the horizontal and an altitude for the vertical — so a client applies it
/// exactly as it applies a `PrimUpsert`, with no frame of the server's own to
/// translate from. The velocity is in the client world frame's axes (x east,
/// y up, z south: every client's frame shares them, only the origin differs),
/// which is what lets a receiver carry a body forward between ticks.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct PrimBody {
    pub id: i64,
    pub geo: GeoAnchor,
    /// Metres above the ground, as `PrimDto::position.y`.
    pub altitude: f32,
    /// Orientation as a quaternion `[x, y, z, w]` — not the Euler angles
    /// `PrimDto::rotation` stores, because a tumbling body is interpolated, and
    /// interpolating Euler angles is how a ball spins the long way round.
    pub rotation: [f32; 4],
    /// Metres per second.
    pub velocity: Vec3,
    /// The body has come to rest: place it exactly here and stop carrying it
    /// forward.
    pub resting: bool,
}

/// One person's avatar on Earth ([`NetMessage::PeopleWhere`]).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PersonWhereDto {
    pub username: String,
    /// The avatar's id, the same number as in `WorldSnapshot`, so a client that
    /// also has the avatar in view can put the marker on it exactly.
    pub avatar_id: u64,
    pub latitude: f64,
    pub longitude: f64,
}

/// What [`NetMessage::SetPrimVideo`] asks for; the server adds the start time.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SetVideo {
    pub asset: String,
    pub looped: bool,
}

/// One track on a device's receiving connection ([`NetMessage::CallOffer`]).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct CallTrack {
    /// The SDP media id it arrives under.
    pub mid: String,
    /// Whose it is, as [`CallMember::participant`] numbers them.
    pub participant: u64,
    pub video: bool,
}

/// One device in a call ([`NetMessage::CallRoster`]).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct CallMember {
    pub participant: u64,
    pub username: String,
    pub device: Uuid,
}

/// Who may send you a message request (`docs/notes/contacts.md`). Anyone with
/// your contact link may, whatever this says.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
pub enum RequestPolicy {
    /// Anybody, including by typing your username.
    Anyone,
    /// People who were near your avatar in the last hour, or who share a
    /// contact with you.
    #[default]
    Near,
    /// Nobody finds you by name or in the world; only your link works.
    LinkOnly,
}

impl RequestPolicy {
    /// The number stored in the database.
    #[must_use]
    pub fn to_db(self) -> i64 {
        match self {
            Self::Anyone => 0,
            Self::Near => 1,
            Self::LinkOnly => 2,
        }
    }

    /// Read back what [`Self::to_db`] wrote; anything unknown is the default.
    #[must_use]
    pub fn from_db(v: i64) -> Self {
        match v {
            0 => Self::Anyone,
            2 => Self::LinkOnly,
            _ => Self::Near,
        }
    }
}

/// This account's own contact settings.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct ContactSettings {
    pub policy: RequestPolicy,
    /// Contacts can see whether this account is signed in.
    pub show_online: bool,
    /// Say so in the app when a request arrives.
    pub notify_requests: bool,
    /// Requests from accounts younger than a week wait in their own section.
    pub separate_new_accounts: bool,
    /// The code in this account's contact link.
    pub link_code: String,
}

/// Where and when two people's avatars were last near each other, as the server
/// saw it. Only ever sent to one of the two.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct MetDto {
    pub at_ms: u64,
    pub latitude: f64,
    pub longitude: f64,
}

/// One contact.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ContactDto {
    pub username: String,
    pub since_ms: u64,
    /// Where you were when one of you sent the request, if you were near.
    pub met: Option<MetDto>,
    /// `None` when they do not show it.
    pub online: Option<bool>,
}

/// One request, either way round.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ContactRequestDto {
    /// The other person.
    pub username: String,
    pub sent_at_ms: u64,
    pub met: Option<MetDto>,
    /// The other person's account is younger than a week.
    pub new_account: bool,
    /// When the other person's account was made.
    pub joined_ms: u64,
    /// Contacts you both have.
    pub in_common: Vec<String>,
}

/// How the asker stands with a person.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum Relation {
    Nobody,
    Me,
    Contact,
    /// The asker sent them a request (waiting, as far as the asker knows).
    RequestSent,
    /// They sent the asker a request.
    RequestReceived,
}

/// What the person card shows.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PersonDto {
    pub username: String,
    pub joined_ms: u64,
    pub new_account: bool,
    pub in_common: Vec<String>,
    pub relation: Relation,
    pub met: Option<MetDto>,
    /// Metres between the two avatars now, when both are in the world.
    pub distance_m: Option<f32>,
    /// A request from the asker would be accepted by the server right now.
    pub may_request: bool,
}

/// One person near the asker.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct NearbyPersonDto {
    pub username: String,
    pub distance_m: f32,
    pub relation: Relation,
    pub may_request: bool,
}

/// One device's key package, as handed out by [`NetMessage::E2eeKeyPackages`].
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct E2eeKeyPackage {
    pub device: Uuid,
    pub bytes: Vec<u8>,
}

/// A member's device missing from a chat ([`NetMessage::E2eeAddDevices`]).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct E2eeNewDevice {
    /// The full address of the account it belongs to.
    pub address: String,
    pub package: E2eeKeyPackage,
}

/// An invitation riding on a commit ([`NetMessage::E2eeSend`]).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct E2eeWelcome {
    /// The devices it is addressed to (the ones whose key packages were used).
    pub devices: Vec<Uuid>,
    /// The full addresses of the accounts those devices belong to, which become
    /// members of the chat for delivery purposes. The server checks each device
    /// belongs to one.
    pub members: Vec<String>,
    pub bytes: Vec<u8>,
}

/// "I have read chat `group` up to and including `after`."
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct E2eeCursor {
    pub group: Vec<u8>,
    pub after: u64,
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
/// The field is a `String` off the wire and its only other bound is
/// `MAX_FRAME`, which makes an unbounded one an allocation multiplier. Long enough for a paragraph explaining
/// what happened, short enough that a report table cannot be used as storage.
pub const MAX_REPORT_NOTE_LEN: usize = 2000;

/// Longest reported-message excerpt the server will store, in characters.
pub const MAX_REPORT_MESSAGE_LEN: usize = 2000;

/// Severity of a [`NetMessage::ScriptOutput`] line.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum ScriptLevel {
    /// `log` from a running script.
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
        NetMessage::GetPrimScript { .. } => MessageKind::GetPrimScript,
        NetMessage::SetPrimScript { .. } => MessageKind::SetPrimScript,
        NetMessage::PrimScript { .. } => MessageKind::PrimScript,
        NetMessage::ScriptOutput { .. } => MessageKind::ScriptOutput,
        NetMessage::TouchPrim { .. } => MessageKind::TouchPrim,
        NetMessage::WorldCapacity { .. } => MessageKind::WorldCapacity,
        NetMessage::VoiceFrame { .. } => MessageKind::VoiceFrame,
        NetMessage::VoiceData { .. } => MessageKind::VoiceData,
        NetMessage::SetBlocked { .. } => MessageKind::SetBlocked,
        NetMessage::BlockedList { .. } => MessageKind::BlockedList,
        NetMessage::ReportUser { .. } => MessageKind::ReportUser,
        NetMessage::ReportAck { .. } => MessageKind::ReportAck,
        NetMessage::DeleteAccount { .. } => MessageKind::DeleteAccount,
        NetMessage::AccountDeleted => MessageKind::AccountDeleted,
        NetMessage::Teleport { .. } => MessageKind::Teleport,
        NetMessage::SetPresence { .. } => MessageKind::SetPresence,
        NetMessage::E2eeRegisterDevice { .. } => MessageKind::E2eeRegisterDevice,
        NetMessage::E2eeDeviceStatus { .. } => MessageKind::E2eeDeviceStatus,
        NetMessage::E2eeClaimKeyPackages { .. } => MessageKind::E2eeClaimKeyPackages,
        NetMessage::E2eeKeyPackages { .. } => MessageKind::E2eeKeyPackages,
        NetMessage::E2eeCreateGroup { .. } => MessageKind::E2eeCreateGroup,
        NetMessage::E2eeGroupCreated { .. } => MessageKind::E2eeGroupCreated,
        NetMessage::E2eeSend { .. } => MessageKind::E2eeSend,
        NetMessage::E2eeSent { .. } => MessageKind::E2eeSent,
        NetMessage::E2eeSync { .. } => MessageKind::E2eeSync,
        NetMessage::E2eeSynced { .. } => MessageKind::E2eeSynced,
        NetMessage::E2eeDeliver { .. } => MessageKind::E2eeDeliver,
        NetMessage::E2eeInvite { .. } => MessageKind::E2eeInvite,
        NetMessage::CallJoin { .. } => MessageKind::CallJoin,
        NetMessage::CallJoined { .. } => MessageKind::CallJoined,
        NetMessage::CallOffer { .. } => MessageKind::CallOffer,
        NetMessage::CallAnswer { .. } => MessageKind::CallAnswer,
        NetMessage::CallLeave { .. } => MessageKind::CallLeave,
        NetMessage::CallRoster { .. } => MessageKind::CallRoster,
        NetMessage::ContactsState { .. } => MessageKind::ContactsState,
        NetMessage::RequestContact { .. } => MessageKind::RequestContact,
        NetMessage::AnswerContact { .. } => MessageKind::AnswerContact,
        NetMessage::RemoveContact { .. } => MessageKind::RemoveContact,
        NetMessage::SetContactSettings { .. } => MessageKind::SetContactSettings,
        NetMessage::LookupPerson { .. } => MessageKind::LookupPerson,
        NetMessage::PersonInfo { .. } => MessageKind::PersonInfo,
        NetMessage::GetNearbyPeople { .. } => MessageKind::GetNearbyPeople,
        NetMessage::NearbyPeople { .. } => MessageKind::NearbyPeople,
        NetMessage::SetPrimVideo { .. } => MessageKind::SetPrimVideo,
        NetMessage::SetPushEndpoint { .. } => MessageKind::SetPushEndpoint,
        NetMessage::WherePeople { .. } => MessageKind::WherePeople,
        NetMessage::PeopleWhere { .. } => MessageKind::PeopleWhere,
        NetMessage::CallDecline { .. } => MessageKind::CallDecline,
        NetMessage::CallDeclined { .. } => MessageKind::CallDeclined,
        NetMessage::PrimSound { .. } => MessageKind::PrimSound,
        NetMessage::SetPrimPhysical { .. } => MessageKind::SetPrimPhysical,
        NetMessage::PrimMotion { .. } => MessageKind::PrimMotion,
        NetMessage::Permissions { .. } => MessageKind::Permissions,
        NetMessage::HandPose { .. } => MessageKind::HandPose,
        NetMessage::PrimTone { .. } => MessageKind::PrimTone,
        NetMessage::E2eeAddDevices { .. } => MessageKind::E2eeAddDevices,
        NetMessage::CommunityList { .. } => MessageKind::CommunityList,
        NetMessage::Communities { .. } => MessageKind::Communities,
        NetMessage::CommunityCreate { .. } => MessageKind::CommunityCreate,
        NetMessage::CommunityCreated { .. } => MessageKind::CommunityCreated,
        NetMessage::CommunityEdit { .. } => MessageKind::CommunityEdit,
        NetMessage::CommunityDirectory { .. } => MessageKind::CommunityDirectory,
        NetMessage::CommunityListings { .. } => MessageKind::CommunityListings,
        NetMessage::CommunityJoin { .. } => MessageKind::CommunityJoin,
        NetMessage::CommunityLeave { .. } => MessageKind::CommunityLeave,
        NetMessage::CommunityDecide { .. } => MessageKind::CommunityDecide,
        NetMessage::CommunityAddMember { .. } => MessageKind::CommunityAddMember,
        NetMessage::CommunitySetRole { .. } => MessageKind::CommunitySetRole,
        NetMessage::CommunityRemove { .. } => MessageKind::CommunityRemove,
        NetMessage::CommunityDelete { .. } => MessageKind::CommunityDelete,
        NetMessage::TopicCreate { .. } => MessageKind::TopicCreate,
        NetMessage::TopicEdit { .. } => MessageKind::TopicEdit,
        NetMessage::TopicAddPeople { .. } => MessageKind::TopicAddPeople,
        NetMessage::TopicRemovePeople { .. } => MessageKind::TopicRemovePeople,
        NetMessage::TopicRemoved { .. } => MessageKind::TopicRemoved,
        NetMessage::TopicLeft { .. } => MessageKind::TopicLeft,
        NetMessage::ModerateDelete { .. } => MessageKind::ModerateDelete,
        NetMessage::E2eeRedacted { .. } => MessageKind::E2eeRedacted,
        NetMessage::CommunityReport { .. } => MessageKind::CommunityReport,
        NetMessage::CommunityReportResolve { .. } => MessageKind::CommunityReportResolve,
        NetMessage::CommunityInviteCreate { .. } => MessageKind::CommunityInviteCreate,
        NetMessage::CommunityInvite { .. } => MessageKind::CommunityInvite,
        NetMessage::CommunityInviteLookup { .. } => MessageKind::CommunityInviteLookup,
        NetMessage::CommunityJoinInvite { .. } => MessageKind::CommunityJoinInvite,
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
        NetMessage::Teleport { request_id, .. } => *request_id,
        NetMessage::SetPresence { request_id, .. } => *request_id,
        NetMessage::E2eeRegisterDevice { request_id, .. } => *request_id,
        NetMessage::E2eeDeviceStatus { request_id, .. } => *request_id,
        NetMessage::E2eeClaimKeyPackages { request_id, .. } => *request_id,
        NetMessage::E2eeKeyPackages { request_id, .. } => *request_id,
        NetMessage::E2eeCreateGroup { request_id, .. } => *request_id,
        NetMessage::E2eeGroupCreated { request_id, .. } => *request_id,
        NetMessage::E2eeSend { request_id, .. } => *request_id,
        NetMessage::E2eeSent { request_id, .. } => *request_id,
        NetMessage::E2eeSync { request_id, .. } => *request_id,
        NetMessage::E2eeSynced { request_id } => *request_id,
        NetMessage::CallJoin { request_id, .. } => *request_id,
        NetMessage::CallJoined { request_id, .. } => *request_id,
        NetMessage::RequestContact { request_id, .. } => *request_id,
        NetMessage::AnswerContact { request_id, .. } => *request_id,
        NetMessage::RemoveContact { request_id, .. } => *request_id,
        NetMessage::SetContactSettings { request_id, .. } => *request_id,
        NetMessage::LookupPerson { request_id, .. } => *request_id,
        NetMessage::SetPrimVideo { request_id, .. } => *request_id,
        NetMessage::SetPrimPhysical { request_id, .. } => *request_id,
        NetMessage::WherePeople { request_id, .. } => *request_id,
        NetMessage::PeopleWhere { request_id, .. } => *request_id,
        NetMessage::PersonInfo { request_id, .. } => *request_id,
        NetMessage::GetNearbyPeople { request_id } => *request_id,
        NetMessage::NearbyPeople { request_id, .. } => *request_id,
        NetMessage::CommunityList { request_id, .. } => *request_id,
        NetMessage::Communities { request_id, .. } => *request_id,
        NetMessage::CommunityCreate { request_id, .. } => *request_id,
        NetMessage::CommunityCreated { request_id, .. } => *request_id,
        NetMessage::CommunityEdit { request_id, .. } => *request_id,
        NetMessage::CommunityDirectory { request_id, .. } => *request_id,
        NetMessage::CommunityListings { request_id, .. } => *request_id,
        NetMessage::CommunityJoin { request_id, .. } => *request_id,
        NetMessage::CommunityLeave { request_id, .. } => *request_id,
        NetMessage::CommunityDecide { request_id, .. } => *request_id,
        NetMessage::CommunityAddMember { request_id, .. } => *request_id,
        NetMessage::CommunitySetRole { request_id, .. } => *request_id,
        NetMessage::CommunityRemove { request_id, .. } => *request_id,
        NetMessage::CommunityDelete { request_id, .. } => *request_id,
        NetMessage::TopicCreate { request_id, .. } => *request_id,
        NetMessage::TopicEdit { request_id, .. } => *request_id,
        NetMessage::ModerateDelete { request_id, .. } => *request_id,
        NetMessage::CommunityReport { request_id, .. } => *request_id,
        NetMessage::CommunityReportResolve { request_id, .. } => *request_id,
        NetMessage::CommunityInviteCreate { request_id, .. } => *request_id,
        NetMessage::CommunityInvite { request_id, .. } => *request_id,
        NetMessage::CommunityInviteLookup { request_id, .. } => *request_id,
        NetMessage::CommunityJoinInvite { request_id, .. } => *request_id,
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
    let kind = message_kind(msg) as u16;
    let rid = message_request_id(msg);
    let mut out = Vec::with_capacity(APP_HEADER_LEN + 64);
    out.extend_from_slice(&PROTOCOL_VERSION.to_le_bytes());
    out.extend_from_slice(&kind.to_le_bytes());
    out.extend_from_slice(&rid.to_le_bytes());
    // Serialised straight after the header, not into a second buffer that is
    // then copied: a full snapshot is the size of the world.
    postcard::to_extend(msg, out)
}

/// The kind in an encoded frame's header, without decoding its payload.
///
/// For the one caller that has to *route* a frame it is not going to read:
/// `net::handle_connection` fans out already-encoded bytes from a broadcast
/// channel carrying snapshots, prim upserts and capacity updates together, and an
/// idle client (`SetPresence`) must drop only the snapshots. Decoding a full
/// `WorldSnapshot` — every region, prim and avatar — per connection per tick, to
/// then throw it away, is the obvious way to write that and is far more expensive
/// than the send it saves.
///
/// Reads the header only, so an unknown kind or a truncated frame is `None`
/// rather than an error: a caller that cannot classify a frame should pass it
/// along and let the real decoder produce the diagnosis.
#[must_use]
pub fn frame_kind(bytes: &[u8]) -> Option<MessageKind> {
    if bytes.len() < APP_HEADER_LEN {
        return None;
    }
    MessageKind::from_wire(u16::from_le_bytes(bytes[2..4].try_into().unwrap()))
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

    /// The frame is the header then exactly `encode_message`'s bytes: encoding
    /// straight after the header must not change a byte on the wire.
    #[test]
    fn an_app_frame_is_the_header_then_the_message() {
        let m = NetMessage::ClientHello {
            protocol_version: PROTOCOL_VERSION,
            auth: AuthCredential::Password {
                username: "dev".into(),
                password: "x".repeat(300),
            },
        };
        let b = encode_app_frame(&m).unwrap();
        assert_eq!(b.len(), APP_HEADER_LEN + encode_message(&m).unwrap().len());
        assert_eq!(&b[..2], &PROTOCOL_VERSION.to_le_bytes());
        assert_eq!(&b[APP_HEADER_LEN..], &encode_message(&m).unwrap()[..]);
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
            api_base_url: String::new(),
            server_name: "cartopolis.org".into(),
        };
        let b = encode_app_frame(&m).unwrap();
        let m2 = decode_app_frame(&b).unwrap();
        assert_eq!(m, m2);
    }

    #[test]
    fn roundtrip_snapshot_app_frame() {
        let m = NetMessage::WorldSnapshot {
            tick: 42,
            prims: vec![],
            avatars: vec![],
        };
        let b = encode_app_frame(&m).unwrap();
        let m2 = decode_app_frame(&b).unwrap();
        assert_eq!(m, m2);
    }

    /// The seat travels both ways (protocol 19), and it survives the encoding as
    /// an *absent* value too — `None` is the state almost every intent and almost
    /// every replicated avatar is in, so it is the one that must not cost
    /// anything or go missing.
    #[test]
    fn roundtrip_seated_app_frames() {
        for seated in [None, Some([0.42, -1.03])] {
            let m = NetMessage::ClientIntent {
                request_id: 0,
                move_x: 0.0,
                move_z: 0.0,
                display_yaw: 1.25,
                flying: false,
                fly_up: false,
                fly_down: false,
                jump: false,
                emote: None,
                seated,
                geo: None,
            };
            let b = encode_app_frame(&m).unwrap();
            assert_eq!(decode_app_frame(&b).unwrap(), m);

            let m = NetMessage::WorldSnapshot {
                tick: 7,
                prims: vec![],
                avatars: vec![AvatarStateDto {
                    id: 3,
                    position: Vec3::new(1.0, 0.05, 2.0),
                    yaw: 1.25,
                    emote: None,
                    seated,
                    velocity: Vec3::new(1.8, 0.0, -0.5),
                }],
            };
            let b = encode_app_frame(&m).unwrap();
            assert_eq!(decode_app_frame(&b).unwrap(), m);
        }
    }

    #[test]
    fn roundtrip_create_prim_app_frame() {
        let m = NetMessage::CreatePrim {
            request_id: 123,
            name: "New Prim".into(),
            position: Vec3::new(10.0, 5.0, 20.0),
            rotation: Vec3::ZERO,
            scale: Vec3::ONE,
            color: [0.5, 0.5, 0.5],
            texture_id: None,
            model: None,
            shape: "box".into(),
            surface: PrimSurface::default(),
            geometry: PrimGeometry::default(),
            geo: GeoAnchor {
                latitude: 53.2194,
                longitude: 6.5665,
            },
            particles: Some(crate::particles::ParticleEffect::fire()),
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
            model: Some("2f8a1c".into()),
            name: "My Prim".into(),
            surface: PrimSurface::default(),
            geometry: PrimGeometry::default(),
            geo: GeoAnchor {
                latitude: 53.2194,
                longitude: 6.5665,
            },
            particles: None,
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
                scope: ChatScope::Global,
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
                note: "followed me home and would not leave".into(),
                message: Some("the line they sent".into()),
                asset: None,
            },
            NetMessage::ReportUser {
                request_id: 5,
                username: "loud".into(),
                reason: ReportReason::Other,
                note: String::new(),
                message: None,
                // The other shape a report has since protocol 22: raised from an
                // object rather than a chat line.
                asset: Some("d0".repeat(32)),
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
                scope: ChatScope::Global,
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
                channel: "voice:global".into(),
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
    fn roundtrip_call_app_frames() {
        let group = vec![7u8; 16];
        for m in [
            NetMessage::CallJoin {
                request_id: 3,
                group: group.clone(),
                offer: "v=0\r\n".into(),
            },
            NetMessage::CallJoined {
                request_id: 3,
                group: group.clone(),
                participant: 12,
                answer: "v=0\r\n".into(),
            },
            NetMessage::CallOffer {
                group: group.clone(),
                sdp: "v=0\r\n".into(),
                tracks: vec![CallTrack {
                    mid: "0".into(),
                    participant: 11,
                    video: true,
                }],
            },
            NetMessage::CallAnswer {
                group: group.clone(),
                sdp: "v=0\r\n".into(),
            },
            NetMessage::CallLeave {
                group: group.clone(),
            },
            NetMessage::CallRoster {
                group,
                members: vec![CallMember {
                    participant: 11,
                    username: "alice".into(),
                    device: Uuid::from_u128(1),
                }],
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
            name: "Updated Prim".into(),
            shape: "sphere".into(),
            position: Vec3::new(15.0, 8.0, 25.0),
            rotation: Vec3::new(45.0, 45.0, 0.0),
            scale: Vec3::new(1.5, 1.5, 1.5),
            color: [0.0, 1.0, 0.0],
            texture_id: Some("grass".into()),
            model: None,
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
            particles: Some(crate::particles::ParticleEffect::blast()),
            scripted: true,
            video: Some(PrimVideo {
                asset: "ab".repeat(32),
                started_at_ms: 1_790_000_000_000,
                looped: true,
            }),
            sound_loop: Some(crate::sounds::PrimSound {
                sound: crate::sounds::SoundName::Bell,
                volume: 0.5,
                range_m: 80.0,
            }),
            physical: true,
            hand_range: 2.0,
            tone: Some(crate::tone::ToneSpec::default()),
        };
        let m = NetMessage::PrimUpsert { prim: prim.clone() };
        let b = encode_app_frame(&m).unwrap();
        let m2 = decode_app_frame(&b).unwrap();
        assert_eq!(m, m2);
    }

    #[test]
    fn prim_motion_round_trip_and_size() {
        let body = PrimBody {
            id: 42,
            geo: GeoAnchor {
                latitude: 53.2066,
                longitude: 6.5918,
            },
            altitude: 0.11,
            rotation: [0.0, 0.38, 0.0, 0.92],
            velocity: Vec3::new(4.0, 1.5, -2.0),
            resting: false,
        };
        let m = NetMessage::PrimMotion {
            tick: 9_000,
            bodies: vec![body; 3],
        };
        let b = encode_app_frame(&m).unwrap();
        assert_eq!(decode_app_frame(&b).unwrap(), m);
        // The point of the message is that it is small: a ball in play is one of
        // these twenty times a second to every client.
        assert!(b.len() < 3 * 64, "{} bytes for three bodies", b.len());
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
        const ALL: [MessageKind; 101] = [
            ClientHello,
            ServerHelloAck,
            ServerError,
            ClientIntent,
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
            GetPrimScript,
            SetPrimScript,
            PrimScript,
            ScriptOutput,
            TouchPrim,
            WorldCapacity,
            VoiceFrame,
            VoiceData,
            SetBlocked,
            BlockedList,
            ReportUser,
            ReportAck,
            DeleteAccount,
            AccountDeleted,
            Teleport,
            SetPresence,
            E2eeRegisterDevice,
            E2eeDeviceStatus,
            E2eeClaimKeyPackages,
            E2eeKeyPackages,
            E2eeCreateGroup,
            E2eeGroupCreated,
            E2eeSend,
            E2eeSent,
            E2eeSync,
            E2eeSynced,
            E2eeDeliver,
            E2eeInvite,
            CallJoin,
            CallJoined,
            CallOffer,
            CallAnswer,
            CallLeave,
            CallRoster,
            ContactsState,
            RequestContact,
            AnswerContact,
            RemoveContact,
            SetContactSettings,
            LookupPerson,
            PersonInfo,
            GetNearbyPeople,
            NearbyPeople,
            SetPrimVideo,
            SetPushEndpoint,
            WherePeople,
            PeopleWhere,
            CallDecline,
            CallDeclined,
            PrimSound,
            SetPrimPhysical,
            PrimMotion,
            Permissions,
            HandPose,
            PrimTone,
            E2eeAddDevices,
            CommunityList,
            Communities,
            CommunityCreate,
            CommunityCreated,
            CommunityEdit,
            CommunityDirectory,
            CommunityListings,
            CommunityJoin,
            CommunityLeave,
            CommunityDecide,
            CommunityAddMember,
            CommunitySetRole,
            CommunityRemove,
            CommunityDelete,
            TopicCreate,
            TopicEdit,
            TopicAddPeople,
            TopicRemovePeople,
            TopicRemoved,
            TopicLeft,
            ModerateDelete,
            E2eeRedacted,
            CommunityReport,
            CommunityReportResolve,
            CommunityInviteCreate,
            CommunityInvite,
            CommunityInviteLookup,
            CommunityJoinInvite,
        ];
        for k in ALL {
            assert_eq!(
                MessageKind::from_wire(k as u16),
                Some(k),
                "discriminant {} lost its from_wire arm",
                k as u16
            );
        }
        // Nothing on either side of the assigned run. `ALL` is the list to extend
        // when a kind is added — that is the point of the test, and the reason it
        // is written against a hand-maintained list rather than a derive.
        assert_eq!(MessageKind::from_wire(0), None);
        assert_eq!(MessageKind::from_wire(CommunityJoinInvite as u16 + 1), None);
        // 5 is the hole `ObserverUpdate` left when regions went (protocol 23). A
        // retired number stays retired: reusing it would make a stale client's
        // observer update decode as whatever took the slot, and the run is
        // therefore no longer contiguous on purpose.
        assert_eq!(MessageKind::from_wire(5), None);
        // 19 (`ChatSubscribe`) and 75–81 (workspaces and channels) went with
        // rooms and workspaces in protocol 39, and stay retired for the same reason.
        for retired in [19, 75, 76, 77, 78, 79, 80, 81] {
            assert_eq!(MessageKind::from_wire(retired), None);
        }
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
