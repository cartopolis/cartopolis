//! Hands near a prim (protocol 36, `cartopolis_core::tone`).
//!
//! A client reports its user's hand to a prim whose script senses hands, while
//! the hand is within that prim's range. The simulator checks the range and
//! passes the hand to the script as `on_hand`, with who it belongs to; what a
//! hand means — including whose counts — is the script's business.

use super::{take_token, ConnCtx, FrameTransport};
use crate::scripting::ScriptEvent;
use cartopolis_core::HandPose;
use std::time::Duration;

/// The per-connection bucket's refill: 100 reports a second, which is a hand
/// near four prims at once at `tone::HAND_HZ` with room for a client's timer
/// jitter. Per connection rather than per prim, so it bounds what one client
/// can make the script host do whatever it claims to be near.
pub(super) const HAND_INTERVAL: Duration = Duration::from_millis(10);
/// How many reports may arrive at once.
pub(super) const HAND_BURST: u32 = 8;

pub(super) async fn on_hand<T: FrameTransport>(
    ctx: &mut ConnCtx<T>,
    pose: HandPose,
) -> anyhow::Result<()> {
    if !take_token(&mut ctx.gates.hand, HAND_INTERVAL, HAND_BURST) {
        return Ok(());
    }
    if ctx.world.read().await.hand_in_range(&pose) {
        ctx.scripts.event(
            pose.prim_id,
            ScriptEvent::Hand {
                avatar_id: ctx.avatar_id,
                avatar_name: ctx.display_name.clone(),
                offset: pose.offset,
                precise: pose.precise,
            },
        );
    }
    Ok(())
}
