//! 3DBAG **LoD2.2** real building meshes (ADR-027).
//!
//! Where the OSM/LoD1.2 path (`osm_buildings.rs`) extrudes footprints and *guesses*
//! roofs, this streams the actual triangulated BAG roof geometry (sloped, hipped,
//! complex roofs) preprocessed server-side into per-cell meshes
//! (atlas's `pipeline/lod22.rs` →`https://3dbag.cartopolis.org/lod22/…`). Each cell file is
//! `{"elements":[{id, p:[[lat,lng,h],…], t:[[i,j,k,roof],…]}]}` — deduped vertices
//! and triangles tagged roof/wall.
//!
//! **The same cell also comes as a binary, and that is what is asked for first**
//! (`lod22/14/{x}/{y}.bin`, [`cartopolis_geo::lod22_cell`] — the one definition
//! atlas writes it with and this module reads it with). It carries exactly the
//! JSON's numbers on the JSON's own 10⁻⁷°/1 cm grid, in a fifth of the bytes
//! (half, gzipped) and with no float text to parse; `docs/notes/atlas.md` has
//! the measurements.
//! [`fetch_lod22_body`] falls back to the JSON on a 404 or any failure and latches
//! the binary off for the session after [`MAX_BIN_FAILURES`], so a host that has
//! not re-run the pipeline costs a handful of 404s. Both formats end in
//! [`cell_file`], so nothing downstream of the parse can tell them apart, and each
//! is cached under its own key so neither parser is ever handed the other's body.
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
use crate::systems::cell_ring;
use crate::systems::globe::{GlobeState, GLOBE_SHOW_ALT};
use crate::systems::load_control;
use crate::systems::map_stream::MapStream;
use crate::systems::osm_buildings::{lat_lng_to_local, lod22_covers, tdbag_url, MeshData};
use cartopolis_core::world::{lat_lng_to_tile, tile_to_lat_lng, tile_to_meters};

/// Fixed cache/streaming zoom — the cells the server binned into, and the cells
/// `osm_buildings` skips; the same constant both of those read.
const LOD22_ZOOM: u32 = cartopolis_geo::formats::CACHE_ZOOM;

/// Concurrent cell fetch+mesh tasks. Our own 3DBAG host has no external rate limit,
/// so this is a citizenship/throughput cap, not a throttle (min-interval 0). Six,
/// not three: the fetch is the long pole of a cold load (the 5×5 ring took
/// ceil(25/3) serial rounds of network), the parse runs on the worker pool, and
/// the GPU guard is the per-frame upload cap, not this.
///
/// Nine, the near 3×3 ring, for the reason `map_geometry::SURFACE_MAX_IN_FLIGHT`
/// gives: over graded ground a job parks on the terrain hold holding its slot,
/// and at six the last three cells of the ring could not start downloading until
/// the hold was over. The mesh build is bounded separately, by the heavy lane.
const LOD22_CONCURRENCY: usize = 9;
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

/// The LoD2.2 ring where atlas also serves the cell as LoD1.2 blocks: 1 (3×3,
/// ~±2.2 km). Past it those blocks stand in, out to `far_cell_radius()`
/// (`level-of-detail-tile-tree.md`, step 4). A LoD2.2 cell is the heaviest thing
/// streamed — up to 63 MB for Groningen's densest (`upload-bursts.md`) — and at
/// 2–3.5 km its shells and the blocks cover the same pixels.
const LOD22_NEAR_RADIUS: i64 = 1;

/// How far out this cell is drawn as LoD2.2: [`LOD22_NEAR_RADIUS`] where the
/// blocks can take over from it, and the whole [`lod22_radius`] where they
/// cannot. The rest of that ring would then be a hole or, where atlas has not cut
/// blocks, an Overpass fetch for the same buildings.
#[must_use]
pub fn lod22_ring(cx: i64, cy: i64) -> i64 {
    if crate::systems::osm_buildings::tdbag_covers(cx, cy) {
        load_control::radius(LOD22_NEAR_RADIUS).min(lod22_radius())
    } else {
        lod22_radius()
    }
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
pub(crate) const FLAT_ROOF_COLOR: [f32; 3] = [0.52, 0.52, 0.54];
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

/// A doorway built onto a wall face: the leaf set back inside a surround that
/// stands off the wall, reveals on three sides of the recess, and a step.
///
/// **Built proud, not cut in.** The wall is a finished triangle soup and a hole
/// would mean re-triangulating it; the surround instead stands
/// [`SURROUND_PROUD_M`] off the brick and the leaf sits back inside it, so the
/// recess is real geometry — its reveals take the sun and throw a shadow onto
/// the leaf — without touching the wall. Placed on the wall's own plane (the
/// face `WallFaces` found), not on the register's outline, which can run a few
/// centimetres off the surveyed wall and hung doors in the air or inside it.
///
/// Every part carries a negative UV0.y, which `building_windows.wgsl` reads as
/// "not a wall": the surround and step are drawn plain, the leaf with its
/// panels, glass and handle from its own coordinates (`x` across it plus ten
/// times its style, `-1 - height` up it).
fn push_doorway(
    buf: &mut MeshData,
    face: &crate::systems::building_windows::WallFace,
    at: Vec2,
    base: f32,
    origin: Vec2,
    era: u8,
    uses: u16,
) {
    let n = face.outward;
    let t = face.tangent();
    let p = |u: f32, y: f32, out: f32| {
        let q = at + t * u + n * out;
        Vec3::new(q.x - origin.x, base + y, q.y - origin.y)
    };
    let (n3, t3) = (Vec3::new(n.x, 0.0, n.y), Vec3::new(t.x, 0.0, t.y));
    let w = DOOR_WIDTH_M * 0.5;
    let h = DOOR_HEIGHT_M;
    let (fw, proud, leaf_out) = (SURROUND_M, SURROUND_PROUD_M, 0.015);
    let plain = [0.0, -0.5];
    let shop = uses & crate::systems::building_facts::uses::SHOP != 0;
    // The style of the leaf: panelled with a glass transom before the war,
    // flush with a glass strip in the system-built decades, glass in a shop or
    // a new building.
    let style: u8 = if shop || era == 4 {
        1
    } else if era == 3 || era == 0 {
        2
    } else {
        0
    };
    // Surround: painted timber on an old house, dark aluminium on a new one.
    let surround = if era == 1 || era == 2 {
        [0.58, 0.56, 0.5, 1.0]
    } else {
        [0.045, 0.045, 0.05, 1.0]
    };
    // Front doors are painted, and not all the same colour: picked by where the
    // door is, so it is stable from one build to the next.
    const LEAVES: [[f32; 4]; 6] = [
        [0.018, 0.06, 0.03, 1.0],
        [0.02, 0.03, 0.085, 1.0],
        [0.13, 0.018, 0.02, 1.0],
        [0.014, 0.014, 0.016, 1.0],
        [0.16, 0.075, 0.03, 1.0],
        [0.2, 0.2, 0.19, 1.0],
    ];
    let pick = ((at.x * 7.13 + at.y * 3.71).abs() as usize) % LEAVES.len();
    let leaf = if style == 1 {
        [0.05, 0.05, 0.055, 1.0]
    } else {
        LEAVES[pick]
    };
    let step = [0.26, 0.26, 0.24, 1.0];
    let s = 10.0 * f32::from(style);

    // The leaf, set back in the opening.
    buf.quad_facing_uv(
        [
            p(-w, 0.0, leaf_out),
            p(w, 0.0, leaf_out),
            p(w, h, leaf_out),
            p(-w, h, leaf_out),
        ],
        leaf,
        n3,
        [
            [s, -1.0],
            [s + 2.0 * w, -1.0],
            [s + 2.0 * w, -1.0 - h],
            [s, -1.0 - h],
        ],
    );
    // The recess: two reveals and the soffit over the leaf.
    buf.quad_facing_uv(
        [
            p(-w, 0.0, leaf_out),
            p(-w, 0.0, proud),
            p(-w, h, proud),
            p(-w, h, leaf_out),
        ],
        surround,
        t3,
        [plain; 4],
    );
    buf.quad_facing_uv(
        [
            p(w, 0.0, leaf_out),
            p(w, 0.0, proud),
            p(w, h, proud),
            p(w, h, leaf_out),
        ],
        surround,
        -t3,
        [plain; 4],
    );
    buf.quad_facing_uv(
        [
            p(-w, h, leaf_out),
            p(w, h, leaf_out),
            p(w, h, proud),
            p(-w, h, proud),
        ],
        surround,
        -Vec3::Y,
        [plain; 4],
    );
    // The surround's face: two jambs and a head.
    let (ow, top) = (w + fw, h + fw);
    for (u0, u1, y0, y1) in [(-ow, -w, 0.0, top), (w, ow, 0.0, top), (-w, w, h, top)] {
        buf.quad_facing_uv(
            [
                p(u0, y0, proud),
                p(u1, y0, proud),
                p(u1, y1, proud),
                p(u0, y1, proud),
            ],
            surround,
            n3,
            [plain; 4],
        );
    }
    // …and its outer sides and top, back to the wall.
    buf.quad_facing_uv(
        [
            p(-ow, 0.0, 0.0),
            p(-ow, 0.0, proud),
            p(-ow, top, proud),
            p(-ow, top, 0.0),
        ],
        surround,
        -t3,
        [plain; 4],
    );
    buf.quad_facing_uv(
        [
            p(ow, 0.0, 0.0),
            p(ow, 0.0, proud),
            p(ow, top, proud),
            p(ow, top, 0.0),
        ],
        surround,
        t3,
        [plain; 4],
    );
    buf.quad_facing_uv(
        [
            p(-ow, top, 0.0),
            p(ow, top, 0.0),
            p(ow, top, proud),
            p(-ow, top, proud),
        ],
        surround,
        Vec3::Y,
        [plain; 4],
    );
    // A step, a little wider than the surround.
    let (sw, sd, sh) = (ow + 0.1, STEP_DEPTH_M, STEP_HEIGHT_M);
    buf.quad_facing_uv(
        [
            p(-sw, sh, 0.0),
            p(sw, sh, 0.0),
            p(sw, sh, sd),
            p(-sw, sh, sd),
        ],
        step,
        Vec3::Y,
        [plain; 4],
    );
    buf.quad_facing_uv(
        [
            p(-sw, 0.0, sd),
            p(sw, 0.0, sd),
            p(sw, sh, sd),
            p(-sw, sh, sd),
        ],
        step,
        n3,
        [plain; 4],
    );
    for (u, facing) in [(-sw, -t3), (sw, t3)] {
        buf.quad_facing_uv(
            [p(u, 0.0, 0.0), p(u, 0.0, sd), p(u, sh, sd), p(u, sh, 0.0)],
            step,
            facing,
            [plain; 4],
        );
    }
}

/// How far the door surround stands off the wall, and how wide it is. The
/// recess is the difference between this and the leaf's own 15 mm.
const SURROUND_PROUD_M: f32 = 0.09;
const SURROUND_M: f32 = 0.11;
/// The front step.
const STEP_DEPTH_M: f32 = 0.32;
const STEP_HEIGHT_M: f32 = 0.16;

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
    /// A model element's own colour, sRGB (`lod3` only); `None` takes the
    /// building's palette.
    rgb: Option<[u8; 3]>,
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

/// Each drawn shell's BAG id and what its geometry measures. See [`ShellMeasure`]
/// and [`shell_stats`] — this is the only place in the client that knows how tall
/// a building actually is, or how much of it there is, because all of that exists
/// only on the LoD2.2 vertices and is thrown away the moment the mesh is merged.
type ShellStats = Vec<(String, ShellMeasure)>;

type Lod22Build = (
    GridCell,
    Vec<Piece>,
    Vec<crate::systems::collision::Footprint>,
    DatedCount,
    DoorCount,
    ShellStats,
);

/// The mesh assets a live cell's entities are drawn from, kept beside the
/// entities so that retiring the cell can hold on to them.
///
/// The entities are what leave; the meshes are what were expensive. Reading them
/// back off the entities at retire time would need a `Query<&Mesh3d>` and would
/// find nothing for a cell whose entities are already in the despawn queue, so
/// they are recorded here as each piece is spawned.
struct LiveMeshes {
    grid_cell: GridCell,
    /// One per piece: the mesh, and whether it is an imported model's interior
    /// (which picks the double-sided material).
    pieces: Vec<(Handle<Mesh>, bool)>,
    /// What these meshes occupy on the GPU, by `upload_meter`'s own reckoning —
    /// so the pool is bounded in the units `resident_bytes_target` is in.
    bytes: usize,
}

/// A retired cell held whole, so returning to it is a re-spawn.
///
/// Everything here was thrown away on the way out and rebuilt on the way back
/// in: the meshes (a registry fetch, a serde parse and a merge on a worker), the
/// collision footprints, the register join tallies and the storey data. Only the
/// entities are not kept — they are cheap, and an entity that exists but is not
/// drawn is a thing every other system would have to learn about.
struct WarmCell {
    meshes: LiveMeshes,
    footprints: Vec<crate::systems::collision::Footprint>,
    dated: DatedCount,
    doors: DoorCount,
    heights: HashMap<String, ShellMeasure>,
    detail: load_control::Detail,
}

impl crate::platform::warm::WarmSize for WarmCell {
    fn warm_bytes(&self) -> usize {
        // The mesh bytes and nothing else. The footprints and the tallies beside
        // them are kilobytes of CPU state, and counting those into a ceiling that
        // exists to bound *graphics* memory would make the number mean two
        // things at once.
        self.meshes.bytes
    }
}

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
    /// Each result carries the terrain revision its cell was built on.
    stream: CellStream<(i64, i64), (Result<Option<Lod22Build>, String>, u32)>,
    /// Spawned cell entities, despawned when out of range — one per mesh piece a
    /// cell was cut into ([`MAX_CELL_MESH_BYTES`]), which for most cells is still
    /// exactly one.
    cells: HashMap<(i64, i64), Vec<Entity>>,
    /// The terrain revision each live cell stands on: a new field rebuilds only
    /// the cells built on an older one (`MapSurfaces::built_rev` is the same).
    built_rev: HashMap<(i64, i64), u32>,
    /// What each live cell's ground was when it was built.
    built_digest: HashMap<(i64, i64), u64>,
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
    /// BAG, LoD3 and roofs coverage revisions these cells were built against
    /// (`building_facts::coverage_revision` and its two siblings) — 0 while a
    /// manifest is still in flight, counting up with each different answer. The era palette and the window occupancy
    /// are baked into vertex colours, so cells built during that gap carry the
    /// generated look and have to be rebuilt once the answer arrives. With the
    /// copy kept from the last launch answering first, that is only when the
    /// network disagrees with it.
    bag_revision: (u32, u32, u32),
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
    /// Each loaded shell's measurements ([`ShellMeasure`]), keyed by BAG
    /// identificatie. Per cell so they are evicted with the cell they were
    /// measured from, exactly as the colliders are.
    heights: HashMap<(i64, i64), HashMap<String, ShellMeasure>>,
    /// Terrain-field revision these cells were built against. The field arrives a
    /// second or two after the anchor, so the first cells are always built on flat
    /// ground and have to be rebuilt onto the measured one.
    terrain_revision: u32,
    /// The one cell mid-application, if any. Dropped by every teardown that
    /// drops `cells` — a parked piece from the old anchor spawning into the new
    /// world would stand in the wrong place for the life of the ring.
    partial: Option<PartialCell>,
    /// The mesh handles behind each live cell's entities, so a retirement can
    /// keep them.
    live: HashMap<(i64, i64), LiveMeshes>,
    /// Cells that have left the ring but whose meshes are still held, bounded by
    /// [`crate::quality::Budget::warm_gpu_bytes`].
    ///
    /// **This is the one cache in the client that spends graphics memory**, and
    /// the three rules that make that safe live with the budget field rather
    /// than here. The one this module owes: it is shed on demand
    /// (`load_control::take_shed`), and shedding must be complete rather than
    /// partial — a controller that asked for room and got a quarter of it would
    /// go on to narrow the ring anyway, having paid for both.
    retained: crate::platform::warm::WarmStore<(i64, i64), WarmCell>,
    /// Cells re-spawned out of [`Self::retained`] over the life of the process,
    /// for `--dump-state`.
    warm_restored: u32,
    /// The building-design stamps each cell was built under
    /// (`building_designs::neighbourhood_revision`). A designed building's
    /// entrances are baked into its facade and collider here, so an edit has to
    /// rebuild the cell it stands in — and only that cell, not the ring.
    design_rev: HashMap<(i64, i64), u64>,
    /// `building_designs::manifest_revision` last compared against.
    design_manifest_seen: u64,
}

impl crate::systems::stream_watch::StreamBusy for Lod22Buildings {
    const NAME: &'static str = "lod22";
    fn stream_busy(&self) -> bool {
        self.streaming_busy()
    }
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

    /// Whether this cell's LoD2.2 shells are standing. The far blocks stand in for
    /// a cell until they are, so moving it inwards swaps one for the other
    /// rather than opening a hole while the shells load (`osm_buildings`).
    #[must_use]
    pub fn holds(&self, cell: (i64, i64)) -> bool {
        self.cells.contains_key(&cell)
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

    /// Building cells re-spawned out of the retained-mesh pool over the life of
    /// the process, and what that pool is holding now.
    ///
    /// For `--dump-state`, and both halves are machine-independent: a count of
    /// cells and a count of asset bytes, neither of which the adapter touches.
    /// The second is a *share* of `gpu_resident_kb` rather than an addition to
    /// it, so a capture that reports 300 MB resident with 80 MB retained is
    /// saying 80 of those 300 belong to buildings nothing is drawing.
    #[must_use]
    pub fn warm_stats(&self) -> (u32, usize) {
        (self.warm_restored, self.retained.bytes())
    }

    /// Doors drawn across the loaded cells. See [`DoorCount`].
    pub fn doors(&self) -> usize {
        self.doors.values().sum()
    }

    /// Every material this lane draws with — the shells' and the imported models'.
    ///
    /// Both, and that matters: `building_select` writes the ghosted building's
    /// outline into all of them, and a LoD3 model standing in for a shell is drawn
    /// with the second one. Ghosting only the shells would leave an imported
    /// building opaque around an avatar standing in it, which is the one case
    /// where somebody has gone to the trouble of modelling the inside.
    pub fn material_handles(&self) -> Vec<Handle<BuildingMaterial>> {
        self.material
            .iter()
            .chain(self.interior_material.iter())
            .cloned()
            .collect()
    }

    /// How tall a building's shell is, metres above its own base — the datum
    /// `systems::interiors` divides into storeys. `None` for a building whose
    /// LoD2.2 cell is not loaded, which every caller must read as "one storey",
    /// because that is the answer this layer gave before it could ask.
    ///
    /// Linear in loaded cells (a handful) and a hash lookup in each; called once
    /// per building when the interior set is rebuilt, not per frame.
    pub fn shell_height(&self, id: &str) -> Option<f32> {
        self.shell_measure(id).map(|m| m.datum_m)
    }

    /// Everything this lane measured about a building's shell — the storey datum,
    /// the roof area and the volume. `None` for a building whose LoD2.2 cell is
    /// not loaded.
    ///
    /// Same cost as [`Self::shell_height`], which is a thin wrapper over it:
    /// linear in loaded cells (a handful) and a hash lookup in each.
    pub fn shell_measure(&self, id: &str) -> Option<ShellMeasure> {
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
            built_rev: HashMap::new(),
            built_digest: HashMap::new(),
            dated: HashMap::new(),
            doors: HashMap::new(),
            built_detail: HashMap::new(),
            heights: HashMap::new(),
            no_shadow: HashSet::new(),
            material: None,
            interior_material: None,
            enabled_was: true,
            terrain_revision: 0,
            bag_revision: (0, 0, 0),
            partial: None,
            live: HashMap::new(),
            retained: crate::platform::warm::WarmStore::new(crate::quality::warm_gpu_ceiling()),
            warm_restored: 0,
            design_rev: HashMap::new(),
            design_manifest_seen: 0,
        }
    }
}

/// The lane's two shared materials, created on first use.
///
/// Lifted out of the apply loop when the restore path grew a second caller: a
/// cell re-spawned from the retained pool has to be drawn with the *same* two
/// handles as a freshly built one, or the window shader's single uniform write
/// (`building_windows`) lights one set of buildings and not the other.
fn cell_materials(
    lod: &mut Lod22Buildings,
    materials: &mut Assets<BuildingMaterial>,
) -> (Handle<BuildingMaterial>, Handle<BuildingMaterial>) {
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
                    // The one difference from the shells' material, and the whole
                    // reason there are two: you can be inside one of these, where
                    // the wall in front of you shows its back.
                    double_sided: true,
                    cull_mode: None,
                    ..default()
                },
                extension: BuildingWindows::default(),
            })
        })
        .clone();
    (material, interior_material)
}

/// A building cell's box in the anchored world-XZ frame.
fn cell_box(clat: f64, clng: f64, zoom: u32, c: (i64, i64)) -> (Vec2, Vec2) {
    let anchor = (clat, clng, zoom);
    let (north, west) = tile_to_lat_lng(c.0, c.1, LOD22_ZOOM);
    let (south, east) = tile_to_lat_lng(c.0 + 1, c.1 + 1, LOD22_ZOOM);
    let (x0, z0) = crate::systems::geo_nav::lat_lng_to_world(anchor, north, west);
    let (x1, z1) = crate::systems::geo_nav::lat_lng_to_world(anchor, south, east);
    (
        Vec2::new(x0.min(x1) as f32, z0.min(z1) as f32),
        Vec2::new(x0.max(x1) as f32, z0.max(z1) as f32),
    )
}

/// Retire one live cell so that it is built again: its detail was promoted, or
/// the ground under it changed.
fn rebuild_cell(
    lod: &mut Lod22Buildings,
    colliders: &mut crate::systems::collision::BuildingColliders,
    c: (i64, i64),
) {
    lod.dated.remove(&c);
    lod.doors.remove(&c);
    lod.design_rev.remove(&c);
    lod.built_detail.remove(&c);
    lod.heights.remove(&c);
    lod.built_rev.remove(&c);
    lod.built_digest.remove(&c);
    // Dropped, not retained: a rebuild exists *because* this mesh is the wrong
    // one, so keeping it would only give the restore path something to hand back
    // that would be rebuilt again.
    lod.live.remove(&c);
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
    // `forget` is what drops it from the stream's `requested` set and hands its
    // slots back. The re-fetch is a disk-cache read.
    lod.stream.forget(&c);
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
    tile_url: Res<crate::resources::OsmTileUrlTemplate>,
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
    crate::systems::building_designs::poll(now);
    crate::systems::roof_colours::prefetch_coverage(now);
    // The additive manifests are folded in the same way and for the same reason:
    // the BAG and the roofs decide a building's colours, lod3 decides whether it
    // is drawn from a shell or from a model, and a cell built before one landed
    // has none of what it says.
    let bag_revision = (
        crate::systems::building_facts::coverage_revision(),
        crate::systems::lod3::coverage_revision(),
        crate::systems::roof_colours::coverage_revision(),
    );
    let reanchored = lod.stream.sync(stream.anchor_generation);
    // **A new field alone rebuilds only the cells built on an older one**, as the
    // ground cells do (`MapSurfaces::built_rev`). Over graded ground the cells were
    // waiting for this very field (`TerrainField::ground_later`), and tearing the
    // ring down threw them away and built every one twice.
    //
    // And only those whose ground the new field changed, as the ground cells do
    // (`TerrainField::cell_digest`): a field that follows the camera is a new
    // revision over ground most of the ring already stands on.
    if !reanchored && lod.bag_revision == bag_revision && lod.terrain_revision != terrain.revision {
        let stale: Vec<(i64, i64)> = lod
            .built_rev
            .iter()
            .filter(|(_, rev)| **rev != terrain.revision)
            .map(|(c, _)| *c)
            .collect();
        let mut old = Vec::new();
        let field: &crate::systems::terrain::TerrainField = &terrain;
        let boxes: Vec<_> = stale
            .iter()
            .map(|&c| {
                let (lo, hi) = cell_box(clat, clng, base_zoom, c);
                (c, lo, hi)
            })
            .collect();
        let digests = crate::systems::terrain::cell_digests(
            field,
            boxes.iter().map(|&(c, lo, hi)| ((c, lo), lo, hi)),
            false,
        );
        for ((c, lo), now) in digests {
            if lod.built_digest.get(&c) == Some(&now) {
                lod.built_rev.insert(c, terrain.revision);
            } else {
                tracing::debug!(
                    cell = ?c,
                    had = ?lod.built_digest.get(&c),
                    now,
                    lo_x = lo.x,
                    lo_z = lo.y,
                    "lod22 cell ground changed"
                );
                old.push(c);
            }
        }
        tracing::info!(
            cells = old.len(),
            of = lod.cells.len(),
            terrain_rev = terrain.revision,
            was_terrain_rev = lod.terrain_revision,
            "lod22 cells retear"
        );
        lod.terrain_revision = terrain.revision;
        for c in old {
            rebuild_cell(&mut lod, &mut colliders, c);
        }
        // The held cells stand on the old ground, and nothing records which.
        lod.retained.clear();
    }
    // **A building design that changed rebuilds the cells around it, and only
    // those** — an owner moving a door is a door in one building, and the ring
    // retear below is for causes that move every building at once. Asked only
    // when the published manifest moves, which is rare.
    let design_manifest = crate::systems::building_designs::manifest_revision();
    if !reanchored && lod.design_manifest_seen != design_manifest {
        lod.design_manifest_seen = design_manifest;
        let stale: Vec<(i64, i64)> = lod
            .design_rev
            .iter()
            .filter(|(c, rev)| {
                crate::systems::building_designs::neighbourhood_revision(c.0, c.1) != **rev
            })
            .map(|(c, _)| *c)
            .collect();
        if !stale.is_empty() {
            tracing::info!(
                cells = stale.len(),
                "lod22 cells rebuilt for a building design"
            );
            // Held cells were built under the old designs and nothing records
            // which; dropping them costs a re-mesh on return, never a wrong door.
            lod.retained.clear();
            for c in stale {
                rebuild_cell(&mut lod, &mut colliders, c);
            }
        }
    }
    if reanchored || lod.terrain_revision != terrain.revision || lod.bag_revision != bag_revision {
        // The same line `map_geometry` logs, for the same reason: a ring rebuilt
        // once because the ground arrived and a ring rebuilt four times because
        // four facts arrived separately are indistinguishable from outside, and
        // this streamer's cells are the most expensive meshes the client builds.
        tracing::info!(
            cells = lod.cells.len(),
            reanchored,
            terrain_rev = terrain.revision,
            was_terrain_rev = lod.terrain_revision,
            bag_rev = ?bag_revision,
            was_bag_rev = ?lod.bag_revision,
            "lod22 ring retear"
        );
        lod.terrain_revision = terrain.revision;
        lod.bag_revision = bag_revision;
        lod.built_rev.clear();
        lod.built_digest.clear();
        lod.dated.clear();
        lod.doors.clear();
        lod.design_rev.clear();
        lod.built_detail.clear();
        lod.heights.clear();
        colliders.clear_lod22();
        lod.no_shadow.clear();
        {
            let lod = &mut *lod;
            lod.stream.restart(&mut lod.cells, &mut lod.anchor);
        }
        lod.partial = None;
        // Held cells go with the live ones, and for a stronger reason than
        // tidiness: each carries a `GridCell` that means a place under the *old*
        // anchor, and every one of the four causes here — a re-anchor, a new
        // terrain field, the BAG coverage answer, the LoD3 manifest — changes
        // either where a building stands or what colour it is.
        lod.live.clear();
        lod.retained.clear();
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
        lod.built_rev.clear();
        lod.built_digest.clear();
        lod.dated.clear();
        lod.doors.clear();
        lod.built_detail.clear();
        lod.heights.clear();
        lod.stream.retire_all(retired);
        colliders.clear_lod22();
        lod.no_shadow.clear();
        lod.stream.reset();
        lod.partial = None;
        // Unticking the layer is the user saying they do not want these
        // buildings; holding a ring of them in graphics memory against a change
        // of mind is exactly the trade `map_geometry`'s detail toggle refuses
        // for the same reason.
        lod.live.clear();
        lod.retained.clear();
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
    let Ok(root) = bigspace.single() else {
        return;
    };

    // Camera's CACHE_ZOOM cell from its base-zoom tile coords.
    let (ccx, ccy) = cell_ring::coarsen(
        cell_ring::camera_tile((ax, ay), tile_m, cell, tf),
        z,
        LOD22_ZOOM,
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
        rebuild_cell(&mut lod, &mut colliders, c);
    }

    // The controller has asked for room, and this pool is the cheapest place to
    // find it — nothing in it is on screen. Complete rather than partial: see the
    // field's own note. Dropping the handles is what frees the graphics memory;
    // `upload_meter` sees the `Removed` events next frame and the residency it
    // reports falls, which is what `load_control` is waiting for.
    if load_control::take_shed() && !lod.retained.is_empty() {
        tracing::info!(
            cells = lod.retained.len(),
            freed_mb = lod.retained.bytes() / (1024 * 1024),
            "dropped the retained building cells to make room"
        );
        lod.retained.clear();
    }

    // Queue missing covered cells, nearest first. `CellStream::request` is also the
    // retry pump — it refuses a cell that has spent its attempts. Not
    // `cell_ring::request_ring`: the list is also where the restore below looks
    // for the nearest held cell, so it is built first and requested after.
    //
    // Nearest first is what put the nearest cell first when the queue was arrival
    // ordered; the rank says the same thing to the queue and to the global
    // `fetch_gate`, and keeps saying it when the camera moves under a queue that
    // was filled from somewhere else.
    let to_request = cell_ring::ring((ccx, ccy), lod22_radius(), |c, d| {
        lod22_covers(c.0, c.1) && d <= lod22_ring(c.0, c.1)
    });
    use crate::platform::fetch_gate::ring_distance;

    // Re-spawn one held cell, nearest first, before anything is queued for it.
    //
    // **One per frame, matching [`MAX_BUILDS_PER_FRAME`].** A restore uploads
    // nothing — that is the whole point of it — so `stream_ledger` has nothing to
    // admit and the frame's byte budget is not what bounds this. What is still
    // real is the command buffer: re-entering a ring can bring five cells at
    // once, and spawning five cells' worth of entities in one frame is the
    // *despawn* burst's twin. A cap that matches the build cap keeps the two
    // paths costing the same.
    let restore = to_request
        .iter()
        .copied()
        .find(|c| !lod.stream.is_requested(c) && lod.retained.holds(c));
    if let Some(c) = restore.filter(|_| lod.partial.is_none()) {
        if let Some(warm) = lod.retained.take(&c) {
            let (material, interior_material) = cell_materials(&mut lod, &mut materials);
            let mut spawned = Vec::with_capacity(warm.meshes.pieces.len());
            for (mesh, inside) in &warm.meshes.pieces {
                let id = commands
                    .spawn((
                        Mesh3d(mesh.clone()),
                        MeshMaterial3d(if *inside {
                            interior_material.clone()
                        } else {
                            material.clone()
                        }),
                        Transform::from_translation(Vec3::new(0.0, 0.05, 0.0)),
                        warm.meshes.grid_cell,
                        ChildOf(root),
                        Lod22Mesh,
                        HideInSpace,
                    ))
                    .id();
                spawned.push(id);
            }
            // The shadow-LOD pass at the foot of this system acts on boundary
            // crossings against `no_shadow`, which this cell is not in — so it
            // will decide for these entities this frame, and there is nothing to
            // pre-set here.
            lod.cells.insert(c, spawned);
            lod.dated.insert(c, warm.dated);
            lod.doors.insert(c, warm.doors);
            // The pool is emptied whenever a design changes, so what it holds
            // was built under the stamps standing now.
            lod.design_rev.insert(
                c,
                crate::systems::building_designs::neighbourhood_revision(c.0, c.1),
            );
            lod.heights.insert(c, warm.heights);
            lod.built_detail.insert(c, warm.detail);
            lod.live.insert(c, warm.meshes);
            // The pool is emptied whenever the field changes, so what it holds
            // stands on the current one.
            lod.built_rev.insert(c, terrain.revision);
            let (lo, hi) = cell_box(clat, clng, base_zoom, c);
            lod.built_digest
                .insert(c, terrain.cell_digest(lo, hi, false));
            colliders.set_cell(c, warm.footprints);
            // The far tier may have taken this cell over while we were away, the
            // same case the freshly-built path handles once its last piece is up.
            osm.evict_far_cell(c);
            // Without this the cell is re-requested on the very next line and
            // rebuilt over the meshes just re-spawned.
            lod.stream.adopt(c);
            lod.warm_restored += 1;
            tracing::info!(
                cell = ?c,
                pieces = lod.cells.get(&c).map_or(0, Vec::len),
                held_mb = lod.retained.bytes() / (1024 * 1024),
                "lod22 cell restored from memory"
            );
        }
    }

    for c in to_request {
        let rank = ring_distance(c, ccx, ccy);
        lod.stream.request_ranked(c, rank, now);
    }

    // Dispatch queued cells up to the concurrency cap: each becomes a detached
    // fetch+mesh task (ADR-033 seam) pushing to the sink — no OS thread.
    let results = lod.stream.sink();
    // Graded ground on its way: one build per cell, on it (`TerrainField::holds_builds`).
    // The jobs fetch meanwhile; only the parse and mesh wait for the ground.
    let held = terrain.holds_builds(stream.anchor_generation);
    // The street tile, for the tunnels under a cell's buildings (see
    // `over_a_lid`). Only a vector source has streets to read.
    let street_template = tile_url.0.lock().map(|g| g.clone()).unwrap_or_default();
    let street_template = (cartopolis_core::tile_source_kind(&street_template)
        == cartopolis_core::TileSourceKind::Vector)
        .then_some(street_template);
    while let Some((cx, cy)) = lod.stream.next_job(now) {
        let street_template = street_template.clone();
        let results = results.clone();
        // Resolved at dispatch, not inside the worker: the worker has no camera,
        // and recording what a cell was *built* with is what makes the promotion
        // sweep above possible at all.
        let detail = door_detail(ring_distance((cx, cy), ccx, ccy));
        let urgency = crate::platform::workers::urgency_for_ring(ring_distance((cx, cy), ccx, ccy));
        lod.built_detail.insert((cx, cy), detail);
        lod.design_rev.insert(
            (cx, cy),
            crate::systems::building_designs::neighbourhood_revision(cx, cy),
        );
        // The terrain travels with the job: a building's base is one height for the
        // whole footprint, and the footprints only exist inside the worker.
        let mut ground_now = (!held).then(|| (terrain.snapshot(), terrain.revision));
        let ground_later = held.then(|| terrain.ground_later());
        let live = lod.stream.live();
        // Two halves on two executors, matching the tile path (ADR-033): the fetch
        // is awaited here — free on wasm, where this is the main thread but the
        // browser does the transfer — while the parse and mesh, which are pure
        // arithmetic and cost a core, go to `platform::workers`. Running them here
        // is what put `serde_json` at ~46% of the main thread's non-idle time.
        IoTaskPool::get()
            .spawn(async move {
                // **Each input is its own task, so the downloads overlap** — the
                // reason `map_geometry::spawn_geometry_task` gives: natively a fetch
                // is a blocking call with no await point, so awaiting these in turn
                // ran four requests end to end per cell, one round trip after
                // another before the mesh could start.
                let io = IoTaskPool::get();
                // The cell's BAG facts travel with the mesh job rather than
                // arriving through the `BuildingFacts` resource on their own
                // schedule: a year and a use are baked into vertex colours, so
                // they have to be known on the worker that writes them. It is a
                // second request that hits the same storage cache as the facts
                // streamer's, so at worst it is a cache read; at best it is the
                // fetch the streamer then reads. Uncovered cells return an empty
                // set without asking anything.
                let facts_task = io.spawn(crate::systems::building_facts::load_bag_cell(cx, cy));
                // …with any designed building's entrances taken from its design, so
                // the door drawn on the facade, the opening in the collider and the
                // doorway in the room are the door its owner drew. The designs
                // travel on too: a designed building's walls are drawn from its
                // design, with the windows and doors its owner drew
                // (`lod22_facade`).
                let designs_task = io.spawn(crate::systems::building_designs::load_around(cx, cy));
                // …the roof colours measured from the aerial photo, likewise baked.
                let roofs_task = io.spawn(crate::systems::roof_colours::load_roof_cell(cx, cy));
                // …and the imported models for the same cell, on the same terms.
                let lod3_task = io.spawn(crate::systems::lod3::load_lod3_text(cx, cy));
                let body = match fetch_lod22_body(cx, cy).await {
                    Ok(t) => t,
                    Err(e) => {
                        // Let the others finish rather than dropping them, which
                        // would cancel them: a BAG download that other readers are
                        // waiting on (`building_facts::fetch_bag_text`) must not
                        // be abandoned half-way.
                        facts_task.detach();
                        designs_task.detach();
                        roofs_task.detach();
                        lod3_task.detach();
                        if let Ok(mut g) = results.lock() {
                            g.push(((cx, cy), (Err(e), 0)));
                        }
                        return;
                    }
                };
                let designs = designs_task.await;
                let facts =
                    crate::systems::building_designs::with_design_doors(facts_task.await, &designs);
                let roofs = roofs_task.await;
                let lod3 = lod3_task.await;
                // The ground last, after every fetch.
                if let Some(rx) = ground_later {
                    let (placement, rev) = rx.await.unwrap_or((None, 0));
                    ground_now = Some((placement.map(|p| (*p).clone()).unwrap_or_default(), rev));
                }
                let (ground, built_rev) = ground_now.unwrap_or_default();
                // Torn down while it fetched (`CellStream::live`): no parse, no mesh.
                if !live.load(std::sync::atomic::Ordering::Relaxed) {
                    return;
                }
                // …and the street tile, shared with the surfaces streamer's parse
                // of the same z14 body, so a building standing in a tunnel's mouth
                // can be recognised on the worker that would draw it.
                let streets = match street_template {
                    Some(t) => {
                        let key = cartopolis_core::world::TileKey::new(cx, cy, LOD22_ZOOM);
                        crate::systems::tile_loader::load_tile_image(&key, &t)
                            .await
                            .ok()
                            .and_then(|b| crate::systems::tile_loader::parse_shared(key, &b).ok())
                    }
                    None => None,
                };
                crate::platform::workers::submit_heavy_ranked(
                    "lod22",
                    urgency,
                    Box::new(move || {
                        let built = build_lod22_from_body(
                            &body,
                            cx,
                            cy,
                            ax,
                            ay,
                            z,
                            lat,
                            &ground,
                            &facts,
                            &roofs,
                            lod3.as_deref(),
                            detail,
                            streets.as_deref(),
                            &designs,
                        );
                        if let Ok(mut g) = results.lock() {
                            g.push(((cx, cy), (built, built_rev)));
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
        for (c, (built, built_rev)) in done {
            // Built on a field that has since been replaced: ask again, on this one.
            if built.is_ok() && built_rev != terrain.revision {
                lod.stream.forget(&c);
                continue;
            }
            match built {
                Ok(Some((grid_cell, pieces, footprints, dated, doors, heights))) => {
                    lod.built_rev.insert(c, built_rev);
                    let (lo, hi) = cell_box(clat, clng, base_zoom, c);
                    lod.built_digest
                        .insert(c, terrain.cell_digest(lo, hi, false));
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
                    // the key simply isn't there. Both formats' keys: the error
                    // does not say which body was parsed, and dropping a good
                    // entry costs one refetch.
                    let keys = [cell_cache_key(c.0, c.1), bin_cache_key(c.0, c.1)];
                    IoTaskPool::get()
                        .spawn(async move {
                            for key in keys {
                                storage::remove(storage::Namespace::Cache, &key).await;
                            }
                        })
                        .detach();
                    lod.stream.failed(&c, now); // allows a bounded re-queue later
                }
            }
        }
    }

    // Drain the cell in application: spawn the pieces the frame's ledger admits,
    // park the rest for the next frame.
    if let Some(mut partial) = lod.partial.take() {
        let (material, interior_material) = cell_materials(&mut lod, &mut materials);
        while let Some((mesh, _)) = partial.pieces.first() {
            let cost = crate::systems::stream_ledger::mesh_cost(mesh);
            if !crate::systems::stream_ledger::admit(cost) {
                break;
            }
            let (mesh, inside) = partial.pieces.remove(0);
            // One entity per piece, flat under the same root rather than a parent
            // with children: every consumer — the shadow-LOD toggle,
            // `HideInSpace`, the despawn queue — acts per mesh entity, and a
            // parent would mean each of them learning to walk children for a
            // hierarchy with nothing else to do.
            let mesh_handle = meshes.add(mesh);
            let mut entity = commands.spawn((
                Mesh3d(mesh_handle.clone()),
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
            // Keep the handle and what it costs, so retiring this cell can hold
            // the mesh rather than free it. `mesh_cost` is the same figure
            // `stream_ledger` admitted it on and the same one `upload_meter`
            // counts as resident, which is what lets the pool be bounded in the
            // units the residency target is written in.
            let live = lod.live.entry(partial.cell).or_insert(LiveMeshes {
                grid_cell: partial.grid_cell,
                pieces: Vec::new(),
                bytes: 0,
            });
            live.bytes += cost;
            live.pieces.push((mesh_handle, inside));
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
        .filter(|(x, y)| (x - ccx).abs().max((y - ccy).abs()) > lod22_ring(*x, *y) + 1)
        .copied()
        .collect();
    for c in stale {
        let dated = lod.dated.remove(&c);
        let doors = lod.doors.remove(&c);
        let detail = lod.built_detail.remove(&c);
        let heights = lod.heights.remove(&c);
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
        let footprints = colliders.remove_cell(c);
        lod.no_shadow.remove(&c);
        lod.stream.forget(&c);
        // Hold the whole cell if there is room for it. The entities are gone
        // either way; what this decides is whether coming back is a re-spawn or
        // another registry fetch, serde parse, merge and upload.
        //
        // **Only a cell that is complete and accounted for.** A cell dropped
        // mid-application above has no entry in `live` yet, and any of the other
        // five being absent means a state this pool cannot faithfully restore —
        // in which case the `remove` in the scrutinee has already dropped the
        // handles and the return is an ordinary rebuild.
        if let (
            Some(meshes),
            Some(footprints),
            Some(dated),
            Some(doors),
            Some(heights),
            Some(detail),
        ) = (
            lod.live.remove(&c),
            footprints,
            dated,
            doors,
            heights,
            detail,
        ) {
            lod.retained.put(
                c,
                WarmCell {
                    meshes,
                    footprints,
                    dated,
                    doors,
                    heights,
                    detail,
                },
            );
        }
    }

    // Shadow LOD: cells within SHADOW_RADIUS cast shadows, the rest don't. Only act
    // when a cell crosses the boundary (tracked in `no_shadow`), so a stationary
    // camera issues no commands. This is a mesh-level toggle; the forbidden runtime
    // change is the directional lights' `num_cascades` — see CLAUDE.md.
    // Only the cells that *cross* the boundary are collected, and only their keys:
    // `lod.cells` is read and `lod.no_shadow` written in the same pass, so
    // something has to be copied out to end the borrow — but a stationary camera
    // crosses nothing, so in the steady state this allocates an empty `Vec` rather
    // than cloning every live cell's entity list every frame.
    let crossing: Vec<((i64, i64), bool)> = lod
        .cells
        .keys()
        .map(|&(cx, cy)| {
            let near = (cx - ccx).abs().max((cy - ccy).abs()) <= SHADOW_RADIUS;
            ((cx, cy), near)
        })
        .filter(|(cell, near)| *near == lod.no_shadow.contains(cell))
        .collect();
    for (cell, near) in crossing {
        // The whole cell crosses the boundary at once — it is one cell's worth of
        // geometry however many pieces it was cut into — so the `no_shadow` set
        // stays keyed by cell and the commands fan out over its entities.
        let entities = lod.cells.get(&cell).cloned().unwrap_or_default();
        if near {
            for e in entities {
                commands.entity(e).remove::<bevy::light::NotShadowCaster>();
            }
            lod.no_shadow.remove(&cell);
        } else {
            for e in entities {
                commands.entity(e).insert(bevy::light::NotShadowCaster);
            }
            lod.no_shadow.insert(cell);
        }
    }
}

/// [`cartopolis_geo::tunnels::stands_in_a_covered_trench`] for a building of
/// this cell, from the cell's own street tile, whose frame is the cell's
/// north-west corner.
fn over_a_lid(
    t: &cartopolis_geo::tunnels::TunnelTessellation,
    world_xz: &[Vec2],
    cell_nw: Vec2,
    ground: &crate::systems::terrain::TerrainSnapshot,
) -> bool {
    if t.tunnels.is_empty() || world_xz.is_empty() {
        return false;
    }
    let ring: Vec<[f32; 2]> = world_xz
        .iter()
        .map(|p| [p.x - cell_nw.x, p.y - cell_nw.y])
        .collect();
    cartopolis_geo::tunnels::footprint_in_a_covered_trench(&t.tunnels, &t.openings, &ring, &|p| {
        let (x, z) = (p[0] + cell_nw.x, p[1] + cell_nw.y);
        ground.surrounding(x, z, cartopolis_geo::tunnels::DATUM_M) - ground.at(x, z)
    })
}

/// Build one fetched cell's merged LoD2.2 mesh, anchored to the cell's own
/// BigSpace grid cell. Runs on the worker thread. `None` if empty.
/// The CPU half: a cell body (binary or JSON) in, finished mesh out. Pure — no
/// I/O, no globals, and every input owned — which is what lets [`update_lod22`]
/// hand it to [`platform::workers`] instead of running it on the thread that
/// renders.
///
/// That split is the whole point of the function existing separately. Profiling
/// the web build put `serde_json` at ~46% of the main thread's non-idle time,
/// with `drop_glue::<Value>` the single largest entry, because this ran inside an
/// `IoTaskPool` future — and on wasm `IoTaskPool` is the main thread, Bevy having
/// no `multi_threaded` there. Awaiting a fetch on it costs nothing; parsing a
/// megabyte of CityJSON and meshing it costs a frame.
///
/// [`platform::workers`]: crate::platform::workers
fn build_lod22_from_body(
    body: &Lod22Body,
    cx: i64,
    cy: i64,
    ax: i64,
    ay: i64,
    z: u32,
    lat: f64,
    ground: &crate::systems::terrain::TerrainSnapshot,
    facts: &crate::systems::building_facts::BagCell,
    roofs: &cartopolis_geo::formats::roofs::Cell,
    lod3_text: Option<&str>,
    detail: load_control::Detail,
    streets: Option<&cartopolis_geo::vector_tiles::TileData>,
    designs: &[std::sync::Arc<cartopolis_geo::building_design::BuildingDesign>],
) -> Result<Option<Lod22Build>, String> {
    let shell = body.parse()?;
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
    // The cell's tunnels, in the cell's own tile-local metres, when it has any.
    let cell_m = tile_to_meters(LOD22_ZOOM, lat) as f32;
    let cell_nw = lat_lng_to_local(north, west, ax, ay, z, tile_m);
    let tunnels = streets.map(|t| cartopolis_geo::tunnels::build_tunnels(t, cell_m, None));
    let mut in_a_mouth = 0usize;

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
    let mut buf = MeshData::with_dedup().with_uvs();
    // Imported models mesh into their own buffer: they are drawn with culling off
    // (see [`Piece`]) and so cannot share a mesh with the shells.
    // UV0 carries `building_windows::WINDOWLESS_UV` on every vertex: a model's
    // walls get no painted windows.
    let mut interior = MeshData::with_dedup().with_uvs();
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
    let mut heights: ShellStats = Vec::new();
    // The buildings drawn from a design (`lod22_facade`): their surveyed walls
    // are left out and drawn again with the design's openings, and the survey's
    // wall vertices are what the redrawn walls take their height from.
    let designed: HashMap<&str, &cartopolis_geo::building_design::BuildingDesign> =
        designs.iter().map(|d| (d.pand.as_str(), &**d)).collect();
    let mut shell_walls: HashMap<&str, Vec<Vec3>> = HashMap::new();
    if !designed.is_empty() {
        for b in elements
            .iter()
            .filter(|b| designed.contains_key(b.id.as_str()))
        {
            let v = shell_walls.entry(b.id.as_str()).or_default();
            for &(i, j, k, is_roof) in &b.t {
                if is_roof == 1 {
                    continue;
                }
                for ix in [i, j, k] {
                    if let Some(&(la, lo, h)) = b.p.get(ix) {
                        let w = lat_lng_to_local(la, lo, ax, ay, z, tile_m);
                        v.push(Vec3::new(w.x, h, w.y));
                    }
                }
            }
        }
    }
    let mut facades_drawn: HashSet<&str> = HashSet::new();
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
        let design = designed.get(b.id.as_str()).copied();
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
        // …and beside it, packed into the same float, what the facade shader
        // draws the walls from: the era, a shop on the ground floor, and the
        // eaves — the 25th percentile of the roof's vertex heights, so a low
        // annex does not pull a terrace's eaves down to its own.
        let eave_m = {
            let mut h: Vec<f32> =
                b.t.iter()
                    .filter(|t| t.3 == 1)
                    .flat_map(|t| [t.0, t.1, t.2])
                    .filter_map(|i| b.p.get(i as usize).map(|p| p.2))
                    .collect();
            h.sort_by(f32::total_cmp);
            h.get(h.len() / 4).copied().filter(|&e| e > 2.0)
        };
        let lit = crate::systems::building_windows::pack_facade(
            crate::systems::building_windows::Facade {
                occupancy: crate::systems::osm_buildings::night_lit_scale(
                    fact.map_or(0, |f| f.uses),
                    fact.map_or(0, |f| f.units),
                ),
                shop: fact
                    .is_some_and(|f| f.uses & crate::systems::building_facts::uses::SHOP != 0),
                era: crate::systems::building_windows::era(fact.and_then(|f| f.year)),
                eave_m,
            },
        );
        let wall = [wall[0], wall[1], wall[2], lit];
        let roof_tile = [roof_tile[0], roof_tile[1], roof_tile[2], lit];
        // `FLAT_ROOF_COLOR` is authored in sRGB; vertex colours multiply in
        // linear space (same exit rule as `building_palette`).
        let flat_lin = crate::systems::scatter::srgb_to_linear_f32(FLAT_ROOF_COLOR);
        let flat = [flat_lin[0], flat_lin[1], flat_lin[2], lit];
        // The roof's measured colours, where atlas has photographed it: its flat
        // planes and its pitched ones separately, each falling back to the
        // generated tone when that half could not be measured.
        let measured = (!b.id.is_empty()).then(|| roofs.roofs.get(&b.id)).flatten();
        let flat = measured.and_then(|r| r.f.as_ref()).map_or(flat, |s| {
            crate::systems::roof_colours::vertex_colour(s, lit)
        });
        let roof_tile = measured.and_then(|r| r.p.as_ref()).map_or(roof_tile, |s| {
            crate::systems::roof_colours::vertex_colour(s, lit)
        });

        // World-XZ position of each vertex (before the cell offset).
        let world_xz: Vec<Vec2> =
            b.p.iter()
                .map(|&(la, lo, _)| lat_lng_to_local(la, lo, ax, ay, z, tile_m))
                .collect();
        if let Some(t) = &tunnels {
            if over_a_lid(t, &world_xz, cell_nw, ground) {
                in_a_mouth += 1;
                continue;
            }
        }
        // Only a shell is worth measuring. An imported model is many elements and
        // brings its own floors (`systems::lod3`), so measuring one would report
        // the height of a wall panel as the height of a building — and its volume
        // as the volume of a slab.
        if b.class.is_empty() && !b.id.is_empty() {
            if let Some(m) = shell_stats(&world_xz, &b.p, &b.t) {
                heights.push((b.id.clone(), m));
            }
        }
        // The register's own outline where we have it, and the convex hull of the
        // mesh where we do not.
        //
        // The hull is what a triangle soup can give and it is wrong for every
        // building that is not convex: an L-shaped block's hull fills in the inner
        // corner, so the yard between its wings is a place the avatar cannot walk
        // and nothing is drawn. Now that atlas's `pipeline/bag.rs` ships the ring, that
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
        // The walls as flat faces, each entrance placed on one of them
        // (`building_windows::WallFaces`). Done before the collider below so
        // the opening it leaves is where the door is drawn — within the snap
        // of `DOOR_STEP_M` and the clearance from the face's corners, of the
        // point the register gives.
        let era = crate::systems::building_windows::era(fact.and_then(|f| f.year));
        let shell_pts: Vec<Vec3> =
            b.p.iter()
                .zip(&world_xz)
                .map(|(&(_, _, h), &w)| Vec3::new(w.x, h, w.y))
                .collect();
        let mut faces = crate::systems::building_windows::WallFaces::new(&shell_pts, &b.t, era);
        let placed: Vec<(Option<usize>, Vec2)> = doors
            .iter()
            .filter_map(|&d| {
                use crate::systems::building_windows::DoorPlacement;
                match faces.place_door(d) {
                    DoorPlacement::On(i, at) => Some((Some(i), at)),
                    DoorPlacement::Merged => None,
                    DoorPlacement::Nowhere => Some((None, d)),
                }
            })
            .collect();
        let doors: Vec<Vec2> = placed.iter().map(|p| p.1).collect();
        // Hollow if there is a door *and* a room behind it, which is one question
        // asked in one place: `interiors` builds the room from this same outline,
        // so a building the avatar can walk into is a building with a floor,
        // walls and a ceiling waiting for it. Everything else stays a solid mass
        // — a shell is back-face culled, so walking into one is a hole in the
        // world with the street showing through it.
        let hollow = !doors.is_empty() && crate::systems::interiors::can_hold_a_room(&outline);
        let pending_fp =
            crate::systems::collision::Footprint::with_doors(outline.clone(), &doors, hollow);

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
        // A designed building's doors are openings in its redrawn walls, not
        // panels on the surveyed ones.
        if design.is_none()
            && detail == load_control::Detail::Near
            && !doors.is_empty()
            && outline.len() >= 3
        {
            let base = ground.level_over(outline.iter().map(|w| [w.x, w.y]));
            // Only the entrances no wall face could take; the rest are built
            // as doorways further down, once the building's base is known.
            for door in placed.iter().filter(|p| p.0.is_none()).map(|p| p.1) {
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
        // The building's own height above its base, for the ground-contact
        // shading below. Taken from the shell's raw vertex heights rather than
        // from `shell_datum_height`: that answers the 70th percentile of the
        // *roof*, which is the right datum for counting storeys and the wrong one
        // for "where does this wall stop", and it is only computed for shells
        // carrying an id.
        let wall_h = b.p.iter().map(|&(_, _, h)| h).fold(0.0f32, f32::max);
        // The collider spans the building's own height, so a bridge deck or a
        // flier above the ridge is not stopped by it.
        if let Some(fp) = pending_fp {
            footprints.push(fp.with_extent(base - 3.0, base + wall_h.max(3.0)));
        }
        if let Some(d) = design {
            // Once per building, whichever of its parts gets here first.
            if facades_drawn.insert(b.id.as_str()) && outline.len() >= 3 {
                let openings = crate::systems::lod22_facade::openings_of(d, |la, lo| {
                    lat_lng_to_local(la, lo, ax, ay, z, tile_m)
                });
                crate::systems::lod22_facade::draw(
                    &mut buf,
                    &outline,
                    base,
                    shell_walls
                        .get(b.id.as_str())
                        .map_or(&[][..], Vec::as_slice),
                    wall_h,
                    &openings,
                    wall,
                    Vec2::new(ox, oz),
                );
            }
        }
        if design.is_none() && detail == load_control::Detail::Near {
            for (face, _) in placed.iter().filter_map(|p| p.0.map(|i| (i, p.1))) {
                let face = &faces.faces[face];
                let Some(at) = face.door_at() else { continue };
                push_doorway(
                    &mut buf,
                    face,
                    at,
                    base,
                    Vec2::new(ox, oz),
                    era,
                    fact.map_or(0, |f| f.uses),
                );
                drawn_doors += 1;
            }
        }
        for (ti, &(i, j, k, is_roof)) in b.t.iter().enumerate() {
            let (a, bb, c) = (local[i], local[j], local[k]);
            if is_roof == 1 {
                let n = (bb - a).cross(c - a).normalize_or_zero();
                let color = if n.y.abs() > FLAT_ROOF_NORMAL_Y {
                    flat
                } else {
                    roof_tile
                };
                buf.tri(a, bb, c, color);
                continue;
            }
            // Redrawn from the design, above.
            if design.is_some() {
                continue;
            }
            // Ground-contact darkening, baked into the wall vertices on the
            // worker that builds the cell — so it costs nothing at all in a
            // frame, and it is there on the rungs and the adapters where the
            // screen-space occlusion pass is not (a handset, WebGL2, anyone who
            // has turned it off). Until this, `lod22` — the lane that draws the
            // actual city — gave every building one flat tone from pavement to
            // eaves, while the OSM fallback lane beside it had carried
            // `wall_base_tone` for as long as it had existed.
            //
            // It is not a substitute for real occlusion and does not try to be:
            // it knows about the ground and about nothing else, so it says
            // nothing about the alley between two blocks. It is the one cue that
            // stops an extrusion looking like it is hovering, which is the thing
            // you see first.
            let tone = |v: Vec3| {
                let d = crate::systems::osm_buildings::wall_shade(wall_h, v.y - base);
                [wall[0] * d, wall[1] * d, wall[2] * d, wall[3]]
            };
            match faces.of_triangle.get(ti).copied().flatten() {
                Some(fi) => {
                    let face = &faces.faces[fi as usize];
                    buf.tri_graded_uv(
                        [a, bb, c],
                        [tone(a), tone(bb), tone(c)],
                        [
                            face.uv(world_xz[i]),
                            face.uv(world_xz[j]),
                            face.uv(world_xz[k]),
                        ],
                    );
                }
                None => buf.tri_graded(a, bb, c, [tone(a), tone(bb), tone(c)]),
            }
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
    if in_a_mouth > 0 {
        tracing::info!(
            cx,
            cy,
            in_a_mouth,
            "lod22: skipped buildings standing in a covered tunnel's trench"
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
            // A model part that says what colour it is gets that colour on every
            // face; the dated palette is for parts that do not say.
            let (wall, roof_tile) = match b.rgb {
                Some(c) => {
                    let lin = crate::systems::scatter::srgb_to_linear_f32([
                        f32::from(c[0]) / 255.0,
                        f32::from(c[1]) / 255.0,
                        f32::from(c[2]) / 255.0,
                    ]);
                    (lin, lin)
                }
                None => (wall, roof_tile),
            };
            let wall = [wall[0], wall[1], wall[2], lit];
            let roof_tile = [roof_tile[0], roof_tile[1], roof_tile[2], lit];

            let world_xz: Vec<Vec2> =
                b.p.iter()
                    .map(|&(la, lo, _)| lat_lng_to_local(la, lo, ax, ay, z, tile_m))
                    .collect();
            // Only the walls. A slab's footprint is the whole storey, so hulling
            // one would fill the building with an invisible block — which is
            // exactly the collider a shell has and a model exists to lose.
            let mut model_fp = None;
            if crate::systems::lod3::is_solid_class(&b.class) {
                model_fp = crate::systems::collision::Footprint::new(
                    crate::systems::collision::convex_hull_xz(world_xz.clone()),
                );
            }
            let base = ground.level_over(world_xz.iter().map(|w| [w.x, w.y]));
            if let Some(fp) = model_fp {
                let top = b.p.iter().map(|&(_, _, h)| h).fold(0.0f32, f32::max);
                footprints.push(fp.with_extent(base - 3.0, base + top.max(3.0)));
            }
            let local: Vec<Vec3> =
                b.p.iter()
                    .zip(&world_xz)
                    .map(|(&(_, _, h), &w)| Vec3::new(w.x - ox, h + base, w.y - oz))
                    .collect();
            for &(i, j, k, is_roof) in &b.t {
                let (a, bb, c) = (local[i], local[j], local[k]);
                let color = if is_roof == 1 { roof_tile } else { wall };
                interior.tri_graded_uv(
                    [a, bb, c],
                    [color; 3],
                    [crate::systems::building_windows::WINDOWLESS_UV; 3],
                );
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

/// What a building's shell measures: how tall it is, how much roof it has, and
/// how much building there is.
///
/// **All three are read off the geometry the client already has, and that is the
/// point.** 3DBAG publishes `b3_h_dak_70p`, `b3_opp_dak_plat + b3_opp_dak_schuin`
/// and `b3_volume_lod22` as attributes, so the obvious way to put a roof area and
/// a volume on the building card is to ship them from atlas's extractor — a
/// format change on both sides of the atlas/client seam, a pipeline re-run for every city and a cache-key
/// bump, all to carry numbers *derived from the very triangles already in this
/// buffer*. Measuring them here costs one pass over a soup this function was
/// walking anyway, needs no wire change, and works on any cell already cached.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct ShellMeasure {
    /// The storey datum, metres above the building's own base. See
    /// [`shell_datum_height`].
    pub datum_m: f32,
    /// Total roof surface area, m² — flat and pitched together, which is what
    /// 3DBAG's own two columns add up to. It is the *sloping* area, so a pitched
    /// roof measures more than the footprint under it.
    pub roof_area_m2: f32,
    /// Enclosed volume, m³.
    ///
    /// The divergence-theorem sum over the triangles, with the origin moved to the
    /// building's own first vertex at ground level. Both halves of that are
    /// load-bearing. The soup carries no floor — a LoD2.2 solid is the surface
    /// above its base — and a missing face contributes nothing to this sum exactly
    /// when its plane passes through the origin, which the base plane (y = 0) does.
    /// And the recentring is what keeps it computable at all: the vertices are
    /// metres from the *map anchor*, so at kilometre offsets the triple product of
    /// a 10 m building is the difference of two enormous numbers.
    pub volume_m3: f32,
    /// The highest vertex, metres above the building's own base — the ridge.
    ///
    /// Deliberately the number [`Self::datum_m`] refuses to be: the datum is the
    /// eaves, because storeys stack under a roof, and this is the top of the roof,
    /// because a highlight drawn to the eaves leaves a pitched roof sticking out
    /// of it unselected.
    pub top_m: f32,
}

/// Measure a shell: its storey datum, its roof area and its volume.
///
/// `xz` is each vertex in metres (the same local frame the mesh is built in) and
/// `p` carries the height on its third component; they are indexed alike.
fn shell_stats(
    xz: &[Vec2],
    p: &[(f64, f64, f32)],
    t: &[(usize, usize, usize, u8)],
) -> Option<ShellMeasure> {
    let datum_m = shell_datum_height(p, t)?;
    // Recentre on the building's own first vertex — see `ShellMeasure::volume_m3`.
    let o = *xz.first()?;
    let at = |ix: usize| -> Option<[f64; 3]> {
        let q = xz.get(ix)?;
        let h = p.get(ix)?.2;
        Some([f64::from(q.x - o.x), f64::from(h), f64::from(q.y - o.y)])
    };
    let mut roof = 0.0f64;
    let mut vol6 = 0.0f64;
    let mut top = 0.0f32;
    for &(_, _, h) in p {
        top = top.max(h);
    }
    for &(i, j, k, is_roof) in t {
        let (Some(a), Some(b), Some(c)) = (at(i), at(j), at(k)) else {
            continue;
        };
        let u = [b[0] - a[0], b[1] - a[1], b[2] - a[2]];
        let v = [c[0] - a[0], c[1] - a[1], c[2] - a[2]];
        let n = [
            u[1] * v[2] - u[2] * v[1],
            u[2] * v[0] - u[0] * v[2],
            u[0] * v[1] - u[1] * v[0],
        ];
        if is_roof == 1 {
            roof += 0.5 * (n[0] * n[0] + n[1] * n[1] + n[2] * n[2]).sqrt();
        }
        vol6 += a[0] * n[0] + a[1] * n[1] + a[2] * n[2];
    }
    Some(ShellMeasure {
        datum_m,
        roof_area_m2: roof as f32,
        // Absolute, because nothing promises which way a 3DBAG soup is wound and
        // a negative volume is the same building inside out.
        volume_m3: (vol6.abs() / 6.0) as f32,
        top_m: top.max(datum_m),
    })
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

/// Cache key for the same cell in the binary encoding
/// ([`cartopolis_geo::lod22_cell`]).
///
/// **A different key, not a different body under the same one.** The two formats
/// share nothing a parser could tell apart by accident, but a cached JSON body
/// handed to the binary decoder (or the reverse) would be a failed cell and an
/// eviction every time, so each format has its own namespace and a body is only
/// ever read back by the parser that wrote it. The `bin1` names the format's
/// version, which the decoder also checks in the header.
fn bin_cache_key(cx: i64, cy: i64) -> String {
    format!("buildings/lod22/bin1/{}/{}/{}.bin", LOD22_ZOOM, cx, cy)
}

/// One cell's body as fetched, in whichever format the host served.
enum Lod22Body {
    Json(String),
    Bin(Vec<u8>),
}

impl Lod22Body {
    /// Parse into the cell's elements. Both arms go through [`cell_file`], so
    /// everything downstream receives the same data whichever format arrived.
    fn parse(&self) -> Result<CellFile, String> {
        match self {
            Lod22Body::Json(text) => parse_lod22_json(text).map(cell_file),
            Lod22Body::Bin(bytes) => cartopolis_geo::lod22_cell::decode(bytes)
                .map(cell_file)
                .map_err(|e| e.to_string()),
        }
    }
}

/// Whether this host serves the binary cells: unknown, yes, or no — the same
/// latch `cell_bundle::SUPPORT` keeps, for the same reason. A host that has not
/// re-run the pipeline since the binary was added answers 404, and that must cost
/// a handful of requests per session rather than one per cell.
mod bin_support {
    pub const UNKNOWN: u8 = 0;
    pub const YES: u8 = 1;
    pub const NO: u8 = 2;
}

static BIN_SUPPORT: std::sync::atomic::AtomicU8 =
    std::sync::atomic::AtomicU8::new(bin_support::UNKNOWN);
static BIN_FAILURES: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);

/// Failed `.bin` requests before the host is written off as not serving them.
/// One wave of the streamer's concurrency: the opening ring asks for that many
/// cells at once, so latching on the first failure would decide on a coin toss
/// among requests already in flight.
const MAX_BIN_FAILURES: u32 = LOD22_CONCURRENCY as u32;

/// Count a failed binary fetch, and latch the binary off once enough have failed.
/// A host that has served one binary cell is never written off: a later failure
/// there is an outage or a missing cell, and the JSON fallback covers both.
fn note_bin_failure() {
    use std::sync::atomic::Ordering;
    if BIN_SUPPORT.load(Ordering::Relaxed) == bin_support::YES {
        return;
    }
    if BIN_FAILURES.fetch_add(1, Ordering::Relaxed) + 1 >= MAX_BIN_FAILURES {
        BIN_SUPPORT.store(bin_support::NO, Ordering::Relaxed);
        tracing::info!("this host serves no binary LoD2.2 cells; reading the JSON");
    }
}

/// Fetch one LoD2.2 cell from the host, cached through the storage seam. Runs on
/// an `IoTaskPool` task — no thread, no `ureq`, no `std::fs` (ADR-033), so it is
/// wasm-buildable.
/// The network half: cache lookup, fetch, cache write — and no parsing.
///
/// **The binary first, the JSON as the fallback.** `lod22/14/x/y.bin` is the
/// same cell in [`cartopolis_geo::lod22_cell`]'s encoding — about a quarter of
/// the JSON's bytes and no float text to parse. A host that has not been re-run
/// since atlas learned to write it answers 404, which falls through to the JSON
/// in the same call and counts towards [`BIN_SUPPORT`] latching off. A cached
/// JSON body is still read when there is no cached binary and the host has no
/// binary to give; it is the same data.
///
/// It used to parse a cache hit to validate it, discarding the entry if that
/// failed. That check has moved to the caller, which now knows a parse failed and
/// evicts the key then; doing it here would mean parsing on this thread, which is
/// exactly what [`build_lod22_from_body`] exists to avoid. The binary is checked
/// for its magic before it is stored, which costs four byte comparisons.
async fn fetch_lod22_body(cx: i64, cy: i64) -> Result<Lod22Body, String> {
    use std::sync::atomic::Ordering;
    let bin_key = bin_cache_key(cx, cy);
    if let Some(cached) = storage::get(storage::Namespace::Cache, &bin_key).await {
        return Ok(Lod22Body::Bin(cached));
    }
    if BIN_SUPPORT.load(Ordering::Relaxed) != bin_support::NO {
        let url = format!("{}/lod22/{}/{}/{}.bin", tdbag_url(), LOD22_ZOOM, cx, cy);
        match http::fetch_optional(http::Request::get(url)).await {
            Ok(Some(bytes)) if cartopolis_geo::lod22_cell::looks_like_cell(&bytes) => {
                BIN_SUPPORT.store(bin_support::YES, Ordering::Relaxed);
                storage::put(storage::Namespace::Cache, &bin_key, &bytes).await;
                return Ok(Lod22Body::Bin(bytes));
            }
            // A 404, a body that is not a cell (an HTML error page from a proxy),
            // or a transport failure: all of them are answered by the JSON below.
            _ => note_bin_failure(),
        }
    }
    let key = cell_cache_key(cx, cy);
    if let Some(cached) = storage::get_string(storage::Namespace::Cache, &key).await {
        return Ok(Lod22Body::Json(cached));
    }
    let url = format!("{}/lod22/{}/{}/{}.json", tdbag_url(), LOD22_ZOOM, cx, cy);
    let bytes = http::fetch(http::Request::get(url)).await?;
    let text = String::from_utf8(bytes).map_err(|e| e.to_string())?;
    storage::put(storage::Namespace::Cache, &key, text.as_bytes()).await;
    Ok(Lod22Body::Json(text))
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
///
/// Used for the imported-model (LoD3) files too, which are only ever JSON.
fn parse_lod22(text: &str) -> Result<CellFile, String> {
    parse_lod22_json(text).map(cell_file)
}

/// A parsed cell's elements, validated: triangles whose indices fall outside
/// their vertex list are dropped, and so is an element left with fewer than three
/// vertices or no triangles. The one place that happens, for the JSON and the
/// binary alike — which is what makes the two formats produce the same buildings.
fn cell_file(cell: cartopolis_geo::lod22_cell::Cell) -> CellFile {
    let mut out = Vec::with_capacity(cell.elements.len());
    for el in cell.elements {
        let p = el.p;
        let mut t = Vec::with_capacity(el.t.len());
        for (i, j, k, rf) in el.t {
            let (i, j, k) = (i as usize, j as usize, k as usize);
            if i < p.len() && j < p.len() && k < p.len() {
                t.push((i, j, k, rf));
            }
        }
        if p.len() >= 3 && !t.is_empty() {
            out.push(Bldg {
                id: el.id,
                p,
                t,
                class: el.class,
                rgb: el.rgb,
            });
        }
    }
    CellFile {
        elements: out,
        pands: cell.pands,
    }
}

/// The JSON cell file as the host wrote it, before [`cell_file`]'s validation —
/// the shape [`cartopolis_geo::lod22_cell`] encodes, so the two formats can be
/// compared element for element.
///
/// The wire types — the string-or-legacy-number id, the three-or-four-element
/// triangle row — live in `cartopolis_geo::formats::lod22`, which atlas writes
/// the file with.
fn parse_lod22_json(text: &str) -> Result<cartopolis_geo::lod22_cell::Cell, String> {
    cartopolis_geo::formats::lod22::parse(text)
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

    use cartopolis_geo::lod22_cell::{self, Grid};

    /// 34 real buildings from Groningen's densest cell (8490/5319), cut from the
    /// host's JSON on 2026-09-26: the first thirty, three split pands (`part`),
    /// and the cell's largest shell (1,026 vertices).
    fn sample_json() -> String {
        std::fs::read_to_string(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../geo/tests/fixtures/lod22_groningen_sample.json"
        ))
        .expect("the lod22 sample fixture")
    }

    const SAMPLE_CELL: (i64, i64) = (8490, 5319);

    /// Everything the pipeline carries, compared field by field: the binary
    /// holds the JSON's numbers, not an approximation of them. atlas rounds to
    /// 10⁻⁷° and 1 cm before writing either file and the binary is on exactly that
    /// grid, so the only difference allowed is float parsing (an ulp or so).
    #[test]
    fn the_binary_carries_exactly_what_the_json_does() {
        let json = parse_lod22_json(&sample_json()).unwrap();
        assert!(
            json.elements.iter().any(|e| e.part != 0),
            "no split pand in the sample"
        );
        let bin = lod22_cell::decode(&lod22_cell::encode(&json, Grid::JSON).unwrap()).unwrap();
        assert_eq!(bin.pands, json.pands);
        assert_eq!(bin.elements.len(), json.elements.len());
        let (mut exact, mut total) = (0usize, 0usize);
        for (b, j) in bin.elements.iter().zip(&json.elements) {
            assert_eq!((&b.id, b.part, &b.class), (&j.id, j.part, &j.class));
            assert_eq!(b.t, j.t);
            assert_eq!(b.p.len(), j.p.len());
            for (pb, pj) in b.p.iter().zip(&j.p) {
                total += 1;
                exact += usize::from(pb == pj);
                assert!((pb.0 - pj.0).abs() < 1e-12 && (pb.1 - pj.1).abs() < 1e-12);
                assert!((pb.2 - pj.2).abs() <= f32::EPSILON * pj.2.abs().max(1.0));
            }
        }
        eprintln!("{exact} of {total} vertices bit-identical");
    }

    fn build(body: &Lod22Body) -> Result<Option<Lod22Build>, String> {
        build_with_roofs(body, &cartopolis_geo::formats::roofs::Cell::default())
    }

    fn build_with_roofs(
        body: &Lod22Body,
        roofs: &cartopolis_geo::formats::roofs::Cell,
    ) -> Result<Option<Lod22Build>, String> {
        let (clat, clng) = (53.2194, 6.5665);
        let z = 16;
        let (ax, ay) = lat_lng_to_tile(clat, clng, z);
        build_lod22_from_body(
            body,
            SAMPLE_CELL.0,
            SAMPLE_CELL.1,
            ax,
            ay,
            z,
            clat,
            &crate::systems::terrain::TerrainSnapshot::default(),
            &crate::systems::building_facts::BagCell::default(),
            roofs,
            None,
            load_control::Detail::Near,
            None,
            &[],
        )
    }

    fn positions_and_indices(mesh: &Mesh) -> (Vec<[f32; 3]>, Vec<u32>) {
        let positions = match mesh.attribute(Mesh::ATTRIBUTE_POSITION).unwrap() {
            bevy::mesh::VertexAttributeValues::Float32x3(v) => v.clone(),
            other => panic!("unexpected position format: {other:?}"),
        };
        let indices = match mesh.indices().unwrap() {
            bevy::mesh::Indices::U32(v) => v.clone(),
            bevy::mesh::Indices::U16(v) => v.iter().map(|&i| u32::from(i)).collect(),
        };
        (positions, indices)
    }

    /// …and the mesh built from either is the same mesh: same pieces, same
    /// vertex and index counts, the same indices, positions within a millimetre.
    #[test]
    fn the_binary_and_the_json_build_the_same_meshes() {
        let text = sample_json();
        let cell = parse_lod22_json(&text).unwrap();
        let bin = lod22_cell::encode(&cell, Grid::JSON).unwrap();
        let (gj, pj, fj, dj, doorsj, hj) = build(&Lod22Body::Json(text)).unwrap().unwrap();
        let (gb, pb, fb, db, doorsb, hb) = build(&Lod22Body::Bin(bin)).unwrap().unwrap();
        assert_eq!(gj, gb);
        assert_eq!((fj.len(), dj, doorsj), (fb.len(), db, doorsb));
        assert_eq!(hj.len(), hb.len());
        assert!(!fj.is_empty(), "the sample built no buildings");
        assert_eq!(pj.len(), pb.len());
        for ((mj, ij), (mb, ib)) in pj.iter().zip(&pb) {
            assert_eq!(ij, ib);
            let (vj, xj) = positions_and_indices(mj);
            let (vb, xb) = positions_and_indices(mb);
            assert_eq!(vj.len(), vb.len(), "vertex counts differ");
            assert_eq!(xj, xb, "indices differ");
            for (a, b) in vj.iter().zip(&vb) {
                let d = Vec3::from(*a).distance(Vec3::from(*b));
                assert!(d <= 1e-3, "a vertex moved {d} m");
            }
        }
    }

    /// A roof atlas photographed is drawn in the colour it measured, on its
    /// pitched planes and its flat ones, and nothing else is.
    #[test]
    fn a_measured_roof_is_drawn_in_its_measured_colour() {
        use cartopolis_geo::formats::roofs::{Cell, Material, Roof, Surface};
        let text = sample_json();
        let cell = parse_lod22_json(&text).unwrap();
        let magenta = Surface {
            c: [255, 0, 255],
            m: Material::Tile,
            n: 100,
        };
        let mut roofs = Cell::default();
        for e in &cell.elements {
            roofs.roofs.insert(
                e.id.clone(),
                Roof {
                    f: Some(magenta.clone()),
                    p: Some(magenta.clone()),
                },
            );
        }
        let count = |roofs: &Cell| {
            let (_, pieces, ..) = build_with_roofs(&Lod22Body::Json(text.clone()), roofs)
                .unwrap()
                .unwrap();
            pieces
                .iter()
                .map(|(mesh, _)| match mesh.attribute(Mesh::ATTRIBUTE_COLOR) {
                    Some(bevy::mesh::VertexAttributeValues::Float32x4(v)) => v
                        .iter()
                        .filter(|c| c[0] == 1.0 && c[1] == 0.0 && c[2] == 1.0)
                        .count(),
                    _ => 0,
                })
                .sum::<usize>()
        };
        assert_eq!(count(&Cell::default()), 0);
        assert!(
            count(&roofs) > 1000,
            "the measured colour never reached a roof"
        );
    }

    /// A cut-off download is a failed cell — which evicts the cache entry and
    /// retries — never a panic and never a cell missing its last buildings.
    #[test]
    fn a_truncated_binary_is_an_error() {
        let cell = parse_lod22_json(&sample_json()).unwrap();
        let bin = lod22_cell::encode(&cell, Grid::JSON).unwrap();
        for n in [0, 3, 16, 17, bin.len() / 2, bin.len() - 1] {
            assert!(
                build(&Lod22Body::Bin(bin[..n].to_vec())).is_err(),
                "a {n}-byte prefix built"
            );
        }
    }

    /// The two formats never share a cache entry.
    #[test]
    fn the_formats_are_cached_apart() {
        assert_ne!(cell_cache_key(8490, 5319), bin_cache_key(8490, 5319));
        assert!(bin_cache_key(8490, 5319).ends_with(".bin"));
    }

    /// Bytes and parse+build time of one real cell, JSON against binary — the
    /// instrument behind the numbers in `docs/notes/atlas.md`. Reads the cell's
    /// JSON from `CARTO_LOD22_CELL` (default `/tmp/lod22bin/8490_5319.json`) and
    /// writes the encoding beside it as `.bin`, so `gzip -6 -c` can size both.
    ///
    /// ```text
    /// curl -o /tmp/lod22bin/8490_5319.json https://3dbag.cartopolis.org/lod22/14/8490/5319.json
    /// cargo test -p cartopolis --release print_lod22_cell_cost -- --ignored --nocapture
    /// ```
    #[test]
    #[ignore = "timing instrument; needs a downloaded cell"]
    fn print_lod22_cell_cost() {
        let path = std::env::var("CARTO_LOD22_CELL")
            .unwrap_or_else(|_| "/tmp/lod22bin/8490_5319.json".into());
        let text = std::fs::read_to_string(&path).expect("a downloaded lod22 cell");
        let cell = parse_lod22_json(&text).unwrap();
        let bin = lod22_cell::encode(&cell, Grid::JSON).unwrap();
        std::fs::write(std::path::Path::new(&path).with_extension("bin"), &bin).unwrap();
        let (verts, tris): (usize, usize) = cell
            .elements
            .iter()
            .fold((0, 0), |(v, t), e| (v + e.p.len(), t + e.t.len()));
        eprintln!(
            "{path}: {} elements, {verts} vertices, {tris} triangles; json {} B, bin {} B",
            cell.elements.len(),
            text.len(),
            bin.len()
        );
        let best = |f: &dyn Fn()| {
            (0..5)
                .map(|_| {
                    let t = std::time::Instant::now();
                    f();
                    t.elapsed().as_secs_f64() * 1e3
                })
                .fold(f64::INFINITY, f64::min)
        };
        let json_parse = best(&|| {
            std::hint::black_box(Lod22Body::Json(text.clone()).parse().unwrap());
        });
        let bin_parse = best(&|| {
            std::hint::black_box(Lod22Body::Bin(bin.clone()).parse().unwrap());
        });
        let json_build = best(&|| {
            std::hint::black_box(build(&Lod22Body::Json(text.clone())).unwrap());
        });
        let bin_build = best(&|| {
            std::hint::black_box(build(&Lod22Body::Bin(bin.clone())).unwrap());
        });
        let encode = best(&|| {
            std::hint::black_box(lod22_cell::encode(&cell, Grid::JSON).unwrap());
        });
        eprintln!("parse only (best of 5): json {json_parse:.1} ms, bin {bin_parse:.1} ms");
        eprintln!("parse + build (best of 5): json {json_build:.1} ms, bin {bin_build:.1} ms");
        eprintln!("encode (atlas side): {encode:.1} ms");
    }
}
