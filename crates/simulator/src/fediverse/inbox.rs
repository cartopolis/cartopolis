//! What to do with an activity another server sends.
//!
//! By the time anything here runs, `routes` has checked the request's signature
//! against the key of the actor it names and refused a blocked server. What is
//! left is deciding what the activity means for this server:
//!
//! - **Follow / Undo**: somebody elsewhere follows (or stops following) a
//!   community or an account here. Always accepted, unless the community's
//!   moderators blocked them.
//! - **Accept / Reject**: the answer to a community here following something
//!   elsewhere.
//! - **Create**: a post or reply. It lands in a community here when it answers
//!   a post here, when it is addressed to or mentions a community here, or when
//!   its author is followed by a community here. A post that came *to* a
//!   community is then boosted to the community's followers, which is what
//!   being a group means (FEP-1b12).
//! - **Announce**: a boost. Only a followed source's boosts count — that is how
//!   a Lemmy community delivers its posts — and only what the boosted object's
//!   own server vouches for is believed: an object hosted elsewhere than the
//!   booster is fetched from its origin rather than taken from the boost.
//! - **Update / Delete**: only from the author's own server.
//! - **Like / Dislike / Undo**: a vote on a post here (Mastodon's favourite,
//!   Lemmy's up- and downvote), counted on the post.
//! - **Flag**: a report about a post here, for the community's moderators.
//!
//! Everything else (emoji reactions, moves) is acknowledged and dropped.

use std::sync::Mutex;

use anyhow::Context as _;
use cartopolis_core::moderation::Filter;
use cartopolis_core::posts::{NoticeKind, PostPicture};
use rusqlite::{params, Connection, OptionalExtension};
use serde_json::Value;
use uuid::Uuid;

use super::fetch::{host_of, Client, FetchError};
use super::notices::{self, Who};
use super::objects::{self, addressees, id_of, kind, Actor};
use super::posts::{self, Remote};
use super::{blocklist, delivery, local_actor, servers, signing, Federation, Local};

/// How long a fetched actor is believed before it is fetched again.
const ACTOR_FRESH_MS: i64 = 24 * 3600 * 1000;

/// What handling an activity needs.
pub struct Ctx<'a> {
    pub conn: &'a Mutex<Connection>,
    pub client: &'a Client,
    pub fed: &'a Federation,
    pub filter: &'a Filter,
    pub asset_dir: &'a str,
}

impl Ctx<'_> {
    fn db<T>(&self, f: impl FnOnce(&Connection) -> anyhow::Result<T>) -> anyhow::Result<T> {
        let c = self
            .conn
            .lock()
            .map_err(|_| anyhow::anyhow!("database lock poisoned"))?;
        f(&c)
    }

    /// The instance key, for fetches that must be signed.
    fn instance_signer(&self) -> anyhow::Result<(rsa::RsaPrivateKey, String)> {
        let key = self.db(|c| signing::private_key(c, "instance", super::now_ms()))?;
        Ok((key, super::key_id(&super::instance_url(&self.fed.base))))
    }

    /// GET an ActivityPub document, signed as this server. Nothing is asked of
    /// a blocked server.
    pub fn fetch(&self, url: &str) -> Result<Value, FetchError> {
        self.refuse_blocked(url)?;
        let signer = self
            .instance_signer()
            .map_err(|e| FetchError::Transport(url.into(), e.to_string()))?;
        self.client.get_json(url, Some((&signer.0, &signer.1)))
    }

    fn refuse_blocked(&self, url: &str) -> Result<(), FetchError> {
        let blocked = host_of(url).is_none_or(|h| {
            self.db(|c| Ok(blocklist::is_blocked(c, &h)?))
                .unwrap_or(true)
        });
        if blocked {
            return Err(FetchError::Refused(url.into(), "a blocked server"));
        }
        Ok(())
    }
}

/// Store an actor we have read.
pub fn store_actor(conn: &Connection, a: &Actor, now_ms: i64) -> rusqlite::Result<()> {
    conn.execute(
        "INSERT INTO fedi_actors (id, kind, username, host, name, inbox, shared_inbox, key_id,
                                  public_pem, url, fetched_ms)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11)
         ON CONFLICT (id) DO UPDATE SET kind = ?2, username = ?3, host = ?4, name = ?5,
             inbox = ?6, shared_inbox = ?7, key_id = ?8, public_pem = ?9, url = ?10,
             fetched_ms = ?11",
        params![
            a.id,
            a.kind,
            a.username,
            a.host,
            a.name,
            a.inbox,
            a.shared_inbox,
            a.key_id,
            a.public_pem,
            a.url,
            now_ms
        ],
    )?;
    Ok(())
}

fn cached_actor(
    conn: &Connection,
    sql_where: &str,
    value: &str,
) -> rusqlite::Result<Option<(Actor, i64)>> {
    conn.query_row(
        &format!(
            "SELECT id, kind, username, host, name, inbox, shared_inbox, key_id, public_pem, url,
                    fetched_ms
             FROM fedi_actors WHERE {sql_where} = ?1"
        ),
        params![value],
        |r| {
            Ok((
                Actor {
                    id: r.get(0)?,
                    kind: r.get(1)?,
                    username: r.get(2)?,
                    host: r.get(3)?,
                    name: r.get(4)?,
                    inbox: r.get(5)?,
                    shared_inbox: r.get(6)?,
                    key_id: r.get(7)?,
                    public_pem: r.get(8)?,
                    url: r.get(9)?,
                },
                r.get(10)?,
            ))
        },
    )
    .optional()
}

/// An actor elsewhere, from the cache while it is fresh (or `force` is off and
/// it is merely old), fetched otherwise. Its key must be its own: a document
/// whose key names another owner, or another server, is refused.
pub fn resolve_actor(ctx: &Ctx<'_>, url: &str, force: bool) -> anyhow::Result<Actor> {
    let now = super::now_ms();
    if !force {
        if let Some((a, at)) = ctx.db(|c| Ok(cached_actor(c, "id", url)?))? {
            if now - at < ACTOR_FRESH_MS {
                return Ok(a);
            }
        }
    }
    let doc = ctx.fetch(url)?;
    accept_actor(ctx, url, &doc, None)
}

/// Check an actor document fetched from `url` and keep it, with the key named
/// `key_id` when one is asked for.
fn accept_actor(
    ctx: &Ctx<'_>,
    url: &str,
    doc: &Value,
    key_id: Option<&str>,
) -> anyhow::Result<Actor> {
    let actor = objects::read_actor_key(doc, key_id).with_context(|| match key_id {
        Some(k) => format!("{url} publishes no key {k}"),
        None => format!("{url} is not an actor"),
    })?;
    if actor.id != url.split('#').next().unwrap_or(url) {
        anyhow::bail!("{url} answered as {}", actor.id);
    }
    if host_of(&actor.key_id) != Some(actor.host.clone()) {
        anyhow::bail!("{url}'s key lives on another server");
    }
    ctx.db(|c| Ok(store_actor(c, &actor, super::now_ms())?))?;
    Ok(actor)
}

/// The actor a signature's key belongs to.
pub fn actor_for_key(ctx: &Ctx<'_>, key_id: &str, force: bool) -> anyhow::Result<Actor> {
    if !force {
        if let Some((a, _)) = ctx.db(|c| Ok(cached_actor(c, "key_id", key_id)?))? {
            return Ok(a);
        }
    }
    // Most servers name the key `<actor>#main-key`; the rest publish a key
    // document that names its owner.
    let url = key_id.split('#').next().unwrap_or(key_id);
    let doc = ctx.fetch(url)?;
    if doc.get("inbox").is_some() {
        return accept_actor(ctx, url, &doc, Some(key_id));
    }
    let owner = doc
        .get("owner")
        .and_then(Value::as_str)
        .with_context(|| format!("{key_id} names no owner"))?
        .to_string();
    let owner_doc = ctx.fetch(&owner)?;
    accept_actor(ctx, &owner, &owner_doc, Some(key_id))
}

/// Handle one activity from `sender`.
pub fn handle(ctx: &Ctx<'_>, sender: &Actor, activity: &Value) -> anyhow::Result<()> {
    match kind(activity) {
        "Follow" => on_follow(ctx, sender, activity),
        "Undo" => on_undo(ctx, sender, activity),
        "Accept" | "Reject" => on_answer(ctx, sender, activity),
        "Create" => on_create(ctx, sender, activity),
        "Update" => on_update(ctx, sender, activity),
        "Delete" => on_delete(ctx, sender, activity),
        "Announce" => on_announce(ctx, sender, activity),
        "Like" => on_vote(ctx, sender, activity, 1),
        "Dislike" => on_vote(ctx, sender, activity, -1),
        "Flag" => on_flag(ctx, sender, activity),
        "Move" => on_move(ctx, sender, activity),
        other => {
            tracing::debug!(kind = other, from = %sender.id, "fediverse: activity ignored");
            Ok(())
        }
    }
}

fn on_follow(ctx: &Ctx<'_>, sender: &Actor, act: &Value) -> anyhow::Result<()> {
    let Some(object) = act.get("object").and_then(id_of) else {
        return Ok(());
    };
    let now = super::now_ms();
    let (local, signer) = match local_actor(&ctx.fed.base, &object) {
        Some(Local::Group(c)) => {
            let open = ctx.db(|conn| Ok(posts::public(conn, c)?.is_some_and(|p| p.enabled)))?;
            if !open {
                return Ok(());
            }
            if ctx.db(|conn| Ok(posts::actor_blocked(conn, c, &sender.id)?))? {
                let reject =
                    objects::document(objects::answer_follow("Reject", &object, act.clone()));
                ctx.db(|conn| {
                    Ok(delivery::enqueue(
                        conn,
                        &sender.inbox,
                        &posts::group_local(c),
                        &reject,
                        now,
                    )?)
                })?;
                ctx.fed.wake.notify_one();
                return Ok(());
            }
            (posts::group_local(c), posts::group_local(c))
        }
        Some(Local::Person(a)) => {
            let exists = ctx.db(|conn| {
                Ok(conn
                    .query_row("SELECT 1 FROM accounts WHERE id = ?1", params![a], |_| {
                        Ok(())
                    })
                    .optional()?
                    .is_some())
            })?;
            if !exists {
                return Ok(());
            }
            (posts::person_local(&a), posts::person_local(&a))
        }
        // A community's reader follows; nobody follows it.
        _ => return Ok(()),
    };
    // Which boost the follower's server reads (`servers`), learned now so the
    // first one it is sent is in its shape.
    let _ = servers::software(ctx.conn, ctx.client, &sender.host, now);
    let accept = objects::document(objects::answer_follow("Accept", &object, act.clone()));
    let follow_id = id_of(act);
    ctx.db(|conn| {
        conn.execute(
            "INSERT INTO fedi_followers (local, actor, followed_ms, follow_id) VALUES (?1, ?2, ?3, ?4)
             ON CONFLICT (local, actor) DO UPDATE SET follow_id = ?4",
            params![local, sender.id, now, follow_id],
        )?;
        if let Some(account) = local.strip_prefix("u:") {
            store_actor(conn, sender, now)?;
            notices::tell(
                conn,
                account,
                NoticeKind::Follow,
                None,
                Who::Elsewhere(&sender.id),
                now,
            )?;
        }
        delivery::enqueue(conn, &sender.inbox, &signer, &accept, now)?;
        Ok(())
    })?;
    ctx.fed.wake.notify_one();
    tracing::info!(follower = %sender.id, %local, "fediverse: new follower");
    Ok(())
}

fn on_undo(ctx: &Ctx<'_>, sender: &Actor, act: &Value) -> anyhow::Result<()> {
    let Some(inner) = act.get("object") else {
        return Ok(());
    };
    // A vote taken back.
    if matches!(kind(inner), "Like" | "Dislike") {
        let activity = id_of(inner);
        let object = inner.get("object").and_then(id_of);
        ctx.db(|c| {
            Ok(posts::unvote(
                c,
                &sender.id,
                activity.as_deref(),
                object.as_deref(),
            )?)
        })?;
        return Ok(());
    }
    // A boost of a post here taken back.
    if kind(inner) == "Announce" {
        let activity = id_of(inner).unwrap_or_default();
        let object = inner.get("object").and_then(id_of).unwrap_or_default();
        ctx.db(|c| Ok(posts::boost(c, &object, &sender.id, &activity, false, 0)?))?;
        return Ok(());
    }
    // Otherwise only a follow can be undone here; a boost undone means nothing
    // to this server.
    if inner.is_object() && kind(inner) != "Follow" {
        return Ok(());
    }
    let target = inner.get("object").and_then(id_of);
    ctx.db(|conn| {
        match target
            .as_deref()
            .and_then(|t| local_actor(&ctx.fed.base, t))
        {
            Some(Local::Group(c)) => {
                conn.execute(
                    "DELETE FROM fedi_followers WHERE local = ?1 AND actor = ?2",
                    params![posts::group_local(c), sender.id],
                )?;
            }
            Some(Local::Person(a)) => {
                conn.execute(
                    "DELETE FROM fedi_followers WHERE local = ?1 AND actor = ?2",
                    params![posts::person_local(&a), sender.id],
                )?;
            }
            // Given only by its id: a vote's, if it was one, and otherwise the
            // follow that id made. Anything else so named (a boost, a
            // reaction) leaves every follow be.
            _ if !inner.is_object() => {
                let id = id_of(inner).unwrap_or_default();
                if posts::unvote(conn, &sender.id, Some(&id), None)? == 0
                    && !posts::boost(conn, "", &sender.id, &id, false, 0)?
                {
                    conn.execute(
                        "DELETE FROM fedi_followers WHERE actor = ?1 AND follow_id = ?2",
                        params![sender.id, id],
                    )?;
                }
            }
            _ => {}
        }
        Ok(())
    })
}

fn on_answer(ctx: &Ctx<'_>, sender: &Actor, act: &Value) -> anyhow::Result<()> {
    let accepted = kind(act) == "Accept";
    let Some(inner) = act.get("object") else {
        return Ok(());
    };
    let follow_id = id_of(inner);
    ctx.db(|conn| {
        let n = match &follow_id {
            Some(id) if accepted => conn.execute(
                "UPDATE community_follows SET accepted = 1 WHERE follow_id = ?1 AND actor = ?2",
                params![id, sender.id],
            )?,
            Some(id) => conn.execute(
                "DELETE FROM community_follows WHERE follow_id = ?1 AND actor = ?2",
                params![id, sender.id],
            )?,
            None => 0,
        };
        // Some servers answer with a follow of their own making rather than
        // ours; its `actor` still names the community that asked.
        if n == 0 {
            if let Some(Local::Group(c) | Local::Reader(c)) = inner
                .get("actor")
                .and_then(id_of)
                .and_then(|a| local_actor(&ctx.fed.base, &a))
            {
                if accepted {
                    conn.execute(
                        "UPDATE community_follows SET accepted = 1 WHERE community = ?1 AND actor = ?2",
                        params![c.to_string(), sender.id],
                    )?;
                } else {
                    conn.execute(
                        "DELETE FROM community_follows WHERE community = ?1 AND actor = ?2",
                        params![c.to_string(), sender.id],
                    )?;
                }
            }
        }
        Ok(())
    })
}

/// The object an activity carries, as its own server has it: taken as sent
/// when the sender is that server, fetched from it otherwise.
fn authoritative_object(ctx: &Ctx<'_>, sender: &Actor, object: &Value) -> anyhow::Result<Value> {
    let id = id_of(object).context("an object with no id")?;
    let same_server = host_of(&id).as_deref() == Some(sender.host.as_str());
    if object.is_object() && same_server {
        return Ok(object.clone());
    }
    let fetched = ctx.fetch(&id)?;
    if id_of(&fetched).as_deref() != Some(id.as_str()) {
        anyhow::bail!("{id} answered as something else");
    }
    Ok(fetched)
}

/// Object types that are posts.
fn is_post(v: &Value) -> bool {
    matches!(kind(v), "Note" | "Page" | "Article" | "Question" | "Event")
}

fn on_create(ctx: &Ctx<'_>, sender: &Actor, act: &Value) -> anyhow::Result<()> {
    let Some(object) = act.get("object") else {
        return Ok(());
    };
    let obj = authoritative_object(ctx, sender, object)?;
    if !is_post(&obj) {
        return Ok(());
    }
    let author_id = obj.get("attributedTo").and_then(id_of).unwrap_or_default();
    let author = if author_id == sender.id {
        sender.clone()
    } else {
        resolve_actor(ctx, &author_id, false)?
    };
    accept_object(ctx, &obj, &author, None, Some(act))
}

/// Where a post from elsewhere belongs here, and under which post.
fn targets(
    ctx: &Ctx<'_>,
    obj: &Value,
    act: Option<&Value>,
    author: &Actor,
    via: Option<&Actor>,
) -> anyhow::Result<Vec<(Uuid, Option<Uuid>, bool)>> {
    let mut out: Vec<(Uuid, Option<Uuid>, bool)> = Vec::new();
    let reply_to = obj.get("inReplyTo").and_then(id_of);
    ctx.db(|conn| {
        if let Some(parent) = &reply_to {
            // A reply in a thread that is the community's own is boosted to its
            // followers; one in a thread mirrored from a followed source is
            // that source's business, and is only kept.
            for row in posts::rows_by_ap_id(conn, parent)? {
                if !row.hidden {
                    let own = row.via_actor.is_none();
                    out.push((row.community, Some(row.parent.unwrap_or(row.id)), own));
                }
            }
            return Ok(());
        }
        let mut named = addressees(obj);
        if let Some(a) = act {
            named.extend(addressees(a));
        }
        for n in named {
            if let Some(Local::Group(c)) = local_actor(&ctx.fed.base, &n) {
                if !out.iter().any(|(t, _, _)| *t == c) {
                    out.push((c, None, true));
                }
            }
        }
        // A followed source's own posts, or a followed group's boosts.
        let source = via.map_or(author.id.as_str(), |v| v.id.as_str());
        let mut stmt = conn
            .prepare("SELECT community FROM community_follows WHERE actor = ?1 AND accepted = 1")?;
        let following: Vec<String> = stmt
            .query_map(params![source], |r| r.get(0))?
            .collect::<Result<_, _>>()?;
        for c in following.iter().filter_map(|c| Uuid::parse_str(c).ok()) {
            if !out.iter().any(|(t, _, _)| *t == c) {
                out.push((c, None, false));
            }
        }
        Ok(())
    })?;
    Ok(out)
}

/// Keep a post or reply from elsewhere in every community it belongs in. When
/// it came *to* a community (rather than from a source the community follows),
/// the community boosts it to its own followers.
fn accept_object(
    ctx: &Ctx<'_>,
    obj: &Value,
    author: &Actor,
    via: Option<&Actor>,
    act: Option<&Value>,
) -> anyhow::Result<()> {
    let ap_id = id_of(obj).context("an object with no id")?;
    // Our own post coming back, boosted by a community elsewhere: it is here
    // already, as itself.
    if super::local_post(&ctx.fed.base, &ap_id).is_some() {
        return Ok(());
    }
    // A post is made on its author's own server. An object on one server that
    // names an author on another is somebody writing in their name: the
    // signature and `authoritative_object` prove only where the object is.
    if host_of(&ap_id).as_deref() != Some(author.host.as_str()) {
        tracing::info!(%ap_id, author = %author.id, "fediverse: author on another server, not kept");
        return Ok(());
    }
    // The signer's server was checked against the block list; the author's
    // and the booster's may be others, when a community or a relay passes a
    // post on.
    for host in std::iter::once(&author.host).chain(via.map(|v| &v.host)) {
        if ctx.db(|c| Ok(blocklist::is_blocked(c, host)?))? {
            tracing::debug!(%ap_id, %host, "fediverse: from a blocked server, not kept");
            return Ok(());
        }
    }
    // A followers-only post or a private mention is not ours to show to
    // anybody, which is what a community's posts are.
    if !objects::is_public(obj) {
        tracing::debug!(%ap_id, "fediverse: not public, not kept");
        return Ok(());
    }
    let targets = targets(ctx, obj, act, author, via)?;
    if targets.is_empty() {
        return Ok(());
    }
    let content = read_content(ctx, obj);
    let (text, picture) = (content.text.clone(), content.picture.clone());
    if text.is_empty() && picture.is_none() {
        return Ok(());
    }
    let now = super::now_ms();
    let place = objects::object_place(obj);
    let url = obj
        .get("url")
        .and_then(id_of)
        .filter(|u| super::place_from_link(u).is_none())
        .or_else(|| Some(ap_id.clone()))
        .filter(|u| super::web_link(u));
    for (community, parent, to_group) in targets {
        let stored = ctx.db(|conn| {
            if posts::actor_blocked(conn, community, &author.id)?
                || via.is_some_and(|v| posts::actor_blocked(conn, community, &v.id).unwrap_or(true))
            {
                return Ok(None);
            }
            store_actor(conn, author, now)?;
            let remote = Remote {
                ap_id: ap_id.clone(),
                author_actor: author.id.clone(),
                via_actor: via.map(|v| v.id.clone()),
                text: text.clone(),
                picture: picture.clone(),
                place: place.clone(),
                url: url.clone(),
                created_ms: objects::published_ms(obj).unwrap_or(now).min(now),
                parent,
                in_reply_to: obj.get("inReplyTo").and_then(id_of),
                warning: content.warning.clone(),
                sensitive: content.sensitive,
                picture_alt: content.picture_alt.clone(),
                more_pictures: content.more_pictures.clone(),
            };
            match posts::store_remote(conn, community, &remote) {
                Ok(id) => Ok(id),
                Err(posts::Refusal::PostsOff) => Ok(None),
                Err(e) => Err(anyhow::anyhow!(e)),
            }
        })?;
        if let Some(id) = stored {
            tracing::info!(%ap_id, %community, author = %author.id, "fediverse: post arrived");
            ctx.db(|conn| {
                let who = Who::Elsewhere(&author.id);
                if parent.is_some() {
                    notices::tell_replied(conn, id, who, now)?;
                }
                for account in mentioned_here(&ctx.fed.base, obj) {
                    notices::tell(conn, &account, NoticeKind::Mention, Some(id), who, now)?;
                }
                Ok(())
            })?;
            if to_group {
                announce_to_followers(ctx, community, act.cloned(), &ap_id, now)?;
            }
        }
    }
    Ok(())
}

/// A community boosts something to its followers, each server the way it reads
/// a boost (`servers`): the activity (Lemmy) or the object's id (Mastodon).
pub fn announce_to_followers(
    ctx: &Ctx<'_>,
    community: Uuid,
    activity: Option<Value>,
    object_id: &str,
    now: i64,
) -> anyhow::Result<()> {
    let local = posts::group_local(community);
    ctx.db(|conn| {
        let inboxes = delivery::inboxes(conn, Some(&local), &[])?;
        if inboxes.is_empty() {
            return Ok(());
        }
        let of_activity = activity.map(|a| {
            objects::document(objects::announce(
                &ctx.fed.base,
                community,
                strip_context(a),
            ))
        });
        let of_object = objects::document(objects::announce(
            &ctx.fed.base,
            community,
            Value::String(object_id.to_string()),
        ));
        delivery::fan_out_boost(
            conn,
            &inboxes,
            &local,
            of_activity.as_ref(),
            Some(&of_object),
            now,
        )?;
        Ok(())
    })?;
    ctx.fed.wake.notify_one();
    Ok(())
}

/// A like or a dislike of a post here. Lemmy sends a vote to the community, and
/// Mastodon a favourite to the author; both reach the one inbox.
fn on_vote(ctx: &Ctx<'_>, sender: &Actor, act: &Value, value: i64) -> anyhow::Result<()> {
    let (Some(activity), Some(object)) = (id_of(act), act.get("object").and_then(id_of)) else {
        return Ok(());
    };
    ctx.db(|c| {
        let now = super::now_ms();
        if posts::vote(c, &object, &sender.id, &activity, value, now)? && value > 0 {
            store_actor(c, sender, now)?;
            notices::tell_author(
                c,
                &object,
                NoticeKind::Like,
                Who::Elsewhere(&sender.id),
                now,
            )?;
        }
        Ok(())
    })
}

/// A report from another server. Mastodon sends one as its server's own actor,
/// naming the account and the posts reported; Lemmy as the person who reported,
/// naming the post. Each post here that it names becomes a report for that
/// community's moderators. One that names only an account is dropped: a
/// community's report is about a post.
fn on_flag(ctx: &Ctx<'_>, sender: &Actor, act: &Value) -> anyhow::Result<()> {
    let objects_named = objects::ids(act.get("object"));
    let reason = act
        .get("content")
        .or_else(|| act.get("summary"))
        .and_then(Value::as_str)
        .map(objects::html_to_text)
        .unwrap_or_default();
    let (reason, _) = ctx.filter.mask(&reason);
    let now = super::now_ms();
    let n = ctx.db(|c| {
        let mut n = 0;
        for o in &objects_named {
            n += posts::report(c, o, &sender.id, &reason, now)?;
        }
        Ok(n)
    })?;
    if n > 0 {
        tracing::info!(from = %sender.id, reports = n, "fediverse: report received");
    }
    Ok(())
}

/// An account a community here follows moved to another address. The new
/// account must name the old one as itself (`alsoKnownAs`), as Mastodon
/// requires before it moves followers; then each community that followed the
/// old one follows the new one, and forgets the old.
fn on_move(ctx: &Ctx<'_>, sender: &Actor, act: &Value) -> anyhow::Result<()> {
    if act.get("object").and_then(id_of).as_deref() != Some(sender.id.as_str()) {
        return Ok(());
    }
    let Some(target) = act.get("target").and_then(id_of) else {
        return Ok(());
    };
    let following: Vec<(String, String)> = ctx.db(|c| {
        let mut stmt =
            c.prepare("SELECT community, created_by FROM community_follows WHERE actor = ?1")?;
        let rows = stmt.query_map(params![sender.id], |r| Ok((r.get(0)?, r.get(1)?)))?;
        Ok(rows.collect::<Result<_, _>>()?)
    })?;
    if following.is_empty() {
        return Ok(());
    }
    let doc = ctx.fetch(&target)?;
    if !objects::ids(doc.get("alsoKnownAs")).contains(&sender.id) {
        anyhow::bail!("{target} does not say it was {}", sender.id);
    }
    let moved = accept_actor(ctx, &target, &doc, None)?;
    let now = super::now_ms();
    ctx.db(|c| {
        for (community, by) in &following {
            let Ok(id) = Uuid::parse_str(community) else {
                continue;
            };
            let reader = super::reader_url(&ctx.fed.base, id);
            let follow_id = format!("{reader}#follow-{}", Uuid::new_v4());
            c.execute(
                "INSERT INTO community_follows (community, actor, follow_id, accepted, created_by, created_ms)
                 VALUES (?1, ?2, ?3, 0, ?4, ?5)
                 ON CONFLICT (community, actor) DO UPDATE SET follow_id = ?3, accepted = 0",
                params![community, moved.id, follow_id, by, now],
            )?;
            c.execute(
                "DELETE FROM community_follows WHERE community = ?1 AND actor = ?2",
                params![community, sender.id],
            )?;
            let body = objects::document(objects::follow(&follow_id, &reader, &moved.id));
            delivery::enqueue(c, &moved.inbox, &posts::reader_local(id), &body, now)?;
        }
        Ok(())
    })?;
    ctx.fed.wake.notify_one();
    tracing::info!(from = %sender.id, to = %moved.id, "fediverse: a followed account moved");
    Ok(())
}

/// What a post from elsewhere says and shows: its words, masked; its pictures,
/// copied in, with what each shows in words; its content warning.
fn read_content(ctx: &Ctx<'_>, obj: &Value) -> posts::Content {
    let (text, _) = ctx.filter.mask(&objects::object_text(obj));
    let listed = objects::images(obj);
    let pictures_listed = listed.len();
    let mut pictures: Vec<PostPicture> = listed
        .into_iter()
        .filter_map(|(url, alt)| match copy_picture(ctx, &url) {
            Ok(hash) => Some(PostPicture {
                hash,
                alt: alt.map(|a| ctx.filter.mask(&a).0),
            }),
            Err(e) => {
                tracing::info!(%url, "fediverse: picture not copied: {e:#}");
                None
            }
        })
        .collect();
    let first = (!pictures.is_empty()).then(|| pictures.remove(0));
    posts::Content {
        text,
        picture: first.as_ref().map(|p| p.hash.clone()),
        picture_alt: first.and_then(|p| p.alt),
        more_pictures: pictures,
        pictures_listed,
        warning: objects::object_warning(obj).map(|w| ctx.filter.mask(&w).0),
        sensitive: objects::object_sensitive(obj),
    }
}

/// The accounts here an object mentions, by its `Mention` tags.
fn mentioned_here(base: &str, obj: &Value) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    let tags = match obj.get("tag") {
        Some(Value::Array(a)) => a.iter().collect(),
        Some(t) => vec![t],
        None => Vec::new(),
    };
    for t in tags {
        if kind(t) != "Mention" {
            continue;
        }
        if let Some(Local::Person(a)) = t
            .get("href")
            .and_then(Value::as_str)
            .and_then(|h| local_actor(base, h))
        {
            if !out.contains(&a) {
                out.push(a);
            }
        }
    }
    out
}

/// Whether `source` brought the post `object` into a community here.
fn brought_by(conn: &Connection, object: &str, source: &str) -> anyhow::Result<bool> {
    Ok(posts::rows_by_ap_id(conn, object)?
        .iter()
        .any(|r| r.via_actor.as_deref() == Some(source)))
}

fn strip_context(mut v: Value) -> Value {
    if let Some(m) = v.as_object_mut() {
        m.remove("@context");
    }
    v
}

/// Copy a picture from another server into the picture store, so that no
/// reader's device ever asks that server for it. Answers its hash.
fn copy_picture(ctx: &Ctx<'_>, url: &str) -> anyhow::Result<String> {
    ctx.refuse_blocked(url)?;
    let max = cartopolis_core::picture_limits::MAX_PICTURE_BYTES as u64;
    let (bytes, _) = ctx.client.get(url, "image/*", None, max)?;
    let format = image::guess_format(&bytes).context("not a picture")?;
    if !matches!(
        format,
        image::ImageFormat::Jpeg
            | image::ImageFormat::Png
            | image::ImageFormat::WebP
            | image::ImageFormat::Gif
    ) {
        anyhow::bail!("a {format:?} picture");
    }
    // Read the size from the header only: a small file can still be an
    // enormous picture, and nothing here decodes one.
    let (w, h) = image::ImageReader::with_format(std::io::Cursor::new(&bytes), format)
        .into_dimensions()
        .context("an unreadable picture")?;
    if w == 0 || h == 0 || w > 8192 || h > 8192 {
        anyhow::bail!("a {w}x{h} picture");
    }
    let hash = crate::assets::content_hash(&bytes);
    crate::pictures::store(ctx.asset_dir, &hash, &bytes)?;
    Ok(hash)
}

fn on_update(ctx: &Ctx<'_>, sender: &Actor, act: &Value) -> anyhow::Result<()> {
    let Some(object) = act.get("object") else {
        return Ok(());
    };
    // A profile changed: forget the cached copy so the next use fetches it.
    if id_of(object).as_deref() == Some(sender.id.as_str()) {
        ctx.db(|c| {
            c.execute(
                "UPDATE fedi_actors SET fetched_ms = 0 WHERE id = ?1",
                params![sender.id],
            )?;
            Ok(())
        })?;
        return Ok(());
    }
    let obj = authoritative_object(ctx, sender, object)?;
    if !is_post(&obj)
        || obj.get("attributedTo").and_then(id_of).as_deref() != Some(sender.id.as_str())
    {
        return Ok(());
    }
    let ap_id = id_of(&obj).unwrap_or_default();
    let content = read_content(ctx, &obj);
    ctx.db(|c| {
        Ok(posts::update_remote(
            c,
            &ap_id,
            &sender.id,
            &content,
            super::now_ms(),
        )?)
    })?;
    Ok(())
}

fn on_delete(ctx: &Ctx<'_>, sender: &Actor, act: &Value) -> anyhow::Result<()> {
    let Some(object) = act.get("object").and_then(id_of) else {
        return Ok(());
    };
    ctx.db(|c| {
        if object == sender.id {
            posts::forget_actor(c, &sender.id)?;
            c.execute(
                "DELETE FROM community_follows WHERE actor = ?1",
                params![sender.id],
            )?;
            c.execute("DELETE FROM fedi_actors WHERE id = ?1", params![sender.id])?;
            tracing::info!(actor = %sender.id, "fediverse: account deleted at home, forgotten");
        } else {
            posts::delete_remote(c, &object, &sender.id)?;
        }
        Ok(())
    })
}

fn on_announce(ctx: &Ctx<'_>, sender: &Actor, act: &Value) -> anyhow::Result<()> {
    // Somebody elsewhere passing on a post written here: counted, and its
    // author told. Nothing else about it concerns this server.
    if let Some(post) = act
        .get("object")
        .and_then(id_of)
        .filter(|o| super::local_post(&ctx.fed.base, o).is_some())
    {
        let activity = id_of(act).unwrap_or_default();
        return ctx.db(|c| {
            let now = super::now_ms();
            if posts::boost(c, &post, &sender.id, &activity, true, now)? {
                store_actor(c, sender, now)?;
                notices::tell_author(c, &post, NoticeKind::Boost, Who::Elsewhere(&sender.id), now)?;
            }
            Ok(())
        });
    }
    let followed = ctx.db(|c| {
        Ok(c.query_row(
            "SELECT 1 FROM community_follows WHERE actor = ?1 AND accepted = 1 LIMIT 1",
            params![sender.id],
            |_| Ok(()),
        )
        .optional()?
        .is_some())
    })?;
    if !followed {
        return Ok(());
    }
    let Some(inner) = act.get("object") else {
        return Ok(());
    };
    match kind(inner) {
        // Lemmy's way: the group boosts the activity itself.
        "Create" => {
            let Some(object) = inner.get("object") else {
                return Ok(());
            };
            let obj = authoritative_object(ctx, sender, object)?;
            if !is_post(&obj) {
                return Ok(());
            }
            let author_id = obj.get("attributedTo").and_then(id_of).unwrap_or_default();
            let author = resolve_actor(ctx, &author_id, false)?;
            accept_object(ctx, &obj, &author, Some(sender), None)
        }
        "Update" => {
            let Some(id) = inner.get("object").and_then(id_of) else {
                return Ok(());
            };
            let obj = ctx.fetch(&id)?;
            let author = obj.get("attributedTo").and_then(id_of).unwrap_or_default();
            let content = read_content(ctx, &obj);
            ctx.db(|c| {
                Ok(posts::update_remote(
                    c,
                    &id,
                    &author,
                    &content,
                    super::now_ms(),
                )?)
            })?;
            Ok(())
        }
        // A deletion or a moderator's removal: believe it only once the object
        // is gone at its origin.
        "Delete" | "Remove" => {
            let Some(id) = inner.get("object").and_then(id_of) else {
                return Ok(());
            };
            let gone = match ctx.fetch(&id) {
                Err(e) => matches!(e.status(), Some(404 | 410)),
                // Some servers answer a removed object with its tombstone.
                Ok(doc) => kind(&doc) == "Tombstone",
            };
            if gone {
                ctx.db(|c| {
                    for row in posts::rows_by_ap_id(c, &id)? {
                        if let Some(author) = &row.author_actor {
                            posts::delete_remote(c, &id, author)?;
                        }
                    }
                    Ok(())
                })?;
            }
            Ok(())
        }
        // Lemmy's community passes its members' votes on. The vote is the
        // voter's, vouched for by the community that brought the post; it
        // counts only on a post that community brought.
        "Like" | "Dislike" => {
            let (Some(voter), Some(activity), Some(object)) = (
                inner.get("actor").and_then(id_of),
                id_of(inner),
                inner.get("object").and_then(id_of),
            ) else {
                return Ok(());
            };
            let value = if kind(inner) == "Like" { 1 } else { -1 };
            ctx.db(|c| {
                if brought_by(c, &object, &sender.id)? {
                    posts::vote(c, &object, &voter, &activity, value, super::now_ms())?;
                }
                Ok(())
            })
        }
        "Undo" => {
            let Some(vote) = inner
                .get("object")
                .filter(|v| matches!(kind(v), "Like" | "Dislike"))
            else {
                return Ok(());
            };
            let Some(voter) = inner.get("actor").and_then(id_of) else {
                return Ok(());
            };
            let object = vote.get("object").and_then(id_of);
            ctx.db(|c| {
                if object
                    .as_deref()
                    .is_some_and(|o| brought_by(c, o, &sender.id).unwrap_or(false))
                {
                    posts::unvote(c, &voter, id_of(vote).as_deref(), object.as_deref())?;
                }
                Ok(())
            })
        }
        // Mastodon's way: a boost of a bare object (or its id).
        _ if inner.is_string() || is_post(inner) => {
            let obj = authoritative_object(ctx, sender, inner)?;
            if !is_post(&obj) || obj.get("inReplyTo").and_then(id_of).is_some() {
                return Ok(());
            }
            let author_id = obj.get("attributedTo").and_then(id_of).unwrap_or_default();
            let author = if author_id == sender.id {
                sender.clone()
            } else {
                resolve_actor(ctx, &author_id, false)?
            };
            accept_object(ctx, &obj, &author, Some(sender), None)
        }
        _ => Ok(()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use cartopolis_core::posts::{PublicSettings, WhoPosts};
    use cartopolis_core::{CommunitySettings, JoinRule};
    use serde_json::json;

    const BASE: &str = "https://cartopolis.test";

    /// A server with one public community, and no way out: nothing here may
    /// fetch, so every activity below carries all it needs.
    struct World {
        conn: Mutex<Connection>,
        fed: Federation,
        filter: Filter,
        community: Uuid,
        owner: String,
    }

    impl World {
        fn new() -> Self {
            let conn = crate::db::open_and_migrate(":memory:").unwrap();
            let owner = crate::auth::create_account(&conn, "karim", "pw")
                .unwrap()
                .to_string();
            let settings = CommunitySettings {
                name: "Oosterpoort".into(),
                about: String::new(),
                join: JoinRule::Open,
                listed: true,
                members_start_topics: true,
                picture: None,
                place: None,
            };
            let community = crate::communities::create(&conn, &owner, &settings, 1).unwrap();
            posts::set_public(
                &conn,
                &owner,
                community,
                &PublicSettings {
                    enabled: true,
                    handle: Some("oosterpoort".into()),
                    who_posts: WhoPosts::Members,
                },
                2,
            )
            .unwrap();
            let config = crate::config::SimConfig {
                public_base_url: BASE.into(),
                fediverse: true,
                ..Default::default()
            };
            Self {
                conn: Mutex::new(conn),
                fed: Federation::from_config(&config),
                filter: Filter::default(),
                community,
                owner,
            }
        }

        fn ctx(&self) -> Ctx<'_> {
            Ctx {
                conn: &self.conn,
                client: &self.fed.client,
                fed: &self.fed,
                filter: &self.filter,
                asset_dir: "",
            }
        }

        fn c(&self) -> std::sync::MutexGuard<'_, Connection> {
            self.conn.lock().unwrap()
        }

        /// A remote actor, as if fetched.
        fn actor(&self, id: &str) -> Actor {
            let host = host_of(id).unwrap();
            let a = Actor {
                id: id.into(),
                kind: "Person".into(),
                username: id.rsplit('/').next().unwrap().into(),
                host: host.clone(),
                name: String::new(),
                inbox: format!("{id}/inbox"),
                shared_inbox: Some(format!("https://{host}/inbox")),
                key_id: format!("{id}#main-key"),
                public_pem: String::new(),
                url: None,
            };
            store_actor(&self.c(), &a, 1).unwrap();
            a
        }

        /// A post from elsewhere, brought by `via` when given.
        fn remote_post(&self, ap_id: &str, author: &Actor, via: Option<&Actor>) {
            posts::store_remote(
                &self.c(),
                self.community,
                &Remote {
                    ap_id: ap_id.into(),
                    author_actor: author.id.clone(),
                    via_actor: via.map(|v| v.id.clone()),
                    text: "a heron on the quay".into(),
                    picture: None,
                    place: None,
                    url: None,
                    created_ms: 3,
                    parent: None,
                    in_reply_to: None,
                    warning: None,
                    sensitive: false,
                    picture_alt: None,
                    more_pictures: Vec::new(),
                },
            )
            .unwrap()
            .unwrap();
        }

        fn feed_len(&self) -> usize {
            posts::feed(&self.c(), self.community, None, None, "cartopolis.test")
                .unwrap()
                .len()
        }

        fn likes(&self, ap_id: &str) -> u32 {
            let c = self.c();
            let row = posts::rows_by_ap_id(&c, ap_id).unwrap().remove(0);
            posts::dto(&c, &row, None, "cartopolis.test").unwrap().likes
        }
    }

    #[test]
    fn an_undo_by_bare_id_ends_that_follow_and_no_other() {
        let w = World::new();
        let anna = w.actor("https://m.test/users/anna");
        let group = posts::group_local(w.community);
        let person = posts::person_local("someone");
        for (local, follow) in [
            (&group, "https://m.test/f/1"),
            (&person, "https://m.test/f/2"),
        ] {
            w.c()
                .execute(
                    "INSERT INTO fedi_followers (local, actor, followed_ms, follow_id)
                     VALUES (?1, ?2, 0, ?3)",
                    params![local, anna.id, follow],
                )
                .unwrap();
        }
        let followers = || -> i64 {
            w.c()
                .query_row("SELECT COUNT(*) FROM fedi_followers", [], |r| r.get(0))
                .unwrap()
        };
        // A boost taken back by its id is no follow of anything.
        let undo = |object: &str| json!({"type": "Undo", "actor": anna.id, "object": object});
        handle(&w.ctx(), &anna, &undo("https://m.test/statuses/9/activity")).unwrap();
        assert_eq!(followers(), 2);
        handle(&w.ctx(), &anna, &undo("https://m.test/f/1")).unwrap();
        assert_eq!(followers(), 1);
        let left: String = w
            .c()
            .query_row("SELECT local FROM fedi_followers", [], |r| r.get(0))
            .unwrap();
        assert_eq!(left, person);
    }

    #[test]
    fn a_followed_lemmy_community_passes_its_votes_on() {
        let w = World::new();
        let lemmy = w.actor("https://l.test/c/groningen");
        let author = w.actor("https://l.test/u/piet");
        let other = w.actor("https://m.test/users/anna");
        w.c()
            .execute(
                "INSERT INTO community_follows (community, actor, follow_id, accepted, created_by, created_ms)
                 VALUES (?1, ?2, 'f', 1, 'x', 0)",
                params![w.community.to_string(), lemmy.id],
            )
            .unwrap();
        w.remote_post("https://l.test/post/1", &author, Some(&lemmy));
        w.remote_post("https://m.test/statuses/1", &other, None);
        let vote = |id: &str, object: &str| {
            json!({
                "type": "Announce",
                "actor": lemmy.id,
                "object": {
                    "type": "Like",
                    "id": id,
                    "actor": "https://l.test/u/voter",
                    "object": object,
                },
            })
        };
        handle(
            &w.ctx(),
            &lemmy,
            &vote("https://l.test/like/1", "https://l.test/post/1"),
        )
        .unwrap();
        assert_eq!(w.likes("https://l.test/post/1"), 1);
        // Not its post to vouch for.
        handle(
            &w.ctx(),
            &lemmy,
            &vote("https://l.test/like/2", "https://m.test/statuses/1"),
        )
        .unwrap();
        assert_eq!(w.likes("https://m.test/statuses/1"), 0);
        let undo = json!({
            "type": "Announce",
            "actor": lemmy.id,
            "object": {
                "type": "Undo",
                "actor": "https://l.test/u/voter",
                "object": {
                    "type": "Like",
                    "id": "https://l.test/like/1",
                    "actor": "https://l.test/u/voter",
                    "object": "https://l.test/post/1",
                },
            },
        });
        handle(&w.ctx(), &lemmy, &undo).unwrap();
        assert_eq!(w.likes("https://l.test/post/1"), 0);
    }

    #[test]
    fn a_blocked_server_is_not_heard_through_another() {
        let w = World::new();
        let relay = w.actor("https://relay.test/actor");
        let spammer = w.actor("https://spam.test/users/deals");
        w.c()
            .execute(
                "INSERT INTO community_follows (community, actor, follow_id, accepted, created_by, created_ms)
                 VALUES (?1, ?2, 'f', 1, 'x', 0)",
                params![w.community.to_string(), relay.id],
            )
            .unwrap();
        blocklist::block(&w.c(), "spam.test", "", 1).unwrap();
        let note = json!({
            "type": "Note",
            "id": "https://spam.test/statuses/1",
            "attributedTo": spammer.id,
            "to": [objects::PUBLIC],
            "content": "best phones",
        });
        accept_object(&w.ctx(), &note, &spammer, Some(&relay), None).unwrap();
        assert_eq!(w.feed_len(), 0);
        // Nor is anything asked of it.
        assert!(matches!(
            w.ctx().fetch("https://media.spam.test/x"),
            Err(FetchError::Refused(..))
        ));
    }

    #[test]
    fn blocking_a_server_hides_what_came_from_it_and_cuts_its_ties() {
        let w = World::new();
        let anna = w.actor("https://m.test/users/anna");
        w.remote_post("https://m.test/statuses/1", &anna, None);
        w.c()
            .execute(
                "INSERT INTO fedi_followers (local, actor, followed_ms) VALUES ('c:x', ?1, 0)",
                params![anna.id],
            )
            .unwrap();
        delivery::enqueue(&w.c(), "https://m.test/inbox", "instance", &json!({}), 1).unwrap();
        assert_eq!(w.feed_len(), 1);
        blocklist::block(&w.c(), "m.test", "", 2).unwrap();
        assert_eq!(w.feed_len(), 0);
        let count = |t: &str| -> i64 {
            w.c()
                .query_row(&format!("SELECT COUNT(*) FROM {t}"), [], |r| r.get(0))
                .unwrap()
        };
        assert_eq!(count("fedi_followers"), 0);
        assert_eq!(count("fedi_deliveries"), 0);
        // Kept, so an unblock brings it back.
        blocklist::unblock(&w.c(), "m.test").unwrap();
        assert_eq!(w.feed_len(), 1);
    }

    #[test]
    fn the_author_here_is_told_of_replies_mentions_likes_and_boosts_from_elsewhere() {
        let w = World::new();
        let anna = w.actor("https://m.test/users/anna");
        let post = posts::create_post(
            &w.c(),
            &w.owner,
            w.community,
            &cartopolis_core::posts::NewPost {
                text: "Plant swap".into(),
                picture: None,
                snapshot: None,
                place: cartopolis_core::posts::PostPlace {
                    latitude: 53.2,
                    longitude: 6.6,
                    name: None,
                    view: None,
                },
                warning: None,
                sensitive: false,
                picture_alt: None,
            },
            BASE,
            1,
        )
        .unwrap();
        let post_id = super::super::post_url(BASE, post);
        let me = super::super::person_url(BASE, &w.owner);
        let reply = json!({
            "type": "Create",
            "actor": anna.id,
            "object": {
                "type": "Note",
                "id": "https://m.test/statuses/2",
                "attributedTo": anna.id,
                "inReplyTo": post_id,
                "to": [objects::PUBLIC],
                "content": "<p>I'll bring cuttings</p>",
                "tag": [{"type": "Mention", "href": me}],
            },
        });
        handle(&w.ctx(), &anna, &reply).unwrap();
        let like = json!({"type": "Like", "id": "https://m.test/l/1", "actor": anna.id, "object": post_id});
        handle(&w.ctx(), &anna, &like).unwrap();
        handle(&w.ctx(), &anna, &like).unwrap();
        let boost = json!({"type": "Announce", "id": "https://m.test/b/1", "actor": anna.id, "object": post_id});
        handle(&w.ctx(), &anna, &boost).unwrap();
        let told = notices::list(&w.c(), &w.owner, "cartopolis.test").unwrap();
        let mut kinds: Vec<NoticeKind> = told.items.iter().map(|n| n.kind).collect();
        kinds.sort_by_key(|k| format!("{k:?}"));
        assert_eq!(
            kinds,
            [
                NoticeKind::Boost,
                NoticeKind::Like,
                NoticeKind::Mention,
                NoticeKind::Reply
            ],
            "a like sent twice tells once"
        );
        assert_eq!(told.unread, 4);
        assert!(told.items.iter().all(|n| n.who.address == "anna@m.test"));
        let c = w.c();
        let row = posts::row(&c, post).unwrap().unwrap();
        let dto = posts::dto(&c, &row, None, "cartopolis.test").unwrap();
        assert_eq!((dto.likes, dto.boosts, dto.replies), (1, 1, 1));
        drop(c);
        let undo = json!({"type": "Undo", "actor": anna.id, "object": boost});
        handle(&w.ctx(), &anna, &undo).unwrap();
        let c = w.c();
        assert_eq!(
            posts::dto(&c, &row, None, "cartopolis.test")
                .unwrap()
                .boosts,
            0
        );
        notices::mark_read(&c, &w.owner, None).unwrap();
        assert_eq!(
            notices::list(&c, &w.owner, "cartopolis.test")
                .unwrap()
                .unread,
            0
        );
    }
}
