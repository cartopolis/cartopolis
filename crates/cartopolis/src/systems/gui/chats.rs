//! The messenger: a list of chats, and one chat at a time with two views of it
//! (docs/notes/chats-and-world.md; the design is `docs/mockups/chat-world/v5`,
//! and contacts are `v5-contacts` there and `systems::contacts` here).
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
use crate::systems::user_store::SavedScreen;
use crate::systems::{icons, theme};

// Every colour and size below is the design's (`docs/mockups/chat-world/
// style.css`), which is itself the app palette: `theme`
// already has ink, surface, raised, accent, brand and cream at the same values.

/// The hairline around glass and the segmented control (`--gline`, white at
/// 14 %).
const GLINE: egui::Color32 = egui::Color32::from_rgba_unmultiplied_const(0xFF, 0xFF, 0xFF, 36);
/// The round initials, picked by name so a person keeps theirs (`.c1`–`.c6`).
const AVATAR_TONES: [egui::Color32; 6] = [
    egui::Color32::from_rgb(0x7A, 0x5A, 0x9A),
    egui::Color32::from_rgb(0x3F, 0x7D, 0x6E),
    egui::Color32::from_rgb(0xA0, 0x62, 0x3E),
    egui::Color32::from_rgb(0x4D, 0x6A, 0x91),
    egui::Color32::from_rgb(0x8A, 0x4F, 0x5F),
    egui::Color32::from_rgb(0x5E, 0x6F, 0x3C),
];
/// The "Here" disc: the v1 inbox's dark slate, with a location pin on it.
const HERE_TONE: egui::Color32 = egui::Color32::from_rgb(0x34, 0x44, 0x4A);
/// Bubble corners: round, with the corner nearest the speaker tucked in.
const BUBBLE_R: u8 = 18;
const TAIL_R: u8 = 6;
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
    /// A contacts screen shown instead of the list (`systems::contacts`).
    pub sub: Option<crate::systems::contacts::Sub>,
    /// A picture being picked or taken, and the chat it goes to. One at a time:
    /// every platform arm puts a modal in front of the person.
    capture: Option<(String, crate::platform::photo::Capture)>,
    /// The picture open full size, if any.
    viewing: Option<crate::systems::chat_pictures::PictureRef>,
    /// Why the last picture could not be picked, until the next try.
    picture_error: Option<String>,
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
pub(crate) fn avatar(ui: &mut egui::Ui, name: &str, kind: ChatKind, size: f32) {
    let (rect, _) = ui.allocate_exact_size(egui::Vec2::splat(size), egui::Sense::hover());
    paint_avatar(ui.painter(), rect.center(), name, kind, size);
}

pub(crate) fn paint_avatar(
    p: &egui::Painter,
    c: egui::Pos2,
    name: &str,
    kind: ChatKind,
    size: f32,
) {
    let (tone, text, scale) = match kind {
        ChatKind::Here => (HERE_TONE, icons::PIN.to_string(), 0.45),
        ChatKind::Everyone => (AVATAR_TONES[3], icons::CAM_EARTH.to_string(), 0.45),
        ChatKind::Room => (AVATAR_TONES[5], "#".to_string(), 0.42),
        ChatKind::Private => (
            tone_for(name),
            name.chars().next().unwrap_or('?').to_uppercase().collect(),
            0.38,
        ),
    };
    // A room is a place people gather rather than a person, so it is the
    // design's rounded square (`.room`), not a circle.
    if kind == ChatKind::Room {
        let rect = egui::Rect::from_center_size(c, egui::Vec2::splat(size));
        p.rect_filled(rect, egui::CornerRadius::same((size * 0.29) as u8), tone);
    } else {
        p.circle_filled(c, size / 2.0, tone);
    }
    p.text(
        c,
        egui::Align2::CENTER_CENTER,
        text,
        egui::FontId::proportional(size * scale),
        egui::Color32::WHITE,
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

/// Paint a row of text pieces centred in `rect`, `gap` apart.
fn centred_row(
    p: &egui::Painter,
    rect: egui::Rect,
    parts: &[(String, f32, egui::Color32)],
    gap: f32,
) {
    let galleys: Vec<_> = parts
        .iter()
        .map(|(t, size, c)| p.layout_no_wrap(t.clone(), egui::FontId::proportional(*size), *c))
        .collect();
    let total: f32 = galleys.iter().map(|g| g.size().x).sum::<f32>()
        + gap * galleys.len().saturating_sub(1) as f32;
    let mut x = rect.center().x - total / 2.0;
    for (g, (_, _, c)) in galleys.into_iter().zip(parts) {
        let y = rect.center().y - g.size().y / 2.0;
        let w = g.size().x;
        p.galley(egui::pos2(x, y), g, *c);
        x += w + gap;
    }
}

/// The "live" dot beside World (`.live`): brand orange with a soft halo, or
/// plain white on the orange of a selected World.
fn live_dot(p: &egui::Painter, c: egui::Pos2, on_orange: bool) {
    if on_orange {
        p.circle_filled(c, 3.5, egui::Color32::WHITE);
    } else {
        p.circle_filled(
            c,
            6.5,
            egui::Color32::from_rgba_unmultiplied(0xFD, 0x60, 0x12, 64),
        );
        p.circle_filled(c, 3.5, theme::BRAND);
    }
}

/// The design's segmented toggle (`.seg`), "the one control this whole design
/// is about": a raised pill with a Chat side and a World side. A selected Chat
/// is the pale accent; a selected World is brand orange, and World always
/// carries the live dot. `glass` is the variant that floats over the map.
/// Answers `Some(true)` for World, `Some(false)` for the other side.
fn segmented(
    ui: &mut egui::Ui,
    left: &str,
    unread: usize,
    world_on: bool,
    glass: bool,
) -> Option<bool> {
    let (rect, _) =
        ui.allocate_exact_size(egui::vec2(ui.available_width(), 40.0), egui::Sense::hover());
    let inner = rect.shrink(3.0);
    let half = (inner.width() - 2.0) / 2.0;
    let l = egui::Rect::from_min_size(inner.min, egui::vec2(half, inner.height()));
    let r = egui::Rect::from_min_size(egui::pos2(l.right() + 2.0, inner.top()), l.size());
    let id = ui.id().with(("chats_segmented", left));
    let lr = ui.interact(l, id.with(0), egui::Sense::click());
    let rr = ui.interact(r, id.with(1), egui::Sense::click());

    let p = ui.painter();
    p.rect_filled(
        rect,
        egui::CornerRadius::same(12),
        if glass {
            theme::GLASS
        } else {
            theme::SURFACE_RAISED
        },
    );
    p.rect_stroke(
        rect,
        egui::CornerRadius::same(12),
        egui::Stroke::new(1.0, GLINE),
        egui::StrokeKind::Inside,
    );
    let seg = egui::CornerRadius::same(9);
    if !world_on {
        p.rect_filled(l, seg, theme::ACCENT);
    } else if lr.hovered() {
        p.rect_filled(l, seg, theme::SURFACE_HOVER);
    }
    if world_on {
        p.rect_filled(r, seg, theme::BRAND);
    } else if rr.hovered() {
        p.rect_filled(r, seg, theme::SURFACE_HOVER);
    }

    let left_ink = if world_on {
        theme::TEXT_DIM
    } else {
        theme::INK
    };
    let mut parts = vec![
        (icons::CHAT.to_string(), 16.0, left_ink),
        (left.to_string(), 14.0, left_ink),
    ];
    if unread > 0 {
        parts.push((format!("{unread}"), 12.0, theme::BRAND));
    }
    centred_row(p, l, &parts, 6.0);

    let right_ink = if world_on {
        egui::Color32::WHITE
    } else {
        theme::TEXT_DIM
    };
    let label = p.layout_no_wrap("World".into(), egui::FontId::proportional(14.0), right_ink);
    let total = 7.0 + 6.0 + label.size().x;
    let x0 = r.center().x - total / 2.0;
    live_dot(p, egui::pos2(x0 + 3.5, r.center().y), world_on);
    p.galley(
        egui::pos2(x0 + 13.0, r.center().y - label.size().y / 2.0),
        label,
        right_ink,
    );

    if lr.clicked() {
        Some(false)
    } else if rr.clicked() {
        Some(true)
    } else {
        None
    }
}

/// A round button of the design's composer (`.circ`): raised, or the pale
/// accent when it is the one that acts (`.circ.hot`).
pub(crate) fn circle_button(
    ui: &mut egui::Ui,
    icon: &str,
    hot: bool,
    enabled: bool,
    hint: &str,
) -> egui::Response {
    let (rect, resp) = ui.allocate_exact_size(egui::Vec2::splat(38.0), egui::Sense::click());
    let (fill, ink) = match (hot, enabled) {
        (true, true) => (theme::ACCENT, theme::INK),
        (true, false) => (theme::ACCENT_MUTED, theme::TEXT_DIM),
        (false, _) if resp.hovered() => (theme::SURFACE_HOVER, theme::TEXT),
        (false, _) => (theme::SURFACE_RAISED, theme::TEXT),
    };
    let p = ui.painter();
    p.circle_filled(rect.center(), 19.0, fill);
    p.text(
        rect.center(),
        egui::Align2::CENTER_CENTER,
        icon,
        egui::FontId::proportional(20.0),
        ink,
    );
    resp.on_hover_text(hint)
}

/// The v1 pinned row's wash (`.chat-row.pinned`): brand orange at 10 % on the
/// left, fading out by 70 % of the width.
fn paint_pinned_wash(p: &egui::Painter, rect: egui::Rect) {
    let tint = egui::Color32::from_rgba_unmultiplied(0xFD, 0x60, 0x12, 26);
    let clear = egui::Color32::TRANSPARENT;
    let mid = rect.left() + rect.width() * 0.7;
    let mut mesh = egui::Mesh::default();
    for (x, c) in [(rect.left(), tint), (mid, clear)] {
        mesh.colored_vertex(egui::pos2(x, rect.top()), c);
        mesh.colored_vertex(egui::pos2(x, rect.bottom()), c);
    }
    mesh.add_triangle(0, 1, 2);
    mesh.add_triangle(1, 3, 2);
    p.add(mesh);
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

/// One row of the list (`.chat-row`): the 48 pt avatar, the name and time on
/// one line, the last message and the unread pill on the next.
fn chat_row(ui: &mut egui::Ui, row: &Row, sending: bool) -> egui::Response {
    let h = 70.0;
    let w = ui.available_width();
    let (rect, resp) = ui.allocate_exact_size(egui::vec2(w, h), egui::Sense::click());
    let p = ui.painter();
    if row.kind == ChatKind::Here {
        paint_pinned_wash(p, rect);
    }
    if resp.hovered() || resp.is_pointer_button_down_on() {
        p.rect_filled(rect, egui::CornerRadius::same(12), theme::SURFACE_RAISED);
    }
    let size = 48.0;
    paint_avatar(
        p,
        egui::pos2(rect.left() + 6.0 + size / 2.0, rect.center().y),
        &row.name,
        row.kind,
        size,
    );
    let x = rect.left() + 6.0 + size + 12.0;
    let time_w = if row.time.is_empty() {
        0.0
    } else {
        p.text(
            egui::pos2(rect.right() - 8.0, rect.top() + 17.0),
            egui::Align2::RIGHT_TOP,
            &row.time,
            egui::FontId::proportional(12.0),
            theme::TEXT_DIM,
        )
        .width()
            + 8.0
    };
    // A private chat's name carries the padlock: with no section headings, the
    // row is where "only the people in it can read this" has to be said.
    let name_x = if row.kind == ChatKind::Private {
        let r = p.text(
            egui::pos2(x, rect.top() + 16.0),
            egui::Align2::LEFT_TOP,
            icons::LOCK,
            egui::FontId::proportional(13.0),
            theme::TEXT_DIM,
        );
        r.right() + 5.0
    } else {
        x
    };
    paint_line(
        p,
        egui::pos2(name_x, rect.top() + 14.0),
        &row.name,
        15.5,
        theme::CREAM,
        rect.right() - name_x - time_w - 8.0,
    );
    let (preview, tint) = if sending && row.kind == ChatKind::Private {
        ("Tap to send the view here".to_string(), theme::BRAND)
    } else {
        (row.preview.clone(), theme::TEXT_DIM)
    };
    let mut badge_w = 0.0;
    if row.unread > 0 {
        let text = if row.unread > 99 {
            "99+".to_string()
        } else {
            row.unread.to_string()
        };
        // Brand orange with white, as the first mockups drew it (`.badge.b`):
        // something new for you is the one thing in the list worth the colour.
        let g = p.layout_no_wrap(text, egui::FontId::proportional(11.5), egui::Color32::WHITE);
        let pill = egui::Rect::from_min_size(
            egui::pos2(rect.right() - 8.0 - g.size().x - 14.0, rect.top() + 38.0),
            egui::vec2(g.size().x + 14.0, 19.0),
        );
        p.rect_filled(pill, egui::CornerRadius::same(10), theme::BRAND);
        let gs = g.size();
        p.galley(pill.center() - gs / 2.0, g, egui::Color32::WHITE);
        badge_w = pill.width() + 10.0;
    }
    paint_line(
        p,
        egui::pos2(x, rect.top() + 38.0),
        &preview,
        13.5,
        tint,
        rect.right() - x - badge_w - 8.0,
    );
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
    call: Option<crate::systems::calls::CallRequest>,
    /// Open a contacts screen.
    sub: Option<crate::systems::contacts::Sub>,
    /// Pick or take a picture to send to this chat.
    pick_picture: Option<String>,
    /// Open a picture full size.
    view_picture: Option<crate::systems::chat_pictures::PictureRef>,
    /// Send a snapshot of the map to this chat.
    snapshot: Option<String>,
}

/// What a private chat's header offers about the chat's call.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum CallButton {
    /// Not a private chat, or a build that cannot call.
    Hidden,
    /// Nobody is in a call: start one.
    Call,
    /// Somebody is: join them.
    Join,
    /// This device is in it.
    HangUp,
}

fn call_button(calls: &crate::systems::calls::Calls, key: &str) -> CallButton {
    let Some(group) = crate::systems::private_chat::group_of(key) else {
        return CallButton::Hidden;
    };
    if !crate::systems::calls::Calls::available() {
        CallButton::Hidden
    } else if calls.active_group() == Some(group.as_slice()) {
        CallButton::HangUp
    } else if calls.in_call(&group).is_empty() {
        CallButton::Call
    } else {
        CallButton::Join
    }
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
    keyboard: Res<crate::systems::onscreen_keyboard::OnScreenKeyboard>,
    mut calls: ResMut<crate::systems::calls::Calls>,
    mut contacts: ResMut<crate::systems::contacts::Contacts>,
    identity: Res<crate::resources::AuthIdentity>,
    mut pictures: ResMut<crate::systems::chat_pictures::ChatPictures>,
    mut screenshot: ResMut<crate::resources::ScreenshotState>,
) {
    // A picture being picked keeps arriving whether or not the panel is open:
    // the camera app or the file dialog is in front of it.
    poll_picture_capture(&mut view, &mut chat);
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
    // The composer sits on the panel's bottom edge, which is where the
    // on-screen keys land, so while they are up the band lifts itself clear of
    // them (and says so to the keyboard, in `compose_row`).
    let lift = crate::systems::chat_overlay::keyboard_reserve(&egui, keyboard.is_open());

    // The design's screen is solid ink (`.phone`), not the glass the other
    // panels use: a conversation is read, and a map showing through the text
    // is noise behind it.
    crate::systems::egui_manager::responsive_right_panel("chats_panel", compact, viewport_w, 360.0)
        .frame(
            egui::Frame::NONE
                .fill(theme::INK)
                .inner_margin(egui::Margin::symmetric(12, 10)),
        )
        .show_collapsible(egui.ui_mut(), &mut open_flag, |ui| {
            let me = identity.username.clone().unwrap_or_default();
            match (view.sub.clone(), view.open.clone()) {
                (Some(sub), _) => crate::systems::contacts::screen(
                    ui,
                    &sub,
                    &mut contacts,
                    &mut chat,
                    &mut view,
                    online.as_deref(),
                    &me,
                ),
                (None, None) => list_screen(
                    ui, &mut chat, &mut view, &mut voice, &contacts, &mut act, now,
                ),
                (None, Some(key)) => {
                    let call = call_button(&calls, &key);
                    chat_screen(
                        ui,
                        &mut chat,
                        &mut view,
                        &contacts,
                        &key,
                        &mut act,
                        now,
                        lift,
                        call,
                        &mut pictures,
                    );
                }
            }
        });
    if !open_flag {
        act.close = true;
    }
    if let Some(request) = act.call.take() {
        calls.request = Some(request);
    }
    if let Some(channel) = act.pick_picture.take() {
        view.picture_error = None;
        match crate::platform::photo::start() {
            Ok(capture) => {
                if let Some((_, mut old)) = view.capture.take() {
                    old.cancel();
                }
                view.capture = Some((channel, capture));
            }
            Err(why) => view.picture_error = Some(why),
        }
    }
    if let Some(picture) = act.view_picture.take() {
        view.viewing = Some(picture);
    }
    if let Some(channel) = act.snapshot.take() {
        // Taken by `ui::drive_screenshots` with the panels hidden for the one
        // frame it captures, and handed back to this chat as a picture.
        screenshot.chat_snapshot = Some(channel);
        screenshot.hide_ui = true;
        view.attach = false;
    }
    let viewport = egui.viewport_pts();
    picture_viewer(&egui.ctx, viewport, &mut view, &mut pictures);

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
    contacts: &crate::systems::contacts::Contacts,
    act: &mut Actions,
    now: u64,
) {
    // The v1 inbox: the title bar, then the Chats | World toggle under it.
    // Contacts hang off the title bar's people button (`v5-contacts`).
    ui.add_space(4.0);
    ui.horizontal(|ui| {
        ui.label(egui::RichText::new("Chats").size(28.0).color(theme::CREAM));
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            if theme::round_button(ui, icons::APPEARANCE, "Contacts", false).clicked() {
                act.sub = Some(crate::systems::contacts::Sub::Contacts);
            }
        });
    });
    ui.add_space(10.0);
    if segmented(ui, "Chats", 0, false, false) == Some(true) {
        act.to_world = true;
    }
    ui.add_space(8.0);
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
        raised_frame().show(ui, |ui| {
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

    if view.new_chat {
        new_chat_section(ui, chat, act);
    }

    // Message requests wait in one row above the chats, and the chats that
    // are still requests are not in the list until they are accepted.
    if crate::systems::contacts::requests_row(ui, contacts) {
        act.sub = Some(crate::systems::contacts::Sub::Requests);
    }
    let requests = contacts.request_channels(chat);

    let rows = |filter: &dyn Fn(ChatKind) -> bool| -> Vec<Row> {
        let mut out: Vec<(u64, Row)> = chat
            .channels
            .iter()
            .filter(|c| filter(kind_of(&c.key)) && !requests.contains(&c.key))
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
    // The v1 order, and no section headings: Here pinned first, every
    // conversation by its latest line, Everyone last. Which chats are private
    // is said on the row itself (the padlock), where it cannot be missed.
    let here = rows(&|k| k == ChatKind::Here);
    let middle = rows(&|k| matches!(k, ChatKind::Private | ChatKind::Room));
    let everyone = rows(&|k| k == ChatKind::Everyone);

    let panel = ui.max_rect();
    egui::ScrollArea::vertical()
        .auto_shrink([false, false])
        .show(ui, |ui| {
            for row in here.iter().chain(&middle).chain(&everyone) {
                if chat_row(ui, row, sending).clicked() {
                    if sending && row.kind == ChatKind::Private {
                        act.send_point_to = Some(row.key.clone());
                    } else {
                        act.open = Some(row.key.clone());
                    }
                }
            }
            if !middle.iter().any(|r| r.kind == ChatKind::Private) {
                ui.add_space(6.0);
                theme::note(
                    ui,
                    "Start a private chat with the pencil below. Only the people in it can read it.",
                );
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
            // Room under the last row for the compose button to float over.
            ui.add_space(80.0);
            let _ = now;
        });

    // New chat: the design's floating button (`.fab`), bottom right, pale
    // accent with the compose icon. Drawn after the list so it sits on top.
    let fab = egui::Rect::from_min_size(
        panel.right_bottom() - egui::vec2(56.0 + 6.0, 56.0 + 12.0),
        egui::Vec2::splat(56.0),
    );
    let resp = ui
        .interact(
            fab,
            egui::Id::new("chats_new_chat_fab"),
            egui::Sense::click(),
        )
        .on_hover_text(if view.new_chat {
            "Close"
        } else {
            "New chat: a person, a group or a room"
        });
    let p = ui.painter();
    p.add(
        egui::epaint::Shadow {
            offset: [0, 8],
            blur: 20,
            spread: 0,
            color: egui::Color32::from_black_alpha(100),
        }
        .as_shape(fab, egui::CornerRadius::same(18)),
    );
    p.rect_filled(
        fab,
        egui::CornerRadius::same(18),
        if resp.hovered() {
            theme::CREAM
        } else {
            theme::ACCENT
        },
    );
    p.text(
        fab.center(),
        egui::Align2::CENTER_CENTER,
        if view.new_chat {
            icons::CLEAR
        } else {
            icons::RENAME
        },
        egui::FontId::proportional(24.0),
        theme::INK,
    );
    if resp.clicked() {
        view.new_chat = !view.new_chat;
    }
}

fn new_chat_section(ui: &mut egui::Ui, chat: &mut ChatState, act: &mut Actions) {
    raised_frame().show(ui, |ui| {
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
#[allow(clippy::too_many_arguments)]
fn chat_header(
    ui: &mut egui::Ui,
    chat: &ChatState,
    key: &str,
    name: &str,
    kind: ChatKind,
    act: &mut Actions,
    now: u64,
    call: CallButton,
) {
    // The design's thread header (`.thead`): a bare chevron, the small avatar,
    // the name over one line of status.
    ui.horizontal(|ui| {
        ui.spacing_mut().item_spacing.x = 10.0;
        if ui
            .add(
                egui::Label::new(
                    egui::RichText::new(icons::CHEVRON_LEFT)
                        .size(26.0)
                        .color(theme::TEXT),
                )
                .sense(egui::Sense::click()),
            )
            .on_hover_text("All chats")
            .on_hover_cursor(egui::CursorIcon::PointingHand)
            .clicked()
        {
            act.back = true;
        }
        avatar(ui, name, kind, 34.0);
        ui.vertical(|ui| {
            ui.spacing_mut().item_spacing.y = 1.0;
            ui.label(egui::RichText::new(name).size(16.0).color(theme::CREAM));
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
                    .size(12.0)
                    .color(theme::REAL_LOCATION),
                );
            } else {
                ui.label(
                    egui::RichText::new(subtitle(kind))
                        .size(12.0)
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
        // A private chat can be called. Encrypted like its lines, so the
        // button lives only where the padlock does.
        let label = match call {
            CallButton::Hidden => None,
            CallButton::Call => Some("Call"),
            CallButton::Join => Some("Join call"),
            CallButton::HangUp => Some("Hang up"),
        };
        if let (Some(label), Some(group)) = (label, crate::systems::private_chat::group_of(key)) {
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if theme::action_button(ui, label).clicked() {
                    act.call = Some(if call == CallButton::HangUp {
                        crate::systems::calls::CallRequest::Leave
                    } else {
                        crate::systems::calls::CallRequest::Join(group)
                    });
                }
            });
        }
    });
}

/// One chat's thread.
#[allow(clippy::too_many_arguments)]
fn chat_screen(
    ui: &mut egui::Ui,
    chat: &mut ChatState,
    view: &mut ChatsView,
    contacts: &crate::systems::contacts::Contacts,
    key: &str,
    act: &mut Actions,
    now: u64,
    lift: f32,
    call: CallButton,
    pictures: &mut crate::systems::chat_pictures::ChatPictures,
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
    chat_header(ui, chat, key, &name, kind, act, now, call);
    ui.add_space(8.0);
    if segmented(ui, "Chat", 0, false, false) == Some(true) {
        act.world_of = Some(key.to_string());
    }
    ui.add_space(10.0);
    // The hairline under the tabs (`.tabs { border-bottom }`), edge to edge.
    let y = ui.cursor().top();
    let full = ui.max_rect();
    ui.painter().hline(
        (full.left() - 12.0)..=(full.right() + 12.0),
        y,
        egui::Stroke::new(1.0, GLINE),
    );
    ui.add_space(8.0);
    // A chat that is still your request to somebody holds one message until
    // they accept (`contacts`, enforced by the server); say so before the
    // second one is refused.
    let peer = chat
        .direct
        .iter()
        .find(|(_, k)| k.as_str() == key)
        .map(|(p, _)| p.clone());
    if let Some(peer) = peer.filter(|p| contacts.sent_to(p)) {
        theme::note(
            ui,
            &format!(
                "Request sent. {peer} sees your first message in their requests; you can \
                 write more once they accept."
            ),
        );
        ui.add_space(6.0);
    }

    // The text field sits in a bottom band of its own, so the thread above it
    // can take every point of height that is left and scroll. The band is the
    // design's composer: ink, a hairline on top, "+", the field, send.
    egui::Panel::bottom(egui::Id::new("chats_composer_band"))
        .frame(egui::Frame::NONE.fill(theme::INK))
        .show_separator_line(false)
        .show(ui, |ui| {
            let top = ui.max_rect().top();
            ui.painter().hline(
                (ui.max_rect().left() - 12.0)..=(ui.max_rect().right() + 12.0),
                top,
                egui::Stroke::new(1.0, GLINE),
            );
            ui.add_space(10.0);
            let sending = pictures.sending(key);
            if sending > 0 {
                ui.horizontal(|ui| {
                    ui.spinner();
                    ui.label(
                        egui::RichText::new(if sending == 1 {
                            "Sending a picture…".to_string()
                        } else {
                            format!("Sending {sending} pictures…")
                        })
                        .size(theme::SIZE_SUB)
                        .color(theme::TEXT_DIM),
                    );
                });
            }
            if view.capture.as_ref().is_some_and(|(k, _)| k == key) {
                theme::note(ui, "Waiting for a picture…");
            }
            if let Some(why) = view.picture_error.clone() {
                ui.label(
                    egui::RichText::new(format!("Could not get a picture: {why}"))
                        .size(theme::SIZE_SUB)
                        .color(theme::WARN),
                );
            }
            composer(ui, chat, view, key, kind, act, now);
            ui.add_space(6.0 + lift);
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
            let mut last_day = String::new();
            for line in &lines {
                // `.day`: a small chip wherever the date changes ("Today").
                if line.at_ms > 0 {
                    let day = day_label(line.at_ms, now);
                    if day != last_day {
                        day_chip(ui, &day);
                        last_day = day;
                    }
                }
                let n = line.point.as_ref().map(|_| {
                    number += 1;
                    number
                });
                bubble(ui, chat, line, group, n, act, pictures);
                ui.add_space(4.0);
            }
        });
}

/// "Today", "Yesterday", or the date, in this device's time zone.
fn day_label(at_ms: u64, now_ms: u64) -> String {
    let tz = jiff::tz::TimeZone::system();
    let (Ok(at), Ok(now)) = (
        jiff::Timestamp::from_millisecond(at_ms as i64),
        jiff::Timestamp::from_millisecond(now_ms as i64),
    ) else {
        return String::new();
    };
    let day = at.to_zoned(tz.clone()).date();
    let today = now.to_zoned(tz).date();
    if day == today {
        "Today".into()
    } else if today.yesterday().is_ok_and(|y| y == day) {
        "Yesterday".into()
    } else {
        day.strftime("%a %d %b").to_string()
    }
}

fn day_chip(ui: &mut egui::Ui, text: &str) {
    // Painted rather than a `Frame`: a frame in a centred layout takes the
    // whole row, and the design's chip is only as wide as its word.
    ui.add_space(4.0);
    let galley = ui.painter().layout_no_wrap(
        text.to_string(),
        egui::FontId::proportional(11.5),
        theme::TEXT_DIM,
    );
    let size = galley.size() + egui::vec2(20.0, 6.0);
    let (row, _) = ui.allocate_exact_size(
        egui::vec2(ui.available_width(), size.y),
        egui::Sense::hover(),
    );
    let pill = egui::Rect::from_center_size(row.center(), size);
    ui.painter()
        .rect_filled(pill, egui::CornerRadius::same(10), theme::SURFACE_RAISED);
    ui.painter()
        .galley(pill.center() - galley.size() / 2.0, galley, theme::TEXT_DIM);
    ui.add_space(4.0);
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
    pictures: &mut crate::systems::chat_pictures::ChatPictures,
) {
    let mine = is_mine(chat, line);
    let max_w = (ui.available_width() * 0.78).max(120.0);
    let layout = if mine {
        egui::Layout::right_to_left(egui::Align::TOP)
    } else {
        egui::Layout::left_to_right(egui::Align::TOP)
    };
    // `.m.me` / `.m.them`: yours pale accent on ink, theirs raised; the corner
    // on the speaker's side tucked in.
    let (fill, ink, dim) = if mine {
        (
            theme::ACCENT,
            theme::INK,
            egui::Color32::from_rgb(0x4A, 0x55, 0x5A),
        )
    } else {
        (theme::SURFACE_RAISED, theme::TEXT, theme::TEXT_DIM)
    };
    let corners = if mine {
        egui::CornerRadius {
            nw: BUBBLE_R,
            ne: BUBBLE_R,
            sw: BUBBLE_R,
            se: TAIL_R,
        }
    } else {
        egui::CornerRadius {
            nw: BUBBLE_R,
            ne: BUBBLE_R,
            sw: TAIL_R,
            se: BUBBLE_R,
        }
    };
    // A picture is a Polaroid, not a bubble: a white card with a narrow edge
    // on three sides and a deep one at the bottom, where the caption is
    // written. The same card whoever sent it, so it reads as a photograph
    // handed over rather than as a message in somebody's colour.
    if let Some(picture) = &line.picture {
        if group && !mine {
            ui.with_layout(layout, |ui| {
                ui.label(
                    egui::RichText::new(&line.sender)
                        .size(theme::SIZE_SUB)
                        .strong()
                        .color(theme::ACCENT_TEXT),
                );
            });
        }
        ui.with_layout(layout, |ui| {
            let size = crate::systems::chat_pictures::fit(
                picture,
                max_w.min(PICTURE_MAX_PT),
                PICTURE_MAX_PT,
            );
            egui::Frame::NONE
                .fill(POLAROID_WHITE)
                .corner_radius(2.0)
                .inner_margin(egui::Margin {
                    left: POLAROID_EDGE,
                    right: POLAROID_EDGE,
                    top: POLAROID_EDGE,
                    bottom: POLAROID_EDGE / 2,
                })
                .shadow(egui::Shadow {
                    offset: [0, 3],
                    blur: 10,
                    spread: 0,
                    color: egui::Color32::from_black_alpha(110),
                })
                .show(ui, |ui| {
                    ui.set_max_width(size.x);
                    ui.with_layout(egui::Layout::top_down(egui::Align::LEFT), |ui| {
                        picture_tile(ui, pictures, picture, size, act);
                        // The deep bottom edge: the caption, else blank paper.
                        ui.add_space(6.0);
                        let caption = crate::systems::private_chat::caption_of(&line.body);
                        ui.add(
                            egui::Label::new(
                                egui::RichText::new(caption.unwrap_or(" "))
                                    .size(14.0)
                                    .italics()
                                    .color(POLAROID_INK),
                            )
                            .wrap(),
                        );
                        if line.at_ms > 0 {
                            ui.with_layout(egui::Layout::right_to_left(egui::Align::Min), |ui| {
                                ui.label(
                                    egui::RichText::new(clock_time(line.at_ms))
                                        .size(10.5)
                                        .color(POLAROID_DIM),
                                );
                            });
                        }
                    });
                });
        });
        return;
    }
    // A place is its own card, not a bubble with a card inside it.
    if let (Some(point), Some(n)) = (&line.point, number) {
        ui.with_layout(layout, |ui| {
            if point_card(ui, point, n, &line.body, mine, max_w).clicked() {
                act.go_to = Some(point.clone());
            }
        });
        return;
    }
    ui.with_layout(layout, |ui| {
        egui::Frame::NONE
            .fill(fill)
            .corner_radius(corners)
            .inner_margin(egui::Margin::symmetric(12, 8))
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
                    ui.add(
                        egui::Label::new(egui::RichText::new(&line.body).size(14.5).color(ink))
                            .wrap(),
                    );
                    if line.at_ms > 0 {
                        ui.with_layout(egui::Layout::right_to_left(egui::Align::Min), |ui| {
                            ui.label(
                                egui::RichText::new(clock_time(line.at_ms))
                                    .size(10.5)
                                    .color(dim),
                            );
                        });
                    }
                });
            });
    });
}

/// The longest edge of a picture in the thread, in points. Big enough to see
/// what it is; tapping it opens it full size.
const PICTURE_MAX_PT: f32 = 240.0;

/// A Polaroid's paper: warm white rather than pure white, which would glare
/// against the ink of the panel, and the edge it leaves round the picture.
const POLAROID_WHITE: egui::Color32 = egui::Color32::from_rgb(0xF4, 0xF2, 0xEC);
const POLAROID_EDGE: i8 = 10;
/// What is written on it, and the time in the corner.
const POLAROID_INK: egui::Color32 = egui::Color32::from_rgb(0x2A, 0x2E, 0x33);
const POLAROID_DIM: egui::Color32 = egui::Color32::from_rgb(0x7A, 0x7E, 0x82);

/// A picture in the thread: the image once it has arrived, a placeholder of
/// its own size until then — so the thread does not jump when it lands.
fn picture_tile(
    ui: &mut egui::Ui,
    pictures: &mut crate::systems::chat_pictures::ChatPictures,
    picture: &crate::systems::chat_pictures::PictureRef,
    size: egui::Vec2,
    act: &mut Actions,
) {
    use crate::systems::chat_pictures::Shown;
    match pictures.show(ui.ctx(), picture) {
        Shown::Ready(texture) => {
            let response = ui
                .add(
                    egui::Image::new(egui::load::SizedTexture::new(texture.id(), size))
                        .corner_radius(1.0)
                        .sense(egui::Sense::click()),
                )
                .on_hover_text("Open full size");
            if response.clicked() {
                act.view_picture = Some(picture.clone());
            }
        }
        Shown::Loading => {
            let (rect, _) = ui.allocate_exact_size(size, egui::Sense::hover());
            ui.painter().rect_filled(rect, 10.0, theme::SURFACE);
            ui.put(rect, egui::Spinner::new());
        }
        Shown::Failed(_) => {
            let (rect, _) = ui.allocate_exact_size(size, egui::Sense::hover());
            ui.painter().rect_filled(rect, 10.0, theme::SURFACE);
            ui.painter().text(
                rect.center(),
                egui::Align2::CENTER_CENTER,
                "This picture could not be opened",
                egui::FontId::proportional(12.0),
                theme::TEXT_DIM,
            );
        }
    }
}

/// The picture open full size, over everything. The same window, order and
/// sizing as a place's photo (`bookmarks::photo_viewer`), for the same reasons.
fn picture_viewer(
    ctx: &egui::Context,
    viewport: egui::Vec2,
    view: &mut ChatsView,
    pictures: &mut crate::systems::chat_pictures::ChatPictures,
) {
    use crate::systems::chat_pictures::Shown;
    let Some(picture) = view.viewing.clone() else {
        return;
    };
    let mut open = true;
    egui::Window::new("Picture")
        .id(egui::Id::new(("chat_picture", &picture.hash)))
        .open(&mut open)
        .order(egui::Order::Foreground)
        .collapsible(false)
        .resizable(true)
        .max_size(viewport * 0.9)
        .default_size(viewport * 0.8)
        .anchor(egui::Align2::CENTER_CENTER, egui::vec2(0.0, 0.0))
        .show(ctx, |ui| match pictures.show(ctx, &picture) {
            Shown::Ready(texture) => {
                ui.add(
                    egui::Image::new(egui::load::SizedTexture::new(
                        texture.id(),
                        texture.size_vec2(),
                    ))
                    .max_size(ui.available_size())
                    .maintain_aspect_ratio(true)
                    .corner_radius(4.0),
                );
            }
            Shown::Loading => {
                ui.spinner();
            }
            Shown::Failed(why) => {
                ui.label(
                    egui::RichText::new(format!("This picture could not be opened: {why}"))
                        .size(13.0)
                        .color(theme::WARN),
                );
            }
        });
    if !open {
        view.viewing = None;
    }
}

/// Fold in a picture the person picked or took, and queue it for the chat it
/// was asked for in.
fn poll_picture_capture(view: &mut ChatsView, chat: &mut ChatState) {
    use crate::platform::photo::Outcome;
    let Some((_, capture)) = view.capture.as_mut() else {
        return;
    };
    match capture.poll() {
        Outcome::Pending => {}
        Outcome::Ready(bytes) => {
            if let Some((channel, _)) = view.capture.take() {
                chat.private_pictures
                    .push(crate::systems::chat_pictures::OutgoingPicture {
                        channel,
                        bytes,
                        caption: String::new(),
                        rgba: None,
                    });
            }
        }
        Outcome::Cancelled => view.capture = None,
        Outcome::Failed(why) => {
            view.capture = None;
            view.picture_error = Some(why);
        }
    }
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
    mine: bool,
    max_w: f32,
) -> egui::Response {
    // `.pt`: 258 wide, raised, a 112 tall picture, then the name and caption,
    // with the speaker's corner tucked in like a bubble's.
    let w = max_w.min(258.0);
    let title = point
        .name
        .clone()
        .unwrap_or_else(|| format!("{:.4}, {:.4}", point.latitude, point.longitude));
    // The caption is the text when there is one and the name otherwise
    // (`private_chat::point_caption`), so it is only a second line when it is
    // not the title.
    let text =
        (point.name.is_some() && !caption.is_empty() && point.name.as_deref() != Some(caption))
            .then(|| caption.to_string());
    // Where it was seen from, as the first mockup's card says it ("seen from
    // 120 m"): a point is a view, not only a spot.
    let coords = format!(
        "{:.4}° {}, {:.4}° {}",
        point.latitude.abs(),
        if point.latitude >= 0.0 { 'N' } else { 'S' },
        point.longitude.abs(),
        if point.longitude >= 0.0 { 'E' } else { 'W' },
    );
    let seen = Some(match point.view {
        Some(v) => format!("{coords} · seen from {:.0} m", v.altitude.max(0.0)),
        None => coords,
    });
    let sub = match (text, seen) {
        (Some(t), Some(s)) => Some(format!("{t}\n{s}")),
        (t, s) => t.or(s),
    };
    let p = ui.painter().clone();
    let title_g = p.layout(
        title,
        egui::FontId::proportional(14.5),
        theme::CREAM,
        w - 24.0,
    );
    let sub_g = sub.map(|s| {
        p.layout(
            s,
            egui::FontId::proportional(12.0),
            theme::TEXT_DIM,
            w - 24.0,
        )
    });
    let img_h = 112.0;
    // Room for the orange button under the text (`.card .go`).
    const GO_H: f32 = 32.0;
    let meta_h = 8.0
        + title_g.size().y
        + sub_g.as_ref().map_or(0.0, |g| 2.0 + g.size().y)
        + 10.0
        + GO_H
        + 12.0;
    let (rect, resp) = ui.allocate_exact_size(egui::vec2(w, img_h + meta_h), egui::Sense::click());
    let (r, t) = (BUBBLE_R, TAIL_R);
    let corners = if mine {
        egui::CornerRadius {
            nw: r,
            ne: r,
            sw: r,
            se: t,
        }
    } else {
        egui::CornerRadius {
            nw: r,
            ne: r,
            sw: t,
            se: r,
        }
    };
    p.rect_filled(rect, corners, theme::SURFACE_RAISED);

    // The picture. A point carries the camera and no image yet, so this is
    // where the image goes rather than an invented one: a few faint streets
    // and the pin, so it reads as "a place" and not as an error.
    let img = egui::Rect::from_min_size(rect.min, egui::vec2(w, img_h));
    p.rect_filled(
        img,
        egui::CornerRadius {
            nw: r,
            ne: r,
            sw: 0,
            se: 0,
        },
        egui::Color32::from_rgb(0x2B, 0x3A, 0x40),
    );
    let street = egui::Stroke::new(4.0, egui::Color32::from_rgb(0x3A, 0x4B, 0x52));
    for (a, b) in [(0.15, 0.85), (0.4, 0.25), (0.75, 0.95)] {
        p.line_segment(
            [
                egui::pos2(img.left(), img.top() + img.height() * a),
                egui::pos2(img.right(), img.top() + img.height() * b),
            ],
            street,
        );
    }
    crate::systems::place_pins::paint_pin_glyph(
        &p,
        egui::Rect::from_center_size(img.center(), egui::vec2(22.0, 32.0)),
        theme::BRAND,
    );
    // `.pt .n`: the number, cream on the picture's top left.
    let badge = egui::pos2(img.left() + 8.0 + 12.0, img.top() + 8.0 + 12.0);
    p.circle_filled(badge, 12.0, theme::CREAM);
    p.text(
        badge,
        egui::Align2::CENTER_CENTER,
        number.to_string(),
        egui::FontId::proportional(13.0),
        theme::INK,
    );

    let mut y = img.bottom() + 8.0;
    let th = title_g.size().y;
    p.galley(egui::pos2(rect.left() + 12.0, y), title_g, theme::CREAM);
    y += th + 2.0;
    if let Some(g) = sub_g {
        p.galley(egui::pos2(rect.left() + 12.0, y), g, theme::TEXT_DIM);
    }
    // The first mockup's "Open in world" (`.card .go`): the card's one action,
    // brand orange across its width. The whole card answers the tap; the
    // button is what says so.
    let go = egui::Rect::from_min_max(
        egui::pos2(rect.left() + 12.0, rect.bottom() - 12.0 - GO_H),
        egui::pos2(rect.right() - 12.0, rect.bottom() - 12.0),
    );
    p.rect_filled(
        go,
        egui::CornerRadius::same(10),
        if resp.hovered() {
            egui::Color32::from_rgb(0xFF, 0x7A, 0x33)
        } else {
            theme::BRAND
        },
    );
    centred_row(
        &p,
        go,
        &[
            (icons::CAM_EARTH.to_string(), 15.0, egui::Color32::WHITE),
            ("Open in world".to_string(), 13.5, egui::Color32::WHITE),
        ],
        6.0,
    );
    if resp.hovered() {
        p.rect_stroke(
            rect,
            corners,
            egui::Stroke::new(1.5, theme::ACCENT),
            egui::StrokeKind::Inside,
        );
    }
    resp.on_hover_cursor(egui::CursorIcon::PointingHand)
        .on_hover_text("Look from where it was sent")
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
        raised_frame().show(ui, |ui| {
            ui.set_width(ui.available_width());
            if kind == ChatKind::Private {
                if crate::platform::photo::is_available()
                    && ui
                        .button(format!("{} Send a picture", icons::CAMERA))
                        .on_hover_text(
                            "A photo or an image file. Encrypted on this device: only this \
                             chat can open it.",
                        )
                        .clicked()
                {
                    act.pick_picture = Some(key.to_string());
                    view.attach = false;
                }
                if ui
                    .button(format!("{} Send a snapshot", icons::CAMERA))
                    .on_hover_text(
                        "A picture of the map as you see it now, without the panels. \
                         Encrypted on this device: only this chat can open it.",
                    )
                    .clicked()
                {
                    act.snapshot = Some(key.to_string());
                }
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

/// "+", the field and send, shared by the Chat view and the World view: the
/// design's composer, a round raised "+", a raised pill of a field, and the
/// pale round button that sends.
fn compose_row(ui: &mut egui::Ui, view: &mut ChatsView, key: &str, id: &str, act: &mut Actions) {
    let claim = std::mem::take(&mut view.claim_focus);
    ui.horizontal(|ui| {
        ui.spacing_mut().item_spacing.x = 10.0;
        if circle_button(
            ui,
            if view.attach {
                icons::CLEAR
            } else {
                icons::ADD
            },
            false,
            true,
            "Send a place, or share where you are",
        )
        .clicked()
        {
            view.attach = !view.attach;
        }
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            let ready = !view.draft.trim().is_empty();
            let send = circle_button(ui, icons::SEND, true, ready, "Send");
            let field = egui::Frame::NONE
                .fill(theme::SURFACE_RAISED)
                .corner_radius(egui::CornerRadius::same(20))
                .inner_margin(egui::Margin::symmetric(14, 9))
                .show(ui, |ui| {
                    ui.horizontal(|ui| {
                        // The v1 field's location pin, at its right end: send
                        // what the camera is looking at. Private chats only —
                        // a point never goes into a public one.
                        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                            if ChatState::is_private(key)
                                && ui
                                    .add(
                                        egui::Label::new(
                                            egui::RichText::new(icons::PIN)
                                                .size(18.0)
                                                .color(theme::TEXT_DIM),
                                        )
                                        .sense(egui::Sense::click()),
                                    )
                                    .on_hover_text("Send this view")
                                    .on_hover_cursor(egui::CursorIcon::PointingHand)
                                    .clicked()
                            {
                                act.view_to = Some(key.to_string());
                            }
                            crate::systems::text_touch::field(
                                ui,
                                egui::TextEdit::singleline(&mut view.draft)
                                    .id(egui::Id::new(id))
                                    .frame(egui::Frame::NONE)
                                    .font(egui::FontId::proportional(14.5))
                                    .text_color(theme::TEXT)
                                    .desired_width(ui.available_width())
                                    .hint_text(egui::RichText::new("Message").color(theme::LINE)),
                            )
                        })
                        .inner
                    })
                    .inner
                })
                .inner;
            // Both callers lift this field above the on-screen keys (the
            // panel's band, the World sheet), so the keys belong on the bottom.
            crate::systems::onscreen_keyboard::field_makes_room(ui.ctx(), field.id);
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

/// A raised block inside the ink panel, for the forms and menus.
pub(crate) fn raised_frame() -> egui::Frame {
    egui::Frame::NONE
        .fill(theme::SURFACE_RAISED)
        .stroke(egui::Stroke::new(1.0, GLINE))
        .corner_radius(egui::CornerRadius::same(14))
        .inner_margin(egui::Margin::same(12))
}

/// The design's floating glass (`.float-top`, `.sheet`): ink at 84 %, a
/// hairline, round corners.
pub(crate) fn glass_frame() -> egui::Frame {
    egui::Frame::NONE
        .fill(theme::GLASS)
        .stroke(egui::Stroke::new(1.0, GLINE))
        .corner_radius(egui::CornerRadius::same(20))
        .inner_margin(egui::Margin::symmetric(12, 10))
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
    if let Some(sub) = act.sub {
        view.sub = Some(sub);
        view.new_chat = false;
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

/// What [`remember_screen`] would save now: the Chats tab and its open chat, or
/// a chat's World view (`chats: false` with a chat open), or plain World.
fn screen_now(chat: &ChatState, view: &ChatsView, panel: RightPanel) -> SavedScreen {
    if panel.is(RightPanel::Chat) {
        SavedScreen {
            chats: true,
            open: view.open.clone(),
        }
    } else {
        SavedScreen {
            chats: false,
            open: chat.world.clone(),
        }
    }
}

/// Keep the screen in `user.json` as it changes, so Android stopping the app in
/// the background does not also forget where the person was (`SavedScreen`).
///
/// Only while signed in and not mid-restore: the chats exist only then, and a
/// session still coming up (or a chat still arriving) would otherwise overwrite
/// the screen about to be put back with World.
pub fn remember_screen(
    chat: Res<ChatState>,
    view: Res<ChatsView>,
    panel: Res<RightPanel>,
    online: Option<Res<OnlineSession>>,
    restored: Res<ScreenRestore>,
    mut store: ResMut<crate::systems::user_store::UserStore>,
) {
    if online.is_none() || view.demo || !restored.done || restored.pending.is_some() {
        return;
    }
    let now = screen_now(&chat, &view, *panel);
    if store.data.screen.as_ref() != Some(&now) {
        store.data.screen = Some(now);
        store.dirty = true;
    }
}

/// How far [`restore_screen`] has got with this session.
#[derive(Resource, Default)]
pub struct ScreenRestore {
    /// The saved screen has been put back for the session that is up now.
    pub done: bool,
    /// A chat to open once it exists: private chats arrive a moment after the
    /// session does, and a chat's World view whose chat is missing closes itself.
    pending: Option<(SavedScreen, web_time::Instant)>,
}

/// How long a remembered chat is waited for before the list is left showing.
const RESTORE_WAIT_SECS: f32 = 20.0;

/// Put the remembered screen back whenever a session comes up — at a cold start,
/// and after a resume, where the socket died while the phone was suspended and
/// the private chats are re-synced from nothing.
///
/// A call ringing is drawn over all of it (`call_screen`), so it needs no rule
/// here; anything else that already chose a panel (a place link, the sign-in
/// card's follow-up) wins over a memory.
pub fn restore_screen(
    mut chat: ResMut<ChatState>,
    mut view: ResMut<ChatsView>,
    mut panel: ResMut<RightPanel>,
    online: Option<Res<OnlineSession>>,
    store: Res<crate::systems::user_store::UserStore>,
    mut restore: ResMut<ScreenRestore>,
) {
    if view.demo {
        return;
    }
    if online.is_none() {
        restore.done = false;
        restore.pending = None;
        return;
    }
    if !restore.done {
        restore.done = true;
        let Some(saved) = store.data.screen.clone() else {
            return;
        };
        let free = panel.is(RightPanel::None) || panel.is(RightPanel::Chat);
        if !free {
            return;
        }
        if saved.chats {
            *panel = RightPanel::Chat;
        }
        if saved.open.is_some() {
            restore.pending = Some((saved, web_time::Instant::now()));
        }
    }
    let Some((saved, since)) = restore.pending.clone() else {
        return;
    };
    let Some(key) = saved.open.as_deref() else {
        restore.pending = None;
        return;
    };
    // The person moved on while the chat was still arriving.
    let still_there = if saved.chats {
        panel.is(RightPanel::Chat) && view.open.as_deref().is_none_or(|k| k == key)
    } else {
        panel.is(RightPanel::None) && chat.world.as_deref().is_none_or(|k| k == key)
    };
    if !still_there || since.elapsed().as_secs_f32() > RESTORE_WAIT_SECS {
        restore.pending = None;
        return;
    }
    if chat.channels.iter().any(|c| c.key == key) {
        if saved.chats {
            view.open_chat(key);
        } else {
            chat.world = Some(key.to_string());
        }
        restore.pending = None;
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
            // `.topseg`: the glass segmented pill on its own, no frame around it.
            ui.set_width(250.0);
            if segmented(ui, "Chats", unread, true, true) == Some(false) {
                to_chats = true;
            }
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
    // The v1 World view: the map's own buttons and the dock stand down while a
    // chat is laid over it (`dock`, `hud`, `map_osd` check `ChatState::world`),
    // so the header runs the full width, 10 pt in from each edge, and the sheet
    // sits on the bottom edge (only the keyboard can push it up).
    let header_width = (viewport.x - 20.0).min(WORLD_SHEET_MAX_WIDTH);
    let width = viewport.x.min(WORLD_SHEET_MAX_WIDTH);
    let bottom = crate::systems::chat_overlay::keyboard_reserve(&egui, true);
    let top = top_offset(&chat);
    let ctx = egui.ctx_mut().clone();
    let mut act = Actions::default();

    egui::Area::new(egui::Id::new("chat_world_header"))
        .order(egui::Order::Middle)
        .anchor(egui::Align2::CENTER_TOP, egui::vec2(0.0, top))
        .show(&ctx, |ui| {
            ui.set_width(header_width);
            glass_frame().show(ui, |ui| {
                ui.set_width(header_width - 24.0);
                chat_header(
                    ui,
                    &chat,
                    &key,
                    &name,
                    kind,
                    &mut act,
                    now,
                    CallButton::Hidden,
                );
                ui.add_space(8.0);
                if segmented(ui, "Chat", 0, true, false) == Some(false) {
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
            // `.sheet`: glass, rounded only at the top, sitting on the edge.
            glass_frame()
                .corner_radius(egui::CornerRadius {
                    nw: 24,
                    ne: 24,
                    sw: 0,
                    se: 0,
                })
                .inner_margin(egui::Margin {
                    left: 14,
                    right: 14,
                    top: 4,
                    bottom: 14,
                })
                .show(ui, |ui| {
                    ui.set_width(width - 28.0);
                    // `.handle`: the grip at the top of the design's sheet.
                    let (grip, _) = ui.allocate_exact_size(
                        egui::vec2(ui.available_width(), 9.0),
                        egui::Sense::hover(),
                    );
                    ui.painter().rect_filled(
                        egui::Rect::from_center_size(grip.center(), egui::vec2(40.0, 5.0)),
                        egui::CornerRadius::same(3),
                        theme::LINE,
                    );
                    ui.add_space(2.0);
                    // `.mini`: the speaker's name in brand orange (yours in the
                    // pale accent), then what they said.
                    for line in &recent {
                        let mine = is_mine(&chat, line);
                        let who = if mine {
                            "You".to_string()
                        } else {
                            line.sender.clone()
                        };
                        ui.horizontal_wrapped(|ui| {
                            ui.spacing_mut().item_spacing.x = 4.0;
                            ui.label(egui::RichText::new(who).size(13.5).color(if mine {
                                theme::ACCENT
                            } else {
                                theme::BRAND
                            }));
                            ui.label(
                                egui::RichText::new(&line.body)
                                    .size(13.5)
                                    .color(theme::TEXT),
                            );
                        });
                    }
                    ui.add_space(6.0);
                    let y = ui.cursor().top();
                    let r = ui.max_rect();
                    ui.painter().hline(
                        (r.left() - 12.0)..=(r.right() + 12.0),
                        y,
                        egui::Stroke::new(1.0, GLINE),
                    );
                    ui.add_space(8.0);
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
        // Minutes before now, so the thread's day chip reads "Today".
        let start = now_ms().saturating_sub(10 * 60_000);
        let at = move |min: u64| start + min * 60_000;
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
        // A snapshot, so a capture shows what a picture looks like in the
        // thread (`chat_pictures::DEMO_HASH` is drawn locally, never fetched).
        chat.receive_picture(
            sanne,
            0,
            "Sanne".into(),
            crate::systems::private_chat::picture_caption("the view from the Martinitoren!"),
            crate::systems::chat_pictures::demo_picture(),
        );
        if let Some(l) = chat
            .channels
            .iter_mut()
            .find(|c| c.key == sanne)
            .and_then(|c| c.lines.last_mut())
        {
            l.at_ms = at(8);
        }
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
            // Something Sanne has just said, to float over her marker
            // (`chat_people`); the thread's last line is yours.
            let said = "Great, see you at 7!";
            let already = chat
                .channels
                .iter()
                .any(|c| c.key == sanne && c.lines.iter().any(|l| l.body == said));
            if !already {
                chat.receive(sanne, 0, "Sanne".into(), said.into(), None);
            }
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
