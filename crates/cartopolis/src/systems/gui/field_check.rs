//! The **field check** (`docs/mockups/field-check/v4.html`): a photograph,
//! Cartopolis drawn from the same pose, and the aerial photograph seen from
//! that pose, side by side in one picture. The picture is shown when it is
//! made, and one button sends it into the private chat with the agent, which
//! says where the world is wrong and can put the camera back on the same view
//! later.
//!
//! A developer tool: it opens from Developer tools ▸ View. It used to be a
//! camera button in the chat bar of a chat with the agent, where nobody found
//! it. The picture goes to the first account in [`FieldCheck::agents`]
//! (`CARTO_FIELD_CHECK_WITH`, default [`DEFAULT_WITH`]) this device has a
//! direct chat with.
//!
//! **The aerial panel is the map from above**: PDOK's aerial photograph around
//! where the camera stood, the camera a red dot, the line of sight red, its aim
//! a yellow cross (`aerial_reference::from_above`, the `.aerial-map.jpg`
//! `--aerial-reference` writes). The same photograph seen from the camera
//! (`from_camera`) was tried first and lost: from eye height it is a smear of
//! paving with every building flattened into it. The Netherlands only;
//! elsewhere the panel says so.
//!
//! **How the three panels come to share a pose.** On a phone, while the capture
//! screen is up, the world camera *is* the phone: placed on the position fix at
//! eye height above the measured ground, turned by the compass and the gravity
//! sensor, and given the photograph's lens. The shutter takes the newest camera
//! frame and the pose it was taken at, holds the world camera there for a few
//! frames, and captures the window with the UI hidden for one frame — the same
//! chrome-free capture "Send a snapshot" makes (`ui::drive_screenshots`). The
//! aerial is fetched around the same pose, the three are put side by side, and the pose is written under them and into the caption
//! ([`cartopolis_core::field_check`]).
//!
//! On a desktop there are no sensors, so the world camera is left where the
//! person put it and *that* is the pose (`PoseSource::View`); the photograph is
//! the webcam's. It is a test of the pipeline, and the strip says so.
//!
//! **Only one camera can be open**: a field check while the high-precision mode
//! holds the back camera reports the camera as taken.
// The web build has no camera: the button is never offered there, so the
// picture-making half is compiled (and tested) but never called.
#![cfg_attr(target_arch = "wasm32", allow(dead_code))]

use bevy::prelude::*;
use cartopolis_core::field_check::{self as fc, FieldPose};

#[cfg(not(target_arch = "wasm32"))]
use native::Session;

/// Who the picture is sent to when nothing else says.
pub const DEFAULT_WITH: &str = "viberfox";

/// Each panel of the picture, and the strip under them, in pixels. Three
/// panels make 1584 px, inside the 1600 px a chat picture is re-encoded to, so
/// nothing is scaled twice.
const PANEL_W: u32 = 528;
const PANEL_H: u32 = 704;
const STRIP_H: u32 = 120;

/// The panels' names, left to right.
const PANELS: [&str; 3] = ["REAL", "CARTOPOLIS", "AERIAL"];

/// How far ahead the aerial panel's line of sight reaches, metres: where a
/// view looking down meets the ground, and no farther than this, which is where
/// a level one ends. The same 150 m `--aerial-reference` aims a level view at.
const AIM_MAX_M: f32 = 150.0;

/// Frames to hold the world camera on the shutter's pose before capturing it,
/// so the capture is of that pose and not of the frame before it.
const HOLD_FRAMES: u8 = 3;

/// The long side of a phone camera's view when it publishes no lens data, in
/// degrees: a main camera is about 26 mm equivalent, 69–72° across the long
/// side (the high-precision tracker assumes the same, `precise_tracker`).
const FALLBACK_LONG_FOV_DEG: f32 = 72.0;

/// The field check: who it is sent to, a request from the developer panel,
/// and the capture in progress.
#[derive(Resource)]
pub struct FieldCheck {
    /// Lowercase usernames the picture may go to, in order of preference.
    agents: Vec<String>,
    /// From Developer tools: open the capture screen.
    pub request: bool,
    /// The world, captured for the shutter by `ui::drive_screenshots`: RGBA
    /// pixels and their size.
    pub world_in: Option<(Vec<u8>, [u32; 2])>,
    #[cfg(not(target_arch = "wasm32"))]
    session: Option<Session>,
}

impl Default for FieldCheck {
    fn default() -> Self {
        let list = std::env::var("CARTO_FIELD_CHECK_WITH").unwrap_or_else(|_| DEFAULT_WITH.into());
        Self {
            agents: parse_with(&list),
            request: false,
            world_in: None,
            #[cfg(not(target_arch = "wasm32"))]
            session: None,
        }
    }
}

fn parse_with(list: &str) -> Vec<String> {
    list.split(',')
        .map(|s| s.trim().to_lowercase())
        .filter(|s| !s.is_empty())
        .collect()
}

impl FieldCheck {
    /// The direct chat the picture goes to: the first of [`Self::agents`]
    /// this device has one with, as `(username, channel key)`.
    #[must_use]
    pub fn agent_chat(&self, chat: &crate::systems::chat::ChatState) -> Option<(String, String)> {
        self.agents.iter().find_map(|a| {
            chat.direct
                .iter()
                .find(|(peer, _)| *peer == a)
                .map(|(peer, key)| (peer.clone(), key.clone()))
        })
    }
}

/// The three panels side by side over the strip: `(rgba, [width, height])`.
///
/// `photo` and `world` are RGBA of their own sizes, each cropped to the
/// picture's 3:4 about its centre (`field_check::picture_crop`) and scaled to
/// one panel. `aerial` is the map from above, RGB: it is fitted whole into its
/// panel instead, because its shape follows the line of sight and a crop could
/// cut off the camera. No `aerial` (outside the imagery, or it could not be
/// fetched) leaves its panel dark with `aerial_note` in it. Pure, so it runs
/// on a thread and is tested off any GPU.
#[must_use]
pub fn compose(
    photo: (&[u8], u32, u32),
    world: (&[u8], u32, u32),
    aerial: Option<&image::RgbImage>,
    aerial_note: &str,
    pose: &FieldPose,
    font: Option<&fontdue::Font>,
) -> (Vec<u8>, [u32; 2]) {
    use image::{imageops, DynamicImage, Rgba, RgbaImage};
    let (w, h) = (PANEL_W * 3, PANEL_H + STRIP_H);
    let mut canvas = RgbaImage::from_pixel(w, h, Rgba([0x0A, 0x0E, 0x10, 0xFF]));
    let aerial = aerial.map(|a| DynamicImage::ImageRgb8(a.clone()).to_rgba8());
    let panels = [
        RgbaImage::from_raw(photo.1, photo.2, photo.0.to_vec()),
        RgbaImage::from_raw(world.1, world.2, world.0.to_vec()),
        aerial,
    ];
    for (i, img) in panels.iter().enumerate() {
        let Some(img) = img else {
            continue;
        };
        let x0 = i64::from(PANEL_W * i as u32);
        if i == 2 {
            let scale = (PANEL_W as f32 / img.width().max(1) as f32)
                .min(PANEL_H as f32 / img.height().max(1) as f32);
            let (fw, fh) = (
                ((img.width() as f32 * scale) as u32).clamp(1, PANEL_W),
                ((img.height() as f32 * scale) as u32).clamp(1, PANEL_H),
            );
            let fitted = imageops::resize(img, fw, fh, imageops::FilterType::Triangle);
            let (ox, oy) = ((PANEL_W - fw) / 2, (PANEL_H - fh) / 2);
            imageops::overlay(&mut canvas, &fitted, x0 + i64::from(ox), i64::from(oy));
            continue;
        }
        let (x, y, cw, ch) = fc::picture_crop(img.width(), img.height());
        let cropped = imageops::crop_imm(img, x, y, cw, ch).to_image();
        let panel = imageops::resize(&cropped, PANEL_W, PANEL_H, imageops::FilterType::Triangle);
        imageops::overlay(&mut canvas, &panel, x0, 0);
    }
    if let Some(font) = font {
        for (i, label) in PANELS.into_iter().enumerate() {
            let x0 = PANEL_W as i32 * i as i32 + 16;
            let width = text_width(font, label, 24.0) as i32;
            fill_rect(&mut canvas, x0, 16, width + 22, 38, [10, 14, 16, 200]);
            draw_text(&mut canvas, font, label, x0 + 11, 44, 24.0, [255, 255, 255]);
        }
        if panels[2].is_none() {
            let x0 = PANEL_W as i32 * 2 + 24;
            draw_text(
                &mut canvas,
                font,
                aerial_note,
                x0,
                PANEL_H as i32 / 2,
                22.0,
                [0xA5, 0xB2, 0xB7],
            );
        }
        let [first, second] = pose.strip_lines();
        let top = PANEL_H as i32;
        draw_text(
            &mut canvas,
            font,
            &first,
            24,
            top + 50,
            27.0,
            [0xF3, 0xF7, 0xF9],
        );
        draw_text(
            &mut canvas,
            font,
            &second,
            24,
            top + 94,
            23.0,
            [0xA5, 0xB2, 0xB7],
        );
    }
    (canvas.into_raw(), [w, h])
}

fn text_width(font: &fontdue::Font, text: &str, px: f32) -> f32 {
    text.chars()
        .map(|c| font.metrics(c, px).advance_width)
        .sum()
}

/// One line of text, left edge `x`, baseline `baseline`, blended over.
fn draw_text(
    canvas: &mut image::RgbaImage,
    font: &fontdue::Font,
    text: &str,
    x: i32,
    baseline: i32,
    px: f32,
    rgb: [u8; 3],
) {
    let (cw, ch) = (canvas.width() as i32, canvas.height() as i32);
    let mut pen = x as f32;
    for c in text.chars() {
        let (m, bitmap) = font.rasterize(c, px);
        let gx = pen.round() as i32 + m.xmin;
        let gy = baseline - m.height as i32 - m.ymin;
        for row in 0..m.height {
            for col in 0..m.width {
                let a = u32::from(bitmap[row * m.width + col]);
                let (px_x, px_y) = (gx + col as i32, gy + row as i32);
                if a == 0 || px_x < 0 || px_y < 0 || px_x >= cw || px_y >= ch {
                    continue;
                }
                let p = canvas.get_pixel_mut(px_x as u32, px_y as u32);
                for k in 0..3 {
                    let under = u32::from(p.0[k]);
                    p.0[k] = ((u32::from(rgb[k]) * a + under * (255 - a)) / 255) as u8;
                }
            }
        }
        pen += m.advance_width;
    }
}

fn fill_rect(canvas: &mut image::RgbaImage, x: i32, y: i32, w: i32, h: i32, rgba: [u8; 4]) {
    let a = u32::from(rgba[3]);
    for py in y.max(0)..(y + h).min(canvas.height() as i32) {
        for px in x.max(0)..(x + w).min(canvas.width() as i32) {
            let p = canvas.get_pixel_mut(px as u32, py as u32);
            for k in 0..3 {
                let under = u32::from(p.0[k]);
                p.0[k] = ((u32::from(rgba[k]) * a + under * (255 - a)) / 255) as u8;
            }
        }
    }
}

/// The brand face the street names are drawn in, for the strip.
fn strip_font() -> Option<fontdue::Font> {
    fontdue::Font::from_bytes(
        crate::systems::theme::BRAND_TTF,
        fontdue::FontSettings::default(),
    )
    .ok()
}

/// The vertical field of view of a photograph `width × height` (upright, as
/// delivered) once cropped to the picture's 3:4, from what its camera says
/// about its lens; without lens data, a main camera's usual angle along the
/// long side.
#[must_use]
pub fn photo_fov_y_deg(lens: Option<&cartopolis_core::lens::Lens>, width: u32, height: u32) -> f32 {
    // The sensor's own frame is the upright one turned back.
    let turned = lens.is_some_and(|l| l.orientation_deg % 180 == 90);
    let (sw, sh) = if turned {
        (height, width)
    } else {
        (width, height)
    };
    let upright_v = lens
        .and_then(|l| l.field_of_view_deg(sw, sh))
        .map(|(h, v)| if turned { h } else { v })
        .unwrap_or_else(|| {
            let long = width.max(height) as f32;
            let focal = long / 2.0 / (FALLBACK_LONG_FOV_DEG.to_radians() / 2.0).tan();
            2.0 * (height as f32 / 2.0 / focal).atan().to_degrees()
        });
    fc::cropped_fov_y_deg(width, height, upright_v)
}

#[cfg(not(target_arch = "wasm32"))]
pub use native::{drive_field_check, render_field_check};

#[cfg(not(target_arch = "wasm32"))]
mod native {
    use super::{
        compose, fc, photo_fov_y_deg, strip_font, FieldCheck, FieldPose, AIM_MAX_M, HOLD_FRAMES,
    };
    use crate::systems::aerial_reference;
    use crate::systems::egui_manager::EguiMut;
    use bevy::prelude::*;
    use big_space::prelude::GridCell;
    use cartopolis_calls::video::I420;

    use crate::components::FreeCamera;
    use crate::platform::camera::Camera;
    use crate::platform::location::LocationWatch;
    use crate::platform::orientation::OrientationWatch;
    use crate::resources::{CameraState, MapAnchor, ScreenshotState};
    use crate::systems::geo_nav::{CameraPose, GeoNavState, Teleport};
    use crate::systems::icons;
    use crate::systems::theme;

    /// Metres of walking after which the world camera is teleported rather than
    /// moved: the map's streaming follows a teleport, not a nudge.
    const JUMP_M: f64 = 200.0;

    /// Frames to wait for the world capture before giving up on it.
    const CAPTURE_GIVE_UP_FRAMES: u16 = 300;

    /// The capture screen, while it is up.
    pub(super) struct Session {
        /// Where the picture goes: the agent's direct chat, as
        /// `(username, channel key)`, looked up every frame.
        send_to: Option<(String, String)>,
        front: bool,
        camera: Camera,
        location: LocationWatch,
        compass: OrientationWatch,
        /// The newest frame, upright and full size: what the shutter takes.
        frame: Option<I420>,
        texture: Option<egui::TextureHandle>,
        /// The world fills the screen and the photograph is the small one.
        world_big: bool,
        /// The world camera follows the sensors (a phone). Decided at opening,
        /// by whether there is a position provider at all; a desktop's camera
        /// is never moved, and its own view is the pose.
        moves_world: bool,
        /// The view to go back to when the screen closes.
        restore: Option<Teleport>,
        /// Where the world camera was last put, to re-place it only on a move.
        placed_at: Option<(f64, f64)>,
        /// The pose the sensors give now, for the chips and the shutter.
        live: Option<FieldPose>,
        shutter: Shutter,
        /// The picture once it is made, shown until it is sent or retaken.
        result: Option<Finished>,
        /// Flags from the screen for the drive system.
        press: bool,
        flip: bool,
        close: bool,
        send: bool,
        retake: bool,
        error: Option<String>,
    }

    /// A finished picture: its pixels, the pose it was taken at, and its
    /// texture for the result screen.
    struct Finished {
        rgba: Vec<u8>,
        size: [u32; 2],
        pose: FieldPose,
        texture: Option<egui::TextureHandle>,
    }

    enum Shutter {
        Idle,
        /// The world camera is held on `pose` for a few frames.
        Holding {
            photo: I420,
            pose: FieldPose,
            frames: u8,
        },
        /// Waiting for the world capture, for `waited` frames so far. `aerial`
        /// is where the world camera stood and looked, for the aerial panel.
        Capturing {
            photo: I420,
            pose: FieldPose,
            aerial: Option<aerial_reference::View>,
            waited: u16,
        },
        /// The picture is being put together on a thread.
        Composing {
            pose: FieldPose,
            /// Behind a mutex only because a resource must be `Sync` and a
            /// receiver is not; nothing else ever takes the lock.
            done: std::sync::Mutex<std::sync::mpsc::Receiver<(Vec<u8>, [u32; 2])>>,
        },
    }

    impl Session {
        fn busy(&self) -> bool {
            !matches!(self.shutter, Shutter::Idle)
        }

        fn composing(&self) -> bool {
            matches!(self.shutter, Shutter::Composing { .. })
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
    fn receiver() -> Option<fc::Receiver> {
        let status = crate::platform::gnss_receiver::status()?;
        let s = status.fresh_solution(std::time::Duration::from_secs(3))?;
        Some(fc::Receiver {
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

    /// The camera drives the world, the shutter takes both halves, and the
    /// finished picture goes to the chat.
    #[allow(clippy::too_many_arguments)]
    pub fn drive_field_check(
        mut check: ResMut<FieldCheck>,
        mut nav: ResMut<GeoNavState>,
        mut cs: ResMut<CameraState>,
        anchor: Res<MapAnchor>,
        terrain: Res<crate::systems::terrain::TerrainField>,
        globe: Res<crate::systems::globe::GlobeState>,
        mut camera: Query<(&GridCell, &mut Transform, &Projection), With<FreeCamera>>,
        windows: Query<&Window>,
        headless: Option<Res<crate::resources::HeadlessSurface>>,
        mut shot: ResMut<ScreenshotState>,
        mut chat: ResMut<crate::systems::chat::ChatState>,
    ) {
        let check = &mut *check;
        // Open on a request from Developer tools.
        if std::mem::take(&mut check.request) {
            if check.session.is_none() {
                let moves_world = LocationWatch::available();
                let restore = if moves_world {
                    camera.single().ok().and_then(|(cell, tf, _)| {
                        let geo = anchor.geo?;
                        let (lat, lng) = crate::systems::geo_nav::camera_geo(geo, &globe, cell, tf);
                        Some(Teleport {
                            lat,
                            lng,
                            altitude: Some(globe.altitude_m(cell, tf)),
                            pose: Some(CameraPose::capture(&cs)),
                            silent: true,
                        })
                    })
                } else {
                    None
                };
                check.session = Some(Session {
                    send_to: None,
                    front: false,
                    camera: Camera::open_still(false),
                    location: LocationWatch::start(),
                    compass: OrientationWatch::start(),
                    frame: None,
                    texture: None,
                    world_big: false,
                    moves_world,
                    restore,
                    placed_at: None,
                    live: None,
                    shutter: Shutter::Idle,
                    result: None,
                    press: false,
                    flip: false,
                    close: false,
                    send: false,
                    retake: false,
                    error: None,
                });
            }
        }
        let send_to = check.agent_chat(&chat);
        let Some(session) = check.session.as_mut() else {
            check.world_in = None;
            return;
        };
        session.send_to = send_to;

        // The result screen: send the picture, or go back to the camera.
        if std::mem::take(&mut session.retake) {
            session.result = None;
            session.error = None;
        }
        if std::mem::take(&mut session.send) {
            if let (Some(done), Some((peer, channel))) =
                (session.result.take(), session.send_to.clone())
            {
                chat.private_pictures
                    .push(crate::systems::chat_pictures::OutgoingPicture {
                        channel,
                        bytes: done.rgba,
                        caption: done.pose.caption(""),
                        rgba: Some(done.size),
                    });
                chat.notice = Some(crate::systems::chat::ChatNotice {
                    text: format!("Field check sent to {peer}."),
                    error: false,
                });
                session.close = true;
            }
        }

        // Close: put the view back, and the lens.
        if session.close && !session.busy() {
            if let Some(back) = session.restore.take() {
                nav.pending_teleport = Some(back);
            }
            cs.free_camera_roll = 0.0;
            cs.free_fov = None;
            check.session = None;
            check.world_in = None;
            return;
        }
        if session.flip && !session.busy() {
            session.flip = false;
            session.front = !session.front;
            session.camera = Camera::open_still(session.front);
            session.frame = None;
            session.texture = None;
        }
        if let Some(e) = session.camera.error() {
            session.error = Some(e);
        }
        if let Some(frame) = session.camera.take_preview() {
            session.frame = Some(frame);
        }

        let Some((win_w, win_h)) = window_px(&windows, headless.as_deref()) else {
            return;
        };
        let Ok((cell, mut tf, projection)) = camera.single_mut() else {
            return;
        };
        let geo = anchor.geo;

        // The pose now: the sensors on a phone, the app's own view elsewhere.
        let photo_size = session.frame.as_ref().map(|f| (f.width, f.height));
        let fov_y = photo_size.map(|(w, h)| photo_fov_y_deg(session.camera.lens().as_ref(), w, h));
        let fix = session.location.latest();
        let attitude = session.compass.latest();
        let sensors = fix.is_some() && attitude.is_some() && session.moves_world;
        let ground_ahn = |lat: f64, lng: f64| -> Option<f32> {
            let geo = geo?;
            terrain.is_ready().then(|| {
                let (wx, wz) = crate::systems::geo_nav::lat_lng_to_world(geo, lat, lng);
                terrain.datum_m + terrain.at(wx as f32, wz as f32)
            })
        };
        let live = if let (true, Some(fix), Some(attitude)) = (sensors, fix, attitude) {
            let (heading, pitch, roll) = attitude.view_angles(session.front);
            Some(FieldPose {
                lat: fix.lat,
                lng: fix.lng,
                height_m: fc::EYE_HEIGHT_M,
                heading_deg: heading as f32,
                pitch_deg: pitch as f32,
                roll_deg: roll as f32,
                fov_y_deg: fov_y.unwrap_or(super::FALLBACK_LONG_FOV_DEG),
                camera: if session.front {
                    fc::Facing::Front
                } else {
                    fc::Facing::Rear
                },
                taken_ms: now_ms(),
                source: fc::PoseSource::Sensors,
                accuracy_m: Some(fix.accuracy_m as f32),
                fix: crate::platform::gnss_receiver::fix_label().map(str::to_string),
                heading_accuracy_deg: attitude.accuracy_deg.map(|a| a as f32),
                ground_ahn_m: ground_ahn(fix.lat, fix.lng),
                receiver: receiver(),
                client: client_version(),
            })
        } else if !session.moves_world {
            geo.map(|geo| {
                let (lat, lng) = crate::systems::geo_nav::camera_geo(geo, &globe, cell, &tf);
                let (yaw, pitch, roll) = tf.rotation.to_euler(EulerRot::YXZ);
                let cam_fov = match projection {
                    Projection::Perspective(p) => p.fov.to_degrees(),
                    _ => 45.0,
                };
                FieldPose {
                    lat,
                    lng,
                    height_m: globe.altitude_m(cell, &tf),
                    heading_deg: (-yaw.to_degrees()).rem_euclid(360.0),
                    pitch_deg: pitch.to_degrees(),
                    roll_deg: roll.to_degrees(),
                    fov_y_deg: fc::cropped_fov_y_deg(win_w, win_h, cam_fov),
                    camera: fc::Facing::Rear,
                    taken_ms: now_ms(),
                    source: fc::PoseSource::View,
                    accuracy_m: None,
                    fix: None,
                    heading_accuracy_deg: None,
                    ground_ahn_m: ground_ahn(lat, lng),
                    receiver: None,
                    client: client_version(),
                }
            })
        } else {
            None
        };
        session.live = live.clone();

        // The world camera follows the pose on a phone; held while the shutter is.
        let held = match &session.shutter {
            Shutter::Holding { pose, .. } | Shutter::Capturing { pose, .. } => Some(pose.clone()),
            _ => None,
        };
        let drive = held.or_else(|| live.filter(|p| p.source == fc::PoseSource::Sensors));
        if let (Some(pose), Some(geo)) = (drive, geo) {
            // A jump only to arrive, or after a walk long enough that the map
            // should follow; every step in between moves the camera itself, so
            // a walk is not a stream of teleports.
            let jump = session.placed_at.is_none_or(|at| {
                crate::systems::geo_nav::great_circle_m(at, (pose.lat, pose.lng)) > JUMP_M
            });
            let yaw = pose.harness_yaw_deg().to_radians();
            let pitch = pose.pitch_deg.to_radians();
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
            cs.free_camera_rotation = Vec2::new(pitch, yaw);
            cs.free_camera_roll = pose.roll_deg.to_radians();
            cs.free_initialized = true;
            // Through `free_fov`: `camera_controls` resets the projection's lens
            // every frame, so a direct write would be undone before it is drawn.
            let want = fc::window_fov_y_deg(win_w, win_h, pose.fov_y_deg).to_radians();
            if cs.free_fov.is_none_or(|f| (f - want).abs() > 1e-4) {
                cs.free_fov = Some(want);
            }
            // On the fix, at eye height above the ground underfoot (not the
            // anchor's). Skipped on the frame a jump is pending: the teleport
            // puts the camera there itself.
            if !jump {
                let (wx, wz) = crate::systems::geo_nav::lat_lng_to_world(geo, pose.lat, pose.lng);
                let (wx, wz) = (wx as f32, wz as f32);
                let here = crate::grid::anchored_position(cell, tf.translation);
                let want = Vec3::new(wx, terrain.at(wx, wz) + pose.height_m as f32, wz);
                let d = want - here;
                if d.length_squared() > 1e-4 {
                    tf.translation += d;
                }
            }
        }

        // The shutter.
        let pressed = std::mem::take(&mut session.press);
        session.shutter = match std::mem::replace(&mut session.shutter, Shutter::Idle) {
            Shutter::Idle => match (pressed, session.frame.clone(), session.live.clone()) {
                (true, Some(photo), Some(pose)) => Shutter::Holding {
                    photo,
                    pose,
                    frames: HOLD_FRAMES,
                },
                (true, None, _) => {
                    session.error = Some("The camera has not delivered a picture yet.".into());
                    Shutter::Idle
                }
                (true, _, None) => {
                    session.error = Some("No position and heading yet.".into());
                    Shutter::Idle
                }
                _ => Shutter::Idle,
            },
            Shutter::Holding {
                photo,
                pose,
                frames,
            } => {
                if frames == 0 {
                    check.world_in = None;
                    shot.field_check_snapshot = true;
                    // Where the world camera stands and what it looks at, for
                    // the aerial panel: the ground it is aimed at, no farther
                    // than `AIM_MAX_M`, which a level view always is.
                    let aerial = geo
                        .filter(|_| crate::systems::aerial::covers(pose.lat, pose.lng))
                        .map(|anchor| {
                            let at = crate::grid::anchored_position(cell, tf.translation);
                            let fwd = tf.forward();
                            let flat = Vec2::new(fwd.x, fwd.z).normalize_or_zero();
                            let above = at.y - terrain.at(at.x, at.z);
                            let ahead = if fwd.y < -1e-3 && above > 0.0 {
                                (above / -fwd.y * Vec2::new(fwd.x, fwd.z).length()).min(AIM_MAX_M)
                            } else {
                                AIM_MAX_M
                            };
                            let aim = Vec2::new(at.x, at.z) + flat * ahead;
                            let ll = |x: f32, z: f32| {
                                crate::systems::geo_nav::world_to_lat_lng(
                                    anchor,
                                    f64::from(x),
                                    f64::from(z),
                                )
                            };
                            aerial_reference::View {
                                camera: ll(at.x, at.z),
                                aim: ll(aim.x, aim.y),
                                pose: None,
                            }
                        });
                    Shutter::Capturing {
                        photo,
                        pose,
                        aerial,
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
                aerial,
                waited,
            } => match check.world_in.take() {
                Some((world, [ww, wh])) => {
                    let (tx, rx) = std::sync::mpsc::channel();
                    let job_pose = pose.clone();
                    let spawned = std::thread::Builder::new()
                        .name("field-check".into())
                        .spawn(move || {
                            let rgba = photo.to_rgba();
                            let font = strip_font();
                            // The tiles are fetched here, off the frame; a
                            // failure costs the panel, not the picture.
                            let (aerial, note) = match aerial.map(aerial_reference::from_above) {
                                Some(Ok(img)) => (Some(img), ""),
                                Some(Err(e)) => {
                                    warn!("field check: no aerial photo: {e}");
                                    (None, "The aerial photo could not be fetched")
                                }
                                None => (None, "No aerial photo here"),
                            };
                            let out = compose(
                                (&rgba, photo.width, photo.height),
                                (&world, ww, wh),
                                aerial.as_ref(),
                                note,
                                &job_pose,
                                font.as_ref(),
                            );
                            let _ = tx.send(out);
                        });
                    match spawned {
                        Ok(_) => Shutter::Composing {
                            pose,
                            done: std::sync::Mutex::new(rx),
                        },
                        Err(e) => {
                            session.error =
                                Some(format!("Could not put the picture together: {e}"));
                            Shutter::Idle
                        }
                    }
                }
                // A capture that never comes back (the readback failed and was
                // only logged) must not leave the screen stuck on "Taking…".
                None if waited > CAPTURE_GIVE_UP_FRAMES => {
                    session.error = Some("The view could not be captured. Try again.".into());
                    Shutter::Idle
                }
                None => Shutter::Capturing {
                    photo,
                    pose,
                    aerial,
                    waited: waited + 1,
                },
            },
            Shutter::Composing { pose, done } => {
                let got = done
                    .lock()
                    .map_err(|_| std::sync::mpsc::TryRecvError::Disconnected)
                    .and_then(|rx| rx.try_recv());
                match got {
                    Ok((rgba, size)) => {
                        session.result = Some(Finished {
                            rgba,
                            size,
                            pose,
                            texture: None,
                        });
                        Shutter::Idle
                    }
                    Err(std::sync::mpsc::TryRecvError::Empty) => Shutter::Composing { pose, done },
                    Err(std::sync::mpsc::TryRecvError::Disconnected) => {
                        session.error = Some("The picture could not be put together.".into());
                        Shutter::Idle
                    }
                }
            }
        };
    }

    /// The capture screen: the camera (or the world) filling the screen, the
    /// other small in the corner, the pose's quality on chips, the shutter and
    /// the flip.
    pub fn render_field_check(mut egui: EguiMut, mut check: ResMut<FieldCheck>) {
        let agent = check
            .agents
            .first()
            .cloned()
            .unwrap_or_else(|| super::DEFAULT_WITH.into());
        let Some(session) = check.session.as_mut() else {
            return;
        };
        let ctx = egui.ctx_mut().clone();
        let viewport = egui.viewport_pts();
        let full = egui::Rect::from_min_size(egui::Pos2::ZERO, viewport);
        if session.result.is_some() {
            render_result(&ctx, full, session, &agent);
            return;
        }

        // The newest frame as a texture, small enough to upload every frame.
        if let Some(frame) = session.frame.as_ref() {
            let max_w = 720u32.min(frame.width);
            let h = ((frame.height * max_w / frame.width.max(1)) & !1).max(2);
            let small = if frame.width > max_w {
                frame.scaled(max_w, h)
            } else {
                frame.clone()
            };
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
                        Some(ctx.load_texture("field-check", image, egui::TextureOptions::LINEAR))
                }
            }
        }

        egui::Area::new(egui::Id::new("field_check_screen"))
            .order(egui::Order::Foreground)
            .fixed_pos(egui::Pos2::ZERO)
            .show(&ctx, |ui| {
                ui.set_min_size(viewport);
                let painter = ui.painter().clone();
                // A touch anywhere is the screen's, not the map's.
                ui.allocate_rect(full, egui::Sense::click_and_drag());
                let photo = session.texture.as_ref().map(|t| (t.id(), t.size_vec2()));
                if !session.world_big {
                    painter.rect_filled(full, 0.0, theme::INK);
                    if let Some((id, size)) = photo {
                        painter.image(id, full, cover_uv(size, full.size()), egui::Color32::WHITE);
                    }
                }

                // The small one, top right: the photograph over the world, or a
                // button that brings the world forward.
                let pip = egui::Rect::from_min_size(
                    egui::pos2(full.right() - 124.0, full.top() + 112.0),
                    egui::vec2(112.0, 150.0),
                );
                let pip_resp = ui.interact(pip, egui::Id::new("fc_pip"), egui::Sense::click());
                if session.world_big {
                    painter.rect_filled(pip, 12.0, theme::INK);
                    if let Some((id, size)) = photo {
                        painter.image(id, pip, cover_uv(size, pip.size()), egui::Color32::WHITE);
                    }
                    pip_label(&painter, pip, "Real");
                } else {
                    painter.rect_filled(pip, 12.0, egui::Color32::from_black_alpha(150));
                    painter.text(
                        pip.center() - egui::vec2(0.0, 10.0),
                        egui::Align2::CENTER_CENTER,
                        icons::LAYERS,
                        egui::FontId::proportional(26.0),
                        theme::CREAM,
                    );
                    painter.text(
                        pip.center() + egui::vec2(0.0, 22.0),
                        egui::Align2::CENTER_CENTER,
                        "Cartopolis",
                        egui::FontId::proportional(13.0),
                        theme::CREAM,
                    );
                }
                painter.rect_stroke(
                    pip,
                    12.0,
                    egui::Stroke::new(2.0, egui::Color32::WHITE),
                    egui::StrokeKind::Outside,
                );
                if pip_resp.clicked() {
                    session.world_big = !session.world_big;
                }

                // Top: close, where it goes, and how good the pose is.
                let close = egui::Rect::from_min_size(
                    egui::pos2(full.left() + 14.0, full.top() + 48.0),
                    egui::vec2(40.0, 40.0),
                );
                if round_button(ui, &painter, close, icons::CLEAR, false, "fc_close").clicked()
                    && !session.busy()
                {
                    session.close = true;
                }
                painter.text(
                    egui::pos2(full.center().x, full.top() + 58.0),
                    egui::Align2::CENTER_CENTER,
                    "Field check",
                    egui::FontId::proportional(16.0),
                    egui::Color32::WHITE,
                );
                painter.text(
                    egui::pos2(full.center().x, full.top() + 78.0),
                    egui::Align2::CENTER_CENTER,
                    "real · Cartopolis · aerial",
                    egui::FontId::proportional(12.5),
                    egui::Color32::WHITE,
                );
                let mut chip_x = full.left() + 14.0;
                for (text, tone) in chips(session) {
                    chip_x =
                        chip(&painter, egui::pos2(chip_x, full.top() + 98.0), &text, tone) + 8.0;
                }

                // Bottom: the shutter in the middle, the flip on the right.
                let cy = full.bottom() - 70.0;
                let shutter = egui::Rect::from_center_size(
                    egui::pos2(full.center().x, cy),
                    egui::vec2(76.0, 76.0),
                );
                let sresp = ui.interact(shutter, egui::Id::new("fc_shutter"), egui::Sense::click());
                painter.circle_stroke(
                    shutter.center(),
                    36.0,
                    egui::Stroke::new(4.0, egui::Color32::WHITE),
                );
                painter.circle_filled(
                    shutter.center(),
                    29.0,
                    if session.busy() {
                        theme::TEXT_DIM
                    } else {
                        egui::Color32::WHITE
                    },
                );
                if sresp.clicked() && !session.busy() {
                    session.error = None;
                    session.press = true;
                }
                let flip = egui::Rect::from_center_size(
                    egui::pos2(full.right() - 60.0, cy),
                    egui::vec2(48.0, 48.0),
                );
                if round_button(ui, &painter, flip, icons::SWAP, false, "fc_flip").clicked()
                    && !session.busy()
                {
                    session.flip = true;
                }
                painter.text(
                    flip.center_bottom() + egui::vec2(0.0, 12.0),
                    egui::Align2::CENTER_CENTER,
                    if session.front { "front" } else { "rear" },
                    egui::FontId::proportional(11.0),
                    egui::Color32::WHITE,
                );
                let status = if session.composing() {
                    Some("Fetching the aerial photo…".to_string())
                } else if session.busy() {
                    Some("Taking the picture…".to_string())
                } else {
                    session.error.clone()
                };
                if let Some(text) = status {
                    painter.text(
                        egui::pos2(full.center().x, cy - 66.0),
                        egui::Align2::CENTER_CENTER,
                        text,
                        egui::FontId::proportional(13.0),
                        egui::Color32::WHITE,
                    );
                }
            });
    }

    /// The picture once it is made: real, Cartopolis and aerial side by side,
    /// fitted to the screen, with Send and Retake under it.
    fn render_result(ctx: &egui::Context, full: egui::Rect, session: &mut Session, agent: &str) {
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
                ctx.load_texture("field-check-result", image, egui::TextureOptions::LINEAR)
            })
            .clone();
        let send_to = session.send_to.as_ref().map(|(peer, _)| peer.clone());
        egui::Area::new(egui::Id::new("field_check_result"))
            .order(egui::Order::Foreground)
            .fixed_pos(egui::Pos2::ZERO)
            .show(ctx, |ui| {
                ui.set_min_size(full.size());
                let painter = ui.painter().clone();
                ui.allocate_rect(full, egui::Sense::click_and_drag());
                painter.rect_filled(full, 0.0, theme::INK);

                let close = egui::Rect::from_min_size(
                    egui::pos2(full.left() + 14.0, full.top() + 48.0),
                    egui::vec2(40.0, 40.0),
                );
                if round_button(ui, &painter, close, icons::CLEAR, false, "fc_result_close")
                    .clicked()
                {
                    session.close = true;
                }
                painter.text(
                    egui::pos2(full.center().x, full.top() + 68.0),
                    egui::Align2::CENTER_CENTER,
                    "Field check",
                    egui::FontId::proportional(16.0),
                    egui::Color32::WHITE,
                );

                // The picture, as large as the screen allows.
                let room = egui::Rect::from_min_max(
                    egui::pos2(full.left() + 12.0, full.top() + 104.0),
                    egui::pos2(full.right() - 12.0, full.bottom() - 150.0),
                );
                let size = texture.size_vec2();
                let scale = (room.width() / size.x).min(room.height() / size.y);
                let shown = egui::Rect::from_center_size(room.center(), size * scale);
                painter.image(
                    texture.id(),
                    shown,
                    egui::Rect::from_min_max(egui::pos2(0.0, 0.0), egui::pos2(1.0, 1.0)),
                    egui::Color32::WHITE,
                );

                let row = egui::Rect::from_min_max(
                    egui::pos2(full.left() + 16.0, full.bottom() - 130.0),
                    egui::pos2(full.right() - 16.0, full.bottom() - 30.0),
                );
                ui.scope_builder(egui::UiBuilder::new().max_rect(row), |ui| {
                    ui.vertical_centered(|ui| {
                        match &send_to {
                            Some(peer) => {
                                if theme::primary_button_sized(
                                    ui,
                                    &format!("Send to {peer}"),
                                    200.0,
                                )
                                .clicked()
                                {
                                    session.send = true;
                                }
                            }
                            None => theme::note(
                                ui,
                                &format!("Start a chat with {agent} to send field checks."),
                            ),
                        }
                        ui.add_space(6.0);
                        if theme::action_button(ui, format!("{}  Retake", icons::CAMERA)).clicked()
                        {
                            session.retake = true;
                        }
                    });
                });
            });
    }

    #[derive(Clone, Copy)]
    enum Tone {
        Good,
        Warn,
        Plain,
    }

    fn chips(session: &Session) -> Vec<(String, Tone)> {
        let Some(pose) = session.live.as_ref() else {
            return vec![(
                if session.location.error().is_some() {
                    "No position".to_string()
                } else {
                    "Waiting for a position…".to_string()
                },
                Tone::Warn,
            )];
        };
        if pose.source == fc::PoseSource::View {
            return vec![("No sensors: the pose is this view".into(), Tone::Plain)];
        }
        let fix = pose.fix.clone().unwrap_or_else(|| "GPS".into());
        let acc = pose.accuracy_m.map_or(String::new(), |a| {
            if a < 1.0 {
                format!(" · {:.0} cm", (a * 100.0).max(1.0))
            } else {
                format!(" · {a:.1} m")
            }
        });
        let good = pose.fix.as_deref() == Some("RTK fixed");
        let heading = match pose.heading_accuracy_deg {
            Some(e) => format!("heading {:.0}° ± {e:.0}°", pose.heading_deg),
            None => format!("heading {:.0}°", pose.heading_deg),
        };
        vec![
            (
                format!("{fix}{acc}"),
                if good { Tone::Good } else { Tone::Warn },
            ),
            (heading, Tone::Warn),
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

    fn pip_label(painter: &egui::Painter, pip: egui::Rect, text: &str) {
        painter.text(
            pip.left_bottom() + egui::vec2(8.0, -10.0),
            egui::Align2::LEFT_CENTER,
            text,
            egui::FontId::proportional(11.0),
            egui::Color32::WHITE,
        );
    }

    fn round_button(
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
            egui::FontId::proportional(22.0),
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
pub fn drive_field_check() {}

#[cfg(target_arch = "wasm32")]
pub fn render_field_check() {}

#[cfg(test)]
mod tests {
    use super::*;
    use cartopolis_core::field_check::{Facing, PoseSource};

    fn pose() -> FieldPose {
        FieldPose {
            lat: 53.2183,
            lng: 6.5679,
            height_m: 1.5,
            heading_deg: 9.0,
            pitch_deg: 14.0,
            roll_deg: 0.0,
            fov_y_deg: 68.0,
            camera: Facing::Rear,
            taken_ms: 1_792_065_791_000,
            source: PoseSource::Sensors,
            accuracy_m: Some(0.012),
            fix: Some("RTK fixed".into()),
            heading_accuracy_deg: Some(3.0),
            ground_ahn_m: None,
            receiver: None,
            client: "test".into(),
        }
    }

    fn solid(w: u32, h: u32, rgb: [u8; 3]) -> Vec<u8> {
        (0..w * h)
            .flat_map(|_| [rgb[0], rgb[1], rgb[2], 255])
            .collect()
    }

    #[test]
    fn the_picture_is_the_photograph_the_world_and_the_aerial() {
        // A landscape webcam frame and a tall phone window are cropped to 3:4
        // and land in their own panel, at the same size; a wide aerial map is
        // fitted whole into the third.
        let photo = solid(640, 480, [200, 0, 0]);
        let world = solid(1080, 2400, [0, 0, 200]);
        let aerial = image::RgbImage::from_pixel(900, 400, image::Rgb([0, 200, 0]));
        let (rgba, [w, h]) = compose(
            (&photo, 640, 480),
            (&world, 1080, 2400),
            Some(&aerial),
            "",
            &pose(),
            None,
        );
        assert_eq!([w, h], [PANEL_W * 3, PANEL_H + STRIP_H]);
        assert!(w <= 1600, "a chat picture is re-encoded above 1600 px");
        assert_eq!(rgba.len(), (w * h * 4) as usize);
        let at = |x: u32, y: u32| {
            let i = ((y * w + x) * 4) as usize;
            [rgba[i], rgba[i + 1], rgba[i + 2]]
        };
        assert_eq!(at(PANEL_W / 2, PANEL_H / 2), [200, 0, 0]);
        assert_eq!(at(PANEL_W + PANEL_W / 2, PANEL_H / 2), [0, 0, 200]);
        assert_eq!(at(2 * PANEL_W + PANEL_W / 2, PANEL_H / 2), [0, 200, 0]);
        // Fitted, not cropped: above and below the wide map is the ink.
        assert_eq!(at(2 * PANEL_W + PANEL_W / 2, 10), [0x0A, 0x0E, 0x10]);
        assert_eq!(
            at(2 * PANEL_W + 2, PANEL_H / 2),
            [0, 200, 0],
            "the map's edge was cut"
        );
        // The strip is the dark ink, not any picture.
        assert_eq!(at(w / 2, PANEL_H + STRIP_H / 2), [0x0A, 0x0E, 0x10]);
    }

    #[test]
    fn without_an_aerial_its_panel_stays_dark() {
        let photo = solid(300, 400, [200, 200, 200]);
        let (rgba, [w, _]) = compose(
            (&photo, 300, 400),
            (&photo, 300, 400),
            None,
            "",
            &pose(),
            None,
        );
        let i = (((PANEL_H / 2) * w + 2 * PANEL_W + PANEL_W / 2) * 4) as usize;
        assert_eq!(&rgba[i..i + 3], &[0x0A, 0x0E, 0x10]);
    }

    #[test]
    fn the_strip_is_written_when_there_is_a_font() {
        let photo = solid(300, 400, [0, 0, 0]);
        let font = strip_font().expect("the brand face parses");
        let (rgba, [w, _]) = compose(
            (&photo, 300, 400),
            (&photo, 300, 400),
            None,
            "No aerial photo here",
            &pose(),
            Some(&font),
        );
        // Some pixel of the strip's first line is light.
        let lit = (PANEL_H + 30..PANEL_H + 56).any(|y| {
            (24..600).any(|x| {
                let i = ((y * w + x) * 4) as usize;
                rgba[i] > 128
            })
        });
        assert!(lit, "nothing was written in the strip");
    }

    #[test]
    fn a_phone_photographs_through_its_own_lens_or_a_usual_one() {
        // No lens data: the long side spans the usual 72°, and a 3:4 portrait
        // frame is already the picture's shape.
        assert!((photo_fov_y_deg(None, 1440, 1920) - 72.0).abs() < 0.01);
        // A sensor mounted sideways (90°): the upright vertical is the sensor's
        // horizontal.
        let lens = cartopolis_core::lens::Lens {
            focal_length_mm: Some(4.0),
            physical_size_mm: Some([6.4, 4.8]),
            orientation_deg: 90,
            ..Default::default()
        };
        let v = photo_fov_y_deg(Some(&lens), 1440, 1920);
        assert!((v - 77.32).abs() < 0.05, "{v}");
    }

    #[test]
    fn it_goes_to_the_first_listed_account_there_is_a_chat_with() {
        let mut chat = crate::systems::chat::ChatState::default();
        chat.direct.insert("viberfox".into(), "dm:abc".into());
        chat.direct.insert("sanne".into(), "dm:def".into());
        let check = |list: &str| FieldCheck {
            agents: parse_with(list),
            request: false,
            world_in: None,
            #[cfg(not(target_arch = "wasm32"))]
            session: None,
        };
        assert_eq!(
            check(" other , Viberfox ").agent_chat(&chat),
            Some(("viberfox".into(), "dm:abc".into()))
        );
        assert_eq!(check("other").agent_chat(&chat), None);
    }
}
