//! Streaming map tier (ADR-020): a camera-following grid of OSM ground tiles that
//! load as you roam and unload behind you. Supersedes the static patch.
//!
//! Stage 1: tiles are root entities in the world frame. This is correct while the
//! camera stays within the home big_space cell (~14 km of ground — ample for a
//! city). Stage 2 will place them under the `BigSpace` with `GridCell`s for
//! unlimited range, and stream buildings per tile.
//!
//! Textures are fetched on a background worker (one Overpass/tile HTTP call per
//! tile, via an mpsc queue) so roaming never blocks the frame.

use std::collections::{HashMap, HashSet};
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::{Arc, Mutex};

use bevy::prelude::*;
use bevy::tasks::IoTaskPool;
use web_time::Instant;
use bevy::asset::RenderAssetUsages;
use bevy::render::render_resource::TextureFormat;
use bevy::camera::visibility::NoFrustumCulling;
use bevy_image::{Image, ImageSampler};
use big_space::prelude::{BigSpace, Grid, GridCell};

use crate::resources::OsmTileUrlTemplate;
use crate::systems::free_camera::{FreeCamera, WORLD_CELL_EDGE, WORLD_SWITCH_THRESHOLD};
use vibe_core::world::{lat_lng_to_tile, tile_to_lat_lng, tile_to_meters};
use vibe_core::TileKey;

/// Snap a (lat, lng) to the **centre of its base-zoom tile** — the anchor
/// convention every placement consumer assumes (`osm_buildings` and the data
/// layers position vertices relative to the anchor *tile centre* via
/// `lat_lng_to_local`). Re-anchoring to a raw point instead of a tile centre
/// shifted buildings/pins off the map imagery by up to half a tile.
pub fn snap_anchor(lat: f64, lng: f64, zoom: u32) -> (f64, f64) {
    let (tx, ty) = lat_lng_to_tile(lat, lng, zoom);
    let (n_lat, w_lng) = tile_to_lat_lng(tx, ty, zoom);
    let (s_lat, e_lng) = tile_to_lat_lng(tx + 1, ty + 1, zoom);
    ((n_lat + s_lat) / 2.0, (w_lng + e_lng) / 2.0)
}

/// Fractional Web Mercator tile coordinate (no flooring) of a lat/lng at a zoom.
pub fn lat_lng_to_tile_frac(lat: f64, lng: f64, zoom: u32) -> (f64, f64) {
    let n = 2.0_f64.powi(zoom as i32);
    let x = (lng + 180.0) / 360.0 * n;
    let lat_rad = lat.to_radians();
    let y = (1.0 - (lat_rad.tan() + 1.0 / lat_rad.cos()).ln() / std::f64::consts::PI) / 2.0 * n;
    (x, y)
}

/// Tiles to keep loaded in each direction from the camera at the finest zoom
/// (radius 4 → 9×9).
const LOAD_RADIUS: i64 = 4;
/// Ring radius for each coarser clipmap level beneath the finest (radius 3 → 7×7,
/// covering twice the previous level's extent per step).
const COARSE_RADIUS: i64 = 3;
/// Lowest (coarsest) zoom of the clipmap pyramid; its ring is what fills the view
/// out to the horizon (7×7 tiles at zoom 10 ≈ 165 km).
const MIN_ZOOM: u32 = 10;
/// Give up on a tile fetch after this many failed attempts (leaves the placeholder).
const MAX_FETCH_ATTEMPTS: u32 = 4;
/// Max ground tiles to despawn per frame — the mass-*despawn* twin of
/// `MAX_UPLOADS_PER_FRAME`. A zoom-out steps the LOD zoom down and retires the
/// whole previous finest-zoom ring at once; freeing all those per-tile textures in
/// one command buffer overran a weak GPU's timeout. Retired tiles are queued and
/// freed at this rate.
const MAX_TILE_DESPAWNS_PER_FRAME: usize = 12;
/// Altitude margin a zoom boundary must be crossed by before switching (anti-thrash).
const ZOOM_HYSTERESIS_M: f32 = 60.0;

/// Pick a map zoom from camera altitude: detailed near the ground, coarser high up
/// so a wide area is visible cheaply. Capped at the region's base zoom.
fn zoom_for_altitude(altitude_m: f32, base_zoom: u32) -> u32 {
    let z = if altitude_m < 200.0 {
        base_zoom
    } else if altitude_m < 700.0 {
        base_zoom.saturating_sub(1)
    } else if altitude_m < 2_000.0 {
        base_zoom.saturating_sub(2)
    } else if altitude_m < 6_000.0 {
        base_zoom.saturating_sub(3)
    } else {
        base_zoom.saturating_sub(4)
    };
    z.max(MIN_ZOOM)
}

#[derive(Component)]
pub struct StreamTile {
    key: TileKey,
}

/// Marker: imagery applied — don't reprocess, and safe to show.
#[derive(Component)]
pub struct StreamTileReady;

/// Marker: the fetch attempt budget is spent and this tile will never get imagery.
///
/// Kept distinct from [`StreamTileReady`] because the two want opposite treatment
/// on screen: a textured tile is revealed, a permanently failed one stays hidden so
/// the coarser level beneath keeps covering that ground. Both stop reprocessing.
#[derive(Component)]
pub struct StreamTileFailed;

/// Marker on the **coarsest** clipmap level (`MIN_ZOOM`) — the one level allowed to
/// show its untextured placeholder.
///
/// Every finer tile stays hidden until its imagery lands, so what shows through the
/// gap is the coarser level already stacked beneath it: the map arrives blurry and
/// sharpens, instead of arriving as grey squares. That backstop-from-below is what
/// the pyramid was built for, and an opaque placeholder quad was defeating it — the
/// finer tile spawned *on top* of the coarse imagery it was supposed to be refining.
/// `MIN_ZOOM` has nothing beneath it, so it keeps a visible placeholder or the
/// horizon would be a hole; it is painted the basemap's own paper colour, which
/// reads as unsurveyed ground rather than as a slab of concrete.
#[derive(Component)]
pub struct MapBackstop;

/// A tile decoded and mipmapped **off the main thread** by a fetch worker. Holds
/// the full packed mip chain ready for a cheap `Image` assembly + GPU upload on
/// the main thread — the PNG decode and mip generation (the expensive parts) never
/// touch the render loop, so streaming new tiles while moving doesn't hitch.
struct DecodedTile {
    data: Vec<u8>,
    width: u32,
    height: u32,
    mip_count: u32,
}

#[derive(Resource)]
pub struct MapStream {
    /// World origin: a (lat, lng) plus base zoom. Every zoom's tiles are placed
    /// relative to this one point, so changing zoom never shifts the map. No longer
    /// fixed to a "region" — re-anchoring moves it anywhere on the globe so detail
    /// can stream wherever the camera goes (ADR-025, anchor-less direction).
    anchor: Option<(f64, f64, u32)>,
    /// Bumped whenever the anchor changes (init or re-anchor). Other systems (the
    /// globe, buildings) compare against this to rebuild/reset for the new origin.
    pub anchor_generation: u32,
    /// A requested re-anchor target, applied by `apply_reanchor` next frame.
    pending_reanchor: Option<(f64, f64)>,
    /// Finest displayed zoom (driven by camera altitude for LOD); coarser clipmap
    /// levels are derived from it down to `MIN_ZOOM`.
    current_zoom: u32,
    loaded: HashMap<TileKey, Entity>,
    /// Tiles with a fetch **in flight** (job sent, result not yet applied). Purely
    /// an in-flight guard so a pending tile isn't re-queued every frame — a tile is
    /// dropped from here when its result is consumed, and when it's retired, because
    /// the texture dies with the entity and a re-entering tile must fetch again (the
    /// on-disk cache makes that cheap). Treating it as a permanent "ever requested"
    /// set left every re-streamed tile grey forever — and it also pinned
    /// [`MapStream::streaming_busy`] true for the whole session, so every `--shot`
    /// sat out its full `--wait` instead of capturing once the map went quiet.
    requested: HashSet<TileKey>,
    /// Failed fetches awaiting a bounded retry: attempt count + time of last try.
    failed: HashMap<TileKey, (u32, Instant)>,
    results: Arc<Mutex<HashMap<TileKey, Option<DecodedTile>>>>,
    /// The live anchor generation a fetch task compares against (main thread
    /// stores, task loads) — a re-anchor's stale in-flight fetches drop themselves.
    gen_shared: Arc<AtomicU32>,
    /// Shared ground-quad mesh per zoom level (each zoom has its own tile size).
    quads: HashMap<u32, Handle<Mesh>>,
    placeholder: Option<Handle<StandardMaterial>>,
    /// Tiles retired from `loaded`, despawned under `MAX_TILE_DESPAWNS_PER_FRAME`
    /// per frame. A LOD zoom-out retires a whole finest-zoom ring at once and a
    /// re-anchor retires the entire patch; freeing hundreds of tile textures in one
    /// command buffer is the mass-despawn burst that hangs a weak GPU
    /// (`VK_ERROR_DEVICE_LOST`) — the same failure the upload cap already guards.
    despawn_queue: Vec<Entity>,
}

impl MapStream {
    /// Whether any streamed tile has been applied yet — the boot splash's
    /// "the world is visible" signal (`systems::splash`). Exposed as a predicate
    /// rather than the map itself so the field stays private.
    pub fn has_tiles(&self) -> bool {
        !self.loaded.is_empty()
    }
}

impl MapStream {
    /// The geo anchor (lat, lng of the world origin + base zoom) — the geo↔world
    /// mapping other systems build on.
    pub fn geo_anchor(&self) -> Option<(f64, f64, u32)> {
        self.anchor
    }

    /// Are tiles still being fetched or retired? Used by the `--shot` harness to
    /// capture as soon as the view has settled instead of on a fixed timer.
    pub fn streaming_busy(&self) -> bool {
        !self.requested.is_empty() || !self.failed.is_empty() || !self.despawn_queue.is_empty()
    }

    /// Request a re-anchor to a new (lat, lng); applied by `apply_reanchor`, which
    /// drops the loaded tiles so they re-stream around the camera at the new origin.
    pub fn request_reanchor(&mut self, lat: f64, lng: f64) {
        self.pending_reanchor = Some((lat, lng));
    }
}

/// Apply a pending re-anchor: despawn every loaded tile (its world position is
/// relative to the old origin), move the anchor, and bump the generation so the
/// globe and buildings rebuild for the new origin. `update_map_stream` re-streams
/// tiles around the camera next frame; the on-disk tile cache keeps it cheap.
pub fn apply_reanchor(
    mut stream: ResMut<MapStream>,
    tiles: Query<Entity, With<StreamTile>>,
) {
    let Some((lat, lng)) = stream.pending_reanchor.take() else {
        return;
    };
    let base_zoom = stream
        .anchor
        .map(|(_, _, z)| z)
        .unwrap_or(vibe_core::world::REGION_ZOOM_LEVEL);
    // Queue the whole old-anchor patch for bounded despawn rather than freeing it
    // all in one frame (mass-despawn device-loss guard; drained by
    // `update_map_stream`). The queued entities are still live StreamTiles, so the
    // query re-captures them — clear any prior queue first to avoid double-frees.
    stream.despawn_queue.clear();
    for e in tiles.iter() {
        stream.despawn_queue.push(e);
    }
    stream.loaded.clear();
    // Clear the fetch bookkeeping so the re-streamed tiles request again (served
    // from the on-disk cache when already fetched); the anchor is unchanged tiles
    // would otherwise be treated as still-in-flight and never re-textured.
    stream.requested.clear();
    stream.failed.clear();
    // Quad meshes are sized by tile metres *at the anchor latitude*; stale sizes
    // at a new latitude would leave gaps or overlaps between tiles.
    stream.quads.clear();
    // Anchor on the containing tile's centre (see `snap_anchor`) so tiles,
    // buildings and pins keep sharing one placement convention.
    let (lat, lng) = snap_anchor(lat, lng, base_zoom);
    stream.anchor = Some((lat, lng, base_zoom));
    stream.current_zoom = base_zoom;
    stream.anchor_generation = stream.anchor_generation.wrapping_add(1);
    // Retire queued fetches from the old anchor: the workers drop any job whose
    // generation doesn't match, so the new anchor's tiles aren't stuck behind
    // hundreds of stale fetches.
    stream
        .gen_shared
        .store(stream.anchor_generation, Ordering::Relaxed);
    tracing::info!(lat, lng, "map re-anchored");
}

impl Default for MapStream {
    fn default() -> Self {
        Self {
            anchor: None,
            anchor_generation: 0,
            pending_reanchor: None,
            current_zoom: 0,
            loaded: HashMap::new(),
            requested: HashSet::new(),
            failed: HashMap::new(),
            results: Arc::new(Mutex::new(HashMap::new())),
            gen_shared: Arc::new(AtomicU32::new(0)),
            quads: HashMap::new(),
            placeholder: None,
            despawn_queue: Vec::new(),
        }
    }
}

/// Once a region exists, set the anchor and spawn the background fetch worker.
/// Bootstrap map anchor when nothing else has set one (Groningen Grote Markt).
/// `user_store` re-anchors to the last location on startup if one is saved.
const DEFAULT_ANCHOR_LAT: f64 = 53.2194;
const DEFAULT_ANCHOR_LNG: f64 = 6.5665;
const DEFAULT_ANCHOR_ZOOM: u32 = 17;

pub fn init_map_stream(
    mut stream: ResMut<MapStream>,
    mut materials: ResMut<Assets<StandardMaterial>>,
    _tile_url: Res<OsmTileUrlTemplate>,
) {
    if stream.anchor.is_some() {
        return;
    }
    // Bootstrap anchor (movable, not region-derived): the map opens here, then
    // `user_store` teleports a returning user to their last location, which
    // re-anchors. Regions are retired (ADR-020/029), so there is no seed region
    // to read a location from.
    let (clat, clng, z) = (DEFAULT_ANCHOR_LAT, DEFAULT_ANCHOR_LNG, DEFAULT_ANCHOR_ZOOM);
    stream.anchor = Some((clat, clng, z));
    stream.current_zoom = z;
    stream.anchor_generation = stream.anchor_generation.wrapping_add(1);
    stream
        .gen_shared
        .store(stream.anchor_generation, Ordering::Relaxed);
    // The basemap's own background tone, not a neutral grey: only the coarsest ring
    // ever shows this (see `MapBackstop`), and there it should read as blank paper
    // continuous with the tiles around it.
    const PAPER: u32 = crate::systems::vector_tiles::palette::PAPER;
    stream.placeholder = Some(materials.add(StandardMaterial {
        base_color: Color::srgb(
            ((PAPER >> 16) & 0xFF) as f32 / 255.0,
            ((PAPER >> 8) & 0xFF) as f32 / 255.0,
            (PAPER & 0xFF) as f32 / 255.0,
        ),
        // Fully matte: default reflectance puts a grazing-angle Fresnel sheen on
        // distant ground, bleaching the far map toward white.
        perceptual_roughness: 1.0,
        reflectance: 0.0,
        ..default()
    }));

    tracing::info!("map streaming initialized");
}

/// Spawn one ground-tile fetch+decode task (ADR-033 seam): gen-tagged so a
/// re-anchor's stale in-flight fetch drops itself, then decode + mip-chain off the
/// main thread and hand the finished pixels back for the render loop to upload.
fn spawn_map_tile(
    key: TileKey,
    gen: u32,
    template: String,
    results: Arc<Mutex<HashMap<TileKey, Option<DecodedTile>>>>,
    live_gen: Arc<AtomicU32>,
) {
    // Two halves on two kinds of executor (ADR-033): the fetch is awaited here on
    // the `IoTaskPool` — on wasm that is the main thread, but awaiting a browser
    // fetch costs it nothing — while the decode + mip chain, which is pure
    // arithmetic and *does* cost a core, is handed to `platform::workers`. Doing
    // the decode inline is what made the web build fill the map at a crawl.
    IoTaskPool::get()
        .spawn(async move {
            if gen != live_gen.load(Ordering::Relaxed) {
                // Stale (re-anchored while queued); insert a miss so `requested`
                // clears and a still-wanted tile re-queues on the fresh anchor.
                if let Ok(mut g) = results.lock() {
                    g.insert(key, None);
                }
                return;
            }
            match crate::systems::tile_loader::fetch_tile_payload(&key, &template).await {
                Ok(payload) => {
                    crate::platform::workers::submit(Box::new(move || {
                        let decoded = match payload.decode() {
                            Ok(rgba) => {
                                let (width, height) = (rgba.width(), rgba.height());
                                let (data, mip_count) =
                                    crate::systems::rendering::build_mip_chain(
                                        width,
                                        height,
                                        rgba.into_raw(),
                                    );
                                Some(DecodedTile {
                                    data,
                                    width,
                                    height,
                                    mip_count,
                                })
                            }
                            Err(e) => {
                                tracing::warn!(tile = %key.to_path(), error = %e, "map tile decode failed");
                                None
                            }
                        };
                        if let Ok(mut g) = results.lock() {
                            g.insert(key, decoded);
                        }
                    }));
                }
                Err(e) => {
                    tracing::warn!(tile = %key.to_path(), error = %e, "map tile load failed");
                    if let Ok(mut g) = results.lock() {
                        g.insert(key, None);
                    }
                }
            }
        })
        .detach();
}

/// Spawn ground tiles within `LOAD_RADIUS` of the camera; despawn the rest.
pub fn update_map_stream(
    mut commands: Commands,
    mut stream: ResMut<MapStream>,
    mut meshes: ResMut<Assets<Mesh>>,
    tile_url: Res<OsmTileUrlTemplate>,
    camera: Query<(&GridCell, &Transform), With<FreeCamera>>,
    bigspace: Query<Entity, With<BigSpace>>,
    globe: Res<crate::systems::globe::GlobeState>,
) {
    let Some((clat, clng, base_zoom)) = stream.anchor else {
        return;
    };
    let Some(placeholder) = stream.placeholder.clone() else {
        return;
    };
    let template = tile_url.0.lock().map(|g| g.clone()).unwrap_or_default();
    let Ok((cell, tf)) = camera.single() else {
        return;
    };
    let Ok(root) = bigspace.single() else {
        return;
    };

    // Drain the tile despawn queue under a per-frame budget (mass-despawn guard).
    // Above the space-freeze return below nothing is queued, but draining here
    // keeps a zoom-out's retired rings clearing steadily as the camera climbs.
    if !stream.despawn_queue.is_empty() {
        let n = stream.despawn_queue.len().min(MAX_TILE_DESPAWNS_PER_FRAME);
        for e in stream.despawn_queue.drain(..n) {
            commands.entity(e).despawn();
        }
    }

    // Freeze streaming from orbit (ADR-025): above the globe-show altitude the
    // detailed tiles are hidden anyway (globe::HideInSpace), so keep the pyramid
    // loaded instead of despawning it — returning to the surface is then instant,
    // and the space camera doesn't spam tile requests for wherever it roams.
    // Geocentric altitude, NOT local y: orbiting to the far side of the globe
    // drives local y negative, which would unfreeze streaming and fetch tiles
    // for nonsense coordinates (real bug: it hammered the tile server).
    let altitude = globe.altitude_m(cell, tf) as f32;
    if altitude > crate::systems::globe::GLOBE_SHOW_ALT {
        return;
    }

    // LOD: choose zoom from camera altitude; rebuild the patch when it changes.
    // Hysteresis: the boundary must be crossed by a margin so hovering can't thrash.
    let cur = stream.current_zoom;
    let desired = zoom_for_altitude(altitude, base_zoom);
    let confirmed = if desired < cur {
        zoom_for_altitude(altitude - ZOOM_HYSTERESIS_M, base_zoom) <= desired
    } else if desired > cur {
        zoom_for_altitude(altitude + ZOOM_HYSTERESIS_M, base_zoom) >= desired
    } else {
        false
    };
    if confirmed {
        tracing::info!(from = cur, to = desired, altitude, "map LOD zoom changed");
        stream.current_zoom = desired;
    }

    // Camera world position (tile centres are derived from it at every zoom).
    let world_x = cell.x as f32 * WORLD_CELL_EDGE + tf.translation.x;
    let world_z = cell.z as f32 * WORLD_CELL_EDGE + tf.translation.z;

    // Desired tiles: a clipmap pyramid. Level 0 is the finest (altitude-driven)
    // ring around the camera; each level beneath halves the zoom, doubling the
    // covered extent, down to MIN_ZOOM whose ring reaches the horizon (~20 km).
    // Coarser levels sit slightly lower so finer tiles draw on top — no seams or
    // hole-cutting, and part-loaded levels are backstopped by the level beneath.
    // A tile (tx,ty) centre sits at world ((tx+0.5 - fcx)*gs, (ty+0.5 - fcy)*gs)
    // relative to the fixed anchor, so the map never shifts across zoom changes.
    let mut wanted: HashMap<TileKey, f32> = HashMap::new();
    // Spawn/fetch order: coarsest zoom first, so a fresh anchor (teleport or
    // globe descent) gets full — if blurry — ground coverage after a handful of
    // fetches, refined by the finer rings as they land.
    let mut spawn_order: Vec<TileKey> = Vec::new();
    for zoom in MIN_ZOOM..=stream.current_zoom {
        let gs = tile_to_meters(zoom, clat) as f32;
        let (fcx, fcy) = lat_lng_to_tile_frac(clat, clng, zoom);
        let ctx = (world_x as f64 / gs as f64 + fcx - 0.5).round() as i64;
        let cty = (world_z as f64 / gs as f64 + fcy - 0.5).round() as i64;
        let radius = if zoom == stream.current_zoom {
            LOAD_RADIUS
        } else {
            COARSE_RADIUS
        };
        // Height is a fixed function of the *zoom*, not the pyramid level: level
        // indices shift when altitude changes the finest zoom, and cached tiles
        // keep their spawn height — zoom-keyed heights mean a given tile is
        // always at the same y, so coarser imagery can never end up on top.
        let y = -0.03 - 0.4 * base_zoom.saturating_sub(zoom) as f32;
        stream
            .quads
            .entry(zoom)
            .or_insert_with(|| meshes.add(Cuboid::new(gs, 0.04, gs)));
        for b in -radius..=radius {
            for a in -radius..=radius {
                let key = TileKey::new(ctx + a, cty + b, zoom);
                wanted.insert(key.clone(), y);
                spawn_order.push(key);
            }
        }
    }

    // Spawn newly-needed tiles (coarse-first, see spawn_order above).
    let gen = stream.anchor_generation;
    let mut queued = 0u32;
    for key in &spawn_order {
        let y = wanted[key];
        if stream.loaded.contains_key(key) {
            continue;
        }
        let Some(quad) = stream.quads.get(&key.z).cloned() else {
            continue;
        };
        let gs = tile_to_meters(key.z, clat) as f32;
        let (fcx, fcy) = lat_lng_to_tile_frac(clat, clng, key.z);
        let wx = ((key.x as f64 + 0.5 - fcx) * gs as f64) as f32;
        let wz = ((key.y as f64 + 0.5 - fcy) * gs as f64) as f32;
        // Place under the BigSpace so the tile rebases with the camera (any range).
        let world = Vec3::new(wx, y, wz);
        let (tile_cell, local) =
            Grid::new(WORLD_CELL_EDGE, WORLD_SWITCH_THRESHOLD).translation_to_grid(world.as_dvec3());
        let backstop = key.z == MIN_ZOOM;
        let mut tile = commands.spawn((
            Mesh3d(quad.clone()),
            MeshMaterial3d(placeholder.clone()),
            Transform::from_translation(local),
            tile_cell,
            ChildOf(root),
            // Flat tiles + big_space rebasing confuse frustum culling; opt out.
            NoFrustumCulling,
            StreamTile { key: key.clone() },
            // Hidden from orbit alongside the ground disc (ADR-025).
            crate::systems::globe::HideInSpace,
            // Untextured tiles start hidden so the coarser level shows through
            // (see `MapBackstop`). `data_layers::apply_feature_toggles` is the sole
            // visibility writer for a `StreamTile` and keeps them that way until
            // `StreamTileReady` lands — this only avoids a one-frame grey flash
            // before that system next runs.
            if backstop { Visibility::Inherited } else { Visibility::Hidden },
        ));
        if backstop {
            tile.insert(MapBackstop);
        }
        let entity = tile.id();
        stream.loaded.insert(key.clone(), entity);
        if stream.requested.insert(key.clone()) {
            spawn_map_tile(
                key.clone(),
                gen,
                template.clone(),
                stream.results.clone(),
                stream.gen_shared.clone(),
            );
            queued += 1;
        }
    }
    // Terse fetch accounting: one line per burst, not per tile (served from the
    // on-disk cache when already seen; only new tiles hit the network).
    if queued > 0 {
        tracing::info!(queued, finest_zoom = stream.current_zoom, "tile fetches queued");
    }

    // Re-queue failed fetches after a short backoff (bounded attempts, so a
    // transient network error or rate limit doesn't leave permanent grey tiles).
    let now = Instant::now();
    let retry: Vec<TileKey> = stream
        .failed
        .iter()
        .filter(|(k, (attempts, last))| {
            *attempts < MAX_FETCH_ATTEMPTS
                && last.elapsed().as_secs() >= 3
                && stream.loaded.contains_key(*k)
        })
        .map(|(k, _)| k.clone())
        .collect();
    for key in retry {
        if let Some((_, last)) = stream.failed.get_mut(&key) {
            *last = now;
        }
        // A retry is a fetch in flight like any other, so it goes back into
        // `requested` — that is what `apply_stream_textures` matches its arriving
        // result against before keeping it.
        stream.requested.insert(key.clone());
        spawn_map_tile(
            key,
            gen,
            template.clone(),
            stream.results.clone(),
            stream.gen_shared.clone(),
        );
    }

    // Despawn tiles that drifted out of range or left the pyramid after a LOD
    // change (texture bytes stay cached).
    let stale: Vec<TileKey> = stream
        .loaded
        .keys()
        .filter(|k| !wanted.contains_key(*k))
        .cloned()
        .collect();
    for key in stale {
        if let Some(entity) = stream.loaded.remove(&key) {
            // Queue rather than despawn inline: a LOD zoom-out retires a whole
            // ring at once, and the per-frame drain keeps the teardown bounded.
            stream.despawn_queue.push(entity);
        }
        // Clear the fetch bookkeeping: the material/texture is dropped with the
        // entity, so a tile that comes back into range (pan back, or a LOD
        // zoom-in restoring a finer ring) has to fetch again. Leaving the key in
        // `requested` suppressed that job and the re-spawned tile kept the grey
        // placeholder permanently — the "tiles stop loading" instability.
        stream.requested.remove(&key);
        stream.failed.remove(&key);
    }
}

/// Apply fetched textures to streamed tiles as they arrive.
pub fn apply_stream_textures(
    mut commands: Commands,
    mut stream: ResMut<MapStream>,
    mut images: ResMut<Assets<Image>>,
    mut materials: ResMut<Assets<StandardMaterial>>,
    tiles: Query<(Entity, &StreamTile), (Without<StreamTileReady>, Without<StreamTileFailed>)>,
    time: Res<Time>,
    mut next_report: Local<f64>,
) {
    let results_arc = stream.results.clone();
    let Ok(mut results) = results_arc.lock() else {
        return;
    };

    // Decode and upload run on different clocks now (worker pool vs. the render
    // frame), so "the map is filling slowly" has two very different causes and the
    // fix differs: a small `decoded` backlog with tiles still awaiting means the
    // CPU side is behind, while a large backlog means uploads are — capped at
    // MAX_UPLOADS_PER_FRAME *per frame*, so a slow frame rate throttles the map
    // even when every tile has long since decoded. Reporting one number could not
    // tell those apart, and guessing wrong sent us optimising the wrong half.
    // Throttle on the clock, not on a frame count: the whole point is to diagnose a
    // build whose frames are seconds long, where "every N frames" is minutes apart.
    let now = time.elapsed_secs_f64();
    if now >= *next_report {
        *next_report = now + 2.0;
        tracing::info!(
            decoded_pending = results.len(),
            awaiting_upload = tiles.iter().count(),
            loaded = stream.loaded.len(),
            "tile fill"
        );
    }
    // Drop results nobody will ever consume — they hold a full decoded mip chain
    // each (~1.4 MB at 512², mips included), so a leak here grows without bound as
    // you roam. Two ways to become unconsumable, and the second was leaking:
    //
    // * the tile was **retired** before its fetch landed (entity gone). A tile
    //   re-entering range re-queues its own fetch, so nothing is lost.
    // * a **second** fetch for a still-loaded tile landed after the first one had
    //   already textured it. Two can be in flight at once — a retry racing the
    //   attempt it gave up on, or a pre-re-anchor task for a key the new anchor
    //   wants too — and the loser's mip chain then sat in this map forever. A
    //   settled cold start was holding ~150 of these.
    //
    // `requested` is exactly "a fetch is in flight whose result is still wanted",
    // which answers both.
    results.retain(|key, _| stream.loaded.contains_key(key) && stream.requested.contains(key));

    // Cap texture finalizations per frame: a fast pan can land dozens of decoded
    // tiles at once, and uploading them all in one frame hitches. The rest keep
    // their decoded data queued (Without<StreamTileReady>) and upload next frame.
    const MAX_UPLOADS_PER_FRAME: usize = 8;
    let mut uploaded = 0;
    for (entity, tile) in tiles.iter() {
        if uploaded >= MAX_UPLOADS_PER_FRAME {
            break;
        }
        // Take ownership so the decoded mip chain is freed once uploaded (leaving
        // it in `results` would grow the map unboundedly as you move).
        let Some(outcome) = results.remove(&tile.key) else {
            continue;
        };
        // The fetch is no longer in flight. `failed` takes over the bookkeeping for
        // anything the retry pump still owes, so `streaming_busy` stays honest —
        // this is what lets it fall to false at all, and with it `--settle`.
        stream.requested.remove(&tile.key);
        match outcome {
            None => {
                // Fetch/decode failed. Record the failure so the retry pump in
                // update_map_stream can re-queue it; only after the attempt budget
                // is spent do we give up and leave the coarser level showing.
                let attempts = {
                    let entry = stream
                        .failed
                        .entry(tile.key.clone())
                        .or_insert((0, Instant::now()));
                    entry.0 += 1;
                    entry.1 = Instant::now();
                    entry.0
                };
                if attempts >= MAX_FETCH_ATTEMPTS {
                    stream.failed.remove(&tile.key);
                    commands.entity(entity).insert(StreamTileFailed);
                }
            }
            Some(decoded) => {
                // Decode + mipmaps already done off-thread; just assemble and upload.
                let bevy_image = crate::systems::rendering::image_from_mip_chain(
                    decoded.width,
                    decoded.height,
                    decoded.data,
                    decoded.mip_count,
                    TextureFormat::Rgba8UnormSrgb,
                    RenderAssetUsages::default(),
                    ImageSampler::linear(),
                );
                let handle = images.add(bevy_image);
                let mat = materials.add(StandardMaterial {
                    base_color_texture: Some(handle),
                    // Matte, like the placeholder — no grazing-angle sheen.
                    perceptual_roughness: 1.0,
                    reflectance: 0.0,
                    ..default()
                });
                commands.entity(entity).insert(MeshMaterial3d(mat));
                stream.failed.remove(&tile.key);
                commands.entity(entity).insert(StreamTileReady);
                uploaded += 1;
            }
        }
    }
}
