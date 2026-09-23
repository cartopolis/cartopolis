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
use crate::systems::furniture::FurnitureMask;
use crate::systems::globe::GlobeState;
use crate::systems::grass::GrassAreas;
use crate::systems::ground_detail::{GroundDetailExtension, GroundMaterial};
use crate::systems::height::{wgs84_to_rd, HeightLayer, HeightModel, HeightRequest};
use crate::systems::map_stream::{lat_lng_to_tile_frac, MapStream};
use crate::systems::moorings;
use crate::systems::municipal;
use crate::systems::parking;
use crate::systems::places;
use crate::systems::road_depth::{RoadDepthExtension, RoadMaterial};
use crate::systems::road_texture::{road_texture, RoadClass};
use crate::systems::scatter;
use crate::systems::street_furniture::FurnitureLod;
use crate::systems::street_labels::StreetLabelStore;
use crate::systems::terrain::{MeasuredGround, TerrainField, TerrainSnapshot};
use crate::systems::tile_loader::load_tile_image;
use crate::systems::tunnels::CellTunnels;
use crate::systems::turf::GrassArea;
use crate::systems::vector_tiles::ShoreField;
use crate::systems::vector_tiles::{self, BridgeDeck, StreetLabelLine, SurfaceGroup, SurfaceMesh};
use crate::systems::vegetation::VegetationLod;
use crate::systems::water::{self, WaterMaterial, WaterSeaState};
use crate::systems::world_places::{CellPlaces, WorldPlaces, CROSSING_MAST_RADIUS};
use cartopolis_core::world::tile_to_meters;
use cartopolis_core::TileKey;
use cartopolis_core::{tile_source_kind, TileSourceKind};

/// Cells are streamed at the vector source's own zoom — no overzoom, because
/// geometry needs no LOD pyramid: the same mesh is correct at any distance. This
/// matches `osm_buildings::CACHE_ZOOM` and `lod22::LOD22_ZOOM`, so all three
/// streamers agree on what a "cell" is.
const GEOMETRY_ZOOM: u32 = vector_tiles::MAX_SOURCE_ZOOM;

/// Whether the flat vector ground is drawn as meshes at all.
///
/// `CARTO_NO_GROUND_MESHES=1` drops them — the land fills, the roads, the paving,
/// the water — and leaves the basemap image under the camera to stand for them,
/// keeping everything else a cell produces: the extracted data (labels, places,
/// collision, drive lines), trees, street furniture, tunnels and bridges. The
/// comparison step 5 of `level-of-detail-tile-tree.md` asked for, kept so the
/// image-only ground can still be seen at any altitude.
#[must_use]
pub fn ground_meshes_enabled() -> bool {
    static ON: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *ON.get_or_init(|| {
        !matches!(
            std::env::var("CARTO_NO_GROUND_MESHES").as_deref(),
            Ok("1") | Ok("true")
        )
    })
}

/// A group that lies flat on the ground, which the basemap image already shows.
fn is_flat_ground(group: SurfaceGroup) -> bool {
    !matches!(
        group,
        SurfaceGroup::Canopy
            | SurfaceGroup::Trunk
            | SurfaceGroup::Furniture
            | SurfaceGroup::TunnelRoad(_)
            | SurfaceGroup::TunnelStructure
            | SurfaceGroup::BridgeDeck
    )
}

/// Stage stopwatch for one cell job: which of a dozen passes a cell's second
/// and a half actually goes on, logged once per cell as `surface cell stages`.
struct Lap {
    last: bevy::platform::time::Instant,
    split: String,
}

impl Lap {
    fn new() -> Self {
        Self {
            last: bevy::platform::time::Instant::now(),
            split: String::new(),
        }
    }

    fn mark(&mut self, stage: &str) {
        use std::fmt::Write as _;
        let now = bevy::platform::time::Instant::now();
        let _ = write!(
            self.split,
            "{stage}:{} ",
            now.duration_since(self.last).as_millis()
        );
        self.last = now;
    }
}

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

/// How much bigger a ground surface gets when it is refined against the measured
/// terrain, as a divisor on the byte cap handed to the tessellators.
///
/// The cap is enforced where a mesh is *built*, and `stand_on_terrain` subdivides
/// it afterwards, so the cap has to leave room for that. Measured on `deck-1`,
/// 2026-09-22, ground level over Groningen: a surface cut to the 8 MB cap came
/// out of refinement at **11.3 MB** — 261,673 vertices where the cap allowed
/// about 165,000. That is 1.41x on the bytes, and the worst piece is what the
/// bound exists to stop, so **2** rather than 1.5: the failure modes are not
/// symmetric, and a cap that is too generous is the one that freezes the frame.
///
/// It is a divisor and not a second constant so the relationship cannot drift —
/// `lod22::max_cell_mesh_bytes` remains the one place the ceiling is decided, and
/// this says only "and leave room for the refinement".
///
/// Zero effect where the height layer is off: nothing refines, so the pieces are
/// simply smaller than they need to be, which costs draws and not correctness. A
/// tighter version would ask `TerrainField::is_ready()` — it cannot, because this
/// is resolved on the frame that *spawns* the worker and the field that will
/// refine the result may not have landed yet.
const TERRAIN_REFINE_EXPANSION: usize = 2;

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
    // The carriageways riding those decks, `(deck index, mesh)`, at `y = 0` and
    // already cut out of the slab they stand on. They are handed over separately
    // for the same reason the decks are: neither can be placed until the height
    // arrives.
    Vec<(u32, SurfaceMesh)>,
    // The ground stretch of each bridge-flagged street either side of its span,
    // which is what the height field grades an embankment along. Travels with the
    // decks because it is indexed by them and waits on the same measurement — see
    // `Plate::approaches`.
    Vec<cartopolis_geo::vector_tiles::Approach>,
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
    // The cell's tunnels and the holes they need in the ground over their entrances
    // (`cartopolis_geo::tunnels`), which are places rather than triangles and so
    // do not travel with the meshes above.
    CellTunnels,
    // The road sheet's triangles in tile-local XZ, taken before the meshes above
    // were refined against the terrain, for `terrain::RoadMask`.
    Vec<[[f32; 2]; 3]>,
    // Whether the meshes above were already stood on the measured ground by the
    // worker — see `spawn_geometry_task`'s `ground`.
    bool,
    // The terrain revision the cell was built against (`MapSurfaces::built_rev`).
    u32,
    // The deck heights atlas measured in this cell, if it did
    // (`terrain_tiles::load_atlas_decks`).
    Option<Vec<[f32; 3]>>,
);

/// The ground a cell job builds on: the field in place now, or — while atlas's
/// graded ground is on its way (`TerrainField::holds_builds`) — whatever lands,
/// awaited after the job's fetches rather than before them.
enum GroundJob {
    Now(Option<crate::systems::terrain::GroundPlacement>, u32),
    Later(futures_channel::oneshot::Receiver<crate::systems::terrain::GroundArrival>),
}

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
    /// The drivable network (`systems::traffic`). Here for the reason all of
    /// these are: keyed by cell, filled from the same parse, and retired on
    /// exactly the same events.
    roads: ResMut<'w, crate::systems::traffic::DriveNetwork>,
    /// The operator's street-object models, for the same reason again: what it
    /// holds per cell is a queue of objects to stand up, which has to be dropped
    /// with the cell it belongs to (`systems::placeholder_models`).
    placeholders: ResMut<'w, crate::systems::placeholder_models::PlaceholderModels>,
    /// The tunnels under the cell (`systems::tunnels`), for the same reason as
    /// all of these: built from this parse, keyed by this cell, and retired with
    /// it — a tunnel whose mesh has gone must not still be a floor to stand on.
    tunnels: ResMut<'w, crate::systems::tunnels::Tunnels>,
    /// Where the roads are, on the height field's own grid
    /// (`systems::terrain::RoadMask`). Not keyed by cell and not retired with one:
    /// it is a single grid in the anchor's frame that every cell marks its own part
    /// of, and it is cleared by the re-anchor rather than by the ring.
    road_mask: ResMut<'w, crate::systems::terrain::RoadMask>,
}

/// Everything a surface cell needs to *make a material out of*, in one parameter
/// for the same reason [`CellStores`] is one: `update_map_surfaces` sits on Bevy's
/// 16-parameter ceiling, and the road ladder added a fourth asset collection to
/// the three it already held.
///
/// There are three material types because there are three shaders. The flat
/// ground groups carry the sub-metre slope (`systems::ground_detail`); water owns
/// its wave shader; roads carry the depth ladder in a vertex attribute
/// (`systems::road_depth`) and so need a pipeline that declares it.
#[derive(bevy::ecs::system::SystemParam)]
pub struct SurfaceMaterials<'w> {
    plain: ResMut<'w, Assets<GroundMaterial>>,
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
/// Only decks that were actually *lifted* are kept ([`cartopolis_geo::terrain_grade::MIN_LIFT_M`]). A flat deck is
/// a piece of road surface and there is nothing to climb onto.
#[derive(Resource, Default)]
pub struct DeckPlates {
    plates: HashMap<Cell, Vec<Plate>>,
    /// What each cell's decks looked like last time, so a cell that is
    /// re-tessellated and publishes the same decks does not move the revision —
    /// see [`set_cell`](Self::set_cell).
    digests: HashMap<Cell, u64>,
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
    /// The ground stretches of this deck's own street either side of its span,
    /// tile-local, abutment first — `vector_tiles::Approach`.
    ///
    /// Carried here because the *terrain* is what has to act on them and the
    /// terrain cannot ask the tile: the approach and the street passing
    /// underneath are both at grade and both unflagged, so nothing about their
    /// geometry separates them. What separates them is that the approach is the
    /// same street feature as the span, which `split_by_deck` establishes on the
    /// worker and which is lost the moment the cell's meshes are merged.
    approaches: Vec<Vec<[f32; 2]>>,
}

impl DeckPlates {
    /// Install (or replace) one cell's lifted decks.
    ///
    /// **The revision moves only when the decks do, and that is load-bearing now
    /// that the height field grades against it.** Every terrain bake bumps
    /// `TerrainField::revision`, every streamer standing on the ground answers
    /// that by re-tessellating its whole ring, and every rebuilt cell with a
    /// viaduct in it arrives back here. Bumping unconditionally therefore closes a
    /// loop — bake, retear, re-publish, bake — that never settles and retears the
    /// city about once a second for the rest of the session.
    ///
    /// A retear produces the *same* decks: a plate's height is the survey's DSM
    /// over the structure less its DTM around it (`deck_height`), which does not
    /// depend on the field being graded, and the approaches are the tessellation's
    /// own output. So comparing is enough, and comparing a digest rather than the
    /// meshes keeps it to a few dozen words per cell.
    ///
    /// `RoadMask::mark_cell` avoids the same loop a different way — it marks each
    /// cell once per anchor generation and ignores it thereafter — which does not
    /// suit here, because a deck legitimately *changes* when its measurement lands.
    fn set_cell(&mut self, cell: Cell, plates: Vec<Plate>) {
        if plates.is_empty() && !self.plates.contains_key(&cell) {
            return;
        }
        let digest = Self::digest(&plates);
        let unchanged = self.digests.get(&cell) == Some(&digest);
        self.plates.insert(cell, plates);
        self.digests.insert(cell, digest);
        if !unchanged {
            self.revision = self.revision.wrapping_add(1);
        }
    }

    /// Everything the *height field* grades against, over every streamed cell, as
    /// one number.
    ///
    /// **A counter cannot answer this and a digest can.** The terrain bake waits
    /// for the decks to stop moving, and every bake retears every cell — so each
    /// rebuilt cell passes through `remove_cell` and back through `set_cell`, and
    /// a monotonic `revision` moves twice per cell per bake whatever the decks
    /// actually did. Measured over Groningen on 2026-09-22 that was **fifteen
    /// bakes** in one fill, the last eight over identical input, each one a retear
    /// of every streamer standing on the ground.
    ///
    /// Summed rather than folded in order, because the cells are a `HashMap` and
    /// iterate in whatever order they please: addition commutes, so the same set
    /// of decks is the same number however it was arrived at, and a cell that
    /// leaves and comes back unchanged leaves it where it started.
    #[must_use]
    pub fn grade_digest(&self) -> u64 {
        self.digests.values().fold(0u64, |a, d| a.wrapping_add(*d))
    }

    /// Everything about a cell's decks that any consumer reads, as one number.
    ///
    /// The outlines are compared by their bounding box rather than vertex by
    /// vertex: a deck whose *shape* changed without its extent changing would have
    /// to be a different tile of the same cell, which is a re-anchor, and that
    /// clears the whole resource anyway.
    fn digest(plates: &[Plate]) -> u64 {
        let mut h: u64 = 0xcbf2_9ce4_8422_2325;
        let mut eat = |v: u32| {
            h ^= u64::from(v);
            h = h.wrapping_mul(0x100_0000_01b3);
        };
        for p in plates {
            eat(p.height.to_bits());
            eat(p.world.x.to_bits());
            eat(p.world.y.to_bits());
            for v in p.deck.min.iter().chain(p.deck.max.iter()) {
                eat(v.to_bits());
            }
            for run in &p.approaches {
                eat(run.len() as u32);
                if let (Some(a), Some(b)) = (run.first(), run.last()) {
                    for v in a.iter().chain(b.iter()) {
                        eat(v.to_bits());
                    }
                }
            }
        }
        h
    }

    /// Drop one cell's decks (cell retired).
    pub fn remove_cell(&mut self, cell: &Cell) {
        self.digests.remove(cell);
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

    /// The two grids the height field needs to build an embankment: where the
    /// approach roads run, and how high the structure they climb to stands.
    ///
    /// Both are `n × n` on the terrain field's own grid, which is centred on the
    /// map anchor and spans `±half` metres — the same frame `RoadMask::mark_cell`
    /// marks in. `lift[i]` is the deck height at a sample the structure covers and
    /// `f32::NAN` everywhere else; `ramp[i]` is an approach sample.
    ///
    /// **Rasterised here rather than on the bake's worker** because it is per
    /// *plate*, not per sample: each one scans its own bounding box, so a city's
    /// forty-odd viaducts cost a few tens of thousands of point-in-outline tests
    /// against the four million samples a full-grid pass would be.
    ///
    /// **And the approaches are a mask of their own, not part of the road mask.**
    /// Ramping through the whole road corridor raised 18 % of the Groningen
    /// fixture's streets for five bridges: a raster mask is one connected shape
    /// and a gradient travels through everything touching it, including the street
    /// passing *under* the viaduct. The approach is separable only because it is
    /// the same street feature as the span (`vector_tiles::Approach`).
    #[must_use]
    pub fn grade_masks(&self, n: usize, half: f32) -> (Vec<bool>, Vec<f32>) {
        let plates: Vec<cartopolis_geo::terrain_grade::DeckPlate<'_>> = self
            .plates
            .values()
            .flatten()
            .map(|p| cartopolis_geo::terrain_grade::DeckPlate {
                deck: &p.deck,
                world: p.world.to_array(),
                height: p.height,
                approaches: &p.approaches,
            })
            .collect();
        #[allow(clippy::cast_precision_loss)]
        let step = half * 2.0 / (n.max(2) - 1) as f32;
        cartopolis_geo::terrain_grade::deck_masks(
            &plates,
            cartopolis_geo::terrain_grade::Lattice {
                n,
                origin: [-half, -half],
                step,
            },
        )
    }
}

/// Deck slab thickness in metres: the fascia band extruded down from the surface,
/// so a bridge reads as a structure with an edge and a shadow rather than as a
/// sheet of paper floating over the tracks.
const DECK_THICKNESS_M: f32 = 0.9;

/// Below this height a deck gets no fascia: a flat deck is a piece of road
/// surface, and a skirt around it would read as a kerb.
const FASCIA_MIN_M: f32 = 0.4;

/// How far outside a deck the ground level is measured. Wide enough to clear the
/// structure and its abutments, narrow enough to stay in the same terrain.
const GROUND_RING_M: f64 = cartopolis_geo::terrain_grade::GROUND_RING_M;

/// **Decks are measured in clusters, not one by one.** Every deck used to ask for
/// its own surface patch and its own ground patch, and a cell asked for them one
/// deck at a time, waiting for each answer before the next request: ~600 requests
/// over Groningen, three in flight and 0.15 s apart at best, and the map could not
/// settle until the last one landed. Measured on the Deck (2026-09-24, Grote Markt
/// at 600 m): lifting that throttle alone took a cold load from 155 s to 72 s.
/// Instead, decks within [`DECK_CLUSTER_M`] of each other share one pair of patches
/// at [`DECK_SAMPLE_M`], and every cluster of a cell is asked for at once.
const DECK_CLUSTER_M: f64 = 256.0;

/// Sample spacing of a cluster's patches, in metres. Finer than the 1.5 m a deck
/// used to be measured at when it was large, so a 10 m canal bridge still has
/// dozens of samples on its deck.
const DECK_SAMPLE_M: f64 = 1.0;

/// Bound on a cluster patch's grid: the cluster's span plus the ground ring on
/// both sides, at `DECK_SAMPLE_M`, with room to spare.
const DECK_GRID_MAX: u32 = 384;

/// Streaming state for the vector surface meshes.
#[derive(Resource)]
pub struct MapSurfaces {
    /// Live cells → their spawned mesh entities (one per surface group).
    loaded: HashMap<Cell, Vec<Entity>>,
    /// The terrain revision each live cell stands on. **A new field retires only
    /// the cells built on an older one** — the ring used to be torn down whole,
    /// which also threw away cells still waiting for the very field that landed.
    built_rev: HashMap<Cell, u32>,
    /// Requested/failed/in-flight bookkeeping, the results sink, the despawn queue
    /// and the anchor generation (`platform::stream`).
    stream: CellStream<Cell, Tessellated>,
    /// One material per surface group, created on first use.
    materials: HashMap<SurfaceGroup, Handle<GroundMaterial>>,
    /// The same, for the road groups — they carry the depth ladder in a vertex
    /// attribute, so they draw with [`RoadMaterial`] and cannot share the map
    /// above. Still one handle per group: the ordering is per *vertex* precisely
    /// so that splitting the materials was never necessary.
    road_materials: HashMap<SurfaceGroup, Handle<RoadMaterial>>,
    /// Terrain-field revision the live cells were tessellated against, so a field
    /// that lands (or goes away) after them rebuilds them onto the right ground.
    terrain_revision: u32,
    /// Surveyed-coverage revision these cells were built against — the BGT's
    /// (`surveyed::coverage_revision`) in bit 0 and the crop register's
    /// (`crops::coverage_revision`) in bit 1, each 0 while its manifest is in
    /// flight and 1 once it has answered. Surveyed trees, lampposts and crop
    /// parcels are merged into these cells' meshes, so a cell built during either
    /// gap has none of them and nothing would revisit it. Each bit changes at
    /// most once per process.
    bgt_revision: u32,
    /// Cells whose bridge decks are tessellated but not yet *placed*: their height
    /// is a measurement that arrives from `systems::height` some frames later. Held
    /// here rather than built flat and corrected, so a viaduct never visibly jumps
    /// off the ground once its answer lands.
    pending_decks: HashMap<Cell, PendingDecks>,
    /// Which optional detail the live cells were built with, so flipping a layer
    /// retears them ([`DetailWants`]). `None` until the first pass, which is what
    /// makes the very first frame a build rather than a rebuild.
    detail_wants: Option<DetailWants>,
    /// The shared ground-finish roughness map ([`ground_finish_uv`]), minted on
    /// first use and held so every group that takes it gets the same image rather
    /// than its own copy of identical texels.
    ground_finish: Option<Handle<Image>>,
    /// When this anchor began waiting for the BGT and crop coverage answers
    /// before dispatching (`COVERAGE_WAIT_S`).
    coverage_wait: Option<(u32, bevy::platform::time::Instant)>,
}

impl Default for MapSurfaces {
    fn default() -> Self {
        Self {
            loaded: HashMap::new(),
            built_rev: HashMap::new(),
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
            bgt_revision: 0,
            pending_decks: HashMap::new(),
            detail_wants: None,
            ground_finish: None,
            coverage_wait: None,
        }
    }
}

/// A cell's decks, waiting on their heights.
struct PendingDecks {
    decks: Vec<BridgeDeck>,
    /// The approach runs, by the same index as `decks`.
    approaches: Vec<cartopolis_geo::vector_tiles::Approach>,
    /// `(deck index, mesh)` — the road surfaces those decks carry, waiting on the
    /// same heights.
    roads: Vec<(u32, SurfaceMesh)>,
    /// Cell origin in world metres — the same north-west corner the cell's other
    /// meshes are placed at.
    world: Vec2,
    /// Where each deck is in RD, and which pair of patches measures it. Empty when
    /// `known` is set.
    clusters: Vec<DeckCluster>,
    boxes: Vec<DeckBox>,
    /// Every deck's height, by the same index as `decks`, when atlas measured
    /// them all (`terrain_tiles::match_atlas_decks`).
    known: Option<Vec<f32>>,
}

/// One deck's footprint in RD: its outline, and the box around it the ground
/// level is read from.
struct DeckBox {
    rings: Vec<Vec<[f64; 2]>>,
    min: [f64; 2],
    max: [f64; 2],
    /// The cluster that measures it; `None` outside AHN, where a deck is settled
    /// at street level without asking.
    cluster: Option<usize>,
}

/// Decks that share a surface patch and a ground patch.
struct DeckCluster {
    min: [f64; 2],
    max: [f64; 2],
}

impl DeckCluster {
    fn requests(&self) -> (HeightRequest, HeightRequest) {
        let (min, max) = (
            [self.min[0] - GROUND_RING_M, self.min[1] - GROUND_RING_M],
            [self.max[0] + GROUND_RING_M, self.max[1] + GROUND_RING_M],
        );
        let span = (max[0] - min[0]).max(max[1] - min[1]);
        let grid = ((span / DECK_SAMPLE_M).ceil() as u32 + 1).clamp(16, DECK_GRID_MAX);
        let req = |model| HeightRequest {
            model,
            min,
            max,
            grid,
        };
        (req(HeightModel::Surface), req(HeightModel::Terrain))
    }
}

/// Every deck of a cell in RD, and the clusters that measure them. Greedy: each
/// deck joins the first cluster it keeps within `DECK_CLUSTER_M`, else starts one.
fn cluster_decks(
    decks: &[BridgeDeck],
    cell: Cell,
    tile_m: f32,
) -> (Vec<DeckBox>, Vec<DeckCluster>) {
    let to_rd = |p: [f32; 2]| -> [f64; 2] {
        let (lat, lng) = tile_frac_to_lat_lng(
            cell.0 as f64 + f64::from(p[0]) / f64::from(tile_m),
            cell.1 as f64 + f64::from(p[1]) / f64::from(tile_m),
        );
        wgs84_to_rd(lat, lng)
    };
    let mut boxes = Vec::with_capacity(decks.len());
    let mut clusters: Vec<DeckCluster> = Vec::new();
    for deck in decks {
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
        let cluster = HeightLayer::covers(lat, lng).then(|| {
            let joined = clusters.iter().position(|c| {
                let lo = [c.min[0].min(min[0]), c.min[1].min(min[1])];
                let hi = [c.max[0].max(max[0]), c.max[1].max(max[1])];
                hi[0] - lo[0] <= DECK_CLUSTER_M && hi[1] - lo[1] <= DECK_CLUSTER_M
            });
            match joined {
                Some(i) => {
                    let c = &mut clusters[i];
                    c.min = [c.min[0].min(min[0]), c.min[1].min(min[1])];
                    c.max = [c.max[0].max(max[0]), c.max[1].max(max[1])];
                    i
                }
                None => {
                    clusters.push(DeckCluster { min, max });
                    clusters.len() - 1
                }
            }
        });
        boxes.push(DeckBox {
            rings,
            min,
            max,
            cluster,
        });
    }
    (boxes, clusters)
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
    /// Street forms this server draws with an uploaded model instead
    /// (`systems::placeholder_models`). Those are **not meshed** — a lamppost
    /// replaced by a model must not also be six boxes — and the instances are
    /// stood up as glTF scenes after the cell lands.
    ///
    /// It belongs here rather than in the spawn path for the reason the flags
    /// above do: the geometry is decided on the worker, so a change has to retear
    /// the live cells, and this struct's comparison is what does that.
    modelled: FurnitureMask,
    /// Mesh the flat ground — land fills, roads, paving, water — rather than
    /// leave it to the basemap image (`FLAT_GROUND_ON_M`). The data they carry is
    /// extracted either way.
    flat_ground: bool,
}

/// Below this camera altitude the flat ground is drawn as meshes; above
/// [`FLAT_GROUND_OFF_M`] it is left to the basemap image under it.
///
/// **Measured on the Deck (2026-09-24, step 5 of `level-of-detail-tile-tree.md`),
/// Grote Markt, warm:** at 600 m the two pictures are the same to the eye (the
/// image is marginally softer, and the water is its flat colour rather than the
/// wave shader) and dropping the meshes settles in 5.6 s against 7.3 s at 12.0 ms
/// a frame against 15.7. At walking height the meshes are the picture — the
/// carriageway, the surveyed paving, the kerbs — and the image is a soft beige
/// blur with no road in it. So near the ground, meshes; above it, the image.
const FLAT_GROUND_ON_M: f32 = 250.0;

/// Longest the ground cells wait for the BGT and crop coverage before they are
/// dispatched without them (see the dispatch loop).
const COVERAGE_WAIT_S: f32 = 3.0;
const FLAT_GROUND_OFF_M: f32 = 500.0;

impl DetailWants {
    /// Everything on — what a desktop tier asks for, and what the tessellation
    /// tests expect.
    #[cfg(test)]
    const ALL: Self = Self {
        trees: true,
        furniture: true,
        shore: true,
        grass: true,
        modelled: FurnitureMask::NONE,
        flat_ground: true,
    };
}

fn spawn_geometry_task(
    cell: Cell,
    tile_m: f32,
    template: String,
    wants: DetailWants,
    // The measured ground and this cell's north-west corner in world metres, when
    // there is a field to stand the cell on.
    //
    // **Placing a cell on the terrain used to be the main thread's job, and it was
    // the longest thing the main thread did.** Refining every ground surface against
    // the field, displacing it and recomputing its normals is pure buffer work, done
    // on arrival two cells a frame. Measured cold over Groningen on `deck-1`
    // (2026-09-23): each terrain bake retears the nine-cell ring, and the rebuilt
    // cells then froze the main loop for 3–4 s a frame, five frames in a row, twice
    // per fill. A frozen loop also dispatches nothing, which is why the worker pool
    // read 20 % busy over the same fill.
    //
    // It can happen here because the retear is what guarantees the field this job
    // was handed is still the live one: a terrain revision moving resets the stream,
    // which orphans this job's sink, so a cell placed on a superseded field is never
    // delivered.
    ground: GroundJob,
    origin: Vec2,
    results: Arc<Mutex<Vec<(Cell, Tessellated)>>>,
    live: Arc<std::sync::atomic::AtomicBool>,
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
    // **And the bound has to survive what happens to the mesh after it.** The
    // tessellators cut a cell to this many bytes, and then `stand_on_terrain`
    // subdivides every ground surface against the measured height field —
    // `refine_for_terrain`, per edge, precisely to follow a hill earcut's two
    // triangles cannot. So the cap was being applied *before* the only step that
    // multiplies the geometry, and the piece that reached the GPU was larger than
    // the piece that was measured.
    //
    // Measured on `deck-1` 2026-09-22, ground level over Groningen: a surface cut
    // to this 8 MB cap arrived as a single **11.3 MB** mesh of 261,673 vertices,
    // in a frame that uploaded 86 MB and took **3,766 ms**. Twenty-two such
    // frames froze 63 s of a 101 s fill, and a frozen main loop dispatches
    // nothing, so the worker pool drained to 0.16 utilisation behind it. The
    // bound existing and not holding is what made that invisible: `upload_meter`
    // flagged the asset every time, and the number it flagged was inside the cap
    // everybody believed was in force.
    //
    // Dividing rather than splitting afterwards, deliberately. Splitting a
    // refined mesh is the more exact answer and it is a bigger change — this
    // function returns one mesh per surface and its caller spawns one entity for
    // it — while the refinement's expansion is bounded and cheap to allow for
    // here. What it costs is more pieces, i.e. more draws, which is the trade
    // `MAX_CELL_MESH_BYTES` already documents and `ShotMetrics::draws` already
    // reports.
    let mesh_bytes = Some(crate::systems::lod22::max_cell_mesh_bytes() / TERRAIN_REFINE_EXPANSION);
    IoTaskPool::get()
        .spawn(async move {
            let key = TileKey::new(cell.0, cell.1, GEOMETRY_ZOOM);
            let fetched = load_tile_image(&key, &template).await;
            // One request that fills the cache entries the next three awaits are
            // about to look for (`systems::cell_bundle`). It changes nothing about
            // what they do — a host that serves no bundles, or a cell already
            // bundled, and this returns without asking — but where the host does
            // serve them it turns three sequential round trips into one. They are
            // sequential natively whatever the `zip`s below suggest: `http::fetch`
            // is a blocking call with no await point in it.
            crate::systems::cell_bundle::prefill(cell.0, cell.1).await;
            // The cell's surveyed BGT objects travel with the job for the reason
            // `building_facts` gives about `lod22`: they end up merged into this
            // cell's meshes, so they have to be in hand on the worker that builds
            // them rather than arriving later on a stream of their own. Empty
            // outside the coverage, which is everywhere the host has not run
            // atlas's `pipeline/bgt.rs`.
            let surveyed = crate::systems::surveyed::load_surveyed_cell(cell.0, cell.1).await;
            // …and, where the city this cell falls in publishes a tree register
            // of its own, that register — which supersedes the BGT's trees below
            // rather than joining them (`systems::municipal` says why). Empty
            // everywhere no city in `municipal::REGISTERS` reaches.
            //
            // Gated on `wants.trees` rather than filtered afterwards, unlike the
            // BGT cell above: this one reaches a *city's* server, not ours, and a
            // dense cell of it is 1.3 MB. Asking a third party for a layer that
            // is switched off is the one cost here worth not paying.
            let registered = if wants.trees || wants.furniture {
                crate::systems::municipal::load_registered_cell(cell.0, cell.1).await
            } else {
                crate::systems::municipal::RegisteredCell::default()
            };
            // …and its surveyed paving, in a file of its own because it is by far
            // the heavier half.
            let paving = crate::systems::surveyed::load_surveyed_paving(cell.0, cell.1).await;
            // …and the cell's registered crop parcels, which ride along for the
            // same reason: they are merged into this cell's meshes, and the
            // arable ones are a keep-out the grass area is built against. Empty
            // outside the coverage, which is everywhere until the BRP extractor
            // (not yet written; it belongs in atlas's `pipeline/`) has run.
            let crop_cell = crate::systems::crops::load_crop_cell(cell.0, cell.1).await;
            // …and the cell's navigation marks, on the same terms and with two
            // gates of their own (`systems::nav_marks`). The water one reads the
            // *undecoded* body: the parse happens on the worker below, and a
            // fetch decision cannot wait for it without moving a ~120 ms decode
            // onto the frame loop on wasm. `wants.furniture` is the third gate —
            // marks ride that group's mesh, so a run with it off asks for
            // nothing.
            //
            // …and the cell's registered level crossings, on exactly the same
            // terms and with the same gates (`systems::crossings`). Its second
            // one reads the undecoded body for the railway the way the marks
            // read it for water, and for the same reason.
            //
            // Both gate reads happen here, *before* either future is polled, so
            // the two can be awaited together: these are the only two awaits in
            // this task that reach a host we do not own, and running them end to
            // end means a cell's ground waits on one third party's answer twice
            // over (#267). Natively that is presentation rather than overlap —
            // `http::fetch` is a blocking `ureq` call with no await point in it
            // — and what bounds the wait there is `http::OPTIONAL_LAYER_TIMEOUT`
            // on each request plus each service's breaker.
            let has_water = fetched
                .as_ref()
                .is_ok_and(|bytes| vector_tiles::body_mentions_water(bytes));
            let has_rail = fetched
                .as_ref()
                .is_ok_and(|bytes| vector_tiles::body_mentions_rail(bytes));
            let (mark_cell, crossing_cell) = if wants.furniture {
                futures_lite::future::zip(
                    crate::systems::nav_marks::load_marks_cell(cell.0, cell.1, has_water),
                    crate::systems::crossings::load_crossings_cell(cell.0, cell.1, has_rail),
                )
                .await
            } else {
                (
                    crate::systems::nav_marks::MarkCell::default(),
                    crate::systems::crossings::CrossingCell::default(),
                )
            };
            let atlas_decks =
                crate::systems::terrain_tiles::load_atlas_decks(cell.0, cell.1).await;
            // The ground last, after every fetch: a job waiting for graded ground
            // has spent the wait downloading.
            let (ground, built_rev) = match ground {
                GroundJob::Now(g, rev) => (g, rev),
                GroundJob::Later(rx) => rx.await.unwrap_or((None, 0)),
            };
            let ground = ground.map(|g| (g, origin));
            // Torn down while it fetched or waited (`CellStream::live`): stop
            // before the meshing, which is the expensive half.
            if !live.load(std::sync::atomic::Ordering::Relaxed) {
                return;
            }
            crate::platform::workers::submit_heavy("surfaces", Box::new(move || {
                let mut lap = Lap::new();
                let mut shore = None;
                let mut grass = None;
                let mut decks = Vec::new();
                let mut deck_roads = Vec::new();
                let mut approaches = Vec::new();
                let mut cell_tunnels = Vec::new();
                let mut cell_openings = Vec::new();
                let sunken_ways: Vec<cartopolis_geo::tunnels::SunkenWay>;
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
                lap.mark("parse");
                let outcome = match parsed {
                    Ok(tile) => match vector_tiles::tessellate_surfaces(&tile, tile_m, mesh_bytes) {
                        Ok(mut meshes) => {
                            lap.mark("fills");
                            // Decks first: the road tessellation needs them, since a
                            // bridge-flagged street's span rides the deck that
                            // carries it rather than the ground plane — and it
                            // *cuts itself out* of that deck, which is why the
                            // slabs come back from `tessellate_roads` changed.
                            match vector_tiles::tessellate_bridges(&tile, tile_m) {
                                Ok(d) => decks = d,
                                Err(e) => {
                                    tracing::warn!(cell = ?cell, error = %e, "bridge tessellation failed");
                                }
                            }
                            lap.mark("bridges");
                            // Roads share the fetch, the parse, the worker and the
                            // per-cell batching with the area surfaces — they
                            // differ only in being lines, so they ride along here
                            // rather than running a second streamer over the same
                            // tiles.
                            // The surveyed paving goes down first, under the road
                            // ribbons that will be laid over it — and it is also
                            // what tells `tessellate_roads` to leave the footways,
                            // the cycle paths and the pedestrian streets alone,
                            // since this cell draws those as real shapes rather
                            // than as constant-width lines.
                            let paved = crate::systems::surveyed::surveyed_paving(
                                &paving, cell.0, cell.1, tile_m,
                            );
                            let surveyed_paving = !paved.is_empty();
                            cell_places.surveyed.3 = paved.len();
                            // Held back rather than appended here: the carriageways
                            // have to come out of it, and they do not exist yet.
                            // See `paving::cut_paving_under_roads` below.
                            let mut paving_meshes =
                                crate::systems::paving::build_paving_meshes(&paved, mesh_bytes);
                            // The registered crops go down over the generic green
                            // and under everything else on the ground stack
                            // (`SurfaceGroup::Crop`), so a field takes its own
                            // colour without disturbing a single road, canal or
                            // pavement. Held past the mesh build because the
                            // arable ones are the grass keep-out below.
                            let parcels = crate::systems::crops::crop_parcels(
                                &crop_cell, cell.0, cell.1, tile_m,
                            );
                            cell_places.surveyed.4 = parcels.len();
                            // Held back past the cut below for the reason
                            // `the-ground-stack.md` gives: a parcel and a
                            // carriageway do not share ground the way a verge and
                            // one do, so `Crop` is the one ground layer the road
                            // cut leaves alone. Appended after it, untouched.
                            let crop_meshes = cartopolis_geo::crops::build_crop_meshes(
                                &parcels, mesh_bytes,
                            );
                            lap.mark("paving");
                            match vector_tiles::tessellate_roads(
                                &tile,
                                tile_m,
                                &mut decks,
                                mesh_bytes,
                                surveyed_paving,
                            ) {
                                Ok(roads) => {
                                    // **The carriageways come out of the paving
                                    // before either is drawn.** The two sources
                                    // disagree by centimetres about where a kerb
                                    // is, and the ground stack held that overlap
                                    // apart with 1.0 mm of height — which works
                                    // until the ground is the measured one and
                                    // each sheet is bent onto it against its own
                                    // triangles. Removing the overlap is the only
                                    // thing that has ever held here.
                                    crate::systems::paving::cut_paving_under_roads(
                                        &mut paving_meshes,
                                        &roads.ground,
                                    );
                                    // **And the landuse fills come out from under
                                    // them too.** Measured on the Groningen z14
                                    // fixture: 67 % of the road sheet's area has
                                    // built land drawn under it, 7 % grass and 2 %
                                    // woodland — three quarters of every
                                    // carriageway in the city was a second opaque
                                    // coplanar surface, held apart by half a
                                    // millimetre of `GroundLayer` clearance. That
                                    // is the largest overlap left anywhere in the
                                    // stack and it is the one in the phone
                                    // captures: the pale wedges standing through
                                    // the tarmac are `Built`, bent onto the
                                    // measured ground against its own triangles
                                    // rather than the road's.
                                    cartopolis_geo::road_stitch::cut_under_roads(
                                        &mut meshes,
                                        &roads.ground,
                                    );
                                    meshes.extend(std::mem::take(&mut paving_meshes));
                                    meshes.extend(roads.ground);
                                    // The stretches on a deck cannot be placed
                                    // yet — they are at y = 0 and the deck's
                                    // height is measured a few frames from now —
                                    // so they travel beside the decks and are
                                    // lifted with them in `resolve_bridge_decks`.
                                    deck_roads = roads.on_deck;
                                    // **The one thing that tells a road climbing
                                    // onto a structure from the road going under
                                    // it**, and it was discarded here. See
                                    // `Plate::approaches`.
                                    approaches = roads.approaches;
                                }
                                Err(e) => {
                                    tracing::warn!(cell = ?cell, error = %e, "road tessellation failed");
                                }
                            }
                            lap.mark("roads");
                            // A cell whose roads failed still has its pavements,
                            // uncut because there is nothing to cut them against.
                            // `take` above makes this the empty vector on the
                            // ordinary path rather than a second copy.
                            meshes.extend(std::mem::take(&mut paving_meshes));
                            // And the crop parcels rejoin here, on both paths and
                            // uncut either way — they are held back only so the
                            // road cut above does not see them, never to be
                            // dropped. Position in this vector means nothing: a
                            // sheet's place in the ground stack is its group's
                            // rung, not its order here.
                            meshes.extend(crop_meshes);
                            // The roads that are *not* on the road sheet, because
                            // they run under it (`cartopolis_geo::tunnels`). Built
                            // here rather than inside `tessellate_roads` for the
                            // reason the module note gives: a tunnel is a deck, two
                            // walls and a roof at its own depth, and it owes the
                            // ground a hole rather than a place in the coplanar
                            // stack. The corridors it hands back travel with the
                            // cell so the ground can be opened at the entrance and
                            // the avatar can find the floor down there.
                            let tunnels = cartopolis_geo::tunnels::build_tunnels(
                                &tile, tile_m, mesh_bytes,
                            );
                            // The ground over an open approach has to go, or the
                            // entrance is roofed by the sheet the tunnel was cut into
                            // and there is no way in. Cut here, on this worker,
                            // while the cell's ground meshes are still buffers —
                            // the alternative is a hole punched in the fragment
                            // shader, which would leave the sheet standing as
                            // something to walk on and to stop the camera.
                            cartopolis_geo::tunnels::cut_ground(
                                &mut meshes,
                                &tunnels.openings,
                            );
                            cell_tunnels = tunnels.tunnels;
                            // Kept for the drive network below, which needs to
                            // know how deep each *street* runs. It cannot ask the
                            // finished tunnels, because those are corridors and a
                            // car belongs to a feature — see
                            // `cartopolis_geo::tunnels::SunkenWay`.
                            sunken_ways = tunnels.sunken;
                            cell_openings = tunnels.openings;
                            meshes.extend(tunnels.meshes);
                            lap.mark("tunnels");
                            // Trees ride the same fetch and the same per-cell
                            // batching (`systems::vegetation`). Scattering and
                            // merging thousands of canopies is pure CPU work, so
                            // it belongs on this worker and not on the frame that
                            // uploads the result.
                            if wants.trees {
                                match scatter::scatter_vegetation(&tile, tile_m, veg_seed(cell)) {
                                    Ok(mut trees) => {
                                        // The BGT's individual street trees join
                                        // the scattered canopies rather than
                                        // replacing them — see
                                        // `systems::surveyed` for why that is not
                                        // a compromise: the scatter fills park and
                                        // wood polygons, these stand on the
                                        // pavement, and the registration is
                                        // optional content whose coverage varies
                                        // by municipality.
                                        // A city's own register, where there is
                                        // one, is read *instead of* the BGT's
                                        // trees — the two describe the same
                                        // trees and the city's is the fuller
                                        // count by an order of magnitude, so
                                        // planting both puts a second lime a
                                        // metre from the first.
                                        let city = municipal::registered_trees(
                                            &registered,
                                            cell.0,
                                            cell.1,
                                            tile_m,
                                            veg_seed(cell),
                                        );
                                        cell_places.surveyed.7 = city.len();
                                        // **A registered tree is a point, and a
                                        // register is not a map of the roads.**
                                        // The scatter keeps out of every drawn
                                        // street (`scatter::RoadMask`); the
                                        // registers' trees were only
                                        // de-duplicated, so wherever a surveyed
                                        // trunk lands inside a drawn carriageway
                                        // — the OSM ribbon's width is a class
                                        // convention, not the kerb — a tree grew
                                        // out of the road (reported 2026-09-23).
                                        // By the trunk alone: a street tree's
                                        // crown overhangs the road on purpose.
                                        let mut roads: Option<scatter::RoadMask> = None;
                                        let mut off_road = |list: &mut Vec<scatter::TreeInstance>| {
                                            if list.is_empty() {
                                                return;
                                            }
                                            let mask = roads.get_or_insert_with(|| {
                                                scatter::RoadMask::build(&tile, tile_m)
                                            });
                                            list.retain(|t| !mask.blocks(t.x, t.z, 0.0));
                                        };
                                        let mut city = city;
                                        off_road(&mut city);
                                        if !municipal::supersedes(
                                            cell.0,
                                            cell.1,
                                            municipal::Supply::Trees,
                                        ) {
                                            let planted =
                                                crate::systems::surveyed::surveyed_trees(
                                                    &surveyed,
                                                    cell.0,
                                                    cell.1,
                                                    tile_m,
                                                    veg_seed(cell),
                                                );
                                            cell_places.surveyed.0 = planted.len();
                                            let mut planted = planted;
                                            off_road(&mut planted);
                                            trees.extend(planted);
                                        }
                                        trees.extend(city);
                                        // A canopy standing in an open cut is a
                                        // tree growing out of a carriageway seven
                                        // metres below it. Dropped whole rather
                                        // than cut, for the reason `cut_ground`
                                        // leaves the volumetric groups alone: a
                                        // third of a tree is not a smaller tree.
                                        if !cell_openings.is_empty() {
                                            // By the crown, not the trunk: a
                                            // tree on the lip of a trench
                                            // otherwise hangs over the road in it.
                                            trees.retain(|t| {
                                                !cartopolis_geo::tunnels::near_an_opening(
                                                    &cell_openings,
                                                    [t.x, t.z],
                                                    t.radius * 0.8,
                                                )
                                            });
                                        }
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
                            lap.mark("trees");
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
                                    Ok(mut items) => {
                                        // …and the surveyed street objects join
                                        // the ones read out of `pois`. Mostly
                                        // lighting: `pois` has no street lighting
                                        // in it at all, and a lamppost is the most
                                        // numerous thing on a street by a wide
                                        // margin.
                                        let mut placed =
                                            crate::systems::surveyed::surveyed_props(
                                                &surveyed,
                                                &tile,
                                                cell.0,
                                                cell.1,
                                                tile_m,
                                                veg_seed(cell),
                                            );
                                        // A city that registers its own lamp
                                        // posts is the authority on them here, so
                                        // the BGT's copies are dropped — but only
                                        // those. The city says nothing about
                                        // benches, and suppressing the whole cell
                                        // would delete them to fix the lighting.
                                        let by_city = municipal::supplied_props(cell.0, cell.1);
                                        if !by_city.is_empty() {
                                            placed.retain(|p| !by_city.contains(&p.kind));
                                            // …and the same for the ones read out
                                            // of the tile's own `pois`. A bottle
                                            // bank is in OpenStreetMap *and* in
                                            // the city's register, so filtering
                                            // only the national register would
                                            // still draw every container twice.
                                            items.retain(|p| !by_city.contains(&p.kind));
                                        }
                                        cell_places.surveyed.1 = placed.len();
                                        items.extend(placed);
                                        let city_props = municipal::registered_props(
                                            &registered,
                                            &tile,
                                            cell.0,
                                            cell.1,
                                            tile_m,
                                            veg_seed(cell),
                                        );
                                        cell_places.surveyed.8 = city_props.len();
                                        items.extend(city_props);
                                        // …and the surveyed boundaries, which are
                                        // lines rather than point props and so get
                                        // their own builder — one box per surveyed
                                        // segment, in the same group and material
                                        // as everything above.
                                        let mut lines = crate::systems::surveyed::surveyed_lines(
                                            &surveyed,
                                            cell.0,
                                            cell.1,
                                            tile_m,
                                        );
                                        // A sunken road's retaining and portal
                                        // walls are the tunnel's to draw; stood up
                                        // from the ground by their measured height
                                        // they filled its mouths
                                        // (`tunnels::wall_of_a_sunken_road`).
                                        if !cell_tunnels.is_empty() {
                                            let before = lines.len();
                                            lines.retain(|l| {
                                                l.kind != furniture::LineKind::Wall
                                                    || !cartopolis_geo::tunnels::wall_of_a_sunken_road(
                                                        &cell_tunnels,
                                                        &cell_openings,
                                                        &l.points,
                                                    )
                                            });
                                            if lines.len() < before {
                                                tracing::info!(
                                                    cell = ?cell,
                                                    dropped = before - lines.len(),
                                                    "surveyed walls of a sunken road left to the tunnel"
                                                );
                                            }
                                        }
                                        cell_places.surveyed.2 = lines.len();
                                        meshes.extend(furniture::build_line_meshes(
                                            &lines, mesh_bytes,
                                        ));
                                        // A bench or a lamppost standing in the
                                        // open cut has the same problem a tree
                                        // there does, and the same answer: the
                                        // instance goes, and it goes before the
                                        // list becomes colliders and prompts.
                                        if !cell_openings.is_empty() {
                                            items.retain(|p| {
                                                !cartopolis_geo::tunnels::in_an_opening(
                                                    &cell_openings,
                                                    [p.x, p.z],
                                                )
                                            });
                                        }
                                        // A form this server replaces with a model
                                        // is not meshed at all — it is stood up as
                                        // a glTF scene once the cell lands
                                        // (`systems::placeholder_models`). Skipped
                                        // at the source rather than hidden later,
                                        // so an overridden lamppost costs no
                                        // vertices, no upload and no draw.
                                        meshes.extend(furniture::build_furniture_meshes_except(
                                            &items, wants.modelled, mesh_bytes,
                                        ));
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
                                // The `sites` layer's car parks, filled — same
                                // fetch, same parse, same group and material as
                                // the benches above, and the same toggle, because
                                // collision has to match what is drawn
                                // (`systems::parking`).
                                match parking::extract_parking(&tile, tile_m, veg_seed(cell)) {
                                    Ok(items) => {
                                        // …and a replaced car is replaced in the
                                        // car parks too: a placeholder is about a
                                        // form, not about which extractor placed it.
                                        meshes.extend(furniture::build_furniture_meshes_except(
                                            &items, wants.modelled, mesh_bytes,
                                        ));
                                        cell_places.vehicles = items;
                                    }
                                    Err(e) => {
                                        tracing::warn!(cell = ?cell, error = %e, "parked vehicle fill failed");
                                    }
                                }
                                // …and the boats along the canal banks, off the
                                // water polygons this same parse already
                                // tessellated (`systems::moorings`). Same group,
                                // same material, same toggle.
                                match moorings::extract_moorings(&tile, tile_m, veg_seed(cell)) {
                                    Ok(items) => {
                                        meshes.extend(furniture::build_furniture_meshes_except(
                                            &items, wants.modelled, mesh_bytes,
                                        ));
                                        // Boats are not solid — a collider on
                                        // water the avatar cannot walk on is an
                                        // invisible wall in a canal — so they go
                                        // in beside the parked vehicles, whose
                                        // radius table already answers zero here.
                                        cell_places.vehicles.extend(items);
                                    }
                                    Err(e) => {
                                        tracing::warn!(cell = ?cell, error = %e, "mooring fill failed");
                                    }
                                }
                                // …and the surveyed buoys and beacons standing on
                                // the water the boats above are inferred from
                                // (`systems::nav_marks`). Same group, same
                                // material, same toggle — and **no collider**:
                                // they are not `FurnitureInstance`s, so nothing
                                // reaches `collision::sync_prop_colliders`, which
                                // is the answer a boat gets by the other route.
                                let marks = crate::systems::nav_marks::cell_marks(
                                    &mark_cell, cell.0, cell.1, tile_m,
                                );
                                cell_places.surveyed.5 = marks.len();
                                meshes.extend(cartopolis_geo::nav_marks::build_mark_meshes(
                                    &marks, mesh_bytes,
                                ));
                                // …and the installations on ProRail's registered
                                // level crossings (`systems::crossings`). Same
                                // group, same material, same toggle — but the
                                // masts are **solid**, unlike the marks above.
                                // A buoy stands on water the avatar cannot walk
                                // on; a crossing mast stands on a verge half a
                                // metre outside the kerb of a street it walks
                                // along, which is the lamp column's case, so it
                                // gets the lamp column's circle.
                                //
                                // Placed here rather than in the fetch above
                                // because the orientation is read off *this*
                                // tile's `streets` layer, which only exists
                                // after the parse — a boom lies across the road,
                                // and the register carries no azimuth at all.
                                let crossings = cartopolis_geo::crossings::place_crossings(
                                    &tile,
                                    tile_m,
                                    &crate::systems::crossings::cell_crossings(
                                        &crossing_cell,
                                        cell.0,
                                        cell.1,
                                        tile_m,
                                    ),
                                );
                                cell_places.surveyed.6 = crossings.len();
                                // The two masts of each, as bare circles: the
                                // positions come off the same derivation the
                                // boxes are meshed from, so the collider cannot
                                // drift off the post
                                // (`crossings::mast_centres`). Nothing else on
                                // an installation is solid — the raised bar
                                // starts a metre off the ground and these are
                                // height-free circles.
                                cell_places.colliders.extend(crossings.iter().flat_map(|c| {
                                    cartopolis_geo::crossings::mast_centres(c)
                                        .map(|[x, z]| (Vec2::new(x, z), CROSSING_MAST_RADIUS))
                                }));
                                meshes.extend(cartopolis_geo::crossings::build_crossing_meshes(
                                    &crossings,
                                    mesh_bytes,
                                ));
                            }
                            lap.mark("furniture");
                            // …and so do the two layers this tile was decoding
                            // and dropping outright (`docs/notes/vector-tile-unused-layers.md`).
                            match places::extract_pois(&tile, tile_m) {
                                Ok(p) => cell_places.pois = p,
                                Err(e) => {
                                    tracing::warn!(cell = ?cell, error = %e, "poi extraction failed");
                                }
                            }
                            // The drivable network for ambient traffic, off the
                            // same `streets` layer the ribbons and the routing
                            // graph are built from (`systems::traffic`).
                            match cartopolis_geo::traffic::extract_drive_lines(
                                &tile,
                                tile_m,
                                &sunken_ways,
                            ) {
                                Ok(lines) => cell_places.drive_lines = lines,
                                Err(e) => {
                                    tracing::warn!(cell = ?cell, error = %e, "drive line extraction failed");
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
                            lap.mark("extract");
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
                            lap.mark("shore");
                            // Where a blade may stand. Rides this fetch and this
                            // parse like everything above it — the `land` layer
                            // is already decoded here, and the two keep-out masks
                            // are the same ones the tree scatter builds.
                            if wants.grass {
                                // The parcels go in as the third keep-out: OSM
                                // draws farmland as grass, so without the
                                // register a ploughed field grows a lawn.
                                let mut area = cartopolis_geo::turf::GrassArea::build(
                                    &tile, tile_m, &parcels,
                                );
                                // …and the tunnel openings as the fourth. The
                                // blades are generated from this cell's grass
                                // polygons rather than placed on the ground mesh,
                                // so the hole cut in that mesh does not reach
                                // them: without this a park whose tile says grass
                                // across the block grows a lawn over the trench.
                                area.block_mouths(&cell_openings);
                                if !area.is_empty() {
                                    grass = Some(area);
                                }
                            }
                            lap.mark("grass");
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
                let mut outcome = outcome;
                // Before the refinement below multiplies them.
                let road_tris = outcome
                    .as_deref()
                    .map(crate::systems::terrain::road_triangles)
                    .unwrap_or_default();
                // Above `FLAT_GROUND_OFF_M` the basemap image stands for the flat
                // ground; everything else — the data, the volumes — as it was.
                // Dropped before the drape, which is most of their cost.
                if !wants.flat_ground {
                    if let Some(meshes) = outcome.as_mut() {
                        meshes.retain(|m| !is_flat_ground(m.group));
                    }
                }
                let mut placed = false;
                if let (Some(meshes), Some((ground, origin))) = (outcome.as_mut(), ground.as_ref()) {
                    // Only this cell's own tunnels, where the main thread would have
                    // seen whichever neighbours had happened to land first. A tile
                    // carries a buffer past its edge, so a bore near the boundary is
                    // in both cells' features.
                    let mut tunnels = crate::systems::tunnels::Tunnels::default();
                    tunnels.insert_cell(
                        cell,
                        *origin,
                        CellTunnels {
                            tunnels: cell_tunnels.clone(),
                            openings: cell_openings.clone(),
                        },
                    );
                    for sm in meshes.iter_mut() {
                        stand_on_terrain(
                            sm.group,
                            &mut sm.positions,
                            &mut sm.indices,
                            &mut sm.uvs,
                            &mut sm.normals,
                            &mut sm.colors,
                            &mut sm.rungs,
                            *origin,
                            ground,
                            &tunnels,
                        );
                    }
                    placed = true;
                }
                lap.mark("ground");
                let split = GROUND_SPLIT.with(|g| g.replace([0; 4]));
                tracing::debug!(
                    cell = ?cell,
                    placed,
                    stages = %lap.split,
                    refine_ms = split[0] / 1000,
                    displace_ms = split[1] / 1000,
                    normals_ms = split[2] / 1000,
                    verts = split[3],
                    "surface cell stages"
                );
                if let Ok(mut g) = results.lock() {
                    g.push((
                        cell,
                        (
                            outcome,
                            shore,
                            decks,
                            deck_roads,
                            approaches,
                            labels,
                            cell_places,
                            grass,
                            CellTunnels {
                                tunnels: cell_tunnels,
                                openings: cell_openings,
                            },
                            road_tris,
                            placed,
                            built_rev,
                            atlas_decks,
                        ),
                    ));
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
    pub fn material(&self, group: SurfaceGroup) -> Option<&Handle<GroundMaterial>> {
        self.materials.get(&group)
    }

    /// The shared ground-finish map, minting it on first use.
    pub(crate) fn ground_finish(&mut self, images: &mut Assets<Image>) -> Handle<Image> {
        self.ground_finish
            .get_or_insert_with(|| images.add(ground_finish_image()))
            .clone()
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
        materials: &mut Assets<GroundMaterial>,
    ) -> Handle<GroundMaterial> {
        debug_assert!(
            !matches!(
                group,
                SurfaceGroup::Road(_) | SurfaceGroup::RoadCasing(_) | SurfaceGroup::TunnelRoad(_)
            ),
            "road materials carry a texture and are created by update_map_surfaces"
        );
        self.materials
            .entry(group)
            .or_insert_with(|| {
                let [r, g, b] = group.base_color();
                materials.add(GroundMaterial {
                    base: StandardMaterial {
                        base_color: Color::srgb(r, g, b),
                        perceptual_roughness: group.perceptual_roughness(),
                        depth_bias: group.depth_bias(),
                        ..default()
                    },
                    extension: ground_detail_for(group),
                })
            })
            .clone()
    }

    /// Every plain ground material this run has created, with the group that owns
    /// it. `systems::wet_surfaces` walks these to ramp the ground between a dry
    /// and a wet look; it needs the group as well as the handle because only the
    /// hard surfaces take the full treatment.
    pub(crate) fn ground_materials(
        &self,
    ) -> impl Iterator<Item = (SurfaceGroup, &Handle<GroundMaterial>)> {
        self.materials
            .iter()
            .map(|(group, handle)| (*group, handle))
    }

    /// The same, for the road groups — they carry `RoadDepthExtension`, so their
    /// handles live in a different asset store with the `StandardMaterial` nested
    /// inside (see `systems::road_depth`).
    pub(crate) fn road_materials(
        &self,
    ) -> impl Iterator<Item = (SurfaceGroup, &Handle<RoadMaterial>)> {
        self.road_materials
            .iter()
            .map(|(group, handle)| (*group, handle))
    }

    /// Whether this cell is still live — asked by anything that spawns into a
    /// cell some frames after the cell was built (`systems::placeholder_models`).
    #[must_use]
    pub fn holds(&self, cell: Cell) -> bool {
        self.loaded.contains_key(&cell)
    }

    /// Hang one more entity off a live cell, so it is retired with it.
    ///
    /// The alternative — a second store keyed by cell, retired alongside — is
    /// what every "an object outlived its cell" bug in this file has been.
    pub fn attach(&mut self, cell: Cell, entity: Entity) {
        if let Some(entities) = self.loaded.get_mut(&cell) {
            entities.push(entity);
        }
    }

    /// Retire every live cell (used on a re-anchor, or when the source stops being
    /// a vector one). Entities go through the despawn queue, not straight out.
    fn clear(&mut self) {
        for (_, entities) in self.loaded.drain() {
            self.stream.retire_all(entities);
        }
        self.built_rev.clear();
        self.pending_decks.clear();
        // Drops the requested/failed bookkeeping and orphans the results sink, so
        // in-flight tasks cannot deliver a cell placed in the old origin's frame.
        self.stream.reset();
    }
}

/// Retire one live cell and everything that lives and dies with it; a cell still
/// in the ring is asked for again.
fn retire_cell(surfaces: &mut MapSurfaces, stores: &mut CellStores, c: Cell) {
    if let Some(entities) = surfaces.loaded.remove(&c) {
        surfaces.stream.retire_all(entities);
    }
    surfaces.built_rev.remove(&c);
    surfaces.pending_decks.remove(&c);
    surfaces.stream.forget(&c);
    stores.labels.remove_cell(&c);
    stores.places.remove_cell(&c);
    stores.decks.remove_cell(&c);
    stores.grass.remove_cell(&c);
    stores.roads.remove_cell(&c);
    stores.tunnels.drop_cell(c);
    stores.placeholders.forget_cell(c);
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
            stores.roads.clear();
            stores.tunnels.clear();
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
        stores.roads.clear();
        stores.tunnels.clear();
        stores.placeholders.clear_queue();
    }
    // So does the ground moving under them. The measured field lands a second or
    // two after the anchor it belongs to, by which time these cells have already
    // been built flat; they are re-tessellated once, against the cached MVT bodies
    // the first pass fetched. Correcting the meshes in place instead would mean
    // holding every cell's CPU-side geometry for the life of the cell, to save a
    // rebuild that happens once per anchor.
    // The BGT coverage answer lands the same way and takes the same retear: it
    // decides whether a cell gets its surveyed street trees and lighting, and
    // those are merged into the cell's own meshes. Unlike the terrain it can only
    // change once, so this costs one rebuild of whatever was standing when the
    // manifest arrived.
    // The crop coverage is folded in beside it — same shape, same one-shot answer,
    // and the parcels are baked into the same meshes — so the two share a rebuild
    // rather than costing one each. Two bits of one revision, as `lod22` already
    // does with the BAG and the LoD3 manifests.
    let bgt_revision = crate::systems::surveyed::coverage_revision()
        | (crate::systems::crops::coverage_revision() << 1);
    // **A new field alone retires only the cells built on an older one** (step 2c
    // of `level-of-detail-tile-tree.md`). The ring used to go whole, which also threw
    // away the in-flight cells waiting for this very field and built them twice.
    // A cell arriving later on an old field is dropped at arrival for the same
    // reason (`built_rev`).
    if surfaces.bgt_revision == bgt_revision && surfaces.terrain_revision != terrain.revision {
        let old: Vec<Cell> = surfaces
            .built_rev
            .iter()
            .filter(|(_, rev)| **rev != terrain.revision)
            .map(|(c, _)| *c)
            .collect();
        tracing::info!(
            cells = old.len(),
            of = surfaces.loaded.len(),
            terrain_rev = terrain.revision,
            was_terrain_rev = surfaces.terrain_revision,
            "surface cells retear"
        );
        surfaces.terrain_revision = terrain.revision;
        for c in old {
            retire_cell(&mut surfaces, &mut stores, c);
        }
    }
    if surfaces.terrain_revision != terrain.revision || surfaces.bgt_revision != bgt_revision {
        // **What a retear costs, and why it is worth a line in the log.** Each one
        // re-tessellates the whole ring, and a cell job is the most expensive thing
        // this client runs on a worker — measured cold over Groningen on 2026-09-21,
        // 3.6 s per cell and 54.7% of all worker time in the fill. Two very
        // different worlds look identical from outside: a ring rebuilt once because
        // the ground arrived, and a ring rebuilt four times because four separate
        // facts arrived one after another. This says which.
        tracing::info!(
            cells = surfaces.loaded.len(),
            terrain_rev = terrain.revision,
            was_terrain_rev = surfaces.terrain_revision,
            bgt_rev = bgt_revision,
            was_bgt_rev = surfaces.bgt_revision,
            "surface ring retear"
        );
        surfaces.terrain_revision = terrain.revision;
        surfaces.bgt_revision = bgt_revision;
        surfaces.clear();
        stores.places.clear();
        stores.labels.clear();
        stores.decks.clear();
        stores.grass.clear();
        stores.roads.clear();
        stores.tunnels.clear();
        stores.placeholders.clear_queue();
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
    // The flat ground as meshes near the ground, as the basemap image above it
    // (`FLAT_GROUND_ON_M`), with the gap between the two thresholds so a camera
    // hovering at one does not retear the ring every few frames.
    let alt = globe.altitude_m(cell, tf) as f32;
    let flat_ground = match surfaces.detail_wants {
        Some(w) if w.flat_ground => alt < FLAT_GROUND_OFF_M,
        Some(_) => alt < FLAT_GROUND_ON_M,
        None => alt < FLAT_GROUND_OFF_M,
    };
    let wants = DetailWants {
        trees: veg_lod.enabled,
        furniture: furniture_lod.enabled,
        shore: sea_state.shading,
        grass: stores.grass.enabled,
        modelled: stores.placeholders.mask(),
        flat_ground: flat_ground && ground_meshes_enabled(),
    };
    if surfaces.detail_wants != Some(wants) {
        surfaces.detail_wants = Some(wants);
        surfaces.clear();
        stores.places.clear();
        stores.labels.clear();
        stores.decks.clear();
        stores.grass.clear();
        stores.roads.clear();
        stores.tunnels.clear();
        stores.placeholders.clear_queue();
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
        retire_cell(&mut surfaces, &mut stores, c);
    }

    // 2. Queue cells not yet fetched as detached tessellation tasks, bounded to
    // SURFACE_MAX_IN_FLIGHT concurrent. Unqueued cells are re-evaluated next frame as
    // slots free (the `requested` set already de-dups), so nothing is dropped.
    // Ranked by ring distance, so the cell the camera is standing in outranks its
    // neighbours here *and* may spend the global `fetch_gate`'s near reserve — the
    // ground under your feet is not allowed to queue behind a far building cell
    // from another streamer that happened to run first this frame.
    for c in &wanted {
        if !surfaces.loaded.contains_key(c) {
            let rank = crate::platform::fetch_gate::ring_distance(*c, ccx, ccy);
            surfaces.stream.request_ranked(*c, rank, now);
        }
    }
    // Dispatch under SURFACE_MAX_IN_FLIGHT; whatever doesn't fit stays queued and
    // starts on a later frame as slots free, so nothing is dropped.
    let sink = surfaces.stream.sink();
    let placement = terrain.placement();
    // Graded ground on its way: build the cells once, on it, rather than flat and
    // then again (`TerrainField::holds_builds`). The jobs start now and fetch; only
    // the meshing waits for the ground.
    let held = terrain.holds_builds(stream.anchor_generation);
    // **Not before the BGT and crop coverage have answered**, both baked into the
    // cell's meshes: a cell dispatched before an answer is torn down by it (the
    // retear above), and one of them arrives a second in on a warm start — the
    // Deck (2026-09-24) built every street-level cell twice that way. Bounded,
    // because a host that never answers costs a moment and not the ground.
    let gen = stream.anchor_generation;
    let since = match surfaces.coverage_wait {
        Some((g, t)) if g == gen => t,
        _ => {
            let t = bevy::platform::time::Instant::now();
            surfaces.coverage_wait = Some((gen, t));
            t
        }
    };
    let coverage_known = bgt_revision == 0b11 || since.elapsed().as_secs_f32() > COVERAGE_WAIT_S;
    while let Some(c) = coverage_known
        .then(|| surfaces.stream.next_job(now))
        .flatten()
    {
        // The same origin the arrival below computes, so the worker stands the cell
        // exactly where it will be spawned.
        let origin = Vec2::new(
            ((c.0 as f64 - fcx) * f64::from(tile_m)) as f32,
            ((c.1 as f64 - fcy) * f64::from(tile_m)) as f32,
        );
        let ground = if held {
            GroundJob::Later(terrain.ground_later())
        } else {
            GroundJob::Now(placement.clone(), terrain.revision)
        };
        tracing::debug!(cell = ?c, held, terrain_rev = terrain.revision, "surface cell dispatched");
        spawn_geometry_task(
            c,
            tile_m,
            template.clone(),
            wants,
            ground,
            origin,
            sink.clone(),
            surfaces.stream.live(),
        );
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
    for (
        c,
        (
            outcome,
            shore,
            decks,
            deck_roads,
            approaches,
            street_names,
            mut cell_places,
            grass,
            tunnels,
            road_tris,
            placed,
            built_rev,
            atlas_decks,
        ),
    ) in ready
    {
        // Dropped from the ring (or re-anchored) while the worker was busy.
        if !wanted.contains(&c) {
            surfaces.stream.forget(&c);
            continue;
        }
        // Built on a field that has since been replaced: ask again, on this one.
        if built_rev != terrain.revision {
            tracing::debug!(cell = ?c, built_rev, terrain_rev = terrain.revision, "surface cell on an old field; again");
            surfaces.stream.forget(&c);
            continue;
        }
        tracing::debug!(cell = ?c, built_rev, replaces = surfaces.loaded.contains_key(&c), "surface cell arrived");
        let Some(surface_meshes) = outcome else {
            // Not in `loaded`, and marked failed so it is not retried every frame;
            // the ring moving off it forgets the cell.
            surfaces.stream.failed(&c, now);
            continue;
        };
        surfaces.stream.succeeded(&c);
        surfaces.built_rev.insert(c, built_rev);

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
        // The roads go to their own store first — `insert_cell` consumes
        // `cell_places`, and taking the lines out of it here is what keeps
        // `WorldPlaces` from holding a copy of a network it does not use.
        stores
            .roads
            .insert_cell(c, wx, wz, std::mem::take(&mut cell_places.drive_lines));
        // **Where this cell's carriageways are, for the height field to grade
        // itself along.** A survey reads the kerbs, the parked cars and the hedge;
        // a built road has none of them, so without this a ridge stands along every
        // road for its whole length (`systems::terrain::RoadMask`). Marked from the
        // drawn ribbons rather than from centre lines, because at a junction the
        // drawn surface is the junction's actual shape.
        stores.road_mask.mark_triangles(
            stream.anchor_generation,
            crate::quality::budget()
                .terrain_grid
                .clamp(65, crate::systems::terrain::TERRAIN_GRID),
            c,
            Vec2::new(wx, wz),
            &road_tris,
        );
        // Whatever this server draws with a model of its own is queued to be stood
        // up (`systems::placeholder_models`). The worker has already left those
        // forms out of the merged furniture mesh, so this is the *only* geometry
        // they get — and it is queued rather than spawned, because a dense cell is
        // hundreds of instances and a teleport lands several cells at once.
        //
        // Both lists, because a placeholder is about a *form*: replacing the
        // parked car replaces it in the car parks (`cell_places.vehicles`) exactly
        // as replacing the bench replaces it on the pavement.
        if !stores.placeholders.is_empty() {
            let mask = stores.placeholders.mask();
            let queued: Vec<(String, Transform)> = cell_places
                .furniture
                .iter()
                .chain(cell_places.vehicles.iter())
                // …and only where the instance will take one. A fountain standing
                // in open water is a jet in a pond; an uploaded fountain model is
                // something with a base, meant for a square.
                .filter(|f| mask.contains(f.kind) && f.takes_placeholder_model())
                .filter_map(|f| {
                    let p = stores.placeholders.get(f.kind)?;
                    let x = wx + f.x;
                    let z = wz + f.z;
                    Some((
                        p.hash.clone(),
                        Transform {
                            // The ground under a street object is the measured
                            // field, the same one its boxes would have been
                            // built onto (`systems::terrain`), and 0 wherever
                            // there is no field.
                            translation: Vec3::new(x, terrain.at(x, z), z),
                            rotation: Quat::from_rotation_y(f.yaw + p.yaw_offset),
                            scale: Vec3::splat(p.scale),
                        },
                    ))
                })
                .collect();
            for (hash, transform) in queued {
                stores.placeholders.enqueue(c, &hash, transform);
            }
        }
        stores.places.insert_cell(c, wx, wz, cell_places);
        // Same origin, same lifetime, same retire path again.
        stores.tunnels.insert_cell(c, Vec2::new(wx, wz), tunnels);
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
            let (field, bank) = match shore {
                Some(f) => (
                    Some(mats.images.add(water::shore_field_image(&f))),
                    Some(mats.images.add(water::bank_field_image(&f))),
                ),
                None => (None, None),
            };
            mats.water
                .add(sea_state.material(field, bank, Vec2::new(wx, wz), tile_m))
        });

        let mut entities = Vec::with_capacity(surface_meshes.len());
        for sm in surface_meshes {
            let stand = if placed {
                Stand::Placed
            } else {
                Stand::Terrain(&terrain, &stores.tunnels)
            };
            let mesh = meshes.add(build_mesh(&sm, wx, wz, stand));
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
                SurfaceGroup::Road(_) | SurfaceGroup::RoadCasing(_) | SurfaceGroup::TunnelRoad(_)
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
                            extension: RoadDepthExtension::default(),
                        })
                    })
                    .clone();
                commands.spawn((placement, MeshMaterial3d(material))).id()
            } else {
                // The ground's own finish, for the groups that are walked on. It
                // *multiplies* `perceptual_roughness` rather than replacing it,
                // which is what keeps `systems::wet_surfaces` composing: rain
                // writes the scalar, this varies it across the square, and
                // neither has to know about the other. Same contract the road map
                // has, and the same order the three roughness terms are required
                // to stay in (`docs/notes/surface-finish-and-occlusion.md`).
                //
                // Minted here rather than inside the closure below because that
                // one already borrows `surfaces.materials`, and this is cached on
                // `surfaces` too — every group that takes the finish shares one
                // image, since the map says nothing about colour and the groups
                // differ in nothing else.
                let finish =
                    wants_ground_finish(sm.group).then(|| surfaces.ground_finish(&mut mats.images));
                let material = surfaces
                    .materials
                    .entry(sm.group)
                    .or_insert_with(|| {
                        let [r, g, b] = sm.group.base_color();
                        mats.plain.add(GroundMaterial {
                            base: StandardMaterial {
                                base_color: Color::srgb(r, g, b),
                                perceptual_roughness: sm.group.perceptual_roughness(),
                                metallic_roughness_texture: finish,
                                // UV0 on an area surface is **metres from the map
                                // anchor**, so this is what turns a distance into a
                                // number of repeats. Set unconditionally: with no
                                // texture bound there is nothing for it to transform,
                                // and a field that is only sometimes right is worse
                                // than one that is always the same.
                                uv_transform: ground_finish_uv(),
                                // What actually resolves the coplanar ground stack —
                                // see `SurfaceGroup::depth_bias`. Physical separation
                                // is only millimetres and cannot survive the far plane.
                                depth_bias: sm.group.depth_bias(),
                                ..default()
                            },
                            extension: ground_detail_for(sm.group),
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
            if matches!(
                sm.group,
                SurfaceGroup::TunnelRoad(_) | SurfaceGroup::TunnelStructure
            ) {
                commands.entity(id).insert(crate::components::TunnelSurface);
            }
            entities.push(id);
        }
        tracing::debug!(cell = ?c, groups = entities.len(), "surface cell built");
        surfaces.loaded.insert(c, entities);
        if !decks.is_empty() {
            // Measured by atlas while it graded this ground: nothing to ask PDOK.
            let known = atlas_decks
                .as_deref()
                .and_then(|a| crate::systems::terrain_tiles::match_atlas_decks(&decks, tile_m, a));
            let (boxes, clusters) = if known.is_some() {
                (Vec::new(), Vec::new())
            } else {
                cluster_decks(&decks, c, tile_m)
            };
            surfaces.pending_decks.insert(
                c,
                PendingDecks {
                    decks,
                    approaches,
                    roads: deck_roads,
                    world: Vec2::new(wx, wz),
                    clusters,
                    boxes,
                    known,
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
/// is left in the padded box is the terrain the structure stands over. Out of
/// coverage, or where the layer has given up, the deck settles flat — which is
/// where every road already is, so the fallback is the old picture with the draw
/// order finally right.
pub fn resolve_bridge_decks(
    mut commands: Commands,
    mut surfaces: ResMut<MapSurfaces>,
    mut height: ResMut<HeightLayer>,
    mut meshes: ResMut<Assets<Mesh>>,
    // The same three asset collections `update_map_surfaces` writes: a deck is a
    // plain material, and the carriageway standing on it is a road one.
    mut mats: SurfaceMaterials,
    bigspace: Query<Entity, With<BigSpace>>,
    terrain: Res<TerrainField>,
    tunnels: Res<crate::systems::tunnels::Tunnels>,
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

        // Every cluster is asked for at once, and nothing is measured until all
        // have answered — a patch, or a settled "nothing here".
        let mut ready = true;
        for cluster in &pending.clusters {
            let (surface, ground) = cluster.requests();
            for req in [surface, ground] {
                if height
                    .patch_ranked(req, crate::systems::height::RANK_DECKS)
                    .is_none()
                    && !height.settled(&req)
                {
                    ready = false;
                }
            }
        }
        if !ready {
            continue;
        }
        let heights: Vec<f32> = match &pending.known {
            Some(known) => known.clone(),
            None => pending
                .boxes
                .iter()
                .map(|b| deck_height(&height, &pending.clusters, b))
                .collect(),
        };

        let Some(pending) = surfaces.pending_decks.remove(&c) else {
            continue;
        };
        let mesh = build_deck_mesh(&pending.decks, &heights);
        let cell_world = pending.world;
        // The one terrain height the whole structure stands on — read here rather
        // than left to `build_mesh`, because the carriageways cut out of these
        // slabs have to be lifted onto *this* number and not onto the field's
        // answer over their own narrower footprint.
        let deck_level = if terrain.is_ready() {
            terrain.level_over(
                mesh.positions
                    .iter()
                    .map(|p| [cell_world.x + p[0], cell_world.y + p[2]]),
            )
        } else {
            0.0
        };
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
                .enumerate()
                .filter(|(_, (_, &h))| h > 0.0)
                .map(|(i, (deck, &height))| Plate {
                    deck,
                    world: cell_world,
                    height,
                    // This deck's own approaches, by the index
                    // `vector_tiles::Approach::deck` carries.
                    approaches: pending
                        .approaches
                        .iter()
                        .filter(|a| a.deck as usize == i)
                        .map(|a| a.points.clone())
                        .collect(),
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
                mats.plain.add(GroundMaterial {
                    base: StandardMaterial {
                        base_color: Color::srgb(r, g, b),
                        perceptual_roughness: SurfaceGroup::BridgeDeck.perceptual_roughness(),
                        depth_bias: SurfaceGroup::BridgeDeck.depth_bias(),
                        // The fascia is a single band of quads with no inside to
                        // speak of, and a ring's winding out of MVT is not something
                        // to bet a whole structure's visibility on.
                        double_sided: true,
                        cull_mode: None,
                        ..default()
                    },
                    extension: ground_detail_for(SurfaceGroup::BridgeDeck),
                })
            })
            .clone();
        let placement = || {
            (
                Transform::from_translation(local),
                grid_cell,
                ChildOf(root),
                MapSurface,
                crate::components::HideInSpace,
            )
        };
        let mut spawned: Vec<Entity> = Vec::with_capacity(pending.roads.len() + 1);
        // A slab can be cut away to nothing where the carriageway covers the whole
        // structure — the roads on it still have to be placed, so this skips the
        // deck rather than the cell.
        if !mesh.indices.is_empty() {
            spawned.push(
                commands
                    .spawn((
                        Mesh3d(meshes.add(build_mesh(
                            &mesh,
                            cell_world.x,
                            cell_world.y,
                            Stand::Terrain(&terrain, &tunnels),
                        ))),
                        MeshMaterial3d(material),
                        placement(),
                    ))
                    .id(),
            );
        }
        // …and the carriageways those slabs carry, lifted onto them. Same
        // materials as any road, so a bridge keeps its tarmac, its lane markings
        // and its place in the depth ladder all the way across.
        for (di, sm) in &pending.roads {
            let Some(&h) = heights.get(*di as usize) else {
                continue;
            };
            let material = surfaces
                .road_materials
                .entry(sm.group)
                .or_insert_with(|| {
                    let base = road_base_material(sm.group, &mut mats.images);
                    mats.road.add(RoadMaterial {
                        base,
                        extension: RoadDepthExtension::default(),
                    })
                })
                .clone();
            spawned.push(
                commands
                    .spawn((
                        Mesh3d(meshes.add(build_mesh(
                            sm,
                            cell_world.x,
                            cell_world.y,
                            Stand::Lifted(deck_level + h),
                        ))),
                        MeshMaterial3d(material),
                        placement(),
                    ))
                    .id(),
            );
        }
        if let Some(entities) = surfaces.loaded.get_mut(&c) {
            entities.extend(spawned);
        }
        let tallest = heights.iter().copied().fold(0.0f32, f32::max);
        tracing::debug!(
            cell = ?c,
            decks = heights.len(),
            roads = pending.roads.len(),
            tallest,
            "bridge decks placed"
        );
    }
}

/// How high one deck stands above the ground it crosses, in metres, read from its
/// cluster's two patches (`cluster_decks`).
///
/// `0.0` is the answer for "no data here" as much as for a bridge that really is
/// at street level — a Dutch canal bridge is exactly that, and telling the two
/// apart would buy nothing, because both want a flat deck.
fn deck_height(layer: &HeightLayer, clusters: &[DeckCluster], deck: &DeckBox) -> f32 {
    let Some(cluster) = deck.cluster.and_then(|i| clusters.get(i)) else {
        return 0.0;
    };
    let (surface, ground) = cluster.requests();
    let deck_top = layer
        .get(&surface)
        .and_then(|p| p.median_where(|rd| inside_rings(&deck.rings, rd)));
    // No outline test on the terrain side: AHN's DTM has the structure cut out of
    // it already, so everything left in the box around the deck *is* the ground
    // beside it. The box is the deck's own, padded — the cluster's patch reaches
    // further, into other decks' ground.
    let (lo, hi) = (
        [deck.min[0] - GROUND_RING_M, deck.min[1] - GROUND_RING_M],
        [deck.max[0] + GROUND_RING_M, deck.max[1] + GROUND_RING_M],
    );
    let ground = layer.get(&ground).and_then(|p| {
        p.median_where(|rd| (lo[0]..=hi[0]).contains(&rd[0]) && (lo[1]..=hi[1]).contains(&rd[1]))
    });
    match (deck_top, ground) {
        // The same rule atlas measures its decks by, for the ground it grades.
        (Some(top), Some(ground)) => cartopolis_geo::terrain_grade::deck_clearance(top, ground),
        _ => 0.0,
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

/// What a group's material is created with, for the sub-metre slope layer.
///
/// Zero for the ground — `ground_detail::refresh_ground_detail` fills it in on the
/// frame after, and its material count is what makes a newly streamed cell get one.
/// **Opted out for the volumetric groups**: a canopy's normals describe the tree and
/// a bench's describe the bench, which is the same rule `stand_on_terrain` follows
/// when it refuses to re-derive their normals from the field.
fn ground_detail_for(group: SurfaceGroup) -> GroundDetailExtension {
    if matches!(
        group,
        SurfaceGroup::Canopy
            | SurfaceGroup::Trunk
            | SurfaceGroup::Furniture
            | SurfaceGroup::TunnelRoad(_)
            | SurfaceGroup::TunnelStructure
    ) {
        return GroundDetailExtension {
            params: crate::systems::ground_detail::DETAIL_OPTED_OUT,
            slope: None,
            // A surface group is never a clipmap tile, so the level cross-fade is
            // off here as it is on every other ground material but those.
            ..default()
        };
    }
    GroundDetailExtension::default()
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
            SurfaceGroup::Road(class) | SurfaceGroup::TunnelRoad(class) => {
                Some(images.add(road_surface_image(class)))
            }
            _ => None,
        },
        // The roughness/metallic half of the same generated surface: polished
        // wheel paths, smoother paint, and a metal rail head (see
        // `RoadTexture::surface`). It multiplies `perceptual_roughness` below
        // rather than replacing it, so the class's own finish is still what sets
        // the level and this only varies it across the carriageway — and
        // `systems::wet_surfaces` keeps working, because that lowers the same
        // scalar.
        metallic_roughness_texture: match group {
            SurfaceGroup::Road(class) | SurfaceGroup::TunnelRoad(class) => {
                Some(images.add(road_material_image(class)))
            }
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

/// How many metres of ground one repeat of the finish map covers.
///
/// Chosen against the two ways it can be wrong. Much smaller and the coarse
/// octave becomes a repeating pattern you can pick out from standing height —
/// which is worse than no variation, because a pattern is something the map looks
/// like it is asserting. Much larger and the whole visible square falls inside
/// one blotch, i.e. the flat sheet this exists to break up, just at a different
/// level.
const GROUND_FINISH_M: f32 = 7.0;

/// Which groups take the ground finish.
///
/// The two the note names as the largest flat areas in a street scene, plus the
/// one other bare-ground group — and no more than that. Every group left out has
/// a reason. Roads carry their own, and a better one. Water is a shader. Grass, forest and crops have their own colour
/// systems writing the same material, and `systems::grass` draws actual blades
/// over the first of them. Canopy, trunk and furniture are objects rather than
/// ground, and their UV0 is not the anchor-metre projection this is scaled
/// against. A bridge deck is a structure, not a surface you walk to.
fn wants_ground_finish(group: SurfaceGroup) -> bool {
    matches!(
        group,
        SurfaceGroup::Paving | SurfaceGroup::Built | SurfaceGroup::Sand
    )
}

/// UV0 is metres from the map anchor, so a repeat every [`GROUND_FINISH_M`] is a
/// plain scale.
fn ground_finish_uv() -> bevy::math::Affine2 {
    bevy::math::Affine2::from_scale(Vec2::splat(1.0 / GROUND_FINISH_M))
}

/// Upload [`ground_texture::ground_finish_texture`].
///
/// `Rgba8Unorm`, not sRGB, for the reason spelled out at [`road_material_image`]:
/// these are coefficients and the sRGB decode would bend every one of them toward
/// zero. Mipmapped and anisotropic because the ground is the surface most often
/// seen at a grazing angle — without the chain, an un-averaged roughness at
/// distance is a crawling sparkle rather than a finish.
///
/// One known and accepted inexactness, shared with [`road_material_image`]:
/// `mip::build_mip_chain` averages in *linear sRGB*, which is right for a colour
/// and not for a coefficient, so the downsampled levels are biased slightly
/// toward the smoother end. Mip 0 is copied verbatim and is exact, and over this
/// map's narrow 0.75–1.0 range the curve is close enough to straight that the
/// bias is under a texel value. A coefficient-space chain would be the correct
/// fix and belongs to both call sites at once, not to this one.
fn ground_finish_image() -> Image {
    use bevy::asset::RenderAssetUsages;
    use bevy::render::render_resource::TextureFormat;

    let px = crate::systems::ground_texture::GROUND_TEX_PX;
    let (data, mip_count) = crate::systems::mip::build_mip_chain(
        px,
        px,
        crate::systems::ground_texture::ground_finish_texture(),
    );
    crate::systems::tile_loader::image_from_mip_chain(
        px,
        px,
        data,
        mip_count,
        TextureFormat::Rgba8Unorm,
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

/// The roughness/metallic map that goes with [`road_surface_image`].
///
/// Two differences from its sibling and both are load-bearing. The format is
/// **`Rgba8Unorm`, not sRGB** — these are material coefficients, and running them
/// through the sRGB decode would bend every one of them toward zero (a stored
/// 0.62 would arrive as 0.34, i.e. a road markedly glossier than intended). And
/// there is no anisotropy: it is sampled at the same rate as the colour, which
/// already carries the filtering that matters.
fn road_material_image(class: RoadClass) -> Image {
    use bevy::render::render_resource::TextureFormat;

    let t = road_texture(class);
    let (data, mip_count) = crate::systems::mip::build_mip_chain(t.width, t.height, t.surface);
    crate::systems::tile_loader::image_from_mip_chain(
        t.width,
        t.height,
        data,
        mip_count,
        TextureFormat::Rgba8Unorm,
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
        let (SurfaceGroup::Road(class) | SurfaceGroup::TunnelRoad(class)) = *group else {
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
            material.base.metallic_roughness_texture = Some(images.add(road_material_image(class)));
            material.base.base_color = Color::WHITE;
        } else {
            material.base.base_color_texture = None;
            // The roughness map goes with it. Leaving it bound would keep the
            // wheel paths and the rail head's metal on a road that has just been
            // flattened to one colour, which is a stranger picture than either
            // side of the toggle is meant to produce.
            material.base.metallic_roughness_texture = None;
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
/// How [`build_mesh`] gets a surface off the flat plane it was tessellated on.
enum Stand<'a> {
    /// Onto the measured ground, here and now.
    Terrain(&'a TerrainField, &'a crate::systems::tunnels::Tunnels),
    /// A ready-made height for the whole mesh, in place of the terrain. This is how
    /// a carriageway riding a bridge deck follows the *deck* — the deck's own
    /// single level, plus its measured lift — instead of asking the field again
    /// over its own slightly different footprint and landing a few centimetres off
    /// the slab it was cut out of.
    Lifted(f32),
    /// Already stood on the ground by the surface worker (`spawn_geometry_task`).
    Placed,
}

fn build_mesh(sm: &SurfaceMesh, wx: f32, wz: f32, stand: Stand<'_>) -> Mesh {
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

    let mut on_terrain = matches!(stand, Stand::Placed);
    match stand {
        Stand::Placed => {}
        Stand::Lifted(y) => {
            for p in &mut positions {
                p[1] += y;
            }
        }
        Stand::Terrain(terrain, _) if !terrain.is_ready() => {}
        Stand::Terrain(terrain, tunnels) => {
            on_terrain = true;
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
                tunnels,
            );
        }
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
    // Before `indices` is moved into the mesh: the oversize report below needs
    // both counts, and the upload cost is a function of the pair.
    let index_count = indices.len();
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
        SurfaceGroup::Road(_) | SurfaceGroup::RoadCasing(_) | SurfaceGroup::TunnelRoad(_)
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
    // **Say which group produced an oversize piece.** `upload_meter` already
    // flags a mesh no per-frame throttle can spread, and it is downstream of
    // every producer in the client — it sees an `AssetId` and a vertex count and
    // cannot name what made it, so six such meshes a run were attributable to
    // nothing. Here the group is in hand.
    //
    // Deliberately a report and not a clamp: the cap is enforced where a mesh is
    // *cut*, and cutting one here means splitting a finished mesh into several
    // entities, which is the caller's shape to change and not this function's.
    // What this buys is knowing which caller to change.
    {
        let bytes = cartopolis_geo::vector_tiles::mesh_upload_bytes(positions.len(), index_count);
        let cap = crate::systems::lod22::max_cell_mesh_bytes();
        if bytes > cap {
            tracing::warn!(
                group = ?sm.group,
                asset_kb = bytes / 1024,
                cap_kb = cap / 1024,
                vertices = positions.len(),
                indices = index_count,
                terrain_ready = on_terrain,
                "a finished surface is over the cell mesh cap — the cap is applied \
                 where the mesh is cut and this group grew past it afterwards"
            );
        }
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
/// Place a cell's water, at the measured level where the register reaches it.
///
/// Water used to take one height for the whole mesh, because a body is level and
/// following the ground per vertex would tilt a lake down the valley it sits in.
/// That stays true, and it stops being the whole story once the level is *measured*:
/// one Groningen cell holds bodies at −0.08, −0.77, −0.86 and −1.19 m NAP, so a
/// single height is wrong for all but one of them.
///
/// Two things then have to hold at once, and the second is what this function is
/// really for.
///
/// **The level comes from the register, per vertex**, which gives each body its own
/// flat surface for free — the lookup is by position and every registered body is
/// flat. Outside the Netherlands nothing is registered and the ground's own level is
/// used, which is exactly the old behaviour.
///
/// **And the surface may never sink below the ground drawn under it.** The register
/// knows a canal is 2 m down; the *field the basemap is displaced by* samples every
/// 25 m and can only render a blurred, shallower dip. Taking the measured level
/// literally therefore put the water several metres under an opaque basemap that had
/// barely moved — the sheet vanished, and what you saw was flat painted map where
/// rendered water with its shore shading used to be. Measured 2026-09-12: the
/// waterline moved 1 px when the drop asked for five.
///
/// So the surface is clamped to the ground beneath it. In the middle of a body the
/// carve has taken the ground below the water and the measured level wins; at the
/// bank, where 25 m cannot cut a kerb, the water rises to meet the quay instead of
/// disappearing under it. That reads as a bank rather than as a wall, which is the
/// honest picture of what the ground model can currently say — and it gets deeper on
/// its own as the field gets finer, with no change here.
///
/// The mesh is refined first because earcut emits **only the polygon's boundary**:
/// with no interior vertices the clamp could only ever be evaluated at the banks,
/// where it is the ground level, and every body would be flat at quay height again.
#[allow(clippy::too_many_arguments)]
fn water_surface<G: MeasuredGround>(
    positions: &mut Vec<[f32; 3]>,
    indices: &mut Vec<u32>,
    uvs: &mut Vec<[f32; 2]>,
    normals: &mut Vec<[f32; 3]>,
    colors: &mut Vec<[f32; 4]>,
    rungs: &mut Vec<f32>,
    origin: Vec2,
    terrain: &G,
) {
    let fallback = terrain.level_over(positions.iter().map(|p| [origin.x + p[0], origin.y + p[2]]));
    if !terrain.has_water_levels() {
        for p in positions.iter_mut() {
            p[1] += fallback;
        }
        return;
    }
    crate::systems::terrain::refine_for_terrain(
        positions,
        indices,
        uvs,
        normals,
        colors,
        rungs,
        terrain.ground_edge_m(),
        Some(crate::systems::terrain::Crease {
            // Through the deref: `TerrainField` is the live resource and
            // `TerrainSnapshot` is the immutable field it hands out, which is what
            // the refinement is defined against.
            field: &**terrain,
            origin: origin.to_array(),
        }),
    );
    for p in positions.iter_mut() {
        let (wx, wz) = (origin.x + p[0], origin.y + p[2]);
        let level = terrain.water_surface_at(wx, wz).unwrap_or(fallback);
        p[1] += level.max(terrain.at(wx, wz));
    }
    // The surface is no longer planar, so the normals earcut left behind describe a
    // shape that is not there any more.
    if !normals.is_empty() {
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

/// Ground surfaces get a fresh normal from the field's own slope; the vegetation
/// groups keep theirs (a tree's normals describe the tree, not the hill).
#[allow(clippy::too_many_arguments)]
/// How far to reach for the land *around* a tunnel — see
/// [`cartopolis_geo::tunnels::DATUM_M`], which is where it lives so the harness
/// reaches for the same distance.
pub use cartopolis_geo::tunnels::DATUM_M as TUNNEL_DATUM_M;

/// What [`cartopolis_geo::tunnels`] asks of the streamed height field.
struct FieldSurvey<'a>(&'a TerrainSnapshot);

impl cartopolis_geo::tunnels::Survey for FieldSurvey<'_> {
    fn here(&self, x: f32, z: f32) -> f32 {
        self.0.at(x, z)
    }
    fn around(&self, x: f32, z: f32) -> f32 {
        self.0.surrounding(x, z, TUNNEL_DATUM_M)
    }
}

fn stand_on_terrain<G: MeasuredGround>(
    group: SurfaceGroup,
    positions: &mut Vec<[f32; 3]>,
    indices: &mut Vec<u32>,
    uvs: &mut Vec<[f32; 2]>,
    normals: &mut Vec<[f32; 3]>,
    colors: &mut Vec<[f32; 4]>,
    rungs: &mut Vec<f32>,
    origin: Vec2,
    terrain: &G,
    tunnels: &crate::systems::tunnels::Tunnels,
) {
    // A tunnel belongs here for the same reason a tree does, in both halves of what
    // this flag means. Its walls are vertical, so re-deriving their normals from the
    // ground's slope would light them as though they were the hillside they are cut
    // into; and refining them against the field would split a wall quad into a grid
    // to follow ground it is not standing on. Displacing every vertex by the field
    // is still right, and is what carries the tunnel under a slope with it.
    let volumetric = matches!(
        group,
        SurfaceGroup::Canopy
            | SurfaceGroup::Trunk
            | SurfaceGroup::Furniture
            | SurfaceGroup::TunnelRoad(_)
            | SurfaceGroup::TunnelStructure
    );
    // **A tunnel is not dug twice.** Its vertices are metres below the ground sheet,
    // and over an open cut the measured ground sheet is already the trench floor —
    // AHN's bare-ground model has no roof to hide the bottom of a cutting, so it
    // measures it (the Groningen ring falls from +5.5 m to −3.6 m across forty
    // metres). Displacing an invented seven metres by a measured nine puts the
    // carriageway sixteen metres under the street, with the wall tops buried with
    // it.
    //
    // So each vertex is lifted back by however much the measurement has *already*
    // accounted for: `dug`, the drop from the land around the tunnel to the ground
    // under it. Four cases and the same arithmetic settles all of them — a fully
    // measured cut puts the deck exactly on the measured floor and the wall tops
    // exactly at street level; a covered tunnel, where the laser sees the roof and
    // `dug` is zero, keeps the full convention; a partly-measured cut absorbs what
    // was measured and invents the rest; and outside the Netherlands the field is
    // flat zero, so this is the old behaviour to the bit.
    //
    // **The cap is the depth under this vertex, not the deepest point of the mesh
    // it happens to be in.** It was the latter until 2026-09-18, and a merged mesh
    // is the whole tile's tunnels: every vertex in it was lifted by the deepest
    // one's convention, so everything shallower rose too far. Almost all of a
    // tunnel is shallower — `depth_at` spends about 117 m ramping at each end, and
    // Groningen's runs arrive 172–242 m long — so forty metres in, where the
    // tessellator wants 2.4 m of depth and the survey has already dug 6.4, the deck
    // was lifted by the full 7 m and drawn four metres above the land beside it.
    // That is the carriageway standing proud of the ground in the phone captures.
    //
    // `convention_depth` answers for the place rather than for the vertex's own
    // height, which is what keeps a wall from shearing: the top of a wall quad sits
    // at `WALL_TOP_Y` and would otherwise be capped at zero and left behind while
    // the deck below it rose. Asking by position gives the whole section of tunnel
    // at that (x, z) one lift, so its height is preserved and the clearance with
    // it. A vertex outside every corridor — the notch triangles on the outside of a
    // bend reach a hair past `half_w` — keeps the old mesh-wide cap, which for
    // those is the right answer anyway.
    if matches!(
        group,
        SurfaceGroup::TunnelRoad(_) | SurfaceGroup::TunnelStructure
    ) {
        cartopolis_geo::tunnels::stand_tunnel_on(
            positions,
            origin.to_array(),
            &FieldSurvey(&**terrain),
            &|p| tunnels.convention_depth(Vec2::from_array(p)),
        );
        return;
    }
    // A viaduct wants *one* height for the whole thing: it is straight, and the
    // ground under it dipping into a valley is precisely what it does not do.
    if matches!(group, SurfaceGroup::BridgeDeck) {
        let level =
            terrain.level_over(positions.iter().map(|p| [origin.x + p[0], origin.y + p[2]]));
        for p in positions.iter_mut() {
            p[1] += level;
        }
        return;
    }
    if matches!(group, SurfaceGroup::Water) {
        water_surface(
            positions, indices, uvs, normals, colors, rungs, origin, terrain,
        );
        return;
    }
    // **The ground these stand on is the measured ground with the lids back on
    // it**, and that has to be one surface rather than a displacement and a later
    // correction. Where a road has a lid the survey — flown before the lid was
    // built — still shows the open trench; lifting the vertices afterwards left
    // the lift invisible to the refinement, so a land parcel of two triangles
    // interpolated nine metres across a chord while the road beside it followed
    // the taper, and the ground tore open along the buried road.
    let lidded = cartopolis_geo::tunnels::Lidded {
        ground: &**terrain,
        around: &|x, z| terrain.surrounding(x, z, TUNNEL_DATUM_M),
        weight: &|p| tunnels.covered_weight(Vec2::from_array(p), TUNNEL_DATUM_M),
    };
    let t0 = bevy::platform::time::Instant::now();
    if !volumetric {
        crate::systems::terrain::refine_for_terrain(
            positions,
            indices,
            uvs,
            normals,
            colors,
            rungs,
            terrain.ground_edge_m(),
            Some(crate::systems::terrain::Crease {
                field: &lidded,
                origin: origin.to_array(),
            }),
        );
    }
    let t1 = bevy::platform::time::Instant::now();
    {
        use cartopolis_geo::terrain_refine::GroundField as _;
        for p in positions.iter_mut() {
            p[1] += lidded.at(origin.x + p[0], origin.y + p[2]);
        }
    }
    let t2 = bevy::platform::time::Instant::now();
    GROUND_SPLIT.with(|g| {
        let mut v = g.get();
        v[0] += t1.duration_since(t0).as_micros() as u64;
        v[1] += t2.duration_since(t1).as_micros() as u64;
        v[3] += positions.len() as u64;
        g.set(v);
    });
    if !volumetric {
        // Normals from the field, then flattened where the fill has raised the
        // ground: a surface lifted out of a trench is level, and shading it with
        // the trench's own slope would light a flat lid as though it still dipped.
        *normals = positions
            .iter()
            .map(|p| {
                let (x, z) = (origin.x + p[0], origin.y + p[2]);
                let w = tunnels.covered_weight(Vec2::new(x, z), TUNNEL_DATUM_M);
                let n = terrain.normal_at(x, z);
                n.lerp(Vec3::Y, w).normalize_or(Vec3::Y).to_array()
            })
            .collect();
    }
    GROUND_SPLIT.with(|g| {
        let mut v = g.get();
        v[2] += t2.elapsed().as_micros() as u64;
        g.set(v);
    });
}

thread_local! {
    /// Where `stand_on_terrain`'s time goes on this worker, µs: refine, displace,
    /// normals, and the vertex count out. Read and reset per cell job.
    static GROUND_SPLIT: std::cell::Cell<[u64; 4]> = const { std::cell::Cell::new([0; 4]) };
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

    /// `square_deck`, moved `dx`, `dz` metres into the tile.
    fn deck_at(span: f32, dx: f32, dz: f32) -> BridgeDeck {
        let mut d = square_deck(span);
        for p in &mut d.rings[0] {
            *p = [p[0] + dx, p[1] + dz];
        }
        d.min = [dx, dz];
        d.max = [dx + span, dz + span];
        d
    }

    /// **Neighbouring decks share their patches; distant ones do not; and every
    /// deck is measured from its own box, not its cluster's.**
    #[test]
    fn decks_are_measured_in_clusters() {
        // Groningen's Grote Markt cell.
        let cell = (8490, 5320);
        let decks = [
            deck_at(20.0, 100.0, 100.0),
            deck_at(15.0, 180.0, 120.0),
            deck_at(30.0, 1_000.0, 900.0),
        ];
        let (boxes, clusters) = cluster_decks(&decks, cell, 1470.0);
        assert_eq!(
            clusters.len(),
            2,
            "two near decks share, the far one does not"
        );
        assert_eq!(boxes[0].cluster, boxes[1].cluster);
        assert_ne!(boxes[0].cluster, boxes[2].cluster);
        for c in &clusters {
            assert!(c.max[0] - c.min[0] <= DECK_CLUSTER_M && c.max[1] - c.min[1] <= DECK_CLUSTER_M);
            let (s, g) = c.requests();
            assert!(s.grid <= DECK_GRID_MAX && g.grid == s.grid);
            assert_eq!(s.model, HeightModel::Surface);
        }
        // A deck's own box is its footprint, not the cluster's.
        let b = &boxes[0];
        assert!(b.max[0] - b.min[0] < 25.0 && b.max[1] - b.min[1] < 25.0);
    }

    /// Outside AHN a deck is settled flat without a request.
    #[test]
    fn a_deck_outside_ahn_asks_for_nothing() {
        // Paris, at z14.
        let (x, y) = cartopolis_core::lat_lng_to_tile(48.8566, 2.3522, 14);
        let (boxes, clusters) = cluster_decks(&[deck_at(20.0, 10.0, 10.0)], (x, y), 1470.0);
        assert!(clusters.is_empty());
        assert_eq!(boxes[0].cluster, None);
        assert_eq!(
            deck_height(&HeightLayer::default(), &clusters, &boxes[0]),
            0.0
        );
    }

    /// **The approach corridor is the approach, and the deck is not in it.**
    ///
    /// The two failure modes this separates are the ones that made the ramp worth
    /// having a mask of its own. Marking the approach over the structure lets the
    /// relaxation grade the land up to meet a road that is not standing on it; and
    /// a ramp that travels through the whole road mask raised 18 % of the
    /// Groningen fixture's streets for five bridges, because the street passing
    /// *under* a viaduct touches its approach and a gradient goes wherever a
    /// raster corridor is connected.
    #[test]
    fn the_approach_is_marked_beside_the_deck_and_never_on_it() {
        let mut plates = DeckPlates::default();
        // A 40 m deck at the grid's own origin, with an approach running 60 m east
        // out of its eastern edge.
        let run: Vec<[f32; 2]> = (0..7).map(|k| [40.0 + k as f32 * 10.0, 20.0]).collect();
        plates.set_cell(
            (0, 0),
            vec![Plate {
                deck: square_deck(40.0),
                world: Vec2::new(-20.0, -20.0),
                height: 6.0,
                approaches: vec![run],
            }],
        );

        // 201 samples over ±100 m is one per metre, which resolves a 40 m deck.
        let (n, half) = (201usize, 100.0f32);
        let (ramp, lift) = plates.grade_masks(n, half);
        let at = |x: f32, z: f32| -> usize {
            let g = |v: f32| ((v + half).round()) as usize;
            g(z) * n + g(x)
        };

        // On the structure: a height, and not approach.
        assert_eq!(
            lift[at(0.0, 0.0)],
            6.0,
            "the deck's own ground has no height"
        );
        assert!(
            !ramp[at(0.0, 0.0)],
            "the deck is not something to ramp along"
        );

        // Beside it, along the run: approach, and no height of its own.
        assert!(
            ramp[at(30.0, 0.0)],
            "the approach beside the deck is not marked"
        );
        assert!(
            lift[at(30.0, 0.0)].is_nan(),
            "a sample off the structure must carry no deck height"
        );

        // Well away from both: neither.
        assert!(
            !ramp[at(-80.0, -80.0)],
            "ordinary ground was marked as approach"
        );
        assert!(lift[at(-80.0, -80.0)].is_nan());

        // And an empty set of plates is an empty pair, not a panic — which is
        // every tile in a city with no viaduct in it.
        let (ramp, lift) = DeckPlates::default().grade_masks(n, half);
        assert!(!ramp.iter().any(|m| *m));
        assert!(!lift.iter().any(|v| v.is_finite()));
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
                approaches: Vec::new(),
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
                    approaches: Vec::new(),
                }],
            );
        }
        assert_eq!(plates.lift_at(Vec2::new(20.0, 20.0)), 11.0);
    }

    /// **A cell that is re-tessellated and publishes the same decks must not move
    /// the revision**, or the terrain grading loops on itself for ever.
    ///
    /// The height field now grades its approach embankments against these plates,
    /// so a bake bumps `TerrainField::revision`; every streamer standing on the
    /// ground answers that by re-tessellating its ring; every rebuilt cell with a
    /// viaduct arrives back in `set_cell`. Bumping unconditionally closes that
    /// loop and the city retears about once a second for the rest of the session —
    /// with nothing in the log to say why, because each step is behaving exactly
    /// as designed.
    #[test]
    fn republishing_the_same_decks_is_not_a_change() {
        let plate = || Plate {
            deck: square_deck(40.0),
            world: Vec2::new(100.0, 200.0),
            height: 6.0,
            approaches: vec![vec![[40.0, 20.0], [90.0, 20.0]]],
        };
        let mut plates = DeckPlates::default();
        plates.set_cell((1, 1), vec![plate()]);
        let after_first = plates.revision;
        assert_ne!(after_first, 0, "the first deck is a change");

        plates.set_cell((1, 1), vec![plate()]);
        assert_eq!(
            plates.revision, after_first,
            "the same cell re-tessellated to the same decks moved the revision"
        );

        // A measurement that lands differently is a change, which is the case the
        // revision exists for: a deck's height arrives some frames after its cell.
        let mut taller = plate();
        taller.height = 6.5;
        plates.set_cell((1, 1), vec![taller]);
        assert_ne!(
            plates.revision, after_first,
            "a deck that changed height must move the revision"
        );

        // …and so is one whose approaches changed, since those are what the
        // embankment is graded along.
        let mut rerouted = plate();
        rerouted.approaches = vec![vec![[40.0, 20.0], [140.0, 20.0]]];
        rerouted.height = 6.5;
        let before = plates.revision;
        plates.set_cell((1, 1), vec![rerouted]);
        assert_ne!(plates.revision, before, "a moved approach must be a change");
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
                approaches: Vec::new(),
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
            (
                "flat_ground",
                DetailWants {
                    flat_ground: false,
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
