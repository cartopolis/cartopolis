//! Chat UI (ADR-028, Simulation tier). Renders a collapsible panel with a tab per
//! channel — Local (proximity), Global, and joined rooms — and sends `ChatSend` /
//! `ChatSubscribe` through the online session. Only shown while connected.
//!
//! Geo tiers (country/province/municipality) land with `vibe_geo`; they slot in as
//! extra tabs with no change to this panel.

use bevy::prelude::*;
use vibe_core::{ChatScope, NetMessage};

use crate::resources::{OnlineSession, RightPanel};
use crate::systems::egui_manager::EguiManager;

/// Per-channel scrollback cap on the client.
const MAX_LINES: usize = 200;

#[derive(Resource)]
pub struct ChatState {
    pub channels: Vec<ChatChannel>,
    pub active: usize,
    pub input: String,
    /// Local say/shout toggle.
    pub shout: bool,
    pub join_input: String,
    /// This client's avatar id (from `ServerHelloAck`), to tag own lines.
    pub my_id: Option<u64>,
    /// Whether the floating chat-history window is open (toggled from the bar).
    pub history_open: bool,
}

pub struct ChatChannel {
    pub key: String,
    pub label: String,
    pub lines: Vec<ChatLine>,
}

pub struct ChatLine {
    pub sender: String,
    pub body: String,
    pub distance: Option<f32>,
}

impl Default for ChatState {
    fn default() -> Self {
        Self {
            channels: vec![
                ChatChannel {
                    key: "local".into(),
                    label: "Local".into(),
                    lines: Vec::new(),
                },
                ChatChannel {
                    key: "geo:".into(),
                    label: "Global".into(),
                    lines: Vec::new(),
                },
            ],
            active: 0,
            input: String::new(),
            shout: false,
            join_input: String::new(),
            my_id: None,
            history_open: true,
        }
    }
}

impl ChatState {
    /// Index of a channel by key, creating a room tab if absent.
    fn channel_index(&mut self, key: &str) -> usize {
        if let Some(i) = self.channels.iter().position(|c| c.key == key) {
            return i;
        }
        self.channels.push(ChatChannel {
            key: key.to_string(),
            label: channel_label(key),
            lines: Vec::new(),
        });
        self.channels.len() - 1
    }

    /// Append an incoming message to its channel (ADR-028 delivery).
    pub fn receive(&mut self, channel: &str, sender: String, body: String, distance: Option<f32>) {
        let idx = self.channel_index(channel);
        let lines = &mut self.channels[idx].lines;
        lines.push(ChatLine {
            sender,
            body,
            distance,
        });
        if lines.len() > MAX_LINES {
            let excess = lines.len() - MAX_LINES;
            lines.drain(0..excess);
        }
    }
}

fn channel_label(key: &str) -> String {
    match key {
        "local" => "Local".into(),
        "geo:" => "Global".into(),
        _ => key.strip_prefix("room:").unwrap_or(key).to_string(),
    }
}

/// Scope to send into for a channel key.
fn scope_for(key: &str, shout: bool) -> ChatScope {
    match key {
        "geo:" => ChatScope::Global,
        "local" => ChatScope::Local { shout },
        _ => ChatScope::Room(key.strip_prefix("room:").unwrap_or(key).to_string()),
    }
}

/// SL-style bottom bar (persistent nearby-chat input + a button strip that drives
/// the mutually-exclusive right-panel slot) plus the chat history as an animated
/// right panel. Present only when online (Simulation tier).
#[allow(clippy::too_many_arguments)]
pub fn render_chat_panel(
    mut egui: ResMut<EguiManager>,
    mut chat: ResMut<ChatState>,
    online: Option<Res<OnlineSession>>,
    mut right_panel: ResMut<RightPanel>,
    mut camera_state: ResMut<crate::resources::CameraState>,
    globe: Res<crate::systems::globe::GlobeState>,
    avatars: Query<(), With<crate::components::Avatar>>,
) {
    let Some(online) = online else {
        return;
    };
    let has_avatar = !avatars.is_empty();
    let mut close_history = false;

    let tabs: Vec<(String, String)> = chat
        .channels
        .iter()
        .map(|c| (c.key.clone(), c.label.clone()))
        .collect();
    let mut active = chat.active.min(tabs.len().saturating_sub(1));
    let is_local = tabs.get(active).map(|(k, _)| k == "local").unwrap_or(false);

    let mut send_now = false;
    let mut join_now = false;

    // --- Bottom bar: dark strip along the bottom (floating Area, doesn't disturb
    //     the existing panel layout). ---
    // Responsive: on a narrow (phone-ish) viewport the strip is too wide for one
    // row, so bound it to the viewport and let the controls wrap onto several rows
    // instead of clipping off both screen edges.
    let compact = egui.compact();
    let viewport_w = egui.viewport_pts().x;
    let ctx = egui.ctx_mut();
    egui::Area::new(egui::Id::new("sl_bottom_bar"))
        .anchor(egui::Align2::CENTER_BOTTOM, egui::vec2(0.0, -6.0))
        .show(ctx, |ui| {
            egui::Frame::NONE
                .fill(egui::Color32::from_rgba_unmultiplied(12, 16, 24, 225))
                .corner_radius(6.0)
                .inner_margin(egui::Margin::symmetric(8, 6))
                .show(ui, |ui| {
                    let row = |ui: &mut egui::Ui,
                               add: &mut dyn FnMut(&mut egui::Ui)| {
                        if compact {
                            // Bound the bar to the viewport (minus a small margin)
                            // so the wrapped row has an edge to wrap against.
                            ui.set_max_width((viewport_w - 24.0).max(200.0));
                            ui.horizontal_wrapped(add);
                        } else {
                            // Wide screens: unbounded content-sized single row,
                            // exactly as before.
                            ui.horizontal(add);
                        }
                    };
                    row(ui, &mut |ui| {
                        // Chat history toggle (drives the right-panel slot).
                        if ui
                            .selectable_label(right_panel.is(RightPanel::Chat), "💬 Chat")
                            .clicked()
                        {
                            right_panel.toggle(RightPanel::Chat);
                        }
                        // Channel + say/shout.
                        egui::ComboBox::from_id_salt("chat_scope")
                            .selected_text(
                                tabs.get(active).map(|(_, l)| l.as_str()).unwrap_or("Local"),
                            )
                            .show_ui(ui, |ui| {
                                for (i, (_k, label)) in tabs.iter().enumerate() {
                                    ui.selectable_value(&mut active, i, label);
                                }
                            });
                        if is_local {
                            ui.checkbox(&mut chat.shout, "Shout");
                        }
                        // Nearby-chat input (SL's persistent "say" line). Narrow it
                        // on a phone so the 280 pt default doesn't force the strip
                        // wider than the screen before wrapping can even help.
                        let input_w = if compact {
                            (viewport_w - 60.0).clamp(120.0, 280.0)
                        } else {
                            280.0
                        };
                        let resp = ui.add(
                            egui::TextEdit::singleline(&mut chat.input)
                                .desired_width(input_w)
                                .hint_text("Say something…"),
                        );
                        let entered =
                            resp.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter));
                        if (ui.button("Send").clicked() || entered)
                            && !chat.input.trim().is_empty()
                        {
                            send_now = true;
                        }

                        ui.separator();
                        // Camera-mode picker (moved off the auto-hiding top bar).
                        crate::systems::ui::camera_picker_ui(
                            ui,
                            &mut camera_state,
                            has_avatar,
                            &globe,
                        );

                        ui.separator();
                        // Button strip → mutually-exclusive right-panel slot.
                        for (panel, label) in [
                            (RightPanel::Appearance, "🧍 Appearance"),
                            (RightPanel::Layers, "🗺 Layers"),
                            (RightPanel::Route, "🧭 Route"),
                            (RightPanel::Places, "★ Places"),
                            (RightPanel::Assistant, "🤖 Assistant"),
                        ] {
                            if ui.selectable_label(right_panel.is(panel), label).clicked() {
                                right_panel.toggle(panel);
                            }
                        }
                    });
                });
        });

    // --- Chat history as an animated right panel (mutually exclusive slot). ---
    let mut chat_open = right_panel.is(RightPanel::Chat);
    crate::systems::egui_manager::responsive_right_panel("chat_panel", compact, viewport_w, 320.0)
        .show_collapsible(egui.ui_mut(), &mut chat_open, |ui| {
            ui.horizontal(|ui| {
                ui.heading("Nearby Chat");
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if ui.button("✕").clicked() {
                        close_history = true;
                    }
                });
            });
            ui.horizontal_wrapped(|ui| {
                for (i, (_key, label)) in tabs.iter().enumerate() {
                    ui.selectable_value(&mut active, i, label);
                }
            });
            ui.separator();
            egui::ScrollArea::vertical()
                .stick_to_bottom(true)
                .auto_shrink([false, false])
                .show(ui, |ui| {
                    if let Some(ch) = chat.channels.get(active) {
                        for line in &ch.lines {
                            let who = match line.distance {
                                Some(d) => format!("{} ({:.0} m)", line.sender, d),
                                None => line.sender.clone(),
                            };
                            ui.horizontal_wrapped(|ui| {
                                ui.label(
                                    egui::RichText::new(format!("{who}:"))
                                        .strong()
                                        .color(egui::Color32::from_rgb(120, 170, 235)),
                                );
                                ui.label(&line.body);
                            });
                        }
                    }
                });
            ui.separator();
            ui.horizontal(|ui| {
                ui.label("Join room:");
                ui.text_edit_singleline(&mut chat.join_input);
                join_now = ui.button("Join").clicked() && !chat.join_input.trim().is_empty();
            });
        });

    if close_history {
        *right_panel = RightPanel::None;
    }

    chat.active = active;

    if join_now {
        let room = chat.join_input.trim().to_string();
        let key = format!("room:{room}");
        let _ = online.intent_tx.send(NetMessage::ChatSubscribe {
            room: room.clone(),
            join: true,
        });
        let idx = chat.channel_index(&key);
        chat.active = idx;
        chat.join_input.clear();
    }

    if send_now {
        if let Some((key, _)) = tabs.get(active) {
            let scope = scope_for(key, chat.shout);
            let body = chat.input.trim().to_string();
            let _ = online.intent_tx.send(NetMessage::ChatSend {
                request_id: 0,
                scope,
                body,
            });
        }
        chat.input.clear();
    }
}
