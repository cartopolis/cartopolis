//! The bottom dock: one round icon button per right-hand panel.
//!
//! This used to be a wide strip of labelled toggles sharing the bottom of the
//! screen with the nearby-chat composer, camera picker and five text buttons —
//! it read as a taskbar, and the map ran out under it. The dock is what is left
//! once each of those goes where it belongs: the composer into the Chat panel
//! (it is chat, and the panel is already open when you are chatting), the camera
//! picker up beside the address bar (it is navigation), and Appearance and the
//! Assistant into the address bar's menu (both are settings you visit, not
//! things you flip while looking at the map).
//!
//! What remains is the four places you actually go, centred, as discs in the same
//! family as the compass in the corner. An icon and a tooltip carry a panel name
//! perfectly well when there are four of them and they never move.
//!
//! **Chat is the one session-bound entry.** It needs a server to talk to, so it
//! only appears when there is one; the map panels do not, and the dock therefore
//! renders in `--offline` too — which the old bar, living inside
//! `chat::render_chat_panel` behind its `OnlineSession` guard, did not.

use bevy::prelude::*;

use crate::resources::{OnlineSession, RightPanel};
use crate::systems::egui_manager::EguiManager;
use crate::systems::theme;

/// Gap between the discs.
const SPACING: f32 = 10.0;
/// Gap from the bottom edge.
const BOTTOM_MARGIN: f32 = 14.0;

/// The four panels that get a disc, in screen order. Their glyphs and names come
/// from [`RightPanel::icons`] / [`RightPanel::label`] — the same table the panel
/// headers wear, so a dock button and the header it opens can never drift apart.
const ENTRIES: [RightPanel; 4] = [
    RightPanel::Chat,
    RightPanel::Layers,
    RightPanel::Route,
    RightPanel::Places,
];

/// How much of the bottom edge the dock claims, in points.
///
/// Exported so that anything else anchored down there can sit *above* it rather
/// than guess. The discs are centred, so on a desktop window a corner-anchored
/// control misses them — and on a phone, where the row is most of the width, it
/// does not. `map_osd`'s thumbstick is the first caller; a second copy of this
/// arithmetic is how the two would drift when the dock grows a fifth entry.
#[must_use]
pub fn reserved_bottom() -> f32 {
    BOTTOM_MARGIN + theme::ROUND_BUTTON_SIZE
}

/// Draw the dock and route clicks into the (mutually exclusive) panel slot.
pub fn render_dock(
    mut egui: ResMut<EguiManager>,
    mut right_panel: ResMut<RightPanel>,
    online: Option<Res<OnlineSession>>,
    driver: Res<crate::systems::driver::DriverState>,
    keyboard: Res<crate::systems::onscreen_keyboard::OnScreenKeyboard>,
) {
    // Driving owns the screen — its own chrome already occupies the bottom.
    if driver.active {
        return;
    }
    // So does the on-screen keyboard, and for the same reason: it docks to the
    // bottom edge, which is exactly where these discs sit. An `Area` drawn later
    // paints over an earlier one, so leaving the dock up would not merely look
    // crowded — the discs would take taps meant for the bottom key row.
    if keyboard.is_open() {
        return;
    }
    let has_session = online.is_some();
    let ctx = egui.ctx_mut();
    let icons: Vec<&'static str> = ENTRIES
        .iter()
        .map(|p| theme::first_drawable(ctx, p.icons()))
        .collect();

    let mut clicked: Option<RightPanel> = None;
    egui::Area::new(egui::Id::new("panel_dock"))
        .anchor(egui::Align2::CENTER_BOTTOM, egui::vec2(0.0, -BOTTOM_MARGIN))
        .show(ctx, |ui| {
            ui.spacing_mut().item_spacing.x = SPACING;
            ui.horizontal(|ui| {
                for (panel, icon) in ENTRIES.iter().zip(&icons) {
                    if *panel == RightPanel::Chat && !has_session {
                        continue;
                    }
                    let open = right_panel.is(*panel);
                    if theme::round_button(ui, icon, panel.label(), open).clicked() {
                        clicked = Some(*panel);
                    }
                }
            });
        });

    if let Some(panel) = clicked {
        right_panel.toggle(panel);
    }
}
