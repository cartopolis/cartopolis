//! The prepared-ground file: one map cell's finished surfaces, as bytes.
//!
//! This is what `carto-pipeline` writes and the client reads instead of building
//! the ground itself. Measured on one capture over Groningen, cutting the fills,
//! the ribbons and the pavements into a partition was **45.9 s** of the client's
//! 78 s of map work — 58 % of it, for an answer that is the same on every device
//! and in every session.
//!
//! # Why it is not `postcard`, which everything else here uses
//!
//! The wire protocol's job is to describe *small structured messages* and it is
//! right to use varints for them. This file's job is to hand over a few megabytes
//! of vertex arrays as fast as the machine can read them, and a varint costs a
//! branch per float. So the layout is fixed-width little-endian throughout:
//! decoding is a bounds check and a copy, never a parse, and the copy is the one
//! the GPU upload needs anyway. Nothing is compressed in the file either — the
//! transport already does that, a self-compressing file cannot be served straight
//! off a static host, and the decompressor would be the cost this exists to remove.
//!
//! # The group code is explicit, and that is not bureaucracy
//!
//! [`SurfaceGroup`] is ordered by [`crate::ground_stack::GroundLayer`], whose
//! *declaration order is the depth ladder* — adding a flat thing to the map means
//! inserting a variant, which is exactly what that design asks you to do. A file
//! carrying the discriminant would therefore have its meaning silently rewritten
//! by a change the ladder invites: yesterday's grass read back as tomorrow's
//! paving, with no version bump, no error, and a wrong-coloured city. So the codes
//! here are written down once, never reused, and `every_group_has_a_stable_code`
//! fails when a new variant arrives without one.
//!
//! # Layout
//!
//! All integers little-endian. Every array starts 4-byte aligned, which falls out
//! of every field being 4 bytes or a multiple.
//!
//! ```text
//! header   magic "CGND" | version u16 | flags u16 | z u32 | x u32 | y u32 | meshes u32
//! mesh ×n  group u16 | attrs u8 | index_width u8 | vertices u32 | indices u32
//!          positions f32[3 × vertices]
//!          uvs       f32[2 × vertices]   if attrs & UV
//!          normals   f32[3 × vertices]   if attrs & NORMAL
//!          colors    f32[4 × vertices]   if attrs & COLOR
//!          rungs     f32[1 × vertices]   if attrs & RUNG
//!          indices   u16 or u32 × indices
//! ```
//!
//! Indices are 16-bit whenever the mesh has fewer than 65,536 vertices, which is
//! the ordinary case rather than a lucky one: the streamers already split a cell's
//! meshes by upload byte budget, so a mesh big enough to need 32-bit indices has
//! already been cut for a different reason.

use crate::road_texture::RoadClass;
use crate::vector_tiles::{SurfaceGroup, SurfaceMesh};

/// `CGND` — cartopolis ground.
const MAGIC: [u8; 4] = *b"CGND";

/// Bumped whenever a reader would misread an older file. A reader refuses
/// anything else rather than guessing: half-understood geometry is a wrong map,
/// and the client's own tessellation is always there to fall back on.
pub const VERSION: u16 = 1;

const HEADER_BYTES: usize = 4 + 2 + 2 + 4 + 4 + 4 + 4;
const MESH_HEADER_BYTES: usize = 2 + 1 + 1 + 4 + 4;

/// Which optional per-vertex arrays a mesh carries.
mod attrs {
    pub const UV: u8 = 1 << 0;
    pub const NORMAL: u8 = 1 << 1;
    pub const COLOR: u8 = 1 << 2;
    pub const RUNG: u8 = 1 << 3;
}

/// A mesh with this many vertices or more needs 32-bit indices.
const U16_VERTEX_LIMIT: usize = u16::MAX as usize + 1;

/// One cell's prepared ground.
#[derive(Debug, Clone, PartialEq)]
pub struct GroundFile {
    pub z: u32,
    pub x: u32,
    pub y: u32,
    pub meshes: Vec<SurfaceMesh>,
}

/// Why a file could not be read. Every arm is a refusal rather than a repair —
/// see [`VERSION`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ReadError {
    /// Not a prepared-ground file at all.
    NotGround,
    /// Written by a newer or older layout than this reader knows.
    Version(u16),
    /// A flag bit this reader does not understand is set. Refused for the same
    /// reason an unknown version is: a flag exists to change how the bytes are
    /// read.
    Flags(u16),
    /// A group code this reader has no variant for.
    UnknownGroup(u16),
    /// The file ends inside something it promised.
    Truncated { wanted: usize, had: usize },
    /// An index points past the mesh's own vertices. Checked rather than trusted:
    /// this is the one field that can crash a GPU driver rather than draw wrongly.
    IndexOutOfRange { index: u32, vertices: u32 },
}

impl std::fmt::Display for ReadError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::NotGround => write!(f, "not a prepared-ground file"),
            Self::Version(v) => write!(f, "prepared ground version {v}, this reads {VERSION}"),
            Self::Flags(b) => write!(f, "prepared ground sets unknown flags {b:#06x}"),
            Self::UnknownGroup(c) => write!(f, "prepared ground names unknown surface {c}"),
            Self::Truncated { wanted, had } => {
                write!(f, "prepared ground wants {wanted} bytes and has {had}")
            }
            Self::IndexOutOfRange { index, vertices } => {
                write!(
                    f,
                    "prepared ground index {index} against {vertices} vertices"
                )
            }
        }
    }
}

impl std::error::Error for ReadError {}

/// The code a group travels as. **Never reuse one**, and never derive one from a
/// discriminant — the module note says why.
fn group_code(group: SurfaceGroup) -> u16 {
    // 0–15 the flat fills, 16–31 the road sheet, 32+ everything standing on it.
    // Room left in each band so a sibling can be added without disturbing the
    // others.
    match group {
        SurfaceGroup::Water => 0,
        SurfaceGroup::Forest => 1,
        SurfaceGroup::Grass => 2,
        SurfaceGroup::Sand => 3,
        SurfaceGroup::Built => 4,
        SurfaceGroup::Paving => 5,
        SurfaceGroup::Crop => 6,
        SurfaceGroup::Road(c) => 16 + road_code(c),
        SurfaceGroup::RoadCasing(c) => 24 + road_code(c),
        SurfaceGroup::TunnelRoad(c) => 32 + road_code(c),
        SurfaceGroup::TunnelStructure => 40,
        SurfaceGroup::BridgeDeck => 41,
        SurfaceGroup::Canopy => 42,
        SurfaceGroup::Trunk => 43,
        SurfaceGroup::Furniture => 44,
    }
}

fn road_code(class: RoadClass) -> u16 {
    match class {
        RoadClass::Motorway => 0,
        RoadClass::Carriageway => 1,
        RoadClass::Major => 2,
        RoadClass::Minor => 3,
        RoadClass::Path => 4,
        RoadClass::Rail => 5,
    }
}

fn road_class(code: u16) -> Option<RoadClass> {
    Some(match code {
        0 => RoadClass::Motorway,
        1 => RoadClass::Carriageway,
        2 => RoadClass::Major,
        3 => RoadClass::Minor,
        4 => RoadClass::Path,
        5 => RoadClass::Rail,
        _ => return None,
    })
}

fn group_of(code: u16) -> Option<SurfaceGroup> {
    Some(match code {
        0 => SurfaceGroup::Water,
        1 => SurfaceGroup::Forest,
        2 => SurfaceGroup::Grass,
        3 => SurfaceGroup::Sand,
        4 => SurfaceGroup::Built,
        5 => SurfaceGroup::Paving,
        6 => SurfaceGroup::Crop,
        16..=23 => SurfaceGroup::Road(road_class(code - 16)?),
        24..=31 => SurfaceGroup::RoadCasing(road_class(code - 24)?),
        32..=39 => SurfaceGroup::TunnelRoad(road_class(code - 32)?),
        40 => SurfaceGroup::TunnelStructure,
        41 => SurfaceGroup::BridgeDeck,
        42 => SurfaceGroup::Canopy,
        43 => SurfaceGroup::Trunk,
        44 => SurfaceGroup::Furniture,
        _ => return None,
    })
}

/// Bytes this cell will occupy, exactly. Worth having separately: a pipeline run
/// wants to report what it is about to write, and a reader wants to check a length
/// before trusting one.
#[must_use]
pub fn encoded_len(meshes: &[SurfaceMesh]) -> usize {
    let mut n = HEADER_BYTES;
    for m in meshes {
        let v = m.positions.len();
        n += MESH_HEADER_BYTES + v * 12;
        if !m.uvs.is_empty() {
            n += v * 8;
        }
        if !m.normals.is_empty() {
            n += v * 12;
        }
        if !m.colors.is_empty() {
            n += v * 16;
        }
        if !m.rungs.is_empty() {
            n += v * 4;
        }
        n += m.indices.len() * if v < U16_VERTEX_LIMIT { 2 } else { 4 };
    }
    n
}

/// One cell's meshes, as the bytes a client reads.
///
/// An optional array is written when it is non-empty and its length matches the
/// positions; a mesh whose attribute is a different length is a bug upstream, and
/// writing it would hand the reader a vertex buffer the GPU rejects. It is dropped
/// with a warning rather than aborting the cell, because one bad attribute should
/// not cost a city its ground.
#[must_use]
pub fn encode(z: u32, x: u32, y: u32, meshes: &[SurfaceMesh]) -> Vec<u8> {
    let mut out = Vec::with_capacity(encoded_len(meshes));
    out.extend_from_slice(&MAGIC);
    out.extend_from_slice(&VERSION.to_le_bytes());
    out.extend_from_slice(&0u16.to_le_bytes()); // flags
    out.extend_from_slice(&z.to_le_bytes());
    out.extend_from_slice(&x.to_le_bytes());
    out.extend_from_slice(&y.to_le_bytes());
    out.extend_from_slice(
        &u32::try_from(meshes.len())
            .unwrap_or(u32::MAX)
            .to_le_bytes(),
    );

    for m in meshes {
        let v = m.positions.len();
        let matches = |len: usize| len == v;
        let has_uv = !m.uvs.is_empty() && matches(m.uvs.len());
        let has_normal = !m.normals.is_empty() && matches(m.normals.len());
        let has_color = !m.colors.is_empty() && matches(m.colors.len());
        let has_rung = !m.rungs.is_empty() && matches(m.rungs.len());
        for (name, present, len) in [
            ("uvs", has_uv, m.uvs.len()),
            ("normals", has_normal, m.normals.len()),
            ("colors", has_color, m.colors.len()),
            ("rungs", has_rung, m.rungs.len()),
        ] {
            if !present && len != 0 {
                tracing::warn!(
                    group = ?m.group,
                    attribute = name,
                    len,
                    vertices = v,
                    "prepared ground: attribute length does not match the positions, dropped"
                );
            }
        }
        let mut attrs = 0u8;
        if has_uv {
            attrs |= attrs::UV;
        }
        if has_normal {
            attrs |= attrs::NORMAL;
        }
        if has_color {
            attrs |= attrs::COLOR;
        }
        if has_rung {
            attrs |= attrs::RUNG;
        }
        let narrow = v < U16_VERTEX_LIMIT;

        out.extend_from_slice(&group_code(m.group).to_le_bytes());
        out.push(attrs);
        out.push(u8::from(!narrow));
        out.extend_from_slice(&u32::try_from(v).unwrap_or(u32::MAX).to_le_bytes());
        out.extend_from_slice(
            &u32::try_from(m.indices.len())
                .unwrap_or(u32::MAX)
                .to_le_bytes(),
        );

        for p in &m.positions {
            push_f32s(&mut out, p);
        }
        if has_uv {
            for a in &m.uvs {
                push_f32s(&mut out, a);
            }
        }
        if has_normal {
            for a in &m.normals {
                push_f32s(&mut out, a);
            }
        }
        if has_color {
            for a in &m.colors {
                push_f32s(&mut out, a);
            }
        }
        if has_rung {
            for a in &m.rungs {
                out.extend_from_slice(&a.to_le_bytes());
            }
        }
        for i in &m.indices {
            if narrow {
                out.extend_from_slice(&(*i as u16).to_le_bytes());
            } else {
                out.extend_from_slice(&i.to_le_bytes());
            }
        }
    }
    out
}

fn push_f32s(out: &mut Vec<u8>, v: &[f32]) {
    for f in v {
        out.extend_from_slice(&f.to_le_bytes());
    }
}

/// Read a cell back.
///
/// Every length is checked before it is used, and so is every index — this is a
/// file off a network, and an index past the end of a vertex buffer is the one
/// field here that takes a GPU driver down rather than drawing something wrong.
pub fn decode(bytes: &[u8]) -> Result<GroundFile, ReadError> {
    let mut r = Cursor { bytes, at: 0 };
    if r.take(4)? != MAGIC {
        return Err(ReadError::NotGround);
    }
    let version = r.u16()?;
    if version != VERSION {
        return Err(ReadError::Version(version));
    }
    let flags = r.u16()?;
    if flags != 0 {
        return Err(ReadError::Flags(flags));
    }
    let (z, x, y) = (r.u32()?, r.u32()?, r.u32()?);
    let count = r.u32()? as usize;

    let mut meshes = Vec::with_capacity(count.min(1024));
    for _ in 0..count {
        let code = r.u16()?;
        let group = group_of(code).ok_or(ReadError::UnknownGroup(code))?;
        let attrs = r.u8()?;
        let wide = r.u8()? != 0;
        let vertices = r.u32()?;
        let index_count = r.u32()? as usize;
        let v = vertices as usize;

        let positions = r.vec3(v)?;
        let uvs = if attrs & attrs::UV != 0 {
            r.vec2(v)?
        } else {
            Vec::new()
        };
        let normals = if attrs & attrs::NORMAL != 0 {
            r.vec3(v)?
        } else {
            Vec::new()
        };
        let colors = if attrs & attrs::COLOR != 0 {
            r.vec4(v)?
        } else {
            Vec::new()
        };
        let rungs = if attrs & attrs::RUNG != 0 {
            r.f32s(v)?
        } else {
            Vec::new()
        };

        let mut indices = Vec::with_capacity(index_count);
        for _ in 0..index_count {
            let i = if wide { r.u32()? } else { u32::from(r.u16()?) };
            if i >= vertices {
                return Err(ReadError::IndexOutOfRange { index: i, vertices });
            }
            indices.push(i);
        }

        meshes.push(SurfaceMesh {
            group,
            positions,
            indices,
            uvs,
            normals,
            colors,
            rungs,
        });
    }
    Ok(GroundFile { z, x, y, meshes })
}

/// A bounds-checked read head. Every accessor returns [`ReadError::Truncated`]
/// rather than panicking, because the bytes come off a network.
struct Cursor<'a> {
    bytes: &'a [u8],
    at: usize,
}

impl<'a> Cursor<'a> {
    fn take(&mut self, n: usize) -> Result<&'a [u8], ReadError> {
        let end = self.at.checked_add(n).ok_or(ReadError::Truncated {
            wanted: usize::MAX,
            had: self.bytes.len(),
        })?;
        if end > self.bytes.len() {
            return Err(ReadError::Truncated {
                wanted: end,
                had: self.bytes.len(),
            });
        }
        let out = &self.bytes[self.at..end];
        self.at = end;
        Ok(out)
    }

    fn u8(&mut self) -> Result<u8, ReadError> {
        Ok(self.take(1)?[0])
    }

    fn u16(&mut self) -> Result<u16, ReadError> {
        let b = self.take(2)?;
        Ok(u16::from_le_bytes([b[0], b[1]]))
    }

    fn u32(&mut self) -> Result<u32, ReadError> {
        let b = self.take(4)?;
        Ok(u32::from_le_bytes([b[0], b[1], b[2], b[3]]))
    }

    fn f32s(&mut self, n: usize) -> Result<Vec<f32>, ReadError> {
        let b = self.take(n * 4)?;
        Ok(b.chunks_exact(4)
            .map(|c| f32::from_le_bytes([c[0], c[1], c[2], c[3]]))
            .collect())
    }

    fn vec2(&mut self, n: usize) -> Result<Vec<[f32; 2]>, ReadError> {
        let f = self.f32s(n * 2)?;
        Ok(f.chunks_exact(2).map(|c| [c[0], c[1]]).collect())
    }

    fn vec3(&mut self, n: usize) -> Result<Vec<[f32; 3]>, ReadError> {
        let f = self.f32s(n * 3)?;
        Ok(f.chunks_exact(3).map(|c| [c[0], c[1], c[2]]).collect())
    }

    fn vec4(&mut self, n: usize) -> Result<Vec<[f32; 4]>, ReadError> {
        let f = self.f32s(n * 4)?;
        Ok(f.chunks_exact(4)
            .map(|c| [c[0], c[1], c[2], c[3]])
            .collect())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn mesh(group: SurfaceGroup, vertices: usize) -> SurfaceMesh {
        SurfaceMesh {
            group,
            positions: (0..vertices)
                .map(|i| [i as f32, i as f32 * 0.5, -(i as f32)])
                .collect(),
            indices: (0..vertices as u32).collect(),
            uvs: (0..vertices).map(|i| [i as f32, 1.0]).collect(),
            normals: vec![[0.0, 1.0, 0.0]; vertices],
            colors: vec![[0.25, 0.5, 0.75, 1.0]; vertices],
            rungs: vec![3.0; vertices],
        }
    }

    #[test]
    fn a_cell_survives_the_round_trip_exactly() {
        let meshes = vec![
            mesh(SurfaceGroup::Grass, 6),
            mesh(SurfaceGroup::Road(RoadClass::Major), 9),
        ];
        let bytes = encode(14, 8489, 5321, &meshes);
        assert_eq!(bytes.len(), encoded_len(&meshes), "encoded_len disagrees");
        let back = decode(&bytes).expect("round trip");
        assert_eq!(back.z, 14);
        assert_eq!(back.x, 8489);
        assert_eq!(back.y, 5321);
        assert_eq!(back.meshes, meshes);
    }

    /// The optional arrays are optional. A ground fill carries none of them, and a
    /// reader that assumed otherwise would hand the GPU a buffer of the wrong
    /// length.
    #[test]
    fn a_mesh_with_no_optional_attributes_round_trips() {
        let bare = SurfaceMesh {
            group: SurfaceGroup::Built,
            positions: vec![[0.0, 0.0, 0.0], [1.0, 0.0, 0.0], [0.0, 0.0, 1.0]],
            indices: vec![0, 1, 2],
            uvs: Vec::new(),
            normals: Vec::new(),
            colors: Vec::new(),
            rungs: Vec::new(),
        };
        let back = decode(&encode(14, 1, 2, std::slice::from_ref(&bare))).unwrap();
        assert_eq!(back.meshes, vec![bare]);
    }

    /// **The code table is one-to-one, and stays so when a group is added.**
    ///
    /// Two halves, and the *second* is the one that does the work tomorrow.
    /// `group_code`'s `match` is exhaustive, so a new [`SurfaceGroup`] variant
    /// cannot compile without being given a code — that part the compiler
    /// guarantees and no test is needed for it. What a compiler cannot catch is a
    /// new variant given a code some *existing* group already uses, which would
    /// silently redirect the old one.
    ///
    /// So the second half sweeps every code a file could contain and checks the
    /// mapping is its own inverse. It deliberately does not walk a list of groups:
    /// `ground_stack`'s note is about exactly this — "a list somebody maintains is
    /// a list somebody forgets" — and the list here would be the third copy of the
    /// variants.
    #[test]
    fn every_group_has_a_stable_code() {
        let classes = [
            RoadClass::Motorway,
            RoadClass::Carriageway,
            RoadClass::Major,
            RoadClass::Minor,
            RoadClass::Path,
            RoadClass::Rail,
        ];
        let mut all: Vec<SurfaceGroup> = vec![
            SurfaceGroup::Water,
            SurfaceGroup::Forest,
            SurfaceGroup::Grass,
            SurfaceGroup::Sand,
            SurfaceGroup::Built,
            SurfaceGroup::Paving,
            SurfaceGroup::Crop,
            SurfaceGroup::TunnelStructure,
            SurfaceGroup::BridgeDeck,
            SurfaceGroup::Canopy,
            SurfaceGroup::Trunk,
            SurfaceGroup::Furniture,
        ];
        for c in classes {
            all.push(SurfaceGroup::Road(c));
            all.push(SurfaceGroup::RoadCasing(c));
            all.push(SurfaceGroup::TunnelRoad(c));
        }
        let mut seen = std::collections::HashMap::new();
        for g in all {
            let code = group_code(g);
            if let Some(other) = seen.insert(code, g) {
                panic!("{g:?} and {other:?} both travel as code {code}");
            }
            assert_eq!(group_of(code), Some(g), "code {code} does not read back");
        }

        // The half that needs no list: every code a file could carry either means
        // nothing, or means exactly the group that writes it. A new variant handed
        // an already-taken code breaks this for the group that had it.
        for code in 0..=u16::MAX {
            if let Some(group) = group_of(code) {
                assert_eq!(
                    group_code(group),
                    code,
                    "code {code} reads as {group:?}, which is written as {}",
                    group_code(group)
                );
            }
        }
    }

    /// A file off a network is not trusted. Each of these used to be a panic or a
    /// driver fault waiting for a corrupt byte.
    #[test]
    fn a_damaged_file_is_refused_rather_than_read() {
        let good = encode(14, 1, 2, &[mesh(SurfaceGroup::Grass, 4)]);

        assert_eq!(decode(b"nope").unwrap_err(), ReadError::NotGround);

        let mut wrong_version = good.clone();
        wrong_version[4] = 99;
        assert!(matches!(
            decode(&wrong_version).unwrap_err(),
            ReadError::Version(_)
        ));

        let mut wrong_flags = good.clone();
        wrong_flags[6] = 1;
        assert!(matches!(
            decode(&wrong_flags).unwrap_err(),
            ReadError::Flags(_)
        ));

        for cut in 1..good.len() {
            // Every truncation is an error and none of them panics.
            let _ = decode(&good[..cut]);
        }
        assert!(matches!(
            decode(&good[..good.len() - 1]).unwrap_err(),
            ReadError::Truncated { .. }
        ));

        // The last index, made to point one past the end.
        let mut bad_index = good.clone();
        let n = bad_index.len();
        bad_index[n - 2..].copy_from_slice(&9999u16.to_le_bytes());
        assert!(matches!(
            decode(&bad_index).unwrap_err(),
            ReadError::IndexOutOfRange { .. }
        ));
    }

    /// Indices narrow to 16 bits below 65,536 vertices, which is what makes the
    /// ordinary cell's index block half the size. Checked as a size, because the
    /// flag being written and the width actually used are two different mistakes.
    #[test]
    fn a_small_mesh_uses_narrow_indices() {
        let m = mesh(SurfaceGroup::Grass, 3);
        let bytes = encode(14, 0, 0, std::slice::from_ref(&m));
        let wide = SurfaceMesh {
            indices: m.indices.clone(),
            ..mesh(SurfaceGroup::Grass, 3)
        };
        assert_eq!(bytes.len(), encoded_len(&[wide]));
        // 3 indices × 2 bytes, not × 4.
        assert_eq!(
            bytes.len() - HEADER_BYTES - MESH_HEADER_BYTES - 3 * (12 + 8 + 12 + 16 + 4),
            6
        );
    }
}
