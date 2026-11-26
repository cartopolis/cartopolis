//! The room the library's side panels take, kept in egui's layout for as long
//! as egui still lays out the map's chrome.
//!
//! Each panel's egui system used to draw, with the library on, an empty
//! unframed egui panel exactly the library panel's width, so the dock, the
//! search bar, the round buttons and the pins — still placed from egui's
//! leftover rect (`place_pins::map_area`) — moved aside, and a click there was
//! the UI's and not the world's. Those systems are gone with egui's screens;
//! without the strip the search bar and the right column slid under an open
//! panel. This one system keeps the strip for every such panel until the map's
//! free area comes from the library's own layout and egui goes.

use crate::resources::RightPanel;
use crate::systems::egui_manager::{responsive_right_panel, EguiMut};

/// The library panel a `RightPanel` opens, and the width it draws at on a wide
/// screen (each `lib_*` screen's own); `None` for what takes no side slot.
fn slot(panel: &RightPanel) -> Option<(&'static str, f32)> {
    Some(match panel {
        RightPanel::Settings => ("settings_panel", 340.0),
        RightPanel::Layers => ("data_layers_panel", 300.0),
        RightPanel::Places => ("places_panel", 300.0),
        RightPanel::Search => ("search_panel", 320.0),
        RightPanel::Admin => ("admin_panel", 360.0),
        RightPanel::Models => ("model_library_panel", 360.0),
        RightPanel::Script => ("script_panel", 420.0),
        RightPanel::About => ("about_panel", 320.0),
        // The Community panel (`lib_chats`).
        RightPanel::Chat => ("chats_panel", 360.0),
        _ => return None,
    })
}

/// An empty, unframed egui panel the width of the library panel that is open.
pub fn reserve_library_panels(mut egui: EguiMut, right_panel: bevy::prelude::Res<RightPanel>) {
    let Some((id, width)) = slot(&right_panel) else {
        return;
    };
    let compact = egui.compact();
    let viewport_w = egui.viewport_pts().x;
    let mut open = true;
    let mut strip = responsive_right_panel(id, compact, viewport_w, width)
        .frame(egui::Frame::NONE)
        .show_separator_line(false);
    if !compact {
        // Exactly the library panel's width: an empty resizable panel shrinks
        // to nothing, and the dock then sat over the panel's header.
        strip = strip.resizable(false).exact_size(width);
    }
    strip.show_collapsible(egui.ui_mut(), &mut open, |_ui| {});
}
