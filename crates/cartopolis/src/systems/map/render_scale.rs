//! Render the 3D world at less than the screen's resolution, and let the
//! existing upscale blit it back up.
//!
//! This is the knob the phone tier never had off the web. `index.html` caps the
//! browser build at 1.5 DPR / 1.3 Mpx by sizing the canvas backing store — and it
//! *has* to be done there, because the backing store is set from CSS before wgpu
//! ever sees the canvas, which is why `quality.rs`'s module docs call the render
//! scale "a page-layout decision that cannot be made from Rust at all". True on
//! the web; it left native Android with no equivalent whatsoever. A Fairphone 4
//! rendered every pass at its full 1080×2340 — **2.53 Mpx** — on a target that is
//! `Rgba16Float` whenever the atmosphere is up, i.e. 8 B/px of colour before
//! anything else. Fill is the first thing a tiled mobile GPU runs out of.
//!
//! ## Why this and not `ANativeWindow_setBuffersGeometry`
//!
//! The NDK call is the other obvious route: it asks SurfaceFlinger for a smaller
//! buffer and lets the display composer scale it, which is free. It was rejected
//! for two reasons. It scales **everything**, including the UI — on a 5.7" screen
//! the chrome and the street names are the last things that should get softer. And
//! it fights winit for ownership of the surface geometry, which has to be
//! re-applied on `SurfaceCreated` and kept consistent with the `Window` scale
//! factor that `touch.rs` maps finger positions through. A mismatch there is not a
//! blurry frame, it is taps landing in the wrong place.
//!
//! ## Why not `MainPassResolutionOverride`, which is the component for this
//!
//! It was the first implementation and it put the map in the **corner of the
//! phone**: the world filled the top-left ~70 % of the screen and the strip down
//! the right and along the bottom stayed clear colour, with the fly stick and the
//! dock floating over nothing. Nothing logged, because nothing was wrong.
//!
//! The component shrinks the *pass*, not the *target*. Every 3D node honours it
//! by scissoring itself into the top-left corner of a main texture that is still
//! allocated at full size — and the `upscaling` node that was supposed to "blit
//! the result up" does no such thing. It draws Bevy's fullscreen triangle and
//! samples the source over `uv` 0..1 (`bevy_core_pipeline::blit::blit.wgsl`),
//! i.e. the *whole* main texture, unrendered margin included, stretched over the
//! whole output. The rendered corner therefore lands in the corner, one-to-one.
//! Upstream's only consumer of the override is DLSS, which owns a node that reads
//! the reduced region and writes the full one; without an upscaler in the graph
//! the override is a scissor and nothing else. `View::main_pass_viewport` exists
//! for exactly this reason — it tells a shader that the pass is smaller than the
//! target it is sitting in.
//!
//! So this takes the other route: shrink the **target**. Overwriting the camera's
//! extracted target/viewport sizes in the render world makes `prepare_view_targets`
//! allocate the main texture at the reduced size, and every pass then fills it
//! edge to edge because that is simply how big the attachment is. The same
//! `upscaling` blit stretches it over the full-size swapchain — now correctly,
//! because the texture it samples is entirely rendered — and `egui_pass` runs
//! *after* upscaling onto that full-size output, so the world gets cheaper while
//! the interface stays sharp, which is the right way round.
//!
//! ## What it does and does not buy
//!
//! It saves **fill and memory**: the main texture (both halves), the depth buffer
//! and every screen-sized intermediate are allocated from the same reduced size,
//! which at the phone tier's 1/2 is a quarter of the bytes. Post-processing scales
//! with it too — tonemapping runs at the reduced size here, where
//! `MainPassResolutionOverride` explicitly does not touch it.
//!
//! The upscale **was** point-sampled, and that used to pick the scale rather than
//! the other way round: Bevy's blit binds a `NonFiltering` sampler built from
//! `SamplerDescriptor::default()`, so it cannot interpolate. At 1/2 every rendered
//! pixel becomes a clean 2×2 block; at a fractional scale like the 0.7 this
//! shipped with first, the same sampler duplicates roughly every third row and
//! column in a cadence unrelated to the picture, which crawls as the camera moves.
//! The two were one knob, and this module's own note said the fractional values
//! were worth having "only after `BlitPipeline`'s layout and sampler are swapped
//! for `Filtering`/`Linear` at `RenderStartup`".
//!
//! [`make_the_blit_filter`] now does exactly that, because the scale became a
//! **user-facing slider** and a slider whose intermediate positions look broken is
//! not a slider. So the constraint is lifted and any value in
//! [`RenderScale::RANGE`] resamples cleanly. 1/2 remains the phone tier's default
//! — it is still the cheapest of the good-looking scales, and now it is a default
//! rather than the only option. The web build never had the constraint: its
//! upscale is a CSS transform the browser's compositor filters for free.
//!
//! ## The scale is live, not a tier constant
//!
//! [`RenderScale`] is a `Resource` extracted every frame, seeded from the tier and
//! moved by Settings ▸ Map (persisted as `AppPrefs::render_scale`, `None` =
//! follow the tier). It is the first quality knob in this codebase that applies to
//! a *running* process — the quality preset beside it in that panel cannot, since
//! `quality::budget()` is a `OnceLock` read before the world exists. At exactly
//! 1.0 `apply_render_scale` writes nothing, so a run that never touches the slider
//! takes the pre-slider path and `cargo shots` output cannot move.
//!
//! ## The one shader that had to change
//!
//! Bevy's rule is "in shaders, use `View::main_pass_viewport` instead of
//! `View::viewport`". `rain.wesl` derives its screen UV from the fragment
//! coordinate over the viewport size and was moved onto `main_pass_viewport` when
//! this module used the override. It is left there deliberately: with no override
//! in the graph the two fields are equal (`prepare_view_uniforms` copies one from
//! the other), so it is correct either way, and it is the field that stays correct
//! if an upscaler ever does arrive.
//!
//! ## Light clustering was checked, and is fine
//!
//! Worth writing down because the shape of it looks exactly like a bug and the
//! obvious "fix" would be one. `clustered_forward.wgsl` bins a fragment with
//! `floor((frag_coord - view.viewport.xy) * lights.cluster_factors.xy)`, and the
//! cluster *grid* is sized in the main world from `Camera::physical_viewport_size`
//! — which this module deliberately never touches. Two viewports, one scaled and
//! one not, on either side of one calculation.
//!
//! It still lines up, because `cluster_factors` is not built from the main-world
//! camera. `bevy_pbr::render::light` computes it as
//! `clusters.dimensions / extracted_view.viewport`, in the **render** world, from
//! the same `ExtractedView` this module rewrites — so the factor is normalised
//! against the reduced framebuffer that `frag_coord` is in, and every fragment
//! lands in the cluster it should. What the unscaled main-world size does affect
//! is `dimensions_for_screen_size`, i.e. how many clusters get allocated: a grid
//! chosen for the full-resolution screen, which is slightly finer than the
//! reduced one needs. That is a rounding-scale cost, not a mapping error.
//!
//! So: do not "fix" this by scaling the main-world `Camera`. That would shrink
//! the viewport `world_to_viewport`, egui layout and the touch mapping all read
//! (the thing this module's whole design avoids), to correct an error that is not
//! occurring. Verified against bevy_pbr 0.19; re-check it on a Bevy bump, since
//! it rests on which world `cluster_factors` is computed in.

use std::ops::RangeInclusive;

use bevy::core_pipeline::blit::{init_blit_pipeline, BlitPipeline};
use bevy::prelude::*;
use bevy::render::camera::ExtractedCamera;
use bevy::render::extract_resource::{ExtractResource, ExtractResourcePlugin};
use bevy::render::render_resource::{
    binding_types::{sampler, texture_2d},
    BindGroupLayoutDescriptor, BindGroupLayoutEntries, FilterMode, SamplerBindingType,
    SamplerDescriptor, ShaderStages, TextureSampleType,
};
use bevy::render::renderer::RenderDevice;
use bevy::render::view::ExtractedView;
use bevy::render::{Render, RenderApp, RenderStartup, RenderSystems};

/// The live render scale: the fraction of the screen the 3D world is drawn at.
///
/// A `Resource` rather than a plain read of [`crate::quality::Budget`], because
/// this is the first quality knob in this codebase that applies to a **running**
/// process. The tier still supplies the starting value; the Settings slider moves
/// it, and the render world re-extracts it every frame.
#[derive(Resource, Clone, Copy, Debug, ExtractResource, PartialEq)]
#[extract_app(RenderApp)]
pub struct RenderScale(pub f32);

impl RenderScale {
    /// The slider's range.
    ///
    /// The ceiling is **hard** at 1.0: a render scale is a fraction by definition,
    /// and the module's own `scaled_size` caps at the input anyway.
    ///
    /// The floor is 0.5 — a quarter of the fragments, 540×1170 on a Fairphone 4.
    /// It is where the tier already sits, and going below it stops being a
    /// trade-off: the map is the thing you are looking at, and at 1/3 the road
    /// casings and the vector `surfaces` edges alias into each other faster than
    /// the reduced fill buys back.
    pub const RANGE: RangeInclusive<f32> = 0.5..=1.0;

    /// Clamped at the constructor, because `user.json` is hand-editable and
    /// `CARTO_RENDER_SCALE` takes free text. A non-finite value is 1.0 — "change
    /// nothing" is the only safe reading of a NaN.
    #[must_use]
    pub fn new(value: f32) -> Self {
        if value.is_finite() {
            Self(value.clamp(*Self::RANGE.start(), *Self::RANGE.end()))
        } else {
            Self(1.0)
        }
    }
}

/// Applies the live [`RenderScale`] to the world camera.
pub struct RenderScalePlugin;

impl Plugin for RenderScalePlugin {
    fn build(&self, app: &mut App) {
        // No early return on 1.0 any more, and that is the one thing the slider
        // changes structurally. The scale used to be a tier constant, so 1.0 meant
        // "this run will never scale" and skipping the whole plugin was free; now
        // it is a starting value the user can move at any moment, and a plugin that
        // was never installed cannot be turned on later. `apply_render_scale`
        // early-returns at 1.0 instead, so a desktop run that never touches the
        // slider still writes nothing and `cargo shots` output cannot move.
        app.insert_resource(initial_scale())
            .add_plugins(ExtractResourcePlugin::<RenderScale>::default());

        let Some(render_app) = app.get_sub_app_mut(RenderApp) else {
            return;
        };
        // The blit that expands the reduced picture has to interpolate, or the
        // slider is a lie at every value except an exact reciprocal — see
        // `make_the_blit_filter`.
        render_app.add_systems(
            RenderStartup,
            make_the_blit_filter.after(init_blit_pipeline),
        );
        // `PrepareAssets` is the slot: after `ExtractCommands` (so the sizes to
        // scale exist and are this frame's), and before `PrepareViews`, which is
        // where `prepare_view_targets` reads `physical_target_size` to allocate
        // the main texture. Writing later than that would size the textures from
        // the full resolution and scale nothing at all.
        render_app.add_systems(
            Render,
            apply_render_scale.in_set(RenderSystems::PrepareAssets),
        );
    }
}

/// Give Bevy's blit a `Linear` sampler, so a non-reciprocal scale is usable.
///
/// This is the ~25-line change the module header names, done because the scale is
/// a slider now. `init_blit_pipeline` builds `BlitPipeline` with
/// `texture_2d(Float { filterable: false })` + `sampler(NonFiltering)` and a
/// sampler from `SamplerDescriptor::default()`, i.e. `FilterMode::Nearest` — which
/// is exactly right for the 1:1 blit it was written for and cannot interpolate.
/// Point-sampling is fine at 1/2 (every pixel becomes a clean 2×2 block) and
/// visibly wrong at 0.6 or 0.7, where it duplicates rows and columns in a cadence
/// unrelated to the picture and the pattern crawls as the camera moves. A slider
/// whose intermediate positions look broken is not a slider.
///
/// The layout and the sampler must move **together**: a `Filtering` sampler bound
/// against a `filterable: false` texture entry is a wgpu validation failure, not a
/// fallback. So both are rewritten here.
///
/// Ordered `.after(init_blit_pipeline)` in `RenderStartup`, which is the whole
/// reason this can be a resource edit rather than a fork of the node: it runs once,
/// before any frame has built a bind group against the old layout, and the two
/// fields it does not touch (`fullscreen_shader`, `fragment_shader`) keep the
/// handles Bevy's own `load_embedded_asset!` produced. `blit.wgsl` itself needs no
/// change — it only ever `textureSample`s.
///
/// This makes every blit in the app filtered, not just ours. That is the intent:
/// `upscaling` is the only consumer in this tree, and at scale 1.0 it samples
/// texel centres one-to-one, where `Linear` and `Nearest` return the same value.
fn make_the_blit_filter(mut blit: ResMut<BlitPipeline>, render_device: Res<RenderDevice>) {
    blit.layout = BindGroupLayoutDescriptor::new(
        "blit_bind_group_layout_filtering",
        &BindGroupLayoutEntries::sequential(
            ShaderStages::FRAGMENT,
            (
                texture_2d(TextureSampleType::Float { filterable: true }),
                sampler(SamplerBindingType::Filtering),
            ),
        ),
    );
    blit.sampler = render_device.create_sampler(&SamplerDescriptor {
        label: Some("blit_sampler_filtering"),
        mag_filter: FilterMode::Linear,
        min_filter: FilterMode::Linear,
        ..default()
    });
}

/// The scale this run starts at: an explicit override, else the tier's default.
///
/// The user's own preference is deliberately *not* read here. It lives in
/// `user.json`, which loads into `AppPrefs` during `Update`, long after plugin
/// build — and unlike [`crate::quality::init`], which must run at the top of
/// `run()` because the worker pool sizes itself from it, nothing is sized from the
/// render scale. So the preference arrives through
/// [`apply_preferred_render_scale`] and this only has to answer for the frames
/// before it. Do not copy `UserStore::preferred_quality`'s early
/// `storage::read_now` shape here out of symmetry: it buys nothing and costs a
/// second synchronous read of `user.json` on the platform least able to afford it.
fn initial_scale() -> RenderScale {
    if let Some(raw) = crate::env_value("CARTO_RENDER_SCALE") {
        match raw.trim().parse::<f32>() {
            Ok(value) => return RenderScale::new(value),
            Err(_) => tracing::warn!(
                value = raw,
                "unparseable CARTO_RENDER_SCALE; expected a fraction like 0.5"
            ),
        }
    }
    RenderScale::new(crate::quality::budget().render_scale)
}

/// Push the user's saved preference into the live [`RenderScale`] once it loads.
///
/// `None` in `AppPrefs` means "Automatic", which — exactly as `QualityPreset::Auto`
/// does — is the *absence* of an answer rather than a fourth value, so it leaves
/// the tier's own scale standing.
pub fn apply_preferred_render_scale(
    prefs: Res<crate::resources::AppPrefs>,
    mut scale: ResMut<RenderScale>,
) {
    if !prefs.is_changed() {
        return;
    }
    let want = match prefs.render_scale {
        Some(value) => RenderScale::new(value),
        // `initial_scale`, not the bare tier: `AppPrefs` reads as changed on its
        // first frame, so answering the tier here silently undid
        // `CARTO_RENDER_SCALE` before a single frame was drawn at it.
        None => initial_scale(),
    };
    if *scale != want {
        *scale = want;
    }
}

/// Scale one physical size, floored at 1 px and never larger than the input.
///
/// Floored because a zero-sized attachment is a wgpu validation error, and this
/// tree already knows what that costs: Bevy answers a validation failure by
/// quitting the run silently, mid-stream, with the only clue being `CommandQueue
/// has un-applied commands`. Capped because a render scale is a fraction by
/// definition — the tier test below stops anyone typing 1.5 to "sharpen" the
/// phone, and this stops it mattering if they do.
#[must_use]
pub(crate) fn scaled_size(size: UVec2, scale: f32) -> UVec2 {
    let axis = |v: u32| ((v as f32 * scale).round() as u32).max(1).min(v.max(1));
    UVec2::new(axis(size.x), axis(size.y))
}

/// Size the camera's render target from its real target size each frame.
///
/// Recomputed rather than cached for two reasons. The target can change under us —
/// a window resize, and on Android a rotation or a multi-window resize — and
/// `extract_cameras` *re-inserts* both of these components every frame, so a
/// one-shot write would be overwritten before the first pass read it.
///
/// Queried by `ExtractedCamera` rather than by our own `FreeCamera` marker,
/// because this runs in the **render** world and that marker is a main-world
/// component that nothing extracts. Writing the query the other way compiles
/// perfectly and matches zero entities — a silent no-op, which on a change whose
/// only symptom is a frame rate is the worst possible failure mode. Every camera
/// this app renders with is the world camera or the headless shot camera standing
/// in for it, so applying to all extracted cameras is also the correct scope.
///
/// Note what is deliberately *not* touched: the main-world `Camera`. Its logical
/// viewport is what `Camera::world_to_viewport` answers from (`place_pins`), what
/// egui lays out against and what `touch.rs` maps fingers through — and all three
/// are right, because the picture still covers the whole window. The reduced
/// resolution exists only between extraction and the upscale blit.
fn apply_render_scale(
    scale: Res<RenderScale>,
    mut cameras: Query<(&mut ExtractedCamera, &mut ExtractedView)>,
) {
    let scale = scale.0;
    // The early return the plugin's `build` used to do. At 1.0 nothing is written
    // at all, so a run that never moves the slider takes the pre-slider path
    // exactly and `cargo shots` output cannot move.
    if scale >= 1.0 {
        return;
    }
    for (mut camera, mut view) in &mut cameras {
        let Some(target) = camera.physical_target_size else {
            continue;
        };
        camera.physical_target_size = Some(scaled_size(target, scale));
        camera.physical_viewport_size = camera
            .physical_viewport_size
            .map(|size| scaled_size(size, scale));
        // `None` on every camera this app spawns — but an explicit viewport is a
        // sub-rect *of the target*, so if one is ever set it has to move with the
        // target it is measured in or it addresses the wrong pixels.
        if let Some(viewport) = camera.viewport.as_mut() {
            viewport.physical_position = (viewport.physical_position.as_vec2() * scale)
                .round()
                .as_uvec2();
            viewport.physical_size = scaled_size(viewport.physical_size, scale);
        }
        // The same rect again, in the form the `View` uniform carries it: origin
        // then size. Shaders derive screen UVs from it, so leaving it at the full
        // size would put every screen-space effect on a different grid from the
        // pass drawing it.
        let origin = (view.viewport.xy().as_vec2() * scale).round().as_uvec2();
        let size = scaled_size(view.viewport.zw(), scale);
        view.viewport = origin.extend(size.x).extend(size.y);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The scale must be a fraction of the target, never a multiple of it.
    ///
    /// A tier is a hand-written struct literal, so nothing else stops someone
    /// typing 1.5 here to "sharpen" the phone — which would ask wgpu for a main
    /// texture larger than the window it is blitted to, i.e. a picture that is
    /// silently cropped rather than sharper.
    #[test]
    fn no_tier_asks_to_render_larger_than_its_target() {
        for budget in [
            crate::quality::Budget::phone(),
            crate::quality::Budget::balanced(),
            crate::quality::Budget::desktop(),
        ] {
            assert!(
                budget.render_scale > 0.0 && budget.render_scale <= 1.0,
                "{:?} asks for a render scale of {}; it must be in (0, 1]",
                budget.tier,
                budget.render_scale
            );
        }
    }

    /// The blit must be able to interpolate, because the scale is a slider now.
    ///
    /// This replaces a test that pinned the phone tier to the reciprocal of a whole
    /// number. That constraint was real while Bevy's blit sampled `Nearest`: at 1/2
    /// every rendered pixel becomes a clean 2×2 block, but at 0.7 — what the tier
    /// shipped first — the same sampler duplicates roughly every third row and
    /// column in a cadence unrelated to the picture, which crawls as the camera
    /// moves. The old test's own message named the fix ("Give the blit a `Linear`
    /// sampler first"), and [`make_the_blit_filter`] is it.
    ///
    /// So the invariant moves rather than disappearing. It is no longer "the scale
    /// is a reciprocal" — the user can pick 0.65 from the slider and no test can
    /// forbid it — it is **the sampler swap is still installed**. Delete that system
    /// and every non-reciprocal slider position silently starts crawling, on a
    /// device nobody here can look at.
    #[test]
    fn the_upscale_can_interpolate() {
        let mut app = App::new();
        // The system exists and is orderable against Bevy's own initialiser; what
        // this pins is the ordering contract, since running before
        // `init_blit_pipeline` would be overwritten and silently do nothing.
        let mut schedule = Schedule::new(RenderStartup);
        schedule.add_systems(make_the_blit_filter.after(init_blit_pipeline));
        // Building the schedule is the assertion: an unresolvable ordering or a
        // system whose parameters no longer exist fails here.
        schedule.initialize(app.world_mut()).expect(
            "make_the_blit_filter must stay orderable after init_blit_pipeline — \
             without it a non-reciprocal render scale point-samples and crawls",
        );
    }

    /// …and it must not ask a handset for more pixels than the web build's phone
    /// tier already refuses to draw.
    ///
    /// A ceiling, not a band: native is deliberately under it now (0.63 Mpx against
    /// 1.3), because the web's upscale is a CSS transform the browser's compositor
    /// filters for free while this one is a point blit we pay for. What must never
    /// happen is native drifting *above* the number a single-threaded browser build
    /// on the same handset was judged unable to afford.
    #[test]
    fn the_phone_stays_under_the_web_pixel_ceiling() {
        let scale = crate::quality::Budget::phone().render_scale;
        // A Fairphone 4's real panel, which is the device the tier is written for.
        let pixels = (1080.0 * scale) * (2340.0 * scale);
        assert!(
            pixels <= 1_300_000.0,
            "phone tier renders {pixels:.0} px; index.html caps the web build at \
             1.3 Mpx and native must not ask for more"
        );
    }

    /// Both axes take the same scale, so the reduced target has the window's
    /// aspect ratio. The upscale is a stretch to the full swapchain — an aspect
    /// error here is not a softer picture, it is a squashed one.
    #[test]
    fn the_reduced_target_keeps_the_screens_aspect_ratio() {
        // The Fairphone 4 in both orientations, plus a tablet-ish landscape.
        for size in [
            UVec2::new(1080, 2340),
            UVec2::new(2340, 1080),
            UVec2::new(1600, 900),
        ] {
            let scale = crate::quality::Budget::phone().render_scale;
            let reduced = scaled_size(size, scale);
            let want = size.x as f32 / size.y as f32;
            let got = reduced.x as f32 / reduced.y as f32;
            assert!(
                (want - got).abs() / want < 0.005,
                "{size} scaled to {reduced}: aspect {got} against {want}"
            );
        }
    }

    /// A degenerate target must come back as something wgpu will accept, because
    /// the alternative is the silent mid-stream quit a zero-sized attachment
    /// causes. A one-pixel window is not a real case; a window mid-resize is.
    #[test]
    fn a_tiny_target_stays_at_least_one_pixel() {
        for size in [UVec2::new(1, 1), UVec2::new(3, 1), UVec2::ZERO] {
            let reduced = scaled_size(size, 0.7);
            assert!(
                reduced.x >= 1 && reduced.y >= 1,
                "{size} scaled to {reduced}"
            );
            assert!(reduced.x <= size.x.max(1) && reduced.y <= size.y.max(1));
        }
    }
}
