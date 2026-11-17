//! The lens's areas, drawn by the GPU. The boards, the colours and the meshes
//! are `cartopolis::systems::lens_areas`.

use bevy::pbr::{MaterialPipeline, MaterialPipelineKey};
use bevy::prelude::*;
use bevy::render::mesh::MeshVertexBufferLayoutRef;
use bevy::render::render_resource::{
    AsBindGroup, CompareFunction, RenderPipelineDescriptor, SpecializedMeshPipelineError,
};
use bevy::shader::ShaderRef;

/// One board of a lens: its areas' fills (a triangle list) and their edges (a
/// line list) share it, every vertex carrying the texel of its colour
/// (`shaders/lens_areas.wesl`).
///
/// The meshes are built once per board and frame of reference; what changes
/// as a year plays or a figure is switched is only `colours`, one texel an
/// area, so nothing else is uploaded while the map moves.
#[derive(Asset, TypePath, AsBindGroup, Clone)]
pub struct LensAreaMaterial {
    /// x: the whole board's opacity. y: how far behind the horizon a point
    /// may stand and still be drawn, metres. zw: spare.
    #[uniform(0)]
    pub params: Vec4,
    /// One texel per area, read whole (`textureLoad`): sRGB, not
    /// premultiplied, alpha the fill's opacity.
    #[texture(1)]
    pub colours: Handle<Image>,
}

impl Material for LensAreaMaterial {
    fn vertex_shader() -> ShaderRef {
        "shaders/lens_areas.wesl".into()
    }

    fn fragment_shader() -> ShaderRef {
        "shaders/lens_areas.wesl".into()
    }

    /// Blended, so it is in the transparent phase: after the map, and
    /// writing no depth.
    fn alpha_mode(&self) -> AlphaMode {
        AlphaMode::Blend
    }

    /// Out of the depth prepass: it writes no depth, and one written for it
    /// first would hide the map under it.
    fn enable_prepass() -> bool {
        false
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
        // Read over the map as the painted overlay was: over the buildings
        // and whatever relief the board was not laid on. The far side of the
        // planet is not drawn through it, because the shader drops what faces
        // away from the camera.
        if let Some(depth) = descriptor.depth_stencil.as_mut() {
            depth.depth_compare = Some(CompareFunction::Always);
        }
        // Earcut's winding follows the ring's, which the data does not fix.
        descriptor.primitive.cull_mode = None;
        Ok(())
    }
}
