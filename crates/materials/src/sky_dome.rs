//! The fallback sky's material. When it is drawn, and everything that drives it,
//! is `cartopolis::systems::sky_dome`.

use bevy::pbr::{MaterialPipeline, MaterialPipelineKey};
use bevy::prelude::*;
use bevy::render::mesh::MeshVertexBufferLayoutRef;
use bevy::render::render_resource::{
    AsBindGroup, RenderPipelineDescriptor, SpecializedMeshPipelineError,
};
use bevy::shader::ShaderRef;

/// The dome's material. One shell, five uniforms, no textures — see the module
/// header (`cartopolis::systems::sky_dome`) for why that list matters.
#[derive(Asset, TypePath, AsBindGroup, Clone)]
pub struct SkyDomeMaterial {
    /// xyz: world direction to the sun. w: sun-disc brightness.
    #[uniform(0)]
    pub sun: Vec4,
    /// xyz: Rayleigh optical depth at the zenith. w: Mie asymmetry.
    #[uniform(1)]
    pub rayleigh: Vec4,
    /// xyz: aerosol optical depth at the zenith. w: cosine of the sun disc's
    /// angular radius.
    #[uniform(2)]
    pub mie: Vec4,
    /// xyz: the radiance being scattered. w unused.
    #[uniform(3)]
    pub source: Vec4,
    /// xyz: the twilight/night floor. w unused.
    #[uniform(4)]
    pub night: Vec4,
}

impl Material for SkyDomeMaterial {
    fn fragment_shader() -> ShaderRef {
        "shaders/sky_dome.wgsl".into()
    }

    fn specialize(
        _pipeline: &MaterialPipeline,
        descriptor: &mut RenderPipelineDescriptor,
        _layout: &MeshVertexBufferLayoutRef,
        _key: MaterialPipelineKey<Self>,
    ) -> Result<(), SpecializedMeshPipelineError> {
        // The camera is inside the shell, so it is the interior that is wanted.
        descriptor.primitive.cull_mode = None;
        // Opaque (so it composites before every transparent shell — stars, moon,
        // haze and cloud all have to land on top of it, and they are all
        // camera-centred, which makes their distance sort a tie the dome must not
        // be entered into), but writing no depth: the shell is farther than
        // anything in the flat tier, so the depth *test* is what keeps it behind
        // the world, and writing depth would only give it the chance to reject
        // something drawn later.
        if let Some(depth) = descriptor.depth_stencil.as_mut() {
            depth.depth_write_enabled = Some(false);
        }
        Ok(())
    }
}
