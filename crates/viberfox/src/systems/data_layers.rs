//! Data layers (View ▸ Data Layers, ADR-023): togglable point-of-interest
//! overlays streamed from the OSM Overpass API — the same open data source the
//! tiles and 3D buildings already come from (© OpenStreetMap contributors, ODbL).
//!
//! Layers are declared in [`LAYER_DEFS`]; adding a future layer (businesses,
//! charging stations, …) is one new entry — the streaming, caching, panel and
//! rendering are shared. Each layer streams per fixed LAYER_ZOOM cell around
//! the camera (like the far building cells): fetched once through a disk cache
//! (`cache/layers/{key}/…`), meshed on a worker thread into one merged
//! pin-marker mesh per cell, and despawned again when out of range. Toggling a
//! layer off just hides its entities, so re-enabling is instant.
//!
//! Each cell also yields its POIs as [`PoiInstance`]s, retained alongside the
//! mesh so other systems can attach behaviour to individual points — that is how
//! `transit_live` hangs live departure boards off the transit stops (ADR-024).
//!
//! First layer: public transport stops (rail stations, tram stops, bus stops).

use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::sync::mpsc::Sender;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use bevy::prelude::*;
use big_space::prelude::{BigSpace, Grid, GridCell};

use crate::components::Region;
use crate::systems::egui_manager::EguiManager;
use crate::systems::free_camera::{FreeCamera, WORLD_CELL_EDGE, WORLD_SWITCH_THRESHOLD};
use crate::systems::osm_buildings::{
    lat_lng_to_local, overpass_query, MeshData, OVERPASS_THROTTLE,
};
use vibe_core::world::{tile_to_lat_lng, tile_to_meters};

/// Fixed grid zoom at which layer data is fetched + cached (same scheme as the
/// building cells: coarse cells, one Overpass query each, cache never
/// invalidated by radius changes).
const LAYER_ZOOM: u32 = 14;
/// LAYER_ZOOM cells out from the camera kept loaded per enabled layer.
const LAYER_CELL_RADIUS: i64 = 3;
/// Give up on a cell after this many failed fetch rounds.
const MAX_CELL_ATTEMPTS: u32 = 5;

/// Index of the public-transport layer in [`LAYER_DEFS`] (see `transit_live`).
pub const TRANSIT_LAYER: usize = 0;

/// How one POI category is drawn: pin colour and pole/head size in metres.
///
/// Pins are annotations, not scenery: they are deliberately taller than the
/// surrounding buildings so they stay findable from an oblique camera instead
/// of sinking into the rooftops.
pub struct MarkerStyle {
    pub color: [f32; 3],
    pub pole_height: f32,
    pub head_radius: f32,
}

impl MarkerStyle {
    /// Height of the anchor point above ground: just clear of the pin's head.
    /// Live overlays (departure boards) hang here.
    fn anchor_height(&self) -> f32 {
        self.pole_height + 2.0 * self.head_radius + 1.5
    }
}

/// A declarative data layer: what to fetch from Overpass and how to draw it.
pub struct LayerDef {
    /// Stable key, used as the disk-cache directory name.
    pub key: &'static str,
    /// Label shown in the Data Layers panel.
    pub name: &'static str,
    /// Overpass QL statement; every `{bbox}` is replaced with
    /// `south,west,north,east`.
    pub query: &'static str,
    /// Categories (panel legend + marker styles); indices match `classify`.
    pub categories: &'static [(&'static str, MarkerStyle)],
    /// Map a node's OSM tags to a category index (None = skip the node).
    pub classify: fn(Option<&serde_json::Value>) -> Option<usize>,
    /// Extract the key this POI is known by in a live-data feed, if any.
    pub live_id: Option<fn(Option<&serde_json::Value>) -> Option<String>>,
}

/// All available data layers. Add an entry here to add a layer.
pub const LAYER_DEFS: &[LayerDef] = &[LayerDef {
    key: "transit",
    name: "Public transport stops",
    query: "[out:json][timeout:25];(node[\"railway\"~\"^(station|halt|tram_stop)$\"]({bbox});node[\"highway\"=\"bus_stop\"]({bbox}););out;",
    categories: &[
        (
            "Rail station",
            MarkerStyle { color: [0.15, 0.40, 0.85], pole_height: 40.0, head_radius: 4.0 },
        ),
        (
            "Tram stop",
            MarkerStyle { color: [0.16, 0.60, 0.30], pole_height: 28.0, head_radius: 2.8 },
        ),
        (
            "Bus stop",
            MarkerStyle { color: [0.95, 0.66, 0.12], pole_height: 20.0, head_radius: 2.2 },
        ),
    ],
    classify: classify_transit,
    live_id: Some(transit_live_id),
}];

fn classify_transit(tags: Option<&serde_json::Value>) -> Option<usize> {
    let tag = |k: &str| tags.and_then(|t| t.get(k)).and_then(|v| v.as_str());
    match tag("railway") {
        Some("station") | Some("halt") => return Some(0),
        Some("tram_stop") => return Some(1),
        _ => {}
    }
    (tag("highway") == Some("bus_stop")).then_some(2)
}

/// OVapi keys Dutch stops by timing-point code, which is the numeric suffix of
/// the OSM `ref:IFOPT` tag (`NL:Q:10006090` → `10006090`). Stops outside the
/// Netherlands carry no `NL:Q:` ref and simply get no live board.
fn transit_live_id(tags: Option<&serde_json::Value>) -> Option<String> {
    let ifopt = tags?.get("ref:IFOPT")?.as_str()?;
    let code = ifopt.strip_prefix("NL:Q:")?;
    (!code.is_empty() && code.bytes().all(|b| b.is_ascii_digit())).then(|| code.to_string())
}

/// One rendered POI, retained so other systems can attach behaviour to it.
#[derive(Clone)]
pub struct PoiInstance {
    /// BigSpace cell the anchor is relative to.
    pub cell: GridCell,
    /// Point just above the pin's head, in `cell`-local metres.
    pub anchor: Vec3,
    pub name: Option<String>,
    /// Key into a live feed (see [`LayerDef::live_id`]).
    pub live_id: Option<String>,
    pub category: usize,
}

/// Per-layer streaming state (mirrors the far-building-cell bookkeeping).
#[derive(Default)]
pub struct LayerRuntime {
    /// Panel checkbox: whether this layer's markers are shown (and streamed).
    pub enabled: bool,
    /// Visibility last applied to the spawned entities.
    shown: bool,
    /// Cells currently queued or loaded (so they aren't re-queried).
    requested: HashSet<(i64, i64)>,
    /// Failed fetch rounds per cell (bounded retry).
    failed: HashMap<(i64, i64), u32>,
    /// Earliest time a failed cell may be re-queued (exponential backoff).
    retry_at: HashMap<(i64, i64), Instant>,
    /// Spawned marker-mesh entity per cell.
    cells: HashMap<(i64, i64), Entity>,
    /// POIs of each loaded cell, in the same lifetime as its mesh entity.
    pub pois: HashMap<(i64, i64), Vec<PoiInstance>>,
}

/// A cell built on the worker: its anchor cell, merged marker mesh, and POIs.
pub struct CellBuild {
    cell: GridCell,
    mesh: Mesh,
    pois: Vec<PoiInstance>,
}

/// A completed cell build: the layer it belongs to, its cell coords, and the
/// build (None for cells without matching POIs).
type CellResult = (usize, (i64, i64), Result<Option<CellBuild>, String>);

/// Data-layer streaming + panel state.
#[derive(Resource)]
pub struct DataLayers {
    /// Whether the Data Layers panel window is open (View ▸ Data Layers).
    pub panel_open: bool,
    /// Region anchor: tile_x, tile_y, zoom, latitude (same as the buildings).
    anchor: Option<(i64, i64, u32, f64)>,
    /// Worker job queue: (layer index, cell x, cell y).
    jobs: Option<Sender<(usize, i64, i64)>>,
    results: Arc<Mutex<Vec<CellResult>>>,
    /// One unlit material shared by all markers (hue is per-vertex).
    material: Option<Handle<StandardMaterial>>,
    pub layers: Vec<LayerRuntime>,
}

impl Default for DataLayers {
    fn default() -> Self {
        Self {
            panel_open: false,
            anchor: None,
            jobs: None,
            results: Arc::default(),
            material: None,
            layers: LAYER_DEFS.iter().map(|_| LayerRuntime::default()).collect(),
        }
    }
}

/// Marker component for spawned data-layer cell meshes.
#[derive(Component)]
pub struct DataLayerMarkers;

/// Stream marker cells for every enabled layer as the camera roams, and apply
/// enable/disable toggles from the panel by flipping entity visibility.
pub fn update_data_layers(
    mut commands: Commands,
    mut dl: ResMut<DataLayers>,
    mut meshes: ResMut<Assets<Mesh>>,
    mut materials: ResMut<Assets<StandardMaterial>>,
    regions: Query<&Region>,
    camera: Query<(&GridCell, &Transform), With<FreeCamera>>,
    bigspace: Query<Entity, With<BigSpace>>,
) {
    // Establish the region anchor once and start the cell worker thread.
    if dl.anchor.is_none() {
        let Some(region) = regions.iter().next() else {
            return;
        };
        let z = region.tile_z.clamp(0, u32::MAX as i64) as u32;
        let (ax, ay, lat) = (region.tile_x, region.tile_y, region.latitude);
        dl.anchor = Some((ax, ay, z, lat));

        let (tx, rx) = std::sync::mpsc::channel::<(usize, i64, i64)>();
        dl.jobs = Some(tx);
        let results = dl.results.clone();
        std::thread::spawn(move || {
            while let Ok((li, cx, cy)) = rx.recv() {
                let built = build_layer_cell(li, cx, cy, ax, ay, z, lat);
                if let Ok(mut g) = results.lock() {
                    g.push((li, (cx, cy), built));
                }
            }
        });
    }
    let (ax, ay, z, lat) = dl.anchor.unwrap();
    let tile_m = tile_to_meters(z, lat) as f32;

    let Ok((cell, tf)) = camera.single() else {
        return;
    };
    let Ok(root) = bigspace.single() else {
        return;
    };

    // Camera's LAYER_ZOOM cell from its base-zoom tile coords.
    let world_x = cell.x as f32 * WORLD_CELL_EDGE + tf.translation.x;
    let world_z = cell.z as f32 * WORLD_CELL_EDGE + tf.translation.z;
    let shift = z.saturating_sub(LAYER_ZOOM);
    let ccx = (ax + (world_x / tile_m).round() as i64).div_euclid(1i64 << shift);
    let ccy = (ay + (world_z / tile_m).round() as i64).div_euclid(1i64 << shift);

    // Queue missing cells for enabled layers, nearest first. A cell that failed
    // waits out its backoff first: Overpass 504s come in bursts, and retrying
    // straight away just burns the attempt budget while it is still overloaded.
    if let Some(jobs) = dl.jobs.clone() {
        let now = Instant::now();
        for li in 0..dl.layers.len() {
            if !dl.layers[li].enabled {
                continue;
            }
            let rt = &mut dl.layers[li];
            let mut to_request: Vec<(i64, i64)> = Vec::new();
            for dy in -LAYER_CELL_RADIUS..=LAYER_CELL_RADIUS {
                for dx in -LAYER_CELL_RADIUS..=LAYER_CELL_RADIUS {
                    let c = (ccx + dx, ccy + dy);
                    let attempts = rt.failed.get(&c).copied().unwrap_or(0);
                    let ready = rt.retry_at.get(&c).is_none_or(|t| now >= *t);
                    if !rt.requested.contains(&c) && attempts < MAX_CELL_ATTEMPTS && ready {
                        to_request.push(c);
                    }
                }
            }
            to_request.sort_by_key(|c| (c.0 - ccx).abs().max((c.1 - ccy).abs()));
            for c in to_request {
                rt.requested.insert(c);
                let _ = jobs.send((li, c.0, c.1));
            }
        }
    }

    // Apply completed cells.
    let done: Vec<CellResult> = dl
        .results
        .lock()
        .map(|mut g| g.drain(..).collect())
        .unwrap_or_default();
    for (li, cell, built) in done {
        match built {
            Ok(Some(build)) => {
                let material = dl
                    .material
                    .get_or_insert_with(|| {
                        materials.add(StandardMaterial {
                            base_color: Color::WHITE,
                            unlit: true,
                            double_sided: true,
                            cull_mode: None,
                            ..default()
                        })
                    })
                    .clone();
                let visibility = if dl.layers[li].enabled {
                    Visibility::Visible
                } else {
                    Visibility::Hidden
                };
                let pois = build.pois.len();
                let entity = commands
                    .spawn((
                        Mesh3d(meshes.add(build.mesh)),
                        MeshMaterial3d(material),
                        Transform::from_translation(Vec3::new(0.0, 0.1, 0.0)),
                        build.cell,
                        ChildOf(root),
                        visibility,
                        DataLayerMarkers,
                    ))
                    .id();
                dl.layers[li].cells.insert(cell, entity);
                dl.layers[li].pois.insert(cell, build.pois);
                dl.layers[li].failed.remove(&cell);
                dl.layers[li].retry_at.remove(&cell);
                tracing::info!(layer = LAYER_DEFS[li].key, cell = ?cell, pois, "data layer cell loaded");
            }
            Ok(None) => {} // no POIs; stays in `requested` so it isn't re-queried
            Err(e) => {
                let rt = &mut dl.layers[li];
                let attempts = rt.failed.entry(cell).or_insert(0);
                *attempts += 1;
                // 15s, 30s, 1m, 2m, 4m — long enough to outlast an Overpass wobble.
                let backoff = Duration::from_secs(15u64 << (*attempts - 1).min(4));
                let attempts = *attempts;
                rt.retry_at.insert(cell, Instant::now() + backoff);
                rt.requested.remove(&cell);
                tracing::warn!(
                    layer = LAYER_DEFS[li].key, cell = ?cell, attempts,
                    retry_in_s = backoff.as_secs(), error = %e,
                    "data layer cell failed; will retry"
                );
            }
        }
    }

    for rt in dl.layers.iter_mut() {
        // Despawn cells out of range (one cell of hysteresis); re-entry
        // re-requests them, served from the disk cache.
        let stale: Vec<(i64, i64)> = rt
            .cells
            .keys()
            .filter(|(cx, cy)| (cx - ccx).abs().max((cy - ccy).abs()) > LAYER_CELL_RADIUS + 1)
            .copied()
            .collect();
        for c in stale {
            if let Some(entity) = rt.cells.remove(&c) {
                commands.entity(entity).despawn();
            }
            rt.pois.remove(&c);
            rt.requested.remove(&c);
        }

        // Apply panel toggles by flipping visibility (cheap on/off).
        if rt.shown != rt.enabled {
            rt.shown = rt.enabled;
            let visibility = if rt.enabled {
                Visibility::Visible
            } else {
                Visibility::Hidden
            };
            for &entity in rt.cells.values() {
                commands.entity(entity).insert(visibility);
            }
        }
    }
}

/// The Layers side panel (menu bar ▸ Layers): a checkbox per layer with a
/// colour legend, plus the required OSM attribution. Styled like the AI
/// assistant panel (right side panel, resizable).
pub fn render_data_layers_panel(mut egui: ResMut<EguiManager>, mut dl: ResMut<DataLayers>) {
    if !dl.panel_open {
        return;
    }
    let ctx = egui.ctx_mut();
    let mut close = false;
    egui::SidePanel::right("layers_panel")
        .resizable(true)
        .default_width(260.0)
        .show(ctx, |ui| {
            ui.horizontal(|ui| {
                ui.heading("Layers");
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if ui.button("✕").clicked() {
                        close = true;
                    }
                });
            });
            ui.separator();
            for (i, def) in LAYER_DEFS.iter().enumerate() {
                ui.add_space(4.0);
                ui.checkbox(
                    &mut dl.layers[i].enabled,
                    egui::RichText::new(def.name).size(14.0),
                );
                ui.indent(def.key, |ui| {
                    for (label, style) in def.categories {
                        let c = egui::Color32::from_rgb(
                            (style.color[0] * 255.0) as u8,
                            (style.color[1] * 255.0) as u8,
                            (style.color[2] * 255.0) as u8,
                        );
                        ui.horizontal(|ui| {
                            ui.label(egui::RichText::new("●").size(12.0).color(c));
                            ui.label(egui::RichText::new(*label).size(13.0));
                        });
                    }
                });
                ui.add_space(4.0);
                ui.separator();
            }
            ui.with_layout(egui::Layout::bottom_up(egui::Align::LEFT), |ui| {
                ui.add_space(6.0);
                ui.small("Data © OpenStreetMap contributors (ODbL), via Overpass API");
                ui.small("Live departures: OVapi (v0.ovapi.nl)");
            });
        });
    if close {
        dl.panel_open = false;
    }
}

// --- Worker-thread fetch + mesh build ---

/// A point of interest parsed from Overpass.
struct Poi {
    lat: f64,
    lng: f64,
    category: usize,
    name: Option<String>,
    live_id: Option<String>,
}

/// Fetch one layer cell (disk-cached Overpass) and build its merged marker
/// mesh, anchored to the cell centre's BigSpace grid cell for f32 precision.
/// Returns `None` for cells without matching POIs.
fn build_layer_cell(
    li: usize,
    cx: i64,
    cy: i64,
    ax: i64,
    ay: i64,
    z: u32,
    lat: f64,
) -> Result<Option<CellBuild>, String> {
    let def = &LAYER_DEFS[li];
    let pois = fetch_cell(def, cx, cy)?;
    if pois.is_empty() {
        return Ok(None);
    }
    let (north, west) = tile_to_lat_lng(cx, cy, LAYER_ZOOM);
    let (south, east) = tile_to_lat_lng(cx + 1, cy + 1, LAYER_ZOOM);
    let tile_m = tile_to_meters(z, lat) as f32;

    let c_world = lat_lng_to_local((north + south) / 2.0, (west + east) / 2.0, ax, ay, z, tile_m);
    let (grid_cell, _) = Grid::new(WORLD_CELL_EDGE, WORLD_SWITCH_THRESHOLD)
        .translation_to_grid(Vec3::new(c_world.x, 0.0, c_world.y).as_dvec3());
    let ox = grid_cell.x as f32 * WORLD_CELL_EDGE;
    let oz = grid_cell.z as f32 * WORLD_CELL_EDGE;

    let mut buf = MeshData::default();
    let mut instances = Vec::with_capacity(pois.len());
    for poi in &pois {
        let w = lat_lng_to_local(poi.lat, poi.lng, ax, ay, z, tile_m);
        let local = Vec2::new(w.x - ox, w.y - oz);
        let style = &def.categories[poi.category].1;
        push_marker(&mut buf, local, style);
        instances.push(PoiInstance {
            cell: grid_cell,
            anchor: Vec3::new(local.x, style.anchor_height(), local.y),
            name: poi.name.clone(),
            live_id: poi.live_id.clone(),
            category: poi.category,
        });
    }
    Ok(buf.into_mesh().map(|mesh| CellBuild {
        cell: grid_cell,
        mesh,
        pois: instances,
    }))
}

/// On-disk cache path for one layer cell.
fn cell_cache_path(key: &str, cx: i64, cy: i64) -> PathBuf {
    PathBuf::from("cache")
        .join("layers")
        .join(key)
        .join(LAYER_ZOOM.to_string())
        .join(cx.to_string())
        .join(format!("{}.json", cy))
}

/// One cell's POIs, from disk cache or a single throttled Overpass query.
fn fetch_cell(def: &LayerDef, cx: i64, cy: i64) -> Result<Vec<Poi>, String> {
    let path = cell_cache_path(def.key, cx, cy);
    if let Ok(cached) = std::fs::read_to_string(&path) {
        return parse_pois(def, &cached);
    }

    let (north, west) = tile_to_lat_lng(cx, cy, LAYER_ZOOM);
    let (south, east) = tile_to_lat_lng(cx + 1, cy + 1, LAYER_ZOOM);
    let bbox = format!("{south},{west},{north},{east}");
    let fetched = overpass_query(&def.query.replace("{bbox}", &bbox))?;
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    let _ = std::fs::write(&path, &fetched);
    // Space out live queries so a burst of cells doesn't trip the rate limit.
    std::thread::sleep(OVERPASS_THROTTLE);
    parse_pois(def, &fetched)
}

/// Parse an Overpass node response into POIs via the layer's classifier.
fn parse_pois(def: &LayerDef, text: &str) -> Result<Vec<Poi>, String> {
    let json: serde_json::Value = serde_json::from_str(text).map_err(|e| e.to_string())?;
    let elements = json
        .get("elements")
        .and_then(|e| e.as_array())
        .ok_or_else(|| "no elements in Overpass response".to_string())?;

    let mut out = Vec::new();
    for el in elements {
        if el.get("type").and_then(|t| t.as_str()) != Some("node") {
            continue;
        }
        let (Some(lat), Some(lng)) = (
            el.get("lat").and_then(|v| v.as_f64()),
            el.get("lon").and_then(|v| v.as_f64()),
        ) else {
            continue;
        };
        let tags = el.get("tags");
        if let Some(category) = (def.classify)(tags) {
            out.push(Poi {
                lat,
                lng,
                category,
                name: tags
                    .and_then(|t| t.get("name"))
                    .and_then(|v| v.as_str())
                    .map(str::to_string),
                live_id: def.live_id.and_then(|f| f(tags)),
            });
        }
    }
    Ok(out)
}

/// Append one map pin at `p`: a slim crossed-quad pole with an octahedral head
/// floating above it. Unlit material + vertex colour, so it reads day and night.
fn push_marker(buf: &mut MeshData, p: Vec2, style: &MarkerStyle) {
    let color = [style.color[0], style.color[1], style.color[2], 1.0];
    let h = style.pole_height;
    let r = style.head_radius;
    let w = (0.18 * r).max(0.25);

    // Pole: two crossed vertical quads (cull is off, so both sides show).
    buf.quad(
        Vec3::new(p.x - w, 0.0, p.y),
        Vec3::new(p.x + w, 0.0, p.y),
        Vec3::new(p.x + w, h, p.y),
        Vec3::new(p.x - w, h, p.y),
        color,
    );
    buf.quad(
        Vec3::new(p.x, 0.0, p.y - w),
        Vec3::new(p.x, 0.0, p.y + w),
        Vec3::new(p.x, h, p.y + w),
        Vec3::new(p.x, h, p.y - w),
        color,
    );

    // Head: an octahedron whose lower tip meets the pole top.
    let cy = h + r;
    let top = Vec3::new(p.x, cy + r, p.y);
    let bottom = Vec3::new(p.x, cy - r, p.y);
    let equator = [
        Vec3::new(p.x + r, cy, p.y),
        Vec3::new(p.x, cy, p.y + r),
        Vec3::new(p.x - r, cy, p.y),
        Vec3::new(p.x, cy, p.y - r),
    ];
    for i in 0..4 {
        let (a, b) = (equator[i], equator[(i + 1) % 4]);
        buf.tri(top, a, b, color);
        buf.tri(bottom, b, a, color);
    }
}
