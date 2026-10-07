//! The app leaving the screen and coming back: park the map work, tell the sim
//! whether anybody is looking, and release the fingers Android forgot.
//! Registered by `lib.rs`'s `ShellPlugin`.

use bevy::prelude::*;

#[cfg(target_os = "android")]
use crate::crash::run_breadcrumb_path;
use crate::{platform, resources, systems};

/// Stop doing map work while the app is in the background.
///
/// Nothing else notices being backgrounded: the tile streamers keep fetching and
/// the worker pool keeps rasterising, on a device whose battery the user is not
/// looking at. `workers::set_paused` already exists for the shot harness and
/// *parks* jobs rather than dropping them, which is the property that matters —
/// the streamers de-duplicate on "a fetch is in flight", so a discarded decode is
/// never re-queued and the tile is simply missing forever afterwards.
///
/// This is a no-op on desktop, where `AppLifecycle` never leaves `Running`.
pub fn quiesce_while_backgrounded(
    mut lifecycle: MessageReader<bevy::window::AppLifecycle>,
    mut exits: MessageReader<AppExit>,
    mut touches: ResMut<bevy::input::touch::Touches>,
    session: Option<Res<resources::OnlineSession>>,
    mut session_state: Option<ResMut<systems::session::SessionState>>,
) {
    // Leaving on purpose — suspended or exiting — is not a crash, so the
    // breadcrumb goes. What it must survive is the app dying on screen.
    #[cfg(target_os = "android")]
    if exits.read().next().is_some() {
        let _ = std::fs::remove_file(run_breadcrumb_path());
    }
    #[cfg(not(target_os = "android"))]
    let _ = exits.read().next();
    for event in lifecycle.read() {
        // Logged, and not only for tidiness: on the phone this is the one record
        // of the app having left the screen and come back. Anything that puts a
        // system dialog up — the runtime location permission is the first one
        // this client ever asks for — backgrounds the activity and destroys its
        // surface, so "did it suspend, and did it resume" is the first question
        // to ask of a freeze that follows a permission prompt, and the answer
        // is otherwise unrecoverable from a device with no cable attached.
        tracing::info!(?event, "app lifecycle");
        match event {
            bevy::window::AppLifecycle::WillSuspend | bevy::window::AppLifecycle::Suspended => {
                platform::workers::set_paused(true);
                // Tell the sim to stop sending world snapshots (protocol 18).
                // This is the *low-power* half of the background connection: the
                // service in `systems::background` keeps the process alive, and
                // without this it would stay alive receiving thirty full world
                // states a second that nothing draws — a held connection that
                // costs as much as a visible one is not a feature.
                //
                // Sent on suspend rather than gated on the setting, and on every
                // platform: a desktop window that is not on screen has the same
                // nothing to draw. Chat, voice and prim updates keep arriving,
                // which is what makes staying connected worth anything.
                set_presence(session.as_deref(), false);
                #[cfg(target_os = "android")]
                let _ = std::fs::remove_file(run_breadcrumb_path());
            }
            bevy::window::AppLifecycle::WillResume | bevy::window::AppLifecycle::Running => {
                platform::workers::set_paused(false);
                // Every finger that was on the glass when we left is gone, and we
                // will never be told so: android-activity detaches the input queue
                // on pause and drops what is unconsumed, and nothing in Bevy 0.19
                // releases a touch on a lifecycle change. A touch that stays
                // "pressed" across a suspend is not a stale reading, it is a
                // permanently held finger — `TouchGestures` keeps its primary and
                // reads every later finger as the second of a pinch, and
                // `egui_manager::heal_lost_touch` needs this to be the signal that
                // its own held id is dead. Both bridges recover from this one call.
                touches.release_all();
                // The next tick's snapshot is the resynchronisation; there is no
                // catch-up to ask for, because a snapshot is the whole state.
                set_presence(session.as_deref(), true);
                // A session Android took away while we were gone is being got
                // back on a back-off, which may have run out against a network
                // that was down in the background. Coming back is when to try.
                if let Some(state) = session_state.as_deref_mut() {
                    state.retry_now();
                    state.check_link_after_resume();
                }
            }
            bevy::window::AppLifecycle::Idle => {}
        }
    }
}

/// Tell the sim whether this client is being looked at, if there is a sim.
///
/// Offline is the absence of [`resources::OnlineSession`], so this is a no-op
/// there rather than a condition every caller has to remember. Routed through
/// `send` — not the per-frame path — because presence is an *edge*: a dropped
/// "I'm back" is not corrected by the next frame, it is a client left with no
/// world state and no reason to ask again.
fn set_presence(session: Option<&resources::OnlineSession>, active: bool) {
    if let Some(session) = session {
        session.send(
            "presence",
            cartopolis_core::NetMessage::SetPresence {
                request_id: 0,
                active,
            },
        );
    }
}
