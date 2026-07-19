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

use bevy::pbr::{
    ExtendedMaterial, MaterialExtension, MaterialExtensionKey, MaterialExtensionPipeline,
};
use bevy::prelude::*;
use bevy::render::mesh::MeshVertexBufferLayoutRef;
use bevy::render::render_resource::{
    AsBindGroup, CompareFunction, RenderPipelineDescriptor, SpecializedMeshPipelineError,
};
use big_space::prelude::{BigSpace, Grid, GridCell};

use crate::systems::egui_manager::EguiManager;
use crate::systems::free_camera::{FreeCamera, WORLD_CELL_EDGE, WORLD_SWITCH_THRESHOLD};
use crate::systems::osm_buildings::{
    lat_lng_to_local, overpass_query, MeshData, OVERPASS_THROTTLE,
};
use vibe_core::world::{tile_to_lat_lng, tile_to_meters};

/// Material for the floating shop-pin heads: a plain `StandardMaterial` (unlit,
/// per-vertex hue) whose depth test is forced to always pass, so the gem heads
/// draw over the buildings like real map POI markers. The stems and ground
/// discs stay on the ordinary depth-tested marker material and still occlude
/// behind walls, so an occluded pin reads as a gem hovering above its roof
/// rather than a needle poking through it. The base uses `AlphaMode::Blend` so
/// heads render in the transparent phase, after all opaque geometry — nothing
/// painted earlier can then hide them.
pub type PinHeadMaterial = ExtendedMaterial<StandardMaterial, DepthAlways>;

/// Pipeline tweak for [`PinHeadMaterial`]: pass the depth test unconditionally.
#[derive(Asset, AsBindGroup, Reflect, Clone, Default)]
pub struct DepthAlways {}

impl MaterialExtension for DepthAlways {
    fn specialize(
        _pipeline: &MaterialExtensionPipeline,
        descriptor: &mut RenderPipelineDescriptor,
        _layout: &MeshVertexBufferLayoutRef,
        _key: MaterialExtensionKey<Self>,
    ) -> Result<(), SpecializedMeshPipelineError> {
        if let Some(depth) = descriptor.depth_stencil.as_mut() {
            depth.depth_compare = Some(CompareFunction::Always);
        }
        Ok(())
    }
}

/// Fixed grid zoom at which layer data is fetched + cached (same scheme as the
/// building cells: coarse cells, one Overpass query each, cache never
/// invalidated by radius changes).
const LAYER_ZOOM: u32 = 14;

/// Streaming ceiling for these *surface* layers (POI pins, transit stops):
/// above this camera altitude no new cells are fetched. Distinct from the
/// space-tier props: pins are irrelevant long before the globe appears.
const SURFACE_LAYER_MAX_ALT: f64 = 20_000.0;
/// LAYER_ZOOM cells out from the camera kept loaded per enabled layer.
const LAYER_CELL_RADIUS: i64 = 3;
/// Give up on a cell after this many failed fetch rounds.
const MAX_CELL_ATTEMPTS: u32 = 5;
/// Density LOD: max markers rendered per cell. A dense shopping district returns
/// 500+ POIs in one cell, and every pin head renders *always-on-top* and
/// alpha-blended (see [`PinHeadMaterial`] / [`DepthAlways`]) — so emitting them all
/// is unbounded transparent overdraw with no early-z rejection, which pins a weak
/// iGPU at its command-buffer timeout (`VK_ERROR_DEVICE_LOST`) in shop-dense areas.
/// Above the cap we subsample evenly, keeping a representative spread: a lower-LOD
/// overview instead of every single POI.
const MAX_MARKERS_PER_CELL: usize = 80;

/// Index of the public-transport layer in [`LAYER_DEFS`] (see `transit_live`).
pub const TRANSIT_LAYER: usize = 0;

/// How a layer's POIs are shaped in the world (all categories share a shape).
#[derive(Clone, Copy, PartialEq)]
pub enum MarkerShape {
    /// A tall anthracite street-furniture pole (R-NET style), with an optional
    /// live departure board near the top and a pulsing ground ring. Right for
    /// sparse layers like transit stops.
    Pole,
    /// A colour-coded gem head floating above the location on a slim stem, with
    /// a small disc marking the exact spot on the ground — a recognisable,
    /// clickable map pin that stands clear of the shopfronts. Right for dense
    /// layers like shops, where poles would disappear into the streetscape.
    Pin,
}

/// How one POI category is drawn. `pole_height` is the height of the marker's
/// business end — the pole top for [`MarkerShape::Pole`] (where the sign board
/// and anchor sit) or the floating gem-head centre for [`MarkerShape::Pin`].
pub struct MarkerStyle {
    /// Category colour (pole cap / pin head, ground ring, panel legend).
    pub color: [f32; 3],
    /// Height in metres of the pole top / the floating pin head.
    pub pole_height: f32,
    /// Width of the live display board mounted on this pole, metres (poles
    /// only; 0 = none).
    pub board_width: f32,
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
    /// How each POI is drawn in the world (pole vs floating pin).
    pub shape: MarkerShape,
    /// Whether this layer's POIs pulse a radiating ground ring. Right for
    /// sparse layers (stops); overwhelming for dense ones (shops).
    pub rings: bool,
}

/// All available data layers. Add an entry here to add a layer.
pub const LAYER_DEFS: &[LayerDef] = &[
    LayerDef {
        key: "transit",
        name: "Public transport stops",
        query: "[out:json][timeout:25];(node[\"railway\"~\"^(station|halt|tram_stop)$\"]({bbox});node[\"highway\"=\"bus_stop\"]({bbox}););out;",
        categories: &[
            (
                "Rail station",
                MarkerStyle { color: [0.15, 0.40, 0.85], pole_height: 7.0, board_width: 3.2 },
            ),
            (
                "Tram stop",
                MarkerStyle { color: [0.16, 0.60, 0.30], pole_height: 5.0, board_width: 1.8 },
            ),
            (
                "Bus stop",
                MarkerStyle { color: [0.95, 0.66, 0.12], pole_height: 4.6, board_width: 1.6 },
            ),
        ],
        classify: classify_transit,
        live_id: Some(transit_live_id),
        shape: MarkerShape::Pole,
        rings: true,
    },
    LayerDef {
        key: "shops",
        name: "Shops",
        query: "[out:json][timeout:25];node[\"shop\"]({bbox});out;",
        categories: &[
            (
                "Food & groceries",
                MarkerStyle { color: [0.25, 0.70, 0.35], pole_height: 8.0, board_width: 0.0 },
            ),
            (
                "Fashion & accessories",
                MarkerStyle { color: [0.85, 0.35, 0.60], pole_height: 8.0, board_width: 0.0 },
            ),
            (
                "Other shops",
                MarkerStyle { color: [0.62, 0.44, 0.86], pole_height: 8.0, board_width: 0.0 },
            ),
        ],
        classify: classify_shops,
        live_id: None,
        shape: MarkerShape::Pin,
        rings: false,
    },
];

fn classify_transit(tags: Option<&serde_json::Value>) -> Option<usize> {
    let tag = |k: &str| tags.and_then(|t| t.get(k)).and_then(|v| v.as_str());
    match tag("railway") {
        Some("station") | Some("halt") => return Some(0),
        Some("tram_stop") => return Some(1),
        _ => {}
    }
    (tag("highway") == Some("bus_stop")).then_some(2)
}

/// Bucket a shop node by its `shop=*` value: food, fashion, or other.
fn classify_shops(tags: Option<&serde_json::Value>) -> Option<usize> {
    let shop = tags?.get("shop")?.as_str()?;
    Some(match shop {
        "supermarket" | "convenience" | "bakery" | "butcher" | "greengrocer" | "deli"
        | "cheese" | "confectionery" | "chocolate" | "seafood" | "alcohol" | "beverages"
        | "health_food" | "frozen_food" | "pastry" | "coffee" | "tea" => 0,
        "clothes" | "shoes" | "boutique" | "jewelry" | "jewellery" | "bag" | "watches"
        | "fashion_accessories" | "leather" | "fabric" => 1,
        _ => 2,
    })
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
    /// OSM node id (for the fallback openstreetmap.org page).
    pub osm_id: i64,
    /// `website` / `contact:website` / `url` tag, if the POI has one.
    pub website: Option<String>,
    /// "street housenumber", when addr tags are present.
    pub addr: Option<String>,
}

impl PoiInstance {
    /// Camera-relative position of the anchor, exact under the floating
    /// origin: grid rebasing is a pure translation, so (poi − camera) from
    /// grid cells + local translations equals the render-space delta.
    pub fn delta(&self, cam_cell: &GridCell, cam_tf: &Transform) -> Vec3 {
        Vec3::new(
            (self.cell.x - cam_cell.x) as f32 * WORLD_CELL_EDGE + self.anchor.x
                - cam_tf.translation.x,
            (self.cell.y - cam_cell.y) as f32 * WORLD_CELL_EDGE + self.anchor.y
                - cam_tf.translation.y,
            (self.cell.z - cam_cell.z) as f32 * WORLD_CELL_EDGE + self.anchor.z
                - cam_tf.translation.z,
        )
    }
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
    /// Spawned entities per cell: the merged marker mesh + one ground ring
    /// per POI.
    cells: HashMap<(i64, i64), Vec<Entity>>,
    /// POIs of each loaded cell, in the same lifetime as its mesh entity.
    pub pois: HashMap<(i64, i64), Vec<PoiInstance>>,
}

/// A cell built on the worker: its anchor cell, merged marker mesh, and POIs.
pub struct CellBuild {
    cell: GridCell,
    /// Depth-tested geometry: poles, or pin stems + ground discs.
    mesh: Mesh,
    /// Always-on-top geometry: the floating pin heads (`None` for pole layers).
    heads: Option<Mesh>,
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
    /// Feature toggle: the OSM 3D buildings (meshes + lit windows).
    pub buildings_enabled: bool,
    /// Feature toggle: the map ground (streamed OSM tiles + region plane).
    pub map_enabled: bool,
    /// Feature toggle: live moving vehicles on the transit layer (ADR-026).
    pub vehicles_enabled: bool,
    /// Info card for a clicked POI (name, address, website links).
    pub popup: Option<PoiPopup>,
    /// Anchor: tile_x, tile_y, zoom, latitude — derived from the map anchor
    /// (`MapStream`), so pins follow re-anchors like the buildings (ADR-025).
    anchor: Option<(i64, i64, u32, f64)>,
    /// The `MapStream::anchor_generation` the loaded cells belong to; a change
    /// resets them for the new origin.
    synced_gen: u32,
    /// Worker job queue: (layer index, cell x, cell y).
    jobs: Option<Sender<(usize, i64, i64)>>,
    results: Arc<Mutex<Vec<CellResult>>>,
    /// One unlit material shared by all depth-tested marker geometry (hue is
    /// per-vertex): poles, pin stems, pin ground discs.
    material: Option<Handle<StandardMaterial>>,
    /// Shared always-on-top material for the floating pin heads.
    head_material: Option<Handle<PinHeadMaterial>>,
    /// Shared unit annulus for the ground rings (scaled per frame).
    ring_mesh: Option<Handle<Mesh>>,
    /// One translucent ring material per (layer, category); their shared
    /// alpha is pulsed by [`animate_stop_rings`].
    ring_materials: HashMap<(usize, usize), Handle<StandardMaterial>>,
    pub layers: Vec<LayerRuntime>,
}

impl Default for DataLayers {
    fn default() -> Self {
        Self {
            panel_open: false,
            buildings_enabled: true,
            map_enabled: true,
            vehicles_enabled: false,
            popup: None,
            anchor: None,
            synced_gen: 0,
            jobs: None,
            results: Arc::default(),
            material: None,
            head_material: None,
            ring_mesh: None,
            ring_materials: HashMap::new(),
            // Overlay layers (transit stops, shops) start off — only the base
            // map tiles and 3D buildings are on at startup; the rest are opt-in
            // from the Layers panel.
            layers: LAYER_DEFS.iter().map(|_| LayerRuntime::default()).collect(),
        }
    }
}

impl DataLayers {
    /// The live-feed anchor once established: base tile `(x, y)`, zoom, and the
    /// anchor latitude — the frame `lat_lng_to_local` maps into. Used by
    /// `transit_vehicles` to place live vehicles the same way pins are placed.
    pub fn anchor(&self) -> Option<(i64, i64, u32, f64)> {
        self.anchor
    }
}

/// Marker component for spawned data-layer cell meshes.
#[derive(Component)]
pub struct DataLayerMarkers;

/// A radiating ground ring at a POI (subtle visibility aid; the signs
/// themselves are real-world sized and easy to miss from a distance).
#[derive(Component)]
pub struct StopRing;

/// Info card for a clicked POI, anchored above its pole.
#[derive(Clone)]
pub struct PoiPopup {
    pub cell: GridCell,
    pub anchor: Vec3,
    pub title: String,
    pub subtitle: String,
    pub website: Option<String>,
    pub osm_id: i64,
}

/// Stream marker cells for every enabled layer as the camera roams, and apply
/// enable/disable toggles from the panel by flipping entity visibility.
pub fn update_data_layers(
    mut commands: Commands,
    mut dl: ResMut<DataLayers>,
    mut meshes: ResMut<Assets<Mesh>>,
    mut materials: ResMut<Assets<StandardMaterial>>,
    mut head_materials: ResMut<Assets<PinHeadMaterial>>,
    stream: Res<crate::systems::map_stream::MapStream>,
    globe: Res<crate::systems::globe::GlobeState>,
    camera: Query<(&GridCell, &Transform), With<FreeCamera>>,
    bigspace: Query<Entity, With<BigSpace>>,
) {
    let Some((clat, clng, base_zoom)) = stream.geo_anchor() else {
        return;
    };

    // Re-anchor (ADR-025): when the map origin moves, drop every loaded cell and
    // re-establish at the new location, exactly like the buildings. Dropping
    // `jobs` retires the old worker; a fresh `results` orphans in-flight builds.
    if dl.synced_gen != stream.anchor_generation {
        dl.synced_gen = stream.anchor_generation;
        for layer in dl.layers.iter_mut() {
            for (_, ents) in layer.cells.drain() {
                for e in ents {
                    commands.entity(e).despawn();
                }
            }
            layer.pois.clear();
            layer.requested.clear();
            layer.failed.clear();
            layer.retry_at.clear();
        }
        dl.popup = None;
        dl.jobs = None;
        dl.results = Arc::default();
        dl.anchor = None;
    }

    // Establish the anchor (from the map origin) and start the cell worker.
    if dl.anchor.is_none() {
        let z = base_zoom;
        let (ax, ay) = vibe_core::world::lat_lng_to_tile(clat, clng, z);
        let lat = clat;
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

    // These are *surface* layers — sub-100 m POI markers, meaningless from
    // altitude. Freeze streaming when the camera is high (and certainly from
    // orbit, where the flat-frame position sweeps across the planet and every
    // "nearby" cell would be a wasted Overpass query — this was hammering it).
    if globe.altitude_m(cell, tf) > SURFACE_LAYER_MAX_ALT {
        return;
    }

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
                let mut entities = vec![commands
                    .spawn((
                        Mesh3d(meshes.add(build.mesh)),
                        MeshMaterial3d(material),
                        Transform::from_translation(Vec3::new(0.0, 0.1, 0.0)),
                        build.cell,
                        ChildOf(root),
                        visibility,
                        DataLayerMarkers,
                    ))
                    .id()];
                // Floating pin heads render always-on-top, on their own material,
                // so buildings never hide them (see PinHeadMaterial).
                if let Some(head_mesh) = build.heads {
                    let head_material = dl
                        .head_material
                        .get_or_insert_with(|| {
                            head_materials.add(PinHeadMaterial {
                                base: StandardMaterial {
                                    base_color: Color::WHITE,
                                    unlit: true,
                                    double_sided: true,
                                    cull_mode: None,
                                    alpha_mode: AlphaMode::Blend,
                                    ..default()
                                },
                                extension: DepthAlways {},
                            })
                        })
                        .clone();
                    entities.push(
                        commands
                            .spawn((
                                Mesh3d(meshes.add(head_mesh)),
                                MeshMaterial3d(head_material),
                                Transform::from_translation(Vec3::new(0.0, 0.1, 0.0)),
                                build.cell,
                                ChildOf(root),
                                visibility,
                                DataLayerMarkers,
                                bevy::light::NotShadowCaster,
                                bevy::light::NotShadowReceiver,
                            ))
                            .id(),
                    );
                }
                // One radiating ground ring per POI (shared mesh; one material
                // per category, pulsed by animate_stop_rings) — only for
                // layers that opted in.
                let ring_mesh = dl
                    .ring_mesh
                    .get_or_insert_with(|| meshes.add(Annulus::new(0.85, 1.0)))
                    .clone();
                for poi in build.pois.iter().filter(|_| LAYER_DEFS[li].rings) {
                    let c = LAYER_DEFS[li].categories[poi.category].1.color;
                    let ring_mat = dl
                        .ring_materials
                        .entry((li, poi.category))
                        .or_insert_with(|| {
                            materials.add(StandardMaterial {
                                base_color: Color::srgba(c[0], c[1], c[2], 0.25),
                                unlit: true,
                                alpha_mode: AlphaMode::Blend,
                                ..default()
                            })
                        })
                        .clone();
                    entities.push(
                        commands
                            .spawn((
                                Mesh3d(ring_mesh.clone()),
                                MeshMaterial3d(ring_mat),
                                Transform::from_translation(Vec3::new(
                                    poi.anchor.x,
                                    0.08,
                                    poi.anchor.z,
                                ))
                                .with_rotation(Quat::from_rotation_x(
                                    -std::f32::consts::FRAC_PI_2,
                                ))
                                .with_scale(Vec3::splat(0.4)),
                                poi.cell,
                                ChildOf(root),
                                visibility,
                                StopRing,
                                bevy::light::NotShadowCaster,
                                bevy::light::NotShadowReceiver,
                            ))
                            .id(),
                    );
                }
                dl.layers[li].cells.insert(cell, entities);
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
            for entity in rt.cells.remove(&c).unwrap_or_default() {
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
            for &entity in rt.cells.values().flatten() {
                commands.entity(entity).insert(visibility);
            }
        }
    }
}

/// Left-click on a POI marker: open (or move) its info card. Clicking empty
/// world closes it. Hit test is done in screen space — each POI's head is
/// projected to the viewport and the one nearest the cursor within a forgiving
/// pixel radius wins. That matches what the user actually clicks (the visible
/// pin), and is reliable even where markers are dense and overlapping, unlike a
/// world-space ray against thin, short poles.
pub fn handle_poi_click(
    mouse: Res<ButtonInput<MouseButton>>,
    mut egui: ResMut<EguiManager>,
    mut dl: ResMut<DataLayers>,
    windows: Query<&Window>,
    camera: Query<(&Camera, &GlobalTransform, &GridCell, &Transform), With<FreeCamera>>,
) {
    if !mouse.just_pressed(MouseButton::Left) {
        return;
    }
    // Clicks on the UI (panels, the popup itself) are egui's, not the world's.
    if egui.ctx_mut().egui_wants_pointer_input() {
        return;
    }
    let Some(cursor) = windows.iter().next().and_then(|w| w.cursor_position()) else {
        return;
    };
    let Ok((cam, cam_gt, cam_cell, cam_tf)) = camera.single() else {
        return;
    };

    /// Cursor-to-head pixel radius that still counts as a click on the marker.
    const HIT_RADIUS_PX: f32 = 22.0;
    /// Markers past this range aren't deliberate click targets.
    const MAX_RANGE_M: f32 = 500.0;
    // Best candidate: (pixel distance, camera distance, popup, layer, category).
    let mut best: Option<(f32, f32, PoiPopup, usize, usize)> = None;
    for (li, rt) in dl.layers.iter().enumerate() {
        if !rt.enabled {
            continue;
        }
        for poi in rt.pois.values().flatten() {
            let world = cam_gt.translation() + poi.delta(cam_cell, cam_tf);
            let dist = world.distance(cam_gt.translation());
            if dist > MAX_RANGE_M {
                continue;
            }
            let Ok(screen) = cam.world_to_viewport(cam_gt, world) else {
                continue; // behind the camera
            };
            let px = screen.distance(cursor);
            if px > HIT_RADIUS_PX {
                continue;
            }
            // Nearest to the cursor wins; on a near-tie the closer marker wins,
            // so a foreground pin beats a distant one projecting behind it.
            let better = best.as_ref().is_none_or(|(bpx, bdist, ..)| {
                if (px - bpx).abs() < 6.0 {
                    dist < *bdist
                } else {
                    px < *bpx
                }
            });
            if better {
                best = Some((
                    px,
                    dist,
                    PoiPopup {
                        cell: poi.cell,
                        anchor: poi.anchor,
                        title: poi
                            .name
                            .clone()
                            .unwrap_or_else(|| LAYER_DEFS[li].categories[poi.category].0.to_string()),
                        subtitle: String::new(),
                        website: poi.website.clone(),
                        osm_id: poi.osm_id,
                    },
                    li,
                    poi.category,
                ));
            }
        }
    }
    dl.popup = best.map(|(_, _, mut popup, li, cat)| {
        let category = LAYER_DEFS[li].categories[cat].0;
        popup.subtitle = match dl
            .layers[li]
            .pois
            .values()
            .flatten()
            .find(|p| p.osm_id == popup.osm_id)
            .and_then(|p| p.addr.clone())
        {
            Some(addr) => format!("{category} · {addr}"),
            None => category.to_string(),
        };
        popup
    });
}

/// The clicked-POI info card: name, category/address, and Website / OSM
/// buttons. Anchored above the POI's pole; closes via ✕ or a click elsewhere.
pub fn render_poi_popup(
    mut egui: ResMut<EguiManager>,
    mut dl: ResMut<DataLayers>,
    camera: Query<(&Camera, &GlobalTransform, &GridCell, &Transform), With<FreeCamera>>,
) {
    let Some(popup) = dl.popup.clone() else {
        return;
    };
    let Ok((cam, cam_gt, cam_cell, cam_tf)) = camera.single() else {
        return;
    };
    let world = cam_gt.translation()
        + Vec3::new(
            (popup.cell.x - cam_cell.x) as f32 * WORLD_CELL_EDGE + popup.anchor.x
                - cam_tf.translation.x,
            (popup.cell.y - cam_cell.y) as f32 * WORLD_CELL_EDGE + popup.anchor.y
                - cam_tf.translation.y,
            (popup.cell.z - cam_cell.z) as f32 * WORLD_CELL_EDGE + popup.anchor.z
                - cam_tf.translation.z,
        );
    let Ok(screen) = cam.world_to_viewport(cam_gt, world) else {
        return; // behind the camera; keep the popup for when we turn back
    };

    let mut close = false;
    let mut open: Option<String> = None;
    egui::Area::new(egui::Id::new("poi_popup"))
        .fixed_pos(egui::pos2(screen.x, screen.y))
        .pivot(egui::Align2::CENTER_BOTTOM)
        .show(egui.ctx_mut(), |ui| {
            egui::Frame::NONE
                .fill(egui::Color32::from_rgba_unmultiplied(12, 16, 24, 235))
                .corner_radius(4.0)
                .inner_margin(egui::Margin::symmetric(10, 8))
                .show(ui, |ui| {
                    ui.set_max_width(240.0);
                    ui.horizontal(|ui| {
                        ui.label(
                            egui::RichText::new(&popup.title)
                                .strong()
                                .size(14.0)
                                .color(egui::Color32::WHITE),
                        );
                        ui.with_layout(
                            egui::Layout::right_to_left(egui::Align::Center),
                            |ui| {
                                if ui.small_button("✕").clicked() {
                                    close = true;
                                }
                            },
                        );
                    });
                    ui.label(
                        egui::RichText::new(&popup.subtitle)
                            .size(12.0)
                            .color(egui::Color32::from_gray(170)),
                    );
                    ui.add_space(4.0);
                    ui.horizontal(|ui| {
                        if let Some(url) = &popup.website {
                            if ui.button("Website ↗").clicked() {
                                open = Some(url.clone());
                            }
                        }
                        if ui.button("OSM").clicked() {
                            open = Some(format!(
                                "https://www.openstreetmap.org/node/{}",
                                popup.osm_id
                            ));
                        }
                    });
                });
        });
    if let Some(url) = open {
        open_url(&url);
    }
    if close {
        dl.popup = None;
    }
}

/// Open a URL in the system browser. Scheme-restricted to http(s): OSM tag
/// values are untrusted data, and a bare domain gets https:// prepended.
fn open_url(url: &str) {
    let url = url.trim();
    let url = if url.starts_with("http://") || url.starts_with("https://") {
        url.to_string()
    } else if url.contains("://") {
        tracing::warn!(url, "refusing non-http(s) website tag");
        return;
    } else {
        format!("https://{url}")
    };
    tracing::info!(%url, "opening in browser");
    #[cfg(target_os = "linux")]
    let result = std::process::Command::new("xdg-open").arg(&url).spawn();
    #[cfg(target_os = "macos")]
    let result = std::process::Command::new("open").arg(&url).spawn();
    #[cfg(target_os = "windows")]
    let result = std::process::Command::new("cmd").args(["/C", "start", "", &url]).spawn();
    if let Err(e) = result {
        tracing::warn!(error = %e, "could not open browser");
    }
}

/// Apply the Buildings / Map feature toggles by writing entity visibility.
/// Runs every frame with compare-before-write, so it is cheap when nothing
/// changed and newly streamed-in entities pick up the current toggle state.
pub fn apply_feature_toggles(
    dl: Res<DataLayers>,
    globe: Res<crate::systems::globe::GlobeState>,
    camera: Query<(&GridCell, &Transform), With<FreeCamera>>,
    mut buildings: Query<
        &mut Visibility,
        Or<(
            With<crate::systems::osm_buildings::OsmBuildingMesh>,
            With<crate::systems::osm_buildings::WindowGlow>,
        )>,
    >,
    mut map: Query<
        &mut Visibility,
        (
            Or<(
                With<crate::systems::map_stream::StreamTile>,
                With<crate::systems::rendering::RegionMesh>,
            )>,
            Without<crate::systems::osm_buildings::OsmBuildingMesh>,
            Without<crate::systems::osm_buildings::WindowGlow>,
        ),
    >,
) {
    // Sole visibility writer for these entities: the panel toggle AND the space
    // tier (from orbit the flat patch floats over the globe, parallax-shifted —
    // ADR-025). Folding both in here avoids racing another writer.
    let in_space = camera
        .single()
        .map(|(cell, tf)| globe.in_space(cell, tf))
        .unwrap_or(false);
    let want = if dl.buildings_enabled && !in_space {
        Visibility::Inherited
    } else {
        Visibility::Hidden
    };
    for mut vis in buildings.iter_mut() {
        if *vis != want {
            *vis = want;
        }
    }
    let want = if dl.map_enabled && !in_space {
        Visibility::Inherited
    } else {
        Visibility::Hidden
    };
    for mut vis in map.iter_mut() {
        if *vis != want {
            *vis = want;
        }
    }
}

/// Pulse the ground rings: every ring expands from its POI and fades out over
/// one period, all in phase (one transform write per ring, one alpha write per
/// category material).
pub fn animate_stop_rings(
    time: Res<Time>,
    dl: Res<DataLayers>,
    mut materials: ResMut<Assets<StandardMaterial>>,
    mut rings: Query<&mut Transform, With<StopRing>>,
) {
    const PERIOD: f32 = 3.0;
    const MAX_RADIUS: f32 = 4.5;
    let phase = (time.elapsed_secs() % PERIOD) / PERIOD;
    let scale = 0.4 + phase * (MAX_RADIUS - 0.4);
    for mut tf in rings.iter_mut() {
        tf.scale = Vec3::splat(scale);
    }
    // Quadratic fade-out keeps it subtle: strongest just as the pulse starts.
    let alpha = 0.25 * (1.0 - phase) * (1.0 - phase);
    for handle in dl.ring_materials.values() {
        if let Some(mut mat) = materials.get_mut(handle) {
            mat.base_color = mat.base_color.with_alpha(alpha);
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
    let mut close = false;
    egui::Panel::right("layers_panel")
        .resizable(true)
        .default_size(260.0)
        .show(egui.ui_mut(), |ui| {
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
                    // Live moving vehicles ride on the transit layer (ADR-026).
                    if i == TRANSIT_LAYER {
                        ui.add_space(2.0);
                        ui.checkbox(
                            &mut dl.vehicles_enabled,
                            egui::RichText::new("Live vehicles").size(13.0),
                        );
                    }
                });
                ui.add_space(4.0);
                ui.separator();
            }
            ui.add_space(4.0);
            ui.checkbox(
                &mut dl.buildings_enabled,
                egui::RichText::new("Buildings").size(14.0),
            );
            ui.indent("buildings", |ui| {
                // 3DBAG serves covered NL areas (real BAG heights); OSM elsewhere.
                let label = if crate::systems::osm_buildings::tdbag_attribution().is_some() {
                    "3DBAG (NL) + OSM 3D buildings"
                } else {
                    "OSM 3D buildings and windows"
                };
                ui.label(egui::RichText::new(label).size(13.0));
            });
            ui.add_space(4.0);
            ui.separator();
            ui.add_space(4.0);
            ui.checkbox(&mut dl.map_enabled, egui::RichText::new("Map").size(14.0));
            ui.indent("map", |ui| {
                ui.label(egui::RichText::new("OSM ground tiles").size(13.0));
            });
            ui.add_space(4.0);
            ui.separator();
            ui.with_layout(egui::Layout::bottom_up(egui::Align::LEFT), |ui| {
                ui.add_space(6.0);
                if let Some(credit) = crate::systems::osm_buildings::tdbag_attribution() {
                    ui.small(credit);
                }
                ui.small("Data © OpenStreetMap contributors (ODbL), via Overpass API");
                ui.small("Live departures & vehicles: OVapi (v0 / gtfs.ovapi.nl)");
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
    osm_id: i64,
    website: Option<String>,
    addr: Option<String>,
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
    let mut heads = MeshData::default();
    // Density LOD: cap markers per cell with an even subsample, so a shop-dense
    // cell doesn't emit hundreds of always-on-top transparent pin heads at once
    // (the overdraw that hangs a weak GPU). Instances are capped in lockstep, so
    // only rendered markers stay clickable.
    let step = pois.len().div_ceil(MAX_MARKERS_PER_CELL).max(1);
    let mut instances = Vec::with_capacity(pois.len() / step + 1);
    for poi in pois.iter().step_by(step) {
        let w = lat_lng_to_local(poi.lat, poi.lng, ax, ay, z, tile_m);
        let local = Vec2::new(w.x - ox, w.y - oz);
        let style = &def.categories[poi.category].1;
        push_marker(&mut buf, &mut heads, local, def.shape, style);
        // Anchor (click target + popup + board mount): just clear of the pole's
        // sign plate, or the gem-head centre for a pin.
        let anchor_y = match def.shape {
            MarkerShape::Pole => style.pole_height + 0.5,
            MarkerShape::Pin => style.pole_height,
        };
        instances.push(PoiInstance {
            cell: grid_cell,
            anchor: Vec3::new(local.x, anchor_y, local.y),
            name: poi.name.clone(),
            live_id: poi.live_id.clone(),
            category: poi.category,
            osm_id: poi.osm_id,
            website: poi.website.clone(),
            addr: poi.addr.clone(),
        });
    }
    Ok(buf.into_mesh().map(|mesh| CellBuild {
        cell: grid_cell,
        mesh,
        heads: heads.into_mesh(),
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
            let tag = |k: &str| {
                tags.and_then(|t| t.get(k))
                    .and_then(|v| v.as_str())
                    .map(str::to_string)
            };
            // First website-ish tag; OSM values sometimes list several ("a; b").
            let website = ["website", "contact:website", "url"]
                .iter()
                .find_map(|k| tag(k))
                .map(|w| w.split(';').next().unwrap_or("").trim().to_string())
                .filter(|w| !w.is_empty());
            let addr = match (tag("addr:street"), tag("addr:housenumber")) {
                (Some(s), Some(n)) => Some(format!("{s} {n}")),
                (Some(s), None) => Some(s),
                _ => None,
            };
            out.push(Poi {
                lat,
                lng,
                category,
                name: tag("name"),
                live_id: def.live_id.and_then(|f| f(tags)),
                osm_id: el.get("id").and_then(|v| v.as_i64()).unwrap_or(0),
                website,
                addr,
            });
        }
    }
    Ok(out)
}

/// Anthracite pole / stem tone, like real Dutch street furniture.
const POLE_COLOR: [f32; 4] = [0.13, 0.14, 0.15, 1.0];

/// Append one marker at ground point `p`, per the layer's [`MarkerShape`].
/// Depth-tested geometry (poles, pin stems + discs) goes to `base`; the floating
/// pin head — which draws always-on-top — goes to `heads`. Cull is off on both
/// marker materials, so triangle winding is irrelevant.
fn push_marker(
    base: &mut MeshData,
    heads: &mut MeshData,
    p: Vec2,
    shape: MarkerShape,
    style: &MarkerStyle,
) {
    match shape {
        MarkerShape::Pole => push_pole(base, p, style),
        MarkerShape::Pin => push_pin(base, heads, p, style),
    }
}

/// Append one stop pole at `p`: a rectangular anthracite column (0.18×0.12 m
/// section) — the R-NET-style mast the live display mounts on.
fn push_pole(buf: &mut MeshData, p: Vec2, style: &MarkerStyle) {
    let h = style.pole_height;
    let (hx, hz) = (0.09, 0.06);
    let (x0, x1) = (p.x - hx, p.x + hx);
    let (z0, z1) = (p.y - hz, p.y + hz);
    let v = Vec3::new;

    buf.quad(v(x0, 0.0, z1), v(x1, 0.0, z1), v(x1, h, z1), v(x0, h, z1), POLE_COLOR);
    buf.quad(v(x0, 0.0, z0), v(x1, 0.0, z0), v(x1, h, z0), v(x0, h, z0), POLE_COLOR);
    buf.quad(v(x0, 0.0, z0), v(x0, 0.0, z1), v(x0, h, z1), v(x0, h, z0), POLE_COLOR);
    buf.quad(v(x1, 0.0, z0), v(x1, 0.0, z1), v(x1, h, z1), v(x1, h, z0), POLE_COLOR);
    buf.quad(v(x0, h, z0), v(x1, h, z0), v(x1, h, z1), v(x0, h, z1), POLE_COLOR);
}

/// Append one floating map pin at ground point `p`: a small colour disc on the
/// pavement and a slim dark stem (into `base`, depth-tested), plus a colour-coded
/// gem head (hexagonal bipyramid, point down) hovering at `style.pole_height`
/// (into `heads`, always-on-top). The head is shaded bright on top and base-hue
/// below so it reads as 3D under the unlit marker material — a recognisable,
/// clickable pin that stays visible above the shopfronts.
fn push_pin(base: &mut MeshData, heads: &mut MeshData, p: Vec2, style: &MarkerStyle) {
    let v = Vec3::new;
    let hue = [style.color[0], style.color[1], style.color[2], 1.0];
    // Upper facets: category hue lifted toward white for a lit-from-above look.
    let light = [
        style.color[0] * 0.55 + 0.45,
        style.color[1] * 0.55 + 0.45,
        style.color[2] * 0.55 + 0.45,
        1.0,
    ];

    const SEG: usize = 6; // hexagonal head + ground disc
    const R: f32 = 0.85; // head horizontal radius
    let hc = style.pole_height; // head centre height
    let top = hc + R; // rounded crown
    let tip = hc - R * 1.9; // elongated point, aimed down at the spot

    // Gem head (always-on-top): equator ring fanned to a top crown and bottom tip.
    let ring: [Vec3; SEG] = std::array::from_fn(|i| {
        let a = std::f32::consts::TAU * i as f32 / SEG as f32;
        v(p.x + R * a.cos(), hc, p.y + R * a.sin())
    });
    let crown = v(p.x, top, p.y);
    let point = v(p.x, tip, p.y);
    for i in 0..SEG {
        let (a, b) = (ring[i], ring[(i + 1) % SEG]);
        heads.tri(a, b, crown, light); // upper facet
        heads.tri(b, a, point, hue); // lower facet
    }

    // Slim dark stem from just above the ground disc up to the head's tip.
    let s = 0.05;
    let (x0, x1) = (p.x - s, p.x + s);
    let (z0, z1) = (p.y - s, p.y + s);
    let (y0, y1) = (0.05, tip);
    base.quad(v(x0, y0, z1), v(x1, y0, z1), v(x1, y1, z1), v(x0, y1, z1), POLE_COLOR);
    base.quad(v(x0, y0, z0), v(x1, y0, z0), v(x1, y1, z0), v(x0, y1, z0), POLE_COLOR);
    base.quad(v(x0, y0, z0), v(x0, y0, z1), v(x0, y1, z1), v(x0, y1, z0), POLE_COLOR);
    base.quad(v(x1, y0, z0), v(x1, y0, z1), v(x1, y1, z1), v(x1, y1, z0), POLE_COLOR);

    // Ground disc marking the exact footprint (category hue, hexagonal fan).
    const DR: f32 = 0.6;
    let dy = 0.03;
    let centre = v(p.x, dy, p.y);
    let disc: [Vec3; SEG] = std::array::from_fn(|i| {
        let a = std::f32::consts::TAU * i as f32 / SEG as f32;
        v(p.x + DR * a.cos(), dy, p.y + DR * a.sin())
    });
    for i in 0..SEG {
        base.tri(centre, disc[i], disc[(i + 1) % SEG], hue);
    }
}
