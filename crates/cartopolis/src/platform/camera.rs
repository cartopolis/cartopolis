//! The camera, for calls (`systems::calls`): pictures as I420, delivered from
//! the camera's own thread ([`Camera::set_sink`]).
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
//! On the phone it is Camera2 through the NDK (the `phone` module below): the
//! front camera, asked for at runtime like the microphone.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use cartopolis_calls::video::{self, I420};

/// Where a camera delivers pictures as it takes them (see [`Camera::set_sink`]).
pub type Sink = Box<dyn Fn(I420) + Send>;

#[derive(Default)]
struct Shared {
    error: Mutex<Option<String>>,
    stop: AtomicBool,
    sink: Mutex<Option<Sink>>,
    last_sunk: Mutex<Option<std::time::Instant>>,
}

impl Shared {
    /// A new picture: handed to the sink at no more than the call's frame
    /// rate, and dropped otherwise — a call wants what the camera sees now.
    fn deliver(&self, picture: I420) {
        let period = std::time::Duration::from_micros(1_000_000 / u64::from(video::FPS));
        if let Ok(sink) = self.sink.lock() {
            if let Some(sink) = sink.as_ref() {
                let mut last = self.last_sunk.lock().unwrap();
                if last.is_none_or(|t| t.elapsed() >= period) {
                    *last = Some(std::time::Instant::now());
                    sink(picture.clone());
                }
            }
        }
    }
}

/// A running camera. Dropping it closes the device.
pub struct Camera {
    shared: Arc<Shared>,
}

impl Camera {
    /// Whether this build can open a real camera at all.
    #[must_use]
    pub const fn available() -> bool {
        cfg!(not(target_arch = "wasm32"))
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
        #[cfg(target_os = "android")]
        {
            let s = Arc::clone(&shared);
            if let Err(e) = std::thread::Builder::new()
                .name("cartopolis-camera".into())
                .spawn(move || phone::run(&s))
            {
                *shared.error.lock().unwrap() = Some(format!("Camera: {e}"));
            }
        }
        #[cfg(target_arch = "wasm32")]
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
                    s.deliver(video::test_pattern(n));
                    n += 1;
                    std::thread::sleep(period);
                }
            });
        if let Err(e) = started {
            *shared.error.lock().unwrap() = Some(format!("Camera: {e}"));
        }
        Self { shared }
    }

    /// Send every picture to `sink` as it is taken, at most at the call's
    /// frame rate, from the camera's own thread — so video does not wait on
    /// the app's frame loop.
    // The web build has no call session to sink into yet (calls there are
    // the browser's own WebRTC, not built), so nothing calls this on wasm.
    #[cfg_attr(target_arch = "wasm32", allow(dead_code))]
    pub fn set_sink(&self, sink: Sink) {
        *self.shared.sink.lock().unwrap() = Some(sink);
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
                shared.deliver(p);
            }
        }
        let _ = cam.stop_stream();
    }
}

/// The phone's camera through Camera2's C API (`libcamera2ndk`, API 24+), read
/// through an `AImageReader`: no Java, no `SurfaceTexture`, and pictures in
/// the same planar 4:2:0 a call encodes, so nothing is converted twice.
///
/// **The front camera, held upright.** A call is a face, so the front camera
/// is chosen when there is one. Its sensor is mounted sideways, and
/// `ACAMERA_SENSOR_ORIENTATION` says how far to turn a picture to stand it up
/// with the phone in its natural (portrait) orientation; this module turns it
/// that far and assumes the phone *is* upright. The picture is not mirrored:
/// that is how the other person should see you. Following the phone as it
/// turns needs the display rotation from Java, and is not done.
#[cfg(target_os = "android")]
mod phone {
    use super::{Shared, I420};
    use cartopolis_calls::video::Plane;
    use ndk_sys as sys;
    use std::ffi::{c_int, c_void, CStr};
    use std::ptr::null_mut;
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::Arc;
    use std::time::Duration;

    // ndk-sys declares Camera2 but, unlike its other libraries, does not link it.
    #[link(name = "camera2ndk")]
    extern "C" {}

    /// What the sensor is asked for; a call sends 320×240 (or 240×320), so
    /// this leaves room to crop and scale without asking the ISP for much.
    const ASK: (i32, i32) = (640, 480);

    /// Set by the device callbacks: the camera was taken away (another app,
    /// the system) or failed.
    static LOST: AtomicBool = AtomicBool::new(false);

    unsafe extern "C" fn on_disconnected(_: *mut c_void, _: *mut sys::ACameraDevice) {
        LOST.store(true, Ordering::Relaxed);
    }
    unsafe extern "C" fn on_error(_: *mut c_void, _: *mut sys::ACameraDevice, _: c_int) {
        LOST.store(true, Ordering::Relaxed);
    }
    unsafe extern "C" fn on_session(_: *mut c_void, _: *mut sys::ACameraCaptureSession) {}

    /// Everything Camera2 handed out, released in reverse order on drop.
    struct Held {
        manager: *mut sys::ACameraManager,
        device: *mut sys::ACameraDevice,
        reader: *mut sys::AImageReader,
        output: *mut sys::ACaptureSessionOutput,
        container: *mut sys::ACaptureSessionOutputContainer,
        target: *mut sys::ACameraOutputTarget,
        request: *mut sys::ACaptureRequest,
        session: *mut sys::ACameraCaptureSession,
    }

    impl Drop for Held {
        fn drop(&mut self) {
            // SAFETY: each pointer is either null or was returned by the
            // matching create call and has not been freed; the session is
            // closed (which stops the repeating request) before what it uses.
            unsafe {
                if !self.session.is_null() {
                    sys::ACameraCaptureSession_close(self.session);
                }
                if !self.request.is_null() {
                    sys::ACaptureRequest_free(self.request);
                }
                if !self.target.is_null() {
                    sys::ACameraOutputTarget_free(self.target);
                }
                if !self.container.is_null() {
                    sys::ACaptureSessionOutputContainer_free(self.container);
                }
                if !self.output.is_null() {
                    sys::ACaptureSessionOutput_free(self.output);
                }
                if !self.device.is_null() {
                    sys::ACameraDevice_close(self.device);
                }
                if !self.reader.is_null() {
                    sys::AImageReader_delete(self.reader);
                }
                if !self.manager.is_null() {
                    sys::ACameraManager_delete(self.manager);
                }
            }
        }
    }

    fn ok(status: sys::camera_status_t, what: &str) -> Result<(), String> {
        if status == sys::camera_status_t::ACAMERA_OK {
            Ok(())
        } else {
            Err(format!("Camera: {what} failed ({})", status.0))
        }
    }

    pub fn run(shared: &Arc<Shared>) {
        let fail = |msg: String| *shared.error.lock().unwrap() = Some(msg);
        match crate::platform::android::ensure_permission(
            crate::platform::android::PERM_CAMERA,
            &shared.stop,
        ) {
            Ok(true) => {}
            Ok(false) => {
                return fail(
                    "Camera access was refused. It can be allowed in the system \
                     settings for this app."
                        .into(),
                )
            }
            Err(e) => return fail(format!("Camera: {e}")),
        }
        LOST.store(false, Ordering::Relaxed);
        // SAFETY: `open` and `pump` follow Camera2's documented call order and
        // `Held` owns every handle until it is dropped here.
        let result = unsafe { open().and_then(|(held, turn)| pump(shared, &held, turn)) };
        if let Err(e) = result {
            fail(e);
        }
    }

    /// Pick a camera, open it, and start it streaming into an image reader.
    /// Returns the handles and how far to turn each picture.
    unsafe fn open() -> Result<(Held, u32), String> {
        let mut held = Held {
            manager: sys::ACameraManager_create(),
            device: null_mut(),
            reader: null_mut(),
            output: null_mut(),
            container: null_mut(),
            target: null_mut(),
            request: null_mut(),
            session: null_mut(),
        };
        if held.manager.is_null() {
            return Err("Camera: no camera service".into());
        }
        let (id, turn) = choose(held.manager)?;
        tracing::info!(camera = %id.to_string_lossy(), turn, "opening the camera");

        let format = sys::AIMAGE_FORMATS::AIMAGE_FORMAT_YUV_420_888.0 as i32;
        if sys::AImageReader_new(ASK.0, ASK.1, format, 3, &mut held.reader)
            != sys::media_status_t::AMEDIA_OK
        {
            return Err("Camera: could not make an image reader".into());
        }
        let mut window = null_mut();
        if sys::AImageReader_getWindow(held.reader, &mut window) != sys::media_status_t::AMEDIA_OK {
            return Err("Camera: the image reader has no window".into());
        }

        let mut callbacks = sys::ACameraDevice_StateCallbacks {
            context: null_mut(),
            onDisconnected: Some(on_disconnected),
            onError: Some(on_error),
        };
        ok(
            sys::ACameraManager_openCamera(
                held.manager,
                id.as_ptr(),
                &mut callbacks,
                &mut held.device,
            ),
            "opening the camera",
        )?;
        ok(
            sys::ACaptureSessionOutputContainer_create(&mut held.container),
            "making the outputs",
        )?;
        ok(
            sys::ACaptureSessionOutput_create(window, &mut held.output),
            "adding the output",
        )?;
        ok(
            sys::ACaptureSessionOutputContainer_add(held.container, held.output),
            "adding the output",
        )?;
        ok(
            sys::ACameraOutputTarget_create(window, &mut held.target),
            "making the target",
        )?;
        ok(
            sys::ACameraDevice_createCaptureRequest(
                held.device,
                sys::ACameraDevice_request_template::TEMPLATE_RECORD,
                &mut held.request,
            ),
            "making the request",
        )?;
        ok(
            sys::ACaptureRequest_addTarget(held.request, held.target),
            "aiming the request",
        )?;
        let session_callbacks = sys::ACameraCaptureSession_stateCallbacks {
            context: null_mut(),
            onClosed: Some(on_session),
            onReady: Some(on_session),
            onActive: Some(on_session),
        };
        ok(
            sys::ACameraDevice_createCaptureSession(
                held.device,
                held.container,
                &session_callbacks,
                &mut held.session,
            ),
            "starting the session",
        )?;
        let mut requests = [held.request];
        ok(
            sys::ACameraCaptureSession_setRepeatingRequest(
                held.session,
                null_mut(),
                1,
                requests.as_mut_ptr(),
                null_mut(),
            ),
            "starting the stream",
        )?;
        Ok((held, turn))
    }

    /// The front camera if there is one, else the first; and its sensor's
    /// orientation.
    unsafe fn choose(
        manager: *mut sys::ACameraManager,
    ) -> Result<(std::ffi::CString, u32), String> {
        let mut list = null_mut();
        ok(
            sys::ACameraManager_getCameraIdList(manager, &mut list),
            "listing cameras",
        )?;
        let ids: Vec<std::ffi::CString> = (0..(*list).numCameras as usize)
            .map(|i| CStr::from_ptr(*(*list).cameraIds.add(i)).to_owned())
            .collect();
        sys::ACameraManager_deleteCameraIdList(list);
        let mut best: Option<(std::ffi::CString, u32, bool)> = None;
        for id in ids {
            let mut meta = null_mut();
            if sys::ACameraManager_getCameraCharacteristics(manager, id.as_ptr(), &mut meta)
                != sys::camera_status_t::ACAMERA_OK
            {
                continue;
            }
            let mut entry = std::mem::zeroed::<sys::ACameraMetadata_const_entry>();
            let front = sys::ACameraMetadata_getConstEntry(
                meta,
                sys::acamera_metadata_tag::ACAMERA_LENS_FACING.0,
                &mut entry,
            ) == sys::camera_status_t::ACAMERA_OK
                && entry.count > 0
                && u32::from(*entry.data.u8_)
                    == sys::acamera_metadata_enum_acamera_lens_facing::ACAMERA_LENS_FACING_FRONT.0;
            let turn = if sys::ACameraMetadata_getConstEntry(
                meta,
                sys::acamera_metadata_tag::ACAMERA_SENSOR_ORIENTATION.0,
                &mut entry,
            ) == sys::camera_status_t::ACAMERA_OK
                && entry.count > 0
            {
                (*entry.data.i32_).rem_euclid(360) as u32
            } else {
                0
            };
            sys::ACameraMetadata_free(meta);
            if best.as_ref().is_none_or(|b| front && !b.2) {
                best = Some((id, turn, front));
            }
        }
        best.map(|(id, turn, _)| (id, turn))
            .ok_or_else(|| "No camera on this device".into())
    }

    /// Take pictures until told to stop or the camera goes away.
    unsafe fn pump(shared: &Arc<Shared>, held: &Held, turn: u32) -> Result<(), String> {
        while !shared.stop.load(Ordering::Relaxed) {
            if LOST.load(Ordering::Relaxed) {
                return Err("The camera was taken by another app".into());
            }
            let mut image = null_mut();
            if sys::AImageReader_acquireLatestImage(held.reader, &mut image)
                != sys::media_status_t::AMEDIA_OK
                || image.is_null()
            {
                std::thread::sleep(Duration::from_millis(10));
                continue;
            }
            let picture = read(image);
            sys::AImage_delete(image);
            if let Some(p) = picture {
                shared.deliver(p.rotated(turn, false));
            }
        }
        Ok(())
    }

    /// One `YUV_420_888` image as I420.
    unsafe fn read(image: *mut sys::AImage) -> Option<I420> {
        let (mut w, mut h) = (0, 0);
        sys::AImage_getWidth(image, &mut w);
        sys::AImage_getHeight(image, &mut h);
        let plane = |i: c_int| -> Option<Plane<'static>> {
            let (mut data, mut len, mut row, mut pixel) = (null_mut(), 0, 0, 0);
            let good = sys::AImage_getPlaneData(image, i, &mut data, &mut len)
                == sys::media_status_t::AMEDIA_OK
                && sys::AImage_getPlaneRowStride(image, i, &mut row)
                    == sys::media_status_t::AMEDIA_OK
                && sys::AImage_getPlanePixelStride(image, i, &mut pixel)
                    == sys::media_status_t::AMEDIA_OK
                && !data.is_null()
                && len > 0;
            // The slice lives until `AImage_delete`, which the caller does
            // only after `from_planes` has copied out of it.
            good.then(|| Plane {
                data: std::slice::from_raw_parts(data, len as usize),
                row_stride: row as usize,
                pixel_stride: pixel as usize,
            })
        };
        I420::from_planes(w as u32, h as u32, plane(0)?, plane(1)?, plane(2)?)
    }
}
