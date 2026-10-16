//! People's pictures: the face a person's disc shows instead of their initial,
//! in every list, call and map marker (`docs/notes/profile-pictures.md`).
//!
//! **Every disc already goes through one function**, `chats::paint_avatar`, and
//! it takes a painter and a name — no `World`, no resource. So the pictures
//! reach it through [`texture_for`], a small shared table this module rewrites
//! every frame, and a disc asking for a name it has no picture for is how this
//! module learns whom to look up. Twenty call sites draw pictures with no edit,
//! and a new one will too.
//!
//! **A picture is a hash, looked up by name.** The account names one
//! (`PUT /api/pictures/mine`) out of the picture store everything else uses, so
//! the bytes are fetched, cached on disk and forgotten exactly as a community's
//! picture is. Names are asked in batches (`GET /api/pictures/people`) and asked
//! again every [`REFRESH_S`], which is how somebody else's new picture arrives:
//! nothing is pushed.
//!
//! **Three ways to make one**, each ending in the same preview, framed as a
//! polaroid with Use or Again: a file, the camera facing you, and a polaroid of
//! the world — a square viewfinder over the map, which you frame yourself (Face
//! me swings the follow camera round in front of your avatar) and shoot with the
//! panels hidden, the same chrome-free capture a chat's snapshot makes.

use std::collections::{HashMap, HashSet};
use std::sync::LazyLock;

use bevy::prelude::*;
use bevy::tasks::{IoTaskPool, Task};

use crate::components::{Avatar, FreeCamera};
use crate::platform::photo::{Capture, Outcome, Source};
use crate::platform::sync::Mutex;
use crate::resources::{CameraMode, CameraState, OnlineSession, RightPanel};
use crate::systems::egui_manager::{EguiMut, EguiRef};
use crate::systems::{icons, theme};

/// How long a name's answer is believed before it is asked again.
const REFRESH_S: f64 = 300.0;
/// How long after a failed lookup a name is asked again.
const RETRY_S: f64 = 30.0;
/// At most one lookup this often, whatever is waiting.
const LOOKUP_EVERY_S: f64 = 0.5;
/// Pictures kept as textures before the ones nobody has drawn lately go. A
/// 256-pixel square is 256 KB of graphics memory.
const MAX_TEXTURES: usize = 64;
/// A picture not drawn for this long may be let go once over [`MAX_TEXTURES`].
const UNSEEN_S: f64 = 30.0;
/// The viewfinder's square, as a share of the map's shorter side. The capture
/// is cropped to the same share of the window's, so what is in the frame is
/// what is in the picture.
const FINDER_SHARE: f32 = 0.8;
/// How far in front of your avatar Face me puts the camera, and how high.
const FACE_ME_DISTANCE_M: f32 = 2.4;
const FACE_ME_PITCH: f32 = 0.12;

/// What `paint_avatar` reads and writes, without a `World` to reach.
#[derive(Default)]
struct Bridge {
    /// The texture to paint for each name that has one.
    shown: HashMap<String, egui::TextureId>,
    /// Names painted since the last frame, by key.
    seen: HashSet<String>,
    /// This server's name, so `name@here` reads as `name` and anybody else's
    /// server is left alone: their names mean nothing to this one's accounts.
    server: Option<String>,
    /// Off while signed out: there is nobody to ask.
    on: bool,
}

static BRIDGE: LazyLock<Mutex<Bridge>> = LazyLock::new(Default::default);

/// The account name a disc's label stands for, if it can be looked up here.
fn key_of(name: &str, server: Option<&str>) -> Option<String> {
    let name = name.trim();
    let user = match name.split_once('@') {
        Some((user, host)) if server.is_some_and(|s| s.eq_ignore_ascii_case(host)) => user,
        Some(_) => return None,
        None => name,
    };
    let user = user.to_lowercase();
    let plausible = !user.is_empty()
        && user.len() <= 64
        && user
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '-' | '.'));
    plausible.then_some(user)
}

/// The picture to paint for a person's disc, if they have one and it has
/// arrived. Asking is also what gets it fetched.
#[must_use]
pub fn texture_for(name: &str) -> Option<egui::TextureId> {
    let mut bridge = BRIDGE.lock().ok()?;
    if !bridge.on {
        return None;
    }
    let key = key_of(name, bridge.server.as_deref())?;
    let texture = bridge.shown.get(&key).copied();
    bridge.seen.insert(key);
    texture
}

/// A picture inside a circle of radius `r`: a fan of triangles with the
/// texture laid across it, centre-cropped (the pictures are square already).
pub fn paint_face(p: &egui::Painter, c: egui::Pos2, r: f32, texture: egui::TextureId) {
    const SEGMENTS: u32 = 48;
    let mut mesh = egui::Mesh::with_texture(texture);
    let white = egui::Color32::WHITE;
    mesh.vertices.push(egui::epaint::Vertex {
        pos: c,
        uv: egui::pos2(0.5, 0.5),
        color: white,
    });
    for i in 0..=SEGMENTS {
        let a = i as f32 / SEGMENTS as f32 * std::f32::consts::TAU;
        let d = egui::vec2(a.cos(), a.sin());
        mesh.vertices.push(egui::epaint::Vertex {
            pos: c + r * d,
            uv: egui::pos2(0.5 + d.x * 0.5, 0.5 + d.y * 0.5),
            color: white,
        });
        if i > 0 {
            mesh.add_triangle(0, i, i + 1);
        }
    }
    p.add(mesh);
    // The fan's edge is not anti-aliased; a hairline over it is.
    p.circle_stroke(
        c,
        r - 0.5,
        egui::Stroke::new(1.0, egui::Color32::from_white_alpha(40)),
    );
}

/// One person's answer: their picture's hash, or none, and when it was asked.
struct Known {
    hash: Option<String>,
    asked: f64,
    seen: f64,
}

/// A picture by hash: on its way, a texture, or not to be had.
enum Picture {
    Loading(Task<Result<(Vec<u8>, [usize; 2]), String>>),
    Ready(egui::TextureHandle),
    Failed,
}

/// A picture made here and not yet chosen.
pub struct Preview {
    jpeg: Vec<u8>,
    pixels: Option<(Vec<u8>, [usize; 2])>,
    texture: Option<egui::TextureHandle>,
}

type Prepared = Result<(Vec<u8>, (Vec<u8>, [usize; 2])), String>;
type Lookup = Result<HashMap<String, Option<String>>, String>;

#[derive(Resource, Default)]
pub struct Faces {
    known: HashMap<String, Known>,
    pictures: HashMap<String, Picture>,
    lookup: Option<(Vec<String>, Task<Lookup>)>,
    last_lookup: f64,
    /// Who the table is for: a different account starts it again.
    account: Option<String>,
    /// A source the You page asked for, started next frame.
    pub pick: Option<Source>,
    capture: Option<Capture>,
    prepare: Option<Task<Prepared>>,
    pub preview: Option<Preview>,
    /// Upload and naming; the answer is the new hash, or `None` for removed.
    saving: Option<Task<Result<Option<String>, String>>>,
    /// What to tell the person, in words.
    pub note: Option<String>,
    /// The polaroid's viewfinder is over the map.
    pub viewfinder: bool,
    /// The shutter was pressed; the pixels arrive in [`Faces::snapshot_in`].
    pub shooting: bool,
    /// The window, as `ui::drive_screenshots` captured it for the polaroid.
    pub snapshot_in: Option<(Vec<u8>, [usize; 2])>,
}

impl Faces {
    /// Your own picture's hash, as far as this client knows.
    #[must_use]
    pub fn mine(&self, me: &str) -> Option<&str> {
        self.known
            .get(&me.to_lowercase())
            .and_then(|k| k.hash.as_deref())
    }

    /// Whether something is under way, so the buttons wait.
    #[must_use]
    pub fn busy(&self) -> bool {
        self.capture.is_some() || self.prepare.is_some() || self.saving.is_some() || self.viewfinder
    }

    /// Use the preview as your picture.
    pub fn use_preview(&mut self) {
        let Some(preview) = self.preview.as_ref() else {
            return;
        };
        let jpeg = preview.jpeg.clone();
        self.note = Some("Saving your picture…".into());
        self.saving = Some(IoTaskPool::get().spawn(async move {
            let hash = crate::systems::model_assets::api_upload_picture(jpeg).await?;
            name_mine(Some(&hash)).await?;
            Ok(Some(hash))
        }));
    }

    /// Go back to your initial.
    pub fn remove(&mut self) {
        self.note = Some("Removing your picture…".into());
        self.saving = Some(IoTaskPool::get().spawn(async move {
            name_mine(None).await?;
            Ok(None)
        }));
    }

    fn start_prepare(&mut self, work: impl FnOnce() -> Prepared + Send + 'static) {
        self.note = None;
        self.prepare = Some(IoTaskPool::get().spawn(async move { work() }));
    }
}

async fn name_mine(hash: Option<&str>) -> Result<(), String> {
    let body = serde_json::json!({ "picture": hash }).to_string();
    crate::systems::model_assets::api_put("/api/pictures/mine", body)
        .await
        .map(|_| ())
}

/// The centre square of a window capture, [`FINDER_SHARE`] of its shorter side
/// — what the viewfinder framed.
fn crop_finder(rgba: &[u8], size: [usize; 2]) -> (Vec<u8>, [usize; 2]) {
    let [w, h] = size;
    let side = ((w.min(h) as f32 * FINDER_SHARE) as usize).max(1);
    let (x0, y0) = ((w - side) / 2, (h - side) / 2);
    let mut out = Vec::with_capacity(side * side * 4);
    for y in y0..y0 + side {
        let row = (y * w + x0) * 4;
        out.extend_from_slice(&rgba[row..row + side * 4]);
    }
    (out, [side, side])
}

/// Keep the table: look up the names discs asked for, fetch their pictures,
/// make the textures, and drive the picker.
#[allow(clippy::too_many_arguments)]
pub fn drive_faces(
    egui: EguiRef,
    time: Res<Time>,
    online: Option<Res<OnlineSession>>,
    identity: Res<crate::resources::AuthIdentity>,
    mut faces: ResMut<Faces>,
) {
    let now = time.elapsed_secs_f64();
    let ctx = egui.ctx();
    let signed_in = online.is_some() && crate::systems::model_assets::api_base().is_some();
    let account = identity
        .username
        .clone()
        .filter(|_| signed_in)
        .map(|u| u.to_lowercase());
    if faces.account != account {
        // Somebody else, or nobody: nothing known so far is theirs to see.
        if let Ok(mut b) = BRIDGE.lock() {
            b.shown.clear();
            b.seen.clear();
        }
        *faces = Faces {
            account: account.clone(),
            ..Faces::default()
        };
    }
    let seen = match BRIDGE.lock() {
        Ok(mut b) => {
            b.on = account.is_some();
            b.server = identity.server_name.clone();
            std::mem::take(&mut b.seen)
        }
        Err(_) => return,
    };
    if account.is_none() {
        return;
    }

    // Whom to ask about: drawn, and never asked or asked long enough ago.
    for key in seen {
        let k = faces.known.entry(key).or_insert(Known {
            hash: None,
            asked: f64::NEG_INFINITY,
            seen: now,
        });
        k.seen = now;
    }
    if let Some((names, task)) = faces.lookup.as_mut() {
        if let Some(result) = crate::platform::http::poll_task(task) {
            let names = std::mem::take(names);
            faces.lookup = None;
            match result {
                Ok(answer) => {
                    for name in names {
                        if let Some(k) = faces.known.get_mut(&name) {
                            k.hash = answer.get(&name).cloned().flatten();
                            k.asked = now;
                        }
                    }
                }
                Err(why) => {
                    tracing::warn!(error = %why, "people's pictures did not load");
                    for name in names {
                        if let Some(k) = faces.known.get_mut(&name) {
                            k.asked = now - REFRESH_S + RETRY_S;
                        }
                    }
                }
            }
        }
    }
    if faces.lookup.is_none() && now - faces.last_lookup >= LOOKUP_EVERY_S {
        let due: Vec<String> = faces
            .known
            .iter()
            .filter(|(_, k)| now - k.asked >= REFRESH_S)
            .map(|(n, _)| n.clone())
            .take(MAX_PER_LOOKUP)
            .collect();
        if !due.is_empty() {
            faces.last_lookup = now;
            for name in &due {
                if let Some(k) = faces.known.get_mut(name) {
                    k.asked = now;
                }
            }
            let path = format!("/api/pictures/people?names={}", due.join(","));
            let task = IoTaskPool::get().spawn(async move {
                let body = crate::systems::model_assets::api_get(&path).await?;
                let v: serde_json::Value =
                    serde_json::from_str(&body).map_err(|e| e.to_string())?;
                let map = v
                    .get("pictures")
                    .and_then(|p| p.as_object())
                    .ok_or("no pictures in the answer")?;
                Ok(map
                    .iter()
                    .map(|(k, v)| (k.clone(), v.as_str().map(str::to_string)))
                    .collect())
            });
            faces.lookup = Some((due, task));
        }
    }

    // The pictures themselves.
    let wanted: Vec<String> = faces
        .known
        .values()
        .filter_map(|k| k.hash.clone())
        .collect();
    for hash in wanted {
        faces.pictures.entry(hash.clone()).or_insert_with(|| {
            Picture::Loading(IoTaskPool::get().spawn(async move {
                let jpeg = crate::systems::model_assets::fetch_picture(&hash).await?;
                crate::utils::photo::decode_rgba(&jpeg)
            }))
        });
    }
    for (hash, picture) in &mut faces.pictures {
        if let Picture::Loading(task) = picture {
            if let Some(result) = crate::platform::http::poll_task(task) {
                *picture = match result {
                    Ok((rgba, size)) => Picture::Ready(ctx.load_texture(
                        format!("face_{}", &hash[..hash.len().min(16)]),
                        egui::ColorImage::from_rgba_unmultiplied(size, &rgba),
                        egui::TextureOptions::LINEAR,
                    )),
                    Err(why) => {
                        tracing::warn!(error = %why, "a person's picture did not load");
                        Picture::Failed
                    }
                };
            }
        }
    }
    // Over the budget, let go of the pictures nobody has drawn lately. They
    // come back from the disk cache when somebody is.
    let ready = faces
        .pictures
        .values()
        .filter(|p| matches!(p, Picture::Ready(_)))
        .count();
    if ready > MAX_TEXTURES {
        let recent: HashSet<String> = faces
            .known
            .values()
            .filter(|k| now - k.seen < UNSEEN_S)
            .filter_map(|k| k.hash.clone())
            .collect();
        faces
            .pictures
            .retain(|hash, p| !matches!(p, Picture::Ready(_)) || recent.contains(hash));
    }
    let shown: HashMap<String, egui::TextureId> = faces
        .known
        .iter()
        .filter_map(|(name, k)| match faces.pictures.get(k.hash.as_ref()?) {
            Some(Picture::Ready(t)) => Some((name.clone(), t.id())),
            _ => None,
        })
        .collect();
    if let Ok(mut b) = BRIDGE.lock() {
        b.shown = shown;
    }

    drive_picker(&mut faces, ctx, account.as_deref().unwrap_or_default(), now);
}

/// The most names one lookup asks about; the server answers up to 100.
pub const MAX_PER_LOOKUP: usize = 100;

fn drive_picker(faces: &mut Faces, ctx: &egui::Context, me: &str, now: f64) {
    if let Some(source) = faces.pick.take() {
        match crate::platform::photo::start_from(source) {
            Ok(capture) => faces.capture = Some(capture),
            Err(why) => faces.note = Some(why),
        }
    }
    if let Some(capture) = faces.capture.as_mut() {
        match capture.poll() {
            Outcome::Pending => {}
            Outcome::Ready(bytes) => {
                faces.capture = None;
                faces.start_prepare(move || crate::utils::photo::face(&bytes));
            }
            Outcome::Cancelled => faces.capture = None,
            Outcome::Failed(why) => {
                faces.capture = None;
                faces.note = Some(why);
            }
        }
    }
    if let Some((rgba, size)) = faces.snapshot_in.take() {
        faces.shooting = false;
        faces.start_prepare(move || {
            let (square, side) = crop_finder(&rgba, size);
            crate::utils::photo::face_from_rgba(square, side)
        });
    }
    if let Some(task) = faces.prepare.as_mut() {
        if let Some(result) = crate::platform::http::poll_task(task) {
            faces.prepare = None;
            match result {
                Ok((jpeg, pixels)) => {
                    faces.preview = Some(Preview {
                        jpeg,
                        pixels: Some(pixels),
                        texture: None,
                    });
                }
                Err(why) => faces.note = Some(why),
            }
        }
    }
    if let Some(preview) = faces.preview.as_mut() {
        if preview.texture.is_none() {
            if let Some((rgba, size)) = preview.pixels.as_ref() {
                preview.texture = Some(ctx.load_texture(
                    "face_preview",
                    egui::ColorImage::from_rgba_unmultiplied(*size, rgba),
                    egui::TextureOptions::LINEAR,
                ));
            }
        }
    }
    if let Some(task) = faces.saving.as_mut() {
        if let Some(result) = crate::platform::http::poll_task(task) {
            faces.saving = None;
            match result {
                Ok(hash) => {
                    faces.note = None;
                    // Shown at once from the preview's own pixels, rather than
                    // fetched back from where it was just sent.
                    if let (Some(h), Some(preview)) = (hash.as_ref(), faces.preview.take()) {
                        if let Some(t) = preview.texture {
                            faces.pictures.insert(h.clone(), Picture::Ready(t));
                        }
                    }
                    faces.preview = None;
                    faces.known.insert(
                        me.to_string(),
                        Known {
                            hash,
                            asked: now,
                            seen: now,
                        },
                    );
                }
                Err(why) => faces.note = Some(why),
            }
        }
    }
}

/// Your picture on the You page: the disc, or the preview as a polaroid, and
/// the ways to change it.
pub fn you_picture(ui: &mut egui::Ui, faces: &mut Faces, me: &str, panel: &mut Option<RightPanel>) {
    if let Some(preview) = faces.preview.as_ref() {
        if let Some(texture) = preview.texture.as_ref() {
            polaroid(ui, texture, me);
        }
        ui.add_space(8.0);
        let saving = faces.saving.is_some();
        centred_row(ui, 230.0, |ui| {
            if ui
                .add_enabled(!saving, pill(icons::CHECK, "Use this"))
                .clicked()
            {
                faces.use_preview();
            }
            if ui
                .add_enabled(!saving, pill(icons::CLEAR, "Again"))
                .clicked()
            {
                faces.preview = None;
            }
        });
    } else {
        let (rect, _) = ui.allocate_exact_size(egui::Vec2::splat(96.0), egui::Sense::hover());
        ui.painter().circle_stroke(
            rect.center(),
            48.0,
            egui::Stroke::new(1.5, theme::TEXT_DIM.gamma_multiply(0.4)),
        );
        let name = if me.is_empty() { "?" } else { me };
        crate::systems::chats::paint_avatar(
            ui.painter(),
            rect.center(),
            name,
            crate::systems::chats::ChatKind::Private,
            90.0,
        );
        ui.add_space(6.0);
        let busy = faces.busy();
        let has = faces.mine(me).is_some();
        let mut buttons: Vec<(&str, &str, u8)> = Vec::new();
        if crate::platform::photo::offers(Source::File) {
            buttons.push((icons::ADD, "Photo", 0));
        }
        if crate::platform::photo::offers(Source::Selfie) {
            buttons.push((icons::CAMERA, "Camera", 1));
        }
        buttons.push((icons::CAM_FOLLOW, "Polaroid", 2));
        if has {
            buttons.push((icons::DELETE, "Remove", 3));
        }
        centred_row(ui, 92.0 * buttons.len() as f32, |ui| {
            for (icon, label, which) in buttons {
                if ui.add_enabled(!busy, pill(icon, label)).clicked() {
                    match which {
                        0 => faces.pick = Some(Source::File),
                        1 => faces.pick = Some(Source::Selfie),
                        2 => {
                            faces.viewfinder = true;
                            *panel = Some(RightPanel::None);
                        }
                        _ => faces.remove(),
                    }
                }
            }
        });
    }
    if let Some(note) = faces.note.as_deref() {
        ui.label(egui::RichText::new(note).size(12.5).color(theme::TEXT_DIM));
    }
}

fn pill(icon: &str, text: &str) -> egui::Button<'static> {
    egui::Button::new(
        egui::RichText::new(format!("{icon} {text}"))
            .size(12.5)
            .color(theme::TEXT),
    )
    .fill(theme::SURFACE_RAISED)
    .corner_radius(egui::CornerRadius::same(12))
    .min_size(egui::vec2(0.0, 32.0))
}

/// A row of buttons, centred as a group of about `width` points.
fn centred_row(ui: &mut egui::Ui, width: f32, add: impl FnOnce(&mut egui::Ui)) {
    let w = ui.available_width();
    ui.allocate_ui_with_layout(
        egui::vec2(w, 36.0),
        egui::Layout::left_to_right(egui::Align::Center),
        |ui| {
            ui.add_space(((w - width) / 2.0).max(0.0));
            add(ui);
        },
    );
}

/// The picture as a polaroid: warm white card, narrow edge, deep foot with
/// your name written on it.
fn polaroid(ui: &mut egui::Ui, texture: &egui::TextureHandle, me: &str) {
    const PHOTO: f32 = 168.0;
    const EDGE: f32 = 10.0;
    const FOOT: f32 = 36.0;
    let size = egui::vec2(PHOTO + 2.0 * EDGE, PHOTO + EDGE + FOOT);
    let (rect, _) = ui.allocate_exact_size(size, egui::Sense::hover());
    let p = ui.painter();
    p.rect_filled(
        rect.translate(egui::vec2(0.0, 3.0)).expand(1.0),
        egui::CornerRadius::same(3),
        egui::Color32::from_black_alpha(70),
    );
    p.rect_filled(
        rect,
        egui::CornerRadius::same(2),
        crate::systems::chats::POLAROID_WHITE,
    );
    let photo =
        egui::Rect::from_min_size(rect.min + egui::vec2(EDGE, EDGE), egui::Vec2::splat(PHOTO));
    egui::Image::new(texture).paint_at(ui, photo);
    ui.painter().text(
        egui::pos2(rect.center().x, photo.max.y + FOOT / 2.0),
        egui::Align2::CENTER_CENTER,
        me,
        egui::FontId::proportional(15.0),
        crate::systems::chats::POLAROID_INK,
    );
}

/// The polaroid's viewfinder: a square over the map, the shutter, Face me and
/// Cancel. Drawn over everything, with the panels shut so the map is the
/// screen.
#[allow(clippy::too_many_arguments)]
pub fn render_face_viewfinder(
    mut egui: EguiMut,
    mut faces: ResMut<Faces>,
    mut camera_state: ResMut<CameraState>,
    avatar: Query<&Transform, (With<Avatar>, Without<FreeCamera>)>,
    mut screenshot: ResMut<crate::resources::ScreenshotState>,
    mut right_panel: ResMut<RightPanel>,
    mut chats_view: ResMut<crate::systems::chats::ChatsView>,
) {
    if !faces.viewfinder {
        return;
    }
    let ctx = egui.ctx_mut().clone();
    let screen = ctx.viewport_rect();
    let side = screen.width().min(screen.height()) * FINDER_SHARE;
    let frame = egui::Rect::from_center_size(screen.center(), egui::Vec2::splat(side));
    // The frame: the world outside it dimmed, the square itself outlined.
    let painter = ctx.layer_painter(egui::LayerId::new(
        egui::Order::Foreground,
        egui::Id::new("face_viewfinder_shade"),
    ));
    let shade = egui::Color32::from_black_alpha(110);
    for r in [
        egui::Rect::from_min_max(screen.min, egui::pos2(screen.max.x, frame.min.y)),
        egui::Rect::from_min_max(egui::pos2(screen.min.x, frame.max.y), screen.max),
        egui::Rect::from_min_max(
            egui::pos2(screen.min.x, frame.min.y),
            egui::pos2(frame.min.x, frame.max.y),
        ),
        egui::Rect::from_min_max(
            egui::pos2(frame.max.x, frame.min.y),
            egui::pos2(screen.max.x, frame.max.y),
        ),
    ] {
        painter.rect_filled(r, egui::CornerRadius::ZERO, shade);
    }
    painter.rect_stroke(
        frame,
        egui::CornerRadius::ZERO,
        egui::Stroke::new(2.0, theme::CREAM),
        egui::StrokeKind::Outside,
    );
    let mut done = false;
    egui::Area::new(egui::Id::new("face_viewfinder_controls"))
        .order(egui::Order::Foreground)
        .anchor(egui::Align2::CENTER_BOTTOM, egui::vec2(0.0, -28.0))
        .show(&ctx, |ui| {
            ui.horizontal(|ui| {
                if ui.add(pill(icons::CLEAR, "Cancel")).clicked() {
                    done = true;
                }
                if ui.add(pill(icons::CAM_FOLLOW, "Face me")).clicked() {
                    if let Ok(body) = avatar.single() {
                        if camera_state.mode != CameraMode::Avatar {
                            crate::systems::free_camera::enter_mode(
                                &mut camera_state,
                                CameraMode::Avatar,
                            );
                            camera_state.mode = CameraMode::Avatar;
                        }
                        // In front: the follow rig sits behind the avatar at
                        // `facing + π`, so at `facing` it looks back at them.
                        let face = body.rotation.to_euler(EulerRot::YXZ).0;
                        camera_state.follow_rotate = false;
                        camera_state.first_person = false;
                        camera_state.azimuth = face;
                        camera_state.pitch = FACE_ME_PITCH;
                        camera_state.distance = FACE_ME_DISTANCE_M;
                    } else {
                        faces.note = Some("Sign in to stand somewhere first.".into());
                    }
                }
                let shutter = egui::Button::new(
                    egui::RichText::new(format!("{}  Take it", icons::CAMERA))
                        .size(15.0)
                        .color(theme::INK)
                        .strong(),
                )
                .fill(theme::ACCENT)
                .corner_radius(egui::CornerRadius::same(18))
                .min_size(egui::vec2(0.0, 40.0));
                if ui.add_enabled(!faces.shooting, shutter).clicked() {
                    // Taken next frame by `ui::drive_screenshots` with the UI
                    // hidden, so neither this frame nor the shade is in it.
                    screenshot.face_snapshot = true;
                    screenshot.hide_ui = true;
                    faces.shooting = true;
                    done = true;
                }
            });
        });
    if done {
        faces.viewfinder = false;
        *right_panel = RightPanel::Chat;
        chats_view.you = true;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A disc's label is looked up only as one of this server's accounts.
    #[test]
    fn only_this_servers_names_are_looked_up() {
        let here = Some("cartopolis.org");
        assert_eq!(key_of("Alice", here).as_deref(), Some("alice"));
        assert_eq!(
            key_of("alice@Cartopolis.org", here).as_deref(),
            Some("alice")
        );
        assert_eq!(key_of("alice@mastodon.social", here), None);
        assert_eq!(key_of("alice@cartopolis.org", None), None);
        assert_eq!(key_of("Friday walk", here), None);
        assert_eq!(key_of("", here), None);
    }

    /// The crop is the square the viewfinder drew: centred, the same share of
    /// the shorter side.
    #[test]
    fn the_polaroid_is_the_square_in_the_viewfinder() {
        let (w, h) = (100usize, 50usize);
        let rgba: Vec<u8> = (0..w * h)
            .flat_map(|i| [(i % w) as u8, (i / w) as u8, 0, 255])
            .collect();
        let (out, size) = crop_finder(&rgba, [w, h]);
        assert_eq!(size, [40, 40]);
        assert_eq!(out.len(), 40 * 40 * 4);
        // Top-left of the crop is (30, 5) in the window.
        assert_eq!(&out[..2], &[30, 5]);
        let last = (40 * 40 - 1) * 4;
        assert_eq!(&out[last..last + 2], &[69, 44]);
    }
}
