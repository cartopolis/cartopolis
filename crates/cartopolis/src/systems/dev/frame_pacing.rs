//! Frame pacing: the ADPF report to Android's performance governor, the
//! presentation-rate limiter, the frame-time stats, the slow-frame watchdog and
//! the screen wakelock. Moved out of the crate root on 2026-08-19 for the same
//! reason as `systems::sky` — see that module's note. Registration stays in
//! `lib.rs`.

use bevy::prelude::*;
use big_space::prelude::GridCell;
use std::time::Duration;

use crate::env_flag;
// The limiter's cap resolution and the vsync-paced probe are native-only code
// paths, so their imports are too — on wasm they are cfg-stripped and `unused =
// "deny"` makes an unconditional import an error.
#[cfg(not(target_arch = "wasm32"))]
use crate::env_value;
use crate::platform;
#[cfg(not(target_arch = "wasm32"))]
use crate::quality;
use crate::resources;
use crate::systems;

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
    mut started: Local<bool>,
    mut cadence: Local<Cadence>,
) {
    #[cfg(target_os = "android")]
    {
        if !*started {
            // **Let the display decide the rate, and aim ADPF at what it decided.**
            //
            // This used to ask for a hard 60, out of a real concern: FIFO paces to
            // *the panel*, so on a 90 or 120 Hz screen a scene budgeted for 30 fps
            // renders two to four times the frames it needs and is thermally
            // throttled for the privilege. The mistake was answering that concern
            // with a number nobody could check. 60 is not a property of any display
            // — it is a property of the desktop the tier table was written on — and
            // on a 120 Hz LTPO handset asking for it is worse than asking for
            // nothing: the panel is pinned to half its rate, and it is pinned there
            // by a constant this codebase could not have derived from anything the
            // device said.
            //
            // A capability cannot be *read* here — winit's Android backend answers
            // `None` for the refresh rate outright and fabricates a 60 in
            // `video_modes` (see `quality::Capabilities`), so bevy's `Monitor` knows
            // nothing on this platform. So it is *handed back*: `0.0` is
            // `ANativeWindow_setFrameRate`'s documented "the app will accept the
            // system's choice of display frame rate". The compositor knows the
            // panel's modes, the thermal state and what every other surface on
            // screen is asking for, and it is the only party here that does.
            //
            // ADPF then wants a deadline, and it must be the one we are *actually*
            // being paced to rather than the one we asked for — so it is measured
            // (`measured_period_ns`) instead of divided out of a constant. A hint
            // aimed at 16.6 ms on a panel presenting every 8.3 ms reports every
            // frame as comfortably early and the governor never boosts, which is
            // the bug this had before in the other direction.
            //
            // Retried until the surface exists: `native_window()` is `None` for the
            // first few seconds on Android (winit logs `Creating new window` about
            // five seconds after `android_main`), so a one-shot at startup would
            // silently never take.
            const PLATFORMS_CHOICE: f32 = 0.0;
            if platform::android::set_frame_rate(PLATFORMS_CHOICE) {
                *started = true;
                // Seeded from the tier so the session opens with *a* deadline; the
                // measurement below replaces it within a second or so.
                let period = (1.0e9 / f64::from(quality::budget().target_fps())) as i64;
                platform::android::start_perf_hint(period);
                platform::android::set_perf_hint_target(period);
            }
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
    mut deadline: Local<Option<std::time::Instant>>,
    mut period: Local<Option<Duration>>,
) {
    // Native only: a `thread::sleep`-based FPS cap. The browser paces frames itself
    // (requestAnimationFrame), and `Instant::now()`/`thread::sleep` would panic on
    // wasm, so this is a no-op there.
    #[cfg(target_arch = "wasm32")]
    let _ = (&mut deadline, &mut period);
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
        if quality::capabilities().is_some_and(|c| c.vsync_paced) {
            return;
        }
        let period = *period.get_or_insert_with(|| {
            // The tier's answer, with `CARTO_FPS_CAP` still overriding it — a
            // debugging knob has to win over a default to be useful, and lifting
            // the cap is how headroom gets measured at all.
            //
            // Through `env_value`, not `std::env::var`: on Android the former maps
            // to the `debug.carto.*` system properties and the latter reads an
            // environment the zygote never gave us. This was the one switch that
            // bypassed it — so the knob for A/B-ing a frame cap was unreachable on
            // the only platform where a frame cap is the answer, while the
            // property-name test below already listed it as if it worked.
            let fps: f64 = env_value("CARTO_FPS_CAP")
                .and_then(|v| v.parse().ok())
                .unwrap_or_else(|| quality::budget().fps_cap.unwrap_or(0.0));
            if fps <= 0.0 {
                Duration::ZERO
            } else {
                Duration::from_secs_f64(1.0 / fps)
            }
        });
        if period.is_zero() {
            return;
        }
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
            "asset census"
        );
    }
}

/// Render-cost census: triangle counts by layer, plus the **shadow-caster**
/// triangle load — the costs that mesh/image/material counts hide (a static scene
/// with 26 meshes still hung the GPU). Shadow-caster triangles are re-rasterized
/// once per shadow cascade (sun + moon, 4 each), so `ktris_shadow` weighs far
/// heavier than its raw value and is usually what pins this iGPU; `draws` is the
/// visible mesh count. Watch these against `frame_ms` to see what "too much" is.
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
