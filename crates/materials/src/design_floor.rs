//! The design room's floor: a technical grid drawn per pixel
//! (`shaders/design_floor.wesl`). The room itself is
//! `cartopolis::systems::design_room`.

use bevy::pbr::{MaterialPipeline, MaterialPipelineKey};
use bevy::prelude::*;
use bevy::render::mesh::MeshVertexBufferLayoutRef;
use bevy::render::render_resource::{
    AsBindGroup, RenderPipelineDescriptor, SpecializedMeshPipelineError,
};
use bevy::shader::ShaderRef;

/// The floor's one uniform. Its mesh carries UV0 in metres from the spot.
#[derive(Asset, TypePath, AsBindGroup, Clone)]
pub struct DesignFloorMaterial {
    /// x: the room's white, linear. y, z: the grid fades out between these
    /// distances from the spot, metres. w: spare.
    #[uniform(0)]
    pub params: Vec4,
}

impl Material for DesignFloorMaterial {
    fn fragment_shader() -> ShaderRef {
        "shaders/design_floor.wesl".into()
    }

    fn enable_shadows() -> bool {
        false
    }

    fn specialize(
        _pipeline: &MaterialPipeline,
        descriptor: &mut RenderPipelineDescriptor,
        _layout: &MeshVertexBufferLayoutRef,
        _key: MaterialPipelineKey<Self>,
    ) -> Result<(), SpecializedMeshPipelineError> {
        // One quad, seen from above only; no winding to get wrong.
        descriptor.primitive.cull_mode = None;
        Ok(())
    }
}
