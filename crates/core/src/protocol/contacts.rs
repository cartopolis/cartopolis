//! Contacts and people on the wire: request policy, contacts, people nearby.

use serde::{Deserialize, Serialize};

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
    /// An AI agent speaks for this account (protocol 43).
    #[serde(default)]
    pub agent: bool,
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
    /// An AI agent speaks for the other account (protocol 43).
    #[serde(default)]
    pub agent: bool,
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
