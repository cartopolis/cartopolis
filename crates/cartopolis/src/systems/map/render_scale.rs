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
//! [`MainPassResolutionOverride`] has neither problem. Every 3D pass node honours
//! it, the `upscaling` node already in the graph blits the result up, and
//! `egui_pass` runs *after* upscaling onto the full-size output — so the world
//! gets cheaper while the interface stays sharp, which is the right way round.
//!
//! ## What it does not do
//!
//! It saves **fill, not memory**: the render target is still allocated at full
//! size. And it does not touch post-processing, by Bevy's own definition of the
//! component — tonemapping and bloom still run at output resolution.
//!
//! ## The one shader that had to change
//!
//! Bevy's rule is "in shaders, use `View::main_pass_viewport` instead of
//! `View::viewport`". One shader in this tree read the latter: `rain.wgsl` derives
//! its screen UV from the fragment coordinate over the viewport size. At a reduced
//! main pass the fragment coordinate is in the *reduced* space while
//! `view.viewport` still reports the full target, so the rain would have been
//! squashed into a corner of the screen. Everything else here works in world space
//! and never asks how big the screen is.

use bevy::camera::MainPassResolutionOverride;
use bevy::prelude::*;
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
        // In `Render`, before the passes read it, rather than in `ExtractSchedule`:
        // the camera's `ExtractedCamera` (which carries the physical target size
        // this is a fraction *of*) is written by the extract step, so a system
        // there would be racing the value it needs. `PrepareAssets` is the first
        // set that runs after extraction has settled.
        render_app.add_systems(
            Render,
            apply_render_scale.in_set(RenderSystems::PrepareAssets),
        );
    }
}

/// Size the main pass from the camera's real target size each frame.
///
/// Recomputed rather than cached because the target can change under us — a window
/// resize, and on Android a rotation or a multi-window resize. Bevy requires the
/// override to be **no larger than the viewport**, and a violation is not a
/// clamped frame but a validation failure, so the clamp below is load-bearing
/// rather than defensive.
///
/// Queried by `ExtractedCamera` rather than by our own `FreeCamera` marker,
/// because this runs in the **render** world and that marker is a main-world
/// component that nothing extracts. Writing the query the other way compiles
/// perfectly and matches zero entities — a silent no-op, which on a change whose
/// only symptom is a frame rate is the worst possible failure mode. Every camera
/// this app renders with is the world camera or the headless shot camera standing
/// in for it, so applying to all extracted cameras is also the correct scope.
fn apply_render_scale(
    mut commands: Commands,
    cameras: Query<(Entity, &bevy::render::camera::ExtractedCamera)>,
) {
    let scale = crate::quality::budget().render_scale;
    for (render_entity, camera) in &cameras {
        let Some(target) = camera.physical_target_size else {
            continue;
        };
        // Floor to at least 1×1. A zero-sized main pass is a wgpu validation error,
        // and this tree already knows what that costs: Bevy answers a validation
        // failure by quitting the run silently, mid-stream, with the only clue
        // being `CommandQueue has un-applied commands`.
        let scaled = UVec2::new(
            ((target.x as f32 * scale) as u32).max(1).min(target.x),
            ((target.y as f32 * scale) as u32).max(1).min(target.y),
        );
        commands
            .entity(render_entity)
            .insert(MainPassResolutionOverride(scaled));
    }
}

#[cfg(test)]
mod tests {
    /// The scale must be a fraction of the target, never a multiple of it.
    ///
    /// Bevy's contract is that the override is *smaller* than the viewport; an
    /// override larger than the render target is a validation failure, and this
    /// tree's note on that failure mode is that Bevy answers it by quitting the run
    /// without a usable error. A tier is a hand-written struct literal, so nothing
    /// else stops someone typing 1.5 here to "sharpen" the phone.
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

    /// The phone's scale must actually land near the ceiling the web build already
    /// enforces, or the two platforms' "phone tier" mean different things.
    #[test]
    fn the_phone_scale_lands_near_the_web_pixel_ceiling() {
        let scale = crate::quality::Budget::phone().render_scale;
        // A Fairphone 4's real panel, which is the device the tier is written for.
        let pixels = (1080.0 * scale) * (2340.0 * scale);
        assert!(
            (1_100_000.0..=1_400_000.0).contains(&pixels),
            "phone tier renders {pixels:.0} px; index.html caps the web build at \
             1.3 Mpx and the two budgets should agree"
        );
    }
}
