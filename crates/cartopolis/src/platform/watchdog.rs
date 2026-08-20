//! Notice that the main loop has stopped, and say what the process was doing.
//!
//! **This exists because of a specific hole in the phone's diagnostics.** The
//! crash reporter (`lib.rs`) covers a run that panicked and a run that was killed;
//! `docs/notes/android-build.md` covers a run whose renderer halted. None of them
//! covers the case the device actually produced on 2026-08-14: the Bevy main loop
//! simply stopped, every periodic log line with it, and six seconds later
//! `ActivityManager` sent `SIGQUIT` and took the process. What reached the next
//! launch was a log that *ends*, with the last line before the silence being
//! whichever system happened to log most recently — which is not the same thing as
//! where it stopped, and led to a page of plausible suspects and no way to choose
//! between them.
//!
//! A stalled main loop cannot report on itself. So this is a separate thread that
//! watches a heartbeat, and when the beat stops it writes two things nobody could
//! otherwise obtain from inside a frozen app:
//!
//! 1. **That it stalled at all, and for how long** — which alone separates "the
//!    main loop is wedged" from "the app was killed while healthy", the two
//!    readings a truncated log cannot tell apart.
//! 2. **What every thread in the process is doing**, out of `/proc/self/task`.
//!    `stat` gives the scheduler state and `wchan` the kernel function a sleeping
//!    thread is parked in, and **both are readable by the process itself with no
//!    permission and no root** — unlike `/proc/<pid>/stack`, unlike the ANR trace
//!    the system writes to tombstoned, and unlike the `ActivityManager` line that
//!    announces the ANR, which belongs to the system UID. That is the whole reason
//!    this is worth building rather than asking someone to plug a cable in.
//!
//! Reading it is mechanical. A main thread in `R` (running) for the whole stall is
//! a frame that is merely enormous — look for what got asked to build. A main
//! thread in `S`/`D` parked in a futex is a deadlock, and the other threads' rows
//! say what it is waiting behind. A main thread parked in a *binder* wchan is a
//! call into another process that never came back, which on Android is usually a
//! framework call made from a thread the framework did not expect.
//!
//! It is an instrument and not a cure: nothing here unsticks anything. It costs
//! one relaxed atomic store per frame and a sleeping thread.

use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Instant;

// Native-only, alongside the three items below them that are the only users. See
// the comment on `STALL`.
#[cfg(not(target_arch = "wasm32"))]
use std::sync::atomic::AtomicBool;
#[cfg(not(target_arch = "wasm32"))]
use std::time::Duration;

/// How long the main loop may go without a beat before it is called a stall.
///
/// Under Android's five-second ANR window on purpose, so the report is *in the
/// log* before `ActivityManager` takes the process — a diagnosis that only
/// appears after the kill would never be written. Comfortably above any honest
/// frame: this client's worst measured hitches are a re-anchor's tile rebuild, and
/// those are per-frame-bounded and land in tens of milliseconds.
// `cfg`: the only reader is `watch`, which is native-only — see `start`. The
// module itself is unconditional because `beat` is called from the frame loop
// on every target, so without this the workspace's `unused = "deny"` fails the
// wasm build on three dead items. That lane is compiled by no CI job
// (`docs/notes/checking-the-web-build.md`), so it fails silently until someone
// runs it by hand.
#[cfg(not(target_arch = "wasm32"))]
const STALL: Duration = Duration::from_millis(2_500);

/// How often the watcher wakes. A quarter of [`STALL`] bounds how late the report
/// can be, and a four-times-a-second sleeper costs nothing measurable.
// `cfg`: the only reader is `watch`, which is native-only — see `start`. The
// module itself is unconditional because `beat` is called from the frame loop
// on every target, so without this the workspace's `unused = "deny"` fails the
// wasm build on three dead items. That lane is compiled by no CI job
// (`docs/notes/checking-the-web-build.md`), so it fails silently until someone
// runs it by hand.
#[cfg(not(target_arch = "wasm32"))]
const POLL: Duration = Duration::from_millis(600);

/// Milliseconds since [`start`], written by [`beat`] and read by the watcher.
///
/// A plain relaxed counter rather than an `Instant` behind a lock: this is
/// written every frame from the hot path, and the reader only needs to know that
/// the number has moved. Zero means "no beat yet".
static LAST_BEAT_MS: AtomicU64 = AtomicU64::new(0);
/// Set while a stall is being reported, so the report is written once per stall
/// rather than four times a second for as long as it lasts.
// `cfg`: the only reader is `watch`, which is native-only — see `start`. The
// module itself is unconditional because `beat` is called from the frame loop
// on every target, so without this the workspace's `unused = "deny"` fails the
// wasm build on three dead items. That lane is compiled by no CI job
// (`docs/notes/checking-the-web-build.md`), so it fails silently until someone
// runs it by hand.
#[cfg(not(target_arch = "wasm32"))]
static STALLED: AtomicBool = AtomicBool::new(false);

/// The main loop is alive. Called once per frame; everything else here is
/// downstream of this store not happening.
pub fn beat() {
    LAST_BEAT_MS.store(elapsed_ms(), Ordering::Relaxed);
}

/// How long the frame now in flight has been running, in milliseconds, or `None`
/// if [`start`] never ran and there is no heartbeat to measure against.
///
/// **The one frame whose duration nothing else can report is the one that kills
/// the device**, and it is therefore the frame every device-loss report so far has
/// been missing. `frame_pacing::frame_time_watchdog` reads `Time<Real>::delta`,
/// which is the *previous* frame; a fatal frame has no successor, because
/// `handle_render_error` ends the process from inside it. So the log of a loss
/// ends with a run of healthy frames and a silence, and the silence is equally
/// consistent with the two live explanations — a single enormous frame that
/// overran the command-buffer timeout, and a healthy frame on which the GPU
/// faulted for reasons that are not about our workload at all. Seconds here means
/// the first; a frame's worth of milliseconds means the second.
///
/// It is the heartbeat's own clock and not a new one, deliberately: [`beat`] runs
/// in `Last`, so "time since the last beat" is exactly "time spent in the frame
/// that has not finished yet", already maintained, already free.
pub fn since_last_beat_ms() -> Option<u64> {
    match LAST_BEAT_MS.load(Ordering::Relaxed) {
        // Zero is the pre-`start` sentinel, not a real reading — see the static.
        0 => None,
        last => Some(elapsed_ms().saturating_sub(last)),
    }
}

/// Process uptime in milliseconds, from a base fixed at [`start`].
fn elapsed_ms() -> u64 {
    // A `OnceLock<Instant>` rather than a `static Instant`, which cannot be const.
    // The `unwrap_or` arm is unreachable once `start` has run and is a plain zero
    // rather than a panic: a diagnostic that can take the app down is worse than
    // no diagnostic.
    static BASE: std::sync::OnceLock<Instant> = std::sync::OnceLock::new();
    let base = BASE.get_or_init(Instant::now);
    // Saturating for the same reason: monotonic clocks are monotonic, but this is
    // not the code to find out otherwise in.
    u64::try_from(base.elapsed().as_millis()).unwrap_or(u64::MAX)
}

/// Start watching. Idempotent-ish — call it once, from `run`.
///
/// No-op on wasm, which has no threads to spawn one from and, more to the point,
/// no silent kill to diagnose: a browser tab that stops painting still has a
/// console attached to it.
#[cfg(not(target_arch = "wasm32"))]
pub fn start() {
    beat();
    let spawned = std::thread::Builder::new()
        .name("cartopolis-watchdog".to_string())
        .spawn(watch);
    match spawned {
        // Detached: there is nothing to join and nothing to cancel. It outlives
        // every subsystem it might have to report on, which is the point.
        Ok(_handle) => tracing::info!(stall_ms = STALL.as_millis() as u64, "main-loop watchdog on"),
        Err(e) => tracing::warn!(error = %e, "could not start the main-loop watchdog"),
    }
}

#[cfg(target_arch = "wasm32")]
pub fn start() {}

#[cfg(not(target_arch = "wasm32"))]
fn watch() {
    loop {
        std::thread::sleep(POLL);
        let now = elapsed_ms();
        let last = LAST_BEAT_MS.load(Ordering::Relaxed);
        let quiet_ms = now.saturating_sub(last);
        let stalled = quiet_ms >= STALL.as_millis() as u64;

        if stalled && !STALLED.swap(true, Ordering::Relaxed) {
            // ERROR, not WARN: on Android the log filter is the crash report's
            // contents, and a stall that reads as a warning is one nobody scrolls
            // back to. This is the line the whole module exists to emit.
            tracing::error!(
                quiet_ms,
                "MAIN LOOP STALLED — no frame for {} ms; thread dump follows",
                quiet_ms
            );
            dump_threads();
        } else if !stalled && STALLED.swap(false, Ordering::Relaxed) {
            tracing::error!(
                recovered_after_ms = quiet_ms,
                "main loop resumed after a stall"
            );
        }
    }
}

/// Log one line per thread: name, scheduler state, and what it is parked in.
///
/// Linux-family only, because it is `/proc` and nothing else. Every read is
/// best-effort and a missing file is skipped rather than reported — a thread can
/// exit between the directory listing and the open, and a dump that panics on the
/// race would take the app down at the one moment it is trying to explain
/// something.
#[cfg(all(
    not(target_arch = "wasm32"),
    any(target_os = "linux", target_os = "android")
))]
fn dump_threads() {
    let Ok(entries) = std::fs::read_dir("/proc/self/task") else {
        tracing::error!("no /proc/self/task — cannot say what the threads are doing");
        return;
    };
    for entry in entries.flatten() {
        let dir = entry.path();
        let tid = entry.file_name().to_string_lossy().to_string();
        let name = read_trimmed(dir.join("comm")).unwrap_or_else(|| "?".to_string());
        // `stat`'s third field is the state character, and the *second* is the
        // comm in parentheses — which may itself contain spaces and parentheses,
        // so the fields are counted from the last ')' rather than split from the
        // start. This is the classic way to misparse /proc.
        let state = read_trimmed(dir.join("stat"))
            .and_then(|s| {
                let rest = s.rsplit_once(')')?.1;
                rest.split_whitespace().next().map(str::to_string)
            })
            .unwrap_or_else(|| "?".to_string());
        // `wchan` is the kernel symbol a sleeping thread is parked in — the field
        // that turns "blocked" into "blocked on a futex" or "blocked in binder".
        // It reads as `0` for a running thread and is unreadable on some kernels.
        let wchan = read_trimmed(dir.join("wchan")).unwrap_or_else(|| "?".to_string());
        tracing::error!(tid = %tid, thread = %name, state = %state, wchan = %wchan, "stalled-process thread");
    }
}

#[cfg(all(
    not(target_arch = "wasm32"),
    not(any(target_os = "linux", target_os = "android"))
))]
fn dump_threads() {
    tracing::error!("no per-thread introspection on this platform");
}

#[cfg(all(
    not(target_arch = "wasm32"),
    any(target_os = "linux", target_os = "android")
))]
fn read_trimmed(path: std::path::PathBuf) -> Option<String> {
    std::fs::read_to_string(path)
        .ok()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The heartbeat has to actually move, or the watcher reports a stall on a
    /// perfectly healthy app — a false alarm in a crash report is worse than
    /// silence, because it sends the next person after a phantom.
    #[test]
    fn a_beat_moves_the_clock_forward() {
        beat();
        let first = LAST_BEAT_MS.load(Ordering::Relaxed);
        std::thread::sleep(Duration::from_millis(5));
        beat();
        assert!(LAST_BEAT_MS.load(Ordering::Relaxed) > first);
    }

    /// The stall threshold must sit inside Android's ANR window, or the report it
    /// exists to write is only reached after the process has been killed.
    #[test]
    fn the_stall_threshold_beats_the_anr_timeout() {
        const ANR: Duration = Duration::from_secs(5);
        assert!(STALL < ANR);
        // And the watcher has to wake often enough that the report lands inside
        // the window too, not just that the threshold does.
        assert!(STALL + POLL < ANR);
    }
}
