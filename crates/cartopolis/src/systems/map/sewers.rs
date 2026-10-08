//! The sewers under the street, and a ground you can look through to see them
//! (Map ▸ Sewers, `docs/notes/sewers.md`).
//!
//! The municipalities publish their sewer registers in one national vocabulary
//! (GWSW) and PDOK serves them all, CC0; atlas's `pipeline/sewers.rs` bins them
//! into the z14 cells everything here streams in. It is the one network under a
//! Dutch street that is open — gas, water, power and telecom are in KLIC, which
//! is handed out per dig and may not be republished.
//!
//! **Depths, not levels.** Each pipe end carries how far below the ground its
//! invert lies (the register's ground at the manhole minus its *bob*), so a pipe
//! is drawn at `terrain − depth` and inherits whatever datum the terrain field
//! uses. Between its ends the depth is interpolated along the line and the ground
//! is sampled every [`STEP_M`], so the pipe follows the street over a rise. A
//! real gravity sewer is straight in absolute level, not in depth, and the two
//! differ by the ground's own curvature across a 40 m section — centimetres here.
//! A pipe with no measured level is drawn at [`DEFAULT_DEPTH_M`].
//!
//! **No pipe breaks the street.** Whatever the depth says, the crown stays at
//! least [`MIN_COVER_M`] under the ground ([`invert_depths`]). The default is a
//! street sewer's depth, and the unmeasured pipes include Groningen's culverts
//! and storage sewers, two to three and a half metres across — hung from 1.7 m
//! they stood a metre out of the road. A few measured ends are as bad (0.1 m
//! over a 315 mm pipe, a ground level at the manhole that is wrong); a register
//! that puts a crown above its own street is wrong about the depth, not the
//! street.
//!
//! **Seeing them means opening the ground**, and the ground is a stack of
//! opaque sheets ordered by a depth ladder (`ground_stack`), so it cannot simply
//! be made translucent: sorting would put every sheet in a fight with the others.
//! It is a screen door instead — `ground_detail.wgsl`'s `GROUND_SEE_THROUGH`
//! variant discards a fixed ordered-dither share of the ground's pixels near the
//! camera, the same pixels on every sheet, while the rest stays opaque and keeps
//! writing depth. Behind the holes there would be sky, so a dark [`Backdrop`]
//! sheet follows the camera a few metres under the deepest pipe.

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
use crate::systems::street_detail::{linear, shade};
use crate::systems::terrain::TerrainSnapshot;
use cartopolis_core::world::{lat_lng_to_tile, tile_to_lat_lng, tile_to_meters};
use cartopolis_geo::formats::sewers::{self as wire, SewerKind};

const SEWER_ZOOM: u32 = cartopolis_geo::formats::CACHE_ZOOM;
/// Cells out from the camera: 3×3 ≈ 4.4 km, far wider than the ground opens.
const CELL_RADIUS: i64 = 1;
const KEEP_RADIUS: i64 = CELL_RADIUS + 1;

const CONCURRENCY: usize = 4;
const MAX_ATTEMPTS: u32 = 3;
const BACKOFF_SECS: f64 = 2.0;
const BACKOFF_DOUBLINGS: u32 = 2;
const RETRY_AFTER_SECS: f64 = 60.0;
const MAX_DESPAWNS_PER_FRAME: usize = 4;
const MAX_BUILDS_PER_FRAME: usize = 1;

/// Longest straight run of a drawn pipe before the ground is sampled again.
const STEP_M: f32 = 8.0;
/// Where a pipe with no measured level is drawn: the median of the measured
/// ones over central Groningen was 1.72 m (atlas's `pipeline/sewers.rs`).
pub const DEFAULT_DEPTH_M: f32 = 1.7;
/// A pumped main is laid shallower — it does not need a fall.
const DEFAULT_PRESSURE_DEPTH_M: f32 = 1.2;
/// The least ground over a pipe's crown. Dutch practice lays sewers under 0.8 m
/// or more; this is the floor, not a typical value.
const MIN_COVER_M: f32 = 0.5;
/// A manhole the register gives no depth for.
const DEFAULT_MANHOLE_DEPTH_M: f32 = 2.0;
/// A house drain is 110 mm across and would be a hairline from fifty metres; this
/// is the thinnest a pipe is drawn, as a radius.
const MIN_RADIUS_M: f32 = 0.12;
/// A pipe the register gives no size for (a common 300 mm sewer).
const DEFAULT_DIAMETER_MM: u16 = 300;
const MANHOLE_RADIUS_M: f32 = 0.45;
const PIPE_SIDES: u32 = 8;
const MANHOLE_SIDES: u32 = 8;

/// How far under the ground at the camera the backdrop lies. Deeper than nearly
/// every sewer (the 95th percentile is ~3 m) and than the relief of a city
/// within the opening's radius.
const BACKDROP_DEPTH_M: f32 = 14.0;
/// The backdrop's half-width: past the opening (`SEE_THROUGH_FAR_M` in the
/// shader, 450 m), so its edge is never seen through a hole.
const BACKDROP_HALF_M: f32 = 700.0;
/// How far under the deepest loaded underground shape the backdrop lies.
const BACKDROP_MARGIN_M: f32 = 30.0;
/// How far under the backdrop the ground skirt is sunk while the ground is open.
const SKIRT_CLEARANCE_M: f32 = 20.0;
const BACKDROP_RGB: u32 = 0x2B_22_1A;

static COVERAGE: CoverageSlot = CoverageSlot::new("sewers", "/sewers/coverage.json", SEWER_ZOOM);

/// The attribution line, once coverage has loaded and is non-empty.
pub fn sewer_attribution() -> Option<String> {
    COVERAGE.attribution()
}

/// The colour a pipe of this kind is drawn in. Foul water brown, rain blue — the
/// convention of every municipal sewer drawing — with the combined system that
/// carries both between them in amber, since it is most of an old city.
#[must_use]
pub fn kind_colour(kind: SewerKind) -> u32 {
    match kind {
        SewerKind::Combined => 0xD0_8A_2E,
        SewerKind::Foul => 0x8E_55_33,
        SewerKind::Storm => 0x3B_7D_D8,
        SewerKind::Drainage => 0x2F_A3_9A,
        SewerKind::Pressure => 0xC2_40_8F,
        SewerKind::Other => 0x8C_8C_8C,
    }
}

const MANHOLE_RGB: u32 = 0xC8_C8_C0;

/// Invert depth at both ends, filling one missing end from the other and both
/// from the kind's default, then pushed down until the crown of a pipe
/// `height_m` tall has [`MIN_COVER_M`] of ground over it (module note).
#[must_use]
pub fn invert_depths(
    kind: SewerKind,
    start: Option<f32>,
    end: Option<f32>,
    height_m: f32,
) -> (f32, f32) {
    let fallback = if kind == SewerKind::Pressure {
        DEFAULT_PRESSURE_DEPTH_M
    } else {
        DEFAULT_DEPTH_M
    };
    let (a, b) = match (start, end) {
        (Some(a), Some(b)) => (a, b),
        (Some(a), None) => (a, a),
        (None, Some(b)) => (b, b),
        (None, None) => (fallback, fallback),
    };
    let floor = height_m + MIN_COVER_M;
    (a.max(floor), b.max(floor))
}

/// An `n`-sided prism from `a` to `b` (centres), `half` its half-width and
/// half-height — a culvert is a wide, low box, and drawing it round as wide as it
/// is wide would stand it out of the street.
pub(crate) fn push_pipe(buf: &mut MeshData, a: Vec3, b: Vec3, half: Vec2, color: [f32; 4]) {
    let axis = b - a;
    if axis.length_squared() < 1e-6 {
        return;
    }
    let dir = axis.normalize();
    let side = dir.cross(Vec3::Y);
    let right = if side.length_squared() < 1e-6 {
        Vec3::X
    } else {
        side.normalize()
    };
    let up = right.cross(dir).normalize();
    let n = PIPE_SIDES;
    let ring = |i: u32| {
        let t = (i as f32 / n as f32) * std::f32::consts::TAU;
        right * (t.cos() * half.x) + up * (t.sin() * half.y)
    };
    for i in 0..n {
        let (o0, o1) = (ring(i), ring(i + 1));
        let out = (o0 + o1).normalize();
        // Unlit, so the form is baked into the colour: lighter on top, as if lit
        // from above, which is how the eye reads a tube underground.
        let c = shade(color, 0.62 + 0.38 * (0.5 + 0.5 * out.y));
        buf.quad_facing(a + o0, a + o1, b + o1, b + o0, c, out);
    }
}

/// A vertical shaft from `top` down `depth`.
fn push_shaft(buf: &mut MeshData, at: Vec2, top: f32, depth: f32, color: [f32; 4]) {
    let n = MANHOLE_SIDES;
    let r = MANHOLE_RADIUS_M;
    for i in 0..n {
        let a0 = (i as f32 / n as f32) * std::f32::consts::TAU;
        let a1 = ((i + 1) as f32 / n as f32) * std::f32::consts::TAU;
        let (s0, c0) = a0.sin_cos();
        let (s1, c1) = a1.sin_cos();
        let out = Vec3::new((c0 + c1) * 0.5, 0.0, (s0 + s1) * 0.5);
        let c = shade(color, 0.7 + 0.3 * (0.5 + 0.5 * out.x));
        buf.quad_facing(
            Vec3::new(at.x + c0 * r, top - depth, at.y + s0 * r),
            Vec3::new(at.x + c1 * r, top - depth, at.y + s1 * r),
            Vec3::new(at.x + c1 * r, top, at.y + s1 * r),
            Vec3::new(at.x + c0 * r, top, at.y + s0 * r),
            c,
            out,
        );
    }
}

/// One built cell.
pub(crate) struct SewerCellBuild {
    grid_cell: GridCell,
    mesh: Option<Mesh>,
    pipes: usize,
}

type CellResult = Result<Option<SewerCellBuild>, String>;

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
    let (north, west) = tile_to_lat_lng(cx, cy, SEWER_ZOOM);
    let (south, east) = tile_to_lat_lng(cx + 1, cy + 1, SEWER_ZOOM);
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

    let mut buf = MeshData::default();
    let mut pipes = 0usize;
    for (kind, width, height, start, end, flat) in &cell.pipes {
        let Some(points) = wire::line_points(flat) else {
            continue;
        };
        let kind = SewerKind::from_index(*kind);
        let width = if *width == 0 {
            DEFAULT_DIAMETER_MM
        } else {
            *width
        };
        let height = if *height == 0 { width } else { *height };
        let half = Vec2::new(f32::from(width), f32::from(height)) / 2000.0;
        let (d0, d1) = invert_depths(kind, *start, *end, half.y * 2.0);
        let half = half.max(Vec2::splat(MIN_RADIUS_M));
        // The centre stands half the pipe's height over its invert.
        let lift = half.y;
        let color = linear(kind_colour(kind));

        let world: Vec<Vec2> = points.iter().map(|&p| to_world(p)).collect();
        let total: f32 = world.windows(2).map(|w| w[0].distance(w[1])).sum();
        if total <= 0.0 {
            continue;
        }
        let at = |p: Vec2, run: f32| {
            let t = (run / total).clamp(0.0, 1.0);
            let depth = d0 + (d1 - d0) * t;
            let y = ground.at(p.x, p.y) - depth + lift;
            Vec3::new(p.x - origin.x, y, p.y - origin.y)
        };
        let mut run = 0.0;
        let mut prev = at(world[0], 0.0);
        for w in world.windows(2) {
            let len = w[0].distance(w[1]);
            let steps = (len / STEP_M).ceil().max(1.0) as usize;
            for s in 1..=steps {
                let f = s as f32 / steps as f32;
                let p = w[0].lerp(w[1], f);
                let next = at(p, run + len * f);
                push_pipe(&mut buf, prev, next, half, color);
                prev = next;
            }
            run += len;
        }
        pipes += 1;
    }
    let manhole = linear(MANHOLE_RGB);
    for &(la, ln, depth) in &cell.manholes {
        let p = to_world((la, ln));
        let top = ground.at(p.x, p.y);
        push_shaft(
            &mut buf,
            p - origin,
            top,
            depth.unwrap_or(DEFAULT_MANHOLE_DEPTH_M),
            manhole,
        );
    }
    let mesh = buf.into_mesh();
    if mesh.is_none() {
        return Ok(None);
    }
    Ok(Some(SewerCellBuild {
        grid_cell,
        mesh,
        pipes,
    }))
}

async fn fetch_cell(cx: i64, cy: i64) -> Result<wire::Cell, String> {
    // Keyed on the extraction run, so a re-run is never answered from an old copy.
    let stamp = COVERAGE.stamp();
    let key = format!("sewers/{stamp}/{SEWER_ZOOM}/{cx}/{cy}.json");
    let text = match storage::get_string(storage::Namespace::Cache, &key).await {
        Some(cached) => cached,
        None => {
            let url = format!(
                "{}/sewers/{SEWER_ZOOM}/{cx}/{cy}.json?v={stamp}",
                tdbag_url()
            );
            let bytes = http::fetch(http::Request::get(url)).await?;
            let text = String::from_utf8(bytes).map_err(|e| e.to_string())?;
            storage::put(storage::Namespace::Cache, &key, text.as_bytes()).await;
            text
        }
    };
    wire::parse(&text)
}

/// The streamed sewer cells.
#[derive(Resource)]
pub struct Sewers {
    anchor: Option<(i64, i64, u32, f64)>,
    terrain_revision: u32,
    enabled_was: bool,
    cells: CellStream<(i64, i64), CellResult>,
    cell_entities: HashMap<(i64, i64), Vec<Entity>>,
    pipes: HashMap<(i64, i64), usize>,
    material: Option<Handle<StandardMaterial>>,
}

impl Default for Sewers {
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
            cell_entities: HashMap::new(),
            pipes: HashMap::new(),
            material: None,
        }
    }
}

impl crate::systems::stream_watch::StreamBusy for Sewers {
    const NAME: &'static str = "sewers";
    fn stream_busy(&self) -> bool {
        self.cells.busy()
    }
}

impl Sewers {
    /// Pipes drawn, for `--dump-state`.
    #[must_use]
    pub fn pipe_count(&self) -> usize {
        self.pipes.values().sum()
    }

    /// Spawned mesh entities, for `--dump-state`.
    #[must_use]
    pub fn meshes(&self) -> usize {
        self.cell_entities.values().map(Vec::len).sum()
    }
}

/// Is the underground layer wanted? One answer for the streamer, the ground
/// and the backdrop.
#[must_use]
pub fn wanted(layers: &crate::systems::data_layers::DataLayers) -> bool {
    layers.map_enabled && layers.sewers
}

/// Stream the sewer cells around the camera and mesh them.
#[allow(clippy::too_many_arguments)]
pub fn update_sewers(
    mut commands: Commands,
    mut sewers: ResMut<Sewers>,
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
    sewers.cells.drain_despawns(&mut commands);

    let Some((clat, clng, base_zoom)) = stream.geo_anchor() else {
        return;
    };
    let enabled = wanted(&layers);
    let turned_off = sewers.enabled_was && !enabled;
    sewers.enabled_was = enabled;
    // A pipe hangs under the terrain, so a new field moves every pipe.
    let reanchored = sewers.cells.sync(stream.anchor_generation);
    if reanchored || turned_off || sewers.terrain_revision != terrain.revision {
        sewers.terrain_revision = terrain.revision;
        let s = &mut *sewers;
        s.cells.restart(&mut s.cell_entities, &mut s.anchor);
        s.pipes.clear();
    }
    if !enabled {
        return;
    }
    let now = time.elapsed_secs_f64();
    COVERAGE.ensure(now);

    if sewers.anchor.is_none() {
        let (ax, ay) = lat_lng_to_tile(clat, clng, base_zoom);
        sewers.anchor = Some((ax, ay, base_zoom, clat));
    }
    let (ax, ay, z, lat) = sewers.anchor.expect("set just above");
    let tile_m = tile_to_meters(z, lat) as f32;

    let Some((cell, tf)) = camera.get() else {
        return;
    };
    if globe.altitude_m(cell, tf) as f32 > crate::systems::globe::GLOBE_SHOW_ALT {
        return;
    }
    let (ccx, ccy) = cell_ring::coarsen(
        cell_ring::camera_tile((ax, ay), tile_m, cell, tf),
        z,
        SEWER_ZOOM,
    );
    cell_ring::request_ring(&mut sewers.cells, (ccx, ccy), CELL_RADIUS, now, |c, _| {
        COVERAGE.covers(c.0, c.1)
    });

    let sink = sewers.cells.sink();
    while let Some((cx, cy)) = sewers.cells.next_job(now) {
        let results = sink.clone();
        let ground = terrain.snapshot();
        IoTaskPool::get()
            .spawn(async move {
                match fetch_cell(cx, cy).await {
                    Ok(parsed) => {
                        crate::platform::workers::submit_ranked(
                            "sewers",
                            crate::platform::workers::FAR,
                            Box::new(move || {
                                let built = build_cell(&parsed, cx, cy, ax, ay, z, lat, &ground);
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
        crate::systems::stream_ledger::take_admitted(&mut sewers.cells, cap, |built| match built {
            Ok(Some(b)) => b
                .mesh
                .iter()
                .map(crate::systems::stream_ledger::mesh_cost)
                .sum(),
            _ => 0,
        });
    for (cell, built) in done {
        match built {
            Ok(Some(build)) => {
                let material = sewers
                    .material
                    .get_or_insert_with(|| {
                        materials.add(StandardMaterial {
                            base_color: Color::WHITE,
                            // Underground there is no sun; the form is in the
                            // vertex colour (`push_pipe`).
                            unlit: true,
                            // No air underground to fog: three kilometres of distance fog
                            // turns a gas well into sky.
                            fog_enabled: false,
                            ..default()
                        })
                    })
                    .clone();
                let mut entities = Vec::new();
                if let Some(mesh) = build.mesh {
                    entities.push(
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
                sewers.pipes.insert(cell, build.pipes);
                sewers.cell_entities.insert(cell, entities);
                sewers.cells.succeeded(&cell);
            }
            Ok(None) => sewers.cells.succeeded(&cell),
            Err(e) => {
                let attempts = sewers.cells.failed(&cell, now);
                if sewers.cells.exhausted(&cell) {
                    tracing::debug!(?cell, attempts, error = %e, "sewer cell unavailable");
                }
            }
        }
    }

    let retired: Vec<(i64, i64)> = sewers
        .cell_entities
        .keys()
        .filter(|c| (c.0 - ccx).abs().max((c.1 - ccy).abs()) > KEEP_RADIUS)
        .copied()
        .collect();
    for cell in retired {
        if let Some(entities) = sewers.cell_entities.remove(&cell) {
            sewers.cells.retire_all(entities);
        }
        sewers.pipes.remove(&cell);
        sewers.cells.forget(&cell);
    }
}

/// Open the ground while the layer is on: switch every ground and road material
/// to the screen-door variant (`ground_detail::GroundKey`), and back.
///
/// Every material when the switch flips; after that, only the ones whose asset
/// events say they are new or were rewritten. **Not a count of materials**: the
/// first version guarded on `len()`, and a clipmap tile that swaps one material
/// for another leaves the count where it was — that tile then stayed solid, and
/// every hole above it opened onto it instead of onto the backdrop. Rewritten
/// ones are read too, because code that rebuilds an extension wholesale puts the
/// flag back to its default.
pub fn apply_ground_see_through(
    layers: Res<crate::systems::data_layers::DataLayers>,
    mut ground: ResMut<Assets<crate::systems::ground_detail::GroundMaterial>>,
    mut roads: ResMut<Assets<crate::systems::road_depth::RoadMaterial>>,
    mut ground_events: MessageReader<AssetEvent<crate::systems::ground_detail::GroundMaterial>>,
    mut road_events: MessageReader<AssetEvent<crate::systems::road_depth::RoadMaterial>>,
    mut applied: Local<Option<bool>>,
) {
    let on = crate::systems::underground::opens_ground(&layers);
    let touched = |e: &AssetEvent<_>| match e {
        AssetEvent::Added { id } | AssetEvent::Modified { id } => Some(*id),
        _ => None,
    };
    let fresh_ground: Vec<_> = ground_events.read().filter_map(touched).collect();
    let fresh_roads: Vec<_> = road_events
        .read()
        .filter_map(|e| match e {
            AssetEvent::Added { id } | AssetEvent::Modified { id } => Some(*id),
            _ => None,
        })
        .collect();
    let flipped = *applied != Some(on);
    *applied = Some(on);
    let ground_ids: Vec<_> = if flipped {
        ground.ids().collect()
    } else {
        fresh_ground
    };
    for id in ground_ids {
        // Read first: writing a material that is already right re-prepares it.
        let Some(m) = ground.get(id) else { continue };
        // The volumetric groups opt out of the ground's look for good
        // (`ground_detail::DETAIL_OPTED_OUT`); a tree is not a pavement.
        if m.extension.params.w < 0.0 || m.extension.see_through == on {
            continue;
        }
        if let Some(mut m) = ground.get_mut(id) {
            m.extension.see_through = on;
        }
    }
    let road_ids: Vec<_> = if flipped {
        roads.ids().collect()
    } else {
        fresh_roads
    };
    for id in road_ids {
        if roads.get(id).is_some_and(|m| m.extension.see_through != on) {
            if let Some(mut m) = roads.get_mut(id) {
                m.extension.see_through = on;
            }
        }
    }
}

/// The soil the opening looks into.
#[derive(Component)]
pub struct Backdrop;

/// Where the backdrop was put this frame, for `--dump-state`: a backdrop that
/// is not drawn leaves the opening showing the sky pass's planet ground, which
/// looks like soil from a distance and is not.
#[derive(Resource, Default, Clone, Copy, Debug)]
pub struct BackdropState {
    pub spawned: bool,
    /// Whether it passed the CPU-side culling last frame — false with
    /// `spawned` true means something culled it, not that it is hidden behind
    /// the ground.
    pub visible: bool,
    pub depth_m: f32,
    pub half_m: f32,
}

/// Keep the backdrop under the camera while the layer is on.
#[allow(clippy::too_many_arguments)]
pub fn follow_backdrop(
    mut commands: Commands,
    layers: Res<crate::systems::data_layers::DataLayers>,
    camera: AnchoredCamera,
    terrain: Res<crate::systems::terrain::TerrainField>,
    bigspace: Query<Entity, With<BigSpace>>,
    // `Without<FreeCamera>`: `camera` reads a `Transform` and a `GridCell` too,
    // and Bevy refuses two queries it cannot prove disjoint (B0001).
    mut backdrop: Query<
        (Entity, &mut Transform, &mut GridCell, &ViewVisibility),
        (With<Backdrop>, Without<crate::components::FreeCamera>),
    >,
    mut meshes: ResMut<Assets<Mesh>>,
    mut materials: ResMut<Assets<StandardMaterial>>,
    under: Res<crate::systems::underground::Underground>,
    mut state: ResMut<BackdropState>,
    mut skirt: Query<
        &mut Transform,
        (
            With<crate::components::GroundSkirt>,
            Without<Backdrop>,
            Without<crate::components::FreeCamera>,
        ),
    >,
) {
    let on = crate::systems::underground::opens_ground(&layers);
    let existing = backdrop.single_mut().ok();
    state.spawned = existing.is_some();
    state.visible = existing.as_ref().is_some_and(|b| b.3.get());
    // The paper-toned skirt that carries the ground past the last tile is a
    // plain opaque sheet 25 m down (`free_camera::GROUND_SKIRT_SINK`). It does
    // not take the screen door, so anything deeper than it — the backdrop, a
    // soil column's foot, every gas well — was hidden behind it, and the opening
    // showed blank paper with the street's shadows on it. Measured 2026-10-08:
    // a backdrop at 14 m showed and one at 280 m did not. While the ground is
    // open the skirt goes under the backdrop; it is the ground *past the tiles*,
    // kilometres off and fogged, so its depth there is not something one sees.
    let skirt_y = if on {
        -(crate::systems::free_camera::GROUND_SKIRT_SINK.max(state.depth_m + SKIRT_CLEARANCE_M))
    } else {
        -crate::systems::free_camera::GROUND_SKIRT_SINK
    };
    for mut t in &mut skirt {
        if t.translation.y != skirt_y {
            t.translation.y = skirt_y;
        }
    }
    if !on {
        if let Some((e, ..)) = existing {
            commands.entity(e).try_despawn();
        }
        return;
    }
    let Some((cell, tf)) = camera.get() else {
        return;
    };
    let world = crate::grid::anchored_xz(cell, tf.translation);
    // Under the deepest thing loaded — a sewer, or a gas well three kilometres
    // down — and wider the deeper it lies, so its edge stays out of a view
    // that looks down that far.
    let depth = BACKDROP_DEPTH_M.max(under.deepest_m() + BACKDROP_MARGIN_M);
    let half = BACKDROP_HALF_M.max(depth * 1.5);
    let y = terrain.ground_at(world.x, world.y) - depth;
    state.depth_m = depth;
    state.half_m = half;
    let scale = Vec3::splat(half);
    // Same cell as the camera in x and z, the bottom row in y, so the sheet is
    // never more than a cell's float error away from where it is drawn.
    let at = GridCell::new(cell.x, 0, cell.z);
    let translation = Vec3::new(tf.translation.x, y, tf.translation.z);
    match existing {
        Some((_, mut t, mut g, _)) => {
            if t.translation != translation {
                t.translation = translation;
            }
            if t.scale != scale {
                t.scale = scale;
            }
            if *g != at {
                *g = at;
            }
        }
        None => {
            let Ok(root) = bigspace.single() else {
                return;
            };
            // A unit sheet, sized by the transform's scale.
            let mesh = Plane3d::new(Vec3::Y, Vec2::splat(1.0)).mesh().build();
            let [r, g, b, _] = linear(BACKDROP_RGB);
            commands.spawn((
                Backdrop,
                Mesh3d(meshes.add(mesh)),
                MeshMaterial3d(materials.add(StandardMaterial {
                    base_color: Color::linear_rgb(r, g, b),
                    unlit: true,
                    // No air underground to fog: three kilometres of distance fog
                    // turns a gas well into sky.
                    fog_enabled: false,
                    ..default()
                })),
                Transform::from_translation(translation).with_scale(scale),
                at,
                ChildOf(root),
                crate::components::HideInSpace,
                bevy::light::NotShadowCaster,
            ));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_missing_end_takes_the_other_and_both_take_the_default() {
        assert_eq!(
            invert_depths(SewerKind::Combined, Some(1.5), Some(2.0), 0.3),
            (1.5, 2.0)
        );
        assert_eq!(
            invert_depths(SewerKind::Combined, None, Some(2.0), 0.3),
            (2.0, 2.0)
        );
        assert_eq!(
            invert_depths(SewerKind::Storm, None, None, 0.3),
            (DEFAULT_DEPTH_M, DEFAULT_DEPTH_M)
        );
        assert_eq!(
            invert_depths(SewerKind::Pressure, None, None, 0.3),
            (DEFAULT_PRESSURE_DEPTH_M, DEFAULT_PRESSURE_DEPTH_M)
        );
    }

    /// Groningen's unmeasured 2 m culverts, and a measured end 0.1 m under the
    /// street over a 315 mm pipe (both in the published cells, 2026-10-08):
    /// neither may put a crown above the ground.
    #[test]
    fn no_crown_comes_out_of_the_street() {
        for (start, end, height) in [
            (None, None, 2.0),
            (Some(1.79), Some(0.1), 0.315),
            (None, Some(4.43), 2.2),
        ] {
            let (a, b) = invert_depths(SewerKind::Combined, start, end, height);
            for d in [a, b] {
                assert!(
                    d - height >= MIN_COVER_M - 1e-6,
                    "{start:?} {end:?} {height}"
                );
            }
        }
    }

    #[test]
    fn every_kind_has_its_own_colour() {
        let mut seen = std::collections::HashSet::new();
        for k in SewerKind::ALL {
            assert!(seen.insert(kind_colour(k)), "{k:?}");
        }
    }

    /// A pipe is drawn its depth under flat ground, with its centre half its
    /// height over the invert, and a long one is cut into short pieces.
    #[test]
    fn a_pipe_hangs_under_the_ground_at_its_depth() {
        let (ax, ay) = lat_lng_to_tile(53.2194, 6.5665, SEWER_ZOOM);
        let cell = wire::Cell {
            manholes: vec![],
            pipes: vec![(
                SewerKind::Combined.index(),
                400,
                400,
                Some(2.0),
                Some(2.0),
                vec![53.2194, 6.5665, 53.2194, 6.5675],
            )],
        };
        let built = build_cell(
            &cell,
            ax,
            ay,
            ax,
            ay,
            SEWER_ZOOM,
            53.2194,
            &TerrainSnapshot::default(),
        )
        .unwrap()
        .expect("a pipe");
        assert_eq!(built.pipes, 1);
        let mesh = built.mesh.unwrap();
        let Some(bevy::mesh::VertexAttributeValues::Float32x3(pos)) =
            mesh.attribute(Mesh::ATTRIBUTE_POSITION)
        else {
            panic!("positions");
        };
        let (lo, hi) = pos.iter().fold((f32::MAX, f32::MIN), |(lo, hi), p| {
            (lo.min(p[1]), hi.max(p[1]))
        });
        // Centre at −2.0 + 0.2, radius 0.2: from −2.0 to −1.6.
        assert!((lo - -2.0).abs() < 0.01, "{lo}");
        assert!((hi - -1.6).abs() < 0.01, "{hi}");
        // ~67 m long in steps of at most 8 m: at least nine pieces of eight sides.
        assert!(pos.len() >= 9 * PIPE_SIDES as usize * 4, "{}", pos.len());
    }
}
