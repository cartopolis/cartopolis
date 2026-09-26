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
//! 3. Otherwise the platform. Android takes [`Budget::balanced`] and everything
//!    else [`Budget::desktop`] — note that *no* platform auto-detects to
//!    [`Budget::phone`] any more; see `auto()` for why, and reach it through
//!    Settings or `CARTO_QUALITY=phone` when a device needs it.
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

use bevy::core_pipeline::tonemapping::Tonemapping;
use bevy::light::ShadowFilteringMethod;
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
    /// Octaves the water shader sums per fragment (`assets/shaders/water.wgsl`).
    ///
    /// Each one is two sines, a cosine and two powers, and the finest ones are the
    /// centimetre ripples that only exist within a few tens of metres of the
    /// camera — past that the shader's own distance fade retires them and the loop
    /// stops early, so this is very nearly a *near-field* cost. `water-surface.md`
    /// has the octaves-per-fragment table by view distance.
    ///
    /// Dropping it does not flatten the sea: `water::octave_norms` renormalises so
    /// every rung renders the same RMS slope, and a rung with fewer octaves simply
    /// stops resolving the small stuff sooner and takes it as roughness instead.
    /// That is the one property that makes this safe to put on the ladder.
    pub water_octaves: u32,
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
    /// Shadow cascades per directional light, below the platform's own ceiling
    /// (`lib.rs::shadow_cascades`).
    ///
    /// Only meaningful where [`Self::shadows`] is on, but it is a rung on this
    /// ladder rather than a constant because it *was* a constant — a bare
    /// `cfg!(target_arch = "wasm32")` test, which Android fails in the direction
    /// that costs it: the handset got a workstation's four cascades, over two
    /// lights, across the ~79% of submitted triangles that cast shadows.
    ///
    /// The count must be the same for the sun and the moon or bevy_light panics
    /// outright (see CLAUDE.md), which is why one function answers for both and
    /// why this is not a per-light field.
    pub shadow_cascades: usize,
    /// How far the sun's cascades reach, in metres. The moon's is fixed and
    /// shorter; it is only ever a night-time fill light.
    ///
    /// This is also the only distance LOD the casters have, and that is on
    /// purpose: `check_dir_light_mesh_visibility` tests every caster against
    /// every cascade frustum, so anything past the reach is already not
    /// submitted. A second per-streamer shadow radius beside this one would be
    /// two mechanisms arbitrating the same question.
    pub shadow_distance_m: f32,
    /// How a shadow's *edge* is filtered — the only shadow setting on this ladder
    /// that costs per **pixel** rather than per triangle.
    ///
    /// Everything else here bounds what the shadow pass draws. This one bounds
    /// what the *main* pass reads: bevy's default, [`ShadowFilteringMethod::Gaussian`],
    /// takes nine taps with Castaño's filtering to approximate a 5×5 kernel, for
    /// every shadowed fragment, for every light — so at a handset's 2772×1272
    /// with the sun and the moon both up it is the largest single shadow cost in
    /// the frame and none of the caster work above touches it.
    /// [`ShadowFilteringMethod::Hardware2x2`] is one hardware PCF tap.
    ///
    /// It was never set at all until 2026-09-11, so every tier silently took the
    /// nine-tap default.
    ///
    /// `Temporal` is not reachable from here and should not become so: it is the
    /// good answer *when TAA is running*, and `docs/notes/surface-finish-and-occlusion.md`
    /// records why this codebase cannot run TAA — `big_space` re-anchors the whole
    /// coordinate frame as the camera moves, which is exactly what a history
    /// buffer indexed by last frame's motion vectors cannot survive. Without TAA
    /// the randomised filter is noise that crawls.
    ///
    /// Overridable per run with `CARTO_SHADOW_FILTER=fast|soft`, which is not
    /// redundant with the rung: this is a per-pixel cost whose value is a
    /// judgement about a picture, and the two questions it raises — "is the soft
    /// edge worth it on a workstation" and "is the hard edge acceptable on a
    /// phone" — cannot be answered on the same machine. The flag goes through
    /// [`crate::env_value`], so it reaches a shipped Android build as
    /// `debug.carto.shadow_filter` with no rebuild, the way `CARTO_NO_CLOUDS`
    /// settled the globe cloud shell.
    pub shadow_filter: ShadowFilteringMethod,
    /// Tonemapping curve for the main pass.
    ///
    /// A cost question only on a tier with no `Hdr` camera, which is most of what
    /// the phone is: with no atmosphere and no bloom there is no `Hdr` component,
    /// and bevy_pbr answers that by setting `TONEMAP_IN_SHADER` — so the curve
    /// stops being one full-screen pass and becomes work in *every mesh fragment
    /// shader*, multiplied by the ground overdraw. Bevy's default there is
    /// `TonyMcMapface`, which samples a 3D LUT; `AcesFitted` is pure arithmetic
    /// and needs no texture bound at all.
    ///
    /// Deband dither stays on regardless — the sky dome's gradient is precisely
    /// the banding case it exists for, and it is arithmetic, not a fetch.
    pub tonemapping: Tonemapping,
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
    /// Samples per side in the terrain field's 6.4 km box (`systems::terrain`).
    ///
    /// **A memory and a bandwidth rung, not a picture one.** 2049² is one 12 MB
    /// request per map anchor and 17 MB resident, plus a 5.6 MB lower-envelope
    /// pyramid over it — which is what it takes for a Dutch street to have a
    /// gradient at all (`docs/notes/terrain-relief.md`). A handset is the machine
    /// where that download is somebody's data allowance and that resident set is the
    /// low-memory killer, so [`handset_guarded`] brings it down; the picture cost is
    /// real and is the honest trade, not a hidden one.
    ///
    /// Always one more than a power of two, so the *intervals* halve exactly and the
    /// pyramid divides (`terrain::TERRAIN_GRID` says why).
    pub terrain_grid: usize,
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
    /// Ceiling on the bytes of assets the streamers hand the GPU in one frame, or
    /// `None` for "as fast as they arrive". Enforced by `stream_ledger` before the
    /// assets exist — **not** by bevy's `RenderAssetBytesPerFrame`, which this used
    /// to feed and which desynchronised deferred meshes from their buffers (see the
    /// resource's insertion in `lib.rs`).
    ///
    /// **The bound this codebase was missing.** Every streamer here is already
    /// per-frame-bounded — the flat tiles, the globe tiles, the LoD2.2 cells, the
    /// OSM detail patches all cap how many *items* they finalise per frame, and
    /// CLAUDE.md states the rule. But the items differ in size by orders of
    /// magnitude: a 256² tile mip chain is ~350 kB and a LoD2.2 cell is a whole
    /// z14 triangle soup, so a count of two can be 700 kB or 40 MB. The quantity
    /// that decides whether a tiler's command buffer survives the frame is the
    /// byte count, and nothing was bounding it — which is how a cold start could
    /// hand the driver a burst that lost the device (an Adreno 840, 2026-08-17,
    /// five seconds into a first run; the same failure mode as the Intel iGPU
    /// entry in CLAUDE.md).
    ///
    /// It **defers, it does not drop**: bevy re-queues what does not fit and
    /// uploads it next frame, so a low ceiling costs fill *latency* — the map
    /// takes longer to sharpen — and never content. That is the trade, and it is
    /// the right way round for a handset: a map that fills over three seconds is a
    /// map, and a lost device is a black screen and a crash report.
    ///
    /// `None` above the phone, and not out of generosity: a workstation's uploads
    /// are the same bytes over a bus with an order of magnitude more of them, the
    /// burst has never cost a desktop run a device, and a throttle there would slow
    /// `cargo shots` for nothing. This is one of the few fields where the phone rung
    /// is not simply a smaller number than the rungs above it.
    ///
    /// Measure before changing it: `systems::upload_meter` reports the frame that
    /// queues the most, in the same units, in the `slow frame` line and as
    /// `upload_peak_kb` in `--dump-state`.
    pub upload_bytes_per_frame: Option<usize>,
    /// How many bytes of streamed content this machine should hold at once, or
    /// `None` for "no ceiling".
    ///
    /// The **total** to `upload_bytes_per_frame`'s **rate**, and the term that
    /// actually loses a device: the 2026-08-19 Adreno 840 report had the rate
    /// guard in force and died anyway, holding ~1 GB (see [`handset_guarded`],
    /// which is where that table lives). Deferring an upload changes when a tile
    /// arrives, never whether it stays.
    ///
    /// Unlike every other field here, this one is **not** applied at startup.
    /// `tile_px` and `radius_bias` are the levers a preset can pull for it, and
    /// what they can promise is a ring of a given size — not a number of bytes,
    /// because the bytes depend on the ground the ring lands on. So this is a
    /// *target* that `systems::load_control` steers the ring toward with the
    /// measurement `systems::upload_meter` was already taking, rather than a
    /// setting anything reads directly.
    ///
    /// The numbers are read off the table in [`handset_guarded`], which measured
    /// Groningen at 150 m with a warm tile cache. Each rung sits a little above
    /// what that configuration actually held, so the controller does nothing in
    /// the case that was measured and acts only where the ground is denser than
    /// the rung was tuned against. `None` on the desktop rung for the same reason
    /// the rate ceiling is: no desktop run on record has lost a device to
    /// streaming, and a ring that narrowed itself on a workstation would be
    /// taking away a view nobody was struggling to render.
    pub resident_bytes_target: Option<usize>,
    /// How much **system memory** the streamers may spend keeping the finished
    /// form of a retired cell, so returning to it costs an upload instead of a
    /// rebuild (`platform::warm`).
    ///
    /// Unrelated to every other number in this struct, and the difference is the
    /// one thing to keep straight about it: `upload_bytes_per_frame` and
    /// `resident_bytes_target` bound what the *GPU* holds, because that is what
    /// loses a device. This bounds what the CPU holds, which costs a device
    /// nothing and costs the low-memory killer everything.
    ///
    /// The value it buys is measured in `docs/notes/tile-raster-cost.md`: a warm
    /// *disk* cache takes a 27.8 s fill of Groningen to 25.7 s, because the cost
    /// is scan conversion rather than fetching. Only keeping the raster itself
    /// can move that, and a 512² ground tile's mip chain is a fixed 1,365 kB —
    /// so this divided by 1.4 MB is roughly how many ground tiles a return
    /// journey gets for free.
    ///
    /// `CARTO_WARM_MB` overrides it ([`warm_ceiling`]), and `0` is off.
    pub warm_bytes: usize,
    /// How much **graphics memory** may be spent holding the meshes of cells
    /// that have left the ring, so returning to one is a re-spawn rather than a
    /// re-fetch, re-parse, re-merge and re-upload (`systems::lod22`).
    ///
    /// This is the only cache in the client that spends the quantity
    /// [`Self::resident_bytes_target`] bounds, so three rules travel with it and
    /// none is optional. It is **inside** that target, not added to it — the
    /// bytes appear in `upload_meter::resident_bytes` exactly as live ones do,
    /// because they are live ones that nothing is drawing. It is the **first
    /// thing shed** when `systems::load_control` finds the target exceeded,
    /// which is what makes it safe to hold at all: everything else the
    /// controller can free is on screen, and this is not. And it is **0 on any
    /// handset** ([`handset_guarded`]), because the 2026-08-19 Adreno 840 that
    /// lost its Vulkan device did so holding streamed content it was still
    /// using, with every rate guard in force.
    ///
    /// What it buys where it is on: a LoD2.2 cell returning to range currently
    /// re-reads a multi-megabyte registry file, re-parses it, re-merges the
    /// mesh on a worker and uploads the result. A retained handle skips all
    /// four, and costs no system memory at all — the building meshes are
    /// `RENDER_WORLD` only, so there is no CPU-side copy left to hold.
    pub warm_gpu_bytes: usize,
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

/// The phone tier's per-frame GPU upload ceiling.
///
/// **Sized against a measurement** (2026-08-18, `systems::upload_meter`, a cold
/// cache over Groningen at 1,678 m with `CARTO_QUALITY=phone`): the opening view
/// queued ~215 MB of mesh and texture data in about three and a half seconds, one
/// LoD2.2 cell per frame, the largest of them 63 MB in a single frame. 4 MB is
/// ~240 MB/s at 60 fps — enough that the map still fills in about a second, and
/// two orders of magnitude below the burst that lost an Adreno 840 its device.
///
/// It is a *soft* limit in bevy's sense: one whole asset is always written even if
/// it exceeds the budget, so uploads can never stall on an asset nothing will ever
/// have room for. The real per-frame worst case is therefore this plus the largest
/// single asset — which is why `lod22::MAX_CELL_MESH_BYTES` exists and why
/// `ShotMetrics::upload_peak_asset_kb` is the column to watch. Neither number means
/// much without the other.
const PHONE_UPLOAD_BYTES_PER_FRAME: usize = 4 * 1024 * 1024;

/// The largest flat-map tile raster a handset rasterises, on any rung.
///
/// The same 256 the phone rung and the globe both settled on, and the single
/// biggest term in what a handset has to *hold*: a 512² RGBA mip chain is 1.4 MB
/// against 350 kB, and the clipmap keeps 424 of them. Measured over Groningen on
/// 2026-08-19, that one field is 424 MB — see [`handset_guarded`] for the table.
const HANDSET_TILE_PX: u32 = 256;

/// Samples per side of the terrain field a handset may ask for, whatever rung it is
/// on. 513² over 6.4 km is 12.5 m ground: 780 kB per anchor against 12 MB, and 1 MB
/// resident against 22 MB.
const HANDSET_TERRAIN_GRID: usize = 513;

impl Budget {
    /// The settings the rest of the codebase is written against.
    pub(crate) const fn desktop() -> Self {
        Self {
            tier: Tier::Desktop,
            shadows: true,
            bloom: true,
            sky_ibl: true,
            detail_layers: true,
            water_octaves: 8,
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
            shadow_cascades: 4,
            // 1.5 km, not the 3 km this was. The reach has to cover the casters
            // and nothing else, and the casters are bounded by `lod22`'s
            // `SHADOW_RADIUS = 0` — only the camera's own z14 cell, which is
            // ~1.5 km across at this latitude — plus whatever long shadow a low
            // sun throws off it. The outer cascade was covering the 1.5–3 km band,
            // where by construction nothing casts, and it was being paid for
            // twice: once as a pass, and once as a quarter of the texel budget
            // spent on ground nothing can darken. Halving the reach at the same
            // cascade count is the rare change that is cheaper *and* sharper —
            // every cascade gets ~2× the texel density it had.
            shadow_distance_m: 1500.0,
            // The rung that can afford the good edge, and the one with the widest
            // shadows on screen — a workstation is where a hard 2×2 edge would be
            // most visible.
            shadow_filter: ShadowFilteringMethod::Gaussian,
            // The camera is `Hdr` here (atmosphere + bloom), so this is one
            // full-screen pass and the LUT is worth its fidelity.
            tonemapping: Tonemapping::TonyMcMapface,
            space_raymarch: true,
            tile_px: crate::systems::vector_tiles::TILE_PX,
            terrain_grid: crate::systems::terrain::TERRAIN_GRID,
            render_scale: 1.0,
            fps_cap: Some(60.0),
            upload_bytes_per_frame: None,
            resident_bytes_target: None,
            // 512 MiB, which at 1,365 kB per 512² ground tile is roughly 380 of
            // them — a clipmap's worth of city held behind the live one, so
            // flying out and back costs an upload rather than another 68 ms of
            // scan conversion per tile. This is the machine where `cargo shots`,
            // a route preview and a fly-through all revisit ground they have
            // already drawn.
            //
            // It started at 1 GiB and was halved on 2026-09-12, not from a
            // measurement of what a return journey needs but from one of what
            // this costs: the store reached 339 MB inside four minutes of a
            // ground-level fill over Groningen, on top of a process already
            // holding a Bevy world and ~1 GB of streamed content. Nothing bounds
            // that sum, and the honest position is that the pool should be a
            // fraction of the process rather than a peer of it. `CARTO_WARM_MB`
            // is there for anyone who measures otherwise.
            warm_bytes: 512 * 1024 * 1024,
            // 512 MiB, and the rung with no `resident_bytes_target` at all is the
            // only one where a number this size is not a claim about a ceiling.
            // Groningen's whole streamed scene measured 969 MB on this rung, of
            // which the LoD2.2 cells are the bulk, so this is roughly a ring's
            // worth of buildings held behind the live one — enough that walking
            // out of a district and back is a re-spawn, and not so much that the
            // pool is the largest thing in the process.
            warm_gpu_bytes: 512 * 1024 * 1024,
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
            // Two fewer than a workstation. The pair dropped are the finest, worth
            // ripples of a few centimetres inside ~40 m of the camera — the least
            // of what this rung gives up.
            water_octaves: 6,
            radius_bias: 0,
            per_frame_divisor: 1,
            worker_cap: None,
            // Not `Sample2`: see the field's own comment. Only 1 and 4 are
            // portable, and 4 on an HDR target is precisely the cost this tier
            // exists to avoid — so this rung is the one that gives up edges.
            msaa: Msaa::Off,
            atmosphere_luts: AtmosphereLuts::halved(),
            // Three, and 1 km. An iGPU can afford shadows — this rung keeps them —
            // but not a workstation's cascade count, and neither rung now reaches
            // past what actually casts (see the desktop rung's note on the same
            // field). 1 km still holds a 30 m building's shadow at a 2° sun.
            shadow_cascades: 3,
            shadow_distance_m: 1000.0,
            // The rung's whole brief is "keep the scene's shape, give up what
            // costs fill", and this is the purest example of that on the ladder: a
            // shadow's *placement* is its shape, and the filter decides only how
            // soft the edge is. Nine taps per shadowed fragment per light is fill,
            // and this is the rung Android resolves to — where it is nine taps
            // over 3.5 million pixels.
            shadow_filter: ShadowFilteringMethod::Hardware2x2,
            // This rung's brief is to keep the picture, and the sky is most of it.
            tonemapping: Tonemapping::TonyMcMapface,
            space_raymarch: false,
            tile_px: crate::systems::vector_tiles::TILE_PX,
            terrain_grid: crate::systems::terrain::TERRAIN_GRID,
            // Native resolution: an integrated GPU is bandwidth-limited, not
            // resolution-limited, and this rung's brief is to keep the picture.
            render_scale: 1.0,
            fps_cap: Some(60.0),
            // Unbounded, like the rung above: an iGPU's uploads cross a bus with
            // an order of magnitude more bandwidth than a handset's, and no
            // desktop or laptop run on record has lost its device to a streaming
            // burst. (The Intel iGPU loss CLAUDE.md documents was mesh *churn* —
            // uploads and despawns together — and its guards are the streamers'
            // own caps.)
            upload_bytes_per_frame: None,
            // 700 MB: comfortably above the 567 MB the table in `handset_guarded`
            // measured for this rung with the tile raster capped, and well under
            // the gigabyte that lost a device. Set here rather than left `None`
            // because this is the rung Android's automatic answer resolves to.
            resident_bytes_target: Some(700 * 1024 * 1024),
            // A quarter of the desktop allowance. This is the rung Android's
            // automatic answer resolves to, so the ceiling has to suit a phone
            // with a good GPU rather than a workstation: 256 MiB is ~190 ground
            // tiles at this rung's full-size raster, enough that walking a few
            // blocks and coming back is free, and small enough to be
            // unremarkable beside the app's own footprint. `handset_guarded`
            // holds it down further on Android whatever rung is chosen by hand.
            warm_bytes: 256 * 1024 * 1024,
            // A third of the desktop pool, against a 700 MB residency target it
            // now has to fit inside. `handset_guarded` takes it to 0, so this is
            // what a thin laptop or an integrated GPU gets and not what a phone
            // resolving to this rung gets.
            warm_gpu_bytes: 160 * 1024 * 1024,
        }
    }

    /// Everything that costs fill or main-thread time, off or halved.
    ///
    /// This is not a gentle preset and is not meant to be one: the brief it was
    /// written to was "whatever it takes to hold ~30 fps on a Fairphone 4", and
    /// each of these was chosen because it buys frame time rather than because it
    /// was the least missed.
    ///
    /// **Nothing auto-detects to this rung any more** (2026-08-18): the Fairphone
    /// 4 is no longer a supported device and `auto()` takes [`Self::balanced`] on
    /// Android. It is reached deliberately — Settings ▸ Map ▸ Battery saver,
    /// `CARTO_QUALITY=phone`, or `?quality=phone` on the web, where `index.html`
    /// still detects it — and every comment below should be read as describing
    /// the floor rather than the handset.
    pub(crate) const fn phone() -> Self {
        Self {
            tier: Tier::Phone,
            shadows: false,
            bloom: false,
            sky_ibl: false,
            detail_layers: false,
            // Dead while `detail_layers` is false — the water shading toggle
            // follows that rung, and a still sheet sums no octaves worth counting.
            // It is what the dev panel's water toggle then gets.
            water_octaves: 4,
            radius_bias: 1,
            per_frame_divisor: 2,
            worker_cap: Some(3),
            msaa: Msaa::Off,
            atmosphere_luts: AtmosphereLuts::halved(),
            // Dead while `shadows` is false, which is the default here — but the
            // dev panel can turn shadows on, and this is what it then gets. Two
            // cascades to 800 m is a shadow you can stand in, not a shadow map of
            // the horizon.
            shadow_cascades: 2,
            shadow_distance_m: 800.0,
            // Same reasoning as the rung above, more so: this is the floor, and a
            // machine that reaches it has no fill to spend on an edge.
            shadow_filter: ShadowFilteringMethod::Hardware2x2,
            // No atmosphere and no bloom means no `Hdr`, which means the curve
            // runs per fragment rather than once — so it must not sample a LUT.
            tonemapping: Tonemapping::AcesFitted,
            space_raymarch: false,
            // Half of `vector_tiles::TILE_PX`, and the same 256 the globe settled
            // on. Written out rather than derived from `TILE_PX / 2` so raising
            // the flat map's raster size later does not silently raise the
            // phone's with it.
            tile_px: 256,
            // Quarter the samples per side of the desktop rungs: 513² is 12.5 m
            // ground, 780 kB on the wire and 1 MB resident. Two rungs coarser than
            // the box a workstation gets and still twice as fine as the 25 m every
            // machine had until 2026-09-13.
            terrain_grid: 513,
            // A half, which is **a quarter of the fragments**: a Fairphone 4's
            // 1080×2340 (2.53 Mpx) is rendered at 540×1170 (0.63 Mpx) and blitted
            // up. The chrome is untouched at the full panel resolution, so what
            // this softens is the map and nothing that carries text.
            //
            // Exactly a half rather than the 0.7 this started at. Historically
            // that was forced — Bevy's `upscaling` blit point-sampled, so only an
            // integer reciprocal upscaled cleanly. `render_scale` has since
            // swapped the blit to a `Filtering` sampler (the scale is a user
            // slider now, and every position must look right), so 1/2 is merely
            // the cheapest of the good-looking scales rather than the only one.
            //
            // This is now deliberately *below* the 1.3 Mpx ceiling `index.html`
            // enforces for the web phone tier, where the upscale is a CSS transform
            // the compositor filters for free. The two platforms' phone budgets no
            // longer mean the same number of pixels, and that is the honest position:
            // one of them pays for its upscale and the other does not.
            render_scale: 0.5,
            upload_bytes_per_frame: Some(PHONE_UPLOAD_BYTES_PER_FRAME),
            // 350 MB, against the 307 MB this rung measured over Groningen.
            resident_bytes_target: Some(350 * 1024 * 1024),
            // Leave pacing to FIFO. See the field's own comment: a sleep-based
            // limiter layered on vsync is a judder source, not a saving, and the
            // handset is the platform that runs `AutoVsync`.
            fps_cap: None,
            // Off. This rung is reached by a person asking for battery saver or
            // by a machine that could not hold 30 fps, and on both the scarce
            // thing is memory rather than the redraw: `crates/geo/src/perf.rs`
            // is built on the observation that on a phone allocation *is*
            // malloc contention plus cache pressure plus, eventually, the
            // low-memory killer. Trading that for a faster return to ground the
            // camera has already left is the wrong way round here.
            warm_bytes: 0,
            // Off, for the reason above it and one sharper: this rung's whole
            // brief is a machine that could not hold a frame rate, and holding
            // graphics memory for a view nobody is looking at is the last thing
            // such a machine should spend.
            warm_gpu_bytes: 0,
        }
    }

    /// The frame rate this tier wants to be *presented* at.
    ///
    /// Distinct from [`Self::fps_cap`], and the difference is the point.
    /// `fps_cap` drives `frame_limiter`, a `thread::sleep` that only makes sense
    /// where the client is racing an unsynchronised presentation mode; it is
    /// `None` on the phone precisely because Android runs `AutoVsync` and a second
    /// pacer would beat against the first.
    ///
    /// This is the number to *tell the compositor*, which is a different
    /// conversation: it says "do not run me faster than this", and the platform
    /// obliges by picking a display mode rather than by us sleeping. Without it a
    /// 120 Hz panel renders a 30 fps-budgeted scene at 120 and the phone is
    /// thermally throttled for the privilege.
    ///
    /// 60 everywhere else, matching what the limiter would have done.
    // Its only non-test caller is `report_frame_to_scheduler`'s Android arm, which
    // is `cfg`-ed away everywhere else — so off the handset the tests below are the
    // only users, and the workspace's `unused = "deny"` would otherwise fail the
    // native lane for a method that exists precisely to be called on one target.
    // Same arrangement as `lib.rs::android_property_name`.
    #[cfg_attr(not(target_os = "android"), allow(dead_code))]
    #[must_use]
    pub fn target_fps(&self) -> f32 {
        match self.tier {
            // Not 30. The phone budget's brief is "whatever it takes to hold ~30 fps
            // on a Fairphone 4", but that is a floor it aims to clear, not a rate to
            // pin a better device to — and this is a hint the platform rounds to a
            // mode it has. Asking for 60 lets a handset that can hold 60 run at 60
            // and one that cannot fall to its own half-rate, while still telling a
            // 120 Hz panel not to spend twice the power on frames nobody asked for.
            Tier::Phone => 60.0,
            _ => self.fps_cap.unwrap_or(60.0) as f32,
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

    /// The sample count the main pass actually gets, with every clamp applied.
    ///
    /// [`Self::msaa`] is what the *tier* asks for, and `restricted` is what can
    /// overrule it: the web and a software adapter both permit only 1 and 4
    /// samples, and an unsupported request does not degrade — it fails to create
    /// the main texture and renders nothing (see [`Self::msaa`]). That is a
    /// property of *this run's surface and adapter*, so the caller answers it.
    ///
    /// There used to be a second overrule here, `occlusion`, because bevy's SSAO
    /// node requires `Msaa::Off` and merely warns if it does not get it — a run
    /// asking for both got the multisampling and no occlusion. The occlusion pass
    /// was removed (2026-09-21, see `render_features`), so the clamp went with it
    /// and multisampling is available again wherever the surface permits it.
    ///
    /// Returning the answer from one function is what makes
    /// [`wants_edge_filter`](Self::wants_edge_filter) truthful: the edge filter
    /// has to go on exactly when the samples went away.
    #[must_use]
    pub fn main_pass_msaa(&self, restricted: bool) -> Msaa {
        if restricted {
            Msaa::Off
        } else {
            self.msaa
        }
    }

    /// Whether this run should carry `Fxaa`.
    ///
    /// Exactly when the main pass ends up with one sample and the scene is still
    /// worth filtering — i.e. when something took the multisampling away. A rung
    /// that never had any (the phone) does **not** gain a full-screen pass here:
    /// it gave up edge quality on purpose and an extra pass is the thing it has
    /// least of.
    #[must_use]
    pub fn wants_edge_filter(&self, restricted: bool) -> bool {
        self.msaa != Msaa::Off && self.main_pass_msaa(restricted) == Msaa::Off
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

/// Say again, where a phone can read it, which budget this run took.
///
/// **[`init`]'s own line goes nowhere on the one platform that needs it.** It is
/// called at the top of `run()` — it has to be, `platform::workers` sizes itself
/// from the answer — and that is *before* `DefaultPlugins` installs `LogPlugin`,
/// which on Android is the only thing that writes to logcat at all (stderr is
/// discarded there). So the tier has never appeared in an Android log, and on
/// 2026-08-19 two crash reports from the same device had to have their rung
/// *inferred* from `peak_asset_kb` — 4,095 kB meaning a 4 MB ceiling, 7,920
/// meaning none. That inference is what found [`handset_guarded`]'s bug, and it
/// should not have been necessary.
///
/// A `Startup` system rather than a second call inside `init`: the logger exists
/// by then on every platform, and this is the same reason `init_capabilities`
/// reports from `setup_camera`.
///
/// [`upload_ceiling`] and not the field, because that is the number in force —
/// `CARTO_UPLOAD_KB` is folded in, and a report that names the tier's answer while
/// the device runs another is the kind of half-true line this whole module keeps
/// trying to delete.
pub fn report_budget() {
    let resolved = budget();
    tracing::info!(
        tier = ?resolved.tier,
        chose = SOURCE.get().copied().unwrap_or("auto"),
        shadows = resolved.shadows,
        // Beside `shadows` because it is the shadow setting a reader is least
        // likely to guess: it costs per pixel rather than per caster, and
        // `CARTO_SHADOW_FILTER` can have moved it off the rung's own answer.
        shadow_filter = ?resolved.shadow_filter,
        shadow_cascades = resolved.shadow_cascades,
        shadow_distance_m = resolved.shadow_distance_m,
        bloom = resolved.bloom,
        sky_ibl = resolved.sky_ibl,
        detail_layers = resolved.detail_layers,
        msaa = resolved.msaa.samples(),
        space_raymarch = resolved.space_raymarch,
        tile_px = resolved.tile_px,
        render_scale = resolved.render_scale,
        upload_kb = upload_ceiling().map_or(0, |bytes| bytes / 1024),
        warm_mb = warm_ceiling() / (1024 * 1024),
        warm_gpu_mb = warm_gpu_ceiling() / (1024 * 1024),
        "render budget in force"
    );
}

/// Which of [`detect`]'s three answers this run took, for [`report_budget`].
///
/// A tier alone does not say whether a person chose it, and the difference is the
/// first question of every report: `Balanced` from the settings screen and
/// `Balanced` from `auto()` are the same struct and different bugs.
static SOURCE: OnceLock<&'static str> = OnceLock::new();

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

/// How many bytes a frame may hand the GPU, with `CARTO_UPLOAD_KB` folded in.
///
/// One resolution of the question, read by `stream_ledger` and `map_stream`'s own
/// tile cap. It also fed bevy's `RenderAssetBytesPerFrame` until 2026-09-26, when
/// that throttle was switched off for drawing deferred meshes with the wrong
/// buffers (`lib.rs` says how); the notes below about its scripted-capture
/// exemption describe why the two budgets were kept apart while both existed.
///
/// `0` means no ceiling, which is how a run opts out entirely.
///
/// **The scripted-capture exemption is deliberately not here.** It belongs to
/// bevy's budget alone and `lib.rs` applies it there: a deferred *bevy write* is
/// invisible to a settle test that watches streamers, so a throttled capture can
/// photograph a half-uploaded world and call itself settled. A deferred *tile* is
/// the opposite — it sits in `MapStream`, `awaiting_upload` counts it and the
/// settle test blocks on `map_tiles` — so the tile cap can be honoured in a
/// capture, and has to be, or the harness measures a code path the device does not
/// run.
/// Memoised, because the callers are hot: `map_stream` asks once a frame and
/// `scatter` once a cell, while `env_value` is a system-property read on Android
/// and a query-string parse on the web. The answer cannot change during a run —
/// same reason [`BUDGET`] is a `OnceLock`.
#[must_use]
pub fn upload_ceiling() -> Option<usize> {
    static CEILING: OnceLock<Option<usize>> = OnceLock::new();
    *CEILING.get_or_init(|| {
        match crate::env_value("CARTO_UPLOAD_KB").and_then(|v| v.parse::<usize>().ok()) {
            Some(0) => None,
            Some(kb) => Some(kb * 1024),
            None => budget().upload_bytes_per_frame,
        }
    })
}

/// Bytes of system memory the warm cell store may hold, with `CARTO_WARM_MB`
/// folded in. `0` is off.
///
/// The same shape as [`upload_ceiling`] and memoised for the same reason — every
/// streamer asks once at construction, and `env_value` is a system-property read
/// on Android and a query-string parse on the web.
///
/// **No scripted-capture exemption**, unlike `load_control`'s ring and like
/// `stream_ledger`'s budget, and the rule that decides which way round is what
/// the mechanism does when it acts. A narrower ring means content that is never
/// fetched, so two `cargo shots` runs would differ for a reason no image could
/// show. A warm hit produces the *same bytes* the rebuild would have produced —
/// it changes how long a capture takes to settle and nothing about what settles
/// — and a manifest run is six viewpoints over one city in one launch, which is
/// the revisiting case this exists for.
#[must_use]
pub fn warm_ceiling() -> usize {
    static CEILING: OnceLock<usize> = OnceLock::new();
    *CEILING.get_or_init(|| {
        match crate::env_value("CARTO_WARM_MB").and_then(|v| v.parse::<usize>().ok()) {
            Some(mb) => mb * 1024 * 1024,
            None => budget().warm_bytes,
        }
    })
}

/// Bytes of graphics memory the retained-mesh pool may hold, with
/// `CARTO_WARM_GPU_MB` folded in. `0` is off, which is every handset.
///
/// Separate from [`warm_ceiling`] because the two bound different scarcities and
/// a single knob would tie them together: a workstation wants a large system
/// memory cache *and* a large retained-mesh pool, while a phone wants a small
/// one of the first and none of the second.
#[must_use]
pub fn warm_gpu_ceiling() -> usize {
    static CEILING: OnceLock<usize> = OnceLock::new();
    *CEILING.get_or_init(|| {
        match crate::env_value("CARTO_WARM_GPU_MB").and_then(|v| v.parse::<usize>().ok()) {
            Some(mb) => mb * 1024 * 1024,
            None => budget().warm_gpu_bytes,
        }
    })
}

/// Shorthand for the common `budget().tier == Tier::Phone` test.
#[must_use]
pub fn is_phone() -> bool {
    budget().tier == Tier::Phone
}

/// What this run's hardware and window actually report.
///
/// [`Budget`] answers *"how much frame may this class of machine spend"*, which is
/// a judgement about a class and has to be made before a renderer exists —
/// `platform::workers` is sized from it. This answers the different question
/// *"what can **this** machine do"*, from the adapter, the device and the window,
/// and it is therefore only available once Bevy has all three
/// (`free_camera::setup_camera`, which is where [`init_capabilities`] is called).
///
/// **Everything here is measured. Nothing here is a name, a platform or a model
/// number.** That rule is the point of the type and it was learned twice. A
/// `cfg!(target_os = "android")` meant "a Fairphone 4" until a OnePlus 15 arrived
/// and inherited a 2021 mid-ranger's sky; the fix for *that* was a table of GPU
/// families (`Adreno >= 700`, `Mali-G >= 700`, `Xclipse >= 900`) which is the same
/// substitution one level down — a guess about hardware nobody in the loop owns,
/// wrong by default for every part not on the list, and needing an edit for each
/// new one. An adapter that can filter a 32-bit float texture says so, and asking
/// is one line.
///
/// So: if a question can be *asked* of the device, it belongs here. If it can only
/// be *judged* — is this scene worth the fill it costs — it belongs in [`Budget`].
#[derive(bevy::ecs::resource::Resource, Clone, Copy, Debug)]
pub struct Capabilities {
    /// `FLOAT32_FILTERABLE`, and with it bevy_pbr's real atmosphere.
    ///
    /// Bevy 0.19 bakes each `ScatteringMedium` into `Rgba32Float` lookup textures
    /// read through a *filtering* sampler, which wgpu permits only on an adapter
    /// that offers this. Without it the bind group fails validation and takes the
    /// whole app down, so `systems::sky_dome` stands in — see
    /// `docs/notes/sky-without-an-atmosphere.md`.
    pub float32_filterable: bool,
    /// The presentation mode already blocks on the display's own cadence.
    ///
    /// True for FIFO and its relatives, which is what `PresentMode::AutoVsync`
    /// resolves to. **A second pacer on top of one of these is a bug, not a
    /// saving**: `lib.rs::frame_limiter` sleeps the main thread to a deadline it
    /// measures from a different point in the pipeline than the swapchain does,
    /// and two clocks beating against each other is the standard way to get
    /// alternating judder on a device with headroom to spare. Reading it off the
    /// window rather than off the platform is what stops that rule from having to
    /// be restated for every new target.
    pub vsync_paced: bool,
    /// The adapter is a CPU rasteriser — Mesa lavapipe in this container and in
    /// CI, or a driver-less machine that fell back to one.
    ///
    /// Recorded rather than re-derived, because three places now need the same
    /// answer and one of them (`systems::light_shafts`) runs long after the
    /// adapter has been queried. It already decided the sample count
    /// (`Budget::main_pass_msaa`, whose `restricted` argument this feeds); it now
    /// also decides whether a per-pixel ray march is attempted at all.
    ///
    /// **This is not "software is slow, so cut quality".** Colour, exposure and
    /// lighting are not evidence on a CPU rasteriser at all
    /// (`docs/notes/headless-shots-software-renderer.md`), so a volumetric march
    /// there is minutes of frame time spent on a picture nobody may draw a
    /// conclusion from.
    pub software_renderer: bool,
    /// The backend can run a compute shader.
    ///
    /// True everywhere this client currently ships — native Vulkan/Metal/DX12,
    /// lavapipe, and WebGPU — and false on WebGL2, which has no compute stage at
    /// all.
    ///
    /// **Nothing gates on it any more**, and it is kept because it is a
    /// measurement rather than a setting: it was the hard requirement for bevy's
    /// ambient-occlusion pass, which was removed on 2026-09-21
    /// (`render_features`), and `report_capabilities` still logs it because "what
    /// could this adapter have done" is the first question asked of a device that
    /// renders differently from every other.
    pub compute_shaders: bool,
}

static CAPABILITIES: OnceLock<Capabilities> = OnceLock::new();

/// Record what the adapter, device and window turned out to be. Called once, from
/// `free_camera::setup_camera`.
///
/// Idempotent like [`init`], and for the same reason: a second call from a test
/// harness or a second camera should be a no-op rather than a panic.
pub fn init_capabilities(caps: Capabilities) {
    if CAPABILITIES.set(caps).is_ok() {
        tracing::info!(
            float32_filterable = caps.float32_filterable,
            vsync_paced = caps.vsync_paced,
            software_renderer = caps.software_renderer,
            compute_shaders = caps.compute_shaders,
            "device capabilities measured"
        );
    }
}

/// What this run's device reports, or `None` before the renderer exists.
///
/// `Option`, deliberately, and callers must handle it rather than defaulting: the
/// systems that read this run in `Last` from frame one, while `setup_camera` is a
/// `Startup` system, so there genuinely are frames with no answer yet. Inventing
/// one here would be the same "a class of machine stands in for the machine" this
/// type exists to remove.
#[must_use]
pub fn capabilities() -> Option<Capabilities> {
    CAPABILITIES.get().copied()
}

fn detect(preferred: Option<&str>) -> Budget {
    // An explicit setting wins everywhere, which is the whole point of it: it is
    // how the phone budget gets exercised on a machine that can profile it.
    if let Some(v) = crate::env_value("CARTO_QUALITY") {
        match parse(&v) {
            Some(budget) => {
                let _ = SOURCE.set("CARTO_QUALITY");
                return on_this_machine(budget);
            }
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
        let _ = SOURCE.set("settings");
        return on_this_machine(budget);
    }
    let _ = SOURCE.set("auto");
    on_this_machine(auto())
}

/// Apply the guards that belong to the *machine* rather than to the rung.
///
/// `CARTO_HANDSET` applies them on any machine. `CARTO_QUALITY=phone` picks the
/// rung a handset *could* be put on; a real phone runs `balanced` through these
/// guards, and `CARTO_QUALITY=balanced CARTO_HANDSET=1` is that configuration on a
/// machine that can capture it — the handset's terrain grid and the graded tiles
/// thinned for it included.
///
/// `cfg!` rather than `#[cfg]` so [`handset_guarded`] is compiled — and testable —
/// on every target. That is the same trap [`android_budget`] fell into: a rule
/// that only exists in the Android build is a rule no test on any other host can
/// see.
fn on_this_machine(budget: Budget) -> Budget {
    let budget = if cfg!(target_os = "android") || crate::env_value("CARTO_HANDSET").is_some() {
        handset_guarded(budget)
    } else {
        budget
    };
    with_shadow_cascade_override(with_shadow_filter_override(budget))
}

/// Apply `CARTO_SHADOW_CASCADES`, if it is set.
///
/// The companion to [`with_shadow_filter_override`] and it exists for the same
/// reason: the two halves of what shadows cost are the **caster** passes, one per
/// cascade, and the **per-fragment** lookup, and telling them apart needs a lever
/// on each that does not need a rebuild.
///
/// Measured on the Deck 2026-09-22, `city` at 3 km: the filter is worth 0.63 ms of
/// the opaque pass and **nothing at all** to the frame, while the whole of shadows
/// is most of a frame that the pass timers never see — one caster pass per cascade
/// is rendered and `RenderDiagnosticsPlugin` reports none of them. So the number
/// worth sweeping is this one, and until now there was no way to sweep it short of
/// moving the whole tier.
///
/// Clamped to bevy's own 1..=4 rather than passed through: `shadow_cascades` is
/// shared by the sun and the moon precisely so the two can never disagree (bevy
/// panics outright if they do — CLAUDE.md), and a flag that could put a 0 or a 9
/// in there would hand that panic back through the one door left open. 0 is not
/// how you turn shadows off; `CARTO_NO_SHADOWS` is.
fn with_shadow_cascade_override(budget: Budget) -> Budget {
    let Some(n) = crate::env_value("CARTO_SHADOW_CASCADES")
        .and_then(|v| v.trim().parse::<usize>().ok())
        .map(|n| n.clamp(1, 4))
    else {
        return budget;
    };
    Budget {
        shadow_cascades: n,
        ..budget
    }
}

/// Apply `CARTO_SHADOW_FILTER`, if it is set.
///
/// Applied *after* the handset guard rather than before, which is the order every
/// debugging switch in this module wants: the guard is a rule about the machine
/// and a flag is an instruction for this run, and a flag that the guard could
/// overrule would be a flag that silently did nothing on the one device it exists
/// to measure.
fn with_shadow_filter_override(budget: Budget) -> Budget {
    let Some(filter) = crate::env_value("CARTO_SHADOW_FILTER").and_then(|v| parse_filter(&v))
    else {
        return budget;
    };
    Budget {
        shadow_filter: filter,
        ..budget
    }
}

/// The two words `CARTO_SHADOW_FILTER` takes.
///
/// Named for what they cost rather than for bevy's variants: `Gaussian` and
/// `Hardware2x2` describe the algorithm, and the question anybody setting this is
/// asking is which of them is fast and which is soft. Anything else is `None` —
/// so a typo falls through to the rung rather than resolving to some third
/// answer, and `detect`'s own logging still reports what was resolved.
fn parse_filter(value: &str) -> Option<ShadowFilteringMethod> {
    match value.trim().to_ascii_lowercase().as_str() {
        "fast" | "hard" | "hardware" | "2x2" => Some(ShadowFilteringMethod::Hardware2x2),
        "soft" | "gaussian" | "pcf" => Some(ShadowFilteringMethod::Gaussian),
        _ => None,
    }
}

/// What a handset cannot afford on **any** rung: the two guards a phone keeps
/// whoever picked the preset, and whatever the preset says.
///
/// Both are about GPU memory and they are the two halves of it — a **rate**
/// (`upload_bytes_per_frame`, how much a frame may hand over) and a **total**
/// (`tile_px` and `radius_bias`, how much ends up being held at once). Everything
/// else the rung asks for is a judgement about the picture and is left alone: a
/// handset on "Full detail" still gets shadows, bloom, the detail layers and its
/// native render scale.
///
/// # The rate
///
/// [`android_budget`] kept `upload_bytes_per_frame` when Android's *automatic*
/// answer moved from [`Budget::phone`] to [`Budget::balanced`], and its comment
/// says why: this is the only hardware known to lose its device to a streaming
/// burst. What that fix did not cover is the person who picks a rung by hand.
/// Settings ▸ Graphics ▸ Detail writes `QualityPreset::Medium` → the word
/// `balanced` → `Budget::balanced()` **verbatim**, ceiling and all — so choosing
/// "Balanced" or "Full detail" on the phone silently deleted the one guard the
/// phone needs, from a screen whose whole purpose is to help a struggling device.
///
/// Two OnePlus 15 (Adreno 840) reports five minutes apart on 2026-08-19 are the
/// same run with and without it, and the give-away is the largest single asset —
/// `lod22::max_cell_mesh_bytes` derives its cut from this field:
///
/// | run | `peak_asset_kb` | ceiling in force |
/// |---|---|---|
/// | 23:38, `Auto` | 4,095 | 4 MB (`android_budget`) |
/// | 23:43, an explicit rung | 7,920 | none (`Budget::balanced`) |
///
/// The second run also logged eleven `single asset above the frame upload budget`
/// lines that the first never printed, and lost the device 4.3 s in.
///
/// A *lower* ceiling survives: the minimum, not an overwrite, so
/// `QualityPreset::Low` keeps the phone rung's own number and a future rung that
/// asks for less than 4 MB is not raised to it. `CARTO_UPLOAD_KB` remains the
/// override for whoever holds the device (`0` = no ceiling), and it is applied
/// downstream in [`upload_ceiling`], so this cannot take that away either.
///
/// **Why this is a property of the machine and not of the rung.** On 2026-08-17
/// an Adreno 840 lost its Vulkan device five seconds into a cold start, handed
/// ~215 MB of mesh and texture data in three and a half seconds, *while already on
/// the phone budget*. `upload_bytes_per_frame` is the guard written for it, and it
/// is `None` on every rung above the phone — reasonably, since those rungs
/// describe machines whose uploads cross a bus with an order of magnitude more
/// bandwidth and no desktop run has ever lost a device to streaming. A handset on
/// one of those rungs is neither case, and it is the *rung* that raises the
/// pressure: 512 px tile rasters are four times the texture bytes of 256, and
/// `radius_bias: 0` nearly doubles the tile count. With the ceiling kept, that
/// extra volume costs fill latency — the map sharpens a little later — rather than
/// a black screen and a crash report.
///
/// Revisit the number itself on the device, with `systems::upload_meter`'s
/// `upload_peak_kb` beside `lod22::max_cell_mesh_bytes`; neither means much
/// without the other. Not from a desk.
///
/// # The total, which no ceiling can reach
///
/// The rate guard was in force in the 23:38 run **and the device was lost anyway**,
/// three seconds in. It could not have helped: deferring an upload changes when a
/// tile arrives, never whether it stays, and a driver that refuses an allocation is
/// failing on what is held rather than on what arrived this frame. Nothing measured
/// that until `upload_meter` grew `resident_bytes`; the first thing it said is that
/// the phone's own report already contained the number, as `upload_total_mb = 993`
/// against an asset census that had released almost none of it.
///
/// Measured here, Groningen at 150 m with a warm tile cache, `--dump-state`'s
/// `gpu_resident_kb` (asset bytes, so machine-independent — the container's
/// lavapipe changes the frame times in that run and none of this):
///
/// | configuration | resident | draws | settled |
/// |---|---|---|---|
/// | the handset rung of 2026-08-18 | **991 MB** | 695 | 45.1 s (hit the cap) |
/// | + `tile_px` 256 | 567 MB | 695 | 27.3 s |
/// | + `radius_bias` 1 — **this** | **371 MB** | 527 | 15.7 s |
/// | `Budget::phone`, for scale | 307 MB | 225 | 12.9 s |
///
/// A gigabyte of streamed content is not a budget any handset has, and it arrived
/// by accident: moving Android to `balanced` on 2026-08-18 took `tile_px` 256 → 512
/// (four times the texture bytes of every one of the clipmap's 424 tiles) and
/// `radius_bias` 1 → 0 (more tiles, and more of the LoD2.2 cells that are the other
/// half of the total) — two fields whose cost is *memory*, in a change reasoned
/// about entirely in terms of fill and frame time. The two caps take it back to
/// 371 MB, within a quarter of the rung that was shipping before, while keeping
/// everything that change was actually for.
///
/// The honest limit of this: **991 → 371 MB is measured, "and therefore the device
/// survives" is not.** No harness here can lose a Vulkan device. What can be said
/// is that the app now asks for a third of what it asked for on the run that
/// crashed, and that the number is in every report from now on.
#[must_use]
pub(crate) fn handset_guarded(budget: Budget) -> Budget {
    let ceiling = budget
        .upload_bytes_per_frame
        .map_or(PHONE_UPLOAD_BYTES_PER_FRAME, |own| {
            own.min(PHONE_UPLOAD_BYTES_PER_FRAME)
        });
    let resident = budget
        .resident_bytes_target
        .map_or(HANDSET_RESIDENT_BYTES, |own| {
            own.min(HANDSET_RESIDENT_BYTES)
        });
    Budget {
        upload_bytes_per_frame: Some(ceiling),
        tile_px: budget.tile_px.min(HANDSET_TILE_PX),
        // The terrain box, on the same rule as `tile_px`: a rung picked by hand in
        // Settings ▸ Graphics must not hand a phone a 12 MB request per anchor and
        // 22 MB of samples to hold. The minimum, so the phone rung's own 513 stands.
        terrain_grid: budget.terrain_grid.min(HANDSET_TERRAIN_GRID),
        radius_bias: budget.radius_bias.max(1),
        // The total, guarded the same way and for the same reason as the rate: a
        // rung picked by hand in Settings must not delete a ceiling the machine
        // needs. The minimum, not an overwrite, so a rung asking for less keeps
        // its own number.
        resident_bytes_target: Some(resident),
        // System memory, guarded on the same principle as the two above but for
        // a different scarcity: a handset's risk is the low-memory killer, not
        // the graphics device. The minimum, so the phone rung's own `0` survives
        // and only a rung asking for *more* is brought down.
        warm_bytes: budget.warm_bytes.min(HANDSET_WARM_BYTES),
        // **Zero, not a minimum.** Every other guard here takes the smaller of
        // two numbers, because every other one is a rung asking for too much of
        // something a handset has some of. This is the one cache that spends the
        // resource whose exhaustion is a lost Vulkan device and a crash report,
        // and no amount of it is worth a phone's return journey being quick.
        warm_gpu_bytes: 0,
        ..budget
    }
}

/// What a handset may keep in system memory for a return journey, whatever rung
/// it is on.
///
/// 128 MB — roughly 90 of this rung's 256² ground tiles, so a walk round a block
/// and back is free while a fly-out is not. Deliberately a small fraction of
/// [`HANDSET_RESIDENT_BYTES`]: a phone that is already holding 400 MB of streamed
/// content on the GPU, a Bevy world, an egui context and the browser-sized native
/// heap behind them has no business quietly adding another 256 MB it never
/// reports. Chosen rather than measured — nothing in this container can watch an
/// Android process get killed for its resident set — and it is one const so a
/// measurement can move it.
const HANDSET_WARM_BYTES: usize = 128 * 1024 * 1024;

/// What a handset may hold, whatever rung it is on.
///
/// 400 MB, just above the **371 MB** the table above measured for the guarded
/// configuration over Groningen — so on the ground this was tuned against the
/// controller never acts, and it is only somewhere denser that the ring narrows.
const HANDSET_RESIDENT_BYTES: usize = 400 * 1024 * 1024;

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
        // The rung Android resolves to, by name, on every platform — so the
        // handset's real budget can be run on a machine that can profile it.
        // `balanced` is **not** a substitute for it *off* the handset: there it is
        // the same rung with no upload ceiling at all, which is exactly the field a
        // phone's cold start turns on. Naming it is what stopped
        // `CARTO_QUALITY=balanced CARTO_UPLOAD_KB=4096` from being a thing anyone
        // had to know. On Android every word here comes back guarded anyway — see
        // `handset_guarded`, which is the fix for the half of this that a settings
        // screen could reach.
        "android" | "handset" => Some(android_budget()),
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

/// A handset, and since 2026-08-18 that means [`Budget::balanced`] rather than
/// [`Budget::phone`].
///
/// **This is a target change, not a re-tuning.** Every number in `phone()` was
/// chosen against a Fairphone 4 — its own doc says the brief was "whatever it
/// takes to hold ~30 fps on a Fairphone 4" — and that device is no longer
/// supported. The phone this client is developed against now is a Snapdragon 8
/// Elite Gen 5 (Adreno 840): 18 MB of on-chip tile memory against the 619's
/// 512 KiB, ~5× the memory bandwidth, and eight performance cores with no
/// efficiency cores at all. Three of the arguments `phone()` is built from are
/// therefore void on it — the tile budget that rules out `Hdr`, the fill
/// scarcity that halves the render scale, and the 2-big-6-little thermal
/// envelope that caps the worker pool.
///
/// `Balanced` is not a new rung invented for this; it is the rung whose own
/// documentation already describes the device — *"an integrated GPU, a thin
/// laptop, **or a handset good enough to want more than the floor**"*. Taking it
/// changes render scale 0.5 → 1.0, the tile raster 256 → 512 px, the clipmap
/// back to its full 424 tiles, the per-frame build budgets back to undivided,
/// the worker pool off its cap, and turns the detail layers, shadows and bloom
/// on.
///
/// **Deliberately not adapter-aware, because it cannot be.** `init` runs at the
/// top of `run()`, before a render device exists — `platform::workers` is sized
/// from the result — so this function has no adapter to ask. Anything that *can*
/// be asked of the device is not decided here at all: it belongs in
/// [`Capabilities`], which is resolved once there is an adapter, a device and a
/// window to ask. The sky is the worked example — this function used to carry a
/// `sky_dome` field guessing at it, and the guess was wrong on the first flagship
/// that ran the app.
///
/// `phone()` remains reachable, and is what a device that cannot hold this
/// should be put on: Settings ▸ Map ▸ Battery saver, or `CARTO_QUALITY=phone`.
#[cfg(all(not(target_arch = "wasm32"), target_os = "android"))]
fn auto() -> Budget {
    android_budget()
}

/// The rung a handset actually runs: [`Budget::balanced`] with the burst guard kept.
///
/// **Its own function, and compiled on every target, because the tests could not
/// see it.** This was an inline struct literal inside a `cfg(target_os =
/// "android")` `auto()`, so it existed only in the Android build — while
/// `no_rung_permits_an_asset_larger_than_its_own_frame`, the test written
/// precisely to stop a single asset from exceeding the frame budget, iterates
/// `phone`/`balanced`/`desktop` and `continue`s on a `None` ceiling. `balanced`
/// has a `None` ceiling. So the one configuration that has a ceiling *and* a
/// handset behind it was the one configuration nothing checked, on a host where it
/// did not compile. Every number below is unchanged; only its reachability is.
///
/// Since 2026-08-19 it is [`handset_guarded`] applied to `balanced` rather than a
/// literal of its own: the guard below turned out to be needed for every rung a
/// handset can end up on, not only the automatic one, so it belongs in a function
/// the *machine* applies rather than in the one rung `auto()` happens to pick.
#[must_use]
pub(crate) fn android_budget() -> Budget {
    handset_guarded(Budget::balanced())
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
            assert!(
                lower.terrain_grid <= upper.terrain_grid,
                "terrain_grid: {name}"
            );
            assert!(
                lower.shadow_cascades <= upper.shadow_cascades,
                "shadow_cascades: {name}"
            );
            assert!(
                lower.shadow_distance_m <= upper.shadow_distance_m,
                "shadow_distance_m: {name}"
            );
            // `ShadowFilteringMethod` has no ordering of its own — it is an enum
            // of three named techniques, not a quality number — so the ladder
            // needs one stated here. Ranking it rather than asserting the pair
            // directly is what keeps the rule readable if a fourth filter ever
            // arrives: give it a rank and the test keeps meaning what it says.
            fn filter_cost(f: ShadowFilteringMethod) -> u8 {
                match f {
                    ShadowFilteringMethod::Hardware2x2 => 0,
                    // Eight taps to nine, so they rank together; neither is
                    // reachable from a rung today (see `Budget::shadow_filter`).
                    ShadowFilteringMethod::Temporal => 1,
                    ShadowFilteringMethod::Gaussian => 1,
                }
            }
            assert!(
                filter_cost(lower.shadow_filter) <= filter_cost(upper.shadow_filter),
                "shadow_filter: {name}"
            );
            assert!(lower.msaa.samples() <= upper.msaa.samples(), "msaa: {name}");
            assert!(
                lower.water_octaves <= upper.water_octaves,
                "water_octaves: {name}"
            );
            // A LUT tonemapper is the expensive one, and it is only affordable on
            // a rung whose camera is `Hdr` — where the curve is one pass rather
            // than per fragment. Tying the two together here is what stops a
            // future rung turning the atmosphere off and quietly moving a 3D
            // texture fetch into every mesh shader.
            assert!(
                lower.tonemapping != Tonemapping::TonyMcMapface
                    || upper.tonemapping == Tonemapping::TonyMcMapface,
                "tonemapping: {name}"
            );
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

    /// Every rung has a real rate to offer the compositor, including the one that
    /// deliberately has no `fps_cap`.
    ///
    /// The phone's `fps_cap` is `None` because a `thread::sleep` limiter layered on
    /// `AutoVsync` is two pacers beating against each other — but "do not sleep" and
    /// "run as fast as the panel will go" are different instructions, and reading
    /// the first as the second is what leaves a 120 Hz handset rendering a 30
    /// fps-budgeted scene at 120 until it throttles. `target_fps` is the second
    /// conversation and must never be zero.
    ///
    /// **This is not the guard, and mistaking it for one cost a release.** It
    /// asserts what `target_fps()` returns; between 2026-08-19 and 2026-08-21 the
    /// call site passed a hardcoded `0.0` to `ANativeWindow_setFrameRate` instead
    /// of passing this on, and the test went on being green about a number nobody
    /// read — the same demotion the sibling test below records for `fps_cap`. What
    /// the ask actually resolves to is pinned by
    /// `frame_pacing::rate_tests::the_tier_s_target_is_what_gets_asked_for`.
    #[test]
    fn every_rung_asks_the_compositor_for_a_rate() {
        for budget in [Budget::phone(), Budget::balanced(), Budget::desktop()] {
            let fps = budget.target_fps();
            assert!(
                fps >= 30.0 && fps <= 120.0,
                "{:?} asked for {fps} fps",
                budget.tier
            );
        }
    }

    /// The phone leaves pacing to the presentation mode; the others cap.
    ///
    /// Not a taste question. A FIFO swapchain blocks on acquire, so it is already a
    /// clock, and a sleep-based limiter on top is a second one timing from a
    /// different point in the pipeline — the standard way to get alternating judder
    /// on a device with headroom to spare.
    ///
    /// **This assertion is no longer what enforces that**, and the demotion is the
    /// point. It only ever described the *rungs*, so when Android moved from `phone`
    /// to `balanced` on 2026-08-18 the rule stopped applying to the only platform it
    /// was written for and this test went on passing, green, describing a preset
    /// nothing selected. `lib.rs::frame_limiter` now asks the window instead
    /// ([`Capabilities::vsync_paced`]), which cannot be lost by changing a rung.
    /// What is left here is the tier's *preference* for a machine that presents
    /// without a vsync — keep it consistent, but do not mistake it for the guard.
    #[test]
    fn the_phone_leaves_frame_pacing_to_vsync() {
        assert_eq!(Budget::phone().fps_cap, None);
        assert!(Budget::balanced().fps_cap.is_some());
        assert!(Budget::desktop().fps_cap.is_some());
    }

    /// No rung may permit a single asset larger than its own frame upload budget.
    ///
    /// The two are the same quantity seen from opposite ends, and nothing but this
    /// test connects them. `RenderAssetBytesPerFrame` always writes at least one
    /// *whole* asset per frame so that uploads can never stall — so a mesh above
    /// the budget is a frame above the budget, and the throttle that exists to
    /// prevent exactly that quietly does not apply. They drifted to 8 MB against
    /// 4 MB, which is how an Adreno 840 was handed a 6 MB asset on 2026-08-18 and
    /// lost its device with the budget switched on and apparently working.
    /// **Every rung a handset can reach keeps the burst guard**, not only the one
    /// `auto()` picks.
    ///
    /// The hole this closes was reachable from a settings screen: Settings ▸
    /// Graphics ▸ Detail writes a `QualityPreset`, `parse` turns it into the rung's
    /// own literal, and `Budget::balanced()` / `Budget::desktop()` carry no ceiling
    /// — so "Balanced" on a phone was `android_budget()` minus the one field that
    /// exists for phones. Two Adreno 840 reports five minutes apart on 2026-08-19
    /// caught it: `peak_asset_kb` 4,095 on `Auto`, 7,920 on an explicit rung.
    ///
    /// Written against [`handset_guarded`] rather than against `detect`, because
    /// `detect`'s Android arm is a `cfg!` this test's host does not take — the same
    /// reason [`android_budget`] had to stop being a literal inside `auto()`.
    #[test]
    fn a_handset_keeps_the_burst_guard_on_every_rung() {
        for (name, rung) in [
            ("phone", Budget::phone()),
            ("balanced", Budget::balanced()),
            ("desktop", Budget::desktop()),
        ] {
            let guarded = handset_guarded(rung);
            let ceiling = guarded
                .upload_bytes_per_frame
                .unwrap_or_else(|| panic!("{name}: a handset must never run without a ceiling"));
            assert!(
                ceiling <= PHONE_UPLOAD_BYTES_PER_FRAME,
                "{name}: {ceiling} B is above the guard the device needs"
            );
            // …and the total, which the ceiling cannot reach: 424 tiles at a 512 px
            // raster are 565 MB of texture however slowly they are handed over.
            assert!(
                guarded.tile_px <= HANDSET_TILE_PX,
                "{name}: a {} px tile raster on a handset",
                guarded.tile_px
            );
            assert!(
                guarded.radius_bias >= 1,
                "{name}: the full streaming radius is a memory budget, not a rung"
            );
            // 12 MB of AHN per map anchor and 22 MB held for it is a desktop's
            // trade, not a phone's — and unlike the two above it is somebody's
            // data allowance as well as their memory.
            assert!(
                guarded.terrain_grid <= HANDSET_TERRAIN_GRID,
                "{name}: a {}² terrain field on a handset",
                guarded.terrain_grid
            );
            // Everything else is a judgement about the picture and stays the
            // person's to make. "Full detail" on a phone still means full detail —
            // this is not the place to second-guess a rung, only to stop one from
            // asking the driver for memory no handset has.
            assert_eq!(
                guarded.tier, rung.tier,
                "{name}: the guard must not silently move the rung"
            );
            assert_eq!(guarded.render_scale, rung.render_scale, "{name}");
            assert_eq!(guarded.shadows, rung.shadows, "{name}");
            assert_eq!(guarded.bloom, rung.bloom, "{name}");
            assert_eq!(guarded.detail_layers, rung.detail_layers, "{name}");
        }
        // A rung that already asks for less keeps its own numbers, in both
        // directions: `min` on the raster, `max` on the bias.
        let phone = handset_guarded(Budget::phone());
        assert_eq!(phone.tile_px, Budget::phone().tile_px);
        assert_eq!(phone.radius_bias, Budget::phone().radius_bias);
        // A rung that already asks for less keeps its own number.
        assert_eq!(
            handset_guarded(Budget::phone()).upload_bytes_per_frame,
            Budget::phone().upload_bytes_per_frame
        );
    }

    #[test]
    fn no_rung_permits_an_asset_larger_than_its_own_frame() {
        for (name, budget) in [
            ("phone", Budget::phone()),
            ("balanced", Budget::balanced()),
            // The rung a handset actually resolves to, and the only one with both
            // a ceiling and a device behind it. It was absent here for as long as
            // it was an inline literal in a `cfg`-gated `auto()` — see
            // `android_budget`.
            ("android", android_budget()),
            // …and every rung as a *handset* would run it, which is the
            // configuration an explicit `QualityPreset` produces.
            ("handset-balanced", handset_guarded(Budget::balanced())),
            ("handset-desktop", handset_guarded(Budget::desktop())),
            ("desktop", Budget::desktop()),
        ] {
            let Some(per_frame) = budget.upload_bytes_per_frame else {
                continue;
            };
            assert!(
                crate::systems::lod22::max_cell_mesh_bytes_for(&budget) <= per_frame,
                "{name}: a cell may be larger than the whole frame's budget"
            );
        }
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

    /// The vocabulary of `CARTO_SHADOW_FILTER`, and the thing that makes a
    /// debugging switch safe to leave in: a word it does not know resolves to
    /// `None` so the rung's own answer stands. The alternative — falling back to
    /// one of the two — is a typo that silently changes the picture being
    /// measured, which is exactly the failure this switch exists to avoid.
    #[test]
    fn the_shadow_filter_switch_takes_two_words_and_refuses_the_rest() {
        for word in ["fast", "hard", "hardware", "2x2", " FAST "] {
            assert_eq!(
                parse_filter(word),
                Some(ShadowFilteringMethod::Hardware2x2),
                "{word}"
            );
        }
        for word in ["soft", "gaussian", "pcf", "Soft"] {
            assert_eq!(
                parse_filter(word),
                Some(ShadowFilteringMethod::Gaussian),
                "{word}"
            );
        }
        // `temporal` among them: it is a real bevy variant and it is deliberately
        // unreachable here, so naming it must not work by accident.
        for word in ["", "temporal", "off", "1", "smooth"] {
            assert_eq!(parse_filter(word), None, "{word} should name no filter");
        }
    }
}
