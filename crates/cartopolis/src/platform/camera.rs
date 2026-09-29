//! The camera, for calls (`systems::calls`): the newest picture, as I420.
//!
//! One API over a webcam on the desktop (`nokhwa`: V4L2, Media Foundation,
//! AVFoundation) and a stand-in anywhere: [`Camera::fake`] draws
//! `cartopolis_calls::video::test_pattern`, a moving picture that carries its
//! own frame number, which is what a headless test of a call has instead of a
//! camera (`CARTO_FAKE_CAMERA=1`). Everything after the camera — encoding,
//! sealing, the forwarder, decoding, display — is the real path either way.
//!
//! Like `platform::audio`, a camera that cannot be opened still yields a
//! `Camera`, whose [`Camera::error`] says why: one shape for the UI to draw.
//!
//! The phone has no camera here yet: Camera2 through the NDK is its own piece
//! of work, and until it exists an Android call receives video and sends none
//! (or the stand-in, for testing).

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use cartopolis_calls::video::{self, I420};

#[derive(Default)]
struct Shared {
    latest: Mutex<Option<I420>>,
    error: Mutex<Option<String>>,
    stop: AtomicBool,
}

/// A running camera. Dropping it closes the device.
pub struct Camera {
    shared: Arc<Shared>,
}

impl Camera {
    /// Whether this build can open a real camera at all.
    #[must_use]
    pub const fn available() -> bool {
        cfg!(all(not(target_arch = "wasm32"), not(target_os = "android")))
    }

    /// Open the default camera.
    #[must_use]
    pub fn open() -> Self {
        let shared = Arc::new(Shared::default());
        #[cfg(all(not(target_arch = "wasm32"), not(target_os = "android")))]
        {
            let s = Arc::clone(&shared);
            if let Err(e) = std::thread::Builder::new()
                .name("cartopolis-camera".into())
                .spawn(move || webcam::run(&s))
            {
                *shared.error.lock().unwrap() = Some(format!("Camera: {e}"));
            }
        }
        #[cfg(any(target_arch = "wasm32", target_os = "android"))]
        {
            *shared.error.lock().unwrap() = Some("No camera support on this device yet".into());
        }
        Self { shared }
    }

    /// The stand-in camera: the test pattern at the call's frame rate.
    #[must_use]
    pub fn fake() -> Self {
        let shared = Arc::new(Shared::default());
        let s = Arc::clone(&shared);
        let started = std::thread::Builder::new()
            .name("cartopolis-fake-camera".into())
            .spawn(move || {
                let period = std::time::Duration::from_micros(1_000_000 / u64::from(video::FPS));
                let mut n = 0u64;
                while !s.stop.load(Ordering::Relaxed) {
                    *s.latest.lock().unwrap() = Some(video::test_pattern(n));
                    n += 1;
                    std::thread::sleep(period);
                }
            });
        if let Err(e) = started {
            *shared.error.lock().unwrap() = Some(format!("Camera: {e}"));
        }
        Self { shared }
    }

    /// The newest picture since last asked, if there is one. Older ones are
    /// gone: a call wants what the camera sees now, not a queue.
    #[must_use]
    pub fn take(&self) -> Option<I420> {
        self.shared.latest.lock().ok()?.take()
    }

    /// Why the camera is not working, if it is not.
    #[must_use]
    pub fn error(&self) -> Option<String> {
        self.shared.error.lock().ok()?.clone()
    }
}

impl Drop for Camera {
    fn drop(&mut self) {
        self.shared.stop.store(true, Ordering::Relaxed);
    }
}

#[cfg(all(not(target_arch = "wasm32"), not(target_os = "android")))]
mod webcam {
    use super::{Shared, I420};
    use nokhwa::pixel_format::YuyvFormat;
    use nokhwa::utils::{
        CameraFormat, CameraIndex, FrameFormat, RequestedFormat, RequestedFormatType, Resolution,
    };
    use std::sync::atomic::Ordering;
    use std::sync::Arc;

    /// Ask for 640×480 YUYV — uncompressed, so nothing has to decode JPEG,
    /// and offered by nearly every webcam. The call scales it down.
    pub fn run(shared: &Arc<Shared>) {
        let fail = |msg: String| *shared.error.lock().unwrap() = Some(msg);
        let want = RequestedFormat::new::<YuyvFormat>(RequestedFormatType::Closest(
            CameraFormat::new(Resolution::new(640, 480), FrameFormat::YUYV, 30),
        ));
        let mut cam = match nokhwa::Camera::new(CameraIndex::Index(0), want) {
            Ok(c) => c,
            Err(e) => return fail(format!("No camera: {e}")),
        };
        if let Err(e) = cam.open_stream() {
            return fail(format!("Camera would not start: {e}"));
        }
        while !shared.stop.load(Ordering::Relaxed) {
            let frame = match cam.frame() {
                Ok(f) => f,
                Err(e) => return fail(format!("Camera stopped: {e}")),
            };
            let r = frame.resolution();
            let picture = match frame.source_frame_format() {
                FrameFormat::YUYV => I420::from_yuyv(r.width(), r.height(), frame.buffer()),
                FrameFormat::NV12 => I420::from_nv12(r.width(), r.height(), frame.buffer()),
                other => {
                    return fail(format!(
                        "The camera only offers {other:?}, which this app cannot read yet"
                    ))
                }
            };
            if let Some(p) = picture {
                *shared.latest.lock().unwrap() = Some(p);
            }
        }
        let _ = cam.stop_stream();
    }
}
