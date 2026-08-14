use bevy::asset::AssetPlugin;
use bevy::camera::Exposure;
use bevy::light::light_consts::lux::RAW_SUNLIGHT;
use bevy::light::{CascadeShadowConfigBuilder, DirectionalLightShadowMap};
// Used on every target except wasm, but for opposite reasons: desktop disables it
// in favour of `tracing_subscriber`, Android reconfigures it. See `run`.
#[cfg(not(target_arch = "wasm32"))]
use bevy::log::LogPlugin;
use bevy::pbr::{Material, MaterialPipeline, MaterialPipelineKey, MaterialPlugin};
use bevy::prelude::*;
use bevy::render::mesh::MeshVertexBufferLayoutRef;
use bevy::render::render_resource::{
    AsBindGroup, RenderPipelineDescriptor, SpecializedMeshPipelineError,
};
use bevy::shader::ShaderRef;
use bevy::window::WindowResolution;
use clap::Parser;
use std::path::PathBuf;
use std::time::Duration;

mod components;
mod grid;
mod login;
mod platform;
mod quality;
mod resources;
mod simd_baseline;
#[cfg(feature = "solo")]
mod solo;
mod systems;
mod utils;

/// Where the asset store is, as `AssetPlugin::file_path` wants it — and the
/// **only** place in this crate that gets to answer that question.
///
/// It is a constant rather than a local in `run()` because it was a local, and
/// a second answer grew beside it: `avatar::avatar_asset_exists` probed a
/// *relative* `assets/` to decide whether a character glTF was present. The two
/// agreed only when the process happened to be started from the repo root, and
/// the places they disagreed are the places the avatar was invisible — Android
/// (a NativeActivity starts at `/`, and its assets are inside the APK, reachable
/// only through the framework `AssetManager`), the web (no filesystem at all),
/// and any native binary launched from another directory. Anything that needs
/// to know where an asset lives asks here.
///
/// The value per platform:
/// - **Desktop** — a repo-relative path resolved from the manifest dir.
/// - **Wasm** — `CARGO_MANIFEST_DIR` is a build-machine absolute path and
///   meaningless in a browser; Bevy fetches assets over HTTP relative to the
///   served page, so a plain `assets` URL is right (ADR-033).
/// - **Android** — `AssetSource::get_default_reader` returns
///   `AndroidAssetReader`, which ignores this string entirely and reads every
///   path straight out of the APK's `assets/`. So the value is unused — but a
///   build-machine absolute path left here would still be *read* by anyone
///   debugging where an asset came from, and point them at a directory that does
///   not exist on the device. `assets` says the true thing.
#[cfg(all(not(target_arch = "wasm32"), not(target_os = "android")))]
pub(crate) const ASSET_ROOT: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/../../assets");
#[cfg(any(target_arch = "wasm32", target_os = "android"))]
pub(crate) const ASSET_ROOT: &str = "assets";

use big_space::prelude::{BigSpace, GridCell};
use components::Avatar;
use resources::{
    AiAssistantState, AiConfig, AuthIdentity, AvatarChooserState, AvatarState, CameraState,
    ContextMenuState, DevPanelState, EditDialogState, GameState, LocalAvatarSimId, MarqueeState,
    MouseState, OsmTileUrlTemplate, PrimTextureCache, StatsState, TextureLibrary,
};
use systems::egui_manager::EguiPlugin;
use systems::*;
use utils::sun::sun_direction;

#[derive(Parser, Debug)]
#[command(name = "cartopolis")]
struct Cli {
    /// Connect to an external `simulator` instance (TCP, postcard messages).
    /// Omit to enter whatever the sign-in screen last remembered — solo mode on a
    /// fresh install (`systems::session`).
    #[arg(long)]
    connect: Option<String>,

    /// Run without any sim: single user, no auth, no persistence. What `--shot`
    /// uses. The sign-in screen can still switch modes from inside the run; this
    /// only decides where the launch starts.
    #[arg(long)]
    offline: bool,

    /// Credentials for the online handshake, so you don't log in each run: a path
    /// to a TOML login file, or inline `user:pass`. Defaults to `data/dev-login.toml`
    /// if present, else env `CARTO_USER`/`CARTO_PASSWORD`, else the solo `dev` account.
    #[arg(long, value_name = "FILE|user:pass")]
    login: Option<String>,

    /// Scripted capture: place the camera, wait for the world to stream in,
    /// save this PNG and exit (see `systems::shot_harness`).
    #[arg(long, value_name = "PATH")]
    shot: Option<PathBuf>,

    /// Run the remembered camera positions from a preset manifest (default
    /// `docs/shots.toml`): every named view captured in one launch, each to its
    /// own file, with its own camera mode, panels and time of day. This is the
    /// documentation-image build step — see `cargo shots`. Bare `--shots` (the
    /// common case) parses to `Some(None)` and uses the checked-in manifest.
    #[arg(long, value_name = "FILE", num_args = 0..=1)]
    shots: Option<Option<PathBuf>>,

    /// Restrict a `--shots` run to these preset names (comma-separated).
    #[arg(long, value_name = "NAMES", value_delimiter = ',')]
    only: Vec<String>,

    /// Camera position for `--shot`, as "lat,lng,altitude_m". Repeat for a
    /// sweep: one capture each, all in a single launch.
    #[arg(long, value_name = "LAT,LNG,ALT")]
    at: Vec<String>,

    /// Look angles for `--shot`, as "pitch,yaw" in degrees (−90 = straight
    /// down). Pairs with `--at` by position; the last one carries over.
    #[arg(long, value_name = "PITCH,YAW")]
    look: Vec<String>,

    /// Offscreen render size for `--shot` (default 1280x720). Overrides the
    /// manifest's `size` in a `--shots` run.
    #[arg(long, value_name = "WxH")]
    size: Option<String>,

    /// Seconds of quiet tile streaming required before a `--shot` capture
    /// (default 0.8; overrides the manifest's `settle`).
    #[arg(long)]
    settle: Option<f32>,

    /// Hard cap on the settle wait per capture, in seconds (default 15).
    #[arg(long)]
    wait: Option<f32>,

    /// Leave the egui overlay (menu bar, HUD) out of `--shot` captures.
    #[arg(long)]
    hide_ui: bool,

    /// Open UI panels in `--shot` captures, comma-separated: `dev`, `layers`,
    /// `places`, `ai`, `avatar`, `stats`, `route`, or `all`. The menu bar and
    /// HUD already show (unless `--hide-ui`); this documents a panel too, e.g.
    /// `--ui dev` to capture the lighting/weather controls. `route` also seeds a
    /// demo route, so the planner is captured with a result in it.
    #[arg(long, value_name = "LIST", value_delimiter = ',')]
    ui: Vec<String>,

    /// Camera mode for `--shot`: "free" (exact placement) or "earth" (orbit).
    #[arg(long, default_value = "free")]
    shot_mode: String,

    /// Densify the `--at` waypoints (two or more) into an N-frame sweep: capture N
    /// viewpoints interpolated along the path (altitude log-interpolated) instead
    /// of one per `--at`. With `--csv` this A/B's the altitude-driven ramps
    /// (space_factor, exposure, bloom, albedo) for a hard cut. 0 = off.
    #[arg(long, default_value_t = 0, value_name = "N")]
    frames: u32,

    /// Write per-capture metrics (altitude, tier, ramp scalars, mesh tally) as one
    /// CSV row each — the sweep analysis artifact (see `systems::shot_harness`).
    #[arg(long, value_name = "PATH")]
    csv: Option<PathBuf>,

    /// Dump the full per-capture metrics as JSON on exit (one object for a single
    /// shot, an array for a sweep) — inspect the settled world state from a shell.
    #[arg(long, value_name = "PATH")]
    dump_state: Option<PathBuf>,

    /// Assert a metric on every capture and exit non-zero if it does not hold —
    /// what makes `--shot` a CI gate rather than only an instrument. Repeatable:
    /// `--expect settled==true --expect globe_rendered>0`. Any `--dump-state`
    /// field is a valid key (an unknown one is an error, not a silent pass);
    /// operators are `>= <= == != > <`, booleans are `true`/`false`. Use `>=`
    /// rather than `==` on a float.
    #[arg(long, value_name = "KEY<OP>VALUE")]
    expect: Vec<String>,

    /// Time of day in hours (0–24) to start at — the main knob for checking the
    /// lighting curves in a `--shot` capture (e.g. `--time 6` for sunrise).
    #[arg(long, value_name = "HOURS")]
    time: Option<f32>,

    /// Distance-fog density to start at (0–1); pairs with `--time` for isolating
    /// how much of a washed-out frame is the fog and how much is the lighting.
    #[arg(long, value_name = "DENSITY")]
    fog: Option<f32>,

    /// Cloud coverage to start at (0–1): fraction of the sky the procedural
    /// cloud deck fills. 0 = clear sky. Handy for A/B'ing weather in `--shot`.
    #[arg(long, value_name = "COVERAGE")]
    clouds: Option<f32>,

    /// Rain intensity to start at (0–1): density of the screen-space rain
    /// streaks. 0 = dry. Pairs with `--clouds` for an overcast, wet scene.
    #[arg(long, value_name = "INTENSITY")]
    rain: Option<f32>,

    /// Pull real conditions for the map anchor from met.no and drive the
    /// cloud/fog/rain knobs from them (overrides `--clouds`/`--rain`/`--fog`).
    /// In `--shot` mode the capture waits for the first sample (capped by `--wait`).
    #[arg(long)]
    weather_live: bool,

    /// Switch the **height layer** on for this run (`systems::height`): measured
    /// AHN elevation from PDOK, over the Netherlands only. Off by default because
    /// it reaches a third party's service; this is how a scripted capture opts in,
    /// since a headless run has no Layers panel to tick.
    #[arg(long)]
    height: bool,
}

/// Resolve the scripted capture the CLI asked for, if any.
///
/// Two ways in, and they don't mix: `--shots` runs the remembered camera
/// positions from a preset manifest (the documentation build step), `--shot`
/// places one or more ad-hoc viewpoints from `--at`/`--look`. Both are built
/// here, before the app, because the result decides whether the run gets a
/// window at all and how big its surface is.
fn build_shot_harness(cli: &Cli) -> Result<Option<systems::shot_harness::ShotHarness>, String> {
    use systems::shot_harness::{ShotHarness, DEFAULT_SETTLE, DEFAULT_SIZE, DEFAULT_WAIT};

    if let Some(manifest) = cli.shots.clone() {
        if cli.shot.is_some() {
            return Err("--shots and --shot are alternatives; pass one".into());
        }
        // `--shots` with no value falls back to the checked-in manifest.
        let path = manifest.unwrap_or_else(systems::shot_presets::default_manifest_path);
        return ShotHarness::from_manifest(
            &path,
            &cli.only,
            cli.size.as_deref(),
            cli.settle,
            cli.wait,
            cli.csv.clone(),
            cli.dump_state.clone(),
            &cli.expect,
        )
        .map(Some)
        .map_err(|e| format!("--shots: {e}"));
    }
    if !cli.only.is_empty() {
        return Err("--only names presets from a --shots manifest".into());
    }
    let Some(path) = cli.shot.clone() else {
        return Ok(None);
    };
    ShotHarness::from_cli(
        path,
        &cli.at,
        &cli.look,
        cli.size.as_deref().unwrap_or(DEFAULT_SIZE),
        cli.settle.unwrap_or(DEFAULT_SETTLE),
        cli.wait.unwrap_or(DEFAULT_WAIT),
        cli.hide_ui,
        cli.ui.clone(),
        cli.shot_mode.clone(),
        cli.frames,
        cli.csv.clone(),
        cli.dump_state.clone(),
        &cli.expect,
    )
    .map(Some)
    .map_err(|e| format!("--shot: {e}"))
}

/// Force Bevy onto the stable CPU light-clustering path.
///
/// Bevy 0.19's GPU-driven clustering grows its per-view cluster buffers via an
/// async GPU readback (`cluster::gpu::cluster_on_gpu` → `schedule_readback_buffer_map`).
/// When the view frustum gets large — e.g. zooming the Earth camera out to altitude
/// — that readback-and-grow path races/frees its staging buffer and loses the wgpu
/// device, which then makes every downstream buffer creation that frame (the
/// atmosphere env-map generation, the clustering uploads) fail with "buffer invalid"
/// across threads. CPU clustering has no such readback and is far more than enough
/// for this scene's handful of clustered lights. `GlobalClusterSettings` documents
/// `gpu_clustering = None` as the supported off-switch; it is an `ExtractResource`,
/// so the `None` is cloned into the render world every frame and gates every
/// GPU-clustering system (`.run_if(gpu_clustering_is_enabled)`) off.
struct ForceCpuClusteringPlugin;

impl Plugin for ForceCpuClusteringPlugin {
    fn build(&self, _app: &mut App) {}

    // `finish` hooks run in plugin-add order, so adding this plugin after
    // DefaultPlugins guarantees it runs after `PbrPlugin::finish` has inserted
    // `GlobalClusterSettings` into the main world.
    fn finish(&self, app: &mut App) {
        // Native only. The device-loss this avoids is the Intel iGPU's
        // command-buffer timeout on the cluster readback during a zoom-out — a
        // native-driver failure. The browser build reaches the GPU through
        // WebGPU's own submission and timeout handling, and there CPU clustering
        // is not a safety net but the single most expensive thing in the frame:
        // it is one of the fallbacks that made the web build cost seconds per
        // frame. Forcing it on where the hazard doesn't apply would spend that
        // cost for nothing. `CARTO_FORCE_CPU_CLUSTERING=1` restores it anywhere.
        let force =
            cfg!(not(target_arch = "wasm32")) || crate::env_flag("CARTO_FORCE_CPU_CLUSTERING");
        if !force {
            return;
        }
        if let Some(mut settings) = app
            .world_mut()
            .get_resource_mut::<bevy::light::cluster::GlobalClusterSettings>()
        {
            settings.gpu_clustering = None;
            tracing::info!(
                "GPU light clustering disabled; using CPU clustering (avoids the \
                 readback device-loss when zooming out)"
            );
        }
    }
}

/// The command line, on platforms that have one.
///
/// Android does not. An app process is forked from the zygote, so the `argv` that
/// std's init-array constructor captured is `app_process`'s, with argv[0]
/// overwritten by the package name and whatever else the runtime left in the
/// array — not a command line anyone wrote, and not one this `Cli` has heard of.
/// Exactly what survives there is a runtime detail that varies by Android version,
/// which is the point: `Cli::parse()` does not fall back to defaults on input it
/// dislikes, it prints usage and calls `exit(2)`, so the app would die on launch
/// with a blank screen and one clap line in `logcat` — on some devices, in some
/// releases. A fixed configuration is the only version of this with a predictable
/// outcome.
///
/// `--offline` is that configuration, and it is a deliberate choice rather than a
/// stub: Phase-1 Android is the same shape as Phase-1 web (`docs/notes/android-build.md`)
/// — the streamed OSM/3DBAG map plus a local avatar, no sim, no auth, no world
/// persistence. It also happens to be what the build produces anyway, since the
/// APK is built `--no-default-features` and so carries no `solo` sim to connect to;
/// saying it explicitly keeps `login::resolve` from going looking for a credentials
/// file that cannot exist on the device.
#[cfg(not(target_os = "android"))]
fn parse_cli() -> Cli {
    Cli::parse()
}

#[cfg(target_os = "android")]
fn parse_cli() -> Cli {
    // `parse_from` rather than a hand-built struct literal: every field keeps its
    // clap-declared default, so a new flag added to `Cli` cannot leave this arm
    // silently uninitialised.
    Cli::parse_from(["cartopolis", "--offline"])
}

/// Build and run the client. The `cartopolis` binary is a one-line shim over this
/// (`src/main.rs`); everything lives in the lib target so `cargo test` links the
/// rlib instead of compiling all ~52k lines a second time under `cfg(test)`.

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
fn install_android_panic_hook() {
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
        let _ = std::fs::write(crash_report_path(), report);
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
fn run_breadcrumb_path() -> std::path::PathBuf {
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
fn tail_bytes(text: &str, max: usize) -> &str {
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
fn offer_last_crash() {
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

/// How many times the renderer may be rebuilt from scratch before the app stops
/// trying.
///
/// A cap, not a policy: a device that has genuinely gone (a driver reset that
/// takes the GPU with it, a phone that has decided this process may not have one)
/// answers every attempt the same way, and an uncapped loop would spend the
/// battery re-initialising wgpu forever. Two is enough to cross a *transient* —
/// the one this exists for is the Android pause/resume that a system dialog
/// causes, where the surface goes away underneath a frame in flight.
const MAX_RENDER_RECOVERIES: u32 = 2;

/// Rebuild attempts spent so far. A `RenderErrorHandler` is a bare `fn` pointer
/// with no captured state and no `&mut self`, so the counter has to live out
/// here.
static RENDER_RECOVERIES: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);
/// Set once the renderer has been halted for good, so the halt is logged and
/// recorded once rather than on every frame for the rest of the run.
static RENDER_HALTED: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

/// What to do about a `RenderError`. **Never end the app**, and — this is the part
/// that changed — do not always freeze it either.
///
/// Bevy's default handler writes `AppExit::error()` on any `RenderError`, and its
/// own comment calls that overzealous. It cost this project a day: the first phone
/// the client ever ran on lacked `FLOAT32_FILTERABLE`, four atmosphere LUT bind
/// groups failed validation, and the client *vanished* three seconds in — aborting
/// inside libc on the way out, so there was no Rust panic, no exit message, and a
/// crash report that could only say "the process was killed".
///
/// `StopRendering` was the answer to that and it kept the log flowing, but it is
/// **terminal**: bevy leaves the render state `Errored` and never draws again, so
/// the app the user is holding is a still photograph of the last good frame that
/// answers no input. That is indistinguishable from a hang, and it is what a
/// phone reports as "it froze, I had to restart it". Applying that to *every*
/// error class meant one lost surface — the ordinary consequence of a system
/// dialog pausing an Android activity — cost the whole session.
///
/// So the answer is by kind:
///
/// - **`DeviceLost`** is the one bevy's recovery path exists for (it is delivered
///   by wgpu's own `set_device_lost_callback`), and it is the one that is
///   plausibly transient: the device came back, our handles did not. Rebuild, up
///   to [`MAX_RENDER_RECOVERIES`], with `RenderCreation::Automatic` — the same
///   thing `RenderPlugin`'s default builds the renderer with at startup, so
///   recovery lands on the configuration the app already had rather than on a
///   second, divergent one.
/// - **`Validation`** is a bug in how we drive wgpu and it recurs the moment
///   rendering resumes. Rebuilding cannot fix it and would loop; `Ignore` would
///   resume straight into it, and bevy's own warning on that variant is rapid
///   flashing, which is not a thing to do to a handheld screen. Halt.
/// - **`OutOfMemory`** likewise: the scene that would not fit is still the scene.
///   Halt rather than thrash a device that is already short.
///
/// **A halt is recorded, not just logged.** The point of the whole crash-reporter
/// apparatus is that a phone has no cable; a frozen picture with the explanation
/// only in `logcat` is the same undiagnosable freeze as before. So the first halt
/// writes the report the *next* launch hands to the share sheet
/// (`offer_last_crash`), which is how `FLOAT32_FILTERABLE` was named in one round
/// trip. Once only — bevy calls this handler every frame for as long as the state
/// stays `Errored`.
fn handle_render_error(
    error: &bevy::render::error_handler::RenderError,
    _main_world: &mut World,
    _render_world: &mut World,
) -> bevy::render::error_handler::RenderErrorPolicy {
    use bevy::render::error_handler::{ErrorType, RenderErrorPolicy};
    use std::sync::atomic::Ordering;

    let recoverable = matches!(error.ty, ErrorType::DeviceLost);
    if recoverable {
        let spent = RENDER_RECOVERIES.fetch_add(1, Ordering::Relaxed);
        if spent < MAX_RENDER_RECOVERIES {
            tracing::warn!(
                kind = ?error.ty,
                description = %error.description,
                attempt = spent + 1,
                "render device lost — rebuilding the renderer"
            );
            return RenderErrorPolicy::Recover(bevy::render::settings::RenderCreation::Automatic(
                default(),
            ));
        }
    }

    // `swap` rather than a load-then-store: this is reached every frame from here
    // on, and the report and the log line are for the *first* one.
    if !RENDER_HALTED.swap(true, Ordering::Relaxed) {
        tracing::error!(
            kind = ?error.ty,
            description = %error.description,
            recoveries = RENDER_RECOVERIES.load(Ordering::Relaxed),
            "render error — halting the renderer, keeping the app alive"
        );
        record_render_halt(error);
    }
    RenderErrorPolicy::StopRendering
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
fn record_render_halt(error: &bevy::render::error_handler::RenderError) {
    let report = format!(
        "cartopolis {} ({})\nabi: {}\nThe renderer was halted by a {:?} error and the \
         picture is frozen from here on; the process itself is still running. This is not \
         a crash — it is the deliberate response to a GPU error we cannot resume through. \
         Rebuild attempts spent: {}.\nerror: {}\n",
        env!("CARGO_PKG_VERSION"),
        option_env!("CARTO_BUILD_SHA").unwrap_or("unknown build"),
        std::env::consts::ARCH,
        error.ty,
        RENDER_RECOVERIES.load(std::sync::atomic::Ordering::Relaxed),
        error.description,
    );
    let _ = std::fs::write(crash_report_path(), report);
}

#[cfg(not(target_os = "android"))]
fn record_render_halt(_error: &bevy::render::error_handler::RenderError) {}

/// Start the client.
///
/// On the web this is deliberately *not* the whole of startup: the persistent
/// store is IndexedDB, which has no synchronous read, and `UserStore::load` runs
/// while the `App` is being built. So the keys that must be readable
/// synchronously are pulled into memory first and the app is built inside the
/// completion — a few milliseconds behind a page that is already showing its
/// loader, in exchange for every startup path downstream (the quality preset, the
/// layer and sky restores, the remembered location, the session decision) working
/// exactly as it does natively instead of on a hydration event.
///
/// `spawn_local` and not a `block_on`: there is no blocking on a browser's main
/// thread, and Bevy's winit backend hands control to the event loop rather than
/// looping here, so starting it from inside a promise callback is ordinary.
pub fn run() {
    #[cfg(target_arch = "wasm32")]
    wasm_bindgen_futures::spawn_local(async {
        use crate::platform::storage;
        storage::preload(systems::user_store::USER_KEY, "").await;
        // Thumbnails only — a row draws them inside an egui pass and cannot wait.
        // The full-size photos beside them are fetched when someone opens one.
        storage::preload(
            utils::photo_store::PHOTO_PREFIX,
            utils::photo_store::THUMB_SUFFIX,
        )
        .await;
        run_client();
    });
    #[cfg(not(target_arch = "wasm32"))]
    run_client();
}

fn run_client() {
    dotenvy::dotenv().ok();

    #[cfg(target_os = "android")]
    install_android_panic_hook();
    // Before anything else can fail: if the last run died, let the user send the
    // reason somewhere useful without needing a cable.
    #[cfg(target_os = "android")]
    offer_last_crash();
    #[cfg(target_os = "android")]
    let _ = std::fs::write(run_breadcrumb_path(), "");
    // Started here, before the `App` exists, because the failure it reports is one
    // the `App` cannot: a main loop that stops. Everything after this line runs
    // watched (`platform::watchdog`).
    platform::watchdog::start();

    // Resolve the render budget first: it decides how wide the worker pool below
    // is allowed to be, and every later reader (`setup_camera`, the streamers)
    // expects it settled before the first frame. See `quality`.
    quality::init(systems::user_store::UserStore::preferred_quality());

    // Start the CPU worker pool before anything streams (ADR-033). A no-op
    // natively — `AsyncComputeTaskPool` is already a thread pool — and on the web
    // it spawns the Web Workers that keep tile decoding off the render thread.
    // One core is left for the main thread, which still renders and fetches.
    //
    // …and on a phone, rather more than one. `available_parallelism` counts the
    // Fairphone 4's 8 cores without saying that 6 of them are efficiency cores
    // sharing a thermal envelope with the render thread and the compositor, so
    // the honest ceiling is well under the count (`Budget::worker_cap`).
    let workers = std::thread::available_parallelism()
        .map(|n| n.get().saturating_sub(1))
        .unwrap_or(4)
        .max(1);
    platform::workers::start(match quality::budget().worker_cap {
        Some(cap) => workers.min(cap),
        None => workers,
    });

    let cli = parse_cli();

    // Desktop: our own `tracing_subscriber` (env-filtered fmt to stderr). On wasm we
    // don't init it — its default timer calls `SystemTime::now`, which panics on
    // wasm, and its writer targets stdout; Bevy's `LogPlugin` (kept enabled on wasm
    // below) routes logs/panics to the browser console instead. ADR-033.
    //
    // Android is on the wasm side of this, and for a closely related reason: it is
    // a native target, but **stderr on Android goes nowhere**. Nothing forwards a
    // process's standard streams to logcat unless the app arranges it, so this
    // subscriber would swallow every line the app ever logs, on the one platform
    // where the first run is entirely diagnosis. Bevy's `LogPlugin` has an
    // `android_log-sys` layer that writes to logcat properly, so it stays enabled
    // there and this does not run. See `docs/notes/android-build.md`.
    //
    // What *was* wrong here until 2026-08-14 is the unstated half: "this does not
    // run" was read as "nothing else writes to stderr on Android", and `LogPlugin`
    // was quietly adding its own formatted layer on top of the Android one — see
    // the `fmt_layer` note where that plugin is configured below. The device log
    // showed every line twice.
    #[cfg(all(not(target_arch = "wasm32"), not(target_os = "android")))]
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                // wgpu/naga at `warn` so GPU validation + device-loss errors
                // surface (they're the real cause of the zoom-out crash; the
                // panics only show the aftermath). Override with RUST_LOG.
                //
                // `bevy_render=error` is not redundant with those: a validation
                // failure is reported by Bevy's *own* `bevy_render::error_handler`,
                // which then writes the `AppExit` that ends the run — and the `off`
                // default silenced exactly that. The observed failure was a `--shot`
                // that exited 0 in 3 s with no PNG and an empty log; the wgpu
                // targets above said nothing, because the error never came from
                // them. A run that dies for a render reason must say so.
                .unwrap_or_else(|_| {
                    tracing_subscriber::EnvFilter::new(
                        "off,cartopolis=info,bevy_render=error,wgpu=warn,wgpu_core=warn,wgpu_hal=warn,naga=warn",
                    )
                }),
        )
        .init();

    let mut app = App::new();
    // The scripted capture is configured *before* the app is built: a preset
    // manifest can set the render size, and a windowless run needs that size to
    // stand a `HeadlessSurface` in for the missing `Window`. A bad manifest or
    // `--at` therefore fails here with a message, not as a black run.
    let harness = match build_shot_harness(&cli) {
        Ok(h) => h,
        Err(e) => {
            eprintln!("{e}");
            std::process::exit(2);
        }
    };
    // A scripted run is windowless (see `HeadlessSurface`): it renders into an
    // offscreen image, so the shot size is the whole surface — there is nothing
    // for egui to lay out against but this.
    let shot_size = harness.as_ref().map(|h| h.size);
    let scripted = harness.is_some();
    // Self-install the Linux desktop file + themed icon before the window exists,
    // so Wayland compositors (where set_window_icon is a no-op) can resolve the
    // app icon on first map. Idempotent, best-effort, no-op off Linux and for
    // windowless scripted captures. See `systems::desktop_integration`.
    if !scripted {
        systems::desktop_integration::ensure_desktop_integration();
    }
    let asset_dir = ASSET_ROOT.to_string();
    let plugins = DefaultPlugins.build();
    // Desktop uses the custom `tracing_subscriber` above, so drop Bevy's LogPlugin
    // there. On wasm keep it: it wires logs + panics to the browser console and does
    // not call `SystemTime::now` (which panics on wasm). ADR-033.
    #[cfg(all(not(target_arch = "wasm32"), not(target_os = "android")))]
    let plugins = plugins.disable::<LogPlugin>();
    // Android keeps LogPlugin too (stderr is discarded there — see above), but not
    // its defaults: it filters only `wgpu=error,naga=warn` and lets everything else
    // through at INFO, which buries the app's own lines under Bevy's startup and
    // asset chatter on a device where the log is the only instrument there is.
    //
    // The per-target directives started as the desktop set, and two of them have
    // since been loosened, because the desktop set is tuned for a platform where
    // the log is a convenience and this is the one where it is the only instrument.
    //
    // `bevy_render` and `bevy_winit` run at INFO rather than the desktop's `error`.
    // That is not chatter: between them they log the adapter and backend actually
    // chosen and the surface's creation and lifecycle, which is precisely the
    // ground a launch that dies before its first frame dies on. Carrying
    // `bevy_render=error` from the desktop meant the first hardware run in this
    // repo's history (2026-08-12, a process killed before any frame) produced a
    // crash report that could not say whether wgpu had ever obtained a device.
    // `level` is the *global* default that bevy prepends, and it is WARN rather
    // than the desktop's `off` for the same reason: a warning from a crate nobody
    // thought to name is exactly the thing worth reading here. Tighten all three
    // once the phone is boring — and note `offer_last_crash` mails this log to
    // whoever is holding the device, so the filter is the report's contents too.
    //
    // **And every line was going to logcat twice.** `LogPlugin` adds bevy's
    // `AndroidLayer` *in addition to* a `tracing_subscriber::fmt` layer writing to
    // stderr — `fmt_layer`'s default is `|_| None`, and `None` means "use the
    // stderr one", not "use none". Android captures a NativeActivity's stderr into
    // logcat under `RustStdoutStderr`, so the 2026-08-14 device log carried each
    // event as a clean `event` line *and* as an ANSI-escaped duplicate: twice the
    // logcat traffic, twice the formatting, and a crash report interleaved with
    // `[2m[31m` for a reader with no terminal to interpret it.
    //
    // It is also a hazard and not only waste. That stderr is a pipe with a fixed
    // kernel buffer, drained by a pump thread; a writer that outruns the pump
    // **blocks in `write`**, and the writer here is whichever thread logged —
    // usually the one running the schedule. A burst is exactly what a re-anchor
    // produces. Sinking the formatted layer removes the duplicate, the escape
    // codes and the pipe together; `AndroidLayer` is the one that reaches logcat
    // properly, and it is the one `own_logcat` reads back.
    #[cfg(target_os = "android")]
    let plugins = plugins.set(LogPlugin {
        filter: "cartopolis=info,bevy_render=info,bevy_winit=info,wgpu=warn,wgpu_core=warn,\
                 wgpu_hal=warn,naga=warn"
            .into(),
        level: bevy::log::Level::WARN,
        // Not `|_| None` — see above; that is the default and it selects stderr.
        fmt_layer: |_| {
            Some(Box::new(
                tracing_subscriber::fmt::Layer::default().with_writer(std::io::sink),
            ))
        },
        ..default()
    });
    let default_plugins = plugins
        // Bevy's defaults cap **both** the io and async-compute pools at 4 threads
        // (`bevy_app::TaskPoolOptions`), and the whole map streamer lives in those
        // two: `platform::http` is a *blocking* `ureq` call that parks an io thread
        // for a full round trip, and `platform::workers` hands every MVT
        // rasterisation to `AsyncComputeTaskPool`. Filling the clipmap after a
        // teleport queues ~400 tiles at ~120 ms of tiny-skia scan conversion each
        // (`docs/notes/tile-raster-cost.md`) — so on a 22-core workstation the map
        // filled 4-wide with 14 cores idle, and rasterisation, not the network, was
        // the thing making you wait.
        //
        // Measured 2026-07-31 on a 22-core box, `--shot` over Groningen on
        // `dev-bevy` (the profile now called `dev`), seconds for the streamer to go quiet — 4/4 → these widths
        // (io=7, cpu=10): 3 km cold 12.6 → 7.5, street cold 15.7 → 9.8, street
        // **warm** 13.3 → 4.1. That warm row is the tell: a full on-disk cache
        // barely helped at 4/4, because what you were waiting for was never the
        // network. Uploads were not the limit either (the decoded backlog stayed
        // under 8 throughout), which is why widening *this* pool is what moved.
        //
        // The io threads block on a socket rather than burning a core, so they can
        // be oversubscribed; the async-compute ones are genuinely CPU-bound, so the
        // percentages deliberately leave Bevy's own `compute` pool (its parallel
        // system executor, which runs every frame forever, unlike a fill) a real
        // share rather than the two threads a greedier split left it.
        .set(bevy::app::TaskPoolPlugin {
            task_pool_options: bevy::app::TaskPoolOptions {
                io: bevy::app::TaskPoolThreadAssignmentPolicy {
                    min_threads: 2,
                    max_threads: 8,
                    percent: 0.30,
                    on_thread_spawn: None,
                    on_thread_destroy: None,
                },
                async_compute: bevy::app::TaskPoolThreadAssignmentPolicy {
                    min_threads: 2,
                    // Bevy's own default width, and no longer ours to cap.
                    //
                    // `Budget::worker_cap` used to be enforced here, because
                    // `platform::workers::submit` *was* `AsyncComputeTaskPool` on
                    // native and `workers::init` was a no-op off the web. The tile
                    // jobs have their own pool now (`platform::workers`), which is
                    // where that cap and the Android `nice` both moved.
                    //
                    // What is left on this pool is Bevy's own async work — and the
                    // reason the split matters: **pipeline compilation** is spawned
                    // here (`pipeline_cache::create_pipeline_task`, on exactly
                    // `not(wasm32) + not(macos) + multi_threaded`, i.e. Android), as
                    // is the screenshot readback. Both used to queue behind hundreds
                    // of 120 ms blocking rasterises with no yield point. Do not put
                    // a cap back on this without moving the tile jobs back too.
                    max_threads: usize::MAX,
                    percent: 0.45,
                    on_thread_spawn: None,
                    on_thread_destroy: None,
                },
                ..default()
            },
        })
        .set(WindowPlugin {
            // No window at all for a scripted run — see the headless block below.
            primary_window: (shot_size.is_none()).then(|| Window {
                title: "Cartopolis".into(),
                // Stable app-id: Wayland `app_id` / X11 `WM_CLASS`. The compositor
                // keys the taskbar/titlebar icon off this, matching the
                // `cartopolis.desktop` entry that `systems::desktop_integration`
                // self-installs at startup.
                name: Some("cartopolis".into()),
                resolution: WindowResolution::new(1280, 720),
                // NOT vsync (FIFO): on Wayland, when this window is occluded the
                // compositor stops releasing swapchain images, FIFO's acquire then
                // blocks ~1 s per frame (the watchdog's eerily constant ~1024 ms
                // "slow frames" with a near-idle GPU), and submissions waiting on
                // the never-signaled acquire semaphore eventually trip i915's hang
                // detection → `VK_ERROR_DEVICE_LOST` → the app dies in
                // `Buffer::get_mapped_range` panics. Mailbox/Immediate never block
                // on occlusion. This was misdiagnosed for a long time as content
                // overload (tiles/buildings/shadows/atmosphere) because crashes
                // struck whenever the window sat covered while logs were read.
                //
                // Android is the exception, and it is the opposite trade. There is
                // no Wayland compositor and no i915 here, so none of the above
                // applies — what "no vsync" buys on a handset is frames the
                // SurfaceFlinger throws away, paid for in heat on a device that
                // thermally throttles rather than in anything the user can see.
                // FIFO is also what every other Android app is doing, so it is what
                // the display pipeline is tuned for.
                present_mode: if cfg!(target_os = "android") {
                    bevy::window::PresentMode::AutoVsync
                } else {
                    bevy::window::PresentMode::AutoNoVsync
                },
                // On the web the canvas otherwise keeps the fixed `resolution`
                // above forever — 1280x720 pinned in the corner of whatever size
                // the browser window actually is, with dead space around it, and
                // no response to resizing. This tracks the parent element instead.
                //
                // The field's documented hazard is a parent sized *by its
                // children*, which feeds back into the canvas growing every
                // resize. Ours is `#stage`, given an explicit pixel size by
                // index.html's render-scale code, so its size comes from the
                // viewport and never from the canvas inside it.
                //
                // `resolution` still applies natively, and is the initial size on
                // the web until the first fit.
                #[cfg(target_arch = "wasm32")]
                fit_canvas_to_parent: true,
                // Adopt the canvas index.html already put on the page rather than
                // letting winit append one to `<body>`.
                //
                // This is what makes the render scale possible, and the mechanism
                // is worth stating because none of it is visible from Rust. On the
                // web the render resolution is *not* something this process picks:
                // wgpu's surface `configure` writes `canvas.width/height` from the
                // window's physical size, and that physical size is whatever
                // winit's `ResizeObserver` measured the element's device-pixel
                // content box to be — i.e. CSS size × `devicePixelRatio`. Bevy's
                // `scale_factor_override` does not help, because winit's web
                // `request_inner_size` sets the *CSS* size and returns `None`;
                // asking for fewer physical pixels there just shrinks the visible
                // canvas.
                //
                // So the only place the decision can be made is in page layout,
                // and index.html makes it: `#stage` is sized to a fraction of the
                // viewport and scaled back up with a CSS transform, which leaves
                // the canvas covering the screen while its backing store — every
                // pixel this renderer pays for — is smaller. See the
                // `RENDER SCALE` block there.
                #[cfg(target_arch = "wasm32")]
                canvas: Some("#cartopolis-canvas".into()),
                ..default()
            }),
            // With no window there is nothing to close, and the harness writes its
            // own `AppExit` when the last capture lands.
            exit_condition: if shot_size.is_some() {
                bevy::window::ExitCondition::DontExit
            } else {
                bevy::window::ExitCondition::OnAllClosed
            },
            close_when_requested: shot_size.is_none(),
            ..default()
        })
        .set(AssetPlugin {
            file_path: asset_dir,
            // No `.meta` sidecars are shipped, so don't look for them. On the web
            // build the server answers each missing `<asset>.meta` request with
            // non-meta content, which otherwise spams "failed to deserialize meta"
            // (harmless — Bevy falls back to defaults — but noisy). ADR-033.
            meta_check: bevy::asset::AssetMetaCheck::Never,
            ..default()
        })
        // big_space owns transform propagation (ADR-019); Bevy's TransformPlugin
        // must be disabled or big_space errors at startup.
        .disable::<bevy::transform::TransformPlugin>();

    // Loaded before the app is built, not inside the chain below, because the
    // remembered session in it decides which world this launch enters (and seeds
    // the sign-in screen) — see the connection-mode block further down.
    // Trim the disk cache to its ceiling before the streamers start refilling it.
    // Nothing else ever reclaims it, and on a phone an unbounded cache is the app
    // quietly eating the user's storage. Runs on its own thread, so this returns at
    // once. `utils::cache` is compiled out on wasm, where the browser's quota is the
    // bound instead.
    #[cfg(not(target_arch = "wasm32"))]
    utils::cache::sweep_cache_root();

    let user_store = systems::user_store::UserStore::load();
    let session_prefs = user_store.data.session.clone();
    // Lifted out before the store is moved into the world, so the Settings panel
    // comes up holding what the user last chose rather than the defaults. This is
    // also why `AppPrefs` is `Copy`.
    let app_prefs = user_store.data.settings;

    // Headless (`--shot`): no window, and therefore no winit event loop to pump
    // the app — `ScheduleRunnerPlugin` drives the frames instead, as fast as the
    // GPU allows, until the harness writes its `AppExit`. wgpu needs no surface
    // to render into an image, so the whole run happens off the desktop: nothing
    // pops up, nothing can occlude (and blacken) a capture, and it works over SSH
    // or in CI with no compositor at all.
    let default_plugins = match shot_size {
        Some(_) => default_plugins.disable::<bevy::winit::WinitPlugin>().add(
            bevy::app::ScheduleRunnerPlugin::run_loop(std::time::Duration::ZERO),
        ),
        None => default_plugins,
    };

    app.add_plugins(default_plugins)
        // `BigSpaceTimingStatsPlugin` is disabled rather than the group being
        // swapped for `BigSpaceMinimalPlugins`, and the difference matters: the
        // default group is Minimal + timing + *validation*, and the validator is
        // already `#[cfg(not(debug_assertions))]`-disabled, so it costs a shipped
        // build nothing and is a genuinely useful hierarchy check while
        // developing. Dropping the group would throw that away to remove the
        // timing.
        //
        // The timing plugin, by contrast, is pure overhead **in every profile**:
        // four systems in `PostUpdate`, ~10 `Instant::now()` per frame inside the
        // propagation systems, and an `update_averages` that clones both stat
        // structs into a 64-deep `VecDeque` and recomputes a 64-entry mean every
        // frame — to populate `PropagationStats` and `GridHashStats`, which
        // nothing in this tree reads. Upstream keeps it on by default because
        // upstream's examples display it; we have no such display.
        .add_plugins(
            big_space::prelude::BigSpaceDefaultPlugins
                .build()
                .disable::<big_space::plugin::BigSpaceTimingStatsPlugin>(),
        )
        // Added after DefaultPlugins so its `finish` runs after PbrPlugin's (which
        // creates GlobalClusterSettings) — see the plugin's doc comment.
        .add_plugins(ForceCpuClusteringPlugin)
        .add_plugins(EguiPlugin)
        // Renders the 3D world below the screen's resolution on a tier that asks
        // for it, and no-ops entirely on the tiers that do not. Must come after
        // `EguiPlugin`, which is what puts `egui_pass` after `upscaling` — the
        // ordering that keeps the interface at full resolution while the world
        // scales.
        .add_plugins(systems::render_scale::RenderScalePlugin)
        // The atmosphere is Bevy 0.19's built-in bevy_pbr Atmosphere (added by
        // PbrPlugin); it's attached as a component to the camera in setup_camera.
        .add_plugins(MaterialPlugin::<systems::data_layers::PinHeadMaterial>::default())
        .add_plugins(MaterialPlugin::<StarFieldMaterial>::default())
        .add_plugins(MaterialPlugin::<MoonMaterial>::default())
        .add_plugins(MaterialPlugin::<HorizonHazeMaterial>::default())
        .add_plugins(MaterialPlugin::<CloudMaterial>::default())
        .add_plugins(MaterialPlugin::<GlobeCloudMaterial>::default())
        .add_plugins(MaterialPlugin::<RainMaterial>::default())
        .add_plugins(MaterialPlugin::<systems::water::WaterMaterial>::default())
        .add_plugins(MaterialPlugin::<systems::globe_ocean::GlobeSurfaceMaterial>::default())
        .init_resource::<GameState>()
        .init_resource::<AvatarState>()
        .init_resource::<LocalAvatarSimId>()
        .init_resource::<CameraState>()
        .init_resource::<MouseState>()
        .init_resource::<systems::touch::TouchGestures>()
        .init_resource::<systems::tile_loader::TileCache>()
        // A scripted run never sees the server handshake that normally supplies the
        // tile template, and the built-in fallback is the raster OSM basemap — which
        // the route planner cannot solve against. A preset manifest may therefore pin
        // its own source; `CARTO_TILE_URL` still wins over it (see `shot_presets`).
        .insert_resource(match harness.as_ref().and_then(|h| h.tile_url.clone()) {
            Some(url) if resources::tile_url_override().is_none() => {
                OsmTileUrlTemplate(std::sync::Arc::new(std::sync::Mutex::new(url)))
            }
            _ => OsmTileUrlTemplate::default(),
        })
        .init_resource::<systems::settings::SettingsState>()
        // The dock's mark disc and the card it opens (`systems::ui`), and whether
        // the status readout is showing (`systems::hud`, off until the ⓘ disc
        // under the compass is pressed).
        .init_resource::<systems::ui::MainMenu>()
        .init_resource::<systems::hud::StatusHud>()
        .init_resource::<systems::onscreen_keyboard::OnScreenKeyboard>()
        // What the on-screen navigation controls asked for this frame
        // (`systems::map_osd`); read by `free_camera::camera_controls`.
        .init_resource::<systems::map_osd::OsdInput>()
        // The user's own preferences — units, turn chimes, keep-screen-on, the
        // quality preset. Distinct from `DevPanelState` (a debugging surface) and
        // from `LayerPrefs` (what is drawn); this is what the Settings panel owns,
        // and `render_settings_panel` reads it unconditionally.
        .insert_resource(app_prefs)
        .init_resource::<ContextMenuState>()
        .init_resource::<EditDialogState>()
        .init_resource::<systems::gizmo::GizmoState>()
        .init_resource::<MarqueeState>()
        .init_resource::<TextureLibrary>()
        .init_resource::<PrimTextureCache>()
        .init_resource::<systems::rendering::MeshCache>()
        .init_resource::<systems::rendering::MaterialCache>()
        .insert_resource(AiConfig {
            api_key: std::env::var("ANTHROPIC_KEY").ok(),
            model: std::env::var("ANTHROPIC_MODEL")
                .unwrap_or_else(|_| "claude-haiku-4-5-20251001".to_string()),
        })
        .init_resource::<AiAssistantState>()
        .init_resource::<AvatarChooserState>()
        // Which of the sky knobs below came from a flag, so `restore_sky_prefs`
        // leaves them alone (a flag is an instruction for this launch).
        .insert_resource(systems::user_store::SkyCliOverrides {
            time: cli.time.is_some(),
            fog: cli.fog.is_some(),
            clouds: cli.clouds.is_some(),
            rain: cli.rain.is_some(),
            weather_live: cli.weather_live,
        })
        .insert_resource(DevPanelState {
            time_of_day: cli.time.unwrap_or(12.0).rem_euclid(24.0),
            // A `--shot` capture must be deterministic, and an explicit `--time`
            // pins the clock, so both start in Manual; a normal launch tracks the
            // real local time of wherever the map opens.
            sun_mode: if scripted || cli.time.is_some() {
                resources::SunMode::Manual
            } else {
                resources::SunMode::Auto
            },
            fog_density: cli.fog.unwrap_or(0.15).clamp(0.0, 1.0),
            cloud_cover: cli.clouds.unwrap_or(0.0).clamp(0.0, 1.0),
            precipitation: cli.rain.unwrap_or(0.0).clamp(0.0, 1.0),
            ..default()
        })
        .init_resource::<StatsState>()
        .init_resource::<resources::ScreenshotState>()
        .init_resource::<systems::osm_buildings::OsmBuildings>()
        .init_resource::<systems::lod22::Lod22Buildings>()
        .init_resource::<systems::collision::BuildingColliders>()
        .init_resource::<systems::height::HeightLayer>()
        .init_resource::<systems::terrain::TerrainField>()
        .insert_resource({
            // `--height` is the only way into the layer in a scripted run; an
            // interactive session ticks it in the Layers panel (or restores it from
            // `data/user.json`, which `apply_prefs` does after this).
            let mut layers = systems::data_layers::DataLayers::default();
            layers.height_enabled = cli.height;
            layers
        })
        .init_resource::<systems::transit_live::TransitLive>()
        .init_resource::<systems::transit_live::WorldClock>()
        // Default to Auto (live met.no) in interactive runs; keep scripted captures
        // deterministic — no network wait — unless `--weather-live` asks for it.
        .insert_resource(systems::weather::WeatherState::with_live(
            !scripted || cli.weather_live,
        ))
        .init_resource::<systems::transit_vehicles::TransitVehicles>()
        .init_resource::<systems::geo_nav::GeoNavState>()
        .init_resource::<systems::navigation::RouteState>()
        .init_resource::<systems::driver::DriverState>()
        .init_resource::<systems::route_ahead::RouteAhead>()
        .init_resource::<systems::bookmarks::BookmarksState>()
        .insert_resource(user_store)
        .init_resource::<systems::map_stream::MapStream>()
        .init_resource::<resources::MapAnchor>()
        .init_resource::<systems::map_geometry::MapSurfaces>()
        // Filled by the same streamer, cell for cell (`systems::street_labels`).
        .init_resource::<systems::world_places::WorldPlaces>()
        .init_resource::<systems::interact::InteractState>()
        .init_resource::<systems::street_labels::StreetLabelStore>()
        .init_resource::<systems::street_labels::StreetLabelSigns>()
        // Must come after the `WaterMaterial` plugin above: it mints the shared
        // handle out of `Assets<WaterMaterial>`.
        .init_resource::<systems::water::WaterSeaState>()
        .init_resource::<systems::vegetation::VegetationLod>()
        .init_resource::<systems::street_furniture::FurnitureLod>()
        .init_resource::<systems::globe::GlobeState>()
        .init_resource::<systems::globe::GlobeSurfaceAlbedo>()
        .init_resource::<systems::globe_ocean::GlobeOceanGloss>()
        // Space background: the atmosphere skybox covers this everywhere in the local
        // view, but it's hidden from orbit (ADR-025), so the sky reads as black space.
        .insert_resource(ClearColor(Color::BLACK))
        // A render error is answered by kind: rebuild the renderer, or halt it —
        // never by ending the app. See `handle_render_error`.
        .insert_resource(bevy::render::error_handler::RenderErrorHandler(
            handle_render_error,
        ))
        // 2048 rather than 4096: with sun + moon each casting 4 cascades, a night
        // frame renders 8 shadow maps — 2048² quarters the shadow-pass fill and
        // memory versus 4096² for a barely-perceptible near-shadow softness at this
        // scene's cascade distances.
        //
        // Halved again on the phone tier, which is not the same trade. There
        // shadows are off by default (`quality::Budget::shadows`), so this is what
        // a user who switches them back on pays: 1024² is a soft shadow on a
        // 540-px-wide render target, and 4 MB of depth rather than 16 MB on a
        // device where GPU memory is the same memory the 71 MB module is living
        // in.
        .insert_resource(DirectionalLightShadowMap {
            size: if quality::is_phone() { 1024 } else { 2048 },
        });

    // Connection mode (ADR-016, and `systems::session` for the runtime half):
    //  - `--connect ADDR` (or `server` in the login file) → external sim.
    //  - `--offline` or `--shot` → no sim, no auth, no sign-in screen.
    //  - otherwise → whatever `user.json` remembers, which on a fresh install is
    //    solo mode entered automatically — i.e. unchanged behaviour.
    //
    // None of it happens *here* any more: the mode is expressed as the session
    // driver's first request and applied on frame one, which is the same path the
    // sign-in screen uses at any later moment. A flag is an instruction for this
    // launch, so it wins over the remembered session and is not written back.
    app.init_resource::<AuthIdentity>();
    app.init_resource::<systems::chat::ChatState>();
    app.init_resource::<systems::script_editor::ScriptEditorState>();
    app.init_resource::<resources::RightPanel>();
    app.init_resource::<systems::session::SessionState>();
    let mut screen = systems::login_screen::LoginScreen::from_prefs(&session_prefs);
    // The web build used to be excluded here outright — it was the Phase-1 viewer
    // with no transport to a sim at all. It has one now (a WebSocket carrier), so
    // it takes this path like any other build. What it still cannot do is *solo*:
    // that needs an in-process sim and a bundled SQLite, so `can_solo()` is false
    // there and on Android. The guard below is therefore about solo specifically
    // rather than about simulating at all — without it, a first-time web visitor
    // inherits the default remembered session (Solo, auto-enter) and is greeted by
    // a failure card about a simulator that was never coming.
    if !(cli.offline || scripted) {
        use systems::session::{SessionKind, SessionRequest};
        let login = login::resolve(cli.login.as_deref());
        // `Ok` = enter this; `Err(true)` = ask; `Err(false)` = stay offline
        // because that is what was chosen last time.
        let request = match cli.connect.clone().or(login.server.clone()) {
            Some(server) => Ok(SessionRequest::Remote {
                server,
                credential: login.credential,
                persist_token: true,
            }),
            // A `--login` with no server names the account solo logs in as.
            None if cli.login.is_some() => Ok(SessionRequest::Solo {
                credential: Some(login.credential),
            }),
            None if !session_prefs.auto_enter => Err(true),
            None => match session_prefs.kind {
                SessionKind::Offline => Err(false),
                // A remembered solo session on a build with no solo: stay offline
                // silently. `Err(false)` and not `Err(true)` — the user did not
                // choose this here, so opening the sign-in screen would be asking
                // them about a decision they made on another machine.
                SessionKind::Solo if !systems::session::can_solo() => Err(false),
                SessionKind::Solo => Ok(SessionRequest::Solo { credential: None }),
                // A remembered server with no address is not a session; ask.
                SessionKind::Remote if session_prefs.server.is_empty() => Err(true),
                SessionKind::Remote => Ok(SessionRequest::Remote {
                    server: session_prefs.server.clone(),
                    // Remembered sessions ride the persisted token. The fallback
                    // credential is this username with no password, which the
                    // server rejects — and a rejection opens the sign-in screen
                    // with the reason on it, which is the honest outcome for a
                    // token that has expired.
                    credential: cartopolis_core::AuthCredential::Password {
                        username: session_prefs.username.clone(),
                        password: String::new(),
                    },
                    persist_token: true,
                }),
            },
        };
        match request {
            Ok(request) => app
                .world_mut()
                .resource_mut::<systems::session::SessionState>()
                .request(request),
            // Offer the choice rather than dropping the user into a world they
            // did not pick.
            Err(ask) => screen.open = ask,
        }
    }
    app.insert_resource(screen);

    // Scripted capture mode (`--shot` / `--shots`): the harness owns the camera
    // and ends the run, and the user store stays out of it (no restore, no
    // autosave). It was already built and validated above.
    if let Some(harness) = harness {
        app.insert_resource(resources::HeadlessSurface {
            size: harness.size,
            scale: 1.0,
        })
        .insert_resource(harness)
        .init_resource::<systems::shot_harness::ShotProgress>()
        .add_systems(
            Startup,
            systems::shot_harness::setup_offscreen_target.after(systems::free_camera::setup_camera),
        )
        .add_systems(
            Update,
            (
                systems::shot_harness::open_scripted_ui,
                systems::shot_harness::place_camera.before(systems::geo_nav::apply_teleport),
                systems::shot_harness::place_space_orbit
                    .after(systems::geo_nav::apply_teleport)
                    .before(systems::free_camera::camera_controls),
                // After drive_screenshots so the `hide_ui` it clears
                // each frame isn't cleared again before PostUpdate's
                // egui suppression reads it.
                systems::shot_harness::drive_capture
                    .after(systems::free_camera::camera_controls)
                    .after(systems::ui::drive_screenshots),
            ),
        );
    }

    // Per-window OS icon via winit (Windows + Linux/X11; a no-op on Wayland and
    // macOS, which use the desktop file / .app bundle respectively). See
    // `systems::window_icon`.
    app.add_systems(Update, systems::window_icon::set_window_icon);

    // Touch → gestures + the emulated mouse (`systems::touch`). MUST run after
    // `InputSystems`, which is what fills `Touches` from this frame's events;
    // unordered it would read last frame's fingers and report every delta one
    // frame late. Everything downstream is an ordinary `Update` reader, so no
    // further ordering is needed.
    app.add_systems(
        PreUpdate,
        systems::touch::recognize_touch_gestures.after(bevy::input::InputSystems),
    );

    app.add_systems(
        Startup,
        (
            systems::free_camera::setup_camera,
            spawn_avatar_entity,
            setup_sky,
            setup_stars,
            setup_moon,
            setup_horizon_haze,
            setup_clouds,
            setup_globe_clouds,
            setup_rain,
        ),
    )
    .add_systems(
        Update,
        (
            network::apply_network_snapshot,
            network::create_egui_texture_handles.after(network::apply_network_snapshot),
            rendering::spawn_regions.after(network::apply_network_snapshot),
            // ADR-029: derive geo-anchored prims' GridCell/Transform from the
            // current OSM anchor (and re-project on re-anchor) before meshing.
            systems::prim_geo::place_geo_prims
                .after(network::apply_network_snapshot)
                .before(rendering::spawn_prims),
            rendering::spawn_prims.after(network::apply_network_snapshot),
            rendering::rebuild_prim_meshes.after(rendering::spawn_prims),
            rendering::refresh_prim_textures
                .after(rendering::spawn_prims)
                .after(network::apply_network_snapshot),
        ),
    )
    .add_systems(
        Update,
        (
            systems::tile_loader::load_region_tiles,
            rendering::update_region_materials,
            systems::osm_buildings::update_buildings.after(rendering::spawn_regions),
            systems::lod22::update_lod22.after(rendering::spawn_regions),
            systems::data_layers::update_data_layers.after(rendering::spawn_regions),
            systems::data_layers::animate_stop_rings
                .after(systems::data_layers::update_data_layers),
            // "Minimal mode" forces the layer + weather toggles off before they're
            // applied (so opening the Route panel strips the map the same frame),
            // then apply_feature_toggles writes visibility. Nested as one element
            // to stay under Bevy's per-tuple system limit.
            (
                systems::data_layers::apply_minimal_mode
                    .before(systems::data_layers::apply_feature_toggles)
                    .before(systems::data_layers::apply_feature_toggles_to_new),
                // The full sweep is the expensive one — four queries over every
                // building, window, 3DBAG mesh, stream tile and region quad — so it
                // runs only when the answer can have changed. Note the gate is a
                // *value* comparison, not `resource_changed::<DataLayers>()`:
                // `update_data_layers` mutably derefs the resource every frame, and
                // Bevy marks a resource changed on `DerefMut` rather than by value,
                // so `resource_changed` here would have been true forever and gated
                // nothing. It also has to cover two inputs that are not toggles at
                // all — the space-tier boolean and the anchor tile — or a descent
                // from orbit leaves the buildings hidden.
                systems::data_layers::apply_feature_toggles
                    .run_if(systems::data_layers::feature_toggles_changed)
                    .after(systems::osm_buildings::update_buildings)
                    .after(systems::map_stream::update_map_stream)
                    .after(rendering::spawn_regions),
                // …and the entities that streamed in *this* frame get the current
                // state applied unconditionally, because the gate above is closed on
                // exactly the frames where nothing about the toggles changed but a
                // new building may still have arrived. A skipped system still
                // satisfies its ordering edges, so the `.after` below holds either
                // way.
                systems::data_layers::apply_feature_toggles_to_new
                    .after(systems::data_layers::apply_feature_toggles)
                    .after(systems::osm_buildings::update_buildings)
                    .after(systems::map_stream::update_map_stream)
                    .after(systems::map_stream::apply_stream_textures)
                    .after(systems::lod22::update_lod22)
                    .after(rendering::spawn_regions),
            ),
            systems::data_layers::handle_poi_click.after(systems::data_layers::update_data_layers),
            // Transit polling, nested to stay under the per-tuple system limit.
            (
                systems::transit_live::update_world_clock,
                systems::transit_live::update_local_timezone
                    .after(systems::transit_live::update_world_clock),
                systems::transit_live::poll_transit_live
                    .after(systems::data_layers::update_data_layers)
                    .after(systems::transit_live::update_world_clock),
                // Live moving vehicles (ADR-026): fold new GPS fixes after the
                // data layer has (re)established its anchor and the clock ticked.
                systems::transit_vehicles::poll_transit_vehicles
                    .after(systems::data_layers::update_data_layers)
                    .after(systems::transit_live::update_world_clock),
                // Live weather for the current anchor (met.no); folds onto the
                // cloud/fog/rain knobs after the anchor is (re)established.
                systems::weather::poll_weather.after(systems::map_stream::apply_reanchor),
                // Live global cloud cover (GFS/TCDC) for the planet layer (ADR-032):
                // one background fetch → the coarse coverage texture the globe
                // cloud shader samples. Independent of the anchor (it's global).
                systems::globe_weather::poll_globe_clouds,
                // The height layer (AHN): the queue that fills its patch cache,
                // and the anchor ground probe that both proves the path works and
                // gives `--dump-state` a measured number to show.
                systems::height::poll_height_layer,
                systems::height::probe_anchor_ground
                    .after(systems::map_stream::apply_reanchor)
                    .before(systems::height::poll_height_layer),
                // Terrain relief: one measured box per anchor, baked into the field
                // every ground-attached system samples. Asks *before* the queue is
                // polled so a fetch it starts is dispatched the same frame.
                systems::terrain::update_terrain_field
                    .after(systems::map_stream::apply_reanchor)
                    .before(systems::height::poll_height_layer),
            ),
            systems::map_stream::init_map_stream.after(rendering::spawn_regions),
            // Re-anchor requests (from teleports / globe descent) drop the old
            // tiles before streaming resumes at the new origin (ADR-025).
            systems::map_stream::apply_reanchor
                .after(systems::geo_nav::apply_teleport)
                .before(systems::map_stream::update_map_stream),
            systems::map_stream::update_map_stream
                .after(systems::map_stream::init_map_stream)
                .after(systems::map_stream::apply_reanchor),
            systems::geo_nav::drive_geocode,
            // Restore feeds pending_teleport, so it must run before the
            // teleport applies; teleports move the camera, so apply before
            // camera_controls reads it this frame.
            // Both skipped under `--shot`: a scripted capture places its own
            // camera and must not overwrite the interactive session's location.
            // Nested to stay under the per-tuple system limit.
            (
                systems::user_store::restore_start_location
                    .after(systems::map_stream::init_map_stream)
                    .run_if(not(systems::shot_harness::harness_active)),
                // Same gate: a scripted capture's layer state comes from its
                // manifest (`--ui`), never from what a session left in user.json.
                systems::user_store::restore_layer_prefs
                    .run_if(not(systems::shot_harness::harness_active)),
                // And the same for the sun/weather switchers: a capture's sky
                // comes from `--time`/`--fog`/`--clouds`/`--rain`, not from what
                // the last interactive session was looking at.
                systems::user_store::restore_sky_prefs
                    .run_if(not(systems::shot_harness::harness_active)),
            ),
            systems::geo_nav::apply_teleport
                .after(systems::geo_nav::drive_geocode)
                .after(systems::user_store::restore_start_location)
                // The avatar has to be *in* the BigSpace before it can be
                // teleported within it: `attach_avatar_to_bigspace` inserts the
                // `GridCell` that `apply_teleport`'s avatar query filters on, and
                // without this edge the startup restore ran first, missed the
                // query and moved only the camera. Bevy inserts the sync point
                // that makes the deferred insert visible because this ordering
                // is explicit.
                .after(attach_avatar_to_bigspace)
                .before(systems::free_camera::camera_controls),
            systems::user_store::autosave_user_store
                .run_if(not(systems::shot_harness::harness_active)),
            // Nested to stay under the per-tuple system limit.
            (
                systems::bookmarks::poll_bookmark_addresses,
                // Not one of the ordered panel systems, and not gated on the
                // panel being open: a photo is captured while the user is in
                // another app entirely, and it has to land whatever the panel is
                // doing when they come back. (Spelled without naming the panel
                // chain's own phase marker — `every_root_ui_panel_is_in_the_chain`
                // finds that block by the first mention of it in this file.)
                systems::bookmarks::poll_place_photos,
            ),
            // Nested to stay under the per-tuple system limit.
            (
                systems::map_stream::apply_stream_textures
                    .after(systems::map_stream::update_map_stream),
                // Stand the basemap on the measured ground. After the spawn pass,
                // so a tile that appeared this frame is lifted in the same frame
                // it is placed rather than showing flat for one.
                systems::map_stream::apply_terrain_to_tiles
                    .after(systems::map_stream::update_map_stream)
                    .after(systems::terrain::update_terrain_field),
                // Vector surfaces as geometry (ADR-035 option B): water and landuse
                // meshes over the textured tiles. No-op on a raster tile source.
                systems::map_geometry::update_map_surfaces
                    .after(systems::map_stream::update_map_stream),
                // Bridge decks stand at a *measured* height, so they are placed a
                // few frames after their cell — whenever the height layer answers.
                systems::map_geometry::resolve_bridge_decks
                    .after(systems::map_geometry::update_map_surfaces)
                    .after(systems::height::poll_height_layer),
                // Sea state from the live wind (cheap: skips unless the wind moved).
                systems::water::update_water_waves,
                // "Road textures" toggle: retexture/flatten the shared road
                // materials (cheap: reads unless a material is on the wrong side).
                systems::map_geometry::apply_road_texture
                    .after(systems::map_geometry::update_map_surfaces),
                // Ramp the scattered trees into the flat fill with altitude. Must
                // follow the surface build, which is what creates the materials it
                // repaints. Skips unless the ramp actually moved.
                systems::vegetation::update_vegetation_lod
                    .after(systems::map_geometry::update_map_surfaces),
                // Same for the street furniture, over its own (much lower) band.
                systems::street_furniture::update_furniture_lod
                    .after(systems::map_geometry::update_map_surfaces),
            ),
            // Globe / space tier (ADR-025): (re)build when the anchor moves, then
            // bake its OSM skin in the background. Nested to stay under the
            // per-tuple system limit.
            (
                systems::globe::setup_globe
                    .after(systems::map_stream::init_map_stream)
                    .after(systems::map_stream::apply_reanchor),
                // Whole-globe quadtree tile streaming (ADR-025).
                systems::globe::update_globe_tiles.after(systems::globe::setup_globe),
                // Globe-descent re-anchor → teleport (must precede the teleport).
                systems::geo_nav::apply_camera_reanchor.before(systems::geo_nav::apply_teleport),
            ),
            attach_avatar_to_bigspace,
        ),
    )
    .add_systems(
        Update,
        (
            systems::free_camera::camera_mode_toggle,
            avatar::handle_avatar_movement.after(network::apply_network_snapshot),
            // After `send_network_intent`, which stamps this frame's
            // `predicted_velocity` that the prediction integrates.
            avatar::smooth_online_avatar_display
                .after(network::apply_network_snapshot)
                .after(avatar::handle_avatar_movement)
                .after(network::send_network_intent),
            avatar::smooth_remote_avatars.after(avatar::smooth_online_avatar_display),
            avatar::tick_remote_avatar_motion_hint.after(avatar::smooth_remote_avatars),
            // Keep the avatar out of buildings: block walking into them, and
            // eject one that lands inside (teleport / a building streaming over
            // it). Runs after movement and before the camera follows the
            // corrected position.
            systems::collision::eject_avatar_from_buildings
                .after(avatar::handle_avatar_movement)
                .before(systems::free_camera::camera_controls),
            network::send_network_intent.after(avatar::handle_avatar_movement),
            // 20 Hz, not per-frame: the window runs `AutoNoVsync`, so unthrottled
            // this pushed several hundred position messages a second at the sim for
            // a value it only uses to pick an area of interest.
            network::send_observer_update
                .after(network::apply_network_snapshot)
                .after(avatar::handle_avatar_movement)
                .run_if(bevy::time::common_conditions::on_timer(
                    Duration::from_millis(50),
                )),
            systems::free_camera::camera_controls.after(avatar::smooth_online_avatar_display),
            systems::globe::update_globe_visibility.after(systems::free_camera::camera_controls),
            systems::globe::update_globe_albedo.after(systems::free_camera::camera_controls),
            // Track the atmosphere's planet centre against the floating-origin
            // camera cell (correct sky altitude in every tier — see its doc).
            systems::globe::update_atmosphere_origin.after(systems::free_camera::camera_controls),
            avatar::respawn_avatar_on_appearance_change.before(avatar::spawn_avatar),
            avatar::spawn_avatar,
            // After `send_network_intent`: the pending emote is a one-shot that
            // this system *takes*, and the wire needs it first or a wave would be
            // seen by everyone except the people watching it happen.
            avatar::update_avatar_animation
                .after(avatar::handle_avatar_movement)
                .after(network::send_network_intent),
            // The airborne pose the clip set doesn't have: lean the model into
            // flight. After `spawn_avatar` so a holder that appeared this frame
            // (first spawn, appearance change) is leant on the frame it exists.
            avatar::update_avatar_flight_pose
                .after(avatar::handle_avatar_movement)
                .after(avatar::spawn_avatar),
            avatar::update_remote_avatar_animation.after(avatar::tick_remote_avatar_motion_hint),
            avatar::clear_remote_emote_edges.after(avatar::update_remote_avatar_animation),
            avatar::sync_avatar_animations.after(avatar::update_avatar_animation),
            systems::debug::debug_region_entities.after(rendering::spawn_regions),
        ),
    )
    // Phase 3b: the world the avatar can touch. Its own `add_systems` because the
    // tuple above is at Bevy's 20-element ceiling, not because these belong apart.
    .add_systems(
        Update,
        (
            // The props the city draws are the props the avatar cannot walk
            // through. Before movement, so a bench that streamed in this frame is
            // solid on the frame it appears.
            systems::collision::sync_prop_colliders.before(avatar::handle_avatar_movement),
            // What the avatar is standing next to, and the nearest house number.
            systems::interact::track_interaction_target.after(avatar::handle_avatar_movement),
            // Before `send_network_intent`, which is what puts a triggered emote on
            // the wire this frame rather than the next.
            systems::interact::handle_interaction_input.before(network::send_network_intent),
        ),
    );
    // Phase 3c: the soundscape. Its own registration because the whole module is
    // behind the `audio` feature (ALSA headers on Linux — see Cargo.toml), and a
    // `#[cfg]` inside a system tuple would have to be repeated per entry.
    #[cfg(feature = "audio")]
    let app = app
        .add_systems(Startup, systems::soundscape::setup_soundscape)
        .add_systems(
            Update,
            (
                // Footsteps, landings and wind: each is a function of what
                // movement just did, so all three run after it.
                systems::soundscape::play_avatar_sounds.after(avatar::handle_avatar_movement),
                systems::soundscape::update_wind_volume.after(avatar::handle_avatar_movement),
                // Turn cues. Here rather than with the rest of driver mode because
                // the whole soundscape is behind this feature, and because a cue is
                // a function of the distance the driver tick just computed — it has
                // to see this frame's band, not last frame's.
                //
                // This is the one part of navigation you can use without looking at
                // the screen, which on a phone in a car is the only way it gets
                // used at all.
                systems::driver::play_driver_cues.after(systems::driver::drive_driver_mode),
            ),
        );
    app
        // Phase 4: Prim selection, raycasting, marquee, keyboard shortcuts
        .add_systems(
            Update,
            (
                // After the gizmo so a handle-drag (sets active_axis) is seen this frame and
                // the click isn't treated as a world selection.
                systems::picking::prim_picking.after(systems::gizmo::handle_gizmo_interaction),
                systems::picking::update_marquee.after(systems::picking::prim_picking),
                systems::picking::render_marquee.after(systems::picking::update_marquee),
                systems::picking::handle_delete_key,
                systems::picking::highlight_selected_prim,
                systems::picking::unhighlight_deselected_prim,
            ),
        )
        // Phase 5: Context menu and edit dialog
        .add_systems(
            Update,
            (
                systems::ui::render_context_menu,
                // Follow selection changes from picking before the dialog draws / live edits apply.
                systems::ui::sync_dialog_to_selection
                    .after(systems::picking::prim_picking)
                    .before(systems::ui::render_edit_dialog),
                systems::ui::render_edit_dialog,
                systems::ui::apply_live_prim_edits.after(systems::ui::render_edit_dialog),
                systems::ui::send_prim_mutations.after(systems::ui::render_edit_dialog),
            ),
        )
        // Phase 7: AI assistant panel + top menu bar
        .add_systems(
            Update,
            (
                // The bottom dock and the top-left layers disc — floating `Area`s of
                // round panel toggles, so they reserve no root-`Ui` space and stay
                // out of the panel chain (Phase 7b). **Before the main menu**, which
                // is not a preference: the dock owns the mark disc that toggles the
                // menu, and a menu drawn before its own switch would open a frame
                // late and dismiss itself on the click that opened it.
                systems::dock::render_dock.before(systems::ui::render_main_menu),
                systems::ui::toggle_ai_panel_shortcut,
                systems::ui::toggle_dev_panel_shortcut,
                // Nested to stay under the per-tuple system limit: the driver runs after
                // the panel so a button click is seen the same frame it's captured.
                (
                    systems::ui::render_dev_panel,
                    systems::ui::drive_screenshots.after(systems::ui::render_dev_panel),
                ),
                systems::ui::render_stats_overlay.after(systems::ui::render_main_menu),
                // The right-hand panel chain lives in Phase 7b, declared once with
                // `.chain()`. A panel added here rather than there is NOT in it.
                (
                    systems::hud::render_hud.after(systems::ui::render_main_menu),
                    // Draws a root-`Ui` panel as well as the bottom bar's `Area`, so it
                    // belongs in the chain below — last, after the route panel.
                ),
                systems::data_layers::render_poi_popup
                    .after(systems::ui::render_main_menu)
                    .after(systems::free_camera::camera_controls),
                // Transit in-world rendering, nested to stay under the per-tuple
                // system limit. Both run after the camera has moved this frame:
                // boards billboard toward it, vehicles interpolate relative to it.
                (
                    systems::transit_live::update_transit_boards
                        .after(systems::transit_live::poll_transit_live)
                        .after(systems::free_camera::camera_controls),
                    systems::transit_vehicles::update_transit_vehicles
                        .after(systems::transit_vehicles::poll_transit_vehicles)
                        .after(systems::free_camera::camera_controls),
                ),
                (
                    apply_sun_mode
                        .before(advance_day_cycle)
                        .after(systems::transit_live::update_world_clock),
                    advance_day_cycle.before(apply_day_night_cycle),
                ),
                apply_day_night_cycle.after(systems::ui::render_dev_panel),
                manage_lights.after(systems::ui::render_dev_panel),
                apply_distance_fog.after(systems::ui::render_dev_panel),
                update_window_glow,
                // Track the camera after it has been positioned this frame.
                update_star_sky.after(systems::free_camera::camera_controls),
                update_moon.after(systems::free_camera::camera_controls),
                (
                    update_horizon_haze.after(systems::free_camera::camera_controls),
                    update_clouds.after(systems::free_camera::camera_controls),
                    update_globe_clouds.after(systems::free_camera::camera_controls),
                    update_rain.after(systems::free_camera::camera_controls),
                ),
                systems::ai_assistant::poll_ai_response
                    .after(systems::ai_assistant::render_ai_panel),
            ),
        )
        // Phase 7b: the right-hand panel chain — ONE ordered declaration.
        //
        // Every system here draws into the *same* shared root `Ui`, so their call
        // order fixes their right-to-left placement, and every one of them takes
        // `ResMut<EguiManager>`, so Bevy already runs them one at a time regardless.
        // The only thing ordering decides is *which* order — and where two are
        // unconstrained, Bevy is free to pick differently from frame to frame.
        //
        // That is not a visual swap. The panels share the single `Background` layer,
        // so a panel that lands over another on some frames wins egui's hit test on
        // those frames, while `layer_id_at` still answers `Background` because the
        // contest is *within* a layer. The symptom is a checkbox that ignores about
        // half the clicks aimed at it, invisible to every layer-level diagnostic and
        // impossible to reproduce in a single-panel test. `chat` and `appearance`
        // were the two left unlinked once (fixed 2026-07-31).
        //
        // This used to be ~12 `.after()` links scattered over seven places in two
        // `add_systems` calls, with the intended chain recorded only in a comment and
        // the note "adding a panel here means adding a link; there is no run-time
        // guard". `.chain()` makes the order structural: a panel added to this tuple
        // is ordered by construction, and one added anywhere else is visibly not in
        // the chain. Since these systems cannot run concurrently anyway, the total
        // order costs no parallelism.
        //
        // `render_place_pins` must stay last: it culls against the root `Ui`'s
        // leftover space, which is only the map area once every side panel has taken
        // its width.
        .add_systems(
            Update,
            (
                systems::ui::render_main_menu,
                systems::geo_nav::render_search_panel,
                systems::ai_assistant::render_ai_panel,
                systems::data_layers::render_data_layers_panel,
                systems::bookmarks::render_bookmarks_panel,
                systems::navigation::render_route_panel,
                systems::chat::render_chat_panel,
                systems::script_editor::render_script_panel,
                systems::ui::render_avatar_chooser,
                systems::settings::render_settings_panel,
                systems::settings::render_about_panel,
                // Before the pins and after every panel, for the same reason both of
                // those hold: it culls against the root `Ui`'s leftover space, which is
                // only the map once the side panels have taken their width.
                systems::interact::render_interaction_prompt,
                // Also after the camera has moved, so pins project onto this frame's view.
                systems::place_pins::render_place_pins.after(systems::free_camera::camera_controls),
            )
                .chain(),
        )
        // Phase 7c: the session — which world this client is in, and the screen that
        // changes it (`systems::session`, `systems::login_screen`).
        //
        // The screen draws into its own floating `Area` (a modal must sit above the
        // main menu, which is itself a `Foreground` area), so it is deliberately
        // *not* in the panel chain above — it never touches the shared root `Ui`. It
        // still runs after the main menu, which is where it is opened from.
        //
        // The driver runs last of the two so a mode picked this frame is applied this
        // frame: a sign-in that visibly waits a frame for its own spinner reads as a
        // stutter on the one screen whose whole job is to say what is happening.
        .add_systems(
            Update,
            (
                systems::login_screen::render_login_screen.after(systems::ui::render_main_menu),
                systems::session::drive_session.after(systems::login_screen::render_login_screen),
            ),
        )
        // The two systems that make the app a well-behaved Android citizen: stop
        // working while backgrounded, and answer the back gesture. Their own
        // `add_systems` call because the Update tuple above is at Bevy's
        // 20-system-per-tuple limit.
        .add_systems(
            Update,
            (
                quiesce_while_backgrounded,
                handle_back_gesture,
                apply_screen_wakelock,
                sync_guidance_pref,
                systems::render_scale::apply_preferred_render_scale,
            ),
        )
        // Phase 7d: the on-screen keyboard — the only keyboard Android and a phone
        // browser have (`systems::onscreen_keyboard`). NativeActivity brings up no
        // IME at all, so every text field in the client can be focused and cannot
        // be typed into, which on a routing client means the address bar and the
        // geo search: the front door.
        //
        // It draws into a floating `Area`, never the shared root `Ui`, so it is
        // deliberately *not* in the Phase 7b chain — same as `dock` and the HUD.
        //
        // But it **must run after every system that can draw a text field**, and
        // that is not a preference. egui surrenders a focused widget's focus on a
        // click landing anywhere else, so tapping a key unfocuses the field, and
        // putting that focus back in the same frame is most of what this system
        // does. Registered earlier, the surrender happens *after* the restore and
        // undoes it: the keyboard types one character into nothing and hides
        // itself. Reading the focused field's rect — to decide whether to dock to
        // the bottom or flip to the top — needs the same ordering, because
        // `read_response` only sees widgets already drawn this pass.
        //
        // Three anchors, because the fields are spread over three phases: the
        // panel chain (geo search, chat, assistant, bookmark rename, route), the
        // sign-in card, and the prim edit dialog.
        .add_systems(
            Update,
            systems::onscreen_keyboard::render_onscreen_keyboard
                .after(systems::place_pins::render_place_pins)
                .after(systems::login_screen::render_login_screen)
                .after(systems::ui::render_edit_dialog),
        )
        // Phase 7e: the on-screen navigation controls (`systems::map_osd`) — the
        // verbs a finger has no key for. Floating `Area`s, so like the dock, the
        // HUD and the keyboard it is deliberately *not* in the Phase 7b chain.
        //
        // Both links are load-bearing. It runs **after the drive tick**, which is
        // what decides whether driver mode owns the screen this frame, and
        // **before the camera**, so a held control is acted on in the frame it
        // was pushed — a frame of lag on a *held* control reads as the camera
        // lagging the thumb, and at 20 fps on a phone that is visible.
        .add_systems(
            Update,
            systems::map_osd::render_map_osd
                .after(systems::driver::drive_driver_mode)
                .before(systems::free_camera::camera_controls),
        )
        // Phase 8: Route planner (ADR-037). Its own `add_systems` call because the
        // Phase 7 tuple is already at Bevy's 20-system-per-tuple limit.
        .add_systems(
            Update,
            (
                // Draws into the same root `Ui` as the other right-hand panels, so its
                // call order fixes its placement — keep it last in that chain.
                systems::navigation::drive_route_geocoding,
                systems::navigation::drive_route_solver
                    .after(systems::navigation::render_route_panel),
                // After the camera has moved, so the cursor ray matches the frame the
                // user actually clicked on.
                systems::navigation::handle_route_map_pick
                    .after(systems::free_camera::camera_controls),
                systems::navigation::update_route_visuals
                    .after(systems::navigation::drive_route_solver),
                // Saved-place pins over the map. Last of the UI systems on purpose:
                // it culls against the root `Ui`'s leftover space, which is only the
                // map area once every side panel has taken its width — otherwise a
                // pin would paint on top of the panel covering it. And after the
                // camera has moved, so pins project onto this frame's view.
                // Street names, laid on the streets themselves. World geometry, not
                // an overlay — after the camera so a turn that flips the letters is
                // acted on in the frame it happened.
                systems::street_labels::update_street_labels
                    .after(systems::free_camera::camera_controls),
            ),
        )
        // Phase 8b: Driver mode — following a solved route (`systems::driver`).
        .add_systems(
            Update,
            (
                // Measures the drive (position, speed, course, progress) before anything
                // reads it. After the panel, so the button press that starts driving is
                // acted on in the same frame it was captured.
                // …and before the solver, so a reroute it asks for on leaving the
                // route is picked up in the same frame rather than a frame late.
                systems::driver::drive_driver_mode
                    .after(systems::navigation::render_route_panel)
                    .after(avatar::handle_avatar_movement)
                    .before(systems::navigation::drive_route_solver),
                // Writes the chase rig into `CameraState`, so it must land before the
                // camera consumes it — this is the whole mechanism by which driver mode
                // holds the view still.
                systems::driver::steady_driver_camera
                    .after(systems::driver::drive_driver_mode)
                    .before(systems::free_camera::camera_controls),
                // After the avatar has been moved (and after the camera, so the arrow
                // is placed against the same frame the view was built from).
                systems::driver::update_driver_arrow
                    .after(systems::driver::drive_driver_mode)
                    .after(systems::free_camera::camera_controls),
                // What lies ahead on the route (the rail's markers, the speed limit).
                // After the drive tick, which is what turns driver mode on, and before
                // the chrome that reads it.
                systems::route_ahead::drive_route_ahead.after(systems::driver::drive_driver_mode),
                systems::driver::render_driver_hud
                    .after(systems::ui::render_main_menu)
                    .after(systems::route_ahead::drive_route_ahead),
                // The compass/locate column, which driver mode repurposes.
                systems::hud::render_map_controls
                    .after(systems::ui::render_main_menu)
                    .after(systems::driver::drive_driver_mode),
            ),
        )
        // Phase 6: Transform gizmos
        .add_systems(
            Update,
            (
                systems::gizmo::handle_gizmo_mode_input,
                systems::gizmo::render_gizmo_visuals,
                systems::gizmo::handle_gizmo_interaction,
            ),
        )
        .add_systems(
            Update,
            (
                frame_time_watchdog,
                // Gated at registration, not inside the body. The body's own
                // `is_phone()` return came *after* the system was in the schedule
                // with a seven-component unfiltered `Query<&Mesh3d, …>`, which Bevy
                // re-matches against every new archetype — and archetypes are
                // created continuously here as tiles, buildings and surfaces
                // stream. A system that runs is a system that pays for its query;
                // the cheapest diagnostic is one the phone never registers.
                log_render_cost.run_if(|| !quality::is_phone()),
                frame_stats,
            ),
        )
        // Drawn over everything until the first tiles are up. Mirrors the
        // splash in index.html, which covers the wasm load this cannot.
        .init_resource::<systems::splash::BootSplash>()
        .add_systems(Update, systems::splash::boot_splash)
        // Drop egui geometry for the screenshot frame, after end_egui_frame has written it.
        .add_systems(
            PostUpdate,
            systems::ui::suppress_egui_for_screenshot.after(systems::egui_manager::end_egui_frame),
        )
        // Three systems, one chain, and each position is load-bearing.
        //
        // `report_frame_to_scheduler` runs **before** the limiter: it tells ADPF how
        // long the frame took, and the limiter *sleeps* to hit a deadline, so
        // measuring after it would report the sleep as work and describe a process
        // that is always exactly on time however slow it really is.
        //
        // `watchdog_beat` runs **after** it, for the mirror-image reason: the
        // heartbeat has to mean "a whole frame completed", so anything that can
        // wedge the schedule — a system, a render extract, the limiter's own sleep
        // — is inside the interval the watchdog measures. Beating in `First` would
        // say the loop is alive at the moment it enters the frame it is about to
        // hang in, which is the one frame that matters.
        .add_systems(
            Last,
            (report_frame_to_scheduler, frame_limiter, watchdog_beat).chain(),
        );

    // The run's verdict has to become the process's. `app.run()` returns an
    // `AppExit` and dropping it made every failure look like a success: a `--shot`
    // that never completed its readback logs `capture never completed`, writes
    // `AppExit::error()`, produces no PNG — and still exited **0**. Anything
    // scripted on top of the harness (a CI job, an agent checking its own work)
    // therefore could not tell a finished capture from a dead one, which is the
    // one distinction such a caller exists to make.
    if let AppExit::Error(code) = app.run() {
        // No process to exit on the web, and the browser build never shoots.
        #[cfg(not(target_arch = "wasm32"))]
        std::process::exit(code.get() as i32);
        #[cfg(target_arch = "wasm32")]
        let _ = code;
    }
}

fn spawn_avatar_entity(mut commands: Commands) {
    // Scale is applied by `avatar::spawn_avatar` from the resolved model, before
    // the mesh instantiates — so spawn at the identity scale here.
    commands.spawn((
        Avatar,
        Transform::from_translation(Vec3::new(0.0, 0.0, 0.0)),
    ));
}

/// Attach the avatar to the world `BigSpace` once it exists, so it rebases with
/// the camera instead of glitching past the first cell (ADR-019).
fn attach_avatar_to_bigspace(
    mut commands: Commands,
    avatars: Query<Entity, (With<Avatar>, Without<GridCell>)>,
    bigspace: Query<Entity, With<BigSpace>>,
) {
    let Ok(root) = bigspace.single() else {
        return;
    };
    for entity in avatars.iter() {
        commands
            .entity(entity)
            .insert((GridCell::default(), ChildOf(root)));
    }
}

/// Startup-time feature switch read from the environment. Used for the render
/// subsystems that can't be safely toggled at runtime (atmosphere, env-map IBL):
/// removing those components mid-run tears down in-flight GPU resources and hangs
/// the device, so they're chosen once at launch. `CARTO_NO_ATMOSPHERE=1`,
/// `CARTO_NO_ENVMAP=1`, `CARTO_NO_SHADOWS=1`.
///
/// Shadows are also a live dev-panel toggle, so the flag looks redundant — it is
/// not. The dev panel is unreachable in a scripted run, and shadows turned out to
/// be the single largest slice of submitted geometry (~79% of triangles), which
/// makes "measure it without them" the first question anyone profiling the web
/// build wants to ask. The flag only forces them *off*; the panel still governs
/// the rest.
///
/// A browser has no environment, so on wasm these are read from the page's query
/// string instead (ADR-033 already puts web configuration there). Without it every
/// one of these switches is silently dead on the web — `std::env::var_os` simply
/// always returns `None` — and they are switches whose whole purpose is to bisect a
/// slow or broken renderer, which is exactly the situation the web build is in.
/// `?CARTO_NO_ATMOSPHERE=1`, or the tidier `?no_atmosphere` (the `CARTO_`
/// prefix and the case are both optional in a URL, where there is no environment
/// to collide with).
///
/// **Android has no environment either**, and its version of the problem is worse
/// than the browser's: a NativeActivity is started by the zygote and inherits
/// `app_process`'s environment, and there is no shell on the phone to add to it.
/// So every switch here was silently dead on the one platform whose renderer is
/// hardest to bisect — the platform where you cannot attach a debugger, cannot
/// read stderr and get one build per CI run. There the source is a **system
/// property**: `CARTO_NO_ATMOSPHERE` → `debug.carto.no_atmosphere`, set with
/// `adb shell setprop` and read at startup. See
/// [`platform::android::system_property`] for why `debug.` specifically, and for
/// the rest of the recipe.
pub fn env_flag(name: &str) -> bool {
    #[cfg(target_os = "android")]
    {
        // Android has no environment either, and for a harder reason than the
        // browser's: the zygote hands a NativeActivity `app_process`'s own
        // environment and there is no shell on the device to add to it. So the
        // same switches come from system properties instead — see
        // `platform::android::system_property`, which is also where the
        // `adb shell setprop` recipe lives.
        return platform::android::system_property(name).is_some_and(|v| v != "0" && !v.is_empty());
    }
    #[cfg(all(not(target_arch = "wasm32"), not(target_os = "android")))]
    {
        std::env::var_os(name).is_some_and(|v| v != "0" && !v.is_empty())
    }
    #[cfg(target_arch = "wasm32")]
    {
        query_flag(name)
    }
}

/// The wasm half of [`env_flag`]: look `name` up in `window.location.search`.
///
/// Deliberately *not* identical to the environment's rule about empty values. An
/// empty environment variable is idiomatically unset, so `env_flag` treats it as
/// false; a bare `?no_atmosphere` in a URL just as plainly means "on", and typing
/// `=1` to satisfy a convention borrowed from a different platform would be a
/// papercut on every debugging URL. Presence is truth here; `=0` and `=false` are
/// the ways to say otherwise.
#[cfg(target_arch = "wasm32")]
fn query_flag(name: &str) -> bool {
    query_value(name)
        .is_some_and(|v| v.is_empty() || (v != "0" && !v.eq_ignore_ascii_case("false")))
}

/// A settings *value* (not a switch), read from the environment on desktop and
/// from the page's query string on the web — the same two sources, and the same
/// naming rules, as [`env_flag`]. `CARTO_PLACE="53.2, 6.5, 120m"` on desktop,
/// `?place=53.2,6.5,120m` in the browser.
pub fn env_value(name: &str) -> Option<String> {
    #[cfg(target_os = "android")]
    {
        // See `env_flag`: system properties stand in for the environment.
        return platform::android::system_property(name).filter(|v| !v.is_empty());
    }
    #[cfg(all(not(target_arch = "wasm32"), not(target_os = "android")))]
    {
        std::env::var(name).ok().filter(|v| !v.is_empty())
    }
    #[cfg(target_arch = "wasm32")]
    {
        query_value(name).filter(|v| !v.is_empty())
    }
}

/// `CARTO_NO_ATMOSPHERE` → `debug.carto.no_atmosphere`: the Android half of
/// [`env_value`]'s name mapping.
///
/// **Deliberately not inside `platform::android`**, and not `cfg`-gated, although
/// it is only ever *called* from there. That module is
/// `cfg(target_os = "android")`; no CI lane builds Android tests, and `cargo
/// check` — which is all the Android lane runs — does not compile `cfg(test)`
/// code at all. A test next to the caller would therefore never execute on any
/// machine, and this is precisely the function whose failure is silent: a name
/// that maps to a property nobody sets reads as "unset" forever, so a typo here
/// looks exactly like a switch that does not work. Here it is covered by the
/// ordinary `cargo test -p cartopolis` run.
///
/// The `debug.` prefix is load-bearing rather than decorative — it is the one
/// property namespace `adb shell setprop` may write on a **production** build
/// without root, which is what makes this reachable on a user's own phone.
// `allow(dead_code)` off Android, against the workspace's `unused = "deny"`: the
// sole non-test caller is `platform::android`, which does not exist on this
// target, so here the tests below are the only users — which is exactly the
// arrangement this function was moved out of that module to get.
#[cfg_attr(not(target_os = "android"), allow(dead_code))]
fn android_property_name(name: &str) -> String {
    let short = name.strip_prefix("CARTO_").unwrap_or(name);
    format!("debug.carto.{}", short.to_ascii_lowercase())
}

#[cfg(test)]
mod android_property_tests {
    use super::android_property_name;

    #[test]
    fn a_carto_variable_maps_to_a_debug_property() {
        assert_eq!(
            android_property_name("CARTO_NO_ATMOSPHERE"),
            "debug.carto.no_atmosphere"
        );
        assert_eq!(
            android_property_name("CARTO_FPS_LOG"),
            "debug.carto.fps_log"
        );
        assert_eq!(
            android_property_name("CARTO_QUALITY"),
            "debug.carto.quality"
        );
    }

    /// The prefix is optional, so a name that never carried it still lands in our
    /// own namespace rather than loose in the platform's.
    #[test]
    fn an_unprefixed_name_is_still_namespaced() {
        assert_eq!(android_property_name("QUALITY"), "debug.carto.quality");
    }

    /// A property *name* was capped at 31 bytes before Android 8. We target 28+,
    /// where it is not — but the cap is the kind of thing that would truncate
    /// silently on an older handset, and every switch this is called with fits it
    /// anyway, so the cheap thing is to keep it that way.
    #[test]
    fn every_switch_fits_the_legacy_name_limit() {
        for name in [
            "CARTO_NO_ATMOSPHERE",
            "CARTO_NO_HAZE",
            "CARTO_NO_ENVMAP",
            "CARTO_NO_OCEAN",
            "CARTO_NO_SHADOWS",
            "CARTO_NO_SPACE_RAYMARCH",
            "CARTO_FPS_LOG",
            "CARTO_FPS_CAP",
            "CARTO_QUALITY",
            "CARTO_RENDER_SCALE",
            "CARTO_TILE_URL",
            "CARTO_PLACE",
        ] {
            let mapped = android_property_name(name);
            assert!(mapped.len() <= 31, "{mapped} is {} bytes", mapped.len());
        }
    }
}

/// Look `name` up in `window.location.search`, accepting the `CARTO_`-prefixed
/// spelling and the bare one, in either case.
#[cfg(target_arch = "wasm32")]
fn query_value(name: &str) -> Option<String> {
    let search = web_sys::window().and_then(|w| w.location().search().ok())?;
    let params = web_sys::UrlSearchParams::new_with_str(&search).ok()?;
    let short = name.strip_prefix("CARTO_").unwrap_or(name);
    [name, short]
        .iter()
        .flat_map(|k| [k.to_ascii_uppercase(), k.to_ascii_lowercase()])
        .find_map(|k| params.get(&k))
}

fn setup_sky(mut commands: Commands, dev: Res<DevPanelState>) {
    // The built-in atmosphere derives the sky from the sun's `DirectionalLight`
    // direction and the camera position, so there's no atmosphere model to poke
    // here — we just spawn the sun/moon lights. `AmbientLight` is a per-camera
    // component now (set in setup_camera, driven by apply_day_night_cycle).
    spawn_sun_light(&mut commands, &dev);

    // Moonlight is a second, shadow-casting directional light. It only exists while
    // it actually contributes (moon up at night); `manage_moon_light` spawns and
    // despawns it so its 4 shadow cascades aren't rendered through the whole day.
    if moon_light_wanted(&dev) {
        spawn_moon_light(&mut commands, &dev);
    }
}

/// Spawn the sun's directional light. Shadows follow the dev panel toggle
/// (`dev.shadows_enabled`), applied at spawn — the toggle respawns the light
/// rather than mutating `shadow_maps_enabled`, which panics bevy_light.
/// Shadow cascades for the sun *and* the moon.
///
/// One constant rather than two literals because bevy_light panics outright if the
/// two directional lights disagree (see CLAUDE.md) — a shared value makes that
/// class of mistake unrepresentable rather than merely avoided.
///
/// The web gets 1: WebGPU reports a supported cascade limit of 1 here, and Bevy
/// clamps to it while warning. Asking for 4 anyway does not buy four cascades, it
/// just builds a config the backend then discards — so this asks for what the
/// platform can actually deliver.
const SHADOW_CASCADES: usize = if cfg!(target_arch = "wasm32") { 1 } else { 4 };

fn spawn_sun_light(commands: &mut Commands, dev: &DevPanelState) {
    let sun_dir = sun_direction(dev.time_of_day, dev.sun_declination_rad);
    let (sun_color, illuminance) =
        sun_light(dev.time_of_day, dev.sun_declination_rad, dev.fog_density);
    commands.spawn((
        DirectionalLight {
            color: sun_color,
            illuminance,
            shadow_maps_enabled: dev.shadows_enabled && !env_flag("CARTO_NO_SHADOWS"),
            ..default()
        },
        // Cascades sized to cover the visible city while keeping crisp near shadows.
        CascadeShadowConfigBuilder {
            num_cascades: SHADOW_CASCADES,
            minimum_distance: 2.0,
            maximum_distance: 3000.0,
            first_cascade_far_bound: 60.0,
            overlap_proportion: 0.2,
        }
        .build(),
        Transform::from_translation(Vec3::ZERO).looking_to(-sun_dir, Vec3::Y),
        // Bevy's procedural sun disk (drawn by the atmosphere's sky pass, so it
        // shows both from the ground and — raymarched — from orbit). Stated
        // explicitly rather than left to the `SunDisk::EARTH` default because
        // the size is a deliberate departure from it: at the true 0.53° the disc
        // is ~6 px wide at 1080p, a dot you have to hunt for. Widened to ~1.4°,
        // which is roughly what the eye reports anyway once glare is included,
        // and left at physical intensity so bloom does the rest.
        bevy::light::SunDisk {
            angular_size: SUN_DISK_ANGULAR_SIZE,
            intensity: 1.0,
        },
        SunLight,
    ));
}

/// Apparent diameter of the sun disk, in radians (the true value is 0.0093).
const SUN_DISK_ANGULAR_SIZE: f32 = 0.024;

/// Threshold on `moonlight_factor` below which the moon light is despawned: its
/// illuminance (and shadows) would be imperceptible, so paying for its cascade
/// shadow passes is pure waste.
const MOON_LIGHT_MIN: f32 = 0.002;

/// Whether the moon light should currently exist (moon up, illuminated, at night).
fn moon_light_wanted(dev: &DevPanelState) -> bool {
    moonlight_factor(dev.time_of_day, dev.moon_age_days, dev.sun_declination_rad) > MOON_LIGHT_MIN
}

/// Spawn the moon's directional light with its shadow cascades. Perceptually
/// blue-gray, with a shorter shadow range than the sun — moon shadows only need to
/// read up close.
fn spawn_moon_light(commands: &mut Commands, dev: &DevPanelState) {
    let moon_dir = utils::sun::moon_direction(dev.time_of_day, dev.moon_age_days);
    let illuminance = MOONLIGHT_ILLUMINANCE
        * moonlight_factor(dev.time_of_day, dev.moon_age_days, dev.sun_declination_rad)
        * (1.0 - 0.75 * dev.fog_density);
    commands.spawn((
        DirectionalLight {
            color: Color::linear_rgb(0.55, 0.72, 1.0),
            illuminance,
            shadow_maps_enabled: dev.shadows_enabled && !env_flag("CARTO_NO_SHADOWS"),
            ..default()
        },
        // Match the sun's cascade count (4): Bevy 0.19's bevy_light panics
        // (cascade-visibility index out of bounds) when two directional lights in
        // the scene have *different* numbers of shadow cascades. Spawning/despawning
        // the whole light (rather than toggling `shadow_maps_enabled`, which also
        // panics) keeps the two live lights' cascade counts equal at all times.
        CascadeShadowConfigBuilder {
            num_cascades: SHADOW_CASCADES,
            minimum_distance: 2.0,
            maximum_distance: 1200.0,
            first_cascade_far_bound: 80.0,
            overlap_proportion: 0.2,
        }
        .build(),
        Transform::from_translation(Vec3::ZERO).looking_to(-moon_dir, Vec3::Y),
        MoonLight,
    ));
}

/// Owns the sun/moon light lifecycle whenever the dev panel changes:
///   - moon spawned at dusk / despawned at dawn, so its 4 shadow cascade passes
///     only render while moonlight is actually visible;
///   - both lights respawned when the shadow toggle flips (Bevy 0.19's bevy_light
///     panics if `shadow_maps_enabled` is mutated on a live light, so it
///     despawns/respawns instead — keeping their cascade counts matched, the
///     other panic condition).
/// One system, one command buffer: the despawn-then-spawn on a shadow flip can't
/// race the moon's dusk/dawn spawn into two moons.
fn manage_lights(
    mut commands: Commands,
    dev: Res<DevPanelState>,
    sun: Query<Entity, With<SunLight>>,
    moon: Query<Entity, With<MoonLight>>,
    mut last_shadows: Local<Option<bool>>,
) {
    if !dev.is_changed() {
        return;
    }
    // A shadow-toggle flip (not the first run, where setup_sky already spawned
    // the lights correctly) respawns both lights with the new shadow state.
    let prev = last_shadows.replace(dev.shadows_enabled);
    let shadows_flipped = prev.is_some() && prev != Some(dev.shadows_enabled);

    if shadows_flipped {
        for e in sun.iter() {
            commands.entity(e).despawn();
        }
        spawn_sun_light(&mut commands, &dev);
        for e in moon.iter() {
            commands.entity(e).despawn();
        }
        if moon_light_wanted(&dev) {
            spawn_moon_light(&mut commands, &dev);
        }
    } else {
        match (moon_light_wanted(&dev), moon.single()) {
            (true, Err(_)) => spawn_moon_light(&mut commands, &dev), // wanted but absent
            (false, Ok(entity)) => {
                commands.entity(entity).despawn(); // present but no longer wanted
            }
            _ => {}
        }
    }
}

/// Global trim on the sun's illuminance: the OSM basemap is near-white, so full
/// physical daylight drives most of the frame into the tonemapper's shoulder
/// (Bevy's default TonyMcMapface) and everything reads flat and washed out.
const SUN_SCALE: f32 = 0.75;

/// Camera exposure (EV100) at full daylight. Exposure adapts toward night by this
/// many stops, mimicking eye/camera adaptation so twilight and the lit city aren't
/// crushed by the bright-noon exposure. Lower ev100 = brighter.
///
/// Kept modest (2.0 stops = 4×): the old 3.5-stop lift (≈11×) multiplied the
/// night ambient floor and moon terms so hard that the near-white OSM ground read
/// as bright daylight, not night. Night should be dark — lit by the moon's
/// directional shaping and the glowing windows, not a lifted exposure.
const DAY_EXPOSURE_EV100: f32 = 13.6;
const NIGHT_EXPOSURE_LIFT: f32 = 2.0;
/// Extra stops around sunrise/sunset, on top of the night lift. The direct beam
/// there is ~3% of noon (air mass ~38 strips most of it), so the two lifts the
/// night curve gives are nowhere near enough — dawn rendered as a black city
/// under a dark red band. Real twilight *looks* bright because the eye adapts to
/// it; this is that adaptation. Gated on `horizon_proximity`, so it fades out
/// well before midnight and leaves the night floor untouched.
const TWILIGHT_EXPOSURE_LIFT: f32 = 2.5;
/// Stops of exposure lift at full overcast, so an overcast scene reads dim but
/// legible after the direct sun is pulled down (see the `overcast` term).
const OVERCAST_EXPOSURE_LIFT: f32 = 0.4;

/// How saturated the sun's *light* colour is (1 = full transmittance colour,
/// 0 = white). Kept well below 1 because the built-in atmosphere already reddens
/// the sky at low sun and the near-white basemap takes the cast readily: at 0.7
/// the whole city went maroon at dawn, ambient and all.
const SUN_COLOR_SATURATION: f32 = 0.25;
/// Exponent softening our own extinction of the sun's *magnitude* (1 = the full
/// Kasten–Young air-mass curve, 0 = none).
///
/// Bevy's atmosphere is fed by the sun `DirectionalLight`'s colour and
/// illuminance and runs its own scattering/extinction on top (`bevy_pbr`
/// atmosphere docs). Handing it a beam we had already extincted double-counted
/// the air mass: at sunrise the raw curve leaves 9% of the beam, the atmosphere
/// then took its own cut, and the sky came out near-black maroon instead of a
/// lit dawn. Softening the exponent (0.09 → 0.32 at the horizon) leaves noon
/// essentially untouched — the curve only bends where the air mass is large.
const SUN_EXTINCTION_SOFTNESS: f32 = 0.45;
/// Peak added ambient brightness at noon. Low on purpose: the sky's own IBL
/// (`AtmosphereEnvironmentMapLight`) already fills the shadows, so ambient piled
/// on top only flattens midday — the shadows are where the depth comes from, and
/// at 1800 the near-white basemap and the near-white buildings washed into one
/// pale sheet.
const AMBIENT_DAY_BRIGHTNESS: f32 = 900.0;
/// Ambient floor at night — a faint blue city skyglow so the world doesn't vanish
/// into pure black between the lit windows. Deliberately low: this is a *floor*,
/// not the main night light (that's the moon + window glow). At the old value of
/// 80 the near-white OSM ground lit up almost like day once the exposure lift
/// (see `NIGHT_EXPOSURE_LIFT`) amplified it.
const AMBIENT_NIGHT_BRIGHTNESS: f32 = 15.0;
/// Peak moonlight illuminance (full moon at the zenith), on the same trimmed
/// scale as the sun. Deliberately well above the night ambient floor: for the
/// directional shaping to *read* — lit roofs and moon-facing facades standing
/// out, soft shadows behind buildings — moonlight must dominate the ambient,
/// which is also how a real moonlit night looks (the full moon is ~2500× any
/// other natural night-sky light).
const MOONLIGHT_ILLUMINANCE: f32 = 180.0;
/// Peak added ambient from a full moon overhead — the sky-scattered part. Halved
/// alongside the night-ambient trim so a full moon lifts the scene enough to read
/// by without flattening it back to daylight brightness.
const MOON_AMBIENT_BRIGHTNESS: f32 = 40.0;
/// Ambient in the space tier — Earthshine scale, not sky scale (see
/// `apply_day_night_cycle`). There is no atmosphere overhead to scatter light
/// into the night side from orbit, so this is only a floor that keeps the dark
/// half legible against the starfield.
const SPACE_AMBIENT_BRIGHTNESS: f32 = 120.0;
/// Stops the camera stops down by in deep space (positive = darker), faded in
/// from the globe threshold on `space_factor`. Held near 0: the near-white map
/// imagery sits on the tonemapper's shoulder, where stopping the camera down
/// barely darkens the surface but does dim the limb and stars. The surface
/// darkening is a tint on the imagery instead (`globe::GLOBE_DEEP_ALBEDO`), which
/// bites where exposure can't; this is left as a small trim knob.
const SPACE_EXPOSURE_STOPS: f32 = 0.0;

/// Marker for the sun's directional light.
#[derive(Component)]
struct SunLight;

/// Marker for the moon's directional light.
#[derive(Component)]
struct MoonLight;

/// Moonlight strength 0–1: moon elevation × illuminated fraction, faded out by
/// day (moonlight is negligible next to any amount of daylight).
fn moonlight_factor(hours: f32, moon_age_days: f32, decl_rad: f32) -> f32 {
    let up = utils::sun::moon_direction(hours, moon_age_days).y.max(0.0);
    up * utils::sun::moon_illuminated_fraction(moon_age_days)
        * (1.0 - utils::sun::daylight(hours, decl_rad))
}

/// Sun light color and illuminance for a time of day. The hue and magnitude both
/// come from the atmosphere's transmittance toward the sun (see `utils::sun`), so
/// the light warms and dims exactly as the sky it hangs in: warm white at noon,
/// gold in the morning/evening, deep red and faint as the disc grazes the horizon.
fn sun_light(hours: f32, decl_rad: f32, fog_density: f32) -> (Color, f32) {
    let t = utils::sun::sun_transmittance(hours, decl_rad);
    let peak = t.max_element().max(1e-4);
    // Hue in the color (peak-normalized), magnitude in the illuminance. Desaturated
    // toward white so the sun light doesn't compound the atmosphere's own sunset red.
    let hue = Vec3::ONE.lerp(
        Vec3::new(t.x / peak, t.y / peak, t.z / peak),
        SUN_COLOR_SATURATION,
    );
    let color = Color::linear_rgb(hue.x, hue.y, hue.z);
    // Fog diffuses sunlight: dim the direct sun (softening shadows) as density rises.
    let sun_through_fog = 1.0 - 0.75 * fog_density;
    // Physical sunlight (RAW_SUNLIGHT ≈ 130k lux, unfiltered) so the built-in
    // atmosphere is lit correctly — its sky brightness/colour scale with the sun's
    // illuminance. The camera's Exposure (see setup_camera) brings this back to a
    // sensible scene brightness; tune `ATMOSPHERE_EXPOSURE_EV100` to taste.
    let illuminance = RAW_SUNLIGHT
        * SUN_SCALE
        * peak.powf(SUN_EXTINCTION_SOFTNESS)
        * utils::sun::sun_disc(hours, decl_rad)
        * sun_through_fog;
    (color, illuminance)
}

/// Ambient light for a time of day: brightness follows the sky through twilight
/// (the sky lights the city before sunrise and after sunset), color follows the
/// sky's hue — blue night, warm dawn/dusk, cool-white day.
fn ambient_light(hours: f32, moon_age_days: f32, decl_rad: f32) -> AmbientLight {
    let c = utils::sun::ambient_sky_color(hours, decl_rad);
    AmbientLight {
        color: Color::linear_rgb(c.x, c.y, c.z),
        brightness: AMBIENT_NIGHT_BRIGHTNESS
            + AMBIENT_DAY_BRIGHTNESS * utils::sun::daylight(hours, decl_rad)
            + MOON_AMBIENT_BRIGHTNESS * moonlight_factor(hours, moon_age_days, decl_rad),
        affects_lightmapped_meshes: true,
    }
}

/// Marker for the catalog star field (spun around the celestial pole).
#[derive(Component)]
struct StarSky;

/// Global brightness multiplier for the star field.
const STAR_BRIGHTNESS: f32 = 3.5;

/// Star-shell radius: beyond all world geometry (so buildings and the ground
/// occlude stars) but inside the far plane and the atmosphere skybox faces.
const STAR_SHELL_RADIUS: f32 = 150_000.0;

/// Material for the catalog star field (see `shaders/starfield.wgsl`): additive
/// gaussian points, with per-star tint and twinkle seed in the vertex colors.
/// `params.x` = overall brightness (night factor × fog veil), `.y` = twinkle clock.
#[derive(Asset, TypePath, AsBindGroup, Clone)]
struct StarFieldMaterial {
    #[uniform(0)]
    params: Vec4,
}

impl Material for StarFieldMaterial {
    fn fragment_shader() -> ShaderRef {
        "shaders/starfield.wgsl".into()
    }

    fn alpha_mode(&self) -> AlphaMode {
        AlphaMode::Add
    }

    fn specialize(
        _pipeline: &MaterialPipeline,
        descriptor: &mut RenderPipelineDescriptor,
        _layout: &MeshVertexBufferLayoutRef,
        _key: MaterialPipelineKey<Self>,
    ) -> Result<(), SpecializedMeshPipelineError> {
        use bevy::render::render_resource::{
            BlendComponent, BlendFactor, BlendOperation, BlendState,
        };
        // Quad winding depends on each star's tangent basis; draw both faces.
        descriptor.primitive.cull_mode = None;
        // Don't write depth — it must not occlude the atmosphere or anything else.
        if let Some(depth) = descriptor.depth_stencil.as_mut() {
            depth.depth_write_enabled = Some(false);
        }
        // Force pure additive blending (src + dst): black gaps add nothing, stars add light.
        if let Some(fragment) = descriptor.fragment.as_mut() {
            if let Some(Some(target)) = fragment.targets.get_mut(0) {
                target.blend = Some(BlendState {
                    color: BlendComponent {
                        src_factor: BlendFactor::One,
                        dst_factor: BlendFactor::One,
                        operation: BlendOperation::Add,
                    },
                    alpha: BlendComponent {
                        src_factor: BlendFactor::Zero,
                        dst_factor: BlendFactor::One,
                        operation: BlendOperation::Add,
                    },
                });
            }
        }
        Ok(())
    }
}

/// Marker for the moon disc quad.
#[derive(Component)]
struct Moon;

/// Angular radius of the moon *quad* in radians. The disc proper fills the inner
/// ~42% of it (`DISC_R` in moon.wgsl) — ~0.7°, ~2.5× the real moon, which is how
/// cameras exaggerate it too; the rest of the quad carries the radiant glow halo.
const MOON_ANGULAR_RADIUS: f32 = 0.028;
/// Moon shell distance: inside the star shell so the disc occludes stars behind it.
const MOON_DISTANCE: f32 = 140_000.0;

/// Material for the moon disc (see `shaders/moon.wgsl`): a sphere-shaded quad lit
/// by the sun direction expressed in the quad's local frame (xyz), so the lunar
/// phase and the terminator's orientation emerge from the real sun–moon geometry.
/// `sun_local.w` = overall brightness; alpha-blended so the disc covers stars.
#[derive(Asset, TypePath, AsBindGroup, Clone)]
struct MoonMaterial {
    #[uniform(0)]
    sun_local: Vec4,
}

impl Material for MoonMaterial {
    fn fragment_shader() -> ShaderRef {
        "shaders/moon.wgsl".into()
    }

    fn alpha_mode(&self) -> AlphaMode {
        AlphaMode::Blend
    }

    fn specialize(
        _pipeline: &MaterialPipeline,
        descriptor: &mut RenderPipelineDescriptor,
        _layout: &MeshVertexBufferLayoutRef,
        _key: MaterialPipelineKey<Self>,
    ) -> Result<(), SpecializedMeshPipelineError> {
        descriptor.primitive.cull_mode = None;
        // Don't write depth — it must not occlude the atmosphere or the stars' pass.
        if let Some(depth) = descriptor.depth_stencil.as_mut() {
            depth.depth_write_enabled = Some(false);
        }
        Ok(())
    }
}

/// Spawns the moon disc: a single quad in its local XY plane with UVs spanning
/// [-1, 1]²; `update_moon` points it at the camera and shades it via the material.
fn setup_moon(
    mut commands: Commands,
    mut meshes: ResMut<Assets<Mesh>>,
    mut materials: ResMut<Assets<MoonMaterial>>,
) {
    let s = MOON_DISTANCE * MOON_ANGULAR_RADIUS;
    let mut mesh = Mesh::new(
        bevy::render::mesh::PrimitiveTopology::TriangleList,
        bevy::asset::RenderAssetUsages::RENDER_WORLD,
    );
    let corners = [(-1.0f32, -1.0f32), (1.0, -1.0), (1.0, 1.0), (-1.0, 1.0)];
    mesh.insert_attribute(
        Mesh::ATTRIBUTE_POSITION,
        corners
            .iter()
            .map(|&(u, v)| [u * s, v * s, 0.0])
            .collect::<Vec<_>>(),
    );
    mesh.insert_attribute(Mesh::ATTRIBUTE_NORMAL, vec![[0.0, 0.0, 1.0]; 4]);
    mesh.insert_attribute(
        Mesh::ATTRIBUTE_UV_0,
        corners.iter().map(|&(u, v)| [u, v]).collect::<Vec<_>>(),
    );
    mesh.insert_indices(bevy::render::mesh::Indices::U32(vec![0, 1, 2, 0, 2, 3]));

    commands.spawn((
        Mesh3d(meshes.add(mesh)),
        MeshMaterial3d(materials.add(MoonMaterial {
            sun_local: Vec4::ZERO, // set each frame; starts invisible
        })),
        Transform::default(),
        bevy::light::NotShadowCaster,
        bevy::light::NotShadowReceiver,
        Moon,
    ));
}

/// Places the moon disc on the sky shell around the camera and feeds the material
/// the sun direction in the quad's local frame (phase shading) plus the overall
/// brightness: fading in through dusk, veiled by fog, extinguished near the horizon.
#[allow(clippy::type_complexity)]
fn update_moon(
    dev: Res<DevPanelState>,
    globe: Res<systems::globe::GlobeState>,
    camera: Query<&GlobalTransform, With<Camera3d>>,
    free_camera: Query<
        (&GridCell, &Transform),
        (With<crate::components::FreeCamera>, Without<Moon>),
    >,
    mut moon: Query<(&mut Transform, &MeshMaterial3d<MoonMaterial>), With<Moon>>,
    mut materials: ResMut<Assets<MoonMaterial>>,
) {
    let Ok(cam) = camera.single() else {
        return;
    };
    let Ok((mut tf, mat_handle)) = moon.single_mut() else {
        return;
    };
    let moon_dir = utils::sun::moon_direction(dev.time_of_day, dev.moon_age_days);
    let rotation = Quat::from_rotation_arc(Vec3::Z, -moon_dir);
    // Same shell rescale as the stars: at its ground distance the disc is nearer
    // than the planet, so from orbit an opaque moon quad hung in front of Earth.
    let space_shell = free_camera
        .single()
        .ok()
        .and_then(|(cell, cam_tf)| globe.sky_shell_radius(cell, cam_tf));
    let scale = space_shell.map_or(1.0, |r| r / MOON_DISTANCE);
    // The disc follows the camera every frame so it stays fixed on the sky.
    tf.translation = cam.translation() + moon_dir * MOON_DISTANCE * scale;
    tf.rotation = rotation;
    tf.scale = Vec3::splat(scale);

    // The material (phase lighting + brightness) depends only on time of day, so
    // rewrite its uniform only when that changed — not every frame.
    if !dev.is_changed() {
        return;
    }

    // Visible from twilight on (the moon shows before full dark), dimmed by fog
    // and by atmospheric extinction as it nears the horizon.
    let sun_elev = sun_direction(dev.time_of_day, dev.sun_declination_rad).y;
    let dusk = ((0.05 - sun_elev) / 0.15).clamp(0.0, 1.0);
    let fog_veil = (-3.0 * dev.fog_density).exp();
    let extinction = 0.3 + 0.7 * utils::sun::smoothstep(0.0, 0.25, moon_dir.y);
    let brightness = dusk * fog_veil * extinction;

    if let Some(mut mat) = materials.get_mut(&mat_handle.0) {
        // Phase-consistent sunlight (not the raw sun direction — see sun.rs).
        let sun_local = rotation.inverse()
            * utils::sun::moon_sunlight_direction(
                dev.time_of_day,
                dev.moon_age_days,
                dev.sun_declination_rad,
            );
        mat.sun_local = sun_local.extend(brightness);
    }
}

/// Marker for the fog-colored horizon-haze shell (see `shaders/horizonhaze.wgsl`).
#[derive(Component)]
struct HorizonHaze;

/// Alpha-blended band of fog color around the horizon, bridging the fogged ground
/// and the atmosphere sky (whose single-scattering model darkens at grazing angles).
/// Colored from the camera's `DistanceFog` each frame (see `update_horizon_haze`).
#[derive(Asset, TypePath, AsBindGroup, Clone)]
struct HorizonHazeMaterial {
    #[uniform(0)]
    color: LinearRgba,
    /// x: elevation where the haze fades out fully, y: peak alpha,
    /// z: directional-light exponent (w unused).
    #[uniform(1)]
    params: Vec4,
    /// `DistanceFog::directional_light_color` — rgb tint, alpha = strength.
    #[uniform(2)]
    dir_light_color: LinearRgba,
    /// x: view-direction `y` where the shell's own rim sits (negative — the ray
    /// that grazes the shell exactly where the ground does). The haze fades to
    /// nothing there, so the rim itself never draws an edge. yzw unused.
    #[uniform(3)]
    rim: Vec4,
}

impl Material for HorizonHazeMaterial {
    fn fragment_shader() -> ShaderRef {
        "shaders/horizonhaze.wgsl".into()
    }

    fn alpha_mode(&self) -> AlphaMode {
        AlphaMode::Blend
    }

    fn specialize(
        _pipeline: &MaterialPipeline,
        descriptor: &mut RenderPipelineDescriptor,
        _layout: &MeshVertexBufferLayoutRef,
        _key: MaterialPipelineKey<Self>,
    ) -> Result<(), SpecializedMeshPipelineError> {
        // Camera is inside the shell; render its interior, and don't occlude
        // anything behind it (depth test still lets world geometry cover it).
        descriptor.primitive.cull_mode = None;
        if let Some(depth) = descriptor.depth_stencil.as_mut() {
            depth.depth_write_enabled = Some(false);
        }
        Ok(())
    }
}

/// When the dev panel's "Quick cycle" toggle is on, advances the time of day smoothly,
/// looping the full 24 h over `cycle_seconds`.
/// Drive `time_of_day` from the sun-mode switcher (HUD Auto/Day/Night). `Auto`
/// tracks the real local *solar* time at the map anchor (so the sun is where it
/// really is right now); `Day`/`Night` pin noon/midnight; `Manual` hands the
/// clock to the dev panel. Runs before `advance_day_cycle` and
/// `apply_day_night_cycle`, so the whole sun/moon/fog pipeline sees the value.
fn apply_sun_mode(
    clock: Res<systems::transit_live::WorldClock>,
    globe: Res<systems::globe::GlobeState>,
    mut dev: ResMut<DevPanelState>,
) {
    // Solar declination depends only on the date, not the sun mode — so track it
    // from the real clock in *every* mode, including Manual (the dev panel drives
    // the time of day, but the season is still today's). This is what gives the
    // sun its correct seasonal day length; without it the arc set sunset at 18:00
    // year-round (a July evening in NL read as dark hours too early).
    let day_of_year = jiff::Timestamp::from_second(clock.now_unix as i64)
        .map(|ts| ts.to_zoned(jiff::tz::TimeZone::UTC).date().day_of_year() as f32)
        .unwrap_or(80.0); // equinox fallback
    let target_decl = utils::sun::solar_declination_rad(day_of_year);
    if (dev.sun_declination_rad - target_decl).abs() > 1e-5 {
        dev.sun_declination_rad = target_decl;
    }

    // Manual hands the rest of the day/night group — time *and* moon — to the dev
    // panel (slider / presets / Quick-cycle).
    if dev.sun_mode == resources::SunMode::Manual {
        return;
    }
    let target_time = match dev.sun_mode {
        resources::SunMode::Day => 12.0,
        resources::SunMode::Night => 0.0,
        // Quantize to the minute: the sun moves imperceptibly between, and this
        // keeps `apply_day_night_cycle` (gated on `dev.is_changed()`) from
        // re-lighting the whole scene every single frame.
        resources::SunMode::Auto => {
            let solar = utils::sun::solar_time_of_day(clock.now_unix, globe.anchor_lng);
            (solar * 60.0).round() / 60.0
        }
        resources::SunMode::Manual => unreachable!("returned above"),
    };
    // The cycle includes the moon: in every automatic mode its phase and
    // position ride the real date, not a fixed dev-panel value.
    let target_moon = utils::sun::lunar_age_days(clock.now_unix);
    // Only touch the resource when a value actually moves, so change detection
    // stays quiet once Day/Night settle (and between Auto's minute ticks).
    if (dev.time_of_day - target_time).abs() > 1e-4 {
        dev.time_of_day = target_time;
    }
    if (dev.moon_age_days - target_moon).abs() > 1e-4 {
        dev.moon_age_days = target_moon;
    }
}

fn advance_day_cycle(time: Res<Time>, mut dev: ResMut<DevPanelState>) {
    // Quick-cycle animates `time_of_day`/`moon_age_days` directly, so it only
    // runs in Manual — any automatic mode overwrites them from the real clock.
    if !dev.auto_cycle || dev.sun_mode != resources::SunMode::Manual {
        return;
    }
    let speed = 24.0 / dev.cycle_seconds.max(0.1); // hours per real second
    let dh = time.delta_secs() * speed;
    dev.time_of_day = (dev.time_of_day + dh).rem_euclid(24.0);
    // The moon ages with the cycling days, so moonrise drifts ~50 min later per
    // day and the phase creeps through the month, just like the real moon.
    dev.moon_age_days = (dev.moon_age_days + dh / 24.0).rem_euclid(utils::sun::LUNAR_CYCLE_DAYS);
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
fn report_frame_to_scheduler(time: Res<Time<Real>>, mut started: Local<bool>) {
    #[cfg(target_os = "android")]
    {
        if !*started {
            *started = true;
            // The deadline to aim at. The phone tier leaves pacing to FIFO
            // (`fps_cap: None`), so the target is the display's own period rather
            // than a cap of ours — 60 Hz, which is what a Fairphone 4 has.
            let target = quality::budget()
                .fps_cap
                .map_or(1.0e9 / 60.0, |fps| 1.0e9 / fps);
            platform::android::start_perf_hint(target as i64);
        }
        platform::android::report_frame(time.delta().as_nanos() as i64);
    }
}

fn frame_limiter(
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
        let period = *period.get_or_insert_with(|| {
            // The tier's answer, with `CARTO_FPS_CAP` still overriding it — a
            // debugging knob has to win over a default to be useful, and lifting
            // the cap is how headroom gets measured at all.
            //
            // The tier answers `None` on the phone, which is not "uncapped" so much
            // as "already paced": Android runs `PresentMode::AutoVsync`, and a
            // sleep-based limiter layered on FIFO is two pacers timing from
            // different points in the pipeline, which beats against itself. It also
            // made the main thread look idle to the Linux scheduler on a
            // 2-big-6-little SoC whose tile workers look busy — the wrong signal
            // for deciding which of them gets a fast core.
            let fps: f64 = std::env::var("CARTO_FPS_CAP")
                .ok()
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
fn frame_stats(
    time: Res<Time<Real>>,
    // (elapsed_s, frames, worst_ms, sum_ms, sum_ms_sq)
    mut acc: Local<(f32, u32, f32, f32, f32)>,
    mut enabled: Local<Option<bool>>,
) {
    // Through `env_flag`, not `std::env` directly, so `?fps_log` reaches it on the
    // web — where a number for "how slow is slow" is harder to come by than
    // anywhere else, there being no external profiler to point at the tab.
    if !*enabled.get_or_insert_with(|| env_flag("CARTO_FPS_LOG")) {
        return;
    }
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
        tracing::info!(
            fps = (acc.1 as f32 / acc.0) as u32,
            mean_frame_ms = mean as u32,
            worst_frame_ms = acc.2 as u32,
            jitter_ms = var.sqrt() as u32,
            "frame stats"
        );
        *acc = (0.0, 0, 0.0, 0.0, 0.0);
    }
}

fn frame_time_watchdog(
    time: Res<Time<Real>>,
    globe_meshes: Query<(), With<crate::components::GlobeMesh>>,
    meshes: Res<Assets<Mesh>>,
    images: Res<Assets<Image>>,
    materials: Res<Assets<StandardMaterial>>,
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
/// Edge-triggered on the answer, not the inputs: `set_window_flags` crosses a
/// JNI-ish boundary into the activity's looper, and doing that every frame for
/// an unchanged value would be silly.
fn apply_screen_wakelock(
    driver: Res<systems::driver::DriverState>,
    prefs: Res<resources::AppPrefs>,
    mut held: Local<bool>,
) {
    let want = driver.active && prefs.keep_screen_on;
    if want == *held {
        return;
    }
    // Not while a permission dialog is up. `*held` is deliberately left alone, so
    // this is a *deferral* and not a skip: the next frame asks again, and the flag
    // lands as soon as the dialog is gone. The reason is
    // `platform::android::PERMISSION_DIALOG_UP` — driver mode raises the dialog and
    // wants the wakelock on the same frame, and both end up writing the activity's
    // one `Window` from different threads.
    #[cfg(target_os = "android")]
    if platform::android::permission_dialog_up() {
        return;
    }
    *held = want;
    #[cfg(target_os = "android")]
    platform::android::set_keep_screen_on(want);
}

/// Tell the watchdog a frame finished. See `platform::watchdog`.
fn watchdog_beat() {
    platform::watchdog::beat();
}

/// Mirror the user's guidance preference into driver mode.
///
/// `DriverState` owns the flag the cue system reads, because that system runs
/// deep inside the driving tick and taking a second resource there would push it
/// over Bevy's system-parameter limit. So the preference is copied in rather than
/// read through.
fn sync_guidance_pref(
    prefs: Res<resources::AppPrefs>,
    mut driver: ResMut<systems::driver::DriverState>,
) {
    if !prefs.is_changed() {
        return;
    }
    driver.cues = systems::driver::GuidanceSound(prefs.voice_guidance);
}

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
fn quiesce_while_backgrounded(
    mut lifecycle: MessageReader<bevy::window::AppLifecycle>,
    mut exits: MessageReader<AppExit>,
    mut touches: ResMut<bevy::input::touch::Touches>,
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
            }
            bevy::window::AppLifecycle::Idle => {}
        }
    }
}

/// Android's back gesture, which until now did nothing at all.
///
/// Under NativeActivity the key is delivered to the app and consumed there, so a
/// user who swipes back — the first thing every Android user reaches for to
/// dismiss something — got no response, and the only way out of a full-screen
/// panel was to find its close button. Back now unwinds the same stack Escape
/// does on the desktop, innermost first: leave driver mode, then close the open
/// panel, and only quit when there is nothing left to dismiss.
///
/// Quitting last is the part worth stating. Android's convention is that back
/// from the root screen leaves the app, and an app that refuses to is a worse
/// citizen than one that exits when you meant to close a panel — but a map client
/// that quits on the first back press while a panel is open would be unusable.
fn handle_back_gesture(
    keys: Res<ButtonInput<KeyCode>>,
    mut keyboard: ResMut<systems::onscreen_keyboard::OnScreenKeyboard>,
    mut right_panel: ResMut<resources::RightPanel>,
    mut driver: ResMut<systems::driver::DriverState>,
    mut exit: MessageWriter<AppExit>,
) {
    // `BrowserBack` is what winit maps Android's back key to. Desktop keyboards
    // can carry the same key, and doing the same thing there is right anyway.
    if !keys.just_pressed(KeyCode::BrowserBack) {
        return;
    }
    // The keyboard is the innermost rung, because that is where the system IME
    // sits on every other Android app: Back puts the keyboard away first and only
    // then starts closing the thing you were typing into. Getting this order
    // wrong is worse than it sounds — it would have made Back leave driver mode
    // out from under someone who was only trying to dismiss the keys.
    if keyboard.is_open() {
        keyboard.dismiss();
    } else if driver.active {
        driver.exit();
    } else if *right_panel != resources::RightPanel::None {
        *right_panel = resources::RightPanel::None;
    } else {
        exit.write(AppExit::Success);
    }
}

fn log_render_cost(
    time: Res<Time<Real>>,
    mut next: Local<f32>,
    meshes: Res<Assets<Mesh>>,
    q: Query<(
        &Mesh3d,
        &bevy::camera::visibility::ViewVisibility,
        Has<bevy::light::NotShadowCaster>,
        Has<crate::components::OsmBuildingMesh>,
        Has<systems::lod22::Lod22Mesh>,
        Has<systems::map_stream::StreamTile>,
        Has<crate::components::GlobeMesh>,
    )>,
) {
    *next -= time.delta_secs();
    if *next > 0.0 {
        return;
    }
    *next = 5.0;
    // A full walk of every `Mesh3d` entity, resolving each handle into
    // `Assets<Mesh>` to sum its vertices. That is a real periodic hitch, and on the
    // one platform where it is most expensive it is also useless: nobody is reading
    // a phone's log while holding the phone. Desktop keeps it — it is how the tile
    // and building budgets get watched — but the handset does not pay for a
    // diagnostic with no reader.
    //
    // The phone gate is a `run_if` at registration (see `lib.rs`'s `add_systems`)
    // rather than a return here, because an early return does not save the query:
    // Bevy still matches this system's seven-component `Query` against every
    // archetype the streamers create.

    let tris = |h: &Handle<Mesh>| -> usize {
        meshes.get(h).map_or(0, |m| {
            // Use the non-panicking accessors: meshes uploaded RENDER_WORLD-only
            // (tiles, globe, transit) have had their main-world geometry dropped
            // after extraction, so `indices()`/`count_vertices()` would panic
            // (they assert MAIN_WORLD). Count those as 0 rather than crashing.
            match m.try_indices_option() {
                Ok(Some(i)) => i.len() / 3,
                Ok(None) => m.count_vertices() / 3, // non-indexed, still in main world
                Err(_) => 0,                        // extracted to render world only
            }
        })
    };
    let (mut draws, mut total, mut shadow) = (0usize, 0usize, 0usize);
    let (mut bld, mut lod22, mut tiles, mut globe) = (0usize, 0usize, 0usize, 0usize);
    for (mesh, vis, no_shadow, is_bld, is_lod22, is_tile, is_globe) in &q {
        if !vis.get() {
            continue; // not actually rendered this frame
        }
        let t = tris(&mesh.0);
        draws += 1;
        total += t;
        if !no_shadow {
            shadow += t;
        }
        if is_bld {
            bld += t;
        }
        if is_lod22 {
            lod22 += t;
        }
        if is_tile {
            tiles += t;
        }
        if is_globe {
            globe += t;
        }
    }
    tracing::info!(
        draws,
        ktris_total = total / 1000,
        ktris_shadow = shadow / 1000,
        ktris_buildings = bld / 1000,
        ktris_lod22 = lod22 / 1000,
        ktris_tiles = tiles / 1000,
        ktris_globe = globe / 1000,
        "render cost",
    );
}

#[allow(clippy::type_complexity)]
fn apply_day_night_cycle(
    dev: Res<DevPanelState>,
    globe: Res<systems::globe::GlobeState>,
    mut sun: Query<(&mut Transform, &mut DirectionalLight), (With<SunLight>, Without<MoonLight>)>,
    mut moon: Query<(&mut Transform, &mut DirectionalLight), With<MoonLight>>,
    // AmbientLight is a per-camera component in Bevy 0.19 (attached in setup_camera).
    mut ambient: Query<&mut AmbientLight>,
    camera: Query<
        (
            &GridCell,
            &Transform,
            &mut Exposure,
            // `Option`, because a tier that does not want glare omits `Bloom`
            // rather than zeroing it (`free_camera::setup_camera` says why: the
            // component carries `#[require(Hdr)]`, so keeping it costs 8 B/px of
            // main texture even at zero intensity). Naming it bare here would take
            // the camera out of *this whole query* on such a tier — no exposure
            // adaptation, no day/night cycle at all — which is a lighting failure
            // that looks nothing like a missing bloom.
            Option<&mut bevy::post_process::bloom::Bloom>,
        ),
        (
            With<crate::components::FreeCamera>,
            Without<SunLight>,
            Without<MoonLight>,
        ),
    >,
    // The space tier changes the whole lighting model, so the `dev.is_changed()`
    // gate has to let a climb through it too — `space_factor` at the last apply.
    mut was_space: Local<f32>,
) {
    let mut camera = camera;
    // 0 on the ground, 1 in deep space, smooth in between: every lighting term
    // below crossfades on it. As a boolean these all stepped at once, mid-zoom.
    let space = camera
        .single()
        .map(|(cell, tf, _, _)| globe.space_factor(cell, tf))
        .unwrap_or(0.0);
    if !dev.is_changed() && (space - *was_space).abs() < SPACE_RAMP_STEP {
        return;
    }
    *was_space = space;

    // Overcast: thick cloud (and, a little, rain) dims and flattens the ground
    // lighting — the direct sun drops, the sky's fill light rises, colours grey
    // out. Ground tier only (space has its own lighting model, and the planet
    // cloud sphere shades the globe there), so it fades out with `space`.
    let overcast = dev.cloud_cover.max(dev.precipitation * 0.9) * (1.0 - space);

    let sun_dir = sun_direction(dev.time_of_day, dev.sun_declination_rad);

    // Adapt exposure to the light level (eye/camera adaptation): brighten toward
    // twilight and night by up to NIGHT_EXPOSURE_LIFT stops so dusk and the lit city
    // aren't crushed by the bright-noon exposure that suits the physical daytime sun.
    // From space there's nothing to adapt to — the lit half of the planet is in full
    // daylight whatever the local clock says — so the day exposure holds.
    let day = utils::sun::daylight(dev.time_of_day, dev.sun_declination_rad);
    let ground_ev = DAY_EXPOSURE_EV100
        - NIGHT_EXPOSURE_LIFT * (1.0 - day)
        - TWILIGHT_EXPOSURE_LIFT * utils::sun::horizon_proximity(dev.time_of_day, dev.sun_declination_rad)
        // A small lift so an overcast scene reads dim-but-legible rather than
        // being crushed once the direct sun is pulled down.
        - OVERCAST_EXPOSURE_LIFT * overcast;
    if let Ok((_, _, mut exp, bloom)) = camera.single_mut() {
        // The globe's map imagery is bright (near-white paper); from orbit the
        // sunlit hemisphere blows out, so stop the camera down as you climb. This
        // rides `space_factor`, so at the tier boundary it's exactly the ground
        // exposure — the globe there renders as bright as the flat tiles it
        // replaces, no step — and only deep in space does it darken to a
        // planetary look. (The darkening lives here, not baked into the tile
        // material, precisely so it can be zero at the boundary; a fixed material
        // tint can't be, because exposure is global and can't tell the globe
        // above the boundary from the flat tiles just below it.)
        let space_ev = DAY_EXPOSURE_EV100 + SPACE_EXPOSURE_STOPS;
        exp.ev100 = ground_ev.lerp(space_ev, space);
        // More glare the farther out you go (see the constants): from deep space
        // the limb is a few pixels of arc, and glare is most of what makes it
        // read as an atmosphere rather than a drawn outline. Near the ground
        // that much bloom just smears the map.
        if let Some(mut bloom) = bloom {
            bloom.intensity = systems::free_camera::bloom_intensity(space);
        }
    }

    // Above the atmosphere the sun is unfiltered: no air mass to redden or dim
    // it, and no local horizon to set behind. Attenuating it by the *anchor's*
    // ground-level extinction (`sun_light`) is what left the globe a dim red disc
    // at 18:00 and unlit at all at night, when what the view should show is a
    // fully sunlit day side and a dark night side, terminator between. Crossfaded
    // rather than switched, or climbing out at dusk changed the light in a step.
    let (ground_color, ground_lux) =
        sun_light(dev.time_of_day, dev.sun_declination_rad, dev.fog_density);
    let mut sun_color = Color::from(LinearRgba::from_vec4(
        LinearRgba::from(ground_color)
            .to_vec4()
            .lerp(LinearRgba::WHITE.to_vec4(), space),
    ));
    let mut illuminance = ground_lux.lerp(RAW_SUNLIGHT * SUN_SCALE, space);
    if overcast > 0.0 {
        // Cloud thins the direct beam and greys its colour toward the sky.
        illuminance *= 1.0 - 0.72 * overcast;
        let c = LinearRgba::from(sun_color).to_vec4();
        let lum = c.x * 0.2126 + c.y * 0.7152 + c.z * 0.0722;
        sun_color = Color::from(LinearRgba::from_vec4(
            c.lerp(Vec4::new(lum, lum, lum, 1.0), 0.6 * overcast),
        ));
    }
    for (mut tf, mut light) in sun.iter_mut() {
        *tf = Transform::from_translation(Vec3::ZERO).looking_to(-sun_dir, Vec3::Y);
        light.color = sun_color;
        light.illuminance = illuminance;
    }

    // Moonlight tracks the moon disc; fog diffuses it like it does the sun.
    let moon_dir = utils::sun::moon_direction(dev.time_of_day, dev.moon_age_days);
    let moonlight = MOONLIGHT_ILLUMINANCE
        * moonlight_factor(dev.time_of_day, dev.moon_age_days, dev.sun_declination_rad)
        * (1.0 - 0.75 * dev.fog_density);
    for (mut tf, mut light) in moon.iter_mut() {
        *tf = Transform::from_translation(Vec3::ZERO).looking_to(-moon_dir, Vec3::Y);
        light.illuminance = moonlight;
        // NB: don't toggle `shadow_maps_enabled` at runtime — Bevy 0.19's bevy_light
        // panics (cascade-visibility index out of bounds) when a directional light's
        // shadows are switched on/off after spawn. Moon shadows stay enabled; the
        // near-zero daytime illuminance makes them invisible anyway.
    }

    if let Ok(mut a) = ambient.single_mut() {
        // Space has no sky to scatter light back into the shadowed side, so the
        // ambient falls off toward Earthshine scale — just enough that the night
        // side reads as unlit land rather than a hole cut out of the starfield.
        let ground = ambient_light(dev.time_of_day, dev.moon_age_days, dev.sun_declination_rad);
        let mut ground_rgb = LinearRgba::from(ground.color).to_vec4();
        // Overcast fills shadows: the diffuse sky light rises and greys, so an
        // overcast day is flat and cool rather than sunlit-and-shadowed.
        let ground_brightness = ground.brightness * (1.0 + 0.8 * overcast);
        if overcast > 0.0 {
            let lum = ground_rgb.x * 0.2126 + ground_rgb.y * 0.7152 + ground_rgb.z * 0.0722;
            ground_rgb = ground_rgb.lerp(Vec4::new(lum, lum, lum, 1.0), 0.5 * overcast);
        }
        let space_rgb = LinearRgba::rgb(0.55, 0.62, 0.85).to_vec4();
        *a = AmbientLight {
            color: Color::from(LinearRgba::from_vec4(ground_rgb.lerp(space_rgb, space))),
            brightness: ground_brightness.lerp(SPACE_AMBIENT_BRIGHTNESS, space),
            affects_lightmapped_meshes: true,
        };
    }
}

/// Change in `GlobeState::space_factor` worth re-applying the lighting for. The
/// terms it drives are all smooth, so a coarse step is invisible — and it keeps a
/// slow climb from rewriting the sun light (and its shadow cascades) every frame.
const SPACE_RAMP_STEP: f32 = 0.01;

/// Drives the camera's atmospheric [`DistanceFog`] from the dev panel's fog slider,
/// the time of day (fog darkens at night), and the camera altitude — the fog thins
/// as you climb (see `fog_altitude_factor`) so a high-flight or orbit view looks
/// down through clear air rather than a grey wall. Runs each frame because the
/// altitude term is continuous.
fn apply_distance_fog(
    dev: Res<DevPanelState>,
    globe: Res<systems::globe::GlobeState>,
    mut camera: Query<
        (&GridCell, &Transform, &mut DistanceFog),
        With<crate::components::FreeCamera>,
    >,
    // The (density, time, declination) this last wrote. `DistanceFog` is not
    // `PartialEq`, so the inputs are what gets compared — the same shape
    // `water::update_water_waves` uses for the sea state.
    mut applied: Local<Option<(f32, f32, f32)>>,
) {
    let Ok((cell, tf, mut fog)) = camera.single_mut() else {
        return;
    };
    // The atmosphere itself stays a clear sky: boosting Nishita's Mie scattering
    // with density was tried and abandoned — single scattering *darkens* the low
    // sky instead of whitening it. The horizon-haze shell fogs the sky instead
    // (see update_horizon_haze), veiling the whole dome as density rises.
    let altitude = globe.altitude_m(cell, tf) as f32;
    let density = dev.fog_density * systems::free_camera::fog_altitude_factor(altitude);

    // Assigning through the `Mut` marks `DistanceFog` changed, which re-extracts
    // and re-uploads the camera's fog uniform. This ran unconditionally, so that
    // happened on every frame of the run for three inputs that move slowly at
    // most: the dev panel's density, the clock, and the sun's declination. Only
    // `density` moves continuously, and only while the camera is changing
    // altitude — so on a still frame this now writes nothing at all.
    let next = (density, dev.time_of_day, dev.sun_declination_rad);
    // Epsilons rather than exact equality: `density` is a product of a float
    // altitude factor, so it dithers in its last bits while hovering, and an exact
    // test would call that a change every frame and buy nothing.
    const FOG_EPS: f32 = 1e-4;
    if applied.is_some_and(|(d, t, s)| {
        (d - next.0).abs() < FOG_EPS && (t - next.1).abs() < FOG_EPS && (s - next.2).abs() < FOG_EPS
    }) {
        return;
    }
    *applied = Some(next);
    *fog = systems::free_camera::distance_fog(density, dev.time_of_day, dev.sun_declination_rad);
}

/// Spawns the star-map night sky: a box centred on the viewer whose custom material
/// samples the equirectangular star map by view direction (no UV-sphere poles or seams).
/// Fade the emissive building windows in as the sun drops below the horizon, so the
/// city lights up at night (and windows read as dark glass by day).
fn update_window_glow(
    dev: Res<DevPanelState>,
    windows: Query<(
        &systems::osm_buildings::WindowGlow,
        &MeshMaterial3d<StandardMaterial>,
    )>,
    // Windows stream in continuously as you move; newly-added ones need their glow
    // set even on frames where the time of day hasn't changed.
    new_windows: Query<
        (
            &systems::osm_buildings::WindowGlow,
            &MeshMaterial3d<StandardMaterial>,
        ),
        Added<systems::osm_buildings::WindowGlow>,
    >,
    mut materials: ResMut<Assets<StandardMaterial>>,
) {
    let elevation = sun_direction(dev.time_of_day, dev.sun_declination_rad).y;
    // Begin glowing as the sun lowers in the evening (elevation ~0.18), reaching
    // full a little after dusk — a gentle ramp, not an on/off at full dark.
    let glow = ((0.18 - elevation) / 0.33).clamp(0.0, 1.0);
    let mut apply = |bucket: &systems::osm_buildings::WindowGlow,
                     mat: &MeshMaterial3d<StandardMaterial>| {
        if let Some(mut m) = materials.get_mut(&mat.0) {
            let e = 1.2 * glow * bucket.0; // subtle, warm; per-bucket brightness
            m.emissive = LinearRgba::rgb(e, 0.72 * e, 0.34 * e);
        }
    };
    // Only rewrite every window's emissive when the time of day actually changed;
    // otherwise just catch up newly-streamed windows.
    if dev.is_changed() {
        for (bucket, mat) in windows.iter() {
            apply(bucket, mat);
        }
    } else {
        for (bucket, mat) in new_windows.iter() {
            apply(bucket, mat);
        }
    }
}

/// Builds the star field from the embedded bright-star catalog (~9k naked-eye
/// stars, HYG database — see `tools/gen_stars.py`): one tiny sphere-tangent quad
/// per star at its real equatorial position, weighted by magnitude and tinted by
/// B−V color temperature, with a per-star twinkle seed in the vertex alpha. The
/// whole sky is one mesh and one draw call; `update_star_sky` spins it around the
/// celestial pole, fades it with the night, and drives the twinkle clock.
fn setup_stars(
    mut commands: Commands,
    mut meshes: ResMut<Assets<Mesh>>,
    mut materials: ResMut<Assets<StarFieldMaterial>>,
) {
    static CATALOG: &[u8] = include_bytes!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../assets/sky/bright_stars.bin"
    ));
    let n = CATALOG.len() / 16;
    let mut positions: Vec<[f32; 3]> = Vec::with_capacity(n * 4);
    let mut normals: Vec<[f32; 3]> = Vec::with_capacity(n * 4);
    let mut uvs: Vec<[f32; 2]> = Vec::with_capacity(n * 4);
    let mut colors: Vec<[f32; 4]> = Vec::with_capacity(n * 4);
    let mut indices: Vec<u32> = Vec::with_capacity(n * 6);

    for (i, rec) in CATALOG.chunks_exact(16).enumerate() {
        let f = |k: usize| f32::from_le_bytes(rec[4 * k..4 * k + 4].try_into().unwrap());
        let (ra, dec, mag, bv) = (f(0), f(1), f(2), f(3));

        // Equatorial frame with +Y at the north celestial pole. RA winds so that a
        // larger right ascension culminates *later* under the westward diurnal spin,
        // which keeps constellations un-mirrored for a ground observer.
        let dir = Vec3::new(dec.cos() * ra.cos(), dec.sin(), -(dec.cos() * ra.sin()));
        // Tangent basis for the quad (X fallback for stars at the poles).
        let up = if dir.y.abs() > 0.99 { Vec3::X } else { Vec3::Y };
        let t1 = up.cross(dir).normalize();
        let t2 = dir.cross(t1);

        // Compressed magnitude scale: real flux spans ~1500:1 from Sirius to the
        // naked-eye limit, far more than additive blending can usefully show.
        let lum = 10f32.powf(-0.24 * mag).min(2.5);
        // Bright stars also get a slightly larger quad (their glow bleeds wider).
        let half_angle = 0.0010 + 0.0009 * ((2.5 - mag) / 4.0).clamp(0.0, 1.0);
        let tint = star_color(bv) * lum;
        let seed = (i as f32 * 0.618_034).fract(); // per-star twinkle phase

        let base = positions.len() as u32;
        for (su, sv) in [(-1.0, -1.0), (1.0, -1.0), (1.0, 1.0), (-1.0, 1.0)] {
            let p = (dir + (t1 * su + t2 * sv) * half_angle) * STAR_SHELL_RADIUS;
            positions.push([p.x, p.y, p.z]);
            normals.push([dir.x, dir.y, dir.z]);
            uvs.push([su, sv]);
            colors.push([tint.x, tint.y, tint.z, seed]);
        }
        indices.extend_from_slice(&[base, base + 1, base + 2, base, base + 2, base + 3]);
    }

    let mut mesh = Mesh::new(
        bevy::render::mesh::PrimitiveTopology::TriangleList,
        bevy::asset::RenderAssetUsages::RENDER_WORLD,
    );
    mesh.insert_attribute(Mesh::ATTRIBUTE_POSITION, positions);
    mesh.insert_attribute(Mesh::ATTRIBUTE_NORMAL, normals);
    mesh.insert_attribute(Mesh::ATTRIBUTE_UV_0, uvs);
    mesh.insert_attribute(Mesh::ATTRIBUTE_COLOR, colors);
    mesh.insert_indices(bevy::render::mesh::Indices::U32(indices));

    let material = materials.add(StarFieldMaterial {
        params: Vec4::ZERO, // brightness set each frame; starts invisible (day)
    });
    commands.spawn((
        Mesh3d(meshes.add(mesh)),
        MeshMaterial3d(material),
        Transform::default(),
        bevy::light::NotShadowCaster,
        bevy::light::NotShadowReceiver,
        StarSky,
    ));
}

/// Approximate display tint for a stellar B−V color index: blue-white for hot
/// stars (Rigel, −0.3) through white and yellow to orange-red for cool ones
/// (Betelgeuse, ~1.85). Piecewise-linear over perceptual anchor stops.
fn star_color(bv: f32) -> Vec3 {
    const STOPS: [(f32, Vec3); 6] = [
        (-0.3, Vec3::new(0.62, 0.72, 1.0)),
        (0.0, Vec3::new(0.79, 0.86, 1.0)),
        (0.4, Vec3::new(1.0, 0.98, 0.92)),
        (0.8, Vec3::new(1.0, 0.90, 0.76)),
        (1.5, Vec3::new(1.0, 0.80, 0.60)),
        (2.0, Vec3::new(1.0, 0.70, 0.46)),
    ];
    let bv = bv.clamp(STOPS[0].0, STOPS[STOPS.len() - 1].0);
    for w in STOPS.windows(2) {
        let ((a, ca), (b, cb)) = (w[0], w[1]);
        if bv <= b {
            return ca.lerp(cb, (bv - a) / (b - a));
        }
    }
    STOPS[STOPS.len() - 1].1
}

/// Spawns the fog-colored horizon-haze shell: a slightly smaller sibling of the star
/// box whose shader paints a band of fog color at the horizon (see horizonhaze.wgsl).
fn setup_horizon_haze(
    mut commands: Commands,
    mut meshes: ResMut<Assets<Mesh>>,
    mut materials: ResMut<Assets<HorizonHazeMaterial>>,
) {
    // Startup switch, alongside CARTO_NO_ATMOSPHERE: leave the haze shell out
    // entirely, to tell "the scene is dark" from "a sky shell is covering it"
    // when bisecting a render problem with `--shot`.
    if env_flag("CARTO_NO_HAZE") {
        return;
    }
    // Unit shell: `update_horizon_haze` scales it to the fog's saturation
    // distance each frame, so it stands *in front of* the ground the fog has
    // already hidden rather than behind it (see `fog_saturation_distance`).
    //
    // A sphere, not a box: the rim where the shell stops covering the ground is
    // the one edge the haze can't hide, so it has to sit at the same elevation in
    // every direction. A cube's rim runs from `r` at the face to `r√2` at the
    // corner, which swept up and down as a ring as the camera pivoted.
    // `ico(3)` — 1,280 triangles, not `ico(5)`'s 20,480. `horizonhaze.wgsl` samples
    // by per-pixel view direction and says so in its own header ("the mesh shape is
    // irrelevant — it's just a cheap shell around the camera"), so the subdivision
    // buys no shading quality whatsoever; it only decides how round the *rim* is,
    // which is the one thing the paragraph above cares about.
    //
    // Not `ico(2)`, and the difference is the rim rather than the silhouette. A
    // geodesic sphere's radius dips between its vertices by `1 - cos θ`, where θ is
    // the angular radius of a face: ~6.4° at `ico(2)` (0.6 %) against ~3.2° at
    // `ico(3)` (0.15 %). Both are nothing next to the cube's 41 % that the rejected
    // option above was rejected for, but the rim is a ring the eye tracks as the
    // camera pivots — the exact artefact that argument is about — so this takes the
    // 16× and leaves the last 4× on the table rather than spending the rim on it.
    let mesh = meshes.add(Sphere::new(1.0).mesh().ico(3).unwrap());
    let material = materials.add(HorizonHazeMaterial {
        color: LinearRgba::WHITE,
        params: Vec4::ZERO, // set each frame from the distance fog
        dir_light_color: LinearRgba::NONE,
        rim: Vec4::ZERO,
    });
    commands.spawn((
        Mesh3d(mesh),
        MeshMaterial3d(material),
        Transform::default(),
        bevy::light::NotShadowCaster,
        bevy::light::NotShadowReceiver,
        HorizonHaze,
    ));
}

/// Keeps the horizon-haze shell on the camera and matches its color and strength to
/// the camera's `DistanceFog`, so fogged ground and sky meet seamlessly at the horizon.
fn update_horizon_haze(
    dev: Res<DevPanelState>,
    globe: Res<systems::globe::GlobeState>,
    camera: Query<
        (&GridCell, &GlobalTransform, &Transform, &DistanceFog),
        (With<Camera3d>, Without<HorizonHaze>),
    >,
    mut haze: Query<
        (
            &mut Transform,
            &mut Visibility,
            &MeshMaterial3d<HorizonHazeMaterial>,
        ),
        With<HorizonHaze>,
    >,
    mut materials: ResMut<Assets<HorizonHazeMaterial>>,
) {
    let Ok((cell, cam, cam_local, fog)) = camera.single() else {
        return;
    };
    let Ok((mut tf, mut vis, mat_handle)) = haze.single_mut() else {
        return;
    };
    tf.translation = cam.translation();

    // The haze thins with altitude alongside the ground fog, so an orbit view
    // sees the map through clear air instead of a grey band at the horizon.
    let altitude = globe.altitude_m(cell, cam_local) as f32;
    let alt = systems::free_camera::fog_altitude_factor(altitude);
    let d = dev.fog_density * alt;

    // Peak alpha is the fog's *own* opacity where the map ends, read off the same
    // `DistanceFog` the ground wears rather than approximated: the shell is that
    // fog wall continued past the last tile, and anything short of it draws a step
    // at the horizon. The old `0.35 * alt + 1.2 * d` only reached 1.0 at density
    // 0.54, so at the default 0.15 the ground went to full fog colour while the
    // sky above it got 53% — a visible grey line at every ordinary altitude.
    // In practice this is ~1.0 for any density worth calling fog (82 km of it
    // against 7–15 km of visibility) and exactly 0 when the fog is off.
    let peak = systems::free_camera::fog_factor_at(
        &fog.falloff,
        systems::free_camera::HORIZON_FOG_DISTANCE,
    );

    // Stand the shell just inside the distance where the fog has hidden the ground
    // completely, so it draws *over* the far map instead of behind it. Bevy's
    // atmosphere composites over geometry only (and its in-scattering is clamped
    // at 32 km while its extinction is not), so ground past that reads as a warm
    // band that no amount of fog colour behind it can cover — see
    // `fog_saturation_distance`. Capped at the ground disc so a thin, high-altitude
    // haze never pulls the shell in over map the fog has genuinely left visible.
    let radius =
        systems::free_camera::fog_saturation_distance(&fog.falloff).clamp(1_000.0, 150_000.0);
    tf.scale = Vec3::splat(radius);

    // Once it carries no haze at all, drop the shell out of the frame rather than
    // drawing a fully transparent shell over the whole view every frame.
    let want = if peak <= 0.0 {
        Visibility::Hidden
    } else {
        Visibility::Inherited
    };
    if *vis != want {
        *vis = want;
    }

    if let Some(mut mat) = materials.get_mut(&mat_handle.0) {
        mat.color = fog.color.to_linear();
        // Density-driven only: the built-in atmosphere (Bevy 0.19) has no
        // single-scattering horizon artifact to cover, so there's no base haze —
        // a base veil just tinted the (already correct) sky with the warm fog colour.
        // The zenith floor (w) grows quadratically so light haze leaves the sky
        // blue while dense fog closes over the whole dome.
        mat.params = Vec4::new(0.25 * d, peak, fog.directional_light_exponent, d * d);
        mat.dir_light_color = fog.directional_light_color.to_linear();
        // Where the shell's rim meets the ground, in view-direction `y`: the ray
        // that grazes the sphere at `radius` is the same one that hits the ground
        // plane there, so it is `-altitude / radius` (small, and always below the
        // horizon). The shader ramps the haze from nothing at the rim to full at
        // the horizon, which is what keeps the rim from being an edge of its own:
        // the ground just inside it wears the atmosphere's own aerial perspective,
        // and at sunset that is a strong warm tint for flat fog colour to butt up
        // against. Fading in over that span also means the haze only takes over
        // where the ground is far enough for the atmosphere to have got it wrong.
        mat.rim = Vec4::new(-(altitude / radius).clamp(1e-4, 1.0), 0.0, 0.0, 0.0);
    }
}

/// Keeps the star field centred on the camera (so it reads as infinitely far),
/// spins it around the celestial pole with the time of day, and drives the
/// material's brightness (night factor, veiled by fog) and twinkle clock.
#[allow(clippy::type_complexity)]
fn update_star_sky(
    dev: Res<DevPanelState>,
    time: Res<Time>,
    globe: Res<systems::globe::GlobeState>,
    camera: Query<&GlobalTransform, With<Camera3d>>,
    free_camera: Query<
        (&GridCell, &Transform),
        (With<crate::components::FreeCamera>, Without<StarSky>),
    >,
    mut star: Query<
        (
            &mut Transform,
            &mut Visibility,
            &MeshMaterial3d<StarFieldMaterial>,
        ),
        With<StarSky>,
    >,
    mut materials: ResMut<Assets<StarFieldMaterial>>,
) {
    let Ok(cam) = camera.single() else {
        return;
    };
    let Ok((mut tf, mut vis, mat_handle)) = star.single_mut() else {
        return;
    };
    // In space the shell moves out past the far limb so stars sit *behind* the
    // planet instead of blending over it (see `GlobeState::sky_shell_radius`).
    let space_shell = free_camera
        .single()
        .ok()
        .and_then(|(cell, cam_tf)| globe.sky_shell_radius(cell, cam_tf));
    tf.scale = Vec3::splat(space_shell.map_or(1.0, |r| r / STAR_SHELL_RADIUS));
    tf.translation = cam.translation();
    // Diurnal rotation of the celestial sphere (shared with the moon, so they
    // move together). Quick-cycle mode becomes a star-trail time-lapse.
    tf.rotation = utils::sun::celestial_rotation(dev.time_of_day);

    // Fade in once the sun is below the horizon; full when it's well below (dark sky).
    // Above the atmosphere there is no daylight to drown them: the stars are out
    // whatever the clock at the anchor says, and no fog can veil them. Without
    // this the space view was an unlit black void whenever it was daytime below.
    let elevation = sun_direction(dev.time_of_day, dev.sun_declination_rad).y;
    let night = match space_shell {
        Some(_) => 1.0,
        None => ((-0.05 - elevation) / 0.2).clamp(0.0, 1.0),
    };
    // Fog is opaque to starlight long before it swallows the city: veil the star
    // field with density so a foggy night reads as murk, not crisp stars over murk.
    let fog_veil = match space_shell {
        Some(_) => 1.0,
        None => (-3.0 * dev.fog_density).exp(),
    };
    // Drop the field out of the frame entirely by day, rather than blending
    // ~8,900 additive quads at a brightness of zero. `bright_stars.bin` is 16
    // bytes a star, so that is ~17,800 triangles in the transparent phase — with
    // `AlphaMode::Add` and no depth write, so not one of their fragments is
    // rejected early — multiplied by nothing, for the whole daylight half of the
    // cycle. The clouds, the rain and the haze shell each already do exactly
    // this, each with its own note saying why; the star field and the moon were
    // simply missed when they grew their fades.
    let brightness = night * STAR_BRIGHTNESS * fog_veil;
    let want = if brightness <= 0.0 {
        Visibility::Hidden
    } else {
        Visibility::Inherited
    };
    if *vis != want {
        *vis = want;
    }
    if let Some(mut mat) = materials.get_mut(&mat_handle.0) {
        mat.params = Vec4::new(
            brightness,
            time.elapsed_secs(),
            // Space flag: drops the shader's horizon cut, air extinction and
            // twinkle, none of which exist above the atmosphere.
            space_shell.is_some() as u32 as f32,
            0.0,
        );
    }
}

/// Marker for the procedural cloud-ceiling shell (see `shaders/clouds.wgsl`).
#[derive(Component)]
struct Clouds;

/// Height of the low cloud layer above the camera, in metres — a mid-level deck.
const CLOUD_LAYER_HEIGHT: f32 = 2_500.0;
/// Height of the high (cirrus) deck. The gap between the two is what produces
/// parallax as the camera translates — see the shader's header.
const CLOUD_HIGH_LAYER_HEIGHT: f32 = 7_000.0;
/// Vertical extent of the low deck, in metres. The sun march traverses it, so it
/// sets how far a shadow is thrown across the field at a given sun elevation.
const CLOUD_DECK_THICKNESS: f32 = 900.0;
/// Overall cloud opacity before the coverage/altitude/space fades.
const CLOUD_OPACITY: f32 = 0.92;
/// How prominent the high cirrus deck is relative to the low one.
const CLOUD_HIGH_OPACITY: f32 = 0.55;
/// Edge softness of the coverage cutoff (0 = hard, higher = wispier margins).
///
/// Widened from 0.18 when the alpha curve became Beer's law: at an optical depth
/// of 7 a column is 60 % opaque a quarter of the way up the coverage ramp, where
/// the old `smoothstep().powf(0.75)` needed half of it, so the *visible* edge band
/// halved. Roughly doubling the ramp restores it.
const CLOUD_SOFTNESS: f32 = 0.30;
/// Wind velocity in metres/second (drifts the deck). Faster than any real wind —
/// it is tuned so the field visibly crosses the sky in well under a minute rather
/// than to be meteorologically honest. (`systems::water` reuses the *direction*
/// so the waves travel with the clouds; only that is load-bearing there.)
pub const CLOUD_WIND: Vec2 = Vec2::new(118.0, 44.0);
/// Gain on the sunlit scattering term. The scattering model is not
/// energy-conserving, so this is the one knob that sets the deck's absolute
/// brightness; the *shape* of the lighting comes from the shader.
const CLOUD_SUN_GAIN: f32 = 1.0;
/// Gain on the ambient sky term — how much the shadowed side is filled in.
const CLOUD_AMBIENT_GAIN: f32 = 1.0;
/// Clouds are at full strength up to this camera altitude, then fade out so a
/// high flight or orbit view sees clear sky (the shell is a ceiling seen from
/// below; flying *above* the deck to look down on the tops is a follow-up).
const CLOUD_FADE_LOW: f32 = 2_000.0;
/// Camera altitude past which the cloud deck has fully faded out.
const CLOUD_FADE_HIGH: f32 = 9_000.0;

/// Camera-centred procedural cloud ceiling, alpha-blended over the sky. Coverage
/// is driven by the dev panel (`DevPanelState::cloud_cover`); the sun and sky
/// colours are the *same* day/night model the sunlight and the fog are built
/// from, so the deck can't drift off the sky's palette (see `update_clouds`).
#[derive(Asset, TypePath, AsBindGroup, Clone)]
struct CloudMaterial {
    /// x: coverage 0–1, y: softness, z: time (wind clock), w: low deck height (m).
    #[uniform(0)]
    params0: Vec4,
    /// xy: wind velocity (m/s), z: space fade (1 = hidden), w: altitude fade.
    #[uniform(1)]
    params1: Vec4,
    /// xyz: world direction to the sun. w: deck thickness (m), for the sun march.
    #[uniform(2)]
    sun_dir: Vec4,
    /// Sunlight reaching the deck's altitude; alpha carries the overall opacity.
    #[uniform(3)]
    sun_color: LinearRgba,
    /// Ambient sky light on the deck; alpha scales the high deck's opacity.
    #[uniform(4)]
    sky_color: LinearRgba,
    /// xy: world-anchor offset in metres (camera displacement from the anchor), so
    /// the deck stays fixed over the ground. z: the camera fog's per-metre density
    /// (aerial perspective). w: high deck height (m).
    #[uniform(5)]
    params2: Vec4,
    /// The colour the deck recedes into with distance — the camera's fog colour,
    /// so cloud and sky reach the horizon together.
    #[uniform(6)]
    haze_color: LinearRgba,
}

impl Material for CloudMaterial {
    fn fragment_shader() -> ShaderRef {
        "shaders/clouds.wgsl".into()
    }

    fn alpha_mode(&self) -> AlphaMode {
        AlphaMode::Blend
    }

    fn specialize(
        _pipeline: &MaterialPipeline,
        descriptor: &mut RenderPipelineDescriptor,
        _layout: &MeshVertexBufferLayoutRef,
        _key: MaterialPipelineKey<Self>,
    ) -> Result<(), SpecializedMeshPipelineError> {
        // Camera is inside the shell; draw its interior and don't occlude the
        // world behind it (depth test still lets geometry cover the clouds).
        descriptor.primitive.cull_mode = None;
        if let Some(depth) = descriptor.depth_stencil.as_mut() {
            depth.depth_write_enabled = Some(false);
        }
        Ok(())
    }
}

/// Spawns the cloud shell (mirrors `setup_horizon_haze`). `CARTO_NO_CLOUDS`
/// leaves it out for bisecting a render problem with `--shot`.
fn setup_clouds(
    mut commands: Commands,
    mut meshes: ResMut<Assets<Mesh>>,
    mut materials: ResMut<Assets<CloudMaterial>>,
) {
    if env_flag("CARTO_NO_CLOUDS") {
        return;
    }
    let mesh = meshes.add(Cuboid::from_length(360_000.0).mesh());
    let material = materials.add(CloudMaterial {
        params0: Vec4::ZERO, // set each frame from the dev panel
        params1: Vec4::ZERO,
        sun_dir: Vec4::Y,
        sun_color: LinearRgba::WHITE,
        sky_color: LinearRgba::BLACK,
        params2: Vec4::ZERO,
        haze_color: LinearRgba::BLACK,
    });
    commands.spawn((
        Mesh3d(mesh),
        MeshMaterial3d(material),
        Transform::default(),
        bevy::light::NotShadowCaster,
        bevy::light::NotShadowReceiver,
        Clouds,
    ));
}

/// Keeps the cloud shell centred on the camera, drives coverage from the dev
/// panel and the lighting from the day/night cycle, and fades the deck out with
/// altitude (high flight / orbit) and the space tier.
///
/// The two light colours are the load-bearing part: a cloud's colour has to be an
/// *output of the sky model* rather than a palette of its own
/// (`docs/notes/cloud-lighting.md`). So the sun colour is `utils::sun`'s
/// transmittance — the very function the sun light and the fog colour are built
/// from — and the haze colour is read straight off the camera's own
/// `DistanceFog`, which is what the horizon shell paints. Hand-authored
/// lit/shadow constants used to live here, and they drifted.
#[allow(clippy::type_complexity)]
fn update_clouds(
    dev: Res<DevPanelState>,
    time: Res<Time>,
    globe: Res<systems::globe::GlobeState>,
    camera: Query<
        (&GridCell, &GlobalTransform, &Transform, &DistanceFog),
        (With<Camera3d>, Without<Clouds>),
    >,
    mut clouds: Query<
        (
            &mut Transform,
            &mut Visibility,
            &MeshMaterial3d<CloudMaterial>,
        ),
        With<Clouds>,
    >,
    mut materials: ResMut<Assets<CloudMaterial>>,
) {
    let Ok((cell, cam, cam_local, fog)) = camera.single() else {
        return;
    };
    let Ok((mut tf, mut vis, mat_handle)) = clouds.single_mut() else {
        return;
    };
    tf.translation = cam.translation();

    let altitude = globe.altitude_m(cell, cam_local) as f32;
    let alt_fade =
        (1.0 - ((altitude - CLOUD_FADE_LOW) / (CLOUD_FADE_HIGH - CLOUD_FADE_LOW))).clamp(0.0, 1.0);
    let space = globe.space_factor(cell, cam_local);

    // Drop the shell out of the frame entirely once it carries no cloud, rather
    // than blending a fully transparent 360 km box over the whole view.
    let want = if dev.cloud_cover <= 0.0 || alt_fade <= 0.0 || space >= 1.0 {
        Visibility::Hidden
    } else {
        Visibility::Inherited
    };
    if *vis != want {
        *vis = want;
    }

    let hours = dev.time_of_day;
    let decl = dev.sun_declination_rad;
    let day = utils::sun::daylight(hours, decl);

    // Sunlight *at deck altitude* — less air above the deck than above the city,
    // so it is brighter and less reddened than the beam the ground gets. That
    // difference is what makes a sunset deck read as lit from above rather than
    // as a grey lid, and it is why the sun colour can't just be reused from
    // `sun_light`. Faded out through sunrise/sunset by the disc's own occlusion,
    // so the scattering term goes to nothing rather than snapping off.
    let beam = utils::sun::sun_transmittance_at(hours, decl, CLOUD_LAYER_HEIGHT);
    let sun_rgb = beam * utils::sun::sun_disc(hours, decl) * CLOUD_SUN_GAIN;

    // Ambient sky light filling the shadowed side. Built from the same
    // ingredients — and in the same shape — as `free_camera::distance_fog`'s
    // colour, so the two agree at every hour instead of being two guesses at the
    // same sky: a grey-blue day dome, near-black at night, warmed toward the sun's
    // own tint while it sits near the horizon.
    let tint = beam / beam.max_element().max(1e-4);
    let warm = 0.35 * utils::sun::horizon_proximity(hours, decl);
    let sky_day = Vec3::new(0.38, 0.44, 0.55);
    // Deliberately brighter, and warmer, than the fog's night colour: a cloud base
    // over a city catches its glow from below, so an overcast night sky reads as a
    // dim lid rather than as a void. Matching the fog exactly here would make the
    // deck darker than the sky behind it and turn it into black holes in the star
    // field — the one place cloud and fog should *not* agree.
    let sky_night = Vec3::new(0.045, 0.045, 0.055);
    let sky_rgb = sky_night.lerp(sky_day, day) * Vec3::ONE.lerp(tint, warm) * CLOUD_AMBIENT_GAIN;

    if let Some(mut mat) = materials.get_mut(&mat_handle.0) {
        mat.params0 = Vec4::new(
            dev.cloud_cover,
            CLOUD_SOFTNESS,
            time.elapsed_secs(),
            CLOUD_LAYER_HEIGHT,
        );
        mat.params1 = Vec4::new(CLOUD_WIND.x, CLOUD_WIND.y, space, alt_fade);
        mat.sun_dir = sun_direction(hours, decl).extend(CLOUD_DECK_THICKNESS);
        mat.sun_color = LinearRgba::new(sun_rgb.x, sun_rgb.y, sun_rgb.z, CLOUD_OPACITY);
        mat.sky_color = LinearRgba::new(sky_rgb.x, sky_rgb.y, sky_rgb.z, CLOUD_HIGH_OPACITY);
        mat.haze_color = fog.color.to_linear();
        // World-anchor: the camera's horizontal position in the local anchor
        // frame, in metres, so the deck stays fixed over the ground as you fly
        // under it rather than sliding with the camera. Metres, not the shader's
        // UV units, because the two decks have different frequencies — and it
        // retires the `CLOUD_FREQ` constant that existed only to mirror the
        // shader's, one duplicated number less to keep in step.
        let edge = crate::grid::WORLD_CELL_EDGE as f32;
        let world_x = cell.x as f32 * edge + cam_local.translation.x;
        let world_z = cell.z as f32 * edge + cam_local.translation.z;
        mat.params2 = Vec4::new(
            world_x,
            world_z,
            fog_density_coefficient(&fog.falloff),
            CLOUD_HIGH_LAYER_HEIGHT,
        );
    }
}

/// Per-metre density of a [`FogFalloff`], for the cloud deck's aerial perspective
/// — 0 whenever the fog is off.
///
/// `free_camera::distance_fog` only ever builds `ExponentialSquared` (fog on) or
/// `Linear` at infinity (fog off), and the shader hard-codes the squared curve to
/// match. `Exponential` is mapped anyway so a future falloff change degrades to
/// "slightly wrong haze" rather than to no haze at all.
fn fog_density_coefficient(falloff: &FogFalloff) -> f32 {
    match falloff {
        FogFalloff::ExponentialSquared { density } => *density,
        FogFalloff::Exponential { density } => *density,
        _ => 0.0,
    }
}

/// Marker for the screen-space rain quad (see `shaders/rain.wgsl`).
#[derive(Component)]
struct Rain;

/// Wind slant of the rain streaks (screen-space shear).
const RAIN_SLANT: f32 = 0.12;
/// Overall opacity of the rain streaks.
const RAIN_OPACITY: f32 = 0.5;
/// Rain is full-strength up to this altitude, then fades (you climb out of it).
const RAIN_FADE_LOW: f32 = 1_500.0;
/// Camera altitude past which the rain has fully faded out.
const RAIN_FADE_HIGH: f32 = 6_000.0;

/// Screen-space rain: an animated streak overlay on a small camera-centred shell
/// (`update_rain` keeps it on the camera). The shell's interior sits ~5 m out —
/// nearer than any world geometry — so with depth-write off the streaks draw over
/// the whole scene; the egui HUD still draws in its own later pass. The pattern is
/// keyed off the framebuffer coordinate, so the mesh shape only has to cover view.
#[derive(Asset, TypePath, AsBindGroup, Clone)]
struct RainMaterial {
    /// x: intensity 0–1, y: time (fall clock), z: wind slant, w unused.
    #[uniform(0)]
    params: Vec4,
    /// Streak colour; alpha carries the overall opacity.
    #[uniform(1)]
    tint: LinearRgba,
}

impl Material for RainMaterial {
    fn fragment_shader() -> ShaderRef {
        "shaders/rain.wgsl".into()
    }

    fn alpha_mode(&self) -> AlphaMode {
        AlphaMode::Blend
    }

    fn specialize(
        _pipeline: &MaterialPipeline,
        descriptor: &mut RenderPipelineDescriptor,
        _layout: &MeshVertexBufferLayoutRef,
        _key: MaterialPipelineKey<Self>,
    ) -> Result<(), SpecializedMeshPipelineError> {
        descriptor.primitive.cull_mode = None;
        if let Some(depth) = descriptor.depth_stencil.as_mut() {
            depth.depth_write_enabled = Some(false);
        }
        Ok(())
    }
}

/// Spawns the rain overlay quad. `CARTO_NO_RAIN` leaves it out for `--shot`
/// bisecting.
fn setup_rain(
    mut commands: Commands,
    mut meshes: ResMut<Assets<Mesh>>,
    mut materials: ResMut<Assets<RainMaterial>>,
) {
    if env_flag("CARTO_NO_RAIN") {
        return;
    }
    let mesh = meshes.add(Cuboid::from_length(10.0).mesh());
    let material = materials.add(RainMaterial {
        params: Vec4::ZERO,
        tint: LinearRgba::rgb(0.72, 0.78, 0.88),
    });
    commands.spawn((
        Mesh3d(mesh),
        MeshMaterial3d(material),
        Transform::default(),
        bevy::light::NotShadowCaster,
        bevy::light::NotShadowReceiver,
        Rain,
    ));
}

/// Keeps the rain shell centred on the camera, drives intensity from the dev
/// panel's precipitation slider, and fades it out with altitude.
#[allow(clippy::type_complexity)]
fn update_rain(
    dev: Res<DevPanelState>,
    time: Res<Time>,
    globe: Res<systems::globe::GlobeState>,
    camera: Query<(&GridCell, &GlobalTransform, &Transform), (With<Camera3d>, Without<Rain>)>,
    mut rain: Query<
        (
            &mut Transform,
            &mut Visibility,
            &MeshMaterial3d<RainMaterial>,
        ),
        With<Rain>,
    >,
    mut materials: ResMut<Assets<RainMaterial>>,
) {
    let Ok((cell, cam, cam_local)) = camera.single() else {
        return;
    };
    let Ok((mut tf, mut vis, mat_handle)) = rain.single_mut() else {
        return;
    };

    let altitude = globe.altitude_m(cell, cam_local) as f32;
    let alt_fade =
        (1.0 - ((altitude - RAIN_FADE_LOW) / (RAIN_FADE_HIGH - RAIN_FADE_LOW))).clamp(0.0, 1.0);
    let intensity = dev.precipitation * alt_fade;

    // Drop the shell out of the frame when it carries no rain, the same way
    // `update_clouds` does with its own — dry weather is the common case (and
    // every scripted `--shot` run), and the shader is not cheap to run for a
    // guaranteed-zero result: it is an alpha-blended box drawn from the inside,
    // so it covers the viewport, and each fragment runs three `rain_layer`
    // octaves of hashes and smoothsteps before being multiplied by an intensity
    // of 0 at the very end.
    let want = if intensity <= 0.0 {
        Visibility::Hidden
    } else {
        Visibility::Inherited
    };
    if *vis != want {
        *vis = want;
    }
    if intensity <= 0.0 {
        // Also skips the material write below, which was re-uploading a uniform
        // every frame to say the same thing.
        return;
    }

    tf.translation = cam.translation();

    if let Some(mut mat) = materials.get_mut(&mat_handle.0) {
        mat.params = Vec4::new(intensity, time.elapsed_secs(), RAIN_SLANT, 0.0);
        mat.tint = LinearRgba::new(0.72, 0.78, 0.88, RAIN_OPACITY);
    }
}

/// Marker for the planet-wrapping cloud sphere (see `shaders/globe_clouds.wgsl`).
#[derive(Component)]
struct GlobeClouds;

/// Height of the planet cloud shell above the globe surface, in metres. Well
/// clear of the surface so it doesn't z-fight the globe from orbit (the depth
/// test is also forced Always — see the material's `specialize`).
const GLOBE_CLOUD_ALT: f32 = 25_000.0;
/// 3D noise frequency for the planet cloud sphere (feature scale on the globe) —
/// low, so cloud systems are large and connected rather than a field of puffs.
const GLOBE_CLOUD_FREQ: f32 = 1.8;
/// Overall opacity of the planet cloud sphere.
const GLOBE_CLOUD_OPACITY: f32 = 0.85;
/// Fixed global cloudiness for the planet layer — a realistic partly-cloudy Earth
/// (the shader's latitude banding pushes this up in the ITCZ, down in the
/// subtropics). Deliberately NOT the ground weather slider, which is one point.
const GLOBE_CLOUD_COVER: f32 = 0.5;

/// Planet-scale cloud layer for the globe / space tier: a sphere just above the
/// surface, sampled by a 3D noise field and shaded by the sun terminator. Fades
/// in with `space_factor` (absent on the ground, where the ceiling shell rules).
#[derive(Asset, TypePath, AsBindGroup, Clone)]
struct GlobeCloudMaterial {
    /// xyz: sphere centre in render space, w: radius (m).
    #[uniform(0)]
    centre: Vec4,
    /// World direction to the sun (local anchor frame).
    #[uniform(1)]
    sun_dir: Vec4,
    /// x: coverage, y: opacity (space fade), z: time, w: noise frequency.
    #[uniform(2)]
    params: Vec4,
    /// Sunlit cloud colour; alpha carries the overall opacity.
    #[uniform(3)]
    lit_color: LinearRgba,
    /// Night-side cloud colour.
    #[uniform(4)]
    shadow_color: LinearRgba,
    /// Earth's spin axis in the local frame (latitude banding + zonal drift).
    #[uniform(5)]
    north_axis: Vec4,
    /// xyz: local-frame direction to geographic (0°,0°), used to recover longitude
    /// for the data lookup. w: data weight (0 = procedural fallback, 1 = live
    /// GFS/TCDC coverage). See `systems::globe_weather` and ADR-032.
    #[uniform(6)]
    frame_ref: Vec4,
    /// Live Total Cloud Cover grid (equirectangular, r = cloud fraction). A
    /// mid-grey placeholder until the first GFS fetch lands (weight stays 0 so it's
    /// unused meanwhile).
    #[texture(7)]
    #[sampler(8)]
    cloud_field: Handle<Image>,
}

impl Material for GlobeCloudMaterial {
    fn fragment_shader() -> ShaderRef {
        "shaders/globe_clouds.wgsl".into()
    }

    fn alpha_mode(&self) -> AlphaMode {
        AlphaMode::Blend
    }

    fn specialize(
        _pipeline: &MaterialPipeline,
        descriptor: &mut RenderPipelineDescriptor,
        _layout: &MeshVertexBufferLayoutRef,
        _key: MaterialPipelineKey<Self>,
    ) -> Result<(), SpecializedMeshPipelineError> {
        use bevy::render::render_resource::CompareFunction;
        // Seen from outside in orbit: keep back-face culling (draw the outer
        // shell). Don't write depth, and force the depth test to Always — from
        // 10,000 km the ~25 km shell is within the globe's depth precision, so a
        // normal test z-fights and the clouds show only as scattered dots; Always
        // makes the near hemisphere's clouds draw cleanly over the surface.
        if let Some(depth) = descriptor.depth_stencil.as_mut() {
            depth.depth_write_enabled = Some(false);
            depth.depth_compare = Some(CompareFunction::Always);
        }
        Ok(())
    }
}

/// Spawns the planet cloud sphere (hidden until the globe/space tier). A unit
/// sphere scaled to the shell radius each frame in `update_globe_clouds`.
fn setup_globe_clouds(
    mut commands: Commands,
    mut meshes: ResMut<Assets<Mesh>>,
    mut materials: ResMut<Assets<GlobeCloudMaterial>>,
    mut images: ResMut<Assets<Image>>,
) {
    if env_flag("CARTO_NO_CLOUDS") {
        return;
    }
    // Placeholder cloud-cover grid (mid-grey) at the live grid's dimensions, so the
    // texture binding is valid from frame 0. `globe_weather` overwrites its pixels
    // once a GFS fetch lands and ramps the material's data weight up from 0.
    let cloud_field = images.add(systems::globe_weather::placeholder_grid());
    let mesh = meshes.add(Sphere::new(1.0).mesh().ico(6).unwrap());
    let material = materials.add(GlobeCloudMaterial {
        centre: Vec4::ZERO,
        sun_dir: Vec4::Y,
        params: Vec4::ZERO,
        lit_color: LinearRgba::WHITE,
        shadow_color: LinearRgba::BLACK,
        north_axis: Vec4::Y,
        frame_ref: Vec4::new(1.0, 0.0, 0.0, 0.0),
        cloud_field: cloud_field.clone(),
    });
    commands.insert_resource(systems::globe_weather::GlobeCloudData::new(cloud_field));
    commands.spawn((
        Mesh3d(mesh),
        MeshMaterial3d(material),
        Transform::default(),
        Visibility::Hidden,
        bevy::light::NotShadowCaster,
        bevy::light::NotShadowReceiver,
        GlobeClouds,
    ));
}

/// Positions the planet cloud sphere at Earth's render-space centre (like
/// `globe::update_atmosphere_origin`), sizes it to the shell radius, and drives
/// coverage / lighting / space fade. Hidden below the globe threshold.
#[allow(clippy::type_complexity)]
fn update_globe_clouds(
    dev: Res<DevPanelState>,
    time: Res<Time>,
    globe: Res<systems::globe::GlobeState>,
    camera: Query<
        (&GridCell, &Transform),
        (With<crate::components::FreeCamera>, Without<GlobeClouds>),
    >,
    mut clouds: Query<
        (
            &mut Transform,
            &mut Visibility,
            &MeshMaterial3d<GlobeCloudMaterial>,
        ),
        With<GlobeClouds>,
    >,
    mut materials: ResMut<Assets<GlobeCloudMaterial>>,
    cloud_data: Option<Res<systems::globe_weather::GlobeCloudData>>,
    // Resolved once, not every frame: `std::env::var` takes a process-global lock
    // and allocates, and this is a debug seed that cannot change while running.
    // Same shape `frame_stats` uses for its own flag.
    mut cloud_time_override: Local<Option<Option<f32>>>,
) {
    let Ok((mut tf, mut vis, mat_handle)) = clouds.single_mut() else {
        return;
    };
    let Ok((cell, cam_local)) = camera.single() else {
        return;
    };
    let space = if globe.ready {
        globe.space_factor(cell, cam_local)
    } else {
        0.0
    };

    // Absent on the ground; the camera-relative ceiling shell covers that tier.
    let want = if space <= 0.0 {
        Visibility::Hidden
    } else {
        Visibility::Inherited
    };
    if *vis != want {
        *vis = want;
    }
    if space <= 0.0 {
        return;
    }

    // Earth's centre in render coords (camera is the floating origin), same as
    // the planet atmosphere entity.
    let edge = crate::grid::WORLD_CELL_EDGE as f64;
    let centre = globe.center_local.as_dvec3()
        - bevy::math::DVec3::new(cell.x as f64, cell.y as f64, cell.z as f64) * edge;
    let centre = centre.as_vec3();
    let radius = globe.radius + GLOBE_CLOUD_ALT;
    tf.translation = centre;
    tf.scale = Vec3::splat(radius);

    // Sunlit cloud colour is white lightly warmed by the current sun tint; the
    // shader's terminator handles the day→night falloff across the sphere.
    let (sun_c, _) = sun_light(dev.time_of_day, dev.sun_declination_rad, dev.fog_density);
    let sun_v = LinearRgba::from(sun_c).to_vec4();
    let mut lit = Vec4::splat(1.0).lerp(sun_v, 0.35);
    lit.w = GLOBE_CLOUD_OPACITY;

    if let Some(mut mat) = materials.get_mut(&mat_handle.0) {
        mat.centre = centre.extend(radius);
        mat.sun_dir = sun_direction(dev.time_of_day, dev.sun_declination_rad).extend(0.0);
        // Coverage: live global Total Cloud Cover (GFS/TCDC, ADR-032) when it has
        // landed, else the fixed procedural baseline. `GLOBE_CLOUD_COVER` is the
        // fallback the shader's latitude banding varies (heavier ITCZ, clear
        // subtropics); the ground weather slider is deliberately NOT used here (one
        // met.no point shouldn't overcast the planet). `frame_ref` carries the
        // geographic longitude reference for the data lookup and the data weight
        // (0 until the first fetch lands, then ramped to 1 in `globe_weather`).
        let weight = cloud_data.as_ref().map_or(0.0, |d| d.weight);
        mat.frame_ref = globe.direction_local(0.0, 0.0).extend(weight);
        // Cloud animation phase. Normally the elapsed wall-clock; a `--shot`
        // captures only ~2 s in, so cloud MOTION is otherwise untestable offscreen
        // — `CARTO_CLOUD_TIME=<seconds>` seeds the phase to inspect the field
        // at any age (e.g. verify it doesn't smear after 10 min), like `--time`
        // does for the day-night lighting.
        let seeded = *cloud_time_override.get_or_insert_with(|| {
            std::env::var("CARTO_CLOUD_TIME")
                .ok()
                .and_then(|s| s.parse::<f32>().ok())
        });
        let cloud_t = seeded.unwrap_or_else(|| time.elapsed_secs());
        mat.params = Vec4::new(GLOBE_CLOUD_COVER, space, cloud_t, GLOBE_CLOUD_FREQ);
        mat.lit_color = LinearRgba::from_vec4(lit);
        mat.shadow_color = LinearRgba::rgb(0.06, 0.09, 0.16);
        mat.north_axis = globe.north_axis_local().extend(0.0);
    }
}
