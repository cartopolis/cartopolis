//! `systems::gui` — one file per concern; see systems/mod.rs for the map.

/// The group's registration: its resources and its systems, each in its
/// `crate::phase::ClientPhase`. Added by `run_client`.
mod plugin;
pub use plugin::GuiPlugin;

pub mod bookmarks;
pub mod building_card;
pub mod chat;
pub mod chat_overlay;
pub mod chat_people;
pub mod chats;
pub mod contacts;
pub mod data_layers;
pub mod dismiss;
pub mod dock;
pub mod egui_manager;
pub mod glass;
pub mod hud;
pub mod icons;
pub mod login_screen;
pub mod map_osd;
pub mod model_library;
pub mod onscreen_keyboard;
pub mod place_card;
pub mod place_pins;
pub mod script_editor;
pub mod settings;
pub mod splash;
pub mod text_touch;
pub mod theme;
pub mod ui;
