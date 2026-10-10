//! Text chat (ADR-028): sending a line to Here or Everyone.

use super::{
    now_millis, ConnCtx, FrameTransport, GLOBAL_CHANNEL, LOCAL_CHANNEL, MAX_CHAT_LEN,
    MIN_GLOBAL_INTERVAL, MIN_LOCAL_INTERVAL,
};
use crate::db::{self, SharedConn};
use bytes::Bytes;
use cartopolis_core::{encode_app_frame, ChatScope, NetMessage};
use std::time::Instant;

/// Store a line for scrollback, on a blocking thread.
///
/// Awaited before the line is delivered, as it always was: a client connecting a
/// moment later replays Everyone from the table, and a line that was delivered but not yet stored
/// would be missing from that replay.
async fn store_line(
    conn: &SharedConn,
    channel: String,
    avatar_id: u64,
    sender: String,
    sent_at_ms: u64,
    body: String,
) -> anyhow::Result<()> {
    db::call(conn, move |c| {
        crate::db::insert_chat(
            c,
            &channel,
            avatar_id as i64,
            &sender,
            sent_at_ms as i64,
            &body,
        )
    })
    .await?
}

pub(super) async fn on_send<T: FrameTransport>(
    ctx: &mut ConnCtx<T>,
    scope: ChatScope,
    body: String,
) -> anyhow::Result<()> {
    let body: String = body.trim().chars().take(MAX_CHAT_LEN).collect();
    if body.is_empty() {
        return Ok(());
    }
    // Masked once, here, before the message is stored, echoed or fanned out — so
    // every copy that exists anywhere is the masked one, and a client cannot opt
    // out of the filter by not applying it.
    let (body, masked) = ctx.filter.mask(&body);
    if masked > 0 {
        tracing::debug!(display_name = %ctx.display_name, masked, "chat line filtered");
    }
    let avatar_id = ctx.avatar_id;
    let now_ms = now_millis();
    match scope {
        ChatScope::Global => {
            if ctx.gates.last_global.elapsed() < MIN_GLOBAL_INTERVAL {
                return Ok(());
            }
            ctx.gates.last_global = Instant::now();
            // Delivered live either way, but a store that failed means the
            // scrollback replay will silently miss this line — say so.
            if let Err(e) = store_line(
                &ctx.conn,
                GLOBAL_CHANNEL.to_string(),
                avatar_id,
                ctx.display_name.clone(),
                now_ms,
                body.clone(),
            )
            .await
            {
                tracing::warn!(avatar_id, "storing global chat failed: {e:#}");
            }
            let frame = encode_app_frame(&NetMessage::ChatMessage {
                channel: GLOBAL_CHANNEL.into(),
                sender_id: avatar_id,
                sender_name: ctx.display_name.clone(),
                sent_at_ms: now_ms,
                body,
                distance_m: None,
            })?;
            ctx.chat.deliver_all(&ctx.display_name, &Bytes::from(frame));
        }
        ChatScope::Local { shout } => {
            if ctx.gates.last_local.elapsed() < MIN_LOCAL_INTERVAL {
                return Ok(());
            }
            ctx.gates.last_local = Instant::now();
            let radius = if shout {
                crate::chat::SHOUT_RADIUS_M
            } else {
                crate::chat::SAY_RADIUS_M
            };
            // Sender + all candidate recipient positions in one read.
            let (sender_pos, positions) = {
                let w = ctx.world.read().await;
                let sp = w.embodied_position(avatar_id);
                let ps: Vec<(u64, _)> = ctx
                    .chat
                    .connected_ids()
                    .into_iter()
                    .filter_map(|id| w.embodied_position(id).map(|p| (id, p)))
                    .collect();
                (sp, ps)
            };
            // Local is ephemeral — delivered by distance, never stored.
            if let Some(sp) = sender_pos {
                for (id, d) in crate::chat::within_radius(sp, &positions, radius) {
                    let frame = encode_app_frame(&NetMessage::ChatMessage {
                        channel: LOCAL_CHANNEL.into(),
                        sender_id: avatar_id,
                        sender_name: ctx.display_name.clone(),
                        sent_at_ms: now_ms,
                        body: body.clone(),
                        distance_m: Some(d),
                    })?;
                    ctx.chat.deliver_to(
                        id,
                        &ctx.display_name,
                        crate::chat::Lane::Text,
                        Bytes::from(frame),
                    );
                }
            }
        }
    }
    Ok(())
}
