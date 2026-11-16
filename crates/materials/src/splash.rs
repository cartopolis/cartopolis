//! The splash's sheet of water: the crown a body throws up going in, the
//! column a feet-first drop sends up its middle, and the foam left on the
//! surface. The mesh and the systems that arm them are
//! `cartopolis::systems::splashes`; the shape and its motion are all in
//! `assets/shaders/splash.wesl`, a function of the clock like the particles.

use bevy::pbr::{MaterialPipeline, MaterialPipelineKey};
use bevy::prelude::*;
use bevy::render::mesh::MeshVertexBufferLayoutRef;
use bevy::render::render_resource::{
    AsBindGroup, RenderPipelineDescriptor, SpecializedMeshPipelineError,
};
use bevy::shader::ShaderRef;

/// What the sheet is drawn as.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SplashShape {
    /// An open, flaring shell with a ragged rim, rising and falling back.
    Crown,
    /// A narrow column up the middle, later and taller.
    Column,
    /// A flat, ragged patch of white water that breaks up as it fades.
    Foam,
}

/// The uniforms `splash.wesl` reads.
#[derive(Asset, TypePath, AsBindGroup, Clone)]
pub struct SplashMaterial {
    /// x: start time (`globals.time`), y: rise seconds, z: fall seconds,
    /// w: the shape (0 crown, 1 column, 2 foam).
    #[uniform(0)]
    pub timing: Vec4,
    /// x: radius at the start (m), y: radius gained over the life (m),
    /// z: height (m), w: a seed for the ragged edge.
    #[uniform(1)]
    pub shape: Vec4,
    /// The foam's reach (m) in eight directions, an eighth of a turn apart
    /// from +x towards +z: it fades out before the drawn water leaves its
    /// level, as it does beside a quay. Unused by the crown and column.
    #[uniform(2)]
    pub reach_a: Vec4,
    #[uniform(3)]
    pub reach_b: Vec4,
}

impl SplashMaterial {
    /// A sheet of `shape`, started at `start` on the shader's clock.
    #[must_use]
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        shape: SplashShape,
        start: f32,
        rise_s: f32,
        fall_s: f32,
        radius: f32,
        grow: f32,
        height: f32,
        seed: f32,
    ) -> Self {
        let w = match shape {
            SplashShape::Crown => 0.0,
            SplashShape::Column => 1.0,
            SplashShape::Foam => 2.0,
        };
        Self {
            timing: Vec4::new(start, rise_s.max(0.01), fall_s.max(0.01), w),
            shape: Vec4::new(radius, grow, height, seed),
            reach_a: Vec4::splat(1.0e6),
            reach_b: Vec4::splat(1.0e6),
        }
    }

    /// This sheet with the foam's reach in eight directions (m).
    #[must_use]
    pub fn with_reach(mut self, reach: [f32; 8]) -> Self {
        self.reach_a = Vec4::new(reach[0], reach[1], reach[2], reach[3]);
        self.reach_b = Vec4::new(reach[4], reach[5], reach[6], reach[7]);
        self
    }

    /// Seconds from the start until it has gone.
    #[must_use]
    pub fn life(&self) -> f32 {
        self.timing.y + self.timing.z
    }
}

impl Material for SplashMaterial {
    fn vertex_shader() -> ShaderRef {
        "shaders/splash.wesl".into()
    }

    fn fragment_shader() -> ShaderRef {
        "shaders/splash.wesl".into()
    }

    /// Blended, not added: white water covers what is behind it, which is
    /// what makes a crown read as a sheet rather than as light.
    fn alpha_mode(&self) -> AlphaMode {
        AlphaMode::Blend
    }

    fn specialize(
        _pipeline: &MaterialPipeline,
        descriptor: &mut RenderPipelineDescriptor,
        _layout: &MeshVertexBufferLayoutRef,
        _key: MaterialPipelineKey<Self>,
    ) -> Result<(), SpecializedMeshPipelineError> {
        // An open shell is seen from inside across the far side.
        descriptor.primitive.cull_mode = None;
        Ok(())
    }
}
