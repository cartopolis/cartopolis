//! What the phone knows about itself, in a form somebody holding it can send.
//!
//! **This exists because a device-only bug is otherwise unanswerable.** The
//! invisible avatar is the case it was built for and is worth stating in full,
//! because the same shape will recur. The client already logs everything needed to
//! diagnose it — `avatar: model requested holders=…`, `avatar: meshes drawn
//! meshes=… rendered=…`, the adapter, the sky decision, and whatever wgpu says
//! when a pipeline will not build. On a workstation that log is in the terminal.
//! On a phone it is in `logcat`, which needs a cable, a workstation, `adb`, and
//! developer mode — i.e. it is unavailable to exactly the person who has the
//! failing device in their hand.
//!
//! So the report goes out through the share sheet, the way
//! [`crate::offer_last_crash`] already sends a panic. The difference is only *when*:
//! that one fires on the launch after a crash, and this one fires when asked. A
//! run that is *working wrongly* never crashes, and until now it produced nothing
//! a user could send.
//!
//! **The live header is not a duplicate of the log.** logd's buffer is a ring, and
//! this client is chatty: by the time somebody notices something is wrong,
//! scrolls to the menu and taps, the lines that explain it may already have
//! rolled out. The header is re-measured at the moment of the tap, from the ECS,
//! so the facts that answer "is it spawned / is it drawn / where is it" are in
//! the report even when the log no longer reaches back that far.
//!
//! **Every platform gets it, and that is deliberate** — the same reason
//! `avatar::log_avatar_draw_counts` is not gated to Android. A diagnostic that
//! exists only on the platform you cannot debug is a diagnostic nobody has ever
//! seen working. On a desktop the report goes to the clipboard and to a file
//! under [`crate::utils::cache::app_data_root`], which is also how its wording
//! gets proof-read before a phone is the only reader; on the web it goes to the
//! clipboard alone, for the reason [`deliver`]'s wasm arm gives.

use bevy::prelude::*;
use big_space::prelude::GridCell;

use crate::components::Avatar;
use crate::resources::{AvatarState, CameraMode, CameraState};
use crate::systems::avatar::{AvatarReadout, SlotMask};

/// Set by the menu entry; cleared by [`share_diagnostics`] on the next frame.
///
/// A flag rather than the menu assembling the report itself, for a boring reason
/// that is nonetheless the whole design: `ui::render_main_menu` is at fifteen of
/// Bevy's sixteen system parameters, and the report needs six of its own. It also
/// keeps the assembly out of a system that runs every frame a menu is open.
#[derive(Resource, Default)]
pub struct DiagnosticsRequest {
    pub pending: bool,
    /// What the last request produced, shown back in the menu — on a phone the
    /// share sheet is its own confirmation, but on a desktop nothing visible
    /// happens at all and a silent button reads as a broken one.
    pub last: Option<String>,
}

/// The file the desktop arm writes, beside the user's own data.
#[cfg(not(any(target_os = "android", target_arch = "wasm32")))]
const REPORT_FILE: &str = "diagnostics.txt";

/// Cap on the log excerpt. Same figure and the same reasoning as the crash
/// reporter's: the binder transaction behind an `ACTION_SEND` tops out near a
/// megabyte, but the binding limit is the *receiving* app, and a note app handed
/// half a megabyte of text is a poor bet.
#[cfg(target_os = "android")]
const LOG_MAX_BYTES: usize = 64 * 1024;

/// Everything the report measures, in one parameter.
#[derive(bevy::ecs::system::SystemParam)]
pub(crate) struct DiagnosticsSources<'w, 's> {
    readout: AvatarReadout<'w, 's>,
    camera_state: Res<'w, CameraState>,
    avatar_state: Res<'w, AvatarState>,
    /// The last whole second of frame timing, accumulated unconditionally by
    /// `lib.rs::frame_stats`.
    ///
    /// The report had no performance number in it at all until 2026-08-18, which
    /// left it describing a device's *configuration* while saying nothing about
    /// whether that configuration was working. On the phone this client targets
    /// there is no `adb`, no environment variable and no GPU profiler that
    /// supports the part — so a report the user taps and sends is the only
    /// instrument, and it has to carry the number.
    frame: Res<'w, crate::resources::FrameStats>,
    /// Whether this device has demoted itself, and how far
    /// (`systems::adaptive_quality`).
    ///
    /// **The report is the only instrument that reaches a phone**, and a device
    /// that quietly gave up an effect looks identical in a screenshot to one that
    /// never had it. Without this line, "the clouds are missing on my phone" and
    /// "my phone decided it could not afford the clouds" are the same bug report.
    adaptive: Res<'w, crate::systems::adaptive_quality::AdaptiveQuality>,
    /// Draw and triangle counts for the frame the report is composed on.
    ///
    /// `counts()` resolves every mesh handle, so it is a real hitch on a dense
    /// scene — which is exactly why it belongs *here*, on a once-per-tap path,
    /// rather than on the five-second `log_render_cost` timer that is switched
    /// off on handsets for that cost.
    census: crate::systems::render_census::RenderCensus<'w, 's>,
    /// How evenly the sim's snapshots are reaching the prediction. Reported for
    /// the same reason `frame` is: "the avatar stutters" is a device-only symptom,
    /// and until this line existed it was impossible to tell a slow link from our
    /// own frame hitches delaying the mailbox drain from a real disagreement with
    /// the server.
    net: Res<'w, crate::resources::NetPredictionHealth>,
    /// Only to say whether the numbers above describe anything at all — offline
    /// there is no sim to be waiting for, and a row of zeroes must not read as a
    /// healthy connection.
    online: Option<Res<'w, crate::resources::OnlineSession>>,
    store: Res<'w, crate::systems::user_store::UserStore>,
    sky: Option<Res<'w, crate::systems::sky_dome::SkyMode>>,
    adapter: Option<Res<'w, bevy::render::renderer::RenderAdapterInfo>>,
    /// For the two limits that decide how a **skinned** mesh is drawn, which the
    /// avatar is and nothing else in this scene is. bevy_pbr picks its skinning
    /// path from `max_storage_buffers_per_shader_stage`: zero means storage
    /// buffers are unavailable and joints travel as uniforms with dynamic
    /// offsets — a far less-travelled path than the storage one every desktop
    /// takes, and one worth knowing a device is on before guessing at anything
    /// else.
    device: Option<Res<'w, bevy::render::renderer::RenderDevice>>,
    /// The avatar itself: its cell and local translation say *where* it is, and
    /// its `GlobalTransform` — the camera being big_space's floating origin — is
    /// its offset from the eye, which is the number that separates "not drawn"
    /// from "drawn a kilometre away".
    avatars: Query<
        'w,
        's,
        (
            Entity,
            &'static GridCell,
            &'static Transform,
            &'static GlobalTransform,
        ),
        With<Avatar>,
    >,
    /// One per distinct character glTF, carrying its parent so the count is the
    /// *local* avatar's rather than every avatar's in the world — a remote
    /// player standing next to you would otherwise inflate it, and the number is
    /// only worth reporting if it is about the model that is missing.
    ///
    /// `0` is the failure the 2026-08-14 fix was about and it is invisible from
    /// anywhere else: an avatar with no holders walks, is followed and is
    /// picked, and simply has no meshes.
    holders: Query<'w, 's, &'static ChildOf, With<SlotMask>>,
}

/// Assemble and deliver the report when the menu asks for it.
///
/// `pub(crate)` and not `pub`, because [`DiagnosticsSources`] is: a system whose
/// parameter names a crate-private type cannot itself be public.
pub(crate) fn share_diagnostics(
    mut request: ResMut<DiagnosticsRequest>,
    sources: DiagnosticsSources,
) {
    if !std::mem::take(&mut request.pending) {
        return;
    }
    let report = compose(&sources);
    // Into our own log as well as out of the app: a report that was shared and a
    // report that failed to share look identical from the outside, and on a
    // desktop this line *is* the delivery for anyone running from a terminal.
    tracing::info!(%report, "diagnostics report");
    request.last = Some(deliver(&report));
}

/// The live half of the report — everything measured at the moment of the tap.
fn compose(s: &DiagnosticsSources) -> String {
    let budget = crate::quality::budget();
    let (meshes, rendered) = s.readout.counts();
    let avatar = s.avatars.single().map(|(e, ..)| e).ok();
    let holders = s
        .holders
        .iter()
        .filter(|child_of| Some(child_of.parent()) == avatar)
        .count();
    let a = &s.store.data.appearance;

    let mut out = String::new();
    out.push_str(&format!(
        "cartopolis {} ({})\nplatform: {} / {}\n",
        env!("CARGO_PKG_VERSION"),
        option_env!("CARTO_BUILD_SHA").unwrap_or("unknown build"),
        std::env::consts::OS,
        std::env::consts::ARCH,
    ));
    match s.adapter.as_deref() {
        Some(info) => out.push_str(&format!(
            "adapter: {} — {:?} — {:?}, driver {} {}\n",
            info.name, info.backend, info.device_type, info.driver, info.driver_info,
        )),
        None => out.push_str("adapter: (no render adapter — headless)\n"),
    }
    out.push_str(&format!(
        "quality: {:?} (render scale {:.2}, msaa {:?}, shadows {})\n",
        budget.tier, budget.render_scale, budget.msaa, budget.shadows,
    ));
    // What the tier *asked for* is above; what the device *answered* is here, and
    // the two are separate lines because confusing them is what this whole seam
    // exists to stop (`quality::Capabilities`).
    match crate::quality::capabilities() {
        Some(c) => out.push_str(&format!(
            "capabilities: float32_filterable {}, vsync-paced {}\n",
            c.float32_filterable, c.vsync_paced,
        )),
        None => out.push_str("capabilities: (no renderer yet)\n"),
    }
    match s.sky.as_deref() {
        Some(mode) => out.push_str(&format!("sky: {mode:?}\n")),
        None => out.push_str("sky: (not decided yet)\n"),
    }
    if let Some(device) = s.device.as_deref() {
        let limits = device.limits();
        out.push_str(&format!(
            "limits: storage_buffers/stage {} (0 = skinning goes through uniforms), \
             bind_groups {}, max_texture_2d {}\n",
            limits.max_storage_buffers_per_shader_stage,
            limits.max_bind_groups,
            limits.max_texture_dimension_2d,
        ));
    }
    // Whether the pipelines behind the picture actually built. `sky: Atmosphere`
    // above is a *decision*; a driver that then rejects the atmosphere's shaders
    // leaves that line true and the sky black, with the only evidence a
    // creation-time log line that has long scrolled out of logd's ring. This is
    // the line that catches it (`systems::pipeline_health`).
    match crate::systems::pipeline_health::snapshot() {
        Some(h) => {
            out.push_str(&format!(
                "pipelines: {} ready, {} building, {} errored\n",
                h.ready,
                h.building,
                h.errored.len(),
            ));
            for e in &h.errored {
                out.push_str(&format!("  pipeline error: {e}\n"));
            }
        }
        None => out.push_str("pipelines: (not measured yet)\n"),
    }

    // Which bisecting switches this run has on, because a run with any of them on
    // is a run whose rendering proves nothing — and since 2026-08-23 they are
    // reachable from Settings, i.e. somebody can now leave one set and forget.
    // Listed by env name rather than by label so the reader can reproduce it,
    // which on Android means `adb shell setprop debug.carto.<name>`.
    let switches = s.store.data.settings.debug_switches;
    let on: Vec<&str> = crate::resources::DebugSwitches::SWITCHES
        .iter()
        .filter(|(name, _, _)| crate::env_flag(name))
        .map(|(name, _, _)| *name)
        .collect();
    if on.is_empty() {
        out.push_str("debug switches: none\n");
    } else {
        out.push_str(&format!("debug switches: {}\n", on.join(", ")));
    }
    // The preference separately from the resolved answer: an env var or property
    // overrides it, so the two disagreeing is itself worth seeing.
    if switches.any() {
        out.push_str("  (set in Developer tools ▸ Switches)\n");
    }

    // The performance block. Placed directly under `quality:` because the two are
    // read together: the tier says what was asked for, this says what it cost.
    //
    // `worst` beside `mean` on purpose — a hitch is invisible in a mean, and a
    // streaming app's characteristic failure is a good average with a 300 ms
    // frame every time a cell lands. `jitter` separates a pacing problem (high
    // jitter, healthy mean) from a load one.
    //
    // `draws` is here rather than triangles alone because it is the number that
    // decides cost on a mobile GPU Bevy excludes from GPU preprocessing — see
    // `render_census::RenderCounts::draws`. On a part that *is* included the two
    // diverge, and knowing which side of that line a device fell on is most of
    // the diagnosis.
    let c = s.census.counts();
    out.push_str("\nperf: ");
    if s.frame.sampled {
        out.push_str(&format!(
            "{} fps (mean {:.1} ms, worst {:.1} ms, jitter {:.1} ms)\n",
            s.frame.fps, s.frame.mean_ms, s.frame.worst_ms, s.frame.jitter_ms,
        ));
    } else {
        // Never a confident `0 fps`: the first whole second has not elapsed, which
        // is a different fact from a stalled renderer and must not read as one.
        out.push_str("(no full second sampled yet)\n");
    }
    if s.adaptive.step() > 0 {
        out.push_str(&format!(
            "  adaptive: this device gave up {} effect(s) after missing its frame \
             budget — step 1 drops the globe cloud shell\n",
            s.adaptive.step(),
        ));
    }
    out.push_str(&format!(
        "  draws {} | tris {} total, {} shadow-casting, {} buildings, {} lod22, {} tiles, {} globe\n",
        c.draws, c.tris_total, c.tris_shadow, c.tris_buildings, c.tris_lod22, c.tris_tiles,
        c.tris_globe,
    ));
    out.push_str(&format!(
        "  workers {} wide, {} jobs outstanding\n",
        crate::platform::workers::worker_count(),
        crate::platform::workers::outstanding(),
    ));
    // The four pool widths, and `compute` is the one to read.
    //
    // `ComputeTaskPool` *is* Bevy's parallel system executor, and its width is
    // not configured directly — it is whatever the io and async_compute policies
    // leave over (`lib.rs`'s `TaskPoolPlugin` override has the arithmetic). That
    // made it possible for it to resolve to **one thread** on an 8-core handset,
    // serialising every Update system, while the same settings gave a 22-core
    // workstation five and looked entirely healthy. A width that is derived
    // rather than declared has to be reported, or the next person to change a
    // percentage here has no way to see what they did on hardware they do not
    // own.
    out.push_str(&format!(
        "  pools: compute {} | io {} | async {} (of {} cores)\n",
        bevy::tasks::ComputeTaskPool::get().thread_num(),
        bevy::tasks::IoTaskPool::get().thread_num(),
        bevy::tasks::AsyncComputeTaskPool::get().thread_num(),
        std::thread::available_parallelism().map_or(0, std::num::NonZeroUsize::get),
    ));

    // The network block, and like `perf` above it is a worst-beside-mean reading:
    // a snapshot stream that averages 20 a second can still park the avatar twice
    // a minute, and only `worst` and `parked` say so.
    //
    // **`parked` is the stutter, stated as a number.** It is the share of frames
    // the display was being braked to a halt because there was nothing left to
    // extrapolate from — so a stuttering flight with `parked 0%` is not a network
    // problem at all and the `perf` block above is where to look, while anything
    // above a percent or two is. Nothing else in this report distinguishes the
    // two, and they need opposite fixes.
    out.push_str("\nnet: ");
    if s.online.is_none() {
        out.push_str("offline (no sim — the figures below would be meaningless)\n");
    } else if s.net.snapshots == 0 {
        out.push_str("online, no snapshots applied yet\n");
    } else {
        out.push_str(&format!(
            "{} snapshots | last gap {:.0} ms, worst {:.0} ms | parked {:.1}% of {} frames\n",
            s.net.snapshots,
            s.net.last_gap_s * 1000.0,
            s.net.worst_gap_s * 1000.0,
            s.net.stalled_fraction() * 100.0,
            s.net.frames,
        ));
    }

    // The avatar block, and the order is the diagnosis. `holders = 0` is "no model
    // was ever asked for"; `meshes = 0` with holders is "the glTF did not load";
    // `rendered = 0` with meshes is "culled"; and meshes == rendered with nothing
    // on screen is the driver dropping the draw — which is the one case that has
    // to be read off the log below rather than from these numbers.
    out.push_str(&format!(
        "\navatar: holders={holders} meshes={meshes} rendered={rendered}\n"
    ));
    out.push_str(&format!(
        "  appearance: {:?} / head {} / top {} / legs {} / feet {} / accessory {}\n",
        a.sex,
        a.head,
        a.top,
        a.legs,
        a.feet,
        a.accessory.as_deref().unwrap_or("—"),
    ));
    match s.avatars.single() {
        Ok((_, cell, tf, global)) => {
            out.push_str(&format!(
                "  entity: cell ({}, {}) local ({:.1}, {:.1}, {:.1}) scale {:.3}\n",
                cell.x, cell.z, tf.translation.x, tf.translation.y, tf.translation.z, tf.scale.x,
            ));
            out.push_str(&format!(
                "  camera→avatar: {:.1} m\n",
                global.translation().length()
            ));
        }
        Err(_) => out.push_str("  entity: (no avatar entity at all)\n"),
    }
    out.push_str(&format!(
        "  state: position ({:.1}, {:.1}, {:.1}) display ({:.1}, {:.1}, {:.1})\n",
        s.avatar_state.position.x,
        s.avatar_state.position.y,
        s.avatar_state.position.z,
        s.avatar_state.display_position.x,
        s.avatar_state.display_position.y,
        s.avatar_state.display_position.z,
    ));
    let mode = match s.camera_state.mode {
        CameraMode::Avatar => "follow",
        CameraMode::Free => "fly",
        CameraMode::Orbit => "earth",
    };
    out.push_str(&format!(
        "camera: {mode}, follow distance {:.1} m, pitch {:.0}°, azimuth {:.0}°\n",
        s.camera_state.distance,
        s.camera_state.pitch.to_degrees(),
        s.camera_state.azimuth.to_degrees(),
    ));
    out
}

/// Hand the report to whatever this platform has, and say what happened.
#[cfg(target_os = "android")]
fn deliver(report: &str) -> String {
    // The log is appended here rather than in `compose` so that the header — the
    // part that is always small and always readable — is at the top of the share,
    // where a mail composer's preview shows it.
    let full = match crate::platform::android::own_logcat() {
        Some(log) => format!(
            "{report}\n--- our own logcat, last lines (newest at the bottom) ---\n{}\n",
            crate::tail_bytes(&log, LOG_MAX_BYTES)
        ),
        None => format!(
            "{report}\n(no logcat available — the buffer was empty or the exec was refused)\n"
        ),
    };
    match crate::platform::android::share_text("Cartopolis diagnostics", &full) {
        Ok(()) => "Shared.".to_string(),
        Err(e) => format!("Could not share: {e}"),
    }
}

/// Desktop: the clipboard for a quick paste, and a file for everything the
/// clipboard is a bad fit for.
#[cfg(not(any(target_os = "android", target_arch = "wasm32")))]
fn deliver(report: &str) -> String {
    crate::utils::clipboard::set_text(report);
    let path = crate::utils::cache::app_data_root().join(REPORT_FILE);
    match std::fs::write(&path, report) {
        Ok(()) => format!("Copied, and written to {}", path.display()),
        // The clipboard still worked, so this is a partial success and must not
        // read as a failure.
        Err(e) => format!(
            "Copied to the clipboard (could not write {}: {e})",
            path.display()
        ),
    }
}

/// The web: the clipboard, and **deliberately no file**.
///
/// `std::fs` compiles on wasm32 and does nothing — every write fails and the
/// error is the only sign, which is exactly how this build persisted nothing at
/// all until 2026-08-13 (`docs/notes/web-persistence.md`). Sharing the desktop
/// arm here would therefore report "could not write /…/diagnostics.txt" on every
/// tap, naming a path no browser has. Persistence on the web goes through
/// `platform::storage`, and a diagnostics report is not something to persist —
/// the browser's own console already has the log, so the clipboard is the whole
/// job.
#[cfg(target_arch = "wasm32")]
fn deliver(report: &str) -> String {
    crate::utils::clipboard::set_text(report);
    "Copied to the clipboard.".to_string()
}
