//! The high-precision mode's sensors (`docs/notes/high-precision-mode.md`): the
//! back camera as greyscale frames, and the accelerometer and gyroscope at
//! 200 Hz, all stamped on one clock and handed to the mode as they arrive.
//!
//! **Nothing is stored.** The camera is there to pin the phone's position down
//! against the 3DBAG buildings; a frame is looked at and dropped. The mode takes
//! the newest frame ([`Sensors::take_frame`]) and every motion-sensor event since
//! it last asked ([`Sensors::drain_motion`]).
//!
//! **Threads, not frames.** The camera delivers ~30 frames a second and the
//! motion sensors 200 events a second each; none of that may wait on a Bevy
//! frame. So each has a thread and the app reads what they left.
//!
//! **One clock:** `CLOCK_BOOTTIME` nanoseconds ([`now_ns`]) — what
//! `ASensorEvent.timestamp` is, and what a frame's `AImage_getTimestamp` is when
//! the lens says its timestamp source is `REALTIME`
//! ([`cartopolis_core::lens::Lens::realtime_timestamps`]). Fusing a camera with
//! a gyroscope needs their stamps within a millisecond of one base.
//!
//! Android only. [`Sensors::available`] is `false` elsewhere, and [`Sensors`]
//! still compiles there so the systems that hold one need no `cfg`.

#![cfg_attr(not(target_os = "android"), allow(dead_code, unused_imports))]

use crate::platform::sync::{Arc, Mutex};
use std::collections::VecDeque;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};

use cartopolis_core::lens::Lens;

/// A camera frame: greyscale, as the sensor delivered it (not turned — the
/// lens's orientation says how it stands).
#[derive(Debug, Clone)]
pub struct Frame {
    pub t_ns: i64,
    pub width: u32,
    pub height: u32,
    pub luma: Vec<u8>,
}

impl Frame {
    /// Mean brightness, 0–255: what says a lens is covered or it is night.
    #[must_use]
    pub fn mean(&self) -> f32 {
        if self.luma.is_empty() {
            return 0.0;
        }
        self.luma.iter().map(|&v| u64::from(v)).sum::<u64>() as f32 / self.luma.len() as f32
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MotionKind {
    /// m/s², device axes, gravity included.
    Accel,
    /// rad/s, device axes.
    Gyro,
}

#[derive(Debug, Clone, Copy)]
pub struct Motion {
    pub t_ns: i64,
    pub kind: MotionKind,
    pub xyz: [f32; 3],
}

/// Motion events kept for a reader that has not come back: ten seconds of both
/// sensors at 200 Hz. A stalled reader loses the oldest, never the phone's memory.
const MAX_MOTION_KEPT: usize = 4_000;

#[derive(Default)]
struct Shared {
    error: Mutex<Option<String>>,
    /// Android took the camera away (another app, or the app went to the
    /// background) — the one failure worth starting again from.
    camera_lost: AtomicBool,
    stop: AtomicBool,
    lens: Mutex<Option<Lens>>,
    frame: Mutex<Option<Frame>>,
    motion: Mutex<VecDeque<Motion>>,
    frames: AtomicU64,
    accel: AtomicU64,
    gyro: AtomicU64,
}

impl Shared {
    fn fail(&self, why: String) {
        tracing::warn!(%why, "high-precision sensors");
        let mut e = self.error.lock().unwrap();
        if e.is_none() {
            *e = Some(why);
        }
    }

    fn push_motion(&self, m: Motion) {
        match m.kind {
            MotionKind::Accel => self.accel.fetch_add(1, Ordering::Relaxed),
            MotionKind::Gyro => self.gyro.fetch_add(1, Ordering::Relaxed),
        };
        let mut q = self.motion.lock().unwrap();
        if q.len() >= MAX_MOTION_KEPT {
            q.pop_front();
        }
        q.push_back(m);
    }
}

/// The sensors, running. Dropping it stops them.
pub struct Sensors {
    shared: Arc<Shared>,
}

/// Counts since the sensors started, for the mode's log.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Counts {
    pub frames: u64,
    pub accel: u64,
    pub gyro: u64,
}

/// Nanoseconds of `CLOCK_BOOTTIME`. Elsewhere, a monotonic clock from the first
/// call.
#[must_use]
pub fn now_ns() -> i64 {
    #[cfg(target_os = "android")]
    {
        let mut ts = libc::timespec {
            tv_sec: 0,
            tv_nsec: 0,
        };
        // SAFETY: `clock_gettime` writes one `timespec` it is handed.
        #[allow(unsafe_code)]
        unsafe {
            libc::clock_gettime(libc::CLOCK_BOOTTIME, &mut ts);
        }
        ts.tv_sec * 1_000_000_000 + ts.tv_nsec
    }
    #[cfg(not(target_os = "android"))]
    {
        static START: std::sync::OnceLock<web_time::Instant> = std::sync::OnceLock::new();
        START
            .get_or_init(web_time::Instant::now)
            .elapsed()
            .as_nanos() as i64
    }
}

/// The phone's maker and model, for the mode's log (`ro.product.manufacturer`,
/// `ro.product.model`).
#[must_use]
pub fn device_name() -> String {
    #[cfg(target_os = "android")]
    {
        let prop = |k| crate::platform::android::platform_property(k).unwrap_or_default();
        format!(
            "{} {}",
            prop("ro.product.manufacturer"),
            prop("ro.product.model")
        )
        .trim()
        .to_string()
    }
    #[cfg(not(target_os = "android"))]
    {
        String::new()
    }
}

impl Sensors {
    /// Whether this platform has them at all.
    #[must_use]
    pub const fn available() -> bool {
        cfg!(target_os = "android")
    }

    /// Start the camera and the motion sensors. Errors — a refused camera, a
    /// phone without a gyroscope — arrive in [`Self::error`], as everywhere in
    /// `platform`.
    pub fn start() -> Self {
        let sensors = Self {
            shared: Arc::new(Shared::default()),
        };
        #[cfg(target_os = "android")]
        sensors.start_threads();
        #[cfg(not(target_os = "android"))]
        sensors
            .shared
            .fail("high precision needs a phone's camera and motion sensors".into());
        sensors
    }

    #[cfg(target_os = "android")]
    fn start_threads(&self) {
        let shared = self.shared.clone();
        let camera = std::thread::Builder::new()
            .name("cartopolis-precise-camera".into())
            .spawn(move || {
                let result = crate::platform::camera::run_back_gray(
                    &shared.stop,
                    |lens| *shared.lens.lock().unwrap() = Some(lens),
                    |t_ns, width, height, luma| {
                        shared.frames.fetch_add(1, Ordering::Relaxed);
                        *shared.frame.lock().unwrap() = Some(Frame {
                            t_ns,
                            width,
                            height,
                            luma,
                        });
                    },
                );
                if let Err(e) = result {
                    if e == crate::platform::camera::CAMERA_TAKEN {
                        shared.camera_lost.store(true, Ordering::Relaxed);
                    }
                    shared.fail(e);
                }
            });
        if camera.is_err() {
            self.shared.fail("could not start the camera thread".into());
        }
        let shared = self.shared.clone();
        let motion = std::thread::Builder::new()
            .name("cartopolis-precise-motion".into())
            .spawn(move || {
                if let Err(e) = motion::run(&shared) {
                    shared.fail(e);
                }
            });
        if motion.is_err() {
            self.shared
                .fail("could not start the motion-sensor thread".into());
        }
    }

    #[must_use]
    pub fn error(&self) -> Option<String> {
        self.shared.error.lock().unwrap().clone()
    }

    /// Whether Android took the camera away — which it does to every app that
    /// goes to the background. A new [`Sensors`] gets it back; a refused
    /// permission or a missing gyroscope does not come back that way.
    #[must_use]
    pub fn camera_lost(&self) -> bool {
        self.shared.camera_lost.load(Ordering::Relaxed)
    }

    /// What the camera said about its lens, once it has opened.
    #[must_use]
    pub fn lens(&self) -> Option<Lens> {
        self.shared.lens.lock().unwrap().clone()
    }

    /// The newest frame, if one arrived since the last call.
    pub fn take_frame(&self) -> Option<Frame> {
        self.shared.frame.lock().unwrap().take()
    }

    /// Every motion event since the last call, oldest first.
    pub fn drain_motion(&self) -> Vec<Motion> {
        self.shared.motion.lock().unwrap().drain(..).collect()
    }

    #[must_use]
    pub fn counts(&self) -> Counts {
        Counts {
            frames: self.shared.frames.load(Ordering::Relaxed),
            accel: self.shared.accel.load(Ordering::Relaxed),
            gyro: self.shared.gyro.load(Ordering::Relaxed),
        }
    }
}

impl Drop for Sensors {
    fn drop(&mut self) {
        self.shared.stop.store(true, Ordering::Relaxed);
    }
}

/// The accelerometer and the gyroscope through the NDK's sensor manager, on a
/// looper of this thread's own — the compass's shape
/// (`platform::orientation`), except that **every** event is kept: a tracker
/// integrates them, so a dropped one is a gap in the motion, where the compass
/// only ever wanted the newest.
#[cfg(target_os = "android")]
mod motion {
    use super::{Motion, MotionKind, Shared};
    use std::sync::atomic::Ordering;

    /// 5 ms: 200 Hz, the most Android 12+ grants an app that does not declare
    /// `HIGH_SAMPLING_RATE_SENSORS`.
    const SAMPLE_PERIOD_US: i32 = 5_000;
    const POLL_MS: i32 = 50;
    const QUEUE_IDENT: i32 = 2;

    /// SAFETY, for the whole function: every handle comes from the call above
    /// its first use and is checked for null; the queue is destroyed before
    /// the function returns; the looper is this thread's own.
    #[allow(unsafe_code)]
    pub fn run(shared: &Shared) -> Result<(), String> {
        unsafe {
            let looper = ndk_sys::ALooper_prepare(
                ndk_sys::ALOOPER_PREPARE_ALLOW_NON_CALLBACKS as std::os::raw::c_int,
            );
            if looper.is_null() {
                return Err("could not prepare a looper for the motion sensors".into());
            }
            let manager = match crate::platform::android::package_name()
                .and_then(|n| std::ffi::CString::new(n).ok())
            {
                Some(c) => ndk_sys::ASensorManager_getInstanceForPackage(c.as_ptr()),
                None => ndk_sys::ASensorManager_getInstance(),
            };
            if manager.is_null() {
                return Err("no sensor manager".into());
            }
            let accel = ndk_sys::ASensorManager_getDefaultSensor(
                manager,
                ndk_sys::ASENSOR_TYPE_ACCELEROMETER as i32,
            );
            let gyro = ndk_sys::ASensorManager_getDefaultSensor(
                manager,
                ndk_sys::ASENSOR_TYPE_GYROSCOPE as i32,
            );
            if accel.is_null() || gyro.is_null() {
                return Err("this phone has no gyroscope or no accelerometer".into());
            }
            let queue = ndk_sys::ASensorManager_createEventQueue(
                manager,
                looper,
                QUEUE_IDENT,
                None,
                std::ptr::null_mut(),
            );
            if queue.is_null() {
                return Err("could not open a sensor queue".into());
            }
            for sensor in [accel, gyro] {
                ndk_sys::ASensorEventQueue_enableSensor(queue, sensor);
                ndk_sys::ASensorEventQueue_setEventRate(queue, sensor, SAMPLE_PERIOD_US);
            }
            let mut events = [std::mem::zeroed::<ndk_sys::ASensorEvent>(); 64];
            while !shared.stop.load(Ordering::Relaxed) {
                ndk_sys::ALooper_pollOnce(
                    POLL_MS,
                    std::ptr::null_mut(),
                    std::ptr::null_mut(),
                    std::ptr::null_mut(),
                );
                loop {
                    let n = ndk_sys::ASensorEventQueue_getEvents(
                        queue,
                        events.as_mut_ptr(),
                        events.len(),
                    );
                    if n <= 0 {
                        break;
                    }
                    #[allow(clippy::cast_sign_loss)]
                    for e in &events[..n as usize] {
                        let d = e.__bindgen_anon_1.__bindgen_anon_1.data;
                        let kind = if e.type_ == ndk_sys::ASENSOR_TYPE_GYROSCOPE as i32 {
                            MotionKind::Gyro
                        } else {
                            MotionKind::Accel
                        };
                        shared.push_motion(Motion {
                            t_ns: e.timestamp,
                            kind,
                            xyz: [d[0], d[1], d[2]],
                        });
                    }
                }
            }
            for sensor in [accel, gyro] {
                ndk_sys::ASensorEventQueue_disableSensor(queue, sensor);
            }
            ndk_sys::ASensorManager_destroyEventQueue(manager, queue);
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_stalled_reader_loses_the_oldest_motion_not_memory() {
        let shared = Shared::default();
        for i in 0..(MAX_MOTION_KEPT as i64 + 10) {
            shared.push_motion(Motion {
                t_ns: i,
                kind: MotionKind::Gyro,
                xyz: [0.0; 3],
            });
        }
        let q = shared.motion.lock().unwrap();
        assert_eq!(q.len(), MAX_MOTION_KEPT);
        assert_eq!(q.front().unwrap().t_ns, 10);
        assert_eq!(
            shared.gyro.load(Ordering::Relaxed),
            MAX_MOTION_KEPT as u64 + 10
        );
    }

    #[test]
    fn a_frame_knows_how_bright_it_is() {
        let f = Frame {
            t_ns: 0,
            width: 2,
            height: 1,
            luma: vec![0, 200],
        };
        assert_eq!(f.mean(), 100.0);
    }
}
