//! The grass's material. The patch pool, the mask and the systems that drive
//! them are `cartopolis::systems::grass`.

use bevy::pbr::{ExtendedMaterial, MaterialExtension};
use bevy::prelude::*;
use bevy::render::render_resource::{AsBindGroup, ShaderType};
use bevy::shader::ShaderRef;

/// The one material every grass patch draws with.
pub type GrassMaterial = ExtendedMaterial<StandardMaterial, GrassExtension>;

/// Uniforms for `shaders/grass.wgsl`. Every one is read in the **vertex** stage —
/// this extension has no fragment shader at all, which is the point: the blade is
/// built here and then handed to `StandardMaterial` to be lit like any other
/// surface in the scene.
#[derive(Asset, AsBindGroup, Reflect, Clone, Debug)]
// One uniform buffer, not one per field: see the shader's `Turf` for the
// browser limit seven of them broke.
#[uniform(100, GrassGpu)]
pub struct GrassExtension {
    /// xy: the mask's minimum corner in anchor metres. z: its ground size.
    /// w: the ground plane the blades stand on.
    pub field: Vec4,
    /// x: blade height. y: blade width. z: slot spacing (= jitter range).
    /// w: patch edge length, the unit `MeshTag` counts in.
    pub blade: Vec4,
    /// xy: unit wind direction. z: sway amplitude. w: sway speed.
    pub wind: Vec4,
    /// x: fade start (m). y: fade end (m). z: mask height origin. w: height span.
    pub range: Vec4,
    /// rgb: tip colour, linear. a: how much darker the root is.
    pub tint: Vec4,
    /// x: clump size (m). y, z: the density the thinnest and thickest patches
    /// keep. w: how far a blade's facing may swing off its clump's, in radians.
    pub clump: Vec4,
    /// xy: the distance band over which this tier's blades *arrive*, staggered
    /// per blade — the far ring stands up across the exact band the near ring
    /// retires in. Below zero (the near tier) means always arrived. zw: spare.
    pub range2: Vec4,
    /// r: coverage. g: ground height. Sampled in the vertex stage, hence the
    /// visibility — the derive defaults a texture to fragment-only, and a
    /// vertex-stage sample against that fails bind-group validation at draw time
    /// rather than at compile time.
    #[texture(105, visibility(vertex))]
    #[sampler(106, visibility(vertex))]
    pub mask: Handle<Image>,
}

/// [`GrassExtension`]'s uniforms as `grass.wgsl`'s `Turf` lays them out, in
/// that order.
#[derive(ShaderType, Clone, Copy, Debug)]
pub struct GrassGpu {
    pub field: Vec4,
    pub blade: Vec4,
    pub wind: Vec4,
    pub range: Vec4,
    pub tint: Vec4,
    pub clump: Vec4,
    pub range2: Vec4,
}

/// What `#[uniform(100, GrassGpu)]` binds.
impl From<&GrassExtension> for GrassGpu {
    fn from(g: &GrassExtension) -> Self {
        Self {
            field: g.field,
            blade: g.blade,
            wind: g.wind,
            range: g.range,
            tint: g.tint,
            clump: g.clump,
            range2: g.range2,
        }
    }
}

impl MaterialExtension for GrassExtension {
    fn vertex_shader() -> ShaderRef {
        "shaders/grass.wgsl".into()
    }
}
