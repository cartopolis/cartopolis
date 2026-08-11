//! Chat UI (ADR-028, Simulation tier). Renders a collapsible panel with a tab per
//! channel — Local (proximity), Global, and joined rooms — and sends `ChatSend` /
//! `ChatSubscribe` through the online session. Only shown while connected.
//!
//! Geo tiers (country/province/municipality) land with `cartopolis_geo`; they slot in as
//! extra tabs with no change to this panel.

use bevy::prelude::*;
use cartopolis_core::{ChatScope, NetMessage};

use crate::resources::{OnlineSession, RightPanel};
use crate::systems::egui_manager::EguiManager;
use crate::systems::theme;

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

/// The chat panel: channel tabs, history, and the composer.
///
/// The composer used to be a persistent "say" line in the bottom bar, Second
/// Life style, with the channel picker and the Shout toggle beside it. It has
/// moved in here, and three things fell out of that. The scope **combo box is
/// gone** — the panel already has channel tabs, and two controls picking the same
/// value is one too many. The bottom of the screen is back to being map. And the
/// composer now sits under the history it writes into, which is where every chat
/// client puts it and where the reply you are answering is still on screen.
///
/// Present only when online (Simulation tier) — there is no one to talk to
/// otherwise. The dock button that opens this panel is hidden on the same test.
pub fn render_chat_panel(
    mut egui: ResMut<EguiManager>,
    mut chat: ResMut<ChatState>,
    online: Option<Res<OnlineSession>>,
    mut right_panel: ResMut<RightPanel>,
) {
    let Some(online) = online else {
        return;
    };
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

    let compact = egui.compact();
    let viewport_w = egui.viewport_pts().x;

    // --- Chat history as an animated right panel (mutually exclusive slot). ---
    let mut chat_open = right_panel.is(RightPanel::Chat);
    crate::systems::egui_manager::responsive_right_panel("chat_panel", compact, viewport_w, 320.0)
        .show_collapsible(egui.ui_mut(), &mut chat_open, |ui| {
            let panel = RightPanel::Chat;
            close_history = crate::systems::theme::panel_header(ui, panel.icons(), panel.label());
            ui.horizontal_wrapped(|ui| {
                for (i, (_key, label)) in tabs.iter().enumerate() {
                    ui.selectable_value(&mut active, i, label);
                }
            });
            ui.separator();
            // The composer and the Join row are laid out *first*, from the bottom
            // up, so the history takes whatever is left. Give the scroll area the
            // remaining height instead and it claims the whole panel, pushing the
            // input off the bottom edge the moment the backlog is long enough —
            // which is exactly when you want to reply.
            egui::Panel::bottom("chat_composer")
                .show_separator_line(false)
                .frame(egui::Frame::NONE.outer_margin(egui::Margin {
                    top: 6,
                    ..Default::default()
                }))
                .show(ui, |ui| {
                    ui.horizontal(|ui| {
                        ui.label("Join room:");
                        ui.text_edit_singleline(&mut chat.join_input);
                        join_now =
                            ui.button("Join").clicked() && !chat.join_input.trim().is_empty();
                    });
                    ui.separator();
                    // Shout is a property of the *local* channel only; a room or
                    // the global channel has no distance to shout across.
                    if is_local {
                        crate::systems::theme::checkbox(ui, &mut chat.shout, "Shout");
                    }
                    // Right-to-left so Send lands at the right edge and the field
                    // takes whatever is left: the panel is resizable and responsive,
                    // and a fixed input width either overflows the narrow layout or
                    // leaves a gap in the wide one.
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        let send = ui.button("Send");
                        // `desired_width`, not `add_sized(available_size())`: this
                        // layout's available *height* is the whole rest of the
                        // panel, and sizing to it grows the single-line field into
                        // a full-height block.
                        let field = ui.add(
                            egui::TextEdit::singleline(&mut chat.input)
                                .desired_width(ui.available_width())
                                .hint_text("Say something…"),
                        );
                        let entered =
                            field.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter));
                        if (send.clicked() || entered) && !chat.input.trim().is_empty() {
                            send_now = true;
                            field.request_focus();
                        }
                    });
                });
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
                                        .color(theme::ACCENT_TEXT),
                                );
                                ui.label(&line.body);
                            });
                        }
                    }
                });
        });

    if close_history {
        *right_panel = RightPanel::None;
    }

    chat.active = active;

    if join_now {
        let room = chat.join_input.trim().to_string();
        let key = format!("room:{room}");
        let _ = online.intent_tx.unbounded_send(NetMessage::ChatSubscribe {
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
            let _ = online.intent_tx.unbounded_send(NetMessage::ChatSend {
                request_id: 0,
                scope,
                body,
            });
        }
        chat.input.clear();
    }
}
