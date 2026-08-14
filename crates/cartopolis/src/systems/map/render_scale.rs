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
//! The upscale is **point-sampled**, and that is what picks the scale rather than
//! the other way round: Bevy's blit binds a `NonFiltering` sampler built from
//! `SamplerDescriptor::default()`, so it cannot interpolate. At the tier's 1/2
//! every rendered pixel becomes a clean 2×2 block; at a fractional scale like the
//! 0.7 this shipped with first, the same sampler duplicates roughly every third
//! row and column in a cadence unrelated to the picture, which crawls as the
//! camera moves. So the two knobs are one knob — a non-integer scale is worth
//! having only after `BlitPipeline`'s layout and sampler are swapped for
//! `Filtering`/`Linear` at `RenderStartup`, which is a ~25-line change here and
//! the thing to reach for if the phone map ever reads as crunchy rather than soft.
//! The web build has no such constraint: its upscale is a CSS transform and the
//! browser's compositor filters it for free.
//!
//! ## The one shader that had to change
//!
//! Bevy's rule is "in shaders, use `View::main_pass_viewport` instead of
//! `View::viewport`". `rain.wgsl` derives its screen UV from the fragment
//! coordinate over the viewport size and was moved onto `main_pass_viewport` when
//! this module used the override. It is left there deliberately: with no override
//! in the graph the two fields are equal (`prepare_view_uniforms` copies one from
//! the other), so it is correct either way, and it is the field that stays correct
//! if an upscaler ever does arrive.

use bevy::prelude::*;
use bevy::render::camera::ExtractedCamera;
use bevy::render::view::ExtractedView;
use bevy::render::{Render, RenderApp, RenderSystems};

/// Applies [`crate::quality::Budget::render_scale`] to the world camera.
pub struct RenderScalePlugin;

impl Plugin for RenderScalePlugin {
    fn build(&self, app: &mut App) {
        // Exactly 1.0 means "insert nothing", so every tier above the phone keeps
        // a bit-identical render path and `cargo shots` output cannot move. The
        // plugin is not even added to the render app in that case.
        if (crate::quality::budget().render_scale - 1.0).abs() < f32::EPSILON {
            return;
        }
        let Some(render_app) = app.get_sub_app_mut(RenderApp) else {
            return;
        };
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

/// Scale one physical size, floored at 1 px and never larger than the input.
///
/// Floored because a zero-sized attachment is a wgpu validation error, and this
/// tree already knows what that costs: Bevy answers a validation failure by
/// quitting the run silently, mid-stream, with the only clue being `CommandQueue
/// has un-applied commands`. Capped because a render scale is a fraction by
/// definition — the tier test below stops anyone typing 1.5 to "sharpen" the
/// phone, and this stops it mattering if they do.
#[must_use]
fn scaled_size(size: UVec2, scale: f32) -> UVec2 {
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
fn apply_render_scale(mut cameras: Query<(&mut ExtractedCamera, &mut ExtractedView)>) {
    let scale = crate::quality::budget().render_scale;
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

    /// The phone's scale must be the reciprocal of a whole number, because the
    /// upscale that follows it is **point-sampled**.
    ///
    /// At 1/2 every rendered pixel becomes a clean 2×2 block. At 0.7 — which is
    /// what this tier shipped first — the same sampler duplicates roughly every
    /// third row and column, in a cadence that has no relation to anything in the
    /// picture and that crawls across it as the camera moves. So a fractional scale
    /// is a decision about `BlitPipeline`'s sampler before it is a decision about
    /// the budget, and the two have to move in one commit. This test is the note
    /// that says so.
    #[test]
    fn the_phone_scale_is_a_whole_pixel_multiple() {
        let scale = crate::quality::Budget::phone().render_scale;
        let step = 1.0 / scale;
        assert!(
            (step - step.round()).abs() < 1.0e-6,
            "phone tier renders at {scale}, i.e. one pixel every {step} — not a \
             whole multiple, which Bevy's non-filtering upscale blit cannot \
             resolve evenly. Give the blit a `Linear` sampler first."
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
