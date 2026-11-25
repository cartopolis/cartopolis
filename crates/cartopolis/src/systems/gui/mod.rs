//! `systems::gui` — one file per concern; see systems/mod.rs for the map.

/// The group's registration: its resources and its systems, each in its
/// `crate::phase::ClientPhase`. Added by `run_client`.
mod plugin;
pub use plugin::GuiPlugin;

#[cfg(feature = "comms")]
pub mod access_tokens;
pub mod action_ring;
#[cfg(feature = "comms")]
pub mod admin;
pub mod app_update;
pub mod avatar_designer;
pub mod bookmarks;
#[cfg(feature = "comms")]
pub mod bridges;
pub mod building_card;
#[cfg(feature = "comms")]
pub mod calendar;
#[cfg(feature = "comms")]
pub mod call_screen;
#[cfg(feature = "comms")]
pub mod camera_screen;
pub mod chat;
#[cfg(feature = "comms")]
pub mod chat_overlay;
#[cfg(feature = "comms")]
pub mod chat_people;
#[cfg(feature = "comms")]
pub mod chats;
#[cfg(feature = "comms")]
pub mod communities;
#[cfg(feature = "comms")]
pub mod community_posts;
#[cfg(feature = "comms")]
pub mod contacts;
pub mod cosmos;
pub mod data_layers;
pub mod design_room;
pub mod discs;
pub mod dismiss;
pub mod dock;
pub mod egui_manager;
#[cfg(feature = "comms")]
pub mod external_signin;
pub mod facade_check;
pub mod faces;
pub mod floor_picker;
pub mod hud;
pub mod icons;
pub mod js_highlight;
pub mod layer_tiles;
pub mod layers_panel;
#[cfg(feature = "ui-lib")]
pub mod lib_app_update;
#[cfg(feature = "ui-lib")]
pub mod lib_avatar;
#[cfg(feature = "ui-lib")]
pub mod lib_building_card;
#[cfg(feature = "ui-lib")]
pub mod lib_design_room;
#[cfg(feature = "ui-lib")]
pub mod lib_host;
#[cfg(feature = "ui-probe")]
pub mod lib_keyboard;
#[cfg(feature = "ui-lib")]
pub mod lib_layers;
#[cfg(all(feature = "ui-lib", feature = "comms"))]
pub mod lib_login;
#[cfg(feature = "ui-lib")]
pub mod lib_models;
#[cfg(feature = "ui-lib")]
pub mod lib_place_card;
#[cfg(feature = "ui-lib")]
pub mod lib_places;
#[cfg(feature = "ui-lib")]
pub mod lib_plans;
#[cfg(feature = "ui-lib")]
pub mod lib_route;
#[cfg(feature = "ui-lib")]
pub mod lib_scenario;
#[cfg(feature = "ui-lib")]
pub mod lib_script;
#[cfg(feature = "ui-lib")]
pub mod lib_search;
#[cfg(feature = "ui-lib")]
pub mod lib_settings;
// The library's screens that talk: the app's only, as their egui ones (`comms`).
#[cfg(all(feature = "ui-lib", feature = "comms"))]
pub mod lib_admin;
#[cfg(all(feature = "ui-lib", feature = "comms"))]
pub mod lib_calendar;
#[cfg(all(feature = "ui-lib", feature = "comms"))]
pub mod lib_chat_conversation;
#[cfg(all(feature = "ui-lib", feature = "comms"))]
pub mod lib_chats;
#[cfg(all(feature = "ui-lib", feature = "comms"))]
pub mod lib_communities;
#[cfg(all(feature = "ui-lib", feature = "comms"))]
pub mod lib_contacts;
#[cfg(all(feature = "ui-lib", feature = "comms"))]
pub mod lib_filter_chip;
#[cfg(all(feature = "ui-lib", feature = "comms"))]
pub mod lib_person_card;
#[cfg(all(feature = "ui-lib", feature = "comms"))]
pub mod lib_post_composer;
#[cfg(all(feature = "ui-lib", feature = "comms"))]
pub mod lib_reminder;
#[cfg(all(feature = "ui-lib", feature = "comms"))]
pub mod lib_welcome;
#[cfg(feature = "comms")]
pub mod login_screen;
pub mod map_osd;
pub mod model_library;
pub mod motion;
pub mod onscreen_keyboard;
pub mod place_card;
pub mod place_names;
pub mod place_pins;
pub mod plans;
pub mod scenario;
pub mod script_editor;
pub mod settings;
pub mod splash;
pub mod text_touch;
pub mod theme;
pub mod ui;
pub mod ui_theme;
/// cartopolis.org's palette, generated from [`theme`] (test-only: it writes a
/// checked-in stylesheet and nothing at run time reads it).
#[cfg(test)]
mod web_theme;
pub mod welcome;
pub mod zoom_rail;
