//! The far building tier's material and the vertex attribute it reads. Why the
//! ring goes down into the map, and what a prepass would owe it, is
//! `cartopolis::systems::building_fade`'s module note.

use bevy::mesh::{MeshVertexAttribute, MeshVertexBufferLayoutRef};
use bevy::pbr::{
    ExtendedMaterial, MaterialExtension, MaterialExtensionKey, MaterialExtensionPipeline,
};
use bevy::prelude::*;
use bevy::render::render_resource::{
    AsBindGroup, RenderPipelineDescriptor, SpecializedMeshPipelineError, VertexFormat,
};
use bevy::shader::ShaderRef;

/// The ground height this vertex's building stands on, in the same frame as its
/// position — so the shader can scale the building's *height* rather than its
/// distance from the world's zero.
///
/// The attribute id is arbitrary but must not collide with bevy's own (below 10)
/// or with `road_depth::ATTRIBUTE_ROAD_RUNG`.
pub const ATTRIBUTE_BUILDING_BASE: MeshVertexAttribute =
    MeshVertexAttribute::new("BuildingBase", 812_774_915, VertexFormat::Float32);

/// The shader location the vertex stage reads the base from. Bevy's own mesh
/// attributes occupy 0–7; `road_depth` takes 8 on a different material.
const BASE_SHADER_LOCATION: u32 = 8;

/// The far tier's material: the `StandardMaterial` the lane always used, plus the
/// height ramp.
pub type FarBuildingMaterial = ExtendedMaterial<StandardMaterial, BuildingFade>;

/// Where the outer ring of buildings goes down into the map.
#[derive(Asset, AsBindGroup, Reflect, Debug, Clone, Default)]
pub struct BuildingFade {
    /// xy: the camera's ground position in **render-space** XZ — the centre the
    /// ring is measured from. z: the scale that turns a distance in metres into
    /// *ramp widths*. w: the distance, in those units, at which a building has no
    /// height left — the ring's own outer boundary.
    ///
    /// Ramp widths rather than cells so `FADE_CELLS`
    /// (`cartopolis::systems::building_fade::FADE_CELLS`) lives **here and only
    /// here**: the shader's ramp is then `smoothstep(w - 1.0, w, d)`, whose `1.0`
    /// is structural. A constant mirrored in a `.wgsl` file is one nobody edits in
    /// both places.
    ///
    /// `w = 0` is the off switch, and it is what a material carries before the
    /// camera has been read and what `CARTO_NO_BUILDING_FADE=1` pins it to.
    #[uniform(100)]
    pub ring: Vec4,
}

impl MaterialExtension for BuildingFade {
    fn vertex_shader() -> ShaderRef {
        "shaders/building_fade.wesl".into()
    }

    fn specialize(
        _pipeline: &MaterialExtensionPipeline,
        descriptor: &mut RenderPipelineDescriptor,
        layout: &MeshVertexBufferLayoutRef,
        _key: MaterialExtensionKey<Self>,
    ) -> Result<(), SpecializedMeshPipelineError> {
        // **Do nothing for a prepass**, matched on the label because that is the
        // only thing distinguishing the two calls — the mesh key carries the
        // prepass bits in both. `road_depth::specialize` carries the long version
        // of this warning; the short one is that replacing the vertex layout under
        // *bevy's* prepass shader is a pipeline that never validates, which
        // `handle_render_error` answers by halting the renderer for good.
        //
        // This client has no prepass today. The guard is not for today — see the
        // module docs on what a prepass would owe this material.
        if descriptor
            .label
            .as_ref()
            .is_some_and(|label| label.contains("prepass"))
        {
            return Ok(());
        }
        let Ok(vertex) = layout.0.get_layout(&[
            Mesh::ATTRIBUTE_POSITION.at_shader_location(0),
            Mesh::ATTRIBUTE_NORMAL.at_shader_location(1),
            Mesh::ATTRIBUTE_COLOR.at_shader_location(5),
            ATTRIBUTE_BUILDING_BASE.at_shader_location(BASE_SHADER_LOCATION),
        ]) else {
            // A mesh without the base attribute keeps bevy's own layout and draws
            // unfaded rather than not drawing: this layer is a softening, and
            // refusing the pipeline would trade a visible seam for a missing city.
            return Ok(());
        };
        descriptor.vertex.buffers = vec![vertex];
        Ok(())
    }
}
