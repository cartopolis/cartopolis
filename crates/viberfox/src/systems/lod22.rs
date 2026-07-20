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
use std::io::Read;
use std::path::PathBuf;
use std::sync::mpsc::Sender;
use std::sync::{Arc, Mutex};

use bevy::prelude::*;
use big_space::prelude::{BigSpace, Grid, GridCell};

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

/// A built cell: its BigSpace grid cell and the merged mesh.
type Lod22Build = (GridCell, Mesh);

/// Camera-following LoD2.2 mesh streamer. Same shape as the building far-cell
/// streamer: a worker builds one cell at a time; cells are kept while in range.
#[derive(Resource, Default)]
pub struct Lod22Buildings {
    /// Anchor tile (tile_x, tile_y, zoom, latitude) from the map origin.
    anchor: Option<(i64, i64, u32, f64)>,
    /// `MapStream::anchor_generation` this streamer is built for; a change resets it.
    synced_gen: u32,
    established: bool,
    /// Worker job queue of CACHE_ZOOM cell coords.
    jobs: Option<Sender<(i64, i64)>>,
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
) {
    let Some((clat, clng, base_zoom)) = stream.geo_anchor() else {
        return;
    };

    // Re-anchor (ADR-025): tear down and re-establish at the new origin. Dropping
    // `jobs` ends the old worker; a fresh `results` orphans any in-flight push.
    if lod.synced_gen != stream.anchor_generation {
        lod.synced_gen = stream.anchor_generation;
        for (_, e) in lod.cells.drain() {
            commands.entity(e).despawn();
        }
        lod.no_shadow.clear();
        lod.requested.clear();
        lod.failed.clear();
        lod.jobs = None;
        lod.results = Arc::new(Mutex::new(Vec::new()));
        lod.anchor = None;
        lod.established = false;
    }

    // Establish the anchor and start the cell worker (fetch + mesh off-thread).
    if !lod.established {
        let z = base_zoom;
        let (ax, ay) = lat_lng_to_tile(clat, clng, z);
        lod.anchor = Some((ax, ay, z, clat));
        lod.established = true;
        let (tx, rx) = std::sync::mpsc::channel::<(i64, i64)>();
        lod.jobs = Some(tx);
        let results = lod.results.clone();
        std::thread::spawn(move || {
            while let Ok((cx, cy)) = rx.recv() {
                let built = build_lod22_cell(cx, cy, ax, ay, z, clat);
                if let Ok(mut g) = results.lock() {
                    g.push(((cx, cy), built));
                }
            }
        });
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
    let Some(jobs) = lod.jobs.clone() else {
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
        let _ = jobs.send(c);
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
    for (c, built) in done {
        match built {
            Ok(Some((grid_cell, mesh))) => {
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
                tracing::info!(cell = ?c, live = lod.cells.len(), "lod22 cell rendered");
            }
            Ok(None) => {} // empty cell; stays requested so it isn't re-queried
            Err(err) => {
                tracing::warn!(cell = ?c, error = %err, "lod22 cell failed");
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
fn build_lod22_cell(
    cx: i64,
    cy: i64,
    ax: i64,
    ay: i64,
    z: u32,
    lat: f64,
) -> Result<Option<Lod22Build>, String> {
    let elements = fetch_lod22_cell(cx, cy)?;
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

        let local: Vec<Vec3> = b
            .p
            .iter()
            .map(|&(la, lo, h)| {
                let w = lat_lng_to_local(la, lo, ax, ay, z, tile_m);
                Vec3::new(w.x - ox, h, w.y - oz)
            })
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
    Ok(buf.into_mesh().map(|m| (grid_cell, m)))
}

/// On-disk cache path for a LoD2.2 cell (its own namespace).
fn cell_cache_path(cx: i64, cy: i64) -> PathBuf {
    PathBuf::from("cache")
        .join("buildings")
        .join("lod22")
        .join(LOD22_ZOOM.to_string())
        .join(cx.to_string())
        .join(format!("{}.json", cy))
}

/// Fetch one LoD2.2 cell from the host (disk-cached).
fn fetch_lod22_cell(cx: i64, cy: i64) -> Result<Vec<Bldg>, String> {
    let path = cell_cache_path(cx, cy);
    if let Ok(cached) = std::fs::read_to_string(&path) {
        if let Ok(b) = parse_lod22(&cached) {
            return Ok(b);
        }
        let _ = std::fs::remove_file(&path);
    }
    let url = format!("{}/lod22/{}/{}/{}.json", tdbag_url(), LOD22_ZOOM, cx, cy);
    // A dense cell decompresses to >10 MB — past ureq's `into_string` cap — so
    // read the body directly (bounded at 64 MB) instead.
    let mut text = String::new();
    ureq::get(&url)
        .call()
        .map_err(|e| e.to_string())?
        .into_reader()
        .take(64 * 1024 * 1024)
        .read_to_string(&mut text)
        .map_err(|e| e.to_string())?;
    let buildings = parse_lod22(&text)?;
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    if let Ok(mut f) = std::fs::File::create(&path) {
        use std::io::Write;
        let _ = f.write_all(text.as_bytes());
    }
    Ok(buildings)
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
