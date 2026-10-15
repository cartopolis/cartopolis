//! The command line: what a launch was asked to do, and the scripted capture it
//! resolves to. Parsed once, near the top of `run_client`.

use clap::Parser;
use std::path::PathBuf;

use crate::systems;

#[derive(Parser, Debug)]
#[command(name = "cartopolis")]
pub(crate) struct Cli {
    /// Connect to a `simulator` at this address. Omit to enter whatever the
    /// sign-in screen last remembered — offline on a fresh install
    /// (`systems::session`).
    #[arg(long)]
    pub(crate) connect: Option<String>,

    /// Run without any sim: single user, no auth, no persistence. What `--shot`
    /// uses. The sign-in screen can still switch modes from inside the run; this
    /// only decides where the launch starts.
    #[arg(long)]
    pub(crate) offline: bool,

    /// Credentials for the `--connect` handshake, so you don't log in each run: a
    /// path to a TOML login file, or inline `user:pass`. Defaults to
    /// `data/dev-login.toml` if present, else env `CARTO_USER`/`CARTO_PASSWORD`,
    /// else `dev:dev`. Meaningless without a server to connect to.
    #[arg(long, value_name = "FILE|user:pass")]
    pub(crate) login: Option<String>,

    /// Scripted capture: place the camera, wait for the world to stream in,
    /// save this PNG and exit (see `systems::shot_harness`).
    #[arg(long, value_name = "PATH")]
    pub(crate) shot: Option<PathBuf>,

    /// Run the remembered camera positions from a preset manifest (default
    /// `docs/shots.toml`): every named view captured in one launch, each to its
    /// own file, with its own camera mode, panels and time of day. This is the
    /// documentation-image build step — see `cargo shots`. Bare `--shots` (the
    /// common case) parses to `Some(None)` and uses the checked-in manifest.
    #[arg(long, value_name = "FILE", num_args = 0..=1)]
    pub(crate) shots: Option<Option<PathBuf>>,

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
    pub(crate) perf: Option<Option<PathBuf>>,

    /// Where the performance report goes. Defaults to `perf.json` in the
    /// temporary directory.
    #[arg(long, value_name = "PATH")]
    pub(crate) perf_out: Option<PathBuf>,

    /// Frames in every `steady` window, overriding the manifest.
    #[arg(long, value_name = "N")]
    pub(crate) perf_frames: Option<u32>,

    /// Fly every `--perf` scenario N times in one launch, round-robin, and report
    /// each window's columns as runs, min, median and max rather than as one
    /// sample. A single flight differenced against another is how every withdrawn
    /// performance result here was made.
    #[arg(long, value_name = "N", default_value_t = 1)]
    pub(crate) repeat: u32,

    /// Restrict a `--shots` run to these preset names (comma-separated).
    #[arg(long, value_name = "NAMES", value_delimiter = ',')]
    pub(crate) only: Vec<String>,

    /// Camera position for `--shot`, as "lat,lng,altitude_m". Repeat for a
    /// sweep: one capture each, all in a single launch.
    #[arg(long, value_name = "LAT,LNG,ALT")]
    pub(crate) at: Vec<String>,

    /// Look angles for `--shot`, as "pitch,yaw" in degrees (−90 = straight
    /// down). Pairs with `--at` by position; the last one carries over.
    #[arg(long, value_name = "PITCH,YAW")]
    pub(crate) look: Vec<String>,

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
    pub(crate) look_at: Vec<String>,

    /// Offscreen render size for `--shot` (default 1280x720). Overrides the
    /// manifest's `size` in a `--shots` run.
    #[arg(long, value_name = "WxH")]
    pub(crate) size: Option<String>,

    /// Seconds of quiet tile streaming required before a `--shot` capture
    /// (default 0.8; overrides the manifest's `settle`).
    #[arg(long)]
    pub(crate) settle: Option<f32>,

    /// Hard cap on the settle wait per capture, in seconds (default 15).
    #[arg(long)]
    pub(crate) wait: Option<f32>,

    /// Leave the egui overlay (menu bar, HUD) out of `--shot` captures.
    #[arg(long)]
    pub(crate) hide_ui: bool,

    /// Open UI panels in `--shot` captures, comma-separated: `dev`, `layers`,
    /// `places`, `avatar`, `stats`, `route`, or `all`. The menu bar and
    /// HUD already show (unless `--hide-ui`); this documents a panel too, e.g.
    /// `--ui dev` to capture the lighting/weather controls. `route` also seeds a
    /// demo route, so the planner is captured with a result in it.
    #[arg(long, value_name = "LIST", value_delimiter = ',')]
    pub(crate) ui: Vec<String>,

    /// Camera mode for `--shot`: "free" (exact placement) or "earth" (orbit).
    #[arg(long, default_value = "free")]
    pub(crate) shot_mode: String,

    /// Densify the `--at` waypoints (two or more) into an N-frame sweep: capture N
    /// viewpoints interpolated along the path (altitude log-interpolated) instead
    /// of one per `--at`. With `--csv` this A/B's the altitude-driven ramps
    /// (space_factor, exposure, bloom, albedo) for a hard cut. 0 = off.
    #[arg(long, default_value_t = 0, value_name = "N")]
    pub(crate) frames: u32,

    /// Write per-capture metrics (altitude, tier, ramp scalars, mesh tally) as one
    /// CSV row each — the sweep analysis artifact (see `systems::shot_harness`).
    #[arg(long, value_name = "PATH")]
    pub(crate) csv: Option<PathBuf>,

    /// Dump the full per-capture metrics as JSON on exit (one object for a single
    /// shot, an array for a sweep) — inspect the settled world state from a shell.
    #[arg(long, value_name = "PATH")]
    pub(crate) dump_state: Option<PathBuf>,

    /// Assert a metric on every capture and exit non-zero if it does not hold —
    /// what makes `--shot` a CI gate rather than only an instrument. Repeatable:
    /// `--expect settled==true --expect globe_rendered>0`. Any `--dump-state`
    /// field is a valid key (an unknown one is an error, not a silent pass);
    /// operators are `>= <= == != > <`, booleans are `true`/`false`. Use `>=`
    /// rather than `==` on a float.
    #[arg(long, value_name = "KEY<OP>VALUE")]
    pub(crate) expect: Vec<String>,

    /// Time of day in hours (0–24) to start at — the main knob for checking the
    /// lighting curves in a `--shot` capture (e.g. `--time 6` for sunrise).
    #[arg(long, value_name = "HOURS")]
    pub(crate) time: Option<f32>,

    /// Distance-fog density to start at (0–1); pairs with `--time` for isolating
    /// how much of a washed-out frame is the fog and how much is the lighting.
    #[arg(long, value_name = "DENSITY")]
    pub(crate) fog: Option<f32>,

    /// Cloud coverage to start at (0–1): fraction of the sky the procedural
    /// cloud deck fills. 0 = clear sky. Handy for A/B'ing weather in `--shot`.
    #[arg(long, value_name = "COVERAGE")]
    pub(crate) clouds: Option<f32>,

    /// Rain intensity to start at (0–1): density of the screen-space rain
    /// streaks. 0 = dry. Pairs with `--clouds` for an overcast, wet scene.
    #[arg(long, value_name = "INTENSITY")]
    pub(crate) rain: Option<f32>,

    /// Pull real conditions for the map anchor from met.no and drive the
    /// cloud/fog/rain knobs from them (overrides `--clouds`/`--rain`/`--fog`).
    /// In `--shot` mode the capture waits for the first sample (capped by `--wait`).
    #[arg(long)]
    pub(crate) weather_live: bool,

    /// Capture every `--at` viewpoint **twice**, ~5 cm apart, and report the
    /// pixels that flipped between the pair as the `flicker_px` metric — the
    /// depth-stability instrument. A z-fight between coplanar surfaces re-rolls
    /// per pixel under any camera move, so it is invisible to a single still and
    /// lights up under a nudge that moves honest geometry sub-pixel. Gate it
    /// with `--expect 'flicker_px<=N'`, in a pinned environment (`--time
    /// --clouds 0 --rain 0`) so traffic and weather set the floor, not the sky.
    #[arg(long)]
    pub(crate) flicker: bool,

    /// Switch the **aerial basemap** on for this run (`systems::aerial`): PDOK's
    /// Luchtfoto RGB under the clipmap instead of the drawn map, over the
    /// Netherlands only. A headless run has no Layers panel to tick, and unlike
    /// the measured ground this one *replaces* the drawn map rather than adding
    /// to it, so it stays opt-in. `CARTO_AERIAL_LAYER` picks the layer.
    #[arg(long)]
    pub(crate) aerial: bool,

    /// Switch the **sewers** layer on for this run (`systems::sewers`): the
    /// municipal sewer network drawn at its depth, and the ground near the
    /// camera opened to show it. Opt-in for the reason `--aerial` is — it
    /// changes how the ground looks.
    #[arg(long)]
    pub(crate) sewers: bool,

    /// Switch underground layers on for this run, by `underground::LAYERS` key
    /// (`geotop,gasfield`), or `all`. Opt-in for the reason `--sewers` is.
    #[arg(long, value_delimiter = ',')]
    pub(crate) underground: Vec<String>,

    /// Switch the **satellites** layer on for this run (`systems::satellites`):
    /// every active satellite round the globe, drawn from space. Opt-in, as it is
    /// in the panel, because it is a download of its own. `CARTO_SATELLITES_URL`
    /// points it at another element-set file.
    #[arg(long)]
    pub(crate) satellites: bool,

    /// Beside every capture, write the aerial photograph of the ground it looks
    /// at — `<shot>.aerial.jpg`, PDOK's Luchtfoto RGB with the camera and the
    /// point it is aimed at drawn on (`systems::aerial_reference`). The check a
    /// render of a real place is held to: whether the structure it draws is the
    /// one that is there. The Netherlands only.
    #[arg(long)]
    pub(crate) aerial_reference: bool,

    /// Serve the Model Context Protocol on this Unix socket rather than the
    /// default (`$XDG_RUNTIME_DIR/cartopolis/mcp.sock`), so an editor can drive
    /// this client: move the camera, photograph what it sees, read the scene's
    /// numbers (`systems::mcp`). An editor reaches it through `cartopolis mcp`.
    ///
    /// **Serving is on by default on Linux and macOS** and switched in Settings ▸
    /// General ▸ Editor access; this flag only changes *where*, and forces it on
    /// for the run regardless of that switch, as a flag should. Bare, it forces
    /// the default path on. There is no token: the socket is the owner's alone.
    #[cfg(mcp)]
    #[arg(long, value_name = "PATH", num_args = 0..=1, default_missing_value = "")]
    pub(crate) mcp_listen: Option<String>,

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
    pub(crate) mcp_headless: bool,

    /// Whose chat an agent driving this client may read: usernames separated by
    /// commas, or `*` for everyone. Everybody else's lines are withheld from it.
    /// Overrides Settings ▸ General ▸ Editor access for this run; with neither,
    /// the agent hears nobody.
    #[cfg(mcp)]
    #[arg(long, value_name = "NAMES")]
    pub(crate) mcp_allow: Option<String>,

    /// Offscreen surface size for `--mcp-headless`, `WxH`. Ignored when there is
    /// a window, where the window *is* the surface.
    #[cfg(mcp)]
    #[arg(long, value_name = "WxH")]
    pub(crate) mcp_size: Option<String>,

    /// Don't serve MCP this run, whatever Settings remembers.
    ///
    /// For running a second client beside one that already serves the socket, and
    /// for anyone who would rather decide per launch. It does not change the
    /// stored preference — a flag is an instruction for this launch, never
    /// written back, the same rule the sky flags follow.
    #[cfg(mcp)]
    #[arg(long, conflicts_with_all = ["mcp_listen", "mcp_headless"])]
    pub(crate) no_mcp: bool,
}

/// Resolve the scripted capture the CLI asked for, if any.
///
/// Two ways in, and they don't mix: `--shots` runs the remembered camera
/// positions from a preset manifest (the documentation build step), `--shot`
/// places one or more ad-hoc viewpoints from `--at`/`--look`. Both are built
/// here, before the app, because the result decides whether the run gets a
/// window at all and how big its surface is.
pub(crate) type ScriptedRun = (
    Option<systems::shot_harness::ShotHarness>,
    Option<systems::perf::PerfRun>,
);

pub(crate) fn build_shot_harness(cli: &Cli) -> Result<ScriptedRun, String> {
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
            cli.repeat,
            cli.perf_out
                .clone()
                .unwrap_or_else(|| std::env::temp_dir().join("perf.json")),
        )
        .map_err(|e| format!("--perf: {e}"))?;
        return Ok((Some(harness), Some(run)));
    }
    // Refused rather than ignored, as `--only` outside `--shots` is below.
    if cli.repeat != 1 {
        return Err("--repeat repeats the scenarios of a --perf run".into());
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
pub(crate) fn parse_cli() -> Cli {
    Cli::parse()
}

#[cfg(target_os = "android")]
pub(crate) fn parse_cli() -> Cli {
    // `parse_from` rather than a hand-built struct literal: every field keeps its
    // clap-declared default, so a new flag added to `Cli` cannot leave this arm
    // silently uninitialised.
    Cli::parse_from(["cartopolis"])
}
