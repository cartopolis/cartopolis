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
    /// The heights over an RD box, `n × n`, north row first, `NaN`
    /// where nothing was measured. Blocking.
    fn raster(&self, min: [f64; 2], max: [f64; 2], n: usize) -> Result<Vec<f32>, String>;

    /// [`Self::raster`] for either model. The surface model is what the client's
    /// bridge decks are measured against (`/height`); a source that only knows
    /// the ground says so.
    fn raster_of(
        &self,
        model: Model,
        min: [f64; 2],
        max: [f64; 2],
        n: usize,
    ) -> Result<Vec<f32>, String> {
        match model {
            Model::Terrain => self.raster(min, max, n),
            Model::Surface => Err("this source has no surface model".into()),
        }
    }
}

/// Which of AHN's two rasters: the bare ground, with buildings and bridge decks
/// cut out, or everything the laser hit.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Model {
    Terrain,
    Surface,
}

impl Model {
    /// PDOK's coverage name, and the `model=` value `/height` takes.
    #[must_use]
    pub fn coverage(self) -> &'static str {
        match self {
            Model::Terrain => "dtm_05m",
            Model::Surface => "dsm_05m",
        }
    }

    #[must_use]
    pub fn from_coverage(name: &str) -> Option<Self> {
        match name {
            "dtm_05m" => Some(Model::Terrain),
            "dsm_05m" => Some(Model::Surface),
            _ => None,
        }
    }
}

/// PDOK's WCS, the same service and request the client's height layer uses
/// (`cartopolis_geo::ahn::coverage_url`).
pub struct Pdok {
    agent: ureq::Agent,
    /// `ahn::DTM` or `ahn::DSM`.
    coverage: &'static str,
}

impl Pdok {
    /// The bare ground, which is what every terrain tile is cut from.
    #[must_use]
    pub fn new() -> Self {
        Self::of(ahn::DTM)
    }

    /// The surface model: bridge decks at their real height, for measuring them
    /// (`pipeline::terrain_grade`).
    #[must_use]
    pub fn surface() -> Self {
        Self::of(ahn::DSM)
    }

    fn of(coverage: &'static str) -> Self {
        Self {
            agent: ureq::AgentBuilder::new()
                .timeout(std::time::Duration::from_secs(30))
                .user_agent("cartopolis-atlas (+https://cartopolis.org)")
                .build(),
            coverage,
        }
    }
}

impl Default for Pdok {
    fn default() -> Self {
        Self::new()
    }
}

impl HeightSource for Pdok {
    /// The coverage this source was built for ([`Pdok::new`], [`Pdok::surface`]).
    fn raster(&self, min: [f64; 2], max: [f64; 2], n: usize) -> Result<Vec<f32>, String> {
        self.coverage_raster(self.coverage, min, max, n)
    }

    /// The named model, whichever this source was built for.
    fn raster_of(
        &self,
        model: Model,
        min: [f64; 2],
        max: [f64; 2],
        n: usize,
    ) -> Result<Vec<f32>, String> {
        self.coverage_raster(model.coverage(), min, max, n)
    }
}

impl Pdok {
    fn coverage_raster(
        &self,
        coverage: &str,
        min: [f64; 2],
        max: [f64; 2],
        n: usize,
    ) -> Result<Vec<f32>, String> {
        use std::io::Read;
        let url = ahn::coverage_url_of(coverage, min, max, n as u32);
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

/// Largest grid `/height` answers: the client's own ceiling (`height::MAX_GRID`),
/// which its 2049² terrain field needs.
pub const MAX_PATCH_GRID: usize = 4096;

/// Largest box `/height` answers, RD metres a side: the client's terrain field is
/// 6.4 km, and nothing it asks is bigger.
pub const MAX_PATCH_SPAN_M: f64 = 10_000.0;

/// `/height`: a box of either model at a chosen grid, the question the client's
/// height layer used to put to PDOK directly — answered from the same source as
/// the terrain tiles, so a deck and the ground it stands over come from one
/// flight.
pub struct HeightPatches {
    source: Arc<dyn HeightSource>,
    upstream: Semaphore,
}

impl HeightPatches {
    pub fn new(source: Arc<dyn HeightSource>, upstream_concurrency: usize) -> Self {
        Self {
            source,
            upstream: Semaphore::new(upstream_concurrency.max(1)),
        }
    }

    /// `n × n` heights over the box, north row first, `NaN` where nothing was
    /// measured. `Err(None)` for a request outside what is served.
    pub async fn patch(
        &self,
        model: Model,
        min: [f64; 2],
        max: [f64; 2],
        n: usize,
    ) -> Result<Vec<f32>, Option<String>> {
        let span = (max[0] - min[0]).max(max[1] - min[1]);
        if !(2..=MAX_PATCH_GRID).contains(&n)
            || !(max[0] > min[0] && max[1] > min[1])
            || span > MAX_PATCH_SPAN_M
            || ![min[0], min[1], max[0], max[1]]
                .iter()
                .all(|v| v.is_finite())
        {
            return Err(None);
        }
        let _permit = self
            .upstream
            .acquire()
            .await
            .map_err(|e| Some(e.to_string()))?;
        let source = self.source.clone();
        tokio::task::spawn_blocking(move || source.raster_of(model, min, max, n))
            .await
            .map_err(|e| Some(e.to_string()))?
            .map_err(Some)
    }
}

/// A float32 GeoTIFF of `n × n` samples, deflated — the shape PDOK's WCS answers
/// in, so the client's decoder (`cartopolis_geo::ahn::decode_float_tiff`) reads
/// either without knowing which it asked.
pub fn encode_float_tiff(samples: &[f32], n: usize) -> Result<Vec<u8>, String> {
    use tiff::encoder::{
        colortype::Gray32Float, compression::DeflateLevel, Compression, TiffEncoder,
    };
    let mut out = std::io::Cursor::new(Vec::new());
    TiffEncoder::new(&mut out)
        .map_err(|e| e.to_string())?
        .with_compression(Compression::Deflate(DeflateLevel::Fast))
        .write_image::<Gray32Float>(n as u32, n as u32, samples)
        .map_err(|e| e.to_string())?;
    Ok(out.into_inner())
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

type Key = (u32, i64, i64, usize);

/// The grids a tile may be cut at: the tile tree's own, and the dense one the
/// client's terrain field is assembled from (`terrain_tile::DENSE_GRID`).
#[must_use]
pub fn valid_grid(grid: usize) -> bool {
    grid == terrain_tile::GRID || grid == terrain_tile::DENSE_GRID
}

pub struct TerrainService {
    source: Arc<dyn HeightSource>,
    dir: PathBuf,
    graded: PathBuf,
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
            dir: store_dir(data_dir),
            graded: graded_dir(data_dir),
            upstream: Semaphore::new(upstream_concurrency.max(1)),
            cutting: Mutex::new(HashMap::new()),
        }
    }

    pub async fn tile(&self, z: u32, x: i64, y: i64) -> Result<Answer, TerrainError> {
        self.tile_at(z, x, y, terrain_tile::GRID).await
    }

    /// A graded tile, if the pipeline has graded it. Never cut or graded here:
    /// `None` is the client's cue to fetch the raw tile and grade it itself.
    ///
    /// The pipeline grades at [`terrain_tile::DENSE_GRID`] only. The tile tree's
    /// grid is that tile on every fourth sample — what a handset's field, ~12 m
    /// apart, is assembled from, at a sixteenth of the bytes — thinned
    /// on each request, which costs about a millisecond. Not kept: the pipeline
    /// re-grades in place, and a kept copy would outlive the tile it came from.
    pub async fn graded(&self, z: u32, x: i64, y: i64, grid: usize) -> Option<Answer> {
        let dir = self.graded.clone();
        tokio::task::spawn_blocking(move || {
            if let Some(answer) = read_stored(&dir, z, x, y, grid) {
                return Some(answer);
            }
            if grid != terrain_tile::GRID {
                return None;
            }
            let answer = match read_stored(&dir, z, x, y, terrain_tile::DENSE_GRID)? {
                Answer::Tile(bytes) => {
                    let dense = terrain_tile::TerrainTile::decode(&bytes).ok()?;
                    let step = (terrain_tile::DENSE_GRID - 1) / (terrain_tile::GRID - 1);
                    Answer::Tile(dense.every(step)?.encode())
                }
                Answer::Nothing => Answer::Nothing,
            };
            Some(answer)
        })
        .await
        .ok()
        .flatten()
    }

    /// [`Self::tile`] at a chosen grid ([`valid_grid`]).
    pub async fn tile_at(
        &self,
        z: u32,
        x: i64,
        y: i64,
        grid: usize,
    ) -> Result<Answer, TerrainError> {
        let n = 1i64 << z.min(MAX_ZOOM);
        if !(MIN_ZOOM..=MAX_ZOOM).contains(&z)
            || !(0..n).contains(&x)
            || !(0..n).contains(&y)
            || !valid_grid(grid)
        {
            return Err(TerrainError::OutOfRange);
        }
        // Outside AHN there is nothing to ask for, and no need to remember that.
        let (lat, lng) =
            cartopolis_core::world::tile_frac_to_lat_lng(x as f64 + 0.5, y as f64 + 0.5, z);
        if !ahn::covers(lat, lng) {
            return Ok(Answer::Nothing);
        }
        if let Some(answer) = self.stored(z, x, y, grid).await {
            return Ok(answer);
        }

        let gate = {
            let mut cutting = self.cutting.lock().await;
            cutting.entry((z, x, y, grid)).or_default().clone()
        };
        let _cut = gate.lock().await;
        // Somebody may have cut it while this request waited.
        if let Some(answer) = self.stored(z, x, y, grid).await {
            return Ok(answer);
        }
        let result = {
            let _permit = self
                .upstream
                .acquire()
                .await
                .map_err(|e| TerrainError::Upstream(e.to_string()))?;
            let source = self.source.clone();
            tokio::task::spawn_blocking(move || cut(source.as_ref(), z, x, y, grid))
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
        self.store(z, x, y, grid, &answer).await;
        self.cutting.lock().await.remove(&(z, x, y, grid));
        Ok(answer)
    }

    // The disk is read and written with `std::fs` on a blocking thread, not
    // `tokio::fs`: that needs tokio's `fs` feature, and turning a feature on for one
    // member changes tokio's build for every crate in a workspace build — the
    // "one dependency's feature forks the whole build" trap.
    async fn stored(&self, z: u32, x: i64, y: i64, grid: usize) -> Option<Answer> {
        let dir = self.dir.clone();
        tokio::task::spawn_blocking(move || read_stored(&dir, z, x, y, grid))
            .await
            .ok()
            .flatten()
    }

    async fn store(&self, z: u32, x: i64, y: i64, grid: usize, answer: &Answer) {
        let dir = self.dir.clone();
        let answer = match answer {
            Answer::Tile(b) => Answer::Tile(b.clone()),
            Answer::Nothing => Answer::Nothing,
        };
        let _ =
            tokio::task::spawn_blocking(move || write_stored(&dir, z, x, y, grid, &answer)).await;
    }
}

/// Where tiles are kept under the data directory. **A new directory whenever the
/// tiles change meaning**, rather than a purge, so an old tile can never be
/// served as a new one: `terrain/` held tiles cut with their holes already filled
/// (a client that carves water into a filled tile carves into ground the fill has
/// already smeared across the banks, `docs/notes/level-of-detail-tile-tree.md`,
/// step 2a), and `terrain-v2` holds tiles cut from AHN4 alone — which over
/// Groningen's southern ring is a trench that has since been roofed over
/// (`ahn6`).
#[must_use]
pub fn store_dir(data_dir: &Path) -> PathBuf {
    data_dir.join("terrain-v3")
}

/// Any source's **surface** model as a source of its own: what a caller that asks
/// [`HeightSource::raster`] of a DSM expects (`pipeline::terrain_grade`'s deck
/// measurement), over a source that serves both models.
pub struct SurfaceOf(pub Arc<dyn HeightSource>);

impl HeightSource for SurfaceOf {
    fn raster(&self, min: [f64; 2], max: [f64; 2], n: usize) -> Result<Vec<f32>, String> {
        self.0.raster_of(Model::Surface, min, max, n)
    }

    fn raster_of(
        &self,
        model: Model,
        min: [f64; 2],
        max: [f64; 2],
        n: usize,
    ) -> Result<Vec<f32>, String> {
        self.0.raster_of(model, min, max, n)
    }
}

/// Where the **graded** tiles are kept (`pipeline::terrain_grade`): the same
/// ground, water carved, holes filled, roads flattened and bridge approaches
/// ramped, so the client only has to sample it. Only the pipeline writes here;
/// nothing is graded on a request.
#[must_use]
pub fn graded_dir(data_dir: &Path) -> PathBuf {
    data_dir.join("terrain-graded-v1")
}

/// `{z}/{x}/{y}.ctt` for the tile tree's grid, `{z}/{x}/{y}.g513.ctt` for another,
/// so the two cuts of one tile never overwrite each other.
fn tile_path(dir: &Path, z: u32, x: i64, y: i64, grid: usize, ext: &str) -> PathBuf {
    if grid == terrain_tile::GRID {
        dir.join(format!("{z}/{x}/{y}.{ext}"))
    } else {
        dir.join(format!("{z}/{x}/{y}.g{grid}.{ext}"))
    }
}

pub(crate) fn read_stored(dir: &Path, z: u32, x: i64, y: i64, grid: usize) -> Option<Answer> {
    if let Ok(bytes) = std::fs::read(tile_path(dir, z, x, y, grid, "ctt")) {
        // A torn or foreign file is a miss, and gets cut again.
        if terrain_tile::TerrainTile::decode(&bytes).is_ok_and(|t| t.grid == grid) {
            return Some(Answer::Tile(bytes));
        }
    }
    tile_path(dir, z, x, y, grid, "none")
        .exists()
        .then_some(Answer::Nothing)
}

/// Through a temporary file and a rename, so a reader never sees half a tile. The
/// temporary name is the writer's own: the service and a pipeline run may cut the
/// same tile at once, and two writers sharing one temporary file can rename the
/// other's half-written bytes into place.
pub(crate) fn write_stored(dir: &Path, z: u32, x: i64, y: i64, grid: usize, answer: &Answer) {
    let (path, bytes): (PathBuf, &[u8]) = match answer {
        Answer::Tile(b) => (tile_path(dir, z, x, y, grid, "ctt"), b),
        Answer::Nothing => (tile_path(dir, z, x, y, grid, "none"), &[]),
    };
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    static WRITES: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let n = WRITES.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let tmp = path.with_extension(format!("{}.{n}.tmp", std::process::id()));
    if std::fs::write(&tmp, bytes).is_ok() {
        let _ = std::fs::rename(&tmp, &path);
    }
}

/// What [`precut`] did with one tile.
#[derive(Debug, PartialEq)]
pub enum Precut {
    /// Already in the store, from an earlier run or a player's request.
    Stored,
    /// Cut now, with measurements.
    Cut,
    /// Cut now, and nothing was measured there.
    Nothing,
}

/// Cut one tile into the store if it is not there yet — the pipeline's path to
/// the same store the HTTP route fills on demand (`pipeline::terrain`). Blocking.
pub fn precut(
    source: &dyn HeightSource,
    data_dir: &Path,
    z: u32,
    x: i64,
    y: i64,
    grid: usize,
) -> Result<Precut, String> {
    let dir = store_dir(data_dir);
    if read_stored(&dir, z, x, y, grid).is_some() {
        return Ok(Precut::Stored);
    }
    let (answer, outcome) = match cut(source, z, x, y, grid)? {
        Some(tile) => (Answer::Tile(tile.encode()), Precut::Cut),
        None => (Answer::Nothing, Precut::Nothing),
    };
    write_stored(&dir, z, x, y, grid, &answer);
    Ok(outcome)
}

/// How many raster pixels to ask for across `span` metres: two per sample
/// spacing, so the bilinear lookup reads the ground rather than one pixel of it,
/// never finer than AHN's own half metre, and bounded.
fn raster_size(span_m: f64, spacing_m: f64) -> usize {
    let pixel = (spacing_m / 2.0).max(0.5);
    ((span_m / pixel).ceil() as usize + 1).clamp(2, MAX_RASTER)
}

/// Cut one tile: ask for the RD box its samples fall in and sample it on the
/// tile's own grid. `None` when nothing was measured.
///
/// **The holes are left open** — under buildings, where AHN's bare-ground model
/// has none, and over water. Filling is the client's job, because it has to carve
/// the registered water levels in *first*; a tile filled here has already pulled
/// the banks across the water, and carving that lowers a smear rather than the
/// water.
fn cut(
    source: &dyn HeightSource,
    z: u32,
    x: i64,
    y: i64,
    grid: usize,
) -> Result<Option<terrain_tile::TerrainTile>, String> {
    let points = terrain_tile::sample_points_grid(z, x, y, grid);
    let (lat, _) = cartopolis_core::world::tile_frac_to_lat_lng(x as f64 + 0.5, y as f64 + 0.5, z);
    let spacing = cartopolis_core::world::tile_to_meters(z, lat) / (grid - 1) as f64;
    let (min, max) = terrain_tile::rd_box(&points, spacing);
    let span = (max[0] - min[0]).max(max[1] - min[1]);
    // A square raster over a square box, so a pixel is the same size both ways.
    let max = [min[0] + span, min[1] + span];
    let n = raster_size(span, spacing);
    let raster = source.raster(min, max, n)?;
    if raster.len() != n * n {
        return Err(format!("asked for {n}², got {} samples", raster.len()));
    }
    let tile = terrain_tile::TerrainTile {
        grid,
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

    /// A tile over the north of Groningen city (the Grote Markt is 8490/5320).
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
