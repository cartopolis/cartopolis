//! The client's systems, grouped by concern.
//!
//! | Group | Holds |
//! |-------|-------|
//! | `cartopolis_geo` | Bevy-free geodata: MVT decode/raster, road classes, tree scatter, the routing graph — its own crate, re-exported here |
//! | [`map`] | Everything streamed and drawn as world geometry — tiles, globe, buildings, terrain, water, vegetation, labels |
//! | [`nav`] | Route planning and following, geocoding, transit |
//! | [`player`] | Avatar, camera, collision, picking, gizmos |
//! | [`gui`] | egui: the manager, the theme, and every panel |
//! | [`net`] | Sim transport and the user store |
//! | [`dev`] | Headless capture, debug overlays, desktop integration |
//!
//! **The flat re-exports below are this module's public surface**, not a
//! transitional shim to be cleaned up later. The folders group the *files* — the
//! thing that was unmanageable at 50 in one directory — while callers keep naming a
//! system by what it is (`systems::water`) rather than by where it is filed. That
//! split is deliberate, because the filing is expected to keep moving: `geo` is
//! slated to leave for a crate of its own, and a group rename must not become a
//! 151-call-site edit across `lib.rs`, which reaches most systems through
//! `use systems::*`.
//!
//! Adding a system means one `pub mod` in the group's `mod.rs` and one name in the
//! matching `pub use` here. Both are alphabetical; keep them that way.
//!
//! The group is named `gui` rather than `ui` deliberately — `systems::ui` is one of
//! the panels, and a group of the same name could not coexist with its re-export.

pub mod dev;
pub mod gui;
pub mod map;
pub mod nav;
pub mod net;
pub mod player;

pub use dev::{
    debug, desktop_integration, diagnostics, frame_pacing, pipeline_health, render_census,
    shot_harness, shot_presets, upload_meter, window_icon,
};
// `geo` is its own crate now (crates/geo). Re-exported here so client code keeps
// writing `systems::vector_tiles` — the same separation of filing from naming the
// group folders use, and the reason the move cost no call-site edits.
pub use gui::{
    ai_assistant, bookmarks, chat, chat_overlay, data_layers, dismiss, dock, egui_manager, hud,
    icons, login_screen, map_osd, onscreen_keyboard, place_card, place_pins, script_editor,
    settings, splash, text_touch, theme, ui,
};
pub use map::{
    atmosphere_patch, globe, globe_ocean, globe_weather, grass, height, lod22, map_geometry,
    map_stream, osm_buildings, render_scale, rendering, road_depth, sky, sky_dome, stream_ledger,
    street_detail, street_furniture, street_labels, tall_structures, terrain, tile_loader,
    tile_source, vegetation, water, weather, world_places,
};
pub use nav::{
    driver, geo_nav, locate, navigation, place_link, route_ahead, transit_live, transit_vehicles,
};
pub use net::{background, network, session, user_store, voice};
pub use player::{
    avatar, collision, free_camera, gizmo, interact, locomotion, picking, prim_geo, sit_pose, touch,
};
// Re-exported only where something reaches it by name: with `audio` off the
// module is still compiled (its synthesis and rate maths are pure and tested),
// but nothing outside it calls in, and an unconditional re-export would be an
// unused import rather than a convenience.
pub use cartopolis_geo::{
    furniture, mip, parking, places, pois, road_texture, routing, scatter, turf, vector_tiles,
};
#[cfg(feature = "audio")]
pub use player::soundscape;
