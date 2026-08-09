//! How much frame the client is allowed to spend, resolved once at startup.
//!
//! The web build targets phones now, and a phone is not a small desktop: a
//! Fairphone 4 renders at a device pixel ratio near 2.75 (1080×2340 real pixels
//! for a 393 CSS-px-wide viewport), drives an Adreno 619, and — because
//! `bevy_tasks` disables its multi-threaded pool on wasm — runs **every ECS
//! system, extract, queue and batch step on one thread**. So the two things that
//! cost a frame there are fill rate and main-thread work, and both of them scale
//! with settings this codebase otherwise picks for a workstation.
//!
//! This module is the one place that answers "which of those settings applies".
//! It is deliberately shaped like [`crate::env_flag`] rather than like a Bevy
//! resource: the tier is constant for the process, and several of its consumers
//! (`free_camera::camera_controls`, the streamers' radius constants) either run
//! before the world exists or are already at Bevy's 16-parameter cap for a
//! system. A `OnceLock` read costs an atomic load and plumbs through nothing.
//!
//! ## Where the answer comes from
//!
//! 1. An explicit `VIBERFOX_QUALITY` / `?quality=` — `phone`/`low` or
//!    `desktop`/`high`. Always wins, on every platform, which is what makes the
//!    phone budget reproducible on a workstation.
//! 2. On the web, `<html data-quality>`, written by the detection in
//!    `index.html` before the module is fetched.
//! 3. Otherwise the platform: Android is a phone, everything else is not.
//!
//! **The browser detects, not this module**, and the split is deliberate.
//! Deciding here would mean re-deriving in Rust what the page already had to know
//! to size the canvas — the canvas backing store is set from CSS by the time
//! wgpu ever sees it, so the render scale is a page-layout decision that cannot
//! be made from Rust at all. One detector, read by both halves, is the only
//! version of this where the tier the renderer thinks it is in and the resolution
//! it is actually rendering at cannot disagree.

use std::sync::OnceLock;

/// Which class of machine this run is being asked to render on.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Tier {
    /// A workstation or laptop GPU: the settings the rest of this codebase was
    /// tuned against.
    Desktop,
    /// A handset. Assume a tiled mobile GPU behind a high-DPI screen and a single
    /// thread for everything the CPU does.
    Phone,
}

/// The render budget for this run. Every field is a thing that measurably costs
/// either fill or main-thread time; nothing is here for taste.
#[derive(Clone, Copy, Debug)]
pub struct Budget {
    pub tier: Tier,
    /// Sun + moon shadow maps. The single most expensive geometry setting in the
    /// scene: shadow casters were ~79% of submitted triangles when
    /// `docs/notes/wasm-performance.md` last counted, and every one of them is a
    /// second draw call issued from the one thread the web build has.
    pub shadows: bool,
    /// Bloom's downsample/upsample chain, which is ~13 full-screen passes over
    /// the HDR target — pure fill, and fill is what a high-DPI phone has least
    /// of.
    pub bloom: bool,
    /// `AtmosphereEnvironmentMapLight`: a sky-driven IBL cubemap re-baked **every
    /// frame**, plus its mip chain.
    pub sky_ibl: bool,
    /// Trees, street furniture, street labels, textured roads and the animated
    /// water shader. Individually modest, collectively a large slice of both the
    /// draw list and the per-cell meshing the main thread does while streaming.
    pub detail_layers: bool,
    /// Subtracted from every streamer's ring radius (floored at 1 by
    /// [`Self::radius`]). One step takes the clipmap from 424 tiles to 224, which
    /// is close to half the ground draw list and half the streaming work behind
    /// it.
    pub radius_bias: i64,
    /// Divides the per-frame build/upload/despawn budgets. These exist to stop a
    /// burst overrunning a GPU's command-buffer timeout; on a phone the same
    /// burst is also the difference between a steady 30 fps and a fill that
    /// hitches every time a cell lands.
    pub per_frame_divisor: usize,
    /// Ceiling on `platform::workers`' pool. `available_parallelism` reports the
    /// Fairphone 4's 8 cores, but they are 2 performance plus 6 efficiency cores
    /// sharing one thermal envelope with the render thread and the browser's
    /// compositor — and each Web Worker re-instantiates the whole module against
    /// the shared memory. `None` = as many as the machine reports, minus one.
    pub worker_cap: Option<usize>,
}

impl Budget {
    /// The settings the rest of the codebase is written against.
    pub(crate) const fn desktop() -> Self {
        Self {
            tier: Tier::Desktop,
            shadows: true,
            bloom: true,
            sky_ibl: true,
            detail_layers: true,
            radius_bias: 0,
            per_frame_divisor: 1,
            worker_cap: None,
        }
    }

    /// Everything that costs fill or main-thread time, off or halved.
    ///
    /// This is not a gentle preset and is not meant to be one: the brief it was
    /// written to was "whatever it takes to hold ~30 fps on a Fairphone 4", and
    /// each of these was chosen because it buys frame time rather than because it
    /// was the least missed.
    pub(crate) const fn phone() -> Self {
        Self {
            tier: Tier::Phone,
            shadows: false,
            bloom: false,
            sky_ibl: false,
            detail_layers: false,
            radius_bias: 1,
            per_frame_divisor: 2,
            worker_cap: Some(3),
        }
    }

    /// A streaming ring radius with the tier's bias applied.
    ///
    /// Floored at 1 rather than 0: a radius of 0 is a single tile under the
    /// camera with nothing beside it, which is not a cheaper view of the world
    /// but a broken one.
    #[must_use]
    pub fn radius(&self, base: i64) -> i64 {
        (base - self.radius_bias).max(1)
    }

    /// A per-frame work budget with the tier's divisor applied, floored at 1 for
    /// the same reason [`Self::radius`] floors at 1 — a budget of 0 is a streamer
    /// that never finishes.
    #[must_use]
    pub fn per_frame(&self, base: usize) -> usize {
        (base / self.per_frame_divisor).max(1)
    }
}

static BUDGET: OnceLock<Budget> = OnceLock::new();

/// Resolve the budget for this run. Call once, early — before anything reads
/// [`budget`], and in particular before `platform::workers` sizes its pool.
///
/// Idempotent, and deliberately not a panic if called twice: a second call from a
/// test harness should be a no-op, not a crash in a build that works.
pub fn init() {
    let resolved = detect();
    if BUDGET.set(resolved).is_ok() {
        tracing::info!(
            tier = ?resolved.tier,
            shadows = resolved.shadows,
            bloom = resolved.bloom,
            sky_ibl = resolved.sky_ibl,
            detail_layers = resolved.detail_layers,
            "render budget resolved"
        );
    }
}

/// The budget for this run, resolving it on first use if [`init`] never ran (a
/// unit test, or a system that beat startup to it).
#[must_use]
pub fn budget() -> &'static Budget {
    BUDGET.get_or_init(detect)
}

/// Shorthand for the common `budget().tier == Tier::Phone` test.
#[must_use]
pub fn is_phone() -> bool {
    budget().tier == Tier::Phone
}

fn detect() -> Budget {
    // An explicit setting wins everywhere, which is the whole point of it: it is
    // how the phone budget gets exercised on a machine that can profile it.
    if let Some(v) = crate::env_value("VIBERFOX_QUALITY") {
        match parse(&v) {
            Some(budget) => return budget,
            None => tracing::warn!(
                value = v,
                "unknown VIBERFOX_QUALITY; expected phone|low|mobile or desktop|high"
            ),
        }
    }
    auto()
}

/// The spelling of a tier, as `VIBERFOX_QUALITY` / `?quality=` accepts it.
///
/// Kept as its own function so the vocabulary is testable without a process-wide
/// `OnceLock` to set: `index.html` matches the same words before any of this runs,
/// and a word accepted by one half and not the other would put the page's render
/// scale and the renderer's own settings in different tiers.
fn parse(value: &str) -> Option<Budget> {
    match value.trim().to_ascii_lowercase().as_str() {
        "phone" | "low" | "mobile" => Some(Budget::phone()),
        "desktop" | "high" => Some(Budget::desktop()),
        _ => None,
    }
}

#[cfg(target_arch = "wasm32")]
fn auto() -> Budget {
    // `index.html` decided this before the module was fetched, because it had to:
    // the canvas resolution is set from CSS, so the page cannot defer the
    // question. Absent (an `index.html` that predates the detector, or a page
    // embedding the canvas itself) is not an error — fall back to the tier that
    // changes nothing.
    match html_quality_attr().as_deref() {
        Some("phone") => Budget::phone(),
        _ => Budget::desktop(),
    }
}

#[cfg(target_arch = "wasm32")]
fn html_quality_attr() -> Option<String> {
    web_sys::window()?
        .document()?
        .document_element()?
        .get_attribute("data-quality")
}

#[cfg(all(not(target_arch = "wasm32"), target_os = "android"))]
fn auto() -> Budget {
    Budget::phone()
}

#[cfg(all(not(target_arch = "wasm32"), not(target_os = "android")))]
fn auto() -> Budget {
    Budget::desktop()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_radius_never_collapses_to_a_single_tile() {
        let phone = Budget::phone();
        // The clipmap's own radii, biased.
        assert_eq!(phone.radius(4), 3);
        assert_eq!(phone.radius(3), 2);
        // …and the ones that are already small stop rather than reaching 0.
        assert_eq!(phone.radius(2), 1);
        assert_eq!(phone.radius(1), 1);
    }

    #[test]
    fn a_per_frame_budget_never_reaches_zero() {
        let phone = Budget::phone();
        assert_eq!(phone.per_frame(8), 4);
        assert_eq!(phone.per_frame(2), 1);
        // A budget of 1 halves to 0 by integer division; a streamer with a budget
        // of 0 never finishes anything, which is a hang, not a saving.
        assert_eq!(phone.per_frame(1), 1);
    }

    /// The words `index.html`'s detector matches, and no others.
    ///
    /// The two halves are separate implementations of one vocabulary — the page
    /// has to answer before the module is fetched, this has to answer without a
    /// DOM — so the list is duplicated by necessity. Keep them in step: a word
    /// only one side knows puts the canvas resolution and the renderer settings
    /// in different tiers, and nothing reports that.
    #[test]
    fn the_tier_vocabulary_matches_the_page() {
        for word in ["phone", "low", "mobile", "PHONE", " Low "] {
            assert_eq!(parse(word).map(|b| b.tier), Some(Tier::Phone), "{word}");
        }
        for word in ["desktop", "high", "Desktop", "HIGH"] {
            assert_eq!(parse(word).map(|b| b.tier), Some(Tier::Desktop), "{word}");
        }
        // Unrecognised is `None`, not a silent tier: `detect` warns and falls
        // through to auto-detection rather than pretending a typo meant one.
        for word in ["", "phones", "medium", "1"] {
            assert!(parse(word).is_none(), "{word} should not name a tier");
        }
    }

    #[test]
    fn the_desktop_budget_changes_nothing() {
        let desktop = Budget::desktop();
        for base in [1_i64, 2, 3, 4] {
            assert_eq!(desktop.radius(base), base);
        }
        for base in [1_usize, 2, 8, 12] {
            assert_eq!(desktop.per_frame(base), base);
        }
    }
}
