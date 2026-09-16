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

use crate::components::FreeCamera;
use crate::grid::WORLD_CELL_EDGE;
use crate::platform::fetch_gate;
use crate::platform::stream::{CellStream, Policy};
use crate::platform::{http, storage};
use crate::systems::coverage::CoverageSlot;
use crate::systems::globe::{GlobeState, GLOBE_SHOW_ALT};
use crate::systems::map_stream::MapStream;
use crate::systems::osm_buildings::tdbag_url;
use big_space::prelude::GridCell;
use cartopolis_core::world::{lat_lng_to_tile, tile_to_meters};

/// Fixed cache/streaming zoom — must match the bin the extractor writes into.
const WATER_ZOOM: u32 = 14;

/// Cells streamed around the camera. The same reach as the flood defences, and for a
/// cheaper reason: a whole city's water is tens of kilobytes, so the ring is sized by
/// what you can see rather than by what it costs.
const WATER_RADIUS: i64 = 3;

/// Concurrent cell fetches and the retry policy — our own host, so this is throughput
/// rather than courtesy.
const WATER_CONCURRENCY: usize = 4;
const WATER_MAX_ATTEMPTS: u32 = 3;
const WATER_BACKOFF_SECS: f64 = 1.0;
const WATER_BACKOFF_DOUBLINGS: u32 = 3;
const WATER_RETRY_AFTER_SECS: f64 = 30.0;

/// How far below its surface a canal's bed is taken to be, in metres.
///
/// **This one number is not measured, and it is the only one here that isn't.** AHN's
/// laser does not come back off water and the BGT records the surface, so no source in
/// this client's reach says how deep a canal is. The ground is therefore carved to the
/// measured surface *minus this*, purely so the water sheet has something to stand its
/// own depth rung on rather than lying coplanar with the bed — the z-fight this
/// codebase spends a whole depth ladder avoiding.
///
/// Deliberately shallow. Being wrong by a metre of invisible depth under an opaque
/// sheet costs nothing; carving a trench deeper than the quays are tall would show.
const WATER_BED_M: f32 = 0.5;

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

/// Parse a cell body. Public for the round-trip test, and because the format is a seam
/// with an extractor in another repository.
pub fn parse_water_cell(text: &str) -> Result<WaterCell, String> {
    #[derive(serde::Deserialize)]
    struct Wire {
        #[serde(default)]
        rings: Vec<(f32, Vec<f64>)>,
    }
    let wire: Wire = serde_json::from_str(text).map_err(|e| e.to_string())?;
    Ok(WaterCell {
        rings: wire
            .rings
            .into_iter()
            // A ring needs three corners to enclose anything. The extractor never
            // writes a shorter one; a truncated cache entry can, and a two-point
            // "ring" would make every point-in-polygon test below answer nonsense
            // rather than fail.
            .filter(|(_, flat)| flat.len() >= 6 && flat.len() % 2 == 0)
            .map(|(level_nap_m, flat)| WaterRing {
                level_nap_m,
                points: flat.chunks_exact(2).map(|p| (p[0], p[1])).collect(),
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

    fn clear(&mut self) {
        self.cells.clear();
        self.revision = self.revision.wrapping_add(1);
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
}

impl WaterLevelField {
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
        Self {
            rings: std::sync::Arc::new(rings),
        }
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.rings.is_empty()
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
        Self {
            rings: std::sync::Arc::new(vec![(level_nap_m, ring.to_vec(), bb)]),
        }
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
    /// The bounding-box test first is not an optimisation detail: this is called once
    /// per no-data sample of a 1024² grid, and a city cell holds hundreds of rings.
    ///
    /// Where rings overlap — a bridge's water and the canal under it are separate
    /// bodies in the register — the **lowest** wins. Two surfaces claiming one point is
    /// a disagreement about which body it belongs to, and carving to the lower of them
    /// leaves the higher one floating on its own sheet rather than burying it.
    #[must_use]
    pub fn level_at(&self, wx: f32, wz: f32) -> Option<f32> {
        let mut found: Option<f32> = None;
        for (level, pts, bb) in self.rings.iter() {
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
        for (level, pts, bb) in self.rings.iter() {
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

/// How far below the measured surface the ground is carved. See [`WATER_BED_M`].
#[must_use]
pub fn bed_below_surface_m() -> f32 {
    WATER_BED_M
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
    camera: Query<(&GridCell, &Transform), With<FreeCamera>>,
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

    let Ok((cell, tf)) = camera.single() else {
        return;
    };
    if globe.altitude_m(cell, tf) as f32 > GLOBE_SHOW_ALT {
        return;
    }

    let world_x = cell.x as f32 * WORLD_CELL_EDGE + tf.translation.x;
    let world_z = cell.z as f32 * WORLD_CELL_EDGE + tf.translation.z;
    let center = (
        ax + (world_x / tile_m).round() as i64,
        ay + (world_z / tile_m).round() as i64,
    );
    let shift = z.saturating_sub(WATER_ZOOM);
    let (ccx, ccy) = (
        center.0.div_euclid(1i64 << shift),
        center.1.div_euclid(1i64 << shift),
    );

    for dy in -WATER_RADIUS..=WATER_RADIUS {
        for dx in -WATER_RADIUS..=WATER_RADIUS {
            let c = (ccx + dx, ccy + dy);
            if water_covers(c.0, c.1) {
                let rank = fetch_gate::ring_distance(c, ccx, ccy);
                levels.stream.request_ranked(c, rank, now);
            }
        }
    }

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

    fn square(level: f32, half: f32) -> WaterLevelField {
        WaterLevelField {
            rings: std::sync::Arc::new(vec![(
                level,
                vec![[-half, -half], [half, -half], [half, half], [-half, half]],
                [-half, -half, half, half],
            )]),
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
        let mut f = square(-1.0, 10.0);
        let deeper = square(-3.0, 5.0);
        std::sync::Arc::get_mut(&mut f.rings)
            .expect("sole owner")
            .extend(deeper.rings.iter().cloned());
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
