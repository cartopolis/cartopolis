//! The sky's materials: stars, moon, horizon haze, the cloud deck, rain and the
//! planet's cloud sphere. Everything that spawns and drives them is
//! `cartopolis::systems::sky`.

use std::sync::atomic::{AtomicBool, Ordering};

use bevy::pbr::{Material, MaterialPipeline, MaterialPipelineKey};
use bevy::prelude::*;
use bevy::render::mesh::MeshVertexBufferLayoutRef;
use bevy::render::render_resource::{
    AsBindGroup, RenderPipelineDescriptor, ShaderType, SpecializedMeshPipelineError,
};
use bevy::render::storage::ShaderBuffer;
use bevy::shader::ShaderRef;

/// Material for the catalog star field (see `shaders/starfield.wgsl`): additive
/// gaussian points, with per-star tint and twinkle seed in the vertex colors.
/// `params.x` = overall brightness (night factor × fog veil), `.y` = twinkle clock.
#[derive(Asset, TypePath, AsBindGroup, Clone)]
pub struct StarFieldMaterial {
    #[uniform(0)]
    pub params: Vec4,
}

impl Material for StarFieldMaterial {
    fn fragment_shader() -> ShaderRef {
        "shaders/starfield.wgsl".into()
    }

    fn alpha_mode(&self) -> AlphaMode {
        AlphaMode::Add
    }

    fn specialize(
        _pipeline: &MaterialPipeline,
        descriptor: &mut RenderPipelineDescriptor,
        _layout: &MeshVertexBufferLayoutRef,
        _key: MaterialPipelineKey<Self>,
    ) -> Result<(), SpecializedMeshPipelineError> {
        use bevy::render::render_resource::{
            BlendComponent, BlendFactor, BlendOperation, BlendState,
        };
        // Quad winding depends on each star's tangent basis; draw both faces.
        descriptor.primitive.cull_mode = None;
        // Don't write depth — it must not occlude the atmosphere or anything else.
        if let Some(depth) = descriptor.depth_stencil.as_mut() {
            depth.depth_write_enabled = Some(false);
        }
        // Force pure additive blending (src + dst): black gaps add nothing, stars add light.
        if let Some(fragment) = descriptor.fragment.as_mut() {
            if let Some(Some(target)) = fragment.targets.get_mut(0) {
                target.blend = Some(BlendState {
                    color: BlendComponent {
                        src_factor: BlendFactor::One,
                        dst_factor: BlendFactor::One,
                        operation: BlendOperation::Add,
                    },
                    alpha: BlendComponent {
                        src_factor: BlendFactor::Zero,
                        dst_factor: BlendFactor::One,
                        operation: BlendOperation::Add,
                    },
                });
            }
        }
        Ok(())
    }
}

/// Material for the moon disc (see `shaders/moon.wgsl`): a sphere-shaded quad lit
/// by the sun direction expressed in the quad's local frame (xyz), so the lunar
/// phase and the terminator's orientation emerge from the real sun–moon geometry.
/// `sun_local.w` = overall brightness; alpha-blended so the disc covers stars.
#[derive(Asset, TypePath, AsBindGroup, Clone)]
pub struct MoonMaterial {
    #[uniform(0)]
    pub sun_local: Vec4,
}

impl Material for MoonMaterial {
    fn fragment_shader() -> ShaderRef {
        "shaders/moon.wgsl".into()
    }

    fn alpha_mode(&self) -> AlphaMode {
        AlphaMode::Blend
    }

    fn specialize(
        _pipeline: &MaterialPipeline,
        descriptor: &mut RenderPipelineDescriptor,
        _layout: &MeshVertexBufferLayoutRef,
        _key: MaterialPipelineKey<Self>,
    ) -> Result<(), SpecializedMeshPipelineError> {
        descriptor.primitive.cull_mode = None;
        // Don't write depth — it must not occlude the atmosphere or the stars' pass.
        if let Some(depth) = descriptor.depth_stencil.as_mut() {
            depth.depth_write_enabled = Some(false);
        }
        Ok(())
    }
}

/// Alpha-blended band of fog color around the horizon, bridging the fogged ground
/// and the atmosphere sky (whose single-scattering model darkens at grazing angles).
/// Colored from the camera's `DistanceFog` each frame (see `update_horizon_haze`).
#[derive(Asset, TypePath, AsBindGroup, Clone)]
pub struct HorizonHazeMaterial {
    #[uniform(0)]
    pub color: LinearRgba,
    /// x: elevation where the haze fades out fully, y: peak alpha,
    /// z: directional-light exponent (w unused).
    #[uniform(1)]
    pub params: Vec4,
    /// `DistanceFog::directional_light_color` — rgb tint, alpha = strength.
    #[uniform(2)]
    pub dir_light_color: LinearRgba,
    /// x: view-direction `y` where the shell's own rim sits (negative — the ray
    /// that grazes the shell exactly where the ground does). The haze fades to
    /// nothing there, so the rim itself never draws an edge. yzw unused.
    #[uniform(3)]
    pub rim: Vec4,
}

impl Material for HorizonHazeMaterial {
    fn fragment_shader() -> ShaderRef {
        "shaders/horizonhaze.wgsl".into()
    }

    fn alpha_mode(&self) -> AlphaMode {
        AlphaMode::Blend
    }

    fn specialize(
        _pipeline: &MaterialPipeline,
        descriptor: &mut RenderPipelineDescriptor,
        _layout: &MeshVertexBufferLayoutRef,
        _key: MaterialPipelineKey<Self>,
    ) -> Result<(), SpecializedMeshPipelineError> {
        // Camera is inside the shell; render its interior, and don't occlude
        // anything behind it (depth test still lets world geometry cover it).
        descriptor.primitive.cull_mode = None;
        if let Some(depth) = descriptor.depth_stencil.as_mut() {
            depth.depth_write_enabled = Some(false);
        }
        Ok(())
    }
}

/// The deck's shared buffer (`CloudField`, `cartopolis::systems::sky::CloudField`),
/// at a fixed id.
///
/// Fixed so that a material can name it in its `Default`. The ground's materials
/// are built with `..default()` in a dozen places, and a storage buffer that does
/// not exist is not an empty binding — the material never prepares, and a ground
/// material that never prepares is a hole in the map. `Handle::default()` names
/// no asset at all, so every material that binds the deck takes this instead.
pub const CLOUD_FIELD_BUFFER: Handle<ShaderBuffer> =
    bevy::asset::uuid_handle!("0aec065d-2710-4c8e-a7e1-02ad5c193299");

/// [`CLOUD_FIELD_BUFFER`]'s layout: [`CloudMaterial`]'s uniforms, in binding order,
/// under the names `cloud_field.wgsl`'s `Deck` gives them — `shape` is the
/// material's `params0`, `motion` its `params1`, `place` its `params2` and `light`
/// its `params3`. naga renames a struct member that ends in a digit, and naga_oil
/// refuses an imported struct whose names would not survive it. All `vec4` so it
/// has one layout on every driver (see `atmosphere_patch` for what a packed `vec3`
/// cost).
#[derive(ShaderType, Clone, Copy, Default, Debug, PartialEq)]
pub struct CloudFieldGpu {
    pub shape: Vec4,
    pub motion: Vec4,
    pub sun_dir: Vec4,
    pub sun_color: Vec4,
    pub sky_color: Vec4,
    pub place: Vec4,
    pub haze_color: Vec4,
    pub light: Vec4,
}

/// Whether the cloud deck is drawn with the phone's cheaper variant
/// ([`CloudMaterial`]'s `CLOUDS_LOW` shader def).
///
/// This was `quality::is_phone()`, read inside [`CloudMaterial::specialize`]; this
/// crate sits below the client and cannot see its quality rung, so the client
/// writes the answer here instead. **It must be written before any pipeline is
/// specialised**, or the first cloud pipeline would be compiled for the wrong
/// tier: `cartopolis::quality::install` sets it, and `install` runs from
/// `quality::init` at the very top of `run()` — before the `App` exists, so before
/// any render pipeline — and again on every preset swap, which is when
/// `is_phone()` used to change its answer too.
static LOW_CLOUDS: AtomicBool = AtomicBool::new(false);

/// Set whether the cloud deck takes the phone's cheaper variant. See [`LOW_CLOUDS`].
pub fn set_low_clouds(low: bool) {
    LOW_CLOUDS.store(low, Ordering::Release);
}

/// What [`set_low_clouds`] last said.
#[must_use]
pub fn low_clouds() -> bool {
    LOW_CLOUDS.load(Ordering::Acquire)
}

/// Camera-centred procedural cloud ceiling, alpha-blended over the sky. Coverage
/// is driven by the dev panel (`DevPanelState::cloud_cover`); the sun and sky
/// colours are the *same* day/night model the sunlight and the fog are built
/// from, so the deck can't drift off the sky's palette (see `update_clouds`).
#[derive(Asset, TypePath, AsBindGroup, Clone)]
pub struct CloudMaterial {
    /// x: coverage 0–1, y: softness, z: the sky's clock in seconds (drives the
    /// churn), w: low deck height (m).
    #[uniform(0)]
    pub params0: Vec4,
    /// xy: the wind's accumulated drift (m, `CloudField`,
    /// `cartopolis::systems::sky::CloudField`), z: space fade (1 =
    /// hidden), w: altitude fade.
    #[uniform(1)]
    pub params1: Vec4,
    /// xyz: world direction to the sun. w: deck thickness (m), for the sun march.
    #[uniform(2)]
    pub sun_dir: Vec4,
    /// Sunlight reaching the deck's altitude by `utils::sun`'s model — what a view
    /// without bevy's atmosphere uses; alpha carries the overall opacity.
    #[uniform(3)]
    pub sun_color: LinearRgba,
    /// Ambient sky light on the deck; alpha scales the high deck's opacity.
    #[uniform(4)]
    pub sky_color: LinearRgba,
    /// xy: world-anchor offset in metres (camera displacement from the anchor), so
    /// the deck stays fixed over the ground. z: the camera fog's per-metre density
    /// (aerial perspective). w: high deck height (m).
    #[uniform(5)]
    pub params2: Vec4,
    /// The colour the deck recedes into with distance — the camera's fog colour,
    /// so cloud and sky reach the horizon together.
    #[uniform(6)]
    pub haze_color: LinearRgba,
    /// x: the sunlight's scale apart from its colour (the sun disc's occlusion ×
    /// `CLOUD_SUN_GAIN`, `cartopolis::systems::sky::CLOUD_SUN_GAIN`). With bevy's
    /// atmosphere running the shader takes the
    /// *colour* from the atmosphere's own transmittance table instead of
    /// `sun_color`, so sunset clouds match the sky around them. y: 1 on the phone
    /// tier (`CLOUDS_LOW`), for the water that mirrors the deck — the shell
    /// itself reads its shader def. zw: unused.
    #[uniform(7)]
    pub params3: Vec4,
}

impl CloudMaterial {
    /// The eight vectors as the shared buffer lays them out ([`CloudFieldGpu`]).
    pub fn gpu(&self) -> CloudFieldGpu {
        CloudFieldGpu {
            shape: self.params0,
            motion: self.params1,
            sun_dir: self.sun_dir,
            sun_color: self.sun_color.to_vec4(),
            sky_color: self.sky_color.to_vec4(),
            place: self.params2,
            haze_color: self.haze_color.to_vec4(),
            light: self.params3,
        }
    }
}

impl Material for CloudMaterial {
    fn fragment_shader() -> ShaderRef {
        "shaders/clouds.wgsl".into()
    }

    fn alpha_mode(&self) -> AlphaMode {
        AlphaMode::Blend
    }

    fn specialize(
        _pipeline: &MaterialPipeline,
        descriptor: &mut RenderPipelineDescriptor,
        _layout: &MeshVertexBufferLayoutRef,
        _key: MaterialPipelineKey<Self>,
    ) -> Result<(), SpecializedMeshPipelineError> {
        // Camera is inside the shell; draw its interior and don't occlude the
        // world behind it (depth test still lets geometry cover the clouds).
        descriptor.primitive.cull_mode = None;
        if let Some(depth) = descriptor.depth_stencil.as_mut() {
            depth.depth_write_enabled = Some(false);
        }
        // The phone's cheaper cloud variant. A shader def rather than a uniform
        // branch, so the desktop program is byte-identical to what it was and the
        // handset's is genuinely shorter — a dynamic branch would still cost the
        // register pressure of the octaves it skips.
        //
        // Resolved from the tier here rather than passed through the material,
        // because it cannot change while the process runs and one extra pipeline
        // variant is exactly what specialization is for. The tier itself is the
        // client's; it reaches this crate through `set_low_clouds`.
        if low_clouds() {
            if let Some(fragment) = descriptor.fragment.as_mut() {
                fragment.shader_defs.push("CLOUDS_LOW".into());
            }
        }
        Ok(())
    }
}

/// Screen-space rain: an animated streak overlay on a small camera-centred shell
/// (`update_rain` keeps it on the camera). The shell's interior sits ~5 m out —
/// nearer than any world geometry — so with depth-write off the streaks draw over
/// the whole scene; the egui HUD still draws in its own later pass. The pattern is
/// keyed off the framebuffer coordinate, so the mesh shape only has to cover view.
#[derive(Asset, TypePath, AsBindGroup, Clone)]
pub struct RainMaterial {
    /// x: intensity 0–1, y: time (fall clock), z: wind slant, w unused.
    #[uniform(0)]
    pub params: Vec4,
    /// Streak colour; alpha carries the overall opacity.
    #[uniform(1)]
    pub tint: LinearRgba,
}

impl Material for RainMaterial {
    fn fragment_shader() -> ShaderRef {
        "shaders/rain.wgsl".into()
    }

    fn alpha_mode(&self) -> AlphaMode {
        AlphaMode::Blend
    }

    fn specialize(
        _pipeline: &MaterialPipeline,
        descriptor: &mut RenderPipelineDescriptor,
        _layout: &MeshVertexBufferLayoutRef,
        _key: MaterialPipelineKey<Self>,
    ) -> Result<(), SpecializedMeshPipelineError> {
        descriptor.primitive.cull_mode = None;
        if let Some(depth) = descriptor.depth_stencil.as_mut() {
            depth.depth_write_enabled = Some(false);
        }
        Ok(())
    }
}

/// Planet-scale cloud layer for the globe / space tier: a sphere just above the
/// surface, sampled by a 3D noise field and shaded by the sun terminator. Fades
/// in with `space_factor` (absent on the ground, where the ceiling shell rules).
#[derive(Asset, TypePath, AsBindGroup, Clone)]
pub struct GlobeCloudMaterial {
    /// xyz: sphere centre in render space, w: radius (m).
    #[uniform(0)]
    pub centre: Vec4,
    /// World direction to the sun (local anchor frame).
    #[uniform(1)]
    pub sun_dir: Vec4,
    /// x: coverage, y: opacity (space fade), z: time, w: noise frequency.
    #[uniform(2)]
    pub params: Vec4,
    /// Sunlit cloud colour; alpha carries the overall opacity.
    #[uniform(3)]
    pub lit_color: LinearRgba,
    /// Night-side cloud colour.
    #[uniform(4)]
    pub shadow_color: LinearRgba,
    /// Earth's spin axis in the local frame (latitude banding + zonal drift).
    #[uniform(5)]
    pub north_axis: Vec4,
    /// xyz: local-frame direction to geographic (0°,0°), used to recover longitude
    /// for the data lookup. w: data weight (0 = procedural fallback, 1 = live
    /// GFS/TCDC coverage). See `systems::globe_weather` and ADR-032.
    #[uniform(6)]
    pub frame_ref: Vec4,
    /// Live Total Cloud Cover grid (equirectangular, r = cloud fraction). A
    /// mid-grey placeholder until the first GFS fetch lands (weight stays 0 so it's
    /// unused meanwhile).
    #[texture(7)]
    #[sampler(8)]
    pub cloud_field: Handle<Image>,
}

impl Material for GlobeCloudMaterial {
    fn fragment_shader() -> ShaderRef {
        "shaders/globe_clouds.wgsl".into()
    }

    fn alpha_mode(&self) -> AlphaMode {
        AlphaMode::Blend
    }

    fn specialize(
        _pipeline: &MaterialPipeline,
        descriptor: &mut RenderPipelineDescriptor,
        _layout: &MeshVertexBufferLayoutRef,
        _key: MaterialPipelineKey<Self>,
    ) -> Result<(), SpecializedMeshPipelineError> {
        use bevy::render::render_resource::CompareFunction;
        // Seen from outside in orbit: keep back-face culling (draw the outer
        // shell). Don't write depth, and force the depth test to Always — from
        // 10,000 km the ~25 km shell is within the globe's depth precision, so a
        // normal test z-fights and the clouds show only as scattered dots; Always
        // makes the near hemisphere's clouds draw cleanly over the surface.
        if let Some(depth) = descriptor.depth_stencil.as_mut() {
            depth.depth_write_enabled = Some(false);
            depth.depth_compare = Some(CompareFunction::Always);
        }
        Ok(())
    }
}
