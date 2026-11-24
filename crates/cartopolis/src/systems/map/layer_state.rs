//! The layer switches and what the layers hold, as data: [`DataLayers`] and
//! the types of its fields.
//!
//! Seventeen map modules read [`DataLayers`] to know whether to draw, and
//! `data_layers`, which drives the POI layers and applies the switches, reaches
//! into most of them; with the type filed there, every one of those readers sat
//! in one import cycle with it (`docs/notes/module-graph.md`). The type lives
//! here, below them all, and its methods stay in `data_layers` (an `impl` may
//! sit anywhere in its crate).
//!
//! The `pub(crate)` fields are that module's machinery, crate-visible only
//! because its code is filed apart from its type: read the switches, not those.

use bevy::prelude::*;
use big_space::prelude::GridCell;
use std::collections::{HashMap, HashSet};

use crate::platform::stream::CellStream;
pub use cartopolis_materials::data_layers::PinHeadMaterial;

/// How many underground layers `underground::LAYERS` lists.
pub const UNDERGROUND_LAYERS: usize = 7;
/// How many satellite kinds `satellites::KINDS` lists.
pub const SATELLITE_KINDS: usize = 5;
/// How many notice kinds `plans::KINDS` lists.
pub const PLAN_KINDS: usize = 6;

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

/// Per-layer streaming state. The requested/failed/backoff bookkeeping is *not*
/// here: it lives in the one shared [`CellStream`] keyed by `(layer, cell)`, so all
/// the layers share the single Overpass-throttled admission slot they have to.
#[derive(Default)]
pub struct LayerRuntime {
    /// Panel checkbox: whether this layer's markers are shown (and streamed).
    pub enabled: bool,
    /// Visibility last applied to the spawned entities.
    pub(crate) shown: bool,
    /// Spawned entities per cell: the merged marker mesh + one ground ring
    /// per POI.
    pub(crate) cells: HashMap<(i64, i64), Vec<Entity>>,
    /// POIs of each loaded cell, in the same lifetime as its mesh entity.
    pub pois: HashMap<(i64, i64), Vec<PoiInstance>>,
    /// Cells whose Overpass detail has already been merged into `pois`.
    ///
    /// The two sources land in either order — the tile is usually cached and the
    /// query usually is not, but a revisit inverts that — so neither arrival can
    /// be the trigger. Both just record what they have, and `merge_details` joins
    /// whatever is complete and not yet joined.
    pub(crate) enriched: HashSet<(i64, i64)>,
}

/// A cell built on the worker: its anchor cell, merged marker mesh, and POIs.
pub struct CellBuild {
    pub(crate) cell: GridCell,
    /// Depth-tested geometry: poles, or pin stems + ground discs.
    pub(crate) mesh: Mesh,
    /// Always-on-top geometry: the floating pin heads (`None` for pole layers).
    pub(crate) heads: Option<Mesh>,
    pub(crate) pois: Vec<PoiInstance>,
}

/// What identifies one streamed cell: which layer, and where. One key space for
/// every layer, because they share one fetch budget.
pub(crate) type LayerCell = (usize, i64, i64);

/// A completed cell build (`None` for cells without matching POIs).
pub(crate) type CellResult = Result<Option<CellBuild>, String>;

/// One cell's Overpass detail: the fields the tile does not carry, to be matched
/// onto markers that are already standing.
pub(crate) type DetailResult = Result<Vec<Poi>, String>;

/// Data-layer streaming + panel state.
#[derive(Resource)]
pub struct DataLayers {
    /// Whether the Data Layers panel window is open (View ▸ Data Layers).
    pub panel_open: bool,
    /// The component library draws the Layers panel (`lib_layers`, on with
    /// `CARTO_UI_LIB=layers`): egui's steps aside.
    pub lib: bool,
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
    /// Feature toggle: the map ground (the streamed OSM tiles).
    pub map_enabled: bool,
    /// Feature toggle (under Life): live moving vehicles (ADR-026) — real Dutch
    /// buses and trains off OVapi's position feed.
    ///
    /// It used to hang under the Public transport *marker* layer and to require
    /// it, which was a politeness gate rather than a data dependency: the feed
    /// carries its own positions and never reads a stop. Under Life it stands on
    /// its own, next to the two generated crowds — "what is moving out there" is
    /// one question whether the thing moving is invented or reported.
    pub vehicles_enabled: bool,
    /// Feature toggle (under Map): the merged tree canopies/trunks
    /// (`systems::vegetation`). Gates their visibility on top of the altitude LOD.
    pub trees_enabled: bool,
    /// Feature toggle (under Map): real tree models up close instead of the
    /// merged crowns (`systems::tree_models`). Starts as the rung says
    /// (`quality::Budget::tree_models`): on for a workstation, off on a phone,
    /// where the person may turn it on.
    pub tree_models_enabled: bool,
    /// Feature toggle (under Map): near-field grass on the green areas
    /// (`systems::grass`) — blades within a few tens of metres of the camera.
    /// Unlike the layers around it this gates no streaming of its own: what it
    /// switches off is the per-cell coverage read and the patch pool.
    pub grass_enabled: bool,
    /// Feature toggle (under Map): the merged street furniture
    /// (`systems::street_furniture`) — benches, bins and shelters read out of the
    /// tile's `pois` layer, **and** the cars and bikes filled into its `sites` car
    /// parks (`systems::parking`). Gates their visibility on top of the altitude
    /// LOD.
    ///
    /// One toggle for both because they are one mesh group, one material and one
    /// collider set: `collision::sync_prop_colliders` gates on this, and a prop
    /// that is hidden but still solid is an invisible wall.
    pub furniture_enabled: bool,
    /// Feature toggle (under Life): the ambient cars (`systems::traffic`) driving
    /// the streets the map already draws, off the same `streets` layer.
    ///
    /// Under Life rather than under Map because Map's other sub-toggles are all
    /// surveyed things that are placed and then stand still, and these move. It
    /// is the first thing somebody who finds the motion distracting will look
    /// for, and the first thing to switch off when judging anything else in a
    /// still — so it should not be five rows down a list of ground textures.
    pub traffic_enabled: bool,
    /// Feature toggle (under Life): the ambient people (`systems::traffic`'s
    /// [`AgentKind::Walker`](crate::components::AgentKind) pool) walking the
    /// pavements beside those same streets.
    ///
    /// Split from the cars, though they share every line of the steering: the
    /// pools are separate and independently sized, one is invented crowds and the
    /// other is invented traffic, and someone judging a street's *emptiness* will
    /// want exactly one of them. Cheap, too — a hidden pool costs a transform
    /// write it never makes.
    pub people_enabled: bool,
    /// Feature toggle (under Life): the ambient cyclists (`systems::traffic`'s
    /// [`AgentKind::Cyclist`](crate::components::AgentKind) pool) on the
    /// cycle tracks and in the streets' cycle lanes, and the fatbikes
    /// (`AgentKind::Fatbike`) on those and the pavements. Its own row on the terms
    /// the people have one: a separate pool, and one somebody judging a street
    /// wants apart from the others.
    pub cyclists_enabled: bool,
    /// Feature toggle (under Life): the crows (`systems::crows`) flying over
    /// and feeding on the lawns. Its own row because it is its own kind of
    /// motion — in the sky, not on the street — and the first thing someone
    /// judging a skyline still would switch off.
    pub birds_enabled: bool,
    /// Your contacts on the map, wherever their avatars are, with or without a
    /// chat open (`contacts::drive_contacts` asks, `place_pins` draws). Only
    /// contacts who show they are online are ever answered for.
    pub friends_enabled: bool,
    /// Communities' public posts on the map (`community_posts`), every
    /// community this account is in.
    pub community_posts_enabled: bool,
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
    /// Feature toggle (under Map): cities, districts and stations named from a
    /// distance (`systems::place_names`). Gates the tiles' fetching too.
    pub place_names: bool,
    /// From above (under Map): the globe's cloud shell, off / light / full
    /// (`sky::update_globe_clouds`).
    pub globe_clouds: crate::resources::GlobeCloudMode,
    /// From above: the globe's night side, from the real sun (`globe_ocean.wesl`).
    pub night_side: bool,
    /// From above: city lights on the night side.
    pub city_lights: bool,
    /// From above: the hex grid over the land (`hex_grid.wesl`), off by default.
    pub hex_grid: bool,
    /// The lens open over the map (`systems::scenario`), chosen under its own
    /// disc rather than in this panel,
    /// as `<file>/<layer>`. Not saved: a layer left open must not greet the
    /// next start.
    /// `CARTO_SCENARIO=cbs-2024/share_65_plus` opens one for a capture.
    pub scenario: Option<String>,
    /// Feature toggle (under Map): the flood defences, named where they stand
    /// (`systems::dikes`). Gates both the streaming and the labelling — off, the
    /// cells are never asked for.
    pub flood_defences: bool,
    /// Feature toggle (under Map): the sewers under the street, and the ground
    /// opened near the camera so they can be seen (`systems::sewers`). Off by
    /// default because it changes how the ground itself looks, not only what is
    /// drawn on it — the same reason the aerial basemap is opt-in.
    pub sewers: bool,
    /// The other underground layers, in `underground::LAYERS` order — soil,
    /// boreholes, groundwater, heat storage, pipelines, the gas field, soil
    /// contamination. Off by default for the reason `sewers` is.
    pub underground: [bool; UNDERGROUND_LAYERS],
    /// Feature toggle (under Map): the Overpass street-detail layer
    /// (`systems::street_detail`) — surveyed trees, street lamps, traffic
    /// signals, zebra paint, barriers, piers. Off tears the cells down, the
    /// same contract the Buildings master toggle keeps.
    pub street_detail_enabled: bool,
    /// Feature toggle (under Map): the **aerial basemap** (`systems::aerial`) —
    /// PDOK's Luchtfoto RGB under the clipmap in place of the drawn one. Off by
    /// default because it *replaces* the drawn ground rather than adding to it,
    /// which is a change to how the map looks and not only to what it knows.
    ///
    /// It used to say "for the same reason the height layer is", and that has been
    /// wrong since the measured ground stopped being opt-in — `terrain` has no
    /// toggle at all now, only `HeightLayer::covers`. The line survived the change
    /// that made it false and then answered a direct question about whether AHN is
    /// on by default with the opposite of the truth (2026-09-20). Reaching a third
    /// party is not by itself a reason to be off; `Default` below is the one that
    /// counts and it already said so.
    ///
    /// Read by `map_stream` alone. `map_geometry`, `globe`, `navigation` and the
    /// router keep the vector template, so nothing about the geometry, the
    /// routing, the street labels or the water shading changes with it.
    pub aerial_enabled: bool,
    /// Satellites in orbit around the globe (`systems::satellites`), shown only
    /// from space. Off on a fresh install: it is a download of its own.
    pub satellites_enabled: bool,
    /// Which kinds of satellite are drawn, indexed by `satellites::SatKind`.
    pub satellite_kinds: [bool; SATELLITE_KINDS],
    /// Places ▸ Plans (`systems::plans`): the municipality's notices and the
    /// council's business on the map. Off on a fresh install: a download of
    /// its own, and Groningen only.
    pub plans_enabled: bool,
    /// Which kinds of notice are drawn, in `plans::KINDS` order.
    pub plans_kinds: [bool; PLAN_KINDS],
    /// Whether the council's squares are drawn.
    pub plans_council: bool,
    /// Whether the maintenance programme's works are drawn.
    pub plans_works: bool,
    /// How far back the layer looks.
    pub plans_period: Period,
    /// The Layers panel's open category. View state, not a preference.
    pub category: LayerCategory,
    /// The tile whose ⋯ options are showing under the grid, by its label.
    pub open_tile: Option<&'static str>,
    /// Info card for a clicked POI (name, address, website links).
    pub popup: Option<PoiPopup>,
    /// Terrain-field revision the live marker cells were built against.
    pub(crate) terrain_revision: u32,
    /// Anchor: tile_x, tile_y, zoom, latitude — derived from the map anchor
    /// (`MapStream`), so pins follow re-anchors like the buildings (ADR-025).
    pub(crate) anchor: Option<(i64, i64, u32, f64)>,
    /// Cell streaming for every layer at once (`platform::stream`): the
    /// requested/failed sets with their exponential backoff, the results sink,
    /// the admission bound and the bounded despawn queue. This is the *marker*
    /// stream — the tile path in the ordinary case.
    pub(crate) cells: CellStream<LayerCell, CellResult>,
    /// The Overpass detail pass ([`DETAIL_CELL_RADIUS`]). Its own stream because
    /// it covers a different ring at a different pace: a rate-limited service
    /// owed a slow ladder, against a byte cache that usually answers at once.
    /// Unused while the markers themselves come from Overpass — there would be
    /// nothing left to add.
    pub(crate) details: CellStream<LayerCell, DetailResult>,
    /// Detail that has arrived, whether or not its markers have. Held rather
    /// than applied-and-dropped so a cell that streams out and back is enriched
    /// again without a second query.
    ///
    /// This is the one of the four per-cell maps that deliberately **outlives**
    /// its cell — and it is the only thing that does. It is not an oversight and
    /// it must not be pruned alongside `pois` / `enriched` / `cells`: it is also
    /// the de-duplication key for the detail stream (`dl.details.request` is only
    /// made for a cell absent from here), and `CellStream::reset` on a re-anchor
    /// wipes that stream's own `requested` set. Prune it with its siblings and a
    /// re-anchor, or a walk out of a cell and back, re-queries a *rate-limited*
    /// Overpass for ground already answered for.
    ///
    /// What it must not do is grow without bound — every z14 cell ever visited ×
    /// every enabled layer, each holding a `Vec<Poi>` of owned names, websites and
    /// addresses. So it is bounded by *distance* instead of by lifetime:
    /// [`DETAIL_CACHE_RADIUS`] cells around the camera survive, the rest are
    /// dropped when the camera changes cell. That keeps the whole of a city in
    /// hand — which is the case the cache exists for — and lets the city you flew
    /// away from go.
    pub(crate) detail_pois: HashMap<LayerCell, Vec<Poi>>,
    /// One unlit material shared by all depth-tested marker geometry (hue is
    /// per-vertex): poles, pin stems, pin ground discs.
    pub(crate) material: Option<Handle<StandardMaterial>>,
    /// Shared always-on-top material for the floating pin heads.
    pub(crate) head_material: Option<Handle<PinHeadMaterial>>,
    /// Shared unit annulus for the ground rings (scaled per frame).
    pub(crate) ring_mesh: Option<Handle<Mesh>>,
    /// One translucent ring material per (layer, category); their shared
    /// alpha is pulsed by [`animate_stop_rings`].
    pub(crate) ring_materials: HashMap<(usize, usize), Handle<StandardMaterial>>,
    pub layers: Vec<LayerRuntime>,
    /// State captured when driver mode started, so "minimal mode" — stripping the
    /// map to just the vector tiles + the route while driving — can restore exactly
    /// what the user had when they stop. `None` when not driving.
    pub(crate) minimal: Option<MinimalSave>,
}

/// Snapshot of everything minimal mode turns off, taken when driving starts, so
/// stopping puts the map back exactly as it was. Weather lives in other
/// resources ([`DevPanelState`], [`WeatherState`]) but is restored together here.
pub(crate) struct MinimalSave {
    pub(crate) buildings: bool,
    pub(crate) vehicles: bool,
    pub(crate) trees: bool,
    pub(crate) grass: bool,
    pub(crate) furniture: bool,
    pub(crate) street_detail: bool,
    pub(crate) traffic: bool,
    pub(crate) people: bool,
    pub(crate) cyclists: bool,
    pub(crate) birds: bool,
    pub(crate) layers: Vec<bool>,
    pub(crate) fog_density: f32,
    pub(crate) cloud_cover: f32,
    pub(crate) precipitation: f32,
    pub(crate) weather_live: bool,
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

/// A point of interest, from either source. The tile fills the first four fields
/// and leaves the rest empty; [`merge_details`] is what supplies them later.
pub(crate) struct Poi {
    pub(crate) lat: f64,
    pub(crate) lng: f64,
    pub(crate) category: usize,
    pub(crate) name: Option<String>,
    pub(crate) live_id: Option<String>,
    pub(crate) osm_id: i64,
    pub(crate) website: Option<String>,
    pub(crate) addr: Option<String>,
}

/// How far back the layer looks.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Period {
    /// Only notices whose response period still runs.
    Open,
    #[default]
    ThreeMonths,
    Year,
}

/// The six places a layer can be: what the panel groups by.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum LayerCategory {
    #[default]
    Map,
    City,
    Places,
    Moving,
    Under,
    Orbit,
}
