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
use std::sync::{Arc, Mutex, RwLock};

use bevy::camera::primitives::Aabb;

/// Half-height of a clipmap tile's stated bound, in metres.
///
/// The bound cannot be derived — see the spawn site — so it is stated, and it is
/// stated generously because the two failure modes are not symmetric. Too large
/// keeps a tile that could have been culled, which costs one draw. Too small
/// culls ground somebody is looking at, which is a hole in the world.
///
/// 500 m against a measured 39.9 m of relief over Groningen, on a tile 1.5 km
/// across at z14 and larger at every coarser level.
const TILE_AABB_Y_MARGIN_M: f32 = 500.0;
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
/// It costs the roots and nothing more: above `globe::GLOBE_SHOW_ALT`
/// `update_map_stream` caps the selection at [`MIN_ZOOM`], so anywhere in the
/// prefetch band the desired set is the one 9×9 coarse ring — 81 tiles. That is
/// load-bearing enough to be a test (`prefetch_band_is_one_coarse_ring`), because
/// refining here would fetch fine ground for every place an orbiting camera drifts
/// over. The original freeze exists to stop exactly that.
const MAP_PREFETCH_ALT: f32 = 250_000.0;

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
///
/// Also what [`MapStream::warm`] keeps after a tile is retired, which is why it
/// is `Clone`: this is the finished form, so a tile that comes back into range
/// can be rebuilt from one without touching the tile server, the MVT parser or
/// tiny-skia.
#[derive(Clone)]
struct DecodedTile {
    data: Vec<u8>,
    width: u32,
    height: u32,
    mip_count: u32,
}

impl crate::platform::warm::WarmSize for DecodedTile {
    fn warm_bytes(&self) -> usize {
        // The mip chain is the whole of it — the other three fields are its
        // shape. A 512² RGBA8 chain is 1,398,100 bytes every time, because the
        // raster size is the tier's and not the content's.
        self.data.len()
    }
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
    /// Rasterised tiles kept after they were retired, so panning back, zooming
    /// back in or shooting a second viewpoint over the same city re-uploads a
    /// texture instead of rebuilding it.
    ///
    /// **This streamer is where the time goes.** `docs/notes/tile-raster-cost.md`
    /// attributes ~29 s of the ~38 s of worker time in a cold Groningen fill to
    /// `map_tiles` scan conversion, and measures a fully warm *disk* cache as
    /// buying only 2 s of a 27.8 s fill — the bytes were never what was
    /// expensive. Half the tier's whole system-memory allowance goes here for
    /// that reason; `map_geometry` takes the other half.
    warm: crate::platform::warm::WarmStore<TileKey, DecodedTile>,
    /// Tiles served from [`Self::warm`] since the last report, for the `tile
    /// fill` line. A counter rather than a read of the store's own total,
    /// because the interesting number is the rate during a burst.
    warm_served: u32,
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
    /// The tiles the clipmap wants *now*, readable from a fetch or decode task.
    ///
    /// The generation says a tile is from the wrong anchor; this says it is from
    /// the right one and nobody wants it any more. A fly-over retires a ring's
    /// worth of tiles while their fetches are in flight (`forget` frees the slot
    /// at once but cannot stop the task), and without this each one still spends a
    /// worker on a ~50 ms decode and mip chain whose result is dropped on arrival
    /// — work that queues in front of the tiles the camera is now over.
    wanted_shared: Arc<RwLock<HashSet<TileKey>>>,
    /// Shared ground-quad mesh per zoom level (each zoom has its own tile size).
    quads: HashMap<u32, Handle<Mesh>>,
    placeholder: Option<Handle<crate::systems::ground_detail::GroundMaterial>>,
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

/// What decides the desired set — see [`DesiredTiles::key`].
type DesiredKey = (Vec<(u32, i64, i64)>, u32, i64, [i32; 5]);

/// The basemap's desired set, cached against the camera pose that produced it.
///
/// Keyed on **every level's** centre tile as well as the pose. The centres are
/// `round()`ed, so the levels' step points are not nested — a coarse level can
/// cross its own centre line on a frame the finest does not — and the rank and the
/// cross-fade are measured from them.
#[derive(Default)]
struct DesiredTiles {
    /// `(zoom, centre x, centre y)` per level, the anchor generation, the radius
    /// bias `systems::load_control` currently holds, and the camera pose quantised
    /// (position, altitude, yaw, pitch) — all four decide which tiles are wanted,
    /// so all four have to invalidate the set.
    key: Option<DesiredKey>,
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

    /// Ground tiles served from the warm store instead of being rebuilt, over
    /// the life of the process, and what the store is holding right now.
    ///
    /// For `--dump-state`, which is where a `cargo shots` manifest run can state
    /// whether its second viewpoint over the same city re-rasterised the ground
    /// or re-uploaded it. Colour is not evidence on this container's software
    /// rasteriser; a count is.
    pub fn warm_stats(&self) -> (u32, usize) {
        (self.warm.hits(), self.warm.bytes())
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
        .unwrap_or(cartopolis_core::world::DEFAULT_TILE_ZOOM);
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
    if let Ok(mut shared) = stream.wanted_shared.write() {
        shared.clear();
    }
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
    retear_tiles(&mut stream, tiles.iter());
    tracing::info!(aerial = want, "map basemap source changed");
}

/// Retire every standing tile and re-stream it, without touching the anchor.
///
/// The teardown half of [`apply_aerial_source`], lifted out because a cache reset
/// (`systems::cache_reset`) wants exactly the same thing for a different reason:
/// the bytes behind these tiles are gone and the pictures on screen are the last
/// evidence of them. Sharing it is what keeps "re-stream the basemap" one
/// sequence — the order below is load-bearing and there should not be two copies
/// of it to keep in step.
///
/// `clear_despawn_queue` comes first for the reason `apply_reanchor` gives: the
/// caller's query re-collects tiles a previous teardown already queued, so
/// without it an entity is freed twice. The quads are deliberately kept — they
/// are sized by tile metres at the anchor latitude, and the anchor has not moved.
pub(crate) fn retear_tiles(stream: &mut MapStream, tiles: impl Iterator<Item = Entity>) {
    stream.tiles.clear_despawn_queue();
    let retired: Vec<Entity> = tiles.collect();
    stream.tiles.retire_all(retired);
    stream.loaded.clear();
    stream.aerial_ready.clear();
    stream.tiles.reset();
    stream.desired = DesiredTiles::default();
    if let Ok(mut shared) = stream.wanted_shared.write() {
        shared.clear();
    }
    stream.bump_fetch_generation();
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
            // Half the tier's allowance: this streamer and `map_geometry` are
            // the two that hold anything worth holding, and this is the one the
            // measurement points at. Integer-halved, so a `0` allowance (the
            // phone rung, `CARTO_WARM_MB=0`) stays off.
            warm: crate::platform::warm::WarmStore::new(crate::quality::warm_ceiling() / 2),
            warm_served: 0,
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
            wanted_shared: Arc::new(RwLock::new(HashSet::new())),
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

/// The anchor the map opens on: the default point **snapped to its tile centre**,
/// like every other anchor (`snap_anchor`).
///
/// It was the raw point until 2026-09-26, which cost nothing while every teleport
/// re-anchored — the first jump replaced it with a snapped one. Since short jumps
/// keep the frame (`geo_nav::SHORT_JUMP_M`), a session that starts anywhere
/// within 3 km of the Grote Markt — a restored location, a place opened from a
/// chat, every `--shot` over Groningen — keeps this anchor for good, and every
/// building (`lat_lng_to_local`, relative to the anchor *tile's centre*) stood
/// 51 m east and 73 m south of the map under it.
fn bootstrap_anchor() -> (f64, f64, u32) {
    let (lat, lng) = snap_anchor(DEFAULT_ANCHOR_LAT, DEFAULT_ANCHOR_LNG, DEFAULT_ANCHOR_ZOOM);
    (lat, lng, DEFAULT_ANCHOR_ZOOM)
}

pub fn init_map_stream(
    mut stream: ResMut<MapStream>,
    mut published: ResMut<crate::resources::MapAnchor>,
    mut materials: ResMut<Assets<crate::systems::ground_detail::GroundMaterial>>,
    _tile_url: Res<OsmTileUrlTemplate>,
) {
    if stream.anchor.is_some() {
        return;
    }
    // Bootstrap anchor (movable, not region-derived): the map opens here, then
    // `user_store` teleports a returning user to their last location, which
    // re-anchors. Regions are retired (ADR-020/029), so there is no seed region
    // to read a location from.
    let (clat, clng, z) = bootstrap_anchor();
    stream.anchor = Some((clat, clng, z));
    published.geo = stream.anchor;
    stream.current_zoom = z;
    stream.anchor_generation = stream.anchor_generation.wrapping_add(1);
    stream.bump_fetch_generation();
    // The basemap's own background tone, not a neutral grey: only the coarsest ring
    // ever shows this (see `MapBackstop`), and there it should read as blank paper
    // continuous with the tiles around it.
    const PAPER: u32 = crate::systems::vector_tiles::palette::PAPER;
    stream.placeholder = Some(
        materials.add(crate::systems::ground_detail::GroundMaterial {
            base: StandardMaterial {
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
            },
            // Zero, not opted out: `refresh_ground_detail` fills it in when a tier lands.
            // The placeholder is only ever the coarsest ring, where the box does not
            // reach — but it costs nothing to let it, and one rule is fewer than two.
            extension: default(),
        }),
    );

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

/// Whether a tile task's result would still be used: from the live anchor and
/// still in the clipmap's wanted set. A poisoned lock answers "yes" — decoding a
/// tile nobody needs is a waste, dropping one somebody does is a hole.
fn tile_still_wanted(
    key: TileKey,
    gen: u32,
    live_gen: &AtomicU32,
    wanted: &RwLock<HashSet<TileKey>>,
) -> bool {
    gen == live_gen.load(Ordering::Relaxed) && wanted.read().map_or(true, |w| w.contains(&key))
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
    wanted: Arc<RwLock<HashSet<TileKey>>>,
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
                Ok(_) if !tile_still_wanted(key, gen, &live_gen, &wanted) => {
                    // Retired or re-anchored while the fetch was in flight. Nothing
                    // is listening for this key any more (`forget` already gave its
                    // slot back), so push nothing: the decode below is the expensive
                    // half and its result would be discarded.
                }
                Ok(payload) => {
                    // A tile drawn on an earlier visit is read here, where the
                    // storage read can be awaited, and decoded on the worker below
                    // in place of drawing it again (`tile_loader::rendered_cache_key`).
                    let cache_key =
                        crate::systems::tile_loader::rendered_cache_key(&key, &payload, tile_px);
                    let stored = match &cache_key {
                        Some(k) => {
                            crate::platform::storage::get(
                                crate::platform::storage::Namespace::Cache,
                                k,
                            )
                            .await
                        }
                        None => None,
                    };
                    let wanted = wanted.clone();
                    let live_gen = live_gen.clone();
                    crate::platform::workers::submit_ranked("map_tiles", crate::platform::workers::BASEMAP, Box::new(move || {
                        // Checked again on the worker: the job may have waited behind
                        // others for a while since the check above.
                        if !tile_still_wanted(key, gen, &live_gen, &wanted) {
                            return;
                        }
                        let reused = stored.and_then(|png| {
                            crate::systems::tile_loader::decode_rendered(&png, tile_px)
                        });
                        let drawn = match reused {
                            Some(img) => Ok(img),
                            None => {
                                let drawn = payload.decode(tile_px);
                                if let (Ok(img), Some(k)) = (&drawn, &cache_key) {
                                    crate::systems::tile_loader::store_rendered(k, img);
                                }
                                drawn
                            }
                        };
                        let decoded = match drawn {
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

/// The mip bias a clipmap tile reaches at the edge of its own ring.
///
/// **One, and it is derived rather than chosen.** Each level covers twice the ground
/// of the one above it at the same raster size, so its texel density is exactly half:
/// one mip level. A tile sampled at bias 1.0 is therefore sampled at precisely the
/// density its parent carries, which is what makes the two meet without a step. Any
/// other value would be a decision to draw the seam somewhere else rather than to
/// remove it.
const LOD_BLEND_BIAS: f32 = 1.0;

/// Whether the level cross-fade is on at all.
///
/// `CARTO_NO_LOD_BLEND=1` strips it, on the same rule as `CARTO_NO_GROUND_DETAIL`,
/// `CARTO_NO_HAZE` and the rest: this is a *shading* change, and on a software
/// rasteriser — or under haze, or with the vector surfaces drawn over the ring it
/// softens — the only honest A/B is the same scene with the term removed. It is also
/// what a bisect reaches for when a seam turns up somewhere unexpected.
#[must_use]
pub fn lod_blend_enabled() -> bool {
    !matches!(
        std::env::var("CARTO_NO_LOD_BLEND").as_deref(),
        Ok("1") | Ok("true")
    )
}

/// Half-extent of a level's ring in metres: centre to the *outer* edge of the
/// outermost tile, which is where the coarser level takes over.
fn ring_half_extent_m(radius: i64, tile_m: f32) -> f32 {
    (radius as f32 + 0.5) * tile_m
}

/// Write each clipmap tile's ring into its material, so the shader can soften the
/// tile towards the level beneath it (`assets/shaders/ground_detail.wgsl`).
///
/// **Why the ring and not the camera.** The seam is at the ring's edge, not at a
/// distance — a level ends where its tiles end, and on a square boundary. Chebyshev
/// distance from the level's own centre is 1.0 along exactly that boundary, so the
/// blend reaches full bias where the parent becomes visible and nowhere else.
///
/// The coarsest level is left at zero: it has no parent in the pyramid, so softening
/// its rim would blur the horizon into nothing rather than into the next level down.
///
/// Rewritten only when the desired set or the floating origin moves — the same two
/// triggers, and the same reasoning, as
/// [`ground_detail::refresh_ground_detail`](crate::systems::ground_detail::refresh_ground_detail):
/// touching every tile material is affordable on a cell crossing and would not be
/// every frame. The tile count is part of the key for the reason the count is there:
/// a tile that landed after the last write would otherwise keep the zero it was
/// built with until the camera happened to move.
pub fn refresh_tile_lod_blend(
    stream: Res<MapStream>,
    origin: Res<crate::systems::ground_detail::GridCellOrigin>,
    mut materials: ResMut<Assets<crate::systems::ground_detail::GroundMaterial>>,
    // **`StreamTileReady` is not a nicety here, it is what keeps the placeholder
    // out.** Every untextured tile shares one `placeholder` material, so writing a
    // ring into a tile that has not got its own would write it into all of them —
    // including the backstop, whose `basemap` is `None` and which would then blend
    // its rim towards the bound fallback texture. A white horizon, from a system
    // whose whole job is that nothing steps.
    tiles: Query<
        (
            &StreamTile,
            &MeshMaterial3d<crate::systems::ground_detail::GroundMaterial>,
        ),
        With<StreamTileReady>,
    >,
    mut applied: Local<Option<(DesiredKey, Vec2, usize)>>,
) {
    let Some((clat, clng, _)) = stream.anchor else {
        return;
    };
    let Some(key) = stream.desired.key.clone() else {
        return;
    };
    let want = (key, origin.0, tiles.iter().len());
    if applied.as_ref() == Some(&want) {
        return;
    }
    let centres = &want.0 .0;
    // Each level's reach from its own centre, in tiles, measured off what is
    // actually selected. With the rings this was the ring radius by construction;
    // a screen-driven selection is not square, so this is the smallest square
    // around the centre that holds the level, and the fade still lands on the
    // level's outermost tiles.
    let mut reach: HashMap<u32, i64> = HashMap::new();
    for t in stream.desired.wanted.keys() {
        if let Some(&(_, ctx, cty)) = centres.iter().find(|(z, _, _)| *z == t.z) {
            let r = (t.x - ctx).abs().max((t.y - cty).abs());
            let e = reach.entry(t.z).or_insert(0);
            *e = (*e).max(r);
        }
    }
    let bias = if lod_blend_enabled() {
        LOD_BLEND_BIAS
    } else {
        0.0
    };
    for (tile, material) in &tiles {
        let Some(mut material) = materials.get_mut(&material.0) else {
            continue;
        };
        let Some(&(_, ctx, cty)) = centres.iter().find(|(z, _, _)| *z == tile.key.z) else {
            continue;
        };
        // Belt and braces against the placeholder the query filter already excludes:
        // with no image of its own there is nothing to sample softer, and the bound
        // fallback is a white texture.
        if tile.key.z == MIN_ZOOM || material.extension.basemap.is_none() {
            material.extension.lod = Vec4::ZERO;
            continue;
        }
        let radius = reach.get(&tile.key.z).copied().unwrap_or(1).max(1);
        let gs = tile_to_meters(tile.key.z, clat) as f32;
        let (fcx, fcy) = lat_lng_to_tile_frac(clat, clng, tile.key.z);
        // The ring's centre in the anchored world frame is its centre *tile's* centre,
        // placed exactly as `update_map_stream` places every tile — then moved into
        // render space, which is the frame `world_position` arrives in.
        let cx = ((ctx as f64 + 0.5 - fcx) * gs as f64) as f32 - origin.0.x;
        let cz = ((cty as f64 + 0.5 - fcy) * gs as f64) as f32 - origin.0.y;
        let half = ring_half_extent_m(radius, gs);
        material.extension.lod = Vec4::new(cx, cz, 1.0 / half, bias);
    }
    *applied = Some(want);
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

/// Basemap tiles whose relief mesh is still owed, this frame.
///
/// **A capture can otherwise photograph flat ground and call itself settled.** The
/// relief meshes are built [`MAX_RELIEF_BUILDS_PER_FRAME`] at a time — the documented
/// guard against an upload burst — and over a city ~166 tiles qualify, so a cold
/// clipmap is ~83 frames from being shaped. Nothing in the settle test knew that: the
/// tile *streams* went quiet, `terrain_ready` said true, and the shutter fired on a
/// plane. Measured 2026-09-13 on the Posbank at `--wait 20`, which is where this
/// resource came from.
#[derive(Resource, Default)]
pub struct ReliefBacklog(pub usize);

/// Clipmap tiles whose sheet has a tunnel opening taken out of it.
///
/// **The number is what makes the hole assertable.** A capture over a entrance comes
/// back the same whether the basemap was cut or merely happens to be hidden by a
/// vector surface at that range and that angle — which is exactly the ambiguity
/// that left "does the raster roof the opening?" unresolved for a day. This counts
/// the tiles, so `--expect 'basemap_open_tiles>0'` states it instead.
#[derive(Resource, Default)]
pub struct BasemapHoles(pub usize);

/// Marker on a tile whose flat quad has been replaced by a mesh of its own —
/// ground lifted onto the terrain field, bent by the globe's curve, or opened over
/// a tunnel opening — carrying what it was built for.
#[derive(Component)]
pub struct TileRelief {
    revision: u32,
    /// The openings this tile's sheet was cut against
    /// ([`Tunnels::opening_digest`](crate::systems::tunnels::Tunnels::opening_digest)),
    /// kept apart from `revision` because it is the one input that is per tile.
    openings: u64,
    /// The lids this tile's relief was filled over
    /// ([`Tunnels::lid_digest`](crate::systems::tunnels::Tunnels::lid_digest)).
    lids: u64,
}

/// Subdivisions per side for a relief tile of `tile_m` metres: a constant count,
/// bounded by what the field actually measures.
///
/// **A clipmap level is twice the size and twice as far out as the one above it**, so
/// every level's tiles subtend about the same solid angle and a constant division
/// count is what puts a constant number of *pixels* on a quad. The world-space rule
/// this replaced — one quad per [`TERRAIN_SAMPLE_M`] on every level — spent its
/// triangles the other way round: at 25 m sampling a 2,500 m tile got 100 divisions
/// and 20k triangles for ground kilometres away, while the tile underfoot got 8.
///
/// It was not an oversight. Relieved levels are stacked only 0.4 m apart, so a coarse
/// level that approximated the ground more crudely than the fine one above it would
/// poke through it on any slope, and matching every level to the field was the way to
/// make that impossible. It also pinned the field: the coarsest relieved tile at 128
/// divisions cannot describe anything finer than `RELIEF_MAX_TILE_M / 128` = 19.5 m,
/// so the field could never go below 25 m however much relief was being thrown away.
///
/// The poke-through is now prevented where it belongs, in what the mesh samples:
/// `displaced_grid` reads `TerrainSnapshot::floor_at` at its own quad size, a lower
/// envelope that cannot rise above the measured ground. So a level may be as coarse
/// as its distance deserves, and the budget goes to the ground you are standing on.
///
/// The earlier trap is still live and is what the upper bound is for. A **fixed**
/// count was tried once (`RELIEF_DIVISIONS = 48`, sized for a ~1.5 km tile) and
/// applied to a 184 m one; measured on the Groningen anchor (2026-07-31, `--offline`,
/// settled, 60 m) the basemap went from 3k triangles flat to **942k**, the shadow pass
/// from 1.07M to 2.96M, and the frame rate from ~26 fps to 10–11 — which is also what
/// made the egui panels feel broken, the click bridge spending one pass per pointer
/// event. The difference here is the second clamp: a tile never gets more divisions
/// than the field has samples across it, so a fine level asks for detail that exists
/// and a coarse one asks for a lot less than it used to.
fn relief_divisions(tile_m: f32, sample_m: f32) -> usize {
    // One quad per ~1/32 of a tile. Chosen against the triangle budget rather than
    // the picture: ~166 tiles qualify for relief over a Dutch city, and 2·32²
    // triangles each is ~340k — the same order as the 373k the per-sample rule cost
    // at 25 m, which measured 35–45 fps.
    const RELIEF_DIVISIONS: usize = 32;
    // Never finer than the measurements: below this a grid interpolates ground
    // nothing surveyed and pays full geometry price for it.
    //
    // The field's *live* spacing, not `TERRAIN_SAMPLE_M`, because a handset asks for a
    // coarser box (`quality::Budget::terrain_grid`) — reading the constant there would
    // hand the machine with the least to spend four times the vertices it can use.
    // **Floored, not rounded up.** `ceil` is what the per-sample rule used and it is
    // wrong for a bound that means "never finer than the measurements": a tile that is
    // not a whole number of samples across gets one division too many and its quads
    // land between them. A 10 m tile at 3.125 m sampling is the case — four divisions
    // is 2.5 m quads — and `relief_grid_is_constant_per_level_and_never_finer_than_the_field`
    // is what found it.
    let by_field = if sample_m > 0.0 {
        (tile_m / sample_m).floor() as usize
    } else {
        RELIEF_DIVISIONS
    };
    RELIEF_DIVISIONS.min(by_field).max(1)
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
    tunnels: Res<crate::systems::tunnels::Tunnels>,
    mut tiles: Query<(Entity, &StreamTile, &mut Transform, Option<&TileRelief>)>,
    mut backlog: ResMut<ReliefBacklog>,
    mut holes: ResMut<BasemapHoles>,
    mut last_revision: Local<u32>,
) {
    backlog.0 = 0;
    holes.0 = 0;
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
            let (field_centre, half) = field.extent();
            let reach = half + gs * 0.5;
            let d = (centre - field_centre).abs();
            d.x <= reach && d.y <= reach
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
            relief_divisions(gs, field.sample_m()).max(curve_divs)
        } else {
            curve_divs
        };
        // **Every level needs the hole, not just the one you are looking at.** The
        // pyramid stacks its levels 0.4 m apart and sinks the coarse ones by the
        // field's depth — a few metres in a polder — so all of them are still well
        // above a seven-metre carriageway. Cut only the finest and the tunnel opens
        // onto z10 imagery instead of onto z17 imagery.
        let openings = tunnels.opening_digest(centre, gs * 0.5);
        // Only a relieved tile is displaced by the field at all, so only one of
        // those can have a trench under a lid to fill.
        let lids = if relieved {
            tunnels.lid_digest(centre, gs * 0.5 + cartopolis_geo::tunnels::DATUM_M)
        } else {
            0
        };
        let shaped = relieved || divisions > 1 || openings != 0;

        if !shaped {
            if relief.is_some() {
                // Nothing shapes this tile any more — the field went away (layer
                // off, out of coverage, re-anchored to Berlin), or the cell whose
                // tunnels it was opened for was retired. Back to the shared quad,
                // which is also how a hole is filled back in.
                if let Some(quad) = stream.quads.get(&tile.key.z) {
                    commands
                        .entity(entity)
                        .insert(Mesh3d(quad.clone()))
                        .remove::<TileRelief>();
                }
            }
            continue;
        }
        // What is *drawn* with a hole, not what is owed one: the component is the
        // mesh that exists, and counting the wish would report an open basemap for
        // the frames between a cell landing and its tiles being rebuilt.
        if relief.is_some_and(|r| r.openings != 0) {
            holes.0 += 1;
        }
        if relief
            .is_some_and(|r| r.revision == revision && r.openings == openings && r.lids == lids)
        {
            continue;
        }
        // Counted before the per-frame cap turns it away, because this is the number
        // the settle test needs: "how much of the ground is still the flat quad".
        backlog.0 += 1;
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
        let rings = if openings == 0 {
            Vec::new()
        } else {
            tunnels.openings_near(centre, gs * 0.5)
        };
        // The lift the vector ground over a lid takes (`tunnels::Lidded`), added
        // to this sheet's own floor rather than replacing it, so the basemap stays
        // at or under the vector ground as `floor_at` promises.
        //
        // **The least lift among the vertex and its four grid neighbours**, not
        // its own. This sheet's quads are far coarser than the vector ground's
        // refined triangles, so a quad whose corners were lifted by their own
        // fill spans the crease at a lid's edge as a straight chord — above the
        // finer ground on one side of it — and the basemap stood up through the
        // road over the ring's east lid and through the canal beside it
        // (captures, 2026-09-23). The least of the neighbourhood keeps the sheet
        // under the ground it underlies, at the cost of the fill arriving a quad
        // late at a lid's edge, where the vector ground covers it anyway.
        let step = gs / divisions.max(1) as f32;
        let lift_at = |x: f32, z: f32| -> (f32, f32) {
            let (w, land) = tunnels.fill(Vec2::new(x, z), cartopolis_geo::tunnels::DATUM_M);
            if w <= 0.0 {
                return (0.0, 0.0);
            }
            let here = field.at(x, z);
            (w * (land - here).max(0.0), w)
        };
        let lid = |wx: f32, wz: f32| -> (f32, f32) {
            if lids == 0 {
                return (0.0, 0.0);
            }
            let (own, w) = lift_at(wx, wz);
            if w <= 0.0 {
                return (0.0, 0.0);
            }
            let least = [(step, 0.0), (-step, 0.0), (0.0, step), (0.0, -step)]
                .iter()
                .map(|(dx, dz)| lift_at(wx + dx, wz + dz).0)
                .fold(own, f32::min);
            // **Never under a tunnel's roof.** The least-of-the-neighbourhood rule
            // above lets a vertex over a lid sag toward the trench floor wherever a
            // neighbour's fill is smaller, and at a mouth the neighbour is the open
            // cutting, where the fill is nothing. From above that sag is under the
            // vector ground and nobody sees it; from the cutting, looking into the
            // tunnel, it is a pale slope of basemap under the headwall, standing up
            // through the road (Groningen's ring by DUO, reported 2026-09-26). The
            // vector ground over a lid never comes out below the roof and its slab
            // (`Tunnels::cover_at`), so holding the basemap there — scaled by this
            // vertex's own fill weight, as the vector ground's lift is — keeps it
            // inside the lid and still under the ground.
            let here = field.at(wx, wz);
            let roofed = tunnels.cover_at(Vec2::new(wx, wz)).map_or(0.0, |cover| {
                here + w * (cover - here).max(0.0) - field.floor_at(wx, wz, step)
            });
            (least.max(roofed), w)
        };
        let mesh = crate::systems::terrain::displaced_grid(
            &field, &globe, centre, gs, divisions, &rings, &lid,
        );
        commands
            .entity(entity)
            .insert(Mesh3d(meshes.add(mesh)))
            .insert(TileRelief {
                revision,
                openings,
                lids,
            });
        // No `NotShadowCaster` here any more — every `StreamTile` carries it from
        // spawn (see the comment at the spawn site). The reasoning this comment
        // used to hold is still the reasoning, and is worth keeping: four clipmap
        // levels of relief drawn again into every shadow cascade measured as
        // ~2.5× the mesh's own cost (`ktris_shadow` 1.07M flat → 2.96M), for
        // self-shadowing that a 25 m field can barely express and that shows up
        // mostly as acne. `lod22` gives up distant building shadows for the same
        // reason. The trade is real and deliberate: a hill no longer darkens the
        // valley behind it at a low sun. Over Dutch relief (±2 m) that is
        // invisible; if the Veluwe ever needs it, it belongs to the finest level
        // alone, not to all four.
    }
    backlog.0 = backlog.0.saturating_sub(built + curved);
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

/// How many tiles the old ring pyramid held for this tier, down from `finest` —
/// the ceiling [`select_tiles`] works under, so no view asks for more than the
/// rings did: a `LOAD_RADIUS` ring at the finest zoom and a `COARSE_RADIUS` ring at
/// every coarser one, 424 on a desktop from z17.
///
/// `budget` is a parameter rather than a `quality::budget()` read so a test can
/// state the number for a tier it is not running on.
fn ring_budget(finest: u32, budget: &crate::quality::Budget, ring_bias: i64) -> usize {
    (MIN_ZOOM..=finest.max(MIN_ZOOM))
        .map(|z| {
            let r = (budget.radius(if z == finest {
                LOAD_RADIUS
            } else {
                COARSE_RADIUS
            }) - ring_bias)
                .max(1);
            ((2 * r + 1) * (2 * r + 1)) as usize
        })
        .sum()
}

/// Texels per tile edge the screen-space error is measured against, whatever the
/// tier rasterises at.
///
/// **Not `Budget::tile_px`.** The phone tier draws 256² tiles to save memory; if
/// the error were measured at that raster it would ask for a zoom deeper to make
/// up the difference — four times the tiles, which is the memory the smaller
/// raster was there to save. A tier's raster is a deliberate quality choice, not
/// an error for the selection to correct.
const SSE_TILE_PX: f32 = 512.0;

/// Screen height the error is measured in, whatever the device's actually is.
///
/// For the same reason as [`SSE_TILE_PX`]: a 2,400-pixel phone must not select
/// more ground than a 1080p desktop does, or its tile count — which the phone
/// budget exists to bound — would grow with its display.
const SSE_SCREEN_PX: f32 = 1080.0;

/// How many screen pixels one texel may cover before a tile is split.
///
/// 2.0 keeps the ground under the camera about as sharp as the old altitude ladder
/// kept it: at the 45° field of view, straight down from 600 m it selects z16, as
/// the ladder did (the ladder ran from ~2 to ~4.7 pixels per texel under the camera
/// across its bands). Everything further away goes coarser, which is where the
/// saving is — the rings held a whole 7×7 of each level whatever its distance.
const SSE_THRESHOLD: f32 = 2.0;

/// A tile whose children are already selected keeps them until its error falls
/// this far below the threshold, so a camera hovering at a boundary cannot flip a
/// block of tiles back and forth.
const SSE_HYSTERESIS: f32 = 0.8;

/// Widening of the field of view the visibility test uses, so a small turn does not
/// reveal tiles that were never refined.
const VIEW_MARGIN: f32 = 1.3;

/// What [`select_tiles`] needs to know about the camera, in the anchored flat frame.
#[derive(Clone, Copy, Debug)]
struct TileView {
    /// Camera position: world X, altitude above the ground plane, world Z.
    eye: Vec3,
    /// Clip-space transform for the visibility test, widened by [`VIEW_MARGIN`].
    /// `None` skips the test (the prefetch band, and tests that only care about
    /// distance).
    view_proj: Option<Mat4>,
    /// Metres one screen pixel spans per metre of distance.
    pixel_per_m: f32,
}

impl TileView {
    fn new(eye: Vec3, rotation: Quat, fov_y: f32, aspect: f32) -> Self {
        let view = Mat4::from_rotation_translation(rotation, eye).inverse();
        let proj =
            Mat4::perspective_rh((fov_y * VIEW_MARGIN).min(3.0), aspect.max(0.1), 1.0, 1.0e8);
        Self {
            eye,
            view_proj: Some(proj * view),
            pixel_per_m: 2.0 * (fov_y * 0.5).tan() / SSE_SCREEN_PX,
        }
    }

    /// Whether any part of an axis-aligned box can be on screen. Conservative: a
    /// box straddling the camera plane counts as visible.
    fn sees(&self, min: Vec3, max: Vec3) -> bool {
        let Some(vp) = self.view_proj else {
            return true;
        };
        let corners = [
            Vec3::new(min.x, min.y, min.z),
            Vec3::new(max.x, min.y, min.z),
            Vec3::new(min.x, max.y, min.z),
            Vec3::new(max.x, max.y, min.z),
            Vec3::new(min.x, min.y, max.z),
            Vec3::new(max.x, min.y, max.z),
            Vec3::new(min.x, max.y, max.z),
            Vec3::new(max.x, max.y, max.z),
        ];
        let clip = corners.map(|c| vp * c.extend(1.0));
        if clip.iter().all(|c| c.w <= 0.0) {
            return false;
        }
        if clip.iter().any(|c| c.w <= 0.0) {
            return true;
        }
        let all = |f: &dyn Fn(&Vec4) -> bool| clip.iter().all(f);
        !(all(&|c| c.x > c.w)
            || all(&|c| c.x < -c.w)
            || all(&|c| c.y > c.w)
            || all(&|c| c.y < -c.w))
    }
}

/// The basemap tiles to hold for this view: refined by **screen-space error**
/// rather than by a ring per zoom (`docs/notes/level-of-detail-tile-tree.md`).
///
/// The roots are the coarsest level's ring around the camera, which is what
/// reaches the horizon and is never culled — it is the ground under everything.
/// From there the tile whose texels are largest on screen is split first, and only
/// if it can be seen, until nothing needs splitting or `cap` tiles are held. So
/// the ground under the camera is as fine as the old altitude ladder made it,
/// ground far off is coarser, and ground behind the camera stays at whatever level
/// covers it.
///
/// **Every selected tile's parent is selected too.** That is what the old rings
/// gave for free — each level sat under the one above it — and it is what lets
/// [`occluded_by_finer`] hide a parent only once all four children are ready, so a
/// refinement never opens a hole. It costs about a third more tiles than the
/// leaves alone, all of them coarse.
///
/// `cap` bounds the total, ancestors included. It is the old ring pyramid's own
/// count for the tier ([`ring_budget`]), so no view can ask for more than the rings
/// did. `held` is what the previous selection chose, for the hysteresis.
///
/// Spawn order is coarse-first and nearest-first within a level, as the rings'
/// was: a blurry whole ground beats a sharp patch of it.
#[allow(clippy::too_many_arguments)]
fn select_tiles(
    view: &TileView,
    anchor: (f64, f64),
    roots: (i64, i64, i64),
    base_zoom: u32,
    max_zoom: u32,
    cap: usize,
    held: &HashMap<TileKey, f32>,
) -> (HashMap<TileKey, f32>, Vec<TileKey>) {
    let (clat, clng) = anchor;
    // Per zoom: tile edge in metres and the anchor's fractional tile coordinate, so a
    // tile's extent in the flat frame is ((x - fcx) * gs .. (x + 1 - fcx) * gs).
    let frames: HashMap<u32, (f64, f64, f64)> = (MIN_ZOOM..=max_zoom.max(MIN_ZOOM))
        .map(|z| {
            let (fcx, fcy) = lat_lng_to_tile_frac(clat, clng, z);
            (z, (tile_to_meters(z, clat), fcx, fcy))
        })
        .collect();
    let bounds = |t: TileKey| -> (Vec3, Vec3) {
        let (gs, fcx, fcy) = frames[&t.z];
        let x0 = ((t.x as f64 - fcx) * gs) as f32;
        let z0 = ((t.y as f64 - fcy) * gs) as f32;
        let e = gs as f32;
        (
            Vec3::new(x0, -TILE_AABB_Y_MARGIN_M, z0),
            Vec3::new(x0 + e, TILE_AABB_Y_MARGIN_M, z0 + e),
        )
    };
    // Screen pixels one texel of this tile covers at its nearest point.
    let error = |t: TileKey| -> f32 {
        let (min, max) = bounds(t);
        let dx = (min.x - view.eye.x).max(view.eye.x - max.x).max(0.0);
        let dz = (min.z - view.eye.z).max(view.eye.z - max.z).max(0.0);
        let d = (dx * dx + dz * dz + view.eye.y * view.eye.y)
            .sqrt()
            .max(1.0);
        let texel = (max.x - min.x) / SSE_TILE_PX;
        texel / (d * view.pixel_per_m)
    };

    let mut wanted: HashMap<TileKey, f32> = HashMap::new();
    // Max-heap on the error, as integer millipixels so the key is `Ord`.
    let mut open: std::collections::BinaryHeap<(u64, i64, i64, u32)> =
        std::collections::BinaryHeap::new();
    let (r, cx, cy) = roots;
    for b in -r..=r {
        for a in -r..=r {
            let t = TileKey::new(cx + a, cy + b, MIN_ZOOM);
            wanted.insert(t, level_y(base_zoom, MIN_ZOOM));
            open.push(((error(t) * 1000.0) as u64, t.x, t.y, t.z));
        }
    }
    while let Some((e_milli, x, y, z)) = open.pop() {
        if z >= max_zoom || wanted.len() + 4 > cap {
            continue;
        }
        let t = TileKey::new(x, y, z);
        let children = [
            TileKey::new(2 * x, 2 * y, z + 1),
            TileKey::new(2 * x + 1, 2 * y, z + 1),
            TileKey::new(2 * x, 2 * y + 1, z + 1),
            TileKey::new(2 * x + 1, 2 * y + 1, z + 1),
        ];
        let threshold = if children.iter().all(|c| held.contains_key(c)) {
            SSE_THRESHOLD * SSE_HYSTERESIS
        } else {
            SSE_THRESHOLD
        };
        if (e_milli as f32 / 1000.0) <= threshold {
            continue;
        }
        let (min, max) = bounds(t);
        if !view.sees(min, max) {
            continue;
        }
        for c in children {
            wanted.insert(c, level_y(base_zoom, c.z));
            open.push(((error(c) * 1000.0) as u64, c.x, c.y, c.z));
        }
    }

    let mut order: Vec<(u32, u64, TileKey)> = wanted
        .keys()
        .map(|t| {
            let (min, max) = bounds(*t);
            let c = (min + max) * 0.5;
            let d2 = (c.x - view.eye.x).powi(2) + (c.z - view.eye.z).powi(2);
            (t.z, d2 as u64, *t)
        })
        .collect();
    order.sort_unstable_by_key(|(z, d2, t)| (*z, *d2, t.x, t.y));
    (wanted, order.into_iter().map(|(_, _, t)| t).collect())
}

/// A tile's rank: [`tile_rank`]'s near band, and behind it the tile's place in the
/// current spawn order (coarse level first, then nearest).
///
/// The place matters because equal ranks keep *arrival* order, and arrival order
/// belongs to the view a tile was first asked for in. A camera that keeps moving
/// queued the newly-visible tiles behind ones that were wanted a second ago — the
/// stall where what you are looking at waits on what you looked at. A tile that has
/// left the wanted set has no place and takes the back of the queue.
fn tile_rank_at(
    tile: TileKey,
    centres: &[(u32, i64, i64)],
    place: Option<usize>,
) -> fetch_gate::Rank {
    let near = tile_rank(tile, centres);
    if near <= fetch_gate::NEAR {
        return near;
    }
    place.map_or(fetch_gate::ANY, |i| {
        (fetch_gate::NEAR + 1)
            .saturating_add(u32::try_from(i).unwrap_or(u32::MAX - 2 - fetch_gate::NEAR))
    })
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
    camera: Query<(&GridCell, &Transform, &Projection), With<FreeCamera>>,
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
    let Ok((cell, tf, projection)) = camera.single() else {
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

    // Above the globe threshold nothing here is on screen — the globe is
    // (`data_layers::apply_feature_toggles` hides every `StreamTile` in space), and
    // the streamer only runs to warm the descent (`MAP_PREFETCH_ALT`). Warm the
    // roots and no more: refining for an orbiting camera would fetch fine ground
    // for every patch it drifts across.
    let max_zoom = if altitude > crate::systems::globe::GLOBE_SHOW_ALT {
        MIN_ZOOM
    } else {
        base_zoom.max(MIN_ZOOM)
    };

    // Camera world position (tile centres are derived from it at every zoom).
    let world_x = cell.x as f32 * WORLD_CELL_EDGE + tf.translation.x;
    let world_z = cell.z as f32 * WORLD_CELL_EDGE + tf.translation.z;

    // Each level's centre tile under the camera — no longer what decides the
    // desired set (`select_tiles` does), but still what the fetch rank's near band
    // and the level cross-fade are measured from. Plus each zoom's shared quad
    // mesh. A tile (tx,ty) centre sits at world ((tx+0.5 - fcx)*gs, (ty+0.5 -
    // fcy)*gs) relative to the fixed anchor, so the map never shifts across zooms;
    // coarser zooms sit slightly lower so finer tiles draw on top.
    let mut centres: Vec<(u32, i64, i64)> = Vec::with_capacity(8);
    for zoom in MIN_ZOOM..=max_zoom {
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
    // The pose, coarsely: the selection depends on the camera continuously, and
    // re-deriving it every frame would re-derive it for sub-pixel moves. A step of
    // 5 % of the altitude is well under the error one zoom level makes, and three
    // degrees of turn is well inside `VIEW_MARGIN`.
    let pose = {
        let step = (altitude * 0.05).max(5.0);
        let (yaw, pitch, _) = tf.rotation.to_euler(EulerRot::YXZ);
        [
            (world_x / step).floor() as i32,
            (world_z / step).floor() as i32,
            (altitude.max(1.0).log2() * 16.0).floor() as i32,
            (yaw.to_degrees() / 3.0).floor() as i32,
            (pitch.to_degrees() / 3.0).floor() as i32,
        ]
    };
    let key = (centres, stream.fetch_generation, ring_bias, pose);
    let mut view_changed = false;
    if stream.desired.key.as_ref() != Some(&key) {
        let budget = crate::quality::budget();
        let (fov, aspect) = match projection {
            Projection::Perspective(p) => (p.fov, p.aspect_ratio),
            _ => (std::f32::consts::FRAC_PI_4, 16.0 / 9.0),
        };
        let eye = Vec3::new(world_x, altitude, world_z);
        let view = TileView::new(eye, tf.rotation, fov, aspect);
        let roots_radius = (budget.radius(if max_zoom == MIN_ZOOM {
            LOAD_RADIUS
        } else {
            COARSE_RADIUS
        }) - ring_bias)
            .max(1);
        let (_, rx, ry) = key.0[0];
        let (wanted, spawn_order) = select_tiles(
            &view,
            (clat, clng),
            (roots_radius, rx, ry),
            base_zoom,
            max_zoom,
            ring_budget(max_zoom, budget, ring_bias),
            &stream.desired.wanted,
        );
        let finest = wanted.keys().map(|t| t.z).max().unwrap_or(MIN_ZOOM);
        if finest != stream.current_zoom {
            tracing::debug!(
                from = stream.current_zoom,
                to = finest,
                altitude,
                "finest map zoom changed"
            );
            stream.current_zoom = finest;
        }
        if let Ok(mut shared) = stream.wanted_shared.write() {
            shared.clear();
            shared.extend(wanted.keys().copied());
        }
        stream.desired = DesiredTiles {
            key: Some(key),
            wanted,
            spawn_order,
        };
        view_changed = true;
    }

    // Spawn newly-needed tiles and ask for their imagery, coarse-first (see
    // `DesiredTiles::spawn_order`). Indexed rather than iterated because the body
    // mutably borrows `stream`; `TileKey` is `Copy`, so reading one out costs
    // nothing and no 424-entry clone is needed.
    let gen = stream.fetch_generation;
    // Everything held for a previous fetch generation is wrong, not merely old:
    // a re-anchor moves every tile's world position and a source switch changes
    // what its pixels are. Discarding here, where the generation is already read
    // out, keeps that rule in one place rather than in the three functions that
    // bump it.
    stream.warm.set_generation(gen);
    // The level centres, for the rank below. Eight entries, cloned out because the
    // loop body borrows `stream` mutably.
    let level_centres: Vec<(u32, i64, i64)> = stream
        .desired
        .key
        .as_ref()
        .map(|(centres, _, _, _)| centres.clone())
        .unwrap_or_default();
    // A queued tile was ranked for the view it was asked for in. When the view has
    // moved, the queue is re-ranked to the new one, or a tile the camera is now over
    // waits behind everything queued before it.
    if view_changed {
        let order: HashMap<TileKey, usize> = stream
            .desired
            .spawn_order
            .iter()
            .enumerate()
            .map(|(i, k)| (*k, i))
            .collect();
        stream
            .tiles
            .reprice_queue(|k| tile_rank_at(*k, &level_centres, order.get(k).copied()));
    }
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
        let rank = tile_rank_at(key, &level_centres, Some(i));
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
            // **An explicit bound, so these can be culled like anything else.**
            //
            // This was `NoFrustumCulling`, on the stated reasoning that "flat
            // tiles + big_space rebasing confuse frustum culling". big_space was
            // not the problem: the tile is a child of the `BigSpace` root with its
            // own `GridCell`, and producing a correct `GlobalTransform` for
            // exactly that arrangement is what the crate is for.
            //
            // The problem is that `stream.quads` **caches one quad mesh per zoom
            // and reuses it**. Bevy's `calculate_bounds` only gives an `Aabb` to
            // an entity that has none, and only while it can see the mesh's
            // positions — which for a stashed, reused handle is the first spawn
            // and no other. An entity with no `Aabb` is treated exactly like one
            // that opted out, so the opt-out was describing the symptom.
            //
            // So the bound is stated rather than derived. It has to stay correct
            // when the flat quad is later swapped for a relief mesh, and
            // `calculate_bounds` will not revisit an entity that already has one,
            // so it is deliberately generous in Y: `TILE_AABB_Y_MARGIN_M` is far
            // beyond any relief this field carries (39.9 m measured over
            // Groningen) and still negligible against a tile that is 1.5 km across
            // at z14 and larger at every coarser level. A too-large bound costs a
            // tile that could have been culled; a too-small one deletes ground
            // somebody is looking at.
            Aabb::from_min_max(
                Vec3::new(-gs / 2.0, -TILE_AABB_Y_MARGIN_M, -gs / 2.0),
                Vec3::new(gs / 2.0, TILE_AABB_Y_MARGIN_M, gs / 2.0),
            ),
            // The ground never casts, on any tile, in any shape. This used to be
            // inserted only by the relief path below, on the reasoning quoted
            // there — which was right about relief and silently wrong about
            // everything else: the line above opts these entities out of frustum
            // culling, and `check_dir_light_mesh_visibility` answers
            // `NoFrustumCulling` by pushing the entity into **every** cascade of
            // **every** light with no test at all. So the whole clipmap pyramid —
            // ~424 quads, including the ones behind the camera — was rasterised
            // into up to eight shadow maps every frame, each one a ground plane
            // covering its whole cascade, to cast a shadow that is by
            // construction the same flat plane it is cast on.
            //
            // Opting out here rather than there also makes the relief path's job
            // smaller: it no longer has to add the component on the way into a
            // displaced mesh and remove it on the way back out.
            bevy::light::NotShadowCaster,
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
        // Held at start-up until the crop register's coverage has answered (or
        // `crops::RASTER_WAIT_S` has passed): a tile drawn before it is drawn
        // without its fields, and is kept that way on disk too.
        while let Some(key) = crate::systems::crops::raster_ready()
            .then(|| stream.tiles.next_job(now))
            .flatten()
        {
            jobs.push(key);
        }
        for key in jobs {
            // Held from the last time this tile was on screen? Then there is no
            // fetch, no MVT parse and no scan conversion to do — push the
            // finished mip chain straight into the sink the worker would have
            // pushed it into, so the upload path, the byte ledger and
            // `CellStream`'s slot accounting are all unchanged.
            //
            // Looked up *after* `next_job`, which means a warm hit spends a
            // `fetch_gate` slot for the one frame it takes `apply_stream_textures`
            // to drain it. That costs nothing real: uploads are capped at
            // `MAX_UPLOADS_PER_FRAME` and by `stream_ledger`'s byte budget, so
            // even an entirely warm ring could not be applied faster than this.
            // Looking it up before dispatch would mean reaching inside
            // `Dispatcher`'s queue for the sake of a bound that is not binding.
            if let Some(warm) = stream.warm.take(&key) {
                if let Ok(mut g) = results.lock() {
                    g.push((key, Some(warm)));
                }
                stream.warm_served += 1;
                continue;
            }
            // The **only** place a consumer changes basemap source. `map_geometry`,
            // `globe`, `navigation` and the router keep reading
            // `OsmTileUrlTemplate`: they want MVT geometry, not pixels, and the
            // globe would be asking a national service for a planet.
            let template = if stream.aerial_for(&key) {
                crate::systems::aerial::template().to_string()
            } else {
                template.clone()
            };
            spawn_map_tile(
                key,
                gen,
                template,
                results.clone(),
                live_gen.clone(),
                stream.wanted_shared.clone(),
            );
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
    mut materials: ResMut<Assets<crate::systems::ground_detail::GroundMaterial>>,
    detail: Res<crate::systems::ground_detail::GroundDetail>,
    origin: Res<crate::systems::ground_detail::GridCellOrigin>,
    water: Res<crate::systems::water::WaterSeaState>,
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
        // Read out first: `ResMut` derefs as a whole, so the macro cannot borrow
        // one field mutably and another immutably in the same expression.
        let warm_served = std::mem::take(&mut stream.warm_served);
        let warm_held = stream.warm.len();
        let warm_mb = stream.warm.bytes() / (1024 * 1024);
        tracing::info!(
            in_flight = stream.tiles.in_flight(),
            awaiting_upload = tiles.iter().count(),
            loaded = stream.loaded.len(),
            requeued = stream.requeues,
            // How much of this burst cost nothing but an upload, and how much of
            // the allowance that is buying. Beside `requeued` on purpose: the two
            // together say whether a ring is filling, refetching, or being
            // re-served from memory.
            warm_served,
            warm_held,
            warm_mb,
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
                // Keep the finished mip chain for a return journey *before* it is
                // moved into the image, because `TILE_TEXTURE_USAGE` is
                // `RENDER_WORLD` only: once uploaded there is no CPU copy left to
                // keep, so this is the single moment it can be taken.
                //
                // The cost is one memcpy of the chain — ~1.4 MB, a few hundred
                // microseconds — against the 48–68 ms of scan conversion it saves
                // if the tile is ever wanted again. `put` is a no-op when the
                // store is disabled, so the phone rung does not pay the copy
                // either. Cloned rather than shared behind an `Arc` because
                // `Image::data` owns its bytes: sharing would only move the copy,
                // not remove it.
                if !stream.warm.disabled() {
                    stream.warm.put(key, decoded.clone());
                }
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
                let mut extension = detail.extension(&origin);
                // The same handle the base colour takes, bound a second time so the
                // cross-fade can sample it at a mip bias
                // (`refresh_tile_lod_blend`). The ring itself is written by that
                // system on its next pass; until then `lod.w` is 0 and the tile is
                // drawn exactly as it was before.
                extension.basemap = Some(handle.clone());
                // The tile's water texels are lit as the water surface's far
                // field (`ground_detail.wgsl`), from the sea state as it is now;
                // `water::update_water_waves` keeps them in step after. A tile
                // with no mask in its alpha reads as all land and is untouched.
                (extension.water_body, extension.water_sea) = water.far_water();
                let mat = materials.add(crate::systems::ground_detail::GroundMaterial {
                    base: StandardMaterial {
                        base_color_texture: Some(handle),
                        // Matte, like the placeholder — no grazing-angle sheen.
                        perceptual_roughness: 1.0,
                        reflectance: 0.0,
                        ..default()
                    },
                    // The half-metre residual slope, if a tier has landed. Read at
                    // spawn like `WaterSeaState`, and kept in step afterwards by
                    // `ground_detail::refresh_ground_detail`.
                    extension,
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

    /// The anchor the map opens on places a building where the world frame
    /// places the ground under it.
    ///
    /// Buildings go through `lat_lng_to_local` (metres from the anchor *tile's
    /// centre*); the camera, the teleport and the ground go through
    /// `lat_lng_to_world` (metres from the anchor itself). The two agree only if
    /// the anchor is a tile centre, and the bootstrap one was not: with short
    /// jumps keeping the frame it lasted a whole session and put every building
    /// 89 m off the map.
    #[test]
    fn the_bootstrap_anchor_places_buildings_on_the_map() {
        let anchor = bootstrap_anchor();
        let (clat, clng, z) = anchor;
        let (ax, ay) = lat_lng_to_tile(clat, clng, z);
        let tile_m = tile_to_meters(z, clat) as f32;
        // The Martinitoren, and a point near the edge of a short jump.
        for (lat, lng) in [(53.21937, 6.56820), (53.2030, 6.5930)] {
            let building =
                crate::systems::osm_buildings::lat_lng_to_local(lat, lng, ax, ay, z, tile_m);
            let (wx, wz) = crate::systems::geo_nav::lat_lng_to_world(anchor, lat, lng);
            assert!(
                (building.x as f64 - wx).abs() < 0.05 && (building.y as f64 - wz).abs() < 0.05,
                "building at ({}, {}) but the ground at ({wx:.2}, {wz:.2})",
                building.x,
                building.y,
            );
        }
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

    /// The relief grid must be constant per *level* and never finer than the field.
    ///
    /// Two rules, and the history is that each was learned by breaking the other. A
    /// fixed 48 divisions was sized for a ~1.5 km tile and applied to the 184 m ones
    /// the clipmap actually streams over a city, which is where the 942k-triangle
    /// basemap came from — hence the upper bound. Then one quad per field sample was
    /// applied to *every* level, which spent 20k triangles on ground 3 km away and
    /// pinned the field at 25 m — hence the constant.
    #[test]
    fn relief_grid_is_constant_per_level_and_never_finer_than_the_field() {
        let sample = crate::systems::terrain::TERRAIN_SAMPLE_M;
        assert!(
            (sample - 3.125).abs() < 0.01,
            "the field is a 3.125 m grid; this test's numbers assume it: {sample}"
        );

        // Every level a Dutch city streams gets the same count — same triangles, and
        // (a clipmap level being twice the size and twice as far out) about the same
        // pixels per quad.
        for tile_m in [184.0, 368.0, 736.0, 1471.0, RELIEF_MAX_TILE_M] {
            assert_eq!(
                relief_divisions(tile_m, sample),
                32,
                "a {tile_m} m tile should get the level-constant count"
            );
        }

        // Below that, the field is the bound: a tile smaller than 32 samples across
        // must not invent vertices between measurements.
        for tile_m in [10.0f32, 25.0, 90.0] {
            let d = relief_divisions(tile_m, sample);
            let quad_m = tile_m / d as f32;
            assert!(
                quad_m >= sample,
                "{tile_m} m tile: {quad_m} m quads interpolate detail nothing measured"
            );
        }

        // And the whole relieved set is affordable. ~166 tiles qualify over a Dutch
        // city; the per-sample rule at 25 m cost ~373k triangles there and measured
        // 35–45 fps, so that is the ceiling this has to stay under.
        let tris = |d: usize| d * d * 2;
        let total: usize = [(184.0, 49), (368.0, 49), (736.0, 49), (1471.0, 19)]
            .iter()
            .map(|&(m, tiles): &(f32, usize)| tris(relief_divisions(m, sample)) * tiles)
            .sum();
        assert!(
            total < 400_000,
            "the relieved clipmap is {total} triangles; the 25 m per-sample rule was 373k"
        );
    }

    /// A tile far coarser than the field still yields a bounded mesh.
    #[test]
    fn relief_grid_is_bounded() {
        let sample = crate::systems::terrain::TERRAIN_SAMPLE_M;
        assert_eq!(relief_divisions(1.0e6, sample), 32);
        assert_eq!(relief_divisions(0.0, sample), 1);
        // A coarser field (a handset's) buys fewer vertices, not the same ones.
        assert_eq!(relief_divisions(184.0, 12.5), 14);
    }

    use crate::systems::vector_tiles::SubTile;

    #[test]
    fn a_tile_ranks_by_its_place_in_the_spawn_order_behind_the_near_band() {
        let c = vec![(14, 100, 100)];
        let near = TileKey::new(101, 99, 14);
        let far = TileKey::new(110, 100, 14);
        assert_eq!(tile_rank_at(near, &c, Some(50)), fetch_gate::NEAR);
        let early = tile_rank_at(far, &c, Some(3));
        let late = tile_rank_at(far, &c, Some(40));
        assert!(fetch_gate::NEAR < early && early < late && late < fetch_gate::ANY);
        assert_eq!(tile_rank_at(far, &c, None), fetch_gate::ANY);
    }

    /// Each level's centre tile for a camera sitting on the anchor — the same
    /// arithmetic `update_map_stream` runs every frame.
    fn centres(lat: f64, lng: f64, finest_zoom: u32) -> Vec<(u32, i64, i64)> {
        (MIN_ZOOM..=finest_zoom)
            .map(|zoom| {
                let (fcx, fcy) = lat_lng_to_tile_frac(lat, lng, zoom);
                (zoom, (fcx - 0.5).round() as i64, (fcy - 0.5).round() as i64)
            })
            .collect()
    }

    /// A camera over the default anchor at `altitude`, pitched `pitch_deg` below
    /// the horizon and facing north (−Z), selected at the desktop tier.
    fn select_at(altitude: f32, pitch_deg: f32) -> (HashMap<TileKey, f32>, Vec<TileKey>) {
        let eye = Vec3::new(0.0, altitude, 0.0);
        let rotation = Quat::from_rotation_x(-pitch_deg.to_radians());
        let view = TileView::new(eye, rotation, std::f32::consts::FRAC_PI_4, 16.0 / 9.0);
        let c = centres(DEFAULT_ANCHOR_LAT, DEFAULT_ANCHOR_LNG, DEFAULT_ANCHOR_ZOOM);
        let (_, rx, ry) = c[0];
        select_tiles(
            &view,
            (DEFAULT_ANCHOR_LAT, DEFAULT_ANCHOR_LNG),
            (COARSE_RADIUS, rx, ry),
            DEFAULT_ANCHOR_ZOOM,
            DEFAULT_ANCHOR_ZOOM,
            ring_budget(DEFAULT_ANCHOR_ZOOM, &desktop(), 0),
            &HashMap::new(),
        )
    }

    /// The ceiling, stated so a change to a radius or a zoom has to restate it:
    /// what the old rings held, 9×9 at z17 plus 7×7 at each of z10–z16.
    #[test]
    fn the_desktop_ceiling_is_424_and_the_phone_224() {
        assert_eq!(ring_budget(DEFAULT_ANCHOR_ZOOM, &desktop(), 0), 424);
        assert_eq!(
            ring_budget(DEFAULT_ANCHOR_ZOOM, &crate::quality::Budget::phone(), 0),
            224
        );
        // The load controller's lever is still a lever.
        assert!(ring_budget(DEFAULT_ANCHOR_ZOOM, &desktop(), 1) < 424);
    }

    /// Straight down from 600 m: the ground under the camera is z16, as the old
    /// altitude ladder made it, and the whole selection is well under the rings'.
    #[test]
    fn straight_down_from_600_m_is_z16_underfoot_and_fewer_tiles() {
        let (wanted, order) = select_at(600.0, 90.0);
        assert_eq!(wanted.len(), order.len());
        assert!(wanted.len() < 424, "selected {} tiles", wanted.len());
        let (fx, fy) = lat_lng_to_tile_frac(DEFAULT_ANCHOR_LAT, DEFAULT_ANCHOR_LNG, 16);
        assert!(
            wanted.contains_key(&TileKey::new(fx.floor() as i64, fy.floor() as i64, 16)),
            "the tile under the camera is not z16"
        );
        assert!(
            !wanted.keys().any(|t| t.z > 16),
            "600 m straight down asked for z17"
        );
        assert_eq!(order.first().unwrap().z, MIN_ZOOM, "not coarse-first");
    }

    /// Every selected tile's parent is selected, so a refinement can never open a
    /// hole — `occluded_by_finer` only hides a parent once all four children cover
    /// it, and that needs the parent to be there.
    #[test]
    fn every_selected_tile_has_its_parent() {
        for (alt, pitch) in [(600.0, 90.0), (300.0, 20.0), (40.0, 5.0), (8_000.0, 45.0)] {
            let (wanted, _) = select_at(alt, pitch);
            for t in wanted.keys().filter(|t| t.z > MIN_ZOOM) {
                let parent = TileKey::new(t.x.div_euclid(2), t.y.div_euclid(2), t.z - 1);
                assert!(
                    wanted.contains_key(&parent),
                    "{t:?} is selected without its parent at {alt} m, {pitch}°"
                );
            }
            assert!(
                wanted.len() <= 424,
                "{} tiles at {alt} m, {pitch}°",
                wanted.len()
            );
        }
    }

    /// Ground behind the camera is not refined: looking north, the finest tile to
    /// the south is coarser than the finest to the north.
    #[test]
    fn ground_behind_the_camera_stays_coarse() {
        let (wanted, _) = select_at(300.0, 20.0);
        let (_, fcy) = lat_lng_to_tile_frac(DEFAULT_ANCHOR_LAT, DEFAULT_ANCHOR_LNG, 17);
        let finest = |south: bool| {
            wanted
                .keys()
                .filter(|t| {
                    let y17 = (t.y as f64 + 0.5) * f64::from(1u32 << (17 - t.z));
                    if south {
                        y17 > fcy + 20.0
                    } else {
                        y17 < fcy - 20.0
                    }
                })
                .map(|t| t.z)
                .max()
                .unwrap_or(MIN_ZOOM)
        };
        assert!(
            finest(true) < finest(false),
            "behind the camera reached z{} against z{} ahead",
            finest(true),
            finest(false)
        );
    }

    /// The prefetch band must stay one coarse ring. `MAP_PREFETCH_ALT` unfreezes
    /// the streamer well above the globe threshold so a descent lands on warm
    /// tiles; that is only affordable because the selection there is capped at
    /// `MIN_ZOOM` — the roots and nothing else, 9×9 = 81.
    #[test]
    fn prefetch_band_is_one_coarse_ring() {
        let view = TileView {
            eye: Vec3::new(0.0, MAP_PREFETCH_ALT, 0.0),
            view_proj: None,
            pixel_per_m: 2.0 * (std::f32::consts::FRAC_PI_8).tan() / SSE_SCREEN_PX,
        };
        let c = centres(DEFAULT_ANCHOR_LAT, DEFAULT_ANCHOR_LNG, MIN_ZOOM);
        let (_, rx, ry) = c[0];
        let (wanted, _) = select_tiles(
            &view,
            (DEFAULT_ANCHOR_LAT, DEFAULT_ANCHOR_LNG),
            (LOAD_RADIUS, rx, ry),
            DEFAULT_ANCHOR_ZOOM,
            MIN_ZOOM,
            ring_budget(MIN_ZOOM, &desktop(), 0),
            &HashMap::new(),
        );
        assert_eq!(wanted.len(), 81, "the prefetch ring changed size");
        assert!(wanted.keys().all(|t| t.z == MIN_ZOOM));
    }

    /// Overzoomed requests add no source tile: Shortbread stops at z14, and every
    /// z15+ tile's z14 ancestor is already selected (parents always are), so the
    /// fetches collapse onto bodies the same pass is asking for anyway.
    #[test]
    fn the_overzoomed_requests_collapse_onto_selected_source_tiles() {
        let (_, order) = select_at(150.0, 30.0);
        let sources: std::collections::HashSet<(u32, i64, i64)> = order
            .iter()
            .map(|k| SubTile::resolve(k.z, k.x, k.y).0)
            .collect();
        let below: std::collections::HashSet<(u32, i64, i64)> = order
            .iter()
            .filter(|k| k.z <= crate::systems::vector_tiles::MAX_SOURCE_ZOOM)
            .map(|k| (k.z, k.x, k.y))
            .collect();
        assert!(order
            .iter()
            .any(|k| k.z > crate::systems::vector_tiles::MAX_SOURCE_ZOOM));
        assert_eq!(
            sources, below,
            "an overzoomed request needed a new source tile"
        );
    }
}
