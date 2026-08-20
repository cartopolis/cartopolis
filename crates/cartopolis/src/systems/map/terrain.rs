//! **Terrain relief**: the measured ground, as one field the whole map stands on.
//!
//! `systems::height` answers questions about *specific* things — how high is this
//! bridge deck, what is the ground under the anchor. This module asks the one
//! question everything else needs: **how high is the ground here**, for any point
//! near the camera, cheaply, thousands of times per mesh build. It is the layer
//! that turns the client's flat `y = 0` plane into relief.
//!
//! # One box, baked once
//!
//! A consumer cannot afford a round trip per vertex, so this is not a cache of
//! per-question patches: it is a single [`HeightRequest`] — 6.4 km of ground at
//! 256×256 (25 m/sample, ~256 KB) — fetched once per map anchor and **baked** into
//! a regular grid in the anchored world-XZ frame. After that, [`TerrainField::at`]
//! is a bilinear lookup with no I/O and no allocation, and every ground-attached
//! system samples the same field, which is what keeps the road, the water, the
//! trees, the buildings and the avatar on the *same* surface.
//!
//! Baking converts each grid point through `world → lat/lng → RD` rather than
//! mapping the RD box onto the world box directly. RD's grid is rotated against
//! Web Mercator's by the meridian convergence (~1° over the Netherlands) and
//! scaled slightly differently; taking the axis-aligned shortcut puts the corners
//! of a 6.4 km field off by ~80 m, which is a hill in the wrong place.
//!
//! # Relative to the anchor, and tapered to nothing at the edge
//!
//! Samples are stored **relative to the ground at the anchor**, so `y = 0` keeps
//! meaning "the ground where you are" and every existing placement stays right at
//! the map's centre. The alternative — absolute metres above NAP — would drop the
//! whole world 322 m below the camera in Vaals and lift it above in the polder,
//! for no gain: what the eye reads as relief is the *difference* between here and
//! there.
//!
//! The field is finite (6.4 km) but the map is not: the textured clipmap runs to
//! the horizon and the world beyond AHN is flat by definition. So the field
//! **tapers to exactly zero** at its own boundary, baked into the samples rather
//! than applied at lookup — outside the box `at()` returns 0, and the two meet
//! without a step. The cost is real and worth naming: a hillside crossing the
//! boundary is flattened toward the anchor's level over the last ~600 m. Measured
//! against the alternative it is cheap — see `docs/notes/terrain-relief.md` for
//! what NL relief actually is over a 6.4 km box.
//!
//! # No-data is normal
//!
//! AHN's DTM is full of holes and that is not an error: bridges and buildings are
//! *cut out* of it, the sea has no measurement, and a box near the border is half
//! Germany. Small holes are filled from their neighbours ([`FILL_PASSES`]
//! dilations, ~150 m of reach) because a cut-out viaduct must not read as a pit;
//! what is left over — the sea, the far side of a border — settles at the anchor's
//! own level, which is flat, and flat is the honest answer for ground we have not
//! measured.

use bevy::prelude::*;

use crate::systems::data_layers::DataLayers;
use crate::systems::geo_nav::{world_to_lat_lng, WorldFrame};
use crate::systems::height::{wgs84_to_rd, HeightLayer, HeightModel, HeightPatch, HeightRequest};
use crate::systems::map_stream::MapStream;

/// Half-extent of the field in world metres (a 6.4 km box). Sized to cover the
/// geometry ring and the finest textured tiles around it, which is everything
/// close enough for relief to read as relief.
pub const TERRAIN_HALF_M: f32 = 3_200.0;

/// Samples per side. With [`TERRAIN_HALF_M`] this is 25 m/sample — the resolution
/// that shows dunes, dikes-as-slopes and river valleys, and deliberately not the
/// one that shows a kerb. AHN's own grid is 0.5 m; asking for it over 6.4 km would
/// be a 650 MB answer to a question nothing on screen can express.
const TERRAIN_GRID: usize = 256;

/// Ground metres between adjacent samples (~25 m). Public because it is the only
/// honest answer to "how finely should I subdivide a mesh that stands on this
/// field": below it a grid interpolates measurements that were never taken, and
/// pays full geometry price for them. [`displaced_grid`]'s callers size themselves
/// off this rather than off a constant chosen for one zoom level.
pub const TERRAIN_SAMPLE_M: f32 = TERRAIN_HALF_M * 2.0 / (TERRAIN_GRID as f32 - 1.0);

/// Fraction of the half-extent held at full strength before the edge taper begins.
const TAPER_START: f32 = 0.82;

/// Neighbour-dilation passes used to close no-data holes. Six passes reach ~150 m,
/// which covers a cut-out viaduct or a building footprint and stops well short of
/// smearing a coastline across the North Sea.
const FILL_PASSES: usize = 6;

/// Extra RD metres requested around the field's own box, so a bilinear lookup at
/// the very edge still has measured samples on both sides of it.
const REQUEST_PAD_M: f64 = 80.0;

/// The sampled ground, shareable by value.
///
/// Cloning is an `Arc` bump, so a mesh-building **worker thread** can carry the
/// terrain with it. That is not a convenience: a building's base is one height for
/// the whole footprint, and the only place footprints exist as such is inside the
/// worker that assembles the cell — by the time a cell reaches the main thread it
/// is one merged triangle soup with no buildings in it any more.
#[derive(Clone, Default)]
pub struct TerrainSnapshot {
    /// `TERRAIN_GRID²` samples, all finite, row-major with x fastest and both axes
    /// ascending in world metres. Empty until the first bake lands.
    samples: std::sync::Arc<Vec<f32>>,
    /// Half-extent the samples span, in world metres.
    half_m: f32,
}

/// The measured ground near the anchor, as a sampled field.
///
/// Heights are metres **relative to the ground at the map anchor** (see the module
/// docs), positive upward, in the same anchored world-XZ frame the map surfaces
/// and the building colliders use.
#[derive(Resource, Default)]
pub struct TerrainField {
    ground: TerrainSnapshot,
    /// Map-anchor generation this field has already **answered for** — with
    /// samples or without. A re-anchor invalidates it because the whole frame
    /// moved.
    ///
    /// "Answered", not "baked", and the distinction is the whole point. It used to
    /// be a bare `u32` paired with an `is_ready()` test, which meant a box AHN has
    /// no data for — all sea, over the border, anywhere outside the Netherlands
    /// that `covers` still admits — never satisfied the guard: `bake` returned
    /// `None`, `clear()` early-returned because the field was already empty,
    /// `is_ready()` stayed false, and `update_terrain_field` re-baked 65,536
    /// samples on the main thread on **every frame, forever**. The comment beside
    /// it claimed the opposite. An empty answer is still an answer, and only an
    /// `Option` can say so.
    answered_gen: Option<u32>,
    /// Absolute height (m above NAP) that `0.0` means here — the ground under the
    /// anchor. Not used for placement (everything is relative); reported, because
    /// it is the one number that says *where* the field is.
    pub datum_m: f32,
    /// Bumped on every change, including going away. Consumers that bake the field
    /// into a mesh compare it to know when their geometry is stale.
    pub revision: u32,
    /// Lowest and highest sample, metres relative to the datum — the field's own
    /// answer to "is there any relief here at all".
    pub range_m: (f32, f32),
}

impl TerrainField {
    /// A by-value handle on the same measurements, for a worker thread.
    #[must_use]
    pub fn snapshot(&self) -> TerrainSnapshot {
        self.ground.clone()
    }

    fn clear(&mut self) {
        if !self.is_ready() {
            return;
        }
        self.ground = TerrainSnapshot::default();
        self.range_m = (0.0, 0.0);
        self.revision = self.revision.wrapping_add(1);
    }
}

/// Everything a consumer asks of the terrain is asked of the snapshot; the resource
/// just carries the current one.
impl std::ops::Deref for TerrainField {
    type Target = TerrainSnapshot;
    fn deref(&self) -> &Self::Target {
        &self.ground
    }
}

impl TerrainSnapshot {
    /// Whether the field carries measurements. `false` means every consumer should
    /// behave exactly as it did before this module existed.
    #[must_use]
    pub fn is_ready(&self) -> bool {
        !self.samples.is_empty()
    }

    /// Ground height at a point in the anchored world-XZ frame, metres relative to
    /// the anchor's ground. `0.0` outside the field, and whenever it is off, out of
    /// coverage or not yet fetched — so a caller can add it unconditionally.
    #[must_use]
    pub fn at(&self, wx: f32, wz: f32) -> f32 {
        if self.samples.is_empty() {
            return 0.0;
        }
        let n = TERRAIN_GRID;
        let span = self.half_m * 2.0;
        // Sample coordinates, in grid cells from the south-west corner.
        let fx = (wx + self.half_m) / span * (n - 1) as f32;
        let fz = (wz + self.half_m) / span * (n - 1) as f32;
        if !(0.0..=(n - 1) as f32).contains(&fx) || !(0.0..=(n - 1) as f32).contains(&fz) {
            return 0.0;
        }
        let (x0, z0) = (fx.floor() as usize, fz.floor() as usize);
        let (x1, z1) = ((x0 + 1).min(n - 1), (z0 + 1).min(n - 1));
        let (tx, tz) = (fx - x0 as f32, fz - z0 as f32);
        let s = |x: usize, z: usize| self.samples[z * n + x];
        let a = s(x0, z0) + (s(x1, z0) - s(x0, z0)) * tx;
        let b = s(x0, z1) + (s(x1, z1) - s(x0, z1)) * tx;
        a + (b - a) * tz
    }

    /// Surface normal at a world-XZ point, from the field's own slope (central
    /// differences over one grid step). `+Y` on flat ground and wherever the field
    /// is absent.
    #[must_use]
    pub fn normal_at(&self, wx: f32, wz: f32) -> Vec3 {
        if self.samples.is_empty() {
            return Vec3::Y;
        }
        let d = self.half_m * 2.0 / (TERRAIN_GRID - 1) as f32;
        let dx = self.at(wx + d, wz) - self.at(wx - d, wz);
        let dz = self.at(wx, wz + d) - self.at(wx, wz - d);
        Vec3::new(-dx, 2.0 * d, -dz).normalize()
    }

    /// The median height over a polygon's vertices — what a *level* surface (a
    /// canal, a lake) stands at. Following the field per-vertex would tilt a water
    /// body down the valley it sits in; water is flat, and the whole point of
    /// measuring is not to invent slopes that aren't there.
    #[must_use]
    pub fn level_over(&self, points: impl IntoIterator<Item = [f32; 2]>) -> f32 {
        let mut hs: Vec<f32> = points.into_iter().map(|p| self.at(p[0], p[1])).collect();
        if hs.is_empty() {
            return 0.0;
        }
        hs.sort_by(f32::total_cmp);
        hs[hs.len() / 2]
    }
}

/// Keep [`TerrainField`] in step with the map anchor: request the box once, bake it
/// when it lands, and drop it whenever the layer is off or out of coverage.
pub fn update_terrain_field(
    mut field: ResMut<TerrainField>,
    mut height: ResMut<HeightLayer>,
    stream: Res<MapStream>,
    dl: Res<DataLayers>,
) {
    let Some(anchor) = stream.geo_anchor() else {
        return;
    };
    if !dl.height_enabled || !HeightLayer::covers(anchor.0, anchor.1) {
        field.clear();
        // The layer can come back on without the anchor moving, so this is not an
        // answer for the current generation — leave `answered_gen` alone and let
        // the bake run when it does.
        field.answered_gen = None;
        return;
    }
    if field.answered_gen == Some(stream.anchor_generation) {
        return;
    }

    let req = field_request(anchor, TERRAIN_HALF_M);
    // `patch` queues the fetch on the first ask and answers `None` until it lands;
    // the field simply stays flat for those few frames.
    let Some(patch) = height.patch(req) else {
        return;
    };
    let Some((samples, datum)) = bake(patch, anchor, TERRAIN_HALF_M) else {
        // A box AHN has nothing at all for (all sea, or over the border): stay
        // flat rather than re-asking every frame — `HeightLayer` caches the patch,
        // so this costs one comparison from here on. Recording the generation is
        // what makes that true; see `answered_gen`.
        field.answered_gen = Some(stream.anchor_generation);
        field.clear();
        return;
    };

    let lo = samples.iter().copied().fold(f32::MAX, f32::min);
    let hi = samples.iter().copied().fold(f32::MIN, f32::max);
    field.ground = TerrainSnapshot {
        samples: std::sync::Arc::new(samples),
        half_m: TERRAIN_HALF_M,
    };
    field.answered_gen = Some(stream.anchor_generation);
    field.datum_m = datum;
    field.range_m = (lo, hi);
    field.revision = field.revision.wrapping_add(1);
    tracing::info!(
        datum_m = datum,
        low_m = lo,
        high_m = hi,
        "terrain field baked"
    );
}

/// The RD box to measure for a world box of `half` metres around the anchor.
///
/// Built from the world box's corners *and* edge midpoints: Mercator-to-RD is not
/// an affine map, so the extreme easting on a north edge is not necessarily at a
/// corner, and a box that merely spans the corners can miss ground the field will
/// ask about.
fn field_request(anchor: (f64, f64, u32), half: f32) -> HeightRequest {
    let h = half as f64;
    let mut min = [f64::MAX, f64::MAX];
    let mut max = [f64::MIN, f64::MIN];
    for wz in [-h, 0.0, h] {
        for wx in [-h, 0.0, h] {
            let (lat, lng) = world_to_lat_lng(anchor, wx, wz);
            let rd = wgs84_to_rd(lat, lng);
            min = [min[0].min(rd[0]), min[1].min(rd[1])];
            max = [max[0].max(rd[0]), max[1].max(rd[1])];
        }
    }
    HeightRequest {
        model: HeightModel::Terrain,
        min: [min[0] - REQUEST_PAD_M, min[1] - REQUEST_PAD_M],
        max: [max[0] + REQUEST_PAD_M, max[1] + REQUEST_PAD_M],
        grid: TERRAIN_GRID as u32,
    }
    .padded(0.0)
}

/// Resample a measured patch into the world-XZ grid, fill its holes, subtract the
/// anchor's own ground and taper the edge to zero.
///
/// `None` when the patch has no usable sample under the field at all.
fn bake(patch: &HeightPatch, anchor: (f64, f64, u32), half: f32) -> Option<(Vec<f32>, f32)> {
    let n = TERRAIN_GRID;
    let step = (half * 2.0) as f64 / (n - 1) as f64;
    // Mercator is separable, so the grid's 65,536 coordinate conversions are
    // really `n` longitudes and `n` latitudes (`geo_nav::WorldFrame` says why this
    // is worth spelling out). Naively this ran the anchor's trig, a `sinh` and an
    // `atan` per *sample*, on the main thread, once per map anchor.
    let frame = WorldFrame::new(anchor);
    let axis: Vec<f64> = (0..n).map(|i| -half as f64 + i as f64 * step).collect();
    let lngs: Vec<f64> = axis.iter().map(|&wx| frame.lng_at(wx)).collect();
    let lats: Vec<f64> = axis.iter().map(|&wz| frame.lat_at(wz)).collect();

    let mut raw = vec![f32::NAN; n * n];
    for (z, &lat) in lats.iter().enumerate() {
        for (x, &lng) in lngs.iter().enumerate() {
            // `wgs84_to_rd` is a bivariate polynomial and genuinely is per sample;
            // after the above it is the whole remaining cost of this loop.
            if let Some(v) = patch.at(wgs84_to_rd(lat, lng)) {
                raw[z * n + x] = v;
            }
        }
    }
    if !raw.iter().any(|v| v.is_finite()) {
        return None;
    }

    // The datum is the ground at the anchor — the field's own centre — so the
    // world's existing `y = 0` keeps meaning "here". Falling back to the median
    // covers an anchor that happens to sit on a bridge (a hole in the DTM).
    let centre = raw[(n / 2) * n + n / 2];
    let datum = if centre.is_finite() {
        centre
    } else {
        median_finite(&raw)?
    };

    fill_holes(&mut raw, n);
    let mut samples = vec![0.0f32; n * n];
    for z in 0..n {
        for x in 0..n {
            let v = raw[z * n + x];
            let h = if v.is_finite() { v - datum } else { 0.0 };
            samples[z * n + x] = h * edge_taper(x, z, n);
        }
    }
    Some((samples, datum))
}

/// Strength of the field at a grid position: 1 through the middle, easing to 0 at
/// the boundary so the measured ground meets the flat world outside it seamlessly.
fn edge_taper(x: usize, z: usize, n: usize) -> f32 {
    let norm = |i: usize| ((i as f32 / (n - 1) as f32) * 2.0 - 1.0).abs();
    let d = norm(x).max(norm(z));
    if d <= TAPER_START {
        return 1.0;
    }
    let t = ((d - TAPER_START) / (1.0 - TAPER_START)).clamp(0.0, 1.0);
    // Smoothstep, so the field flattens without a crease where the taper starts.
    let s = t * t * (3.0 - 2.0 * t);
    1.0 - s
}

/// Close no-data holes by repeated neighbour averaging. Each pass reaches one cell
/// further, so [`FILL_PASSES`] is a distance, not a quality knob.
fn fill_holes(grid: &mut [f32], n: usize) {
    // One scratch buffer for the whole run, refilled per pass, rather than a fresh
    // 256 KB `to_vec()` each time round. A pass has to read a *snapshot* — filling
    // in place would let a cell average neighbours this pass already invented, so
    // the fill would run further than `FILL_PASSES` says and in row-major order
    // only — but it does not need a new allocation to get one.
    let mut prev = vec![0.0f32; grid.len()];
    for _ in 0..FILL_PASSES {
        if grid.iter().all(|v| v.is_finite()) {
            return;
        }
        prev.copy_from_slice(grid);
        for z in 0..n {
            for x in 0..n {
                if prev[z * n + x].is_finite() {
                    continue;
                }
                let mut sum = 0.0;
                let mut count = 0;
                for (dx, dz) in [(-1i32, 0i32), (1, 0), (0, -1), (0, 1)] {
                    let (nx, nz) = (x as i32 + dx, z as i32 + dz);
                    if nx < 0 || nz < 0 || nx >= n as i32 || nz >= n as i32 {
                        continue;
                    }
                    let v = prev[nz as usize * n + nx as usize];
                    if v.is_finite() {
                        sum += v;
                        count += 1;
                    }
                }
                if count > 0 {
                    grid[z * n + x] = sum / count as f32;
                }
            }
        }
    }
}

/// Longest ground edge (m) left unsplit before displacement. A touch under the
/// field's own 25 m sampling, so a refined surface can express every measurement
/// the field holds and not one more.
pub const MAX_GROUND_EDGE_M: f32 = 24.0;

/// Refinement passes. Five takes a cell-spanning 1.5 km triangle down to ~46 m;
/// the cap is what stops one absurd polygon from tessellating the frame away.
const MAX_REFINE_PASSES: usize = 5;

/// Split every triangle edge longer than `max_edge`, repeatedly, so a flat polygon
/// has vertices to be displaced *by*.
///
/// Earcut answers "cover this outline" with as few triangles as it can, which for
/// a field or a lake is two — and two triangles cannot follow a hill, they can only
/// be tilted by it. The refinement is **per edge, not per triangle**: an edge is
/// split once, its midpoint is shared by both triangles that own it, and each
/// triangle is then re-cut according to how many of its edges were split (the
/// classic red-green refinement). Deciding per triangle instead — "this one is big,
/// subdivide it" — leaves a T-junction wherever a split triangle meets an unsplit
/// neighbour, and a T-junction on a slope is a crack you can see the sky through.
///
/// Attributes ride along, interpolated at each midpoint, so UV-mapped roads and
/// colour-per-vertex canopies survive refinement.
pub fn refine_for_terrain(
    positions: &mut Vec<[f32; 3]>,
    indices: &mut Vec<u32>,
    uvs: &mut Vec<[f32; 2]>,
    normals: &mut Vec<[f32; 3]>,
    colors: &mut Vec<[f32; 4]>,
    max_edge: f32,
) {
    use std::collections::HashMap;

    let limit = max_edge.max(1.0);
    let limit_sq = limit * limit;
    for _ in 0..MAX_REFINE_PASSES {
        let mut midpoints: HashMap<(u32, u32), u32> = HashMap::new();
        let split = |a: u32,
                     b: u32,
                     positions: &mut Vec<[f32; 3]>,
                     uvs: &mut Vec<[f32; 2]>,
                     normals: &mut Vec<[f32; 3]>,
                     colors: &mut Vec<[f32; 4]>,
                     midpoints: &mut HashMap<(u32, u32), u32>|
         -> Option<u32> {
            let key = if a < b { (a, b) } else { (b, a) };
            if let Some(m) = midpoints.get(&key) {
                return Some(*m);
            }
            let (pa, pb) = (positions[a as usize], positions[b as usize]);
            let d = [pa[0] - pb[0], pa[1] - pb[1], pa[2] - pb[2]];
            if d[0] * d[0] + d[1] * d[1] + d[2] * d[2] <= limit_sq {
                return None;
            }
            let m = positions.len() as u32;
            positions.push([
                (pa[0] + pb[0]) * 0.5,
                (pa[1] + pb[1]) * 0.5,
                (pa[2] + pb[2]) * 0.5,
            ]);
            if !uvs.is_empty() {
                let (ua, ub) = (uvs[a as usize], uvs[b as usize]);
                uvs.push([(ua[0] + ub[0]) * 0.5, (ua[1] + ub[1]) * 0.5]);
            }
            if !normals.is_empty() {
                let (na, nb) = (normals[a as usize], normals[b as usize]);
                let n = Vec3::from(na) + Vec3::from(nb);
                normals.push(n.normalize_or(Vec3::Y).to_array());
            }
            if !colors.is_empty() {
                let (ca, cb) = (colors[a as usize], colors[b as usize]);
                colors.push(std::array::from_fn(|i| (ca[i] + cb[i]) * 0.5));
            }
            midpoints.insert(key, m);
            Some(m)
        };

        let mut out: Vec<u32> = Vec::with_capacity(indices.len());
        let mut any = false;
        for tri in indices.chunks_exact(3) {
            let (a, b, c) = (tri[0], tri[1], tri[2]);
            let ab = split(a, b, positions, uvs, normals, colors, &mut midpoints);
            let bc = split(b, c, positions, uvs, normals, colors, &mut midpoints);
            let ca = split(c, a, positions, uvs, normals, colors, &mut midpoints);
            any |= ab.is_some() || bc.is_some() || ca.is_some();
            match (ab, bc, ca) {
                (None, None, None) => out.extend_from_slice(&[a, b, c]),
                // One split edge: a straight cut from the midpoint to the opposite
                // corner. Winding is preserved in every arm — a flipped sub-triangle
                // is a backface-culled hole in the ground.
                (Some(m), None, None) => out.extend_from_slice(&[a, m, c, m, b, c]),
                (None, Some(m), None) => out.extend_from_slice(&[b, m, a, m, c, a]),
                (None, None, Some(m)) => out.extend_from_slice(&[c, m, b, m, a, b]),
                (Some(m1), Some(m2), None) => {
                    out.extend_from_slice(&[m1, b, m2, a, m1, m2, a, m2, c]);
                }
                (None, Some(m1), Some(m2)) => {
                    out.extend_from_slice(&[m1, c, m2, b, m1, m2, b, m2, a]);
                }
                (Some(m2), None, Some(m1)) => {
                    out.extend_from_slice(&[m1, a, m2, c, m1, m2, c, m2, b]);
                }
                (Some(mab), Some(mbc), Some(mca)) => {
                    out.extend_from_slice(&[a, mab, mca, mab, b, mbc, mca, mbc, c, mab, mbc, mca]);
                }
            }
        }
        *indices = out;
        if !any {
            return;
        }
    }
}

/// Lay an existing mesh over the measured ground: refine its long edges, then lift
/// every vertex by the field under it.
///
/// `origin` is the world-XZ point the mesh's local coordinates are relative to. A
/// no-op when the field is absent, so a caller can apply it unconditionally.
///
/// It **adds** to whatever height the mesh already carries rather than assigning
/// one, which is what lets the route line arrive with a bridge deck already baked
/// into its vertices and still follow the ground the bridge stands on. A deck height
/// cannot be a field sampled here: a road on a viaduct and the road beneath it have
/// the same world XZ, so no function of position can tell them apart — see
/// `navigation::route_lift`.
pub fn drape_mesh(mesh: &mut Mesh, origin: Vec2, field: &TerrainSnapshot) {
    use bevy::render::mesh::{Indices, VertexAttributeValues};

    if !field.is_ready() {
        return;
    }

    let Some(VertexAttributeValues::Float32x3(pos)) = mesh.attribute(Mesh::ATTRIBUTE_POSITION)
    else {
        return;
    };
    let mut positions = pos.clone();
    let mut indices: Vec<u32> = match mesh.indices() {
        Some(Indices::U32(i)) => i.clone(),
        Some(Indices::U16(i)) => i.iter().map(|v| u32::from(*v)).collect(),
        None => return,
    };
    let had_normals = mesh.attribute(Mesh::ATTRIBUTE_NORMAL).is_some();
    refine_for_terrain(
        &mut positions,
        &mut indices,
        &mut Vec::new(),
        &mut Vec::new(),
        &mut Vec::new(),
        MAX_GROUND_EDGE_M,
    );
    for p in positions.iter_mut() {
        p[1] += field.at(origin.x + p[0], origin.y + p[2]);
    }
    if had_normals {
        let normals: Vec<[f32; 3]> = positions
            .iter()
            .map(|p| field.normal_at(origin.x + p[0], origin.y + p[2]).to_array())
            .collect();
        mesh.insert_attribute(Mesh::ATTRIBUTE_NORMAL, normals);
    }
    mesh.insert_attribute(Mesh::ATTRIBUTE_POSITION, positions);
    mesh.insert_indices(Indices::U32(indices));
}

/// Build a ground quad of `size` metres centred on `centre` (world XZ) as a
/// `divisions`×`divisions` grid lifted onto the field.
///
/// Positions are **local to the quad's centre**, so this drops straight into the
/// entity transform the flat tile already had. UVs match the `+Y` face of the
/// `Cuboid` this replaces (u east, v south, 0–1 across the tile) — a tile whose
/// imagery is mirrored against its neighbours is what a mismatch looks like.
///
/// Normals come from the field's own slope rather than from the triangles: the
/// field is what every other consumer stands on, and shading the ground with the
/// gradient of the *same* function is what keeps a lit hillside agreeing with the
/// road running over it.
pub fn displaced_grid(field: &TerrainField, centre: Vec2, size: f32, divisions: usize) -> Mesh {
    use bevy::asset::RenderAssetUsages;
    use bevy::render::mesh::{Indices, PrimitiveTopology};

    let n = divisions.max(1) + 1;
    let step = size / divisions.max(1) as f32;
    let half = size * 0.5;
    let mut positions = Vec::with_capacity(n * n);
    let mut normals = Vec::with_capacity(n * n);
    let mut uvs = Vec::with_capacity(n * n);
    for j in 0..n {
        let lz = -half + j as f32 * step;
        for i in 0..n {
            let lx = -half + i as f32 * step;
            let (wx, wz) = (centre.x + lx, centre.y + lz);
            positions.push([lx, field.at(wx, wz), lz]);
            normals.push(field.normal_at(wx, wz).to_array());
            uvs.push([(lx + half) / size, (lz + half) / size]);
        }
    }
    let mut indices = Vec::with_capacity(divisions * divisions * 6);
    for j in 0..n - 1 {
        for i in 0..n - 1 {
            let (a, b) = ((j * n + i) as u32, (j * n + i + 1) as u32);
            let (c, d) = (((j + 1) * n + i) as u32, ((j + 1) * n + i + 1) as u32);
            // Wound to face up, matching `Cuboid`'s top face — a downward-wound
            // ground is backface-culled and reads as a hole, silently.
            indices.extend_from_slice(&[b, a, c, c, d, b]);
        }
    }
    // `RENDER_WORLD` only: the relief grid is regenerated from the field whenever
    // the revision moves, so the vertex buffer is never a source of truth anyone
    // reads back — keeping the main-world copy `default()` implies just doubles the
    // resident cost of every clipmap level. `map_stream` spawns the tile in the
    // same frame it adds this mesh, so the auto-`Aabb` pass still sees the
    // positions; a handle stashed for later reuse could not do this (that is why
    // `street_labels`' shared quad keeps `MAIN_WORLD`).
    Mesh::new(
        PrimitiveTopology::TriangleList,
        RenderAssetUsages::RENDER_WORLD,
    )
    .with_inserted_attribute(Mesh::ATTRIBUTE_POSITION, positions)
    .with_inserted_attribute(Mesh::ATTRIBUTE_NORMAL, normals)
    .with_inserted_attribute(Mesh::ATTRIBUTE_UV_0, uvs)
    .with_inserted_indices(Indices::U32(indices))
}

fn median_finite(grid: &[f32]) -> Option<f32> {
    let mut v: Vec<f32> = grid.iter().copied().filter(|s| s.is_finite()).collect();
    if v.is_empty() {
        return None;
    }
    v.sort_by(f32::total_cmp);
    Some(v[v.len() / 2])
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A field whose samples are a known ramp, bypassing the fetch.
    fn ramp_field(per_metre: f32) -> TerrainField {
        let n = TERRAIN_GRID;
        let half = TERRAIN_HALF_M;
        let step = half * 2.0 / (n - 1) as f32;
        let mut samples = vec![0.0; n * n];
        for z in 0..n {
            for x in 0..n {
                samples[z * n + x] = (-half + x as f32 * step) * per_metre;
            }
        }
        TerrainField {
            ground: TerrainSnapshot {
                samples: std::sync::Arc::new(samples),
                half_m: half,
            },
            ..Default::default()
        }
    }

    /// A flat quad with a vertex at each corner, in the XZ plane — the shape a
    /// route ribbon is made of.
    fn flat_quad(size: f32) -> Mesh {
        use bevy::render::mesh::Indices;
        let mut mesh = Mesh::new(
            bevy::render::mesh::PrimitiveTopology::TriangleList,
            bevy::asset::RenderAssetUsages::default(),
        );
        mesh.insert_attribute(
            Mesh::ATTRIBUTE_POSITION,
            vec![
                [0.0, 0.0, 0.0],
                [size, 0.0, 0.0],
                [size, 0.0, size],
                [0.0, 0.0, size],
            ],
        );
        mesh.insert_indices(Indices::U32(vec![0, 1, 2, 0, 2, 3]));
        mesh
    }

    /// Lift every vertex of a mesh, the way the route ribbon arrives off a bridge
    /// deck.
    fn raise(mesh: &mut Mesh, by: f32) {
        let Some(bevy::render::mesh::VertexAttributeValues::Float32x3(p)) =
            mesh.attribute_mut(Mesh::ATTRIBUTE_POSITION)
        else {
            panic!("no positions");
        };
        for v in p.iter_mut() {
            v[1] += by;
        }
    }

    fn heights_of(mesh: &Mesh) -> Vec<f32> {
        let Some(bevy::render::mesh::VertexAttributeValues::Float32x3(p)) =
            mesh.attribute(Mesh::ATTRIBUTE_POSITION)
        else {
            panic!("no positions");
        };
        p.iter().map(|v| v[1]).collect()
    }

    /// The drape **adds** to the height a mesh already carries. That is what lets the
    /// route line arrive with a bridge deck baked into its vertices and still follow
    /// the ground the bridge stands on — a drape that assigned would flatten the
    /// viaduct back onto the terrain.
    #[test]
    fn draping_adds_to_the_height_a_mesh_already_has() {
        let field = ramp_field(0.01).snapshot();
        let at = Vec2::ZERO;

        let mut ground = flat_quad(10.0);
        drape_mesh(&mut ground, at, &field);

        let mut raised = flat_quad(10.0);
        raise(&mut raised, 6.0);
        drape_mesh(&mut raised, at, &field);

        let (a, b) = (heights_of(&ground), heights_of(&raised));
        assert_eq!(a.len(), b.len());
        assert!(
            a.iter().any(|y| *y != 0.0),
            "the ramp field should have moved the plain drape at all"
        );
        for (g, r) in a.iter().zip(&b) {
            assert!((r - g - 6.0).abs() < 1e-3, "ground {g}, raised {r}");
        }
    }

    /// An absent field leaves a mesh exactly as it was — including a height it
    /// brought with it. The old wording for this was "a no-op", which is only the
    /// same thing while nothing arrives pre-lifted.
    #[test]
    fn an_absent_field_leaves_a_raised_mesh_raised() {
        let mut mesh = flat_quad(10.0);
        raise(&mut mesh, 6.0);
        drape_mesh(&mut mesh, Vec2::ZERO, &TerrainField::default().snapshot());
        assert!(heights_of(&mesh).iter().all(|y| (*y - 6.0).abs() < 1e-6));
    }

    #[test]
    fn an_absent_field_is_flat_everywhere() {
        let f = TerrainField::default();
        assert!(!f.is_ready());
        assert_eq!(f.at(0.0, 0.0), 0.0);
        assert_eq!(f.at(1_000.0, -2_500.0), 0.0);
        // The point of returning zero rather than an Option: every consumer adds it
        // unconditionally, so "layer off" costs a branch, not a code path.
        assert_eq!(f.at(f32::MAX, f32::MIN), 0.0);
    }

    #[test]
    fn sampling_is_bilinear_and_bounded() {
        let f = ramp_field(0.01); // 1 cm per metre east
        assert!((f.at(0.0, 0.0)).abs() < 1e-3);
        assert!((f.at(100.0, 0.0) - 1.0).abs() < 0.01);
        assert!((f.at(-100.0, 250.0) + 1.0).abs() < 0.01);
        // Between two samples, not snapped to one of them.
        let step = TERRAIN_HALF_M * 2.0 / (TERRAIN_GRID - 1) as f32;
        let mid = f.at(step * 0.5, 0.0);
        assert!(mid > f.at(0.0, 0.0) && mid < f.at(step, 0.0));
        // Outside the box is flat world, not an extrapolated ramp.
        assert_eq!(f.at(TERRAIN_HALF_M + 1.0, 0.0), 0.0);
        assert_eq!(f.at(0.0, -TERRAIN_HALF_M - 1.0), 0.0);
    }

    #[test]
    fn the_edge_taper_reaches_zero_at_the_boundary() {
        let n = TERRAIN_GRID;
        assert_eq!(edge_taper(n / 2, n / 2, n), 1.0);
        assert_eq!(edge_taper(0, n / 2, n), 0.0);
        assert_eq!(edge_taper(n - 1, n - 1, n), 0.0);
        // Monotone on the way out from the middle — a taper with a bump in it is a
        // ridge that isn't there.
        let mut last = 1.0;
        for i in n / 2..n {
            let t = edge_taper(i, n / 2, n);
            assert!(t <= last + 1e-6, "taper rose at {i}");
            last = t;
        }
    }

    #[test]
    fn holes_fill_from_their_neighbours_and_big_voids_stay_flat() {
        let n = TERRAIN_GRID;
        let mut grid = vec![5.0f32; n * n];
        // A bridge-sized cut-out.
        for z in 10..14 {
            for x in 10..14 {
                grid[z * n + x] = f32::NAN;
            }
        }
        // …and a void far larger than the fill can reach.
        for z in 100..200 {
            for x in 100..200 {
                grid[z * n + x] = f32::NAN;
            }
        }
        fill_holes(&mut grid, n);
        assert!(grid[12 * n + 12].is_finite(), "small hole not filled");
        assert!(
            (grid[12 * n + 12] - 5.0).abs() < 1e-3,
            "fill invented a slope"
        );
        assert!(
            !grid[150 * n + 150].is_finite(),
            "a 2.5 km void must not be smeared shut"
        );
    }

    /// Signed area doubled — the sign is the winding, and the winding is whether
    /// the triangle is visible at all.
    fn winding(p: &[[f32; 3]], t: &[u32]) -> f32 {
        let (a, b, c) = (p[t[0] as usize], p[t[1] as usize], p[t[2] as usize]);
        (b[0] - a[0]) * (c[2] - a[2]) - (c[0] - a[0]) * (b[2] - a[2])
    }

    #[test]
    fn refinement_splits_long_edges_and_leaves_short_ones_alone() {
        // Two triangles sharing the diagonal of a 200 m square — earcut's answer
        // for a field, and the shape that cannot follow a hill.
        let mut positions = vec![
            [0.0, 0.0, 0.0],
            [200.0, 0.0, 0.0],
            [200.0, 0.0, 200.0],
            [0.0, 0.0, 200.0],
        ];
        let mut indices = vec![0, 1, 2, 0, 2, 3];
        let before = winding(&positions, &indices[0..3]).signum();
        let (mut uvs, mut normals, mut colors) = (vec![], vec![], vec![]);
        refine_for_terrain(
            &mut positions,
            &mut indices,
            &mut uvs,
            &mut normals,
            &mut colors,
            24.0,
        );
        // Every edge now short enough for the field to be expressed on it…
        for tri in indices.chunks_exact(3) {
            for (i, j) in [(0, 1), (1, 2), (2, 0)] {
                let (a, b) = (positions[tri[i] as usize], positions[tri[j] as usize]);
                let len = ((a[0] - b[0]).powi(2) + (a[2] - b[2]).powi(2)).sqrt();
                assert!(len <= 24.0 + 1e-3, "edge of {len} m survived refinement");
            }
            // …and still facing the way it was.
            assert_eq!(
                winding(&positions, tri).signum(),
                before,
                "a sub-triangle flipped"
            );
        }

        // Already-fine geometry is returned untouched: refinement is a cost paid
        // only where the ground is coarse.
        let mut small = vec![[0.0, 0.0, 0.0], [5.0, 0.0, 0.0], [0.0, 0.0, 5.0]];
        let mut small_idx = vec![0, 1, 2];
        refine_for_terrain(
            &mut small,
            &mut small_idx,
            &mut vec![],
            &mut vec![],
            &mut vec![],
            24.0,
        );
        assert_eq!(small.len(), 3);
        assert_eq!(small_idx, vec![0, 1, 2]);
    }

    #[test]
    fn refinement_shares_midpoints_so_a_slope_has_no_cracks() {
        // Two triangles across one long shared edge. If each invented its own
        // midpoint, displacement would pull them apart along that seam.
        let mut positions = vec![
            [0.0, 0.0, 0.0],
            [100.0, 0.0, 0.0],
            [0.0, 0.0, 100.0],
            [100.0, 0.0, 100.0],
        ];
        let mut indices = vec![0, 1, 2, 1, 3, 2];
        refine_for_terrain(
            &mut positions,
            &mut indices,
            &mut vec![],
            &mut vec![],
            &mut vec![],
            24.0,
        );
        // Every position is used by some triangle, and no position is duplicated:
        // both are what "the seam is stitched" means in index terms.
        let used: std::collections::HashSet<u32> = indices.iter().copied().collect();
        assert_eq!(
            used.len(),
            positions.len(),
            "orphaned or duplicated vertices"
        );
        let mut seen = std::collections::HashSet::new();
        for p in &positions {
            let key = (
                (p[0] * 100.0).round() as i64,
                (p[1] * 100.0).round() as i64,
                (p[2] * 100.0).round() as i64,
            );
            assert!(
                seen.insert(key),
                "duplicate vertex at {p:?} — that is a crack"
            );
        }
    }

    #[test]
    fn refinement_interpolates_the_attributes_it_carries() {
        let mut positions = vec![[0.0, 0.0, 0.0], [100.0, 0.0, 0.0], [0.0, 0.0, 100.0]];
        let mut indices = vec![0, 1, 2];
        let mut uvs = vec![[0.0, 0.0], [1.0, 0.0], [0.0, 1.0]];
        let mut normals = vec![[0.0, 1.0, 0.0]; 3];
        let mut colors = vec![
            [1.0, 0.0, 0.0, 1.0],
            [0.0, 1.0, 0.0, 1.0],
            [1.0, 0.0, 0.0, 1.0],
        ];
        refine_for_terrain(
            &mut positions,
            &mut indices,
            &mut uvs,
            &mut normals,
            &mut colors,
            24.0,
        );
        // A road's markings register with its geometry through UV0; dropping the
        // attribute arrays at refinement would strip the markings off every long
        // straight in the city.
        assert_eq!(uvs.len(), positions.len());
        assert_eq!(normals.len(), positions.len());
        assert_eq!(colors.len(), positions.len());
        for n in &normals {
            assert!((Vec3::from(*n).length() - 1.0).abs() < 1e-4);
        }
        for c in &colors {
            assert!(c.iter().all(|v| (0.0..=1.0).contains(v)));
        }
    }

    #[test]
    fn water_sits_level_over_the_polygon_it_covers() {
        let f = ramp_field(0.01);
        // A canal running up the slope: per-vertex would tilt it, the median holds
        // it level at the middle.
        let level = f.level_over([[-100.0, 0.0], [0.0, 0.0], [100.0, 0.0]]);
        assert!((level - f.at(0.0, 0.0)).abs() < 0.01);
        assert_eq!(TerrainField::default().level_over([[0.0, 0.0]]), 0.0);
    }

    #[test]
    fn baking_is_relative_to_the_anchor_and_flat_at_the_rim() {
        // A patch that rises 1 m per 100 m east, over a box around Groningen.
        let anchor = (53.2194, 6.5665, 14);
        let centre = wgs84_to_rd(anchor.0, anchor.1);
        let (w, h) = (64, 64);
        let (half_rd, mut samples) = (4_000.0f64, Vec::with_capacity(w * h));
        for _row in 0..h {
            for col in 0..w {
                let east = -half_rd + col as f64 / (w - 1) as f64 * half_rd * 2.0;
                samples.push(20.0 + (east / 100.0) as f32);
            }
        }
        let patch = HeightPatch {
            min: [centre[0] - half_rd, centre[1] - half_rd],
            max: [centre[0] + half_rd, centre[1] + half_rd],
            w,
            h,
            samples,
        };
        let (baked, datum) = bake(&patch, anchor, TERRAIN_HALF_M).expect("patch covers the field");
        // The datum is the ground at the anchor, and the field is zero there. The
        // tolerance is the synthetic patch's own 125 m sample spacing: `at` takes
        // the nearest measurement, deliberately, so the centre reads the cell it
        // lands in rather than an interpolation of four.
        assert!(
            (datum - 20.0).abs() < 1.0,
            "datum {datum} should be ~20 m NAP"
        );
        let n = TERRAIN_GRID;
        assert!(baked[(n / 2) * n + n / 2].abs() < 0.5);
        // A kilometre east is ~10 m up, relative — not 30 m absolute.
        let one_km =
            ((1_000.0 + TERRAIN_HALF_M) / (TERRAIN_HALF_M * 2.0) * (n - 1) as f32) as usize;
        let east = baked[(n / 2) * n + one_km];
        assert!((east - 10.0).abs() < 1.5, "1 km east read {east} m");
        // Every edge sample is flattened, so the field meets the flat world outside.
        for i in 0..n {
            assert_eq!(baked[i], 0.0, "north rim");
            assert_eq!(baked[(n - 1) * n + i], 0.0, "south rim");
            assert_eq!(baked[i * n], 0.0, "west rim");
            assert_eq!(baked[i * n + n - 1], 0.0, "east rim");
        }
    }
}
