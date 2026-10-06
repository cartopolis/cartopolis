//! `systems::map` — one file per concern; see systems/mod.rs for the map.

/// The group's registration: its resources and its systems, each in its
/// `crate::phase::ClientPhase`. Added by `run_client`.
mod plugin;
pub use plugin::MapPlugin;

pub mod aerial;
pub mod atmosphere_patch;
pub mod building_facts;
pub mod building_fade;
pub mod building_select;
pub mod building_windows;
// Throwing the downloaded map away, and the three layers of cache that have to
// be emptied in the right order for that to be visible.
pub mod cache_reset;
// The ring of cells a streamer wants around the camera, nearest first.
pub mod cell_ring;
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
// Sunbeams through cloud gaps and past buildings: a pass over the finished frame.
pub mod building_designs;
pub mod cutaway;
pub mod ground_probe;
pub mod load_control;
pub mod lod22;
pub mod lod22_facade;
pub mod lod3;
pub mod map_geometry;
pub mod map_stream;
pub mod model_assets;
pub mod sun_rays;
// The fetching half of the navigation marks (`cartopolis_geo::nav_marks` is the
// pure one). Named for the layer rather than for Rijkswaterstaat, the way
// `surveyed` and `crops` are.
pub mod nav_marks;
pub mod osm_buildings;
pub mod particles;
pub mod placeholder_models;
pub mod prim_sound;
pub mod roof_colours;
// Scaffolding on the buildings the register says are being worked on.
pub mod scaffolding;
// The clip player is a thread of its own, which the browser build does not have.
#[cfg(not(target_arch = "wasm32"))]
pub mod prim_video;

/// Whether this build plays video on prims (`prim_video`; not the browser's).
#[must_use]
pub const fn prim_video_available() -> bool {
    cfg!(not(target_arch = "wasm32"))
}
pub mod render_features;
pub mod render_scale;
pub mod rendering;
pub mod road_depth;
pub mod sky;
pub mod sky_bake;
pub mod sky_dome;
pub mod stream_ledger;
// How long each load took, for the log and the diagnostics report.
pub mod load_timing;
// Which streamers the settle test waits for, registered beside each one.
pub mod stream_watch;
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
