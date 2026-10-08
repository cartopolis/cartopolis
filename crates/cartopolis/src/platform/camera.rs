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

use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};

/// What a camera that Android took away reports: another app opened it, or the
/// app went to the background, which on Android revokes the camera. The
/// high-precision mode matches on it to restart rather than give up
/// (`platform::precise_sensors::Sensors::camera_lost`).
#[cfg_attr(not(target_os = "android"), allow(dead_code))]
pub const CAMERA_TAKEN: &str = "The camera was taken by another app";
use crate::platform::sync::{Arc, Mutex};

use cartopolis_calls::video::{self, I420};

/// Where a camera delivers pictures as it takes them (see [`Camera::set_sink`]).
pub type Sink = Box<dyn Fn(I420) + Send>;

#[derive(Default)]
struct Shared {
    error: Mutex<Option<String>>,
    stop: AtomicBool,
    sink: Mutex<Option<Sink>>,
    last_sunk: Mutex<Option<std::time::Instant>>,
    /// The size the call sends, landscape, packed `width << 32 | height`
    /// ([`Camera::set_send_size`]); 0 until told, which means the software
    /// encoder's size.
    send_size: AtomicU64,
    /// The newest picture, for the person's own view of themselves
    /// ([`Camera::take_preview`]); replaced, never queued.
    preview: Mutex<Option<I420>>,
}

impl Shared {
    /// Whether a picture taken now would be used: no more than the call's
    /// frame rate. Asked *before* a picture is converted, since a webcam runs
    /// at 30 fps and every other picture used to be converted, at full size,
    /// only to be dropped here.
    fn due(&self) -> bool {
        let period = std::time::Duration::from_micros(1_000_000 / u64::from(video::FPS));
        let mut last = self.last_sunk.lock().unwrap();
        if last.is_some_and(|t| t.elapsed() < period) {
            return false;
        }
        *last = Some(std::time::Instant::now());
        true
    }

    /// A camera picture at the call's send size, turned by `turn` degrees.
    fn fit(&self, picture: &I420, turn: u32) -> I420 {
        let v = self.send_size.load(Ordering::Relaxed);
        let size = if v == 0 {
            (video::WIDTH, video::HEIGHT)
        } else {
            ((v >> 32) as u32, v as u32)
        };
        to_size(picture, turn, size)
    }

    /// A new picture, already known to be [`Shared::due`]: handed to the sink,
    /// and kept as the preview.
    fn deliver(&self, picture: I420) {
        if let Ok(sink) = self.sink.lock() {
            if let Some(sink) = sink.as_ref() {
                sink(picture.clone());
            }
        }
        if let Ok(mut preview) = self.preview.lock() {
            *preview = Some(picture);
        }
    }
}

/// A running camera. Dropping it closes the device.
pub struct Camera {
    shared: Arc<Shared>,
}

impl Camera {
    /// Whether this build can open a real camera at all. (Not compiled for
    /// the web, where a call uses the browser's camera.)
    #[must_use]
    pub const fn available() -> bool {
        true
    }

    /// Open the default camera.
    #[must_use]
    pub fn open() -> Self {
        let shared = Arc::new(Shared::default());
        #[cfg(not(target_os = "android"))]
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
                    if s.due() {
                        s.deliver(video::test_pattern(n));
                    }
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
    pub fn set_sink(&self, sink: Sink) {
        *self.shared.sink.lock().unwrap() = Some(sink);
    }

    /// The size the call's encoder sends (landscape): pictures are handed over
    /// at it, so a hardware encoder gets the camera's full 640×480 and the
    /// software one a quarter of that, shrunk before anything else is done.
    pub fn set_send_size(&self, size: (u32, u32)) {
        let v = (u64::from(size.0) << 32) | u64::from(size.1);
        self.shared.send_size.store(v, Ordering::Relaxed);
    }

    /// The newest picture since the last call, for showing the person
    /// themselves: a call's self-view, the camera test in Settings.
    #[must_use]
    pub fn take_preview(&self) -> Option<I420> {
        self.shared.preview.lock().ok()?.take()
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
        // Let go of the call's sender now rather than whenever the camera
        // thread next looks at `stop` (a Camera2 open can take seconds).
        if let Ok(mut sink) = self.shared.sink.lock() {
            sink.take();
        }
    }
}

#[cfg(not(target_os = "android"))]
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
            if !shared.due() {
                continue;
            }
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
                shared.deliver(shared.fit(&p, 0));
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
        let result = unsafe {
            open(true).and_then(|(held, lens)| pump(shared, &held, lens.orientation_deg))
        };
        if let Err(e) = result {
            fail(e);
        }
    }

    /// The **back** camera as timestamped greyscale frames, for the
    /// high-precision mode (`platform::precise_sensors`): `on_lens` once
    /// with what the camera says about itself, then `on_frame(t_ns, width,
    /// height, luma)` for every frame until `stop`. Blocks; run it on a thread
    /// of its own.
    ///
    /// Greyscale because the localiser matches edges and tracks corners, and
    /// colour is three times the bytes for neither; the Y plane of
    /// `YUV_420_888` *is* the greyscale image, so this is a copy and not a
    /// conversion. Frames are not turned: the lens's own orientation is in the
    /// `Lens`, and turning would move the principal point the intrinsics name.
    ///
    /// Shares the device-lost flag with the call camera, so a call and a
    /// capture at once would each see the other's loss — two cameras open at
    /// once is not something either path supports.
    pub fn run_back_gray(
        stop: &AtomicBool,
        on_lens: impl FnOnce(cartopolis_core::lens::Lens),
        mut on_frame: impl FnMut(i64, u32, u32, Vec<u8>),
    ) -> Result<(), String> {
        match crate::platform::android::ensure_permission(
            crate::platform::android::PERM_CAMERA,
            stop,
        ) {
            Ok(true) => {}
            Ok(false) => {
                return Err("Camera access was refused. High precision needs the \
                            camera; it can be allowed in the system settings for \
                            this app."
                    .into())
            }
            Err(e) => return Err(format!("Camera: {e}")),
        }
        LOST.store(false, Ordering::Relaxed);
        // SAFETY: as `run`: Camera2's documented call order, every handle owned
        // by `Held` until it drops at the end of this block.
        unsafe {
            let (held, lens) = open(false)?;
            on_lens(lens);
            while !stop.load(Ordering::Relaxed) {
                if LOST.load(Ordering::Relaxed) {
                    return Err(super::CAMERA_TAKEN.into());
                }
                let mut image = null_mut();
                if sys::AImageReader_acquireLatestImage(held.reader, &mut image)
                    != sys::media_status_t::AMEDIA_OK
                    || image.is_null()
                {
                    std::thread::sleep(Duration::from_millis(5));
                    continue;
                }
                let mut t_ns = 0i64;
                sys::AImage_getTimestamp(image, &mut t_ns);
                let frame = read_luma(image);
                sys::AImage_delete(image);
                if let Some((w, h, luma)) = frame {
                    on_frame(t_ns, w, h, luma);
                }
            }
        }
        Ok(())
    }

    /// Plane 0 of a `YUV_420_888` image, tightly packed (the row stride
    /// dropped).
    unsafe fn read_luma(image: *mut sys::AImage) -> Option<(u32, u32, Vec<u8>)> {
        let (mut w, mut h) = (0, 0);
        sys::AImage_getWidth(image, &mut w);
        sys::AImage_getHeight(image, &mut h);
        let (mut data, mut len, mut row) = (null_mut(), 0, 0);
        let good = sys::AImage_getPlaneData(image, 0, &mut data, &mut len)
            == sys::media_status_t::AMEDIA_OK
            && sys::AImage_getPlaneRowStride(image, 0, &mut row) == sys::media_status_t::AMEDIA_OK
            && !data.is_null()
            && w > 0
            && h > 0;
        if !good {
            return None;
        }
        let (w, h, row) = (w as usize, h as usize, row as usize);
        let plane = std::slice::from_raw_parts(data, len as usize);
        let mut out = Vec::with_capacity(w * h);
        for y in 0..h {
            out.extend_from_slice(plane.get(y * row..y * row + w)?);
        }
        Some((w as u32, h as u32, out))
    }

    /// Pick a camera (the front one for a call, the back one for a capture),
    /// open it, and start it streaming into an image reader. Returns the
    /// handles and what the camera says about itself.
    unsafe fn open(front: bool) -> Result<(Held, cartopolis_core::lens::Lens), String> {
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
        let (id, lens) = choose(held.manager, front)?;
        tracing::info!(camera = %id.to_string_lossy(), ?lens, "opening the camera");

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
        Ok((held, lens))
    }

    /// The camera facing the wanted way if there is one, else the first; and
    /// what it reports about its lens.
    unsafe fn choose(
        manager: *mut sys::ACameraManager,
        want_front: bool,
    ) -> Result<(std::ffi::CString, cartopolis_core::lens::Lens), String> {
        let mut list = null_mut();
        ok(
            sys::ACameraManager_getCameraIdList(manager, &mut list),
            "listing cameras",
        )?;
        let ids: Vec<std::ffi::CString> = (0..(*list).numCameras as usize)
            .map(|i| CStr::from_ptr(*(*list).cameraIds.add(i)).to_owned())
            .collect();
        sys::ACameraManager_deleteCameraIdList(list);
        let mut best: Option<(std::ffi::CString, cartopolis_core::lens::Lens, bool)> = None;
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
            let lens = read_lens(meta);
            sys::ACameraMetadata_free(meta);
            let wanted = front == want_front;
            // The first camera facing the wanted way: on a phone with several
            // back cameras the first listed is the main one.
            if best.as_ref().is_none_or(|b| wanted && b.2 != want_front) {
                best = Some((id, lens, front));
            }
        }
        best.map(|(id, lens, _)| (id, lens))
            .ok_or_else(|| "No camera on this device".into())
    }

    /// What a camera's characteristics say about its lens, for the localiser.
    /// Every tag is optional on Android, and a missing one is `None` rather
    /// than a guess.
    unsafe fn read_lens(meta: *const sys::ACameraMetadata) -> cartopolis_core::lens::Lens {
        let entry = |tag: sys::acamera_metadata_tag| {
            let mut e = std::mem::zeroed::<sys::ACameraMetadata_const_entry>();
            (sys::ACameraMetadata_getConstEntry(meta, tag.0, &mut e)
                == sys::camera_status_t::ACAMERA_OK
                && e.count > 0)
                .then_some(e)
        };
        let floats = |tag, n: usize| {
            entry(tag)
                .filter(|e| e.count as usize >= n)
                .map(|e| std::slice::from_raw_parts(e.data.f, n).to_vec())
        };
        use sys::acamera_metadata_tag as Tag;
        cartopolis_core::lens::Lens {
            intrinsics: floats(Tag::ACAMERA_LENS_INTRINSIC_CALIBRATION, 5)
                .map(|v| [v[0], v[1], v[2], v[3], v[4]]),
            distortion: floats(Tag::ACAMERA_LENS_DISTORTION, 5)
                .map(|v| [v[0], v[1], v[2], v[3], v[4]]),
            pose_rotation: floats(Tag::ACAMERA_LENS_POSE_ROTATION, 4)
                .map(|v| [v[0], v[1], v[2], v[3]]),
            pose_translation: floats(Tag::ACAMERA_LENS_POSE_TRANSLATION, 3)
                .map(|v| [v[0], v[1], v[2]]),
            pose_reference: entry(Tag::ACAMERA_LENS_POSE_REFERENCE).map(|e| *e.data.u8_),
            active_array: entry(Tag::ACAMERA_SENSOR_INFO_ACTIVE_ARRAY_SIZE)
                .filter(|e| e.count >= 4)
                .map(|e| {
                    let v = std::slice::from_raw_parts(e.data.i32_, 4);
                    [v[0], v[1], v[2], v[3]]
                }),
            orientation_deg: entry(Tag::ACAMERA_SENSOR_ORIENTATION)
                .map_or(0, |e| (*e.data.i32_).rem_euclid(360) as u32),
            realtime_timestamps: entry(Tag::ACAMERA_SENSOR_INFO_TIMESTAMP_SOURCE).is_some_and(
                |e| {
                    u32::from(*e.data.u8_)
                        == sys::acamera_metadata_enum_acamera_sensor_info_timestamp_source::ACAMERA_SENSOR_INFO_TIMESTAMP_SOURCE_REALTIME.0
                },
            ),
        }
    }

    /// Take pictures until told to stop or the camera goes away.
    unsafe fn pump(shared: &Arc<Shared>, held: &Held, turn: u32) -> Result<(), String> {
        while !shared.stop.load(Ordering::Relaxed) {
            if LOST.load(Ordering::Relaxed) {
                return Err(super::CAMERA_TAKEN.into());
            }
            let mut image = null_mut();
            if sys::AImageReader_acquireLatestImage(held.reader, &mut image)
                != sys::media_status_t::AMEDIA_OK
                || image.is_null()
            {
                std::thread::sleep(Duration::from_millis(10));
                continue;
            }
            // Not wanted yet: give the image straight back unread.
            if !shared.due() {
                sys::AImage_delete(image);
                continue;
            }
            let picture = read(image);
            sys::AImage_delete(image);
            if let Some(p) = picture {
                shared.deliver(shared.fit(&p, turn));
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

#[cfg(target_os = "android")]
pub use phone::run_back_gray;

/// A picture of the person themselves as RGBA, at most `max_width` wide:
/// **mirrored**, as a mirror shows you. What is sent is not mirrored (the
/// other person should see you the right way round); only this view is.
#[must_use]
pub fn self_view(picture: &I420, max_width: u32) -> (u32, u32, Vec<u8>) {
    if picture.width > max_width {
        let h = (picture.height * max_width / picture.width.max(1)) & !1;
        let small = picture.scaled(max_width, h.max(2));
        (small.width, small.height, small.to_rgba_mirrored(true))
    } else {
        (
            picture.width,
            picture.height,
            picture.to_rgba_mirrored(true),
        )
    }
}

/// A camera's picture at `size` (landscape; the call's send size), turned
/// clockwise by `turn` degrees: shrunk *first*, so the turn is done on as few
/// pixels as possible (`video::oriented` is what the turned picture will be,
/// so the shrink aims at that size before the turn). The encoder then finds it
/// already the right size and copies nothing.
#[must_use]
pub fn to_size(picture: &I420, turn: u32, size: (u32, u32)) -> I420 {
    let quarter = turn % 180 == 90;
    let (w, h) = if quarter {
        (picture.height, picture.width)
    } else {
        (picture.width, picture.height)
    };
    let (cw, ch) = video::oriented(size, w, h);
    let (sw, sh) = if quarter { (ch, cw) } else { (cw, ch) };
    let small = picture.scaled(sw, sh);
    if turn % 360 == 0 {
        small
    } else {
        small.rotated(turn, false)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_camera_picture_is_shrunk_then_turned_to_the_calls_size() {
        const CALL: (u32, u32) = (video::WIDTH, video::HEIGHT);
        // With a hardware encoder the call sends the camera's own size: no
        // shrink at all, only the turn.
        let hw = to_size(
            &I420::black(640, 480),
            90,
            (video::HW_WIDTH, video::HW_HEIGHT),
        );
        assert_eq!((hw.width, hw.height), (video::HW_HEIGHT, video::HW_WIDTH));
        let landscape = I420::black(640, 480);
        let p = to_size(&landscape, 0, CALL);
        assert_eq!((p.width, p.height), (video::WIDTH, video::HEIGHT));
        // A phone's sensor is landscape and the phone upright: turned a
        // quarter, it is a portrait picture at the call's portrait size.
        let p = to_size(&landscape, 90, CALL);
        assert_eq!((p.width, p.height), (video::HEIGHT, video::WIDTH));
        let p = to_size(&landscape, 270, CALL);
        assert_eq!((p.width, p.height), (video::HEIGHT, video::WIDTH));
        // And what it is equals turning first and shrinking after.
        let src = video::test_pattern(7).scaled(640, 480);
        let want = src.rotated(90, false).scaled(video::HEIGHT, video::WIDTH);
        let got = to_size(&src, 90, CALL);
        assert_eq!((got.width, got.height), (want.width, want.height));
    }

    #[test]
    fn the_self_view_is_the_mirror_image() {
        let p = video::test_pattern(2);
        let (w, h, rgba) = self_view(&p, 320);
        assert_eq!((w, h), (p.width, p.height));
        assert_eq!(rgba, p.rotated(0, true).to_rgba());
    }
}
