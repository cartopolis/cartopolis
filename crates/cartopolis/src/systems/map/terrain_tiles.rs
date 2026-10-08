//! The terrain field's heights, assembled from atlas's `/terrain/{z}/{x}/{y}`
//! tiles instead of one WCS request per map anchor
//! (`docs/notes/level-of-detail-tile-tree.md`, step 2b).
//!
//! Nothing downstream changes: [`TileMosaic`] answers the same "height at this
//! point" question [`HeightPatch`](super::height::HeightPatch) does, and
//! `terrain::bake` carves, fills and grades whichever it is handed. What changes
//! is the fetch, and three things make it worth changing:
//!
//! - **A tile's key does not depend on the anchor.** The WCS box is centred on the
//!   map anchor, so a re-anchor or a second visit asks for a box nobody has asked
//!   for before; a tile is the same tile from anywhere.
//! - **Tiles are kept on the device** (`platform::storage`'s Cache namespace). The
//!   WCS patch was dropped after every bake, to save its 17 MB, which meant every
//!   *re*-bake — the roads settling, the decks settling — downloaded the same
//!   12 MB again, ~4.5 s a time.
//! - **Atlas cuts a covered city ahead of time** (its `terrain` dataset), so a
//!   tile there is a file read on the server, not a PDOK round trip.
//!
//! **Big tiles, few requests.** The field is assembled from z14 tiles at
//! `DENSE_GRID` (513²), ~25–36 requests a field. It was first built from z16 tiles
//! at the ordinary 129², the same samples, and measured on the container at ~3
//! tiles a second: ~360 small requests each queue for a native pool thread behind
//! the rest of the client's streaming, and the field arrived after the capture.
//!
//! **Only where atlas says it has cut the area.** A tile nobody has asked for is
//! cut from PDOK on the request, three at a time, and a field's worth would then
//! take longer than the one WCS request it replaces. So the terrain coverage
//! manifest decides, per map anchor, and anywhere it does not cover — or before it
//! has answered — the field is fetched as it always was.
//!
//! **A handset reads its tiles at 129²** ([`tile_grid`]), which atlas thins from
//! the dense ones: its field is too coarse to use 513². Graded and raw alike — a
//! raw 129² tile is thinned from a stored dense one where atlas has cut the area
//! and cut from AHN on request elsewhere — so a handset off graded ground takes
//! raw tiles and grades them itself, as a desktop does, rather than one WCS patch
//! that no second visit can reuse.

use crate::platform::sync::{Arc, Mutex};
use std::collections::{HashMap, HashSet};

use bevy::prelude::*;
use bevy::tasks::IoTaskPool;
use cartopolis_core::world::{lat_lng_to_tile, lat_lng_to_tile_frac};
use cartopolis_geo::terrain_tile::TerrainTile;

use crate::platform::{http, storage, work::Dispatcher};
use crate::systems::coverage::CoverageSlot;

/// The zoom the field is assembled from. A z14 tile is ~1.47 km across at 53°N
/// and its 513 samples are ~2.9 m apart: the field's own 3.125 m, and atlas's
/// `pipeline::terrain::ZOOM`.
pub const TILE_ZOOM: u32 = 14;

/// Tiles in flight at once. More than the WCS layer's three: these are ~0.5 MB
/// files atlas already holds, not cuts PDOK has to make. Eight rather than the
/// four it was, because the field cannot bake until all ~30 have answered and
/// over graded ground every ground and building cell waits on that bake.
const CONCURRENCY: usize = 8;

/// Attempts before a tile is given up on, and the anchor falls back to WCS.
const MAX_ATTEMPTS: u32 = 2;

/// Where atlas publishes which area it has pre-cut.
static COVERAGE: CoverageSlot = CoverageSlot::new("terrain", "/terrain/coverage.json", TILE_ZOOM);

/// Where atlas publishes which cells it measured the bridge decks of.
static DECKS: CoverageSlot = CoverageSlot::new("decks", "/decks/coverage.json", TILE_ZOOM);

/// Where atlas publishes which cells it solved the road heights of
/// (`cartopolis_geo::road_levels`).
static ROADS: CoverageSlot = CoverageSlot::new("roads", "/roads/coverage.json", TILE_ZOOM);

/// Keep the coverage question moving from startup, so it has usually answered
/// before the first field is asked for.
pub fn prefetch_terrain_coverage(time: Res<Time>) {
    COVERAGE.ensure(time.elapsed_secs_f64());
    DECKS.ensure(time.elapsed_secs_f64());
    ROADS.ensure(time.elapsed_secs_f64());
}

/// The road heights atlas solved for a cell's streets while grading it, keyed by
/// run (`road_levels::StreetRun::key`), or `None` where atlas has none.
///
/// **The road decides the ground now, not the other way round.** Every street
/// stands at its own height, solved on the road network over the cell and its
/// neighbours, so a deck and the road off its end meet at one number instead of
/// at two interpolations of the terrain field (`docs/notes/bridge-heights.md`).
/// A binary file, per grading run like the graded tiles, cached through the
/// storage seam.
pub(crate) async fn load_road_heights(
    cx: i64,
    cy: i64,
) -> Option<cartopolis_geo::road_levels::RoadHeights> {
    if !ROADS.covers(cx, cy) {
        return None;
    }
    let stamp = COVERAGE.graded_stamp();
    let key = format!("roads/{stamp}/{TILE_ZOOM}/{cx}/{cy}.bin");
    let bytes = match storage::get(storage::Namespace::Cache, &key).await {
        Some(b) => b,
        None => {
            let url = format!(
                "{}/roads/{TILE_ZOOM}/{cx}/{cy}.bin?v={stamp}",
                crate::systems::osm_buildings::tdbag_url()
            );
            let b = http::fetch_optional(http::Request::get(url)).await.ok()??;
            cartopolis_geo::road_levels::RoadHeights::decode(&b).ok()?;
            storage::put(storage::Namespace::Cache, &key, &b).await;
            b
        }
    };
    cartopolis_geo::road_levels::RoadHeights::decode(&bytes).ok()
}

/// One deck as atlas published it (`crates/atlas/src/pipeline/terrain_grade.rs::
/// write_decks`): `[x, y, height, top?, profile?]` — its bounding-box centre in
/// fractions of the tile, its clearance in metres (0 for a deck laid flat), and,
/// trailing so a file from before them still reads, its median top in NAP and its
/// profile (`DeckProfile::to_wire`).
#[derive(Clone, Debug, PartialEq)]
pub struct AtlasDeck {
    pub at: [f32; 2],
    pub height: f32,
    pub top: Option<f32>,
    pub profile: Option<Vec<f64>>,
}

/// The bridge decks atlas measured in a cell while grading, or `None` where atlas
/// did not measure the cell.
///
/// **What it replaces is the cold start's long pole.** The client measured every
/// deck itself against PDOK, a surface and a ground patch per cluster, three at a
/// time and spaced — and the ground cells are not done until their decks are.
/// Measured on the Deck (2026-09-24), cold, with the ground graded on atlas: the
/// cells were built by ~12 s and the last deck placed at ~21 s.
pub(crate) async fn load_atlas_decks(cx: i64, cy: i64) -> Option<Vec<AtlasDeck>> {
    ATLAS_DECKS.load(cx, cy).await
}

const ATLAS_DECKS: crate::platform::cell_source::CellSource<Option<Vec<AtlasDeck>>> =
    crate::platform::cell_source::CellSource {
        label: "atlas decks",
        gate: Some(|cx, cy| DECKS.covers(cx, cy)),
        // Per grading run, like the graded tiles: a re-grade is a new file.
        key: |cx, cy| {
            format!(
                "decks/{}/{TILE_ZOOM}/{cx}/{cy}.json",
                COVERAGE.graded_stamp()
            )
        },
        url: Some(|cx, cy| {
            format!(
                "{}/decks/{TILE_ZOOM}/{cx}/{cy}.json?v={}",
                crate::systems::osm_buildings::tdbag_url(),
                COVERAGE.graded_stamp()
            )
        }),
        breaker: None,
        timeout: None,
        parse: |text| {
            let cell = cartopolis_geo::formats::decks::parse(text).ok()?;
            let decks = cell
                .decks
                .into_iter()
                .map(|d| AtlasDeck {
                    at: [d.at[0] as f32, d.at[1] as f32],
                    height: d.height as f32,
                    top: d.top.map(|v| v as f32),
                    profile: d.profile,
                })
                .collect();
            Some(Some(decks))
        },
    };

/// Every deck of a cell's heights out of atlas's list, by the same index as
/// `decks`, or `None` if any deck is not in it — the map changed since atlas
/// measured it, and the cell measures its own as before. Matched on the
/// bounding-box centre: both sides tessellate the same tile with the same code.
#[must_use]
pub fn match_atlas_decks(
    decks: &[cartopolis_geo::vector_tiles::BridgeDeck],
    tile_m: f32,
    atlas: &[AtlasDeck],
) -> Option<Vec<cartopolis_geo::deck_profile::DeckMeasure>> {
    // A metre, in tile fractions.
    let tol = 1.0 / tile_m;
    decks
        .iter()
        .map(|d| {
            let c = [
                (d.min[0] + d.max[0]) * 0.5 / tile_m,
                (d.min[1] + d.max[1]) * 0.5 / tile_m,
            ];
            atlas
                .iter()
                .find(|a| (a.at[0] - c[0]).abs() < tol && (a.at[1] - c[1]).abs() < tol)
                .map(|a| cartopolis_geo::deck_profile::DeckMeasure {
                    height: a.height,
                    top: a.top.filter(|t| t.is_finite() && a.height > 0.0),
                    // In tile fractions on the wire; this cell's metres here.
                    profile: a.profile.as_deref().and_then(|w| {
                        cartopolis_geo::deck_profile::DeckProfile::from_wire(w, f64::from(tile_m))
                    }),
                })
        })
        .collect()
}

/// Whether the coverage manifest has answered, yes or no.
#[must_use]
pub fn coverage_answered() -> bool {
    COVERAGE.revision() > 0
}

/// Whether atlas has pre-cut the ground under this point.
#[must_use]
pub fn covered(lat: f64, lng: f64) -> bool {
    let (x, y) = lat_lng_to_tile(lat, lng, TILE_ZOOM);
    COVERAGE.covers(x, y)
}

/// The tile URL template, `{z}/{x}/{y}`. `CARTO_TERRAIN_URL` overrides it and an
/// empty value switches tiles off. The browser build asks its own origin, which is
/// atlas — same origin keeps it inside the page's cross-origin isolation.
fn template() -> Option<String> {
    let t = std::env::var("CARTO_TERRAIN_URL").unwrap_or_else(|_| {
        if cfg!(target_arch = "wasm32") {
            "/terrain/{z}/{x}/{y}?grid=513&ahn=6".to_string()
        } else {
            "https://cartopolis.org/terrain/{z}/{x}/{y}?grid=513&ahn=6".to_string()
        }
    });
    (!t.is_empty()).then_some(t)
}

/// The grid a tile is fetched at: [`DENSE_GRID`](cartopolis_geo::terrain_tile::DENSE_GRID)
/// for a desktop field, the tile tree's own 129² for a handset's. A handset's field
/// is 513² over 6.4 km, ~12.5 m apart, and the dense tiles under it would be ~16 MB
/// to read 1 sample in 20; at 129² a z14 tile's samples are ~11.5 m apart and the
/// whole field is ~1 MB. Atlas thins the 129² tile from the dense one, graded or
/// raw.
#[must_use]
pub fn tile_grid() -> usize {
    use cartopolis_geo::terrain_tile::{DENSE_GRID, GRID};
    if crate::quality::budget().terrain_grid >= 1025 {
        DENSE_GRID
    } else {
        GRID
    }
}

fn cache_key(x: i64, y: i64, stamp: u64, grid: usize) -> String {
    if stamp > 0 {
        // Per grading run: a re-grade is new ground, and an old copy is never it.
        return format!("terrain/graded/{stamp}/{TILE_ZOOM}/{x}/{y}.g{grid}.ctt");
    }
    // `v3`: atlas's unfilled tiles, cut from AHN6 where it is published. See
    // `crates/atlas/src/terrain.rs::store_dir`.
    format!("terrain/v3/{TILE_ZOOM}/{x}/{y}.g{grid}.ctt")
}

/// The URL for one tile at `grid`; a graded one carries `graded=1` and the run's
/// stamp, so no HTTP cache between here and atlas can answer it with an older
/// grading. A template that names a grid has it replaced.
fn tile_url(template: &str, x: i64, y: i64, stamp: u64, grid: usize) -> String {
    let mut url = template
        .replace("{z}", &TILE_ZOOM.to_string())
        .replace("{x}", &x.to_string())
        .replace("{y}", &y.to_string());
    let named = url.find("grid=").map(|i| {
        let end = url[i + 5..]
            .find(|c: char| !c.is_ascii_digit())
            .map_or(url.len(), |e| i + 5 + e);
        (i, end)
    });
    match named {
        Some((i, end)) => url.replace_range(i..end, &format!("grid={grid}")),
        None => {
            let sep = if url.contains('?') { '&' } else { '?' };
            url = format!("{url}{sep}grid={grid}");
        }
    }
    if stamp == 0 {
        return url;
    }
    format!("{url}&graded=1&v={stamp}")
}

/// The grading run every tile of `want` was graded by, if they all were and by
/// the same one. The field is graded all or nothing: half a field graded on the
/// server and half on the device would meet at a seam.
///
/// **Every tile, not every covered one.** This used to skip the tiles outside the
/// manifest, on the reading that nothing was measured there — but atlas cuts a
/// raw tile anywhere in AHN on request, and never grades one it has not been
/// asked to. So a field reaching past the graded area (anywhere near the edge of
/// Groningen's 7×7, which a following field does routinely) chose graded ground,
/// asked for graded tiles atlas answers with 404, waited out their retries, and
/// only then fell back — on a handset to one WCS patch for the whole box
/// (measured on the Deck, 2026-09-25: four seconds of 404s at the Weg der
/// Verenigde Naties before the fallback began).
#[must_use]
pub fn graded_stamp(want: &[(i64, i64)]) -> Option<u64> {
    let mut stamp = None;
    for &(x, y) in want {
        let s = COVERAGE.graded(x, y)?;
        if *stamp.get_or_insert(s) != s {
            return None;
        }
    }
    stamp
}

/// The z14 tiles under a square of ground, nearest its centre first.
#[must_use]
pub fn tiles_under(south: f64, west: f64, north: f64, east: f64) -> Vec<(i64, i64)> {
    let (x0, y0) = lat_lng_to_tile(north, west, TILE_ZOOM);
    let (x1, y1) = lat_lng_to_tile(south, east, TILE_ZOOM);
    let (cx, cy) = ((x0 + x1) as f64 / 2.0, (y0 + y1) as f64 / 2.0);
    let mut tiles: Vec<(i64, i64)> = (y0..=y1)
        .flat_map(|y| (x0..=x1).map(move |x| (x, y)))
        .collect();
    tiles.sort_by(|a, b| {
        let d = |t: &(i64, i64)| (t.0 as f64 - cx).powi(2) + (t.1 as f64 - cy).powi(2);
        d(a).total_cmp(&d(b)).then(a.cmp(b))
    });
    tiles
}

/// A set of tiles, answering "the height here" as a WCS patch does.
#[derive(Clone, Default)]
pub struct TileMosaic {
    /// A tile missing from the map is one atlas answered "nothing measured" for.
    tiles: HashMap<(i64, i64), Arc<TerrainTile>>,
}

impl TileMosaic {
    /// Height at a point, or `None` where nothing was measured or no tile covers
    /// it. The nearest sample, not interpolated, for the reason
    /// `HeightPatch::at` gives: interpolating across a deck's edge invents a slope.
    #[must_use]
    pub fn height(&self, lat: f64, lng: f64) -> Option<f32> {
        let (fx, fy) = lat_lng_to_tile_frac(lat, lng, TILE_ZOOM);
        let (x, y) = (fx.floor(), fy.floor());
        let tile = self.tiles.get(&(x as i64, y as i64))?;
        let last = (tile.grid - 1) as f64;
        let col = ((fx - x) * last).round() as usize;
        let row = ((fy - y) * last).round() as usize;
        let v = tile.at(col.min(tile.grid - 1), row.min(tile.grid - 1));
        v.is_finite().then_some(v)
    }

    /// [`Self::height`] over one row of a lattice: `out[i]` for `(lat, lngs[i])`,
    /// NaN where nothing was measured.
    ///
    /// **Web Mercator is separable**, so a tile column depends on the longitude
    /// alone and a tile row on the latitude alone: `columns` is the per-longitude
    /// half, worked out once for the whole lattice by [`Self::columns`], and this
    /// does the latitude's half once per row. Per sample that leaves a rounding and
    /// a lookup — the field is 4.2 million samples, and the `tan`, `cos` and `ln`
    /// in `lat_lng_to_tile_frac` had been paid for every one of them. The answers
    /// are the same arithmetic on the same numbers as [`Self::height`], so they
    /// match it bit for bit (`a_mosaic_row_matches_its_samples`).
    pub fn height_row(&self, lat: f64, columns: &[(i64, f64)], out: &mut [f32]) {
        let (_, fy) = lat_lng_to_tile_frac(lat, 0.0, TILE_ZOOM);
        let y = fy.floor();
        let mut current: Option<(i64, Option<&Arc<TerrainTile>>)> = None;
        for (slot, &(x, frac)) in out.iter_mut().zip(columns) {
            let tile = match current {
                Some((cx, t)) if cx == x => t,
                _ => {
                    let t = self.tiles.get(&(x, y as i64));
                    current = Some((x, t));
                    t
                }
            };
            *slot = tile.map_or(f32::NAN, |tile| {
                let last = (tile.grid - 1) as f64;
                let col = (frac * last).round() as usize;
                let row = ((fy - y) * last).round() as usize;
                let v = tile.at(col.min(tile.grid - 1), row.min(tile.grid - 1));
                if v.is_finite() {
                    v
                } else {
                    f32::NAN
                }
            });
        }
    }

    /// The longitude half of [`Self::height_row`]: each longitude's tile column and
    /// its fraction across that tile.
    #[must_use]
    pub fn columns(lngs: &[f64]) -> Vec<(i64, f64)> {
        lngs.iter()
            .map(|&lng| {
                let (fx, _) = lat_lng_to_tile_frac(0.0, lng, TILE_ZOOM);
                let x = fx.floor();
                (x as i64, fx - x)
            })
            .collect()
    }

    #[must_use]
    pub fn len(&self) -> usize {
        self.tiles.len()
    }
}

/// What asking for a set of tiles comes to this frame.
pub enum Mosaic {
    /// Every tile has answered.
    Ready(TileMosaic),
    /// Still arriving.
    Waiting,
    /// A tile could not be fetched: fall back to WCS for this anchor.
    Failed,
}

type Answer = Result<Option<Arc<TerrainTile>>, String>;

/// A tile as fetched: `(x, y, stamp)`, where the stamp is the atlas grading run
/// for a graded tile and 0 for the raw one. Two different things at one place, so
/// never one key.
type Key = (i64, i64, u64);

#[derive(Resource)]
pub struct TerrainTiles {
    template: Option<String>,
    dispatch: Dispatcher<Key>,
    results: Arc<Mutex<Vec<(Key, Answer)>>>,
    /// Answered tiles, `None` for "nothing measured". Held only until a mosaic is
    /// handed to a bake ([`Self::release`]); the device cache holds them after.
    answered: HashMap<Key, Option<Arc<TerrainTile>>>,
    requested: HashSet<Key>,
    failed: HashMap<Key, u32>,
    /// Failed tiles waiting out [`RETRY_S`] before their next attempt.
    retry: Vec<(Key, f64)>,
    /// Tiles fetched over the network, and read back from the device.
    pub fetched: usize,
}

/// How long a failed tile waits before it is asked for again. Without a wait both
/// attempts land inside the same tenth of a second, and a host that is restarting
/// sends the whole anchor back to WCS — seen on the first local run of this.
const RETRY_S: f64 = 2.0;

impl Default for TerrainTiles {
    fn default() -> Self {
        Self {
            template: template(),
            dispatch: Dispatcher::new(CONCURRENCY, 0.0),
            results: Arc::default(),
            answered: HashMap::new(),
            requested: HashSet::new(),
            failed: HashMap::new(),
            retry: Vec::new(),
            fetched: 0,
        }
    }
}

impl TerrainTiles {
    /// Whether tiles can be used at all (a template is configured).
    #[must_use]
    pub fn enabled(&self) -> bool {
        self.template.is_some()
    }

    /// Ask for these tiles — graded by the atlas run `stamp`, or raw for 0; queue
    /// whatever has not been asked for. Nearest first, as `want` is ordered, so the
    /// ground under the anchor arrives first.
    pub fn mosaic(&mut self, want: &[(i64, i64)], stamp: u64) -> Mosaic {
        let mut ready = true;
        for (rank, &(x, y)) in want.iter().enumerate() {
            let t = (x, y, stamp);
            if self.answered.contains_key(&t) {
                continue;
            }
            ready = false;
            if *self.failed.get(&t).unwrap_or(&0) >= MAX_ATTEMPTS {
                return Mosaic::Failed;
            }
            if self.requested.insert(t) {
                self.dispatch.queue_ranked(t, rank as u32);
            }
        }
        if !ready {
            return Mosaic::Waiting;
        }
        Mosaic::Ready(TileMosaic {
            tiles: want
                .iter()
                .filter_map(|&(x, y)| {
                    self.answered
                        .get(&(x, y, stamp))
                        .and_then(Clone::clone)
                        .map(|tile| ((x, y), tile))
                })
                .collect(),
        })
    }

    /// Forget the answered tiles once a bake holds its own mosaic. A re-bake
    /// reads them back from the device cache.
    pub fn release(&mut self) {
        self.answered.clear();
        self.failed.clear();
        for (t, _) in self.retry.drain(..) {
            self.requested.remove(&t);
        }
    }
}

/// One tile, from the device cache or atlas.
///
/// **A graded tile is never "nothing measured".** The manifest lists a cell as
/// graded only where there was ground to grade, so a 404 for one means this atlas
/// cannot serve it — an atlas older than the client, asked for a grid it does not
/// thin to (seen 2026-09-24: every 129² tile of a handset's field). Read as
/// "nothing here", that drew the whole field flat and cached the answer for the
/// life of the grading run. As an error it is not cached, and the field falls back
/// to the ground it can get (`terrain::update_terrain_field`).
async fn fetch_tile(url: String, key: String, graded: bool) -> Answer {
    if let Some(bytes) = storage::get(storage::Namespace::Cache, &key).await {
        // An empty entry is atlas having answered "nothing measured here".
        if bytes.is_empty() && !graded {
            return Ok(None);
        }
        if let Ok(tile) = TerrainTile::decode(&bytes) {
            return Ok(Some(Arc::new(tile)));
        }
    }
    match http::fetch_optional(http::Request::get(url)).await? {
        Some(bytes) => {
            let tile = TerrainTile::decode(&bytes)?;
            storage::put(storage::Namespace::Cache, &key, &bytes).await;
            Ok(Some(Arc::new(tile)))
        }
        None if graded => Err("atlas does not serve this graded tile".to_string()),
        None => {
            storage::put(storage::Namespace::Cache, &key, &[]).await;
            Ok(None)
        }
    }
}

pub fn poll_terrain_tiles(mut tiles: ResMut<TerrainTiles>, time: Res<Time>) {
    let Some(template) = tiles.template.clone() else {
        return;
    };
    let now = time.elapsed_secs_f64();
    let due: Vec<Key> = tiles
        .retry
        .iter()
        .filter(|(_, at)| *at <= now)
        .map(|(t, _)| *t)
        .collect();
    tiles.retry.retain(|(_, at)| *at > now);
    for t in due {
        tiles.dispatch.queue(t);
    }
    let results = tiles.results.clone();
    let grid = tile_grid();
    while let Some((x, y, stamp)) = tiles.dispatch.next(now) {
        let results = results.clone();
        let url = tile_url(&template, x, y, stamp, grid);
        let key = cache_key(x, y, stamp, grid);
        IoTaskPool::get()
            .spawn(async move {
                let out = fetch_tile(url, key, stamp > 0).await;
                if let Ok(mut g) = results.lock() {
                    g.push(((x, y, stamp), out));
                }
            })
            .detach();
    }
    let done: Vec<(Key, Answer)> = tiles
        .results
        .lock()
        .map(|mut g| g.drain(..).collect())
        .unwrap_or_default();
    for (t, out) in done {
        tiles.dispatch.completed();
        match out {
            Ok(tile) => {
                tiles.requested.remove(&t);
                tiles.fetched += 1;
                tiles.answered.insert(t, tile);
                tracing::debug!(
                    tile = ?t,
                    answered = tiles.answered.len(),
                    in_flight = tiles.requested.len(),
                    "terrain tile"
                );
            }
            Err(e) => {
                let n = tiles.failed.entry(t).or_insert(0);
                *n += 1;
                let n = *n;
                tracing::warn!(tile = ?t, attempt = n, "terrain tile failed: {e}");
                // Still `requested` while it waits, so `mosaic` does not queue it
                // again early; the last failure releases it and `mosaic` reports it.
                if n < MAX_ATTEMPTS {
                    tiles.retry.push((t, now + RETRY_S));
                } else {
                    tiles.requested.remove(&t);
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The grid a template names is the one asked for, and a template that names
    /// none gains one — including an override like the Deck harness's proxy.
    #[test]
    fn a_tile_is_asked_for_at_the_grid_the_field_needs() {
        let t = "https://a.org/terrain/{z}/{x}/{y}?grid=513&ahn=6";
        assert_eq!(
            tile_url(t, 1, 2, 0, 129),
            "https://a.org/terrain/14/1/2?grid=129&ahn=6"
        );
        assert_eq!(
            tile_url(t, 1, 2, 7, 513),
            "https://a.org/terrain/14/1/2?grid=513&ahn=6&graded=1&v=7"
        );
        assert_eq!(
            tile_url("http://h/terrain/{z}/{x}/{y}", 1, 2, 7, 129),
            "http://h/terrain/14/1/2?grid=129&graded=1&v=7"
        );
        assert_ne!(cache_key(1, 2, 7, 129), cache_key(1, 2, 7, 513));
    }

    #[test]
    fn a_point_reads_the_sample_of_the_tile_it_falls_in() {
        let (lat, lng) = (53.2194, 6.5665);
        let (x, y) = lat_lng_to_tile(lat, lng, TILE_ZOOM);
        let grid = cartopolis_geo::terrain_tile::GRID;
        // Each sample's height is its column, so the answer says which one was read.
        let heights = (0..grid * grid).map(|i| (i % grid) as f32).collect();
        let mosaic = TileMosaic {
            tiles: HashMap::from([((x, y), Arc::new(TerrainTile { grid, heights }))]),
        };
        let (fx, _) = lat_lng_to_tile_frac(lat, lng, TILE_ZOOM);
        let want = ((fx - x as f64) * (grid - 1) as f64).round() as f32;
        assert_eq!(mosaic.height(lat, lng), Some(want));
        // A tile atlas answered "nothing" for is no data, not zero.
        assert_eq!(mosaic.height(lat + 0.02, lng), None);
    }

    /// The row path is the same arithmetic as the per-point one, so a lattice
    /// straddling several tiles, a hole and a missing tile reads identically.
    #[test]
    fn a_mosaic_row_matches_its_samples() {
        let grid = cartopolis_geo::terrain_tile::GRID;
        let (x, y) = lat_lng_to_tile(53.2194, 6.5665, TILE_ZOOM);
        let tile = |seed: usize| {
            let mut heights: Vec<f32> = (0..grid * grid)
                .map(|i| ((i * 7919 + seed) % 1000) as f32 * 0.01)
                .collect();
            heights[grid * 3 + 5] = f32::NAN;
            Arc::new(TerrainTile { grid, heights })
        };
        // Three of the four tiles; (x + 1, y + 1) is "nothing measured".
        let mosaic = TileMosaic {
            tiles: HashMap::from([
                ((x, y), tile(1)),
                ((x + 1, y), tile(2)),
                ((x, y + 1), tile(3)),
            ]),
        };
        let (north, west) = cartopolis_core::world::tile_to_lat_lng(x, y, TILE_ZOOM);
        let (south, east) = cartopolis_core::world::tile_to_lat_lng(x + 2, y + 2, TILE_ZOOM);
        let n = 301;
        let lngs: Vec<f64> = (0..n)
            .map(|i| west + (east - west) * i as f64 / (n - 1) as f64)
            .collect();
        let lats: Vec<f64> = (0..n)
            .map(|i| north + (south - north) * i as f64 / (n - 1) as f64)
            .collect();
        let columns = TileMosaic::columns(&lngs);
        let mut row = vec![0.0f32; n];
        let mut holes = 0;
        for &lat in &lats {
            mosaic.height_row(lat, &columns, &mut row);
            for (&lng, &got) in lngs.iter().zip(&row) {
                match mosaic.height(lat, lng) {
                    Some(want) => assert_eq!(got.to_bits(), want.to_bits(), "{lat} {lng}"),
                    None => {
                        assert!(got.is_nan(), "{lat} {lng}: {got}");
                        holes += 1;
                    }
                }
            }
        }
        // The missing tile is a quarter of the lattice.
        assert!(holes >= n * n / 5, "{holes}");
    }

    #[test]
    fn a_hole_is_no_data() {
        let (lat, lng) = (53.2194, 6.5665);
        let (x, y) = lat_lng_to_tile(lat, lng, TILE_ZOOM);
        let grid = cartopolis_geo::terrain_tile::GRID;
        let mosaic = TileMosaic {
            tiles: HashMap::from([(
                (x, y),
                Arc::new(TerrainTile {
                    grid,
                    heights: vec![f32::NAN; grid * grid],
                }),
            )]),
        };
        assert_eq!(mosaic.height(lat, lng), None);
    }

    #[test]
    fn the_tiles_under_a_field_start_in_the_middle_and_cover_it() {
        // Roughly the 6.4 km field box around Groningen's Grote Markt.
        let tiles = tiles_under(53.19, 6.52, 53.25, 6.62);
        assert!((16..=49).contains(&tiles.len()), "{}", tiles.len());
        let centre = lat_lng_to_tile(53.22, 6.57, TILE_ZOOM);
        let first = tiles[0];
        assert!((first.0 - centre.0).abs() <= 1 && (first.1 - centre.1).abs() <= 1);
    }

    #[test]
    fn atlas_deck_heights_are_matched_by_centre_or_not_at_all() {
        let deck = |x: f32, z: f32| cartopolis_geo::vector_tiles::BridgeDeck {
            positions: Vec::new(),
            indices: Vec::new(),
            rings: Vec::new(),
            min: [x - 10.0, z - 5.0],
            max: [x + 10.0, z + 5.0],
        };
        let tile_m = 1470.0;
        let decks = [deck(100.0, 200.0), deck(700.0, 50.0)];
        let at = |x: f32, z: f32, height: f32, top: Option<f32>| AtlasDeck {
            at: [x / tile_m, z / tile_m],
            height,
            top,
            profile: None,
        };
        let atlas = [at(700.0, 50.0, 0.0, None), at(100.2, 199.9, 6.5, Some(8.2))];
        let known = match_atlas_decks(&decks, tile_m, &atlas).unwrap();
        assert_eq!(
            known.iter().map(|m| (m.height, m.top)).collect::<Vec<_>>(),
            vec![(6.5, Some(8.2)), (0.0, None)]
        );
        // One deck atlas never saw, and the cell measures its own.
        let more = [deck(100.0, 200.0), deck(700.0, 50.0), deck(1200.0, 900.0)];
        assert_eq!(match_atlas_decks(&more, tile_m, &atlas), None);
    }

    #[test]
    fn asking_twice_queues_once_and_a_failure_is_final() {
        let mut t = TerrainTiles::default();
        let want = [(1, 1), (1, 2)];
        assert!(matches!(t.mosaic(&want, 0), Mosaic::Waiting));
        assert!(matches!(t.mosaic(&want, 0), Mosaic::Waiting));
        assert_eq!(t.dispatch.queued(), 2);
        t.answered.insert((1, 1, 0), None);
        t.failed.insert((1, 2, 0), MAX_ATTEMPTS);
        assert!(matches!(t.mosaic(&want, 0), Mosaic::Failed));
        t.failed.clear();
        t.answered.insert((1, 2, 0), None);
        assert!(matches!(t.mosaic(&want, 0), Mosaic::Ready(m) if m.len() == 0));
    }
}
