//! People's pictures: the face a person's disc shows instead of their initial,
//! in every list, call and map marker (`docs/notes/profile-pictures.md`).
//!
//! **Every disc asks one function by name**, [`face_url`] — no `World`, no
//! resource: the component library's discs (`lib_chats::disc`) reach the
//! pictures through a small shared table this module rewrites every frame,
//! and a disc asking for a name it has no picture for is how this module
//! learns whom to look up. The answer is a `data:` thumbnail scaled on a
//! worker to the disc's size.
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
use std::sync::{Arc, LazyLock};

use bevy::prelude::*;
use bevy::tasks::{IoTaskPool, Task};

use crate::components::{Avatar, FreeCamera};
use crate::platform::photo::{Capture, Outcome, Source};
use crate::platform::sync::Mutex;
use crate::resources::{CameraMode, CameraState, OnlineSession, RightPanel};
use crate::systems::icons;

/// How long a name's answer is believed before it is asked again.
const REFRESH_S: f64 = 300.0;
/// How long after a failed lookup a name is asked again.
const RETRY_S: f64 = 30.0;
/// At most one lookup this often, whatever is waiting.
const LOOKUP_EVERY_S: f64 = 0.5;
/// Pictures kept before the ones nobody has drawn lately go. They come back
/// from the disk cache when somebody is.
const MAX_PICTURES: usize = 64;
/// A picture not drawn for this long may be let go once over [`MAX_PICTURES`].
const UNSEEN_S: f64 = 30.0;
/// The viewfinder's square, as a share of the map's shorter side. The capture
/// is cropped to the same share of the window's, so what is in the frame is
/// what is in the picture.
const FINDER_SHARE: f32 = 0.8;
/// The viewfinder's buttons stand this far above the window's bottom edge.
#[cfg_attr(not(feature = "ui-lib"), allow(dead_code))]
pub(crate) const FINDER_CONTROLS_BOTTOM: f32 = 28.0;
/// How far in front of your avatar Face me puts the camera, and how high.
const FACE_ME_DISTANCE_M: f32 = 2.4;
const FACE_ME_PITCH: f32 = 0.12;

/// What [`face_url`] reads and writes, without a `World` to reach.
#[derive(Default)]
struct Bridge {
    /// Names drawn since the last frame, by key.
    seen: HashSet<String>,
    /// This server's name, so `name@here` reads as `name` and anybody else's
    /// server is left alone: their names mean nothing to this one's accounts.
    server: Option<String>,
    /// Off while signed out: there is nobody to ask.
    on: bool,
    /// The component library's discs ([`face_url`]): each name's picture's
    /// hash, as far as it is known.
    hashes: HashMap<String, String>,
    /// Thumbnails made, by (hash, edge px).
    thumbs: HashMap<(String, u32), Arc<str>>,
    /// Thumbnails a disc asked for since the last frame.
    want: HashSet<(String, u32)>,
    /// Moves whenever `hashes` or `thumbs` does: the library's discs redraw
    /// on it (`lib_chats::follow_faces`).
    revision: u64,
}

static BRIDGE: LazyLock<Mutex<Bridge>> = LazyLock::new(Default::default);

/// A person's picture for the component library's discs: a `data:image/jpeg`
/// URL `edge` px square ([`thumb_edge`]), if they have one and it is made.
/// Asking is also what gets it looked up, fetched and scaled — on a worker,
/// once per picture and size; a disc asks again when [`face_revision`] moves.
#[must_use]
#[cfg_attr(not(all(feature = "ui-lib", feature = "comms")), allow(dead_code))]
pub fn face_url(name: &str, edge: u32) -> Option<Arc<str>> {
    let mut bridge = BRIDGE.lock().ok()?;
    if !bridge.on {
        return None;
    }
    let key = key_of(name, bridge.server.as_deref())?;
    let hash = bridge.hashes.get(&key).cloned();
    bridge.seen.insert(key);
    let wanted = (hash?, edge);
    if let Some(url) = bridge.thumbs.get(&wanted) {
        return Some(url.clone());
    }
    bridge.want.insert(wanted);
    None
}

/// How many times what [`face_url`] answers has changed.
#[must_use]
#[cfg_attr(not(all(feature = "ui-lib", feature = "comms")), allow(dead_code))]
pub fn face_revision() -> u64 {
    BRIDGE.lock().map_or(0, |b| b.revision)
}

/// The edge, px, of a thumbnail drawn `points` long on its longer side: the
/// next power of two at or above its physical size — so never more than
/// twice it, and few sizes for one picture — and never past `cap`, the
/// picture's own.
#[must_use]
#[cfg_attr(not(all(feature = "ui-lib", feature = "comms")), allow(dead_code))]
pub fn thumb_edge(points: f32, px_per_pt: f32, cap: u32) -> u32 {
    let need = (points * px_per_pt).ceil().clamp(16.0, 4096.0) as u32;
    need.next_power_of_two().min(cap.max(1))
}

/// `bytes` (any format `image` reads) as a `data:image/jpeg` URL no longer
/// than `edge` px on its longer side, never enlarged: what an `<img>` shows
/// in the app (`mithril_bevy::images`) and in the browser alike. Decoding
/// and encoding take milliseconds, so this runs on a worker
/// (`platform::workers`), never on the frame.
pub(crate) fn thumbnail_url(bytes: &[u8], edge: u32) -> Result<String, String> {
    let (rgba, [w, h]) = crate::utils::photo::decode_rgba(bytes)?;
    let image = image::RgbaImage::from_raw(w as u32, h as u32, rgba)
        .ok_or("the picture was the wrong size")?;
    let (w, h) = image.dimensions();
    let edge = edge.max(1);
    let image = if w.max(h) > edge {
        let s = edge as f32 / w.max(h) as f32;
        let tw = ((w as f32 * s).round() as u32).max(1);
        let th = ((h as f32 * s).round() as u32).max(1);
        image::imageops::resize(&image, tw, th, image::imageops::FilterType::Triangle)
    } else {
        image
    };
    let rgb = image::DynamicImage::ImageRgba8(image).to_rgb8();
    let mut out = Vec::new();
    image::codecs::jpeg::JpegEncoder::new_with_quality(&mut out, 82)
        .encode_image(&rgb)
        .map_err(|e| e.to_string())?;
    Ok(jpeg_data_url(&out))
}

/// A JPEG as a `data:` URL. Base64 by hand: the `base64` crate is not in
/// the web build.
#[must_use]
pub(crate) fn jpeg_data_url(jpeg: &[u8]) -> String {
    const ABC: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::with_capacity(23 + jpeg.len().div_ceil(3) * 4);
    out.push_str("data:image/jpeg;base64,");
    for chunk in jpeg.chunks(3) {
        let b = [
            chunk[0],
            chunk.get(1).copied().unwrap_or(0),
            chunk.get(2).copied().unwrap_or(0),
        ];
        let n = (u32::from(b[0]) << 16) | (u32::from(b[1]) << 8) | u32::from(b[2]);
        out.push(ABC[(n >> 18) as usize & 63] as char);
        out.push(ABC[(n >> 12) as usize & 63] as char);
        out.push(if chunk.len() > 1 {
            ABC[(n >> 6) as usize & 63] as char
        } else {
            '='
        });
        out.push(if chunk.len() > 2 {
            ABC[n as usize & 63] as char
        } else {
            '='
        });
    }
    out
}

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

/// One person's answer: their picture's hash, or none, and when it was asked.
struct Known {
    hash: Option<String>,
    asked: f64,
    seen: f64,
}

/// A picture by hash: on its way, here (the JPEG, which the library's
/// thumbnails are scaled from), or not to be had.
enum Picture {
    Loading(Task<Result<Vec<u8>, String>>),
    Ready { jpeg: Vec<u8> },
    Failed,
}

/// A thumbnail for the library's discs, by (hash, edge px).
enum Thumb {
    Making(Task<Option<Result<String, String>>>),
    Made,
    Failed,
}

/// A picture made here and not yet chosen.
pub struct Preview {
    jpeg: Vec<u8>,
    /// The JPEG as a `data:` URL, made with it on the worker: the library's
    /// polaroid. Already 256 px (`utils::photo::FACE_EDGE`), under twice the
    /// 168 pt it is drawn at, so it is not scaled again.
    src: Arc<str>,
}

impl Preview {
    /// The preview for an `<img>` (`lib_chats`' polaroid).
    #[must_use]
    #[cfg_attr(not(all(feature = "ui-lib", feature = "comms")), allow(dead_code))]
    pub fn src(&self) -> Arc<str> {
        self.src.clone()
    }
}

type Prepared = Result<(Vec<u8>, (Vec<u8>, [usize; 2])), String>;
/// [`Prepared`]'s JPEG with its `data:` URL.
type Made = Result<(Vec<u8>, String), String>;
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
    prepare: Option<Task<Made>>,
    /// The library's thumbnails being made or made (the URLs are in the
    /// bridge), and the ones asked for whose picture has not arrived yet.
    thumbs: HashMap<(String, u32), Thumb>,
    thumbs_waiting: HashSet<(String, u32)>,
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
    /// What the library's viewfinder (`lib_face_viewfinder`) shows, while
    /// the viewfinder is up ([`drive_face_viewfinder`] publishes it).
    pub lib_view: Option<FinderView>,
    /// The library's taps, done next frame by [`Faces::finder_act`].
    pub lib_acts: Vec<FinderAct>,
}

/// What the library's viewfinder shows (`lib_face_viewfinder`); where the
/// square stands follows from the window ([`finder_rect`]).
#[derive(Clone, PartialEq, Debug)]
#[cfg_attr(not(feature = "ui-lib"), allow(dead_code))]
pub struct FinderView {
    /// The shutter was pressed and the picture is on its way: the shutter
    /// waits.
    pub shooting: bool,
}

/// A press on the viewfinder's controls.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum FinderAct {
    Cancel,
    /// Swing the follow camera round in front of your avatar.
    FaceMe,
    /// Take it: the chrome-free capture, cropped to the square.
    Shoot,
}

/// The viewfinder's square on a `screen`: [`FINDER_SHARE`] of its shorter
/// side, centred — the crop `crop_finder` takes from the capture.
#[must_use]
#[cfg_attr(not(feature = "ui-lib"), allow(dead_code))]
pub fn finder_rect(screen: Rect) -> Rect {
    // Not `Rect::from_center_size`, which panics on a negative or NaN size.
    let half = Vec2::splat(screen.width().min(screen.height()) * FINDER_SHARE * 0.5);
    Rect {
        min: screen.center() - half,
        max: screen.center() + half,
    }
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
        self.prepare = Some(IoTaskPool::get().spawn(async move {
            work().map(|(jpeg, _pixels)| {
                let src = jpeg_data_url(&jpeg);
                (jpeg, src)
            })
        }));
    }

    /// Do a press on the viewfinder, for both editions; `true` when it is
    /// done with (Cancel, or the picture taken).
    pub fn finder_act(
        &mut self,
        act: FinderAct,
        avatar: Option<&Transform>,
        camera_state: &mut CameraState,
        screenshot: &mut crate::resources::ScreenshotState,
    ) -> bool {
        match act {
            FinderAct::Cancel => true,
            FinderAct::FaceMe => {
                if let Some(body) = avatar {
                    if camera_state.mode != CameraMode::Avatar {
                        crate::systems::free_camera::enter_mode(camera_state, CameraMode::Avatar);
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
                    self.note = Some("Sign in to stand somewhere first.".into());
                }
                false
            }
            FinderAct::Shoot => {
                if self.shooting {
                    return false;
                }
                // Taken next frame by `ui::drive_screenshots` with the UI
                // hidden, so neither this frame nor the shade is in it.
                screenshot.face_snapshot = true;
                screenshot.hide_ui = true;
                self.shooting = true;
                true
            }
        }
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
/// scale the thumbnails, and drive the picker.
pub fn drive_faces(
    time: Res<Time>,
    online: Option<Res<OnlineSession>>,
    identity: Res<crate::resources::AuthIdentity>,
    mut faces: ResMut<Faces>,
) {
    let now = time.elapsed_secs_f64();
    let signed_in = online.is_some() && crate::systems::model_assets::api_base().is_some();
    let account = identity
        .username
        .clone()
        .filter(|_| signed_in)
        .map(|u| u.to_lowercase());
    if faces.account != account {
        // Somebody else, or nobody: nothing known so far is theirs to see.
        if let Ok(mut b) = BRIDGE.lock() {
            b.seen.clear();
            b.hashes.clear();
            b.thumbs.clear();
            b.want.clear();
            b.revision += 1;
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
            Picture::Loading(
                IoTaskPool::get()
                    .spawn(async move { crate::systems::model_assets::fetch_picture(&hash).await }),
            )
        });
    }
    for picture in faces.pictures.values_mut() {
        if let Picture::Loading(task) = picture {
            if let Some(result) = crate::platform::http::poll_task(task) {
                *picture = match result {
                    Ok(jpeg) => Picture::Ready { jpeg },
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
        .filter(|p| matches!(p, Picture::Ready { .. }))
        .count();
    if ready > MAX_PICTURES {
        let recent: HashSet<String> = faces
            .known
            .values()
            .filter(|k| now - k.seen < UNSEEN_S)
            .filter_map(|k| k.hash.clone())
            .collect();
        faces
            .pictures
            .retain(|hash, p| !matches!(p, Picture::Ready { .. }) || recent.contains(hash));
    }
    drive_thumbs(&mut faces);

    drive_picker(&mut faces, account.as_deref().unwrap_or_default(), now);
}

/// The library's discs (`face_url`): publish each name's hash, make the
/// thumbnails they asked for on a worker once the picture is here, and hand
/// the finished ones over.
fn drive_thumbs(faces: &mut Faces) {
    let hashes: HashMap<String, String> = faces
        .known
        .iter()
        .filter_map(|(name, k)| Some((name.clone(), k.hash.clone()?)))
        .collect();
    let asked = match BRIDGE.lock() {
        Ok(mut b) => {
            if b.hashes != hashes {
                b.hashes = hashes.clone();
                b.revision += 1;
            }
            std::mem::take(&mut b.want)
        }
        Err(_) => return,
    };
    faces.thumbs_waiting.extend(asked);
    // Start the ones whose picture is here; drop the ones it never will be.
    let waiting: Vec<(String, u32)> = faces.thumbs_waiting.iter().cloned().collect();
    for key in waiting {
        if faces.thumbs.contains_key(&key) {
            faces.thumbs_waiting.remove(&key);
            continue;
        }
        match faces.pictures.get(&key.0) {
            Some(Picture::Ready { jpeg, .. }) => {
                let (jpeg, edge) = (jpeg.clone(), key.1);
                let task = IoTaskPool::get().spawn(async move {
                    crate::platform::workers::compute("face thumbnail", move || {
                        thumbnail_url(&jpeg, edge)
                    })
                    .await
                });
                faces.thumbs_waiting.remove(&key);
                faces.thumbs.insert(key, Thumb::Making(task));
            }
            Some(Picture::Failed) => {
                faces.thumbs_waiting.remove(&key);
            }
            // On its way, or let go and asked for again next frame.
            _ => {}
        }
    }
    let mut made: Vec<((String, u32), Arc<str>)> = Vec::new();
    for (key, thumb) in &mut faces.thumbs {
        if let Thumb::Making(task) = thumb {
            if let Some(result) = crate::platform::http::poll_task(task) {
                *thumb = match result {
                    Some(Ok(url)) => {
                        made.push((key.clone(), Arc::from(url)));
                        Thumb::Made
                    }
                    Some(Err(why)) => {
                        tracing::warn!(error = %why, "a person's picture was not scaled");
                        Thumb::Failed
                    }
                    None => Thumb::Failed,
                };
            }
        }
    }
    // A thumbnail lives while somebody known has its picture: a picture
    // changed or forgotten takes its thumbnails with it.
    let live: HashSet<&String> = hashes.values().collect();
    faces.thumbs.retain(|(hash, _), _| live.contains(hash));
    faces.thumbs_waiting.retain(|(hash, _)| live.contains(hash));
    if let Ok(mut b) = BRIDGE.lock() {
        let before = b.thumbs.len();
        b.thumbs.retain(|(hash, _), _| live.contains(hash));
        let dropped = b.thumbs.len() != before;
        made.retain(|((hash, _), _)| live.contains(hash));
        if dropped || !made.is_empty() {
            b.thumbs.extend(made);
            b.revision += 1;
        }
    }
}

/// The most names one lookup asks about; the server answers up to 100.
pub const MAX_PER_LOOKUP: usize = 100;

fn drive_picker(faces: &mut Faces, me: &str, now: f64) {
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
                Ok((jpeg, src)) => {
                    faces.preview = Some(Preview {
                        jpeg,
                        src: Arc::from(src),
                    });
                }
                Err(why) => faces.note = Some(why),
            }
        }
    }
    if let Some(task) = faces.saving.as_mut() {
        if let Some(result) = crate::platform::http::poll_task(task) {
            faces.saving = None;
            match result {
                Ok(hash) => {
                    faces.note = None;
                    // Shown at once from the preview's own JPEG, rather than
                    // fetched back from where it was just sent.
                    if let (Some(h), Some(preview)) = (hash.as_ref(), faces.preview.take()) {
                        faces
                            .pictures
                            .insert(h.clone(), Picture::Ready { jpeg: preview.jpeg });
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

/// A way to change your picture on the You page, for both editions
/// (the library's `lib_chats`). The You page is the
/// messenger's, so all of this is unused in a build without `comms`.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
#[cfg_attr(not(feature = "comms"), allow(dead_code))]
pub enum FaceChoice {
    /// A picture from the device.
    Photo,
    /// A selfie.
    Camera,
    /// The polaroid's viewfinder over the world.
    FromTheWorld,
    /// Back to the initial.
    Remove,
    /// The preview becomes your picture.
    UsePreview,
    /// Throw the preview away.
    Again,
}

#[cfg_attr(not(feature = "comms"), allow(dead_code))]
impl FaceChoice {
    #[must_use]
    pub fn icon(self) -> &'static str {
        match self {
            FaceChoice::Photo => icons::ADD,
            FaceChoice::Camera => icons::CAMERA,
            FaceChoice::FromTheWorld => icons::CAM_FOLLOW,
            FaceChoice::Remove => icons::DELETE,
            FaceChoice::UsePreview => icons::CHECK,
            FaceChoice::Again => icons::CLEAR,
        }
    }

    #[must_use]
    pub fn label(self) -> &'static str {
        match self {
            FaceChoice::Photo => "Photo",
            FaceChoice::Camera => "Camera",
            FaceChoice::FromTheWorld => "From the world",
            FaceChoice::Remove => "Remove",
            FaceChoice::UsePreview => "Use this",
            FaceChoice::Again => "Again",
        }
    }
}

#[cfg_attr(not(feature = "comms"), allow(dead_code))]
impl Faces {
    /// What the You page offers: the preview's two answers while one waits,
    /// else the ways this platform can make a picture, and Remove when
    /// there is one.
    #[must_use]
    pub fn choices(&self, me: &str) -> Vec<FaceChoice> {
        if self.preview.is_some() {
            return vec![FaceChoice::UsePreview, FaceChoice::Again];
        }
        let mut out = Vec::new();
        if crate::platform::photo::offers(Source::File) {
            out.push(FaceChoice::Photo);
        }
        if crate::platform::photo::offers(Source::Selfie) {
            out.push(FaceChoice::Camera);
        }
        out.push(FaceChoice::FromTheWorld);
        if self.mine(me).is_some() {
            out.push(FaceChoice::Remove);
        }
        out
    }

    /// The choices can be pressed: nothing is being saved, taken or made.
    #[must_use]
    pub fn choices_enabled(&self) -> bool {
        if self.preview.is_some() {
            self.saving.is_none()
        } else {
            !self.busy()
        }
    }

    /// Do a choice. "From the world" closes the panel for the viewfinder.
    pub fn choose(&mut self, choice: FaceChoice, panel: &mut Option<RightPanel>) {
        match choice {
            FaceChoice::Photo => self.pick = Some(Source::File),
            FaceChoice::Camera => self.pick = Some(Source::Selfie),
            FaceChoice::FromTheWorld => {
                self.viewfinder = true;
                *panel = Some(RightPanel::None);
            }
            FaceChoice::Remove => self.remove(),
            FaceChoice::UsePreview => self.use_preview(),
            FaceChoice::Again => self.preview = None,
        }
    }
}

/// The polaroid's viewfinder: a square over the map, the shutter, Face me and
/// Cancel, with the panels shut so the map is the screen. Publishes what the
/// library's viewfinder (`lib_face_viewfinder`) shows and does the presses
/// it handed back.
pub fn drive_face_viewfinder(
    mut faces: ResMut<Faces>,
    mut camera_state: ResMut<CameraState>,
    avatar: Query<&Transform, (With<Avatar>, Without<FreeCamera>)>,
    mut screenshot: ResMut<crate::resources::ScreenshotState>,
    mut right_panel: ResMut<RightPanel>,
    #[cfg(feature = "comms")] mut chats_view: ResMut<crate::systems::chats::ChatsView>,
) {
    if !faces.viewfinder {
        if faces.lib_view.is_some() {
            faces.lib_view = None;
        }
        if !faces.lib_acts.is_empty() {
            faces.lib_acts.clear();
        }
        return;
    }
    // The presses the library handed back since the last frame.
    let acts = std::mem::take(&mut faces.lib_acts);
    let view = FinderView {
        shooting: faces.shooting,
    };
    if faces.lib_view.as_ref() != Some(&view) {
        faces.lib_view = Some(view);
    }
    let body = avatar.single().ok();
    let mut done = false;
    for act in acts {
        done |= faces.finder_act(act, body, &mut camera_state, &mut screenshot);
    }
    if done {
        faces.viewfinder = false;
        faces.lib_view = None;
        *right_panel = RightPanel::Chat;
        #[cfg(feature = "comms")]
        {
            chats_view.you = true;
        }
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
