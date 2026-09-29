use bevy::asset::{AssetApp, AssetPlugin};
use bevy::light::DirectionalLightShadowMap;
// Used on every target except wasm, but for opposite reasons: desktop disables it
// in favour of `tracing_subscriber`, Android reconfigures it. See `run`.
#[cfg(not(target_arch = "wasm32"))]
use bevy::log::LogPlugin;
use bevy::pbr::MaterialPlugin;
use bevy::prelude::*;
use bevy::window::WindowResolution;
use clap::Parser;
use std::path::PathBuf;

mod components;
mod grid;
mod login;
mod platform;
mod quality;
mod resources;
mod simd_baseline;
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

/// Where the assets are read from at run time: [`ASSET_ROOT`], unless
/// `CARTO_ASSET_ROOT` names another directory (desktop only).
///
/// The override is for a binary that has been copied away from the checkout
/// that built it. The agent-driven client on the Steam Deck runs from a
/// directory of its own, and the compiled-in path points into a shared build
/// slot — where the next agent's sync would swap the shaders and models out
/// from under a binary built against the old ones.
pub(crate) fn asset_root() -> String {
    #[cfg(all(not(target_arch = "wasm32"), not(target_os = "android")))]
    if let Ok(dir) = std::env::var("CARTO_ASSET_ROOT") {
        if !dir.trim().is_empty() {
            return dir;
        }
    }
    ASSET_ROOT.to_string()
}

use big_space::prelude::{BigSpace, GridCell};
use components::Avatar;
use resources::{
    AuthIdentity, AvatarChooserState, AvatarState, CameraState, ContextMenuState, DevPanelState,
    EditDialogState, GameState, LocalAvatarSimId, MarqueeState, OsmTileUrlTemplate,
    PrimTextureCache, TextureLibrary,
};
use systems::egui_manager::EguiPlugin;
use systems::*;

#[derive(Parser, Debug)]
#[command(name = "cartopolis")]
struct Cli {
    /// Connect to a `simulator` at this address. Omit to enter whatever the
    /// sign-in screen last remembered — offline on a fresh install
    /// (`systems::session`).
    #[arg(long)]
    connect: Option<String>,

    /// Run without any sim: single user, no auth, no persistence. What `--shot`
    /// uses. The sign-in screen can still switch modes from inside the run; this
    /// only decides where the launch starts.
    #[arg(long)]
    offline: bool,

    /// Credentials for the `--connect` handshake, so you don't log in each run: a
    /// path to a TOML login file, or inline `user:pass`. Defaults to
    /// `data/dev-login.toml` if present, else env `CARTO_USER`/`CARTO_PASSWORD`,
    /// else `dev:dev`. Meaningless without a server to connect to.
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

    /// Run the performance suite (`docs/perf.toml` unless a path is given).
    ///
    /// Each scenario is flown, settled and captured exactly as `--shots` does, and
    /// then measured twice: over the streaming burst that filled it (`load`) and
    /// over a run of settled frames (`steady`). The report is JSON.
    ///
    /// Build with `--features perf` to add the two things Bevy's own diagnostics
    /// cannot give: allocation traffic, and per-render-pass GPU statistics — of
    /// which `fragment_shader_invocations` is the one that transfers to a handset,
    /// being a count of fill rather than a time.
    ///
    /// Bare `--perf` uses the checked-in manifest, exactly as `--shots` does.
    #[arg(long, value_name = "MANIFEST", num_args = 0..=1)]
    perf: Option<Option<PathBuf>>,

    /// Where the performance report goes. Defaults to `perf.json` in the
    /// temporary directory.
    #[arg(long, value_name = "PATH")]
    perf_out: Option<PathBuf>,

    /// Frames in every `steady` window, overriding the manifest.
    #[arg(long, value_name = "N")]
    perf_frames: Option<u32>,

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

    /// Aim at a place instead of at an angle: `--look-at 53.20542,6.57551`,
    /// paired with `--at` by position and overriding `--look`.
    ///
    /// **A pitch is not an aim.** What lands in frame is `altitude / tan(pitch)`
    /// metres ahead, and `altitude` is measured above the *map anchor's* ground
    /// while the subject stands on the measured ground somewhere else; at the
    /// shallow angle a sunken road or a viaduct needs, three metres of difference
    /// moves the aim thirty. Given a target, the direction is computed each frame
    /// from where the camera actually ended up, in the frame the world is drawn
    /// in (`shot_harness::aim_scripted_camera`). `--dump-state`'s
    /// `tunnel_entrance_lat`/`lng` is where to get the numbers for a tunnel.
    #[arg(long, value_name = "LAT,LNG")]
    look_at: Vec<String>,

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
    /// `places`, `avatar`, `stats`, `route`, or `all`. The menu bar and
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

    /// Capture every `--at` viewpoint **twice**, ~5 cm apart, and report the
    /// pixels that flipped between the pair as the `flicker_px` metric — the
    /// depth-stability instrument. A z-fight between coplanar surfaces re-rolls
    /// per pixel under any camera move, so it is invisible to a single still and
    /// lights up under a nudge that moves honest geometry sub-pixel. Gate it
    /// with `--expect 'flicker_px<=N'`, in a pinned environment (`--time
    /// --clouds 0 --rain 0`) so traffic and weather set the floor, not the sky.
    #[arg(long)]
    flicker: bool,

    /// Switch the **aerial basemap** on for this run (`systems::aerial`): PDOK's
    /// Luchtfoto RGB under the clipmap instead of the drawn map, over the
    /// Netherlands only. A headless run has no Layers panel to tick, and unlike
    /// the measured ground this one *replaces* the drawn map rather than adding
    /// to it, so it stays opt-in. `CARTO_AERIAL_LAYER` picks the layer.
    #[arg(long)]
    aerial: bool,

    /// Beside every capture, write the aerial photograph of the ground it looks
    /// at — `<shot>.aerial.jpg`, PDOK's Luchtfoto RGB with the camera and the
    /// point it is aimed at drawn on (`systems::aerial_reference`). The check a
    /// render of a real place is held to: whether the structure it draws is the
    /// one that is there. The Netherlands only.
    #[arg(long)]
    aerial_reference: bool,

    /// Serve the Model Context Protocol somewhere other than the default
    /// `127.0.0.1:4750`, so an editor can drive this client: move the camera,
    /// photograph what it sees, read the scene's numbers (`systems::mcp`).
    ///
    /// **Serving is on by default on the desktop** and switched in Settings ▸
    /// General ▸ Editor access; this flag only changes *where*, and forces it on
    /// for the run regardless of that switch, as a flag should. A bearer token is
    /// always required — `CARTO_MCP_TOKEN`, else the one in `user.json` that
    /// Settings shows.
    #[cfg(mcp)]
    #[arg(long, value_name = "ADDR", num_args = 0..=1, default_missing_value = systems::mcp::DEFAULT_LISTEN)]
    mcp_listen: Option<String>,

    /// Serve MCP with no window at all: the same offscreen rendering `--shot`
    /// uses, but staying up to answer calls instead of running a list and
    /// quitting.
    ///
    /// This is the shape for a machine nobody is sitting at. The cost a scripted
    /// capture pays is almost entirely *start-up* — the binary, the assets, the
    /// shader compiles, then a tile fill — and a process that stays up pays it
    /// once instead of once per viewpoint. Implies `--mcp-listen`, and offline
    /// unless `--connect` names a server — with one, the run signs in and an
    /// agent can walk, talk and build as that account (`systems::mcp::agent`).
    #[cfg(mcp)]
    #[arg(long)]
    mcp_headless: bool,

    /// Whose chat an agent driving this client may read: usernames separated by
    /// commas, or `*` for everyone. Everybody else's lines are withheld from it.
    /// Overrides Settings ▸ General ▸ Editor access for this run; with neither,
    /// the agent hears nobody.
    #[cfg(mcp)]
    #[arg(long, value_name = "NAMES")]
    mcp_allow: Option<String>,

    /// Accept MCP requests addressed to this host name too (repeatable). For a
    /// client reached through a forward, whose requests carry the forward's
    /// name — the DNS-rebinding guard refuses any other.
    #[cfg(mcp)]
    #[arg(long, value_name = "HOST")]
    mcp_host: Vec<String>,

    /// Offscreen surface size for `--mcp-headless`, `WxH`. Ignored when there is
    /// a window, where the window *is* the surface.
    #[cfg(mcp)]
    #[arg(long, value_name = "WxH")]
    mcp_size: Option<String>,

    /// Don't serve MCP this run, whatever Settings remembers.
    ///
    /// For running a second client beside one that already holds the port, and
    /// for anyone who would rather decide per launch. It does not change the
    /// stored preference — a flag is an instruction for this launch, never
    /// written back, the same rule the sky flags follow.
    #[cfg(mcp)]
    #[arg(long, conflicts_with_all = ["mcp_listen", "mcp_headless"])]
    no_mcp: bool,
}

/// Resolve the scripted capture the CLI asked for, if any.
///
/// Two ways in, and they don't mix: `--shots` runs the remembered camera
/// positions from a preset manifest (the documentation build step), `--shot`
/// places one or more ad-hoc viewpoints from `--at`/`--look`. Both are built
/// here, before the app, because the result decides whether the run gets a
/// window at all and how big its surface is.
type ScriptedRun = (
    Option<systems::shot_harness::ShotHarness>,
    Option<systems::perf::PerfRun>,
);

fn build_shot_harness(cli: &Cli) -> Result<ScriptedRun, String> {
    use systems::shot_harness::{ShotHarness, DEFAULT_SETTLE, DEFAULT_SIZE, DEFAULT_WAIT};

    // `--perf` first: a *third* way in, building both a capture harness to fly the
    // scenarios and the suite that measures them. An alternative to the other two
    // rather than a modifier of them, because a perf run pins its own size, settle
    // and weather — `--perf --shot` would have to decide which manifest won, and
    // every answer to that is surprising.
    if let Some(manifest) = cli.perf.clone() {
        if cli.shot.is_some() || cli.shots.is_some() {
            return Err("--perf, --shots and --shot are alternatives; pass one".into());
        }
        let path = manifest.unwrap_or_else(|| PathBuf::from(systems::perf::DEFAULT_MANIFEST));
        let (harness, run) = systems::perf::build(
            &path,
            &cli.only,
            cli.size.as_deref(),
            cli.perf_frames,
            cli.perf_out
                .clone()
                .unwrap_or_else(|| std::env::temp_dir().join("perf.json")),
        )
        .map_err(|e| format!("--perf: {e}"))?;
        return Ok((Some(harness), Some(run)));
    }
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
        .map(|mut h| {
            h.aerial_reference |= cli.aerial_reference;
            (Some(h), None)
        })
        .map_err(|e| format!("--shots: {e}"));
    }
    if !cli.only.is_empty() {
        return Err("--only names presets from a --shots manifest".into());
    }
    let Some(path) = cli.shot.clone() else {
        return Ok((None, None));
    };
    ShotHarness::from_cli(systems::shot_harness::ShotArgs {
        path,
        at: &cli.at,
        look: &cli.look,
        look_at: &cli.look_at,
        size: cli.size.as_deref().unwrap_or(DEFAULT_SIZE),
        settle: cli.settle.unwrap_or(DEFAULT_SETTLE),
        wait: cli.wait.unwrap_or(DEFAULT_WAIT),
        hide_ui: cli.hide_ui,
        ui: cli.ui.clone(),
        mode: cli.shot_mode.clone(),
        frames: cli.frames,
        csv: cli.csv.clone(),
        dump_state: cli.dump_state.clone(),
        expect: &cli.expect,
        flicker: cli.flicker,
    })
    .map(|mut h| {
        h.aerial_reference = cli.aerial_reference;
        (Some(h), None)
    })
    .map_err(|e| format!("--shot: {e}"))
}

/// Cap Bevy's GPU-driven draw path — by default on Android, and by
/// `CARTO_GPU_PREPROCESS` anywhere.
///
/// **Why.** The device losses on the Adreno 840 have survived every guard aimed at
/// memory: `handset_guarded`'s tile and radius caps, `MAX_CELL_MESH_BYTES`, the
/// 4 MB per-frame ceiling. The halt report of 2026-08-20 puts residency at 126 MB
/// and the whole wgpu allocator at 289 MB in use of 448 MB reserved — a fraction
/// of what this phone has, and by [`gpu_memory_census`]'s own decision rule the
/// reading that says *not a memory problem at all*. What is left is the commands
/// rather than the bytes, and the most GPU-authored commands in the frame are
/// Bevy's: in [`GpuPreprocessingMode::Culling`], a compute shader writes the
/// instance data *and* the indirect draw arguments the GPU then draws from, plus
/// the two-phase occlusion pass. An out-of-range value there is a GPU page fault,
/// which arrives with no warning on a healthy frame and reads as `DeviceLost` —
/// the shape of these losses (no slow frames, healthy census, sudden loss).
///
/// **The Android default is `PreprocessingOnly`, because upstream's own remedy for
/// this class is a denylist the 840 is too new for.** `BatchingPlugin` caps
/// `Adreno != 720 && <= 730` to CPU preprocessing ([bevy#14176]) and a Pixel 10 to
/// preprocessing-without-culling; [bevy#17591] is the same story for the two-phase
/// occlusion pass on further Android devices — the list grows one crashed device
/// at a time, and a part newer than the list gets the full indirect path on a
/// driver nobody upstream has run. This tree already holds a sibling of the same
/// judgement: GPU light clustering is off ([`ForceCpuClusteringPlugin`]) because
/// its readback lost an Intel device. `PreprocessingOnly` keeps the mesh uniforms
/// on the compute path and issues direct draws — the CPU still says *what* to
/// draw, so a bad GPU-written count can no longer be dereferenced.
///
/// `setprop debug.carto.gpu_preprocess cull` restores upstream's detection on the
/// phone; `off` drops to CPU preprocessing entirely if the cap turns out not to be
/// enough. The mode in force is in every halt report (`draw path:`), so a run that
/// still dies says which rung it died on.
///
/// [bevy#14176]: https://github.com/bevyengine/bevy/pull/14176
/// [bevy#17591]: https://github.com/bevyengine/bevy/issues/17591
struct GpuPreprocessingOverridePlugin;

impl Plugin for GpuPreprocessingOverridePlugin {
    fn build(&self, app: &mut App) {
        use bevy::render::batching::gpu_preprocessing::GpuPreprocessingSupport;
        use bevy::render::RenderApp;

        let Some(mode) = requested_gpu_preprocessing() else {
            return;
        };
        let Some(render_app) = app.get_sub_app_mut(RenderApp) else {
            return;
        };
        // `RenderStartup`, not `finish`: `init_gpu_resource` does not insert the
        // resource, it *registers a `RenderStartup` system* that builds it — and
        // that schedule runs again on every device the app acquires, so anything
        // written once at startup would be silently reverted. Ordering against
        // upstream's own system by name is what makes this an override rather than
        // a race.
        render_app.add_systems(
            bevy::render::RenderStartup,
            (move |mut support: ResMut<GpuPreprocessingSupport>| {
                // A *cap*, not an assignment: `GpuPreprocessingSupport::min`
                // cannot promote a mode past what the device supports, so `cull`
                // on a WebGL2 or Adreno-730 adapter stays whatever upstream
                // decided, never above it.
                support.max_supported_mode = support.min(mode);
                tracing::info!(
                    path = gpu_preprocessing_name(support.max_supported_mode),
                    "capped Bevy's draw path (Android default; CARTO_GPU_PREPROCESS overrides)"
                );
            })
            .after(bevy::render::init_gpu_resource::<GpuPreprocessingSupport>),
        );
    }
}

/// The cap on Bevy's draw path: `CARTO_GPU_PREPROCESS` = `off` | `preprocess` |
/// `cull` if set, otherwise `preprocess` on Android and no cap elsewhere.
///
/// Since the plugin applies this with [`GpuPreprocessingSupport::min`], `cull` is
/// a cap at the top — i.e. exactly upstream's own detection — which is what makes
/// it the right spelling for "give me back the default Bevy would have picked".
///
/// An unrecognised value warns and falls back to the platform default rather than
/// quietly parsing as something: this is a switch somebody sets once, on a phone,
/// to answer one question, and a typo that silently changed the answer would
/// answer it wrongly.
///
/// [`GpuPreprocessingSupport::min`]: bevy::render::batching::gpu_preprocessing::GpuPreprocessingSupport::min
fn requested_gpu_preprocessing(
) -> Option<bevy::render::batching::gpu_preprocessing::GpuPreprocessingMode> {
    use bevy::render::batching::gpu_preprocessing::GpuPreprocessingMode as M;
    // `cfg!`, not `#[cfg]`, for the reason `handset_guarded` states: a rule that
    // only exists in the Android build is a rule no test on any other machine can
    // see.
    let default = if cfg!(target_os = "android") {
        Some(M::PreprocessingOnly)
    } else {
        None
    };
    let Some(raw) = env_value("CARTO_GPU_PREPROCESS") else {
        return default;
    };
    match raw.trim().to_ascii_lowercase().as_str() {
        "off" | "none" | "cpu" => Some(M::None),
        "preprocess" | "preprocessing" => Some(M::PreprocessingOnly),
        "cull" | "culling" | "full" => Some(M::Culling),
        other => {
            tracing::warn!(
                value = other,
                "CARTO_GPU_PREPROCESS must be off, preprocess or cull — using the \
                 platform default"
            );
            default
        }
    }
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
/// The fixed configuration is **no flags at all** — and it was `--offline` until
/// 2026-08-21, which is worth spelling out because of what that silently cost.
/// The flag was deliberate when Android was Phase-1 (the streamed map, no
/// transport, no auth), but `--offline` is also the switch that skips the
/// remembered-session startup, and it kept doing so after the phone gained the
/// sign-in card and the WebSocket carrier: every launch saved the "stay signed
/// in" token and prefs faithfully and then never looked at them, which read as
/// the checkbox being broken. The sim's log was what said so — a reopened app
/// made *no* connection attempt at all, where even a stale token would have
/// shown a rejected handshake. With no flags the startup decision is the
/// remembered session's, exactly as on desktop; a fresh install still opens
/// offline because that is the default `SessionPrefs`.
/// (`login::resolve` runs now too, but with no `--login`, no `data/login.toml`
/// under the zygote's `/` cwd and no env vars it just answers the dev default,
/// whose server is `None` — it cannot start a session on its own.)
#[cfg(not(target_os = "android"))]
fn parse_cli() -> Cli {
    Cli::parse()
}

#[cfg(target_os = "android")]
fn parse_cli() -> Cli {
    // `parse_from` rather than a hand-built struct literal: every field keeps its
    // clap-declared default, so a new flag added to `Cli` cannot leave this arm
    // silently uninitialised.
    Cli::parse_from(["cartopolis"])
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
fn record_fatal(report: &str) {
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
fn handle_render_error(
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
        // `WindowSurfaces` away — and on every platform, not just the one that
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
/// `WindowSurfaces` out of the render world and leak it, so no `wgpu::Surface`
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
    let Some(surfaces) = render_world.remove_resource::<bevy::render::view::WindowSurfaces>()
    else {
        tracing::warn!("no WindowSurfaces to abandon at the halt — nothing held a swapchain");
        return;
    };
    // Not `drop`. See above: dropping is the thing that aborts.
    std::mem::forget(surfaces);
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
/// ([`abandon_lost_surface`] removes `WindowSurfaces`, and bevy's
/// `extract_windows` keeps running against it) that was 2,140 warnings in 50 s,
/// most of what the console cost the main thread. The first occurrence of each
/// distinct failure logs in full and then every 1,000th says how many there have
/// been, so a stream is still visible as a stream.
fn warn_once_per_failure(error: bevy::ecs::error::BevyError, ctx: bevy::ecs::error::ErrorContext) {
    use std::collections::HashMap;
    use std::sync::{Mutex, OnceLock};

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
    record_fatal(&report);
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

/// A name for a [`GpuPreprocessingMode`], which upstream derives no `Debug` for.
fn gpu_preprocessing_name(
    mode: bevy::render::batching::gpu_preprocessing::GpuPreprocessingMode,
) -> &'static str {
    use bevy::render::batching::gpu_preprocessing::GpuPreprocessingMode as M;
    match mode {
        M::None => "cpu (no preprocessing)",
        M::PreprocessingOnly => "gpu preprocessing, direct draws",
        M::Culling => "gpu preprocessing and gpu culling (indirect draws)",
    }
}

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
        // Private-chat keys are read on the frame a session comes up, which
        // cannot wait on IndexedDB either.
        storage::preload(systems::private_chat::STATE_PREFIX, "").await;
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

    // …and the bisecting switches, for the same reason and from the same early
    // read: `CARTO_NO_DUAL_SOURCE` is consulted while `DefaultPlugins` is being
    // assembled, several hundred lines before `UserStore::load()` runs. Anything
    // that reads `env_flag` before this line silently sees no preference at all.
    init_debug_switches(systems::user_store::UserStore::preferred_debug_switches());

    // Start the CPU worker pool before anything streams (ADR-033). A no-op
    // natively — `AsyncComputeTaskPool` is already a thread pool — and on the web
    // it spawns the Web Workers that keep tile decoding off the render thread.
    // One core is left for the main thread, which still renders and fetches.
    //
    // …and on a phone, historically rather more than one: `available_parallelism`
    // counted the Fairphone 4's 8 cores without saying that 6 of them were
    // efficiency cores sharing a thermal envelope with the render thread and the
    // compositor, so the honest ceiling was well under the count.
    //
    // That reasoning retired with the device (2026-08-18). The handset this is
    // developed against now is 2 prime + 6 performance Oryon cores with no
    // efficiency cores at all, and `auto()` puts Android on `Budget::balanced`,
    // whose `worker_cap` is `None` — so the cap below is once again only what a
    // deliberately-selected `phone` tier asks for.
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
    // What the stderr log shows, shared by both arms below so a profiling build and
    // an ordinary one log identically.
    //
    // wgpu/naga at `warn` so GPU validation + device-loss errors surface (they're
    // the real cause of the zoom-out crash; the panics only show the aftermath).
    // Override with RUST_LOG.
    //
    // `bevy_render=error` is not redundant with those: a validation failure is
    // reported by Bevy's *own* `bevy_render::error_handler`, which then writes the
    // `AppExit` that ends the run — and the `off` default silenced exactly that.
    // The observed failure was a `--shot` that exited 0 in 3 s with no PNG and an
    // empty log; the wgpu targets above said nothing, because the error never came
    // from them. A run that dies for a render reason must say so.
    #[cfg(all(not(target_arch = "wasm32"), not(target_os = "android")))]
    fn stderr_filter() -> tracing_subscriber::EnvFilter {
        tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| {
            tracing_subscriber::EnvFilter::new(
                "off,cartopolis=info,bevy_render=error,wgpu=warn,wgpu_core=warn,wgpu_hal=warn,naga=warn",
            )
        })
    }

    // **The per-system counter, which reports through `perf` rather than a file.**
    //
    // `bevy/trace` wraps every system run in a span and
    // `systems::dev::system_timings` counts them in process, so a perf window can
    // subtract two snapshots and say what *that window* cost. The alternative —
    // writing a chrome trace and aggregating it elsewhere — was built first and
    // thrown away: one `city` scenario was 2.5 GB, it needed a second program to
    // read, and a whole-run total cannot separate a streaming cost from a frame
    // cost, which is the only question worth asking here.
    //
    // It carries its own filter, and that is load-bearing: the stderr filter
    // defaults to `off` outside this crate, so a layer sharing it would faithfully
    // count nothing — the same silent no-op that makes `bevy/trace_chrome`
    // unusable in this tree. `bevy_ecs=info` is where the system spans live;
    // `CARTO_TRACE_FILTER` widens it for anyone who wants the render thread too.
    #[cfg(all(
        feature = "perf",
        not(target_arch = "wasm32"),
        not(target_os = "android")
    ))]
    {
        use tracing_subscriber::layer::SubscriberExt as _;
        use tracing_subscriber::util::SubscriberInitExt as _;
        // `with_filter` is `Layer`'s, and per-layer filtering is the whole point:
        // the stderr arm keeps its `off`-by-default filter while the counting arm
        // sees spans that filter would drop.
        use tracing_subscriber::Layer as _;
        let span_filter = tracing_subscriber::EnvFilter::try_from_env("CARTO_TRACE_FILTER")
            .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("off,bevy_ecs=info"));
        tracing_subscriber::registry()
            .with(
                tracing_subscriber::fmt::layer()
                    .with_writer(std::io::stderr)
                    .with_filter(stderr_filter()),
            )
            .with(systems::dev::system_timings::SystemTimings.with_filter(span_filter))
            .init();
    }

    #[cfg(all(
        not(feature = "perf"),
        not(target_arch = "wasm32"),
        not(target_os = "android")
    ))]
    tracing_subscriber::fmt()
        .with_env_filter(stderr_filter())
        .init();

    let mut app = App::new();
    // **A stale entity must not be able to kill the app on a phone.**
    //
    // Bevy's fallback handler panics on an `EntityDespawnedError`, and every
    // `commands.entity(e).insert(..)` in the tree reaches it. That is a *race*,
    // not a logic error: two systems with no ordering between them both hold a
    // live view of the same entity, one queues a despawn and the other queues an
    // insert, and the flush applies them in that order. It killed the Android
    // build on every app switch — a dropped session tore the server's own
    // entities down while a streamer was queueing a component onto one of them —
    // and it aborts the process, which on
    // a handheld reads as "the app closed itself" with the explanation only in
    // `logcat`. The same policy `handle_render_error` follows: a recoverable
    // failure logs and the app keeps running.
    //
    // Debug builds keep the panic, because that is where the race is meant to be
    // found. `warn` is deliberately not `error`: these are expected on the frame a
    // session ends, and a red line per frame trains people to ignore the log.
    if !cfg!(debug_assertions) {
        app.set_error_handler(warn_once_per_failure);
    }
    // The scripted capture is configured *before* the app is built: a preset
    // manifest can set the render size, and a windowless run needs that size to
    // stand a `HeadlessSurface` in for the missing `Window`. A bad manifest or
    // `--at` therefore fails here with a message, not as a black run.
    //
    // Mutable only where there is an MCP server to replace it with a live
    // harness; off the desktop nothing reassigns it and the `mut` is dead.
    #[cfg_attr(not(mcp), allow(unused_mut))]
    let (mut harness, perf_run) = match build_shot_harness(&cli) {
        Ok(h) => h,
        Err(e) => {
            eprintln!("{e}");
            std::process::exit(2);
        }
    };
    // Same rule, same reason: `--mcp-headless` also decides whether there is a
    // window, so a bad address or size has to fail here rather than at the first
    // call an editor makes. A scripted capture resolves to `None` — see
    // `Config::resolve`, which is also where the two are refused together.
    #[cfg(mcp)]
    let mcp_config = match systems::mcp::Config::resolve(
        cli.mcp_listen.as_deref(),
        cli.mcp_headless,
        cli.mcp_size.as_deref(),
        // `--no-mcp` takes this run out of it entirely, exactly as a scripted
        // capture is: no config, no listener, no Settings row, nothing to switch.
        harness.is_some() || cli.no_mcp,
    ) {
        Ok(c) => c.map(|mut c| {
            c.hosts = cli.mcp_host.clone();
            c
        }),
        Err(e) => {
            eprintln!("{e}");
            std::process::exit(2);
        }
    };
    // **A live harness for the MCP camera operator.** `view` photographs by
    // pushing a viewpoint onto the shot harness, which owns the only cycle that
    // knows how to place a camera, wait for the world and quiesce the workers
    // before the shutter (`systems::mcp`'s module note). So a run that *could*
    // serve MCP gets an *empty* harness that parks until it is handed a
    // viewpoint, rather than a second copy of that cycle.
    //
    // Built whether or not the listener is currently on, because the switch that
    // turns it on is in Settings: a harness that only existed when the app
    // started with serving enabled would make the switch a restart.
    #[cfg(mcp)]
    if let Some(cfg) = &mcp_config {
        harness = Some(systems::shot_harness::ShotHarness::live_harness(
            cfg.size,
            !cfg.headless,
        ));
    }
    // A run with no window renders into an offscreen image, so the shot size is
    // the whole surface — there is nothing for egui to lay out against but this
    // (see `HeadlessSurface`). Two ways to get there: a scripted capture, and
    // `--mcp-headless`. A *windowed* MCP run is an ordinary session that happens
    // to be drivable, and takes none of it.
    let windowless = harness.as_ref().is_some_and(|h| !h.windowed);
    let shot_size = windowless
        .then(|| harness.as_ref().map(|h| h.size))
        .flatten();
    // "A script owns this run": no user store, no frame limiter, no live weather,
    // no load controller. A windowed MCP session is somebody's session and keeps
    // all of it.
    let scripted = windowless;
    // Self-install the Linux desktop file + themed icon before the window exists,
    // so Wayland compositors (where set_window_icon is a no-op) can resolve the
    // app icon on first map. Idempotent, best-effort, no-op off Linux and for
    // windowless scripted captures. See `systems::desktop_integration`.
    if !scripted {
        systems::desktop_integration::ensure_desktop_integration();
    }
    let asset_dir = asset_root();
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
                    // Four, not two, and the floor is what a phone actually gets:
                    // `percent` resolves against `available_parallelism`, so an
                    // 8-core handset lands on 0.30 x 8 = 2 while the widths above
                    // were measured on a 22-core workstation that lands on 7. Two
                    // sockets is what the six streamers' 57 aggregate in-flight
                    // slots were really funnelling through, and `platform::http`'s
                    // fetch is a *blocking* call — so a single slow Overpass patch
                    // held half the device's HTTP capacity.
                    //
                    // Raising a floor rather than the percentage, because this is
                    // the small-machine case and the large one was measured. Safe
                    // for the same reason the paragraph above gives: these threads
                    // block on a socket rather than burning a core.
                    //
                    // Eight since 2026-09-24. The device cache is read on these
                    // same threads, so on a warm start a file read that takes a
                    // millisecond waits behind whichever four requests are parked
                    // on a slow host: on the Deck (8 cores, io=4) 49 water cells
                    // that were all on the disk had not finished loading 3 s in,
                    // while the terrain was ready to bake. Every streamer still
                    // caps its own requests in flight, so this widens the queue
                    // for reads, not the load on any server.
                    //
                    // Sixteen, because a cold start saturates eight: the Deck's
                    // cold fill held io threads for ~117 s of blocking requests
                    // inside ~40 s (PDOK 51 s, Overpass 28 s at 0.75 s each), and
                    // the basemap's 145 tile requests queued behind them — the
                    // map image finished at 11.7 s cold against 3.9 s warm.
                    min_threads: 16,
                    max_threads: 24,
                    percent: 0.30,
                    on_thread_spawn: None,
                    on_thread_destroy: None,
                },
                async_compute: bevy::app::TaskPoolThreadAssignmentPolicy {
                    // Four, which is what an 8-core device already got here while
                    // io was four: the pools are sized in order out of what is
                    // left, so io's floor of eight would otherwise leave this at
                    // two — and pipeline compilation runs here (below).
                    min_threads: 4,
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
                // **Bevy's parallel system executor, and without this floor a
                // handset gets exactly one thread for it.**
                //
                // `create_default_pools` sizes the three pools *in order*, each
                // taking from what the last one left, and
                // `get_number_of_threads` applies `min(remaining)` *before*
                // `clamp(min_threads, max_threads)`. Run that on the 8 cores an
                // Android device reports: io wants 0.30 × 8 = 2 but its floor
                // above lifts it to **4**, leaving 4; async_compute wants
                // 0.45 × 8 = 3.6 → 4, taking all of it and leaving **0**; and
                // compute — whose default policy is `percent: 1.0`, i.e.
                // "whatever is left over" — gets `min(8, 0) = 0`, clamped up to
                // its default floor of **1**.
                //
                // One thread for `ComputeTaskPool` means no two Update systems
                // ever overlap: transform propagation, `check_visibility`, every
                // `par_iter`, and all eight panel systems run strictly serially,
                // on the platform with the least headroom. It is not a tuning
                // loss, it is the parallel executor being switched off by
                // arithmetic, and it was invisible because the widths above were
                // measured on a 22-core box where they resolve to io=7,
                // async=10, compute=5 and the bug cannot appear.
                //
                // A floor rather than re-cutting the percentages, because those
                // two were measured (see the block above) and this is the case
                // they were not measured on. It changes nothing where there is
                // slack — the workstation still lands on 5 — and on an 8-core
                // handset it buys 3 at the cost of nominally oversubscribing by
                // 3, which the io threads pay for by blocking on sockets rather
                // than burning a core. Note the tile jobs are **not** in any of
                // these pools any more (`platform::workers`), so the real
                // contention is smaller than the totals suggest.
                compute: bevy::app::TaskPoolThreadAssignmentPolicy {
                    min_threads: 3,
                    max_threads: usize::MAX,
                    percent: 1.0,
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

    // `CARTO_NO_DUAL_SOURCE=1`: run the renderer without `DUAL_SOURCE_BLENDING`,
    // the way every Adreno and most Malis do (Qualcomm has never shipped
    // `dualSrcBlend`). bevy_pbr's atmosphere sky pass has a single-output fallback
    // for exactly that case, and this container's lavapipe — like every desktop
    // GPU — offers the feature, so the fallback is otherwise unreachable here.
    // A bisecting switch in the `CARTO_NO_*` family: it reproduces the phone's
    // blend path on a machine that can screenshot it.
    let default_plugins = if env_flag("CARTO_NO_DUAL_SOURCE") {
        default_plugins.set(bevy::render::RenderPlugin {
            render_creation: bevy::render::settings::RenderCreation::Automatic(Box::new(
                bevy::render::settings::WgpuSettings {
                    disabled_features: Some(
                        bevy::render::settings::WgpuFeatures::DUAL_SOURCE_BLENDING,
                    ),
                    ..default()
                },
            )),
            ..default()
        })
    } else {
        default_plugins
    };

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
    //
    // **How hard it spins depends on whether the run ends.** A scripted capture
    // has a list and an `AppExit` at the end of it, so it takes every frame it
    // can get — that is the whole cost of the run and it is over in a minute. A
    // headless MCP server has no end: it sits waiting for a call, and a zero
    // interval there is one core burned continuously, on a machine that is
    // usually shared, for a world nobody is looking at. `MCP_IDLE_HZ` is fast
    // enough that the streamers make progress during a settle and slow enough
    // that an idle server costs almost nothing. It is a *minimum* frame time, so
    // on a renderer already slower than that (this container draws a city in ~4 s)
    // it changes nothing at all.
    const MCP_IDLE_HZ: f64 = 30.0;
    let live_server = harness.as_ref().is_some_and(|h| h.live);
    let default_plugins = match shot_size {
        Some(_) => default_plugins.disable::<bevy::winit::WinitPlugin>().add(
            bevy::app::ScheduleRunnerPlugin::run_loop(if live_server {
                std::time::Duration::from_secs_f64(1.0 / MCP_IDLE_HZ)
            } else {
                std::time::Duration::ZERO
            }),
        ),
        None => default_plugins,
    };

    // Models a person uploaded to the simulator, reachable as `sim://<hash>.glb`
    // (`systems::model_assets`). Registered **before** the plugin group, because
    // `AssetPlugin::build` consumes the registered sources — a source added after
    // it is silently never used, and the symptom is a model that fails to load
    // with a path error naming a scheme nothing has heard of.
    app.register_asset_source(
        model_assets::SOURCE,
        bevy::asset::io::AssetSourceBuilder::new(|| Box::new(model_assets::SimAssetReader)),
    );

    // **Bevy's own diagnostics, in every build** — the three that cost a counter
    // and an atomic. They feed the Developer tools' Profiler tab
    // (`systems::dev_panel::profiler_tab`), which is the reason they are no
    // longer confined to a `cargo perf` run: an engine measurement that is only
    // available on a scripted flight is unavailable in exactly the situation
    // somebody wants it, which is "this device is slow right now".
    //
    // The two that are *not* here are absent for reasons of their own, and the
    // Profiler tab says so on screen rather than showing an empty table:
    // `SystemInformationDiagnosticsPlugin` needs `bevy/sysinfo_plugin` (a
    // build-time dependency, so it rides the `perf` feature below), and
    // `RenderDiagnosticsPlugin` reads GPU timestamps back on a task-pool thread
    // — a readback that panicked at startup on this project's Mesa/Intel host
    // (`frame_pacing`'s note), so it is behind the `CARTO_GPU_TIMINGS` switch.
    //
    // None of them asks anything new of the device: `WgpuSettings` defaults to
    // `Functionality` priority, under which `initialize_renderer` already
    // enables every feature the adapter reports.
    app.add_plugins((
        bevy::diagnostic::FrameTimeDiagnosticsPlugin::default(),
        bevy::diagnostic::EntityCountDiagnosticsPlugin::default(),
        bevy::render::diagnostic::MeshAllocatorDiagnosticPlugin,
    ));
    // Process CPU and memory, both as **percentages** (of one core, and of total
    // system memory). Its own docs warn the sample is not taken per frame and may
    // be stale, so it is a trend rather than a reading.
    #[cfg(feature = "perf")]
    app.add_plugins(bevy::diagnostic::SystemInformationDiagnosticsPlugin);
    if env_flag("CARTO_GPU_TIMINGS") {
        app.add_plugins(bevy::render::diagnostic::RenderDiagnosticsPlugin);
    }

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
        // Same reason for the placement: it orders a `RenderStartup` system after
        // one `RenderPlugin` registers, so the sub-app has to exist first.
        .add_plugins(GpuPreprocessingOverridePlugin)
        .add_plugins(EguiPlugin)
        // Renders the 3D world below the screen's resolution on a tier that asks
        // for it, and no-ops entirely on the tiers that do not. Must come after
        // `EguiPlugin`, which is what puts `egui_pass` after `upscaling` — the
        // ordering that keeps the interface at full resolution while the world
        // scales.
        .add_plugins(systems::render_scale::RenderScalePlugin)
        // Watches the render app's `PipelineCache` for pipelines that failed to
        // build, for the diagnostics report — a failed pipeline is otherwise a
        // silently skipped pass and one startup-time log line (its module header
        // has the Adreno case this was built for).
        .add_plugins(systems::pipeline_health::PipelineHealthPlugin)
        // The atmosphere is Bevy 0.19's built-in bevy_pbr Atmosphere (added by
        // PbrPlugin); it's attached as a component to the camera in setup_camera.
        .add_plugins(MaterialPlugin::<systems::data_layers::PinHeadMaterial>::default())
        .add_plugins(MaterialPlugin::<sky::StarFieldMaterial>::default())
        .add_plugins(MaterialPlugin::<sky::MoonMaterial>::default())
        .add_plugins(MaterialPlugin::<sky::HorizonHazeMaterial>::default())
        // The sky for adapters that cannot run the atmosphere above (its module
        // header has the whole story). The plugin is added unconditionally — the
        // shell is only spawned on the runs that need it.
        .add_plugins(MaterialPlugin::<systems::sky_dome::SkyDomeMaterial>::default())
        .add_plugins(MaterialPlugin::<sky::CloudMaterial>::default())
        .add_plugins(MaterialPlugin::<sky::GlobeCloudMaterial>::default())
        .add_plugins(MaterialPlugin::<sky::RainMaterial>::default())
        .add_plugins(MaterialPlugin::<systems::particles::ParticleMaterial>::default())
        .add_plugins(MaterialPlugin::<systems::water::WaterMaterial>::default())
        // The basemap and the ground surfaces, carrying the half-metre residual slope
        // the geometry is too coarse to hold (`systems::ground_detail`).
        .add_plugins(MaterialPlugin::<systems::ground_detail::GroundMaterial>::default())
        // The LoD2.2 buildings' walls, with the window pattern on them
        // (`systems::building_windows`).
        .add_plugins(MaterialPlugin::<systems::building_windows::BuildingMaterial>::default())
        // The far building tier's own material: the same `StandardMaterial`, plus a
        // vertex stage that takes the outermost ring down into the map rather than
        // ending the city at a cell edge (`systems::building_fade`).
        .add_plugins(MaterialPlugin::<systems::building_fade::FarBuildingMaterial>::default())
        .add_plugins(MaterialPlugin::<systems::grass::GrassMaterial>::default())
        // Roads and their casings. The ladder that decides which of two
        // overlapping ribbons is on top rides in a vertex attribute, so the road
        // groups need a pipeline that declares it (`systems::road_depth`).
        .add_plugins(MaterialPlugin::<systems::road_depth::RoadMaterial>::default())
        .add_plugins(MaterialPlugin::<systems::globe_ocean::GlobeSurfaceMaterial>::default())
        .init_resource::<GameState>()
        .init_resource::<AvatarState>()
        .init_resource::<resources::NetPredictionHealth>()
        .init_resource::<LocalAvatarSimId>()
        .init_resource::<CameraState>()
        .init_resource::<systems::touch::TouchGestures>()
        .init_resource::<systems::touch::UiClaimRegions>()
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
        // The dock's mark disc and the card it opens (`systems::ui`).
        .init_resource::<systems::ui::MainMenu>()
        // The place card a picked search result opens (`systems::place_card`).
        .init_resource::<systems::place_card::PlaceCard>()
        // Which screen of the developer panel is showing
        // (`systems::dev_panel`) — a resource so `--ui profiler` can name one.
        .init_resource::<systems::dev_panel::DevTools>()
        .init_resource::<systems::onscreen_keyboard::OnScreenKeyboard>()
        // What the on-screen navigation controls asked for this frame
        // (`systems::map_osd`); read by `free_camera::camera_controls`.
        .init_resource::<systems::map_osd::OsdInput>()
        // …and what an agent driving this client asked for (`systems::mcp`);
        // read beside it by `avatar::handle_avatar_movement`.
        .init_resource::<avatar::AgentWalk>()
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
        .init_resource::<AvatarChooserState>()
        .init_resource::<systems::user_store::RestoredCameraMode>()
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
        .init_resource::<resources::FrameStats>()
        .init_resource::<resources::ScreenshotState>()
        // **Bevy's own upload throttle stays off, on every tier, and that is a
        // correctness decision.** `RenderAssetBytesPerFrame` defers a mesh's
        // `RenderMesh` to a later frame, but bevy 0.19's `MeshAllocator` has
        // already written that mesh's vertex and index data in the frame it was
        // extracted — so the throttle never bounded the bytes it claims to, and a
        // mesh that is modified or retired while its `RenderMesh` waits is drawn
        // with the wrong geometry or not at all. On the handset, whose 4 MB
        // ceiling is the only one ever set, that was the ground cells missing
        // with the flat basemap showing metres below the roads, slabs across the
        // view and buildings standing where they are not (Deck, `CARTO_HANDSET=1`,
        // Europaplein, 2026-09-26: three throttled runs broken three different
        // ways, every unthrottled run right). The 2026-08-18 capture that went
        // "missing the city centre's buildings" with the budget on, and was
        // exempted from scripted runs as "partly uploaded", was this.
        //
        // The byte bound this was for is `stream_ledger`, which every streamer
        // asks before it *creates* a frame's assets — deferring there costs
        // latency and cannot desynchronise anything. `CARTO_UPLOAD_KB` and the
        // tier's `upload_bytes_per_frame` reach it through `quality::upload_ceiling`.
        .insert_resource(bevy::render::render_asset::RenderAssetBytesPerFrame { max_bytes: None })
        .init_resource::<systems::osm_buildings::OsmBuildings>()
        .init_resource::<systems::lod22::Lod22Buildings>()
        .init_resource::<systems::building_facts::BuildingFacts>()
        .init_resource::<systems::dikes::Dikes>()
        .init_resource::<systems::interiors::Interiors>()
        .init_resource::<systems::interiors::InteriorFloors>()
        .init_resource::<systems::tunnels::Tunnels>()
        .init_resource::<systems::terrain::RoadMask>()
        .init_resource::<systems::building_select::BuildingSelection>()
        .init_resource::<systems::picking::PrimClick>()
        .init_resource::<systems::street_detail::StreetDetail>()
        .init_resource::<systems::tall_structures::TallStructures>()
        .init_resource::<systems::collision::BuildingColliders>()
        .init_resource::<systems::height::HeightLayer>()
        .init_resource::<systems::terrain_tiles::TerrainTiles>()
        .init_resource::<systems::water_levels::WaterLevels>()
        .init_resource::<systems::terrain::TerrainField>()
        .insert_resource({
            // `--aerial` is the only way into that layer in a scripted run; an
            // interactive session ticks it in the Layers panel (or restores it
            // from `data/user.json`, which `apply_prefs` does after this). The
            // measured ground needs no such switch any more — it is always on.
            let mut layers = systems::data_layers::DataLayers::default();
            layers.aerial_enabled = cli.aerial;
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
        .init_resource::<systems::locate::LocateTracking>()
        .init_resource::<systems::route_ahead::RouteAhead>()
        .init_resource::<systems::bookmarks::BookmarksState>()
        .insert_resource(user_store)
        .init_resource::<systems::map_stream::MapStream>()
        .init_resource::<systems::map_stream::ReliefBacklog>()
        .init_resource::<systems::map_stream::BasemapHoles>()
        .init_resource::<systems::ground_detail::GroundDetail>()
        .init_resource::<systems::ground_detail::GridCellOrigin>()
        // The Settings ▸ Storage button's request, and what came of it.
        .init_resource::<systems::cache_reset::CacheReset>()
        .init_resource::<resources::MapAnchor>()
        .init_resource::<systems::map_geometry::MapSurfaces>()
        // Filled by the same streamer, cell for cell (`systems::street_labels`).
        .init_resource::<systems::world_places::WorldPlaces>()
        .init_resource::<systems::traffic::DriveNetwork>()
        .init_resource::<systems::interact::InteractState>()
        .init_resource::<systems::street_labels::StreetLabelStore>()
        .init_resource::<systems::map_geometry::DeckPlates>()
        .init_resource::<systems::street_labels::StreetLabelSigns>()
        // Before any ground or water material can prepare: they all bind the
        // cloud deck's buffer (`sky::CLOUD_FIELD_BUFFER`), which this inserts.
        .init_resource::<systems::sky::CloudField>()
        // Must come after the `WaterMaterial` plugin above: it mints the shared
        // handle out of `Assets<WaterMaterial>`.
        .init_resource::<systems::water::WaterSeaState>()
        .init_resource::<systems::vegetation::VegetationLod>()
        .init_resource::<systems::wet_surfaces::DrySurfaces>()
        .init_resource::<systems::grass::GrassAreas>()
        .init_resource::<systems::grass::GrassPatches>()
        .init_resource::<systems::street_furniture::FurnitureLod>()
        .init_resource::<systems::globe::GlobeState>()
        .init_resource::<systems::globe::GlobeSurfaceAlbedo>()
        .init_resource::<systems::globe_ocean::GlobeOceanGloss>()
        // Space background: the atmosphere skybox covers this everywhere in the local
        // view, but it's hidden from orbit (ADR-025), so the sky reads as black space.
        .insert_resource(ClearColor(Color::BLACK))
        // A render error halts the renderer and records why — never ends the app,
        // and never rebuilds it. See `handle_render_error`.
        .insert_resource(bevy::render::error_handler::RenderErrorHandler(
            handle_render_error,
        ))
        // 2048 rather than 4096: with sun + moon each casting 4 cascades, a night
        // frame renders 8 shadow maps — 2048² quarters the shadow-pass fill and
        // memory versus 4096² for a barely-perceptible near-shadow softness at this
        // scene's cascade distances.
        //
        // Halved below the desktop rung, which is not the same trade: 1024² is 4 MB
        // of depth per cascade rather than 16, on machines where GPU memory is the
        // same memory the module is living in.
        //
        // **`tier < Desktop`, not `is_phone()`.** It was the latter, which read as
        // "a handset gets the small one" right up until Android moved off the phone
        // rung — at which point every handset silently took a workstation's 2048²
        // across three cascades and two lights, in a change whose commit message
        // listed the fields it was moving and did not know about this one. A test
        // keyed on the *bottom* rung says nothing about the middle; the ladder is
        // ordered precisely so that a threshold can be written against it.
        .insert_resource(DirectionalLightShadowMap {
            size: if quality::budget().tier < quality::Tier::Desktop {
                1024
            } else {
                2048
            },
        });

    // Connection mode (ADR-016, and `systems::session` for the runtime half):
    //  - `--connect ADDR` (or `server` in the login file) → that simulator.
    //  - `--offline` or `--shot` → no sim, no auth, no sign-in screen.
    //  - otherwise → whatever `user.json` remembers, which on a fresh install is
    //    the offline map.
    //
    // None of it happens *here* any more: the mode is expressed as the session
    // driver's first request and applied on frame one, which is the same path the
    // sign-in screen uses at any later moment. A flag is an instruction for this
    // launch, so it wins over the remembered session and is not written back.
    app.init_resource::<AuthIdentity>();
    app.init_resource::<systems::chat::ChatState>();
    app.init_resource::<systems::private_chat::PrivateInbox>();
    app.init_resource::<systems::private_chat::PrivateChats>();
    app.init_resource::<systems::calls::CallInbox>();
    app.init_resource::<systems::calls::Calls>();
    app.init_resource::<systems::chat_overlay::ChatOverlay>();
    app.init_resource::<systems::chats::ChatsView>();
    app.init_resource::<systems::voice::VoiceState>();
    // Voice plays through the output device the soundscape already opened, as a
    // custom `Decodable` asset. Registering the type is what teaches bevy_audio
    // to build a rodio source from it; without this an `AudioPlayer<VoiceStream>`
    // is a component nothing looks at, and voice is silent with nothing logged.
    #[cfg(feature = "audio")]
    {
        use bevy::audio::AddAudioSource;
        app.add_audio_source::<systems::voice::VoiceStream>();
    }
    app.init_resource::<systems::script_editor::ScriptEditorState>();
    app.init_resource::<systems::model_library::ModelLibraryState>();
    app.init_resource::<systems::placeholder_models::PlaceholderModels>();
    app.init_resource::<resources::RightPanel>();
    app.init_resource::<systems::session::SessionState>();
    let screen = systems::login_screen::LoginScreen::from_prefs(&session_prefs);
    // The web build used to be excluded here outright — it was the Phase-1 viewer
    // with no transport to a sim at all. It has one now (a WebSocket carrier), so
    // it takes this path like any other build.
    //
    // A windowless run is "scripted" and stays offline — except `--mcp-headless`
    // told a server. That run is not a capture: it is an account an agent drives
    // (`systems::mcp::agent`), and signing in is the whole of how it gets an
    // avatar to walk and a voice to talk with.
    #[cfg(mcp)]
    let signs_in = cli.mcp_headless && cli.connect.is_some();
    #[cfg(not(mcp))]
    let signs_in = false;
    // What this launch signed in with, kept so an agent can sign back in after
    // signing out (`mcp::agent`, `sign_in`).
    #[cfg(mcp)]
    let mut launch_sign_in: Option<systems::session::SessionRequest> = None;
    if !cli.offline && (!scripted || signs_in) {
        use systems::session::{SessionKind, SessionRequest};
        let login = login::resolve(cli.login.as_deref());
        // `Some` = enter this; `None` = open onto the offline map, which is where
        // everybody starts. **Startup never raises the sign-in card** — that is
        // the app menu's, and only the app menu's (`systems::login_screen`). A
        // launch that has nothing to enter has no question either: offline is a
        // world, not a waiting room, and a modal in front of it on the first frame
        // is the loading-screen-pretending-to-be-a-login that this design exists to
        // avoid. What a *failed* entry owes the user is the reason, and that is in
        // the menu beside the entry that offers to try again.
        let request = match cli.connect.clone().or(login.server.clone()) {
            Some(server) => Some(SessionRequest::Remote {
                server,
                credential: login.credential,
                persist_token: true,
            }),
            None if !session_prefs.auto_enter => None,
            None => match session_prefs.kind {
                SessionKind::Offline => None,
                // A remembered server with no address is not a session.
                SessionKind::Remote if session_prefs.server.is_empty() => None,
                SessionKind::Remote => Some(SessionRequest::Remote {
                    server: session_prefs.server.clone(),
                    // Remembered sessions ride the persisted token. The fallback
                    // credential is this username with no password, which the
                    // server rejects — and the rejection is reported in the app
                    // menu beside "Sign in…", which is the honest outcome for a
                    // token that has expired without a modal over the map.
                    credential: cartopolis_core::AuthCredential::Password {
                        username: session_prefs.username.clone(),
                        password: String::new(),
                    },
                    persist_token: true,
                }),
            },
        };
        if let Some(request) = request {
            #[cfg(mcp)]
            if matches!(request, SessionRequest::Remote { .. }) {
                launch_sign_in = Some(request.clone());
            }
            app.world_mut()
                .resource_mut::<systems::session::SessionState>()
                .request(request);
        }
    }
    app.insert_resource(screen);

    // **The camera operator.** One state machine places a camera, waits for every
    // streamer to go quiet, quiesces the CPU pool so the GPU readback is not
    // queued behind a rasterise backlog, fires the shutter and measures what it
    // photographed. Three things drive it and none of them owns a copy: `--shot`
    // and `--shots` hand it a list and quit at the end, `--perf` measures a
    // window after each shutter, and `--mcp-listen` hands it one viewpoint at a
    // time while the app runs.
    if let Some(harness) = harness {
        // Fed at runtime: no `--ui` list to apply, and the holds must let go of
        // the camera between calls.
        let live = harness.live;
        let windowless = !harness.windowed;
        // No window at all: the camera renders into an offscreen image and
        // `HeadlessSurface` stands in for the `Window` egui lays out against.
        // A windowed MCP session has a real one and takes neither.
        if windowless {
            app.insert_resource(resources::HeadlessSurface {
                size: harness.size,
                scale: 1.0,
            })
            .add_systems(
                Startup,
                systems::shot_harness::setup_offscreen_target
                    .after(systems::free_camera::setup_camera),
            );
        }
        app.insert_resource(harness)
            .init_resource::<systems::shot_harness::ShotProgress>()
            .add_systems(
                Update,
                (
                    // Anywhere in Update is early enough: it only has to beat
                    // PostUpdate's `end_egui_frame`, which is what would otherwise
                    // reuse an older frame's tessellation for the capture.
                    //
                    // Gated, because in a live run this is a cost paid every frame
                    // for a capture that may be minutes away — and between calls
                    // the ordinary skip is what the client wants.
                    systems::shot_harness::keep_egui_fresh
                        .run_if(systems::shot_harness::shot_in_progress),
                    systems::shot_harness::place_camera.before(systems::geo_nav::apply_teleport),
                    systems::shot_harness::place_space_orbit
                        .after(systems::geo_nav::apply_teleport)
                        .before(systems::free_camera::camera_controls),
                    // Hold the scripted camera mode against the toggle that would
                    // drop an offline Follow to Fly, and before the movement system
                    // that reads the mode. **Gated**: a mode held every frame by an
                    // idle camera operator is a mode the person at the keyboard
                    // cannot change.
                    systems::shot_harness::hold_scripted_camera_mode
                        .after(systems::free_camera::camera_mode_toggle)
                        .before(avatar::handle_avatar_movement)
                        .run_if(systems::shot_harness::shot_in_progress),
                    // `--look-at`: aim at a place rather than at an angle. After
                    // the teleport has put the camera somewhere and before the
                    // camera system reads the rotation, and every frame, because
                    // the terrain under both ends lands a second after the anchor.
                    // Gated for the reason the mode hold is: an idle camera
                    // operator's camera belongs to the person at the keyboard.
                    systems::shot_harness::aim_scripted_camera
                        .after(systems::geo_nav::apply_teleport)
                        .before(systems::free_camera::camera_controls)
                        .run_if(systems::shot_harness::shot_in_progress),
                    // Ungated, and it has to be: it owns the advance past the last
                    // viewpoint — which is the edge that parks a live run and the
                    // one that ends a scripted one.
                    //
                    // After drive_screenshots so the `hide_ui` it clears each frame
                    // isn't cleared again before PostUpdate's egui suppression
                    // reads it.
                    systems::shot_harness::drive_capture
                        .after(systems::free_camera::camera_controls)
                        .after(systems::ui::drive_screenshots),
                ),
            );
        // The `--ui` family, which only a *scripted* run has: a live harness is
        // handed viewpoints, not presets, and `open_scripted_ui` keyed on the
        // shot index would close every panel the user had open each time an
        // editor took a picture.
        if !live {
            app.add_systems(
                Update,
                (
                    systems::shot_harness::open_scripted_ui,
                    // After the camera is placed, so the demo speaker `--ui chat`
                    // spawns stands where this shot's camera is actually looking.
                    systems::shot_harness::hold_scripted_chat
                        .after(systems::shot_harness::open_scripted_ui),
                    // After the camera is placed, for the same reason the chat demo
                    // is: the seeded prims stand where this shot is actually looking.
                    systems::shot_harness::hold_scripted_fire
                        .after(systems::shot_harness::open_scripted_ui),
                    // After the teleport for the reason its doc gives — the teleport
                    // clears the seat — and before the camera, so the shift it applies
                    // is framed on the same frame it is written.
                    systems::shot_harness::seat_scripted_avatar
                        .after(systems::geo_nav::apply_teleport)
                        .after(systems::shot_harness::hold_scripted_camera_mode)
                        .before(systems::free_camera::camera_controls),
                    // `--ui building`: select the building nearest the avatar, since a
                    // headless run has no cursor to click one with. After the teleport,
                    // which is what decides where "nearest" is measured from; the glass
                    // it asks for is built by `update_building_shells` on the next
                    // frame, which a settling capture has hundreds of.
                    systems::shot_harness::select_scripted_building
                        .after(systems::geo_nav::apply_teleport),
                    // `--ui locate`: a fix with no provider, so the device arrow can
                    // be photographed at all.
                    //
                    // **After** the tracking system, not before. That system reads
                    // the live watches every frame and writes what it finds — which
                    // in a scripted run is nothing, since no provider exists — so a
                    // seed seen first is a seed wiped before the overlay reads it.
                    // Running last is the whole reason `--ui locate` produces a
                    // picture rather than an empty map.
                    //
                    // …and **before the camera**, which is the other half of the
                    // same argument: with `--ui firstperson` the seeded attitude
                    // is what aims the view, and a rotation written after
                    // `camera_controls` has read it is one frame of lag on a
                    // capture that only ever takes one frame.
                    systems::shot_harness::track_scripted_device
                        .after(systems::geo_nav::apply_teleport)
                        .after(systems::locate::track_device_pose)
                        .before(systems::free_camera::camera_controls)
                        .before(systems::locate::render_locate_arrow),
                    // `--ui firstperson`: zoom the follow camera into the avatar's
                    // head. Before the device seeding above, which asks whether it
                    // is on, and before the camera that acts on both.
                    systems::shot_harness::zoom_scripted_first_person
                        .before(systems::shot_harness::track_scripted_device)
                        .before(systems::free_camera::camera_controls),
                ),
            );
        }
    }

    // The performance suite rides the harness above: same teleport, same settle
    // test, same shutter. All it adds is when to measure.
    if let Some(run) = perf_run {
        // The per-pass GPU timings, which the suite's report is half built on. They
        // are behind `CARTO_GPU_TIMINGS` for every other launch (see the plugin
        // block above) because the readback can take a driver down at startup — but
        // a `cargo perf` run is a measurement run by definition, so it opts itself
        // in. Guarded, because the switch may already have added it and bevy
        // answers a duplicate plugin with a panic.
        if !app.is_plugin_added::<bevy::render::diagnostic::RenderDiagnosticsPlugin>() {
            app.add_plugins(bevy::render::diagnostic::RenderDiagnosticsPlugin);
        }
        app.insert_resource(run)
            .init_resource::<systems::perf::PerfProgress>()
            .init_resource::<systems::perf::MeshChurn>()
            .add_systems(
                Update,
                (
                    systems::perf::count_mesh_churn,
                    // **Before `drive_capture`**, which is load-bearing: the
                    // capture path consumes its `saved` flag with a swap, so a
                    // system that needs to see the capture land has to look first.
                    systems::perf::drive_perf_window,
                )
                    .chain()
                    .before(systems::shot_harness::drive_capture),
            );
    }

    // **The editor's way in** (`systems::mcp`), on by default on the desktop and
    // switched from Settings ▸ General ▸ Editor access.
    //
    // Everything is registered here; whether a socket is actually open is
    // `mcp::reconcile_listener`'s, once a frame's worth of change detection. The
    // thing this enables is an editor attaching to a client that is *already
    // running*, and a launch flag cannot serve that: the moment somebody wants it
    // is the moment something is on screen.
    #[cfg(mcp)]
    if let Some(cfg) = mcp_config {
        // The token: the environment wins (a headless run has no screen to read
        // one off), then whatever this installation last minted, then a fresh
        // one. The fresh one is written back so Settings has something to show
        // and an editor's configuration keeps working across launches.
        //
        // **Written back only when something will save it.** A windowless run
        // takes `harness_active`'s exemption from the user store — no restore, no
        // autosave — so marking the store dirty there would be a promise nothing
        // keeps, and the next launch would mint a different token having appeared
        // to remember one. It logs the token instead.
        let token = match std::env::var("CARTO_MCP_TOKEN") {
            Ok(t) if !t.trim().is_empty() => t.trim().to_string(),
            _ => {
                let store = app.world().resource::<systems::user_store::UserStore>();
                match store.data.mcp_token.clone() {
                    Some(t) if !t.trim().is_empty() => t,
                    _ => {
                        let minted = systems::mcp::mint_token();
                        if !cfg.headless {
                            let mut store = app
                                .world_mut()
                                .resource_mut::<systems::user_store::UserStore>();
                            store.data.mcp_token = Some(minted.clone());
                            store.dirty = true;
                        }
                        minted
                    }
                }
            }
        };
        // A windowless run has no Settings panel to read the token off, so it is
        // logged — once, at startup, beside where it *will* serve.
        //
        // "will", and the wording matters: the socket is opened a frame later by
        // `reconcile_listener`, which is the only thing that knows whether the
        // port was free. A line here saying "serving" is a claim this code cannot
        // make, and the run that proves it is the second client on one port,
        // which logged "serving" and then "cannot serve" three lines apart.
        if cfg.headless {
            tracing::info!(listen = %cfg.listen, token = %token, "mcp: headless, will serve");
        }
        let auth = systems::mcp::McpAuth(std::sync::Arc::new(std::sync::RwLock::new(token)));
        let (tx, rx) = futures_channel::mpsc::unbounded();
        let headless = cfg.headless;
        // Whose chat the agent may read: the flag for this run, else what Settings
        // remembers, else nobody (`mcp::agent::Audience`).
        let audience = match &cli.mcp_allow {
            Some(list) => systems::mcp::agent::Audience {
                pinned: true,
                ..systems::mcp::agent::Audience::parse(list)
            },
            None => systems::mcp::agent::Audience::parse(
                app.world()
                    .resource::<systems::user_store::UserStore>()
                    .data
                    .mcp_allow
                    .as_deref()
                    .unwrap_or(""),
            ),
        };
        tracing::info!(can_hear = %audience.describe(), "mcp: agent allow-list");
        app.insert_resource(audience)
            .insert_resource(match launch_sign_in {
                Some(request) => systems::mcp::agent::AgentState::signing_in_with(request),
                None => systems::mcp::agent::AgentState::default(),
            })
            .add_systems(
                Update,
                (
                    // After the pump, which is what drains the agent's requests
                    // off the server thread's queue.
                    systems::mcp::agent::serve.after(systems::mcp::pump),
                    // A walk is a travel wish, so it is aimed before the movement
                    // system reads it — and after the mode toggle, which would
                    // otherwise take back the Follow camera a walk needs.
                    systems::mcp::agent::steer
                        .after(systems::free_camera::camera_mode_toggle)
                        .before(avatar::handle_avatar_movement),
                ),
            );
        app.insert_resource(cfg.clone())
            .insert_resource(auth)
            .insert_resource(systems::mcp::JobSender(tx))
            .init_resource::<systems::mcp::McpTrouble>()
            .insert_resource(systems::mcp::McpBridge::new(rx, cfg.listen))
            .add_systems(
                Update,
                (
                    // Opens or closes the socket to match the switch. Before the
                    // pump, so a listener enabled this frame can be answered from
                    // this frame.
                    systems::mcp::reconcile_listener,
                    // Before the camera operator, so a viewpoint that arrives
                    // this frame is placed this frame rather than waiting one.
                    systems::mcp::pump.before(systems::shot_harness::place_camera),
                    // After it, so a capture that lands this frame is answered
                    // this frame. The two halves are ordered around
                    // `drive_capture` for the same reason and in opposite
                    // directions.
                    systems::mcp::land_captures.after(systems::shot_harness::drive_capture),
                )
                    .chain(),
            );
        if !headless {
            app.add_systems(
                Update,
                systems::mcp::overlay::render_mcp_activity
                    .after(systems::place_pins::render_place_pins),
            );
        }
    }

    // Per-window OS icon via winit (Windows + Linux/X11; a no-op on Wayland and
    // macOS, which use the desktop file / .app bundle respectively). See
    // `systems::window_icon`.
    app.add_systems(Update, systems::window_icon::set_window_icon);
    // `CARTO_DBG_HIDE`: a bisecting switch that hides whole layers by name.
    app.add_systems(PostUpdate, systems::map_geometry::debug_hide_layers);

    // Swap bevy_pbr's atmosphere functions module for the patched copy as soon
    // as its async load lands (Adreno 840 black-sky hardening — see
    // `systems::atmosphere_patch`). Its own registration: it acts on an asset,
    // not on this frame's entities, so it orders against nothing.
    app.add_systems(Update, systems::atmosphere_patch::patch_atmosphere_shader);

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
            // Which rung this run took, said where a phone can read it —
            // `quality::init` answers the question before `LogPlugin` exists, so
            // its own line has never reached logcat. See `quality::report_budget`.
            quality::report_budget,
            // Whether the APK's own `classes.dex` is loaded and callable from the
            // thread `android_main` runs on. Reported for the same reason the
            // budget above is: it is a startup fact with no other instrument on a
            // phone, and every Java-shaped feature planned on top of the dex lane
            // — the background-connection service first — is dead if it is `NOT
            // reachable`.
            #[cfg(target_os = "android")]
            platform::android::report_dex,
            systems::free_camera::setup_camera,
            // The car pool. One mesh, a handful of materials and a fixed number
            // of entities, all built once — after which this layer uploads
            // nothing (`systems::traffic`).
            //
            // **After the camera**, which is what spawns the `BigSpace` root the
            // cars are parented to (`free_camera::setup_camera` calls
            // `spawn_big_space`). Registered in this tuple with no ordering it
            // ran first, found no root, returned silently, and the pool was never
            // created — no cars and nothing in the log. `setup_sky_dome` below
            // carries the same `.after` for its own reason.
            systems::traffic::setup_traffic.after(systems::free_camera::setup_camera),
            // After the camera, which is where this run's `SkyMode` is decided —
            // it can't be known before the render device exists.
            systems::sky_dome::setup_sky_dome.after(systems::free_camera::setup_camera),
            sky::setup_sky,
            sky::setup_stars,
            sky::setup_moon,
            sky::setup_horizon_haze,
            sky::setup_clouds,
            sky::setup_globe_clouds,
            sky::setup_rain,
            // The shared quad pool every particle emitter draws. No ordering: it
            // only builds a mesh, and `sync_particle_emitters` waits for the
            // resource rather than assuming it.
            systems::particles::setup_particle_pool,
        ),
    )
    .add_systems(
        Update,
        (
            network::apply_network_snapshot,
            network::create_egui_texture_handles.after(network::apply_network_snapshot),
            // After the network system has sorted this frame's private-chat
            // traffic into the inbox it drains.
            systems::private_chat::drive_private_chats.after(network::apply_network_snapshot),
            // After the chats: a call reads the chat's epoch, which a commit
            // applied there this frame has just moved.
            systems::calls::drive_calls.after(systems::private_chat::drive_private_chats),
            // A chat's "+ → Send this view": the camera's view, as a point, queued
            // for the sealing above to pick up on its next pass.
            systems::geo_nav::answer_view_requests
                .before(systems::private_chat::drive_private_chats),
            // ADR-029: derive geo-anchored prims' GridCell/Transform from the
            // current OSM anchor (and re-project on re-anchor) before meshing.
            systems::prim_geo::place_geo_prims
                .after(network::apply_network_snapshot)
                .before(rendering::spawn_prims),
            rendering::spawn_prims.after(network::apply_network_snapshot),
            // After `spawn_prims`, for the same reason the particle emitters are:
            // this hangs a child off the prim, and a `Children` list written in
            // the same flush as the parent's own components is how a scene ends up
            // on an entity that is still being assembled. It also *removes*
            // `PrimMesh` when a model is taken off, which hands the prim back to
            // `spawn_prims` on the following frame.
            model_assets::attach_prim_models
                .after(network::apply_network_snapshot)
                .after(rendering::spawn_prims),
            rendering::rebuild_prim_meshes.after(rendering::spawn_prims),
            rendering::refresh_prim_textures
                .after(rendering::spawn_prims)
                .after(network::apply_network_snapshot),
            // After the snapshot has written the prims' `particles` field, and
            // after `spawn_prims`, so the emitter child is parented to an entity
            // that already has its mesh — a `Children` list rewritten in the same
            // flush as the parent's own components is how an emitter ends up on a
            // prim that is still being assembled.
            systems::particles::sync_particle_emitters
                .after(network::apply_network_snapshot)
                .after(rendering::spawn_prims),
        ),
    )
    .add_systems(
        Update,
        (
            // Nested, and only because Bevy's system tuples top out at twenty
            // elements: this one was at the limit and the next addition to it will
            // need the same treatment. Nothing about the grouping is meaningful.
            (
                // Drains finished model-library requests and the file picker. Not
                // in the panel, because both outlive it being open: an upload
                // started and then the panel closed must still land.
                systems::model_library::poll_model_library,
            ),
            systems::osm_buildings::update_buildings,
            // Nested as one element to stay under Bevy's per-tuple system limit.
            // `update_building_facts` takes no ordering edges: it spawns nothing
            // and is read by panels and readouts, never by a streamer mid-frame.
            (
                // After the field's ask, for the hold — see `update_map_surfaces`.
                systems::lod22::update_lod22.after(systems::terrain::update_terrain_field),
                systems::building_facts::update_building_facts,
                systems::surveyed::prefetch_bgt_coverage,
                systems::crops::prefetch_brp_coverage,
                systems::terrain_tiles::prefetch_terrain_coverage,
                systems::interiors::update_interiors,
                // Which building the avatar is standing in, and the glass that
                // answers. Chained after the rooms because `track_indoors` reads
                // the set `update_interiors` has just published — a frame's lag
                // there is a frame in which the collider still seals a doorway the
                // avatar has already walked through.
                (
                    systems::building_select::track_indoors,
                    systems::building_select::clear_selection_on_reanchor,
                    systems::building_select::update_building_shells,
                )
                    .chain()
                    .after(systems::interiors::update_interiors),
                // The flood defences: stream, measure the crest, hand the names
                // to the label placer. Chained because each step reads what the
                // one before it wrote, and the whole run is a few hundred lines
                // of polyline — the alternative is a name that appears a frame
                // after the crest it stands on.
                (
                    systems::dikes::update_dikes,
                    systems::dikes::measure_dike_crests,
                    systems::dikes::feed_dike_labels,
                )
                    .chain(),
            ),
            systems::data_layers::update_data_layers,
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
                // building, window, 3DBAG mesh and stream tile — so it
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
                    .after(systems::map_stream::update_tile_occlusion),
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
                    .after(systems::lod22::update_lod22),
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
                // The same field's heights as atlas tiles, where atlas has cut the
                // area (`terrain_tiles`); asked for by the same system.
                systems::terrain_tiles::poll_terrain_tiles,
                systems::height::probe_anchor_ground
                    .after(systems::map_stream::apply_reanchor)
                    .before(systems::height::poll_height_layer),
                // Terrain relief: one measured box per anchor, baked into the field
                // every ground-attached system samples. Asks *before* the queue is
                // polled so a fetch it starts is dispatched the same frame.
                // The measured water levels the two terrain bakes carve with. Before
                // them, so a cell that lands is folded into the same frame's bake
                // rather than costing an extra re-bake of the whole field.
                systems::water_levels::update_water_levels
                    .after(systems::map_stream::apply_reanchor),
                systems::terrain::update_terrain_field
                    .after(systems::map_stream::apply_reanchor)
                    .after(systems::water_levels::update_water_levels)
                    .after(systems::terrain::apply_coarse_terrain)
                    .before(systems::height::poll_height_layer),
                // Install a field bake that landed. Before the ask, so a box that
                // finished this frame is the one the ask compares against.
                systems::terrain::apply_coarse_terrain.after(systems::map_stream::apply_reanchor),
                // The sub-metre half: the tier's slope into a shared texture, and the
                // render-space uniform that tells the ground materials where it is.
                // After `apply_fine_terrain`, which is what hands the texels over.
                systems::ground_detail::track_grid_origin,
                systems::ground_detail::receive_ground_detail
                    .after(systems::terrain::apply_fine_terrain),
                systems::ground_detail::refresh_ground_detail
                    .after(systems::ground_detail::receive_ground_detail)
                    .after(systems::ground_detail::track_grid_origin),
                // The half-metre tier under the camera. After the coarse field, whose
                // datum it borrows and whose answer it fades into at its rim.
                systems::terrain::update_fine_terrain
                    .after(systems::terrain::update_terrain_field)
                    .after(systems::terrain::apply_coarse_terrain)
                    .after(systems::water_levels::update_water_levels)
                    .after(systems::terrain::apply_fine_terrain)
                    .before(systems::height::poll_height_layer),
                // Install a bake that landed. Before the ask, so a box that finished
                // this frame is the one the ask compares against.
                systems::terrain::apply_fine_terrain.after(systems::map_stream::apply_reanchor),
            ),
            systems::map_stream::init_map_stream,
            // Re-anchor requests (from teleports / globe descent) drop the old
            // tiles before streaming resumes at the new origin (ADR-025).
            // Nested as one element to stay under Bevy's per-tuple system limit.
            (
                systems::map_stream::apply_reanchor
                    .after(systems::geo_nav::apply_teleport)
                    .before(systems::map_stream::update_map_stream),
                // A basemap source change retears the clipmap the same way, so it
                // has to land before the streamer refills it — and before
                // `apply_stream_textures`, which asks the same resolved answer
                // which source a finished tile came from.
                systems::map_stream::apply_aerial_source
                    .after(systems::map_stream::init_map_stream)
                    .before(systems::map_stream::update_map_stream),
                // …and so does a cache reset, for the same reason and through the
                // same teardown: the tiles it drops must be gone before the
                // streamer decides what to refill.
                systems::cache_reset::apply_cache_reset
                    .after(systems::map_stream::init_map_stream)
                    .before(systems::map_stream::update_map_stream),
            ),
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
            )
                // All three read the loaded document, and the autosave below
                // overwrites it — on the first frame, unordered against them
                // until this edge existed. See `user_store::Restore`.
                .in_set(systems::user_store::Restore),
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
                // Never before a restore has read the document this overwrites:
                // its first deadline is time zero, so without this it wrote the
                // launch defaults over the saved sky and layers on frame one and
                // the restores then read back its own output.
                .after(systems::user_store::Restore)
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
                // Which clipmap tiles are buried under finer ones, and can be left
                // out of the draw list. After the textures land, so a tile that
                // became ready this frame counts this frame; before the visibility
                // sweep, which is the only thing allowed to act on it.
                systems::map_stream::update_tile_occlusion
                    .after(systems::map_stream::apply_stream_textures),
                // Each tile's own ring, so the shader can soften it into the level
                // beneath rather than meeting it at a step. After the textures, for
                // the reason the occlusion pass is: a tile that gained its material
                // this frame gets its ring in the same frame instead of being drawn
                // once at full sharpness and corrected afterwards.
                systems::map_stream::refresh_tile_lod_blend
                    .after(systems::map_stream::apply_stream_textures)
                    .after(systems::ground_detail::track_grid_origin),
                // Stand the basemap on the measured ground. After the spawn pass,
                // so a tile that appeared this frame is lifted in the same frame
                // it is placed rather than showing flat for one.
                systems::map_stream::apply_terrain_to_tiles
                    .after(systems::map_stream::update_map_stream)
                    .after(systems::terrain::update_terrain_field),
                // Vector surfaces as geometry (ADR-035 option B): water and landuse
                // meshes over the textured tiles. No-op on a raster tile source.
                // After the field's ask, so a new anchor's hold on the cells
                // (`TerrainField::holds_builds`) is in place before any dispatch.
                systems::map_geometry::update_map_surfaces
                    .after(systems::map_stream::update_map_stream)
                    .after(systems::terrain::update_terrain_field),
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
                // Stand up the models this server draws its street objects with,
                // a bounded number per frame. After the surface build, which is
                // what queues them and what owns the cell they are attached to —
                // running first would spawn into cells that do not exist yet.
                systems::placeholder_models::spawn_placeholder_models
                    .after(systems::map_geometry::update_map_surfaces),
                // …and ask the server what those are, once per session.
                systems::placeholder_models::poll_placeholders,
                // Near-field grass: slide the patch pool along its lattice, keep
                // the coverage mask under it and drive the sway from the live
                // wind. After the surface build, which is what fills the per-cell
                // grass areas it plants into — a frame's lag there is a frame of
                // bare ground on a cell that just streamed in.
                systems::grass::update_grass.after(systems::map_geometry::update_map_surfaces),
                // Wet the hard ground when it rains. Same ordering and the same
                // reason as the two ramps above: it repaints materials that the
                // surface build is what creates, so running it first would ramp
                // whatever existed last frame and leave a cell that has just
                // landed dry until the rain next moved.
                systems::wet_surfaces::apply_wet_surfaces
                    .after(systems::map_geometry::update_map_surfaces),
                // Ambient occlusion and the light shafts, reconciled with what
                // Settings ▸ Graphics currently asks for. After the camera, which
                // is the entity it writes and which measures the capability half
                // of the answer.
                systems::render_features::apply_render_features
                    .after(systems::free_camera::camera_controls),
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
            // Before `attach_avatar_to_bigspace`, so an avatar that appears when
            // a session comes up is in the grid on the frame it is spawned
            // rather than the one after.
            sync_local_avatar,
            attach_avatar_to_bigspace.after(sync_local_avatar),
        ),
    )
    // The Overpass street-detail streamer, and the small systems riding it:
    // night glow + the point-light pool, the signal phase cycle and the
    // altitude fade. Its own block: the map tuple above is at the limit.
    .add_systems(
        Update,
        (
            systems::street_detail::update_street_detail,
            systems::street_detail::update_lamp_glow
                .after(systems::street_detail::update_street_detail),
            systems::street_detail::update_lamp_lights
                .after(systems::street_detail::update_street_detail),
            systems::street_detail::update_signal_cycle
                .after(systems::street_detail::update_street_detail),
            systems::street_detail::update_prop_fade
                .after(systems::street_detail::update_street_detail),
            systems::tall_structures::update_tall_structures,
            systems::tall_structures::spin_rotors
                .after(systems::tall_structures::update_tall_structures),
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
            // Keep the avatar out of buildings: block walking into them, and
            // eject one that lands inside (teleport / a building streaming over
            // it). Runs after movement and before the camera follows the
            // corrected position.
            systems::collision::eject_avatar_from_buildings
                .after(avatar::handle_avatar_movement)
                .before(systems::free_camera::camera_controls),
            network::send_network_intent.after(avatar::handle_avatar_movement),
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
            avatar::update_avatar_model_pose
                .after(avatar::handle_avatar_movement)
                .after(avatar::spawn_avatar),
            avatar::update_remote_avatar_animation.after(avatar::smooth_remote_avatars),
            avatar::clear_remote_emote_edges.after(avatar::update_remote_avatar_animation),
            avatar::sync_avatar_animations.after(avatar::update_avatar_animation),
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
            // The tunnel walls, on the same terms and for the same reason: a tunnel
            // that streamed in this frame is a floor on the frame it appears, and
            // a floor with no walls is a way to walk out into solid ground.
            systems::collision::sync_bore_colliders.before(avatar::handle_avatar_movement),
            // What the avatar is standing next to, and the nearest house number.
            systems::interact::track_interaction_target.after(avatar::handle_avatar_movement),
            // Before `send_network_intent`, which is what puts a triggered emote on
            // the wire this frame rather than the next.
            systems::interact::handle_interaction_input.before(network::send_network_intent),
        ),
    )
    // Phase 3b'': the camera mode a sign-in lands in. Its own `add_systems`
    // because the camera tuple in Phase 3 is at Bevy's 20-element ceiling.
    // Before the toggle so the mode it writes is the one the toggle's
    // "Follow is not on offer" guard sees this frame.
    .add_systems(
        Update,
        systems::free_camera::follow_on_sign_in.before(systems::free_camera::camera_mode_toggle),
    );
    // Phase 3b': voice capture. Its own `add_systems` rather than a line in the
    // tuple above, which is at Bevy's 20-element limit for one call — and two
    // systems rather than one because the *other* writer of `talk_held` is the
    // chat panel's Hold-to-talk button, the only way to talk on a phone, so the
    // key reader must not be the thing that clears a flag it does not own.
    // Capture reads the panel's press from the frame before: 16 ms of latency on
    // a control a finger is holding, against threading a frame-order constraint
    // through the whole panel chain.
    //
    // Not behind the `audio` feature. `AudioInput::start` fails cleanly in a
    // build without it and the failure is shown, which is the honest shape — the
    // alternative is a Voice control that is simply absent with no explanation.
    let app = app.add_systems(
        Update,
        (
            systems::voice::read_talk_key,
            systems::voice::drive_voice_capture
                .after(systems::voice::read_talk_key)
                .after(network::apply_network_snapshot),
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
                // The arrival whoosh. After the teleport that causes it, so a
                // jump is heard on the frame it lands rather than the next one.
                systems::soundscape::announce_teleports.after(systems::geo_nav::apply_teleport),
                // Turn cues. Here rather than with the rest of driver mode because
                // the whole soundscape is behind this feature, and because a cue is
                // a function of the distance the driver tick just computed — it has
                // to see this frame's band, not last frame's.
                //
                // This is the one part of navigation you can use without looking at
                // the screen, which on a phone in a car is the only way it gets
                // used at all.
                systems::driver::play_driver_cues.after(systems::driver::drive_driver_mode),
                // Other people's voices. Here because it spawns `AudioPlayer`s and
                // is therefore as feature-bound as the rest of this block — the
                // *receive* half is not: frames are decoded into a ring by the
                // network drain whatever this build can play, so a no-audio build
                // still reports who is talking rather than pretending nobody is.
                systems::voice::update_voice_playback.after(network::apply_network_snapshot),
                // A call's voices, through the same output.
                systems::calls::play_calls.after(systems::calls::drive_calls),
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
                // The buildings behind the prims. **After** `prim_picking`, which
                // is what gets to say whether the click was already spent — see
                // `picking::PrimClick`.
                systems::building_select::pick_building.after(systems::picking::prim_picking),
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
        // Phase 7: the top menu bar and the floating map chrome
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
                // The place card — a floating `Area` above the dock, so it stays
                // out of the panel chain too. **Before the panels**: taking a
                // pick out of `GeoNavState::pending_card` is also what closes
                // the Search panel, and doing it before the chain runs means the
                // panel never draws a frame over the card it just opened.
                systems::place_card::render_place_card.before(systems::ui::render_main_menu),
                // The building card, on the same terms and for the same reasons:
                // a floating `Area` above the dock, outside the Phase 7b chain,
                // suppressed by whatever owns the bottom of the screen. **After**
                // the place card, which is anchored to the same centre-bottom —
                // they are never both up (one comes from a search pick, the other
                // from a click on the map) and the order makes that definite
                // rather than a race if they ever are.
                systems::building_card::render_building_card
                    .after(systems::place_card::render_place_card)
                    .before(systems::ui::render_main_menu),
                systems::dev_panel::toggle_dev_panel_shortcut,
                // Nested to stay under the per-tuple system limit: the driver runs after
                // the panel so a button click is seen the same frame it's captured.
                (
                    systems::dev_panel::render_dev_panel,
                    systems::ui::drive_screenshots.after(systems::dev_panel::render_dev_panel),
                ),
                // The right-hand panel chain lives in Phase 7b, declared once with
                // `.chain()`. A panel added here rather than there is NOT in it.
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
                    sky::apply_sun_mode
                        .before(sky::advance_day_cycle)
                        .after(systems::transit_live::update_world_clock),
                    sky::advance_day_cycle.before(sky::apply_day_night_cycle),
                ),
                sky::apply_day_night_cycle.after(systems::dev_panel::render_dev_panel),
                sky::manage_lights.after(systems::dev_panel::render_dev_panel),
                sky::apply_distance_fog.after(systems::dev_panel::render_dev_panel),
                sky::update_window_glow,
                // Ambient traffic: one transform write per car per frame, and no
                // asset touched after startup (`systems::traffic`).
                systems::traffic::drive_traffic,
                // The other lane's windows. `update_window_glow` above only
                // reaches the OSM buildings, which are never drawn over 3DBAG
                // coverage — see `systems::building_windows`.
                systems::building_windows::update_building_windows,
                // Keep the far tier's height ramp centred on the camera. Every
                // frame, affordable for the reason the line above is: five
                // materials, not one per cell.
                systems::building_fade::update_building_fade
                    .after(systems::ground_detail::track_grid_origin),
                // Track the camera after it has been positioned this frame.
                // Nested to stay under the per-tuple system limit, as elsewhere in
                // this file. Both track the camera and both must run after it has
                // moved this frame.
                (
                    sky::update_star_sky.after(systems::free_camera::camera_controls),
                    // The fog volume the light shafts are marched through: placed
                    // from the camera's transform, so a frame's lag here is a
                    // frame of shafts centred where the viewer used to be.
                    systems::light_shafts::follow_camera
                        .after(systems::free_camera::camera_controls),
                ),
                sky::update_moon.after(systems::free_camera::camera_controls),
                (
                    sky::update_horizon_haze.after(systems::free_camera::camera_controls),
                    systems::sky_dome::update_sky_dome.after(systems::free_camera::camera_controls),
                    sky::update_clouds.after(systems::free_camera::camera_controls),
                    sky::update_globe_clouds.after(systems::free_camera::camera_controls),
                    sky::update_rain.after(systems::free_camera::camera_controls),
                ),
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
        // its width. `locate::render_locate_arrow` reads the same leftover space and
        // is ordered after it (Phase 8) for the same reason.
        .add_systems(
            Update,
            (
                systems::ui::render_main_menu,
                systems::geo_nav::render_search_panel,
                systems::data_layers::render_data_layers_panel,
                systems::bookmarks::render_bookmarks_panel,
                // The route planner draws a floating bottom sheet, not a docked
                // panel (`navigation::SheetDetent`), so nothing about the shared
                // root `Ui` puts it here. It stays in the chain because it owns
                // two text fields and Phase 7d's keyboard anchors on the *end* of
                // this chain to find them — registered outside it, the endpoint
                // fields would be undrawn when the keys go looking for their rect.
                systems::navigation::render_route_sheet,
                // The messenger: the chat list and one chat's Chat view
                // (`systems::chats`). Its World view and the Chats | World switch
                // are drawn over the map, after the composer below.
                systems::chats::render_chats_panel,
                systems::script_editor::render_script_panel,
                systems::model_library::render_model_library_panel,
                systems::ui::render_avatar_chooser,
                systems::settings::render_settings_panel,
                systems::settings::render_about_panel,
                // Before the pins and after every panel, for the same reason both of
                // those hold: it culls against the root `Ui`'s leftover space, which is
                // only the map once the side panels have taken their width.
                systems::interact::render_interaction_prompt,
                // Chat over the map (`systems::chat_overlay`). In the chain
                // because the bubbles cull against the root `Ui`'s leftover
                // space exactly as the pins do; near the end because that space
                // is only the map once every panel has taken its width. The
                // composer draws last of the three so it is on top of the
                // transcript it expands, and — being ordered before Phase 7d —
                // its field is registered in time for the on-screen keyboard to
                // find it.
                systems::chat_overlay::render_chat_bubbles
                    .after(systems::free_camera::camera_controls),
                systems::chat_overlay::render_chat_transcript,
                systems::chat_overlay::render_chat_composer,
                // A chat's World view has a text field, so it is ordered before
                // the on-screen keyboard (Phase 7d) for the composer's reason.
                systems::chats::render_chat_world,
                systems::chats::render_chats_switch,
                // Also after the camera has moved, so pins project onto this frame's view.
                systems::place_pins::render_place_pins.after(systems::free_camera::camera_controls),
                // The device arrow: where the phone is, in the views where the
                // avatar isn't standing in for it. Same placement rules as the
                // pins above — it reads the root `Ui`'s leftover space to find
                // the map area, so it must run after every panel, and after the
                // camera so it projects onto this frame's view. It paints and
                // never senses, so unlike a pin it cannot take a tap and needs no
                // ordering against the dock beyond that.
                systems::locate::render_locate_arrow
                    .after(systems::free_camera::camera_controls)
                    .after(systems::place_pins::render_place_pins),
            )
                .chain(),
        )
        // The call bar: a call ringing, or the one you are in. Its own
        // `Foreground` area like the sharing notice it sits under, so it is not
        // in the panel chain; after the pins only so it is drawn over them.
        .add_systems(
            Update,
            systems::calls::render_call_bar.after(systems::place_pins::render_place_pins),
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
                // After the driver, which is what adds and removes `OnlineSession`
                // — half of the want this reconciles. Ordered so that leaving a
                // session takes the service down on the same frame rather than
                // leaving a "connected" notification over a client that isn't.
                systems::background::apply_background_connection
                    .after(systems::session::drive_session),
                // After the driver marks a session as come back, and after the
                // avatar it brings back is in the grid, so the resume lands on
                // the same frame the avatar appears.
                systems::session::keep_avatar_across_reconnect
                    .after(systems::session::drive_session)
                    .after(attach_avatar_to_bigspace)
                    .before(systems::geo_nav::apply_teleport),
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
                // Escape's single dismissal ladder. It reads state every panel
                // has already written this frame, so it runs after the Phase 7b
                // chain — which this tuple does, being registered after it.
                systems::dismiss::handle_escape,
                // The clock every chat line is aged against. Its own system
                // rather than a line at the top of one of the drawing ones,
                // which return early on half a dozen conditions — a clock that
                // stops whenever nothing is drawn makes every line on screen
                // immortal. A frame's lag against the network apply is
                // deliberate and irrelevant: the shortest thing measured off it
                // is a 1.5 s fade.
                systems::chat_overlay::advance_chat_clock,
                frame_pacing::apply_screen_wakelock,
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
                    .after(systems::navigation::render_route_sheet),
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
                    .after(systems::navigation::render_route_sheet)
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
                // The first-person twin of the line above: take the avatar's model
                // out of shot while the camera is inside its head. After the camera,
                // which is what decides that, and after the driver arrow, which owns
                // the avatar's visibility whenever it is drawing over it — the two
                // never both want it, and ordering them makes that visible rather
                // than merely true.
                systems::free_camera::update_first_person_avatar
                    .after(systems::free_camera::camera_controls)
                    .after(systems::driver::update_driver_arrow),
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
                // Device tracking (`systems::locate`) — the locate button's own
                // mode, and the smaller sibling of the drive above. After the
                // drive tick, which is what declares driver mode active and so
                // decides which of the two owns the avatar this frame; before the
                // camera, which is what consumes the heading it writes.
                systems::locate::track_device_pose
                    .after(systems::driver::drive_driver_mode)
                    .after(avatar::handle_avatar_movement)
                    .before(systems::free_camera::camera_controls),
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
                frame_pacing::frame_time_watchdog,
                // Gated at registration, not inside the body. The body's own
                // `is_phone()` return came *after* the system was in the schedule
                // with a seven-component unfiltered `Query<&Mesh3d, …>`, which Bevy
                // re-matches against every new archetype — and archetypes are
                // created continuously here as tiles, buildings and surfaces
                // stream. A system that runs is a system that pays for its query;
                // the cheapest diagnostic is one the phone never registers.
                log_render_cost.run_if(|| !quality::is_phone() || env_flag("CARTO_RENDER_COST")),
                frame_pacing::frame_stats,
            ),
        )
        // In `Last`, not `Update`, and the placement is load-bearing — see
        // `measure_uploads` for both halves of why (a mesh's data is gone after
        // extraction, and this is where the frame's assets are all present and
        // all still readable). Registered on every tier, unlike `log_render_cost`
        // above: it holds two message readers and two `Res<Assets<_>>`, matches no
        // archetypes at all, and the phone is the platform whose bursts it names.
        .add_systems(Last, systems::upload_meter::measure_uploads)
        .init_resource::<systems::upload_meter::UploadMeter>()
        // The meter's other consumer: the ring that streamers reach with, steered
        // by the residency the meter measures rather than fixed by the tier alone
        // (`systems::load_control`). After `measure_uploads` in the same schedule,
        // so it reads this frame's total and not last frame's; off under the shot
        // harness, where a ring that changed size between captures would make two
        // runs of `cargo shots` differ for a reason no image could show.
        .add_systems(
            Last,
            systems::load_control::observe
                .after(systems::upload_meter::measure_uploads)
                .run_if(bevy::prelude::not(systems::shot_harness::harness_active)),
        )
        // The meter's enforcement half: fund the frame's shared finalisation
        // budget before any streamer runs, so what they collectively hand the
        // GPU per frame is bounded once rather than per streamer — see
        // `systems::stream_ledger` for why the per-streamer caps alone still
        // queued 27 MB frames.
        .add_systems(First, systems::stream_ledger::open_frame)
        // Drawn over everything until the first tiles are up. Mirrors the
        // splash in index.html, which covers the wasm load this cannot.
        .init_resource::<systems::splash::BootSplash>()
        .add_systems(Update, systems::splash::boot_splash)
        // Drop egui geometry for the screenshot frame, after end_egui_frame has written it.
        .add_systems(
            PostUpdate,
            systems::ui::suppress_egui_for_screenshot.after(systems::egui_manager::end_egui_frame),
        )
        // Says whether the avatar is actually on screen, on every platform.
        //
        // `Last` and not `Update`, because the thing it reads is `ViewVisibility`,
        // which Bevy writes in `PostUpdate`. From `Update` it would report the
        // *previous* frame — harmless for a settled `--shot`, which is why
        // `drive_capture` gets away with it, and misleading for a live log where the
        // interesting moment is the frame the count changes.
        .add_systems(Last, systems::avatar::log_avatar_draw_counts)
        // Same schedule and the same reason: the report quotes `ViewVisibility`,
        // which `PostUpdate` writes, so from `Update` it would describe the frame
        // before the tap.
        .init_resource::<systems::diagnostics::DiagnosticsRequest>()
        .add_systems(Last, systems::diagnostics::share_diagnostics)
        // Four systems, one chain, and each position is load-bearing.
        //
        // `present_rate` runs **first**, because both of the next two consume what
        // it decides: it is the single resolution of "what rate do we want",
        // handed to the compositor on Android and slept to by the limiter
        // elsewhere. Resolved in `Last` rather than `First` so the idle test can
        // read this frame's finished work — the bytes uploaded and the camera's
        // settled transform — instead of the previous frame's.
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
        .init_resource::<frame_pacing::PresentationRate>()
        .add_systems(
            Last,
            (
                frame_pacing::present_rate,
                frame_pacing::report_frame_to_scheduler,
                // Not under the shot harness. The limiter sleeps to a deadline, so
                // with it running `ShotMetrics::frame_ms` can never read below the
                // tier's cap however cheap the frame got — an optimisation worth
                // measuring is exactly one the limiter would hide. A scripted
                // capture has no display to pace to and no eyes on it anyway.
                frame_pacing::frame_limiter.run_if(not(systems::shot_harness::harness_active)),
                frame_pacing::watchdog_beat,
            )
                .chain(),
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

/// **The avatar belongs to a session, and offline there is not one.**
///
/// It used to be spawned once at startup and never despawned, so `--offline` —
/// which is the default, and every scripted run — put a body on the map with no
/// sim behind it: nothing owned its position, nobody else could see it, and the
/// Follow camera that is its whole point was refused
/// ([`free_camera::follow_available`]). Offline is the streamed map and a camera.
///
/// The one exception is the shot harness, and it is not a loophole: a scripted
/// run is always offline, so `--shot-mode avatar`, `--ui sit` and `--ui driver`
/// would otherwise be photographing an empty street. The harness already states
/// what each shot is of, so [`ShotHarness::wants_avatar`] reads that rather than
/// taking a flag of its own — a capture cannot ask for an avatar without saying
/// what it wants one for.
///
/// Written as a sync rather than a pair of one-shot systems because the session
/// is switched at runtime in both directions (`systems::session`), and `connect`
/// and `teardown` are already an exact pair without a third thing to keep in
/// step.
fn sync_local_avatar(
    mut commands: Commands,
    session: Res<systems::session::SessionState>,
    harness: Option<Res<systems::shot_harness::ShotHarness>>,
    avatars: Query<Entity, With<Avatar>>,
) {
    let wanted = session.is_online() || harness.is_some_and(|h| h.wants_avatar());
    let present = !avatars.is_empty();
    if wanted == present {
        return;
    }
    if wanted {
        // Scale is applied by `avatar::spawn_avatar` from the resolved model,
        // before the mesh instantiates — so spawn at the identity scale here.
        // `attach_avatar_to_bigspace` gives it a `GridCell` on the same frame.
        commands.spawn((Avatar, Transform::from_translation(Vec3::ZERO)));
        return;
    }
    // `try_despawn`, and recursive: the model holders are children, and
    // `session::teardown` retires this same entity from the other side with no
    // ordering between the two.
    for e in avatars.iter() {
        commands.entity(e).try_despawn();
    }
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
/// The persisted half of [`env_flag`], seeded once by [`init_debug_switches`].
///
/// A process-wide `OnceLock` and not a `Resource` for the same reason
/// `quality::budget()` is one: the earliest reader runs before the `App` exists
/// (`CARTO_NO_DUAL_SOURCE` is consulted while `DefaultPlugins` is still being
/// built), so there is no world to hold it.
static DEBUG_SWITCHES: std::sync::OnceLock<resources::DebugSwitches> = std::sync::OnceLock::new();

/// Seed the persisted switches. Called once at the top of `run_client`, beside
/// `quality::init` and for the same reason: everything downstream expects it
/// settled, and the first reader is earlier than it looks.
///
/// Silently keeps the first value if called twice, as `quality::init` does — a
/// second answer would mean two different runs' switches inside one process.
pub fn init_debug_switches(switches: resources::DebugSwitches) {
    let _ = DEBUG_SWITCHES.set(switches);
}

/// **Two sources, and the explicit one wins.** Since 2026-08-23 these switches are
/// also a persisted preference (`resources::DebugSwitches`, Settings ▸ Graphics ▸
/// Developer), because on Android the source below is `adb`, i.e. a cable and a
/// second machine — and the Adreno sky bug spent four days with its own instrument
/// built and unreachable by the person holding the device. An env var or property
/// that is *present* still decides, in both directions: it is an instruction for
/// this launch and is never written back, exactly as `--connect` is against the
/// remembered session. The preference is only what a launch falls back to.
///
/// Resolving it here rather than at the call sites is the point: every switch in
/// the codebase became reachable from the phone without touching one of them.
pub fn env_flag(name: &str) -> bool {
    resolve_flag(
        env_flag_explicit(name),
        DEBUG_SWITCHES.get().and_then(|switches| switches.get(name)),
    )
}

/// The resolution [`env_flag`] performs, as a function so it can be tested.
///
/// Its own function for the reason `frame_pacing::resolve_frame_rate` records:
/// the rung test there stayed green about a number the call site had stopped
/// passing on, because what was asserted was the input and not the resolution. A
/// `OnceLock` and a process environment are both untestable in a unit test; this
/// is the part with a rule in it.
fn resolve_flag(explicit: Option<bool>, preference: Option<bool>) -> bool {
    explicit.or(preference).unwrap_or(false)
}

/// The platform's own answer: `Some` when the switch is *present*, `None` when it
/// is absent and [`env_flag`] should fall back to the preference.
///
/// The three-way answer is what makes `CARTO_X=0` mean "off" rather than "not
/// set" — without it, a property set to `0` to overrule a preference would fall
/// straight through to that preference and appear to do nothing, which is the
/// worst possible behaviour for a debugging switch.
fn env_flag_explicit(name: &str) -> Option<bool> {
    #[cfg(target_os = "android")]
    {
        // Android has no environment either, and for a harder reason than the
        // browser's: the zygote hands a NativeActivity `app_process`'s own
        // environment and there is no shell on the device to add to it. So the
        // same switches come from system properties instead — see
        // `platform::android::system_property`, which is also where the
        // `adb shell setprop` recipe lives.
        //
        // An *empty* property is the absence of one: `getprop` cannot tell them
        // apart, and `setprop … ""` is how a property is cleared.
        return platform::android::system_property(name)
            .filter(|v| !v.is_empty())
            .map(|v| v != "0");
    }
    #[cfg(all(not(target_arch = "wasm32"), not(target_os = "android")))]
    {
        // An empty environment variable is idiomatically unset, and was already
        // read as false here; it is now read as *absent*, which is the same thing
        // one layer up unless a preference says otherwise.
        std::env::var_os(name)
            .filter(|v| !v.is_empty())
            .map(|v| v != "0")
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
/// `None` when the key is absent from the query string — see
/// [`env_flag_explicit`] for why absence and `=0` must be different answers.
#[cfg(target_arch = "wasm32")]
fn query_flag(name: &str) -> Option<bool> {
    query_value(name).map(|v| v.is_empty() || (v != "0" && !v.eq_ignore_ascii_case("false")))
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

#[cfg(test)]
mod flag_resolution_tests {
    use super::resolve_flag;
    use crate::resources::DebugSwitches;

    /// A switch nobody has expressed an opinion about is off, which is what every
    /// call site assumed when the environment was the only source.
    #[test]
    fn an_unmentioned_switch_is_off() {
        assert!(!resolve_flag(None, None));
    }

    /// The preference is what a launch falls back to — the whole point of the
    /// feature, since on Android there is nothing else to fall back to.
    #[test]
    fn the_preference_decides_when_nothing_is_set() {
        assert!(resolve_flag(None, Some(true)));
        assert!(!resolve_flag(None, Some(false)));
    }

    /// **An explicit setting wins in both directions, and the second half is the
    /// one worth a test.** `CARTO_X=1` beating a preference is obvious;
    /// `CARTO_X=0` beating a preference that is *on* is what stops `setprop
    /// debug.carto.no_haze 0` from silently doing nothing, which for a
    /// debugging switch is the worst behaviour available.
    #[test]
    fn an_explicit_setting_overrides_the_preference() {
        assert!(resolve_flag(Some(true), Some(false)));
        assert!(!resolve_flag(Some(false), Some(true)));
    }

    /// Every switch the Settings section offers must survive the property-name
    /// mapping, because on the platform this exists for that mapping is the only
    /// way anyone else can reproduce what the user set.
    #[test]
    fn every_settings_switch_is_reachable_as_a_property() {
        for (name, _, _) in DebugSwitches::SWITCHES {
            let mapped = super::android_property_name(name);
            assert!(
                mapped.starts_with("debug.carto."),
                "{name} maps to {mapped}"
            );
            assert!(mapped.len() <= 31, "{mapped} is {} bytes", mapped.len());
        }
    }
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
            "CARTO_NO_CLOUDS",
            "CARTO_NO_SHADOWS",
            "CARTO_NO_SPACE_RAYMARCH",
            "CARTO_NO_DUAL_SOURCE",
            "CARTO_FPS_LOG",
            "CARTO_FPS_CAP",
            "CARTO_QUALITY",
            "CARTO_RENDER_SCALE",
            "CARTO_RENDER_COST",
            "CARTO_TILE_URL",
            "CARTO_PLACE",
            "CARTO_UPLOAD_KB",
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
    egui: Res<systems::egui_manager::EguiManager>,
    mut dismissable: systems::dismiss::Dismissable,
    mut exit: MessageWriter<AppExit>,
) {
    // `BrowserBack` is what winit maps Android's back key to. Desktop keyboards
    // can carry the same key, and doing the same thing there is right anyway.
    if !keys.just_pressed(KeyCode::BrowserBack) {
        return;
    }
    // **Back is Escape plus one rung.** This used to walk its own three-step
    // ladder — keyboard, driver, panel — while Escape walked a different and
    // mostly broken one, so the app's two dismissal gestures disagreed about
    // what dismissing meant: Back unwound panels and Escape could not, Escape
    // left driver mode from under an open dialog and Back would not. There is
    // one ladder now (`systems::dismiss`), and the *only* difference between the
    // two gestures is this last rung, which is why it is the only thing written
    // here.
    let text_focused = egui.ctx.text_edit_focused();
    if dismissable.dismiss_innermost(text_focused).is_none() {
        exit.write(AppExit::Success);
    }
}

/// The periodic half of [`systems::render_census`] — the same walk the shot
/// harness makes per capture, on a five-second timer.
///
/// Both the walk and the phone gate are deliberate. Resolving every `Mesh3d`
/// handle into `Assets<Mesh>` is a real hitch on a dense scene, so it is throttled
/// rather than run per frame; and the gate is a `run_if` at registration rather
/// than an early return here, because a return does not save the query — Bevy
/// still matches the census's seven-component `Query` against every archetype the
/// streamers create.
///
/// The phone *does* get it on request, which is the part that changed. Gating it
/// off entirely meant the numbers the phone budget is a set of predictions about
/// (`docs/notes/wasm-performance.md`) were the ones the phone could never report,
/// and a scripted capture cannot stand in — Android has no CLI. `CARTO_RENDER_COST`
/// goes through `env_flag`, so on a device it is `adb shell setprop
/// debug.carto.render_cost 1` against the shipped APK, no rebuild.
fn log_render_cost(
    time: Res<Time<Real>>,
    mut next: Local<f32>,
    census: systems::render_census::RenderCensus,
) {
    *next -= time.delta_secs();
    if *next > 0.0 {
        return;
    }
    *next = 5.0;
    let c = census.counts();
    tracing::info!(
        draws = c.draws,
        ktris_total = c.tris_total / 1000,
        ktris_shadow = c.tris_shadow / 1000,
        shadow_casters = c.shadow_casters,
        ktris_buildings = c.tris_buildings / 1000,
        ktris_lod22 = c.tris_lod22 / 1000,
        ktris_tiles = c.tris_tiles / 1000,
        ktris_globe = c.tris_globe / 1000,
        "render cost",
    );
}
