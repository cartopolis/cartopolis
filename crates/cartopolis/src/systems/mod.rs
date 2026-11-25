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
//! matching `pub use` here. Both are alphabetical; keep them that way. Its
//! registration goes in the group's `plugin.rs` (`NetPlugin`, `MapPlugin`, …),
//! inside the `crate::phase::ClientPhase` it belongs to — that module's note says
//! what each phase is for.
//!
//! The group is named `gui` rather than `ui` deliberately — `systems::ui` is one of
//! the panels, and a group of the same name could not coexist with its re-export.

pub mod dev;
pub mod gui;
pub mod map;
pub mod nav;
pub mod net;
pub mod player;

#[cfg(not(any(target_arch = "wasm32", target_os = "android")))]
pub use dev::light_compare;
#[cfg(all(feature = "ui-probe", not(target_arch = "wasm32")))]
pub use dev::ui_probe;
#[cfg(not(target_arch = "wasm32"))]
pub use dev::{aerial_reference, midas_link, release_approval, release_review, shot_replay};
pub use dev::{
    anim_clock, camera_pacing, desktop_integration, dev_panel, diagnostics, frame_pacing,
    frame_split, perf, pipeline_health, render_census, shot_harness, shot_presets, upload_meter,
    window_icon,
};
// Desktop only — a phone has no editor pointed at it and a browser tab cannot
// listen on a socket (`build.rs` sets the cfg, `Cargo.toml` gates the
// dependency on the same predicate).
#[cfg(mcp)]
pub use dev::{mcp, mcp_access};
// `geo` is its own crate now (crates/geo). Re-exported here so client code keeps
// writing `systems::vector_tiles` — the same separation of filing from naming the
// group folders use, and the reason the move cost no call-site edits.
pub use cartopolis_geo::{
    furniture, ground_stack, ground_texture, mip, moorings, parking, paving, places, pois,
    road_texture, routing, scatter, texcomp, turf, vector_tiles,
};
pub use gui::{
    action_ring, app_update, avatar_designer, bookmarks, building_card, chat, cosmos, data_layers,
    design_room, discs, dismiss, dock, egui_manager, facade_check, faces, floor_picker, hud, icons,
    js_highlight, layer_tiles, layers_panel, map_osd, model_library, motion, onscreen_keyboard,
    place_card, place_names, place_pins, plans, scenario, script_editor, settings, splash,
    text_touch, theme, ui, ui_theme, welcome, zoom_rail,
};
// Settings drawn by the component library, beside egui (`ui-probe`).
#[cfg(all(feature = "ui-lib", feature = "comms", not(target_arch = "wasm32")))]
pub use gui::lib_camera;
#[cfg(feature = "ui-probe")]
pub use gui::lib_keyboard;
#[cfg(all(feature = "ui-lib", mcp, not(target_os = "android")))]
pub use gui::lib_mcp_overlay;
#[cfg(all(feature = "ui-lib", feature = "comms"))]
pub use gui::lib_picture_viewer;
#[cfg(feature = "ui-lib")]
pub use gui::{
    lib_action_ring, lib_app_update, lib_avatar, lib_chrome, lib_cosmos, lib_design_room,
    lib_driver, lib_face_viewfinder, lib_interact, lib_locate, lib_main_menu, lib_map_osd,
    lib_models, lib_pins, lib_place_names, lib_plans, lib_poi_card, lib_satellite_menu,
    lib_scenario, lib_script, lib_splash, lib_walk_guide,
};
#[cfg(all(feature = "ui-lib", feature = "comms"))]
pub use gui::{
    lib_admin, lib_calendar, lib_call, lib_chat_conversation, lib_chat_overlay, lib_chats,
    lib_communities, lib_contacts, lib_dialogs, lib_filter_chip, lib_login, lib_person_card,
    lib_post_composer, lib_reminder, lib_share, lib_welcome,
};
#[cfg(feature = "ui-lib")]
pub use gui::{
    lib_building_card, lib_host, lib_layers, lib_place_card, lib_places, lib_route, lib_search,
    lib_settings,
};
#[cfg(feature = "ui-lib")]
pub use gui::{lib_dev_panel, lib_marquee, lib_object_editor};
#[cfg(all(feature = "ui-lib", not(target_arch = "wasm32")))]
pub use gui::{lib_midas_link, lib_release_approval};
// What talks rather than shows the world: the app's, not the web build's
// (`comms` in Cargo.toml).
#[cfg(feature = "comms")]
pub use gui::{
    access_tokens, admin, bridges, calendar, call_screen, camera_screen, chat_overlay, chat_people,
    chats, communities, community_posts, contacts, external_signin, login_screen,
};
pub use map::prim_hand;
pub use map::prim_sound;
pub use map::prim_tone;
#[cfg(not(target_arch = "wasm32"))]
pub use map::prim_video;
pub use map::{
    aerial, atmosphere_patch, building_designs, building_facts, building_fade, building_select,
    building_windows, cache_reset, cell_ring, coverage, crops, crossings, cutaway, dikes,
    drawn_ground, drive_network, facade_colours, fog, globe, globe_ocean, globe_weather, graffiti,
    grass, ground_detail, ground_probe, height, interiors, kerb_shadows, label_atlas, layer_state,
    lens_areas, light_shafts, load_control, load_timing, lod22, lod22_facade, lod3, map_geometry,
    map_stream, mesh_format, model_assets, municipal, nav_marks, osm_buildings, particles,
    placeholder_models, render_features, render_scale, rendering, road_depth, roof_colours,
    satellites, scaffolding, sewers, sky, sky_bake, sky_dome, stream_ledger, stream_watch,
    street_detail, street_furniture, street_labels, sun_cascades, sun_rays, surveyed,
    tall_structures, terrain, terrain_tiles, tile_loader, tile_source, tree_models, tunnels,
    underground, vegetation, water, water_levels, weather, wet_surfaces, world_places,
};
#[cfg(feature = "comms")]
pub use nav::meetup;
pub use nav::{
    crowd, crowd_react, crows, driver, geo_nav, give_way, gnss_receiver, locate, navigation,
    place_link, route_ahead, traffic, traffic_hours, traffic_physics, transit_live,
    transit_vehicles, walk_guide,
};
pub use nav::{high_precision, precise_tracker};
#[cfg(target_arch = "wasm32")]
pub use net::framed_session;
#[cfg(all(target_os = "android", feature = "comms"))]
pub use net::push;
pub use net::{background, lifecycle, network, session, user_store, voice};
#[cfg(feature = "comms")]
pub use net::{call_tones, calls, chat_pictures, private_chat};
#[cfg(not(target_arch = "wasm32"))]
pub use player::avatar_video;
#[cfg(not(target_arch = "wasm32"))]
pub use player::face_tracking;
pub use player::{
    avatar, avatar_face, camera_attach, climb, collision, feet, free_camera, gizmo, greet,
    held_phone, human_avatar, interact, local_avatar, locomotion, picking, prim_geo, splashes,
    touch,
};
// Re-exported only where something reaches it by name: with `audio` off the
// module is still compiled (its synthesis and rate maths are pure and tested),
// but nothing outside it calls in, and an unconditional re-export would be an
// unused import rather than a convenience.
#[cfg(feature = "audio")]
pub use player::{ambience, soundscape};
