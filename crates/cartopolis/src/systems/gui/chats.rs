//! The messenger: a list of chats, and one chat at a time with two views of it
//! (docs/notes/chats-and-world.md; the design is `docs/mockups/chat-world/v5`,
//! and contacts are `v5-contacts` there and `systems::contacts` here).
//!
//! # Three screens, one state
//!
//!  * **Community | World** at the top of the map. *World* is the app
//!    as it always was; *Community* is the list, which is the right-hand panel
//!    (the whole screen on a phone). It was called Chats until 2026-10-16, when
//!    it took the web page's word, since it holds communities and the public
//!    chats as well as conversations (`docs/mockups/home-and-profile`). Your
//!    disc beside the switch opens the You page, and from there Settings and,
//!    for an account that may run something, Admin (`systems::admin`).
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

use crate::systems::egui_manager::EguiMut;
use bevy::prelude::*;
use cartopolis_core::meeting::Every;
use cartopolis_core::{NetMessage, ReportReason};
use std::collections::HashMap;

use crate::resources::{OnlineSession, RightPanel};
use crate::systems::chat::{person_menu, ChatLine, ChatState, PersonAction, ReportDraft};
use crate::systems::user_store::SavedScreen;
use crate::systems::{icons, theme};

// Every colour and size below is the design's (`docs/mockups/chat-world/
// style.css`), which is itself the app palette: `theme`
// already has ink, surface, raised, accent, brand and cream at the same values.

/// The hairline around glass and the segmented control (`--gline`, white at
/// 14 %).
pub(crate) const GLINE: egui::Color32 =
    egui::Color32::from_rgba_unmultiplied_const(0xFF, 0xFF, 0xFF, 36);
/// The round initials, picked by name so a person keeps theirs (`.c1`–`.c6`).
///
/// Lifted from the mockup's muted set on 2026-10-04: on the ink screen those
/// read as six shades of the same mud, and a person's colour is the one
/// thing in a list of names that is theirs. Each still carries white initials
/// at better than 3:1.
const AVATAR_TONES: [egui::Color32; 6] = [
    egui::Color32::from_rgb(0x86, 0x5C, 0xC4),
    egui::Color32::from_rgb(0x2C, 0x91, 0x7C),
    egui::Color32::from_rgb(0xC0, 0x66, 0x30),
    egui::Color32::from_rgb(0x43, 0x74, 0xC4),
    egui::Color32::from_rgb(0xB8, 0x48, 0x68),
    egui::Color32::from_rgb(0x5F, 0x8A, 0x2E),
];
/// The section labels' tone ("ONLINE NOW"): a warm tint of the brand orange,
/// so the signposts in a list carry a little colour without competing with
/// the unread badges, which are the full orange.
pub(crate) const LABEL_WARM: egui::Color32 = egui::Color32::from_rgb(0xF2, 0xA0, 0x6E);
/// A row's resting card: white at 4 %, so the list reads as a stack of
/// rounded rows on the screen rather than as text on black.
pub(crate) const ROW_CARD: egui::Color32 =
    egui::Color32::from_rgba_unmultiplied_const(0xFF, 0xFF, 0xFF, 10);

/// Blend `base` toward `over` by `k`.
pub(crate) fn blend(base: egui::Color32, over: egui::Color32, k: f32) -> egui::Color32 {
    let ch = |b: u8, o: u8| (f32::from(b) + (f32::from(o) - f32::from(b)) * k).round() as u8;
    egui::Color32::from_rgb(
        ch(base.r(), over.r()),
        ch(base.g(), over.g()),
        ch(base.b(), over.b()),
    )
}

/// A disc lit from above: `tone` in the middle, lighter at the top rim and
/// deeper at the bottom, with a faint light edge. What makes an initial read
/// as a token rather than a flat sticker.
fn lit_disc(p: &egui::Painter, c: egui::Pos2, r: f32, tone: egui::Color32) {
    const SEGMENTS: u32 = 40;
    let light = blend(tone, egui::Color32::WHITE, 0.30);
    let deep = blend(tone, egui::Color32::BLACK, 0.22);
    let mut mesh = egui::Mesh::default();
    mesh.colored_vertex(c, tone);
    for i in 0..=SEGMENTS {
        let a = i as f32 / SEGMENTS as f32 * std::f32::consts::TAU;
        let d = egui::vec2(a.cos(), a.sin());
        // `d.y` is -1 at the top of the disc and +1 at the bottom.
        let k = (d.y + 1.0) / 2.0;
        let rim = if k < 0.5 {
            blend(light, tone, k * 2.0)
        } else {
            blend(tone, deep, (k - 0.5) * 2.0)
        };
        mesh.colored_vertex(c + r * d, rim);
        if i > 0 {
            mesh.add_triangle(0, i, i + 1);
        }
    }
    p.add(mesh);
    p.circle_stroke(
        c,
        r - 0.5,
        egui::Stroke::new(
            1.0,
            egui::Color32::from_rgba_unmultiplied(0xFF, 0xFF, 0xFF, 40),
        ),
    );
}
/// The "Here" disc: the v1 inbox's dark slate, with a location pin on it.
const HERE_TONE: egui::Color32 = egui::Color32::from_rgb(0x3F, 0x3B, 0x38);
/// Bubble corners: round, with the corner nearest the speaker tucked in.
pub(crate) const BUBBLE_R: u8 = 18;
pub(crate) const TAIL_R: u8 = 6;
/// Widest the World view's composer pill gets on a desktop window.
const WORLD_SHEET_MAX_WIDTH: f32 = 520.0;

#[derive(Resource, Default)]
pub struct ChatsView {
    /// The chat on screen, by channel key; `None` is the list.
    pub open: Option<String>,
    /// What is being typed into the open chat.
    pub draft: String,
    /// The list's "New chat" section is open.
    pub new_chat: bool,
    /// Which of its forms.
    pub new_kind: NewKind,
    /// The open chat's "+" menu is open.
    pub attach: bool,
    /// The "+" menu's Location is open, showing how long to share for.
    pub share_open: bool,
    /// "+ → Camera" asked for the camera, for this chat
    /// (`camera_screen::open_from_chat`).
    pub camera_for: Option<String>,
    /// "+ → Upload" asked for a picture from the device, for this chat. Taken
    /// where the capture is polled, so it works from the World view too.
    pub upload_for: Option<String>,
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
    /// The You page is showing instead of the list (your disc, top right).
    pub you: bool,
    /// What is typed into the list's search field: the list shows only the
    /// chats and communities whose name or last line holds it.
    pub search: String,
    /// The contacts screen the You page opened, so its back arrow returns to
    /// the You page rather than to the contacts list.
    pub sub_root: Option<crate::systems::contacts::Sub>,
    /// Settings was opened from the You page: its home gets a back arrow to
    /// there instead of a close button (`settings::render_settings_panel`).
    pub settings_from_you: bool,
    /// Sign out was pressed on the You page, for [`apply_sign_out`].
    pub sign_out: bool,
    /// How many reports wait, for the You page's Admin row; `None` for an
    /// account that may run nothing, which then has no Admin row at all.
    /// Written by `admin::render_admin_panel`.
    pub admin: Option<usize>,
    /// A meeting or a poll being written for a chat (`+ → Plan a meeting`,
    /// `+ → Ask a poll`), in that chat's composer: (chat key, the form).
    pub meeting_form: Option<(String, MeetingForm)>,
}

/// The fields of `+ → Plan a meeting` and `+ → Ask a poll`, the web chat's two
/// dialogs (`crates/atlas/web`, `chat.tsx`) as forms above the composer.
#[derive(Debug, Clone, PartialEq)]
pub enum MeetingForm {
    Plan {
        title: String,
        start_ms: u64,
        minutes: u32,
        /// One item a line; a number at the end is its minutes.
        agenda: String,
        text: String,
        /// How often it comes back, and every how many: `None` for once.
        repeat: Option<(Every, u32)>,
        /// The start of the series' last date; `None` for no end.
        until_ms: Option<u64>,
    },
    Poll {
        question: String,
        /// One option a line.
        options: String,
        multiple: bool,
    },
}

impl MeetingForm {
    /// A meeting at the next whole hour, an hour long, as the web chat starts one.
    #[must_use]
    pub fn plan(now_ms: u64) -> Self {
        MeetingForm::Plan {
            title: String::new(),
            start_ms: next_hour_ms(now_ms),
            minutes: 60,
            agenda: String::new(),
            text: String::new(),
            repeat: None,
            until_ms: None,
        }
    }

    #[must_use]
    pub fn poll() -> Self {
        MeetingForm::Poll {
            question: String::new(),
            options: String::new(),
            multiple: false,
        }
    }

    /// What the form posts, or `None` while it is not yet complete: a plan
    /// needs a title, a poll a question and two options.
    #[must_use]
    pub fn object(&self) -> Option<cartopolis_core::meeting::MeetingObject> {
        use cartopolis_core::meeting::{
            new_id, AgendaItem, MeetingObject, Plan, Poll, Repeat, MAX_ITEMS,
        };
        match self {
            MeetingForm::Plan {
                title,
                start_ms,
                minutes,
                agenda,
                text,
                repeat,
                until_ms,
            } => {
                let title = title.trim();
                (!title.is_empty()).then(|| {
                    MeetingObject::Plan(Plan {
                        id: new_id(),
                        title: title.to_string(),
                        start_ms: *start_ms,
                        end_ms: Some(start_ms + u64::from(*minutes) * 60_000),
                        text: Some(text.trim().to_string()).filter(|t| !t.is_empty()),
                        agenda: agenda
                            .lines()
                            .filter_map(AgendaItem::from_line)
                            .take(MAX_ITEMS)
                            .collect(),
                        repeat: repeat.map(|(every, interval)| Repeat {
                            every,
                            interval,
                            until_ms: until_ms
                                .and_then(|u| snap_until(*start_ms, every, interval, u)),
                        }),
                        skip: Vec::new(),
                    })
                })
            }
            MeetingForm::Poll {
                question,
                options,
                multiple,
            } => {
                let question = question.trim();
                let options: Vec<String> = options
                    .lines()
                    .map(str::trim)
                    .filter(|o| !o.is_empty())
                    .map(str::to_string)
                    .take(MAX_ITEMS)
                    .collect();
                (!question.is_empty() && options.len() >= 2).then(|| {
                    MeetingObject::Poll(Poll {
                        id: new_id(),
                        question: question.to_string(),
                        options,
                        multiple: *multiple,
                    })
                })
            }
        }
    }
}

/// The next whole hour on this device's clock, which is not a whole UTC hour
/// where the offset has minutes in it.
fn next_hour_ms(now_ms: u64) -> u64 {
    const HOUR: i64 = 3_600_000;
    let off = i64::from(local_offset_s(now_ms).unwrap_or(0)) * 1000;
    let local = now_ms as i64 + off;
    ((local.div_euclid(HOUR) + 1) * HOUR - off).max(0) as u64
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
    /// A topic of a community: a private chat, drawn as a place people gather.
    Topic,
    /// A community itself (its row in the list and its screens).
    Community,
}

#[must_use]
pub fn kind_of(key: &str) -> ChatKind {
    if ChatState::is_private(key) {
        ChatKind::Private
    } else if key == "local" {
        ChatKind::Here
    } else {
        // "geo:", and anything else public: rooms are gone (protocol 39).
        ChatKind::Everyone
    }
}

/// The line under a chat's name: who can read it.
#[must_use]
pub fn subtitle(kind: ChatKind) -> String {
    match kind {
        ChatKind::Private => format!("{} Private · end-to-end encrypted", icons::LOCK),
        ChatKind::Here => "Nearby · public, whoever is within earshot".into(),
        ChatKind::Everyone => "Everyone on this server · public".into(),
        ChatKind::Topic => format!("{} A topic · end-to-end encrypted", icons::LOCK),
        ChatKind::Community => "A community".into(),
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
        ChatKind::Topic | ChatKind::Community => label.to_string(),
    }
}

fn is_mine(chat: &ChatState, line: &ChatLine) -> bool {
    chat.is_mine(line)
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

pub(crate) fn tone_for(name: &str) -> egui::Color32 {
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
    // A person with a picture is their picture (`systems::faces`).
    if matches!(kind, ChatKind::Private) {
        if let Some(texture) = crate::systems::faces::texture_for(name) {
            crate::systems::faces::paint_face(p, c, size / 2.0, texture);
            return;
        }
    }
    let (tone, text, scale) = match kind {
        ChatKind::Here => (HERE_TONE, icons::PIN.to_string(), 0.45),
        ChatKind::Everyone => (AVATAR_TONES[3], icons::CAM_EARTH.to_string(), 0.45),
        ChatKind::Topic => (AVATAR_TONES[5], "#".to_string(), 0.42),
        ChatKind::Community => (
            tone_for(name),
            name.chars().next().unwrap_or('?').to_uppercase().collect(),
            0.4,
        ),
        ChatKind::Private => (
            tone_for(name),
            name.chars().next().unwrap_or('?').to_uppercase().collect(),
            0.38,
        ),
    };
    // A topic or a community is a place people gather rather than a person,
    // so it is the design's rounded square (`.room`), not a circle.
    if matches!(kind, ChatKind::Topic | ChatKind::Community) {
        let rect = egui::Rect::from_center_size(c, egui::Vec2::splat(size));
        p.rect_filled(rect, egui::CornerRadius::same((size * 0.29) as u8), tone);
    } else {
        lit_disc(p, c, size / 2.0, tone);
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

/// The two places the top switch leads: the Community panel (chats,
/// communities, people) and the World (the map). Admin is not a third side: it
/// is a row on the You page, behind your disc, since it is a few people's tool
/// and not a place everybody goes.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum Seg {
    Community,
    World,
}

/// The design's segmented toggle (`.seg`), "the one control this whole design
/// is about": a raised pill with a Community side and a World side. A selected
/// Community is the pale accent, a selected World brand orange (World always
/// carries the live dot). `glass` is the variant that floats over the map.
pub(crate) fn segmented(
    ui: &mut egui::Ui,
    left: &str,
    width: f32,
    on: Seg,
    unread: usize,
    glass: bool,
) -> Option<Seg> {
    let (rect, _) = ui.allocate_exact_size(egui::vec2(width, 40.0), egui::Sense::hover());
    let inner = rect.shrink(3.0);
    let segs = [Seg::Community, Seg::World];
    let n = segs.len() as f32;
    let w = (inner.width() - 2.0 * (n - 1.0)) / n;
    let id = ui.id().with("chats_segmented");

    let p = ui.painter().clone();
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
    let corner = egui::CornerRadius::same(9);
    let size = 14.0;
    let mut picked = None;
    for (i, &seg) in segs.iter().enumerate() {
        let r = egui::Rect::from_min_size(
            egui::pos2(inner.left() + i as f32 * (w + 2.0), inner.top()),
            egui::vec2(w, inner.height()),
        );
        let resp = ui.interact(r, id.with(i), egui::Sense::click());
        if resp.clicked() && seg != on {
            picked = Some(seg);
        }
        let selected = seg == on;
        let (fill, ink) = match seg {
            Seg::Community => (theme::ACCENT, theme::INK),
            Seg::World => (theme::BRAND, egui::Color32::WHITE),
        };
        if selected {
            p.rect_filled(r, corner, fill);
        } else if resp.hovered() {
            p.rect_filled(r, corner, theme::SURFACE_HOVER);
        }
        let ink = if selected { ink } else { theme::TEXT_DIM };
        match seg {
            Seg::Community => {
                let icon = if left == "Chat" {
                    icons::CHAT
                } else {
                    icons::GROUP
                };
                let mut parts = vec![(icon.to_string(), 16.0, ink)];
                parts.push((left.to_string(), size, ink));
                if unread > 0 && !selected {
                    parts.push((format!("{unread}"), 12.0, theme::BRAND));
                }
                centred_row(&p, r, &parts, 6.0);
            }
            Seg::World => {
                let label = p.layout_no_wrap("World".into(), egui::FontId::proportional(size), ink);
                let total = 7.0 + 6.0 + label.size().x;
                let x0 = r.center().x - total / 2.0;
                live_dot(&p, egui::pos2(x0 + 3.5, r.center().y), selected);
                p.galley(
                    egui::pos2(x0 + 13.0, r.center().y - label.size().y / 2.0),
                    label,
                    ink,
                );
            }
        }
    }
    picked
}

/// What [`top_bar`] was asked to do.
#[derive(Default)]
pub(crate) struct TopBar {
    pub seg: Option<Seg>,
    /// Your disc was tapped: open the You page.
    pub you: bool,
}

/// The top of the Community panel: the switch, and your own disc beside it
/// (`docs/mockups/home-and-profile/v1`, A). No title above it: the switch says
/// where you are. `waiting` puts an orange dot on the disc, for a message
/// request or a report waiting for an admin — both are on the You page.
pub(crate) fn top_bar(
    ui: &mut egui::Ui,
    on: Seg,
    unread: usize,
    me: &str,
    waiting: bool,
) -> TopBar {
    let mut out = TopBar::default();
    ui.add_space(2.0);
    ui.horizontal(|ui| {
        let disc = 42.0;
        let w = ui.available_width() - disc - ui.spacing().item_spacing.x;
        out.seg = segmented(ui, "Community", w, on, unread, false);
        let (rect, resp) = ui.allocate_exact_size(egui::Vec2::splat(disc), egui::Sense::click());
        let p = ui.painter();
        p.circle_stroke(
            rect.center(),
            disc / 2.0 + 1.0,
            egui::Stroke::new(1.0, GLINE),
        );
        let name = if me.is_empty() { "?" } else { me };
        paint_avatar(p, rect.center(), name, ChatKind::Private, disc - 2.0);
        if waiting {
            let c = rect.right_top() + egui::vec2(-4.0, 4.0);
            p.circle_filled(c, 6.5, theme::PANEL);
            p.circle_filled(c, 5.0, theme::BRAND);
        }
        out.you = resp
            .on_hover_text("You: your profile and settings")
            .clicked();
    });
    ui.add_space(10.0);
    out
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
    /// When its last line was said, for the newest-first order.
    at_ms: u64,
    /// A community's picture, in place of its initial.
    picture: Option<egui::TextureHandle>,
}

/// One row of the list (`.chat-row`): the 48 pt avatar, the name and time on
/// one line, the last message and the unread pill on the next.
fn chat_row(ui: &mut egui::Ui, row: &Row, sending: bool) -> egui::Response {
    let h = 70.0;
    let w = ui.available_width();
    let (rect, resp) = ui.allocate_exact_size(egui::vec2(w, h), egui::Sense::click());
    // Each row is a soft card, with a little air between them.
    ui.add_space(6.0);
    let p = ui.painter();
    let card = egui::CornerRadius::same(14);
    let fill = if resp.hovered() || resp.is_pointer_button_down_on() {
        theme::SURFACE_HOVER
    } else {
        ROW_CARD
    };
    p.rect_filled(rect, card, fill);
    // Something new in it: a brand stripe down the card's left edge, so an
    // unread chat is found by the eye before the badge is read.
    if row.unread > 0 {
        p.rect_filled(
            egui::Rect::from_min_size(
                egui::pos2(rect.left(), rect.top() + 14.0),
                egui::vec2(3.0, rect.height() - 28.0),
            ),
            egui::CornerRadius::same(2),
            theme::BRAND,
        );
    }
    let size = 48.0;
    let face = egui::pos2(rect.left() + 6.0 + size / 2.0, rect.center().y);
    match &row.picture {
        Some(t) => {
            egui::Image::new(t)
                .corner_radius(egui::CornerRadius::same((size * 0.29) as u8))
                .paint_at(
                    ui,
                    egui::Rect::from_center_size(face, egui::Vec2::splat(size)),
                );
        }
        None => paint_avatar(p, face, &row.name, row.kind, size),
    }
    let p = ui.painter();
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
        (
            row.preview.clone(),
            if row.unread > 0 {
                theme::TEXT
            } else {
                theme::TEXT_DIM
            },
        )
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

/// What an open chat needs to know when it is a topic of a community.
#[derive(Clone)]
struct TopicContext {
    /// "# general".
    name: String,
    /// "Garage 44 · 12 people".
    sub: String,
    community: uuid::Uuid,
    community_name: String,
    /// Whether this account moderates the community.
    moderates: bool,
    group: Vec<u8>,
}

impl TopicContext {
    fn of(communities: &crate::systems::communities::Communities, key: &str) -> Option<Self> {
        let (c, t) = communities.topic(key)?;
        let (name, sub, community) = crate::systems::communities::topic_header(communities, key)?;
        Some(Self {
            name,
            sub,
            community,
            community_name: c.settings.name.clone(),
            moderates: c
                .role
                .is_some_and(cartopolis_core::CommunityRole::moderates),
            group: t.group.clone(),
        })
    }
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
    /// Work for a community, handed to `Communities::ops`.
    community_ops: Vec<crate::systems::communities::CommunityOp>,
    /// Open a community's screen.
    show_community: Option<uuid::Uuid>,
    /// A community picked in the list: the World, with only its posts on it.
    community_world: Option<uuid::Uuid>,
    /// Open the directory, or the form for starting a community.
    community_find: bool,
    community_start: bool,
    /// Report a line to its community's moderators: (who, line, community,
    /// topic, seq).
    report_to_mods: Option<(String, Option<String>, uuid::Uuid, Vec<u8>, u64)>,
    /// A server admin suspends (`false`) or deletes (`true`) an account.
    admin: Option<(String, bool)>,
    share: Option<(String, u64)>,
    stop_share: Option<String>,
    show_location: Option<(f64, f64, String)>,
    /// Plan a route to a point: `(lat, lng, name)`.
    route_to: Option<(f64, f64, String)>,
    /// Plan a route to a person sharing their location, which follows them:
    /// `(chat key, username, lat, lng)`.
    route_to_person: Option<(String, String, f64, f64)>,
    /// `+ → Meet up` in this chat.
    meetup: Option<String>,
    /// Move the draft meet-up's time, in minutes.
    meetup_shift: Option<i64>,
    meetup_send: bool,
    meetup_cancel: bool,
    /// "I'm going" to a meet-up: (chat key, the meet-up).
    meetup_going: Option<(String, cartopolis_core::point::Point)>,
    /// An answer to a plan or a vote in a poll, to seal and send: (chat key,
    /// what).
    meeting: Option<(String, cartopolis_core::meeting::MeetingObject)>,
    block: Option<String>,
    unblock: Option<String>,
    report: Option<(String, Option<String>)>,
    message: Option<String>,
    dismiss_notice: bool,
    call: Option<crate::systems::calls::CallRequest>,
    /// Open a contacts screen.
    sub: Option<crate::systems::contacts::Sub>,
    /// Open a picture full size.
    view_picture: Option<crate::systems::chat_pictures::PictureRef>,
    /// Open (`true`) or leave (`false`) the You page.
    you: Option<bool>,
    /// Put another panel in this one's place: Admin, Settings, Appearance.
    panel: Option<RightPanel>,
    /// The contacts screen the You page opened, so back returns there.
    sub_root: Option<crate::systems::contacts::Sub>,
    /// Settings, opened from the You page.
    settings_from_you: bool,
    sign_out: bool,
    /// Open the calendar.
    calendar: bool,
    /// Open the calendar on this row.
    calendar_pick: Option<crate::systems::calendar::Pick>,
}

/// Do what the calendar's pages asked for, through the same [`Actions`] the
/// chat cards use, so an answer from the calendar is the answer from the card.
fn calendar_out(
    out: crate::systems::calendar::Out,
    calendar: &mut crate::systems::calendar::Calendar,
    view: &mut ChatsView,
    act: &mut Actions,
) {
    if out.back {
        calendar.open = false;
    }
    if out.save {
        crate::systems::calendar::mark_dirty(calendar);
    }
    act.open = out.open_chat;
    act.meeting = out.meeting;
    act.meetup_going = out.meetup_going;
    act.route_to = out.route_to;
    act.go_to = out.go_to;
    if let Some(group) = out.join {
        act.call = Some(crate::systems::calls::CallRequest::Join(group));
    }
    if let Some((key, day_ms)) = out.plan_in {
        // The chat's own form, on the day the calendar was showing, at the
        // clock time it would have proposed for today.
        let today = MeetingForm::plan(now_ms());
        let mut form = today.clone();
        if let (MeetingForm::Plan { start_ms, .. }, MeetingForm::Plan { start_ms: t, .. }) =
            (&mut form, &today)
        {
            let clock = t.saturating_sub(crate::systems::calendar::day_start_ms(
                crate::systems::calendar::local_day(*t),
            ));
            *start_ms = (day_ms + clock).max(*t);
        }
        view.meeting_form = Some((key.clone(), form));
        act.open = Some(key);
    }
    if let Some(key) = out.meetup_in {
        act.meetup = Some(key.clone());
        act.open = Some(key);
    }
}

/// The call key in a chat's header: between the call screen's banner size
/// ([`theme::CALL_BUTTON_SMALL`]) and its thumb size, so it is the largest thing
/// in the header without crowding the name.
const HEADER_CALL_KEY: f32 = 42.0;

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
    mut egui: EguiMut,
    mut chat: ResMut<ChatState>,
    mut view: ResMut<ChatsView>,
    online: Option<Res<OnlineSession>>,
    mut right_panel: ResMut<RightPanel>,
    mut nav: ResMut<crate::systems::geo_nav::GeoNavState>,
    keyboard: Res<crate::systems::onscreen_keyboard::OnScreenKeyboard>,
    mut calls: ResMut<crate::systems::calls::Calls>,
    mut contacts: ResMut<crate::systems::contacts::Contacts>,
    identity: Res<crate::resources::AuthIdentity>,
    mut pictures: ResMut<crate::systems::chat_pictures::ChatPictures>,
    mut communities: ResMut<crate::systems::communities::Communities>,
    mut faces: ResMut<crate::systems::faces::Faces>,
    mut calendar: ResMut<crate::systems::calendar::Calendar>,
) {
    for said in communities.take_admin_results() {
        chat.notice = Some(crate::systems::chat::ChatNotice {
            text: said,
            error: false,
        });
    }
    // "+ → Upload", from the panel or a chat's World view.
    if let Some(channel) = view.upload_for.take() {
        view.picture_error = None;
        match crate::platform::photo::start_from(crate::platform::photo::Source::File) {
            Ok(capture) => {
                if let Some((_, mut old)) = view.capture.take() {
                    old.cancel();
                }
                view.capture = Some((channel, capture));
            }
            Err(why) => view.picture_error = Some(why),
        }
    }
    // A picture being picked keeps arriving whether or not the panel is open:
    // the picker is in front of it.
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
                .fill(theme::PANEL)
                .inner_margin(egui::Margin::symmetric(12, 10)),
        )
        .show_collapsible(egui.ui_mut(), &mut open_flag, |ui| {
            let mut me = identity.username.clone().unwrap_or_default();
            if me.is_empty() && view.demo {
                // A scripted capture has nobody signed in to draw.
                me = "jeroen".into();
            }
            let server = match identity.server_name.as_deref() {
                None if view.demo => "cartopolis.org",
                other => other.unwrap_or_default(),
            };
            let me_full = cartopolis_core::address::person(&me, server);
            let admin = identity.may(cartopolis_core::permissions::Permission::AccountsManage);
            if communities.screen.is_some() && view.sub.is_none() && view.open.is_none() {
                let out = crate::systems::communities::screen(ui, &mut communities, &me_full);
                if let Some(key) = out.open {
                    communities.screen = None;
                    act.open = Some(key);
                }
                if let Some(who) = out.message {
                    communities.screen = None;
                    act.message = Some(who);
                }
                // A public post's place: open it as a point in a chat opens,
                // with the community's posts pinned (`community_posts`).
                if let Some(point) = out.go_to {
                    act.go_to = Some(point);
                }
                // The ✕: what World does from the list. The page stays where
                // it was, so opening Community again comes back to it.
                act.close |= out.close;
                return;
            }
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
                (None, None) if view.you => {
                    you_screen(
                        ui, &me, &me_full, &contacts, view.admin, &mut faces, &mut act,
                    );
                }
                (None, None) if calendar.open => {
                    let mut out = crate::systems::calendar::Out::default();
                    crate::systems::calendar::screen(
                        ui,
                        &mut calendar,
                        &chat,
                        &communities,
                        now,
                        &mut out,
                    );
                    calendar_out(out, &mut calendar, &mut view, &mut act);
                }
                (None, None) => list_screen(
                    ui,
                    &mut chat,
                    &mut view,
                    &contacts,
                    &mut communities,
                    &calendar,
                    &mut act,
                    &me,
                    lift,
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
                        TopicContext::of(&communities, &key),
                        admin,
                    );
                }
            }
        });
    if !open_flag {
        act.close = true;
    }
    if std::mem::take(&mut act.calendar) {
        calendar.show(now);
    }
    if let Some(pick) = act.calendar_pick.take() {
        calendar.show(now);
        calendar.picked = Some(pick);
        calendar.every = false;
        calendar.page = crate::systems::calendar::Page::Detail;
    }
    if let Some(request) = act.call.take() {
        calls.request = Some(request);
    }
    communities.ops.append(&mut act.community_ops);
    if let Some(id) = act.show_community.take() {
        view.open = None;
        communities.show(id);
    }
    if let Some(id) = act.community_world.take() {
        view.open = None;
        communities.posts.show_only(id);
        chat.world = None;
        *right_panel = RightPanel::None;
    }
    if std::mem::take(&mut act.community_find) {
        view.new_chat = false;
        communities.find();
    }
    if std::mem::take(&mut act.community_start) {
        view.new_chat = false;
        communities.start();
    }
    if let Some((who, line, id, group, seq)) = act.report_to_mods.take() {
        chat.report = Some(ReportDraft {
            username: who,
            message: line,
            reason: ReportReason::Spam,
            note: String::new(),
            asset: None,
            community: Some((id, group, seq)),
        });
    }
    match act.admin.take() {
        Some((who, false)) => communities.admin_act(who, false),
        Some((who, true)) => communities.confirm_delete = Some(who),
        None => {}
    }
    crate::systems::communities::confirm_account_delete(&egui.ctx, &mut communities);
    if std::mem::take(&mut communities.pick_picture) {
        match crate::platform::photo::start_from(crate::platform::photo::Source::File) {
            Ok(capture) => communities.capture = Some(capture),
            Err(why) => communities.picture_note = Some(why),
        }
    }
    if let Some(capture) = communities.capture.as_mut() {
        use crate::platform::photo::Outcome;
        match capture.poll() {
            Outcome::Pending => {}
            Outcome::Ready(bytes) => {
                communities.capture = None;
                communities.upload_picture(bytes);
            }
            Outcome::Cancelled => {
                communities.capture = None;
                communities.picture_for_post = false;
            }
            Outcome::Failed(why) => {
                communities.capture = None;
                communities.picture_for_post = false;
                communities.picture_note = Some(why);
            }
        }
    }
    if let Some(picture) = act.view_picture.take() {
        view.viewing = Some(picture);
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

#[allow(clippy::too_many_arguments)]
fn list_screen(
    ui: &mut egui::Ui,
    chat: &mut ChatState,
    view: &mut ChatsView,
    contacts: &crate::systems::contacts::Contacts,
    communities: &mut crate::systems::communities::Communities,
    calendar: &crate::systems::calendar::Calendar,
    act: &mut Actions,
    me: &str,
    lift: f32,
) {
    // The switch and your disc; no title above them, since the switch says
    // where you are (`docs/mockups/home-and-profile/v1`, A).
    let bar = top_bar(
        ui,
        Seg::Community,
        0,
        me,
        !contacts.incoming.is_empty() || view.admin.is_some_and(|n| n > 0),
    );
    if bar.seg == Some(Seg::World) {
        act.to_world = true;
    }
    if bar.you {
        act.you = Some(true);
    }
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

    // Search: the list as you type, by name or by the last line. On this
    // device only; it does not search message history.
    egui::Frame::NONE
        .fill(theme::SURFACE_RAISED)
        .stroke(egui::Stroke::new(1.0, GLINE))
        .corner_radius(egui::CornerRadius::same(14))
        .inner_margin(egui::Margin::symmetric(12, 9))
        .show(ui, |ui| {
            ui.horizontal(|ui| {
                ui.label(
                    egui::RichText::new(icons::SEARCH)
                        .size(17.0)
                        .color(theme::LINE),
                );
                crate::systems::text_touch::field(
                    ui,
                    egui::TextEdit::singleline(&mut view.search)
                        .id(egui::Id::new("chats_search"))
                        .frame(egui::Frame::NONE)
                        .desired_width(ui.available_width())
                        .hint_text("Search"),
                );
            });
        });
    ui.add_space(10.0);
    let needle = view.search.trim().to_lowercase();
    let matches = |name: &str, preview: &str| {
        needle.is_empty()
            || name.to_lowercase().contains(&needle)
            || preview.to_lowercase().contains(&needle)
    };

    // Message requests wait in one row above the chats, and the chats that
    // are still requests are not in the list until they are accepted.
    if crate::systems::contacts::requests_row(ui, contacts) {
        act.sub = Some(crate::systems::contacts::Sub::Requests);
    }
    let requests = contacts.request_channels(chat);

    let row_of = |c: &crate::systems::chat::ChatChannel| -> Row {
        let visible: Vec<&ChatLine> = c
            .lines
            .iter()
            .filter(|l| !chat.is_blocked(&l.sender))
            .collect();
        let last = visible.last();
        // A camera picture's pose is for the agent; a person reads its note.
        let shown = |l: &ChatLine| crate::systems::private_chat::readable(&l.body).into_owned();
        let kind = kind_of(&c.key);
        let preview = match last {
            None => match kind {
                ChatKind::Here => "Whoever is near your avatar".into(),
                ChatKind::Everyone => "Everyone on this server".into(),
                _ => "No messages yet".into(),
            },
            Some(l) if is_mine(chat, l) => format!("You: {}", shown(l)),
            Some(l) if kind == ChatKind::Private && !c.label.contains(&l.sender) => {
                format!("{}: {}", l.sender, shown(l))
            }
            Some(l) if kind != ChatKind::Private => format!("{}: {}", l.sender, shown(l)),
            Some(l) => shown(l),
        };
        let at = last.map_or(0, |l| l.at_ms);
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
            at_ms: at,
            picture: None,
        }
    };
    let public: Vec<Row> = [ChatKind::Here, ChatKind::Everyone]
        .iter()
        .filter_map(|k| chat.channels.iter().find(|c| kind_of(&c.key) == *k))
        .map(row_of)
        .filter(|r| matches(&r.name, &r.preview))
        .collect();
    // One list, newest first: people, groups and communities together. A
    // community is one row; a tap opens its only topic, or the community.
    let mut rows: Vec<Row> = chat
        .channels
        .iter()
        .filter(|c| {
            kind_of(&c.key) == ChatKind::Private
                && !requests.contains(&c.key)
                // Topics are inside their community's row.
                && !communities.is_topic(&c.key)
        })
        .map(row_of)
        .collect();
    let unread = |key: &str| {
        chat.channels
            .iter()
            .find(|c| c.key == key)
            .map_or(0, |c| view.unread(chat, key, &c.lines))
    };
    let mut community_of = HashMap::new();
    for c in crate::systems::communities::list_rows(ui.ctx(), communities, chat, &unread) {
        let key = format!("community:{}", c.id);
        community_of.insert(key.clone(), (c.id, c.open.clone()));
        rows.push(Row {
            key,
            name: c.name,
            kind: ChatKind::Community,
            preview: c.preview,
            time: if c.at_ms > 0 {
                clock_time(c.at_ms)
            } else {
                String::new()
            },
            unread: c.unread,
            at_ms: c.at_ms,
            picture: c.picture,
        });
    }
    rows.retain(|r| matches(&r.name, &r.preview));
    rows.sort_by(|a, b| b.at_ms.cmp(&a.at_ms));

    let panel = ui.max_rect();
    egui::ScrollArea::vertical()
        .auto_shrink([false, false])
        .show(ui, |ui| {
            // Here and Everyone, the two public chats, side by side.
            if !public.is_empty() {
                ui.horizontal(|ui| {
                    let gap = ui.spacing().item_spacing.x;
                    let w = (ui.available_width() - gap) / 2.0;
                    for row in &public {
                        if public_tile(ui, row, w).clicked() {
                            act.open = Some(row.key.clone());
                        }
                    }
                });
                ui.add_space(8.0);
            }
            // What is coming up from every chat, and the way into the
            // calendar (`docs/mockups/calendar/v1`, A). Not while searching:
            // the list is then the answer to the search.
            if needle.is_empty() && !sending {
                let (open, pick) =
                    crate::systems::calendar::coming_up(ui, calendar, chat, communities, now_ms());
                act.calendar |= open;
                if pick.is_some() {
                    act.calendar_pick = pick;
                }
                ui.add_space(4.0);
            }
            if !rows.is_empty() {
                theme::section(ui, "Conversations");
            }
            for row in &rows {
                if !chat_row(ui, row, sending).clicked() {
                    continue;
                }
                if let Some((id, _)) = community_of.get(&row.key) {
                    // A community is a place on the map before it is a
                    // conversation (2026-10-18): picking one goes to the
                    // World with only its posts there. Its page and topics are
                    // a tap on the chip that says so (`community_posts::
                    // render_filter_chip`).
                    act.community_world = Some(*id);
                } else if sending && row.kind == ChatKind::Private {
                    act.send_point_to = Some(row.key.clone());
                } else {
                    act.open = Some(row.key.clone());
                }
            }
            if rows.is_empty() && !needle.is_empty() {
                theme::note(ui, "Nothing here goes by that name.");
            } else if !rows.iter().any(|r| r.kind == ChatKind::Private) {
                ui.add_space(6.0);
                theme::note(
                    ui,
                    "Start a private chat with the + below. Only the people in it can read it.",
                );
            }
            // Room under the last row for the + button to float over.
            ui.add_space(80.0);
        });

    // New: the design's floating button (`.fab`), bottom right, pale accent
    // with a plus. Drawn after the list so it sits on top.
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
        .on_hover_text("New: a person, a group or a community");
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
        icons::ADD,
        egui::FontId::proportional(26.0),
        theme::INK,
    );
    if resp.clicked() {
        view.new_chat = true;
        view.new_kind = NewKind::Choose;
    }
    if view.new_chat {
        let mut kind = view.new_kind;
        if new_sheet(ui.ctx(), panel, lift, chat, &mut kind, act) {
            view.new_chat = false;
        }
        view.new_kind = kind;
    }
}

/// Here or Everyone as a tile, half the list's width: the two public chats
/// share one row instead of taking two (`docs/mockups/home-and-profile/v1`).
fn public_tile(ui: &mut egui::Ui, row: &Row, w: f32) -> egui::Response {
    let (rect, resp) = ui.allocate_exact_size(egui::vec2(w, 66.0), egui::Sense::click());
    let p = ui.painter();
    let card = egui::CornerRadius::same(16);
    p.rect_filled(
        rect,
        card,
        if resp.hovered() || resp.is_pointer_button_down_on() {
            theme::SURFACE_HOVER
        } else {
            ROW_CARD
        },
    );
    if row.kind == ChatKind::Here {
        paint_pinned_wash(p, rect);
    }
    p.rect_stroke(
        rect,
        card,
        egui::Stroke::new(1.0, GLINE),
        egui::StrokeKind::Inside,
    );
    let face = egui::pos2(rect.left() + 12.0 + 14.0, rect.top() + 12.0 + 14.0);
    paint_avatar(p, face, &row.name, row.kind, 28.0);
    let mut name_room = rect.right() - face.x - 30.0;
    if row.unread > 0 {
        let g = p.layout_no_wrap(
            if row.unread > 99 {
                "99+".into()
            } else {
                row.unread.to_string()
            },
            egui::FontId::proportional(11.5),
            egui::Color32::WHITE,
        );
        let pill = egui::Rect::from_min_size(
            egui::pos2(rect.right() - 10.0 - g.size().x - 14.0, rect.top() + 10.0),
            egui::vec2(g.size().x + 14.0, 19.0),
        );
        p.rect_filled(pill, egui::CornerRadius::same(10), theme::BRAND);
        let gs = g.size();
        p.galley(pill.center() - gs / 2.0, g, egui::Color32::WHITE);
        name_room -= pill.width() + 6.0;
    }
    paint_line(
        p,
        egui::pos2(face.x + 22.0, face.y - 9.5),
        &row.name,
        14.5,
        theme::CREAM,
        name_room,
    );
    paint_line(
        p,
        egui::pos2(rect.left() + 12.0, rect.bottom() - 24.0),
        &row.preview,
        12.5,
        theme::TEXT_DIM,
        rect.width() - 24.0,
    );
    resp
}

/// Which of the "+" sheet's forms is open.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub enum NewKind {
    /// The four choices.
    #[default]
    Choose,
    Person,
    Group,
}

/// One big choice on the "+" sheet: an icon, what it is, one line on it.
fn new_choice(ui: &mut egui::Ui, icon: &str, title: &str, line: &str, brand: bool) -> bool {
    let id = ui.next_auto_id().with("new_choice");
    let hovered = ui.ctx().read_response(id).is_some_and(|r| r.hovered());
    let inner = egui::Frame::NONE
        .fill(if hovered {
            theme::SURFACE_HOVER
        } else {
            egui::Color32::TRANSPARENT
        })
        .corner_radius(egui::CornerRadius::same(12))
        .inner_margin(egui::Margin::symmetric(6, 6))
        .show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.horizontal(|ui| {
                let (rect, _) =
                    ui.allocate_exact_size(egui::Vec2::splat(42.0), egui::Sense::hover());
                ui.painter()
                    .rect_filled(rect, egui::CornerRadius::same(12), theme::SURFACE_RAISED);
                ui.painter().text(
                    rect.center(),
                    egui::Align2::CENTER_CENTER,
                    icon,
                    egui::FontId::proportional(20.0),
                    if brand { theme::BRAND } else { theme::ACCENT },
                );
                ui.vertical(|ui| {
                    ui.spacing_mut().item_spacing.y = 1.0;
                    ui.label(egui::RichText::new(title).size(15.0).color(theme::CREAM));
                    ui.add(
                        egui::Label::new(
                            egui::RichText::new(line).size(12.5).color(theme::TEXT_DIM),
                        )
                        .wrap(),
                    );
                });
            });
        });
    ui.interact(inner.response.rect, id, egui::Sense::click())
        .clicked()
}

/// The "+" sheet: one question — a person, a group, or a community — and then
/// the form for whichever was picked. It rises over the list with the list
/// dimmed behind it, and a tap on the dimmed list closes it
/// (`docs/mockups/home-and-profile/v1`, B); it used to open inside the list
/// and push it down. `lift` keeps a form clear of the on-screen keys. Answers
/// whether it should close.
fn new_sheet(
    ctx: &egui::Context,
    panel: egui::Rect,
    lift: f32,
    chat: &mut ChatState,
    kind: &mut NewKind,
    act: &mut Actions,
) -> bool {
    let mut close = false;
    // One area for the scrim and the sheet, so the sheet is always the one on
    // top: two areas at one order are stacked by egui's last-touched rule.
    egui::Area::new(egui::Id::new("chats_new_sheet"))
        .order(egui::Order::Middle)
        .fixed_pos(panel.min)
        .show(ctx, |ui| {
            let scrim = ui.interact(
                panel,
                egui::Id::new("chats_new_scrim"),
                egui::Sense::click(),
            );
            ui.painter()
                .rect_filled(panel, 0.0, egui::Color32::from_black_alpha(150));
            if scrim.clicked() {
                close = true;
            }
            // The sheet sits on the panel's bottom edge, at the height it
            // measured itself last frame (a sheet's height is only known once
            // drawn, and a frame of it a little off is never seen).
            let height_id = egui::Id::new("chats_new_sheet_height");
            let height = ui
                .ctx()
                .data(|d| d.get_temp::<f32>(height_id))
                .unwrap_or(panel.height() * 0.5)
                .min(panel.height());
            let bottom = panel.bottom() - lift;
            let room = egui::Rect::from_min_max(
                egui::pos2(panel.left(), (bottom - height).max(panel.top())),
                egui::pos2(panel.right(), bottom),
            );
            ui.scope_builder(
                egui::UiBuilder::new()
                    .max_rect(room)
                    .layout(egui::Layout::top_down(egui::Align::Min)),
                |ui| {
                    // Taps on the sheet itself stay on it. Registered before
                    // the rows, because egui hands a tap to the last widget
                    // under it: registered after, this took every tap and no
                    // choice on the sheet could be picked.
                    let _ = ui.interact(
                        room,
                        egui::Id::new("chats_new_sheet_body"),
                        egui::Sense::click(),
                    );
                    let sheet = egui::Frame::NONE
                        .fill(theme::SURFACE)
                        .stroke(egui::Stroke::new(1.0, GLINE))
                        .corner_radius(egui::CornerRadius {
                            nw: 24,
                            ne: 24,
                            sw: 0,
                            se: 0,
                        })
                        .inner_margin(egui::Margin {
                            left: 16,
                            right: 16,
                            top: 8,
                            bottom: 22,
                        })
                        .show(ui, |ui| {
                            ui.set_width(room.width() - 32.0);
                            let (handle, _) = ui.allocate_exact_size(
                                egui::vec2(ui.available_width(), 14.0),
                                egui::Sense::hover(),
                            );
                            ui.painter().rect_filled(
                                egui::Rect::from_center_size(
                                    handle.center(),
                                    egui::vec2(40.0, 5.0),
                                ),
                                egui::CornerRadius::same(3),
                                theme::LINE,
                            );
                            new_sheet_body(ui, chat, kind, act);
                        });
                    let h = sheet.response.rect.height();
                    ui.ctx().data_mut(|d| d.insert_temp(height_id, h));
                },
            );
        });
    if act.community_find || act.community_start || act.start_private.is_some() {
        close = true;
    }
    close
}

fn new_sheet_body(ui: &mut egui::Ui, chat: &mut ChatState, kind: &mut NewKind, act: &mut Actions) {
    match *kind {
        NewKind::Choose => {
            ui.label(egui::RichText::new("New").size(18.0).color(theme::CREAM));
            ui.add_space(6.0);
            if new_choice(
                ui,
                icons::CHAT,
                "Message a person",
                "From your contacts, or by name",
                false,
            ) {
                *kind = NewKind::Person;
            }
            if new_choice(
                ui,
                icons::GROUP,
                "Start a group",
                "A few people you pick",
                false,
            ) {
                *kind = NewKind::Group;
            }
            if new_choice(
                ui,
                icons::SEARCH,
                "Find a community",
                "Clubs, teams, neighbourhoods",
                true,
            ) {
                act.community_find = true;
            }
            if new_choice(
                ui,
                icons::CAT_CITY,
                "Start a community",
                "Several topics, one set of people",
                true,
            ) {
                act.community_start = true;
            }
            ui.add_space(6.0);
            theme::note(
                ui,
                &format!("{} All four are end-to-end encrypted.", icons::LOCK),
            );
        }
        NewKind::Person => {
            ui.label(
                egui::RichText::new("Message a person")
                    .size(18.0)
                    .color(theme::CREAM),
            );
            ui.add_space(4.0);
            let field = crate::systems::text_touch::field(
                ui,
                egui::TextEdit::singleline(&mut chat.private_input)
                    .id(egui::Id::new("chats_new_private"))
                    .desired_width(ui.available_width())
                    .hint_text("Username"),
            );
            let entered = field.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter));
            let ready = !chat.private_input.trim().is_empty();
            ui.horizontal(|ui| {
                if theme::action_button(ui, "Back").clicked() {
                    *kind = NewKind::Choose;
                }
                if theme::action_button(ui, "Contacts").clicked() {
                    act.sub = Some(crate::systems::contacts::Sub::Contacts);
                }
                if (theme::action_enabled_because(ui, ready, "Start", "Type who to write to first")
                    .clicked()
                    || entered)
                    && ready
                {
                    act.start_private = Some(chat.private_input.trim().to_string());
                }
            });
        }
        NewKind::Group => {
            ui.label(
                egui::RichText::new("Start a group")
                    .size(18.0)
                    .color(theme::CREAM),
            );
            ui.add_space(4.0);
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
            let ready = !chat.group_name_input.trim().is_empty()
                && !chat.group_members_input.trim().is_empty();
            ui.horizontal(|ui| {
                if theme::action_button(ui, "Back").clicked() {
                    *kind = NewKind::Choose;
                }
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
            });
        }
    }
}

/// The You page, from your disc at the top of the list: who you are, your
/// link, your avatar, your contacts and requests, and the way to Settings,
/// Admin (for an account that may run something; `admin` is how many reports
/// wait) and out (`docs/mockups/home-and-profile/v1`, C). Everything on it existed
/// before, in four different places.
fn you_screen(
    ui: &mut egui::Ui,
    me: &str,
    address: &str,
    contacts: &crate::systems::contacts::Contacts,
    admin: Option<usize>,
    faces: &mut crate::systems::faces::Faces,
    act: &mut Actions,
) {
    use crate::systems::contacts::Sub;
    if theme::back_title(ui, "You") {
        act.you = Some(false);
    }
    egui::ScrollArea::vertical()
        .auto_shrink([false, false])
        .show(ui, |ui| {
            ui.vertical_centered(|ui| {
                // Your disc, or the picture you are about to make it, and the
                // ways to make one (`systems::faces`).
                crate::systems::faces::you_picture(ui, faces, me, &mut act.panel);
                ui.add_space(8.0);
                ui.label(egui::RichText::new(me).size(22.0).color(theme::CREAM));
                ui.label(
                    egui::RichText::new(address)
                        .size(13.5)
                        .color(theme::TEXT_DIM),
                );
                ui.add_space(12.0);
                // The two pills, centred as a pair.
                let pill = |ui: &mut egui::Ui, icon: &str, text: &str| {
                    ui.add(
                        egui::Button::new(
                            egui::RichText::new(format!("{icon}  {text}"))
                                .size(13.5)
                                .color(theme::TEXT),
                        )
                        .fill(theme::SURFACE_RAISED)
                        .stroke(egui::Stroke::new(1.0, GLINE))
                        .corner_radius(egui::CornerRadius::same(12))
                        .min_size(egui::vec2(0.0, 36.0)),
                    )
                };
                let w = 300.0_f32.min(ui.available_width());
                ui.allocate_ui_with_layout(
                    egui::vec2(w, 40.0),
                    egui::Layout::left_to_right(egui::Align::Center),
                    |ui| {
                        ui.add_space(((w - 250.0) / 2.0).max(0.0));
                        if pill(ui, icons::SHARE, "Share my link").clicked() {
                            act.sub = Some(Sub::Add);
                            act.sub_root = Some(Sub::Add);
                        }
                        if pill(ui, icons::APPEARANCE, "Avatar").clicked() {
                            act.panel = Some(RightPanel::Appearance);
                        }
                    },
                );
            });
            ui.add_space(14.0);
            theme::list_group(ui, |ui| {
                if theme::list_row(
                    ui,
                    icons::GROUP,
                    "Contacts",
                    None,
                    theme::RowEnd::Count(contacts.contacts.len()),
                    theme::ACCENT,
                )
                .clicked()
                {
                    act.sub = Some(Sub::Contacts);
                    act.sub_root = Some(Sub::Contacts);
                }
                if theme::list_row(
                    ui,
                    icons::CHAT,
                    "Message requests",
                    None,
                    theme::RowEnd::Badge(contacts.incoming.len()),
                    theme::BRAND,
                )
                .clicked()
                {
                    act.sub = Some(Sub::Requests);
                    act.sub_root = Some(Sub::Requests);
                }
                if theme::list_row(
                    ui,
                    icons::LOCK,
                    "Who can message me",
                    Some(crate::systems::contacts::policy_word(
                        contacts.settings.policy,
                    )),
                    theme::RowEnd::Chevron,
                    theme::ACCENT,
                )
                .clicked()
                {
                    act.sub = Some(Sub::Privacy);
                    act.sub_root = Some(Sub::Privacy);
                }
            });
            theme::list_group(ui, |ui| {
                if theme::list_row(
                    ui,
                    icons::SETTINGS,
                    "Settings",
                    None,
                    theme::RowEnd::Chevron,
                    theme::ACCENT,
                )
                .clicked()
                {
                    act.panel = Some(RightPanel::Settings);
                    act.settings_from_you = true;
                }
                if let Some(waiting) = admin {
                    if theme::list_row(
                        ui,
                        RightPanel::Admin.icon(),
                        RightPanel::Admin.label(),
                        None,
                        theme::RowEnd::Badge(waiting),
                        theme::WARN,
                    )
                    .clicked()
                    {
                        act.panel = Some(RightPanel::Admin);
                    }
                }
                if theme::list_row(
                    ui,
                    icons::LOGOUT,
                    "Sign out",
                    None,
                    theme::RowEnd::Nothing,
                    theme::ACCENT,
                )
                .clicked()
                {
                    act.sign_out = true;
                }
            });
        });
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
    topic: Option<&TopicContext>,
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
            } else if let Some(t) = topic {
                // A topic says which community it is in, and opens it.
                if ui
                    .add(
                        egui::Label::new(
                            egui::RichText::new(format!("{} {}", t.sub, icons::CHEVRON_RIGHT))
                                .size(12.0)
                                .color(theme::TEXT_DIM),
                        )
                        .sense(egui::Sense::click()),
                    )
                    .on_hover_cursor(egui::CursorIcon::PointingHand)
                    .clicked()
                {
                    act.show_community = Some(t.community);
                }
            } else {
                ui.label(
                    egui::RichText::new(subtitle(kind))
                        .size(12.0)
                        .color(theme::TEXT_DIM),
                );
            }
        });
        // A private chat can be called. Encrypted like its lines, so the
        // button lives only where the padlock does. It is the call screen's own
        // round key (`theme::call_button`) — green to call or join, red to hang
        // up — so the header and the call it opens speak one language; the web
        // chat's header draws the same keys.
        let key_of = match call {
            CallButton::Hidden => None,
            CallButton::Call => Some((icons::CALL, "Call", theme::CallTone::Accept)),
            CallButton::Join => Some((icons::CALL, "Join the call", theme::CallTone::Accept)),
            CallButton::HangUp => Some((icons::CALL_END, "Hang up", theme::CallTone::HangUp)),
        };
        if let (Some((icon, tip, tone)), Some(group)) =
            (key_of, crate::systems::private_chat::group_of(key))
        {
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                // A call going on without you says so beside its key: the key
                // alone would read as "start one".
                let clicked = theme::call_button(ui, icon, tip, tone, HEADER_CALL_KEY).clicked();
                if call == CallButton::Join {
                    ui.label(
                        egui::RichText::new("Join")
                            .size(theme::SIZE_SUB)
                            .color(theme::CALL_ACCEPT_GREEN),
                    );
                }
                if clicked {
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
    topic: Option<TopicContext>,
    admin: bool,
) {
    let Some(channel) = chat.channels.iter().find(|c| c.key == key) else {
        act.back = true;
        return;
    };
    let kind = if topic.is_some() {
        ChatKind::Topic
    } else {
        kind_of(key)
    };
    let name = topic
        .as_ref()
        .map_or_else(|| display_name(key, &channel.label), |t| t.name.clone());
    let lines: Vec<ChatLine> = channel
        .lines
        .iter()
        .filter(|l| !chat.is_blocked(&l.sender))
        .cloned()
        .collect();
    chat_header(ui, chat, key, &name, kind, act, now, call, topic.as_ref());
    ui.add_space(8.0);
    // A chat's own Chat | World: its thread, or the map with its points.
    let w = ui.available_width();
    if segmented(ui, "Chat", w, Seg::Community, 0, false) == Some(Seg::World) {
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
        .frame(egui::Frame::NONE.fill(theme::PANEL))
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
            composer(ui, chat, view, key, kind_of(key), act, now);
            ui.add_space(6.0 + lift);
        });

    let group = !matches!(kind, ChatKind::Private) || !channel_is_direct(chat, key);
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
            let mut told_shared = false;
            for line in &lines {
                // History from before this device joined is somebody's copy,
                // not the log, and says so once, above the first of it.
                if let (Some(by), false) = (&line.shared_by, told_shared) {
                    told_shared = true;
                    ui.vertical_centered(|ui| {
                        theme::note(
                            ui,
                            &format!("Earlier messages, shared by {by} when you joined"),
                        );
                    });
                }
                // `.day`: a small chip wherever the date changes ("Today").
                if line.at_ms > 0 {
                    let day = day_label(line.at_ms, now);
                    if day != last_day {
                        day_chip(ui, &day);
                        last_day = day;
                    }
                }
                // What a meeting left is a card; an answer or a vote is counted
                // on what it answers and drawn nowhere itself.
                if let Some(meeting) = &line.meeting {
                    if meeting.is_line() {
                        meeting_card(ui, chat, key, line, meeting, &lines, now, act);
                        ui.add_space(4.0);
                    }
                    continue;
                }
                // A meet-up is its own card, and an answer to one is a small
                // line under it rather than a second card at the same place.
                match line.point.as_ref().and_then(|p| p.meetup) {
                    Some(m) if m.going => {
                        going_chip(ui, chat, line);
                        ui.add_space(4.0);
                        continue;
                    }
                    Some(_) => {
                        number += 1;
                        meetup_card(ui, chat, key, line, &lines, now, act);
                        ui.add_space(4.0);
                        continue;
                    }
                    None => {}
                }
                let n = line.point.as_ref().map(|_| {
                    number += 1;
                    number
                });
                bubble(
                    ui,
                    chat,
                    line,
                    group,
                    n,
                    act,
                    pictures,
                    topic.as_ref(),
                    admin,
                );
                ui.add_space(4.0);
            }
        });
}

/// "Sanne is going", centred under the thread like a day chip: an answer to a
/// meet-up, which the meet-up's own card also counts.
fn going_chip(ui: &mut egui::Ui, chat: &ChatState, line: &ChatLine) {
    let who = if is_mine(chat, line) {
        "You are going".to_string()
    } else {
        format!("{} is going", line.sender)
    };
    ui.vertical_centered(|ui| {
        ui.label(
            egui::RichText::new(format!("{}  {who}", icons::CHECK))
                .size(12.0)
                .color(theme::REAL_LOCATION),
        );
    });
}

/// What a meeting left in the chat, as a card (`cartopolis_core::meeting`,
/// the web's `meetingCard`): a plan with its answers, a poll with its votes, a
/// note, a record. Answers and votes are counted from the thread, per person
/// the newest.
#[allow(clippy::too_many_arguments)]
fn meeting_card(
    ui: &mut egui::Ui,
    chat: &ChatState,
    key: &str,
    line: &ChatLine,
    meeting: &cartopolis_core::meeting::MeetingObject,
    thread: &[ChatLine],
    now: u64,
    act: &mut Actions,
) {
    use cartopolis_core::meeting::{MeetingObject, NoteKind, Rsvp};
    // Who said each thing: "You" for this device's own, so a person's newest
    // answer replaces their earlier one whichever device sent it.
    let who = |l: &ChatLine| {
        if is_mine(chat, l) {
            "You".to_string()
        } else {
            l.sender.clone()
        }
    };
    let said: Vec<(String, &MeetingObject)> = thread
        .iter()
        .filter_map(|l| l.meeting.as_ref().map(|m| (who(l), m)))
        .collect();
    let w = (ui.available_width() * 0.86).clamp(220.0, 360.0);
    let frame = |live: bool| {
        egui::Frame::NONE
            .fill(theme::SURFACE_RAISED)
            .corner_radius(egui::CornerRadius::same(18))
            .stroke(egui::Stroke::new(
                if live { 2.0 } else { 1.0 },
                if live {
                    theme::ACCENT
                } else {
                    theme::OVERLAY_LINE
                },
            ))
            .inner_margin(egui::Margin::same(12))
    };
    let kicker = |ui: &mut egui::Ui, icon: &str, text: &str| {
        ui.label(
            egui::RichText::new(format!("{icon}  {}", text.to_uppercase()))
                .size(11.0)
                .strong()
                .color(theme::TEXT_DIM),
        );
    };
    let title = |ui: &mut egui::Ui, text: &str| {
        ui.label(
            egui::RichText::new(text)
                .size(15.5)
                .strong()
                .color(theme::CREAM),
        );
    };
    match meeting {
        MeetingObject::Plan(first) => {
            // As its planner last left it: a date skipped, the series ended.
            let plan = &cartopolis_core::meeting::current_plan(
                first,
                &line.sender,
                thread
                    .iter()
                    .filter_map(|l| l.meeting.as_ref().map(|m| (l.sender.as_str(), m))),
            );
            let offset = &crate::systems::calendar::offset_s;
            // The date the card is about: the next one not over, else the
            // series' last.
            let date = plan.next_date(now, offset).unwrap_or_else(|| {
                plan.dates(plan.start_ms, now, offset)
                    .last()
                    .copied()
                    .unwrap_or(plan.start_ms)
            });
            let answers = cartopolis_core::meeting::tally_rsvps(
                plan,
                date,
                said.iter().map(|(w, m)| (w.as_str(), *m)),
            );
            let mine = answers.iter().find(|(w, _)| w == "You").map(|(_, r)| *r);
            let going: Vec<&str> = answers
                .iter()
                .filter(|(_, r)| *r == Rsvp::Going)
                .map(|(w, _)| w.as_str())
                .collect();
            let end = date + plan.duration_ms();
            let live = now + 15 * 60_000 >= date && now <= end;
            frame(live).show(ui, |ui| {
                ui.set_width(w);
                let when = crate::systems::meetup::when_label(date, now, local_offset_s(date));
                kicker(ui, icons::EVENT, &format!("Meeting · {when}"));
                title(ui, &plan.title);
                ui.label(
                    egui::RichText::new(format!("{}–{}", clock_time(date), clock_time(end)))
                        .size(theme::SIZE_SUB)
                        .color(theme::TEXT_DIM),
                );
                if let Some(rule) = crate::systems::calendar::rule_words(plan) {
                    let next = if date != plan.start_ms || plan.next_date(now, offset).is_some() {
                        format!(" · next {}", crate::systems::calendar::short_day(date))
                    } else {
                        String::new()
                    };
                    ui.label(
                        egui::RichText::new(format!("{rule}{next}"))
                            .size(theme::SIZE_SUB)
                            .color(theme::ACCENT),
                    );
                }
                if let Some(text) = &plan.text {
                    ui.label(egui::RichText::new(text).size(13.0).color(theme::TEXT));
                }
                for (i, item) in plan.agenda.iter().enumerate() {
                    let minutes = if item.minutes > 0 {
                        format!("  · {} min", item.minutes)
                    } else {
                        String::new()
                    };
                    ui.label(
                        egui::RichText::new(format!("{}. {}{minutes}", i + 1, item.name))
                            .size(13.0)
                            .color(theme::TEXT),
                    );
                }
                if !going.is_empty() {
                    ui.add_space(4.0);
                    ui.label(
                        egui::RichText::new(format!("{} going", going.join(", ")))
                            .size(12.5)
                            .color(theme::TEXT_DIM),
                    );
                }
                ui.add_space(8.0);
                if live && crate::systems::private_chat::group_of(key).is_some() {
                    if theme::primary_button_sized(
                        ui,
                        &format!("{}  Join", icons::VIDEOCAM),
                        ui.available_width(),
                    )
                    .clicked()
                    {
                        if let Some(group) = crate::systems::private_chat::group_of(key) {
                            act.call = Some(crate::systems::calls::CallRequest::Join(group));
                        }
                    }
                    ui.add_space(4.0);
                }
                if now <= end {
                    ui.horizontal(|ui| {
                        for r in [Rsvp::Going, Rsvp::Maybe, Rsvp::No] {
                            let n = answers.iter().filter(|(_, a)| *a == r).count();
                            let label = if n > 0 {
                                format!("{} {n}", r.words())
                            } else {
                                r.words().to_string()
                            };
                            if theme::chip(ui, &label, mine == Some(r)).clicked() {
                                // On a series the card answers its next date.
                                let date = if plan.repeat.is_some() {
                                    cartopolis_core::meeting::AnswerFor::Date(date)
                                } else {
                                    cartopolis_core::meeting::AnswerFor::First
                                };
                                act.meeting = Some((
                                    key.to_string(),
                                    MeetingObject::Answer {
                                        plan: plan.id.clone(),
                                        rsvp: r,
                                        date,
                                    },
                                ));
                            }
                        }
                    });
                }
                // Its planner can take the next date out, or end the series
                // with it (`docs/mockups/calendar/v2-repeat`, K).
                let last = plan.repeat.and_then(|r| r.until_ms) == Some(date);
                if is_mine(chat, line) && plan.repeat.is_some() && now <= end {
                    ui.horizontal_wrapped(|ui| {
                        let day = crate::systems::calendar::short_day(date);
                        if theme::action_button(ui, format!("Skip {day}")).clicked() {
                            let mut p = plan.clone();
                            p.skip.push(date);
                            act.meeting = Some((key.to_string(), MeetingObject::Update(p)));
                        }
                        if !last && theme::action_button(ui, format!("Stop after {day}")).clicked()
                        {
                            let mut p = plan.clone();
                            if let Some(r) = p.repeat.as_mut() {
                                r.until_ms = Some(date);
                            }
                            p.skip.retain(|d| *d <= date);
                            act.meeting = Some((key.to_string(), MeetingObject::Update(p)));
                        }
                    });
                }
            });
        }
        MeetingObject::Update(after) => {
            // Said once, as a line: what the planner changed. An update from
            // anybody else changes nothing, so it says nothing either.
            let at = thread
                .iter()
                .position(|l| std::ptr::eq(l, line))
                .unwrap_or(thread.len());
            let original = thread.iter().find_map(|l| match &l.meeting {
                Some(MeetingObject::Plan(p)) if p.id == after.id => Some((l, p)),
                _ => None,
            });
            let text = match original {
                Some((planned, _)) if planned.sender != line.sender => return,
                Some((planned, p)) => {
                    let before = cartopolis_core::meeting::current_plan(
                        p,
                        &planned.sender,
                        thread[..at]
                            .iter()
                            .filter_map(|l| l.meeting.as_ref().map(|m| (l.sender.as_str(), m))),
                    );
                    cartopolis_core::meeting::PlanChange::between(&before, after)
                        .words(&after.title, &crate::systems::calendar::long_day)
                }
                None => format!("changed {}", after.title),
            };
            let who = if is_mine(chat, line) {
                "You".to_string()
            } else {
                line.sender.clone()
            };
            ui.vertical_centered(|ui| {
                ui.label(
                    egui::RichText::new(format!("{}  {who} {text}", icons::EVENT))
                        .size(12.0)
                        .color(theme::TEXT_DIM),
                );
            });
        }
        MeetingObject::Poll(poll) => {
            let counted = cartopolis_core::meeting::tally_votes(
                poll,
                said.iter().map(|(w, m)| (w.as_str(), *m)),
            );
            let mine: Vec<String> = counted
                .iter()
                .filter(|(_, who)| who.iter().any(|w| w == "You"))
                .map(|(o, _)| o.clone())
                .collect();
            let voters = {
                let mut v: Vec<&String> = counted.iter().flat_map(|(_, w)| w).collect();
                v.sort();
                v.dedup();
                v.len()
            };
            frame(false).show(ui, |ui| {
                ui.set_width(w);
                kicker(
                    ui,
                    icons::POLL,
                    if poll.multiple {
                        "Poll · choose any"
                    } else {
                        "Poll · choose one"
                    },
                );
                title(ui, &poll.question);
                ui.add_space(4.0);
                for (option, who) in &counted {
                    let picked = mine.contains(option);
                    let (rect, response) = ui.allocate_exact_size(
                        egui::vec2(ui.available_width(), 34.0),
                        egui::Sense::click(),
                    );
                    let p = ui.painter();
                    p.rect(
                        rect,
                        egui::CornerRadius::same(10),
                        theme::SURFACE,
                        egui::Stroke::new(
                            1.0,
                            if picked {
                                theme::ACCENT
                            } else {
                                theme::OVERLAY_LINE
                            },
                        ),
                        egui::StrokeKind::Inside,
                    );
                    let share = if voters > 0 {
                        who.len() as f32 / voters as f32
                    } else {
                        0.0
                    };
                    if share > 0.0 {
                        let mut bar = rect.shrink(1.0);
                        bar.set_width(bar.width() * share);
                        p.rect_filled(
                            bar,
                            egui::CornerRadius::same(9),
                            theme::ACCENT.gamma_multiply(0.22),
                        );
                    }
                    let mark = if picked {
                        format!("{}  ", icons::CHECK)
                    } else {
                        String::new()
                    };
                    p.text(
                        rect.left_center() + egui::vec2(12.0, 0.0),
                        egui::Align2::LEFT_CENTER,
                        format!("{mark}{option}"),
                        egui::FontId::proportional(13.5),
                        theme::TEXT,
                    );
                    p.text(
                        rect.right_center() - egui::vec2(12.0, 0.0),
                        egui::Align2::RIGHT_CENTER,
                        who.len().to_string(),
                        egui::FontId::proportional(13.0),
                        theme::TEXT_DIM,
                    );
                    if response.clicked() {
                        let choices = if poll.multiple {
                            let mut c = mine.clone();
                            if picked {
                                c.retain(|x| x != option);
                            } else {
                                c.push(option.clone());
                            }
                            c
                        } else if picked {
                            Vec::new()
                        } else {
                            vec![option.clone()]
                        };
                        act.meeting = Some((
                            key.to_string(),
                            MeetingObject::Vote {
                                poll: poll.id.clone(),
                                choices,
                            },
                        ));
                    }
                    ui.add_space(4.0);
                }
                ui.label(
                    egui::RichText::new(format!(
                        "{voters} {} · everybody in the chat sees who chose what",
                        if voters == 1 { "vote" } else { "votes" }
                    ))
                    .size(12.0)
                    .color(theme::TEXT_DIM),
                );
            });
        }
        MeetingObject::Note { kind, text } => {
            ui.horizontal_wrapped(|ui| {
                ui.set_max_width(w);
                let tint = match kind {
                    NoteKind::Decision => theme::ACCENT,
                    NoteKind::Action => theme::WARN,
                    NoteKind::Note => theme::TEXT_DIM,
                };
                ui.label(
                    egui::RichText::new(kind.label().to_uppercase())
                        .size(11.0)
                        .strong()
                        .color(tint),
                );
                ui.label(
                    egui::RichText::new(format!("{text} · {}", who(line)))
                        .size(13.5)
                        .color(theme::TEXT),
                );
            });
        }
        MeetingObject::Record(r) => {
            frame(false).show(ui, |ui| {
                ui.set_width(w);
                let mins = (r.end_ms.saturating_sub(r.start_ms) / 60_000).max(1);
                kicker(ui, icons::RECORD, "Meeting record");
                title(
                    ui,
                    if r.title.is_empty() {
                        "Meeting record"
                    } else {
                        &r.title
                    },
                );
                ui.label(
                    egui::RichText::new(format!(
                        "{}–{} · {mins} min · {}",
                        clock_time(r.start_ms),
                        clock_time(r.end_ms),
                        r.people.join(", ")
                    ))
                    .size(theme::SIZE_SUB)
                    .color(theme::TEXT_DIM),
                );
                for kind in [NoteKind::Decision, NoteKind::Action, NoteKind::Note] {
                    let notes: Vec<_> = r.notes.iter().filter(|n| n.kind == kind).collect();
                    if notes.is_empty() {
                        continue;
                    }
                    ui.add_space(4.0);
                    ui.label(
                        egui::RichText::new(format!("{}s", kind.label()))
                            .size(12.5)
                            .strong()
                            .color(theme::CREAM),
                    );
                    for n in notes {
                        ui.label(
                            egui::RichText::new(format!("•  {}", n.text))
                                .size(13.0)
                                .color(theme::TEXT),
                        );
                    }
                }
                for p in &r.polls {
                    ui.add_space(4.0);
                    ui.label(
                        egui::RichText::new(&p.question)
                            .size(12.5)
                            .strong()
                            .color(theme::CREAM),
                    );
                    for (o, n) in &p.results {
                        ui.label(
                            egui::RichText::new(format!("{o}: {n}"))
                                .size(13.0)
                                .color(theme::TEXT),
                        );
                    }
                }
                if !r.transcript.is_empty() {
                    ui.add_space(4.0);
                    egui::CollapsingHeader::new(format!(
                        "Transcript · {} lines",
                        r.transcript.len()
                    ))
                    .id_salt(("record", line.at_ms, line.log_seq))
                    .show(ui, |ui| {
                        for (who, text) in &r.transcript {
                            ui.label(
                                egui::RichText::new(format!("{who}: {text}"))
                                    .size(12.5)
                                    .color(theme::TEXT),
                            );
                        }
                    });
                }
                ui.add_space(6.0);
                if theme::secondary_button(ui, icons::COPY, "Copy as text", 0.0).clicked() {
                    ui.ctx().copy_text(format!(
                        "{}\n{}",
                        r.title,
                        cartopolis_core::meeting::record_text(r)
                    ));
                }
            });
        }
        // Counted on what they answer (`is_line`).
        MeetingObject::Answer { .. } | MeetingObject::Vote { .. } => {}
    }
}

/// Who is going to a meet-up and how far each is from it, as the card lists
/// them: `(name, status)`.
///
/// The proposer counts as going. A person's distance comes from the location
/// they are sharing with this chat — which "I'm going" turns on before the start
/// — measured on this phone; somebody going who is not sharing (yet) reads as
/// "going", and nothing more.
fn meetup_people(
    chat: &ChatState,
    key: &str,
    proposal: &ChatLine,
    thread: &[ChatLine],
) -> Vec<(String, String)> {
    let Some(place) = proposal.point.as_ref() else {
        return Vec::new();
    };
    let mut names: Vec<(String, bool)> = vec![(proposal.sender.clone(), is_mine(chat, proposal))];
    for l in thread {
        let answers = l
            .point
            .as_ref()
            .is_some_and(|p| p.meetup.is_some_and(|m| m.going) && p.same_meetup(place));
        if answers && !names.iter().any(|(n, _)| *n == l.sender) {
            names.push((l.sender.clone(), is_mine(chat, l)));
        }
    }
    // An answer this phone sent but has not seen echoed back yet.
    if chat
        .meetups_going
        .iter()
        .any(|(k, p)| k == key && p.same_meetup(place))
        && !names.iter().any(|(_, mine)| *mine)
    {
        names.push(("You".into(), true));
    }
    names
        .into_iter()
        .map(|(name, mine)| {
            let status = if mine {
                "going".to_string()
            } else {
                chat.locations
                    .iter()
                    .find(|l| l.channel == key && l.from == name)
                    .map_or_else(
                        || "going".to_string(),
                        |l| {
                            let d = crate::systems::driver::metres_between(
                                [l.latitude, l.longitude],
                                [place.latitude, place.longitude],
                            );
                            if d < 60.0 {
                                "there".to_string()
                            } else {
                                format!("{} away", crate::systems::routing::format_distance(d))
                            }
                        },
                    )
            };
            (if mine { "You".to_string() } else { name }, status)
        })
        .collect()
}

/// A meet-up as the thread shows it: chat-world v4's card (screen 6) — where
/// and when, who is going and how far off they are, what "I'm going" will share,
/// and the two things to do about it.
fn meetup_card(
    ui: &mut egui::Ui,
    chat: &ChatState,
    key: &str,
    line: &ChatLine,
    thread: &[ChatLine],
    now: u64,
    act: &mut Actions,
) {
    let Some(point) = line.point.as_ref() else {
        return;
    };
    let Some(m) = point.meetup else {
        return;
    };
    let people = meetup_people(chat, key, line, thread);
    let im_going = people.iter().any(|(n, _)| n == "You");
    let place = point
        .name
        .clone()
        .unwrap_or_else(|| "this spot".to_string());
    let when = crate::systems::meetup::when_label(m.start_ms, now, local_offset_s(m.start_ms));
    let by = if is_mine(chat, line) {
        "you suggested it".to_string()
    } else {
        format!("suggested by {}", line.sender)
    };
    let w = (ui.available_width() * 0.86).clamp(220.0, 330.0);
    egui::Frame::NONE
        .fill(theme::SURFACE_RAISED)
        .corner_radius(egui::CornerRadius::same(18))
        .stroke(egui::Stroke::new(1.0, theme::OVERLAY_LINE))
        .inner_margin(egui::Margin::same(12))
        .show(ui, |ui| {
            ui.set_width(w);
            ui.label(
                egui::RichText::new(format!("{}  Meet at {place}", icons::FLAG))
                    .size(15.5)
                    .strong()
                    .color(theme::CREAM),
            );
            ui.label(
                egui::RichText::new(format!("{when} · {by}"))
                    .size(12.5)
                    .color(theme::TEXT_DIM),
            );
            ui.add_space(6.0);
            for (name, status) in &people {
                ui.horizontal(|ui| {
                    ui.label(egui::RichText::new(name).size(13.0).color(theme::TEXT));
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        ui.label(egui::RichText::new(status).size(12.0).color(
                            if status == "going" {
                                theme::TEXT_DIM
                            } else {
                                theme::REAL_LOCATION
                            },
                        ));
                    });
                });
            }
            if !im_going {
                ui.add_space(6.0);
                let from = crate::systems::meetup::when_label(
                    m.start_ms
                        .saturating_sub(crate::systems::chat::MEETUP_SHARE_BEFORE_MS),
                    now,
                    local_offset_s(m.start_ms),
                );
                egui::Frame::NONE
                    .fill(theme::REAL_LOCATION.gamma_multiply(0.16))
                    .stroke(egui::Stroke::new(
                        1.0,
                        theme::REAL_LOCATION.gamma_multiply(0.4),
                    ))
                    .corner_radius(egui::CornerRadius::same(12))
                    .inner_margin(egui::Margin::symmetric(10, 8))
                    .show(ui, |ui| {
                        ui.set_width(ui.available_width());
                        ui.label(
                            egui::RichText::new(format!(
                                "Going shares your real location with this chat from {} \
                                 until you arrive.",
                                from.to_lowercase()
                            ))
                            .size(12.5)
                            .color(theme::TEXT),
                        );
                    });
            }
            ui.add_space(8.0);
            ui.horizontal(|ui| {
                let half = (ui.available_width() - ui.spacing().item_spacing.x) / 2.0;
                if !im_going {
                    if theme::primary_button_sized(
                        ui,
                        &format!("{}  I'm going", icons::CHECK),
                        half,
                    )
                    .clicked()
                    {
                        act.meetup_going = Some((key.to_string(), point.clone()));
                    }
                } else if theme::primary_button_sized(
                    ui,
                    &format!("{}  Open in world", icons::CAM_EARTH),
                    half,
                )
                .clicked()
                {
                    act.go_to = Some(point.clone());
                }
                if ui
                    .add_sized(
                        egui::vec2(half, theme::row_height()),
                        egui::Button::new(format!("{}  Route", icons::ROUTE)),
                    )
                    .clicked()
                {
                    act.route_to = Some((point.latitude, point.longitude, place.clone()));
                }
            });
        });
}

/// This device's UTC offset at `at_ms`, in seconds — the time zone the thread's
/// own clock times are printed in (`day_label`, `clock_time`).
fn local_offset_s(at_ms: u64) -> Option<i32> {
    let ts = jiff::Timestamp::from_millisecond(at_ms as i64).ok()?;
    Some(jiff::tz::TimeZone::system().to_offset(ts).seconds())
}

/// The meet-up being proposed in this chat, above its composer: where, when
/// (moved a quarter hour at a time), and Send.
fn meetup_draft(ui: &mut egui::Ui, chat: &ChatState, key: &str, now: u64, act: &mut Actions) {
    let Some((draft_key, point)) = chat.meetup_draft.as_ref() else {
        return;
    };
    if draft_key != key {
        return;
    }
    let Some(m) = point.meetup else {
        return;
    };
    raised_frame().show(ui, |ui| {
        ui.set_width(ui.available_width());
        let place = point
            .name
            .clone()
            .unwrap_or_else(|| "this spot".to_string());
        ui.label(
            egui::RichText::new(format!("{}  Meet at {place}", icons::FLAG))
                .size(15.0)
                .color(theme::CREAM),
        );
        let halfway = chat.locations.iter().any(|l| l.channel == key);
        theme::note(
            ui,
            if halfway {
                "Halfway along the walk between you."
            } else {
                "Where the map is now. Move the map and ask again for somewhere else."
            },
        );
        ui.horizontal(|ui| {
            if theme::action_button(ui, "−15 min").clicked() {
                act.meetup_shift = Some(-15);
            }
            ui.label(
                egui::RichText::new(crate::systems::meetup::when_label(
                    m.start_ms,
                    now,
                    local_offset_s(m.start_ms),
                ))
                .color(theme::TEXT),
            );
            if theme::action_button(ui, "+15 min").clicked() {
                act.meetup_shift = Some(15);
            }
        });
        ui.horizontal(|ui| {
            if theme::action_button(ui, "Cancel").clicked() {
                act.meetup_cancel = true;
            }
            if theme::primary_button_sized(ui, &format!("{}  Send", icons::SEND), 120.0).clicked() {
                act.meetup_send = true;
            }
        });
    });
    ui.add_space(6.0);
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
    topic: Option<&TopicContext>,
    admin: bool,
) {
    // A line a moderator deleted keeps its place, as a marker, so a reply to
    // it still reads and nobody thinks it was never there.
    if line.removed {
        ui.vertical_centered(|ui| {
            ui.label(
                egui::RichText::new("Message removed by a moderator")
                    .italics()
                    .size(theme::SIZE_SUB)
                    .color(theme::TEXT_DIM),
            );
        });
        return;
    }
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
                        // A camera picture's caption is its note; its pose line is
                        // for the agent (`cartopolis_core::photo_pose`).
                        let caption = crate::systems::private_chat::caption_of(&line.body)
                            .map(cartopolis_core::photo_pose::display_caption)
                            .filter(|c| !c.is_empty());
                        ui.add(
                            egui::Label::new(
                                egui::RichText::new(caption.as_deref().unwrap_or(" "))
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
            match point_card(ui, point, n, &line.body, mine, max_w) {
                Some(CardHit::Open) => act.go_to = Some(point.clone()),
                Some(CardHit::Route) => {
                    act.route_to = Some((
                        point.latitude,
                        point.longitude,
                        point.name.clone().unwrap_or_else(|| line.body.clone()),
                    ));
                }
                None => {}
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
                        let extras = crate::systems::chat::PersonExtras {
                            community: topic.map(|t| t.community_name.clone()),
                            moderate_topic: topic.filter(|t| t.moderates).map(|t| t.name.clone()),
                            admin,
                        };
                        let seq = line.log_seq;
                        person_menu(
                            &name,
                            &line.sender,
                            Some(&line.body),
                            &extras,
                            |a| match a {
                                PersonAction::Message => act.message = Some(line.sender.clone()),
                                PersonAction::Block => act.block = Some(line.sender.clone()),
                                PersonAction::Report(msg) => {
                                    act.report = Some((line.sender.clone(), msg));
                                }
                                PersonAction::ReportToModerators(msg) => {
                                    if let (Some(t), Some(seq)) = (topic, seq) {
                                        act.report_to_mods = Some((
                                            line.sender.clone(),
                                            msg,
                                            t.community,
                                            t.group.clone(),
                                            seq,
                                        ));
                                    }
                                }
                                PersonAction::DeleteForEveryone => {
                                    if let (Some(t), Some(seq)) = (topic, seq) {
                                        act.community_ops.push(
                                        crate::systems::communities::CommunityOp::ModerateDelete(
                                            t.group.clone(),
                                            seq,
                                        ),
                                    );
                                    }
                                }
                                PersonAction::RemoveFromTopic
                                | PersonAction::RemoveFromCommunity => {
                                    if let Some(t) = topic {
                                        let whole = matches!(a, PersonAction::RemoveFromCommunity);
                                        act.community_ops.push(
                                            crate::systems::communities::CommunityOp::Remove {
                                                id: t.community,
                                                who: line.sender.clone(),
                                                topic: (!whole).then(|| t.group.clone()),
                                                block: false,
                                            },
                                        );
                                    }
                                }
                                PersonAction::Suspend => {
                                    act.admin = Some((line.sender.clone(), false))
                                }
                                PersonAction::DeleteAccount => {
                                    act.admin = Some((line.sender.clone(), true));
                                }
                            },
                        );
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
pub(crate) const POLAROID_WHITE: egui::Color32 = egui::Color32::from_rgb(0xF4, 0xF2, 0xEC);
const POLAROID_EDGE: i8 = 10;
/// What is written on it, and the time in the corner.
pub(crate) const POLAROID_INK: egui::Color32 = egui::Color32::from_rgb(0x2A, 0x2E, 0x33);
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
) -> Option<CardHit> {
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
    // The first mockup's "Open in world" (`.card .go`) in brand orange, and
    // beside it Route, in chat-world v4's button row (`.btnrow`): a point is a
    // place to look at *and* a place to go. The whole card still answers a tap
    // with Open in world; only the Route button routes.
    let row = egui::Rect::from_min_max(
        egui::pos2(rect.left() + 12.0, rect.bottom() - 12.0 - GO_H),
        egui::pos2(rect.right() - 12.0, rect.bottom() - 12.0),
    );
    let route_w = (row.width() * 0.42).min(104.0);
    let go = egui::Rect::from_min_max(
        row.min,
        egui::pos2(row.right() - route_w - 8.0, row.bottom()),
    );
    let route = egui::Rect::from_min_max(egui::pos2(row.right() - route_w, row.top()), row.max);
    let pointer = resp.hover_pos();
    let on_route = pointer.is_some_and(|p| route.contains(p));
    p.rect_filled(
        go,
        egui::CornerRadius::same(10),
        if resp.hovered() && !on_route {
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
    p.rect_filled(
        route,
        egui::CornerRadius::same(10),
        if on_route {
            theme::SURFACE_HOVER.gamma_multiply(1.4)
        } else {
            theme::SURFACE_HOVER
        },
    );
    centred_row(
        &p,
        route,
        &[
            (icons::ROUTE.to_string(), 15.0, theme::TEXT),
            ("Route".to_string(), 13.5, theme::TEXT),
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
    let resp = resp
        .on_hover_cursor(egui::CursorIcon::PointingHand)
        .on_hover_text(if on_route {
            "Plan a route there"
        } else {
            "Look from where it was sent"
        });
    resp.clicked().then(|| {
        if resp
            .interact_pointer_pos()
            .is_some_and(|p| route.contains(p))
        {
            CardHit::Route
        } else {
            CardHit::Open
        }
    })
}

/// Which of a point card's two actions a tap asked for.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum CardHit {
    /// Look from where it was sent.
    Open,
    /// Plan a route there.
    Route,
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
        meetup_draft(ui, chat, key, now, act);
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
                if theme::action_button(ui, format!("{}  Route", icons::ROUTE))
                    .on_hover_text("Plan a route to them; it follows them as they move")
                    .clicked()
                {
                    act.route_to_person =
                        Some((key.to_string(), l.from.clone(), l.latitude, l.longitude));
                }
            });
        }
    }
    if view.attach {
        raised_frame().show(ui, |ui| {
            ui.set_width(ui.available_width());
            attach_menu(ui, view, key, kind, chat, act, now);
        });
    }
    // The form takes the composer's place: a phone has no room for both, and
    // a line typed under a half-written poll would be easy to send by mistake.
    if view.meeting_form.as_ref().is_some_and(|(k, _)| k == key) {
        meeting_form(ui, view, key, now, act);
    } else {
        compose_row(ui, view, key, "chats_message_field", act);
    }
}

/// The "+" menu: round buttons with a word under each, as everywhere else in
/// the app, one icon for one meaning (`docs/mockups/social-camera/`, "One icon,
/// one meaning"). It replaced a column of text buttons where "Send a picture"
/// (the camera app) and "Send a snapshot" (the map) wore the same camera:
/// Camera is now the app's own camera, whose In world mode is the snapshot,
/// and Upload picks a picture already on the phone.
fn attach_menu(
    ui: &mut egui::Ui,
    view: &mut ChatsView,
    key: &str,
    kind: ChatKind,
    chat: &ChatState,
    act: &mut Actions,
    now: u64,
) {
    if kind != ChatKind::Private {
        theme::note(
            ui,
            "Places and locations go in private chats only: this one is public.",
        );
        return;
    }
    let sharing = chat.sharing.iter().any(|s| s.channel == key);
    let camera = crate::systems::camera_screen::available();
    let upload = crate::platform::photo::offers(crate::platform::photo::Source::File);
    let mut entries: Vec<(&str, &str, &str, bool)> = Vec::new();
    if camera {
        entries.push((
            icons::CAMERA,
            "Camera",
            "Take a picture for this chat",
            false,
        ));
    }
    if upload {
        entries.push((
            icons::UPLOAD,
            "Upload",
            "A picture from this device. Encrypted here: only this chat can open it.",
            false,
        ));
    }
    entries.push((
        icons::PIN,
        "This view",
        "Send where the camera is looking, as a place people can tap",
        false,
    ));
    entries.push((
        icons::FLAG,
        "Meet up",
        "Propose a place and a time. Halfway between you when they are sharing \
         where they are; otherwise where the map is now.",
        false,
    ));
    entries.push((
        icons::EVENT,
        "Meeting",
        "A call in this chat at a time, with an agenda",
        false,
    ));
    entries.push((
        icons::POLL,
        "Poll",
        "A question with options; everybody here sees who chose what",
        false,
    ));
    if !sharing {
        entries.push((
            icons::HERE,
            "Location",
            "Share your real location with this chat for a while",
            view.share_open,
        ));
    }
    let cell = (ui.available_width() / 4.0).floor();
    let mut picked = None;
    for row in entries.chunks(4) {
        ui.horizontal(|ui| {
            ui.spacing_mut().item_spacing.x = 0.0;
            for (icon, label, hint, on) in row {
                ui.allocate_ui_with_layout(
                    egui::vec2(cell, theme::ROUND_BUTTON_SIZE + 22.0),
                    egui::Layout::top_down(egui::Align::Center),
                    |ui| {
                        if theme::round_button(ui, icon, hint, *on).clicked() {
                            picked = Some(*label);
                        }
                        ui.label(egui::RichText::new(*label).size(12.0).color(theme::TEXT));
                    },
                );
            }
        });
        ui.add_space(6.0);
    }
    match picked {
        Some("Camera") => {
            view.camera_for = Some(key.to_string());
            view.attach = false;
        }
        Some("Upload") => {
            view.upload_for = Some(key.to_string());
            view.attach = false;
        }
        Some("This view") => act.view_to = Some(key.to_string()),
        Some("Meet up") => act.meetup = Some(key.to_string()),
        Some("Meeting") => {
            view.meeting_form = Some((key.to_string(), MeetingForm::plan(now)));
            view.attach = false;
        }
        Some("Poll") => {
            view.meeting_form = Some((key.to_string(), MeetingForm::poll()));
            view.attach = false;
        }
        Some("Location") => view.share_open = !view.share_open,
        _ => {}
    }
    if view.share_open && !sharing {
        ui.horizontal_wrapped(|ui| {
            ui.label(egui::RichText::new("for").color(theme::TEXT_DIM));
            for (label, minutes) in [("15 min", 15u64), ("1 hour", 60), ("8 hours", 480)] {
                if theme::action_button(ui, label).clicked() {
                    act.share = Some((key.to_string(), minutes * 60_000));
                    view.share_open = false;
                }
            }
        });
        theme::note(ui, "Only this chat sees it, and it stops by itself.");
    }
}

/// `+ → Plan a meeting` and `+ → Ask a poll`, filled in above where the
/// composer was; posting seals the object into the chat (`act.meeting`).
/// The dates a plan starting at `start_ms` with this rule would have, for
/// the next two years: what Until steps through.
fn series_dates(start_ms: u64, every: Every, interval: u32) -> Vec<u64> {
    let series = cartopolis_core::meeting::Plan {
        start_ms,
        repeat: Some(cartopolis_core::meeting::Repeat {
            every,
            interval,
            until_ms: None,
        }),
        ..Default::default()
    };
    series.dates(
        start_ms,
        start_ms + 730 * 86_400_000,
        &crate::systems::calendar::offset_s,
    )
}

/// The series' last date on or before `until_ms`; `None` when there is none.
fn snap_until(start_ms: u64, every: Every, interval: u32, until_ms: u64) -> Option<u64> {
    series_dates(start_ms, every, interval)
        .into_iter()
        .take_while(|d| *d <= until_ms)
        .last()
}

/// Repeats and Until in the plan form (`docs/mockups/calendar/v2-repeat`, I):
/// how often, and the last date stepped one meeting at a time.
fn repeat_fields(
    ui: &mut egui::Ui,
    start_ms: u64,
    repeat: &mut Option<(Every, u32)>,
    until_ms: &mut Option<u64>,
) {
    ui.label(
        egui::RichText::new("Repeats")
            .size(theme::SIZE_SUB)
            .color(theme::TEXT_DIM),
    );
    ui.horizontal_wrapped(|ui| {
        for (label, rule) in [
            ("Never", None),
            ("Every week", Some((Every::Week, 1))),
            ("Every 2 weeks", Some((Every::Week, 2))),
            ("Every month", Some((Every::Month, 1))),
        ] {
            if theme::chip(ui, label, *repeat == rule).clicked() && *repeat != rule {
                *repeat = rule;
                *until_ms = None;
            }
        }
    });
    let Some((every, interval)) = *repeat else {
        return;
    };
    let dates = series_dates(start_ms, every, interval);
    // Where Until is in the series, by its date, after the start moved.
    let at = until_ms.and_then(|u| dates.iter().position(|d| *d >= u));
    ui.horizontal_wrapped(|ui| {
        ui.label(
            egui::RichText::new("Until")
                .size(theme::SIZE_SUB)
                .color(theme::TEXT_DIM),
        );
        let last = dates.len().saturating_sub(1);
        if theme::chip(ui, "No end", at.is_none()).clicked() {
            *until_ms = None;
        }
        match at {
            // An end starts four meetings in, and steps one meeting at a time.
            None => {
                if theme::action_button(ui, "Set an end").clicked() {
                    *until_ms = dates.get(3.min(last)).copied();
                }
            }
            Some(i) => {
                if theme::action_button(ui, "−1").clicked() {
                    *until_ms = dates.get(i.saturating_sub(1).max(1).min(last)).copied();
                }
                ui.label(
                    egui::RichText::new(format!(
                        "{} · {} times",
                        crate::systems::calendar::short_day(dates[i]),
                        i + 1
                    ))
                    .color(theme::TEXT),
                );
                if theme::action_button(ui, "+1").clicked() {
                    *until_ms = dates.get((i + 1).min(last)).copied();
                }
            }
        }
    });
}

fn meeting_form(ui: &mut egui::Ui, view: &mut ChatsView, key: &str, now: u64, act: &mut Actions) {
    let Some((_, form)) = view.meeting_form.as_mut() else {
        return;
    };
    let field = |ui: &mut egui::Ui, text: &mut String, hint: &str, rows: usize| {
        let edit = if rows > 1 {
            egui::TextEdit::multiline(text).desired_rows(rows)
        } else {
            egui::TextEdit::singleline(text)
        };
        ui.add(edit.hint_text(hint).desired_width(f32::INFINITY));
    };
    let mut close = false;
    raised_frame().show(ui, |ui| {
        ui.set_width(ui.available_width());
        let (icon, title, note) = match form {
            MeetingForm::Plan { .. } => (
                icons::EVENT,
                "Plan a meeting",
                "A call in this chat at a time. Everybody here can say whether they come.",
            ),
            MeetingForm::Poll { .. } => (
                icons::POLL,
                "Ask a poll",
                "Votes are messages in this chat: everybody in it sees who chose what.",
            ),
        };
        ui.label(
            egui::RichText::new(format!("{icon}  {title}"))
                .size(15.0)
                .color(theme::CREAM),
        );
        theme::note(ui, note);
        ui.add_space(4.0);
        match form {
            MeetingForm::Plan {
                title,
                start_ms,
                minutes,
                agenda,
                text,
                repeat,
                until_ms,
            } => {
                field(ui, title, "What: Q4 planning", 1);
                ui.horizontal_wrapped(|ui| {
                    // Never before the hour now running: a plan for the past
                    // is not something to ask people to come to.
                    let floor = next_hour_ms(now).saturating_sub(3_600_000);
                    for (label, shift) in [("−1 day", -1440i64), ("−15 min", -15)] {
                        if theme::action_button(ui, label).clicked() {
                            *start_ms = start_ms.saturating_add_signed(shift * 60_000).max(floor);
                        }
                    }
                    ui.label(
                        egui::RichText::new(crate::systems::meetup::when_label(
                            *start_ms,
                            now,
                            local_offset_s(*start_ms),
                        ))
                        .color(theme::TEXT),
                    );
                    for (label, shift) in [("+15 min", 15i64), ("+1 day", 1440)] {
                        if theme::action_button(ui, label).clicked() {
                            *start_ms = start_ms.saturating_add_signed(shift * 60_000);
                        }
                    }
                });
                ui.horizontal_wrapped(|ui| {
                    for m in [15u32, 30, 45, 60, 90, 120] {
                        let label = if m < 60 {
                            format!("{m} min")
                        } else if m % 60 == 0 {
                            format!("{} h", m / 60)
                        } else {
                            format!("{} h", f64::from(m) / 60.0)
                        };
                        if theme::chip(ui, &label, *minutes == m).clicked() {
                            *minutes = m;
                        }
                    }
                });
                repeat_fields(ui, *start_ms, repeat, until_ms);
                field(
                    ui,
                    agenda,
                    "Agenda, one item a line; a number at the end is its minutes\nBudget 10\nStage and sound 15",
                    3,
                );
                field(ui, text, "Anything else (optional)", 1);
            }
            MeetingForm::Poll {
                question,
                options,
                multiple,
            } => {
                field(ui, question, "Question: which day suits?", 1);
                field(ui, options, "Options, one a line\nFriday\nSaturday", 3);
                theme::checkbox(ui, multiple, "People may choose more than one");
            }
        }
        ui.add_space(4.0);
        let ready = form.object();
        ui.horizontal(|ui| {
            if theme::action_button(ui, "Cancel").clicked() {
                close = true;
            }
            let post = match form {
                MeetingForm::Plan { .. } => "Post the plan",
                MeetingForm::Poll { .. } => "Ask",
            };
            ui.add_enabled_ui(ready.is_some(), |ui| {
                if theme::primary_button_sized(ui, &format!("{}  {post}", icons::SEND), 150.0)
                    .clicked()
                {
                    if let Some(object) = ready {
                        act.meeting = Some((key.to_string(), object));
                        close = true;
                    }
                }
            });
        });
    });
    if close {
        view.meeting_form = None;
    }
    ui.add_space(6.0);
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
    // A line that failed to send goes back in its chat's box, ahead of
    // anything typed since, so nothing typed is lost to a failed send.
    for (key, text) in std::mem::take(&mut chat.unsent) {
        if view.open.as_deref() == Some(key.as_str()) {
            view.draft = if view.draft.trim().is_empty() {
                text
            } else {
                format!("{text}\n{}", view.draft)
            };
            view.claim_focus = true;
        } else {
            // Its chat is not open, so there is no box to put it in: say it
            // with the text, so it can be copied and sent again.
            chat.notice = Some(crate::systems::chat::ChatNotice {
                text: format!("A private message was not sent: \u{201c}{text}\u{201d}"),
                error: true,
            });
        }
    }
    if let Some(sub) = act.sub {
        view.sub = Some(sub);
        view.new_chat = false;
    }
    if let Some(root) = act.sub_root {
        view.sub_root = Some(root);
    }
    if let Some(you) = act.you {
        view.you = you;
        view.new_chat = false;
    }
    if act.sign_out {
        view.sign_out = true;
    }
    if let Some(panel) = act.panel {
        view.settings_from_you = act.settings_from_you;
        view.new_chat = false;
        *right_panel = panel;
    }
    if act.to_world || act.close {
        *right_panel = RightPanel::None;
        chat.world = None;
        view.you = false;
        view.new_chat = false;
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
    // Meet-ups (`systems::meetup`): ask for a place, adjust the time, send.
    if let Some(key) = act.meetup {
        chat.meetup_requests.push(key);
        view.attach = false;
    }
    if let Some(minutes) = act.meetup_shift {
        let floor = crate::systems::meetup::default_start_ms(now_ms()).saturating_sub(30 * 60_000);
        if let Some(m) = chat
            .meetup_draft
            .as_mut()
            .and_then(|(_, p)| p.meetup.as_mut())
        {
            // Never into the past: a meet-up that has already started is not
            // something to propose.
            m.start_ms = m
                .start_ms
                .saturating_add_signed(minutes * 60_000)
                .max(floor);
        }
    }
    if act.meetup_cancel {
        chat.meetup_draft = None;
    }
    if act.meetup_send {
        if let Some((key, point)) = chat.meetup_draft.take() {
            // The proposer is going to their own meet-up: the share and the
            // arrival apply to them as to everyone who answers.
            chat.meetups_going.push((key.clone(), point.clone()));
            chat.private_points.push((key, point));
        }
    }
    if let Some(said) = act.meeting {
        chat.private_meetings.push(said);
    }
    if let Some((key, place)) = act.meetup_going {
        let mut answer = place.clone();
        answer.text = Some("I'm going".into());
        answer.view = None;
        if let Some(m) = answer.meetup.as_mut() {
            m.going = true;
        }
        chat.private_points.push((key.clone(), answer));
        chat.meetups_going.push((key, place.clone()));
        // And the way there, which is what somebody going needs next.
        nav.route_to(
            place.name.clone().unwrap_or_else(|| "Meet-up".to_string()),
            place.latitude,
            place.longitude,
        );
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
    // Route: the planner opens on the destination (`geo_nav::RouteRequest`),
    // which replaces this panel in the right-hand slot.
    if let Some((lat, lng, name)) = act.route_to {
        nav.route_to(name, lat, lng);
    }
    if let Some((key, who, lat, lng)) = act.route_to_person {
        nav.route_to_person(&key, &who, lat, lng);
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
            community: None,
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
pub(crate) fn top_offset(chat: &ChatState) -> f32 {
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
    mut egui: EguiMut,
    chat: Res<ChatState>,
    mut view: ResMut<ChatsView>,
    online: Option<Res<OnlineSession>>,
    mut right_panel: ResMut<RightPanel>,
    driver: Res<crate::systems::driver::DriverState>,
    camera: Res<crate::systems::camera_screen::CameraScreen>,
) {
    if (online.is_none() && !view.demo)
        || driver.active
        || camera.is_open()
        || right_panel.is(RightPanel::Chat)
        || right_panel.is(RightPanel::Admin)
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
    let mut picked = None;
    let width = 260.0;
    egui::Area::new(egui::Id::new("chats_world_switch"))
        .order(egui::Order::Middle)
        .anchor(egui::Align2::CENTER_TOP, egui::vec2(0.0, top_offset(&chat)))
        .show(&ctx, |ui| {
            // `.topseg`: the glass segmented pill on its own, no frame around it.
            ui.set_width(width);
            picked = segmented(ui, "Community", width, Seg::World, unread, true);
        });
    match picked {
        Some(Seg::Community) => {
            *right_panel = RightPanel::Chat;
            view.open = None;
            view.you = false;
        }
        _ => {}
    }
}

/// Sign out from the You page: the same path as the main menu's Log out.
pub fn apply_sign_out(
    mut view: ResMut<ChatsView>,
    mut session: ResMut<crate::systems::session::SessionState>,
    mut store: ResMut<crate::systems::user_store::UserStore>,
    mut right_panel: ResMut<RightPanel>,
) {
    if !std::mem::take(&mut view.sign_out) {
        return;
    }
    view.you = false;
    view.sub = None;
    view.sub_root = None;
    *right_panel = RightPanel::None;
    crate::systems::session::sign_out(&mut session, &mut store);
}

/// A chat's World view: one slim pill over the map (back, who, Chat | World),
/// and nothing else of the chat's own. The chat's points are pinned by
/// `place_pins`.
///
/// **It reuses the map's chat rather than drawing a second one** (2026-10-18,
/// `docs/mockups/social-camera/`). Its last lines are the map's transcript,
/// bottom-left, fading, narrowed to this chat (`chat_overlay`); writing is the
/// dock's chat button, whose composer pill is drawn here for this chat because
/// a private chat sends through the engine and not through a channel scope. The
/// dock, the joystick and the map buttons stay: the half-screen sheet this
/// replaced hid all of them, and stopped part-way down the screen.
#[allow(clippy::too_many_arguments)]
pub fn render_chat_world(
    mut egui: EguiMut,
    mut chat: ResMut<ChatState>,
    mut view: ResMut<ChatsView>,
    online: Option<Res<OnlineSession>>,
    mut right_panel: ResMut<RightPanel>,
    driver: Res<crate::systems::driver::DriverState>,
    mut nav: ResMut<crate::systems::geo_nav::GeoNavState>,
    mut overlay: ResMut<crate::systems::chat_overlay::ChatOverlay>,
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
    let n = channel.lines.len();
    view.seen.insert(key.clone(), n);
    if view.open.as_deref() != Some(key.as_str()) {
        view.open_chat(&key);
    }

    let now = now_ms();
    let viewport = egui.viewport_pts();
    let top = top_offset(&chat);
    let ctx = egui.ctx_mut().clone();
    let mut act = Actions::default();

    egui::Area::new(egui::Id::new("chat_world_header"))
        .order(egui::Order::Middle)
        .anchor(egui::Align2::CENTER_TOP, egui::vec2(0.0, top))
        .show(&ctx, |ui| {
            glass_frame()
                .corner_radius(egui::CornerRadius::same(24))
                .inner_margin(egui::Margin::same(4))
                .show(ui, |ui| {
                    ui.horizontal(|ui| {
                        ui.spacing_mut().item_spacing.x = 8.0;
                        if circle_button(ui, icons::BACK, false, true, "Back to the chats")
                            .clicked()
                        {
                            act.back = true;
                        }
                        ui.label(
                            egui::RichText::new(&name)
                                .size(theme::SIZE_BODY)
                                .color(theme::CREAM),
                        );
                        if segmented(ui, "Chat", 170.0, Seg::World, 0, false)
                            == Some(Seg::Community)
                        {
                            act.to_world = false;
                            act.open = Some(key.clone());
                        }
                    });
                });
        });

    if overlay.composing {
        if overlay.take_claim() {
            view.claim_focus = true;
        }
        let keys = crate::systems::chat_overlay::keyboard_reserve(&egui, true);
        let (bottom, _) = crate::systems::chat_overlay::stack_bottoms(true, keys, 0.0);
        let width = (viewport.x - 2.0 * theme::OSD_MARGIN).min(WORLD_SHEET_MAX_WIDTH);
        egui::Area::new(egui::Id::new("chat_world_composer"))
            .order(egui::Order::Foreground)
            .anchor(egui::Align2::CENTER_BOTTOM, egui::vec2(0.0, -bottom))
            .show(&ctx, |ui| {
                ui.set_width(width);
                glass_frame()
                    .corner_radius(egui::CornerRadius::same(26))
                    .inner_margin(egui::Margin::same(6))
                    .show(ui, |ui| {
                        ui.set_width(width - 12.0);
                        if view.attach {
                            attach_menu(ui, &mut view, &key, kind, &chat, &mut act, now);
                            ui.add_space(6.0);
                        }
                        compose_row(ui, &mut view, &key, "chat_world_field", &mut act);
                    });
            });
        // Sent, the pill goes, as the map's own does.
        if act.send.is_some() {
            overlay.close();
        }
    }

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
            meetup: None,
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
        // A meet-up (`systems::meetup`), and Sanne sharing where she is, so the
        // card has a distance to show and the share row its Route.
        let mut vismarkt = point(53.2166, 6.5683, "Vismarkt", "halfway for both?");
        vismarkt.view = None;
        vismarkt.meetup = Some(cartopolis_core::point::Meetup {
            start_ms: crate::systems::meetup::default_start_ms(now_ms()),
            going: false,
        });
        push(
            chat,
            sanne,
            0,
            "Sanne",
            "halfway for both?",
            Some(vismarkt),
            9,
        );
        chat.update_location(
            crate::systems::chat::SharedLocation {
                channel: sanne.into(),
                from: "Sanne".into(),
                latitude: 53.2232,
                longitude: 6.5720,
                accuracy_m: 8.0,
                until_ms: now_ms() + 60 * 60_000,
            },
            now_ms(),
        );
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
        "meeting" => {
            seed_demo_meeting(chat);
            view.open_chat(DEMO_MEETING_CHAT);
            chat.world = None;
            RightPanel::Chat
        }
        // `+ → Plan a meeting` and `+ → Ask a poll`, half filled in.
        "plan" | "poll" => {
            seed_demo_meeting(chat);
            view.open_chat(DEMO_MEETING_CHAT);
            view.meeting_form = Some((
                DEMO_MEETING_CHAT.to_string(),
                if screen == "plan" {
                    MeetingForm::Plan {
                        title: "Street party wrap-up".into(),
                        start_ms: next_hour_ms(now_ms()) + 24 * 3_600_000,
                        minutes: 45,
                        agenda: "What went well 10\nThe money 15\nNext year 20".into(),
                        text: String::new(),
                        repeat: Some((Every::Week, 1)),
                        // Nine weeks, so the capture shows Until stepped.
                        until_ms: Some(next_hour_ms(now_ms()) + 57 * 24 * 3_600_000),
                    }
                } else {
                    MeetingForm::Poll {
                        question: "Which evening suits?".into(),
                        options: "Tuesday\nWednesday\nThursday".into(),
                        multiple: true,
                    }
                },
            ));
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

const DEMO_MEETING_CHAT: &str = "dm:6d656574";

/// A group chat with what a meeting leaves in it, for `--ui chatmeeting`: a
/// plan with answers, a poll with votes, notes taken in the call, the record.
fn seed_demo_meeting(chat: &mut ChatState) {
    use cartopolis_core::meeting::{
        AgendaItem, MeetingObject, NoteKind, Plan, Poll, Record, RecordNote, RecordPoll, Rsvp,
    };
    let key = DEMO_MEETING_CHAT;
    if chat.channels.iter().any(|c| c.key == key) {
        return;
    }
    chat.open_channel(key, "Street party crew");
    let now = now_ms();
    let say = |chat: &mut ChatState, id: u64, who: &str, m: MeetingObject| {
        chat.receive_meeting(key, id, who.into(), m.caption(), m);
    };
    let plan = "urn:uuid:demo-plan";
    say(
        chat,
        0,
        "Ann",
        MeetingObject::Plan(Plan {
            id: plan.into(),
            title: "Q4 planning".into(),
            start_ms: now + 5 * 60_000,
            end_ms: Some(now + 65 * 60_000),
            text: Some("Bring the numbers from the last party.".into()),
            agenda: vec![
                AgendaItem {
                    name: "Budget".into(),
                    minutes: 10,
                },
                AgendaItem {
                    name: "Stage and sound".into(),
                    minutes: 15,
                },
                AgendaItem {
                    name: "Who does what".into(),
                    minutes: 5,
                },
            ],
            // Weekly, so the capture shows a series and its next date.
            repeat: Some(cartopolis_core::meeting::Repeat {
                every: Every::Week,
                interval: 1,
                until_ms: None,
            }),
            skip: Vec::new(),
        }),
    );
    for (who, rsvp) in [
        ("Bob", Rsvp::Going),
        ("Carol", Rsvp::Maybe),
        ("Dave", Rsvp::Going),
    ] {
        say(
            chat,
            0,
            who,
            MeetingObject::Answer {
                plan: plan.into(),
                rsvp,
                date: cartopolis_core::meeting::AnswerFor::First,
            },
        );
    }
    let poll = "urn:uuid:demo-poll";
    say(
        chat,
        0,
        "Bob",
        MeetingObject::Poll(Poll {
            id: poll.into(),
            question: "Which day for the party?".into(),
            options: vec!["Friday".into(), "Saturday".into()],
            multiple: false,
        }),
    );
    for (who, id, day) in [
        ("Carol", 0, "Saturday"),
        ("Dave", 0, "Saturday"),
        ("You", 1, "Friday"),
    ] {
        say(
            chat,
            id,
            who,
            MeetingObject::Vote {
                poll: poll.into(),
                choices: vec![day.into()],
            },
        );
    }
    say(
        chat,
        0,
        "Ann",
        MeetingObject::Note {
            kind: NoteKind::Decision,
            text: "The gazebos stay in the budget".into(),
        },
    );
    say(
        chat,
        0,
        "Carol",
        MeetingObject::Note {
            kind: NoteKind::Action,
            text: "Carol books the stage by Friday".into(),
        },
    );
    say(
        chat,
        0,
        "Ann",
        MeetingObject::Record(Record {
            title: "Q4 planning".into(),
            start_ms: now.saturating_sub(40 * 60_000),
            end_ms: now.saturating_sub(5 * 60_000),
            people: vec!["ann".into(), "bob".into(), "carol".into(), "dave".into()],
            notes: vec![
                RecordNote {
                    kind: NoteKind::Decision,
                    text: "The gazebos stay in the budget".into(),
                    by: "ann".into(),
                },
                RecordNote {
                    kind: NoteKind::Action,
                    text: "Carol books the stage by Friday".into(),
                    by: "carol".into(),
                },
            ],
            polls: vec![RecordPoll {
                question: "Which day for the party?".into(),
                results: vec![("Friday".into(), 1), ("Saturday".into(), 2)],
            }],
            transcript: vec![
                ("bob".into(), "Shall we start with the budget?".into()),
                ("ann".into(), "Yes, the gazebos first.".into()),
            ],
        }),
    );
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
    fn a_meeting_form_posts_only_when_complete() {
        use cartopolis_core::meeting::MeetingObject;
        let now = 1_792_300_000_000;
        let mut plan = MeetingForm::plan(now);
        assert!(plan.object().is_none(), "a plan needs a title");
        let MeetingForm::Plan {
            title,
            start_ms,
            agenda,
            ..
        } = &mut plan
        else {
            unreachable!()
        };
        assert!(*start_ms > now && *start_ms - now <= 3_600_000);
        *title = " Q4 planning ".into();
        *agenda = "Budget 10\n\n- Wrap-up".into();
        let Some(MeetingObject::Plan(p)) = plan.object() else {
            panic!("a titled plan posts");
        };
        assert_eq!(p.title, "Q4 planning");
        assert_eq!(p.end_ms, Some(p.start_ms + 60 * 60_000));
        assert_eq!(p.agenda.len(), 2);
        assert_eq!(
            (p.agenda[0].minutes, p.agenda[1].name.as_str()),
            (10, "Wrap-up")
        );
        assert!(p.text.is_none());

        let mut poll = MeetingForm::poll();
        let MeetingForm::Poll {
            question, options, ..
        } = &mut poll
        else {
            unreachable!()
        };
        *question = "Which day?".into();
        *options = "Friday\n  \n".into();
        assert!(poll.object().is_none(), "one option is not a poll");
        if let MeetingForm::Poll { options, .. } = &mut poll {
            options.push_str("Saturday\n");
        }
        let Some(MeetingObject::Poll(p)) = poll.object() else {
            panic!("two options make a poll");
        };
        assert_eq!(
            p.options,
            vec!["Friday".to_string(), "Saturday".to_string()]
        );
    }

    #[test]
    fn a_chat_says_who_can_read_it() {
        assert!(subtitle(kind_of("dm:ab")).contains("end-to-end"));
        assert!(subtitle(kind_of("local")).contains("public"));
        assert!(subtitle(kind_of("geo:")).contains("public"));
        assert!(subtitle(ChatKind::Topic).contains("end-to-end"));
        assert_eq!(
            display_name(
                "dm:ab",
                &crate::systems::private_chat::private_label("Sanne")
            ),
            "Sanne"
        );
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

    /// The "+" sheet's choices take a tap. A click-catcher laid over the
    /// whole sheet after its rows once swallowed every one, so the sheet
    /// opened and nothing on it could be picked.
    #[test]
    fn a_tap_on_the_new_sheet_picks_a_choice() {
        use crate::systems::egui_manager::UnpaintedPasses as _;
        let ctx = egui::Context::default();
        theme::install(&ctx);
        let panel = egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(360.0, 720.0));
        let frame = |kind: &mut NewKind, events: Vec<egui::Event>| {
            let mut act = Actions::default();
            let mut chat = ChatState::default();
            let _ = ctx.run_ui_unpainted(
                egui::RawInput {
                    screen_rect: Some(panel),
                    events,
                    ..Default::default()
                },
                |ui| {
                    new_sheet(ui.ctx(), panel, 0.0, &mut chat, kind, &mut act);
                },
            );
            act
        };
        // Two frames: the sheet sits at the height it measured last frame.
        let mut kind = NewKind::Choose;
        frame(&mut kind, Vec::new());
        frame(&mut kind, Vec::new());
        let height: f32 = ctx
            .data(|d| d.get_temp(egui::Id::new("chats_new_sheet_height")))
            .unwrap();
        // Sweep taps down the sheet; the first choice under one is "Message
        // a person".
        let mut y = panel.bottom() - height;
        while y < panel.bottom() && kind == NewKind::Choose {
            let at = egui::pos2(panel.center().x, y);
            let button = |pressed| egui::Event::PointerButton {
                pos: at,
                button: egui::PointerButton::Primary,
                pressed,
                modifiers: egui::Modifiers::default(),
            };
            frame(&mut kind, vec![egui::Event::PointerMoved(at), button(true)]);
            frame(&mut kind, vec![button(false)]);
            y += 4.0;
        }
        assert_eq!(kind, NewKind::Person, "no tap on the sheet picked a choice");
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
