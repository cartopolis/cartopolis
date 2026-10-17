//! Near-field grass — the pure half: where a blade may stand, and the one mesh
//! every patch of them draws.
//!
//! The ECS half (the patch pool, the mask upload, the wind and the altitude ramp)
//! is `systems::grass`, and `assets/shaders/grass.wesl` is where a blade actually
//! becomes geometry.
//!
//! # Why this layer looks nothing like [`crate::scatter`]
//!
//! Trees are scattered on the CPU, merged into one mesh per cell, and uploaded.
//! That works because a z14 cell holds at most [`MAX_TREES_PER_CELL`] of them and
//! a tree is visible from two kilometres up. Grass is the opposite on both counts:
//! it is only *ever* legible within a few tens of metres, and to read as grass it
//! needs ten blades per square metre — two orders of magnitude more instances over
//! four orders less ground. Scattered and merged the same way, a 25 m disc of lawn
//! is ~20,000 blades ≈ 6 MB of vertex buffer that has to be **rebuilt and
//! re-uploaded as the camera walks**, which is the mass-churn pattern that loses
//! the device (see `docs/notes/upload-bursts.md`). There is no per-frame cap that
//! makes continuous churn free; it only spreads it out.
//!
//! So no blade is ever stored. What is stored is:
//!
//! * **one lattice mesh** ([`blade_lattice`]) — a patch's worth of blade *slots*,
//!   each five vertices carrying nothing but which slot and which corner they are.
//!   Every patch in the world draws this same handle, so it is uploaded once, at
//!   startup, and never again;
//! * **one mask** ([`mask_field`]) — a small moving raster around the camera whose
//!   red channel says "grass may grow here" and whose green channel says how high
//!   the ground is. It is rebuilt when the camera walks out of it, which at the
//!   sizes `systems::grass` uses is every twenty-odd metres.
//!
//! The vertex shader turns a slot into a blade: it hashes the slot's position in
//! the anchor-metre frame into a jitter, a yaw, a height, a lean and a tone,
//! samples the mask to decide whether the blade exists at all and what it stands
//! on, bends it in the wind, and shrinks it back into the ground with distance.
//! Because the hash input is a *world* position rather than a vertex index, the
//! same blade comes back identically every frame no matter which patch entity is
//! standing over it — which is what lets the patches be a fixed pool that slides
//! along a lattice under the camera instead of a stream that spawns and despawns.
//!
//! [`MAX_TREES_PER_CELL`]: crate::scatter::MAX_TREES_PER_CELL

use std::collections::HashMap;

use crate::crops::CropParcel;
use crate::scatter::{bucket_range, in_triangle, RoadMask, WaterMask};
use crate::vector_tiles::{kind_of, land_group, tessellate_polygon, SurfaceGroup, TileData};

/// Vertices in one blade. Two segments, so a blade is a tapered strip that can
/// bend once — the shape everyone converges on, and the smallest one that does
/// not read as a flat shard when it leans.
///
/// The count matters here rather than in the shader because the index buffer is
/// built against it and the shader reads a vertex's *role* (0 = base left, 1 =
/// base right, 2 = mid left, 3 = mid right, 4 = tip) out of an attribute this
/// module writes.
pub const BLADE_VERTS: u32 = 5;

/// Triangles in one blade: the two-quad strip is 2 + 1 (the tip is a single
/// triangle, which is the whole point of tapering).
pub const BLADE_TRIS: u32 = 3;

/// How far a blade keeps off a carriageway or a bank, in metres.
///
/// Deliberately much smaller than the canopy clearance [`crate::scatter`] uses: a
/// tree must not *overhang* a quay, while grass growing right up to the kerb is
/// what a kerb looks like. It is not zero because the road ribbon and the landuse
/// polygon are independent generalisations of the same ground and disagree by a
/// few tens of centimetres in places.
const BLADE_CLEARANCE_M: f32 = 0.15;

/// One streamed cell's answer to "may a blade stand here?".
///
/// Built on the tile worker with the rest of the cell (`map_geometry`), and held
/// for as long as the cell is live. This is *not* a raster: the near-field mask is
/// rebuilt from it a few times a minute over a moving 100 m window, and rasterising
/// a whole 1.5 km cell to the resolution that window wants would cost more memory
/// per cell than the entire rest of the layer.
///
/// The keep-outs are the same two [`crate::scatter`] plants trees against, and for
/// the same reason: Shortbread's `land` polygons are *not* cut where a path or a
/// canal runs through them — the road and water layers are simply drawn on top —
/// so a park's own boundary says nothing about the path across it.
///
/// The third is the crop register ([`crate::crops`]), and it fixes something the
/// first two could not: `farmland` is one of the OSM kinds [`land_group`] calls
/// [`SurfaceGroup::Grass`], so every arable field in the country grew a lawn.
/// Nothing in the tile says otherwise — a potato field and a meadow are the same
/// polygon there — which is exactly why the answer had to come from a register.
pub struct GrassArea {
    /// Grass-group triangles in the cell's tile-local metre frame.
    tris: Vec<[(f32, f32); 3]>,
    /// Uniform grid over `tris`, so a point test is a bucket lookup and not a scan
    /// over every triangle in the cell.
    buckets: HashMap<(i32, i32), Vec<u32>>,
    water: WaterMask,
    roads: RoadMask,
    arable: ArableMask,
    /// The tunnel openings this cell's ground has been taken away over
    /// ([`crate::tunnels::cut_ground`]), each with its bounding box.
    ///
    /// **A fourth keep-out, for the reason the other three are keep-outs.** The
    /// blades are not placed on the ground mesh — they are generated from this
    /// cell's grass polygons — so cutting a hole in the ground does nothing to
    /// them, and a park whose tile says "grass" across the whole block grows
    /// straight over an open cut. Seen in the 2026-09-16 capture over the
    /// Groningen ring: a lawn lying across the trench with the carriageway under
    /// it. The trees were already dropped this way (`map_geometry`,
    /// `tunnels::in_an_opening`); this is the same rule for the layer that has no
    /// instances to drop.
    openings: Vec<([f32; 4], Vec<[f32; 2]>)>,
}

/// Where the crop register says the ground is worked rather than grazed.
///
/// A bucketed triangle soup like the masks in [`crate::scatter`], and bucketed for
/// the same reason: the near-field mask is re-rasterised every twenty-odd metres
/// of walking over 16k texels, and a linear scan over a cell's few thousand parcel
/// triangles per texel is not a thing to do on a worker.
#[derive(Default)]
struct ArableMask {
    tris: Vec<[(f32, f32); 3]>,
    buckets: HashMap<(i32, i32), Vec<u32>>,
}

impl ArableMask {
    /// Matches [`GrassArea::CELL`]: the same kind of soup asked the same question.
    const CELL: f32 = 16.0;

    fn build(parcels: &[CropParcel], tile_m: f32) -> Self {
        let mut mask = Self::default();
        for tri in crate::crops::arable_triangles(parcels) {
            let idx = mask.tris.len() as u32;
            mask.tris.push(tri);
            let (minx, maxx) = (
                tri[0].0.min(tri[1].0).min(tri[2].0),
                tri[0].0.max(tri[1].0).max(tri[2].0),
            );
            let (minz, maxz) = (
                tri[0].1.min(tri[1].1).min(tri[2].1),
                tri[0].1.max(tri[1].1).max(tri[2].1),
            );
            for i in bucket_range(minx, maxx, Self::CELL, tile_m) {
                for j in bucket_range(minz, maxz, Self::CELL, tile_m) {
                    mask.buckets.entry((i, j)).or_default().push(idx);
                }
            }
        }
        mask
    }

    fn blocks(&self, x: f32, z: f32) -> bool {
        let key = (
            (x / Self::CELL).floor() as i32,
            (z / Self::CELL).floor() as i32,
        );
        let Some(ids) = self.buckets.get(&key) else {
            return false;
        };
        ids.iter().any(|&id| {
            let t = self.tris[id as usize];
            in_triangle((x, z), t[0], t[1], t[2])
        })
    }
}

impl GrassArea {
    /// Bucket size in metres. Matches [`WaterMask`]'s: these are the same kind of
    /// triangle soup being asked the same kind of question.
    const CELL: f32 = 16.0;

    /// Read a tile's mowable green.
    ///
    /// Which polygons count is [`land_group`]'s [`SurfaceGroup::Grass`] — one
    /// table, so the blades stand on exactly the polygons that are painted as
    /// grass and never on a forest floor or a beach.
    ///
    /// `crops` is this cell's registered agricultural parcels, in the same
    /// tile-local metre frame; the arable ones are a keep-out. Pass an empty slice
    /// where there is no register — outside the Netherlands, and on every host
    /// that has not run the extractor — and the layer behaves exactly as it did
    /// before crops existed.
    #[must_use]
    pub fn build(tile: &TileData, tile_m: f32, crops: &[CropParcel]) -> Self {
        use geo_types::Geometry;

        let Some(meta) = tile.layer("land") else {
            // No landuse, so no grass — and therefore no reason to pay for any
            // keep-out mask, each of which walks a whole layer of the tile.
            return Self {
                tris: Vec::new(),
                buckets: HashMap::new(),
                water: WaterMask::empty(tile_m),
                roads: RoadMask::empty(tile_m),
                arable: ArableMask::default(),
                openings: Vec::new(),
            };
        };
        let extent = TileData::extent(meta);
        let scale = tile_m / extent;
        let mut area = Self {
            tris: Vec::new(),
            buckets: HashMap::new(),
            water: WaterMask::build(tile, scale, extent, tile_m),
            roads: RoadMask::build(tile, tile_m),
            arable: ArableMask::build(crops, tile_m),
            openings: Vec::new(),
        };

        let mut positions = Vec::new();
        let mut indices = Vec::new();
        for feature in tile.features(meta) {
            if kind_of(feature).and_then(land_group) != Some(SurfaceGroup::Grass) {
                continue;
            }
            let polys: Vec<&geo_types::Polygon<f32>> = match &feature.geometry {
                Geometry::Polygon(p) => vec![p],
                Geometry::MultiPolygon(mp) => mp.iter().collect(),
                _ => continue,
            };
            // Grass runs right up to its polygon's own edge — unlike a canopy it
            // has no overhang to keep off the neighbour — so unlike the tree
            // scatter this needs no boundary inset, only the two keep-outs for
            // the layers drawn *over* the landuse.
            for p in &polys {
                positions.clear();
                indices.clear();
                tessellate_polygon(p, scale, extent, &mut positions, &mut indices);
                for tri in indices.chunks_exact(3) {
                    let v = [
                        (positions[tri[0] as usize][0], positions[tri[0] as usize][2]),
                        (positions[tri[1] as usize][0], positions[tri[1] as usize][2]),
                        (positions[tri[2] as usize][0], positions[tri[2] as usize][2]),
                    ];
                    area.push(v, tile_m);
                }
            }
        }
        area
    }

    /// Add a surveyed green surface — a verge, a grass-material surface, the
    /// planted part of a garden — in the cell's tile-local metres, outer ring
    /// first. The tile's `land` layer rarely maps private gardens or verges, so
    /// without this the survey's green would be drawn green and grow nothing.
    pub fn add_surveyed(&mut self, rings: &[Vec<[f32; 2]>], tile_m: f32) {
        let Some(outer) = rings.first() else {
            return;
        };
        if outer.len() < 3 {
            return;
        }
        let mut flat: Vec<f64> = Vec::new();
        let mut holes = Vec::new();
        for (i, ring) in rings.iter().enumerate() {
            if ring.len() < 3 {
                continue;
            }
            if i > 0 {
                holes.push(flat.len() / 2);
            }
            for p in ring {
                flat.push(f64::from(p[0]));
                flat.push(f64::from(p[1]));
            }
        }
        let Ok(tris) = earcutr::earcut(&flat, &holes, 2) else {
            return;
        };
        let p = |i: usize| (flat[i * 2] as f32, flat[i * 2 + 1] as f32);
        for t in tris.chunks_exact(3) {
            self.push([p(t[0]), p(t[1]), p(t[2])], tile_m);
        }
    }

    fn push(&mut self, v: [(f32, f32); 3], tile_m: f32) {
        let idx = self.tris.len() as u32;
        self.tris.push(v);
        let (minx, maxx) = (
            v[0].0.min(v[1].0).min(v[2].0),
            v[0].0.max(v[1].0).max(v[2].0),
        );
        let (minz, maxz) = (
            v[0].1.min(v[1].1).min(v[2].1),
            v[0].1.max(v[1].1).max(v[2].1),
        );
        for i in bucket_range(minx, maxx, Self::CELL, tile_m) {
            for j in bucket_range(minz, maxz, Self::CELL, tile_m) {
                self.buckets.entry((i, j)).or_default().push(idx);
            }
        }
    }

    /// Does this cell hold any grass at all? A cell that does not is dropped
    /// rather than kept, which is most of a city centre.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.tris.is_empty()
    }

    /// Grass triangles in this cell. Diagnostic only: "the cell has geometry" and
    /// "the window found any of it" are different failures.
    #[must_use]
    pub fn triangles(&self) -> usize {
        self.tris.len()
    }

    /// May a blade stand at `(x, z)`, in this cell's tile-local metre frame?
    #[must_use]
    pub fn plantable(&self, x: f32, z: f32) -> bool {
        let key = (
            (x / Self::CELL).floor() as i32,
            (z / Self::CELL).floor() as i32,
        );
        let Some(ids) = self.buckets.get(&key) else {
            return false;
        };
        let on_grass = ids.iter().any(|&id| {
            let t = self.tris[id as usize];
            in_triangle((x, z), t[0], t[1], t[2])
        });
        on_grass
            && !self.water.blocks(x, z, BLADE_CLEARANCE_M)
            && !self.roads.blocks(x, z, BLADE_CLEARANCE_M)
            // No clearance on this one: a lawn beside a ploughed field runs right
            // up to the headland, and the parcel boundary is a surveyed line
            // rather than a generalisation that has to be given room.
            && !self.arable.blocks(x, z)
            // No clearance here either: the opening's edge and the retaining wall
            // standing in it are the same line by construction
            // (`tunnels::cut_ground` subtracts the opening from the ground on the
            // same lattice), so a margin would leave a bald strip along the kerb
            // rather than protecting anything.
            && !self.over_a_mouth(x, z)
    }

    /// Record the tunnel openings this cell's ground was cut against.
    ///
    /// Set after construction rather than taken by [`Self::build`] because the
    /// openings are a product of [`crate::tunnels::build_tunnels`], which runs on
    /// the same worker and needs the same tile — threading them through the
    /// constructor would order two independent pieces of work for no reason.
    pub fn block_mouths(&mut self, openings: &[Vec<[f32; 2]>]) {
        self.openings = openings
            .iter()
            .filter(|ring| ring.len() >= 4)
            .map(|ring| {
                let mut b = [f32::MAX, f32::MAX, f32::MIN, f32::MIN];
                for p in ring {
                    b[0] = b[0].min(p[0]);
                    b[1] = b[1].min(p[1]);
                    b[2] = b[2].max(p[0]);
                    b[3] = b[3].max(p[1]);
                }
                (b, ring.clone())
            })
            .collect();
    }

    fn over_a_mouth(&self, x: f32, z: f32) -> bool {
        self.openings.iter().any(|(b, ring)| {
            x >= b[0]
                && x <= b[2]
                && z >= b[1]
                && z <= b[3]
                && crate::tunnels::point_in_ring([x, z], ring)
        })
    }
}

/// A live cell, placed: its [`GrassArea`] and the anchor-metre position of its
/// north-west corner — the same origin `map_geometry` spawns its meshes at.
pub struct PlacedArea<'a> {
    pub origin: [f32; 2],
    pub tile_m: f32,
    pub area: &'a GrassArea,
}

/// The moving near-field mask the vertex shader samples.
///
/// Two channels, both of which a blade needs and neither of which it can work out
/// for itself: **red** is grass coverage, **green** is the ground height, encoded
/// over `[min_y, min_y + span_y]` so a byte is worth about a centimetre over any
/// relief this field spans.
///
/// Height is in here rather than being read off the patch entity's transform
/// because a patch is metres across and the ground under it is not flat. Sampling
/// the same terrain field the ground *mesh* was built from (`systems::terrain`,
/// which this crate may not name), at the blade's own position, is what keeps a
/// blade's feet on the ground rather than a few centimetres under or over it.
pub struct MaskField {
    /// Side length in texels.
    pub px: u32,
    /// `px * px * 2` bytes, row-major, `[coverage, height]` per texel.
    pub data: Vec<u8>,
    /// Anchor-metre position of the field's minimum corner.
    pub origin: [f32; 2],
    /// Ground size of the whole field, in metres.
    pub size_m: f32,
    /// Ground height the green channel's `0` stands for, and the range a full
    /// byte covers.
    pub min_y: f32,
    pub span_y: f32,
    /// How many cells the rasterise actually consulted. Diagnostic: a mask is
    /// built from a snapshot, so this is the only honest answer to "what was
    /// there when this was made".
    pub cells: usize,
}

/// The smallest height range the green channel is allowed to span, in metres.
///
/// Over flat ground — which is most of the Netherlands, and *all* of it when the
/// height layer is off — the measured range is zero, and encoding a zero range
/// makes every byte stand for the same height, which is fine, and makes the
/// decode divide by zero, which is not. Clamping the span instead keeps one code
/// path: the shader always decodes `min + g * span`.
const MIN_HEIGHT_SPAN_M: f32 = 1.0;

/// Rasterise the near-field mask over a `size_m` square whose minimum corner is
/// `origin`, in anchor metres.
///
/// `ground` is the terrain lift at an anchor-metre position — `TerrainField::at`,
/// passed in because this crate may not depend on Bevy and the field is a client
/// resource. It answers 0 everywhere the layer is off or out of coverage, which
/// is exactly the flat-ground behaviour this had before there was a terrain layer
/// at all.
///
/// Every texel is one point sample rather than a supersampled coverage, and the
/// edge that produces is soft anyway: the texture is bilinearly filtered and the
/// shader crosses it with a `smoothstep`, so a boundary lands within about half a
/// texel. At `systems::grass`'s sizes that is ~12 cm, which is finer than the
/// disagreement between the landuse polygon and the road ribbon beside it.
#[must_use]
pub fn mask_field(
    cells: &[PlacedArea<'_>],
    origin: [f32; 2],
    size_m: f32,
    px: u32,
    ground: &dyn Fn(f32, f32) -> f32,
) -> MaskField {
    let n = (px * px) as usize;
    let step = size_m / px as f32;
    let mut cover = vec![0u8; n];
    let mut heights = vec![0.0f32; n];
    let (mut min_y, mut max_y) = (f32::MAX, f32::MIN);

    for j in 0..px {
        for i in 0..px {
            // Texel centres, so a sample is never taken exactly on a polygon edge.
            let wx = origin[0] + (i as f32 + 0.5) * step;
            let wz = origin[1] + (j as f32 + 0.5) * step;
            let k = (j * px + i) as usize;
            let y = ground(wx, wz);
            heights[k] = y;
            min_y = min_y.min(y);
            max_y = max_y.max(y);
            if cells.iter().any(|c| {
                let (lx, lz) = (wx - c.origin[0], wz - c.origin[1]);
                // A cell only answers for its own ground; asking a neighbour about
                // a point outside it would read its buckets at a negative index.
                lx >= 0.0 && lz >= 0.0 && lx < c.tile_m && lz < c.tile_m && c.area.plantable(lx, lz)
            }) {
                cover[k] = u8::MAX;
            }
        }
    }

    let span_y = (max_y - min_y).max(MIN_HEIGHT_SPAN_M);
    let mut data = Vec::with_capacity(n * 2);
    for k in 0..n {
        data.push(cover[k]);
        data.push((((heights[k] - min_y) / span_y).clamp(0.0, 1.0) * 255.0 + 0.5) as u8);
    }
    MaskField {
        px,
        data,
        origin,
        size_m,
        min_y,
        span_y,
        cells: cells.len(),
    }
}

/// The vertex buffers of the one mesh every grass patch draws.
///
/// `positions` is the blade *slot* — the same value on all five of a blade's
/// vertices — and `uvs.x` is which of the five this is. Everything else a blade
/// has is derived in the shader; `normals` and `colors` are placeholders, present
/// only because their presence is what defines `VERTEX_NORMALS` and
/// `VERTEX_COLORS` and so gives the vertex stage somewhere to *write* the normal
/// and the tone it computes.
pub struct BladeLattice {
    pub positions: Vec<[f32; 3]>,
    pub normals: Vec<[f32; 3]>,
    pub uvs: Vec<[f32; 2]>,
    pub colors: Vec<[f32; 4]>,
    pub indices: Vec<u32>,
}

impl BladeLattice {
    /// Blades in this lattice.
    #[must_use]
    pub fn blades(&self) -> usize {
        self.positions.len() / BLADE_VERTS as usize
    }
}

/// Build the shared lattice: `side²` blade slots on a `spacing`-metre grid,
/// filling a patch `side * spacing` metres square.
///
/// The slots are a regular grid on purpose. A jittered *stored* lattice would have
/// to be stored, and this mesh is shared by every patch in the world — the jitter
/// belongs in the shader, where it is a hash of the slot's world position and
/// therefore both free and stable.
#[must_use]
pub fn blade_lattice(side: u32, spacing: f32) -> BladeLattice {
    let blades = (side * side) as usize;
    let verts = blades * BLADE_VERTS as usize;
    let mut lattice = BladeLattice {
        positions: Vec::with_capacity(verts),
        normals: vec![[0.0, 1.0, 0.0]; verts],
        uvs: Vec::with_capacity(verts),
        colors: vec![[1.0, 1.0, 1.0, 1.0]; verts],
        indices: Vec::with_capacity(blades * BLADE_TRIS as usize * 3),
    };
    // Rows are staggered half a cell against each other — hexagonal packing
    // rather than a square grid, and it is the whole reason this loop is not two
    // multiplications.
    //
    // A square lattice has *rows*, and the shader's jitter cannot remove them: it
    // offsets each blade by up to half a cell, so a blade never leaves the cell it
    // was born in and the rows survive as rows. Looking along either axis at a
    // grazing angle they line up into the corduroy a ploughed field has, which is
    // exactly what a lawn does not look like. Offsetting alternate rows by half a
    // cell breaks the alignment along both axes at once, costs nothing (the slots
    // are placed once, into a mesh every patch shares) and leaves the count, the
    // spacing and the density exactly as they were.
    //
    // ±a quarter rather than one row at +a half, so the patch's own bounds are
    // unchanged and `the_lattice_fills_its_patch_and_nothing_more` still holds;
    // what matters is the *relative* half-cell between neighbouring rows.
    //
    // **`side` must be even**, or the wrap puts two same-phase rows against each
    // other at the patch seam and the artefact comes back once per patch instead
    // of once per cell. Asserted rather than documented, because every caller's
    // value is a constant and a future odd one would be a silent regression.
    debug_assert!(
        side % 2 == 0,
        "a staggered lattice needs an even side, got {side}"
    );
    for j in 0..side {
        let stagger = if j % 2 == 0 { -0.25 } else { 0.25 };
        for i in 0..side {
            let base = lattice.positions.len() as u32;
            let (x, z) = (
                (i as f32 + 0.5 + stagger) * spacing,
                (j as f32 + 0.5) * spacing,
            );
            for role in 0..BLADE_VERTS {
                lattice.positions.push([x, 0.0, z]);
                lattice.uvs.push([role as f32, 0.0]);
            }
            // 0,1 = base left/right · 2,3 = mid left/right · 4 = tip. Winding is
            // not load-bearing: a blade is drawn `cull_mode: None`, because from
            // the far side it is the same blade and there is no back of one.
            lattice.indices.extend_from_slice(&[
                base,
                base + 1,
                base + 3,
                base,
                base + 3,
                base + 2,
                base + 2,
                base + 3,
                base + 4,
            ]);
        }
    }
    lattice
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fixture() -> (TileData, f32) {
        (crate::testdata::groningen(), 1500.0)
    }

    /// **A lawn must not grow over an open cut.**
    ///
    /// The blades come from this cell's grass polygons, not from the ground mesh,
    /// so `tunnels::cut_ground` taking the ground away over a entrance does nothing
    /// to them on its own — which is how the 2026-09-16 capture over the Groningen
    /// ring came back with grass lying across the trench and the carriageway
    /// underneath it. The trees had been handled (`tunnels::in_an_opening`); this
    /// layer has no instances to drop, so it keeps out by mask.
    #[test]
    fn grass_keeps_out_of_a_tunnel_mouth() {
        let (tile, tile_m) = fixture();
        let mut area = GrassArea::build(&tile, tile_m, &[]);
        assert!(!area.is_empty(), "the fixture has grass in it");

        // A point the tile plants on, found rather than assumed: the fixture's
        // grass is wherever Groningen's parks are.
        let mut on_grass = None;
        'search: for zi in 0..150 {
            for xi in 0..150 {
                let (x, z) = (xi as f32 * 10.0, zi as f32 * 10.0);
                if area.plantable(x, z) {
                    on_grass = Some((x, z));
                    break 'search;
                }
            }
        }
        let (x, z) = on_grass.expect("the fixture plants somewhere");

        // A opening over exactly that point, and the same point elsewhere untouched.
        let ring = vec![
            [x - 15.0, z - 15.0],
            [x + 15.0, z - 15.0],
            [x + 15.0, z + 15.0],
            [x - 15.0, z + 15.0],
            [x - 15.0, z - 15.0],
        ];
        area.block_mouths(&[ring]);
        assert!(
            !area.plantable(x, z),
            "a blade may not stand in the opening at ({x}, {z})"
        );
        assert!(
            area.plantable(x + 40.0, z)
                || !GrassArea::build(&tile, tile_m, &[]).plantable(x + 40.0, z),
            "the keep-out must be the opening and not the whole cell"
        );
    }

    #[test]
    fn the_lattice_is_one_blade_per_slot() {
        let l = blade_lattice(4, 0.3);
        assert_eq!(l.blades(), 16);
        assert_eq!(l.positions.len(), 16 * BLADE_VERTS as usize);
        assert_eq!(l.indices.len(), 16 * BLADE_TRIS as usize * 3);
        // Every attribute must be the same length or the mesh is rejected at
        // upload with a message about the *mesh*, not about this function.
        assert_eq!(l.normals.len(), l.positions.len());
        assert_eq!(l.uvs.len(), l.positions.len());
        assert_eq!(l.colors.len(), l.positions.len());
    }

    #[test]
    fn a_slot_is_one_position_and_five_roles() {
        // The shader reads the slot off `position` and the corner off `uv.x`;
        // if the five vertices of a blade disagreed about the slot, the blade
        // would be smeared across the patch.
        let l = blade_lattice(2, 1.0);
        for b in 0..l.blades() {
            let s = b * BLADE_VERTS as usize;
            let slot = l.positions[s];
            for r in 0..BLADE_VERTS as usize {
                assert_eq!(l.positions[s + r], slot);
                assert!((l.uvs[s + r][0] - r as f32).abs() < 1e-6);
            }
        }
    }

    #[test]
    fn the_lattice_fills_its_patch_and_nothing_more() {
        let (side, spacing) = (8u32, 0.25);
        let l = blade_lattice(side, spacing);
        let edge = side as f32 * spacing;
        for p in &l.positions {
            assert!(p[0] > 0.0 && p[0] < edge, "x {} outside 0..{edge}", p[0]);
            assert!(p[2] > 0.0 && p[2] < edge, "z {} outside 0..{edge}", p[2]);
        }
    }

    #[test]
    fn a_real_tile_has_grass_and_it_is_not_everywhere() {
        let (tile, tile_m) = fixture();
        let area = GrassArea::build(&tile, tile_m, &[]);
        assert!(!area.is_empty(), "Groningen z14 has parks in it");
        // Sample the cell on a coarse grid: a city tile must come out neither
        // bare (the layer would be invisible) nor solid (it would be planting
        // grass through the buildings and the canals).
        let mut hits = 0;
        let mut total = 0;
        for i in 0..100 {
            for j in 0..100 {
                let (x, z) = (i as f32 * tile_m / 100.0, j as f32 * tile_m / 100.0);
                total += 1;
                if area.plantable(x, z) {
                    hits += 1;
                }
            }
        }
        let frac = hits as f32 / total as f32;
        assert!(
            (0.01..0.75).contains(&frac),
            "grass covers {frac:.3} of the tile, which is not a city"
        );
    }

    #[test]
    fn grass_stops_at_the_carriageway() {
        // The whole reason a keep-out exists: `land` and `streets` are separate
        // layers, so a park polygon runs straight under the path across it. If
        // this ever regresses, blades grow down the middle of every road through
        // a green.
        let (tile, tile_m) = fixture();
        let area = GrassArea::build(&tile, tile_m, &[]);
        let mut on_road = 0;
        let mut sampled = 0;
        let meta = tile.layer("streets").expect("the fixture has streets");
        let scale = tile_m / TileData::extent(meta);
        for feature in tile.features(meta) {
            if crate::vector_tiles::drawn_street(feature).is_none() {
                continue;
            }
            let geo_types::Geometry::LineString(ls) = &feature.geometry else {
                continue;
            };
            for c in ls.0.iter().take(20) {
                let (x, z) = (c.x * scale, c.y * scale);
                if x < 0.0 || z < 0.0 || x >= tile_m || z >= tile_m {
                    continue;
                }
                sampled += 1;
                if area.plantable(x, z) {
                    on_road += 1;
                }
            }
            if sampled > 400 {
                break;
            }
        }
        assert!(sampled > 0, "the fixture has streets");
        assert_eq!(on_road, 0, "{on_road} of {sampled} road points grew grass");
    }

    #[test]
    fn grass_stops_at_the_water() {
        // The sibling of `grass_stops_at_the_carriageway`, and the same trap: a
        // park polygon runs straight under the pond in the middle of it, because
        // `land` and `water_polygons` are independent layers and Shortbread draws
        // the water on top. Without the keep-out, a lawn grows across the canal.
        let (tile, tile_m) = fixture();
        let area = GrassArea::build(&tile, tile_m, &[]);
        let meta = tile
            .layer("water_polygons")
            .expect("the fixture has canals in it");
        let extent = TileData::extent(meta);
        let scale = tile_m / extent;
        let mut on_water = 0;
        let mut sampled = 0;
        for feature in tile.features(meta) {
            let polys: Vec<&geo_types::Polygon<f32>> = match &feature.geometry {
                geo_types::Geometry::Polygon(p) => vec![p],
                geo_types::Geometry::MultiPolygon(mp) => mp.iter().collect(),
                _ => continue,
            };
            for p in polys {
                // The polygon's own interior, not its outline: a centroid-ish
                // sample of each triangle is a point that is unambiguously *in*
                // the water rather than on its edge.
                let mut positions = Vec::new();
                let mut indices = Vec::new();
                crate::vector_tiles::tessellate_polygon(
                    p,
                    scale,
                    extent,
                    &mut positions,
                    &mut indices,
                );
                for tri in indices.chunks_exact(3) {
                    let c = |k: usize| {
                        (positions[tri[0] as usize][k]
                            + positions[tri[1] as usize][k]
                            + positions[tri[2] as usize][k])
                            / 3.0
                    };
                    let (x, z) = (c(0), c(2));
                    if x < 0.0 || z < 0.0 || x >= tile_m || z >= tile_m {
                        continue;
                    }
                    sampled += 1;
                    if area.plantable(x, z) {
                        on_water += 1;
                    }
                }
            }
            if sampled > 400 {
                break;
            }
        }
        assert!(sampled > 0, "the fixture has water polygons");
        assert_eq!(
            on_water, 0,
            "{on_water} of {sampled} water points grew grass"
        );
    }

    /// The value-1 defect this layer fixes, at the point it is fixed: a potato
    /// field and a meadow are the same `farmland`-ish polygon in the tile, so
    /// without the register the blades grow on both. Two parcels over the *same*
    /// ground, so nothing but the register can be the difference.
    #[test]
    fn blades_grow_on_a_registered_meadow_and_not_on_a_ploughed_field() {
        use crate::crops::{CropCategory, CropParcel};
        let (tile, tile_m) = fixture();
        // A spot the tile itself calls plantable, so the only thing under test is
        // the keep-out.
        let bare = GrassArea::build(&tile, tile_m, &[]);
        let mut hit = None;
        'search: for i in 0..200 {
            for j in 0..200 {
                let (x, z) = (i as f32 * tile_m / 200.0, j as f32 * tile_m / 200.0);
                if bare.plantable(x, z) {
                    hit = Some((x, z));
                    break 'search;
                }
            }
        }
        let (hx, hz) = hit.expect("the fixture has plantable grass");
        let parcel = |category| CropParcel {
            category,
            code: 265,
            seed: 0,
            rings: vec![vec![
                [hx - 30.0, hz - 30.0],
                [hx + 30.0, hz - 30.0],
                [hx + 30.0, hz + 30.0],
                [hx - 30.0, hz + 30.0],
            ]],
        };
        let grazed = GrassArea::build(&tile, tile_m, &[parcel(CropCategory::Grassland)]);
        let ploughed = GrassArea::build(&tile, tile_m, &[parcel(CropCategory::Arable)]);
        assert!(
            grazed.plantable(hx, hz),
            "a registered meadow should still grow blades"
        );
        assert!(
            !ploughed.plantable(hx, hz),
            "blades grew on an arable parcel"
        );
    }

    #[test]
    fn the_mask_carries_coverage_and_a_decodable_height() {
        let (tile, tile_m) = fixture();
        let area = GrassArea::build(&tile, tile_m, &[]);
        let placed = [PlacedArea {
            origin: [0.0, 0.0],
            tile_m,
            area: &area,
        }];
        // A ramp, so the encode is exercised over a real range rather than the
        // clamped-flat case.
        let ground = |x: f32, _z: f32| x * 0.01;
        let field = mask_field(&placed, [0.0, 0.0], 64.0, 32, &ground);
        assert_eq!(field.data.len(), 32 * 32 * 2);
        assert!(field.span_y >= MIN_HEIGHT_SPAN_M);
        // Green decodes back to the height that went in, to within a byte.
        let step = field.size_m / field.px as f32;
        for j in 0..field.px {
            for i in 0..field.px {
                let wx = field.origin[0] + (i as f32 + 0.5) * step;
                let g = f32::from(field.data[((j * field.px + i) * 2 + 1) as usize]) / 255.0;
                let decoded = field.min_y + g * field.span_y;
                assert!(
                    (decoded - ground(wx, 0.0)).abs() <= field.span_y / 255.0 + 1e-4,
                    "height decode drifted at ({i},{j})"
                );
            }
        }
    }

    #[test]
    fn the_mask_finds_grass_when_the_window_is_over_a_park() {
        // The gap the first version of this suite left open: `plantable` was
        // asserted directly and the mask's *height* channel was asserted, but
        // nothing asserted coverage through `mask_field` — so a window that
        // agreed with no cell came back all-zero and looked like a lawn with no
        // grass in it. That is exactly what shipped to the first capture.
        let (tile, tile_m) = fixture();
        let area = GrassArea::build(&tile, tile_m, &[]);
        // Find a spot the area itself calls plantable, then put the window on it.
        let mut hit = None;
        'search: for i in 0..200 {
            for j in 0..200 {
                let (x, z) = (i as f32 * tile_m / 200.0, j as f32 * tile_m / 200.0);
                if area.plantable(x, z) {
                    hit = Some((x, z));
                    break 'search;
                }
            }
        }
        let (hx, hz) = hit.expect("the fixture has plantable grass");

        // The cell placed away from the origin, because that is the only case
        // that ever happens: `map_geometry` puts a cell's north-west corner at
        // its own offset from the map anchor, never at zero.
        let cell_origin = [-2000.0, 3500.0];
        let placed = [PlacedArea {
            origin: cell_origin,
            tile_m,
            area: &area,
        }];
        let origin = [cell_origin[0] + hx - 48.0, cell_origin[1] + hz - 48.0];
        let field = mask_field(&placed, origin, 96.0, 128, &|_, _| 0.0);
        let covered = field.data.chunks_exact(2).filter(|t| t[0] > 0).count();
        assert!(
            covered > 0,
            "a 96 m window centred on plantable grass came back bare"
        );
    }

    #[test]
    fn the_mask_is_empty_where_no_cell_answers() {
        // A window that overlaps nothing must come back all-zero rather than
        // all-one: an unplanted mask is an unplanted lawn, but an accidentally
        // saturated one is grass growing over the sea.
        let (tile, tile_m) = fixture();
        let area = GrassArea::build(&tile, tile_m, &[]);
        let placed = [PlacedArea {
            origin: [0.0, 0.0],
            tile_m,
            area: &area,
        }];
        let field = mask_field(&placed, [-500.0, -500.0], 64.0, 16, &|_, _| 0.0);
        assert!(field.data.chunks_exact(2).all(|t| t[0] == 0));
    }

    #[test]
    fn a_flat_world_still_encodes() {
        // The common case: no height layer, so every sample is 0 and the range is
        // degenerate. The span must not be, or the shader divides by zero.
        let (tile, tile_m) = fixture();
        let area = GrassArea::build(&tile, tile_m, &[]);
        let placed = [PlacedArea {
            origin: [0.0, 0.0],
            tile_m,
            area: &area,
        }];
        let field = mask_field(&placed, [0.0, 0.0], 32.0, 8, &|_, _| 0.0);
        assert_eq!(field.span_y, MIN_HEIGHT_SPAN_M);
        assert_eq!(field.min_y, 0.0);
    }
}
