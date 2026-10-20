//! The tall-structure lane: wind turbines, historic windmills, masts, towers,
//! chimneys, water towers and construction cranes, streamed from Overpass at a
//! *coarse* zoom over a wide ring.
//!
//! These are the landmarks that make a horizon recognisable, and their whole
//! value is being visible from kilometres away — which is exactly what the
//! street-detail ring (±2 km) cannot give them. So they get their own
//! `CellStream` at z11 (a cell ≈ 12 km at NL latitudes, radius 2 ≈ ±30 km),
//! fetched with `out center` so a turbine mapped as an area way still costs a
//! single coordinate. Payloads are tiny; the ring is cheap.
//!
//! **Where atlas has pre-cut a cell it is a static file, not a query.** Each cell
//! used to be a live Overpass query costing 0.7–2.6 s of server time, paced four
//! in flight, and on a cold start at 600 m over Groningen that made this the last
//! lane to finish — 24.3 s, ~8 s after everything else (Steam Deck, 2026-09-26).
//! Atlas's `tall` dataset now writes the *same* query's answer
//! (`cartopolis_geo::tall::query`, shared with it) to
//! `{tdbag_url}/tall/11/{x}/{y}.json` and lists the cells in
//! `tall/coverage.json`. A covered cell is fetched from there; an uncovered one,
//! a host without the dataset, or a failed fetch goes to Overpass as before. The
//! cache key and the parse are the same for both, so nothing past the fetch
//! knows which answered. Dispatch waits for the manifest's answer (a 404 is one,
//! in ~70 ms) up to [`COVERAGE_WAIT_SECS`], or a cold start would send the whole
//! ring to Overpass in the moment before the manifest landed.
//!
//! Static geometry merges per cell into one vertex-coloured mesh on the shared
//! street-detail `Solid`-style material recipe. Each wind turbine and windmill
//! additionally spawns a **rotor entity** ([`TurbineRotor`]) whose transform
//! `spin_rotors` advances — the one thing here that moves. Rotor speed rides
//! the live wind (`WeatherState`), yaw faces the prevailing south-westerly.

use std::collections::HashMap;

use bevy::prelude::*;
use bevy::tasks::IoTaskPool;
use big_space::prelude::{BigSpace, Grid, GridCell};

use crate::grid::{AnchoredCamera, WORLD_CELL_EDGE, WORLD_SWITCH_THRESHOLD};
use crate::platform::stream::{CellStream, Policy};
use crate::platform::{http, storage};
use crate::systems::cell_ring;
use crate::systems::coverage::CoverageSlot;
use crate::systems::osm_buildings::{
    lat_lng_to_local, num_from_value, overpass_query, overpass_remark, tdbag_url, MeshData,
};
use crate::systems::street_detail::{feature_seed, linear, push_yawed_box, shade, srand};
use crate::systems::terrain::TerrainSnapshot;
use cartopolis_core::world::{lat_lng_to_tile, tile_to_lat_lng, tile_to_meters};
use cartopolis_geo::tall;

/// Coarse fetch/cache zoom — a z11 cell is ~12 km across at NL latitudes.
const TALL_ZOOM: u32 = tall::ZOOM;
/// Cells out from the camera (2 → 5×5 ≈ ±30 km). Atlas grows its area by the
/// same constant, which is why it is shared.
const CELL_RADIUS: i64 = tall::RING;
const KEEP_RADIUS: i64 = CELL_RADIUS + 1;
/// How long dispatch waits for the atlas manifest before it asks Overpass for
/// everything. Ours answers either way in well under a second; this only bounds
/// the case of a host that cannot be reached at all.
const COVERAGE_WAIT_SECS: f64 = 3.0;

/// The atlas host's tall-structure cells (`crates/atlas/src/pipeline/tall.rs`).
static COVERAGE: CoverageSlot = CoverageSlot::new("tall", "/tall/coverage.json", TALL_ZOOM);

const MAX_ATTEMPTS: u32 = 3;
const BACKOFF_SECS: f64 = 2.0;
const BACKOFF_DOUBLINGS: u32 = 2;
const RETRY_AFTER_SECS: f64 = 60.0;
const MAX_DESPAWNS_PER_FRAME: usize = 8;
const MAX_BUILDS_PER_FRAME: usize = 2;
/// A cell of the Veluwe has a handful of these; a cell of Rotterdam harbour has
/// hundreds of cranes and masts. Cap against the harbour.
const MAX_STRUCTURES_PER_CELL: usize = 400;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum StructureKind {
    WindTurbine,
    Windmill,
    Mast,
    Tower,
    Chimney,
    WaterTower,
    Crane,
}

impl StructureKind {
    fn parse(man_made: Option<&str>, is_wind_generator: bool) -> Option<Self> {
        if is_wind_generator {
            return Some(Self::WindTurbine);
        }
        Some(match man_made? {
            "windmill" => Self::Windmill,
            "mast" => Self::Mast,
            "tower" | "communications_tower" => Self::Tower,
            "chimney" => Self::Chimney,
            "water_tower" => Self::WaterTower,
            "crane" => Self::Crane,
            _ => return None,
        })
    }

    /// Default overall height in metres, when untagged.
    fn default_height(self) -> f32 {
        match self {
            Self::WindTurbine => 85.0, // hub; the rotor adds its radius on top
            Self::Windmill => 14.0,
            Self::Mast => 45.0,
            Self::Tower => 30.0,
            Self::Chimney => 40.0,
            Self::WaterTower => 35.0,
            Self::Crane => 35.0,
        }
    }
}

/// One parsed structure, still in (lat, lng).
#[derive(Debug)]
struct Structure {
    kind: StructureKind,
    lat: f64,
    lng: f64,
    height: Option<f32>,
    /// `rotor:diameter`, turbines only.
    rotor_d: Option<f32>,
}

fn cache_key(cx: i64, cy: i64) -> String {
    format!("features/tall/v1/{}/{}/{}.json", TALL_ZOOM, cx, cy)
}

/// Where one cell's answer comes from.
#[derive(Debug, PartialEq, Eq)]
enum CellSource {
    /// The atlas host's pre-cut file, at this URL.
    Atlas(String),
    /// A live query (`cartopolis_geo::tall::query`).
    Overpass,
}

/// A covered cell on a configured host is a file; anything else is a query.
fn cell_source(covered: bool, host: &str, cx: i64, cy: i64) -> CellSource {
    if covered && !host.is_empty() {
        CellSource::Atlas(format!("{host}/tall/{TALL_ZOOM}/{cx}/{cy}.json"))
    } else {
        CellSource::Overpass
    }
}

/// The pre-cut file, if it is there and looks like a document rather than an
/// error page — anything else is reported so the caller can ask Overpass.
async fn fetch_atlas_text(url: String) -> Result<String, String> {
    let bytes = http::fetch(http::Request::get(url)).await?;
    let text = String::from_utf8(bytes).map_err(|e| e.to_string())?;
    if !text.trim_start().starts_with('{') {
        return Err("not a JSON document".into());
    }
    Ok(text)
}

async fn fetch_cell_text(cx: i64, cy: i64) -> Result<String, String> {
    let key = cache_key(cx, cy);
    if let Some(cached) = storage::get_string(storage::Namespace::Cache, &key).await {
        // A refusal page cached before `overpass_remark` recognised one is asked
        // for again: parsed, it failed every retry from the cache without a
        // request, and the cell gave up for the session and every one after.
        match overpass_remark(&cached) {
            None => return Ok(cached),
            Some(remark) => {
                tracing::warn!(key, %remark, "re-fetching a cached Overpass refusal");
                storage::remove(storage::Namespace::Cache, &key).await;
            }
        }
    }
    let pre_cut = match cell_source(COVERAGE.covers(cx, cy), &tdbag_url(), cx, cy) {
        CellSource::Atlas(url) => match fetch_atlas_text(url.clone()).await {
            Ok(text) => Some(text),
            Err(e) => {
                tracing::debug!(%url, error = %e, "pre-cut tall cell unavailable; asking Overpass");
                None
            }
        },
        CellSource::Overpass => None,
    };
    let fetched = match pre_cut {
        Some(text) => text,
        None => overpass_query(&tall::query(cx, cy)).await?,
    };
    storage::put(storage::Namespace::Cache, &key, fetched.as_bytes()).await;
    Ok(fetched)
}

fn tag_str<'a>(tags: Option<&'a serde_json::Value>, key: &str) -> Option<&'a str> {
    tags?.get(key)?.as_str()
}

fn tag_num(tags: Option<&serde_json::Value>, key: &str) -> Option<f32> {
    tags?.get(key).and_then(num_from_value)
}

/// Parse an `out center` response: nodes carry `lat`/`lon`, ways a `center`.
fn parse_structures(text: &str) -> Result<Vec<Structure>, String> {
    let json: serde_json::Value = serde_json::from_str(text).map_err(|e| e.to_string())?;
    if let Some(remark) = json.get("remark").and_then(|r| r.as_str()) {
        return Err(format!("Overpass remark: {}", remark.trim()));
    }
    let elements = json
        .get("elements")
        .and_then(|e| e.as_array())
        .ok_or_else(|| "no elements in Overpass response".to_string())?;
    let mut out = Vec::new();
    for el in elements {
        let tags = el.get("tags");
        let center = el.get("center");
        let lat = el
            .get("lat")
            .or_else(|| center?.get("lat"))
            .and_then(|v| v.as_f64());
        let lng = el
            .get("lon")
            .or_else(|| center?.get("lon"))
            .and_then(|v| v.as_f64());
        let (Some(lat), Some(lng)) = (lat, lng) else {
            continue;
        };
        let wind = tag_str(tags, "power") == Some("generator")
            && tag_str(tags, "generator:source") == Some("wind");
        let Some(kind) = StructureKind::parse(tag_str(tags, "man_made"), wind) else {
            continue;
        };
        // A turbine's `height` is usually the tip; prefer the hub when tagged.
        let height = tag_num(tags, "height:hub").or_else(|| tag_num(tags, "height"));
        out.push(Structure {
            kind,
            lat,
            lng,
            height,
            rotor_d: tag_num(tags, "rotor:diameter"),
        });
    }
    Ok(out)
}

// ─── Geometry ────────────────────────────────────────────────────────────────

const TOWER_WHITE: u32 = 0xDC_DC_D8;
const BLADE_WHITE: u32 = 0xE8_E8_E4;
const BRICK: u32 = 0x8A_5A_48;
const CONCRETE: u32 = 0xB5_B2_AC;
const STEEL: u32 = 0xA8_A8_A4;
const CRANE_YELLOW: u32 = 0xD9_A1_3B;
const SAIL_WOOD: u32 = 0x9C_8A_72;

/// A vertical tapered tube: an n-gon prism from `y0` to `y0 + h`.
#[allow(clippy::too_many_arguments)]
fn push_tube(
    buf: &mut MeshData,
    at: Vec2,
    y0: f32,
    r_bottom: f32,
    r_top: f32,
    h: f32,
    sides: u32,
    color: [f32; 4],
) {
    let n = sides.max(5);
    for i in 0..n {
        let a0 = (i as f32 / n as f32) * std::f32::consts::TAU;
        let a1 = ((i + 1) as f32 / n as f32) * std::f32::consts::TAU;
        let (s0, c0) = a0.sin_cos();
        let (s1, c1) = a1.sin_cos();
        let out = Vec3::new((c0 + c1) * 0.5, 0.0, (s0 + s1) * 0.5);
        buf.quad_facing(
            Vec3::new(at.x + c0 * r_bottom, y0, at.y + s0 * r_bottom),
            Vec3::new(at.x + c1 * r_bottom, y0, at.y + s1 * r_bottom),
            Vec3::new(at.x + c1 * r_top, y0 + h, at.y + s1 * r_top),
            Vec3::new(at.x + c0 * r_top, y0 + h, at.y + s0 * r_top),
            color,
            out,
        );
    }
    // A small flat cap.
    if r_top > 0.05 {
        let top = shade(color, 1.06);
        let c = Vec3::new(at.x, y0 + h, at.y);
        for i in 0..n {
            let a0 = (i as f32 / n as f32) * std::f32::consts::TAU;
            let a1 = ((i + 1) as f32 / n as f32) * std::f32::consts::TAU;
            let (s0, c0) = a0.sin_cos();
            let (s1, c1) = a1.sin_cos();
            buf.tri_facing(
                c,
                Vec3::new(at.x + c0 * r_top, y0 + h, at.y + s0 * r_top),
                Vec3::new(at.x + c1 * r_top, y0 + h, at.y + s1 * r_top),
                top,
                Vec3::Y,
            );
        }
    }
}

/// A tapered rotor blade in the rotor's local frame: root at the origin,
/// pointing along `angle` in the XY plane, both faces emitted (the rotor mesh
/// is seen from either side).
fn push_blade(buf: &mut MeshData, angle: f32, len: f32, root_w: f32, tip_w: f32, color: [f32; 4]) {
    let dir = Vec2::from_angle(angle);
    let perp = Vec2::new(-dir.y, dir.x);
    let v = |p: Vec2| Vec3::new(p.x, p.y, 0.0);
    let (r0, r1) = (perp * root_w * 0.5, perp * tip_w * 0.5);
    let root = dir * len * 0.04; // a small hub gap
    let tip = dir * len;
    buf.quad_facing(
        v(root - r0),
        v(root + r0),
        v(tip + r1),
        v(tip - r1),
        color,
        Vec3::Z,
    );
    buf.quad_facing(
        v(root - r0),
        v(root + r0),
        v(tip + r1),
        v(tip - r1),
        shade(color, 0.92),
        Vec3::NEG_Z,
    );
}

/// A rotor: hub position in cell-local metres, which shared shape it wears and
/// how it is placed, and its spin rate.
struct RotorBuild {
    /// Hub centre, cell-local (x, y-above-ground-absolute, z).
    hub: Vec3,
    yaw: f32,
    shape: RotorShape,
    /// Blade length in metres — the shared shape's scale.
    radius: f32,
    /// Starting angle about the shaft, so a wind farm is not in lockstep.
    phase: f32,
    /// Radians per second at a reference 8 m/s wind.
    spin: f32,
}

/// The two rotors there are. Each is **one mesh shared by every instance**, sized
/// and phased by its `Transform`.
///
/// It was one mesh per rotor, and that is a draw call apiece: the ±30 km ring
/// over Groningen holds 612 of them, 490 on screen from 30 km up (measured
/// 2026-10-17) — on the web, where every draw is issued from the one thread, a
/// third of the frame's draws for a few hundred specks. One mesh and one material
/// is one batch. Every dimension of a blade was already proportional to its
/// length, so a unit shape scaled by the radius is the same geometry; only the
/// hub plate, which was a fixed 1 × 2.4 m, now scales with the rotor.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum RotorShape {
    Turbine,
    Windmill,
}

impl RotorShape {
    /// The shape at unit blade length, blades in the XY plane about +Z.
    fn mesh(self) -> Option<Mesh> {
        let mut buf = MeshData::default();
        match self {
            Self::Turbine => {
                let blade = linear(BLADE_WHITE);
                for b in 0..3 {
                    push_blade(
                        &mut buf,
                        b as f32 * std::f32::consts::TAU / 3.0,
                        1.0,
                        0.09,
                        0.025,
                        blade,
                    );
                }
                // The hub plate, at its old 1 × 2.4 m on a 45 m rotor.
                push_blade(&mut buf, 0.0, 1.0 / 45.0, 2.4 / 45.0, 2.4 / 45.0, blade);
            }
            Self::Windmill => {
                let wood = linear(SAIL_WOOD);
                for b in 0..4 {
                    push_blade(
                        &mut buf,
                        b as f32 * std::f32::consts::TAU / 4.0 + 0.4,
                        1.0,
                        0.22,
                        0.18,
                        wood,
                    );
                }
            }
        }
        buf.into_mesh()
    }

    fn index(self) -> usize {
        match self {
            Self::Turbine => 0,
            Self::Windmill => 1,
        }
    }
}

/// One built tall-structure cell.
pub(crate) struct TallCellBuild {
    grid_cell: GridCell,
    mesh: Option<Mesh>,
    rotors: Vec<RotorBuild>,
}

type CellResult = Result<Option<TallCellBuild>, String>;

/// Prevailing south-westerly, as a mesh yaw. The rotor plane faces the wind;
/// nothing here re-aims a standing turbine when the live wind veers — that
/// would re-pose the world for a number most users never check.
const PREVAILING_YAW: f32 = 0.75 * std::f32::consts::PI;

fn build_cell(
    text: &str,
    cx: i64,
    cy: i64,
    ax: i64,
    ay: i64,
    z: u32,
    lat: f64,
    ground: &TerrainSnapshot,
) -> CellResult {
    let structures = parse_structures(text)?;
    let (north, west) = tile_to_lat_lng(cx, cy, TALL_ZOOM);
    let (south, east) = tile_to_lat_lng(cx + 1, cy + 1, TALL_ZOOM);
    let tile_m = tile_to_meters(z, lat) as f32;
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

    let mut buf = MeshData::default();
    let mut rotors = Vec::new();
    let mut count = 0usize;
    for s in &structures {
        // Half-open ownership, so a structure on a cell edge exists once.
        if s.lat < south || s.lat >= north || s.lng < west || s.lng >= east {
            continue;
        }
        if count >= MAX_STRUCTURES_PER_CELL {
            break;
        }
        count += 1;
        let w = lat_lng_to_local(s.lat, s.lng, ax, ay, z, tile_m);
        let p = Vec2::new(w.x - ox, w.y - oz);
        let y0 = ground.at(w.x, w.y);
        let seed = feature_seed(s.lat, s.lng);
        let h = s
            .height
            .unwrap_or(s.kind.default_height())
            .clamp(4.0, 260.0);
        match s.kind {
            StructureKind::WindTurbine => {
                let tower = linear(TOWER_WHITE);
                push_tube(&mut buf, p, y0, (h * 0.026).max(1.6), 1.2, h, 10, tower);
                // Nacelle: a rounded box behind the hub.
                let yaw = PREVAILING_YAW + (srand(seed, 1) - 0.5) * 0.3;
                push_yawed_box(
                    &mut buf,
                    p,
                    y0,
                    yaw,
                    [0.0, h + 1.2, 2.2],
                    [1.6, 1.6, 4.0],
                    tower,
                );
                let rotor_r = s
                    .rotor_d
                    .map(|d| d * 0.5)
                    .unwrap_or(h * 0.55)
                    .clamp(4.0, 90.0);
                rotors.push(RotorBuild {
                    hub: Vec3::new(p.x, y0 + h + 1.2, p.y),
                    yaw,
                    shape: RotorShape::Turbine,
                    radius: rotor_r,
                    phase: srand(seed, 2) * 2.0,
                    spin: 1.0 + srand(seed, 3) * 0.3,
                });
            }
            StructureKind::Windmill => {
                let body = linear(BRICK);
                push_tube(&mut buf, p, y0, h * 0.42, h * 0.22, h * 0.82, 8, body);
                push_tube(
                    &mut buf,
                    p,
                    y0 + h * 0.82,
                    h * 0.26,
                    0.06,
                    h * 0.24,
                    8,
                    shade(linear(SAIL_WOOD), 0.5),
                );
                // Four lattice sails on a slow rotor, on the prevailing wind.
                let yaw = PREVAILING_YAW + (srand(seed, 1) - 0.5) * 0.4;
                rotors.push(RotorBuild {
                    hub: Vec3::new(p.x, y0 + h * 0.9, p.y),
                    yaw,
                    shape: RotorShape::Windmill,
                    radius: h * 0.85,
                    phase: 0.0,
                    spin: 0.25,
                });
            }
            StructureKind::Mast => {
                let steel = linear(STEEL);
                push_tube(&mut buf, p, y0, 1.6, 0.4, h, 6, steel);
                // A couple of dishes/platforms to break the silhouette.
                push_yawed_box(
                    &mut buf,
                    p,
                    y0,
                    srand(seed, 1),
                    [0.0, h * 0.8, 0.0],
                    [1.6, 0.25, 1.6],
                    steel,
                );
            }
            StructureKind::Tower => {
                let conc = linear(CONCRETE);
                push_tube(&mut buf, p, y0, 3.4, 2.2, h * 0.85, 10, conc);
                push_tube(
                    &mut buf,
                    p,
                    y0 + h * 0.85,
                    3.2,
                    2.6,
                    h * 0.15,
                    10,
                    shade(conc, 0.9),
                );
            }
            StructureKind::Chimney => {
                push_tube(&mut buf, p, y0, 2.6, 1.6, h, 10, linear(BRICK));
            }
            StructureKind::WaterTower => {
                let conc = linear(CONCRETE);
                push_tube(&mut buf, p, y0, 2.6, 2.2, h * 0.7, 10, conc);
                // The tank: a wider drum on top.
                push_tube(
                    &mut buf,
                    p,
                    y0 + h * 0.7,
                    h * 0.18,
                    h * 0.16,
                    h * 0.3,
                    12,
                    shade(conc, 1.05),
                );
            }
            StructureKind::Crane => {
                let yellow = linear(CRANE_YELLOW);
                let yaw = srand(seed, 1) * std::f32::consts::TAU;
                push_tube(&mut buf, p, y0, 1.2, 1.2, h, 4, yellow);
                // Jib and counter-jib.
                push_yawed_box(
                    &mut buf,
                    p,
                    y0,
                    yaw,
                    [h * 0.45, h + 1.0, 0.0],
                    [h * 0.55, 0.6, 0.6],
                    yellow,
                );
                push_yawed_box(
                    &mut buf,
                    p,
                    y0,
                    yaw,
                    [-h * 0.18, h + 1.0, 0.0],
                    [h * 0.18, 0.8, 0.8],
                    yellow,
                );
            }
        }
    }
    let mesh = buf.into_mesh();
    if mesh.is_none() && rotors.is_empty() {
        return Ok(None);
    }
    Ok(Some(TallCellBuild {
        grid_cell,
        mesh,
        rotors,
    }))
}

// ─── Resource + systems ──────────────────────────────────────────────────────

/// A spinning rotor: the local Z axis is the shaft.
#[derive(Component)]
pub struct TurbineRotor {
    /// Radians per second at the reference 8 m/s wind.
    base_spin: f32,
}

#[derive(Resource)]
pub struct TallStructures {
    anchor: Option<(i64, i64, u32, f64)>,
    terrain_revision: u32,
    enabled_was: bool,
    cells: CellStream<(i64, i64), CellResult>,
    cell_entities: HashMap<(i64, i64), Vec<Entity>>,
    material: Option<Handle<StandardMaterial>>,
    /// The shared rotor meshes, by [`RotorShape::index`], made on first use.
    rotor_meshes: [Option<Handle<Mesh>>; 2],
    /// When the lane first wanted cells, for [`COVERAGE_WAIT_SECS`].
    coverage_wait_since: Option<f64>,
}

impl Default for TallStructures {
    fn default() -> Self {
        Self {
            anchor: None,
            terrain_revision: 0,
            enabled_was: true,
            cells: CellStream::new(Policy {
                // Both halves of the endpoint's pace, for the reason
                // `osm_buildings`' own far stream states: the spacing is the
                // dispatcher's job, not a `thread::sleep` parking a pool thread.
                max_in_flight: crate::systems::osm_buildings::overpass_pace().0,
                min_interval_secs: crate::systems::osm_buildings::overpass_pace()
                    .1
                    .as_secs_f64(),
                max_attempts: MAX_ATTEMPTS,
                backoff_secs: BACKOFF_SECS,
                backoff_doublings: BACKOFF_DOUBLINGS,
                cooldown_secs: RETRY_AFTER_SECS,
                despawns_per_frame: MAX_DESPAWNS_PER_FRAME,
            }),
            cell_entities: HashMap::new(),
            material: None,
            rotor_meshes: [None, None],
            coverage_wait_since: None,
        }
    }
}

impl crate::systems::stream_watch::StreamBusy for TallStructures {
    const NAME: &'static str = "tall_structures";
    fn stream_busy(&self) -> bool {
        self.streaming_busy()
    }
}

impl TallStructures {
    /// Read by the `--shot` settle test alongside the other streamers.
    #[must_use]
    pub fn streaming_busy(&self) -> bool {
        self.cells.busy()
    }

    /// Spawned mesh entities (towers + rotors), for the shot harness.
    #[must_use]
    pub fn spawned(&self) -> usize {
        self.cell_entities.values().map(Vec::len).sum()
    }
}

/// Stream the tall structures around the camera.
#[allow(clippy::too_many_arguments)]
pub fn update_tall_structures(
    mut commands: Commands,
    mut ts: ResMut<TallStructures>,
    mut meshes: ResMut<Assets<Mesh>>,
    mut materials: ResMut<Assets<StandardMaterial>>,
    stream: Res<crate::systems::map_stream::MapStream>,
    time: Res<Time>,
    camera: AnchoredCamera,
    bigspace: Query<Entity, With<BigSpace>>,
    globe: Res<crate::systems::globe::GlobeState>,
    terrain: Res<crate::systems::terrain::TerrainField>,
    layers: Res<crate::systems::data_layers::DataLayers>,
) {
    ts.cells.drain_despawns(&mut commands);

    let Some((clat, clng, base_zoom)) = stream.geo_anchor() else {
        return;
    };

    let enabled = layers.street_detail_enabled;
    let turned_off = ts.enabled_was && !enabled;
    ts.enabled_was = enabled;
    let reanchored = ts.cells.sync(stream.anchor_generation);
    if reanchored || turned_off || ts.terrain_revision != terrain.revision {
        ts.terrain_revision = terrain.revision;
        let ts = &mut *ts;
        ts.cells.restart(&mut ts.cell_entities, &mut ts.anchor);
    }
    if !enabled {
        return;
    }
    COVERAGE.ensure(time.elapsed_secs_f64());

    if ts.anchor.is_none() {
        let (ax, ay) = lat_lng_to_tile(clat, clng, base_zoom);
        ts.anchor = Some((ax, ay, base_zoom, clat));
    }
    let (ax, ay, z, lat) = ts.anchor.unwrap();
    let tile_m = tile_to_meters(z, lat) as f32;

    let Some((cell, tf)) = camera.get() else {
        return;
    };
    let cam_altitude = globe.altitude_m(cell, tf) as f32;
    if cam_altitude > crate::systems::globe::GLOBE_SHOW_ALT {
        return;
    }
    let (ccx, ccy) = cell_ring::coarsen(
        cell_ring::tile_at((ax, ay), tile_m, camera.stream_xz_at(cell, tf)),
        z,
        TALL_ZOOM,
    );

    let now_secs = time.elapsed_secs_f64();
    cell_ring::request_ring(&mut ts.cells, (ccx, ccy), CELL_RADIUS, now_secs, |_, _| {
        true
    });

    // Hold dispatch until the manifest has answered — yes, no, or not reachable
    // within the wait — so a covered cell is not sent to Overpass in the moment
    // before the answer lands. The cells stay queued, so the settle test still
    // sees the lane as busy.
    let since = *ts.coverage_wait_since.get_or_insert(now_secs);
    let mut jobs = Vec::new();
    if COVERAGE.revision() > 0 || now_secs - since >= COVERAGE_WAIT_SECS {
        while let Some(cell) = ts.cells.next_job(now_secs) {
            jobs.push(cell);
        }
    }
    let sink = ts.cells.sink();
    for (cx, cy) in jobs {
        let results = sink.clone();
        let ground = terrain.snapshot();
        IoTaskPool::get()
            .spawn(async move {
                match fetch_cell_text(cx, cy).await {
                    Ok(text) => {
                        crate::platform::workers::submit_ranked(
                            "tall_structures",
                            crate::platform::workers::FAR,
                            Box::new(move || {
                                let built = build_cell(&text, cx, cy, ax, ay, z, lat, &ground);
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

    let cap = crate::quality::budget().per_frame(MAX_BUILDS_PER_FRAME);
    let done =
        crate::systems::stream_ledger::take_admitted(&mut ts.cells, cap, |built| match built {
            // The rotors' meshes are shared and already resident.
            Ok(Some(b)) => b
                .mesh
                .iter()
                .map(crate::systems::stream_ledger::mesh_cost)
                .sum::<usize>(),
            _ => 0,
        });

    for (cell, built) in done {
        match built {
            Ok(Some(build)) => {
                let material = ts
                    .material
                    .get_or_insert_with(|| {
                        materials.add(StandardMaterial {
                            base_color: Color::WHITE,
                            perceptual_roughness: 0.8,
                            ..default()
                        })
                    })
                    .clone();
                let mut entities = Vec::new();
                if let Some(mesh) = &build.mesh {
                    entities.push(
                        commands
                            .spawn((
                                Mesh3d(meshes.add(mesh.clone())),
                                MeshMaterial3d(material.clone()),
                                Transform::default(),
                                build.grid_cell,
                                ChildOf(root),
                                crate::components::HideInSpace,
                            ))
                            .id(),
                    );
                }
                for rotor in &build.rotors {
                    let slot = &mut ts.rotor_meshes[rotor.shape.index()];
                    if slot.is_none() {
                        *slot = rotor.shape.mesh().map(|m| meshes.add(m));
                    }
                    let Some(mesh) = slot.clone() else {
                        continue;
                    };
                    entities.push(
                        commands
                            .spawn((
                                Mesh3d(mesh),
                                MeshMaterial3d(material.clone()),
                                Transform::from_translation(rotor.hub)
                                    .with_rotation(
                                        Quat::from_rotation_y(rotor.yaw)
                                            * Quat::from_rotation_z(rotor.phase),
                                    )
                                    .with_scale(Vec3::splat(rotor.radius)),
                                build.grid_cell,
                                ChildOf(root),
                                TurbineRotor {
                                    base_spin: rotor.spin,
                                },
                                crate::components::HideInSpace,
                                bevy::light::NotShadowCaster,
                            ))
                            .id(),
                    );
                }
                ts.cell_entities.insert(cell, entities);
                ts.cells.succeeded(&cell);
            }
            Ok(None) => ts.cells.succeeded(&cell),
            Err(e) => {
                let attempts = ts.cells.failed(&cell, now_secs);
                if ts.cells.exhausted(&cell) {
                    tracing::warn!(?cell, attempts, error = %e, "tall-structure cell gave up");
                }
            }
        }
    }

    let retired: Vec<(i64, i64)> = ts
        .cell_entities
        .keys()
        .filter(|c| (c.0 - ccx).abs().max((c.1 - ccy).abs()) > KEEP_RADIUS)
        .copied()
        .collect();
    for cell in retired {
        if let Some(entities) = ts.cell_entities.remove(&cell) {
            ts.cells.retire_all(entities);
        }
        ts.cells.forget(&cell);
    }
}

/// Spin the rotors. Speed rides the live wind when there is one; becalmed
/// turbines still idle slowly, because a stopped wind farm reads as broken.
pub fn spin_rotors(
    // The moving world's clock, not the loaders': frozen between the frames of a
    // `--video-fps` movie (`systems::anim_clock`), the virtual clock otherwise.
    time: Res<crate::systems::anim_clock::AnimClock>,
    weather: Res<crate::systems::weather::WeatherState>,
    mut rotors: Query<(&TurbineRotor, &mut Transform, &ViewVisibility)>,
) {
    if rotors.is_empty() {
        return;
    }
    let wind_ms = weather
        .latest
        .as_ref()
        .map(|o| o.wind_ms)
        .unwrap_or(5.0)
        .clamp(1.0, 20.0);
    let factor = wind_ms / 8.0;
    let dt = time.delta_secs();
    for (rotor, mut tf, seen) in &mut rotors {
        // A rotor off screen keeps its angle: every write is a transform to
        // propagate and a mesh uniform to re-extract, for ~600 of them a frame.
        if !seen.get() {
            continue;
        }
        tf.rotate_local_z(rotor.base_spin * factor * dt);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_nodes_and_way_centers() {
        let text = r#"{"elements":[
            {"type":"node","id":1,"lat":53.3,"lon":6.5,
             "tags":{"power":"generator","generator:source":"wind","height:hub":"98","rotor:diameter":"112"}},
            {"type":"way","id":2,"center":{"lat":53.31,"lon":6.51},
             "tags":{"man_made":"windmill","height":"16"}},
            {"type":"node","id":3,"lat":53.32,"lon":6.52,"tags":{"man_made":"chimney"}},
            {"type":"node","id":4,"lat":53.33,"lon":6.53,"tags":{"man_made":"pier"}}
        ]}"#;
        let s = parse_structures(text).unwrap();
        assert_eq!(s.len(), 3);
        assert_eq!(s[0].kind, StructureKind::WindTurbine);
        assert_eq!(s[0].height, Some(98.0));
        assert_eq!(s[0].rotor_d, Some(112.0));
        assert_eq!(s[1].kind, StructureKind::Windmill);
        assert_eq!(s[2].kind, StructureKind::Chimney);
    }

    #[test]
    fn a_turbine_cell_builds_a_tower_and_a_rotor() {
        let text = r#"{"elements":[
            {"type":"node","id":1,"lat":53.3,"lon":6.5,
             "tags":{"power":"generator","generator:source":"wind"}}
        ]}"#;
        let (cx, cy) = lat_lng_to_tile(53.3, 6.5, TALL_ZOOM);
        let build = build_cell(
            text,
            cx,
            cy,
            8600,
            5500,
            14,
            53.3,
            &TerrainSnapshot::default(),
        )
        .unwrap()
        .unwrap();
        assert!(build.mesh.is_some(), "tower mesh");
        assert_eq!(build.rotors.len(), 1);
        // The hub stands at the default height above flat ground.
        assert!(
            (build.rotors[0].hub.y - 86.2).abs() < 0.5,
            "{}",
            build.rotors[0].hub.y
        );
    }

    /// Every rotor of a shape is one shared mesh at unit blade length, so the
    /// instance's scale is its blade length: the tip sits at radius 1.
    #[test]
    fn a_rotor_shape_is_unit_length_and_shared() {
        for shape in [RotorShape::Turbine, RotorShape::Windmill] {
            let mesh = shape.mesh().expect("a rotor has blades");
            let Some(bevy::mesh::VertexAttributeValues::Float32x3(positions)) =
                mesh.attribute(Mesh::ATTRIBUTE_POSITION)
            else {
                panic!("positions");
            };
            let reach = positions
                .iter()
                .map(|p| Vec2::new(p[0], p[1]).length())
                .fold(0.0f32, f32::max);
            assert!((reach - 1.0).abs() < 0.05, "{shape:?} reaches {reach}");
            assert!(positions.iter().all(|p| p[2] == 0.0), "{shape:?} is planar");
        }
        let text = r#"{"elements":[
            {"type":"node","id":1,"lat":53.3,"lon":6.5,"tags":{"power":"generator","generator:source":"wind","rotor:diameter":"112"}},
            {"type":"node","id":2,"lat":53.301,"lon":6.501,"tags":{"power":"generator","generator:source":"wind"}}
        ]}"#;
        let (cx, cy) = lat_lng_to_tile(53.3, 6.5, TALL_ZOOM);
        let build = build_cell(
            text,
            cx,
            cy,
            8600,
            5500,
            14,
            53.3,
            &TerrainSnapshot::default(),
        )
        .unwrap()
        .unwrap();
        assert_eq!(build.rotors.len(), 2);
        assert!(build.rotors.iter().all(|r| r.shape == RotorShape::Turbine));
        assert!(build.rotors.iter().any(|r| (r.radius - 56.0).abs() < 1e-3));
    }

    /// A covered cell is read from the atlas host's pre-cut file; an uncovered
    /// one, or any cell when the host is configured off, is a live query.
    #[test]
    fn a_covered_cell_reads_the_file_and_the_rest_ask_overpass() {
        let host = "https://3dbag.cartopolis.org";
        assert_eq!(
            cell_source(true, host, 1061, 664),
            CellSource::Atlas("https://3dbag.cartopolis.org/tall/11/1061/664.json".into())
        );
        assert_eq!(cell_source(false, host, 1061, 664), CellSource::Overpass);
        assert_eq!(cell_source(true, "", 1061, 664), CellSource::Overpass);
    }

    /// Before the manifest lands nothing is covered, so a cell asks Overpass — the
    /// lane never waits on a host that does not have the dataset.
    #[test]
    fn nothing_is_covered_before_the_manifest_answers() {
        assert!(!COVERAGE.covers(1061, 664));
        assert_eq!(COVERAGE.revision(), 0);
    }

    /// What atlas writes — the Overpass document re-serialised compactly, keys
    /// sorted — parses as the live answer does.
    #[test]
    fn an_atlas_cell_parses_like_a_live_answer() {
        let text = r#"{"elements":[{"center":{"lat":53.31,"lon":6.51},"id":2,"nodes":[1,2],"tags":{"man_made":"water_tower"},"type":"way"},{"id":1,"lat":53.3,"lon":6.5,"tags":{"generator:source":"wind","power":"generator"},"type":"node"}],"generator":"Overpass API","osm3s":{"copyright":"ODbL","timestamp_osm_base":"2026-09-26T08:00:00Z"},"version":0.6}"#;
        let s = parse_structures(text).unwrap();
        assert_eq!(s.len(), 2);
        assert_eq!(s[0].kind, StructureKind::WaterTower);
        assert_eq!(s[1].kind, StructureKind::WindTurbine);
    }

    #[test]
    fn a_remark_is_an_error() {
        assert!(parse_structures(r#"{"remark":"timeout","elements":[]}"#).is_err());
    }
}
