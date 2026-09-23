//! `/terrain/{z}/{x}/{y}`: measured Dutch ground cut to map tiles
//! (`cartopolis_geo::terrain_tile` is the format).
//!
//! **Cut on first request and kept, not built for the whole country up front.**
//! Storage grows only where somebody has actually been, there is no national batch
//! run to schedule, and the first request for a tile costs one PDOK round trip —
//! which the client already paid itself, per 6.4 km box, before this existed
//! (`docs/notes/atlas.md`).
//!
//! Two answers besides a tile, and they are not the same thing: a tile outside
//! AHN's envelope, or one where AHN measured nothing (open sea), is **no data** —
//! a 404 the client reads as "flat here" — while PDOK failing is an error, a 502,
//! and is never remembered, because the next request may well succeed.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use cartopolis_geo::{ahn, terrain_tile};
use tokio::sync::{Mutex, Semaphore};

/// Zooms served. Below 8 a tile is larger than the country and the grid would be
/// sampling the North Sea; above 17 a sample is under 1.5 m apart and AHN's own
/// 0.5 m raster is the better thing to ask for directly.
pub const MIN_ZOOM: u32 = 8;
pub const MAX_ZOOM: u32 = 17;

/// Largest raster asked of PDOK for one tile. The client's own ceiling is 4096
/// and PDOK serves 2049² in ~4.5 s; a tile needs far less (see `raster_size`).
const MAX_RASTER: usize = 1024;

/// Where AHN heights come from. A trait so the service can be tested without the
/// network, and so a pre-built national raster can stand in for PDOK later.
pub trait HeightSource: Send + Sync + 'static {
    /// The bare-ground heights over an RD box, `n × n`, north row first, `NaN`
    /// where nothing was measured. Blocking.
    fn raster(&self, min: [f64; 2], max: [f64; 2], n: usize) -> Result<Vec<f32>, String>;
}

/// PDOK's WCS, the same service and request the client's height layer uses
/// (`cartopolis_geo::ahn::coverage_url`).
pub struct Pdok {
    agent: ureq::Agent,
}

impl Pdok {
    #[must_use]
    pub fn new() -> Self {
        Self {
            agent: ureq::AgentBuilder::new()
                .timeout(std::time::Duration::from_secs(30))
                .user_agent("cartopolis-atlas (+https://cartopolis.org)")
                .build(),
        }
    }
}

impl Default for Pdok {
    fn default() -> Self {
        Self::new()
    }
}

impl HeightSource for Pdok {
    fn raster(&self, min: [f64; 2], max: [f64; 2], n: usize) -> Result<Vec<f32>, String> {
        use std::io::Read;
        let url = ahn::coverage_url(min, max, n as u32);
        let response = self.agent.get(&url).call().map_err(|e| e.to_string())?;
        let mut bytes = Vec::new();
        // A float32 raster is 4 bytes a sample; anything much larger is not one.
        response
            .into_reader()
            .take((n * n * 4 + 1_000_000) as u64)
            .read_to_end(&mut bytes)
            .map_err(|e| e.to_string())?;
        ahn::decode_float_tiff(&bytes, n)
    }
}

/// What a request for a tile comes to.
#[derive(Debug, PartialEq)]
pub enum Answer {
    /// The encoded tile.
    Tile(Vec<u8>),
    /// Nothing measured here — outside AHN, or all water.
    Nothing,
}

#[derive(Debug, PartialEq)]
pub enum TerrainError {
    /// A zoom this service does not cut, or a tile index outside the zoom.
    OutOfRange,
    /// PDOK failed. Not remembered.
    Upstream(String),
}

type Key = (u32, i64, i64);

pub struct TerrainService {
    source: Arc<dyn HeightSource>,
    dir: PathBuf,
    upstream: Semaphore,
    /// One cut per tile at a time: a second request for a tile being cut waits for
    /// the first and then reads its result from disk, rather than asking PDOK again.
    cutting: Mutex<HashMap<Key, Arc<Mutex<()>>>>,
}

impl TerrainService {
    pub fn new(
        source: Arc<dyn HeightSource>,
        data_dir: &Path,
        upstream_concurrency: usize,
    ) -> Self {
        Self {
            source,
            dir: data_dir.join("terrain"),
            upstream: Semaphore::new(upstream_concurrency.max(1)),
            cutting: Mutex::new(HashMap::new()),
        }
    }

    pub async fn tile(&self, z: u32, x: i64, y: i64) -> Result<Answer, TerrainError> {
        let n = 1i64 << z.min(MAX_ZOOM);
        if !(MIN_ZOOM..=MAX_ZOOM).contains(&z) || !(0..n).contains(&x) || !(0..n).contains(&y) {
            return Err(TerrainError::OutOfRange);
        }
        // Outside AHN there is nothing to ask for, and no need to remember that.
        let (lat, lng) =
            cartopolis_core::world::tile_frac_to_lat_lng(x as f64 + 0.5, y as f64 + 0.5, z);
        if !ahn::covers(lat, lng) {
            return Ok(Answer::Nothing);
        }
        if let Some(answer) = self.stored(z, x, y).await {
            return Ok(answer);
        }

        let gate = {
            let mut cutting = self.cutting.lock().await;
            cutting.entry((z, x, y)).or_default().clone()
        };
        let _cut = gate.lock().await;
        // Somebody may have cut it while this request waited.
        if let Some(answer) = self.stored(z, x, y).await {
            return Ok(answer);
        }
        let result = {
            let _permit = self
                .upstream
                .acquire()
                .await
                .map_err(|e| TerrainError::Upstream(e.to_string()))?;
            let source = self.source.clone();
            tokio::task::spawn_blocking(move || cut(source.as_ref(), z, x, y))
                .await
                .map_err(|e| TerrainError::Upstream(e.to_string()))?
        };
        let answer = match result {
            Ok(Some(tile)) => Answer::Tile(tile.encode()),
            Ok(None) => Answer::Nothing,
            Err(e) => {
                tracing::warn!(z, x, y, error = %e, "terrain tile cut failed");
                return Err(TerrainError::Upstream(e));
            }
        };
        self.store(z, x, y, &answer).await;
        self.cutting.lock().await.remove(&(z, x, y));
        Ok(answer)
    }

    fn path(&self, z: u32, x: i64, y: i64, ext: &str) -> PathBuf {
        self.dir.join(format!("{z}/{x}/{y}.{ext}"))
    }

    // The disk is read and written with `std::fs` on a blocking thread, not
    // `tokio::fs`: that needs tokio's `fs` feature, and turning a feature on for one
    // member changes tokio's build for every crate in a workspace build — the
    // "one dependency's feature forks the whole build" trap.
    async fn stored(&self, z: u32, x: i64, y: i64) -> Option<Answer> {
        let (tile, none) = (self.path(z, x, y, "ctt"), self.path(z, x, y, "none"));
        tokio::task::spawn_blocking(move || {
            if let Ok(bytes) = std::fs::read(&tile) {
                // A torn or foreign file is a miss, and gets cut again.
                if terrain_tile::TerrainTile::decode(&bytes).is_ok() {
                    return Some(Answer::Tile(bytes));
                }
            }
            none.exists().then_some(Answer::Nothing)
        })
        .await
        .ok()
        .flatten()
    }

    /// Through a temporary file and a rename, so a reader never sees half a tile.
    async fn store(&self, z: u32, x: i64, y: i64, answer: &Answer) {
        let (path, bytes) = match answer {
            Answer::Tile(b) => (self.path(z, x, y, "ctt"), b.clone()),
            Answer::Nothing => (self.path(z, x, y, "none"), Vec::new()),
        };
        let _ = tokio::task::spawn_blocking(move || {
            if let Some(parent) = path.parent() {
                let _ = std::fs::create_dir_all(parent);
            }
            let tmp = path.with_extension("tmp");
            if std::fs::write(&tmp, bytes).is_ok() {
                let _ = std::fs::rename(&tmp, &path);
            }
        })
        .await;
    }
}

/// How many raster pixels to ask for across `span` metres: two per sample
/// spacing, so the bilinear lookup reads the ground rather than one pixel of it,
/// never finer than AHN's own half metre, and bounded.
fn raster_size(span_m: f64, spacing_m: f64) -> usize {
    let pixel = (spacing_m / 2.0).max(0.5);
    ((span_m / pixel).ceil() as usize + 1).clamp(2, MAX_RASTER)
}

/// Cut one tile: ask for the RD box its samples fall in, close the holes AHN
/// leaves under buildings, and sample it on the tile's own grid. `None` when
/// nothing was measured.
fn cut(
    source: &dyn HeightSource,
    z: u32,
    x: i64,
    y: i64,
) -> Result<Option<terrain_tile::TerrainTile>, String> {
    let points = terrain_tile::sample_points(z, x, y);
    let (lat, _) = cartopolis_core::world::tile_frac_to_lat_lng(x as f64 + 0.5, y as f64 + 0.5, z);
    let spacing = cartopolis_core::world::tile_to_meters(z, lat) / (terrain_tile::GRID - 1) as f64;
    let (min, max) = terrain_tile::rd_box(&points, spacing);
    let span = (max[0] - min[0]).max(max[1] - min[1]);
    // A square raster over a square box, so a pixel is the same size both ways.
    let max = [min[0] + span, min[1] + span];
    let n = raster_size(span, spacing);
    let mut raster = source.raster(min, max, n)?;
    if raster.len() != n * n {
        return Err(format!("asked for {n}², got {} samples", raster.len()));
    }
    let pixel_m = (span / (n - 1) as f64) as f32;
    ahn::fill_holes(&mut raster, n, Some(ahn::FILL_REACH_M / pixel_m));
    let tile = terrain_tile::TerrainTile {
        grid: terrain_tile::GRID,
        heights: terrain_tile::resample(&raster, n, min, max, &points),
    };
    Ok(tile.has_data().then_some(tile))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};

    /// A tilted plane in RD, counting how often it is asked.
    struct Plane {
        calls: AtomicUsize,
        all_water: bool,
    }

    impl HeightSource for Plane {
        fn raster(&self, min: [f64; 2], max: [f64; 2], n: usize) -> Result<Vec<f32>, String> {
            self.calls.fetch_add(1, Ordering::SeqCst);
            std::thread::sleep(std::time::Duration::from_millis(20));
            let mut out = vec![f32::NAN; n * n];
            if !self.all_water {
                for row in 0..n {
                    for col in 0..n {
                        let e = min[0] + (max[0] - min[0]) * col as f64 / (n - 1) as f64;
                        let north = max[1] - (max[1] - min[1]) * row as f64 / (n - 1) as f64;
                        out[row * n + col] =
                            (0.001 * (e - 230_000.0) + 0.002 * (north - 580_000.0)) as f32;
                    }
                }
            }
            Ok(out)
        }
    }

    fn service(all_water: bool) -> (Arc<Plane>, TerrainService, tempfile::TempDir) {
        let dir = tempfile::tempdir().unwrap();
        let plane = Arc::new(Plane {
            calls: AtomicUsize::new(0),
            all_water,
        });
        let svc = TerrainService::new(plane.clone(), dir.path(), 3);
        (plane, svc, dir)
    }

    /// The tile over central Groningen (Grote Markt).
    const GRONINGEN: (u32, i64, i64) = (14, 8490, 5318);

    #[tokio::test]
    async fn a_dutch_tile_is_cut_once_and_then_read_from_disk() {
        let (plane, svc, _dir) = service(false);
        let (z, x, y) = GRONINGEN;
        let Answer::Tile(bytes) = svc.tile(z, x, y).await.unwrap() else {
            panic!("no tile over Groningen");
        };
        let tile = terrain_tile::TerrainTile::decode(&bytes).unwrap();
        assert!(tile.heights.iter().all(|h| h.is_finite()));
        // The plane, at a sample: the tile is the ground it was cut from.
        let (lat, lng) = terrain_tile::sample_points(z, x, y)[0];
        let [e, n] = ahn::wgs84_to_rd(lat, lng);
        let want = (0.001 * (e - 230_000.0) + 0.002 * (n - 580_000.0)) as f32;
        assert!(
            (tile.at(0, 0) - want).abs() < 0.02,
            "{} vs {want}",
            tile.at(0, 0)
        );

        assert_eq!(svc.tile(z, x, y).await.unwrap(), Answer::Tile(bytes));
        assert_eq!(
            plane.calls.load(Ordering::SeqCst),
            1,
            "the second request asked PDOK again"
        );
    }

    #[tokio::test]
    async fn concurrent_requests_for_one_tile_cut_it_once() {
        let (plane, svc, _dir) = service(false);
        let svc = Arc::new(svc);
        let (z, x, y) = GRONINGEN;
        let jobs: Vec<_> = (0..4)
            .map(|_| {
                let svc = svc.clone();
                tokio::spawn(async move { svc.tile(z, x, y).await })
            })
            .collect();
        for j in jobs {
            assert!(matches!(j.await.unwrap(), Ok(Answer::Tile(_))));
        }
        assert_eq!(plane.calls.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn outside_ahn_is_nothing_without_asking() {
        let (plane, svc, _dir) = service(false);
        // Paris.
        let (x, y) = cartopolis_core::lat_lng_to_tile(48.8566, 2.3522, 14);
        assert_eq!(svc.tile(14, x, y).await.unwrap(), Answer::Nothing);
        assert_eq!(plane.calls.load(Ordering::SeqCst), 0);
    }

    #[tokio::test]
    async fn open_water_is_nothing_and_is_remembered() {
        let (plane, svc, _dir) = service(true);
        let (z, x, y) = GRONINGEN;
        assert_eq!(svc.tile(z, x, y).await.unwrap(), Answer::Nothing);
        assert_eq!(svc.tile(z, x, y).await.unwrap(), Answer::Nothing);
        assert_eq!(plane.calls.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn a_zoom_outside_the_served_range_is_refused() {
        let (_, svc, _dir) = service(false);
        assert_eq!(
            svc.tile(MAX_ZOOM + 1, 0, 0).await,
            Err(TerrainError::OutOfRange)
        );
        assert_eq!(svc.tile(14, -1, 0).await, Err(TerrainError::OutOfRange));
        assert_eq!(
            svc.tile(14, 1 << 14, 0).await,
            Err(TerrainError::OutOfRange)
        );
    }

    #[test]
    fn the_raster_stays_bounded_and_never_finer_than_ahn() {
        // z17: ~1.4 m spacing over ~190 m → 0.7 m pixels.
        assert!(raster_size(190.0, 1.43) <= 300);
        // Never below AHN's half metre, however fine the tile.
        assert_eq!(raster_size(100.0, 0.2), 201);
        // z8's 600 km would want thousands of pixels; it is capped.
        assert_eq!(raster_size(600_000.0, 4_700.0), 257);
        assert!(raster_size(10_000_000.0, 1.0) <= MAX_RASTER);
    }
}
