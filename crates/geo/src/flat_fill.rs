//! Triangulating a flat, ground-lying polygon layer into byte-bounded meshes.
//!
//! Two layers do exactly this — surveyed paving ([`crate::paving`]) and crop
//! parcels ([`crate::crops`]) — and they differ only in which group the mesh
//! lands in and how a polygon's tone is chosen. Everything else is shared and had
//! to stay shared: the winding fix, the byte cap and the sRGB→linear vertex tint
//! are each a bug this codebase has already had once, and two copies of them is
//! two places for the next one to be fixed in.

use crate::vector_tiles::{SurfaceGroup, SurfaceMesh, INDEX_BYTES, VERTEX_BYTES};

/// One polygon to fill: outer ring first, holes after, open rings (no repeated
/// closing vertex), in the cell's local metre frame — plus the sRGB tone it is
/// drawn in, which rides in the vertex colours.
pub(crate) struct FlatFill<'a> {
    pub rings: &'a [Vec<[f32; 2]>],
    pub rgb: u32,
}

/// What one triangle costs to upload, in the units the frame budget is in.
pub(crate) const TRI_BYTES: usize = 3 * VERTEX_BYTES + 3 * INDEX_BYTES;

/// Triangulate a cell's fills into merged meshes of `group`, each no larger than
/// `max_bytes`.
///
/// One group for all of it, like the furniture and the canopies: the per-polygon
/// tone rides in the vertex colour, so a whole cell is one material and a handful
/// of draw calls.
pub(crate) fn build_flat_fills(
    fills: &[FlatFill<'_>],
    group: SurfaceGroup,
    max_bytes: Option<usize>,
) -> Vec<SurfaceMesh> {
    if fills.is_empty() {
        return Vec::new();
    }
    let mut out = Vec::new();
    let mut buf = Buf::new(group);
    for fill in fills {
        let Some(outer) = fill.rings.first() else {
            continue;
        };
        if outer.len() < 3 {
            continue;
        }
        // earcut takes one flat coordinate list with the holes appended and their
        // start indices given separately.
        let mut flat: Vec<f64> = Vec::with_capacity(outer.len() * 2);
        for p in outer {
            flat.push(f64::from(p[0]));
            flat.push(f64::from(p[1]));
        }
        let mut holes = Vec::new();
        for ring in &fill.rings[1..] {
            if ring.len() < 3 {
                continue;
            }
            holes.push(flat.len() / 2);
            for p in ring {
                flat.push(f64::from(p[0]));
                flat.push(f64::from(p[1]));
            }
        }
        let Ok(tris) = earcutr::earcut(&flat, &holes, 2) else {
            continue;
        };
        if let Some(max) = max_bytes {
            let step = tris.len() / 3 * TRI_BYTES;
            if buf.bytes() + step > max && !buf.positions.is_empty() {
                out.push(std::mem::replace(&mut buf, Buf::new(group)).finish());
            }
        }
        let tint = srgb_to_linear(fill.rgb);
        // Unshared vertices per triangle. A shared-vertex mesh would be smaller,
        // but every surface here is flat and faces up, so there is no shading to
        // gain from sharing and the winding fix below needs to own its corners.
        for t in tris.chunks_exact(3) {
            let p = |i: usize| -> [f32; 3] { [flat[i * 2] as f32, 0.0, flat[i * 2 + 1] as f32] };
            let (a, b, c) = (p(t[0]), p(t[1]), p(t[2]));
            // Earcut keeps the input ring's orientation, and no register promises
            // one. A ground surface always faces up, so wind for +Y — the same
            // rule `tessellate_polygon` follows, and the same failure if it is
            // skipped: half the layer is invisible from above and visible from
            // below.
            let cross = (b[0] - a[0]) * (c[2] - a[2]) - (b[2] - a[2]) * (c[0] - a[0]);
            let base = buf.positions.len() as u32;
            if cross < 0.0 {
                buf.push(a, tint);
                buf.push(b, tint);
                buf.push(c, tint);
            } else {
                buf.push(a, tint);
                buf.push(c, tint);
                buf.push(b, tint);
            }
            buf.indices.extend([base, base + 1, base + 2]);
        }
    }
    if !buf.positions.is_empty() {
        out.push(buf.finish());
    }
    out
}

struct Buf {
    group: SurfaceGroup,
    positions: Vec<[f32; 3]>,
    normals: Vec<[f32; 3]>,
    colors: Vec<[f32; 4]>,
    indices: Vec<u32>,
}

impl Buf {
    fn new(group: SurfaceGroup) -> Self {
        Self {
            group,
            positions: Vec::new(),
            normals: Vec::new(),
            colors: Vec::new(),
            indices: Vec::new(),
        }
    }

    fn push(&mut self, p: [f32; 3], tint: [f32; 4]) {
        self.positions.push(p);
        self.normals.push([0.0, 1.0, 0.0]);
        self.colors.push(tint);
    }

    fn bytes(&self) -> usize {
        self.positions.len() * VERTEX_BYTES + self.indices.len() * INDEX_BYTES
    }

    fn finish(self) -> SurfaceMesh {
        SurfaceMesh {
            group: self.group,
            positions: self.positions,
            indices: self.indices,
            uvs: Vec::new(),
            normals: self.normals,
            colors: self.colors,
            rungs: Vec::new(),
        }
    }
}

/// One sRGB colour as a linear vertex tint.
///
/// Linear because that is where the shader multiplies it into the group's white
/// base colour — the same exit every other per-vertex palette here takes.
pub(crate) fn srgb_to_linear(rgb: u32) -> [f32; 4] {
    let c = |shift: u32| {
        let v = f32::from((rgb >> shift & 0xFF) as u8) / 255.0;
        if v <= 0.04045 {
            v / 12.92
        } else {
            ((v + 0.055) / 1.055).powf(2.4)
        }
    };
    [c(16), c(8), c(0), 1.0]
}
