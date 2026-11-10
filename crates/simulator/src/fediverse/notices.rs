//! Telling somebody what happened to them in public: a reply to what they
//! wrote, a mention, a like, a boost, a new follower (`post_notices`).
//!
//! A notice is written where the thing arrives — the inbox for the fediverse,
//! the posts API for somebody here — and read by the app and the web page
//! (`/api/fediverse/notices`). It is one row per thing, so a server that sends
//! the same like twice, or a like taken back and given again, tells once.
//! Nobody is told about what they did themselves, nor about somebody they
//! blocked.

use cartopolis_core::posts::{NoticeKind, PostNotice, PostNotices};
use rusqlite::{params, Connection};
use uuid::Uuid;

use super::posts::{self, Refusal};

/// Notices one page answers with.
pub const PAGE: i64 = 50;
/// Characters of a post shown with its notice.
const EXCERPT: usize = 120;

fn kind_name(kind: NoticeKind) -> &'static str {
    match kind {
        NoticeKind::Reply => "reply",
        NoticeKind::Mention => "mention",
        NoticeKind::Like => "like",
        NoticeKind::Boost => "boost",
        NoticeKind::Follow => "follow",
    }
}

fn kind_of(name: &str) -> Option<NoticeKind> {
    Some(match name {
        "reply" => NoticeKind::Reply,
        "mention" => NoticeKind::Mention,
        "like" => NoticeKind::Like,
        "boost" => NoticeKind::Boost,
        "follow" => NoticeKind::Follow,
        _ => return None,
    })
}

/// Who did it.
#[derive(Debug, Clone, Copy)]
pub enum Who<'a> {
    /// An account here, by `accounts.id`.
    Here(&'a str),
    /// An actor elsewhere, by its fediverse id.
    Elsewhere(&'a str),
}

/// Tell `account` that `who` did `kind`, about `post` when there is one.
/// Answers whether a notice was written.
pub fn tell(
    conn: &Connection,
    account: &str,
    kind: NoticeKind,
    post: Option<Uuid>,
    who: Who<'_>,
    now_ms: i64,
) -> rusqlite::Result<bool> {
    let (actor, local) = match who {
        Who::Here(a) => {
            if a == account || blocked_here(conn, account, a)? {
                return Ok(false);
            }
            (a, true)
        }
        Who::Elsewhere(a) => (a, false),
    };
    let n = conn.execute(
        "INSERT OR IGNORE INTO post_notices (account, kind, post, actor, local, created_ms)
         SELECT ?1, ?2, ?3, ?4, ?5, ?6 WHERE EXISTS (SELECT 1 FROM accounts WHERE id = ?1)",
        params![
            account,
            kind_name(kind),
            post.map(|p| p.to_string()),
            actor,
            local,
            now_ms
        ],
    )?;
    Ok(n == 1)
}

/// Whether `account` blocked `other` (both here).
fn blocked_here(conn: &Connection, account: &str, other: &str) -> rusqlite::Result<bool> {
    conn.query_row(
        "SELECT EXISTS (SELECT 1 FROM blocks b JOIN accounts a ON lower(a.username) = lower(b.blocked_username)
                        WHERE b.blocker_id = ?1 AND a.id = ?2)",
        params![account, other],
        |r| r.get(0),
    )
}

/// Tell whoever wrote the post here `post` (by its fediverse id) about it.
/// Nothing for a post written elsewhere.
pub fn tell_author(
    conn: &Connection,
    post_ap_id: &str,
    kind: NoticeKind,
    who: Who<'_>,
    now_ms: i64,
) -> Result<(), Refusal> {
    for row in posts::rows_by_ap_id(conn, post_ap_id)? {
        if let Some(author) = &row.author_account {
            tell(conn, author, kind, Some(row.id), who, now_ms)?;
        }
    }
    Ok(())
}

/// A reply arrived or was written: tell whoever wrote what it answers, and
/// whoever started the thread, once each.
pub fn tell_replied(
    conn: &Connection,
    reply: Uuid,
    who: Who<'_>,
    now_ms: i64,
) -> Result<(), Refusal> {
    let Some(row) = posts::row(conn, reply)? else {
        return Ok(());
    };
    let mut told: Vec<String> = Vec::new();
    let answered = row
        .in_reply_to
        .as_deref()
        .map(|to| posts::rows_by_ap_id(conn, to))
        .transpose()?
        .unwrap_or_default()
        .into_iter()
        .find(|r| r.community == row.community);
    let root = row
        .parent
        .map(|p| posts::row(conn, p))
        .transpose()?
        .flatten();
    for r in answered.into_iter().chain(root) {
        if let Some(author) = r.author_account {
            if !told.contains(&author) {
                tell(conn, &author, NoticeKind::Reply, Some(reply), who, now_ms)?;
                told.push(author);
            }
        }
    }
    Ok(())
}

/// `account`'s notices, newest first, and how many are unread.
pub fn list(conn: &Connection, account: &str, server: &str) -> Result<PostNotices, Refusal> {
    let unread: i64 = conn.query_row(
        "SELECT COUNT(*) FROM post_notices WHERE account = ?1 AND read = 0",
        params![account],
        |r| r.get(0),
    )?;
    let mut stmt = conn.prepare(
        "SELECT id, kind, post, actor, local, created_ms, read FROM post_notices
         WHERE account = ?1 ORDER BY created_ms DESC, id DESC LIMIT ?2",
    )?;
    let rows: Vec<(i64, String, Option<String>, String, bool, i64, bool)> = stmt
        .query_map(params![account, PAGE], |r| {
            Ok((
                r.get(0)?,
                r.get(1)?,
                r.get(2)?,
                r.get(3)?,
                r.get(4)?,
                r.get(5)?,
                r.get(6)?,
            ))
        })?
        .collect::<Result<_, _>>()?;
    let mut items = Vec::with_capacity(rows.len());
    for (id, kind, post, actor, local, created_ms, read) in rows {
        let Some(kind) = kind_of(&kind) else { continue };
        let post = post
            .as_deref()
            .and_then(|p| Uuid::parse_str(p).ok())
            .map(|p| posts::row(conn, p))
            .transpose()?
            .flatten();
        // What it was about is gone, or hidden: so is the notice.
        if kind != NoticeKind::Follow && post.as_ref().is_none_or(|p| p.hidden) {
            continue;
        }
        let who = if local {
            posts::local_author(conn, &actor, server)?
        } else {
            posts::remote_author(conn, &actor)?
        };
        items.push(PostNotice {
            id,
            kind,
            who,
            community: post.as_ref().map(|p| p.community),
            excerpt: post.as_ref().map(|p| {
                let mut s: String = p.text.chars().take(EXCERPT).collect();
                if p.text.chars().count() > EXCERPT {
                    s.push('…');
                }
                s
            }),
            post: post.map(|p| p.id),
            created_ms,
            read,
        });
    }
    Ok(PostNotices {
        unread: u32::try_from(unread).unwrap_or(0),
        items,
    })
}

/// Mark `account`'s notices read, up to `up_to` (every one when `None`).
pub fn mark_read(conn: &Connection, account: &str, up_to: Option<i64>) -> rusqlite::Result<usize> {
    conn.execute(
        "UPDATE post_notices SET read = 1 WHERE account = ?1 AND read = 0 AND id <= ?2",
        params![account, up_to.unwrap_or(i64::MAX)],
    )
}

/// An account is gone: so are its notices, both ways.
pub fn forget_account(conn: &Connection, account: &str) -> rusqlite::Result<()> {
    conn.execute(
        "DELETE FROM post_notices WHERE account = ?1 OR (local = 1 AND actor = ?1)",
        params![account],
    )?;
    Ok(())
}
