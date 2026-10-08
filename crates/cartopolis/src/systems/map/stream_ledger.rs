//! One byte budget per frame, **shared by every streamer that finalises assets**.
//!
//! **Why sharing is the missing piece.** Every streamer in this client already
//! bounds what it finalises per frame — `map_stream` in bytes, `lod22`,
//! `map_geometry`, `osm_buildings` and `globe` in items — and with all of those
//! caps in force the 2026-08-20 Android log still shows **27 MB queued in a
//! single frame**. The arithmetic is simply that the caps do not know about each
//! other: the tile streamer spends its 4 MB, `lod22` lands an 8 MB piece,
//! surfaces and the far buildings land theirs, all in the same frame — each
//! within its own budget, the frame ~7× over the one that
//! `quality::Budget::upload_bytes_per_frame` was written to enforce. The result
//! at startup is a few seconds of 100–500 ms frames: a frozen splash bar and a
//! fade nobody sees, on the same burst shape that used to lose the device
//! outright.
//!
//! So this is a ledger, not another cap: [`open_frame`] funds it each frame with
//! [`crate::quality::upload_ceiling`] — the same single resolution of "how much
//! may a frame hand the GPU" that bevy's `RenderAssetBytesPerFrame` and the tile
//! cap already share — and every finalisation site asks [`admit`] before
//! creating the frame's assets. What is refused is **deferred, never dropped**:
//! each streamer already owns a defer path (that is what their per-item caps
//! use), so a refusal costs latency and nothing else.
//!
//! **Two rules, both taken from bevy's own throttle.** The first admission of a
//! frame always passes whatever its size, because an asset larger than the whole
//! budget must still land some frame or it is deferred forever. After that, an
//! asset is admitted only if it fits entirely in what remains. The first rule is
//! also what keeps the big irregular items — a LoD2.2 cell, the detail patch —
//! from starving: they cannot fit *beside* anything, so they land on the frames
//! where they come first, alone, which is exactly the spreading this module
//! exists to produce.
//!
//! **Statics, not a `Resource`, and that is a parameter-count decision.** Six
//! systems consume this, and at least one (`update_map_surfaces`) is already at
//! Bevy's 16-parameter ceiling — threading a `ResMut` through all of them costs
//! the parameter the next feature needs. The state is a `Mutex` over two words,
//! locked a handful of times per frame; the same pattern as
//! `platform::workers`' pause flag, for the same reason.
//!
//! Uncapped tiers (everything but the phone, absent `CARTO_UPLOAD_KB`) admit
//! everything: [`open_frame`] funds the ledger with "no ceiling" and [`admit`]
//! short-circuits, so desktop behaviour is byte-identical to before this module
//! existed. Scripted captures keep the ledger **in force**, exactly as the tile
//! cap they inherit it from does: a deferred finalisation stays in its streamer,
//! is counted by the settle test, and is therefore the code path the handset
//! actually runs (`map_stream` documents why the bevy-side budget makes the
//! opposite choice).

use crate::platform::sync::Mutex;

/// The frame's remaining allowance, or `None` while uncapped.
struct Ledger {
    remaining: Option<usize>,
    /// Whether anything has been admitted this frame — the first admission
    /// always passes (see the module doc).
    admitted_any: bool,
}

static LEDGER: Mutex<Ledger> = Mutex::new(Ledger {
    remaining: None,
    admitted_any: false,
});

/// Fund the ledger for the coming frame. Runs in `First`, before any streamer.
pub fn open_frame() {
    refund(crate::quality::upload_ceiling());
}

/// May `bytes` of assets be created this frame? Spends the allowance when yes.
///
/// A refusal means "not beside what already landed this frame" — retry next
/// frame, via the streamer's own defer path. Zero-byte admissions (a failed
/// tile, an empty cell) always pass and spend nothing.
pub fn admit(bytes: usize) -> bool {
    let mut ledger = LEDGER
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let Some(remaining) = ledger.remaining else {
        return true;
    };
    if ledger.admitted_any && bytes > remaining {
        return false;
    }
    ledger.admitted_any |= bytes > 0;
    ledger.remaining = Some(remaining.saturating_sub(bytes));
    true
}

/// The finished results a streamer may apply this frame: at most `cap` of
/// them, in arrival order, and only as many as the frame's ledger [`admit`]s.
/// Everything behind the first refusal goes back through
/// `CellStream::defer` — still holding its concurrency slot, so the stream's
/// bound keeps bounding memory as well as work — and lands on a later frame.
///
/// The per-item cap is checked **before** the ledger, so an item past the cap
/// is never priced and never spends the frame's allowance, and the first
/// refusal ends the take rather than skipping to a cheaper item behind it:
/// arrival order is kept, and a big item that cannot fit beside anything lands
/// on the frame where it comes first (the module doc's first rule).
///
/// `cost` is what the result will hand the GPU, in bytes; a failure or an
/// empty cell is 0, which always passes and spends nothing.
pub fn take_admitted<K, R>(
    stream: &mut crate::platform::stream::CellStream<K, R>,
    cap: usize,
    mut cost: impl FnMut(&R) -> usize,
) -> Vec<(K, R)>
where
    K: Eq + std::hash::Hash + Clone,
{
    let mut done = stream.take_results();
    let mut take = 0usize;
    for (_, result) in &done {
        if take >= cap {
            break;
        }
        if !admit(cost(result)) {
            break;
        }
        take += 1;
    }
    let deferred = done.split_off(take);
    stream.defer(deferred);
    done
}

/// What one [`bevy::prelude::Mesh`] will cost the GPU, by the same arithmetic
/// [`crate::systems::upload_meter`] measures with — so what the ledger admits
/// and what the meter reports are the same number.
#[must_use]
pub fn mesh_cost(mesh: &bevy::prelude::Mesh) -> usize {
    crate::systems::upload_meter::mesh_bytes(mesh).unwrap_or(0)
}

/// Set the ledger's funding directly — the testable core of [`open_frame`].
fn refund(ceiling: Option<usize>) {
    *LEDGER
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner) = Ledger {
        remaining: ceiling,
        admitted_any: false,
    };
}

#[cfg(test)]
mod tests {
    use super::{admit, refund, take_admitted};
    use crate::platform::stream::{CellStream, Policy};

    /// The tests share one process-wide ledger, so they run under one lock —
    /// otherwise a parallel test's `refund` lands mid-assertion and the failure
    /// reads as a broken rule rather than as a shared static.
    static SERIAL: crate::platform::sync::Mutex<()> = crate::platform::sync::Mutex::new(());

    #[test]
    fn the_first_admission_always_passes_and_later_ones_must_fit() {
        let _guard = SERIAL
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        refund(Some(4096));
        // Twice the budget, admitted anyway: an asset larger than the whole
        // ceiling must still land some frame.
        assert!(admit(10_000));
        // …but it emptied the frame; nothing lands beside it.
        assert!(!admit(1));
        // A failure report costs nothing and is never deferred.
        assert!(admit(0));
    }

    #[test]
    fn admissions_share_one_pool_rather_than_each_getting_their_own() {
        let _guard = SERIAL
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        refund(Some(4096));
        assert!(admit(3000)); // the tile streamer's spend…
        assert!(!admit(3000)); // …is visible to the next streamer, unlike before
        assert!(admit(1000)); // what still fits, fits
        refund(Some(4096)); // the next frame funds it afresh
        assert!(admit(3000));
    }

    #[test]
    fn an_uncapped_tier_admits_everything() {
        let _guard = SERIAL
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        refund(None);
        assert!(admit(usize::MAX));
        assert!(admit(usize::MAX));
    }

    /// The streamers' shared drain: the item cap first, then the ledger, and the
    /// first refusal ends the take — a cheaper result behind it waits its turn
    /// rather than jumping the queue. Whatever is not taken keeps its slot.
    #[test]
    fn take_admitted_stops_at_the_first_refusal_and_defers_the_rest() {
        let _guard = SERIAL
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let mut s = CellStream::<i32, usize>::new(Policy {
            max_in_flight: 8,
            min_interval_secs: 0.0,
            max_attempts: 1,
            backoff_secs: 0.0,
            backoff_doublings: 0,
            cooldown_secs: 0.0,
            despawns_per_frame: 1,
        });
        // Near rank, so the global fetch gate's reserve admits them even while
        // other tests hold its slots.
        for k in 0..4 {
            s.request_ranked(k, crate::platform::fetch_gate::NEAR, 0.0);
        }
        while s.next_job(0.0).is_some() {}
        s.sink()
            .lock()
            .unwrap()
            .extend([(0, 60), (1, 60), (2, 10), (3, 10)]);

        refund(Some(100));
        let taken = take_admitted(&mut s, 3, |bytes| *bytes);
        assert_eq!(
            taken,
            vec![(0, 60)],
            "the ledger's refusal must end the take"
        );
        assert_eq!(s.in_flight(), 3, "deferred results must keep their slots");

        refund(None);
        let taken = take_admitted(&mut s, 2, |bytes| *bytes);
        assert_eq!(
            taken,
            vec![(1, 60), (2, 10)],
            "the item cap applies uncapped too"
        );
        assert_eq!(s.in_flight(), 1);
    }
}
