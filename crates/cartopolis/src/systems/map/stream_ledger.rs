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

use std::sync::Mutex;

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
    use super::{admit, refund};

    /// The tests share one process-wide ledger, so they run under one lock —
    /// otherwise a parallel test's `refund` lands mid-assertion and the failure
    /// reads as a broken rule rather than as a shared static.
    static SERIAL: std::sync::Mutex<()> = std::sync::Mutex::new(());

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
}
