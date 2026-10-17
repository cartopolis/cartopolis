//! The call screen: a call ringing, the call you are in, and the bar either
//! folds down to (`systems::calls` is the call itself; this only draws it).
//!
//! Laid out the way Signal and WhatsApp lay theirs out, because that is the
//! screen everybody already knows how to use:
//!
//!  * **A call coming in takes the whole screen**: who, large, on a round
//!    initial with a slow pulse; what it is, small; Decline in red on the left
//!    and Answer in green on the right, at thumb height. Over a call you are
//!    already in it is a banner instead, so the call is not covered.
//!  * **In a call, the other person fills the screen** — their picture when
//!    their camera is on, cropped to fill (letterboxed only when an upright
//!    phone picture meets a wide desktop window), their initial when it is off —
//!    and your own picture is a small rounded window in the corner. Two or more
//!    others share the screen as a grid, with you as one of its tiles. While you are still ringing them, your own camera
//!    is the backdrop, as on a phone.
//!  * **The controls sit on the bottom edge** (camera, microphone, hang up) and
//!    the name and the timer on the top one; over a picture both fade out after
//!    a few seconds and come back at a tap.
//!  * **The arrow top-left, or Back, folds the screen down** to a green bar at
//!    the top of the app, with the microphone and hang-up on it, so the chats
//!    and the map stay usable during a call. Tapping the bar unfolds it. It is
//!    never *ended* by a gesture: hanging up and declining are buttons.
//!
//! Every surface here is an `Area` at `Order::Foreground` rather than part of a
//! panel: a call rings over whichever panel is open, and must be hang-up-able
//! from anywhere.

use crate::systems::egui_manager::EguiMut;
use std::collections::{HashMap, HashSet};

use bevy::prelude::*;

use crate::resources::{AuthIdentity, OnlineSession};
use crate::systems::calls::{camera_available, CallRequest, Calls, Ringing, SELF_TILE};
use crate::systems::chat::ChatState;
use crate::systems::chats::{paint_avatar, ChatKind};
use crate::systems::icons;
use crate::systems::private_chat::channel_key;
use crate::systems::theme::{self, CallTone, CALL_BUTTON_LARGE, CALL_BUTTON_SMALL};
use cartopolis_core::{CallAction, CallSettings};

/// How long the controls stay up over a picture after the last touch.
const CONTROLS_HIDE_SECS: f32 = 4.0;
/// How long a notice ("Missed call from …") stays when nothing else is up.
const NOTICE_SECS: f32 = 6.0;
/// The bar a folded call or a ringing banner is, at most.
const BAR_MAX_WIDTH: f32 = 440.0;
/// Inset of the screen's controls from its edges. More than the map's
/// `OSD_MARGIN` at the bottom: a phone's gesture bar lives there.
const EDGE: f32 = 16.0;
const BOTTOM_EDGE: f32 = 28.0;

/// The screen's own state between frames: textures and timers, nothing the
/// call needs.
#[derive(Default)]
pub struct CallScreenState {
    /// Each person's newest picture, uploaded only when it changed: a texture
    /// per person, reused, so a call is one small upload per picture received
    /// rather than a texture allocated per frame (`docs/notes/upload-bursts.md`).
    tiles: HashMap<u64, egui::TextureHandle>,
    /// Each shared screen's newest picture, the same way.
    screens: HashMap<u64, egui::TextureHandle>,
    /// The sheet open over the call screen.
    sheet: Option<Sheet>,
    /// The person a host tapped in the people sheet.
    picked: Option<u64>,
    /// Speaker view: whoever talks is large. Off is the even grid.
    speaker_view: bool,
    /// Who is large in Speaker view, kept until somebody else talks.
    big: Option<u64>,
    /// Who was waiting at the last frame, to say when somebody new knocks.
    waiting_seen: Vec<u64>,
    /// Calls that were ringing last frame, to unfold the screen for a new one.
    ringing_before: HashSet<Vec<u8>>,
    /// Last touch on the call screen, for hiding the controls over a picture.
    touched: Option<web_time::Instant>,
    /// Whether the controls are up over a picture.
    controls: bool,
    /// The notice on screen and when it appeared.
    notice: Option<(String, web_time::Instant)>,
}

/// One person on the call screen.
struct Person {
    id: u64,
    name: String,
    /// Their picture and its width over its height, when their camera is on.
    texture: Option<(egui::TextureId, f32)>,
    speaking: bool,
    /// A raised hand's place in the queue.
    hand: Option<u32>,
    muted: bool,
    /// A host muted them.
    held: bool,
    host: bool,
    /// The reaction floating over them.
    reaction: Option<String>,
}

/// What the screen is showing this frame, for its layout.
enum Mode<'a> {
    /// A call ringing here, nobody's call on this device.
    Incoming(&'a Ringing),
    /// The call this device is in.
    InCall,
}

/// Draw the call screen, the folded bar, ringing banners and call notices.
#[allow(clippy::too_many_arguments)]
pub fn render_call_screen(
    mut egui: EguiMut,
    mut calls: ResMut<Calls>,
    identity: Res<AuthIdentity>,
    chat: Res<ChatState>,
    online: Option<Res<OnlineSession>>,
    mut st: Local<CallScreenState>,
) {
    calls.screen_shown = false;
    if online.is_none() && !calls.demo {
        st.tiles.clear();
        return;
    }
    let me = identity.username.clone().unwrap_or_default();
    let ringing = calls.ringing(&me);
    let active = calls.active_group().map(<[u8]>::to_vec);

    // A call that starts ringing unfolds the screen, whatever was folded before.
    let now_ringing: HashSet<Vec<u8>> = ringing.iter().map(|r| r.group.clone()).collect();
    if now_ringing.difference(&st.ringing_before).next().is_some() {
        calls.minimized = false;
        st.controls = true;
        st.touched = Some(web_time::Instant::now());
    }
    st.ringing_before = now_ringing;

    // Notices go on their own after a few seconds, or at a tap, as a phone's
    // "Call ended" does; nobody should have to find an OK to clear one.
    match (&calls.notice, &st.notice) {
        (Some(n), Some((shown, _))) if n == shown => {}
        (Some(n), _) => st.notice = Some((n.clone(), web_time::Instant::now())),
        (None, _) => st.notice = None,
    }
    if st
        .notice
        .as_ref()
        .is_some_and(|(_, at)| at.elapsed().as_secs_f32() > NOTICE_SECS)
    {
        calls.notice = None;
        st.notice = None;
    }

    if ringing.is_empty() && active.is_none() && calls.notice.is_none() {
        st.tiles.clear();
        return;
    }

    let ctx = egui.ctx_mut().clone();
    // Folded to the bar, nothing shows a picture: leave the newest waiting
    // (`fresh`) for when the screen comes back rather than upload it unseen.
    if !calls.minimized {
        let CallScreenState { tiles, screens, .. } = &mut *st;
        upload_pictures(&ctx, &mut calls, tiles, screens);
    }

    let label_of = |group: &[u8]| {
        let key = channel_key(group);
        chat.channels
            .iter()
            .find(|c| c.key == key)
            .map(|c| crate::systems::private_chat::peer_of_label(&c.label).to_string())
    };
    let viewport = egui.viewport_pts();
    let full = egui::Rect::from_min_size(egui::Pos2::ZERO, viewport);
    let mut request = None;
    let mut clear_notice = false;

    // ── The full screen ────────────────────────────────────────────────────
    // The controls come up whenever the call screen does (a call starting, or
    // one unfolding), and a scripted capture keeps them up.
    if active.is_none() || calls.minimized {
        st.touched = None;
    } else if st.touched.is_none() || calls.demo {
        st.touched = Some(web_time::Instant::now());
        st.controls = true;
    }
    let mode = if calls.minimized {
        None
    } else if active.is_some() {
        Some(Mode::InCall)
    } else {
        ringing.first().map(Mode::Incoming)
    };
    if let Some(mode) = &mode {
        calls.screen_shown = true;
        egui::Area::new(egui::Id::new("call_screen"))
            .fixed_pos(egui::Pos2::ZERO)
            .order(egui::Order::Foreground)
            .show(&ctx, |ui| {
                ui.set_clip_rect(full);
                // Swallow every touch the controls do not take, so nothing
                // reaches the map or the panels underneath.
                let backdrop = ui.allocate_rect(full, egui::Sense::click());
                ui.painter().rect_filled(full, 0.0, theme::INK);
                match mode {
                    Mode::Incoming(r) => {
                        let chat_label = label_of(&r.group);
                        if incoming_screen(ui, full, r, chat_label.as_deref(), &mut request) {
                            calls.minimized = true;
                        }
                    }
                    Mode::InCall => {
                        if backdrop.clicked() {
                            st.controls = !st.controls;
                            st.touched = Some(web_time::Instant::now());
                        }
                        let group = active.clone().unwrap_or_default();
                        in_call_screen(
                            ui,
                            full,
                            &mut calls,
                            &mut st,
                            label_of(&group),
                            &me,
                            &mut request,
                        );
                    }
                }
                // Repaint for the pulse, the timer and the controls' fade.
                ctx.request_repaint_after(std::time::Duration::from_millis(50));
            });
    }

    // ── The folded bar and the banners at the top ──────────────────────────
    let top = if chat.sharing.is_empty() {
        theme::OSD_MARGIN
    } else {
        theme::OSD_MARGIN + 44.0
    };
    let bar_width = (viewport.x - 2.0 * theme::OSD_MARGIN).min(BAR_MAX_WIDTH);
    // Banners for calls ringing while the full screen shows something else:
    // over a call you are in, or every ringing call while folded.
    let banners: Vec<&Ringing> = match &mode {
        Some(Mode::Incoming(first)) => ringing.iter().filter(|r| r.group != first.group).collect(),
        _ => ringing.iter().collect(),
    };
    let folded_call = calls.minimized && active.is_some();
    let notice = st.notice.as_ref().map(|(n, _)| n.clone());
    if folded_call || !banners.is_empty() || notice.is_some() {
        egui::Area::new(egui::Id::new("call_bar"))
            .anchor(egui::Align2::CENTER_TOP, egui::vec2(0.0, top))
            .order(egui::Order::Foreground)
            .show(&ctx, |ui| {
                // Pinned, never left to the contents: a size-to-content `Area`
                // that centres anything feeds its own width back and grows
                // (`hud`'s module note).
                ui.set_width(bar_width);
                ui.spacing_mut().item_spacing.y = 6.0;
                if folded_call {
                    let group = active.clone().unwrap_or_default();
                    if folded_bar(
                        ui,
                        bar_width,
                        &mut calls,
                        label_of(&group),
                        &me,
                        &mut request,
                    ) {
                        calls.minimized = false;
                        st.controls = true;
                        st.touched = Some(web_time::Instant::now());
                    }
                }
                for r in &banners {
                    let chat_label = label_of(&r.group);
                    if ringing_banner(
                        ui,
                        bar_width,
                        r,
                        chat_label.as_deref(),
                        active.is_some(),
                        &mut request,
                    ) && active.is_none()
                    {
                        calls.minimized = false;
                    }
                }
                if let Some(text) = &notice {
                    if notice_chip(ui, bar_width, text) {
                        clear_notice = true;
                    }
                }
            });
    }

    if request.is_some() {
        calls.request = request;
    }
    if clear_notice {
        calls.notice = None;
        st.notice = None;
    }
}

/// Upload each person's newest picture, and forget the textures of anybody no
/// longer sending.
fn upload_pictures(
    ctx: &egui::Context,
    calls: &mut Calls,
    tiles: &mut HashMap<u64, egui::TextureHandle>,
    screens: &mut HashMap<u64, egui::TextureHandle>,
) {
    upload(ctx, calls.pictures_mut(), tiles, "call-video");
    upload(ctx, calls.screens_mut(), screens, "call-screen");
}

fn upload<'a>(
    ctx: &egui::Context,
    pictures: impl Iterator<Item = (u64, &'a mut crate::systems::calls::Picture)>,
    tiles: &mut HashMap<u64, egui::TextureHandle>,
    name: &str,
) {
    let mut present = HashSet::new();
    for (id, p) in pictures {
        present.insert(id);
        if !p.fresh {
            continue;
        }
        p.fresh = false;
        // Opaque pictures are premultiplied already, which skips the
        // per-pixel multiply `from_rgba_unmultiplied` would do.
        let image = egui::ColorImage::from_rgba_premultiplied(
            [p.width as usize, p.height as usize],
            &p.rgba,
        );
        match tiles.get_mut(&id) {
            Some(t) => t.set(image, egui::TextureOptions::LINEAR),
            None => {
                let t =
                    ctx.load_texture(format!("{name}-{id}"), image, egui::TextureOptions::LINEAR);
                tiles.insert(id, t);
            }
        }
    }
    tiles.retain(|id, _| present.contains(id));
}

fn texture_of(
    tiles: &HashMap<u64, egui::TextureHandle>,
    id: u64,
) -> Option<(egui::TextureId, f32)> {
    tiles.get(&id).map(|t| {
        let [w, h] = t.size();
        (t.id(), w as f32 / h.max(1) as f32)
    })
}

/// The part of a picture of shape `aspect` that fills `rect` when cropped to
/// it, centred.
fn cover_uv(aspect: f32, rect: egui::Rect) -> egui::Rect {
    let target = rect.width() / rect.height().max(1.0);
    if aspect > target {
        let w = target / aspect;
        egui::Rect::from_min_max(
            egui::pos2((1.0 - w) * 0.5, 0.0),
            egui::pos2((1.0 + w) * 0.5, 1.0),
        )
    } else {
        let h = aspect / target;
        egui::Rect::from_min_max(
            egui::pos2(0.0, (1.0 - h) * 0.5),
            egui::pos2(1.0, (1.0 + h) * 0.5),
        )
    }
}

/// The largest rect of shape `aspect` inside `rect`, centred.
fn contain(aspect: f32, rect: egui::Rect) -> egui::Rect {
    let target = rect.width() / rect.height().max(1.0);
    let size = if aspect > target {
        egui::vec2(rect.width(), rect.width() / aspect)
    } else {
        egui::vec2(rect.height() * aspect, rect.height())
    };
    egui::Rect::from_center_size(rect.center(), size)
}

/// Paint a picture into `rect`, cropped to fill it — always on a portrait
/// screen, which is a phone, where a call is full-screen video whatever the
/// other camera sends. Only an upright picture on a wide window (a phone
/// calling a desktop) is letterboxed, or it would be cut to a strip.
fn paint_picture(
    ui: &egui::Ui,
    rect: egui::Rect,
    (id, aspect): (egui::TextureId, f32),
    radius: u8,
    always_fill: bool,
) {
    let tall_picture_on_wide_rect = aspect < 1.0 && rect.width() > rect.height();
    let (rect, uv) = if always_fill || !tall_picture_on_wide_rect {
        (rect, cover_uv(aspect, rect))
    } else {
        (
            contain(aspect, rect),
            egui::Rect::from_min_max(egui::Pos2::ZERO, egui::pos2(1.0, 1.0)),
        )
    };
    egui::Image::new((id, rect.size()))
        .uv(uv)
        .corner_radius(egui::CornerRadius::same(radius))
        .paint_at(ui, rect);
}

/// A vertical shade over a picture's top or bottom edge, so white text and
/// controls read on any picture.
fn paint_shade(p: &egui::Painter, rect: egui::Rect, dark_at_top: bool) {
    let dark = egui::Color32::from_black_alpha(150);
    let clear = egui::Color32::TRANSPARENT;
    let (top, bottom) = if dark_at_top {
        (dark, clear)
    } else {
        (clear, dark)
    };
    let mut mesh = egui::Mesh::default();
    let i = mesh.vertices.len() as u32;
    mesh.colored_vertex(rect.left_top(), top);
    mesh.colored_vertex(rect.right_top(), top);
    mesh.colored_vertex(rect.right_bottom(), bottom);
    mesh.colored_vertex(rect.left_bottom(), bottom);
    mesh.add_triangle(i, i + 1, i + 2);
    mesh.add_triangle(i, i + 2, i + 3);
    p.add(mesh);
}

/// A call's status line: the timer once it is up, or where it has got to.
fn status_line(calls: &Calls, others: usize) -> String {
    if calls.reconnecting() {
        "Reconnecting…".into()
    } else if others == 0 {
        "Ringing…".into()
    } else if calls.connected() {
        crate::systems::calls::call_clock(calls.elapsed_secs())
    } else {
        "Connecting…".into()
    }
}

/// Text centred at `pos`, one line.
fn centred(p: &egui::Painter, pos: egui::Pos2, text: &str, size: f32, color: egui::Color32) {
    p.text(
        pos,
        egui::Align2::CENTER_CENTER,
        text,
        egui::FontId::proportional(size),
        color,
    );
}

/// A call button placed with its centre at `c`.
fn button_at(
    ui: &mut egui::Ui,
    c: egui::Pos2,
    icon: &str,
    tip: &str,
    tone: CallTone,
    d: f32,
) -> bool {
    let rect = egui::Rect::from_center_size(c, egui::Vec2::splat(d));
    ui.scope_builder(egui::UiBuilder::new().max_rect(rect), |ui| {
        theme::call_button(ui, icon, tip, tone, d).clicked()
    })
    .inner
}

/// The fold-down arrow in the top-left corner.
fn fold_button(ui: &mut egui::Ui, full: egui::Rect) -> bool {
    let d = 40.0;
    let c = egui::pos2(full.min.x + EDGE + d * 0.5, full.min.y + EDGE + d * 0.5);
    let rect = egui::Rect::from_center_size(c, egui::Vec2::splat(d));
    let resp = ui
        .interact(rect, egui::Id::new("call_fold"), egui::Sense::click())
        .on_hover_text("Back to the app — the call carries on");
    let p = ui.painter();
    if resp.hovered() {
        p.circle_filled(c, d * 0.5, egui::Color32::from_white_alpha(30));
    }
    centred(p, c, icons::BACK, 24.0, theme::CREAM);
    resp.clicked()
}

/// The lock line every call screen carries: what makes it a private call.
fn encrypted_line(p: &egui::Painter, pos: egui::Pos2) {
    centred(
        p,
        pos,
        &format!("{} End-to-end encrypted", icons::LOCK),
        theme::SIZE_SUB,
        theme::TEXT_DIM,
    );
}

/// A call coming in, over the whole screen.
fn incoming_screen(
    ui: &mut egui::Ui,
    full: egui::Rect,
    r: &Ringing,
    chat_label: Option<&str>,
    request: &mut Option<CallRequest>,
) -> bool {
    let name = r.who.join(", ");
    let p = ui.painter().clone();
    let x = full.center().x;
    let h = full.height();
    let disc_c = egui::pos2(x, full.min.y + h * 0.30);
    let disc = (full.width().min(h) * 0.34).clamp(96.0, 150.0);
    // A slow pulse around the initial: the screen is ringing, not frozen.
    let t = ui.input(|i| i.time) as f32;
    for k in 0..2 {
        let phase = ((t * 0.6) + k as f32 * 0.5).fract();
        let radius = disc * 0.5 * (1.0 + phase * 0.55);
        let alpha = ((1.0 - phase) * 60.0) as u8;
        p.circle_stroke(
            disc_c,
            radius,
            egui::Stroke::new(2.0, egui::Color32::from_white_alpha(alpha)),
        );
    }
    paint_avatar(&p, disc_c, &name, ChatKind::Private, disc);
    let below = disc_c.y + disc * 0.5;
    centred(&p, egui::pos2(x, below + 40.0), &name, 28.0, theme::CREAM);
    let what = match chat_label {
        Some(chat) if !chat.eq_ignore_ascii_case(&name) => format!("Incoming call · {chat}"),
        _ => "Incoming call".to_string(),
    };
    centred(
        &p,
        egui::pos2(x, below + 72.0),
        &what,
        theme::SIZE_BODY,
        theme::TEXT,
    );
    encrypted_line(&p, egui::pos2(x, below + 96.0));

    // Folded, it keeps ringing as a banner at the top.
    let folded = fold_button(ui, full);

    // Decline left, Answer right, at thumb height.
    let row_y = full.max.y - BOTTOM_EDGE - CALL_BUTTON_LARGE * 0.5 - 28.0;
    let spread = (full.width() * 0.26).clamp(70.0, 150.0);
    let decline = egui::pos2(x - spread, row_y);
    let answer = egui::pos2(x + spread, row_y);
    if button_at(
        ui,
        decline,
        icons::CALL_END,
        "Decline",
        CallTone::HangUp,
        CALL_BUTTON_LARGE,
    ) {
        *request = Some(CallRequest::Dismiss(r.group.clone()));
    }
    if button_at(
        ui,
        answer,
        icons::CALL,
        "Answer",
        CallTone::Accept,
        CALL_BUTTON_LARGE,
    ) {
        *request = Some(CallRequest::Join(r.group.clone()));
    }
    let label_y = row_y + CALL_BUTTON_LARGE * 0.5 + 16.0;
    centred(
        &p,
        egui::pos2(decline.x, label_y),
        "Decline",
        theme::SIZE_SUB,
        theme::TEXT,
    );
    centred(
        &p,
        egui::pos2(answer.x, label_y),
        "Answer",
        theme::SIZE_SUB,
        theme::TEXT,
    );
    folded
}

/// The call you are in, over the whole screen.
#[allow(clippy::too_many_arguments)]
fn in_call_screen(
    ui: &mut egui::Ui,
    full: egui::Rect,
    calls: &mut Calls,
    st: &mut CallScreenState,
    chat_label: Option<String>,
    me: &str,
    request: &mut Option<CallRequest>,
) {
    if calls.waiting_to_join() {
        waiting_screen(ui, full, calls, st, chat_label.as_deref(), request);
        return;
    }
    // Somebody new knocking is news to a host, wherever the people sheet is.
    let waiting = calls.extras().waiting;
    if calls.hosting() {
        if let Some(w) = waiting
            .iter()
            .find(|w| !st.waiting_seen.contains(&w.participant))
        {
            calls.notice = Some(format!(
                "{} is waiting to join. Admit them under People.",
                w.username
            ));
        }
    }
    st.waiting_seen = waiting.iter().map(|w| w.participant).collect();
    let mut people: Vec<Person> = calls
        .others()
        .into_iter()
        .map(|(id, name)| person_of(calls, &st.tiles, id, name))
        .collect();
    // A breakout room shows only who is in it.
    let my_room = calls.me().map_or(0, |m| m.room);
    people.retain(|p| calls.member(p.id).map_or(0, |m| m.room) == my_room);
    people.sort_by(|a, b| a.name.cmp(&b.name));
    let own = texture_of(&st.tiles, SELF_TILE);
    let sharer = calls.sharer();
    let screen = sharer.and_then(|s| texture_of(&st.screens, s));
    let any_picture = screen.is_some()
        || people.iter().any(|p| p.texture.is_some())
        || (people.is_empty() && own.is_some());
    // Over a picture the controls fade after a while; over initials they stay,
    // and so do they while a sheet is open.
    if !any_picture || st.sheet.is_some() {
        st.controls = true;
    } else if st
        .touched
        .is_none_or(|t| t.elapsed().as_secs_f32() > CONTROLS_HIDE_SECS)
    {
        st.controls = false;
    }
    let show_controls = st.controls;

    let p = ui.painter().clone();
    let x = full.center().x;
    let status = status_line(calls, people.len());
    let title = if people.is_empty() || people.len() > 3 {
        chat_label.unwrap_or_else(|| "Calling…".into())
    } else {
        people
            .iter()
            .map(|p| p.name.as_str())
            .collect::<Vec<_>>()
            .join(", ")
    };
    let mut self_person = person_of(calls, &st.tiles, SELF_TILE, "You".into());
    self_person.muted = calls.muted;
    self_person.speaking = calls.speaking(SELF_TILE);

    // ── The stage ──────────────────────────────────────────────────────────
    let top_room = 72.0;
    let bottom_room = CALL_BUTTON_LARGE + BOTTOM_EDGE + 30.0;
    let stage = egui::Rect::from_min_max(
        egui::pos2(full.min.x + 8.0, full.min.y + top_room),
        egui::pos2(full.max.x - 8.0, full.max.y - bottom_room),
    );
    let mut pip = own;
    let mut centre_title = false;
    if let (Some(tex), Some(who)) = (screen, sharer) {
        // A shared screen is the stage: drawn whole, never cropped, with
        // everybody (you too) in a strip beside it or under it.
        pip = None;
        let portrait = stage.height() > stage.width();
        let mut tiles: Vec<&Person> = people.iter().collect();
        tiles.push(&self_person);
        let (screen_rect, strip) = if portrait {
            let h = (stage.width() / tex.1.max(0.1)).min(stage.height() * 0.55);
            let screen_rect = egui::Rect::from_min_size(stage.min, egui::vec2(stage.width(), h));
            let strip = egui::Rect::from_min_max(
                egui::pos2(stage.min.x, screen_rect.max.y + 8.0),
                stage.max,
            );
            (screen_rect, strip)
        } else {
            let w = (stage.width() * 0.78).min(stage.width() - 160.0);
            let screen_rect = egui::Rect::from_min_size(stage.min, egui::vec2(w, stage.height()));
            let strip = egui::Rect::from_min_max(
                egui::pos2(screen_rect.max.x + 8.0, stage.min.y),
                stage.max,
            );
            (screen_rect, strip)
        };
        p.rect_filled(screen_rect, 14.0, egui::Color32::from_black_alpha(160));
        let shown = contain(tex.1, screen_rect);
        egui::Image::new((tex.0, shown.size()))
            .corner_radius(egui::CornerRadius::same(10))
            .paint_at(ui, shown);
        let name = calls.name_of(who).unwrap_or("Somebody").to_string();
        let chip_text = format!("{}  {name} is presenting", icons::SCREEN_SHARE);
        name_chip(
            &p,
            egui::pos2(screen_rect.min.x + 8.0, screen_rect.min.y + 8.0),
            &chip_text,
            false,
        );
        let cols = if portrait { 2 } else { 1 };
        grid(ui, strip, &tiles, cols);
    } else if let Some(big) = speaker_of(calls, st, &people) {
        // One person large (the spotlight, or whoever talks), everybody else
        // in a row under them.
        pip = None;
        let strip_h = (stage.height() * 0.22).clamp(70.0, 150.0);
        let big_rect = egui::Rect::from_min_max(
            stage.min,
            egui::pos2(stage.max.x, stage.max.y - strip_h - 8.0),
        );
        let strip =
            egui::Rect::from_min_max(egui::pos2(stage.min.x, stage.max.y - strip_h), stage.max);
        let big_person = people
            .iter()
            .find(|p| p.id == big)
            .expect("chosen from people");
        grid(ui, big_rect, &[big_person], 1);
        let mut rest: Vec<&Person> = people.iter().filter(|p| p.id != big).collect();
        rest.push(&self_person);
        let cols = rest.len();
        grid(ui, strip, &rest, cols);
    } else {
        match people.len() {
            0 => {
                // Still ringing them: your own camera is the backdrop, if it is on.
                if let Some(tex) = own {
                    paint_picture(ui, full, tex, 0, true);
                    p.rect_filled(full, 0.0, egui::Color32::from_black_alpha(70));
                    pip = None;
                }
                centre_title = true;
            }
            1 => {
                let person = &people[0];
                match person.texture {
                    Some(tex) => {
                        paint_picture(ui, full, tex, 0, false);
                        tile_badges(&p, stage, person);
                    }
                    None => centre_title = true,
                }
            }
            _ => {
                // You are one of the tiles in a group, last, rather than a
                // window laid over somebody else's.
                pip = None;
                let mut tiles: Vec<&Person> = people.iter().collect();
                tiles.push(&self_person);
                let n = tiles.len();
                let portrait = stage.height() > stage.width();
                let cols = if portrait {
                    if n <= 3 {
                        1
                    } else {
                        2
                    }
                } else {
                    (n as f32).sqrt().ceil() as usize
                };
                grid(ui, stage, &tiles, cols);
            }
        }
    }
    if centre_title {
        // No picture to look at: the person, large, in the middle.
        let disc = (full.width().min(full.height()) * 0.34).clamp(96.0, 150.0);
        let c = egui::pos2(x, full.min.y + full.height() * 0.34);
        let speaking = people.first().is_some_and(|p| p.speaking);
        if speaking {
            p.circle_stroke(
                c,
                disc * 0.5 + 6.0,
                egui::Stroke::new(3.0, theme::ACCENT_RING),
            );
        }
        paint_avatar(&p, c, &title, ChatKind::Private, disc);
        if let Some(r) = people.first().and_then(|p| p.reaction.as_deref()) {
            centred(
                &p,
                c + egui::vec2(disc * 0.42, disc * 0.36),
                r,
                40.0,
                theme::CREAM,
            );
        }
        centred(
            &p,
            egui::pos2(x, c.y + disc * 0.5 + 40.0),
            &title,
            28.0,
            theme::CREAM,
        );
        centred(
            &p,
            egui::pos2(x, c.y + disc * 0.5 + 72.0),
            &status,
            theme::SIZE_BODY,
            theme::TEXT,
        );
    }

    // ── Your own picture, in the corner ────────────────────────────────────
    if let Some(tex) = pip {
        let w = (full.width() * 0.28).clamp(90.0, 180.0);
        let h = w / tex.1.max(0.1);
        let h = h.min(full.height() * 0.32);
        let w = h * tex.1;
        let top = if show_controls {
            full.min.y + 76.0
        } else {
            full.min.y + EDGE
        };
        let rect =
            egui::Rect::from_min_size(egui::pos2(full.max.x - EDGE - w, top), egui::vec2(w, h));
        p.rect_filled(rect.expand(1.0), 13.0, egui::Color32::from_black_alpha(120));
        paint_picture(ui, rect, tex, 12, true);
        p.rect_stroke(
            rect,
            12.0,
            egui::Stroke::new(1.0, egui::Color32::from_white_alpha(60)),
            egui::StrokeKind::Outside,
        );
        if calls.muted {
            let badge = egui::pos2(rect.min.x + 16.0, rect.max.y - 16.0);
            p.circle_filled(badge, 12.0, theme::CREAM);
            centred(&p, badge, icons::MIC_OFF, 14.0, theme::INK);
        }
        if let Some(r) = calls.reaction_of(SELF_TILE) {
            centred(
                &p,
                egui::pos2(rect.max.x - 18.0, rect.max.y - 20.0),
                &r.emoji,
                26.0,
                theme::CREAM,
            );
        }
    }

    if show_controls {
        // ── The top: fold, name, timer, people ─────────────────────────────
        if any_picture {
            paint_shade(
                &p,
                egui::Rect::from_min_size(full.min, egui::vec2(full.width(), 120.0)),
                true,
            );
            paint_shade(
                &p,
                egui::Rect::from_min_max(egui::pos2(full.min.x, full.max.y - 180.0), full.max),
                false,
            );
        }
        if fold_button(ui, full) {
            calls.minimized = true;
        }
        if people_button(ui, full, calls) {
            st.sheet = Some(Sheet::People);
            st.picked = None;
        }
        if centre_title {
            encrypted_line(&p, egui::pos2(x, full.min.y + EDGE + 20.0));
        } else {
            centred(
                &p,
                egui::pos2(x, full.min.y + EDGE + 14.0),
                &title,
                theme::SIZE_HEADING,
                theme::CREAM,
            );
            centred(
                &p,
                egui::pos2(x, full.min.y + EDGE + 38.0),
                &format!("{} {status}", icons::LOCK),
                theme::SIZE_SUB,
                theme::TEXT,
            );
        }
        dock(ui, full, calls, st, request);
        meeting_chips(ui, full, calls, request);
    }
    let _ = me;

    // ── Captions, above the keys, whether or not the keys show ─────────────
    if let Some((who, text)) = calls.caption_now() {
        let width = (full.width() - 2.0 * EDGE).min(640.0);
        let galley = p.layout(
            text,
            egui::FontId::proportional(theme::SIZE_BODY + 2.0),
            theme::CREAM,
            width - 28.0,
        );
        let h = galley.size().y + 30.0;
        let bottom = if show_controls {
            full.max.y - BOTTOM_EDGE - CALL_BUTTON_LARGE - 40.0
        } else {
            full.max.y - BOTTOM_EDGE
        };
        let rect = egui::Rect::from_min_size(
            egui::pos2(full.center().x - width * 0.5, bottom - h),
            egui::vec2(width, h),
        );
        p.rect_filled(
            rect,
            egui::CornerRadius::same(14),
            egui::Color32::from_black_alpha(205),
        );
        p.text(
            rect.min + egui::vec2(14.0, 8.0),
            egui::Align2::LEFT_TOP,
            who,
            egui::FontId::proportional(theme::SIZE_TINY),
            theme::ACCENT_RING,
        );
        p.galley(rect.min + egui::vec2(14.0, 24.0), galley, theme::CREAM);
    }

    // ── A sheet over it all ────────────────────────────────────────────────
    if let Some(sheet) = st.sheet {
        let shade = ui.interact(
            full,
            egui::Id::new("call_sheet_shade"),
            egui::Sense::click(),
        );
        p.rect_filled(full, 0.0, egui::Color32::from_black_alpha(140));
        let width = full.width().min(520.0);
        let left = full.center().x - width * 0.5;
        let max_h = full.height() * 0.8;
        let mut close = shade.clicked();
        let _ = left;
        egui::Area::new(egui::Id::new("call_sheet"))
            .order(egui::Order::Foreground)
            .anchor(egui::Align2::CENTER_BOTTOM, egui::Vec2::ZERO)
            .show(ui.ctx(), |ui| {
                ui.set_width(width);
                ui.set_max_height(max_h);
                let frame = egui::Frame::NONE
                    .fill(theme::SURFACE)
                    .stroke(egui::Stroke::new(1.0, theme::OVERLAY_LINE))
                    .corner_radius(egui::CornerRadius {
                        nw: 22,
                        ne: 22,
                        sw: 0,
                        se: 0,
                    })
                    .inner_margin(egui::Margin {
                        left: 16,
                        right: 16,
                        top: 10,
                        bottom: 28,
                    });
                // Pinned to the bottom edge whatever its height.
                frame.show(ui, |ui| {
                    ui.set_width(width - 32.0);
                    let (grab, _) = ui
                        .allocate_exact_size(egui::vec2(width - 32.0, 14.0), egui::Sense::hover());
                    ui.painter().rect_filled(
                        egui::Rect::from_center_size(grab.center(), egui::vec2(38.0, 5.0)),
                        3.0,
                        theme::TEXT_DIM,
                    );
                    egui::ScrollArea::vertical().max_height(max_h - 60.0).show(
                        ui,
                        |ui| match sheet {
                            Sheet::More => more_sheet(ui, calls, st, request, &mut close),
                            Sheet::People => people_sheet(ui, calls, st, request),
                            Sheet::Leave => leave_sheet(ui, calls, request, &mut close),
                        },
                    );
                });
            });
        if close {
            st.sheet = None;
            st.picked = None;
        }
    }
}

/// The sheet open over the call screen.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Sheet {
    /// Reactions, and the rest of the call's actions.
    More,
    /// Everybody in the call: hands, and a host's actions on a person.
    People,
    /// A host leaving: leave, or end it for everyone.
    Leave,
}

/// The reactions offered, in the order the row shows them.
const REACTIONS: [&str; 6] = ["👍", "👏", "😂", "❤", "🎉", "😮"];

/// Somebody on the call screen, from the call's roster.
fn person_of(
    calls: &Calls,
    tiles: &HashMap<u64, egui::TextureHandle>,
    id: u64,
    name: String,
) -> Person {
    let member = if id == SELF_TILE {
        calls.me()
    } else {
        calls.member(id)
    };
    Person {
        id,
        texture: texture_of(tiles, id),
        speaking: calls.speaking(id),
        hand: member.and_then(|m| m.hand),
        muted: member.is_some_and(|m| m.muted),
        held: member.is_some_and(|m| m.held),
        host: member.is_some_and(|m| m.role.hosts()),
        reaction: calls.reaction_of(id).map(|r| r.emoji.clone()),
        name,
    }
}

/// Lay `tiles` out in an even grid of `cols` columns over `area`.
fn grid(ui: &egui::Ui, area: egui::Rect, tiles: &[&Person], cols: usize) {
    let n = tiles.len().max(1);
    let cols = cols.clamp(1, n);
    let rows = n.div_ceil(cols);
    let gap = 8.0;
    let cell = egui::vec2(
        (area.width() - gap * (cols - 1) as f32) / cols as f32,
        (area.height() - gap * (rows - 1) as f32) / rows as f32,
    );
    for (i, person) in tiles.iter().enumerate() {
        let (r, c) = (i / cols, i % cols);
        let rect = egui::Rect::from_min_size(
            area.min + egui::vec2(c as f32 * (cell.x + gap), r as f32 * (cell.y + gap)),
            cell,
        );
        grid_tile(ui, rect, person);
    }
}

/// A name on a dark chip with its top-left at `at`, as tiles carry it.
fn name_chip(p: &egui::Painter, at: egui::Pos2, text: &str, bottom: bool) -> egui::Rect {
    let galley = p.layout_no_wrap(
        text.to_string(),
        egui::FontId::proportional(theme::SIZE_SUB),
        theme::CREAM,
    );
    let pad = egui::vec2(8.0, 3.0);
    let size = galley.size() + 2.0 * pad;
    let min = if bottom {
        at - egui::vec2(0.0, size.y)
    } else {
        at
    };
    let chip = egui::Rect::from_min_size(min, size);
    p.rect_filled(
        chip,
        egui::CornerRadius::same(theme::RADIUS_PILL),
        egui::Color32::from_black_alpha(160),
    );
    p.galley(chip.min + pad, galley, theme::CREAM);
    chip
}

/// What a tile says about its person besides the picture: the name with the
/// microphone's state, a raised hand's place in the queue, a reaction.
fn tile_badges(p: &egui::Painter, rect: egui::Rect, person: &Person) {
    let mic = if person.held {
        format!("{} ", icons::LOCK)
    } else if person.muted {
        format!("{} ", icons::MIC_OFF)
    } else {
        String::new()
    };
    let host = if person.host {
        format!("  {}", icons::HOST)
    } else {
        String::new()
    };
    name_chip(
        p,
        egui::pos2(rect.min.x + 8.0, rect.max.y - 8.0),
        &format!("{mic}{}{host}", person.name),
        true,
    );
    if let Some(n) = person.hand {
        let text = format!("{} {n}", icons::HAND);
        let galley = p.layout_no_wrap(
            text,
            egui::FontId::proportional(theme::SIZE_SUB),
            theme::INK,
        );
        let pad = egui::vec2(8.0, 3.0);
        let chip =
            egui::Rect::from_min_size(rect.min + egui::vec2(8.0, 8.0), galley.size() + 2.0 * pad);
        p.rect_filled(
            chip,
            egui::CornerRadius::same(theme::RADIUS_PILL),
            theme::WARN,
        );
        p.galley(chip.min + pad, galley, theme::INK);
    }
    if let Some(r) = &person.reaction {
        centred(
            p,
            egui::pos2(rect.max.x - 26.0, rect.max.y - 30.0),
            r,
            34.0,
            theme::CREAM,
        );
    }
}

/// One person in a group call's grid.
fn grid_tile(ui: &egui::Ui, rect: egui::Rect, person: &Person) {
    let p = ui.painter();
    let radius = 14u8;
    match person.texture {
        Some(tex) => paint_picture(ui, rect, tex, radius, true),
        None => {
            p.rect_filled(rect, radius, theme::SURFACE_RAISED);
            let d = (rect.width().min(rect.height()) * 0.42).clamp(32.0, 120.0);
            paint_avatar(p, rect.center(), &person.name, ChatKind::Private, d);
        }
    }
    tile_badges(p, rect, person);
    if person.speaking {
        p.rect_stroke(
            rect,
            radius,
            egui::Stroke::new(3.0, theme::ACCENT_RING),
            egui::StrokeKind::Inside,
        );
    }
    let _ = person.id;
}

/// The people button, top right: how many are in the call, and a gold count of
/// raised hands. Answers whether it was pressed.
fn people_button(ui: &mut egui::Ui, full: egui::Rect, calls: &Calls) -> bool {
    let d = 40.0;
    let c = egui::pos2(full.max.x - EDGE - d * 0.5, full.min.y + EDGE + d * 0.5);
    let rect = egui::Rect::from_center_size(c, egui::Vec2::splat(d));
    let resp = ui
        .interact(rect, egui::Id::new("call_people"), egui::Sense::click())
        .on_hover_text("People in the call");
    let p = ui.painter();
    p.circle_filled(
        c,
        d * 0.5,
        egui::Color32::from_white_alpha(if resp.hovered() { 46 } else { 30 }),
    );
    centred(p, c, icons::PEOPLE, 22.0, theme::CREAM);
    let hands = calls.members().iter().filter(|m| m.hand.is_some()).count()
        + if calls.hosting() {
            calls.extras().waiting.len()
        } else {
            0
        };
    let (count, fill, ink) = if hands > 0 {
        (hands, theme::WARN, theme::INK)
    } else {
        (calls.members().len(), theme::SURFACE_RAISED, theme::CREAM)
    };
    if count > 0 {
        let b = c + egui::vec2(d * 0.38, -d * 0.36);
        p.circle_filled(b, 9.0, fill);
        centred(p, b, &count.to_string(), theme::SIZE_TINY, ink);
    }
    resp.clicked()
}

/// The keys along the bottom: microphone, camera, hand, more, leave. Each
/// carries a word under it: a phone never shows a hover tip, and an icon
/// alone left "is the crossed-out camera the state or the action?" to guess.
fn dock(
    ui: &mut egui::Ui,
    full: egui::Rect,
    calls: &mut Calls,
    st: &mut CallScreenState,
    request: &mut Option<CallRequest>,
) {
    const LABEL_GAP: f32 = 16.0;
    let p = ui.painter().clone();
    let x = full.center().x;
    let y = full.max.y - BOTTOM_EDGE - CALL_BUTTON_LARGE * 0.5 - LABEL_GAP * 0.5;
    let with_camera = camera_available();
    let n = if with_camera { 5.0 } else { 4.0 };
    // Narrow phones fit five keys by tightening the step, not the keys.
    let step = (CALL_BUTTON_LARGE + 18.0).min((full.width() - 2.0 * EDGE - 16.0) / n);
    let d = CALL_BUTTON_LARGE.min(step - 8.0);
    let mut cx = x - (n - 1.0) * step * 0.5;
    let tray = egui::Rect::from_center_size(
        egui::pos2(x, y + LABEL_GAP * 0.5),
        egui::vec2((n - 1.0) * step + d + 24.0, d + 20.0 + LABEL_GAP),
    );
    p.rect_filled(
        tray,
        egui::CornerRadius::same(theme::RADIUS_PILL),
        egui::Color32::from_black_alpha(130),
    );
    let label = |cx: f32, text: &str| {
        centred(
            &p,
            egui::pos2(cx, y + d * 0.5 + LABEL_GAP * 0.5 + 2.0),
            text,
            theme::SIZE_TINY,
            theme::TEXT,
        );
    };
    let mut touched = false;
    let held = calls.me().is_some_and(|m| m.held);
    let (icon, tip, tone) = if held {
        (icons::LOCK, "A host muted you", CallTone::Off)
    } else if calls.muted {
        (icons::MIC_OFF, "Unmute", CallTone::Off)
    } else {
        (icons::MIC, "Mute", CallTone::On)
    };
    if button_at(ui, egui::pos2(cx, y), icon, tip, tone, d) {
        if held {
            calls.notice = Some("A host muted you. Raise your hand to ask to talk.".into());
        } else {
            calls.muted = !calls.muted;
        }
        touched = true;
    }
    label(
        cx,
        if held {
            "Held"
        } else if calls.muted {
            "Unmute"
        } else {
            "Mute"
        },
    );
    cx += step;
    if with_camera {
        let (icon, tip, tone) = if calls.camera_on {
            (icons::VIDEOCAM, "Turn the camera off", CallTone::On)
        } else {
            (icons::VIDEOCAM_OFF, "Turn the camera on", CallTone::Off)
        };
        if button_at(ui, egui::pos2(cx, y), icon, tip, tone, d) {
            calls.camera_on = !calls.camera_on;
            touched = true;
        }
        label(cx, "Camera");
        cx += step;
    }
    let raised = calls.me().is_some_and(|m| m.hand.is_some());
    if raised {
        p.circle_stroke(
            egui::pos2(cx, y),
            d * 0.5 + 3.0,
            egui::Stroke::new(3.0, theme::WARN),
        );
    }
    let tip = if raised {
        "Lower your hand"
    } else {
        "Raise your hand"
    };
    if button_at(ui, egui::pos2(cx, y), icons::HAND, tip, CallTone::On, d) {
        *request = Some(CallRequest::Control(CallAction::Hand(!raised)));
        touched = true;
    }
    label(cx, if raised { "Lower" } else { "Hand" });
    cx += step;
    if button_at(
        ui,
        egui::pos2(cx, y),
        icons::MORE,
        "React, and more",
        CallTone::On,
        d,
    ) {
        st.sheet = Some(Sheet::More);
        touched = true;
    }
    label(cx, "More");
    cx += step;
    if button_at(
        ui,
        egui::pos2(cx, y),
        icons::CALL_END,
        "Leave",
        CallTone::HangUp,
        d,
    ) {
        // A host with others still in the call is asked: leave, or end it for
        // everyone. Anybody else just leaves, as on a phone.
        if calls.hosting() && !calls.others().is_empty() {
            st.sheet = Some(Sheet::Leave);
        } else {
            *request = Some(CallRequest::Leave);
        }
    }
    label(cx, "Leave");
    if touched {
        st.touched = Some(web_time::Instant::now());
    }
}

/// A full-width row in a sheet: icon, words, an optional switch state.
fn sheet_row(ui: &mut egui::Ui, icon: &str, text: &str, on: Option<bool>) -> bool {
    let resp = theme::menu_entry(ui, icon, text);
    if let Some(on) = on {
        let r = resp.rect;
        let track = egui::Rect::from_center_size(
            egui::pos2(r.max.x - 26.0, r.center().y),
            egui::vec2(34.0, 20.0),
        );
        let p = ui.painter();
        p.rect_filled(
            track,
            10.0,
            if on {
                theme::ACCENT
            } else {
                theme::SURFACE_RAISED
            },
        );
        let knob = if on {
            track.max.x - 10.0
        } else {
            track.min.x + 10.0
        };
        p.circle_filled(
            egui::pos2(knob, track.center().y),
            7.0,
            if on { theme::INK } else { theme::TEXT_DIM },
        );
    }
    resp.clicked()
}

/// Reactions, and the call's other actions; a host's controls at the end.
fn more_sheet(
    ui: &mut egui::Ui,
    calls: &mut Calls,
    st: &mut CallScreenState,
    request: &mut Option<CallRequest>,
    close: &mut bool,
) {
    theme::section(ui, "React");
    ui.horizontal(|ui| {
        ui.spacing_mut().item_spacing.x = 6.0;
        for r in REACTIONS {
            let b = egui::Button::new(egui::RichText::new(r).size(26.0))
                .min_size(egui::vec2(48.0, 48.0))
                .corner_radius(12);
            if ui.add(b).clicked() {
                *request = Some(CallRequest::React(r.to_string()));
                *close = true;
            }
        }
    });
    theme::section(ui, "In this call");
    if sheet_row(ui, icons::PEOPLE, "People", None) {
        st.sheet = Some(Sheet::People);
    }
    if sheet_row(ui, icons::STAR, "Speaker view", Some(st.speaker_view)) {
        st.speaker_view = !st.speaker_view;
    }
    // Off for music, or a room where the background is the point.
    if sheet_row(ui, icons::MIC, "Noise suppression", Some(!calls.keep_noise)) {
        calls.keep_noise = !calls.keep_noise;
    }
    if !calls.hosting() {
        return;
    }
    let s = calls.call_settings();
    theme::section(ui, "Hosting");
    if sheet_row(ui, icons::MIC_OFF, "Mute everyone", None) {
        *request = Some(CallRequest::Control(CallAction::MuteAll));
        *close = true;
    }
    let mut set =
        |ui: &mut egui::Ui, icon: &str, text: &str, on: bool, f: fn(&mut CallSettings, bool)| {
            if sheet_row(ui, icon, text, Some(on)) {
                let mut next = s;
                f(&mut next, !on);
                *request = Some(CallRequest::Control(CallAction::Settings(next)));
            }
        };
    set(ui, icons::LOCK, "Lock the call", s.locked, |s, v| {
        s.locked = v
    });
    set(ui, icons::MIC, "Others may unmute", s.may_unmute, |s, v| {
        s.may_unmute = v
    });
    set(
        ui,
        icons::SCREEN_SHARE,
        "Others may share a screen",
        s.may_share,
        |s, v| {
            s.may_share = v;
        },
    );
    set(
        ui,
        icons::HOST,
        "Members wait to be let in",
        s.members_wait,
        |s, v| {
            s.members_wait = v;
        },
    );
    set(
        ui,
        icons::DOT,
        "Anybody may record",
        s.may_record,
        |s, v| {
            s.may_record = v;
        },
    );
    theme::section(ui, "Breakout rooms");
    if calls.extras().rooms.is_some() {
        if sheet_row(ui, icons::PEOPLE, "Bring everybody back", None) {
            *request = Some(CallRequest::Control(CallAction::CloseRooms));
            *close = true;
        }
    } else {
        for n in [2u32, 3, 4] {
            if sheet_row(
                ui,
                icons::PEOPLE,
                &format!("Split into {n} rooms for 10 minutes"),
                None,
            ) {
                *request = Some(CallRequest::Control(split_into_rooms(calls, n)));
                *close = true;
            }
        }
    }
    ui.add_space(6.0);
    if sheet_row(ui, icons::CALL_END, "End for everyone", None) {
        *request = Some(CallRequest::Control(CallAction::EndForAll));
        *close = true;
    }
}

/// Everybody in the call: raised hands first, in the order they went up; then
/// everyone. A host taps a person for what can be done to them.
fn people_sheet(
    ui: &mut egui::Ui,
    calls: &mut Calls,
    st: &mut CallScreenState,
    request: &mut Option<CallRequest>,
) {
    let hosting = calls.hosting();
    let me = calls.me().map(|m| m.participant);
    let members: Vec<cartopolis_core::CallMember> = calls.members().to_vec();
    ui.label(
        egui::RichText::new(format!("People · {}", members.len()))
            .size(theme::SIZE_PANEL_TITLE)
            .color(theme::CREAM),
    );
    let waiting = calls.extras().waiting;
    if hosting && !waiting.is_empty() {
        theme::section(ui, &format!("Waiting to join · {}", waiting.len()));
        for w in &waiting {
            ui.horizontal(|ui| {
                let guest = if w.guest { " · guest" } else { "" };
                ui.label(
                    egui::RichText::new(format!("{}{guest}", w.username))
                        .size(theme::SIZE_BODY)
                        .color(theme::TEXT),
                );
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if theme::chip(ui, "Turn away", false).clicked() {
                        *request = Some(CallRequest::Control(CallAction::Deny {
                            participant: w.participant,
                        }));
                    }
                    if theme::chip(ui, "Admit", true).clicked() {
                        *request = Some(CallRequest::Control(CallAction::Admit {
                            participant: w.participant,
                        }));
                    }
                });
            });
        }
        if waiting.len() > 1 && theme::chip(ui, "Admit everyone", false).clicked() {
            *request = Some(CallRequest::Control(CallAction::AdmitAll));
        }
    }
    let mut hands: Vec<&cartopolis_core::CallMember> =
        members.iter().filter(|m| m.hand.is_some()).collect();
    hands.sort_by_key(|m| m.hand);
    if !hands.is_empty() {
        theme::section(ui, "Hands up");
        for (i, m) in hands.iter().enumerate() {
            ui.horizontal(|ui| {
                ui.label(
                    egui::RichText::new(format!("{} {}", icons::HAND, i + 1))
                        .color(theme::WARN)
                        .size(theme::SIZE_BODY),
                );
                ui.label(
                    egui::RichText::new(&m.username)
                        .size(theme::SIZE_BODY)
                        .color(theme::TEXT),
                );
                if hosting {
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        if theme::chip(ui, "Lower", false).clicked() {
                            *request = Some(CallRequest::Control(CallAction::LowerHand {
                                participant: m.participant,
                            }));
                        }
                        if theme::chip(ui, "Let talk", true).clicked() {
                            *request = Some(CallRequest::Control(CallAction::AllowTalk {
                                participant: m.participant,
                            }));
                        }
                    });
                } else if Some(m.participant) == me {
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        if theme::chip(ui, "Lower", false).clicked() {
                            *request = Some(CallRequest::Control(CallAction::Hand(false)));
                        }
                    });
                }
            });
        }
    }
    theme::section(ui, "In the call");
    for m in &members {
        let you = Some(m.participant) == me;
        let role = match m.role {
            cartopolis_core::CallRole::Host => " · host",
            cartopolis_core::CallRole::CoHost => " · co-host",
            cartopolis_core::CallRole::Member => "",
        };
        let state = if m.held {
            format!("  {}", icons::LOCK)
        } else if m.muted {
            format!("  {}", icons::MIC_OFF)
        } else {
            String::new()
        };
        let sharing = if m.sharing {
            format!("  {}", icons::SCREEN_SHARE)
        } else {
            String::new()
        };
        let name = if you { "You" } else { m.username.as_str() };
        let text = format!("{name}{role}{state}{sharing}");
        let picked = st.picked == Some(m.participant);
        let resp = theme::menu_entry_selected(ui, picked, icons::DOT, &text);
        if hosting && !you && resp.clicked() {
            st.picked = if picked { None } else { Some(m.participant) };
        }
        if hosting && picked {
            let participant = m.participant;
            ui.indent("call_person_actions", |ui| {
                if m.held {
                    if sheet_row(ui, icons::MIC, "Let talk", None) {
                        *request =
                            Some(CallRequest::Control(CallAction::AllowTalk { participant }));
                    }
                } else if sheet_row(ui, icons::MIC_OFF, "Mute", None) {
                    *request = Some(CallRequest::Control(CallAction::Mute { participant }));
                }
                if m.sharing && sheet_row(ui, icons::SCREEN_SHARE, "Stop their screen", None) {
                    *request = Some(CallRequest::Control(CallAction::StopSharing {
                        participant,
                    }));
                }
                let (word, role) = match m.role {
                    cartopolis_core::CallRole::Member => {
                        ("Make co-host", cartopolis_core::CallRole::CoHost)
                    }
                    _ => ("Make a member again", cartopolis_core::CallRole::Member),
                };
                if m.role != cartopolis_core::CallRole::Host
                    && sheet_row(ui, icons::HOST, word, None)
                {
                    *request = Some(CallRequest::Control(CallAction::Role { participant, role }));
                }
                if calls
                    .me()
                    .is_some_and(|me| me.role == cartopolis_core::CallRole::Host)
                    && sheet_row(ui, icons::STAR, "Hand the call over", None)
                {
                    *request = Some(CallRequest::Control(CallAction::Role {
                        participant,
                        role: cartopolis_core::CallRole::Host,
                    }));
                }
                let extras = calls.extras();
                let spotlit = extras.spotlight == Some(participant);
                let (word, target) = if spotlit {
                    ("Stop the spotlight", None)
                } else {
                    ("Spotlight for everyone", Some(participant))
                };
                if sheet_row(ui, icons::STAR, word, None) {
                    *request = Some(CallRequest::Control(CallAction::Spotlight {
                        participant: target,
                    }));
                }
                if let Some(rooms) = &extras.rooms {
                    for (i, name) in std::iter::once("the main call")
                        .chain(rooms.names.iter().map(String::as_str))
                        .enumerate()
                    {
                        if i as u32 != m.room
                            && sheet_row(ui, icons::PEOPLE, &format!("Move to {name}"), None)
                        {
                            *request = Some(CallRequest::Control(CallAction::MoveTo {
                                participant,
                                room: i as u32,
                            }));
                        }
                    }
                }
                if sheet_row(ui, icons::PERSON_REMOVE, "Remove from the call", None) {
                    *request = Some(CallRequest::Control(CallAction::Remove { participant }));
                    st.picked = None;
                }
            });
        }
    }
    if hosting {
        ui.label(
            egui::RichText::new("Tap a person for what a host can do.")
                .size(theme::SIZE_SUB)
                .color(theme::TEXT_DIM),
        );
    }
}

/// A host leaving a call others are still in: leave it to them, or end it.
fn leave_sheet(
    ui: &mut egui::Ui,
    calls: &Calls,
    request: &mut Option<CallRequest>,
    close: &mut bool,
) {
    ui.label(
        egui::RichText::new("Leave the call?")
            .size(theme::SIZE_PANEL_TITLE)
            .color(theme::CREAM),
    );
    let others = calls.others().len();
    if sheet_row(ui, icons::BACK, "Leave — the call goes on", None) {
        *request = Some(CallRequest::Leave);
        *close = true;
    }
    if sheet_row(
        ui,
        icons::CALL_END,
        &format!("End for everyone ({} others)", others),
        None,
    ) {
        *request = Some(CallRequest::Control(CallAction::EndForAll));
        *close = true;
    }
    if sheet_row(ui, icons::CLEAR, "Cancel", None) {
        *close = true;
    }
}

/// The call folded down to a bar: green, with who and how long, the
/// microphone and hang-up. Answers whether the bar was tapped to unfold.
fn folded_bar(
    ui: &mut egui::Ui,
    width: f32,
    calls: &mut Calls,
    chat_label: Option<String>,
    me: &str,
    request: &mut Option<CallRequest>,
) -> bool {
    let h = CALL_BUTTON_SMALL + 12.0;
    let (rect, resp) = ui.allocate_exact_size(egui::vec2(width, h), egui::Sense::click());
    let resp = resp.on_hover_text("Back to the call");
    let p = ui.painter().clone();
    let fill = if resp.hovered() {
        theme::CALL_ACCEPT_GREEN.gamma_multiply(1.12)
    } else {
        theme::CALL_ACCEPT_GREEN
    };
    p.rect_filled(rect, egui::CornerRadius::same(theme::RADIUS_PILL), fill);
    let others: Vec<String> = calls.others().into_iter().map(|(_, n)| n).collect();
    let who = if others.is_empty() {
        chat_label.unwrap_or_else(|| "Calling".into())
    } else {
        others.join(", ")
    };
    let status = status_line(calls, others.len());
    let _ = me;
    let text_x = rect.min.x + 16.0;
    p.text(
        egui::pos2(text_x, rect.center().y),
        egui::Align2::LEFT_CENTER,
        format!("{}  {who}", icons::CALL),
        egui::FontId::proportional(theme::SIZE_BODY),
        theme::CREAM,
    );
    // The timer just left of the buttons.
    let buttons_w = 2.0 * CALL_BUTTON_SMALL + 8.0 + 6.0;
    p.text(
        egui::pos2(rect.max.x - buttons_w - 12.0, rect.center().y),
        egui::Align2::RIGHT_CENTER,
        status,
        egui::FontId::monospace(theme::SIZE_SUB),
        theme::CREAM,
    );
    let y = rect.center().y;
    let hang = egui::pos2(rect.max.x - 6.0 - CALL_BUTTON_SMALL * 0.5, y);
    let mic = egui::pos2(hang.x - CALL_BUTTON_SMALL - 8.0, y);
    let (icon, tip, tone) = if calls.muted {
        (icons::MIC_OFF, "Unmute", CallTone::Off)
    } else {
        (icons::MIC, "Mute", CallTone::On)
    };
    let mut pressed = false;
    if button_at(ui, mic, icon, tip, tone, CALL_BUTTON_SMALL) {
        calls.muted = !calls.muted;
        pressed = true;
    }
    if button_at(
        ui,
        hang,
        icons::CALL_END,
        "Hang up",
        CallTone::HangUp,
        CALL_BUTTON_SMALL,
    ) {
        *request = Some(CallRequest::Leave);
        pressed = true;
    }
    resp.clicked() && !pressed
}

/// A call ringing as a banner: over a call you are in, or folded. Answers
/// whether the banner's body was tapped (to unfold it).
fn ringing_banner(
    ui: &mut egui::Ui,
    width: f32,
    r: &Ringing,
    chat_label: Option<&str>,
    in_a_call: bool,
    request: &mut Option<CallRequest>,
) -> bool {
    let h = 64.0;
    let (rect, resp) = ui.allocate_exact_size(egui::vec2(width, h), egui::Sense::click());
    let p = ui.painter().clone();
    p.rect_filled(
        rect,
        egui::CornerRadius::same(theme::RADIUS_LG),
        theme::SURFACE_RAISED,
    );
    p.rect_stroke(
        rect,
        egui::CornerRadius::same(theme::RADIUS_LG),
        egui::Stroke::new(1.0, theme::OVERLAY_LINE),
        egui::StrokeKind::Inside,
    );
    let name = r.who.join(", ");
    let disc_c = egui::pos2(rect.min.x + 12.0 + 20.0, rect.center().y);
    paint_avatar(&p, disc_c, &name, ChatKind::Private, 40.0);
    let text_x = disc_c.x + 32.0;
    p.text(
        egui::pos2(text_x, rect.center().y - 9.0),
        egui::Align2::LEFT_CENTER,
        &name,
        egui::FontId::proportional(theme::SIZE_VALUE),
        theme::CREAM,
    );
    let sub = if in_a_call {
        "Incoming call · answering ends this one".to_string()
    } else {
        match chat_label {
            Some(chat) if !chat.eq_ignore_ascii_case(&name) => format!("Incoming call · {chat}"),
            _ => "Incoming call".to_string(),
        }
    };
    p.text(
        egui::pos2(text_x, rect.center().y + 10.0),
        egui::Align2::LEFT_CENTER,
        sub,
        egui::FontId::proportional(theme::SIZE_SUB),
        theme::TEXT_DIM,
    );
    let y = rect.center().y;
    let answer = egui::pos2(rect.max.x - 12.0 - CALL_BUTTON_SMALL * 0.5, y);
    let decline = egui::pos2(answer.x - CALL_BUTTON_SMALL - 10.0, y);
    let mut pressed = false;
    if button_at(
        ui,
        decline,
        icons::CALL_END,
        "Decline",
        CallTone::HangUp,
        CALL_BUTTON_SMALL,
    ) {
        *request = Some(CallRequest::Dismiss(r.group.clone()));
        pressed = true;
    }
    if button_at(
        ui,
        answer,
        icons::CALL,
        "Answer",
        CallTone::Accept,
        CALL_BUTTON_SMALL,
    ) {
        *request = Some(CallRequest::Join(r.group.clone()));
        pressed = true;
    }
    resp.clicked() && !pressed
}

/// A notice chip under the bar. Answers whether it was tapped away.
fn notice_chip(ui: &mut egui::Ui, width: f32, text: &str) -> bool {
    let galley = ui.painter().layout(
        text.to_string(),
        egui::FontId::proportional(theme::SIZE_BODY),
        theme::CREAM,
        width - 32.0,
    );
    let h = galley.size().y + 20.0;
    let (rect, resp) = ui.allocate_exact_size(egui::vec2(width, h), egui::Sense::click());
    let p = ui.painter();
    p.rect_filled(
        rect,
        egui::CornerRadius::same(theme::RADIUS_LG),
        theme::LABEL_CHIP,
    );
    p.galley(
        egui::pos2(rect.center().x - galley.size().x * 0.5, rect.min.y + 10.0),
        galley,
        theme::CREAM,
    );
    resp.on_hover_text("Dismiss").clicked()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_picture_fills_its_rect_when_cropped() {
        let rect = egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(400.0, 800.0));
        // A landscape picture on a portrait screen keeps its middle.
        let uv = cover_uv(4.0 / 3.0, rect);
        assert!((uv.width() - 0.375).abs() < 1e-4, "{uv:?}");
        assert!((uv.height() - 1.0).abs() < 1e-4);
        assert!((uv.center().x - 0.5).abs() < 1e-4);
        // A portrait picture of the screen's own shape is not cropped at all.
        let uv = cover_uv(0.5, rect);
        assert!((uv.width() - 1.0).abs() < 1e-4 && (uv.height() - 1.0).abs() < 1e-4);
    }

    #[test]
    fn a_letterboxed_picture_keeps_its_shape() {
        let rect = egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(1600.0, 900.0));
        let r = contain(0.75, rect);
        assert!((r.height() - 900.0).abs() < 1e-3);
        assert!((r.width() / r.height() - 0.75).abs() < 1e-4);
        assert!((r.center().x - 800.0).abs() < 1e-3);
    }
}

/// Who is large in Speaker view, if it is on: the hosts' spotlight first,
/// then whoever is talking, kept until somebody else talks. Needs two others,
/// or the one other person fills the screen anyway.
fn speaker_of(calls: &Calls, st: &mut CallScreenState, people: &[Person]) -> Option<u64> {
    if let Some(s) = calls.extras().spotlight {
        if people.iter().any(|p| p.id == s) {
            return Some(s);
        }
    }
    if !st.speaker_view || people.len() < 2 {
        return None;
    }
    if let Some(talking) = people.iter().find(|p| p.speaking) {
        st.big = Some(talking.id);
    }
    match st.big {
        Some(b) if people.iter().any(|p| p.id == b) => Some(b),
        _ => people.first().map(|p| p.id),
    }
}

/// Waiting for a host to let this device in: your own picture, if any, behind
/// who you wait for, and Leave.
fn waiting_screen(
    ui: &mut egui::Ui,
    full: egui::Rect,
    calls: &mut Calls,
    st: &mut CallScreenState,
    chat_label: Option<&str>,
    request: &mut Option<CallRequest>,
) {
    let p = ui.painter().clone();
    if let Some(tex) = texture_of(&st.tiles, SELF_TILE) {
        paint_picture(ui, full, tex, 0, true);
        p.rect_filled(full, 0.0, egui::Color32::from_black_alpha(150));
    }
    let x = full.center().x;
    let y = full.min.y + full.height() * 0.38;
    let spin = (ui.input(|i| i.time) * 4.0) as f32;
    let r = 18.0;
    p.circle_stroke(
        egui::pos2(x, y),
        r,
        egui::Stroke::new(3.0, egui::Color32::from_white_alpha(40)),
    );
    let arc: Vec<egui::Pos2> = (0..=12)
        .map(|i| {
            let a = spin + i as f32 * 0.12;
            egui::pos2(x + r * a.cos(), y + r * a.sin())
        })
        .collect();
    p.add(egui::Shape::line(arc, egui::Stroke::new(3.0, theme::WARN)));
    centred(
        &p,
        egui::pos2(x, y + 54.0),
        chat_label.unwrap_or("The call"),
        26.0,
        theme::CREAM,
    );
    centred(
        &p,
        egui::pos2(x, y + 88.0),
        "Waiting for a host to let you in",
        theme::SIZE_BODY,
        theme::TEXT,
    );
    encrypted_line(&p, egui::pos2(x, full.min.y + EDGE + 20.0));
    if fold_button(ui, full) {
        calls.minimized = true;
    }
    let c = egui::pos2(x, full.max.y - BOTTOM_EDGE - CALL_BUTTON_LARGE * 0.5 - 8.0);
    if button_at(
        ui,
        c,
        icons::CALL_END,
        "Stop waiting",
        CallTone::HangUp,
        CALL_BUTTON_LARGE,
    ) {
        *request = Some(CallRequest::Leave);
    }
    centred(
        &p,
        c + egui::vec2(0.0, CALL_BUTTON_LARGE * 0.5 + 12.0),
        "Leave",
        theme::SIZE_TINY,
        theme::TEXT,
    );
    ui.ctx()
        .request_repaint_after(std::time::Duration::from_millis(40));
}

/// What the meeting says about everybody, under the title: somebody records,
/// this device is in a breakout room (with its time left, and the way back
/// when the rooms allow it).
fn meeting_chips(
    ui: &mut egui::Ui,
    full: egui::Rect,
    calls: &Calls,
    request: &mut Option<CallRequest>,
) {
    let p = ui.painter().clone();
    let mut y = full.min.y + EDGE + 60.0;
    let recorders: Vec<&str> = calls
        .members()
        .iter()
        .filter(|m| m.recording)
        .map(|m| m.username.as_str())
        .collect();
    if !recorders.is_empty() {
        let text = format!("●  Recording · {}", recorders.join(", "));
        let galley = p.layout_no_wrap(
            text,
            egui::FontId::proportional(theme::SIZE_SUB),
            theme::CREAM,
        );
        let rect = egui::Rect::from_center_size(
            egui::pos2(full.center().x, y),
            galley.size() + egui::vec2(20.0, 8.0),
        );
        p.rect_filled(
            rect,
            egui::CornerRadius::same(theme::RADIUS_PILL),
            theme::CALL_END_RED.gamma_multiply(0.8),
        );
        p.galley(rect.min + egui::vec2(10.0, 4.0), galley, theme::CREAM);
        y += 30.0;
    }
    let extras = calls.extras();
    let my_room = calls.me().map_or(0, |m| m.room);
    let (Some(rooms), true) = (extras.rooms.as_ref(), my_room > 0) else {
        return;
    };
    let name = rooms
        .names
        .get(my_room as usize - 1)
        .map_or("a breakout room", String::as_str);
    let left = rooms.until_ms.map(|until| {
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |d| d.as_millis() as u64);
        let secs = until.saturating_sub(now) / 1000;
        format!(" · {}:{:02} left", secs / 60, secs % 60)
    });
    let text = format!("{}  In {name}{}", icons::PEOPLE, left.unwrap_or_default());
    let galley = p.layout_no_wrap(
        text,
        egui::FontId::proportional(theme::SIZE_SUB),
        theme::INK,
    );
    let rect = egui::Rect::from_center_size(
        egui::pos2(full.center().x, y),
        galley.size() + egui::vec2(20.0, 8.0),
    );
    p.rect_filled(
        rect,
        egui::CornerRadius::same(theme::RADIUS_PILL),
        theme::ACCENT,
    );
    p.galley(rect.min + egui::vec2(10.0, 4.0), galley, theme::INK);
    if rooms.may_move {
        let back = egui::Rect::from_min_size(
            egui::pos2(rect.max.x + 6.0, rect.min.y),
            egui::vec2(110.0, rect.height()),
        );
        let resp = ui.interact(back, egui::Id::new("call_room_back"), egui::Sense::click());
        p.rect_filled(
            back,
            egui::CornerRadius::same(theme::RADIUS_PILL),
            egui::Color32::from_white_alpha(if resp.hovered() { 46 } else { 30 }),
        );
        centred(
            &p,
            back.center(),
            "Back to the call",
            theme::SIZE_SUB,
            theme::CREAM,
        );
        if resp.clicked() {
            if let Some(me) = calls.me() {
                *request = Some(CallRequest::Control(CallAction::MoveTo {
                    participant: me.participant,
                    room: 0,
                }));
            }
        }
    }
}

/// Split everybody but the hosts into `n` rooms, in turn, after shuffling by
/// participant number and the clock (a host can move anybody after).
fn split_into_rooms(calls: &Calls, n: u32) -> CallAction {
    let mut people: Vec<u64> = calls
        .members()
        .iter()
        .filter(|m| !m.role.hosts())
        .map(|m| m.participant)
        .collect();
    let seed = web_time::SystemTime::now()
        .duration_since(web_time::UNIX_EPOCH)
        .map_or(0, |d| d.as_nanos() as u64);
    people.sort_by_key(|p| p.wrapping_mul(0x9E37_79B9_7F4A_7C15) ^ seed);
    CallAction::OpenRooms {
        names: (1..=n).map(|i| format!("Room {i}")).collect(),
        assign: people
            .iter()
            .enumerate()
            .map(|(i, p)| (*p, i as u32 % n + 1))
            .collect(),
        minutes: 10,
        may_move: true,
    }
}
