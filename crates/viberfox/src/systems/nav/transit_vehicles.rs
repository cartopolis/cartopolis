//! Live moving vehicles for the transit data layer (ADR-026, companion to the
//! departure boards of ADR-024).
//!
//! Where [`crate::systems::transit_live`] shows *where the stops are and when
//! the next bus leaves*, this module shows *where the buses are right now*: it
//! polls OVapi's open GTFS-Realtime vehicle-position feed
//! (`https://gtfs.ovapi.nl/nl/vehiclePositions.pb` — one small protobuf
//! covering all live Dutch vehicles), converts each fix to a world position via
//! the shared map anchor (like the POI pins), and drives a coloured box mesh per
//! vehicle that glides between successive fixes.
//!
//! Rendering is deliberately a placeholder: one shared cuboid per vehicle,
//! aimed along its travel direction (the feed carries no bearing, so heading is
//! derived from motion), tinted by transport mode, with the public line number
//! on a billboarded label above it. A real bus model is future work.
//!
//! Line numbers & mode: the feed's `route_id` is an opaque number, so the
//! worker fetches OVapi's `/line/` list once (disk-cached) to map the operator
//! line-planning number carried in the vehicle id (`…:GVB:293:…`) to the public
//! line (`N93`) and its `TransportType`. See [`load_line_map`].
//!
//! Protobuf: rather than pull in `prost` + a generated GTFS-Realtime schema,
//! this reads the handful of fields it needs (entity id, position lat/lng,
//! timestamp) with a small hand-rolled wire reader — matching the codebase's
//! other hand-rolled parsers (the naive-timestamp code in `transit_live`). See
//! [`parse_vehicle_positions`].
//!
//! Politeness: one request per [`POLL_INTERVAL`] on a worker thread, gated on
//! the transit layer being on, the camera being near the ground, and vehicles
//! being in range — the same discipline the boards and building streaming use.

use crate::components::FreeCamera;
use crate::components::{VehicleBox, VehicleLabel};
use std::collections::{HashMap, HashSet};
use std::sync::{Arc, Mutex};
use std::time::Duration;
use web_time::Instant;

use bevy::asset::RenderAssetUsages;
use bevy::light::{NotShadowCaster, NotShadowReceiver};
use bevy::prelude::*;
use bevy::render::render_resource::{Extent3d, TextureDimension, TextureFormat};
use bevy::tasks::IoTaskPool;
use big_space::prelude::{BigSpace, Grid, GridCell};

use crate::platform::{http, storage};
use crate::systems::data_layers::{DataLayers, TRANSIT_LAYER};
use crate::systems::free_camera::{WORLD_CELL_EDGE, WORLD_SWITCH_THRESHOLD};
use crate::systems::globe::GlobeState;
use crate::systems::map_stream::MapStream;
use crate::systems::osm_buildings::lat_lng_to_local;
use crate::systems::transit_live::WorldClock;
use viberfox_core::world::tile_to_meters;

/// OVapi's GTFS-Realtime vehicle-position feed (all live Dutch vehicles).
const FEED_URL: &str = "https://gtfs.ovapi.nl/nl/vehiclePositions.pb";
/// Refresh cadence; also the interval a vehicle glides across between fixes.
const POLL_INTERVAL: Duration = Duration::from_secs(15);
/// Vehicles within this range of the camera get a box (metres).
const SHOW_RANGE: f32 = 3_000.0;
/// At most this many boxes in the world (nearest first).
const MAX_VEHICLES: usize = 80;
/// Max new vehicle boxes + labels to create per frame. Toggling the layer on (or
/// re-anchoring) lands the whole feed at once; spawning every box and painting +
/// uploading every 128×64 label texture in a single frame packs one giant command
/// buffer that overran a weak GPU's timeout (`VK_ERROR_DEVICE_LOST`) — the same
/// mass-upload burst the tile/globe build caps guard against. The rest follow on
/// later frames (existing vehicles keep updating meanwhile).
const MAX_VEHICLE_SPAWNS_PER_FRAME: usize = 6;
/// Drop a vehicle not seen in the feed for this long (finished its trip / gone).
const VEHICLE_TTL: Duration = Duration::from_secs(90);
/// Ignore a fix whose GPS timestamp is older than this (stale ghosts linger in
/// the feed; some carry trip dates days in the past).
const STALE_REPORT_SECS: i64 = 600;
/// Freeze streaming above this camera altitude — these are surface props,
/// meaningless from the air (mirrors the data-layer ceiling).
const MAX_ALT: f64 = 20_000.0;
/// A jump larger than this between fixes snaps instead of gliding (GPS glitch,
/// or the id was reused for a new trip on the far side of town).
const SNAP_DIST: f32 = 400.0;
/// Only re-aim a vehicle once it has travelled at least this far between fixes;
/// below it, keep the last heading (the feed carries no bearing, so heading is
/// derived from motion and jitter at a standstill would spin the box).
const HEADING_MIN_MOVE: f32 = 3.0;

/// Placeholder bus box (metres): a 12 m standard-length bus, front along +Z.
const BUS_LEN: f32 = 12.0;
const BUS_WIDTH: f32 = 2.55;
const BUS_HEIGHT: f32 = 3.1;
/// Line-number label size in the world (metres) and its height above the box.
const LABEL_W: f32 = 3.0;
const LABEL_H: f32 = 1.5;
const LABEL_LIFT: f32 = 2.2;

/// OVapi's line list — a map of every line's public number + mode.
const LINES_URL: &str = "http://v0.ovapi.nl/line/";
/// Disk cache for the line list (under the shared cache root); refetched when
/// older than [`LINES_TTL`].
const LINES_CACHE: [&str; 2] = ["transit", "lines.json"];
const LINES_TTL: Duration = Duration::from_secs(24 * 3600);

/// Transport mode, from OVapi's `TransportType` — drives the box/label colour.
#[derive(Clone, Copy, PartialEq, Debug)]
enum Transport {
    Bus,
    Tram,
    Metro,
    Train,
}

impl Transport {
    /// Map an OVapi `TransportType` string; unknown modes read as a bus.
    fn from_type(s: &str) -> Transport {
        match s {
            "TRAM" => Transport::Tram,
            "METRO" | "SUBWAY" => Transport::Metro,
            "TRAIN" => Transport::Train,
            _ => Transport::Bus,
        }
    }

    /// Category colour (same hues as the transit-layer stop categories).
    fn color(self) -> [u8; 3] {
        match self {
            Transport::Bus => [242, 168, 31],
            Transport::Tram => [41, 153, 77],
            Transport::Metro => [217, 51, 51],
            Transport::Train => [38, 102, 217],
        }
    }
}

/// Public line number + mode for a line-planning number, from OVapi's `/line/`.
type LineMap = HashMap<String, (Option<String>, Transport)>;

/// One vehicle fix parsed off the feed (absolute lat/lng — anchor-independent,
/// so it survives a re-anchor and is converted to the world frame at fold time).
/// The feed carries no bearing/speed, so heading is derived from motion; the
/// public line + mode are resolved from the [`LineMap`] on the worker.
struct VehicleReport {
    id: String,
    lat: f64,
    lng: f64,
    /// Fix time (unix seconds) if the feed reported one.
    ts: Option<i64>,
    /// Public line number (`N93`, `227`), if the planning number resolved.
    line: Option<String>,
    transport: Transport,
}

/// A finished feed fetch: the vehicles parsed off it, or an error string.
type FetchResult = Result<Vec<VehicleReport>, String>;

/// A tracked vehicle: the segment it is gliding along in the flat world frame,
/// plus its heading and class. `from`/`to` are metres in the same continuous
/// frame `lat_lng_to_local` produces, so they are only valid for the current
/// anchor generation (cleared on re-anchor).
struct VehicleState {
    from: Vec2,
    to: Vec2,
    /// Unix seconds when `to` was received; glide runs over [`POLL_INTERVAL`].
    t0: f64,
    /// Heading as a Bevy Y rotation (radians), derived from the travel
    /// direction between fixes (the feed carries no bearing).
    yaw: f32,
    transport: Transport,
    /// Public line number for the label, if it resolved.
    line: Option<String>,
    last_seen: Instant,
}

/// Live vehicle polling state, tracked vehicles, and their spawned boxes.
#[derive(Resource, Default)]
pub struct TransitVehicles {
    /// OVapi line list (public number + mode per line), loaded once and shared by
    /// every feed fetch. `Arc` so a fetch task can hold it without re-loading.
    lines: Arc<Mutex<Option<Arc<LineMap>>>>,
    results: Arc<Mutex<Vec<FetchResult>>>,
    in_flight: bool,
    last_poll: Option<Instant>,
    /// Anchor generation the flat `from`/`to` coords belong to.
    synced_gen: u32,
    vehicles: HashMap<String, VehicleState>,
    entities: HashMap<String, Entity>,
    /// Shared cuboid (all vehicles) and one material per transport class.
    mesh: Option<Handle<Mesh>>,
    materials: HashMap<u8, Handle<StandardMaterial>>,
    /// Line-number labels: one entity per shown vehicle, a shared quad, and one
    /// painted texture-material per (mode, line) shared across buses on a line.
    labels: HashMap<String, Entity>,
    label_mesh: Option<Handle<Mesh>>,
    label_mats: HashMap<String, Handle<StandardMaterial>>,
    painter: Option<LabelPainter>,
}

/// Interpolated flat-frame position of a vehicle at time `now`.
fn interp(s: &VehicleState, now: f64) -> Vec2 {
    let a = ((now - s.t0) as f32 / POLL_INTERVAL.as_secs_f32()).clamp(0.0, 1.0);
    s.from.lerp(s.to, a)
}

/// Bevy Y-rotation that aims the box front (+Z) along a flat-frame travel
/// vector `d` (`d.x` = east/+X, `d.y` = the world −Z... i.e. the map z axis).
/// `from_rotation_y(yaw) * +Z = (sin yaw, cos yaw)`, so `yaw = atan2(d.x, d.y)`.
fn heading_yaw(d: Vec2) -> f32 {
    d.x.atan2(d.y)
}

/// Poll OVapi's vehicle feed on a worker thread and fold fixes into `vehicles`.
/// Absolute lat/lng are converted to the flat world frame here (needs the
/// shared map anchor); a re-anchor clears the frame-relative track first.
pub fn poll_transit_vehicles(
    mut tv: ResMut<TransitVehicles>,
    clock: Res<WorldClock>,
    dl: Res<DataLayers>,
    globe: Res<GlobeState>,
    stream: Res<MapStream>,
    camera: Query<(&GridCell, &Transform), With<FreeCamera>>,
) {
    // Re-anchor: the flat frame moved, so the frame-relative track is stale.
    // In-flight/queued fixes are absolute lat/lng and re-fold correctly.
    if tv.synced_gen != stream.anchor_generation {
        tv.synced_gen = stream.anchor_generation;
        tv.vehicles.clear();
    }

    // Without the anchor there is no flat frame to fold into yet; leave any
    // completed fetches queued for a later frame.
    let Some((ax, ay, z, lat)) = dl.anchor() else {
        return;
    };
    let tile_m = tile_to_meters(z, lat) as f32;
    let now = clock.now_unix;
    let now_i = now as i64;

    // Fold in completed fetches.
    let done: Vec<FetchResult> = tv
        .results
        .lock()
        .map(|mut g| g.drain(..).collect())
        .unwrap_or_default();
    for result in done {
        tv.in_flight = false;
        match result {
            Ok(reports) => {
                for report in reports {
                    if report.ts.is_some_and(|ts| now_i - ts > STALE_REPORT_SECS) {
                        continue;
                    }
                    let flat = lat_lng_to_local(report.lat, report.lng, ax, ay, z, tile_m);
                    match tv.vehicles.get_mut(&report.id) {
                        Some(s) => {
                            // Glide from where it is shown now to the new fix,
                            // unless the jump is too big to be real motion.
                            let cur = interp(s, now);
                            s.from = if cur.distance(flat) > SNAP_DIST {
                                flat
                            } else {
                                cur
                            };
                            s.to = flat;
                            s.t0 = now;
                            // Re-aim along the travel direction (only once it has
                            // actually moved; else keep the last heading).
                            let d = s.to - s.from;
                            if d.length_squared() > HEADING_MIN_MOVE * HEADING_MIN_MOVE {
                                s.yaw = heading_yaw(d);
                            }
                            s.transport = report.transport;
                            s.line = report.line;
                            s.last_seen = Instant::now();
                        }
                        None => {
                            tv.vehicles.insert(
                                report.id,
                                VehicleState {
                                    from: flat,
                                    to: flat,
                                    t0: now,
                                    yaw: 0.0,
                                    transport: report.transport,
                                    line: report.line,
                                    last_seen: Instant::now(),
                                },
                            );
                        }
                    }
                }
            }
            Err(e) => tracing::warn!(error = %e, "vehicle feed fetch failed"),
        }
    }
    tv.vehicles
        .retain(|_, s| s.last_seen.elapsed() < VEHICLE_TTL);

    // Decide whether to poll now.
    if tv.in_flight || !(dl.layers[TRANSIT_LAYER].enabled && dl.vehicles_enabled) {
        return;
    }
    let Ok((cell, tf)) = camera.single() else {
        return;
    };
    if globe.altitude_m(cell, tf) > MAX_ALT {
        return;
    }
    if tv.last_poll.is_some_and(|t| t.elapsed() < POLL_INTERVAL) {
        return;
    }
    tv.last_poll = Some(Instant::now());
    tv.in_flight = true;
    // Detached fetch (ADR-033 seam): the line map loads once into the shared slot
    // on the first fetch, then every feed fetch reuses it. `in_flight` serialises
    // these, so there is no double-load race.
    let results = tv.results.clone();
    let lines = tv.lines.clone();
    // Two halves on two executors (ADR-033): both bodies are awaited here — free
    // on wasm, where this is the main thread but the browser does the transfer —
    // while the GTFS-RT protobuf decode and the `serde_json` line-list parse go to
    // `platform::workers`. Every `POLL_INTERVAL`, on the frame loop, is what they
    // used to be.
    IoTaskPool::get()
        .spawn(async move {
            let have_lines = lines.lock().is_ok_and(|g| g.is_some());
            let line_body = if have_lines {
                None
            } else {
                Some(load_line_body().await)
            };
            let feed = fetch_feed_bytes().await;
            crate::platform::workers::submit(Box::new(move || {
                let map = resolve_line_map(&lines, line_body);
                let fetched = feed.map(|buf| parse_vehicle_positions(&buf, &map));
                if let Ok(mut g) = results.lock() {
                    g.push(fetched);
                }
            }));
        })
        .detach();
}

/// The shared line map: parse `line_body` into it on first use, else read the slot
/// the earlier poll filled. `in_flight` serialises the polls, so there is no
/// double-load race here.
fn resolve_line_map(
    slot: &Arc<Mutex<Option<Arc<LineMap>>>>,
    line_body: Option<Option<String>>,
) -> Arc<LineMap> {
    if let Some(body) = line_body {
        let map = Arc::new(body.as_deref().map(parse_line_map).unwrap_or_default());
        if let Ok(mut g) = slot.lock() {
            *g = Some(map.clone());
        }
        return map;
    }
    slot.lock().ok().and_then(|g| g.clone()).unwrap_or_default()
}

/// Spawn / move / retire the vehicle boxes and their line-number labels:
/// interpolate each tracked vehicle to this frame, keep the nearest
/// [`MAX_VEHICLES`] within range, drive one oriented box per vehicle, and hang a
/// camera-facing line label above it. Bounded — no per-frame asset churn (label
/// textures are painted once per line and shared).
#[allow(clippy::too_many_arguments)]
pub fn update_transit_vehicles(
    mut commands: Commands,
    mut tv: ResMut<TransitVehicles>,
    clock: Res<WorldClock>,
    dl: Res<DataLayers>,
    globe: Res<GlobeState>,
    mut meshes: ResMut<Assets<Mesh>>,
    mut materials: ResMut<Assets<StandardMaterial>>,
    mut images: ResMut<Assets<Image>>,
    camera: Query<(&GridCell, &Transform), With<FreeCamera>>,
    bigspace: Query<Entity, With<BigSpace>>,
    mut boxes: Query<(&mut Transform, &mut GridCell), (With<VehicleBox>, Without<FreeCamera>)>,
    mut labels: Query<
        (&mut Transform, &mut GridCell),
        (With<VehicleLabel>, Without<VehicleBox>, Without<FreeCamera>),
    >,
    terrain: Res<crate::systems::terrain::TerrainField>,
) {
    let tv = tv.as_mut();

    // Hide everything when the layer is off, we are in space, or there is no
    // camera — despawn is fine, the set is small and bounded.
    let cam = camera.single().ok();
    let layer_on = dl.layers[TRANSIT_LAYER].enabled && dl.vehicles_enabled;
    let hidden = !layer_on || cam.is_none_or(|(c, t)| globe.in_space(c, t));
    if hidden {
        for (_, e) in tv.entities.drain().chain(tv.labels.drain()) {
            commands.entity(e).despawn();
        }
        return;
    }
    let (cam_cell, cam_tf) = cam.unwrap();
    let Ok(root) = bigspace.single() else {
        return;
    };
    let now = clock.now_unix;

    // Camera position in the flat world frame.
    let cam_flat = Vec2::new(
        cam_cell.x as f32 * WORLD_CELL_EDGE + cam_tf.translation.x,
        cam_cell.z as f32 * WORLD_CELL_EDGE + cam_tf.translation.z,
    );

    // Nearest vehicles in range, this frame's interpolated positions.
    let mut target: Vec<(f32, String, Vec2, f32, Transport, Option<String>)> = Vec::new();
    for (id, s) in tv.vehicles.iter() {
        let pos = interp(s, now);
        let dist = (pos - cam_flat).length();
        if dist <= SHOW_RANGE {
            target.push((dist, id.clone(), pos, s.yaw, s.transport, s.line.clone()));
        }
    }
    target.sort_by(|a, b| a.0.total_cmp(&b.0));
    target.truncate(MAX_VEHICLES);
    let target_ids: HashSet<&String> = target.iter().map(|t| &t.1).collect();

    // Retire boxes + labels that fell out of the target set.
    let stale: Vec<String> = tv
        .entities
        .keys()
        .filter(|id| !target_ids.contains(id))
        .cloned()
        .collect();
    for id in stale {
        if let Some(e) = tv.entities.remove(&id) {
            commands.entity(e).despawn();
        }
        if let Some(e) = tv.labels.remove(&id) {
            commands.entity(e).despawn();
        }
    }

    let mesh = tv
        .mesh
        .get_or_insert_with(|| meshes.add(Cuboid::new(BUS_WIDTH, BUS_HEIGHT, BUS_LEN)))
        .clone();
    let label_mesh = tv
        .label_mesh
        .get_or_insert_with(|| meshes.add(Rectangle::new(LABEL_W, LABEL_H)))
        .clone();
    let grid = Grid::new(WORLD_CELL_EDGE, WORLD_SWITCH_THRESHOLD);

    // Per-frame budget for new box/label creation (paints + uploads), so a feed
    // that lands 80 vehicles at once spreads over frames instead of one burst.
    let mut spawn_budget = MAX_VEHICLE_SPAWNS_PER_FRAME;
    for (_, id, pos, yaw, transport, line) in &target {
        // On the measured ground: a bus is a thing standing in the street, and the
        // street has moved.
        let ground = terrain.at(pos.x, pos.y);
        let (cell, local) = grid.translation_to_grid(Vec3::new(pos.x, ground, pos.y).as_dvec3());
        let translation = local + Vec3::new(0.0, BUS_HEIGHT * 0.5, 0.0);
        let rotation = Quat::from_rotation_y(*yaw);
        if let Some(&e) = tv.entities.get(id) {
            if let Ok((mut tf, mut gc)) = boxes.get_mut(e) {
                tf.translation = translation;
                tf.rotation = rotation;
                *gc = cell;
            }
        } else if spawn_budget > 0 {
            spawn_budget -= 1;
            let material = tv
                .materials
                .entry(*transport as u8)
                .or_insert_with(|| {
                    let c = transport.color();
                    materials.add(StandardMaterial {
                        base_color: Color::srgb_u8(c[0], c[1], c[2]),
                        perceptual_roughness: 0.7,
                        ..default()
                    })
                })
                .clone();
            let e = commands
                .spawn((
                    Mesh3d(mesh.clone()),
                    MeshMaterial3d(material),
                    Transform {
                        translation,
                        rotation,
                        ..default()
                    },
                    cell,
                    ChildOf(root),
                    VehicleBox,
                    NotShadowCaster,
                    NotShadowReceiver,
                ))
                .id();
            tv.entities.insert(id.clone(), e);
        } else {
            // Budget spent and no box yet — defer this vehicle (and its label) to
            // a later frame rather than bursting the command buffer.
            continue;
        }

        // Line-number label: a camera-facing quad above the box. Vehicles with
        // no resolved line get none.
        let Some(line) = line else { continue };
        let lbl_translation = local + Vec3::new(0.0, BUS_HEIGHT + LABEL_LIFT, 0.0);
        // Billboard toward the camera (yaw + pitch), like the departure boards.
        let dx = (cam_cell.x - cell.x) as f32 * WORLD_CELL_EDGE + cam_tf.translation.x
            - lbl_translation.x;
        let dy = (cam_cell.y - cell.y) as f32 * WORLD_CELL_EDGE + cam_tf.translation.y
            - lbl_translation.y;
        let dz = (cam_cell.z - cell.z) as f32 * WORLD_CELL_EDGE + cam_tf.translation.z
            - lbl_translation.z;
        let pitch = dy.atan2((dx * dx + dz * dz).sqrt());
        let lbl_rot = Quat::from_rotation_y(dx.atan2(dz)) * Quat::from_rotation_x(-pitch);

        if let Some(&e) = tv.labels.get(id) {
            if let Ok((mut tf, mut gc)) = labels.get_mut(e) {
                tf.translation = lbl_translation;
                tf.rotation = lbl_rot;
                *gc = cell;
            }
        } else if spawn_budget > 0 {
            spawn_budget -= 1;
            // Paint once per (mode, line); shared by every bus on that line.
            let key = format!("{}:{line}", *transport as u8);
            let painter = tv.painter.get_or_insert_with(LabelPainter::new);
            let material = tv
                .label_mats
                .entry(key)
                .or_insert_with(|| {
                    let image = images.add(painter.paint(line, transport.color()));
                    materials.add(StandardMaterial {
                        base_color_texture: Some(image),
                        unlit: true,
                        ..default()
                    })
                })
                .clone();
            let e = commands
                .spawn((
                    Mesh3d(label_mesh.clone()),
                    MeshMaterial3d(material),
                    Transform {
                        translation: lbl_translation,
                        rotation: lbl_rot,
                        ..default()
                    },
                    cell,
                    ChildOf(root),
                    VehicleLabel,
                    NotShadowCaster,
                    NotShadowReceiver,
                ))
                .id();
            tv.labels.insert(id.clone(), e);
        }
    }
}

// --- Worker-thread line list + feed fetch + GTFS-Realtime protobuf parse ---

/// Fetch and parse the vehicle-position feed (raw protobuf bytes over HTTP),
/// resolving each vehicle's public line + mode against the line list.
async fn fetch_feed_bytes() -> Result<Vec<u8>, String> {
    http::fetch(http::Request::get(FEED_URL).timeout(Duration::from_secs(15))).await
}

/// OVapi's line list **as text** (public number + `TransportType` per
/// line-planning number), from the disk cache when fresh, else fetched once and
/// cached. `None` means neither worked — vehicles then show without labels, as
/// buses. The `serde_json` parse of it happens on a worker, not here.
async fn load_line_body() -> Option<String> {
    let key = LINES_CACHE.join("/");
    let fresh = storage::age_secs(&key)
        .await
        .is_some_and(|age| age < LINES_TTL.as_secs_f64());
    let body = if fresh {
        storage::get_string(&key).await
    } else {
        match fetch_line_list().await {
            Ok(text) => {
                storage::put(&key, text.as_bytes()).await;
                Some(text)
            }
            // On a fetch error, fall back to a stale cache if we have one.
            Err(e) => {
                tracing::warn!(error = %e, "OVapi line list fetch failed");
                storage::get_string(&key).await
            }
        }
    };
    body
}

/// GET the OVapi `/line/` JSON. Plain `http://`, so the web build reaches it via
/// the relay (ADR-034), not directly.
async fn fetch_line_list() -> Result<String, String> {
    http::fetch_string(http::Request::get(LINES_URL).timeout(Duration::from_secs(30))).await
}

/// Parse the `/line/` object into a planning-number → (public number, mode) map.
fn parse_line_map(text: &str) -> LineMap {
    let mut map = LineMap::new();
    let Ok(json) = serde_json::from_str::<serde_json::Value>(text) else {
        return map;
    };
    let Some(obj) = json.as_object() else {
        return map;
    };
    for line in obj.values() {
        let s = |k: &str| line.get(k).and_then(|v| v.as_str());
        let Some(planning) = s("LinePlanningNumber") else {
            continue;
        };
        let public = s("LinePublicNumber").map(str::to_string);
        let transport = Transport::from_type(s("TransportType").unwrap_or("BUS"));
        map.insert(planning.to_string(), (public, transport));
    }
    map
}

/// A minimal, tolerant protobuf field reader over a byte slice — just enough of
/// the wire format to walk the GTFS-Realtime `FeedMessage`. Unknown fields are
/// skipped; a malformed field ends the current message rather than panicking.
struct Buf<'a> {
    b: &'a [u8],
    i: usize,
}

impl<'a> Buf<'a> {
    fn new(b: &'a [u8]) -> Self {
        Self { b, i: 0 }
    }

    fn byte(&mut self) -> Option<u8> {
        let v = *self.b.get(self.i)?;
        self.i += 1;
        Some(v)
    }

    /// Base-128 varint.
    fn varint(&mut self) -> Option<u64> {
        let mut out = 0u64;
        let mut shift = 0u32;
        loop {
            let b = self.byte()?;
            out |= ((b & 0x7f) as u64) << shift;
            if b & 0x80 == 0 {
                return Some(out);
            }
            shift += 7;
            if shift >= 64 {
                return None;
            }
        }
    }

    /// A field tag → (field number, wire type).
    fn tag(&mut self) -> Option<(u64, u8)> {
        let t = self.varint()?;
        Some((t >> 3, (t & 7) as u8))
    }

    /// A length-delimited field's bytes.
    fn bytes(&mut self) -> Option<&'a [u8]> {
        let len = self.varint()? as usize;
        let s = self.b.get(self.i..self.i.checked_add(len)?)?;
        self.i += len;
        Some(s)
    }

    /// A 32-bit fixed field as f32 (little-endian, wire type 5).
    fn f32(&mut self) -> Option<f32> {
        let s = self.b.get(self.i..self.i + 4)?;
        self.i += 4;
        Some(f32::from_le_bytes([s[0], s[1], s[2], s[3]]))
    }

    /// Consume and discard one field of the given wire type. Returns `None` on a
    /// truncated / unknown wire type so callers can stop cleanly.
    fn skip(&mut self, wire: u8) -> Option<()> {
        match wire {
            0 => {
                self.varint()?;
            }
            1 => {
                self.b.get(self.i..self.i + 8)?;
                self.i += 8;
            }
            2 => {
                self.bytes()?;
            }
            5 => {
                self.b.get(self.i..self.i + 4)?;
                self.i += 4;
            }
            _ => return None,
        }
        Some(())
    }
}

/// Walk a GTFS-Realtime `FeedMessage`, returning a report per entity that
/// carries a vehicle position. Field numbers from the GTFS-Realtime spec:
/// `FeedMessage.entity = 2`, `FeedEntity.{id=1, vehicle=4}`.
fn parse_vehicle_positions(data: &[u8], lines: &LineMap) -> Vec<VehicleReport> {
    let mut out = Vec::new();
    let mut m = Buf::new(data);
    while let Some((field, wire)) = m.tag() {
        if field == 2 && wire == 2 {
            match m.bytes() {
                Some(entity) => {
                    if let Some(v) = parse_entity(entity, lines) {
                        out.push(v);
                    }
                }
                None => break,
            }
        } else if m.skip(wire).is_none() {
            break;
        }
    }
    out
}

/// A `FeedEntity`: `id = 1` (string), `vehicle = 4` (`VehiclePosition`). The id
/// is `DATE:AGENCY:PLANNING:JOURNEY…`; its planning segment resolves the public
/// line + mode against the line list (unresolved → no label, drawn as a bus).
fn parse_entity(data: &[u8], lines: &LineMap) -> Option<VehicleReport> {
    let mut id: Option<String> = None;
    let mut pos: Option<(f32, f32, Option<i64>)> = None;
    let mut b = Buf::new(data);
    while let Some((field, wire)) = b.tag() {
        match (field, wire) {
            (1, 2) => id = b.bytes().map(|s| String::from_utf8_lossy(s).into_owned()),
            (4, 2) => pos = b.bytes().and_then(parse_vehicle_position),
            _ => b.skip(wire)?,
        }
    }
    let (lat, lng, ts) = pos?;
    let id = id.filter(|s| !s.is_empty())?;
    let (line, transport) = id
        .split(':')
        .nth(2)
        .and_then(|planning| lines.get(planning))
        .cloned()
        .unwrap_or((None, Transport::Bus));
    Some(VehicleReport {
        id,
        lat: lat as f64,
        lng: lng as f64,
        ts,
        line,
        transport,
    })
}

/// A `VehiclePosition`: `position = 2` (`Position`), `timestamp = 5` (uint64).
/// Returns lat/lng/timestamp, or `None` if it carries no position.
fn parse_vehicle_position(data: &[u8]) -> Option<(f32, f32, Option<i64>)> {
    let mut latlng: Option<(f32, f32)> = None;
    let mut ts: Option<i64> = None;
    let mut b = Buf::new(data);
    while let Some((field, wire)) = b.tag() {
        match (field, wire) {
            (2, 2) => latlng = b.bytes().and_then(parse_position),
            (5, 0) => ts = b.varint().map(|v| v as i64),
            _ => b.skip(wire)?,
        }
    }
    let (lat, lng) = latlng?;
    Some((lat, lng, ts))
}

/// A `Position`: `latitude = 1`, `longitude = 2` (both float; the feed omits
/// bearing/speed, so those are ignored).
fn parse_position(data: &[u8]) -> Option<(f32, f32)> {
    let mut lat: Option<f32> = None;
    let mut lng: Option<f32> = None;
    let mut b = Buf::new(data);
    while let Some((field, wire)) = b.tag() {
        match (field, wire) {
            (1, 5) => lat = b.f32(),
            (2, 5) => lng = b.f32(),
            _ => b.skip(wire)?,
        }
    }
    Some((lat?, lng?))
}

// --- Line-number label painter (fontdue over egui's embedded Hack font) ---

/// Label texture size (2:1, matching `LABEL_W`:`LABEL_H`).
const LABEL_PX_W: usize = 128;
const LABEL_PX_H: usize = 64;

/// Rasterises a short line number onto a small badge texture. Reuses the `Hack`
/// font egui already embeds (no extra assets), like `transit_live`'s boards.
struct LabelPainter {
    font: Option<fontdue::Font>,
}

impl LabelPainter {
    fn new() -> Self {
        let font = egui::FontDefinitions::default()
            .font_data
            .get("Hack")
            .and_then(|d| {
                fontdue::Font::from_bytes(d.font.as_ref(), fontdue::FontSettings::default()).ok()
            });
        Self { font }
    }

    /// A `line` badge: a filled rectangle in the mode colour with a darker
    /// border and the number in white, sized to fit.
    fn paint(&self, line: &str, color: [u8; 3]) -> Image {
        let (w, h) = (LABEL_PX_W, LABEL_PX_H);
        let mut px = vec![0u8; w * h * 4];
        let dim = |c: u8| (c as f32 * 0.55) as u8;
        fill_rect(
            &mut px,
            0,
            0,
            w,
            h,
            [dim(color[0]), dim(color[1]), dim(color[2]), 255],
        );
        fill_rect(
            &mut px,
            4,
            4,
            w - 4,
            h - 4,
            [color[0], color[1], color[2], 255],
        );

        if let Some(font) = &self.font {
            let measure = |s: f32| {
                line.chars()
                    .map(|c| font.metrics(c, s).advance_width)
                    .sum::<f32>()
            };
            // Shrink to fit the badge width.
            let mut size = 46.0f32;
            while size > 16.0 && measure(size) > w as f32 - 20.0 {
                size -= 2.0;
            }
            let mut cx = (w as f32 - measure(size)) / 2.0;
            let baseline = h as f32 / 2.0 + size * 0.35;
            for ch in line.chars() {
                let (m, cov) = font.rasterize(ch, size);
                let gx = cx as i32 + m.xmin;
                let gy = baseline as i32 - m.height as i32 - m.ymin;
                for row in 0..m.height {
                    let py = gy + row as i32;
                    if py < 0 || py >= h as i32 {
                        continue;
                    }
                    for col in 0..m.width {
                        let pxx = gx + col as i32;
                        if pxx < 0 || pxx >= w as i32 {
                            continue;
                        }
                        let a = cov[row * m.width + col] as u32;
                        if a == 0 {
                            continue;
                        }
                        // White text over the badge colour.
                        let i = (py as usize * w + pxx as usize) * 4;
                        for k in 0..3 {
                            let dst = px[i + k] as u32;
                            px[i + k] = ((255 * a + dst * (255 - a)) / 255) as u8;
                        }
                    }
                }
                cx += m.advance_width;
            }
        }

        Image::new(
            Extent3d {
                width: w as u32,
                height: h as u32,
                depth_or_array_layers: 1,
            },
            TextureDimension::D2,
            px,
            TextureFormat::Rgba8UnormSrgb,
            RenderAssetUsages::RENDER_WORLD,
        )
    }
}

fn fill_rect(px: &mut [u8], x0: usize, y0: usize, x1: usize, y1: usize, c: [u8; 4]) {
    for y in y0..y1.min(LABEL_PX_H) {
        for x in x0..x1.min(LABEL_PX_W) {
            let i = (y * LABEL_PX_W + x) * 4;
            px[i..i + 4].copy_from_slice(&c);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Hand-encode a one-vehicle `FeedMessage` (with a trip sub-message and an
    /// unknown field, to exercise skipping) and assert the reader recovers the
    /// id, lat/lng, and timestamp.
    #[test]
    fn parses_a_vehicle_position() {
        /// tag byte = (field << 3) | wire.
        fn tag(field: u64, wire: u8) -> u8 {
            ((field << 3) as u8) | wire
        }
        /// length-delimited field: tag + varint(len) + payload (len < 128 here).
        fn ld(field: u64, payload: &[u8]) -> Vec<u8> {
            let mut v = vec![tag(field, 2), payload.len() as u8];
            v.extend_from_slice(payload);
            v
        }
        /// fixed32 float field.
        fn fx(field: u64, val: f32) -> Vec<u8> {
            let mut v = vec![tag(field, 5)];
            v.extend_from_slice(&val.to_le_bytes());
            v
        }

        // Position with an extra bearing field (3) the reader must ignore.
        let position = [fx(1, 52.09), fx(2, 5.11), fx(3, 42.0)].concat();
        let trip = ld(5, b"12345"); // TripDescriptor.route_id — skipped
        let mut vehicle = Vec::new();
        vehicle.extend(ld(1, &trip)); // trip (field 1) — skipped
        vehicle.extend(ld(2, &position)); // position (field 2)
        vehicle.extend([tag(5, 0), 200, 6]); // timestamp varint = 6*128+72 = 840
        let mut entity = Vec::new();
        entity.extend(ld(1, b"2026-07-19:GVB:293:104951")); // id (planning = 293)
        entity.extend(ld(4, &vehicle)); // vehicle
        let feed = ld(2, &entity); // FeedMessage.entity

        // Line list resolves planning "293" → public "N93", a tram.
        let mut lines = LineMap::new();
        lines.insert("293".into(), (Some("N93".into()), Transport::Tram));

        let reports = parse_vehicle_positions(&feed, &lines);
        assert_eq!(reports.len(), 1);
        let r = &reports[0];
        assert_eq!(r.id, "2026-07-19:GVB:293:104951");
        assert!((r.lat - 52.09).abs() < 1e-3, "lat = {}", r.lat);
        assert!((r.lng - 5.11).abs() < 1e-3, "lng = {}", r.lng);
        assert_eq!(r.ts, Some(840));
        // Public line + mode resolved from the id's planning segment (293).
        assert_eq!(r.line.as_deref(), Some("N93"));
        assert_eq!(r.transport, Transport::Tram);

        // An unknown planning number → no label, drawn as a bus.
        let bare = parse_vehicle_positions(&feed, &LineMap::new());
        assert_eq!(bare[0].line, None);
        assert_eq!(bare[0].transport, Transport::Bus);
    }

    /// OVapi `TransportType` strings map to modes; unknown → bus.
    #[test]
    fn transport_from_type() {
        assert_eq!(Transport::from_type("TRAM"), Transport::Tram);
        assert_eq!(Transport::from_type("METRO"), Transport::Metro);
        assert_eq!(Transport::from_type("TRAIN"), Transport::Train);
        assert_eq!(Transport::from_type("BOAT"), Transport::Bus);
    }

    /// The line list parses into planning → (public, mode).
    #[test]
    fn line_map_parsing() {
        let json = r#"{
            "NL_293_1": {"LinePlanningNumber":"293","LinePublicNumber":"N93","TransportType":"TRAM"},
            "NL_5302_1": {"LinePlanningNumber":"5302","LinePublicNumber":"302","TransportType":"BUS"}
        }"#;
        let map = parse_line_map(json);
        assert_eq!(
            map.get("293"),
            Some(&(Some("N93".to_string()), Transport::Tram))
        );
        assert_eq!(
            map.get("5302"),
            Some(&(Some("302".to_string()), Transport::Bus))
        );
    }

    /// Motion-derived heading maps to the right Bevy yaw (north = −Z, east = +X
    /// in the map frame; `d.y` is the map z axis).
    #[test]
    fn motion_maps_to_heading() {
        let dir = |d: Vec2| Quat::from_rotation_y(heading_yaw(d)) * Vec3::Z;
        // Travelling north = toward smaller map-z (d.y negative).
        let north = dir(Vec2::new(0.0, -1.0));
        assert!(north.z < -0.99, "north should face −Z, got {north:?}");
        // Travelling east = toward +X.
        let east = dir(Vec2::new(1.0, 0.0));
        assert!(east.x > 0.99, "east should face +X, got {east:?}");
    }
}
