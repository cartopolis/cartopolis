//! Proximity voice: one frame in, fanned out to whoever may hear it.

use super::{ConnCtx, FrameTransport, MAX_VOICE_BYTES, MIN_VOICE_INTERVAL, VOICE_CHANNEL};
use bytes::Bytes;
use cartopolis_core::{encode_app_frame, ChatScope, NetMessage};
use std::time::Instant;

pub(super) async fn on_frame<T: FrameTransport>(
    ctx: &mut ConnCtx<T>,
    scope: ChatScope,
    seq: u32,
    payload: Vec<u8>,
) -> anyhow::Result<()> {
    if payload.is_empty() || payload.len() > MAX_VOICE_BYTES {
        return Ok(());
    }
    if ctx.gates.last_voice.elapsed() < MIN_VOICE_INTERVAL {
        return Ok(());
    }
    ctx.gates.last_voice = Instant::now();
    let avatar_id = ctx.avatar_id;
    // Voice is never stored: it is ephemeral in every scope, not just Local, so
    // no `insert_chat` here.
    match scope {
        ChatScope::Local { shout } => {
            let radius = if shout {
                crate::chat::SHOUT_RADIUS_M
            } else {
                crate::chat::SAY_RADIUS_M
            };
            let (sender_pos, positions) = {
                let w = ctx.world.read().await;
                let sp = w.embodied_position(avatar_id);
                let ps: Vec<(u64, _)> = ctx
                    .chat
                    .connected_ids()
                    .into_iter()
                    .filter(|id| *id != avatar_id)
                    .filter_map(|id| w.embodied_position(id).map(|p| (id, p)))
                    .collect();
                (sp, ps)
            };
            if let Some(sp) = sender_pos {
                for (id, d) in crate::chat::within_radius(sp, &positions, radius) {
                    let frame = encode_app_frame(&NetMessage::VoiceData {
                        channel: VOICE_CHANNEL.into(),
                        sender_id: avatar_id,
                        seq,
                        payload: payload.clone(),
                        distance_m: Some(d),
                    })?;
                    ctx.chat.deliver_to(
                        id,
                        &ctx.display_name,
                        crate::chat::Lane::Voice,
                        Bytes::from(frame),
                    );
                }
            }
        }
        // Global voice is deliberately not carried: it is an unbounded fan-out and
        // the loudest abuse surface chat has, and ADR-028 already suggests
        // launching without the widest text tiers. Enable it here if that call is
        // revisited.
        ChatScope::Global => {
            tracing::debug!("dropped global voice frame");
        }
    }
    Ok(())
}
