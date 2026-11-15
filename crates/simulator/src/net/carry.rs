//! Picking things up, throwing them and putting them down (protocol 48,
//! `docs/notes/physical-prims.md`).
//!
//! The world decides everything (`SimWorld::grab_prim`): what may be carried,
//! whether it is within reach of where the sim holds the avatar, and whether
//! somebody else has it. A grant is not answered — it shows up as
//! `AvatarStateDto::holding` in the next snapshot, to everyone at once — and a
//! refusal is a `ServerError` to the asker alone.

use super::{server_error, take_token, ConnCtx, FrameTransport};
use std::time::Duration;

/// The bucket's refill: five a second. Somebody mashing E picks up and throws
/// about that fast; a client sending more is not a person.
pub(super) const CARRY_INTERVAL: Duration = Duration::from_millis(200);
/// How many may arrive at once: a pick-up and a throw in one breath.
pub(super) const CARRY_BURST: u32 = 4;

pub(super) async fn on_grab<T: FrameTransport>(
    ctx: &mut ConnCtx<T>,
    request_id: u32,
    prim_id: i64,
) -> anyhow::Result<()> {
    if !take_token(&mut ctx.gates.carry, CARRY_INTERVAL, CARRY_BURST) {
        return Ok(());
    }
    let refused = ctx.world.write().await.grab_prim(ctx.avatar_id, prim_id);
    match refused {
        Ok(()) => tracing::debug!(avatar_id = ctx.avatar_id, prim_id, "picked up"),
        Err(why) => {
            tracing::debug!(avatar_id = ctx.avatar_id, prim_id, ?why, "pick-up refused");
            ctx.send(&server_error(request_id, why.code(), why.message()))
                .await?;
        }
    }
    Ok(())
}

/// Not answered either way: empty hands letting go is nothing to report.
///
/// Not rate limited, unlike the grab: it can only ever empty this avatar's own
/// hands, costs one map lookup, and a dropped one would leave the thing stuck
/// in them with nothing to make the client ask again.
pub(super) async fn on_release<T: FrameTransport>(
    ctx: &mut ConnCtx<T>,
    request_id: u32,
    throw: bool,
) -> anyhow::Result<()> {
    let let_go = ctx.world.write().await.release_prim(ctx.avatar_id, throw);
    tracing::debug!(
        avatar_id = ctx.avatar_id,
        request_id,
        ?let_go,
        throw,
        "let go"
    );
    Ok(())
}
