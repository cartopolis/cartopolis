use crate::resources::OsmTileUrlTemplate;
use bevy::prelude::*;
use bevy::asset::RenderAssetUsages;
use bevy_image::{Image, ImageSampler};
use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use bevy::tasks::IoTaskPool;

use crate::platform::work::Dispatcher;
pub use vibe_core::TileKey;

/// Number of concurrent tile fetch+decode tasks (was the background worker count).
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
    /// Frame-gated fetch+decode dispatcher (TileKey, url-template), bounded to
    /// `TILE_WORKERS` concurrent tasks — replaces the worker pool + channel.
    dispatch: Dispatcher<(TileKey, String)>,
    /// Completed fetches drained on the main thread (`None` = fetch/decode failed).
    results: Arc<Mutex<Vec<(TileKey, Option<TileImageData>)>>>,
}

impl Default for TileCache {
    fn default() -> Self {
        Self {
            handles: Arc::new(Mutex::new(HashMap::new())),
            loading: Arc::new(Mutex::new(HashMap::new())),
            dispatch: Dispatcher::new(TILE_WORKERS, 0.0),
            results: Arc::new(Mutex::new(Vec::new())),
        }
    }
}

/// Spawn one tile fetch+decode task (ADR-033 seam): await the fetch on the
/// `IoTaskPool`, then hand the decode + mip chain to [`crate::platform::workers`]
/// and push the finished pixels back for the render loop to upload.
///
/// The split is the same one `map_stream::spawn_map_tile` makes and for the same
/// reason: on wasm `IoTaskPool` *is* the main thread, so awaiting the fetch there
/// is free but decoding there is not.
fn spawn_tile_task(
    key: TileKey,
    template: String,
    results: Arc<Mutex<Vec<(TileKey, Option<TileImageData>)>>>,
) {
    IoTaskPool::get()
        .spawn(async move {
            match fetch_tile_payload(&key, &template).await {
                Ok(payload) => {
                    crate::platform::workers::submit(Box::new(move || {
                        let outcome = match payload.decode() {
                            Ok(rgba) => {
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
                                tracing::error!(tile = %key.to_path(), error = %e, "map tile decode failed");
                                None
                            }
                        };
                        if let Ok(mut r) = results.lock() {
                            r.push((key, outcome));
                        }
                    }));
                }
                Err(e) => {
                    tracing::error!(tile = %key.to_path(), error = %e, "failed to load map tile");
                    if let Ok(mut r) = results.lock() {
                        r.push((key, None));
                    }
                }
            }
        })
        .detach();
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
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
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
pub(crate) fn tile_cache_key(key: &TileKey, kind: TileSourceKind) -> String {
    let ext = match kind {
        TileSourceKind::Raster => "png",
        TileSourceKind::Vector => "mvt",
    };
    format!("tiles/{}/{}/{}.{ext}", key.z, key.x, key.y)
}

/// Fetch a single tile's raw bytes. A thin wrapper over
/// [`tile_source::tile_bytes`], which owns the in-memory byte cache, the
/// single-flight table and — behind those — the on-disk cache and the network.
/// The payload may be a raster image or an MVT body — see [`TilePayload::decode`]
/// for the decoded form.
///
/// `Arc` rather than `Vec`: several consumers share one body (the overzoom
/// pyramid alone collapses 179 requests per anchor onto a few dozen z14 tiles),
/// and handing each of them its own ~500 KB copy would give back most of what the
/// cache saves.
pub async fn load_tile_image(key: &TileKey, template: &str) -> Result<Arc<Vec<u8>>, String> {
    crate::systems::tile_source::tile_bytes(key, template).await
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
/// A fetched tile body, still undecoded, tagged with what it turned out to be.
///
/// This is the seam between the two halves of tile loading, and they want
/// different machinery. Fetching is I/O the browser already runs off the main
/// thread; decoding is arithmetic that needs a core of its own (ADR-033). Holding
/// only owned bytes plus a `Copy` sub-tile keeps the payload `Send`, so
/// [`TilePayload::decode`] can be handed to [`platform::workers`] verbatim.
///
/// [`platform::workers`]: crate::platform::workers
pub enum TilePayload {
    /// An encoded image (PNG/JPEG/WebP) — either a raster source, or a vector
    /// template that answered with an image anyway (see `looks_like_raster`).
    Raster(Arc<Vec<u8>>),
    /// A Mapbox Vector Tile plus the sub-rectangle of it we actually want.
    Vector {
        bytes: Arc<Vec<u8>>,
        sub: crate::systems::vector_tiles::SubTile,
    },
}

impl TilePayload {
    /// The CPU half: parse/rasterise into RGBA. Pure computation — no I/O, no
    /// globals — which is what lets it run on a worker.
    pub fn decode(self) -> Result<image::RgbaImage, String> {
        use crate::systems::vector_tiles;
        match self {
            TilePayload::Raster(bytes) => image::load_from_memory(&bytes)
                .map(|img| img.to_rgba8())
                .map_err(|e| e.to_string()),
            TilePayload::Vector { bytes, sub } => {
                let tile = vector_tiles::TileData::parse(&bytes)?;
                let rgba = vector_tiles::render_mvt(&tile, sub).map_err(|e| e.to_string())?;
                image::RgbaImage::from_raw(vector_tiles::TILE_PX, vector_tiles::TILE_PX, rgba)
                    .ok_or_else(|| "rendered tile had the wrong buffer size".to_string())
            }
        }
    }
}

/// The network half: fetch the tile body and classify it, decoding nothing.
pub async fn fetch_tile_payload(key: &TileKey, template: &str) -> Result<TilePayload, String> {
    use crate::systems::vector_tiles;

    if tile_source_kind(template) == TileSourceKind::Raster {
        return Ok(TilePayload::Raster(load_tile_image(key, template).await?));
    }

    let ((z, x, y), sub) = vector_tiles::SubTile::resolve(key.z, key.x, key.y);
    let source = TileKey::new(x, y, z);
    let bytes = load_tile_image(&source, template).await?;

    // Backstop for a misclassified template (see `tile_source_kind`): if the body
    // is actually an image, decode it and skip the overzoom entirely.
    if vector_tiles::looks_like_raster(&bytes) {
        return Ok(TilePayload::Raster(bytes));
    }

    Ok(TilePayload::Vector { bytes, sub })
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
/// The network half of a globe tile: fetch its body and classify it, decoding
/// nothing. Split from [`GlobeTilePayload::decode`] for the reason every loader on
/// the ADR-033 seams is split — the fetch is awaited on the `IoTaskPool` (which on
/// wasm *is* the main thread, where awaiting a browser fetch is free) and the
/// decode goes to [`crate::platform::workers`], which is where a core is actually
/// spent. Running the rasterise + mask + mip chain inline, up to `QT_FETCH_BUDGET`
/// at a time, put all of it on the frame loop.
pub async fn fetch_globe_tile_payload(
    key: &TileKey,
    template: &str,
) -> Result<GlobeTilePayload, String> {
    use crate::systems::vector_tiles;

    if tile_source_kind(template) == TileSourceKind::Raster {
        return Ok(GlobeTilePayload::Raster(
            load_tile_image(key, template).await?,
        ));
    }

    let ((z, x, y), sub) = vector_tiles::SubTile::resolve(key.z, key.x, key.y);
    let source = TileKey::new(x, y, z);
    let bytes = load_tile_image(&source, template).await?;

    if vector_tiles::looks_like_raster(&bytes) {
        return Ok(GlobeTilePayload::Raster(bytes));
    }
    Ok(GlobeTilePayload::Vector { bytes, sub })
}

/// A fetched globe tile body, still undecoded. The globe's twin of [`TilePayload`],
/// separate because a globe tile also carries the ocean mask in its alpha.
pub enum GlobeTilePayload {
    Raster(Arc<Vec<u8>>),
    Vector {
        bytes: Arc<Vec<u8>>,
        sub: crate::systems::vector_tiles::SubTile,
    },
}

impl GlobeTilePayload {
    /// The CPU half: rasterise (or image-decode) and pack the ocean mask into
    /// alpha. Pure computation, so it runs on a worker.
    pub fn decode(self) -> Result<(image::RgbaImage, bool), String> {
        use crate::systems::vector_tiles;

        match self {
            GlobeTilePayload::Raster(bytes) => {
                let mut rgba = image::load_from_memory(&bytes)
                    .map_err(|e| e.to_string())?
                    .to_rgba8();
                paint_water_alpha_by_colour(&mut rgba);
                Ok((rgba, true))
            }
            GlobeTilePayload::Vector { bytes, sub } => {
                // One parse for both passes: the render and the mask read the same
                // layers, and each used to open its own `Reader` over this body.
                let tile = vector_tiles::TileData::parse(&bytes)?;
                let pixels = vector_tiles::render_mvt(&tile, sub).map_err(|e| e.to_string())?;
                let mut rgba = image::RgbaImage::from_raw(
                    vector_tiles::TILE_PX,
                    vector_tiles::TILE_PX,
                    pixels,
                )
                .ok_or_else(|| "rendered tile had the wrong buffer size".to_string())?;
                match vector_tiles::ocean_mask(&tile, sub) {
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
        }
    }
}

/// Classify water by colour into the alpha channel, for basemaps that arrive as
/// pixels. Soft-edged (a ramp, not a threshold) so coastlines blend.
fn paint_water_alpha_by_colour(rgba: &mut image::RgbaImage) {
    // Integer throughout, on a flat byte slice. The inputs are `u8`, so `b - r`
    // and the ramp are exact in integers and the `f32` round trip bought nothing
    // — but it did cost the vectoriser: a per-pixel widen, divide and `clamp`
    // over an `RgbaImage` pixel iterator will not lower to a SIMD loop, while
    // this does (~2.5x, and it is the whole cost of a raster globe tile).
    for px in rgba.chunks_exact_mut(4) {
        let excess = i32::from(px[2]) - i32::from(px[0]) - WATER_BLUE_BIAS;
        let ramp = excess.clamp(0, WATER_BLUE_RANGE);
        px[3] = ((ramp * 255) / WATER_BLUE_RANGE) as u8;
    }
}

/// How much bluer than red a texel must be before it counts as water at all.
const WATER_BLUE_BIAS: i32 = 10;
/// The span over which that excess ramps up to fully water.
const WATER_BLUE_RANGE: i32 = 30;

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
    time: Res<Time>,
    region_query: Query<(Entity, &RegionTile), Without<RegionTileTexture>>,
) {
    // 1. Drain finished fetches: upload the texture and publish its handle.
    let finished: Vec<(TileKey, Option<TileImageData>)> = tile_cache
        .results
        .lock()
        .map(|mut r| std::mem::take(&mut *r))
        .unwrap_or_default();
    for _ in 0..finished.len() {
        tile_cache.dispatch.completed();
    }
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
                    handles.insert(tile_key, handle);
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

    // 2. Attach ready textures / enqueue missing ones onto the dispatcher.
    let template = tile_url.0.lock().map(|g| g.clone()).unwrap_or_default();
    for (entity, region_tile) in region_query.iter() {
        let tile_key = region_tile.tile_key;

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
            loading.insert(tile_key, true);
        }

        tracing::info!(tile = %tile_key.to_path(), "queued OSM tile fetch");
        tile_cache.dispatch.queue((tile_key, template.clone()));
    }

    // 3. Dispatch queued tiles up to the concurrency cap as fetch+decode tasks.
    let now = time.elapsed_secs_f64();
    let results = tile_cache.results.clone();
    while let Some((key, template)) = tile_cache.dispatch.next(now) {
        spawn_tile_task(key, template, results.clone());
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

    /// The water ramp was rewritten from `f32` into integers so it would
    /// vectorise. Both channels are bytes, so the domain is small enough to
    /// simply check the whole of it: every `(red, blue)` pair must land on the
    /// alpha the float version produced, or coastlines move.
    #[test]
    fn integer_water_ramp_matches_the_float_one() {
        for r in 0..=255u8 {
            for b in 0..=255u8 {
                let want = {
                    let (rf, bf) = (f32::from(r), f32::from(b));
                    ((((bf - rf - 10.0) / 30.0).clamp(0.0, 1.0)) * 255.0) as u8
                };
                let mut img = image::RgbaImage::from_pixel(1, 1, image::Rgba([r, 0, b, 0]));
                paint_water_alpha_by_colour(&mut img);
                assert_eq!(img.get_pixel(0, 0).0[3], want, "red {r}, blue {b}");
            }
        }
    }

    /// The basemap's own water paint has to survive this classifier, and it is
    /// tuned against something else entirely: `palette::WATER` is matched to the
    /// tone the *shaded* vector water renders at, so the ring where the geometry
    /// stops does not show a step. Those two pulls are not aligned — the match
    /// wants it darker and greyer, this wants it blue — and only this one fails
    /// silently, as a globe that stops having oceans. Pin it.
    #[test]
    fn the_basemaps_water_paint_reads_as_fully_water() {
        let w = crate::systems::vector_tiles::palette::WATER;
        let (r, b) = (((w >> 16) & 0xFF) as u8, (w & 0xFF) as u8);
        let mut img = image::RgbaImage::from_pixel(1, 1, image::Rgba([r, 0, b, 0]));
        paint_water_alpha_by_colour(&mut img);
        assert_eq!(
            img.get_pixel(0, 0).0[3],
            255,
            "palette::WATER #{w:06X} is only blue-red={} and needs {} to read as \
             fully water; the globe's raster ocean mask fades out below that",
            i32::from(b) - i32::from(r),
            WATER_BLUE_BIAS + WATER_BLUE_RANGE
        );
    }

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
            "https://tiles.garage44.eu/tiles/osm/14/8490/5319",
            "an empty template must fall back to the default vector source, not raster OSM"
        );
    }

    #[test]
    fn cache_keys_separate_by_kind() {
        let key = TileKey::new(8490, 5319, 14);
        let raster = tile_cache_key(&key, TileSourceKind::Raster);
        let vector = tile_cache_key(&key, TileSourceKind::Vector);
        assert_ne!(raster, vector, "a source switch must not reuse cached bytes");
        assert!(raster.ends_with("5319.png"));
        assert!(vector.ends_with("5319.mvt"));
    }
}
