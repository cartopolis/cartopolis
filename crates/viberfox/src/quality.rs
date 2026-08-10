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
//!
//! ## Why a field here rather than another env flag
//!
//! Every escape hatch this codebase grew — `VIBERFOX_FPS_CAP`,
//! `VIBERFOX_NO_SPACE_RAYMARCH`, `VIBERFOX_QUALITY` itself — reaches the process
//! through an environment or a query string, and **Android has neither**: the
//! zygote's `argv` belongs to `app_process` (which is why `Cli::parse()` must not
//! run there at all, see CLAUDE.md) and there is no shell to export a variable
//! from. So on the one platform the phone budget exists for, an env-gated knob is
//! not a weaker default — it is unreachable. Anything a handset needs turned down
//! belongs in [`Budget`], where `auto()` can answer for it.

use std::sync::OnceLock;

use bevy::math::{UVec2, UVec3};
use bevy::render::view::Msaa;

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
    /// Main-pass MSAA sample count.
    ///
    /// **A sample count is a correctness question before it is a cost one.** Only
    /// 1 and 4 are portable: WebGPU permits exactly those two, Mesa lavapipe
    /// advertises exactly those two, and Vulkan's *guaranteed minimum* for
    /// `framebufferColorSampleCounts` is `VK_SAMPLE_COUNT_1_BIT |
    /// VK_SAMPLE_COUNT_4_BIT` — 2× is an optional capability that a mobile GPU is
    /// free not to have. wgpu-core validates the request against the adapter's
    /// per-format `TextureFormatFeatureFlags`, so asking for an unsupported 2×
    /// fails to create `main_texture_sampled` and **nothing renders**, with the
    /// only console output being `CommandQueue has un-applied commands` — the
    /// symptom, never the cause. `free_camera::setup_camera` had already learned
    /// this twice (web, then lavapipe) and both times fixed it with a `cfg!`;
    /// Android is a third platform that answers "native" to every one of those
    /// tests and was still being handed 2×.
    ///
    /// Off, not `Sample4`, for the phone. `AtmosphereSettings` carries
    /// `#[require(Hdr)]`, so the main texture is `Rgba16Float`: at 1080×2340 that
    /// is 8 B/px of colour before multisampling, and 2× colour plus depth already
    /// exceeds the on-chip tile budget of an Adreno/Mali, which answers by binning
    /// the pass into more tiles and re-issuing the draws for each. 4× is worse
    /// again. Edge quality is the cheapest thing a handset has to give up.
    pub msaa: Msaa,
    /// Resolution of bevy_pbr's four atmosphere lookup tables.
    ///
    /// The atmosphere itself stays on in every tier — it *is* the sky — but its
    /// LUT sizes were never anything but `AtmosphereSettings::default()`, and the
    /// LUTs are recomputed **every frame**. See [`AtmosphereLuts`].
    pub atmosphere_luts: AtmosphereLuts,
    /// Whether the space tier may switch the atmosphere to
    /// `AtmosphereMode::Raymarched` (`globe::space_raymarch`).
    ///
    /// A full-screen ray march at `globe::GLOBE_SKY_SAMPLES` per fragment,
    /// measured at a few ms on a desktop iGPU and never measured on a handset. It
    /// buys the planet its atmospheric limb, which is worth those milliseconds on
    /// a machine that has them; the phone tier keeps `LookupTexture` and loses the
    /// limb. Note this only ever applied above `GLOBE_SHOW_ALT` — the city view is
    /// unaffected either way.
    pub space_raymarch: bool,
    /// Edge length, in pixels, that a vector tile body is rasterised to for the
    /// flat clipmap.
    ///
    /// The globe already made this decision (`globe::GLOBE_TILE_PX` = 256, carried
    /// by `vector_tiles::render_mvt_sized`) after `docs/notes/globe-tile-cost.md`
    /// measured its leaves at 170–340 screen px against a 512² raster — 2–3×
    /// oversampled per axis, for 4–9× the scan conversion. The flat path never got
    /// the same treatment, and it is the larger of the two: a phone clipmap is 224
    /// tiles, and 512² RGBA plus mips is ~1.4 MB each — ~310 MB of texture,
    /// against ~78 MB at 256². `docs/notes/tile-raster-cost.md` has scan
    /// conversion dominating parsing ~20×, and it scales with area, so this is
    /// also most of the time-to-first-map.
    ///
    /// **The honest cost:** at street level the finest clipmap ring is ~2.8
    /// texel/m at 512² and ~1.4 texel/m at 256², which is a visibly softer
    /// basemap if you look for it. It is acceptable *there specifically* because
    /// at ground level the imagery is not what you are looking at — the vector
    /// `surfaces` geometry (`map_geometry`) is drawn over it, and the imagery is
    /// the backdrop between the polygons. It would not be acceptable on a tier
    /// where the imagery is the map.
    pub tile_px: u32,
}

/// Sizes for the four LUTs bevy_pbr's atmosphere rebuilds every frame.
///
/// At Bevy's defaults that is roughly 4 M ray-march steps per frame before a
/// single pixel of sky is shaded: transmittance 256×128 × 40 samples,
/// multiscattering 32×32 × 64 dirs × 20 samples, sky-view 400×200 × 16, aerial
/// view 32³ × 10. Halving each axis takes it to ~1 M.
///
/// **Sizes, not sample counts.** The per-ray sample counts are left at Bevy's
/// defaults on both tiers, for the same reason `globe::GLOBE_SKY_SAMPLES` is:
/// starving the integration trades a fixed cost for visible banding, while the
/// LUT is a texture whose resolution nothing on a 1080-px-wide screen can
/// resolve. What this does cost is gradient smoothness — a 200×100 sky-view LUT
/// is where dawn's colour ramp lives, so if the phone sky ever bands across the
/// horizon this is the knob, not the samples.
#[derive(Clone, Copy, Debug)]
pub struct AtmosphereLuts {
    pub transmittance: UVec2,
    pub multiscattering: UVec2,
    pub sky_view: UVec2,
    pub aerial_view: UVec3,
}

impl AtmosphereLuts {
    /// `AtmosphereSettings::default()`, restated so the desktop tier changes
    /// nothing and a Bevy upgrade that moves the defaults shows up as a diff here
    /// rather than as a silent tier change.
    pub(crate) const fn bevy_defaults() -> Self {
        Self {
            transmittance: UVec2::new(256, 128),
            multiscattering: UVec2::new(32, 32),
            sky_view: UVec2::new(400, 200),
            aerial_view: UVec3::new(32, 32, 32),
        }
    }

    /// Every axis halved.
    pub(crate) const fn halved() -> Self {
        Self {
            transmittance: UVec2::new(128, 64),
            multiscattering: UVec2::new(16, 16),
            sky_view: UVec2::new(200, 100),
            aerial_view: UVec3::new(16, 16, 16),
        }
    }
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
            // 2× instead of Bevy's implicit 4×: roughly halves the main-pass
            // multisample cost for a marginal edge-quality change at this scene's
            // geometric density. `setup_camera` still overrides this to `Off` on a
            // software adapter, which reports the same 1-or-4 restriction the web
            // does — that is an *adapter* fact, not a tier one, so it cannot live
            // here.
            msaa: Msaa::Sample2,
            atmosphere_luts: AtmosphereLuts::bevy_defaults(),
            space_raymarch: true,
            tile_px: crate::systems::vector_tiles::TILE_PX,
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
            msaa: Msaa::Off,
            atmosphere_luts: AtmosphereLuts::halved(),
            space_raymarch: false,
            // Half of `vector_tiles::TILE_PX`, and the same 256 the globe settled
            // on. Written out rather than derived from `TILE_PX / 2` so raising
            // the flat map's raster size later does not silently raise the
            // phone's with it.
            tile_px: 256,
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
            msaa = resolved.msaa.samples(),
            space_raymarch = resolved.space_raymarch,
            tile_px = resolved.tile_px,
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
        assert_eq!(
            desktop.tile_px,
            crate::systems::vector_tiles::TILE_PX,
            "the desktop tier must rasterise at the size the flat map was tuned at"
        );
    }

    /// The phone tier must ask for a sample count every adapter is *required* to
    /// have. Vulkan guarantees only 1 and 4 in `framebufferColorSampleCounts`,
    /// WebGPU permits only 1 and 4, and lavapipe advertises only 1 and 4 — so 2×
    /// is a request a handset may legally refuse, and wgpu answers a refusal by
    /// failing to create `main_texture_sampled`, i.e. by rendering nothing at all.
    /// A tier that ships a black screen is not a cheaper tier.
    #[test]
    fn the_phone_tier_asks_for_a_universally_supported_sample_count() {
        assert!(
            matches!(Budget::phone().msaa, Msaa::Off | Msaa::Sample4),
            "only 1× and 4× are guaranteed; anything else can fail to render"
        );
    }

    /// Halving is the whole claim, so state it as one: every LUT axis strictly
    /// smaller than Bevy's default, none of them zero (a zero-sized texture is a
    /// wgpu validation error, not a cheap one).
    #[test]
    fn the_phone_atmosphere_luts_are_smaller_and_non_empty() {
        let (a, b) = (AtmosphereLuts::bevy_defaults(), AtmosphereLuts::halved());
        assert!(b.transmittance.cmplt(a.transmittance).all());
        assert!(b.multiscattering.cmplt(a.multiscattering).all());
        assert!(b.sky_view.cmplt(a.sky_view).all());
        assert!(b.aerial_view.cmplt(a.aerial_view).all());
        assert!(b.transmittance.min_element() > 0);
        assert!(b.multiscattering.min_element() > 0);
        assert!(b.sky_view.min_element() > 0);
        assert!(b.aerial_view.min_element() > 0);
    }
}
