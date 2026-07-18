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
                let outcome = match load_tile_image(&key, &template) {
                    Ok(bytes) => match image::load_from_memory(&bytes) {
                        Ok(img) => {
                            let rgba = img.to_rgba8();
                            let (width, height) = (rgba.width(), rgba.height());
                            let (data, mip_count) =
                                crate::systems::rendering::build_mip_chain(
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
                            tracing::error!(tile = %key.to_path(), error = %e, "failed to decode tile image");
                            None
                        }
                    },
                    Err(e) => {
                        tracing::error!(tile = %key.to_path(), error = %e, "failed to fetch OSM tile");
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

/// Build tile URL from template (`{z}`, `{x}`, `{y}`) or default OSM (ADR-004 / ADR-014).
#[must_use]
pub fn format_osm_tile_url(template: &str, key: &TileKey) -> String {
    if template.is_empty() {
        return format!(
            "https://tile.openstreetmap.org/{}/{}/{}.png",
            key.z, key.x, key.y
        );
    }
    template
        .replace("{z}", &key.z.to_string())
        .replace("{x}", &key.x.to_string())
        .replace("{y}", &key.y.to_string())
}

/// On-disk cache path for a tile, under the app's `cache/` folder (persists across
/// runs so revisits/LOD bursts don't re-hit the tile server and trip rate limits).
fn tile_cache_path(key: &TileKey) -> PathBuf {
    PathBuf::from("cache")
        .join("tiles")
        .join(key.z.to_string())
        .join(key.x.to_string())
        .join(format!("{}.png", key.y))
}

/// Load a single OSM tile image, using the on-disk cache first and falling back
/// to a network fetch (which is then cached).
pub fn load_tile_image(
    key: &TileKey,
    template: &str,
) -> Result<Vec<u8>, Box<dyn std::error::Error>> {
    let path = tile_cache_path(key);
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
