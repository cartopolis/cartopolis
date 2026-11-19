//! Communities on the wire: roles, settings, members, topics, listings.

use serde::{Deserialize, Serialize};
use uuid::Uuid;

use super::*;

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
