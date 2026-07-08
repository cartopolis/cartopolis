use crate::resources::OsmTileUrlTemplate;
use bevy::prelude::*;
use bevy::render::render_asset::RenderAssetUsages;
use bevy_image::{Image, ImageSampler};
use std::collections::HashMap;
use std::io::Read;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
pub use vibe_core::TileKey;

/// Identifying User-Agent — OSM's tile policy blocks requests without a real one.
const TILE_USER_AGENT: &str = "viberfox/0.1 (OSM map client; https://github.com/garage44/viberfox)";

/// Resource for managing OSM tile loading and caching
#[derive(Resource)]
pub struct TileCache {
    /// Cache of loaded tile handles
    pub handles: Arc<Mutex<HashMap<TileKey, Handle<Image>>>>,
    /// Cache of loading states (tiles currently being loaded)
    pub loading: Arc<Mutex<HashMap<TileKey, bool>>>,
}

impl Default for TileCache {
    fn default() -> Self {
        Self {
            handles: Arc::new(Mutex::new(HashMap::new())),
            loading: Arc::new(Mutex::new(HashMap::new())),
        }
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

/// System to load OSM tiles for regions
pub fn load_region_tiles(
    mut commands: Commands,
    mut images: ResMut<Assets<Image>>,
    tile_cache: Res<TileCache>,
    tile_url: Res<OsmTileUrlTemplate>,
    region_query: Query<(Entity, &RegionTile), Without<RegionTileTexture>>,
) {
    let template = tile_url.0.lock().map(|g| g.clone()).unwrap_or_default();
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

        tracing::info!(tile = %tile_key.to_path(), "fetching OSM tile");
        match load_tile_image(&tile_key, &template) {
            Ok(bytes) => match image::load_from_memory(&bytes) {
                Ok(img) => {
                    let rgba = img.to_rgba8();
                    let size = bevy::render::render_resource::Extent3d {
                        width: rgba.width(),
                        height: rgba.height(),
                        depth_or_array_layers: 1,
                    };

                    let mut bevy_image = Image::new(
                        size,
                        bevy::render::render_resource::TextureDimension::D2,
                        rgba.into_raw(),
                        bevy::render::render_resource::TextureFormat::Rgba8UnormSrgb,
                        RenderAssetUsages::default(),
                    );

                    bevy_image.sampler = ImageSampler::linear();

                    let handle = images.add(bevy_image);

                    {
                        let mut handles = tile_cache.handles.lock().unwrap();
                        handles.insert(tile_key.clone(), handle.clone());
                    }

                    commands.entity(entity).insert(RegionTileTexture {
                        handle: handle.clone(),
                    });

                    {
                        let mut loading = tile_cache.loading.lock().unwrap();
                        loading.remove(&tile_key);
                    }

                    tracing::info!(tile = %tile_key.to_path(), "tile loaded and applied");
                }
                Err(e) => {
                    tracing::error!(tile = %tile_key.to_path(), error = %e, "failed to decode tile image");
                    let mut loading = tile_cache.loading.lock().unwrap();
                    loading.remove(&tile_key);
                }
            },
            Err(e) => {
                tracing::error!(tile = %tile_key.to_path(), error = %e, "failed to fetch OSM tile");
                let mut loading = tile_cache.loading.lock().unwrap();
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
