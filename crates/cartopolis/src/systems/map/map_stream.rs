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

use crate::components::FreeCamera;
use crate::components::{MapBackstop, StreamTileFailed, StreamTileReady};
use crate::grid::{WORLD_CELL_EDGE, WORLD_SWITCH_THRESHOLD};
use std::collections::{HashMap, HashSet};
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::{Arc, Mutex};

use bevy::camera::visibility::NoFrustumCulling;
use bevy::prelude::*;
use bevy::render::render_resource::TextureFormat;
use bevy::tasks::IoTaskPool;
use bevy_image::Image;
use big_space::prelude::{BigSpace, Grid, GridCell};

use crate::platform::fetch_gate;
use crate::platform::stream::{CellStream, Policy};
use crate::resources::OsmTileUrlTemplate;
use cartopolis_core::world::{lat_lng_to_tile, tile_to_lat_lng, tile_to_meters};
use cartopolis_core::TileKey;

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
///
/// Re-exported from `cartopolis_core`, where it lives next to the integer
/// [`cartopolis_core::lat_lng_to_tile`] it used to duplicate verbatim. Kept visible here
/// because a dozen call sites reach for it through this module, and because the
/// routing graph needs it *without* needing the streamer — that dependency was the
/// last non-doc tie from `systems::geo` to a Bevy module.
pub use cartopolis_core::lat_lng_to_tile_frac;

/// Tiles to keep loaded in each direction from the camera at the finest zoom
/// (radius 4 → 9×9).
const LOAD_RADIUS: i64 = 4;
/// Ring radius for each coarser clipmap level beneath the finest (radius 3 → 7×7,
/// covering twice the previous level's extent per step).
const COARSE_RADIUS: i64 = 3;

/// Lowest (coarsest) zoom of the clipmap pyramid; its ring is what fills the view
/// out to the horizon (7×7 tiles at zoom 10 ≈ 165 km).
const MIN_ZOOM: u32 = 10;
/// How far the streamed clipmap reaches from the anchor, in metres — the radius
/// of the coarsest ring's square, measured to the middle of its outermost tile.
///
/// Public because the globe's quadtree needs it: between `GLOBE_DRAW_ALT` and
/// `GLOBE_SHOW_ALT` both tiers are drawn, and everything inside this radius is
/// painted by the clipmap at a zoom the globe cannot reach. Refining the planet
/// under it is work whose entire output is hidden — a hundred-odd tiles fetched,
/// rasterised, uploaded and drawn behind an opaque map.
///
/// Conservative on purpose (the middle of the outer tile, not its far edge): the
/// cost of stopping the split slightly too early is one coarse globe tile
/// showing through at the very rim of the clipmap, which the clipmap's own
/// backstop level covers anyway.
#[must_use]
pub fn clipmap_reach_m(anchor_lat: f64) -> f64 {
    COARSE_RADIUS as f64 * tile_to_meters(MIN_ZOOM, anchor_lat)
}

/// Give up on a tile fetch after this many failed attempts (leaves the placeholder).
const MAX_FETCH_ATTEMPTS: u32 = 4;
/// Wait between attempts, so a transient error or a rate limit doesn't burn the
/// whole attempt budget in four frames.
const FETCH_RETRY_SECS: f64 = 3.0;
/// Concurrent tile fetch+decode tasks.
///
/// This streamer had **no** bound at all: it spawned one detached task per
/// newly-wanted tile, so a fresh anchor or a re-anchor released the whole z10–z17
/// clipmap — 424 requests — in a single frame. It was the only tile streamer
/// without one, and the cost is two-sided. A burst that size is what trips the tile
/// server's rate limiting (`globe`'s `QT_FETCH_BUDGET` comment records the same
/// lesson), and every *finished* tile holds a full 512² mip chain (~1.4 MB) until
/// the render loop uploads it at `MAX_UPLOADS_PER_FRAME` — so an unbounded burst
/// also decides how much decoded memory is live at once.
///
/// 8 matches `map_geometry::SURFACE_MAX_IN_FLIGHT` and the `platform::workers` pool
/// ceiling, so every worker stays fed while the decoded backlog stays around 11 MB.
/// That 11 MB is the same number the *upload* side had to stop handing over in one
/// frame — see `apply_stream_textures`, where the count is now paired with a byte
/// ceiling, because a bound in items says nothing about the quantity a driver
/// actually chokes on.
const TILE_MAX_IN_FLIGHT: usize = 8;
/// Max ground tiles to despawn per frame — the mass-*despawn* twin of
/// `MAX_UPLOADS_PER_FRAME`. A zoom-out steps the LOD zoom down and retires the
/// whole previous finest-zoom ring at once; freeing all those per-tile textures in
/// one command buffer overran a weak GPU's timeout. Retired tiles are queued and
/// freed at this rate.
const MAX_TILE_DESPAWNS_PER_FRAME: usize = 12;
/// Altitude margin a zoom boundary must be crossed by before switching (anti-thrash).
const ZOOM_HYSTERESIS_M: f32 = 60.0;

/// Altitude above which the streamer stops entirely (see the freeze in
/// [`update_map_stream`]).
///
/// Deliberately **above** `globe::GLOBE_SHOW_ALT`, which is where it used to sit,
/// and the gap is the point. Descending from orbit to a fresh anchor, the flat
/// pyramid began filling at the exact instant the globe handed off to it — so the
/// handoff was onto an empty patch, and the ~10 s cold fill
/// (`docs/notes/tile-raster-cost.md`) played out in full view. The band between
/// the two altitudes buys that fill a head start while the tiles are still hidden
/// by `data_layers::apply_feature_toggles`.
///
/// It costs one ring, not a pyramid: [`zoom_for_altitude`] clamps to [`MIN_ZOOM`]
/// far below this, so anywhere in the prefetch band the desired set is a single
/// 7×7 level — 49 tiles, not the 424 of a full clipmap. That is load-bearing
/// enough to be a test (`prefetch_band_is_one_coarse_ring`), because a future edit
/// to the zoom ladder could quietly turn this into a 424-tile fetch for every
/// place the camera drifts over in orbit. The original freeze exists to stop
/// exactly that.
const MAP_PREFETCH_ALT: f32 = 250_000.0;

/// Pick a map zoom from camera altitude: detailed near the ground, coarser high up
/// so a wide area is visible cheaply. Capped at the region's base zoom.
fn zoom_for_altitude(altitude_m: f32, base_zoom: u32) -> u32 {
    // Above the globe threshold nothing here is on screen — the globe is
    // (`data_layers::apply_feature_toggles` hides every `StreamTile` in space), and
    // the streamer is only still running to warm the descent (`MAP_PREFETCH_ALT`).
    // Warm one coarse ring and no more. The ladder below bottoms out at
    // `base_zoom - 4`, which for a z17 anchor is z13 and a four-level, 228-tile
    // pyramid — affordable to *hold* while you are standing in it, not to fetch
    // speculatively for every patch of ground an orbiting camera drifts across.
    if altitude_m > crate::systems::globe::GLOBE_SHOW_ALT {
        return MIN_ZOOM;
    }
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

/// Marker for a streamed ground tile, carrying its key.
///
/// The streamer itself works key-first (`MapStream::loaded` maps key → entity),
/// but `apply_terrain_to_tiles` walks the tiles the other way and needs to know
/// which one it is holding to sample the field under it.
#[derive(Component)]
pub struct StreamTile {
    pub key: TileKey,
}

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
    /// Bumped whenever the standing tiles have to be re-fetched — an anchor
    /// change, or a basemap source change ([`apply_aerial_source`]). This is what
    /// [`Self::gen_shared`] carries, so a stale in-flight fetch drops itself.
    ///
    /// **Separate from [`Self::anchor_generation`] on purpose**, and the
    /// separation is the difference between retearing one layer and retearing the
    /// world: sixteen other systems watch the anchor generation and answer a bump
    /// by rebuilding — every building cell, the terrain field, the interiors, the
    /// dikes, a fresh weather fetch. Flipping a checkbox over the ground texture
    /// must not cost the mass mesh churn CLAUDE.md's device-loss entry is about.
    fetch_generation: u32,
    /// A requested re-anchor target, applied by `apply_reanchor` next frame.
    pending_reanchor: Option<(f64, f64)>,
    /// Finest displayed zoom (driven by camera altitude for LOD); coarser clipmap
    /// levels are derived from it down to `MIN_ZOOM`.
    current_zoom: u32,
    loaded: HashMap<TileKey, Entity>,
    /// The requested/in-flight/failed bookkeeping, the results sink, the
    /// concurrency bound and the bounded despawn queue (`platform::stream`).
    ///
    /// `requested` there is an in-flight-and-served guard, not a permanent "ever
    /// requested" set: a retired tile is *forgotten*, because its texture died with
    /// its entity and a re-entering tile has to fetch again (the byte cache makes
    /// that cheap). Treating it as permanent left every re-streamed tile grey
    /// forever — and it also pinned [`MapStream::streaming_busy`] true for the
    /// whole session, so every `--shot` sat out its full `--wait` instead of
    /// capturing once the map went quiet.
    tiles: CellStream<TileKey, Option<DecodedTile>>,
    /// How many times a tile that was **already loaded** has been armed for
    /// fetching again, cumulative for the process.
    ///
    /// This exists because the device log could not distinguish two very
    /// different worlds. `update_map_stream` calls `tiles.request` for every
    /// desired key each frame, loaded or not, and the burst line below fires
    /// whenever any of them was newly armed — so a log full of "tile fetches
    /// queued" is either a cold ring filling once (fine) or the same 224 tiles
    /// being re-fetched forever (~40-140 ms of scan conversion each, on a worker
    /// pool three threads wide, plus a fresh `Image` and `StandardMaterial`
    /// minted per arrival in `apply_stream_textures`). Both look identical from
    /// outside, and a four-minute capture over a stationary camera showed the
    /// line 30-60 times a second with `loaded` pinned at 224.
    ///
    /// Non-zero and climbing while the camera is still is the second world, and
    /// the cause is upstream of here: either the desired set's `.round()`-ed
    /// centres flickering across a level boundary, or `forget` (which clears the
    /// failure history) re-arming keys faster than the attempt budget can retire
    /// them. Flat is the first world, and then the burst line is only noise.
    requeues: u64,
    /// The live anchor generation a fetch task compares against (main thread
    /// stores, task loads) — a re-anchor's stale in-flight fetches drop themselves.
    gen_shared: Arc<AtomicU32>,
    /// Shared ground-quad mesh per zoom level (each zoom has its own tile size).
    quads: HashMap<u32, Handle<Mesh>>,
    placeholder: Option<Handle<StandardMaterial>>,
    /// The last computed clipmap: each level's centre tile (the cache key) and the
    /// desired set it produced. Rebuilding 424 entries plus a 424-entry spawn order
    /// every frame was pure waste — the desired set only changes when the camera
    /// crosses a tile centre at some level, or the LOD zoom steps.
    desired: DesiredTiles,
    /// Tiles completely painted over by finer, ready ones — see
    /// [`occluded_by_finer`]. Written by [`update_tile_occlusion`], read by
    /// `data_layers::map_tile_want`, which is the sole visibility writer for a
    /// `StreamTile` and would otherwise un-hide them the next frame.
    pub occluded: HashSet<TileKey>,
    /// Bumped whenever [`Self::occluded`] changes, so the gated visibility sweep
    /// has a value to notice. Without it the sweep's `run_if` — a comparison over
    /// `ToggleWants` — would be false on every frame a tile finished streaming,
    /// and the holes would only be cut the next time somebody touched a checkbox.
    pub occlusion_revision: u32,
    /// The aerial-basemap setting the standing tiles were fetched under
    /// (`data_layers::DataLayers::aerial_enabled`), and the single place the tile
    /// path asks about it — `update_map_stream` and `apply_stream_textures` both
    /// read *this* rather than the resource, so a flip mid-frame cannot leave a
    /// tile fetched from one source and recorded as the other.
    ///
    /// `None` until [`apply_aerial_source`] first runs: adopting the setting is
    /// not a change, and a run that starts with the layer on must not retear a
    /// pyramid that was never built under the other source.
    aerial: Option<bool>,
    /// Standing tiles textured from the aerial source — the count `--dump-state`
    /// reports as `aerial_tiles`.
    ///
    /// Mirrors [`Self::loaded`] and must be maintained wherever that is: a stale
    /// key here is a number that says the photograph is up when it is not.
    aerial_ready: HashSet<TileKey>,
}

/// The clipmap's desired set, cached against the centres that produced it.
///
/// Keyed on **every level's** centre tile, not just the finest. The centres are
/// `round()`ed, so the levels' step points are not nested — a coarse ring can cross
/// its own centre line on a frame the finest ring does not, and keying on the
/// finest alone would leave that level's set one tile stale. Computing 8 centres is
/// a handful of flops; it is the 424 hash inserts behind them that were the cost.
#[derive(Default)]
struct DesiredTiles {
    /// `(zoom, centre x, centre y)` per clipmap level, the anchor generation, and
    /// the radius bias `systems::load_control` currently holds — all three decide
    /// which tiles are wanted, so all three have to invalidate the set.
    key: Option<(Vec<(u32, i64, i64)>, u32, i64)>,
    /// Wanted tile → its spawn height.
    wanted: HashMap<TileKey, f32>,
    /// Spawn/fetch order: coarsest zoom first, so a fresh anchor gets full — if
    /// blurry — ground coverage after a handful of fetches.
    spawn_order: Vec<TileKey>,
}

impl MapStream {
    /// Whether any streamed tile has been applied yet — the boot splash's
    /// "the world is visible" signal (`systems::splash`). Exposed as a predicate
    /// rather than the map itself so the field stays private.
    pub fn has_tiles(&self) -> bool {
        !self.loaded.is_empty()
    }

    /// How many tiles the clipmap currently wants — the denominator the boot
    /// splash's progress track counts against.
    ///
    /// Deliberately the *wanted* set and not `loaded`: `loaded` is entities that
    /// have been spawned, which the whole ring reaches within a frame or two of a
    /// fresh anchor while every one of them is still showing the paper
    /// placeholder. A bar that counts those is at 100 % before a single texture
    /// has arrived, which is the one thing a progress bar must not do.
    pub fn wanted_tiles(&self) -> usize {
        self.desired.wanted.len()
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
        self.tiles.busy()
    }

    /// Request a re-anchor to a new (lat, lng); applied by `apply_reanchor`, which
    /// drops the loaded tiles so they re-stream around the camera at the new origin.
    pub fn request_reanchor(&mut self, lat: f64, lng: f64) {
        self.pending_reanchor = Some((lat, lng));
    }

    /// Standing clipmap tiles textured from the aerial source
    /// (`systems::aerial`) — `aerial_tiles` in the `--shot` metrics.
    ///
    /// Textured, not merely wanted: a tile still showing the placeholder has not
    /// answered the question the number is asked for, which is whether the
    /// photograph arrived.
    pub fn aerial_tiles(&self) -> usize {
        self.aerial_ready.len()
    }

    /// Is this tile's imagery coming from the aerial source this frame?
    ///
    /// A pure function of the key and the adopted toggle, which is what lets the
    /// dispatch and the texture-apply agree without a second bookkeeping set
    /// between them: [`apply_aerial_source`] retears everything on a flip, so no
    /// tile can outlive the answer it was fetched under.
    fn aerial_for(&self, key: &TileKey) -> bool {
        self.aerial.unwrap_or(false) && crate::systems::aerial::covers_tile(key)
    }

    /// Retire the current tile fetch generation, so in-flight jobs drop their
    /// results instead of texturing tiles that no longer want them.
    fn bump_fetch_generation(&mut self) {
        self.fetch_generation = self.fetch_generation.wrapping_add(1);
        self.gen_shared
            .store(self.fetch_generation, Ordering::Relaxed);
    }
}

/// Apply a pending re-anchor: despawn every loaded tile (its world position is
/// relative to the old origin), move the anchor, and bump the generation so the
/// globe and buildings rebuild for the new origin. `update_map_stream` re-streams
/// tiles around the camera next frame; the on-disk tile cache keeps it cheap.
pub fn apply_reanchor(
    mut stream: ResMut<MapStream>,
    mut published: ResMut<crate::resources::MapAnchor>,
    tiles: Query<Entity, With<StreamTile>>,
) {
    let Some((lat, lng)) = stream.pending_reanchor.take() else {
        return;
    };
    let base_zoom = stream
        .anchor
        .map(|(_, _, z)| z)
        .unwrap_or(cartopolis_core::world::REGION_ZOOM_LEVEL);
    // Queue the whole old-anchor patch for bounded despawn rather than freeing it
    // all in one frame (mass-despawn device-loss guard; drained by
    // `update_map_stream`). The queued entities are still live StreamTiles, so the
    // query re-captures them — clear any prior queue first to avoid double-frees.
    stream.tiles.clear_despawn_queue();
    let retired: Vec<Entity> = tiles.iter().collect();
    stream.tiles.retire_all(retired);
    stream.loaded.clear();
    stream.aerial_ready.clear();
    // Clear the fetch bookkeeping so the re-streamed tiles request again (served
    // from the byte cache when already fetched); the anchor is unchanged tiles
    // would otherwise be treated as still-in-flight and never re-textured.
    stream.tiles.reset();
    stream.desired = DesiredTiles::default();
    // Quad meshes are sized by tile metres *at the anchor latitude*; stale sizes
    // at a new latitude would leave gaps or overlaps between tiles.
    stream.quads.clear();
    // Anchor on the containing tile's centre (see `snap_anchor`) so tiles,
    // buildings and pins keep sharing one placement convention.
    let (lat, lng) = snap_anchor(lat, lng, base_zoom);
    stream.anchor = Some((lat, lng, base_zoom));
    // Republish. Without this the resource keeps whatever `init_map_stream` set
    // and every consumer places against the *first* anchor for the rest of the
    // run — invisible until you teleport.
    published.geo = stream.anchor;
    stream.current_zoom = base_zoom;
    stream.anchor_generation = stream.anchor_generation.wrapping_add(1);
    // Retire queued fetches from the old anchor: the workers drop any job whose
    // generation doesn't match, so the new anchor's tiles aren't stuck behind
    // hundreds of stale fetches.
    stream.bump_fetch_generation();
    tracing::info!(lat, lng, "map re-anchored");
}

/// Adopt a change to the aerial basemap toggle, retearing the standing tiles so
/// they re-stream from the source that is now selected.
///
/// The tile *fetch* generation only ([`MapStream::fetch_generation`]), never the
/// anchor generation: the anchor has not moved, so nothing about the buildings,
/// the terrain field, the interiors or the weather is stale, and telling them it
/// is would answer one checkbox with a city's worth of mesh churn.
///
/// The teardown is the re-anchor's, minus the parts that belong to an origin
/// change: the quads are sized by tile metres at the anchor latitude and the
/// anchor has not moved, so they are kept. Retired tiles go through the same
/// bounded despawn queue (`MAX_TILE_DESPAWNS_PER_FRAME`) and the re-streamed ones
/// through the same per-frame upload ceiling; a source switch is a re-fill, not a
/// burst with its own rules.
pub fn apply_aerial_source(
    mut stream: ResMut<MapStream>,
    layers: Res<crate::systems::data_layers::DataLayers>,
    tiles: Query<Entity, With<StreamTile>>,
) {
    let want = layers.aerial_enabled;
    // Compared, not `is_changed()`: `DataLayers` is written by a dozen systems a
    // frame, so change detection on the resource says nothing about this field.
    let Some(current) = stream.aerial else {
        stream.aerial = Some(want);
        return;
    };
    if current == want {
        return;
    }
    stream.aerial = Some(want);
    // Same order as `apply_reanchor`, and for the same reason: the query
    // re-collects tiles that a previous teardown already queued, so the queue is
    // cleared first or an entity is freed twice.
    stream.tiles.clear_despawn_queue();
    let retired: Vec<Entity> = tiles.iter().collect();
    stream.tiles.retire_all(retired);
    stream.loaded.clear();
    stream.aerial_ready.clear();
    stream.tiles.reset();
    stream.desired = DesiredTiles::default();
    stream.bump_fetch_generation();
    tracing::info!(aerial = want, "map basemap source changed");
}

impl Default for MapStream {
    fn default() -> Self {
        Self {
            anchor: None,
            anchor_generation: 0,
            fetch_generation: 0,
            pending_reanchor: None,
            current_zoom: 0,
            loaded: HashMap::new(),
            requeues: 0,
            tiles: CellStream::new(Policy {
                max_in_flight: TILE_MAX_IN_FLIGHT,
                min_interval_secs: 0.0,
                max_attempts: MAX_FETCH_ATTEMPTS,
                backoff_secs: FETCH_RETRY_SECS,
                backoff_doublings: 0,
                // No cooldown: once the attempts are spent the grey placeholder is
                // final, until the tile leaves the pyramid and comes back.
                cooldown_secs: 0.0,
                despawns_per_frame: crate::quality::budget().per_frame(MAX_TILE_DESPAWNS_PER_FRAME),
            }),
            gen_shared: Arc::new(AtomicU32::new(0)),
            quads: HashMap::new(),
            placeholder: None,
            desired: DesiredTiles::default(),
            occluded: HashSet::new(),
            occlusion_revision: 0,
            aerial: None,
            aerial_ready: HashSet::new(),
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
    mut published: ResMut<crate::resources::MapAnchor>,
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
    published.geo = stream.anchor;
    stream.current_zoom = z;
    stream.anchor_generation = stream.anchor_generation.wrapping_add(1);
    stream.bump_fetch_generation();
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

    // The two numbers that set the ceiling on how fast the map can fill, and which
    // were invisible while both silently sat at Bevy's default of 4 (see the
    // `TaskPoolPlugin` override in `main`): `io` is how many tile fetches can be in
    // flight, `cpu` how many can rasterise at once.
    tracing::info!(
        io = bevy::tasks::IoTaskPool::get().thread_num(),
        cpu = crate::platform::workers::worker_count(),
        "map streaming initialized"
    );
}

/// Spawn one ground-tile fetch+decode task (ADR-033 seam): gen-tagged so a
/// re-anchor's stale in-flight fetch drops itself, then decode + mip-chain off the
/// main thread and hand the finished pixels back for the render loop to upload.
fn spawn_map_tile(
    key: TileKey,
    gen: u32,
    template: String,
    results: Arc<Mutex<Vec<(TileKey, Option<DecodedTile>)>>>,
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
                // Stale (re-anchored while queued): drop it before the fetch. The
                // miss goes into whichever sink we were handed, which after a
                // re-anchor is the orphan `CellStream::reset` left behind — the new
                // anchor's bookkeeping is already clear, so a still-wanted tile
                // re-requests on its own.
                if let Ok(mut g) = results.lock() {
                    g.push((key, None));
                }
                return;
            }
            // The clipmap's raster size, read here rather than threaded in — it is a
            // `OnceLock` load, and this is the one place that decides how much of
            // the phone's memory the 224-tile ring costs. 512² RGBA plus mips is
            // ~1.4 MB a tile; 256² is a quarter of that, and the globe already
            // measured 512² as 2-3× oversampled per axis for the same reason
            // (`docs/notes/globe-tile-cost.md`). Scan conversion is ~area, so it is
            // also most of the time-to-first-map.
            let tile_px = crate::quality::budget().tile_px;
            match crate::systems::tile_loader::fetch_tile_payload(&key, &template).await {
                Ok(payload) => {
                    crate::platform::workers::submit("map_tiles", Box::new(move || {
                        let decoded = match payload.decode(tile_px) {
                            Ok(rgba) => {
                                let (width, height) = (rgba.width(), rgba.height());
                                let (data, mip_count) =
                                    crate::systems::mip::build_mip_chain(
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
                            g.push((key, decoded));
                        }
                    }));
                }
                Err(e) => {
                    tracing::warn!(tile = %key.to_path(), error = %e, "map tile load failed");
                    if let Ok(mut g) = results.lock() {
                        g.push((key, None));
                    }
                }
            }
        })
        .detach();
}

/// Resting height of a clipmap level's tiles, relative to the ground plane.
///
/// A fixed function of the *zoom*, not the pyramid level: level indices shift when
/// altitude changes the finest zoom, and cached tiles keep their spawn height —
/// zoom-keyed heights mean a given tile is always at the same y, so coarser
/// imagery can never end up on top.
fn level_y(base_zoom: u32, zoom: u32) -> f32 {
    -0.03 - 0.4 * base_zoom.saturating_sub(zoom) as f32
}

/// Marker on a tile whose flat quad has been replaced by ground lifted onto the
/// terrain field, carrying the field revision it was built for.
#[derive(Component)]
pub struct TileRelief {
    revision: u32,
}

/// Subdivisions per side for a relief tile of `tile_m` metres: one quad per
/// terrain sample, so the mesh carries exactly the measurements the field holds
/// and not one vertex more.
///
/// This has to be a function of the tile, not a constant. The clipmap's finest
/// zoom follows altitude — over a Dutch city it is z17, a **184 m** tile, not the
/// ~1.5 km one a fixed 48 was sized for — and the same constant is applied to all
/// four levels that qualify for relief. That put 4,608 triangles on a tile the
/// field describes with 7 samples across: ~8× over-subdivided per axis, 64× in
/// triangles, interpolating detail that was never measured. Measured on the
/// Groningen anchor (2026-07-31, `--offline`, settled, 60 m): the basemap went
/// from 3k triangles flat to **942k** with relief, the shadow pass from 1.07M to
/// 2.96M, and the frame rate from ~26 fps to 10–11 — which is also what made the
/// egui panels feel broken, the click bridge spending one pass per pointer event.
///
/// Sizing off [`TERRAIN_SAMPLE_M`] instead keeps every level agreeing with the
/// *same* field to within one interpolation step. That matters beyond the triangle
/// count: relieved levels are stacked only 0.4 m apart, so a coarse level that
/// approximated the ground more crudely than the fine one above it would poke
/// through it on any slope.
fn relief_divisions(tile_m: f32) -> usize {
    // A backstop on one mesh, not a quality knob — and deliberately above what
    // [`RELIEF_MAX_TILE_M`] can ask for (~100), because a level that subdivided
    // *more* coarsely than the one above it is the poke-through this rule exists
    // to prevent. The field is 256 samples across, so nothing inside it can
    // honestly ask for more than that either.
    const MAX_DIVISIONS: usize = 128;
    ((tile_m / crate::systems::terrain::TERRAIN_SAMPLE_M).ceil() as usize).clamp(1, MAX_DIVISIONS)
}

/// Largest tile that gets relief, in metres. Coarser levels of the clipmap span
/// tens of kilometres against a 6.4 km field: a grid that coarse could only draw a
/// blocky caricature of the same ground the fine tiles carry properly, and it
/// would poke through them.
const RELIEF_MAX_TILE_M: f32 = 2_500.0;

/// Relief meshes built per frame. A burst of mesh uploads in one command buffer is
/// the documented device-loss pattern (see CLAUDE.md), and the cost is per *build*
/// regardless of how big each one is.
const MAX_RELIEF_BUILDS_PER_FRAME: usize = 2;

/// Curvature-only tile meshes built per frame — the cheap half of the same
/// guard. A curve grid is at most 8×8 vertices (see
/// `globe::curvature_divisions`), so a frame's worth is a few kilobytes rather
/// than the megabytes a relief grid moves, and the whole clipmap can take its
/// shape inside a second instead of over a minute.
const MAX_CURVE_BUILDS_PER_FRAME: usize = 16;

/// Give the basemap tiles their shape: the Earth's curve under all of them, and
/// the terrain field under the ones it reaches.
///
/// **The curve is the reason the flat tier and the globe are one surface.** Every
/// tile drops by `GlobeState::drop_at` at its own centre and carries the
/// remainder across itself in vertices, so the clipmap is a patch of the same
/// sphere the quadtree draws — 528 m down at the coarsest ring's edge, which is
/// what turns a plane that ended in a hard rim into ground that falls away to a
/// horizon. It costs one small grid per coarse tile and nothing at all on the
/// fine levels, whose own sag is millimetres.
///
/// The terrain half has two parts and the second is not optional. The finest
/// tiles inside the field get a subdivided, displaced mesh. **Every coarser
/// level then sinks** below the field's lowest point, because the clipmap stacks
/// levels only 0.4 m apart: a dip of eight metres — routine in a Dutch polder —
/// would otherwise drop the detailed tile *underneath* the coarse imagery it is
/// supposed to be refining, and the hollow would fill with blurry z10 pixels.
/// The curve does not have that problem and is not treated the same way: it is
/// applied to every level equally, so their spacing is preserved exactly.
pub fn apply_terrain_to_tiles(
    mut commands: Commands,
    mut meshes: ResMut<Assets<Mesh>>,
    stream: Res<MapStream>,
    field: Res<crate::systems::terrain::TerrainField>,
    globe: Res<crate::systems::globe::GlobeState>,
    mut tiles: Query<(Entity, &StreamTile, &mut Transform, Option<&TileRelief>)>,
    mut last_revision: Local<u32>,
) {
    let Some((clat, clng, base_zoom)) = stream.anchor else {
        return;
    };
    let revision = field.revision;
    let sink = if field.is_ready() {
        // A metre of clearance under the deepest hollow the field knows about.
        field.range_m.0.min(0.0) - 1.0
    } else {
        0.0
    };
    // The curve is keyed to the globe build, not to the terrain field: a
    // re-anchor rebuilds the sphere in a new tangent frame, which moves every
    // tile's drop. Folded into the same revision the relief meshes already carry
    // so one comparison retires stale geometry from either cause.
    let revision = revision.wrapping_mul(0x9E37_79B9) ^ globe.build_revision();

    let mut built = 0;
    let mut curved = 0;
    for (entity, tile, mut tf, relief) in tiles.iter_mut() {
        let gs = tile_to_meters(tile.key.z, clat) as f32;
        // Where this tile's centre sits in the anchored world frame — the same
        // arithmetic that placed it, so the mesh samples the field under itself.
        let (fcx, fcy) = lat_lng_to_tile_frac(clat, clng, tile.key.z);
        let centre = Vec2::new(
            ((tile.key.x as f64 + 0.5 - fcx) * gs as f64) as f32,
            ((tile.key.y as f64 + 0.5 - fcy) * gs as f64) as f32,
        );
        let relieved = field.is_ready() && gs <= RELIEF_MAX_TILE_M && {
            // Tiles wholly outside the field are flat ground as far as relief
            // is concerned; they may still need the curve.
            let reach = crate::systems::terrain::TERRAIN_HALF_M + gs * 0.5;
            centre.x.abs() <= reach && centre.y.abs() <= reach
        };
        // **The whole pyramid drops together.** Every level takes the curve at its
        // own centre, so the 0.4 m that separates them is preserved exactly and a
        // fine tile can no more sink into a coarse one than it could on the flat
        // plane. That is why there is no "curve only past N km" threshold here:
        // a boundary between curved and uncurved levels is precisely the step
        // this is removing.
        let curve = globe.drop_at(centre.length());
        // Coarse levels: sit below the deepest part of the relief. Fine ones keep
        // their own resting height and carry the displacement in the mesh.
        let want_y = level_y(base_zoom, tile.key.z) + if relieved { 0.0 } else { sink } - curve;
        if (tf.translation.y - want_y).abs() > 1e-3 {
            tf.translation.y = want_y;
        }

        // Subdivision is the union of what the two displacements ask for: the
        // field wants a vertex per measurement, the curve wants one per
        // `MAX_CURVE_SAG_M`. A tile that needs neither keeps the shared flat quad,
        // which is still most of them — the finest levels are under 3.5 km across
        // and their own sag is millimetres.
        let curve_divs = crate::systems::globe::curvature_divisions(gs, globe.radius);
        let divisions = if relieved {
            relief_divisions(gs).max(curve_divs)
        } else {
            curve_divs
        };
        let shaped = relieved || divisions > 1;

        if !shaped {
            if relief.is_some() {
                // The field went away (layer off, out of coverage, re-anchored to
                // Berlin) and the tile is small enough to be flat: back to the
                // shared quad.
                if let Some(quad) = stream.quads.get(&tile.key.z) {
                    commands
                        .entity(entity)
                        .insert(Mesh3d(quad.clone()))
                        .remove::<TileRelief>()
                        .remove::<bevy::light::NotShadowCaster>();
                }
            }
            continue;
        }
        if relief.is_some_and(|r| r.revision == revision) {
            continue;
        }
        // Two budgets, because the two meshes are three orders of magnitude
        // apart: a relief grid follows a 25 m field and can run to 128 divisions,
        // while a curve-only grid is at most 7 and exists to hide a quarter-metre
        // of sag. Charging them to one counter meant either throttling the cheap
        // one to two a frame — 75 frames to curve a cold clipmap, visible as the
        // horizon unbending after the map has arrived — or letting the expensive
        // one through in bursts, which is the documented device-loss pattern.
        let cost = if relieved { &mut built } else { &mut curved };
        let cap = if relieved {
            MAX_RELIEF_BUILDS_PER_FRAME
        } else {
            MAX_CURVE_BUILDS_PER_FRAME
        };
        if *cost >= crate::quality::budget().per_frame(cap) {
            continue;
        }
        *cost += 1;
        let mesh = crate::systems::terrain::displaced_grid(&field, &globe, centre, gs, divisions);
        commands
            .entity(entity)
            .insert(Mesh3d(meshes.add(mesh)))
            .insert(TileRelief { revision })
            // The ground does not cast: four clipmap levels of relief are drawn
            // again into every shadow cascade, which measured as ~2.5× the mesh's
            // own cost (`ktris_shadow` 1.07M flat → 2.96M) — for self-shadowing
            // that a 25 m field can barely express and that shows up mostly as
            // acne. `lod22` gives up distant building shadows for the same reason.
            // The trade is real and deliberate: a hill no longer darkens the
            // valley behind it at a low sun. Over Dutch relief (±2 m) that is
            // invisible; if the Veluwe ever needs it, it belongs to the finest
            // level alone, not to all four.
            .insert(bevy::light::NotShadowCaster);
    }
    if built + curved > 0 && *last_revision != revision {
        *last_revision = revision;
        tracing::info!(
            revision,
            sink_m = sink,
            curve_m = globe.drop_at(clipmap_reach_m(clat) as f32),
            "shaped the basemap"
        );
    }
}

/// The clipmap's desired tiles: every level's ring around its own centre, with the
/// spawn height each tile takes, plus the coarse-first order to fetch them in.
///
/// Split out of [`update_map_stream`] because this is the arithmetic that decides
/// the fan-out — 424 tiles for a z10–z17 pyramid — and the number is worth being
/// able to assert (see the tests).
///
/// `budget` is a parameter rather than a `quality::budget()` read for exactly that
/// reason: the fan-out is now a function of the tier, and a test that could only
/// see the tier of whichever machine ran it could not state either number.
fn clipmap(
    centres: &[(u32, i64, i64)],
    base_zoom: u32,
    finest_zoom: u32,
    budget: &crate::quality::Budget,
    ring_bias: i64,
) -> (HashMap<TileKey, f32>, Vec<TileKey>) {
    let mut wanted: HashMap<TileKey, f32> = HashMap::new();
    let mut spawn_order: Vec<TileKey> = Vec::new();
    for &(zoom, ctx, cty) in centres {
        // `budget.radius` and not `load_control::radius`, because `budget` is a
        // parameter here precisely so a test can state the fan-out for a tier it
        // is not running on. The controller's bias is folded in by the caller
        // (`ring_bias`), which keeps this function a pure function of its
        // arguments.
        let radius = (budget.radius(if zoom == finest_zoom {
            LOAD_RADIUS
        } else {
            COARSE_RADIUS
        }) - ring_bias)
            .max(1);
        let y = level_y(base_zoom, zoom);
        // Within a level, nearest-first. The fetches are queued in this order and
        // the pools drain it roughly FIFO, so the ground under the camera is the
        // first thing to sharpen; row-major from a corner instead put the tile you
        // are standing on ~40th of 81 at the finest zoom, which is most of a fill
        // spent watching the periphery resolve.
        let mut level: Vec<(i64, TileKey)> =
            Vec::with_capacity(((2 * radius + 1) * (2 * radius + 1)) as usize);
        for b in -radius..=radius {
            for a in -radius..=radius {
                let tile = TileKey::new(ctx + a, cty + b, zoom);
                wanted.insert(tile, y);
                level.push((a * a + b * b, tile));
            }
        }
        level.sort_by_key(|(d2, _)| *d2);
        spawn_order.extend(level.into_iter().map(|(_, tile)| tile));
    }
    (wanted, spawn_order)
}

/// [`fetch_gate::NEAR`] for a tile in the 3×3 around its own level's centre, and
/// [`fetch_gate::ANY`] for the rest of the clipmap.
///
/// Two bands rather than a full distance, for the reason the call site gives. A
/// tile whose level is not in `centres` — which cannot happen, since the desired
/// set is built from them — answers `ANY`, the conservative side.
fn tile_rank(tile: TileKey, centres: &[(u32, i64, i64)]) -> fetch_gate::Rank {
    centres
        .iter()
        .find(|(zoom, _, _)| *zoom == tile.z)
        .map_or(fetch_gate::ANY, |&(_, ctx, cty)| {
            if fetch_gate::ring_distance((tile.x, tile.y), ctx, cty) <= fetch_gate::NEAR {
                fetch_gate::NEAR
            } else {
                fetch_gate::ANY
            }
        })
}

/// Which tiles are completely painted over by finer, ready ones — and can
/// therefore be hidden without changing a single pixel.
///
/// **The clipmap is eight opaque layers deep under the camera**, and nothing was
/// cutting holes in it. Each level is a ring around its own centre and the levels
/// nest, so the ground you are standing on is covered by z10 *through* z17 at once,
/// stacked 0.4 m apart. `Opaque3d` is a `BinnedPhaseItem` in Bevy 0.19 — binned by
/// pipeline and asset, not sorted front-to-back — and every tile has its own
/// material, so the draw order is effectively arbitrary; there is no depth prepass
/// in this tree either. Early-Z therefore rejects nothing reliably and all eight
/// textured fragments are shaded, per ground pixel, on a device whose scarcest
/// resource is fill.
///
/// The rule is recursive and the recursion is the point: a tile covers its
/// footprint if it is ready *or* if all four of its children cover theirs. So a
/// coarse tile may be hidden behind a child that is itself hidden behind
/// grandchildren, which is exactly the eight-deep case. Working from the finest
/// zoom down makes it one pass.
///
/// `tiles` maps every live tile to whether it has imagery. Returns the subset to
/// hide; the caller is responsible for never hiding a `MapBackstop` (see
/// `data_layers::map_tile_want`).
fn occluded_by_finer(
    tiles: &HashMap<TileKey, bool>,
    finest: u32,
    coarsest: u32,
) -> HashSet<TileKey> {
    let mut covering: HashSet<TileKey> = HashSet::new();
    let mut occluded: HashSet<TileKey> = HashSet::new();
    // The finest level has no children, so it covers exactly where it is ready.
    for (&key, &ready) in tiles {
        if key.z == finest && ready {
            covering.insert(key);
        }
    }
    for z in (coarsest..finest).rev() {
        for (&key, &ready) in tiles {
            if key.z != z {
                continue;
            }
            let all_children_cover = [(0, 0), (1, 0), (0, 1), (1, 1)].iter().all(|(dx, dy)| {
                covering.contains(&TileKey::new(key.x * 2 + dx, key.y * 2 + dy, z + 1))
            });
            if all_children_cover {
                occluded.insert(key);
                covering.insert(key);
            } else if ready {
                covering.insert(key);
            }
        }
    }
    occluded
}

/// Recompute which clipmap tiles are hidden behind finer ones.
///
/// Ordered after `apply_stream_textures`, so a tile that gained its imagery this
/// frame is counted this frame rather than the next.
///
/// Runs every frame and rebuilds the map from scratch, which is affordable and
/// deliberately simpler than the alternative: the whole pyramid is ~424 entries, so
/// this is a few thousand hash operations against a saving of several *screens* of
/// fragment shading. The scratch map is a `Local` so the steady state allocates
/// nothing. An incremental scheme would have to track despawns, re-anchors and LOD
/// zoom steps as well as readiness edges, and each of those is a way to leave a
/// stale hole in the basemap.
pub fn update_tile_occlusion(
    mut stream: ResMut<MapStream>,
    tiles: Query<(&StreamTile, Has<StreamTileReady>)>,
    mut live: Local<HashMap<TileKey, bool>>,
) {
    live.clear();
    live.extend(tiles.iter().map(|(tile, ready)| (tile.key, ready)));
    let finest = stream.current_zoom;
    if finest < MIN_ZOOM {
        return; // no pyramid yet (no anchor)
    }
    let occluded = occluded_by_finer(&live, finest, MIN_ZOOM);
    // Compared before it is written: this is a `ResMut`, and assigning
    // unconditionally would mark `MapStream` changed on every frame of the run for
    // every reader that watches it.
    if occluded != stream.occluded {
        stream.occluded = occluded;
        stream.occlusion_revision = stream.occlusion_revision.wrapping_add(1);
    }
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
    time: Res<Time>,
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
    stream.tiles.drain_despawns(&mut commands);
    let now = time.elapsed_secs_f64();

    // Freeze streaming from orbit (ADR-025): high above, the detailed tiles are
    // hidden anyway (crate::components::HideInSpace), so keep the pyramid loaded
    // instead of despawning it — returning to the surface is then instant, and the
    // space camera doesn't spam tile requests for wherever it roams.
    // Geocentric altitude, NOT local y: orbiting to the far side of the globe
    // drives local y negative, which would unfreeze streaming and fetch tiles
    // for nonsense coordinates (real bug: it hammered the tile server).
    //
    // The threshold is `MAP_PREFETCH_ALT`, not the globe's own show altitude:
    // between the two the streamer runs while its tiles are still hidden, so a
    // descent from orbit lands on a warm coarse ring instead of on nothing. See
    // that constant for why the band cannot become expensive.
    let altitude = globe.altitude_m(cell, tf) as f32;
    if altitude > MAP_PREFETCH_ALT {
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
    // Cheap every frame: each level's centre tile, plus its shared quad mesh.
    let mut centres: Vec<(u32, i64, i64)> = Vec::with_capacity(8);
    for zoom in MIN_ZOOM..=stream.current_zoom {
        let gs = tile_to_meters(zoom, clat) as f32;
        let (fcx, fcy) = lat_lng_to_tile_frac(clat, clng, zoom);
        let ctx = (world_x as f64 / gs as f64 + fcx - 0.5).round() as i64;
        let cty = (world_z as f64 / gs as f64 + fcy - 0.5).round() as i64;
        centres.push((zoom, ctx, cty));
        stream
            .quads
            .entry(zoom)
            .or_insert_with(|| meshes.add(Cuboid::new(gs, 0.04, gs)));
    }

    // Expensive, and only when the answer can have changed: the 424-entry desired
    // set and its spawn order. See `DesiredTiles` on why the key is every level's
    // centre rather than just the finest.
    // The controller's bias is part of the key, not just an argument: it changes
    // which tiles are wanted, so a change to it has to re-derive the desired set
    // exactly as a re-anchor does. Left out, the ring would narrow only the next
    // time the camera crossed a cell boundary.
    let ring_bias = crate::systems::load_control::bias();
    let key = (centres, stream.fetch_generation, ring_bias);
    if stream.desired.key.as_ref() != Some(&key) {
        let (wanted, spawn_order) = clipmap(
            &key.0,
            base_zoom,
            stream.current_zoom,
            crate::quality::budget(),
            ring_bias,
        );
        stream.desired = DesiredTiles {
            key: Some(key),
            wanted,
            spawn_order,
        };
    }

    // Spawn newly-needed tiles and ask for their imagery, coarse-first (see
    // `DesiredTiles::spawn_order`). Indexed rather than iterated because the body
    // mutably borrows `stream`; `TileKey` is `Copy`, so reading one out costs
    // nothing and no 424-entry clone is needed.
    let gen = stream.fetch_generation;
    // The level centres, for the rank below. Eight entries, cloned out because the
    // loop body borrows `stream` mutably.
    let level_centres: Vec<(u32, i64, i64)> = stream
        .desired
        .key
        .as_ref()
        .map(|(centres, _, _)| centres.clone())
        .unwrap_or_default();
    for i in 0..stream.desired.spawn_order.len() {
        let key = stream.desired.spawn_order[i];
        let y = stream.desired.wanted[&key];
        // **Only the 3×3 under the camera is ranked near; everything else keeps
        // its place.** The spawn order here is coarse-level-first on purpose (a
        // blurry whole ground beats a sharp patch of it), and ranking the full
        // 424 by distance would overturn that decision — which was made against a
        // measurement and this one was not. What the rank does buy is the global
        // `fetch_gate`'s near reserve for the tiles a person is actually looking
        // at, and since equal ranks keep arrival order, the coarse-first sequence
        // survives inside both bands.
        let rank = tile_rank(key, &level_centres);
        // `CellStream::request_ranked` de-dups and is also the retry pump — a
        // failed tile waits out `FETCH_RETRY_SECS` and stops being offered once
        // its attempts are spent, so there is no second retry loop.
        let armed = stream.tiles.request_ranked(key, rank, now);
        if stream.loaded.contains_key(&key) {
            // Armed a tile we already hold: see `MapStream::requeues`. The fetch
            // itself is left alone deliberately — this is an instrument, not a
            // guard, and suppressing the re-fetch here would hide the cause while
            // changing behaviour.
            if armed {
                stream.requeues += 1;
            }
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
        let (tile_cell, local) = Grid::new(WORLD_CELL_EDGE, WORLD_SWITCH_THRESHOLD)
            .translation_to_grid(world.as_dvec3());
        let backstop = key.z == MIN_ZOOM;
        let mut tile = commands.spawn((
            Mesh3d(quad.clone()),
            MeshMaterial3d(placeholder.clone()),
            Transform::from_translation(local),
            tile_cell,
            ChildOf(root),
            // Flat tiles + big_space rebasing confuse frustum culling; opt out.
            NoFrustumCulling,
            StreamTile { key },
            // Hidden from orbit alongside the ground disc (ADR-025).
            crate::components::HideInSpace,
            // Untextured tiles start hidden so the coarser level shows through
            // (see `MapBackstop`). `data_layers::apply_feature_toggles` is the sole
            // visibility writer for a `StreamTile` and keeps them that way until
            // `StreamTileReady` lands — this only avoids a one-frame grey flash
            // before that system next runs.
            if backstop {
                Visibility::Inherited
            } else {
                Visibility::Hidden
            },
        ));
        if backstop {
            tile.insert(MapBackstop);
        }
        let entity = tile.id();
        stream.loaded.insert(key, entity);
    }

    // Dispatch under TILE_MAX_IN_FLIGHT: the coarse-first queue order is preserved,
    // and what doesn't fit starts on a later frame as slots free.
    let mut queued = 0u32;
    {
        let results = stream.tiles.sink();
        let live_gen = stream.gen_shared.clone();
        let mut jobs: Vec<TileKey> = Vec::new();
        while let Some(key) = stream.tiles.next_job(now) {
            jobs.push(key);
        }
        for key in jobs {
            // The **only** place a consumer changes basemap source. `map_geometry`,
            // `globe`, `navigation` and the router keep reading
            // `OsmTileUrlTemplate`: they want MVT geometry, not pixels, and the
            // globe would be asking a national service for a planet.
            let template = if stream.aerial_for(&key) {
                crate::systems::aerial::template().to_string()
            } else {
                template.clone()
            };
            spawn_map_tile(key, gen, template, results.clone(), live_gen.clone());
            queued += 1;
        }
    }
    // Terse fetch accounting: one line per burst, not per tile (served from the
    // byte cache when already seen; only new tiles hit the network).
    //
    // `debug!`, not `info!`, and the level is the point. This fires once per
    // frame per burst, which on a device is 30-60 lines a second for minutes —
    // and `cartopolis=info` is a build-time filter on Android (`RUST_LOG` is
    // unreachable there), so it swamped the one log a user can actually send:
    // a four-minute capture was ~90% this line, and the LOD change, the scatter
    // cap and the avatar counts had to be found between the repeats. What is
    // worth reporting steadily is the *aggregate*, which the `tile fill` line
    // below now carries — including `requeued`, the number that says whether
    // these bursts are a ring filling or the same ring refetching.
    if queued > 0 {
        tracing::debug!(
            queued,
            finest_zoom = stream.current_zoom,
            "tile fetches queued"
        );
    }

    // Despawn tiles that drifted out of range or left the pyramid after a LOD
    // change (texture bytes stay cached).
    let stale: Vec<TileKey> = stream
        .loaded
        .keys()
        .filter(|k| !stream.desired.wanted.contains_key(*k))
        .copied()
        .collect();
    for key in stale {
        stream.aerial_ready.remove(&key);
        if let Some(entity) = stream.loaded.remove(&key) {
            // Queue rather than despawn inline: a LOD zoom-out retires a whole
            // ring at once, and the per-frame drain keeps the teardown bounded.
            stream.tiles.retire(entity);
        }
        // Clear the fetch bookkeeping: the material/texture is dropped with the
        // entity, so a tile that comes back into range (pan back, or a LOD
        // zoom-in restoring a finer ring) has to fetch again. Leaving the key
        // `requested` suppressed that job and the re-spawned tile kept the grey
        // placeholder permanently — the "tiles stop loading" instability.
        stream.tiles.forget(&key);
    }
}

/// Apply fetched textures to streamed tiles as they arrive.
pub fn apply_stream_textures(
    mut commands: Commands,
    mut stream: ResMut<MapStream>,
    mut images: ResMut<Assets<Image>>,
    mut materials: ResMut<Assets<StandardMaterial>>,
    tiles: Query<
        Entity,
        (
            With<StreamTile>,
            Without<StreamTileReady>,
            Without<StreamTileFailed>,
        ),
    >,
    time: Res<Time>,
    mut next_report: Local<f64>,
) {
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
            in_flight = stream.tiles.in_flight(),
            awaiting_upload = tiles.iter().count(),
            loaded = stream.loaded.len(),
            requeued = stream.requeues,
            "tile fill"
        );
    }

    // Cap texture finalizations per frame: a fast pan can land dozens of decoded
    // tiles at once, and uploading them all in one frame hitches. The rest go back
    // into the sink — still holding their concurrency slots, so the bound also
    // bounds how much decoded mip chain is live — and upload next frame.
    //
    // `docs/notes/tile-raster-cost.md` measured this bound as never binding on a
    // workstation and said to leave it alone. That measurement is about *fill
    // latency*, and the phone tier is trading exactly that for smoothness: each
    // finalization is a texture upload issued from the only thread the web build
    // runs anything on, so halving the burst is halving the worst-case hitch when
    // a pan lands a batch.
    const MAX_UPLOADS_PER_FRAME: usize = 8;
    let uploads = crate::quality::budget().per_frame(MAX_UPLOADS_PER_FRAME);
    let mut ready = stream.tiles.take_results();

    // …and the same bound in **bytes**, which is the one that decides whether a
    // mobile driver survives the frame.
    //
    // The count above is not a byte budget and never was, and on this rung the two
    // are not close. A finished tile at `tile_px = 512` is a fixed-size RGBA8 mip
    // chain — 1,365 kB every time, since the raster size is the tier's, not the
    // content's — so `8` is not "eight tiles' worth", it is *always* 10.9 MB. On
    // 2026-08-19 an Adreno 840 was measured queueing exactly that from a cold start
    // (`frame_kb = 10922`, repeatedly) against a 4 MB frame budget, and the same
    // figure reproduces here headlessly on `CARTO_QUALITY=android`. CLAUDE.md's
    // rule — bound it in bytes, not only in items — was written for `lod22` and
    // applied there; this is the streamer that dominates startup, and it was still
    // counting items.
    //
    // The backlog is the damage, and it is arithmetic rather than inference: bevy
    // writes ~4 MB of what it is handed and re-queues the rest, so a frame that
    // queues 10.9 MB adds ~7 MB to a pending queue that only drains at 4, for as
    // long as tiles keep landing — with every deferred tile's decoded mip chain
    // resident behind it. What that cost the device is the weaker claim and is left
    // as one: the same run went to 2-second frames, `dequeueBuffer` timeouts and a
    // lost Vulkan device, which is consistent with this and not proven by it.
    //
    // `quality::upload_ceiling` and not the `RenderAssetBytesPerFrame` resource,
    // and the difference is one rule: that resource carries the scripted-capture
    // exemption, which belongs to *bevy's* budget and not to this one. A deferred
    // bevy write is invisible to a settle test that watches streamers; a deferred
    // tile is a streamer, counted by `awaiting_upload` and waited on as
    // `map_tiles`. Inheriting the exemption meant a `--shot` run measured a code
    // path the handset never takes — which is how the first version of this fix
    // read as doing nothing. `None` — every rung above the phone — leaves this
    // exactly as it was.
    //
    // **Always at least one tile**, whatever the ceiling says. A tile larger than
    // the whole frame's budget must still be uploaded or it is deferred forever
    // and the map never fills — the same "one whole asset per frame" rule bevy
    // itself uses, and for the same reason.
    //
    // Through `stream_ledger` since 2026-08-20, and the difference is the word
    // *shared*: this loop used to keep its own tally against the same ceiling,
    // which bounded the tiles and said nothing about the frame — `lod22`, the
    // surfaces and the far buildings each spent their own caps beside it and the
    // frame queued 27 MB with every individual bound in force. The ledger is that
    // tally lifted out to where every streamer draws from it.
    let mut take = 0usize;
    for (_, outcome) in ready.iter() {
        if take >= uploads {
            break;
        }
        let cost = outcome.as_ref().map_or(0, |tile| tile.data.len());
        if !crate::systems::stream_ledger::admit(cost) {
            break;
        }
        take += 1;
    }

    let deferred = ready.split_off(take);
    stream.tiles.defer(deferred);
    for (key, outcome) in ready {
        // Retired before its fetch landed: the entity is gone, so nobody will ever
        // consume this mip chain. A tile re-entering range re-fetches, so dropping
        // it loses nothing. (`update_map_stream` already forgot the key.)
        let Some(&entity) = stream.loaded.get(&key) else {
            continue;
        };
        match outcome {
            None => {
                // Fetch/decode failed. `CellStream` holds the backoff and the
                // attempt budget; only once it is spent do we give up and leave
                // the coarser level showing.
                stream.tiles.failed(&key, now);
                if stream.tiles.exhausted(&key) {
                    commands.entity(entity).insert(StreamTileFailed);
                }
            }
            Some(decoded) => {
                // Decode + mipmaps already done off-thread; just assemble and upload.
                let bevy_image = crate::systems::tile_loader::image_from_mip_chain(
                    decoded.width,
                    decoded.height,
                    decoded.data,
                    decoded.mip_count,
                    TextureFormat::Rgba8UnormSrgb,
                    crate::systems::tile_loader::TILE_TEXTURE_USAGE,
                    crate::systems::tile_loader::ground_sampler(),
                );
                let handle = images.add(bevy_image);
                let mat = materials.add(StandardMaterial {
                    base_color_texture: Some(handle),
                    // Matte, like the placeholder — no grazing-angle sheen.
                    perceptual_roughness: 1.0,
                    reflectance: 0.0,
                    ..default()
                });
                commands
                    .entity(entity)
                    .insert((MeshMaterial3d(mat), StreamTileReady));
                if stream.aerial_for(&key) {
                    stream.aerial_ready.insert(key);
                }
                stream.tiles.succeeded(&key);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The fan-out tests state the *desktop* pyramid, so they say so rather than
    /// reading whatever tier the machine running them resolved to.
    fn desktop() -> crate::quality::Budget {
        crate::quality::Budget::desktop()
    }

    /// All four children ready hides the parent; three does not.
    ///
    /// The asymmetry is the whole safety property. Hiding a tile that is *not*
    /// fully covered punches a hole through to whatever is under it — at the
    /// coarsest level, to the sky — and a hole in the basemap is a much worse
    /// outcome than the overdraw this exists to remove. So the test that matters
    /// is the negative one.
    #[test]
    fn a_tile_hides_only_when_every_child_covers_it() {
        let parent = TileKey::new(10, 20, 14);
        let children = |ready: usize| {
            let mut tiles = HashMap::from([(parent, true)]);
            for (i, (dx, dy)) in [(0, 0), (1, 0), (0, 1), (1, 1)].iter().enumerate() {
                tiles.insert(
                    TileKey::new(parent.x * 2 + dx, parent.y * 2 + dy, 15),
                    i < ready,
                );
            }
            tiles
        };
        assert!(occluded_by_finer(&children(4), 15, 14).contains(&parent));
        for partial in 0..4 {
            assert!(
                !occluded_by_finer(&children(partial), 15, 14).contains(&parent),
                "{partial} ready children must not hide the parent"
            );
        }
    }

    /// A missing child is not a covering child. The clipmap's coarse levels span a
    /// wider area than the fine ones, so most coarse tiles have no children in the
    /// live set at all — and reading "no children" as "all children cover" would
    /// hide the entire horizon.
    #[test]
    fn a_tile_with_no_children_at_all_stays_visible() {
        let lone = TileKey::new(3, 4, 12);
        let tiles = HashMap::from([(lone, true)]);
        assert!(occluded_by_finer(&tiles, 17, 10).is_empty());
    }

    /// Coverage is transitive: a z14 tile hidden behind its own hidden children is
    /// still covering, so *its* parent may hide too. This is the eight-deep stack
    /// the clipmap actually builds, and a non-recursive rule would only ever
    /// remove one layer of it.
    #[test]
    fn coverage_passes_through_a_hidden_generation() {
        // z12 → four z13 → sixteen z14. Only the z14 leaves carry imagery.
        let mut tiles = HashMap::new();
        let root = TileKey::new(2, 2, 12);
        tiles.insert(root, true);
        for dx in 0..2 {
            for dy in 0..2 {
                let mid = TileKey::new(root.x * 2 + dx, root.y * 2 + dy, 13);
                tiles.insert(mid, false); // no imagery of its own
                for ex in 0..2 {
                    for ey in 0..2 {
                        tiles.insert(TileKey::new(mid.x * 2 + ex, mid.y * 2 + ey, 14), true);
                    }
                }
            }
        }
        let hidden = occluded_by_finer(&tiles, 14, 12);
        assert!(
            hidden.contains(&root),
            "the root must hide behind two levels"
        );
        assert_eq!(hidden.len(), 5, "the root and its four unpainted children");
    }

    /// The relief grid must follow the tile, not a constant. A fixed 48 was sized
    /// for a ~1.5 km tile and then applied to the 184 m ones the clipmap actually
    /// streams over a city, which is where the 942k-triangle basemap came from.
    #[test]
    fn relief_grid_matches_the_field_not_the_zoom() {
        let sample = crate::systems::terrain::TERRAIN_SAMPLE_M;
        assert!(
            (sample - 25.1).abs() < 0.2,
            "the field is a 25 m grid; this test's numbers assume it: {sample}"
        );

        // The four clipmap levels that qualify for relief at Dutch latitudes.
        for (tile_m, want) in [(184.0, 8), (368.0, 15), (736.0, 30), (1471.0, 59)] {
            assert_eq!(
                relief_divisions(tile_m),
                want,
                "a {tile_m} m tile wants one quad per sample"
            );
        }

        // Across the whole relief range, a quad is never coarser than a sample (the
        // level above would poke through it) and never much finer (geometry paid
        // for detail nothing measured). The cap must not bite inside this range.
        for tile_m in [50.0, 184.0, 400.0, 1471.0, RELIEF_MAX_TILE_M] {
            let quad_m = tile_m / relief_divisions(tile_m) as f32;
            assert!(
                quad_m <= sample,
                "{tile_m} m tile: {quad_m} m quads under-describe the field"
            );
            assert!(
                quad_m > sample * 0.5,
                "{tile_m} m tile: {quad_m} m quads interpolate detail nothing measured"
            );
        }

        // And the finest tile is an order of magnitude cheaper than the old fixed
        // grid — the frame-rate regression this fixes.
        let tris = |d: usize| d * d * 2;
        assert!(
            tris(relief_divisions(184.0)) * 30 < tris(48),
            "the z17 tile should be >30x cheaper than the old fixed 48"
        );
    }

    /// A tile far coarser than the field still yields a bounded mesh.
    #[test]
    fn relief_grid_is_bounded() {
        assert_eq!(relief_divisions(1.0e6), 128);
        assert_eq!(relief_divisions(0.0), 1);
    }

    use crate::systems::vector_tiles::SubTile;

    /// Each clipmap level's centre tile for a camera sitting on the anchor — the
    /// same arithmetic `update_map_stream` runs every frame.
    fn centres(lat: f64, lng: f64, finest_zoom: u32) -> Vec<(u32, i64, i64)> {
        (MIN_ZOOM..=finest_zoom)
            .map(|zoom| {
                let (fcx, fcy) = lat_lng_to_tile_frac(lat, lng, zoom);
                (
                    (zoom),
                    (fcx - 0.5).round() as i64,
                    (fcy - 0.5).round() as i64,
                )
            })
            .collect()
    }

    /// The fan-out, stated so a change to a radius or a zoom has to restate it.
    ///
    /// A fresh anchor opens at [`DEFAULT_ANCHOR_ZOOM`] and the pyramid runs
    /// `MIN_ZOOM..=17`: the finest ring is `LOAD_RADIUS` (9×9 = 81) and the seven
    /// coarser ones are `COARSE_RADIUS` (7×7 = 49 each), so **424** tiles are
    /// wanted at once. That burst is what `TILE_MAX_IN_FLIGHT` now meters out and
    /// what used to be released in a single frame.
    #[test]
    fn a_fresh_anchor_wants_424_tiles() {
        let centres = centres(DEFAULT_ANCHOR_LAT, DEFAULT_ANCHOR_LNG, DEFAULT_ANCHOR_ZOOM);
        let (wanted, order) = clipmap(
            &centres,
            DEFAULT_ANCHOR_ZOOM,
            DEFAULT_ANCHOR_ZOOM,
            &desktop(),
            0,
        );
        assert_eq!(order.len(), 424, "the clipmap fan-out changed");
        assert_eq!(
            wanted.len(),
            order.len(),
            "a tile was wanted twice in one pass"
        );

        // Coarsest first: a fresh anchor gets full, blurry coverage from a handful
        // of fetches before the finer rings refine it.
        assert_eq!(order.first().unwrap().z, MIN_ZOOM);
        assert_eq!(order.last().unwrap().z, DEFAULT_ANCHOR_ZOOM);
    }

    /// …and what the phone tier asks for instead.
    ///
    /// One step off each radius: the finest ring is 7×7 = 49 and the seven coarser
    /// ones are 5×5 = 25 each, so **224**. Stated because it is the single largest
    /// item in the phone budget — a bit over half the ground draw list, and the
    /// same fraction off the tile fetches, MVT decodes and rasterisations behind
    /// it. If this number ever creeps back up, the frame budget on the device goes
    /// with it.
    #[test]
    fn the_phone_tier_wants_224() {
        let centres = centres(DEFAULT_ANCHOR_LAT, DEFAULT_ANCHOR_LNG, DEFAULT_ANCHOR_ZOOM);
        let (wanted, order) = clipmap(
            &centres,
            DEFAULT_ANCHOR_ZOOM,
            DEFAULT_ANCHOR_ZOOM,
            &crate::quality::Budget::phone(),
            0,
        );
        assert_eq!(order.len(), 224, "the phone-tier clipmap fan-out changed");
        assert_eq!(wanted.len(), order.len());

        // The ordering guarantee is a property of the clipmap, not of the tier:
        // coarse-first still has to hold, or a phone gets the slowest possible
        // fill on top of the smallest one.
        assert_eq!(order.first().unwrap().z, MIN_ZOOM);
        assert_eq!(order.last().unwrap().z, DEFAULT_ANCHOR_ZOOM);
    }

    /// The prefetch band must stay one coarse ring, whatever the zoom ladder does.
    ///
    /// `MAP_PREFETCH_ALT` unfreezes the streamer well above the globe threshold so
    /// a descent lands on warm tiles. That is only affordable because
    /// `zoom_for_altitude` has bottomed out at `MIN_ZOOM` long before, making the
    /// desired set a single 7×7 level. Re-band the ladder so it hasn't, and the
    /// same code silently fetches a whole 424-tile pyramid for every patch of
    /// ground an orbiting camera drifts across — which is the load the freeze was
    /// put there to prevent.
    #[test]
    fn prefetch_band_is_one_coarse_ring() {
        for base_zoom in [MIN_ZOOM, 14, DEFAULT_ANCHOR_ZOOM] {
            for altitude in [
                crate::systems::globe::GLOBE_SHOW_ALT + 1.0,
                MAP_PREFETCH_ALT * 0.5,
                MAP_PREFETCH_ALT,
            ] {
                assert_eq!(
                    zoom_for_altitude(altitude, base_zoom),
                    MIN_ZOOM,
                    "at {altitude} m with base zoom {base_zoom} the prefetch band \
                     is no longer clamped to the coarsest level"
                );
            }
        }

        // …and that one level really is a single ring, not a pyramid. It is the
        // finest level in this band as well as the coarsest, so it gets
        // `LOAD_RADIUS`: 9×9 = 81 tiles against a full clipmap's 424.
        let centres = centres(DEFAULT_ANCHOR_LAT, DEFAULT_ANCHOR_LNG, MIN_ZOOM);
        let (_, order) = clipmap(&centres, DEFAULT_ANCHOR_ZOOM, MIN_ZOOM, &desktop(), 0);
        assert_eq!(order.len(), 81, "the prefetch ring changed size");
    }

    /// …and how few *bodies* those 424 requests actually are.
    ///
    /// Shortbread stops at `vector_tiles::MAX_SOURCE_ZOOM` = 14, so every request
    /// above it resolves to a z14 ancestor. The 179 requests at z15/z16/z17 land
    /// entirely inside the z14 ring the same pass is already asking for, which
    /// leaves **245** distinct source tiles — 49 per zoom for z10–z14 and nothing
    /// new above. Those 179 duplicates were 179 independent disk reads (or, on the
    /// web build, network fetches) and 179 independent MVT parses before
    /// `tile_source` coalesced them.
    #[test]
    fn the_overzoomed_requests_collapse_onto_a_few_dozen_source_tiles() {
        let centres = centres(DEFAULT_ANCHOR_LAT, DEFAULT_ANCHOR_LNG, DEFAULT_ANCHOR_ZOOM);
        let (_, order) = clipmap(
            &centres,
            DEFAULT_ANCHOR_ZOOM,
            DEFAULT_ANCHOR_ZOOM,
            &desktop(),
            0,
        );

        let sources: std::collections::HashSet<(u32, i64, i64)> = order
            .iter()
            .map(|k| SubTile::resolve(k.z, k.x, k.y).0)
            .collect();
        assert!(sources.len() < order.len(), "no requests were deduplicated");
        assert_eq!(sources.len(), 245, "the source-tile fan-in changed");

        let overzoomed = order
            .iter()
            .filter(|k| k.z > crate::systems::vector_tiles::MAX_SOURCE_ZOOM)
            .count();
        assert_eq!(
            overzoomed, 179,
            "the overzoomed share of the pyramid changed"
        );
        // Every one of them resolves onto a tile the z14 ring already wants, so
        // they add no distinct body at all.
        let below: std::collections::HashSet<(u32, i64, i64)> = order
            .iter()
            .filter(|k| k.z <= crate::systems::vector_tiles::MAX_SOURCE_ZOOM)
            .map(|k| (k.z, k.x, k.y))
            .collect();
        assert_eq!(
            sources, below,
            "an overzoomed request needed a new source tile"
        );
    }
}
