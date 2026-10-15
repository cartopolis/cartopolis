//! **What the editor is doing, while it does it.**
//!
//! A client being driven from somewhere else is otherwise a client that moves on
//! its own: the camera flies, the map re-streams, the shutter fires, and the
//! person sitting in front of it has no idea whether that was the agent, a bug,
//! or their own last keystroke. So every call this client serves is announced
//! here, in a box over the map, as it happens — the tool, what it was asked for,
//! and what it answered.
//!
//! It is a readout and nothing else. No buttons, `interactable(false)`, and it
//! stands down while a right-hand panel is open, which is the rule every other
//! floating box here follows: egui paints `Area`s above panels even at
//! `Order::Background`, so disappearing is the only way to be *behind* one.

use crate::systems::egui_manager::EguiRef;
use bevy::prelude::*;

use super::{McpBridge, Outcome};
use crate::systems::theme;

/// How long a finished line stays up after it lands.
///
/// Long enough to read a sentence, short enough that a client left running does
/// not keep a stale log over its map. A line that is still *working* never
/// expires — that one is the answer to "why is it moving".
const LINGER_S: f64 = 12.0;

/// The box's width. Fixed, because its content is a log: lines arrive at
/// unpredictable lengths and a box that resized around each one would twitch its
/// way across the corner.
const WIDTH: f32 = 268.0;

pub fn render_mcp_activity(
    egui: EguiRef,
    bridge: Res<McpBridge>,
    right_panel: Res<crate::resources::RightPanel>,
    driver: Res<crate::systems::driver::DriverState>,
    real: Res<Time<Real>>,
) {
    // Driver mode owns the screen, and the top-right corner is where its trip
    // strip goes. There is no ambient "driver mode is on" that hides an `Area`,
    // so anything new that draws over the map opts out by hand — see
    // `docs/notes/map-chrome.md`, which is a list of the things that did not.
    if !right_panel.is(crate::resources::RightPanel::None) || driver.active {
        return;
    }
    let now = real.elapsed_secs_f64();
    let live: Vec<_> = bridge
        .log
        .iter()
        .filter(|e| e.outcome == Outcome::Working || now - e.at < LINGER_S)
        .collect();
    if live.is_empty() {
        return;
    }
    // A capture in flight repaints for the whole of its settle, and egui would
    // otherwise re-present the frame it tessellated when the first line landed —
    // the spinner would be a still picture of a spinner.
    if bridge.busy() {
        egui.animating();
    }

    egui::Area::new(egui::Id::new("mcp_activity"))
        // Top-right: the compass, the locate button and the layers disc hold the
        // top-left column, the dock and the stick hold the bottom, and this is
        // the corner nothing else draws in outside driver mode.
        .anchor(
            egui::Align2::RIGHT_TOP,
            egui::vec2(-theme::OSD_MARGIN, theme::OSD_MARGIN),
        )
        // A readout over the map must not sense the taps meant for the world.
        .interactable(false)
        .show(egui.ctx(), |ui| {
            theme::overlay_frame().show(ui, |ui| {
                ui.set_width(WIDTH);
                ui.horizontal(|ui| {
                    if bridge.busy() {
                        ui.add(
                            egui::Spinner::new()
                                .size(theme::SIZE_SUB)
                                .color(theme::ACCENT),
                        );
                    }
                    ui.label(
                        egui::RichText::new("Editor")
                            .size(theme::SIZE_CHIP)
                            .color(theme::CREAM)
                            .strong(),
                    );
                    ui.label(
                        egui::RichText::new(bridge.listen.display().to_string())
                            .size(theme::SIZE_TINY)
                            .color(theme::TEXT_DIM),
                    );
                });
                for entry in live {
                    let colour = match entry.outcome {
                        Outcome::Working => theme::ACCENT,
                        Outcome::Done => theme::TEXT_DIM,
                        Outcome::Failed => theme::ERROR,
                    };
                    ui.horizontal_top(|ui| {
                        ui.label(
                            egui::RichText::new(&entry.label)
                                .size(theme::SIZE_TINY)
                                .color(colour)
                                .monospace(),
                        );
                        ui.add(
                            egui::Label::new(
                                egui::RichText::new(&entry.detail)
                                    .size(theme::SIZE_TINY)
                                    .color(theme::TEXT_DIM),
                            )
                            .wrap(),
                        );
                    });
                }
            });
        });
}
