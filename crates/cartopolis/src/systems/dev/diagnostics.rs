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
        "quality: {:?} (render scale {:.2}, msaa {:?}, shadows {}, sky_dome {})\n",
        budget.tier, budget.render_scale, budget.msaa, budget.shadows, budget.sky_dome,
    ));
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
