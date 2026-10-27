//! The user's own face, followed through the front camera, driving their
//! avatar's (`docs/notes/avatar-faces.md`).
//!
//! Something asks for it each frame it wants it — the camera's In world
//! selfie, a call that sends the avatar instead of the camera — through
//! [`FaceTracking::want`]; while anything does, the front camera is open and
//! every picture it takes is read by `cartopolis_face` **on the camera's own
//! thread**, which is otherwise idle (the picture goes nowhere else), so the
//! frame loop never waits on a model. The 52 weights it finds are shown on the
//! local avatar's face (`avatar_face::Face::track`) and, online, sent to the
//! people near (`SetFaceFrame`, at most `FACE_FRAME_HZ`). Only the weights
//! leave the phone; no picture does.
//!
//! The models are three `.tflite` files under `assets/models/face`, loaded
//! the first time a face is wanted. Not on the web, which has no camera here.

use std::sync::Arc;

use bevy::asset::io::Reader;
use bevy::asset::{AssetLoader, LoadContext};
use bevy::prelude::*;
use cartopolis_calls::video::I420;
use cartopolis_core::FACE_UNITS;

use crate::components::Avatar;
use crate::platform::camera::Camera;
use crate::platform::sync::Mutex;
use crate::systems::avatar_face::Face;

/// A model file, as bytes: `cartopolis_face` reads them itself.
#[derive(Asset, TypePath)]
pub(crate) struct FaceModel(pub Arc<Vec<u8>>);

#[derive(Default, TypePath)]
pub(crate) struct FaceModelLoader;

impl AssetLoader for FaceModelLoader {
    type Asset = FaceModel;
    type Settings = ();
    type Error = std::io::Error;

    async fn load(
        &self,
        reader: &mut dyn Reader,
        _settings: &(),
        _load_context: &mut LoadContext<'_>,
    ) -> Result<Self::Asset, Self::Error> {
        let mut bytes = Vec::new();
        reader.read_to_end(&mut bytes).await?;
        Ok(FaceModel(Arc::new(bytes)))
    }

    fn extensions(&self) -> &[&str] {
        &["tflite"]
    }
}

/// The three models, MediaPipe's Face Landmarker's.
const MODELS: [&str; 3] = [
    "models/face/face_detector.tflite",
    "models/face/face_landmarks_detector.tflite",
    "models/face/face_blendshapes.tflite",
];

/// What the camera thread found last, and when.
type Latest = Arc<Mutex<Option<([f32; FACE_UNITS], std::time::Instant)>>>;

/// Following the user's own face.
#[derive(Resource, Default)]
pub(crate) struct FaceTracking {
    /// Asked for this frame; cleared once read.
    wanted: bool,
    models: Option<[Handle<FaceModel>; 3]>,
    running: Option<Running>,
    /// Why it is not following a face, for whoever asked: the camera's
    /// error, or the models failing to load.
    pub(crate) problem: Option<String>,
    /// A face was seen in the last half second.
    pub(crate) seeing: bool,
}

struct Running {
    /// Held open while running; dropping it closes the camera.
    camera: Camera,
    latest: Latest,
}

impl FaceTracking {
    /// Follow the face this frame: call every frame it is wanted.
    pub(crate) fn want(&mut self) {
        self.wanted = true;
    }
}

/// A camera's picture as `cartopolis_face` reads it: RGB from the I420
/// planes, one pixel at a time, where the tracker samples.
struct I420Frame<'a>(&'a I420);

impl cartopolis_face::Frame for I420Frame<'_> {
    fn width(&self) -> u32 {
        self.0.width
    }
    fn height(&self) -> u32 {
        self.0.height
    }
    fn rgb(&self, x: u32, y: u32) -> [u8; 3] {
        let p = self.0;
        let cw = p.width.div_ceil(2) as usize;
        let (x, y) = (x as usize, y as usize);
        let yy = 298 * (i32::from(p.y[y * p.width as usize + x]) - 16);
        let c = (y / 2) * cw + x / 2;
        let u = i32::from(p.u[c]) - 128;
        let v = i32::from(p.v[c]) - 128;
        [
            ((yy + 409 * v + 128) >> 8).clamp(0, 255) as u8,
            ((yy - 100 * u - 208 * v + 128) >> 8).clamp(0, 255) as u8,
            ((yy + 516 * u + 128) >> 8).clamp(0, 255) as u8,
        ]
    }
}

/// Open or close the camera as the face is wanted, and start reading faces
/// on its thread once the models are in.
pub(crate) fn drive_face_tracking(
    mut tracking: ResMut<FaceTracking>,
    asset_server: Res<AssetServer>,
    models: Res<Assets<FaceModel>>,
) {
    let tracking = &mut *tracking;
    let wanted = std::mem::take(&mut tracking.wanted);
    if !wanted {
        if tracking.running.take().is_some() {
            tracing::info!("face tracking: camera closed");
        }
        tracking.seeing = false;
        return;
    }
    if let Some(run) = &tracking.running {
        if let Some(e) = run.camera.error() {
            tracking.problem = Some(e);
        }
        let fresh = run
            .latest
            .lock()
            .ok()
            .and_then(|l| l.map(|(_, at)| at.elapsed().as_secs_f32() < 0.5))
            .unwrap_or(false);
        tracking.seeing = fresh;
        return;
    }
    let handles = tracking
        .models
        .get_or_insert_with(|| MODELS.map(|path| asset_server.load(path)))
        .clone();
    let loaded: Vec<Arc<Vec<u8>>> = handles
        .iter()
        .filter_map(|h| models.get(h).map(|m| Arc::clone(&m.0)))
        .collect();
    if loaded.len() < 3 {
        if handles
            .iter()
            .any(|h| asset_server.load_state(h).is_failed())
        {
            tracking.problem = Some("The face models did not load".into());
        }
        return;
    }
    let latest: Latest = Arc::default();
    let camera = Camera::open();
    // The landmarker reads a 256-pixel square; a face across a third of a
    // 640 × 480 picture is about that, and a quarter-size one is not.
    camera.set_send_size((640, 480));
    let into = Arc::clone(&latest);
    // Built on the camera thread at its first picture; `None` inside once it
    // failed to, so it is not tried again every picture.
    let tracker: Mutex<Option<Option<cartopolis_face::FaceTracker>>> = Mutex::new(None);
    camera.set_sink(Box::new(move |picture: I420| {
        let Ok(mut tracker) = tracker.lock() else {
            return;
        };
        let built = tracker.get_or_insert_with(|| {
            cartopolis_face::FaceTracker::new(&loaded[0], &loaded[1], &loaded[2])
                .inspect_err(|e| tracing::warn!("face tracking: the models would not run: {e}"))
                .ok()
        });
        let Some(t) = built.as_mut() else {
            return;
        };
        if let Some(face) = t.track(&I420Frame(&picture)) {
            if let Ok(mut l) = into.lock() {
                *l = Some((face.weights, std::time::Instant::now()));
            }
        }
    }));
    tracing::info!("face tracking: camera open");
    tracking.problem = None;
    tracking.running = Some(Running { camera, latest });
}

/// Show what the camera sees on the local avatar's face, and send it to the
/// people near.
pub(crate) fn show_own_face(
    tracking: Res<FaceTracking>,
    online: Option<Res<crate::resources::OnlineSession>>,
    mut faces: Query<&mut Face, With<Avatar>>,
    mut last: Local<Option<std::time::Instant>>,
    mut sent_any: Local<bool>,
) {
    let found = tracking
        .running
        .as_ref()
        .and_then(|r| r.latest.lock().ok().and_then(|l| *l))
        .filter(|(_, at)| at.elapsed().as_secs_f32() < 0.5);
    let Some((weights, at)) = found else {
        // Tell the others once that the face stopped, so theirs eases back.
        if std::mem::take(&mut *sent_any) {
            if let Some(online) = &online {
                online.send(
                    "face frame",
                    cartopolis_core::NetMessage::SetFaceFrame { weights: vec![] },
                );
            }
        }
        return;
    };
    // Each new face once: the camera hands pictures over at its own pace,
    // `video::FPS`, which is `FACE_FRAME_HZ`.
    if *last == Some(at) {
        return;
    }
    *last = Some(at);
    for mut face in &mut faces {
        face.track(weights);
    }
    if let Some(online) = &online {
        let bytes = weights
            .iter()
            .map(|w| (w.clamp(0.0, 1.0) * 255.0).round() as u8)
            .collect();
        if online.send(
            "face frame",
            cartopolis_core::NetMessage::SetFaceFrame { weights: bytes },
        ) {
            *sent_any = true;
        }
    }
}

/// The camera's In world selfie wants the face: the avatar looks back at the
/// user, so it pulls the user's faces. The screen says so, since the camera
/// is on without showing its picture.
pub(crate) fn want_for_selfie(
    mut screen: ResMut<crate::systems::camera_screen::CameraScreen>,
    mut tracking: ResMut<FaceTracking>,
) {
    let note = screen.avatar_selfie().then(|| {
        tracking.want();
        if let Some(p) = &tracking.problem {
            p.clone()
        } else if tracking.seeing {
            "Your avatar makes your faces".to_string()
        } else {
            "Looking for your face…".to_string()
        }
    });
    if screen.face_note != note {
        screen.face_note = note;
    }
}
