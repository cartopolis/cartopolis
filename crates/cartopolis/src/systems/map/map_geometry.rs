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

use cartopolis_geo::deck_profile::DeckMeasure;

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
use crate::systems::terrain::{MeasuredGround, TerrainField};
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
///
/// **Nine, the whole 3×3 ring, not the eight it was.** Over graded ground every
/// job parks after its downloads until the terrain lands
/// (`TerrainField::holds_builds`, up to `terrain::GRADED_HOLD_S`) and keeps its
/// slot while it waits, so at eight the ninth cell could not even start
/// downloading until the hold ended — one cell of every start built a hold later
/// than the rest. A parked job holds downloaded text, not meshes.
const SURFACE_MAX_IN_FLIGHT: usize = 9;

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
    // The surveyed piers, outer rings tile-local: stood from the ground up to the
    // deck they hold, so they too wait on the deck's height.
    Vec<Vec<[f32; 2]>>,
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
    Option<Vec<crate::systems::terrain_tiles::AtlasDeck>>,
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
    /// Surveyed piers each cell stood — a count for `--dump-state`, nothing reads
    /// it to place anything.
    piers: HashMap<Cell, usize>,
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
    /// The deck's measured top in NAP metres — the survey's surface over the
    /// structure — where the survey covers it. What an approach is graded up to
    /// and what the drawn deck stands at; see [`deck_measure`].
    top_nap: Option<f32>,
    /// Where the deck was drawn, in world `y`: what traffic on it stands on.
    world_top: f32,
    /// The deck's height along its length, where the survey measured one
    /// (`DeckShape`). `world_top` and `top_nap` are its median, and what a deck
    /// without one stands at everywhere.
    shape: Option<Arc<DeckShape>>,
}

impl Plate {
    /// World `y` of the drawn deck over a tile-local point.
    fn world_top_at(&self, local: [f32; 2]) -> f32 {
        self.shape
            .as_ref()
            .map_or(self.world_top, |s| s.world_y(local))
    }
}

/// **A deck's measured height along its length**, in the tile-local frame the
/// deck is meshed in (`cartopolis_geo::deck_profile`), and where NAP is in the
/// world.
pub struct DeckShape {
    profile: cartopolis_geo::deck_profile::DeckProfile,
    /// World `y` of NAP 0 — minus the field's datum.
    offset: f32,
}

/// Tile-local metres → RD, linearised at one point of a deck — for reading the
/// survey's RD patches at a deck's own points (`DeckProfile::fit_over`). Over a
/// deck's few hundred metres the projection is linear to well under a centimetre.
#[derive(Clone, Copy, Debug)]
struct LocalToRd {
    /// The local point it was fitted at, and RD there.
    at: [f32; 2],
    rd0: [f64; 2],
    /// RD metres per tile-local metre along x and along z.
    ex: [f64; 2],
    ez: [f64; 2],
}

impl LocalToRd {
    fn fit(at: [f32; 2], to_rd: impl Fn([f32; 2]) -> [f64; 2]) -> Self {
        const STEP: f32 = 10.0;
        let rd0 = to_rd(at);
        let (x, z) = (to_rd([at[0] + STEP, at[1]]), to_rd([at[0], at[1] + STEP]));
        let s = f64::from(STEP);
        Self {
            at,
            rd0,
            ex: [(x[0] - rd0[0]) / s, (x[1] - rd0[1]) / s],
            ez: [(z[0] - rd0[0]) / s, (z[1] - rd0[1]) / s],
        }
    }

    fn rd(&self, local: [f32; 2]) -> [f64; 2] {
        let (dx, dz) = (
            f64::from(local[0] - self.at[0]),
            f64::from(local[1] - self.at[1]),
        );
        [
            self.rd0[0] + dx * self.ex[0] + dz * self.ez[0],
            self.rd0[1] + dx * self.ex[1] + dz * self.ez[1],
        ]
    }
}

impl DeckShape {
    #[must_use]
    pub fn nap_at(&self, local: [f32; 2]) -> f32 {
        self.profile.at([f64::from(local[0]), f64::from(local[1])])
    }

    #[must_use]
    pub fn world_y(&self, local: [f32; 2]) -> f32 {
        self.nap_at(local) + self.offset
    }
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
    /// over the structure less its DTM around it (`deck_measure`), which does not
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
            if let Some(shape) = &p.shape {
                for h in shape.profile.heights() {
                    eat(h.to_bits());
                }
            }
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

    /// How many surveyed piers a cell stood under its decks (`add_piers`).
    fn set_cell_piers(&mut self, cell: Cell, piers: usize) {
        if piers == 0 {
            self.piers.remove(&cell);
        } else {
            self.piers.insert(cell, piers);
        }
    }

    /// Decks standing along a measured profile rather than at one height.
    #[must_use]
    pub fn profiled(&self) -> usize {
        self.plates
            .values()
            .flatten()
            .filter(|p| p.shape.is_some())
            .count()
    }

    /// Surveyed piers stood under the streamed decks.
    #[must_use]
    pub fn piers(&self) -> usize {
        self.piers.values().sum()
    }

    /// Drop one cell's decks (cell retired).
    pub fn remove_cell(&mut self, cell: &Cell) {
        self.piers.remove(cell);
        self.digests.remove(cell);
        if self.plates.remove(cell).is_some() {
            self.revision = self.revision.wrapping_add(1);
        }
    }

    /// Drop every deck (re-anchor, or the source stopped being a vector one).
    pub fn clear(&mut self) {
        self.piers.clear();
        if !self.plates.is_empty() {
            self.plates.clear();
            self.revision = self.revision.wrapping_add(1);
        }
    }

    /// How well the raised decks meet their approaches: `(decks, decks with no
    /// approach, largest step, mean step)`, a step being the deck's drawn top less
    /// the ground three metres out along each approach.
    ///
    /// **Because an embankment cannot be seen to be right.** 150 m of ground
    /// climbing to a viaduct reads as ground from any angle that also shows the
    /// viaduct, and the bridge research of 2026-09-23 found every view in the
    /// manifest too small to show an 8.7 m cliff. This is the number instead.
    #[must_use]
    pub fn approach_steps(
        &self,
        terrain: &crate::systems::terrain::TerrainField,
    ) -> (usize, usize, f32, f32) {
        let (mut decks, mut bare, mut max, mut sum, mut n) =
            (0usize, 0usize, 0.0f32, 0.0f32, 0usize);
        for plate in self.plates.values().flatten() {
            decks += 1;
            if plate.approaches.is_empty() {
                bare += 1;
                continue;
            }
            for run in &plate.approaches {
                // A stub under a metre is where the tile border cut a deck that
                // carries on in the next tile, not an abutment: the ground three
                // metres past it is under the same structure, and measured it
                // reads as an 8.8 m cliff that is not there (2026-09-24).
                let len: f32 = run
                    .windows(2)
                    .map(|w| (w[1][0] - w[0][0]).hypot(w[1][1] - w[0][1]))
                    .sum();
                if len < 1.0 {
                    continue;
                }
                // Past the deck's own outline, which is often wider than the road
                // and under which the ground is left alone on purpose: the step is
                // where the embankment meets the structure, not under it.
                // Stepped in half metres, not by vertex: an approach's segments
                // run to a hundred metres, and skipping one whole put the probe
                // at natural ground far down the road.
                let mut clear = 0.0f32;
                while clear < len
                    && plate
                        .deck
                        .contains(crate::systems::map_geometry::point_along(run, clear))
                {
                    clear += 0.5;
                }
                let edge = crate::systems::map_geometry::point_along(run, clear);
                let out = crate::systems::map_geometry::point_along(run, clear + APPROACH_PROBE_M);
                // A run that never leaves the deck — a slip road inside a wide
                // viaduct's outline, cut by the tile's edge — has no ground end
                // to measure: probed at its end it reads the ground under the
                // deck (a false 6.8 m at Laan 1940–1945, 2026-09-24).
                if clear >= len || plate.deck.contains(out) {
                    continue;
                }
                let ground = terrain.at(plate.world.x + out[0], plate.world.y + out[1]);
                // The deck's top where this approach leaves it: a sloped deck
                // meets each approach at its own end's height.
                let step = (plate.world_top_at(edge) - ground).abs();
                max = max.max(step);
                sum += step;
                n += 1;
            }
        }
        (decks, bare, max, if n > 0 { sum / n as f32 } else { 0.0 })
    }

    /// The drawn top of the tallest deck over `p`, in world `y`, or `None` off
    /// every deck. What a car on a bridge-flagged line stands on: the height the
    /// deck was *drawn* at, which is the one number that cannot disagree with it.
    #[must_use]
    pub fn top_at(&self, p: Vec2) -> Option<f32> {
        self.plates
            .values()
            .flatten()
            .filter_map(|plate| {
                let local = [p.x - plate.world.x, p.y - plate.world.y];
                plate
                    .deck
                    .contains(local)
                    .then(|| plate.world_top_at(local))
            })
            .reduce(f32::max)
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
            let local = [p.x - plate.world.x, p.y - plate.world.y];
            if !plate.deck.contains(local) {
                continue;
            }
            // The clearance is the median's; a sloped deck stands that much
            // higher or lower here than at its median.
            let here = plate.height + plate.world_top_at(local) - plate.world_top;
            best = best.max(here);
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
    pub fn grade_masks(&self, n: usize, half: f32) -> cartopolis_geo::terrain_grade::DeckMasks {
        let plates: Vec<cartopolis_geo::terrain_grade::DeckPlate<'_>> = self
            .plates
            .values()
            .flatten()
            .map(|p| cartopolis_geo::terrain_grade::DeckPlate {
                deck: &p.deck,
                world: p.world.to_array(),
                height: p.height,
                top: p.top_nap,
                profile: p.shape.as_ref().map(|s| &s.profile),
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
    /// What each live cell's ground was when it was built
    /// (`TerrainField::cell_digest`).
    built_digest: HashMap<Cell, u64>,
    /// Live cells whose flat ground is drawn and **complete** — the vector sheets
    /// plus `gap_fill` over whatever they leave — so the basemap under them is
    /// never seen and must not be drawn (`map_stream::update_tile_occlusion`).
    /// A cell leaves the set the moment it is retired, which is what puts the
    /// basemap back under a cell being rebuilt rather than leaving a hole.
    pub complete: HashSet<Cell>,
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
    /// Surveyed-coverage revisions these cells were built against — the BGT's
    /// (`surveyed::coverage_revision`) and the crop register's
    /// (`crops::coverage_revision`), each 0 while its manifest is in flight and
    /// counting up with each different answer. Surveyed trees, lampposts and crop
    /// parcels are merged into these cells' meshes, so a cell built during either
    /// gap has none of them and nothing would revisit it. Each moves at most
    /// twice per process: the copy kept from the last launch, then the network if
    /// it disagrees (`coverage`).
    bgt_revision: (u32, u32),
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
            built_digest: HashMap::new(),
            complete: HashSet::new(),
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
            bgt_revision: (0, 0),
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
    /// The cell's surveyed piers, outer rings tile-local.
    piers: Vec<Vec<[f32; 2]>>,
    /// Cell origin in world metres — the same north-west corner the cell's other
    /// meshes are placed at.
    world: Vec2,
    /// Where each deck is in RD, and which pair of patches measures it. Empty when
    /// `known` is set.
    clusters: Vec<DeckCluster>,
    boxes: Vec<DeckBox>,
    /// Every deck's height, by the same index as `decks`, when atlas measured
    /// them all (`terrain_tiles::match_atlas_decks`).
    known: Option<Vec<DeckMeasure>>,
}

/// One deck's footprint in RD: its outline, and the box around it the ground
/// level is read from.
struct DeckBox {
    rings: Vec<Vec<[f64; 2]>>,
    min: [f64; 2],
    max: [f64; 2],
    /// How the deck's tile-local frame maps into RD, for reading a profile
    /// fitted there back at the mesh's vertices.
    frame: LocalToRd,
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
            frame: LocalToRd::fit(
                [
                    (deck.min[0] + deck.max[0]) * 0.5,
                    (deck.min[1] + deck.max[1]) * 0.5,
                ],
                &to_rd,
            ),
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
    urgency: crate::platform::workers::Urgency,
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
            // **Each input is its own task, so the downloads overlap.** Natively
            // `http::fetch` is a blocking `ureq` call with no await point in it, so
            // awaiting these one after another — or `zip`ping them, which is only
            // presentation there — ran up to nine requests end to end per cell, a
            // round trip each before the mesh could start. A spawned task parks an
            // io thread of its own, and awaiting its handle parks nothing. On wasm
            // the fetches were already concurrent and this changes nothing. What
            // bounds the overlap is the io pool's width (`lib.rs`, at least 16).
            let io = IoTaskPool::get();
            let key = TileKey::new(cell.0, cell.1, GEOMETRY_ZOOM);
            let tile_task = io.spawn(async move { load_tile_image(&key, &template).await });
            // The cell's surveyed BGT objects travel with the job for the reason
            // `building_facts` gives about `lod22`: they end up merged into this
            // cell's meshes, so they have to be in hand on the worker that builds
            // them rather than arriving later on a stream of their own. Empty
            // outside the coverage, which is everywhere the host has not run
            // atlas's `pipeline/bgt.rs`.
            let surveyed_task = io.spawn(crate::systems::surveyed::load_surveyed_cell(cell.0, cell.1));
            // …and, where the city this cell falls in publishes a tree register
            // of its own, that register — which supersedes the BGT's trees below
            // rather than joining them (`systems::municipal` says why). Empty
            // everywhere no city in `municipal::REGISTERS` reaches.
            //
            // Gated on `wants.trees` rather than filtered afterwards, unlike the
            // BGT cell above: this one reaches a *city's* server, not ours, and a
            // dense cell of it is 1.3 MB. Asking a third party for a layer that
            // is switched off is the one cost here worth not paying.
            let registered_task = (wants.trees || wants.furniture)
                .then(|| io.spawn(crate::systems::municipal::load_registered_cell(cell.0, cell.1)));
            // …and its surveyed paving, in a file of its own because it is by far
            // the heavier half.
            let paving_task = io.spawn(crate::systems::surveyed::load_surveyed_paving(cell.0, cell.1));
            // …and the paving's colours as the aerial photo measured them, which
            // ride in the roofs file (atlas's `roofs` step measures both).
            let measured_task = io.spawn(crate::systems::roof_colours::load_roof_cell(cell.0, cell.1));
            // …and the cell's registered crop parcels, which ride along for the
            // same reason: they are merged into this cell's meshes, and the
            // arable ones are a keep-out the grass area is built against. Empty
            // outside the coverage, i.e. wherever atlas's `brp` dataset has not
            // been run.
            let crop_task = io.spawn(crate::systems::crops::load_crop_cell(cell.0, cell.1));
            let decks_task = io.spawn(crate::systems::terrain_tiles::load_atlas_decks(cell.0, cell.1));
            let roads_task = io.spawn(crate::systems::terrain_tiles::load_road_heights(cell.0, cell.1));
            // …and the registered houseboat berths, which ride in the BAG's places
            // file (shared with `building_facts`, which fetches it anyway).
            let berths_task = wants
                .furniture
                .then(|| io.spawn(crate::systems::building_facts::load_berths(cell.0, cell.1)));
            // The marks and crossings below decide whether to ask at all from the
            // tile body, so they start once it is here; everything else is
            // already on its way.
            let fetched = tile_task.await;
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
            // These are the only two requests in this task that reach a host we do
            // not own, so each is its own task too: end to end, a cell's ground
            // waited on one third party's answer twice over (#267). What bounds
            // the wait is `http::OPTIONAL_LAYER_TIMEOUT` on each request plus each
            // service's breaker.
            let has_water = fetched
                .as_ref()
                .is_ok_and(|bytes| vector_tiles::body_mentions_water(bytes));
            let has_rail = fetched
                .as_ref()
                .is_ok_and(|bytes| vector_tiles::body_mentions_rail(bytes));
            let (mark_cell, crossing_cell) = if wants.furniture {
                let marks = io.spawn(crate::systems::nav_marks::load_marks_cell(cell.0, cell.1, has_water));
                let crossings =
                    io.spawn(crate::systems::crossings::load_crossings_cell(cell.0, cell.1, has_rail));
                (marks.await, crossings.await)
            } else {
                (
                    crate::systems::nav_marks::MarkCell::default(),
                    crate::systems::crossings::CrossingCell::default(),
                )
            };
            let surveyed = surveyed_task.await;
            let registered = match registered_task {
                Some(task) => task.await,
                None => crate::systems::municipal::RegisteredCell::default(),
            };
            let paving = paving_task.await;
            let measured = measured_task.await;
            let crop_cell = crop_task.await;
            let atlas_decks = decks_task.await;
            let road_heights = roads_task.await;
            let berths = match berths_task {
                Some(task) => task.await,
                None => Vec::new(),
            };
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
            crate::platform::workers::submit_heavy_ranked("surfaces", urgency, Box::new(move || {
                let mut lap = Lap::new();
                let mut shore = None;
                let mut grass = None;
                let mut decks = Vec::new();
                let mut deck_roads = Vec::new();
                let mut levels: Option<Arc<cartopolis_geo::road_levels::LevelMap>> = None;
                let mut approaches = Vec::new();
                // Needs no tile: the piers are the register's, and only become
                // anything once a deck over them is measured.
                let piers =
                    crate::systems::surveyed::surveyed_piers(&surveyed, cell.0, cell.1, tile_m);
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
                // Meshes placed at their own heights rather than draped on the
                // ground — the moored boats — joined to the cell after the drape.
                let mut floating: Vec<vector_tiles::SurfaceMesh> = Vec::new();
                let outcome = match parsed {
                    Ok(tile) => {
                    // The streets' own heights, where atlas solved them: matched
                    // to this tile's runs by name, which both sides derive from
                    // the same tile.
                    let runs = cartopolis_geo::road_levels::street_runs(&tile, tile_m);
                    if let Some(heights) = road_heights.as_ref() {
                        let mut map = cartopolis_geo::road_levels::LevelMap::default();
                        for run in &runs {
                            if let Some(ys) = heights.runs.get(&run.key) {
                                map.add(run.level, run.half_w, run.points.clone(), ys.clone());
                            }
                        }
                        if !map.is_empty() {
                            levels = Some(Arc::new(map));
                        }
                    }
                    match vector_tiles::tessellate_surfaces(&tile, tile_m, mesh_bytes) {
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
                                &paving, &measured, cell.0, cell.1, tile_m,
                            );
                            // How much of the ground the survey draws: nothing,
                            // the paths, or the local streets too — the last only
                            // where the paving file carries carriageways, so a
                            // cell surveyed before atlas took them keeps its
                            // ribbons.
                            let cover = if paved.iter().any(|a| {
                                matches!(
                                    a.kind,
                                    crate::systems::paving::PavingKind::Street
                                        | crate::systems::paving::PavingKind::LivingStreet
                                )
                            }) {
                                cartopolis_geo::road_texture::SurveyedCover::Streets
                            } else if paved.is_empty() {
                                cartopolis_geo::road_texture::SurveyedCover::Nothing
                            } else {
                                cartopolis_geo::road_texture::SurveyedCover::Paths
                            };
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
                            // Held back past the road cut below and partitioned
                            // against the whole finished ground in one pass of
                            // its own (`crops::cut_into_ground`) once the roads
                            // and the paving are in.
                            let mut crop_meshes = cartopolis_geo::crops::build_crop_meshes(
                                &parcels, mesh_bytes,
                            );
                            lap.mark("paving");
                            match vector_tiles::tessellate_roads_surveyed(
                                &tile,
                                tile_m,
                                &mut decks,
                                mesh_bytes,
                                cover,
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
                                    // …and from under the paving, for the same
                                    // reason (`paving::cut_ground_under_paving`).
                                    cartopolis_geo::paving::cut_ground_under_paving(
                                        &mut meshes,
                                        &paving_meshes,
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
                            cartopolis_geo::paving::cut_ground_under_paving(&mut meshes, &paving_meshes);
                            meshes.extend(std::mem::take(&mut paving_meshes));
                            // And the crop parcels rejoin here, on both paths,
                            // with the ground made a partition around them: the
                            // grass under a field comes out, and the field under
                            // a canal, a wood or a road does. Without it every
                            // field was two sheets over one piece of ground and
                            // drew in blotches once relief bent them apart.
                            // Position in this vector means nothing: a sheet's
                            // place in the ground stack is its group's rung.
                            cartopolis_geo::crops::cut_into_ground(&mut meshes, &mut crop_meshes);
                            meshes.extend(crop_meshes);
                            // **And the ground nothing covered.** Every flat sheet
                            // is in by now; what is left of the cell is a hole that
                            // used to show the basemap underneath, and the basemap
                            // is no longer drawn under a built cell
                            // (`cartopolis_geo::gap_fill`).
                            if let Some(fill) = cartopolis_geo::gap_fill::gap_fill(
                                &meshes,
                                [0.0, 0.0],
                                [tile_m, tile_m],
                                cartopolis_geo::vector_tiles::palette::RESIDENTIAL,
                            ) {
                                meshes.push(fill);
                            }
                            lap.mark("gap fill");
                            // The roads that are *not* on the road sheet, because
                            // they run below the ground (`cartopolis_geo::tunnels`):
                            // a tunnel, its ramps and the cuttings between, each a
                            // carriageway with walls and, under a lid, a roof — and
                            // a hole in the ground over the open stretches, which
                            // travels with the cell so the ground can be opened
                            // and the avatar can find the floor down there.
                            //
                            // **Where atlas solved the roads, it also decided
                            // which stretches are structures and published the land
                            // each runs through** (`road_levels::structures`), so
                            // the heights here are the drawn ones. Elsewhere — or
                            // from an atlas that predates the land — the tile's own
                            // runs are solved on flat land (`unmeasured_profiles`)
                            // and stood on the level of the land around each point.
                            let profiles: Vec<Option<cartopolis_geo::tunnels::Profile>> =
                                match (road_heights.as_ref(), ground.as_ref()) {
                                    (Some(h), Some((g, _)))
                                        if !h.land.is_empty()
                                            || !runs.iter().any(|r| r.level < 0) =>
                                    {
                                        use crate::systems::terrain::MeasuredGround as _;
                                        let datum = g.datum_m();
                                        runs.iter()
                                            .map(|r| {
                                                let (road, land) =
                                                    (h.runs.get(&r.key)?, h.land.get(&r.key)?);
                                                (road.len() == r.points.len()
                                                    && land.len() == r.points.len())
                                                .then(|| cartopolis_geo::tunnels::Profile {
                                                    road: road.iter().map(|y| y - datum).collect(),
                                                    land: land.iter().map(|y| y - datum).collect(),
                                                    walls: h
                                                        .walls
                                                        .get(&r.key)
                                                        .filter(|w| w.len() == r.points.len())
                                                        .cloned()
                                                        .unwrap_or_default(),
                                                })
                                            })
                                            .collect()
                                    }
                                    _ => {
                                        let mut profiles =
                                            cartopolis_geo::tunnels::unmeasured_profiles(&runs);
                                        if let Some((g, origin)) = ground.as_ref() {
                                            for (r, p) in runs.iter().zip(profiles.iter_mut()) {
                                                let Some(p) = p else { continue };
                                                for (k, q) in r.points.iter().enumerate() {
                                                    let land = g.lid_level(
                                                        origin.x + q[0],
                                                        origin.y + q[1],
                                                        cartopolis_geo::tunnels::DATUM_M,
                                                    );
                                                    p.road[k] += land;
                                                    p.land[k] += land;
                                                }
                                            }
                                        }
                                        profiles
                                    }
                                };
                            let tunnels = cartopolis_geo::tunnels::build_structures(
                                &runs,
                                &profiles,
                                mesh_bytes,
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
                                                !matches!(
                                                    l.kind,
                                                    furniture::LineKind::Wall | furniture::LineKind::Quay
                                                ) || !cartopolis_geo::tunnels::wall_of_a_sunken_road(
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
                                        // …and a raised bridge's parapets and
                                        // screens, which stood on the road under
                                        // the deck (`vector_tiles::wall_of_a_bridge`).
                                        // Raised only, and only where atlas has
                                        // already measured the decks: around a
                                        // canal bridge that lies flat the same
                                        // walls are the quay and the gardens, and
                                        // the rule took 209 of them out of the
                                        // city centre's cell (2026-09-27).
                                        let raised: Vec<usize> = atlas_decks
                                            .as_deref()
                                            .and_then(|a| {
                                                crate::systems::terrain_tiles::match_atlas_decks(
                                                    &decks, tile_m, a,
                                                )
                                            })
                                            .map(|m| {
                                                m.iter()
                                                    .enumerate()
                                                    .filter(|(_, d)| {
                                                        d.height
                                                            >= cartopolis_geo::terrain_grade::MIN_LIFT_M
                                                    })
                                                    .map(|(i, _)| i)
                                                    .collect()
                                            })
                                            .unwrap_or_default();
                                        if !raised.is_empty() {
                                            let raised_decks: Vec<_> =
                                                raised.iter().map(|&i| decks[i].clone()).collect();
                                            let raised_ramps: Vec<_> = approaches
                                                .iter()
                                                .filter(|a| raised.contains(&(a.deck as usize)))
                                                .cloned()
                                                .collect();
                                            let before = lines.len();
                                            lines.retain(|l| {
                                                !matches!(
                                                    l.kind,
                                                    furniture::LineKind::Wall | furniture::LineKind::Quay
                                                ) || !vector_tiles::wall_of_a_bridge(
                                                        &raised_decks,
                                                        &raised_ramps,
                                                        &l.points,
                                                        l.height_m,
                                                    )
                                            });
                                            if lines.len() < before {
                                                tracing::info!(
                                                    cell = ?cell,
                                                    dropped = before - lines.len(),
                                                    "surveyed walls of a bridge left to the deck"
                                                );
                                            }
                                        }
                                        cell_places.surveyed.2 = lines.len();
                                        // …and the kerbstones, standing along the
                                        // surveyed edge between a pavement and the
                                        // street (`paving_edges::kerb_lines`).
                                        // Meshed with the walls, never colliders:
                                        // a kerb is something you step over.
                                        lines.extend(
                                            cartopolis_geo::paving_edges::kerb_lines(&paved)
                                                .into_iter()
                                                .map(|points| furniture::SurveyedLine {
                                                    points,
                                                    kind: furniture::LineKind::Kerb,
                                                    height_m: None,
                                                }),
                                        );
                                        // A measured wall stands on its lower
                                        // side (`build_line_meshes_footed`): the
                                        // ground or water a metre or two either
                                        // side, against the land at the wall that
                                        // `stand_on_terrain` will put it on.
                                        let foot = |mid: [f32; 2], dir: [f32; 2]| -> f32 {
                                            let Some((g, origin)) = ground.as_ref() else {
                                                return 0.0;
                                            };
                                            let at = |p: [f32; 2]| (origin.x + p[0], origin.y + p[1]);
                                            let (x, z) = at(mid);
                                            let base = g.land_at(x, z);
                                            let mut low = base;
                                            for d in [1.0, 2.5, WALL_FOOT_PROBE_M]
                                                .into_iter()
                                                .flat_map(|d| [-d, d])
                                            {
                                                let (px, pz) = at([mid[0] - dir[1] * d, mid[1] + dir[0] * d]);
                                                // Ground only close by; out at the
                                                // far reach only water counts, or a
                                                // garden wall on a slope would sink.
                                                if d.abs() < WALL_FOOT_PROBE_M {
                                                    low = low.min(g.sunk_at(px, pz));
                                                }
                                                if let Some(w) = g.water_surface_at(px, pz) {
                                                    low = low.min(w);
                                                }
                                            }
                                            base - low
                                        };
                                        meshes.extend(furniture::build_line_meshes_footed(
                                            &lines, mesh_bytes, &foot,
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
                                        // **A boat floats**: built at the water's
                                        // surface under its own centre, and kept
                                        // out of the terrain drape below, which
                                        // would tip a hull up a sloping bank
                                        // (`build_furniture_meshes_lifted`).
                                        // Where no level is registered the water is
                                        // drawn at the median of its own outline
                                        // (`water_surface`'s fallback), so a boat is
                                        // floated on that same number — the carve's
                                        // floor sank yachts to their cabin roofs.
                                        let drawn_water = ground.as_ref().map(|(g, origin)| {
                                            g.level_over(
                                                meshes
                                                    .iter()
                                                    .filter(|m| m.group == SurfaceGroup::Water)
                                                    .flat_map(|m| m.positions.iter())
                                                    .map(|p| [origin.x + p[0], origin.y + p[2]]),
                                            )
                                        });
                                        let level = |b: &furniture::FurnitureInstance| -> f32 {
                                            let Some((g, origin)) = ground.as_ref() else {
                                                return 0.0;
                                            };
                                            let (x, z) = (origin.x + b.x, origin.y + b.z);
                                            g.water_surface_at(x, z)
                                                .or(drawn_water)
                                                .unwrap_or_else(|| g.sunk_at(x, z))
                                        };
                                        floating.extend(furniture::build_furniture_meshes_lifted(
                                            &items, wants.modelled, mesh_bytes, &level,
                                        ));
                                        // …and the marinas' yachts, along the
                                        // jetties, and the registered houseboats,
                                        // afloat the same way.
                                        let berth_points: Vec<([f32; 2], bool)> = berths
                                            .iter()
                                            .filter_map(|&((lat, lng), housed)| {
                                                crate::systems::surveyed::cell_local(
                                                    lat,
                                                    lng,
                                                    cell.0,
                                                    cell.1,
                                                    cartopolis_geo::formats::CACHE_ZOOM,
                                                    tile_m,
                                                )
                                                .map(|(x, z)| ([x, z], housed))
                                            })
                                            .collect();
                                        let berth_xy: Vec<[f32; 2]> =
                                            berth_points.iter().map(|b| b.0).collect();
                                        let mut afloat = moorings::extract_marina(
                                            &tile,
                                            tile_m,
                                            veg_seed(cell),
                                            &berth_xy,
                                        )
                                        .unwrap_or_default();
                                        afloat.extend(moorings::place_houseboats(
                                            &tile,
                                            tile_m,
                                            &berth_points,
                                            veg_seed(cell),
                                        ));
                                        floating.extend(furniture::build_furniture_meshes_lifted(
                                            &afloat, wants.modelled, mesh_bytes, &level,
                                        ));
                                        cell_places.vehicles.extend(afloat);
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
                                // …and the survey's own lawns: verges, grassed
                                // surfaces and the planted half of a garden.
                                // The tile's `land` layer rarely maps private
                                // ground, so these were drawn green and bare.
                                for a in &paved {
                                    if a.kind == crate::systems::paving::PavingKind::Verge
                                        || a.material
                                            == Some(crate::systems::paving::PavingMaterial::Grass)
                                    {
                                        area.add_surveyed(&a.rings, tile_m);
                                    }
                                }
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
                    }
                    }
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
                            levels.as_deref(),
                        );
                    }
                    if let Some(walls) = quay_walls(meshes, tile_m, *origin, ground) {
                        meshes.push(walls);
                    }
                    placed = true;
                }
                // The boats, at the heights they were built at.
                if let Some(meshes) = outcome.as_mut() {
                    meshes.append(&mut floating);
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
                            piers,
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

impl crate::systems::stream_watch::StreamBusy for MapSurfaces {
    const NAME: &'static str = "surfaces";
    fn stream_busy(&self) -> bool {
        self.streaming_busy()
    }
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
        self.built_digest.clear();
        self.complete.clear();
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
    surfaces.built_digest.remove(&c);
    surfaces.complete.remove(&c);
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
    // those are merged into the cell's own meshes. Unlike the terrain it changes
    // at most twice — the copy kept from the last launch answers at once, and only
    // a network answer that differs from it moves it again.
    // The crop coverage is folded in beside it — same shape, same one-shot answer,
    // and the parcels are baked into the same meshes — so the two share a rebuild
    // rather than costing one each. One pair of revisions, as `lod22` keeps for the
    // BAG, LoD3 and roofs manifests.
    let bgt_revision = (
        crate::systems::surveyed::coverage_revision(),
        crate::systems::crops::coverage_revision(),
    );
    // **A new field alone retires only the cells built on an older one** (step 2c
    // of `level-of-detail-tile-tree.md`). The ring used to go whole, which also threw
    // away the in-flight cells waiting for this very field and built them twice.
    // A cell arriving later on an old field is dropped at arrival for the same
    // reason (`built_rev`).
    //
    // **And only the cells whose ground the new field changed.** A field that
    // follows the camera (`terrain::FOLLOW_M`) is a new revision every time it
    // moves, over ground most of the ring already stands on unchanged; the cell's
    // digest of the ground and water under it says which (`TerrainField::cell_digest`).
    if surfaces.bgt_revision == bgt_revision && surfaces.terrain_revision != terrain.revision {
        let tile_m = tile_to_meters(GEOMETRY_ZOOM, clat) as f32;
        let (fcx, fcy) = lat_lng_to_tile_frac(clat, clng, GEOMETRY_ZOOM);
        let stale: Vec<Cell> = surfaces
            .built_rev
            .iter()
            .filter(|(_, rev)| **rev != terrain.revision)
            .map(|(c, _)| *c)
            .collect();
        let mut old = Vec::new();
        let field: &crate::systems::terrain::TerrainField = &terrain;
        let digests = crate::systems::terrain::cell_digests(
            field,
            stale.iter().map(|&c| {
                let wx = ((c.0 as f64 - fcx) * f64::from(tile_m)) as f32;
                let wz = ((c.1 as f64 - fcy) * f64::from(tile_m)) as f32;
                (c, Vec2::new(wx, wz), Vec2::new(wx + tile_m, wz + tile_m))
            }),
            true,
        );
        for (c, now) in digests {
            if surfaces.built_digest.get(&c) == Some(&now) {
                surfaces.built_rev.insert(c, terrain.revision);
            } else {
                old.push(c);
            }
        }
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
            bgt_rev = ?bgt_revision,
            was_bgt_rev = ?surfaces.bgt_revision,
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
    let coverage_known = (bgt_revision.0 != 0 && bgt_revision.1 != 0)
        || since.elapsed().as_secs_f32() > COVERAGE_WAIT_S;
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
            crate::platform::workers::urgency_for_ring(crate::platform::fetch_gate::ring_distance(
                c, ccx, ccy,
            )),
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
    let cap = crate::quality::budget().per_frame(MAX_BUILDS_PER_FRAME);
    let ready =
        crate::systems::stream_ledger::take_admitted(&mut surfaces.stream, cap, |(outcome, ..)| {
            outcome.as_ref().map_or(0, |surface_meshes| {
                surface_meshes
                    .iter()
                    .map(|sm| {
                        crate::systems::vector_tiles::mesh_upload_bytes(
                            sm.positions.len(),
                            sm.indices.len(),
                        )
                    })
                    .sum()
            })
        });
    for (
        c,
        (
            outcome,
            shore,
            decks,
            deck_roads,
            approaches,
            piers,
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
        surfaces.built_digest.insert(
            c,
            terrain.cell_digest(Vec2::new(wx, wz), Vec2::new(wx + tile_m, wz + tile_m), true),
        );

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

        // A cell is complete ground when it draws flat sheets at all: the worker
        // always closes those with `gap_fill`, and drops them together above
        // `FLAT_GROUND_OFF_M`, where the basemap is the ground again.
        let complete = surface_meshes
            .iter()
            .any(|m| is_flat_ground(m.group) && m.group.layer().is_sheet());
        let mut entities = Vec::with_capacity(surface_meshes.len());
        for sm in surface_meshes {
            let stand = if placed {
                Stand::Placed
            } else {
                Stand::Terrain(&terrain, &stores.tunnels)
            };
            let group = sm.group;
            let mesh = meshes.add(build_mesh_owned(sm, wx, wz, stand));
            let world = Vec3::new(wx, group.y_offset(), wz);
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
            let id = if let (SurfaceGroup::Water, Some(water)) = (group, &water_material) {
                // Water gets the wave material (`systems::water`), which owns its
                // albedo, roughness and depth bias — and this cell's shore field.
                commands
                    .spawn((placement, MeshMaterial3d(water.clone())))
                    .id()
            } else if matches!(
                group,
                SurfaceGroup::Road(_) | SurfaceGroup::RoadCasing(_) | SurfaceGroup::TunnelRoad(_)
            ) {
                // Roads and casings carry their place in the depth ladder per
                // vertex (`systems::road_depth`), which is what settles the
                // coplanar fight at a junction at *every* range. Everything else
                // about the material is the standard road recipe.
                let material = surfaces
                    .road_materials
                    .entry(group)
                    .or_insert_with(|| {
                        let base = road_base_material(group, &mut mats.images);
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
                    wants_ground_finish(group).then(|| surfaces.ground_finish(&mut mats.images));
                let material = surfaces
                    .materials
                    .entry(group)
                    .or_insert_with(|| {
                        let [r, g, b] = group.base_color();
                        mats.plain.add(GroundMaterial {
                            base: StandardMaterial {
                                base_color: Color::srgb(r, g, b),
                                perceptual_roughness: group.perceptual_roughness(),
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
                                depth_bias: group.depth_bias(),
                                ..default()
                            },
                            extension: ground_detail_for(group),
                        })
                    })
                    .clone();
                commands.spawn((placement, MeshMaterial3d(material))).id()
            };
            if !casts_shadow(group) {
                commands.entity(id).insert(bevy::light::NotShadowCaster);
            }
            if matches!(group, SurfaceGroup::Canopy | SurfaceGroup::Trunk) {
                // Read the *current* ramp at spawn, the way new globe tiles read
                // the current albedo: a cell streaming in above the fade band must
                // not flash a full forest for the frame before the ramp catches up.
                commands
                    .entity(id)
                    .insert((VegetationSurface, veg_lod.visibility()));
            }
            if group == SurfaceGroup::Furniture {
                // Same contract, its own (much lower) band.
                commands
                    .entity(id)
                    .insert((FurnitureSurface, furniture_lod.visibility()));
            }
            if matches!(
                group,
                SurfaceGroup::TunnelRoad(_) | SurfaceGroup::TunnelStructure
            ) {
                commands.entity(id).insert(crate::components::TunnelSurface);
            }
            if !debug_hide_tokens().is_empty() {
                commands.entity(id).insert(DebugSurfaceGroup(group));
            }
            entities.push(id);
        }
        tracing::debug!(cell = ?c, groups = entities.len(), "surface cell built");
        surfaces.loaded.insert(c, entities);
        if complete {
            surfaces.complete.insert(c);
        } else {
            surfaces.complete.remove(&c);
        }
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
                    piers,
                    world: Vec2::new(wx, wz),
                    clusters,
                    boxes,
                    known,
                },
            );
        }
    }
}

/// The words in `CARTO_DBG_HIDE` (comma-separated, lower-case), read once.
///
/// **A bisecting switch for "what is that?"**, built after four guesses in a row
/// about one pale block in a tunnel mouth — a building, the lid fill, a bridge
/// deck, a surveyed wall — were each disproved by a capture (2026-09-23). A word
/// hides a whole layer: `osm` (OSM buildings), `lod22` (3DBAG buildings),
/// `basemap` (clipmap tiles), `tunnel` (tunnel decks and walls), `surface`
/// (every vector ground mesh), or any fragment of a ground group's name as
/// `{:?}` prints it (`grass`, `road`, `built`, `furniture` …), and `!word` hides
/// every ground group whose name does *not* contain it. Unset, it
/// costs one empty check a frame.
fn debug_hide_tokens() -> &'static [String] {
    static TOKENS: std::sync::OnceLock<Vec<String>> = std::sync::OnceLock::new();
    TOKENS.get_or_init(|| {
        std::env::var("CARTO_DBG_HIDE")
            .map(|s| {
                s.split(',')
                    .map(|t| t.trim().to_ascii_lowercase())
                    .filter(|t| !t.is_empty())
                    .collect()
            })
            .unwrap_or_default()
    })
}

/// Which ground group a surface entity was built from — only attached while
/// `CARTO_DBG_HIDE` is set.
#[derive(Component)]
pub struct DebugSurfaceGroup(SurfaceGroup);

/// Hide every layer `CARTO_DBG_HIDE` names (see [`debug_hide_tokens`]).
#[allow(clippy::type_complexity)]
pub fn debug_hide_layers(
    mut vis: Query<(
        &mut Visibility,
        Option<&DebugSurfaceGroup>,
        Has<MapSurface>,
        Has<crate::components::OsmBuildingMesh>,
        Has<crate::systems::lod22::Lod22Mesh>,
        Has<crate::systems::map_stream::StreamTile>,
        Has<crate::components::TunnelSurface>,
    )>,
) {
    let tokens = debug_hide_tokens();
    if tokens.is_empty() {
        return;
    }
    let has = |w: &str| tokens.iter().any(|t| t == w);
    for (mut v, group, surface, osm, lod22, tile, tunnel) in &mut vis {
        let hide = (osm && has("osm"))
            || (lod22 && has("lod22"))
            || (tile && has("basemap"))
            || (tunnel && has("tunnel"))
            || (surface && has("surface"))
            || group.is_some_and(|g| {
                let name = format!("{:?}", g.0).to_ascii_lowercase();
                tokens.iter().any(|t| match t.strip_prefix('!') {
                    // `!word`: every ground group *except* those naming it, so a
                    // layer can be looked at on its own (`!tunnel`).
                    Some(keep) => !name.contains(keep),
                    None => name.contains(t.as_str()),
                })
            });
        if hide && *v != Visibility::Hidden {
            *v = Visibility::Hidden;
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
        // Atlas's measurements where it published every deck of this cell, the
        // cell's own otherwise — the same rule, the same sampling
        // (`cartopolis_geo::deck_profile::DeckMeasure`).
        let measures: Vec<Option<DeckMeasure>> = match &pending.known {
            Some(known) => known.iter().cloned().map(Some).collect(),
            None => pending
                .boxes
                .iter()
                .zip(&pending.decks)
                .map(|(b, d)| deck_measure(&height, &pending.clusters, b, d))
                .collect(),
        };
        let heights: Vec<f32> = measures
            .iter()
            .map(|m| m.as_ref().map_or(0.0, |m| m.height))
            .collect();
        let tops: Vec<Option<f32>> = measures
            .iter()
            .map(|m| m.as_ref().and_then(|m| m.top))
            .collect();
        // **Each deck along its own measured length**, where the survey gives it
        // one: in world `y` through the field's datum, which is why a field that
        // has not landed yet leaves every deck at its single height.
        let shapes: Vec<Option<Arc<DeckShape>>> = measures
            .into_iter()
            .map(|m| {
                let profile = m?.profile?;
                terrain.is_ready().then(|| {
                    Arc::new(DeckShape {
                        profile,
                        offset: -terrain.datum_m,
                    })
                })
            })
            .collect();

        let Some(pending) = surfaces.pending_decks.remove(&c) else {
            continue;
        };
        let cell_world = pending.world;
        // The one terrain height the cell's mesh is placed on — read here rather
        // than left to `build_mesh`, because the carriageways cut out of these
        // slabs have to be lifted onto *this* number and not onto the field's
        // answer over their own narrower footprint.
        let deck_level = if terrain.is_ready() {
            terrain.level_over(
                pending
                    .decks
                    .iter()
                    .flat_map(|d| d.positions.iter())
                    .map(|p| [cell_world.x + p[0], cell_world.y + p[2]]),
            )
        } else {
            0.0
        };
        // **Each deck at its own measured top**, as an offset from that shared
        // level; the clearance where the survey has no top for it.
        let levels: Vec<f32> = heights
            .iter()
            .zip(&tops)
            .map(|(&h, top)| match top {
                Some(t) if terrain.is_ready() => (t - terrain.datum_m) - deck_level,
                _ => h,
            })
            .collect();
        // The ground under the deck in the slab's own frame; with no field yet,
        // nothing is higher than a fascia.
        let ground = |x: f32, z: f32| {
            if terrain.is_ready() {
                terrain.at(cell_world.x + x, cell_world.y + z) - deck_level
            } else {
                f32::NEG_INFINITY
            }
        };
        let mut mesh = build_deck_mesh(
            &pending.decks,
            &levels,
            &heights,
            &shapes,
            deck_level,
            &ground,
        );
        let piers = if terrain.is_ready() {
            add_piers(
                &mut mesh,
                &pending.piers,
                &pending.decks,
                &levels,
                &heights,
                &shapes,
                deck_level,
                |p| terrain.at(cell_world.x + p[0], cell_world.y + p[1]),
            )
        } else {
            0
        };
        plates.set_cell_piers(c, piers);
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
                    top_nap: tops[i],
                    world_top: deck_level + levels[i],
                    shape: shapes[i].clone(),
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
                        // On `deck_level` itself rather than on the field again: the
                        // field's answer is a function of the points it is asked
                        // over, and this mesh's (fascia, and a profiled deck's
                        // subdivision) are not the outline's that `deck_level` and
                        // the carriageways on the slab were placed with.
                        Mesh3d(meshes.add(build_mesh(
                            &mesh,
                            cell_world.x,
                            cell_world.y,
                            Stand::Lifted(deck_level),
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
            let Some(&h) = levels.get(*di as usize) else {
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
                            match shapes.get(*di as usize).and_then(Option::as_deref) {
                                Some(shape) => Stand::Deck(shape),
                                None => Stand::Lifted(deck_level + h),
                            },
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

/// How one deck measures against the survey: its clearance over the ground
/// around it, its median top, and its profile from the same surface samples.
///
/// `None` for "no data here" as much as for a bridge that really is at street
/// level — a Dutch canal bridge is exactly that, and telling the two apart would
/// buy nothing, because both want a flat deck.
fn deck_measure(
    layer: &HeightLayer,
    clusters: &[DeckCluster],
    deck: &DeckBox,
    bridge: &BridgeDeck,
) -> Option<DeckMeasure> {
    let cluster = deck.cluster.and_then(|i| clusters.get(i))?;
    let (surface, ground) = cluster.requests();
    let surface = layer.get(&surface);
    let deck_top = surface.and_then(|p| p.median_where(|rd| inside_rings(&deck.rings, rd)));
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
    // **The top is kept, not only the difference.** The clearance says whether
    // the deck is a structure at all; the top says where it *is*. Using ground +
    // clearance for that instead counted the embankment twice wherever the survey's
    // ground already held it — approaches graded 4–5 m above the deck they lead to
    // (Emmaviaduct north, 11.4–12.8 against a 7.81 top) — and one median ground for
    // all of a tile's decks floated a small one 3.4 m over its canal and buried
    // another (bridge research, 2026-09-23).
    let (Some(top), Some(ground)) = (deck_top, ground) else {
        return None;
    };
    // The same rule atlas measures its decks by, for the ground it grades.
    let height = cartopolis_geo::terrain_grade::deck_clearance(top, ground);
    if height <= 0.0 {
        return None;
    }
    // Only a lifted deck gets a profile: one at street level is road surface,
    // and the ground under it already has the slope. Read at the deck's own
    // tile-local points, as atlas reads it (`DeckProfile::fit_over`).
    let profile = surface.and_then(|patch| {
        cartopolis_geo::deck_profile::DeckProfile::fit_over(bridge, |local| {
            patch.at(deck.frame.rd(local))
        })
    });
    Some(DeckMeasure {
        height,
        top: Some(top),
        profile,
    })
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
/// `levels` is where each slab is drawn relative to the cell's level; `clearance`
/// is how far the structure stands over its own ground, which is what decides
/// whether it has a fascia at all.
fn build_deck_mesh(
    decks: &[BridgeDeck],
    levels: &[f32],
    clearance: &[f32],
    shapes: &[Option<Arc<DeckShape>>],
    deck_level: f32,
    ground: &dyn Fn(f32, f32) -> f32,
) -> SurfaceMesh {
    let mut out = deck_slab(&[], &[], &[]);
    for (i, deck) in decks.iter().enumerate() {
        let (Some(&level), Some(&lift)) = (levels.get(i), clearance.get(i)) else {
            continue;
        };
        let one = std::slice::from_ref(deck);
        let slab = match shapes.get(i).and_then(Option::as_deref) {
            None => deck_slab(one, &[level], &[lift]),
            // A profiled deck: built level at 0, subdivided so its slope has
            // vertices to follow, then stood on the profile. Fascia and surface
            // move together, so the slab keeps its thickness all the way along.
            Some(shape) => {
                let mut slab = deck_slab(one, &[0.0], &[lift]);
                let (mut uvs, mut colors, mut rungs) = (Vec::new(), Vec::new(), Vec::new());
                cartopolis_geo::terrain_refine::refine_for_terrain(
                    &mut slab.positions,
                    &mut slab.indices,
                    &mut uvs,
                    &mut slab.normals,
                    &mut colors,
                    &mut rungs,
                    DECK_PROFILE_EDGE_M,
                    None,
                );
                for p in &mut slab.positions {
                    p[1] += shape.world_y([p[0], p[2]]) - deck_level;
                }
                slab
            }
        };
        // **The fascia stops at the ground.** It hangs a slab's thickness below
        // the deck all the way round, ends included — and at an end the ground is
        // the abutment, held at the deck's top, so the fascia there is a band of
        // wall standing *on* the road where it meets the deck. Where a deck's end
        // is several carriageways cut square at different points, it was a
        // staircase of them across the joint (the Europaplein, 2026-09-27). Each
        // fascia vertex below the surface is lifted to the ground where the
        // ground is higher; buried, the face collapses to nothing.
        let mut slab = slab;
        for (p, n) in slab.positions.iter_mut().zip(&slab.normals) {
            if n[1].abs() > 0.5 {
                continue;
            }
            let surface = match shapes.get(i).and_then(Option::as_deref) {
                Some(shape) => shape.world_y([p[0], p[2]]) - deck_level,
                None => level,
            };
            if p[1] < surface - 1e-3 {
                p[1] = p[1].max(ground(p[0], p[2]).min(surface));
            }
        }
        let base = out.positions.len() as u32;
        out.positions.extend(slab.positions);
        out.normals.extend(slab.normals);
        out.indices.extend(slab.indices.iter().map(|i| base + i));
    }
    out
}

/// How far a pier is sunk below the ground it stands on, so a slope under it or
/// a few centimetres of disagreement between the field and the deck's own level
/// never shows a gap at its foot.
const PIER_FOOTING_M: f32 = 0.3;

/// **The piers under the decks**, as walls from the ground to the deck's
/// underside, appended to the cell's deck mesh — in its frame, i.e. relative to
/// `deck_level`.
///
/// A viaduct used to be a slab with a fascia and nothing under it. The BGT
/// surveys the piers (`overbruggingsdeel` of type `pijler`) as footprints on the
/// ground, and what they lack — how tall — is exactly what the deck over them
/// already knows. A pier is paired with the lowest lifted deck its centre is
/// under: at a stacked interchange that is the deck it holds up, the upper one
/// standing on piers of its own. A pier under no lifted deck of this cell is
/// left out rather than guessed at. `ground` is world `y` at a tile-local point.
///
/// Returns how many piers were stood.
#[allow(clippy::too_many_arguments)]
fn add_piers(
    mesh: &mut SurfaceMesh,
    piers: &[Vec<[f32; 2]>],
    decks: &[BridgeDeck],
    levels: &[f32],
    clearance: &[f32],
    shapes: &[Option<Arc<DeckShape>>],
    deck_level: f32,
    ground: impl Fn([f32; 2]) -> f32,
) -> usize {
    let mut stood = 0;
    for ring in piers {
        if ring.len() < 3 {
            continue;
        }
        let n = ring.len() as f32;
        let c = [
            ring.iter().map(|p| p[0]).sum::<f32>() / n,
            ring.iter().map(|p| p[1]).sum::<f32>() / n,
        ];
        let top_at = |i: usize| -> f32 {
            match shapes.get(i).and_then(Option::as_deref) {
                Some(shape) => shape.world_y(c),
                None => deck_level + levels[i],
            }
        };
        let Some(top) = decks
            .iter()
            .enumerate()
            .filter(|(i, d)| clearance.get(*i).is_some_and(|&h| h >= FASCIA_MIN_M) && d.contains(c))
            .map(|(i, _)| top_at(i))
            .reduce(f32::min)
        else {
            continue;
        };
        let underside = top - DECK_THICKNESS_M;
        let foot = ring
            .iter()
            .map(|p| ground(*p))
            .fold(f32::INFINITY, f32::min)
            - PIER_FOOTING_M;
        if !(underside - foot > PIER_FOOTING_M + 0.2) {
            continue;
        }
        let (y0, y1) = (foot - deck_level, underside - deck_level);
        for k in 0..ring.len() {
            let (a, b) = (ring[k], ring[(k + 1) % ring.len()]);
            let (dx, dz) = (b[0] - a[0], b[1] - a[1]);
            let len = dx.hypot(dz);
            if len < 1e-3 {
                continue;
            }
            // Outward from the pier's own centre, as the fascia is oriented:
            // cheaper and surer than trusting a surveyed ring's winding.
            let (mut nx, mut nz) = (-dz / len, dx / len);
            let mid = [(a[0] + b[0]) * 0.5 - c[0], (a[1] + b[1]) * 0.5 - c[1]];
            if nx * mid[0] + nz * mid[1] < 0.0 {
                (nx, nz) = (-nx, -nz);
            }
            let base = mesh.positions.len() as u32;
            for (p, y) in [(a, y1), (b, y1), (b, y0), (a, y0)] {
                mesh.positions.push([p[0], y, p[1]]);
                mesh.normals.push([nx, 0.0, nz]);
            }
            mesh.indices
                .extend([base, base + 1, base + 2, base, base + 2, base + 3]);
        }
        stood += 1;
    }
    stood
}

/// Longest edge a profiled deck's mesh keeps: under a station
/// (`deck_profile::STATION_M`) apart, so the drawn slope bends where the
/// measured one does.
const DECK_PROFILE_EDGE_M: f32 = 3.0;

/// Level slabs, each deck at its own level: the surface and, for a deck lifted
/// far enough to have one, the fascia down its sides.
fn deck_slab(decks: &[BridgeDeck], levels: &[f32], clearance: &[f32]) -> SurfaceMesh {
    let mut positions: Vec<[f32; 3]> = Vec::new();
    let mut normals: Vec<[f32; 3]> = Vec::new();
    let mut indices: Vec<u32> = Vec::new();

    for ((deck, &h), &lift) in decks.iter().zip(levels).zip(clearance) {
        // Surface.
        let base = positions.len() as u32;
        for p in &deck.positions {
            positions.push([p[0], h, p[2]]);
            normals.push([0.0, 1.0, 0.0]);
        }
        indices.extend(deck.indices.iter().map(|i| base + i));

        if lift < FASCIA_MIN_M {
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
            | SurfaceGroup::QuayWall
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

/// Whether a group's meshes go into the sun's shadow maps.
///
/// Only what stands up off the ground. A flat sheet cannot throw a shadow onto
/// anything — nothing it could fall on is underneath it — but each one is a
/// single mesh across a whole ~1.5 km cell, wider than every cascade, so it is
/// drawn into all four of them on every frame for nothing. The clipmap tiles under
/// them opted out for the same reason (`map_stream`). With the Height layer on, the
/// ground keeps its relief shading from its normals; what it gives up is a hill's
/// shadow over the next valley, which the clipmap never cast either.
#[must_use]
pub fn casts_shadow(group: SurfaceGroup) -> bool {
    match group {
        SurfaceGroup::BridgeDeck
        | SurfaceGroup::Canopy
        | SurfaceGroup::Trunk
        | SurfaceGroup::Furniture
        | SurfaceGroup::TunnelStructure
        | SurfaceGroup::QuayWall => true,
        SurfaceGroup::Water
        | SurfaceGroup::Forest
        | SurfaceGroup::Grass
        | SurfaceGroup::Sand
        | SurfaceGroup::Built
        | SurfaceGroup::Road(_)
        | SurfaceGroup::RoadCasing(_)
        | SurfaceGroup::Paving
        | SurfaceGroup::Crop
        | SurfaceGroup::TunnelRoad(_) => false,
    }
}

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
    /// Onto a deck's measured profile: [`Self::Lifted`] with a height per vertex,
    /// after subdividing so a long deck's slope has vertices to follow — a road
    /// ribbon across a straight 200 m viaduct is otherwise two triangles.
    Deck(&'a DeckShape),
    /// Already stood on the ground by the surface worker (`spawn_geometry_task`).
    Placed,
}

fn build_mesh(sm: &SurfaceMesh, wx: f32, wz: f32, stand: Stand<'_>) -> Mesh {
    build_mesh_owned(sm.clone(), wx, wz, stand)
}

/// [`build_mesh`] for a caller that is done with the tessellated mesh — every
/// surface cell as it lands.
///
/// **This is on the main thread, once per group per cell arrival**, and the
/// borrowing version cloned every vertex attribute of the group on the way in and
/// the positions a second time on the way out, only to read their length. A dense
/// cell's groups are hundreds of thousands of vertices, so a cell's arrival was a
/// few tens of megabytes of `memcpy` in one frame for nothing; moving the buffers
/// in costs nothing.
fn build_mesh_owned(sm: SurfaceMesh, wx: f32, wz: f32, stand: Stand<'_>) -> Mesh {
    use bevy::asset::RenderAssetUsages;
    use bevy::render::mesh::{Indices, PrimitiveTopology};

    let n = sm.positions.len();
    let group = sm.group;
    let mut positions = sm.positions;
    let mut indices = sm.indices;
    // Ground is flat *until the terrain field says otherwise*, so the common case is
    // a straight-up normal per vertex; only the vegetation meshes have volume and
    // bring their own.
    let mut normals: Vec<[f32; 3]> = if sm.normals.len() == n {
        sm.normals
    } else {
        Vec::new()
    };
    let mut uvs: Vec<[f32; 2]> = if sm.uvs.len() == n {
        sm.uvs
    } else {
        Vec::new()
    };
    let mut colors: Vec<[f32; 4]> = if sm.colors.len() == n {
        sm.colors
    } else {
        Vec::new()
    };
    // Road groups only — and it must survive the terrain refinement below, which
    // appends midpoint vertices and would otherwise leave the attribute shorter
    // than the positions it belongs to (a mesh bevy rejects at draw time).
    let mut rungs: Vec<f32> = if sm.rungs.len() == n {
        sm.rungs
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
        Stand::Deck(shape) => {
            cartopolis_geo::terrain_refine::refine_for_terrain(
                &mut positions,
                &mut indices,
                &mut uvs,
                &mut normals,
                &mut colors,
                &mut rungs,
                DECK_PROFILE_EDGE_M,
                None,
            );
            for p in &mut positions {
                p[1] += shape.world_y([p[0], p[2]]);
            }
        }
        Stand::Terrain(terrain, _) if !terrain.is_ready() => {}
        Stand::Terrain(terrain, tunnels) => {
            on_terrain = true;
            stand_on_terrain(
                group,
                &mut positions,
                &mut indices,
                &mut uvs,
                &mut normals,
                &mut colors,
                &mut rungs,
                Vec2::new(wx, wz),
                terrain,
                tunnels,
                None,
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
    let vertex_count = positions.len();
    let mut mesh = Mesh::new(
        PrimitiveTopology::TriangleList,
        RenderAssetUsages::RENDER_WORLD,
    )
    .with_inserted_attribute(Mesh::ATTRIBUTE_POSITION, positions)
    .with_inserted_attribute(Mesh::ATTRIBUTE_NORMAL, normals)
    .with_inserted_attribute(Mesh::ATTRIBUTE_UV_0, uvs)
    .with_inserted_indices(Indices::U32(indices));
    // `StandardMaterial` multiplies `base_color` by this whenever the attribute is
    // present — which is how one canopy material gives thousands of trees their own
    // tone and brightness without a material or a draw call per tree.
    if colors.len() == vertex_count {
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
        group,
        SurfaceGroup::Road(_) | SurfaceGroup::RoadCasing(_) | SurfaceGroup::TunnelRoad(_)
    ) {
        debug_assert_eq!(
            rungs.len(),
            vertex_count,
            "{:?} lost its depth rungs between tessellation and upload",
            group,
        );
        if rungs.len() != vertex_count {
            rungs = vec![0.0; vertex_count];
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
        let bytes = cartopolis_geo::vector_tiles::mesh_upload_bytes(vertex_count, index_count);
        let cap = crate::systems::lod22::max_cell_mesh_bytes();
        if bytes > cap {
            tracing::warn!(
                group = ?group,
                asset_kb = bytes / 1024,
                cap_kb = cap / 1024,
                vertices = vertex_count,
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
    // The median of the *outline*: with no register, the only level there is. It
    // is not safe to use on its own, and the clamp below is what makes it so — see
    // the note after the refinement.
    let fallback = terrain.level_over(positions.iter().map(|p| [origin.x + p[0], origin.y + p[2]]));
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
    // **The clamp is not only for registered water.** Where nothing is registered
    // the level is the median of the outline, and the outline is the one place the
    // body's ground is lowest. AHN has no return off open water, so the field over a
    // lake is filled in from its banks — and beside the Afsluitdijk that fill rises
    // metres above the shoreline median. The clipmap's relief tiles follow the field,
    // so they stood through the flat sheet and hid it: square patches of basemap in
    // the open IJsselmeer (bisected on the Deck, 2026-09-26, by discarding the
    // tiles' water texels — the missing surface was there all along, underneath).
    // The same rule the register path always had — never below the ground drawn
    // under it — settles it. (Smaller patches near registered shores are a
    // different, still open case: `docs/notes/water-surface.md`.)
    for p in positions.iter_mut() {
        let (wx, wz) = (origin.x + p[0], origin.y + p[2]);
        let level = terrain.water_surface_at(wx, wz).unwrap_or(fallback);
        // Above the basemap, which reads the same canal-side ground: where the carve
        // reached, that is the registered level to the bank; where a canal is too
        // narrow for the field to have caught it, the water rises to the ground as
        // it always did, and no wall stands there.
        p[1] += level.max(terrain.sunk_at(wx, wz));
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

/// Shortest step between a canal's surface and the street beside it that gets a
/// wall. Below it the two meet as they always did — a ditch in a field, a lake
/// shore, a canal too narrow for the field to have carved.
const QUAY_WALL_MIN_M: f32 = 0.3;

/// How far either side of a measured surveyed wall its foot is looked for,
/// metres (also at 1 m and 2.5 m).
///
/// Further than the wall is thick because the survey's line and the map's water
/// polygon do not agree about where the quay edge is: the 243 m, 4.25 m quay
/// wall along the Westerhaven lies five-odd metres from OSM's water, and at a
/// 2.5 m reach it found no water, stood its full height on the walkway and read
/// as a grey screen down the bank (2026-10-01). Short of the next street.
const WALL_FOOT_PROBE_M: f32 = 7.0;
/// Tallest wall stood. Groningen's tallest registered quay wall is 4.45 m
/// (`walls-have-a-measured-height.md`); past this a step is a bad join, not masonry.
const QUAY_WALL_MAX_M: f32 = 8.0;
/// How far a wall reaches below the water, so no sliver of sky shows between the
/// two at a grazing angle.
const QUAY_WALL_FOOT_M: f32 = 0.5;
/// How far outside a water edge the "is there more water here" probe looks.
const QUAY_PROBE_M: f32 = 0.4;

/// The walls a cell's water stands against: a vertical face along every edge of
/// its water where the street beside it is higher than the surface, from just
/// under the water up to the street.
///
/// **What made a wall necessary is that the two sides stopped sharing a surface.**
/// The water stands at its registered level ([`water_surface`]) and the street at
/// the ground beside the water ([`TerrainSnapshot::land_at`](crate::systems::terrain::TerrainSnapshot::land_at)); on a Groningen
/// canal that is two to five metres. Before the split the street sagged into the
/// carve and the water climbed out of it, and the handset — whose field is four
/// times coarser — drew every inner canal as a smeared ramp with no edge to it.
///
/// Built from the *drawn* water, after it is placed, so a wall meets the surface
/// it bounds exactly; the register's rings only ever said how deep. Three kinds of
/// outline edge are not banks and get nothing: an edge on the tile border (the
/// canal goes on in the next cell), an edge with more of this cell's water just
/// outside it (two water polygons meeting), and an edge whose street is within
/// [`QUAY_WALL_MIN_M`] of the water.
fn quay_walls<G: MeasuredGround>(
    meshes: &[SurfaceMesh],
    tile_m: f32,
    origin: Vec2,
    terrain: &G,
) -> Option<SurfaceMesh> {
    if !terrain.has_banks() {
        return None;
    }
    let mut out = SurfaceMesh {
        group: SurfaceGroup::QuayWall,
        positions: Vec::new(),
        indices: Vec::new(),
        uvs: Vec::new(),
        normals: Vec::new(),
        colors: Vec::new(),
        rungs: Vec::new(),
    };
    for water in meshes.iter().filter(|m| m.group == SurfaceGroup::Water) {
        let pos = &water.positions;
        let tris: Vec<[u32; 3]> = water
            .indices
            .chunks_exact(3)
            .map(|t| [t[0], t[1], t[2]])
            .collect();
        if tris.is_empty() {
            continue;
        }
        // Outline edges: used by one triangle. Keyed by the vertices' positions
        // rather than their indices, because the refinement shares a midpoint
        // between two triangles by index but separate polygons never share one.
        let key = |i: u32| {
            let p = pos[i as usize];
            ((p[0] * 100.0).round() as i32, (p[2] * 100.0).round() as i32)
        };
        let mut edges: HashMap<((i32, i32), (i32, i32)), (u32, u32, u32, u32)> = HashMap::new();
        for t in &tris {
            for k in 0..3 {
                let (a, b, c) = (t[k], t[(k + 1) % 3], t[(k + 2) % 3]);
                let (ka, kb) = (key(a), key(b));
                let e = if ka <= kb { (ka, kb) } else { (kb, ka) };
                edges
                    .entry(e)
                    .and_modify(|v| v.3 += 1)
                    .or_insert((a, b, c, 1));
            }
        }
        let index = TriangleIndex::new(pos, &tris);
        let on_border = |p: [f32; 3]| {
            const EPS: f32 = 0.05;
            (
                p[0].abs() < EPS || (p[0] - tile_m).abs() < EPS,
                p[2].abs() < EPS || (p[2] - tile_m).abs() < EPS,
            )
        };
        for &(a, b, c, uses) in edges.values() {
            if uses != 1 {
                continue;
            }
            let (pa, pb, pc) = (pos[a as usize], pos[b as usize], pos[c as usize]);
            let (ba, bb) = (on_border(pa), on_border(pb));
            if (ba.0 && bb.0 && (pa[0] - pb[0]).abs() < 0.1)
                || (ba.1 && bb.1 && (pa[2] - pb[2]).abs() < 0.1)
            {
                continue;
            }
            let along = Vec2::new(pb[0] - pa[0], pb[2] - pa[2]);
            let len = along.length();
            if len < 1e-3 {
                continue;
            }
            // Away from the triangle's third vertex: out of the water.
            let mut out_n = Vec2::new(-along.y, along.x) / len;
            let mid = Vec2::new(pa[0] + pb[0], pa[2] + pb[2]) * 0.5;
            if out_n.dot(Vec2::new(pc[0], pc[2]) - mid) > 0.0 {
                out_n = -out_n;
            }
            if index.contains(mid + out_n * QUAY_PROBE_M) {
                continue;
            }
            let street = |p: [f32; 3]| terrain.land_at(origin.x + p[0], origin.y + p[2]);
            let (ta, tb) = (street(pa), street(pb));
            let (ha, hb) = (ta - pa[1], tb - pb[1]);
            if ha.max(hb) < QUAY_WALL_MIN_M || ha.min(hb) > QUAY_WALL_MAX_M {
                continue;
            }
            let top = |t: f32, water_y: f32| t.clamp(water_y, water_y + QUAY_WALL_MAX_M);
            let base = out.positions.len() as u32;
            out.positions.extend_from_slice(&[
                [pa[0], pa[1] - QUAY_WALL_FOOT_M, pa[2]],
                [pb[0], pb[1] - QUAY_WALL_FOOT_M, pb[2]],
                [pb[0], top(tb, pb[1]), pb[2]],
                [pa[0], top(ta, pa[1]), pa[2]],
            ]);
            // Facing the water, whichever way round the outline runs.
            let face = [-out_n.x, 0.0, -out_n.y];
            out.normals.extend_from_slice(&[face; 4]);
            let (v0, v1, v2) = (
                Vec3::from_array(out.positions[base as usize]),
                Vec3::from_array(out.positions[base as usize + 1]),
                Vec3::from_array(out.positions[base as usize + 2]),
            );
            if (v1 - v0).cross(v2 - v0).dot(Vec3::from_array(face)) >= 0.0 {
                out.indices.extend_from_slice(&[
                    base,
                    base + 1,
                    base + 2,
                    base,
                    base + 2,
                    base + 3,
                ]);
            } else {
                out.indices.extend_from_slice(&[
                    base,
                    base + 2,
                    base + 1,
                    base,
                    base + 3,
                    base + 2,
                ]);
            }
        }
    }
    (!out.indices.is_empty()).then_some(out)
}

/// A uniform grid over a mesh's triangles in the XZ plane, for "is this point on
/// the water" asked once per outline edge.
struct TriangleIndex<'a> {
    pos: &'a [[f32; 3]],
    tris: &'a [[u32; 3]],
    min: Vec2,
    cell: f32,
    cols: usize,
    rows: usize,
    buckets: Vec<Vec<u32>>,
}

impl<'a> TriangleIndex<'a> {
    fn new(pos: &'a [[f32; 3]], tris: &'a [[u32; 3]]) -> Self {
        const CELL_M: f32 = 16.0;
        let (mut min, mut max) = (Vec2::splat(f32::MAX), Vec2::splat(f32::MIN));
        for p in pos {
            min = min.min(Vec2::new(p[0], p[2]));
            max = max.max(Vec2::new(p[0], p[2]));
        }
        let span = (max - min).max(Vec2::splat(1.0));
        let cols = ((span.x / CELL_M).ceil() as usize).clamp(1, 1024);
        let rows = ((span.y / CELL_M).ceil() as usize).clamp(1, 1024);
        let cell = (span.x / cols as f32).max(span.y / rows as f32);
        let mut index = Self {
            pos,
            tris,
            min,
            cell,
            cols,
            rows,
            buckets: vec![Vec::new(); cols * rows],
        };
        for (i, t) in tris.iter().enumerate() {
            let (mut lo, mut hi) = (Vec2::splat(f32::MAX), Vec2::splat(f32::MIN));
            for &v in t {
                let p = pos[v as usize];
                lo = lo.min(Vec2::new(p[0], p[2]));
                hi = hi.max(Vec2::new(p[0], p[2]));
            }
            let (c0, r0) = index.bucket(lo);
            let (c1, r1) = index.bucket(hi);
            for r in r0..=r1 {
                for c in c0..=c1 {
                    index.buckets[r * cols + c].push(i as u32);
                }
            }
        }
        index
    }

    fn bucket(&self, p: Vec2) -> (usize, usize) {
        let q = (p - self.min) / self.cell;
        (
            (q.x.max(0.0) as usize).min(self.cols - 1),
            (q.y.max(0.0) as usize).min(self.rows - 1),
        )
    }

    fn contains(&self, p: Vec2) -> bool {
        let (c, r) = self.bucket(p);
        self.buckets[r * self.cols + c].iter().any(|&i| {
            let t = self.tris[i as usize];
            let v = |k: usize| {
                let q = self.pos[t[k] as usize];
                Vec2::new(q[0], q[2])
            };
            let (a, b, c) = (v(0), v(1), v(2));
            let side = |p0: Vec2, p1: Vec2| (p1 - p0).perp_dot(p - p0);
            let (d0, d1, d2) = (side(a, b), side(b, c), side(c, a));
            let neg = d0 < 0.0 || d1 < 0.0 || d2 < 0.0;
            let pos = d0 > 0.0 || d1 > 0.0 || d2 > 0.0;
            !(neg && pos)
        })
    }
}

/// How far to reach for the land *around* a tunnel — see
/// [`cartopolis_geo::tunnels::DATUM_M`], which is where it lives so every
/// consumer reaches for the same distance.
pub use cartopolis_geo::tunnels::DATUM_M as TUNNEL_DATUM_M;

/// Ground surfaces get a fresh normal from the field's own slope; the vegetation
/// groups keep theirs (a tree's normals describe the tree, not the hill).
#[allow(clippy::too_many_arguments)]
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
    levels: Option<&cartopolis_geo::road_levels::LevelMap>,
) {
    // Volumetric things keep their own normals and are not refined against the
    // field: a tree's normals describe the tree, and a tunnel's walls are vertical.
    let volumetric = matches!(
        group,
        SurfaceGroup::Canopy
            | SurfaceGroup::Trunk
            | SurfaceGroup::Furniture
            | SurfaceGroup::TunnelRoad(_)
            | SurfaceGroup::TunnelStructure
    );
    // **A structure below the ground is built at its own heights**
    // (`cartopolis_geo::tunnels::build_structures`): the road where the network
    // put it and the walls up to the land, in this field's frame. Nothing lifts
    // it — every earlier version drew an invented depth and lifted it back by
    // what the survey had already dug, and four consumers had to repeat that
    // arithmetic and agree.
    if matches!(
        group,
        SurfaceGroup::TunnelRoad(_) | SurfaceGroup::TunnelStructure
    ) {
        // **Under water the structure stays under the water.** The land a lid is
        // buried under is the level of the quays around, and a canal over the
        // lid is lower than that: the roof and walls under the Oude
        // Winschoterdiep stood out of it as two dark wedges (2026-09-23). They
        // are held below the registered surface; the deck is left where it is,
        // so the traffic on it is unchanged.
        if group == SurfaceGroup::TunnelStructure {
            for p in positions.iter_mut() {
                let (x, z) = (origin.x + p[0], origin.y + p[2]);
                if let Some(w) = terrain.water_surface_at(x, z) {
                    p[1] = p[1].min(w - UNDER_WATER_M);
                }
            }
        }
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
    // **The ground these stand on is the measured ground with the land put back
    // over every lid and beside every wall**, and that has to be one surface
    // rather than a displacement and a later correction. Where a road has a lid
    // the survey — flown before the lid was built — still shows the open trench;
    // lifting the vertices afterwards left the lift invisible to the refinement,
    // so a land parcel of two triangles interpolated nine metres across a chord
    // while the road beside it followed the taper, and the ground tore open along
    // the buried road. The land is the structure's own — the level its walls
    // reach and its lid is buried under — so the ground meets them; where atlas
    // graded the ground it is already there and this changes nothing.
    // Stood on the ground *beside* the water rather than the carved field, so a
    // street runs level to the quay instead of sagging into the canal; the step
    // left at the edge is what `quay_walls` closes. The same field everywhere a
    // canal is not near.
    let land = terrain.land();
    let lidded = cartopolis_geo::tunnels::Lidded {
        ground: &land,
        around: &|x, z| tunnels.fill(Vec2::new(x, z), TUNNEL_DATUM_M).1,
        weight: &|p| tunnels.fill_weight(Vec2::from_array(p), TUNNEL_DATUM_M),
    };
    // **A road stands at its own height** where atlas solved one
    // (`road_levels`), not on the ground: the ground was laid to the road there,
    // and the road off a bridge's end meets the deck at the node they share.
    // Refined against the same field, so a long straight road still has the
    // vertices to follow its embankment.
    let on_roads = levels
        .filter(|_| matches!(group, SurfaceGroup::Road(_) | SurfaceGroup::RoadCasing(_)))
        .map(|levels| OnRoads {
            levels,
            origin,
            datum: terrain.datum_m(),
            under: &lidded,
        });
    let field: &dyn cartopolis_geo::terrain_refine::GroundField = match &on_roads {
        Some(f) => f,
        None => &lidded,
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
                field,
                origin: origin.to_array(),
            }),
        );
    }
    let t1 = bevy::platform::time::Instant::now();
    {
        for p in positions.iter_mut() {
            p[1] += field.at(origin.x + p[0], origin.y + p[2]);
        }
    }
    for (i, p) in crate::systems::ground_probe::probes().iter().enumerate() {
        let local = *p - origin;
        if let Some(y) =
            crate::systems::ground_probe::height_at(positions, indices, local.x, local.y)
        {
            bevy::log::info!(
                probe = i,
                group = ?group,
                drawn = y,
                field = field.at(p.x, p.y),
                "ground probe: sheet"
            );
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
                let w = tunnels.fill_weight(Vec2::new(x, z), TUNNEL_DATUM_M);
                let n = terrain.land_normal_at(x, z);
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

/// How far past a street's half-width a road surface still takes that street's
/// height, metres: the kerb casing and the stub a junction pushes into the road
/// it meets.
const ROAD_REACH_M: f32 = 1.5;

/// The ground a road surface stands on: its street's solved height where there
/// is one (`road_levels`), the measured ground elsewhere.
struct OnRoads<'a> {
    levels: &'a cartopolis_geo::road_levels::LevelMap,
    origin: Vec2,
    datum: f32,
    under: &'a dyn cartopolis_geo::terrain_refine::GroundField,
}

impl cartopolis_geo::terrain_refine::GroundField for OnRoads<'_> {
    fn min_ground_edge_m(&self) -> f32 {
        self.under.min_ground_edge_m()
    }

    fn at(&self, x: f32, z: f32) -> f32 {
        let local = [x - self.origin.x, z - self.origin.y];
        self.levels
            .y_at(local, |level| level == 0, ROAD_REACH_M)
            .map_or_else(|| self.under.at(x, z), |y| y - self.datum)
    }
}

thread_local! {
    /// Where `stand_on_terrain`'s time goes on this worker, µs: refine, displace,
    /// normals, and the vertex count out. Read and reset per cell job.
    static GROUND_SPLIT: std::cell::Cell<[u64; 4]> = const { std::cell::Cell::new([0; 4]) };
}

/// How far below a registered water surface a tunnel's roof and walls are held.
const UNDER_WATER_M: f32 = 0.3;

/// How far out along an approach [`DeckPlates::approach_steps`] reads the ground.
const APPROACH_PROBE_M: f32 = 3.0;

/// The point `at` metres along a polyline, clamped to its ends.
pub(crate) fn point_along(pts: &[[f32; 2]], at: f32) -> [f32; 2] {
    let mut walked = 0.0f32;
    for w in pts.windows(2) {
        let seg = (w[1][0] - w[0][0]).hypot(w[1][1] - w[0][1]);
        if walked + seg >= at && seg > 1e-6 {
            let t = (at - walked) / seg;
            return [
                w[0][0] + (w[1][0] - w[0][0]) * t,
                w[0][1] + (w[1][1] - w[0][1]) * t,
            ];
        }
        walked += seg;
    }
    pts.last().copied().unwrap_or([0.0, 0.0])
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

    /// `water.wgsl` fades its waves out toward the edge of this module's ring,
    /// which it cannot ask about — so it carries the radius as a constant, and
    /// this is what stops the two drifting. A shader that believed the ring was
    /// bigger would put the fade's end past the last cell: waves right up to a
    /// hard line again.
    #[test]
    fn the_water_shader_knows_the_ring_radius() {
        let src = include_str!("../../../../../assets/shaders/water.wgsl");
        let decl = format!("const RING_RADIUS_CELLS: f32 = {CELL_RADIUS}.0;");
        assert!(src.contains(&decl), "water.wgsl must declare `{decl}`");
    }

    /// The ground of `terrain::tests::canal` as a placement: a 20 m canal at −3 m
    /// in a street at +2 m.
    fn canal_ground() -> crate::systems::terrain::GroundPlacement {
        let water = crate::systems::water_levels::WaterLevelField::build_for_test(
            -3.0,
            &[
                [-10.0, -300.0],
                [10.0, -300.0],
                [10.0, 300.0],
                [-10.0, 300.0],
            ],
        );
        let n = 65;
        let samples: Vec<f32> = (0..n * n)
            .map(|i| {
                let x = -100.0 + (i % n) as f32 * 3.125;
                if x.abs() < 10.0 {
                    -3.5
                } else {
                    2.0
                }
            })
            .collect();
        crate::systems::terrain::GroundPlacement::for_test(
            crate::systems::terrain::TerrainSnapshot::from_samples_for_test(samples, n, 100.0),
            water,
        )
    }

    /// A placed water rectangle `x0..x1` × `z0..z1` at `y`.
    fn water_rect(x0: f32, x1: f32, z0: f32, z1: f32, y: f32) -> SurfaceMesh {
        SurfaceMesh {
            group: SurfaceGroup::Water,
            positions: vec![[x0, y, z0], [x1, y, z0], [x1, y, z1], [x0, y, z1]],
            indices: vec![0, 1, 2, 0, 2, 3],
            uvs: Vec::new(),
            normals: Vec::new(),
            colors: Vec::new(),
            rungs: Vec::new(),
        }
    }

    /// Each bank of a canal gets a wall from under the water to the street, facing
    /// the water; the ends where the canal runs on into the next tile get none.
    #[test]
    fn a_canal_gets_a_wall_on_each_bank_and_none_across_its_ends() {
        let ground = canal_ground();
        // The tile runs z 0..90 (inside the ±100 m field), so the canal's two
        // short edges are on its border.
        let meshes = vec![water_rect(-10.0, 10.0, 0.0, 90.0, -3.0)];
        let walls = quay_walls(&meshes, 90.0, Vec2::ZERO, &ground).expect("walls");
        assert_eq!(walls.group, SurfaceGroup::QuayWall);
        assert_eq!(walls.positions.len(), 8, "two banks, one quad each");
        for (p, n) in walls.positions.iter().zip(&walls.normals) {
            assert!(p[0].abs() > 9.99, "a wall off the bank at x = {}", p[0]);
            let is_top = p[1] > 0.0;
            let want = if is_top { 2.0 } else { -3.0 - QUAY_WALL_FOOT_M };
            assert!((p[1] - want).abs() < 1e-3, "height {} want {want}", p[1]);
            // Facing into the canal.
            assert!(n[0] * p[0] < 0.0, "normal {n:?} at x = {}", p[0]);
        }
        // Every triangle faces the way its normal says.
        for t in walls.indices.chunks_exact(3) {
            let [a, b, c] = [0, 1, 2].map(|k| Vec3::from_array(walls.positions[t[k] as usize]));
            let n = Vec3::from_array(walls.normals[t[0] as usize]);
            assert!((b - a).cross(c - a).dot(n) > 0.0);
        }
    }

    /// Where two water polygons meet there is water on both sides of the edge, and
    /// no wall; a canal the field is too coarse to have carved has nothing to hold.
    #[test]
    fn no_wall_between_two_waters_or_where_the_street_is_the_water() {
        let ground = canal_ground();
        let meshes = vec![
            water_rect(-10.0, 0.0, 0.0, 90.0, -3.0),
            water_rect(0.0, 10.0, 0.0, 90.0, -3.0),
        ];
        let walls = quay_walls(&meshes, 90.0, Vec2::ZERO, &ground).expect("walls");
        assert!(
            walls.positions.iter().all(|p| p[0].abs() > 9.99),
            "a wall down the middle of the canal"
        );
        // The same outline with its surface at the street: nothing to stand.
        let level = vec![water_rect(-10.0, 10.0, 0.0, 90.0, 2.0)];
        assert!(quay_walls(&level, 90.0, Vec2::ZERO, &ground).is_none());
    }

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

    /// **A sloped deck is drawn sloped, and what stands on it stands on the
    /// slope.** A 120 × 12 m deck whose survey rises 6 m end to end: the slab's
    /// surface follows it within a few centimetres all along — which needs the
    /// subdivision, since its outline is four corners — and `top_at` answers the
    /// height under a car at either end, not the median.
    #[test]
    fn a_profiled_deck_follows_its_slope() {
        let deck = BridgeDeck {
            positions: vec![
                [0.0, 0.0, 0.0],
                [120.0, 0.0, 0.0],
                [120.0, 0.0, 12.0],
                [0.0, 0.0, 12.0],
            ],
            indices: vec![0, 2, 1, 0, 3, 2],
            rings: vec![vec![
                [0.0, 0.0],
                [120.0, 0.0],
                [120.0, 12.0],
                [0.0, 12.0],
                [0.0, 0.0],
            ]],
            min: [0.0, 0.0],
            max: [120.0, 12.0],
        };
        let rise = |x: f64| (2.0 + 0.05 * x) as f32;
        let profile = cartopolis_geo::deck_profile::DeckProfile::fit_over(&deck, |p| {
            Some(rise(f64::from(p[0])))
        })
        .unwrap();
        let shape = Arc::new(DeckShape {
            profile,
            offset: 0.0,
        });

        let mesh = build_deck_mesh(
            std::slice::from_ref(&deck),
            &[5.0],
            &[5.0],
            &[Some(shape.clone())],
            0.0,
            &|_, _| f32::NEG_INFINITY,
        );
        let mut checked = 0;
        for (p, n) in mesh.positions.iter().zip(&mesh.normals) {
            // The top surface only: the fascia's subdivided faces have vertices
            // all the way down the slab's thickness.
            if n[1] < 0.9 {
                continue;
            }
            let want = rise(f64::from(p[0]).clamp(2.0, 118.0));
            assert!((p[1] - want).abs() < 0.12, "{p:?} against {want}");
            checked += 1;
        }
        assert!(checked > 40, "only {checked} vertices on the surface");

        let mut plates = DeckPlates::default();
        plates.set_cell(
            (0, 0),
            vec![Plate {
                deck,
                world: Vec2::ZERO,
                height: 5.0,
                top_nap: Some(5.0),
                world_top: 5.0,
                shape: Some(shape),
                approaches: Vec::new(),
            }],
        );
        let low = plates.top_at(Vec2::new(10.0, 6.0)).unwrap();
        let high = plates.top_at(Vec2::new(110.0, 6.0)).unwrap();
        assert!((low - rise(10.0)).abs() < 0.12, "{low}");
        assert!((high - rise(110.0)).abs() < 0.12, "{high}");
    }

    /// **A pier stands from below the ground to the underside of the deck over
    /// it**, and a pier under no lifted deck is not stood at all.
    #[test]
    fn a_pier_holds_up_the_deck_over_it_and_nothing_else() {
        let deck = deck_at(40.0, 0.0, 0.0);
        let under = vec![[18.0, 18.0], [22.0, 18.0], [22.0, 22.0], [18.0, 22.0]];
        let away = vec![[80.0, 80.0], [84.0, 80.0], [84.0, 84.0], [80.0, 84.0]];
        let mut mesh = build_deck_mesh(
            std::slice::from_ref(&deck),
            &[6.0],
            &[6.0],
            &[None],
            1.0,
            &|_, _| f32::NEG_INFINITY,
        );
        let before = mesh.positions.len();
        let stood = add_piers(
            &mut mesh,
            &[under, away],
            std::slice::from_ref(&deck),
            &[6.0],
            &[6.0],
            &[None],
            1.0,
            |_| 1.5,
        );
        assert_eq!(stood, 1);
        let pier = &mesh.positions[before..];
        assert_eq!(pier.len(), 16, "four walls of four corners");
        // Relative to `deck_level` (1.0): the deck's top is at 1 + 6 = 7 world,
        // its underside a slab lower; the foot sunk below the ground at 1.5.
        let top = pier.iter().map(|p| p[1]).fold(f32::MIN, f32::max);
        let foot = pier.iter().map(|p| p[1]).fold(f32::MAX, f32::min);
        assert!((top - (6.0 - DECK_THICKNESS_M)).abs() < 1e-4, "{top}");
        assert!((foot - (1.5 - PIER_FOOTING_M - 1.0)).abs() < 1e-4, "{foot}");
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
        let deck = deck_at(20.0, 10.0, 10.0);
        let (boxes, clusters) = cluster_decks(std::slice::from_ref(&deck), (x, y), 1470.0);
        assert!(clusters.is_empty());
        assert_eq!(boxes[0].cluster, None);
        assert_eq!(
            deck_measure(&HeightLayer::default(), &clusters, &boxes[0], &deck),
            None
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
                top_nap: None,
                world_top: 0.0,
                shape: None,
            }],
        );

        // 201 samples over ±100 m is one per metre, which resolves a 40 m deck.
        let (n, half) = (201usize, 100.0f32);
        let cartopolis_geo::terrain_grade::DeckMasks { ramp, lift, .. } =
            plates.grade_masks(n, half);
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
        let cartopolis_geo::terrain_grade::DeckMasks { ramp, lift, .. } =
            DeckPlates::default().grade_masks(n, half);
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
                top_nap: None,
                world_top: 0.0,
                shape: None,
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
                    top_nap: None,
                    world_top: 0.0,
                    shape: None,
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
            top_nap: None,
            world_top: 0.0,
            shape: None,
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
                top_nap: None,
                world_top: 0.0,
                shape: None,
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
