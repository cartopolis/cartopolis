//! Voice calls in a private chat (protocol 26): joining, answering a
//! renegotiation, leaving. The media itself never passes through here — it goes
//! to the forwarder (`crate::calls`) over its own UDP socket.

use super::{blocked_lowercase, server_error, ConnCtx, FrameTransport, MIN_CALL_JOIN_INTERVAL};
use crate::db;
use cartopolis_core::NetMessage;
use std::time::Instant;

pub(super) async fn on_join<T: FrameTransport>(
    ctx: &mut ConnCtx<T>,
    request_id: u32,
    group: Vec<u8>,
    offer: String,
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
    if offer.len() > crate::calls::MAX_SDP_BYTES || group.len() > crate::e2ee::MAX_GROUP_ID_BYTES {
        return ctx.send(&server_error(request_id, 413, "too large")).await;
    }
    if ctx.gates.last_call_join.elapsed() < MIN_CALL_JOIN_INTERVAL {
        return ctx.send(&server_error(request_id, 429, "slow down")).await;
    }
    ctx.gates.last_call_join = Instant::now();
    let account = ctx.principal_key.clone();
    let id = group.clone();
    let (members, blocked) = match db::call(&ctx.conn, move |c| {
        // Fresh rather than the list at connect, as the private-chat sync does: a
        // block made since connecting holds.
        let blocked = blocked_lowercase(c, &account);
        (crate::e2ee::members(c, &id).unwrap_or_default(), blocked)
    })
    .await
    {
        Ok(read) => read,
        Err(_) => return Ok(()),
    };
    if !members.contains(&ctx.principal_key) {
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
            device,
            blocked,
            members,
            offer,
        })
        .await;
    match joined {
        Ok((seat, answer)) => {
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
        Err(message) => ctx.send(&server_error(request_id, 409, message)).await,
    }
}

pub(super) fn on_answer<T: FrameTransport>(ctx: &mut ConnCtx<T>, group: Vec<u8>, sdp: String) {
    if let (Some(handle), Some(seat)) = (&ctx.calls, &ctx.call_seat) {
        if seat.group == group && sdp.len() <= crate::calls::MAX_SDP_BYTES {
            handle.answer(seat, sdp);
        }
    }
}

pub(super) fn on_leave<T: FrameTransport>(ctx: &mut ConnCtx<T>, group: Vec<u8>) {
    if ctx.call_seat.as_ref().is_some_and(|s| s.group == group) {
        ctx.call_seat = None;
    }
}
