//! 3DBAG **LoD2.2** real building meshes (ADR-027).
//!
//! Where the OSM/LoD1.2 path (`osm_buildings.rs`) extrudes footprints and *guesses*
//! roofs, this streams the actual triangulated BAG roof geometry (sloped, hipped,
//! complex roofs) preprocessed server-side into per-cell meshes
//! (`tools/3dbag_lod22.py` → `https://3dbag.garage44.eu/lod22/…`). Each cell file is
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

use std::collections::{HashMap, HashSet};
use std::sync::{Arc, Mutex};

use bevy::prelude::*;
use bevy::tasks::IoTaskPool;
use big_space::prelude::{BigSpace, Grid, GridCell};

use crate::platform::work::Dispatcher;
use crate::platform::{http, storage};
use crate::systems::free_camera::{FreeCamera, WORLD_CELL_EDGE, WORLD_SWITCH_THRESHOLD};
use crate::systems::globe::{GlobeState, HideInSpace, GLOBE_SHOW_ALT};
use crate::systems::map_stream::MapStream;
use crate::systems::osm_buildings::{
    building_palette, lat_lng_to_local, lod22_covers, tdbag_url, MeshData,
};
use vibe_core::world::{lat_lng_to_tile, tile_to_lat_lng, tile_to_meters};

/// Fixed cache/streaming zoom — must match `osm_buildings::CACHE_ZOOM` (the cells
/// the server binned into, and the cells `osm_buildings` skips).
const LOD22_ZOOM: u32 = 14;

/// Concurrent cell fetch+mesh tasks. Our own 3DBAG host has no external rate limit,
/// so this is a citizenship/throughput cap, not a throttle (min-interval 0).
const LOD22_CONCURRENCY: usize = 3;
/// CACHE_ZOOM cells streamed around the camera at full LoD2.2 detail (radius 2 →
/// 5×5 ≈ ±3.5 km). Covered cells *beyond* this fall back to the simplified,
/// inset, non-shadow far-cell extrusion in `osm_buildings` (a distance LOD): the
/// simplified volume nests inside the real one, so the transition never z-fights.
pub const LOD22_RADIUS: i64 = 2;
/// Cap on cell meshes uploaded per frame (iGPU device-loss guard — see module doc).
const MAX_BUILDS_PER_FRAME: usize = 1;
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

/// A built cell: its BigSpace grid cell, the merged mesh, and one footprint
/// polygon per building (world-XZ, for avatar collision).
type Lod22Build = (GridCell, Mesh, Vec<crate::systems::collision::Footprint>);

/// Camera-following LoD2.2 mesh streamer. Same shape as the building far-cell
/// streamer: a worker builds one cell at a time; cells are kept while in range.
#[derive(Resource, Default)]
pub struct Lod22Buildings {
    /// Anchor tile (tile_x, tile_y, zoom, latitude) from the map origin.
    anchor: Option<(i64, i64, u32, f64)>,
    /// `MapStream::anchor_generation` this streamer is built for; a change resets it.
    synced_gen: u32,
    established: bool,
    /// Frame-gated cell fetch+mesh dispatcher (ADR-033), replacing the old worker
    /// thread + channel.
    dispatch: Dispatcher<(i64, i64)>,
    /// Completed cells from the worker (None for cells with no buildings).
    #[allow(clippy::type_complexity)]
    results: Arc<Mutex<Vec<((i64, i64), Result<Option<Lod22Build>, String>)>>>,
    /// Cells ever queued (kept so completed/empty cells aren't re-queried).
    requested: HashSet<(i64, i64)>,
    /// Failed cells (bounded retry).
    failed: HashMap<(i64, i64), u32>,
    /// Spawned cell entities, despawned when out of range.
    cells: HashMap<(i64, i64), Entity>,
    /// Cells currently marked `NotShadowCaster` (beyond `SHADOW_RADIUS`), so the
    /// shadow-LOD pass only issues a command when a cell actually crosses the
    /// boundary rather than every frame.
    no_shadow: HashSet<(i64, i64)>,
    /// Shared material (white base, per-vertex wall/roof colours).
    material: Option<Handle<StandardMaterial>>,
    /// Last-seen state of the Layers panel's 3DBAG toggle, so the off-edge
    /// teardown in [`update_lod22`] runs exactly once per uncheck.
    enabled_was: bool,
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
) {
    let Some((clat, clng, base_zoom)) = stream.geo_anchor() else {
        return;
    };

    // Re-anchor (ADR-025): tear down and re-establish at the new origin. A fresh
    // `results` orphans any in-flight push; `established` re-inits the dispatcher.
    if lod.synced_gen != stream.anchor_generation {
        lod.synced_gen = stream.anchor_generation;
        for (_, e) in lod.cells.drain() {
            commands.entity(e).despawn();
        }
        colliders.clear_lod22();
        lod.no_shadow.clear();
        lod.requested.clear();
        lod.failed.clear();
        lod.results = Arc::new(Mutex::new(Vec::new()));
        lod.anchor = None;
        lod.established = false;
    }

    // The Layers panel's 3DBAG toggle: on the off-edge, tear down like a
    // re-anchor — despawn the meshes, drop their colliders (the OSM streamers
    // publish replacements as they take the cells over), orphan any in-flight
    // results, and forget `requested` so re-checking re-queues every covered
    // cell. While off, `lod22_covers()` reports no coverage, so the queue loop
    // below stays empty on its own.
    let enabled = crate::systems::osm_buildings::tdbag_user_enabled();
    if lod.enabled_was && !enabled {
        for (_, e) in lod.cells.drain() {
            commands.entity(e).despawn();
        }
        colliders.clear_lod22();
        lod.no_shadow.clear();
        lod.requested.clear();
        lod.failed.clear();
        lod.results = Arc::new(Mutex::new(Vec::new()));
    }
    lod.enabled_was = enabled;

    // Establish the anchor and reset the fetch+mesh dispatcher.
    if !lod.established {
        let z = base_zoom;
        let (ax, ay) = lat_lng_to_tile(clat, clng, z);
        lod.anchor = Some((ax, ay, z, clat));
        lod.established = true;
        lod.dispatch = Dispatcher::new(LOD22_CONCURRENCY, 0.0);
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

    // Queue missing covered cells, nearest first.
    let mut to_request: Vec<(i64, i64)> = Vec::new();
    for dy in -LOD22_RADIUS..=LOD22_RADIUS {
        for dx in -LOD22_RADIUS..=LOD22_RADIUS {
            let c = (ccx + dx, ccy + dy);
            if !lod.requested.contains(&c)
                && *lod.failed.get(&c).unwrap_or(&0) < 3
                && lod22_covers(c.0, c.1)
            {
                to_request.push(c);
            }
        }
    }
    to_request.sort_by_key(|c| (c.0 - ccx).abs().max((c.1 - ccy).abs()));
    for c in to_request {
        lod.requested.insert(c);
        lod.dispatch.queue(c);
    }

    // Dispatch queued cells up to the concurrency cap: each becomes a detached
    // fetch+mesh task (ADR-033 seam) pushing to `results` — no OS thread.
    let now = time.elapsed_secs_f64();
    let results = lod.results.clone();
    while let Some((cx, cy)) = lod.dispatch.next(now) {
        let results = results.clone();
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
                    let built = build_lod22_from_text(&text, cx, cy, ax, ay, z, lat);
                    if let Ok(mut g) = results.lock() {
                        g.push(((cx, cy), built));
                    }
                }));
            })
            .detach();
    }

    // Apply completed cells — at most MAX_BUILDS_PER_FRAME uploads per frame.
    let done: Vec<((i64, i64), Result<Option<Lod22Build>, String>)> = lod
        .results
        .lock()
        .map(|mut g| {
            let n = g.len().min(MAX_BUILDS_PER_FRAME);
            g.drain(..n).collect()
        })
        .unwrap_or_default();
    // Each drained result frees one concurrency slot.
    for _ in 0..done.len() {
        lod.dispatch.completed();
    }
    for (c, built) in done {
        match built {
            Ok(Some((grid_cell, mesh, footprints))) => {
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
                let e = commands
                    .spawn((
                        Mesh3d(meshes.add(mesh)),
                        MeshMaterial3d(material),
                        Transform::from_translation(Vec3::new(0.0, 0.05, 0.0)),
                        grid_cell,
                        ChildOf(root),
                        Lod22Mesh,
                        HideInSpace,
                    ))
                    .id();
                // Shadow-caster state is set by the shadow-LOD pass below.
                lod.cells.insert(c, e);
                // The far tier may already hold this cell — queued before the
                // coverage manifest loaded, or while the 3DBAG toggle was off.
                // Evict it now that the real mesh is up, or both tiers render
                // stacked (see `OsmBuildings::evict_far_cell`).
                osm.evict_far_cell(&mut commands, c);
                tracing::info!(cell = ?c, live = lod.cells.len(), "lod22 cell rendered");
            }
            Ok(None) => {} // empty cell; stays requested so it isn't re-queried
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
                    .spawn(async move { storage::remove(&key).await })
                    .detach();
                *lod.failed.entry(c).or_insert(0) += 1;
                lod.requested.remove(&c); // allow a bounded re-queue later
            }
        }
    }

    // Despawn cells out of range (one cell of hysteresis); re-entry re-requests,
    // served from the disk cache.
    let stale: Vec<(i64, i64)> = lod
        .cells
        .keys()
        .filter(|(x, y)| (x - ccx).abs().max((y - ccy).abs()) > LOD22_RADIUS + 1)
        .copied()
        .collect();
    for c in stale {
        if let Some(e) = lod.cells.remove(&c) {
            commands.entity(e).despawn();
        }
        colliders.remove_cell(c);
        lod.no_shadow.remove(&c);
        lod.requested.remove(&c);
    }

    // Shadow LOD: cells within SHADOW_RADIUS cast shadows, the rest don't. Only act
    // when a cell crosses the boundary (tracked in `no_shadow`), so a stationary
    // camera issues no commands. This is a mesh-level toggle; the forbidden runtime
    // change is the directional lights' `num_cascades` — see CLAUDE.md.
    let snapshot: Vec<((i64, i64), Entity)> = lod.cells.iter().map(|(&k, &v)| (k, v)).collect();
    for ((cx, cy), e) in snapshot {
        let near = (cx - ccx).abs().max((cy - ccy).abs()) <= SHADOW_RADIUS;
        let off = lod.no_shadow.contains(&(cx, cy));
        if near && off {
            commands.entity(e).remove::<bevy::light::NotShadowCaster>();
            lod.no_shadow.remove(&(cx, cy));
        } else if !near && !off {
            commands.entity(e).insert(bevy::light::NotShadowCaster);
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
) -> Result<Option<Lod22Build>, String> {
    let elements = parse_lod22(text)?;
    let (north, west) = tile_to_lat_lng(cx, cy, LOD22_ZOOM);
    let (south, east) = tile_to_lat_lng(cx + 1, cy + 1, LOD22_ZOOM);
    let tile_m = tile_to_meters(z, lat) as f32;

    // Anchor vertices to the BigSpace cell at the cell centre.
    let c_world = lat_lng_to_local((north + south) / 2.0, (west + east) / 2.0, ax, ay, z, tile_m);
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
        let flat = [FLAT_ROOF_COLOR[0], FLAT_ROOF_COLOR[1], FLAT_ROOF_COLOR[2], 1.0];

        // World-XZ position of each vertex (before the cell offset); the hull of
        // these is the building's footprint no-go polygon.
        let world_xz: Vec<Vec2> = b
            .p
            .iter()
            .map(|&(la, lo, _)| lat_lng_to_local(la, lo, ax, ay, z, tile_m))
            .collect();
        if let Some(fp) = crate::systems::collision::Footprint::new(
            crate::systems::collision::convex_hull_xz(world_xz.clone()),
        ) {
            footprints.push(fp);
        }

        let local: Vec<Vec3> = b
            .p
            .iter()
            .zip(&world_xz)
            .map(|(&(_, _, h), &w)| Vec3::new(w.x - ox, h, w.y - oz))
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
    Ok(buf.into_mesh().map(|m| (grid_cell, m, footprints)))
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
    if let Some(cached) = storage::get_string(&key).await {
        return Ok(cached);
    }
    let url = format!("{}/lod22/{}/{}/{}.json", tdbag_url(), LOD22_ZOOM, cx, cy);
    let bytes = http::fetch(http::Request::get(url)).await?;
    let text = String::from_utf8(bytes).map_err(|e| e.to_string())?;
    storage::put(&key, text.as_bytes()).await;
    Ok(text)
}

/// Parse a LoD2.2 cell file: `{"elements":[{p:[[lat,lng,h],…], t:[[i,j,k,roof],…]}]}`.
fn parse_lod22(text: &str) -> Result<Vec<Bldg>, String> {
    let json: serde_json::Value = serde_json::from_str(text).map_err(|e| e.to_string())?;
    let elements = json
        .get("elements")
        .and_then(|e| e.as_array())
        .ok_or_else(|| "no elements".to_string())?;
    let mut out = Vec::with_capacity(elements.len());
    for el in elements {
        let Some(parr) = el.get("p").and_then(|v| v.as_array()) else {
            continue;
        };
        let mut p = Vec::with_capacity(parr.len());
        for pt in parr {
            let a = pt.as_array().ok_or_else(|| "bad vertex".to_string())?;
            let lat = a.first().and_then(|v| v.as_f64());
            let lng = a.get(1).and_then(|v| v.as_f64());
            let h = a.get(2).and_then(|v| v.as_f64());
            if let (Some(lat), Some(lng), Some(h)) = (lat, lng, h) {
                p.push((lat, lng, h as f32));
            }
        }
        let Some(tarr) = el.get("t").and_then(|v| v.as_array()) else {
            continue;
        };
        let mut t = Vec::with_capacity(tarr.len());
        for tr in tarr {
            let a = tr.as_array().ok_or_else(|| "bad triangle".to_string())?;
            let i = a.first().and_then(|v| v.as_u64()).unwrap_or(u64::MAX) as usize;
            let j = a.get(1).and_then(|v| v.as_u64()).unwrap_or(u64::MAX) as usize;
            let k = a.get(2).and_then(|v| v.as_u64()).unwrap_or(u64::MAX) as usize;
            let rf = a.get(3).and_then(|v| v.as_u64()).unwrap_or(0) as u8;
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
