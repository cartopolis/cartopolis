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

use crate::systems::egui_manager::{EguiMut, EguiRef};
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
    link: Option<Res<crate::systems::midas_link::MidasLink>>,
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
    // The Midas button holds the corner while the link is up; stand under it.
    let below = if link.is_some_and(|l| l.config.on && l.linked()) {
        theme::ROUND_BUTTON_SIZE + theme::OSD_GAP
    } else {
        0.0
    };

    egui::Area::new(egui::Id::new("mcp_activity"))
        // Top-right: the compass, the locate button and the layers disc hold the
        // top-left column, the dock and the stick hold the bottom, and this is
        // the corner nothing else draws in outside driver mode.
        .anchor(
            egui::Align2::RIGHT_TOP,
            egui::vec2(-theme::OSD_MARGIN, theme::OSD_MARGIN + below),
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
                                .color(theme::p().accent),
                        );
                    }
                    ui.label(
                        egui::RichText::new("Editor")
                            .size(theme::SIZE_CHIP)
                            .color(theme::p().cream)
                            .strong(),
                    );
                    ui.label(
                        egui::RichText::new(bridge.listen.display().to_string())
                            .size(theme::SIZE_TINY)
                            .color(theme::p().text_dim),
                    );
                });
                for entry in live {
                    let colour = match entry.outcome {
                        Outcome::Working => theme::p().accent,
                        Outcome::Done => theme::p().text_dim,
                        Outcome::Failed => theme::p().error,
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
                                    .color(theme::p().text_dim),
                            )
                            .wrap(),
                        );
                    });
                }
            });
        });
}

/// How long after its last call the agent still counts as at work in the room.
const AI_ACTIVE_SECS: f64 = 15.0;

/// While an agent works in the room (an MCP client, or a session on the Midas
/// page through the device link): a bar at the top saying what it is doing,
/// with Pause. Paused, its building tools answer that the person paused it
/// (`mcp::agent`), so it waits and the person steers — in the room, or in the
/// session's chat. Fed by the same activity log the desktop's editor overlay
/// draws (`McpBridge::log`), so every tool shows here with no extra wiring.
pub fn render_design_room_ai(
    egui: EguiMut,
    mut room: ResMut<crate::systems::design_room::DesignRoom>,
    bridge: Option<Res<McpBridge>>,
    real: Res<Time<bevy::time::Real>>,
) {
    if room.spot.is_none() {
        return;
    }
    let now = real.elapsed_secs_f64();
    let latest = bridge.as_ref().and_then(|b| {
        b.log
            .iter()
            .filter(|e| e.outcome == Outcome::Working || now - e.at <= AI_ACTIVE_SECS)
            .max_by(|a, b| a.at.total_cmp(&b.at))
            .cloned()
    });
    if latest.is_none() && !room.ai_paused {
        return;
    }
    let mut paused = room.ai_paused;
    crate::systems::design_room::room_panel(
        egui.ctx(),
        "design_room_ai",
        egui::Align2::CENTER_TOP,
        egui::vec2(0.0, theme::OSD_MARGIN),
        None,
        |ui| {
            ui.horizontal(|ui| {
                // A live dot: the agent at work, still when paused.
                let (dot, _) = ui.allocate_exact_size(egui::vec2(14.0, 14.0), egui::Sense::hover());
                let colour = if paused {
                    theme::p().warn
                } else {
                    theme::p().accent_bright
                };
                ui.painter()
                    .circle_filled(dot.center(), 7.0, colour.gamma_multiply(0.25));
                ui.painter().circle_filled(dot.center(), 4.0, colour);
                let (head, rest) = if paused {
                    ("Paused".to_string(), "the AI waits for you".to_string())
                } else {
                    match &latest {
                        Some(e) if e.outcome == Outcome::Working => (
                            "The AI is building".to_string(),
                            format!("{} {}", e.label, e.detail),
                        ),
                        Some(e) => ("The AI".to_string(), format!("{}: {}", e.label, e.detail)),
                        None => (String::new(), String::new()),
                    }
                };
                ui.label(egui::RichText::new(head).color(theme::p().cream));
                let rest: String = rest.chars().take(56).collect();
                ui.label(egui::RichText::new(rest).color(theme::p().text_dim));
                ui.add_space(8.0);
                let (icon, label, tip) = if paused {
                    (
                        crate::systems::icons::PLAY,
                        "Resume",
                        "let the AI carry on building",
                    )
                } else {
                    (
                        crate::systems::icons::STOP,
                        "Pause",
                        "hold the AI's next change until you resume it",
                    )
                };
                if theme::secondary_button(ui, icon, label, 0.0)
                    .on_hover_text(tip)
                    .clicked()
                {
                    paused = !paused;
                }
            });
        },
    );
    if paused != room.ai_paused {
        room.ai_paused = paused;
    }
}
