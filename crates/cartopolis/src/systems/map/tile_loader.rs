use bevy::asset::RenderAssetUsages;
use bevy::prelude::*;
use bevy_image::{Image, ImageSampler, ImageSamplerDescriptor};
use std::collections::HashMap;
use std::sync::{Arc, Mutex};

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
        /// The registered crop parcels over `source`, painted over its land
        /// (`crops::raster_overlay`). Empty outside the BRP coverage.
        crops: Arc<Vec<vector_tiles::OverlayFill>>,
        /// The land cover under `source`, painted under everything
        /// ([`landcover_underlay`]). Empty where the host serves none.
        underlay: Arc<Vec<vector_tiles::OverlayFill>>,
        /// The surveyed ground over `source`, painted as the meshes draw it
        /// (`surveyed::raster_survey`). Empty outside the BGT coverage.
        survey: Arc<vector_tiles::Survey>,
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
            TilePayload::Vector {
                bytes,
                sub,
                source,
                crops,
                underlay,
                survey,
            } => {
                let tile = parse_shared(source, &bytes)?;
                let rgba = vector_tiles::render_mvt_surveyed(
                    &tile,
                    sub,
                    size,
                    source_ground_m(source),
                    &underlay,
                    &crops,
                    &survey,
                )
                .map_err(|e| e.to_string())?;
                let mut img = image::RgbaImage::from_raw(size, size, rgba)
                    .ok_or_else(|| "rendered tile had the wrong buffer size".to_string())?;
                pack_land_alpha(&mut img, vector_tiles::ocean_mask_sized(&tile, sub, size));
                Ok(img)
            }
        }
    }
}

/// Pack a flat tile's water mask into its alpha as **land** coverage: 255 is
/// land, 0 is water.
///
/// Inverted against the globe's (`GlobeTilePayload::decode`, 255 = water) on
/// purpose. `ground_detail.wgsl` lights the water texels as the water surface's
/// far field, and every tile that carries no mask — an image source, a PNG from
/// before the mask existed, a mask that failed — decodes opaque, i.e. 255. With
/// 255 meaning land, "no mask" and "no water" are the same answer, and nothing
/// that did not ask for it is ever drawn as a lake. A failed mask is logged and
/// the tile keeps its painted water.
fn pack_land_alpha(img: &mut image::RgbaImage, mask: Result<Vec<u8>, String>) {
    match mask {
        Ok(mask) if mask.len() == (img.width() * img.height()) as usize => {
            for (px, m) in img.pixels_mut().zip(mask) {
                px.0[3] = u8::MAX - m;
            }
        }
        Ok(_) => tracing::warn!("water mask had the wrong size; tile keeps its painted water"),
        Err(e) => tracing::warn!(error = %e, "water mask failed; tile keeps its painted water"),
    }
}

/// What the pixels a [`rendered_cache_key`] entry holds *mean*, beyond what the
/// renderer's revision already covers. The revision measures what
/// `cartopolis_geo` draws, and the water mask is packed on this side of the seam
/// (`pack_land_alpha`), so without this an entry drawn before the mask existed
/// would be read back as a tile with no water in it.
///
/// 3: the surveyed ground is painted in (`surveyed::raster_survey`), built on this
/// side of the seam.
const RENDERED_FORMAT: u32 = 3;

/// Where a finished basemap tile is kept on disk, or `None` when it is not kept.
///
/// **The basemap is drawn on this machine, and drawing it is the whole cost of a
/// warm start.** An image tile map downloads pixels; this one downloads vector
/// tiles and rasterises them, ~30–70 ms a tile, and a warm *byte* cache buys 2 s of
/// a 27.8 s fill because the bytes were never what was slow
/// (`docs/notes/keeping-what-was-expensive.md`). So the finished pixels are kept
/// too, as a PNG, and a return visit decodes one instead of drawing it.
///
/// The key carries everything the pixels depend on, so a hit can never be a stale
/// picture: the renderer's revision (`cartopolis_geo::render_revision`, a hash
/// of what it draws, so a build that changed no pixel keeps every stored tile),
/// the raster size, the tile, and a hash of the vector
/// body it was drawn from — a server that re-publishes the tile publishes a
/// different body, and the old entry is simply never asked for again.
///
/// Native only: the web's storage is IndexedDB behind the main thread, and the
/// write happens on a worker.
#[must_use]
pub fn rendered_cache_key(key: &TileKey, payload: &TilePayload, size: u32) -> Option<String> {
    if cfg!(target_arch = "wasm32") {
        return None;
    }
    let TilePayload::Vector {
        bytes,
        crops,
        underlay,
        survey,
        ..
    } = payload
    else {
        return None;
    };
    use std::hash::Hash;
    let mut h = std::collections::hash_map::DefaultHasher::new();
    bytes.hash(&mut h);
    // The crops are drawn into the same pixels, so they are part of what the
    // picture is of: a field re-registered next year, or a tile first drawn
    // before the host served the register at all, must not be read back. The
    // land cover and the survey likewise.
    hash_fills(&mut h, crops);
    hash_fills(&mut h, underlay);
    hash_fills(&mut h, &survey.paving);
    for c in &survey.cells {
        c.bounds.map(f32::to_bits).hash(&mut h);
        (c.cover as u8).hash(&mut h);
    }
    use std::hash::Hasher;
    Some(format!(
        "rendered/{:016x}-{RENDERED_FORMAT}/{size}/{}/{}/{}-{:016x}.png",
        cartopolis_geo::render_revision::RENDER_REVISION,
        key.z,
        key.x,
        key.y,
        h.finish()
    ))
}

/// [`rendered_cache_key`] for a globe tile: the same pixels, drawn at the globe's
/// own size, with the ocean mask packed into alpha — so a distinct entry.
///
/// **The globe re-drew every tile on every launch.** Its bytes were cached and its
/// pixels were not, so zooming out to the planet paid tiny-skia for every tile of
/// the quadtree each time, where the flat map had been reading finished tiles
/// back since `keeping-what-was-expensive.md`. Only a tile whose mask came out is
/// kept, because the entry cannot say whether it has one and a read assumes it.
#[must_use]
pub fn globe_cache_key(key: &TileKey, payload: &GlobeTilePayload, size: u32) -> Option<String> {
    if cfg!(target_arch = "wasm32") {
        return None;
    }
    let GlobeTilePayload::Vector {
        bytes, underlay, ..
    } = payload
    else {
        return None;
    };
    use std::hash::{Hash, Hasher};
    let mut h = std::collections::hash_map::DefaultHasher::new();
    bytes.hash(&mut h);
    hash_fills(&mut h, underlay);
    Some(format!(
        "rendered-globe/{:016x}/{size}/{}/{}/{}-{:016x}.png",
        cartopolis_geo::render_revision::RENDER_REVISION,
        key.z,
        key.x,
        key.y,
        h.finish()
    ))
}

/// Fold painted-in fills into a rendered-tile cache key.
fn hash_fills(h: &mut impl std::hash::Hasher, fills: &[vector_tiles::OverlayFill]) {
    use std::hash::Hash;
    for fill in fills {
        fill.rgb.hash(h);
        for ring in &fill.rings {
            ring.len().hash(h);
            for p in ring {
                p[0].to_bits().hash(h);
                p[1].to_bits().hash(h);
            }
        }
    }
}

/// A stored tile back to pixels, if it decodes to the size that was asked for.
/// Anything else — a torn file, a stray entry — is a miss, and the tile is drawn.
#[must_use]
pub fn decode_rendered(bytes: &[u8], size: u32) -> Option<image::RgbaImage> {
    decode_tile_image(bytes)
        .ok()
        .filter(|img| img.width() == size && img.height() == size)
}

/// Keep a freshly drawn tile under `cache_key`. Best-effort, like every cache
/// write: a failure is a future miss.
///
/// PNG at the fastest setting, because the encode is paid on the cold path: a
/// basemap tile is large areas of flat colour, which that setting still packs
/// well, and a slower one would tax the fill it is meant to speed up later.
pub fn store_rendered(cache_key: &str, img: &image::RgbaImage) {
    if let Some(png) = encode_rendered(img) {
        let _ = crate::platform::storage::write_now(
            crate::platform::storage::Namespace::Cache,
            cache_key,
            &png,
        );
    }
}

fn encode_rendered(img: &image::RgbaImage) -> Option<Vec<u8>> {
    use image::ImageEncoder;
    let mut png = Vec::new();
    image::codecs::png::PngEncoder::new_with_quality(
        &mut png,
        image::codecs::png::CompressionType::Fast,
        image::codecs::png::FilterType::Adaptive,
    )
    .write_image(
        img.as_raw(),
        img.width(),
        img.height(),
        image::ExtendedColorType::Rgba8,
    )
    .ok()?;
    Some(png)
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

/// The layers painted into one source tile from outside its body: the crop
/// parcels, the land cover and the surveyed paving.
type Overlays = (
    Arc<Vec<vector_tiles::OverlayFill>>,
    Arc<Vec<vector_tiles::OverlayFill>>,
    Arc<vector_tiles::Survey>,
);

/// What a source's overlays were built against: the source, the map template
/// (which names the land cover), and the coverage revisions of the three trees
/// they are read from — so an answer built before a manifest landed is never
/// served after it.
type OverlayKey = (TileKey, String, (u32, u32, u32));

type OverlayWaiters = Vec<futures_channel::oneshot::Sender<Overlays>>;

/// Overlays already built, held by `Weak` — alive exactly as long as some tile
/// drawn from them still is — plus the builds in progress and who is waiting.
#[derive(Default)]
struct OverlayMemo {
    built: HashMap<
        OverlayKey,
        (
            std::sync::Weak<Vec<vector_tiles::OverlayFill>>,
            std::sync::Weak<Vec<vector_tiles::OverlayFill>>,
            std::sync::Weak<vector_tiles::Survey>,
        ),
    >,
    building: HashMap<OverlayKey, OverlayWaiters>,
}

static OVERLAYS: std::sync::LazyLock<Mutex<OverlayMemo>> =
    std::sync::LazyLock::new(Default::default);

/// One source tile's overlays, **built once however many tiles are drawn from
/// it**.
///
/// Every clipmap tile above z14 is drawn from its z14 ancestor, so up to 64 tiles
/// share one source — and each used to build that source's overlays itself:
/// read and parse the crop parcels (no memo at all), the land cover, and every
/// surveyed paving and roof file under it (a 24-entry memo that the first eight
/// tiles, starting together, all missed). Groningen's densest paving file is
/// 2.3 MB of JSON. All of it also ran before a tile could look up its finished
/// image on disk, whose key is made from these, so a launch with every tile
/// already drawn still paid for it per tile.
///
/// A second tile asking while the first is building waits for its answer rather
/// than building its own; the answer lives as long as a tile holding it does.
async fn source_overlays(source: TileKey, template: &str) -> Overlays {
    let key: OverlayKey = (
        source,
        template.to_string(),
        (
            crate::systems::crops::coverage_revision(),
            crate::systems::surveyed::coverage_revision(),
            crate::systems::roof_colours::coverage_revision(),
        ),
    );
    let lock = || {
        OVERLAYS
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    };
    let waiter = {
        let mut memo = lock();
        if let Some((c, u, s)) = memo.built.get(&key) {
            if let (Some(c), Some(u), Some(s)) = (c.upgrade(), u.upgrade(), s.upgrade()) {
                return (c, u, s);
            }
        }
        match memo.building.get_mut(&key) {
            Some(waiters) => {
                let (tx, rx) = futures_channel::oneshot::channel();
                waiters.push(tx);
                Some(rx)
            }
            None => {
                memo.building.insert(key.clone(), Vec::new());
                None
            }
        }
    };
    if let Some(rx) = waiter {
        // A dropped sender (the builder's task went away) is not an answer;
        // build it here instead.
        if let Ok(overlays) = rx.await {
            return overlays;
        }
    }
    // Whatever happens to this build — finished, or its task dropped or panicking
    // part-way — the entry leaves `building`, which drops any waiters' senders so
    // they build for themselves instead of waiting for ever.
    struct Building(Option<OverlayKey>);
    impl Drop for Building {
        fn drop(&mut self) {
            if let Some(key) = self.0.take() {
                OVERLAYS
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner)
                    .building
                    .remove(&key);
            }
        }
    }
    let mut guard = Building(Some(key.clone()));
    let built: Overlays = (
        Arc::new(crate::systems::crops::raster_overlay(source).await),
        Arc::new(landcover_underlay(source, template).await),
        crate::systems::surveyed::raster_survey(source).await,
    );
    let waiters = {
        let mut memo = lock();
        memo.built.retain(|_, (c, _, _)| c.strong_count() > 0);
        memo.built.insert(
            key.clone(),
            (
                Arc::downgrade(&built.0),
                Arc::downgrade(&built.1),
                Arc::downgrade(&built.2),
            ),
        );
        guard.0 = None;
        memo.building.remove(&key)
    };
    for tx in waiters.into_iter().flatten() {
        let _ = tx.send(built.clone());
    }
    built
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

    let (crops, underlay, survey) = source_overlays(source, template).await;
    Ok(TilePayload::Vector {
        bytes,
        sub,
        source,
        crops,
        underlay,
        survey,
    })
}

/// Where the land-cover tiles are, for a map served from `template`.
///
/// VersaTiles serves each archive as its own tile set on one host
/// (`/tiles/osm/…`, `/tiles/landcover/…`), so the land cover is found by naming
/// the other set. `CARTO_LANDCOVER_URL` overrides it and `off` disables it; a
/// template that is not a VersaTiles `osm` set has no land cover to find.
fn landcover_template(template: &str) -> Option<String> {
    #[cfg(not(target_arch = "wasm32"))]
    if let Ok(url) = std::env::var("CARTO_LANDCOVER_URL") {
        return (!url.is_empty() && url != "off").then_some(url);
    }
    let effective = if template.is_empty() {
        cartopolis_core::world::DEFAULT_TILE_URL_TEMPLATE
    } else {
        template
    };
    effective
        .contains("/tiles/osm/")
        .then(|| effective.replacen("/tiles/osm/", "/tiles/landcover/", 1))
}

/// The land cover under `source`, as fills in its unit square
/// (`vector_tiles::landcover_fills`). A source deeper than the set's last zoom
/// reads its ancestor at that zoom. Any failure — a host without the set, a 404
/// over open ocean or Antarctica — is an empty underlay, i.e. the paper the map
/// always had.
async fn landcover_underlay(source: TileKey, template: &str) -> Vec<vector_tiles::OverlayFill> {
    let Some(lc) = landcover_template(template) else {
        return Vec::new();
    };
    let lz = source.z.min(vector_tiles::LANDCOVER_MAX_ZOOM);
    let shift = source.z - lz;
    let (ax, ay) = (source.x >> shift, source.y >> shift);
    let ancestor = TileKey::new(ax, ay, lz);
    let Ok(bytes) = load_tile_image(&ancestor, &lc).await else {
        return Vec::new();
    };
    if bytes.is_empty() || vector_tiles::looks_like_raster(&bytes) {
        return Vec::new();
    }
    match vector_tiles::TileData::parse(&bytes) {
        Ok(tile) => vector_tiles::landcover_fills(
            &tile,
            shift,
            (source.x - (ax << shift)) as u32,
            (source.y - (ay << shift)) as u32,
        ),
        Err(e) => {
            tracing::warn!(tile = %ancestor.to_path(), error = %e, "land cover tile did not parse");
            Vec::new()
        }
    }
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
    let underlay = Arc::new(landcover_underlay(source, template).await);
    Ok(GlobeTilePayload::Vector {
        bytes,
        sub,
        source,
        underlay,
    })
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
        /// The land cover, painted under everything ([`landcover_underlay`]).
        /// This is most of what the globe shows: below z8 the map has no land.
        underlay: Arc<Vec<crate::systems::vector_tiles::OverlayFill>>,
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
            GlobeTilePayload::Vector {
                bytes,
                sub,
                source,
                underlay,
            } => {
                // One parse for both passes — and, through `parse_shared`, one for
                // every other tile magnified from the same ancestor. The render and
                // the mask read the same layers, and each used to open its own
                // `Reader` over this body.
                let tile = parse_shared(source, &bytes)?;
                let pixels = vector_tiles::render_mvt_layered(
                    &tile,
                    sub,
                    size,
                    source_ground_m(source),
                    &underlay,
                    &[],
                )
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

    /// A stored tile must come back as exactly the pixels that were drawn — the
    /// cache stands in for the renderer, so any drift would be a different map on
    /// a warm start than on a cold one.
    #[test]
    fn a_stored_tile_comes_back_pixel_for_pixel() {
        let key = TileKey::new(8431, 5374, 14);
        let payload = TilePayload::Vector {
            bytes: Arc::new(fixture()),
            sub: crate::systems::vector_tiles::SubTile::WHOLE,
            source: key,
            crops: Arc::default(),
            underlay: Arc::default(),
            survey: Arc::default(),
        };
        let size = 256;
        let cache_key = rendered_cache_key(&key, &payload, size).expect("a vector tile is kept");
        assert!(cache_key.contains(&format!(
            "{:016x}",
            cartopolis_geo::render_revision::RENDER_REVISION
        )));
        let drawn = payload.decode(size).expect("fixture draws");
        let png = encode_rendered(&drawn).expect("encodes");
        let back = decode_rendered(&png, size).expect("decodes");
        assert!(
            drawn.as_raw() == back.as_raw(),
            "the stored tile is not the drawn one"
        );
        // …and at the wrong size it is a miss, not a stretched picture.
        assert!(decode_rendered(&png, size * 2).is_none());
    }

    /// Two bodies for one tile are two entries: a re-published tile must not be
    /// answered with the picture of the old one.
    #[test]
    fn a_changed_body_is_a_different_entry() {
        let key = TileKey::new(8431, 5374, 14);
        let mut other = fixture();
        other.push(0);
        let a = TilePayload::Vector {
            bytes: Arc::new(fixture()),
            sub: crate::systems::vector_tiles::SubTile::WHOLE,
            source: key,
            crops: Arc::default(),
            underlay: Arc::default(),
            survey: Arc::default(),
        };
        let b = TilePayload::Vector {
            bytes: Arc::new(other),
            sub: crate::systems::vector_tiles::SubTile::WHOLE,
            source: key,
            crops: Arc::default(),
            underlay: Arc::default(),
            survey: Arc::default(),
        };
        assert_ne!(
            rendered_cache_key(&key, &a, 256),
            rendered_cache_key(&key, &b, 256)
        );
        assert_ne!(
            rendered_cache_key(&key, &a, 256),
            rendered_cache_key(&key, &a, 512)
        );
        // …and a tile drawn with crop fields over it is not the one drawn
        // without them.
        let with_crops = TilePayload::Vector {
            bytes: Arc::new(fixture()),
            sub: crate::systems::vector_tiles::SubTile::WHOLE,
            source: key,
            crops: Arc::new(vec![crate::systems::vector_tiles::OverlayFill {
                rings: vec![vec![[0.1, 0.1], [0.2, 0.1], [0.2, 0.2]]],
                rgb: 0xE1_D3_A4,
            }]),
            underlay: Arc::default(),
            survey: Arc::default(),
        };
        assert_ne!(
            rendered_cache_key(&key, &a, 256),
            rendered_cache_key(&key, &with_crops, 256)
        );
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
