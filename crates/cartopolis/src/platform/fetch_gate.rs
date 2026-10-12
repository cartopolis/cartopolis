//! One bound on **how many cell jobs the whole client may have outstanding**,
//! and a reserve of it that only near work may spend.
//!
//! **Why a global bound is the missing piece.** Every streamer bounds its own
//! concurrency — `Policy::max_in_flight`, twelve of them — and with all twelve in
//! force the aggregate is around **fifty-five** simultaneous jobs against an
//! `IoTaskPool` that is four threads wide on a handset and eight on a
//! workstation (`lib.rs`'s `TaskPoolThreadAssignmentPolicy`). The arithmetic is
//! the one [`stream_ledger`](crate::systems::stream_ledger) already had to fix
//! for uploads: the caps do not know about each other, so the sum is whatever the
//! twelve happen to add up to rather than a number anybody chose.
//!
//! What that costs is not throughput, it is **order**. `http::fetch` is a
//! blocking `ureq` call natively, so a job holds a pool thread for its whole
//! chain of requests; more jobs than threads means the surplus queues *inside*
//! the pool, first come first served, where nothing can reorder it. A fresh
//! anchor releases the far building cells, the coarse clipmap rings and the
//! attribute cells in the same frame as the cell the camera is standing on, and
//! which of them starts first is decided by which Bevy system ran first that
//! frame. The centre cell can and does wait behind a dozen tiles from eight
//! kilometres away.
//!
//! # Two rules
//!
//! * **A cap** ([`capacity`]), so the pool's own queue is never the thing doing
//!   the scheduling. It is deliberately generous — most jobs are a cache read
//!   that returns without touching the network, and a tight cap would slow a warm
//!   start to fix a cold one.
//! * **A reserve** ([`RESERVED_FOR_NEAR`]) that a job may only take if its
//!   [`Rank`] is [`NEAR`] or better. So a burst of distant work can fill the cap
//!   to within the reserve and no further, and the cell under the camera always
//!   has somewhere to start.
//!
//! # The rank is a ring distance, and it is comparable across streamers
//!
//! [`Rank`] is Chebyshev distance in cells from the one the camera is over: 0 is
//! the cell you are standing in, 1 its eight neighbours, and so on. Every
//! streamer cuts cells at its own zoom, so the *metres* differ — but what the
//! gate needs is an ordering, and "the cell I am in" means the same thing to the
//! ground surfaces, the LoD2.2 shells and the building attributes. A streamer
//! with nothing distance-shaped to say passes [`ANY`] and competes only for the
//! unreserved part.
//!
//! # A static, for the reason `stream_ledger` is one
//!
//! Twelve streamers consume this and several are already on Bevy's
//! sixteen-parameter ceiling, so a `Resource` would cost the parameter the next
//! feature needs. The state is two words behind a `Mutex`, touched a few times
//! per frame.
//!
//! # Slots are held, so every path that ends a job must give one back
//!
//! [`acquire`] is paired with [`release`] by
//! [`CellStream`](super::stream::CellStream) — on a drained result, on a
//! forgotten cell and on a re-anchor's `reset`. The re-anchor is the one that
//! bites: it drops the whole in-flight set at once, and a version that forgot to
//! return those slots would leak the entire cap over a few teleports and then
//! stream nothing, for ever, with nothing in the log.

use crate::platform::sync::Mutex;

/// How far, in cells, from the one the camera is over. Lower is more urgent.
pub type Rank = u32;

/// The cell the camera is in, and its immediate neighbours: the work a person is
/// actually waiting to see. Only these may spend the reserve.
pub const NEAR: Rank = 1;

/// "No useful distance to report" — a whole-region fetch, a manifest, an
/// attribute cell whose streamer does not track a ring. Competes for everything
/// except the reserve.
pub const ANY: Rank = Rank::MAX;

/// Slots kept back for [`NEAR`] work.
///
/// Four, which was the narrowest the `IoTaskPool` ever was (its `min_threads`)
/// when this was chosen, so on the machine with the least headroom the near ring
/// could saturate the pool by itself while every distant streamer was held off.
/// The floor is eight since 2026-09-24; four kept back of it is still a reserve.
const RESERVED_FOR_NEAR: usize = 4;

/// Outstanding jobs allowed at once, absent a pool to size it against.
#[cfg_attr(target_arch = "wasm32", allow(dead_code))]
const FALLBACK_CAPACITY: usize = 16;

/// Multiple of the io pool's width. Generous on purpose — see the module note:
/// most jobs never reach the network, and the cap exists to stop the pool's own
/// queue becoming the scheduler, not to ration cache reads.
#[cfg_attr(target_arch = "wasm32", allow(dead_code))]
const CAPACITY_PER_IO_THREAD: usize = 4;

/// How far a cell is from the one the camera is over, in cells — the [`Rank`]
/// every ring-shaped streamer states.
///
/// Chebyshev rather than Euclidean, because that is the shape of the ring itself:
/// a streamer wants the 3×3 or 5×5 square around the anchor, so the diagonal
/// neighbour is one step away in the same sense the orthogonal one is. Saturating
/// at [`ANY`] costs nothing real (no ring is four billion cells wide) and means a
/// nonsense cell cannot wrap round into looking urgent.
#[must_use]
pub fn ring_distance(cell: (i64, i64), centre_x: i64, centre_y: i64) -> Rank {
    let dx = (cell.0 - centre_x).unsigned_abs();
    let dy = (cell.1 - centre_y).unsigned_abs();
    u32::try_from(dx.max(dy)).unwrap_or(ANY)
}

struct Gate {
    held: usize,
}

static GATE: Mutex<Gate> = Mutex::new(Gate { held: 0 });

/// Jobs allowed outstanding across every streamer at once.
///
/// Derived from the io pool rather than declared, because that pool's width is
/// itself derived (a percentage of `available_parallelism`, with floors) and a
/// constant here would mean two numbers that can disagree about the same machine.
///
/// **Except in the browser** ([`WEB_CAPACITY`]), where there is no such pool to
/// derive from.
#[must_use]
pub fn capacity() -> usize {
    #[cfg(target_arch = "wasm32")]
    {
        WEB_CAPACITY
    }
    #[cfg(not(target_arch = "wasm32"))]
    {
        bevy::tasks::IoTaskPool::try_get().map_or(FALLBACK_CAPACITY, |pool| {
            (pool.thread_num() * CAPACITY_PER_IO_THREAD).max(FALLBACK_CAPACITY)
        })
    }
}

/// Outstanding jobs allowed at once in the web build.
///
/// The browser's `IoTaskPool` is the main thread — one "thread", whose fetches
/// are async and block nothing — so the native formula answered
/// [`FALLBACK_CAPACITY`] there: 16, a quarter of the 64 a desktop gets from its
/// sixteen-thread floor, for the build with the most work to share them. A slot
/// is also held for the whole job, worker meshing included, not only for its
/// download. Measured on the Deck's GPU in Chrome (2026-10-12): all 16 were taken
/// from 8 s to 27 s after start, the ground surfaces holding 8–9 of them while
/// their meshes built, and the basemap — 177 tiles queued at rank 1 — ran 0–6 at
/// a time and finished at 48 s. 32 lets the basemap and the surfaces each run
/// their own cap at once with room beside them.
#[cfg_attr(not(target_arch = "wasm32"), allow(dead_code))]
const WEB_CAPACITY: usize = 32;

/// Take a slot for a job of this rank, or report that it may not start yet.
///
/// A refusal is "not this frame" and nothing more: the caller's cell stays queued
/// and is offered again next frame, which is the same shape `Dispatcher`'s own
/// concurrency bound already has.
pub fn acquire(rank: Rank) -> bool {
    let cap = capacity();
    let mut gate = GATE
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    if admits(gate.held, cap, rank) {
        gate.held += 1;
        return true;
    }
    false
}

/// Take a slot for work that is **already outstanding**, without asking.
///
/// One caller: a deferred result being handed back to its stream. It never
/// stopped being outstanding — the slot was returned when the result drained and
/// has to come back with it — so refusing here would silently un-bound the gate
/// rather than delay anything.
pub fn reoccupy() {
    let mut gate = GATE
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    gate.held += 1;
}

/// Give a slot back. Saturating, so an unpaired release cannot wrap the count
/// into a permanently closed gate.
pub fn release() {
    let mut gate = GATE
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    gate.held = gate.held.saturating_sub(1);
}

/// Give back several at once — a re-anchor dropping its whole in-flight set.
pub fn release_many(n: usize) {
    let mut gate = GATE
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    gate.held = gate.held.saturating_sub(n);
}

/// Jobs outstanding across every streamer — for the diagnostics readout.
#[must_use]
pub fn outstanding() -> usize {
    GATE.lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .held
}

/// The decision itself, pure over the state so it can be asserted without a pool,
/// a clock or a process-wide static.
#[must_use]
fn admits(held: usize, capacity: usize, rank: Rank) -> bool {
    if held >= capacity {
        return false;
    }
    if rank <= NEAR {
        return true;
    }
    capacity - held > RESERVED_FOR_NEAR
}

#[cfg(test)]
mod tests {
    use super::{admits, ANY, NEAR, RESERVED_FOR_NEAR};

    #[test]
    fn distant_work_stops_at_the_reserve_and_near_work_does_not() {
        let cap = 16;
        let free_to_all = cap - RESERVED_FOR_NEAR;
        // Distant work fills everything but the reserve…
        assert!(admits(free_to_all - 1, cap, ANY));
        assert!(!admits(free_to_all, cap, ANY));
        // …and the reserve is still there for the cell under the camera.
        assert!(admits(free_to_all, cap, NEAR));
        assert!(admits(cap - 1, cap, 0));
    }

    #[test]
    fn the_cap_binds_every_rank() {
        let cap = 16;
        assert!(!admits(cap, cap, 0));
        assert!(!admits(cap, cap, ANY));
        assert!(!admits(cap + 1, cap, 0));
    }

    #[test]
    fn the_ring_distance_is_chebyshev_and_centres_on_zero() {
        use super::ring_distance;
        assert_eq!(ring_distance((5, 7), 5, 7), 0);
        assert_eq!(
            ring_distance((6, 8), 5, 7),
            1,
            "a diagonal neighbour is one step"
        );
        assert_eq!(ring_distance((5, 10), 5, 7), 3);
        assert_eq!(ring_distance((2, 7), 5, 7), 3, "distance has no direction");
    }

    /// A capacity at or below the reserve would refuse *all* distant work for
    /// ever rather than merely holding it back, so the floor on
    /// [`super::capacity`] has to stay above it.
    #[test]
    fn the_capacity_floor_leaves_room_outside_the_reserve() {
        assert!(super::FALLBACK_CAPACITY > RESERVED_FOR_NEAR);
        assert!(admits(0, super::FALLBACK_CAPACITY, ANY));
    }

    /// The web build's cap must hold the basemap and the ground surfaces at their
    /// own caps at once, outside the reserve — the case that left the basemap
    /// running 0–6 tiles at a time behind the surfaces' meshing at 16.
    #[test]
    fn the_web_cap_runs_the_basemap_beside_the_surfaces() {
        use crate::systems::map_geometry::SURFACE_MAX_IN_FLIGHT;
        use crate::systems::map_stream::TILE_MAX_IN_FLIGHT;
        assert!(
            super::WEB_CAPACITY - RESERVED_FOR_NEAR >= TILE_MAX_IN_FLIGHT + SURFACE_MAX_IN_FLIGHT
        );
    }
}
