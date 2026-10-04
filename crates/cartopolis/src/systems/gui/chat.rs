//! Chat state (ADR-028, Simulation tier): every channel, its lines, and the
//! requests the screens make of the network and encryption systems.
//!
//! Two sets of screens read it. **Over the map**, [`crate::systems::chat_overlay`]
//! draws a speech bubble above whoever said it nearby, an ambient transcript and
//! a composer pill. **The messenger**, [`crate::systems::chats`], is the chat
//! list and one chat at a time in a Chat and a World view (the v5 design). This
//! module draws only the person menu and the report form both of them use.
//!
//! Geo tiers (country/province/municipality) land with `cartopolis_geo`; they slot in as
//! extra channels with no change to any of this.

use bevy::prelude::*;
use cartopolis_core::{ChatScope, NetMessage, ReportReason};
use std::collections::HashMap;

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
    /// The [`ChatLine::seq`] the next line will get.
    pub next_seq: u64,
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
    /// People to open a private chat with, asked for this frame. Drained by
    /// `systems::private_chat`, which owns the keys; left here so the panels
    /// that ask need no handle on it.
    pub private_start: Vec<String>,
    /// Lines typed into private channels this frame: (channel key, text).
    /// Drained by `systems::private_chat`, which seals and sends them — a
    /// private line must never go out as a `ChatSend`.
    pub private_outbox: Vec<(String, String)>,
    /// Private lines that could not be sent: (channel key, text). Put back in
    /// the chat's message box (`gui::chats`) rather than lost with the draft
    /// that was cleared when Send was pressed.
    pub unsent: Vec<(String, String)>,
    /// The "private chat with…" field in the panel.
    pub private_input: String,
    /// Points to seal and send this frame: (channel key, point). Drained by
    /// `systems::private_chat`, like [`Self::private_outbox`].
    pub private_points: Vec<(String, cartopolis_core::point::Point)>,
    /// Pictures to send into private chats. Drained by
    /// `systems::private_chat`, which hands them to `chat_pictures` to encrypt
    /// and upload before the line goes out.
    pub private_pictures: Vec<crate::systems::chat_pictures::OutgoingPicture>,
    /// A view waiting for the person to pick which private chat it goes to —
    /// set by "Send this view to a chat" and offered at the top of the panel.
    pub point_draft: Option<cartopolis_core::point::Point>,
    /// The optional line typed to go with [`Self::point_draft`].
    pub point_text: String,
    /// The private chat whose World view is on: its points are pinned on the
    /// map (`place_pins`). One at a time, so the map says which conversation
    /// it is showing.
    pub world: Option<String>,
    /// Other people's phones, shared with a private chat this account is in.
    /// Each is dropped at its `until_ms` whatever was last heard, so a phone
    /// that went quiet mid-share still disappears on time.
    pub locations: Vec<SharedLocation>,
    /// This phone's own shares, one per private chat, each with its end.
    pub sharing: Vec<OwnShare>,
    /// Chats to tell "stopped sharing" this frame. Drained by
    /// `systems::private_chat`.
    pub share_stops: Vec<String>,
    /// Group chats to start this frame: (name, member usernames). Drained by
    /// `systems::private_chat`.
    pub group_start: Vec<(String, Vec<String>)>,
    /// The "new group" fields in the panel: its name, and who is in it.
    pub group_name_input: String,
    pub group_members_input: String,
    /// Chats to send the camera's current view to, as a point. Asked for by
    /// the messenger's "+ → Send this view" and answered by `geo_nav`, which is
    /// what knows where the camera is.
    pub view_requests: Vec<String>,
    /// First messages to people who are not contacts yet: (username, text).
    /// Each is sent into the direct chat with that person the moment
    /// `systems::private_chat` has opened it (`systems::contacts`).
    pub private_hello: Vec<(String, String)>,
    /// Every direct private chat this device is in: lowercase username ->
    /// channel key. Kept by `systems::private_chat`, read by the contacts
    /// screens to find the chat that goes with a person or a request.
    pub direct: HashMap<String, String>,
    /// Private chats to drop from this device, by channel key: a declined
    /// request. Drained by `systems::private_chat`.
    pub private_forget: Vec<String>,
}

/// Somebody else's phone, as last shared with a chat.
#[derive(Clone, Debug, PartialEq)]
pub struct SharedLocation {
    pub channel: String,
    pub from: String,
    pub latitude: f64,
    pub longitude: f64,
    pub accuracy_m: f32,
    /// Unix milliseconds the share ends at.
    pub until_ms: u64,
}

/// This phone's position being shared with one chat, until `until_ms`.
#[derive(Clone, Debug, PartialEq)]
pub struct OwnShare {
    pub channel: String,
    pub until_ms: u64,
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
    /// The model asset the report is about, when it was raised from an object
    /// rather than from a chat line (protocol v22).
    ///
    /// The subject stays the person — an operator acts on accounts — and this
    /// says *which file*, so hiding it reaches every prim wearing it rather than
    /// the one the reporter was standing in front of.
    pub asset: Option<String>,
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
    /// A place that came with the line (private chats only for now): the log
    /// draws it as a button that puts the camera where the sender's was.
    pub point: Option<cartopolis_core::point::Point>,
    /// A picture that came with the line (private chats only): where it is
    /// stored and the key that opens it. `body` is then its caption.
    pub picture: Option<crate::systems::chat_pictures::PictureRef>,
    /// Wall-clock Unix milliseconds this line arrived on this device, for the
    /// time a bubble shows. Only for showing: everything that fades or orders
    /// runs on [`Self::arrived`].
    pub at_ms: u64,
    /// Arrival order across every channel, starting at 1 and never reused.
    ///
    /// [`Self::arrived`] cannot serve as a cursor: every line that lands on one
    /// frame shares it, and it only moves while the overlay runs. An agent
    /// reading the chat over MCP (`systems::mcp::agent`) asks for "everything
    /// after the last line I saw", which needs a number no two lines share.
    #[cfg_attr(not(mcp), allow(dead_code))]
    pub seq: u64,
}

#[cfg(mcp)]
impl ChatLine {
    /// Whether the name on this line is one the **server** vouches for.
    ///
    /// A line on a public channel with `sender_id == 0` is a scripted object
    /// speaking (`vfSay`), and its name is the object's name — which anybody who
    /// builds can set to anything, including somebody else's username. Private
    /// lines carry no avatar id but are checked against the account the server
    /// delivered them from (`private_chat::Engine::on_deliver`). So this is the
    /// test anything that *trusts* a sender name has to pass first.
    #[must_use]
    pub fn sender_is_account(&self, channel: &str) -> bool {
        self.sender_id != 0 || ChatState::is_private(channel)
    }
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
            next_seq: 1,
            my_id: None,
            blocked: Vec::new(),
            report: None,
            notice: None,
            private_start: Vec::new(),
            private_outbox: Vec::new(),
            unsent: Vec::new(),
            private_input: String::new(),
            private_points: Vec::new(),
            private_pictures: Vec::new(),
            point_draft: None,
            point_text: String::new(),
            world: None,
            locations: Vec::new(),
            sharing: Vec::new(),
            share_stops: Vec::new(),
            group_start: Vec::new(),
            group_name_input: String::new(),
            group_members_input: String::new(),
            view_requests: Vec::new(),
            private_hello: Vec::new(),
            direct: HashMap::new(),
            private_forget: Vec::new(),
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

    /// Make sure a channel exists under `label`. For channels whose name is not
    /// in their key — a private chat's key is its group id, and the label is who
    /// it is with (`systems::private_chat`).
    pub fn open_channel(&mut self, key: &str, label: &str) {
        let i = self.channel_index(key);
        self.channels[i].label = label.to_string();
    }

    /// Point the composer at a channel, e.g. the private chat just opened.
    pub fn focus_channel(&mut self, key: &str) {
        if let Some(i) = self.channels.iter().position(|c| c.key == key) {
            self.active = i;
        }
    }

    /// Whether a channel is a private, end-to-end encrypted chat. Its lines go
    /// through `systems::private_chat`, never `ChatSend`.
    #[must_use]
    pub fn is_private(key: &str) -> bool {
        key.starts_with("dm:")
    }

    /// Start (or extend) sharing this phone's position with a chat, for
    /// `duration_ms` from `now_ms`.
    pub fn start_sharing(&mut self, channel: &str, duration_ms: u64, now_ms: u64) {
        let until_ms = now_ms + duration_ms;
        match self.sharing.iter_mut().find(|s| s.channel == channel) {
            Some(s) => s.until_ms = until_ms,
            None => self.sharing.push(OwnShare {
                channel: channel.to_string(),
                until_ms,
            }),
        }
    }

    /// Stop sharing with a chat now, and tell it so.
    pub fn stop_sharing(&mut self, channel: &str) {
        let before = self.sharing.len();
        self.sharing.retain(|s| s.channel != channel);
        if self.sharing.len() != before {
            self.share_stops.push(channel.to_string());
        }
    }

    /// End every share whose time is up: this phone's own (queued to tell the
    /// chat) and everybody else's (dropped whatever was last heard).
    pub fn expire_shares(&mut self, now_ms: u64) {
        let (done, live): (Vec<OwnShare>, Vec<OwnShare>) = std::mem::take(&mut self.sharing)
            .into_iter()
            .partition(|s| s.until_ms <= now_ms);
        self.sharing = live;
        self.share_stops.extend(done.into_iter().map(|s| s.channel));
        self.locations.retain(|l| l.until_ms > now_ms);
    }

    /// Record somebody's shared position, replacing what they shared before.
    pub fn update_location(&mut self, update: SharedLocation, now_ms: u64) {
        self.locations
            .retain(|l| !(l.channel == update.channel && l.from == update.from));
        if update.until_ms > now_ms {
            self.locations.push(update);
        }
    }

    /// Somebody stopped sharing with a chat.
    pub fn drop_location(&mut self, channel: &str, from: &str) {
        self.locations
            .retain(|l| !(l.channel == channel && l.from == from));
    }

    /// The points of the chat whose World view is on, each with the label its
    /// pin shows: the chat laid out on the map. Empty when no World view is on.
    ///
    /// Numbered in the order they were sent, which is the number the same
    /// point's card carries in the thread (`chats::point_card`), so "2" on the
    /// map and "2" in the chat are one place.
    #[must_use]
    pub fn world_points(&self) -> Vec<(cartopolis_core::point::Point, String)> {
        let Some(key) = &self.world else {
            return Vec::new();
        };
        self.channels
            .iter()
            .filter(|c| &c.key == key)
            .flat_map(|c| c.lines.iter())
            .filter(|l| !self.is_blocked(&l.sender))
            .filter_map(|l| l.point.clone().map(|p| (p, l)))
            .enumerate()
            .map(|(i, (p, l))| {
                let name = p.name.clone().unwrap_or_else(|| l.body.clone());
                (p, format!("{}  {}", i + 1, name))
            })
            .collect()
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
        self.receive_point(channel, sender_id, sender, body, distance, None);
    }

    /// [`Self::receive`], for a line that may carry a place.
    pub fn receive_point(
        &mut self,
        channel: &str,
        sender_id: u64,
        sender: String,
        body: String,
        distance: Option<f32>,
        point: Option<cartopolis_core::point::Point>,
    ) {
        let arrived = self.clock;
        let seq = self.next_seq;
        self.next_seq += 1;
        let idx = self.channel_index(channel);
        let lines = &mut self.channels[idx].lines;
        lines.push(ChatLine {
            sender,
            sender_id,
            body,
            distance,
            arrived,
            point,
            picture: None,
            at_ms: web_time::SystemTime::now()
                .duration_since(web_time::UNIX_EPOCH)
                .map(|d| d.as_millis() as u64)
                .unwrap_or(0),
            seq,
        });
        if lines.len() > MAX_LINES {
            let excess = lines.len() - MAX_LINES;
            lines.drain(0..excess);
        }
    }

    /// [`Self::receive`], for a line that is a picture.
    pub fn receive_picture(
        &mut self,
        channel: &str,
        sender_id: u64,
        sender: String,
        caption: String,
        picture: crate::systems::chat_pictures::PictureRef,
    ) {
        self.receive_point(channel, sender_id, sender, caption, None, None);
        let idx = self.channel_index(channel);
        if let Some(line) = self.channels[idx].lines.last_mut() {
            line.picture = Some(picture);
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

/// What the person menu can ask for.
pub enum PersonAction {
    /// Open (or start) a private, end-to-end encrypted chat with them.
    Message,
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
        if ui.button("Message privately").clicked() {
            act(PersonAction::Message);
            ui.close();
        }
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
pub(crate) fn voice_row(ui: &mut egui::Ui, voice: &mut crate::systems::voice::VoiceState) {
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
pub(crate) fn render_report_dialog(
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
                asset: draft.asset.clone(),
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

    /// A share ends on time on both sides: this phone's own is queued to tell the
    /// chat, and somebody else's is dropped whatever was last heard.
    #[test]
    fn a_location_share_ends_on_time_on_both_sides() {
        let mut chat = ChatState::default();
        chat.start_sharing("dm:aa", 60_000, 1_000);
        chat.update_location(
            SharedLocation {
                channel: "dm:aa".into(),
                from: "bob".into(),
                latitude: 53.2,
                longitude: 6.5,
                accuracy_m: 10.0,
                until_ms: 30_000,
            },
            1_000,
        );
        chat.expire_shares(30_000);
        assert!(chat.locations.is_empty(), "bob's share ended at 30 s");
        assert_eq!(chat.sharing.len(), 1, "ours runs to 61 s");
        assert!(chat.share_stops.is_empty());
        chat.expire_shares(61_000);
        assert!(chat.sharing.is_empty());
        assert_eq!(
            chat.share_stops,
            vec!["dm:aa".to_string()],
            "the chat is told"
        );
    }

    #[test]
    fn stopping_tells_the_chat_and_a_newer_position_replaces_the_old() {
        let mut chat = ChatState::default();
        chat.start_sharing("dm:aa", 60_000, 0);
        chat.stop_sharing("dm:aa");
        chat.stop_sharing("dm:aa");
        assert_eq!(chat.share_stops, vec!["dm:aa".to_string()], "told once");
        let at = |lat: f64| SharedLocation {
            channel: "dm:aa".into(),
            from: "bob".into(),
            latitude: lat,
            longitude: 6.5,
            accuracy_m: 10.0,
            until_ms: 90_000,
        };
        chat.update_location(at(53.1), 0);
        chat.update_location(at(53.2), 0);
        assert_eq!(chat.locations, vec![at(53.2)]);
        chat.drop_location("dm:aa", "bob");
        assert!(chat.locations.is_empty());
    }

    /// The map shows one chat's points: the chosen one's, and none from any other.
    #[test]
    fn the_world_view_pins_only_the_chosen_chats_points() {
        let spot = |lat: f64| cartopolis_core::point::Point {
            latitude: lat,
            longitude: 6.57,
            name: None,
            text: None,
            view: None,
        };
        let mut chat = ChatState::default();
        chat.receive_point(
            "dm:aa",
            0,
            "bob".into(),
            "here".into(),
            None,
            Some(spot(53.1)),
        );
        chat.receive_point(
            "dm:bb",
            0,
            "eve".into(),
            "there".into(),
            None,
            Some(spot(52.0)),
        );
        chat.receive("dm:aa", 0, "bob".into(), "no place".into(), None);
        assert!(
            chat.world_points().is_empty(),
            "nothing is on the map by default"
        );
        chat.world = Some("dm:aa".into());
        let pins = chat.world_points();
        assert_eq!(pins.len(), 1);
        assert_eq!(pins[0].0.latitude, 53.1);
        // Numbered as the thread's card is, and named by the place (or, with no
        // name, by the line that sent it).
        assert_eq!(pins[0].1, "1  here");
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
