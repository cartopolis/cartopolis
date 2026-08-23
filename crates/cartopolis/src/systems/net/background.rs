//! Keeping the connection alive while the app is not on screen.
//!
//! One system, and almost all of it is deciding *when* rather than doing
//! anything: the wire already runs on its own OS thread with its own tokio
//! runtime (`network.rs`), entirely outside Bevy's schedule, so no part of the
//! connection depends on the activity being up. What depends on the activity is
//! the process being allowed to keep running at all — Android freezes a cached
//! process — and the only thing that buys that without a push service is a
//! foreground service with an ongoing notification (`ConnectionService.java`).
//!
//! FCM was rejected as the alternative: it is Google Play services only. See
//! `docs/notes/background-connection.md`.
//!
//! ## The state this reconciles
//!
//! Wanted = the user asked for it **and** there is a session to keep. The second
//! half is not a detail: offline is the absence of [`OnlineSession`], and an
//! offline viewer has no connection to hold open, so a permanent notification
//! over it would be an indicator for nothing. Leaving a session therefore takes
//! the service down without the user touching the setting, and signing back in
//! brings it back.
//!
//! ## Why this is keyed on the setting and not on suspend
//!
//! The obvious shape is "start the service as we go to the background". It is
//! wrong twice. Since Android 12 a `startForegroundService` call from the
//! background throws `ForegroundServiceStartNotAllowedException`, and
//! `AppLifecycle::WillSuspend` is already too late to count as foreground — so
//! the natural trigger is the one that crashes, at the worst moment, as the user
//! leaves. And the permission ask below cannot happen there either: a dialog
//! raised while the activity is going away is a dialog nobody sees.
//!
//! So the service starts when the switch is flipped, with the app plainly in
//! front, and simply keeps running. It costs nothing while the app is visible —
//! the process was staying alive anyway.

// `Ordering` is used only by the Android arm below, and importing it here would
// be an unused-import error on every other target (the workspace denies those).
use std::sync::atomic::AtomicBool;

use bevy::prelude::*;

use crate::resources::{AppPrefs, OnlineSession};

/// Whether a reconcile is already in flight on a worker thread.
///
/// The permission ask blocks for as long as the user takes to answer (up to two
/// minutes — see `platform::android::ensure_permission`), so the work cannot run
/// on the schedule and a second request must not pile up behind the first. A
/// plain flag is enough because the reconcile always applies the *current* want,
/// not the one it was spawned with.
static IN_FLIGHT: AtomicBool = AtomicBool::new(false);

/// Start or stop the background connection service to match the settings.
///
/// Change-gated on the resolved want rather than on `AppPrefs` changing, because
/// `AppPrefs` is rewritten by any settings edit and the session comes and goes on
/// its own — two inputs, one answer, and the answer is what is worth acting on.
pub fn apply_background_connection(
    prefs: Res<AppPrefs>,
    session: Option<Res<OnlineSession>>,
    mut applied: Local<Option<bool>>,
) {
    let want = prefs.background_connection && session.is_some();
    if *applied == Some(want) {
        return;
    }
    // Recorded before the work is attempted, not after it succeeds: a failure
    // that re-fired every frame would ask for the notification permission in a
    // loop. The user can retry by toggling the switch, which is also the only
    // way they would learn anything happened.
    *applied = Some(want);
    reconcile(want);
}

#[cfg(target_os = "android")]
fn reconcile(want: bool) {
    if IN_FLIGHT.swap(true, std::sync::atomic::Ordering::SeqCst) {
        tracing::warn!(want, "background connection: a change is still in flight");
        return;
    }
    std::thread::spawn(move || {
        // POST_NOTIFICATIONS is dangerous from API 33, and the service is
        // obliged to show one. A refusal is **not** fatal and is not treated as
        // one: the service still runs and stays alive, Android simply does not
        // draw its notification. Asking is still right — a silent ongoing
        // notification the user never agreed to is worse than a prompt.
        if want {
            let cancel = AtomicBool::new(false);
            match crate::platform::android::ensure_permission(
                "android.permission.POST_NOTIFICATIONS",
                &cancel,
            ) {
                Ok(true) => {}
                Ok(false) => tracing::warn!(
                    "background connection: notifications refused; the service will run \
                     without a visible indicator"
                ),
                Err(e) => tracing::warn!(error = %e, "background connection: could not ask \
                     for the notification permission"),
            }
        }
        match crate::platform::android::set_background_service(want) {
            Ok(()) => tracing::info!(running = want, "background connection service"),
            Err(e) => tracing::error!(error = %e, want, "background connection service failed"),
        }
        IN_FLIGHT.store(false, std::sync::atomic::Ordering::SeqCst);
    });
}

/// Everywhere else this is a no-op, and deliberately not an error.
///
/// A desktop keeps its socket for as long as the process lives, so the setting is
/// already satisfied there and there is nothing to start. The web cannot do it at
/// all — a wasm client dies with its tab — which is why the switch is hidden
/// rather than disabled on that build (`systems::settings`).
#[cfg(not(target_os = "android"))]
fn reconcile(want: bool) {
    let _ = &IN_FLIGHT;
    tracing::debug!(
        want,
        "background connection: nothing to do on this platform"
    );
}
