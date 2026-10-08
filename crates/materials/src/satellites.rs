//! The satellite layer's material. The catalogue, the propagation and the mesh
//! are `cartopolis::systems::satellites`.

use bevy::pbr::{MaterialPipeline, MaterialPipelineKey};
use bevy::prelude::*;
use bevy::render::mesh::MeshVertexBufferLayoutRef;
use bevy::render::render_resource::{
    AsBindGroup, RenderPipelineDescriptor, SpecializedMeshPipelineError,
};
use bevy::shader::ShaderRef;

/// One quad per satellite, placed and sized in the vertex stage
/// (`shaders/satellites.wgsl`).
///
/// The mesh is a snapshot: each satellite's position and velocity at one
/// instant, in the anchor's frame. The shader carries every quad forward from
/// that instant under the Earth's gravity, so the mesh is rebuilt every few
/// seconds rather than every frame and nothing is uploaded in between but these
/// eight floats.
#[derive(Asset, TypePath, AsBindGroup, Clone)]
pub struct SatelliteMaterial {
    /// xyz: the Earth's centre in the mesh's own (anchor) frame, metres.
    /// w: seconds since the instant the mesh describes.
    #[uniform(0)]
    pub motion: Vec4,
    /// x: the Earth's gravitational parameter, m³/s². y: brightness. z: point
    /// size multiplier (the display's scale factor). w: spare.
    #[uniform(1)]
    pub params: Vec4,
}

impl Material for SatelliteMaterial {
    fn vertex_shader() -> ShaderRef {
        "shaders/satellites.wgsl".into()
    }

    fn fragment_shader() -> ShaderRef {
        "shaders/satellites.wgsl".into()
    }

    /// Additive, as the particles are: a satellite is a point of light, and
    /// additive puts it in the transparent phase, where it is depth-*tested*
    /// (the planet hides the ones behind it) without writing depth.
    fn alpha_mode(&self) -> AlphaMode {
        AlphaMode::Add
    }

    fn specialize(
        _pipeline: &MaterialPipeline,
        descriptor: &mut RenderPipelineDescriptor,
        _layout: &MeshVertexBufferLayoutRef,
        _key: MaterialPipelineKey<Self>,
    ) -> Result<(), SpecializedMeshPipelineError> {
        // The quads are turned to the screen in the vertex stage, so their
        // winding depends on the view.
        descriptor.primitive.cull_mode = None;
        Ok(())
    }
}
