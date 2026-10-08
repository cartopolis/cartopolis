//! The compact vertex formats the streamed map is uploaded in.
//!
//! **Graphics memory, not processor time, is what limits how much of a city the
//! client can hold**, and most of it is vertex data: Groningen seen from 3 km was
//! 954 MB resident on the desktop rung, 544 MB of it geometry
//! (`docs/notes/performance-suite.md`). Every streamed mesh used to be uploaded
//! as four-byte floats throughout — 12 bytes of normal for a vector that needs
//! three bytes of precision, 16 bytes of colour where four do, and four-byte
//! indices on meshes with a few hundred vertices.
//!
//! The trick that makes this cheap to adopt is that a bevy mesh attribute is
//! looked up by **id** while its format travels with the mesh's own vertex
//! layout (`Mesh::get_mesh_vertex_buffer_layout` reads the format from the
//! attribute stored on the mesh, never from bevy's constant). So
//! [`COMPACT_NORMAL`] carries the same id as `Mesh::ATTRIBUTE_NORMAL`, every
//! pipeline that asks for "the normal" finds it, and the GPU's vertex fetch
//! turns the bytes back into floats. wgpu lets a shader declare `vec3<f32>` over
//! a four-component format (`is_subtype_of` in wgpu-core's validation), so no
//! shader in the tree had to change for the normals.
//!
//! What this cannot do is make a CPU reader of these meshes keep working: a test
//! or system that matches `VertexAttributeValues::Float32x3` on a normal will
//! not find one. Positions are untouched, since bevy's own bounding-box pass
//! reads them as `Float32x3`.

use bevy::mesh::{Indices, MeshVertexAttribute, VertexAttributeValues, VertexFormat};

/// Bevy's normal attribute as four signed bytes; the fourth is padding.
///
/// A byte per axis is a worst-case error of about half a degree, below anything
/// a facade's or a street's shading can show, for eight bytes a vertex back.
pub const COMPACT_NORMAL: MeshVertexAttribute =
    MeshVertexAttribute::new("Vertex_Normal", 1, VertexFormat::Snorm8x4);
/// What one [`COMPACT_NORMAL`] costs.
pub const COMPACT_NORMAL_BYTES: usize = 4;

/// Bevy's colour attribute as four normalised bytes. Only for colours that stay
/// within 0..1 — a channel above 1 (a brightening tint) or an alpha carrying
/// something that is not a colour is clamped away.
pub const COMPACT_COLOR: MeshVertexAttribute =
    MeshVertexAttribute::new("Vertex_Color", 5, VertexFormat::Unorm8x4);
/// What one [`COMPACT_COLOR`] costs.
pub const COMPACT_COLOR_BYTES: usize = 4;

/// How far above 1 a surface group's vertex tint may run, and so the factor its
/// vertex colours are divided by before they are stored as bytes and its
/// material's base colour multiplied by to give the same product back.
///
/// The canopy's per-tree brightness (`scatter`'s `luma`) is ±4 % about 1, so a
/// tree's tint reaches 1.04 — the one thing that kept the tree meshes at four
/// floats of colour. Every group without volume tints at or under 1.
#[must_use]
pub fn tint_headroom(group: cartopolis_geo::vector_tiles::SurfaceGroup) -> f32 {
    use cartopolis_geo::vector_tiles::SurfaceGroup;
    match group {
        SurfaceGroup::Canopy | SurfaceGroup::Trunk => 1.05,
        _ => 1.0,
    }
}

/// Put a surface group's vertex colours on `mesh`: four bytes where every
/// channel fits once the group's [`tint_headroom`] is divided out (the group's
/// material multiplies it back — `map_geometry`'s material constructors and
/// `vegetation::set_base_color`), four floats where one does not. A colour is
/// never clamped to make it fit.
///
/// **Every mesh drawn with a group's shared material must come through here**,
/// or its colours and the material's headroom disagree and it draws 5 % off —
/// the scattered trees (`map_geometry`) and the surveyed ones (`street_detail`)
/// hold the same canopy handle.
pub fn insert_group_colors(
    mesh: &mut bevy::mesh::Mesh,
    group: cartopolis_geo::vector_tiles::SurfaceGroup,
    colors: Vec<[f32; 4]>,
) {
    let h = tint_headroom(group);
    if colors
        .iter()
        .all(|c| c[..3].iter().all(|x| (0.0..=h).contains(x)) && (0.0..=1.0).contains(&c[3]))
    {
        let scaled: Vec<[f32; 4]> = colors
            .iter()
            .map(|c| [c[0] / h, c[1] / h, c[2] / h, c[3]])
            .collect();
        mesh.insert_attribute(COMPACT_COLOR, pack_colors(&scaled));
    } else if h == 1.0 {
        mesh.insert_attribute(bevy::mesh::Mesh::ATTRIBUTE_COLOR, colors);
    } else {
        // Floats, but still divided: the material carries the headroom either way.
        let scaled: Vec<[f32; 4]> = colors
            .iter()
            .map(|c| [c[0] / h, c[1] / h, c[2] / h, c[3]])
            .collect();
        mesh.insert_attribute(bevy::mesh::Mesh::ATTRIBUTE_COLOR, scaled);
    }
}

/// Normals in the [`COMPACT_NORMAL`] format.
#[must_use]
pub fn pack_normals(normals: &[[f32; 3]]) -> VertexAttributeValues {
    VertexAttributeValues::Snorm8x4(normals.iter().map(|n| pack_snorm8(*n)).collect())
}

/// Colours in the [`COMPACT_COLOR`] format.
#[must_use]
pub fn pack_colors(colors: &[[f32; 4]]) -> VertexAttributeValues {
    VertexAttributeValues::Unorm8x4(colors.iter().map(|c| pack_unorm8(*c)).collect())
}

/// A unit vector as four signed bytes.
fn pack_snorm8(n: [f32; 3]) -> [i8; 4] {
    let q = |x: f32| (x.clamp(-1.0, 1.0) * 127.0).round() as i8;
    [q(n[0]), q(n[1]), q(n[2]), 0]
}

/// A colour as four normalised bytes.
fn pack_unorm8(c: [f32; 4]) -> [u8; 4] {
    let q = |x: f32| (x.clamp(0.0, 1.0) * 255.0).round() as u8;
    [q(c[0]), q(c[1]), q(c[2]), q(c[3])]
}

/// The index buffer for a mesh of `vertex_count` vertices: 16-bit wherever that
/// many vertices can be addressed with two bytes, which halves the index bytes
/// of every small mesh (street detail, windows, most surface groups).
#[must_use]
pub fn indices_for(vertex_count: usize, indices: Vec<u32>) -> Indices {
    if vertex_count <= usize::from(u16::MAX) + 1 {
        Indices::U16(indices.into_iter().map(|i| i as u16).collect())
    } else {
        Indices::U32(indices)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use bevy::mesh::{Mesh, MeshVertexAttributeId};

    /// The whole scheme rests on these ids being bevy's: a compact attribute on
    /// a different id is an attribute no pipeline asks for, and the mesh would
    /// draw unlit and uncoloured (or not at all, where a material requires it).
    #[test]
    fn compact_attributes_answer_to_bevys_ids() {
        assert_eq!(
            MeshVertexAttributeId::from(COMPACT_NORMAL),
            MeshVertexAttributeId::from(Mesh::ATTRIBUTE_NORMAL)
        );
        assert_eq!(
            MeshVertexAttributeId::from(COMPACT_COLOR),
            MeshVertexAttributeId::from(Mesh::ATTRIBUTE_COLOR)
        );
        assert_eq!(COMPACT_NORMAL.format.size() as usize, COMPACT_NORMAL_BYTES);
        assert_eq!(COMPACT_COLOR.format.size() as usize, COMPACT_COLOR_BYTES);
    }

    /// Axis-aligned normals — every wall, roof and flat street — survive
    /// exactly, so a shader test like `abs(n.y) < WALL_NORMAL_Y` answers as it
    /// did on floats; anything else is within half a degree.
    #[test]
    fn normals_round_trip_within_half_a_degree() {
        for n in [[0.0, 1.0, 0.0], [1.0, 0.0, 0.0], [0.0, 0.0, -1.0]] {
            let p = pack_snorm8(n);
            assert_eq!([p[0], p[1], p[2]], n.map(|x: f32| (x * 127.0) as i8));
        }
        for i in 0..200 {
            let a = i as f32 * 0.137;
            let b = i as f32 * 0.071;
            let n =
                bevy::math::Vec3::new(a.cos() * b.sin(), b.cos(), a.sin() * b.sin()).normalize();
            let p = pack_snorm8(n.to_array());
            let back = bevy::math::Vec3::new(
                f32::from(p[0]) / 127.0,
                f32::from(p[1]) / 127.0,
                f32::from(p[2]) / 127.0,
            )
            .normalize();
            assert!(n.angle_between(back).to_degrees() < 0.5, "{n} → {back}");
        }
    }

    #[test]
    fn small_meshes_get_sixteen_bit_indices_and_large_ones_do_not() {
        assert!(matches!(indices_for(65_536, vec![0, 65_535]), Indices::U16(v) if v[1] == 65_535));
        assert!(matches!(
            indices_for(65_537, vec![0, 65_536]),
            Indices::U32(_)
        ));
    }
}
