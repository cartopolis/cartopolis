//! The flood defences, named on the ground they already shape.
//!
//! This is a country that mostly lies below the sea it holds out, and until now
//! the map could not say so. The *shape* was never the missing half: a dike is
//! bare earth, so it is in the national height model `systems::height` fetches
//! and `systems::terrain` renders it as a ridge like any other ground. What was
//! missing is that the ridge has a name, a type and a job.
//!
//! The water boards register every defence and PDOK republishes their registers
//! together as IMWA; atlas's `pipeline/dikes.rs` bins the `waterkering` lines into the same
//! z14 cells everything else here streams in, and this module streams them,
//! measures where their crest actually is, and hands them to
//! `systems::street_labels` to be written along.
//!
//! # Why the crest has to be measured separately
//!
//! [`TerrainField`](crate::systems::terrain::TerrainField) is one 6.4 km box at
//! 25 m per sample — the resolution that "shows dunes, dikes-as-slopes and river
//! valleys, and deliberately not the one that shows a kerb". That is the right
//! trade for the ground the whole map stands on and it is **not** enough to put a
//! label on a crest. Measured on the Lauwersmeerdijk, 2026-08-29, over the same
//! 400 m box: the height model at 0.5 m spacing gives 6.34 m of relief and at
//! 25 m spacing gives 4.24 m. So the coarse field keeps about two thirds of a sea
//! dike and flattens the top of it by roughly two metres — and a label laid on
//! that answer is buried inside the embankment it names.
//!
//! So each *named* defence near the camera gets one fine box of its own
//! ([`CREST_HALF_M`] at [`CREST_GRID`], i.e. half-metre samples), and the label
//! stands on the **highest** measurement in it. One request per visible name, not
//! per vertex and not per frame: `HeightLayer` keys and caches them, so re-asking
//! is free and a dike that has been looked at once is answered from memory.
//!
//! This is deliberately not a finer *field*. Sharpening the terrain grid along a
//! line would mean a second resolution inside the lookup every mesh build calls
//! thousands of times, to change ground the camera reads as a ridge either way.
//! The label is the one consumer that genuinely cannot use the coarse answer.
//!
//! **Where atlas has measured them, the cell file carries the crests**
//! (`crests`, beside `lines`) and nothing is asked here: the same half-metre
//! maximum, taken once on the server instead of on every device. The boxes were
//! most of what a cold start asked of the height service. A file without them —
//! an older extraction — is measured here as before.

use std::sync::Arc;

use bevy::prelude::*;
use bevy::tasks::IoTaskPool;

use crate::grid::AnchoredCamera;
use crate::platform::stream::{CellStream, Policy};
use crate::platform::{http, storage};
use crate::systems::cell_ring;
use crate::systems::coverage::CoverageSlot;
use crate::systems::globe::{GlobeState, GLOBE_SHOW_ALT};
use crate::systems::height::{wgs84_to_rd, HeightLayer, HeightModel, HeightRequest};
use crate::systems::map_stream::MapStream;
use crate::systems::osm_buildings::tdbag_url;
use cartopolis_core::world::{lat_lng_to_tile, tile_to_meters};

/// Fixed cache/streaming zoom — the one the extractor bins into.
const DIKE_ZOOM: u32 = cartopolis_geo::formats::CACHE_ZOOM;

/// Cells streamed around the camera. Wider than the building ring and it can be:
/// a whole city's defences are ~120 kB (measured over Groningen, 2026-08-29:
/// 150 defences cut into 196 pieces across 18 cells), against the ~4 MB a single
/// LoD2.2 building cell costs. A defence is also a thing you see from far away,
/// so a ring that only reached the buildings would drop the sea wall on the
/// horizon — which is the one you most want named.
const DIKE_RADIUS: i64 = 3;

/// Concurrent cell fetches, and the retry policy — our own host, so this is
/// throughput rather than courtesy.
const DIKE_CONCURRENCY: usize = 4;
const DIKE_MAX_ATTEMPTS: u32 = 3;
const DIKE_BACKOFF_SECS: f64 = 2.0;
const DIKE_BACKOFF_DOUBLINGS: u32 = 2;
const DIKE_RETRY_AFTER_SECS: f64 = 60.0;

/// Half-width of the box a crest is measured in, metres. Wide enough to contain
/// the whole cross-section of a sea dike (a big one is ~60 m from toe to toe) so
/// the highest sample in it is the crest wherever the register's reference line
/// happens to run — that line is usually *on* the crest and the register does not
/// promise it.
pub const CREST_HALF_M: f64 = 30.0;

/// Samples across that box: 120 over 60 m is half a metre, which is the height
/// model's own grid. Asking for finer would resample, not measure.
pub const CREST_GRID: u32 = 120;

/// **Crests are measured in clusters, not one line at a time.** One box per named
/// line was ~450 requests over Groningen — most of every request the client makes
/// to the height service, three in flight and 0.15 s apart, and the map could not
/// settle until they had all landed (counted on the Deck, 2026-09-24: 446 of 553
/// patches). Lines whose midpoints fall within this many metres of each other share
/// one patch at the same half metre, and each takes the highest sample in its own
/// box of it.
const CREST_CLUSTER_M: f64 = 120.0;

/// Bound on a cluster patch's grid: the cluster's span plus a crest box, at half
/// a metre.
const CREST_GRID_MAX: u32 = 400;

/// Which cells the host serves defences for.
static COVERAGE: CoverageSlot = CoverageSlot::new("dikes", "/dikes/coverage.json", DIKE_ZOOM);

/// The attribution line, once coverage has loaded and is non-empty.
pub fn dike_attribution() -> Option<String> {
    COVERAGE.attribution()
}

/// What kind of defence it is. Mirrors `DIKE_KINDS` in atlas's `pipeline/dikes.rs` —
/// **append, never renumber**.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DikeKind {
    /// An embankment.
    Dike,
    /// A dune ridge doing the same job. The coast holds out the sea with sand
    /// over most of its length, and the register says so.
    Dune,
    /// A dam.
    Dam,
    /// Ground that is simply high enough, and is therefore part of the line.
    HighGround,
    /// A lock or a pumping station standing in the line. Genuinely part of the
    /// defence and genuinely not an embankment, which is why it keeps a form of
    /// its own rather than being folded into [`DikeKind::Dike`].
    Structure,
    /// The register has a defence here and does not say what sort. About one in
    /// five (measured over four regions, 2026-08-29: 244 of 1,320) — still a
    /// defence, with a category and usually a name, so it is carried rather than
    /// dropped.
    Unknown,
}

impl DikeKind {
    /// Whether "the highest ground within thirty metres" is this defence's crest.
    ///
    /// True for everything that *is* ground — an embankment, a dune, a dam, high
    /// ground, and the untyped majority, which is mostly embankment. False for a
    /// [`DikeKind::Structure`]: the highest thing in a box around a lock or a
    /// pumping station is the lock or the pumping station, and standing a name on
    /// that measurement puts it on a roof.
    #[must_use]
    pub fn has_crest(self) -> bool {
        !matches!(self, Self::Structure)
    }
}

/// How much country stands behind it. Mirrors `DIKE_CATEGORIES` in
/// atlas's `pipeline/dikes.rs`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DikeCategory {
    /// A primary defence: the line between the country and the sea or the great
    /// rivers.
    Primary,
    /// A regional defence, holding a polder or a canal.
    Regional,
    Other,
}

impl DikeCategory {
    /// Label priority, in `street_labels`' own scale where 0 is a motorway.
    ///
    /// A primary sea defence is the largest named thing in a Dutch landscape and
    /// competes with the motorways for a label slot on purpose; a canal
    /// embankment sits with the secondary roads and loses to them at a crowded
    /// junction, which is the right way round.
    #[must_use]
    pub fn label_rank(self) -> u8 {
        match self {
            Self::Primary => 0,
            Self::Regional => 2,
            Self::Other => 4,
        }
    }
}

/// The forms, in wire order.
const DIKE_KINDS: [DikeKind; 6] = [
    DikeKind::Dike,
    DikeKind::Dune,
    DikeKind::Dam,
    DikeKind::HighGround,
    DikeKind::Structure,
    DikeKind::Unknown,
];

/// The categories, in wire order.
const DIKE_CATEGORIES: [DikeCategory; 3] = [
    DikeCategory::Primary,
    DikeCategory::Regional,
    DikeCategory::Other,
];

/// One piece of a defence, as the extractor cut it: never longer than the cell it
/// is in.
#[derive(Clone, Debug)]
pub struct DikeLine {
    pub kind: DikeKind,
    pub category: DikeCategory,
    /// The register's name for it, where it has one. Often the water it holds
    /// back (`Winschoterdiep`) or the structure it runs through rather than a
    /// dike's own name — which is what the register says, so it is what is shown.
    pub name: Option<Arc<str>>,
    /// The reference line, lat/lng, as surveyed.
    pub points: Vec<(f64, f64)>,
    /// Crest height above NAP, once measured. `None` until the fine height patch
    /// lands, and for ever where there is no height data — the label falls back
    /// to the coarse terrain field then, which is exactly the old behaviour.
    pub crest_nap_m: Option<f32>,
    /// Whether the crest measurement has been settled one way or the other, so a
    /// line that will never be answered stops being asked about.
    crest_settled: bool,
}

/// One cell's defences.
#[derive(Clone, Debug, Default)]
pub struct DikeCell {
    pub lines: Vec<DikeLine>,
}

/// Parse a cell file: `{"names":[…],"lines":[[kind,category,name_ix,[[lat,lng],…]],…]}`.
pub fn parse_dike_cell(text: &str) -> Result<DikeCell, String> {
    // The file's own type, shared with atlas's `pipeline/dikes.rs`. `crests` is
    // what atlas measured, by line; a file without them is measured here.
    let wire = cartopolis_geo::formats::dikes::parse(text)?;
    let names: Vec<Arc<str>> = wire.names.into_iter().map(Arc::from).collect();
    let crests = wire.crests.filter(|c| c.len() == wire.lines.len());
    Ok(DikeCell {
        lines: wire
            .lines
            .into_iter()
            .enumerate()
            // A line with one point is not a line. The extractor never writes
            // one; a truncated cache entry can.
            .filter(|(_, (_, _, _, pts))| pts.len() >= 2)
            .map(|(i, (k, c, n, points))| {
                // Measured on atlas: settled, whether it found a crest or not.
                let measured = crests.as_ref().map(|c| c[i].map(|m| m as f32));
                (k, c, n, points, measured)
            })
            .map(|(k, c, n, points, measured)| DikeLine {
                // An index past the end is clamped to `Unknown` rather than
                // dropping the defence: a host running a newer extractor should
                // cost this build the *type* of a defence, never the defence.
                kind: DIKE_KINDS.get(k).copied().unwrap_or(DikeKind::Unknown),
                category: DIKE_CATEGORIES
                    .get(c)
                    .copied()
                    .unwrap_or(DikeCategory::Other),
                name: usize::try_from(n).ok().and_then(|i| names.get(i).cloned()),
                points,
                crest_nap_m: measured.flatten(),
                crest_settled: measured.is_some(),
            })
            .collect(),
    })
}

/// The defences currently streamed in, by cell.
#[derive(Resource)]
pub struct Dikes {
    stream: CellStream<(i64, i64), Result<DikeCell, String>>,
    cells: std::collections::HashMap<(i64, i64), DikeCell>,
    anchor: Option<(i64, i64, u32, f64)>,
    /// Bumped whenever the set of lines or a measured crest changes, so the label
    /// placer knows to re-place and nothing else has to be told.
    pub revision: u32,
}

impl Default for Dikes {
    fn default() -> Self {
        Self {
            stream: CellStream::new(Policy {
                max_in_flight: DIKE_CONCURRENCY,
                min_interval_secs: 0.0,
                max_attempts: DIKE_MAX_ATTEMPTS,
                backoff_secs: DIKE_BACKOFF_SECS,
                backoff_doublings: DIKE_BACKOFF_DOUBLINGS,
                cooldown_secs: DIKE_RETRY_AFTER_SECS,
                // Nothing here is an entity, so the despawn queue is unused; the
                // field still has to say something and one is the honest answer
                // for "there is never a backlog".
                despawns_per_frame: 1,
            }),
            cells: std::collections::HashMap::new(),
            anchor: None,
            revision: 0,
        }
    }
}

impl Dikes {
    /// Every defence currently loaded.
    pub fn lines(&self) -> impl Iterator<Item = &DikeLine> {
        self.cells.values().flat_map(|c| c.lines.iter())
    }

    /// How many pieces are loaded — the cheap "is there anything here?" test, and
    /// what `--dump-state` reports.
    #[must_use]
    pub fn len(&self) -> usize {
        self.cells.values().map(|c| c.lines.len()).sum()
    }

    /// How many crests have been measured, so a headless run can tell "the
    /// defences are here" from "the defences are here and standing on measured
    /// ground".
    #[must_use]
    pub fn measured(&self) -> usize {
        self.lines().filter(|l| l.crest_nap_m.is_some()).count()
    }

    fn clear(&mut self) {
        self.cells.clear();
        self.revision = self.revision.wrapping_add(1);
    }
}

/// Only ever busy over covered ground below the globe tier: nothing is requested
/// for a cell the host does not serve, and a stream that was never asked is quiet.
impl crate::systems::stream_watch::StreamBusy for Dikes {
    const NAME: &'static str = "dikes";
    fn stream_busy(&self) -> bool {
        self.stream.busy()
    }
}

/// Does the host serve defences for this cell?
fn dike_covers(cx: i64, cy: i64) -> bool {
    COVERAGE.covers(cx, cy)
}

async fn fetch_dike_text(cx: i64, cy: i64) -> Result<String, String> {
    // Keyed on the extraction run (`CoverageSlot::stamp`), so a re-run — which is
    // how the cells gained their crests — is never answered from an old copy.
    let stamp = COVERAGE.stamp();
    let key = format!("dikes/{stamp}/{DIKE_ZOOM}/{cx}/{cy}.json");
    if let Some(cached) = storage::get_string(storage::Namespace::Cache, &key).await {
        return Ok(cached);
    }
    let url = format!(
        "{}/dikes/{}/{}/{}.json?v={stamp}",
        tdbag_url(),
        DIKE_ZOOM,
        cx,
        cy
    );
    let bytes = http::fetch(http::Request::get(url)).await?;
    let text = String::from_utf8(bytes).map_err(|e| e.to_string())?;
    storage::put(storage::Namespace::Cache, &key, text.as_bytes()).await;
    Ok(text)
}

/// Stream the defence cells around the camera into [`Dikes`].
///
/// The same shape as `update_building_facts` and for the same reasons: nothing
/// here is spawned, uploaded or despawned, so there is no ledger, no per-frame
/// build cap and no partial application — only the ring and the re-anchor
/// teardown.
pub fn update_dikes(
    mut dikes: ResMut<Dikes>,
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

    // A re-anchor does not change where a dike is, but the ring is expressed in
    // cells around the old anchor, so the bookkeeping starts over and the index
    // is rebuilt from the cells that come back.
    if dikes.stream.sync(stream.anchor_generation) {
        dikes.stream.reset();
        dikes.clear();
        dikes.anchor = None;
    }

    if dikes.anchor.is_none() {
        let (ax, ay) = lat_lng_to_tile(clat, clng, base_zoom);
        dikes.anchor = Some((ax, ay, base_zoom, clat));
    }
    let (ax, ay, z, lat) = dikes.anchor.expect("set just above");
    let tile_m = tile_to_meters(z, lat) as f32;

    let Some((cell, tf)) = camera.get() else {
        return;
    };
    // Freeze from orbit, like every other cell streamer: the ring would otherwise
    // chase the camera across a continent, and a name written into the ground is
    // not readable from 60 km up anyway.
    if globe.altitude_m(cell, tf) as f32 > GLOBE_SHOW_ALT {
        return;
    }

    let centre = cell_ring::coarsen(
        cell_ring::tile_at((ax, ay), tile_m, camera.stream_xz_at(cell, tf)),
        z,
        DIKE_ZOOM,
    );
    cell_ring::request_ring(&mut dikes.stream, centre, DIKE_RADIUS, now, |c, _| {
        dike_covers(c.0, c.1)
    });

    let sink = dikes.stream.sink();
    while let Some((cx, cy)) = dikes.stream.next_job(now) {
        let sink = sink.clone();
        IoTaskPool::get()
            .spawn(async move {
                let text = fetch_dike_text(cx, cy).await;
                let parsed = text.and_then(|t| parse_dike_cell(&t));
                if let Ok(mut g) = sink.lock() {
                    g.push(((cx, cy), parsed));
                }
            })
            .detach();
    }

    // No distance eviction, for the reason `building_facts` gives: walking far
    // enough to leave the ring re-anchors the map, and a re-anchor clears
    // everything above. A cell is ~7 kB (Groningen's 18 cells are 120 kB in
    // total), so the working set between anchors is not worth a second index.
    for (c, result) in dikes.stream.take_results() {
        match result {
            Ok(parsed) => {
                dikes.stream.succeeded(&c);
                dikes.cells.insert(c, parsed);
                dikes.revision = dikes.revision.wrapping_add(1);
            }
            Err(e) => {
                let attempts = dikes.stream.failed(&c, now);
                if dikes.stream.exhausted(&c) {
                    tracing::debug!(cx = c.0, cy = c.1, attempts, error = %e,
                                    "flood-defence cell unavailable");
                }
            }
        }
    }
}

/// Measure the crest of every named defence that has not been measured yet.
///
/// One fine height box per line, keyed and cached by `HeightLayer`, so re-asking
/// costs a hash lookup and a defence looked at once is answered from memory.
/// The **maximum** finite sample in the box is the crest: the mean would be
/// dragged down by the toe on both sides and the median would land on the slope.
///
/// Only *named* lines are measured, because only a named one can be labelled and
/// the label is the only consumer that needs an answer the coarse field cannot
/// give — and only lines whose crest is ground (see [`DikeKind::has_crest`]).
/// Crest midpoints that share one height patch.
struct CrestCluster {
    min: [f64; 2],
    max: [f64; 2],
    /// `(line index, midpoint in RD)`.
    members: Vec<(usize, [f64; 2])>,
}

impl CrestCluster {
    fn request(&self) -> HeightRequest {
        let min = [self.min[0] - CREST_HALF_M, self.min[1] - CREST_HALF_M];
        let max = [self.max[0] + CREST_HALF_M, self.max[1] + CREST_HALF_M];
        let span = (max[0] - min[0]).max(max[1] - min[1]);
        // The same half metre a single crest box is sampled at.
        let step = CREST_HALF_M * 2.0 / f64::from(CREST_GRID);
        let grid = ((span / step).ceil() as u32).clamp(CREST_GRID, CREST_GRID_MAX);
        HeightRequest {
            model: HeightModel::Terrain,
            min,
            max,
            grid,
        }
    }
}

/// Greedy: each midpoint joins the first cluster it keeps within
/// [`CREST_CLUSTER_M`], else starts one.
fn crest_clusters(points: &[(usize, [f64; 2])]) -> Vec<CrestCluster> {
    let mut clusters: Vec<CrestCluster> = Vec::new();
    for &(i, p) in points {
        let joined = clusters.iter_mut().find(|c| {
            let lo = [c.min[0].min(p[0]), c.min[1].min(p[1])];
            let hi = [c.max[0].max(p[0]), c.max[1].max(p[1])];
            hi[0] - lo[0] <= CREST_CLUSTER_M && hi[1] - lo[1] <= CREST_CLUSTER_M
        });
        match joined {
            Some(c) => {
                c.min = [c.min[0].min(p[0]), c.min[1].min(p[1])];
                c.max = [c.max[0].max(p[0]), c.max[1].max(p[1])];
                c.members.push((i, p));
            }
            None => clusters.push(CrestCluster {
                min: p,
                max: p,
                members: vec![(i, p)],
            }),
        }
    }
    clusters
}

pub fn measure_dike_crests(mut dikes: ResMut<Dikes>, mut height: ResMut<HeightLayer>) {
    if dikes.cells.is_empty() {
        return;
    }
    let mut changed = false;
    for cell in dikes.cells.values_mut() {
        // Only named lines, because only a named one can be labelled and the label
        // is the only consumer that needs an answer the coarse field cannot give —
        // and only the ones whose crest is ground.
        let mut waiting: Vec<(usize, [f64; 2])> = Vec::new();
        for (i, line) in cell.lines.iter_mut().enumerate() {
            if line.crest_settled || line.name.is_none() || !line.kind.has_crest() {
                continue;
            }
            let mid = line.points[line.points.len() / 2];
            if !HeightLayer::covers(mid.0, mid.1) {
                line.crest_settled = true; // outside the model; never ask again
                continue;
            }
            waiting.push((i, wgs84_to_rd(mid.0, mid.1)));
        }
        for cluster in crest_clusters(&waiting) {
            let req = cluster.request();
            let settled = height.settled(&req);
            if let Some(patch) = height.patch(req) {
                for &(i, mid) in &cluster.members {
                    let top = patch.max_in(
                        [mid[0] - CREST_HALF_M, mid[1] - CREST_HALF_M],
                        [mid[0] + CREST_HALF_M, mid[1] + CREST_HALF_M],
                    );
                    if let Some(top) = top {
                        cell.lines[i].crest_nap_m = Some(top);
                        changed = true;
                    }
                    cell.lines[i].crest_settled = true;
                }
            } else if settled {
                // Asked, answered with nothing, and will not be answered later:
                // stop asking rather than re-queueing it every frame.
                for &(i, _) in &cluster.members {
                    cell.lines[i].crest_settled = true;
                }
            }
        }
    }
    if changed {
        dikes.revision = dikes.revision.wrapping_add(1);
    }
}

/// Hand the named defences to `street_labels` as lines it can write along.
///
/// Runs only when something has actually changed — a cell landing, a crest
/// measurement, a re-anchor, or the toggle — because the store's revision is what
/// makes the label placer re-place, and bumping it every frame would re-lay every
/// street name in the city too.
///
/// The crest arrives as metres above NAP and the label frame is metres relative
/// to the **anchor's** ground, which is what `HeightLayer::ground_m` measures. So
/// the conversion is a subtraction, and it is skipped entirely until that
/// measurement exists — a label placed against a missing datum would be off by
/// the anchor's own elevation, which in this country is most of the relief there
/// is.
pub fn feed_dike_labels(
    dikes: Res<Dikes>,
    height: Res<HeightLayer>,
    stream: Res<MapStream>,
    layers: Res<crate::systems::data_layers::DataLayers>,
    mut store: ResMut<crate::systems::street_labels::StreetLabelStore>,
    mut last: Local<Option<(u32, u32, bool, Option<i32>)>>,
) {
    let on = layers.map_enabled && layers.flood_defences;
    // The anchor ground rounded to a centimetre: it is a measurement that settles
    // once and then jitters in its last bits, and a raw float here would re-place
    // every label in the city whenever it did.
    let datum = height.ground_m.map(|g| (g * 100.0).round() as i32);
    let now = (dikes.revision, stream.anchor_generation, on, datum);
    if *last == Some(now) {
        return;
    }
    *last = Some(now);

    if !on {
        store.set_dikes(Vec::new());
        return;
    }
    let Some((clat, clng, zoom)) = stream.geo_anchor() else {
        return;
    };
    let anchor = (clat, clng, zoom);
    let ground_nap = height.ground_m;
    let lines: Vec<crate::systems::street_labels::WorldLabelLine> = dikes
        .lines()
        .filter_map(|line| {
            let name = line.name.as_deref()?;
            let points: Vec<Vec2> = line
                .points
                .iter()
                .map(|&(lat, lng)| {
                    let (x, z) = crate::systems::geo_nav::lat_lng_to_world(anchor, lat, lng);
                    Vec2::new(x as f32, z as f32)
                })
                .collect();
            if points.len() < 2 {
                return None;
            }
            let ground_m = match (line.crest_nap_m, ground_nap) {
                (Some(crest), Some(base)) => Some(crest - base),
                _ => None,
            };
            Some(crate::systems::street_labels::WorldLabelLine::new(
                name.to_string(),
                line.category.label_rank(),
                points,
                ground_m,
            ))
        })
        .collect();
    store.set_dikes(lines);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn crests_measured_on_atlas_are_settled_and_a_file_without_them_is_not() {
        let line = r#"[0,0,0,[[53.2,6.5],[53.21,6.51]]]"#;
        let with = format!(r#"{{"names":["Dijk"],"lines":[{line},{line}],"crests":[3.5,null]}}"#);
        let cell = parse_dike_cell(&with).unwrap();
        assert_eq!(cell.lines[0].crest_nap_m, Some(3.5));
        assert!(cell.lines[0].crest_settled);
        assert_eq!(cell.lines[1].crest_nap_m, None);
        assert!(
            cell.lines[1].crest_settled,
            "measured and found nothing is an answer"
        );

        let without = format!(r#"{{"names":["Dijk"],"lines":[{line}]}}"#);
        let cell = parse_dike_cell(&without).unwrap();
        assert!(!cell.lines[0].crest_settled, "measured here, as before");
    }

    /// Midpoints along one quay share a patch; a line across town does not; and a
    /// cluster's patch keeps the half metre a single crest box was sampled at.
    #[test]
    fn crests_are_measured_in_clusters() {
        let points = [
            (0, [233_000.0, 582_000.0]),
            (1, [233_050.0, 582_030.0]),
            (2, [233_110.0, 582_010.0]),
            (3, [234_500.0, 581_000.0]),
        ];
        let clusters = crest_clusters(&points);
        assert_eq!(clusters.len(), 2);
        assert_eq!(clusters[0].members.len(), 3);
        for c in &clusters {
            let req = c.request();
            let step = (req.max[0] - req.min[0]) / f64::from(req.grid);
            assert!(step <= 0.5 + 1e-9, "{step}");
            assert!(req.grid <= CREST_GRID_MAX);
        }
    }

    #[test]
    fn parses_a_cell_and_maps_its_forms() {
        let cell = parse_dike_cell(
            r#"{"names":["Lauwersmeerdijk"],
                "lines":[[0,0,0,[[53.3122,6.2239],[53.3130,6.2250]]],
                         [4,1,-1,[[53.2194,6.5665],[53.2195,6.5666]]]]}"#,
        )
        .expect("valid cell");
        assert_eq!(cell.lines.len(), 2);
        assert_eq!(cell.lines[0].kind, DikeKind::Dike);
        assert_eq!(cell.lines[0].category, DikeCategory::Primary);
        assert_eq!(cell.lines[0].name.as_deref(), Some("Lauwersmeerdijk"));
        assert_eq!(cell.lines[1].kind, DikeKind::Structure);
        assert_eq!(cell.lines[1].category, DikeCategory::Regional);
        assert_eq!(cell.lines[1].name, None, "-1 means the register named none");
    }

    /// A defence is carried even when this build does not know its form. The
    /// street objects drop an unknown form because there is no shape to draw it
    /// as; here there is nothing to draw at all — only a name to write — so
    /// dropping it would lose a sea wall over a vocabulary mismatch.
    #[test]
    fn an_unknown_form_keeps_the_defence() {
        let cell = parse_dike_cell(r#"{"lines":[[99,99,-1,[[53.2,6.5],[53.3,6.6]]]]}"#)
            .expect("valid cell");
        assert_eq!(cell.lines.len(), 1);
        assert_eq!(cell.lines[0].kind, DikeKind::Unknown);
        assert_eq!(cell.lines[0].category, DikeCategory::Other);
    }

    #[test]
    fn a_degenerate_line_is_dropped() {
        let cell = parse_dike_cell(r#"{"lines":[[0,0,-1,[[53.2,6.5]]],[0,0,-1,[]]]}"#)
            .expect("valid cell");
        assert!(cell.lines.is_empty());
    }

    #[test]
    fn an_empty_cell_is_empty_rather_than_an_error() {
        assert!(parse_dike_cell("{}").expect("valid").lines.is_empty());
    }

    /// The ranks decide which name survives a crowded junction, so the ordering
    /// is the feature: a sea defence must outrank a canal embankment.
    #[test]
    fn a_primary_defence_outranks_a_regional_one() {
        assert!(DikeCategory::Primary.label_rank() < DikeCategory::Regional.label_rank());
        assert!(DikeCategory::Regional.label_rank() < DikeCategory::Other.label_rank());
    }

    /// The measuring box has to be the height model's own grid or finer, or the
    /// "measured crest" is a resample of the coarse answer it exists to replace.
    #[test]
    fn the_crest_box_samples_at_least_as_finely_as_the_model() {
        let spacing = CREST_HALF_M * 2.0 / f64::from(CREST_GRID);
        assert!(
            spacing <= 0.5,
            "crest spacing is {spacing} m, model is 0.5 m"
        );
    }

    /// A lock is part of the defence line and is not ground: the highest sample
    /// in a box around one is the building, so it must not be measured as a
    /// crest. Everything else here is ground and must be.
    #[test]
    fn only_a_structure_has_no_crest() {
        assert!(!DikeKind::Structure.has_crest());
        for k in [
            DikeKind::Dike,
            DikeKind::Dune,
            DikeKind::Dam,
            DikeKind::HighGround,
            DikeKind::Unknown,
        ] {
            assert!(k.has_crest(), "{k:?} is ground and should be measured");
        }
    }
}
