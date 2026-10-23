//! Voice calls in a private chat (protocol 26): joining, answering a
//! renegotiation, leaving. The media itself never passes through here — it goes
//! to the forwarder (`crate::calls`) over its own UDP socket.

use super::{
    blocked_lowercase, server_error, ConnCtx, FrameTransport, MIN_CALL_JOIN_INTERVAL,
    MIN_CALL_REACT_INTERVAL,
};
use crate::db;
use cartopolis_core::{CallAction, NetMessage, CALL_CONTROL_REQUEST};
use std::time::Instant;

pub(super) async fn on_join<T: FrameTransport>(
    ctx: &mut ConnCtx<T>,
    request_id: u32,
    group: Vec<u8>,
    offer: String,
    screen: Option<String>,
) -> anyhow::Result<()> {
    let Some(device) = ctx.e2ee_device else {
        return Ok(());
    };
    // Cloned rather than borrowed: every refusal below is a send on the same
    // connection, which needs all of it.
    let Some(handle) = ctx.calls.clone() else {
        return ctx
            .send(&server_error(
                request_id,
                503,
                "calls are not available on this server",
            ))
            .await;
    };
    if offer.len() > crate::calls::MAX_SDP_BYTES
        || group.len() > crate::e2ee::MAX_GROUP_ID_BYTES
        || screen.as_ref().is_some_and(|m| m.len() > 16)
    {
        return ctx.send(&server_error(request_id, 413, "too large")).await;
    }
    if ctx.gates.last_call_join.elapsed() < MIN_CALL_JOIN_INTERVAL {
        return ctx.send(&server_error(request_id, 429, "slow down")).await;
    }
    ctx.gates.last_call_join = Instant::now();
    let account = ctx.principal_key.clone();
    let id = group.clone();
    let (members, blocked, community, agent) = match db::call(&ctx.conn, move |c| {
        // Fresh rather than the list at connect, as the private-chat sync does: a
        // block made since connecting holds.
        let blocked = blocked_lowercase(c, &account);
        let community = crate::communities::community_of_topic(c, &id)
            .ok()
            .flatten();
        let agent = crate::auth::is_agent(c, &account);
        (
            crate::e2ee::members(c, &id).unwrap_or_default(),
            blocked,
            community,
            agent,
        )
    })
    .await
    {
        Ok(read) => read,
        Err(_) => return Ok(()),
    };
    // A guest is in no chat: it may join the one call its link names.
    let allowed = match &ctx.guest_of {
        Some(of) => *of == group,
        None => members.contains(&ctx.principal_key),
    };
    if !allowed {
        return ctx
            .send(&server_error(request_id, 403, "you are not in that chat"))
            .await;
    }
    // One call per connection: joining another leaves this one.
    ctx.call_seat = None;
    let joined = handle
        .join(crate::calls::Joiner {
            group: group.clone(),
            username: ctx.display_name.clone(),
            account: ctx.principal_key.clone(),
            device,
            blocked,
            members,
            offer,
            screen,
            request_id,
            guest: ctx.guest,
            agent,
            community,
        })
        .await;
    match joined {
        Ok(crate::calls::Joined::In(seat, answer)) => {
            let participant = seat.participant;
            ctx.call_seat = Some(seat);
            ctx.send(&NetMessage::CallJoined {
                request_id,
                group,
                participant,
                answer,
            })
            .await
        }
        // A host lets it in, or not: the `CallJoined` or the refusal comes
        // later, from the calls task (`crate::calls::State::control`).
        Ok(crate::calls::Joined::Waiting(seat)) => {
            let participant = seat.participant;
            ctx.call_seat = Some(seat);
            ctx.send(&NetMessage::CallWaiting {
                request_id,
                group,
                participant,
            })
            .await
        }
        Err(message) => ctx.send(&server_error(request_id, 409, message)).await,
    }
}

/// A member asks for a guest link to its chat's call. The server keeps the
/// code and which chat; the secret that seals the call's key for the guest is
/// the asking device's and is never sent here.
pub(super) async fn on_invite<T: FrameTransport>(
    ctx: &mut ConnCtx<T>,
    group: Vec<u8>,
) -> anyhow::Result<()> {
    if group.len() > crate::e2ee::MAX_GROUP_ID_BYTES {
        return Ok(());
    }
    if ctx.gates.last_call_invite.elapsed() < MIN_CALL_JOIN_INTERVAL {
        return ctx
            .send(&server_error(CALL_CONTROL_REQUEST, 429, "slow down"))
            .await;
    }
    ctx.gates.last_call_invite = Instant::now();
    let (account, id) = (ctx.principal_key.clone(), group.clone());
    let made = db::call(&ctx.conn, move |c| {
        if !crate::e2ee::members(c, &id)?.contains(&account) {
            return Ok(None);
        }
        crate::auth::create_call_link(c, &id, &account, crate::auth::unix_now_s()).map(Some)
    })
    .await;
    match made {
        Ok(Ok(Some((code, expires_ms)))) => {
            ctx.send(&NetMessage::CallInvited {
                group,
                code,
                expires_ms,
            })
            .await
        }
        Ok(Ok(None)) => {
            ctx.send(&server_error(
                CALL_CONTROL_REQUEST,
                403,
                "only somebody in the chat can invite a guest",
            ))
            .await
        }
        _ => {
            ctx.send(&server_error(
                CALL_CONTROL_REQUEST,
                500,
                "could not make a guest link",
            ))
            .await
        }
    }
}

/// A host hands a guest the call's key, sealed under the link's secret
/// (`crate::calls::State::key_drop` checks who and to whom).
pub(super) fn on_key_drop<T: FrameTransport>(
    ctx: &mut ConnCtx<T>,
    group: Vec<u8>,
    participant: u64,
    sealed: Vec<u8>,
) {
    if sealed.len() > crate::calls::MAX_KEY_DROP_BYTES {
        return;
    }
    if let (Some(handle), Some(seat)) = (&ctx.calls, &ctx.call_seat) {
        if seat.group == group {
            handle.key_drop(seat, participant, sealed);
        }
    }
}

pub(super) fn on_answer<T: FrameTransport>(ctx: &mut ConnCtx<T>, group: Vec<u8>, sdp: String) {
    if let (Some(handle), Some(seat)) = (&ctx.calls, &ctx.call_seat) {
        // Said, not dropped quietly: an answer this refuses leaves the device
        // receiving nothing, and the forwarder offering it nothing more.
        if sdp.len() > crate::calls::MAX_SDP_BYTES {
            tracing::warn!(user = %ctx.display_name, bytes = sdp.len(), "call answer too large");
        } else if seat.group == group {
            handle.answer(seat, sdp);
        }
    }
}

/// A hand, a shared screen or a host's action in the call this connection is
/// in (protocol 41). A refusal is a `ServerError` under
/// [`CALL_CONTROL_REQUEST`], which the call shows and does not end on.
pub(super) async fn on_control<T: FrameTransport>(
    ctx: &mut ConnCtx<T>,
    group: Vec<u8>,
    action: CallAction,
) -> anyhow::Result<()> {
    let (Some(handle), Some(seat)) = (ctx.calls.clone(), &ctx.call_seat) else {
        return Ok(());
    };
    if seat.group != group {
        return Ok(());
    }
    let participant = seat.participant;
    match handle.control(participant, action).await {
        Ok(()) => Ok(()),
        Err(why) => {
            ctx.send(&server_error(CALL_CONTROL_REQUEST, 403, why))
                .await
        }
    }
}

/// A sealed reaction for everyone else in this connection's call.
pub(super) fn on_react<T: FrameTransport>(
    ctx: &mut ConnCtx<T>,
    group: Vec<u8>,
    epoch: u64,
    sealed: Vec<u8>,
) {
    if sealed.len() > crate::calls::MAX_REACTION_BYTES {
        return;
    }
    // At most a few a second: each is a fan-out to the whole call.
    if ctx.gates.last_call_react.elapsed() < MIN_CALL_REACT_INTERVAL {
        return;
    }
    ctx.gates.last_call_react = Instant::now();
    if let (Some(handle), Some(seat)) = (&ctx.calls, &ctx.call_seat) {
        if seat.group == group {
            handle.react(seat, epoch, sealed);
        }
    }
}

/// Whose cameras this connection draws.
pub(super) fn on_watch<T: FrameTransport>(
    ctx: &mut ConnCtx<T>,
    group: Vec<u8>,
    cameras: Option<Vec<u64>>,
) {
    if cameras
        .as_ref()
        .is_some_and(|c| c.len() > cartopolis_calls::sfu::MAX_PARTICIPANTS)
    {
        return;
    }
    if let (Some(handle), Some(seat)) = (&ctx.calls, &ctx.call_seat) {
        if seat.group == group {
            handle.watch(seat, cameras);
        }
    }
}

pub(super) fn on_leave<T: FrameTransport>(ctx: &mut ConnCtx<T>, group: Vec<u8>) {
    if ctx.call_seat.as_ref().is_some_and(|s| s.group == group) {
        ctx.call_seat = None;
    }
}

/// Register (or take back) where this device is woken when its app is closed
/// (protocol 29, `crate::push`). A refused endpoint is dropped with a log line
/// and no answer: the client has nothing it could do differently, and the
/// check is the server's, made again before every push.
pub(super) async fn on_set_push_endpoint<T: FrameTransport>(
    ctx: &mut ConnCtx<T>,
    endpoint: Option<String>,
) -> anyhow::Result<()> {
    let account = ctx.principal_key.clone();
    match endpoint {
        Some(endpoint) => {
            if let Err(why) = crate::push::check_endpoint(&endpoint) {
                tracing::info!(%why, "push endpoint refused");
                return Ok(());
            }
            let now = super::now_millis() as i64;
            let saved = endpoint.clone();
            let _ = crate::db::call(&ctx.conn, move |c| {
                crate::db::set_push_endpoint(c, &account, &saved, now)
            })
            .await;
            ctx.push_endpoint = Some(endpoint);
        }
        None => {
            if let Some(endpoint) = ctx.push_endpoint.take() {
                let _ = crate::db::call(&ctx.conn, move |c| {
                    crate::db::remove_push_endpoint(c, &account, &endpoint)
                })
                .await;
            }
        }
    }
    Ok(())
}

/// This device declines the call ringing in `group`: tell the chat's
/// connected devices, so the caller's stop ringing out at once ("declined")
/// and this account's other devices stop ringing. Through `deliver_private`,
/// so somebody the decliner has blocked is not told anything by them.
///
/// Only a member of the chat may decline in it, and only while a call is
/// ringing there is it worth saying — but checking the second would need the
/// calls task, and a stray decline for a call that has ended does nothing on
/// any client.
pub(super) async fn on_decline<T: FrameTransport>(
    ctx: &mut ConnCtx<T>,
    group: Vec<u8>,
) -> anyhow::Result<()> {
    if group.len() > crate::e2ee::MAX_GROUP_ID_BYTES {
        return Ok(());
    }
    // One per join interval, as a join is: a decline is a fan-out to every
    // device of the chat.
    if ctx.gates.last_call_join.elapsed() < MIN_CALL_JOIN_INTERVAL {
        return Ok(());
    }
    ctx.gates.last_call_join = Instant::now();
    let id = group.clone();
    let members = match db::call(&ctx.conn, move |c| {
        crate::e2ee::members(c, &id).unwrap_or_default()
    })
    .await
    {
        Ok(m) => m,
        Err(_) => return Ok(()),
    };
    if !members.contains(&ctx.principal_key) {
        return Ok(());
    }
    let frame = bytes::Bytes::from(cartopolis_core::encode_app_frame(
        &NetMessage::CallDeclined {
            group,
            username: ctx.display_name.clone(),
        },
    )?);
    ctx.chat
        .deliver_private(&members, &ctx.display_name, false, &frame);
    Ok(())
}
