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

/// The most poses a body may bake (walk, standing, ride and wave): the
/// length of the shaders' `pivots` array.
pub const CROWD_MAX_POSES: usize = 40;

/// Uniforms and the pose texture for `shaders/crowd.wesl` and
/// `shaders/crowd_prepass.wesl`, all read in the vertex stage.
#[derive(Asset, AsBindGroup, Reflect, Clone, Debug)]
#[uniform(100, CrowdGpu)]
pub struct CrowdExtension {
    /// x: the mesh's vertices. y: the poses in the cycle it plays (a walker's
    /// walk, with the standing pose after it). z: the pose texture's width in
    /// texels. w: the poses in its wave (0 on the coarser level, and for a
    /// material that does not wave).
    pub counts: UVec4,
    /// Which poses this material plays: x the first of its cycle of `counts.y`
    /// (0, the walk, for a walker; the turn of the cranks after the standing
    /// pose for a rider), y the standing pose it eases into, z the first pose
    /// of the wave (after the ride).
    pub cycle: UVec4,
    /// The colours a walker's top and bottom may take, linear.
    pub tops: [Vec4; CROWD_PALETTE],
    pub bottoms: [Vec4; CROWD_PALETTE],
    /// Per pose, the head joint in the body's frame: what a head turn
    /// pivots about.
    pub pivots: [Vec4; CROWD_MAX_POSES],
    /// Two texels a vertex a pose — its position (with its share of a head
    /// turn in `w`), then its normal — vertex after vertex, pose after pose,
    /// in rows `counts.z` wide. Read with `textureLoad`, so there is no
    /// sampler.
    #[texture(101, visibility(vertex))]
    pub poses: Handle<Image>,
}

/// [`CrowdExtension`]'s uniforms as the shaders' `Crowd` lays them out.
#[derive(ShaderType, Clone, Copy, Debug)]
pub struct CrowdGpu {
    pub counts: UVec4,
    pub cycle: UVec4,
    pub tops: [Vec4; CROWD_PALETTE],
    pub bottoms: [Vec4; CROWD_PALETTE],
    pub pivots: [Vec4; CROWD_MAX_POSES],
}

impl From<&CrowdExtension> for CrowdGpu {
    fn from(c: &CrowdExtension) -> Self {
        Self {
            counts: c.counts,
            cycle: c.cycle,
            tops: c.tops,
            bottoms: c.bottoms,
            pivots: c.pivots,
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
