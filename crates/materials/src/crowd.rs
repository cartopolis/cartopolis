//! The street crowd's material: `StandardMaterial`, with a vertex stage that
//! plays a walker's walk out of a texture of baked poses. The bodies and the
//! systems that drive them are `cartopolis::systems::crowd`.

use bevy::pbr::{ExtendedMaterial, MaterialExtension};
use bevy::prelude::*;
use bevy::render::render_resource::{AsBindGroup, ShaderType};
use bevy::shader::ShaderRef;

/// One per body and level of detail; every walker wearing it shares it.
pub type CrowdMaterial = ExtendedMaterial<StandardMaterial, CrowdExtension>;

/// How many top and bottom colours a walker's `MeshTag` can pick from.
pub const CROWD_PALETTE: usize = 16;

/// Uniforms and the pose texture for `shaders/crowd.wesl` and
/// `shaders/crowd_prepass.wesl`, all read in the vertex stage.
#[derive(Asset, AsBindGroup, Reflect, Clone, Debug)]
#[uniform(100, CrowdGpu)]
pub struct CrowdExtension {
    /// x: the mesh's vertices. y: the poses in its walk cycle (the standing
    /// pose is the one after them). z: the pose texture's width in texels.
    pub counts: UVec4,
    /// The colours a walker's top and bottom may take, linear.
    pub tops: [Vec4; CROWD_PALETTE],
    pub bottoms: [Vec4; CROWD_PALETTE],
    /// Two texels a vertex a pose — its position, then its normal — vertex
    /// after vertex, pose after pose, in rows `counts.z` wide. Read with
    /// `textureLoad`, so there is no sampler.
    #[texture(101, visibility(vertex))]
    pub poses: Handle<Image>,
}

/// [`CrowdExtension`]'s uniforms as the shaders' `Crowd` lays them out.
#[derive(ShaderType, Clone, Copy, Debug)]
pub struct CrowdGpu {
    pub counts: UVec4,
    pub tops: [Vec4; CROWD_PALETTE],
    pub bottoms: [Vec4; CROWD_PALETTE],
}

impl From<&CrowdExtension> for CrowdGpu {
    fn from(c: &CrowdExtension) -> Self {
        Self {
            counts: c.counts,
            tops: c.tops,
            bottoms: c.bottoms,
        }
    }
}

impl MaterialExtension for CrowdExtension {
    fn vertex_shader() -> ShaderRef {
        "shaders/crowd.wesl".into()
    }

    // The shadow and depth passes must pose the body too, or a walker casts
    // the shadow of a body in its rest pose.
    fn prepass_vertex_shader() -> ShaderRef {
        "shaders/crowd_prepass.wesl".into()
    }
}
