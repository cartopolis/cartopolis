//! Contacts and message requests (protocol 27). `crate::contacts` holds the
//! rules; these handlers look people up, apply them on a blocking thread, and
//! push both sides' new state.

use super::{
    now_millis, server_error, take_token, ConnCtx, FrameTransport, LOOKUP_BURST, MAX_USERNAME_LEN,
    MAX_WHERE_PEOPLE, MIN_LOOKUP_INTERVAL, MIN_WHERE_INTERVAL,
};
use crate::chat::ChatHub;
use crate::contacts::{Refusal, Sent};
use crate::db::{self, SharedConn};
use bytes::Bytes;
use cartopolis_core::{encode_app_frame, NetMessage, RequestPolicy};
use std::sync::Arc;

/// Build `account`'s contact state and hand it to every connection it has
/// (`contacts`). Sent whole after anything in it changes, including a change
/// the *other* person made, so each side's lists never need merging.
pub(super) async fn push_contacts(conn: &SharedConn, chat: &Arc<ChatHub>, account: &str) {
    let Some(msg) = contacts_message(conn, chat, account).await else {
        return;
    };
    if let Ok(frame) = encode_app_frame(&msg) {
        chat.deliver_to_account(account, &Bytes::from(frame));
    }
}

/// `account`'s contact state as a message, read on the blocking pool; `None`
/// if the account is gone or the read failed (logged).
pub(super) async fn contacts_message(
    conn: &SharedConn,
    chat: &Arc<ChatHub>,
    account: &str,
) -> Option<NetMessage> {
    let hub = chat.clone();
    let who = account.to_string();
    db::call(conn, move |c| -> Option<NetMessage> {
        let me = crate::contacts::account_by_id(c, &who).ok()??;
        let online = |a: &str| hub.account_online(a);
        match crate::contacts::state(c, &me, &online, now_millis()) {
            Ok(m) => Some(m),
            Err(e) => {
                tracing::warn!(error = %e, "could not read contacts");
                None
            }
        }
    })
    .await
    .ok()
    .flatten()
}

/// Tell `account`'s contacts who are online about it: it came or went.
pub(super) async fn push_contacts_to_contacts(
    conn: &SharedConn,
    chat: &Arc<ChatHub>,
    account: &str,
) {
    let who = account.to_string();
    let ids = db::call(conn, move |c| {
        crate::contacts::contact_ids(c, &who).unwrap_or_default()
    })
    .await
    .unwrap_or_default();
    for id in ids {
        if chat.account_online(&id) {
            push_contacts(conn, chat, &id).await;
        }
    }
}

/// The signed-in account, as `contacts` needs it.
fn own_account(
    c: &rusqlite::Connection,
    principal: &str,
) -> Result<crate::contacts::Account, Refusal> {
    crate::contacts::account_by_id(c, principal)?
        .ok_or_else(|| Refusal::Storage("this account is gone".into()))
}

/// Send a contact request from `principal` to `subject`; the outcome and the
/// target's account id.
fn contact_request(
    c: &rusqlite::Connection,
    principal: &str,
    subject: &str,
    met: Option<cartopolis_core::MetDto>,
    link_code: Option<&str>,
) -> Result<(Sent, String), Refusal> {
    let me = own_account(c, principal)?;
    let target = crate::contacts::account_by_name(c, subject)?
        .ok_or_else(|| Refusal::NoSuchPerson(subject.to_string()))?;
    let sent = crate::contacts::request(c, &me, &target, met, link_code, now_millis())?;
    Ok((sent, target.id))
}

/// The people near `principal`'s avatar worth listing on the contacts screen:
/// the ones a request would reach, and the ones already connected to you.
/// Somebody who takes requests only through their link is left off — the
/// screen is an invitation to say hi, and they asked not to get one this way.
fn nearby_people(
    c: &rusqlite::Connection,
    principal: &str,
    near: Vec<(String, f32)>,
) -> Vec<cartopolis_core::NearbyPersonDto> {
    let Ok(me) = own_account(c, principal) else {
        return Vec::new();
    };
    near.into_iter()
        .filter_map(|(name, distance_m)| {
            let them = crate::contacts::account_by_name(c, &name).ok()??;
            let relation = crate::contacts::relation(c, &me.id, &them.id).ok()?;
            let may_request = relation == cartopolis_core::Relation::Nobody
                && crate::contacts::policy_allows(c, &me, &them, true, None)
                    .ok()?
                    .is_ok();
            (may_request || relation != cartopolis_core::Relation::Nobody).then_some(
                cartopolis_core::NearbyPersonDto {
                    username: them.username,
                    distance_m,
                    relation,
                    may_request,
                },
            )
        })
        .collect()
}

pub(super) async fn on_request<T: FrameTransport>(
    ctx: &mut ConnCtx<T>,
    request_id: u32,
    subject: String,
    link_code: Option<String>,
) -> anyhow::Result<()> {
    let subject = subject.trim().to_string();
    if subject.is_empty() || subject.chars().count() > MAX_USERNAME_LEN {
        return Ok(());
    }
    let met = ctx.world.read().await.met(&ctx.display_name, &subject);
    let principal = ctx.principal_key.clone();
    let target_name = subject.clone();
    let result = match db::call(&ctx.conn, move |c| {
        contact_request(c, &principal, &target_name, met, link_code.as_deref())
    })
    .await
    {
        Ok(result) => result,
        Err(_) => return Ok(()),
    };
    match result {
        Ok((sent, target)) => {
            push_contacts(&ctx.conn, &ctx.chat, &ctx.principal_key).await;
            if sent != Sent::Unchanged {
                push_contacts(&ctx.conn, &ctx.chat, &target).await;
            }
            Ok(())
        }
        Err(refusal) => {
            // "Only through their link" would confirm the account exists to
            // somebody it asked not to be found by; unless you are standing next
            // to it.
            let refusal = match refusal {
                Refusal::OnlyLink(name) if met.is_none() => Refusal::NoSuchPerson(name),
                r => r,
            };
            ctx.send(&server_error(request_id, 403, refusal.to_string()))
                .await
        }
    }
}

pub(super) async fn on_answer<T: FrameTransport>(
    ctx: &mut ConnCtx<T>,
    request_id: u32,
    subject: String,
    accept: bool,
) -> anyhow::Result<()> {
    let principal = ctx.principal_key.clone();
    let result = match db::call(&ctx.conn, move |c| -> Result<String, Refusal> {
        let me = own_account(c, &principal)?;
        let from = crate::contacts::account_by_name(c, &subject)?
            .ok_or_else(|| Refusal::NoRequest(subject.clone()))?;
        crate::contacts::answer(c, &me, &from, accept, now_millis())?;
        Ok(from.id)
    })
    .await
    {
        Ok(result) => result,
        Err(_) => return Ok(()),
    };
    match result {
        Ok(from) => {
            push_contacts(&ctx.conn, &ctx.chat, &ctx.principal_key).await;
            // Only an acceptance is news to the sender: a decline must look like
            // nothing happened.
            if accept {
                push_contacts(&ctx.conn, &ctx.chat, &from).await;
            }
            Ok(())
        }
        Err(refusal) => {
            ctx.send(&server_error(request_id, 404, refusal.to_string()))
                .await
        }
    }
}

pub(super) async fn on_remove<T: FrameTransport>(
    ctx: &mut ConnCtx<T>,
    subject: String,
) -> anyhow::Result<()> {
    let principal = ctx.principal_key.clone();
    let other = match db::call(&ctx.conn, move |c| {
        match crate::contacts::account_by_name(c, &subject) {
            Ok(Some(them)) => crate::contacts::remove(c, &principal, &them.id)
                .map(|()| them.id)
                .ok(),
            _ => None,
        }
    })
    .await
    {
        Ok(other) => other,
        Err(_) => return Ok(()),
    };
    push_contacts(&ctx.conn, &ctx.chat, &ctx.principal_key).await;
    if let Some(other) = other {
        push_contacts(&ctx.conn, &ctx.chat, &other).await;
    }
    Ok(())
}

/// The settings as a `SetContactSettings` carries them.
pub(super) struct Settings {
    pub policy: RequestPolicy,
    pub show_online: bool,
    pub notify_requests: bool,
    pub separate_new_accounts: bool,
    pub reset_link: bool,
}

pub(super) async fn on_settings<T: FrameTransport>(
    ctx: &mut ConnCtx<T>,
    settings: Settings,
) -> anyhow::Result<()> {
    let principal = ctx.principal_key.clone();
    let written = db::call(&ctx.conn, move |c| {
        crate::contacts::set_settings(
            c,
            &principal,
            settings.policy,
            settings.show_online,
            settings.notify_requests,
            settings.separate_new_accounts,
            settings.reset_link,
        )
    })
    .await;
    match written {
        Ok(Ok(())) => {}
        Ok(Err(e)) => tracing::warn!(error = %e, "contact settings write failed"),
        Err(_) => return Ok(()),
    }
    push_contacts(&ctx.conn, &ctx.chat, &ctx.principal_key).await;
    push_contacts_to_contacts(&ctx.conn, &ctx.chat, &ctx.principal_key).await;
    Ok(())
}

pub(super) async fn on_lookup<T: FrameTransport>(
    ctx: &mut ConnCtx<T>,
    request_id: u32,
    subject: Option<String>,
    tapped: Option<u64>,
) -> anyhow::Result<()> {
    // A lookup says whether a username exists, so an unthrottled loop would list
    // every account on the server.
    if !take_token(&mut ctx.gates.lookup, MIN_LOOKUP_INTERVAL, LOOKUP_BURST) {
        return ctx
            .send(&server_error(
                request_id,
                429,
                "too many lookups at once — try again in a moment",
            ))
            .await;
    }
    let (name, met, distance) = {
        let w = ctx.world.read().await;
        let name = match (subject, tapped) {
            (Some(n), _) => Some(n.trim().to_string()),
            (None, Some(id)) => w.avatar_name(id),
            (None, None) => None,
        };
        let met = name.as_deref().and_then(|n| w.met(&ctx.display_name, n));
        let distance = name
            .as_deref()
            .and_then(|n| w.distance_between(&ctx.display_name, n));
        (name, met, distance)
    };
    let person = match name.filter(|n| !n.is_empty() && n.chars().count() <= MAX_USERNAME_LEN) {
        None => None,
        Some(n) => {
            let principal = ctx.principal_key.clone();
            db::call(&ctx.conn, move |c| {
                let me = own_account(c, &principal).ok()?;
                let them = crate::contacts::account_by_name(c, &n).ok()??;
                crate::contacts::person(c, &me, &them, met, distance, now_millis()).ok()?
            })
            .await
            .ok()
            .flatten()
        }
    };
    ctx.send(&NetMessage::PersonInfo { request_id, person })
        .await
}

/// Where the people in a chat are, for its World view (protocol 30): only the
/// asker's contacts who show that they are online (`contacts::visible_to`).
pub(super) async fn on_where<T: FrameTransport>(
    ctx: &mut ConnCtx<T>,
    request_id: u32,
    mut usernames: Vec<String>,
) -> anyhow::Result<()> {
    let now = std::time::Instant::now();
    if now.duration_since(ctx.gates.last_where) < MIN_WHERE_INTERVAL {
        return Ok(());
    }
    ctx.gates.last_where = now;
    usernames.retain(|n| !n.trim().is_empty() && n.chars().count() <= MAX_USERNAME_LEN);
    usernames.truncate(MAX_WHERE_PEOPLE);
    let principal = ctx.principal_key.clone();
    let visible = match db::call(&ctx.conn, move |c| {
        crate::contacts::visible_to(c, &principal, &usernames).unwrap_or_default()
    })
    .await
    {
        Ok(v) => v,
        Err(_) => return Ok(()),
    };
    let people = ctx
        .world
        .read()
        .await
        .whereabouts(&visible)
        .into_iter()
        .map(
            |(username, avatar_id, geo)| cartopolis_core::PersonWhereDto {
                username,
                avatar_id,
                latitude: geo.latitude,
                longitude: geo.longitude,
            },
        )
        .collect();
    ctx.send(&NetMessage::PeopleWhere { request_id, people })
        .await
}

pub(super) async fn on_nearby<T: FrameTransport>(
    ctx: &mut ConnCtx<T>,
    request_id: u32,
) -> anyhow::Result<()> {
    let near = ctx.world.read().await.near(&ctx.display_name);
    let principal = ctx.principal_key.clone();
    let people = match db::call(&ctx.conn, move |c| nearby_people(c, &principal, near)).await {
        Ok(people) => people,
        Err(_) => return Ok(()),
    };
    ctx.send(&NetMessage::NearbyPeople { request_id, people })
        .await
}
