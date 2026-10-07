//! The ground's material. The slope tier it reads, and the systems that bake and
//! place it, are `cartopolis::systems::ground_detail`.

use bevy::mesh::MeshVertexBufferLayoutRef;
use bevy::pbr::{
    ExtendedMaterial, MaterialExtension, MaterialExtensionKey, MaterialExtensionPipeline,
};
use bevy::prelude::*;
use bevy::render::render_resource::{
    AsBindGroup, RenderPipelineDescriptor, SpecializedMeshPipelineError,
};
use bevy::render::storage::ShaderBuffer;
use bevy::shader::ShaderRef;

/// The ground material every flat surface and every relief tile carries.
pub type GroundMaterial = ExtendedMaterial<StandardMaterial, GroundDetailExtension>;

/// Which variant of `shaders/ground_detail.wgsl` a ground material compiles.
///
/// One flag so far: [`GroundKey::see_through`], the screen-door the underground
/// layer looks through (`systems::sewers`). It is a *variant* rather than a
/// uniform because the cost is in the shader's existence, not in its branch: a
/// fragment shader that can `discard` loses early depth testing for every draw
/// it is used in, whether or not it ever takes the branch, and the ground is
/// several stacked sheets deep. So the ordinary ground never has the statement in
/// it at all.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub struct GroundKey {
    pub see_through: bool,
}

impl From<&GroundDetailExtension> for GroundKey {
    fn from(e: &GroundDetailExtension) -> Self {
        Self {
            see_through: e.see_through,
        }
    }
}

/// Add the shader defs a [`GroundKey`] asks for to a ground pipeline. Shared with
/// `road_depth`, whose fragment shader is this one.
pub fn specialize_ground(descriptor: &mut RenderPipelineDescriptor, key: GroundKey) {
    if key.see_through {
        if let Some(fragment) = descriptor.fragment.as_mut() {
            fragment.shader_defs.push("GROUND_SEE_THROUGH".into());
        }
    }
}

/// The sub-metre half of the ground, for `shaders/ground_detail.wgsl`.
#[derive(Asset, AsBindGroup, Reflect, Clone, Debug)]
#[bind_group_data(GroundKey)]
pub struct GroundDetailExtension {
    /// xy: the detail box's centre in **render-space** XZ. z: 1 / its half-extent.
    /// w: strength, and `0.0` means "no tier" — see the module docs
    /// (`cartopolis::systems::ground_detail`).
    #[uniform(100)]
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
    #[uniform(103)]
    pub lod: Vec4,
    /// The tile's *own* basemap image, bound a second time so the shader can sample
    /// it at a mip bias.
    ///
    /// The same handle `base_color_texture` already holds, so this costs a binding
    /// and no memory. Bound here rather than reached for through
    /// `bevy_pbr::pbr_bindings`, whose base-colour binding is `#ifdef BINDLESS`-shaped
    /// and is bevy's to rearrange; this one is ours and is the same on every platform.
    #[texture(104)]
    #[sampler(105)]
    pub basemap: Option<Handle<Image>>,
    /// The water surface's deep body colour (rgb, linear) and its `reflectance`
    /// (w), for a clipmap tile's water texels —
    /// `WaterSeaState::far_water` (`cartopolis::systems::water::WaterSeaState::far_water`).
    #[uniform(106)]
    pub water_body: Vec4,
    /// x: the surface's roughness with every wave retired. y: how clear the sky
    /// is. z: `1.0` on a clipmap tile, whose alpha carries the water mask, and
    /// `0.0` on every other ground surface — the off switch.
    #[uniform(107)]
    pub water_sea: Vec4,
    /// The ground cloud deck
    /// ([`CLOUD_FIELD_BUFFER`](crate::sky::CLOUD_FIELD_BUFFER)), for a
    /// clipmap tile's water texels to mirror the same clouds the streamed water
    /// inside the ring mirrors — without it, a lake would show its clouds up to
    /// the ring's edge and none past it. Every ground material binds the one
    /// buffer, and only a water texel reads it.
    #[storage(108, read_only, visibility(fragment))]
    pub clouds: Handle<ShaderBuffer>,
    /// Compile the screen-door variant ([`GroundKey`]). Written on every ground
    /// material at once by `sewers::apply_ground_see_through`.
    pub see_through: bool,
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
        }
    }
}

impl MaterialExtension for GroundDetailExtension {
    fn fragment_shader() -> ShaderRef {
        "shaders/ground_detail.wgsl".into()
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
