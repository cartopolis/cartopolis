use crate::resources::OsmTileUrlTemplate;
use bevy::prelude::*;
use bevy::asset::RenderAssetUsages;
use bevy_image::{Image, ImageSampler};
use std::collections::HashMap;
use std::io::Read;
use std::path::PathBuf;
use std::sync::mpsc::{Receiver, Sender};
use std::sync::{Arc, Mutex};
pub use vibe_core::TileKey;

/// Identifying User-Agent — OSM's tile policy blocks requests without a real one.
const TILE_USER_AGENT: &str = "viberfox/0.1 (OSM map client; https://github.com/garage44/viberfox)";

/// Number of background tile-fetch workers (matches the globe's fetch pool).
const TILE_WORKERS: usize = 3;

/// Decoded, mipmapped tile image handed back from a worker thread. Decoding and
/// mip-chain generation happen off the main thread; only the cheap `Image`/`Assets`
/// insertion stays on it. `data` holds the full packed mip chain.
struct TileImageData {
    data: Vec<u8>,
    width: u32,
    height: u32,
    mip_count: u32,
}

/// Resource for managing OSM tile loading and caching
#[derive(Resource)]
pub struct TileCache {
    /// Cache of loaded tile handles
    pub handles: Arc<Mutex<HashMap<TileKey, Handle<Image>>>>,
    /// Cache of loading states (tiles currently being loaded)
    pub loading: Arc<Mutex<HashMap<TileKey, bool>>>,
    /// Job queue to the background fetch workers (created on first use).
    jobs: Option<Sender<(TileKey, String)>>,
    /// Completed fetches drained on the main thread (`None` = fetch/decode failed).
    results: Arc<Mutex<Vec<(TileKey, Option<TileImageData>)>>>,
}

impl Default for TileCache {
    fn default() -> Self {
        Self {
            handles: Arc::new(Mutex::new(HashMap::new())),
            loading: Arc::new(Mutex::new(HashMap::new())),
            jobs: None,
            results: Arc::new(Mutex::new(Vec::new())),
        }
    }
}

impl TileCache {
    /// Lazily spawn the fetch worker pool and return the job sender. Workers pull
    /// `(tile, url-template)` jobs, do the blocking network/disk fetch + PNG decode
    /// off the main thread, and push results back for the main thread to upload.
    fn jobs_sender(&mut self) -> Sender<(TileKey, String)> {
        if let Some(tx) = &self.jobs {
            return tx.clone();
        }
        let (tx, rx) = std::sync::mpsc::channel::<(TileKey, String)>();
        let rx: Arc<Mutex<Receiver<(TileKey, String)>>> = Arc::new(Mutex::new(rx));
        for _ in 0..TILE_WORKERS {
            let rx = rx.clone();
            let results = self.results.clone();
            std::thread::spawn(move || loop {
                let Ok((key, template)) = ({
                    let Ok(guard) = rx.lock() else { break };
                    guard.recv()
                }) else {
                    break;
                };
                let outcome = match load_tile_rgba(&key, &template) {
                    Ok(rgba) => {
                        let (width, height) = (rgba.width(), rgba.height());
                        let (data, mip_count) = crate::systems::rendering::build_mip_chain(
                            width,
                            height,
                            rgba.into_raw(),
                        );
                        Some(TileImageData {
                            data,
                            width,
                            height,
                            mip_count,
                        })
                    }
                    Err(e) => {
                        tracing::error!(tile = %key.to_path(), error = %e, "failed to load map tile");
                        None
                    }
                };
                if let Ok(mut r) = results.lock() {
                    r.push((key, outcome));
                }
            });
        }
        self.jobs = Some(tx.clone());
        tx
    }
}

/// Component to mark regions that need tile textures
#[derive(Component)]
#[allow(dead_code)] // lod_level kept for ADR-004 planned LOD
pub struct RegionTile {
    pub tile_key: TileKey,
    pub lod_level: u32, // 0 = high-res (2x2), 1 = medium-res (1x1), 2 = low-res (1x1)
}

/// Build tile URL from template (`{z}`, `{x}`, `{y}`) or the default vector source
/// ([`vibe_core::world::DEFAULT_TILE_URL_TEMPLATE`], ADR-035). An empty template
/// only reaches here offline / under `--shot`; solo and split mode fill it from the
/// sim handshake first.
#[must_use]
pub fn format_osm_tile_url(template: &str, key: &TileKey) -> String {
    let template = if template.is_empty() {
        vibe_core::world::DEFAULT_TILE_URL_TEMPLATE
    } else {
        template
    };
    template
        .replace("{z}", &key.z.to_string())
        .replace("{x}", &key.x.to_string())
        .replace("{y}", &key.y.to_string())
}

/// What a tile endpoint serves. Raster payloads are decoded by the `image` crate;
/// vector payloads are drawn by [`crate::systems::vector_tiles`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TileSourceKind {
    Raster,
    Vector,
}

/// Classify a URL template by the tail after its `{y}` placeholder: `.png`/`.webp`
/// → raster, nothing at all → vector (VersaTiles serves `/tiles/osm/{z}/{x}/{y}`
/// with no extension). `VIBE_TILE_FORMAT=vector|raster` overrides.
///
/// This has to be decided *before* the fetch rather than sniffed from the bytes,
/// because a vector source caps out at `MAX_SOURCE_ZOOM` and the deeper tiles the
/// map streamer asks for must be redirected to an ancestor — which changes which
/// URL we request. The byte sniff in `looks_like_raster` still runs afterwards as
/// a backstop, so a misclassified source degrades to "no overzoom" rather than to
/// a decode failure.
#[must_use]
pub fn tile_source_kind(template: &str) -> TileSourceKind {
    if let Ok(v) = std::env::var("VIBE_TILE_FORMAT") {
        match v.to_ascii_lowercase().as_str() {
            "vector" | "mvt" | "pbf" => return TileSourceKind::Vector,
            "raster" | "png" | "webp" | "jpeg" => return TileSourceKind::Raster,
            _ => {}
        }
    }
    match template.rsplit_once("{y}") {
        // An extension right after `{y}` means pre-rendered pixels.
        Some((_, tail)) => {
            let tail = tail.split('?').next().unwrap_or("");
            if tail.starts_with('.') {
                TileSourceKind::Raster
            } else {
                TileSourceKind::Vector
            }
        }
        // Empty/!templated → the built-in default, which is now the VersaTiles
        // vector source (`DEFAULT_TILE_URL_TEMPLATE`), so classify it as such.
        None => TileSourceKind::Vector,
    }
}

/// On-disk cache path for a tile, under the app's `cache/` folder (persists across
/// runs so revisits/LOD bursts don't re-hit the tile server and trip rate limits).
/// The extension tracks the source kind so a raster and a vector cache can coexist
/// (and so switching sources doesn't serve MVT bytes to the PNG decoder).
fn tile_cache_path(key: &TileKey, kind: TileSourceKind) -> PathBuf {
    let ext = match kind {
        TileSourceKind::Raster => "png",
        TileSourceKind::Vector => "mvt",
    };
    PathBuf::from("cache")
        .join("tiles")
        .join(key.z.to_string())
        .join(key.x.to_string())
        .join(format!("{}.{ext}", key.y))
}

/// Fetch a single tile's raw bytes, using the on-disk cache first and falling back
/// to the network (which is then cached). The payload may be a raster image or an
/// MVT body — see [`load_tile_rgba`] for the decoded form.
pub fn load_tile_image(
    key: &TileKey,
    template: &str,
) -> Result<Vec<u8>, Box<dyn std::error::Error>> {
    let kind = tile_source_kind(template);
    let path = tile_cache_path(key, kind);
    if let Ok(bytes) = std::fs::read(&path) {
        if !bytes.is_empty() {
            return Ok(bytes);
        }
    }

    let url = format_osm_tile_url(template, key);
    let response = ureq::get(&url)
        .set("User-Agent", TILE_USER_AGENT)
        .call()?;
    let mut bytes = Vec::new();
    response.into_reader().read_to_end(&mut bytes)?;

    // Best-effort write-through to the cache.
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    let _ = std::fs::write(&path, &bytes);

    Ok(bytes)
}

/// Fetch a tile and hand back finished RGBA pixels, whatever the source serves.
///
/// This is the single seam between "where map pixels come from" and everyone who
/// wants them (the region loader here, `map_stream`, and `globe`). For a raster
/// source it is a fetch plus an `image` decode, exactly as before. For a vector
/// source it also resolves overzoom — a request deeper than
/// [`vector_tiles::MAX_SOURCE_ZOOM`] fetches the z14 ancestor and renders the
/// matching sub-rectangle, so the streamer's z15–z17 requests keep working against
/// a source that stops at z14.
pub fn load_tile_rgba(
    key: &TileKey,
    template: &str,
) -> Result<image::RgbaImage, Box<dyn std::error::Error>> {
    use crate::systems::vector_tiles;

    if tile_source_kind(template) == TileSourceKind::Raster {
        let bytes = load_tile_image(key, template)?;
        return Ok(image::load_from_memory(&bytes)?.to_rgba8());
    }

    let ((z, x, y), sub) = vector_tiles::SubTile::resolve(key.z, key.x, key.y);
    let source = TileKey::new(x, y, z);
    let bytes = load_tile_image(&source, template)?;

    // Backstop for a misclassified template (see `tile_source_kind`): if the body
    // is actually an image, decode it and skip the overzoom entirely.
    if vector_tiles::looks_like_raster(&bytes) {
        return Ok(image::load_from_memory(&bytes)?.to_rgba8());
    }

    let rgba = vector_tiles::render_mvt(&bytes, sub)?;
    image::RgbaImage::from_raw(vector_tiles::TILE_PX, vector_tiles::TILE_PX, rgba)
        .ok_or_else(|| "rendered tile had the wrong buffer size".into())
}

/// A globe tile's RGBA **with the ocean mask packed into the alpha channel**
/// (255 = water), and whether that mask means anything.
///
/// Alpha is free real estate here: the globe's tile materials are opaque, so the
/// channel is otherwise wasted, and riding along in the same texture costs no
/// extra binding, no extra upload and no extra memory — which matters because the
/// globe holds a whole quadtree of these. The mip chain averages the mask along
/// with the colour, so a coastline softens with distance instead of aliasing.
///
/// Two ways to get the mask, because the basemap can be either kind of source:
///
/// * **Vector** — rasterise the water layers ([`vector_tiles::ocean_mask`]). Exact,
///   and by construction it agrees with what the local water shader calls water.
/// * **Raster** — no geometry to ask, so classify by colour. Every map style paints
///   water markedly bluer than its land (and on satellite imagery the sea really
///   is), so `blue − red` separates them where a hue test would trip over the
///   green end. A misread here costs a little gloss in the wrong place, not a
///   wrong shape, which is why the heuristic is acceptable at all.
pub fn load_globe_tile_rgba(
    key: &TileKey,
    template: &str,
) -> Result<(image::RgbaImage, bool), Box<dyn std::error::Error>> {
    use crate::systems::vector_tiles;

    if tile_source_kind(template) == TileSourceKind::Raster {
        let bytes = load_tile_image(key, template)?;
        let mut rgba = image::load_from_memory(&bytes)?.to_rgba8();
        paint_water_alpha_by_colour(&mut rgba);
        return Ok((rgba, true));
    }

    let ((z, x, y), sub) = vector_tiles::SubTile::resolve(key.z, key.x, key.y);
    let source = TileKey::new(x, y, z);
    let bytes = load_tile_image(&source, template)?;

    if vector_tiles::looks_like_raster(&bytes) {
        let mut rgba = image::load_from_memory(&bytes)?.to_rgba8();
        paint_water_alpha_by_colour(&mut rgba);
        return Ok((rgba, true));
    }

    let pixels = vector_tiles::render_mvt(&bytes, sub)?;
    let mut rgba = image::RgbaImage::from_raw(vector_tiles::TILE_PX, vector_tiles::TILE_PX, pixels)
        .ok_or_else(|| "rendered tile had the wrong buffer size".to_string())?;
    match vector_tiles::ocean_mask(&bytes, sub) {
        Ok(mask) if mask.len() == (rgba.width() * rgba.height()) as usize => {
            for (px, m) in rgba.pixels_mut().zip(mask) {
                px.0[3] = m;
            }
            Ok((rgba, true))
        }
        Ok(_) => Ok((rgba, false)),
        Err(e) => {
            tracing::warn!(error = %e, "ocean mask failed; globe tile stays matte");
            Ok((rgba, false))
        }
    }
}

/// Classify water by colour into the alpha channel, for basemaps that arrive as
/// pixels. Soft-edged (a ramp, not a threshold) so coastlines blend.
fn paint_water_alpha_by_colour(rgba: &mut image::RgbaImage) {
    for px in rgba.pixels_mut() {
        let (r, b) = (f32::from(px.0[0]), f32::from(px.0[2]));
        let water = ((b - r - WATER_BLUE_BIAS) / WATER_BLUE_RANGE).clamp(0.0, 1.0);
        px.0[3] = (water * 255.0) as u8;
    }
}

/// How much bluer than red a texel must be before it counts as water at all.
const WATER_BLUE_BIAS: f32 = 10.0;
/// The span over which that excess ramps up to fully water.
const WATER_BLUE_RANGE: f32 = 30.0;

/// System to load OSM tiles for regions.
///
/// The blocking network/disk fetch + PNG decode runs on a background worker pool
/// (see [`TileCache::jobs_sender`]); this system only drains completed results and
/// enqueues new work, so it never stalls the render loop on a tile fetch.
pub fn load_region_tiles(
    mut commands: Commands,
    mut images: ResMut<Assets<Image>>,
    mut tile_cache: ResMut<TileCache>,
    tile_url: Res<OsmTileUrlTemplate>,
    region_query: Query<(Entity, &RegionTile), Without<RegionTileTexture>>,
) {
    // 1. Drain finished fetches: upload the texture and publish its handle.
    let finished: Vec<(TileKey, Option<TileImageData>)> = tile_cache
        .results
        .lock()
        .map(|mut r| std::mem::take(&mut *r))
        .unwrap_or_default();
    for (tile_key, outcome) in finished {
        match outcome {
            Some(tile) => {
                let bevy_image = crate::systems::rendering::image_from_mip_chain(
                    tile.width,
                    tile.height,
                    tile.data,
                    tile.mip_count,
                    bevy::render::render_resource::TextureFormat::Rgba8UnormSrgb,
                    RenderAssetUsages::default(),
                    ImageSampler::linear(),
                );
                let handle = images.add(bevy_image);
                if let Ok(mut handles) = tile_cache.handles.lock() {
                    handles.insert(tile_key.clone(), handle);
                }
                tracing::info!(tile = %tile_key.to_path(), "tile loaded and applied");
            }
            None => {
                // Fetch/decode failed — already logged by the worker. Clearing the
                // loading flag below lets it be retried on a later frame.
            }
        }
        if let Ok(mut loading) = tile_cache.loading.lock() {
            loading.remove(&tile_key);
        }
    }

    // 2. Attach ready textures / enqueue missing ones for the background workers.
    let template = tile_url.0.lock().map(|g| g.clone()).unwrap_or_default();
    let mut sender: Option<Sender<(TileKey, String)>> = None;
    for (entity, region_tile) in region_query.iter() {
        let tile_key = region_tile.tile_key.clone();

        {
            let handles = tile_cache.handles.lock().unwrap();
            if let Some(handle) = handles.get(&tile_key) {
                commands.entity(entity).insert(RegionTileTexture {
                    handle: handle.clone(),
                });
                continue;
            }
        }

        {
            let mut loading = tile_cache.loading.lock().unwrap();
            if loading.contains_key(&tile_key) {
                continue;
            }
            loading.insert(tile_key.clone(), true);
        }

        let tx = sender.get_or_insert_with(|| tile_cache.jobs_sender());
        tracing::info!(tile = %tile_key.to_path(), "queued OSM tile fetch");
        if tx.send((tile_key.clone(), template.clone())).is_err() {
            // Worker pool gone (shouldn't happen) — undo the loading flag so a
            // later frame can retry.
            if let Ok(mut loading) = tile_cache.loading.lock() {
                loading.remove(&tile_key);
            }
        }
    }
}

/// Component to store the tile texture handle for a region
#[derive(Component)]
pub struct RegionTileTexture {
    pub handle: Handle<Image>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn classifies_raster_and_vector_templates() {
        // Raster: an extension right after {y}.
        for t in [
            "https://tile.openstreetmap.org/{z}/{x}/{y}.png",
            "https://tiles.versatiles.org/tiles/satellite/{z}/{x}/{y}.webp",
            "https://example.org/{z}/{x}/{y}.jpg?key=abc",
        ] {
            assert_eq!(tile_source_kind(t), TileSourceKind::Raster, "{t}");
        }

        // Vector: nothing after {y}, query string or not.
        for t in [
            "https://tiles.versatiles.org/tiles/osm/{z}/{x}/{y}",
            "https://tiles.garage44.eu/tiles/osm/{z}/{x}/{y}?v=1",
        ] {
            assert_eq!(tile_source_kind(t), TileSourceKind::Vector, "{t}");
        }

        // An empty template means the built-in default, which is now vector.
        assert_eq!(tile_source_kind(""), TileSourceKind::Vector);
    }

    #[test]
    fn empty_template_resolves_to_the_default_vector_source() {
        let key = TileKey::new(8490, 5319, 14);
        assert_eq!(
            format_osm_tile_url("", &key),
            "https://tiles.versatiles.org/tiles/osm/14/8490/5319",
            "an empty template must fall back to the default vector source, not raster OSM"
        );
    }

    #[test]
    fn cache_paths_separate_by_kind() {
        let key = TileKey::new(8490, 5319, 14);
        let raster = tile_cache_path(&key, TileSourceKind::Raster);
        let vector = tile_cache_path(&key, TileSourceKind::Vector);
        assert_ne!(raster, vector, "a source switch must not reuse cached bytes");
        assert!(raster.to_string_lossy().ends_with("5319.png"));
        assert!(vector.to_string_lossy().ends_with("5319.mvt"));
    }
}
