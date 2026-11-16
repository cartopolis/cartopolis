//! What another server may ask of this one about chat: signed POSTs under
//! [`super::PREFIX`]. Each handler first checks who is asking
//! ([`verified`]), then that the people it names are its own, then applies the
//! same rules the wire applies for somebody here.

use std::sync::Arc;

use axum::body::Bytes;
use axum::extract::State;
use axum::http::{HeaderMap, StatusCode, Uri};
use axum::response::{IntoResponse, Response};
use axum::routing::post;
use axum::{Json, Router};
use cartopolis_core::{address, NetMessage};
use rusqlite::{params, Connection, OptionalExtension};
use serde::de::DeserializeOwned;

use super::{
    account, own_device, ClaimKeys, ContactAnswer, ContactRequest, ContactRequested, Deliver, Done,
    Entry, Invite, KeyPackage, Keys, Link, LookUp, Person, Submit, Submitted, Sync, Synced, PREFIX,
};
use crate::contacts::{self, Refusal, Sent};
use crate::fediverse::fetch::host_of;
use crate::fediverse::inbox::{self, Ctx};
use crate::fediverse::{blocklist, signing};
use crate::http::HttpState;

/// Largest body taken: a sealed entry and its invitation, base64'd, with room.
const MAX_BODY: usize = 256 * 1024;

pub fn routes() -> Router<HttpState> {
    let limit = axum::extract::DefaultBodyLimit::max(MAX_BODY);
    Router::new()
        .route(&format!("{PREFIX}/person"), post(person))
        .route(&format!("{PREFIX}/contact-request"), post(contact_request))
        .route(&format!("{PREFIX}/contact-answer"), post(contact_answer))
        .route(&format!("{PREFIX}/keys"), post(keys))
        .route(&format!("{PREFIX}/submit"), post(submit))
        .route(&format!("{PREFIX}/deliver"), post(deliver))
        .route(&format!("{PREFIX}/invite"), post(invite))
        .route(&format!("{PREFIX}/sync"), post(sync))
        .layer(limit)
}

fn refuse(status: StatusCode, why: impl Into<String>) -> Response {
    (status, why.into()).into_response()
}

/// The server a request comes from, once its signature is checked against
/// that server's own instance key, and its body. A server speaks for the
/// addresses that end in its name and for nothing else; every handler checks
/// that of what it is asked.
async fn verified<T: DeserializeOwned>(
    state: &HttpState,
    uri: &Uri,
    headers: &HeaderMap,
    body: &Bytes,
) -> Result<(String, Link, T), Response> {
    let link = Link::from_config(&state.config);
    if !state.fedi.enabled {
        return Err(refuse(StatusCode::NOT_FOUND, "not found"));
    }
    let header = |name: &str| -> Option<String> {
        headers
            .get(name)
            .and_then(|v| v.to_str().ok())
            .map(str::to_string)
    };
    let Some(sig) = header("signature").and_then(|s| signing::parse_signature(&s)) else {
        return Err(refuse(StatusCode::UNAUTHORIZED, "sign the request"));
    };
    let Some(server) = host_of(&sig.key_id) else {
        return Err(refuse(StatusCode::UNAUTHORIZED, "a key with no server"));
    };
    if sig.key_id != link.key_id_of(&server) {
        return Err(refuse(
            StatusCode::UNAUTHORIZED,
            "sign with the server's own key",
        ));
    }
    let blocked = {
        let c = state
            .conn
            .lock()
            .map_err(|_| refuse(StatusCode::SERVICE_UNAVAILABLE, ""))?;
        blocklist::is_blocked(&c, &server).unwrap_or(true)
    };
    if blocked {
        return Err(refuse(
            StatusCode::FORBIDDEN,
            "this server does not talk to yours",
        ));
    }
    if !state.fedi.inbox_limits.per_server.allow(&server) {
        return Err(refuse(StatusCode::TOO_MANY_REQUESTS, "too many at once"));
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
    let (conn, fed) = (state.conn.clone(), state.fedi.clone());
    let filter = Arc::new(state.config.moderation_filter());
    let asset_dir = state.config.asset_dir.clone();
    let signed = body.clone();
    let checked = tokio::task::spawn_blocking(move || {
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
        let check = |pem: &str| signing::verify(&sig, pem, "POST", &path, get, Some(&signed), now);
        // The cached key first; one that no longer verifies may have been
        // rotated, so it is fetched again once.
        let mut actor = inbox::actor_for_key(&ctx, &sig.key_id, false);
        if actor.as_ref().is_ok_and(|a| check(&a.public_pem).is_err()) {
            actor = inbox::actor_for_key(&ctx, &sig.key_id, true);
        }
        let actor = actor.map_err(|e| format!("{e:#}"))?;
        check(&actor.public_pem).map_err(|e| e.to_string())
    })
    .await;
    match checked {
        Ok(Ok(())) => {}
        Ok(Err(why)) => {
            tracing::info!(%server, "chat elsewhere: refused a call: {why}");
            return Err(refuse(
                StatusCode::UNAUTHORIZED,
                "the signature does not verify",
            ));
        }
        Err(_) => return Err(refuse(StatusCode::INTERNAL_SERVER_ERROR, "")),
    }
    let body = serde_json::from_slice(body).map_err(|e| {
        refuse(
            StatusCode::BAD_REQUEST,
            format!("not what was expected: {e}"),
        )
    })?;
    Ok((server, link, body))
}

/// The name in `who` when it is somebody on `server`.
fn on<'a>(who: &'a str, server: &str) -> Option<&'a str> {
    address::split(who.trim())
        .filter(|(_, s)| s.eq_ignore_ascii_case(server))
        .map(|(n, _)| n)
}

/// A person here by name: never somebody elsewhere, whose name holds an `@`.
fn here(c: &Connection, name: &str) -> Result<Option<contacts::Account>, Refusal> {
    if name.contains(super::ADDRESS_MARK) {
        return Ok(None);
    }
    contacts::account_by_name(c, name)
}

/// Run `f` on the database off the async runtime; a storage failure is a 500.
async fn db<R: Send + 'static>(
    state: &HttpState,
    f: impl FnOnce(&Connection) -> Result<R, Response> + Send + 'static,
) -> Result<R, Response> {
    crate::db::call(&state.conn, move |c| f(c))
        .await
        .map_err(|_| refuse(StatusCode::INTERNAL_SERVER_ERROR, ""))?
}

fn storage<E: std::fmt::Display>(e: E) -> Response {
    tracing::warn!("chat elsewhere: {e}");
    refuse(StatusCode::INTERNAL_SERVER_ERROR, "")
}

/// Somebody elsewhere looks somebody here up, for the card that has the
/// request button: answered as for a lookup here, with nothing about who else
/// is here (no contacts in common, no distance).
async fn person(
    State(state): State<HttpState>,
    uri: Uri,
    headers: HeaderMap,
    body: Bytes,
) -> Response {
    let (server, link, ask) = match verified::<LookUp>(&state, &uri, &headers, &body).await {
        Ok(v) => v,
        Err(r) => return r,
    };
    if on(&ask.by, &server).is_none() {
        return refuse(StatusCode::FORBIDDEN, "not somebody on your server");
    }
    let found = db(&state, move |c| {
        let by = account(c, &ask.by, &link.server)
            .map_err(storage)?
            .and_then(|id| contacts::account_by_id(c, &id).ok().flatten())
            .ok_or_else(|| refuse(StatusCode::BAD_REQUEST, "not an address elsewhere"))?;
        let Some(them) = here(c, &ask.name).map_err(storage)? else {
            return Ok(None);
        };
        let person = contacts::person(c, &by, &them, None, None, now_ms()).map_err(storage)?;
        Ok(person.map(|mut p| {
            p.username = address::person(&them.username, &link.server);
            p.in_common.clear();
            p
        }))
    })
    .await;
    match found {
        Ok(person) => Json(Person { person }).into_response(),
        Err(r) => r,
    }
}

/// Somebody elsewhere asks somebody here to be contacts. Applied here as if
/// they had asked on this server, this person's setting and limits included:
/// their home applied its own before it sent it.
async fn contact_request(
    State(state): State<HttpState>,
    uri: Uri,
    headers: HeaderMap,
    body: Bytes,
) -> Response {
    let (server, link, req) = match verified::<ContactRequest>(&state, &uri, &headers, &body).await
    {
        Ok(v) => v,
        Err(r) => return r,
    };
    if on(&req.from, &server).is_none() {
        return refuse(StatusCode::FORBIDDEN, "not somebody on your server");
    }
    let outcome = db(&state, move |c| {
        let from = account(c, &req.from, &link.server)
            .map_err(storage)?
            .and_then(|id| contacts::account_by_id(c, &id).ok().flatten())
            .ok_or_else(|| refuse(StatusCode::BAD_REQUEST, "not an address elsewhere"))?;
        let to = here(c, &req.to).map_err(storage)?.ok_or_else(|| {
            refuse(
                StatusCode::FORBIDDEN,
                Refusal::NoSuchPerson(req.to.clone()).to_string(),
            )
        })?;
        match contacts::request(c, &from, &to, None, req.link_code.as_deref(), now_ms()) {
            Ok(sent) => Ok((sent, to.id)),
            // Nobody was standing next to anybody: "only through their link"
            // would confirm an account that asked not to be found.
            Err(Refusal::OnlyLink(name)) => Err(refuse(
                StatusCode::FORBIDDEN,
                Refusal::NoSuchPerson(name).to_string(),
            )),
            Err(r @ Refusal::Storage(_)) => Err(storage(r)),
            Err(r) => Err(refuse(StatusCode::FORBIDDEN, r.to_string())),
        }
    })
    .await;
    match outcome {
        Ok((sent, to)) => {
            if sent != Sent::Unchanged {
                crate::net::contacts::push_contacts(&state.conn, &state.chat, &to).await;
            }
            Json(ContactRequested {
                contacts: sent == Sent::Contacts,
            })
            .into_response()
        }
        Err(r) => r,
    }
}

/// Somebody elsewhere answers the request somebody here sent them.
async fn contact_answer(
    State(state): State<HttpState>,
    uri: Uri,
    headers: HeaderMap,
    body: Bytes,
) -> Response {
    let (server, link, ans) = match verified::<ContactAnswer>(&state, &uri, &headers, &body).await {
        Ok(v) => v,
        Err(r) => return r,
    };
    if on(&ans.from, &server).is_none() {
        return refuse(StatusCode::FORBIDDEN, "not somebody on your server");
    }
    let accept = ans.accept;
    let outcome = db(&state, move |c| {
        let me = account(c, &ans.from, &link.server)
            .map_err(storage)?
            .and_then(|id| contacts::account_by_id(c, &id).ok().flatten())
            .ok_or_else(|| refuse(StatusCode::BAD_REQUEST, "not an address elsewhere"))?;
        let asker = here(c, &ans.to)
            .map_err(storage)?
            .ok_or_else(|| refuse(StatusCode::FORBIDDEN, "no such person"))?;
        match contacts::answer(c, &me, &asker, accept, now_ms()) {
            Ok(()) => Ok(asker.id),
            Err(r @ Refusal::Storage(_)) => Err(storage(r)),
            Err(r) => Err(refuse(StatusCode::FORBIDDEN, r.to_string())),
        }
    })
    .await;
    match outcome {
        Ok(asker) => {
            // Only an acceptance is news: a decline must look like nothing.
            if accept {
                crate::net::contacts::push_contacts(&state.conn, &state.chat, &asker).await;
            }
            Json(Done {}).into_response()
        }
        Err(r) => r,
    }
}

/// Somebody elsewhere claims a key package of each device of somebody here,
/// to start a private chat with them: only with a contact or a request
/// between the two, as here (`contacts`, rule 4).
async fn keys(
    State(state): State<HttpState>,
    uri: Uri,
    headers: HeaderMap,
    body: Bytes,
) -> Response {
    let (server, link, claim) = match verified::<ClaimKeys>(&state, &uri, &headers, &body).await {
        Ok(v) => v,
        Err(r) => return r,
    };
    if on(&claim.by, &server).is_none() {
        return refuse(StatusCode::FORBIDDEN, "not somebody on your server");
    }
    let outcome = db(&state, move |c| {
        let by = account(c, &claim.by, &link.server)
            .map_err(storage)?
            .ok_or_else(|| refuse(StatusCode::BAD_REQUEST, "not an address elsewhere"))?;
        let of = here(c, &claim.of).map_err(storage)?;
        let Some(of) = of else {
            // Nobody by that name: no devices, as here.
            return Ok(Vec::new());
        };
        if !contacts::may_invite(c, &by, &of.id).map_err(storage)? {
            return Err(refuse(
                StatusCode::FORBIDDEN,
                Refusal::NotAContact(address::person(&of.username, &link.server)).to_string(),
            ));
        }
        crate::e2ee::claim_key_packages(c, &of.username, None).map_err(storage)
    })
    .await;
    match outcome {
        Ok(packages) => Json(Keys {
            packages: packages
                .into_iter()
                .map(|(device, bytes)| KeyPackage { device, bytes })
                .collect(),
        })
        .into_response(),
        Err(r) => r,
    }
}

/// A device elsewhere writes into a chat this server is the hub of: appended
/// as if it had been sent here, by the account that stands for its person.
async fn submit(
    State(state): State<HttpState>,
    uri: Uri,
    headers: HeaderMap,
    body: Bytes,
) -> Response {
    let (server, link, sub) = match verified::<Submit>(&state, &uri, &headers, &body).await {
        Ok(v) => v,
        Err(r) => return r,
    };
    if on(&sub.from, &server).is_none() {
        return refuse(StatusCode::FORBIDDEN, "not somebody on your server");
    }
    if link.hub_elsewhere(&sub.group).is_some() || address::group_home(&sub.group).is_none() {
        return refuse(StatusCode::BAD_REQUEST, "that chat does not live here");
    }
    let here_name = link.server.clone();
    let commit = sub.commit;
    let from = address::qualify(&sub.from, &here_name);
    let outcome = db(&state, move |c| {
        let who = account(c, &sub.from, &here_name)
            .map_err(storage)?
            .ok_or_else(|| refuse(StatusCode::BAD_REQUEST, "not an address elsewhere"))?;
        if !own_device(c, &who, sub.device).map_err(storage)? {
            return Ok(Err(crate::e2ee::Refusal::ForeignDevice.to_string()));
        }
        // An invitation's members as the accounts here that stand for them.
        let welcome = match sub.welcome {
            None => None,
            Some(w) => {
                let mut members = Vec::with_capacity(w.members.len());
                for m in &w.members {
                    let name = match account(c, m, &here_name).map_err(storage)? {
                        Some(_) => address::qualify(m, &here_name),
                        None => match address::split(m) {
                            Some((n, _)) => n.to_string(),
                            None => m.clone(),
                        },
                    };
                    members.push(name);
                }
                Some((w.devices, members, w.bytes))
            }
        };
        if !sub.commit {
            if let Err(r) = contacts::may_send(c, &who, &sub.group) {
                return Ok(Err(r.to_string()));
            }
        }
        let appended = crate::e2ee::append(
            c,
            &who,
            &from,
            sub.device,
            &sub.group,
            sub.epoch,
            sub.commit,
            &sub.message,
            welcome
                .as_ref()
                .map(|(devices, members, bytes)| crate::e2ee::WelcomeFor {
                    devices,
                    members,
                    bytes,
                }),
            now_ms(),
        );
        let appended = match appended {
            Ok(a) => a,
            Err(r) => return Ok(Err(r.to_string())),
        };
        // The declined stay out of it, silently, as for a send here.
        let silenced = contacts::silenced_by(c, &who, &sub.group).ok().flatten();
        if let Some(o) = &silenced {
            let _ = contacts::leave_direct_chat(c, o, &who);
        }
        Ok(Ok((appended, silenced)))
    })
    .await;
    let (appended, silenced) = match outcome {
        Ok(Ok(v)) => v,
        Ok(Err(refused)) => {
            return Json(Submitted {
                seq: None,
                refused: Some(refused),
            })
            .into_response()
        }
        Err(r) => return r,
    };
    let (members, invites) = match &silenced {
        Some(o) => (
            appended.members.into_iter().filter(|m| m != o).collect(),
            Vec::new(),
        ),
        None => (appended.members, appended.invites),
    };
    let e = &appended.entry;
    let entry = Entry::of(e, &link.server);
    super::deliver_here(
        &state.chat,
        &members,
        &entry.clone().message(),
        &e.sender_name,
        commit,
    );
    for inv in &invites {
        if let Ok(frame) = cartopolis_core::encode_app_frame(&NetMessage::E2eeInvite {
            group: inv.group.clone(),
            from: address::qualify(&inv.from, &link.server),
            after: inv.after,
            welcome: inv.welcome.clone(),
        }) {
            state.chat.deliver_to_device(inv.device, &frame.into());
        }
    }
    super::forward_appended(&link, &state.conn, &members, &invites, e);
    Json(Submitted {
        seq: Some(e.seq),
        refused: None,
    })
    .into_response()
}

/// The hub of a chat hands on an entry for people here.
async fn deliver(
    State(state): State<HttpState>,
    uri: Uri,
    headers: HeaderMap,
    body: Bytes,
) -> Response {
    let (server, _link, d) = match verified::<Deliver>(&state, &uri, &headers, &body).await {
        Ok(v) => v,
        Err(r) => return r,
    };
    // Only a chat's hub speaks for its log.
    if !address::group_home(&d.entry.group).is_some_and(|h| h.eq_ignore_ascii_case(&server)) {
        return refuse(StatusCode::FORBIDDEN, "that chat is not yours");
    }
    let names = d.to.clone();
    let accounts = match db(&state, move |c| {
        let mut ids = Vec::new();
        for n in &names {
            if let Some(a) = here(c, n).map_err(storage)? {
                ids.push(a.id);
            }
        }
        Ok(ids)
    })
    .await
    {
        Ok(ids) => ids,
        Err(r) => return r,
    };
    let sender = d.entry.sender.to_lowercase();
    let commit = d.entry.commit;
    super::deliver_here(&state.chat, &accounts, &d.entry.message(), &sender, commit);
    Json(Done {}).into_response()
}

/// The hub of a chat sends an invitation for a device here: kept until the
/// device joins, as one made here is.
async fn invite(
    State(state): State<HttpState>,
    uri: Uri,
    headers: HeaderMap,
    body: Bytes,
) -> Response {
    let (server, link, inv) = match verified::<Invite>(&state, &uri, &headers, &body).await {
        Ok(v) => v,
        Err(r) => return r,
    };
    let here_name = link.server.clone();
    if !address::group_home(&inv.group).is_some_and(|h| h.eq_ignore_ascii_case(&server)) {
        return refuse(StatusCode::FORBIDDEN, "that chat is not yours");
    }
    let (device, group, from, after, welcome) = (
        inv.device,
        inv.group.clone(),
        inv.from.to_lowercase(),
        inv.after,
        inv.welcome.clone(),
    );
    let kept = db(&state, move |c| {
        let Some(to) = here(c, &inv.to).map_err(storage)? else {
            return Ok(false);
        };
        let owner: Option<String> = c
            .query_row(
                "SELECT account FROM e2ee_devices WHERE device = ?1",
                params![inv.device.to_string()],
                |r| r.get(0),
            )
            .optional()
            .map_err(storage)?;
        if owner.as_deref() != Some(to.id.as_str()) {
            return Ok(false);
        }
        // Only from somebody they have a contact or a request with, as an
        // invitation made here: a server cannot put strangers' chats on
        // somebody's devices.
        let inviter = match address::split(&inv.from) {
            Some((name, s)) if s.eq_ignore_ascii_case(&here_name) => {
                here(c, name).map_err(storage)?.map(|a| a.id)
            }
            _ => account(c, &inv.from, &here_name).map_err(storage)?,
        };
        let Some(inviter) = inviter else {
            return Ok(false);
        };
        if !contacts::may_invite(c, &inviter, &to.id).map_err(storage)? {
            return Ok(false);
        }
        c.execute(
            "INSERT INTO e2ee_invites (device, group_id, sender_name, after, welcome)
             VALUES (?1, ?2, ?3, ?4, ?5)",
            params![
                inv.device.to_string(),
                inv.group,
                inv.from.to_lowercase(),
                inv.after as i64,
                inv.welcome
            ],
        )
        .map_err(storage)?;
        Ok(true)
    })
    .await;
    match kept {
        Ok(true) => {
            if let Ok(frame) = cartopolis_core::encode_app_frame(&NetMessage::E2eeInvite {
                group,
                from,
                after,
                welcome,
            }) {
                state.chat.deliver_to_device(device, &frame.into());
            }
            Json(Done {}).into_response()
        }
        Ok(false) => refuse(StatusCode::FORBIDDEN, "no such device"),
        Err(r) => r,
    }
}

/// A device elsewhere catches up on chats this server is the hub of, through
/// its home. What its person has blocked is filtered at home, which knows.
async fn sync(
    State(state): State<HttpState>,
    uri: Uri,
    headers: HeaderMap,
    body: Bytes,
) -> Response {
    let (server, link, s) = match verified::<Sync>(&state, &uri, &headers, &body).await {
        Ok(v) => v,
        Err(r) => return r,
    };
    if on(&s.who, &server).is_none() {
        return refuse(StatusCode::FORBIDDEN, "not somebody on your server");
    }
    let here_name = link.server.clone();
    let outcome = db(&state, move |c| {
        let who = account(c, &s.who, &here_name)
            .map_err(storage)?
            .ok_or_else(|| refuse(StatusCode::BAD_REQUEST, "not an address elsewhere"))?;
        if !own_device(c, &who, s.device).map_err(storage)? {
            return Err(refuse(StatusCode::FORBIDDEN, "not their device"));
        }
        let cursors: Vec<(Vec<u8>, u64)> = s
            .cursors
            .into_iter()
            .filter(|cur| {
                address::group_home(&cur.group).is_some_and(|h| h.eq_ignore_ascii_case(&here_name))
            })
            .map(|cur| (cur.group, cur.after))
            .collect();
        let (entries, _invites) =
            crate::e2ee::sync(c, &who, s.device, &cursors, &Default::default()).map_err(storage)?;
        Ok(entries
            .iter()
            .map(|e| Entry::of(e, &here_name))
            .collect::<Vec<_>>())
    })
    .await;
    match outcome {
        Ok(entries) => Json(Synced { entries }).into_response(),
        Err(r) => r,
    }
}

fn now_ms() -> u64 {
    crate::fediverse::now_ms() as u64
}
