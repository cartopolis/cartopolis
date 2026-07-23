//! Vector map surfaces as real geometry (ADR-035, option B).
//!
//! `map_stream` draws the map as *textures* on ground quads. This module draws the
//! same source data as **meshes**: water and landuse polygons tessellated from MVT
//! and spawned into the scene, so they are lit by the sun, shaded by the atmosphere
//! and reactive to the day/night cycle — none of which a baked raster tile can do.
//!
//! It runs *alongside* the textured tiles rather than replacing them. The texture
//! stays as the backdrop (and covers the coarse rings beyond this module's radius);
//! the geometry sits a few millimetres above it. That keeps the map working at every
//! altitude while the geometry path grows layer by layer — roads and routing next.
//!
//! Three constraints shape the design, all of them from the same failure mode
//! (`VK_ERROR_DEVICE_LOST` on a weak iGPU, see CLAUDE.md):
//!
//! * **One mesh per surface group per cell**, never one per feature. A z14 tile has
//!   ~120 water and landuse polygons; batched, that is at most 5 meshes.
//! * **A per-frame build cap**, like `globe::MAX_GLOBE_TILE_BUILDS_PER_FRAME`.
//! * **A per-frame despawn cap**, like `map_stream::MAX_TILE_DESPAWNS_PER_FRAME` —
//!   retiring a whole ring in one command buffer is what killed the device before.
//!
//! Only vector sources produce geometry; against a raster template (the OSM PNG
//! default, or the VersaTiles satellite layer) this module does nothing at all.

use std::collections::{HashMap, HashSet};
use std::sync::mpsc::Sender;
use std::sync::{Arc, Mutex};

use bevy::prelude::*;
use big_space::prelude::{BigSpace, Grid, GridCell};

use crate::resources::OsmTileUrlTemplate;
use crate::systems::free_camera::{FreeCamera, WORLD_CELL_EDGE, WORLD_SWITCH_THRESHOLD};
use crate::systems::globe::GlobeState;
use crate::systems::map_stream::{lat_lng_to_tile_frac, MapStream};
use crate::systems::tile_loader::{load_tile_image, tile_source_kind, TileSourceKind};
use crate::systems::vector_tiles::{self, SurfaceGroup, SurfaceMesh};
use vibe_core::world::tile_to_meters;
use vibe_core::TileKey;

/// Cells are streamed at the vector source's own zoom — no overzoom, because
/// geometry needs no LOD pyramid: the same mesh is correct at any distance. This
/// matches `osm_buildings::CACHE_ZOOM` and `lod22::LOD22_ZOOM`, so all three
/// streamers agree on what a "cell" is.
const GEOMETRY_ZOOM: u32 = vector_tiles::MAX_SOURCE_ZOOM;

/// Cells kept live around the camera (radius 1 → 3×3 ≈ 6 km at NL latitudes).
/// Beyond this the textured tiles carry the map on their own.
const CELL_RADIUS: i64 = 1;

/// Meshes uploaded per frame. Each is up to a few thousand triangles; a burst of
/// them in one command buffer is the documented device-loss pattern.
const MAX_BUILDS_PER_FRAME: usize = 2;

/// Cells despawned per frame — the mass-despawn twin of the build cap.
const MAX_DESPAWNS_PER_FRAME: usize = 4;

/// Number of background tessellation workers.
const WORKERS: usize = 2;

/// A z14 cell coordinate (absolute Web Mercator tile x/y at [`GEOMETRY_ZOOM`]).
type Cell = (i64, i64);

/// A tessellation job: which cell, its ground size in metres, and the tile template.
type Job = (Cell, f32, String);

/// A finished tessellation (`None` = fetch or tessellation failed).
type Tessellated = (Cell, Option<Vec<SurfaceMesh>>);

/// Marker for a spawned surface mesh, so the whole layer can be found and cleared.
#[derive(Component)]
pub struct MapSurface;

/// Streaming state for the vector surface meshes.
#[derive(Resource, Default)]
pub struct MapSurfaces {
    /// Live cells → their spawned mesh entities (one per surface group).
    loaded: HashMap<Cell, Vec<Entity>>,
    /// Cells already queued, so a slow fetch isn't requested twice.
    requested: HashSet<Cell>,
    /// Entities retired but not yet despawned (drained under the per-frame cap).
    despawn_queue: Vec<Entity>,
    /// Job queue to the tessellation workers.
    jobs: Option<Sender<Job>>,
    /// Finished tessellations, drained on the main thread.
    results: Arc<Mutex<Vec<Tessellated>>>,
    /// One material per surface group, created on first use.
    materials: HashMap<SurfaceGroup, Handle<StandardMaterial>>,
    /// Anchor generation these cells belong to; a re-anchor clears everything.
    generation: u32,
}

impl MapSurfaces {
    /// Lazily spawn the worker pool. Workers fetch the MVT body (shared on-disk
    /// cache with the texture path — the same bytes serve both) and tessellate it
    /// off the main thread, which only uploads the finished buffers.
    fn jobs_sender(&mut self) -> Sender<Job> {
        if let Some(tx) = &self.jobs {
            return tx.clone();
        }
        let (tx, rx) = std::sync::mpsc::channel::<Job>();
        let rx = Arc::new(Mutex::new(rx));
        for _ in 0..WORKERS {
            let rx = rx.clone();
            let results = self.results.clone();
            std::thread::spawn(move || loop {
                let Ok((cell, tile_m, template)) = ({
                    let Ok(guard) = rx.lock() else { break };
                    guard.recv()
                }) else {
                    break;
                };
                let key = TileKey::new(cell.0, cell.1, GEOMETRY_ZOOM);
                let outcome = match load_tile_image(&key, &template) {
                    Ok(bytes) => match vector_tiles::tessellate_surfaces(&bytes, tile_m) {
                        Ok(meshes) => Some(meshes),
                        Err(e) => {
                            tracing::warn!(cell = ?cell, error = %e, "surface tessellation failed");
                            None
                        }
                    },
                    Err(e) => {
                        tracing::warn!(cell = ?cell, error = %e, "surface tile fetch failed");
                        None
                    }
                };
                if let Ok(mut g) = results.lock() {
                    g.push((cell, outcome));
                }
            });
        }
        self.jobs = Some(tx.clone());
        tx
    }

    /// Retire every live cell (used on a re-anchor, or when the source stops being
    /// a vector one). Entities go through the despawn queue, not straight out.
    fn clear(&mut self) {
        for (_, entities) in self.loaded.drain() {
            self.despawn_queue.extend(entities);
        }
        self.requested.clear();
    }
}

/// Stream vector surface meshes around the camera.
#[allow(clippy::too_many_arguments)]
pub fn update_map_surfaces(
    mut commands: Commands,
    mut surfaces: ResMut<MapSurfaces>,
    mut meshes: ResMut<Assets<Mesh>>,
    mut materials: ResMut<Assets<StandardMaterial>>,
    stream: Res<MapStream>,
    tile_url: Res<OsmTileUrlTemplate>,
    camera: Query<(&GridCell, &Transform), With<FreeCamera>>,
    bigspace: Query<Entity, With<BigSpace>>,
    globe: Res<GlobeState>,
) {
    // Always drain the despawn queue, even on the early returns below — a source
    // switch or a climb into space must still clear steadily rather than stall.
    if !surfaces.despawn_queue.is_empty() {
        let n = surfaces.despawn_queue.len().min(MAX_DESPAWNS_PER_FRAME);
        for e in surfaces.despawn_queue.drain(..n) {
            commands.entity(e).despawn();
        }
    }

    let template = tile_url.0.lock().map(|g| g.clone()).unwrap_or_default();
    if tile_source_kind(&template) != TileSourceKind::Vector {
        // Raster source (OSM PNG, or satellite imagery): nothing to tessellate.
        if !surfaces.loaded.is_empty() {
            surfaces.clear();
        }
        return;
    }

    let Some((clat, clng, _base_zoom)) = stream.geo_anchor() else {
        return;
    };
    let Ok((cell, tf)) = camera.single() else {
        return;
    };
    let Ok(root) = bigspace.single() else {
        return;
    };

    // A re-anchor invalidates every cell's placement — drop them and refetch.
    if surfaces.generation != stream.anchor_generation {
        surfaces.generation = stream.anchor_generation;
        surfaces.clear();
    }

    // Freeze from orbit, exactly as `map_stream` does: the ground is hidden up
    // there anyway, and roaming in space would fetch cells for nonsense coords.
    if globe.altitude_m(cell, tf) as f32 > crate::systems::globe::GLOBE_SHOW_ALT {
        return;
    }

    let tile_m = tile_to_meters(GEOMETRY_ZOOM, clat) as f32;
    let (fcx, fcy) = lat_lng_to_tile_frac(clat, clng, GEOMETRY_ZOOM);
    let world_x = cell.x as f32 * WORLD_CELL_EDGE + tf.translation.x;
    let world_z = cell.z as f32 * WORLD_CELL_EDGE + tf.translation.z;
    let ccx = (f64::from(world_x) / f64::from(tile_m) + fcx).floor() as i64;
    let ccy = (f64::from(world_z) / f64::from(tile_m) + fcy).floor() as i64;

    // 1. Retire cells that fell outside the ring.
    let wanted: HashSet<Cell> = (-CELL_RADIUS..=CELL_RADIUS)
        .flat_map(|b| (-CELL_RADIUS..=CELL_RADIUS).map(move |a| (a, b)))
        .map(|(a, b)| (ccx + a, ccy + b))
        .collect();
    let stale: Vec<Cell> = surfaces
        .loaded
        .keys()
        .filter(|c| !wanted.contains(*c))
        .copied()
        .collect();
    for c in stale {
        if let Some(entities) = surfaces.loaded.remove(&c) {
            surfaces.despawn_queue.extend(entities);
        }
        surfaces.requested.remove(&c);
    }

    // 2. Queue cells not yet fetched.
    let jobs = surfaces.jobs_sender();
    for c in &wanted {
        if surfaces.loaded.contains_key(c) || surfaces.requested.contains(c) {
            continue;
        }
        surfaces.requested.insert(*c);
        let _ = jobs.send((*c, tile_m, template.clone()));
    }

    // 3. Upload finished tessellations, capped per frame.
    let ready: Vec<Tessellated> = {
        let Ok(mut guard) = surfaces.results.lock() else {
            return;
        };
        let n = guard.len().min(MAX_BUILDS_PER_FRAME);
        guard.drain(..n).collect()
    };
    for (c, outcome) in ready {
        // Dropped from the ring (or re-anchored) while the worker was busy.
        if !wanted.contains(&c) {
            surfaces.requested.remove(&c);
            continue;
        }
        let Some(surface_meshes) = outcome else {
            // Leave it out of `loaded` but keep it in `requested` so a failed cell
            // is not retried every frame; the ring moving off it clears the flag.
            continue;
        };

        // Cell's north-west corner in world metres — the origin the tessellator's
        // tile-local coordinates are relative to.
        let wx = ((c.0 as f64 - fcx) * f64::from(tile_m)) as f32;
        let wz = ((c.1 as f64 - fcy) * f64::from(tile_m)) as f32;

        let mut entities = Vec::with_capacity(surface_meshes.len());
        for sm in surface_meshes {
            let material = surfaces
                .materials
                .entry(sm.group)
                .or_insert_with(|| {
                    let [r, g, b] = sm.group.base_color();
                    materials.add(StandardMaterial {
                        base_color: Color::srgb(r, g, b),
                        // Water reads as water through its highlight, not its hue —
                        // this is the thing a baked raster tile can never do.
                        perceptual_roughness: if sm.group == SurfaceGroup::Water {
                            0.15
                        } else {
                            0.95
                        },
                        // What actually resolves the coplanar ground stack — see
                        // `SurfaceGroup::depth_bias`. Physical separation is only
                        // millimetres here and cannot survive the far plane.
                        depth_bias: sm.group.depth_bias(),
                        ..default()
                    })
                })
                .clone();

            let mesh = meshes.add(build_mesh(&sm));
            let world = Vec3::new(wx, sm.group.y_offset(), wz);
            let (grid_cell, local) = Grid::new(WORLD_CELL_EDGE, WORLD_SWITCH_THRESHOLD)
                .translation_to_grid(world.as_dvec3());
            entities.push(
                commands
                    .spawn((
                        Mesh3d(mesh),
                        MeshMaterial3d(material),
                        Transform::from_translation(local),
                        grid_cell,
                        ChildOf(root),
                        MapSurface,
                        // Hidden from orbit alongside the ground tiles (ADR-025).
                        crate::systems::globe::HideInSpace,
                    ))
                    .id(),
            );
        }
        tracing::debug!(cell = ?c, groups = entities.len(), "surface cell built");
        surfaces.loaded.insert(c, entities);
    }
}

/// Turn a tessellated surface into a Bevy mesh. Flat ground, so every normal is up
/// and UVs are unused (the material is a solid colour, lit by the scene).
fn build_mesh(sm: &SurfaceMesh) -> Mesh {
    use bevy::asset::RenderAssetUsages;
    use bevy::render::mesh::{Indices, PrimitiveTopology};

    let normals = vec![[0.0f32, 1.0, 0.0]; sm.positions.len()];
    Mesh::new(
        PrimitiveTopology::TriangleList,
        RenderAssetUsages::default(),
    )
    .with_inserted_attribute(Mesh::ATTRIBUTE_POSITION, sm.positions.clone())
    .with_inserted_attribute(Mesh::ATTRIBUTE_NORMAL, normals)
    .with_inserted_indices(Indices::U32(sm.indices.clone()))
}
