//! **The camera** (`docs/mockups/social-camera/`): the orange disc in the
//! middle of the dock, Camera in a chat's + menu and in the post composer.
//! Three modes, each with a selfie flip and an upload button beside the
//! shutter:
//!
//! - **In world**: a polaroid of the view as it is (Follow, Fly or Earth), the
//!   frame drawn over the map and the picture cropped to it
//!   (`utils::world_polaroid`). Selfie turns the Follow camera round to face
//!   your avatar, as the profile picture's Face me does.
//! - **Mixed**: the phone's camera and Cartopolis drawn from the phone's own
//!   pose, side by side when held sideways and one above the other when held
//!   upright. While it is up the world camera *is* the phone: on the position
//!   fix at eye height above the measured ground, turned by the compass and
//!   the gravity sensor, given the photograph's lens.
//! - **Real**: the phone's camera alone.
//!
//! After the shutter (or an upload) the picture is shown with **Post**
//! (to the community on the map, else a picker), **Send** (to the chat the
//! camera was opened from, else a list of chats), **To phone** (the gallery)
//! and **Retake**. Every picture the camera makes carries its whole pose in
//! its EXIF and XMP (`cartopolis_core::photo_pose`), so the file alone is
//! enough to put a camera back on the same view; an upload keeps the EXIF it
//! came with. This replaced the field check (2026-10-18), a developer tool that
//! did the Mixed picture and nothing else.
//!
//! **Only one camera can be open**: the high-precision mode holds the back
//! camera, and Mixed or Real while it is on reports the camera as taken.
//!
//! On the web there is no camera (`platform::camera` is not compiled there),
//! so the dock offers none and this draws nothing.
#![cfg_attr(target_arch = "wasm32", allow(dead_code))]

use bevy::prelude::*;
use cartopolis_core::photo_pose::{self as pp, PhotoPose};

#[cfg(not(target_arch = "wasm32"))]
use native::Session;

/// The three ways the camera takes a picture.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Mode {
    /// A polaroid of the view.
    #[default]
    World,
    /// The photograph beside the view from the same pose.
    Mixed,
    /// The photograph alone.
    Real,
}

impl Mode {
    pub const ALL: [Mode; 3] = [Mode::World, Mode::Mixed, Mode::Real];

    #[must_use]
    pub fn label(self) -> &'static str {
        match self {
            Mode::World => "In world",
            Mode::Mixed => "Mixed",
            Mode::Real => "Real",
        }
    }

    /// The word a picture's pose records it under.
    #[must_use]
    pub fn key(self) -> &'static str {
        match self {
            Mode::World => "world",
            Mode::Mixed => "mixed",
            Mode::Real => "real",
        }
    }

    /// Whether this mode opens the phone's camera.
    #[must_use]
    pub fn uses_camera(self) -> bool {
        self != Mode::World
    }
}

/// Where the picture goes, when the camera was opened for something.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub enum Target {
    /// From the dock: the result screen offers everything.
    #[default]
    Anywhere,
    /// From a chat's + menu: Send goes straight to this chat.
    Chat(String),
    /// From the post composer: the picture goes back into the post.
    Post,
}

/// A finished picture on its way into a post (`hand_picture_to_post`).
pub struct ForPost {
    pub jpeg: Vec<u8>,
    pub place: Option<cartopolis_core::posts::PostPlace>,
}

/// The camera: a request to open it, the mode it was last in, and the screen
/// while it is up.
#[derive(Resource, Default)]
pub struct CameraScreen {
    /// Open the camera, for this.
    pub request: Option<Target>,
    /// The mode it opens in: the one used last.
    pub mode: Mode,
    /// The world, captured for the shutter by `ui::drive_screenshots`: RGBA
    /// pixels and their size.
    pub world_in: Option<(Vec<u8>, [u32; 2])>,
    /// A picture for a post, for `hand_picture_to_post`.
    pub for_post: Option<ForPost>,
    /// A camera opened from a chat or the composer closed: open the panel it
    /// came from again.
    reopen_chat: bool,
    #[cfg(not(target_arch = "wasm32"))]
    session: Option<Session>,
}

impl CameraScreen {
    /// Whether the camera's screen is up.
    #[must_use]
    pub fn is_open(&self) -> bool {
        #[cfg(not(target_arch = "wasm32"))]
        {
            self.session.is_some()
        }
        #[cfg(target_arch = "wasm32")]
        {
            false
        }
    }
}

/// Whether this build has a camera to offer.
#[must_use]
pub fn available() -> bool {
    cfg!(not(target_arch = "wasm32"))
}

/// The polaroid's photo, width over height: what `world_polaroid` frames.
const POLAROID_ASPECT: f32 = 960.0 / 900.0;

/// The longest side of each half of a Mixed picture: two make 1600, the size
/// a chat picture is re-encoded to, so nothing is scaled twice.
const HALF_LONG: u32 = 800;

/// Frames to hold the world camera on the shutter's pose before capturing it,
/// so the capture is of that pose and not of the frame before it.
const HOLD_FRAMES: u8 = 3;

/// The long side of a phone camera's view when it publishes no lens data, in
/// degrees: a main camera is about 26 mm equivalent, 69–72° across the long
/// side (the high-precision tracker assumes the same, `precise_tracker`).
const FALLBACK_LONG_FOV_DEG: f32 = 72.0;

/// JPEG quality of a picture the camera makes.
const QUALITY: u8 = 90;

/// The two halves of a Mixed picture, `(rgba, [w, h])`: the photograph as it
/// came (its shape is the picture's) and the world cropped to that shape,
/// side by side when the photograph is wider than tall, else one above the
/// other, each at most [`HALF_LONG`] on its long side. Pure, so it runs on a
/// thread and is tested off any GPU.
#[must_use]
pub fn compose_mixed(photo: (&[u8], u32, u32), world: (&[u8], u32, u32)) -> (Vec<u8>, [u32; 2]) {
    use image::{imageops, Rgba, RgbaImage};
    let (pw, ph) = (photo.1.max(1), photo.2.max(1));
    let aspect = pw as f32 / ph as f32;
    let (hw, hh) = if pw >= ph {
        (
            HALF_LONG,
            ((HALF_LONG as f32 / aspect).round() as u32).max(1),
        )
    } else {
        (
            ((HALF_LONG as f32 * aspect).round() as u32).max(1),
            HALF_LONG,
        )
    };
    let sideways = pw >= ph;
    let (w, h) = if sideways { (hw * 2, hh) } else { (hw, hh * 2) };
    let mut canvas = RgbaImage::from_pixel(w, h, Rgba([0x0A, 0x0E, 0x10, 0xFF]));
    let halves = [
        RgbaImage::from_raw(photo.1, photo.2, photo.0.to_vec()),
        RgbaImage::from_raw(world.1, world.2, world.0.to_vec()),
    ];
    for (i, img) in halves.iter().enumerate() {
        let Some(img) = img else {
            continue;
        };
        let (x, y, cw, ch) = pp::crop_to(img.width(), img.height(), aspect);
        let cropped = imageops::crop_imm(img, x, y, cw, ch).to_image();
        let half = imageops::resize(&cropped, hw, hh, imageops::FilterType::Triangle);
        let (ox, oy) = if sideways {
            (i64::from(hw) * i as i64, 0)
        } else {
            (0, i64::from(hh) * i as i64)
        };
        imageops::overlay(&mut canvas, &half, ox, oy);
    }
    (canvas.into_raw(), [w, h])
}

/// The part of a `[w, h]` RGBA capture inside `frame` (fractions of the
/// window, `[x0, y0, x1, y1]`).
#[must_use]
pub fn crop_fraction(rgba: &[u8], [w, h]: [u32; 2], frame: [f32; 4]) -> (Vec<u8>, [u32; 2]) {
    let x0 = ((frame[0].clamp(0.0, 1.0) * w as f32) as u32).min(w.saturating_sub(1));
    let y0 = ((frame[1].clamp(0.0, 1.0) * h as f32) as u32).min(h.saturating_sub(1));
    let x1 = ((frame[2].clamp(0.0, 1.0) * w as f32) as u32).clamp(x0 + 1, w);
    let y1 = ((frame[3].clamp(0.0, 1.0) * h as f32) as u32).clamp(y0 + 1, h);
    let (cw, ch) = (x1 - x0, y1 - y0);
    let mut out = Vec::with_capacity((cw * ch * 4) as usize);
    for y in y0..y1 {
        let row = ((y * w + x0) * 4) as usize;
        out.extend_from_slice(&rgba[row..row + (cw * 4) as usize]);
    }
    (out, [cw, ch])
}

/// The vertical field of view of a photograph `width × height` (upright, as
/// delivered), from what its camera says about its lens; without lens data, a
/// main camera's usual angle along the long side. A phone's sensor is wider
/// than tall, so a picture taller than wide was turned from it.
#[must_use]
pub fn photo_fov_y_deg(lens: Option<&cartopolis_core::lens::Lens>, width: u32, height: u32) -> f32 {
    let turned = height > width;
    let (sw, sh) = if turned {
        (height, width)
    } else {
        (width, height)
    };
    lens.and_then(|l| l.field_of_view_deg(sw, sh))
        .map(|(h, v)| if turned { h } else { v })
        .unwrap_or_else(|| {
            let long = width.max(height) as f32;
            let focal = long / 2.0 / (FALLBACK_LONG_FOV_DEG.to_radians() / 2.0).tan();
            2.0 * (height as f32 / 2.0 / focal).atan().to_degrees()
        })
}

/// A picture's file: JPEG at [`QUALITY`] with the pose in its EXIF and XMP.
pub fn jpeg_with_pose(
    rgba: &[u8],
    size: [u32; 2],
    pose: Option<&PhotoPose>,
) -> Result<Vec<u8>, String> {
    let meta = pose.map_or_else(Default::default, |p| crate::utils::jpeg_meta::Meta {
        exif: Some(p.exif()),
        xmp: Some(p.xmp().into_bytes()),
    });
    crate::utils::jpeg_meta::encode_rgba(rgba, size, QUALITY, &meta)
}

/// Hand a finished picture to the post it is for: into the draft being
/// written, or a new one in the community on the map, or the community
/// picker. Then upload it, once a draft is there to hold it.
///
/// It also opens the camera for the panels that ask for it (a chat's "+",
/// the post composer), shutting the panel so the map is the screen, and opens
/// the chat again when a camera opened from one closes.
pub fn hand_picture_to_post(
    mut camera: ResMut<CameraScreen>,
    mut communities: ResMut<crate::systems::communities::Communities>,
    mut right_panel: ResMut<crate::resources::RightPanel>,
    mut chats: ResMut<crate::systems::chats::ChatsView>,
) {
    if !available() {
        chats.camera_for = None;
        communities.posts.want_camera = false;
    }
    if let Some(key) = chats.camera_for.take() {
        camera.request = Some(Target::Chat(key));
        *right_panel = crate::resources::RightPanel::None;
    }
    if std::mem::take(&mut communities.posts.want_camera) {
        camera.request = Some(Target::Post);
        *right_panel = crate::resources::RightPanel::None;
    }
    if std::mem::take(&mut camera.reopen_chat) {
        *right_panel = crate::resources::RightPanel::Chat;
    }
    let cs = &mut *communities;
    if let Some(handed) = camera.for_post.take() {
        cs.posts.pending_picture = Some(handed.jpeg);
        // Into the draft it was taken for, where that is; a new one over the
        // map in the community on the map; else the picker, in the panel.
        let panel = if cs.posts.draft.is_some() {
            !cs.posts.over_map
        } else {
            cs.posts.from_view = handed.place;
            match cs.posts.community_to_post_in(&cs.list) {
                Some(id) => {
                    cs.posts.compose(id);
                    cs.posts.over_map = true;
                    false
                }
                None => {
                    cs.screen = Some(crate::systems::communities::Screen::PostTo);
                    true
                }
            }
        };
        *right_panel = if panel {
            crate::resources::RightPanel::Chat
        } else {
            crate::resources::RightPanel::None
        };
    }
    if cs.posts.draft.is_some() && cs.picture_for_post_idle() {
        if let Some(jpeg) = cs.posts.pending_picture.take() {
            cs.picture_for_post = true;
            cs.upload_picture(jpeg);
        }
    }
}

/// The address at `lat`, `lng` from the geocoder, shortened as a saved place's
/// name is ("Grote Markt 1, Groningen"). Blocks on the request: called on the
/// thread that puts the picture together. `None` when it cannot say.
#[cfg(not(target_arch = "wasm32"))]
fn place_at(lat: f64, lng: f64) -> Option<String> {
    let url = format!(
        "{}/reverse?format=jsonv2&zoom=18&lat={lat}&lon={lng}",
        crate::systems::geo_nav::nominatim_base_url()
    );
    let body = futures_lite::future::block_on(crate::platform::http::fetch_string(
        crate::platform::http::Request::get(url),
    ))
    .map_err(|e| warn!("camera: no place name: {e}"))
    .ok()?;
    let json: serde_json::Value = serde_json::from_str(&body).ok()?;
    json.get("display_name")
        .and_then(|v| v.as_str())
        .map(crate::systems::bookmarks::short_address)
}

#[cfg(not(target_arch = "wasm32"))]
pub use native::{drive_camera, render_camera};

#[cfg(not(target_arch = "wasm32"))]
mod native {
    use super::{
        compose_mixed, crop_fraction, jpeg_with_pose, photo_fov_y_deg, pp, CameraScreen, ForPost,
        Mode, PhotoPose, Target, HOLD_FRAMES, POLAROID_ASPECT,
    };
    use crate::systems::egui_manager::EguiMut;
    use bevy::prelude::*;
    use big_space::prelude::GridCell;
    use cartopolis_calls::video::I420;
    use std::sync::mpsc::{Receiver, TryRecvError};
    use std::sync::Mutex;

    use crate::components::{Avatar, FreeCamera};
    use crate::platform::camera::Camera;
    use crate::platform::location::LocationWatch;
    use crate::platform::orientation::OrientationWatch;
    use crate::resources::{CameraMode, CameraState, MapAnchor, ScreenshotState};
    use crate::systems::geo_nav::{CameraPose, GeoNavState, Teleport};
    use crate::systems::icons;
    use crate::systems::theme;

    /// Metres of walking after which the world camera is teleported rather than
    /// moved: the map's streaming follows a teleport, not a nudge.
    const JUMP_M: f64 = 200.0;

    /// Frames to wait for the world capture before giving up on it.
    const CAPTURE_GIVE_UP_FRAMES: u16 = 300;

    /// How far in front of your avatar the In world selfie puts the camera,
    /// and how high: the profile picture's Face me.
    const SELFIE_DISTANCE_M: f32 = 2.4;
    const SELFIE_PITCH: f32 = 0.12;

    /// The camera's screen, while it is up.
    pub(super) struct Session {
        target: Target,
        mode: Mode,
        front: bool,
        /// The phone's camera, in Mixed and Real.
        camera: Option<Camera>,
        location: LocationWatch,
        compass: OrientationWatch,
        /// The newest frame, upright and full size: what the shutter takes.
        frame: Option<I420>,
        texture: Option<egui::TextureHandle>,
        /// In Mixed the world camera follows the sensors (a phone). False on a
        /// desktop, whose own view is the pose.
        moves_world: bool,
        /// The view to go back to when Mixed ends.
        restore: Option<Teleport>,
        /// Where the world camera was last put, to re-place it only on a move.
        placed_at: Option<(f64, f64)>,
        /// The pose the sensors (or the view) give now.
        live: Option<PhotoPose>,
        /// The polaroid's frame on screen, as fractions of the window.
        frame_rect: [f32; 4],
        shutter: Shutter,
        result: Option<Finished>,
        /// A picture being picked from the phone.
        upload: Option<crate::platform::photo::Capture>,
        /// Private chats to send to, `(key, name)`, when no chat was named.
        chats: Vec<(String, String)>,
        choosing_chat: bool,
        /// Flags from the screen for the drive system.
        press: bool,
        flip: bool,
        close: bool,
        switch: Option<Mode>,
        pick_upload: bool,
        post: bool,
        send_to: Option<String>,
        save: bool,
        retake: bool,
        error: Option<String>,
    }

    /// A finished picture: its file, its pixels for the screen, its pose.
    struct Finished {
        jpeg: Vec<u8>,
        rgba: Vec<u8>,
        size: [u32; 2],
        pose: Option<PhotoPose>,
        texture: Option<egui::TextureHandle>,
        /// A save in flight: where it went, or why not. Behind a mutex only
        /// because a resource must be `Sync` and a receiver is not.
        saving: Option<Mutex<Receiver<Result<String, String>>>>,
        saved: Option<Result<String, String>>,
    }

    type Composed = Result<(Vec<u8>, Vec<u8>, [u32; 2], Option<PhotoPose>), String>;

    enum Shutter {
        Idle,
        /// The world camera is held on `pose` for a few frames.
        Holding {
            photo: Option<I420>,
            pose: Option<PhotoPose>,
            frames: u8,
        },
        /// Waiting for the world capture, for `waited` frames so far.
        Capturing {
            photo: Option<I420>,
            pose: Option<PhotoPose>,
            waited: u16,
        },
        /// The picture is being put together on a thread.
        Composing {
            done: Mutex<Receiver<Composed>>,
        },
    }

    impl Session {
        fn busy(&self) -> bool {
            !matches!(self.shutter, Shutter::Idle) || self.upload.is_some()
        }
    }

    /// The window's size in physical pixels, or the headless surface's.
    fn window_px(
        windows: &Query<&Window>,
        headless: Option<&crate::resources::HeadlessSurface>,
    ) -> Option<(u32, u32)> {
        windows
            .iter()
            .next()
            .map(|w| {
                (
                    w.resolution.physical_width(),
                    w.resolution.physical_height(),
                )
            })
            .or_else(|| headless.map(|h| (h.size.x, h.size.y)))
            .filter(|(w, h)| *w > 0 && *h > 0)
    }

    fn client_version() -> String {
        option_env!("CARTO_GIT_SHA")
            .map(|s| s.chars().take(8).collect())
            .unwrap_or_else(|| env!("CARGO_PKG_VERSION").to_string())
    }

    fn now_ms() -> u64 {
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |d| d.as_millis() as u64)
    }

    /// What the external receiver says, as the pose's data.
    fn receiver() -> Option<pp::Receiver> {
        let status = crate::platform::gnss_receiver::status()?;
        let s = status.fresh_solution(std::time::Duration::from_secs(3))?;
        Some(pp::Receiver {
            altitude_m: s.altitude_m,
            geoid_separation_m: s.geoid_separation_m,
            satellites: s.satellites,
            hdop: s.hdop,
            correction_age_s: s.correction_age_s,
            station: status.corrections.mountpoint.clone(),
            station_km: status.corrections.distance_km,
            // GST is latitude, longitude, height: north, east, up.
            sigma_m: s.sigma_m.map(|(n, e, _)| [e, n]),
            sigma_up_m: s.sigma_m.and_then(|(_, _, u)| u),
        })
    }

    /// Run `job` on a thread of its own, its answer behind the receiver.
    fn spawn<T: Send + 'static>(
        name: &str,
        job: impl FnOnce() -> T + Send + 'static,
    ) -> Result<Mutex<Receiver<T>>, String> {
        let (tx, rx) = std::sync::mpsc::channel();
        std::thread::Builder::new()
            .name(name.into())
            .spawn(move || {
                let _ = tx.send(job());
            })
            .map_err(|e| e.to_string())?;
        Ok(Mutex::new(rx))
    }

    fn try_take<T>(rx: &Mutex<Receiver<T>>) -> Result<T, TryRecvError> {
        rx.lock()
            .map_err(|_| TryRecvError::Disconnected)
            .and_then(|rx| rx.try_recv())
    }

    /// The camera drives the world, the shutter takes the picture, and the
    /// finished picture goes where it is sent.
    #[allow(clippy::too_many_arguments)]
    pub fn drive_camera(
        mut screen: ResMut<CameraScreen>,
        mut nav: ResMut<GeoNavState>,
        mut cs: ResMut<CameraState>,
        anchor: Res<MapAnchor>,
        terrain: Res<crate::systems::terrain::TerrainField>,
        globe: Res<crate::systems::globe::GlobeState>,
        mut camera: Query<(&GridCell, &mut Transform, &Projection), With<FreeCamera>>,
        avatar: Query<&Transform, (With<Avatar>, Without<FreeCamera>)>,
        windows: Query<&Window>,
        headless: Option<Res<crate::resources::HeadlessSurface>>,
        mut shot: ResMut<ScreenshotState>,
        mut chat: ResMut<crate::systems::chat::ChatState>,
    ) {
        let screen = &mut *screen;
        if let Some(target) = screen.request.take() {
            if screen.session.is_none() {
                let session = Session {
                    target,
                    mode: Mode::World,
                    front: false,
                    camera: None,
                    location: LocationWatch::start(),
                    compass: OrientationWatch::start(),
                    frame: None,
                    texture: None,
                    moves_world: false,
                    restore: None,
                    placed_at: None,
                    live: None,
                    frame_rect: [0.0, 0.0, 1.0, 1.0],
                    shutter: Shutter::Idle,
                    result: None,
                    upload: None,
                    chats: Vec::new(),
                    choosing_chat: false,
                    press: false,
                    flip: false,
                    close: false,
                    switch: Some(screen.mode),
                    pick_upload: false,
                    post: false,
                    send_to: None,
                    save: false,
                    retake: false,
                    error: None,
                };
                screen.session = Some(session);
            }
        }
        let Some(session) = screen.session.as_mut() else {
            screen.world_in = None;
            return;
        };
        let Ok((cell, mut tf, projection)) = camera.single_mut() else {
            return;
        };
        let geo = anchor.geo;

        // ── Switching mode, flipping, closing ──
        let leave_mixed = |session: &mut Session, nav: &mut GeoNavState, cs: &mut CameraState| {
            if let Some(back) = session.restore.take() {
                nav.pending_teleport = Some(back);
            }
            session.placed_at = None;
            cs.free_camera_roll = 0.0;
            cs.free_fov = None;
        };
        if session.close && !session.busy() {
            leave_mixed(session, &mut nav, &mut cs);
            screen.mode = session.mode;
            // Back to the chat or the post it was opened from; a post being
            // handed a picture opens its panel itself.
            screen.reopen_chat = session.target != Target::Anywhere && screen.for_post.is_none();
            screen.session = None;
            screen.world_in = None;
            return;
        }
        if let Some(next) = session.switch.take().filter(|_| !session.busy()) {
            if session.mode == Mode::Mixed && next != Mode::Mixed {
                leave_mixed(session, &mut nav, &mut cs);
            }
            session.mode = next;
            screen.mode = next;
            session.frame = None;
            session.texture = None;
            session.error = None;
            session.camera = next
                .uses_camera()
                .then(|| Camera::open_still(session.front));
            session.moves_world = next == Mode::Mixed && LocationWatch::available();
            if session.moves_world && session.restore.is_none() {
                session.restore = geo.map(|geo| {
                    let (lat, lng) = crate::systems::geo_nav::camera_geo(geo, &globe, cell, &tf);
                    Teleport {
                        lat,
                        lng,
                        altitude: Some(globe.altitude_m(cell, &tf)),
                        pose: Some(CameraPose::capture(&cs)),
                        silent: true,
                    }
                });
            }
        }
        if std::mem::take(&mut session.flip) && !session.busy() {
            session.front = !session.front;
            if session.mode.uses_camera() {
                session.camera = Some(Camera::open_still(session.front));
                session.frame = None;
                session.texture = None;
            } else if let Ok(body) = avatar.single() {
                // In world: the Follow camera in front of your avatar, looking
                // back at it, or behind it again.
                if cs.mode != CameraMode::Avatar {
                    crate::systems::free_camera::enter_mode(&mut cs, CameraMode::Avatar);
                    cs.mode = CameraMode::Avatar;
                }
                let face = body.rotation.to_euler(EulerRot::YXZ).0;
                cs.follow_rotate = false;
                cs.first_person = false;
                if session.front {
                    cs.azimuth = face;
                    cs.pitch = SELFIE_PITCH;
                    cs.distance = SELFIE_DISTANCE_M;
                } else {
                    cs.azimuth = face + std::f32::consts::PI;
                }
            }
        }
        if let Some(cam) = session.camera.as_ref() {
            if let Some(e) = cam.error() {
                session.error = Some(e);
            }
            if let Some(frame) = cam.take_preview() {
                session.frame = Some(frame);
            }
        }

        // ── An upload from the phone ──
        if std::mem::take(&mut session.pick_upload) && session.upload.is_none() {
            match crate::platform::photo::start_from(crate::platform::photo::Source::File) {
                Ok(capture) => session.upload = Some(capture),
                Err(e) => session.error = Some(e),
            }
        }
        if let Some(capture) = session.upload.as_mut() {
            use crate::platform::photo::Outcome;
            match capture.poll() {
                Outcome::Pending => {}
                Outcome::Ready(bytes) => {
                    session.upload = None;
                    match crate::utils::photo::decode_rgba(&bytes) {
                        Ok((rgba, [w, h])) => {
                            session.result = Some(Finished {
                                jpeg: bytes,
                                rgba,
                                size: [w as u32, h as u32],
                                pose: None,
                                texture: None,
                                saving: None,
                                saved: None,
                            });
                        }
                        Err(e) => session.error = Some(e),
                    }
                }
                Outcome::Cancelled => session.upload = None,
                Outcome::Failed(why) => {
                    session.upload = None;
                    session.error = Some(why);
                }
            }
        }

        // ── The result: post it, send it, keep it ──
        session.chats = chat
            .channels
            .iter()
            .filter(|c| crate::systems::chat::ChatState::is_private(&c.key))
            .map(|c| {
                (
                    c.key.clone(),
                    crate::systems::chats::display_name(&c.key, &c.label),
                )
            })
            .collect();
        if std::mem::take(&mut session.retake) {
            session.result = None;
            session.error = None;
            session.choosing_chat = false;
        }
        if let Some(done) = session.result.as_mut() {
            if std::mem::take(&mut session.save) && done.saving.is_none() {
                let jpeg = done.jpeg.clone();
                let name = format!(
                    "{}.jpg",
                    done.pose.as_ref().map_or_else(
                        || format!("cartopolis-{}", now_ms() / 1000),
                        PhotoPose::name
                    )
                );
                match spawn("camera-save", move || {
                    crate::platform::gallery::save_jpeg(&name, &jpeg)
                }) {
                    Ok(rx) => {
                        done.saved = None;
                        done.saving = Some(rx);
                    }
                    Err(e) => done.saved = Some(Err(e)),
                }
            }
            let got = done.saving.as_ref().map(try_take);
            match got {
                Some(Ok(outcome)) => {
                    if let Err(e) = &outcome {
                        warn!("camera: not saved: {e}");
                    }
                    done.saved = Some(outcome);
                    done.saving = None;
                }
                Some(Err(TryRecvError::Disconnected)) => {
                    done.saved = Some(Err("the save stopped".into()));
                    done.saving = None;
                }
                Some(Err(TryRecvError::Empty)) | None => {}
            }
        }
        if let Some(key) = session.send_to.take() {
            if let Some(done) = session.result.take() {
                let name = session
                    .chats
                    .iter()
                    .find(|(k, _)| *k == key)
                    .map_or_else(|| "the chat".to_string(), |(_, n)| n.clone());
                // The pose rides in the caption too: the agent is handed the
                // chat line, not the file (`photo_pose`'s module note).
                let caption = done
                    .pose
                    .as_ref()
                    .map_or_else(String::new, |p| p.caption(""));
                chat.private_pictures
                    .push(crate::systems::chat_pictures::OutgoingPicture {
                        channel: key,
                        bytes: done.jpeg,
                        caption,
                        rgba: None,
                    });
                chat.notice = Some(crate::systems::chat::ChatNotice {
                    text: format!("Picture sent to {name}."),
                    error: false,
                });
                session.close = true;
            }
        }
        if std::mem::take(&mut session.post) {
            if let Some(done) = session.result.take() {
                // The pose's place; for an upload, the GPS position in its own
                // EXIF; else the composer asks the view.
                let place = done
                    .pose
                    .as_ref()
                    .map(|p| (p.lat, p.lng, p.place.clone()))
                    .or_else(|| {
                        crate::utils::jpeg_meta::gps(&done.jpeg).map(|(lat, lng)| (lat, lng, None))
                    })
                    .map(
                        |(latitude, longitude, name)| cartopolis_core::posts::PostPlace {
                            latitude,
                            longitude,
                            name,
                            view: None,
                        },
                    );
                screen.for_post = Some(ForPost {
                    place,
                    jpeg: done.jpeg,
                });
                session.close = true;
            }
        }

        let Some((win_w, win_h)) = window_px(&windows, headless.as_deref()) else {
            return;
        };

        // ── The pose now ──
        let photo_size = session.frame.as_ref().map(|f| (f.width, f.height));
        let lens = session.camera.as_ref().and_then(Camera::lens);
        let fix = session.location.latest();
        let attitude = session.compass.latest();
        let ground_ahn = |lat: f64, lng: f64| -> Option<f32> {
            let geo = geo?;
            terrain.is_ready().then(|| {
                let (wx, wz) = crate::systems::geo_nav::lat_lng_to_world(geo, lat, lng);
                terrain.datum_m + terrain.at(wx as f32, wz as f32)
            })
        };
        let mode_key = Some(session.mode.key().to_string());
        let facing = if session.front {
            pp::Facing::Front
        } else {
            pp::Facing::Rear
        };
        let from_sensors = session.mode.uses_camera() && fix.is_some() && attitude.is_some();
        let live = if let (true, Some(fix), Some(attitude)) = (from_sensors, fix, attitude) {
            let (heading, pitch, roll) = attitude.view_angles(session.front);
            let (fov, aspect) =
                photo_size.map_or((super::FALLBACK_LONG_FOV_DEG, 0.75), |(w, h)| {
                    (
                        photo_fov_y_deg(lens.as_ref(), w, h),
                        w as f32 / h.max(1) as f32,
                    )
                });
            Some(PhotoPose {
                lat: fix.lat,
                lng: fix.lng,
                height_m: pp::EYE_HEIGHT_M,
                heading_deg: heading as f32,
                pitch_deg: pitch as f32,
                roll_deg: roll as f32,
                fov_y_deg: fov,
                aspect,
                mode: mode_key.clone(),
                camera: facing,
                taken_ms: now_ms(),
                source: pp::PoseSource::Sensors,
                accuracy_m: Some(fix.accuracy_m as f32),
                fix: crate::platform::gnss_receiver::fix_label().map(str::to_string),
                heading_accuracy_deg: attitude.accuracy_deg.map(|a| a as f32),
                ground_ahn_m: ground_ahn(fix.lat, fix.lng),
                receiver: receiver(),
                place: None,
                client: client_version(),
            })
        } else if session.mode == Mode::Real || (session.mode == Mode::Mixed && session.moves_world)
        {
            // A photograph says where only from the sensors: a webcam's is not
            // of the view, and a phone still waiting on them has none yet. A
            // Real picture is still a picture; Mixed waits for them.
            None
        } else {
            geo.map(|geo| {
                let (lat, lng) = crate::systems::geo_nav::camera_geo(geo, &globe, cell, &tf);
                let (yaw, pitch, roll) = tf.rotation.to_euler(EulerRot::YXZ);
                let cam_fov = match projection {
                    Projection::Perspective(p) => p.fov.to_degrees(),
                    _ => 45.0,
                };
                // The picture is the polaroid's frame in World, the
                // photograph's shape otherwise.
                let aspect = match (session.mode, photo_size) {
                    (Mode::World, _) => POLAROID_ASPECT,
                    (_, Some((w, h))) => w as f32 / h.max(1) as f32,
                    _ => win_w as f32 / win_h as f32,
                };
                let shown = if session.mode == Mode::World {
                    let [_, y0, _, y1] = session.frame_rect;
                    // The frame's share of the window's height narrows the angle.
                    let half = (cam_fov.to_radians() / 2.0).tan() * (y1 - y0).max(0.01);
                    2.0 * half.atan().to_degrees()
                } else {
                    pp::cropped_fov_y_deg(win_w, win_h, aspect, cam_fov)
                };
                PhotoPose {
                    lat,
                    lng,
                    height_m: globe.altitude_m(cell, &tf),
                    heading_deg: (-yaw.to_degrees()).rem_euclid(360.0),
                    pitch_deg: pitch.to_degrees(),
                    roll_deg: roll.to_degrees(),
                    fov_y_deg: shown,
                    aspect,
                    mode: mode_key.clone(),
                    camera: facing,
                    taken_ms: now_ms(),
                    source: pp::PoseSource::View,
                    accuracy_m: None,
                    fix: None,
                    heading_accuracy_deg: None,
                    ground_ahn_m: ground_ahn(lat, lng),
                    receiver: None,
                    place: None,
                    client: client_version(),
                }
            })
        };
        session.live = live.clone();

        // ── In Mixed the world camera is the phone; held while the shutter is ──
        if session.mode == Mode::Mixed {
            let held = match &session.shutter {
                Shutter::Holding { pose, .. } | Shutter::Capturing { pose, .. } => pose.clone(),
                _ => None,
            };
            let drive = held.or_else(|| live.filter(|p| p.source == pp::PoseSource::Sensors));
            if let (Some(pose), Some(geo)) = (drive, geo) {
                // A jump only to arrive, or after a walk long enough that the
                // map should follow; every step in between moves the camera.
                let jump = session.placed_at.is_none_or(|at| {
                    crate::systems::geo_nav::great_circle_m(at, (pose.lat, pose.lng)) > JUMP_M
                });
                if jump {
                    nav.pending_teleport = Some(Teleport {
                        lat: pose.lat,
                        lng: pose.lng,
                        altitude: Some(pose.height_m),
                        pose: Some(CameraPose::free_look(
                            pose.pitch_deg,
                            pose.harness_yaw_deg(),
                        )),
                        silent: true,
                    });
                    session.placed_at = Some((pose.lat, pose.lng));
                }
                cs.free_camera_rotation = Vec2::new(
                    pose.pitch_deg.to_radians(),
                    pose.harness_yaw_deg().to_radians(),
                );
                cs.free_camera_roll = pose.roll_deg.to_radians();
                cs.free_initialized = true;
                // Through `free_fov`: `camera_controls` resets the projection's
                // lens every frame, so a direct write would be undone.
                let want =
                    pp::window_fov_y_deg(win_w, win_h, pose.aspect, pose.fov_y_deg).to_radians();
                if cs.free_fov.is_none_or(|f| (f - want).abs() > 1e-4) {
                    cs.free_fov = Some(want);
                }
                // On the fix, at eye height above the ground underfoot (not
                // the anchor's). Skipped on the frame a jump is pending.
                if !jump {
                    let (wx, wz) =
                        crate::systems::geo_nav::lat_lng_to_world(geo, pose.lat, pose.lng);
                    let (wx, wz) = (wx as f32, wz as f32);
                    let here = crate::grid::anchored_position(cell, tf.translation);
                    let want = Vec3::new(wx, terrain.at(wx, wz) + pose.height_m as f32, wz);
                    let d = want - here;
                    if d.length_squared() > 1e-4 {
                        tf.translation += d;
                    }
                }
            }
        }

        // ── The shutter ──
        let pressed = std::mem::take(&mut session.press);
        let mode = session.mode;
        let frame_rect = session.frame_rect;
        session.shutter = match std::mem::replace(&mut session.shutter, Shutter::Idle) {
            Shutter::Idle if pressed => {
                let photo = session.frame.clone();
                let pose = session.live.clone();
                match mode {
                    _ if mode.uses_camera() && photo.is_none() => {
                        session.error = Some("The camera has not delivered a picture yet.".into());
                        Shutter::Idle
                    }
                    Mode::Mixed if pose.is_none() => {
                        session.error = Some("No position and heading yet.".into());
                        Shutter::Idle
                    }
                    // The photograph alone needs no world: put it together now.
                    Mode::Real => match photo {
                        Some(photo) => {
                            compose(None, photo, pose, mode, frame_rect, &mut session.error)
                        }
                        None => Shutter::Idle,
                    },
                    _ => Shutter::Holding {
                        photo,
                        pose,
                        frames: if mode == Mode::Mixed { HOLD_FRAMES } else { 1 },
                    },
                }
            }
            Shutter::Idle => Shutter::Idle,
            Shutter::Holding {
                photo,
                pose,
                frames,
            } => {
                if frames == 0 {
                    screen.world_in = None;
                    shot.camera_snapshot = true;
                    Shutter::Capturing {
                        photo,
                        pose,
                        waited: 0,
                    }
                } else {
                    Shutter::Holding {
                        photo,
                        pose,
                        frames: frames - 1,
                    }
                }
            }
            Shutter::Capturing {
                photo,
                pose,
                waited,
            } => match screen.world_in.take() {
                Some(world) => match photo {
                    Some(photo) if mode == Mode::Mixed => compose(
                        Some(world),
                        photo,
                        pose,
                        mode,
                        frame_rect,
                        &mut session.error,
                    ),
                    _ => compose_world(world, pose, frame_rect, &mut session.error),
                },
                // A capture that never comes back (the readback failed and was
                // only logged) must not leave the screen stuck.
                None if waited > CAPTURE_GIVE_UP_FRAMES => {
                    session.error = Some("The view could not be captured. Try again.".into());
                    Shutter::Idle
                }
                None => Shutter::Capturing {
                    photo,
                    pose,
                    waited: waited + 1,
                },
            },
            Shutter::Composing { done } => match try_take(&done) {
                Ok(Ok((jpeg, rgba, size, pose))) => {
                    session.result = Some(Finished {
                        jpeg,
                        rgba,
                        size,
                        pose,
                        texture: None,
                        saving: None,
                        saved: None,
                    });
                    Shutter::Idle
                }
                Ok(Err(e)) => {
                    session.error = Some(format!("The picture could not be made: {e}"));
                    Shutter::Idle
                }
                Err(TryRecvError::Empty) => Shutter::Composing { done },
                Err(TryRecvError::Disconnected) => {
                    session.error = Some("The picture could not be made.".into());
                    Shutter::Idle
                }
            },
        };
    }

    /// A Mixed or Real picture, put together on a thread: the photograph (and
    /// the world beside it), the place looked up, the pose written in.
    fn compose(
        world: Option<(Vec<u8>, [u32; 2])>,
        photo: I420,
        pose: Option<PhotoPose>,
        mode: Mode,
        _frame: [f32; 4],
        error: &mut Option<String>,
    ) -> Shutter {
        let job = move || {
            let rgba = photo.to_rgba();
            let mut pose = pose;
            if let Some(p) = pose.as_mut() {
                p.place = super::place_at(p.lat, p.lng);
            }
            let (out, size) = match (mode, world) {
                (Mode::Mixed, Some((w, [ww, wh]))) => {
                    compose_mixed((&rgba, photo.width, photo.height), (&w, ww, wh))
                }
                _ => (rgba, [photo.width, photo.height]),
            };
            let jpeg = jpeg_with_pose(&out, size, pose.as_ref())?;
            Ok((jpeg, out, size, pose))
        };
        match spawn("camera-picture", job) {
            Ok(done) => Shutter::Composing { done },
            Err(e) => {
                *error = Some(format!("Could not make the picture: {e}"));
                Shutter::Idle
            }
        }
    }

    /// An In world picture: the polaroid's frame cut from the capture, framed,
    /// the place written on it and the pose in it.
    fn compose_world(
        (rgba, size): (Vec<u8>, [u32; 2]),
        pose: Option<PhotoPose>,
        frame: [f32; 4],
        error: &mut Option<String>,
    ) -> Shutter {
        let job = move || {
            let (cut, [cw, ch]) = crop_fraction(&rgba, size, frame);
            let mut pose = pose;
            let place = pose
                .as_ref()
                .and_then(|p| super::place_at(p.lat, p.lng))
                .unwrap_or_default();
            if let Some(p) = pose.as_mut() {
                p.place = (!place.is_empty()).then(|| place.clone());
            }
            let date = jiff::Zoned::now().strftime("%-d %B %Y").to_string();
            let card = crate::utils::world_polaroid::compose(&cut, cw, ch, &place, &date)?;
            let meta =
                pose.as_ref()
                    .map_or_else(Default::default, |p| crate::utils::jpeg_meta::Meta {
                        exif: Some(p.exif()),
                        xmp: Some(p.xmp().into_bytes()),
                    });
            let jpeg = crate::utils::jpeg_meta::write(&card, &meta);
            let (pixels, [w, h]) = crate::utils::photo::decode_rgba(&jpeg)?;
            Ok((jpeg, pixels, [w as u32, h as u32], pose))
        };
        match spawn("camera-polaroid", job) {
            Ok(done) => Shutter::Composing { done },
            Err(e) => {
                *error = Some(format!("Could not make the picture: {e}"));
                Shutter::Idle
            }
        }
    }

    /// The camera's screen: the picture filling it (or, In world, the map
    /// through the polaroid's frame), the modes beside the shutter, the flip
    /// under it and the upload above it.
    pub fn render_camera(mut egui: EguiMut, mut screen: ResMut<CameraScreen>) {
        let Some(session) = screen.session.as_mut() else {
            return;
        };
        let ctx = egui.ctx_mut().clone();
        let viewport = egui.viewport_pts();
        let full = egui::Rect::from_min_size(egui::Pos2::ZERO, viewport);
        if session.result.is_some() {
            render_result(&ctx, full, session);
            return;
        }
        // While the shutter runs the screen stays out of the picture: the
        // capture hides every panel, and this is one.
        let sideways = viewport.x > viewport.y;

        // The newest frame as a texture, small enough to upload every frame.
        if let Some(frame) = session
            .frame
            .as_ref()
            .filter(|_| session.mode.uses_camera())
        {
            let max = 720u32.min(frame.width.max(frame.height));
            let (w, h) = if frame.width >= frame.height {
                (max, ((frame.height * max / frame.width.max(1)) & !1).max(2))
            } else {
                (((frame.width * max / frame.height.max(1)) & !1).max(2), max)
            };
            let small = if frame.width > w {
                frame.scaled(w, h)
            } else {
                frame.clone()
            };
            // A selfie is shown as a mirror shows you; it is saved the right
            // way round.
            let rgba = if session.front {
                small.to_rgba_mirrored(true)
            } else {
                small.to_rgba()
            };
            let image = egui::ColorImage::from_rgba_unmultiplied(
                [small.width as usize, small.height as usize],
                &rgba,
            );
            match session.texture.as_mut() {
                Some(t) => t.set(image, egui::TextureOptions::LINEAR),
                None => {
                    session.texture =
                        Some(ctx.load_texture("camera", image, egui::TextureOptions::LINEAR))
                }
            }
        }

        // The controls' column (sideways: the right edge) or row (upright:
        // the bottom), and what the picture may fill.
        let shutter_c = if sideways {
            egui::pos2(full.right() - 56.0, full.center().y)
        } else {
            egui::pos2(full.center().x, full.bottom() - 84.0)
        };
        let picture = if sideways {
            egui::Rect::from_min_max(full.min, egui::pos2(full.right() - 112.0, full.bottom()))
        } else {
            egui::Rect::from_min_max(full.min, egui::pos2(full.right(), full.bottom() - 168.0))
        };

        // In world the map inside the frame stays the map, to be framed by
        // dragging: the screen then covers only its controls' strip, and the
        // close button has an area of its own.
        let world = session.mode == Mode::World;
        let strip = if sideways {
            egui::Rect::from_min_max(egui::pos2(picture.right(), full.top()), full.max)
        } else {
            egui::Rect::from_min_max(egui::pos2(full.left(), picture.bottom()), full.max)
        };
        let area = if world { strip } else { full };
        let close = egui::Rect::from_min_size(
            egui::pos2(full.left() + 14.0, full.top() + 14.0),
            egui::vec2(theme::ROUND_BUTTON_SIZE, theme::ROUND_BUTTON_SIZE),
        );
        if world {
            egui::Area::new(egui::Id::new("camera_close"))
                .order(egui::Order::Foreground)
                .fixed_pos(close.min)
                .show(&ctx, |ui| {
                    if theme::round_button(ui, icons::CLEAR, "Close the camera", false).clicked()
                        && !session.busy()
                    {
                        session.close = true;
                    }
                });
        }
        egui::Area::new(egui::Id::new("camera_screen"))
            .order(egui::Order::Foreground)
            .fixed_pos(area.min)
            .show(&ctx, |ui| {
                ui.set_min_size(area.size());
                let painter = ui.painter().clone();
                let photo = session.texture.as_ref().map(|t| (t.id(), t.size_vec2()));
                match session.mode {
                    Mode::World => {
                        // The polaroid's frame over the map: the world outside
                        // it dimmed, and the picture is what is inside.
                        let room = picture.shrink(24.0);
                        let size = if room.width() / room.height() > POLAROID_ASPECT {
                            egui::vec2(room.height() * POLAROID_ASPECT, room.height())
                        } else {
                            egui::vec2(room.width(), room.width() / POLAROID_ASPECT)
                        };
                        let frame = egui::Rect::from_center_size(room.center(), size);
                        session.frame_rect = [
                            frame.min.x / viewport.x,
                            frame.min.y / viewport.y,
                            frame.max.x / viewport.x,
                            frame.max.y / viewport.y,
                        ];
                        let shade = egui::Color32::from_black_alpha(140);
                        for r in [
                            egui::Rect::from_min_max(full.min, egui::pos2(full.max.x, frame.min.y)),
                            egui::Rect::from_min_max(egui::pos2(full.min.x, frame.max.y), full.max),
                            egui::Rect::from_min_max(
                                egui::pos2(full.min.x, frame.min.y),
                                egui::pos2(frame.min.x, frame.max.y),
                            ),
                            egui::Rect::from_min_max(
                                egui::pos2(frame.max.x, frame.min.y),
                                egui::pos2(full.max.x, frame.max.y),
                            ),
                        ] {
                            painter.rect_filled(r, 0.0, shade);
                        }
                        painter.rect_stroke(
                            frame,
                            0.0,
                            egui::Stroke::new(6.0, egui::Color32::from_rgb(0xFB, 0xFA, 0xF6)),
                            egui::StrokeKind::Outside,
                        );
                        // Only the controls take touches: the map inside the
                        // frame is still the map, to be framed by dragging.
                    }
                    Mode::Real => {
                        ui.allocate_rect(full, egui::Sense::click_and_drag());
                        painter.rect_filled(full, 0.0, theme::INK);
                        if let Some((id, size)) = photo {
                            painter.image(
                                id,
                                full,
                                cover_uv(size, full.size()),
                                egui::Color32::WHITE,
                            );
                        }
                    }
                    Mode::Mixed => {
                        ui.allocate_rect(full, egui::Sense::click_and_drag());
                        // The photograph in one half; the other half is left
                        // open, and the world drawn from the phone's pose
                        // shows through it.
                        let half = if sideways {
                            egui::Rect::from_min_max(
                                picture.min,
                                egui::pos2(picture.center().x, picture.max.y),
                            )
                        } else {
                            egui::Rect::from_min_max(
                                picture.min,
                                egui::pos2(picture.max.x, picture.center().y),
                            )
                        };
                        painter.rect_filled(half, 0.0, theme::INK);
                        if let Some((id, size)) = photo {
                            painter.image(
                                id,
                                half,
                                cover_uv(size, half.size()),
                                egui::Color32::WHITE,
                            );
                        }
                        let line = if sideways {
                            [half.right_top(), half.right_bottom()]
                        } else {
                            [half.left_bottom(), half.right_bottom()]
                        };
                        painter.line_segment(line, egui::Stroke::new(2.0, egui::Color32::WHITE));
                        tag(&painter, half.min + egui::vec2(12.0, 52.0), "REAL");
                        let other = if sideways {
                            egui::pos2(half.right() + 12.0, half.top() + 52.0)
                        } else {
                            egui::pos2(half.left() + 12.0, half.bottom() + 12.0)
                        };
                        tag(&painter, other, "CARTOPOLIS");
                    }
                }

                // Close, top left.
                if !world
                    && round(ui, &painter, close, icons::CLEAR, false, "camera_close").clicked()
                    && !session.busy()
                {
                    session.close = true;
                }

                // The pose's quality, where it means something.
                if session.mode.uses_camera() {
                    let mut x = full.left() + 14.0;
                    let y = if sideways {
                        full.bottom() - 40.0
                    } else {
                        picture.bottom() - 40.0
                    };
                    for (text, tone) in chips(session) {
                        x = chip(&painter, egui::pos2(x, y), &text, tone) + 8.0;
                    }
                }

                // The shutter, the modes beside it, upload before it and the
                // flip after it.
                let shutter = egui::Rect::from_center_size(shutter_c, egui::vec2(72.0, 72.0));
                let sresp = ui.interact(
                    shutter,
                    egui::Id::new("camera_shutter"),
                    egui::Sense::click(),
                );
                painter.circle_stroke(
                    shutter.center(),
                    34.0,
                    egui::Stroke::new(4.0, egui::Color32::WHITE),
                );
                let ready = !session.busy()
                    && (session.mode != Mode::Mixed || session.live.is_some())
                    && (!session.mode.uses_camera() || session.frame.is_some());
                let fill = if ready {
                    match session.mode {
                        Mode::World => theme::BRAND,
                        _ => egui::Color32::WHITE,
                    }
                } else {
                    theme::TEXT_DIM
                };
                painter.circle_filled(shutter.center(), 27.0, fill);
                if session.mode == Mode::Mixed && ready {
                    // Half photograph, half world: the Mixed mark.
                    painter.add(egui::Shape::convex_polygon(
                        half_disc(shutter.center(), 27.0),
                        theme::BRAND,
                        egui::Stroke::NONE,
                    ));
                }
                if sresp.clicked() && !session.busy() {
                    session.error = None;
                    session.press = true;
                }
                let step = if sideways {
                    egui::vec2(0.0, 76.0)
                } else {
                    egui::vec2(-96.0, 0.0)
                };
                let upload = egui::Rect::from_center_size(
                    shutter_c - step,
                    egui::vec2(theme::ROUND_BUTTON_SIZE, theme::ROUND_BUTTON_SIZE),
                );
                if round(ui, &painter, upload, icons::UPLOAD, false, "camera_upload")
                    .on_hover_text("Upload a picture from the phone")
                    .clicked()
                    && !session.busy()
                {
                    session.pick_upload = true;
                }
                let flip = egui::Rect::from_center_size(
                    shutter_c + step,
                    egui::vec2(theme::ROUND_BUTTON_SIZE, theme::ROUND_BUTTON_SIZE),
                );
                if round(
                    ui,
                    &painter,
                    flip,
                    icons::FLIP_CAMERA,
                    session.front,
                    "camera_flip",
                )
                .on_hover_text("Selfie")
                .clicked()
                    && !session.busy()
                {
                    session.flip = true;
                }
                // The modes: a column left of the shutter held sideways, a row
                // above it held upright.
                for (i, m) in Mode::ALL.into_iter().enumerate() {
                    let at = if sideways {
                        egui::pos2(shutter_c.x - 56.0, shutter_c.y + (i as f32 - 1.0) * 30.0)
                    } else {
                        egui::pos2(shutter_c.x + (i as f32 - 1.0) * 92.0, shutter_c.y - 60.0)
                    };
                    let align = if sideways {
                        egui::Align2::RIGHT_CENTER
                    } else {
                        egui::Align2::CENTER_CENTER
                    };
                    let on = m == session.mode;
                    let rect = painter.text(
                        at,
                        align,
                        m.label(),
                        egui::FontId::proportional(14.0),
                        if on {
                            theme::BRAND
                        } else {
                            egui::Color32::WHITE
                        },
                    );
                    let hit = rect.expand(8.0);
                    if ui
                        .interact(hit, egui::Id::new(("camera_mode", i)), egui::Sense::click())
                        .clicked()
                        && !on
                        && !session.busy()
                    {
                        session.switch = Some(m);
                    }
                }

                let status = if session.upload.is_some() {
                    Some("Choose a picture…".to_string())
                } else if session.busy() {
                    Some("Taking the picture…".to_string())
                } else {
                    session.error.clone()
                };
                if let Some(text) = status {
                    let at = if sideways {
                        egui::pos2(picture.center().x, full.bottom() - 70.0)
                    } else {
                        egui::pos2(full.center().x, shutter_c.y - 96.0)
                    };
                    let galley = painter.layout_no_wrap(
                        text,
                        egui::FontId::proportional(13.0),
                        egui::Color32::WHITE,
                    );
                    let r =
                        egui::Rect::from_center_size(at, galley.size() + egui::vec2(20.0, 10.0));
                    painter.rect_filled(r, 10.0, egui::Color32::from_black_alpha(170));
                    painter.galley(r.min + egui::vec2(10.0, 5.0), galley, egui::Color32::WHITE);
                }
            });
    }

    /// The picture once it is made: shown as large as fits, with Post, Send,
    /// To phone and Retake as round buttons beside (or under) it.
    fn render_result(ctx: &egui::Context, full: egui::Rect, session: &mut Session) {
        let Some(done) = session.result.as_mut() else {
            return;
        };
        let texture = done
            .texture
            .get_or_insert_with(|| {
                let image = egui::ColorImage::from_rgba_unmultiplied(
                    [done.size[0] as usize, done.size[1] as usize],
                    &done.rgba,
                );
                ctx.load_texture("camera-result", image, egui::TextureOptions::LINEAR)
            })
            .clone();
        let save_line = if done.saving.is_some() {
            Some(("Saving…".to_string(), false))
        } else {
            match &done.saved {
                Some(Ok(at)) => Some((format!("Saved to {at}"), false)),
                Some(Err(e)) => Some((format!("Not saved: {e}"), true)),
                None => None,
            }
        };
        let saving = done.saving.is_some();
        let place = done.pose.as_ref().and_then(|p| p.place.clone());
        let sideways = full.width() > full.height();
        let target = session.target.clone();
        let chats = session.chats.clone();
        egui::Area::new(egui::Id::new("camera_result"))
            .order(egui::Order::Foreground)
            .fixed_pos(egui::Pos2::ZERO)
            .show(ctx, |ui| {
                ui.set_min_size(full.size());
                let painter = ui.painter().clone();
                ui.allocate_rect(full, egui::Sense::click_and_drag());
                painter.rect_filled(full, 0.0, theme::INK);

                let (room, side) = if sideways {
                    (
                        egui::Rect::from_min_max(
                            full.min + egui::vec2(16.0, 16.0),
                            egui::pos2(full.right() - 300.0, full.bottom() - 16.0),
                        ),
                        egui::Rect::from_min_max(
                            egui::pos2(full.right() - 284.0, full.top() + 20.0),
                            full.max - egui::vec2(16.0, 16.0),
                        ),
                    )
                } else {
                    (
                        egui::Rect::from_min_max(
                            full.min + egui::vec2(12.0, 64.0),
                            egui::pos2(full.right() - 12.0, full.bottom() - 240.0),
                        ),
                        egui::Rect::from_min_max(
                            egui::pos2(full.left() + 16.0, full.bottom() - 224.0),
                            full.max - egui::vec2(16.0, 16.0),
                        ),
                    )
                };
                let size = texture.size_vec2();
                let scale = (room.width() / size.x).min(room.height() / size.y);
                let shown = egui::Rect::from_center_size(room.center(), size * scale);
                painter.image(
                    texture.id(),
                    shown,
                    egui::Rect::from_min_max(egui::pos2(0.0, 0.0), egui::pos2(1.0, 1.0)),
                    egui::Color32::WHITE,
                );

                ui.scope_builder(egui::UiBuilder::new().max_rect(side), |ui| {
                    ui.vertical(|ui| {
                        if let Some(place) = &place {
                            ui.label(
                                egui::RichText::new(place)
                                    .size(theme::SIZE_TITLE)
                                    .color(theme::CREAM),
                            );
                            ui.add_space(8.0);
                        }
                        if session.choosing_chat {
                            ui.label(
                                egui::RichText::new("Send to")
                                    .size(theme::SIZE_BODY)
                                    .color(theme::TEXT_DIM),
                            );
                            egui::ScrollArea::vertical()
                                .max_height(180.0)
                                .show(ui, |ui| {
                                    if chats.is_empty() {
                                        theme::note(ui, "No private chats yet.");
                                    }
                                    for (key, name) in &chats {
                                        if theme::action_button(ui, name).clicked() {
                                            session.send_to = Some(key.clone());
                                        }
                                    }
                                });
                            if theme::action_button(ui, "Back").clicked() {
                                session.choosing_chat = false;
                            }
                            return;
                        }
                        ui.horizontal(|ui| {
                            ui.spacing_mut().item_spacing.x = 18.0;
                            if labelled(ui, icons::NOTE, "Post", true) {
                                session.post = true;
                            }
                            if labelled(ui, icons::CHAT, "Send", false) {
                                match &target {
                                    Target::Chat(key) => session.send_to = Some(key.clone()),
                                    _ => session.choosing_chat = true,
                                }
                            }
                            if !saving && labelled(ui, icons::DOWNLOAD, "To phone", false) {
                                session.save = true;
                            }
                            if labelled(ui, icons::CLEAR, "Retake", false) {
                                session.retake = true;
                            }
                        });
                        if let Some((line, bad)) = &save_line {
                            ui.add_space(6.0);
                            ui.label(egui::RichText::new(line).size(12.5).color(if *bad {
                                theme::ERROR
                            } else {
                                theme::TEXT_DIM
                            }));
                        }
                        if done.pose.is_some() {
                            ui.add_space(6.0);
                            theme::note(
                                ui,
                                "Where it was taken, the direction, tilt and lens are in the \
                                 picture: enough to put the camera back on the same view.",
                            );
                        }
                    });
                });
            });
        // Retake also leaves the result: the close button is the camera's.
        if session.retake {
            session.choosing_chat = false;
        }
    }

    /// A round button with its label under it, the same size as every other.
    fn labelled(ui: &mut egui::Ui, icon: &str, label: &str, hot: bool) -> bool {
        ui.vertical_centered(|ui| {
            ui.set_width(theme::ROUND_BUTTON_SIZE + 16.0);
            let clicked = if hot {
                let (rect, resp) = ui.allocate_exact_size(
                    egui::Vec2::splat(theme::ROUND_BUTTON_SIZE),
                    egui::Sense::click(),
                );
                ui.painter().circle_filled(
                    rect.center(),
                    theme::ROUND_BUTTON_SIZE / 2.0,
                    theme::BRAND,
                );
                ui.painter().text(
                    rect.center(),
                    egui::Align2::CENTER_CENTER,
                    icon,
                    egui::FontId::proportional(18.0),
                    egui::Color32::WHITE,
                );
                resp.on_hover_text(label).clicked()
            } else {
                theme::round_button(ui, icon, label, false).clicked()
            };
            ui.label(egui::RichText::new(label).size(12.0).color(theme::TEXT));
            clicked
        })
        .inner
    }

    /// The right half of a disc, as a polygon.
    fn half_disc(c: egui::Pos2, r: f32) -> Vec<egui::Pos2> {
        (0..=16)
            .map(|i| {
                let a = -std::f32::consts::FRAC_PI_2 + std::f32::consts::PI * i as f32 / 16.0;
                c + egui::vec2(a.cos(), a.sin()) * r
            })
            .collect()
    }

    #[derive(Clone, Copy)]
    enum Tone {
        Good,
        Warn,
        Plain,
    }

    fn chips(session: &Session) -> Vec<(String, Tone)> {
        // What the shutter is still waiting for, one thing at a time, in the
        // order it is needed: permission and a fix, then the compass.
        let Some(pose) = session.live.as_ref() else {
            let fix = session.location.latest();
            let first = match (fix, session.location.error()) {
                (Some(f), _) => format!("GPS · {:.0} m", f.accuracy_m),
                (None, Some(e)) => format!("No position: {e}"),
                (None, None) => "Waiting for GPS…".to_string(),
            };
            let mut chips = vec![(first, Tone::Warn)];
            if fix.is_some() {
                chips.push((
                    match session.compass.error() {
                        Some(e) => format!("No compass: {e}"),
                        None => "Waiting for the compass…".to_string(),
                    },
                    Tone::Warn,
                ));
            }
            return chips;
        };
        if pose.source == pp::PoseSource::View {
            return vec![("No sensors: the pose is this view".into(), Tone::Plain)];
        }
        let fix = pose.fix.clone().unwrap_or_else(|| "GPS".into());
        let acc = pose.accuracy_m.map_or(String::new(), |a| {
            if a < 1.0 {
                format!(" ±{:.0} cm", (a * 100.0).max(1.0))
            } else {
                format!(" ±{a:.1} m")
            }
        });
        let good = pose.fix.as_deref() == Some("RTK fixed");
        let heading = match pose.heading_accuracy_deg {
            Some(e) => format!("{:.0}° ±{e:.0}°", pose.heading_deg),
            None => format!("{:.0}°", pose.heading_deg),
        };
        vec![
            (
                format!("{fix}{acc}"),
                if good { Tone::Good } else { Tone::Warn },
            ),
            (heading, Tone::Plain),
        ]
    }

    /// A chip at `at`; returns its right edge.
    fn chip(painter: &egui::Painter, at: egui::Pos2, text: &str, tone: Tone) -> f32 {
        let color = match tone {
            Tone::Good => egui::Color32::from_rgb(0x9f, 0xd3, 0xb4),
            Tone::Warn => egui::Color32::from_rgb(0xff, 0xcf, 0x7a),
            Tone::Plain => theme::CREAM,
        };
        let galley =
            painter.layout_no_wrap(text.to_string(), egui::FontId::proportional(12.5), color);
        let rect = egui::Rect::from_min_size(at, galley.size() + egui::vec2(20.0, 10.0));
        painter.rect_filled(rect, 11.0, egui::Color32::from_black_alpha(170));
        painter.galley(at + egui::vec2(10.0, 5.0), galley, color);
        rect.right()
    }

    fn tag(painter: &egui::Painter, at: egui::Pos2, text: &str) {
        let galley = painter.layout_no_wrap(
            text.to_string(),
            egui::FontId::proportional(11.0),
            egui::Color32::WHITE,
        );
        let rect = egui::Rect::from_min_size(at, galley.size() + egui::vec2(14.0, 6.0));
        painter.rect_filled(rect, 7.0, egui::Color32::from_black_alpha(180));
        painter.galley(at + egui::vec2(7.0, 3.0), galley, egui::Color32::WHITE);
    }

    /// A round button drawn at `rect`, the same disc as `theme::round_button`.
    fn round(
        ui: &mut egui::Ui,
        painter: &egui::Painter,
        rect: egui::Rect,
        icon: &str,
        on: bool,
        id: &str,
    ) -> egui::Response {
        let resp = ui.interact(rect, egui::Id::new(id), egui::Sense::click());
        let fill = if on {
            theme::ACCENT
        } else {
            egui::Color32::from_black_alpha(150)
        };
        painter.circle_filled(rect.center(), rect.width() / 2.0, fill);
        painter.text(
            rect.center(),
            egui::Align2::CENTER_CENTER,
            icon,
            egui::FontId::proportional(18.0),
            if on { theme::INK } else { egui::Color32::WHITE },
        );
        resp
    }

    /// The part of a `size` texture that fills `area` without stretching.
    fn cover_uv(size: egui::Vec2, area: egui::Vec2) -> egui::Rect {
        let (tw, th) = (size.x.max(1.0), size.y.max(1.0));
        let (aw, ah) = (area.x.max(1.0), area.y.max(1.0));
        if tw / th > aw / ah {
            let u = (ah / th * tw - aw) / (ah / th * tw) / 2.0;
            egui::Rect::from_min_max(egui::pos2(u, 0.0), egui::pos2(1.0 - u, 1.0))
        } else {
            let v = (aw / tw * th - ah) / (aw / tw * th) / 2.0;
            egui::Rect::from_min_max(egui::pos2(0.0, v), egui::pos2(1.0, 1.0 - v))
        }
    }
}

/// On the web there is no camera, so nothing to drive or draw.
#[cfg(target_arch = "wasm32")]
pub fn drive_camera() {}

#[cfg(target_arch = "wasm32")]
pub fn render_camera() {}

#[cfg(test)]
mod tests {
    use super::*;

    fn solid(w: u32, h: u32, v: u8) -> Vec<u8> {
        (0..w * h).flat_map(|_| [v, v, v, 255]).collect()
    }

    /// Held sideways the halves sit side by side, held upright one above the
    /// other, and each half has the photograph's shape.
    #[test]
    fn mixed_halves_follow_the_way_the_phone_was_held() {
        let world = solid(1000, 500, 50);
        let (_, sideways) = compose_mixed((&solid(400, 300, 200), 400, 300), (&world, 1000, 500));
        assert_eq!(sideways, [1600, 600]);
        let (px, upright) = compose_mixed((&solid(300, 400, 200), 300, 400), (&world, 1000, 500));
        assert_eq!(upright, [600, 1600]);
        // Photograph on top, world under it.
        assert_eq!(px[0], 200);
        assert_eq!(px[(600 * 1599 * 4) as usize], 50);
    }

    #[test]
    fn the_polaroid_is_what_was_in_its_frame() {
        let (w, h) = (100u32, 50u32);
        let rgba: Vec<u8> = (0..w * h)
            .flat_map(|i| [(i % w) as u8, (i / w) as u8, 0, 255])
            .collect();
        let (out, size) = crop_fraction(&rgba, [w, h], [0.3, 0.1, 0.7, 0.9]);
        assert_eq!(size, [40, 40]);
        assert_eq!(&out[..2], &[30, 5]);
    }

    /// Without lens data the long side sees a main camera's 72°, and turning
    /// the phone swaps which side that is.
    #[test]
    fn a_photograph_without_lens_data_gets_a_main_cameras_angle() {
        let wide = photo_fov_y_deg(None, 1920, 1440);
        let tall = photo_fov_y_deg(None, 1440, 1920);
        assert!((tall - FALLBACK_LONG_FOV_DEG).abs() < 0.01, "{tall}");
        assert!(wide < tall);
    }
}
