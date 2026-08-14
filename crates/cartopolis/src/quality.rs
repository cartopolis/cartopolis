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
//! 1. An explicit `CARTO_QUALITY` / `?quality=` — `phone`/`low` or
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
//! Every escape hatch this codebase grew — `CARTO_FPS_CAP`,
//! `CARTO_NO_SPACE_RAYMARCH`, `CARTO_QUALITY` itself — reaches the process
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
///
/// **Ordered**, and the order is the point: `Phone < Balanced < Desktop`, so a
/// tier can be compared rather than matched. Code that wants "at least this much
/// machine" writes `budget().tier >= Tier::Balanced` and keeps working when a rung
/// is added, where a `match` on two variants had to be revisited. Progressive
/// enhancement needs a ladder, not a pair of special cases.
///
/// The rungs are deliberately few. Every one of them is a combination somebody has
/// to be able to reason about — and, more to the point, a combination nobody can
/// test on hardware they do not own.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Debug)]
pub enum Tier {
    /// A handset. Assume a tiled mobile GPU behind a high-DPI screen and a single
    /// thread for everything the CPU does.
    Phone,
    /// An integrated GPU, a thin laptop, or a handset good enough to want more
    /// than the floor: the scene as designed, minus the things that cost fill
    /// rather than shape.
    Balanced,
    /// A workstation or laptop GPU: the settings the rest of this codebase was
    /// tuned against.
    Desktop,
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
    /// Linear scale for the 3D main pass, as a fraction of the render target.
    ///
    /// **The one knob the phone tier was missing entirely.** `index.html` caps the
    /// web build at 1.5 DPR / 1.3 Mpx by sizing the canvas backing store — a page
    /// layout decision, so it cannot exist off the web — and native Android had no
    /// equivalent at all. A Fairphone 4 therefore rendered at its full 1080×2340 =
    /// **2.53 Mpx**, against the ~1.3 Mpx the same budget calls a phone's share, on
    /// a target that is `Rgba16Float` whenever the atmosphere is up.
    ///
    /// Applied by shrinking the camera's *extracted target size* in the render
    /// world (`systems::render_scale`), so the main texture is allocated at the
    /// reduced size and the `upscaling` blit already in the graph stretches it
    /// over the full-size swapchain. **Not** `MainPassResolutionOverride`, which
    /// is the component that looks like it is for this and which put the map in
    /// the corner of the phone — see that module's header for why.
    ///
    /// The **UI is unaffected** — `egui_pass` runs after `upscaling`, onto the
    /// full-size output — so the chrome and the text stay crisp while the world
    /// gets cheaper, which is the right way round on a 5.7" screen.
    ///
    /// 1.0 means "insert nothing", so every tier above the phone is bit-identical
    /// to before and `cargo shots` output does not move.
    pub render_scale: f32,
    /// Frame-rate ceiling for the sleep-based limiter, or `None` to leave pacing
    /// entirely to the presentation mode.
    ///
    /// `None` on Android, and that is a fix rather than a preference. The limiter
    /// sleeps the main thread to a 60 fps deadline in `Last`, while Android also
    /// runs `PresentMode::AutoVsync` — FIFO, which already blocks on acquire. Two
    /// pacers measuring from different points in the pipeline is the classic recipe
    /// for beat-frequency judder (alternating 60/30) even with GPU headroom to
    /// spare, and the limiter's own doc block was written for the `AutoNoVsync`
    /// desktop case and never revisited when the Android arm was added.
    ///
    /// It also decays the render thread's scheduler utilisation: a main thread that
    /// sleeps every frame looks idle to Linux EAS, which is exactly the wrong
    /// signal on a 2-big-6-little SoC where the tile workers are running 150 ms
    /// blocking jobs and look busy.
    ///
    /// The value is a `Budget` field for the reason this module's header gives:
    /// `CARTO_FPS_CAP` is an environment variable, and **Android has no
    /// environment** — so on the one platform whose pacing this decides, the
    /// existing knob is unreachable rather than merely inconvenient.
    ///
    /// `allow` on wasm: the only reader is the frame limiter in `run`, which is
    /// `cfg(not(target_arch = "wasm32"))` because the browser paces the loop
    /// itself — a `requestAnimationFrame` callback is already once per display
    /// refresh, and a sleep-based limiter inside one is the same double-pacing
    /// this field exists to avoid on Android. The field stays unconditional so
    /// the `Budget` tiers read the same on every target (and the tests above
    /// assert it on all of them).
    #[cfg_attr(target_arch = "wasm32", allow(dead_code))]
    pub fps_cap: Option<f64>,
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
            render_scale: 1.0,
            fps_cap: Some(60.0),
        }
    }

    /// The middle rung: keep the scene's *shape*, give up what costs fill.
    ///
    /// The split is deliberate and is the whole reason this tier is worth having
    /// rather than being a second phone preset. What survives is everything that
    /// decides what the world looks like — the detail layers (trees, furniture,
    /// street labels, textured roads, water shading), the full streaming radii, the
    /// undivided per-frame budgets and the full-resolution tile raster. What goes
    /// is the per-pixel and per-frame overhead: MSAA on a target that is already
    /// `Rgba16Float`, the every-frame sky IBL bake and its mip chain, the
    /// full-size atmosphere LUTs, and the space tier's full-screen ray march.
    ///
    /// Shadows and bloom stay. They are the two things whose absence reads as a
    /// different scene rather than a softer one — an unshadowed city looks flat and
    /// wrong, not cheaper — and a machine that cannot afford them belongs on
    /// [`Self::phone`], which is one step down and says so.
    pub(crate) const fn balanced() -> Self {
        Self {
            tier: Tier::Balanced,
            shadows: true,
            bloom: true,
            // The one bake that happens *every frame*, plus its mip chain. Its
            // contribution is low-frequency ambient light, which is the least
            // missed thing on this list.
            sky_ibl: false,
            detail_layers: true,
            radius_bias: 0,
            per_frame_divisor: 1,
            worker_cap: None,
            // Not `Sample2`: see the field's own comment. Only 1 and 4 are
            // portable, and 4 on an HDR target is precisely the cost this tier
            // exists to avoid — so this rung is the one that gives up edges.
            msaa: Msaa::Off,
            atmosphere_luts: AtmosphereLuts::halved(),
            space_raymarch: false,
            tile_px: crate::systems::vector_tiles::TILE_PX,
            // Native resolution: an integrated GPU is bandwidth-limited, not
            // resolution-limited, and this rung's brief is to keep the picture.
            render_scale: 1.0,
            fps_cap: Some(60.0),
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
            // 0.7 linear = ~49% of the fragments: 2.53 Mpx becomes ~1.24 Mpx,
            // which lands almost exactly on the 1.3 Mpx ceiling `index.html`
            // already enforces for the web phone tier. Chosen to match that number
            // rather than picked for feel, so the two platforms' phone budgets
            // mean the same thing.
            render_scale: 0.7,
            // Leave pacing to FIFO. See the field's own comment: a sleep-based
            // limiter layered on vsync is a judder source, not a saving, and the
            // handset is the platform that runs `AutoVsync`.
            fps_cap: None,
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
pub fn init(preferred: Option<&str>) {
    let resolved = detect(preferred);
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
    // `None` rather than the stored preference: this arm only runs when nobody
    // called `init`, which means there is no app and therefore no user — a unit
    // test, or a system that beat startup to it. Reading `user.json` from here
    // would make a test's answer depend on the developer's saved settings.
    BUDGET.get_or_init(|| detect(None))
}

/// Shorthand for the common `budget().tier == Tier::Phone` test.
#[must_use]
pub fn is_phone() -> bool {
    budget().tier == Tier::Phone
}

fn detect(preferred: Option<&str>) -> Budget {
    // An explicit setting wins everywhere, which is the whole point of it: it is
    // how the phone budget gets exercised on a machine that can profile it.
    if let Some(v) = crate::env_value("CARTO_QUALITY") {
        match parse(&v) {
            Some(budget) => return budget,
            None => tracing::warn!(
                value = v,
                "unknown CARTO_QUALITY; expected phone|low|mobile or desktop|high"
            ),
        }
    }
    // Then the user's own choice from Settings ▸ Map. Below the env var because
    // that one is a debugging override and has to win over a stored preference to
    // be useful; above `auto()` because a person who has picked a tier has said
    // something the detector cannot know — that this phone runs hot, or that this
    // laptop is nicer than its core count suggests. `None` is "Auto", which is not
    // a third answer but the absence of one.
    if let Some(budget) = preferred.and_then(parse) {
        return budget;
    }
    auto()
}

/// The spelling of a tier, as `CARTO_QUALITY` / `?quality=` accepts it.
///
/// Kept as its own function so the vocabulary is testable without a process-wide
/// `OnceLock` to set: `index.html` matches the same words before any of this runs,
/// and a word accepted by one half and not the other would put the page's render
/// scale and the renderer's own settings in different tiers.
fn parse(value: &str) -> Option<Budget> {
    match value.trim().to_ascii_lowercase().as_str() {
        "phone" | "low" | "mobile" => Some(Budget::phone()),
        "balanced" | "medium" => Some(Budget::balanced()),
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
        Some("balanced") => Budget::balanced(),
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
        for word in ["balanced", "medium", "Balanced", "MEDIUM", " medium "] {
            assert_eq!(parse(word).map(|b| b.tier), Some(Tier::Balanced), "{word}");
        }
        for word in ["desktop", "high", "Desktop", "HIGH"] {
            assert_eq!(parse(word).map(|b| b.tier), Some(Tier::Desktop), "{word}");
        }
        // Unrecognised is `None`, not a silent tier: `detect` warns and falls
        // through to auto-detection rather than pretending a typo meant one.
        for word in ["", "phones", "mid", "1"] {
            assert!(parse(word).is_none(), "{word} should not name a tier");
        }
    }

    /// The ladder has to be a ladder: each rung at least as cheap as the one above
    /// it, on every field where "cheaper" is even defined.
    ///
    /// Without this, a tier is just a struct literal somebody typed, and a middle
    /// rung that accidentally asked for *more* than Desktop on one field would be
    /// found by a user on the hardware least able to afford it. The ordering on
    /// [`Tier`] is what lets this be written as a loop instead of a pile of pairs.
    #[test]
    fn every_rung_costs_no_more_than_the_one_above_it() {
        let rungs = [Budget::phone(), Budget::balanced(), Budget::desktop()];
        for pair in rungs.windows(2) {
            let (lower, upper) = (pair[0], pair[1]);
            assert!(
                lower.tier < upper.tier,
                "{:?} must sort below {:?}",
                lower.tier,
                upper.tier
            );
            let name = format!("{:?} vs {:?}", lower.tier, upper.tier);
            assert!(!lower.shadows || upper.shadows, "shadows: {name}");
            assert!(!lower.bloom || upper.bloom, "bloom: {name}");
            assert!(!lower.sky_ibl || upper.sky_ibl, "sky_ibl: {name}");
            assert!(
                !lower.detail_layers || upper.detail_layers,
                "detail_layers: {name}"
            );
            assert!(
                !lower.space_raymarch || upper.space_raymarch,
                "raymarch: {name}"
            );
            assert!(lower.tile_px <= upper.tile_px, "tile_px: {name}");
            assert!(lower.msaa.samples() <= upper.msaa.samples(), "msaa: {name}");
            // A bigger bias is a *smaller* ring, and a bigger divisor a smaller
            // per-frame budget — both invert.
            assert!(lower.radius_bias >= upper.radius_bias, "radius: {name}");
            assert!(
                lower.per_frame_divisor >= upper.per_frame_divisor,
                "per_frame: {name}"
            );
            assert!(
                lower
                    .atmosphere_luts
                    .sky_view
                    .cmple(upper.atmosphere_luts.sky_view)
                    .all(),
                "atmosphere LUTs: {name}"
            );
        }
    }

    /// The phone leaves pacing to the presentation mode; the others cap.
    ///
    /// Not a taste question. Android runs `PresentMode::AutoVsync` (FIFO, which
    /// blocks on acquire), so a sleep-based limiter on top is a second pacer
    /// timing from a different point in the pipeline — the standard way to get
    /// alternating 60/30 judder on a device with headroom to spare.
    #[test]
    fn the_phone_leaves_frame_pacing_to_vsync() {
        assert_eq!(Budget::phone().fps_cap, None);
        assert!(Budget::balanced().fps_cap.is_some());
        assert!(Budget::desktop().fps_cap.is_some());
    }

    /// Only 1× and 4× are portable (see `Budget::msaa`), so **no** rung may ask for
    /// anything else — not just the phone. A middle tier that quietly asked for 2×
    /// would render nothing at all on an adapter that refuses it, and the only
    /// console output would be `CommandQueue has un-applied commands`.
    #[test]
    fn no_rung_asks_for_an_unportable_sample_count() {
        for budget in [Budget::phone(), Budget::balanced()] {
            assert!(
                matches!(budget.msaa, Msaa::Off | Msaa::Sample4),
                "{:?} asks for {}× MSAA; only 1 and 4 are guaranteed",
                budget.tier,
                budget.msaa.samples()
            );
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
