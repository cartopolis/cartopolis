//! 3DBAG **LoD2.2** real building meshes (ADR-027).
//!
//! Where the OSM/LoD1.2 path (`osm_buildings.rs`) extrudes footprints and *guesses*
//! roofs, this streams the actual triangulated BAG roof geometry (sloped, hipped,
//! complex roofs) preprocessed server-side into per-cell meshes
//! (`tools/3dbag_lod22.py` → `https://3dbag.cartopolis.org/lod22/…`). Each cell file is
//! `{"elements":[{id, p:[[lat,lng,h],…], t:[[i,j,k,roof],…]}]}` — deduped vertices
//! and triangles tagged roof/wall.
//!
//! It mirrors the building far-cell streamer: one CACHE_ZOOM cell fetched + meshed
//! per worker turn, anchored to its own BigSpace grid cell for f32 precision, kept
//! while in range. To avoid overlapping the simplified extrusions, `osm_buildings`
//! skips any cell `lod22_covers()` reports — so over covered areas (Groningen) the
//! real meshes are the *only* buildings.
//!
//! Uploads are capped at one cell mesh per frame: a dense central cell is ~240k
//! triangles, and bursting several such uploads into a couple of frames is exactly
//! what trips the Intel iGPU's command-buffer timeout (`VK_ERROR_DEVICE_LOST`, see
//! CLAUDE.md).

use crate::components::{FreeCamera, HideInSpace};
use crate::grid::{WORLD_CELL_EDGE, WORLD_SWITCH_THRESHOLD};
use std::collections::{HashMap, HashSet};

use bevy::prelude::*;
use bevy::tasks::IoTaskPool;
use big_space::prelude::{BigSpace, Grid, GridCell};

use crate::platform::stream::{CellStream, Policy};
use crate::platform::{http, storage};
use crate::systems::globe::{GlobeState, GLOBE_SHOW_ALT};
use crate::systems::map_stream::MapStream;
use crate::systems::osm_buildings::{
    building_palette, lat_lng_to_local, lod22_covers, tdbag_url, MeshData,
};
use cartopolis_core::world::{lat_lng_to_tile, tile_to_lat_lng, tile_to_meters};

/// Fixed cache/streaming zoom — must match `osm_buildings::CACHE_ZOOM` (the cells
/// the server binned into, and the cells `osm_buildings` skips).
const LOD22_ZOOM: u32 = 14;

/// Concurrent cell fetch+mesh tasks. Our own 3DBAG host has no external rate limit,
/// so this is a citizenship/throughput cap, not a throttle (min-interval 0). Six,
/// not three: the fetch is the long pole of a cold load (the 5×5 ring took
/// ceil(25/3) serial rounds of network), the parse runs on the worker pool, and
/// the GPU guard is the per-frame upload cap, not this.
const LOD22_CONCURRENCY: usize = 6;
/// Bounded attempts per cell. A 3DBAG cell that fails three times is either absent
/// upstream or corrupt in the cache (which the error path invalidates), so the
/// fourth attempt would cost a fetch to learn the same thing.
const LOD22_MAX_ATTEMPTS: u32 = 3;

/// Wait after a cell's first failure, doubling twice: 2 s, 4 s, 8 s — and then a
/// minute's rest before the attempt count is forgiven.
///
/// Both were 0.0, and the pair of zeroes turned the reasoning above into something
/// it does not support. "Absent upstream or corrupt" describes a *permanent*
/// condition, but with no backoff the three attempts were spent on three
/// consecutive frames, so a transient network failure produced exactly the same
/// verdict — and with no cooldown that verdict was final, leaving a hole in the
/// LoD2.2 buildings for the life of the map anchor. Three tries spread over
/// fourteen seconds, retried a minute later, tells the two apart.
const LOD22_BACKOFF_SECS: f64 = 2.0;
const LOD22_BACKOFF_DOUBLINGS: u32 = 2;
const LOD22_RETRY_AFTER_SECS: f64 = 60.0;
/// Cells despawned per frame. One is a whole merged LoD2.2 mesh — the single
/// heaviest entity the client frees, which is why the *upload* cap next door is 1 —
/// and a re-anchor retires all `(2·LOD22_RADIUS+1)² = 25` of them at once. Freeing
/// that many in one command buffer is the documented `VK_ERROR_DEVICE_LOST` burst;
/// this streamer used to do exactly that and is the reason `platform::stream` owns
/// the queue now.
const MAX_DESPAWNS_PER_FRAME: usize = 2;
/// CACHE_ZOOM cells streamed around the camera at full LoD2.2 detail (radius 2 →
/// 5×5 ≈ ±3.5 km). Covered cells *beyond* this fall back to the simplified,
/// inset, non-shadow far-cell extrusion in `osm_buildings` (a distance LOD): the
/// simplified volume nests inside the real one, so the transition never z-fights.
const LOD22_RADIUS: i64 = 2;

/// [`LOD22_RADIUS`] with the render budget applied — 1 (3×3) on the phone tier.
///
/// A function rather than a constant because `osm_buildings` derives its own
/// handover boundary from it (`lod22_radius() + 1` is the ring where the far-cell
/// extrusion takes over). The two must be reading the same number or a re-anchor
/// leaves a ring of cells that neither streamer owns; a shared call cannot drift
/// the way two tier-adjusted constants could.
pub fn lod22_radius() -> i64 {
    crate::quality::budget().radius(LOD22_RADIUS)
}
/// Cap on cell meshes uploaded per frame (iGPU device-loss guard — see module doc).
const MAX_BUILDS_PER_FRAME: usize = 1;
/// Largest mesh a single cell may produce; past this the cell's triangles are cut
/// into several ([`MeshData::into_meshes`]).
///
/// **Measured, not chosen for roundness** (2026-08-18, `systems::upload_meter`, a
/// cold cache over Groningen at 1,678 m on the phone tier): the nine cells of the
/// opening view queued 5.8, 5.5, 17, 17.4, 8.3, 24.7, 31.7, 14, 25.8 and **63.4
/// MB**, one cell per frame, ~215 MB over about three and a half seconds. Each of
/// those was one asset, so each was one frame's upload however the streamers were
/// capped — and a burst like that on a mobile driver is the documented way this
/// client loses the GPU (an Adreno 840 did, five seconds into a first run, on
/// 2026-08-17).
///
/// 8 MB rather than something smaller because every extra piece is an extra draw
/// call with its own bind group, on the platform where Bevy issues those from the
/// CPU. At 8 MB the measured set above becomes one piece for six of the nine cells
/// and eight for the worst — about twenty extra draws for the opening view, against
/// a burst cut by a factor of eight. Lower it if a device still hitches on a dense
/// cell; the cost of doing so is visible in `ShotMetrics::draws`.
///
/// This is the **ceiling on the ceiling**; the effective value is
/// [`max_cell_mesh_bytes`], which is never above the frame's own upload budget.
const MAX_CELL_MESH_BYTES: usize = 8 * 1024 * 1024;

/// The largest mesh a cell may produce on *this* run.
///
/// **The two numbers this is made of are meaningless apart, and they had drifted.**
/// `quality::Budget::upload_bytes_per_frame` says how many bytes a frame may hand
/// the GPU, and its own doc points here: bevy's `RenderAssetBytesPerFrame` always
/// writes at least one *whole* asset per frame so uploads can never stall, so an
/// asset bigger than the budget **is** a frame bigger than the budget, and the
/// throttle cannot do anything about it. That made the constant above a promise
/// about the same quantity — and at 8 MB against a 4 MB budget it was promising
/// twice what the budget allowed. The device agreed: the OnePlus 15 log of
/// 2026-08-18 reports `peak_asset_kb = 6103` and loses its Vulkan device a second
/// later.
///
/// So the cap is *derived* rather than chosen: whichever of the two is smaller. A
/// dense cell is cut into more pieces on a machine that asked for a byte budget,
/// and into the same pieces as before on one that did not. Deriving it is what
/// stops the pair drifting again — the previous version of this comment ended with
/// "revisit this together, on the device; neither number means much without the
/// other", which is exactly the kind of instruction nobody is there to follow.
fn max_cell_mesh_bytes() -> usize {
    max_cell_mesh_bytes_for(crate::quality::budget())
}

/// [`max_cell_mesh_bytes`] for an arbitrary budget, so `quality`'s test can assert
/// the relationship on every rung rather than only on the one this run resolved to.
pub(crate) fn max_cell_mesh_bytes_for(budget: &crate::quality::Budget) -> usize {
    budget
        .upload_bytes_per_frame
        .unwrap_or(MAX_CELL_MESH_BYTES)
        .min(MAX_CELL_MESH_BYTES)
}
/// A roof triangle whose |normal.y| exceeds this reads as flat → grey; steeper
/// faces get the tiled-roof tone. So flat commercial roofs and pitched houses
/// colour correctly from the real geometry.
const FLAT_ROOF_NORMAL_Y: f32 = 0.90;
const FLAT_ROOF_COLOR: [f32; 3] = [0.52, 0.52, 0.54];
/// Shadow LOD: only cells within this Chebyshev radius of the camera cast into
/// the sun/moon cascades (radius 1 → the 3×3 around the camera). Distant cells'
/// shadows are sub-pixel, but rasterizing their ~240k-triangle geometry into up
/// to 8 cascade passes is the dominant frame cost — so they're `NotShadowCaster`.
/// Set to a large value to shadow everything, or -1 to disable shadow casting.
/// At 0 only the camera's own cell casts — profiling showed shadow-casting
/// geometry (rendered into up to 8 sun+moon cascades) was the dominant frame
/// cost, and near dense cells carry most of it.
const SHADOW_RADIUS: i64 = 0;
/// Cull back faces. The LoD2.2 solids are consistently wound (outward), and the
/// lat/lng→local mapping preserves orientation, so back-face culling drops the
/// ~half of triangles facing away — a big raster/vertex saving. Set false (→
/// double-sided) if any building renders inside-out.
const BACKFACE_CULL: bool = true;

/// Marker for spawned LoD2.2 meshes.
#[derive(Component)]
pub struct Lod22Mesh;

/// One building's mesh data as received from the host.
struct Bldg {
    /// Vertices as (lat, lng, height-above-own-base).
    p: Vec<(f64, f64, f32)>,
    /// Triangles: indices into `p` plus a roof flag (1 = roof, 0 = wall).
    t: Vec<(usize, usize, usize, u8)>,
}

/// A built cell: its BigSpace grid cell, the merged mesh **in byte-bounded
/// pieces** ([`MAX_CELL_MESH_BYTES`]), and one footprint polygon per building
/// (world-XZ, for avatar collision).
///
/// A `Vec<Mesh>` rather than the single merged mesh this used to be, because one
/// cell is not one size: the dense central cell of Groningen measured 63 MB, and
/// nothing downstream — not the one-cell-per-frame cap here, not bevy's upload
/// budget, which always writes at least one whole asset — can spread an upload
/// that arrives as a single asset.
type Lod22Build = (
    GridCell,
    Vec<Mesh>,
    Vec<crate::systems::collision::Footprint>,
);

/// Camera-following LoD2.2 mesh streamer. Same shape as the building far-cell
/// streamer: a worker builds one cell at a time; cells are kept while in range.
#[derive(Resource)]
pub struct Lod22Buildings {
    /// Anchor tile (tile_x, tile_y, zoom, latitude) from the map origin.
    anchor: Option<(i64, i64, u32, f64)>,
    /// Requested/failed bookkeeping, the results sink, the concurrency bound, the
    /// bounded despawn queue and the anchor generation (`platform::stream`).
    stream: CellStream<(i64, i64), Result<Option<Lod22Build>, String>>,
    /// Spawned cell entities, despawned when out of range — one per mesh piece a
    /// cell was cut into ([`MAX_CELL_MESH_BYTES`]), which for most cells is still
    /// exactly one.
    cells: HashMap<(i64, i64), Vec<Entity>>,
    /// Cells currently marked `NotShadowCaster` (beyond `SHADOW_RADIUS`), so the
    /// shadow-LOD pass only issues a command when a cell actually crosses the
    /// boundary rather than every frame.
    no_shadow: HashSet<(i64, i64)>,
    /// Shared material (white base, per-vertex wall/roof colours).
    material: Option<Handle<StandardMaterial>>,
    /// Last-seen state of the Layers panel's 3DBAG toggle, so the off-edge
    /// teardown in [`update_lod22`] runs exactly once per uncheck.
    enabled_was: bool,
    /// Terrain-field revision these cells were built against. The field arrives a
    /// second or two after the anchor, so the first cells are always built on flat
    /// ground and have to be rebuilt onto the measured one.
    terrain_revision: u32,
}

impl Lod22Buildings {
    /// Are 3DBAG cells still being fetched or meshed? Read by the `--shot`
    /// harness's settle test — these are the buildings a city shot is *of*.
    #[must_use]
    pub fn streaming_busy(&self) -> bool {
        self.stream.busy()
    }
}

impl Default for Lod22Buildings {
    fn default() -> Self {
        Self {
            anchor: None,
            stream: CellStream::new(Policy {
                max_in_flight: LOD22_CONCURRENCY,
                min_interval_secs: 0.0,
                max_attempts: LOD22_MAX_ATTEMPTS,
                backoff_secs: LOD22_BACKOFF_SECS,
                backoff_doublings: LOD22_BACKOFF_DOUBLINGS,
                cooldown_secs: LOD22_RETRY_AFTER_SECS,
                despawns_per_frame: MAX_DESPAWNS_PER_FRAME,
            }),
            cells: HashMap::new(),
            no_shadow: HashSet::new(),
            material: None,
            enabled_was: true,
            terrain_revision: 0,
        }
    }
}

/// Stream 3DBAG LoD2.2 cells around the camera (ADR-027).
pub fn update_lod22(
    mut commands: Commands,
    mut lod: ResMut<Lod22Buildings>,
    mut meshes: ResMut<Assets<Mesh>>,
    mut materials: ResMut<Assets<StandardMaterial>>,
    stream: Res<MapStream>,
    camera: Query<(&GridCell, &Transform), With<FreeCamera>>,
    bigspace: Query<Entity, With<BigSpace>>,
    globe: Res<GlobeState>,
    time: Res<Time>,
    mut colliders: ResMut<crate::systems::collision::BuildingColliders>,
    mut osm: ResMut<crate::systems::osm_buildings::OsmBuildings>,
    terrain: Res<crate::systems::terrain::TerrainField>,
) {
    let Some((clat, clng, base_zoom)) = stream.geo_anchor() else {
        return;
    };

    // Free retired cells at MAX_DESPAWNS_PER_FRAME, ahead of every early return
    // below: a re-anchor at ground level must keep clearing even if the camera then
    // climbs into space and the streamer freezes.
    lod.stream.drain_despawns(&mut commands);
    let now = time.elapsed_secs_f64();

    // Re-anchor (ADR-025): tear down and re-establish at the new origin. The
    // teardown goes through the shared despawn queue rather than freeing 25 merged
    // meshes in one frame, which is what it used to do; `CellStream::reset` orphans
    // any in-flight push. The ground moving under the buildings invalidates them
    // exactly as a re-anchor does — every base was computed against the field as it
    // stood — so it takes the same path. `sync` runs unconditionally so it adopts
    // the new generation either way.
    let reanchored = lod.stream.sync(stream.anchor_generation);
    if reanchored || lod.terrain_revision != terrain.revision {
        lod.terrain_revision = terrain.revision;
        let retired: Vec<Entity> = lod.cells.drain().flat_map(|(_, e)| e).collect();
        lod.stream.retire_all(retired);
        colliders.clear_lod22();
        lod.no_shadow.clear();
        lod.stream.reset();
        lod.anchor = None;
    }

    // The Layers panel's 3DBAG toggle: on the off-edge, tear down like a
    // re-anchor — retire the meshes (through the shared queue, at the per-frame
    // cap), drop their colliders (the OSM streamers publish replacements as they
    // take the cells over), orphan any in-flight results, and forget `requested`
    // so re-checking re-queues every covered cell. While off, `lod22_covers()`
    // reports no coverage, so the queue loop below stays empty on its own.
    // Both toggles, not just the 3DBAG one: the master Buildings switch reached
    // only visibility until now, so unticking it left this streamer fetching and
    // meshing every covered cell to draw nothing.
    let enabled = crate::systems::osm_buildings::buildings_user_enabled()
        && crate::systems::osm_buildings::tdbag_user_enabled();
    if lod.enabled_was && !enabled {
        let retired: Vec<Entity> = lod.cells.drain().flat_map(|(_, e)| e).collect();
        lod.stream.retire_all(retired);
        colliders.clear_lod22();
        lod.no_shadow.clear();
        lod.stream.reset();
    }
    lod.enabled_was = enabled;
    if !enabled {
        // Nothing below this point may run: the queue loop would re-request every
        // covered cell. The despawn drain above has already happened, so a ring
        // retired by the edge case just above still clears at its per-frame cap.
        return;
    }

    // Establish the anchor.
    if lod.anchor.is_none() {
        let z = base_zoom;
        let (ax, ay) = lat_lng_to_tile(clat, clng, z);
        lod.anchor = Some((ax, ay, z, clat));
    }
    let (ax, ay, z, lat) = lod.anchor.unwrap();
    let tile_m = tile_to_meters(z, lat) as f32;

    let Ok((cell, tf)) = camera.single() else {
        return;
    };
    // Freeze from orbit (same as map streaming / buildings), using geocentric
    // altitude so it's right on the far side of the globe.
    if globe.altitude_m(cell, tf) as f32 > GLOBE_SHOW_ALT {
        return;
    }
    let world_x = cell.x as f32 * WORLD_CELL_EDGE + tf.translation.x;
    let world_z = cell.z as f32 * WORLD_CELL_EDGE + tf.translation.z;
    let center = (
        ax + (world_x / tile_m).round() as i64,
        ay + (world_z / tile_m).round() as i64,
    );

    let Ok(root) = bigspace.single() else {
        return;
    };

    // Camera's CACHE_ZOOM cell from its base-zoom tile coords.
    let shift = z.saturating_sub(LOD22_ZOOM);
    let (ccx, ccy) = (
        center.0.div_euclid(1i64 << shift),
        center.1.div_euclid(1i64 << shift),
    );

    // Queue missing covered cells, nearest first. `CellStream::request` is also the
    // retry pump — it refuses a cell that has spent its attempts.
    let mut to_request: Vec<(i64, i64)> = Vec::new();
    let radius = lod22_radius();
    for dy in -radius..=radius {
        for dx in -radius..=radius {
            let c = (ccx + dx, ccy + dy);
            if lod22_covers(c.0, c.1) {
                to_request.push(c);
            }
        }
    }
    to_request.sort_by_key(|c| (c.0 - ccx).abs().max((c.1 - ccy).abs()));
    for c in to_request {
        lod.stream.request(c, now);
    }

    // Dispatch queued cells up to the concurrency cap: each becomes a detached
    // fetch+mesh task (ADR-033 seam) pushing to the sink — no OS thread.
    let results = lod.stream.sink();
    while let Some((cx, cy)) = lod.stream.next_job(now) {
        let results = results.clone();
        // The terrain travels with the job: a building's base is one height for the
        // whole footprint, and the footprints only exist inside the worker.
        let ground = terrain.snapshot();
        // Two halves on two executors, matching the tile path (ADR-033): the fetch
        // is awaited here — free on wasm, where this is the main thread but the
        // browser does the transfer — while the parse and mesh, which are pure
        // arithmetic and cost a core, go to `platform::workers`. Running them here
        // is what put `serde_json` at ~46% of the main thread's non-idle time.
        IoTaskPool::get()
            .spawn(async move {
                let text = match fetch_lod22_text(cx, cy).await {
                    Ok(t) => t,
                    Err(e) => {
                        if let Ok(mut g) = results.lock() {
                            g.push(((cx, cy), Err(e)));
                        }
                        return;
                    }
                };
                crate::platform::workers::submit(Box::new(move || {
                    let built = build_lod22_from_text(&text, cx, cy, ax, ay, z, lat, &ground);
                    if let Ok(mut g) = results.lock() {
                        g.push(((cx, cy), built));
                    }
                }));
            })
            .detach();
    }

    // Apply completed cells — at most MAX_BUILDS_PER_FRAME uploads per frame; the
    // rest go back into the sink still holding their concurrency slots.
    let mut done = lod.stream.take_results();
    let deferred = done.split_off(done.len().min(MAX_BUILDS_PER_FRAME));
    lod.stream.defer(deferred);
    for (c, built) in done {
        match built {
            Ok(Some((grid_cell, pieces, footprints))) => {
                colliders.set_cell(c, footprints);
                let material = lod
                    .material
                    .get_or_insert_with(|| {
                        materials.add(StandardMaterial {
                            base_color: Color::WHITE,
                            perceptual_roughness: 0.95,
                            double_sided: !BACKFACE_CULL,
                            cull_mode: if BACKFACE_CULL {
                                Some(bevy::render::render_resource::Face::Back)
                            } else {
                                None
                            },
                            ..default()
                        })
                    })
                    .clone();
                // One entity per piece, flat under the same root rather than a
                // parent with children: every consumer below — the shadow-LOD
                // toggle, `HideInSpace`, the despawn queue — acts per mesh entity,
                // and a parent would mean each of them learning to walk children
                // for a hierarchy with nothing else to do. A cell that fits in one
                // piece is bit-identical to what this spawned before.
                let entities: Vec<Entity> = pieces
                    .into_iter()
                    .map(|mesh| {
                        commands
                            .spawn((
                                Mesh3d(meshes.add(mesh)),
                                MeshMaterial3d(material.clone()),
                                Transform::from_translation(Vec3::new(0.0, 0.05, 0.0)),
                                grid_cell,
                                ChildOf(root),
                                Lod22Mesh,
                                HideInSpace,
                            ))
                            .id()
                    })
                    .collect();
                let pieces = entities.len();
                // Shadow-caster state is set by the shadow-LOD pass below.
                lod.cells.insert(c, entities);
                lod.stream.succeeded(&c);
                // The far tier may already hold this cell — queued before the
                // coverage manifest loaded, or while the 3DBAG toggle was off.
                // Evict it now that the real mesh is up, or both tiers render
                // stacked (see `OsmBuildings::evict_far_cell`).
                osm.evict_far_cell(&mut commands, c);
                tracing::info!(
                    cell = ?c,
                    live = lod.cells.len(),
                    pieces,
                    "lod22 cell rendered"
                );
            }
            // Empty cell; stays requested so it isn't re-queried.
            Ok(None) => lod.stream.succeeded(&c),
            Err(err) => {
                tracing::warn!(cell = ?c, error = %err, "lod22 cell failed");
                // Drop any cached copy. The fetch no longer validates a cache hit
                // by parsing it (that parse moved to a worker), so this is what
                // stops a corrupt entry being re-served forever: whatever failed,
                // the stored text is the one thing we can invalidate. Harmless
                // when the failure came from the network instead — the key simply
                // isn't there.
                let key = cell_cache_key(c.0, c.1);
                IoTaskPool::get()
                    .spawn(async move { storage::remove(storage::Namespace::Cache, &key).await })
                    .detach();
                lod.stream.failed(&c, now); // allows a bounded re-queue later
            }
        }
    }

    // Despawn cells out of range (one cell of hysteresis); re-entry re-requests,
    // served from the disk cache.
    let stale: Vec<(i64, i64)> = lod
        .cells
        .keys()
        .filter(|(x, y)| (x - ccx).abs().max((y - ccy).abs()) > lod22_radius() + 1)
        .copied()
        .collect();
    for c in stale {
        if let Some(entities) = lod.cells.remove(&c) {
            for e in entities {
                lod.stream.retire(e);
            }
        }
        colliders.remove_cell(c);
        lod.no_shadow.remove(&c);
        lod.stream.forget(&c);
    }

    // Shadow LOD: cells within SHADOW_RADIUS cast shadows, the rest don't. Only act
    // when a cell crosses the boundary (tracked in `no_shadow`), so a stationary
    // camera issues no commands. This is a mesh-level toggle; the forbidden runtime
    // change is the directional lights' `num_cascades` — see CLAUDE.md.
    let snapshot: Vec<((i64, i64), Vec<Entity>)> =
        lod.cells.iter().map(|(&k, v)| (k, v.clone())).collect();
    for ((cx, cy), entities) in snapshot {
        let near = (cx - ccx).abs().max((cy - ccy).abs()) <= SHADOW_RADIUS;
        let off = lod.no_shadow.contains(&(cx, cy));
        // The whole cell crosses the boundary at once — it is one cell's worth of
        // geometry however many pieces it was cut into — so the `no_shadow` set
        // stays keyed by cell and the commands fan out over its entities.
        if near && off {
            for e in entities {
                commands.entity(e).remove::<bevy::light::NotShadowCaster>();
            }
            lod.no_shadow.remove(&(cx, cy));
        } else if !near && !off {
            for e in entities {
                commands.entity(e).insert(bevy::light::NotShadowCaster);
            }
            lod.no_shadow.insert((cx, cy));
        }
    }
}

/// Fetch one cell (disk-cached) and build its merged LoD2.2 mesh, anchored to the
/// cell's own BigSpace grid cell. Runs on the worker thread. `None` if empty.
/// The CPU half: JSON text in, finished mesh out. Pure — no I/O, no globals, and
/// every input owned — which is what lets [`update_lod22`] hand it to
/// [`platform::workers`] instead of running it on the thread that renders.
///
/// That split is the whole point of the function existing separately. Profiling
/// the web build put `serde_json` at ~46% of the main thread's non-idle time,
/// with `drop_glue::<Value>` the single largest entry, because this ran inside an
/// `IoTaskPool` future — and on wasm `IoTaskPool` is the main thread, Bevy having
/// no `multi_threaded` there. Awaiting a fetch on it costs nothing; parsing a
/// megabyte of CityJSON and meshing it costs a frame.
///
/// [`platform::workers`]: crate::platform::workers
fn build_lod22_from_text(
    text: &str,
    cx: i64,
    cy: i64,
    ax: i64,
    ay: i64,
    z: u32,
    lat: f64,
    ground: &crate::systems::terrain::TerrainSnapshot,
) -> Result<Option<Lod22Build>, String> {
    let elements = parse_lod22(text)?;
    let (north, west) = tile_to_lat_lng(cx, cy, LOD22_ZOOM);
    let (south, east) = tile_to_lat_lng(cx + 1, cy + 1, LOD22_ZOOM);
    let tile_m = tile_to_meters(z, lat) as f32;

    // Anchor vertices to the BigSpace cell at the cell centre.
    let c_world = lat_lng_to_local(
        (north + south) / 2.0,
        (west + east) / 2.0,
        ax,
        ay,
        z,
        tile_m,
    );
    let (grid_cell, _) = Grid::new(WORLD_CELL_EDGE, WORLD_SWITCH_THRESHOLD)
        .translation_to_grid(Vec3::new(c_world.x, 0.0, c_world.y).as_dvec3());
    let ox = grid_cell.x as f32 * WORLD_CELL_EDGE;
    let oz = grid_cell.z as f32 * WORLD_CELL_EDGE;

    // Deduplicating: the host sends each building with a shared vertex list plus an
    // index list, and the loop below expands it triangle-by-triangle. Without this
    // a dense cell ballooned to three unshared vertices per triangle (~720k for a
    // ~240k-triangle cell) behind a useless identity index buffer.
    let mut buf = MeshData::with_dedup();
    // One footprint polygon per building, in world XZ (for avatar collision).
    let mut footprints: Vec<crate::systems::collision::Footprint> = Vec::new();
    for b in &elements {
        // The server bins each building to the cell owning its first vertex; keep
        // the same half-open ownership so a cross-edge building renders once.
        let (blat, blng, _) = b.p[0];
        if blat < south || blat >= north || blng < west || blng >= east {
            continue;
        }
        let (wall, roof_tile) = building_palette(blat, blng);
        let wall = [wall[0], wall[1], wall[2], 1.0];
        let roof_tile = [roof_tile[0], roof_tile[1], roof_tile[2], 1.0];
        // `FLAT_ROOF_COLOR` is authored in sRGB; vertex colours multiply in
        // linear space (same exit rule as `building_palette`).
        let flat_lin = crate::systems::scatter::srgb_to_linear_f32(FLAT_ROOF_COLOR);
        let flat = [flat_lin[0], flat_lin[1], flat_lin[2], 1.0];

        // World-XZ position of each vertex (before the cell offset); the hull of
        // these is the building's footprint no-go polygon.
        let world_xz: Vec<Vec2> =
            b.p.iter()
                .map(|&(la, lo, _)| lat_lng_to_local(la, lo, ax, ay, z, tile_m))
                .collect();
        if let Some(fp) = crate::systems::collision::Footprint::new(
            crate::systems::collision::convex_hull_xz(world_xz.clone()),
        ) {
            footprints.push(fp);
        }

        // The whole building steps up to the ground under it — one height for the
        // footprint, never per vertex. 3DBAG's vertices are heights above the
        // building's *own* base, so displacing them individually would rack the
        // walls out of plumb and tilt the roof; a building on a hillside is level,
        // it is the ground that slopes.
        let base = ground.level_over(world_xz.iter().map(|w| [w.x, w.y]));
        let local: Vec<Vec3> =
            b.p.iter()
                .zip(&world_xz)
                .map(|(&(_, _, h), &w)| Vec3::new(w.x - ox, h + base, w.y - oz))
                .collect();
        for &(i, j, k, is_roof) in &b.t {
            let (a, bb, c) = (local[i], local[j], local[k]);
            let color = if is_roof == 1 {
                let n = (bb - a).cross(c - a).normalize_or_zero();
                if n.y.abs() > FLAT_ROOF_NORMAL_Y {
                    flat
                } else {
                    roof_tile
                }
            } else {
                wall
            };
            buf.tri(a, bb, c, color);
        }
    }
    let pieces = buf.into_meshes(max_cell_mesh_bytes());
    Ok((!pieces.is_empty()).then_some((grid_cell, pieces, footprints)))
}

/// Cache key for a LoD2.2 cell (its own namespace under the cache root).
fn cell_cache_key(cx: i64, cy: i64) -> String {
    format!("buildings/lod22/{}/{}/{}.json", LOD22_ZOOM, cx, cy)
}

/// Fetch one LoD2.2 cell from the host, cached through the storage seam. Runs on
/// an `IoTaskPool` task — no thread, no `ureq`, no `std::fs` (ADR-033), so it is
/// wasm-buildable.
/// The network half: cache lookup, fetch, cache write — and no parsing.
///
/// It used to parse a cache hit to validate it, discarding the entry if that
/// failed. That check has moved to the caller, which now knows a parse failed and
/// evicts the key then; doing it here would mean parsing on this thread, which is
/// exactly what [`build_lod22_from_text`] exists to avoid.
async fn fetch_lod22_text(cx: i64, cy: i64) -> Result<String, String> {
    let key = cell_cache_key(cx, cy);
    if let Some(cached) = storage::get_string(storage::Namespace::Cache, &key).await {
        return Ok(cached);
    }
    let url = format!("{}/lod22/{}/{}/{}.json", tdbag_url(), LOD22_ZOOM, cx, cy);
    let bytes = http::fetch(http::Request::get(url)).await?;
    let text = String::from_utf8(bytes).map_err(|e| e.to_string())?;
    storage::put(storage::Namespace::Cache, &key, text.as_bytes()).await;
    Ok(text)
}

/// Parse a LoD2.2 cell file: `{"elements":[{p:[[lat,lng,h],…], t:[[i,j,k,roof],…]}]}`.
///
/// Typed `Deserialize`, not a `serde_json::Value` walk: the DOM route boxed
/// every coordinate of a ~4 MB cell in a `Value` and re-inspected it in our own
/// code — a dense central cell (~240k triangles) is mostly numbers, so the walk
/// was a large share of the per-cell cost on a cold load. The triangle row gets
/// a hand-written seq visitor (an `untagged` enum would buffer each row and
/// give the win back); the missing-roof-flag row still reads as wall, matching
/// the old lenient parser.
fn parse_lod22(text: &str) -> Result<Vec<Bldg>, String> {
    #[derive(serde::Deserialize)]
    struct File {
        elements: Vec<El>,
    }
    #[derive(serde::Deserialize)]
    struct El {
        #[serde(default)]
        p: Vec<(f64, f64, f32)>,
        #[serde(default)]
        t: Vec<Tri>,
    }
    struct Tri(u32, u32, u32, u8);
    impl<'de> serde::Deserialize<'de> for Tri {
        fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
            struct V;
            impl<'de> serde::de::Visitor<'de> for V {
                type Value = Tri;
                fn expecting(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
                    f.write_str("[i, j, k, roof?]")
                }
                fn visit_seq<A: serde::de::SeqAccess<'de>>(
                    self,
                    mut seq: A,
                ) -> Result<Tri, A::Error> {
                    let missing = || serde::de::Error::custom("bad triangle");
                    let i = seq.next_element()?.ok_or_else(missing)?;
                    let j = seq.next_element()?.ok_or_else(missing)?;
                    let k = seq.next_element()?.ok_or_else(missing)?;
                    let rf = seq.next_element()?.unwrap_or(0);
                    Ok(Tri(i, j, k, rf))
                }
            }
            d.deserialize_seq(V)
        }
    }

    let file: File = serde_json::from_str(text).map_err(|e| e.to_string())?;
    let mut out = Vec::with_capacity(file.elements.len());
    for el in file.elements {
        let p = el.p;
        let mut t = Vec::with_capacity(el.t.len());
        for Tri(i, j, k, rf) in el.t {
            let (i, j, k) = (i as usize, j as usize, k as usize);
            if i < p.len() && j < p.len() && k < p.len() {
                t.push((i, j, k, rf));
            }
        }
        if p.len() >= 3 && !t.is_empty() {
            out.push(Bldg { p, t });
        }
    }
    Ok(out)
}
