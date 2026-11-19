//! Calls on the wire: members, tracks, rooms, settings and host actions.

use serde::{Deserialize, Serialize};
use uuid::Uuid;

/// How long a guest link, and a guest's account, last: a meeting, not a
/// membership.
pub const CALL_GUEST_HOURS: u64 = 24;

/// One track on a device's receiving connection ([`NetMessage::CallOffer`]).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct CallTrack {
    /// The SDP media id it arrives under.
    pub mid: String,
    /// Whose it is, as [`CallMember::participant`] numbers them.
    pub participant: u64,
    pub kind: CallTrackKind,
}

/// What a [`CallTrack`] carries.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
pub enum CallTrackKind {
    Audio,
    Camera,
    /// A shared screen (protocol 41).
    Screen,
}

/// One device in a call ([`NetMessage::CallRoster`]).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct CallMember {
    pub participant: u64,
    pub username: String,
    pub device: Uuid,
    /// Protocol 41 from here on.
    pub role: CallRole,
    /// A raised hand's place in the queue: lower went up first.
    pub hand: Option<u32>,
    /// A host muted this device and it may not unmute: the forwarder drops
    /// its sound until a host lets it talk.
    pub held: bool,
    /// Sharing its screen.
    pub sharing: bool,
    /// Its microphone is off, as it says itself (a host's hold is `held`).
    pub muted: bool,
    /// Recording the call on its own device.
    pub recording: bool,
    /// A guest, let in by an invite link, not a member of the chat.
    pub guest: bool,
    /// The breakout room it is in; 0 is the main call.
    pub room: u32,
    /// An AI agent speaks for this account (protocol 43): what it hears goes
    /// to a model provider, which its badge says.
    #[serde(default)]
    pub agent: bool,
}

/// Who runs a call. The first person in hosts it; when the last host or
/// co-host leaves, whoever has been there longest takes over, so a call never
/// loses its host.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Default)]
pub enum CallRole {
    #[default]
    Member,
    CoHost,
    Host,
}

impl CallRole {
    /// Whether this role may take host actions.
    #[must_use]
    pub fn hosts(self) -> bool {
        matches!(self, Self::Host | Self::CoHost)
    }
}

/// A device waiting to be let into a call ([`NetMessage::CallRoster`]).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct CallWaiter {
    /// The number it will have in the call, for `Admit` and `Deny`.
    pub participant: u64,
    pub username: String,
    pub guest: bool,
    /// When it started waiting, unix ms.
    pub since_ms: u64,
}

/// A call's breakout rooms while they are open ([`NetMessage::CallRoster`]).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct CallRooms {
    /// Room `n`'s name is `names[n - 1]`.
    pub names: Vec<String>,
    /// When everybody is brought back, unix ms; `None` until a host closes them.
    pub until_ms: Option<u64>,
    /// Members may move between rooms themselves.
    pub may_move: bool,
}

/// What the hosts allow everyone else in a call ([`NetMessage::CallRoster`]).
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
pub struct CallSettings {
    /// Nobody new joins; anyone who was in the call may come back.
    pub locked: bool,
    /// Members may unmute themselves. Off holds every member's sound until a
    /// host lets them talk.
    pub may_unmute: bool,
    /// Members may share a screen. Off drops a member's screen at the forwarder.
    pub may_share: bool,
    /// Members wait to be let in, as guests always do.
    pub members_wait: bool,
    /// Anybody may record the call on their own device. Off, a device that
    /// says it records is refused, and everybody sees it asked.
    pub may_record: bool,
}

impl Default for CallSettings {
    fn default() -> Self {
        Self {
            locked: false,
            may_unmute: true,
            may_share: true,
            members_wait: false,
            may_record: false,
        }
    }
}

/// The `request_id` of the `ServerError` that refuses a [`NetMessage::CallControl`]:
/// in the call's request range (`0xCA11_xxxx`), so it reaches the call, but not
/// the join's, so a refused host action does not read as the call failing.
pub const CALL_CONTROL_REQUEST: u32 = 0xCA11_0002;

/// [`NetMessage::CallControl`]'s action. The first three are a device's own;
/// the rest need a host or co-host.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub enum CallAction {
    /// Raise or lower this device's hand.
    Hand(bool),
    /// This device started or stopped sharing its screen.
    Sharing(bool),
    /// This device turned its microphone off or on, for the others to see.
    Muted(bool),
    /// Hold a participant's sound: muted, and unable to unmute.
    Mute {
        participant: u64,
    },
    /// Let a participant talk: release a hold and lower its hand. Its
    /// microphone stays off until it turns it on.
    AllowTalk {
        participant: u64,
    },
    LowerHand {
        participant: u64,
    },
    /// Stop a participant's shared screen.
    StopSharing {
        participant: u64,
    },
    /// Hold every member's sound (hosts and co-hosts keep theirs).
    MuteAll,
    /// Take a participant out of the call; it may not come back while the
    /// call lasts.
    Remove {
        participant: u64,
    },
    /// Make a participant a co-host or a member again, or hand it the call
    /// (`Host`), which makes the host who did it a co-host.
    Role {
        participant: u64,
        role: CallRole,
    },
    Settings(CallSettings),
    /// End the call for everybody in it.
    EndForAll,
    /// Let a waiting device in, or turn it away.
    Admit {
        participant: u64,
    },
    Deny {
        participant: u64,
    },
    /// Let everybody waiting in.
    AdmitAll,
    /// This device started or stopped recording the call on itself. Refused
    /// unless the hosts allow recording; announced to everyone either way.
    Recording(bool),
    /// Show this person large for everybody, or nobody (`None`).
    Spotlight {
        participant: Option<u64>,
    },
    /// Split the call into breakout rooms named `names` (room `n` is
    /// `names[n - 1]`; room 0 is the main call), with `assign` saying who
    /// goes where. After `minutes` everybody comes back (0: until closed).
    OpenRooms {
        names: Vec<String>,
        assign: Vec<(u64, u32)>,
        minutes: u32,
        may_move: bool,
    },
    /// Move a participant to a room (0: the main call). A member may move
    /// themselves when the rooms allow it.
    MoveTo {
        participant: u64,
        room: u32,
    },
    /// Bring everybody back to the main call.
    CloseRooms,
}

/// Why [`NetMessage::CallEnded`].
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub enum CallEndReason {
    /// A host (`by`, a username) took this device out of the call.
    Removed { by: String },
    /// A host ended it for everyone.
    Ended { by: String },
}
