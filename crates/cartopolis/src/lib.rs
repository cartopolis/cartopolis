use bevy::asset::{AssetApp, AssetPlugin};
use bevy::light::DirectionalLightShadowMap;
// Used on every target except wasm, but for opposite reasons: desktop disables it
// in favour of `tracing_subscriber`, Android reconfigures it. See `run`.
#[cfg(not(target_arch = "wasm32"))]
use bevy::log::LogPlugin;
use bevy::prelude::*;
use bevy::window::WindowResolution;

mod cli;
mod components;
#[cfg(target_os = "android")]
mod crash;
mod debug_switches;
mod grid;
mod login;
mod phase;
mod platform;
mod quality;
mod render_halt;
mod render_setup;
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

#[cfg(target_os = "android")]
pub(crate) use crash::tail_bytes;
#[cfg(target_os = "android")]
pub(crate) use debug_switches::android_property_name;
// The switches are read all over the crate as `crate::env_flag` /
// `crate::env_value`; they live in `debug_switches` and are named from here.
pub use debug_switches::{env_flag, env_value, init_debug_switches};

use resources::{
    AuthIdentity, AvatarChooserState, AvatarState, CameraState, ContextMenuState, DevPanelState,
    EditDialogState, GameState, LocalAvatarSimId, MarqueeState, OsmTileUrlTemplate,
    PrimTextureCache, TextureLibrary,
};
use systems::egui_manager::EguiPlugin;
use systems::*;

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
    // `cartopolis mcp`: the stdio relay an editor launches. First, before any
    // of the rest exists, because its stdout is the protocol and nothing else
    // may be written there (`systems::mcp::relay`).
    #[cfg(mcp)]
    if systems::mcp::relay::requested() {
        systems::mcp::relay::main();
    }
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

/// Logical cores on this machine. `available_parallelism` is `Unsupported` on
/// `wasm32-unknown-unknown`, so the browser is asked instead — before this, every
/// browser got the fallback of four workers whatever it ran on.
#[cfg(target_arch = "wasm32")]
fn core_count() -> Option<usize> {
    let n = web_sys::window()?.navigator().hardware_concurrency() as usize;
    (n > 0).then_some(n)
}

#[cfg(not(target_arch = "wasm32"))]
fn core_count() -> Option<usize> {
    std::thread::available_parallelism().ok().map(|n| n.get())
}

/// What `CARTO_WEB_LIMITS` holds the device to: the WebGPU spec's defaults, with
/// the few a real browser raises put back. The spec's 256 MB `maxBufferSize` is
/// only a floor — Chrome 146 on the Steam Deck (RADV) reported 2 GB for it and for
/// storage bindings, 16 storage buffers and 48 sampled textures per stage (read
/// from `GPUAdapter.limits` on 2026-10-08) — and bevy asks the browser for
/// everything the adapter has, so the bare defaults failed mesh slabs the web
/// never refuses. The per-stage uniform count stayed at the spec's 12 there, and
/// that is the one that took the web build down.
fn browser_limits() -> bevy::render::settings::WgpuLimits {
    const GIB2: u32 = 2 << 30;
    bevy::render::settings::WgpuLimits {
        max_buffer_size: u64::from(GIB2),
        max_storage_buffer_binding_size: u64::from(GIB2),
        max_storage_buffers_per_shader_stage: 16,
        max_sampled_textures_per_shader_stage: 48,
        ..default()
    }
}

/// Build and run the client. The `cartopolis` binary is a one-line shim over this
/// (`src/main.rs`); everything lives in the lib target so `cargo test` links the
/// rlib instead of compiling all ~52k lines a second time under `cfg(test)`.
fn run_client() {
    dotenvy::dotenv().ok();

    #[cfg(target_os = "android")]
    crash::install_android_panic_hook();
    // Before anything else can fail: if the last run died, let the user send the
    // reason somewhere useful without needing a cable.
    #[cfg(target_os = "android")]
    crash::offer_last_crash();
    #[cfg(target_os = "android")]
    let _ = std::fs::write(crash::run_breadcrumb_path(), "");
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
    let workers = core_count()
        .map(|n| n.saturating_sub(1))
        .unwrap_or(4)
        .max(1);
    platform::workers::start(match quality::budget().worker_cap {
        Some(cap) => workers.min(cap),
        None => workers,
    });

    let cli = cli::parse_cli();

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
        app.set_error_handler(render_halt::warn_once_per_failure);
    }
    // The scripted capture is configured *before* the app is built: a preset
    // manifest can set the render size, and a windowless run needs that size to
    // stand a `HeadlessSurface` in for the missing `Window`. A bad manifest or
    // `--at` therefore fails here with a message, not as a black run.
    //
    // Mutable only where there is an MCP server to replace it with a live
    // harness; off the desktop nothing reassigns it and the `mut` is dead.
    #[cfg_attr(not(mcp), allow(unused_mut))]
    let (mut harness, perf_run) = match cli::build_shot_harness(&cli) {
        Ok(h) => h,
        Err(e) => {
            eprintln!("{e}");
            std::process::exit(2);
        }
    };
    // Same rule, same reason: `--mcp-headless` also decides whether there is a
    // window, so a bad socket path or size has to fail here rather than at the first
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
        Ok(c) => c,
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
    //
    // `CARTO_WEB_LIMITS=1`: hold the device to the limits a browser hands out
    // (`browser_limits`). A desktop Vulkan driver allows far more,
    // so a pipeline over a browser limit compiles and draws everywhere except the
    // web — where the browser refuses it and the renderer halts on the first
    // frame that needs it. That is how the cloud deck took the web build down
    // (13 uniform buffers in one fragment stage, against 12) while every native
    // capture was fine. Run any capture with this set to see the web's failures.
    //
    // The limits alone are not the browser: bevy picks its *bindless* material
    // path from device features, and a native device that still offers binding
    // arrays takes that path and then fails a binding-array limit the web never
    // reaches — which hid the real failure the first time this was tried. So the
    // features the webgpu backend lacks go too (binding arrays, immediates).
    use bevy::render::settings::WgpuFeatures as F;
    let no_dual_source = env_flag("CARTO_NO_DUAL_SOURCE");
    let web_limits = env_flag("CARTO_WEB_LIMITS");
    let mut disabled = F::empty();
    if no_dual_source {
        disabled |= F::DUAL_SOURCE_BLENDING;
    }
    if web_limits {
        disabled |= F::TEXTURE_BINDING_ARRAY
            | F::BUFFER_BINDING_ARRAY
            | F::SAMPLED_TEXTURE_AND_STORAGE_BUFFER_ARRAY_NON_UNIFORM_INDEXING
            | F::PARTIALLY_BOUND_BINDING_ARRAY
            | F::IMMEDIATES;
    }
    let default_plugins = if no_dual_source || web_limits {
        default_plugins.set(bevy::render::RenderPlugin {
            render_creation: bevy::render::settings::RenderCreation::Automatic(Box::new(
                bevy::render::settings::WgpuSettings {
                    disabled_features: Some(disabled),
                    constrained_limits: web_limits.then(browser_limits),
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
        .add_plugins(render_setup::ForceCpuClusteringPlugin)
        // Same reason for the placement: it orders a `RenderStartup` system after
        // one `RenderPlugin` registers, so the sub-app has to exist first.
        .add_plugins(render_setup::GpuPreprocessingOverridePlugin)
        // After DefaultPlugins so the render sub-app's own default is already
        // there to be replaced.
        .add_plugins(render_setup::MeshSlabCapPlugin)
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
        //
        // Every custom material's `MaterialPlugin`, plus the sunbeams'
        // `FullscreenMaterialPlugin`, in the order they were registered here before
        // they moved. They live in `cartopolis_materials` because Bevy's
        // per-material plugin code is monomorphised wherever the plugin is named,
        // and here it was about a quarter of this crate's generated LLVM IR
        // (docs/notes/client-build-cost.md).
        .add_plugins(cartopolis_materials::MaterialsPlugin)
        // The launch's own state: resources `lib.rs` defines, or whose value comes
        // from the command line, the user store or the render budget. Everything a
        // systems group owns is initialised by that group's plugin below.
        .init_resource::<GameState>()
        .init_resource::<AvatarState>()
        .init_resource::<resources::NetPredictionHealth>()
        .init_resource::<LocalAvatarSimId>()
        .init_resource::<CameraState>()
        // A scripted run never sees the server handshake that normally supplies the
        // tile template, and the built-in fallback is the raster OSM basemap — which
        // the route planner cannot solve against. A preset manifest may therefore pin
        // its own source; `CARTO_TILE_URL` still wins over it (see `shot_presets`).
        .insert_resource(match harness.as_ref().and_then(|h| h.tile_url.clone()) {
            Some(url) if resources::tile_url_override().is_none() => OsmTileUrlTemplate(
                crate::platform::sync::Arc::new(crate::platform::sync::Mutex::new(url)),
            ),
            _ => OsmTileUrlTemplate::default(),
        })
        // The user's own preferences — units, turn chimes, keep-screen-on, the
        // quality preset. Distinct from `DevPanelState` (a debugging surface) and
        // from `LayerPrefs` (what is drawn); this is what the Settings panel owns,
        // and `render_settings_panel` reads it unconditionally.
        .insert_resource(app_prefs)
        .init_resource::<ContextMenuState>()
        .init_resource::<EditDialogState>()
        .init_resource::<MarqueeState>()
        .init_resource::<TextureLibrary>()
        .init_resource::<PrimTextureCache>()
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
        .insert_resource(user_store)
        .init_resource::<resources::MapAnchor>()
        // Space background: the atmosphere skybox covers this everywhere in the local
        // view, but it's hidden from orbit (ADR-025), so the sky reads as black space.
        .insert_resource(ClearColor(Color::BLACK))
        // A render error halts the renderer and records why — never ends the app,
        // and never rebuilds it. See `handle_render_error`.
        .insert_resource(bevy::render::error_handler::RenderErrorHandler(
            render_halt::handle_render_error,
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
        })
        .init_resource::<AuthIdentity>()
        .init_resource::<resources::RightPanel>();

    // **The client's registration, one plugin per systems group** — each owns its
    // group's resources and its systems, and puts every `Update` system in the
    // `phase::ClientPhase` it belongs to. The phase order is declared once, by
    // `ClientPhasePlugin`; the module note in `phase.rs` says what runs where and
    // why. Added after everything above, which they may read while building
    // (`MapPlugin`'s `WaterSeaState` needs the `WaterMaterial` plugin), and before
    // the session request below, which writes into `NetPlugin`'s `SessionState`.
    app.add_plugins((
        phase::ClientPhasePlugin,
        ShellPlugin,
        systems::net::NetPlugin,
        systems::map::MapPlugin {
            live_weather: !scripted || cli.weather_live,
        },
        systems::nav::NavPlugin,
        systems::player::PlayerPlugin,
        systems::gui::GuiPlugin {
            aerial: cli.aerial,
            sewers: cli.sewers,
            underground: cli.underground.clone(),
            satellites: cli.satellites,
        },
        systems::dev::DevPlugin,
    ));

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

    // The three registrations only some launches have, each carrying what the
    // command line resolved to (`systems::dev`'s module note). Added after the
    // session request, which is where `launch_sign_in` is decided.
    if let Some(harness) = harness {
        app.add_plugins(systems::dev::ShotHarnessPlugin { harness });
    }
    if let Some(run) = perf_run {
        app.add_plugins(systems::dev::PerfPlugin { run });
    }
    #[cfg(mcp)]
    if let Some(config) = mcp_config {
        app.add_plugins(systems::dev::McpPlugin {
            config,
            allow: cli.mcp_allow.clone(),
            launch_sign_in,
        });
    }

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

/// Registration for the systems that answer for the app as a whole rather than
/// for one systems group: the avatar's existence, the Android lifecycle and back
/// gesture, the render-cost log and the startup reports. Everything else is
/// registered by its group's plugin.
///
/// The functions themselves live with their groups (`local_avatar`, `lifecycle`,
/// `dismiss`, `driver`, `render_census`); only the registration stays here,
/// because moving it into those groups' plugins would change the order the
/// systems are added in, and this plugin is added ahead of all of them.
struct ShellPlugin;

impl Plugin for ShellPlugin {
    fn build(&self, app: &mut App) {
        // Which rung this run took, said where a phone can read it —
        // `quality::init` answers the question before `LogPlugin` exists, so its
        // own line has never reached logcat. See `quality::report_budget`.
        app.add_systems(Startup, quality::report_budget);
        // Whether the APK's own `classes.dex` is loaded and callable from the
        // thread `android_main` runs on. Reported for the same reason the budget
        // above is: it is a startup fact with no other instrument on a phone, and
        // every Java-shaped feature planned on top of the dex lane — the
        // background-connection service first — is dead if it is `NOT reachable`.
        #[cfg(target_os = "android")]
        app.add_systems(Startup, platform::android::report_dex);

        app.add_systems(
            Update,
            (
                // Before `attach_avatar_to_bigspace`, so an avatar that appears
                // when a session comes up is in the grid on the frame it is spawned
                // rather than the one after.
                local_avatar::sync_local_avatar,
                local_avatar::attach_avatar_to_bigspace.after(local_avatar::sync_local_avatar),
            )
                .in_set(phase::ClientPhase::Input),
        )
        .add_systems(
            Update,
            (
                // The two systems that make the app a well-behaved Android
                // citizen: stop working while backgrounded, and answer the back
                // gesture.
                lifecycle::quiesce_while_backgrounded,
                dismiss::handle_back_gesture,
                driver::sync_guidance_pref,
                // Gated at registration, not inside the body. The body's own
                // `is_phone()` return came *after* the system was in the schedule
                // with a seven-component unfiltered `Query<&Mesh3d, …>`, which Bevy
                // re-matches against every new archetype — and archetypes are
                // created continuously here as tiles, buildings and surfaces
                // stream. A system that runs is a system that pays for its query;
                // the cheapest diagnostic is one the phone never registers.
                render_census::log_render_cost
                    .run_if(|| !quality::is_phone() || env_flag("CARTO_RENDER_COST")),
            )
                .in_set(phase::ClientPhase::Present),
        );
    }
}
