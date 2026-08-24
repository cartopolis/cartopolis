//! Chat state (ADR-028, Simulation tier), and the panel that is *not* how you
//! chat.
//!
//! **Talking happens over the map** — [`crate::systems::chat_overlay`] draws a
//! speech bubble above whoever said it, an ambient transcript that fades, and a
//! composer pill that appears only while you are writing. This module owns the
//! state all three read, and one panel that is now a **log and its settings**:
//! the whole backlog in one scroll, the rooms you are in, the people you block,
//! the microphone, and the report form. It is reached from ☰ rather than from
//! the dock, because it is somewhere you visit, not the way you speak.
//!
//! Why the panel stopped being the chat: on a compact viewport a right panel is
//! the *whole screen* (`egui_manager::responsive_right_panel`), so saying
//! anything meant covering the world and the people in it — in a client whose
//! Local channel is scoped by how far away those people are standing. See
//! `docs/notes/chat-without-a-panel.md`.
//!
//! Geo tiers (country/province/municipality) land with `cartopolis_geo`; they slot in as
//! extra channels with no change to any of this.

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
    /// Where the composer sends — an index into [`Self::channels`]. It used to be
    /// the selected *tab*; with the tabs gone it is the scope chip's value and
    /// nothing else reads it.
    pub active: usize,
    pub input: String,
    /// Local say/shout toggle.
    pub shout: bool,
    pub join_input: String,
    /// Seconds since app start, as of the last frame the overlay ran.
    ///
    /// **Chat is timed on the client, not on `sent_at_ms`.** Everything the
    /// overlay does with a clock — hold a bubble up, fade a transcript line,
    /// decide what "recent" means — is about how long *this* screen has shown a
    /// line, so a server clock (unsynchronised, and skewed by however long the
    /// message spent in flight) would make a bubble vanish early or linger on a
    /// client whose watch disagrees. `sent_at_ms` stays on the wire for the log
    /// and for ordering; this is what the picture runs on.
    pub clock: f64,
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

#[derive(Clone)]
pub struct ChatLine {
    pub sender: String,
    /// The speaker's avatar id, which is what hangs a line on the person who
    /// said it: `RemoteAvatar::sim_id` is the same number, so a bubble finds its
    /// entity with no name matching and no protocol change. `0` for anything
    /// that arrived without one.
    pub sender_id: u64,
    pub body: String,
    pub distance: Option<f32>,
    /// [`ChatState::clock`] when this line landed — the age every fade is
    /// measured from.
    pub arrived: f64,
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
            clock: 0.0,
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

    /// Forget a channel we have left, keeping the send scope on something that
    /// still exists.
    ///
    /// The clamp is the whole reason this is a method: `active` is an index, and
    /// leaving the room you were speaking in would otherwise leave the composer
    /// pointing one past the end — where `scope_for` never runs and Send is a
    /// button that does nothing.
    pub fn drop_channel(&mut self, key: &str) {
        let Some(i) = self.channels.iter().position(|c| c.key == key) else {
            return;
        };
        self.channels.remove(i);
        self.active = self.active.min(self.channels.len().saturating_sub(1));
    }

    /// Whether this account blocks `name`. Case-insensitive, as the server's own
    /// comparison is — a block must not be walked around with the shift key.
    #[must_use]
    pub fn is_blocked(&self, name: &str) -> bool {
        self.blocked.iter().any(|b| b.eq_ignore_ascii_case(name))
    }

    /// Append an incoming message to its channel (ADR-028 delivery).
    pub fn receive(
        &mut self,
        channel: &str,
        sender_id: u64,
        sender: String,
        body: String,
        distance: Option<f32>,
    ) {
        let arrived = self.clock;
        let idx = self.channel_index(channel);
        let lines = &mut self.channels[idx].lines;
        lines.push(ChatLine {
            sender,
            sender_id,
            body,
            distance,
            arrived,
        });
        if lines.len() > MAX_LINES {
            let excess = lines.len() - MAX_LINES;
            lines.drain(0..excess);
        }
    }

    /// The lines that landed in the last `window` seconds, newest last, across
    /// **every** channel, capped at `max`.
    ///
    /// Merged rather than per-channel because the transcript has no tabs to
    /// scope it to: what it shows is "what has been said to me lately", which is
    /// one stream however many channels fed it. Blocked senders are dropped
    /// here, so no caller has to remember to (see [`Self::is_blocked`] — the
    /// server withholds new messages, this hides the ones already on screen).
    ///
    /// Each entry carries the channel it came from, so the transcript can prefix
    /// the ones that are *not* proximity chat. A Local line needs no prefix: the
    /// whole design is that you can see who said it.
    #[must_use]
    pub fn recent(&self, now: f64, window: f64, max: usize) -> Vec<(&ChatChannel, &ChatLine)> {
        let mut out: Vec<(&ChatChannel, &ChatLine)> = self
            .channels
            .iter()
            .flat_map(|c| c.lines.iter().map(move |l| (c, l)))
            .filter(|(_, l)| now - l.arrived <= window)
            .filter(|(_, l)| !self.is_blocked(&l.sender))
            .collect();
        // By arrival, which is the order they appeared on this screen — and
        // therefore the order a reader saw them in, whatever the senders' clocks
        // said. Stable, so two lines that landed on one frame keep channel order.
        out.sort_by(|a, b| a.1.arrived.total_cmp(&b.1.arrived));
        if out.len() > max {
            out.drain(0..out.len() - max);
        }
        out
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
pub fn scope_for(key: &str, shout: bool) -> ChatScope {
    match key {
        "geo:" => ChatScope::Global,
        "local" => ChatScope::Local { shout },
        _ => ChatScope::Room(key.strip_prefix("room:").unwrap_or(key).to_string()),
    }
}

/// The chat panel: the whole log, the rooms, the blocks and the microphone.
///
/// **This is not where you talk.** The composer, the scope picker and the
/// per-channel tabs left for [`crate::systems::chat_overlay`], which draws them
/// over the map — see this module's note. What a panel is still good at is the
/// things you do *occasionally* and want space for: reading back further than
/// the transcript keeps, joining and leaving rooms, undoing a block, arming the
/// microphone, and the report form.
///
/// The history here is **merged across every channel**, in arrival order, the
/// same stream the transcript shows and for the same reason: the tabs are gone,
/// so there is nothing to scope it to, and a room's lines interleaved with the
/// proximity ones is what actually happened.
///
/// Present only when online (Simulation tier) — there is no one to talk to
/// otherwise, and the ☰ entry that opens it is hidden on the same test.
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

    // Everything between here and the end of `right_panel` below composes only
    // when this panel is the open one, but building its inputs is not free:
    // `recent` allocates a `Vec` over every line of every channel, sorts it, runs
    // `is_blocked` per line, and then two `String`s are cloned per line on top —
    // several hundred allocations every frame of an online session, for a panel
    // nobody is looking at. `bookmarks::render_bookmarks_panel` guards itself the
    // same way and says so.
    //
    // The report dialog is deliberately *outside* that guard. It is not part of
    // the panel: `person_menu` hangs off a name in the overlay's transcript and
    // off a speech bubble over an avatar's head as well as off the panel's log
    // (see its doc comment), so a report is routinely opened — and must stay on
    // screen — with this panel shut.
    if !right_panel.is(RightPanel::Chat) {
        render_report_dialog(&mut egui, &mut chat, &online);
        return;
    }

    let mut join_now = false;
    // Collected while the panel draws and acted on after it, because every one of
    // these mutates `chat` and the closure already holds it borrowed.
    let mut block_now: Option<String> = None;
    let mut unblock_now: Option<String> = None;
    let mut leave_now: Option<String> = None;
    let mut open_report: Option<(String, Option<String>)> = None;
    let blocked = chat.blocked.clone();
    let mut dismiss_notice = false;
    // Every joined room, as (channel key, name). The two built-in channels are
    // not rooms and cannot be left.
    let rooms: Vec<(String, String)> = chat
        .channels
        .iter()
        .filter(|c| c.key.starts_with("room:"))
        .map(|c| (c.key.clone(), c.label.clone()))
        .collect();
    // The log, flattened out of the borrow before the closure takes `chat`
    // mutably. `f64::INFINITY` because a *log* has no recency window — that is
    // the transcript's job, and this is the place you come when the transcript
    // has already faded.
    let log: Vec<(String, ChatLine)> = chat
        .recent(chat.clock, f64::INFINITY, usize::MAX)
        .into_iter()
        .map(|(c, l)| {
            let prefix = if c.key == "local" {
                String::new()
            } else {
                format!("[{}] ", c.label)
            };
            (prefix, l.clone())
        })
        .collect();

    crate::systems::egui_manager::right_panel(
        &mut egui,
        &mut right_panel,
        RightPanel::Chat,
        "chat_panel",
        320.0,
        |ui| {
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
            voice_row(ui, &mut voice);
            ui.separator();

            theme::section(ui, "Rooms");
            for (key, label) in &rooms {
                ui.horizontal(|ui| {
                    ui.label(egui::RichText::new(label).color(theme::TEXT));
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        if theme::action_button(ui, "Leave").clicked() {
                            leave_now = Some(key.clone());
                        }
                    });
                });
            }
            ui.horizontal(|ui| {
                // Enter submits, as it does in every other field in the client.
                let field = crate::systems::text_touch::field(
                    ui,
                    egui::TextEdit::singleline(&mut chat.join_input)
                        .desired_width(ui.available_width() * 0.55)
                        .hint_text("Room name"),
                );
                let entered = field.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter));
                // Disabled on an empty field rather than live-and-inert.
                // `data_layers` states the rule this restores — "a control that
                // swallows a tap is indistinguishable from a broken app".
                let ready = !chat.join_input.trim().is_empty();
                let join = crate::systems::theme::action_enabled_because(
                    ui,
                    ready,
                    "Join",
                    "Type a room name first",
                );
                join_now = (join.clicked() || entered) && ready;
            });

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
            theme::section(ui, "Everything said");
            egui::ScrollArea::vertical()
                .stick_to_bottom(true)
                .auto_shrink([false, false])
                .show(ui, |ui| {
                    if log.is_empty() {
                        theme::empty_state(
                            ui,
                            "Nothing said yet",
                            "What people say near you appears over their heads, and here.",
                        );
                    }
                    for (prefix, line) in &log {
                        let who = match line.distance {
                            Some(d) => format!("{}{} ({:.0} m)", prefix, line.sender, d),
                            None => format!("{}{}", prefix, line.sender),
                        };
                        ui.horizontal_wrapped(|ui| {
                            // The name is the handle for everything you can do
                            // *about* a person, which is why it senses clicks at
                            // all. It has no click action of its own, so it needs
                            // no `long_touched` guard — the trap the Places rows
                            // document applies to a widget that both jumps
                            // somewhere and opens a menu.
                            let name = ui.add(
                                egui::Label::new(
                                    egui::RichText::new(format!("{who}:"))
                                        .strong()
                                        .color(theme::ACCENT_TEXT),
                                )
                                .sense(egui::Sense::click()),
                            );
                            person_menu(
                                &name,
                                &line.sender,
                                Some(&line.body),
                                |action| match action {
                                    PersonAction::Block => block_now = Some(line.sender.clone()),
                                    PersonAction::Report(msg) => {
                                        open_report = Some((line.sender.clone(), msg))
                                    }
                                },
                            );
                            ui.label(&line.body);
                        });
                    }
                });
        },
    );

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
        // Joining a room is also choosing to speak in it: the scope chip follows,
        // which is the one thing the vanished tab strip did that mattered.
        let idx = chat.channel_index(&key);
        chat.active = idx;
        chat.join_input.clear();
    }

    if let Some(key) = leave_now {
        let room = key.strip_prefix("room:").unwrap_or(&key).to_string();
        online.send(
            "chat unsubscribe",
            NetMessage::ChatSubscribe { room, join: false },
        );
        chat.drop_channel(&key);
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

/// What the person menu can ask for.
pub enum PersonAction {
    Block,
    /// Report, carrying the line that prompted it when there is one.
    Report(Option<String>),
}

/// The menu behind a speaker's name: block them, or report them.
///
/// One function because it hangs off three different widgets now — a name in the
/// panel's log, a name in the transcript, and a speech bubble over an avatar's
/// head — and "the moderation menu differs depending on where you found the
/// person" is not a difference anyone wants to discover while being harassed.
pub fn person_menu(
    response: &egui::Response,
    sender: &str,
    message: Option<&str>,
    mut act: impl FnMut(PersonAction),
) {
    response.context_menu(|ui| {
        ui.label(egui::RichText::new(sender).strong().color(theme::CREAM));
        ui.separator();
        if ui.button("Block").clicked() {
            act(PersonAction::Block);
            ui.close();
        }
        if ui.button("Report…").clicked() {
            act(PersonAction::Report(message.map(str::to_string)));
            ui.close();
        }
    });
}

/// The voice controls: arm the microphone, hold to talk, and see that it works.
///
/// # Why it is always proximity voice
///
/// This used to be drawn only while the Local tab was selected, because the
/// server refuses global voice outright and a room send would need the room the
/// composer was scoped to. With the tabs gone there is no scope here to be wrong
/// about: voice is proximity voice, unconditionally, and the panel says so by
/// being the only place it appears.
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
        chat.receive(
            "local",
            7,
            "loud".into(),
            "before the block".into(),
            Some(3.0),
        );
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
