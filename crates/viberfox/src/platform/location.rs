//! Where the *device* thinks it is — the location provider seam (ADR-030).
//!
//! Every other position in the client is a place the user drove the camera or the
//! avatar to. This one is not: it is the machine reporting its own coordinates,
//! and that is a thing the browser and a desktop OS do completely differently. So
//! it goes behind a provider like [`crate::platform::http`] does for fetching:
//! one API ([`LocationWatch`]), a wasm implementation over
//! `navigator.geolocation`, and a native implementation that currently reports
//! nothing at all.
//!
//! **Native reports nothing on purpose.** A Linux desktop has no location unless
//! something is plugged into it, and the honest answer to "where am I" on a box
//! with no GNSS is *unknown* — not a guess, and not a silent fallback that looks
//! like a fix. [`LocationWatch::available`] is `false` there, and driver mode
//! reads the avatar's own position instead (see `systems::driver`). A real
//! backend — a USB GNSS dongle over serial, GeoClue2, gpsd — drops in behind this
//! same API without any caller changing.
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
type Slot = Arc<Mutex<Option<Result<GpsFix, String>>>>;

/// A live subscription to the device's position. Drop it to unsubscribe.
pub struct LocationWatch {
    slot: Slot,
    #[cfg(target_arch = "wasm32")]
    inner: Option<wasm::Watch>,
}

impl LocationWatch {
    /// Whether this build has a provider that can produce a fix at all. `false`
    /// on native today — see the module note.
    #[must_use]
    pub const fn available() -> bool {
        cfg!(target_arch = "wasm32")
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
        #[cfg(not(target_arch = "wasm32"))]
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
    }
}

#[cfg(target_arch = "wasm32")]
mod wasm {
    use super::{GpsFix, Slot};
    use wasm_bindgen::prelude::Closure;
    use wasm_bindgen::JsCast;

    /// The `watchPosition` id plus the two callbacks it invokes. The closures must
    /// outlive the watch — dropping one and then having the browser call it is a
    /// use-after-free — so they are owned here and released together in [`stop`].
    pub struct Watch {
        id: i32,
        _on_fix: Closure<dyn FnMut(web_sys::GeolocationPosition)>,
        _on_err: Closure<dyn FnMut(web_sys::GeolocationPositionError)>,
    }

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

        match geo.watch_position_with_error_callback_and_options(
            on_fix.as_ref().unchecked_ref(),
            Some(on_err.as_ref().unchecked_ref()),
            &opts,
        ) {
            Ok(id) => Some(Watch {
                id,
                _on_fix: on_fix,
                _on_err: on_err,
            }),
            Err(e) => {
                tracing::warn!(error = ?e, "could not start watchPosition");
                None
            }
        }
    }

    pub fn stop(watch: Watch) {
        if let Some(geo) = web_sys::window()
            .map(|w| w.navigator())
            .and_then(|n| n.geolocation().ok())
        {
            geo.clear_watch(watch.id);
        }
    }
}
