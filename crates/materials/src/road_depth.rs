//! The roads' material and the vertex attribute it reads. Why the road ladder
//! rides in the depth buffer is `cartopolis::systems::road_depth`'s module note.

use bevy::mesh::{MeshVertexAttribute, MeshVertexBufferLayoutRef};
use bevy::pbr::{
    ExtendedMaterial, MaterialExtension, MaterialExtensionKey, MaterialExtensionPipeline,
};
use bevy::prelude::*;
use bevy::render::render_resource::{
    AsBindGroup, RenderPipelineDescriptor, SpecializedMeshPipelineError, VertexFormat,
};
use bevy::render::storage::ShaderBuffer;
use bevy::shader::ShaderRef;

use crate::ground_detail::{GroundKey, GroundUniform};

/// The road ladder rung this vertex draws on — `SurfaceMesh::rungs`.
///
/// Constant across a ribbon, so it interpolates to itself and every fragment of
/// one road gets the same offset. A `f32` rather than a `u32` because it is
/// multiplied straight into clip-space `z`, and the attribute id is arbitrary but
/// must not collide with bevy's own (which live below 10).
pub const ATTRIBUTE_ROAD_RUNG: MeshVertexAttribute =
    MeshVertexAttribute::new("RoadRung", 970_412_233, VertexFormat::Float32);

/// `GroundUniform::water_sea.w` on a road: `ground_detail.wesl` tests `> 0.5`.
const ROAD_FLAG: f32 = 1.0;

/// The shader location `road_depth.wesl` reads the rung from. Bevy's own mesh
/// attributes occupy 0–7, so this is the first free one.
const RUNG_SHADER_LOCATION: u32 = 8;

/// Roads and their casings, ordered by [`ATTRIBUTE_ROAD_RUNG`].
pub type RoadMaterial = ExtendedMaterial<StandardMaterial, RoadDepthExtension>;

/// The vertex stage that applies the depth rung, plus the sub-metre ground slope the
/// flat surfaces carry (`systems::ground_detail`) — a carriageway's camber is exactly
/// the kind of half-metre relief no mesh here can hold.
///
/// The bindings and their meaning are `GroundDetailExtension`'s, deliberately at the
/// same numbers, so the *fragment* shader is literally the same file. Two material
/// types, two vertex stages, one description of what the ground looks like up close.
///
/// That sameness is a standing obligation, not a coincidence: a binding added to one
/// must be added to the other, or the shared shader declares a binding this material's
/// layout does not supply and the pipeline fails validation — which
/// `lib.rs::handle_render_error` answers by halting the renderer for the rest of the
/// process. The clipmap cross-fade (`lod`, and the basemap at 103–104) is here for that reason alone; a
/// road is never a clipmap tile, so they are never written and `lod.w` stays 0.
#[derive(Asset, AsBindGroup, Reflect, Debug, Clone)]
// A bare name: the derive parses an identifier and silently drops a path.
#[bind_group_data(GroundKey)]
#[data(100, GroundUniform, binding_array(110))]
#[bindless(index_table(range(100..108), binding(109)))]
pub struct RoadDepthExtension {
    /// xy: the detail box's centre in render-space XZ. z: 1 / its half-extent.
    /// w: strength, `0.0` when there is no tier to read.
    pub detail: Vec4,
    #[texture(101)]
    #[sampler(102)]
    pub slope: Option<Handle<Image>>,
    /// Always zero — see the type's own docs.
    pub lod: Vec4,
    /// Always `None` — see the type's own docs.
    #[texture(103)]
    #[sampler(104)]
    pub basemap: Option<Handle<Image>>,
    /// Always zero — see the type's own docs. The clipmap's water is
    /// here for the same reason the cross-fade is: a road carries no water mask,
    /// and `water_sea.z == 0` keeps the shader off it.
    pub water_body: Vec4,
    pub water_sea: Vec4,
    /// The cloud deck the clipmap's water mirrors (105) — bound for the same
    /// reason, and never read on a road for the same `water_sea.z`.
    #[storage(105, read_only, visibility(fragment), binding_array(111))]
    pub clouds: Handle<ShaderBuffer>,
    /// Always `None` and zero — the painted cross-fade is the clipmap's, here for
    /// the shared layout like the rest.
    #[texture(106)]
    #[sampler(107)]
    pub painted: Option<Handle<Image>>,
    pub far: Vec4,
    pub tile: Vec4,
    /// The ground's screen-door variant (`ground_detail::GroundKey`), so a road
    /// opens onto what is under it exactly where the pavement beside it does.
    pub see_through: bool,
}

impl From<&RoadDepthExtension> for GroundUniform {
    fn from(e: &RoadDepthExtension) -> Self {
        Self {
            detail: e.detail,
            lod: e.lod,
            water_body: e.water_body,
            // w says "a road ribbon" to the shared shader, which gives a
            // carriageway its aggregate in relief; every ground surface leaves it 0.
            water_sea: e.water_sea.with_w(ROAD_FLAG),
            far: e.far,
            tile: e.tile,
        }
    }
}

impl From<&RoadDepthExtension> for GroundKey {
    fn from(e: &RoadDepthExtension) -> Self {
        Self {
            see_through: e.see_through,
        }
    }
}

impl Default for RoadDepthExtension {
    fn default() -> Self {
        Self {
            detail: Vec4::ZERO,
            slope: None,
            lod: Vec4::ZERO,
            basemap: None,
            water_body: Vec4::ZERO,
            water_sea: Vec4::ZERO,
            // Never `Handle::default()`, which names no asset: a road material
            // whose storage buffer does not exist never prepares, and draws
            // nothing (`sky::CLOUD_FIELD_BUFFER`).
            painted: None,
            far: Vec4::ZERO,
            tile: Vec4::ZERO,
            clouds: crate::sky::CLOUD_FIELD_BUFFER,
            see_through: false,
        }
    }
}

impl MaterialExtension for RoadDepthExtension {
    fn vertex_shader() -> ShaderRef {
        "shaders/road_depth.wesl".into()
    }

    /// `ground_detail`'s, not one of our own: the ladder is a *vertex* trick and the
    /// fragment has nothing to add to it beyond the slope every other ground surface
    /// already gets.
    fn fragment_shader() -> ShaderRef {
        "shaders/ground_detail.wesl".into()
    }

    // `prepass_vertex_shader` is deliberately left at bevy's default. The rung is
    // a depth-buffer trick for the *camera's* view of a coplanar sheet; applying
    // it to the shadow map would bias a road against the sun instead, for no
    // reason. The geometry is flat and honest, so the default is correct there.

    fn specialize(
        _pipeline: &MaterialExtensionPipeline,
        descriptor: &mut RenderPipelineDescriptor,
        layout: &MeshVertexBufferLayoutRef,
        key: MaterialExtensionKey<Self>,
    ) -> Result<(), SpecializedMeshPipelineError> {
        // The fragment variant first: it is harmless on a prepass descriptor,
        // whose fragment stage is bevy's and never reads the def.
        crate::ground_detail::specialize_ground(descriptor, key.bind_group_data);
        // **This hook runs for the prepass too, and must do nothing there.**
        //
        // Bevy calls the material's `specialize` from `PrepassPipelineSpecializer`
        // as well as from the main material pipeline — its own source carries the
        // comment "it's possible to change something that would break the prepass
        // but be fine in the main pass", and this is that case. The prepass
        // compiles **bevy's** vertex shader (`prepass_vertex_shader` is left at
        // the default, for the reason above), which declares a different set of
        // vertex inputs; replacing the buffer layout with the one our shader wants
        // leaves that shader with a location nothing supplies.
        //
        // The failure is not subtle and is not survivable: `RenderPipeline with
        // 'pbr_prepass_pipeline' label is invalid` at `Queue::submit`, which
        // `lib.rs::handle_render_error` halts the renderer on — i.e. the app draws
        // nothing for the rest of the process. It stayed latent for as long as
        // this client had no prepass at all, and appeared the moment ambient
        // occlusion required one.
        //
        // **There is no prepass again**: ambient occlusion was removed on
        // 2026-09-21 (`systems::render_features`) and took `DepthPrepass` and
        // `NormalPrepass` with it, so this branch is currently unreachable. It
        // stays because the last time it was unreachable somebody added a feature
        // that needed a prepass, and the cost of keeping it is one string compare
        // per pipeline specialisation.
        //
        // Matched on the label because that is the only thing here that
        // distinguishes the two calls: the mesh key carries the prepass *bits* in
        // both, since the main pass is told which prepasses exist.
        if descriptor
            .label
            .as_ref()
            .is_some_and(|label| label.contains("prepass"))
        {
            return Ok(());
        }
        // The vertex shader declares the rung at `RUNG_SHADER_LOCATION`, so the
        // buffer layout has to supply it or the pipeline fails validation.
        let vertex_layout = layout.0.get_layout(&[
            Mesh::ATTRIBUTE_POSITION.at_shader_location(0),
            Mesh::ATTRIBUTE_NORMAL.at_shader_location(1),
            Mesh::ATTRIBUTE_UV_0.at_shader_location(2),
            ATTRIBUTE_ROAD_RUNG.at_shader_location(RUNG_SHADER_LOCATION),
        ])?;
        descriptor.vertex.buffers = vec![vertex_layout];
        Ok(())
    }
}
