//! Prims over the wire: build, edit, delete, script and touch.
//!
//! The decisions — who may, what fits, what gets stored and announced — are
//! `crate::prims::PrimService`'s, shared with the authoring API. What is here is
//! the wire's half: the per-connection rate limits, and how a refusal is told to
//! the client that asked.

use super::{
    now_millis, refuse_and_correct, server_error, take_token, ConnCtx, FrameTransport,
    MIN_SCRIPT_STORE_INTERVAL, MIN_TOUCH_INTERVAL, SCRIPT_STORE_BURST,
};
use crate::prims::{ModelCheck, PrimError, PrimFields, PrimSeed};
use cartopolis_core::NetMessage;
use std::time::Instant;

/// A player building something. The model is checked against the store (a hash
/// this server does not hold is refused rather than stored), and the builder
/// becomes the owner. A refusal by the account's roles is a 403; every other
/// refusal a 400, the path a failed create has always used.
pub(super) async fn on_create<T: FrameTransport>(
    ctx: &mut ConnCtx<T>,
    request_id: u32,
    seed: PrimSeed,
    fields: PrimFields,
) -> anyhow::Result<()> {
    let created = ctx
        .prims
        .create(&ctx.actor, seed, move |_| Ok(fields), ModelCheck::Required)
        .await;
    match created {
        Ok(_) => {
            ctx.stats
                .event(crate::stats::Event::PrimCreated, None, None);
            tracing::debug!(request_id, "prim created and broadcast");
        }
        Err(e) => {
            let code = if matches!(e, PrimError::Forbidden(_)) {
                403
            } else {
                400
            };
            let message = e.to_string();
            ctx.send(&server_error(request_id, code, message.clone()))
                .await?;
            tracing::warn!(request_id, error = %message, "prim creation failed");
        }
    }
    Ok(())
}

/// Tell the client an operation on an existing prim did not happen.
///
/// A refusal on permission — the prim is somebody else's, or the account's roles
/// do not allow the operation — goes back as a 403 **with the prim's truth**,
/// because the client applied the edit before it asked (`refuse_and_correct`).
/// Anything else is a plain `ServerError`.
async fn refuse<T: FrameTransport>(
    ctx: &mut ConnCtx<T>,
    request_id: u32,
    prim_id: i64,
    e: PrimError,
    what: &str,
) -> anyhow::Result<()> {
    let message = e.to_string();
    match e {
        PrimError::NotYours(_) | PrimError::Forbidden(_) => {
            let truth = ctx.world.read().await.prim(prim_id).cloned();
            refuse_and_correct(&mut ctx.framed, request_id, truth, message.clone()).await?;
        }
        PrimError::NotFound(_) | PrimError::Storage(_) | PrimError::Refused(_) => {
            let code = match e {
                PrimError::NotFound(_) => 404,
                PrimError::Storage(_) => 500,
                _ => 400,
            };
            ctx.send(&server_error(request_id, code, message.clone()))
                .await?;
        }
    }
    tracing::warn!(request_id, prim_id, error = %message, "{what} refused");
    Ok(())
}

pub(super) async fn on_update<T: FrameTransport>(
    ctx: &mut ConnCtx<T>,
    request_id: u32,
    prim_id: i64,
    fields: PrimFields,
) -> anyhow::Result<()> {
    let updated = ctx
        .prims
        .update(
            &ctx.actor,
            prim_id,
            move |_| Ok(fields),
            ModelCheck::Required,
        )
        .await;
    match updated {
        Ok(_) => {
            // Updates arrive at the gizmo's drag rate: one person working on
            // one prim is one edit a day, however many frames it took.
            let key = format!("{}:{prim_id}", ctx.principal_key);
            ctx.stats.event_once(crate::stats::Event::PrimEdited, &key);
            tracing::debug!(request_id, prim_id, "prim updated and broadcast");
        }
        Err(e) => refuse(ctx, request_id, prim_id, e, "prim update").await?,
    }
    Ok(())
}

/// Start or stop a clip on a prim (protocol 28).
pub(super) async fn on_set_video<T: FrameTransport>(
    ctx: &mut ConnCtx<T>,
    request_id: u32,
    prim_id: i64,
    video: Option<cartopolis_core::SetVideo>,
) -> anyhow::Result<()> {
    match ctx
        .prims
        .set_video(&ctx.actor, prim_id, video, now_millis())
        .await
    {
        Ok(_) => tracing::debug!(request_id, prim_id, "prim video set and broadcast"),
        Err(e) => refuse(ctx, request_id, prim_id, e, "prim video").await?,
    }
    Ok(())
}

pub(super) async fn on_set_physical<T: FrameTransport>(
    ctx: &mut ConnCtx<T>,
    request_id: u32,
    prim_id: i64,
    physical: bool,
) -> anyhow::Result<()> {
    match ctx.prims.set_physical(&ctx.actor, prim_id, physical).await {
        Ok(_) => tracing::debug!(
            request_id,
            prim_id,
            physical,
            "prim physics set and broadcast"
        ),
        Err(e) => refuse(ctx, request_id, prim_id, e, "prim physics").await?,
    }
    Ok(())
}

pub(super) async fn on_delete<T: FrameTransport>(
    ctx: &mut ConnCtx<T>,
    request_id: u32,
    prim_id: i64,
) -> anyhow::Result<()> {
    match ctx.prims.delete(&ctx.actor, prim_id).await {
        Ok(()) => tracing::debug!(request_id, prim_id, "prim deleted and broadcast"),
        Err(e) => refuse(ctx, request_id, prim_id, e, "prim deletion").await?,
    }
    Ok(())
}

/// Send a prim's script to the editor that asked — if this account may read it:
/// its own prim's always, anybody's with `script_read_any` (or `script_edit_any`).
/// It used to go to any connected client that asked for any prim.
pub(super) async fn on_get_script<T: FrameTransport>(
    ctx: &mut ConnCtx<T>,
    request_id: u32,
    prim_id: i64,
) -> anyhow::Result<()> {
    let allowed = ctx.actor.may_read_script(&*ctx.world.read().await, prim_id);
    if !allowed {
        ctx.send(&server_error(
            request_id,
            403,
            "this account may not read that prim's script".to_string(),
        ))
        .await?;
        tracing::debug!(request_id, prim_id, "script read refused");
        return Ok(());
    }
    let source = ctx
        .prims
        .script_source(prim_id)
        .await
        .ok()
        .flatten()
        .unwrap_or_default();
    let running = ctx.world.read().await.is_scripted(prim_id);
    ctx.send(&NetMessage::PrimScript {
        prim_id,
        source,
        running,
    })
    .await?;
    tracing::debug!(request_id, prim_id, "script sent to editor");
    Ok(())
}

pub(super) async fn on_set_script<T: FrameTransport>(
    ctx: &mut ConnCtx<T>,
    request_id: u32,
    prim_id: i64,
    source: String,
) -> anyhow::Result<()> {
    let within_rate = take_token(
        &mut ctx.gates.script_store,
        MIN_SCRIPT_STORE_INTERVAL,
        SCRIPT_STORE_BURST,
    );
    let outcome = if within_rate {
        ctx.prims
            .set_script(&ctx.actor, prim_id, &source)
            .await
            .map_err(|e| e.to_string())
    } else {
        Err("saving too fast — try again in a moment".to_string())
    };
    match outcome {
        Ok(running) => {
            // Acknowledged to the author, not broadcast.
            //
            // This used to go to `tx_snap` — the whole-world channel — so saving
            // a script sent its **source** to every connected client. That is a
            // disclosure (a script is the one part of a prim its author may not
            // want read) and a 64 KiB frame per save against a 256-slot
            // broadcast, which a loop of saves turns into everybody else's
            // snapshots being dropped as `Lagged`.
            //
            // What it bought was a second editor open on the same prim following
            // along. That editor can ask: `GetPrimScript` is one message and it is
            // the same message it opened with.
            let source = if running {
                source.trim().to_string()
            } else {
                String::new()
            };
            ctx.send(&NetMessage::PrimScript {
                prim_id,
                source,
                running,
            })
            .await?;
        }
        Err(e) => {
            // A refusal is console text, not a modal: the editor is where the
            // author is looking, and it is the only place with room to say which
            // limit was hit.
            ctx.send(&NetMessage::ScriptOutput {
                prim_id,
                level: cartopolis_core::ScriptLevel::Error,
                text: e.clone(),
                at_ms: now_millis(),
            })
            .await?;
            tracing::warn!(request_id, prim_id, error = %e, "script store refused");
        }
    }
    Ok(())
}

pub(super) async fn on_touch<T: FrameTransport>(
    ctx: &mut ConnCtx<T>,
    request_id: u32,
    prim_id: i64,
) -> anyhow::Result<()> {
    if ctx.gates.last_touch.elapsed() < MIN_TOUCH_INTERVAL {
        return Ok(());
    }
    ctx.gates.last_touch = Instant::now();
    // Only a prim that has a script gets an event; a touch on plain geometry is
    // not an error, it is simply nothing happening.
    if ctx.world.read().await.is_scripted(prim_id) {
        ctx.scripts.event(
            prim_id,
            crate::scripting::ScriptEvent::Touch {
                avatar_id: ctx.avatar_id,
                avatar_name: ctx.display_name.clone(),
            },
        );
        tracing::debug!(
            request_id,
            prim_id,
            avatar_id = ctx.avatar_id,
            "prim touched"
        );
    }
    Ok(())
}
