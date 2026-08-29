use crate::resources::OsmTileUrlTemplate;
use bevy::asset::RenderAssetUsages;
use bevy::prelude::*;
use bevy_image::{Image, ImageSampler, ImageSamplerDescriptor};
use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use bevy::tasks::IoTaskPool;

use crate::platform::work::Dispatcher;
use crate::systems::vector_tiles;
pub use cartopolis_core::{tile_source_kind, TileKey, TileSourceKind};

/// Linear sampler with anisotropic filtering, for any texture lying in the
/// ground plane. The map is viewed at grazing angles almost all of the time,
/// and there an isotropic mip lookup keys the level off the *worst* axis —
/// blurring the far half of every frame into flat grey long before distance
/// justifies it. The generated road surface has had this since it landed
/// (`map_geometry::road_surface_image`, whose comment carries the finding);
/// the map tiles underneath it, the globe's tiles and the street-label quads
/// were still plain `linear()`, so the roads stayed legible while the ground
/// they lie on dissolved.
pub(crate) fn ground_sampler() -> ImageSampler {
    ImageSampler::Descriptor(ImageSamplerDescriptor {
        anisotropy_clamp: 8,
        ..ImageSamplerDescriptor::linear()
    })
}

/// Ground edge of a tile in metres at its own centre latitude — what
/// `render_mvt_sized` needs to convert real carriageway widths into pixels and
/// pick its style band.
fn source_ground_m(key: TileKey) -> f32 {
    use cartopolis_core::world::{tile_to_lat_lng, tile_to_meters};
    let (north, _) = tile_to_lat_lng(key.x, key.y, key.z);
    let (south, _) = tile_to_lat_lng(key.x, key.y + 1, key.z);
    tile_to_meters(key.z, (north + south) / 2.0) as f32
}

/// Number of concurrent tile fetch+decode tasks (was the background worker count).
const TILE_WORKERS: usize = 3;

/// Parsed tile bodies, shared between everything that wants the same one.
///
/// **The overzoom fan-out is the reason this exists.** Anything above
/// `MAX_SOURCE_ZOOM` is drawn by magnifying its z14 ancestor, so a z17 clipmap
/// ring resolves up to 64 distinct tiles onto one body — and each was a separate
/// worker task calling `TileData::parse`, which is a full `bytes.to_vec()` copy
/// plus a fresh prost decode of every layer it then touches. The byte cache and
/// the single-flight table already collapsed the *fetch*; nothing collapsed the
/// parse. `map_geometry`, both tile-sourced data layers, the globe and the routing
/// corridor each parsed the same bodies again on top of that.
///
/// **`Weak`, deliberately.** A fully-decoded dense z14 tile peaks at ~12.5 MiB
/// (`docs/notes/allocation-budgets.md` — the number `vector_tiles::TileData`'s doc
/// said this was blocked on), and the globe can want hundreds of tiles at once. A
/// strong cache would be a memory policy nobody had designed; weak references
/// give the whole win — every consumer alive at the same time shares one parse —
/// and cost nothing in the steady state, because an entry dies with its last
/// reader. What this is emphatically *not* is a second byte cache with a
/// different eviction rule.
static PARSED_TILES: Mutex<Option<HashMap<TileKey, std::sync::Weak<vector_tiles::TileData>>>> =
    Mutex::new(None);

/// Dead entries tolerated before the map is swept.
///
/// Sweeping on every miss would be O(n) per parse; never sweeping would leak one
/// small entry per tile ever seen. A tile key and a `Weak` are ~40 bytes, so a
/// few hundred is nothing and the sweep is rare.
const PARSED_SWEEP_AT: usize = 512;

/// Parse `bytes` as the vector tile `source`, or hand back the parse someone else
/// is already holding.
///
/// The key is the **source** tile, not the requested one: that is what makes the
/// 64 sub-tiles of one ancestor a single parse. Callers that are not overzooming
/// pass their own key, which is the same thing.
///
/// A poisoned lock is not fatal here and must not be — the cache is an
/// optimisation, and refusing to draw the map because another thread panicked
/// mid-insert would turn a slow tile into a broken one. Both arms fall back to
/// parsing directly.
pub fn parse_shared(source: TileKey, bytes: &[u8]) -> Result<Arc<vector_tiles::TileData>, String> {
    let mut guard = match PARSED_TILES.lock() {
        Ok(guard) => guard,
        Err(poisoned) => poisoned.into_inner(),
    };
    let map = guard.get_or_insert_with(HashMap::new);
    if let Some(hit) = map.get(&source).and_then(std::sync::Weak::upgrade) {
        return Ok(hit);
    }
    // Parsed with the lock held, which is the deliberate choice: it means two
    // workers racing the same body do the work once instead of twice. The
    // alternative — parse outside, insert after — is a single-flight table, and
    // this is not a network round trip. Contention is bounded by the pool width.
    let parsed = Arc::new(vector_tiles::TileData::parse(bytes)?);
    if map.len() >= PARSED_SWEEP_AT {
        map.retain(|_, weak| weak.strong_count() > 0);
    }
    map.insert(source, Arc::downgrade(&parsed));
    Ok(parsed)
}

/// Wait after a tile's first failure before it may be re-queued, doubling with
/// each further failure.
///
/// Any positive number would break the loop; 2 s is chosen so a *transient*
/// failure (one dropped request while the camera settles) still heals within a
/// second or two of arriving somewhere.
const TILE_RETRY_BACKOFF_SECS: f64 = 2.0;

/// Failures before a tile is given up on for this run.
///
/// Three, and then silence. The failure this bounds is not a flaky network — the
/// disk cache and `tile_source`'s single-flight table make a repeat *fetch* nearly
/// free — it is a body that decodes wrong every time it is decoded, which costs a
/// ~120 ms worker rasterise and an `error!` line to establish, forever, at frame
/// rate. There is no count of retries that fixes a corrupt tile, so the useful
/// question is only how quickly we stop asking.
const TILE_MAX_ATTEMPTS: u32 = 3;

/// One tile's failure history, for the backoff.
#[derive(Clone, Copy)]
struct TileFailure {
    attempts: u32,
    /// Clock reading (`Time::elapsed_secs_f64`) of the last failure. Bevy's clock,
    /// not `Instant` — which panics on wasm.
    at: f64,
}

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
    /// Tiles that have failed, with a backoff. Main-thread only (unlike `handles`
    /// and `loading`, which the tasks touch), so it needs no lock.
    ///
    /// The bookkeeping the other streamers get from
    /// [`CellStream`](crate::platform::stream::CellStream); this loader predates it
    /// and still drives a bare [`Dispatcher`], so it keeps its own small copy
    /// rather than growing a refactor into a fix.
    failed: HashMap<TileKey, TileFailure>,
}

impl Default for TileCache {
    fn default() -> Self {
        Self {
            handles: Arc::new(Mutex::new(HashMap::new())),
            loading: Arc::new(Mutex::new(HashMap::new())),
            dispatch: Dispatcher::new(TILE_WORKERS, 0.0),
            results: Arc::new(Mutex::new(Vec::new())),
            failed: HashMap::new(),
        }
    }
}

impl TileCache {
    /// Record a failure and answer whether this tile has spent its attempts.
    fn note_failure(&mut self, key: TileKey, now: f64) -> u32 {
        let f = self.failed.entry(key).or_insert(TileFailure {
            attempts: 0,
            at: now,
        });
        f.attempts += 1;
        f.at = now;
        f.attempts
    }

    /// Whether a tile may be queued at `now` — false while it is inside its
    /// backoff, and false forever once it has exhausted [`TILE_MAX_ATTEMPTS`].
    fn may_retry(&self, key: &TileKey, now: f64) -> bool {
        match self.failed.get(key) {
            None => true,
            Some(f) if f.attempts >= TILE_MAX_ATTEMPTS => false,
            // Doubling, capped by the attempt budget above, so the sequence is
            // 2 s then 4 s and then the tile is done. `powi` rather than a shift:
            // the exponent is bounded by `TILE_MAX_ATTEMPTS` today, and a shift
            // silently becomes a debug-build panic if that constant is ever raised
            // past the width of the integer.
            Some(f) => now - f.at >= TILE_RETRY_BACKOFF_SECS * 2f64.powi(f.attempts as i32 - 1),
        }
    }
}

/// Drop a tile's on-disk cache entry, so a corrupt body is not re-served from
/// disk on every retry and on every subsequent run.
///
/// The key is **the body that was actually fetched**, which for a vector source is
/// the z14 ancestor the overzoom resolved to, not the z15–z17 key that was asked
/// for — evicting the requested key would delete nothing and leave the poison in
/// place. Classification follows the *template*, not the payload variant, because
/// a vector template that answered with an image (the `looks_like_raster`
/// backstop) was still fetched at the ancestor.
///
/// Called on every failure, not only a decode one. A fetch that failed never wrote
/// the key (`tile_source::fetch_uncached` only `put`s a body it got), so the
/// removal is a no-op there — the same trade `lod22` makes, and cheaper than
/// carrying the reason back from the worker.
///
/// What this cannot reach is `tile_source`'s in-memory LRU, which still holds the
/// same bytes until it rolls over. That is what the backoff is for: eviction stops
/// the failure surviving a restart, the backoff stops it costing anything before
/// then.
fn evict_cached_tile(key: TileKey, template: &str) {
    use crate::systems::vector_tiles;

    let kind = tile_source_kind(template);
    let source = if kind == TileSourceKind::Raster {
        key
    } else {
        let ((z, x, y), _) = vector_tiles::SubTile::resolve(key.z, key.x, key.y);
        TileKey::new(x, y, z)
    };
    let cache_key = crate::systems::tile_source::tile_cache_key(
        &source,
        kind,
        crate::systems::tile_source::SourceId::of(template),
    );
    IoTaskPool::get()
        .spawn(async move {
            crate::platform::storage::remove(crate::platform::storage::Namespace::Cache, &cache_key)
                .await
        })
        .detach();
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
            // Read the budget here, on the task, rather than threading it in: it is
            // a process-wide `OnceLock` (see `crate::quality`) and an atomic load,
            // and the alternative is a fifth argument every caller has to carry.
            let tile_px = crate::quality::budget().tile_px;
            match fetch_tile_payload(&key, &template).await {
                Ok(payload) => {
                    crate::platform::workers::submit("map_tiles", Box::new(move || {
                        let outcome = match payload.decode(tile_px) {
                            Ok(rgba) => {
                                let (width, height) = (rgba.width(), rgba.height());
                                let (data, mip_count) =
                                    crate::systems::mip::build_mip_chain(
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
        /// The tile the bytes actually are — the z14 ancestor when overzooming.
        /// Carried so [`parse_shared`] can key on it, which is what makes the up
        /// to 64 sub-tiles of one ancestor a single parse rather than 64.
        source: TileKey,
    },
}

impl TilePayload {
    /// The CPU half: parse/rasterise into RGBA. Pure computation — no I/O, no
    /// globals — which is what lets it run on a worker.
    ///
    /// `size` is the edge length a *vector* body is rasterised at, and it is the
    /// caller's because it is a budget decision: `quality::Budget::tile_px` is 512
    /// on a desktop and 256 on a phone, where the clipmap's 224 tiles are ~310 MB
    /// of texture at the larger size and ~78 MB at the smaller. This is the same
    /// knob [`GlobeTilePayload::decode`] already took, arriving on the flat path a
    /// release late. A raster source arrives at whatever size it was served at and
    /// is left alone — there is nothing to re-rasterise, only pixels to resample,
    /// and resampling on the CPU would cost more than the memory it saved.
    pub fn decode(self, size: u32) -> Result<image::RgbaImage, String> {
        use crate::systems::vector_tiles;
        match self {
            TilePayload::Raster(bytes) => decode_tile_image(&bytes),
            TilePayload::Vector { bytes, sub, source } => {
                let tile = parse_shared(source, &bytes)?;
                let rgba =
                    vector_tiles::render_mvt_sized(&tile, sub, size, source_ground_m(source))
                        .map_err(|e| e.to_string())?;
                image::RgbaImage::from_raw(size, size, rgba)
                    .ok_or_else(|| "rendered tile had the wrong buffer size".to_string())
            }
        }
    }
}

/// Largest edge a served tile image may declare before we refuse to decode it.
///
/// Every basemap this client speaks to serves 256² or 512²; a satellite source
/// someone points `CARTO_TILE_URL` at might serve 1024² or 2048². 4096 clears all
/// of those with room to spare while capping a decoded surface at 4096² × 4 B =
/// 64 MB — small enough that the worst case is a slow frame rather than a dead
/// process.
const MAX_TILE_EDGE_PX: u32 = 4096;

/// Decode an encoded tile image (PNG/JPEG/WebP) to RGBA, under limits.
///
/// **`image::load_from_memory` has no bound of its own, and the compressed body
/// is not one.** A ~200 KB PNG that declares 30000×30000 is a perfectly legal
/// file; `to_rgba8()` on it asks the allocator for 3.6 GB. `http::MAX_BODY` caps
/// what arrives over the wire, which is precisely what a decompression bomb is
/// built to slip past — and the shipped profile is `panic = "abort"`, so an
/// allocation failure here is not an error path this function can return, it is
/// the app disappearing. A hostile tile server is not the only way to reach it: a
/// truncated or misencoded body from our own host reads its dimensions out of the
/// header just as trustingly.
///
/// The width/height limits are the ones that actually bind — `Limits::max_alloc`
/// is documented as non-strict and some decoders ignore it, so it is a second
/// line, not the fence.
fn decode_tile_image(bytes: &[u8]) -> Result<image::RgbaImage, String> {
    let mut reader = image::ImageReader::new(std::io::Cursor::new(bytes))
        .with_guessed_format()
        .map_err(|e| e.to_string())?;
    // `Limits` is `#[non_exhaustive]`, so it has to be built from `default()` and
    // amended rather than written out — which is also what keeps this correct when
    // `image` grows a limit we have not thought about.
    let mut limits = image::Limits::default();
    limits.max_image_width = Some(MAX_TILE_EDGE_PX);
    limits.max_image_height = Some(MAX_TILE_EDGE_PX);
    limits.max_alloc = Some(u64::from(MAX_TILE_EDGE_PX) * u64::from(MAX_TILE_EDGE_PX) * 4);
    reader.limits(limits);
    Ok(reader.decode().map_err(|e| e.to_string())?.to_rgba8())
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

    Ok(TilePayload::Vector { bytes, sub, source })
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
    Ok(GlobeTilePayload::Vector { bytes, sub, source })
}

/// A fetched globe tile body, still undecoded. The globe's twin of [`TilePayload`],
/// separate because a globe tile also carries the ocean mask in its alpha.
pub enum GlobeTilePayload {
    Raster(Arc<Vec<u8>>),
    Vector {
        bytes: Arc<Vec<u8>>,
        sub: crate::systems::vector_tiles::SubTile,
        /// The ancestor the bytes are, for [`parse_shared`]. The globe overzooms
        /// too, and its quadtree wants a *lot* of tiles at once.
        source: TileKey,
    },
}

impl GlobeTilePayload {
    /// The CPU half: rasterise (or image-decode) and pack the ocean mask into
    /// alpha. Pure computation, so it runs on a worker.
    ///
    /// `size` is the edge length to rasterise a vector body at — the globe passes
    /// its own [`globe::GLOBE_TILE_PX`], which is deliberately smaller than the
    /// flat map's [`vector_tiles::TILE_PX`] (see `render_mvt_sized`). A raster
    /// source arrives at whatever size it was served at and is left alone.
    ///
    /// [`globe::GLOBE_TILE_PX`]: crate::systems::globe::GLOBE_TILE_PX
    pub fn decode(self, size: u32) -> Result<(image::RgbaImage, bool), String> {
        use crate::systems::vector_tiles;

        match self {
            GlobeTilePayload::Raster(bytes) => {
                let mut rgba = decode_tile_image(&bytes)?;
                paint_water_alpha_by_colour(&mut rgba);
                Ok((rgba, true))
            }
            GlobeTilePayload::Vector { bytes, sub, source } => {
                // One parse for both passes — and, through `parse_shared`, one for
                // every other tile magnified from the same ancestor. The render and
                // the mask read the same layers, and each used to open its own
                // `Reader` over this body.
                let tile = parse_shared(source, &bytes)?;
                let pixels =
                    vector_tiles::render_mvt_sized(&tile, sub, size, source_ground_m(source))
                        .map_err(|e| e.to_string())?;
                let mut rgba = image::RgbaImage::from_raw(size, size, pixels)
                    .ok_or_else(|| "rendered tile had the wrong buffer size".to_string())?;
                match vector_tiles::ocean_mask_sized(&tile, sub, size) {
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
    let now = time.elapsed_secs_f64();
    let template = tile_url.0.lock().map(|g| g.clone()).unwrap_or_default();

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
                let bevy_image = crate::systems::tile_loader::image_from_mip_chain(
                    tile.width,
                    tile.height,
                    tile.data,
                    tile.mip_count,
                    bevy::render::render_resource::TextureFormat::Rgba8UnormSrgb,
                    TILE_TEXTURE_USAGE,
                    ground_sampler(),
                );
                let handle = images.add(bevy_image);
                if let Ok(mut handles) = tile_cache.handles.lock() {
                    handles.insert(tile_key, handle);
                }
                tracing::info!(tile = %tile_key.to_path(), "tile loaded and applied");
                // A good tile clears any history, so an earlier transient failure
                // doesn't keep counting against a key that now works.
                tile_cache.failed.remove(&tile_key);
            }
            None => {
                // Fetch/decode failed — already logged by the worker.
                //
                // Clearing the `loading` flag below is what used to be the *whole*
                // of this arm, and by itself it is a hot loop: step 2 re-queues the
                // key on the very next frame, the fetch is served straight back out
                // of the byte cache, and a body that decodes wrong decodes wrong
                // again — a ~120 ms worker rasterise and an `error!` line every
                // frame, forever, with nothing to show that anything is wrong
                // beyond the log. `lod22` and `osm_buildings` both learned this and
                // both do what this now does: evict the cached body and back off.
                let attempts = tile_cache.note_failure(tile_key, now);
                evict_cached_tile(tile_key, &template);
                if attempts >= TILE_MAX_ATTEMPTS {
                    tracing::warn!(
                        tile = %tile_key.to_path(),
                        attempts,
                        "giving up on map tile for this run"
                    );
                }
            }
        }
        if let Ok(mut loading) = tile_cache.loading.lock() {
            loading.remove(&tile_key);
        }
    }

    // 2. Attach ready textures / enqueue missing ones onto the dispatcher.
    for (entity, region_tile) in region_query.iter() {
        let tile_key = region_tile.tile_key;

        {
            let handles = tile_cache.handles.lock().unwrap();
            if let Some(handle) = handles.get(&tile_key) {
                // `try_insert`: this query is a live view, and `session::teardown`
                // — which has no ordering against this system — despawns every
                // `Region` the moment a session drops. On the frame it runs first
                // the despawn is already queued while the entity is still visible
                // here, so a plain `insert` lands on a dead entity at the flush and
                // takes the process down (see `run`'s error handler). The texture
                // belongs to a region that no longer exists; dropping it is the
                // right outcome, not a failure.
                commands.entity(entity).try_insert(RegionTileTexture {
                    handle: handle.clone(),
                });
                continue;
            }
        }

        // Before the `loading` flag, not after: a tile inside its backoff must not
        // be marked in-flight, or nothing ever clears the flag and the retry the
        // backoff is holding back can never happen at all.
        if !tile_cache.may_retry(&tile_key, now) {
            continue;
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

    /// The Groningen z14 body the `cartopolis_geo` budgets are measured against.
    fn fixture() -> Vec<u8> {
        std::fs::read(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../geo/tests/fixtures/groningen_z14.mvt"
        ))
        .expect("fixture")
    }

    /// Two asks for the same source share one parse; a different source does not.
    ///
    /// The first half is the whole point — it is what turns a z17 ring's 64
    /// sub-tiles of one ancestor into one decode. The second is the half that
    /// would be a *bug*: keying too loosely would hand a tile its neighbour's
    /// geometry, which is not slow, it is wrong.
    #[test]
    fn one_source_is_parsed_once_and_shared() {
        let bytes = fixture();
        let a = parse_shared(TileKey::new(8380, 5254, 14), &bytes).expect("parse");
        let b = parse_shared(TileKey::new(8380, 5254, 14), &bytes).expect("parse");
        assert!(Arc::ptr_eq(&a, &b), "the same source must share one parse");

        let other = parse_shared(TileKey::new(8381, 5254, 14), &bytes).expect("parse");
        assert!(
            !Arc::ptr_eq(&a, &other),
            "a different source is its own entry"
        );
    }

    /// How much the sharing is actually worth, stated rather than assumed.
    ///
    /// The finest clipmap ring at street level is 9x9 z17 tiles, and z17 is three
    /// levels above `MAX_SOURCE_ZOOM` — so every one of them is a magnified z14
    /// ancestor, and 81 separate worker tasks each did a full `bytes.to_vec()` plus
    /// a prost decode of the *same handful* of bodies. This is the number that
    /// makes `parse_shared` worth a global.
    ///
    /// A ratio rather than an exact count, because the ring's position decides how
    /// many ancestors it straddles (a 9-wide span of z17 covers two z14 columns
    /// unless it lands on a boundary). The floor is what matters.
    #[test]
    fn a_street_level_ring_collapses_to_a_handful_of_parses() {
        use crate::systems::vector_tiles::SubTile;
        let (cx, cy) = (67043, 42033); // a z17 tile over Groningen
        let sources: std::collections::HashSet<_> = (-4..=4)
            .flat_map(|dy| (-4..=4).map(move |dx| (cx + dx, cy + dy)))
            .map(|(x, y)| SubTile::resolve(17, x, y).0)
            .collect();
        assert_eq!(81 / sources.len(), 20, "81 tiles should share ~4 parses");
        assert!(sources.len() <= 4, "got {} distinct bodies", sources.len());
    }

    /// The cache holds `Weak`s, so a parse nobody is reading is not kept alive.
    ///
    /// This is the property that lets it be unbounded: a dense z14 tile peaks at
    /// ~12.5 MiB, and the globe can want hundreds at once, so anything strong here
    /// would be a memory policy rather than a cache.
    #[test]
    fn a_parse_nobody_holds_is_dropped() {
        let bytes = fixture();
        let key = TileKey::new(1234, 5678, 14);
        let first = parse_shared(key, &bytes).expect("parse");
        let addr = Arc::as_ptr(&first);
        drop(first);
        let second = parse_shared(key, &bytes).expect("parse");
        assert!(
            !std::ptr::eq(addr, Arc::as_ptr(&second)),
            "a dropped parse must not be resurrected from the cache"
        );
    }

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
            "https://tiles.cartopolis.org/tiles/osm/{z}/{x}/{y}?v=1",
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
            cartopolis_core::format_osm_tile_url("", &key),
            "https://tiles.cartopolis.org/tiles/osm/14/8490/5319",
            "an empty template must fall back to the default vector source, not raster OSM"
        );
    }

    /// The property under test is not "a failed tile retries" — it is that it
    /// *stops*. The arm this replaced only cleared the `loading` flag, so the tile
    /// was re-queued on the very next frame and a body that decodes wrong burned a
    /// ~120 ms worker rasterise and an `error!` line per frame for the life of the
    /// process.
    #[test]
    fn a_failing_tile_backs_off_and_then_gives_up() {
        let mut cache = TileCache::default();
        let key = TileKey::new(8490, 5319, 14);

        assert!(
            cache.may_retry(&key, 0.0),
            "a tile with no history loads immediately"
        );

        assert_eq!(cache.note_failure(key, 0.0), 1);
        assert!(!cache.may_retry(&key, 0.5), "inside the first backoff");
        assert!(cache.may_retry(&key, TILE_RETRY_BACKOFF_SECS));

        assert_eq!(cache.note_failure(key, 10.0), 2);
        assert!(
            !cache.may_retry(&key, 10.0 + TILE_RETRY_BACKOFF_SECS),
            "the backoff doubles with each failure"
        );
        assert!(cache.may_retry(&key, 10.0 + TILE_RETRY_BACKOFF_SECS * 2.0));

        assert_eq!(cache.note_failure(key, 20.0), TILE_MAX_ATTEMPTS);
        // Exhausted, and no amount of waiting brings it back: a body that is
        // corrupt now is corrupt in an hour, and the cost of finding out again is
        // a worker core for 120 ms.
        assert!(!cache.may_retry(&key, 1.0e9));
    }

    #[test]
    fn cache_keys_separate_by_kind() {
        let key = TileKey::new(8490, 5319, 14);
        let source = crate::systems::tile_source::SourceId::of("");
        let raster =
            crate::systems::tile_source::tile_cache_key(&key, TileSourceKind::Raster, source);
        let vector =
            crate::systems::tile_source::tile_cache_key(&key, TileSourceKind::Vector, source);
        assert_ne!(
            raster, vector,
            "a source switch must not reuse cached bytes"
        );
        assert!(raster.ends_with("5319.png"));
        assert!(vector.ends_with("5319.mvt"));
    }
}

/// A streamed tile texture's [`RenderAssetUsages`].
///
/// `RENDER_WORLD` **without** `MAIN_WORLD`, which is not the default: the default
/// keeps the decoded pixels alive in `Assets<Image>` for the lifetime of the
/// handle *as well as* on the GPU, so every tile is paid for twice. Nothing reads
/// a tile's pixels back — they are decoded once on a worker, uploaded, and only
/// ever sampled — so the main-world copy is pure overhead, and at the globe
/// quadtree's several hundred live tiles it is hundreds of megabytes of it.
///
/// The cost of getting this wrong is not subtle but it is silent: a consumer that
/// later wants CPU pixels (a readback, a resize) finds `Image::data` is `None`
/// once the asset has been prepared.
pub const TILE_TEXTURE_USAGE: RenderAssetUsages = RenderAssetUsages::RENDER_WORLD;

/// Assemble an [`Image`] from a pre-packed mipmap chain (see [`build_mip_chain`]).
/// Uses `new_uninit` to skip `Image::new`'s mip-0-only length assertion, then
/// points the descriptor at all levels; wgpu uploads each from the packed data.
pub fn image_from_mip_chain(
    width: u32,
    height: u32,
    data: Vec<u8>,
    mip_count: u32,
    format: bevy::render::render_resource::TextureFormat,
    usage: RenderAssetUsages,
    sampler: ImageSampler,
) -> Image {
    use bevy::render::render_resource::{Extent3d, TextureDimension};
    let size = Extent3d {
        width,
        height,
        depth_or_array_layers: 1,
    };
    let mut image = Image::new_uninit(size, TextureDimension::D2, format, usage);
    image.texture_descriptor.mip_level_count = mip_count;
    image.data = Some(data);
    image.sampler = sampler;
    image
}
