//! Avatar looks (protocol 42): a client's own, kept with its account and handed
//! to everyone; and everyone's, to a client that has just arrived.

use super::{take_token, ConnCtx, FrameTransport};
use cartopolis_core::{AvatarLook, NetMessage};
use std::time::Duration;

/// A look is saved when its owner presses Apply, so a few a minute is generous;
/// the bucket is what stops a client turning it into a broadcast storm.
pub(super) const LOOK_INTERVAL: Duration = Duration::from_secs(2);
pub(super) const LOOK_BURST: u32 = 5;

/// A client saved its look: keep it with the account, wear it in the world,
/// and tell every client.
pub(super) async fn on_look<T: FrameTransport>(
    ctx: &mut ConnCtx<T>,
    look: AvatarLook,
) -> anyhow::Result<()> {
    if !look.is_reasonable() || !take_token(&mut ctx.gates.look, LOOK_INTERVAL, LOOK_BURST) {
        return Ok(());
    }
    let principal = ctx.principal_key.clone();
    let stored = look.clone();
    let written = crate::db::call(&ctx.conn, move |c| {
        crate::looks::save(c, &principal, &stored)
    })
    .await;
    if let Err(e) = written.and_then(|r| r) {
        tracing::warn!("avatar look not saved: {e}");
    }
    ctx.world
        .write()
        .await
        .set_avatar_look(ctx.avatar_id, look.clone());
    ctx.prims.broadcast(&NetMessage::AvatarLookOf {
        avatar_id: ctx.avatar_id,
        look,
    });
    Ok(())
}

/// Just after the handshake: wear the account's saved look, tell everyone, and
/// hand the newcomer every online avatar's look.
pub(super) async fn on_arrival<T: FrameTransport>(ctx: &mut ConnCtx<T>) -> anyhow::Result<()> {
    // A meeting guest is shown no world, so it has no avatar to dress and no
    // use for anybody else's look.
    if ctx.guest {
        return Ok(());
    }
    let principal = ctx.principal_key.clone();
    let saved = crate::db::call(&ctx.conn, move |c| crate::looks::load(c, &principal))
        .await
        .and_then(|r| r)
        .unwrap_or_else(|e| {
            tracing::warn!("avatar look not read: {e}");
            None
        });
    if let Some(look) = saved {
        ctx.world
            .write()
            .await
            .set_avatar_look(ctx.avatar_id, look.clone());
        ctx.prims.broadcast(&NetMessage::AvatarLookOf {
            avatar_id: ctx.avatar_id,
            look,
        });
    }
    let everyone = ctx.world.read().await.avatar_looks();
    for (avatar_id, look) in everyone {
        if avatar_id != ctx.avatar_id {
            ctx.feed(&NetMessage::AvatarLookOf { avatar_id, look })
                .await?;
        }
    }
    Ok(())
}
