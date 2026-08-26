//! Streamed OSM street detail (Overpass): the close-range layer of things the
//! vector tiles do not carry.
//!
//! One combined Overpass query per z14 cell brings in **surveyed trees**
//! (`natural=tree` / `tree_row` — the municipal street-tree registries Dutch
//! cities have imported, which is what finally puts a tree on the street it is
//! actually on, rather than only inside the land-cover polygons `scatter`
//! plants), **street lamps** (posts by day, the emissive heads and the point
//! lights of `update_lamp_lights` by night), **traffic signals** with cycling
//! heads, **zebra crossings** painted onto the road, **barriers** (walls,
//! fences, hedges, retaining walls, guard rails — extruded and solid to walk
//! into), **piers, lock gates and weirs**, and a few free-standing props the
//! tile `pois` layer lacks (bicycle racks, billboards, bollards).
//!
//! The shape of the module is `osm_buildings`' far lane: a `CellStream` of z14
//! cells around the camera, fetch on `IoTaskPool`, parse + mesh on
//! `platform::workers`, spawn capped per frame and byte-bounded through
//! `stream_ledger`, teardown through the bounded despawn queue on re-anchor /
//! terrain revision / toggle-off. Vertices are anchored to the BigSpace cell at
//! each z14 cell's centre, so f32 stays precise anywhere on Earth.
//!
//! Trees deliberately ride the *existing* vegetation pipeline: they become
//! `scatter::TreeInstance`s, are meshed by `scatter::build_vegetation_meshes`,
//! spawn with the shared Canopy/Trunk materials and the `VegetationSurface`
//! marker — so the altitude fade, the Trees toggle and the palette treat a
//! surveyed tree and a scattered one identically. No dedup is attempted against
//! the scatter: registries and land-cover overlap only inside parks, where two
//! near-coincident trees read as "more trees", and a cross-streamer keep-out
//! would couple two workers that finish in either order.
//!
//! Context for orientation comes from the query itself: `way(bn.sig)` /
//! `way(bn.zeb)` return the highway ways *containing* each signal/crossing
//! node, so a zebra's stripes run with its actual road and a signal pole stands
//! beside its actual stop line — no client-side nearest-road guessing.

use std::collections::HashMap;

use bevy::asset::RenderAssetUsages;
use bevy::prelude::*;
use bevy::render::mesh::{Indices, PrimitiveTopology};
use bevy::tasks::IoTaskPool;
use big_space::prelude::{BigSpace, Grid, GridCell};

use crate::components::{FreeCamera, VegetationSurface};
use crate::grid::{WORLD_CELL_EDGE, WORLD_SWITCH_THRESHOLD};
use crate::platform::storage;
use crate::platform::stream::{CellStream, Policy};
use crate::systems::collision::{BuildingColliders, Footprint};
use crate::systems::osm_buildings::{lat_lng_to_local, num_from_value, overpass_query, MeshData};
use crate::systems::scatter::{
    self, build_vegetation_meshes, srgb_to_linear_rgb, TreeInstance, TREE_FADE_END,
};
use crate::systems::terrain::TerrainSnapshot;
use crate::systems::vector_tiles::SurfaceGroup;
use cartopolis_core::world::{lat_lng_to_tile, tile_to_lat_lng, tile_to_meters};

/// Rung on the shared depth ladder for painted road markings — zebra bars and
/// the like (`vector_tiles::DEPTH_BIAS_STEP` is the step, and
/// `road_depth::everything_drawn_over_a_road_clears_the_road_ladder` is what
/// keeps this above the roads it is painted on).
///
/// It sat at this value while the road ladder grew from a handful of rungs to
/// 399, whose ceiling reached 1,916 depth units — so every zebra crossing on a
/// road wider than a residential street was quietly drawn *underneath* the
/// tarmac. The road ladder is two rungs again now that `road_stitch` settles the
/// overlap in geometry, and the test is there so the next person to change one
/// side hears about the other.
pub(crate) const PAINT_RUNG: f32 = 10.5;

/// Fixed grid zoom for fetch + cache, matching the building cells so one mental
/// model covers both. Cache keys are versioned (`features/v1/…`) because the
/// combined query *will* grow tags over time, and a cached cell fetched under an
/// older query would silently lack the new features forever.
const DETAIL_ZOOM: u32 = 14;
/// Cells out from the camera (1 → 3×3 ≈ ±2.2 km at NL latitudes). The small
/// props fade out by 450 m and the trees by [`TREE_FADE_END`] (2 km), so a wider
/// ring would fetch geometry nothing can see.
const CELL_RADIUS: i64 = 1;
/// One cell of hysteresis on despawn, like every other ring here.
const KEEP_RADIUS: i64 = CELL_RADIUS + 1;
/// Stop requesting new cells above this altitude — the same freeze the building
/// detail patch uses, for the same churn reason. Standing cells stay up.
const FREEZE_ALT: f32 = 3_000.0;

/// Fetch retry/backoff — mirrors the building far lane.
const MAX_ATTEMPTS: u32 = 3;
const BACKOFF_SECS: f64 = 2.0;
const BACKOFF_DOUBLINGS: u32 = 2;
const RETRY_AFTER_SECS: f64 = 60.0;
/// Despawns per frame through the bounded queue. A street-detail cell is many
/// small meshes rather than one huge one, so this is per-entity and generous.
const MAX_DESPAWNS_PER_FRAME: usize = 8;
/// Cells applied (spawned) per frame, before the ledger's byte gate.
const MAX_BUILDS_PER_FRAME: usize = 2;

/// Caps against a pathological cell (a nursery, an orchard mapped per-tree).
const MAX_TREES_PER_CELL: usize = 4_000;
const MAX_PROPS_PER_CELL: usize = 2_500;

/// Small props (lamp posts, signals, racks, bollards, zebra paint) fade over
/// the same band as the tile street furniture — they are the same size of thing.
const NEAR_FADE_START: f32 = 150.0;
const NEAR_FADE_END: f32 = 450.0;
/// Linear features (barriers, piers) are hedge-and-wall scale: visible further,
/// gone by the trees' own fade end.
const FAR_FADE_START: f32 = 800.0;
const FAR_FADE_END: f32 = TREE_FADE_END;

// ─── Feature model (parse output) ────────────────────────────────────────────

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum BarrierKind {
    Wall,
    CityWall,
    Fence,
    Hedge,
    Retaining,
    GuardRail,
}

impl BarrierKind {
    fn parse(v: &str) -> Option<Self> {
        Some(match v {
            "wall" => Self::Wall,
            "city_wall" => Self::CityWall,
            "fence" => Self::Fence,
            "hedge" => Self::Hedge,
            "retaining_wall" => Self::Retaining,
            "guard_rail" => Self::GuardRail,
            _ => return None,
        })
    }

    /// (default height m, thickness m, sRGB colour). Heights are what the tag
    /// overrides; thickness and colour are the form.
    fn form(self) -> (f32, f32, u32) {
        match self {
            Self::Wall => (1.5, 0.25, 0x9C_94_8A),
            Self::CityWall => (5.0, 1.2, 0x9A_6B_58),
            Self::Fence => (1.4, 0.06, 0x8B_88_80),
            Self::Hedge => (1.6, 0.7, 0x7E_97_74),
            Self::Retaining => (1.2, 0.3, 0xB5_B2_AC),
            Self::GuardRail => (0.75, 0.08, 0xC0_C0_BC),
        }
    }

    /// Guard rails are a band on posts, not a wall from the ground.
    fn base_height(self) -> f32 {
        match self {
            Self::GuardRail => 0.45,
            _ => 0.0,
        }
    }

    /// Whether the avatar should collide with it. A guard rail at shin height
    /// is stepped over everywhere in real life; everything else blocks.
    fn solid(self) -> bool {
        !matches!(self, Self::GuardRail)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum WaterKind {
    Weir,
    LockGate,
}

/// One parsed cell, still in (lat, lng).
#[derive(Default, Debug)]
struct DetailData {
    /// (lat, lng, height tag, crown-diameter tag, needleleaved).
    trees: Vec<(f64, f64, Option<f32>, Option<f32>, bool)>,
    tree_rows: Vec<Vec<(f64, f64)>>,
    /// (lat, lng, height tag).
    lamps: Vec<(f64, f64, Option<f32>)>,
    /// (node id, lat, lng).
    signals: Vec<(i64, f64, f64)>,
    /// (node id, lat, lng).
    crossings: Vec<(i64, f64, f64)>,
    barriers: Vec<(BarrierKind, Option<f32>, Vec<(f64, f64)>)>,
    piers: Vec<Vec<(f64, f64)>>,
    waterway: Vec<(WaterKind, Vec<(f64, f64)>)>,
    racks: Vec<(f64, f64)>,
    billboards: Vec<(f64, f64)>,
    bollards: Vec<(f64, f64)>,
    /// Highway ways fetched as node context: (node ids, geometry, width m).
    roads: Vec<(Vec<i64>, Vec<(f64, f64)>, f32, bool)>,
}

/// The combined per-cell query. Signals and crossings are named sets so their
/// containing highway ways ride along (`way(bn.…)`) — that is where a zebra's
/// direction and a signal's roadside come from.
fn detail_query(bbox: &str) -> String {
    format!(
        concat!(
            "[out:json][timeout:25];",
            "node[\"highway\"=\"traffic_signals\"]({bb})->.sig;",
            "node[\"highway\"=\"crossing\"][\"crossing\"~\"^(zebra|marked|uncontrolled)$\"]({bb})->.zeb;",
            "(",
            "node[\"natural\"=\"tree\"]({bb});",
            "way[\"natural\"=\"tree_row\"]({bb});",
            "node[\"highway\"=\"street_lamp\"]({bb});",
            "node[\"amenity\"=\"bicycle_parking\"]({bb});",
            "node[\"advertising\"=\"billboard\"]({bb});",
            "node[\"barrier\"=\"bollard\"]({bb});",
            "way[\"barrier\"~\"^(wall|city_wall|fence|hedge|retaining_wall|guard_rail)$\"]({bb});",
            "way[\"man_made\"=\"pier\"]({bb});",
            "way[\"waterway\"~\"^(weir|lock_gate)$\"]({bb});",
            ".sig;.zeb;",
            "way(bn.sig)[\"highway\"];",
            "way(bn.zeb)[\"highway\"];",
            ");out geom;"
        ),
        bb = bbox
    )
}

fn cache_key(cx: i64, cy: i64) -> String {
    format!("features/v1/{}/{}/{}.json", DETAIL_ZOOM, cx, cy)
}

/// Fetch one cell's text: disk cache, else the combined Overpass query
/// (validated before caching — the remark channel again).
async fn fetch_cell_text(cx: i64, cy: i64) -> Result<String, String> {
    let key = cache_key(cx, cy);
    if let Some(cached) = storage::get_string(storage::Namespace::Cache, &key).await {
        return Ok(cached);
    }
    let (north, west) = tile_to_lat_lng(cx, cy, DETAIL_ZOOM);
    let (south, east) = tile_to_lat_lng(cx + 1, cy + 1, DETAIL_ZOOM);
    let bbox = format!("{south},{west},{north},{east}");
    let fetched = overpass_query(&detail_query(&bbox)).await?;
    // Pace live queries against a public endpoint, exactly as the building
    // cells do (zero against our own instance).
    #[cfg(not(target_arch = "wasm32"))]
    {
        let (_, throttle) = crate::systems::osm_buildings::overpass_pace();
        if !throttle.is_zero() {
            std::thread::sleep(throttle);
        }
    }
    storage::put(storage::Namespace::Cache, &key, fetched.as_bytes()).await;
    Ok(fetched)
}

fn tag_str<'a>(tags: Option<&'a serde_json::Value>, key: &str) -> Option<&'a str> {
    tags?.get(key)?.as_str()
}

fn tag_num(tags: Option<&serde_json::Value>, key: &str) -> Option<f32> {
    tags?.get(key).and_then(num_from_value)
}

/// Estimate a road's paved width from its tags, for zebra length and signal
/// pole offset. Deliberately coarse — the answer only positions paint and poles.
fn road_width(tags: Option<&serde_json::Value>) -> f32 {
    if let Some(w) = tag_num(tags, "width") {
        return w.clamp(3.0, 15.0);
    }
    if let Some(lanes) = tag_num(tags, "lanes") {
        return (lanes * 3.25).clamp(3.0, 15.0);
    }
    match tag_str(tags, "highway").unwrap_or("") {
        "motorway" | "trunk" => 12.0,
        "primary" | "secondary" => 7.5,
        "tertiary" => 6.5,
        "residential" | "unclassified" | "living_street" => 5.0,
        "service" => 4.0,
        _ => 4.5,
    }
}

/// True for the ways that are the *pedestrian* half of a crossing — usable for
/// orientation only as a fallback, and rotated 90°.
fn is_footway(tags: Option<&serde_json::Value>) -> bool {
    matches!(
        tag_str(tags, "highway").unwrap_or(""),
        "footway" | "path" | "cycleway" | "steps" | "pedestrian"
    )
}

/// True for a feature that is not on the surface — a tunnel, a covered way, or
/// anything explicitly stacked below ground.
///
/// **Every other consumer of this ground already refuses these, and this layer
/// arrived without the check.** `vector_tiles::drawn_street` keeps a tunnelled
/// carriageway out of both the geometry and the raster, `extract_street_labels`
/// keeps its name off whatever is genuinely above it, and `scatter`/`turf`
/// inherit the same refusal by reusing `drawn_street`. Without it here, a run of
/// railings, a bollard line or a zebra tagged on a buried way is painted flat on
/// the park or the carriageway over the top of it.
///
/// `covered` counts alongside `tunnel` because an arcade and a bore are the same
/// thing to a camera looking down, and a negative `layer` catches the sources
/// that describe the stack without either flag. Overpass returns every tag as a
/// string, which `tag_num` already parses (leading `-` included).
fn underground(tags: Option<&serde_json::Value>) -> bool {
    let flagged = |key: &str| matches!(tag_str(tags, key), Some(v) if v != "no");
    flagged("tunnel") || flagged("covered") || tag_num(tags, "layer").is_some_and(|l| l < 0.0)
}

/// Parse the combined response. Elements disambiguate by tag, not by position
/// in the response: a way is a tree row, a barrier, a pier, a waterway or a
/// context road depending on what it carries.
fn parse_detail(text: &str) -> Result<DetailData, String> {
    let json: serde_json::Value = serde_json::from_str(text).map_err(|e| e.to_string())?;
    if let Some(remark) = json.get("remark").and_then(|r| r.as_str()) {
        return Err(format!("Overpass remark: {}", remark.trim()));
    }
    let elements = json
        .get("elements")
        .and_then(|e| e.as_array())
        .ok_or_else(|| "no elements in Overpass response".to_string())?;

    let mut out = DetailData::default();
    // Nodes belonging to a buried carriageway. A signal or a zebra carries no
    // grade of its own — it is a point on a way — so the only thing that can say
    // it is underground is the way the query already fetched for its
    // orientation, and that way may arrive after the node does.
    let mut buried_nodes: std::collections::HashSet<i64> = std::collections::HashSet::new();
    for el in elements {
        let tags = el.get("tags");
        match el.get("type").and_then(|t| t.as_str()) {
            Some("node") => {
                if underground(tags) {
                    continue;
                }
                let (Some(lat), Some(lng)) = (
                    el.get("lat").and_then(|v| v.as_f64()),
                    el.get("lon").and_then(|v| v.as_f64()),
                ) else {
                    continue;
                };
                let id = el.get("id").and_then(|v| v.as_i64()).unwrap_or(0);
                if tag_str(tags, "natural") == Some("tree") {
                    let needle = tag_str(tags, "leaf_type") == Some("needleleaved");
                    out.trees.push((
                        lat,
                        lng,
                        tag_num(tags, "height"),
                        tag_num(tags, "diameter_crown"),
                        needle,
                    ));
                } else if tag_str(tags, "highway") == Some("street_lamp") {
                    out.lamps.push((lat, lng, tag_num(tags, "height")));
                } else if tag_str(tags, "highway") == Some("traffic_signals") {
                    out.signals.push((id, lat, lng));
                } else if tag_str(tags, "highway") == Some("crossing") {
                    // `uncontrolled` counts only when the ref says zebra; the
                    // query pre-filters the rest.
                    let c = tag_str(tags, "crossing").unwrap_or("");
                    if c == "zebra"
                        || c == "marked"
                        || (c == "uncontrolled" && tag_str(tags, "crossing_ref") == Some("zebra"))
                    {
                        out.crossings.push((id, lat, lng));
                    }
                } else if tag_str(tags, "amenity") == Some("bicycle_parking") {
                    out.racks.push((lat, lng));
                } else if tag_str(tags, "advertising") == Some("billboard") {
                    out.billboards.push((lat, lng));
                } else if tag_str(tags, "barrier") == Some("bollard") {
                    out.bollards.push((lat, lng));
                }
            }
            Some("way") => {
                let Some(geom) = el.get("geometry").and_then(|g| g.as_array()) else {
                    continue;
                };
                let line: Vec<(f64, f64)> = geom
                    .iter()
                    .filter_map(|pt| Some((pt.get("lat")?.as_f64()?, pt.get("lon")?.as_f64()?)))
                    .collect();
                if line.len() < 2 {
                    continue;
                }
                if underground(tags) {
                    // Remember a buried carriageway's nodes before dropping it,
                    // so the signals and zebras standing on it go too.
                    if tag_str(tags, "highway").is_some() {
                        if let Some(ids) = el.get("nodes").and_then(|n| n.as_array()) {
                            buried_nodes.extend(ids.iter().filter_map(|v| v.as_i64()));
                        }
                    }
                    continue;
                }
                if tag_str(tags, "natural") == Some("tree_row") {
                    out.tree_rows.push(line);
                } else if let Some(kind) = tag_str(tags, "barrier").and_then(BarrierKind::parse) {
                    out.barriers.push((kind, tag_num(tags, "height"), line));
                } else if tag_str(tags, "man_made") == Some("pier") {
                    out.piers.push(line);
                } else if let Some(kind) = match tag_str(tags, "waterway") {
                    Some("weir") => Some(WaterKind::Weir),
                    Some("lock_gate") => Some(WaterKind::LockGate),
                    _ => None,
                } {
                    out.waterway.push((kind, line));
                } else if tag_str(tags, "highway").is_some() {
                    let ids: Vec<i64> = el
                        .get("nodes")
                        .and_then(|n| n.as_array())
                        .map(|n| n.iter().filter_map(|v| v.as_i64()).collect())
                        .unwrap_or_default();
                    if ids.len() == line.len() {
                        out.roads
                            .push((ids, line, road_width(tags), is_footway(tags)));
                    }
                }
            }
            _ => {}
        }
    }
    // Applied after the loop because the carriageway that condemns a node can
    // appear anywhere in the response, including after the node itself.
    if !buried_nodes.is_empty() {
        out.signals.retain(|(id, _, _)| !buried_nodes.contains(id));
        out.crossings
            .retain(|(id, _, _)| !buried_nodes.contains(id));
    }
    Ok(out)
}

// ─── Worker build ────────────────────────────────────────────────────────────

/// Which material a built mesh binds. `Signal(n)` is `group (0|1) * 3 + colour
/// (0 red, 1 amber, 2 green)` — six tiny shared materials whose emissive a
/// single global system cycles, so a whole city's signals cost six writes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum PropClass {
    Canopy,
    Trunk,
    /// Vertex-coloured solids that fade with the street furniture band.
    Solid,
    /// Vertex-coloured solids that stay to the tree fade band (walls, piers).
    Linear,
    /// Road paint: unlit white, depth-biased over the carriageway.
    Paint,
    /// Lamp heads — emissive rides the night factor.
    LampHead,
    Signal(u8),
}

/// A finished cell: meshes tagged by material class, colliders in world XZ,
/// lamp-light positions in world space (terrain-lifted, at head height).
pub(crate) struct DetailCellBuild {
    grid_cell: GridCell,
    meshes: Vec<(PropClass, Mesh)>,
    colliders: Vec<Footprint>,
    lamp_points: Vec<Vec3>,
}

type CellResult = Result<Option<DetailCellBuild>, String>;

pub(crate) fn linear(rgb: u32) -> [f32; 4] {
    let [r, g, b] = srgb_to_linear_rgb(rgb);
    [r, g, b, 1.0]
}

pub(crate) fn shade(c: [f32; 4], f: f32) -> [f32; 4] {
    [c[0] * f, c[1] * f, c[2] * f, c[3]]
}

/// Deterministic per-feature randomness, keyed on the raw coordinate so a cell
/// that streams out and back regrows identically.
pub(crate) fn feature_seed(lat: f64, lng: f64) -> u64 {
    let a = (lat * 1e7) as i64 as u64;
    let b = (lng * 1e7) as i64 as u64;
    a.wrapping_mul(0x9E37_79B9_7F4A_7C15) ^ b.rotate_left(31)
}

pub(crate) fn srand(seed: u64, stream: u32) -> f32 {
    scatter::rand01(seed, stream)
}

/// Convert one accumulated `MeshData` into a Bevy mesh (positions already
/// terrain-lifted by the callers).
fn into_mesh(buf: MeshData) -> Option<Mesh> {
    buf.into_mesh()
}

/// A vertical box between two ground points — the barrier segment primitive.
/// `a`/`b` are cell-local XZ, `ya`/`yb` the terrain under each end.
#[allow(clippy::too_many_arguments)]
fn push_wall_segment(
    buf: &mut MeshData,
    a: Vec2,
    b: Vec2,
    ya: f32,
    yb: f32,
    base: f32,
    top: f32,
    half_t: f32,
    color: [f32; 4],
) {
    let dir = (b - a).normalize_or_zero();
    if dir == Vec2::ZERO {
        return;
    }
    let n = Vec2::new(-dir.y, dir.x) * half_t;
    let (a0, a1) = (a - n, a + n);
    let (b0, b1) = (b - n, b + n);
    let v = |p: Vec2, y: f32| Vec3::new(p.x, y, p.y);
    let top_c = shade(color, 1.08);
    // Two long faces, two ends, one top — no bottom (it is in the ground).
    let out0 = Vec3::new(-n.x, 0.0, -n.y).normalize_or_zero();
    buf.quad_facing(
        v(a0, ya + base),
        v(b0, yb + base),
        v(b0, yb + top),
        v(a0, ya + top),
        color,
        out0,
    );
    buf.quad_facing(
        v(a1, ya + base),
        v(b1, yb + base),
        v(b1, yb + top),
        v(a1, ya + top),
        color,
        -out0,
    );
    let end_dir = Vec3::new(-dir.x, 0.0, -dir.y);
    buf.quad_facing(
        v(a0, ya + base),
        v(a1, ya + base),
        v(a1, ya + top),
        v(a0, ya + top),
        color,
        end_dir,
    );
    buf.quad_facing(
        v(b0, yb + base),
        v(b1, yb + base),
        v(b1, yb + top),
        v(b0, yb + top),
        color,
        -end_dir,
    );
    buf.quad_facing(
        v(a0, ya + top),
        v(b0, yb + top),
        v(b1, yb + top),
        v(a1, ya + top),
        top_c,
        Vec3::Y,
    );
}

/// An axis-aligned-in-object-space box, yawed and placed. `center`/`half` in
/// metres, `y0` the ground.
pub(crate) fn push_yawed_box(
    buf: &mut MeshData,
    at: Vec2,
    y0: f32,
    yaw: f32,
    center: [f32; 3],
    half: [f32; 3],
    color: [f32; 4],
) {
    let (sn, cs) = yaw.sin_cos();
    let place = |lx: f32, ly: f32, lz: f32| {
        Vec3::new(at.x + lx * cs + lz * sn, y0 + ly, at.y - lx * sn + lz * cs)
    };
    let (cx, cy, cz) = (center[0], center[1], center[2]);
    let (hx, hy, hz) = (half[0], half[1], half[2]);
    let corners = [
        place(cx - hx, cy - hy, cz - hz),
        place(cx + hx, cy - hy, cz - hz),
        place(cx + hx, cy - hy, cz + hz),
        place(cx - hx, cy - hy, cz + hz),
        place(cx - hx, cy + hy, cz - hz),
        place(cx + hx, cy + hy, cz - hz),
        place(cx + hx, cy + hy, cz + hz),
        place(cx - hx, cy + hy, cz + hz),
    ];
    let x_dir = place(1.0, 0.0, 0.0) - place(0.0, 0.0, 0.0);
    let z_dir = place(0.0, 0.0, 1.0) - place(0.0, 0.0, 0.0);
    // Six faces, outward-facing.
    buf.quad_facing(
        corners[0], corners[1], corners[5], corners[4], color, -z_dir,
    );
    buf.quad_facing(corners[2], corners[3], corners[7], corners[6], color, z_dir);
    buf.quad_facing(
        corners[3], corners[0], corners[4], corners[7], color, -x_dir,
    );
    buf.quad_facing(corners[1], corners[2], corners[6], corners[5], color, x_dir);
    buf.quad_facing(
        corners[4],
        corners[5],
        corners[6],
        corners[7],
        shade(color, 1.08),
        Vec3::Y,
    );
    buf.quad_facing(
        corners[0],
        corners[3],
        corners[2],
        corners[1],
        shade(color, 0.8),
        -Vec3::Y,
    );
}

/// Sample points every `step` metres along a local-frame polyline.
fn along(points: &[Vec2], step: f32) -> Vec<(Vec2, Vec2)> {
    let mut out = Vec::new();
    let mut carried = 0.0f32;
    for pair in points.windows(2) {
        let (a, b) = (pair[0], pair[1]);
        let seg = a.distance(b);
        if seg <= f32::EPSILON {
            continue;
        }
        let dir = (b - a) / seg;
        let mut d = carried;
        while d < seg {
            out.push((a + dir * d, dir));
            d += step;
        }
        carried = d - seg;
    }
    out
}

const METAL: u32 = 0x9A_98_93;
const WOOD: u32 = 0xB4_A9_98;
const LAMP_POST: u32 = 0x4A_4E_52;
const SIGNAL_POST: u32 = 0x3F_44_48;
const PILE: u32 = 0x6E_66_5C;

/// Everything the worker needs, captured by value.
struct BuildCtx {
    ax: i64,
    ay: i64,
    z: u32,
    tile_m: f32,
    /// Cell bounds for node ownership (half-open, matching the far lane).
    south: f64,
    west: f64,
    north: f64,
    east: f64,
    ground: TerrainSnapshot,
    wants_trees: bool,
}

impl BuildCtx {
    fn local(&self, lat: f64, lng: f64) -> Vec2 {
        lat_lng_to_local(lat, lng, self.ax, self.ay, self.z, self.tile_m)
    }

    fn owns(&self, lat: f64, lng: f64) -> bool {
        lat >= self.south && lat < self.north && lng >= self.west && lng < self.east
    }
}

/// Build one cell's meshes from a fetched response.
fn build_cell(text: &str, cx: i64, cy: i64, ctx: &BuildCtx) -> CellResult {
    let data = parse_detail(text)?;
    // Anchor to the BigSpace cell at the cell centre, like the far lane.
    let (north, west) = tile_to_lat_lng(cx, cy, DETAIL_ZOOM);
    let (south, east) = tile_to_lat_lng(cx + 1, cy + 1, DETAIL_ZOOM);
    let c_world = ctx.local((north + south) / 2.0, (west + east) / 2.0);
    let (grid_cell, _) = Grid::new(WORLD_CELL_EDGE, WORLD_SWITCH_THRESHOLD)
        .translation_to_grid(Vec3::new(c_world.x, 0.0, c_world.y).as_dvec3());
    let ox = grid_cell.x as f32 * WORLD_CELL_EDGE;
    let oz = grid_cell.z as f32 * WORLD_CELL_EDGE;
    // Cell-local position of a (lat,lng), and the world/terrain frame around it.
    let local = |lat: f64, lng: f64| {
        let w = ctx.local(lat, lng);
        Vec2::new(w.x - ox, w.y - oz)
    };
    let ground_at = |p: Vec2| ctx.ground.at(p.x + ox, p.y + oz);

    let mut meshes: Vec<(PropClass, Mesh)> = Vec::new();
    let mut colliders: Vec<Footprint> = Vec::new();
    let mut lamp_points: Vec<Vec3> = Vec::new();

    // ── Trees ────────────────────────────────────────────────────────────
    if ctx.wants_trees {
        let mut instances: Vec<TreeInstance> = Vec::new();
        let mut push_tree =
            |p: Vec2, height: Option<f32>, crown: Option<f32>, needle: bool, seed: u64| {
                if instances.len() >= MAX_TREES_PER_CELL {
                    return;
                }
                let h = height
                    .unwrap_or_else(|| 8.0 + 6.0 * srand(seed, 1))
                    .clamp(2.0, 40.0);
                let r = crown
                    .map(|d| d * 0.5)
                    .unwrap_or_else(|| {
                        let base = if needle { h * 0.22 } else { h * 0.32 };
                        base * (0.85 + 0.3 * srand(seed, 2))
                    })
                    .clamp(0.8, 9.0);
                instances.push(TreeInstance {
                    x: p.x,
                    z: p.y,
                    radius: r,
                    height: h,
                    tone: if needle {
                        0.7 + 0.3 * srand(seed, 3)
                    } else {
                        srand(seed, 3)
                    },
                    luma: 0.9 + 0.2 * srand(seed, 4),
                });
            };
        for &(lat, lng, h, crown, needle) in &data.trees {
            if !ctx.owns(lat, lng) {
                continue;
            }
            push_tree(local(lat, lng), h, crown, needle, feature_seed(lat, lng));
        }
        for row in &data.tree_rows {
            let Some(&(flat, flng)) = row.first() else {
                continue;
            };
            if !ctx.owns(flat, flng) {
                continue;
            }
            let line: Vec<Vec2> = row.iter().map(|&(la, lo)| local(la, lo)).collect();
            for (p, _) in along(&line, 7.0) {
                let seed = feature_seed(flat + p.x as f64 * 1e-5, flng + p.y as f64 * 1e-5);
                push_tree(p, None, None, false, seed);
            }
        }
        // The vegetation builder works in a tile-local frame with ground y = 0;
        // ours is cell-local. Lift each mesh onto the terrain afterwards, the
        // way `map_geometry` does for volumetric groups (no refinement — a
        // tree's normals describe the tree, not the hill).
        // The same derived cap the barrier pieces below take, and for the same
        // reason: the raw upload ceiling is `None` off the phone, which leaves a
        // dense row of surveyed trees as one unsplittable mesh.
        for sm in build_vegetation_meshes(
            &instances,
            Some(crate::systems::lod22::max_cell_mesh_bytes()),
        ) {
            let mut positions = sm.positions.clone();
            if ctx.ground.is_ready() {
                for p in positions.iter_mut() {
                    p[1] += ctx.ground.at(p[0] + ox, p[2] + oz);
                }
            }
            let mut mesh = Mesh::new(
                PrimitiveTopology::TriangleList,
                RenderAssetUsages::RENDER_WORLD,
            )
            .with_inserted_attribute(Mesh::ATTRIBUTE_POSITION, positions)
            .with_inserted_attribute(Mesh::ATTRIBUTE_NORMAL, sm.normals.clone())
            .with_inserted_indices(Indices::U32(sm.indices.clone()));
            if sm.colors.len() == sm.positions.len() {
                mesh.insert_attribute(Mesh::ATTRIBUTE_COLOR, sm.colors.clone());
            }
            let class = if sm.group == SurfaceGroup::Trunk {
                PropClass::Trunk
            } else {
                PropClass::Canopy
            };
            meshes.push((class, mesh));
        }
    }

    // ── Road context index (for signals + crossings) ─────────────────────
    // node id → (direction of the containing way at that node, width, footway).
    let mut road_at: HashMap<i64, (Vec2, f32, bool)> = HashMap::new();
    for (ids, line, width, footway) in &data.roads {
        let pts: Vec<Vec2> = line.iter().map(|&(la, lo)| local(la, lo)).collect();
        for (i, id) in ids.iter().enumerate() {
            let prev = if i > 0 { pts[i - 1] } else { pts[i] };
            let next = if i + 1 < pts.len() {
                pts[i + 1]
            } else {
                pts[i]
            };
            let dir = (next - prev).normalize_or_zero();
            if dir == Vec2::ZERO {
                continue;
            }
            // A road beats a footway; among roads, the wider wins (the zebra
            // crosses the carriageway, not the side street).
            let e = road_at.entry(*id).or_insert((dir, *width, *footway));
            let better = (!*footway && e.2) || (*footway == e.2 && *width > e.1);
            if better {
                *e = (dir, *width, *footway);
            }
        }
    }
    let road_for = |id: i64| {
        road_at.get(&id).map(|&(dir, w, footway)| {
            if footway {
                // The footway *is* the walking direction; the road runs across it.
                (Vec2::new(-dir.y, dir.x), w.max(5.0))
            } else {
                (dir, w)
            }
        })
    };

    // ── Small props (Solid) ──────────────────────────────────────────────
    let mut solid = MeshData::default();
    let mut paint = MeshData::default();
    let mut heads = MeshData::default();
    let mut signals: [MeshData; 6] = std::array::from_fn(|_| MeshData::default());
    let mut props = 0usize;
    let white = [1.0, 1.0, 1.0, 1.0];

    for &(lat, lng, height) in &data.lamps {
        if !ctx.owns(lat, lng) || props >= MAX_PROPS_PER_CELL {
            continue;
        }
        props += 1;
        let p = local(lat, lng);
        let y0 = ground_at(p);
        let seed = feature_seed(lat, lng);
        let h = height.unwrap_or(5.0).clamp(2.5, 12.0);
        let yaw = srand(seed, 1) * std::f32::consts::TAU;
        let post = linear(LAMP_POST);
        push_yawed_box(
            &mut solid,
            p,
            y0,
            yaw,
            [0.0, h * 0.5, 0.0],
            [0.05, h * 0.5, 0.05],
            post,
        );
        // The head: a squat lantern box on top, in its own emissive mesh.
        push_yawed_box(
            &mut heads,
            p,
            y0,
            yaw,
            [0.0, h + 0.14, 0.0],
            [0.14, 0.14, 0.14],
            white,
        );
        push_yawed_box(
            &mut solid,
            p,
            y0,
            yaw,
            [0.0, h + 0.31, 0.0],
            [0.17, 0.03, 0.17],
            post,
        );
        lamp_points.push(Vec3::new(p.x + ox, y0 + h, p.y + oz));
    }

    for &(id, lat, lng) in &data.signals {
        if !ctx.owns(lat, lng) || props >= MAX_PROPS_PER_CELL {
            continue;
        }
        props += 1;
        let p = local(lat, lng);
        let (dir, width) = road_for(id).unwrap_or((Vec2::X, 5.0));
        // Stand beside the carriageway, on the right of the direction of
        // travel, like the stop line the node marks.
        let side = Vec2::new(-dir.y, dir.x);
        let at = p + side * (width * 0.5 + 0.6);
        let y0 = ground_at(at);
        let yaw = dir.y.atan2(dir.x);
        let group = signal_group(dir);
        let post = linear(SIGNAL_POST);
        push_yawed_box(
            &mut solid,
            at,
            y0,
            yaw,
            [0.0, 1.8, 0.0],
            [0.05, 1.8, 0.05],
            post,
        );
        push_yawed_box(
            &mut solid,
            at,
            y0,
            yaw,
            [0.0, 3.25, 0.0],
            [0.11, 0.42, 0.09],
            post,
        );
        // Three lamps, each in its phase-group + colour mesh; the cycle system
        // lights them by material.
        for (slot, dy) in [(0u8, 0.26), (1, 0.0), (2, -0.26)] {
            push_yawed_box(
                &mut signals[(group * 3 + slot) as usize],
                at,
                y0,
                yaw,
                [0.0, 3.25 + dy, 0.10],
                [0.07, 0.07, 0.03],
                white,
            );
        }
        colliders.extend(square_collider(at + Vec2::new(ox, oz), 0.15));
    }

    for &(id, lat, lng) in &data.crossings {
        if !ctx.owns(lat, lng) {
            continue;
        }
        let Some((dir, width)) = road_for(id) else {
            continue;
        };
        let p = local(lat, lng);
        let walk = Vec2::new(-dir.y, dir.x);
        let y0 = ground_at(p) + 0.02;
        // Stripes elongated in the walking direction, repeating along the
        // road: five bars, 0.5 m wide on a 0.9 m pitch.
        for i in 0..5 {
            let c = p + dir * ((i as f32 - 2.0) * 0.9);
            let half_w = width * 0.5 - 0.2;
            let a = c - walk * half_w;
            let b = c + walk * half_w;
            let n = dir * 0.25;
            paint.quad_facing(
                Vec3::new(a.x - n.x, y0, a.y - n.y),
                Vec3::new(b.x - n.x, y0, b.y - n.y),
                Vec3::new(b.x + n.x, y0, b.y + n.y),
                Vec3::new(a.x + n.x, y0, a.y + n.y),
                white,
                Vec3::Y,
            );
        }
    }

    for &(lat, lng) in &data.bollards {
        if !ctx.owns(lat, lng) || props >= MAX_PROPS_PER_CELL {
            continue;
        }
        props += 1;
        let p = local(lat, lng);
        let y0 = ground_at(p);
        push_yawed_box(
            &mut solid,
            p,
            y0,
            0.0,
            [0.0, 0.42, 0.0],
            [0.07, 0.42, 0.07],
            linear(METAL),
        );
        colliders.extend(square_collider(p + Vec2::new(ox, oz), 0.12));
    }

    for &(lat, lng) in &data.racks {
        if !ctx.owns(lat, lng) || props >= MAX_PROPS_PER_CELL {
            continue;
        }
        props += 1;
        let p = local(lat, lng);
        let y0 = ground_at(p);
        let seed = feature_seed(lat, lng);
        let yaw = srand(seed, 1) * std::f32::consts::TAU;
        let metal = linear(METAL);
        // Three staple hoops on a half-metre pitch.
        for i in -1i32..=1 {
            let cz = i as f32 * 0.5;
            push_yawed_box(
                &mut solid,
                p,
                y0,
                yaw,
                [-0.35, 0.35, cz],
                [0.03, 0.35, 0.03],
                metal,
            );
            push_yawed_box(
                &mut solid,
                p,
                y0,
                yaw,
                [0.35, 0.35, cz],
                [0.03, 0.35, 0.03],
                metal,
            );
            push_yawed_box(
                &mut solid,
                p,
                y0,
                yaw,
                [0.0, 0.72, cz],
                [0.38, 0.03, 0.03],
                metal,
            );
        }
        colliders.extend(square_collider(p + Vec2::new(ox, oz), 0.8));
    }

    for &(lat, lng) in &data.billboards {
        if !ctx.owns(lat, lng) || props >= MAX_PROPS_PER_CELL {
            continue;
        }
        props += 1;
        let p = local(lat, lng);
        let y0 = ground_at(p);
        let seed = feature_seed(lat, lng);
        let yaw = srand(seed, 1) * std::f32::consts::TAU;
        let metal = linear(METAL);
        push_yawed_box(
            &mut solid,
            p,
            y0,
            yaw,
            [-0.9, 1.1, 0.0],
            [0.06, 1.1, 0.06],
            metal,
        );
        push_yawed_box(
            &mut solid,
            p,
            y0,
            yaw,
            [0.9, 1.1, 0.0],
            [0.06, 1.1, 0.06],
            metal,
        );
        // The board face, slightly warm white — an unlit ad would be a black
        // rectangle at night, and a lit one is someone else's feature.
        push_yawed_box(
            &mut solid,
            p,
            y0,
            yaw,
            [0.0, 3.0, 0.0],
            [1.55, 1.05, 0.06],
            [0.92, 0.90, 0.86, 1.0],
        );
        colliders.extend(square_collider(p + Vec2::new(ox, oz), 0.3));
    }

    // ── Linear features ──────────────────────────────────────────────────
    let mut lin = MeshData::default();
    for (kind, height, line) in &data.barriers {
        let Some(&(flat, flng)) = line.first() else {
            continue;
        };
        if !ctx.owns(flat, flng) {
            continue;
        }
        let (default_h, half_t, rgb) = {
            let (h, t, c) = kind.form();
            (h, t * 0.5, c)
        };
        let top = height.unwrap_or(default_h).clamp(0.3, 8.0);
        let base = kind.base_height().min(top - 0.1);
        let color = linear(rgb);
        let pts: Vec<Vec2> = line.iter().map(|&(la, lo)| local(la, lo)).collect();
        // Subdivide to ≤ 10 m so the wall follows the ground.
        for pair in pts.windows(2) {
            let (a, b) = (pair[0], pair[1]);
            let len = a.distance(b);
            if len <= f32::EPSILON {
                continue;
            }
            let steps = (len / 10.0).ceil().max(1.0) as usize;
            for s in 0..steps {
                let t0 = s as f32 / steps as f32;
                let t1 = (s + 1) as f32 / steps as f32;
                let pa = a.lerp(b, t0);
                let pb = a.lerp(b, t1);
                push_wall_segment(
                    &mut lin,
                    pa,
                    pb,
                    ground_at(pa),
                    ground_at(pb),
                    base,
                    top,
                    half_t,
                    color,
                );
            }
            if kind.solid() {
                colliders.extend(segment_collider(
                    a + Vec2::new(ox, oz),
                    b + Vec2::new(ox, oz),
                    half_t.max(0.15),
                ));
            }
        }
        // Guard-rail posts, every 2 m — the band alone floats unexplained.
        if *kind == BarrierKind::GuardRail {
            for (p, _) in along(&pts, 2.0) {
                let y0 = ground_at(p);
                push_yawed_box(
                    &mut lin,
                    p,
                    y0,
                    0.0,
                    [0.0, 0.3, 0.0],
                    [0.03, 0.3, 0.03],
                    color,
                );
            }
        }
    }

    for line in &data.piers {
        let Some(&(flat, flng)) = line.first() else {
            continue;
        };
        if !ctx.owns(flat, flng) {
            continue;
        }
        let pts: Vec<Vec2> = line.iter().map(|&(la, lo)| local(la, lo)).collect();
        let deck = linear(WOOD);
        let pile = linear(PILE);
        // One water level for the whole pier (a deck is straight), taken from
        // the terrain under its head — which over water is roughly the surface.
        let level = ground_at(pts[0]);
        // Deck: a 2.4 m plank slab at ~half a metre over the water, with piles
        // every 4 m reaching down into it.
        for pair in pts.windows(2) {
            push_wall_segment(
                &mut lin,
                pair[0],
                pair[1],
                level + 0.47,
                level + 0.47,
                0.0,
                0.08,
                1.2,
                deck,
            );
        }
        for (p, _) in along(&pts, 4.0) {
            push_yawed_box(
                &mut lin,
                p,
                level - 0.6,
                0.0,
                [0.0, 0.55, 0.0],
                [0.09, 0.55, 0.09],
                pile,
            );
        }
    }

    for (kind, line) in &data.waterway {
        let Some(&(flat, flng)) = line.first() else {
            continue;
        };
        if !ctx.owns(flat, flng) {
            continue;
        }
        let pts: Vec<Vec2> = line.iter().map(|&(la, lo)| local(la, lo)).collect();
        let level = ground_at(pts[0]);
        match kind {
            // A weir: a low sill just proud of the water.
            WaterKind::Weir => {
                for pair in pts.windows(2) {
                    push_wall_segment(
                        &mut lin,
                        pair[0],
                        pair[1],
                        level,
                        level,
                        -0.3,
                        0.35,
                        0.4,
                        linear(0xB5_B2_AC),
                    );
                }
            }
            // A lock gate: a dark timber wall across the chamber, with posts.
            WaterKind::LockGate => {
                let dark = shade(linear(WOOD), 0.55);
                for pair in pts.windows(2) {
                    push_wall_segment(
                        &mut lin, pair[0], pair[1], level, level, -0.4, 1.3, 0.25, dark,
                    );
                }
                for p in [pts[0], *pts.last().unwrap()] {
                    push_yawed_box(
                        &mut lin,
                        p,
                        level,
                        0.0,
                        [0.0, 0.9, 0.0],
                        [0.16, 0.9, 0.16],
                        dark,
                    );
                }
            }
        }
    }

    // ── Assemble ─────────────────────────────────────────────────────────
    let max_bytes = crate::systems::lod22::max_cell_mesh_bytes();
    for m in solid.into_meshes(max_bytes) {
        meshes.push((PropClass::Solid, m));
    }
    for m in lin.into_meshes(max_bytes) {
        meshes.push((PropClass::Linear, m));
    }
    if let Some(m) = into_mesh(paint) {
        meshes.push((PropClass::Paint, m));
    }
    if let Some(m) = into_mesh(heads) {
        meshes.push((PropClass::LampHead, m));
    }
    for (i, buf) in signals.into_iter().enumerate() {
        if let Some(m) = into_mesh(buf) {
            meshes.push((PropClass::Signal(i as u8), m));
        }
    }

    if meshes.is_empty() && colliders.is_empty() && lamp_points.is_empty() {
        return Ok(None);
    }
    Ok(Some(DetailCellBuild {
        grid_cell,
        meshes,
        colliders,
        lamp_points,
    }))
}

/// Which signal phase group a road direction belongs to: roughly north–south
/// approaches are group 0, east–west group 1 — opposing arms of a junction then
/// alternate green the way a real junction does.
fn signal_group(dir: Vec2) -> u8 {
    if dir.x.abs() > dir.y.abs() {
        1
    } else {
        0
    }
}

/// A small square footprint (for point props the avatar should not pass through).
fn square_collider(center: Vec2, half: f32) -> Option<Footprint> {
    Footprint::new(vec![
        center + Vec2::new(-half, -half),
        center + Vec2::new(half, -half),
        center + Vec2::new(half, half),
        center + Vec2::new(-half, half),
    ])
}

/// A thin rectangle along a wall segment.
fn segment_collider(a: Vec2, b: Vec2, half_t: f32) -> Option<Footprint> {
    let dir = (b - a).normalize_or_zero();
    if dir == Vec2::ZERO {
        return None;
    }
    let n = Vec2::new(-dir.y, dir.x) * half_t;
    Footprint::new(vec![a - n, b - n, b + n, a + n])
}

// ─── Resource + main system ──────────────────────────────────────────────────

/// Marker: a street-detail mesh that fades on the near (street furniture) band.
#[derive(Component)]
pub struct PropNear;

/// Marker: a street-detail mesh that fades on the far (barrier/tree) band.
#[derive(Component)]
pub struct PropFar;

#[derive(Resource)]
pub struct StreetDetail {
    anchor: Option<(i64, i64, u32, f64)>,
    terrain_revision: u32,
    /// (master toggle, trees toggle) the standing cells were built under; a
    /// change is a full retear, like `DetailWants`.
    wants_was: Option<(bool, bool)>,
    cells: CellStream<(i64, i64), CellResult>,
    cell_entities: HashMap<(i64, i64), Vec<Entity>>,
    /// Lamp head positions (world frame) per cell, for the light pool.
    lamps: HashMap<(i64, i64), Vec<Vec3>>,
    /// Shared materials by class.
    solid_material: Option<Handle<StandardMaterial>>,
    linear_material: Option<Handle<StandardMaterial>>,
    paint_material: Option<Handle<StandardMaterial>>,
    lamp_material: Option<Handle<StandardMaterial>>,
    signal_materials: Option<[Handle<StandardMaterial>; 6]>,
    /// The pool of live point lights `update_lamp_lights` re-aims.
    light_pool: Vec<Entity>,
    /// Count of spawned mesh entities, for the shot harness.
    pub spawned_meshes: usize,
}

impl Default for StreetDetail {
    fn default() -> Self {
        Self {
            anchor: None,
            terrain_revision: 0,
            wants_was: None,
            cells: CellStream::new(Policy {
                max_in_flight: crate::systems::osm_buildings::overpass_pace().0,
                min_interval_secs: 0.0,
                max_attempts: MAX_ATTEMPTS,
                backoff_secs: BACKOFF_SECS,
                backoff_doublings: BACKOFF_DOUBLINGS,
                cooldown_secs: RETRY_AFTER_SECS,
                despawns_per_frame: MAX_DESPAWNS_PER_FRAME,
            }),
            cell_entities: HashMap::new(),
            lamps: HashMap::new(),
            solid_material: None,
            linear_material: None,
            paint_material: None,
            lamp_material: None,
            signal_materials: None,
            light_pool: Vec::new(),
            spawned_meshes: 0,
        }
    }
}

impl StreetDetail {
    /// Read by the `--shot` settle test alongside the other streamers.
    #[must_use]
    pub fn streaming_busy(&self) -> bool {
        self.cells.busy()
    }

    /// All live lamp positions (world frame).
    fn lamp_iter(&self) -> impl Iterator<Item = &Vec3> {
        self.lamps.values().flatten()
    }
}

/// Stream the street-detail cells around the camera.
#[allow(clippy::too_many_arguments)]
pub fn update_street_detail(
    mut commands: Commands,
    mut sd: ResMut<StreetDetail>,
    mut meshes: ResMut<Assets<Mesh>>,
    mut materials: ResMut<Assets<StandardMaterial>>,
    stream: Res<crate::systems::map_stream::MapStream>,
    time: Res<Time>,
    camera: Query<(&GridCell, &Transform), With<FreeCamera>>,
    bigspace: Query<Entity, With<BigSpace>>,
    globe: Res<crate::systems::globe::GlobeState>,
    mut colliders: ResMut<BuildingColliders>,
    terrain: Res<crate::systems::terrain::TerrainField>,
    layers: Res<crate::systems::data_layers::DataLayers>,
    surfaces: ResMut<crate::systems::map_geometry::MapSurfaces>,
    veg_lod: Res<crate::systems::vegetation::VegetationLod>,
) {
    let surfaces = surfaces.into_inner();
    // Free retired entities at the per-frame cap, before any early return.
    sd.cells.drain_despawns(&mut commands);

    let Some((clat, clng, base_zoom)) = stream.geo_anchor() else {
        return;
    };

    let wants = (layers.street_detail_enabled, layers.trees_enabled);
    let reanchored = sd.cells.sync(stream.anchor_generation);
    if reanchored || sd.wants_was != Some(wants) || sd.terrain_revision != terrain.revision {
        sd.wants_was = Some(wants);
        sd.terrain_revision = terrain.revision;
        let retired: Vec<Entity> = sd.cell_entities.drain().flat_map(|(_, e)| e).collect();
        sd.spawned_meshes = 0;
        sd.cells.retire_all(retired);
        sd.lamps.clear();
        colliders.clear_features();
        sd.cells.reset();
        sd.anchor = None;
    }
    if !wants.0 {
        return;
    }

    if sd.anchor.is_none() {
        let (ax, ay) = lat_lng_to_tile(clat, clng, base_zoom);
        sd.anchor = Some((ax, ay, base_zoom, clat));
    }
    let (ax, ay, z, lat) = sd.anchor.unwrap();
    let tile_m = tile_to_meters(z, lat) as f32;

    let Ok((cell, tf)) = camera.single() else {
        return;
    };
    let cam_altitude = globe.altitude_m(cell, tf) as f32;
    if cam_altitude > crate::systems::globe::GLOBE_SHOW_ALT {
        return;
    }
    let world_x = cell.x as f32 * WORLD_CELL_EDGE + tf.translation.x;
    let world_z = cell.z as f32 * WORLD_CELL_EDGE + tf.translation.z;
    let center = (
        ax + (world_x / tile_m).round() as i64,
        ay + (world_z / tile_m).round() as i64,
    );
    let shift = z.saturating_sub(DETAIL_ZOOM);
    let (ccx, ccy) = (
        center.0.div_euclid(1i64 << shift),
        center.1.div_euclid(1i64 << shift),
    );

    // Queue missing cells, nearest first; frozen at altitude.
    let now_secs = time.elapsed_secs_f64();
    if cam_altitude < FREEZE_ALT {
        let mut to_request: Vec<(i64, i64)> = Vec::new();
        for dy in -CELL_RADIUS..=CELL_RADIUS {
            for dx in -CELL_RADIUS..=CELL_RADIUS {
                to_request.push((ccx + dx, ccy + dy));
            }
        }
        to_request.sort_by_key(|c| (c.0 - ccx).abs().max((c.1 - ccy).abs()));
        for cell in to_request {
            sd.cells.request(cell, now_secs);
        }
    }

    // Dispatch: fetch on the IO pool, parse + mesh on the worker pool.
    let mut jobs = Vec::new();
    while let Some(cell) = sd.cells.next_job(now_secs) {
        jobs.push(cell);
    }
    let sink = sd.cells.sink();
    for (cx, cy) in jobs {
        let results = sink.clone();
        let (north, west) = tile_to_lat_lng(cx, cy, DETAIL_ZOOM);
        let (south, east) = tile_to_lat_lng(cx + 1, cy + 1, DETAIL_ZOOM);
        let ctx = BuildCtx {
            ax,
            ay,
            z,
            tile_m,
            south,
            west,
            north,
            east,
            ground: terrain.snapshot(),
            wants_trees: wants.1,
        };
        IoTaskPool::get()
            .spawn(async move {
                match fetch_cell_text(cx, cy).await {
                    Ok(text) => {
                        crate::platform::workers::submit(
                            "street_detail",
                            Box::new(move || {
                                let built = build_cell(&text, cx, cy, &ctx);
                                if let Ok(mut g) = results.lock() {
                                    g.push(((cx, cy), built));
                                }
                            }),
                        );
                    }
                    Err(e) => {
                        if let Ok(mut g) = results.lock() {
                            g.push(((cx, cy), Err(e)));
                        }
                    }
                }
            })
            .detach();
    }

    let Ok(root) = bigspace.single() else {
        return;
    };

    // Apply completed cells: item-capped and byte-bounded, overflow deferred.
    let mut done = sd.cells.take_results();
    let cap = crate::quality::budget().per_frame(MAX_BUILDS_PER_FRAME);
    let mut take = 0usize;
    for (_, built) in &done {
        if take >= cap {
            break;
        }
        let cost = match built {
            Ok(Some(b)) => b
                .meshes
                .iter()
                .map(|(_, m)| crate::systems::stream_ledger::mesh_cost(m))
                .sum(),
            _ => 0,
        };
        if !crate::systems::stream_ledger::admit(cost) {
            break;
        }
        take += 1;
    }
    let deferred = done.split_off(take);
    sd.cells.defer(deferred);

    for (cell, built) in done {
        match built {
            Ok(Some(build)) => {
                let entities = spawn_cell(
                    &mut commands,
                    &mut sd,
                    &mut meshes,
                    &mut materials,
                    surfaces,
                    &veg_lod,
                    root,
                    &build,
                );
                sd.spawned_meshes += entities.len();
                sd.cell_entities.insert(cell, entities);
                if !build.colliders.is_empty() {
                    colliders.set_feature_cell(cell, build.colliders);
                }
                if !build.lamp_points.is_empty() {
                    sd.lamps.insert(cell, build.lamp_points);
                }
                sd.cells.succeeded(&cell);
            }
            Ok(None) => sd.cells.succeeded(&cell),
            Err(e) => {
                let attempts = sd.cells.failed(&cell, now_secs);
                if sd.cells.exhausted(&cell) {
                    tracing::warn!(?cell, attempts, error = %e, "street-detail cell gave up");
                }
            }
        }
    }

    // Retire cells out of range (with hysteresis).
    let retired: Vec<(i64, i64)> = sd
        .cell_entities
        .keys()
        .filter(|c| (c.0 - ccx).abs().max((c.1 - ccy).abs()) > KEEP_RADIUS)
        .copied()
        .collect();
    for cell in retired {
        if let Some(entities) = sd.cell_entities.remove(&cell) {
            sd.spawned_meshes = sd.spawned_meshes.saturating_sub(entities.len());
            sd.cells.retire_all(entities);
        }
        sd.lamps.remove(&cell);
        colliders.remove_feature_cell(cell);
        sd.cells.forget(&cell);
    }
}

/// Spawn one built cell's meshes; returns the entities for later retirement.
#[allow(clippy::too_many_arguments)]
fn spawn_cell(
    commands: &mut Commands,
    sd: &mut StreetDetail,
    meshes: &mut Assets<Mesh>,
    materials: &mut Assets<StandardMaterial>,
    surfaces: &mut crate::systems::map_geometry::MapSurfaces,
    veg_lod: &crate::systems::vegetation::VegetationLod,
    root: Entity,
    build: &DetailCellBuild,
) -> Vec<Entity> {
    let mut entities = Vec::with_capacity(build.meshes.len());
    for (class, mesh) in &build.meshes {
        let placement = (
            Mesh3d(meshes.add(mesh.clone())),
            Transform::default(),
            build.grid_cell,
            ChildOf(root),
            crate::components::HideInSpace,
        );
        let id = match class {
            PropClass::Canopy | PropClass::Trunk => {
                let group = if *class == PropClass::Canopy {
                    SurfaceGroup::Canopy
                } else {
                    SurfaceGroup::Trunk
                };
                // The shared vegetation material + marker: the altitude fade and
                // the Trees toggle then treat surveyed and scattered trees alike.
                let material = surfaces.group_material(group, materials);
                commands
                    .spawn((
                        placement,
                        MeshMaterial3d(material),
                        VegetationSurface,
                        veg_lod.visibility(),
                    ))
                    .id()
            }
            PropClass::Solid => {
                let material = sd
                    .solid_material
                    .get_or_insert_with(|| {
                        materials.add(StandardMaterial {
                            base_color: Color::WHITE,
                            perceptual_roughness: 0.85,
                            ..default()
                        })
                    })
                    .clone();
                commands
                    .spawn((placement, MeshMaterial3d(material), PropNear))
                    .id()
            }
            PropClass::Linear => {
                let material = sd
                    .linear_material
                    .get_or_insert_with(|| {
                        materials.add(StandardMaterial {
                            base_color: Color::WHITE,
                            perceptual_roughness: 0.9,
                            ..default()
                        })
                    })
                    .clone();
                commands
                    .spawn((placement, MeshMaterial3d(material), PropFar))
                    .id()
            }
            PropClass::Paint => {
                let material = sd
                    .paint_material
                    .get_or_insert_with(|| {
                        materials.add(StandardMaterial {
                            base_color: Color::srgb(0.93, 0.93, 0.90),
                            unlit: true,
                            depth_bias: PAINT_RUNG * crate::systems::vector_tiles::DEPTH_BIAS_STEP,
                            cull_mode: None,
                            ..default()
                        })
                    })
                    .clone();
                commands
                    .spawn((
                        placement,
                        MeshMaterial3d(material),
                        PropNear,
                        bevy::light::NotShadowCaster,
                    ))
                    .id()
            }
            PropClass::LampHead => {
                let material = sd
                    .lamp_material
                    .get_or_insert_with(|| {
                        materials.add(StandardMaterial {
                            base_color: Color::srgb(0.85, 0.82, 0.72),
                            emissive: LinearRgba::BLACK,
                            perceptual_roughness: 0.4,
                            ..default()
                        })
                    })
                    .clone();
                commands
                    .spawn((
                        placement,
                        MeshMaterial3d(material),
                        PropFar,
                        bevy::light::NotShadowCaster,
                    ))
                    .id()
            }
            PropClass::Signal(i) => {
                let mats = sd
                    .signal_materials
                    .get_or_insert_with(|| {
                        std::array::from_fn(|_| {
                            materials.add(StandardMaterial {
                                base_color: Color::srgb(0.10, 0.10, 0.10),
                                emissive: LinearRgba::BLACK,
                                perceptual_roughness: 0.3,
                                ..default()
                            })
                        })
                    })
                    .clone();
                commands
                    .spawn((
                        placement,
                        MeshMaterial3d(mats[*i as usize].clone()),
                        PropNear,
                        bevy::light::NotShadowCaster,
                    ))
                    .id()
            }
        };
        entities.push(id);
    }
    entities
}

// ─── Night: lamp glow + the light pool ───────────────────────────────────────

/// Warm sodium-ish lamp colour.
const LAMP_COLOR: Color = Color::srgb(1.0, 0.85, 0.6);
/// Lumens of one street lamp at full night.
const LAMP_LUMENS: f32 = 6_000.0;
/// Light pool radius around the camera.
const LAMP_LIGHT_RANGE_M: f32 = 160.0;

/// How many real `PointLight`s the lamp layer may keep alive. These are the
/// first clustered lights in the scene, and on the web CPU clustering is the
/// single most expensive thing in the frame — the pool is a structural cap,
/// not a hope.
fn lamp_pool_size() -> usize {
    if crate::quality::is_phone() {
        0
    } else if crate::quality::budget().tier == crate::quality::Tier::Desktop {
        48
    } else {
        16
    }
}

/// The same night curve the building windows use.
fn night_glow(dev: &crate::resources::DevPanelState) -> f32 {
    let elevation = crate::utils::sun::sun_direction(dev.time_of_day, dev.sun_declination_rad).y;
    ((0.18 - elevation) / 0.33).clamp(0.0, 1.0)
}

/// Drive the lamp heads' emissive with the night factor.
pub fn update_lamp_glow(
    dev: Res<crate::resources::DevPanelState>,
    sd: Res<StreetDetail>,
    mut materials: ResMut<Assets<StandardMaterial>>,
    mut applied: Local<Option<f32>>,
) {
    let Some(handle) = sd.lamp_material.as_ref() else {
        return;
    };
    let glow = night_glow(&dev);
    if applied.is_some_and(|a| (a - glow).abs() < 0.01) {
        return;
    }
    *applied = Some(glow);
    if let Some(mut mat) = materials.get_mut(handle) {
        let c = LAMP_COLOR.to_linear();
        mat.emissive = LinearRgba::rgb(
            c.red * 14.0 * glow,
            c.green * 14.0 * glow,
            c.blue * 14.0 * glow,
        );
    }
}

/// Re-aim the point-light pool at the lamps nearest the camera, at night.
///
/// The pool is spawned once, lazily, and only ever moved — the number of live
/// clustered lights is a constant the tier chooses, however many lamps stream
/// in. Throttled: re-aiming is cheap but not free, and lamps do not move.
pub fn update_lamp_lights(
    mut commands: Commands,
    mut sd: ResMut<StreetDetail>,
    dev: Res<crate::resources::DevPanelState>,
    camera: Query<(&GridCell, &Transform), With<FreeCamera>>,
    bigspace: Query<Entity, With<BigSpace>>,
    mut lights: Query<
        (
            &mut Transform,
            &mut GridCell,
            &mut PointLight,
            &mut Visibility,
        ),
        Without<FreeCamera>,
    >,
    time: Res<Time>,
    mut last_run: Local<f32>,
) {
    let pool = lamp_pool_size();
    if pool == 0 {
        return;
    }
    let now = time.elapsed_secs();
    if now - *last_run < 0.5 {
        return;
    }
    *last_run = now;

    let Ok((cam_cell, cam_tf)) = camera.single() else {
        return;
    };
    let Ok(root) = bigspace.single() else {
        return;
    };
    let glow = night_glow(&dev);

    // Spawn the pool on first use.
    if sd.light_pool.is_empty() {
        for _ in 0..pool {
            let id = commands
                .spawn((
                    PointLight {
                        color: LAMP_COLOR,
                        intensity: 0.0,
                        range: 26.0,
                        radius: 0.15,
                        shadow_maps_enabled: false,
                        ..default()
                    },
                    Transform::default(),
                    GridCell::default(),
                    ChildOf(root),
                    Visibility::Hidden,
                ))
                .id();
            sd.light_pool.push(id);
        }
    }

    let cam_world = Vec2::new(
        cam_cell.x as f32 * WORLD_CELL_EDGE + cam_tf.translation.x,
        cam_cell.z as f32 * WORLD_CELL_EDGE + cam_tf.translation.z,
    );

    // Nearest lamps within range — a partial sort over the live set.
    let mut near: Vec<(f32, Vec3)> = sd
        .lamp_iter()
        .filter_map(|p| {
            let d2 = Vec2::new(p.x, p.z).distance_squared(cam_world);
            (d2 < LAMP_LIGHT_RANGE_M * LAMP_LIGHT_RANGE_M).then_some((d2, *p))
        })
        .collect();
    near.sort_by(|a, b| a.0.total_cmp(&b.0));
    near.truncate(pool);

    let grid = Grid::new(WORLD_CELL_EDGE, WORLD_SWITCH_THRESHOLD);
    let lit = if glow > 0.05 { near.len() } else { 0 };
    for (i, entity) in sd.light_pool.clone().into_iter().enumerate() {
        let Ok((mut tf, mut cell, mut light, mut vis)) = lights.get_mut(entity) else {
            continue;
        };
        if i < lit {
            let p = near[i].1;
            // Just under the head, so the pool of light falls on the street.
            let (c, local) = grid.translation_to_grid(Vec3::new(p.x, p.y - 0.35, p.z).as_dvec3());
            *cell = c;
            tf.translation = local;
            light.intensity = LAMP_LUMENS * glow;
            *vis = Visibility::Inherited;
        } else if *vis != Visibility::Hidden {
            light.intensity = 0.0;
            *vis = Visibility::Hidden;
        }
    }
}

// ─── Signals: the phase cycle ────────────────────────────────────────────────

/// Full cycle length in seconds: each group gets green, amber, then all-red
/// clearance while the other runs.
const SIGNAL_CYCLE_S: f32 = 24.0;

/// Which lamp of a group is lit at `t` seconds into the cycle: the group's half
/// is green 0–9 s, amber 9–12 s, red for the rest (the other group mirrors at
/// half-cycle offset).
fn signal_phase(t: f32, group: u8) -> u8 {
    let half = SIGNAL_CYCLE_S / 2.0;
    let local = (t + f32::from(group) * half) % SIGNAL_CYCLE_S;
    if local < 9.0 {
        2 // green
    } else if local < 12.0 {
        1 // amber
    } else {
        0 // red
    }
}

/// Light the six signal materials by the global cycle clock. Writes only on a
/// phase transition — six material writes a few times a minute.
pub fn update_signal_cycle(
    sd: Res<StreetDetail>,
    time: Res<Time>,
    mut materials: ResMut<Assets<StandardMaterial>>,
    mut applied: Local<Option<(u8, u8)>>,
) {
    let Some(mats) = sd.signal_materials.as_ref() else {
        return;
    };
    let t = time.elapsed_secs() % SIGNAL_CYCLE_S;
    let state = (signal_phase(t, 0), signal_phase(t, 1));
    if *applied == Some(state) {
        return;
    }
    *applied = Some(state);
    const COLORS: [LinearRgba; 3] = [
        LinearRgba::rgb(8.0, 0.6, 0.4),
        LinearRgba::rgb(8.0, 4.5, 0.6),
        LinearRgba::rgb(0.8, 8.0, 1.2),
    ];
    for group in 0..2u8 {
        let lit = if group == 0 { state.0 } else { state.1 };
        for colour in 0..3u8 {
            let idx = (group * 3 + colour) as usize;
            if let Some(mut mat) = materials.get_mut(&mats[idx]) {
                mat.emissive = if colour == lit {
                    COLORS[colour as usize]
                } else {
                    LinearRgba::BLACK
                };
            }
        }
    }
}

// ─── Altitude fade ───────────────────────────────────────────────────────────

/// Fade the street props with altitude: the near band collapses the small
/// props' contrast toward the paving and hides them past its end (the same
/// treatment the tile street furniture gets), the far band does the same for
/// walls and piers on the trees' longer band.
pub fn update_prop_fade(
    sd: Res<StreetDetail>,
    globe: Res<crate::systems::globe::GlobeState>,
    camera: Query<(&GridCell, &Transform), With<FreeCamera>>,
    mut materials: ResMut<Assets<StandardMaterial>>,
    mut near_q: Query<&mut Visibility, (With<PropNear>, Without<PropFar>)>,
    mut far_q: Query<&mut Visibility, (With<PropFar>, Without<PropNear>)>,
    mut last: Local<Option<(f32, f32)>>,
) {
    let Ok((cell, tf)) = camera.single() else {
        return;
    };
    let alt = globe.altitude_m(cell, tf) as f32;
    let band = |start: f32, end: f32| {
        let t = ((alt - start) / (end - start)).clamp(0.0, 1.0);
        t * t * (3.0 - 2.0 * t)
    };
    let (near_f, far_f) = (
        band(NEAR_FADE_START, NEAR_FADE_END),
        band(FAR_FADE_START, FAR_FADE_END),
    );
    let previous = *last;
    if previous.is_some_and(|(n, f)| (n - near_f).abs() < 0.002 && (f - far_f).abs() < 0.002) {
        return;
    }
    *last = Some((near_f, far_f));

    // The vertex colours are absolute; walking the shared base toward a paving
    // grey collapses everything's contrast against the street from above.
    let walk =
        |handle: &Option<Handle<StandardMaterial>>, f: f32, mats: &mut Assets<StandardMaterial>| {
            if let Some(h) = handle {
                if let Some(mut m) = mats.get_mut(h) {
                    let g = 1.0 - 0.35 * f;
                    m.base_color = Color::linear_rgb(g, g, g);
                }
            }
        };
    walk(&sd.solid_material, near_f, &mut materials);
    walk(&sd.linear_material, far_f, &mut materials);

    let flip = |hidden: bool, was: Option<bool>| match was {
        Some(w) if w == hidden => None,
        _ => Some(if hidden {
            Visibility::Hidden
        } else {
            Visibility::Inherited
        }),
    };
    if let Some(v) = flip(near_f >= 1.0, previous.map(|(n, _)| n >= 1.0)) {
        for mut vis in &mut near_q {
            *vis = v;
        }
    }
    if let Some(v) = flip(far_f >= 1.0, previous.map(|(_, f)| f >= 1.0)) {
        for mut vis in &mut far_q {
            *vis = v;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ctx() -> BuildCtx {
        BuildCtx {
            ax: 8600,
            ay: 5500,
            z: 14,
            tile_m: 1500.0,
            south: -90.0,
            west: -180.0,
            north: 90.0,
            east: 180.0,
            ground: TerrainSnapshot::default(),
            wants_trees: true,
        }
    }

    #[test]
    fn parses_the_combined_response() {
        let text = r#"{"elements":[
            {"type":"node","id":1,"lat":53.21,"lon":6.56,"tags":{"natural":"tree","height":"12"}},
            {"type":"node","id":2,"lat":53.211,"lon":6.561,"tags":{"highway":"street_lamp"}},
            {"type":"node","id":3,"lat":53.212,"lon":6.562,"tags":{"highway":"traffic_signals"}},
            {"type":"node","id":4,"lat":53.213,"lon":6.563,"tags":{"highway":"crossing","crossing":"zebra"}},
            {"type":"node","id":5,"lat":53.214,"lon":6.564,"tags":{"barrier":"bollard"}},
            {"type":"way","id":10,"nodes":[3,4,99],
             "geometry":[{"lat":53.212,"lon":6.562},{"lat":53.213,"lon":6.563},{"lat":53.214,"lon":6.565}],
             "tags":{"highway":"residential","lanes":"2"}},
            {"type":"way","id":11,
             "geometry":[{"lat":53.215,"lon":6.565},{"lat":53.216,"lon":6.566}],
             "tags":{"barrier":"hedge","height":"2"}},
            {"type":"way","id":12,
             "geometry":[{"lat":53.217,"lon":6.567},{"lat":53.218,"lon":6.568}],
             "tags":{"man_made":"pier"}}
        ]}"#;
        let data = parse_detail(text).unwrap();
        assert_eq!(data.trees.len(), 1);
        assert_eq!(data.trees[0].2, Some(12.0));
        assert_eq!(data.lamps.len(), 1);
        assert_eq!(data.signals, vec![(3, 53.212, 6.562)]);
        assert_eq!(data.crossings, vec![(4, 53.213, 6.563)]);
        assert_eq!(data.bollards.len(), 1);
        assert_eq!(data.barriers.len(), 1);
        assert_eq!(data.barriers[0].0, BarrierKind::Hedge);
        assert_eq!(data.barriers[0].1, Some(2.0));
        assert_eq!(data.piers.len(), 1);
        assert_eq!(data.roads.len(), 1);
        assert_eq!(data.roads[0].2, 6.5); // two lanes at 3.25 m
    }

    /// A buried feature is not drawn on the ground above it — the refusal
    /// `drawn_street` has always made for the carriageway, made here for the
    /// furniture standing on it.
    ///
    /// The signal and the zebra are the interesting half: neither node carries a
    /// grade of its own, so the only thing that condemns them is the tunnelled
    /// way they belong to, and that way is listed *after* them on purpose —
    /// Overpass does not order its response for our convenience, and a filter
    /// that only looked backwards would pass them both.
    #[test]
    fn nothing_underground_reaches_the_surface() {
        let text = r#"{"elements":[
            {"type":"node","id":1,"lat":1.0,"lon":1.0,"tags":{"highway":"traffic_signals"}},
            {"type":"node","id":2,"lat":1.0,"lon":1.1,"tags":{"highway":"crossing","crossing":"zebra"}},
            {"type":"node","id":3,"lat":1.0,"lon":1.2,"tags":{"highway":"street_lamp","layer":"-1"}},
            {"type":"node","id":4,"lat":1.0,"lon":1.3,"tags":{"barrier":"bollard"}},
            {"type":"way","id":10,"nodes":[1,2],
             "geometry":[{"lat":1.0,"lon":1.0},{"lat":1.0,"lon":1.1}],
             "tags":{"highway":"residential","tunnel":"yes"}},
            {"type":"way","id":11,
             "geometry":[{"lat":2.0,"lon":2.0},{"lat":2.0,"lon":2.1}],
             "tags":{"barrier":"fence","covered":"yes"}},
            {"type":"way","id":12,
             "geometry":[{"lat":3.0,"lon":3.0},{"lat":3.0,"lon":3.1}],
             "tags":{"barrier":"wall"}}
        ]}"#;
        let data = parse_detail(text).unwrap();
        // Both nodes stood on the tunnelled way, which arrived after them.
        assert!(data.signals.is_empty(), "a signal in a tunnel was drawn");
        assert!(data.crossings.is_empty(), "a zebra in a tunnel was drawn");
        // Explicitly stacked below ground, with no way to inherit it from.
        assert!(data.lamps.is_empty(), "a lamp at layer=-1 was drawn");
        // The tunnelled carriageway is not context for anything anymore.
        assert!(data.roads.is_empty());
        // A covered way is refused for the same reason a bored one is; the
        // ordinary wall beside it is untouched.
        assert_eq!(data.barriers.len(), 1);
        assert_eq!(data.barriers[0].0, BarrierKind::Wall);
        // Nothing above ground was harmed.
        assert_eq!(data.bollards.len(), 1);
    }

    #[test]
    fn a_remark_is_an_error_not_an_empty_cell() {
        let text = r#"{"remark":"runtime error: timeout","elements":[]}"#;
        assert!(parse_detail(text).is_err());
    }

    #[test]
    fn an_uncontrolled_crossing_needs_the_zebra_ref() {
        let text = r#"{"elements":[
            {"type":"node","id":1,"lat":1.0,"lon":1.0,"tags":{"highway":"crossing","crossing":"uncontrolled"}},
            {"type":"node","id":2,"lat":1.0,"lon":1.1,"tags":{"highway":"crossing","crossing":"uncontrolled","crossing_ref":"zebra"}}
        ]}"#;
        let data = parse_detail(text).unwrap();
        assert_eq!(data.crossings.len(), 1);
        assert_eq!(data.crossings[0].0, 2);
    }

    #[test]
    fn builds_meshes_for_a_populated_cell() {
        let text = r#"{"elements":[
            {"type":"node","id":1,"lat":53.21,"lon":6.56,"tags":{"natural":"tree"}},
            {"type":"node","id":2,"lat":53.211,"lon":6.561,"tags":{"highway":"street_lamp"}},
            {"type":"way","id":11,
             "geometry":[{"lat":53.215,"lon":6.565},{"lat":53.216,"lon":6.566}],
             "tags":{"barrier":"wall"}}
        ]}"#;
        let (cx, cy) = lat_lng_to_tile(53.21, 6.56, DETAIL_ZOOM);
        let build = build_cell(text, cx, cy, &ctx()).unwrap().unwrap();
        let classes: Vec<PropClass> = build.meshes.iter().map(|(c, _)| *c).collect();
        assert!(classes.contains(&PropClass::Canopy), "{classes:?}");
        assert!(classes.contains(&PropClass::Solid), "{classes:?}");
        assert!(classes.contains(&PropClass::LampHead), "{classes:?}");
        assert!(classes.contains(&PropClass::Linear), "{classes:?}");
        assert_eq!(build.lamp_points.len(), 1);
        // The wall blocks walking.
        assert!(!build.colliders.is_empty());
    }

    #[test]
    fn trees_can_be_toggled_out_of_the_build() {
        let text = r#"{"elements":[
            {"type":"node","id":1,"lat":53.21,"lon":6.56,"tags":{"natural":"tree"}}
        ]}"#;
        let (cx, cy) = lat_lng_to_tile(53.21, 6.56, DETAIL_ZOOM);
        let mut c = ctx();
        c.wants_trees = false;
        assert!(build_cell(text, cx, cy, &c).unwrap().is_none());
    }

    #[test]
    fn zebra_stripes_span_the_road() {
        // A zebra on a straight east-west road: stripes must extend along the
        // walking (north-south) axis by the road's half width.
        let text = r#"{"elements":[
            {"type":"node","id":4,"lat":53.2130,"lon":6.5630,"tags":{"highway":"crossing","crossing":"zebra"}},
            {"type":"way","id":10,"nodes":[8,4,9],
             "geometry":[{"lat":53.2130,"lon":6.5620},{"lat":53.2130,"lon":6.5630},{"lat":53.2130,"lon":6.5640}],
             "tags":{"highway":"residential","width":"6"}}
        ]}"#;
        let (cx, cy) = lat_lng_to_tile(53.213, 6.563, DETAIL_ZOOM);
        let build = build_cell(text, cx, cy, &ctx()).unwrap().unwrap();
        let paint = build
            .meshes
            .iter()
            .find(|(c, _)| *c == PropClass::Paint)
            .expect("zebra paint mesh");
        let positions = paint
            .1
            .attribute(Mesh::ATTRIBUTE_POSITION)
            .unwrap()
            .as_float3()
            .unwrap();
        // 5 stripes × 4 corners.
        assert_eq!(positions.len(), 20);
        // The road runs east–west (local +x), so stripes span north–south (z):
        // z-extent ≈ road width − margin, x-extent ≈ stripe pitch span.
        let (mut zmin, mut zmax) = (f32::MAX, f32::MIN);
        for p in positions {
            zmin = zmin.min(p[2]);
            zmax = zmax.max(p[2]);
        }
        assert!((zmax - zmin) > 4.0, "stripes too short: {}", zmax - zmin);
    }

    #[test]
    fn signal_groups_alternate() {
        // At any time, the two groups never show green together.
        for t in 0..24 {
            let a = signal_phase(t as f32, 0);
            let b = signal_phase(t as f32, 1);
            assert!(!(a == 2 && b == 2), "both green at t={t}");
        }
    }

    #[test]
    fn guard_rails_do_not_block_walking() {
        let text = r#"{"elements":[
            {"type":"way","id":11,
             "geometry":[{"lat":53.215,"lon":6.565},{"lat":53.216,"lon":6.566}],
             "tags":{"barrier":"guard_rail"}}
        ]}"#;
        let (cx, cy) = lat_lng_to_tile(53.215, 6.565, DETAIL_ZOOM);
        let build = build_cell(text, cx, cy, &ctx()).unwrap().unwrap();
        assert!(build.colliders.is_empty());
        assert!(build.meshes.iter().any(|(c, _)| *c == PropClass::Linear));
    }

    #[test]
    fn cell_ownership_is_half_open() {
        // A node exactly on the shared north edge belongs to one cell only.
        let (cx, cy) = lat_lng_to_tile(53.21, 6.56, DETAIL_ZOOM);
        let (north, west) = tile_to_lat_lng(cx, cy, DETAIL_ZOOM);
        let mut c = ctx();
        let (s, w2) = tile_to_lat_lng(cx + 1, cy + 1, DETAIL_ZOOM);
        c.south = s;
        c.west = west;
        c.north = north;
        c.east = w2;
        assert!(!c.owns(north, west + 0.001));
        assert!(c.owns(north - 1e-9, west + 0.001));
    }
}
