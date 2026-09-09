//! 3DBAG **LoD2.2** real building meshes (ADR-027).
//!
//! Where the OSM/LoD1.2 path (`osm_buildings.rs`) extrudes footprints and *guesses*
//! roofs, this streams the actual triangulated BAG roof geometry (sloped, hipped,
//! complex roofs) preprocessed server-side into per-cell meshes
//! (cartopy's `lod22.ts` → `https://3dbag.cartopolis.org/lod22/…`). Each cell file is
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
use crate::systems::building_windows::{BuildingMaterial, BuildingWindows};
use crate::systems::globe::{GlobeState, GLOBE_SHOW_ALT};
use crate::systems::load_control;
use crate::systems::map_stream::MapStream;
use crate::systems::osm_buildings::{lat_lng_to_local, lod22_covers, tdbag_url, MeshData};
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
    load_control::radius(LOD22_RADIUS)
}

/// The last ring whose cells are built with door panels on their facades.
///
/// One inside the ring's edge, so only the outermost ring goes without. That
/// placement is the whole safety argument: a rank-1 cell is *adjacent*, and its
/// near edge is under your feet, so it must keep its doors — while a rank-2 cell
/// is at least one cell's width away, about 1.5 km at Dutch latitudes, where a
/// 1.1 × 2.1 m panel subtends well under a pixel.
///
/// It also bounds how often a cell is promoted (and therefore rebuilt): the
/// camera has to cross a whole cell boundary, which is the same event that brings
/// five new cells into the ring anyway.
fn last_door_rank() -> i64 {
    // Floored at 1, not 0. A rank-1 cell is *adjacent* — its near edge is under
    // your feet — so it must keep its doors whatever the ring has narrowed to.
    // Without the floor, `load_control` tightening to radius 1 would take the
    // doors off the cell next to you, which is the adjacency mistake this whole
    // boundary is placed to avoid.
    (lod22_radius() - 1).max(1)
}

/// Whether a cell at this ring distance is built with its doors.
fn door_detail(rank: crate::platform::fetch_gate::Rank) -> load_control::Detail {
    load_control::Detail::for_rank(rank, last_door_rank())
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
pub(crate) fn max_cell_mesh_bytes() -> usize {
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

/// A drawn door's width, height, and how far it stands off the wall (metres).
///
/// The height is the load-bearing one: it must stay under
/// `building_windows::FIRST_SILL_M`, which is the height below which that shader
/// paints no windows. A taller door would come out with panes across it.
const DOOR_WIDTH_M: f32 = 1.1;
const DOOR_HEIGHT_M: f32 = 2.1;
const DOOR_PROUD_M: f32 = 0.04;

/// The door's colour, linear RGB with the neutral night-occupancy alpha.
///
/// A dark warm brown against pale brick and grey render: the one thing on a
/// facade that should read as "a way in" from across the street. It is a *vertex*
/// colour like everything else in this mesh, so it costs no material.
const DOOR_COLOR: [f32; 4] = [0.115, 0.075, 0.055, 1.0];

/// The point on a ring nearest `p`, and the **outward** normal of the edge it
/// lies on.
///
/// Outward is settled by stepping off the wall and asking whether that lands
/// outside the ring, rather than by reasoning about which way the ring is wound.
/// That is deliberate. The first version derived it from the signed area, which
/// is the tidy answer and was the wrong way round in this frame — where +z runs
/// *south* — so every door in the city was drawn on the inside of its own wall,
/// hidden by the shell around it. The count said 113,710 doors and the street
/// showed none, which is the kind of failure a convention produces and a
/// measurement does not: a point either is inside the polygon or it is not, on
/// any winding and on any shape.
fn nearest_edge_point(ring: &[Vec2], p: Vec2) -> Option<(Vec2, Vec2)> {
    let mut best: Option<(f32, Vec2, Vec2)> = None;
    for i in 0..ring.len() {
        let (a, b) = (ring[i], ring[(i + 1) % ring.len()]);
        let ab = b - a;
        let len2 = ab.length_squared();
        if len2 <= f32::EPSILON {
            continue;
        }
        let t = ((p - a).dot(ab) / len2).clamp(0.0, 1.0);
        let at = a + ab * t;
        let d2 = at.distance_squared(p);
        if best.is_none_or(|(bd, _, _)| d2 < bd) {
            best = Some((d2, at, Vec2::new(ab.y, -ab.x) / len2.sqrt()));
        }
    }
    let (_, at, n) = best?;
    /// How far off the wall the inside/outside probe is taken. Well under a wall's
    /// thickness and well over the precision of a ring rounded to 1 cm.
    const PROBE_M: f32 = 0.05;
    Some((
        at,
        if point_in_ring(ring, at + n * PROBE_M) {
            -n
        } else {
            n
        },
    ))
}

/// Even-odd ray cast — the same test `collision::Footprint` uses, kept here
/// because this crate's copy answers about a bare slice rather than a built
/// footprint.
fn point_in_ring(ring: &[Vec2], p: Vec2) -> bool {
    let mut inside = false;
    let n = ring.len();
    let mut j = n - 1;
    for i in 0..n {
        let (a, b) = (ring[i], ring[j]);
        if (a.y > p.y) != (b.y > p.y) {
            let x = a.x + (p.y - a.y) / (b.y - a.y) * (b.x - a.x);
            if p.x < x {
                inside = !inside;
            }
        }
        j = i;
    }
    inside
}

/// Marker for spawned LoD2.2 meshes.
#[derive(Component)]
pub struct Lod22Mesh;

/// One building's mesh data as received from the host.
struct Bldg {
    /// The BAG **identificatie** — the key `systems::building_facts` joins the
    /// register's year, use and status on, and the key any other Dutch register or
    /// BIM model of this building shares. Empty when the host predates the id
    /// being carried, which every consumer reads as "no facts".
    id: String,
    /// Vertices as (lat, lng, height-above-own-base).
    p: Vec<(f64, f64, f32)>,
    /// Triangles: indices into `p` plus a roof flag (1 = roof, 0 = wall).
    t: Vec<(usize, usize, usize, u8)>,
    /// The IFC class this element came from (`IfcWall`, `IfcSlab`, …), for an
    /// imported model. **Empty for a shell**, which is what tells the two apart
    /// everywhere below: a shell is one solid per building and collides as a
    /// footprint, a model is many elements and collides only on its walls.
    class: String,
}

/// One parsed cell file: its elements, and — for an imported-model cell — the
/// buildings it stands in for.
struct CellFile {
    elements: Vec<Bldg>,
    /// BAG ids whose shell this cell replaces (`tools/ifc_lod3.py`). Always empty
    /// for a LoD2.2 cell.
    pands: Vec<String>,
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
/// A mesh piece and whether it must be drawn double-sided.
///
/// True only for an imported model's surfaces. Culling back faces is right for a
/// shell — half its triangles face away and nothing can ever be behind them — and
/// wrong for a building you can be inside, where the wall in front of you is
/// showing you its back.
type Piece = (Mesh, bool);

/// How many of a cell's buildings the BAG join reached, out of how many were
/// drawn.
///
/// The join's failure mode is that it silently does not happen: the client keeps
/// drawing every building in the generated palette, which is what it looked like
/// before the registers existed. That is not hypothetical — the deployed LoD2.2
/// tree carried the *old* mangled ids for a day after the client learned to read
/// real ones, and every fact matched nothing at all while the map looked
/// completely normal. This is the number that says so.
type DatedCount = (usize, usize);

/// Doors drawn on a cell's facades, from the register's entrance points.
///
/// A count for the same reason every other layer here has one: a door is a small
/// dark rectangle at the foot of a wall, and "there are none" and "I am not
/// looking at one" are indistinguishable from any camera. Chasing that difference
/// with viewpoints is how an afternoon goes.
type DoorCount = usize;

/// Each drawn shell's BAG id and its measured storey datum, metres above its own
/// base. See [`shell_datum_height`] — this is the only place in the client that
/// knows how tall a building actually is, because the height only exists in the
/// LoD2.2 geometry and is thrown away the moment the mesh is merged.
type ShellHeights = Vec<(String, f32)>;

type Lod22Build = (
    GridCell,
    Vec<Piece>,
    Vec<crate::systems::collision::Footprint>,
    DatedCount,
    DoorCount,
    ShellHeights,
);

/// A cell taken off the stream but not fully spawned: the remaining pieces land
/// one `stream_ledger` admission at a time, a frame apart. See the apply loop in
/// [`update_lod22`] for why a cell may not land whole.
struct PartialCell {
    cell: (i64, i64),
    grid_cell: GridCell,
    pieces: Vec<Piece>,
}

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
    /// Shared material (white base, per-vertex wall/roof colours) — plus the
    /// window pattern, which is why it is `BuildingMaterial` and not a bare
    /// `StandardMaterial`. One material for the whole lane is what lets
    /// `building_windows::update_building_windows` light every window in the city
    /// by writing a single uniform.
    material: Option<Handle<BuildingMaterial>>,
    /// The same material with culling off, for imported models. A second handle
    /// rather than a second lane: they share the window shader, the palette and
    /// the merging, and differ only in which faces survive.
    interior_material: Option<Handle<BuildingMaterial>>,
    /// Last-seen state of the Layers panel's 3DBAG toggle, so the off-edge
    /// teardown in [`update_lod22`] runs exactly once per uncheck.
    enabled_was: bool,
    /// BAG coverage revision these cells were built against
    /// (`building_facts::coverage_revision`) — 0 while the manifest is still in
    /// flight, 1 once it has answered. The era palette and the window occupancy
    /// are baked into vertex colours, so cells built during that gap carry the
    /// generated look and have to be rebuilt once the answer arrives. It changes
    /// at most once per process.
    bag_revision: u32,
    /// Per-cell BAG join tally ([`DatedCount`]), summed for `--dump-state`.
    dated: HashMap<(i64, i64), DatedCount>,
    /// Per-cell tally of doors drawn ([`DoorCount`]).
    doors: HashMap<(i64, i64), DoorCount>,
    /// What detail each live cell was **built** with.
    ///
    /// The record is what makes a promotion detectable: the ring is re-derived
    /// from the camera every frame, so "how far is this cell now" is free, and
    /// "how far was it when its mesh was written" is only knowable if it was
    /// written down. Recorded at dispatch rather than on completion so a cell in
    /// flight is already accounted for and cannot be promoted twice.
    built_detail: HashMap<(i64, i64), load_control::Detail>,
    /// Each loaded shell's storey datum, metres ([`shell_datum_height`]), keyed
    /// by BAG identificatie. Per cell so it is evicted with the cell it was
    /// measured from, exactly as the colliders are.
    heights: HashMap<(i64, i64), HashMap<String, f32>>,
    /// Terrain-field revision these cells were built against. The field arrives a
    /// second or two after the anchor, so the first cells are always built on flat
    /// ground and have to be rebuilt onto the measured one.
    terrain_revision: u32,
    /// The one cell mid-application, if any. Dropped by every teardown that
    /// drops `cells` — a parked piece from the old anchor spawning into the new
    /// world would stand in the wrong place for the life of the ring.
    partial: Option<PartialCell>,
}

impl Lod22Buildings {
    /// Are 3DBAG cells still being fetched or meshed? Read by the `--shot`
    /// harness's settle test — these are the buildings a city shot is *of*.
    #[must_use]
    pub fn streaming_busy(&self) -> bool {
        // `partial` is the tail of a cell the stream has already been told
        // succeeded — without this term a capture could photograph a city with
        // three quarters of its densest cell missing and call itself settled.
        self.stream.busy() || self.partial.is_some()
    }
}

impl Lod22Buildings {
    /// Buildings whose BAG facts were found, and buildings drawn, over the loaded
    /// cells. See [`DatedCount`] — this is the reading that says whether the
    /// register join is live at all.
    pub fn dated(&self) -> (usize, usize) {
        self.dated
            .values()
            .fold((0, 0), |(m, t), (cm, ct)| (m + cm, t + ct))
    }

    /// Doors drawn across the loaded cells. See [`DoorCount`].
    pub fn doors(&self) -> usize {
        self.doors.values().sum()
    }

    /// How tall a building's shell is, metres above its own base — the datum
    /// `systems::interiors` divides into storeys. `None` for a building whose
    /// LoD2.2 cell is not loaded, which every caller must read as "one storey",
    /// because that is the answer this layer gave before it could ask.
    ///
    /// Linear in loaded cells (a handful) and a hash lookup in each; called once
    /// per building when the interior set is rebuilt, not per frame.
    pub fn shell_height(&self, id: &str) -> Option<f32> {
        self.heights.values().find_map(|c| c.get(id).copied())
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
            dated: HashMap::new(),
            doors: HashMap::new(),
            built_detail: HashMap::new(),
            heights: HashMap::new(),
            no_shadow: HashSet::new(),
            material: None,
            interior_material: None,
            enabled_was: true,
            terrain_revision: 0,
            bag_revision: 0,
            partial: None,
        }
    }
}

/// Stream 3DBAG LoD2.2 cells around the camera (ADR-027).
pub fn update_lod22(
    mut commands: Commands,
    mut lod: ResMut<Lod22Buildings>,
    mut meshes: ResMut<Assets<Mesh>>,
    mut materials: ResMut<Assets<BuildingMaterial>>,
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
    //
    // The BAG coverage answer lands the same way and is folded into the same
    // condition: it decides the facade palette and the window occupancy, both of
    // which are vertex colours, so a cell built before it arrived is as stale as
    // one built on flat ground. Unlike the terrain it can only change once.
    crate::systems::building_facts::prefetch_coverage(now);
    crate::systems::lod3::prefetch_coverage(now);
    // Both additive manifests are folded in the same way and for the same reason:
    // one decides a building's colours, the other decides whether it is drawn from
    // a shell or from a model, and a cell built before either landed has neither.
    let bag_revision = crate::systems::building_facts::coverage_revision()
        | (crate::systems::lod3::coverage_revision() << 1);
    let reanchored = lod.stream.sync(stream.anchor_generation);
    if reanchored || lod.terrain_revision != terrain.revision || lod.bag_revision != bag_revision {
        lod.terrain_revision = terrain.revision;
        lod.bag_revision = bag_revision;
        let retired: Vec<Entity> = lod.cells.drain().flat_map(|(_, e)| e).collect();
        lod.dated.clear();
        lod.doors.clear();
        lod.built_detail.clear();
        lod.heights.clear();
        lod.stream.retire_all(retired);
        colliders.clear_lod22();
        lod.no_shadow.clear();
        lod.stream.reset();
        lod.anchor = None;
        lod.partial = None;
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
        lod.dated.clear();
        lod.doors.clear();
        lod.built_detail.clear();
        lod.heights.clear();
        lod.stream.retire_all(retired);
        colliders.clear_lod22();
        lod.no_shadow.clear();
        lod.stream.reset();
        lod.partial = None;
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

    // **Cells that came closer than they were built for.** A cell's detail is
    // baked into its merged mesh on a worker, so a promotion is a rebuild — and
    // a rebuild is only worth doing where the difference is visible. Only the
    // outermost ring is built `Far` (see `door_detail`), so the cells that
    // promote are one column of the ring, and they promote when the camera
    // crosses a whole cell boundary: about 1.5 km at Dutch latitudes, alongside
    // the five cells that are entering the ring at that moment anyway. Demotions
    // are ignored on purpose — a cell that is drawn with more detail than it now
    // needs costs a few thousand triangles, and rebuilding it to take them away
    // would be paying the expensive half of the trade for the cheap half of the
    // benefit.
    //
    // Two bounds on it. A cell still **in flight** is never promoted: `forget`
    // would let it be requested a second time, both fetches would land, and the
    // second `cells.insert` would overwrite the first's entity list and leak
    // every one of them. And at most one cell is promoted per frame, matching
    // [`MAX_BUILDS_PER_FRAME`] — the rebuild it causes is the same expensive
    // thing that cap exists to meter out.
    let promoted: Option<(i64, i64)> = lod
        .built_detail
        .iter()
        .filter(|(c, built)| {
            **built == load_control::Detail::Far
                && lod.cells.contains_key(*c)
                && door_detail(ring_distance(**c, ccx, ccy)) == load_control::Detail::Near
        })
        .map(|(c, _)| *c)
        .min_by_key(|c| ring_distance(*c, ccx, ccy));
    if let Some(c) = promoted {
        lod.dated.remove(&c);
        lod.doors.remove(&c);
        lod.built_detail.remove(&c);
        lod.heights.remove(&c);
        if let Some(entities) = lod.cells.remove(&c) {
            for e in entities {
                lod.stream.retire(e);
            }
        }
        if lod.partial.as_ref().is_some_and(|p| p.cell == c) {
            lod.partial = None;
        }
        colliders.remove_cell(c);
        lod.no_shadow.remove(&c);
        // `forget`, not `retire`: the cell has to become requestable again, and
        // `forget` is what drops it from the stream's `requested` set and hands
        // its slots back. The re-fetch is a disk-cache read.
        lod.stream.forget(&c);
    }

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
    // The sort is what put the nearest cell first when the queue was arrival
    // ordered; the rank says the same thing to the queue and to the global
    // `fetch_gate`, and keeps saying it when the camera moves under a queue that
    // was filled from somewhere else.
    use crate::platform::fetch_gate::ring_distance;
    to_request.sort_by_key(|c| ring_distance(*c, ccx, ccy));
    for c in to_request {
        let rank = ring_distance(c, ccx, ccy);
        lod.stream.request_ranked(c, rank, now);
    }

    // Dispatch queued cells up to the concurrency cap: each becomes a detached
    // fetch+mesh task (ADR-033 seam) pushing to the sink — no OS thread.
    let results = lod.stream.sink();
    while let Some((cx, cy)) = lod.stream.next_job(now) {
        let results = results.clone();
        // Resolved at dispatch, not inside the worker: the worker has no camera,
        // and recording what a cell was *built* with is what makes the promotion
        // sweep above possible at all.
        let detail = door_detail(ring_distance((cx, cy), ccx, ccy));
        lod.built_detail.insert((cx, cy), detail);
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
                // The cell's BAG facts travel with the mesh job rather than
                // arriving through the `BuildingFacts` resource on their own
                // schedule: a year and a use are baked into vertex colours, so
                // they have to be known on the worker that writes them. It is a
                // second request that hits the same storage cache as the facts
                // streamer's, so at worst it is a cache read; at best it is the
                // fetch the streamer then reads. Uncovered cells return an empty
                // set without asking anything.
                let facts = crate::systems::building_facts::load_bag_cell(cx, cy).await;
                // …and the imported models for the same cell, on the same terms.
                let lod3 = crate::systems::lod3::load_lod3_text(cx, cy).await;
                crate::platform::workers::submit(
                    "lod22",
                    Box::new(move || {
                        let built = build_lod22_from_text(
                            &text,
                            cx,
                            cy,
                            ax,
                            ay,
                            z,
                            lat,
                            &ground,
                            &facts,
                            lod3.as_deref(),
                            detail,
                        );
                        if let Ok(mut g) = results.lock() {
                            g.push(((cx, cy), built));
                        }
                    }),
                );
            })
            .detach();
    }

    // Apply completed cells — one cell in application at a time, and its pieces
    // land **one ledger admission each, across frames**.
    //
    // The ledger is what the item cap could never be: this was the strictest
    // count in the codebase at one cell per frame and still the path that handed
    // the GPU 63 MB in a frame, because a cell is one to four 8 MB pieces and a
    // count cannot see that. The first fix admitted a cell *whole* (summed
    // pieces), and the ledger's first-admission rule then let a four-piece cell
    // through as a 31 MB frame — measured, 2026-08-20, the exact assets in the
    // `single asset above the frame upload budget` lines. The pieces were
    // byte-bounded on the worker precisely so they would not have to land
    // together, so now they don't: a taken cell parks in `partial`, each frame
    // spawns the pieces the ledger admits (on the phone ceiling that is one
    // ~8 MB piece per frame, alone), and the buildings arrive in slices a frame
    // apart. A cell that fits in one piece — most of them — lands exactly as it
    // always did.
    if lod.partial.is_none() {
        let mut done = lod.stream.take_results();
        let deferred = done.split_off(done.len().min(MAX_BUILDS_PER_FRAME));
        lod.stream.defer(deferred);
        for (c, built) in done {
            match built {
                Ok(Some((grid_cell, pieces, footprints, dated, doors, heights))) => {
                    // The whole cell's bookkeeping lands with its first frame:
                    // colliders are cheap CPU state (an avatar can bump into a
                    // building a frame before it is drawn, which beats walking
                    // through one that is), and `succeeded` marks the stream done
                    // — the settle harness sees the tail through
                    // `streaming_busy`'s `partial` term instead.
                    colliders.set_cell(c, footprints);
                    lod.stream.succeeded(&c);
                    lod.cells.insert(c, Vec::new());
                    lod.dated.insert(c, dated);
                    lod.doors.insert(c, doors);
                    lod.heights.insert(c, heights.into_iter().collect());
                    lod.partial = Some(PartialCell {
                        cell: c,
                        grid_cell,
                        pieces,
                    });
                }
                // Empty cell; stays requested so it isn't re-queried.
                Ok(None) => lod.stream.succeeded(&c),
                Err(err) => {
                    tracing::warn!(cell = ?c, error = %err, "lod22 cell failed");
                    // Drop any cached copy. The fetch no longer validates a cache
                    // hit by parsing it (that parse moved to a worker), so this is
                    // what stops a corrupt entry being re-served forever: whatever
                    // failed, the stored text is the one thing we can invalidate.
                    // Harmless when the failure came from the network instead —
                    // the key simply isn't there.
                    let key = cell_cache_key(c.0, c.1);
                    IoTaskPool::get()
                        .spawn(
                            async move { storage::remove(storage::Namespace::Cache, &key).await },
                        )
                        .detach();
                    lod.stream.failed(&c, now); // allows a bounded re-queue later
                }
            }
        }
    }

    // Drain the cell in application: spawn the pieces the frame's ledger admits,
    // park the rest for the next frame.
    if let Some(mut partial) = lod.partial.take() {
        let material = lod
            .material
            .get_or_insert_with(|| {
                materials.add(BuildingMaterial {
                    base: StandardMaterial {
                        base_color: Color::WHITE,
                        perceptual_roughness: 0.95,
                        double_sided: !BACKFACE_CULL,
                        cull_mode: if BACKFACE_CULL {
                            Some(bevy::render::render_resource::Face::Back)
                        } else {
                            None
                        },
                        ..default()
                    },
                    extension: BuildingWindows::default(),
                })
            })
            .clone();
        let interior_material = lod
            .interior_material
            .get_or_insert_with(|| {
                materials.add(BuildingMaterial {
                    base: StandardMaterial {
                        base_color: Color::WHITE,
                        perceptual_roughness: 0.95,
                        // The one difference from the shells' material, and the
                        // whole reason there are two: you can be inside one of
                        // these, where the wall in front of you shows its back.
                        double_sided: true,
                        cull_mode: None,
                        ..default()
                    },
                    extension: BuildingWindows::default(),
                })
            })
            .clone();
        while let Some((mesh, _)) = partial.pieces.first() {
            if !crate::systems::stream_ledger::admit(crate::systems::stream_ledger::mesh_cost(mesh))
            {
                break;
            }
            let (mesh, inside) = partial.pieces.remove(0);
            // One entity per piece, flat under the same root rather than a parent
            // with children: every consumer — the shadow-LOD toggle,
            // `HideInSpace`, the despawn queue — acts per mesh entity, and a
            // parent would mean each of them learning to walk children for a
            // hierarchy with nothing else to do.
            let mut entity = commands.spawn((
                Mesh3d(meshes.add(mesh)),
                MeshMaterial3d(if inside {
                    interior_material.clone()
                } else {
                    material.clone()
                }),
                Transform::from_translation(Vec3::new(0.0, 0.05, 0.0)),
                partial.grid_cell,
                ChildOf(root),
                Lod22Mesh,
                HideInSpace,
            ));
            // A late piece of a cell the shadow-LOD pass has already pushed past
            // SHADOW_RADIUS: the pass only acts on boundary *crossings*, so this
            // entity must arrive already matching its cell's state.
            if lod.no_shadow.contains(&partial.cell) {
                entity.insert(bevy::light::NotShadowCaster);
            }
            let id = entity.id();
            lod.cells.entry(partial.cell).or_default().push(id);
        }
        if partial.pieces.is_empty() {
            // The far tier may already hold this cell — queued before the
            // coverage manifest loaded, or while the 3DBAG toggle was off. Evict
            // only now that every piece is up, or the far mesh vanishes while the
            // real one is still arriving in slices.
            osm.evict_far_cell(partial.cell);
            tracing::info!(
                cell = ?partial.cell,
                live = lod.cells.len(),
                pieces = lod.cells.get(&partial.cell).map_or(0, Vec::len),
                "lod22 cell rendered"
            );
        } else {
            lod.partial = Some(partial);
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
        lod.dated.remove(&c);
        lod.doors.remove(&c);
        lod.built_detail.remove(&c);
        lod.heights.remove(&c);
        if let Some(entities) = lod.cells.remove(&c) {
            for e in entities {
                lod.stream.retire(e);
            }
        }
        // A cell can leave the ring mid-application; its parked pieces must go
        // with it or they keep spawning into a cell whose entities were just
        // retired.
        if lod.partial.as_ref().is_some_and(|p| p.cell == c) {
            lod.partial = None;
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
    facts: &crate::systems::building_facts::BagCell,
    lod3_text: Option<&str>,
    detail: load_control::Detail,
) -> Result<Option<Lod22Build>, String> {
    let shell = parse_lod22(text)?;
    // An imported-model cell, when the host serves one. A failure to parse it is
    // not a failure of the cell: the shells are what this lane exists to draw, and
    // losing them because somebody's model file is malformed would be the wrong
    // trade entirely.
    let models = lod3_text.and_then(|t| match parse_lod22(t) {
        Ok(cell) => Some(cell),
        Err(e) => {
            tracing::warn!(cx, cy, error = %e, "lod3 cell unreadable; drawing the shells");
            None
        }
    });
    let replaced: HashSet<&str> = models
        .as_ref()
        .map(|m| crate::systems::lod3::replaced_pands(&m.pands))
        .unwrap_or_default();
    let elements = shell.elements;
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
    // Imported models mesh into their own buffer: they are drawn with culling off
    // (see [`Piece`]) and so cannot share a mesh with the shells.
    let mut interior = MeshData::with_dedup();
    // One footprint polygon per building, in world XZ (for avatar collision).
    let mut footprints: Vec<crate::systems::collision::Footprint> = Vec::new();
    // Buildings the register has struck off, for the log line below — a silent
    // subtraction from the city is exactly the kind of thing to be able to check.
    let mut struck = 0usize;
    // How many drawn buildings the BAG join reached, and how many were drawn.
    let mut dated = (0usize, 0usize);
    let mut drawn_doors = 0usize;
    // How tall each drawn shell is, for `systems::interiors` to divide into
    // storeys. Measured here because this is the only lane that ever sees the
    // height: it arrives on the vertices, is baked into a merged mesh, and is
    // unrecoverable from anything downstream.
    let mut heights: ShellHeights = Vec::new();
    for b in &elements {
        // The server bins each building to the cell owning its first vertex; keep
        // the same half-open ownership so a cross-edge building renders once.
        let (blat, blng, _) = b.p[0];
        if blat < south || blat >= north || blng < west || blng >= east {
            continue;
        }
        // A building somebody has modelled is drawn from the model instead, at
        // the bottom of this function.
        if !b.id.is_empty() && replaced.contains(b.id.as_str()) {
            continue;
        }
        // What the register says about this building, when the host serves the
        // BAG tree and this element carries an id. `None` everywhere else, and
        // every read below has the old behaviour as its `None` arm.
        let fact = (!b.id.is_empty()).then(|| facts.building(&b.id)).flatten();
        dated.1 += 1;
        if fact.is_some() {
            dated.0 += 1;
        }
        // A pand the register has struck off is not drawn. 3DBAG's solids are a
        // dated snapshot and the BAG is refreshed far more often, so "demolished"
        // here means the building came down after the mesh was cut — the one case
        // where the two sources disagree and the register is certainly right.
        // Nothing weaker qualifies: a permit is not a demolition and an empty
        // building is still a building.
        if matches!(
            fact.map(|f| f.status),
            Some(crate::systems::building_facts::BuildingStatus::Demolished)
                | Some(crate::systems::building_facts::BuildingStatus::NotRealised)
        ) {
            struck += 1;
            continue;
        }
        // Only a shell has a storey datum worth taking. An imported model is many
        // elements and brings its own floors (`systems::lod3`), so measuring one
        // would report the height of a wall panel as the height of a building.
        if b.class.is_empty() && !b.id.is_empty() {
            if let Some(h) = shell_datum_height(&b.p, &b.t) {
                heights.push((b.id.clone(), h));
            }
        }
        let (wall, roof_tile) = crate::systems::osm_buildings::building_palette_dated(
            blat,
            blng,
            fact.and_then(|f| f.year),
        );
        // Alpha is not opacity here — the material is opaque and `alpha_discard`
        // flattens it — it is the per-building night-occupancy scale the window
        // shader reads out of `base_color.a`, the same free channel
        // `globe_ocean` puts its sea mask in. 1.0 for a building we know nothing
        // about, which is exactly the look this lane had before.
        let lit = crate::systems::osm_buildings::night_lit_scale(
            fact.map_or(0, |f| f.uses),
            fact.map_or(0, |f| f.units),
        );
        let wall = [wall[0], wall[1], wall[2], lit];
        let roof_tile = [roof_tile[0], roof_tile[1], roof_tile[2], lit];
        // `FLAT_ROOF_COLOR` is authored in sRGB; vertex colours multiply in
        // linear space (same exit rule as `building_palette`).
        let flat_lin = crate::systems::scatter::srgb_to_linear_f32(FLAT_ROOF_COLOR);
        let flat = [flat_lin[0], flat_lin[1], flat_lin[2], lit];

        // World-XZ position of each vertex (before the cell offset).
        let world_xz: Vec<Vec2> =
            b.p.iter()
                .map(|&(la, lo, _)| lat_lng_to_local(la, lo, ax, ay, z, tile_m))
                .collect();
        // The register's own outline where we have it, and the convex hull of the
        // mesh where we do not.
        //
        // The hull is what a triangle soup can give and it is wrong for every
        // building that is not convex: an L-shaped block's hull fills in the inner
        // corner, so the yard between its wings is a place the avatar cannot walk
        // and nothing is drawn. Now that cartopy's `bag.ts` ships the ring, that
        // approximation is only the fallback.
        let ring: Vec<Vec2> = fact
            .map(|f| f.footprint.as_slice())
            .unwrap_or_default()
            .iter()
            .map(|&(la, lo)| lat_lng_to_local(la, lo, ax, ay, z, tile_m))
            .collect();
        let doors: Vec<Vec2> = fact
            .map(|f| f.doors.as_slice())
            .unwrap_or_default()
            .iter()
            .map(|&(la, lo)| lat_lng_to_local(la, lo, ax, ay, z, tile_m))
            .collect();
        let outline = if ring.len() >= 3 {
            ring
        } else {
            crate::systems::collision::convex_hull_xz(world_xz.clone())
        };
        if let Some(fp) = crate::systems::collision::Footprint::with_doors(outline.clone(), &doors)
        {
            footprints.push(fp);
        }

        // A door on the facade at each entrance the register lists.
        //
        // It is drawn *proud* of the wall rather than cut into it: a hole needs
        // the wall to be re-triangulated around it, and these walls arrive as a
        // finished soup. A panel standing 4 cm off the brick reads as a doorway
        // at every distance this is visible from, and cannot leave a crack.
        //
        // Note the height. `building_windows.wgsl` paints its lattice only above
        // `FIRST_SILL_M` (2.2 m), so a door topping out below that is in the one
        // band of the facade the window shader leaves alone — otherwise every
        // front door in the city would have window panes painted across it.
        // Skipped on a far cell — the *drawn* panel only. The doors still reach
        // `Footprint::with_doors` above, because a collider is CPU state that
        // costs nothing to carry and a building sealed at range would be a
        // building the avatar could not walk into on the frame the cell was
        // promoted.
        if detail == load_control::Detail::Near && !doors.is_empty() && outline.len() >= 3 {
            let base = ground.level_over(outline.iter().map(|w| [w.x, w.y]));
            for &door in &doors {
                let Some((at, normal)) = nearest_edge_point(&outline, door) else {
                    continue;
                };
                let side = Vec2::new(-normal.y, normal.x) * DOOR_WIDTH_M * 0.5;
                let out = normal * DOOR_PROUD_M;
                let foot =
                    |p: Vec2, y: f32| Vec3::new(p.x + out.x - ox, y + base, p.y + out.y - oz);
                let (l, r) = (at - side, at + side);
                buf.quad_facing(
                    foot(l, 0.0),
                    foot(r, 0.0),
                    foot(r, DOOR_HEIGHT_M),
                    foot(l, DOOR_HEIGHT_M),
                    DOOR_COLOR,
                    Vec3::new(normal.x, 0.0, normal.y),
                );
                drawn_doors += 1;
            }
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
    if struck > 0 {
        tracing::debug!(
            cx,
            cy,
            struck,
            "BAG: skipped buildings the register has struck off"
        );
    }
    // …and now the models, element by element. They share the palette and the
    // ground datum with the shells and differ in three ways: the whole model is
    // one building rather than one element being one, only its wall-like elements
    // collide, and its surfaces are drawn from both sides.
    if let Some(models) = &models {
        for b in &models.elements {
            let (blat, blng, _) = b.p[0];
            let fact = (!b.id.is_empty()).then(|| facts.building(&b.id)).flatten();
            let (wall, roof_tile) = crate::systems::osm_buildings::building_palette_dated(
                blat,
                blng,
                fact.and_then(|f| f.year),
            );
            let lit = crate::systems::osm_buildings::night_lit_scale(
                fact.map_or(0, |f| f.uses),
                fact.map_or(0, |f| f.units),
            );
            let wall = [wall[0], wall[1], wall[2], lit];
            let roof_tile = [roof_tile[0], roof_tile[1], roof_tile[2], lit];

            let world_xz: Vec<Vec2> =
                b.p.iter()
                    .map(|&(la, lo, _)| lat_lng_to_local(la, lo, ax, ay, z, tile_m))
                    .collect();
            // Only the walls. A slab's footprint is the whole storey, so hulling
            // one would fill the building with an invisible block — which is
            // exactly the collider a shell has and a model exists to lose.
            if crate::systems::lod3::is_solid_class(&b.class) {
                if let Some(fp) = crate::systems::collision::Footprint::new(
                    crate::systems::collision::convex_hull_xz(world_xz.clone()),
                ) {
                    footprints.push(fp);
                }
            }
            let base = ground.level_over(world_xz.iter().map(|w| [w.x, w.y]));
            let local: Vec<Vec3> =
                b.p.iter()
                    .zip(&world_xz)
                    .map(|(&(_, _, h), &w)| Vec3::new(w.x - ox, h + base, w.y - oz))
                    .collect();
            for &(i, j, k, is_roof) in &b.t {
                let (a, bb, c) = (local[i], local[j], local[k]);
                let color = if is_roof == 1 { roof_tile } else { wall };
                interior.tri(a, bb, c, color);
            }
        }
    }

    let max_bytes = max_cell_mesh_bytes();
    let mut pieces: Vec<Piece> = buf
        .into_meshes(max_bytes)
        .into_iter()
        .map(|m| (m, false))
        .collect();
    pieces.extend(
        interior
            .into_meshes(max_bytes)
            .into_iter()
            .map(|m| (m, true)),
    );
    Ok(
        (!pieces.is_empty()).then_some((
            grid_cell,
            pieces,
            footprints,
            dated,
            drawn_doors,
            heights,
        )),
    )
}

/// The height a building's storeys are counted against, metres above its own
/// base — or `None` for a soup with no roof to measure.
///
/// **Not the maximum vertex height, which is the ridge.** A pitched Dutch house
/// carries three or four metres of roof over its top ceiling, so dividing the
/// ridge by a storey invents a floor in the rafters. This is the 70th percentile
/// of the roof surface's own vertices — the same datum 3DBAG publishes as
/// `b3_h_dak_70p`, which is what it publishes *because* it approximates the
/// eaves on a pitched roof and is exact on a flat one.
///
/// Vertices are taken from the roof triangles rather than from `p` wholesale: the
/// wall vertices run from the ground up, so including them would drag the
/// percentile down by however much wall the building has, i.e. by its height.
fn shell_datum_height(p: &[(f64, f64, f32)], t: &[(usize, usize, usize, u8)]) -> Option<f32> {
    let mut hs: Vec<f32> = Vec::new();
    for &(i, j, k, is_roof) in t {
        if is_roof != 1 {
            continue;
        }
        for ix in [i, j, k] {
            if let Some(&(_, _, h)) = p.get(ix) {
                hs.push(h);
            }
        }
    }
    // A cell that predates the roof flag tags nothing, and a flat-roofed solid is
    // the same height whichever vertices are taken; falling back to all of them
    // beats answering "no height" and flattening the building to one storey.
    if hs.is_empty() {
        hs.extend(p.iter().map(|&(_, _, h)| h));
    }
    if hs.is_empty() {
        return None;
    }
    hs.sort_by(f32::total_cmp);
    let at = ((hs.len() - 1) as f32 * 0.7).round() as usize;
    Some(hs[at].max(0.0))
}

/// Cache key for a LoD2.2 cell (its own namespace under the cache root).
///
/// **The `v2` is a format revision and it is load-bearing.** These cells used to
/// carry a mangled integer where the BAG identificatie should be, and a client
/// that already ran holds those in its cache. Nothing about them fails to parse —
/// the id visitor reads a legacy number and reports "no id" — so a cached cell
/// keeps a building out of every register join for as long as the entry survives,
/// while the map looks entirely normal. Measured on 2026-08-28 with the host
/// serving correct ids: `lod22_dated` was 0 of 75,109 buildings, purely from the
/// cache. Bump this whenever the element format changes in a way an old entry
/// cannot express.
fn cell_cache_key(cx: i64, cy: i64) -> String {
    format!("buildings/lod22/v2/{}/{}/{}.json", LOD22_ZOOM, cx, cy)
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
fn parse_lod22(text: &str) -> Result<CellFile, String> {
    #[derive(serde::Deserialize)]
    struct File {
        elements: Vec<El>,
        /// Only an imported-model cell carries this; a shell cell has no such key
        /// and `serde` fills the default.
        #[serde(default)]
        pands: Vec<String>,
    }
    #[derive(serde::Deserialize)]
    struct El {
        #[serde(default)]
        id: WireId,
        #[serde(default)]
        p: Vec<(f64, f64, f32)>,
        #[serde(default)]
        t: Vec<Tri>,
        /// The IFC class, on an imported model only. Absent on every shell, which
        /// is what the default is for.
        #[serde(default)]
        class: String,
    }
    /// The element's BAG id, which is a **string** since the extractor stopped
    /// mangling it into an integer — and a number in every cell file written
    /// before that, including the ones already sitting in users' caches (a cache
    /// entry is evicted when it fails to parse, so a strict `String` here would
    /// silently re-fetch every cell on the first run after an upgrade). Absent
    /// entirely in the oldest files, which `Default` reads as "no id".
    #[derive(Default)]
    struct WireId(String);
    impl<'de> serde::Deserialize<'de> for WireId {
        fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
            struct V;
            impl serde::de::Visitor<'_> for V {
                type Value = WireId;
                fn expecting(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
                    f.write_str("a BAG identificatie as a string, or a legacy number")
                }
                fn visit_str<E: serde::de::Error>(self, v: &str) -> Result<WireId, E> {
                    Ok(WireId(v.to_string()))
                }
                fn visit_u64<E: serde::de::Error>(self, _: u64) -> Result<WireId, E> {
                    // A legacy id joins with nothing — it was the digits of the
                    // identificatie shifted by the part suffix — so it is dropped
                    // rather than carried as a key that would never match.
                    Ok(WireId(String::new()))
                }
                fn visit_i64<E: serde::de::Error>(self, _: i64) -> Result<WireId, E> {
                    Ok(WireId(String::new()))
                }
            }
            d.deserialize_any(V)
        }
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
            out.push(Bldg {
                id: el.id.0,
                p,
                t,
                class: el.class,
            });
        }
    }
    Ok(CellFile {
        elements: out,
        pands: file.pands,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A square, wound both ways. The register does not promise a winding, and a
    /// door placed on the *inward* normal is drawn inside its own building, where
    /// the shell hides it completely — the door count says 113,710 and the street
    /// shows none. That is exactly what happened, and it is why this is a test and
    /// not a comment.
    #[test]
    fn a_door_faces_out_of_the_building_whichever_way_the_ring_is_wound() {
        let square = vec![
            Vec2::new(0.0, 0.0),
            Vec2::new(10.0, 0.0),
            Vec2::new(10.0, 10.0),
            Vec2::new(0.0, 10.0),
        ];
        let centre = Vec2::new(5.0, 5.0);
        for ring in [square.clone(), square.into_iter().rev().collect::<Vec<_>>()] {
            // A door against each of the four walls in turn.
            for probe in [
                Vec2::new(5.0, 0.5),
                Vec2::new(9.5, 5.0),
                Vec2::new(5.0, 9.5),
                Vec2::new(0.5, 5.0),
            ] {
                let (at, normal) = nearest_edge_point(&ring, probe).expect("a square has edges");
                let outward = (at - centre).normalize();
                assert!(
                    normal.dot(outward) > 0.5,
                    "normal {normal:?} at {at:?} points into the building, not out of it"
                );
            }
        }
    }

    /// The point must land *on* the wall, not at a corner or out in the street.
    #[test]
    fn a_door_lands_on_the_wall_nearest_it() {
        let ring = vec![
            Vec2::new(0.0, 0.0),
            Vec2::new(10.0, 0.0),
            Vec2::new(10.0, 6.0),
            Vec2::new(0.0, 6.0),
        ];
        let (at, _) = nearest_edge_point(&ring, Vec2::new(3.0, 1.2)).expect("edges");
        assert!(
            (at.y - 0.0).abs() < 1e-4,
            "expected the south wall, got {at:?}"
        );
        assert!((at.x - 3.0).abs() < 1e-4);
    }
}
