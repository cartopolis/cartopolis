//! `systems::map` — one file per concern; see systems/mod.rs for the map.

pub mod aerial;
pub mod atmosphere_patch;
pub mod building_facts;
pub mod building_windows;
pub mod coverage;
// The client half of the crop parcels (`cartopolis_geo::crops` is the pure one),
// named for the layer rather than the register the way `surveyed` is.
pub mod crops;
pub mod dikes;
pub mod globe;
pub mod globe_ocean;
pub mod globe_weather;
pub mod grass;
pub mod height;
pub mod interiors;
pub mod lod22;
pub mod lod3;
pub mod map_geometry;
pub mod map_stream;
// The fetching half of the navigation marks (`cartopolis_geo::nav_marks` is the
// pure one). Named for the layer rather than for Rijkswaterstaat, the way
// `surveyed` and `crops` are.
pub mod nav_marks;
pub mod osm_buildings;
pub mod particles;
pub mod render_scale;
pub mod rendering;
pub mod road_depth;
pub mod sky;
pub mod sky_dome;
pub mod stream_ledger;
pub mod street_detail;
pub mod street_furniture;
pub mod street_labels;
pub mod surveyed;
pub mod tall_structures;
pub mod terrain;
pub mod tile_loader;
pub mod tile_source;
pub mod vegetation;
pub mod water;
pub mod weather;
pub mod world_places;
