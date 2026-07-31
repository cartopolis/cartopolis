use bevy::asset::AssetPlugin;
#[cfg(not(target_arch = "wasm32"))]
use bevy::log::LogPlugin;
use bevy::camera::Exposure;
use bevy::light::light_consts::lux::RAW_SUNLIGHT;
use bevy::light::{CascadeShadowConfigBuilder, DirectionalLightShadowMap};
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
mod login;
mod platform;
mod simd_baseline;
mod resources;
#[cfg(feature = "solo")]
mod solo;
mod systems;
mod utils;

use components::Avatar;
use resources::{
    AiAssistantState, AiConfig, AuthIdentity, AvatarChooserState, AvatarState, CameraState,
    ConnectAddr, ConnectAuth,
    ContextMenuState, DevPanelState, EditDialogState, GameState, LocalAvatarSimId,
    MarqueeState, MouseState, OsmTileUrlTemplate, PrimTextureCache, StatsState, TextureLibrary,
};
use big_space::prelude::{BigSpace, GridCell};
use systems::egui_manager::EguiPlugin;
use systems::*;
use utils::sun::sun_direction;

#[derive(Parser, Debug)]
#[command(name = "viberfox")]
struct Cli {
    /// Connect to an external `vibe_sim` instance (TCP, postcard messages).
    /// Omit for solo mode (an in-process sim); see `--offline`.
    #[arg(long)]
    connect: Option<String>,

    /// Run without any sim: local SQLite, single user, no auth. This is the old
    /// default and what `--shot` uses. Otherwise the default is solo mode.
    #[arg(long)]
    offline: bool,

    /// Credentials for the online handshake, so you don't log in each run: a path
    /// to a TOML login file, or inline `user:pass`. Defaults to `data/dev-login.toml`
    /// if present, else env `VIBE_USER`/`VIBE_PASSWORD`, else the solo `dev` account.
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
        // cost for nothing. `VIBERFOX_FORCE_CPU_CLUSTERING=1` restores it anywhere.
        let force = cfg!(not(target_arch = "wasm32"))
            || crate::env_flag("VIBERFOX_FORCE_CPU_CLUSTERING");
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

/// Bring up the in-process solo sim, returning its loopback address. `None` when
/// the `solo` feature is disabled or startup failed (the caller then runs offline).
#[cfg(feature = "solo")]
fn start_solo_sim() -> Option<String> {
    match solo::start() {
        Ok(addr) => Some(addr),
        Err(e) => {
            tracing::error!("solo sim failed to start: {e:#}");
            None
        }
    }
}

#[cfg(not(feature = "solo"))]
fn start_solo_sim() -> Option<String> {
    None
}

fn main() {
    dotenvy::dotenv().ok();

    // Start the CPU worker pool before anything streams (ADR-033). A no-op
    // natively — `AsyncComputeTaskPool` is already a thread pool — and on the web
    // it spawns the Web Workers that keep tile decoding off the render thread.
    // One core is left for the main thread, which still renders and fetches.
    platform::workers::start(
        std::thread::available_parallelism()
            .map(|n| n.get().saturating_sub(1))
            .unwrap_or(4)
            .max(1),
    );

    let cli = Cli::parse();

    // Native: our own `tracing_subscriber` (env-filtered fmt to stderr). On wasm we
    // don't init it — its default timer calls `SystemTime::now`, which panics on
    // wasm, and its writer targets stdout; Bevy's `LogPlugin` (kept enabled on wasm
    // below) routes logs/panics to the browser console instead. ADR-033.
    #[cfg(not(target_arch = "wasm32"))]
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                // wgpu/naga at `warn` so GPU validation + device-loss errors
                // surface (they're the real cause of the zoom-out crash; the
                // panics only show the aftermath). Override with RUST_LOG.
                .unwrap_or_else(|_| {
                    tracing_subscriber::EnvFilter::new(
                        "off,viberfox=info,wgpu=warn,wgpu_core=warn,wgpu_hal=warn,naga=warn",
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
    // Native: assets live at a repo-relative path resolved from the manifest dir.
    // Wasm: `CARGO_MANIFEST_DIR` is a build-machine absolute path meaningless in a
    // browser — Bevy fetches assets over HTTP relative to the served page, so use a
    // plain `assets` URL (trunk copies `assets/` alongside the wasm). ADR-033.
    #[cfg(not(target_arch = "wasm32"))]
    let asset_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../assets")
        .display()
        .to_string();
    #[cfg(target_arch = "wasm32")]
    let asset_dir = "assets".to_string();
    let plugins = DefaultPlugins.build();
    // Native uses the custom `tracing_subscriber` above, so drop Bevy's LogPlugin
    // there. On wasm keep it: it wires logs + panics to the browser console and does
    // not call `SystemTime::now` (which panics on wasm). ADR-033.
    #[cfg(not(target_arch = "wasm32"))]
    let plugins = plugins.disable::<LogPlugin>();
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
        // `dev-bevy`, seconds for the streamer to go quiet — 4/4 → these widths
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
                    max_threads: 12,
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
                title: "Viberfox".into(),
                // Stable app-id: Wayland `app_id` / X11 `WM_CLASS`. The compositor
                // keys the taskbar/titlebar icon off this, matching the
                // `viberfox.desktop` entry that `systems::desktop_integration`
                // self-installs at startup.
                name: Some("viberfox".into()),
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
                present_mode: bevy::window::PresentMode::AutoNoVsync,
                // On the web the canvas otherwise keeps the fixed `resolution`
                // above forever — 1280x720 pinned in the corner of whatever size
                // the browser window actually is, with dead space around it, and
                // no response to resizing. This tracks the parent element instead.
                //
                // The field's documented hazard is a parent sized *by its
                // children*, which feeds back into the canvas growing every
                // resize. Ours is `<body>`, given an explicit `height: 100%` in
                // index.html, so its size comes from the viewport and never from
                // the canvas inside it.
                //
                // `resolution` still applies natively, and is the initial size on
                // the web until the first fit.
                #[cfg(target_arch = "wasm32")]
                fit_canvas_to_parent: true,
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

    // Headless (`--shot`): no window, and therefore no winit event loop to pump
    // the app — `ScheduleRunnerPlugin` drives the frames instead, as fast as the
    // GPU allows, until the harness writes its `AppExit`. wgpu needs no surface
    // to render into an image, so the whole run happens off the desktop: nothing
    // pops up, nothing can occlude (and blacken) a capture, and it works over SSH
    // or in CI with no compositor at all.
    let default_plugins = match shot_size {
        Some(_) => default_plugins
            .disable::<bevy::winit::WinitPlugin>()
            .add(bevy::app::ScheduleRunnerPlugin::run_loop(
                std::time::Duration::ZERO,
            )),
        None => default_plugins,
    };

    app.add_plugins(default_plugins)
    .add_plugins(big_space::prelude::BigSpaceDefaultPlugins)
    // Added after DefaultPlugins so its `finish` runs after PbrPlugin's (which
    // creates GlobalClusterSettings) — see the plugin's doc comment.
    .add_plugins(ForceCpuClusteringPlugin)
    .add_plugins(EguiPlugin)
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
    .init_resource::<systems::tile_loader::TileCache>()
    // A scripted run never sees the server handshake that normally supplies the
    // tile template, and the built-in fallback is the raster OSM basemap — which
    // the route planner cannot solve against. A preset manifest may therefore pin
    // its own source; `VIBE_TILE_URL` still wins over it (see `shot_presets`).
    .insert_resource(match harness.as_ref().and_then(|h| h.tile_url.clone()) {
        Some(url) if resources::tile_url_override().is_none() => {
            OsmTileUrlTemplate(std::sync::Arc::new(std::sync::Mutex::new(url)))
        }
        _ => OsmTileUrlTemplate::default(),
    })
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
    .insert_resource(systems::user_store::UserStore::load())
    .init_resource::<systems::map_stream::MapStream>()
    .init_resource::<systems::map_geometry::MapSurfaces>()
    // Filled by the same streamer, cell for cell (`systems::street_labels`).
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
    // 2048 rather than 4096: with sun + moon each casting 4 cascades, a night
    // frame renders 8 shadow maps — 2048² quarters the shadow-pass fill and
    // memory versus 4096² for a barely-perceptible near-shadow softness at this
    // scene's cascade distances.
    .insert_resource(DirectionalLightShadowMap { size: 2048 });

    // Connection mode (ADR-016):
    //  - `--connect ADDR` (or `server` in the login file) → external sim.
    //  - `--offline` or `--shot` → no sim, local SQLite, no auth.
    //  - otherwise (default) → solo mode: bring up an in-process sim on loopback
    //    and connect to it via the same authenticated path an external sim uses.
    app.init_resource::<AuthIdentity>();
    app.init_resource::<systems::chat::ChatState>();
    app.init_resource::<resources::RightPanel>();
    let offline = cli.offline || scripted;
    if !offline {
        let login = login::resolve(cli.login.as_deref());
        let external = cli.connect.or(login.server);
        app.insert_resource(ConnectAuth {
            credential: login.credential,
            persist_token: external.is_some(),
        });
        if let Some(addr) = external {
            app.insert_resource(ConnectAddr(addr));
        } else {
            match start_solo_sim() {
                Some(addr) => {
                    tracing::info!(%addr, "solo mode: in-process sim");
                    app.insert_resource(ConnectAddr(addr));
                }
                None => tracing::info!("solo mode unavailable; running offline"),
            }
        }
    }

    // Scripted capture mode (`--shot` / `--shots`): the harness owns the camera
    // and ends the run, and the user store stays out of it (no restore, no
    // autosave). It was already built and validated above.
    if let Some(harness) = harness {
        app.insert_resource(resources::HeadlessSurface { size: harness.size, scale: 1.0 })
            .insert_resource(harness)
            .init_resource::<systems::shot_harness::ShotProgress>()
            .add_systems(
                Startup,
                systems::shot_harness::setup_offscreen_target
                    .after(systems::free_camera::setup_camera),
            )
            .add_systems(
                Update,
                (
                    systems::shot_harness::open_scripted_ui,
                    systems::shot_harness::place_camera
                        .before(systems::geo_nav::apply_teleport),
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

    app.add_systems(
        Startup,
        (
            network::spawn_network_thread.run_if(has_connect_addr),
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
            network::create_egui_texture_handles
                .after(network::apply_network_snapshot),
            rendering::spawn_regions
                .after(network::apply_network_snapshot),
            // ADR-029: derive geo-anchored prims' GridCell/Transform from the
            // current OSM anchor (and re-project on re-anchor) before meshing.
            systems::prim_geo::place_geo_prims
                .after(network::apply_network_snapshot)
                .before(rendering::spawn_prims),
            rendering::spawn_prims
                .after(network::apply_network_snapshot),
            rendering::rebuild_prim_meshes
                .after(rendering::spawn_prims),
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
                    .before(systems::data_layers::apply_feature_toggles),
                // After the spawners, so new entities get this frame's toggle state.
                systems::data_layers::apply_feature_toggles
                    .after(systems::osm_buildings::update_buildings)
                    .after(systems::map_stream::update_map_stream)
                    .after(rendering::spawn_regions),
            ),
            systems::data_layers::handle_poi_click
                .after(systems::data_layers::update_data_layers),
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
                systems::weather::poll_weather
                    .after(systems::map_stream::apply_reanchor),
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
            ),
            systems::geo_nav::apply_teleport
                .after(systems::geo_nav::drive_geocode)
                .after(systems::user_store::restore_start_location)
                .before(systems::free_camera::camera_controls),
            systems::user_store::autosave_user_store
                .run_if(not(systems::shot_harness::harness_active)),
            systems::bookmarks::poll_bookmark_addresses,
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
                systems::geo_nav::apply_camera_reanchor
                    .before(systems::geo_nav::apply_teleport),
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
            systems::globe::update_globe_visibility
                .after(systems::free_camera::camera_controls),
            systems::globe::update_globe_albedo
                .after(systems::free_camera::camera_controls),
            // Track the atmosphere's planet centre against the floating-origin
            // camera cell (correct sky altitude in every tier — see its doc).
            systems::globe::update_atmosphere_origin
                .after(systems::free_camera::camera_controls),
            avatar::respawn_avatar_on_appearance_change.before(avatar::spawn_avatar),
            avatar::spawn_avatar,
            avatar::update_avatar_animation.after(avatar::handle_avatar_movement),
            // The airborne pose the clip set doesn't have: lean the model into
            // flight. After `spawn_avatar` so a holder that appeared this frame
            // (first spawn, appearance change) is leant on the frame it exists.
            avatar::update_avatar_flight_pose
                .after(avatar::handle_avatar_movement)
                .after(avatar::spawn_avatar),
            avatar::update_remote_avatar_animation.after(avatar::tick_remote_avatar_motion_hint),
            avatar::sync_avatar_animations.after(avatar::update_avatar_animation),
            systems::debug::debug_region_entities.after(rendering::spawn_regions),
        ),
    )
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
            // Auto-hiding top address bar (floating overlay, reserves no layout
            // space); ordered before the side panels for deterministic input.
            (
                systems::ui::render_menu_bar.before(systems::ai_assistant::render_ai_panel),
                // The bottom dock — a floating `Area` of round panel toggles, so
                // it reserves no root-`Ui` space and stays out of the panel chain
                // below. After the address bar only so input order is
                // deterministic within a frame.
                systems::dock::render_dock.after(systems::ui::render_menu_bar),
            ),
            systems::ui::toggle_ai_panel_shortcut,
            systems::ui::toggle_dev_panel_shortcut,
            // Nested to stay under the per-tuple system limit: the driver runs after
            // the panel so a button click is seen the same frame it's captured.
            (
                systems::ui::render_dev_panel,
                systems::ui::drive_screenshots.after(systems::ui::render_dev_panel),
                systems::ui::render_avatar_chooser.after(systems::ui::render_menu_bar),
            ),
            systems::ui::render_stats_overlay.after(systems::ui::render_menu_bar),
            // The right-hand panels draw into the shared root `Ui`, so their call
            // order fixes their right-to-left placement. Chain them (AI → Layers →
            // Bookmarks) so the order is deterministic — otherwise Bevy runs these
            // systems in a varying order each frame and the panels swap/flicker.
            systems::data_layers::render_data_layers_panel
                .after(systems::ui::render_menu_bar)
                .after(systems::ai_assistant::render_ai_panel),
            (
                systems::hud::render_hud.after(systems::ui::render_menu_bar),
                systems::chat::render_chat_panel.after(systems::ui::render_menu_bar),
            ),
            systems::bookmarks::render_bookmarks_panel
                .after(systems::ui::render_menu_bar)
                .after(systems::data_layers::render_data_layers_panel),
            systems::data_layers::render_poi_popup
                .after(systems::ui::render_menu_bar)
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
            systems::ai_assistant::render_ai_panel,
            systems::ai_assistant::poll_ai_response
                .after(systems::ai_assistant::render_ai_panel),
        ),
    )
    // Phase 8: Route planner (ADR-037). Its own `add_systems` call because the
    // Phase 7 tuple is already at Bevy's 20-system-per-tuple limit.
    .add_systems(
        Update,
        (
            // Draws into the same root `Ui` as the other right-hand panels, so its
            // call order fixes its placement — keep it last in that chain.
            systems::navigation::render_route_panel
                .after(systems::bookmarks::render_bookmarks_panel),
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
            systems::place_pins::render_place_pins
                .after(systems::navigation::render_route_panel)
                .after(systems::chat::render_chat_panel)
                // Tail of the panel chain — the avatar chooser is a root-`Ui`
                // panel too, and a pin placed before it took its width sees a map
                // area that still includes it.
                .after(systems::ui::render_avatar_chooser)
                .after(systems::free_camera::camera_controls),
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
            systems::route_ahead::drive_route_ahead
                .after(systems::driver::drive_driver_mode),
            systems::driver::render_driver_hud
                .after(systems::ui::render_menu_bar)
                .after(systems::route_ahead::drive_route_ahead),
            // The compass/locate column, which driver mode repurposes.
            systems::hud::render_map_controls
                .after(systems::ui::render_menu_bar)
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
    .add_systems(Update, (frame_time_watchdog, log_render_cost, frame_stats))
    // Drawn over everything until the first tiles are up. Mirrors the
    // splash in index.html, which covers the wasm load this cannot.
    .init_resource::<systems::splash::BootSplash>()
    .add_systems(Update, systems::splash::boot_splash)
    // Drop egui geometry for the screenshot frame, after end_egui_frame has written it.
    .add_systems(
        PostUpdate,
        systems::ui::suppress_egui_for_screenshot
            .after(systems::egui_manager::end_egui_frame),
    )
    .add_systems(Last, frame_limiter);

    app.run();
}

fn has_connect_addr(addr: Option<Res<ConnectAddr>>) -> bool {
    addr.is_some()
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
/// the device, so they're chosen once at launch. `VIBERFOX_NO_ATMOSPHERE=1`,
/// `VIBERFOX_NO_ENVMAP=1`, `VIBERFOX_NO_SHADOWS=1`.
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
/// `?VIBERFOX_NO_ATMOSPHERE=1`, or the tidier `?no_atmosphere` (the `VIBERFOX_`
/// prefix and the case are both optional in a URL, where there is no environment
/// to collide with).
pub fn env_flag(name: &str) -> bool {
    #[cfg(not(target_arch = "wasm32"))]
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
/// naming rules, as [`env_flag`]. `VIBERFOX_PLACE="53.2, 6.5, 120m"` on desktop,
/// `?place=53.2,6.5,120m` in the browser.
pub fn env_value(name: &str) -> Option<String> {
    #[cfg(not(target_arch = "wasm32"))]
    {
        std::env::var(name).ok().filter(|v| !v.is_empty())
    }
    #[cfg(target_arch = "wasm32")]
    {
        query_value(name).filter(|v| !v.is_empty())
    }
}

/// Look `name` up in `window.location.search`, accepting the `VIBERFOX_`-prefixed
/// spelling and the bare one, in either case.
#[cfg(target_arch = "wasm32")]
fn query_value(name: &str) -> Option<String> {
    let search = web_sys::window().and_then(|w| w.location().search().ok())?;
    let params = web_sys::UrlSearchParams::new_with_str(&search).ok()?;
    let short = name.strip_prefix("VIBERFOX_").unwrap_or(name);
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
    let (sun_color, illuminance) = sun_light(dev.time_of_day, dev.sun_declination_rad, dev.fog_density);
    commands.spawn((
        DirectionalLight {
            color: sun_color,
            illuminance,
            shadow_maps_enabled: dev.shadows_enabled && !env_flag("VIBERFOX_NO_SHADOWS"),
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
            shadow_maps_enabled: dev.shadows_enabled && !env_flag("VIBERFOX_NO_SHADOWS"),
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
    let hue = Vec3::ONE.lerp(Vec3::new(t.x / peak, t.y / peak, t.z / peak), SUN_COLOR_SATURATION);
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
    mesh.insert_attribute(
        Mesh::ATTRIBUTE_NORMAL,
        vec![[0.0, 0.0, 1.0]; 4],
    );
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
        (With<systems::free_camera::FreeCamera>, Without<Moon>),
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
            * utils::sun::moon_sunlight_direction(dev.time_of_day, dev.moon_age_days, dev.sun_declination_rad);
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
    dev.moon_age_days =
        (dev.moon_age_days + dh / 24.0).rem_euclid(utils::sun::LUNAR_CYCLE_DAYS);
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
/// Cap the presentation rate (default 60 fps; `VIBERFOX_FPS_CAP=0` disables).
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
fn frame_limiter(mut deadline: Local<Option<std::time::Instant>>, mut period: Local<Option<Duration>>) {
    // Native only: a `thread::sleep`-based FPS cap. The browser paces frames itself
    // (requestAnimationFrame), and `Instant::now()`/`thread::sleep` would panic on
    // wasm, so this is a no-op there.
    #[cfg(target_arch = "wasm32")]
    let _ = (&mut deadline, &mut period);
    #[cfg(not(target_arch = "wasm32"))]
    {
        let period = *period.get_or_insert_with(|| {
            let fps: f64 = std::env::var("VIBERFOX_FPS_CAP")
                .ok()
                .and_then(|v| v.parse().ok())
                .unwrap_or(60.0);
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

/// Opt-in per-second frame stats (`VIBERFOX_FPS_LOG=1`): sustained FPS, worst
/// frame this second, and how much the frame time varies (jitter). Motion that
/// reads as "stutter" is usually low or jittery FPS, not the movement code — this
/// tells them apart. Debug builds run Bevy at `opt-level=0`; use `--profile
/// dev-bevy` for a playable framerate (see CLAUDE.md).
fn frame_stats(
    time: Res<Time<Real>>,
    // (elapsed_s, frames, worst_ms, sum_ms, sum_ms_sq)
    mut acc: Local<(f32, u32, f32, f32, f32)>,
    mut enabled: Local<Option<bool>>,
) {
    // Through `env_flag`, not `std::env` directly, so `?fps_log` reaches it on the
    // web — where a number for "how slow is slow" is harder to come by than
    // anywhere else, there being no external profiler to point at the tab.
    if !*enabled.get_or_insert_with(|| env_flag("VIBERFOX_FPS_LOG")) {
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
    globe_meshes: Query<(), With<systems::globe::GlobeMesh>>,
    meshes: Res<Assets<Mesh>>,
    images: Res<Assets<Image>>,
    materials: Res<Assets<StandardMaterial>>,
    globe: Res<systems::globe::GlobeState>,
    camera: Query<(&GridCell, &Transform), With<systems::free_camera::FreeCamera>>,
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
fn log_render_cost(
    time: Res<Time<Real>>,
    mut next: Local<f32>,
    meshes: Res<Assets<Mesh>>,
    q: Query<(
        &Mesh3d,
        &bevy::camera::visibility::ViewVisibility,
        Has<bevy::light::NotShadowCaster>,
        Has<systems::osm_buildings::OsmBuildingMesh>,
        Has<systems::lod22::Lod22Mesh>,
        Has<systems::map_stream::StreamTile>,
        Has<systems::globe::GlobeMesh>,
    )>,
) {
    *next -= time.delta_secs();
    if *next > 0.0 {
        return;
    }
    *next = 5.0;

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
            &mut bevy::post_process::bloom::Bloom,
        ),
        (
            With<systems::free_camera::FreeCamera>,
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
    if let Ok((_, _, mut exp, mut bloom)) = camera.single_mut() {
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
        bloom.intensity = systems::free_camera::GROUND_BLOOM_INTENSITY.lerp(
            systems::free_camera::SPACE_BLOOM_INTENSITY,
            space,
        );
    }

    // Above the atmosphere the sun is unfiltered: no air mass to redden or dim
    // it, and no local horizon to set behind. Attenuating it by the *anchor's*
    // ground-level extinction (`sun_light`) is what left the globe a dim red disc
    // at 18:00 and unlit at all at night, when what the view should show is a
    // fully sunlit day side and a dark night side, terminator between. Crossfaded
    // rather than switched, or climbing out at dusk changed the light in a step.
    let (ground_color, ground_lux) = sun_light(dev.time_of_day, dev.sun_declination_rad, dev.fog_density);
    let mut sun_color = Color::from(LinearRgba::from_vec4(
        LinearRgba::from(ground_color).to_vec4().lerp(LinearRgba::WHITE.to_vec4(), space),
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
        With<systems::free_camera::FreeCamera>,
    >,
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
    let mut apply = |bucket: &systems::osm_buildings::WindowGlow, mat: &MeshMaterial3d<StandardMaterial>| {
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
    // Startup switch, alongside VIBERFOX_NO_ATMOSPHERE: leave the haze shell out
    // entirely, to tell "the scene is dark" from "a sky shell is covering it"
    // when bisecting a render problem with `--shot`.
    if env_flag("VIBERFOX_NO_HAZE") {
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
    let mesh = meshes.add(Sphere::new(1.0).mesh().ico(5).unwrap());
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
        (&mut Transform, &mut Visibility, &MeshMaterial3d<HorizonHazeMaterial>),
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
    let radius = systems::free_camera::fog_saturation_distance(&fog.falloff)
        .clamp(1_000.0, 150_000.0);
    tf.scale = Vec3::splat(radius);

    // Once it carries no haze at all, drop the shell out of the frame rather than
    // drawing a fully transparent shell over the whole view every frame.
    let want = if peak <= 0.0 { Visibility::Hidden } else { Visibility::Inherited };
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
        mat.params = Vec4::new(
            0.25 * d,
            peak,
            fog.directional_light_exponent,
            d * d,
        );
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
        (
            With<systems::free_camera::FreeCamera>,
            Without<StarSky>,
        ),
    >,
    mut star: Query<(&mut Transform, &MeshMaterial3d<StarFieldMaterial>), With<StarSky>>,
    mut materials: ResMut<Assets<StarFieldMaterial>>,
) {
    let Ok(cam) = camera.single() else {
        return;
    };
    let Ok((mut tf, mat_handle)) = star.single_mut() else {
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
    if let Some(mut mat) = materials.get_mut(&mat_handle.0) {
        mat.params = Vec4::new(
            night * STAR_BRIGHTNESS * fog_veil,
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

/// Height of the cloud layer above the camera, in metres — a mid-level deck.
const CLOUD_LAYER_HEIGHT: f32 = 2_500.0;
/// Noise frequency (m⁻¹) — must match `FREQ` in `shaders/clouds.wgsl` so the
/// CPU-side world-anchor offset lands in the same units as the shader's sample.
const CLOUD_FREQ: f32 = 0.00034;
/// Overall cloud opacity before the coverage/altitude/space fades.
const CLOUD_OPACITY: f32 = 0.92;
/// Edge softness of the coverage cutoff (0 = hard, higher = wispier margins).
const CLOUD_SOFTNESS: f32 = 0.18;
/// Wind velocity in the shader's UV units per second (drifts the deck). The
/// visible field spans ~1.7 UV units, so ~0.04/s crosses it in ~40 s — a clear
/// drift without racing (the old 0.004 was imperceptible).
/// (`systems::water` reuses the direction so the waves travel with the clouds.)
pub const CLOUD_WIND: Vec2 = Vec2::new(0.04, 0.015);
/// Clouds are at full strength up to this camera altitude, then fade out so a
/// high flight or orbit view sees clear sky (the shell is a ceiling seen from
/// below; flying *above* the deck to look down on the tops is a follow-up).
const CLOUD_FADE_LOW: f32 = 2_000.0;
/// Camera altitude past which the cloud deck has fully faded out.
const CLOUD_FADE_HIGH: f32 = 9_000.0;

/// Camera-centred procedural cloud ceiling, alpha-blended over the sky. Coverage
/// is driven by the dev panel (`DevPanelState::cloud_cover`); lit/shadow colours
/// track the day/night cycle so the deck matches the sun (see `update_clouds`).
#[derive(Asset, TypePath, AsBindGroup, Clone)]
struct CloudMaterial {
    /// x: coverage 0–1, y: softness, z: time (wind clock), w: layer height (m).
    #[uniform(0)]
    params0: Vec4,
    /// xy: wind velocity, z: space fade (1 = hidden), w: altitude fade.
    #[uniform(1)]
    params1: Vec4,
    /// World direction to the sun (for the self-shadow tap).
    #[uniform(2)]
    sun_dir: Vec4,
    /// Sunlit cloud-top colour; alpha carries the overall opacity.
    #[uniform(3)]
    lit_color: LinearRgba,
    /// Shadowed underside colour.
    #[uniform(4)]
    shadow_color: LinearRgba,
    /// xy: world-anchor offset in UV units (camera displacement from the anchor
    /// × the shader's FREQ), so the deck stays fixed over the ground.
    #[uniform(5)]
    params2: Vec4,
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

/// Spawns the cloud shell (mirrors `setup_horizon_haze`). `VIBERFOX_NO_CLOUDS`
/// leaves it out for bisecting a render problem with `--shot`.
fn setup_clouds(
    mut commands: Commands,
    mut meshes: ResMut<Assets<Mesh>>,
    mut materials: ResMut<Assets<CloudMaterial>>,
) {
    if env_flag("VIBERFOX_NO_CLOUDS") {
        return;
    }
    let mesh = meshes.add(Cuboid::from_length(360_000.0).mesh());
    let material = materials.add(CloudMaterial {
        params0: Vec4::ZERO, // set each frame from the dev panel
        params1: Vec4::ZERO,
        sun_dir: Vec4::Y,
        lit_color: LinearRgba::WHITE,
        shadow_color: LinearRgba::BLACK,
        params2: Vec4::ZERO,
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
/// panel and lit/shadow colours from the day/night cycle, and fades the deck out
/// with altitude (high flight / orbit) and the space tier.
#[allow(clippy::type_complexity)]
fn update_clouds(
    dev: Res<DevPanelState>,
    time: Res<Time>,
    globe: Res<systems::globe::GlobeState>,
    camera: Query<
        (&GridCell, &GlobalTransform, &Transform),
        (With<Camera3d>, Without<Clouds>),
    >,
    mut clouds: Query<
        (&mut Transform, &mut Visibility, &MeshMaterial3d<CloudMaterial>),
        With<Clouds>,
    >,
    mut materials: ResMut<Assets<CloudMaterial>>,
) {
    let Ok((cell, cam, cam_local)) = camera.single() else {
        return;
    };
    let Ok((mut tf, mut vis, mat_handle)) = clouds.single_mut() else {
        return;
    };
    tf.translation = cam.translation();

    let altitude = globe.altitude_m(cell, cam_local) as f32;
    let alt_fade = (1.0
        - ((altitude - CLOUD_FADE_LOW) / (CLOUD_FADE_HIGH - CLOUD_FADE_LOW)))
        .clamp(0.0, 1.0);
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

    // Lit tops and shadowed undersides track the day/night cycle: sun-tinted and
    // bright by day, deep blue-grey at night (the deck goes with the sky).
    let day = utils::sun::daylight(dev.time_of_day, dev.sun_declination_rad);
    let (sun_c, _) = sun_light(dev.time_of_day, dev.sun_declination_rad, dev.fog_density);
    let sun_v = LinearRgba::from(sun_c).to_vec4();
    let day_lit = Vec4::splat(1.0).lerp(sun_v, 0.5);
    let night_lit = Vec4::new(0.06, 0.08, 0.13, 1.0);
    let mut lit = night_lit.lerp(day_lit, day);
    lit.w = CLOUD_OPACITY;
    let day_shadow = Vec4::new(0.45, 0.50, 0.60, 1.0).lerp(sun_v, 0.2);
    let night_shadow = Vec4::new(0.02, 0.03, 0.06, 1.0);
    let shadow = night_shadow.lerp(day_shadow, day);

    if let Some(mut mat) = materials.get_mut(&mat_handle.0) {
        mat.params0 = Vec4::new(
            dev.cloud_cover,
            CLOUD_SOFTNESS,
            time.elapsed_secs(),
            CLOUD_LAYER_HEIGHT,
        );
        mat.params1 = Vec4::new(CLOUD_WIND.x, CLOUD_WIND.y, space, alt_fade);
        mat.sun_dir = sun_direction(dev.time_of_day, dev.sun_declination_rad).extend(0.0);
        mat.lit_color = LinearRgba::from_vec4(lit);
        mat.shadow_color = LinearRgba::from_vec4(shadow);
        // World-anchor: the camera's horizontal position in the local anchor
        // frame (× FREQ, matching the shader) so the deck stays fixed over the
        // ground as you fly under it, rather than sliding with the camera.
        let edge = systems::free_camera::WORLD_CELL_EDGE as f32;
        let world_x = cell.x as f32 * edge + cam_local.translation.x;
        let world_z = cell.z as f32 * edge + cam_local.translation.z;
        mat.params2 = Vec4::new(world_x * CLOUD_FREQ, world_z * CLOUD_FREQ, 0.0, 0.0);
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

/// Spawns the rain overlay quad. `VIBERFOX_NO_RAIN` leaves it out for `--shot`
/// bisecting.
fn setup_rain(
    mut commands: Commands,
    mut meshes: ResMut<Assets<Mesh>>,
    mut materials: ResMut<Assets<RainMaterial>>,
) {
    if env_flag("VIBERFOX_NO_RAIN") {
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
    camera: Query<
        (&GridCell, &GlobalTransform, &Transform),
        (With<Camera3d>, Without<Rain>),
    >,
    mut rain: Query<(&mut Transform, &MeshMaterial3d<RainMaterial>), With<Rain>>,
    mut materials: ResMut<Assets<RainMaterial>>,
) {
    let Ok((cell, cam, cam_local)) = camera.single() else {
        return;
    };
    let Ok((mut tf, mat_handle)) = rain.single_mut() else {
        return;
    };

    let altitude = globe.altitude_m(cell, cam_local) as f32;
    let alt_fade = (1.0
        - ((altitude - RAIN_FADE_LOW) / (RAIN_FADE_HIGH - RAIN_FADE_LOW)))
        .clamp(0.0, 1.0);
    let intensity = dev.precipitation * alt_fade;

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
    if env_flag("VIBERFOX_NO_CLOUDS") {
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
        (With<systems::free_camera::FreeCamera>, Without<GlobeClouds>),
    >,
    mut clouds: Query<
        (&mut Transform, &mut Visibility, &MeshMaterial3d<GlobeCloudMaterial>),
        With<GlobeClouds>,
    >,
    mut materials: ResMut<Assets<GlobeCloudMaterial>>,
    cloud_data: Option<Res<systems::globe_weather::GlobeCloudData>>,
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
    let edge = systems::free_camera::WORLD_CELL_EDGE as f64;
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
        // — `VIBERFOX_CLOUD_TIME=<seconds>` seeds the phase to inspect the field
        // at any age (e.g. verify it doesn't smear after 10 min), like `--time`
        // does for the day-night lighting.
        let cloud_t = std::env::var("VIBERFOX_CLOUD_TIME")
            .ok()
            .and_then(|s| s.parse::<f32>().ok())
            .unwrap_or_else(|| time.elapsed_secs());
        mat.params = Vec4::new(GLOBE_CLOUD_COVER, space, cloud_t, GLOBE_CLOUD_FREQ);
        mat.lit_color = LinearRgba::from_vec4(lit);
        mat.shadow_color = LinearRgba::rgb(0.06, 0.09, 0.16);
        mat.north_axis = globe.north_axis_local().extend(0.0);
    }
}
