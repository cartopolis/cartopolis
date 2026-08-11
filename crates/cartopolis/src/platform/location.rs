//! Where the *device* thinks it is — the location provider seam (ADR-030).
//!
//! Every other position in the client is a place the user drove the camera or the
//! avatar to. This one is not: it is the machine reporting its own coordinates,
//! and that is a thing the browser and a desktop OS do completely differently. So
//! it goes behind a provider like [`crate::platform::http`] does for fetching:
//! one API ([`LocationWatch`]), a wasm implementation over
//! `navigator.geolocation`, an Android implementation over `LocationManager`, and
//! a desktop implementation that reports nothing at all.
//!
//! **Desktop reports nothing on purpose.** A Linux desktop has no location unless
//! something is plugged into it, and the honest answer to "where am I" on a box
//! with no GNSS is *unknown* — not a guess, and not a silent fallback that looks
//! like a fix. [`LocationWatch::available`] is `false` there, and driver mode
//! reads the avatar's own position instead (see `systems::driver`). A real
//! backend — a USB GNSS dongle over serial, GeoClue2, gpsd — drops in behind this
//! same API without any caller changing.
//!
//! **Android is the third arm, and it is the one that had to be designed around a
//! constraint rather than around an API.** The phone has GNSS, but the natural way
//! to read it (`LocationListener`) is a Java interface, and this app ships no
//! `classes.dex` — see `crate::platform::android` and `docs/notes/android-build.md`
//! for what it does instead and, more importantly, for which of its claims a device
//! still has to confirm. None of that reaches this file: the Android arm writes the
//! same [`GpsFix`] into the same slot, and reports a denied permission as an `Err`
//! for the same reason the browser's arm does.
//!
//! The watch is a *subscription*: [`LocationWatch::start`] begins it, dropping the
//! value ends it. Nothing polls a provider that no one is listening to, which
//! matters on the web, where an open watch keeps the browser's location indicator
//! lit and, on a phone, the GNSS radio warm.

use std::sync::{Arc, Mutex};

/// One position report, normalised across providers.
///
/// `speed_ms` and `heading_deg` are `Option` because they genuinely are: the W3C
/// API returns null for both when the device cannot derive them, which is the
/// ordinary case for a stationary receiver (a course over ground needs ground to
/// have been covered). Callers fall back to a course measured from successive
/// fixes rather than treating a missing value as zero.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct GpsFix {
    pub lat: f64,
    pub lng: f64,
    /// Reported horizontal accuracy, in metres (1σ, per the W3C definition).
    pub accuracy_m: f64,
    /// Ground speed in metres per second, when the device reports one.
    pub speed_ms: Option<f64>,
    /// Course over ground, degrees clockwise from true north.
    pub heading_deg: Option<f64>,
}

/// The slot a provider writes into. `Result` so a permission denial reaches the
/// UI as a message rather than as an eternal "waiting for a fix".
///
/// `pub(crate)` only so the Android provider — which lives beside this module
/// rather than inside it, being 400 lines of JNI — can write into it.
pub(crate) type Slot = Arc<Mutex<Option<Result<GpsFix, String>>>>;

/// A live subscription to the device's position. Drop it to unsubscribe.
pub struct LocationWatch {
    slot: Slot,
    // One field, two mutually exclusive cfgs: Android is never `wasm32`, so the
    // two attributes cannot both apply and produce a duplicate field. Sharing the
    // name keeps `start`/`drop` reading as one shape with a per-platform body
    // rather than as two parallel structs.
    #[cfg(target_arch = "wasm32")]
    inner: Option<wasm::Watch>,
    #[cfg(target_os = "android")]
    inner: Option<crate::platform::android::LocationSubscription>,
}

impl LocationWatch {
    /// Whether this build has a provider that can produce a fix at all. `false`
    /// on desktop — see the module note.
    #[must_use]
    pub const fn available() -> bool {
        cfg!(any(target_arch = "wasm32", target_os = "android"))
    }

    /// Begin watching. Cheap and non-blocking; the first fix arrives later (a cold
    /// GNSS start is seconds, and the browser may prompt for permission first).
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
            // The Android provider does its own work on its own thread, including
            // waiting out the runtime-permission dialog — which is why this is
            // still "cheap and non-blocking" on a platform where the first fix can
            // be two minutes away.
            let inner = crate::platform::android::start_location(slot.clone());
            Self { slot, inner }
        }
        #[cfg(not(any(target_arch = "wasm32", target_os = "android")))]
        {
            tracing::info!(
                "no native location provider in this build — driver mode follows the avatar"
            );
            Self { slot }
        }
    }

    /// The most recent fix, or `None` while nothing has arrived (or the provider
    /// reported an error — see [`Self::error`]).
    #[must_use]
    pub fn latest(&self) -> Option<GpsFix> {
        self.slot.lock().ok()?.as_ref()?.as_ref().ok().copied()
    }

    /// The most recent provider error, if the last report was one. Permission
    /// denials land here.
    #[must_use]
    pub fn error(&self) -> Option<String> {
        self.slot.lock().ok()?.as_ref()?.as_ref().err().cloned()
    }
}

impl Drop for LocationWatch {
    fn drop(&mut self) {
        #[cfg(target_arch = "wasm32")]
        if let Some(inner) = self.inner.take() {
            wasm::stop(inner);
        }
        // Explicit, rather than letting the field's own `Drop` do it: a subscription
        // that unsubscribed as a side effect of a field going out of scope would be
        // read as an unused field by `unused = "deny"`, and silencing that with an
        // `allow` would hide the day it really did become unused. The call also
        // makes the two arms symmetrical.
        #[cfg(target_os = "android")]
        if let Some(inner) = self.inner.take() {
            inner.stop();
        }
    }
}

#[cfg(target_arch = "wasm32")]
mod wasm {
    use super::{GpsFix, Slot};
    use std::cell::RefCell;
    use wasm_bindgen::prelude::Closure;
    use wasm_bindgen::JsCast;

    /// A live `watchPosition`, identified by the id the browser handed back.
    ///
    /// **This holds an `i32` and nothing else, and that is the whole point.** The
    /// two callbacks must outlive the watch — dropping one and then having the
    /// browser call it is a use-after-free — but they cannot be *stored here* to
    /// achieve that: a `Closure` is `!Send + !Sync` (it wraps a `JsValue`, whose
    /// `PhantomData<*mut u8>` is what the compiler actually complains about), this
    /// travels inside `LocationWatch` into `driver::DriverState`, and a Bevy
    /// `Resource` must be `Send + Sync`. Keeping them inline made `DriverState`
    /// un-`Resource`-able and broke every one of its ~10 consumers, which is a
    /// ~180-error wall a long way from its cause.
    ///
    /// So the closures live in [`CALLBACKS`] on the thread that created them and
    /// are dropped by id in [`stop`]. Same lifetime guarantee, no `!Send` field,
    /// and no `unsafe impl Send` asserting something about Bevy's executor that a
    /// future Bevy is free to invalidate.
    pub struct Watch {
        id: i32,
    }

    thread_local! {
        /// Callbacks for the live watches on this thread, keyed by watch id.
        ///
        /// Thread-local rather than a global: a JS function is reachable only from
        /// the realm that created it — under `--shared-memory` the linear memory is
        /// shared but the function table is per-instance — so a closure made on the
        /// main thread is meaningless to a worker anyway. Being thread-local makes
        /// that structural instead of a comment. If a `LocationWatch` were somehow
        /// dropped on another thread the entry is simply not found and the callbacks
        /// outlive the process; that leaks two closures, it cannot be unsound.
        static CALLBACKS: RefCell<Vec<(i32, CallbackPair)>> = const { RefCell::new(Vec::new()) };
    }

    type CallbackPair = (
        Closure<dyn FnMut(web_sys::GeolocationPosition)>,
        Closure<dyn FnMut(web_sys::GeolocationPositionError)>,
    );

    pub fn start(slot: Slot) -> Option<Watch> {
        let geo = web_sys::window()
            .map(|w| w.navigator())
            .and_then(|n| n.geolocation().ok())?;

        let sink = slot.clone();
        let on_fix = Closure::<dyn FnMut(web_sys::GeolocationPosition)>::new(
            move |p: web_sys::GeolocationPosition| {
                let c = p.coords();
                let fix = GpsFix {
                    lat: c.latitude(),
                    lng: c.longitude(),
                    accuracy_m: c.accuracy(),
                    speed_ms: c.speed(),
                    heading_deg: c.heading(),
                };
                if let Ok(mut g) = sink.lock() {
                    *g = Some(Ok(fix));
                }
            },
        );
        let sink = slot;
        let on_err = Closure::<dyn FnMut(web_sys::GeolocationPositionError)>::new(
            move |e: web_sys::GeolocationPositionError| {
                let msg = match e.code() {
                    1 => "Location permission denied".to_string(),
                    2 => "Position unavailable".to_string(),
                    3 => "Timed out waiting for a fix".to_string(),
                    _ => e.message(),
                };
                tracing::warn!(code = e.code(), message = %msg, "geolocation error");
                if let Ok(mut g) = sink.lock() {
                    *g = Some(Err(msg));
                }
            },
        );

        // High accuracy: this drives a navigation view, where the network-derived
        // city-block fix is useless. `maximum_age = 0` refuses a cached position —
        // a stale one would place the arrow where the device was minutes ago.
        let opts = web_sys::PositionOptions::new();
        opts.set_enable_high_accuracy(true);
        opts.set_maximum_age(0);
        opts.set_timeout(20_000);

        // Returns a bare `i32`, not a `Result` — and that is a consequence of
        // `--cfg=web_sys_unstable_apis`, not of the API. web-sys carries *two*
        // overloads of this method: the stable one takes untyped `js_sys::Function`
        // callbacks and is `catch`-annotated (`Result<i32, JsValue>`), while the
        // unstable one — the only one that exists once the cfg selects the typed
        // `GeolocationPosition` family — takes typed functions and does not catch.
        // The cfg silently swaps which one resolves here, so a call that used to
        // need a `match` now must not have one. If this ever stops compiling after
        // a web-sys bump, check the overload before changing the logic.
        let id = geo.watch_position_with_error_callback_and_options(
            on_fix.as_ref().unchecked_ref(),
            Some(on_err.as_ref().unchecked_ref()),
            &opts,
        );
        // Registered only once the watch exists, so the pair is never held for a
        // watch the browser is not running.
        CALLBACKS.with(|c| c.borrow_mut().push((id, (on_fix, on_err))));
        Some(Watch { id })
    }

    pub fn stop(watch: Watch) {
        // Clear the watch *before* releasing the callbacks, never after: between
        // the two the browser must already be unable to invoke them.
        if let Some(geo) = web_sys::window()
            .map(|w| w.navigator())
            .and_then(|n| n.geolocation().ok())
        {
            geo.clear_watch(watch.id);
        }
        CALLBACKS.with(|c| c.borrow_mut().retain(|(id, _)| *id != watch.id));
    }
}
