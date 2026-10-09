//! What happens when the renderer fails: the halt policy, the surface that must
//! never be torn down after a device loss, the per-platform exit, the report the
//! next launch shares, and the release build's handler for failing systems.

use bevy::prelude::*;

use crate::render_setup::gpu_preprocessing_name;
use crate::{platform, resources, systems};

/// Set once the renderer has been halted for good, so the halt is logged and
/// recorded once rather than on every frame for the rest of the run.
static RENDER_HALTED: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

/// What to do about a `RenderError`. **Never let bevy end the app** — halt the
/// renderer, leave an explanation the next launch can share, and then decide for
/// ourselves whether staying up is worth anything ([`exit_on_halt`]: on a phone it
/// is not, and the process leaves deliberately with its report already written).
///
/// Bevy's default handler writes `AppExit::error()` on any `RenderError`, and its
/// own comment calls that overzealous. It cost this project a day: the first phone
/// the client ever ran on lacked `FLOAT32_FILTERABLE`, four atmosphere LUT bind
/// groups failed validation, and the client *vanished* three seconds in — aborting
/// inside libc on the way out, so there was no Rust panic, no exit message, and a
/// crash report that could only say "the process was killed".
///
/// `StopRendering` is **terminal**: bevy leaves the render state `Errored` and
/// never draws again, so the app the user is holding is a still photograph of the
/// last good frame that answers no input. That is a poor outcome and it is
/// deliberately the one taken here, for every error class, including `DeviceLost`.
///
/// ## Why `DeviceLost` is no longer recovered
///
/// It was, for a fortnight, on the sound reasoning that a lost device is the one
/// plausibly-transient error — wgpu delivers it from its own
/// `set_device_lost_callback`, and an Android pause/resume can take a surface out
/// from under a frame in flight. `RenderErrorPolicy::Recover` exists for exactly
/// that. **It never once worked on the device, and it aborted the process both
/// times it ran** (Adreno 840, OnePlus 15):
///
/// - 2026-08-17: `Recover` built a second wgpu instance and asked it for a surface
///   on an `ANativeWindow` the *old* surface still held, and wgpu-hal's Android arm
///   answers that with a bare `.expect` —
///   `AndroidSurface failed: ERROR_NATIVE_WINDOW_IN_USE_KHR`, SIGABRT.
/// - 2026-08-18: the fix for that dropped the old surface first, from inside the
///   handler. Which trips the assertion one layer down —
///   `Trying to destroy a SwapchainAcquireSemaphore that is still in use by a
///   SurfaceTexture` (`wgpu-hal-29.0.4/src/vulkan/swapchain/native.rs:359`),
///   SIGABRT again.
///
/// Both are `panic!`/`expect` inside wgpu-hal, on the render thread, and neither is
/// ours to catch. The second one is the deeper lesson, and it took a third attempt
/// and a reading of the wgpu sources to state correctly: **after a device loss the
/// acquired swapchain image can no longer be released by anybody**, because
/// `wgpu-core`'s `Surface::discard` is gated on `check_is_valid()` and a lost
/// device never is. So it is not that a surface cannot be torn down safely at this
/// point in the frame — it is that it cannot be torn down safely ever again, at
/// any point, for the rest of the process. [`abandon_lost_surface`] has the code
/// and the citation. Recovery is upstream's job (`RenderErrorPolicy::Recover` is
/// supposed to *be* that affordance) and is not something to reimplement from the
/// error handler.
///
/// So the policy is one line again, and the trade is stated rather than hidden: a
/// transient device loss costs the session instead of being ridden out. It costs a
/// **restart**, where recovering cost a **crash**. What has changed since that was
/// first written is only what "costs a restart" is allowed to look like: not a
/// frozen picture the user has to force-close and then a tombstone when they do,
/// but the app leaving on its own with the report already saved. If bevy or wgpu
/// grow a recovery that survives an Android surface, this is the function to
/// revisit — and the way to know it works is the device, not this file.
///
/// **A halt is recorded, not just logged.** The point of the whole crash-reporter
/// apparatus is that a phone has no cable; a frozen picture with the explanation
/// only in `logcat` is the same undiagnosable freeze as before. So the first halt
/// writes the report the *next* launch hands to the share sheet
/// (`offer_last_crash`), which is how `FLOAT32_FILTERABLE` was named in one round
/// trip. Once only — bevy calls this handler every frame for as long as the state
/// stays `Errored`.
pub(crate) fn handle_render_error(
    error: &bevy::render::error_handler::RenderError,
    main_world: &mut World,
    render_world: &mut World,
) -> bevy::render::error_handler::RenderErrorPolicy {
    use std::sync::atomic::Ordering;

    // `swap` rather than a load-then-store: this is reached every frame from here
    // on, and the report and the log line are for the *first* one.
    if !RENDER_HALTED.swap(true, Ordering::Relaxed) {
        tracing::error!(
            kind = ?error.ty,
            description = %error.description,
            "render error — halting the renderer, keeping the app alive"
        );
        // Before `abandon_lost_surface`, which takes the render device's
        // window surfaces away — and on every platform, not just the one that
        // writes a file, because the same census goes to the log a desktop halt
        // leaves behind.
        tracing::error!("{}", gpu_memory_census(main_world, render_world));
        record_render_halt(error, main_world, render_world);
        abandon_lost_surface(render_world);
        exit_on_halt();
    }
    bevy::render::error_handler::RenderErrorPolicy::StopRendering
}

/// End the process on a halt, on the one platform where staying up is worse.
///
/// The halt is terminal — nothing is drawn again for the life of the process —
/// so what "keeping the app alive" buys a phone is a picture that answers no
/// input and has to be force-closed, with the explanation in a `logcat` nobody
/// can read. That was still the better half of the trade while the alternative
/// was an abort; it is not the better half of the trade against a deliberate
/// exit that has already written its report, which the next launch hands to the
/// share sheet (`offer_last_crash`).
///
/// There is a second reason and it is not a preference. A lost device makes every
/// render system's next `Buffer::get_mapped_range` a validation failure, and some
/// of those run on `AsyncComputeTaskPool` threads out of reach of this handler —
/// two of the four tombstones from the night of 2026-08-19 are that abort, not
/// the swapchain one. Staying up after a halt means waiting to see which of them
/// arrives first. Leaving promptly is the only thing that shortens that window.
///
/// `process::exit` and not an `AppExit`: an orderly shutdown *runs the Vulkan
/// teardown*, which is the thing [`abandon_lost_surface`] exists to make sure
/// nobody does. Exiting the process runs no destructors, so there is nothing left
/// to abort in. Status 0 because this is a decision rather than a fault —
/// ActivityManager files it as an ordinary exit instead of `CRASH_NATIVE`.
///
/// Desktop deliberately keeps the frozen picture: the explanation is already on
/// the terminal the app was started from, and a developer may want to look at the
/// window rather than have it vanish.
#[cfg(target_os = "android")]
fn exit_on_halt() -> ! {
    tracing::error!("halted renderer is terminal — exiting rather than holding a frozen screen");
    std::process::exit(0)
}

/// On the web, a halted renderer reloads the page — because there a lost device
/// is *weather*, not a wreck.
///
/// The terminal-halt policy above was argued entirely in Vulkan terms: a stranded
/// swapchain semaphore, `Buffer::get_mapped_range` aborts on pool threads, an
/// `ANativeWindow` mid-teardown. None of that exists on `wasm32` — the webgpu
/// backend has no wgpu-core underneath it, so there is nothing to strand and
/// nothing to abort in. What the browser *does* have is a device that can be
/// taken away in the ordinary course of business: Chrome restarts its GPU
/// process under memory pressure, on driver resets and on plain crashes (the
/// Adreno drivers under Android Chrome being a chronic source), and both the
/// WebGPU spec and Chrome's own guidance say the same thing about it — expect
/// `device.lost`, get a new device, carry on. Bevy cannot re-request a device
/// mid-run, but the page can relaunch itself, which is the same recovery one
/// level up. Before this arm existed the web build kept Android's "halt forever"
/// answer minus Android's exit, which a user experiences as a permanently blue
/// page with the explanation in a console a phone cannot open — that was the
/// deployed client's whole failure mode on 2026-08-23.
///
/// **Capped, because a reload is only a recovery if the next boot survives.** A
/// device loss that recurs deterministically at startup would reload forever;
/// the counter in `sessionStorage` stops the third retry and shows an honest
/// card instead ([`halt_card`]). A loss after [`HALT_STREAK_RESET_S`] of healthy
/// running is a fresh incident, not a continuation of the streak — without that
/// rule, two losses an hour apart would eventually strand a long session on the
/// card. The counter is per-tab (`sessionStorage`, not `localStorage`) so a bad
/// streak never outlives the tab that suffered it.
#[cfg(target_arch = "wasm32")]
fn exit_on_halt() {
    let window = web_sys::window();
    let storage = window
        .as_ref()
        .and_then(|w| w.session_storage().ok())
        .flatten();
    // `Some(0)` when the key is absent; `None` only when storage itself is
    // unavailable — in which case nothing can bound a reload loop, so none is
    // started (`halt_action` answers the card for `None`).
    let prior = storage.as_ref().map(|s| {
        s.get_item(HALT_RELOAD_KEY)
            .ok()
            .flatten()
            .and_then(|v| v.parse().ok())
            .unwrap_or(0)
    });
    // `performance.now()` is time since navigation start — which is the right
    // zero here, since what the cap distinguishes is "dies at every boot" from
    // "died after running fine".
    let uptime_s = window
        .as_ref()
        .and_then(|w| w.performance())
        .map_or(0.0, |p| p.now() / 1000.0);
    match halt_action(prior, uptime_s) {
        HaltAction::Reload { attempt } => {
            if let Some(s) = &storage {
                let _ = s.set_item(HALT_RELOAD_KEY, &attempt.to_string());
            }
            tracing::error!(
                attempt,
                max = HALT_RELOAD_MAX,
                "reloading the page after a lost graphics device"
            );
            if let Some(w) = &window {
                let _ = w.location().reload();
            }
        }
        HaltAction::ExplainAndStay => halt_card(),
    }
}

#[cfg(all(not(target_os = "android"), not(target_arch = "wasm32")))]
fn exit_on_halt() {}

/// `sessionStorage` key counting reloads in the current halt streak.
#[cfg(target_arch = "wasm32")]
const HALT_RELOAD_KEY: &str = "carto-halt-reloads";
/// How many automatic reloads a streak of losses may spend before the card.
// `cfg!`-style visibility rather than `#[cfg]`: the decision these constants
// feed is tested natively (see `halt_action_tests`), for the reason
// `android_property_name` states — wasm has no test lane, so logic only a wasm
// build compiles is logic nothing executes before a user does.
#[cfg_attr(not(target_arch = "wasm32"), allow(dead_code))]
const HALT_RELOAD_MAX: u32 = 2;
/// Uptime past which a loss counts as a fresh incident rather than a streak.
#[cfg_attr(not(target_arch = "wasm32"), allow(dead_code))]
const HALT_STREAK_RESET_S: f64 = 120.0;

/// What a web render halt does next. Pure so the policy is testable off-wasm.
#[cfg_attr(not(target_arch = "wasm32"), allow(dead_code))]
#[derive(Debug, PartialEq, Eq)]
enum HaltAction {
    /// Reload the page; `attempt` is what the streak counter becomes.
    Reload { attempt: u32 },
    /// The streak is spent (or cannot be counted): stay halted and say why.
    ExplainAndStay,
}

/// Decide between another reload and the card. `prior_reloads` is `None` when
/// `sessionStorage` is unavailable — an uncountable loop must not be started, so
/// that answers the card straight away.
#[cfg_attr(not(target_arch = "wasm32"), allow(dead_code))]
fn halt_action(prior_reloads: Option<u32>, uptime_s: f64) -> HaltAction {
    let Some(prior) = prior_reloads else {
        return HaltAction::ExplainAndStay;
    };
    if uptime_s >= HALT_STREAK_RESET_S {
        // A fresh incident restarts the streak at 1 rather than adding to a
        // count left over from an earlier, resolved one.
        return HaltAction::Reload { attempt: 1 };
    }
    if prior < HALT_RELOAD_MAX {
        HaltAction::Reload { attempt: prior + 1 }
    } else {
        HaltAction::ExplainAndStay
    }
}

/// Put the failure on the screen, in the page's own words and colours.
///
/// This is the web sibling of Android's crash report: the boot overlay is long
/// gone by now (it removes itself at the first drawn frame), so without this the
/// spent-streak state is a solid `#13191b` rectangle. Plain DOM on purpose —
/// after the halt the ECS is on its way down, so nothing here may depend on
/// another frame running. The retry is an `<a href="">` (a navigation to the
/// same URL) rather than a scripted button, so it keeps working after the wasm
/// runtime is dead.
#[cfg(target_arch = "wasm32")]
fn halt_card() {
    let Some(document) = web_sys::window().and_then(|w| w.document()) else {
        return;
    };
    let Ok(card) = document.create_element("div") else {
        return;
    };
    // The loader's palette: SURFACE, INK, TEXT, CREAM, ACCENT_TEXT.
    card.set_inner_html(
        "<div style=\"position:fixed;inset:0;z-index:9999;display:flex;align-items:center;\
         justify-content:center;background:#13191b;font:15px/1.6 system-ui,sans-serif\">\
         <div style=\"max-width:34rem;margin:0 1.5rem;padding:1.2rem 1.4rem;background:#0a0e10;\
         border:1px solid #69757a;border-radius:8px;color:#dbe2e5\">\
         <h1 style=\"font-size:1.3rem;font-weight:600;color:#f3f7f9;margin:0 0 .5rem\">\
         Cartopolis lost its graphics device</h1>\
         <p style=\"margin:.4rem 0\">The browser took the GPU away, and reloading the page \
         did not bring it back. That is usually the browser&#8217;s GPU process crashing on \
         this page &#8212; restarting the browser sometimes clears it.</p>\
         <p style=\"margin:.4rem 0\"><a href=\"\" style=\"color:#ffffff\">Try again</a></p>\
         </div></div>",
    );
    if let Some(body) = document.body() {
        let _ = body.append_child(&card);
    }
    tracing::error!("halt card shown — automatic reloads are spent for this streak");
}

/// Make sure nobody ever tears down the swapchain this halt has stranded.
///
/// **The previous version of this tried to hand the frame back, and could not.**
/// It cleared every holder bevy has — `ExtractedWindows::swap_chain_texture`, the
/// `ViewTarget`s, `ViewTargetAttachments` — on the reasoning that the acquired
/// image was stuck in the render world because `StopRendering` skips the system
/// that presents. All true, and it changed nothing: the OnePlus 15 aborted with
/// the same `Trying to destroy a SwapchainAcquireSemaphore that is still in use
/// by a SurfaceTexture` on the next background, from a build that contained the
/// fix. The holder was never ours.
///
/// ```text
/// // wgpu-core-29.0.4/src/present.rs — Surface::discard()
/// let device = &present.device;
/// device.check_is_valid()?;                      // Err(DeviceError::Lost)
/// let texture = present.acquired_texture.take()… // never reached
/// ```
///
/// Dropping a `SurfaceTexture` that was never presented calls `discard`, and
/// `discard` refuses to run on a lost device — so the raw texture, and the
/// `Arc<Mutex<SwapchainAcquireSemaphore>>` clone it carries, stays parked in
/// `present.acquired_texture` for good. Whatever destroys the swapchain
/// afterwards then walks into
/// `Arc::into_inner(semaphore).expect("Trying to destroy a
/// SwapchainAcquireSemaphore…")` in `wgpu-hal`'s `NativeSwapchain::release_resources`.
/// `NativeSwapchain::discard_texture` is still `// TODO: Current implementation
/// no-ops`, and **wgpu trunk past 30.0.0 still has the `check_is_valid` gate**, so
/// this is not a version to upgrade out of. Upstream: bevyengine/bevy#24722
/// reports the same panic on desktop Vulkan.
///
/// A device loss therefore leaves an acquire semaphore that *cannot* be released
/// by anyone. The only move left is to make sure nothing ever tries: take
/// every window's `SurfaceData` out of the render world and leak it, so no `wgpu::Surface`
/// here is ever dropped and `release_resources` is never reached — not when
/// Android suspends and `extract_windows` drops the surface, and not at shutdown.
///
/// Leaking is the honest word and the cost is nil: the renderer is halted for the
/// life of the process, the process is going away
/// ([`exit_on_halt`] on Android), and the alternative to holding a few Vulkan
/// objects that can no longer be destroyed is a SIGABRT. The one thing it must not
/// become is a leak on a *live* device — hence the single call site, inside the
/// first-halt branch.
///
/// **Reasoned from the wgpu sources, not measured.** Nothing here can lose a
/// Vulkan device — lavapipe does not and the VPS emulator is SwiftShader — so what
/// is established is the mechanism, in code that is quoted above and readable in
/// the registry. The measurement is a phone: lose the device, background the app,
/// and look for a tombstone that no longer arrives.
fn abandon_lost_surface(render_world: &mut World) {
    use bevy::render::view::SurfaceData;
    // Since bevy 0.20 each window's surface is a `SurfaceData` component on its
    // render-world entity rather than one `WindowSurfaces` resource. Taking them
    // out leaves `ExtractedWindow` without a surface, which would make
    // `create_surfaces` build a fresh one — but that system is in `Render`, which
    // bevy gates on `RenderState::Ready` and so never runs again after a halt.
    let holders: Vec<Entity> = render_world
        .query_filtered::<Entity, With<SurfaceData>>()
        .iter(render_world)
        .collect();
    if holders.is_empty() {
        tracing::warn!("no window surface to abandon at the halt — nothing held a swapchain");
        return;
    }
    for entity in holders {
        if let Some(surface) = render_world.entity_mut(entity).take::<SurfaceData>() {
            // Not `drop`. See above: dropping is the thing that aborts.
            std::mem::forget(surface);
        }
    }
    tracing::warn!(
        "abandoned the window surfaces — a lost device cannot release its acquired \
         swapchain image, so nothing may destroy the swapchain that holds it"
    );
}

/// The release build's answer to a failing system: log it, but not every frame.
///
/// `ecs::error::warn` writes one line per failure, and a system that keeps
/// failing fails on every frame. On the web each line is a synchronous
/// `console.error` call into the browser: after a halted renderer
/// ([`abandon_lost_surface`] removes the window surfaces, and bevy's
/// `extract_windows` keeps running against it) that was 2,140 warnings in 50 s,
/// most of what the console cost the main thread. The first occurrence of each
/// distinct failure logs in full and then every 1,000th says how many there have
/// been, so a stream is still visible as a stream.
pub(crate) fn warn_once_per_failure(
    error: bevy::ecs::error::BevyError,
    ctx: bevy::ecs::error::ErrorContext,
) {
    use crate::platform::sync::{Mutex, OnceLock};
    use std::collections::HashMap;

    static SEEN: OnceLock<Mutex<HashMap<String, u64>>> = OnceLock::new();
    let key = format!("{ctx}: {error}");
    let count = {
        let mut seen = SEEN
            .get_or_init(Default::default)
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let count = seen.entry(key).or_insert(0);
        *count += 1;
        *count
    };
    if count == 1 {
        bevy::ecs::error::warn(error, ctx);
    } else if count % 1000 == 0 {
        tracing::warn!("{ctx} has failed {count} times: {error}");
    }
}

/// Leave a halted renderer's explanation where the next launch will find it.
///
/// Android only, because it is the only platform where nobody can read the log:
/// on a desktop the same information is already on the terminal the app was
/// started from. Deliberately the same file the panic hook writes, so there is one
/// thing `offer_last_crash` has to look for and one share sheet on the next
/// launch — a halted renderer and a panic are the same question from the user's
/// side ("it stopped working, here is why").
#[cfg(target_os = "android")]
fn record_render_halt(
    error: &bevy::render::error_handler::RenderError,
    main_world: &World,
    render_world: &World,
) {
    let report = format!(
        "cartopolis {} ({})\nabi: {}\nThe renderer was halted by a {:?} error and the app \
         closed itself; the report you are reading is why. This is not a crash — a lost \
         GPU device cannot be resumed through, and staying up would only have ended in an \
         abort. See `handle_render_error`.\n\
         error: {}\n\n{}\n",
        env!("CARGO_PKG_VERSION"),
        option_env!("CARTO_BUILD_SHA").unwrap_or("unknown build"),
        std::env::consts::ARCH,
        error.ty,
        error.description,
        gpu_memory_census(main_world, render_world),
    );
    crate::crash::record_fatal(&report);
}

#[cfg(not(target_os = "android"))]
fn record_render_halt(
    _error: &bevy::render::error_handler::RenderError,
    _main_world: &World,
    _render_world: &World,
) {
}

/// What the GPU was holding at the instant of the halt, for the report.
///
/// **A halt report that says only `DeviceLost` is one bit of information**, and one
/// bit is what every account of these losses has had to reason from. It cannot
/// distinguish the three live explanations — this client asking for too much, the
/// driver refusing an allocation it should have served, or something that is not
/// about memory at all — and the losses are rare enough on a phone nobody can
/// attach a profiler to that guessing wrong costs days.
///
/// The pair that matters is `total_allocated_bytes` against `total_reserved_bytes`
/// (`wgpu::Device::generate_allocator_report`, which the vulkan backend gained in
/// wgpu 28 along with the `gpu-allocator` switch). Allocated is what is *in use*;
/// reserved is what wgpu has taken from the driver, slack included. A driver
/// refuses on the second. So:
///
/// - reserved ≈ allocated ≈ our own `resident_mb` → the client is asking for too
///   much, and the streaming budgets are the place to look;
/// - reserved far above allocated → wgpu's allocator is holding memory the app
///   does not have, which is [bevyengine/bevy#24722] and is not fixable from here;
/// - both small → not a memory problem at all, and every budget written for it
///   was aimed at the wrong thing.
///
/// Cheap and only ever called once, on the way out. `generate_allocator_report`
/// answers `None` on backends that do not track it.
///
/// [bevyengine/bevy#24722]: https://github.com/bevyengine/bevy/issues/24722
fn gpu_memory_census(main_world: &World, render_world: &World) -> String {
    use std::fmt::Write as _;

    let budget = crate::quality::budget();
    let mut out = format!(
        "quality: {:?} (tile_px {}, radius_bias {}, upload ceiling {})\n",
        budget.tier,
        budget.tile_px,
        budget.radius_bias,
        crate::quality::upload_ceiling().map_or("none".to_owned(), |b| format!("{} kB", b / 1024)),
    );

    // **Timeout or fault** — the question three rounds of budget work could not
    // settle, because the frame that dies is the only frame nothing measures (see
    // `watchdog::since_last_beat_ms`). Seconds here and the GPU was drowning in
    // work this client handed it; a frame's worth of milliseconds and it was not,
    // and every ceiling written for the burst was aimed past the problem.
    match platform::watchdog::since_last_beat_ms() {
        Some(ms) => {
            let _ = writeln!(out, "the frame that died had been running {ms} ms");
        }
        None => out.push_str("the frame that died: no heartbeat, so no duration\n"),
    }
    // And what the frames *before* it cost, which is the other half of the same
    // reading: a timeout is approached, so a run of climbing frame times is its
    // signature, while a fault arrives out of a healthy second.
    match main_world
        .get_resource::<resources::FrameStats>()
        .filter(|s| s.sampled)
    {
        Some(s) => {
            let _ = writeln!(
                out,
                "the second before it: {} fps, mean {} ms, worst {} ms",
                s.fps, s.mean_ms as u32, s.worst_ms as u32,
            );
        }
        None => out.push_str("the second before it: not yet sampled\n"),
    }

    match main_world.get_resource::<systems::upload_meter::UploadMeter>() {
        Some(m) => {
            let _ = writeln!(
                out,
                "streamed content: resident {} MB (peak {} MB), worst frame {} kB, \
                 largest asset {} kB",
                m.resident_kb() / 1024,
                m.peak_resident_kb() / 1024,
                m.peak_kb(),
                m.peak_asset_kb(),
            );
        }
        None => out.push_str("streamed content: unmeasured\n"),
    }

    // The render world's own answer, which counts what this code never sees:
    // render targets, shadow maps, LUTs, pipelines, and the allocator's slack.
    match render_world
        .get_resource::<bevy::render::renderer::RenderDevice>()
        .and_then(|d| d.wgpu_device().generate_allocator_report())
    {
        Some(r) => {
            let _ = writeln!(
                out,
                "wgpu allocator: {} MB in use of {} MB reserved from the driver \
                 ({} allocations in {} blocks)",
                r.total_allocated_bytes / (1024 * 1024),
                r.total_reserved_bytes / (1024 * 1024),
                r.allocations.len(),
                r.blocks.len(),
            );
        }
        None => out.push_str("wgpu allocator: no report on this backend\n"),
    }

    // Which of Bevy's three draw paths the frame was submitted through. A GPU
    // fault on a healthy frame is a question about the commands, not the bytes,
    // and this is the switch that decides whether the draw counts came off a
    // compute shader — see [`GpuPreprocessingOverridePlugin`].
    let _ = writeln!(
        out,
        "draw path: {}",
        render_world
            .get_resource::<bevy::render::batching::gpu_preprocessing::GpuPreprocessingSupport>()
            .map_or("unknown", |s| gpu_preprocessing_name(s.max_supported_mode)),
    );
    out
}

#[cfg(test)]
mod halt_action_tests {
    use super::{halt_action, HaltAction, HALT_RELOAD_MAX, HALT_STREAK_RESET_S};

    /// The first loss of a streak reloads; the streak counter becomes 1.
    #[test]
    fn a_first_loss_reloads() {
        assert_eq!(
            halt_action(Some(0), 30.0),
            HaltAction::Reload { attempt: 1 }
        );
    }

    /// A loss that keeps recurring at startup runs out of reloads and gets the
    /// card — the whole point of the cap, since each retry costs another full
    /// module download and boot.
    #[test]
    fn a_deterministic_startup_loss_stops_at_the_cap() {
        assert_eq!(
            halt_action(Some(1), 30.0),
            HaltAction::Reload { attempt: 2 }
        );
        assert_eq!(
            halt_action(Some(HALT_RELOAD_MAX), 30.0),
            HaltAction::ExplainAndStay
        );
    }

    /// A loss after a healthy stretch is a fresh incident: the streak restarts
    /// at 1 instead of inheriting a spent counter from hours ago.
    #[test]
    fn a_long_healthy_run_resets_the_streak() {
        assert_eq!(
            halt_action(Some(HALT_RELOAD_MAX), HALT_STREAK_RESET_S),
            HaltAction::Reload { attempt: 1 }
        );
    }

    /// No storage means no bound on a loop, so no loop is started.
    #[test]
    fn an_uncountable_streak_is_never_started() {
        assert_eq!(halt_action(None, 30.0), HaltAction::ExplainAndStay);
        assert_eq!(halt_action(None, 10_000.0), HaltAction::ExplainAndStay);
    }
}
