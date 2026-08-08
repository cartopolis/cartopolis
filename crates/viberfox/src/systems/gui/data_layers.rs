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

use crate::components::FreeCamera;
use crate::components::{DataLayerMarkers, StopRing};
use crate::grid::{WORLD_CELL_EDGE, WORLD_SWITCH_THRESHOLD};
use std::collections::HashMap;

use bevy::pbr::{
    ExtendedMaterial, MaterialExtension, MaterialExtensionKey, MaterialExtensionPipeline,
};
use bevy::prelude::*;
use bevy::render::mesh::MeshVertexBufferLayoutRef;
use bevy::render::render_resource::{
    AsBindGroup, CompareFunction, RenderPipelineDescriptor, SpecializedMeshPipelineError,
};
use bevy::tasks::IoTaskPool;
use big_space::prelude::{BigSpace, Grid, GridCell};

use crate::platform::storage;
use crate::platform::stream::{CellStream, Policy};
use crate::systems::egui_manager::EguiManager;
use crate::systems::osm_buildings::{lat_lng_to_local, overpass_query, MeshData};
use crate::systems::theme;
use viberfox_core::world::{tile_to_lat_lng, tile_to_meters};

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
/// Marker entities freed per frame.
///
/// Counted in *entities*, not cells, because one cell is a merged marker mesh plus
/// a ground ring per POI — up to `MAX_MARKERS_PER_CELL` + 2 of them. A re-anchor
/// retires every cell of every enabled layer at once, which is hundreds; freeing
/// that many in one command buffer is the `VK_ERROR_DEVICE_LOST` burst CLAUDE.md
/// documents. This is a couple of cells' worth per frame, so a full teardown
/// clears in well under a second while never packing one giant buffer.
const MAX_MARKER_DESPAWNS_PER_FRAME: usize = 32;
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
        "clothes"
        | "shoes"
        | "boutique"
        | "jewelry"
        | "jewellery"
        | "bag"
        | "watches"
        | "fashion_accessories"
        | "leather"
        | "fabric" => 1,
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

/// Per-layer streaming state. The requested/failed/backoff bookkeeping is *not*
/// here: it lives in the one shared [`CellStream`] keyed by `(layer, cell)`, so all
/// the layers share the single Overpass-throttled admission slot they have to.
#[derive(Default)]
pub struct LayerRuntime {
    /// Panel checkbox: whether this layer's markers are shown (and streamed).
    pub enabled: bool,
    /// Visibility last applied to the spawned entities.
    shown: bool,
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

/// What identifies one streamed cell: which layer, and where. One key space for
/// every layer, because they share one Overpass budget.
type LayerCell = (usize, i64, i64);

/// A completed cell build (`None` for cells without matching POIs).
type CellResult = Result<Option<CellBuild>, String>;

/// Data-layer streaming + panel state.
#[derive(Resource)]
pub struct DataLayers {
    /// Whether the Data Layers panel window is open (View ▸ Data Layers).
    pub panel_open: bool,
    /// Feature toggle: the OSM 3D buildings (meshes + lit windows).
    pub buildings_enabled: bool,
    /// Feature toggle (under Buildings): the 3DBAG source — LoD2.2 real meshes
    /// (`lod22.rs`) plus LoD1.2 real-height footprints over covered NL cells.
    /// Off = OSM extrusions everywhere; mirrored into `osm_buildings`' coverage
    /// gate by [`apply_feature_toggles`].
    pub tdbag_enabled: bool,
    /// Feature toggle (under Buildings): the OSM source — extruded footprints
    /// and lit windows. The fallback beneath 3DBAG: where 3DBAG covers (and is
    /// on), OSM already skips the cell, so this only ever removes the fallback.
    pub osm_buildings_enabled: bool,
    /// Feature toggle: the map ground (streamed OSM tiles + region plane).
    pub map_enabled: bool,
    /// Feature toggle: live moving vehicles on the transit layer (ADR-026).
    pub vehicles_enabled: bool,
    /// Feature toggle (under Map): the merged tree canopies/trunks
    /// (`systems::vegetation`). Gates their visibility on top of the altitude LOD.
    pub trees_enabled: bool,
    /// Feature toggle (under Map): the merged street furniture
    /// (`systems::street_furniture`) — benches, bins, shelters read out of the
    /// tile's `pois` layer. Gates their visibility on top of the altitude LOD.
    pub furniture_enabled: bool,
    /// Feature toggle (under Map): textured road surfaces (generated tarmac +
    /// markings). Off = flat-coloured roads (`map_geometry::apply_road_texture`).
    pub roads_textured: bool,
    /// Feature toggle (under Map): the animated water surface shader (waves, foam,
    /// chop). Off = calm flat water (`water::update_water_waves`).
    pub water_shading: bool,
    /// Feature toggle (under Map): street names written over their streets
    /// (`systems::street_labels`). Screen-space text, so this gates the drawing
    /// only — the names are parsed with the cell either way, off the frame loop.
    pub street_labels: bool,
    /// Feature toggle: the **height layer** (`systems::height`) — measured ground
    /// and surface elevation from PDOK's AHN. Its own source rather than a Map
    /// sub-toggle, like Buildings: it answers only over the Netherlands, and off
    /// means every consumer falls back to the flat-ground assumption.
    pub height_enabled: bool,
    /// Info card for a clicked POI (name, address, website links).
    pub popup: Option<PoiPopup>,
    /// Terrain-field revision the live marker cells were built against.
    terrain_revision: u32,
    /// Anchor: tile_x, tile_y, zoom, latitude — derived from the map anchor
    /// (`MapStream`), so pins follow re-anchors like the buildings (ADR-025).
    anchor: Option<(i64, i64, u32, f64)>,
    /// Cell streaming for every layer at once (`platform::stream`): the
    /// requested/failed sets with their exponential backoff, the results sink, the
    /// serial Overpass-throttled admission and the bounded despawn queue.
    cells: CellStream<LayerCell, CellResult>,
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
    /// State captured when driver mode started, so "minimal mode" — stripping the
    /// map to just the vector tiles + the route while driving — can restore exactly
    /// what the user had when they stop. `None` when not driving.
    minimal: Option<MinimalSave>,
}

impl DataLayers {
    /// Snapshot the panel's toggles for persistence (`data/user.json`). While
    /// driver mode's minimal mode is active, the user's *real* preferences live
    /// in its snapshot — persist those, so exiting mid-drive doesn't freeze the
    /// stripped-down map into the file.
    pub fn prefs(&self) -> crate::systems::user_store::LayerPrefs {
        let (buildings, vehicles, trees, furniture, overlay) = match &self.minimal {
            Some(m) => (
                m.buildings,
                m.vehicles,
                m.trees,
                m.furniture,
                Some(&m.layers),
            ),
            None => (
                self.buildings_enabled,
                self.vehicles_enabled,
                self.trees_enabled,
                self.furniture_enabled,
                None,
            ),
        };
        crate::systems::user_store::LayerPrefs {
            buildings,
            tdbag: self.tdbag_enabled,
            osm_buildings: self.osm_buildings_enabled,
            map: self.map_enabled,
            trees,
            roads_textured: self.roads_textured,
            water_shading: self.water_shading,
            street_labels: self.street_labels,
            furniture,
            height: self.height_enabled,
            vehicles,
            overlays: LAYER_DEFS
                .iter()
                .enumerate()
                .map(|(i, def)| {
                    let on = overlay
                        .and_then(|l| l.get(i).copied())
                        .unwrap_or(self.layers[i].enabled);
                    (def.key.to_string(), on)
                })
                .collect(),
        }
    }

    /// The sky settings to persist, for the same reason [`Self::prefs`] exists:
    /// driver mode's minimal sky is a *mode*, not a preference. Quitting mid-drive
    /// would otherwise save "manual, no clouds, no rain" and hand that back as the
    /// user's own choice on the next launch — so while `minimal` holds a snapshot,
    /// the snapshot is what gets written.
    pub fn sky_prefs(
        &self,
        dev: &crate::resources::DevPanelState,
        weather: &crate::systems::weather::WeatherState,
    ) -> crate::systems::user_store::SkyPrefs {
        let (weather_live, fog_density, cloud_cover, precipitation) = match &self.minimal {
            Some(m) => (
                m.weather_live,
                m.fog_density,
                m.cloud_cover,
                m.precipitation,
            ),
            None => (
                weather.live,
                dev.fog_density,
                dev.cloud_cover,
                dev.precipitation,
            ),
        };
        crate::systems::user_store::SkyPrefs {
            sun_mode: dev.sun_mode,
            time_of_day: (dev.sun_mode == crate::resources::SunMode::Manual)
                .then_some(dev.time_of_day),
            weather_live,
            fog_density,
            cloud_cover,
            precipitation,
        }
    }

    /// Apply persisted toggles at startup. Overlay layers missing from the file
    /// (added since it was written) keep their defaults.
    pub fn apply_prefs(&mut self, p: &crate::systems::user_store::LayerPrefs) {
        self.buildings_enabled = p.buildings;
        self.tdbag_enabled = p.tdbag;
        self.osm_buildings_enabled = p.osm_buildings;
        self.map_enabled = p.map;
        self.trees_enabled = p.trees;
        self.roads_textured = p.roads_textured;
        self.water_shading = p.water_shading;
        self.street_labels = p.street_labels;
        self.furniture_enabled = p.furniture;
        self.height_enabled = p.height;
        self.vehicles_enabled = p.vehicles;
        for (i, def) in LAYER_DEFS.iter().enumerate() {
            if let Some(on) = p.overlays.get(def.key) {
                self.layers[i].enabled = *on;
            }
        }
    }
}

/// Snapshot of everything minimal mode turns off, taken when driving starts, so
/// stopping puts the map back exactly as it was. Weather lives in other
/// resources ([`DevPanelState`], [`WeatherState`]) but is restored together here.
struct MinimalSave {
    buildings: bool,
    vehicles: bool,
    trees: bool,
    furniture: bool,
    layers: Vec<bool>,
    fog_density: f32,
    cloud_cover: f32,
    precipitation: f32,
    weather_live: bool,
}

impl Default for DataLayers {
    fn default() -> Self {
        // Endpoint-derived, and read once here rather than per anchor: the stream's
        // policy is fixed at construction and `CellStream::reset` rebuilds the
        // dispatcher from it, so a re-anchor keeps the same pace without restating
        // it.
        let (overpass_concurrency, spacing) = crate::systems::osm_buildings::overpass_pace();
        let overpass_spacing = spacing.as_secs_f64();
        Self {
            panel_open: false,
            buildings_enabled: true,
            tdbag_enabled: true,
            osm_buildings_enabled: true,
            map_enabled: true,
            vehicles_enabled: false,
            trees_enabled: true,
            furniture_enabled: true,
            roads_textured: true,
            water_shading: true,
            street_labels: true,
            // Off by default: it reaches a third party's service and only
            // answers over the Netherlands, so opting in is the honest default.
            height_enabled: false,
            terrain_revision: 0,
            popup: None,
            anchor: None,
            cells: CellStream::new(Policy {
                // Paced by the endpoint (`overpass_pace`): concurrent and unspaced
                // against our own Overpass, serial and throttled against a public
                // one, which is owed one query at a time. The budget is shared
                // across the layers too, which is why this is one stream keyed by
                // (layer, cell) rather than one per layer.
                max_in_flight: overpass_concurrency,
                min_interval_secs: overpass_spacing,
                max_attempts: MAX_CELL_ATTEMPTS,
                // 15 s, 30 s, 1 m, 2 m, 4 m — long enough to outlast an Overpass
                // wobble without burning the attempt budget while it is overloaded.
                backoff_secs: 15.0,
                backoff_doublings: 4,
                cooldown_secs: 0.0,
                despawns_per_frame: MAX_MARKER_DESPAWNS_PER_FRAME,
            }),
            material: None,
            head_material: None,
            ring_mesh: None,
            ring_materials: HashMap::new(),
            // Overlay layers (transit stops, shops) start off — only the base
            // map tiles and 3D buildings are on at startup; the rest are opt-in
            // from the Layers panel.
            layers: LAYER_DEFS.iter().map(|_| LayerRuntime::default()).collect(),
            minimal: None,
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
    time: Res<Time>,
    camera: Query<(&GridCell, &Transform), With<FreeCamera>>,
    bigspace: Query<Entity, With<BigSpace>>,
    terrain: Res<crate::systems::terrain::TerrainField>,
) {
    let Some((clat, clng, base_zoom)) = stream.geo_anchor() else {
        return;
    };

    // Re-anchor (ADR-025): when the map origin moves, drop every loaded cell and
    // re-establish at the new location, exactly like the buildings. Dropping
    // `jobs` retires the old worker; a fresh `results` orphans in-flight builds.
    // Free retired marker entities at the shared per-frame cap, before any early
    // return: a re-anchor must keep clearing even once the camera climbs past
    // SURFACE_LAYER_MAX_ALT and this streamer freezes.
    dl.cells.drain_despawns(&mut commands);

    // Retear on a re-anchor — and when the ground moves under them: every marker
    // was placed on the field as it stood, which for the first cells of an anchor
    // is the flat plane. `sync` is called unconditionally so it adopts the new
    // generation either way.
    let reanchored = dl.cells.sync(stream.anchor_generation);
    let reground = dl.terrain_revision != terrain.revision;
    if reanchored || reground {
        dl.terrain_revision = terrain.revision;
        // Through the shared despawn queue, not straight out: a cell is a merged
        // mesh plus a ground ring *per POI*, and this used to free every entity of
        // every enabled layer in one command buffer — the documented device-loss
        // burst that four of the six streamers already guarded against.
        let retired: Vec<Entity> = dl
            .layers
            .iter_mut()
            .flat_map(|layer| {
                layer.pois.clear();
                layer
                    .cells
                    .drain()
                    .flat_map(|(_, ents)| ents)
                    .collect::<Vec<_>>()
            })
            .collect();
        dl.cells.retire_all(retired);
        dl.popup = None;
        dl.cells.reset();
        dl.anchor = None;
    }

    // Establish the anchor (from the map origin).
    if dl.anchor.is_none() {
        let z = base_zoom;
        let (ax, ay) = viberfox_core::world::lat_lng_to_tile(clat, clng, z);
        let lat = clat;
        dl.anchor = Some((ax, ay, z, lat));
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

    // Queue missing cells for enabled layers, nearest first. `CellStream::request`
    // both de-dups and holds the backoff, so there is no second retry loop: a cell
    // that failed waits its 15 s / 30 s / 1 m / … out, because Overpass 504s come
    // in bursts and retrying straight away only burns the attempt budget.
    let now_secs = time.elapsed_secs_f64();
    let mut enqueue: Vec<LayerCell> = Vec::new();
    for li in 0..dl.layers.len() {
        if !dl.layers[li].enabled {
            continue;
        }
        let mut to_request: Vec<(i64, i64)> = Vec::new();
        for dy in -LAYER_CELL_RADIUS..=LAYER_CELL_RADIUS {
            for dx in -LAYER_CELL_RADIUS..=LAYER_CELL_RADIUS {
                to_request.push((ccx + dx, ccy + dy));
            }
        }
        to_request.sort_by_key(|c| (c.0 - ccx).abs().max((c.1 - ccy).abs()));
        enqueue.extend(to_request.into_iter().map(|c| (li, c.0, c.1)));
    }
    for job in enqueue {
        dl.cells.request(job, now_secs);
    }

    // Dispatch queued cells (serial, Overpass-throttled) as detached fetch+build
    // tasks (ADR-033 seam) — no worker thread.
    let results = dl.cells.sink();
    while let Some((li, cx, cy)) = dl.cells.next_job(now_secs) {
        let results = results.clone();
        let ground = terrain.snapshot();
        // Fetch on the task pool, parse + mesh on the worker pool (ADR-033). The
        // `serde_json` decode of an Overpass response and the merged marker mesh
        // are both pure arithmetic; on wasm `IoTaskPool` is the main thread, so
        // running them here was running them on the frame loop.
        IoTaskPool::get()
            .spawn(async move {
                match fetch_cell_text(&LAYER_DEFS[li], cx, cy).await {
                    Ok(text) => {
                        crate::platform::workers::submit(Box::new(move || {
                            let built = build_layer_cell(li, &text, cx, cy, ax, ay, z, lat, ground);
                            if let Ok(mut g) = results.lock() {
                                g.push(((li, cx, cy), built));
                            }
                        }));
                    }
                    Err(e) => {
                        if let Ok(mut g) = results.lock() {
                            g.push(((li, cx, cy), Err(e)));
                        }
                    }
                }
            })
            .detach();
    }

    // Apply completed cells.
    let done = dl.cells.take_results();
    for (key, built) in done {
        let (li, cell) = (key.0, (key.1, key.2));
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
                                .with_rotation(Quat::from_rotation_x(-std::f32::consts::FRAC_PI_2))
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
                dl.cells.succeeded(&key);
                tracing::info!(layer = LAYER_DEFS[li].key, cell = ?cell, pois, "data layer cell loaded");
            }
            // No POIs; stays requested so it isn't re-queried.
            Ok(None) => dl.cells.succeeded(&key),
            Err(e) => {
                let attempts = dl.cells.failed(&key, now_secs);
                tracing::warn!(
                    layer = LAYER_DEFS[li].key, cell = ?cell, attempts,
                    exhausted = dl.cells.exhausted(&key), error = %e,
                    "data layer cell failed; will retry"
                );
            }
        }
    }

    for li in 0..dl.layers.len() {
        // Retire cells out of range (one cell of hysteresis); re-entry re-requests
        // them, served from the disk cache. Through the shared queue, under the
        // per-frame cap, like every other streamer.
        let stale: Vec<(i64, i64)> = dl.layers[li]
            .cells
            .keys()
            .filter(|(cx, cy)| (cx - ccx).abs().max((cy - ccy).abs()) > LAYER_CELL_RADIUS + 1)
            .copied()
            .collect();
        for c in stale {
            let entities = dl.layers[li].cells.remove(&c).unwrap_or_default();
            dl.cells.retire_all(entities);
            dl.layers[li].pois.remove(&c);
            dl.cells.forget(&(li, c.0, c.1));
        }

        // Apply panel toggles by flipping visibility (cheap on/off).
        let rt = &mut dl.layers[li];
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
                        title: poi.name.clone().unwrap_or_else(|| {
                            LAYER_DEFS[li].categories[poi.category].0.to_string()
                        }),
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
        popup.subtitle = match dl.layers[li]
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
                .fill(theme::LABEL_CHIP)
                .corner_radius(4.0)
                .inner_margin(egui::Margin::symmetric(10, 8))
                .show(ui, |ui| {
                    ui.set_max_width(240.0);
                    ui.horizontal(|ui| {
                        ui.label(
                            egui::RichText::new(&popup.title)
                                .strong()
                                .size(14.0)
                                .color(theme::CREAM),
                        );
                        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                            if theme::close_button(ui).clicked() {
                                close = true;
                            }
                        });
                    });
                    ui.label(
                        egui::RichText::new(&popup.subtitle)
                            .size(12.0)
                            .color(theme::TEXT_DIM),
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
    let result = std::process::Command::new("cmd")
        .args(["/C", "start", "", &url])
        .spawn();
    // Web: opening a real link in a new tab is the browser's job (ADR-030). Wired
    // to `web-sys` `window.open` later; a compiling no-op keeps the click harmless.
    #[cfg(target_arch = "wasm32")]
    let result: std::io::Result<()> = {
        tracing::info!(%url, "in-world link open not yet wired on the web build");
        Ok(())
    };
    // Everything else — Android today, a BSD tomorrow. This arm exists because the
    // chain above is exhaustive by coincidence rather than by construction: with
    // only the three desktop arms and wasm, a target that is none of them leaves
    // `result` undefined and the *whole crate* fails to compile on a line that has
    // nothing to do with the port being attempted (which is exactly how Android
    // found it). A catch-all makes an unhandled platform a dead button instead of
    // a build break.
    //
    // Android's real implementation is an `Intent.ACTION_VIEW` through JNI, which
    // needs the activity handle and a JVM-attached thread — the same shape as the
    // clipboard seam, and deferred with it (`docs/notes/android-build.md`).
    #[cfg(not(any(
        target_os = "linux",
        target_os = "macos",
        target_os = "windows",
        target_arch = "wasm32"
    )))]
    let result: std::io::Result<()> = {
        tracing::info!(%url, "opening links is not wired on this platform");
        Ok(())
    };
    if let Err(e) = result {
        tracing::warn!(error = %e, "could not open browser");
    }
}

/// "Minimal mode": while **driver mode** is on, strip the map to just the vector
/// tiles + the route so the line being driven reads clearly. Off go the 3D
/// buildings, trees, live vehicles, every streamed overlay (transit stops, shops),
/// and the weather (clouds, fog, rain — and live weather is frozen so it can't
/// re-apply).
///
/// What stays is the basemap *and its legibility*: the vector tiles
/// (`map_enabled`), the road textures and the water shading. Turning those off too
/// — the first cut did — leaves a driver navigating a plain khaki field with the
/// route line floating on it, which strips out exactly the thing being driven on.
/// Clutter goes; the map does not.
///
/// **Planning a route no longer triggers this.** It used to key off the Route panel
/// being open, which meant looking up a route tore down the view you were looking
/// at — the planner is a form, and a form has no business turning the city off.
/// Driving is the mode that wants a bare map, so driving is what gets one
/// (`systems::driver`).
///
/// The user's state is snapshotted on entry and restored verbatim on exit, so this
/// never destroys their settings. It only *sets* at the two edges (cached in
/// `minimal`), so a user can still re-enable something mid-drive from the panel; the
/// snapshot, not the mid-drive edit, is what's restored. Writes nothing when idle.
pub fn apply_minimal_mode(
    driver: Res<crate::systems::driver::DriverState>,
    mut dl: ResMut<DataLayers>,
    mut dev: ResMut<crate::resources::DevPanelState>,
    mut weather: ResMut<crate::systems::weather::WeatherState>,
) {
    match (driver.active, dl.minimal.is_some()) {
        // Entering: snapshot everything minimal mode dims, then dim it.
        (true, false) => {
            dl.minimal = Some(MinimalSave {
                buildings: dl.buildings_enabled,
                vehicles: dl.vehicles_enabled,
                trees: dl.trees_enabled,
                furniture: dl.furniture_enabled,
                layers: dl.layers.iter().map(|l| l.enabled).collect(),
                fog_density: dev.fog_density,
                cloud_cover: dev.cloud_cover,
                precipitation: dev.precipitation,
                weather_live: weather.live,
            });
            dl.buildings_enabled = false;
            dl.vehicles_enabled = false;
            dl.trees_enabled = false;
            dl.furniture_enabled = false;
            for l in &mut dl.layers {
                l.enabled = false;
            }
            // Clear the sky and freeze live weather so a poll can't re-cloud it.
            weather.live = false;
            dev.fog_density = 0.0;
            dev.cloud_cover = 0.0;
            dev.precipitation = 0.0;
        }
        // Leaving: restore the snapshot exactly.
        (false, true) => {
            if let Some(saved) = dl.minimal.take() {
                dl.buildings_enabled = saved.buildings;
                dl.vehicles_enabled = saved.vehicles;
                dl.trees_enabled = saved.trees;
                dl.furniture_enabled = saved.furniture;
                for (l, was) in dl.layers.iter_mut().zip(saved.layers) {
                    l.enabled = was;
                }
                weather.live = saved.weather_live;
                dev.fog_density = saved.fog_density;
                dev.cloud_cover = saved.cloud_cover;
                dev.precipitation = saved.precipitation;
            }
        }
        _ => {}
    }
}

/// Apply the Buildings / Map feature toggles by writing entity visibility.
/// Runs every frame with compare-before-write, so it is cheap when nothing
/// changed and newly streamed-in entities pick up the current toggle state.
pub fn apply_feature_toggles(
    dl: Res<DataLayers>,
    globe: Res<crate::systems::globe::GlobeState>,
    camera: Query<(&GridCell, &Transform), With<FreeCamera>>,
    mut colliders: ResMut<crate::systems::collision::BuildingColliders>,
    // The two building sources are separate entity types with their own panel
    // checkboxes, so they get separate queries (the `Without`s prove the two
    // `&mut Visibility` sets disjoint). Unchecking Buildings still hides both —
    // an earlier OSM-only query left the 3DBAG soup standing.
    mut osm_meshes: Query<
        &mut Visibility,
        (
            Or<(
                With<crate::components::OsmBuildingMesh>,
                With<crate::systems::osm_buildings::WindowGlow>,
            )>,
            Without<crate::systems::lod22::Lod22Mesh>,
        ),
    >,
    mut lod22_meshes: Query<
        &mut Visibility,
        (
            With<crate::systems::lod22::Lod22Mesh>,
            Without<crate::components::OsmBuildingMesh>,
            Without<crate::systems::osm_buildings::WindowGlow>,
        ),
    >,
    mut map: Query<
        (
            &mut Visibility,
            Has<crate::components::StreamTileReady>,
            Has<crate::components::MapBackstop>,
        ),
        (
            With<crate::systems::map_stream::StreamTile>,
            Without<crate::components::RegionMesh>,
            Without<crate::components::OsmBuildingMesh>,
            Without<crate::systems::osm_buildings::WindowGlow>,
            Without<crate::systems::lod22::Lod22Mesh>,
        ),
    >,
    mut region_quads: Query<
        (&crate::components::Region, &mut Visibility),
        (
            With<crate::components::RegionMesh>,
            Without<crate::systems::map_stream::StreamTile>,
            Without<crate::components::OsmBuildingMesh>,
            Without<crate::systems::osm_buildings::WindowGlow>,
            Without<crate::systems::lod22::Lod22Mesh>,
        ),
    >,
    map_anchor: Res<crate::resources::MapAnchor>,
) {
    // Mirror the panel's 3DBAG toggle into `osm_buildings`' coverage gate — the
    // coverage predicates are free functions over statics, out of reach of this
    // resource. `lod22::update_lod22` tears its meshes down on the off-edge.
    crate::systems::osm_buildings::set_tdbag_enabled(dl.tdbag_enabled);
    // And both source toggles into the collision index, so a hidden source's
    // footprints stop acting as invisible walls (collision matches what's drawn).
    colliders.set_sources(dl.osm_buildings_enabled, dl.tdbag_enabled);

    // Sole visibility writer for these entities: the panel toggle AND the space
    // tier (from orbit the flat patch floats over the globe, parallax-shifted —
    // ADR-025). Folding both in here avoids racing another writer.
    let in_space = camera
        .single()
        .map(|(cell, tf)| globe.in_space(cell, tf))
        .unwrap_or(false);
    let base = dl.buildings_enabled && !in_space;
    let want = if base && dl.osm_buildings_enabled {
        Visibility::Inherited
    } else {
        Visibility::Hidden
    };
    for mut vis in osm_meshes.iter_mut() {
        if *vis != want {
            *vis = want;
        }
    }
    // 3DBAG-off despawns its meshes (`lod22::update_lod22`); this covers the
    // master Buildings toggle, the space tier, and the frame before teardown.
    let want = if base && dl.tdbag_enabled {
        Visibility::Inherited
    } else {
        Visibility::Hidden
    };
    for mut vis in lod22_meshes.iter_mut() {
        if *vis != want {
            *vis = want;
        }
    }
    let want = if dl.map_enabled && !in_space {
        Visibility::Inherited
    } else {
        Visibility::Hidden
    };
    // A tile with no imagery yet stays hidden so the coarser clipmap level beneath
    // it shows through — blurry map, not grey squares (`crate::components::MapBackstop`
    // has the argument). The coarsest level is exempt: nothing is stacked under it,
    // so hiding it would open a hole to the sky at the horizon. This has to happen
    // here rather than in `map_stream`, because this system is the sole visibility
    // writer for a `StreamTile` and would otherwise un-hide them the next frame.
    for (mut vis, ready, backstop) in map.iter_mut() {
        let want = if ready || backstop {
            want
        } else {
            Visibility::Hidden
        };
        if *vis != want {
            *vis = want;
        }
    }

    // The legacy region quad is a single tile-sized textured slab pinned to the
    // world origin, and unlike everything else it does NOT follow a re-anchor
    // (ADR-025). Re-anchored elsewhere it becomes a lone patch of the *old*
    // city's imagery sitting exactly where the camera lands — the one
    // recognisable thing in an otherwise empty view, which reads as "streaming
    // is broken" rather than "you are somewhere else". Hide it once the map
    // anchor has left its tile; the streamed tiles cover that ground now.
    let anchor_tile = map_anchor.geo.map(|(clat, clng, z)| {
        let (ax, ay) = viberfox_core::world::lat_lng_to_tile(clat, clng, z);
        (ax, ay, z as i64)
    });
    for (region, mut vis) in region_quads.iter_mut() {
        let stale = anchor_tile.is_some_and(|t| t != (region.tile_x, region.tile_y, region.tile_z));
        let want = if stale { Visibility::Hidden } else { want };
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
    // Toggling a layer off only hides its entities (`apply_feature_toggles`), so
    // without this gate a disabled transit layer still costs one `Transform` write
    // per ring — thousands of them — each tripping change detection into
    // `GlobalTransform` propagation and render-world re-extraction, plus a
    // `Modified` event per ring material. Nothing is visible to animate.
    if !dl
        .layers
        .iter()
        .enumerate()
        .any(|(li, l)| l.enabled && LAYER_DEFS[li].rings)
    {
        return;
    }
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

/// The Buildings block of the Layers panel: the master checkbox plus one
/// checkbox per source, in precedence order — 3DBAG first (where it covers
/// and is on, it owns the cell), the OSM fallback beneath. The 3DBAG row only
/// appears once a coverage manifest has loaded (`tdbag_available`): before
/// that, or with no host, there is nothing to turn off.
///
/// Split out of [`render_data_layers_panel`] so a unit test can drive it with
/// synthetic clicks. Returns the 3DBAG checkbox's response for that test.
fn buildings_section(
    ui: &mut egui::Ui,
    dl: &mut DataLayers,
    tdbag_available: bool,
) -> Option<egui::Response> {
    crate::systems::theme::checkbox(
        ui,
        &mut dl.buildings_enabled,
        egui::RichText::new("Buildings").size(14.0),
    );
    ui.indent("buildings", |ui| {
        // Grayed out while the master is off: the sources are gated by it, so
        // an active-looking sub-checkbox would promise something it can't do.
        ui.add_enabled_ui(dl.buildings_enabled, |ui| {
            let tdbag = tdbag_available.then(|| {
                crate::systems::theme::checkbox(
                    ui,
                    &mut dl.tdbag_enabled,
                    egui::RichText::new("3DBAG (NL)").size(13.0),
                )
            });
            crate::systems::theme::checkbox(
                ui,
                &mut dl.osm_buildings_enabled,
                egui::RichText::new("OSM 3D buildings and windows").size(13.0),
            );
            tdbag
        })
        .inner
    })
    .inner
}

/// The Layers side panel (menu bar ▸ Layers): a checkbox per layer with a
/// colour legend, plus the required OSM attribution. Styled like the AI
/// assistant panel (right side panel, resizable).
pub fn render_data_layers_panel(
    mut egui: ResMut<EguiManager>,
    mut dl: ResMut<DataLayers>,
    mut right_panel: ResMut<crate::resources::RightPanel>,
) {
    let open_before = right_panel.is(crate::resources::RightPanel::Layers);
    let mut open = open_before;
    let mut close = false;
    let compact = egui.compact();
    let viewport_w = egui.viewport_pts().x;
    crate::systems::egui_manager::responsive_right_panel(
        "layers_panel",
        compact,
        viewport_w,
        260.0,
    )
    .show_collapsible(egui.ui_mut(), &mut open, |ui| {
        let panel = crate::resources::RightPanel::Layers;
        close = crate::systems::theme::panel_header(ui, panel.icons(), panel.label());
        for (i, def) in LAYER_DEFS.iter().enumerate() {
            ui.add_space(4.0);
            crate::systems::theme::checkbox(
                ui,
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
                    crate::systems::theme::checkbox(
                        ui,
                        &mut dl.vehicles_enabled,
                        egui::RichText::new("Live vehicles").size(13.0),
                    );
                }
            });
            ui.add_space(4.0);
            ui.separator();
        }
        ui.add_space(4.0);
        let tdbag_resp = buildings_section(
            ui,
            &mut dl,
            crate::systems::osm_buildings::tdbag_attribution().is_some(),
        );
        // One line per click, so a live session can tell "the click never
        // reached the widget" from "it toggled and something reverted it"
        // without a debugger (#12 reported the checkbox stuck).
        if tdbag_resp.is_some_and(|r| r.clicked()) {
            tracing::info!(tdbag = dl.tdbag_enabled, "3DBAG checkbox clicked");
        }
        ui.add_space(4.0);
        ui.separator();
        ui.add_space(4.0);
        crate::systems::theme::checkbox(
            ui,
            &mut dl.map_enabled,
            egui::RichText::new("Map").size(14.0),
        );
        ui.indent("map", |ui| {
            ui.label(egui::RichText::new("OSM ground tiles").size(13.0));
            ui.add_space(2.0);
            crate::systems::theme::checkbox(
                ui,
                &mut dl.trees_enabled,
                egui::RichText::new("Trees").size(13.0),
            );
            crate::systems::theme::checkbox(
                ui,
                &mut dl.furniture_enabled,
                egui::RichText::new("Street furniture").size(13.0),
            );
            crate::systems::theme::checkbox(
                ui,
                &mut dl.roads_textured,
                egui::RichText::new("Road textures").size(13.0),
            );
            crate::systems::theme::checkbox(
                ui,
                &mut dl.water_shading,
                egui::RichText::new("Water shading").size(13.0),
            );
            crate::systems::theme::checkbox(
                ui,
                &mut dl.street_labels,
                egui::RichText::new("Street labels").size(13.0),
            );
        });
        ui.add_space(4.0);
        ui.separator();
        ui.add_space(4.0);
        crate::systems::theme::checkbox(
            ui,
            &mut dl.height_enabled,
            egui::RichText::new("Height").size(14.0),
        );
        ui.indent("height", |ui| {
            // Say where it works, in the panel rather than in a doc comment: a
            // toggle that silently does nothing over Berlin is a bug report.
            ui.label(egui::RichText::new("AHN elevation — Netherlands only").size(13.0));
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
    if close || (open_before && !open) {
        *right_panel = crate::resources::RightPanel::None;
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

/// Parse one layer cell's Overpass body and build its merged marker mesh, anchored
/// to the cell centre's BigSpace grid cell for f32 precision. Returns `None` for
/// cells without matching POIs.
///
/// Pure CPU — no I/O — which is what lets it run on [`crate::platform::workers`];
/// [`fetch_cell_text`] is the awaited half.
fn build_layer_cell(
    li: usize,
    text: &str,
    cx: i64,
    cy: i64,
    ax: i64,
    ay: i64,
    z: u32,
    lat: f64,
    ground: crate::systems::terrain::TerrainSnapshot,
) -> Result<Option<CellBuild>, String> {
    let def = &LAYER_DEFS[li];
    let pois = parse_pois(def, text)?;
    if pois.is_empty() {
        return Ok(None);
    }
    let (north, west) = tile_to_lat_lng(cx, cy, LAYER_ZOOM);
    let (south, east) = tile_to_lat_lng(cx + 1, cy + 1, LAYER_ZOOM);
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
        // A pole is street furniture: it stands on the ground, and the ground is
        // measured now (`systems::terrain`). One height for the whole marker.
        let base_y = ground.at(w.x, w.y);
        buf.set_base(base_y);
        heads.set_base(base_y);
        push_marker(&mut buf, &mut heads, local, def.shape, style);
        // Anchor (click target + popup + board mount): just clear of the pole's
        // sign plate, or the gem-head centre for a pin.
        let anchor_y = match def.shape {
            MarkerShape::Pole => style.pole_height + 0.5,
            MarkerShape::Pin => style.pole_height,
        };
        instances.push(PoiInstance {
            cell: grid_cell,
            anchor: Vec3::new(local.x, anchor_y + base_y, local.y),
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

/// Cache key for one layer cell.
fn cell_cache_key(key: &str, cx: i64, cy: i64) -> String {
    format!("layers/{}/{}/{}/{}.json", key, LAYER_ZOOM, cx, cy)
}

/// One cell's Overpass response **as text**, from the storage cache or a single
/// query. Runs on an `IoTaskPool` task; dispatch spacing (the old `thread::sleep`
/// throttle) lives in the caller's [`Dispatcher`].
///
/// Deliberately returns the raw body rather than parsed POIs: the `serde_json`
/// parse is CPU work and belongs on `platform::workers` with the mesh build, not
/// on the thread that awaited the fetch (ADR-033 — on wasm that thread is the one
/// drawing the frame, and `serde_json` was ~46% of its non-idle time).
async fn fetch_cell_text(def: &LayerDef, cx: i64, cy: i64) -> Result<String, String> {
    let key = cell_cache_key(def.key, cx, cy);
    if let Some(cached) = storage::get_string(&key).await {
        return Ok(cached);
    }

    let (north, west) = tile_to_lat_lng(cx, cy, LAYER_ZOOM);
    let (south, east) = tile_to_lat_lng(cx + 1, cy + 1, LAYER_ZOOM);
    let bbox = format!("{south},{west},{north},{east}");
    let fetched = overpass_query(&def.query.replace("{bbox}", &bbox)).await?;
    storage::put(&key, fetched.as_bytes()).await;
    Ok(fetched)
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

    buf.quad(
        v(x0, 0.0, z1),
        v(x1, 0.0, z1),
        v(x1, h, z1),
        v(x0, h, z1),
        POLE_COLOR,
    );
    buf.quad(
        v(x0, 0.0, z0),
        v(x1, 0.0, z0),
        v(x1, h, z0),
        v(x0, h, z0),
        POLE_COLOR,
    );
    buf.quad(
        v(x0, 0.0, z0),
        v(x0, 0.0, z1),
        v(x0, h, z1),
        v(x0, h, z0),
        POLE_COLOR,
    );
    buf.quad(
        v(x1, 0.0, z0),
        v(x1, 0.0, z1),
        v(x1, h, z1),
        v(x1, h, z0),
        POLE_COLOR,
    );
    buf.quad(
        v(x0, h, z0),
        v(x1, h, z0),
        v(x1, h, z1),
        v(x0, h, z1),
        POLE_COLOR,
    );
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
    base.quad(
        v(x0, y0, z1),
        v(x1, y0, z1),
        v(x1, y1, z1),
        v(x0, y1, z1),
        POLE_COLOR,
    );
    base.quad(
        v(x0, y0, z0),
        v(x1, y0, z0),
        v(x1, y1, z0),
        v(x0, y1, z0),
        POLE_COLOR,
    );
    base.quad(
        v(x0, y0, z0),
        v(x0, y0, z1),
        v(x0, y1, z1),
        v(x0, y1, z0),
        POLE_COLOR,
    );
    base.quad(
        v(x1, y0, z0),
        v(x1, y0, z1),
        v(x1, y1, z1),
        v(x1, y1, z0),
        POLE_COLOR,
    );

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

#[cfg(test)]
mod tests {
    use super::*;

    /// Panel toggles must survive a save/load cycle, including through JSON —
    /// and overlay layers must round-trip by key, not index.
    #[test]
    fn layer_prefs_round_trip() {
        let mut dl = DataLayers::default();
        dl.tdbag_enabled = false;
        dl.trees_enabled = false;
        dl.vehicles_enabled = true;
        dl.layers[0].enabled = true;

        let json = serde_json::to_string(&dl.prefs()).unwrap();
        let prefs: crate::systems::user_store::LayerPrefs = serde_json::from_str(&json).unwrap();

        let mut restored = DataLayers::default();
        restored.apply_prefs(&prefs);
        assert!(!restored.tdbag_enabled);
        assert!(!restored.trees_enabled);
        assert!(restored.vehicles_enabled);
        assert!(restored.layers[0].enabled);
        assert!(
            restored.buildings_enabled,
            "untouched toggles keep defaults"
        );
        assert_eq!(restored.prefs(), prefs);
    }

    /// The persisted sky is the user's, not driver mode's. Quitting mid-drive
    /// must not save the stripped sky (manual, clear, dry) as a preference and
    /// hand it back on the next launch.
    #[test]
    fn sky_prefs_see_past_driver_mode() {
        use crate::resources::{DevPanelState, SunMode};
        use crate::systems::weather::WeatherState;

        let dl = DataLayers::default();
        let dev = DevPanelState {
            fog_density: 0.4,
            cloud_cover: 0.7,
            ..default()
        };
        let weather = WeatherState::with_live(true);

        // Not driving: what's on screen is what the user set.
        let live = dl.sky_prefs(&dev, &weather);
        assert!(live.weather_live);
        assert_eq!(live.cloud_cover, 0.7);
        assert_eq!(live.sun_mode, SunMode::Auto);
        assert_eq!(live.time_of_day, None, "only Manual owns the clock");

        // Driving: the entry snapshot is, even though the live state is bare.
        let mut driving = dl;
        driving.minimal = Some(MinimalSave {
            buildings: true,
            vehicles: false,
            trees: true,
            furniture: true,
            layers: vec![false; LAYER_DEFS.len()],
            fog_density: 0.4,
            cloud_cover: 0.7,
            precipitation: 0.2,
            weather_live: true,
        });
        let bare = DevPanelState {
            sun_mode: SunMode::Manual,
            time_of_day: 6.5,
            fog_density: 0.0,
            cloud_cover: 0.0,
            precipitation: 0.0,
            ..default()
        };
        let saved = driving.sky_prefs(&bare, &WeatherState::default());
        assert!(
            saved.weather_live,
            "live weather was only frozen for the drive"
        );
        assert_eq!(saved.cloud_cover, 0.7);
        assert_eq!(saved.precipitation, 0.2);
        assert_eq!(saved.time_of_day, Some(6.5), "Manual saves its hour");
    }

    /// Does a `request_discard` on the click frame eat a checkbox toggle
    /// (double-fire across the repeated pass)?
    #[test]
    fn discard_pass_does_not_eat_the_click() {
        let ctx = egui::Context::default();
        crate::systems::theme::install(&ctx);
        let mut dl = DataLayers::default();
        let screen = egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(1280.0, 720.0));

        let mut target: Option<egui::Pos2> = None;
        for frame in 0..5 {
            let mut raw = egui::RawInput::default();
            raw.screen_rect = Some(screen);
            if let Some(pos) = target {
                raw.events = match frame {
                    1 => vec![egui::Event::PointerMoved(pos)],
                    2 => vec![egui::Event::PointerButton {
                        pos,
                        button: egui::PointerButton::Primary,
                        pressed: true,
                        modifiers: egui::Modifiers::NONE,
                    }],
                    3 => vec![egui::Event::PointerButton {
                        pos,
                        button: egui::PointerButton::Primary,
                        pressed: false,
                        modifiers: egui::Modifiers::NONE,
                    }],
                    _ => vec![],
                };
            }
            let mut open = true;
            let rect = std::cell::Cell::new(None);
            let dl_ref = &mut dl;
            ctx.run_ui(raw, |ui| {
                // Force a second pass every frame, like an unstable Area does.
                ui.ctx().request_discard("test");
                crate::systems::egui_manager::responsive_right_panel(
                    "layers_panel",
                    false,
                    1280.0,
                    260.0,
                )
                .show_collapsible(ui, &mut open, |ui| {
                    if let Some(resp) = buildings_section(ui, dl_ref, true) {
                        rect.set(Some(resp.rect));
                    }
                });
            });
            if target.is_none() {
                target = rect.get().map(|r| r.center());
            }
        }
        assert!(
            !dl.tdbag_enabled,
            "a discarded pass ate the checkbox click (double-toggle)"
        );
    }

    /// A click on the 3DBAG checkbox must toggle it — and only it. Drives the
    /// real widget tree (same panel container as the live app) with synthetic
    /// pointer events. egui resolves hits against the *previous* pass's widget
    /// rects, so the press goes in one frame after layout and the release one
    /// frame after that.
    #[test]
    fn tdbag_checkbox_toggles_on_click() {
        let ctx = egui::Context::default();
        crate::systems::theme::install(&ctx);
        let mut dl = DataLayers::default();
        let screen = egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(1280.0, 720.0));

        let mut target: Option<egui::Pos2> = None;
        for frame in 0..5 {
            let mut raw = egui::RawInput::default();
            raw.screen_rect = Some(screen);
            if let Some(pos) = target {
                raw.events = match frame {
                    1 => vec![egui::Event::PointerMoved(pos)],
                    2 => vec![egui::Event::PointerButton {
                        pos,
                        button: egui::PointerButton::Primary,
                        pressed: true,
                        modifiers: egui::Modifiers::NONE,
                    }],
                    3 => vec![egui::Event::PointerButton {
                        pos,
                        button: egui::PointerButton::Primary,
                        pressed: false,
                        modifiers: egui::Modifiers::NONE,
                    }],
                    _ => vec![],
                };
            }
            let mut open = true;
            let rect = std::cell::Cell::new(None);
            let dl_ref = &mut dl;
            ctx.run_ui(raw, |ui| {
                crate::systems::egui_manager::responsive_right_panel(
                    "layers_panel",
                    false,
                    1280.0,
                    260.0,
                )
                .show_collapsible(ui, &mut open, |ui| {
                    if let Some(resp) = buildings_section(ui, dl_ref, true) {
                        rect.set(Some(resp.rect));
                    }
                });
            });
            if target.is_none() {
                target = rect.get().map(|r| r.center());
            }
        }
        assert!(
            !dl.tdbag_enabled,
            "click on the 3DBAG checkbox did not toggle it"
        );
        assert!(
            dl.buildings_enabled && dl.osm_buildings_enabled,
            "the click leaked into a sibling checkbox"
        );
    }
}
