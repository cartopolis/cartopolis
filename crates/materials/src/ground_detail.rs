//! The ground's material. The slope tier it reads, and the systems that bake and
//! place it, are `cartopolis::systems::ground_detail`.

use bevy::mesh::MeshVertexBufferLayoutRef;
use bevy::pbr::{
    ExtendedMaterial, MaterialExtension, MaterialExtensionKey, MaterialExtensionPipeline,
};
use bevy::prelude::*;
use bevy::render::render_resource::{
    AsBindGroup, RenderPipelineDescriptor, ShaderType, SpecializedMeshPipelineError,
};
use bevy::render::storage::ShaderBuffer;
use bevy::shader::ShaderRef;

/// The ground material every flat surface and every relief tile carries.
pub type GroundMaterial = ExtendedMaterial<StandardMaterial, GroundDetailExtension>;

/// Which variant of `shaders/ground_detail.wesl` a ground material compiles.
///
/// Two flags: [`GroundKey::see_through`], the screen-door the underground layer
/// looks through (`systems::sewers`), and [`GroundKey::fade_in`], the one a
/// streamed cell arrives through. Each is a *variant* rather than a uniform
/// because the cost is in the shader's existence, not in its branch: a fragment
/// shader that can `discard` loses early depth testing for every draw it is used
/// in, whether or not it ever takes the branch, and the ground is several stacked
/// sheets deep. So the ordinary ground never has the statement in it at all.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub struct GroundKey {
    pub see_through: bool,
    pub fade_in: bool,
}

impl From<&GroundDetailExtension> for GroundKey {
    fn from(e: &GroundDetailExtension) -> Self {
        Self {
            see_through: e.see_through,
            fade_in: e.fade_in,
        }
    }
}

/// Add the shader defs a [`GroundKey`] asks for to a ground pipeline. Shared with
/// `road_depth`, whose fragment shader is this one.
pub fn specialize_ground(descriptor: &mut RenderPipelineDescriptor, key: GroundKey) {
    if let Some(fragment) = descriptor.fragment.as_mut() {
        if key.see_through {
            fragment.shader_defs.push("GROUND_SEE_THROUGH".into());
        }
        if key.fade_in {
            fragment.shader_defs.push("GROUND_FADE_IN".into());
        }
    }
}

/// The `lod` a fade-in copy carries: `x` the moment its fade began, in seconds of
/// `globals.time`, and `y` 1 once it has — before that every fragment is
/// discarded. `lod.w` stays 0, which is what keeps the clipmap's cross-fade off
/// on a surface that is not a tile. `shaders/ground_detail.wesl` reads it.
#[must_use]
pub fn fade_lod(began: Option<f32>) -> Vec4 {
    match began {
        Some(t) => Vec4::new(t, 1.0, 0.0, 0.0),
        None => Vec4::ZERO,
    }
}

/// How long a fade-in takes, in seconds. `shaders/ground_detail.wesl` has the
/// same number as `FADE_IN_S`.
pub const FADE_IN_S: f32 = 0.5;

/// The ground material's four vectors, as the one buffer the shader reads them
/// from (binding 100; in bindless mode an element of the array at 110).
///
/// **Bindless is what this shape is for.** Every clipmap tile is a material of
/// its own — it has its own basemap image — so before 2026-10 every tile was a
/// bind group of its own and a draw of its own, and the render thread's time on
/// the Steam Deck scales with draws (`docs/notes/render-performance-survey.md`).
/// With `#[bindless]` the tiles share one bind group and bevy batches them
/// together; four separate uniforms cannot be bindless (bevy supports buffers in
/// that mode only as one `data` struct per material), so they became one.
/// Where the platform cannot do bindless — the web, some Android drivers — the
/// same declaration falls back to an ordinary uniform at 100 and nothing else
/// changes. `RoadDepthExtension` declares the identical layout, because the two
/// share a fragment shader.
#[derive(Clone, Copy, Debug, Default, ShaderType)]
pub struct GroundUniform {
    /// `GroundDetailExtension::params`.
    pub detail: Vec4,
    /// `GroundDetailExtension::lod`.
    pub lod: Vec4,
    /// `GroundDetailExtension::water_body`.
    pub water_body: Vec4,
    /// `GroundDetailExtension::water_sea`.
    pub water_sea: Vec4,
}

impl From<&GroundDetailExtension> for GroundUniform {
    fn from(e: &GroundDetailExtension) -> Self {
        Self {
            detail: e.params,
            lod: e.lod,
            water_body: e.water_body,
            water_sea: e.water_sea,
        }
    }
}

/// The sub-metre half of the ground, for `shaders/ground_detail.wesl`.
#[derive(Asset, AsBindGroup, Reflect, Clone, Debug)]
#[bind_group_data(GroundKey)]
#[data(100, GroundUniform, binding_array(110))]
#[bindless(index_table(range(100..106), binding(109)))]
pub struct GroundDetailExtension {
    /// xy: the detail box's centre in **render-space** XZ. z: 1 / its half-extent.
    /// w: strength, and `0.0` means "no tier" — see the module docs
    /// (`cartopolis::systems::ground_detail`).
    pub params: Vec4,
    /// The tier's residual slope, `Rg8Snorm`. Shared by every ground material, so a
    /// re-bake rewrites one image rather than hundreds.
    #[texture(101)]
    #[sampler(102)]
    pub slope: Option<Handle<Image>>,
    /// The clipmap level's ring, for the level-to-level cross-fade: xy its centre in
    /// **render-space** XZ, z `1 /` its half-extent, w the mip bias to reach at its
    /// edge. `cartopolis::systems::map_stream::refresh_tile_lod_blend` writes it and
    /// says what it is for.
    ///
    /// `w = 0` is the off switch and is what every ground surface that is not a
    /// clipmap tile carries — the vector surfaces, the roads, the paving, the
    /// volumetric groups. The shader takes no second sample at all there.
    pub lod: Vec4,
    /// The tile's *own* basemap image, bound a second time so the shader can sample
    /// it at a mip bias.
    ///
    /// The same handle `base_color_texture` already holds, so this costs a binding
    /// and no memory. Bound here rather than reached for through
    /// `bevy_pbr::pbr_bindings`, whose base-colour binding is `#ifdef BINDLESS`-shaped
    /// and is bevy's to rearrange; this one is ours and is the same on every platform.
    #[texture(103)]
    #[sampler(104)]
    pub basemap: Option<Handle<Image>>,
    /// The water surface's deep body colour (rgb, linear) and its `reflectance`
    /// (w), for a clipmap tile's water texels —
    /// `WaterSeaState::far_water` (`cartopolis::systems::water::WaterSeaState::far_water`).
    pub water_body: Vec4,
    /// x: the surface's roughness with every wave retired. y: how clear the sky
    /// is. z: `1.0` on a clipmap tile, whose alpha carries the water mask, and
    /// `0.0` on every other ground surface — the off switch.
    pub water_sea: Vec4,
    /// The ground cloud deck
    /// ([`CLOUD_FIELD_BUFFER`](crate::sky::CLOUD_FIELD_BUFFER)), for a
    /// clipmap tile's water texels to mirror the same clouds the streamed water
    /// inside the ring mirrors — without it, a lake would show its clouds up to
    /// the ring's edge and none past it. Every ground material binds the one
    /// buffer, and only a water texel reads it.
    #[storage(105, read_only, visibility(fragment), binding_array(111))]
    pub clouds: Handle<ShaderBuffer>,
    /// Compile the screen-door variant ([`GroundKey`]). Written on every ground
    /// material at once by `sewers::apply_ground_see_through`.
    pub see_through: bool,
    /// Compile the fade-in variant ([`GroundKey`]): a screen door that closes
    /// over [`FADE_IN_S`] from the moment in `lod` ([`fade_lod`]). Only on the
    /// copies `map_geometry` draws an arriving cell with, one per cell and group,
    /// which it swaps back to the ordinary material once the fade is over.
    pub fade_in: bool,
}

impl Default for GroundDetailExtension {
    fn default() -> Self {
        Self {
            params: Vec4::ZERO,
            slope: None,
            lod: Vec4::ZERO,
            basemap: None,
            water_body: Vec4::ZERO,
            water_sea: Vec4::ZERO,
            // Never `Handle::default()`, which names no asset: see
            // `CLOUD_FIELD_BUFFER` for what a missing storage buffer costs.
            clouds: crate::sky::CLOUD_FIELD_BUFFER,
            see_through: false,
            fade_in: false,
        }
    }
}

impl MaterialExtension for GroundDetailExtension {
    fn fragment_shader() -> ShaderRef {
        "shaders/ground_detail.wesl".into()
    }

    fn specialize(
        _pipeline: &MaterialExtensionPipeline,
        descriptor: &mut RenderPipelineDescriptor,
        _layout: &MeshVertexBufferLayoutRef,
        key: MaterialExtensionKey<Self>,
    ) -> Result<(), SpecializedMeshPipelineError> {
        specialize_ground(descriptor, key.bind_group_data);
        Ok(())
    }
}
