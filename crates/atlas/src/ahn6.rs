//! **AHN6**, the 2025 flight of the national height model, as a [`HeightSource`]
//! that falls back to PDOK's AHN4 wherever AHN6 has not been published yet.
//!
//! Why it matters is one place in Groningen and every place like it. AHN4 was
//! flown while the southern ring (N7) was an open trench; the ring has since been
//! roofed over, and a map standing on AHN4 draws a seven-metre pit where people
//! walk on a park. Over the covered tunnel the two surveys read **−7.41 m NAP
//! (AHN4) against +2.42 m (AHN6)**, and the rebuilt Emmaviaduct's deck 7.8 m
//! against 8.7 m (measured 2026-09-24 by sampling both inside the BGT outlines).
//!
//! AHN6 is not a WCS. It is published as 1 km GeoTIFFs — 0.5 m and 5 m, a terrain
//! (`M`) and a surface (`R`) model each — found through a sheet index, so this
//! module is the WCS it lacks: it answers "this RD box at this grid" the way
//! PDOK's `GetCoverage` does, by sampling the tiles under it.
//!
//! * **Only the 256² blocks a request touches are decoded**, and kept in a small
//!   cache: a tile is 2000² float32, 16 MB decoded, and a terrain cut reads a
//!   strip of it.
//! * **Tiles are downloaded whole and kept** under the data directory. 8–10 MB
//!   each; Groningen's 10 km square is ~1.8 GB for both models. What is stored
//!   grows only where somebody has been, like the terrain tiles cut from it.
//! * **Coarse requests read the 5 m product** — a 6 km box at 3 m pixels would
//!   otherwise download every half-metre tile under it to read one pixel in six.
//! * **A sheet the index does not know is AHN4**, per sample, from PDOK. Coverage
//!   is regional while the flights are processed (Groningen, Zwolle and Delfzijl
//!   are in; Amsterdam, Utrecht and Rotterdam were not, 2026-09-24), so a box can
//!   straddle the edge and gets both.

use std::collections::{HashMap, VecDeque};
use std::io::Read;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{Duration, SystemTime};

use crate::pipeline::rd::rd_to_lat_lng;
use crate::terrain::{HeightSource, Model};

/// The sheet index: an OGC Features API over the published tiles, with each
/// tile's download links as properties.
const INDEX: &str = "https://api.ellipsis-drive.com/v3/ogc/features/\
     0820faae-5240-499b-8486-cf406433cf71/collections/6aec07f5-f7eb-4f51-b6f7-aee45e5767bd/items";

/// Edge of one published tile, RD metres.
const SHEET_M: f64 = 1000.0;

/// At or above this output pixel size a request reads the 5 m product.
const COARSE_PIXEL_M: f64 = 5.0;

/// How long "the index has no sheet here" is believed. The flights are still
/// being processed and published region by region, so a miss is news with a
/// shelf life, not a fact.
const MISS_TTL: Duration = Duration::from_secs(7 * 24 * 3600);

/// Decoded blocks kept, 256 KB each.
const CHUNK_CACHE: usize = 512;

/// The sheet's own no-data value, and anything below this is not ground.
const NODATA_BELOW: f32 = -1000.0;

/// One published tile's four rasters.
#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct Sheet {
    pub dtm: String,
    pub dsm: String,
    pub dtm5: String,
    pub dsm5: String,
}

impl Sheet {
    fn url(&self, model: Model, coarse: bool) -> &str {
        match (model, coarse) {
            (Model::Terrain, false) => &self.dtm,
            (Model::Terrain, true) => &self.dtm5,
            (Model::Surface, false) => &self.dsm,
            (Model::Surface, true) => &self.dsm5,
        }
    }
}

/// What the index said about one sheet, as kept on disk.
#[derive(serde::Serialize, serde::Deserialize)]
struct IndexEntry {
    sheet: Option<Sheet>,
    /// Seconds since the epoch the answer was fetched.
    at: u64,
}

/// One decoded block of one raster.
struct Chunk {
    w: usize,
    h: usize,
    data: Vec<f32>,
}

type ChunkKey = (PathBuf, u32);

pub struct Ahn6 {
    agent: ureq::Agent,
    dir: PathBuf,
    fallback: Arc<dyn HeightSource>,
    index: Mutex<HashMap<(i64, i64), Option<Sheet>>>,
    chunks: Mutex<(HashMap<ChunkKey, Arc<Chunk>>, VecDeque<ChunkKey>)>,
    /// One download per file at a time; a second request for it waits and reads
    /// the finished file.
    downloads: Mutex<HashMap<PathBuf, Arc<Mutex<()>>>>,
}

impl Ahn6 {
    #[must_use]
    pub fn new(data_dir: &Path, fallback: Arc<dyn HeightSource>) -> Self {
        Self {
            agent: ureq::AgentBuilder::new()
                .timeout(Duration::from_secs(120))
                .user_agent("cartopolis-atlas (+https://cartopolis.org)")
                .build(),
            dir: data_dir.join("ahn6"),
            fallback,
            index: Mutex::new(HashMap::new()),
            chunks: Mutex::new((HashMap::new(), VecDeque::new())),
            downloads: Mutex::new(HashMap::new()),
        }
    }

    /// The published sheet with south-west corner `(x, y)` km·1000, or `None`
    /// where AHN6 has none. `Err` when the index could not be asked — which is
    /// not a miss, and is not remembered.
    fn sheet(&self, x: i64, y: i64) -> Result<Option<Sheet>, String> {
        if let Some(s) = self.index.lock().map_err(|e| e.to_string())?.get(&(x, y)) {
            return Ok(s.clone());
        }
        let path = self.dir.join(format!("index/{x}_{y}.json"));
        let now = SystemTime::now()
            .duration_since(SystemTime::UNIX_EPOCH)
            .map_or(0, |d| d.as_secs());
        if let Some(entry) = std::fs::read(&path)
            .ok()
            .and_then(|b| serde_json::from_slice::<IndexEntry>(&b).ok())
        {
            if entry.sheet.is_some() || now.saturating_sub(entry.at) < MISS_TTL.as_secs() {
                self.remember(x, y, entry.sheet.clone());
                return Ok(entry.sheet);
            }
        }
        let sheet = self.ask_index(x, y)?;
        let entry = IndexEntry {
            sheet: sheet.clone(),
            at: now,
        };
        if let Ok(bytes) = serde_json::to_vec(&entry) {
            write_atomic(&path, &bytes);
        }
        self.remember(x, y, sheet.clone());
        Ok(sheet)
    }

    fn remember(&self, x: i64, y: i64, sheet: Option<Sheet>) {
        if let Ok(mut index) = self.index.lock() {
            index.insert((x, y), sheet);
        }
    }

    fn ask_index(&self, x: i64, y: i64) -> Result<Option<Sheet>, String> {
        // A small box around the sheet's middle: the index is queried in degrees,
        // and a box on the corner would answer with the neighbours too.
        let (lat, lng) = rd_to_lat_lng(x as f64 + SHEET_M / 2.0, y as f64 + SHEET_M / 2.0);
        let url = format!(
            "{INDEX}?bbox={:.6},{:.6},{:.6},{:.6}&limit=10",
            lng - 0.0005,
            lat - 0.0005,
            lng + 0.0005,
            lat + 0.0005
        );
        let text = self
            .agent
            .get(&url)
            .call()
            .map_err(|e| format!("AHN6 index: {e}"))?
            .into_string()
            .map_err(|e| format!("AHN6 index: {e}"))?;
        let body: serde_json::Value =
            serde_json::from_str(&text).map_err(|e| format!("AHN6 index: {e}"))?;
        Ok(sheet_from_index(&body, x, y))
    }

    /// The local copy of one raster, downloading it on first use.
    fn raster_file(&self, url: &str) -> Result<PathBuf, String> {
        let name = url.rsplit('/').next().unwrap_or(url);
        let path = self.dir.join("tiles").join(name);
        if path.exists() {
            return Ok(path);
        }
        let gate = {
            let mut downloads = self.downloads.lock().map_err(|e| e.to_string())?;
            downloads.entry(path.clone()).or_default().clone()
        };
        let _held = gate.lock().map_err(|e| e.to_string())?;
        if path.exists() {
            return Ok(path);
        }
        let response = self
            .agent
            .get(url)
            .call()
            .map_err(|e| format!("AHN6 {name}: {e}"))?;
        let mut bytes = Vec::new();
        response
            .into_reader()
            .take(64 * 1024 * 1024)
            .read_to_end(&mut bytes)
            .map_err(|e| format!("AHN6 {name}: {e}"))?;
        if !(bytes.starts_with(b"II*\0") || bytes.starts_with(b"MM\0*")) {
            return Err(format!("AHN6 {name}: not a TIFF"));
        }
        write_atomic(&path, &bytes);
        tracing::info!(file = name, bytes = bytes.len(), "AHN6 tile downloaded");
        Ok(path)
    }

    fn chunk(&self, path: &Path, index: u32) -> Result<Arc<Chunk>, String> {
        let key = (path.to_path_buf(), index);
        if let Some(c) = self.chunks.lock().map_err(|e| e.to_string())?.0.get(&key) {
            return Ok(c.clone());
        }
        let file = std::fs::File::open(path).map_err(|e| e.to_string())?;
        let mut decoder = tiff::decoder::Decoder::new(std::io::BufReader::new(file))
            .map_err(|e| format!("{}: {e}", path.display()))?;
        let (w, h) = decoder.chunk_data_dimensions(index);
        let tiff::decoder::DecodingResult::F32(data) = decoder
            .read_chunk(index)
            .map_err(|e| format!("{}: {e}", path.display()))?
        else {
            return Err(format!("{}: not float32", path.display()));
        };
        let chunk = Arc::new(Chunk {
            w: w as usize,
            h: h as usize,
            data,
        });
        let mut guard = self.chunks.lock().map_err(|e| e.to_string())?;
        let (map, order) = &mut *guard;
        map.insert(key.clone(), chunk.clone());
        order.push_back(key);
        while order.len() > CHUNK_CACHE {
            if let Some(old) = order.pop_front() {
                map.remove(&old);
            }
        }
        Ok(chunk)
    }
}

/// The sheet named `{x}_{y}` in an index answer.
fn sheet_from_index(body: &serde_json::Value, x: i64, y: i64) -> Option<Sheet> {
    let want = format!("{x}_{y}");
    let features = body.get("features")?.as_array()?;
    let props = features
        .iter()
        .filter_map(|f| f.get("properties"))
        .find(|p| p.get("AHN").and_then(|v| v.as_str()) == Some(want.as_str()))?;
    let get = |k: &str| props.get(k).and_then(|v| v.as_str()).map(str::to_string);
    Some(Sheet {
        dtm: get("Maaiveldmodel (DTM) ½m")?,
        dsm: get("Oppervlaktemodel (DSM) ½m")?,
        dtm5: get("Maaiveldmodel (DTM) 5m")?,
        dsm5: get("Oppervlaktemodel (DSM) 5m")?,
    })
}

fn write_atomic(path: &Path, bytes: &[u8]) {
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    static N: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let n = N.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let tmp = path.with_extension(format!("{}.{n}.tmp", std::process::id()));
    if std::fs::write(&tmp, bytes).is_ok() {
        let _ = std::fs::rename(&tmp, path);
    }
}

/// The sheet's south-west corner under an RD point.
fn sheet_of(x: f64, y: f64) -> (i64, i64) {
    (
        ((x / SHEET_M).floor() * SHEET_M) as i64,
        ((y / SHEET_M).floor() * SHEET_M) as i64,
    )
}

impl HeightSource for Ahn6 {
    fn raster(&self, min: [f64; 2], max: [f64; 2], n: usize) -> Result<Vec<f32>, String> {
        self.raster_of(Model::Terrain, min, max, n)
    }

    /// `n × n` over the box, north row first, **pixel-edge** semantics as PDOK's
    /// `GetCoverage` answers: sample `(col, row)` is the value at the middle of
    /// that pixel of the box, the nearest source pixel to it.
    fn raster_of(
        &self,
        model: Model,
        min: [f64; 2],
        max: [f64; 2],
        n: usize,
    ) -> Result<Vec<f32>, String> {
        let n = n.max(1);
        let (dx, dy) = ((max[0] - min[0]) / n as f64, (max[1] - min[1]) / n as f64);
        let coarse = dx.min(dy) >= COARSE_PIXEL_M;
        let resolution = if coarse { 5.0 } else { 0.5 };

        // Which sheets the box covers, and which of them AHN6 has. An index that
        // cannot be asked sends the whole request to the fallback: a half-AHN6
        // answer whose seam depends on which index call timed out is worse than a
        // consistently older one.
        let (sx0, sy0) = sheet_of(min[0], min[1]);
        let (sx1, sy1) = sheet_of(max[0] - 1e-6, max[1] - 1e-6);
        let mut sheets: HashMap<(i64, i64), Option<Sheet>> = HashMap::new();
        let mut y = sy0;
        while y <= sy1 {
            let mut x = sx0;
            while x <= sx1 {
                match self.sheet(x, y) {
                    Ok(s) => {
                        sheets.insert((x, y), s);
                    }
                    Err(e) => {
                        tracing::warn!(error = %e, "AHN6 index unavailable; using AHN4");
                        return self.fallback.raster_of(model, min, max, n);
                    }
                }
                x += SHEET_M as i64;
            }
            y += SHEET_M as i64;
        }
        if sheets.values().all(Option::is_none) {
            return self.fallback.raster_of(model, min, max, n);
        }
        let older = if sheets.values().any(Option::is_none) {
            Some(self.fallback.raster_of(model, min, max, n)?)
        } else {
            None
        };

        let mut files: HashMap<(i64, i64), PathBuf> = HashMap::new();
        for (&key, sheet) in &sheets {
            if let Some(sheet) = sheet {
                files.insert(key, self.raster_file(sheet.url(model, coarse))?);
            }
        }

        let px_per_sheet = (SHEET_M / resolution).round() as usize;
        let mut out = vec![f32::NAN; n * n];
        let mut held: HashMap<(i64, i64, u32), Arc<Chunk>> = HashMap::new();
        let mut chunk_edge: Option<(usize, usize)> = None;
        for row in 0..n {
            let ry = max[1] - (row as f64 + 0.5) * dy;
            for col in 0..n {
                let rx = min[0] + (col as f64 + 0.5) * dx;
                let key = sheet_of(rx, ry);
                let Some(path) = files.get(&key) else {
                    if let Some(older) = &older {
                        out[row * n + col] = older[row * n + col];
                    }
                    continue;
                };
                let c = (((rx - key.0 as f64) / resolution) as usize).min(px_per_sheet - 1);
                let r =
                    (((key.1 as f64 + SHEET_M - ry) / resolution) as usize).min(px_per_sheet - 1);
                let (cw, ch) = match chunk_edge {
                    Some(e) => e,
                    None => {
                        let e = chunk_size(path)?;
                        chunk_edge = Some(e);
                        e
                    }
                };
                let across = px_per_sheet.div_ceil(cw);
                let index = ((r / ch) * across + c / cw) as u32;
                let chunk = match held.get(&(key.0, key.1, index)) {
                    Some(c) => c.clone(),
                    None => {
                        let c = self.chunk(path, index)?;
                        held.insert((key.0, key.1, index), c.clone());
                        c
                    }
                };
                let (lc, lr) = (c % cw, r % ch);
                if lc < chunk.w && lr < chunk.h {
                    let v = chunk.data[lr * chunk.w + lc];
                    if v.is_finite() && v > NODATA_BELOW {
                        out[row * n + col] = v;
                    }
                }
            }
        }
        Ok(out)
    }
}

/// The block size a raster is tiled in. Every AHN6 raster of one product shares
/// it, so it is read once per request.
fn chunk_size(path: &Path) -> Result<(usize, usize), String> {
    let file = std::fs::File::open(path).map_err(|e| e.to_string())?;
    let decoder = tiff::decoder::Decoder::new(std::io::BufReader::new(file))
        .map_err(|e| format!("{}: {e}", path.display()))?;
    let (w, h) = decoder.chunk_dimensions();
    Ok((w as usize, h as usize))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_sheet_is_found_by_its_name_among_its_neighbours() {
        let body = serde_json::json!({"features": [
            {"properties": {"AHN": "233000_580000",
                "Maaiveldmodel (DTM) ½m": "a", "Oppervlaktemodel (DSM) ½m": "b",
                "Maaiveldmodel (DTM) 5m": "c", "Oppervlaktemodel (DSM) 5m": "d"}},
            {"properties": {"AHN": "234000_580000",
                "Maaiveldmodel (DTM) ½m": "M", "Oppervlaktemodel (DSM) ½m": "R",
                "Maaiveldmodel (DTM) 5m": "M5", "Oppervlaktemodel (DSM) 5m": "R5"}}
        ]});
        let s = sheet_from_index(&body, 234_000, 580_000).unwrap();
        assert_eq!(s.url(Model::Terrain, false), "M");
        assert_eq!(s.url(Model::Surface, true), "R5");
        assert_eq!(sheet_from_index(&body, 235_000, 580_000), None);
    }

    #[test]
    fn a_point_belongs_to_the_sheet_named_by_its_south_west_corner() {
        assert_eq!(sheet_of(234_999.9, 580_000.0), (234_000, 580_000));
        assert_eq!(sheet_of(235_000.0, 580_999.9), (235_000, 580_000));
    }

    /// Where AHN6 has no sheet the older survey answers, sample by sample, and
    /// where it has none at all nothing is downloaded.
    #[test]
    fn a_box_with_no_ahn6_is_the_fallback_whole() {
        struct Flat;
        impl HeightSource for Flat {
            fn raster(&self, _: [f64; 2], _: [f64; 2], n: usize) -> Result<Vec<f32>, String> {
                Ok(vec![1.5; n * n])
            }
        }
        let dir = tempfile::tempdir().unwrap();
        let ahn6 = Ahn6::new(dir.path(), Arc::new(Flat));
        // Pretend the index was asked and said no.
        ahn6.remember(100_000, 400_000, None);
        let r = ahn6
            .raster_of(
                Model::Terrain,
                [100_100.0, 400_100.0],
                [100_200.0, 400_200.0],
                4,
            )
            .unwrap();
        assert_eq!(r, vec![1.5; 16]);
        assert!(!dir.path().join("ahn6/tiles").exists());
    }

    /// Against the live tile over the ring's covered tunnel: AHN6 reads the lid.
    /// Network and a 9 MB download, so not in the default run.
    #[test]
    #[ignore = "network"]
    fn the_ring_is_roofed_in_ahn6() {
        struct Never;
        impl HeightSource for Never {
            fn raster(&self, _: [f64; 2], _: [f64; 2], _: usize) -> Result<Vec<f32>, String> {
                Err("no fallback in this test".into())
            }
        }
        let dir = tempfile::tempdir().unwrap();
        let ahn6 = Ahn6::new(dir.path(), Arc::new(Never));
        // 20 m around a point well inside the main covered stretch (BGT
        // tunneldeel L0002.3e70…), in the 234000_580000 sheet.
        let c = [234_876.0, 580_924.0];
        let r = ahn6
            .raster_of(
                Model::Terrain,
                [c[0] - 10.0, c[1] - 10.0],
                [c[0] + 10.0, c[1] + 10.0],
                20,
            )
            .unwrap();
        let mut v: Vec<f32> = r.into_iter().filter(|v| v.is_finite()).collect();
        v.sort_by(f32::total_cmp);
        let median = v[v.len() / 2];
        eprintln!("AHN6 over the covered ring: median {median:.2} m NAP");
        assert!(median > 0.0, "median {median} m NAP: still the trench");
    }

    /// **The sampler reads the sheet the right way up.** Over ground that did not
    /// change between the flights — a stretch of the Diepenring, whose quay walls
    /// put metres of relief a few pixels apart — AHN6 through this module and
    /// AHN4 through PDOK must agree sample for sample. A flipped row order or a
    /// half-sheet offset puts the water where the quay is and fails by metres.
    #[test]
    #[ignore = "network"]
    fn ahn6_agrees_with_ahn4_where_nothing_changed() {
        let dir = tempfile::tempdir().unwrap();
        let pdok = Arc::new(crate::terrain::Pdok::new());
        let ahn6 = Ahn6::new(dir.path(), pdok.clone());
        let c = [234_087.0, 582_282.0];
        let (min, max, n) = ([c[0] - 60.0, c[1] - 60.0], [c[0] + 60.0, c[1] + 60.0], 120);
        let new = ahn6.raster_of(Model::Terrain, min, max, n).unwrap();
        let old = pdok.raster_of(Model::Terrain, min, max, n).unwrap();
        let mut diffs: Vec<f32> = new
            .iter()
            .zip(&old)
            .filter(|(a, b)| a.is_finite() && b.is_finite())
            .map(|(a, b)| (a - b).abs())
            .collect();
        assert!(
            diffs.len() > n * n / 4,
            "only {} common samples",
            diffs.len()
        );
        diffs.sort_by(f32::total_cmp);
        let (median, p90) = (diffs[diffs.len() / 2], diffs[diffs.len() * 9 / 10]);
        let spread = {
            let mut o: Vec<f32> = old.iter().copied().filter(|v| v.is_finite()).collect();
            o.sort_by(f32::total_cmp);
            o[o.len() * 95 / 100] - o[o.len() / 20]
        };
        eprintln!("AHN6 − AHN4: median {median:.3} m, p90 {p90:.3} m over {spread:.2} m of relief");
        assert!(spread > 1.0, "the box has no relief to test against");
        assert!(median < 0.15 && p90 < 0.5, "median {median}, p90 {p90}");
    }
}
