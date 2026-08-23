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

use crate::components::MapSurface;
use crate::components::{FreeCamera, FurnitureSurface, VegetationSurface};
use crate::grid::{WORLD_CELL_EDGE, WORLD_SWITCH_THRESHOLD};
use std::collections::{HashMap, HashSet};
use std::sync::{Arc, Mutex};

use bevy::asset::RenderAssetUsages;
use bevy::image::{ImageAddressMode, ImageSampler, ImageSamplerDescriptor};
use bevy::prelude::*;
use bevy::tasks::IoTaskPool;
use big_space::prelude::{BigSpace, Grid, GridCell};

use crate::platform::stream::{CellStream, Policy};
use crate::resources::OsmTileUrlTemplate;
use crate::systems::furniture;
use crate::systems::globe::GlobeState;
use crate::systems::grass::GrassAreas;
use crate::systems::height::{wgs84_to_rd, HeightLayer, HeightModel, HeightRequest};
use crate::systems::map_stream::{lat_lng_to_tile_frac, MapStream};
use crate::systems::places;
use crate::systems::road_depth::{RoadDepthExtension, RoadMaterial};
use crate::systems::road_texture::{road_texture, RoadClass};
use crate::systems::scatter;
use crate::systems::street_furniture::FurnitureLod;
use crate::systems::street_labels::StreetLabelStore;
use crate::systems::terrain::TerrainField;
use crate::systems::tile_loader::load_tile_image;
use crate::systems::turf::GrassArea;
use crate::systems::vector_tiles::ShoreField;
use crate::systems::vector_tiles::{self, BridgeDeck, StreetLabelLine, SurfaceGroup, SurfaceMesh};
use crate::systems::vegetation::VegetationLod;
use crate::systems::water::{self, WaterMaterial, WaterSeaState};
use crate::systems::world_places::{CellPlaces, WorldPlaces};
use cartopolis_core::world::tile_to_meters;
use cartopolis_core::TileKey;
use cartopolis_core::{tile_source_kind, TileSourceKind};

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

/// Max concurrent tessellation tasks.
///
/// This was 4, to keep a 400-cell burst of MVT decode + earcut + road raster +
/// vegetation from freezing the frame loop — which it did, because that work ran
/// on `IoTaskPool` and on wasm that is the main thread. It runs on
/// `platform::workers` now, so the frame loop is no longer what a burst lands on,
/// and a cap of 4 would instead leave most of the pool idle: the pool is sized to
/// `hardware_concurrency - 1` (clamped to 8), so fewer in flight than workers is
/// simply parallelism left unused.
///
/// Still bounded rather than unlimited: each finished cell holds meshes plus a
/// shore field until the frame loop drains it at `MAX_BUILDS_PER_FRAME`, so an
/// uncapped burst trades a frame-time problem for a memory one. Matching the
/// pool's own ceiling keeps every worker fed without queueing results faster than
/// they can be consumed.
const SURFACE_MAX_IN_FLIGHT: usize = 8;

/// A finished tessellation (`None` = fetch or tessellation failed), the cell's
/// distance-to-shore field when it holds any water (`systems::water`), its
/// bridge decks — which are handed over *unmerged*, because each one still has to
/// be told how high it stands (see [`resolve_bridge_decks`]) — and the street
/// centre lines its names will be written along (`systems::street_labels`).
///
/// This is the `R` of the cell's [`CellStream`]; the `Cell` is its key.
type Tessellated = (
    Option<Vec<SurfaceMesh>>,
    Option<ShoreField>,
    Vec<BridgeDeck>,
    Vec<StreetLabelLine>,
    // What the cell holds that a player can walk up to: the furniture standing in
    // its streets (which is also what makes those props solid) and its named
    // places and house numbers. All read out of the same parse as everything
    // above — see `systems::places`.
    CellPlaces,
    // Where a blade of grass may stand in this cell (`systems::grass`). `None`
    // when the layer is off; empty — and so dropped on arrival — for the many
    // cells of a city centre that hold no green at all.
    Option<GrassArea>,
);

/// The per-cell stores that live and die with a surface cell: the street names
/// written into the roads, everything in the cell a player can walk up to, and
/// the ground its grass may grow on.
///
/// One parameter rather than three because `update_map_surfaces` is at Bevy's
/// 16-parameter ceiling — the same reason `free_camera::CameraInput` exists — and
/// because these genuinely are one concern: each is keyed by cell, each is
/// inserted at the cell's origin, and all must be dropped on exactly the same
/// events, or the map ends up naming a street it no longer draws — or growing
/// grass on ground it has stopped holding.
#[derive(bevy::ecs::system::SystemParam)]
pub struct CellStores<'w> {
    labels: ResMut<'w, StreetLabelStore>,
    places: ResMut<'w, WorldPlaces>,
    decks: ResMut<'w, DeckPlates>,
    grass: ResMut<'w, GrassAreas>,
}

/// Everything a surface cell needs to *make a material out of*, in one parameter
/// for the same reason [`CellStores`] is one: `update_map_surfaces` sits on Bevy's
/// 16-parameter ceiling, and the road ladder added a fourth asset collection to
/// the three it already held.
///
/// There are three material types because there are three shaders. The flat
/// ground groups are plain `StandardMaterial`; water owns its wave shader; roads
/// carry the depth ladder in a vertex attribute (`systems::road_depth`) and so
/// need a pipeline that declares it.
#[derive(bevy::ecs::system::SystemParam)]
pub struct SurfaceMaterials<'w> {
    plain: ResMut<'w, Assets<StandardMaterial>>,
    water: ResMut<'w, Assets<WaterMaterial>>,
    road: ResMut<'w, Assets<RoadMaterial>>,
    images: ResMut<'w, Assets<Image>>,
}

/// Where the raised bridge decks are and how high they stand, in the anchored
/// world-XZ frame — the deck heights kept as a *queryable* fact rather than only as
/// vertices in a mesh.
///
/// It exists because a deck is the one piece of this map that other things have to
/// stand on. `resolve_bridge_decks` already knows every number in here; before this
/// it spent them into `build_deck_mesh` and forgot them, so the route line — drawn
/// on the ground plane, as everything is — ran *underneath* every viaduct it was
/// routed over. Same shape and same contract as `collision::BuildingColliders`: the
/// streamer publishes, consumers sample, and an entry dies with its cell.
///
/// Only decks that were actually *lifted* are kept ([`MIN_LIFT_M`]). A flat deck is
/// a piece of road surface and there is nothing to climb onto.
#[derive(Resource, Default)]
pub struct DeckPlates {
    plates: HashMap<Cell, Vec<Plate>>,
    /// Bumped on every change, so a consumer that caches geometry can tell it has
    /// gone stale. The decks land some frames *after* the cell they belong to —
    /// their height is a measurement — so "the route was drawn before the viaduct
    /// arrived" is the ordinary case, not an edge one, and the route's redraw key
    /// carries this for exactly that reason.
    pub revision: u32,
}

/// One lifted deck: its outline as the tile drew it, where that tile sits in the
/// world, and how high the structure stands.
struct Plate {
    deck: BridgeDeck,
    /// Cell origin in world metres; a deck's rings are tile-local.
    world: Vec2,
    height: f32,
}

impl DeckPlates {
    /// Install (or replace) one cell's lifted decks.
    fn set_cell(&mut self, cell: Cell, plates: Vec<Plate>) {
        if plates.is_empty() && !self.plates.contains_key(&cell) {
            return;
        }
        self.plates.insert(cell, plates);
        self.revision = self.revision.wrapping_add(1);
    }

    /// Drop one cell's decks (cell retired).
    pub fn remove_cell(&mut self, cell: &Cell) {
        if self.plates.remove(cell).is_some() {
            self.revision = self.revision.wrapping_add(1);
        }
    }

    /// Drop every deck (re-anchor, or the source stopped being a vector one).
    pub fn clear(&mut self) {
        if !self.plates.is_empty() {
            self.plates.clear();
            self.revision = self.revision.wrapping_add(1);
        }
    }

    /// How far above the ground plane a deck carries this world-XZ point, or 0
    /// where none does.
    ///
    /// The **tallest** deck wins where two overlap. Stacked decks at an interchange
    /// are the case, and anything standing on them wants the top one — taking the
    /// first hit instead would make the answer depend on cell iteration order, which
    /// is a `HashMap`'s, i.e. it would change between runs.
    #[must_use]
    pub fn lift_at(&self, p: Vec2) -> f32 {
        let mut best = 0.0f32;
        for plate in self.plates.values().flatten() {
            if plate.height > best
                && plate
                    .deck
                    .contains([p.x - plate.world.x, p.y - plate.world.y])
            {
                best = plate.height;
            }
        }
        best
    }
}

/// Deck slab thickness in metres: the fascia band extruded down from the surface,
/// so a bridge reads as a structure with an edge and a shadow rather than as a
/// sheet of paper floating over the tracks.
const DECK_THICKNESS_M: f32 = 0.9;

/// Below this measured height a deck is laid **flat**, at the level of the roads
/// that reach it.
///
/// Not a tolerance on the measurement — the measurement is fine. It is that the
/// road network this deck connects to is flat at `y = 0`, so any lift is a step at
/// both ends, and the step is only worth paying for where something genuinely
/// passes *underneath*. A Dutch canal bridge measures a metre or so above its
/// quays (a deck does arch over the water) while being level with the street it
/// carries; lifting it built a slab floating over its own approach roads. Above
/// this, the clearance is a grade separation — a viaduct over a rail yard measures
/// 6.6 m here — and the step is the honest lesser evil until approach ramps are
/// sampled from AHN too.
const MIN_LIFT_M: f32 = 2.5;

/// Below this height a deck gets no fascia: a flat deck is a piece of road
/// surface, and a skirt around it would read as a kerb.
const FASCIA_MIN_M: f32 = 0.4;

/// Ceiling on a measured deck height, in metres. Beyond this the measurement is
/// not a bridge: it is a crane, a gantry or a stack of containers that the DSM
/// caught standing on the structure.
const MAX_DECK_M: f32 = 25.0;

/// How far outside a deck the ground level is measured. Wide enough to clear the
/// structure and its abutments, narrow enough to stay in the same terrain.
const GROUND_RING_M: f64 = 30.0;

/// Target sample spacing across a deck, in metres, and the bounds on the grid that
/// comes out of it. A 20 m canal bridge wants a fine grid; a 180 m viaduct does not
/// need one to know how high it is.
const DECK_SAMPLE_M: f32 = 1.5;
const DECK_GRID_MIN: u32 = 16;
const DECK_GRID_MAX: u32 = 64;

/// Streaming state for the vector surface meshes.
#[derive(Resource)]
pub struct MapSurfaces {
    /// Live cells → their spawned mesh entities (one per surface group).
    loaded: HashMap<Cell, Vec<Entity>>,
    /// Requested/failed/in-flight bookkeeping, the results sink, the despawn queue
    /// and the anchor generation (`platform::stream`).
    stream: CellStream<Cell, Tessellated>,
    /// One material per surface group, created on first use.
    materials: HashMap<SurfaceGroup, Handle<StandardMaterial>>,
    /// The same, for the road groups — they carry the depth ladder in a vertex
    /// attribute, so they draw with [`RoadMaterial`] and cannot share the map
    /// above. Still one handle per group: the ordering is per *vertex* precisely
    /// so that splitting the materials was never necessary.
    road_materials: HashMap<SurfaceGroup, Handle<RoadMaterial>>,
    /// Terrain-field revision the live cells were tessellated against, so a field
    /// that lands (or goes away) after them rebuilds them onto the right ground.
    terrain_revision: u32,
    /// Cells whose bridge decks are tessellated but not yet *placed*: their height
    /// is a measurement that arrives from `systems::height` some frames later. Held
    /// here rather than built flat and corrected, so a viaduct never visibly jumps
    /// off the ground once its answer lands.
    pending_decks: HashMap<Cell, PendingDecks>,
    /// Which optional detail the live cells were built with, so flipping a layer
    /// retears them ([`DetailWants`]). `None` until the first pass, which is what
    /// makes the very first frame a build rather than a rebuild.
    detail_wants: Option<DetailWants>,
}

impl Default for MapSurfaces {
    fn default() -> Self {
        Self {
            loaded: HashMap::new(),
            stream: CellStream::new(Policy {
                max_in_flight: SURFACE_MAX_IN_FLIGHT,
                min_interval_secs: 0.0,
                // One attempt while the ring sits over the cell: a tile that failed
                // to fetch or tessellate would fail again the same way, and the
                // ring moving off it forgets the cell (so a return re-tries).
                max_attempts: 1,
                backoff_secs: 0.0,
                backoff_doublings: 0,
                cooldown_secs: 0.0,
                despawns_per_frame: MAX_DESPAWNS_PER_FRAME,
            }),
            materials: HashMap::new(),
            road_materials: HashMap::new(),
            terrain_revision: 0,
            pending_decks: HashMap::new(),
            detail_wants: None,
        }
    }
}

/// A cell's decks, waiting on their heights.
struct PendingDecks {
    decks: Vec<BridgeDeck>,
    /// Cell origin in world metres — the same north-west corner the cell's other
    /// meshes are placed at.
    world: Vec2,
    tile_m: f32,
}

/// Spawn one surface-tessellation task (ADR-033 seam): fetch the MVT body (shared
/// cache with the texture path — same bytes serve both), tessellate areas + roads +
/// trees + the water shore field off the main thread, and hand the finished buffers
/// back for the render loop to upload. Replaces one worker of the old pool.
/// Which of the optional per-cell products this worker should actually build.
///
/// The layers these gate were *displayed* conditionally and **built
/// unconditionally**: a phone with Trees, Furniture and Water shading all off —
/// the tier's own defaults — still scattered up to 6,000 canopies per cell,
/// extracted the street furniture, and rasterised a 1024² distance field per water
/// cell, then uploaded the meshes and spawned them `Visibility::Hidden`. Bevy
/// prepares a `Mesh` per *asset*, not per visible entity, so hiding the entity
/// saved the draw call and nothing else — not the scatter, not the upload, not the
/// vertex buffer. Turning a layer off was very nearly free of benefit, which is
/// most of why "only the vector map layer on" was not a cheaper configuration than
/// everything on.
///
/// Each flag comes from a resource `update_map_surfaces` *already* holds
/// (`VegetationLod::enabled`, `FurnitureLod::enabled`, `WaterSeaState::shading`),
/// every one of which is a documented mirror of its `DataLayers` toggle. That is
/// deliberate: the system sits at Bevy's 16-parameter ceiling, so reading
/// `DataLayers` directly is not available, and the mirrors already exist precisely
/// so spawn-time code can see a toggle.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
struct DetailWants {
    /// Scatter and mesh tree canopies and trunks.
    trees: bool,
    /// Extract street furniture — and with it the furniture *instances*, which are
    /// what make benches solid and what the interaction prompt aims at. So this
    /// flag follows the same rule collision already follows for buildings: what is
    /// not drawn is not collided with.
    furniture: bool,
    /// Build the per-cell shore distance field. See `WaterSeaState::shading`.
    shore: bool,
    /// Read the cell's grass polygons and their keep-outs (`systems::grass`).
    /// Same rule as the rest: with the layer off this is not built, so turning
    /// it off costs nothing at all rather than only saving the draw call.
    grass: bool,
}

impl DetailWants {
    /// Everything on — what a desktop tier asks for, and what the tessellation
    /// tests expect.
    #[cfg(test)]
    const ALL: Self = Self {
        trees: true,
        furniture: true,
        shore: true,
        grass: true,
    };
}

fn spawn_geometry_task(
    cell: Cell,
    tile_m: f32,
    template: String,
    wants: DetailWants,
    results: Arc<Mutex<Vec<(Cell, Tessellated)>>>,
) {
    // Fetch on the task pool, tessellate on the worker pool (ADR-033). The
    // comments below already said this work "belongs on the worker" — it did not:
    // on wasm `IoTaskPool` runs on the main thread, so every MVT decode, earcut,
    // road raster, vegetation scatter and shore field landed on the frame loop.
    // Awaiting the fetch there is free; the rest is what a core is for.
    // Resolved here rather than inside the worker: the ceiling is memoised, but
    // reading it on the frame that *spawns* the task keeps the worker closure
    // free of anything that could touch a system property or a `OnceLock` race
    // on a platform where this is the first caller.
    //
    // `lod22::max_cell_mesh_bytes` rather than `quality::upload_ceiling`, which
    // is the same distinction `lod22` already had to make and this path had
    // not. The raw ceiling is `None` on every rung above the phone, so surfaces,
    // roads and vegetation were cut into byte-bounded pieces on a handset and
    // left whole everywhere else — and "whole" here is not a small number:
    // measured over Groningen on 2026-08-21, one cell's canopies reached a
    // single 6,074 KB / 159k-triangle mesh, which `upload_meter` flags as the
    // asset no per-frame throttle can spread (bevy always writes one whole
    // asset). The derived cap is never above the frame budget and never above
    // 8 MB, so a machine that asked for a budget keeps exactly the pieces it
    // asked for and the rest gain the ceiling they never had.
    let mesh_bytes = Some(crate::systems::lod22::max_cell_mesh_bytes());
    IoTaskPool::get()
        .spawn(async move {
            let key = TileKey::new(cell.0, cell.1, GEOMETRY_ZOOM);
            let fetched = load_tile_image(&key, &template).await;
            crate::platform::workers::submit(Box::new(move || {
                let mut shore = None;
                let mut grass = None;
                let mut decks = Vec::new();
                let mut labels = Vec::new();
                let mut cell_places = CellPlaces::default();
                // **One** parse for all seven consumers. Each of them used to open
                // its own `Reader` over this same body — seven full copies plus
                // seven prost decodes of the tile per streamed cell.
                //
                // Through `parse_shared`, so it is also one parse with whichever
                // clipmap tiles are magnifying this same z14 body right now: the
                // geometry cell and the basemap ring stream the same ground at the
                // same moment, and used to decode it twice over.
                let parsed = fetched
                    .map_err(|e| format!("surface tile fetch failed: {e}"))
                    .and_then(|bytes| {
                        crate::systems::tile_loader::parse_shared(key, &bytes)
                    });
                let outcome = match parsed {
                    Ok(tile) => match vector_tiles::tessellate_surfaces(&tile, tile_m, mesh_bytes) {
                        Ok(mut meshes) => {
                            // Decks first: the road tessellation needs them, since a
                            // bridge-flagged street's own span belongs to the deck
                            // that carries it rather than to the ground plane.
                            match vector_tiles::tessellate_bridges(&tile, tile_m) {
                                Ok(d) => decks = d,
                                Err(e) => {
                                    tracing::warn!(cell = ?cell, error = %e, "bridge tessellation failed");
                                }
                            }
                            // Roads share the fetch, the parse, the worker and the
                            // per-cell batching with the area surfaces — they
                            // differ only in being lines, so they ride along here
                            // rather than running a second streamer over the same
                            // tiles.
                            match vector_tiles::tessellate_roads(&tile, tile_m, &decks, mesh_bytes) {
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
                            if wants.trees {
                                match scatter::scatter_vegetation(&tile, tile_m, veg_seed(cell)) {
                                    Ok(trees) => {
                                        meshes.extend(scatter::build_vegetation_meshes(
                                            &trees,
                                            mesh_bytes,
                                        ));
                                    }
                                    Err(e) => {
                                        tracing::warn!(cell = ?cell, error = %e, "vegetation scatter failed");
                                    }
                                }
                            }
                            // Benches, bins and shelters likewise ride this fetch:
                            // Shortbread's `pois` layer is already in these bytes
                            // and was being decoded and dropped
                            // (`systems::street_furniture`).
                            if wants.furniture {
                                match furniture::extract_furniture(
                                    &tile,
                                    tile_m,
                                    veg_seed(cell),
                                ) {
                                    Ok(items) => {
                                        meshes.extend(furniture::build_furniture_meshes(&items));
                                        // Keep the instances, not just their meshes:
                                        // they are where the props stand, which is
                                        // what makes them solid and what the
                                        // interaction prompt aims at.
                                        cell_places.furniture = items;
                                    }
                                    Err(e) => {
                                        tracing::warn!(cell = ?cell, error = %e, "street furniture extraction failed");
                                    }
                                }
                            }
                            // …and so do the two layers this tile was decoding
                            // and dropping outright (`docs/notes/vector-tile-unused-layers.md`).
                            match places::extract_pois(&tile, tile_m) {
                                Ok(p) => cell_places.pois = p,
                                Err(e) => {
                                    tracing::warn!(cell = ?cell, error = %e, "poi extraction failed");
                                }
                            }
                            match places::extract_addresses(&tile, tile_m) {
                                Ok(a) => cell_places.addresses = a,
                                Err(e) => {
                                    tracing::warn!(cell = ?cell, error = %e, "address extraction failed");
                                }
                            }
                            // Street names ride the same fetch too: they are read
                            // from a layer of the very same tile, and parsing them
                            // here means the label renderer never touches an MVT
                            // body on the frame loop (`systems::street_labels`).
                            match vector_tiles::extract_street_labels(&tile, tile_m) {
                                Ok(l) => labels = l,
                                Err(e) => {
                                    tracing::warn!(cell = ?cell, error = %e, "street label extraction failed");
                                }
                            }
                            // The water meshes carry no notion of where the bank
                            // is (earcut only ever emits boundary vertices), so
                            // the shader gets it as a rasterised distance field —
                            // built here, on the worker, for the same reason.
                            if wants.shore
                                && meshes.iter().any(|m| m.group == SurfaceGroup::Water)
                            {
                                match vector_tiles::water_shore_field(&tile, tile_m) {
                                    Ok(field) => shore = field,
                                    Err(e) => {
                                        tracing::warn!(cell = ?cell, error = %e, "shore field failed");
                                    }
                                }
                            }
                            // Where a blade may stand. Rides this fetch and this
                            // parse like everything above it — the `land` layer
                            // is already decoded here, and the two keep-out masks
                            // are the same ones the tree scatter builds.
                            if wants.grass {
                                let area = cartopolis_geo::turf::GrassArea::build(&tile, tile_m);
                                if !area.is_empty() {
                                    grass = Some(area);
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
                        tracing::warn!(cell = ?cell, error = %e, "surface tile unusable");
                        None
                    }
                };
                if let Ok(mut g) = results.lock() {
                    g.push((cell, (outcome, shore, decks, labels, cell_places, grass)));
                }
            }));
        })
        .detach();
}

impl MapSurfaces {
    /// Are surface cells still being tessellated, placed or retired? The `--shot`
    /// harness's settle test reads this alongside the tile streamers': the ground
    /// imagery goes quiet well before the geometry laid over it does, and a
    /// capture taken on the imagery alone shows a city with no buildings in it.
    #[must_use]
    pub fn streaming_busy(&self) -> bool {
        self.stream.busy() || !self.pending_decks.is_empty()
    }

    /// The shared material for a surface group, once some cell has created it.
    /// `systems::vegetation` ramps the canopy, trunk and forest colours through
    /// these handles — one write per group repaints every live cell, which is the
    /// same reason the handles are shared in the first place.
    #[must_use]
    pub fn material(&self, group: SurfaceGroup) -> Option<&Handle<StandardMaterial>> {
        self.materials.get(&group)
    }

    /// The shared material for a *textureless* surface group, created on first
    /// use with the group's standard recipe. `street_detail` spawns surveyed
    /// trees through this so they hold the same canopy/trunk handles the
    /// scattered ones do — the vegetation fade then repaints both with one
    /// write. Road groups sample a generated texture and are created only by
    /// `update_map_surfaces`, which owns the image assets.
    pub(crate) fn group_material(
        &mut self,
        group: SurfaceGroup,
        materials: &mut Assets<StandardMaterial>,
    ) -> Handle<StandardMaterial> {
        debug_assert!(
            !matches!(group, SurfaceGroup::Road(_) | SurfaceGroup::RoadCasing(_)),
            "road materials carry a texture and are created by update_map_surfaces"
        );
        self.materials
            .entry(group)
            .or_insert_with(|| {
                let [r, g, b] = group.base_color();
                materials.add(StandardMaterial {
                    base_color: Color::srgb(r, g, b),
                    perceptual_roughness: group.perceptual_roughness(),
                    depth_bias: group.depth_bias(),
                    ..default()
                })
            })
            .clone()
    }

    /// Retire every live cell (used on a re-anchor, or when the source stops being
    /// a vector one). Entities go through the despawn queue, not straight out.
    fn clear(&mut self) {
        for (_, entities) in self.loaded.drain() {
            self.stream.retire_all(entities);
        }
        self.pending_decks.clear();
        // Drops the requested/failed bookkeeping and orphans the results sink, so
        // in-flight tasks cannot deliver a cell placed in the old origin's frame.
        self.stream.reset();
    }
}

/// Stream vector surface meshes around the camera.
#[allow(clippy::too_many_arguments)]
pub fn update_map_surfaces(
    mut commands: Commands,
    mut surfaces: ResMut<MapSurfaces>,
    mut meshes: ResMut<Assets<Mesh>>,
    mut mats: SurfaceMaterials,
    stream: Res<MapStream>,
    tile_url: Res<OsmTileUrlTemplate>,
    camera: Query<(&GridCell, &Transform), With<FreeCamera>>,
    bigspace: Query<Entity, With<BigSpace>>,
    globe: Res<GlobeState>,
    sea_state: Res<WaterSeaState>,
    veg_lod: Res<VegetationLod>,
    furniture_lod: Res<FurnitureLod>,
    mut stores: CellStores,
    terrain: Res<TerrainField>,
) {
    // Always drain the despawn queue, even on the early returns below — a source
    // switch or a climb into space must still clear steadily rather than stall.
    surfaces.stream.drain_despawns(&mut commands);
    // The surfaces stream has no backoff and no cooldown (`max_attempts: 1`), so
    // it never consults the clock — and `update_map_surfaces` is already at Bevy's
    // system-parameter ceiling, so adding a `Res<Time>` for a value nothing reads
    // costs a param the next feature will need.
    let now = 0.0;

    let template = tile_url.0.lock().map(|g| g.clone()).unwrap_or_default();
    if tile_source_kind(&template) != TileSourceKind::Vector {
        // Raster source (OSM PNG, or satellite imagery): nothing to tessellate.
        if !surfaces.loaded.is_empty() {
            surfaces.clear();
            stores.labels.clear();
            stores.places.clear();
            stores.decks.clear();
            stores.grass.clear();
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
    if surfaces.stream.sync(stream.anchor_generation) {
        surfaces.clear();
        stores.places.clear();
        stores.labels.clear();
        stores.decks.clear();
        stores.grass.clear();
    }
    // So does the ground moving under them. The measured field lands a second or
    // two after the anchor it belongs to, by which time these cells have already
    // been built flat; they are re-tessellated once, against the cached MVT bodies
    // the first pass fetched. Correcting the meshes in place instead would mean
    // holding every cell's CPU-side geometry for the life of the cell, to save a
    // rebuild that happens once per anchor.
    if surfaces.terrain_revision != terrain.revision {
        surfaces.terrain_revision = terrain.revision;
        surfaces.clear();
        stores.places.clear();
        stores.labels.clear();
        stores.decks.clear();
        stores.grass.clear();
    }

    // The cells hold whatever detail was wanted when their worker ran, so turning
    // a layer **on** has to retear the ring — otherwise the toggle does nothing
    // until the camera happens to move to fresh cells, which reads as a dead
    // switch. Same shape as the terrain revision above, and for the same reason:
    // correcting the meshes in place would mean keeping every cell's CPU-side
    // geometry alive for the life of the cell.
    //
    // Turning one *off* retears too. That is a frame of work to avoid holding
    // meshes the user just said they did not want, on a device where that memory
    // is the constraint.
    let wants = DetailWants {
        trees: veg_lod.enabled,
        furniture: furniture_lod.enabled,
        shore: sea_state.shading,
        grass: stores.grass.enabled,
    };
    if surfaces.detail_wants != Some(wants) {
        surfaces.detail_wants = Some(wants);
        surfaces.clear();
        stores.places.clear();
        stores.labels.clear();
        stores.decks.clear();
        stores.grass.clear();
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
            surfaces.stream.retire_all(entities);
        }
        surfaces.stream.forget(&c);
        stores.labels.remove_cell(&c);
        stores.places.remove_cell(&c);
        stores.decks.remove_cell(&c);
        stores.grass.remove_cell(&c);
    }

    // 2. Queue cells not yet fetched as detached tessellation tasks, bounded to
    // SURFACE_MAX_IN_FLIGHT concurrent. Unqueued cells are re-evaluated next frame as
    // slots free (the `requested` set already de-dups), so nothing is dropped.
    for c in &wanted {
        if !surfaces.loaded.contains_key(c) {
            surfaces.stream.request(*c, now);
        }
    }
    // Dispatch under SURFACE_MAX_IN_FLIGHT; whatever doesn't fit stays queued and
    // starts on a later frame as slots free, so nothing is dropped.
    let sink = surfaces.stream.sink();
    while let Some(c) = surfaces.stream.next_job(now) {
        spawn_geometry_task(c, tile_m, template.clone(), wants, sink.clone());
    }

    // 3. Upload finished tessellations, capped per frame and byte-bounded by the
    // shared `stream_ledger` (a planted cell's canopy meshes are the second
    // largest thing on the startup frame after a LoD2.2 cell); the rest go back
    // into the sink (still holding their concurrency slots, so the bound keeps
    // bounding memory as well as work). Priced by the same `VERTEX_BYTES`
    // arithmetic `build_mesh` below makes true — every gap the tessellator left
    // is filled to that layout before the GPU sees it.
    let mut ready = surfaces.stream.take_results();
    let cap = crate::quality::budget().per_frame(MAX_BUILDS_PER_FRAME);
    let mut take = 0usize;
    for (_, (outcome, ..)) in &ready {
        if take >= cap {
            break;
        }
        let cost = outcome.as_ref().map_or(0, |surface_meshes| {
            surface_meshes
                .iter()
                .map(|sm| {
                    crate::systems::vector_tiles::mesh_upload_bytes(
                        sm.positions.len(),
                        sm.indices.len(),
                    )
                })
                .sum()
        });
        if !crate::systems::stream_ledger::admit(cost) {
            break;
        }
        take += 1;
    }
    let deferred = ready.split_off(take);
    surfaces.stream.defer(deferred);
    for (c, (outcome, shore, decks, street_names, cell_places, grass)) in ready {
        // Dropped from the ring (or re-anchored) while the worker was busy.
        if !wanted.contains(&c) {
            surfaces.stream.forget(&c);
            continue;
        }
        let Some(surface_meshes) = outcome else {
            // Not in `loaded`, and marked failed so it is not retried every frame;
            // the ring moving off it forgets the cell.
            surfaces.stream.failed(&c, now);
            continue;
        };
        surfaces.stream.succeeded(&c);

        // Cell's north-west corner in world metres — the origin the tessellator's
        // tile-local coordinates are relative to.
        let wx = ((c.0 as f64 - fcx) * f64::from(tile_m)) as f32;
        let wz = ((c.1 as f64 - fcy) * f64::from(tile_m)) as f32;

        // The street names go in at the same origin as the meshes, and come back
        // out with them below — a label whose cell has been retired points at a
        // road that is no longer drawn.
        stores.labels.insert_cell(c, wx, wz, street_names);
        // The props and places go in on the same terms and at the same origin:
        // they are retired with the cell below, for the same reason the labels
        // are.
        stores.places.insert_cell(c, wx, wz, cell_places);
        // Same origin, same lifetime, same retire path — see `CellStores`.
        if let Some(area) = grass {
            stores.grass.insert_cell(c, Vec2::new(wx, wz), tile_m, area);
        }

        // One water material per cell — it carries that cell's shore field, so it
        // cannot be shared the way the flat-colour group materials are. Built once
        // per cell, not per mesh, and dropped with the cell's entities.
        //
        // Only while water shading is *on*. With it off the extension was still
        // bound and still ran: `update_water_waves` zeroes steepness and foam, but
        // `waves_at` has no early-out, so every water fragment paid the full
        // five-octave loop — two `sin`, a `cos` and two `pow` with a non-constant
        // exponent per octave — and the result was multiplied by zero at the end.
        // Water is a large share of screen area over a Dutch city, and this is the
        // most expensive per-pixel shader in the tree.
        //
        // Falling through to `None` is all that is needed: the branch below already
        // hands any group without a bespoke material to the shared flat-colour one,
        // and `SurfaceGroup::Water` has a palette entry, roughness and depth bias
        // there like every other group. `palette::WATER` is documented as matched
        // to the shaded tone precisely so the two can meet without a step.
        let water_material = (wants.shore
            && surface_meshes
                .iter()
                .any(|m| m.group == SurfaceGroup::Water))
        .then(|| {
            let field = shore.map(|f| mats.images.add(water::shore_field_image(&f)));
            mats.water
                .add(sea_state.material(field, Vec2::new(wx, wz), tile_m))
        });

        let mut entities = Vec::with_capacity(surface_meshes.len());
        for sm in surface_meshes {
            let mesh = meshes.add(build_mesh(&sm, wx, wz, &terrain));
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
                crate::components::HideInSpace,
            );
            let id = if let (SurfaceGroup::Water, Some(water)) = (sm.group, &water_material) {
                // Water gets the wave material (`systems::water`), which owns its
                // albedo, roughness and depth bias — and this cell's shore field.
                commands
                    .spawn((placement, MeshMaterial3d(water.clone())))
                    .id()
            } else if matches!(
                sm.group,
                SurfaceGroup::Road(_) | SurfaceGroup::RoadCasing(_)
            ) {
                // Roads and casings carry their place in the depth ladder per
                // vertex (`systems::road_depth`), which is what settles the
                // coplanar fight at a junction at *every* range. Everything else
                // about the material is the standard road recipe.
                let material = surfaces
                    .road_materials
                    .entry(sm.group)
                    .or_insert_with(|| {
                        let base = road_base_material(sm.group, &mut mats.images);
                        mats.road.add(RoadMaterial {
                            base,
                            extension: RoadDepthExtension {},
                        })
                    })
                    .clone();
                commands.spawn((placement, MeshMaterial3d(material))).id()
            } else {
                let material = surfaces
                    .materials
                    .entry(sm.group)
                    .or_insert_with(|| {
                        let [r, g, b] = sm.group.base_color();
                        mats.plain.add(StandardMaterial {
                            base_color: Color::srgb(r, g, b),
                            perceptual_roughness: sm.group.perceptual_roughness(),
                            // What actually resolves the coplanar ground stack —
                            // see `SurfaceGroup::depth_bias`. Physical separation
                            // is only millimetres and cannot survive the far plane.
                            depth_bias: sm.group.depth_bias(),
                            ..default()
                        })
                    })
                    .clone();
                commands.spawn((placement, MeshMaterial3d(material))).id()
            };
            if matches!(sm.group, SurfaceGroup::Canopy | SurfaceGroup::Trunk) {
                // Read the *current* ramp at spawn, the way new globe tiles read
                // the current albedo: a cell streaming in above the fade band must
                // not flash a full forest for the frame before the ramp catches up.
                commands
                    .entity(id)
                    .insert((VegetationSurface, veg_lod.visibility()));
            }
            if sm.group == SurfaceGroup::Furniture {
                // Same contract, its own (much lower) band.
                commands
                    .entity(id)
                    .insert((FurnitureSurface, furniture_lod.visibility()));
            }
            entities.push(id);
        }
        tracing::debug!(cell = ?c, groups = entities.len(), "surface cell built");
        surfaces.loaded.insert(c, entities);
        if !decks.is_empty() {
            surfaces.pending_decks.insert(
                c,
                PendingDecks {
                    decks,
                    world: Vec2::new(wx, wz),
                    tile_m,
                },
            );
        }
    }
}

/// Place each cell's bridge decks once their heights are known.
///
/// The deck geometry came off the worker with the rest of the cell, but *how high*
/// it stands is not in the tile — it is a measurement, and it arrives from
/// `systems::height` a few frames later. So decks wait here rather than being built
/// flat and corrected: a viaduct that visibly jumped six metres off the ground a
/// second after the cell appeared would be worse than one that arrived late.
///
/// The height is `median(DSM over the deck) − median(DTM around it)`. The DTM is
/// the right ground reference precisely *because* AHN cuts bridges out of it: what
/// is left in the padded box is the terrain the structure stands over. When the
/// layer is off, out of coverage, or has given up, the deck settles flat — which is
/// where every road already is, so the fallback is the old picture with the draw
/// order finally right.
pub fn resolve_bridge_decks(
    mut commands: Commands,
    mut surfaces: ResMut<MapSurfaces>,
    mut height: ResMut<HeightLayer>,
    mut meshes: ResMut<Assets<Mesh>>,
    mut materials: ResMut<Assets<StandardMaterial>>,
    bigspace: Query<Entity, With<BigSpace>>,
    dl: Res<crate::systems::data_layers::DataLayers>,
    terrain: Res<TerrainField>,
    mut plates: ResMut<DeckPlates>,
) {
    if surfaces.pending_decks.is_empty() {
        return;
    }
    let Ok(root) = bigspace.single() else {
        return;
    };

    let cells: Vec<Cell> = surfaces.pending_decks.keys().copied().collect();
    for c in cells {
        let Some(pending) = surfaces.pending_decks.get(&c) else {
            continue;
        };
        // A cell retired while its decks were still waiting takes them with it.
        if !surfaces.loaded.contains_key(&c) {
            surfaces.pending_decks.remove(&c);
            continue;
        }

        let tile_m = pending.tile_m;
        let mut heights = Vec::with_capacity(pending.decks.len());
        let mut settled = true;
        for deck in &pending.decks {
            match deck_height(&mut height, deck, c, tile_m, dl.height_enabled) {
                Some(h) => heights.push(h),
                None => {
                    settled = false;
                    break;
                }
            }
        }
        if !settled {
            continue;
        }

        let Some(pending) = surfaces.pending_decks.remove(&c) else {
            continue;
        };
        let mesh = build_deck_mesh(&pending.decks, &heights);
        if mesh.indices.is_empty() {
            continue;
        }
        let cell_world = pending.world;
        // Publish before the mesh is spawned rather than after, so the two cannot
        // disagree about a cell: this is the same `heights` the vertices were built
        // from, and a deck that reached the screen but not the resource is a route
        // line threaded through a viaduct.
        plates.set_cell(
            c,
            pending
                .decks
                .into_iter()
                .zip(&heights)
                .filter(|(_, &h)| h > 0.0)
                .map(|(deck, &height)| Plate {
                    deck,
                    world: cell_world,
                    height,
                })
                .collect(),
        );
        let world = Vec3::new(cell_world.x, 0.0, cell_world.y);
        let (grid_cell, local) = Grid::new(WORLD_CELL_EDGE, WORLD_SWITCH_THRESHOLD)
            .translation_to_grid(world.as_dvec3());
        let material = surfaces
            .materials
            .entry(SurfaceGroup::BridgeDeck)
            .or_insert_with(|| {
                let [r, g, b] = SurfaceGroup::BridgeDeck.base_color();
                materials.add(StandardMaterial {
                    base_color: Color::srgb(r, g, b),
                    perceptual_roughness: SurfaceGroup::BridgeDeck.perceptual_roughness(),
                    depth_bias: SurfaceGroup::BridgeDeck.depth_bias(),
                    // The fascia is a single band of quads with no inside to
                    // speak of, and a ring's winding out of MVT is not something
                    // to bet a whole structure's visibility on.
                    double_sided: true,
                    cull_mode: None,
                    ..default()
                })
            })
            .clone();
        let id = commands
            .spawn((
                Mesh3d(meshes.add(build_mesh(&mesh, cell_world.x, cell_world.y, &terrain))),
                MeshMaterial3d(material),
                Transform::from_translation(local),
                grid_cell,
                ChildOf(root),
                MapSurface,
                crate::components::HideInSpace,
            ))
            .id();
        if let Some(entities) = surfaces.loaded.get_mut(&c) {
            entities.push(id);
        }
        let tallest = heights.iter().copied().fold(0.0f32, f32::max);
        tracing::debug!(cell = ?c, decks = heights.len(), tallest, "bridge decks placed");
    }
}

/// How high one deck stands above the ground it crosses, in metres.
///
/// `None` while the height layer is still working; `Some(0.0)` is the settled
/// answer for "no data here" as much as for a bridge that really is at street
/// level — a Dutch canal bridge is exactly that, and telling the two apart would
/// buy nothing, because both want a flat deck.
fn deck_height(
    layer: &mut HeightLayer,
    deck: &BridgeDeck,
    cell: Cell,
    tile_m: f32,
    enabled: bool,
) -> Option<f32> {
    let to_rd = |p: [f32; 2]| -> [f64; 2] {
        let (lat, lng) = tile_frac_to_lat_lng(
            cell.0 as f64 + f64::from(p[0]) / f64::from(tile_m),
            cell.1 as f64 + f64::from(p[1]) / f64::from(tile_m),
        );
        wgs84_to_rd(lat, lng)
    };
    // The deck's own outline in RD, which is what says whether a height sample is
    // on the structure or beside it.
    let rings: Vec<Vec<[f64; 2]>> = deck
        .rings
        .iter()
        .map(|r| r.iter().map(|p| to_rd(*p)).collect())
        .collect();
    let corners = [
        to_rd(deck.min),
        to_rd([deck.min[0], deck.max[1]]),
        to_rd([deck.max[0], deck.min[1]]),
        to_rd(deck.max),
    ];
    let min = [
        corners.iter().map(|c| c[0]).fold(f64::MAX, f64::min),
        corners.iter().map(|c| c[1]).fold(f64::MAX, f64::min),
    ];
    let max = [
        corners.iter().map(|c| c[0]).fold(f64::MIN, f64::max),
        corners.iter().map(|c| c[1]).fold(f64::MIN, f64::max),
    ];
    let (lat, lng) = tile_frac_to_lat_lng(
        cell.0 as f64 + f64::from(deck.min[0] + deck.max[0]) * 0.5 / f64::from(tile_m),
        cell.1 as f64 + f64::from(deck.min[1] + deck.max[1]) * 0.5 / f64::from(tile_m),
    );
    if !enabled || !HeightLayer::covers(lat, lng) {
        return Some(0.0);
    }

    let grid = (deck.span_m() / DECK_SAMPLE_M).ceil() as u32;
    let grid = grid.clamp(DECK_GRID_MIN, DECK_GRID_MAX);
    let surface = HeightRequest {
        model: HeightModel::Surface,
        min,
        max,
        grid,
    };
    let terrain = HeightRequest {
        model: HeightModel::Terrain,
        min,
        max,
        grid,
    }
    .padded(GROUND_RING_M);

    let deck_top = layer
        .patch(surface)
        .and_then(|p| p.median_where(|rd| inside_rings(&rings, rd)));
    // No predicate on the terrain side: AHN's DTM has the structure cut out of it
    // already, so everything left in the padded box *is* the ground around it.
    let ground = layer.patch(terrain).and_then(|p| p.median());

    match (deck_top, ground) {
        (Some(top), Some(ground)) => {
            let clearance = (top - ground).clamp(0.0, MAX_DECK_M);
            Some(if clearance < MIN_LIFT_M {
                0.0
            } else {
                clearance
            })
        }
        _ if layer.settled(&surface) && layer.settled(&terrain) => Some(0.0),
        _ => None,
    }
}

/// Even-odd point-in-polygon over a deck's RD rings.
fn inside_rings(rings: &[Vec<[f64; 2]>], p: [f64; 2]) -> bool {
    let mut inside = false;
    for ring in rings {
        for e in ring.windows(2) {
            let (a, b) = (e[0], e[1]);
            if (a[1] > p[1]) != (b[1] > p[1]) {
                let t = (p[1] - a[1]) / (b[1] - a[1]);
                if p[0] < a[0] + t * (b[0] - a[0]) {
                    inside = !inside;
                }
            }
        }
    }
    inside
}

/// Web Mercator, fractional tile coordinates → lat/lng. The inverse of
/// [`lat_lng_to_tile_frac`], for turning a tile-local metre offset back into a
/// place on the planet — which is what the height layer is addressed by.
fn tile_frac_to_lat_lng(fx: f64, fy: f64) -> (f64, f64) {
    let n = 2.0_f64.powi(GEOMETRY_ZOOM as i32);
    let lng = fx / n * 360.0 - 180.0;
    let lat = ((1.0 - 2.0 * fy / n) * std::f64::consts::PI)
        .sinh()
        .atan()
        .to_degrees();
    (lat, lng)
}

/// Merge a cell's decks into one mesh: each deck's surface at its own height, plus
/// a fascia band extruded down from its outline.
///
/// The fascia is what makes a deck a *structure*. Without it a raised deck is a
/// sheet with no thickness, and from any angle but straight down it reads as a
/// texture floating in mid-air rather than as something a road runs over.
fn build_deck_mesh(decks: &[BridgeDeck], heights: &[f32]) -> SurfaceMesh {
    let mut positions: Vec<[f32; 3]> = Vec::new();
    let mut normals: Vec<[f32; 3]> = Vec::new();
    let mut indices: Vec<u32> = Vec::new();

    for (deck, &h) in decks.iter().zip(heights) {
        // Surface.
        let base = positions.len() as u32;
        for p in &deck.positions {
            positions.push([p[0], h, p[2]]);
            normals.push([0.0, 1.0, 0.0]);
        }
        indices.extend(deck.indices.iter().map(|i| base + i));

        if h < FASCIA_MIN_M {
            continue;
        }
        let bottom = h - DECK_THICKNESS_M;
        for ring in &deck.rings {
            // The ring's own centroid orients each face outward — cheaper and more
            // robust than trusting the winding MVT happens to hand us.
            let n = ring.len().max(1) as f32;
            let cx = ring.iter().map(|p| p[0]).sum::<f32>() / n;
            let cz = ring.iter().map(|p| p[1]).sum::<f32>() / n;
            for e in ring.windows(2) {
                let (a, b) = (e[0], e[1]);
                let (dx, dz) = (b[0] - a[0], b[1] - a[1]);
                let len = dx.hypot(dz);
                if len < 1e-3 {
                    continue;
                }
                let mut nx = -dz / len;
                let mut nz = dx / len;
                let mid = [(a[0] + b[0]) * 0.5 - cx, (a[1] + b[1]) * 0.5 - cz];
                if nx * mid[0] + nz * mid[1] < 0.0 {
                    nx = -nx;
                    nz = -nz;
                }
                let base = positions.len() as u32;
                for (p, y) in [(a, h), (b, h), (b, bottom), (a, bottom)] {
                    positions.push([p[0], y, p[1]]);
                    normals.push([nx, 0.0, nz]);
                }
                indices.extend([base, base + 1, base + 2, base, base + 2, base + 3]);
            }
        }
    }

    SurfaceMesh {
        group: SurfaceGroup::BridgeDeck,
        positions,
        indices,
        uvs: Vec::new(),
        normals,
        colors: Vec::new(),
        rungs: Vec::new(),
    }
}

/// The `StandardMaterial` half of a road or casing material.
///
/// Everything a road's surface is made of *except* its place in the depth ladder,
/// which is per vertex and belongs to `RoadDepthExtension`. `depth_bias` here is
/// only the **foot** of the road band — every road group returns the same value
/// from `depth_bias()` now, and `road_depth.wgsl` adds the rung on top.
fn road_base_material(group: SurfaceGroup, images: &mut Assets<Image>) -> StandardMaterial {
    let [r, g, b] = group.base_color();
    StandardMaterial {
        base_color: Color::srgb(r, g, b),
        // Roads sample their tarmac and markings from a generated surface; a
        // casing is the flat kerb band either side of it.
        base_color_texture: match group {
            SurfaceGroup::Road(class) => Some(images.add(road_surface_image(class))),
            _ => None,
        },
        perceptual_roughness: group.perceptual_roughness(),
        depth_bias: group.depth_bias(),
        ..default()
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
    let (data, mip_count) = crate::systems::mip::build_mip_chain(t.width, t.height, t.rgba);
    crate::systems::tile_loader::image_from_mip_chain(
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
    mut materials: ResMut<Assets<RoadMaterial>>,
    mut images: ResMut<Assets<Image>>,
) {
    let want_textured = dl.roads_textured;
    for (group, handle) in &surfaces.road_materials {
        let SurfaceGroup::Road(class) = *group else {
            continue;
        };
        let has_texture = materials
            .get(handle)
            .is_some_and(|m| m.base.base_color_texture.is_some());
        if has_texture == want_textured {
            continue;
        }
        let Some(mut material) = materials.get_mut(handle) else {
            continue;
        };
        if want_textured {
            material.base.base_color_texture = Some(images.add(road_surface_image(class)));
            material.base.base_color = Color::WHITE;
        } else {
            material.base.base_color_texture = None;
            let [r, g, b] = class.flat_color();
            material.base.base_color = Color::srgb(r, g, b);
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
fn build_mesh(sm: &SurfaceMesh, wx: f32, wz: f32, terrain: &TerrainField) -> Mesh {
    use bevy::asset::RenderAssetUsages;
    use bevy::render::mesh::{Indices, PrimitiveTopology};

    let mut positions = sm.positions.clone();
    let mut indices = sm.indices.clone();
    // Ground is flat *until the terrain field says otherwise*, so the common case is
    // a straight-up normal per vertex; only the vegetation meshes have volume and
    // bring their own.
    let mut normals: Vec<[f32; 3]> = if sm.normals.len() == sm.positions.len() {
        sm.normals.clone()
    } else {
        Vec::new()
    };
    let mut uvs: Vec<[f32; 2]> = if sm.uvs.len() == sm.positions.len() {
        sm.uvs.clone()
    } else {
        Vec::new()
    };
    let mut colors: Vec<[f32; 4]> = if sm.colors.len() == sm.positions.len() {
        sm.colors.clone()
    } else {
        Vec::new()
    };
    // Road groups only — and it must survive the terrain refinement below, which
    // appends midpoint vertices and would otherwise leave the attribute shorter
    // than the positions it belongs to (a mesh bevy rejects at draw time).
    let mut rungs: Vec<f32> = if sm.rungs.len() == sm.positions.len() {
        sm.rungs.clone()
    } else {
        Vec::new()
    };

    if terrain.is_ready() {
        stand_on_terrain(
            sm.group,
            &mut positions,
            &mut indices,
            &mut uvs,
            &mut normals,
            &mut colors,
            &mut rungs,
            Vec2::new(wx, wz),
            terrain,
        );
    }

    if normals.is_empty() {
        normals = vec![[0.0f32, 1.0, 0.0]; positions.len()];
    }
    if uvs.is_empty() {
        // The flat-colour groups have no texture; the planar projection is what
        // the shore field and the water shader read as metres from the anchor.
        uvs = positions.iter().map(|p| [wx + p[0], wz + p[2]]).collect();
    }
    // `RENDER_WORLD` without `MAIN_WORLD` — the surface cells are uploaded and only
    // ever drawn, never read back on the CPU (the shore field, the collider index
    // and the routing graph all come from the tile data, not from this mesh), so
    // the main-world copy `default()` would keep alive for the lifetime of the
    // handle is pure duplication. Same reasoning as `tile_loader::TILE_TEXTURE_USAGE`
    // and, since the cells are spawned in the frame their mesh is added, the
    // auto-`Aabb` pass still sees the geometry before extraction takes it.
    let mut mesh = Mesh::new(
        PrimitiveTopology::TriangleList,
        RenderAssetUsages::RENDER_WORLD,
    )
    .with_inserted_attribute(Mesh::ATTRIBUTE_POSITION, positions.clone())
    .with_inserted_attribute(Mesh::ATTRIBUTE_NORMAL, normals)
    .with_inserted_attribute(Mesh::ATTRIBUTE_UV_0, uvs)
    .with_inserted_indices(Indices::U32(indices));
    // `StandardMaterial` multiplies `base_color` by this whenever the attribute is
    // present — which is how one canopy material gives thousands of trees their own
    // tone and brightness without a material or a draw call per tree.
    if colors.len() == positions.len() {
        mesh.insert_attribute(Mesh::ATTRIBUTE_COLOR, colors);
    }
    // The road ladder (`systems::road_depth`). Present on road and casing meshes
    // and on nothing else, which is exactly the set that draws with
    // `RoadMaterial`.
    //
    // The two **must** agree: `RoadDepthExtension::specialize` asks the layout for
    // this attribute, and a road mesh without it fails to build a pipeline — which
    // bevy answers by skipping the pass, i.e. by drawing no roads at all, with
    // healthy cell counts and nothing in the log but `systems::pipeline_health`.
    // So the group decides, not the buffer: a road that somehow arrived without
    // rungs is put flat on rung 0, which loses the ordering within the cell and
    // keeps the street.
    if matches!(
        sm.group,
        SurfaceGroup::Road(_) | SurfaceGroup::RoadCasing(_)
    ) {
        debug_assert_eq!(
            rungs.len(),
            positions.len(),
            "{:?} lost its depth rungs between tessellation and upload",
            sm.group,
        );
        if rungs.len() != positions.len() {
            rungs = vec![0.0; positions.len()];
        }
        mesh.insert_attribute(crate::systems::road_depth::ATTRIBUTE_ROAD_RUNG, rungs);
    }
    mesh
}

/// Lift one surface group's geometry onto the measured ground.
///
/// The group decides *how*, and the three answers are different in kind:
///
/// * **Water is level.** A canal follows the valley it sits in, not the slope —
///   displacing its vertices individually would tip a lake on its side. One height
///   for the whole body ([`TerrainField::level_over`]), which is also what keeps
///   the wave shader's flat-plane assumption true.
/// * **A bridge deck already stands at a measured clearance** over the ground it
///   crosses, so it takes the terrain *underneath* it and keeps its own height on
///   top. Displacing it like a road would sink the viaduct back into the yard.
/// * **Everything else follows the ground per vertex** — after refinement, because
///   earcut's two-triangle answer for a field has no vertices in the middle to
///   follow anything with.
///
/// Ground surfaces get a fresh normal from the field's own slope; the vegetation
/// groups keep theirs (a tree's normals describe the tree, not the hill).
#[allow(clippy::too_many_arguments)]
fn stand_on_terrain(
    group: SurfaceGroup,
    positions: &mut Vec<[f32; 3]>,
    indices: &mut Vec<u32>,
    uvs: &mut Vec<[f32; 2]>,
    normals: &mut Vec<[f32; 3]>,
    colors: &mut Vec<[f32; 4]>,
    rungs: &mut Vec<f32>,
    origin: Vec2,
    terrain: &TerrainField,
) {
    let volumetric = matches!(
        group,
        SurfaceGroup::Canopy | SurfaceGroup::Trunk | SurfaceGroup::Furniture
    );
    // Both of these want *one* height for the whole thing, for opposite reasons: a
    // water body is level, and a viaduct is straight — the ground under it dipping
    // into a valley is precisely what it does not do.
    if matches!(group, SurfaceGroup::Water | SurfaceGroup::BridgeDeck) {
        let level =
            terrain.level_over(positions.iter().map(|p| [origin.x + p[0], origin.y + p[2]]));
        for p in positions.iter_mut() {
            p[1] += level;
        }
        return;
    }
    if !volumetric {
        crate::systems::terrain::refine_for_terrain(
            positions,
            indices,
            uvs,
            normals,
            colors,
            rungs,
            crate::systems::terrain::MAX_GROUND_EDGE_M,
        );
    }
    for p in positions.iter_mut() {
        p[1] += terrain.at(origin.x + p[0], origin.y + p[2]);
    }
    if !volumetric {
        *normals = positions
            .iter()
            .map(|p| {
                terrain
                    .normal_at(origin.x + p[0], origin.y + p[2])
                    .to_array()
            })
            .collect();
    }
}

/// Seed for a cell's tree scatter. Stable for the cell's absolute tile coordinate,
/// so streaming out and back regrows the identical forest rather than reshuffling
/// it — a reshuffle shows up as a shimmer whenever the ring crosses a boundary.
fn veg_seed(cell: Cell) -> u64 {
    (cell.0 as u64).wrapping_mul(0x9E37_79B9_7F4A_7C15) ^ (cell.1 as u64).rotate_left(32)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A square deck, `span` metres on a side, with its south-west corner at the
    /// tile-local origin. Only the outline matters here — `lift_at` never touches
    /// the triangles.
    fn square_deck(span: f32) -> BridgeDeck {
        BridgeDeck {
            positions: Vec::new(),
            indices: Vec::new(),
            rings: vec![vec![
                [0.0, 0.0],
                [span, 0.0],
                [span, span],
                [0.0, span],
                [0.0, 0.0],
            ]],
            min: [0.0, 0.0],
            max: [span, span],
        }
    }

    /// The route line asks one question of a deck — "how high are you here" — and
    /// the answer has to be in the world frame, because that is the only frame the
    /// route knows. A deck's own rings are tile-local, so the cell origin is part of
    /// the lookup and getting it wrong puts the viaduct a tile away.
    #[test]
    fn a_deck_lifts_only_the_ground_it_stands_over() {
        let mut plates = DeckPlates::default();
        plates.set_cell(
            (7, 9),
            vec![Plate {
                deck: square_deck(40.0),
                world: Vec2::new(1000.0, 2000.0),
                height: 6.0,
            }],
        );

        assert_eq!(
            plates.lift_at(Vec2::new(1020.0, 2020.0)),
            6.0,
            "on the deck"
        );
        assert_eq!(
            plates.lift_at(Vec2::new(1050.0, 2020.0)),
            0.0,
            "past its end"
        );
        assert_eq!(
            plates.lift_at(Vec2::new(20.0, 20.0)),
            0.0,
            "the tile-local coordinate must not answer — that is the cell origin \
             being dropped, and it reads as a viaduct a kilometre away"
        );
    }

    /// Stacked decks at an interchange: whatever is drawn on them belongs on the top
    /// one. Taking the first hit instead would make the answer depend on `HashMap`
    /// iteration order, i.e. it would differ between runs of the same scene.
    #[test]
    fn the_tallest_deck_wins_where_two_overlap() {
        let mut plates = DeckPlates::default();
        for (cell, height) in [((0, 0), 5.0), ((1, 0), 11.0), ((2, 0), 8.0)] {
            plates.set_cell(
                cell,
                vec![Plate {
                    deck: square_deck(40.0),
                    world: Vec2::ZERO,
                    height,
                }],
            );
        }
        assert_eq!(plates.lift_at(Vec2::new(20.0, 20.0)), 11.0);
    }

    /// A deck must die with its cell, and every change must be visible to a consumer
    /// that caches. The route mesh is built once and keyed on this revision — a deck
    /// that arrives without bumping it is a route line drawn under a viaduct that is
    /// already on screen, which is the whole bug this resource exists to fix.
    #[test]
    fn a_retired_cell_takes_its_decks_and_moves_the_revision() {
        let mut plates = DeckPlates::default();
        let start = plates.revision;

        plates.set_cell(
            (3, 4),
            vec![Plate {
                deck: square_deck(40.0),
                world: Vec2::ZERO,
                height: 6.0,
            }],
        );
        assert_ne!(plates.revision, start, "an arriving deck is a change");
        let placed = plates.revision;

        plates.remove_cell(&(3, 4));
        assert_eq!(plates.lift_at(Vec2::new(20.0, 20.0)), 0.0);
        assert_ne!(plates.revision, placed, "a retired deck is a change too");

        // …but a cell that never had one is not a change. Every surface cell is
        // offered to `set_cell`, and most cities are mostly not bridges: bumping on
        // those would redraw the route line a few times a second forever.
        let quiet = plates.revision;
        plates.set_cell((5, 5), Vec::new());
        assert_eq!(plates.revision, quiet);
    }

    /// The first pass must be a *build*, not a rebuild.
    ///
    /// `detail_wants` starts `None` rather than at some assumed default precisely
    /// so the comparison in `update_map_surfaces` cannot match on frame one. If it
    /// started at a concrete value, a tier whose real wants differed would clear a
    /// ring it had not built yet — harmless, but it would make the retear fire on
    /// every cold start and hide a real retear in the noise.
    #[test]
    fn a_fresh_surface_store_has_not_committed_to_any_detail_level() {
        assert_eq!(MapSurfaces::default().detail_wants, None);
    }

    /// The retear is driven by equality on the whole struct, so *every* field has
    /// to participate — a flag that stopped being compared would silently become a
    /// dead toggle, which is the exact failure this mechanism exists to prevent.
    #[test]
    fn each_detail_flag_is_visible_to_the_retear_comparison() {
        let all = DetailWants::ALL;
        for (label, one_off) in [
            (
                "trees",
                DetailWants {
                    trees: false,
                    ..all
                },
            ),
            (
                "furniture",
                DetailWants {
                    furniture: false,
                    ..all
                },
            ),
            (
                "shore",
                DetailWants {
                    shore: false,
                    ..all
                },
            ),
        ] {
            assert_ne!(
                Some(all),
                Some(one_off),
                "flipping `{label}` alone must be visible to the retear check, or \
                 turning that layer on leaves the live cells without it"
            );
        }
    }

    /// The three toggles these flags mirror must start where the tier says.
    ///
    /// This is the join that makes the gating work at all: `DetailWants` is read
    /// from the `VegetationLod` / `FurnitureLod` / `WaterSeaState` mirrors, those
    /// mirror `DataLayers`, and `DataLayers` starts from `Budget::detail_layers`.
    /// If that last link ever broke, a phone would go back to scattering 6,000
    /// canopies and rasterising a 1024² distance field per cell before the user
    /// touched anything — and the only symptom would be the framerate.
    #[test]
    fn the_detail_toggles_start_where_the_tier_puts_them() {
        let dl = crate::systems::data_layers::DataLayers::default();
        let tier = crate::quality::budget().detail_layers;
        assert_eq!(dl.trees_enabled, tier, "Trees must start at the tier");
        assert_eq!(
            dl.furniture_enabled, tier,
            "Furniture must start at the tier"
        );
        assert_eq!(
            dl.water_shading, tier,
            "Water shading must start at the tier"
        );
        assert_eq!(
            dl.street_detail_enabled, tier,
            "Street detail must start at the tier"
        );
    }
}
