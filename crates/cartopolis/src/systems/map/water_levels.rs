//! **Measured water levels**: how far a canal actually sits below its quays.
//!
//! Every water surface in this client stood at the ground's own height, because that
//! was the only height anything knew. [`terrain::fill_holes`] is what put it there,
//! and not by accident: AHN's ground model has no measurement over water at all — a
//! canal arrives as a *hole* — and the hole-filling pass, written so a cut-out
//! viaduct does not read as a pit, closes it with the average of its own two quays.
//! The canal was therefore not merely left flat; it was actively raised to street
//! level before the water sheet was laid on it.
//!
//! Measured over Groningen 2026-09-11, the error that produces is **two to five
//! metres**: the Diepenring's quays run +1.1 to +4.6 m NAP and its water is about
//! −0.8.
//!
//! # Where the number comes from, and why not from the other three places
//!
//! The Kadaster's **3D basisvoorziening** drapes the BGT's `Waterdeel_vlak` over AHN
//! and ships it as CityJSON `WaterBody` objects — a triangulated flat surface per
//! water body, carrying its own height in metres NAP. That is a *polygon with a level
//! on it*, which is the whole difficulty solved: national, CC BY 4.0 (the licence
//! this client already carries for 3DBAG), 53 bodies in the Groningen centre 2 km
//! block, each internally flat to a few centimetres.
//!
//! The three sources that look like they should answer this and do not
//! ([rijkswaterstaat-water-levels.md](../../../../docs/notes/rijkswaterstaat-water-levels.md)):
//!
//! * **Rijkswaterstaat's gauge network** measures 705 points to the centimetre and
//!   records *nothing* about which water each one governs — no geometry, no
//!   water-body id. Attaching a level to a drawn polygon would be a nearest-station
//!   guess.
//! * **The water boards' `peilgebied` areas** are published nationally and carry no
//!   level at all; every attribute is a code, a name or a status.
//! * **Rijkswaterstaat's own `peilgebieden`** has the right fields — `vastpeil`,
//!   `zomerpeil`, `winterpeil` — and they are null nearly everywhere: of six city
//!   centres sampled, only Utrecht had values.
//!
//! # What this module is, and what it deliberately is not
//!
//! A ring and a level, per z14 cell, streamed from our own host exactly as the flood
//! defences are. **Rings rather than the source triangles**: 3dfier emits a fine
//! triangulation (847 triangles for one ditch) and the boundary is what both
//! consumers want — the carve needs point-in-polygon and a quay wall would need the
//! outline. Extracting it costs one edge pass and a quarter-metre Douglas-Peucker,
//! and takes the Groningen 2 km block to **40 kB across six cells**, against the
//! 3.8 MB a single paving cell costs.
//!
//! The extractor must walk **directed** boundary edges — an edge is on the boundary
//! when its reverse is absent — so that each part of a multi-part body closes into
//! its own loop. Counting them undirected loses the winding, a vertex where two parts
//! touch gets several outgoing edges, and the walk chains through it into one
//! self-crossing polygon spanning both; even-odd point-in-polygon then answers
//! nonsense for it, silently. (Cost a day, 2026-09-12.)
//!
//! It is **not** the geometry the water is drawn from. That still comes from the
//! vector tile's own `water_polygons`, which covers the whole world; this only says
//! how far down. Where no level is known — outside the Netherlands, outside the
//! sheets, before the cell lands — every consumer behaves exactly as it did before
//! this module existed.

use bevy::prelude::*;
use bevy::tasks::IoTaskPool;

use crate::grid::AnchoredCamera;
use crate::platform::stream::{CellStream, Policy};
use crate::platform::{http, storage};
use crate::systems::cell_ring;
use crate::systems::coverage::CoverageSlot;
use crate::systems::globe::{GlobeState, GLOBE_SHOW_ALT};
use crate::systems::map_stream::MapStream;
use crate::systems::osm_buildings::tdbag_url;
use cartopolis_core::world::{lat_lng_to_tile, tile_to_meters};

/// Fixed cache/streaming zoom — the one the extractor bins into.
const WATER_ZOOM: u32 = cartopolis_geo::formats::CACHE_ZOOM;

/// Cells streamed around the camera. The same reach as the flood defences, and for a
/// cheaper reason: a whole city's water is tens of kilobytes, so the ring is sized by
/// what you can see rather than by what it costs.
const WATER_RADIUS: i64 = 3;

/// Concurrent cell fetches and the retry policy — our own host, so this is throughput
/// rather than courtesy.
///
/// Sixteen, not four: graded ground waits for these before it bakes
/// (`terrain::GRADED_WATER_HOLD_S`), and a stream dispatches a round per frame, so
/// 49 cells four at a time took thirteen frames of a slow start to come in — long
/// enough that the field baked without the last of them and baked again when they
/// landed, rebuilding the ground cells over water. The files are tens of kilobytes.
const WATER_CONCURRENCY: usize = 16;
const WATER_MAX_ATTEMPTS: u32 = 3;
const WATER_BACKOFF_SECS: f64 = 1.0;
const WATER_BACKOFF_DOUBLINGS: u32 = 3;
const WATER_RETRY_AFTER_SECS: f64 = 30.0;

/// How far outside a registered outline a drawn vertex may sit and still be taken as
/// belonging to it.
///
/// **The width of a disagreement between two surveys, not a fudge factor.** The rings
/// come from the BGT and the water this client *draws* comes from Shortbread; they
/// agree on where a canal is to about a metre, not to a centimetre. A drawn vertex
/// therefore routinely sits just outside the registered outline, and a strict
/// inside-test would leave exactly the boundary vertices — the ones that decide where
/// the surface meets the quay — with no level at all.
pub const JOIN_TOLERANCE_M: f32 = 3.0;

static COVERAGE: CoverageSlot = CoverageSlot::new("water", "/water/coverage.json", WATER_ZOOM);

/// One water body's outline and the height its surface stands at.
#[derive(Debug, Clone)]
pub struct WaterRing {
    /// Metres above NAP — absolute, like every other height this host serves, and put
    /// into the world's own frame by whoever reads it.
    pub level_nap_m: f32,
    /// The boundary in lat/lng, closed implicitly (the last point is not repeated).
    pub points: Vec<(f64, f64)>,
}

/// One streamed cell.
#[derive(Debug, Clone, Default)]
pub struct WaterCell {
    pub rings: Vec<WaterRing>,
}

/// Parse a cell body, through `cartopolis_geo::formats::water` — the type atlas's
/// `pipeline/water.rs` writes it with.
pub fn parse_water_cell(text: &str) -> Result<WaterCell, String> {
    let wire = cartopolis_geo::formats::water::parse(text)?;
    Ok(WaterCell {
        rings: wire
            .rings
            .into_iter()
            // A ring needs three corners to enclose anything. The extractor never
            // writes a shorter one; a truncated cache entry can, and a two-point
            // "ring" would make every point-in-polygon test below answer nonsense
            // rather than fail.
            .filter_map(|(level, flat)| {
                cartopolis_geo::formats::water::ring_points(&flat).map(|points| WaterRing {
                    level_nap_m: level as f32,
                    points,
                })
            })
            .collect(),
    })
}

/// The water currently streamed in, by cell.
#[derive(Resource)]
pub struct WaterLevels {
    stream: CellStream<(i64, i64), Result<WaterCell, String>>,
    cells: std::collections::HashMap<(i64, i64), WaterCell>,
    anchor: Option<(i64, i64, u32, f64)>,
    /// Bumped whenever the set of rings changes, so a consumer that baked them into a
    /// field knows to bake again.
    pub revision: u32,
}

impl Default for WaterLevels {
    fn default() -> Self {
        Self {
            stream: CellStream::new(Policy {
                max_in_flight: WATER_CONCURRENCY,
                min_interval_secs: 0.0,
                max_attempts: WATER_MAX_ATTEMPTS,
                backoff_secs: WATER_BACKOFF_SECS,
                backoff_doublings: WATER_BACKOFF_DOUBLINGS,
                cooldown_secs: WATER_RETRY_AFTER_SECS,
                // Nothing here is an entity, so the despawn queue is unused; the field
                // still has to say something and one is the honest answer for "there
                // is never a backlog".
                despawns_per_frame: 1,
            }),
            cells: std::collections::HashMap::new(),
            anchor: None,
            revision: 0,
        }
    }
}

impl WaterLevels {
    /// Every ring currently loaded.
    pub fn rings(&self) -> impl Iterator<Item = &WaterRing> {
        self.cells.values().flat_map(|c| c.rings.iter())
    }

    /// How many rings are loaded — the cheap "is there anything here?" test, and what
    /// `--dump-state` reports.
    #[must_use]
    pub fn len(&self) -> usize {
        self.cells.values().map(|c| c.rings.len()).sum()
    }

    /// Are cells still being fetched? The terrain bake holds for this beside the
    /// surface ring: every level that lands after a bake is another bake.
    #[must_use]
    pub fn loading(&self) -> bool {
        self.stream.busy()
    }

    fn clear(&mut self) {
        self.cells.clear();
        self.revision = self.revision.wrapping_add(1);
    }
}

/// Only ever busy over covered ground below the globe tier: nothing is requested
/// for a cell the host does not serve, and a stream that was never asked is quiet.
impl crate::systems::stream_watch::StreamBusy for WaterLevels {
    const NAME: &'static str = "water_levels";
    fn stream_busy(&self) -> bool {
        self.loading()
    }
}

/// The rings projected into the anchored world-XZ frame, ready for a worker.
///
/// Built once per terrain bake rather than consulted live, for the reason every other
/// `snapshot()` in this tree exists: the consumer is a closure on a pool thread and
/// cannot hold a `Res`. It also lets the expensive part — degrees to metres — happen
/// once per ring instead of once per sample.
#[derive(Clone, Default)]
pub struct WaterLevelField {
    /// `(level in NAP, ring in world XZ, bounding box)`.
    rings: std::sync::Arc<Vec<(f32, Vec<[f32; 2]>, [f32; 4])>>,
    /// Which rings are near which patch of ground ([`RingGrid`]).
    grid: std::sync::Arc<RingGrid>,
}

/// A uniform grid over the rings' bounding boxes, so a sample tests the rings near
/// it rather than all of them.
///
/// **The bounding-box reject was not enough and the arithmetic says why.** It is one
/// comparison, but it was one comparison *per ring*: over Groningen the register
/// yields **8,144** rings, the fine terrain tier samples 1024², and the product is
/// ~8.5 billion tests for one bake. Measured on the Deck 2026-09-21, that bake was
/// **14.0 s** at 600 m and 31.4 s across four of them at 2,500 m — where it was the
/// reason a capture sat out its whole `--wait` with `busy=["terrain"]` and never
/// settled. Nothing about it was visible as "water": it is the terrain stream, and
/// the cost is in a helper two modules away.
///
/// A grid rather than a tree because the query is a **point** and the population is
/// a city's canals: small, similar in size, spread evenly over the box. A bucket
/// lookup is two multiplies and an index, and the build is one pass.
#[derive(Default)]
struct RingGrid {
    /// South-west corner of the indexed area, in world XZ.
    origin: [f32; 2],
    /// Reciprocal of the bucket edge in metres.
    inv_cell: f32,
    cols: usize,
    rows: usize,
    /// Ring indices per bucket, row-major. A ring spanning several buckets is listed
    /// in each: duplication in the index, never in the ring list.
    buckets: Vec<Vec<u32>>,
}

/// Buckets across the longer side of the indexed area.
///
/// 128 puts a city's 12.8 km terrain box on a 100 m grid, which is the scale of the
/// canals themselves — so a bucket holds a handful of rings and a big lake is listed
/// in a bounded number of them. Higher would spend more on a bake's build pass than
/// it saves in its lookups; lower leaves the scan this exists to remove.
const RING_GRID_SPAN: usize = 128;

impl RingGrid {
    fn build(rings: &[(f32, Vec<[f32; 2]>, [f32; 4])]) -> Self {
        if rings.is_empty() {
            return Self::default();
        }
        let mut area = [f32::MAX, f32::MAX, f32::MIN, f32::MIN];
        for (_, _, bb) in rings {
            area[0] = area[0].min(bb[0]);
            area[1] = area[1].min(bb[1]);
            area[2] = area[2].max(bb[2]);
            area[3] = area[3].max(bb[3]);
        }
        let span = (area[2] - area[0]).max(area[3] - area[1]).max(1.0);
        let cell = span / RING_GRID_SPAN as f32;
        let inv_cell = 1.0 / cell;
        let cols = (((area[2] - area[0]) * inv_cell).ceil() as usize + 1).max(1);
        let rows = (((area[3] - area[1]) * inv_cell).ceil() as usize + 1).max(1);
        let mut grid = Self {
            origin: [area[0], area[1]],
            inv_cell,
            cols,
            rows,
            buckets: vec![Vec::new(); cols * rows],
        };
        for (i, (_, _, bb)) in rings.iter().enumerate() {
            let (x0, z0) = grid.cell_of(bb[0], bb[1]);
            let (x1, z1) = grid.cell_of(bb[2], bb[3]);
            for z in z0..=z1 {
                for x in x0..=x1 {
                    grid.buckets[z * cols + x].push(i as u32);
                }
            }
        }
        grid
    }

    /// The bucket a world-XZ point falls in, clamped to the indexed area — a point
    /// outside it is answered by the edge bucket and then rejected by the rings'
    /// own bounding boxes, which is cheaper than a second range test here.
    fn cell_of(&self, wx: f32, wz: f32) -> (usize, usize) {
        let fx = ((wx - self.origin[0]) * self.inv_cell).floor();
        let fz = ((wz - self.origin[1]) * self.inv_cell).floor();
        (
            (fx.max(0.0) as usize).min(self.cols.saturating_sub(1)),
            (fz.max(0.0) as usize).min(self.rows.saturating_sub(1)),
        )
    }

    /// The rings whose bounding box could contain this point.
    fn near(&self, wx: f32, wz: f32) -> &[u32] {
        if self.buckets.is_empty() {
            return &[];
        }
        let (x, z) = self.cell_of(wx, wz);
        &self.buckets[z * self.cols + x]
    }

    /// The rings within `tol` metres, as the buckets that reach that far.
    fn near_within(&self, wx: f32, wz: f32, tol: f32) -> impl Iterator<Item = u32> + '_ {
        // An empty index has no bucket to clamp into — `cell_of` would answer (0, 0)
        // and the lookup would index a zero-length `buckets`. `near` guards the same
        // way; this one did not, and a field with no water in it is the ordinary case
        // outside the register's coverage.
        let empty = self.buckets.is_empty();
        let (x0, z0) = self.cell_of(wx - tol, wz - tol);
        let (x1, z1) = self.cell_of(wx + tol, wz + tol);
        let cols = self.cols;
        (z0..=z1).take_while(move |_| !empty).flat_map(move |z| {
            (x0..=x1).flat_map(move |x| self.buckets[z * cols + x].iter().copied())
        })
    }
}

impl WaterLevelField {
    /// The **only** way to make one, so a ring list and its index cannot disagree.
    ///
    /// Not a nicety: `overlapping_bodies_carve_to_the_lower_level` used to reach
    /// through `Arc::get_mut` and extend `rings` in place, which is harmless while the
    /// list is the whole structure and silently wrong the moment anything is derived
    /// from it — the added ring was invisible to every query, and the test failed by
    /// answering the *shallower* level. Building the two together is what removes the
    /// possibility rather than documenting it.
    fn from_rings(rings: Vec<(f32, Vec<[f32; 2]>, [f32; 4])>) -> Self {
        let grid = RingGrid::build(&rings);
        Self {
            rings: std::sync::Arc::new(rings),
            grid: std::sync::Arc::new(grid),
        }
    }

    /// Project every loaded ring into world XZ, keeping its level in **NAP**.
    ///
    /// Deliberately not converted to the world's relative frame here. The terrain bakes
    /// work in absolute metres right up to their last line, where one subtraction turns
    /// the whole grid relative; handing them a level that had already been shifted would
    /// have it shifted twice. It also means this can be built before the datum is known.
    #[must_use]
    pub fn build(levels: &WaterLevels, anchor: (f64, f64, u32)) -> Self {
        let rings = levels
            .rings()
            .filter_map(|r| {
                let pts: Vec<[f32; 2]> = r
                    .points
                    .iter()
                    .map(|&(lat, lng)| {
                        let (wx, wz) = crate::systems::geo_nav::lat_lng_to_world(anchor, lat, lng);
                        [wx as f32, wz as f32]
                    })
                    .collect();
                if pts.len() < 3 {
                    return None;
                }
                let mut bb = [f32::MAX, f32::MAX, f32::MIN, f32::MIN];
                for p in &pts {
                    bb[0] = bb[0].min(p[0]);
                    bb[1] = bb[1].min(p[1]);
                    bb[2] = bb[2].max(p[0]);
                    bb[3] = bb[3].max(p[1]);
                }
                Some((r.level_nap_m, pts, bb))
            })
            .collect();
        Self::from_rings(rings)
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.rings.is_empty()
    }

    /// Set `row[x] = mark` for every sample of lattice row `z` that lies in any
    /// ring — exactly the samples where [`Self::level_at`] answers `Some`.
    ///
    /// **A scanline rather than a point test per sample.** The field's bank flags
    /// asked `level_at` of all 4.2 million samples, a bucket lookup and an
    /// even-odd walk of every nearby ring's edges each time, and that alone was
    /// **5.0 s** of the field bake with the rest of it at 110 ms (container,
    /// 2026-09-26, four threads) — time every ground and building cell waits for.
    /// Along one row a ring's edges cross the row's `z` at fixed `x`s, so each
    /// ring is walked once per row and every sample in its box is decided by
    /// counting sorted crossings. The crossing is the same arithmetic on the same
    /// numbers as `point_in_ring`, and the box test the same as `level_at`'s, so
    /// the answers match it sample for sample (`a_wet_row_matches_the_point_test`).
    pub fn mark_wet_row(
        &self,
        lattice: &cartopolis_geo::terrain_grade::Lattice,
        z: usize,
        row: &mut [u8],
        mark: u8,
    ) {
        self.walk_row(lattice, z, row, |slot, _| *slot = mark);
    }

    /// [`Self::level_at`] at every sample of `lattice`, NaN where there is no
    /// water: the lowest level of the rings a sample is in, as `level_at` takes.
    ///
    /// For the carve, which asked `level_at` of every sample on one thread — the
    /// same 4.2 million point tests [`Self::mark_wet_row`] replaced, paid again on
    /// ground graded here. By the same scanline, row-parallel.
    #[must_use]
    pub fn levels_on(&self, lattice: &cartopolis_geo::terrain_grade::Lattice) -> Vec<f32> {
        let mut levels = vec![f32::NAN; lattice.n * lattice.n];
        crate::systems::terrain::for_rows(&mut levels, lattice.n, |z, row| {
            self.walk_row(lattice, z, row, |slot, level| {
                if !(*slot <= level) {
                    *slot = level;
                }
            });
        });
        levels
    }

    /// Call `inside(slot, level)` for every sample of row `z` inside each ring, once
    /// per ring that contains it. See [`Self::mark_wet_row`] for why a scanline.
    fn walk_row<T>(
        &self,
        lattice: &cartopolis_geo::terrain_grade::Lattice,
        z: usize,
        row: &mut [T],
        mut inside: impl FnMut(&mut T, f32),
    ) {
        let wz = lattice.at(0, z)[1];
        let mut crossings: Vec<f32> = Vec::new();
        for (level, pts, bb) in self.rings.iter() {
            if wz < bb[1] || wz > bb[3] {
                continue;
            }
            crossings.clear();
            let n = pts.len();
            for i in 0..n {
                let a = pts[i];
                let b = pts[(i + 1) % n];
                if (a[1] > wz) != (b[1] > wz) {
                    let t = (wz - a[1]) / (b[1] - a[1]);
                    crossings.push(a[0] + t * (b[0] - a[0]));
                }
            }
            if crossings.is_empty() {
                continue;
            }
            crossings.sort_unstable_by(f32::total_cmp);
            // The samples in the ring's box, found from the lattice arithmetic
            // itself rather than solved for, so a box edge falling exactly on a
            // sample is decided the way `level_at` decides it.
            let first = ((bb[0] - lattice.origin[0]) / lattice.step)
                .floor()
                .max(0.0) as usize;
            let mut passed = 0;
            for (x, slot) in row.iter_mut().enumerate().skip(first.saturating_sub(1)) {
                let wx = lattice.at(x, z)[0];
                if wx < bb[0] {
                    continue;
                }
                if wx > bb[2] {
                    break;
                }
                // `point_in_ring` flips once per crossing with `wx < crossing`.
                while passed < crossings.len() && !(wx < crossings[passed]) {
                    passed += 1;
                }
                if (crossings.len() - passed) % 2 == 1 {
                    inside(slot, *level);
                }
            }
        }
    }

    /// One ring at one level, for tests in this crate that need a field without a
    /// streamed cell behind it.
    #[cfg(test)]
    #[must_use]
    pub fn build_for_test(level_nap_m: f32, ring: &[[f32; 2]]) -> Self {
        let mut bb = [f32::MAX, f32::MAX, f32::MIN, f32::MIN];
        for p in ring {
            bb[0] = bb[0].min(p[0]);
            bb[1] = bb[1].min(p[1]);
            bb[2] = bb[2].max(p[0]);
            bb[3] = bb[3].max(p[1]);
        }
        Self::from_rings(vec![(level_nap_m, ring.to_vec(), bb)])
    }

    /// The water bodies over a world box, as one number: two fields agree on it
    /// exactly when the same rings at the same levels reach into the box. Summed,
    /// so the order the rings were loaded in does not matter.
    #[must_use]
    pub fn digest_over(&self, min: [f32; 2], max: [f32; 2]) -> u64 {
        // A set, not a sum over the list: a body that spans several cells arrives
        // once per cell file, and a camera that loads the next cell of a canal must
        // not change the answer for every cell the canal reaches.
        let bodies: std::collections::BTreeSet<u64> = self
            .rings
            .iter()
            .filter(|(_, _, bb)| {
                bb[0] <= max[0] && bb[2] >= min[0] && bb[1] <= max[1] && bb[3] >= min[1]
            })
            .map(|(level, pts, bb)| {
                let mut h: u64 = 0xcbf2_9ce4_8422_2325;
                for v in [
                    level.to_bits(),
                    bb[0].to_bits(),
                    bb[1].to_bits(),
                    bb[2].to_bits(),
                    bb[3].to_bits(),
                ] {
                    h ^= u64::from(v);
                    h = h.wrapping_mul(0x100_0000_01b3);
                }
                h ^ pts.len() as u64
            })
            .collect();
        bodies.iter().fold(0u64, |acc, h| acc.wrapping_add(*h))
    }

    /// The lowest surface among the loaded bodies, in metres above NAP.
    #[must_use]
    pub fn lowest_nap(&self) -> Option<f32> {
        self.rings
            .iter()
            .map(|(level, _, _)| *level)
            .fold(None, |acc: Option<f32>, l| {
                Some(acc.map_or(l, |a| a.min(l)))
            })
    }

    /// The water surface height at a world-XZ point, in metres above NAP, or `None`
    /// where no ring covers it.
    ///
    /// Called once per sample of a 1024² grid, against a register that yields
    /// thousands of rings over a city — so it goes through [`RingGrid`] rather than
    /// over the whole list. The bounding-box test is still here and still earns its
    /// keep: a bucket holds the rings whose *box* overlaps it, not the rings that
    /// contain the point.
    ///
    /// Where rings overlap — a bridge's water and the canal under it are separate
    /// bodies in the register — the **lowest** wins. Two surfaces claiming one point is
    /// a disagreement about which body it belongs to, and carving to the lower of them
    /// leaves the higher one floating on its own sheet rather than burying it.
    #[must_use]
    pub fn level_at(&self, wx: f32, wz: f32) -> Option<f32> {
        let mut found: Option<f32> = None;
        for &i in self.grid.near(wx, wz) {
            let (level, pts, bb) = &self.rings[i as usize];
            if wx < bb[0] || wx > bb[2] || wz < bb[1] || wz > bb[3] {
                continue;
            }
            if !point_in_ring(pts, wx, wz) {
                continue;
            }
            found = Some(match found {
                Some(prev) => prev.min(*level),
                None => *level,
            });
        }
        found
    }

    /// The water surface height near a world-XZ point, in metres above NAP.
    ///
    /// Inside a ring first; failing that, the nearest ring within `tol` metres. See
    /// [`JOIN_TOLERANCE_M`] for why that tolerance is the join and not slop. Nearest by
    /// distance to the ring's *edge*, so a point just outside a canal takes that
    /// canal's level rather than a wider body's whose centroid happens to be closer.
    #[must_use]
    pub fn level_near(&self, wx: f32, wz: f32, tol: f32) -> Option<f32> {
        if let Some(level) = self.level_at(wx, wz) {
            return Some(level);
        }
        let mut best: Option<(f32, f32)> = None;
        // The buckets the tolerance reaches, not the whole list. A ring whose box
        // straddles several of them is offered more than once; `distance_to_ring` is
        // idempotent and the `min` below does not care.
        for i in self.grid.near_within(wx, wz, tol) {
            let (level, pts, bb) = &self.rings[i as usize];
            if wx < bb[0] - tol || wx > bb[2] + tol || wz < bb[1] - tol || wz > bb[3] + tol {
                continue;
            }
            let d = distance_to_ring(pts, wx, wz);
            if d <= tol && best.is_none_or(|(bd, _)| d < bd) {
                best = Some((d, *level));
            }
        }
        best.map(|(_, level)| level)
    }
}

/// Shortest distance from a point to a closed ring's boundary.
fn distance_to_ring(pts: &[[f32; 2]], x: f32, z: f32) -> f32 {
    let n = pts.len();
    let mut best = f32::MAX;
    for i in 0..n {
        let a = pts[i];
        let b = pts[(i + 1) % n];
        let (ex, ez) = (b[0] - a[0], b[1] - a[1]);
        let len2 = ex * ex + ez * ez;
        let t = if len2 > 0.0 {
            (((x - a[0]) * ex + (z - a[1]) * ez) / len2).clamp(0.0, 1.0)
        } else {
            0.0
        };
        let (px, pz) = (a[0] + ex * t, a[1] + ez * t);
        best = best.min((x - px).hypot(z - pz));
    }
    best
}

/// Even-odd point-in-polygon over a closed ring whose last point is not repeated.
fn point_in_ring(pts: &[[f32; 2]], x: f32, z: f32) -> bool {
    let mut inside = false;
    let n = pts.len();
    for i in 0..n {
        let a = pts[i];
        let b = pts[(i + 1) % n];
        if (a[1] > z) != (b[1] > z) {
            let t = (z - a[1]) / (b[1] - a[1]);
            if x < a[0] + t * (b[0] - a[0]) {
                inside = !inside;
            }
        }
    }
    inside
}

fn water_covers(cx: i64, cy: i64) -> bool {
    COVERAGE.covers(cx, cy)
}

async fn fetch_water_text(cx: i64, cy: i64) -> Result<String, String> {
    let key = format!("water/{WATER_ZOOM}/{cx}/{cy}.json");
    if let Some(cached) = storage::get_string(storage::Namespace::Cache, &key).await {
        return Ok(cached);
    }
    let url = format!("{}/water/{}/{}/{}.json", tdbag_url(), WATER_ZOOM, cx, cy);
    let bytes = http::fetch(http::Request::get(url)).await?;
    let text = String::from_utf8(bytes).map_err(|e| e.to_string())?;
    storage::put(storage::Namespace::Cache, &key, text.as_bytes()).await;
    Ok(text)
}

/// Stream the water cells around the camera into [`WaterLevels`].
///
/// The same shape as `update_dikes` and for the same reasons: nothing here is spawned,
/// uploaded or despawned, so there is no ledger, no per-frame build cap and no partial
/// application — only the ring and the re-anchor teardown.
pub fn update_water_levels(
    mut levels: ResMut<WaterLevels>,
    stream: Res<MapStream>,
    camera: AnchoredCamera,
    globe: Res<GlobeState>,
    time: Res<Time>,
) {
    let now = time.elapsed_secs_f64();
    COVERAGE.ensure(now);

    let Some((clat, clng, base_zoom)) = stream.geo_anchor() else {
        return;
    };

    // A re-anchor does not move the water, but the ring is expressed in cells around
    // the old anchor, so the bookkeeping starts over.
    if levels.stream.sync(stream.anchor_generation) {
        levels.stream.reset();
        levels.clear();
        levels.anchor = None;
    }

    if levels.anchor.is_none() {
        let (ax, ay) = lat_lng_to_tile(clat, clng, base_zoom);
        levels.anchor = Some((ax, ay, base_zoom, clat));
    }
    let (ax, ay, z, lat) = levels.anchor.expect("set just above");
    let tile_m = tile_to_meters(z, lat) as f32;

    let Some((cell, tf)) = camera.get() else {
        return;
    };
    if globe.altitude_m(cell, tf) as f32 > GLOBE_SHOW_ALT {
        return;
    }

    let centre = cell_ring::coarsen(
        cell_ring::tile_at((ax, ay), tile_m, camera.stream_xz_at(cell, tf)),
        z,
        WATER_ZOOM,
    );
    cell_ring::request_ring(&mut levels.stream, centre, WATER_RADIUS, now, |c, _| {
        water_covers(c.0, c.1)
    });

    let sink = levels.stream.sink();
    while let Some((cx, cy)) = levels.stream.next_job(now) {
        let sink = sink.clone();
        IoTaskPool::get()
            .spawn(async move {
                let text = fetch_water_text(cx, cy).await;
                let parsed = text.and_then(|t| parse_water_cell(&t));
                if let Ok(mut g) = sink.lock() {
                    g.push(((cx, cy), parsed));
                }
            })
            .detach();
    }

    // No distance eviction, for the reason `building_facts` gives: walking far enough
    // to leave the ring re-anchors the map, and a re-anchor clears everything above. A
    // city's water is tens of kilobytes.
    for (c, result) in levels.stream.take_results() {
        match result {
            Ok(parsed) => {
                levels.stream.succeeded(&c);
                levels.cells.insert(c, parsed);
                levels.revision = levels.revision.wrapping_add(1);
            }
            Err(e) => {
                let attempts = levels.stream.failed(&c, now);
                if levels.stream.exhausted(&c) {
                    tracing::debug!(cx = c.0, cy = c.1, attempts, error = %e,
                                    "water-level cell unavailable");
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A field of many small rings plus one that spans most of the box, which is the
    /// case the grid has to get right: a big lake belongs to many buckets, and a ring
    /// listed in only the bucket its corner falls in would be invisible from the rest
    /// of itself.
    fn scattered_field() -> WaterLevelField {
        let mut rings = Vec::new();
        for i in 0..40 {
            for j in 0..40 {
                let (x, z) = (i as f32 * 30.0 - 600.0, j as f32 * 30.0 - 600.0);
                let pts = vec![[x, z], [x + 12.0, z], [x + 12.0, z + 12.0], [x, z + 12.0]];
                let bb = [x, z, x + 12.0, z + 12.0];
                rings.push((-0.5 - i as f32 * 0.01, pts, bb));
            }
        }
        // The spanner: one body across the whole indexed area, at a level low enough
        // that the `min` rule makes it observable wherever it overlaps a small one.
        rings.push((
            -9.0,
            vec![
                [-500.0, -500.0],
                [500.0, -500.0],
                [500.0, 500.0],
                [-500.0, 500.0],
            ],
            [-500.0, -500.0, 500.0, 500.0],
        ));
        WaterLevelField::from_rings(rings)
    }

    /// The index may not change a single answer — it is a way of asking the same
    /// question, and the bake it speeds up decides where the ground is.
    ///
    /// Brute force is the oracle because it is what this replaced: `level_at` walked
    /// every ring, which over Groningen is 8,144 of them per sample of a 1024² grid.
    #[test]
    fn the_ring_index_answers_exactly_what_a_full_scan_does() {
        let field = scattered_field();
        let scan_at = |wx: f32, wz: f32| -> Option<f32> {
            let mut found: Option<f32> = None;
            for (level, pts, bb) in field.rings.iter() {
                if wx < bb[0] || wx > bb[2] || wz < bb[1] || wz > bb[3] {
                    continue;
                }
                if !point_in_ring(pts, wx, wz) {
                    continue;
                }
                found = Some(found.map_or(*level, |p: f32| p.min(*level)));
            }
            found
        };
        // Deliberately not on the lattice the rings were built on: the samples that
        // matter are the ones that fall between bodies, outside the indexed area, and
        // exactly on a bucket boundary.
        let mut checked = 0;
        for i in -70..70 {
            for j in -70..70 {
                let (wx, wz) = (i as f32 * 9.7, j as f32 * 9.7);
                assert_eq!(
                    field.level_at(wx, wz),
                    scan_at(wx, wz),
                    "index and scan disagree at ({wx}, {wz})"
                );
                checked += 1;
            }
        }
        assert!(checked > 10_000, "the sweep did not cover enough ground");
        // Far outside the indexed area, where `cell_of` clamps to an edge bucket and
        // the rings' own boxes are what must reject the point.
        for (wx, wz) in [(9e5, 0.0), (0.0, -9e5), (-9e5, 9e5)] {
            assert_eq!(field.level_at(wx, wz), None);
        }
    }

    /// Same contract for the tolerant query, which reaches across buckets.
    #[test]
    fn the_ring_index_answers_the_tolerant_query_exactly_too() {
        let field = scattered_field();
        const TOL: f32 = 7.0;
        let scan_near = |wx: f32, wz: f32| -> Option<f32> {
            if let Some(level) = field.level_at(wx, wz) {
                return Some(level);
            }
            let mut best: Option<(f32, f32)> = None;
            for (level, pts, bb) in field.rings.iter() {
                if wx < bb[0] - TOL || wx > bb[2] + TOL || wz < bb[1] - TOL || wz > bb[3] + TOL {
                    continue;
                }
                let d = distance_to_ring(pts, wx, wz);
                if d <= TOL && best.is_none_or(|(bd, _)| d < bd) {
                    best = Some((d, *level));
                }
            }
            best.map(|(_, level)| level)
        };
        for i in -60..60 {
            for j in -60..60 {
                let (wx, wz) = (i as f32 * 11.3, j as f32 * 11.3);
                assert_eq!(
                    field.level_near(wx, wz, TOL),
                    scan_near(wx, wz),
                    "index and scan disagree at ({wx}, {wz})"
                );
            }
        }
    }

    /// An empty register must not build a grid that any query can index into.
    #[test]
    fn an_empty_field_indexes_to_nothing() {
        let field = WaterLevelField::default();
        assert!(field.is_empty());
        assert_eq!(field.level_at(0.0, 0.0), None);
        assert_eq!(field.level_near(0.0, 0.0, 50.0), None);
    }

    /// The wire format is a seam with an extractor in another repository, so pin the
    /// shape rather than trusting that both sides were changed together.
    #[test]
    fn a_cell_round_trips_from_the_extractors_shape() {
        let cell = parse_water_cell(
            r#"{"rings":[[-0.77,[53.2357,6.5584,53.2358,6.5585,53.2359,6.5584]]]}"#,
        )
        .expect("parse");
        assert_eq!(cell.rings.len(), 1);
        assert!((cell.rings[0].level_nap_m - -0.77).abs() < 1e-6);
        assert_eq!(cell.rings[0].points.len(), 3);
        assert!((cell.rings[0].points[0].0 - 53.2357).abs() < 1e-9);
    }

    /// A truncated cache entry must cost the ring, never produce one that answers
    /// point-in-polygon with nonsense.
    #[test]
    fn a_ring_too_short_to_enclose_anything_is_dropped() {
        let cell = parse_water_cell(r#"{"rings":[[-0.8,[53.0,6.0,53.1,6.1]]]}"#).expect("parse");
        assert!(cell.rings.is_empty(), "a two-point ring was kept");
        let odd =
            parse_water_cell(r#"{"rings":[[-0.8,[53.0,6.0,53.1,6.1,53.2]]]}"#).expect("parse");
        assert!(
            odd.rings.is_empty(),
            "a ring with a half coordinate was kept"
        );
    }

    /// An empty body is a valid answer — a cell with no water in it.
    #[test]
    fn an_empty_cell_is_not_an_error() {
        assert!(parse_water_cell("{}").expect("parse").rings.is_empty());
        assert!(parse_water_cell(r#"{"rings":[]}"#)
            .expect("parse")
            .rings
            .is_empty());
    }

    /// One square ring, as a `(level, points, bbox)` triple.
    fn square_ring(level: f32, half: f32) -> (f32, Vec<[f32; 2]>, [f32; 4]) {
        (
            level,
            vec![[-half, -half], [half, -half], [half, half], [-half, half]],
            [-half, -half, half, half],
        )
    }

    fn square(level: f32, half: f32) -> WaterLevelField {
        WaterLevelField::from_rings(vec![square_ring(level, half)])
    }

    fn ring(level: f32, pts: Vec<[f32; 2]>) -> (f32, Vec<[f32; 2]>, [f32; 4]) {
        let mut bb = [f32::MAX, f32::MAX, f32::MIN, f32::MIN];
        for p in &pts {
            bb = [
                bb[0].min(p[0]),
                bb[1].min(p[1]),
                bb[2].max(p[0]),
                bb[3].max(p[1]),
            ];
        }
        (level, pts, bb)
    }

    /// The scanline answers exactly what the point test answers, including the
    /// cases a scanline gets wrong when it is not the same arithmetic: samples
    /// landing exactly on vertices and edges (an integer lattice over integer
    /// rings), a concave ring, rings overlapping and nested, a sliver, and a
    /// lattice whose rows run off the rings' boxes.
    #[test]
    fn a_wet_row_matches_the_point_test() {
        let field = WaterLevelField::from_rings(vec![
            square_ring(-1.0, 10.0),
            square_ring(-3.0, 4.0),
            // An L, concave at (2, 2).
            ring(
                0.0,
                vec![
                    [-20.0, -20.0],
                    [2.0, -20.0],
                    [2.0, 2.0],
                    [14.0, 2.0],
                    [14.0, 9.0],
                    [-20.0, 9.0],
                ],
            ),
            // A diagonal sliver, and a triangle whose edges cross rows at fractions.
            ring(
                0.5,
                vec![[-30.0, 30.0], [30.0, -29.5], [30.2, -29.3], [-29.8, 30.2]],
            ),
            ring(0.2, vec![[5.3, 12.1], [27.7, 18.9], [11.4, 26.6]]),
        ]);
        for (origin, step, n) in [
            ([-32.0, -32.0], 1.0, 65),
            ([-31.7, -33.2], 0.37, 180),
            ([-5.0, 25.0], 0.5, 40),
        ] {
            let lattice = cartopolis_geo::terrain_grade::Lattice { n, origin, step };
            let levels = field.levels_on(&lattice);
            let mut wet = 0;
            for z in 0..n {
                let mut row = vec![0u8; n];
                field.mark_wet_row(&lattice, z, &mut row, 7);
                for (x, &got) in row.iter().enumerate() {
                    let [wx, wz] = lattice.at(x, z);
                    let want = field.level_at(wx, wz);
                    assert_eq!(got == 7, want.is_some(), "({wx}, {wz}) step {step}");
                    // The lowest level where rings overlap, as `level_at` answers.
                    let level = levels[z * n + x];
                    assert_eq!(level.is_finite().then_some(level), want, "({wx}, {wz})");
                    wet += usize::from(want.is_some());
                }
            }
            assert!(wet > 0, "a lattice with no water tests nothing");
        }
    }

    #[test]
    fn a_point_inside_a_ring_gets_its_level_and_one_outside_gets_nothing() {
        let f = square(-2.5, 10.0);
        assert_eq!(f.level_at(0.0, 0.0), Some(-2.5));
        assert_eq!(f.level_at(9.0, -9.0), Some(-2.5));
        assert_eq!(f.level_at(11.0, 0.0), None);
        assert_eq!(f.level_at(0.0, -50.0), None);
    }

    /// The two surveys disagree by about a metre, so a drawn vertex just outside the
    /// registered outline must still find its own body.
    #[test]
    fn a_point_just_outside_still_joins_to_the_body_it_belongs_to() {
        let f = square(-2.5, 10.0);
        assert_eq!(f.level_at(11.0, 0.0), None, "strictly outside");
        assert_eq!(f.level_near(11.0, 0.0, JOIN_TOLERANCE_M), Some(-2.5));
        assert_eq!(
            f.level_near(20.0, 0.0, JOIN_TOLERANCE_M),
            None,
            "ten metres out is a different place, not a survey disagreement"
        );
    }

    /// Overlapping bodies must not fight. The lower one wins, so the higher surface
    /// floats on its own sheet instead of being buried by the carve under it.
    #[test]
    fn overlapping_bodies_carve_to_the_lower_level() {
        let f = WaterLevelField::from_rings(vec![square_ring(-1.0, 10.0), square_ring(-3.0, 5.0)]);
        assert_eq!(f.level_at(0.0, 0.0), Some(-3.0), "the deeper body must win");
        assert_eq!(f.level_at(8.0, 0.0), Some(-1.0), "outside the deeper one");
        assert_eq!(f.lowest_nap(), Some(-3.0));
    }

    /// Nothing loaded must read exactly as it did before this module existed.
    #[test]
    fn an_empty_field_answers_nothing_anywhere() {
        let f = WaterLevelField::default();
        assert!(f.is_empty());
        assert_eq!(f.level_at(0.0, 0.0), None);
        assert_eq!(f.level_near(0.0, 0.0, JOIN_TOLERANCE_M), None);
        assert_eq!(f.lowest_nap(), None);
    }
}
