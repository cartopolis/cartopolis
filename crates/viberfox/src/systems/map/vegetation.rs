//! Trees, scattered over the vector map's landuse polygons.
//!
//! The data is the same MVT body `map_geometry` already fetches and tessellates:
//! Shortbread's `land` layer carries `kind = forest | wood | park | garden |
//! orchard | scrub | …`, which is **canopy areas, not individual trees** — there is
//! no `natural=tree` layer in the schema. So the trees here are *scattered*, not
//! surveyed. (Real per-tree positions exist in OSM and would come from an Overpass
//! layer in `data_layers`; that is a separate, close-range path, and it is what
//! would finally distinguish needleleaf from broadleaf, which the MVT tags cannot.)
//!
//! Three things shape the design:
//!
//! * **The map is pale cartographic paper**, not a satellite photo: `FOREST` is a
//!   light desaturated sage at ~80% luminance on near-white. Saturated dark-green
//!   blobs read as confetti scattered on a pastel drawing. Canopies are therefore
//!   one step down the *same* hue ramp, and trunks are a warm grey in the
//!   building-edge family — a brown trunk is the loudest colour on the whole map.
//! * **The camera lives above the map** (orbit cam, `--look=-90` shots, the space
//!   tier). Crossed quads collapse to lines seen from directly overhead and
//!   alpha-tested billboards need a texture pipeline plus a sort order, so canopies
//!   are small opaque solids: correct from every angle, no alpha, no sorting.
//! * **Mass mesh churn loses the device** (see CLAUDE.md). Trees are merged into
//!   *one mesh per cell per group*, ride `map_geometry`'s existing worker, build cap
//!   and despawn cap, and are hard-capped per cell by [`MAX_TREES_PER_CELL`].
//!
//! The altitude transition is the part that needs care, and it is the reason
//! nothing here switches on a boolean — see [`update_vegetation_lod`].

use std::collections::{HashMap, HashSet};

use bevy::prelude::*;
use big_space::prelude::GridCell;

use crate::systems::free_camera::FreeCamera;
use crate::systems::globe::GlobeState;
use crate::systems::map_geometry::MapSurfaces;
use crate::systems::vector_tiles::{palette, SurfaceGroup, SurfaceMesh, TileData};

/// Altitude at which trees start merging into the flat fill.
pub const TREE_FADE_START: f32 = 500.0;

/// Altitude by which they have merged completely and the geometry can be hidden.
pub const TREE_FADE_END: f32 = 2000.0;

/// Hard ceiling on trees per streamed cell. A z14 cell at NL latitudes is ~1.5 km
/// across; a cell that is mostly forest would otherwise scatter ~35,000 trees and
/// upload a million triangles in one command buffer, which is the documented
/// device-loss pattern. Truncation is logged rather than silent.
pub const MAX_TREES_PER_CELL: usize = 6_000;

/// One scattered tree, in the tessellator's tile-local metre frame (origin at the
/// cell's north-west corner, +x east, +z south, ground at y = 0).
#[derive(Debug, Clone, Copy)]
pub struct TreeInstance {
    pub x: f32,
    pub z: f32,
    /// Canopy radius in metres.
    pub radius: f32,
    /// Total height, ground to the top of the crown.
    pub height: f32,
    /// 0 = [`palette::CANOPY`], 1 = [`palette::CANOPY_DEEP`].
    pub tone: f32,
    /// Per-instance brightness multiplier, centred on 1.0.
    pub luma: f32,
}

/// How one landuse kind is planted.
#[derive(Debug, Clone, Copy)]
struct Profile {
    /// Lattice pitch in metres — roughly the mean distance between trunks.
    spacing: f32,
    /// How far a tree may wander inside its lattice cell, as a fraction of the
    /// pitch. Zero gives a regular grid, which is right for exactly one kind.
    jitter: f32,
    /// Height range, metres.
    height: (f32, f32),
    /// Canopy radius range, metres.
    radius: (f32, f32),
    /// Align the lattice to the polygon's dominant edge, so the rows run with the
    /// field rather than with the Mercator grid.
    rows: bool,
}

/// Which `land` kinds get trees, and how densely. Unlisted kinds get none — the
/// same "an unstyled polygon beats a wrongly styled one" rule the fills follow.
///
/// `park`/`garden` are deliberately an order of magnitude sparser than woodland:
/// a park should read as lawn with specimen trees on it, not as forest.
fn veg_profile(kind: &str) -> Option<Profile> {
    Some(match kind {
        "forest" | "wood" => Profile {
            spacing: 8.0,
            jitter: 0.75,
            height: (9.0, 14.0),
            radius: (2.6, 4.2),
            rows: false,
        },
        "park" | "garden" | "village_green" | "recreation_ground" | "cemetery" => Profile {
            spacing: 22.0,
            jitter: 0.8,
            height: (8.0, 13.0),
            radius: (3.0, 4.8),
            rows: false,
        },
        // The one kind where regularity is *correct*: an orchard read from the air
        // is rows, and a jittered scatter would destroy the only thing that makes
        // it recognisable as one.
        "orchard" | "allotments" => Profile {
            spacing: 9.0,
            jitter: 0.0,
            height: (3.5, 5.0),
            radius: (1.8, 2.6),
            rows: true,
        },
        "scrub" | "heath" => Profile {
            spacing: 6.0,
            jitter: 0.9,
            height: (1.4, 2.6),
            radius: (1.0, 1.8),
            rows: false,
        },
        _ => return None,
    })
}

/// Below this height a plant is a bush: no trunk mesh is emitted for it. Scrub is
/// ~40% of the instances in a heathland cell and none of it has a visible stem.
const TRUNK_MIN_HEIGHT: f32 = 4.0;

// ─── Scatter ─────────────────────────────────────────────────────────────────

/// Scatter trees over a tile's vegetated `land` polygons.
///
/// `seed` must be stable for a given cell (its x/y are the natural choice), so a
/// cell that streams out and back in regrows the *same* forest instead of
/// reshuffling every tree — a reshuffle is instantly visible as a shimmer when the
/// ring moves back and forth over a boundary.
pub fn scatter_vegetation(
    tile: &TileData,
    tile_m: f32,
    seed: u64,
) -> Result<Vec<TreeInstance>, String> {
    use crate::systems::vector_tiles::{clipped_rings, kind_of, tessellate_polygon};
    use geo_types::Geometry;

    let Some(meta) = tile.layer("land") else {
        return Ok(Vec::new());
    };
    let features = tile.features(meta);
    let extent = TileData::extent(meta);
    let scale = tile_m / extent;

    // A park polygon routinely *contains* the pond and the canal running through
    // it — Shortbread draws the water on top as its own layer rather than punching
    // a hole in the landuse. Insetting from the park's own boundary therefore does
    // nothing for the water inside it, and the first render planted a wood down
    // the middle of the Noorderhaven. Water is a separate mask.
    let water = WaterMask::build(tile, scale, extent, tile_m);

    let mut out: Vec<TreeInstance> = Vec::new();
    let mut truncated = false;

    for (fi, feature) in features.iter().enumerate() {
        let Some(profile) = kind_of(feature).and_then(veg_profile) else {
            continue;
        };
        let polys: Vec<&geo_types::Polygon<f32>> = match &feature.geometry {
            Geometry::Polygon(p) => vec![p],
            Geometry::MultiPolygon(mp) => mp.iter().collect(),
            _ => continue,
        };

        // Triangles to plant inside, and the rings to keep away from. Both come
        // from the same clamped coordinates the fill uses, so the trees line up
        // with the green they stand on.
        let mut positions = Vec::new();
        let mut indices = Vec::new();
        let mut rings: Vec<Vec<[f32; 2]>> = Vec::new();
        for p in &polys {
            tessellate_polygon(p, scale, extent, &mut positions, &mut indices);
            rings.extend(clipped_rings(p, scale, extent));
        }
        if indices.is_empty() {
            continue;
        }

        let border = BorderGuard::new(tile_m);
        let edges = EdgeGrid::new(&rings, &border);
        let theta = if profile.rows {
            dominant_angle(&rings)
        } else {
            0.0
        };
        let (cs, sn) = (theta.cos(), theta.sin());
        // World → lattice frame is a rotation by −θ; lattice → world by +θ.
        let to_lattice = |x: f32, z: f32| (x * cs + z * sn, -x * sn + z * cs);
        let to_world = |x: f32, z: f32| (x * cs - z * sn, x * sn + z * cs);

        let fseed = seed ^ (fi as u64).wrapping_mul(0x9E37_79B9_7F4A_7C15);
        // Lattice indices are global to the feature, so a point falling on a shared
        // edge is generated by both triangles with the *same* index — deduping on
        // the index is exact and saves an epsilon-tuned half-open containment rule.
        let mut planted: HashSet<(i32, i32)> = HashSet::new();
        let s = profile.spacing;

        for tri in indices.chunks_exact(3) {
            let v: Vec<(f32, f32)> = tri
                .iter()
                .map(|&i| {
                    let p = positions[i as usize];
                    to_lattice(p[0], p[2])
                })
                .collect();
            let (minx, maxx) = min_max(v[0].0, v[1].0, v[2].0);
            let (minz, maxz) = min_max(v[0].1, v[1].1, v[2].1);
            let i0 = (minx / s).floor() as i32;
            let i1 = (maxx / s).floor() as i32;
            let j0 = (minz / s).floor() as i32;
            let j1 = (maxz / s).floor() as i32;

            for i in i0..=i1 {
                for j in j0..=j1 {
                    if planted.contains(&(i, j)) {
                        continue;
                    }
                    let h = hash3(fseed, i, j);
                    let jx = (rand01(h, 0) - 0.5) * profile.jitter;
                    let jz = (rand01(h, 1) - 0.5) * profile.jitter;
                    let lx = (i as f32 + 0.5 + jx) * s;
                    let lz = (j as f32 + 0.5 + jz) * s;
                    if !in_triangle((lx, lz), v[0], v[1], v[2]) {
                        continue;
                    }
                    let radius = lerp(profile.radius.0, profile.radius.1, rand01(h, 2));
                    let (wx, wz) = to_world(lx, lz);
                    // Keep the canopy off whatever the polygon borders — a forest
                    // edge against a canal must not hang trees over the water.
                    if edges.within(wx, wz, radius * 0.8) || water.blocks(wx, wz, radius * 0.8) {
                        continue;
                    }
                    planted.insert((i, j));
                    out.push(TreeInstance {
                        x: wx,
                        z: wz,
                        radius,
                        height: lerp(profile.height.0, profile.height.1, rand01(h, 3)),
                        tone: rand01(h, 4),
                        luma: 1.0 + (rand01(h, 5) - 0.5) * 0.08,
                    });
                    if out.len() >= MAX_TREES_PER_CELL {
                        truncated = true;
                        break;
                    }
                }
                if truncated {
                    break;
                }
            }
            if truncated {
                break;
            }
        }
        if truncated {
            break;
        }
    }

    if truncated {
        tracing::warn!(
            cap = MAX_TREES_PER_CELL,
            "vegetation scatter hit the per-cell cap; this cell is planted short"
        );
    }
    Ok(out)
}

/// Everywhere a tree must not stand because there is water there.
///
/// Two tests, because a bank needs both: **inside** any water polygon (a pond in
/// the middle of a park is interior to the park, so no boundary inset can find
/// it), and **within a canopy radius of** a water edge, so the crown does not
/// overhang the quay.
struct WaterMask {
    tris: Vec<[(f32, f32); 3]>,
    buckets: HashMap<(i32, i32), Vec<u32>>,
    edges: EdgeGrid,
}

impl WaterMask {
    const CELL: f32 = 16.0;

    fn build(tile: &TileData, scale: f32, extent: f32, tile_m: f32) -> Self {
        use crate::systems::vector_tiles::{clipped_rings, tessellate_polygon};
        use geo_types::Geometry;

        let border = BorderGuard::new(tile_m);
        let mut rings: Vec<Vec<[f32; 2]>> = Vec::new();
        let mut positions = Vec::new();
        let mut indices = Vec::new();

        for meta in tile.layers() {
            if !matches!(meta.name.as_str(), "water_polygons" | "ocean") {
                continue;
            }
            for feature in tile.features(meta) {
                let polys: Vec<&geo_types::Polygon<f32>> = match &feature.geometry {
                    Geometry::Polygon(p) => vec![p],
                    Geometry::MultiPolygon(mp) => mp.iter().collect(),
                    _ => continue,
                };
                for p in polys {
                    tessellate_polygon(p, scale, extent, &mut positions, &mut indices);
                    rings.extend(clipped_rings(p, scale, extent));
                }
            }
        }

        let mut mask = Self {
            tris: Vec::new(),
            buckets: HashMap::new(),
            edges: EdgeGrid::new(&rings, &border),
        };
        for tri in indices.chunks_exact(3) {
            let v: Vec<(f32, f32)> = tri
                .iter()
                .map(|&i| (positions[i as usize][0], positions[i as usize][2]))
                .collect();
            let idx = mask.tris.len() as u32;
            mask.tris.push([v[0], v[1], v[2]]);
            let (minx, maxx) = min_max(v[0].0, v[1].0, v[2].0);
            let (minz, maxz) = min_max(v[0].1, v[1].1, v[2].1);
            for i in (minx / Self::CELL).floor() as i32..=(maxx / Self::CELL).floor() as i32 {
                for j in (minz / Self::CELL).floor() as i32..=(maxz / Self::CELL).floor() as i32 {
                    mask.buckets.entry((i, j)).or_default().push(idx);
                }
            }
        }
        mask
    }

    /// True if a tree of canopy radius `r` here would stand in, or lean over, water.
    fn blocks(&self, x: f32, z: f32, r: f32) -> bool {
        let key = (
            (x / Self::CELL).floor() as i32,
            (z / Self::CELL).floor() as i32,
        );
        if let Some(ids) = self.buckets.get(&key) {
            for &id in ids {
                let t = self.tris[id as usize];
                if in_triangle((x, z), t[0], t[1], t[2]) {
                    return true;
                }
            }
        }
        self.edges.within(x, z, r)
    }
}

/// Recognises the tile border, where a polygon's "edge" is an artefact of the
/// clamp rather than a real boundary.
///
/// Without this the inset carves a bald 3-metre strip along every cell seam, and
/// the seams are on a 1.5 km grid across the whole map — far more visible than the
/// overhang the inset exists to prevent.
struct BorderGuard {
    tile_m: f32,
}

impl BorderGuard {
    fn new(tile_m: f32) -> Self {
        Self { tile_m }
    }

    /// True if the whole segment lies along one of the four tile edges.
    fn is_border(&self, a: [f32; 2], b: [f32; 2]) -> bool {
        const EPS: f32 = 0.5;
        let on = |v: f32, at: f32| (v - at).abs() < EPS;
        (on(a[0], 0.0) && on(b[0], 0.0))
            || (on(a[0], self.tile_m) && on(b[0], self.tile_m))
            || (on(a[1], 0.0) && on(b[1], 0.0))
            || (on(a[1], self.tile_m) && on(b[1], self.tile_m))
    }
}

/// A uniform grid over a feature's boundary segments, so the inset test costs a
/// 3×3 cell lookup per candidate instead of a scan over every ring point.
struct EdgeGrid {
    cell: f32,
    segs: Vec<([f32; 2], [f32; 2])>,
    buckets: HashMap<(i32, i32), Vec<u32>>,
}

impl EdgeGrid {
    const CELL: f32 = 8.0;

    fn new(rings: &[Vec<[f32; 2]>], border: &BorderGuard) -> Self {
        let mut grid = Self {
            cell: Self::CELL,
            segs: Vec::new(),
            buckets: HashMap::new(),
        };
        for ring in rings {
            for w in ring.windows(2) {
                if border.is_border(w[0], w[1]) {
                    continue;
                }
                let idx = grid.segs.len() as u32;
                grid.segs.push((w[0], w[1]));
                let (minx, maxx) = (w[0][0].min(w[1][0]), w[0][0].max(w[1][0]));
                let (minz, maxz) = (w[0][1].min(w[1][1]), w[0][1].max(w[1][1]));
                for i in (minx / Self::CELL).floor() as i32..=(maxx / Self::CELL).floor() as i32 {
                    for j in (minz / Self::CELL).floor() as i32..=(maxz / Self::CELL).floor() as i32
                    {
                        grid.buckets.entry((i, j)).or_default().push(idx);
                    }
                }
            }
        }
        grid
    }

    /// True if any boundary segment passes within `r` of the point.
    fn within(&self, x: f32, z: f32, r: f32) -> bool {
        let lo_i = ((x - r) / self.cell).floor() as i32;
        let hi_i = ((x + r) / self.cell).floor() as i32;
        let lo_j = ((z - r) / self.cell).floor() as i32;
        let hi_j = ((z + r) / self.cell).floor() as i32;
        let r2 = r * r;
        for i in lo_i..=hi_i {
            for j in lo_j..=hi_j {
                let Some(ids) = self.buckets.get(&(i, j)) else {
                    continue;
                };
                for &id in ids {
                    let (a, b) = self.segs[id as usize];
                    if point_seg_dist2(x, z, a, b) < r2 {
                        return true;
                    }
                }
            }
        }
        false
    }
}

/// Angle of the longest boundary segment — the orchard's row direction.
fn dominant_angle(rings: &[Vec<[f32; 2]>]) -> f32 {
    let mut best = 0.0f32;
    let mut angle = 0.0f32;
    for ring in rings {
        for w in ring.windows(2) {
            let (dx, dz) = (w[1][0] - w[0][0], w[1][1] - w[0][1]);
            let len = dx * dx + dz * dz;
            if len > best {
                best = len;
                angle = dz.atan2(dx);
            }
        }
    }
    angle
}

// ─── Meshing ─────────────────────────────────────────────────────────────────

/// Turn scattered instances into merged [`SurfaceMesh`]es — at most one canopy
/// mesh and one trunk mesh per cell, whatever the tree count.
#[must_use]
pub fn build_vegetation_meshes(trees: &[TreeInstance]) -> Vec<SurfaceMesh> {
    if trees.is_empty() {
        return Vec::new();
    }
    let unit = unit_canopy();
    let deep = tone_multiplier();

    let mut canopy = MeshBuf::default();
    let mut trunk = MeshBuf::default();

    for t in trees {
        // Crown occupies the top 70% of the tree; the trunk carries the rest.
        let crown_bottom = t.height * 0.30;
        let ry = (t.height - crown_bottom) * 0.5;
        let cy = crown_bottom + ry;
        let (rx, rz) = (t.radius, t.radius);

        let tint = [
            lerp(1.0, deep[0], t.tone) * t.luma,
            lerp(1.0, deep[1], t.tone) * t.luma,
            lerp(1.0, deep[2], t.tone) * t.luma,
            1.0,
        ];

        let base = canopy.positions.len() as u32;
        for p in &unit.verts {
            canopy
                .positions
                .push([t.x + p[0] * rx, cy + p[1] * ry, t.z + p[2] * rz]);
            // Inverse-transpose of a non-uniform scale is the reciprocal scale, or
            // an ellipsoid's normals point the way a sphere's would and the shading
            // gives away that every tree is the same ball.
            let n = normalize([p[0] / rx, p[1] / ry, p[2] / rz]);
            canopy.normals.push(n);
            canopy.colors.push(tint);
        }
        canopy.indices.extend(unit.indices.iter().map(|i| base + i));

        if t.height >= TRUNK_MIN_HEIGHT {
            let hw = (t.radius * 0.09).max(0.08);
            // Overlap into the crown so no gap opens at the join from below.
            let top = crown_bottom + ry * 0.35;
            let tint = [t.luma, t.luma, t.luma, 1.0];
            push_trunk(&mut trunk, t.x, t.z, hw, top, tint);
        }
    }

    let mut out = vec![canopy.finish(SurfaceGroup::Canopy)];
    if !trunk.positions.is_empty() {
        out.push(trunk.finish(SurfaceGroup::Trunk));
    }
    out
}

#[derive(Default)]
struct MeshBuf {
    positions: Vec<[f32; 3]>,
    normals: Vec<[f32; 3]>,
    colors: Vec<[f32; 4]>,
    indices: Vec<u32>,
}

impl MeshBuf {
    fn finish(self, group: SurfaceGroup) -> SurfaceMesh {
        SurfaceMesh {
            group,
            positions: self.positions,
            indices: self.indices,
            uvs: Vec::new(),
            normals: self.normals,
            colors: self.colors,
        }
    }
}

/// A square prism, open at both ends — the crown caps the top and the ground the
/// bottom, so caps would only be triangles nobody ever sees.
fn push_trunk(buf: &mut MeshBuf, x: f32, z: f32, hw: f32, top: f32, tint: [f32; 4]) {
    const SIDES: [([f32; 2], [f32; 2], [f32; 3]); 4] = [
        ([-1.0, -1.0], [1.0, -1.0], [0.0, 0.0, -1.0]),
        ([1.0, -1.0], [1.0, 1.0], [1.0, 0.0, 0.0]),
        ([1.0, 1.0], [-1.0, 1.0], [0.0, 0.0, 1.0]),
        ([-1.0, 1.0], [-1.0, -1.0], [-1.0, 0.0, 0.0]),
    ];
    for (a, b, n) in SIDES {
        let base = buf.positions.len() as u32;
        for (c, y) in [(a, 0.0), (b, 0.0), (b, top), (a, top)] {
            buf.positions.push([x + c[0] * hw, y, z + c[1] * hw]);
            buf.normals.push(n);
            buf.colors.push(tint);
        }
        buf.indices
            .extend([base, base + 2, base + 1, base, base + 3, base + 2]);
    }
}

/// The shared unit canopy: an octahedron subdivided once and pushed out to the
/// unit sphere — 18 vertices and 32 triangles, smooth-shaded off the normalised
/// position. Small enough to spend on every tree, round enough that a canopy seen
/// from directly overhead still reads as a canopy.
struct UnitCanopy {
    verts: Vec<[f32; 3]>,
    indices: Vec<u32>,
}

fn unit_canopy() -> UnitCanopy {
    const OCT: [[f32; 3]; 6] = [
        [1.0, 0.0, 0.0],
        [-1.0, 0.0, 0.0],
        [0.0, 1.0, 0.0],
        [0.0, -1.0, 0.0],
        [0.0, 0.0, 1.0],
        [0.0, 0.0, -1.0],
    ];
    const FACES: [[usize; 3]; 8] = [
        [0, 2, 4],
        [2, 1, 4],
        [1, 3, 4],
        [3, 0, 4],
        [2, 0, 5],
        [1, 2, 5],
        [3, 1, 5],
        [0, 3, 5],
    ];

    let mut verts: Vec<[f32; 3]> = OCT.to_vec();
    let mut midpoints: HashMap<(usize, usize), usize> = HashMap::new();
    let mut indices = Vec::with_capacity(32 * 3);

    for f in FACES {
        let mut mid = |a: usize, b: usize, verts: &mut Vec<[f32; 3]>| -> usize {
            let key = if a < b { (a, b) } else { (b, a) };
            *midpoints.entry(key).or_insert_with(|| {
                let (p, q) = (verts[a], verts[b]);
                verts.push(normalize([
                    p[0] + q[0],
                    p[1] + q[1],
                    p[2] + q[2],
                ]));
                verts.len() - 1
            })
        };
        let ab = mid(f[0], f[1], &mut verts);
        let bc = mid(f[1], f[2], &mut verts);
        let ca = mid(f[2], f[0], &mut verts);
        for t in [
            [f[0], ab, ca],
            [ab, f[1], bc],
            [ca, bc, f[2]],
            [ab, bc, ca],
        ] {
            indices.extend(t.iter().map(|&i| i as u32));
        }
    }

    UnitCanopy { verts, indices }
}

// ─── Altitude ramp ───────────────────────────────────────────────────────────

/// How far the trees have merged into the flat fill: 0 = individual trees, 1 =
/// fully merged. Read at spawn by [`crate::systems::map_geometry`] so a cell that
/// streams in above the band does not flash a forest for one frame.
#[derive(Resource)]
pub struct VegetationLod {
    pub factor: f32,
    /// Mirror of `DataLayers::trees_enabled`, so [`Self::visibility`] (read at
    /// spawn) reflects the Trees toggle and a new cell doesn't flash a forest
    /// that the user has switched off.
    pub enabled: bool,
}

impl Default for VegetationLod {
    fn default() -> Self {
        Self { factor: 0.0, enabled: true }
    }
}

impl VegetationLod {
    /// Whether newly spawned vegetation should be visible at all: off if the Trees
    /// layer is disabled, or if the altitude LOD has fully merged the canopy away.
    #[must_use]
    pub fn visibility(&self) -> Visibility {
        if !self.enabled || self.factor >= 1.0 {
            Visibility::Hidden
        } else {
            Visibility::Inherited
        }
    }
}

/// Marker on the canopy/trunk entities, so the ramp can hide them wholesale.
#[derive(Component)]
pub struct VegetationSurface;

/// Ramp the trees into the fill with altitude.
///
/// Nothing here switches on a threshold, for the same reason the space tier
/// crossfades on `space_factor` instead of on `in_space`: a step in brightness in
/// the middle of a zoom is far more noticeable than the detail it is hiding.
///
/// The trick that makes it seam-free is that **two** colours move at once:
///
/// * the `Forest` fill ramps `FOREST → FOREST_CANOPIED`, because a forest seen
///   from far enough that its trees have merged *is* darker than bare woodland
///   floor — hiding the trees without this brightens every wood by a visible step;
/// * the canopy ramps toward whatever the fill currently is, so by the end of the
///   band a canopy is exactly the colour of the ground under it. It has no
///   silhouette left to lose, which is what lets the geometry be hidden at
///   `factor == 1` with nothing visibly happening.
///
/// No transparency is involved: alpha on a hundred thousand triangles would cost a
/// sort order and a pass, to fade something that is by then sub-pixel anyway.
pub fn update_vegetation_lod(
    mut lod: ResMut<VegetationLod>,
    dl: Res<crate::systems::data_layers::DataLayers>,
    mut materials: ResMut<Assets<StandardMaterial>>,
    surfaces: Res<MapSurfaces>,
    globe: Res<GlobeState>,
    camera: Query<(&GridCell, &Transform), With<FreeCamera>>,
    mut veg: Query<&mut Visibility, With<VegetationSurface>>,
    mut last: Local<Option<(f32, bool)>>,
) {
    let Ok((cell, tf)) = camera.single() else {
        return;
    };
    let alt = globe.altitude_m(cell, tf) as f32;
    let t = ((alt - TREE_FADE_START) / (TREE_FADE_END - TREE_FADE_START)).clamp(0.0, 1.0);
    // Smoothstep: the ends of the band matter more than its middle, because that
    // is where a linear ramp shows its corners.
    let factor = t * t * (3.0 - 2.0 * t);
    let enabled = dl.trees_enabled;

    let previous = *last;
    // Re-run when the ramp moves *or* the Trees toggle flips (same altitude, but
    // the geometry has to appear/disappear).
    if previous.is_some_and(|(p, e)| (p - factor).abs() < 0.002 && e == enabled) {
        return;
    }
    *last = Some((factor, enabled));
    lod.factor = factor;
    lod.enabled = enabled;

    let fill = mix(palette::FOREST, palette::FOREST_CANOPIED, factor);
    set_base_color(&mut materials, &surfaces, SurfaceGroup::Forest, fill);
    set_base_color(
        &mut materials,
        &surfaces,
        SurfaceGroup::Canopy,
        mix_rgb(srgb_to_linear_rgb(palette::CANOPY), fill, factor),
    );
    set_base_color(
        &mut materials,
        &surfaces,
        SurfaceGroup::Trunk,
        mix_rgb(srgb_to_linear_rgb(palette::TRUNK), fill, factor),
    );

    // Only touch visibility when the hidden state actually flips — the far end of
    // the band, or the Trees toggle — leaving thousands of entities alone otherwise.
    let was_hidden = previous.is_some_and(|(p, e)| !e || p >= 1.0);
    let is_hidden = !enabled || factor >= 1.0;
    if previous.is_none() || was_hidden != is_hidden {
        let v = if is_hidden {
            Visibility::Hidden
        } else {
            Visibility::Inherited
        };
        for mut vis in &mut veg {
            *vis = v;
        }
    }
}

fn set_base_color(
    materials: &mut Assets<StandardMaterial>,
    surfaces: &MapSurfaces,
    group: SurfaceGroup,
    linear: [f32; 3],
) {
    let Some(handle) = surfaces.material(group) else {
        return;
    };
    let Some(mut mat) = materials.get_mut(handle) else {
        return;
    };
    mat.base_color = Color::linear_rgb(linear[0], linear[1], linear[2]);
}

// ─── Small maths ─────────────────────────────────────────────────────────────

/// The vertex-colour multiplier that turns [`palette::CANOPY`] into
/// [`palette::CANOPY_DEEP`]. Computed in **linear** space, which is where the
/// shader multiplies, so the second tone lands on the palette value rather than
/// near it.
fn tone_multiplier() -> [f32; 3] {
    let a = srgb_to_linear_rgb(palette::CANOPY);
    let b = srgb_to_linear_rgb(palette::CANOPY_DEEP);
    [b[0] / a[0], b[1] / a[1], b[2] / a[2]]
}

pub(crate) fn srgb_to_linear_rgb(rgb: u32) -> [f32; 3] {
    let ch = |shift: u32| {
        let s = ((rgb >> shift) & 0xFF) as f32 / 255.0;
        if s <= 0.040_45 {
            s / 12.92
        } else {
            ((s + 0.055) / 1.055).powf(2.4)
        }
    };
    [ch(16), ch(8), ch(0)]
}

pub(crate) fn mix(a: u32, b: u32, t: f32) -> [f32; 3] {
    mix_rgb(srgb_to_linear_rgb(a), srgb_to_linear_rgb(b), t)
}

fn mix_rgb(a: [f32; 3], b: [f32; 3], t: f32) -> [f32; 3] {
    [
        lerp(a[0], b[0], t),
        lerp(a[1], b[1], t),
        lerp(a[2], b[2], t),
    ]
}

fn lerp(a: f32, b: f32, t: f32) -> f32 {
    a + (b - a) * t
}

fn normalize(v: [f32; 3]) -> [f32; 3] {
    let len = (v[0] * v[0] + v[1] * v[1] + v[2] * v[2]).sqrt();
    if len <= f32::EPSILON {
        [0.0, 1.0, 0.0]
    } else {
        [v[0] / len, v[1] / len, v[2] / len]
    }
}

fn min_max(a: f32, b: f32, c: f32) -> (f32, f32) {
    (a.min(b).min(c), a.max(b).max(c))
}

fn in_triangle(p: (f32, f32), a: (f32, f32), b: (f32, f32), c: (f32, f32)) -> bool {
    let d = (b.1 - c.1) * (a.0 - c.0) + (c.0 - b.0) * (a.1 - c.1);
    if d.abs() < 1e-9 {
        return false;
    }
    let l1 = ((b.1 - c.1) * (p.0 - c.0) + (c.0 - b.0) * (p.1 - c.1)) / d;
    let l2 = ((c.1 - a.1) * (p.0 - c.0) + (a.0 - c.0) * (p.1 - c.1)) / d;
    let l3 = 1.0 - l1 - l2;
    l1 >= 0.0 && l2 >= 0.0 && l3 >= 0.0
}

fn point_seg_dist2(x: f32, z: f32, a: [f32; 2], b: [f32; 2]) -> f32 {
    let (dx, dz) = (b[0] - a[0], b[1] - a[1]);
    let len2 = dx * dx + dz * dz;
    let t = if len2 <= f32::EPSILON {
        0.0
    } else {
        (((x - a[0]) * dx + (z - a[1]) * dz) / len2).clamp(0.0, 1.0)
    };
    let (px, pz) = (a[0] + dx * t, a[1] + dz * t);
    (x - px) * (x - px) + (z - pz) * (z - pz)
}

/// Deterministic per-lattice-cell hash. `rand01` pulls independent streams out of
/// it by byte, which is enough decorrelation for jitter and size.
pub(crate) fn hash3(seed: u64, i: i32, j: i32) -> u64 {
    let mut h = seed ^ (i as u32 as u64).wrapping_mul(0xA24B_AED4_963E_E407);
    h ^= (j as u32 as u64).wrapping_mul(0x9FB2_1C65_1E98_DF25);
    h ^= h >> 29;
    h = h.wrapping_mul(0xBF58_476D_1CE4_E5B9);
    h ^= h >> 32;
    h = h.wrapping_mul(0x94D0_49BB_1331_11EB);
    h ^ (h >> 31)
}

pub(crate) fn rand01(h: u64, stream: u32) -> f32 {
    let mut x = h.rotate_left(stream * 11 + 7);
    x ^= x >> 33;
    x = x.wrapping_mul(0xFF51_AFD7_ED55_8CCD);
    x ^= x >> 33;
    ((x >> 40) as f32) / 16_777_216.0
}

#[cfg(test)]
mod tests {
    use super::*;

    const GRONINGEN_Z14: &[u8] = include_bytes!(concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fixtures/groningen_z14.mvt"));

    /// The fixture, parsed. The scatter takes a shared [`TileData`] now (one parse
    /// per streamed cell, shared with the tessellators — `TileData`'s docs say
    /// why), so each test that wants it in isolation makes its own.
    fn fixture() -> TileData {
        TileData::parse(GRONINGEN_Z14).expect("parse fixture")
    }

    /// The fixture is a city tile with woodland and parks in it, so the scatter
    /// must produce trees, keep them inside the tile, and stay under the cap.
    #[test]
    fn scatters_trees_over_the_fixture() {
        let tile_m = 1472.0;
        let trees = scatter_vegetation(&fixture(), tile_m, 42).expect("scatter");
        assert!(!trees.is_empty(), "no trees scattered over a tile with parks");
        assert!(trees.len() <= MAX_TREES_PER_CELL);
        for t in &trees {
            assert!(
                t.x >= -1.0 && t.x <= tile_m + 1.0 && t.z >= -1.0 && t.z <= tile_m + 1.0,
                "tree outside the tile at ({}, {})",
                t.x,
                t.z
            );
            assert!(t.height > 0.5 && t.height < 20.0, "implausible height {}", t.height);
            assert!(t.radius > 0.5 && t.radius < 6.0, "implausible radius {}", t.radius);
        }
    }

    /// No tree may stand in water. Groningen's parks have the Noorderhaven and
    /// several ponds *inside* the landuse polygon, so the first render grew a wood
    /// down the middle of a canal — insetting from the park's own boundary cannot
    /// see water that is interior to it.
    #[test]
    fn no_tree_stands_in_water() {
        let tile_m = 1472.0;
        let tile = TileData::parse(GRONINGEN_Z14).expect("parse");
        let extent = tile.layer("water_polygons").map_or(4096.0, TileData::extent);
        let mask = WaterMask::build(&tile, tile_m / extent, extent, tile_m);
        assert!(!mask.tris.is_empty(), "fixture has no water to test against");

        let trees = scatter_vegetation(&tile, tile_m, 42).expect("scatter");
        for t in &trees {
            assert!(
                !mask.blocks(t.x, t.z, 0.0),
                "tree planted in water at ({}, {})",
                t.x,
                t.z
            );
        }
    }

    /// Same seed → same forest. A cell that streams out and back must regrow
    /// identically, or the ring moving over a boundary shimmers.
    #[test]
    fn scatter_is_deterministic() {
        let a = scatter_vegetation(&fixture(), 1472.0, 7).expect("a");
        let b = scatter_vegetation(&fixture(), 1472.0, 7).expect("b");
        assert_eq!(a.len(), b.len());
        for (x, y) in a.iter().zip(&b) {
            assert_eq!(x.x.to_bits(), y.x.to_bits());
            assert_eq!(x.z.to_bits(), y.z.to_bits());
            assert_eq!(x.height.to_bits(), y.height.to_bits());
        }
    }

    /// …and a different cell must not grow the *same* forest, or the map tiles
    /// visibly repeat.
    #[test]
    fn scatter_varies_with_the_seed() {
        let a = scatter_vegetation(&fixture(), 1472.0, 1).expect("a");
        let b = scatter_vegetation(&fixture(), 1472.0, 2).expect("b");
        let same = a
            .iter()
            .zip(&b)
            .filter(|(x, y)| x.x.to_bits() == y.x.to_bits() && x.z.to_bits() == y.z.to_bits())
            .count();
        assert!(
            same * 4 < a.len(),
            "{same}/{} trees identical across seeds",
            a.len()
        );
    }

    #[test]
    fn builds_merged_meshes_with_normals_and_colours() {
        let trees = scatter_vegetation(&fixture(), 1472.0, 3).expect("scatter");
        let meshes = build_vegetation_meshes(&trees);
        assert!(!meshes.is_empty());
        // One mesh per group, never one per tree — this is the device-loss guard.
        assert!(meshes.len() <= 2, "{} meshes for one cell", meshes.len());
        for m in &meshes {
            assert_eq!(m.normals.len(), m.positions.len());
            assert_eq!(m.colors.len(), m.positions.len());
            assert_eq!(m.indices.len() % 3, 0);
            assert!(m.indices.iter().all(|&i| (i as usize) < m.positions.len()));
            for n in &m.normals {
                let len = (n[0] * n[0] + n[1] * n[1] + n[2] * n[2]).sqrt();
                assert!((len - 1.0).abs() < 1e-3, "unnormalised normal {n:?}");
            }
        }
    }

    #[test]
    fn empty_scatter_builds_nothing() {
        assert!(build_vegetation_meshes(&[]).is_empty());
    }

    /// The unit canopy is the per-tree cost multiplier — if it silently grows, so
    /// does every cell's triangle count.
    #[test]
    fn unit_canopy_is_small() {
        let u = unit_canopy();
        assert_eq!(u.verts.len(), 18);
        assert_eq!(u.indices.len(), 32 * 3);
    }

    /// What a real cell actually costs. `cargo test -p viberfox --bin viberfox
    /// dump_vegetation_cost -- --ignored --nocapture`.
    #[test]
    #[ignore]
    fn dump_vegetation_cost() {
        let tile_m = 1472.0;
        let trees = scatter_vegetation(&fixture(), tile_m, 42).expect("scatter");
        let meshes = build_vegetation_meshes(&trees);
        println!("trees            : {}", trees.len());
        println!(
            "per km²          : {:.0}",
            trees.len() as f32 / (tile_m * tile_m / 1_000_000.0)
        );
        for m in &meshes {
            println!(
                "{:?}: {} verts, {} tris",
                m.group,
                m.positions.len(),
                m.indices.len() / 3
            );
        }
    }

    /// The deep tone must be a genuine darkening, and mild — the whole point of
    /// the palette work is that trees stay inside the map's pastel range.
    #[test]
    fn tone_multiplier_darkens_gently() {
        let m = tone_multiplier();
        for c in m {
            assert!(c < 1.0, "deep tone is not darker: {m:?}");
            assert!(c > 0.6, "deep tone is too strong a step: {m:?}");
        }
    }
}
