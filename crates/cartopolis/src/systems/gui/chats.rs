//! The messenger: a list of chats, and one chat at a time with two views of it
//! (docs/notes/chats-and-world.md; the design is `docs/design/chat-world/v5`
//! on the `design/chat-world-mockup` branch).
//!
//! # Three screens, one state
//!
//!  * **Chats | World** at the top of the map. *World* is the app as it always
//!    was; *Chats* is the list, which is the right-hand panel (the whole screen
//!    on a phone).
//!  * **A chat's Chat view**: its thread, as bubbles, with points drawn as
//!    numbered cards. Same panel, with the chat open.
//!  * **A chat's World view**: the map, with the chat's points as numbered pins
//!    (`place_pins`), its header at the top and its last lines and text field at
//!    the bottom. The panel is closed for this one: it *is* the map.
//!
//! Which of the three is showing is [`ChatsView::open`] plus the panel slot plus
//! [`ChatState::world`], and nothing else, so a chat is the same chat in both
//! views: same lines, same text field.
//!
//! # What stayed where it was
//!
//! Nearby chat is also the *Here* chat in the list, but the speech bubbles over
//! avatars and the floating composer on the map are untouched
//! (`chat_overlay`); they stand down only while a chat's World view has the
//! bottom of the screen. The chat state itself, the encryption and the network
//! are all unchanged — this module only draws them.

use bevy::prelude::*;
use cartopolis_core::{NetMessage, ReportReason};
use std::collections::HashMap;

use crate::resources::{OnlineSession, RightPanel};
use crate::systems::chat::{person_menu, ChatLine, ChatState, PersonAction, ReportDraft};
use crate::systems::egui_manager::EguiManager;
use crate::systems::{icons, theme};

/// The fill of your own bubbles: the cream of the palette, so they read as
/// yours at a glance and never as a button.
const MINE_FILL: egui::Color32 = egui::Color32::from_rgb(0xE9, 0xE5, 0xDE);
/// Colours for the round initials, picked by name so a person keeps theirs.
const AVATAR_TONES: [egui::Color32; 5] = [
    egui::Color32::from_rgb(0x6B, 0x4F, 0xA0),
    egui::Color32::from_rgb(0x3F, 0x7F, 0x6E),
    egui::Color32::from_rgb(0x4B, 0x6F, 0xA8),
    egui::Color32::from_rgb(0x9A, 0x5B, 0x3C),
    egui::Color32::from_rgb(0x72, 0x7A, 0x36),
];
/// Widest the World view's header and bottom sheet get on a desktop window.
const WORLD_SHEET_MAX_WIDTH: f32 = 520.0;
/// Lines of the chat shown over the map in its World view.
const WORLD_LINES: usize = 3;

#[derive(Resource, Default)]
pub struct ChatsView {
    /// The chat on screen, by channel key; `None` is the list.
    pub open: Option<String>,
    /// What is being typed into the open chat.
    pub draft: String,
    /// The list's "New chat" section is open.
    pub new_chat: bool,
    /// The open chat's "+" menu is open.
    pub attach: bool,
    /// How many of each chat's lines have been seen, for the unread counts.
    seen: HashMap<String, usize>,
    /// Put the caret in the open chat's field on the next frame it is drawn.
    claim_focus: bool,
    /// A scripted capture with no session behind it (`--ui chats`), the same
    /// switch `ChatOverlay::demo` is: it lets the screens draw offline.
    pub demo: bool,
}

impl ChatsView {
    /// Open a chat in its Chat view.
    pub fn open_chat(&mut self, key: &str) {
        if self.open.as_deref() != Some(key) {
            self.draft.clear();
        }
        self.open = Some(key.to_string());
        self.attach = false;
        self.claim_focus = false;
    }

    fn unread(&self, chat: &ChatState, key: &str, lines: &[ChatLine]) -> usize {
        let seen = self.seen.get(key).copied().unwrap_or(0).min(lines.len());
        lines[seen..].iter().filter(|l| !is_mine(chat, l)).count()
    }
}

/// What kind of chat a channel is, which decides what it says about itself.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum ChatKind {
    Private,
    Here,
    Everyone,
    Room,
}

#[must_use]
pub fn kind_of(key: &str) -> ChatKind {
    if ChatState::is_private(key) {
        ChatKind::Private
    } else if key == "local" {
        ChatKind::Here
    } else if key == "geo:" {
        ChatKind::Everyone
    } else {
        ChatKind::Room
    }
}

/// The line under a chat's name: who can read it.
#[must_use]
pub fn subtitle(kind: ChatKind) -> String {
    match kind {
        ChatKind::Private => format!("{} Private · end-to-end encrypted", icons::LOCK),
        ChatKind::Here => "Nearby · public, whoever is within earshot".into(),
        ChatKind::Everyone => "Everyone on this server · public".into(),
        ChatKind::Room => "Room · public".into(),
    }
}

/// The name a chat is shown under, without the padlock its label carries (the
/// padlock is in the subtitle here, where it can say what it means).
#[must_use]
pub fn display_name(key: &str, label: &str) -> String {
    match kind_of(key) {
        ChatKind::Private => crate::systems::private_chat::peer_of_label(label).to_string(),
        ChatKind::Here => "Here".into(),
        ChatKind::Everyone => "Everyone".into(),
        ChatKind::Room => format!("#{label}"),
    }
}

fn is_mine(chat: &ChatState, line: &ChatLine) -> bool {
    chat.my_id.is_some_and(|id| id != 0 && line.sender_id == id)
}

/// `18:04`, in this device's time zone.
#[must_use]
pub fn clock_time(at_ms: u64) -> String {
    jiff::Timestamp::from_millisecond(at_ms as i64)
        .map(|t| {
            t.to_zoned(jiff::tz::TimeZone::system())
                .strftime("%H:%M")
                .to_string()
        })
        .unwrap_or_default()
}

fn now_ms() -> u64 {
    web_time::SystemTime::now()
        .duration_since(web_time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

fn tone_for(name: &str) -> egui::Color32 {
    let h = name
        .bytes()
        .fold(0u32, |h, b| h.wrapping_mul(31).wrapping_add(u32::from(b)));
    AVATAR_TONES[h as usize % AVATAR_TONES.len()]
}

/// A round initial, the stand-in for a picture of the person or the group.
fn avatar(ui: &mut egui::Ui, name: &str, kind: ChatKind, size: f32) {
    let (rect, _) = ui.allocate_exact_size(egui::Vec2::splat(size), egui::Sense::hover());
    paint_avatar(ui.painter(), rect.center(), name, kind, size);
}

fn paint_avatar(p: &egui::Painter, c: egui::Pos2, name: &str, kind: ChatKind, size: f32) {
    let (tone, text, scale) = match kind {
        ChatKind::Here => (AVATAR_TONES[1], "here".to_string(), 0.26),
        ChatKind::Everyone => (AVATAR_TONES[2], icons::CAM_EARTH.to_string(), 0.45),
        ChatKind::Room => (AVATAR_TONES[4], "#".to_string(), 0.45),
        ChatKind::Private => (
            tone_for(name),
            name.chars().next().unwrap_or('?').to_uppercase().collect(),
            0.42,
        ),
    };
    p.circle_filled(c, size / 2.0, tone);
    p.text(
        c,
        egui::Align2::CENTER_CENTER,
        text,
        egui::FontId::proportional(size * scale),
        theme::CREAM,
    );
}

/// One line of text, cut to a width with an ellipsis.
fn paint_line(
    p: &egui::Painter,
    pos: egui::Pos2,
    text: &str,
    size: f32,
    color: egui::Color32,
    max_width: f32,
) {
    let mut job = egui::text::LayoutJob::simple_singleline(
        text.replace('\n', " "),
        egui::FontId::proportional(size),
        color,
    );
    job.wrap = egui::text::TextWrapping::truncate_at_width(max_width.max(10.0));
    let galley = p.layout_job(job);
    p.galley(pos, galley, color);
}

/// Two equal segments, the right one selected when `right_on`. Answers which
/// was tapped.
fn segmented(ui: &mut egui::Ui, left: &str, right: &str, right_on: bool) -> Option<bool> {
    let h = 34.0_f32.max(theme::row_height() * 0.9);
    let w = ((ui.available_width() - ui.spacing().item_spacing.x) / 2.0).max(40.0);
    let mut pick = None;
    ui.horizontal(|ui| {
        if theme::chip_sized(ui, left, !right_on, egui::vec2(w, h)).clicked() {
            pick = Some(false);
        }
        if theme::chip_sized(ui, right, right_on, egui::vec2(w, h)).clicked() {
            pick = Some(true);
        }
    });
    pick
}

/// A row of the chat list.
struct Row {
    key: String,
    name: String,
    kind: ChatKind,
    preview: String,
    time: String,
    unread: usize,
}

fn chat_row(ui: &mut egui::Ui, row: &Row, sending: bool) -> egui::Response {
    let h = 60.0;
    let w = ui.available_width();
    let (rect, resp) = ui.allocate_exact_size(egui::vec2(w, h), egui::Sense::click());
    let p = ui.painter();
    if resp.hovered() || resp.is_pointer_button_down_on() {
        p.rect_filled(
            rect,
            egui::CornerRadius::same(theme::RADIUS),
            theme::SURFACE_HOVER,
        );
    }
    let size = 42.0;
    paint_avatar(
        p,
        egui::pos2(rect.left() + 8.0 + size / 2.0, rect.center().y),
        &row.name,
        row.kind,
        size,
    );
    let x = rect.left() + 8.0 + size + 12.0;
    let right_reserve = 64.0;
    paint_line(
        p,
        egui::pos2(x, rect.top() + 11.0),
        &row.name,
        theme::SIZE_VALUE,
        theme::CREAM,
        rect.right() - x - right_reserve,
    );
    let (preview, tint) = if sending && row.kind == ChatKind::Private {
        ("Tap to send the view here".to_string(), theme::ACCENT_TEXT)
    } else {
        (row.preview.clone(), theme::TEXT_DIM)
    };
    paint_line(
        p,
        egui::pos2(x, rect.top() + 34.0),
        &preview,
        theme::SIZE_SUB,
        tint,
        rect.right() - x - 36.0,
    );
    if !row.time.is_empty() {
        p.text(
            egui::pos2(rect.right() - 10.0, rect.top() + 13.0),
            egui::Align2::RIGHT_TOP,
            &row.time,
            egui::FontId::proportional(theme::SIZE_TINY),
            theme::TEXT_DIM,
        );
    }
    if row.unread > 0 {
        let c = egui::pos2(rect.right() - 20.0, rect.top() + 41.0);
        p.circle_filled(c, 10.0, theme::ACCENT);
        p.text(
            c,
            egui::Align2::CENTER_CENTER,
            if row.unread > 99 {
                "99+".to_string()
            } else {
                row.unread.to_string()
            },
            egui::FontId::proportional(theme::SIZE_TINY),
            theme::INK,
        );
    }
    resp
}

/// Everything the panel decided while it was drawing, acted on afterwards
/// because the drawing closure holds the state borrowed.
#[derive(Default)]
struct Actions {
    to_world: bool,
    close: bool,
    open: Option<String>,
    back: bool,
    world_of: Option<String>,
    send: Option<String>,
    send_point_to: Option<String>,
    cancel_point: bool,
    view_to: Option<String>,
    go_to: Option<cartopolis_core::point::Point>,
    start_private: Option<String>,
    create_group: bool,
    join_room: bool,
    leave: Option<String>,
    share: Option<(String, u64)>,
    stop_share: Option<String>,
    show_location: Option<(f64, f64, String)>,
    block: Option<String>,
    unblock: Option<String>,
    report: Option<(String, Option<String>)>,
    message: Option<String>,
    dismiss_notice: bool,
}

/// The Chats panel: the list, or one chat's Chat view.
///
/// Present only when online (or seeded for a capture): there is nobody to talk
/// to otherwise, and the switch that opens it is hidden on the same test.
#[allow(clippy::too_many_arguments)]
pub fn render_chats_panel(
    mut egui: ResMut<EguiManager>,
    mut chat: ResMut<ChatState>,
    mut view: ResMut<ChatsView>,
    online: Option<Res<OnlineSession>>,
    mut right_panel: ResMut<RightPanel>,
    mut voice: ResMut<crate::systems::voice::VoiceState>,
    mut nav: ResMut<crate::systems::geo_nav::GeoNavState>,
) {
    // The report dialog is not part of the panel: it hangs off names in the
    // overlay and over avatars too, and must stay up with the panel shut.
    if !right_panel.is(RightPanel::Chat) || (online.is_none() && !view.demo) {
        if let Some(online) = online.as_deref() {
            crate::systems::chat::render_report_dialog(&mut egui, &mut chat, online);
        }
        return;
    }
    // Reading a chat is seeing its lines.
    if let Some(key) = view.open.clone() {
        if let Some(c) = chat.channels.iter().find(|c| c.key == key) {
            let n = c.lines.len();
            view.seen.insert(key, n);
        }
    }

    let mut act = Actions::default();
    let compact = egui.compact();
    let viewport_w = egui.viewport_pts().x;
    let mut open_flag = true;
    let now = now_ms();

    crate::systems::egui_manager::responsive_right_panel("chats_panel", compact, viewport_w, 360.0)
        .show_collapsible(egui.ui_mut(), &mut open_flag, |ui| {
            match view.open.clone() {
                None => list_screen(ui, &mut chat, &mut view, &mut voice, &mut act, now),
                Some(key) => chat_screen(ui, &mut chat, &mut view, &key, &mut act, now),
            }
        });
    if !open_flag {
        act.close = true;
    }

    apply(
        act,
        &mut chat,
        &mut view,
        online.as_deref(),
        &mut right_panel,
        &mut nav,
        now,
    );
    if let Some(online) = online.as_deref() {
        crate::systems::chat::render_report_dialog(&mut egui, &mut chat, online);
    }
}

fn list_screen(
    ui: &mut egui::Ui,
    chat: &mut ChatState,
    view: &mut ChatsView,
    voice: &mut crate::systems::voice::VoiceState,
    act: &mut Actions,
    now: u64,
) {
    if segmented(ui, "Chats", "World", false) == Some(true) {
        act.to_world = true;
    }
    ui.add_space(6.0);
    if let Some(notice) = chat.notice.clone() {
        if notice.error {
            act.dismiss_notice = theme::inline_error(ui, &notice.text);
        } else {
            ui.horizontal_wrapped(|ui| {
                theme::note(ui, &notice.text);
                if theme::action_button(ui, "OK").clicked() {
                    act.dismiss_notice = true;
                }
            });
        }
    }
    // A view waiting to be sent: every private chat row becomes its target.
    let sending = chat.point_draft.is_some();
    if let Some(draft) = chat.point_draft.clone() {
        theme::overlay_frame().show(ui, |ui| {
            ui.label(
                egui::RichText::new(format!(
                    "{} Send this view: {}",
                    icons::PIN,
                    draft.name.clone().unwrap_or_else(|| format!(
                        "{:.4}, {:.4}",
                        draft.latitude, draft.longitude
                    ))
                ))
                .color(theme::CREAM),
            );
            crate::systems::text_touch::field(
                ui,
                egui::TextEdit::singleline(&mut chat.point_text)
                    .id(egui::Id::new("chats_point_text"))
                    .desired_width(ui.available_width())
                    .hint_text("Add a message (optional)"),
            );
            ui.horizontal_wrapped(|ui| {
                theme::note(ui, "Tap a private chat below.");
                if theme::action_button(ui, "Cancel").clicked() {
                    act.cancel_point = true;
                }
            });
        });
        ui.add_space(6.0);
    }

    ui.horizontal(|ui| {
        ui.label(
            egui::RichText::new("Chats")
                .size(theme::SIZE_HEADING)
                .color(theme::CREAM),
        );
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            if theme::action_button(ui, if view.new_chat { "Done" } else { "+ New chat" }).clicked()
            {
                view.new_chat = !view.new_chat;
            }
        });
    });
    if view.new_chat {
        new_chat_section(ui, chat, act);
    }

    let rows = |filter: &dyn Fn(ChatKind) -> bool| -> Vec<Row> {
        let mut out: Vec<(u64, Row)> = chat
            .channels
            .iter()
            .filter(|c| filter(kind_of(&c.key)))
            .map(|c| {
                let visible: Vec<&ChatLine> = c
                    .lines
                    .iter()
                    .filter(|l| !chat.is_blocked(&l.sender))
                    .collect();
                let last = visible.last();
                let preview = match last {
                    None => match kind_of(&c.key) {
                        ChatKind::Here => "Whoever is near your avatar".into(),
                        ChatKind::Everyone => "Everyone on this server".into(),
                        _ => "No messages yet".into(),
                    },
                    Some(l) if is_mine(chat, l) => format!("You: {}", l.body),
                    Some(l)
                        if kind_of(&c.key) == ChatKind::Private && !c.label.contains(&l.sender) =>
                    {
                        format!("{}: {}", l.sender, l.body)
                    }
                    Some(l) if kind_of(&c.key) != ChatKind::Private => {
                        format!("{}: {}", l.sender, l.body)
                    }
                    Some(l) => l.body.clone(),
                };
                let at = last.map_or(0, |l| l.at_ms);
                let kind = kind_of(&c.key);
                (
                    at,
                    Row {
                        key: c.key.clone(),
                        name: display_name(&c.key, &c.label),
                        kind,
                        preview,
                        time: if at > 0 {
                            clock_time(at)
                        } else {
                            String::new()
                        },
                        unread: if kind == ChatKind::Here {
                            0
                        } else {
                            view.unread(chat, &c.key, &c.lines)
                        },
                    },
                )
            })
            .collect();
        // Newest conversation first, as every messenger orders it.
        out.sort_by(|a, b| b.0.cmp(&a.0));
        out.into_iter().map(|(_, r)| r).collect()
    };
    let private = rows(&|k| k == ChatKind::Private);
    let live = rows(&|k| k != ChatKind::Private);

    egui::ScrollArea::vertical()
        .auto_shrink([false, false])
        .show(ui, |ui| {
            theme::section(ui, &format!("{} Private", icons::LOCK));
            if private.is_empty() {
                theme::note(
                    ui,
                    "Start one with + New chat. Only the people in it can read it.",
                );
            }
            for row in &private {
                if chat_row(ui, row, sending).clicked() {
                    if sending {
                        act.send_point_to = Some(row.key.clone());
                    } else {
                        act.open = Some(row.key.clone());
                    }
                }
            }
            ui.add_space(8.0);
            theme::section(ui, "Nearby, live · public");
            for row in &live {
                if chat_row(ui, row, false).clicked() {
                    act.open = Some(row.key.clone());
                }
            }
            ui.add_space(8.0);
            egui::CollapsingHeader::new("Voice and blocked people")
                .id_salt("chats_settings")
                .show(ui, |ui| {
                    crate::systems::chat::voice_row(ui, voice);
                    if chat.blocked.is_empty() {
                        theme::note(ui, "You have not blocked anyone.");
                    }
                    for name in chat.blocked.clone() {
                        ui.horizontal(|ui| {
                            ui.label(egui::RichText::new(&name).color(theme::TEXT));
                            ui.with_layout(
                                egui::Layout::right_to_left(egui::Align::Center),
                                |ui| {
                                    if theme::action_button(ui, "Unblock").clicked() {
                                        act.unblock = Some(name.clone());
                                    }
                                },
                            );
                        });
                    }
                });
            let _ = now;
        });
}

fn new_chat_section(ui: &mut egui::Ui, chat: &mut ChatState, act: &mut Actions) {
    theme::overlay_frame().show(ui, |ui| {
        ui.label(egui::RichText::new("Message someone").color(theme::CREAM));
        ui.horizontal(|ui| {
            let field = crate::systems::text_touch::field(
                ui,
                egui::TextEdit::singleline(&mut chat.private_input)
                    .id(egui::Id::new("chats_new_private"))
                    .desired_width(ui.available_width() * 0.6)
                    .hint_text("Username"),
            );
            let entered = field.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter));
            let ready = !chat.private_input.trim().is_empty();
            if (theme::action_enabled_because(ui, ready, "Start", "Type who to write to first")
                .clicked()
                || entered)
                && ready
            {
                act.start_private = Some(chat.private_input.trim().to_string());
            }
        });
        ui.add_space(6.0);
        ui.label(egui::RichText::new("New group").color(theme::CREAM));
        crate::systems::text_touch::field(
            ui,
            egui::TextEdit::singleline(&mut chat.group_name_input)
                .id(egui::Id::new("chats_group_name"))
                .desired_width(ui.available_width())
                .hint_text("Group name"),
        );
        crate::systems::text_touch::field(
            ui,
            egui::TextEdit::singleline(&mut chat.group_members_input)
                .id(egui::Id::new("chats_group_members"))
                .desired_width(ui.available_width())
                .hint_text("Usernames, separated by commas"),
        );
        let ready =
            !chat.group_name_input.trim().is_empty() && !chat.group_members_input.trim().is_empty();
        if theme::action_enabled_because(
            ui,
            ready,
            "Create group",
            "Give it a name and at least one person",
        )
        .clicked()
            && ready
        {
            act.create_group = true;
        }
        ui.add_space(6.0);
        ui.label(egui::RichText::new("Join a public room").color(theme::CREAM));
        ui.horizontal(|ui| {
            let field = crate::systems::text_touch::field(
                ui,
                egui::TextEdit::singleline(&mut chat.join_input)
                    .id(egui::Id::new("chats_join_room"))
                    .desired_width(ui.available_width() * 0.6)
                    .hint_text("Room name"),
            );
            let entered = field.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter));
            let ready = !chat.join_input.trim().is_empty();
            if (theme::action_enabled_because(ui, ready, "Join", "Type a room name first")
                .clicked()
                || entered)
                && ready
            {
                act.join_room = true;
            }
        });
        theme::note(
            ui,
            "Private chats and groups are end-to-end encrypted: the server passes them on \
             without being able to open them. Rooms, Here and Everyone are public.",
        );
    });
    ui.add_space(6.0);
}

/// The header of an open chat: back, who, and (for a private chat) its menu.
fn chat_header(
    ui: &mut egui::Ui,
    chat: &ChatState,
    key: &str,
    name: &str,
    kind: ChatKind,
    act: &mut Actions,
    now: u64,
) {
    ui.horizontal(|ui| {
        if ui
            .add(egui::Button::new(
                egui::RichText::new(icons::BACK).size(theme::SIZE_TITLE),
            ))
            .on_hover_text("All chats")
            .clicked()
        {
            act.back = true;
        }
        avatar(ui, name, kind, 36.0);
        ui.vertical(|ui| {
            ui.label(
                egui::RichText::new(name)
                    .size(theme::SIZE_VALUE)
                    .color(theme::CREAM),
            );
            // Somebody sharing where their phone is says so under the name,
            // in the blue that means only that.
            let theirs: Vec<_> = chat.locations.iter().filter(|l| l.channel == key).collect();
            if let Some(l) = theirs.first() {
                ui.label(
                    egui::RichText::new(format!(
                        "{} {} shares their location · {} min left",
                        icons::HERE,
                        l.from,
                        l.until_ms.saturating_sub(now) / 60_000 + 1
                    ))
                    .size(theme::SIZE_TINY)
                    .color(theme::REAL_LOCATION),
                );
            } else {
                ui.label(
                    egui::RichText::new(subtitle(kind))
                        .size(theme::SIZE_TINY)
                        .color(theme::TEXT_DIM),
                );
            }
        });
        if kind == ChatKind::Room {
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if theme::action_button(ui, "Leave").clicked() {
                    act.leave = Some(key.to_string());
                }
            });
        }
    });
}

/// One chat's thread.
fn chat_screen(
    ui: &mut egui::Ui,
    chat: &mut ChatState,
    view: &mut ChatsView,
    key: &str,
    act: &mut Actions,
    now: u64,
) {
    let Some(channel) = chat.channels.iter().find(|c| c.key == key) else {
        act.back = true;
        return;
    };
    let kind = kind_of(key);
    let name = display_name(key, &channel.label);
    let lines: Vec<ChatLine> = channel
        .lines
        .iter()
        .filter(|l| !chat.is_blocked(&l.sender))
        .cloned()
        .collect();
    chat_header(ui, chat, key, &name, kind, act, now);
    ui.add_space(4.0);
    if segmented(
        ui,
        &format!("{} Chat", icons::CHAT),
        &format!("{} World", icons::CAM_EARTH),
        false,
    ) == Some(true)
    {
        act.world_of = Some(key.to_string());
    }
    ui.add_space(4.0);

    // The text field sits in a bottom band of its own, so the thread above it
    // can take every point of height that is left and scroll.
    egui::Panel::bottom(egui::Id::new("chats_composer_band"))
        .frame(egui::Frame::NONE)
        .show_separator_line(false)
        .show(ui, |ui| {
            ui.add_space(6.0);
            composer(ui, chat, view, key, kind, act, now);
            ui.add_space(4.0);
        });

    let group = kind != ChatKind::Private || !channel_is_direct(chat, key);
    egui::ScrollArea::vertical()
        .stick_to_bottom(true)
        .auto_shrink([false, false])
        .show(ui, |ui| {
            if lines.is_empty() {
                theme::empty_state(
                    ui,
                    "Nothing here yet",
                    match kind {
                        ChatKind::Private => "Say hello, or send a place with + → Send this view.",
                        ChatKind::Here => {
                            "What people near your avatar say appears here and over their heads."
                        }
                        _ => "Messages appear here.",
                    },
                );
            }
            let mut number = 0usize;
            for line in &lines {
                let n = line.point.as_ref().map(|_| {
                    number += 1;
                    number
                });
                bubble(ui, chat, line, group, n, act);
                ui.add_space(4.0);
            }
        });
}

/// A private chat is a direct one when its label is a person rather than a
/// group's name — which the chat itself does not record, so this asks whether
/// anybody but that person has spoken in it.
fn channel_is_direct(chat: &ChatState, key: &str) -> bool {
    chat.channels.iter().find(|c| c.key == key).is_none_or(|c| {
        let peer = crate::systems::private_chat::peer_of_label(&c.label);
        c.lines.iter().all(|l| {
            is_mine(chat, l) || l.sender == peer || l.sender_id == 0 && l.sender.is_empty()
        })
    })
}

fn bubble(
    ui: &mut egui::Ui,
    chat: &ChatState,
    line: &ChatLine,
    group: bool,
    number: Option<usize>,
    act: &mut Actions,
) {
    let mine = is_mine(chat, line);
    let max_w = (ui.available_width() * 0.8).max(120.0);
    let layout = if mine {
        egui::Layout::right_to_left(egui::Align::TOP)
    } else {
        egui::Layout::left_to_right(egui::Align::TOP)
    };
    let (fill, ink, dim) = if mine {
        (
            MINE_FILL,
            theme::INK,
            egui::Color32::from_rgb(0x5A, 0x60, 0x66),
        )
    } else {
        (theme::SURFACE_RAISED, theme::TEXT, theme::TEXT_DIM)
    };
    ui.with_layout(layout, |ui| {
        egui::Frame::NONE
            .fill(fill)
            .corner_radius(egui::CornerRadius::same(theme::RADIUS_LG))
            .inner_margin(egui::Margin::symmetric(10, 7))
            .show(ui, |ui| {
                ui.set_max_width(max_w);
                ui.with_layout(egui::Layout::top_down(egui::Align::LEFT), |ui| {
                    if group && !mine {
                        let who = match line.distance {
                            Some(d) => format!("{} · {:.0} m", line.sender, d),
                            None => line.sender.clone(),
                        };
                        let name = ui.add(
                            egui::Label::new(
                                egui::RichText::new(who)
                                    .size(theme::SIZE_SUB)
                                    .strong()
                                    .color(theme::ACCENT_TEXT),
                            )
                            .sense(egui::Sense::click()),
                        );
                        person_menu(&name, &line.sender, Some(&line.body), |a| match a {
                            PersonAction::Message => act.message = Some(line.sender.clone()),
                            PersonAction::Block => act.block = Some(line.sender.clone()),
                            PersonAction::Report(msg) => {
                                act.report = Some((line.sender.clone(), msg));
                            }
                        });
                    }
                    match (&line.point, number) {
                        (Some(point), Some(n)) => {
                            if point_card(ui, point, n, &line.body, max_w, ink, dim).clicked() {
                                act.go_to = Some(point.clone());
                            }
                        }
                        _ => {
                            ui.add(
                                egui::Label::new(
                                    egui::RichText::new(&line.body)
                                        .size(theme::SIZE_BODY)
                                        .color(ink),
                                )
                                .wrap(),
                            );
                        }
                    }
                    if line.at_ms > 0 {
                        ui.with_layout(egui::Layout::right_to_left(egui::Align::Min), |ui| {
                            ui.label(
                                egui::RichText::new(clock_time(line.at_ms))
                                    .size(theme::SIZE_TINY)
                                    .color(dim),
                            );
                        });
                    }
                });
            });
    });
}

/// A point as the thread shows it: a numbered card that is also a button. The
/// number is the one its pin carries on the World view.
///
/// **The picture is a placeholder.** The design sends a picture of what the
/// sender saw; a point carries the camera but no image yet, so the card shows
/// where the picture will go rather than inventing one.
fn point_card(
    ui: &mut egui::Ui,
    point: &cartopolis_core::point::Point,
    number: usize,
    caption: &str,
    max_w: f32,
    ink: egui::Color32,
    dim: egui::Color32,
) -> egui::Response {
    let w = max_w.min(260.0);
    let (rect, resp) = ui.allocate_exact_size(egui::vec2(w, w * 0.42), egui::Sense::click());
    let p = ui.painter();
    p.rect_filled(
        rect,
        egui::CornerRadius::same(theme::RADIUS),
        egui::Color32::from_rgb(0x39, 0x4A, 0x55),
    );
    // A few faint streets, so the placeholder reads as "a place" not "an error".
    for (a, b) in [(0.1, 0.8), (0.35, 0.2), (0.7, 0.95)] {
        p.line_segment(
            [
                egui::pos2(rect.left(), rect.top() + rect.height() * a),
                egui::pos2(rect.right(), rect.top() + rect.height() * b),
            ],
            egui::Stroke::new(3.0, egui::Color32::from_rgb(0x4A, 0x5C, 0x68)),
        );
    }
    p.text(
        rect.center(),
        egui::Align2::CENTER_CENTER,
        icons::PIN,
        egui::FontId::proportional(28.0),
        theme::BRAND,
    );
    let badge = egui::pos2(rect.left() + 16.0, rect.top() + 16.0);
    p.circle_filled(badge, 11.0, theme::CREAM);
    p.text(
        badge,
        egui::Align2::CENTER_CENTER,
        number.to_string(),
        egui::FontId::proportional(theme::SIZE_SUB),
        theme::INK,
    );
    if resp.hovered() {
        p.rect_stroke(
            rect,
            egui::CornerRadius::same(theme::RADIUS),
            egui::Stroke::new(1.5, theme::ACCENT),
            egui::StrokeKind::Inside,
        );
    }
    let title = point
        .name
        .clone()
        .unwrap_or_else(|| format!("{:.4}, {:.4}", point.latitude, point.longitude));
    ui.add_space(4.0);
    ui.label(
        egui::RichText::new(title)
            .size(theme::SIZE_BODY)
            .strong()
            .color(ink),
    );
    // The caption is the text when there is one and the name otherwise
    // (`private_chat::point_caption`), so it is only worth a second line when
    // it is not the title just drawn.
    if point.name.is_some() && !caption.is_empty() && point.name.as_deref() != Some(caption) {
        ui.label(
            egui::RichText::new(caption)
                .size(theme::SIZE_SUB)
                .color(dim),
        );
    }
    resp.on_hover_text("Look from where it was sent")
}

/// The open chat's text field, its "+" menu and its location sharing.
fn composer(
    ui: &mut egui::Ui,
    chat: &ChatState,
    view: &mut ChatsView,
    key: &str,
    kind: ChatKind,
    act: &mut Actions,
    now: u64,
) {
    if kind == ChatKind::Private {
        if let Some(share) = chat.sharing.iter().find(|s| s.channel == key) {
            ui.horizontal_wrapped(|ui| {
                ui.label(
                    egui::RichText::new(format!(
                        "{} You share your real location here · {} min left",
                        icons::HERE,
                        share.until_ms.saturating_sub(now) / 60_000 + 1
                    ))
                    .color(theme::REAL_LOCATION),
                );
                if theme::action_button(ui, "Stop").clicked() {
                    act.stop_share = Some(key.to_string());
                }
            });
        }
        for l in chat.locations.iter().filter(|l| l.channel == key) {
            ui.horizontal_wrapped(|ui| {
                ui.label(
                    egui::RichText::new(format!(
                        "{} {} shares their location",
                        icons::HERE,
                        l.from
                    ))
                    .color(theme::REAL_LOCATION),
                );
                if theme::action_button(ui, "Show").clicked() {
                    act.show_location = Some((l.latitude, l.longitude, l.from.clone()));
                }
            });
        }
    }
    if view.attach {
        theme::overlay_frame().show(ui, |ui| {
            ui.set_width(ui.available_width());
            if kind == ChatKind::Private {
                if ui
                    .button(format!("{} Send this view", icons::PIN))
                    .on_hover_text("Send where the camera is looking, as a place people can tap")
                    .clicked()
                {
                    act.view_to = Some(key.to_string());
                }
                if !chat.sharing.iter().any(|s| s.channel == key) {
                    ui.horizontal_wrapped(|ui| {
                        ui.label(
                            egui::RichText::new(format!("{} Share my location for", icons::HERE))
                                .color(theme::REAL_LOCATION),
                        );
                        for (label, minutes) in
                            [("15 min", 15u64), ("1 hour", 60), ("8 hours", 480)]
                        {
                            if theme::action_button(ui, label).clicked() {
                                act.share = Some((key.to_string(), minutes * 60_000));
                            }
                        }
                    });
                    theme::note(ui, "Only this chat sees it, and it stops by itself.");
                }
            } else {
                theme::note(
                    ui,
                    "Places and locations go in private chats only: this one is public.",
                );
            }
        });
    }
    compose_row(ui, view, key, "chats_message_field", act);
}

/// "+", the field and send, shared by the Chat view and the World view.
fn compose_row(ui: &mut egui::Ui, view: &mut ChatsView, key: &str, id: &str, act: &mut Actions) {
    let claim = std::mem::take(&mut view.claim_focus);
    ui.horizontal(|ui| {
        if theme::chip(ui, "+", view.attach)
            .on_hover_text("Send a place, or share where you are")
            .clicked()
        {
            view.attach = !view.attach;
        }
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            let ready = !view.draft.trim().is_empty();
            let send =
                theme::action_enabled_because(ui, ready, icons::FORWARD, "Type a message first");
            let field = crate::systems::text_touch::field(
                ui,
                egui::TextEdit::singleline(&mut view.draft)
                    .id(egui::Id::new(id))
                    .desired_width(ui.available_width())
                    .hint_text("Message"),
            );
            if claim {
                field.request_focus();
            }
            let entered = field.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter));
            if (send.clicked() || entered) && ready {
                act.send = Some(key.to_string());
            }
        });
    });
}

#[allow(clippy::too_many_arguments)]
fn apply(
    act: Actions,
    chat: &mut ChatState,
    view: &mut ChatsView,
    online: Option<&OnlineSession>,
    right_panel: &mut RightPanel,
    nav: &mut crate::systems::geo_nav::GeoNavState,
    now: u64,
) {
    if act.dismiss_notice {
        chat.notice = None;
    }
    if act.to_world || act.close {
        *right_panel = RightPanel::None;
        chat.world = None;
    }
    if let Some(key) = act.open {
        view.open_chat(&key);
        chat.focus_channel(&key);
    }
    if act.back {
        view.open = None;
        view.attach = false;
        chat.world = None;
    }
    if let Some(key) = act.world_of {
        chat.world = Some(key);
        *right_panel = RightPanel::None;
    }
    if let Some(key) = act.send {
        let body = view.draft.trim().to_string();
        send_line(chat, online, view.demo, &key, body);
        view.draft.clear();
        view.claim_focus = true;
    }
    if let Some(key) = act.send_point_to {
        if let Some(mut point) = chat.point_draft.take() {
            let text = chat.point_text.trim().to_string();
            point.text = (!text.is_empty()).then_some(text);
            chat.private_points.push((key.clone(), point));
            chat.point_text.clear();
            view.open_chat(&key);
            chat.focus_channel(&key);
        }
    }
    if act.cancel_point {
        chat.point_draft = None;
        chat.point_text.clear();
    }
    if let Some(key) = act.view_to {
        chat.view_requests.push(key);
        view.attach = false;
    }
    if let Some(point) = act.go_to {
        // A point opens where the sender stood, on the chat's World view.
        nav.pending_label = point.name.clone();
        nav.pending_teleport = Some(crate::systems::geo_nav::Teleport::point(&point));
        if let Some(key) = view.open.clone() {
            chat.world = Some(key);
        }
        *right_panel = RightPanel::None;
    }
    if let Some(name) = act.start_private.or(act.message) {
        chat.private_start.push(name);
        chat.private_input.clear();
        view.new_chat = false;
    }
    if act.create_group {
        let name = chat.group_name_input.trim().to_string();
        let members: Vec<String> = chat
            .group_members_input
            .split(',')
            .map(|m| m.trim().to_string())
            .filter(|m| !m.is_empty())
            .collect();
        chat.group_start.push((name, members));
        chat.group_name_input.clear();
        chat.group_members_input.clear();
        view.new_chat = false;
    }
    if act.join_room {
        let room = chat.join_input.trim().to_string();
        let key = format!("room:{room}");
        if let Some(online) = online {
            online.send(
                "chat subscribe",
                NetMessage::ChatSubscribe {
                    room: room.clone(),
                    join: true,
                },
            );
        }
        chat.open_channel(&key, &room);
        chat.join_input.clear();
        view.new_chat = false;
        view.open_chat(&key);
        chat.focus_channel(&key);
    }
    if let Some(key) = act.leave {
        let room = key.strip_prefix("room:").unwrap_or(&key).to_string();
        if let Some(online) = online {
            online.send(
                "chat unsubscribe",
                NetMessage::ChatSubscribe { room, join: false },
            );
        }
        chat.drop_channel(&key);
        view.open = None;
    }
    if let Some((key, duration)) = act.share {
        chat.start_sharing(&key, duration, now);
        view.attach = false;
    }
    if let Some(key) = act.stop_share {
        chat.stop_sharing(&key);
    }
    if let Some((lat, lng, who)) = act.show_location {
        nav.pending_label = Some(who);
        nav.pending_teleport = Some(crate::systems::geo_nav::Teleport::geo(lat, lng, None));
        *right_panel = RightPanel::None;
    }
    if let Some(online) = online {
        for (name, on) in [(act.block, true), (act.unblock, false)] {
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
    }
    if let Some((username, message)) = act.report {
        chat.report = Some(ReportDraft {
            username,
            message,
            reason: ReportReason::Harassment,
            note: String::new(),
            asset: None,
        });
    }
}

/// Send one line to a chat by the path its kind needs. A private line goes to
/// `private_chat` to be sealed and must never reach `ChatSend`, where the
/// server would read and store it.
fn send_line(
    chat: &mut ChatState,
    online: Option<&OnlineSession>,
    demo: bool,
    key: &str,
    body: String,
) {
    if body.is_empty() {
        return;
    }
    if ChatState::is_private(key) {
        if demo && online.is_none() {
            let me = chat.my_id.unwrap_or(1);
            chat.receive(key, me, "You".into(), body, None);
        } else {
            chat.private_outbox.push((key.to_string(), body));
        }
        return;
    }
    match online {
        Some(online) => {
            online.send(
                "chat send",
                NetMessage::ChatSend {
                    request_id: 0,
                    scope: crate::systems::chat::scope_for(key, false),
                    body,
                },
            );
        }
        None if demo => {
            let me = chat.my_id.unwrap_or(1);
            chat.receive(key, me, "You".into(), body, None);
        }
        None => {}
    }
}

/// How far down the top of the screen this module's overlays start: below the
/// "you are sharing your location" notice while one is up, which
/// `place_pins::sharing_chip` draws at the top centre and which must never be
/// covered.
fn top_offset(chat: &ChatState) -> f32 {
    if chat.sharing.is_empty() {
        theme::OSD_MARGIN
    } else {
        theme::OSD_MARGIN + 44.0
    }
}

/// The switch at the top of the map: back into the chats.
///
/// Not drawn while the Chats panel is up (its own switch is at its top), while
/// a chat's World view has the top of the screen, or while driving.
#[allow(clippy::too_many_arguments)]
pub fn render_chats_switch(
    mut egui: ResMut<EguiManager>,
    chat: Res<ChatState>,
    mut view: ResMut<ChatsView>,
    online: Option<Res<OnlineSession>>,
    mut right_panel: ResMut<RightPanel>,
    driver: Res<crate::systems::driver::DriverState>,
) {
    if (online.is_none() && !view.demo)
        || driver.active
        || right_panel.is(RightPanel::Chat)
        || chat.world.is_some()
        || (egui.compact() && !right_panel.is(RightPanel::None))
    {
        return;
    }
    let unread: usize = chat
        .channels
        .iter()
        .filter(|c| kind_of(&c.key) != ChatKind::Here)
        .map(|c| view.unread(&chat, &c.key, &c.lines))
        .sum();
    let ctx = egui.ctx_mut().clone();
    let mut to_chats = false;
    egui::Area::new(egui::Id::new("chats_world_switch"))
        .order(egui::Order::Middle)
        .anchor(egui::Align2::CENTER_TOP, egui::vec2(0.0, top_offset(&chat)))
        .show(&ctx, |ui| {
            theme::overlay_frame().show(ui, |ui| {
                ui.set_width(220.0);
                let chats = if unread > 0 {
                    format!("{} Chats · {unread}", icons::CHAT)
                } else {
                    format!("{} Chats", icons::CHAT)
                };
                if segmented(ui, &chats, &format!("{} World", icons::CAM_EARTH), true)
                    == Some(false)
                {
                    to_chats = true;
                }
            });
        });
    if to_chats {
        *right_panel = RightPanel::Chat;
        view.open = None;
    }
}

/// A chat's World view: its header over the map, and its last lines and text
/// field at the bottom. The chat's points are pinned by `place_pins`.
#[allow(clippy::too_many_arguments)]
pub fn render_chat_world(
    mut egui: ResMut<EguiManager>,
    mut chat: ResMut<ChatState>,
    mut view: ResMut<ChatsView>,
    online: Option<Res<OnlineSession>>,
    mut right_panel: ResMut<RightPanel>,
    driver: Res<crate::systems::driver::DriverState>,
    mut nav: ResMut<crate::systems::geo_nav::GeoNavState>,
) {
    let Some(key) = chat.world.clone() else {
        return;
    };
    if (online.is_none() && !view.demo) || driver.active || !right_panel.is(RightPanel::None) {
        return;
    }
    let Some(channel) = chat.channels.iter().find(|c| c.key == key) else {
        chat.world = None;
        return;
    };
    let kind = kind_of(&key);
    let name = display_name(&key, &channel.label);
    let recent: Vec<ChatLine> = channel
        .lines
        .iter()
        .filter(|l| !chat.is_blocked(&l.sender))
        .rev()
        .take(WORLD_LINES)
        .cloned()
        .collect::<Vec<_>>()
        .into_iter()
        .rev()
        .collect();
    let n = channel.lines.len();
    view.seen.insert(key.clone(), n);
    if view.open.as_deref() != Some(key.as_str()) {
        view.open_chat(&key);
    }

    let now = now_ms();
    let viewport = egui.viewport_pts();
    let width = (viewport.x - 2.0 * theme::OSD_MARGIN).min(WORLD_SHEET_MAX_WIDTH);
    let bottom = crate::systems::chat_overlay::stack_bottoms(
        true,
        crate::systems::chat_overlay::keyboard_reserve(&egui, true),
        0.0,
    )
    .0;
    // The header stands clear of the column of map buttons down the left edge
    // (the dock's layers disc, the compass), which are drawn over anything in
    // their way, and below the location-sharing notice when that is up.
    let header_width =
        (viewport.x - 2.0 * theme::OSD_MARGIN - theme::ROUND_BUTTON_SIZE - theme::OSD_GAP)
            .min(WORLD_SHEET_MAX_WIDTH);
    let top = top_offset(&chat);
    let ctx = egui.ctx_mut().clone();
    let mut act = Actions::default();

    egui::Area::new(egui::Id::new("chat_world_header"))
        .order(egui::Order::Middle)
        .anchor(egui::Align2::RIGHT_TOP, egui::vec2(-theme::OSD_MARGIN, top))
        .show(&ctx, |ui| {
            ui.set_width(header_width);
            theme::overlay_frame().show(ui, |ui| {
                ui.set_width(header_width - 24.0);
                chat_header(ui, &chat, &key, &name, kind, &mut act, now);
                ui.add_space(4.0);
                if segmented(
                    ui,
                    &format!("{} Chat", icons::CHAT),
                    &format!("{} World", icons::CAM_EARTH),
                    true,
                ) == Some(false)
                {
                    act.to_world = false;
                    act.open = Some(key.clone());
                }
            });
        });

    egui::Area::new(egui::Id::new("chat_world_sheet"))
        .order(egui::Order::Foreground)
        .anchor(egui::Align2::CENTER_BOTTOM, egui::vec2(0.0, -bottom))
        .show(&ctx, |ui| {
            ui.set_width(width);
            theme::overlay_frame().show(ui, |ui| {
                ui.set_width(width - 24.0);
                for line in &recent {
                    let who = if is_mine(&chat, line) {
                        "You".to_string()
                    } else {
                        line.sender.clone()
                    };
                    ui.horizontal_wrapped(|ui| {
                        ui.label(
                            egui::RichText::new(who)
                                .size(theme::SIZE_SUB)
                                .color(theme::ACCENT_TEXT),
                        );
                        ui.label(
                            egui::RichText::new(&line.body)
                                .size(theme::SIZE_SUB)
                                .color(theme::TEXT),
                        );
                    });
                }
                if !recent.is_empty() {
                    ui.add_space(4.0);
                }
                if view.attach {
                    if kind == ChatKind::Private {
                        if ui
                            .button(format!("{} Send this view", icons::PIN))
                            .clicked()
                        {
                            act.view_to = Some(key.clone());
                        }
                    } else {
                        theme::note(ui, "Places go in private chats only: this one is public.");
                    }
                }
                compose_row(ui, &mut view, &key, "chat_world_field", &mut act);
            });
        });

    // "Chat" in the header goes back to the thread, which is the panel.
    if act.open.is_some() {
        chat.world = None;
        *right_panel = RightPanel::Chat;
    }
    if act.back {
        chat.world = None;
        view.open = None;
        *right_panel = RightPanel::Chat;
        act.back = false;
    }
    apply(
        act,
        &mut chat,
        &mut view,
        online.as_deref(),
        &mut right_panel,
        &mut nav,
        now,
    );
}

/// Seed the messenger for a scripted capture: three private chats, one with two
/// points in it, and the nearby chat. `screen` is `list`, `chat` or `world`.
///
/// For the reason `chat_overlay::seed_demo_chat` exists: a scripted run has no
/// server, and a screen nobody can photograph is a screen nobody reviews.
pub fn seed_demo_chats(chat: &mut ChatState, view: &mut ChatsView, screen: &str) -> RightPanel {
    const ME: u64 = 1;
    chat.my_id = Some(ME);
    let sanne = "dm:5a4e";
    if !chat.channels.iter().any(|c| c.key == sanne) {
        let at = |min: u64| 1_790_000_000_000 + min * 60_000;
        chat.open_channel(sanne, &crate::systems::private_chat::private_label("Sanne"));
        let push = |chat: &mut ChatState,
                    key: &str,
                    id: u64,
                    who: &str,
                    body: &str,
                    point: Option<cartopolis_core::point::Point>,
                    min: u64| {
            chat.receive_point(key, id, who.into(), body.into(), None, point);
            if let Some(c) = chat.channels.iter_mut().find(|c| c.key == key) {
                if let Some(l) = c.lines.last_mut() {
                    l.at_ms = at(min);
                }
            }
        };
        let point = |lat: f64, lng: f64, name: &str, text: &str| cartopolis_core::point::Point {
            latitude: lat,
            longitude: lng,
            name: Some(name.into()),
            text: Some(text.into()),
            view: Some(cartopolis_core::point::PointView {
                altitude: 180.0,
                pitch: -35.0,
                yaw: 20.0,
            }),
        };
        push(
            chat,
            sanne,
            0,
            "Sanne",
            "Where shall we have dinner Saturday?",
            None,
            0,
        );
        push(
            chat,
            sanne,
            ME,
            "You",
            "then the show after?",
            Some(point(
                53.2167,
                6.5608,
                "Stadsschouwburg",
                "then the show after?",
            )),
            2,
        );
        push(
            chat,
            sanne,
            0,
            "Sanne",
            "this café next door, 7 pm?",
            Some(point(
                53.2152,
                6.5660,
                "Turfsingel 84",
                "this café next door, 7 pm?",
            )),
            4,
        );
        push(chat, sanne, ME, "You", "Perfect, see you there", None, 6);
        let joris = "dm:10a5";
        chat.open_channel(joris, &crate::systems::private_chat::private_label("Joris"));
        push(
            chat,
            joris,
            0,
            "Joris",
            "Did you see the new bike bridge?",
            None,
            1,
        );
        let family = "dm:fa01";
        chat.open_channel(
            family,
            &crate::systems::private_chat::private_label("Family"),
        );
        push(chat, family, 0, "Oma", "Sunday lunch at ours?", None, 3);
        push(
            chat,
            "local",
            2,
            "Wieke",
            "Are you coming down to the Vismarkt?",
            None,
            5,
        );
        view.seen.insert(sanne.into(), 2);
    }
    view.demo = true;
    match screen {
        "chat" => {
            view.open_chat(sanne);
            chat.world = None;
            RightPanel::Chat
        }
        "switch" => {
            view.open = None;
            chat.world = None;
            RightPanel::None
        }
        "world" => {
            view.open_chat(sanne);
            chat.world = Some(sanne.into());
            RightPanel::None
        }
        _ => {
            view.open = None;
            chat.world = None;
            RightPanel::Chat
        }
    }
}

/// Undo [`seed_demo_chats`] for a capture that did not ask for it.
pub fn clear_demo_chats(chat: &mut ChatState, view: &mut ChatsView) {
    if view.demo {
        view.demo = false;
        view.open = None;
        chat.world = None;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_chat_says_who_can_read_it() {
        assert!(subtitle(kind_of("dm:ab")).contains("end-to-end"));
        assert!(subtitle(kind_of("local")).contains("public"));
        assert!(subtitle(kind_of("geo:")).contains("public"));
        assert!(subtitle(kind_of("room:x")).contains("public"));
        assert_eq!(
            display_name(
                "dm:ab",
                &crate::systems::private_chat::private_label("Sanne")
            ),
            "Sanne"
        );
        assert_eq!(display_name("room:fietsers", "fietsers"), "#fietsers");
        assert_eq!(display_name("local", "Local"), "Here");
    }

    /// Unread counts only other people's lines, and only those after the chat
    /// was last looked at.
    #[test]
    fn your_own_lines_are_never_unread() {
        let mut chat = ChatState::default();
        chat.my_id = Some(1);
        chat.receive("dm:ab", 0, "Sanne".into(), "hi".into(), None);
        chat.receive("dm:ab", 1, "me".into(), "hello".into(), None);
        chat.receive("dm:ab", 0, "Sanne".into(), "dinner?".into(), None);
        let mut view = ChatsView::default();
        let lines = chat
            .channels
            .iter()
            .find(|c| c.key == "dm:ab")
            .unwrap()
            .lines
            .clone();
        assert_eq!(view.unread(&chat, "dm:ab", &lines), 2);
        view.seen.insert("dm:ab".into(), 2);
        assert_eq!(view.unread(&chat, "dm:ab", &lines), 1);
    }

    /// A private line is sealed by `private_chat`; it must never leave as a
    /// plain `ChatSend`.
    #[test]
    fn a_private_line_goes_to_the_sealing_queue() {
        let mut chat = ChatState::default();
        send_line(&mut chat, None, false, "dm:ab", "secret".into());
        assert_eq!(
            chat.private_outbox,
            vec![("dm:ab".to_string(), "secret".to_string())]
        );
    }
}
