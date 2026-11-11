//! The street names' material: one `StandardMaterial` shared by every sign,
//! unlit and alpha-blended, whose fragment reads the sign's name out of one
//! atlas at the slot its `MeshTag` names. The signs, the atlas and the slots
//! are `cartopolis::systems::street_labels` and `label_atlas`.
//!
//! One material and one texture for the whole pool, where there were 160 of
//! each: a sign taking a new name used to be a material written through
//! `get_mut`, re-prepared on the render thread — ~65 of them every time a
//! street-level turn re-placed the names, 8 ms a frame of a 55 ms turning frame
//! on an M5 Pro (`docs/notes/render-performance-survey.md`, 2026-11-10). Now it
//! is a `MeshTag` write and a texture region written from the render world.

use bevy::pbr::{ExtendedMaterial, MaterialExtension};
use bevy::prelude::*;
use bevy::render::render_resource::{AsBindGroup, ShaderType};
use bevy::shader::ShaderRef;

/// Every sign wears this one.
pub type LabelSignMaterial = ExtendedMaterial<StandardMaterial, LabelSignExtension>;

/// Uniforms and the atlas for `shaders/label_sign.wesl`.
#[derive(Asset, AsBindGroup, Reflect, Clone, Debug)]
#[uniform(100, LabelSignGpu)]
pub struct LabelSignExtension {
    /// x, y: one slot cell as a fraction of the atlas. z: cells per row.
    pub cell: Vec4,
    /// Ink colour, linear.
    pub ink: Vec4,
    /// Halo colour, linear; w: the halo's opacity.
    pub halo: Vec4,
    /// Two channels a texel: the ink's coverage in R, the halo's in G.
    #[texture(101)]
    #[sampler(102)]
    pub atlas: Handle<Image>,
}

/// [`LabelSignExtension`]'s uniforms as the shader's `LabelSign` lays them out.
#[derive(ShaderType, Clone, Copy, Debug)]
pub struct LabelSignGpu {
    pub cell: Vec4,
    pub ink: Vec4,
    pub halo: Vec4,
}

impl From<&LabelSignExtension> for LabelSignGpu {
    fn from(e: &LabelSignExtension) -> Self {
        Self {
            cell: e.cell,
            ink: e.ink,
            halo: e.halo,
        }
    }
}

impl MaterialExtension for LabelSignExtension {
    fn fragment_shader() -> ShaderRef {
        "shaders/label_sign.wesl".into()
    }
}

/// A sign's `MeshTag`, as the shader unpacks it: the slot in bits 0–7, then how
/// much of the slot's width and height the name fills, twelve bits each.
#[must_use]
pub fn sign_tag(slot: u32, u_frac: f32, v_frac: f32) -> u32 {
    let q = |f: f32| (f.clamp(0.0, 1.0) * 4095.0).round() as u32;
    (slot & 0xFF) | (q(u_frac) << 8) | (q(v_frac) << 20)
}

#[cfg(test)]
mod tests {
    use super::sign_tag;

    #[test]
    fn a_tag_round_trips_its_three_fields() {
        let t = sign_tag(37, 0.5, 1.0);
        assert_eq!(t & 0xFF, 37);
        assert_eq!((t >> 8) & 0xFFF, 2048);
        assert_eq!((t >> 20) & 0xFFF, 4095);
    }
}
