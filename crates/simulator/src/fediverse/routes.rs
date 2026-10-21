//! The HTTP surface of public posts: what other fediverse servers ask for, and
//! what the app and the web page use.
//!
//! **For other servers:** WebFinger, host-meta and NodeInfo (how an address is
//! found and what software answers it), the actors, a community's outbox and
//! followers, each post written here, the pictures of public posts, and the
//! inboxes. Caddy must route `/.well-known/webfinger`, `/.well-known/host-meta`,
//! `/.well-known/nodeinfo`, `/nodeinfo/` and `/ap/` here as it does `/api/`
//! (`docs/notes/fediverse.md`).
//!
//! **For the apps:** `/api/communities/{id}/public` (by id or by handle),
//! `/posts`, `/follows`, `/blocks` and `/reports`, `/api/posts` (a person's)
//! and `/api/posts/{id}`, `/api/fediverse/follow-link`, and the operator's
//! `/api/admin/fediverse/blocks`. HTTP rather than the wire, for the reason
//! `cartopolis_core::posts` gives.

use std::sync::Arc;

use axum::body::Bytes;
use axum::extract::{Path, Query, State};
use axum::http::{HeaderMap, StatusCode, Uri};
use axum::response::{IntoResponse, Redirect, Response};
use axum::routing::{delete, get, post};
use axum::{Json, Router};
use cartopolis_core::permissions::Permission;
use cartopolis_core::posts::{
    EditPost, FollowLink, NewPost, NewReply, PersonPosts, PostDto, PostReport, PublicInfo,
    PublicSettings, ReportAction,
};
use rusqlite::{params, Connection, OptionalExtension};
use serde::Deserialize;
use serde_json::{json, Value};
use uuid::Uuid;

use super::inbox::{self, Ctx};
use super::objects::{self, id_of, Mention, NoteExtras, ReplyTo};
use super::posts::{self, PostRow, Refusal};
use super::{
    blocklist, delivery, group_url, person_url, picture_facts, reader_url, servers, signing,
    Federation, READER_SUFFIX,
};
use crate::http::{internal, require, require_session, with_db, ApiError, ApiResult, HttpState};

/// Largest activity accepted at an inbox.
const MAX_ACTIVITY_BYTES: usize = 1 << 20;

/// Posts a community's outbox lists.
const OUTBOX_ITEMS: i64 = 20;

/// Accounts elsewhere one post may mention: each is a WebFinger lookup and a
/// fetch before the post is saved, and a delivery after.
const MAX_MENTIONS: usize = 10;

pub fn routes() -> Router<HttpState> {
    let inbox_limit = axum::extract::DefaultBodyLimit::max(MAX_ACTIVITY_BYTES);
    Router::new()
        .route("/.well-known/webfinger", get(webfinger))
        .route("/.well-known/host-meta", get(host_meta))
        .route("/.well-known/nodeinfo", get(nodeinfo_links))
        .route("/nodeinfo/2.1", get(nodeinfo))
        .route("/ap/actor", get(instance_actor))
        .route("/ap/inbox", post(shared_inbox).layer(inbox_limit))
        .route("/ap/u/{id}", get(person_actor))
        .route("/ap/u/{id}/outbox", get(empty_outbox))
        .route("/ap/u/{id}/followers", get(person_followers))
        .route("/ap/u/{id}/inbox", post(shared_inbox).layer(inbox_limit))
        .route("/ap/c/{id}", get(group_actor))
        .route("/ap/c/{id}/outbox", get(group_outbox))
        .route("/ap/c/{id}/followers", get(group_followers))
        .route("/ap/c/{id}/moderators", get(group_moderators))
        .route("/ap/c/{id}/inbox", post(shared_inbox).layer(inbox_limit))
        .route("/ap/c/{id}/reader", get(reader_actor))
        .route("/ap/c/{id}/reader/outbox", get(reader_outbox))
        .route(
            "/ap/c/{id}/reader/inbox",
            post(shared_inbox).layer(inbox_limit),
        )
        .route("/ap/p/{id}", get(post_object))
        .route("/ap/media/{hash}", get(media))
        .route(
            "/api/communities/{id}/public",
            // POST as well as PUT: the app's HTTP layer has no PUT.
            get(public_info).put(set_public).post(set_public),
        )
        .route("/api/communities/{id}/posts", get(feed).post(create_post))
        .route("/api/communities/{id}/follows", post(follow_source))
        .route(
            "/api/communities/{id}/follows/{address}",
            delete(unfollow_source),
        )
        .route("/api/communities/{id}/blocks", post(block_author))
        .route("/api/communities/{id}/reports", get(list_reports))
        .route(
            "/api/communities/{id}/reports/{report}",
            post(resolve_report),
        )
        .route("/api/posts", get(person_posts))
        .route(
            "/api/posts/{id}",
            // POST as well as PUT to edit: the app's HTTP layer has no PUT.
            get(thread)
                .delete(delete_post)
                .put(edit_post)
                .post(edit_post),
        )
        .route("/api/fediverse/follow-link", get(follow_link))
        .route("/api/posts/{id}/replies", post(reply))
        .route("/api/posts/{id}/hide", post(hide_post))
        .route(
            "/api/admin/fediverse/blocks",
            get(list_server_blocks).post(add_server_block),
        )
        .route(
            "/api/admin/fediverse/blocks/{host}",
            delete(remove_server_block),
        )
}

// ── helpers ─────────────────────────────────────────────────────────────────

/// The JSON every ActivityPub document is served as.
fn activity_json(v: Value) -> Response {
    (
        [("content-type", "application/activity+json; charset=utf-8")],
        v.to_string(),
    )
        .into_response()
}

fn not_found() -> Response {
    (StatusCode::NOT_FOUND, "not found").into_response()
}

/// Whether a request asks for ActivityPub rather than a page.
fn wants_activity(headers: &HeaderMap) -> bool {
    headers
        .get(axum::http::header::ACCEPT)
        .and_then(|v| v.to_str().ok())
        .is_none_or(|a| a.contains("json") || a.contains("*/*") && !a.contains("text/html"))
}

/// The signed-in caller, if the request carries a valid session; nobody
/// otherwise. For routes anybody may read.
fn optional_session(state: &HttpState, headers: &HeaderMap) -> Option<(Uuid, String)> {
    headers.get(axum::http::header::AUTHORIZATION)?;
    require_session(state, headers).ok()
}

fn refused(e: Refusal) -> ApiError {
    let status = match e {
        Refusal::NoSuchCommunity
        | Refusal::NoSuchPost
        | Refusal::NoSuchReport
        | Refusal::NoSuchPerson => StatusCode::NOT_FOUND,
        Refusal::NotAModerator
        | Refusal::NotAMember
        | Refusal::ModeratorsOnly
        | Refusal::NotYours => StatusCode::FORBIDDEN,
        Refusal::HandleTaken | Refusal::HandleFixed => StatusCode::CONFLICT,
        Refusal::Storage(ref s) => {
            tracing::warn!("posts: {s}");
            return ApiError(
                StatusCode::INTERNAL_SERVER_ERROR,
                "the server could not complete that".into(),
            );
        }
        _ => StatusCode::BAD_REQUEST,
    };
    ApiError(status, e.to_string())
}

/// Run `f` on the database, a `Refusal` answered as the client should see it.
fn db<T>(state: &HttpState, f: impl FnOnce(&Connection) -> Result<T, Refusal>) -> ApiResult<T> {
    let mut out = None;
    with_db(state, |c| {
        out = Some(f(c));
        Ok(())
    })?;
    out.expect("ran").map_err(refused)
}

fn parse_id(id: &str) -> Result<Uuid, Response> {
    Uuid::parse_str(id).map_err(|_| not_found())
}

/// Whether `account` is somebody this server speaks for on the fediverse: they
/// have written a public post, or they moderate a community with posts on.
/// Everybody else is not findable from outside.
fn is_public_person(conn: &Connection, account: &str) -> rusqlite::Result<bool> {
    conn.query_row(
        "SELECT EXISTS (SELECT 1 FROM posts WHERE author_account = ?1)
             OR EXISTS (SELECT 1 FROM community_members m
                        JOIN community_public p ON p.community = m.community
                        WHERE m.account = ?1 AND m.role < 2 AND p.enabled = 1)",
        params![account],
        |r| r.get(0),
    )
}

// ── finding things ──────────────────────────────────────────────────────────

#[derive(Deserialize)]
struct WebfingerQuery {
    resource: String,
}

async fn webfinger(State(state): State<HttpState>, Query(q): Query<WebfingerQuery>) -> Response {
    let fed = state.fedi.clone();
    let resource = q.resource.trim();
    let acct = resource.strip_prefix("acct:").unwrap_or(resource);
    let Some((name, host)) = acct.trim_start_matches('@').rsplit_once('@') else {
        return not_found();
    };
    if !host.eq_ignore_ascii_case(&fed.server) && !acct.ends_with(&host_with_port(&fed)) {
        return not_found();
    }
    let name = name.to_ascii_lowercase();
    // This server itself, as Mastodon names its own: `acct:<server>@<server>`.
    // Mastodon resolves the signer of a request this way before it believes the
    // signature, so without it every signed fetch from here was answered 401 by
    // a server in authorized-fetch mode (mastodon.social, 2026-10-13), and no
    // follow from there could be answered.
    let instance = name == fed.server || name == host_with_port(&fed);
    let found = with_db(&state, |c| {
        if instance {
            return Ok(Some((super::instance_url(&fed.base), fed.base.clone())));
        }
        if let Some(community) =
            posts::community_by_handle(c, &name).map_err(|e| anyhow::anyhow!(e))?
        {
            return Ok(Some((
                group_url(&fed.base, community),
                objects::community_page(&fed.base, &name),
            )));
        }
        // A community's reader, which Mastodon looks up by its name before it
        // believes a request the reader signed, as it does the server's own.
        if let Some(handle) = name.strip_suffix(READER_SUFFIX) {
            if let Some(community) =
                posts::community_by_handle(c, handle).map_err(|e| anyhow::anyhow!(e))?
            {
                return Ok(Some((
                    reader_url(&fed.base, community),
                    objects::community_page(&fed.base, handle),
                )));
            }
        }
        let account: Option<(String, String)> = c
            .query_row(
                "SELECT id, username FROM accounts WHERE lower(username) = ?1 AND disabled = 0",
                params![name],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .optional()?;
        match account {
            Some((a, u)) if is_public_person(c, &a)? => Ok(Some((
                person_url(&fed.base, &a),
                objects::person_page(&fed.base, &u),
            ))),
            _ => Ok(None),
        }
    });
    let Ok(Some((actor, page))) = found else {
        return not_found();
    };
    (
        [("content-type", "application/jrd+json; charset=utf-8")],
        json!({
            "subject": format!("acct:{name}@{}", fed.server),
            "aliases": [actor],
            "links": [
                { "rel": "self", "type": "application/activity+json", "href": actor },
                { "rel": "http://webfinger.net/rel/profile-page", "type": "text/html", "href": page },
            ],
        })
        .to_string(),
    )
        .into_response()
}

/// `host:port`, for a test server whose address has one.
fn host_with_port(fed: &Federation) -> String {
    fed.base
        .split_once("://")
        .map_or(fed.base.as_str(), |(_, r)| r)
        .to_string()
}

/// Where WebFinger is, for the servers that look here first (Friendica, older
/// Pleroma, some clients) rather than going straight to it.
async fn host_meta(State(state): State<HttpState>) -> Response {
    let body = format!(
        "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n\
         <XRD xmlns=\"http://docs.oasis-open.org/ns/xri/xrd-1.0\">\n  \
         <Link rel=\"lrdd\" template=\"{}/.well-known/webfinger?resource={{uri}}\"/>\n\
         </XRD>\n",
        objects::escape_html(&state.fedi.base)
    );
    (
        [("content-type", "application/xrd+xml; charset=utf-8")],
        body,
    )
        .into_response()
}

async fn nodeinfo_links(State(state): State<HttpState>) -> Response {
    Json(json!({
        "links": [{
            "rel": "http://nodeinfo.diaspora.software/ns/schema/2.1",
            "href": format!("{}/nodeinfo/2.1", state.fedi.base),
        }]
    }))
    .into_response()
}

async fn nodeinfo(State(state): State<HttpState>) -> Response {
    let counts = with_db(&state, |c| {
        let users: i64 = c.query_row(
            "SELECT COUNT(*) FROM accounts WHERE disabled = 0",
            [],
            |r| r.get(0),
        )?;
        let posts: i64 = c.query_row(
            "SELECT COUNT(*) FROM posts WHERE author_account IS NOT NULL AND parent IS NULL",
            [],
            |r| r.get(0),
        )?;
        let replies: i64 = c.query_row(
            "SELECT COUNT(*) FROM posts WHERE author_account IS NOT NULL AND parent IS NOT NULL",
            [],
            |r| r.get(0),
        )?;
        Ok((users, posts, replies))
    });
    let (users, local_posts, local_comments) = counts.unwrap_or((0, 0, 0));
    // People connected in the last 30 days, which server directories list. Not
    // published below `MIN_PEOPLE`, the usage numbers' own floor.
    let month = crate::stats::now_s() - 30 * 86_400;
    let mut users_json = json!({ "total": users });
    if let Some(n) = state.stats.active_since(month) {
        if n >= crate::stats::MIN_PEOPLE {
            users_json["activeMonth"] = json!(n);
        }
    }
    Json(json!({
        "version": "2.1",
        "software": {
            "name": "cartopolis",
            "version": env!("CARGO_PKG_VERSION"),
            "homepage": "https://cartopolis.org",
        },
        "protocols": ["activitypub"],
        "services": { "inbound": [], "outbound": [] },
        "openRegistrations": state.config.registration_open,
        "usage": {
            "users": users_json,
            "localPosts": local_posts,
            "localComments": local_comments,
        },
        "metadata": {
            "nodeName": "Cartopolis",
            "nodeDescription": "A 3D map of the world where neighbourhood communities meet, talk and post where things are.",
        },
    }))
    .into_response()
}

// ── actors ──────────────────────────────────────────────────────────────────

async fn instance_actor(State(state): State<HttpState>) -> Response {
    let fed = state.fedi.clone();
    match with_db(&state, |c| {
        signing::public_pem(c, "instance", super::now_ms())
    }) {
        Ok(pem) => activity_json(objects::document(objects::instance(
            &fed.base,
            &fed.server,
            &pem,
        ))),
        Err(e) => e.into_response(),
    }
}

async fn person_actor(
    State(state): State<HttpState>,
    headers: HeaderMap,
    Path(id): Path<String>,
) -> Response {
    let id = match parse_id(&id) {
        Ok(id) => id.to_string(),
        Err(r) => return r,
    };
    let fed = state.fedi.clone();
    let wants = wants_activity(&headers);
    let found = with_db(&state, |c| {
        let row: Option<(String, Option<String>, String)> = c
            .query_row(
                "SELECT username, picture, created_at FROM accounts WHERE id = ?1 AND disabled = 0",
                params![id],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
            )
            .optional()?;
        let Some((username, picture, created_at)) = row else {
            return Ok(
                if posts::account_gone(c, &id).map_err(|e| anyhow::anyhow!(e))? {
                    PersonAnswer::Gone
                } else {
                    PersonAnswer::Unknown
                },
            );
        };
        if !is_public_person(c, &id)? {
            return Ok(PersonAnswer::Unknown);
        }
        if !wants {
            return Ok(PersonAnswer::Page(objects::person_page(
                &fed.base, &username,
            )));
        }
        let pem = signing::public_pem(c, &posts::person_local(&id), super::now_ms())?;
        let mut stmt = c.prepare(
            "SELECT DISTINCT m.name, p.handle FROM posts s
             JOIN community_public p ON p.community = s.community
             JOIN communities m ON m.id = s.community
             WHERE s.author_account = ?1 AND p.enabled = 1 ORDER BY m.name LIMIT 5",
        )?;
        let communities: Vec<(String, String)> = stmt
            .query_map(params![id], |r| Ok((r.get(0)?, r.get(1)?)))?
            .collect::<Result<_, _>>()?;
        let picture = picture.and_then(|h| picture_facts(&fed.asset_dir, &h).map(|f| (h, f)));
        // `created_at` is SQLite's `datetime('now')`: UTC, a space for the T.
        let created_ms =
            cartopolis_core::point::parse_rfc3339(&format!("{}Z", created_at.replace(' ', "T")))
                .and_then(|ms| i64::try_from(ms).ok());
        Ok(PersonAnswer::Actor(objects::person(
            &fed.base,
            &fed.server,
            &objects::PersonFacts {
                account: &id,
                username: &username,
                picture: picture.as_ref().map(|(h, f)| (h.as_str(), *f)),
                created_ms,
                communities: &communities,
                pem: &pem,
            },
        )))
    });
    match found {
        Ok(PersonAnswer::Actor(v)) => activity_json(objects::document(v)),
        Ok(PersonAnswer::Page(to)) => Redirect::to(&to).into_response(),
        Ok(PersonAnswer::Gone) => (
            StatusCode::GONE,
            [("content-type", "application/activity+json; charset=utf-8")],
            objects::document(json!({
                "id": person_url(&fed.base, &id),
                "type": "Tombstone",
                "formerType": "Person",
            }))
            .to_string(),
        )
            .into_response(),
        Ok(PersonAnswer::Unknown) => not_found(),
        Err(e) => e.into_response(),
    }
}

/// What a person's actor URL answers.
enum PersonAnswer {
    Actor(Value),
    /// A browser, sent to their page.
    Page(String),
    /// Deleted, and known elsewhere: 410, so other servers let go of them.
    Gone,
    Unknown,
}

/// What the community actor needs from the community.
fn group_document(
    conn: &Connection,
    fed: &Federation,
    community: Uuid,
) -> anyhow::Result<Option<Value>> {
    let Some(public) = posts::public(conn, community).map_err(|e| anyhow::anyhow!(e))? else {
        return Ok(None);
    };
    if !public.enabled {
        return Ok(None);
    }
    type Row = (
        String,
        String,
        Option<String>,
        Option<f64>,
        Option<f64>,
        Option<String>,
        i64,
    );
    let row: Option<Row> = conn
        .query_row(
            "SELECT name, about, picture, place_lat, place_lng, place_name, created_ms
             FROM communities WHERE id = ?1",
            params![community.to_string()],
            |r| {
                Ok((
                    r.get(0)?,
                    r.get(1)?,
                    r.get(2)?,
                    r.get(3)?,
                    r.get(4)?,
                    r.get(5)?,
                    r.get(6)?,
                ))
            },
        )
        .optional()?;
    let Some((name, about, picture, lat, lng, place_name, created_ms)) = row else {
        return Ok(None);
    };
    let pem = signing::public_pem(conn, &posts::group_local(community), super::now_ms())?;
    let picture = picture.and_then(|h| picture_facts(&fed.asset_dir, &h).map(|f| (h, f)));
    Ok(Some(objects::group(
        &fed.base,
        &objects::GroupFacts {
            community,
            handle: &public.handle,
            name: &name,
            about: &about,
            picture: picture.as_ref().map(|(h, f)| (h.as_str(), *f)),
            place: lat.zip(lng).map(|(a, b)| (a, b, place_name.as_deref())),
            who_posts: public.who_posts,
            pem: &pem,
            created_ms,
        },
    )))
}

async fn group_actor(
    State(state): State<HttpState>,
    headers: HeaderMap,
    Path(id): Path<String>,
) -> Response {
    let community = match parse_id(&id) {
        Ok(id) => id,
        Err(r) => return r,
    };
    let fed = state.fedi.clone();
    if !wants_activity(&headers) {
        let handle = with_db(&state, |c| {
            Ok(posts::public(c, community)
                .map_err(|e| anyhow::anyhow!(e))?
                .filter(|p| p.enabled)
                .map(|p| p.handle))
        });
        return match handle {
            Ok(Some(h)) => Redirect::to(&objects::community_page(&fed.base, &h)).into_response(),
            _ => not_found(),
        };
    }
    match with_db(&state, |c| group_document(c, &fed, community)) {
        Ok(Some(v)) => activity_json(objects::document(v)),
        Ok(None) => not_found(),
        Err(e) => e.into_response(),
    }
}

/// A community's reader (`super::reader_url`): the bot account its follows of
/// sources elsewhere are made as.
async fn reader_actor(State(state): State<HttpState>, Path(id): Path<String>) -> Response {
    let community = match parse_id(&id) {
        Ok(id) => id,
        Err(r) => return r,
    };
    let fed = state.fedi.clone();
    let found = with_db(&state, |c| {
        let Some(public) = posts::public(c, community)
            .map_err(|e| anyhow::anyhow!(e))?
            .filter(|p| p.enabled)
        else {
            return Ok(None);
        };
        let name: String = c.query_row(
            "SELECT name FROM communities WHERE id = ?1",
            params![community.to_string()],
            |r| r.get(0),
        )?;
        let pem = signing::public_pem(c, &posts::reader_local(community), super::now_ms())?;
        Ok(Some(objects::reader(
            &fed.base,
            community,
            &public.handle,
            &name,
            &pem,
        )))
    });
    match found {
        Ok(Some(v)) => activity_json(objects::document(v)),
        Ok(None) => not_found(),
        Err(e) => e.into_response(),
    }
}

async fn reader_outbox(State(state): State<HttpState>, Path(id): Path<String>) -> Response {
    let Ok(community) = Uuid::parse_str(&id) else {
        return not_found();
    };
    collection(
        &format!("{}/outbox", reader_url(&state.fedi.base, community)),
        0,
        Some(Vec::new()),
    )
}

fn collection(id: &str, total: i64, items: Option<Vec<Value>>) -> Response {
    let mut v = json!({ "id": id, "type": "OrderedCollection", "totalItems": total });
    if let Some(items) = items {
        v["orderedItems"] = Value::Array(items);
    }
    activity_json(objects::document(v))
}

async fn empty_outbox(State(state): State<HttpState>, Path(id): Path<String>) -> Response {
    collection(
        &format!("{}/outbox", person_url(&state.fedi.base, &id)),
        0,
        Some(Vec::new()),
    )
}

/// Who follows a local actor is nobody else's business; how many is.
fn follower_count(state: &HttpState, local: &str) -> i64 {
    with_db(state, |c| {
        Ok(c.query_row(
            "SELECT COUNT(*) FROM fedi_followers WHERE local = ?1",
            params![local],
            |r| r.get(0),
        )?)
    })
    .unwrap_or(0)
}

async fn person_followers(State(state): State<HttpState>, Path(id): Path<String>) -> Response {
    let Ok(id) = Uuid::parse_str(&id) else {
        return not_found();
    };
    let n = follower_count(&state, &posts::person_local(&id.to_string()));
    collection(
        &format!(
            "{}/followers",
            person_url(&state.fedi.base, &id.to_string())
        ),
        n,
        None,
    )
}

async fn group_followers(State(state): State<HttpState>, Path(id): Path<String>) -> Response {
    let Ok(id) = Uuid::parse_str(&id) else {
        return not_found();
    };
    let n = follower_count(&state, &posts::group_local(id));
    collection(
        &format!("{}/followers", group_url(&state.fedi.base, id)),
        n,
        None,
    )
}

async fn group_moderators(State(state): State<HttpState>, Path(id): Path<String>) -> Response {
    let Ok(community) = Uuid::parse_str(&id) else {
        return not_found();
    };
    let base = state.fedi.base.clone();
    let mods = with_db(&state, |c| {
        let mut stmt = c.prepare(
            "SELECT account FROM community_members WHERE community = ?1 AND role < 2 ORDER BY role",
        )?;
        let rows = stmt.query_map(params![community.to_string()], |r| r.get::<_, String>(0))?;
        Ok(rows
            .filter_map(Result::ok)
            .map(|a| json!(person_url(&base, &a)))
            .collect::<Vec<_>>())
    })
    .unwrap_or_default();
    let n = i64::try_from(mods.len()).unwrap_or(0);
    collection(
        &format!("{}/moderators", group_url(&base, community)),
        n,
        Some(mods),
    )
}

/// What a local row looks like as an object: the note, with the thread it
/// answers when it is a reply.
fn local_note(conn: &Connection, fed: &Federation, row: &PostRow) -> anyhow::Result<Option<Value>> {
    let Some(public) = posts::public(conn, row.community).map_err(|e| anyhow::anyhow!(e))? else {
        return Ok(None);
    };
    let parent = match row.parent {
        Some(p) => posts::row(conn, p).map_err(|e| anyhow::anyhow!(e))?,
        None => None,
    };
    let parent_author = parent.as_ref().map(|p| objects::author_url(&fed.base, p));
    let reply_to = parent.as_ref().map(|p| ReplyTo {
        ap_id: &p.ap_id,
        author: parent_author.as_deref(),
    });
    let pictures = [&row.picture, &row.snapshot]
        .into_iter()
        .flatten()
        .filter_map(|h| picture_facts(&fed.asset_dir, h).map(|f| (h.clone(), f)))
        .collect();
    let extras = NoteExtras {
        mentions: posts::mentions_of(conn, row.id).map_err(|e| anyhow::anyhow!(e))?,
        pictures,
    };
    Ok(Some(objects::note(
        &fed.base,
        row,
        &public.handle,
        &fed.server,
        reply_to.as_ref(),
        &extras,
    )))
}

async fn group_outbox(State(state): State<HttpState>, Path(id): Path<String>) -> Response {
    let Ok(community) = Uuid::parse_str(&id) else {
        return not_found();
    };
    let fed = state.fedi.clone();
    let items = with_db(&state, |c| {
        if !posts::public(c, community)
            .map_err(|e| anyhow::anyhow!(e))?
            .is_some_and(|p| p.enabled)
        {
            return Ok(None);
        }
        let mut stmt = c.prepare(
            "SELECT id FROM posts WHERE community = ?1 AND parent IS NULL AND hidden_by IS NULL
                 AND author_account IS NOT NULL
             ORDER BY created_ms DESC LIMIT ?2",
        )?;
        let ids: Vec<String> = stmt
            .query_map(params![community.to_string(), OUTBOX_ITEMS], |r| r.get(0))?
            .collect::<Result<_, _>>()?;
        let mut items = Vec::new();
        for id in ids.iter().filter_map(|i| Uuid::parse_str(i).ok()) {
            if let Some(row) = posts::row(c, id).map_err(|e| anyhow::anyhow!(e))? {
                if let Some(note) = local_note(c, &fed, &row)? {
                    // The community's boost of each post, as Lemmy reads a
                    // community's outbox; under the id the boost was sent with.
                    let create = objects::create(&fed.base, note, &row);
                    items.push(objects::announce_as(
                        &objects::post_boost_id(&fed.base, &row, Some("Create")),
                        &fed.base,
                        community,
                        create,
                    ));
                }
            }
        }
        Ok(Some(items))
    });
    match items {
        Ok(Some(items)) => {
            let n = i64::try_from(items.len()).unwrap_or(0);
            collection(
                &format!("{}/outbox", group_url(&fed.base, community)),
                n,
                Some(items),
            )
        }
        Ok(None) => not_found(),
        Err(e) => e.into_response(),
    }
}

async fn post_object(
    State(state): State<HttpState>,
    headers: HeaderMap,
    Path(id): Path<String>,
) -> Response {
    let Ok(id) = Uuid::parse_str(&id) else {
        return not_found();
    };
    let fed = state.fedi.clone();
    let found = with_db(&state, |c| {
        let Some(row) = posts::row(c, id).map_err(|e| anyhow::anyhow!(e))? else {
            return Ok(None);
        };
        // Only what was written here is ours to serve.
        if row.ap_id != super::post_url(&fed.base, id) {
            return Ok(None);
        }
        let enabled = posts::public(c, row.community)
            .map_err(|e| anyhow::anyhow!(e))?
            .is_some_and(|p| p.enabled);
        if row.hidden || !enabled {
            return Ok(Some((row, None)));
        }
        let note = local_note(c, &fed, &row)?;
        Ok(Some((row, note)))
    });
    match found {
        Ok(Some((row, Some(note)))) => {
            if wants_activity(&headers) {
                activity_json(objects::document(note))
            } else {
                let to = row
                    .place
                    .as_ref()
                    .map_or_else(|| fed.base.clone(), |p| super::place_link(&fed.base, p));
                Redirect::to(&to).into_response()
            }
        }
        Ok(Some((row, None))) => (
            StatusCode::GONE,
            [("content-type", "application/activity+json; charset=utf-8")],
            objects::document(json!({ "id": row.ap_id, "type": "Tombstone" })).to_string(),
        )
            .into_response(),
        Ok(None) => not_found(),
        Err(e) => e.into_response(),
    }
}

/// A public post's picture, to anybody: the one way a picture in the store is
/// served without a session, and only while a visible public post names it.
async fn media(State(state): State<HttpState>, Path(hash): Path<String>) -> Response {
    if !crate::assets::is_hash(&hash) {
        return not_found();
    }
    let public = with_db(&state, |c| {
        Ok(posts::picture_is_public(c, &hash).unwrap_or(false))
    })
    .unwrap_or(false)
        || with_db(&state, |c| {
            // A community's own picture, which its actor names as an icon.
            Ok(c.query_row(
                "SELECT 1 FROM communities m JOIN community_public p ON p.community = m.id
                 WHERE m.picture = ?1 AND p.enabled = 1 LIMIT 1",
                params![hash],
                |_| Ok(()),
            )
            .optional()?
            .is_some())
        })
        .unwrap_or(false)
        || with_db(&state, |c| {
            // The picture of a person other servers know, which their actor
            // names as an icon: only somebody findable from outside.
            let accounts: Vec<String> = c
                .prepare("SELECT id FROM accounts WHERE picture = ?1 AND disabled = 0")?
                .query_map(params![hash], |r| r.get(0))?
                .collect::<Result<_, _>>()?;
            for a in accounts {
                if is_public_person(c, &a)? {
                    return Ok(true);
                }
            }
            Ok(false)
        })
        .unwrap_or(false);
    if !public {
        return not_found();
    }
    let dir = state.config.asset_dir.clone();
    let Ok(Some(bytes)) =
        tokio::task::spawn_blocking(move || crate::pictures::load(&dir, &hash)).await
    else {
        return not_found();
    };
    let mime = match image::guess_format(&bytes) {
        Ok(image::ImageFormat::Png) => "image/png",
        Ok(image::ImageFormat::WebP) => "image/webp",
        Ok(image::ImageFormat::Gif) => "image/gif",
        Ok(image::ImageFormat::Jpeg) => "image/jpeg",
        _ => return not_found(),
    };
    (
        [
            ("content-type", mime),
            ("cache-control", "public, max-age=31536000, immutable"),
        ],
        bytes,
    )
        .into_response()
}

// ── the inbox ───────────────────────────────────────────────────────────────

/// Every inbox here is one: what an activity is for is in the activity.
async fn shared_inbox(
    State(state): State<HttpState>,
    uri: Uri,
    headers: HeaderMap,
    body: Bytes,
) -> Response {
    if !state.fedi.enabled {
        return not_found();
    }
    let header = |name: &str| -> Option<String> {
        headers
            .get(name)
            .and_then(|v| v.to_str().ok())
            .map(str::to_string)
    };
    let Some(sig) = header("signature")
        .or_else(|| header("authorization"))
        .and_then(|s| signing::parse_signature(&s))
    else {
        return (StatusCode::UNAUTHORIZED, "sign the request").into_response();
    };
    let Ok(activity) = serde_json::from_slice::<Value>(&body) else {
        return (StatusCode::BAD_REQUEST, "not JSON").into_response();
    };
    let Some(key_host) = super::fetch::host_of(&sig.key_id) else {
        return (StatusCode::UNAUTHORIZED, "a key with no server").into_response();
    };
    // A blocked server is told nothing went wrong, and nothing happens.
    let blocked = with_db(&state, |c| Ok(blocklist::is_blocked(c, &key_host)?)).unwrap_or(true);
    if blocked {
        return StatusCode::ACCEPTED.into_response();
    }
    let path = uri
        .path_and_query()
        .map_or_else(|| uri.path().to_string(), |p| p.as_str().to_string());
    let header_map: Vec<(String, String)> = headers
        .iter()
        .filter_map(|(k, v)| {
            Some((
                k.as_str().to_ascii_lowercase(),
                v.to_str().ok()?.to_string(),
            ))
        })
        .collect();
    let conn = state.conn.clone();
    let fed = state.fedi.clone();
    let filter = Arc::new(state.config.moderation_filter());
    let asset_dir = state.config.asset_dir.clone();
    let verified = tokio::task::spawn_blocking(move || {
        let get = |n: &str| {
            header_map
                .iter()
                .find(|(k, _)| k == n)
                .map(|(_, v)| v.clone())
        };
        let ctx = Ctx {
            conn: &conn,
            client: &fed.client,
            fed: &fed,
            filter: &filter,
            asset_dir: &asset_dir,
        };
        let now = std::time::SystemTime::now();
        let check = |a: &objects::Actor| {
            signing::verify(&sig, &a.public_pem, "POST", &path, get, Some(&body), now)
        };
        // The cached key first; a key that no longer verifies may have been
        // rotated, so it is fetched again once before the request is refused.
        let mut actor = inbox::actor_for_key(&ctx, &sig.key_id, false);
        if actor.as_ref().is_ok_and(|a| check(a).is_err()) {
            actor = inbox::actor_for_key(&ctx, &sig.key_id, true);
        }
        let actor = match actor {
            Ok(a) => a,
            // An account deleted at home signs its own Delete with a key
            // nobody can fetch any more; there is nothing to do with it.
            Err(e) if objects::kind(&activity) == "Delete" => {
                tracing::debug!("fediverse: delete from an unknown key: {e:#}");
                return Ok(None);
            }
            Err(e) => return Err(format!("unknown key {}: {e:#}", sig.key_id)),
        };
        check(&actor).map_err(|e| format!("{}: {e}", actor.id))?;
        if activity.get("actor").and_then(id_of).as_deref() != Some(actor.id.as_str()) {
            return Err(format!("signed by {} for somebody else", actor.id));
        }
        Ok(Some((
            actor,
            activity,
            ctx.fed.clone(),
            filter.clone(),
            asset_dir.clone(),
            conn.clone(),
        )))
    })
    .await;
    let (actor, activity, fed, filter, asset_dir, conn) = match verified {
        Ok(Ok(Some(v))) => v,
        Ok(Ok(None)) => return StatusCode::ACCEPTED.into_response(),
        Ok(Err(why)) => {
            tracing::info!("fediverse: refused an activity: {why}");
            return (StatusCode::UNAUTHORIZED, "the signature does not verify").into_response();
        }
        Err(_) => return StatusCode::INTERNAL_SERVER_ERROR.into_response(),
    };
    // Believed: what it means is worked out after answering, since it may
    // mean fetching objects and pictures from other servers.
    tokio::task::spawn_blocking(move || {
        let ctx = Ctx {
            conn: &conn,
            client: &fed.client,
            fed: &fed,
            filter: &filter,
            asset_dir: &asset_dir,
        };
        if let Err(e) = inbox::handle(&ctx, &actor, &activity) {
            tracing::info!(from = %actor.id, kind = objects::kind(&activity), "fediverse: activity not handled: {e:#}");
        }
    });
    StatusCode::ACCEPTED.into_response()
}

// ── the apps ────────────────────────────────────────────────────────────────

/// A community named in a path by its id, or by its handle (`/c/{handle}` on
/// the web, which is what other servers link to).
fn community_named(state: &HttpState, named: &str) -> ApiResult<Uuid> {
    if let Ok(id) = Uuid::parse_str(named) {
        return Ok(id);
    }
    let handle = named.trim().to_ascii_lowercase();
    db(state, |c| posts::community_by_handle(c, &handle))?
        .ok_or_else(|| refused(Refusal::NoSuchCommunity))
}

async fn public_info(
    State(state): State<HttpState>,
    axum::extract::ConnectInfo(peer): axum::extract::ConnectInfo<std::net::SocketAddr>,
    headers: HeaderMap,
    Path(named): Path<String>,
) -> ApiResult<Json<PublicInfo>> {
    let id = community_named(&state, &named)?;
    let caller = optional_session(&state, &headers).map(|(p, _)| p.to_string());
    // Somebody outside the community looking at its public side, for its
    // moderators' numbers (`crate::stats`).
    crate::stats::api::community_visit(&state, &headers, peer, id, caller.as_deref());
    let fed = state.fedi.clone();
    db(&state, |c| {
        posts::info(c, caller.as_deref(), id, &fed.server, fed.enabled)
    })
    .map(Json)
}

async fn set_public(
    State(state): State<HttpState>,
    headers: HeaderMap,
    Path(id): Path<Uuid>,
    Json(settings): Json<PublicSettings>,
) -> ApiResult<Json<PublicInfo>> {
    let (principal, _) = require_session(&state, &headers)?;
    let by = principal.to_string();
    let fed = state.fedi.clone();
    db(&state, |c| {
        posts::set_public(c, &by, id, &settings, super::now_ms())?;
        posts::info(c, Some(&by), id, &fed.server, fed.enabled)
    })
    .map(Json)
}

#[derive(Deserialize)]
struct FeedQuery {
    before: Option<i64>,
}

async fn feed(
    State(state): State<HttpState>,
    headers: HeaderMap,
    Path(id): Path<Uuid>,
    Query(q): Query<FeedQuery>,
) -> ApiResult<Json<Vec<PostDto>>> {
    let caller = optional_session(&state, &headers).map(|(p, _)| p.to_string());
    let server = state.fedi.server.clone();
    db(&state, |c| {
        posts::feed(c, id, caller.as_deref(), q.before, &server)
    })
    .map(Json)
}

async fn thread(
    State(state): State<HttpState>,
    headers: HeaderMap,
    Path(id): Path<Uuid>,
) -> ApiResult<Json<cartopolis_core::posts::PostThread>> {
    let caller = optional_session(&state, &headers).map(|(p, _)| p.to_string());
    let server = state.fedi.server.clone();
    db(&state, |c| posts::thread(c, id, caller.as_deref(), &server)).map(Json)
}

/// Tell the fediverse about a post or reply written here.
fn publish(conn: &Connection, fed: &Federation, row: &PostRow) -> anyhow::Result<()> {
    send_note(conn, fed, row, Sent::Create)
}

/// Whether a note goes out new or changed.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Sent {
    Create,
    Update,
}

/// Send a note written here: its author's followers, the author of what a
/// reply answers and everybody it mentions get the activity from the author;
/// the community's followers get the community's boost of it, each server in
/// the shape it reads (`servers`). A change goes to the community's
/// followers from the author too, since Mastodon believes an `Update` only from
/// whoever wrote the note.
fn send_note(conn: &Connection, fed: &Federation, row: &PostRow, sent: Sent) -> anyhow::Result<()> {
    if !fed.enabled {
        return Ok(());
    }
    let now = super::now_ms();
    let Some(note) = local_note(conn, fed, row)? else {
        return Ok(());
    };
    let (kind, activity) = match sent {
        Sent::Create => ("Create", objects::create(&fed.base, note, row)),
        Sent::Update => ("Update", objects::update(&fed.base, note, row)),
    };
    let author = posts::person_local(&row.author_account.clone().unwrap_or_default());
    let mut also: Vec<String> = posts::mentions_of(conn, row.id)?
        .into_iter()
        .map(|m| m.actor)
        .collect();
    if let Some(parent) = row.parent.and_then(|p| posts::row(conn, p).ok().flatten()) {
        also.extend(parent.author_actor);
    }
    let group = posts::group_local(row.community);
    let followers = delivery::inboxes(conn, Some(&group), &[])?;
    let mut inboxes = delivery::inboxes(conn, Some(&author), &also)?;
    if sent == Sent::Update {
        inboxes.extend(followers.iter().cloned());
        inboxes.sort();
        inboxes.dedup();
    }
    delivery::fan_out(
        conn,
        &inboxes,
        &author,
        &objects::document(activity.clone()),
        now,
    )?;
    // An edit's boost names the moment, or Lemmy would take it for the first
    // edit's boost again.
    let boost_kind = match sent {
        Sent::Create => kind.to_string(),
        Sent::Update => format!("{kind}-{}", row.edited_ms.unwrap_or(now)),
    };
    let of_activity = objects::document(objects::announce_as(
        &objects::post_boost_id(&fed.base, row, Some(&boost_kind)),
        &fed.base,
        row.community,
        activity,
    ));
    let of_object = (sent == Sent::Create).then(|| {
        objects::document(objects::announce_as(
            &objects::post_boost_id(&fed.base, row, None),
            &fed.base,
            row.community,
            Value::String(row.ap_id.clone()),
        ))
    });
    delivery::fan_out_boost(
        conn,
        &followers,
        &group,
        Some(&of_activity),
        of_object.as_ref(),
        now,
    )?;
    fed.wake.notify_one();
    Ok(())
}

/// The accounts elsewhere a text names, looked up: each `@user@server` that
/// WebFinger finds becomes a mention, the rest stay words. Not this server's
/// own people, and nobody on a blocked server.
async fn resolve_mentions(state: &HttpState, text: &str) -> Vec<Mention> {
    let fed = state.fedi.clone();
    if !fed.enabled {
        return Vec::new();
    }
    let own = format!("@{}", fed.server);
    let addresses: Vec<String> = objects::mentions(text)
        .into_iter()
        .filter(|a| !a.ends_with(&own))
        .take(MAX_MENTIONS)
        .collect();
    if addresses.is_empty() {
        return Vec::new();
    }
    let conn = state.conn.clone();
    let filter = state.config.moderation_filter();
    let asset_dir = state.config.asset_dir.clone();
    tokio::task::spawn_blocking(move || {
        let ctx = Ctx {
            conn: &conn,
            client: &fed.client,
            fed: &fed,
            filter: &filter,
            asset_dir: &asset_dir,
        };
        addresses
            .iter()
            .filter_map(|address| {
                let host = address.rsplit('@').next()?;
                let blocked = conn
                    .lock()
                    .ok()
                    .map_or(true, |c| blocklist::is_blocked(&c, host).unwrap_or(true));
                if blocked {
                    return None;
                }
                let found = fed
                    .client
                    .webfinger(address)
                    .map_err(anyhow::Error::from)
                    .and_then(|url| inbox::resolve_actor(&ctx, &url, false));
                match found {
                    Ok(actor) => Some(Mention {
                        name: format!("@{address}"),
                        href: actor.url.clone().unwrap_or_else(|| actor.id.clone()),
                        actor: actor.id,
                    }),
                    Err(e) => {
                        tracing::info!(%address, "fediverse: mention not found: {e:#}");
                        None
                    }
                }
            })
            .collect()
    })
    .await
    .unwrap_or_default()
}

/// Tell the fediverse a post written here is gone.
fn retract(conn: &Connection, fed: &Federation, gone: &[PostRow]) -> anyhow::Result<()> {
    if !fed.enabled {
        return Ok(());
    }
    let now = super::now_ms();
    for row in gone {
        let Some(author) = &row.author_account else {
            continue;
        };
        let del = objects::delete(&fed.base, row);
        let person = posts::person_local(author);
        let group = posts::group_local(row.community);
        let mut inboxes = delivery::inboxes(conn, Some(&person), &[])?;
        inboxes.extend(delivery::inboxes(conn, Some(&group), &[])?);
        inboxes.sort();
        inboxes.dedup();
        delivery::fan_out(
            conn,
            &inboxes,
            &person,
            &objects::document(del.clone()),
            now,
        )?;
        let followers = delivery::inboxes(conn, Some(&group), &[])?;
        let body = objects::document(objects::announce(&fed.base, row.community, del));
        delivery::fan_out(conn, &followers, &group, &body, now)?;
    }
    fed.wake.notify_one();
    Ok(())
}

/// Plain text through the server's word filter, as public chat is.
fn masked(state: &HttpState, text: &str) -> String {
    state.config.moderation_filter().mask(text).0
}

async fn create_post(
    State(state): State<HttpState>,
    headers: HeaderMap,
    Path(id): Path<Uuid>,
    Json(mut new): Json<NewPost>,
) -> ApiResult<(StatusCode, Json<PostDto>)> {
    let (principal, username) = require_session(&state, &headers)?;
    let by = principal.to_string();
    new.text = masked(&state, &new.text);
    let mentions = resolve_mentions(&state, &new.text).await;
    let fed = state.fedi.clone();
    let dto = db(&state, |c| {
        let post = posts::create_post(c, &by, id, &new, &fed.base, super::now_ms())?;
        posts::set_mentions(c, post, &mentions)?;
        let row = posts::row(c, post)?.ok_or(Refusal::NoSuchPost)?;
        if let Err(e) = publish(c, &fed, &row) {
            tracing::warn!("fediverse: post not published: {e:#}");
        }
        posts::dto(c, &row, Some(&by), &fed.server)
    })?;
    tracing::info!(%username, community = %id, post = %dto.id, "public post written");
    Ok((StatusCode::CREATED, Json(dto)))
}

async fn reply(
    State(state): State<HttpState>,
    headers: HeaderMap,
    Path(id): Path<Uuid>,
    Json(new): Json<NewReply>,
) -> ApiResult<(StatusCode, Json<PostDto>)> {
    let (principal, _) = require_session(&state, &headers)?;
    let by = principal.to_string();
    let text = masked(&state, &new.text);
    let mentions = resolve_mentions(&state, &text).await;
    let fed = state.fedi.clone();
    let dto = db(&state, |c| {
        let (reply, _) = posts::create_reply(c, &by, id, &text, &fed.base, super::now_ms())?;
        posts::set_mentions(c, reply, &mentions)?;
        let row = posts::row(c, reply)?.ok_or(Refusal::NoSuchPost)?;
        if let Err(e) = publish(c, &fed, &row) {
            tracing::warn!("fediverse: reply not published: {e:#}");
        }
        posts::dto(c, &row, Some(&by), &fed.server)
    })?;
    Ok((StatusCode::CREATED, Json(dto)))
}

/// The author changes a post or reply's words; other servers get the change.
async fn edit_post(
    State(state): State<HttpState>,
    headers: HeaderMap,
    Path(id): Path<Uuid>,
    Json(edit): Json<EditPost>,
) -> ApiResult<Json<PostDto>> {
    let (principal, _) = require_session(&state, &headers)?;
    let by = principal.to_string();
    let text = masked(&state, &edit.text);
    let mentions = resolve_mentions(&state, &text).await;
    let fed = state.fedi.clone();
    let dto = db(&state, |c| {
        let row = posts::edit(c, &by, id, &text, super::now_ms())?;
        posts::set_mentions(c, row.id, &mentions)?;
        if let Err(e) = send_note(c, &fed, &row, Sent::Update) {
            tracing::warn!("fediverse: edit not sent: {e:#}");
        }
        posts::dto(c, &row, Some(&by), &fed.server)
    })?;
    Ok(Json(dto))
}

async fn delete_post(
    State(state): State<HttpState>,
    headers: HeaderMap,
    Path(id): Path<Uuid>,
) -> ApiResult<StatusCode> {
    let (principal, _) = require_session(&state, &headers)?;
    let by = principal.to_string();
    let fed = state.fedi.clone();
    db(&state, |c| {
        let gone = posts::delete(c, &by, id)?;
        if let Err(e) = retract(c, &fed, &gone) {
            tracing::warn!("fediverse: deletion not sent: {e:#}");
        }
        Ok(())
    })?;
    Ok(StatusCode::NO_CONTENT)
}

/// A moderator hides a post. One written here is also withdrawn from the
/// fediverse, as its author deleting it would; one from elsewhere is only
/// hidden here, since it is not this community's to delete.
async fn hide_post(
    State(state): State<HttpState>,
    headers: HeaderMap,
    Path(id): Path<Uuid>,
) -> ApiResult<StatusCode> {
    let (principal, _) = require_session(&state, &headers)?;
    let by = principal.to_string();
    let fed = state.fedi.clone();
    db(&state, |c| {
        let row = posts::hide(c, &by, id)?;
        if row.author_account.is_some() {
            if let Err(e) = retract(c, &fed, std::slice::from_ref(&row)) {
                tracing::warn!("fediverse: removal not sent: {e:#}");
            }
        }
        Ok(())
    })?;
    Ok(StatusCode::NO_CONTENT)
}

#[derive(Deserialize)]
struct AddressBody {
    address: String,
}

/// A moderator, and posts on, and this server federating: what following or
/// blocking something elsewhere needs.
fn must_moderate_public(state: &HttpState, by: &str, community: Uuid) -> ApiResult<()> {
    if !state.fedi.enabled {
        return Err(ApiError(
            StatusCode::CONFLICT,
            "this server does not talk to other servers".into(),
        ));
    }
    let fed = state.fedi.clone();
    let info = db(state, |c| {
        posts::info(c, Some(by), community, &fed.server, true)
    })?;
    if !info.may_moderate {
        return Err(refused(Refusal::NotAModerator));
    }
    if !info.settings.enabled {
        return Err(refused(Refusal::PostsOff));
    }
    Ok(())
}

async fn follow_source(
    State(state): State<HttpState>,
    headers: HeaderMap,
    Path(id): Path<Uuid>,
    Json(body): Json<AddressBody>,
) -> ApiResult<Json<PublicInfo>> {
    let (principal, _) = require_session(&state, &headers)?;
    let by = principal.to_string();
    must_moderate_public(&state, &by, id)?;
    let conn = state.conn.clone();
    let fed = state.fedi.clone();
    let filter = state.config.moderation_filter();
    let asset_dir = state.config.asset_dir.clone();
    let address = body.address.clone();
    let actor = tokio::task::spawn_blocking(move || {
        let ctx = Ctx {
            conn: &conn,
            client: &fed.client,
            fed: &fed,
            filter: &filter,
            asset_dir: &asset_dir,
        };
        // An address (`name@server`), or an actor's own URL: a hashtag relay
        // (`https://relay.fedi.buzz/tag/groningen`) has no address.
        let typed = address.trim();
        let url = if typed.starts_with("https://") || typed.starts_with("http://") {
            typed.to_string()
        } else {
            fed.client.webfinger(typed)?
        };
        inbox::resolve_actor(&ctx, &url, true)
    })
    .await
    .map_err(|e| internal(anyhow::anyhow!("follow panicked: {e}")))?
    .map_err(|e| {
        tracing::info!("fediverse: could not follow {}: {e:#}", body.address);
        ApiError(
            StatusCode::BAD_REQUEST,
            format!("could not find {} on the fediverse", body.address.trim()),
        )
    })?;
    let blocked = with_db(&state, |c| Ok(blocklist::is_blocked(c, &actor.host)?))?;
    if blocked {
        return Err(ApiError(
            StatusCode::FORBIDDEN,
            format!("{} is blocked on this server", actor.host),
        ));
    }
    let fed = state.fedi.clone();
    // The community's reader follows, not the community: Lemmy takes a follow
    // only from a person or a bot (`super::reader_url`).
    let reader = reader_url(&fed.base, id);
    let follow_id = format!("{reader}#follow-{}", Uuid::new_v4());
    let now = super::now_ms();
    let info = db(&state, |c| {
        c.execute(
            "INSERT INTO community_follows (community, actor, follow_id, accepted, created_by, created_ms)
             VALUES (?1, ?2, ?3, 0, ?4, ?5)
             ON CONFLICT (community, actor) DO UPDATE SET follow_id = ?3, accepted = 0",
            params![id.to_string(), actor.id, follow_id, by, now],
        )?;
        let body = objects::document(objects::follow(&follow_id, &reader, &actor.id));
        delivery::enqueue(c, &actor.inbox, &posts::reader_local(id), &body, now)?;
        posts::info(c, Some(&by), id, &fed.server, fed.enabled)
    })?;
    fed.wake.notify_one();
    Ok(Json(info))
}

async fn unfollow_source(
    State(state): State<HttpState>,
    headers: HeaderMap,
    Path((id, address)): Path<(Uuid, String)>,
) -> ApiResult<Json<PublicInfo>> {
    let (principal, _) = require_session(&state, &headers)?;
    let by = principal.to_string();
    must_moderate_public(&state, &by, id)?;
    let fed = state.fedi.clone();
    let typed = address.trim().trim_start_matches(['@', '!']).to_string();
    let by_name = typed
        .rsplit_once('@')
        .map(|(n, h)| (n.to_string(), h.to_ascii_lowercase()));
    if by_name.is_none() && !typed.starts_with("http") {
        return Err(ApiError(StatusCode::BAD_REQUEST, "not name@server".into()));
    }
    let now = super::now_ms();
    let info = db(&state, |c| {
        let row: Option<(String, String, String)> = match &by_name {
            Some((name, host)) => c.query_row(
                "SELECT f.actor, f.follow_id, a.inbox FROM community_follows f
                 JOIN fedi_actors a ON a.id = f.actor
                 WHERE f.community = ?1 AND a.username = ?2 AND a.host = ?3",
                params![id.to_string(), name, host],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
            ),
            None => c.query_row(
                "SELECT f.actor, f.follow_id, a.inbox FROM community_follows f
                 JOIN fedi_actors a ON a.id = f.actor
                 WHERE f.community = ?1 AND f.actor = ?2",
                params![id.to_string(), typed],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
            ),
        }
        .optional()?;
        if let Some((actor, follow_id, inbox)) = row {
            // Undone by whoever followed: the reader, or the community itself
            // for a follow made before readers were.
            let follower = follow_id.split('#').next().unwrap_or_default().to_string();
            let signer = if follower == reader_url(&fed.base, id) {
                posts::reader_local(id)
            } else {
                posts::group_local(id)
            };
            let undo = objects::undo(&follower, objects::follow(&follow_id, &follower, &actor));
            delivery::enqueue(c, &inbox, &signer, &objects::document(undo), now)?;
            c.execute(
                "DELETE FROM community_follows WHERE community = ?1 AND actor = ?2",
                params![id.to_string(), actor],
            )?;
        }
        posts::info(c, Some(&by), id, &fed.server, fed.enabled)
    })?;
    fed.wake.notify_one();
    Ok(Json(info))
}

/// A moderator blocks the author of a post from elsewhere, by the address the
/// feed shows.
async fn block_author(
    State(state): State<HttpState>,
    headers: HeaderMap,
    Path(id): Path<Uuid>,
    Json(body): Json<AddressBody>,
) -> ApiResult<StatusCode> {
    let (principal, _) = require_session(&state, &headers)?;
    let by = principal.to_string();
    let (name, host) = body
        .address
        .trim()
        .trim_start_matches('@')
        .rsplit_once('@')
        .ok_or_else(|| ApiError(StatusCode::BAD_REQUEST, "not name@server".into()))?;
    let (name, host) = (name.to_string(), host.to_ascii_lowercase());
    db(&state, |c| {
        let actor: Option<String> = c
            .query_row(
                "SELECT id FROM fedi_actors WHERE username = ?1 AND host = ?2",
                params![name, host],
                |r| r.get(0),
            )
            .optional()?;
        let actor = actor.ok_or(Refusal::NoSuchPost)?;
        posts::block_actor(c, &by, id, &actor, super::now_ms())
    })?;
    Ok(StatusCode::NO_CONTENT)
}

/// Reports from other servers waiting for the community's moderators.
async fn list_reports(
    State(state): State<HttpState>,
    headers: HeaderMap,
    Path(id): Path<Uuid>,
) -> ApiResult<Json<Vec<PostReport>>> {
    let (principal, _) = require_session(&state, &headers)?;
    let by = principal.to_string();
    let server = state.fedi.server.clone();
    db(&state, |c| posts::open_reports(c, &by, id, &server)).map(Json)
}

#[derive(Deserialize)]
struct ResolveBody {
    action: ReportAction,
}

/// A moderator hides the reported post or dismisses the report. Hiding a post
/// written here withdraws it from other servers, as hiding it does anywhere.
async fn resolve_report(
    State(state): State<HttpState>,
    headers: HeaderMap,
    Path((id, report)): Path<(Uuid, Uuid)>,
    Json(body): Json<ResolveBody>,
) -> ApiResult<StatusCode> {
    let (principal, _) = require_session(&state, &headers)?;
    let by = principal.to_string();
    let fed = state.fedi.clone();
    db(&state, |c| {
        let hidden = posts::resolve_report(c, &by, id, report, body.action, super::now_ms())?;
        if let Some(row) = hidden.filter(|r| r.author_account.is_some()) {
            if let Err(e) = retract(c, &fed, std::slice::from_ref(&row)) {
                tracing::warn!("fediverse: removal not sent: {e:#}");
            }
        }
        Ok(())
    })?;
    Ok(StatusCode::NO_CONTENT)
}

#[derive(Deserialize)]
struct FollowLinkQuery {
    /// Their server, or their address on it.
    server: String,
    /// What to follow: `handle@server` here.
    address: String,
}

/// Where somebody follows an address here from their own server: the page each
/// kind of server has for it (`servers::follow_link`), chosen by asking that
/// server what it runs. For a web page's "Follow from Mastodon, Lemmy, …".
async fn follow_link(
    State(state): State<HttpState>,
    Query(q): Query<FollowLinkQuery>,
) -> ApiResult<Json<FollowLink>> {
    let server = servers::server_in(&q.server).ok_or_else(|| {
        ApiError(
            StatusCode::BAD_REQUEST,
            "a server is a name like mastodon.social".into(),
        )
    })?;
    let fed = state.fedi.clone();
    let address = q.address.trim().trim_start_matches(['@', '!']).to_string();
    let (name, host) = address
        .rsplit_once('@')
        .ok_or_else(|| ApiError(StatusCode::BAD_REQUEST, "not name@server".into()))?;
    if !host.eq_ignore_ascii_case(&fed.server) {
        return Err(ApiError(
            StatusCode::BAD_REQUEST,
            "only an address on this server".into(),
        ));
    }
    let name = name.to_ascii_lowercase();
    let group = db(&state, |c| posts::community_by_handle(c, &name))?.is_some();
    let software = if fed.enabled {
        let conn = state.conn.clone();
        let host = server.clone();
        tokio::task::spawn_blocking(move || {
            servers::software(&conn, &fed.client, &host, super::now_ms())
        })
        .await
        .ok()
        .flatten()
    } else {
        None
    };
    Ok(Json(FollowLink {
        url: servers::follow_link(&server, software.as_deref(), &address, group),
        software,
    }))
}

#[derive(Deserialize)]
struct PersonQuery {
    author: String,
}

/// A person's public posts, for their page (`/@name`), which is where other
/// servers link to them.
async fn person_posts(
    State(state): State<HttpState>,
    headers: HeaderMap,
    Query(q): Query<PersonQuery>,
) -> ApiResult<Json<PersonPosts>> {
    let caller = optional_session(&state, &headers).map(|(p, _)| p.to_string());
    let server = state.fedi.server.clone();
    let name = q.author.trim().trim_start_matches('@');
    let name = name
        .strip_suffix(&format!("@{server}"))
        .unwrap_or(name)
        .to_string();
    db(&state, |c| {
        posts::person_posts(c, &name, caller.as_deref(), &server)
    })
    .map(Json)
}

// ── the operator ────────────────────────────────────────────────────────────

async fn list_server_blocks(
    State(state): State<HttpState>,
    headers: HeaderMap,
) -> ApiResult<Json<Value>> {
    require(&state, &headers, Permission::ReportsTriage)?;
    let rows = with_db(&state, |c| Ok(blocklist::list(c)?))?;
    Ok(Json(json!(rows
        .into_iter()
        .map(|(host, source, reason)| json!({ "host": host, "source": source, "reason": reason }))
        .collect::<Vec<_>>())))
}

#[derive(Deserialize)]
struct BlockBody {
    host: String,
    #[serde(default)]
    reason: String,
}

async fn add_server_block(
    State(state): State<HttpState>,
    headers: HeaderMap,
    Json(body): Json<BlockBody>,
) -> ApiResult<StatusCode> {
    let (_, username) = require(&state, &headers, Permission::ReportsTriage)?;
    let host = body.host.trim().trim_end_matches('.').to_ascii_lowercase();
    if host.is_empty() || !host.contains('.') || host.contains(['/', ' ', '@', ':']) {
        return Err(ApiError(
            StatusCode::BAD_REQUEST,
            "a server is a name like example.org".into(),
        ));
    }
    with_db(&state, |c| {
        Ok(blocklist::block(c, &host, &body.reason, super::now_ms())?)
    })?;
    tracing::info!(%host, by = %username, "fediverse: server blocked");
    Ok(StatusCode::NO_CONTENT)
}

async fn remove_server_block(
    State(state): State<HttpState>,
    headers: HeaderMap,
    Path(host): Path<String>,
) -> ApiResult<StatusCode> {
    let (_, username) = require(&state, &headers, Permission::ReportsTriage)?;
    with_db(&state, |c| Ok(blocklist::unblock(c, &host)?))?;
    tracing::info!(%host, by = %username, "fediverse: server unblocked");
    Ok(StatusCode::NO_CONTENT)
}
