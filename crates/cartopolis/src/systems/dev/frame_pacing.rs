//! Frame pacing: the ADPF report to Android's performance governor, the
//! presentation-rate limiter, the frame-time stats, the slow-frame watchdog and
//! the screen wakelock. Moved out of the crate root on 2026-08-19 for the same
//! reason as `systems::sky` — see that module's note. Registration stays in
//! `lib.rs`.

use bevy::prelude::*;
use big_space::prelude::GridCell;
use std::time::Duration;

use crate::env_flag;
use crate::env_value;
use crate::platform;
// Native only: the one reader left off the handset is `frame_limiter`, whose
// body is compiled out on the web.
#[cfg(not(target_arch = "wasm32"))]
use crate::quality;
use crate::resources;
use crate::systems;

// ---------------------------------------------------------------------------
// The presentation rate: what we ask to present at, and when we ask for less
// ---------------------------------------------------------------------------

/// The rate the app wants to present at right now, in fps; `0.0` means "no cap,
/// the platform decides".
///
/// **One resolution of the question with two consumers**, which is why it is a
/// resource and not a `Local` in each. On Android [`present_rate`] hands it to the
/// compositor — the only pacer that does not fight FIFO — and on a machine that
/// presents without a vsync [`frame_limiter`] sleeps to it. A version where those
/// two resolve it separately is one where the idle throttle applies on one
/// platform and not the other, with nothing to say so.
///
/// Nothing consumes it on the web: `requestAnimationFrame` is the pacer there and
/// there is no lever to pull. It is still computed, so the value a diagnostics
/// report quotes is the same one everywhere.
#[derive(Resource, Debug, Default)]
pub(crate) struct PresentationRate {
    /// What we want — the tier's target, or the idle rate. Set by [`present_rate`],
    /// which is chained ahead of both consumers, so the zero this defaults to is
    /// never read.
    pub wanted: f32,
    /// Whether a person picked a rate in Settings ▸ Graphics. A swapchain that
    /// paces itself is left alone by [`frame_limiter`] *unless* this is set: a
    /// display can round a rate request up to a mode it prefers, and "30" that
    /// quietly runs at 60 is not the setting somebody chose to save battery.
    pub chosen: bool,
    /// What the compositor last accepted, on Android. `None` until the surface
    /// exists — `native_window()` answers `None` for the first few seconds after
    /// `android_main`, which is what [`report_frame_to_scheduler`] waits for
    /// before opening its ADPF session.
    ///
    /// Both the write and the read are inside `cfg(target_os = "android")` blocks,
    /// so off the handset this is a field nothing touches. Kept unconditional
    /// rather than `cfg`-ed away, for the same reason the systems that use it keep
    /// one signature on every target: a second shape of the resource is one more
    /// thing to keep in step, and `wanted` is read everywhere.
    #[cfg_attr(not(target_os = "android"), allow(dead_code))]
    applied: Option<f32>,
}

/// How long everything must be quiet before the rate drops.
///
/// Long enough that the pause between two drags is not idle, short enough that
/// reading the map is. **A guess** — it has never been felt on a device, and it is
/// the first number to move if the drop is noticeable.
const IDLE_AFTER_S: f32 = 2.0;

/// The floor the idle throttle will not go below.
///
/// The scene is never actually static — the sky, the water and the day/night
/// cycle animate whatever the camera does — so this is a *rate* to fall back to
/// and not a stop. Below about this it stops reading as a still picture and starts
/// reading as a broken one.
const IDLE_FPS_FLOOR: f32 = 30.0;

/// The rate to ask for, given the tier's target and any `CARTO_FPS_CAP` override.
///
/// `0.0` is "the platform decides": both what `ANativeWindow_setFrameRate`
/// documents for a zero and what [`frame_limiter`] has always read as "no cap". So
/// `CARTO_FPS_CAP=0` still means uncapped, on the platform where lifting the cap is
/// how headroom gets measured at all.
fn resolve_frame_rate(override_fps: Option<f32>, target_fps: f32) -> f32 {
    match override_fps {
        Some(fps) if fps > 0.0 => fps,
        // An explicit zero (or a negative typo) is the debugging knob asking for
        // no cap, and a knob has to win over a default to be worth having.
        Some(_) => 0.0,
        None => target_fps.max(0.0),
    }
}

/// The idle rate for an active rate.
///
/// Half, floored at [`IDLE_FPS_FLOOR`], and **never applied to a run that asked
/// for no cap at all**: lifting the cap is how somebody measures what this device
/// can do, and an idle throttle underneath that measurement would be measuring
/// itself.
fn idle_frame_rate(active_fps: f32) -> f32 {
    if active_fps <= 0.0 {
        return 0.0;
    }
    (active_fps * 0.5).max(IDLE_FPS_FLOOR).min(active_fps)
}

/// `CARTO_FPS_CAP`, parsed once.
///
/// Through [`env_value`], not `std::env::var`: on Android the former maps to the
/// `debug.carto.*` system properties and the latter reads an environment the
/// zygote never gave us. Cached because this is now consulted every frame rather
/// than once, and on Android each read is a `__system_property_get`.
fn cap_override() -> Option<f32> {
    static CAP: std::sync::OnceLock<Option<f32>> = std::sync::OnceLock::new();
    *CAP.get_or_init(|| env_value("CARTO_FPS_CAP").and_then(|v| v.parse().ok()))
}

/// What [`present_rate`] remembers between frames to tell moving from still.
#[derive(Default)]
pub(crate) struct IdleWatch {
    quiet_s: f32,
    camera: Option<(Vec3, Quat)>,
}

/// Ask to present at the tier's rate, and at half of it while nothing is happening.
///
/// **Why this is the lever and a sleep is not.** Android runs `AutoVsync`, so the
/// swapchain is already a clock; a `thread::sleep` on top is a second one timing
/// from the other end of the pipeline, which is the alternating judder
/// [`frame_limiter`] refuses to cause (see its body). The compositor, though, is
/// the *same* clock — telling it to present every 33 ms throttles the whole loop,
/// because FIFO then blocks on acquire for exactly that long. So the one call that
/// can lower the rate without adding a pacer is `ANativeWindow_setFrameRate`, and
/// it is `dlsym`ed out of `libandroid.so` like the rest of this file's NDK calls,
/// needing no `classes.dex`.
///
/// **What counts as busy.** Input held or just released, the camera having moved,
/// driver mode, and either streaming signal — bytes handed to the GPU this frame,
/// or a tile job still outstanding. The streamers are in that list deliberately:
/// the per-frame build budgets are *per frame*, so a cold map filling in while you
/// sit still would otherwise take twice as long to arrive.
///
/// **What does not count**, and this is the deliberate trade: the world keeps
/// animating while idle — the sun moves, the water has waves, a remote avatar
/// walks past — and it animates at the idle rate. A still camera over a moving
/// scene at 30 fps is the cost of not cooking the device to render a view nobody
/// is touching.
#[cfg_attr(
    not(target_os = "android"),
    allow(unused_variables, clippy::needless_pass_by_value)
)]
pub(crate) fn present_rate(
    time: Res<Time<Real>>,
    keys: Res<ButtonInput<KeyCode>>,
    mouse: Res<ButtonInput<MouseButton>>,
    touches: Res<Touches>,
    uploads: Res<systems::upload_meter::UploadMeter>,
    driver: Res<systems::driver::DriverState>,
    camera: Query<&Transform, With<crate::components::FreeCamera>>,
    prefs: Res<crate::resources::AppPrefs>,
    mut rate: ResMut<PresentationRate>,
    mut watch: Local<IdleWatch>,
) {
    // Compared against the last frame's rather than read from a change detection
    // flag: under `big_space` the camera's `Transform` is re-based whenever the
    // origin moves a grid cell, and half the systems in the app touch it. An
    // epsilon, because a smoothed follow camera never comes exactly to rest and an
    // exact compare would report "moving" for the rest of the session.
    let now = camera.single().ok().map(|tf| (tf.translation, tf.rotation));
    let moved = match (watch.camera, now) {
        (Some(was), Some(is)) => {
            was.0.distance_squared(is.0) > 1.0e-6 || was.1.angle_between(is.1) > 1.0e-4
        }
        // No camera yet, or it just appeared: not a resting scene.
        _ => true,
    };
    watch.camera = now;

    let busy = moved
        || driver.active
        || uploads.frame_bytes > 0
        || platform::workers::outstanding() > 0
        || keys.get_pressed().next().is_some()
        || keys.get_just_released().next().is_some()
        || mouse.get_pressed().next().is_some()
        || mouse.get_just_released().next().is_some()
        || touches.iter().next().is_some()
        || touches.iter_just_released().next().is_some();

    watch.quiet_s = if busy {
        0.0
    } else {
        watch.quiet_s + time.delta_secs()
    };

    let active = resolve_frame_rate(cap_override(), prefs.target_fps());
    let wanted = if watch.quiet_s >= IDLE_AFTER_S {
        idle_frame_rate(active)
    } else {
        active
    };
    rate.wanted = wanted;
    rate.chosen = prefs.frame_rate.is_some_and(|fps| fps > 0);

    #[cfg(target_os = "android")]
    if rate.applied != Some(wanted) {
        let first = rate.applied.is_none();
        if platform::android::set_frame_rate(wanted) {
            rate.applied = Some(wanted);
            // The first ask at `info`, so a session log says what rate this device
            // was asked for; the transitions at `debug`, because there is one every
            // time you stop and start panning and an hour of that is not a log.
            if first {
                tracing::info!(fps = wanted, "asked the compositor to present at this rate");
            } else {
                tracing::debug!(
                    fps = wanted,
                    idle = wanted < active,
                    "presentation rate changed"
                );
            }
        }
    }
}

#[cfg(test)]
mod rate_tests {
    use super::{idle_frame_rate, resolve_frame_rate, IDLE_FPS_FLOOR};

    /// **The regression this exists for.** `report_frame_to_scheduler` used to pass
    /// a hardcoded `0.0` — `ANativeWindow_setFrameRate`'s "the app will accept the
    /// system's choice" — which on a 120 Hz LTPO handset means 120, i.e. twice the
    /// frames the tier budgeted for, paid in heat.
    ///
    /// The rung's own test (`quality::every_rung_asks_the_compositor_for_a_rate`)
    /// could not catch it: it asserts what `target_fps()` *returns*, and the bug was
    /// that nothing passed the return value on. This asserts the resolution the call
    /// site actually performs, which is why the resolution is a function and not
    /// three lines inside a `cfg(target_os = "android")` block.
    #[test]
    fn the_tier_s_target_is_what_gets_asked_for() {
        assert_eq!(resolve_frame_rate(None, 60.0), 60.0);
        assert_eq!(resolve_frame_rate(None, 90.0), 90.0);
    }

    /// `CARTO_FPS_CAP` still wins, on the platform where it is a `debug.carto.*`
    /// property and therefore the only way to A/B a rate without a rebuild.
    #[test]
    fn the_debug_cap_overrides_the_tier() {
        assert_eq!(resolve_frame_rate(Some(120.0), 60.0), 120.0);
    }

    /// …and zero still means "no cap", which is how headroom gets measured.
    #[test]
    fn lifting_the_cap_hands_the_choice_back() {
        assert_eq!(resolve_frame_rate(Some(0.0), 60.0), 0.0);
    }

    /// Idle halves the rate.
    #[test]
    fn idle_halves_the_rate() {
        assert_eq!(idle_frame_rate(60.0), 30.0);
        assert_eq!(idle_frame_rate(120.0), 60.0);
    }

    /// But never below the floor, and never *above* the active rate — a tier
    /// already at the floor must not be throttled up into asking for more.
    #[test]
    fn the_idle_rate_is_bounded_at_both_ends() {
        assert_eq!(idle_frame_rate(IDLE_FPS_FLOOR), IDLE_FPS_FLOOR);
        assert_eq!(idle_frame_rate(24.0), 24.0);
    }

    /// An uncapped run is left alone: the idle throttle must not quietly become the
    /// cap that `CARTO_FPS_CAP=0` was set to remove.
    #[test]
    fn an_uncapped_run_is_not_idle_throttled() {
        assert_eq!(idle_frame_rate(0.0), 0.0);
    }
}

/// Drives the sun direction, sun light intensity, and ambient brightness from the dev
/// panel's time-of-day slider (Ctrl+Shift+D). Runs only when the value changes.
/// Warn when a frame's CPU time climbs toward the GPU command-buffer timeout.
///
/// The `VK_ERROR_DEVICE_LOST` crash gives no warning of its own — the driver just
/// kills the device once a command buffer overruns (~1–2 s on Mesa/Intel). When
/// the GPU falls behind, its backpressure stalls the CPU frame too, so a run of
/// climbing frame times (`slow frame` at 250 ms → 600 ms → …) is the visible
/// warning that a hang is imminent. Watch these lines in the log to see *when*
/// (which altitude / what just streamed) the GPU starts choking.
/// Cap the presentation rate (default 60 fps; `CARTO_FPS_CAP=0` disables).
///
/// The window deliberately runs `PresentMode::AutoNoVsync` — on Wayland, FIFO's
/// acquire blocks ~1 s per frame while the window is occluded and eventually trips
/// i915's hang detection into `VK_ERROR_DEVICE_LOST` (see the `present_mode`
/// comment). That fix is load-bearing, but it also means the app renders as fast as
/// the iGPU will go on hardware this code documents as living near its
/// command-buffer timeout. Sleeping in `Last` to a fixed frame deadline keeps the
/// Wayland behaviour (we never block on acquire) while roughly halving GPU *and*
/// CPU work at identical image quality.
///
/// Sleeping — not spinning: the point is to leave the GPU idle, so burning a core
/// for sub-millisecond deadline precision would defeat it.
/// Tell Android's performance governor how long the frame actually took.
///
/// Ordered **before** `frame_limiter`, and that is the whole subtlety: the limiter
/// sleeps to hit a deadline, so measuring after it would report the sleep as work
/// and describe a process that is always exactly on time no matter how slow it is.
/// `Time<Real>::delta` at this point is the previous frame's wall clock, which is
/// what ADPF wants — it compares it against the target and moves the clocks.
///
/// `Time<Real>`, not `Time`: the virtual clock is clamped (and `--shot` runs it at
/// a fixed step), so it measures intent rather than elapsed time.
///
/// A no-op off Android, where the session never opens. See
/// `platform::android::start_perf_hint` for why this is reachable without a
/// `classes.dex` and why it is `dlsym`ed rather than linked.
// Off Android the body below is `cfg`-ed away entirely, so both parameters become
// unused and `mut` becomes needless — which the workspace's `unused = "deny"` turns
// into two errors. The signature has to stay identical on every target regardless,
// because it is a Bevy system and the registration is not `cfg`-ed: a second
// registration arm would be one more thing to keep in step for a system that does
// nothing off the handset.
#[cfg_attr(
    not(target_os = "android"),
    allow(unused_variables, unused_mut, clippy::needless_pass_by_value)
)]
pub(crate) fn report_frame_to_scheduler(
    time: Res<Time<Real>>,
    rate: Res<PresentationRate>,
    mut started: Local<bool>,
    mut cadence: Local<Cadence>,
) {
    #[cfg(target_os = "android")]
    {
        if !*started && rate.applied.is_some() {
            // **The rate we ask for and the deadline we report against are two
            // different numbers, and conflating them is the bug this had twice.**
            //
            // Asking is [`present_rate`]'s job now. It used to be done here, with a
            // hardcoded `0.0` — `ANativeWindow_setFrameRate`'s "the app will accept
            // the system's choice of display frame rate" — on the argument that the
            // compositor is the only party that knows the panel's modes. True, and
            // the consequence is that a 120 Hz LTPO handset renders every frame the
            // panel can take: twice the tier's budget, spent as heat on a device
            // that then thermally throttles. Handing the choice back is not a
            // neutral act when the choice is "as fast as possible".
            //
            // The earlier version of *that* argument was right about the number
            // being unaccountable, though, and it still is: 60 is not a property of
            // any display. It is not offered as one. `target_fps` is a statement
            // about how much work this tier is willing to do per second, which is a
            // thing this codebase does know, and the panel's own rate is measured
            // rather than assumed (see [`Cadence`]).
            //
            // So ADPF gets a deadline that is the one we are *actually* being paced
            // to. Seeded here from what the compositor accepted — a hint aimed at
            // 16.6 ms on a panel presenting every 8.3 ms reports every frame as
            // comfortably early and the governor never boosts, which is the bug in
            // the other direction — and replaced by `measured_period_ns` within a
            // second or so.
            //
            // Gated on `rate.applied` rather than probing the surface again:
            // `native_window()` is `None` for the first few seconds on Android
            // (winit logs `Creating new window` about five seconds after
            // `android_main`), and `present_rate` is already retrying against it
            // every frame. Two callers racing to be the one that notices the
            // surface is one caller too many.
            let seed_fps = rate
                .applied
                .filter(|fps| *fps > 0.0)
                .unwrap_or_else(|| quality::budget().target_fps());
            let period = (1.0e9 / f64::from(seed_fps)) as i64;
            *started = true;
            platform::android::start_perf_hint(period);
            platform::android::set_perf_hint_target(period);
        }
        if let Some(period) = cadence.observe(time.delta()) {
            tracing::info!(
                hz = 1.0e9 / period as f64,
                "display cadence measured — re-aiming the performance hint"
            );
            platform::android::set_perf_hint_target(period);
        }
        platform::android::report_frame(time.delta().as_nanos() as i64);
    }
}

/// How often the display actually presents, measured from the frames it gives us.
///
/// **The panel's rate is not readable on the platform that needs it.** winit's
/// Android backend returns `None` from `refresh_rate_millihertz` and fabricates a
/// 60 in `video_modes` (see `quality::Capabilities`), so bevy's `Monitor` cannot
/// answer, and there is no NDK call for it either. But it can be *observed*: under
/// a vsync-paced swapchain a frame can never be presented faster than the panel
/// refreshes, so the shortest interval seen over a window of frames **is** the
/// panel period.
///
/// The minimum rather than the mean, and that is the whole point. A mean is the
/// rate we *achieved*, which is a statement about our own frame cost — and it is
/// precisely what ADPF exists to compare against the deadline, so feeding it back
/// in as the deadline makes the governor's test vacuous. The minimum is a property
/// of the display.
///
/// Re-measured every window rather than latched, because an LTPO panel changes
/// mode underneath a running app; the report is throttled to changes that actually
/// matter so a jittery window does not have the governor chasing itself.
#[derive(Default)]
pub(crate) struct Cadence {
    shortest_ns: i64,
    frames: u32,
    reported_ns: i64,
}

impl Cadence {
    /// Frames per measurement window. Two seconds at 60 Hz, one at 120 — long
    /// enough that a single hitch cannot set the minimum, short enough to follow a
    /// mode switch.
    const WINDOW: u32 = 120;
    /// A panel that presented faster than this is not a panel; it is a frame bevy
    /// clamped, a resumed app catching up, or a zero delta on the first frame.
    const FLOOR_NS: i64 = 1_000_000;

    /// Feed one frame's wall-clock delta; answers `Some(period_ns)` on the frames
    /// where the measurement is both complete and materially different from the
    /// last one reported.
    #[cfg_attr(not(target_os = "android"), allow(dead_code))]
    fn observe(&mut self, delta: Duration) -> Option<i64> {
        let delta = i64::try_from(delta.as_nanos()).unwrap_or(i64::MAX);
        if delta >= Self::FLOOR_NS {
            self.shortest_ns = if self.shortest_ns == 0 {
                delta
            } else {
                self.shortest_ns.min(delta)
            };
        }
        self.frames += 1;
        if self.frames < Self::WINDOW {
            return None;
        }
        self.frames = 0;
        let measured = std::mem::take(&mut self.shortest_ns);
        if measured == 0 {
            return None;
        }
        // Within an eighth of the last answer is the same answer. Without this a
        // window whose minimum lands one vsync out re-aims the governor every two
        // seconds, forever.
        if (measured - self.reported_ns).abs() * 8 < self.reported_ns {
            return None;
        }
        self.reported_ns = measured;
        Some(measured)
    }
}

#[cfg(test)]
mod cadence_tests {
    use super::Cadence;
    use std::time::Duration;

    fn run(c: &mut Cadence, frames: u32, ms: f64) -> Option<i64> {
        let mut last = None;
        for _ in 0..frames {
            if let Some(p) = c.observe(Duration::from_secs_f64(ms / 1000.0)) {
                last = Some(p);
            }
        }
        last
    }

    /// A 120 Hz panel is measured as 120, not as the 60 the code used to assume.
    #[test]
    fn a_steady_panel_is_measured_at_its_own_rate() {
        let mut c = Cadence::default();
        let period = run(&mut c, Cadence::WINDOW, 1000.0 / 120.0).expect("a full window answers");
        assert!(
            (period - 8_333_333).abs() < 100_000,
            "measured {period} ns, expected ~8.33 ms"
        );
    }

    /// The *minimum*, so our own slow frames never masquerade as the display's
    /// rate. A run that mostly renders at 30 fps on a 120 Hz panel is still being
    /// presented every 8.3 ms — reporting 33 ms would tell the governor the app is
    /// meeting a deadline it is in fact missing by four vsyncs.
    #[test]
    fn a_slow_app_does_not_slow_the_measured_display() {
        let mut c = Cadence::default();
        let mut reported = None;
        for i in 0..Cadence::WINDOW {
            // One frame in four makes the vsync; the rest miss three of them.
            let ms = if i % 4 == 0 { 1000.0 / 120.0 } else { 33.0 };
            if let Some(p) = c.observe(Duration::from_secs_f64(ms / 1000.0)) {
                reported = Some(p);
            }
        }
        let period = reported.expect("a full window answers");
        assert!(
            period < 12_000_000,
            "measured {period} ns, expected ~8.33 ms"
        );
    }

    /// Nothing is reported until a window is complete, and an unchanged answer is
    /// not re-reported — a governor that is re-aimed every two seconds forever is
    /// worse than one aimed once.
    #[test]
    fn a_stable_rate_is_reported_once() {
        let mut c = Cadence::default();
        assert!(run(&mut c, Cadence::WINDOW - 1, 1000.0 / 60.0).is_none());
        assert!(run(&mut c, 1, 1000.0 / 60.0).is_some());
        assert!(run(&mut c, Cadence::WINDOW * 3, 1000.0 / 60.0).is_none());
    }

    /// …but a real mode switch is followed. An LTPO panel drops to 60 on its own,
    /// and a deadline still aimed at 8.3 ms reports every frame as late.
    #[test]
    fn a_mode_switch_is_followed() {
        let mut c = Cadence::default();
        run(&mut c, Cadence::WINDOW, 1000.0 / 120.0).expect("the first answer");
        let period = run(&mut c, Cadence::WINDOW, 1000.0 / 60.0).expect("the panel changed mode");
        assert!(
            (period - 16_666_666).abs() < 200_000,
            "measured {period} ns, expected ~16.7 ms"
        );
    }

    /// A zero delta is bevy's first frame, not a 1000 Hz display.
    #[test]
    fn an_impossible_frame_is_not_a_display_rate() {
        let mut c = Cadence::default();
        for i in 0..Cadence::WINDOW {
            let d = if i == 7 {
                Duration::ZERO
            } else {
                Duration::from_secs_f64(1.0 / 60.0)
            };
            c.observe(d);
        }
        // The window closed inside the loop; the answer it produced is the 60, and
        // reporting it again here would mean the zero had won.
        assert!(run(&mut c, Cadence::WINDOW, 1000.0 / 60.0).is_none());
    }
}

pub(crate) fn frame_limiter(
    rate: Res<PresentationRate>,
    mut deadline: Local<Option<std::time::Instant>>,
    mut period: Local<Duration>,
) {
    // Native only: a `thread::sleep`-based FPS cap. The browser paces frames itself
    // (requestAnimationFrame), and `Instant::now()`/`thread::sleep` would panic on
    // wasm, so this is a no-op there.
    #[cfg(target_arch = "wasm32")]
    let _ = (&rate, &mut deadline, &mut period);
    #[cfg(not(target_arch = "wasm32"))]
    {
        // **Nothing paces a swapchain that already paces itself.**
        //
        // FIFO blocks on acquire, so it is a clock; a `thread::sleep` to a deadline
        // measured at the *other* end of the pipeline is a second clock, and two
        // clocks beat against each other — alternating judder on a device with
        // headroom to spare, plus a main thread that looks idle to Linux EAS at
        // exactly the moment the tile workers look busy.
        //
        // This was a `Budget::fps_cap` of `None` on the phone rung, with the whole
        // argument written on the field. Then Android moved to the balanced rung,
        // which carries `Some(60.0)`, and the guard silently stopped applying to the
        // only platform it was written for — a 120 Hz handset ran a sleep limiter
        // under FIFO. **The rule was attached to a tier, and a tier is a judgement
        // about a class of machine; this is a property of the surface in front of
        // us.** So it is asked of the window (`quality::Capabilities::vsync_paced`)
        // and the tier cannot lose it again.
        //
        // `None` — before `setup_camera` has run — falls through to the tier, which
        // is right: those are the first frames, there is no swapchain yet, and the
        // conservative answer is the one that caps.
        //
        // The one exception is a rate a person picked (`PresentationRate::chosen`):
        // the rate hint is a request the display may round up, and a battery-saving
        // 30 that runs at 60 is a setting that does nothing. Below the display's
        // rate the sleep lands frames on every other vsync; at or above it the
        // deadline is always already past and it sleeps nothing.
        if !rate.chosen && quality::capabilities().is_some_and(|c| c.vsync_paced) {
            return;
        }
        // The rate comes from `PresentationRate` now rather than being resolved
        // once into a `Local`, because it moves: `present_rate` halves it while
        // nothing is happening. The tier's `fps_cap` and `CARTO_FPS_CAP` are folded
        // in there, so this end of the pipeline no longer has an opinion about
        // either — one resolution, two consumers.
        if rate.wanted <= 0.0 {
            return;
        }
        let wanted = Duration::from_secs_f64(1.0 / f64::from(rate.wanted));
        if wanted != *period {
            // The period changed underneath us. A deadline computed from the old
            // one can be an entire idle frame away, so honouring it would sleep
            // through the input that just ended the idle — re-base instead.
            *period = wanted;
            *deadline = None;
        }
        let period = wanted;
        let now = std::time::Instant::now();
        // A deadline in the past means we're already running below the cap (a hitch,
        // or a genuinely heavy frame). Re-base on `now` rather than sleeping zero for
        // a burst of frames trying to "catch up" to a schedule we can't meet.
        let target = deadline.filter(|d| *d > now).unwrap_or(now);
        if let Some(wait) = target.checked_duration_since(now) {
            std::thread::sleep(wait);
        }
        *deadline = Some(target + period);
    }
}

/// Opt-in per-second frame stats (`CARTO_FPS_LOG=1`): sustained FPS, worst
/// frame this second, and how much the frame time varies (jitter). Motion that
/// reads as "stutter" is usually low or jittery FPS, not the movement code — this
/// tells them apart. The `dev` profile already builds Bevy at `opt-level=3`, so
/// a plain `cargo run` is playable (see CLAUDE.md).
pub(crate) fn frame_stats(
    time: Res<Time<Real>>,
    // (elapsed_s, frames, worst_ms, sum_ms, sum_ms_sq)
    mut acc: Local<(f32, u32, f32, f32, f32)>,
    mut enabled: Local<Option<bool>>,
    mut stats: ResMut<crate::resources::FrameStats>,
) {
    // Through `env_flag`, not `std::env` directly, so `?fps_log` reaches it on the
    // web — where a number for "how slow is slow" is harder to come by than
    // anywhere else, there being no external profiler to point at the tab.
    //
    // **The flag gates the log line, not the accumulation.** Five adds and a max
    // per frame cost nothing measurable, and the resource they feed is the only
    // frame time a handset can report: there is no `adb` on the phone this client
    // targets, so `systems::dev::diagnostics` — a report the user taps and sends
    // — has to carry the number, and it cannot if the accumulator is switched off
    // by an environment variable that platform does not have.
    let logging = *enabled.get_or_insert_with(|| env_flag("CARTO_FPS_LOG"));
    let ms = time.delta_secs() * 1000.0;
    acc.0 += time.delta_secs();
    acc.1 += 1;
    acc.2 = acc.2.max(ms);
    acc.3 += ms;
    acc.4 += ms * ms;
    if acc.0 >= 1.0 {
        let n = acc.1.max(1) as f32;
        let mean = acc.3 / n;
        let var = (acc.4 / n - mean * mean).max(0.0);
        let fps = (acc.1 as f32 / acc.0) as u32;
        *stats = crate::resources::FrameStats {
            fps,
            mean_ms: mean,
            worst_ms: acc.2,
            jitter_ms: var.sqrt(),
            sampled: true,
        };
        if logging {
            tracing::info!(
                fps,
                mean_frame_ms = mean as u32,
                worst_frame_ms = acc.2 as u32,
                jitter_ms = var.sqrt() as u32,
                "frame stats"
            );
        }
        *acc = (0.0, 0, 0.0, 0.0, 0.0);
    }
}

pub(crate) fn frame_time_watchdog(
    time: Res<Time<Real>>,
    globe_meshes: Query<(), With<crate::components::GlobeMesh>>,
    meshes: Res<Assets<Mesh>>,
    images: Res<Assets<Image>>,
    materials: Res<Assets<StandardMaterial>>,
    uploads: Res<systems::upload_meter::UploadMeter>,
    globe: Res<systems::globe::GlobeState>,
    camera: Query<(&GridCell, &Transform), With<crate::components::FreeCamera>>,
    mut census: Local<f32>,
) {
    // NB: RenderDiagnosticsPlugin (per-pass GPU timestamp queries) was tried here
    // and measured the instrumented passes at ~7 ms during 1 s frames — useful
    // once, but its own GPU buffer readback panicked on this Mesa/Intel driver
    // (`Buffer::get_mapped_range` in the task pools, no device loss preceding),
    // crashing the app at startup. Don't re-add it on this hardware.
    let ms = time.delta_secs() * 1000.0;
    if ms > 250.0 {
        let altitude_m = camera
            .single()
            .map(|(cell, tf)| globe.altitude_m(cell, tf))
            .unwrap_or(f64::NAN);
        tracing::warn!(
            frame_ms = ms as u32,
            altitude_m = altitude_m as i64,
            globe_tiles = globe_meshes.iter().count(),
            meshes = meshes.len(),
            images = images.len(),
            materials = materials.len(),
            // The counts above say how much exists; these say how much was handed
            // over *this frame*, which is the term that decides a command-buffer
            // timeout and the one every previous report had to guess at.
            upload_kb = uploads.frame_kb(),
            upload_peak_kb = uploads.peak_kb(),
            upload_peak_asset_kb = uploads.peak_asset_kb(),
            // …and this is what all of it adds up to and has not been given back.
            // A hitch with a quiet `upload_kb` and a large `resident_mb` is an
            // allocation problem, which no per-frame throttle can reach.
            resident_mb = uploads.resident_kb() / 1024,
            "slow frame — GPU may be nearing its command-buffer timeout (TDR)"
        );
    }
    // Periodic asset census: if these climb without bound while flying around,
    // a streaming path is leaking GPU resources — the other way this hardware
    // reaches device-loss/allocation failure besides one-frame upload bursts.
    *census += time.delta_secs();
    if *census >= 10.0 {
        *census = 0.0;
        tracing::info!(
            meshes = meshes.len(),
            images = images.len(),
            materials = materials.len(),
            // The worst frame of the last ten seconds' worth of streaming, so a
            // burst that happened between two hitches still reaches the log.
            upload_peak_kb = uploads.peak_kb(),
            upload_peak_asset_kb = uploads.peak_asset_kb(),
            upload_total_mb = uploads.total_bytes / (1024 * 1024),
            // The counts say how many objects exist; these say what they weigh,
            // which is the quantity a driver refuses. `upload_total_mb` above is
            // everything ever handed over — it climbs forever and says nothing
            // about what is still held.
            resident_mb = uploads.resident_kb() / 1024,
            peak_resident_mb = uploads.peak_resident_kb() / 1024,
            "asset census"
        );
    }
}

/// Render-cost census: triangle counts by layer, plus the **shadow-caster**
/// triangle load — the costs that mesh/image/material counts hide (a static scene
/// with 26 meshes still hung the GPU). Shadow-caster triangles are re-rasterized
/// once per shadow cascade — the sun's, and since 2026-09-11 only the sun's — so
/// `ktris_shadow` weighs heavier than its raw value and is usually what pins this
/// iGPU; `draws` is the visible mesh count. Read `shadow_casters` beside it: a
/// caster whose mesh is `RENDER_WORLD`-only contributes no triangles here and
/// costs a pass anyway. Watch these against `frame_ms` to see what "too much" is.
/// Hold the screen awake while navigating, and let the user say no.
///
/// The window flag is derived from state rather than set at the two ends of
/// driver mode, which is what makes the Settings toggle mean anything: turning
/// it off mid-drive has to release the lock, and `enter`/`exit` cannot observe a
/// preference changing while they are not being called. Deriving it also removes
/// the pairing bug — there is no path that sets the flag and forgets to clear it,
/// because nothing sets it, this only ever reports the current answer.
///
/// Edge-triggered on the answer, not the inputs: the platform call is a binder
/// round-trip to `PowerManagerService`, and making it every frame for an unchanged
/// value would be silly. The wake lock is deliberately *uncounted* on the other
/// side, so this edge-triggering is a cost saving and not a correctness
/// requirement — see `platform::android::new_wake_lock`.
pub(crate) fn apply_screen_wakelock(
    driver: Res<systems::driver::DriverState>,
    prefs: Res<resources::AppPrefs>,
    mut held: Local<bool>,
) {
    let want = driver.active && prefs.keep_screen_on;
    if want == *held {
        return;
    }
    // Not while a permission dialog is up. `*held` is deliberately left alone, so
    // this is a *deferral* and not a skip: the next frame asks again, and the lock
    // lands as soon as the dialog is gone. The reason is
    // `platform::android::PERMISSION_DIALOG_UP` — driver mode raises the dialog and
    // wants the wakelock on the same frame, and asking the framework to light the
    // screen while it is putting a dialog on it is a fight over the same surface.
    #[cfg(target_os = "android")]
    if platform::android::permission_dialog_up() {
        return;
    }
    *held = want;
    #[cfg(target_os = "android")]
    platform::android::set_keep_screen_on(want);
}

/// Tell the watchdog a frame finished. See `platform::watchdog`.
pub(crate) fn watchdog_beat() {
    platform::watchdog::beat();
}
