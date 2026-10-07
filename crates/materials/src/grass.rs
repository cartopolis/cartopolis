//! The grass's material. The patch pool, the mask and the systems that drive
//! them are `cartopolis::systems::grass`.

use bevy::pbr::{ExtendedMaterial, MaterialExtension};
use bevy::prelude::*;
use bevy::render::render_resource::AsBindGroup;
use bevy::shader::ShaderRef;

/// The one material every grass patch draws with.
pub type GrassMaterial = ExtendedMaterial<StandardMaterial, GrassExtension>;

/// Uniforms for `shaders/grass.wgsl`. Every one is read in the **vertex** stage —
/// this extension has no fragment shader at all, which is the point: the blade is
/// built here and then handed to `StandardMaterial` to be lit like any other
/// surface in the scene.
#[derive(Asset, AsBindGroup, Reflect, Clone, Debug)]
pub struct GrassExtension {
    /// xy: the mask's minimum corner in anchor metres. z: its ground size.
    /// w: the ground plane the blades stand on.
    #[uniform(100, visibility(vertex))]
    pub field: Vec4,
    /// x: blade height. y: blade width. z: slot spacing (= jitter range).
    /// w: patch edge length, the unit `MeshTag` counts in.
    #[uniform(101, visibility(vertex))]
    pub blade: Vec4,
    /// xy: unit wind direction. z: sway amplitude. w: sway speed.
    #[uniform(102, visibility(vertex))]
    pub wind: Vec4,
    /// x: fade start (m). y: fade end (m). z: mask height origin. w: height span.
    #[uniform(103, visibility(vertex))]
    pub range: Vec4,
    /// rgb: tip colour, linear. a: how much darker the root is.
    #[uniform(104, visibility(vertex))]
    pub tint: Vec4,
    /// x: clump size (m). y, z: the density the thinnest and thickest patches
    /// keep. w: how far a blade's facing may swing off its clump's, in radians.
    #[uniform(107, visibility(vertex))]
    pub clump: Vec4,
    /// xy: the distance band over which this tier's blades *arrive*, staggered
    /// per blade — the far ring stands up across the exact band the near ring
    /// retires in. Below zero (the near tier) means always arrived. zw: spare.
    #[uniform(108, visibility(vertex))]
    pub range2: Vec4,
    /// r: coverage. g: ground height. Sampled in the vertex stage, hence the
    /// visibility — the derive defaults a texture to fragment-only, and a
    /// vertex-stage sample against that fails bind-group validation at draw time
    /// rather than at compile time.
    #[texture(105, visibility(vertex))]
    #[sampler(106, visibility(vertex))]
    pub mask: Handle<Image>,
}

impl MaterialExtension for GrassExtension {
    fn vertex_shader() -> ShaderRef {
        "shaders/grass.wgsl".into()
    }
}
