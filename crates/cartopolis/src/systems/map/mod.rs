//! `systems::map` — one file per concern; see systems/mod.rs for the map.

pub mod aerial;
pub mod atmosphere_patch;
pub mod building_facts;
pub mod building_fade;
pub mod building_select;
pub mod building_windows;
// Throwing the downloaded map away, and the three layers of cache that have to
// be emptied in the right order for that to be visible.
pub mod cache_reset;
pub mod cell_bundle;
pub mod coverage;
// The fetching half of the level crossings (`cartopolis_geo::crossings` is the
// pure one). Named for the layer rather than for ProRail, the way `surveyed`,
// `crops` and `nav_marks` are.
pub mod crossings;
// The client half of the crop parcels (`cartopolis_geo::crops` is the pure one),
// named for the layer rather than the register the way `surveyed` is.
pub mod crops;
pub mod dikes;
pub mod globe;
pub mod globe_ocean;
pub mod globe_weather;
pub mod grass;
pub mod ground_detail;
pub mod height;
pub mod interiors;
// The fog volume the sun's light shafts are marched through; the camera and the
// sun carry the other two halves (`free_camera::setup_camera`, `sky`).
pub mod light_shafts;
pub mod load_control;
pub mod lod22;
pub mod lod3;
pub mod map_geometry;
pub mod map_stream;
pub mod model_assets;
// The fetching half of the navigation marks (`cartopolis_geo::nav_marks` is the
// pure one). Named for the layer rather than for Rijkswaterstaat, the way
// `surveyed` and `crops` are.
pub mod nav_marks;
pub mod osm_buildings;
pub mod particles;
pub mod placeholder_models;
pub mod render_features;
pub mod render_scale;
pub mod rendering;
pub mod road_depth;
pub mod sky;
pub mod sky_dome;
pub mod stream_ledger;
pub mod street_detail;
pub mod street_furniture;
pub mod street_labels;
// A city's own registers, where the national ones are silent — named for whose
// survey it is rather than for what it carries, the way `surveyed` is.
pub mod municipal;
pub mod surveyed;
pub mod tall_structures;
pub mod terrain;
pub mod terrain_tiles;
pub mod tile_loader;
pub mod tile_source;
// The bores the map used to throw away, as places rather than as triangles:
// the floor down there and the hole in the ground over the portal.
pub mod tunnels;
pub mod vegetation;
pub mod water;
pub mod water_levels;
pub mod weather;
// The ground's answer to the rain the sky already draws.
pub mod wet_surfaces;
pub mod world_places;
