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
use std::sync::{Arc, Mutex};

use bevy::asset::RenderAssetUsages;
use bevy::image::{ImageAddressMode, ImageSampler, ImageSamplerDescriptor};
use bevy::prelude::*;
use bevy::tasks::IoTaskPool;
use big_space::prelude::{BigSpace, Grid, GridCell};

use crate::resources::OsmTileUrlTemplate;
use crate::systems::road_texture::{road_texture, RoadClass};
use crate::systems::free_camera::{FreeCamera, WORLD_CELL_EDGE, WORLD_SWITCH_THRESHOLD};
use crate::systems::globe::GlobeState;
use crate::systems::map_stream::{lat_lng_to_tile_frac, MapStream};
use crate::systems::tile_loader::{load_tile_image, tile_source_kind, TileSourceKind};
use crate::systems::vector_tiles::{self, SurfaceGroup, SurfaceMesh};
use crate::systems::vegetation::{self, VegetationLod, VegetationSurface};
use crate::systems::vector_tiles::ShoreField;
use crate::systems::water::{self, WaterMaterial, WaterSeaState};
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

/// A z14 cell coordinate (absolute Web Mercator tile x/y at [`GEOMETRY_ZOOM`]).
type Cell = (i64, i64);

/// Max concurrent tessellation tasks. Small so single-threaded wasm spreads the
/// heavy per-tile work (MVT decode + earcut + road raster + vegetation) across
/// frames instead of freezing on a 400-tile burst; native threads it anyway.
const SURFACE_MAX_IN_FLIGHT: usize = 4;

/// A finished tessellation (`None` = fetch or tessellation failed), plus the
/// cell's distance-to-shore field when it holds any water (`systems::water`).
type Tessellated = (Cell, Option<Vec<SurfaceMesh>>, Option<ShoreField>);

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
    /// Finished tessellations, drained on the main thread.
    results: Arc<Mutex<Vec<Tessellated>>>,
    /// Tessellation tasks spawned but not yet drained. Bounded to
    /// [`SURFACE_MAX_IN_FLIGHT`] so a burst of cells doesn't spawn hundreds of heavy
    /// earcut/raster tasks at once — which, on single-threaded wasm (no worker
    /// threads), stalls the frame loop. ADR-033.
    in_flight: usize,
    /// One material per surface group, created on first use.
    materials: HashMap<SurfaceGroup, Handle<StandardMaterial>>,
    /// Anchor generation these cells belong to; a re-anchor clears everything.
    generation: u32,
}

/// Spawn one surface-tessellation task (ADR-033 seam): fetch the MVT body (shared
/// cache with the texture path — same bytes serve both), tessellate areas + roads +
/// trees + the water shore field off the main thread, and hand the finished buffers
/// back for the render loop to upload. Replaces one worker of the old pool.
fn spawn_geometry_task(
    cell: Cell,
    tile_m: f32,
    template: String,
    results: Arc<Mutex<Vec<Tessellated>>>,
) {
    IoTaskPool::get()
        .spawn(async move {
                let key = TileKey::new(cell.0, cell.1, GEOMETRY_ZOOM);
                let mut shore = None;
                let outcome = match load_tile_image(&key, &template).await {
                    Ok(bytes) => match vector_tiles::tessellate_surfaces(&bytes, tile_m) {
                        Ok(mut meshes) => {
                            // Roads share the fetch, the worker and the per-cell
                            // batching with the area surfaces — they differ only
                            // in being lines, so they ride along here rather than
                            // running a second streamer over the same tiles.
                            match vector_tiles::tessellate_roads(&bytes, tile_m) {
                                Ok(roads) => meshes.extend(roads),
                                Err(e) => {
                                    tracing::warn!(cell = ?cell, error = %e, "road tessellation failed");
                                }
                            }
                            // Trees ride the same fetch and the same per-cell
                            // batching (`systems::vegetation`). Scattering and
                            // merging thousands of canopies is pure CPU work, so
                            // it belongs on this worker and not on the frame that
                            // uploads the result.
                            match vegetation::scatter_vegetation(&bytes, tile_m, veg_seed(cell)) {
                                Ok(trees) => {
                                    meshes.extend(vegetation::build_vegetation_meshes(&trees));
                                }
                                Err(e) => {
                                    tracing::warn!(cell = ?cell, error = %e, "vegetation scatter failed");
                                }
                            }
                            // The water meshes carry no notion of where the bank
                            // is (earcut only ever emits boundary vertices), so
                            // the shader gets it as a rasterised distance field —
                            // built here, on the worker, for the same reason.
                            if meshes.iter().any(|m| m.group == SurfaceGroup::Water) {
                                match vector_tiles::water_shore_field(&bytes, tile_m) {
                                    Ok(field) => shore = field,
                                    Err(e) => {
                                        tracing::warn!(cell = ?cell, error = %e, "shore field failed");
                                    }
                                }
                            }
                            Some(meshes)
                        }
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
                    g.push((cell, outcome, shore));
                }
        })
        .detach();
}

impl MapSurfaces {
    /// The shared material for a surface group, once some cell has created it.
    /// `systems::vegetation` ramps the canopy, trunk and forest colours through
    /// these handles — one write per group repaints every live cell, which is the
    /// same reason the handles are shared in the first place.
    #[must_use]
    pub fn material(&self, group: SurfaceGroup) -> Option<&Handle<StandardMaterial>> {
        self.materials.get(&group)
    }

    /// Retire every live cell (used on a re-anchor, or when the source stops being
    /// a vector one). Entities go through the despawn queue, not straight out.
    fn clear(&mut self) {
        for (_, entities) in self.loaded.drain() {
            self.despawn_queue.extend(entities);
        }
        self.requested.clear();
        // In-flight tasks push to the (retained) results and drain harmlessly next
        // frame; reset the count so the new anchor starts with a full slot budget.
        self.in_flight = 0;
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
    mut images: ResMut<Assets<Image>>,
    camera: Query<(&GridCell, &Transform), With<FreeCamera>>,
    bigspace: Query<Entity, With<BigSpace>>,
    globe: Res<GlobeState>,
    sea_state: Res<WaterSeaState>,
    mut water_materials: ResMut<Assets<WaterMaterial>>,
    veg_lod: Res<VegetationLod>,
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

    // 2. Queue cells not yet fetched as detached tessellation tasks, bounded to
    // SURFACE_MAX_IN_FLIGHT concurrent. Unqueued cells are re-evaluated next frame as
    // slots free (the `requested` set already de-dups), so nothing is dropped.
    for c in &wanted {
        if surfaces.in_flight >= SURFACE_MAX_IN_FLIGHT {
            break;
        }
        if surfaces.loaded.contains_key(c) || surfaces.requested.contains(c) {
            continue;
        }
        surfaces.requested.insert(*c);
        surfaces.in_flight += 1;
        spawn_geometry_task(*c, tile_m, template.clone(), surfaces.results.clone());
    }

    // 3. Upload finished tessellations, capped per frame.
    let ready: Vec<Tessellated> = {
        let Ok(mut guard) = surfaces.results.lock() else {
            return;
        };
        let n = guard.len().min(MAX_BUILDS_PER_FRAME);
        guard.drain(..n).collect()
    };
    // Each drained result is one finished task — free its concurrency slot.
    surfaces.in_flight = surfaces.in_flight.saturating_sub(ready.len());
    for (c, outcome, shore) in ready {
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

        // One water material per cell — it carries that cell's shore field, so it
        // cannot be shared the way the flat-colour group materials are. Built once
        // per cell, not per mesh, and dropped with the cell's entities.
        let water_material = surface_meshes
            .iter()
            .any(|m| m.group == SurfaceGroup::Water)
            .then(|| {
                let field = shore.map(|f| images.add(water::shore_field_image(&f)));
                water_materials.add(sea_state.material(field, Vec2::new(wx, wz), tile_m))
            });

        let mut entities = Vec::with_capacity(surface_meshes.len());
        for sm in surface_meshes {
            let mesh = meshes.add(build_mesh(&sm, wx, wz));
            let world = Vec3::new(wx, sm.group.y_offset(), wz);
            let (grid_cell, local) = Grid::new(WORLD_CELL_EDGE, WORLD_SWITCH_THRESHOLD)
                .translation_to_grid(world.as_dvec3());
            // Shared bundle; only the material component differs by group.
            let placement = (
                Mesh3d(mesh),
                Transform::from_translation(local),
                grid_cell,
                ChildOf(root),
                MapSurface,
                // Hidden from orbit alongside the ground tiles (ADR-025).
                crate::systems::globe::HideInSpace,
            );
            let id = if let (SurfaceGroup::Water, Some(water)) = (sm.group, &water_material) {
                // Water gets the wave material (`systems::water`), which owns its
                // albedo, roughness and depth bias — and this cell's shore field.
                commands
                    .spawn((placement, MeshMaterial3d(water.clone())))
                    .id()
            } else {
                let material = surfaces
                    .materials
                    .entry(sm.group)
                    .or_insert_with(|| {
                        let [r, g, b] = sm.group.base_color();
                        materials.add(StandardMaterial {
                            base_color: Color::srgb(r, g, b),
                            // Roads sample their tarmac and markings from a
                            // generated surface; the area groups are flat colour.
                            base_color_texture: match sm.group {
                                SurfaceGroup::Road(class) => {
                                    Some(images.add(road_surface_image(class)))
                                }
                                _ => None,
                            },
                            perceptual_roughness: sm.group.perceptual_roughness(),
                            // What actually resolves the coplanar ground stack —
                            // see `SurfaceGroup::depth_bias`. Physical separation
                            // is only millimetres and cannot survive the far plane.
                            depth_bias: sm.group.depth_bias(),
                            ..default()
                        })
                    })
                    .clone();
                commands
                    .spawn((placement, MeshMaterial3d(material)))
                    .id()
            };
            if matches!(sm.group, SurfaceGroup::Canopy | SurfaceGroup::Trunk) {
                // Read the *current* ramp at spawn, the way new globe tiles read
                // the current albedo: a cell streaming in above the fade band must
                // not flash a full forest for the frame before the ramp catches up.
                commands
                    .entity(id)
                    .insert((VegetationSurface, veg_lod.visibility()));
            }
            entities.push(id);
        }
        tracing::debug!(cell = ?c, groups = entities.len(), "surface cell built");
        surfaces.loaded.insert(c, entities);
    }
}

/// Upload a class's generated road surface (see [`road_texture`]) as a texture.
///
/// Mipmapped and anisotropic, both for the same reason: a road is the one ground
/// surface you almost always see at a grazing angle, running away to the horizon.
/// Un-mipmapped lane markings turn into crawling speckle within a few dozen
/// metres, and isotropic filtering blurs the far end of the street into flat grey
/// long before it should. Repeat addressing is what lets `v` keep counting metres
/// down the road instead of clamping at the first texture length.
fn road_surface_image(class: RoadClass) -> Image {
    use bevy::render::render_resource::TextureFormat;

    let t = road_texture(class);
    let (data, mip_count) = crate::systems::rendering::build_mip_chain(t.width, t.height, t.rgba);
    crate::systems::rendering::image_from_mip_chain(
        t.width,
        t.height,
        data,
        mip_count,
        TextureFormat::Rgba8UnormSrgb,
        RenderAssetUsages::default(),
        ImageSampler::Descriptor(ImageSamplerDescriptor {
            address_mode_u: ImageAddressMode::Repeat,
            address_mode_v: ImageAddressMode::Repeat,
            address_mode_w: ImageAddressMode::Repeat,
            anisotropy_clamp: 8,
            ..ImageSamplerDescriptor::linear()
        }),
    )
}

/// Apply the "Road textures" toggle (`DataLayers::roads_textured`). Roads share one
/// cached [`StandardMaterial`] per class (`MapSurfaces::materials`), so toggling here
/// touches a handful of materials and **no** entities — every road cell, present or
/// future, rides the shared handle. Textured = generated tarmac + markings on white;
/// flat = the class's base tarmac colour with no texture.
///
/// Reconciles every frame but only reads (`get`) unless a material is on the wrong
/// side of the toggle, so a steady state costs nothing and a newly-streamed class
/// (created textured by the spawner) is corrected on the next frame.
pub fn apply_road_texture(
    dl: Res<crate::systems::data_layers::DataLayers>,
    surfaces: Res<MapSurfaces>,
    mut materials: ResMut<Assets<StandardMaterial>>,
    mut images: ResMut<Assets<Image>>,
) {
    let want_textured = dl.roads_textured;
    for (group, handle) in &surfaces.materials {
        let SurfaceGroup::Road(class) = group else {
            continue;
        };
        let has_texture = materials
            .get(handle)
            .is_some_and(|m| m.base_color_texture.is_some());
        if has_texture == want_textured {
            continue;
        }
        let Some(mut material) = materials.get_mut(handle) else {
            continue;
        };
        if want_textured {
            material.base_color_texture = Some(images.add(road_surface_image(*class)));
            material.base_color = Color::WHITE;
        } else {
            material.base_color_texture = None;
            let [r, g, b] = class.flat_color();
            material.base_color = Color::srgb(r, g, b);
        }
    }
}

/// Turn a tessellated surface into a Bevy mesh. Flat ground, so every normal is up.
///
/// UV0 means one of two things, decided by the tessellator:
///
/// * **Area surfaces** get **metres from the map anchor** (`wx`/`wz` are the
///   cell's north-west corner in that frame), not a 0–1 tile parametrisation: the
///   water shader evaluates its wave field there, and it has to be continuous
///   from cell to cell or every cell boundary becomes a seam in the sea. World
///   position would not do — under `big_space` the origin jumps a whole grid cell
///   as the camera moves, which would snap the phase of the entire surface. This
///   frame only changes on a re-anchor, which rebuilds all the meshes anyway.
/// * **Roads** carry their own UVs from `tessellate_roads` (across × along the
///   carriageway) and keep them: those are what register the lane markings with
///   the ribbon, and a positional UV would smear the paint across the map.
fn build_mesh(sm: &SurfaceMesh, wx: f32, wz: f32) -> Mesh {
    use bevy::asset::RenderAssetUsages;
    use bevy::render::mesh::{Indices, PrimitiveTopology};

    // Ground is flat, so the common case is a straight-up normal per vertex; only
    // the vegetation meshes have volume and bring their own.
    let normals: Vec<[f32; 3]> = if sm.normals.len() == sm.positions.len() {
        sm.normals.clone()
    } else {
        vec![[0.0f32, 1.0, 0.0]; sm.positions.len()]
    };
    let uvs: Vec<[f32; 2]> = if sm.uvs.len() == sm.positions.len() {
        sm.uvs.clone()
    } else {
        sm.positions
            .iter()
            .map(|p| [wx + p[0], wz + p[2]])
            .collect()
    };
    let mut mesh = Mesh::new(
        PrimitiveTopology::TriangleList,
        RenderAssetUsages::default(),
    )
    .with_inserted_attribute(Mesh::ATTRIBUTE_POSITION, sm.positions.clone())
    .with_inserted_attribute(Mesh::ATTRIBUTE_NORMAL, normals)
    .with_inserted_attribute(Mesh::ATTRIBUTE_UV_0, uvs)
    .with_inserted_indices(Indices::U32(sm.indices.clone()));
    // `StandardMaterial` multiplies `base_color` by this whenever the attribute is
    // present — which is how one canopy material gives thousands of trees their own
    // tone and brightness without a material or a draw call per tree.
    if sm.colors.len() == sm.positions.len() {
        mesh.insert_attribute(Mesh::ATTRIBUTE_COLOR, sm.colors.clone());
    }
    mesh
}

/// Seed for a cell's tree scatter. Stable for the cell's absolute tile coordinate,
/// so streaming out and back regrows the identical forest rather than reshuffling
/// it — a reshuffle shows up as a shimmer whenever the ring crosses a boundary.
fn veg_seed(cell: Cell) -> u64 {
    (cell.0 as u64).wrapping_mul(0x9E37_79B9_7F4A_7C15) ^ (cell.1 as u64).rotate_left(32)
}
