//! Tree scattering and canopy meshing — the pure half of the vegetation layer.
//!
//! No Bevy here: this turns tile geometry into [`TreeInstance`]s and then into
//! meshes. The ECS half — the altitude ramp, the spawn visibility and the
//! materials — is `systems::vegetation`.
//!
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

use crate::vector_tiles::{palette, SurfaceGroup, SurfaceMesh, TileData, TILE_BUFFER_FRAC};

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
    use crate::vector_tiles::{clipped_rings, kind_of, tessellate_polygon};
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
    // Streets are interior to the landuse for exactly the same reason, and the
    // consequence is the same: a wood is not cut out where a road runs through it.
    let roads = RoadMask::build(tile, tile_m);

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
            // An array, not a `Vec`: three elements of known count, in a loop that
            // runs once per triangle of every landuse polygon in the tile. See
            // `turf::grass_areas` for the same loop written the same way.
            let v = [
                {
                    let p = positions[tri[0] as usize];
                    to_lattice(p[0], p[2])
                },
                {
                    let p = positions[tri[1] as usize];
                    to_lattice(p[0], p[2])
                },
                {
                    let p = positions[tri[2] as usize];
                    to_lattice(p[0], p[2])
                },
            ];
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
                    // edge against a canal must not hang trees over the water —
                    // and off the carriageways that run through it.
                    if edges.within(wx, wz, radius * 0.8)
                        || water.blocks(wx, wz, radius * 0.8)
                        || roads.blocks(wx, wz, radius * 0.8)
                    {
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
pub struct WaterMask {
    tris: Vec<[(f32, f32); 3]>,
    buckets: HashMap<(i32, i32), Vec<u32>>,
    edges: EdgeGrid,
}

impl WaterMask {
    const CELL: f32 = 16.0;

    /// A mask over a tile with nothing to keep out of. Both masks walk a whole
    /// layer to build, so a caller that already knows there is nothing to plant
    /// has somewhere cheap to land (`crate::grass`).
    pub(crate) fn empty(tile_m: f32) -> Self {
        Self {
            tris: Vec::new(),
            buckets: HashMap::new(),
            edges: EdgeGrid::new(&[], &BorderGuard::new(tile_m)),
        }
    }

    pub(crate) fn build(tile: &TileData, scale: f32, extent: f32, tile_m: f32) -> Self {
        use crate::vector_tiles::{clipped_rings, tessellate_polygon};
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
            // Array rather than `Vec`, as in `scatter_vegetation` above: one
            // allocation per triangle of every water polygon is the exact
            // regression shape `perf_budgets` exists to catch.
            let v = [
                (positions[tri[0] as usize][0], positions[tri[0] as usize][2]),
                (positions[tri[1] as usize][0], positions[tri[1] as usize][2]),
                (positions[tri[2] as usize][0], positions[tri[2] as usize][2]),
            ];
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
    pub(crate) fn blocks(&self, x: f32, z: f32, r: f32) -> bool {
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

/// Everywhere a tree must not stand because a street is drawn there.
///
/// The sibling of [`WaterMask`], and it exists for the identical reason: `land`
/// and `streets` are independent layers, so a wood is **not** cut out where a road
/// runs through it — Shortbread draws the ribbon on top. Insetting from the
/// polygon's own boundary cannot see a lane that is interior to it, so the scatter
/// planted trees down the middle of every road through a forest and every path
/// across a park.
///
/// The keep-out is the **carriageway**, not the centre line: each segment carries
/// the same half-width [`tessellate_roads`](crate::vector_tiles::tessellate_roads)
/// builds its ribbon from, so a motorway clears eleven metres and a footpath
/// clears one, from one table. Tunnels are absent by construction —
/// `drawn_street` refuses them, and there is nothing wrong with a tree over a
/// tunnel.
pub struct RoadMask {
    /// Segment endpoints in tile-local metres, with the half-width of the street
    /// they belong to.
    segs: Vec<([f32; 2], [f32; 2], f32)>,
    buckets: HashMap<(i32, i32), Vec<u32>>,
    /// The cell's ground size, which is what bounds the bucket grid — see
    /// [`bucket_range`].
    tile_m: f32,
}

impl RoadMask {
    /// Bucket size in metres. Wider than [`EdgeGrid`]'s because a segment is
    /// registered over its half-width as well as its length, and a motorway's
    /// eleven metres would otherwise smear every segment over a 3×3 neighbourhood.
    const CELL: f32 = 32.0;

    /// The sibling of [`WaterMask::empty`], and there for the same reason.
    pub(crate) fn empty(tile_m: f32) -> Self {
        Self {
            segs: Vec::new(),
            buckets: HashMap::new(),
            tile_m,
        }
    }

    pub(crate) fn build(tile: &TileData, tile_m: f32) -> Self {
        use crate::vector_tiles::drawn_street;
        use geo_types::Geometry;

        let mut mask = Self {
            segs: Vec::new(),
            buckets: HashMap::new(),
            tile_m,
        };
        let Some(meta) = tile.layer("streets") else {
            return mask;
        };
        let scale = tile_m / TileData::extent(meta);
        // The window a street segment has to lie in to be this tile's business at
        // all. It replaces an `is_finite` filter on the points, which let a
        // coordinate near `i32::MAX` — all `mvt-reader` guarantees, see
        // `TILE_BUFFER_FRAC` — straight into `push`, where the bbox loop then
        // tried to insert on the order of 10^14 buckets and died in the
        // allocator. `trees_enabled` defaults on, so that was the *default* path
        // on a body nobody here authored.
        //
        // Testing the segment rather than filtering the points is also the
        // stricter reading: dropping a bad point from the list silently welds its
        // two neighbours together into a street that was never in the data.
        let margin = tile_m * TILE_BUFFER_FRAC;
        let inside = |p: [f32; 2]| {
            // Written as a positive test so a NaN — which compares false against
            // everything — is rejected rather than admitted.
            p[0] >= -margin && p[0] <= tile_m + margin && p[1] >= -margin && p[1] <= tile_m + margin
        };

        for feature in tile.features(meta) {
            let Some((_, half)) = drawn_street(feature) else {
                continue;
            };
            let lines: Vec<&geo_types::LineString<f32>> = match &feature.geometry {
                Geometry::LineString(ls) => vec![ls],
                Geometry::MultiLineString(mls) => mls.iter().collect(),
                _ => continue,
            };
            for ls in lines {
                let pts: Vec<[f32; 2]> = ls.0.iter().map(|c| [c.x * scale, c.y * scale]).collect();
                for w in pts.windows(2) {
                    if !inside(w[0]) || !inside(w[1]) {
                        continue;
                    }
                    mask.push(w[0], w[1], half);
                }
            }
        }
        mask
    }

    /// Register one segment over every cell its carriageway touches — the bbox
    /// grown by the half-width, so [`Self::blocks`] only ever has to widen the
    /// *query* by the canopy radius.
    fn push(&mut self, a: [f32; 2], b: [f32; 2], half: f32) {
        let idx = self.segs.len() as u32;
        self.segs.push((a, b, half));
        let (minx, maxx) = (a[0].min(b[0]) - half, a[0].max(b[0]) + half);
        let (minz, maxz) = (a[1].min(b[1]) - half, a[1].max(b[1]) + half);
        for i in bucket_range(minx, maxx, Self::CELL, self.tile_m) {
            for j in bucket_range(minz, maxz, Self::CELL, self.tile_m) {
                self.buckets.entry((i, j)).or_default().push(idx);
            }
        }
    }

    /// True if a tree of canopy radius `r` here would stand on, or lean over, a
    /// carriageway.
    #[must_use]
    pub fn blocks(&self, x: f32, z: f32, r: f32) -> bool {
        for i in ((x - r) / Self::CELL).floor() as i32..=((x + r) / Self::CELL).floor() as i32 {
            for j in ((z - r) / Self::CELL).floor() as i32..=((z + r) / Self::CELL).floor() as i32 {
                let Some(ids) = self.buckets.get(&(i, j)) else {
                    continue;
                };
                for &id in ids {
                    let (a, b, half) = self.segs[id as usize];
                    let reach = half + r;
                    if point_seg_dist2(x, z, a, b) < reach * reach {
                        return true;
                    }
                }
            }
        }
        false
    }
}

/// The inclusive range of grid cells a span covers, clamped to the tile's own
/// grid.
///
/// The clamp is the second half of the guard the segment window in
/// [`RoadMask::build`] is the first half of, and it is deliberately redundant with
/// it: a bucket loop is the one place in this file where a single bad number does
/// not produce a wrong answer but an unbounded amount of *work*, and `as i32` on a
/// float saturates rather than wrapping, so an unclamped `(2.1e9 / 32).floor() as
/// i32` is `i32::MAX` and the loop below never finishes. Nothing outside the tile
/// plus its buffer can ever be queried — [`RoadMask::blocks`] is asked about tree
/// positions, which come from the tile's own polygons — so cells beyond that are
/// not just unsafe, they are unreachable.
///
/// An empty range (`lo > hi`) is the natural answer for a span entirely outside,
/// and `for i in lo..=hi` simply does not run.
pub(crate) fn bucket_range(
    min: f32,
    max: f32,
    cell: f32,
    tile_m: f32,
) -> std::ops::RangeInclusive<i32> {
    let limit = ((tile_m * (1.0 + TILE_BUFFER_FRAC)) / cell).ceil() as i32;
    let lo = ((min / cell).floor() as i32).max(-limit);
    let hi = ((max / cell).floor() as i32).min(limit);
    lo..=hi
}

/// Recognises the tile border, where a polygon's "edge" is an artefact of the
/// clamp rather than a real boundary.
///
/// Without this the inset carves a bald 3-metre strip along every cell seam, and
/// the seams are on a 1.5 km grid across the whole map — far more visible than the
/// overhang the inset exists to prevent.
pub(crate) struct BorderGuard {
    tile_m: f32,
}

impl BorderGuard {
    pub(crate) fn new(tile_m: f32) -> Self {
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
pub(crate) struct EdgeGrid {
    cell: f32,
    segs: Vec<([f32; 2], [f32; 2])>,
    buckets: HashMap<(i32, i32), Vec<u32>>,
}

impl EdgeGrid {
    const CELL: f32 = 8.0;

    pub(crate) fn new(rings: &[Vec<[f32; 2]>], border: &BorderGuard) -> Self {
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
    pub(crate) fn within(&self, x: f32, z: f32, r: f32) -> bool {
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

/// Angle of the longest boundary segment — the orchard's row direction, and the
/// axis a car park's bays are laid out along ([`crate::parking`]).
pub(crate) fn dominant_angle(rings: &[Vec<[f32; 2]>]) -> f32 {
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

/// Turn scattered instances into merged [`SurfaceMesh`]es — canopies and trunks,
/// each cut into pieces no larger than `max_bytes`.
///
/// **It used to be exactly one canopy mesh per cell, whatever the tree count, and
/// that is what made it the largest single asset in the app.** A cell is planted
/// up to [`MAX_TREES_PER_CELL`], each canopy is an 18-vertex ball, and at the cap
/// that is one ~6 MB mesh — measured at `peak_asset_kb = 6103` on an Adreno 840
/// on 2026-08-19 and reproduced headlessly. A single asset is the one quantity a
/// per-frame upload budget cannot help with: bevy's `RenderAssetBytesPerFrame`
/// always writes one *whole* asset per frame so uploads can never stall, so an
/// asset over the budget **is** a frame over the budget. `lod22` learned this and
/// cuts its cells up; this is the same cut for the other producer that reaches
/// that size.
///
/// `max_bytes` is `None` on every rung without an upload ceiling, which restores
/// the old single-mesh behaviour exactly. It is the caller's number rather than a
/// constant here because it is the *frame's* budget, and this crate cannot see
/// `quality` (it may not depend on bevy).
///
/// A piece is never empty and a single tree is never split: the cut is taken
/// *between* trees, so the worst case is one tree's worth over the limit rather
/// than a canopy with no trunk or a ball missing a face.
#[must_use]
pub fn build_vegetation_meshes(
    trees: &[TreeInstance],
    max_bytes: Option<usize>,
) -> Vec<SurfaceMesh> {
    if trees.is_empty() {
        return Vec::new();
    }
    let unit = unit_canopy();
    let deep = tone_multiplier();

    // What one more tree adds, so the check below is "would this one overflow"
    // rather than "has it already", which would let every piece finish over the
    // limit it exists to stay under.
    let canopy_step = unit.verts.len() * VERTEX_BYTES + unit.indices.len() * INDEX_BYTES;
    const TRUNK_STEP: usize = 16 * VERTEX_BYTES + 24 * INDEX_BYTES;

    let mut out = Vec::new();
    let mut canopy = MeshBuf::default();
    let mut trunk = MeshBuf::default();

    for t in trees {
        // Flush before adding, and only when there is something to flush — a piece
        // has to hold at least one tree or a cap below `canopy_step` would emit
        // empty meshes forever.
        if let Some(max) = max_bytes {
            if canopy.bytes() + canopy_step > max && !canopy.positions.is_empty() {
                out.push(std::mem::take(&mut canopy).finish(SurfaceGroup::Canopy));
            }
            if trunk.bytes() + TRUNK_STEP > max && !trunk.positions.is_empty() {
                out.push(std::mem::take(&mut trunk).finish(SurfaceGroup::Trunk));
            }
        }

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

    if !canopy.positions.is_empty() {
        out.push(canopy.finish(SurfaceGroup::Canopy));
    }
    if !trunk.positions.is_empty() {
        out.push(trunk.finish(SurfaceGroup::Trunk));
    }
    out
}

use crate::vector_tiles::{INDEX_BYTES, VERTEX_BYTES};

#[derive(Default)]
struct MeshBuf {
    positions: Vec<[f32; 3]>,
    normals: Vec<[f32; 3]>,
    colors: Vec<[f32; 4]>,
    indices: Vec<u32>,
}

impl MeshBuf {
    /// What this buffer will cost to upload, in the units the frame budget is in.
    fn bytes(&self) -> usize {
        self.positions.len() * VERTEX_BYTES + self.indices.len() * INDEX_BYTES
    }

    fn finish(self, group: SurfaceGroup) -> SurfaceMesh {
        SurfaceMesh {
            group,
            positions: self.positions,
            indices: self.indices,
            uvs: Vec::new(),
            normals: self.normals,
            colors: self.colors,
            rungs: Vec::new(),
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
                verts.push(normalize([p[0] + q[0], p[1] + q[1], p[2] + q[2]]));
                verts.len() - 1
            })
        };
        let ab = mid(f[0], f[1], &mut verts);
        let bc = mid(f[1], f[2], &mut verts);
        let ca = mid(f[2], f[0], &mut verts);
        for t in [[f[0], ab, ca], [ab, f[1], bc], [ca, bc, f[2]], [ab, bc, ca]] {
            indices.extend(t.iter().map(|&i| i as u32));
        }
    }

    UnitCanopy { verts, indices }
}

// ─── Small maths ─────────────────────────────────────────────────────────────

/// The vertex-colour multiplier that turns [`palette::CANOPY`] into
/// [`palette::CANOPY_DEEP`]. Computed in **linear** space, which is where the
/// shader multiplies, so the second tone lands on the palette value rather than
/// near it.
pub fn tone_multiplier() -> [f32; 3] {
    let a = srgb_to_linear_rgb(palette::CANOPY);
    let b = srgb_to_linear_rgb(palette::CANOPY_DEEP);
    [b[0] / a[0], b[1] / a[1], b[2] / a[2]]
}

pub fn srgb_to_linear_rgb(rgb: u32) -> [f32; 3] {
    let ch = |shift: u32| srgb_channel_to_linear(((rgb >> shift) & 0xFF) as f32 / 255.0);
    [ch(16), ch(8), ch(0)]
}

/// [`srgb_to_linear_rgb`] for a colour already unpacked to `[f32; 3]` sRGB
/// components — the building colour tables are authored that way. Vertex colours
/// multiply the material's `base_color` in **linear** space, so a table value fed
/// through unconverted displays a stop too bright (near-white walls, salmon roofs).
pub fn srgb_to_linear_f32(c: [f32; 3]) -> [f32; 3] {
    [
        srgb_channel_to_linear(c[0]),
        srgb_channel_to_linear(c[1]),
        srgb_channel_to_linear(c[2]),
    ]
}

fn srgb_channel_to_linear(s: f32) -> f32 {
    if s <= 0.040_45 {
        s / 12.92
    } else {
        ((s + 0.055) / 1.055).powf(2.4)
    }
}

pub fn mix(a: u32, b: u32, t: f32) -> [f32; 3] {
    mix_rgb(srgb_to_linear_rgb(a), srgb_to_linear_rgb(b), t)
}

pub fn mix_rgb(a: [f32; 3], b: [f32; 3], t: f32) -> [f32; 3] {
    [
        lerp(a[0], b[0], t),
        lerp(a[1], b[1], t),
        lerp(a[2], b[2], t),
    ]
}

pub fn lerp(a: f32, b: f32, t: f32) -> f32 {
    a + (b - a) * t
}

pub fn normalize(v: [f32; 3]) -> [f32; 3] {
    let len = (v[0] * v[0] + v[1] * v[1] + v[2] * v[2]).sqrt();
    if len <= f32::EPSILON {
        [0.0, 1.0, 0.0]
    } else {
        [v[0] / len, v[1] / len, v[2] / len]
    }
}

pub fn min_max(a: f32, b: f32, c: f32) -> (f32, f32) {
    (a.min(b).min(c), a.max(b).max(c))
}

pub fn in_triangle(p: (f32, f32), a: (f32, f32), b: (f32, f32), c: (f32, f32)) -> bool {
    let d = (b.1 - c.1) * (a.0 - c.0) + (c.0 - b.0) * (a.1 - c.1);
    if d.abs() < 1e-9 {
        return false;
    }
    let l1 = ((b.1 - c.1) * (p.0 - c.0) + (c.0 - b.0) * (p.1 - c.1)) / d;
    let l2 = ((c.1 - a.1) * (p.0 - c.0) + (a.0 - c.0) * (p.1 - c.1)) / d;
    let l3 = 1.0 - l1 - l2;
    l1 >= 0.0 && l2 >= 0.0 && l3 >= 0.0
}

pub fn point_seg_dist2(x: f32, z: f32, a: [f32; 2], b: [f32; 2]) -> f32 {
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
pub fn hash3(seed: u64, i: i32, j: i32) -> u64 {
    let mut h = seed ^ (i as u32 as u64).wrapping_mul(0xA24B_AED4_963E_E407);
    h ^= (j as u32 as u64).wrapping_mul(0x9FB2_1C65_1E98_DF25);
    h ^= h >> 29;
    h = h.wrapping_mul(0xBF58_476D_1CE4_E5B9);
    h ^= h >> 32;
    h = h.wrapping_mul(0x94D0_49BB_1331_11EB);
    h ^ (h >> 31)
}

pub fn rand01(h: u64, stream: u32) -> f32 {
    let mut x = h.rotate_left(stream * 11 + 7);
    x ^= x >> 33;
    x = x.wrapping_mul(0xFF51_AFD7_ED55_8CCD);
    x ^= x >> 33;
    ((x >> 40) as f32) / 16_777_216.0
}

#[cfg(test)]
mod tests {
    use super::*;

    const GRONINGEN_Z14: &[u8] = include_bytes!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/tests/fixtures/groningen_z14.mvt"
    ));

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
        assert!(
            !trees.is_empty(),
            "no trees scattered over a tile with parks"
        );
        assert!(trees.len() <= MAX_TREES_PER_CELL);
        for t in &trees {
            assert!(
                t.x >= -1.0 && t.x <= tile_m + 1.0 && t.z >= -1.0 && t.z <= tile_m + 1.0,
                "tree outside the tile at ({}, {})",
                t.x,
                t.z
            );
            assert!(
                t.height > 0.5 && t.height < 20.0,
                "implausible height {}",
                t.height
            );
            assert!(
                t.radius > 0.5 && t.radius < 6.0,
                "implausible radius {}",
                t.radius
            );
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
        let extent = tile
            .layer("water_polygons")
            .map_or(4096.0, TileData::extent);
        let mask = WaterMask::build(&tile, tile_m / extent, extent, tile_m);
        assert!(
            !mask.tris.is_empty(),
            "fixture has no water to test against"
        );

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

    /// No tree may stand on a street. `land` and `streets` are separate layers, so
    /// a wood is not cut out where a road crosses it — the scatter grew trees down
    /// the middle of the lanes through the Sterrebos and across every park path.
    #[test]
    fn no_tree_stands_on_a_street() {
        let tile_m = 1472.0;
        let tile = TileData::parse(GRONINGEN_Z14).expect("parse");
        let mask = RoadMask::build(&tile, tile_m);
        assert!(
            !mask.segs.is_empty(),
            "fixture has no streets to test against"
        );

        let trees = scatter_vegetation(&tile, tile_m, 42).expect("scatter");
        for t in &trees {
            assert!(
                !mask.blocks(t.x, t.z, 0.0),
                "tree planted on a carriageway at ({}, {})",
                t.x,
                t.z
            );
        }
    }

    /// The keep-out is the carriageway, so it has to scale with the street: a
    /// motorway clears metres where a footpath clears about one. A mask built off
    /// the centre line alone would be the same distance for both.
    #[test]
    fn the_road_keep_out_is_as_wide_as_the_road() {
        let mut mask = RoadMask {
            segs: Vec::new(),
            buckets: HashMap::new(),
            tile_m: 400.0,
        };
        mask.push([0.0, 0.0], [100.0, 0.0], 1.0); // a path
        mask.push([0.0, 200.0], [100.0, 200.0], 11.0); // a motorway

        // Three metres off the centre line: clear of the path, well inside the
        // motorway.
        assert!(!mask.blocks(50.0, 3.0, 0.0));
        assert!(mask.blocks(50.0, 203.0, 0.0));
        // …and a canopy that would lean over either one is refused.
        assert!(mask.blocks(50.0, 3.0, 2.5));
        // Past the end of a segment is not "on the road" — a keep-out that
        // extended a motorway's width beyond its last vertex would clear a bald
        // disc at every tile seam.
        assert!(!mask.blocks(-20.0, 200.0, 2.0));
    }

    /// A coordinate no legitimate tile carries must cost a rejected segment, not
    /// an unbounded loop.
    ///
    /// `mvt-reader` clips its cursor at `i32::MAX` and never to the extent, so a
    /// few bytes of crafted varint deliver ~2.1e9 here. The bbox loop in
    /// [`RoadMask::push`] then covers ~6·10^7 cells per axis — 10^14 `HashMap`
    /// inserts of a fresh `Vec`, which is a hang followed by an OOM kill, on the
    /// default path (`trees_enabled` is on). Both halves of the guard are
    /// exercised: the window in `build` never sees this call, and the clamp here
    /// bounds it anyway.
    #[test]
    fn a_wild_coordinate_cannot_sweep_the_bucket_grid() {
        let mut mask = RoadMask {
            segs: Vec::new(),
            buckets: HashMap::new(),
            tile_m: 1472.0,
        };
        mask.push([0.0, 0.0], [2.1e9, 2.1e9], 1.0);
        mask.push([-2.1e9, 0.0], [0.0, 0.0], 1.0);
        mask.push([f32::NAN, 0.0], [10.0, 10.0], 1.0);
        // The tile's own grid is 1472 m of 32 m cells plus the buffer margin, so
        // ~117² cells is the ceiling however wild the input. Unclamped, the first
        // of these alone asks for ~4·10^15.
        assert!(
            mask.buckets.len() < 20_000,
            "{} buckets from three bad segments",
            mask.buckets.len()
        );
    }

    /// …and the window that keeps them out is wide enough for real data. The
    /// fixture's street geometry stops at ±1.6% of the extent past the tile edge,
    /// so a 25% window drops none of it.
    #[test]
    fn the_road_mask_keeps_every_street_of_a_real_tile() {
        let tile_m = 1472.0;
        let mask = RoadMask::build(&fixture(), tile_m);
        let margin = tile_m * TILE_BUFFER_FRAC;
        // 2,595 segments are encoded in the fixture's `streets` layer before
        // `drawn_street` drops the tunnels and the unclassed ways.
        assert!(
            mask.segs.len() > 500,
            "the window swallowed the fixture's streets: {} segments",
            mask.segs.len()
        );
        for (a, b, _) in &mask.segs {
            for p in [a, b] {
                assert!(
                    p[0] >= -margin && p[0] <= tile_m + margin,
                    "segment endpoint at x = {} is outside the window",
                    p[0]
                );
                assert!(
                    p[1] >= -margin && p[1] <= tile_m + margin,
                    "segment endpoint at z = {} is outside the window",
                    p[1]
                );
            }
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
        let meshes = build_vegetation_meshes(&trees, None);
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
        assert!(build_vegetation_meshes(&[], None).is_empty());
    }

    /// The unit canopy is the per-tree cost multiplier — if it silently grows, so
    /// does every cell's triangle count.
    #[test]
    fn unit_canopy_is_small() {
        let u = unit_canopy();
        assert_eq!(u.verts.len(), 18);
        assert_eq!(u.indices.len(), 32 * 3);
    }

    /// No piece of a planted cell exceeds the frame's upload budget.
    ///
    /// The property that matters, because bevy writes one *whole* asset per frame
    /// whatever the budget says — so a single mesh over the limit is a frame over
    /// the limit and the throttle upstream simply does not apply. A full cell used
    /// to be one ~6 MB canopy mesh; at the phone rung's 4 MB ceiling every piece
    /// must now be under it.
    ///
    /// Asserted with the trees kept: `sum(pieces) == one big mesh`, so this cannot
    /// be passed by planting fewer trees, which is the failure mode that would
    /// otherwise look like a fix.
    #[test]
    fn a_planted_cell_is_cut_into_uploadable_pieces() {
        const CEILING: usize = 4 * 1024 * 1024;
        let trees = scatter_vegetation(&fixture(), 1472.0, 42).expect("scatter");
        assert!(!trees.is_empty(), "the fixture plants something");

        let whole = build_vegetation_meshes(&trees, None);
        let split = build_vegetation_meshes(&trees, Some(CEILING));

        for m in &split {
            let bytes = m.positions.len() * VERTEX_BYTES + m.indices.len() * INDEX_BYTES;
            assert!(
                bytes <= CEILING,
                "{:?} piece is {bytes} B, over the {CEILING} B frame budget",
                m.group
            );
        }
        let verts = |ms: &[SurfaceMesh]| ms.iter().map(|m| m.positions.len()).sum::<usize>();
        let tris = |ms: &[SurfaceMesh]| ms.iter().map(|m| m.indices.len()).sum::<usize>();
        assert_eq!(verts(&split), verts(&whole), "cutting dropped vertices");
        assert_eq!(tris(&split), tris(&whole), "cutting dropped triangles");
    }

    /// The tessellated ground obeys the same ceiling, and keeps every triangle.
    ///
    /// The other producer that reached asset sizes a frame budget cannot spread —
    /// 4.2–4.6 MB per cell over Groningen, measured 2026-08-19. Its cut is taken
    /// after a feature rather than before one (a polygon's vertex count is not
    /// known until it is tessellated), so the bound it can promise is "under the
    /// ceiling plus one feature" — asserted as such rather than pretending to the
    /// strict bound the tree cut manages.
    #[test]
    fn tessellated_surfaces_are_cut_to_the_upload_ceiling() {
        use crate::vector_tiles::{mesh_upload_bytes, tessellate_surfaces};

        let tile = fixture();
        let whole = tessellate_surfaces(&tile, 1472.0, None).expect("tessellate");
        let biggest = whole
            .iter()
            .map(|m| mesh_upload_bytes(m.positions.len(), m.indices.len()))
            .max()
            .expect("the fixture tessellates to something");
        // Derived from the fixture rather than a round number, so this keeps
        // testing the cut if the fixture is ever replaced with a lighter tile.
        let ceiling = biggest / 2;

        let split = tessellate_surfaces(&tile, 1472.0, Some(ceiling)).expect("tessellate");
        assert!(
            split.len() > whole.len(),
            "half the largest group ({biggest} B) must have cut something"
        );
        for m in &split {
            let bytes = mesh_upload_bytes(m.positions.len(), m.indices.len());
            assert!(
                bytes <= ceiling + biggest,
                "{:?} piece is {bytes} B, past the ceiling by more than one feature",
                m.group
            );
        }

        let verts = |ms: &[SurfaceMesh]| ms.iter().map(|m| m.positions.len()).sum::<usize>();
        let tris = |ms: &[SurfaceMesh]| ms.iter().map(|m| m.indices.len()).sum::<usize>();
        assert_eq!(verts(&split), verts(&whole), "cutting dropped vertices");
        assert_eq!(tris(&split), tris(&whole), "cutting dropped triangles");
    }

    /// And so does the road ribbon, which is the one that was actually 4.2 MB.
    ///
    /// Worth its own test rather than a second half of the one above: roads carry
    /// UVs (the surfaces do not), and a cut that dropped or misaligned them would
    /// show up as road markings sliding off the carriageway rather than as
    /// anything the vertex and triangle totals would catch — so the UV count is
    /// asserted alongside them.
    #[test]
    fn tessellated_roads_are_cut_to_the_upload_ceiling() {
        use crate::vector_tiles::{mesh_upload_bytes, tessellate_roads};

        let tile = fixture();
        let whole = tessellate_roads(&tile, 1472.0, &[], None).expect("roads");
        let biggest = whole
            .iter()
            .map(|m| mesh_upload_bytes(m.positions.len(), m.indices.len()))
            .max()
            .expect("the fixture has streets");
        let ceiling = biggest / 2;

        let split = tessellate_roads(&tile, 1472.0, &[], Some(ceiling)).expect("roads");
        assert!(
            split.len() > whole.len(),
            "half the largest group ({biggest} B) must have cut something"
        );
        for m in &split {
            let bytes = mesh_upload_bytes(m.positions.len(), m.indices.len());
            assert!(
                bytes <= ceiling + biggest,
                "{:?} piece is {bytes} B, past the ceiling by more than one feature",
                m.group
            );
            assert_eq!(
                m.uvs.len(),
                m.positions.len(),
                "{:?} piece lost its UVs in the cut",
                m.group
            );
        }

        let sum =
            |ms: &[SurfaceMesh], f: fn(&SurfaceMesh) -> usize| ms.iter().map(f).sum::<usize>();
        assert_eq!(
            sum(&split, |m| m.positions.len()),
            sum(&whole, |m| m.positions.len()),
            "cutting dropped vertices"
        );
        assert_eq!(
            sum(&split, |m| m.indices.len()),
            sum(&whole, |m| m.indices.len()),
            "cutting dropped triangles"
        );
    }

    /// A ceiling below one tree still terminates, with one tree to a piece.
    ///
    /// The degenerate end of the rule above: a piece must hold at least one tree or
    /// the flush-before-add loop emits empty meshes forever. Worth its own test
    /// because the guard for it is an `&& !is_empty()` that reads as belt-and-braces
    /// and is not.
    #[test]
    fn a_ceiling_below_one_tree_still_terminates() {
        let trees = scatter_vegetation(&fixture(), 1472.0, 42).expect("scatter");
        let split = build_vegetation_meshes(&trees[..8], Some(1));
        assert!(!split.is_empty());
        assert!(
            split.iter().all(|m| !m.positions.is_empty()),
            "no piece may be empty"
        );
    }

    /// What a real cell actually costs. `cargo test -p cartopolis_geo
    /// dump_vegetation_cost -- --ignored --nocapture`.
    #[test]
    #[ignore]
    fn dump_vegetation_cost() {
        let tile_m = 1472.0;
        let trees = scatter_vegetation(&fixture(), tile_m, 42).expect("scatter");
        let meshes = build_vegetation_meshes(&trees, None);
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
