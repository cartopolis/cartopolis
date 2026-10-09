//! The sunbeams' full-screen pass. Why it is a screen-space pass, and the system
//! that writes where the sun is, are `cartopolis::systems::sun_rays`.

use bevy::core_pipeline::fullscreen_material::FullscreenMaterial;
use bevy::prelude::*;
use bevy::render::extract_component::ExtractComponent;
use bevy::render::render_resource::ShaderType;
use bevy::render::sync_component::SyncComponent;
use bevy::render::RenderApp;
use bevy::shader::ShaderRef;

/// The pass's uniform, and the marker that turns it on for a camera.
///
/// Laid out for WGSL as `SunRays` in `sun_rays.wesl`: keep the two in step.
#[derive(Component, Default, Clone, Copy, ShaderType)]
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

impl SyncComponent<RenderApp> for SunRays {
    type Target = Self;
}

/// Extracted only while there are beams to draw. The pass is a full read and write of
/// the frame whatever `strength` says, and `strength` is 0 at night, with the sun
/// behind the camera, or with it far off screen — most frames. With nothing in the
/// render world, bevy drops the pass for that view.
impl ExtractComponent<RenderApp> for SunRays {
    type QueryData = &'static Self;
    type QueryFilter = ();
    type Out = Self;

    fn extract_component(rays: &Self) -> Option<Self> {
        (rays.strength > 0.0).then_some(*rays)
    }
}

impl FullscreenMaterial for SunRays {
    fn fragment_shader() -> ShaderRef {
        "shaders/sun_rays.wesl".into()
    }
}
