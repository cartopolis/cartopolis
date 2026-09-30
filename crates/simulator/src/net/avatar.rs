//! The avatar: movement intents, teleports and presence.

use super::{ConnCtx, FrameTransport, MIN_INTENT_INTERVAL};
use cartopolis_core::{Emote, GeoAnchor};
use glam::Vec3;
use std::time::Instant;

/// A `ClientIntent`, as the sim reads it.
pub(super) struct Intent {
    pub move_x: f32,
    pub move_z: f32,
    pub display_yaw: f32,
    pub flying: bool,
    pub fly_up: bool,
    pub fly_down: bool,
    pub jump: bool,
    pub emote: Option<Emote>,
    pub seated: Option<[f32; 2]>,
    pub geo: Option<GeoAnchor>,
}

pub(super) async fn on_intent<T: FrameTransport>(
    ctx: &mut ConnCtx<T>,
    intent: Intent,
) -> anyhow::Result<()> {
    // The rate limit drops surplus intents, but a jump and an emote are *edges* —
    // dropping one loses the action outright rather than delaying it by a frame,
    // so they are never rate limited away.
    if ctx.gates.last_intent.elapsed() < MIN_INTENT_INTERVAL
        && !intent.jump
        && intent.emote.is_none()
    {
        return Ok(());
    }
    ctx.gates.last_intent = Instant::now();
    let mut w = ctx.world.write().await;
    w.apply_intent(
        ctx.avatar_id,
        intent.move_x,
        intent.move_z,
        intent.display_yaw,
        intent.flying,
        intent.fly_up,
        intent.fly_down,
        intent.jump,
        intent.emote,
        intent.seated,
        intent.geo,
    );
    Ok(())
}

/// Deliberately not rate-limited like intents: a teleport is a deliberate act,
/// not a stream, and dropping one is not corrected by the next frame.
pub(super) async fn on_teleport<T: FrameTransport>(
    ctx: &mut ConnCtx<T>,
    position: Vec3,
) -> anyhow::Result<()> {
    ctx.world
        .write()
        .await
        .teleport_avatar(ctx.avatar_id, position);
    Ok(())
}

/// Deliberately not rate-limited, unlike intents and chat. It is an *edge* — one
/// message when the app leaves the screen and one when it comes back — and a
/// dropped one is not corrected by the next: refusing the "I'm back" would leave a
/// foregrounded client with no world state and nothing to make it ask again. A
/// client that flapped the flag would only be choosing whether to receive its own
/// snapshots.
pub(super) fn on_presence<T: FrameTransport>(ctx: &mut ConnCtx<T>, active: bool) {
    if ctx.presence_active != active {
        ctx.presence_active = active;
        tracing::debug!(avatar_id = ctx.avatar_id, active, "presence");
    }
}
