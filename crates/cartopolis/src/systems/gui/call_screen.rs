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

use std::collections::{HashMap, HashSet};

use bevy::prelude::*;

use crate::resources::{AuthIdentity, OnlineSession};
use crate::systems::calls::{camera_available, CallRequest, Calls, Ringing, SELF_TILE};
use crate::systems::chat::ChatState;
use crate::systems::chats::{paint_avatar, ChatKind};
use crate::systems::egui_manager::EguiManager;
use crate::systems::icons;
use crate::systems::private_chat::channel_key;
use crate::systems::theme::{self, CallTone, CALL_BUTTON_LARGE, CALL_BUTTON_SMALL};

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
    mut egui: ResMut<EguiManager>,
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
        upload_pictures(&ctx, &mut calls, &mut st.tiles);
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
) {
    let mut present = HashSet::new();
    for (id, p) in calls.pictures_mut() {
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
                let t = ctx.load_texture(
                    format!("call-video-{id}"),
                    image,
                    egui::TextureOptions::LINEAR,
                );
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
    let mut people: Vec<Person> = calls
        .others()
        .into_iter()
        .map(|(id, name)| Person {
            id,
            texture: texture_of(&st.tiles, id),
            speaking: calls.speaking(id),
            name,
        })
        .collect();
    people.sort_by(|a, b| a.name.cmp(&b.name));
    let own = texture_of(&st.tiles, SELF_TILE);
    let any_picture =
        people.iter().any(|p| p.texture.is_some()) || (people.is_empty() && own.is_some());
    // Over a picture the controls fade after a while; over initials they stay.
    if !any_picture {
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
    let title = if people.is_empty() {
        chat_label.unwrap_or_else(|| "Calling…".into())
    } else {
        people
            .iter()
            .map(|p| p.name.as_str())
            .collect::<Vec<_>>()
            .join(", ")
    };

    // ── The stage ──────────────────────────────────────────────────────────
    let mut pip = own;
    let mut centre_title = false;
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
                Some(tex) => paint_picture(ui, full, tex, 0, false),
                None => centre_title = true,
            }
        }
        others => {
            // You are one of the tiles in a group, last, rather than a window
            // laid over somebody else's.
            pip = None;
            let n = others + 1;
            let top_room = 72.0;
            let bottom_room = CALL_BUTTON_LARGE + BOTTOM_EDGE + 24.0;
            let stage = egui::Rect::from_min_max(
                egui::pos2(full.min.x + 8.0, full.min.y + top_room),
                egui::pos2(full.max.x - 8.0, full.max.y - bottom_room),
            );
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
            let rows = n.div_ceil(cols);
            let gap = 8.0;
            let cell = egui::vec2(
                (stage.width() - gap * (cols - 1) as f32) / cols as f32,
                (stage.height() - gap * (rows - 1) as f32) / rows as f32,
            );
            for (i, person) in people.iter().enumerate() {
                let (r, c) = (i / cols, i % cols);
                let rect = egui::Rect::from_min_size(
                    stage.min + egui::vec2(c as f32 * (cell.x + gap), r as f32 * (cell.y + gap)),
                    cell,
                );
                grid_tile(ui, rect, person);
            }
            let i = n - 1;
            let (r, c) = (i / cols, i % cols);
            let rect = egui::Rect::from_min_size(
                stage.min + egui::vec2(c as f32 * (cell.x + gap), r as f32 * (cell.y + gap)),
                cell,
            );
            grid_tile(
                ui,
                rect,
                &Person {
                    id: SELF_TILE,
                    name: "You".into(),
                    texture: own,
                    speaking: calls.speaking(SELF_TILE),
                },
            );
        }
    }
    if centre_title {
        // No picture to look at: the person, large, in the middle.
        let disc = (full.width().min(full.height()) * 0.34).clamp(96.0, 150.0);
        let c = egui::pos2(x, full.min.y + full.height() * 0.34);
        let speaking = people.first().is_some_and(|p| p.speaking);
        if speaking {
            p.circle_stroke(c, disc * 0.5 + 6.0, egui::Stroke::new(3.0, theme::BRAND));
        }
        paint_avatar(&p, c, &title, ChatKind::Private, disc);
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
            p.circle_filled(badge, 12.0, egui::Color32::from_black_alpha(170));
            centred(&p, badge, icons::MIC_OFF, 14.0, theme::CREAM);
        }
    }

    if !show_controls {
        return;
    }

    // ── The top: fold, name, timer ─────────────────────────────────────────
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
    let _ = me;

    // ── The bottom: camera, microphone, hang up ────────────────────────────
    // Each button carries a word under it: a phone never shows a hover tip,
    // and an icon alone left "is the crossed-out camera the state or the
    // action?" to guess.
    const LABEL_GAP: f32 = 16.0;
    let y = full.max.y - BOTTOM_EDGE - CALL_BUTTON_LARGE * 0.5 - LABEL_GAP * 0.5;
    let step = CALL_BUTTON_LARGE + 24.0;
    let with_camera = camera_available();
    let n = if with_camera { 3.0 } else { 2.0 };
    let mut cx = x - (n - 1.0) * step * 0.5;
    // A dark rounded tray behind the row, so it reads over any picture.
    let tray = egui::Rect::from_center_size(
        egui::pos2(x, y + LABEL_GAP * 0.5),
        egui::vec2(
            (n - 1.0) * step + CALL_BUTTON_LARGE + 28.0,
            CALL_BUTTON_LARGE + 20.0 + LABEL_GAP,
        ),
    );
    p.rect_filled(
        tray,
        egui::CornerRadius::same(theme::RADIUS_PILL),
        egui::Color32::from_black_alpha(110),
    );
    let label = |cx: f32, text: &str| {
        centred(
            &p,
            egui::pos2(cx, y + CALL_BUTTON_LARGE * 0.5 + LABEL_GAP * 0.5 + 2.0),
            text,
            theme::SIZE_TINY,
            theme::TEXT,
        );
    };
    let mut touched = false;
    if with_camera {
        let (icon, tip, tone) = if calls.camera_on {
            (icons::VIDEOCAM, "Turn the camera off", CallTone::On)
        } else {
            (icons::VIDEOCAM_OFF, "Turn the camera on", CallTone::Off)
        };
        if button_at(ui, egui::pos2(cx, y), icon, tip, tone, CALL_BUTTON_LARGE) {
            calls.camera_on = !calls.camera_on;
            touched = true;
        }
        label(cx, "Camera");
        cx += step;
    }
    let (icon, tip, tone) = if calls.muted {
        (icons::MIC_OFF, "Unmute", CallTone::Off)
    } else {
        (icons::MIC, "Mute", CallTone::On)
    };
    if button_at(ui, egui::pos2(cx, y), icon, tip, tone, CALL_BUTTON_LARGE) {
        calls.muted = !calls.muted;
        touched = true;
    }
    label(cx, if calls.muted { "Unmute" } else { "Mute" });
    cx += step;
    if button_at(
        ui,
        egui::pos2(cx, y),
        icons::CALL_END,
        "Hang up",
        CallTone::HangUp,
        CALL_BUTTON_LARGE,
    ) {
        *request = Some(CallRequest::Leave);
    }
    label(cx, "End");
    if touched {
        st.touched = Some(web_time::Instant::now());
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
            let d = (rect.width().min(rect.height()) * 0.42).clamp(40.0, 120.0);
            paint_avatar(p, rect.center(), &person.name, ChatKind::Private, d);
        }
    }
    // The name on a chip over the bottom-left corner, where it reads on any
    // picture.
    let galley = p.layout_no_wrap(
        person.name.clone(),
        egui::FontId::proportional(theme::SIZE_SUB),
        theme::CREAM,
    );
    let pad = egui::vec2(8.0, 3.0);
    let chip = egui::Rect::from_min_size(
        egui::pos2(
            rect.min.x + 8.0,
            rect.max.y - 8.0 - galley.size().y - 2.0 * pad.y,
        ),
        galley.size() + 2.0 * pad,
    );
    p.rect_filled(
        chip,
        egui::CornerRadius::same(theme::RADIUS_PILL),
        egui::Color32::from_black_alpha(160),
    );
    p.galley(chip.min + pad, galley, theme::CREAM);
    if person.speaking {
        p.rect_stroke(
            rect,
            radius,
            egui::Stroke::new(3.0, theme::BRAND),
            egui::StrokeKind::Inside,
        );
    }
    let _ = person.id;
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
