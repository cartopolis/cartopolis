//! A community's **public posts**: the half of a community that anyone may
//! read, and that the simulator publishes to the fediverse over ActivityPub
//! (`docs/notes/fediverse.md`, `docs/mockups/communities/v4-public-posts.html`).
//!
//! Topics are encrypted and members-only; posts are the opposite on purpose. The
//! server stores them readable, because it has to send them to other servers,
//! and every reader is told so before they write one.
//!
//! These are the JSON shapes of the simulator's `/api/communities/{id}/posts`
//! surface. Posts are HTTP rather than the wire for the reason the model store
//! is: they are not part of the simulation, the web page reads them with a
//! plain `fetch`, and a new field here does not move every client's protocol
//! version. The web UI's copy is `crates/atlas/web/src/types.ts`, pinned to
//! these by the simulator's `web_mirror` test.

use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::point::PointView;

/// Longest post or reply, in characters: Mastodon's own limit, so a post never
/// arrives there cut short.
pub const MAX_POST_TEXT: usize = 500;

/// Longest place name on a post, in characters (a point's own limit).
pub const MAX_PLACE_NAME: usize = crate::point::MAX_POINT_NAME;

/// Longest community address (`@handle@server`'s handle), in characters.
pub const MAX_HANDLE: usize = 30;

/// Posts one page of a community's feed answers with.
pub const POSTS_PAGE: usize = 30;

/// Where a post was written: a spot, and the camera it was looked at from.
///
/// The same place a point carries (`crate::point`), minus the text, which on a
/// post is the post's own.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PostPlace {
    pub latitude: f64,
    pub longitude: f64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub view: Option<PointView>,
}

impl PostPlace {
    /// Whether this is a place on Earth, and its view (if any) is numbers.
    #[must_use]
    pub fn in_range(&self) -> bool {
        self.latitude.is_finite()
            && self.longitude.is_finite()
            && (-90.0..=90.0).contains(&self.latitude)
            && (-180.0..=180.0).contains(&self.longitude)
            && self
                .view
                .is_none_or(|v| v.altitude.is_finite() && v.pitch.is_finite() && v.yaw.is_finite())
    }
}

/// What a member sends to write a post. A post written in Cartopolis always has
/// a place; that is what it is for.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct NewPost {
    pub text: String,
    /// A plain JPEG already in the picture store (`POST /api/pictures`). Unlike
    /// a chat's pictures it is not sealed: a post is public.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub picture: Option<String>,
    /// A snapshot of the world at the post's place, framed as a polaroid on the
    /// device (the app's "Polaroid of this view"), also in the picture store.
    /// Beside the photo, not instead of it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub snapshot: Option<String>,
    pub place: PostPlace,
}

/// A reply under a post. Replies are flat: a reply to a reply is a reply to the
/// post.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct NewReply {
    pub text: String,
}

/// Who wrote a post.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PostAuthor {
    /// What to show: a display name from elsewhere, or the username here.
    pub name: String,
    /// The full address, `name@server`, here or on another server.
    pub address: String,
    /// Whether they have an account on this server.
    pub local: bool,
}

/// One post or reply, as the feed shows it.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PostDto {
    pub id: Uuid,
    pub community: Uuid,
    /// The post a reply is under; `None` for a post.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub parent: Option<Uuid>,
    pub author: PostAuthor,
    /// The address of the fediverse source the community follows that brought
    /// this post in (`groningen@feddit.nl`), when that is how it arrived.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub via: Option<String>,
    pub text: String,
    /// A hash in the picture store; a picture from another server is copied in
    /// when the post arrives, so no reader's address is ever shown to it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub picture: Option<String>,
    /// Its snapshot of the world, for a post written in Cartopolis.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub snapshot: Option<String>,
    /// Where it was written. Most posts from elsewhere have none: they are
    /// listed, not pinned.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub place: Option<PostPlace>,
    /// The post's own web page, for one from another server.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub url: Option<String>,
    pub created_ms: i64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub edited_ms: Option<i64>,
    /// Replies under a post; 0 on a reply.
    pub replies: u32,
    /// Whether the caller wrote it (and so may delete it).
    pub mine: bool,
}

/// A post and the replies under it, oldest first.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PostThread {
    pub post: PostDto,
    pub replies: Vec<PostDto>,
}

/// Who may write a community's posts. Replies are open to everybody who can
/// read the post, here and elsewhere, as they are on any fediverse server.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum WhoPosts {
    #[default]
    Members,
    /// A notice board: only moderators write posts.
    Moderators,
}

/// What a moderator sets about the public half.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PublicSettings {
    pub enabled: bool,
    /// The community's fediverse address before `@server`. Chosen once, the
    /// first time posts are switched on, and never changed afterwards: a
    /// follower's server knows the community by it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub handle: Option<String>,
    #[serde(default)]
    pub who_posts: WhoPosts,
}

/// Whether a followed source is a person's account or a group (a Lemmy
/// community, another Cartopolis community).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SourceKind {
    Account,
    Community,
}

/// A fediverse account or community this community follows.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FollowDto {
    /// `name@server`, without the leading `@` (or `!` on Lemmy).
    pub address: String,
    pub name: String,
    pub kind: SourceKind,
    /// Whether their server has said yes. Until it does, nothing arrives.
    pub accepted: bool,
}

/// The public half of a community, as its page needs to draw it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PublicInfo {
    pub settings: PublicSettings,
    /// `handle@server`, once there is a handle.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub address: Option<String>,
    /// Followers on other servers.
    pub followers: u32,
    pub follows: Vec<FollowDto>,
    /// Whether the caller may write a post here now.
    pub may_post: bool,
    /// Whether the caller moderates the community.
    pub may_moderate: bool,
    /// Whether this server talks to other servers at all. A development server
    /// on `localhost` keeps posts to itself.
    pub federating: bool,
}

/// Whether `s` may be a community's handle: lower-case letters, digits and
/// `_`, 1–30 of them. What every fediverse server accepts as the part before
/// the `@`, with no case to fold.
#[must_use]
pub fn is_handle(s: &str) -> bool {
    let n = s.chars().count();
    (1..=MAX_HANDLE).contains(&n)
        && s.chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_')
}

/// A handle to suggest for a community called `name`: its letters and digits,
/// lower-cased, the rest folded to `_`. May need a digit added if it is taken.
#[must_use]
pub fn suggest_handle(name: &str) -> String {
    let mut out = String::new();
    for c in name.chars() {
        let c = fold_accent(c).to_ascii_lowercase();
        if c.is_ascii_lowercase() || c.is_ascii_digit() {
            out.push(c);
        } else if !out.ends_with('_') && !out.is_empty() {
            out.push('_');
        }
    }
    let mut out: String = out.trim_matches('_').chars().take(MAX_HANDLE).collect();
    while out.ends_with('_') {
        out.pop();
    }
    if out.is_empty() {
        "community".into()
    } else {
        out
    }
}

/// The few accented letters Dutch and its neighbours write, folded to ASCII so
/// "Oosterpoort Café" suggests `oosterpoort_cafe` rather than `oosterpoort_caf`.
fn fold_accent(c: char) -> char {
    match c {
        'á' | 'à' | 'â' | 'ä' | 'å' | 'Á' | 'À' | 'Â' | 'Ä' => 'a',
        'é' | 'è' | 'ê' | 'ë' | 'É' | 'È' | 'Ê' | 'Ë' => 'e',
        'í' | 'ì' | 'î' | 'ï' | 'Í' | 'Ï' => 'i',
        'ó' | 'ò' | 'ô' | 'ö' | 'Ó' | 'Ö' => 'o',
        'ú' | 'ù' | 'û' | 'ü' | 'Ú' | 'Ü' => 'u',
        'ç' | 'Ç' => 'c',
        'ñ' | 'Ñ' => 'n',
        'ĳ' | 'Ĳ' => 'y',
        other => other,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_handle_is_what_every_server_accepts() {
        assert!(is_handle("oosterpoort"));
        assert!(is_handle("buurt_2"));
        assert!(!is_handle(""));
        assert!(!is_handle("Oosterpoort"));
        assert!(!is_handle("oost-poort"));
        assert!(!is_handle(&"a".repeat(MAX_HANDLE + 1)));
    }

    #[test]
    fn a_suggested_handle_is_a_handle() {
        for (name, want) in [
            ("Buurt Oosterpoort", "buurt_oosterpoort"),
            ("Oosterpoort Café", "oosterpoort_cafe"),
            ("  --  ", "community"),
            ("#fietsers-groningen!", "fietsers_groningen"),
        ] {
            let got = suggest_handle(name);
            assert_eq!(got, want);
            assert!(is_handle(&got), "{got}");
        }
        assert!(is_handle(&suggest_handle(&"Lange naam ".repeat(10))));
    }

    #[test]
    fn a_place_off_the_earth_is_refused() {
        let ok = PostPlace {
            latitude: 53.2,
            longitude: 6.57,
            name: None,
            view: None,
        };
        assert!(ok.in_range());
        assert!(!PostPlace {
            latitude: 91.0,
            ..ok.clone()
        }
        .in_range());
        assert!(!PostPlace {
            longitude: f64::NAN,
            ..ok
        }
        .in_range());
    }

    #[test]
    fn a_post_reads_back_from_its_json() {
        let post = NewPost {
            text: "Plant swap".into(),
            picture: None,
            snapshot: None,
            place: PostPlace {
                latitude: 53.2,
                longitude: 6.57,
                name: Some("Meeuwerderweg".into()),
                view: Some(PointView {
                    altitude: 120.0,
                    pitch: -30.0,
                    yaw: 10.0,
                }),
            },
        };
        let json = serde_json::to_string(&post).unwrap();
        assert!(!json.contains("picture"), "{json}");
        assert_eq!(serde_json::from_str::<NewPost>(&json).unwrap(), post);
        assert_eq!(
            serde_json::to_string(&WhoPosts::Moderators).unwrap(),
            "\"moderators\""
        );
    }
}
