//! Crash reporting on a phone: the panic hook, the breadcrumb that tells a
//! killed run from an unwound one, and the share sheet that hands the previous
//! run's report to whoever is holding the device. Android only — the module is
//! declared under `cfg(target_os = "android")`.

use crate::{platform, utils};

/// Make a panic visible on a phone.
///
/// Android discards a process's stderr unless something arranges otherwise, and
/// this tree deliberately leaves Bevy's `LogPlugin` in place there (its
/// `android_log-sys` layer is the only thing that reaches logcat). But
/// `std::panic`'s default hook writes to stderr and does not go through
/// `tracing` — so on Android a panic is *completely silent*: the process dies,
/// logcat shows nothing from us, and the app "exits before it shows anything".
///
/// That is indistinguishable from a dozen other failures, which is the real cost.
/// The hook routes the payload and location through `tracing::error!` so it lands
/// in logcat under the `cartopolis` tag, then calls the previous hook so the abort
/// behaviour is unchanged.
///
/// It is installed as the first statement of `run` because a panic before this
/// point is one nobody can see, and everything above it is a constant.
#[cfg(target_os = "android")]
pub(crate) fn install_android_panic_hook() {
    let previous = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        let payload = info
            .payload()
            .downcast_ref::<&str>()
            .map(|s| (*s).to_string())
            .or_else(|| info.payload().downcast_ref::<String>().cloned())
            .unwrap_or_else(|| "<non-string panic payload>".to_string());
        let location = info
            .location()
            .map(|l| format!("{}:{}:{}", l.file(), l.line(), l.column()));
        tracing::error!(
            panic = %payload,
            location = location.as_deref().unwrap_or("<unknown>"),
            "PANIC — the app is about to abort"
        );
        // …and to disk, because logcat needs a cable and the person holding this
        // phone may not have one. The next launch picks it up (`offer_last_crash`)
        // and hands it to the share sheet.
        let report = format!(
            "cartopolis {} ({})\nabi: {}\npanic: {}\nat: {}\n",
            env!("CARGO_PKG_VERSION"),
            option_env!("CARTO_BUILD_SHA").unwrap_or("unknown build"),
            std::env::consts::ARCH,
            payload,
            location.as_deref().unwrap_or("<unknown>"),
        );
        record_fatal(&report);
        previous(info);
    }));
}

/// A file that exists only while a run is in the foreground.
///
/// Written at startup, removed when the app exits or is suspended. Finding it at
/// the *next* startup therefore means the previous run died on screen without
/// unwinding — which, combined with the absence of a panic report, is the one
/// piece of evidence that separates "our Rust panicked" from "the process was
/// killed underneath us", i.e. a driver or surface failure or the OOM killer.
///
/// That distinction is worth a file because it is the difference between a bug we
/// can find by reading and one that needs a device.
#[cfg(target_os = "android")]
pub(crate) fn run_breadcrumb_path() -> std::path::PathBuf {
    utils::cache::app_data_root().join("run-in-progress")
}

/// Where a panic report is left for the next launch to find.
///
/// Under the app's own data directory, which is the one location a
/// NativeActivity can always write (`utils::cache::app_data_root`). Nothing on a
/// non-rooted phone can read it, which is exactly why the next launch offers it
/// to the share sheet rather than expecting anyone to go and find it.
#[cfg(target_os = "android")]
fn crash_report_path() -> std::path::PathBuf {
    utils::cache::app_data_root().join("last-crash.txt")
}

/// Record a fatal event for the next launch, **keeping the first one**.
///
/// Both writers into this file — [`install_android_panic_hook`] and
/// [`record_render_halt`] — used a plain `fs::write`, so the last one to fire won.
/// That is exactly the wrong way round, because these events are not independent:
/// a halted renderer is a state the process then keeps running in, and the panic
/// that eventually comes out of it is a *consequence*. The OnePlus 15 log of
/// 2026-08-19 is the worked example. The device was lost at 09:32:30 and the halt
/// wrote the report that says so; the app ran on with a dead renderer for twenty
/// seconds; the user swiped it away at 09:32:50, wgpu tore down a swapchain that
/// still had a `SurfaceTexture` outstanding and panicked — and *that* overwrote
/// the file. The report handed to the share sheet therefore named
/// `SwapchainAcquireSemaphore`, a wgpu-internal assertion at teardown, and said
/// nothing about the device loss that had caused every bit of it. Two people then
/// read the wrong stack.
///
/// So: first write wins the top of the file, and anything after it is appended
/// under a rule. Appending rather than discarding because the later event is still
/// evidence — it is just not the *cause*, and a report is read from the top.
///
/// Safe as a within-run rule because `offer_last_crash` removes this file during
/// startup: if it exists, this run put it there.
#[cfg(target_os = "android")]
pub(crate) fn record_fatal(report: &str) {
    use std::io::Write;

    let path = crash_report_path();
    if path.exists() {
        if let Ok(mut f) = std::fs::OpenOptions::new().append(true).open(&path) {
            let _ = write!(f, "\n--- and then, downstream of the above ---\n{report}");
        }
        return;
    }
    let _ = std::fs::write(&path, report);
}

/// How much of the log excerpt a crash report may carry.
///
/// The report travels as an `Intent` extra, i.e. through a binder transaction with
/// a hard ceiling near a megabyte — but the binding constraint is softer and lower
/// than that: the *receiving* app has to accept it, and a mail composer or a note
/// app handed half a megabyte of text is a poor bet. 64 KB is several hundred lines
/// of logcat, which is more than a launch that dies before its first frame can
/// produce.
#[cfg(target_os = "android")]
const CRASH_LOG_MAX_BYTES: usize = 64 * 1024;

/// The last `max` bytes of `text`, starting at a line boundary.
///
/// The *tail*, because a crash is explained by what happened last. Trimming to the
/// next `\n` costs at most one line and means the excerpt never opens mid-token —
/// a half-line at the top reads as corruption and invites a reply about the wrong
/// thing.
#[cfg(target_os = "android")]
pub(crate) fn tail_bytes(text: &str, max: usize) -> &str {
    if text.len() <= max {
        return text;
    }
    let mut start = text.len() - max;
    while start < text.len() && !text.is_char_boundary(start) {
        start += 1;
    }
    match text[start..].find('\n') {
        Some(nl) => &text[start + nl + 1..],
        None => &text[start..],
    }
}

/// If the previous run left a panic report, hand it to the share sheet and delete
/// it.
///
/// Called before the `App` is built, and that is the whole point: the crash this
/// reports is one that killed the app before it drew a frame, so anything that
/// needs a window — a panel, a dialog, an egui anything — is unavailable by
/// definition. `ACTION_SEND` is a binder call and needs none of it.
///
/// The file is removed **before** the share sheet is launched, not after, so a
/// crash report that itself somehow triggers a crash cannot become a loop that
/// makes the app unusable.
#[cfg(target_os = "android")]
pub(crate) fn offer_last_crash() {
    let path = crash_report_path();
    let breadcrumb = run_breadcrumb_path();
    let unclean = breadcrumb.exists();
    let _ = std::fs::remove_file(&breadcrumb);
    let report = match std::fs::read_to_string(&path) {
        Ok(text) => {
            let _ = std::fs::remove_file(&path);
            text
        }
        // No panic report, but the previous run never got to tidy up: it was
        // killed rather than unwound. Say exactly that, because "the app closed
        // itself" and "the app panicked" look identical from the outside and are
        // nothing alike to debug.
        Err(_) if unclean => format!(
            "cartopolis {} ({})\nabi: {}\nThe previous run ended without a Rust panic and \
             without being suspended — the process was killed. That points at the \
             graphics driver, the Vulkan surface, or the OOM killer rather than at \
             our own code. The log excerpt below is what separates them: a `signal` \
             line with a backtrace is a native crash, while our log simply stopping \
             is the low-memory killer, whose own announcement belongs to the system \
             UID and cannot be read from in here.\n",
            env!("CARGO_PKG_VERSION"),
            option_env!("CARTO_BUILD_SHA").unwrap_or("unknown build"),
            std::env::consts::ARCH,
        ),
        Err(_) => return,
    };
    if report.trim().is_empty() {
        return;
    }
    // The framework's own record of how the last few processes ended, and — for an
    // ANR — the thread dump the system wrote. This goes **above** the log excerpt
    // because it answers a bigger question than the log can: a truncated log says
    // the app stopped, and this says whether it was declared unresponsive, killed
    // for memory, killed by a signal, or simply swiped away. See
    // `platform::android::recent_exit_reasons`.
    let report = match platform::android::recent_exit_reasons() {
        Some(exits) => format!("{report}\n{exits}\n"),
        None => format!(
            "{report}\n(no exit-reason record — the framework keeps these only from API 30)\n"
        ),
    };
    // Read the log buffer *before* anything below adds to it, so the excerpt is
    // the dead run's last words and not this one's first.
    let report = match platform::android::own_logcat() {
        Some(log) => format!(
            "{report}\n--- our own logcat, last lines (newest at the bottom) ---\n{}\n",
            tail_bytes(&log, CRASH_LOG_MAX_BYTES)
        ),
        None => format!(
            "{report}\n(no logcat available — the buffer was empty or the exec was refused)\n"
        ),
    };
    tracing::error!(%report, "the previous run ended badly");
    match platform::android::share_text("Cartopolis crash report", &report) {
        Ok(()) => tracing::info!("offered the previous crash report to the share sheet"),
        Err(e) => tracing::warn!(error = %e, "could not offer the crash report"),
    }
}
