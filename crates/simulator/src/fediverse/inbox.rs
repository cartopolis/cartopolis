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
use rusqlite::{params, Connection, OptionalExtension};
use serde_json::Value;
use uuid::Uuid;

use super::fetch::{host_of, Client, FetchError};
use super::objects::{self, addressees, id_of, kind, Actor};
use super::posts::{self, Remote};
use super::{delivery, local_actor, servers, signing, Federation, Local};

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

    /// GET an ActivityPub document, signed as this server.
    pub fn fetch(&self, url: &str) -> Result<Value, FetchError> {
        let signer = self
            .instance_signer()
            .map_err(|e| FetchError::Transport(url.into(), e.to_string()))?;
        self.client.get_json(url, Some((&signer.0, &signer.1)))
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
    ctx.db(|conn| {
        conn.execute(
            "INSERT OR IGNORE INTO fedi_followers (local, actor, followed_ms) VALUES (?1, ?2, ?3)",
            params![local, sender.id, now],
        )?;
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
            // Given only by its id: a vote's, if it was one, and otherwise a
            // follow's, of whatever it followed.
            _ if !inner.is_object() => {
                let id = id_of(inner).unwrap_or_default();
                if posts::unvote(conn, &sender.id, Some(&id), None)? == 0 {
                    conn.execute(
                        "DELETE FROM fedi_followers WHERE actor = ?1",
                        params![sender.id],
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
    let (text, _) = ctx.filter.mask(&objects::object_text(obj));
    let picture = objects::image_url(obj).and_then(|url| match copy_picture(ctx, &url) {
        Ok(hash) => Some(hash),
        Err(e) => {
            tracing::info!(%url, "fediverse: picture not copied: {e:#}");
            None
        }
    });
    if text.is_empty() && picture.is_none() {
        return Ok(());
    }
    let now = super::now_ms();
    let place = objects::object_place(obj);
    let url = obj
        .get("url")
        .and_then(id_of)
        .filter(|u| super::place_from_link(u).is_none())
        .or_else(|| Some(ap_id.clone()));
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
            };
            match posts::store_remote(conn, community, &remote) {
                Ok(id) => Ok(id),
                Err(posts::Refusal::PostsOff) => Ok(None),
                Err(e) => Err(anyhow::anyhow!(e)),
            }
        })?;
        if stored.is_some() {
            tracing::info!(%ap_id, %community, author = %author.id, "fediverse: post arrived");
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
        Ok(posts::vote(
            c,
            &object,
            &sender.id,
            &activity,
            value,
            super::now_ms(),
        )?)
    })?;
    Ok(())
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

fn strip_context(mut v: Value) -> Value {
    if let Some(m) = v.as_object_mut() {
        m.remove("@context");
    }
    v
}

/// Copy a picture from another server into the picture store, so that no
/// reader's device ever asks that server for it. Answers its hash.
fn copy_picture(ctx: &Ctx<'_>, url: &str) -> anyhow::Result<String> {
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
    let (text, _) = ctx.filter.mask(&objects::object_text(&obj));
    ctx.db(|c| {
        Ok(posts::update_remote(
            c,
            &ap_id,
            &sender.id,
            &text,
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
            let (text, _) = ctx.filter.mask(&objects::object_text(&obj));
            ctx.db(|c| {
                Ok(posts::update_remote(
                    c,
                    &id,
                    &author,
                    &text,
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
            match ctx.fetch(&id) {
                Err(e) if matches!(e.status(), Some(404 | 410)) => {
                    ctx.db(|c| {
                        for row in posts::rows_by_ap_id(c, &id)? {
                            if let Some(author) = &row.author_actor {
                                posts::delete_remote(c, &id, author)?;
                            }
                        }
                        Ok(())
                    })?;
                }
                _ => {}
            }
            Ok(())
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
