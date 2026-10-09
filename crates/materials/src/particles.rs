//! The particle emitters' material. The quad pool and the systems that build and
//! drive emitters are `cartopolis::systems::particles`.

use bevy::pbr::{MaterialPipeline, MaterialPipelineKey};
use bevy::prelude::*;
use bevy::render::mesh::MeshVertexBufferLayoutRef;
use bevy::render::render_resource::{
    AsBindGroup, RenderPipelineDescriptor, SpecializedMeshPipelineError,
};
use bevy::shader::ShaderRef;
use cartopolis_core::ParticleEffect;

/// The uniforms `particles.wesl` reads. Grouped as `vec4`s rather than as twenty
/// scalars because a uniform block is padded to 16 bytes per binding either way,
/// so packing is free and unpacking is not.
#[derive(Asset, TypePath, AsBindGroup, Clone)]
pub struct ParticleMaterial {
    /// x: count. y: life. z: source radius. w: turbulence.
    #[uniform(0)]
    pub emitter: Vec4,
    /// x: speed. y: cone half-angle (radians). z: up acceleration.
    /// w: burst start time, or a negative number for a continuous emitter.
    #[uniform(1)]
    pub motion: Vec4,
    /// x: size at birth. y: size at death. z: opacity. w: spare.
    #[uniform(2)]
    pub sizing: Vec4,
    /// Linear emissive colour at birth and at death.
    #[uniform(3)]
    pub hot: Vec4,
    #[uniform(4)]
    pub cool: Vec4,
}

impl Material for ParticleMaterial {
    fn vertex_shader() -> ShaderRef {
        "shaders/particles.wesl".into()
    }

    fn fragment_shader() -> ShaderRef {
        "shaders/particles.wesl".into()
    }

    /// Additive, which under bevy is premultiplied blending — the shader outputs
    /// an alpha of zero to reach `src + dst`. Fire adds light rather than covering
    /// what is behind it, and this also puts the emitter in the transparent phase,
    /// where depth writing is already off: a particle must not occlude the one
    /// behind it, and a hundred overlapping quads that each wrote depth would
    /// render as a hard-edged sheet.
    fn alpha_mode(&self) -> AlphaMode {
        AlphaMode::Add
    }

    fn specialize(
        _pipeline: &MaterialPipeline,
        descriptor: &mut RenderPipelineDescriptor,
        _layout: &MeshVertexBufferLayoutRef,
        _key: MaterialPipelineKey<Self>,
    ) -> Result<(), SpecializedMeshPipelineError> {
        // The quads are turned to face the camera in the vertex stage, so their
        // winding depends on where the camera is; culling would drop half of them
        // from half the angles. (`navigation::ribbon` sets this for the same
        // reason and says so — a winding that can hide geometry is a bug waiting
        // to be misdiagnosed as "nothing spawned".)
        descriptor.primitive.cull_mode = None;
        Ok(())
    }
}

impl ParticleMaterial {
    /// The uniforms for an effect. `burst_start` is the clock reading a one-shot
    /// began at, and is ignored for a continuous emitter.
    pub fn from_effect(e: &ParticleEffect, burst_start: f32) -> Self {
        let e = e.sanitized();
        Self {
            emitter: Vec4::new(f32::from(e.count), e.life, e.source_radius, e.turbulence),
            motion: Vec4::new(
                e.speed,
                e.spread_deg.to_radians(),
                e.gravity,
                if e.burst { burst_start } else { -1.0 },
            ),
            sizing: Vec4::new(e.size_start, e.size_end, e.alpha, 0.0),
            hot: Vec4::new(e.color_start[0], e.color_start[1], e.color_start[2], 1.0),
            cool: Vec4::new(e.color_end[0], e.color_end[1], e.color_end[2], 1.0),
        }
    }
}
