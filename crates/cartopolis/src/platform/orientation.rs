//! Which way the *device* is pointing — the compass seam, beside
//! [`crate::platform::location`] and shaped exactly like it.
//!
//! Position and heading are two different sensors with two different provider
//! APIs on every platform, so they are two watches rather than one: a viewer that
//! wants only the compass (an indoor look-around) should not light the GNSS
//! radio, and a drive that wants only the position should not wake the
//! magnetometer. [`crate::systems::locate`] happens to start both, which is not a
//! reason to fuse them here.
//!
//! **The number this hands out is the bearing of the top of the *screen*, not of
//! the device.** Every provider reports the device's own frame, and the user
//! turns a *picture*: on a phone held in landscape the two differ by 90°, and a
//! compass that ignores it points sideways in exactly the orientation people
//! navigate in. The correction is [`screen_up_bearing`], and it is applied inside
//! each provider so the value crossing this seam needs no further interpretation.
//!
//! Three arms, the same three as `location`:
//!
//! - **wasm** — `deviceorientationabsolute`, falling back to `deviceorientation`
//!   for Safari, which has no absolute event but does carry a
//!   `webkitCompassHeading`. iOS additionally gates the whole event behind
//!   `DeviceOrientationEvent.requestPermission()`, which must be called under a
//!   user gesture; [`OrientationWatch::start`] is reached from the tap on the
//!   locate button, which is inside Safari's transient-activation window.
//! - **Android** — the NDK's `ASensorManager` on a dedicated thread with its own
//!   `ALooper`. Deliberately *not* JNI: `SensorEventListener` is a Java interface
//!   and implementing one needs a class, which this APK cannot ship
//!   (`crate::platform::android`, `docs/notes/android-build.md`). The C sensor API
//!   needs no `classes.dex` at all, which is the only reason a compass is
//!   reachable here.
//! - **desktop** — nothing, and [`OrientationWatch::available`] says so. A box
//!   with no magnetometer has no heading, and inventing one would point the view
//!   somewhere arbitrary and call it north.
//!
//! **None of the two device arms has run on hardware from this container** — see
//! `docs/notes/device-orientation.md` for what a phone still has to confirm.

use std::sync::{Arc, Mutex};

/// One heading report, normalised across providers: the compass bearing of the
/// top of the screen, degrees clockwise from north.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct DeviceHeading {
    /// Bearing of screen-up, in `[0, 360)`, clockwise from north.
    pub bearing_deg: f64,
    /// Reported accuracy in degrees, when the platform states one. `None` is the
    /// ordinary case on the web, where no provider reports it.
    pub accuracy_deg: Option<f64>,
}

/// The slot a provider writes into. Same shape and same reason as
/// [`crate::platform::location::Slot`]: an `Err` is how a denied permission or a
/// missing sensor reaches the UI instead of an eternal "waiting".
pub(crate) type Slot = Arc<Mutex<Option<Result<DeviceHeading, String>>>>;

/// A live subscription to the device's heading. Drop it to unsubscribe.
pub struct OrientationWatch {
    slot: Slot,
    // One field, two mutually exclusive cfgs — see `LocationWatch` for why the
    // name is shared rather than the struct split.
    #[cfg(target_arch = "wasm32")]
    inner: Option<wasm::Watch>,
    #[cfg(target_os = "android")]
    inner: Option<android::Subscription>,
}

impl OrientationWatch {
    /// Whether this build has a provider that can produce a heading at all.
    #[must_use]
    pub const fn available() -> bool {
        cfg!(any(target_arch = "wasm32", target_os = "android"))
    }

    /// Begin watching. Cheap and non-blocking; the first heading arrives later
    /// (a magnetometer needs a sample, and iOS may prompt first).
    #[must_use]
    pub fn start() -> Self {
        let slot: Slot = Arc::new(Mutex::new(None));
        #[cfg(target_arch = "wasm32")]
        {
            let inner = wasm::start(slot.clone());
            Self { slot, inner }
        }
        #[cfg(target_os = "android")]
        {
            let inner = android::start(slot.clone());
            Self { slot, inner }
        }
        #[cfg(not(any(target_arch = "wasm32", target_os = "android")))]
        {
            tracing::info!("no compass in this build — the view follows the avatar instead");
            Self { slot }
        }
    }

    /// The most recent heading, or `None` while nothing has arrived (or the
    /// provider reported an error — see [`Self::error`]).
    #[must_use]
    pub fn latest(&self) -> Option<DeviceHeading> {
        self.slot.lock().ok()?.as_ref()?.as_ref().ok().copied()
    }

    /// The most recent provider error, if the last report was one.
    #[must_use]
    pub fn error(&self) -> Option<String> {
        self.slot.lock().ok()?.as_ref()?.as_ref().err().cloned()
    }
}

impl Drop for OrientationWatch {
    fn drop(&mut self) {
        // Explicit on both arms, for the same reason `LocationWatch` is: a field
        // whose only job is to be dropped reads as unused under `unused = "deny"`.
        #[cfg(target_arch = "wasm32")]
        if let Some(inner) = self.inner.take() {
            wasm::stop(inner);
        }
        #[cfg(target_os = "android")]
        if let Some(inner) = self.inner.take() {
            inner.stop();
        }
    }
}

// ─── The two pieces of maths, shared by both providers ───────────────────────

/// Compass bearing of the device's own **top edge**, from an Android rotation
/// vector (a unit quaternion taking device coordinates into the world frame
/// East/North/Up).
///
/// This is `SensorManager.getOrientation`'s azimuth written out: that function
/// builds the rotation matrix and returns `atan2(R01, R11)`, which is the angle
/// between the device's `+y` axis (its top edge) and north. Only those two matrix
/// entries are needed, so the matrix is not built.
///
/// The web arm does not use this — the browser has already reduced the same
/// fusion to a single `alpha`.
// `allow`: `platform` is a private module, so a function only the Android arm
// calls is dead code on every other target. It stays compiled and tested
// everywhere on purpose — this is the piece of the compass that can be wrong in a
// way a phone would not obviously show, and a desktop `cargo test` is the only
// place it gets checked.
#[allow(dead_code)]
#[must_use]
pub fn bearing_from_rotation_vector(x: f64, y: f64, z: f64, w: f64) -> f64 {
    // R01 and R11 of the standard quaternion-to-matrix expansion.
    let r01 = 2.0 * z.mul_add(-w, x * y);
    let r11 = 1.0 - 2.0 * z.mul_add(z, x * x);
    r01.atan2(r11).to_degrees().rem_euclid(360.0)
}

/// Turn the bearing of the device's top edge into the bearing of the top of the
/// *screen*, given the display's rotation.
///
/// `screen_angle_deg` is the angle the content is rotated **clockwise** within
/// the device frame to stay upright — `screen.orientation.angle` on the web and
/// `Display.getRotation() × 90` on Android, which are the same quantity.
///
/// The sign is the thing worth checking, so here is the case that pins it. Hold a
/// phone in portrait with its top pointing north (device bearing 0). Rotate the
/// phone 90° anticlockwise: its top now points west (bearing 270), and the system
/// rotates the content 90° clockwise to keep it upright, so `screen.orientation.
/// angle` reads 90. The picture the user is looking at has not moved — screen-up
/// is still north. `270 + 90 = 360 ≡ 0`. Adding is right; subtracting would have
/// read 180, i.e. exactly backwards, and only in landscape.
// `allow`: see `bearing_from_rotation_vector` — both device arms call this and
// no desktop one does.
#[allow(dead_code)]
#[must_use]
pub fn screen_up_bearing(device_top_bearing_deg: f64, screen_angle_deg: f64) -> f64 {
    (device_top_bearing_deg + screen_angle_deg).rem_euclid(360.0)
}

// ─── Web ─────────────────────────────────────────────────────────────────────

#[cfg(target_arch = "wasm32")]
mod wasm {
    use super::{screen_up_bearing, DeviceHeading, Slot};
    use std::cell::RefCell;
    use wasm_bindgen::prelude::Closure;
    use wasm_bindgen::{JsCast, JsValue};

    /// A live pair of listeners, identified by the id we assigned them.
    ///
    /// Holds an `i32` and nothing else, for exactly the reason
    /// [`crate::platform::location`]'s `Watch` does: a `Closure` is `!Send`, this
    /// travels inside a Bevy `Resource`, and storing one here would make that
    /// resource un-`Resource`-able a long way from the cause.
    pub struct Watch {
        id: i32,
    }

    thread_local! {
        static WATCHES: RefCell<Vec<(i32, Registered)>> = const { RefCell::new(Vec::new()) };
        static NEXT_ID: RefCell<i32> = const { RefCell::new(1) };
    }

    type Listener = Closure<dyn FnMut(web_sys::DeviceOrientationEvent)>;

    /// The two listeners and the event names they were added under, so `stop` can
    /// remove exactly what `start` added.
    struct Registered {
        absolute: Listener,
        legacy: Listener,
    }

    /// `screen.orientation.angle`, in degrees clockwise — see
    /// [`super::screen_up_bearing`].
    ///
    /// Read through `Reflect` rather than through `web_sys::ScreenOrientation` so
    /// the legacy `window.orientation` fallback (Safari before 16.4, which is a
    /// live phone population) costs nothing extra: it is the same untyped lookup
    /// against a different property. It also keeps two web-sys features out of
    /// `Cargo.toml` for one integer.
    fn screen_angle() -> f64 {
        let Some(window) = web_sys::window() else {
            return 0.0;
        };
        let angle = js_sys::Reflect::get(&window, &JsValue::from_str("screen"))
            .and_then(|s| js_sys::Reflect::get(&s, &JsValue::from_str("orientation")))
            .and_then(|o| js_sys::Reflect::get(&o, &JsValue::from_str("angle")))
            .ok()
            .and_then(|v| v.as_f64());
        if let Some(a) = angle {
            return a;
        }
        // Legacy `window.orientation`: the same quantity, but reported in
        // (−90, 0, 90, 180) with the *opposite* sign convention for landscape —
        // it is how far the **device** was turned, not how far the content was.
        js_sys::Reflect::get(&window, &JsValue::from_str("orientation"))
            .ok()
            .and_then(|v| v.as_f64())
            .map_or(0.0, |o| (-o).rem_euclid(360.0))
    }

    /// Reduce one event to a screen-up bearing, or `None` if it carries no
    /// absolute heading.
    ///
    /// Two sources, in preference order. `webkitCompassHeading` is Safari's, and
    /// it is *already* a compass bearing of the device top (and already corrected
    /// to true north), so it needs no `360 −`. `alpha` is the standard's, and it
    /// is measured anticlockwise from north, hence the subtraction. A plain
    /// `deviceorientation` event with neither is **relative** — Chrome's alpha
    /// there starts wherever the page was loaded — and reporting it as a compass
    /// would silently point the map at an arbitrary bearing, so it is dropped.
    fn heading_of(event: &web_sys::DeviceOrientationEvent) -> Option<DeviceHeading> {
        let webkit = js_sys::Reflect::get(event, &JsValue::from_str("webkitCompassHeading"))
            .ok()
            .and_then(|v| v.as_f64())
            .filter(|v| v.is_finite());
        let device_top = match webkit {
            Some(h) => h,
            None => {
                if !event.absolute() {
                    return None;
                }
                let alpha = event.alpha()?;
                if !alpha.is_finite() {
                    return None;
                }
                360.0 - alpha
            }
        };
        let accuracy_deg = js_sys::Reflect::get(event, &JsValue::from_str("webkitCompassAccuracy"))
            .ok()
            .and_then(|v| v.as_f64())
            // Safari reports −1 for "no fix yet", which is not an accuracy.
            .filter(|v| v.is_finite() && *v >= 0.0);
        Some(DeviceHeading {
            bearing_deg: screen_up_bearing(device_top, screen_angle()),
            accuracy_deg,
        })
    }

    pub fn start(slot: Slot) -> Option<Watch> {
        let window = web_sys::window()?;

        // iOS gates the event behind an explicit grant, and the call must happen
        // under a transient activation. This is reached from the frame in which
        // the locate button was tapped, which is inside Safari's five-second
        // window; a `start` triggered any other way (a restored session, a
        // script) will be rejected and land in the slot as an error, which is the
        // honest outcome — the alternative is a compass that never reports and
        // never says why. Everything else has no `requestPermission` at all and
        // this whole block is skipped.
        request_ios_permission(slot.clone());

        let make = |sink: Slot| {
            Closure::<dyn FnMut(web_sys::DeviceOrientationEvent)>::new(
                move |e: web_sys::DeviceOrientationEvent| {
                    if let Some(h) = heading_of(&e) {
                        if let Ok(mut g) = sink.lock() {
                            *g = Some(Ok(h));
                        }
                    }
                },
            )
        };
        let absolute = make(slot.clone());
        let legacy = make(slot);

        // Both, always. `deviceorientationabsolute` is what Chrome/Android fires
        // and is the only absolute source there; `deviceorientation` is the only
        // event Safari has, and its `webkitCompassHeading` is absolute. Listening
        // to only one leaves out half the phones, and listening to both is safe
        // because `heading_of` drops a relative report rather than believing it.
        let target: &web_sys::EventTarget = window.as_ref();
        let _ = target.add_event_listener_with_callback(
            "deviceorientationabsolute",
            absolute.as_ref().unchecked_ref(),
        );
        let _ = target
            .add_event_listener_with_callback("deviceorientation", legacy.as_ref().unchecked_ref());

        let id = NEXT_ID.with(|n| {
            let mut n = n.borrow_mut();
            *n += 1;
            *n
        });
        WATCHES.with(|w| w.borrow_mut().push((id, Registered { absolute, legacy })));
        Some(Watch { id })
    }

    /// Ask iOS for the sensor, if this browser is one that asks.
    ///
    /// `DeviceOrientationEvent.requestPermission` exists only on Safari. The
    /// promise is spawned rather than awaited — `start` is called from a Bevy
    /// system and cannot block — so the grant lands a frame or two later and the
    /// listeners registered below simply begin firing when it does.
    fn request_ios_permission(slot: Slot) {
        let Some(ctor) = web_sys::window().and_then(|w| {
            js_sys::Reflect::get(&w, &JsValue::from_str("DeviceOrientationEvent")).ok()
        }) else {
            return;
        };
        let Ok(request) = js_sys::Reflect::get(&ctor, &JsValue::from_str("requestPermission"))
        else {
            return;
        };
        let Ok(request) = request.dyn_into::<js_sys::Function>() else {
            return;
        };
        let Ok(promise) = request.call0(&ctor) else {
            report(&slot, Err("Compass permission was refused".to_string()));
            return;
        };
        let Ok(promise) = promise.dyn_into::<js_sys::Promise>() else {
            return;
        };
        wasm_bindgen_futures::spawn_local(async move {
            match wasm_bindgen_futures::JsFuture::from(promise).await {
                Ok(v) if v.as_string().as_deref() == Some("granted") => {}
                Ok(_) => report(&slot, Err("Compass permission denied".to_string())),
                Err(_) => report(&slot, Err("Compass permission was refused".to_string())),
            }
        });
    }

    fn report(slot: &Slot, value: Result<DeviceHeading, String>) {
        if let Ok(mut g) = slot.lock() {
            *g = Some(value);
        }
    }

    pub fn stop(watch: Watch) {
        let Some(window) = web_sys::window() else {
            return;
        };
        WATCHES.with(|w| {
            let mut w = w.borrow_mut();
            let Some(pos) = w.iter().position(|(id, _)| *id == watch.id) else {
                return;
            };
            // Remove the listeners *before* dropping the closures, never after:
            // between the two the browser must already be unable to call them.
            let (_, reg) = w.remove(pos);
            let target: &web_sys::EventTarget = window.as_ref();
            let _ = target.remove_event_listener_with_callback(
                "deviceorientationabsolute",
                reg.absolute.as_ref().unchecked_ref(),
            );
            let _ = target.remove_event_listener_with_callback(
                "deviceorientation",
                reg.legacy.as_ref().unchecked_ref(),
            );
        });
    }
}

// ─── Android ─────────────────────────────────────────────────────────────────

#[cfg(target_os = "android")]
mod android {
    use super::{bearing_from_rotation_vector, screen_up_bearing, DeviceHeading, Slot};
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::Arc;
    use std::time::{Duration, Instant};

    /// How often a sample is asked for, in microseconds. 60 Hz.
    ///
    /// **This was 20 Hz, and the reason given for it was only true of one of the
    /// two consumers.** The argument was that the view eases toward the bearing at
    /// ~8/s (`free_camera`'s follow-and-rotate rig), so anything faster is
    /// filtered away — which holds for the *camera* and does not hold at all for
    /// the device arrow (`locate::render_locate_arrow`), which paints the reported
    /// bearing raw, every frame. At 20 Hz a phone turned at a brisk 90°/s moves
    /// that arrow in 4.5° steps three to six frames apart, which is exactly what
    /// "the update rate seems low" looks like. At 60 Hz the step is 1.5° and lands
    /// on roughly every frame of a 60 Hz screen.
    ///
    /// This is a *hint*: the framework clamps to what the sensor and the app's
    /// current foreground state allow, so a device that only manages 20 Hz simply
    /// keeps managing 20 Hz. The extra power is bounded by the subscription —
    /// nothing here runs unless the locate button is lit or a drive is on.
    const SAMPLE_PERIOD_US: i32 = 16_666;

    /// How long the looper waits for a sample before checking the cancel flag.
    const POLL_MS: i32 = 100;

    /// How often the display rotation is re-read. It changes only when the user
    /// turns the phone, and reading it is a JNI round trip through three objects
    /// — once a sample would be the sensor's rate, not the screen's.
    const ROTATION_REFRESH: Duration = Duration::from_millis(500);

    /// Our identifier for the sensor queue's fd within the looper. Any non-negative
    /// value distinct from `android-activity`'s idents would do; this looper is
    /// ours alone, so nothing can collide with it.
    const QUEUE_IDENT: std::os::raw::c_int = 1;

    /// A live sensor subscription. Signals and returns rather than joining, for
    /// the same reason `LocationSubscription` does: the dropper is a Bevy system.
    pub struct Subscription {
        cancel: Arc<AtomicBool>,
    }

    impl Subscription {
        pub fn stop(self) {
            self.cancel.store(true, Ordering::Relaxed);
        }
    }

    pub fn start(slot: Slot) -> Option<Subscription> {
        let cancel = Arc::new(AtomicBool::new(false));
        let worker_cancel = cancel.clone();
        let worker_slot = slot.clone();
        let spawned = std::thread::Builder::new()
            .name("cartopolis-compass".to_string())
            .spawn(move || {
                if let Err(msg) = run(&worker_slot, &worker_cancel) {
                    tracing::warn!(error = %msg, "android compass stopped");
                    report(&worker_slot, Err(msg));
                }
            });
        match spawned {
            Ok(_handle) => Some(Subscription { cancel }),
            Err(e) => {
                report(
                    &slot,
                    Err(format!("could not start the compass thread: {e}")),
                );
                None
            }
        }
    }

    fn report(slot: &Slot, value: Result<DeviceHeading, String>) {
        if let Ok(mut g) = slot.lock() {
            *g = Some(value);
        }
    }

    /// The sensor loop.
    ///
    /// **`ROTATION_VECTOR` first, `GEOMAGNETIC_ROTATION_VECTOR` second.** The
    /// first fuses the magnetometer with the gyroscope, so it is steady while the
    /// phone is being turned — which is the entire gesture this feature is about.
    /// The second needs no gyroscope and so exists on cheaper hardware, at the
    /// cost of lagging and wobbling near metal. `GAME_ROTATION_VECTOR` is
    /// deliberately not in the list even though it is the smoothest of the three:
    /// it drops the magnetometer altogether, so its yaw is relative to wherever
    /// the device was switched on, which is not a compass.
    ///
    /// SAFETY, once for the whole function: every pointer below comes from the
    /// call above it and is checked for null before use; the queue and the manager
    /// outlive every event read from them because they are destroyed at the very
    /// end; and the `ALooper` is this thread's own, prepared here and used
    /// nowhere else.
    #[allow(unsafe_code)]
    fn run(slot: &Slot, cancel: &AtomicBool) -> Result<(), String> {
        unsafe {
            let looper = ndk_sys::ALooper_prepare(
                ndk_sys::ALOOPER_PREPARE_ALLOW_NON_CALLBACKS as std::os::raw::c_int,
            );
            if looper.is_null() {
                return Err("could not prepare a looper for the compass".to_string());
            }

            let package = crate::platform::android::package_name();
            let manager = match &package {
                // `getInstanceForPackage` is the supported entry point; the
                // package name is what the platform attributes the sensor's power
                // draw to. `getInstance` is deprecated precisely because it cannot
                // attribute, but it is the only thing left if the name is
                // unreadable, and a compass that works beats an accounting row.
                Some(name) => match std::ffi::CString::new(name.as_str()) {
                    Ok(c) => ndk_sys::ASensorManager_getInstanceForPackage(c.as_ptr()),
                    Err(_) => ndk_sys::ASensorManager_getInstance(),
                },
                None => ndk_sys::ASensorManager_getInstance(),
            };
            if manager.is_null() {
                return Err("no sensor manager".to_string());
            }

            let mut sensor = ndk_sys::ASensorManager_getDefaultSensor(
                manager,
                ndk_sys::ASENSOR_TYPE_ROTATION_VECTOR,
            );
            if sensor.is_null() {
                sensor = ndk_sys::ASensorManager_getDefaultSensor(
                    manager,
                    ndk_sys::ASENSOR_TYPE_GEOMAGNETIC_ROTATION_VECTOR,
                );
            }
            if sensor.is_null() {
                return Err("this device has no compass".to_string());
            }

            let queue = ndk_sys::ASensorManager_createEventQueue(
                manager,
                looper,
                QUEUE_IDENT,
                None,
                std::ptr::null_mut(),
            );
            if queue.is_null() {
                return Err("could not open a sensor queue".to_string());
            }
            if ndk_sys::ASensorEventQueue_enableSensor(queue, sensor) < 0 {
                ndk_sys::ASensorManager_destroyEventQueue(manager, queue);
                return Err("the compass refused to start".to_string());
            }
            ndk_sys::ASensorEventQueue_setEventRate(queue, sensor, SAMPLE_PERIOD_US);

            let mut screen_angle =
                f64::from(crate::platform::android::display_rotation_deg().unwrap_or(0));
            let mut angle_read_at = Instant::now();
            let mut events = [std::mem::zeroed::<ndk_sys::ASensorEvent>(); 16];

            while !cancel.load(Ordering::Relaxed) {
                ndk_sys::ALooper_pollOnce(
                    POLL_MS,
                    std::ptr::null_mut(),
                    std::ptr::null_mut(),
                    std::ptr::null_mut(),
                );
                // Drain, and keep only the last: this loop wakes at the sensor's
                // rate but the reader (a Bevy system) samples at the frame rate,
                // so every event but the newest is already stale by the time
                // anyone looks. Draining rather than reading one keeps the queue
                // from growing while the app is backgrounded.
                let mut newest = None;
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
                    let n = n as usize;
                    newest = Some(events[n - 1]);
                }

                if angle_read_at.elapsed() >= ROTATION_REFRESH {
                    if let Some(deg) = crate::platform::android::display_rotation_deg() {
                        screen_angle = f64::from(deg);
                    }
                    angle_read_at = Instant::now();
                }

                if let Some(event) = newest {
                    // The rotation vector's payload is `[x, y, z, w]` — and `w` is
                    // *optional*: it was added in API 18 and a driver that predates
                    // it reports three values, leaving `data[3]` as whatever was in
                    // the buffer. The quaternion is a unit one, so the missing
                    // component is recoverable; a stale float is not, and would
                    // read as a heading tens of degrees out with nothing to
                    // indicate it.
                    let d = event.__bindgen_anon_1.__bindgen_anon_1.data;
                    let (x, y, z) = (f64::from(d[0]), f64::from(d[1]), f64::from(d[2]));
                    let w = f64::from(d[3]);
                    let w = if w.is_finite() && (x * x + y * y + z * z + w * w - 1.0).abs() < 0.05 {
                        w
                    } else {
                        (1.0 - (x * x + y * y + z * z)).max(0.0).sqrt()
                    };
                    let bearing =
                        screen_up_bearing(bearing_from_rotation_vector(x, y, z, w), screen_angle);
                    report(
                        slot,
                        Ok(DeviceHeading {
                            bearing_deg: bearing,
                            // `ASensorEvent` carries the sensor's accuracy class
                            // (0..3), not an angle. Reporting a class as degrees
                            // would be a made-up number, so nothing is reported.
                            accuracy_deg: None,
                        }),
                    );
                }
            }

            ndk_sys::ASensorEventQueue_disableSensor(queue, sensor);
            ndk_sys::ASensorManager_destroyEventQueue(manager, queue);
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The identity quaternion is a device lying flat with its top pointing north.
    #[test]
    fn a_level_device_pointing_north_reads_zero() {
        let b = bearing_from_rotation_vector(0.0, 0.0, 0.0, 1.0);
        assert!(b < 1e-6 || b > 360.0 - 1e-6, "expected ~0°, got {b}");
    }

    /// Turning the device clockwise (as seen from above) must *increase* the
    /// bearing — north → east → south. A sign slip here is a compass that runs
    /// backwards, which reads as "the map lags" rather than as an inversion.
    #[test]
    fn turning_clockwise_increases_the_bearing() {
        // A rotation of the device by −90° about the world up axis: the device's
        // top swings from north to east. The quaternion taking device coordinates
        // into the world frame is then a −45°/2 rotation about z.
        let (s, c) = (-45.0_f64).to_radians().sin_cos();
        let b = bearing_from_rotation_vector(0.0, 0.0, s, c);
        assert!((b - 90.0).abs() < 1e-6, "expected 90° (east), got {b}");
    }

    /// The case the doc comment argues from: portrait pointing north, phone turned
    /// 90° anticlockwise, content turned 90° clockwise to compensate. The picture
    /// has not moved, so the answer must still be north.
    #[test]
    fn landscape_does_not_move_the_picture() {
        assert!((screen_up_bearing(270.0, 90.0) - 0.0).abs() < 1e-9);
    }

    #[test]
    fn the_bearing_stays_in_one_turn() {
        for (device, screen) in [(350.0, 90.0), (10.0, 270.0), (-30.0, 0.0), (0.0, 180.0)] {
            let b = screen_up_bearing(device, screen);
            assert!((0.0..360.0).contains(&b), "{device}/{screen} gave {b}");
        }
    }
}
