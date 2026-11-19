//! The header's message kind: [`MessageKind`]'s numbers, and the kind and
//! request id of a [`NetMessage`].

use super::*;

/// Writes [`MessageKind`] and everything that has to name each of its rows:
/// [`MessageKind::from_wire`], [`message_kind`] and [`message_request_id`].
///
/// One row per message, `Name = number,`, the name a [`NetMessage`] variant's.
/// A row marked `#[request_id]` puts that variant's `request_id` field in the
/// frame header; an unmarked one sends 0. The matches it writes are exhaustive,
/// so a variant without a row, a row without a variant or a marked variant
/// without the field fails to compile, and two rows on one number fail as a
/// duplicate discriminant. `every_request_id_reaches_the_header` catches the one
/// thing the compiler cannot: a variant with the field and no mark.
///
/// The enum stays written out as an enum, one row per line, because
/// `tools/check-numbers.ts` reads the numbers from this text.
macro_rules! message_kinds {
    (
        $(#[$meta:meta])*
        $vis:vis enum $kind:ident {
            $( $(#[$field:ident])? $name:ident = $number:literal, )*
        }
    ) => {
        $(#[$meta])*
        $vis enum $kind {
            $( $name = $number, )*
        }

        impl $kind {
            #[must_use]
            pub fn from_wire(v: u16) -> Option<Self> {
                match v {
                    $( $number => Some(Self::$name), )*
                    _ => None,
                }
            }

            /// Whether a message of this kind carries its `request_id` in the
            /// frame header (`#[request_id]` in the table), rather than 0.
            #[must_use]
            pub const fn has_request_id(self) -> bool {
                match self {
                    $( Self::$name => request_id_of!(has $($field)?), )*
                }
            }
        }

        #[must_use]
        pub fn message_kind(msg: &NetMessage) -> $kind {
            match msg {
                $( NetMessage::$name { .. } => $kind::$name, )*
            }
        }

        #[must_use]
        pub fn message_request_id(msg: &NetMessage) -> u32 {
            match msg {
                $( NetMessage::$name { $($field,)? .. } => request_id_of!(read $($field)?), )*
            }
        }
    };
}

/// One row's request id, for `message_kinds!`: `read` it from the field the
/// row's mark names (bound by the arm's pattern), or say whether there is one.
macro_rules! request_id_of {
    (read) => {
        0
    };
    (read $field:ident) => {
        *$field
    };
    (has) => {
        false
    };
    (has $field:ident) => {
        true
    };
}

message_kinds! {
#[repr(u16)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MessageKind {
    ClientHello = 1,
    ServerHelloAck = 2,
    #[request_id]
    ServerError = 3,
    #[request_id]
    ClientIntent = 4,
    // 5 was `ObserverUpdate`, retired with regions in protocol 23: it existed only
    // to aim the per-region area-of-interest filter, and there is no longer a
    // region to include or exclude. The number is left unassigned rather than
    // reused — a stale client sending 5 should be rejected, not misread as
    // something else.
    WorldSnapshot = 6,
    PrimRemoved = 7,
    #[request_id]
    CreatePrim = 8,
    #[request_id]
    UpdatePrim = 9,
    #[request_id]
    DeletePrim = 10,
    PrimUpsert = 11,
    TextureCatalogRequest = 12,
    TextureCatalog = 13,
    #[request_id]
    TextureRequest = 14,
    #[request_id]
    TextureData = 15,
    #[request_id]
    TextureNotFound = 16,
    #[request_id]
    ChatSend = 17,
    ChatMessage = 18,
    #[request_id]
    GetPrimScript = 20,
    #[request_id]
    SetPrimScript = 21,
    PrimScript = 22,
    ScriptOutput = 23,
    #[request_id]
    TouchPrim = 24,
    WorldCapacity = 25,
    VoiceFrame = 26,
    VoiceData = 27,
    #[request_id]
    SetBlocked = 28,
    BlockedList = 29,
    #[request_id]
    ReportUser = 30,
    #[request_id]
    ReportAck = 31,
    #[request_id]
    DeleteAccount = 32,
    AccountDeleted = 33,
    #[request_id]
    Teleport = 34,
    #[request_id]
    SetPresence = 35,
    #[request_id]
    E2eeRegisterDevice = 36,
    #[request_id]
    E2eeDeviceStatus = 37,
    #[request_id]
    E2eeClaimKeyPackages = 38,
    #[request_id]
    E2eeKeyPackages = 39,
    #[request_id]
    E2eeCreateGroup = 40,
    #[request_id]
    E2eeGroupCreated = 41,
    #[request_id]
    E2eeSend = 42,
    #[request_id]
    E2eeSent = 43,
    #[request_id]
    E2eeSync = 44,
    #[request_id]
    E2eeSynced = 45,
    E2eeDeliver = 46,
    E2eeInvite = 47,
    #[request_id]
    CallJoin = 48,
    #[request_id]
    CallJoined = 49,
    CallOffer = 50,
    CallAnswer = 51,
    CallLeave = 52,
    CallRoster = 53,
    ContactsState = 54,
    #[request_id]
    RequestContact = 55,
    #[request_id]
    AnswerContact = 56,
    #[request_id]
    RemoveContact = 57,
    #[request_id]
    SetContactSettings = 58,
    #[request_id]
    LookupPerson = 59,
    #[request_id]
    PersonInfo = 60,
    #[request_id]
    GetNearbyPeople = 61,
    #[request_id]
    NearbyPeople = 62,
    #[request_id]
    SetPrimVideo = 63,
    SetPushEndpoint = 64,
    #[request_id]
    WherePeople = 65,
    #[request_id]
    PeopleWhere = 66,
    CallDecline = 67,
    CallDeclined = 68,
    PrimSound = 69,
    #[request_id]
    SetPrimPhysical = 70,
    PrimMotion = 71,
    Permissions = 72,
    HandPose = 73,
    PrimTone = 74,
    E2eeAddDevices = 82,
    #[request_id]
    CommunityList = 83,
    #[request_id]
    Communities = 84,
    #[request_id]
    CommunityCreate = 85,
    #[request_id]
    CommunityCreated = 86,
    #[request_id]
    CommunityEdit = 87,
    #[request_id]
    CommunityDirectory = 88,
    #[request_id]
    CommunityListings = 89,
    #[request_id]
    CommunityJoin = 90,
    #[request_id]
    CommunityLeave = 91,
    #[request_id]
    CommunityDecide = 92,
    #[request_id]
    CommunityAddMember = 93,
    #[request_id]
    CommunitySetRole = 94,
    #[request_id]
    CommunityRemove = 95,
    #[request_id]
    CommunityDelete = 96,
    #[request_id]
    TopicCreate = 97,
    #[request_id]
    TopicEdit = 98,
    TopicAddPeople = 99,
    TopicRemovePeople = 100,
    TopicRemoved = 101,
    TopicLeft = 102,
    #[request_id]
    ModerateDelete = 103,
    E2eeRedacted = 104,
    #[request_id]
    CommunityReport = 105,
    #[request_id]
    CommunityReportResolve = 106,
    #[request_id]
    CommunityInviteCreate = 107,
    #[request_id]
    CommunityInvite = 108,
    #[request_id]
    CommunityInviteLookup = 109,
    #[request_id]
    CommunityJoinInvite = 110,
    CallControl = 111,
    CallEnded = 112,
    CallReact = 113,
    CallReacted = 114,
    CallWatch = 115,
    #[request_id]
    CallWaiting = 116,
    CallInvite = 117,
    CallInvited = 118,
    CallKeyDrop = 119,
    CallKeyDropped = 120,
    SetAvatarLook = 121,
    AvatarLookOf = 122,
    SetFaceFrame = 123,
    FaceFrameOf = 124,
    PrimOutOfReach = 125,
    #[request_id]
    GrabPrim = 126,
    #[request_id]
    ReleasePrim = 127,
    #[request_id]
    OfferGreeting = 128,
    GreetingOffered = 129,
    #[request_id]
    AcceptGreeting = 130,
    GreetingStarted = 131,
    FollowPlans = 132,
}
}
