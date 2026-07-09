//! OSM 3D buildings (ADR-020/022) — a camera-following building patch.
//!
//! Fetches OSM building footprints around the camera from the Overpass API and
//! extrudes each footprint to its `height` / `building:levels` tag — both on a
//! background thread, so the main thread only turns the prepared vertex buffers
//! into assets/entities. Two rings: full detail (windows, roof shapes) near the
//! camera, and a plain-extrusion LOD ring out to `FAR_PATCH_RADIUS` so buildings
//! continue toward the horizon. As the camera roams, the patch is re-fetched and
//! the previous set despawned, so buildings follow the streamed ground
//! (`map_stream`).
//!
//! Placement maps each vertex through the same Web Mercator math as the tiles
//! (relative to the anchor region tile); heights use the interim quad-to-metre
//! scale until the real-metre frame (ADR-019) lands.

use std::collections::HashSet;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use bevy::prelude::*;
use bevy::render::mesh::{Indices, PrimitiveTopology};
use bevy::render::render_asset::RenderAssetUsages;
use big_space::prelude::{BigSpace, Grid, GridCell};

use crate::components::Region;
use crate::systems::free_camera::{FreeCamera, WORLD_CELL_EDGE, WORLD_SWITCH_THRESHOLD};
use vibe_core::world::{lat_lng_to_tile, tile_to_lat_lng, tile_to_meters};

/// Fixed grid zoom at which building data is fetched + cached. Independent of the
/// requested patch size, so changing the patch radius never invalidates the cache
/// (each cell is `cache/buildings/{CACHE_ZOOM}/{x}/{y}.json`). Kept coarse (few,
/// large cells) so a patch is a handful of Overpass queries, not dozens.
const CACHE_ZOOM: u32 = 14;
/// Throttle between live Overpass queries so we don't trip its rate limit (429).
const OVERPASS_THROTTLE: Duration = Duration::from_millis(800);

/// Roof form from OSM `roof:shape` (Simple 3D Buildings).
#[derive(Clone, Copy, Debug, PartialEq)]
enum RoofShape {
    Flat,
    Gabled,
    Hipped,
    Pyramidal,
    Skillion,
}

/// One OSM building: OSM way id (for cross-cell dedup + future per-building
/// conversion), footprint in (lat, lng), height in metres, wall/roof colours, and
/// roof form/height derived from its tags (with fallbacks).
#[derive(Clone, Debug)]
struct Building {
    id: i64,
    ring: Vec<(f64, f64)>,
    /// Either total height (from `height`) or eaves/wall height (from levels or an
    /// estimate) — `height_is_eaves` says which, so the roof sits correctly.
    height_m: f32,
    height_is_eaves: bool,
    wall_color: [f32; 3],
    roof_color: [f32; 3],
    roof_shape: RoofShape,
    /// Explicit `roof:height` in metres, if tagged.
    roof_height: Option<f32>,
    /// `roof:orientation=across` (ridge across the long axis instead of along it).
    roof_across: bool,
}

/// Which stage of the two-stage patch build a worker result belongs to. The
/// detail patch is a handful of Overpass cells and lands within seconds; the far
/// ring is many more cells (throttled) and follows behind, reusing the detail
/// stage's disk-cached cells.
enum PatchStage {
    Detail,
    Far,
}

/// A patch fully prepared on the background thread — fetched footprints *and*
/// built meshes — so the main thread only creates assets and entities (no
/// frame hitch from extruding thousands of buildings).
struct BuiltPatch {
    /// Footprints (detail + far), retained for hit-testing / prim conversion.
    buildings: Vec<Building>,
    /// BigSpace cell the mesh vertices are relative to.
    patch_cell: GridCell,
    /// Full-detail buildings around the camera.
    detail: Option<Mesh>,
    /// Window quads per glow bucket (indexes `WINDOW_GLOW_MULT`).
    windows: [Option<Mesh>; 4],
    /// Simplified far-ring buildings out to `FAR_PATCH_RADIUS`.
    far: Option<Mesh>,
}

/// Camera-following building patch: re-fetched from Overpass when the camera
/// drifts, with the old set despawned (ADR-020/022). Anchored to the region tile
/// so buildings stay aligned with the streamed ground.
#[derive(Resource, Default)]
pub struct OsmBuildings {
    /// Region anchor: tile_x, tile_y, zoom, latitude.
    anchor: Option<(i64, i64, u32, f64)>,
    /// Centre tile of the last fetch (re-fetch when the camera moves away).
    last_center: Option<(i64, i64)>,
    in_flight: bool,
    /// Completed fetch+build stages, consumed in order on the main thread.
    fetch: Arc<Mutex<Vec<(PatchStage, Result<BuiltPatch, String>)>>>,
    /// Detail-ring mesh entities (buildings + windows), despawned on re-fetch.
    detail_entities: Vec<Entity>,
    /// Far LOD ring mesh entity, despawned when its replacement arrives.
    far_entities: Vec<Entity>,
    /// Per-building data for the current patch, retained so an individual building
    /// can later be hit-tested and converted to an editable prim (ADR-020/021).
    buildings: Vec<Building>,
}

/// Marker for spawned building meshes.
#[derive(Component)]
pub struct OsmBuildingMesh;

/// An emissive window mesh; the value is its brightness multiplier (0 = never lit),
/// scaled by the night factor in `update_window_glow`.
#[derive(Component)]
pub struct WindowGlow(pub f32);

/// Default metres per storey when only `building:levels` is present.
const STOREY_HEIGHT: f32 = 3.0;

/// Only buildings with eaves at least this tall get windows (skips houses/sheds).
const WINDOW_MIN_HEIGHT: f32 = 8.0;
/// Window grid: column pitch, size, floor spacing, first-floor sill (metres).
const WINDOW_PITCH: f32 = 2.6;
const WINDOW_W: f32 = 1.3;
const WINDOW_H: f32 = 1.5;
const WINDOW_FLOOR: f32 = 3.0;
const WINDOW_SILL: f32 = 1.2;
/// Per-window brightness buckets: bucket 0 stays dark, the rest glow at these
/// multipliers — so at night some windows are off, some dim, some bright.
const WINDOW_GLOW_MULT: [f32; 4] = [0.0, 0.5, 0.9, 1.35];

/// Tiles out from the camera covered by the full-detail building patch
/// (windows, roof shapes).
const BUILDING_PATCH_RADIUS: i64 = 3;
/// Tiles out from the camera covered by the far LOD mesh: simplified extrusions
/// with flat caps (no windows, no roof shapes), all merged into one extra draw
/// call, so buildings continue toward the horizon at little cost. Covers the
/// detail area too as a flicker-free fallback during patch refreshes.
const FAR_PATCH_RADIUS: i64 = 16;
/// Re-fetch the patch once the camera has moved this many tiles from the last centre.
/// Larger = fewer Overpass calls (avoids 429/504 rate limits) at the cost of the
/// patch trailing the camera a bit further before refreshing.
const REFETCH_TILES: i64 = 4;

/// Muted, realistic wall tones (tans, greys, beige, muted brick, off-white).
const BUILDING_COLORS: [[f32; 3]; 8] = [
    [0.80, 0.76, 0.70],
    [0.72, 0.70, 0.66],
    [0.78, 0.72, 0.64],
    [0.68, 0.66, 0.64],
    [0.76, 0.63, 0.56],
    [0.83, 0.81, 0.77],
    [0.70, 0.65, 0.58],
    [0.74, 0.71, 0.69],
];

/// Tiled-roof tones for residential buildings without a `roof:*` tag (terracotta,
/// brown-red, dark tile, grey tile) — the characteristic look of a Dutch town.
const ROOF_COLORS: [[f32; 3]; 5] = [
    [0.62, 0.30, 0.24],
    [0.55, 0.34, 0.26],
    [0.45, 0.32, 0.28],
    [0.50, 0.50, 0.52],
    [0.40, 0.28, 0.25],
];

/// Re-fetch and respawn the building patch as the camera roams (ADR-020/022).
pub fn update_buildings(
    mut commands: Commands,
    mut osm: ResMut<OsmBuildings>,
    mut meshes: ResMut<Assets<Mesh>>,
    mut materials: ResMut<Assets<StandardMaterial>>,
    regions: Query<&Region>,
    camera: Query<(&GridCell, &Transform), With<FreeCamera>>,
    bigspace: Query<Entity, With<BigSpace>>,
) {
    // Establish the region anchor once.
    if osm.anchor.is_none() {
        let Some(region) = regions.iter().next() else {
            return;
        };
        let z = region.tile_z.clamp(0, u32::MAX as i64) as u32;
        osm.anchor = Some((region.tile_x, region.tile_y, z, region.latitude));
    }
    let (ax, ay, z, lat) = osm.anchor.unwrap();
    let tile_m = tile_to_meters(z, lat) as f32;

    // Camera's current tile (world position → tile offset from the anchor).
    let Ok((cell, tf)) = camera.single() else {
        return;
    };
    let world_x = cell.x as f32 * WORLD_CELL_EDGE + tf.translation.x;
    let world_z = cell.z as f32 * WORLD_CELL_EDGE + tf.translation.z;
    let center = (
        ax + (world_x / tile_m).round() as i64,
        ay + (world_z / tile_m).round() as i64,
    );

    // Kick off a re-fetch when the camera has drifted far enough.
    let need = match osm.last_center {
        None => true,
        Some((lx, ly)) => (center.0 - lx).abs() + (center.1 - ly).abs() >= REFETCH_TILES,
    };
    if need && !osm.in_flight {
        osm.in_flight = true;
        osm.last_center = Some(center);
        let slot = osm.fetch.clone();

        // Bounding boxes for the two stages; each fetch assembles its box from
        // fixed CACHE_ZOOM cells, so the far stage reuses the detail stage's
        // freshly-cached cells and only pays for the outer ring.
        let rd = BUILDING_PATCH_RADIUS;
        let (d_north, d_west) = tile_to_lat_lng(center.0 - rd, center.1 - rd, z);
        let (d_south, d_east) = tile_to_lat_lng(center.0 + rd + 1, center.1 + rd + 1, z);
        let rf = FAR_PATCH_RADIUS;
        let (f_north, f_west) = tile_to_lat_lng(center.0 - rf, center.1 - rf, z);
        let (f_south, f_east) = tile_to_lat_lng(center.0 + rf + 1, center.1 + rf + 1, z);

        // Everything below runs on the worker: the Overpass fetches AND the mesh
        // building (merging thousands of extrusions into shared vertex buffers
        // used to hitch the main thread for a frame). Vertices are relative to
        // the patch's BigSpace cell so f32 stays precise at any range.
        let (patch_cell, _) = Grid::new(WORLD_CELL_EDGE, WORLD_SWITCH_THRESHOLD)
            .translation_to_grid(Vec3::new(world_x, 0.0, world_z).as_dvec3());
        let detail_half = (BUILDING_PATCH_RADIUS as f32 + 0.5) * tile_m;
        std::thread::spawn(move || {
            let cell_ox = patch_cell.x as f32 * WORLD_CELL_EDGE;
            let cell_oz = patch_cell.z as f32 * WORLD_CELL_EDGE;
            // Footprint → (cell-local ring, camera-relative Chebyshev distance).
            let local_ring = |b: &Building| {
                let world_ring: Vec<Vec2> = b
                    .ring
                    .iter()
                    .map(|&(la, lo)| lat_lng_to_local(la, lo, ax, ay, z, tile_m))
                    .collect();
                let centroid =
                    world_ring.iter().copied().sum::<Vec2>() / world_ring.len().max(1) as f32;
                let ring: Vec<Vec2> = world_ring
                    .iter()
                    .map(|w| Vec2::new(w.x - cell_ox, w.y - cell_oz))
                    .collect();
                let dist = (centroid.x - world_x)
                    .abs()
                    .max((centroid.y - world_z).abs());
                (ring, dist)
            };

            // Stage 1: the small detail patch — lands within seconds.
            let detail = fetch_patch(d_south, d_west, d_north, d_east).map(|buildings| {
                let mut bld = MeshData::default();
                let mut wins: [MeshData; 4] = std::array::from_fn(|_| MeshData::default());
                for b in &buildings {
                    let (ring, dist) = local_ring(b);
                    if dist > detail_half {
                        continue;
                    }
                    push_building(
                        &mut bld,
                        &mut wins,
                        b.id as u64,
                        &ring,
                        b.height_m,
                        b.height_is_eaves,
                        b.wall_color,
                        b.roof_color,
                        b.roof_shape,
                        b.roof_height,
                        b.roof_across,
                    );
                }
                BuiltPatch {
                    buildings,
                    patch_cell,
                    detail: bld.into_mesh(),
                    windows: wins.map(MeshData::into_mesh),
                    far: None,
                }
            });
            if let Ok(mut g) = slot.lock() {
                g.push((PatchStage::Detail, detail));
            }

            // Stage 2: the far LOD mesh. It covers the WHOLE box including the
            // detail area — LOD volumes nest inside their detailed twins (see
            // push_building_lod), so there's no hole to flicker through while a
            // later patch refresh replaces the detail ring around a new centre.
            let far = fetch_patch(f_south, f_west, f_north, f_east).map(|buildings| {
                let mut farm = MeshData::default();
                for b in &buildings {
                    let (ring, dist) = local_ring(b);
                    // Sheds and garages are invisible from afar; skipping them
                    // keeps the big ring's vertex budget in check.
                    if dist > 1_200.0 && polygon_area(&ring) < 35.0 {
                        continue;
                    }
                    // Approximate total height when only eaves height is known.
                    let h = if b.height_is_eaves {
                        b.height_m + 2.0
                    } else {
                        b.height_m
                    };
                    push_building_lod(&mut farm, &ring, h, b.wall_color, b.roof_color);
                }
                BuiltPatch {
                    buildings,
                    patch_cell,
                    detail: None,
                    windows: std::array::from_fn(|_| None),
                    far: farm.into_mesh(),
                }
            });
            if let Ok(mut g) = slot.lock() {
                g.push((PatchStage::Far, far));
            }
        });
    }

    // Apply completed stages in order, each replacing only its own entities, so
    // near buildings appear as soon as stage 1 lands.
    let ready: Vec<(PatchStage, Result<BuiltPatch, String>)> = osm
        .fetch
        .lock()
        .map(|mut g| g.drain(..).collect())
        .unwrap_or_default();
    if ready.is_empty() {
        return;
    }
    let Ok(root) = bigspace.single() else {
        return;
    };

    for (stage, result) in ready {
        // The far stage is always posted last; its arrival ends the fetch cycle.
        if matches!(stage, PatchStage::Far) {
            osm.in_flight = false;
        }
        let patch = match result {
            Ok(p) => p,
            Err(e) => {
                tracing::error!(error = %e, "OSM buildings fetch failed");
                continue;
            }
        };
        let patch_cell = patch.patch_cell;
        let count = patch.buildings.len();
        // Keep the widest footprint set for hit-testing / prim conversion.
        osm.buildings = patch.buildings;

        match stage {
            PatchStage::Detail => {
                for entity in osm.detail_entities.drain(..) {
                    commands.entity(entity).despawn();
                }
                spawn_detail_patch(
                    &mut commands,
                    &mut meshes,
                    &mut materials,
                    &mut osm,
                    root,
                    patch_cell,
                    patch.detail,
                    patch.windows,
                );
            }
            PatchStage::Far => {
                for entity in osm.far_entities.drain(..) {
                    commands.entity(entity).despawn();
                }
                // Far LOD ring: one merged mesh of plain extrusions.
                if let Some(mesh) = patch.far {
                    let material = materials.add(StandardMaterial {
                        base_color: Color::WHITE,
                        perceptual_roughness: 0.95,
                        double_sided: true,
                        cull_mode: None,
                        ..default()
                    });
                    let entity = commands
                        .spawn((
                            Mesh3d(meshes.add(mesh)),
                            MeshMaterial3d(material),
                            Transform::from_translation(Vec3::new(0.0, 0.05, 0.0)),
                            patch_cell,
                            ChildOf(root),
                            OsmBuildingMesh,
                        ))
                        .id();
                    osm.far_entities.push(entity);
                }
            }
        }
        tracing::info!(count, "buildings updated");
    }
}

/// Spawn the detail-ring meshes (buildings + per-bucket windows) for a stage-1
/// patch, tracking the entities for despawn on the next refresh.
#[allow(clippy::too_many_arguments)]
fn spawn_detail_patch(
    commands: &mut Commands,
    meshes: &mut Assets<Mesh>,
    materials: &mut Assets<StandardMaterial>,
    osm: &mut OsmBuildings,
    root: Entity,
    patch_cell: GridCell,
    detail: Option<Mesh>,
    windows: [Option<Mesh>; 4],
) {
    // Building mesh: one white material, per-vertex wall/roof colours.
    if let Some(mesh) = detail {
        let material = materials.add(StandardMaterial {
            base_color: Color::WHITE,
            perceptual_roughness: 0.95,
            double_sided: true,
            cull_mode: None,
            ..default()
        });
        let entity = commands
            .spawn((
                Mesh3d(meshes.add(mesh)),
                MeshMaterial3d(material),
                Transform::from_translation(Vec3::new(0.0, 0.05, 0.0)),
                patch_cell,
                ChildOf(root),
                OsmBuildingMesh,
            ))
            .id();
        osm.detail_entities.push(entity);
    }

    // Window meshes: dark glass by day; per-bucket emissive faded in at night by
    // `update_window_glow`, so windows vary (off / dim / bright) across buildings.
    for (i, w) in windows.into_iter().enumerate() {
        let Some(mesh) = w else {
            continue;
        };
        let material = materials.add(StandardMaterial {
            base_color: Color::srgb(0.07, 0.09, 0.13),
            emissive: LinearRgba::BLACK,
            perceptual_roughness: 0.25,
            double_sided: true,
            cull_mode: None,
            ..default()
        });
        let entity = commands
            .spawn((
                Mesh3d(meshes.add(mesh)),
                MeshMaterial3d(material),
                Transform::from_translation(Vec3::new(0.0, 0.05, 0.0)),
                patch_cell,
                ChildOf(root),
                WindowGlow(WINDOW_GLOW_MULT[i]),
            ))
            .id();
        osm.detail_entities.push(entity);
    }
}

/// Web Mercator (lat,lng) → world metres relative to the anchor tile centre.
/// `tile_m` is the real ground edge of a tile; East is `+X`, South is `+Z`.
fn lat_lng_to_local(lat: f64, lng: f64, tile_x: i64, tile_y: i64, z: u32, tile_m: f32) -> Vec2 {
    let n = 2.0_f64.powi(z as i32);
    let x = (lng + 180.0) / 360.0 * n;
    let lat_rad = lat.to_radians();
    let y = (1.0 - (lat_rad.tan() + 1.0 / lat_rad.cos()).ln() / std::f64::consts::PI) / 2.0 * n;

    let fx = (x - tile_x as f64) as f32;
    let fy = (y - tile_y as f64) as f32;
    Vec2::new((fx - 0.5) * tile_m, (fy - 0.5) * tile_m)
}

/// An accumulating triangle-mesh buffer.
#[derive(Default)]
struct MeshData {
    positions: Vec<[f32; 3]>,
    normals: Vec<[f32; 3]>,
    colors: Vec<[f32; 4]>,
    indices: Vec<u32>,
}

impl MeshData {
    /// Push a triangle with a flat computed normal.
    fn tri(&mut self, a: Vec3, b: Vec3, c: Vec3, color: [f32; 4]) {
        let n = (b - a).cross(c - a).normalize_or_zero();
        let base = self.positions.len() as u32;
        for v in [a, b, c] {
            self.positions.push([v.x, v.y, v.z]);
            self.normals.push([n.x, n.y, n.z]);
            self.colors.push(color);
        }
        self.indices.extend_from_slice(&[base, base + 1, base + 2]);
    }

    /// Push a quad (a→b→c→d) as two triangles.
    fn quad(&mut self, a: Vec3, b: Vec3, c: Vec3, d: Vec3, color: [f32; 4]) {
        self.tri(a, b, c, color);
        self.tri(a, c, d, color);
    }

    /// Finalise into a `Mesh` (or `None` if empty).
    fn into_mesh(self) -> Option<Mesh> {
        if self.indices.is_empty() {
            return None;
        }
        let mut mesh = Mesh::new(
            PrimitiveTopology::TriangleList,
            RenderAssetUsages::RENDER_WORLD | RenderAssetUsages::MAIN_WORLD,
        );
        mesh.insert_attribute(Mesh::ATTRIBUTE_POSITION, self.positions);
        mesh.insert_attribute(Mesh::ATTRIBUTE_NORMAL, self.normals);
        mesh.insert_attribute(Mesh::ATTRIBUTE_COLOR, self.colors);
        mesh.insert_indices(Indices::U32(self.indices));
        Some(mesh)
    }
}

/// Oriented bounding box of a footprint in a (u, v) basis (min-area over edges).
struct Obb {
    u: Vec2,
    v: Vec2,
    umin: f32,
    umax: f32,
    vmin: f32,
    vmax: f32,
    area: f32,
}

fn oriented_bbox(ring: &[Vec2]) -> Obb {
    let n = ring.len();
    let mut best: Option<Obb> = None;
    for i in 0..n {
        let e = (ring[(i + 1) % n] - ring[i]).normalize_or_zero();
        if e == Vec2::ZERO {
            continue;
        }
        let (u, v) = (e, Vec2::new(-e.y, e.x));
        let (mut umin, mut umax, mut vmin, mut vmax) = (f32::MAX, f32::MIN, f32::MAX, f32::MIN);
        for p in ring {
            let (du, dv) = (p.dot(u), p.dot(v));
            umin = umin.min(du);
            umax = umax.max(du);
            vmin = vmin.min(dv);
            vmax = vmax.max(dv);
        }
        let area = (umax - umin) * (vmax - vmin);
        if best.as_ref().is_none_or(|b| area < b.area) {
            best = Some(Obb { u, v, umin, umax, vmin, vmax, area });
        }
    }
    best.unwrap_or(Obb {
        u: Vec2::X,
        v: Vec2::Y,
        umin: 0.0,
        umax: 0.0,
        vmin: 0.0,
        vmax: 0.0,
        area: 0.0,
    })
}

fn polygon_area(ring: &[Vec2]) -> f32 {
    let n = ring.len();
    let mut a = 0.0;
    for i in 0..n {
        let p = ring[i];
        let q = ring[(i + 1) % n];
        a += p.x * q.y - q.x * p.y;
    }
    a.abs() * 0.5
}

/// Greedy outline simplification for far LOD: drop vertices that deviate less
/// than `tol` metres from the line between their kept neighbours. Far
/// silhouettes don't need sub-metre detail, and this roughly halves the ring's
/// vertex count on typical OSM footprints.
fn simplify_ring(ring: &[Vec2], tol: f32) -> Vec<Vec2> {
    let n = ring.len();
    if n <= 4 {
        return ring.to_vec();
    }
    let mut out: Vec<Vec2> = Vec::with_capacity(n);
    for i in 0..n {
        let prev = *out.last().unwrap_or(&ring[n - 1]);
        let cur = ring[i];
        let next = ring[(i + 1) % n];
        let ab = next - prev;
        let len = ab.length();
        let dist = if len < 1e-3 {
            tol + 1.0
        } else {
            (cur - prev).perp_dot(ab / len).abs()
        };
        if dist > tol {
            out.push(cur);
        }
    }
    if out.len() >= 3 { out } else { ring.to_vec() }
}

/// Far-LOD building: a simplified plain extrusion with a flat cap — no windows,
/// no roof shapes — merged into a single mesh for the whole far ring. The
/// footprint is inset toward the centroid and the cap kept slightly low, so the
/// LOD volume nests strictly *inside* the detailed version of the same building:
/// the far mesh covers the detail area too (as a fallback while a patch refresh
/// is in flight) without ever z-fighting it.
fn push_building_lod(
    bld: &mut MeshData,
    ring_in: &[Vec2],
    height: f32,
    wall_color: [f32; 3],
    roof_color: [f32; 3],
) {
    let mut ring = ring_in.to_vec();
    if ring.len() >= 2 && ring.first() == ring.last() {
        ring.pop();
    }
    let ring = simplify_ring(&ring, 0.5);
    let n = ring.len();
    if n < 3 || height <= 0.0 {
        return;
    }
    let wall = [wall_color[0], wall_color[1], wall_color[2], 1.0];
    let roof = [roof_color[0], roof_color[1], roof_color[2], 1.0];

    let centroid = ring.iter().copied().sum::<Vec2>() / n as f32;
    let ring: Vec<Vec2> = ring
        .iter()
        .map(|&p| {
            let to_c = centroid - p;
            let d = to_c.length();
            if d > 1.5 { p + to_c / d * 0.35 } else { p }
        })
        .collect();
    let height = (height - 0.25).max(1.5);
    for i in 0..n {
        let a = ring[i];
        let b = ring[(i + 1) % n];
        let mut perp = Vec2::new((b - a).y, -(b - a).x).normalize_or_zero();
        if perp.dot((a + b) * 0.5 - centroid) < 0.0 {
            perp = -perp;
        }
        let nrm = [perp.x, 0.0, perp.y];
        let base = bld.positions.len() as u32;
        for v in [
            Vec3::new(a.x, 0.0, a.y),
            Vec3::new(b.x, 0.0, b.y),
            Vec3::new(b.x, height, b.y),
            Vec3::new(a.x, height, a.y),
        ] {
            bld.positions.push([v.x, v.y, v.z]);
            bld.normals.push(nrm);
            bld.colors.push(wall);
        }
        bld.indices
            .extend_from_slice(&[base, base + 1, base + 2, base, base + 2, base + 3]);
    }
    flat_cap(bld, &ring, height, roof);
}

/// Append one building (walls up to the eaves + a shaped roof) into the buffers,
/// with distinct wall/roof vertex colours. Degenerate footprints are skipped.
#[allow(clippy::too_many_arguments)]
fn push_building(
    bld: &mut MeshData,
    wins: &mut [MeshData; 4],
    seed: u64,
    ring_in: &[Vec2],
    height: f32,
    height_is_eaves: bool,
    wall_color: [f32; 3],
    roof_color: [f32; 3],
    roof_shape: RoofShape,
    roof_height: Option<f32>,
    roof_across: bool,
) {
    let mut ring = ring_in.to_vec();
    if ring.len() >= 2 && ring.first() == ring.last() {
        ring.pop();
    }
    let n = ring.len();
    if n < 3 || height <= 0.0 {
        return;
    }
    let wall = [wall_color[0], wall_color[1], wall_color[2], 1.0];
    let roof = [roof_color[0], roof_color[1], roof_color[2], 1.0];

    let obb = oriented_bbox(&ring);
    let short = (obb.umax - obb.umin).min(obb.vmax - obb.vmin);

    // Roof pitch height: explicit tag, else ~35° over the short span.
    let pitch = if roof_shape == RoofShape::Flat {
        0.0
    } else {
        roof_height.unwrap_or((short * 0.35).clamp(1.5, 6.0))
    };
    // Eaves (wall top) and ridge (roof apex). For an eaves height the roof adds on
    // top; for a total height it's carved out of the top.
    let (wall_h, ridge_y) = if height_is_eaves {
        (height, height + pitch)
    } else {
        let rh = pitch.min(height - 2.0).max(0.0);
        (height - rh, height)
    };
    let roof_h = ridge_y - wall_h;
    let shape = if roof_h <= 0.1 { RoofShape::Flat } else { roof_shape };

    // --- Walls: footprint 0 → eaves, oriented outward via the centroid ---
    let centroid = ring.iter().copied().sum::<Vec2>() / n as f32;
    for i in 0..n {
        let a = ring[i];
        let b = ring[(i + 1) % n];
        let mut perp = Vec2::new((b - a).y, -(b - a).x).normalize_or_zero();
        if perp.dot((a + b) * 0.5 - centroid) < 0.0 {
            perp = -perp;
        }
        let nrm = [perp.x, 0.0, perp.y];
        let base = bld.positions.len() as u32;
        for v in [
            Vec3::new(a.x, 0.0, a.y),
            Vec3::new(b.x, 0.0, b.y),
            Vec3::new(b.x, wall_h, b.y),
            Vec3::new(a.x, wall_h, a.y),
        ] {
            bld.positions.push([v.x, v.y, v.z]);
            bld.normals.push(nrm);
            bld.colors.push(wall);
        }
        bld.indices
            .extend_from_slice(&[base, base + 1, base + 2, base, base + 2, base + 3]);

        // Windows only on the walls of tall enough buildings (skips houses/sheds
        // and all roof/gable surfaces).
        if wall_h >= WINDOW_MIN_HEIGHT {
            push_wall_windows(wins, seed, a, b, perp, wall_h);
        }
    }

    // --- Roof ---
    match shape {
        RoofShape::Flat => flat_cap(bld, &ring, wall_h, roof),
        RoofShape::Pyramidal => {
            let apex = Vec3::new(centroid.x, ridge_y, centroid.y);
            for i in 0..n {
                let a = ring[i];
                let b = ring[(i + 1) % n];
                bld.tri(
                    Vec3::new(a.x, wall_h, a.y),
                    Vec3::new(b.x, wall_h, b.y),
                    apex,
                    roof,
                );
            }
        }
        RoofShape::Skillion => skillion_roof(bld, &ring, &obb, wall_h, roof_h, roof_across, roof),
        RoofShape::Gabled | RoofShape::Hipped => {
            // Ridged roofs assume a ~rectangular footprint; fall back to flat otherwise.
            if obb.area <= 0.0 || polygon_area(&ring) / obb.area < 0.82 {
                flat_cap(bld, &ring, wall_h, roof);
            } else {
                ridged_roof(
                    bld,
                    &obb,
                    wall_h,
                    ridge_y,
                    shape == RoofShape::Hipped,
                    roof_across,
                    roof,
                    wall,
                );
            }
        }
    }
}

/// Emit a grid of window quads on one wall face (proud of the wall), routing each
/// window into a brightness bucket so the lit pattern varies per building.
fn push_wall_windows(wins: &mut [MeshData; 4], seed: u64, a: Vec2, b: Vec2, out: Vec2, wall_h: f32) {
    let along = b - a;
    let len = along.length();
    if len < WINDOW_PITCH {
        return;
    }
    let dir = along / len;
    let proud = out * 0.06;
    let ncols = (len / WINDOW_PITCH).floor() as i32;
    if ncols < 1 {
        return;
    }
    let start = (len - (ncols as f32 - 1.0) * WINDOW_PITCH) * 0.5;
    let color = [1.0, 1.0, 1.0, 1.0];
    let mut row = 0i32;
    let mut wy = WINDOW_SILL;
    while wy + WINDOW_H <= wall_h - 0.4 {
        for c in 0..ncols {
            let cx = start + c as f32 * WINDOW_PITCH;
            let center = a + dir * cx;
            let half = dir * (WINDOW_W * 0.5);
            let (bl, br) = (center - half, center + half);
            let bucket = window_bucket(seed, row, c);
            wins[bucket].quad(
                Vec3::new(bl.x + proud.x, wy, bl.y + proud.y),
                Vec3::new(br.x + proud.x, wy, br.y + proud.y),
                Vec3::new(br.x + proud.x, wy + WINDOW_H, br.y + proud.y),
                Vec3::new(bl.x + proud.x, wy + WINDOW_H, bl.y + proud.y),
                color,
            );
        }
        wy += WINDOW_FLOOR;
        row += 1;
    }
}

/// Deterministically bucket a window (per building + floor + column) into a
/// brightness level, weighted toward dark/dim so not every window blazes.
fn window_bucket(seed: u64, row: i32, col: i32) -> usize {
    let h = unit_rand(
        seed ^ ((row as u64) << 20)
            ^ (col as u64).wrapping_mul(0x9E37_79B9_7F4A_7C15),
    );
    if h < 0.35 {
        0
    } else if h < 0.60 {
        1
    } else if h < 0.85 {
        2
    } else {
        3
    }
}

/// Triangulated flat cap at `y`.
fn flat_cap(buf: &mut MeshData, ring: &[Vec2], y: f32, color: [f32; 4]) {
    let flat: Vec<f64> = ring.iter().flat_map(|p| [p.x as f64, p.y as f64]).collect();
    let Ok(cap) = earcutr::earcut(&flat, &[], 2) else {
        return;
    };
    for t in cap.chunks_exact(3) {
        let p = |i: usize| Vec3::new(ring[i].x, y, ring[i].y);
        buf.tri(p(t[0]), p(t[1]), p(t[2]), color);
    }
}

/// Single-plane mono-pitch roof: cap sloped linearly along the short axis.
fn skillion_roof(
    buf: &mut MeshData,
    ring: &[Vec2],
    obb: &Obb,
    wall_h: f32,
    roof_h: f32,
    across: bool,
    color: [f32; 4],
) {
    // Slope along the short axis by default (or long if `across`).
    let (axis, amin, amax) = if across {
        (obb.u, obb.umin, obb.umax)
    } else {
        (obb.v, obb.vmin, obb.vmax)
    };
    let span = (amax - amin).max(0.001);
    let lift = |p: Vec2| -> Vec3 {
        let t = (p.dot(axis) - amin) / span;
        Vec3::new(p.x, wall_h + roof_h * t, p.y)
    };
    let flat: Vec<f64> = ring.iter().flat_map(|p| [p.x as f64, p.y as f64]).collect();
    let Ok(cap) = earcutr::earcut(&flat, &[], 2) else {
        return;
    };
    for t in cap.chunks_exact(3) {
        buf.tri(lift(ring[t[0]]), lift(ring[t[1]]), lift(ring[t[2]]), color);
    }
}

/// Gabled (or hipped) roof over the oriented bounding box.
#[allow(clippy::too_many_arguments)]
fn ridged_roof(
    buf: &mut MeshData,
    obb: &Obb,
    wall_h: f32,
    ridge_y: f32,
    hipped: bool,
    across: bool,
    roof: [f32; 4],
    wall: [f32; 4],
) {
    let u_len = obb.umax - obb.umin;
    let v_len = obb.vmax - obb.vmin;
    // Ridge runs along the long axis by default; `across` flips it.
    let ridge_along_u = (u_len >= v_len) ^ across;
    // a = ridge axis, b = slope axis.
    let (a_ax, a0, a1, b_ax, b0, b1) = if ridge_along_u {
        (obb.u, obb.umin, obb.umax, obb.v, obb.vmin, obb.vmax)
    } else {
        (obb.v, obb.vmin, obb.vmax, obb.u, obb.umin, obb.umax)
    };
    let bc = (b0 + b1) * 0.5;
    let pt = |av: f32, bv: f32, y: f32| -> Vec3 {
        let xz = a_ax * av + b_ax * bv;
        Vec3::new(xz.x, y, xz.y)
    };

    // Eave corners at the walls.
    let e00 = pt(a0, b0, wall_h);
    let e10 = pt(a1, b0, wall_h);
    let e11 = pt(a1, b1, wall_h);
    let e01 = pt(a0, b1, wall_h);

    // Ridge: full length for gabled; inset by the half-span for hipped.
    let inset = if hipped {
        ((b1 - b0) * 0.5).min((a1 - a0) * 0.5)
    } else {
        0.0
    };
    let r0 = pt(a0 + inset, bc, ridge_y);
    let r1 = pt(a1 - inset, bc, ridge_y);

    // Two main slopes (roof-coloured).
    buf.quad(e00, e10, r1, r0, roof);
    buf.quad(e11, e01, r0, r1, roof);

    if hipped {
        // Hip ends are sloped roof triangles.
        buf.tri(e00, r0, e01, roof);
        buf.tri(e10, e11, r1, roof);
    } else {
        // Gable ends are vertical triangles (wall-coloured).
        buf.tri(e00, r0, e01, wall);
        buf.tri(e10, e11, r1, wall);
    }
}

// --- Overpass fetch + parse (runs on the background thread) ---

/// On-disk cache path for a fixed CACHE_ZOOM cell (independent of patch size, so
/// changing the patch radius never invalidates the cache).
fn cell_cache_path(cx: i64, cy: i64, zoom: u32) -> PathBuf {
    PathBuf::from("cache")
        .join("buildings")
        .join(zoom.to_string())
        .join(cx.to_string())
        .join(format!("{}.json", cy))
}

/// Assemble a patch's buildings from the fixed CACHE_ZOOM cells covering its bbox:
/// each cell is served from disk cache or fetched once, then deduped by OSM way id
/// (a building straddling a cell edge is returned by both cells).
fn fetch_patch(south: f64, west: f64, north: f64, east: f64) -> Result<Vec<Building>, String> {
    // NW corner → min tile (x,y); SE corner → max tile (y grows southward).
    let (cx0, cy0) = lat_lng_to_tile(north, west, CACHE_ZOOM);
    let (cx1, cy1) = lat_lng_to_tile(south, east, CACHE_ZOOM);

    let mut out = Vec::new();
    let mut seen = HashSet::new();
    for cy in cy0..=cy1 {
        for cx in cx0..=cx1 {
            match fetch_cell(cx, cy, CACHE_ZOOM) {
                Ok(cell) => {
                    for b in cell {
                        if seen.insert(b.id) {
                            out.push(b);
                        }
                    }
                }
                Err(e) => tracing::warn!(cx, cy, error = %e, "building cell fetch failed"),
            }
        }
    }
    // The cache cells are coarser than the patch; keep only buildings whose first
    // vertex falls in the requested bbox, so we spawn the patch, not whole cells.
    out.retain(|b| {
        let (lat, lng) = b.ring[0];
        lat >= south && lat <= north && lng >= west && lng <= east
    });
    Ok(out)
}

/// One fixed cell's buildings, from disk cache or a single Overpass query (cached).
/// Cache hits skip the network (and the throttle) entirely.
fn fetch_cell(cx: i64, cy: i64, zoom: u32) -> Result<Vec<Building>, String> {
    let path = cell_cache_path(cx, cy, zoom);
    if let Ok(cached) = std::fs::read_to_string(&path) {
        return parse_buildings(&cached);
    }

    let (north, west) = tile_to_lat_lng(cx, cy, zoom);
    let (south, east) = tile_to_lat_lng(cx + 1, cy + 1, zoom);
    let query = format!(
        "[out:json][timeout:25];way[\"building\"]({south},{west},{north},{east});out geom;"
    );
    let fetched = overpass_query(&query)?;
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    let _ = std::fs::write(&path, &fetched);
    // Space out live queries so a multi-cell patch doesn't burst-trip the limit.
    std::thread::sleep(OVERPASS_THROTTLE);
    parse_buildings(&fetched)
}

/// POST an Overpass query, retrying a couple of times with backoff on failure
/// (429/504 are common under load).
fn overpass_query(query: &str) -> Result<String, String> {
    let mut last_err = String::new();
    for attempt in 0..3 {
        if attempt > 0 {
            std::thread::sleep(Duration::from_secs(3));
        }
        match ureq::post("https://overpass-api.de/api/interpreter").send_form(&[("data", query)]) {
            Ok(resp) => return resp.into_string().map_err(|e| e.to_string()),
            Err(e) => last_err = e.to_string(),
        }
    }
    Err(last_err)
}

/// Parse an Overpass `out geom` response into buildings.
fn parse_buildings(text: &str) -> Result<Vec<Building>, String> {
    let json: serde_json::Value = serde_json::from_str(text).map_err(|e| e.to_string())?;
    let elements = json
        .get("elements")
        .and_then(|e| e.as_array())
        .ok_or_else(|| "no elements in Overpass response".to_string())?;

    let mut out = Vec::new();
    for el in elements {
        if el.get("type").and_then(|t| t.as_str()) != Some("way") {
            continue;
        }
        let Some(geom) = el.get("geometry").and_then(|g| g.as_array()) else {
            continue;
        };
        let ring: Vec<(f64, f64)> = geom
            .iter()
            .filter_map(|pt| Some((pt.get("lat")?.as_f64()?, pt.get("lon")?.as_f64()?)))
            .collect();
        if ring.len() < 3 {
            continue;
        }
        let id = el.get("id").and_then(|v| v.as_i64()).unwrap_or(0);
        let tags = el.get("tags");
        let seed = color_seed(ring[0]);
        let (height_m, height_is_eaves) = building_height(tags, seed);
        let (wall_color, roof_color) = derive_colors(tags, ring[0]);
        let mut roof_shape = parse_roof_shape(tag_str(tags, "roof:shape"));
        // roof:shape is sparsely tagged; small residential buildings are almost
        // always gabled, so default them to that (odd shapes flatten downstream).
        if roof_shape == RoofShape::Flat && default_pitched(tags, &ring) {
            roof_shape = RoofShape::Gabled;
        }
        let roof_height = tags
            .and_then(|t| t.get("roof:height"))
            .and_then(num_from_value);
        let roof_across = tag_str(tags, "roof:orientation") == Some("across");
        out.push(Building {
            id,
            ring,
            height_m,
            height_is_eaves,
            wall_color,
            roof_color,
            roof_shape,
            roof_height,
            roof_across,
        });
    }
    Ok(out)
}

/// Derive wall + roof colours from OSM tags, with heuristic fallbacks plus a small
/// deterministic per-building jitter so same-type buildings aren't identical.
/// Wall: `building:colour` → `building:material` → `building=<type>` → palette.
/// Roof: `roof:colour` → `roof:material` → type-based tiled/flat default.
fn derive_colors(tags: Option<&serde_json::Value>, ring0: (f64, f64)) -> ([f32; 3], [f32; 3]) {
    let seed = color_seed(ring0);
    let btype = tag_str(tags, "building").unwrap_or("yes");

    let wall_base = tag_color(tags, &["building:colour", "building:color", "colour", "color"])
        .or_else(|| tag_str(tags, "building:material").and_then(material_color))
        .or_else(|| type_color(btype))
        .unwrap_or_else(|| palette_color(ring0));
    let wall = jitter(wall_base, seed, 0.12);

    let roof_base = tag_color(tags, &["roof:colour", "roof:color"])
        .or_else(|| tag_str(tags, "roof:material").and_then(roof_material_color))
        .unwrap_or_else(|| default_roof(btype, seed));
    let roof = jitter(roof_base, seed.rotate_left(17), 0.12);
    (wall, roof)
}

/// Default roof tone for a building type without `roof:*` tags: flat grey for
/// commercial/industrial, a varied tile colour otherwise (mostly residential).
fn default_roof(btype: &str, seed: u64) -> [f32; 3] {
    match btype.to_ascii_lowercase().as_str() {
        "commercial" | "retail" | "office" | "supermarket" | "industrial" | "warehouse"
        | "manufacture" | "factory" | "garage" | "garages" | "carport" | "roof" => {
            [0.52, 0.52, 0.54]
        }
        _ => ROOF_COLORS[(seed as usize) % ROOF_COLORS.len()],
    }
}

/// Deterministic seed from a footprint location.
fn color_seed(ring0: (f64, f64)) -> u64 {
    let (lat, lng) = ring0;
    (((lat * 1.0e5) as i64).wrapping_mul(73_856_093)
        ^ ((lng * 1.0e5) as i64).wrapping_mul(19_349_663)) as u64
}

/// Deterministic pseudo-random value in `[0, 1)` from a seed (SplitMix64).
fn unit_rand(seed: u64) -> f32 {
    let mut x = seed.wrapping_add(0x9E37_79B9_7F4A_7C15);
    x = (x ^ (x >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
    x = (x ^ (x >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
    x ^= x >> 31;
    (x % 10_000) as f32 / 10_000.0
}

/// Nudge a colour's brightness by ±`amount` deterministically (weathering/variety).
fn jitter(c: [f32; 3], seed: u64, amount: f32) -> [f32; 3] {
    let f = 1.0 + (unit_rand(seed) - 0.5) * 2.0 * amount;
    [
        (c[0] * f).clamp(0.0, 1.0),
        (c[1] * f).clamp(0.0, 1.0),
        (c[2] * f).clamp(0.0, 1.0),
    ]
}

fn tag_str<'a>(tags: Option<&'a serde_json::Value>, key: &str) -> Option<&'a str> {
    tags?.get(key)?.as_str()
}

fn tag_color(tags: Option<&serde_json::Value>, keys: &[&str]) -> Option<[f32; 3]> {
    keys.iter().find_map(|k| tag_str(tags, k).and_then(parse_color))
}

/// Parse an OSM colour value: `#RRGGBB` hex or a common named colour.
fn parse_color(s: &str) -> Option<[f32; 3]> {
    let s = s.trim();
    if let Some(hex) = s.strip_prefix('#') {
        if hex.len() == 6 {
            let r = u8::from_str_radix(&hex[0..2], 16).ok()?;
            let g = u8::from_str_radix(&hex[2..4], 16).ok()?;
            let b = u8::from_str_radix(&hex[4..6], 16).ok()?;
            return Some([r as f32 / 255.0, g as f32 / 255.0, b as f32 / 255.0]);
        }
        return None;
    }
    Some(match s.to_ascii_lowercase().as_str() {
        "white" => [0.92, 0.92, 0.90],
        "black" => [0.18, 0.18, 0.18],
        "grey" | "gray" | "silver" => [0.60, 0.60, 0.60],
        "red" | "maroon" => [0.70, 0.25, 0.22],
        "brown" => [0.55, 0.42, 0.32],
        "beige" | "cream" | "ivory" => [0.85, 0.80, 0.70],
        "yellow" | "gold" => [0.85, 0.78, 0.45],
        "orange" => [0.80, 0.55, 0.35],
        "green" => [0.45, 0.55, 0.40],
        "blue" => [0.45, 0.55, 0.68],
        "sandstone" | "tan" | "sand" => [0.80, 0.72, 0.60],
        "pink" => [0.85, 0.68, 0.68],
        _ => return None,
    })
}

/// Wall colour by `building:material`.
fn material_color(s: &str) -> Option<[f32; 3]> {
    Some(match s.to_ascii_lowercase().as_str() {
        "brick" | "red_brick" => [0.70, 0.42, 0.35],
        "concrete" | "cement" | "reinforced_concrete" => [0.72, 0.72, 0.70],
        "glass" => [0.55, 0.68, 0.75],
        "wood" | "timber" | "timber_framing" => [0.62, 0.48, 0.35],
        "stone" | "limestone" | "sandstone" | "granite" => [0.78, 0.74, 0.66],
        "plaster" | "render" | "stucco" | "plastered" => [0.86, 0.82, 0.76],
        "metal" | "steel" => [0.66, 0.68, 0.70],
        _ => return None,
    })
}

/// Wall colour by `building=<type>`.
fn type_color(s: &str) -> Option<[f32; 3]> {
    Some(match s.to_ascii_lowercase().as_str() {
        "house" | "detached" | "residential" | "apartments" | "terrace" | "semidetached_house" => {
            [0.80, 0.74, 0.66]
        }
        "commercial" | "retail" | "office" | "supermarket" => [0.73, 0.73, 0.73],
        "industrial" | "warehouse" | "manufacture" | "factory" => [0.66, 0.66, 0.62],
        "church" | "cathedral" | "chapel" | "temple" | "mosque" | "synagogue" => [0.80, 0.76, 0.66],
        "garage" | "garages" | "shed" | "hut" | "roof" | "carport" => [0.70, 0.68, 0.64],
        _ => return None,
    })
}

/// Roof colour by `roof:material`.
fn roof_material_color(s: &str) -> Option<[f32; 3]> {
    Some(match s.to_ascii_lowercase().as_str() {
        "tile" | "tiles" | "roof_tiles" | "clay" | "clay_tiles" => [0.66, 0.34, 0.27],
        "metal" | "tin" | "zinc" | "copper" | "steel" => [0.55, 0.57, 0.60],
        "concrete" => [0.62, 0.62, 0.60],
        "slate" => [0.36, 0.38, 0.42],
        "tar_paper" | "asphalt" | "bitumen" | "gravel" => [0.30, 0.30, 0.32],
        "glass" => [0.55, 0.68, 0.75],
        "eternit" | "asbestos" => [0.60, 0.60, 0.58],
        "grass" | "green" => [0.45, 0.55, 0.38],
        _ => return None,
    })
}

/// Deterministic fallback wall tone from the footprint location, so untagged
/// buildings still read as distinct rather than a uniform mass.
fn palette_color(ring0: (f64, f64)) -> [f32; 3] {
    BUILDING_COLORS[(color_seed(ring0) as usize) % BUILDING_COLORS.len()]
}

/// Whether an untagged building should default to a pitched (gabled) roof: small
/// residential footprints, which are overwhelmingly pitched in European towns.
fn default_pitched(tags: Option<&serde_json::Value>, ring: &[(f64, f64)]) -> bool {
    let btype = tag_str(tags, "building").unwrap_or("yes");
    let residential = matches!(
        btype.to_ascii_lowercase().as_str(),
        "house"
            | "detached"
            | "semidetached_house"
            | "terrace"
            | "residential"
            | "yes"
            | "bungalow"
            | "cabin"
            | "farm"
    );
    if !residential {
        return false;
    }
    // Approximate footprint extent in metres (equirectangular near the anchor).
    let lat0 = ring[0].0;
    let mlng = 111_320.0 * lat0.to_radians().cos();
    let (mut minx, mut maxx, mut miny, mut maxy) = (f64::MAX, f64::MIN, f64::MAX, f64::MIN);
    for &(lat, lng) in ring {
        let (x, y) = (lng * mlng, lat * 111_320.0);
        minx = minx.min(x);
        maxx = maxx.max(x);
        miny = miny.min(y);
        maxy = maxy.max(y);
    }
    let (w, h) = (maxx - minx, maxy - miny);
    w.min(h) < 14.0 && (w * h) < 400.0
}

/// Map an OSM `roof:shape` value to a form we can build (unknown → flat).
fn parse_roof_shape(s: Option<&str>) -> RoofShape {
    match s {
        Some("gabled") => RoofShape::Gabled,
        Some("hipped") | Some("half-hipped") => RoofShape::Hipped,
        Some("pyramidal") | Some("pyramid") => RoofShape::Pyramidal,
        Some("skillion") | Some("lean_to") | Some("mono_pitch") => RoofShape::Skillion,
        _ => RoofShape::Flat,
    }
}

/// Building height and whether it's an eaves (wall-top) height. `height` → total;
/// `building:levels` → eaves; otherwise a type-based estimate (with jitter) → eaves.
fn building_height(tags: Option<&serde_json::Value>, seed: u64) -> (f32, bool) {
    if let Some(h) = tags.and_then(|t| t.get("height")).and_then(num_from_value) {
        return (h.max(2.0), false);
    }
    if let Some(levels) = tags
        .and_then(|t| t.get("building:levels"))
        .and_then(num_from_value)
    {
        return ((levels * STOREY_HEIGHT).max(2.5), true);
    }
    let btype = tag_str(tags, "building").unwrap_or("yes");
    (estimate_height(btype, seed), true)
}

/// Estimate an untagged building's eaves height from its type, with a deterministic
/// per-building jitter so neighbours vary (no flat, uniform blocks).
fn estimate_height(btype: &str, seed: u64) -> f32 {
    let (base, spread) = match btype.to_ascii_lowercase().as_str() {
        "house" | "detached" | "bungalow" | "cabin" => (6.0, 1.5),
        "semidetached_house" | "terrace" => (7.5, 1.5),
        "residential" | "farm" => (8.0, 2.0),
        "apartments" | "dormitory" => (12.0, 4.0),
        "commercial" | "retail" | "office" | "supermarket" => (9.0, 3.0),
        "industrial" | "warehouse" | "manufacture" | "factory" => (7.0, 2.5),
        "church" | "cathedral" | "chapel" | "temple" | "mosque" => (14.0, 4.0),
        "garage" | "garages" | "shed" | "hut" | "roof" | "carport" => (3.0, 0.8),
        _ => (7.0, 2.5),
    };
    (base + (unit_rand(seed.rotate_left(7)) * 2.0 - 1.0) * spread).max(2.5)
}

/// Parse a leading number from a JSON value that may be a number or a string like "12 m".
fn num_from_value(v: &serde_json::Value) -> Option<f32> {
    if let Some(n) = v.as_f64() {
        return Some(n as f32);
    }
    let s = v.as_str()?;
    let num: String = s
        .trim()
        .chars()
        .take_while(|c| c.is_ascii_digit() || *c == '.' || *c == '-')
        .collect();
    num.parse::<f32>().ok()
}
