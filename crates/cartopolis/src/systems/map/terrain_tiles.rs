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

use std::collections::{HashMap, HashSet};
use std::sync::{Arc, Mutex};

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

/// Tiles in flight at once. Four rather than the WCS layer's three: these are
/// ~0.5 MB files atlas already holds, not cuts PDOK has to make.
const CONCURRENCY: usize = 4;

/// Attempts before a tile is given up on, and the anchor falls back to WCS.
const MAX_ATTEMPTS: u32 = 2;

/// Where atlas publishes which area it has pre-cut.
static COVERAGE: CoverageSlot = CoverageSlot::new("terrain", "/terrain/coverage.json", TILE_ZOOM);

/// Keep the coverage question moving from startup, so it has usually answered
/// before the first field is asked for.
pub fn prefetch_terrain_coverage(time: Res<Time>) {
    COVERAGE.ensure(time.elapsed_secs_f64());
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
            "/terrain/{z}/{x}/{y}?grid=513".to_string()
        } else {
            "https://cartopolis.org/terrain/{z}/{x}/{y}?grid=513".to_string()
        }
    });
    (!t.is_empty()).then_some(t)
}

fn cache_key(x: i64, y: i64) -> String {
    // `v2`: atlas's unfilled tiles. See `crates/atlas/src/terrain.rs::store_dir`.
    format!("terrain/v2/{TILE_ZOOM}/{x}/{y}.g513.ctt")
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

#[derive(Resource)]
pub struct TerrainTiles {
    template: Option<String>,
    dispatch: Dispatcher<(i64, i64)>,
    results: Arc<Mutex<Vec<((i64, i64), Answer)>>>,
    /// Answered tiles, `None` for "nothing measured". Held only until a mosaic is
    /// handed to a bake ([`Self::release`]); the device cache holds them after.
    answered: HashMap<(i64, i64), Option<Arc<TerrainTile>>>,
    requested: HashSet<(i64, i64)>,
    failed: HashMap<(i64, i64), u32>,
    /// Failed tiles waiting out [`RETRY_S`] before their next attempt.
    retry: Vec<((i64, i64), f64)>,
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

    /// Ask for these tiles; queue whatever has not been asked for. Nearest first,
    /// as `want` is ordered, so the ground under the anchor arrives first.
    pub fn mosaic(&mut self, want: &[(i64, i64)]) -> Mosaic {
        let mut ready = true;
        for (rank, &t) in want.iter().enumerate() {
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
                .filter_map(|t| {
                    self.answered
                        .get(t)
                        .and_then(Clone::clone)
                        .map(|tile| (*t, tile))
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

async fn fetch_tile(url: String, key: String) -> Answer {
    if let Some(bytes) = storage::get(storage::Namespace::Cache, &key).await {
        // An empty entry is atlas having answered "nothing measured here".
        if bytes.is_empty() {
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
    let due: Vec<(i64, i64)> = tiles
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
    while let Some((x, y)) = tiles.dispatch.next(now) {
        let results = results.clone();
        let url = template
            .replace("{z}", &TILE_ZOOM.to_string())
            .replace("{x}", &x.to_string())
            .replace("{y}", &y.to_string());
        let key = cache_key(x, y);
        IoTaskPool::get()
            .spawn(async move {
                let out = fetch_tile(url, key).await;
                if let Ok(mut g) = results.lock() {
                    g.push(((x, y), out));
                }
            })
            .detach();
    }
    let done: Vec<((i64, i64), Answer)> = tiles
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
    fn asking_twice_queues_once_and_a_failure_is_final() {
        let mut t = TerrainTiles::default();
        let want = [(1, 1), (1, 2)];
        assert!(matches!(t.mosaic(&want), Mosaic::Waiting));
        assert!(matches!(t.mosaic(&want), Mosaic::Waiting));
        assert_eq!(t.dispatch.queued(), 2);
        t.answered.insert((1, 1), None);
        t.failed.insert((1, 2), MAX_ATTEMPTS);
        assert!(matches!(t.mosaic(&want), Mosaic::Failed));
        t.failed.clear();
        t.answered.insert((1, 2), None);
        assert!(matches!(t.mosaic(&want), Mosaic::Ready(m) if m.len() == 0));
    }
}
