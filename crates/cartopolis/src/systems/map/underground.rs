//! Everything under the ground but the sewers: the soil, boreholes and
//! soundings, groundwater, ground-energy wells, transport pipelines, the gas
//! field with its earthquakes, and contaminated sites (Map ▸ Underground,
//! `docs/notes/underground.md`).
//!
//! Each is an atlas dataset writing its own tree in **one** format,
//! `cartopolis_geo::formats::underground` — columns, lines, points and rings,
//! every depth in metres below the ground — so one streamer and one mesher here
//! serve all of them, driven by the [`LAYERS`] table. A layer is a row: its tree,
//! its label, and how far around the camera to stream, which is the one thing
//! that differs in kind — an earthquake three kilometres down under a village
//! ten kilometres away is still in view through the opened ground, a borehole
//! across town is not.
//!
//! What is shared with `systems::sewers` is the opened ground and the backdrop:
//! any underground layer opens the ground ([`opens_ground`]), and the backdrop
//! sinks below the deepest thing loaded ([`Underground::deepest_m`]) — a soil
//! column reaches fifty metres, a gas well three thousand.

use std::collections::HashMap;

use bevy::prelude::*;
use bevy::tasks::IoTaskPool;
use big_space::prelude::{BigSpace, Grid, GridCell};

use crate::grid::{AnchoredCamera, WORLD_CELL_EDGE, WORLD_SWITCH_THRESHOLD};
use crate::platform::stream::{CellStream, Policy};
use crate::platform::{http, storage};
use crate::systems::cell_ring;
use crate::systems::coverage::CoverageSlot;
use crate::systems::osm_buildings::{lat_lng_to_local, tdbag_url, MeshData};
use crate::systems::sewers::push_pipe;
use crate::systems::street_detail::{linear, shade};
use crate::systems::terrain::TerrainSnapshot;
use cartopolis_core::world::{lat_lng_to_tile, tile_to_lat_lng, tile_to_meters};
use cartopolis_geo::formats::underground::{self as wire, Kind};

const ZOOM: u32 = cartopolis_geo::formats::CACHE_ZOOM;

/// One underground layer: a tree on the data host and a row in the Map menu.
pub struct LayerDef {
    /// The tree (`<key>/14/x/y.json`), the preference key and the `--underground`
    /// name.
    pub key: &'static str,
    pub label: &'static str,
    /// Cells streamed out from the camera's.
    pub radius: i64,
}

/// The layers, in menu order. **Append only**: the index is the slot in
/// [`DataLayers::underground`](crate::systems::data_layers::DataLayers) and in
/// [`COVERAGE`].
pub const LAYERS: [LayerDef; 7] = [
    LayerDef {
        key: "geotop",
        label: "Soil layers",
        radius: 1,
    },
    LayerDef {
        key: "boreholes",
        label: "Boreholes and soundings",
        radius: 1,
    },
    LayerDef {
        key: "groundwater",
        label: "Groundwater",
        radius: 1,
    },
    LayerDef {
        key: "wko",
        label: "Heat storage wells",
        radius: 1,
    },
    LayerDef {
        key: "pipelines",
        label: "Transport pipelines",
        radius: 2,
    },
    LayerDef {
        // Kilometres down, and seen through the opening from far off.
        key: "gasfield",
        label: "Gas field and earthquakes",
        radius: 4,
    },
    LayerDef {
        key: "contamination",
        label: "Soil contamination",
        radius: 1,
    },
];

/// Which cells the host serves, per layer, in [`LAYERS`] order.
static COVERAGE: [CoverageSlot; 7] = [
    CoverageSlot::new("geotop", "/geotop/coverage.json", ZOOM),
    CoverageSlot::new("boreholes", "/boreholes/coverage.json", ZOOM),
    CoverageSlot::new("groundwater", "/groundwater/coverage.json", ZOOM),
    CoverageSlot::new("wko", "/wko/coverage.json", ZOOM),
    CoverageSlot::new("pipelines", "/pipelines/coverage.json", ZOOM),
    CoverageSlot::new("gasfield", "/gasfield/coverage.json", ZOOM),
    CoverageSlot::new("contamination", "/contamination/coverage.json", ZOOM),
];

/// Every loaded layer's attribution line.
pub fn attributions() -> impl Iterator<Item = String> {
    COVERAGE.iter().filter_map(CoverageSlot::attribution)
}

const CONCURRENCY: usize = 4;
const MAX_ATTEMPTS: u32 = 3;
const BACKOFF_SECS: f64 = 2.0;
const BACKOFF_DOUBLINGS: u32 = 2;
const RETRY_AFTER_SECS: f64 = 60.0;
const MAX_DESPAWNS_PER_FRAME: usize = 4;
const MAX_BUILDS_PER_FRAME: usize = 1;

/// Longest straight run of a shallow line before the ground is sampled again.
/// Below [`FOLLOW_GROUND_M`] the street's relief is noise and is not followed.
const STEP_M: f32 = 8.0;
const FOLLOW_GROUND_M: f32 = 50.0;
const COLUMN_SIDES: u32 = 6;

/// A shape's colour. Soils in the colours of a Dutch geological map — peat
/// brown, clay green, sand yellow coarsening to orange — so a column reads the
/// way a borehole log does.
#[must_use]
pub fn kind_colour(kind: Kind) -> u32 {
    match kind {
        Kind::Unknown => 0x80_80_80,
        Kind::Anthropogenic => 0x9E_9A_8F,
        Kind::Peat => 0x5A_3B_22,
        Kind::Clay => 0x6E_8B_5A,
        Kind::ClayeySand => 0xA8_B0_60,
        Kind::FineSand => 0xE8_D2_7A,
        Kind::MediumSand => 0xE0_B8_4A,
        Kind::CoarseSand => 0xD3_9A_2C,
        Kind::Gravel => 0xB0_70_40,
        Kind::Shells => 0xF0_EE_E0,
        Kind::WaterTable => 0x2E_9B_FF,
        Kind::WellFilter => 0x6C_C4_FF,
        Kind::WellTube => 0x9A_A4_AE,
        Kind::HeatWarm => 0xE5_48_3A,
        Kind::HeatCold => 0x3A_7B_E5,
        Kind::HeatLoop => 0xB0_5F_D0,
        Kind::PipeGas => 0xF2_C8_1E,
        Kind::PipeChemical => 0xFF_7A_1A,
        Kind::PipeOther => 0x9C_9C_9C,
        Kind::GasWell => 0xED_ED_ED,
        Kind::Earthquake => 0xFF_2D_55,
        Kind::GasField => 0xFF_B0_00,
        Kind::ContaminationSerious => 0xE0_21_8A,
        Kind::Contamination => 0xD9_8B_C0,
        Kind::Sounding => 0xC0_C8_D0,
        Kind::FuelTank => 0x7A_1F_A2,
        Kind::FuelTankFilled => 0xA9_9A_B8,
    }
}

/// The tube a ring is drawn with: thick enough to see at its depth.
#[must_use]
pub fn ring_radius(depth_m: f32) -> f32 {
    (depth_m * 0.004).max(0.3)
}

/// A vertical `n`-gon prism, `top` and `bottom` as heights.
fn push_prism(buf: &mut MeshData, at: Vec2, top: f32, bottom: f32, r: f32, color: [f32; 4]) {
    if top - bottom <= 0.0 {
        return;
    }
    let n = COLUMN_SIDES;
    for i in 0..n {
        let a0 = (i as f32 / n as f32) * std::f32::consts::TAU;
        let a1 = ((i + 1) as f32 / n as f32) * std::f32::consts::TAU;
        let (s0, c0) = a0.sin_cos();
        let (s1, c1) = a1.sin_cos();
        let out = Vec3::new((c0 + c1) * 0.5, 0.0, (s0 + s1) * 0.5);
        // Unlit: the form is baked into the colour, lit from one side.
        let c = shade(color, 0.72 + 0.28 * (0.5 + 0.5 * (out.x + out.z) * 0.7071));
        buf.quad_facing(
            Vec3::new(at.x + c0 * r, bottom, at.y + s0 * r),
            Vec3::new(at.x + c1 * r, bottom, at.y + s1 * r),
            Vec3::new(at.x + c1 * r, top, at.y + s1 * r),
            Vec3::new(at.x + c0 * r, top, at.y + s0 * r),
            c,
            out,
        );
    }
    // The bottom cap, which is what a column is seen by from above through
    // the opening once the segment over it is drawn.
    let cap = Vec3::new(at.x, top, at.y);
    let lid = shade(color, 1.05);
    for i in 0..n {
        let a0 = (i as f32 / n as f32) * std::f32::consts::TAU;
        let a1 = ((i + 1) as f32 / n as f32) * std::f32::consts::TAU;
        buf.tri_facing(
            cap,
            Vec3::new(at.x + a0.cos() * r, top, at.y + a0.sin() * r),
            Vec3::new(at.x + a1.cos() * r, top, at.y + a1.sin() * r),
            lid,
            Vec3::Y,
        );
    }
}

/// An icosahedron of radius `r` — a sphere at the size these are seen.
fn push_ball(buf: &mut MeshData, c: Vec3, r: f32, color: [f32; 4]) {
    let t = (1.0 + 5f32.sqrt()) / 2.0;
    let v = [
        Vec3::new(-1.0, t, 0.0),
        Vec3::new(1.0, t, 0.0),
        Vec3::new(-1.0, -t, 0.0),
        Vec3::new(1.0, -t, 0.0),
        Vec3::new(0.0, -1.0, t),
        Vec3::new(0.0, 1.0, t),
        Vec3::new(0.0, -1.0, -t),
        Vec3::new(0.0, 1.0, -t),
        Vec3::new(t, 0.0, -1.0),
        Vec3::new(t, 0.0, 1.0),
        Vec3::new(-t, 0.0, -1.0),
        Vec3::new(-t, 0.0, 1.0),
    ]
    .map(|p| p.normalize());
    const F: [[usize; 3]; 20] = [
        [0, 11, 5],
        [0, 5, 1],
        [0, 1, 7],
        [0, 7, 10],
        [0, 10, 11],
        [1, 5, 9],
        [5, 11, 4],
        [11, 10, 2],
        [10, 7, 6],
        [7, 1, 8],
        [3, 9, 4],
        [3, 4, 2],
        [3, 2, 6],
        [3, 6, 8],
        [3, 8, 9],
        [4, 9, 5],
        [2, 4, 11],
        [6, 2, 10],
        [8, 6, 7],
        [9, 8, 1],
    ];
    for [a, b, d] in F {
        let out = (v[a] + v[b] + v[d]).normalize();
        let col = shade(color, 0.6 + 0.4 * (0.5 + 0.5 * out.y));
        buf.tri_facing(c + v[a] * r, c + v[b] * r, c + v[d] * r, col, out);
    }
}

/// A tube along `pts` at a depth per point, following the ground where it is
/// shallow enough for the ground to matter.
#[allow(clippy::too_many_arguments)]
fn push_line(
    buf: &mut MeshData,
    pts: &[Vec2],
    depths: &[f32],
    closed: bool,
    r: f32,
    color: [f32; 4],
    ground: &TerrainSnapshot,
    origin: Vec2,
) {
    let n = pts.len();
    if n < 2 || depths.len() != n {
        return;
    }
    let at = |p: Vec2, depth: f32| {
        Vec3::new(p.x - origin.x, ground.at(p.x, p.y) - depth, p.y - origin.y)
    };
    let segments = if closed { n } else { n - 1 };
    for i in 0..segments {
        let (a, b) = (pts[i], pts[(i + 1) % n]);
        let (da, db) = (depths[i], depths[(i + 1) % n]);
        let steps = if da.max(db) < FOLLOW_GROUND_M {
            (a.distance(b) / STEP_M).ceil().max(1.0) as usize
        } else {
            1
        };
        let mut prev = at(a, da);
        for s in 1..=steps {
            let f = s as f32 / steps as f32;
            let next = at(a.lerp(b, f), da + (db - da) * f);
            push_pipe(buf, prev, next, r, color);
            prev = next;
        }
    }
}

/// One built cell.
pub(crate) struct CellBuild {
    grid_cell: GridCell,
    mesh: Option<Mesh>,
    shapes: usize,
    deepest_m: f32,
}

type CellResult = Result<Option<CellBuild>, String>;

#[allow(clippy::too_many_arguments)]
fn build_cell(
    cell: &wire::Cell,
    cx: i64,
    cy: i64,
    ax: i64,
    ay: i64,
    z: u32,
    lat: f64,
    ground: &TerrainSnapshot,
) -> CellResult {
    if cell.is_empty() {
        return Ok(None);
    }
    let (north, west) = tile_to_lat_lng(cx, cy, ZOOM);
    let (south, east) = tile_to_lat_lng(cx + 1, cy + 1, ZOOM);
    let tile_m = tile_to_meters(z, lat) as f32;
    let centre = lat_lng_to_local(
        (north + south) / 2.0,
        (west + east) / 2.0,
        ax,
        ay,
        z,
        tile_m,
    );
    let (grid_cell, _) = Grid::new(WORLD_CELL_EDGE, WORLD_SWITCH_THRESHOLD)
        .translation_to_grid(Vec3::new(centre.x, 0.0, centre.y).as_dvec3());
    let origin = Vec2::new(
        grid_cell.x as f32 * WORLD_CELL_EDGE,
        grid_cell.z as f32 * WORLD_CELL_EDGE,
    );
    let to_world = |(la, ln): (f64, f64)| lat_lng_to_local(la, ln, ax, ay, z, tile_m);
    let colour = |k: u8| linear(kind_colour(Kind::from_index(k)));

    let mut buf = MeshData::default();
    let mut shapes = 0usize;
    for (la, ln, r, segments) in &cell.columns {
        let p = to_world((*la, *ln));
        let top = ground.at(p.x, p.y);
        for &(from, to, kind) in segments {
            push_prism(&mut buf, p - origin, top - from, top - to, *r, colour(kind));
        }
        shapes += 1;
    }
    for (kind, r, depths, flat) in &cell.lines {
        let Some(pts) = wire::points_of(flat, 2) else {
            continue;
        };
        let pts: Vec<Vec2> = pts.into_iter().map(to_world).collect();
        push_line(
            &mut buf,
            &pts,
            depths,
            false,
            *r,
            colour(*kind),
            ground,
            origin,
        );
        shapes += 1;
    }
    for &(kind, la, ln, depth, size) in &cell.points {
        let p = to_world((la, ln));
        let c = Vec3::new(p.x - origin.x, ground.at(p.x, p.y) - depth, p.y - origin.y);
        push_ball(&mut buf, c, size.max(0.2), colour(kind));
        shapes += 1;
    }
    for (kind, depth, flat) in &cell.rings {
        let Some(pts) = wire::points_of(flat, 3) else {
            continue;
        };
        let pts: Vec<Vec2> = pts.into_iter().map(to_world).collect();
        let depths = vec![*depth; pts.len()];
        let r = ring_radius(*depth);
        push_line(
            &mut buf,
            &pts,
            &depths,
            true,
            r,
            colour(*kind),
            ground,
            origin,
        );
        shapes += 1;
    }
    let Some(mesh) = buf.into_mesh() else {
        return Ok(None);
    };
    Ok(Some(CellBuild {
        grid_cell,
        mesh: Some(mesh),
        shapes,
        deepest_m: cell.deepest_m(),
    }))
}

async fn fetch_cell(layer: usize, cx: i64, cy: i64) -> Result<wire::Cell, String> {
    let def = &LAYERS[layer];
    let stamp = COVERAGE[layer].stamp();
    let key = format!("{}/{stamp}/{ZOOM}/{cx}/{cy}.json", def.key);
    let text = match storage::get_string(storage::Namespace::Cache, &key).await {
        Some(cached) => cached,
        None => {
            let url = format!(
                "{}/{}/{ZOOM}/{cx}/{cy}.json?v={stamp}",
                tdbag_url(),
                def.key
            );
            let bytes = http::fetch(http::Request::get(url)).await?;
            let text = String::from_utf8(bytes).map_err(|e| e.to_string())?;
            storage::put(storage::Namespace::Cache, &key, text.as_bytes()).await;
            text
        }
    };
    wire::parse(&text)
}

/// One layer's stream and what it has spawned.
struct LayerStream {
    anchor: Option<(i64, i64, u32, f64)>,
    terrain_revision: u32,
    enabled_was: bool,
    cells: CellStream<(i64, i64), CellResult>,
    entities: HashMap<(i64, i64), Vec<Entity>>,
    shapes: HashMap<(i64, i64), usize>,
    deepest: HashMap<(i64, i64), f32>,
}

impl Default for LayerStream {
    fn default() -> Self {
        Self {
            anchor: None,
            terrain_revision: 0,
            enabled_was: false,
            cells: CellStream::new(Policy {
                max_in_flight: CONCURRENCY,
                min_interval_secs: 0.0,
                max_attempts: MAX_ATTEMPTS,
                backoff_secs: BACKOFF_SECS,
                backoff_doublings: BACKOFF_DOUBLINGS,
                cooldown_secs: RETRY_AFTER_SECS,
                despawns_per_frame: MAX_DESPAWNS_PER_FRAME,
            }),
            entities: HashMap::new(),
            shapes: HashMap::new(),
            deepest: HashMap::new(),
        }
    }
}

impl LayerStream {
    fn restart(&mut self) {
        self.cells.restart(&mut self.entities, &mut self.anchor);
        self.shapes.clear();
        self.deepest.clear();
    }
}

/// Every underground layer's stream.
#[derive(Resource)]
pub struct Underground {
    layers: Vec<LayerStream>,
    material: Option<Handle<StandardMaterial>>,
}

impl Default for Underground {
    fn default() -> Self {
        Self {
            layers: LAYERS.iter().map(|_| LayerStream::default()).collect(),
            material: None,
        }
    }
}

impl crate::systems::stream_watch::StreamBusy for Underground {
    const NAME: &'static str = "underground";
    fn stream_busy(&self) -> bool {
        self.layers.iter().any(|l| l.cells.busy())
    }
}

impl Underground {
    /// Shapes drawn across every layer, for `--dump-state`.
    #[must_use]
    pub fn shapes(&self) -> usize {
        self.layers.iter().flat_map(|l| l.shapes.values()).sum()
    }

    /// Spawned meshes across every layer, for `--dump-state`.
    #[must_use]
    pub fn meshes(&self) -> usize {
        self.layers
            .iter()
            .flat_map(|l| l.entities.values())
            .map(Vec::len)
            .sum()
    }

    /// The deepest depth anything loaded reaches — where the backdrop must lie.
    #[must_use]
    pub fn deepest_m(&self) -> f32 {
        self.layers
            .iter()
            .flat_map(|l| l.deepest.values())
            .copied()
            .fold(0.0, f32::max)
    }
}

/// Is any underground layer, sewers included, asking for the ground to open?
#[must_use]
pub fn opens_ground(layers: &crate::systems::data_layers::DataLayers) -> bool {
    layers.map_enabled && (layers.sewers || layers.underground.iter().any(|on| *on))
}

/// Stream every enabled layer's cells around the camera and mesh them.
#[allow(clippy::too_many_arguments)]
pub fn update_underground(
    mut commands: Commands,
    mut under: ResMut<Underground>,
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
    let now = time.elapsed_secs_f64();
    let Some((clat, clng, base_zoom)) = stream.geo_anchor() else {
        return;
    };
    let camera_at = camera.get().map(|(c, t)| (*c, *t));
    let high = camera_at.is_some_and(|(c, t)| {
        globe.altitude_m(&c, &t) as f32 > crate::systems::globe::GLOBE_SHOW_ALT
    });
    let root = bigspace.single().ok();
    let cap = crate::quality::budget().per_frame(MAX_BUILDS_PER_FRAME);
    let under = &mut *under;

    for (i, def) in LAYERS.iter().enumerate() {
        let layer = &mut under.layers[i];
        layer.cells.drain_despawns(&mut commands);
        let enabled = layers.map_enabled && layers.underground[i];
        let turned_off = layer.enabled_was && !enabled;
        layer.enabled_was = enabled;
        let reanchored = layer.cells.sync(stream.anchor_generation);
        if reanchored || turned_off || layer.terrain_revision != terrain.revision {
            layer.terrain_revision = terrain.revision;
            layer.restart();
        }
        if !enabled {
            continue;
        }
        let coverage = &COVERAGE[i];
        coverage.ensure(now);
        if layer.anchor.is_none() {
            let (ax, ay) = lat_lng_to_tile(clat, clng, base_zoom);
            layer.anchor = Some((ax, ay, base_zoom, clat));
        }
        let (ax, ay, z, lat) = layer.anchor.expect("set just above");
        let tile_m = tile_to_meters(z, lat) as f32;
        let Some((cell, tf)) = camera_at.as_ref() else {
            continue;
        };
        if high {
            continue;
        }
        let (ccx, ccy) =
            cell_ring::coarsen(cell_ring::camera_tile((ax, ay), tile_m, cell, tf), z, ZOOM);
        cell_ring::request_ring(&mut layer.cells, (ccx, ccy), def.radius, now, |c, _| {
            coverage.covers(c.0, c.1)
        });

        let sink = layer.cells.sink();
        while let Some((cx, cy)) = layer.cells.next_job(now) {
            let results = sink.clone();
            let ground = terrain.snapshot();
            IoTaskPool::get()
                .spawn(async move {
                    match fetch_cell(i, cx, cy).await {
                        Ok(parsed) => {
                            crate::platform::workers::submit_ranked(
                                "underground",
                                crate::platform::workers::FAR,
                                Box::new(move || {
                                    let built =
                                        build_cell(&parsed, cx, cy, ax, ay, z, lat, &ground);
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

        let Some(root) = root else {
            continue;
        };
        let done =
            crate::systems::stream_ledger::take_admitted(
                &mut layer.cells,
                cap,
                |built| match built {
                    Ok(Some(b)) => b
                        .mesh
                        .iter()
                        .map(crate::systems::stream_ledger::mesh_cost)
                        .sum(),
                    _ => 0,
                },
            );
        for (c, built) in done {
            match built {
                Ok(Some(build)) => {
                    let material = under
                        .material
                        .get_or_insert_with(|| {
                            materials.add(StandardMaterial {
                                base_color: Color::WHITE,
                                // Underground there is no sun; the form is in
                                // the vertex colour.
                                unlit: true,
                                // No air underground to fog: three kilometres of distance fog
                                // turns a gas well into sky.
                                fog_enabled: false,
                                ..default()
                            })
                        })
                        .clone();
                    let mut spawned = Vec::new();
                    if let Some(mesh) = build.mesh {
                        spawned.push(
                            commands
                                .spawn((
                                    Mesh3d(meshes.add(mesh)),
                                    MeshMaterial3d(material),
                                    Transform::default(),
                                    build.grid_cell,
                                    ChildOf(root),
                                    crate::components::HideInSpace,
                                    bevy::light::NotShadowCaster,
                                ))
                                .id(),
                        );
                    }
                    layer.shapes.insert(c, build.shapes);
                    layer.deepest.insert(c, build.deepest_m);
                    layer.entities.insert(c, spawned);
                    layer.cells.succeeded(&c);
                }
                Ok(None) => layer.cells.succeeded(&c),
                Err(e) => {
                    let attempts = layer.cells.failed(&c, now);
                    if layer.cells.exhausted(&c) {
                        tracing::debug!(layer = def.key, ?c, attempts, error = %e,
                                        "underground cell unavailable");
                    }
                }
            }
        }

        let keep = def.radius + 1;
        let retired: Vec<(i64, i64)> = layer
            .entities
            .keys()
            .filter(|c| (c.0 - ccx).abs().max((c.1 - ccy).abs()) > keep)
            .copied()
            .collect();
        for c in retired {
            if let Some(entities) = layer.entities.remove(&c) {
                layer.cells.retire_all(entities);
            }
            layer.shapes.remove(&c);
            layer.deepest.remove(&c);
            layer.cells.forget(&c);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_kind_has_its_own_colour() {
        let mut seen = std::collections::HashSet::new();
        for k in Kind::ALL {
            assert!(seen.insert(kind_colour(k)), "{k:?}");
        }
    }

    #[test]
    fn the_table_and_the_coverage_slots_agree() {
        for (def, slot) in LAYERS.iter().zip(COVERAGE.iter()) {
            assert_eq!(def.key, slot.label(), "{}", def.key);
        }
        let mut keys: Vec<_> = LAYERS.iter().map(|d| d.key).collect();
        keys.sort_unstable();
        keys.dedup();
        assert_eq!(keys.len(), LAYERS.len());
    }

    /// A column stands under the ground at the depths it gives; a point sits
    /// at its depth; a deep ring is drawn thick enough to see.
    #[test]
    fn shapes_hang_under_the_ground_at_their_depths() {
        let (ax, ay) = lat_lng_to_tile(53.2194, 6.5665, ZOOM);
        let (north, west) = tile_to_lat_lng(ax, ay, ZOOM);
        let (la, ln) = (north - 0.001, west + 0.001);
        let cell = wire::Cell {
            columns: vec![(la, ln, 1.0, vec![(0.0, 2.0, 2), (2.0, 10.0, 6)])],
            points: vec![(Kind::Earthquake.index(), la, ln, 3000.0, 50.0)],
            ..Default::default()
        };
        let built = build_cell(
            &cell,
            ax,
            ay,
            ax,
            ay,
            ZOOM,
            53.2194,
            &TerrainSnapshot::default(),
        )
        .unwrap()
        .expect("shapes");
        assert_eq!(built.shapes, 2);
        assert_eq!(built.deepest_m, 3050.0);
        let mesh = built.mesh.unwrap();
        let Some(bevy::mesh::VertexAttributeValues::Float32x3(pos)) =
            mesh.attribute(Mesh::ATTRIBUTE_POSITION)
        else {
            panic!("positions");
        };
        let (lo, hi) = pos.iter().fold((f32::MAX, f32::MIN), |(lo, hi), p| {
            (lo.min(p[1]), hi.max(p[1]))
        });
        assert!((hi - 0.0).abs() < 1e-3, "column top at the ground, {hi}");
        // An icosahedron's lowest vertex is at 0.85 of its radius, not 1.0.
        assert!(
            (-3050.0..=-3040.0).contains(&lo),
            "ball bottom near 3,050 m, {lo}"
        );
        assert!(ring_radius(3000.0) >= 10.0);
        assert!((ring_radius(0.5) - 0.3).abs() < 1e-6);
    }
}
