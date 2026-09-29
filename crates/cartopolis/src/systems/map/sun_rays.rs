//! Sunbeams: light fanning out from the sun through the gaps in the clouds and
//! past the edges of buildings, drawn as one pass over the finished frame
//! (`assets/shaders/sun_rays.wgsl`, Settings ▸ Graphics ▸ Sun rays).
//!
//! # Why a screen-space pass, when the light shafts already exist
//!
//! The shafts (`systems::light_shafts`) are bevy's volumetric fog: the air around
//! the viewer, cut by the sun's **shadow map**. The cloud deck is a shell drawn
//! over the sky and casts no shadow, so the shafts can never draw the one thing
//! people mean by sunbeams — the sun low behind broken cloud — and around the
//! viewer they are deliberately faint. Measured on the Deck (2026-09-29, Groningen,
//! 15:48 local, shafts on against off): a smooth warm wash over the sky and no
//! beam anywhere.
//!
//! Three ways of finding the beams *inside* the cloud shader were built first and
//! all read as nothing: averaging the deck along the arc to the sun, darkening the
//! shadowed columns, and marching each view ray up to the deck and looking toward
//! the sun from every step. The last one is physically right and is the reason
//! it fails: this deck's clouds are kilometres across, so the lit fraction of any
//! column changes over tens of degrees of sky and draws a gradient, not a beam.
//! Beams as the eye expects them are the screen-space answer every game gives —
//! take what is bright in the frame near the sun, blur it along the lines toward
//! the sun, and add it back — and that is this module.
//!
//! # What makes it a beam and not a glow
//!
//! The mask is the frame itself: the sun, and the sky showing through a gap, are
//! the brightest things near the sun; a cloud's shadowed base and a building's
//! silhouette are darker. Blurred along the radial line, the bright gaps become
//! streaks and the dark occluders cut them. No depth buffer is read — this app has
//! no prepass, on purpose (`render_features`' module note) — and none is needed,
//! because an occluder only has to be darker than the sky behind it.
//!
//! # Placement
//!
//! A [`FullscreenMaterial`] on the camera: before tonemapping, so the beams go
//! through the same exposure curve and bloom as the sky they are made of. The
//! component is inserted and removed by `render_features::apply_render_features`,
//! the one place the Graphics settings reach the camera, and [`update_sun_rays`]
//! writes where the sun is on screen every frame.

use bevy::core_pipeline::fullscreen_material::FullscreenMaterial;
use bevy::prelude::*;
use bevy::render::extract_component::ExtractComponent;
use bevy::render::render_resource::ShaderType;
use bevy::shader::ShaderRef;

use crate::components::FreeCamera;
use crate::systems::sky::SunLight;

/// Overall strength of the beams. The one art knob; the shader's constants set
/// their shape.
const STRENGTH: f32 = 1.1;

/// How far off screen the sun may be, in normalised device units (the screen is
/// ±1), before the beams have faded out. Beams from a sun just past the edge are
/// right — the sky beside it is still the brightest thing in the frame — and a
/// hard cut at the edge would pop as the camera turns.
const OFFSCREEN_FADE: f32 = 1.8;

/// The pass's uniform, and the marker that turns it on for a camera.
///
/// Laid out for WGSL as `SunRays` in `sun_rays.wgsl`: keep the two in step.
#[derive(Component, Default, Clone, Copy, ExtractComponent, ShaderType)]
pub struct SunRays {
    /// Where the sun is in the frame, 0..1 with y down (it may lie outside).
    pub sun_uv: Vec2,
    /// 0 skips the pass's work entirely: sun behind the camera, under the
    /// horizon, or too far off screen.
    pub strength: f32,
    /// Width over height, so the shader's falloff around the sun is round.
    pub aspect: f32,
    /// The sunlight's colour, brightest channel 1.
    pub tint: Vec4,
}

impl FullscreenMaterial for SunRays {
    fn fragment_shader() -> ShaderRef {
        "shaders/sun_rays.wgsl".into()
    }
}

/// Write where the sun is on screen, and whether it can cast beams at all.
///
/// Returns at once when the camera has no [`SunRays`], which is every run with
/// the setting off.
pub fn update_sun_rays(
    mut camera: Query<(&Camera, &GlobalTransform, &mut SunRays), With<FreeCamera>>,
    sun: Query<(&GlobalTransform, &DirectionalLight), With<SunLight>>,
) {
    let Ok((camera, cam_tf, mut rays)) = camera.single_mut() else {
        return;
    };
    let Ok((sun_tf, light)) = sun.single() else {
        rays.strength = 0.0;
        return;
    };
    // The light shines along its forward axis (`sky::spawn_sun_light` looks it
    // along −sun), so the sun itself is behind it.
    let to_sun = sun_tf.back().as_vec3();
    let facing = cam_tf.forward().as_vec3().dot(to_sun);
    let ndc = camera.world_to_ndc(cam_tf, cam_tf.translation() + to_sun * 1_000.0);
    let (Some(ndc), true) = (ndc, facing > 0.0) else {
        rays.strength = 0.0;
        return;
    };
    let offscreen = ndc.x.abs().max(ndc.y.abs());
    let on_screen = 1.0 - smoothstep(1.0, OFFSCREEN_FADE, offscreen);
    // Beams need the sun up: faded in over its first few degrees, so a sunset
    // does not switch them off in one frame.
    let risen = smoothstep(-0.01, 0.06, to_sun.y);

    let colour = light.color.to_linear().to_vec3();
    let tint = colour / colour.max_element().max(1e-4);
    let aspect = camera
        .logical_viewport_size()
        .map_or(1.0, |size| size.x / size.y.max(1.0));

    let next = SunRays {
        sun_uv: Vec2::new(ndc.x * 0.5 + 0.5, 0.5 - ndc.y * 0.5),
        strength: STRENGTH * on_screen * risen,
        aspect,
        tint: tint.extend(1.0),
    };
    // Through the change-detection guard: the component is extracted every frame
    // regardless, but a standing camera should not mark it changed.
    if rays.sun_uv != next.sun_uv
        || rays.strength != next.strength
        || rays.aspect != next.aspect
        || rays.tint != next.tint
    {
        *rays = next;
    }
}

fn smoothstep(edge0: f32, edge1: f32, x: f32) -> f32 {
    let t = ((x - edge0) / (edge1 - edge0)).clamp(0.0, 1.0);
    t * t * (3.0 - 2.0 * t)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_fades_run_the_right_way() {
        assert_eq!(smoothstep(1.0, OFFSCREEN_FADE, 0.5), 0.0);
        assert_eq!(smoothstep(1.0, OFFSCREEN_FADE, 2.0), 1.0);
        assert!(smoothstep(-0.01, 0.06, 0.02) > 0.0);
        assert!(smoothstep(-0.01, 0.06, 0.02) < 1.0);
    }
}
