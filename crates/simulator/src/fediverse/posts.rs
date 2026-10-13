//! A community's public posts: the rules, over a `&Connection`
//! (`docs/notes/fediverse.md`).
//!
//! Plain functions, like `communities`, so the tests below run every rule
//! against a real database with no network. What goes out to other servers
//! because of a change here is decided by the caller (`fediverse::http`), which
//! is the one place that knows this server's public address.
//!
//! The rules:
//!
//! 1. **A community's moderators switch posts on**, and choose its address
//!    (`handle`) the first time. The handle never changes afterwards and is
//!    never handed out again, even after the community is gone: other servers
//!    know it by it. It must not be anybody's username either, because both
//!    are found the same way (`@name@server`).
//! 2. **Members write posts** (or only moderators, for a notice board), and a
//!    post written here always has a place.
//! 3. **Anybody signed in may reply**, here or on another server: a public post
//!    is public.
//! 4. **The author deletes; a moderator hides.** Deleting is the author taking
//!    it back everywhere; hiding is the community saying "not here".

use cartopolis_core::point::PointView;
use cartopolis_core::posts::{
    is_handle, FollowDto, NewPost, PostAuthor, PostDto, PostPlace, PostThread, PublicInfo,
    PublicSettings, SourceKind, WhoPosts, MAX_PLACE_NAME, MAX_POST_TEXT, POSTS_PAGE,
};
use cartopolis_core::CommunityRole;
use rusqlite::{params, Connection, OptionalExtension};
use uuid::Uuid;

use crate::communities;

/// Why a request about posts was refused, in words the client can show.
#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum Refusal {
    #[error("no such community")]
    NoSuchCommunity,
    #[error("this community has no public posts")]
    PostsOff,
    #[error("only a moderator may do that")]
    NotAModerator,
    #[error("only members of this community may post here")]
    NotAMember,
    #[error("only moderators post here")]
    ModeratorsOnly,
    #[error("an address is lower-case letters, digits and _, at most 30")]
    BadHandle,
    #[error("that address is taken")]
    HandleTaken,
    #[error("a community's address can't be changed once it has one")]
    HandleFixed,
    #[error("choose an address for the community first")]
    NeedsHandle,
    #[error("a post needs some text")]
    Empty,
    #[error("that is too long")]
    TooLong,
    #[error("that is not a place on Earth")]
    BadPlace,
    #[error("a picture is a hash in the picture store")]
    BadPicture,
    #[error("no such post")]
    NoSuchPost,
    #[error("only its author may delete a post")]
    NotYours,
    #[error("the server could not store that: {0}")]
    Storage(String),
}

impl From<rusqlite::Error> for Refusal {
    fn from(e: rusqlite::Error) -> Self {
        Refusal::Storage(e.to_string())
    }
}

impl From<communities::Refusal> for Refusal {
    fn from(e: communities::Refusal) -> Self {
        match e {
            communities::Refusal::NoSuchCommunity => Refusal::NoSuchCommunity,
            communities::Refusal::NotAModerator => Refusal::NotAModerator,
            other => Refusal::Storage(other.to_string()),
        }
    }
}

/// The public half of a community, as stored.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Public {
    pub handle: String,
    pub enabled: bool,
    pub who_posts: WhoPosts,
}

fn who_of(n: i64) -> WhoPosts {
    if n == 1 {
        WhoPosts::Moderators
    } else {
        WhoPosts::Members
    }
}

fn who_number(w: WhoPosts) -> i64 {
    match w {
        WhoPosts::Members => 0,
        WhoPosts::Moderators => 1,
    }
}

/// A community's public half, if posts were ever switched on.
pub fn public(conn: &Connection, community: Uuid) -> Result<Option<Public>, Refusal> {
    Ok(conn
        .query_row(
            "SELECT handle, enabled, who_posts FROM community_public WHERE community = ?1",
            params![community.to_string()],
            |r| {
                Ok(Public {
                    handle: r.get(0)?,
                    enabled: r.get::<_, i64>(1)? != 0,
                    who_posts: who_of(r.get(2)?),
                })
            },
        )
        .optional()?)
}

/// The community a handle names, if posts are on there.
pub fn community_by_handle(conn: &Connection, handle: &str) -> Result<Option<Uuid>, Refusal> {
    let id: Option<String> = conn
        .query_row(
            "SELECT community FROM community_public WHERE handle = ?1 AND enabled = 1",
            params![handle],
            |r| r.get(0),
        )
        .optional()?;
    Ok(id.and_then(|s| Uuid::parse_str(&s).ok()))
}

fn community_exists(conn: &Connection, community: Uuid) -> Result<bool, Refusal> {
    Ok(conn
        .query_row(
            "SELECT 1 FROM communities WHERE id = ?1",
            params![community.to_string()],
            |_| Ok(()),
        )
        .optional()?
        .is_some())
}

/// Whether a handle is free: not another community's, not a retired one, and
/// not anybody's username (folded, since a username may have capitals and
/// `@name@server` is looked up without them).
pub fn handle_free(conn: &Connection, handle: &str) -> Result<bool, Refusal> {
    let taken: bool = conn.query_row(
        "SELECT EXISTS (SELECT 1 FROM community_public WHERE handle = ?1)
             OR EXISTS (SELECT 1 FROM retired_handles WHERE handle = ?1)
             OR EXISTS (SELECT 1 FROM accounts WHERE lower(username) = ?1)",
        params![handle],
        |r| r.get(0),
    )?;
    Ok(!taken)
}

/// A moderator switches posts on or off, chooses who may write them, and — the
/// first time — the community's address.
pub fn set_public(
    conn: &Connection,
    by: &str,
    community: Uuid,
    settings: &PublicSettings,
    now_ms: i64,
) -> Result<Public, Refusal> {
    match communities::role(conn, community, by)? {
        Some(r) if r.moderates() => {}
        Some(_) => return Err(Refusal::NotAModerator),
        None if community_exists(conn, community)? => return Err(Refusal::NotAModerator),
        None => return Err(Refusal::NoSuchCommunity),
    }
    let existing = public(conn, community)?;
    let handle = match (&existing, settings.handle.as_deref().map(str::trim)) {
        (Some(p), None) => p.handle.clone(),
        (Some(p), Some(h)) if h == p.handle => p.handle.clone(),
        (Some(_), Some(_)) => return Err(Refusal::HandleFixed),
        (None, None) => return Err(Refusal::NeedsHandle),
        (None, Some(h)) => {
            if !is_handle(h) {
                return Err(Refusal::BadHandle);
            }
            if !handle_free(conn, h)? {
                return Err(Refusal::HandleTaken);
            }
            h.to_string()
        }
    };
    conn.execute(
        "INSERT INTO community_public (community, handle, enabled, who_posts, created_ms)
         VALUES (?1, ?2, ?3, ?4, ?5)
         ON CONFLICT (community) DO UPDATE SET enabled = ?3, who_posts = ?4",
        params![
            community.to_string(),
            handle,
            settings.enabled,
            who_number(settings.who_posts),
            now_ms
        ],
    )?;
    Ok(Public {
        handle,
        enabled: settings.enabled,
        who_posts: settings.who_posts,
    })
}

/// The public half of a community as its page draws it, for `caller` (`None`
/// when nobody is signed in).
pub fn info(
    conn: &Connection,
    caller: Option<&str>,
    community: Uuid,
    server: &str,
    federating: bool,
) -> Result<PublicInfo, Refusal> {
    if !community_exists(conn, community)? {
        return Err(Refusal::NoSuchCommunity);
    }
    let role = match caller {
        Some(a) => communities::role(conn, community, a)?,
        None => None,
    };
    let p = public(conn, community)?;
    let followers: i64 = conn.query_row(
        "SELECT COUNT(*) FROM fedi_followers WHERE local = ?1",
        params![group_local(community)],
        |r| r.get(0),
    )?;
    let mut stmt = conn.prepare(
        "SELECT a.username, a.host, a.name, a.kind, f.accepted
         FROM community_follows f JOIN fedi_actors a ON a.id = f.actor
         WHERE f.community = ?1 ORDER BY f.created_ms",
    )?;
    let follows = stmt
        .query_map(params![community.to_string()], |r| {
            let kind: String = r.get(3)?;
            Ok(FollowDto {
                address: format!("{}@{}", r.get::<_, String>(0)?, r.get::<_, String>(1)?),
                name: r.get(2)?,
                kind: if kind == "Group" {
                    SourceKind::Community
                } else {
                    SourceKind::Account
                },
                accepted: r.get::<_, i64>(4)? != 0,
            })
        })?
        .collect::<Result<Vec<_>, _>>()?;
    let may_post = match (&p, role) {
        (Some(p), Some(r)) if p.enabled => match p.who_posts {
            WhoPosts::Members => true,
            WhoPosts::Moderators => r.moderates(),
        },
        _ => false,
    };
    Ok(PublicInfo {
        address: p.as_ref().map(|p| format!("{}@{server}", p.handle)),
        settings: PublicSettings {
            enabled: p.as_ref().is_some_and(|p| p.enabled),
            handle: p.as_ref().map(|p| p.handle.clone()),
            who_posts: p.as_ref().map(|p| p.who_posts).unwrap_or_default(),
        },
        followers: u32::try_from(followers).unwrap_or(u32::MAX),
        follows,
        may_post,
        may_moderate: role.is_some_and(CommunityRole::moderates),
        federating,
    })
}

/// The follower-table key for a community.
#[must_use]
pub fn group_local(community: Uuid) -> String {
    format!("c:{community}")
}

/// The follower-table key for an account.
#[must_use]
pub fn person_local(account: &str) -> String {
    format!("u:{account}")
}

/// Trimmed and bounded, or refused.
fn clean_text(text: &str) -> Result<String, Refusal> {
    let t = text.trim();
    if t.is_empty() {
        return Err(Refusal::Empty);
    }
    if t.chars().count() > MAX_POST_TEXT {
        return Err(Refusal::TooLong);
    }
    Ok(t.to_string())
}

fn clean_place(place: &PostPlace) -> Result<PostPlace, Refusal> {
    if !place.in_range() {
        return Err(Refusal::BadPlace);
    }
    let name = place
        .name
        .as_deref()
        .map(str::trim)
        .filter(|n| !n.is_empty())
        .map(|n| n.chars().take(MAX_PLACE_NAME).collect());
    Ok(PostPlace {
        name,
        ..place.clone()
    })
}

/// Whether `account` may write a post in `community` now.
fn may_post(conn: &Connection, community: Uuid, account: &str) -> Result<Public, Refusal> {
    if !community_exists(conn, community)? {
        return Err(Refusal::NoSuchCommunity);
    }
    let p = public(conn, community)?
        .filter(|p| p.enabled)
        .ok_or(Refusal::PostsOff)?;
    match (communities::role(conn, community, account)?, p.who_posts) {
        (None, _) => Err(Refusal::NotAMember),
        (Some(r), WhoPosts::Moderators) if !r.moderates() => Err(Refusal::ModeratorsOnly),
        _ => Ok(p),
    }
}

/// A post written here: its id is also the tail of its fediverse id
/// (`{base}/ap/p/{id}`).
pub fn create_post(
    conn: &Connection,
    author: &str,
    community: Uuid,
    new: &NewPost,
    base: &str,
    now_ms: i64,
) -> Result<Uuid, Refusal> {
    may_post(conn, community, author)?;
    let text = clean_text(&new.text)?;
    let place = clean_place(&new.place)?;
    if new
        .picture
        .as_deref()
        .is_some_and(|p| !crate::assets::is_hash(p))
    {
        return Err(Refusal::BadPicture);
    }
    let id = Uuid::new_v4();
    let view = place.view;
    conn.execute(
        "INSERT INTO posts (id, community, parent, ap_id, author_account, text, picture,
                            lat, lng, place_name, view_alt, view_pitch, view_yaw, created_ms)
         VALUES (?1, ?2, NULL, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13)",
        params![
            id.to_string(),
            community.to_string(),
            super::post_url(base, id),
            author,
            text,
            new.picture,
            place.latitude,
            place.longitude,
            place.name,
            view.map(|v| v.altitude),
            view.map(|v| f64::from(v.pitch)),
            view.map(|v| f64::from(v.yaw)),
            now_ms
        ],
    )?;
    Ok(id)
}

/// The post a reply should hang under: `id` itself if it is a post, its parent
/// if it is a reply. Hidden posts take no replies.
fn thread_root(conn: &Connection, id: Uuid) -> Result<(Uuid, Uuid), Refusal> {
    let row: Option<(String, Option<String>)> = conn
        .query_row(
            "SELECT community, parent FROM posts WHERE id = ?1 AND hidden_by IS NULL",
            params![id.to_string()],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .optional()?;
    let (community, parent) = row.ok_or(Refusal::NoSuchPost)?;
    let community = Uuid::parse_str(&community).map_err(|_| Refusal::NoSuchPost)?;
    let root = match parent {
        Some(p) => Uuid::parse_str(&p).map_err(|_| Refusal::NoSuchPost)?,
        None => id,
    };
    Ok((community, root))
}

/// A reply written here, by anybody signed in.
pub fn create_reply(
    conn: &Connection,
    author: &str,
    to: Uuid,
    text: &str,
    base: &str,
    now_ms: i64,
) -> Result<(Uuid, Uuid), Refusal> {
    let text = clean_text(text)?;
    let (community, root) = thread_root(conn, to)?;
    if !public(conn, community)?.is_some_and(|p| p.enabled) {
        return Err(Refusal::PostsOff);
    }
    let id = Uuid::new_v4();
    conn.execute(
        "INSERT INTO posts (id, community, parent, ap_id, author_account, text, created_ms)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
        params![
            id.to_string(),
            community.to_string(),
            root.to_string(),
            super::post_url(base, id),
            author,
            text,
            now_ms
        ],
    )?;
    Ok((id, community))
}

/// Everything about one post or reply.
#[derive(Debug, Clone, PartialEq)]
pub struct PostRow {
    pub id: Uuid,
    pub community: Uuid,
    pub parent: Option<Uuid>,
    pub ap_id: String,
    pub author_account: Option<String>,
    pub author_actor: Option<String>,
    pub via_actor: Option<String>,
    pub text: String,
    pub picture: Option<String>,
    pub place: Option<PostPlace>,
    pub url: Option<String>,
    pub created_ms: i64,
    pub edited_ms: Option<i64>,
    pub hidden: bool,
}

const POST_COLUMNS: &str = "id, community, parent, ap_id, author_account, author_actor, via_actor,
     text, picture, lat, lng, place_name, view_alt, view_pitch, view_yaw, url, created_ms,
     edited_ms, hidden_by";

fn read_row(r: &rusqlite::Row<'_>) -> rusqlite::Result<PostRow> {
    let uuid = |s: String| Uuid::parse_str(&s).unwrap_or_default();
    let lat: Option<f64> = r.get(9)?;
    let lng: Option<f64> = r.get(10)?;
    let alt: Option<f64> = r.get(12)?;
    let pitch: Option<f64> = r.get(13)?;
    let yaw: Option<f64> = r.get(14)?;
    #[allow(clippy::cast_possible_truncation)]
    let view = match (alt, pitch, yaw) {
        (Some(altitude), Some(p), Some(y)) => Some(PointView {
            altitude,
            pitch: p as f32,
            yaw: y as f32,
        }),
        _ => None,
    };
    Ok(PostRow {
        id: uuid(r.get(0)?),
        community: uuid(r.get(1)?),
        parent: r.get::<_, Option<String>>(2)?.map(uuid),
        ap_id: r.get(3)?,
        author_account: r.get(4)?,
        author_actor: r.get(5)?,
        via_actor: r.get(6)?,
        text: r.get(7)?,
        picture: r.get(8)?,
        place: lat.zip(lng).map(|(latitude, longitude)| PostPlace {
            latitude,
            longitude,
            name: r.get(11).ok().flatten(),
            view,
        }),
        url: r.get(15)?,
        created_ms: r.get(16)?,
        edited_ms: r.get(17)?,
        hidden: r.get::<_, Option<String>>(18)?.is_some(),
    })
}

/// One post or reply by id, hidden or not.
pub fn row(conn: &Connection, id: Uuid) -> Result<Option<PostRow>, Refusal> {
    Ok(conn
        .query_row(
            &format!("SELECT {POST_COLUMNS} FROM posts WHERE id = ?1"),
            params![id.to_string()],
            read_row,
        )
        .optional()?)
}

/// Every copy of a fediverse object here (one per community it is in).
pub fn rows_by_ap_id(conn: &Connection, ap_id: &str) -> Result<Vec<PostRow>, Refusal> {
    let mut stmt = conn.prepare(&format!(
        "SELECT {POST_COLUMNS} FROM posts WHERE ap_id = ?1"
    ))?;
    let rows = stmt.query_map(params![ap_id], read_row)?;
    Ok(rows.collect::<Result<_, _>>()?)
}

/// The replies under a post, oldest first, hidden ones left out.
pub fn replies_of(conn: &Connection, root: Uuid) -> Result<Vec<PostRow>, Refusal> {
    let mut stmt = conn.prepare(&format!(
        "SELECT {POST_COLUMNS} FROM posts WHERE parent = ?1 AND hidden_by IS NULL
         ORDER BY created_ms, id"
    ))?;
    let rows = stmt.query_map(params![root.to_string()], read_row)?;
    Ok(rows.collect::<Result<_, _>>()?)
}

/// Who wrote a row, as a reader is shown them.
fn author_of(conn: &Connection, row: &PostRow, server: &str) -> Result<PostAuthor, Refusal> {
    if let Some(account) = &row.author_account {
        let name: String = conn
            .query_row(
                "SELECT username FROM accounts WHERE id = ?1",
                params![account],
                |r| r.get(0),
            )
            .optional()?
            .unwrap_or_else(|| crate::db::DELETED_ACCOUNT_NAME.to_string());
        return Ok(PostAuthor {
            address: format!("{name}@{server}"),
            name,
            local: true,
        });
    }
    let actor = row.author_actor.as_deref().unwrap_or_default();
    Ok(actor_label(conn, actor)?.unwrap_or_else(|| PostAuthor {
        name: actor.to_string(),
        address: actor.to_string(),
        local: false,
    }))
}

/// How a remote account is shown, if we have it.
fn actor_label(conn: &Connection, actor: &str) -> Result<Option<PostAuthor>, Refusal> {
    Ok(conn
        .query_row(
            "SELECT name, username, host FROM fedi_actors WHERE id = ?1",
            params![actor],
            |r| {
                let name: String = r.get(0)?;
                let address = format!("{}@{}", r.get::<_, String>(1)?, r.get::<_, String>(2)?);
                Ok(PostAuthor {
                    name: if name.trim().is_empty() {
                        address.clone()
                    } else {
                        name
                    },
                    address,
                    local: false,
                })
            },
        )
        .optional()?)
}

/// A row as the feed shows it to `caller`.
pub fn dto(
    conn: &Connection,
    row: &PostRow,
    caller: Option<&str>,
    server: &str,
) -> Result<PostDto, Refusal> {
    let replies: i64 = if row.parent.is_none() {
        conn.query_row(
            "SELECT COUNT(*) FROM posts WHERE parent = ?1 AND hidden_by IS NULL",
            params![row.id.to_string()],
            |r| r.get(0),
        )?
    } else {
        0
    };
    let via = match &row.via_actor {
        Some(a) => actor_label(conn, a)?.map(|l| l.address),
        None => None,
    };
    Ok(PostDto {
        id: row.id,
        community: row.community,
        parent: row.parent,
        author: author_of(conn, row, server)?,
        via,
        text: row.text.clone(),
        picture: row.picture.clone(),
        place: row.place.clone(),
        url: row.url.clone(),
        created_ms: row.created_ms,
        edited_ms: row.edited_ms,
        replies: u32::try_from(replies).unwrap_or(0),
        mine: caller.is_some() && row.author_account.as_deref() == caller,
    })
}

/// A page of a community's posts, newest first, older than `before_ms` when it
/// is given. Readable by anybody while posts are on: they are public.
pub fn feed(
    conn: &Connection,
    community: Uuid,
    caller: Option<&str>,
    before_ms: Option<i64>,
    server: &str,
) -> Result<Vec<PostDto>, Refusal> {
    if !community_exists(conn, community)? {
        return Err(Refusal::NoSuchCommunity);
    }
    if !public(conn, community)?.is_some_and(|p| p.enabled) {
        return Ok(Vec::new());
    }
    let mut stmt = conn.prepare(&format!(
        "SELECT {POST_COLUMNS} FROM posts
         WHERE community = ?1 AND parent IS NULL AND hidden_by IS NULL AND created_ms < ?2
         ORDER BY created_ms DESC, id LIMIT ?3"
    ))?;
    let rows = stmt
        .query_map(
            params![
                community.to_string(),
                before_ms.unwrap_or(i64::MAX),
                i64::try_from(POSTS_PAGE).unwrap_or(30)
            ],
            read_row,
        )?
        .collect::<Result<Vec<_>, _>>()?;
    rows.iter().map(|r| dto(conn, r, caller, server)).collect()
}

/// A post and its replies.
pub fn thread(
    conn: &Connection,
    id: Uuid,
    caller: Option<&str>,
    server: &str,
) -> Result<PostThread, Refusal> {
    let post = row(conn, id)?
        .filter(|r| !r.hidden)
        .ok_or(Refusal::NoSuchPost)?;
    let root = post.parent.unwrap_or(post.id);
    let post = if root == post.id {
        post
    } else {
        row(conn, root)?
            .filter(|r| !r.hidden)
            .ok_or(Refusal::NoSuchPost)?
    };
    if !public(conn, post.community)?.is_some_and(|p| p.enabled) {
        return Err(Refusal::NoSuchPost);
    }
    let replies = replies_of(conn, root)?
        .iter()
        .map(|r| dto(conn, r, caller, server))
        .collect::<Result<_, _>>()?;
    Ok(PostThread {
        post: dto(conn, &post, caller, server)?,
        replies,
    })
}

/// The author takes a post back. A post goes with its replies (they answer
/// something that is no longer there). Answers what was deleted, so the caller
/// can tell other servers.
pub fn delete(conn: &Connection, by: &str, id: Uuid) -> Result<Vec<PostRow>, Refusal> {
    let post = row(conn, id)?.ok_or(Refusal::NoSuchPost)?;
    if post.author_account.as_deref() != Some(by) {
        return Err(Refusal::NotYours);
    }
    remove_with_replies(conn, &post)
}

fn remove_with_replies(conn: &Connection, post: &PostRow) -> Result<Vec<PostRow>, Refusal> {
    let mut gone = vec![post.clone()];
    if post.parent.is_none() {
        let mut stmt = conn.prepare(&format!(
            "SELECT {POST_COLUMNS} FROM posts WHERE parent = ?1"
        ))?;
        gone.extend(
            stmt.query_map(params![post.id.to_string()], read_row)?
                .collect::<Result<Vec<_>, _>>()?,
        );
    }
    let tx = conn.unchecked_transaction()?;
    for p in &gone {
        tx.execute("DELETE FROM posts WHERE id = ?1", params![p.id.to_string()])?;
    }
    tx.commit()?;
    Ok(gone)
}

/// A moderator hides a post or reply from the community. It stays in the
/// database, so the decision is on record, and stops being shown or served.
pub fn hide(conn: &Connection, by: &str, id: Uuid) -> Result<PostRow, Refusal> {
    let post = row(conn, id)?.ok_or(Refusal::NoSuchPost)?;
    match communities::role(conn, post.community, by)? {
        Some(r) if r.moderates() => {}
        _ => return Err(Refusal::NotAModerator),
    }
    conn.execute(
        "UPDATE posts SET hidden_by = ?2 WHERE id = ?1",
        params![id.to_string(), by],
    )?;
    Ok(post)
}

/// A post or reply that arrived from another server.
#[derive(Debug, Clone, PartialEq)]
pub struct Remote {
    pub ap_id: String,
    pub author_actor: String,
    pub via_actor: Option<String>,
    pub text: String,
    pub picture: Option<String>,
    pub place: Option<PostPlace>,
    pub url: Option<String>,
    pub created_ms: i64,
    /// The post here it answers, for a reply.
    pub parent: Option<Uuid>,
}

/// Store a post from elsewhere in a community. Answers its id here, or `None`
/// when it was already here (a server may deliver the same thing twice).
pub fn store_remote(
    conn: &Connection,
    community: Uuid,
    post: &Remote,
) -> Result<Option<Uuid>, Refusal> {
    if !public(conn, community)?.is_some_and(|p| p.enabled) {
        return Err(Refusal::PostsOff);
    }
    let text: String = post.text.trim().chars().take(MAX_POST_TEXT * 4).collect();
    let place = post.place.as_ref().filter(|p| p.in_range());
    let view = place.and_then(|p| p.view);
    let id = Uuid::new_v4();
    let n = conn.execute(
        "INSERT OR IGNORE INTO posts (id, community, parent, ap_id, author_actor, via_actor, text,
                                      picture, lat, lng, place_name, view_alt, view_pitch,
                                      view_yaw, url, created_ms)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16)",
        params![
            id.to_string(),
            community.to_string(),
            post.parent.map(|p| p.to_string()),
            post.ap_id,
            post.author_actor,
            post.via_actor,
            text,
            post.picture,
            place.map(|p| p.latitude),
            place.map(|p| p.longitude),
            place.and_then(|p| p.name.clone()),
            view.map(|v| v.altitude),
            view.map(|v| f64::from(v.pitch)),
            view.map(|v| f64::from(v.yaw)),
            post.url,
            post.created_ms
        ],
    )?;
    Ok((n == 1).then_some(id))
}

/// Another server says a post changed. Only its own author's server may say
/// so, which the caller has checked.
pub fn update_remote(
    conn: &Connection,
    ap_id: &str,
    author_actor: &str,
    text: &str,
    now_ms: i64,
) -> Result<usize, Refusal> {
    let text: String = text.trim().chars().take(MAX_POST_TEXT * 4).collect();
    Ok(conn.execute(
        "UPDATE posts SET text = ?3, edited_ms = ?4 WHERE ap_id = ?1 AND author_actor = ?2",
        params![ap_id, author_actor, text, now_ms],
    )?)
}

/// Another server says a post is gone. Its replies here go with it.
pub fn delete_remote(conn: &Connection, ap_id: &str, author_actor: &str) -> Result<usize, Refusal> {
    let mut n = 0;
    for post in rows_by_ap_id(conn, ap_id)? {
        if post.author_actor.as_deref() == Some(author_actor) {
            n += remove_with_replies(conn, &post)?.len();
        }
    }
    Ok(n)
}

/// Everything a remote account wrote here goes (it was deleted at home).
pub fn forget_actor(conn: &Connection, actor: &str) -> Result<(), Refusal> {
    let mut stmt = conn.prepare(&format!(
        "SELECT {POST_COLUMNS} FROM posts WHERE author_actor = ?1"
    ))?;
    let rows = stmt
        .query_map(params![actor], read_row)?
        .collect::<Result<Vec<_>, _>>()?;
    for r in rows {
        remove_with_replies(conn, &r)?;
    }
    conn.execute(
        "DELETE FROM fedi_followers WHERE actor = ?1",
        params![actor],
    )?;
    Ok(())
}

/// A moderator blocks an account elsewhere from the community, and hides what
/// it wrote here.
pub fn block_actor(
    conn: &Connection,
    by: &str,
    community: Uuid,
    actor: &str,
    now_ms: i64,
) -> Result<(), Refusal> {
    match communities::role(conn, community, by)? {
        Some(r) if r.moderates() => {}
        _ => return Err(Refusal::NotAModerator),
    }
    conn.execute(
        "INSERT OR REPLACE INTO community_actor_blocks (community, actor, blocked_by, added_ms)
         VALUES (?1, ?2, ?3, ?4)",
        params![community.to_string(), actor, by, now_ms],
    )?;
    conn.execute(
        "UPDATE posts SET hidden_by = ?3
         WHERE community = ?1 AND author_actor = ?2 AND hidden_by IS NULL",
        params![community.to_string(), actor, by],
    )?;
    conn.execute(
        "DELETE FROM fedi_followers WHERE local = ?1 AND actor = ?2",
        params![group_local(community), actor],
    )?;
    Ok(())
}

/// Whether a community's moderators blocked `actor`.
pub fn actor_blocked(conn: &Connection, community: Uuid, actor: &str) -> Result<bool, Refusal> {
    Ok(conn
        .query_row(
            "SELECT 1 FROM community_actor_blocks WHERE community = ?1 AND actor = ?2",
            params![community.to_string(), actor],
            |_| Ok(()),
        )
        .optional()?
        .is_some())
}

/// The picture hashes public posts name: the only pictures in the store that
/// may be served without a session.
pub fn picture_is_public(conn: &Connection, hash: &str) -> Result<bool, Refusal> {
    Ok(conn
        .query_row(
            "SELECT 1 FROM posts p JOIN community_public c ON c.community = p.community
             WHERE p.picture = ?1 AND p.hidden_by IS NULL AND c.enabled = 1 LIMIT 1",
            params![hash],
            |_| Ok(()),
        )
        .optional()?
        .is_some())
}

/// A community is gone: its posts, followers and follows go, and its handle is
/// retired rather than freed.
pub fn forget_community(conn: &Connection, community: Uuid, now_ms: i64) -> Result<(), Refusal> {
    let id = community.to_string();
    if let Some(p) = public(conn, community)? {
        conn.execute(
            "INSERT OR IGNORE INTO retired_handles (handle, retired_ms) VALUES (?1, ?2)",
            params![p.handle, now_ms],
        )?;
    }
    for table in [
        "posts",
        "community_public",
        "community_follows",
        "community_actor_blocks",
    ] {
        conn.execute(
            &format!("DELETE FROM {table} WHERE community = ?1"),
            params![id],
        )?;
    }
    conn.execute(
        "DELETE FROM fedi_followers WHERE local = ?1",
        params![group_local(community)],
    )?;
    conn.execute(
        "DELETE FROM fedi_keys WHERE actor = ?1",
        params![group_local(community)],
    )?;
    Ok(())
}

/// An account is deleted: what it wrote goes (with the replies under its
/// posts), and so do its followers and its key. Answers the rows, so the caller
/// can tell other servers.
pub fn forget_account(conn: &Connection, account: &str) -> Result<Vec<PostRow>, Refusal> {
    let mut stmt = conn.prepare(&format!(
        "SELECT {POST_COLUMNS} FROM posts WHERE author_account = ?1"
    ))?;
    let rows = stmt
        .query_map(params![account], read_row)?
        .collect::<Result<Vec<_>, _>>()?;
    let mut gone = Vec::new();
    for r in &rows {
        if row(conn, r.id)?.is_some() {
            gone.extend(remove_with_replies(conn, r)?);
        }
    }
    conn.execute(
        "DELETE FROM fedi_followers WHERE local = ?1",
        params![person_local(account)],
    )?;
    conn.execute(
        "DELETE FROM fedi_keys WHERE actor = ?1",
        params![person_local(account)],
    )?;
    Ok(gone)
}

#[cfg(test)]
mod tests {
    use super::*;
    use cartopolis_core::{CommunitySettings, JoinRule};

    const BASE: &str = "https://cartopolis.test";
    const SERVER: &str = "cartopolis.test";

    struct World {
        conn: Connection,
        owner: String,
        member: String,
        outsider: String,
        community: Uuid,
    }

    fn world() -> World {
        let conn = crate::db::open_and_migrate(":memory:").unwrap();
        let owner = crate::auth::create_account(&conn, "karim", "pw")
            .unwrap()
            .to_string();
        let member = crate::auth::create_account(&conn, "Sanne", "pw")
            .unwrap()
            .to_string();
        let outsider = crate::auth::create_account(&conn, "tom", "pw")
            .unwrap()
            .to_string();
        let settings = CommunitySettings {
            name: "Buurt Oosterpoort".into(),
            about: String::new(),
            join: JoinRule::Open,
            listed: true,
            members_start_topics: true,
            picture: None,
            place: None,
        };
        let community = communities::create(&conn, &owner, &settings, 1).unwrap();
        communities::join(&conn, &member, community, "", 2).unwrap();
        World {
            conn,
            owner,
            member,
            outsider,
            community,
        }
    }

    fn on(w: &World, handle: &str) {
        set_public(
            &w.conn,
            &w.owner,
            w.community,
            &PublicSettings {
                enabled: true,
                handle: Some(handle.into()),
                who_posts: WhoPosts::Members,
            },
            10,
        )
        .unwrap();
    }

    fn post(text: &str) -> NewPost {
        NewPost {
            text: text.into(),
            picture: None,
            place: PostPlace {
                latitude: 53.21,
                longitude: 6.58,
                name: Some("Meeuwerderweg".into()),
                view: Some(PointView {
                    altitude: 90.0,
                    pitch: -30.0,
                    yaw: 15.0,
                }),
            },
        }
    }

    #[test]
    fn only_a_moderator_switches_posts_on_and_the_address_is_chosen_once() {
        let w = world();
        let settings = PublicSettings {
            enabled: true,
            handle: Some("oosterpoort".into()),
            who_posts: WhoPosts::Members,
        };
        assert_eq!(
            set_public(&w.conn, &w.member, w.community, &settings, 1),
            Err(Refusal::NotAModerator)
        );
        assert_eq!(
            set_public(
                &w.conn,
                &w.owner,
                w.community,
                &PublicSettings {
                    handle: None,
                    ..settings.clone()
                },
                1
            ),
            Err(Refusal::NeedsHandle)
        );
        assert_eq!(
            set_public(
                &w.conn,
                &w.owner,
                w.community,
                &PublicSettings {
                    handle: Some("Ooster Poort".into()),
                    ..settings.clone()
                },
                1
            ),
            Err(Refusal::BadHandle)
        );
        set_public(&w.conn, &w.owner, w.community, &settings, 1).unwrap();
        assert_eq!(
            set_public(
                &w.conn,
                &w.owner,
                w.community,
                &PublicSettings {
                    handle: Some("other".into()),
                    ..settings.clone()
                },
                2
            ),
            Err(Refusal::HandleFixed)
        );
        // Switching off and on again keeps the address without naming it.
        let off = set_public(
            &w.conn,
            &w.owner,
            w.community,
            &PublicSettings {
                enabled: false,
                handle: None,
                who_posts: WhoPosts::Moderators,
            },
            3,
        )
        .unwrap();
        assert_eq!(off.handle, "oosterpoort");
        assert_eq!(community_by_handle(&w.conn, "oosterpoort").unwrap(), None);
    }

    #[test]
    fn a_handle_is_not_anybodys_username_and_is_never_reused() {
        let w = world();
        // `Sanne` has a capital; her address is still taken folded.
        let mut settings = PublicSettings {
            enabled: true,
            handle: Some("sanne".into()),
            who_posts: WhoPosts::Members,
        };
        assert_eq!(
            set_public(&w.conn, &w.owner, w.community, &settings, 1),
            Err(Refusal::HandleTaken)
        );
        settings.handle = Some("oosterpoort".into());
        set_public(&w.conn, &w.owner, w.community, &settings, 1).unwrap();
        forget_community(&w.conn, w.community, 5).unwrap();
        assert!(!handle_free(&w.conn, "oosterpoort").unwrap());
    }

    #[test]
    fn members_post_with_a_place_and_outsiders_do_not() {
        let w = world();
        assert_eq!(
            create_post(&w.conn, &w.member, w.community, &post("hi"), BASE, 1),
            Err(Refusal::PostsOff)
        );
        on(&w, "oosterpoort");
        assert_eq!(
            create_post(&w.conn, &w.outsider, w.community, &post("hi"), BASE, 1),
            Err(Refusal::NotAMember)
        );
        assert_eq!(
            create_post(&w.conn, &w.member, w.community, &post("   "), BASE, 1),
            Err(Refusal::Empty)
        );
        let mut far = post("hi");
        far.place.latitude = 200.0;
        assert_eq!(
            create_post(&w.conn, &w.member, w.community, &far, BASE, 1),
            Err(Refusal::BadPlace)
        );
        let id = create_post(
            &w.conn,
            &w.member,
            w.community,
            &post("Plant swap"),
            BASE,
            2,
        )
        .unwrap();
        let feed = feed(&w.conn, w.community, None, None, SERVER).unwrap();
        assert_eq!(feed.len(), 1);
        assert_eq!(feed[0].id, id);
        assert_eq!(feed[0].author.address, "Sanne@cartopolis.test");
        assert_eq!(
            feed[0].place.as_ref().unwrap().name.as_deref(),
            Some("Meeuwerderweg")
        );
        assert_eq!(feed[0].place.as_ref().unwrap().view.unwrap().pitch, -30.0);
        assert!(!feed[0].mine);
        let r = row(&w.conn, id).unwrap().unwrap();
        assert_eq!(r.ap_id, format!("{BASE}/ap/p/{id}"));
    }

    #[test]
    fn a_notice_board_takes_posts_from_moderators_only() {
        let w = world();
        set_public(
            &w.conn,
            &w.owner,
            w.community,
            &PublicSettings {
                enabled: true,
                handle: Some("board".into()),
                who_posts: WhoPosts::Moderators,
            },
            1,
        )
        .unwrap();
        assert_eq!(
            create_post(&w.conn, &w.member, w.community, &post("hi"), BASE, 1),
            Err(Refusal::ModeratorsOnly)
        );
        create_post(&w.conn, &w.owner, w.community, &post("notice"), BASE, 1).unwrap();
        let info = info(&w.conn, Some(&w.member), w.community, SERVER, true).unwrap();
        assert!(!info.may_post);
        assert_eq!(info.address.as_deref(), Some("board@cartopolis.test"));
    }

    #[test]
    fn replies_are_flat_and_anybody_signed_in_may_reply() {
        let w = world();
        on(&w, "oosterpoort");
        let p = create_post(
            &w.conn,
            &w.member,
            w.community,
            &post("Plant swap"),
            BASE,
            1,
        )
        .unwrap();
        let (r1, _) = create_reply(&w.conn, &w.outsider, p, "I'll come", BASE, 2).unwrap();
        // A reply to a reply hangs under the post.
        let (r2, _) = create_reply(&w.conn, &w.owner, r1, "great", BASE, 3).unwrap();
        let t = thread(&w.conn, r2, Some(&w.owner), SERVER).unwrap();
        assert_eq!(t.post.id, p);
        assert_eq!(t.post.replies, 2);
        assert_eq!(
            t.replies.iter().map(|r| r.id).collect::<Vec<_>>(),
            vec![r1, r2]
        );
        assert!(t.replies[1].mine);
    }

    #[test]
    fn the_author_deletes_with_replies_and_a_moderator_hides() {
        let w = world();
        on(&w, "oosterpoort");
        let p = create_post(&w.conn, &w.member, w.community, &post("one"), BASE, 1).unwrap();
        let (r, _) = create_reply(&w.conn, &w.outsider, p, "reply", BASE, 2).unwrap();
        assert_eq!(delete(&w.conn, &w.owner, p), Err(Refusal::NotYours));
        let gone = delete(&w.conn, &w.member, p).unwrap();
        assert_eq!(gone.iter().map(|g| g.id).collect::<Vec<_>>(), vec![p, r]);
        assert!(row(&w.conn, r).unwrap().is_none());

        let p = create_post(&w.conn, &w.member, w.community, &post("two"), BASE, 3).unwrap();
        assert_eq!(hide(&w.conn, &w.member, p), Err(Refusal::NotAModerator));
        hide(&w.conn, &w.owner, p).unwrap();
        assert!(feed(&w.conn, w.community, None, None, SERVER)
            .unwrap()
            .is_empty());
        assert_eq!(thread(&w.conn, p, None, SERVER), Err(Refusal::NoSuchPost));
    }

    #[test]
    fn a_remote_post_is_stored_once_and_its_author_alone_changes_it() {
        let w = world();
        on(&w, "oosterpoort");
        let remote = Remote {
            ap_id: "https://mastodon.example/statuses/1".into(),
            author_actor: "https://mastodon.example/users/anna".into(),
            via_actor: None,
            text: "Heron back on the quay".into(),
            picture: None,
            place: None,
            url: Some("https://mastodon.example/@anna/1".into()),
            created_ms: 5,
            parent: None,
        };
        let id = store_remote(&w.conn, w.community, &remote)
            .unwrap()
            .unwrap();
        assert_eq!(store_remote(&w.conn, w.community, &remote).unwrap(), None);
        assert_eq!(
            update_remote(
                &w.conn,
                &remote.ap_id,
                "https://elsewhere/users/x",
                "spam",
                6
            )
            .unwrap(),
            0
        );
        update_remote(&w.conn, &remote.ap_id, &remote.author_actor, "Heron!", 6).unwrap();
        let r = row(&w.conn, id).unwrap().unwrap();
        assert_eq!((r.text.as_str(), r.edited_ms), ("Heron!", Some(6)));
        let shown = dto(&w.conn, &r, None, SERVER).unwrap();
        assert!(!shown.author.local);
        assert_eq!(
            delete_remote(&w.conn, &remote.ap_id, &remote.author_actor).unwrap(),
            1
        );
        assert!(row(&w.conn, id).unwrap().is_none());
    }

    #[test]
    fn a_blocked_account_is_hidden_and_stays_out() {
        let w = world();
        on(&w, "oosterpoort");
        let actor = "https://spam.example/users/deals";
        let remote = Remote {
            ap_id: "https://spam.example/1".into(),
            author_actor: actor.into(),
            via_actor: None,
            text: "best phones".into(),
            picture: None,
            place: None,
            url: None,
            created_ms: 1,
            parent: None,
        };
        store_remote(&w.conn, w.community, &remote).unwrap();
        assert_eq!(
            block_actor(&w.conn, &w.member, w.community, actor, 2),
            Err(Refusal::NotAModerator)
        );
        block_actor(&w.conn, &w.owner, w.community, actor, 2).unwrap();
        assert!(actor_blocked(&w.conn, w.community, actor).unwrap());
        assert!(feed(&w.conn, w.community, None, None, SERVER)
            .unwrap()
            .is_empty());
    }

    #[test]
    fn only_a_public_posts_picture_is_public() {
        let w = world();
        on(&w, "oosterpoort");
        let hash = crate::assets::content_hash(b"jpeg");
        assert!(!picture_is_public(&w.conn, &hash).unwrap());
        let mut p = post("pic");
        p.picture = Some(hash.clone());
        let id = create_post(&w.conn, &w.member, w.community, &p, BASE, 1).unwrap();
        assert!(picture_is_public(&w.conn, &hash).unwrap());
        hide(&w.conn, &w.owner, id).unwrap();
        assert!(!picture_is_public(&w.conn, &hash).unwrap());
        p.picture = Some("../etc".into());
        assert_eq!(
            create_post(&w.conn, &w.member, w.community, &p, BASE, 1),
            Err(Refusal::BadPicture)
        );
    }

    #[test]
    fn a_deleted_account_takes_its_posts_with_it() {
        let w = world();
        on(&w, "oosterpoort");
        let p = create_post(&w.conn, &w.member, w.community, &post("mine"), BASE, 1).unwrap();
        let (r, _) = create_reply(&w.conn, &w.outsider, p, "yours", BASE, 2).unwrap();
        let gone = forget_account(&w.conn, &w.member).unwrap();
        assert_eq!(gone.len(), 2);
        assert!(row(&w.conn, r).unwrap().is_none());
    }
}
