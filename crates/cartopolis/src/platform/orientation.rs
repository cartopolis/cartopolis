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
//! **What this hands out is the attitude of the *screen*, not of the device.**
//! Every provider reports the device's own frame, and the user turns a *picture*:
//! on a phone held in landscape the two differ by 90°, and a compass that ignores
//! it points sideways in exactly the orientation people navigate in. The
//! correction is [`screen_frame`], and it is applied inside each provider so the
//! value crossing this seam needs no further interpretation.
//!
//! **It is a whole rotation and not a bearing, and that is a 2026-09-17 change.**
//! It used to be one number — the compass bearing of screen-up — because the one
//! consumer was a map that turns under a level phone. That is not enough to point
//! a phone *at* something: the first-person view ([`crate::systems::free_camera`])
//! has to answer "which building is the back of this phone aimed at", which needs
//! the tilt as well, and the old number is degenerate in exactly the pose that
//! question is asked in — screen-up points at the sky when the phone is held
//! upright, so its compass bearing is the azimuth of a vertical vector. The
//! bearing is still available ([`DeviceAttitude::bearing_deg`]) and is still what
//! the map arrow reads; it is derived from the rotation now rather than beside it,
//! so the two cannot disagree.
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

use crate::platform::sync::{Arc, Mutex};
use bevy::math::{DQuat, DVec3, Quat};

/// One attitude report, normalised across providers.
///
/// The payload is the rotation taking **screen** coordinates into the sensors'
/// world frame, which is East/North/Up on every platform here. The screen frame
/// is the ordinary one: `+x` right along the screen, `+y` up it, `+z` out of the
/// glass toward the viewer — so `−z` is where the back of the phone, and its
/// camera, is aimed.
///
/// Those axes are *exactly* Bevy's camera axes, which is why
/// [`Self::camera_rotation`] is a change of world basis and nothing more.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct DeviceAttitude {
    /// Screen → world (East/North/Up). Unit.
    pub screen_to_world: DQuat,
    /// Reported accuracy in degrees, when the platform states one. `None` is the
    /// ordinary case on the web, where no provider reports it.
    pub accuracy_deg: Option<f64>,
}

impl DeviceAttitude {
    /// An attitude built from where the phone is *aimed*: the compass bearing and
    /// the elevation of the view out of its back, plus the rotation of the
    /// picture about that axis.
    ///
    /// The inverse of the three readings in [`Self::camera_rotation`]'s frame, and
    /// the constructor every test and the scripted capture seed use — describing a
    /// pose as "north-east, 5° above the horizon, upright" is checkable by hand in
    /// a way a quaternion literal is not.
    #[must_use]
    pub fn pointing(bearing_deg: f64, elevation_deg: f64, roll_deg: f64) -> Self {
        // The camera rotation this must produce, then undo the basis change.
        let camera = DQuat::from_euler(
            bevy::math::EulerRot::YXZ,
            -bearing_deg.to_radians(),
            elevation_deg.to_radians(),
            roll_deg.to_radians(),
        );
        Self {
            screen_to_world: world_from_enu().inverse() * camera,
            accuracy_deg: None,
        }
    }

    /// Compass bearing of the top of the screen, in `[0, 360)` clockwise from
    /// north — what the map arrow and the follow camera's chase rig read.
    ///
    /// Ill-conditioned by construction when the phone is held upright, because
    /// then screen-up *is* the vertical and a vertical vector has no bearing. That
    /// is a property of the question, not of this code: it is why the first-person
    /// view asks [`Self::camera_rotation`] instead.
    #[must_use]
    pub fn bearing_deg(&self) -> f64 {
        let up = self.screen_to_world * DVec3::Y;
        up.x.atan2(up.y).to_degrees().rem_euclid(360.0)
    }

    /// The rotation to give a camera so it sees what the back of the phone is
    /// pointed at, in the app's world frame (`+x` east, `+y` up, `−z` north).
    ///
    /// One change of basis, because the screen frame and Bevy's camera frame are
    /// the same axes — a camera looks down `−z` with `+y` up, which is a phone's
    /// back camera with the top of the picture up the screen. Everything that
    /// could be got wrong here is therefore in [`world_from_enu`], which one test
    /// pins against all three axes.
    #[must_use]
    pub fn camera_rotation(&self) -> Quat {
        (world_from_enu() * self.screen_to_world).as_quat()
    }

    /// Where a camera on this phone looks: `(heading, pitch, roll)` in degrees —
    /// compass heading clockwise from north, elevation above the horizon, and
    /// the third angle of the free camera's `EulerRot::YXZ` (the convention it
    /// is rendered back in). `front` is the selfie camera, which looks out of
    /// the screen: the back camera's view turned half a turn about the screen's
    /// up axis. The inverse of [`Self::pointing`] for the back camera.
    #[must_use]
    #[cfg_attr(target_arch = "wasm32", allow(dead_code))]
    pub fn view_angles(&self, front: bool) -> (f64, f64, f64) {
        let mut camera = world_from_enu() * self.screen_to_world;
        if front {
            camera *= DQuat::from_rotation_y(std::f64::consts::PI);
        }
        let (yaw, pitch, roll) = camera.to_euler(bevy::math::EulerRot::YXZ);
        (
            (-yaw.to_degrees()).rem_euclid(360.0),
            pitch.to_degrees(),
            roll.to_degrees(),
        )
    }
}

/// The rotation taking the sensors' East/North/Up frame into the app's world
/// frame, where `+x` is east, `+y` is up and **north is `−z`**.
///
/// A quarter turn about east, and the sign is the whole content: north must land
/// on `−z` and up on `+y`. Get it backwards and a phone pointed north shows the
/// view to the south, which reads as a compass fault rather than as a frame one.
fn world_from_enu() -> DQuat {
    DQuat::from_rotation_x(-std::f64::consts::FRAC_PI_2)
}

/// The slot a provider writes into. Same shape and same reason as
/// [`crate::platform::location::Slot`]: an `Err` is how a denied permission or a
/// missing sensor reaches the UI instead of an eternal "waiting".
pub(crate) type Slot = Arc<Mutex<Option<Result<DeviceAttitude, String>>>>;

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
    pub fn latest(&self) -> Option<DeviceAttitude> {
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

/// Turn a **device** attitude into a **screen** one, given the display's
/// rotation.
///
/// `screen_angle_deg` is the angle the content is rotated **clockwise** within
/// the device frame to stay upright — `screen.orientation.angle` on the web and
/// `Display.getRotation() × 90` on Android, which are the same quantity. The
/// screen's axes are therefore the device's turned *anticlockwise* by it, which
/// is the negative rotation about the device's own `+z` composed on the right.
///
/// The sign is the thing worth checking, so here is the case that pins it. Hold a
/// phone in portrait with its top pointing north (device bearing 0). Rotate the
/// phone 90° anticlockwise: its top now points west (bearing 270), and the system
/// rotates the content 90° clockwise to keep it upright, so `screen.orientation.
/// angle` reads 90. The picture the user is looking at has not moved — screen-up
/// is still north. Getting the sign backwards reads 180° out, and only in
/// landscape, which is a bug report that sounds like "the compass is unreliable".
// `allow`: `platform` is a private module, so a function only the two device arms
// call is dead code on every other target. It stays compiled and tested
// everywhere on purpose — this is the piece of the compass that can be wrong in a
// way a phone would not obviously show, and a desktop `cargo test` is the only
// place it gets checked.
#[allow(dead_code)]
#[must_use]
pub fn screen_frame(device_to_world: DQuat, screen_angle_deg: f64) -> DQuat {
    device_to_world * DQuat::from_rotation_z(-screen_angle_deg.to_radians())
}

/// The device → world (East/North/Up) rotation the web's `deviceorientation`
/// angles describe.
///
/// The spec defines them as intrinsic rotations applied in the order Z (`alpha`),
/// X (`beta`), Y (`gamma`) to a device that starts aligned with the Earth frame,
/// so this is that product and nothing else. `alpha` runs **anticlockwise** from
/// north, which is why every bearing on this seam is `360 − alpha` rather than
/// `alpha`.
///
/// Android does not use this: its rotation vector already *is* the quaternion.
// `allow`: see `screen_frame` — only the web arm calls it.
#[allow(dead_code)]
#[must_use]
pub fn device_to_world_from_euler(alpha_deg: f64, beta_deg: f64, gamma_deg: f64) -> DQuat {
    DQuat::from_euler(
        bevy::math::EulerRot::ZXY,
        alpha_deg.to_radians(),
        beta_deg.to_radians(),
        gamma_deg.to_radians(),
    )
}

// ─── Web ─────────────────────────────────────────────────────────────────────

#[cfg(target_arch = "wasm32")]
mod wasm {
    use super::{device_to_world_from_euler, screen_frame, DeviceAttitude, Slot};
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

    /// Reduce one event to a screen attitude, or `None` if it carries no absolute
    /// heading.
    ///
    /// Two sources for the yaw, in preference order. `webkitCompassHeading` is
    /// Safari's, and it is *already* a compass bearing of the device top (and
    /// already corrected to true north), so it is turned back into the `alpha`
    /// that would have produced it. `alpha` is the standard's, and it is measured
    /// anticlockwise from north, hence the `360 −` in both directions. A plain
    /// `deviceorientation` event with neither is **relative** — Chrome's alpha
    /// there starts wherever the page was loaded — and reporting it as a compass
    /// would silently point the map at an arbitrary bearing, so it is dropped.
    ///
    /// `beta` and `gamma` are the tilt, and they are absolute on every browser
    /// because they are measured against gravity rather than against the magnetic
    /// field — which is what lets Safari's reconstruction work at all: only the
    /// yaw is missing there, and only the yaw is substituted. A device that
    /// reports neither is treated as lying flat, which is what this seam assumed
    /// of every device before it carried tilt at all.
    fn attitude_of(event: &web_sys::DeviceOrientationEvent) -> Option<DeviceAttitude> {
        let webkit = js_sys::Reflect::get(event, &JsValue::from_str("webkitCompassHeading"))
            .ok()
            .and_then(|v| v.as_f64())
            .filter(|v| v.is_finite());
        let alpha = match webkit {
            Some(heading) => 360.0 - heading,
            None => {
                if !event.absolute() {
                    return None;
                }
                let alpha = event.alpha()?;
                if !alpha.is_finite() {
                    return None;
                }
                alpha
            }
        };
        let tilt = |v: Option<f64>| v.filter(|v| v.is_finite()).unwrap_or(0.0);
        let accuracy_deg = js_sys::Reflect::get(event, &JsValue::from_str("webkitCompassAccuracy"))
            .ok()
            .and_then(|v| v.as_f64())
            // Safari reports −1 for "no fix yet", which is not an accuracy.
            .filter(|v| v.is_finite() && *v >= 0.0);
        let device = device_to_world_from_euler(alpha, tilt(event.beta()), tilt(event.gamma()));
        Some(DeviceAttitude {
            screen_to_world: screen_frame(device, screen_angle()),
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
                    if let Some(a) = attitude_of(&e) {
                        if let Ok(mut g) = sink.lock() {
                            *g = Some(Ok(a));
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

    fn report(slot: &Slot, value: Result<DeviceAttitude, String>) {
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
    use super::{screen_frame, DeviceAttitude, Slot};
    use bevy::math::DQuat;
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

    fn report(slot: &Slot, value: Result<DeviceAttitude, String>) {
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
                    // The rotation vector *is* the device → East/North/Up
                    // quaternion — it is what `getRotationMatrixFromVector`
                    // expands — so nothing is derived here beyond the display's
                    // own turn.
                    let device = DQuat::from_xyzw(x, y, z, w);
                    // `values[4]` is the rotation vector's own estimate of its
                    // heading error, in radians, or −1 when it has none (API 18+;
                    // `ASensorEvent`'s separate accuracy field is a class 0..3, not
                    // an angle, and is not used). Anything outside (0, π) is the
                    // "none" answer or a driver that never wrote it.
                    let heading_error = f64::from(d[4]);
                    report(
                        slot,
                        Ok(DeviceAttitude {
                            screen_to_world: screen_frame(device, screen_angle),
                            accuracy_deg: (heading_error.is_finite()
                                && heading_error > 0.0
                                && heading_error < std::f64::consts::PI)
                                .then(|| heading_error.to_degrees()),
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

    #[test]
    fn view_angles_read_back_what_pointing_built() {
        for (bearing, elevation, roll) in
            [(9.0, 14.0, 0.4), (270.0, -5.0, -3.0), (135.0, 30.0, 10.0)]
        {
            let a = DeviceAttitude::pointing(bearing, elevation, roll);
            let (h, p, r) = a.view_angles(false);
            assert!((h - bearing).abs() < 1e-6, "{h} vs {bearing}");
            assert!((p - elevation).abs() < 1e-6, "{p} vs {elevation}");
            assert!((r - roll).abs() < 1e-6, "{r} vs {roll}");
            // The selfie camera looks the other way, and down where the back
            // one looks up.
            let (hf, pf, _) = a.view_angles(true);
            assert!(
                ((hf - bearing).rem_euclid(360.0) - 180.0).abs() < 1e-6,
                "{hf}"
            );
            assert!((pf + elevation).abs() < 1e-6, "{pf}");
        }
    }

    /// A flat device with its top pointing north, as a rotation vector.
    fn flat_facing(bearing_deg: f64) -> DQuat {
        // Turning the device clockwise seen from above is a *negative* rotation
        // about the ENU up axis.
        DQuat::from_rotation_z(-bearing_deg.to_radians())
    }

    /// The identity quaternion is a device lying flat with its top pointing north.
    #[test]
    fn a_level_device_pointing_north_reads_zero() {
        let b = DeviceAttitude {
            screen_to_world: DQuat::IDENTITY,
            accuracy_deg: None,
        }
        .bearing_deg();
        assert!(b < 1e-6 || b > 360.0 - 1e-6, "expected ~0°, got {b}");
    }

    /// Turning the device clockwise (as seen from above) must *increase* the
    /// bearing — north → east → south. A sign slip here is a compass that runs
    /// backwards, which reads as "the map lags" rather than as an inversion.
    #[test]
    fn turning_clockwise_increases_the_bearing() {
        for want in [0.0, 90.0, 180.0, 270.0] {
            let b = DeviceAttitude {
                screen_to_world: flat_facing(want),
                accuracy_deg: None,
            }
            .bearing_deg();
            let off = (b - want).rem_euclid(360.0);
            assert!(
                off < 1e-6 || off > 360.0 - 1e-6,
                "a device facing {want}° read {b}°"
            );
        }
    }

    /// The case the module doc argues from: portrait pointing north, phone turned
    /// 90° anticlockwise, content turned 90° clockwise to compensate. The picture
    /// has not moved, so the answer must still be north.
    #[test]
    fn landscape_does_not_move_the_picture() {
        let device = flat_facing(270.0);
        let b = DeviceAttitude {
            screen_to_world: screen_frame(device, 90.0),
            accuracy_deg: None,
        }
        .bearing_deg();
        assert!(
            b < 1e-6 || b > 360.0 - 1e-6,
            "expected ~0° (north), got {b}"
        );
    }

    #[test]
    fn the_bearing_stays_in_one_turn() {
        for (device, screen) in [(350.0, 90.0), (10.0, 270.0), (-30.0, 0.0), (0.0, 180.0)] {
            let b = DeviceAttitude {
                screen_to_world: screen_frame(flat_facing(device), screen),
                accuracy_deg: None,
            }
            .bearing_deg();
            assert!((0.0..360.0).contains(&b), "{device}/{screen} gave {b}");
        }
    }

    // ── The attitude half ───────────────────────────────────────────────────

    /// Where the camera looks, as a unit vector in the app's world frame.
    fn look(a: DeviceAttitude) -> bevy::math::Vec3 {
        a.camera_rotation() * bevy::math::Vec3::NEG_Z
    }

    /// The whole feature in one assertion: point the back of the phone at a
    /// compass bearing and the camera looks along it, with north on `−z` and east
    /// on `+x`. A frame slip here aims the view at a different building, which is
    /// the one failure the user would report and the one this container cannot
    /// see.
    #[test]
    fn the_camera_looks_where_the_phone_is_aimed() {
        for (bearing, expected) in [
            (0.0_f64, bevy::math::Vec3::new(0.0, 0.0, -1.0)), // north
            (90.0, bevy::math::Vec3::new(1.0, 0.0, 0.0)),     // east
            (180.0, bevy::math::Vec3::new(0.0, 0.0, 1.0)),    // south
            (270.0, bevy::math::Vec3::new(-1.0, 0.0, 0.0)),   // west
        ] {
            let l = look(DeviceAttitude::pointing(bearing, 0.0, 0.0));
            assert!(
                (l - expected).length() < 1e-5,
                "aimed at {bearing}° the camera looks {l:?}, wanted {expected:?}"
            );
        }
    }

    /// Tilting the phone up must look up. Sky is `+y`.
    #[test]
    fn raising_the_phone_raises_the_view() {
        let up = look(DeviceAttitude::pointing(0.0, 30.0, 0.0));
        assert!((up.y - 0.5).abs() < 1e-5, "30° up gave y={}", up.y);
        let down = look(DeviceAttitude::pointing(0.0, -30.0, 0.0));
        assert!((down.y + 0.5).abs() < 1e-5, "30° down gave y={}", down.y);
    }

    /// A phone lying flat on a table is aimed at the table — its back camera
    /// points at the floor. This is the pose the *bearing* is well-conditioned in
    /// and the *view* is degenerate in, which is the exact opposite of the upright
    /// pose below, and the reason both are carried.
    #[test]
    fn a_flat_phone_looks_straight_down() {
        let a = DeviceAttitude {
            screen_to_world: DQuat::IDENTITY,
            accuracy_deg: None,
        };
        let l = look(a);
        assert!(
            (l.y + 1.0).abs() < 1e-5,
            "expected straight down, got {l:?}"
        );
    }

    /// …and an upright phone has a well-defined view and a meaningless bearing.
    /// Held upright facing north, screen-up is the vertical, so `bearing_deg` is
    /// the azimuth of a vector with no horizontal part — while the camera still
    /// looks squarely north. Pinned because it is the pose the first-person view
    /// is used in, and the reason it may not read the bearing.
    #[test]
    fn an_upright_phone_still_aims_north() {
        let a = DeviceAttitude::pointing(0.0, 0.0, 0.0);
        let l = look(a);
        assert!(
            (l - bevy::math::Vec3::NEG_Z).length() < 1e-5,
            "upright and facing north, the camera looks {l:?}"
        );
        let up = a.screen_to_world * DVec3::Y;
        assert!(
            up.x.hypot(up.y) < 1e-9,
            "screen-up should be vertical here, giving the bearing nothing to \
             measure; it was {up:?}"
        );
    }

    /// Rolling the phone rolls the picture and leaves the aim alone — which is
    /// what makes roll safe to pass through to the camera.
    #[test]
    fn roll_turns_the_picture_without_moving_the_aim() {
        let a = DeviceAttitude::pointing(90.0, 10.0, 25.0);
        let b = DeviceAttitude::pointing(90.0, 10.0, 0.0);
        assert!(
            (look(a) - look(b)).length() < 1e-5,
            "roll moved the aim: {:?} vs {:?}",
            look(a),
            look(b)
        );
    }

    /// Turning an upright phone into landscape, with the system re-laying the
    /// content out to follow it, must show **the same view** — the camera
    /// equivalent of `landscape_does_not_move_the_picture`, which pins the same
    /// property for the bearing alone.
    ///
    /// This is the test that justifies passing the roll through to the camera
    /// rather than levelling the horizon. The layout turn and the hand's turn are
    /// the same rotation here, so what the system adds is exactly what
    /// [`screen_frame`] takes away. Level the horizon instead and only the
    /// discrete half of that pair survives, which is a 90° jolt in the picture
    /// every time the phone is rotated — the fault §1 of the note exists to
    /// prevent, reintroduced one layer up.
    ///
    /// Note what is *not* claimed: that the view is continuous across the instant
    /// the system switches. It is not, and should not be. Tilt a phone 89° with
    /// the layout still in portrait and the world is genuinely drawn 89° round on
    /// the glass — which is upright to the eyes of the person holding it tilted.
    /// When the layout flips, the picture on the glass turns with it. The thing
    /// that stays still through all of it is the view as *seen*, and that is what
    /// this asserts at the four places the two turns agree.
    #[test]
    fn rotating_the_phone_with_the_layout_shows_the_same_view() {
        // Held upright and aimed east, then turned about the axis it is aimed
        // along — the gesture that changes screen orientation.
        let turned = |deg: f64| {
            DeviceAttitude::pointing(90.0, 0.0, 0.0).screen_to_world
                * DQuat::from_rotation_z(deg.to_radians())
        };
        let seen = |deg: f64, screen_angle: f64| {
            DeviceAttitude {
                screen_to_world: screen_frame(turned(deg), screen_angle),
                accuracy_deg: None,
            }
            .camera_rotation()
        };
        // Compared as the two vectors that *are* the view — where it aims and
        // which way is up in it — rather than through `Quat::angle_between`.
        // That function is `acos` of the dot product, which near identity grows
        // as the square root of the error: one f32 ulp between two rotations
        // that agree to the last bit comes out as ~0.04°, so a tolerance tight
        // enough to catch a real fault cannot be met by arithmetic that is
        // already exact. These two are linear in the error and say the same
        // thing.
        let upright = seen(0.0, 0.0);
        let aim = |q: bevy::math::Quat| (q * bevy::math::Vec3::NEG_Z, q * bevy::math::Vec3::Y);
        let (want_fwd, want_up) = aim(upright);
        for quarter in [90.0, 180.0, 270.0] {
            let (fwd, up) = aim(seen(quarter, quarter));
            assert!(
                (fwd - want_fwd).length() < 1e-5,
                "turning the phone {quarter}° with the layout moved the aim from \
                 {want_fwd:?} to {fwd:?}"
            );
            assert!(
                (up - want_up).length() < 1e-5,
                "turning the phone {quarter}° with the layout rolled the picture \
                 from {want_up:?} to {up:?}"
            );
        }
    }

    /// The web's three angles, against the frame the spec defines them in: with
    /// `beta` and `gamma` zero the device lies flat and `alpha` runs
    /// **anticlockwise** from north, so `alpha = 90` is a device facing west.
    ///
    /// This is the assertion that fails if glam's Euler convention ever moves
    /// under us — `EulerRot::ZXY` has to stay the intrinsic product the spec
    /// describes, and nothing else in this crate would notice if it did not.
    #[test]
    fn the_web_angles_match_the_spec_frame() {
        for (alpha, bearing) in [(0.0, 0.0), (90.0, 270.0), (180.0, 180.0), (270.0, 90.0)] {
            let a = DeviceAttitude {
                screen_to_world: device_to_world_from_euler(alpha, 0.0, 0.0),
                accuracy_deg: None,
            };
            let off = (a.bearing_deg() - bearing).rem_euclid(360.0);
            assert!(
                off < 1e-6 || off > 360.0 - 1e-6,
                "alpha {alpha}° should read {bearing}°, read {}°",
                a.bearing_deg()
            );
        }
    }

    /// `beta` is a tip about the device's own `+x`: standing a flat phone up on
    /// its bottom edge is `beta = 90`, and that aims the back camera at the
    /// horizon rather than at the floor.
    #[test]
    fn standing_the_phone_up_aims_it_at_the_horizon() {
        let a = DeviceAttitude {
            screen_to_world: device_to_world_from_euler(0.0, 90.0, 0.0),
            accuracy_deg: None,
        };
        let l = look(a);
        assert!(l.y.abs() < 1e-5, "expected a level view, got {l:?}");
        assert!(
            (l - bevy::math::Vec3::NEG_Z).length() < 1e-5,
            "expected to look north, got {l:?}"
        );
    }

    /// `pointing` and the angles it names are inverses — the constructor the
    /// tests and the capture seed are written in cannot drift from the reading
    /// the camera takes.
    #[test]
    fn pointing_round_trips_through_the_bearing() {
        // Tilted well off vertical, so screen-up has a horizontal part and the
        // bearing is a fair question.
        let a = DeviceAttitude::pointing(123.0, -70.0, 0.0);
        let off = (a.bearing_deg() - 123.0).rem_euclid(360.0);
        assert!(
            off < 1e-6 || off > 360.0 - 1e-6,
            "round trip gave {}",
            a.bearing_deg()
        );
    }
}
