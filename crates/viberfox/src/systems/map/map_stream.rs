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

use std::collections::HashMap;
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::{Arc, Mutex};

use bevy::prelude::*;
use bevy::tasks::IoTaskPool;
use bevy::asset::RenderAssetUsages;
use bevy::render::render_resource::TextureFormat;
use bevy::camera::visibility::NoFrustumCulling;
use bevy_image::{Image, ImageSampler};
use big_space::prelude::{BigSpace, Grid, GridCell};

use crate::platform::stream::{CellStream, Policy};
use crate::resources::OsmTileUrlTemplate;
use crate::systems::free_camera::{FreeCamera, WORLD_CELL_EDGE, WORLD_SWITCH_THRESHOLD};
use viberfox_core::world::{lat_lng_to_tile, tile_to_lat_lng, tile_to_meters};
use viberfox_core::TileKey;

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
/// Re-exported from `viberfox_core`, where it lives next to the integer
/// [`viberfox_core::lat_lng_to_tile`] it used to duplicate verbatim. Kept visible here
/// because a dozen call sites reach for it through this module, and because the
/// routing graph needs it *without* needing the streamer — that dependency was the
/// last non-doc tie from `systems::geo` to a Bevy module.
pub use viberfox_core::lat_lng_to_tile_frac;

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
const TILE_MAX_IN_FLIGHT: usize = 8;
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

/// Marker for a streamed ground tile, carrying its key.
///
/// The streamer itself works key-first (`MapStream::loaded` maps key → entity),
/// but `apply_terrain_to_tiles` walks the tiles the other way and needs to know
/// which one it is holding to sample the field under it.
#[derive(Component)]
pub struct StreamTile {
    pub key: TileKey,
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
    /// `(zoom, centre x, centre y)` per clipmap level, plus the anchor generation.
    key: Option<(Vec<(u32, i64, i64)>, u32)>,
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
        .unwrap_or(viberfox_core::world::REGION_ZOOM_LEVEL);
    // Queue the whole old-anchor patch for bounded despawn rather than freeing it
    // all in one frame (mass-despawn device-loss guard; drained by
    // `update_map_stream`). The queued entities are still live StreamTiles, so the
    // query re-captures them — clear any prior queue first to avoid double-frees.
    stream.tiles.clear_despawn_queue();
    let retired: Vec<Entity> = tiles.iter().collect();
    stream.tiles.retire_all(retired);
    stream.loaded.clear();
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
            tiles: CellStream::new(Policy {
                max_in_flight: TILE_MAX_IN_FLIGHT,
                min_interval_secs: 0.0,
                max_attempts: MAX_FETCH_ATTEMPTS,
                backoff_secs: FETCH_RETRY_SECS,
                backoff_doublings: 0,
                // No cooldown: once the attempts are spent the grey placeholder is
                // final, until the tile leaves the pyramid and comes back.
                cooldown_secs: 0.0,
                despawns_per_frame: MAX_TILE_DESPAWNS_PER_FRAME,
            }),
            gen_shared: Arc::new(AtomicU32::new(0)),
            quads: HashMap::new(),
            placeholder: None,
            desired: DesiredTiles::default(),
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

/// Stand the basemap tiles on the terrain field, and get out of the way of the
/// tiles that can't be.
///
/// Two halves, and the second is not optional. The finest tiles inside the field
/// get a subdivided, displaced mesh. **Every coarser level then sinks** below the
/// field's lowest point, because the clipmap stacks levels only 0.4 m apart: a
/// dip of eight metres — routine in a Dutch polder — would otherwise drop the
/// detailed tile *underneath* the coarse imagery it is supposed to be refining,
/// and the hollow would fill with blurry z10 pixels.
pub fn apply_terrain_to_tiles(
    mut commands: Commands,
    mut meshes: ResMut<Assets<Mesh>>,
    stream: Res<MapStream>,
    field: Res<crate::systems::terrain::TerrainField>,
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

    let mut built = 0;
    for (entity, tile, mut tf, relief) in tiles.iter_mut() {
        let gs = tile_to_meters(tile.key.z, clat) as f32;
        let relieved = field.is_ready() && gs <= RELIEF_MAX_TILE_M;
        // Coarse levels: sit below the deepest part of the relief. Fine ones keep
        // their own resting height and carry the displacement in the mesh.
        let want_y = level_y(base_zoom, tile.key.z) + if relieved { 0.0 } else { sink };
        if (tf.translation.y - want_y).abs() > 1e-3 {
            tf.translation.y = want_y;
        }

        if !relieved {
            if relief.is_some() {
                // The field went away (layer off, out of coverage, re-anchored to
                // Berlin): back to the shared flat quad.
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
        if relief.is_some_and(|r| r.revision == revision) || built >= MAX_RELIEF_BUILDS_PER_FRAME {
            continue;
        }
        // Where this tile's centre sits in the anchored world frame — the same
        // arithmetic that placed it, so the mesh samples the field under itself.
        let (fcx, fcy) = lat_lng_to_tile_frac(clat, clng, tile.key.z);
        let centre = Vec2::new(
            ((tile.key.x as f64 + 0.5 - fcx) * gs as f64) as f32,
            ((tile.key.y as f64 + 0.5 - fcy) * gs as f64) as f32,
        );
        // Tiles wholly outside the field are flat ground the shared quad already
        // draws; only the ones the field reaches need geometry of their own.
        let reach = crate::systems::terrain::TERRAIN_HALF_M + gs * 0.5;
        if centre.x.abs() > reach || centre.y.abs() > reach {
            continue;
        }
        let mesh =
            crate::systems::terrain::displaced_grid(&field, centre, gs, relief_divisions(gs));
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
        built += 1;
    }
    if built > 0 && *last_revision != revision {
        *last_revision = revision;
        tracing::info!(revision, sink_m = sink, "terrain relief on the basemap");
    }
}

/// The clipmap's desired tiles: every level's ring around its own centre, with the
/// spawn height each tile takes, plus the coarse-first order to fetch them in.
///
/// Split out of [`update_map_stream`] because this is the arithmetic that decides
/// the fan-out — 424 tiles for a z10–z17 pyramid — and the number is worth being
/// able to assert (see the tests).
fn clipmap(
    centres: &[(u32, i64, i64)],
    base_zoom: u32,
    finest_zoom: u32,
) -> (HashMap<TileKey, f32>, Vec<TileKey>) {
    let mut wanted: HashMap<TileKey, f32> = HashMap::new();
    let mut spawn_order: Vec<TileKey> = Vec::new();
    for &(zoom, ctx, cty) in centres {
        let radius = if zoom == finest_zoom {
            LOAD_RADIUS
        } else {
            COARSE_RADIUS
        };
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
    let key = (centres, stream.anchor_generation);
    if stream.desired.key.as_ref() != Some(&key) {
        let (wanted, spawn_order) = clipmap(&key.0, base_zoom, stream.current_zoom);
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
    let gen = stream.anchor_generation;
    for i in 0..stream.desired.spawn_order.len() {
        let key = stream.desired.spawn_order[i];
        let y = stream.desired.wanted[&key];
        // `CellStream::request` de-dups and is also the retry pump — a failed tile
        // waits out `FETCH_RETRY_SECS` and stops being offered once its attempts
        // are spent, so there is no second retry loop.
        stream.tiles.request(key, now);
        if stream.loaded.contains_key(&key) {
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
            StreamTile { key },
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
        stream.loaded.insert(key, entity);
    }

    // Dispatch under TILE_MAX_IN_FLIGHT: the coarse-first queue order is preserved,
    // and what doesn't fit starts on a later frame as slots free.
    let mut queued = 0u32;
    {
        let results = stream.tiles.sink();
        let live_gen = stream.gen_shared.clone();
        while let Some(key) = stream.tiles.next_job(now) {
            spawn_map_tile(key, gen, template.clone(), results.clone(), live_gen.clone());
            queued += 1;
        }
    }
    // Terse fetch accounting: one line per burst, not per tile (served from the
    // byte cache when already seen; only new tiles hit the network).
    if queued > 0 {
        tracing::info!(queued, finest_zoom = stream.current_zoom, "tile fetches queued");
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
    tiles: Query<Entity, (With<StreamTile>, Without<StreamTileReady>, Without<StreamTileFailed>)>,
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
            "tile fill"
        );
    }

    // Cap texture finalizations per frame: a fast pan can land dozens of decoded
    // tiles at once, and uploading them all in one frame hitches. The rest go back
    // into the sink — still holding their concurrency slots, so the bound also
    // bounds how much decoded mip chain is live — and upload next frame.
    const MAX_UPLOADS_PER_FRAME: usize = 8;
    let mut ready = stream.tiles.take_results();
    let deferred = ready.split_off(ready.len().min(MAX_UPLOADS_PER_FRAME));
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
                commands
                    .entity(entity)
                    .insert((MeshMaterial3d(mat), StreamTileReady));
                stream.tiles.succeeded(&key);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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
                ((zoom), (fcx - 0.5).round() as i64, (fcy - 0.5).round() as i64)
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
        let (wanted, order) = clipmap(&centres, DEFAULT_ANCHOR_ZOOM, DEFAULT_ANCHOR_ZOOM);
        assert_eq!(order.len(), 424, "the clipmap fan-out changed");
        assert_eq!(wanted.len(), order.len(), "a tile was wanted twice in one pass");

        // Coarsest first: a fresh anchor gets full, blurry coverage from a handful
        // of fetches before the finer rings refine it.
        assert_eq!(order.first().unwrap().z, MIN_ZOOM);
        assert_eq!(order.last().unwrap().z, DEFAULT_ANCHOR_ZOOM);
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
        let (_, order) = clipmap(&centres, DEFAULT_ANCHOR_ZOOM, DEFAULT_ANCHOR_ZOOM);

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
        assert_eq!(overzoomed, 179, "the overzoomed share of the pyramid changed");
        // Every one of them resolves onto a tile the z14 ring already wants, so
        // they add no distinct body at all.
        let below: std::collections::HashSet<(u32, i64, i64)> = order
            .iter()
            .filter(|k| k.z <= crate::systems::vector_tiles::MAX_SOURCE_ZOOM)
            .map(|k| (k.z, k.x, k.y))
            .collect();
        assert_eq!(sources, below, "an overzoomed request needed a new source tile");
    }
}
