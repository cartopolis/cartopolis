//! Greeting another avatar: a handshake or a high five (protocol 49,
//! `docs/notes/paired-gestures.md`).
//!
//! An offer goes to the one offered, an acceptance starts the gesture on both
//! (`SimWorld::accept_greeting`), and a refusal is a `ServerError` to the asker
//! alone. A block either way refuses both the offer and the acceptance — a
//! greeting is something two people do together, and a block is one of them
//! saying no to everything of the other's (`docs/notes/user-safety.md`).

use super::{server_error, take_token, ConnCtx, FrameTransport};
use crate::chat::Lane;
use cartopolis_core::{encode_app_frame, Greeting, NetMessage};
use std::time::Duration;

/// The offer bucket's refill: one every two seconds. Greeting is a social act
/// made a few times a minute; a client offering faster is pestering.
pub(super) const GREET_INTERVAL: Duration = Duration::from_secs(2);
/// How many may be made at once: changing your mind from a handshake to a
/// high five, and once more.
pub(super) const GREET_BURST: u32 = 3;

/// Refused because of a block, either way. Worded to say nothing about who
/// blocked whom.
const BLOCKED: &str = "you cannot greet them";

/// Whether this connection and avatar `other`, signed in as `name`, are kept
/// apart by a block either way. `None` when `other` is nobody signed in.
///
/// Not async, and handed the name rather than looking it up: a future holding
/// `&ConnCtx` across an await would need the connection to be `Sync`, which
/// the WebSocket carrier is not.
fn blocked_with<T>(ctx: &ConnCtx<T>, other: u64, name: Option<String>) -> Option<bool> {
    let name = name?;
    Some(
        ctx.chat
            .blocked_between(ctx.avatar_id, &ctx.display_name, other, &name),
    )
}

pub(super) async fn on_offer<T: FrameTransport>(
    ctx: &mut ConnCtx<T>,
    request_id: u32,
    to: u64,
    greeting: Greeting,
) -> anyhow::Result<()> {
    if !take_token(&mut ctx.gates.greet, GREET_INTERVAL, GREET_BURST) {
        ctx.send(&server_error(request_id, 429, "one moment"))
            .await?;
        return Ok(());
    }
    if to != ctx.avatar_id {
        let name = ctx.world.read().await.avatar_name(to);
        match blocked_with(ctx, to, name) {
            Some(true) => {
                ctx.send(&server_error(request_id, 403, BLOCKED)).await?;
                return Ok(());
            }
            Some(false) => {}
            None => {
                let why = crate::state::GreetRefusal::NoSuchAvatar;
                ctx.send(&server_error(request_id, why.code(), why.message()))
                    .await?;
                return Ok(());
            }
        }
    }
    let made = ctx
        .world
        .write()
        .await
        .offer_greeting(ctx.avatar_id, to, greeting);
    match made {
        Ok(()) => {
            tracing::debug!(from = ctx.avatar_id, to, ?greeting, "greeting offered");
            let frame = bytes::Bytes::from(encode_app_frame(&NetMessage::GreetingOffered {
                from: ctx.avatar_id,
                name: ctx.display_name.clone(),
                greeting: Some(greeting),
            })?);
            ctx.chat
                .deliver_to(to, &ctx.display_name, Lane::Text, frame);
        }
        Err(why) => {
            ctx.send(&server_error(request_id, why.code(), why.message()))
                .await?;
        }
    }
    Ok(())
}

/// Not rate limited: it can only take an offer somebody else made, which their
/// own bucket already bounds, and a dropped one is a greeting that never was.
pub(super) async fn on_accept<T: FrameTransport>(
    ctx: &mut ConnCtx<T>,
    request_id: u32,
    from: u64,
) -> anyhow::Result<()> {
    // A block made since the offer ends it too.
    let name = ctx.world.read().await.avatar_name(from);
    if blocked_with(ctx, from, name) == Some(true) {
        ctx.send(&server_error(request_id, 403, BLOCKED)).await?;
        return Ok(());
    }
    let started = ctx.world.write().await.accept_greeting(ctx.avatar_id, from);
    match started {
        Ok(s) => {
            tracing::debug!(from, to = ctx.avatar_id, greeting = ?s.greeting, "greeting started");
            ctx.send(&NetMessage::GreetingStarted {
                greeting: s.greeting,
                partner: from,
                yaw: s.yaw_to,
            })
            .await?;
            let frame = bytes::Bytes::from(encode_app_frame(&NetMessage::GreetingStarted {
                greeting: s.greeting,
                partner: ctx.avatar_id,
                yaw: s.yaw_from,
            })?);
            ctx.chat
                .deliver_to(from, &ctx.display_name, Lane::Text, frame);
        }
        Err(why) => {
            ctx.send(&server_error(request_id, why.code(), why.message()))
                .await?;
        }
    }
    Ok(())
}

/// Tell the avatars this connection's avatar had offered a greeting that it
/// is withdrawn: it walked off or sat down (`SimWorld::take_withdrawn`).
pub(super) fn tell_withdrawn<T: FrameTransport>(
    ctx: &ConnCtx<T>,
    withdrawn: Vec<(u64, u64)>,
) -> anyhow::Result<()> {
    for (to, from) in withdrawn {
        let frame = bytes::Bytes::from(encode_app_frame(&NetMessage::GreetingOffered {
            from,
            name: ctx.display_name.clone(),
            greeting: None,
        })?);
        ctx.chat
            .deliver_to(to, &ctx.display_name, Lane::Text, frame);
    }
    Ok(())
}
