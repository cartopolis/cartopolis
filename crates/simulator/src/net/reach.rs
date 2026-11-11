//! One connection's reach: the snapshot cut for it each tick, and the things it
//! has been sent (`docs/notes/interest-by-reach.md`).
//!
//! The tick loop leaves a [`TickTable`] on the broadcast channel; every
//! connection reads it here and takes what is within [`reach_m`] of its own
//! avatar: the avatars, as one `WorldSnapshot` assembled from states the tick
//! encoded once (`cartopolis_core::snapshot_frame`), and the things, as the
//! difference against what it was sent before — a `PrimUpsert` for each that
//! came within reach, a `PrimOutOfReach` for each that left. A thing's change
//! ([`Outbound::Thing`]) is forwarded only when it falls within the reach the
//! last tick measured.

use std::collections::HashSet;

use bytes::Bytes;
use cartopolis_core::{snapshot_frame, NetMessage, PrimDto};
use futures_util::SinkExt;

use super::{ConnCtx, FrameTransport};
use crate::reach::{reach_m, Place, TickTable};

/// What a connection remembers about its reach between ticks.
#[derive(Default)]
pub(super) struct ReachState {
    /// Where this connection's avatar stood at the last tick, if anywhere.
    place: Option<Place>,
    /// The things this connection has been sent and not yet told to forget.
    things: HashSet<i64>,
}

impl ReachState {
    /// A thing was deleted: it is nobody's to forget any more.
    pub(super) fn forget(&mut self, id: i64) {
        self.things.remove(&id);
    }
}

/// A tick arrived: feed this connection the snapshot of its reach, the things
/// that came within it and the ids of the things that left.
pub(super) async fn on_tick<T: FrameTransport>(
    ctx: &mut ConnCtx<T>,
    table: &TickTable,
) -> anyhow::Result<()> {
    let Some(place) = table.place_of(ctx.avatar_id) else {
        // Nowhere: nothing is within reach of nowhere, and a placed avatar
        // cannot become unplaced, so there is nothing to forget either. The
        // snapshot still goes, empty: it is also the tick the client hears, and
        // a link that falls silent after a resume is judged dead
        // (`session::check_link_after_resume`).
        let frame = snapshot_frame(table.tick, std::iter::empty())?;
        ctx.framed.feed(Bytes::from(frame)).await?;
        return Ok(());
    };
    ctx.reach.place = Some(place);
    let radius = reach_m(place.alt_m);

    let states = table.avatars_within(place, radius);
    let frame = snapshot_frame(table.tick, states.into_iter())?;
    ctx.framed.feed(Bytes::from(frame)).await?;

    let now: HashSet<i64> = table.things_within(place, radius).collect();
    let gone: Vec<i64> = ctx.reach.things.difference(&now).copied().collect();
    for id in gone {
        ctx.reach.things.remove(&id);
        ctx.feed(&NetMessage::PrimOutOfReach { id }).await?;
    }
    let new: Vec<i64> = now.difference(&ctx.reach.things).copied().collect();
    if !new.is_empty() {
        // Read now rather than carried in the table: a thing enters a reach
        // rarely, and encoding every thing every tick for the few that do was
        // the cost the handshake-only prim list was introduced to stop
        // (`docs/notes/simulator-tick-budget.md`). A thing deleted since the
        // table was taken is simply not here, and not in the next table either.
        let prims: Vec<PrimDto> = {
            let w = ctx.world.read().await;
            new.iter().filter_map(|id| w.prim(*id).cloned()).collect()
        };
        for prim in prims {
            ctx.reach.things.insert(prim.id);
            ctx.feed(&NetMessage::PrimUpsert { prim }).await?;
        }
    }
    Ok(())
}

/// A thing was made or changed: forward it if it is within this connection's
/// reach or this connection's account asked for it, and if it was within reach
/// and has moved out, say so.
pub(super) async fn on_thing<T: FrameTransport>(
    ctx: &mut ConnCtx<T>,
    id: i64,
    place: Place,
    frame: Bytes,
    by: Option<&str>,
) -> anyhow::Result<()> {
    let mine = by.is_some() && by == ctx.actor.principal.as_deref();
    let within = ctx
        .reach
        .place
        .is_some_and(|here| here.distance_m(place) <= reach_m(here.alt_m));
    if within || mine {
        ctx.reach.things.insert(id);
        ctx.framed.feed(frame).await?;
    } else if ctx.reach.things.remove(&id) {
        ctx.feed(&NetMessage::PrimOutOfReach { id }).await?;
    }
    Ok(())
}
