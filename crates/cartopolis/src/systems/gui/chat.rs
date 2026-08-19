//! Chat UI (ADR-028, Simulation tier). Renders a collapsible panel with a tab per
//! channel — Local (proximity), Global, and joined rooms — and sends `ChatSend` /
//! `ChatSubscribe` through the online session. Only shown while connected.
//!
//! Geo tiers (country/province/municipality) land with `cartopolis_geo`; they slot in as
//! extra tabs with no change to this panel.

use bevy::prelude::*;
use cartopolis_core::{ChatScope, NetMessage, ReportReason};

use crate::resources::{OnlineSession, RightPanel};
use crate::systems::egui_manager::EguiManager;
use crate::systems::icons;
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
    /// People this account blocks, exactly as the server last reported them
    /// (`NetMessage::BlockedList`).
    ///
    /// **The server's copy, never the client's guess.** The server already
    /// refuses to deliver a blocked person's messages, so this list is not what
    /// enforces the block — it is what draws the Blocked list and what hides
    /// lines that were already on screen when the block was made. Applying an
    /// edit locally before the server answers would leave the two disagreeing
    /// whenever one was refused (blocking yourself, say), and the UI would be
    /// the one that was wrong.
    pub blocked: Vec<String>,
    /// The report being composed, if any.
    pub report: Option<ReportDraft>,
    /// One-line feedback after a report is acknowledged.
    pub notice: Option<ChatNotice>,
}

/// A one-line answer to something the person just did.
#[derive(Clone)]
pub struct ChatNotice {
    pub text: String,
    /// `true` styles it as a failure. Both outcomes are shown: a report that
    /// vanishes without acknowledgement is indistinguishable from one nobody will
    /// read, and the person filing it has every reason to assume the worse.
    pub error: bool,
}

/// A report in progress. Held until the dialog is submitted or dismissed.
pub struct ReportDraft {
    pub username: String,
    /// The line that prompted the report, if it was raised from one. Sent along,
    /// because proximity chat is never stored server-side — without it a report
    /// about the thing that actually happened arrives with no evidence.
    pub message: Option<String>,
    pub reason: ReportReason,
    pub note: String,
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
            blocked: Vec::new(),
            report: None,
            notice: None,
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

    /// Whether this account blocks `name`. Case-insensitive, as the server's own
    /// comparison is — a block must not be walked around with the shift key.
    #[must_use]
    pub fn is_blocked(&self, name: &str) -> bool {
        self.blocked.iter().any(|b| b.eq_ignore_ascii_case(name))
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
    mut voice: ResMut<crate::systems::voice::VoiceState>,
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
    // Collected while the panel draws and acted on after it, because every one of
    // these mutates `chat` and the closure already holds it borrowed.
    let mut block_now: Option<String> = None;
    let mut unblock_now: Option<String> = None;
    let mut open_report: Option<(String, Option<String>)> = None;
    let blocked = chat.blocked.clone();
    let mut dismiss_notice = false;

    let compact = egui.compact();
    let viewport_w = egui.viewport_pts().x;

    // --- Chat history as an animated right panel (mutually exclusive slot). ---
    let chat_open_before = right_panel.is(RightPanel::Chat);
    let mut chat_open = chat_open_before;
    crate::systems::egui_manager::responsive_right_panel("chat_panel", compact, viewport_w, 320.0)
        .show_collapsible(egui.ui_mut(), &mut chat_open, |ui| {
            let panel = RightPanel::Chat;
            close_history = crate::systems::theme::panel_header(ui, panel.icon(), panel.label());
            ui.horizontal_wrapped(|ui| {
                for (i, (_key, label)) in tabs.iter().enumerate() {
                    ui.selectable_value(&mut active, i, label);
                }
            });
            // A refusal is an error chip; a confirmation is not. They are the same
            // field because they answer the same question ("did my report go?"),
            // and styling a success in the error tone would teach people to
            // ignore the tone.
            if let Some(notice) = chat.notice.clone() {
                if notice.error {
                    dismiss_notice = theme::inline_error(ui, &notice.text);
                } else {
                    ui.horizontal_wrapped(|ui| {
                        theme::note(ui, &notice.text);
                        if theme::action_button(ui, "OK").clicked() {
                            dismiss_notice = true;
                        }
                    });
                }
            }
            // Only once there is something in it. An empty "Blocked (0)" header on
            // every session is chrome that says nothing; the list is a place to
            // *undo* a block, and until one exists there is nothing to undo.
            if !blocked.is_empty() {
                egui::CollapsingHeader::new(format!("Blocked ({})", blocked.len()))
                    .id_salt("chat_blocked")
                    .show(ui, |ui| {
                        for name in &blocked {
                            ui.horizontal(|ui| {
                                ui.label(egui::RichText::new(name).color(theme::TEXT));
                                ui.with_layout(
                                    egui::Layout::right_to_left(egui::Align::Center),
                                    |ui| {
                                        if theme::action_button(ui, "Unblock").clicked() {
                                            unblock_now = Some(name.clone());
                                        }
                                    },
                                );
                            });
                        }
                    });
            }
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
                        ui.label("Join room");
                        // Enter submits, as it does in every other field in the
                        // client — this was the one that only took a click.
                        let field = crate::systems::text_touch::field(
                            ui,
                            egui::TextEdit::singleline(&mut chat.join_input),
                        );
                        let entered =
                            field.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter));
                        // Disabled on an empty field rather than live-and-inert.
                        // `data_layers` states the rule this restores — "a control
                        // that swallows a tap is indistinguishable from a broken
                        // app" — and Join was pressing to no effect and no message.
                        let ready = !chat.join_input.trim().is_empty();
                        let join = crate::systems::theme::action_enabled_because(
                            ui,
                            ready,
                            "Join",
                            "Type a room name first",
                        );
                        join_now = (join.clicked() || entered) && ready;
                    });
                    ui.separator();
                    // Voice sits with the Local tab and only there: the server
                    // refuses global voice outright, and a room send would need
                    // the room this composer is not scoped to.
                    if is_local {
                        voice_row(ui, &mut voice);
                        ui.separator();
                    }
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
                        // Same rule as Join above.
                        let ready = !chat.input.trim().is_empty();
                        let send = crate::systems::theme::action_enabled_because(
                            ui,
                            ready,
                            "Send",
                            "Type a message first",
                        );
                        // `desired_width`, not `add_sized(available_size())`: this
                        // layout's available *height* is the whole rest of the
                        // panel, and sizing to it grows the single-line field into
                        // a full-height block.
                        let field = crate::systems::text_touch::field(
                            ui,
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
                    let empty = chat.channels.get(active).is_none_or(|c| c.lines.is_empty());
                    if empty {
                        theme::empty_state(
                            ui,
                            "No messages yet",
                            "Anything you say here reaches the people nearby.",
                        );
                    }
                    if let Some(ch) = chat.channels.get(active) {
                        for line in &ch.lines {
                            // The server stops a blocked person's *new* messages;
                            // this hides the ones that were already on screen when
                            // the block was made, which is the half a person
                            // notices — they blocked someone and the abuse stayed
                            // where it was.
                            if chat.is_blocked(&line.sender) {
                                continue;
                            }
                            let who = match line.distance {
                                Some(d) => format!("{} ({:.0} m)", line.sender, d),
                                None => line.sender.clone(),
                            };
                            ui.horizontal_wrapped(|ui| {
                                // The name is the handle for everything you can do
                                // *about* a person, which is why it senses clicks
                                // at all. It has no click action of its own, so it
                                // needs no `long_touched` guard — the trap the
                                // Places rows document applies to a widget that
                                // both jumps somewhere and opens a menu.
                                let name = ui.add(
                                    egui::Label::new(
                                        egui::RichText::new(format!("{who}:"))
                                            .strong()
                                            .color(theme::ACCENT_TEXT),
                                    )
                                    .sense(egui::Sense::click()),
                                );
                                name.context_menu(|ui| {
                                    ui.label(
                                        egui::RichText::new(&line.sender)
                                            .strong()
                                            .color(theme::CREAM),
                                    );
                                    ui.separator();
                                    if ui.button("Block").clicked() {
                                        block_now = Some(line.sender.clone());
                                        ui.close();
                                    }
                                    if ui.button("Report…").clicked() {
                                        open_report =
                                            Some((line.sender.clone(), Some(line.body.clone())));
                                        ui.close();
                                    }
                                });
                                ui.label(&line.body);
                            });
                        }
                    }
                });
        });

    // A drag on the panel's edge that pulls it shut writes `open = false` and
    // never touches the header, so a panel that consults only its ✕ reopens on
    // the next frame — the slot still says it is the active panel. Both are the
    // same request. (`data_layers` and About were already written this way.)
    if close_history || (chat_open_before && !chat_open) {
        *right_panel = RightPanel::None;
    }

    chat.active = active;

    if join_now {
        let room = chat.join_input.trim().to_string();
        let key = format!("room:{room}");
        online.send(
            "chat subscribe",
            NetMessage::ChatSubscribe {
                room: room.clone(),
                join: true,
            },
        );
        let idx = chat.channel_index(&key);
        chat.active = idx;
        chat.join_input.clear();
    }

    if send_now {
        if let Some((key, _)) = tabs.get(active) {
            let scope = scope_for(key, chat.shout);
            let body = chat.input.trim().to_string();
            online.send(
                "chat send",
                NetMessage::ChatSend {
                    request_id: 0,
                    scope,
                    body,
                },
            );
        }
        chat.input.clear();
    }

    if dismiss_notice {
        chat.notice = None;
    }
    for (name, on) in [(block_now, true), (unblock_now, false)] {
        let Some(username) = name else { continue };
        online.send(
            "set blocked",
            NetMessage::SetBlocked {
                request_id: 0,
                username,
                blocked: on,
            },
        );
    }
    if let Some((username, message)) = open_report {
        chat.report = Some(ReportDraft {
            username,
            message,
            reason: ReportReason::Harassment,
            note: String::new(),
        });
    }

    render_report_dialog(&mut egui, &mut chat, &online);
}

/// The voice controls: arm the microphone, hold to talk, and see that it works.
///
/// # Why *Hold to talk* is a button and not only a key
///
/// A phone has no V key, and the on-screen keyboard is a text field's
/// affordance — so a push-to-talk bound to the keyboard alone is a feature the
/// platform this was built for cannot use. The button reads
/// `is_pointer_button_down_on`, which the touch bridge feeds from a real finger
/// (`systems::touch` emulates a left button from the primary touch), so one
/// control serves both.
///
/// # Why the meter is here rather than only in Settings
///
/// The question a person asks at the moment they first press this is "is it
/// picking me up?", and the honest answer is a bar that moves. Without it a
/// muted microphone, a denied permission and a room where nobody is listening
/// are the same experience: silence.
fn voice_row(ui: &mut egui::Ui, voice: &mut crate::systems::voice::VoiceState) {
    use crate::systems::voice::{VoiceMode, VoiceState};

    if !VoiceState::can_capture() {
        theme::note(ui, "This build has no microphone support.");
        return;
    }

    let mut mode = voice.mode;
    ui.horizontal_wrapped(|ui| {
        ui.label(egui::RichText::new("Voice").color(theme::TEXT));
        ui.selectable_value(&mut mode, VoiceMode::Off, "Off");
        ui.selectable_value(&mut mode, VoiceMode::PushToTalk, "Hold");
        ui.selectable_value(&mut mode, VoiceMode::Open, "Open mic");
    });
    if mode != voice.mode {
        voice.set_mode(mode);
    }

    if voice.mode == VoiceMode::Off {
        return;
    }

    if let Some(err) = voice.error.clone() {
        if theme::inline_error(ui, &err) {
            voice.error = None;
        }
        return;
    }

    if voice.mode == VoiceMode::PushToTalk {
        // Sized for a thumb rather than a cursor: this is the one control in the
        // panel that is *held*, and a 20-pixel row is not something anyone holds
        // accurately while walking.
        let talk = ui.add_sized(
            egui::vec2(ui.available_width(), 34.0),
            egui::Button::new(if voice.transmitting {
                // A microphone, which is what this control is about. It was `◉`
                // out of a `first_drawable` ladder because U+25CF is in none of
                // the fonts egui ships — the same trap the Settings mic button
                // hit — and the bundled icon subset settles it (`systems::icons`).
                format!("{}  Talking", icons::MIC)
            } else {
                "Hold to talk  (V)".to_string()
            }),
        );
        // Held, not clicked: a click is an edge and this is a state. The system
        // that reads the V key ORs into the same flag and neither clears it.
        if talk.is_pointer_button_down_on() {
            voice.talk_held = true;
        }
    }

    // The meter is the answer to "is it hearing me", so it is shown whenever the
    // device is armed — including while *not* transmitting, which is what tells
    // an open-mic user that the gate is closed rather than the microphone dead.
    let meter = crate::platform::audio::level_to_meter(voice.level);
    crate::systems::settings::level_bar(ui, meter, 0.0);

    let audible = voice.speakers.len();
    if audible > 0 {
        let nearest = voice
            .speakers
            .values()
            .filter_map(|s| s.distance_m)
            .fold(f32::INFINITY, f32::min);
        let who = if audible == 1 {
            "1 person".to_string()
        } else {
            format!("{audible} people")
        };
        theme::note(
            ui,
            &if nearest.is_finite() {
                format!("Hearing {who} — nearest {nearest:.0} m")
            } else {
                format!("Hearing {who}")
            },
        );
    }
}

/// The report form: who, why, and anything the reporter wants to add.
///
/// A `Window` rather than a section of the chat panel, for two reasons. Reporting
/// is a considered act, and it interrupts — a form that scrolls away with the
/// backlog is one you lose halfway through writing. And a report is raised from
/// places other than the chat panel eventually (an avatar's context menu, a
/// prim's), so it cannot live inside one panel's layout.
fn render_report_dialog(
    egui: &mut EguiManager,
    chat: &mut ChatState,
    online: &crate::resources::OnlineSession,
) {
    let Some(draft) = chat.report.as_mut() else {
        return;
    };
    let mut open = true;
    let mut submit = false;
    let mut cancel = false;

    egui::Window::new("Report a person")
        .collapsible(false)
        .resizable(false)
        .default_width(320.0)
        .anchor(egui::Align2::CENTER_CENTER, egui::Vec2::ZERO)
        .open(&mut open)
        .show(egui.ui_mut().ctx(), |ui| {
            ui.label(
                egui::RichText::new(&draft.username)
                    .strong()
                    .color(theme::CREAM),
            );
            if let Some(message) = &draft.message {
                theme::note(ui, &format!("“{message}”"));
            }
            ui.add_space(6.0);
            ui.label(egui::RichText::new("What happened?").color(theme::TEXT));
            for reason in ReportReason::ALL {
                ui.radio_value(&mut draft.reason, *reason, reason.label());
            }
            ui.add_space(6.0);
            ui.add(
                egui::TextEdit::multiline(&mut draft.note)
                    .desired_rows(3)
                    .desired_width(f32::INFINITY)
                    .hint_text("Anything else we should know (optional)"),
            );
            theme::note(
                ui,
                "Reports go to the people who run this world. Blocking is separate \
                 and takes effect immediately.",
            );
            ui.horizontal(|ui| {
                submit = theme::action_button(ui, "Send report").clicked();
                cancel = theme::action_button(ui, "Cancel").clicked();
            });
        });

    if submit {
        online.send(
            "report user",
            NetMessage::ReportUser {
                request_id: 0,
                username: draft.username.clone(),
                reason: draft.reason,
                note: draft.note.clone(),
                message: draft.message.clone(),
            },
        );
    }
    if submit || cancel || !open {
        chat.report = None;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_block_is_matched_regardless_of_case() {
        let mut chat = ChatState::default();
        chat.blocked = vec!["Loud".into()];
        assert!(chat.is_blocked("loud"));
        assert!(chat.is_blocked("LOUD"));
        assert!(!chat.is_blocked("louder"));
        assert!(!chat.is_blocked("quiet"));
    }

    /// The list is whatever the server last said, so an empty `BlockedList`
    /// clears it rather than being read as "no news".
    #[test]
    fn an_empty_list_from_the_server_unblocks_everyone() {
        let mut chat = ChatState::default();
        chat.blocked = vec!["loud".into()];
        chat.blocked = Vec::new();
        assert!(!chat.is_blocked("loud"));
    }

    /// Lines already in the scrollback when a block is made must disappear too —
    /// the server can only withhold what it has not sent yet.
    #[test]
    fn blocking_hides_a_line_that_already_arrived() {
        let mut chat = ChatState::default();
        chat.receive("local", "loud".into(), "before the block".into(), Some(3.0));
        let visible = |chat: &ChatState| {
            chat.channels[0]
                .lines
                .iter()
                .filter(|l| !chat.is_blocked(&l.sender))
                .count()
        };
        assert_eq!(visible(&chat), 1);
        chat.blocked = vec!["loud".into()];
        assert_eq!(visible(&chat), 0, "an already-received line stayed visible");
    }
}
