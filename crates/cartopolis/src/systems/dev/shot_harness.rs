//! Scripted screenshot harness (`--shot`): fly the camera to one or more
//! lat/lng/altitude/orientation set-ups, wait for the world to stream in, and
//! save each frame as a PNG — then exit.
//!
//! This exists so a rendering question ("is the globe actually on screen at
//! 180 km?") can be answered by running the app from a shell, instead of flying
//! there by hand and grabbing the desktop. Each capture also logs a `shot state`
//! line (altitude, tier, near/far, globe entity/visible/rendered counts) so a
//! black image can be told apart from a black *scene*.
//!
//! ```bash
//! # one shot
//! cargo run -p cartopolis -- \
//!     --shot /tmp/shot.png --at 53.2194,6.5665,180000 --look=-90,0
//!
//! # a sweep: one app launch, one PNG per --at (index- and altitude-tagged)
//! cargo run -p cartopolis -- --shot /tmp/sweep.png \
//!     --at 53.2194,6.5665,60000 --at 53.2194,6.5665,180000 --look=-90,0
//! ```
//!
//! Three things make it cheap and side-effect free:
//!   * **Headless.** No window is created at all — `main` drops `WinitPlugin` and
//!     drives the frames with `ScheduleRunnerPlugin`, and the camera renders into
//!     an offscreen image. Nothing appears on the desktop, no compositor can
//!     occlude (and blacken) the captured frame, and the run needs no display
//!     server. `resources::HeadlessSurface` stands in for the missing `Window`
//!     wherever one is needed (egui layout, screen-space LOD).
//!   * **Settle-on-idle.** Rather than a fixed sleep, each capture waits until
//!     the tile streams go quiet ([`MapStream::streaming_busy`] /
//!     [`GlobeState::streaming_busy`]) for `--settle` seconds, capped by
//!     `--wait`. Cached tiles settle in about a second.
//!   * **No user-store writes.** The interactive session's saved location is
//!     neither restored nor overwritten by a scripted run.
//!
//! The egui overlay is part of the capture (it carries the HUD's altitude and
//! coordinates, which is what makes a shot self-documenting); pass `--hide-ui`
//! for chrome-free images. The toggle-able panels (dev lighting/weather, layers,
//! bookmarks, AI, avatar chooser) are closed by default and can't be opened by
//! hand in a headless run, so `--ui <list>` opens them for the capture, e.g.
//! `--ui dev` or `--ui dev,layers` (or `--ui all`); see [`open_scripted_ui`].
//! `--ui search` opens the Search panel *and* seeds it a query, since an empty
//! field documents nothing; `--ui profiler` and `--ui switches` open Developer
//! tools on a named tab (a tab is not reachable by opening the window alone, so
//! naming the window alone would photograph whichever tab was last selected);
//! and `--ui menu` opens the app mark's card, which is where the entries that
//! left the map live.
//!
//! ## Metrics: the numbers behind the frame
//!
//! Every capture also measures a [`ShotMetrics`] — altitude, tier, the
//! altitude-driven crossfade scalars (`space_factor`, exposure, bloom, globe
//! albedo), near/far, and the globe mesh tally. One source, three sinks: the
//! `shot state` log line (always), a `--csv` row per capture, and a `--dump-state`
//! JSON snapshot. A new inspectable fact is one field on `ShotMetrics` and it
//! shows up in all three at once — this is the introspection capability that lets
//! a rendering question be answered from a shell without a recompile per question.
//!
//! ## Sweeps: `--frames N`
//!
//! With two or more `--at` waypoints, `--frames N` interpolates N viewpoints
//! along the polyline (altitude log-interpolated, lat/lng/look linear) instead of
//! capturing each `--at` verbatim — a filmstrip. Paired with `--csv` this is how a
//! ramp is A/B'd for a *hard cut*: a jump between adjacent rows in the
//! `space_factor` / `exposure` / `bloom` / `globe_albedo` columns is a seam a
//! single still can't reveal. Files are index-prefixed (`sweep_00_..m.png`) so the
//! frames sort in flight order.
//!
//! ```bash
//! # 24-frame fly-out, straight down over Groningen, ramps logged to CSV
//! cargo run -p cartopolis -- --shot /tmp/sweep.png \
//!     --at 53.2194,6.5665,2000 --at 53.2194,6.5665,4000000 --frames 24 \
//!     --look=-90,0 --csv /tmp/ramps.csv
//! ```

use crate::components::GlobeMesh;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use bevy::asset::RenderAssetUsages;
use bevy::camera::{Exposure, RenderTarget};
use bevy::image::Image;
use bevy::post_process::bloom::Bloom;
use bevy::prelude::*;
use bevy::render::render_resource::{Extent3d, TextureDimension, TextureFormat, TextureUsages};
use bevy::render::view::screenshot::{Screenshot, ScreenshotCaptured};
use big_space::prelude::GridCell;
use serde::Serialize;

use crate::components::FreeCamera;
use crate::resources::{CameraState, MapAnchor, ScreenshotState};
use crate::systems::geo_nav::{CameraPose, GeoNavState, Teleport};
use crate::systems::globe::{GlobeState, GlobeSurfaceAlbedo};
use crate::systems::map_stream::MapStream;

/// The world state a viewpoint wants to be shot in. Defined in `cartopolis_core`
/// because the MCP server crate builds one too and may not depend on the client.
pub use cartopolis_core::ShotEnv;

/// Defaults shared by the CLI flags and the preset manifest, so a manifest that
/// omits a key and a command line that omits the flag land on the same value.
pub const DEFAULT_SIZE: &str = "1280x720";
pub const DEFAULT_SETTLE: f32 = 0.8;
pub const DEFAULT_WAIT: f32 = 15.0;

/// How long the GPU readback gets *after* the capture fires, before the run is
/// declared dead. Deliberately a constant and not a function of `--wait`: the
/// settle cap and the readback measure unrelated things, and tying them together
/// meant a short `--wait` failed runs that would otherwise have produced a
/// perfectly good (if half-streamed) image. Generous because it is a backstop
/// against hanging, not a deadline anything is expected to approach — a readback
/// on a GPU lands in a frame or two, and the slowest measured on a software
/// rasteriser was ~5 s.
pub const CAPTURE_GRACE_S: f32 = 60.0;

/// How long to let our own CPU pool drain, after the world is judged ready and
/// before the shutter fires, so the readback is not queued behind a rasterise
/// backlog. See [`crate::platform::workers::set_paused`] for why that queue exists
/// at all. Bounded rather than unconditional: a capture that gave up at `--wait`
/// can have hundreds of jobs outstanding, and a half-drained pool still beats
/// waiting forever. On a settled shot every streamer is already quiet, so this
/// costs nothing.
pub const QUIESCE_MAX_S: f32 = 30.0;

/// One scripted viewpoint.
#[derive(Clone, Default)]
pub struct ShotSpec {
    pub lat: f64,
    pub lng: f64,
    /// Metres above the ground at (lat, lng).
    pub alt: f64,
    /// Degrees: −90 = straight down, 0 = horizon.
    pub pitch_deg: f32,
    /// Degrees: 0 = north, clockwise.
    pub yaw_deg: f32,
    /// Preset name, when this viewpoint came from a `--shots` manifest. Names
    /// the file and labels the metrics row.
    pub name: Option<String>,
    /// What the shot is for — logged so a run reads as a list of intents.
    pub description: Option<String>,
    /// Explicit output path (presets name their own file instead of taking an
    /// index-and-altitude suffix off the base `--shot` path).
    pub out: Option<PathBuf>,
    /// Aim at this place on Earth instead of at these angles (`--look-at`).
    ///
    /// **Because a pitch is not an aim.** Where the camera ends up looking is
    /// `altitude / tan(pitch)` metres ahead, and `altitude` is above the map
    /// anchor's ground while the thing being photographed stands on the *measured*
    /// ground somewhere else — with the terrain layer on, those differ by metres.
    /// At a steep angle that hardly matters; at the shallow angle a road needs, a
    /// three-metre error moves the aim thirty metres, and every low capture in
    /// this feature's history missed by 60 to 120 m for exactly that reason. Given
    /// a target, the direction is computed from where the camera actually is,
    /// after the teleport, in the frame the world is drawn in.
    pub look_at: Option<[f64; 2]>,
    /// Per-shot camera mode, overriding `--shot-mode`.
    pub mode: Option<String>,
    /// Per-shot `--hide-ui`.
    pub hide_ui: Option<bool>,
    /// Per-shot `--ui` panel list.
    pub ui: Option<Vec<String>>,
    /// **Photograph from wherever the camera already is**, placing nothing.
    ///
    /// `lat`/`lng`/`alt`/the look angles are then a record of where that was
    /// rather than a request. This is the before-and-after half of a live run:
    /// change one thing and re-photograph the identical viewpoint. A teleport
    /// would not do — it re-anchors the map, retires the tiles and re-streams the
    /// city, which is a great deal of work to arrive back where you started, and
    /// it would move the ground under the comparison.
    pub in_place: bool,
    /// Per-shot settle and wait, overriding the harness-wide `--settle`/`--wait`.
    ///
    /// Harness-wide is right for a sweep, where every frame is the same kind of
    /// picture. It is not right for a live run (see [`ShotHarness::live`]), where
    /// one request is "re-photograph what you are already looking at" — already
    /// settled, so it wants no patience at all — and the next is a first visit to
    /// a city, which wants all of it.
    pub settle: Option<f32>,
    pub wait: Option<f32>,
    /// Time of day and weather for this shot.
    pub env: ShotEnv,
}

impl ShotSpec {
    /// Interpolate between two viewpoints at `t` in `[0, 1]`. Position and look
    /// angles are linear, but **altitude is geometric** (`a·(b/a)^t`): a fly-out
    /// spans orders of magnitude (2 km → 4,000 km), and a linear sweep would put
    /// almost every sample in space and none where the ground-tier ramps bend.
    /// Geometric spacing keeps the visual rate of change even, so the samples land
    /// across each ramp rather than bunched at one end.
    fn lerp(a: &ShotSpec, b: &ShotSpec, t: f64) -> ShotSpec {
        let lin = |x: f64, y: f64| x + (y - x) * t;
        let alt = if a.alt > 0.0 && b.alt > 0.0 {
            a.alt * (b.alt / a.alt).powf(t)
        } else {
            lin(a.alt, b.alt)
        };
        ShotSpec {
            lat: lin(a.lat, b.lat),
            lng: lin(a.lng, b.lng),
            alt,
            pitch_deg: lin(a.pitch_deg as f64, b.pitch_deg as f64) as f32,
            yaw_deg: lin(a.yaw_deg as f64, b.yaw_deg as f64) as f32,
            // An interpolated frame is not a named preset: it takes the leading
            // waypoint's mode/UI/world state, but gets the indexed sweep filename
            // (a shared `out` would have every frame overwrite one file).
            name: None,
            look_at: None,
            description: None,
            out: None,
            mode: a.mode.clone(),
            hide_ui: a.hide_ui,
            ui: a.ui.clone(),
            in_place: false,
            settle: a.settle,
            wait: a.wait,
            env: a.env.clone(),
        }
    }
}

/// Expand `waypoints` into an even `frames`-sample sweep along the polyline they
/// describe (endpoints included). `frames == 0` (or a single waypoint) passes the
/// waypoints through unchanged — the plain multi-`--at` behaviour.
fn densify_path(waypoints: &[ShotSpec], frames: u32) -> Vec<ShotSpec> {
    if frames == 0 || waypoints.len() < 2 {
        return waypoints.to_vec();
    }
    let segs = (waypoints.len() - 1) as f64;
    (0..frames)
        .map(|k| {
            let t = if frames == 1 {
                0.0
            } else {
                k as f64 / (frames - 1) as f64
            };
            // Map the global parameter onto a segment. Clamping `seg` (rather than
            // `pos`) means the final sample resolves to `local == 1.0` on the last
            // segment — landing exactly on the last waypoint, not a hair short.
            let pos = t * segs;
            let seg = (pos.floor() as usize).min(waypoints.len() - 2);
            ShotSpec::lerp(&waypoints[seg], &waypoints[seg + 1], pos - seg as f64)
        })
        .collect()
}

/// The scripted run, parsed from the CLI. Present as a resource only when
/// `--shot` was passed; its absence is what other systems test to know the app
/// is in normal interactive mode.
#[derive(Resource, Clone)]
pub struct ShotHarness {
    /// Base output path; with more than one shot a zero-padded index and the
    /// altitude are appended (`/tmp/s.png` → `/tmp/s_00_180000m.png`). See
    /// [`ShotHarness::path_for`].
    pub path: PathBuf,
    pub shots: Vec<ShotSpec>,
    /// Offscreen render size.
    pub size: UVec2,
    /// Seconds of quiet streaming required before capturing.
    pub settle: f32,
    /// Hard cap on the wait per shot.
    pub wait: f32,
    /// Leave the egui overlay out of the capture.
    pub hide_ui: bool,
    /// UI panels to open for the capture (`dev`, `layers`, `bookmarks`,
    /// `avatar`, `stats`, or `all`) — see [`open_scripted_ui`]. Empty = just the
    /// always-on HUD + menu bar.
    pub ui: Vec<String>,
    /// "free" (exact placement) or "earth" (the orbit camera).
    pub mode: String,
    /// Write a [`ShotMetrics`] row per capture to this CSV (the sweep analysis
    /// artifact). Header + one row per shot, written when the run completes.
    pub csv: Option<PathBuf>,
    /// Tile source for the run, from a preset manifest's `tile_url` (env
    /// `CARTO_TILE_URL` still wins). Applied in `main` when the resource is built.
    pub tile_url: Option<String>,
    /// Dump the full per-capture [`ShotMetrics`] as JSON on exit — a single
    /// object for one shot, an array for a sweep. The "inspect the settled world
    /// from a shell" snapshot.
    pub dump_state: Option<PathBuf>,
    /// Assertions over each capture's [`ShotMetrics`] (`--expect`). Every one is
    /// checked against every shot; any that does not hold makes the run exit
    /// non-zero. Empty = the harness only reports, and only a render failure can
    /// fail the process.
    pub expect: Vec<Expectation>,
    /// `--flicker`: photograph every viewpoint **twice**, the second time with
    /// the camera nudged [`FLICKER_NUDGE_DEG`] north (~5 cm), and report how many
    /// pixels flipped between the pair as [`ShotMetrics::flicker_px`].
    ///
    /// This is the depth-stability instrument: a z-fight between coplanar
    /// surfaces is *temporal* — deterministic per view matrix, re-rolled by any
    /// camera move — so no single still can show it and every one of this
    /// codebase's coplanar-flicker bugs (roads over roads, water over land, the
    /// ghost-pavement duplicates) shipped past a green screenshot job. Under a
    /// centimetre nudge, honest geometry moves sub-pixel while a depth coin flip
    /// decorrelates completely; the difference *is* the flicker, measured.
    pub flicker: bool,
    /// Write the aerial photograph of the ground each capture looks at beside it
    /// (`<shot>.aerial.jpg`, `aerial_reference`), with the camera and where it
    /// is aimed drawn on — so what the render shows can be checked against what
    /// is there. Off unless `--aerial-reference` or the manifest's
    /// `aerial_reference` asks.
    pub aerial_reference: bool,
    /// **Viewpoints arrive while the app runs**, instead of being a list fixed on
    /// the command line — the MCP server pushing a [`ShotSpec`] per `view` call
    /// (`systems::mcp`).
    ///
    /// It changes exactly one thing about the cycle: running off the end of the
    /// list *parks* the camera operator instead of writing the metrics and
    /// quitting. Everything else — the teleport, the settle test, the worker
    /// quiesce, the shutter, the readback budget, the metrics — is the same code
    /// driving the same state machine, which is the point. A second copy of this
    /// cycle would be a second thing to keep in step with it, and the perf suite
    /// already declined to grow one (see [`ShotProgress::perf_hold`]).
    pub live: bool,
    /// **There is a window, and a person in front of it.**
    ///
    /// True only for a live harness riding along in an ordinary session. It is
    /// what [`harness_active`] answers, so every "a script owns this run"
    /// exemption — the user store, the frame limiter, the load controller — stays
    /// *off* for a client somebody is also using by hand, and stays on for the
    /// windowless runs (`--shot`, `--shots`, `--perf`, `--mcp-headless`) where
    /// nobody is.
    pub windowed: bool,
}

/// How far `--flicker` moves the camera between the two captures of a pair:
/// ~5.5 cm of latitude. Small enough that stable geometry shifts well under a
/// pixel at any shot distance, large enough to change every depth interpolation
/// in the frame (verified against the 2026-08-29 ghost-pavement fight, which a
/// 5 cm pair reproduced reliably).
const FLICKER_NUDGE_DEG: f64 = 5.0e-7;

/// The comparison in an [`Expectation`].
///
/// `==` is exact, which is what the boolean metrics want (`settled==true`) and
/// what a float rarely does — assert a float with `>=` / `<=` unless you mean
/// the bit pattern.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ExpectOp {
    Ge,
    Gt,
    Le,
    Lt,
    Eq,
    Ne,
}

impl ExpectOp {
    fn holds(self, measured: f64, want: f64) -> bool {
        match self {
            Self::Ge => measured >= want,
            Self::Gt => measured > want,
            Self::Le => measured <= want,
            Self::Lt => measured < want,
            Self::Eq => measured == want,
            Self::Ne => measured != want,
        }
    }
}

/// One `--expect <key><op><value>` assertion over a capture's metrics — the flag
/// that turns the harness from an instrument into a **gate**.
///
/// Everything the harness measures is already in [`ShotMetrics`]; what was
/// missing was a way for a caller that cannot look at the image to state what it
/// expected. Without it a CI job can only assert "the process exited 0", which
/// the harness reports for a rendered-but-empty scene too.
///
/// Booleans are written `settled==true`; every other metric is a number
/// (`globe_rendered>0`). An **unknown key is a hard error at startup**, on the
/// same reasoning as the preset manifest's unknown-key rule: a typo'd assertion
/// that quietly holds is worse than no assertion, because a green job now claims
/// something nobody checked.
#[derive(Clone, Debug, PartialEq)]
pub struct Expectation {
    key: String,
    op: ExpectOp,
    want: f64,
    /// The spec exactly as typed, for log lines — `settled==true` reads better
    /// than `settled == 1`.
    text: String,
}

impl std::fmt::Display for Expectation {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.text)
    }
}

impl Expectation {
    /// Parse one `--expect` spec. Two-character operators are tried first, or
    /// `a>=1` would split on `>` and try to parse `=1` as a number.
    pub fn parse(spec: &str) -> Result<Self, String> {
        const OPS: [(&str, ExpectOp); 6] = [
            (">=", ExpectOp::Ge),
            ("<=", ExpectOp::Le),
            ("==", ExpectOp::Eq),
            ("!=", ExpectOp::Ne),
            (">", ExpectOp::Gt),
            ("<", ExpectOp::Lt),
        ];
        let (key, op, raw_value) = OPS
            .iter()
            .find_map(|(sym, op)| spec.split_once(sym).map(|(k, v)| (k.trim(), *op, v.trim())))
            .ok_or_else(|| {
                format!("{spec:?} needs one of >= <= == != > < , e.g. \"globe_rendered>0\"")
            })?;

        if !metric_keys().any(|k| k == key) {
            return Err(format!(
                "{key:?} is not a metric; known metrics are: {}",
                metric_keys().collect::<Vec<_>>().join(", ")
            ));
        }
        let want = match raw_value {
            "true" => 1.0,
            "false" => 0.0,
            v => v
                .parse::<f64>()
                .map_err(|e| format!("{spec:?}: {v:?} is not a number ({e})"))?,
        };
        if want.is_nan() {
            return Err(format!("{spec:?}: NaN compares false against everything"));
        }
        Ok(Self {
            key: key.to_string(),
            op,
            want,
            text: spec.trim().to_string(),
        })
    }

    /// Check against one capture. `Ok(())` holds; `Err` is the reason, ready to log.
    fn check(&self, m: &ShotMetrics) -> Result<(), String> {
        match m.measure(&self.key) {
            Some(v) if self.op.holds(v, self.want) => Ok(()),
            Some(v) => Err(format!("measured {} = {v}", self.key)),
            // `ground_m` is `NaN` when the height layer is off or out of AHN's
            // coverage, and `NaN` serialises to JSON null. Saying so beats
            // reporting a comparison against a number that was never taken.
            None => Err(format!("{} was not measured in this capture", self.key)),
        }
    }
}

/// The metric names an `--expect` may use: every [`ShotMetrics`] field except the
/// two that are not a number. Derived from [`CSV_HEADER`] rather than listed a
/// second time, so a new metric reaches the flag on the same commit it reaches
/// the CSV — and `expectation_keys_match_the_metrics` fails if they ever part.
fn metric_keys() -> impl Iterator<Item = &'static str> {
    CSV_HEADER
        .split(',')
        .filter(|k| !matches!(*k, "name" | "busy" | "worker_split"))
}

/// Parse every `--expect` spec, failing on the first bad one. Done while the CLI
/// is being read — before the window, the app and several minutes of streaming —
/// so a mistyped assertion costs a second rather than a whole run.
fn parse_expectations(specs: &[String]) -> Result<Vec<Expectation>, String> {
    specs
        .iter()
        .map(|s| Expectation::parse(s).map_err(|e| format!("--expect {e}")))
        .collect()
}

/// The numbers behind one settled capture: the image is the *what*, this is the
/// *why*. Built once per shot in [`drive_capture`] and emitted three ways — the
/// `shot state` log line, a `--csv` row, and a `--dump-state` JSON object — so
/// there is a single source of truth for "what was the world doing here". A black
/// frame with `globe_rendered > 0` is a black *scene*, not a black *capture*; a
/// `space_factor` that jumps between two rows of a sweep is a hard cut in a ramp.
#[derive(Clone, Debug, Serialize)]
pub struct ShotMetrics {
    /// 1-based index within the run.
    pub shot: usize,
    /// Preset name for a `--shots` run — without it a six-row CSV is six
    /// anonymous viewpoints.
    pub name: Option<String>,
    pub lat: f64,
    pub lng: f64,
    /// Where the camera **actually** is, read back out of the drawn world
    /// (`lat`/`lng` are what `--at` asked for). The two differing is a misplaced
    /// capture that would otherwise be filed under the place it was meant for.
    /// `NaN` (null in the JSON) before the map has an anchor.
    pub camera_lat: f64,
    /// See [`Self::camera_lat`].
    pub camera_lng: f64,
    /// The ground point in the middle of the view, the same way; `NaN` when the
    /// camera looks above the horizon.
    pub aim_lat: f64,
    /// See [`Self::aim_lat`].
    pub aim_lng: f64,
    /// Whether the settle test actually passed, or the capture gave up at
    /// `--wait`. Without it the two are indistinguishable in the output: a run
    /// that timed out reports a large `settled_s` and reads as a slow success,
    /// while every other number below describes a *partially streamed* world.
    pub settled: bool,
    /// Seconds from placement to the capture — settled or not (see [`Self::settled`]).
    pub settled_s: f32,
    /// The streamers still working when the capture fired. Empty on a settled
    /// shot; on a timed-out one this *is* the diagnosis, and without it the only
    /// way to learn which stream was slow is to re-run with a longer `--wait`.
    pub busy: Vec<String>,
    /// Mean and worst frame time over the settled frames (ms).
    ///
    /// Both come from `Time<Real>`, and that is a correction rather than a
    /// detail. They used to be accumulated from `Time`, whose delta Bevy clamps at
    /// `max_delta` (0.25 s) — so on a CPU rasteriser, where a settled ground frame
    /// takes the better part of a second, `frame_ms` saturated at exactly 250.0 and
    /// reported a fixed number for every scene. At the other end `frame_limiter`
    /// floored it at the tier's cap. A metric clamped at both ends measures
    /// neither, which is worse than not having one.
    ///
    /// Read them as a pair and read them *sceptically*: a frame time under lavapipe
    /// or on a shared CI runner is a property of the machine, not of the change, so
    /// this is a column to look at and not a column to gate on. `draws` and the
    /// triangle counts below are the ones a gate may assert.
    ///
    /// `frame_ms_max` exists because the mean is the one statistic a hitch cannot
    /// move: forty settled frames at 16 ms and one at 400 average to 25.
    pub frame_ms: f32,
    pub frame_ms_max: f32,
    /// Pixels whose colour flipped between this capture and its `--flicker`
    /// twin, after a 3×3 erosion that discards the one-pixel aliasing crawl a
    /// nudge legitimately causes on silhouettes ([`solid_flips`]). 0 on the
    /// first capture of a pair and on any run without `--flicker`.
    ///
    /// This is the depth-stability number: coplanar surfaces whose ordering the
    /// depth buffer cannot hold re-roll per pixel under any view change, so a
    /// centimetre nudge that moves honest geometry sub-pixel lights them up.
    /// Exact-ish and machine-independent in what it detects (depth resolution is
    /// geometry, which lavapipe rasterises faithfully) — but not zero on a live
    /// scene: traffic, pedestrians and drifting cloud shadows move between the
    /// two captures, so a gate needs a pinned environment (`--time --clouds
    /// --rain`) and a threshold above that animation floor, not `==0`.
    pub flicker_px: u32,
    /// What the renderer was asked to draw at the shutter
    /// ([`crate::systems::render_census`]): visible mesh entities, then triangles
    /// in total, in the shadow-casting subset, and per streamer.
    ///
    /// Unlike the timings above these are exact and machine-independent — they
    /// describe what the client *selected*, and culling, LOD and visibility run the
    /// same code on lavapipe as on a GPU. That makes them the columns the
    /// `scene-smoke` CI job can assert a ceiling on.
    ///
    /// `draws` matters on its own and not only as a proxy for triangles: Bevy
    /// excludes Adreno ≤ 730 and older Mali from GPU preprocessing entirely, so on
    /// the phones this client targets every one of these is a CPU-issued draw call
    /// with its own bind group.
    pub draws: usize,
    pub tris_total: usize,
    pub tris_shadow: usize,
    /// Entities submitted to the shadow cascades. Not derivable from
    /// `tris_shadow`, and the only one of the two that answers for a caster whose
    /// mesh is `RENDER_WORLD`-only — see `render_census::RenderCounts`.
    pub shadow_casters: usize,
    pub tris_buildings: usize,
    pub tris_lod22: usize,
    pub tris_tiles: usize,
    pub tris_globe: usize,
    /// Tile-decode jobs still outstanding when the settle test passed
    /// (`platform::workers`), i.e. the streaming backlog behind the picture.
    ///
    /// Sampled at the *verdict*, not at the shutter, and that is the only place it
    /// can be: the harness quiesces the worker pool between the two, so a reading
    /// taken at the shutter is 0 by construction and says nothing.
    pub worker_outstanding: usize,
    /// Cell fetch jobs outstanding across every streamer at once
    /// (`platform::fetch_gate`), i.e. what the global bound is bounding.
    ///
    /// The network twin of [`Self::worker_outstanding`], and the two are read at
    /// the same moment for the same reason. A capture that settles slowly with
    /// this pinned at the gate's capacity is a client waiting on requests; one
    /// with this near zero and `worker_outstanding` high is waiting on cores.
    pub fetch_outstanding: usize,
    /// Steps of streaming radius `systems::load_control` was withholding when
    /// this was captured — 0 unless the measured residency pushed it up.
    ///
    /// A picture taken with a narrower ring than the tier asked for has to say
    /// so, or a `cargo shots` run that happened to be under memory pressure
    /// silently documents a thinner map than the one this client draws. The
    /// controller is off under the harness, so in a scripted capture this is
    /// always 0 — which is the assertion, not a placeholder.
    pub radius_bias: i64,
    /// Clipmap tiles left out of the draw list because finer, ready ones already
    /// paint their whole footprint (`map_stream::occluded_by_finer`).
    ///
    /// `draws` already shows the *effect*; this shows the mechanism, which is what
    /// you want when it stops working. The culling is silent by construction — a
    /// bug that switched it off would cost fill and change no pixel, so nothing
    /// else in this struct would move enough to notice.
    pub tiles_occluded: usize,
    /// Standing clipmap tiles textured from the aerial source
    /// (`systems::aerial`), i.e. from PDOK rather than from the drawn basemap.
    ///
    /// The one number that separates the three ways an aerial capture can look
    /// like an ordinary one: the toggle is off (`--aerial` not passed, and this
    /// reads 0), the camera is outside the Netherlands (also 0, and that is the
    /// *correct* answer — the fallback held), or the tiles were asked for and did
    /// not arrive (0 with `settled=false`). Colour is not evidence on this
    /// container's software rasteriser, so this is the assertable half.
    pub aerial_tiles: usize,
    pub alt_m: i64,
    /// Space-tier crossfade, 0 (ground) → 1 (deep space). Every ramp rides this.
    pub space_factor: f32,
    pub in_space: bool,
    pub orbit_space: bool,
    pub near_m: i64,
    pub far_m: i64,
    /// Camera exposure (EV100); lower = brighter.
    pub exposure_ev100: f32,
    pub bloom: f32,
    /// Globe imagery tint (1.0 near the threshold → `GLOBE_DEEP_ALBEDO` in space).
    pub globe_albedo: f32,
    /// How far the scattered trees have merged into the flat landuse fill
    /// (`systems::vegetation`): 0 = individual trees, 1 = merged and hidden. The
    /// column to watch in a `--frames` sweep through 500–2000 m — a jump between
    /// adjacent rows is the seam a single still cannot show.
    pub veg_factor: f32,
    /// The same measurement for the street furniture (`systems::street_furniture`):
    /// 0 = benches and bins standing individually, 1 = merged into the paving and
    /// hidden. Its band is 150–450 m, an order of magnitude below the trees', so
    /// the two columns move at completely different points in a climb.
    pub furniture_factor: f32,
    pub globe_entities: usize,
    pub globe_visible: usize,
    pub globe_rendered: usize,
    /// The quadtree's own books: tiles it holds, and tiles the last selection
    /// wanted. `globe_entities` above counts `GlobeMesh` entities, which also
    /// includes the backstop sphere and anything still draining the despawn
    /// queue — so the two disagree, and the gap between *these* two is how much
    /// of the pyramid is still arriving.
    pub globe_tiles_loaded: usize,
    pub globe_tiles_desired: usize,
    /// Resident tile-texture megabytes, and the edge length they are rasterised
    /// at (`globe::GLOBE_TILE_PX`), so a CSV says which build produced it.
    ///
    /// This is the column that separates the two candidate causes of the black
    /// artifacts at altitude (`docs/notes/globe-tile-cost.md`): if they track
    /// this, it is memory pressure; if they track `alt_m` while this stays flat,
    /// it is the depth range. A `--frames` sweep now answers that in one run.
    pub globe_texture_mb: f32,
    pub globe_tile_px: u32,
    /// Grass patch entities standing, and the blade slots they add up to
    /// (`systems::grass`).
    ///
    /// Read them together, and read them against `grass_slots` in particular:
    /// this layer's failure mode is not "no entities" but "entities whose every
    /// blade collapsed", because a blade that fails the coverage mask is a
    /// zero-height blade rather than an absent one. So `grass_patches > 0` with
    /// a bare-looking capture means the *mask* is wrong (misplaced, unbuilt, or
    /// still on a worker) — which is a different bug from the pool never having
    /// been spawned, and nothing else here can tell the two apart.
    pub grass_patches: usize,
    pub grass_slots: u32,
    /// How many of those patches the view actually drew. The third number is what
    /// makes the pair above diagnostic rather than merely reassuring: the pool is
    /// laid out around the camera's *ground* position, so from any height it sits
    /// below the frame and is frustum-culled — `45 / 0` is that, and it is not a
    /// bug. `n / n` with a bare lawn is the mask; `0 / 0` is the pool.
    pub grass_rendered: usize,
    /// BAG records currently indexed (`systems::building_facts`): buildings,
    /// addressed units, and houseboat/mobile-home berths.
    ///
    /// The layer's failure mode is silence — a host with no `bag/` tree, a
    /// coverage manifest that never loaded, or a cell ring that never overlapped
    /// the covered cells all produce a *correct-looking* city with the generated
    /// palette and the hashed window lighting. `bag_buildings == 0` over
    /// Groningen is the only thing that tells those apart from "the facts landed
    /// and made no difference", and no capture can show it.
    pub bag_buildings: usize,
    pub bag_addresses: usize,
    pub bag_berths: usize,
    /// Surveyed BGT objects standing in the loaded cells: trees, then street
    /// objects (`systems::surveyed`). Same reason as the BAG counts above — the
    /// layer fails by being silently absent, and a street with scattered canopies
    /// and no lighting is exactly what a working map looks like.
    pub surveyed_trees: usize,
    /// Trees a **city's own** register put in the loaded cells
    /// (`systems::municipal`). Counted apart from `surveyed_trees` and not added
    /// to it: over a cell a city covers, the BGT's trees are not read at all, so
    /// one number could not tell a city that answered from a city with no
    /// register whose BGT cell happens to be full.
    pub municipal_trees: usize,
    /// Street objects a city's own register put in the loaded cells. Counted
    /// apart from `surveyed_props` for the same reason as the trees: over ground
    /// a city covers, the BGT's copies of the kinds it supplies are not read.
    pub municipal_props: usize,
    pub surveyed_props: usize,
    /// Surveyed fences, walls and hedges in the loaded cells.
    pub surveyed_lines: usize,
    /// Ground-floor rooms standing near the avatar (`systems::interiors`). A door
    /// with no room behind it is a hole in the world, so this is read against the
    /// doors: zero here with a building in front of you means walking in would
    /// put you inside a shell whose walls are culled away.
    pub interior_rooms: usize,
    /// Storeys standing across those rooms (`interiors::InteriorFloors::storeys`).
    /// Equal to `interior_rooms` means every building was found too short, or too
    /// small to hold a staircase, to be divided — which is exactly what this layer
    /// did before it could count storeys, so the two readings together are what
    /// say whether the division is live at all.
    pub interior_storeys: usize,
    /// Of `interior_rooms`, those built from a design somebody drew in the atlas
    /// editor rather than the generated one (`systems::building_designs`).
    pub interior_designed: usize,
    /// The storey a building is cut away at (`systems::cutaway`), or -1 when
    /// nothing is. With the avatar indoors this is the storey it stands on.
    pub cutaway_storey: i32,
    /// Bores standing in the streamed cells (`systems::tunnels`).
    ///
    /// The one number that separates the three ways a capture over a tunnel comes
    /// back looking empty: no tunnel in the tile at all, a tunnel that was built
    /// but is roofed over from this viewpoint, and a tunnel whose mesh failed. It
    /// counts the *tunnels* — the floor-and-walls description the avatar walks in —
    /// rather than meshes, because that is the half a picture cannot show.
    pub tunnel_count: usize,
    /// How deep the deepest streamed carriageway runs under its land, metres
    /// (negative). Zero with no tunnel in range; about −7 over a Dutch sunken road.
    ///
    /// **A minimum over the whole map, so it says nothing about this view.** It
    /// reads −7.0 wherever one tunnel anywhere in the streamed cells reaches full
    /// depth — including from a camera pointed at an ordinary street — which is
    /// how a capture came to be reported as showing a trench when it showed a
    /// surface road beside one. Use [`Self::tunnel_ahead_m`] to say what the
    /// picture is of; this one only says a tunnel was built somewhere.
    pub tunnel_road_m: f32,
    /// The tunnels' own meshes — carriageway, walls and roof — spawned, and how
    /// many of those the camera can actually see.
    ///
    /// **Two numbers for the reason the route line has two.** A tunnel is
    /// invisible when it works and invisible when it does not: from anywhere but
    /// a low oblique over an open cut, ground is what you see either way. So
    /// `tunnel_count` — which counts the floor-and-collision description, not
    /// triangles — cannot distinguish "no tunnel here", "built but off screen"
    /// and "built, on screen, and drawn under something". These can.
    pub tunnel_meshes: usize,
    /// See [`Self::tunnel_meshes`].
    pub tunnel_rendered: usize,
    /// Where the nearest tunnel goes under, and the compass heading of the road
    /// there — ready to paste into `--at` and `--look`.
    ///
    /// Zero and `-1` when no tunnel is streamed. See
    /// `Tunnels::nearest_entrance`: eleven cameras in this feature's history
    /// were placed by hand-converting the tessellator's own coordinates and every
    /// one of them missed, by between 28 and 178 metres.
    pub tunnel_entrance_lat: f64,
    /// See [`Self::tunnel_entrance_lat`].
    pub tunnel_entrance_lng: f64,
    /// See [`Self::tunnel_entrance_lat`]. Degrees clockwise from north; `-1` when
    /// there is no tunnel.
    pub tunnel_entrance_heading: f32,
    /// Metres from the point this capture is aimed at to the nearest tunnel's
    /// carriageway, in plan. `0` means the aim point is over one; `-1` means
    /// nothing is streamed or the camera is not pointed at the ground.
    ///
    /// This is the field that makes a tunnel capture self-checking. A road seen
    /// from a low oblique is a band a few pixels deep whether it is on the ground
    /// or seven metres under it, and a tunnel approach runs beside ordinary
    /// streets by construction — so the eye cannot tell a picture *of* a tunnel
    /// from a picture pointed fifty metres off one, and twice now it has not.
    pub tunnel_ahead_m: f32,
    /// How far the carriageway at that nearest tunnel runs under its land, metres
    /// (negative).
    ///
    /// Read beside [`Self::tunnel_ahead_m`]: `ahead_m` near 0 with `ahead_deck_m`
    /// near −7 is a capture looking into a full-depth cut, and is the assertion a
    /// proof shot of a tunnel should carry.
    pub tunnel_ahead_road_m: f32,
    /// Clipmap tiles whose raster sheet has an opening cut out of it
    /// (`map_stream::BasemapHoles`).
    ///
    /// The vector ground and the basemap are two sheets a few centimetres apart,
    /// and a capture cannot tell which of them is roofing a tunnel — which is
    /// precisely the question that went a day unanswered. Zero here with
    /// `tunnel_count` above zero means the trench is being photographed through a
    /// hole in only one of the two.
    pub basemap_open_tiles: usize,
    /// Doors drawn on facades from the register's entrance points
    /// (`lod22::DoorCount`).
    pub building_doors: usize,
    /// Buildings drawn with a scaffold because the register says work is going
    /// on at them (`systems::scaffolding`).
    pub building_scaffolds: usize,
    /// Buildings drawn behind a demolition fence (`systems::scaffolding`).
    pub building_fences: usize,
    /// Whether a building is selected, and whether the avatar is standing inside
    /// one (`systems::building_select`).
    ///
    /// Both are the assertable half of a feature whose visible half is a pane of
    /// tinted glass — and colour is not evidence on this container's software
    /// rasteriser (`docs/notes/headless-shots-software-renderer.md`). `--ui
    /// building` selects the nearest building so a capture has something to
    /// photograph; `building_indoors` is what says the avatar got through the
    /// door, which is the thing that used to be impossible.
    pub building_selected: bool,
    pub building_indoors: bool,
    /// Glass shells standing (`systems::building_select::BuildingShell`) and how
    /// many the view actually rendered.
    ///
    /// Two numbers for the reason `route_meshes`/`route_rendered` are two: a shell
    /// that is built but not rendered is a visibility or culling problem, one that
    /// is rendered but unseen is a depth, alpha or placement problem — and a
    /// translucent pane on a software rasteriser is exactly the thing a capture
    /// cannot settle on its own. Without these, "I see no glass" and "the glass
    /// was never built" are the same picture, which is how the door work lost an
    /// afternoon to camera angles.
    pub building_shell_meshes: usize,
    pub building_shell_rendered: usize,
    /// Flood-defence pieces streamed in (`systems::dikes`), and how many of them
    /// have had their crest measured against the fine height model.
    ///
    /// Two numbers rather than one because they fail separately and look the
    /// same: `flood_defences` at zero means the register never arrived, while
    /// `flood_defences_measured` at zero with the first non-zero means the names
    /// are being laid on the coarse terrain field — which over a sea dike is
    /// about two metres inside the embankment. The second stays zero with the
    /// Height layer off, and that is correct: the world is flat then.
    pub flood_defences: usize,
    pub flood_defences_measured: usize,
    /// Sewer pipes drawn, and the meshes they are drawn in (`systems::sewers`,
    /// `--sewers`). Zero pipes with the layer on over a covered city means the
    /// cells never arrived; pipes with zero meshes cannot happen, and if it does
    /// the build dropped them.
    pub sewer_pipes: usize,
    pub sewer_meshes: usize,
    /// Underground shapes drawn across the other underground layers
    /// (`systems::underground`, `--underground`), and their meshes.
    pub underground_shapes: usize,
    pub underground_meshes: usize,
    /// Surveyed paved surfaces — pavements, cycle paths, parking bays, verges.
    /// Zero over a covered city means the paving never arrived, and the street
    /// then looks exactly like the one this client always drew.
    pub surveyed_paving: usize,
    /// Registered crop parcels in the loaded cells (`systems::crops`). Same
    /// failure mode as every count above it, and a worse one to see: a field
    /// drawn in the generic green is exactly what the map looked like before the
    /// register existed, so zero over farmland is the only thing that tells
    /// "the parcels never arrived" from "this ground has none".
    pub crop_parcels: usize,
    /// Buoys and beacons standing in the loaded cells (`systems::nav_marks`).
    ///
    /// The one number that tells the layer apart from its own absence, and it
    /// takes four gates to get here: the register's envelope, the cell's water,
    /// the Furniture toggle and the request itself. Empty water is what this
    /// client always drew, so a capture of the Waal cannot say which of the four
    /// answered no.
    pub nav_marks: usize,
    /// Installations standing on ProRail's registered level crossings
    /// (`systems::crossings`).
    ///
    /// Four gates again — the register's envelope, the cell's railway, the
    /// Furniture toggle and the request — plus a fifth this layer has and the
    /// marks do not: a crossing whose road the tile does not carry is dropped
    /// rather than oriented at a guess, so this counts what is on screen rather
    /// than what came back from the register. A railway with nothing beside it is
    /// what this client always drew, so a capture over a level crossing cannot
    /// say which of the five answered no.
    pub level_crossings: usize,
    /// Collider circles the streamed world publishes — every prop, parked
    /// vehicle and crossing mast in `WorldPlaces::props()`, i.e. what
    /// `collision::sync_prop_colliders` is about to hand the avatar.
    ///
    /// The collider index is invisible to a capture, which is how a settled shot
    /// with `level_crossings = 3` on screen photographed three installations
    /// nothing could walk into. Counted here because this is the only half of
    /// "drawn *and* solid" a headless run can assert: read against
    /// `level_crossings`, it separates the two.
    ///
    /// It is a count of the whole index, not of the crossings' share — a per-layer
    /// tally would need the store to remember where each circle came from, which
    /// nothing else needs.
    pub prop_colliders: usize,
    /// Drawn buildings whose BAG facts were found, and drawn buildings. See
    /// `lod22::DatedCount`: a ratio near zero over the Netherlands means the join
    /// is broken — most likely the served meshes and the client disagree about
    /// what an id is — and the map looks entirely normal while it is.
    pub lod22_dated: usize,
    pub lod22_elements: usize,
    /// Mesh entities spawned by the Overpass street-detail layer
    /// (`systems::street_detail`) — surveyed trees, lamps, signals, barriers,
    /// piers, paint. `0` over a city with the layer on means the cells never
    /// arrived (Overpass, cache, or the toggle), which a capture alone cannot
    /// tell apart from "this town has none of it mapped".
    pub prop_meshes: usize,
    /// Ambient agents currently **placed on a line** — cars and people, counted
    /// apart (`systems::traffic`).
    ///
    /// Placed, **not** on screen: the test is `Visibility != Hidden`, which an
    /// agent behind a building or beyond the far plane still passes. That is the
    /// useful question, because the pools are a fixed size and spawn hidden, so a
    /// raw entity count reads the same whether the layer is working or never
    /// found a road. `traffic_cars == 0` over a city with the toggle on means the
    /// network never arrived or the pool was never created — which is exactly the
    /// failure that hid a missing car pool once already, silently.
    ///
    /// What it cannot tell you is whether any of them are in *frame*; a full
    /// count with an empty-looking street means the pool is spread too thin, not
    /// that it is broken.
    pub traffic_cars: usize,
    pub traffic_walkers: usize,
    /// Drawn-route entities (ADR-037) and how many the view actually rendered.
    /// The same "black image vs black scene" question the globe counts answer: a
    /// route that is built but not rendered is a visibility or culling problem,
    /// one that is rendered but unseen is a depth or colour problem.
    pub route_meshes: usize,
    pub route_rendered: usize,
    /// The three components light shafts need, counted separately, because two
    /// of them draw nothing on their own (`systems::light_shafts`).
    ///
    /// `shaft_fog` is the camera's `VolumetricFog` — the pass. `shaft_volumes` is
    /// the `FogVolume` count — the only place the pass has any effect.
    /// `shaft_volume_m` is how far the volume's centre is from the camera in
    /// render space, which is the number that would have saved a day: a volume
    /// the viewer is not inside contributes *nothing at all*, because bevy's
    /// march starts `end_depth_view` at 0 and only raises it on a backface hit,
    /// so the shader returns `vec4(0)` and the setting reads as on while doing
    /// nothing. Expect ~0; a whole grid cell means the placement is wrong.
    ///
    /// All three because the failure is a missing *combination*: on a capture
    /// where the shafts are invisible, `shaft_fog=false` is a settings answer,
    /// `shaft_volumes=0` is a spawn answer, and a large `shaft_volume_m` is a
    /// placement answer, and no single boolean separates them.
    ///
    /// With no volume at all the distance is `NaN`, which serialises to null and
    /// reads as *not measured* — the same convention `ground_m` uses off the
    /// height layer, and the honest answer to "how far away is a thing that does
    /// not exist". Assert `shaft_volumes` first if you are gating on it.
    pub shaft_fog: bool,
    pub shaft_volumes: usize,
    pub shaft_volume_m: f32,
    /// What `AppPrefs` *decided*, beside what the scene got.
    ///
    /// `shaft_pref` is `AppPrefs::light_shafts_on()` and `shadows_pref` is the
    /// `shadows_on()` that gates it. Reported next to [`Self::shaft_fog`] because
    /// the three of them localise the failure to one line: pref on and fog off is
    /// `render_features`; pref off with shadows on is the capability half; both
    /// off is the shadow setting, and the shafts are a symptom rather than the
    /// bug. Without these, "the shafts are not there" is four possible causes and
    /// a capture that cannot distinguish them.
    pub shaft_pref: bool,
    pub shadows_pref: bool,
    /// Particle emitters drawing, and the quads they add up to
    /// (`systems::particles`).
    ///
    /// The geometry half of "is the fire there?", and on this container it is the
    /// *only* half: colour, blending and brightness are not evidence on a
    /// software rasteriser (`docs/notes/headless-shots-software-renderer.md`), so
    /// a capture proves the pipeline built and the emitter exists, and a person
    /// on a workstation still has to say whether it looks like fire. Zero
    /// emitters on a prim that carries one is the same class of finding
    /// `avatar_meshes == 0` was.
    pub particle_emitters: usize,
    pub particle_quads: usize,
    /// Mesh entities under the local avatar, and how many the view rendered.
    ///
    /// The same split the route and globe counts make, added because the avatar
    /// is where that split had never been instrumented and the cost of that was a
    /// year: `avatar_asset_exists` probed a *relative* `assets/` path, so on
    /// Android, on the web and in any binary started outside the repo root every
    /// character style read as absent, `resolve_avatar_holders` returned nothing,
    /// and the avatar spawned with **no meshes at all**. Nothing failed. Follow
    /// mode was offered, the camera followed, the avatar walked and collision
    /// blocked it, because every one of those hangs off the `Avatar` entity
    /// rather than off its model. `avatar_meshes == 0` is exactly that state, and
    /// it is the one number that could have said so.
    ///
    /// So read them as a pair: `0/0` is "never spawned" (an asset or appearance
    /// problem), `n/0` is "spawned but not drawn" (culling, visibility or the
    /// slot mask). Assert them with `--shot-mode avatar`, which is the only mode
    /// that frames the avatar — in free/earth the camera sits at `--at` while the
    /// avatar stands at the map anchor, so a zero there means "out of shot" and
    /// proves nothing.
    pub avatar_meshes: usize,
    pub avatar_rendered: usize,
    /// The floor the avatar is standing on, metres relative to the ground under
    /// it: a building's storey above (`systems::interiors`), a tunnel's
    /// carriageway below (`systems::tunnels`), 0 outdoors at ground level.
    ///
    /// This is the only way a headless run can say the avatar is *in* the tunnel
    /// rather than over it. A picture cannot: from the deck and from the pavement
    /// seven metres up, the same tunnel fills the frame with the same asphalt.
    pub avatar_floor_m: f32,
    /// Measured ground level at the map anchor, metres above NAP, and how many
    /// patches the height layer has fetched (`systems::height`). `NaN` and 0 mean
    /// the layer is off, out of AHN's coverage, or has not answered yet — which is
    /// the whole question you ask of a layer that only works over one country.
    pub ground_m: f32,
    pub height_patches: usize,
    /// Whether the terrain field has baked (`systems::terrain`), and the relief it
    /// found over its 6.4 km box, in metres. Together they separate the three ways
    /// a flat-looking capture can be flat: no field at all (layer off, outside
    /// AHN, still fetching), a field over genuinely flat ground (a polder reads
    /// ~2 m), and a field with relief in it that something downstream isn't using.
    pub terrain_ready: bool,
    pub terrain_relief_m: f32,
    /// What the field actually samples at, in metres.
    ///
    /// Assertable, because relief is a *gradient* and a gradient is a function of the
    /// spacing: over a Dutch city the same ground reads a 0.46° median slope at 25 m
    /// and 1.34° at 3.125 m, which is the difference between shading the ground within
    /// ±4 % of flat and ±20 %. `terrain_relief_m` cannot see that — it is the range
    /// over the whole box and barely moves — so a capture that wants to state the
    /// ground has slopes in it has to assert this.
    pub terrain_sample_m: f32,
    /// Samples of the height field the roads have been graded flat over
    /// (`terrain::RoadMask`).
    ///
    /// Assertable, and the only thing that is: the fault it answers is a ragged
    /// ridge standing along every road, which is a *shading* difference on ground
    /// that is a metre out — so on a software rasteriser the picture is not
    /// evidence and this is. `0` out of AHN, with the layer off, or before the
    /// first cell's ribbons have landed.
    pub terrain_graded_samples: usize,
    /// Samples pinned at a viaduct's abutment, and samples the approach
    /// embankment raised — `terrain`'s approach grading.
    ///
    /// **Because an embankment is not something a capture can assert.** It is
    /// 150 m of ground rising six metres, which from any angle that also shows
    /// the viaduct reads as ground. A zero here with decks standing is the exact
    /// failure this feature had for a month: the grading existed and nothing in
    /// the client called it.
    pub approach_abutments: usize,
    pub approach_raised: usize,
    /// Raised decks standing, how many have no approach at all, and how far a
    /// deck's drawn top is from the ground three metres out along its approaches
    /// — the largest and the mean (`DeckPlates::approach_steps`). A cliff at the
    /// end of an embankment is a large `deck_step_max_m`, which no view shows.
    pub deck_count: usize,
    pub decks_unapproached: usize,
    pub deck_step_max_m: f32,
    pub deck_step_mean_m: f32,
    /// Of those decks, how many stand along a measured profile rather than at
    /// one height (`cartopolis_geo::deck_profile`), and how many surveyed piers
    /// were stood under them.
    pub deck_profiled: usize,
    pub deck_piers: usize,
    /// Whether the half-metre tier under the camera has baked
    /// (`terrain::update_fine_terrain`), and what it actually samples at.
    ///
    /// Two fields because they answer different questions, and the second catches a
    /// silent regression: a capture reporting `fine_terrain_ready=true` with
    /// `fine_terrain_sample_m` near 25 has a bug no picture on a software rasteriser
    /// would show.
    pub fine_terrain_ready: bool,
    pub fine_terrain_sample_m: f32,
    /// The sub-metre ground layer (`systems::ground_detail`): whether the shared slope
    /// texture is bound, and what one texel of it spans.
    ///
    /// Assertable for the reason `terrain_sample_m` is, only more so: this one is a
    /// *shading* term, and colour and lighting are not evidence on the container's
    /// software rasteriser. `0.0` with the layer stripped
    /// (`CARTO_NO_GROUND_DETAIL=1`), out of AHN, or before the tier lands.
    pub ground_detail_ready: bool,
    pub ground_detail_sample_m: f32,
    /// Registered water bodies loaded (`systems::water_levels`), the terrain samples the
    /// two bakes rewrote to a measured level, and how far below the world's zero the
    /// deepest registered water sits.
    ///
    /// Four numbers because they fail in different places, and telling those apart from
    /// outside is otherwise impossible: `water_rings > 0` with `water_carved_coarse == 0`
    /// is the register arriving but the carve not reaching the field the basemap is
    /// displaced by; a large `water_drop_m` with nothing visible is the carve working and
    /// something downstream hiding it. Finding that distinction by eye cost a day.
    pub water_rings: usize,
    pub water_carved_coarse: usize,
    pub water_carved_fine: usize,
    pub water_drop_m: f32,
    /// The largest single frame's worth of GPU uploads over the whole run, in kB
    /// (`systems::upload_meter`).
    ///
    /// A run-wide high-water mark rather than a reading at the shutter, and that
    /// is the point: by the time a capture settles, the streaming burst that would
    /// have cost a handset its device is minutes past, and every other column here
    /// describes the calm after it. This is the only column that remembers the
    /// worst moment.
    ///
    /// Machine-independent — it counts bytes the client queued, which is the same
    /// arithmetic on lavapipe as on an Adreno — so unlike the frame times it is a
    /// column `--expect` may gate on.
    pub upload_peak_kb: usize,
    /// The largest *single* asset the run queued, in kB.
    ///
    /// The column to gate on, and the one [`crate::systems::lod22::MAX_CELL_MESH_BYTES`]
    /// is answerable for: a frame's total can be spread by bevy's upload budget,
    /// and a single asset cannot be — it is written whole or not at all. Before the
    /// LoD2.2 cells were cut into pieces this read 64,892 over Groningen; the
    /// ceiling is 8,192 now.
    pub upload_peak_asset_kb: usize,
    /// What the streamed content **occupies** at the shutter, in kB — the tiles,
    /// cells, buildings and ribbons this client has created and not dropped.
    ///
    /// The columns above are a *rate* and this is the *total*, and a settled view
    /// is exactly where the difference shows: the burst is minutes past and this is
    /// still being held. It is what a driver refuses when it refuses an allocation,
    /// so it is the column to compare between two rungs — `CARTO_QUALITY=android`
    /// against `CARTO_QUALITY=phone` over the same viewpoint answers "what does the
    /// handset's tile raster actually cost" in one number.
    ///
    /// Machine-independent, like the two above, and for the same reason: it counts
    /// asset bytes, not anything the adapter did with them. So `--expect
    /// 'gpu_resident_kb<=...'` is a legitimate gate.
    pub gpu_resident_kb: usize,
    /// The most it ever occupied during the run, in kB. A capture that peaks well
    /// above where it settles has still asked the driver for the peak.
    pub gpu_resident_peak_kb: usize,
    /// Ground tiles this run rebuilt from system memory rather than from the
    /// tile server and tiny-skia (`platform::warm`), and the megabytes that
    /// store is holding at the shutter.
    ///
    /// Machine-independent — a count of cells and a count of bytes, neither of
    /// which the adapter touches — so both may be gated. The pair is what says
    /// whether a multi-viewpoint run was warm: a `cargo shots` manifest crosses
    /// the same city six times, and `warm_tile_hits` distinguishes "the second
    /// shot settled quickly" from "the second shot settled quickly *because* it
    /// re-rasterised nothing".
    pub warm_tile_hits: u32,
    pub warm_tile_mb: f32,
    /// The same pair for the 3D buildings (`systems::lod22`), whose retained
    /// form is graphics memory rather than system memory.
    ///
    /// `warm_cell_mb` is part of `gpu_resident_kb`, not extra to it — these are
    /// live assets that nothing is drawing — so the two read together say how
    /// much of a capture's residency is the return-journey pool. 0 on every
    /// handset, where the pool is switched off outright.
    pub warm_cell_restores: u32,
    pub warm_cell_mb: f32,
    /// The streaming account for this capture's window (placement → settle
    /// verdict): HTTP requests completed, errors, body megabytes received, and
    /// the summed time to first byte / body-read time (`platform::http::stats`).
    ///
    /// The two duration columns are the split `docs/notes/tile-raster-cost.md`
    /// records as *the* unmeasured number in the HTTP/2-vs-bandwidth question:
    /// TTFB is what a protocol or connection-pool change moves, body time is
    /// what only fewer bytes move. They are sums over concurrent requests, so
    /// compare them to each other and between paired runs — never to the
    /// wall-clock `settled_s`. Counts and bytes are machine-independent and may
    /// be gated; the durations are wall-clock and may not.
    pub fetch_count: u64,
    pub fetch_errors: u64,
    pub fetch_mb: f32,
    pub fetch_ttfb_ms: u64,
    pub fetch_body_ms: u64,
    /// The worker pool's account for the same window (`platform::workers::stats`),
    /// summed over every stream: jobs finished, time spent inside them, and time
    /// they sat queued. Busy says what the decodes cost; **wait** is the
    /// starvation number — a burst shows up as seconds of wait against
    /// milliseconds of busy, which no instantaneous queue depth can show.
    /// Wall-clock, so paired-run material, not a gate. `jobs` may be gated.
    pub worker_jobs: u64,
    pub worker_busy_ms: u64,
    pub worker_wait_ms: u64,
    /// The same account per stream, `label:jobs:busy_ms:wait_ms` — the
    /// attribution that turns "the pool was busy for 40 s" into "lod22 was".
    /// A list like `busy`, so it rides the CSV as one `|`-joined cell and is
    /// excluded from the `--expect` vocabulary.
    pub worker_split: Vec<String>,
    /// How wide the tile worker pool is (`platform::workers::worker_count`).
    ///
    /// On record because every reading above is a *sum over threads* and means
    /// nothing without it: 32 s of busy is saturation on one thread and idleness
    /// on eight. It is also the one number here that the machine chooses rather
    /// than the scene, so a pair of runs that disagree on it are not comparable.
    pub worker_threads: usize,
    /// Fraction of the pool's available thread-time actually spent inside jobs,
    /// over the same window — `busy / (threads × window)`.
    ///
    /// **This is the compute-bound-or-starved question, and nothing else here
    /// answers it.** `worker_busy_ms` says what the decodes cost and
    /// `worker_wait_ms` says that jobs queued, but a burst that queues behind a
    /// saturated pool and a burst that queues waiting for its bytes to arrive
    /// produce the same large wait. Utilisation separates them: near 1.0 the
    /// cores are the wall and making the work cheaper is the only lever; low,
    /// with a large wait, the pool is starved and the fill is bound by fetches
    /// or by dispatch order, where cheaper work buys nothing.
    ///
    /// Measured 2026-09-22 on a Steam Deck (8 threads), Groningen at 3 km on the
    /// handset rung: **0.13**, i.e. 31.7 s of work inside 247 thread-seconds. The
    /// fill was waiting, not computing — which is the opposite of what the
    /// per-cell costs alone suggest.
    ///
    /// Wall-clock-derived, so paired-run material and never a gate.
    pub worker_utilisation: f32,
    /// The capture ran on a CPU rasteriser (Mesa lavapipe in a container or CI)
    /// rather than a GPU. Geometry, placement, culling and UI layout are the same
    /// picture either way; the tone-mapped sky is not, so a colour or lighting
    /// judgement made against a shot with this set is not evidence.
    pub software_renderer: bool,
    /// What wgpu's own allocator has *in use*, in kB — every GPU allocation this
    /// process holds, not just the streamed assets [`Self::gpu_resident_kb`]
    /// counts: render targets, shadow maps, atmosphere LUTs, pipelines.
    ///
    /// `0` when the backend keeps no report.
    pub vram_allocated_kb: u64,
    /// What wgpu has **reserved from the driver** for that, slack included, in kB.
    ///
    /// This is the number a driver refuses on, and the pair is a diagnosis rather
    /// than a statistic. Reserved tracking allocated means the client is asking
    /// for what it gets; reserved standing well above it means the allocator is
    /// holding memory the app does not have, which is the shape of
    /// [bevyengine/bevy#24722] — the `gpu-alloc` → `gpu-allocator` switch in wgpu
    /// 28, reported as OOM and lost devices on desktop Vulkan.
    ///
    /// Backend-dependent, unlike [`Self::gpu_resident_kb`], so it is *not* a
    /// legitimate `--expect` gate across machines. It is here to be compared
    /// against `vram_allocated_kb` in the same run.
    ///
    /// [bevyengine/bevy#24722]: https://github.com/bevyengine/bevy/issues/24722
    pub vram_reserved_kb: u64,
}

/// The `--csv` column order, and the spine the rest of the harness's introspection
/// hangs off: [`ShotMetrics::csv_row`] renders a row by looking each of these names
/// up in the struct, and [`metric_keys`] derives the `--expect` vocabulary from the
/// same list. So this is the one place a column's *name* and *position* are stated,
/// and adding a metric here is what makes it reachable from a CSV and from a gate on
/// the same commit.
///
/// The order is deliberately not the struct's declaration order (related columns
/// were grouped as they were added), which is exactly why a row must be built by
/// name rather than by position.
const CSV_HEADER: &str = "shot,name,lat,lng,camera_lat,camera_lng,aim_lat,aim_lng,settled,settled_s,busy,frame_ms,frame_ms_max,flicker_px,\
draws,tris_total,tris_shadow,shadow_casters,tris_buildings,tris_lod22,tris_tiles,tris_globe,worker_outstanding,fetch_outstanding,radius_bias,\
tiles_occluded,aerial_tiles,\
alt_m,space_factor,in_space,\
orbit_space,near_m,far_m,exposure_ev100,bloom,globe_albedo,veg_factor,furniture_factor,globe_entities,\
globe_visible,globe_rendered,globe_tiles_loaded,globe_tiles_desired,globe_texture_mb,globe_tile_px,\
route_meshes,route_rendered,shaft_fog,shaft_volumes,shaft_volume_m,shaft_pref,shadows_pref,\
particle_emitters,particle_quads,\
grass_patches,grass_slots,grass_rendered,\
bag_buildings,bag_addresses,bag_berths,surveyed_trees,municipal_trees,surveyed_props,municipal_props,surveyed_lines,surveyed_paving,crop_parcels,nav_marks,level_crossings,prop_colliders,flood_defences,flood_defences_measured,sewer_pipes,sewer_meshes,underground_shapes,underground_meshes,tunnel_count,tunnel_road_m,tunnel_meshes,tunnel_rendered,tunnel_ahead_m,tunnel_ahead_road_m,tunnel_entrance_lat,tunnel_entrance_lng,tunnel_entrance_heading,basemap_open_tiles,interior_rooms,interior_storeys,interior_designed,cutaway_storey,building_doors,building_scaffolds,building_fences,building_selected,building_indoors,building_shell_meshes,building_shell_rendered,\
lod22_dated,lod22_elements,prop_meshes,\
traffic_cars,traffic_walkers,\
avatar_meshes,avatar_rendered,avatar_floor_m,\
ground_m,height_patches,\
terrain_ready,terrain_relief_m,terrain_sample_m,terrain_graded_samples,\
approach_abutments,approach_raised,deck_count,decks_unapproached,deck_step_max_m,deck_step_mean_m,deck_profiled,deck_piers,\
fine_terrain_ready,fine_terrain_sample_m,\
ground_detail_ready,ground_detail_sample_m,water_rings,\
water_carved_coarse,water_carved_fine,water_drop_m,upload_peak_kb,upload_peak_asset_kb,gpu_resident_kb,\
gpu_resident_peak_kb,warm_tile_hits,warm_tile_mb,warm_cell_restores,warm_cell_mb,software_renderer,vram_allocated_kb,vram_reserved_kb,\
fetch_count,fetch_errors,fetch_mb,fetch_ttfb_ms,fetch_body_ms,\
worker_jobs,worker_busy_ms,worker_wait_ms,worker_threads,worker_utilisation,worker_split";

/// The avatar's contribution to [`ShotMetrics`].
///
/// Defined in [`crate::systems::avatar`] rather than here, because `--shot` is no
/// longer its only reader: the same count is logged on every platform by
/// `avatar::log_avatar_draw_counts`, which is the only form of it that reaches a
/// phone (a scripted capture needs a CLI, and Android has none — see
/// `docs/notes/android-build.md`). One implementation, two sinks, on the same
/// rule as every other `ShotMetrics` field.
use crate::systems::avatar::AvatarReadout;

impl ShotMetrics {
    /// One metric by name, as a number — booleans as 1/0. Read back out of the
    /// `Serialize` impl rather than a hand-written match, which cannot drift from
    /// the struct. `None` means the field is not a number (`name`, `busy`) or was
    /// serialised as null, which is what a `NaN` float becomes.
    fn measure(&self, key: &str) -> Option<f64> {
        match serde_json::to_value(self).ok()?.get(key)? {
            serde_json::Value::Bool(b) => Some(if *b { 1.0 } else { 0.0 }),
            serde_json::Value::Number(n) => n.as_f64(),
            _ => None,
        }
    }

    /// One CSV line, columns in [`CSV_HEADER`] order — each cell **looked up by
    /// the header's own name** out of the `Serialize` impl, exactly as
    /// [`Self::measure`] does.
    ///
    /// This was a positional `format!` with 54 placeholders and 54 `self.` field
    /// expressions until 2026-08-21, and the danger in that is not the length: the
    /// header, the placeholders and the arguments were three hand-maintained lists
    /// that had to agree on *order*, while the only test over them compared their
    /// *arity*. Inserting a column in the middle of the header without moving its
    /// argument to match still had 54 of each, so it passed — and silently shifted
    /// every column after it, mislabelling the rest of the file. Looking each cell
    /// up by name cannot do that; a header naming a field that does not exist is
    /// what `csv_columns_all_resolve` now catches instead.
    fn csv_row(&self) -> String {
        let value = serde_json::to_value(self).unwrap_or(serde_json::Value::Null);
        CSV_HEADER
            .split(',')
            .map(|key| csv_cell(key, value.get(key)))
            .collect::<Vec<_>>()
            .join(",")
    }
}

/// Decimal places for the columns where a raw `f32`-widened-to-`f64` would be
/// noise (`0.30000001192092896`). A column that isn't listed prints as
/// serialised, which is what every integer and boolean wants.
///
/// Unlike the positional format string this replaced, a name missing from here
/// costs a column some readability — never its label.
const CSV_PRECISION: &[(&str, usize)] = &[
    ("settled_s", 2),
    ("worker_utilisation", 3),
    ("warm_tile_mb", 1),
    ("warm_cell_mb", 1),
    ("frame_ms", 2),
    ("frame_ms_max", 2),
    ("space_factor", 4),
    ("exposure_ev100", 3),
    ("bloom", 4),
    ("globe_albedo", 4),
    ("veg_factor", 4),
    ("furniture_factor", 4),
    ("globe_texture_mb", 2),
    ("avatar_floor_m", 2),
    ("tunnel_road_m", 2),
    ("tunnel_ahead_m", 1),
    ("tunnel_ahead_road_m", 2),
    ("ground_m", 2),
    ("terrain_relief_m", 2),
    ("terrain_sample_m", 2),
    ("terrain_graded_samples", 0),
    ("deck_step_max_m", 2),
    ("deck_step_mean_m", 2),
    ("approach_abutments", 0),
    ("approach_raised", 0),
    ("fine_terrain_sample_m", 2),
    ("ground_detail_sample_m", 2),
    ("water_drop_m", 2),
    ("shaft_volume_m", 1),
    ("fetch_mb", 2),
];

/// Render one serialised metric as a CSV cell.
///
/// `busy` is `|`-joined for the obvious reason. A **null is an empty cell**, and
/// that covers the two ways one arises with the same answer: an unnamed shot has
/// no preset name, and a `NaN` float — which is what `ground_m` is when the height
/// layer is off or outside AHN's coverage — serialises to null too. Empty is the
/// honest cell for both, and it agrees with [`ShotMetrics::measure`], which
/// already reports a `NaN` metric as *not measured* rather than as a number.
fn csv_cell(key: &str, value: Option<&serde_json::Value>) -> String {
    match value {
        Some(serde_json::Value::String(s)) => s.clone(),
        Some(serde_json::Value::Array(items)) => items
            .iter()
            .map(|v| v.as_str().unwrap_or_default())
            .collect::<Vec<_>>()
            .join("|"),
        Some(serde_json::Value::Number(n)) => match CSV_PRECISION.iter().find(|(k, _)| *k == key) {
            Some((_, places)) => match n.as_f64() {
                Some(f) => format!("{f:.places$}"),
                None => n.to_string(),
            },
            None => n.to_string(),
        },
        // Null, or a header naming a field that isn't there — the latter is a bug,
        // and `csv_columns_all_resolve` is what fails on it. Matched before the
        // catch-all below, which would otherwise print the literal `null`.
        None | Some(serde_json::Value::Null) => String::new(),
        Some(other) => other.to_string(),
    }
}

/// The `--shot` half of the CLI, as one value.
///
/// Thirteen positional parameters on [`ShotHarness::from_cli`] before this, which
/// the flag names made *look* self-describing at the one real call site and did
/// not at any other: a test asking only "does `--frames` need two waypoints"
/// still had to spell out `0.1, 1.0, false, vec![], "free".into(), 8, None, None,
/// &[]`, and a reader had to count commas against the signature to find which
/// `None` was `--csv`. Named fields plus a [`Default`] carrying the CLI's own
/// defaults means a caller writes only what it is actually testing.
pub struct ShotArgs<'a> {
    /// Base output path (`--shot`).
    pub path: PathBuf,
    /// One or more "lat,lng,alt".
    pub at: &'a [String],
    /// Zero or more "pitch,yaw", paired with `at` by index; the last one carries
    /// over to any waypoint past the end.
    pub look: &'a [String],
    /// Zero or more "lat,lng" to aim at, paired with `at` by index and taking
    /// precedence over `look` — see [`ShotSpec::look_at`]. The last one carries
    /// over, as `look` does.
    pub look_at: &'a [String],
    pub size: &'a str,
    pub settle: f32,
    pub wait: f32,
    pub hide_ui: bool,
    pub ui: Vec<String>,
    pub mode: String,
    /// `> 0` densifies the waypoints into an N-frame sweep (see [`densify_path`]).
    pub frames: u32,
    pub csv: Option<PathBuf>,
    pub dump_state: Option<PathBuf>,
    pub expect: &'a [String],
    /// Capture each viewpoint twice, ~5 cm apart, and measure the pixels that
    /// flip — see [`ShotHarness::flicker`].
    pub flicker: bool,
}

impl Default for ShotArgs<'_> {
    /// The plain-flag defaults, so the one place they are stated is beside the
    /// [`DEFAULT_SIZE`] / [`DEFAULT_SETTLE`] / [`DEFAULT_WAIT`] constants they come
    /// from — and a test that doesn't care about settle timing doesn't invent one.
    fn default() -> Self {
        Self {
            path: PathBuf::new(),
            at: &[],
            look: &[],
            look_at: &[],
            size: DEFAULT_SIZE,
            settle: DEFAULT_SETTLE,
            wait: DEFAULT_WAIT,
            hide_ui: false,
            ui: Vec::new(),
            mode: String::from("free"),
            frames: 0,
            csv: None,
            dump_state: None,
            expect: &[],
            flicker: false,
        }
    }
}

impl ShotHarness {
    /// Build from the raw CLI strings — see [`ShotArgs`] for what each one is.
    pub fn from_cli(args: ShotArgs) -> Result<Self, String> {
        let ShotArgs {
            path,
            at,
            look,
            look_at,
            size,
            settle,
            wait,
            hide_ui,
            ui,
            mode,
            frames,
            csv,
            dump_state,
            expect,
            flicker,
        } = args;
        fn nums(s: &str, want: usize, what: &str) -> Result<Vec<f64>, String> {
            let v: Vec<f64> = s
                .split(',')
                .map(|p| p.trim().parse::<f64>().map_err(|e| format!("{s:?}: {e}")))
                .collect::<Result<_, _>>()?;
            (v.len() == want)
                .then_some(v)
                .ok_or_else(|| format!("{what} wants {want} comma-separated numbers, got {s:?}"))
        }

        let mut shots = Vec::new();
        for (i, at) in at.iter().enumerate() {
            let p = nums(at, 3, "--at")?;
            // Fewer --look than --at: the last one applies to the rest.
            let angles = look
                .get(i)
                .or_else(|| look.last())
                .map(|l| nums(l, 2, "--look"))
                .transpose()?
                .unwrap_or_else(|| vec![-90.0, 0.0]);
            let target = look_at
                .get(i)
                .or_else(|| look_at.last())
                .map(|l| nums(l, 2, "--look-at"))
                .transpose()?;
            shots.push(ShotSpec {
                lat: p[0],
                lng: p[1],
                alt: p[2],
                pitch_deg: angles[0] as f32,
                yaw_deg: angles[1] as f32,
                look_at: target.map(|t| [t[0], t[1]]),
                ..default()
            });
        }
        if shots.is_empty() {
            return Err("--shot needs at least one --at lat,lng,alt".into());
        }
        if frames > 0 && shots.len() < 2 {
            return Err("--frames needs at least two --at waypoints to interpolate between".into());
        }
        let mut shots = densify_path(&shots, frames);
        if flicker {
            // Every viewpoint becomes a pair: itself, then itself nudged north.
            // Even index = reference, odd = the nudged twin the observer diffs
            // against it — the pairing the capture path relies on.
            shots = shots
                .iter()
                .flat_map(|s| {
                    let mut twin = s.clone();
                    twin.lat += FLICKER_NUDGE_DEG;
                    [s.clone(), twin]
                })
                .collect();
        }
        let size = parse_size(size).ok_or_else(|| format!("--size wants WxH, got {size:?}"))?;
        let expect = parse_expectations(expect)?;
        // Same rule as an unknown key: without `--flicker` the metric is a
        // constant 0 and `flicker_px<=N` holds vacuously — a green gate that
        // measured nothing, which is worse than a startup error.
        if !flicker && expect.iter().any(|e| e.key == "flicker_px") {
            return Err("--expect flicker_px needs --flicker, or it gates a constant 0".into());
        }
        Ok(Self {
            path,
            shots,
            size,
            settle,
            wait,
            hide_ui,
            ui,
            mode,
            tile_url: None,
            csv,
            dump_state,
            expect,
            flicker,
            aerial_reference: false,
            live: false,
            windowed: false,
        })
    }

    /// A harness that is handed its viewpoints while the app runs, rather than
    /// running a list and quitting — see [`ShotHarness::live`] (the field).
    ///
    /// `windowed` is whether this client has a window and a user: `--mcp-listen`
    /// on an ordinary session does, `--mcp-headless` does not.
    ///
    /// `cfg(mcp)` because the only thing that builds one is the MCP camera
    /// operator, which is desktop-only — and this crate denies dead code, so an
    /// Android build reports it rather than ignoring it.
    #[cfg(mcp)]
    pub fn live_harness(size: UVec2, windowed: bool) -> Self {
        Self {
            // Never used: every live shot carries its own `out`. Kept honest
            // anyway, since `path_for` falls back to it.
            path: PathBuf::from("mcp-view.png"),
            shots: Vec::new(),
            size,
            settle: DEFAULT_SETTLE,
            wait: DEFAULT_WAIT,
            hide_ui: false,
            ui: Vec::new(),
            mode: "free".to_string(),
            tile_url: None,
            csv: None,
            dump_state: None,
            expect: Vec::new(),
            flicker: false,
            aerial_reference: false,
            live: true,
            windowed,
        }
    }

    /// Build from a preset manifest (`--shots`): every remembered camera
    /// position, or the subset named by `--only`, each with its own output file,
    /// camera mode, open panels and time-of-day/weather.
    ///
    /// This is the "regenerate the docs images" path — it exists so the six
    /// README views are data in `docs/shots.toml` rather than six hand-flown
    /// camera moves that nobody can reproduce.
    pub fn from_manifest(
        manifest_path: &std::path::Path,
        only: &[String],
        size_override: Option<&str>,
        settle_override: Option<f32>,
        wait_override: Option<f32>,
        csv: Option<PathBuf>,
        dump_state: Option<PathBuf>,
        expect: &[String],
    ) -> Result<Self, String> {
        use crate::systems::shot_presets::Manifest;

        let manifest = Manifest::load(manifest_path)?;
        let selected = manifest.select(only)?;
        let shots: Vec<ShotSpec> = selected
            .iter()
            .map(|p| {
                let look = p.look.unwrap_or([-90.0, 0.0]);
                ShotSpec {
                    lat: p.at[0],
                    lng: p.at[1],
                    alt: p.at[2],
                    pitch_deg: look[0],
                    yaw_deg: look[1],
                    name: Some(p.name.clone()),
                    description: p.description.clone(),
                    look_at: p.look_at,
                    out: Some(manifest.out_path(manifest_path, p)),
                    mode: p.mode.clone().or_else(|| manifest.mode.clone()),
                    hide_ui: p.hide_ui.or(manifest.hide_ui),
                    ui: p.ui.clone(),
                    // Manifest-wide patience, and every preset is flown to.
                    settle: None,
                    wait: None,
                    in_place: false,
                    // Per shot first, then the manifest's own default. A group
                    // is a set of views of one subject and they have to be lit
                    // alike, or two of them cannot be compared.
                    env: ShotEnv {
                        time: p.time.or(manifest.time),
                        fog: p.fog.or(manifest.fog),
                        clouds: p.clouds.or(manifest.clouds),
                        rain: p.rain.or(manifest.rain),
                    },
                }
            })
            .collect();
        // The CLI wins over the manifest so a one-off "same shots, bigger" needs
        // no file edit; the manifest wins over the plain defaults.
        let size = size_override
            .map(str::to_owned)
            .or_else(|| manifest.size.clone())
            .unwrap_or_else(|| DEFAULT_SIZE.to_string());
        let size = parse_size(&size).ok_or_else(|| format!("size wants WxH, got {size:?}"))?;
        Ok(Self {
            // Never used for a manifest run (every shot carries its own `out`),
            // but it keeps `path_for`'s fallback honest.
            path: manifest_path.with_extension("png"),
            shots,
            size,
            settle: settle_override
                .or(manifest.settle)
                .unwrap_or(DEFAULT_SETTLE),
            wait: wait_override.or(manifest.wait).unwrap_or(DEFAULT_WAIT),
            hide_ui: manifest.hide_ui.unwrap_or(false),
            ui: Vec::new(),
            mode: manifest.mode.clone().unwrap_or_else(|| "free".to_string()),
            tile_url: manifest.tile_url.clone(),
            csv,
            dump_state,
            // Manifest-wide, not per preset: `--shots --expect settled==true` asks
            // that *every* documentation image landed on a finished world, which is
            // the question worth asking of the set as a whole.
            expect: {
                let expect = parse_expectations(expect)?;
                if expect.iter().any(|e| e.key == "flicker_px") {
                    return Err(
                        "--expect flicker_px needs the --shot path with --flicker; a preset run never measures it".into(),
                    );
                }
                expect
            },
            // A preset run makes documentation images, one file per named view;
            // doubling each into a nudged twin would overwrite half the set.
            // The flicker instrument is the plain `--shot` path's.
            flicker: false,
            aerial_reference: manifest.aerial_reference.unwrap_or(false),
            live: false,
            windowed: false,
        })
    }

    /// Whether a script owns this run rather than riding along in somebody's
    /// session — see [`ShotHarness::windowed`] and [`harness_active`].
    pub fn owns_run(&self) -> bool {
        !self.windowed
    }

    /// Settle seconds for shot `i` — the shot's own, else the run-wide one.
    pub fn settle_for(&self, i: usize) -> f32 {
        self.shots
            .get(i)
            .and_then(|s| s.settle)
            .unwrap_or(self.settle)
    }

    /// Wait cap for shot `i` — the shot's own, else the run-wide one.
    pub fn wait_for(&self, i: usize) -> f32 {
        self.shots.get(i).and_then(|s| s.wait).unwrap_or(self.wait)
    }

    /// Camera mode for shot `i` — the preset's own, else the run-wide one.
    pub fn mode_for(&self, i: usize) -> String {
        self.shots
            .get(i)
            .and_then(|s| s.mode.clone())
            .unwrap_or_else(|| self.mode.clone())
    }

    /// Does any shot in this run need the local avatar on screen?
    ///
    /// A scripted run is always offline, and offline has no avatar
    /// (`lib::sync_local_avatar`) — so without this `--shot-mode avatar`,
    /// `--ui sit` and `--ui driver` would photograph an empty street. Read off
    /// what the run already says it is photographing rather than taken as a flag
    /// of its own: a capture that wants an avatar has to name the thing it wants
    /// one for, and a new preset cannot forget to opt in.
    ///
    /// Over *every* shot, not the current one: the avatar is spawned for the run,
    /// and a sweep whose third viewpoint is `mode = "avatar"` must not find the
    /// street empty when it gets there.
    #[must_use]
    pub fn wants_avatar(&self) -> bool {
        let n = self.shots.len().max(1);
        (0..n).any(|i| {
            self.mode_for(i) == "avatar"
                || self.ui_for(i).iter().any(|u| {
                    matches!(
                        u.as_str(),
                        "all" | "avatar" | "appearance" | "sit" | "driver" | "walk" | "firstperson"
                    )
                })
        })
    }

    /// Whether shot `i` drops the egui overlay.
    pub fn hide_ui_for(&self, i: usize) -> bool {
        self.shots
            .get(i)
            .and_then(|s| s.hide_ui)
            .unwrap_or(self.hide_ui)
    }

    /// Panels to open for shot `i`.
    pub fn ui_for(&self, i: usize) -> Vec<String> {
        self.shots
            .get(i)
            .and_then(|s| s.ui.clone())
            .unwrap_or_else(|| self.ui.clone())
    }

    /// Write the accumulated per-capture metrics to whichever sinks were
    /// requested. Called once, when the last shot lands. CSV is a header plus one
    /// row per shot; the JSON dump is a bare object for a single shot and an array
    /// for a sweep (so a one-shot `--dump-state` reads as a plain snapshot).
    fn write_metrics(&self, metrics: &[ShotMetrics]) {
        if let Some(path) = &self.csv {
            let mut out = String::from(CSV_HEADER);
            for m in metrics {
                out.push('\n');
                out.push_str(&m.csv_row());
            }
            out.push('\n');
            match std::fs::write(path, out) {
                Ok(()) => {
                    tracing::info!(path = %path.display(), rows = metrics.len(), "shot harness: wrote CSV")
                }
                Err(e) => {
                    tracing::error!(path = %path.display(), error = %e, "shot harness: CSV write failed")
                }
            }
        }
        if let Some(path) = &self.dump_state {
            let json = match metrics {
                [one] => serde_json::to_string_pretty(one),
                many => serde_json::to_string_pretty(many),
            };
            match json {
                Ok(s) => match std::fs::write(path, s) {
                    Ok(()) => {
                        tracing::info!(path = %path.display(), shots = metrics.len(), "shot harness: wrote state dump")
                    }
                    Err(e) => {
                        tracing::error!(path = %path.display(), error = %e, "shot harness: dump write failed")
                    }
                },
                Err(e) => tracing::error!(error = %e, "shot harness: dump serialize failed"),
            }
        }
    }

    /// Output path for shot `i` (bare `--shot` path for a single capture).
    ///
    /// A sweep is index-prefixed *and* altitude-tagged (`s_00_180000m.png`): the
    /// zero-padded index keeps the filmstrip in flight order (altitude alone is
    /// non-monotonic across a polyline and collides when interpolated frames round
    /// to the same metre near the ground), the altitude keeps a frame legible.
    fn path_for(&self, i: usize) -> PathBuf {
        // A named preset owns its filename — `docs/img/globe.png` stays that
        // file every run, which is what lets the README link to it.
        if let Some(out) = self.shots.get(i).and_then(|s| s.out.clone()) {
            return out;
        }
        if self.shots.len() == 1 {
            return self.path.clone();
        }
        let stem = self
            .path
            .file_stem()
            .map(|s| s.to_string_lossy().into_owned());
        let ext = self
            .path
            .extension()
            .map(|s| s.to_string_lossy().into_owned());
        let width = (self.shots.len() - 1).to_string().len();
        let name = format!(
            "{}_{:0width$}_{}m.{}",
            stem.as_deref().unwrap_or("shot"),
            i,
            self.shots[i].alt as i64,
            ext.as_deref().unwrap_or("png"),
            width = width,
        );
        self.path.with_file_name(name)
    }
}

/// Parse a `--size` string ("1280x720"). Also read in `main` to size the window
/// to the offscreen target — egui scissors against the window, so the two must
/// agree or wgpu validation kills the run (see the `shot_size` comment there).
pub fn parse_size(size: &str) -> Option<UVec2> {
    let (w, h) = size.split_once(['x', 'X'])?;
    Some(UVec2::new(w.trim().parse().ok()?, h.trim().parse().ok()?))
}

/// The offscreen colour target the camera renders into.
#[derive(Resource)]
pub struct ShotTarget(pub Handle<Image>);

/// The settle test's verdict for one shot, frozen at the moment it was reached.
#[derive(Clone)]
struct SettleVerdict {
    /// Which arm let the capture through: quiet for `--settle`, or out of `--wait`.
    settled: bool,
    /// The streamers still going, when it wasn't the quiet arm.
    busy: Vec<&'static str>,
    /// `elapsed` when the verdict was reached — the number `settled_s` reports, and
    /// the start of the quiesce budget.
    at: f32,
}

/// Where the scripted run has got to.
#[derive(Resource, Default)]
pub struct ShotProgress {
    /// Index of the shot being set up / captured.
    index: usize,
    /// The teleport for this shot has been issued.
    placed: bool,
    /// Seconds since placement.
    elapsed: f32,
    /// Seconds the tile streams have been quiet.
    quiet: f32,
    /// The streamers busy on the last frame, and the wall-clock moment of
    /// placement — so each one's going quiet is logged with *when*, which is the
    /// load's waterfall: `settled_in` is virtual time and one number for all.
    was_busy: Vec<&'static str>,
    placed_at: Option<bevy::platform::time::Instant>,
    /// Frames, total wall-clock seconds and the worst single frame since the
    /// stream went quiet — the mean and max frame time reported with each capture.
    ///
    /// Wall clock (`Time<Real>`), never the virtual clock: Bevy clamps `Time`'s
    /// delta at `max_delta` (0.25 s), so accumulating that measured a fixed 250 ms
    /// on any renderer slower than four frames a second and quietly described every
    /// scene as costing the same.
    frames: u32,
    frame_time: f32,
    frame_time_max: f32,
    /// Outstanding tile-decode jobs at the settle verdict, held across the quiesce
    /// that follows it — after which the count is 0 by construction.
    worker_outstanding: usize,
    /// Fetch counters at placement — the baseline this capture's `fetch_*`
    /// columns are measured from, so each shot reports its own window rather
    /// than the run so far.
    fetch_base: crate::platform::http::stats::FetchTotals,
    /// The worker pool's per-stream totals at placement, for the same reason.
    worker_base: Vec<(&'static str, crate::platform::workers::stats::Totals)>,
    /// The two windows, computed at the settle verdict beside
    /// `worker_outstanding` and held across the quiesce — the drain that follows
    /// the verdict is harness bookkeeping, not part of the picture's cost.
    fetch_window: crate::platform::http::stats::FetchTotals,
    worker_window: Vec<(&'static str, crate::platform::workers::stats::Totals)>,
    /// The settle test's answer, taken on the frame it passed and held across the
    /// quiesce that follows. Held rather than recomputed because `quiet` keeps
    /// accruing while the CPU pool drains: a shot that gave up at `--wait` would
    /// otherwise go quiet during the drain and report itself settled, which is the
    /// exact lie `settled`/`busy` were added to stop telling.
    verdict: Option<SettleVerdict>,
    /// The capture frame has been requested (waiting on the GPU readback).
    fired: bool,
    /// `elapsed` at the moment it fired, so the readback gets its own budget —
    /// see [`CAPTURE_GRACE_S`].
    fired_at: f32,
    /// The shutter has already been re-fired once after the grace expired.
    retried: bool,
    /// Flipped by the capture observer once the PNG is on disk.
    saved: Arc<AtomicBool>,
    /// The previous capture's RGB8 frame, kept only under `--flicker` so the
    /// pair's second observer has something to diff against. `Arc<Mutex>` for
    /// the same reason `saved` is an `Arc`: the closure outlives the borrow.
    flicker_prev: Arc<std::sync::Mutex<Option<(u32, u32, Vec<u8>)>>>,
    /// Frames left before exiting (drains after the last save).
    exit_in: Option<u32>,
    /// One [`ShotMetrics`] per captured viewpoint, flushed to `--csv`/`--dump-state`
    /// when the run completes.
    metrics: Vec<ShotMetrics>,
    /// `--expect` assertions that did not hold, over the whole run. Counted rather
    /// than fatal on the spot: the remaining viewpoints still get captured, so a
    /// red CI job hands back the PNG and the metrics that explain it. The count is
    /// what turns into a non-zero exit at the end.
    unmet: usize,
    /// The performance suite is measuring this viewpoint and the run must not
    /// advance yet (`systems::perf`).
    ///
    /// **These three fields are the entire coupling between the two harnesses.** A
    /// perf run is a capture run with a measurement window after each shutter;
    /// everything else it needs — the teleport, the settle test, the camera hold,
    /// the pinned weather, the worker quiesce — is this file's, and duplicating any
    /// of it would create a second state machine to keep in step with this one.
    pub(crate) perf_hold: bool,
    /// The window closed and the run may advance. Needed because `saved` is a
    /// one-shot: by the time the measurement finishes there is no flag left.
    pub(crate) perf_release: bool,
    /// The shutter fired while the suite was holding the run.
    ///
    /// **Set by `drive_capture` rather than read off `saved`, and that is a fix.**
    /// `saved` is an `AtomicBool` flipped from the capture's own observer and
    /// consumed here with a `swap`; a reader ordered ahead of that still loses
    /// whenever the flip lands *between* the two systems in one frame. That is not
    /// a rare interleaving — it is what happened on the first run: the PNG was
    /// written, the run advanced, and the suite's steady window never opened, so it
    /// exited 0 having written no report. Handing the edge over cannot race.
    perf_captured: bool,
}

impl ShotProgress {
    /// Which viewpoint the run is on.
    pub(crate) fn shot_index(&self) -> usize {
        self.index
    }

    /// Whether the teleport for the current viewpoint has been issued — the
    /// opening edge of the suite's `load` window.
    pub(crate) fn is_placed(&self) -> bool {
        self.placed
    }

    /// Whether the settle test has answered for the current viewpoint.
    ///
    /// The closing edge of the `load` window, and deliberately *not* the capture
    /// landing: between the verdict and the shutter this harness quiesces the
    /// worker pool and waits on a GPU readback, both its own bookkeeping. Measuring
    /// them as streaming cost would put tens of seconds of harness into the one
    /// window that is meant to be what a teleport costs.
    pub(crate) fn verdict_reached(&self) -> bool {
        self.verdict.is_some()
    }

    /// Whether that verdict was the *quiet* arm rather than `--wait` running
    /// out. Distinct from [`Self::verdict_reached`] on purpose: both arms reach
    /// a verdict, and the suite divides a window's whole cost by its wall clock,
    /// which a timeout stretches to the cap. A caller that cannot tell the two
    /// apart reports a stalled layer as a performance result.
    pub(crate) fn verdict_settled(&self) -> bool {
        self.verdict.as_ref().is_some_and(|v| v.settled)
    }

    /// Whether the shutter has fired for the viewpoint the suite is holding.
    pub(crate) fn perf_captured(&self) -> bool {
        self.perf_captured
    }

    /// The rows captured so far, newest last. The live harness's answer to
    /// "what did that `view` measure?" — matched on [`ShotMetrics::shot`], which
    /// is the shot index plus one and keeps counting across a trim.
    ///
    /// `cfg(mcp)` for the same reason [`ShotHarness::live_harness`] is: nothing
    /// off the desktop reads these rows, and this crate denies dead code.
    #[cfg(mcp)]
    pub fn metrics(&self) -> &[ShotMetrics] {
        &self.metrics
    }

    /// Bound the history a live run accumulates.
    ///
    /// A scripted run's metrics are its *output* — the CSV and the JSON dump are
    /// written from this vector, so nothing may be dropped. A live run's are a
    /// log: an agent reads the row belonging to the call it just made, and a
    /// session that captures for an hour must not grow one row at a time until
    /// it is the largest thing in the process.
    fn trim_metrics(&mut self) {
        const LIVE_METRICS_KEPT: usize = 64;
        if self.metrics.len() > LIVE_METRICS_KEPT {
            let drop = self.metrics.len() - LIVE_METRICS_KEPT;
            self.metrics.drain(..drop);
        }
    }
}

/// True while **a script owns the run** — the condition the user store, the frame
/// limiter and the load controller use to keep out of the way.
///
/// Not simply "a harness exists" any more. A live harness (`--mcp-listen`) rides
/// along in an ordinary windowed session, where all three of those should behave
/// exactly as they do without it: the session is somebody's, their saved location
/// and layers are theirs, and a client being driven by an agent is still a client
/// a person is watching. See [`ShotHarness::windowed`].
pub fn harness_active(harness: Option<Res<ShotHarness>>) -> bool {
    harness.is_some_and(|h| h.owns_run())
}

/// True while the harness has a viewpoint to work on.
///
/// The run condition for every system that *holds* something for the duration of
/// a capture — the camera mode, the egui refresh, the panels a `--ui` list opens.
/// Under `--shot` it is true for the whole run, so nothing about a scripted
/// capture changes. Under a live harness it is false between calls, which is what
/// hands the client back to whoever is sitting in front of it: a camera mode held
/// every frame by an idle camera operator is a camera the user cannot change.
pub fn shot_in_progress(harness: Option<Res<ShotHarness>>, progress: Res<ShotProgress>) -> bool {
    harness.is_some_and(|h| progress.index < h.shots.len())
}

/// Keep egui's geometry in step with the world for the whole of a scripted run.
///
/// **A settled capture could otherwise show chrome from twenty seconds earlier.**
/// `egui_manager::end_egui_frame` skips the tessellate → upload chain whenever
/// egui reports `repaint_delay == Duration::MAX`, and redraws the last-written
/// primitives instead — which is free and correct for the thing it was written
/// for, a static menu bar under a mouse that has not moved. It is not correct
/// for an overlay whose content comes from the **ECS**: egui's repaint signal
/// answers "has anything I know about changed", and it knows nothing about the
/// camera. So a run that flies somewhere and then goes quiet — which is exactly
/// what `--settle` is — keeps painting the shapes from the last frame egui
/// happened to consider interesting.
///
/// It was found the way these things are: a `--shot-mode earth` capture from
/// 14,000 km carried a scale bar reading "200 m", the altitude the camera had on
/// frame 2 of the run, while `render_map_controls` had correctly drawn no bar at
/// all for the twenty seconds since. Anything read off the world and drawn into
/// the overlay could be that stale — the clock, the coordinates, the compass
/// needle, an open panel's contents — and nothing about the image says so.
///
/// Forcing the write costs one tessellation per frame, in the one kind of run
/// that is already spending 250 ms a frame on a software rasteriser and whose
/// entire purpose is the picture at the end of it. The live app keeps the skip.
pub fn keep_egui_fresh(mut render_output: ResMut<crate::systems::egui_manager::EguiRenderOutput>) {
    render_output.force_next_write = true;
}

/// The two floating cards a `--ui` list can raise, plus the preferences a shot
/// may override — bundled into one parameter because [`open_scripted_ui`] is at
/// Bevy's sixteen-parameter ceiling and adding a seventeenth does not fail as a
/// count, it fails as `does not describe a valid system configuration`.
///
/// It briefly held `AppPrefs` too, for the map's frame overlay — one `--ui stats`
/// boolean. Both are gone: the overlay was deleted (`systems::dev_panel`'s module
/// note) and a scripted run never reads or writes `user.json` anyway, so there is
/// nothing left here that a preference decides.
#[derive(bevy::ecs::system::SystemParam)]
pub struct ScriptedCards<'w, 's> {
    menu: ResMut<'w, crate::systems::ui::MainMenu>,
    sign_in: ResMut<'w, crate::systems::login_screen::LoginScreen>,
    settings: ResMut<'w, crate::systems::settings::SettingsState>,
    /// Nested rather than added beside: the ceiling this type exists to work
    /// around is a *parameter* count, so a second bundle would hit it again.
    chat: ScriptedChat<'w, 's>,
}

/// What `--ui chat` needs, bundled for the same reason [`ScriptedCards`] is.
///
/// The spawned speaker is here rather than in a system of its own because it has
/// to be *unspawned* on the next shot that does not ask for chat, exactly as
/// every other `--ui` element is written unconditionally — a preset that raises a
/// stranger over one image and leaves them standing in the next five is the
/// failure mode the "written unconditionally" rule exists to stop.
#[derive(bevy::ecs::system::SystemParam)]
pub struct ScriptedChat<'w, 's> {
    commands: Commands<'w, 's>,
    state: ResMut<'w, crate::systems::chat::ChatState>,
    overlay: ResMut<'w, crate::systems::chat_overlay::ChatOverlay>,
    /// The messenger's screens (`--ui chats` / `chatthread` / `chatworld`).
    chats: ResMut<'w, crate::systems::chats::ChatsView>,
    /// The contacts screens (`--ui contacts` / `requests` / `request` /
    /// `addcontact` / `privacy` / `personcard`).
    contacts: ResMut<'w, crate::systems::contacts::Contacts>,
    /// The call screens (`--ui callincoming` / `call` / `callvideo` /
    /// `callgroup` / `callfolded`, `systems::call_screen`).
    calls: ResMut<'w, crate::systems::calls::Calls>,
    seeded: Query<'w, 's, Entity, With<crate::systems::chat_overlay::ScriptedSpeaker>>,
    avatar: Query<
        'w,
        's,
        &'static Transform,
        (
            With<crate::components::Avatar>,
            Without<crate::components::RemoteAvatar>,
        ),
    >,
}

/// Every name `--ui` accepts — the vocabulary [`open_scripted_ui`] checks the
/// caller's list against, so a typo is a warning rather than a silently missing
/// panel.
///
/// **It must name every string `want(…)` is called with down there**, and the
/// hazard is that nothing enforces it: the two lists drifted for real, `settings`
/// and `graphics` grew handlers without ever being added here, and the result was
/// the worst possible reading — `--ui graphics` opened Settings on the Graphics
/// tab *and* logged that it had been ignored. A reviewer trusting the log would
/// have gone looking for a bug in a panel that had already worked. Adding a `want`
/// arm means adding its name here; `ui_vocabulary_covers_every_handler` is the
/// test that fails when it doesn't.
const UI_ELEMENTS: &[&str] = &[
    "dev",
    "profiler",
    "switches",
    "menu",
    "avatar",
    "appearance",
    "layers",
    "places",
    "bookmarks",
    "route",
    "driver",
    "walk",
    "search",
    "place",
    "settings",
    "graphics",
    "audio",
    "world",
    "signin",
    "chat",
    "chats",
    "chatthread",
    "chatworld",
    "chatswitch",
    "contacts",
    "requests",
    "request",
    "addcontact",
    "privacy",
    "personcard",
    "friends",
    "sit",
    "building",
    "inside",
    "locate",
    "firstperson",
    "fire",
    "callincoming",
    "call",
    "callvideo",
    "callgroup",
    "callfolded",
    "all",
];

/// Open the UI panels named by `--ui` so a scripted capture can document them —
/// the dev lighting/weather controls, the layers list, the bookmarks or AI
/// panels — not just the always-on HUD and menu bar. These panels are normally
/// closed at startup and toggled by hand, which a headless run can't do. Runs
/// once. Unknown names are ignored (and logged) rather than aborting the run.
/// A preset manifest gives each shot its own `ui` list, so this re-applies on
/// every shot rather than once per run: panels the current shot doesn't ask for
/// are closed again, or a `--ui route` preset would leave the planner open over
/// every image after it.
#[allow(clippy::too_many_arguments)]
pub fn open_scripted_ui(
    harness: Res<ShotHarness>,
    progress: Res<ShotProgress>,
    mut dev: ResMut<crate::resources::DevPanelState>,
    mut tools: ResMut<crate::systems::dev_panel::DevTools>,
    mut avatar: ResMut<crate::resources::AvatarChooserState>,
    mut layers: ResMut<crate::systems::data_layers::DataLayers>,
    mut bookmarks: ResMut<crate::systems::bookmarks::BookmarksState>,
    mut right_panel: ResMut<crate::resources::RightPanel>,
    mut route: ResMut<crate::systems::navigation::RouteState>,
    mut driver: ResMut<crate::systems::driver::DriverState>,
    mut walk: ResMut<crate::systems::walk_guide::WalkGuide>,
    mut nav: ResMut<crate::systems::geo_nav::GeoNavState>,
    mut cards: ScriptedCards,
    mut store: ResMut<crate::systems::user_store::UserStore>,
    egui: Res<crate::systems::egui_manager::EguiManager>,
    mut done: Local<Option<usize>>,
) {
    if *done == Some(progress.index) {
        return;
    }
    *done = Some(progress.index);
    let panels = harness.ui_for(progress.index);
    let all = panels.iter().any(|u| u == "all");
    let want = |k: &str| all || panels.iter().any(|u| u == k);
    // Written unconditionally (not only when opening) so each shot starts from
    // exactly the chrome it asked for.
    dev.open = want("dev") || want("profiler") || want("switches");
    // A tab, not just the window: `--ui dev` alone photographs whichever tab the
    // enum defaults to, which is the same trap `--ui graphics` exists to avoid
    // for Settings.
    tools.tab = if want("profiler") {
        crate::systems::dev_panel::DevTab::Profiler
    } else if want("switches") {
        crate::systems::dev_panel::DevTab::Switches
    } else {
        crate::systems::dev_panel::DevTab::Frame
    };
    // `--ui signin` opens the sign-in card. It is a modal over everything else,
    // so it is written before the panels below rather than beside them — and it
    // is here for the reason `--ui menu` is: a card nothing can screenshot is a
    // card nobody reviews, and this one is a form that only opens by hand.
    cards.sign_in.open = want("signin");
    // A scripted run reads no `user.json`, so the form comes up empty — and an
    // empty form is the *disabled* state of the button, which is the one state
    // this capture should not be documenting. Seed it with the address the docs
    // hand out so `--ui signin` photographs the card somebody actually looks at.
    if want("signin") {
        cards.sign_in.seed_for_capture("cartopolis.org", "jeroen");
    }
    // `--ui menu` opens the app mark's card. It is the one piece of chrome a
    // scripted run could not reach at all, and it is where the entries that left
    // the map now live — Set Home, Copy location and the way into Developer
    // tools. A control nothing can screenshot is a control nobody reviews.
    cards.menu.open = want("menu");
    avatar.open = want("avatar");
    layers.panel_open = want("layers");
    bookmarks.panel_open = want("bookmarks") || want("places");
    // Driver mode is a *mode*, not a panel: it takes the map over (bare basemap,
    // locked chase camera, navigation chrome). Written unconditionally like the
    // panels, so a preset that doesn't ask for it also leaves it.
    if want("driver") {
        // Armed rather than entered: the endpoints seeded below still have to go
        // through a worker solve, and driver mode needs the answer.
        driver.arm();
    } else {
        driver.exit();
    }
    // The right-hand dock holds one panel at a time (`RightPanel`), so asking for
    // several picks the first. The legacy `*.panel_open` booleans above only tell
    // the HUD to get out of the way — the panels themselves render off this.
    *right_panel = if want("route") {
        crate::resources::RightPanel::Route
    } else if want("layers") {
        crate::resources::RightPanel::Layers
    } else if want("places") || want("bookmarks") {
        crate::resources::RightPanel::Places
    } else if want("search") {
        crate::resources::RightPanel::Search
    } else if want("avatar") || want("appearance") {
        // The chooser moved into the `RightPanel` slot with the rest; the
        // `avatar.open` bool above is what the HUD reads, not the panel, so
        // `--ui avatar` opened nothing at all until this arm existed.
        crate::resources::RightPanel::Appearance
    } else if want("settings") || want("graphics") || want("world") || want("audio") {
        // `--ui graphics` opens Settings on the Graphics tab, `--ui world` on the
        // World tab. A tab is not reachable by opening the panel alone, so naming
        // the panel would capture whichever tab was last selected — which for a
        // fresh scripted world is always General, i.e. never the one being
        // reviewed.
        // `--ui audio`: the Audio & video tab with the camera test running (on
        // the stand-in camera under `CARTO_FAKE_CAMERA`) and a sample level on
        // the meter, since a capture has no microphone to move it.
        #[cfg(not(target_arch = "wasm32"))]
        if want("audio") && !cards.settings.camera_running() {
            cards.settings.start_camera();
            cards.settings.demo_level = Some((0.62, 0.81));
        }
        cards.settings.tab = if want("audio") {
            crate::systems::settings::SettingsTab::Audio
        } else if want("graphics") {
            crate::systems::settings::SettingsTab::Graphics
        } else if want("world") {
            crate::systems::settings::SettingsTab::World
        } else {
            crate::systems::settings::SettingsTab::General
        };
        crate::resources::RightPanel::Settings
    } else {
        crate::resources::RightPanel::None
    };
    // `--ui chats` is the chat list, `--ui chatthread` one chat's Chat view and
    // `--ui chatworld` its World view (`systems::chats`), seeded with demo chats
    // for `chat_overlay`'s reason: every scripted run is offline, and these
    // screens need a session to draw at all.
    let messenger = if want("chatswitch") {
        Some("switch")
    } else if want("chatworld") {
        Some("world")
    } else if want("chatthread") {
        Some("chat")
    } else if want("chats") {
        Some("list")
    } else {
        None
    };
    match messenger {
        Some(screen) => {
            *right_panel = crate::systems::chats::seed_demo_chats(
                &mut cards.chat.state,
                &mut cards.chat.chats,
                screen,
            );
            // The World view's people (`chat_people`): a scripted run has no
            // server to say where anybody is.
            if screen == "world" {
                crate::systems::contacts::seed_demo_whereabouts(&mut cards.chat.contacts);
            } else {
                cards.chat.contacts.whereabouts.clear();
            }
        }
        None => {
            crate::systems::chats::clear_demo_chats(&mut cards.chat.state, &mut cards.chat.chats);
            // `--ui friends`: the Friends layer's markers, no chat open.
            if want("friends") {
                crate::systems::contacts::seed_demo_friends(&mut cards.chat.contacts);
            } else {
                cards.chat.contacts.whereabouts.clear();
            }
        }
    }
    // The call screens (`systems::call_screen`): a call ringing, a voice call, a
    // video call, a group, and a call folded to its bar — seeded, since a
    // scripted run is offline and has nobody to call.
    let call_screen = [
        "callincoming",
        "callvideo",
        "callgroup",
        "callfolded",
        "call",
    ]
    .into_iter()
    .find(|k| !all && want(k));
    cards.chat.calls.seed_demo(call_screen);
    // The contacts screens (`systems::contacts`), over the same demo chats.
    // `Some(None)` is the person card over the map rather than a panel screen.
    use crate::systems::contacts::Sub;
    let contacts_screen = if want("personcard") {
        Some(None)
    } else if want("privacy") {
        Some(Some(Sub::Privacy))
    } else if want("addcontact") {
        Some(Some(Sub::Add))
    } else if want("request") {
        Some(Some(Sub::Request("jeroen".into())))
    } else if want("requests") {
        Some(Some(Sub::Requests))
    } else if want("contacts") {
        Some(Some(Sub::Contacts))
    } else {
        None
    };
    match contacts_screen {
        Some(sub) => {
            *right_panel = crate::systems::chats::seed_demo_chats(
                &mut cards.chat.state,
                &mut cards.chat.chats,
                "list",
            );
            crate::systems::contacts::seed_demo(&mut cards.chat.contacts, &mut cards.chat.state);
            cards.chat.chats.open = None;
            if sub.is_none() {
                *right_panel = crate::resources::RightPanel::None;
                crate::systems::contacts::seed_demo_card(&mut cards.chat.contacts);
            } else {
                cards.chat.contacts.card = None;
            }
            cards.chat.chats.sub = sub;
        }
        None => {
            if cards.chat.contacts.demo {
                *cards.chat.contacts = crate::systems::contacts::Contacts::default();
                cards.chat.chats.sub = None;
            }
        }
    }
    if panels.is_empty() {
        return;
    }
    // The route planner needs endpoints to show anything worth capturing, so
    // `--ui route` also seeds a demo route — the shot is of a *planned* route, not
    // of an empty form. `--ui driver` needs one for the same reason, and needs it
    // more: driver mode does not exist without a route. Two coordinates ~1 km apart
    // in the fixture city, with neutral labels (no addresses in the captured
    // chrome). `CARTO_ROUTE_FROM` / `CARTO_ROUTE_TO` ("lat,lng") override the pair, so
    // a specific route can be reproduced headlessly for debugging (the `CARTO_*` env
    // convention).
    // `--ui walk`: walking the seeded route in the world (`systems::walk_guide`),
    // armed for the same reason driver mode is — the route still has to solve.
    // On foot, so the line and the times are the walking ones.
    if want("walk") {
        route.mode = crate::systems::routing::TravelMode::Foot;
        walk.arm(false);
    } else {
        walk.stop();
    }
    if want("route") || want("driver") || want("walk") {
        let seed = |var: &str, default: (f64, f64)| {
            std::env::var(var)
                .ok()
                .and_then(|s| crate::systems::geo_nav::parse_lat_lng(&s))
                .unwrap_or(default)
        };
        route
            .from
            .set("Route start", seed("CARTO_ROUTE_FROM", (53.2194, 6.5665)));
        route
            .to
            .set("Route end", seed("CARTO_ROUTE_TO", (53.2109, 6.5642)));
    }
    // `--ui places` needs saved places for the same reason `--ui route` needs a
    // route: a scripted run never loads `user.json`, so the panel would otherwise
    // document its empty state. Only fills a store that has nothing in it.
    if want("places") || want("bookmarks") {
        crate::systems::bookmarks::seed_demo_places(&egui.ctx, &mut bookmarks, &mut store);
    }
    // `--ui search` opens the Search panel (above) *and* seeds a query, so the
    // capture shows the thing the panel exists for rather than an empty field.
    // `CARTO_SEARCH_QUERY` overrides the demo query, per the `CARTO_*` convention.
    if want("search") {
        let query = std::env::var("CARTO_SEARCH_QUERY").unwrap_or_else(|_| "Amsterdam".into());
        nav.seed_search(query);
    }
    // `--ui place` opens the place card (`systems::place_card`) over the map,
    // seeded with a demo place: the card is normally opened by picking a search
    // result, and a scripted run has no geocoder to answer one. Queued through
    // the same `pending_card` slot a real pick uses, so the capture exercises
    // the real path.
    if want("place") {
        nav.pending_card = Some(crate::systems::place_card::CardPlace {
            name: "Martinitoren".into(),
            kind: Some("tower".into()),
            context: "Martinikerkhof, Groningen".into(),
            lat: 53.2194,
            lng: 6.5681,
        });
    }
    // `--ui chat` is the only way to photograph `systems::chat_overlay` at all:
    // the bubbles, the transcript and the composer all need a session, and every
    // scripted run is offline (`--shot` implies it). So this seeds an exchange,
    // claims `my_id` for the local avatar — whose bubble then needs no entity a
    // `--offline` run does not already have — and stands one more speaker beside
    // it. Pair it with `--shot-mode avatar`, or the camera is somewhere the
    // avatar is not and the bubbles are off screen with it.
    seed_scripted_chat(want("chat"), &mut cards.chat);

    // Surface typos: anything that matched nothing is neither a panel nor `all`.
    for u in &panels {
        if !UI_ELEMENTS.contains(&u.as_str()) {
            tracing::warn!(name = %u, "shot harness: unknown --ui element (ignored)");
        }
    }
    tracing::info!(shot = progress.index + 1, ui = ?panels, "shot harness: opened scripted UI panels");
}

/// Seed or clear `--ui chat`. Written unconditionally, like every other element.
fn seed_scripted_chat(want: bool, chat: &mut ScriptedChat) {
    if !want {
        for e in chat.seeded.iter() {
            chat.commands.entity(e).despawn();
        }
        chat.overlay.demo = false;
        chat.overlay.close();
        return;
    }
    crate::systems::chat_overlay::seed_demo_chat(&mut chat.state, &mut chat.overlay);
    if !chat.seeded.is_empty() {
        return;
    }
    let at = chat
        .avatar
        .single()
        .map(|t| t.translation)
        .unwrap_or_default()
        + crate::systems::chat_overlay::DEMO_SPEAKER_OFFSET;
    // The same shape `network::sync_remote_avatar_entities` spawns, so
    // `avatar::spawn_avatar_models` dresses it exactly as a real one — and so
    // the bubble finds it through the ordinary `sim_id` path rather than a
    // second one written for the harness.
    chat.commands.spawn((
        crate::components::RemoteAvatar {
            sim_id: 2,
            net_position: at,
            net_yaw: std::f32::consts::PI,
            net_velocity: Vec3::ZERO,
            net_age: 0.0,
            pending_emote: None,
            last_emote_sequence: None,
            seated: None,
            seat_blend: 0.0,
        },
        crate::systems::chat_overlay::ScriptedSpeaker,
        Transform::from_translation(at).with_rotation(Quat::from_rotation_y(std::f32::consts::PI)),
    ));
}

/// Seed and hold the `--ui fire` demo: two prims in front of the camera, one
/// burning and one exploding.
///
/// It exists for the reason `--ui chat`'s demo speaker does — a scripted run has
/// no server, so a feature that only ever arrives on a prim from a script would
/// otherwise photograph as an empty street, and a feature nobody can photograph
/// is a feature nobody reviews. What a capture proves here is the *geometry*
/// half: that the pipeline built, that the emitter exists, that the quads are
/// placed and in frame (`particle_emitters` / `particle_quads` in
/// `--dump-state`). Colour, brightness and blending are not evidence on a
/// software rasteriser — `docs/notes/headless-shots-software-renderer.md`.
///
/// **A burst has to be re-armed or it cannot be in the picture at all.** A
/// capture settles for fifteen virtual seconds and an explosion lasts about one,
/// so the blast seeded at startup is minutes over by the time the shutter fires.
/// The sequence is bumped once per lifetime here, which is the same "the seed and
/// the shutter are seconds apart" problem `hold_scripted_chat` re-stamps its
/// lines for — and it goes through `ParticleEffect::sequence` rather than round
/// the side of it, so what is being exercised is the real re-arm path.
///
/// The prims follow the camera, again as the demo speaker does and for the same
/// reason: `--look` does not reliably reach the rendered camera, so anything
/// parked at a fixed offset photographs an empty street about as often as not.
pub fn hold_scripted_fire(
    harness: Res<ShotHarness>,
    progress: Res<ShotProgress>,
    time: Res<Time>,
    mut commands: Commands,
    camera: Query<&Transform, (With<FreeCamera>, Without<crate::components::Prim>)>,
    mut seeded: Query<
        (
            &mut Transform,
            &mut crate::components::Prim,
            &crate::systems::particles::ScriptedFire,
        ),
        Without<FreeCamera>,
    >,
) {
    let want = |k: &str| {
        let panels = harness.ui_for(progress.index);
        panels.iter().any(|u| u == k || u == "all")
    };
    if !want("fire") {
        // Not asked for. Anything seeded by an earlier shot in a `--shots` run
        // goes, or one preset's fire would burn through every image after it —
        // the same rule `open_scripted_ui` follows for its panels.
        for (_, mut prim, _) in seeded.iter_mut() {
            prim.particles = None;
        }
        return;
    }
    let Ok(cam) = camera.single() else { return };
    // On the view ray, at a comfortable distance, floored at the ground.
    //
    // **Not "on the ground where the ray hits"**, which is what this did first
    // and is wrong in exactly the case a demo capture is usually taken from: a
    // camera 25 m up at a shallow pitch meets the ground 180 m away, the clamp
    // pulls the distance back to 20 m, and forcing the height to ground level
    // then puts the fire 20 m ahead and 25 m *below* — a 51° depression, outside
    // an 8°-pitched frustum. Two emitters, 296 quads, nothing in the picture.
    // Staying on the ray cannot do that: whatever the camera is doing, the point
    // is in front of it.
    const GROUND_Y: f32 = 0.05;
    let forward = cam.forward().as_vec3();
    let drop = cam.translation.y - GROUND_Y;
    let hit = (forward.y < -0.05 && drop > 0.0).then(|| drop / -forward.y);
    let distance = hit.unwrap_or(8.0).clamp(4.0, 20.0);
    let right = cam.right().as_vec3();
    // Where each of the two stands. They are put at *different* distances rather
    // than side by side because a blast reaches about twenty-five metres
    // (`ParticleEffect::reach_m`) and a flame about five: at one distance the
    // explosion fills the frame and the fire is a detail inside it, which is a
    // picture of one feature. `place` is called per emitter below.
    let place = |away: f32, offset: f32| {
        let mut p = cam.translation + forward * away + right * offset;
        p.y = p.y.max(GROUND_Y);
        p
    };

    if seeded.is_empty() {
        // `(distance multiple, sideways metres)`: the flame near and to one side,
        // the blast well back and to the other.
        for (burst, offset) in [(false, (1.0_f32, -2.0_f32)), (true, (3.0, 6.0))] {
            let effect = if burst {
                cartopolis_core::ParticleEffect::blast()
            } else {
                cartopolis_core::ParticleEffect::fire()
            };
            commands.spawn((
                crate::components::Prim {
                    // Negative, so a seeded prim can never collide with a
                    // server-assigned id — offline there is no server, and this
                    // says so rather than relying on there not being one.
                    id: if burst { -2 } else { -1 },
                    name: if burst { "Blast" } else { "Fire" }.into(),
                    shape: crate::components::PrimShape::Cylinder,
                    color: Color::srgb(0.15, 0.13, 0.12),
                    texture_id: None,
                    model: None,
                    path_cut_begin: 0.0,
                    path_cut_end: 1.0,
                    hollow: 0.0,
                    twist_begin: 0.0,
                    twist_end: 0.0,
                    taper_x: 0.0,
                    taper_y: 0.0,
                    top_shear_x: 0.0,
                    top_shear_y: 0.0,
                    slice_begin: 0.0,
                    slice_end: 1.0,
                    surface: cartopolis_core::PrimSurface::default(),
                    particles: Some(effect),
                    // A scripted run has no server and so no script; this also
                    // keeps the seeded prims out of the interact prompt, which
                    // would otherwise offer to press a thing with nothing behind
                    // it in every `--ui fire` capture.
                    scripted: false,
                    video: None,
                    sound_loop: None,
                    physical: false,
                },
                crate::systems::particles::ScriptedFire { burst },
                Transform::from_translation(place(distance * offset.0, offset.1))
                    .with_scale(Vec3::new(0.8, 0.3, 0.8)),
            ));
        }
        return;
    }

    for (mut at, mut prim, marker) in seeded.iter_mut() {
        at.translation = if marker.burst {
            place(distance * 3.0, 6.0)
        } else {
            place(distance, -2.0)
        };
        let Some(effect) = prim.particles.as_mut() else {
            continue;
        };
        if !marker.burst {
            continue;
        }
        // Re-arm once per lifetime, so whenever the shutter fires the blast is
        // somewhere in its first second.
        let period = effect.life.max(0.1);
        let step = (time.elapsed_secs() / period) as u32;
        if effect.sequence != step {
            effect.sequence = step;
        }
    }
}

/// Hold the `--ui chat` demo steady: keep the seeded lines fresh, and stand the
/// demo speaker where the camera can see them.
///
/// **The lines have to be re-stamped or the bubbles are never in the picture.**
/// A capture waits for the tile streams to go quiet — fifteen virtual seconds
/// over a city, and longer on a software rasteriser — while a speech bubble is
/// deliberately short-lived. The first `--ui chat` shot photographed the demo
/// speaker standing in the street with nothing over their head for exactly that
/// reason. Nothing about it is a fact about the feature; it is the seed and the
/// shutter being seconds apart, so the seed keeps saying "just now".
///
/// **Because `--look` cannot be relied on to frame anything.** The pitch a shot
/// asks for does not reliably reach the rendered camera (`--shot-mode avatar`
/// ignored three different elevations in a row while this was being written), so
/// a demo speaker parked at a fixed offset from the avatar photographs as an
/// empty street about as often as not — and a feature that cannot be
/// photographed is a feature nobody reviews. Following the camera instead makes
/// the bubble land in frame whatever the camera turned out to be doing.
///
/// Runs every frame rather than once with the seed, because the camera is placed
/// by a different system and the settle loop moves it afterwards.
pub fn hold_scripted_chat(
    mut chat: ResMut<crate::systems::chat::ChatState>,
    overlay: Res<crate::systems::chat_overlay::ChatOverlay>,
    // `--ui chatworld` too: its balloons (`chat_people`) age the same way.
    view: Res<crate::systems::chats::ChatsView>,
    camera: Query<
        &Transform,
        (
            With<FreeCamera>,
            Without<crate::systems::chat_overlay::ScriptedSpeaker>,
        ),
    >,
    mut speaker: Query<&mut Transform, With<crate::systems::chat_overlay::ScriptedSpeaker>>,
) {
    if !overlay.demo && !(view.demo && chat.world.is_some()) {
        return;
    }
    let now = chat.clock;
    for channel in &mut chat.channels {
        for line in &mut channel.lines {
            line.arrived = now;
        }
    }
    let Ok(cam) = camera.single() else { return };
    let Ok(mut at) = speaker.single_mut() else {
        return;
    };
    /// Ground the avatars stand on (`geo_nav::AVATAR_GROUND_Y`).
    const GROUND_Y: f32 = 0.05;
    /// Nearest and furthest the speaker is placed, in metres.
    const RANGE: std::ops::RangeInclusive<f32> = 3.0..=15.0;
    let forward = cam.forward().as_vec3();
    // Where the view ray meets the ground, if it is pointed down at all; a level
    // or upward camera gets a speaker straight ahead at head height instead, so
    // the bubble is still on screen.
    let drop = cam.translation.y - GROUND_Y;
    let hit = (forward.y < -0.05 && drop > 0.0).then(|| drop / -forward.y);
    let distance = hit.unwrap_or(6.0).clamp(*RANGE.start(), *RANGE.end());
    let mut pos = cam.translation + forward * distance;
    pos.y = if hit.is_some() {
        GROUND_Y
    } else {
        (cam.translation.y - 1.7).max(GROUND_Y)
    };
    at.translation = pos;
    // Facing the camera, which is what a person talking to you does.
    let away = cam.translation - pos;
    at.rotation = Quat::from_rotation_y(away.x.atan2(away.z));
}

/// Point the camera at an offscreen image of the requested size, so the run
/// renders (and captures) without a visible window. Runs at `Startup` after
/// `free_camera::setup_camera` has spawned the camera.
pub fn setup_offscreen_target(
    mut commands: Commands,
    harness: Res<ShotHarness>,
    mut images: ResMut<Assets<Image>>,
    // `RenderTarget` is its own component in Bevy 0.19 (required by `Camera`).
    mut camera: Query<&mut RenderTarget, With<FreeCamera>>,
) {
    let mut image = Image::new_fill(
        Extent3d {
            width: harness.size.x,
            height: harness.size.y,
            depth_or_array_layers: 1,
        },
        TextureDimension::D2,
        &[0, 0, 0, 255],
        TextureFormat::Rgba8UnormSrgb,
        RenderAssetUsages::RENDER_WORLD,
    );
    // RENDER_ATTACHMENT to draw into it, COPY_SRC so the screenshot readback can
    // pull it back to the CPU.
    image.texture_descriptor.usage =
        TextureUsages::TEXTURE_BINDING | TextureUsages::COPY_SRC | TextureUsages::RENDER_ATTACHMENT;
    let handle = images.add(image);
    if let Ok(mut target) = camera.single_mut() {
        *target = RenderTarget::Image(handle.clone().into());
    }
    commands.insert_resource(ShotTarget(handle));
}

/// Put the camera on the current shot's target as soon as the map anchor
/// exists, reusing the normal teleport path (so a scripted run lands exactly
/// where a bookmark jump would).
pub fn place_camera(
    harness: Res<ShotHarness>,
    mut progress: ResMut<ShotProgress>,
    mut nav: ResMut<GeoNavState>,
    mut dev: ResMut<crate::resources::DevPanelState>,
    map_anchor: Res<MapAnchor>,
) {
    if progress.placed || map_anchor.geo.is_none() {
        return;
    }
    let Some(shot) = harness.shots.get(progress.index) else {
        return;
    };
    progress.placed = true;
    progress.elapsed = 0.0;
    progress.quiet = 0.0;
    progress.was_busy.clear();
    progress.placed_at = Some(bevy::platform::time::Instant::now());
    progress.frames = 0;
    progress.frame_time = 0.0;
    progress.frame_time_max = 0.0;
    // Baselines for the streaming-account columns: everything fetched or decoded
    // from here to the settle verdict belongs to this shot.
    progress.fetch_base = crate::platform::http::stats::snapshot();
    progress.worker_base = crate::platform::workers::stats::snapshot();
    // `--shot-mode avatar`: third-person follow — the camera frames the avatar
    // (teleported to `--at`) instead of sitting at a fixed point. `--at` altitude
    // becomes the follow distance and `--look` (pitch°, yaw°) the orbit
    // elevation/azimuth around the avatar. This is the right mode for checking
    // avatars (free/earth put the camera somewhere the avatar may not be).
    // Time of day and weather are per-shot in a preset manifest: the same city
    // is a different picture at 09:30 and at dusk, and pinning them here is what
    // makes a regenerated set look identical to the last one. Written only when
    // the shot names them (`DevPanelState` is change-detected — an unconditional
    // write would re-run the lighting recompute for nothing).
    let env = &shot.env;
    if !env.is_empty() {
        if let Some(t) = env.time {
            // A scripted run is already in Manual (see `main`), but a preset that
            // names an hour must pin the clock even if that ever changes.
            dev.sun_mode = crate::resources::SunMode::Manual;
            dev.time_of_day = t.rem_euclid(24.0);
        }
        if let Some(f) = env.fog {
            dev.fog_density = f.clamp(0.0, 1.0);
        }
        if let Some(c) = env.clouds {
            dev.cloud_cover = c.clamp(0.0, 1.0);
        }
        if let Some(r) = env.rain {
            dev.precipitation = r.clamp(0.0, 1.0);
        }
    }

    // `in_place`: the camera is already where this shot wants it, so issue no
    // teleport. Everything above still runs — the clocks are reset and the
    // streaming baselines are taken, which is what makes the capture's own
    // numbers about this capture.
    if shot.in_place {
        tracing::info!(
            shot = progress.index + 1,
            "shot harness: capturing in place"
        );
        return;
    }

    let mode = harness.mode_for(progress.index);
    let avatar_follow = mode == "avatar";
    let (azimuth, pitch, distance) = if avatar_follow {
        (
            shot.yaw_deg.to_radians(),
            shot.pitch_deg.to_radians(),
            (shot.alt as f32).clamp(1.5, 25.0),
        )
    } else {
        (0.0, 0.5, 5.0)
    };
    let pose = CameraPose {
        mode: mode.clone(),
        // free_camera_rotation is (pitch, yaw), in radians.
        free_rotation: [shot.pitch_deg.to_radians(), shot.yaw_deg.to_radians()],
        azimuth,
        pitch,
        distance,
        // Earth mode: apply_teleport recomputes the orbit distance from
        // `altitude`, so this only needs to be non-zero.
        orbit_distance: shot.alt,
        orbit_yaw: shot.yaw_deg.to_radians(),
        orbit_pitch: 1.55,
    };
    nav.pending_teleport = Some(Teleport {
        lat: shot.lat,
        lng: shot.lng,
        altitude: Some(shot.alt),
        pose: Some(pose),
        // A scripted capture places the camera; it does not visit anywhere. The
        // arrival animation would put a half-formed avatar in the shot of every
        // preset that frames one.
        silent: true,
    });
    tracing::info!(
        shot = progress.index + 1,
        of = harness.shots.len(),
        name = shot.name.as_deref().unwrap_or("-"),
        about = shot.description.as_deref().unwrap_or(""),
        lat = shot.lat,
        lng = shot.lng,
        alt = shot.alt,
        mode = %mode,
        "shot harness: camera placed"
    );
}

/// Keep a `--shot-mode avatar` run in Follow, which nothing else will.
///
/// **Follow is not on offer offline** (`free_camera::follow_available` wants a
/// session), and every scripted run *is* offline — `--shot` implies it. So
/// `camera_mode_toggle` dropped the camera to Fly on the frame after the
/// teleport put it in Follow, and `--shot-mode avatar` — the mode this harness
/// documents as "the right mode for checking avatars" — silently produced a
/// free-camera view of wherever the avatar was not. It also stood a seated
/// avatar up, because `handle_avatar_movement` leaves the seat when the camera
/// leaves Follow.
///
/// The harness owns the camera for the length of a scripted run, so it re-asserts
/// the mode every frame rather than once at the teleport. Ordered after the
/// toggle that would otherwise undo it.
/// Point the camera at [`ShotSpec::look_at`], every frame, from where it really is.
///
/// **A pitch is a guess about the ground; this is not.** `--look` aims by angle,
/// so what lands in the middle of the frame is `altitude / tan(pitch)` metres
/// ahead — and `altitude` is measured from the map anchor's ground while the
/// subject stands on the measured ground somewhere else. With the terrain layer
/// on, the two differ by metres, which a steep shot barely notices and a shallow
/// one cannot survive: at six degrees, three metres of height error moves the aim
/// thirty metres. Every low capture of a tunnel entrance in this feature's history
/// missed by between sixty and a hundred and twenty metres, and the angles were
/// right every time.
///
/// Run per frame rather than once at placement because the terrain field lands a
/// second or two after the anchor it belongs to, so the ground under the camera —
/// and under the target — moves after the teleport.
pub fn aim_scripted_camera(
    harness: Res<ShotHarness>,
    progress: Res<ShotProgress>,
    anchor: Res<MapAnchor>,
    terrain: Res<crate::systems::terrain::TerrainField>,
    mut cam_state: ResMut<CameraState>,
    camera: Query<(&GridCell, &GlobalTransform), With<FreeCamera>>,
) {
    if !progress.placed {
        return;
    }
    let Some(target) = harness.shots.get(progress.index).and_then(|s| s.look_at) else {
        return;
    };
    let (Some(geo), Ok((cell, tf))) = (anchor.geo, camera.single()) else {
        return;
    };
    let (tx, tz) = crate::systems::geo_nav::lat_lng_to_world(geo, target[0], target[1]);
    let (tx, tz) = (tx as f32, tz as f32);
    let here = crate::grid::anchored_position(cell, tf.translation());
    let to = Vec3::new(tx, terrain.at(tx, tz), tz) - here;
    if to.length_squared() < 1.0 {
        return;
    }
    // `free_camera_rotation` is (pitch, yaw) in radians, yaw measured the way the
    // camera's own controls measure it.
    let yaw = (-to.x).atan2(-to.z);
    let pitch = to.y.atan2(to.x.hypot(to.z));
    let want = Vec2::new(pitch, yaw);
    if cam_state.free_camera_rotation.distance(want) > 1e-4 {
        cam_state.free_camera_rotation = want;
    }
}

pub fn hold_scripted_camera_mode(
    harness: Res<ShotHarness>,
    progress: Res<ShotProgress>,
    mut cam_state: ResMut<CameraState>,
) {
    if !progress.placed || harness.mode_for(progress.index) != "avatar" {
        return;
    }
    if cam_state.mode != crate::resources::CameraMode::Avatar {
        cam_state.mode = crate::resources::CameraMode::Avatar;
    }
}

/// `--ui firstperson` zooms the follow camera all the way into the avatar's head.
///
/// The state itself is one boolean, so this seeds it rather than driving the zoom
/// gesture that normally sets it — a scripted run has no wheel, no pinch and no
/// on-screen buttons, and reproducing the gesture would be testing the input
/// plumbing rather than the view.
///
/// Pair it with `--ui locate` for the half a phone does: with both, the *device*
/// aims the camera (`track_scripted_device` seeds an attitude), which is the only
/// way to see that path at all outside a hand holding a phone. Held every frame,
/// like the seat and for the same reason: a teleport re-enters Follow.
pub fn zoom_scripted_first_person(
    harness: Res<ShotHarness>,
    progress: Res<ShotProgress>,
    mut cam_state: ResMut<CameraState>,
) {
    let panels = harness.ui_for(progress.index);
    let want = panels.iter().any(|u| u == "all" || u == "firstperson");
    if cam_state.first_person != want {
        cam_state.first_person = want;
    }
}

/// `--ui sit` seats the avatar, and it has to invent the bench to do it.
///
/// The real prompt only appears with a real bench inside
/// [`interact::INTERACT_RADIUS`](crate::systems::interact::INTERACT_RADIUS), and
/// where a streamed city puts its benches is not something a scripted camera
/// position can promise. So the seat here is synthetic: half a metre ahead of the
/// avatar, facing the way it already faces, and fully slid onto (`blend: 1.0` — a
/// still of the half-way point documents nothing). What the capture then shows is
/// the authored pose (`systems::sit_pose`), which is the half of this feature with
/// no test that can see it. Pair it with `--shot-mode avatar`, or the camera is
/// not looking at the avatar.
///
/// Unlike the rest of the `--ui` seeding this runs **every frame and after the
/// teleport**, because a teleport clears the seat — rightly, since the offset a
/// seat holds points at a bench in the city being left. Seeding it once per shot
/// would therefore seed it and lose it in the same frame. It also needs
/// [`hold_scripted_camera_mode`], for the same reason and one more: leaving
/// Follow stands a seated avatar up.
pub fn seat_scripted_avatar(
    harness: Res<ShotHarness>,
    progress: Res<ShotProgress>,
    mut avatar_state: ResMut<crate::resources::AvatarState>,
) {
    let panels = harness.ui_for(progress.index);
    let all = panels.iter().any(|u| u == "all");
    let want = |k: &str| all || panels.iter().any(|u| u == k);
    let facing = avatar_state.facing_yaw;
    avatar_state.seated = want("sit").then(|| crate::resources::Seated {
        offset: Vec2::new(facing.sin(), facing.cos()) * 0.5,
        yaw: facing,
        blend: 1.0,
        rising: false,
    });
}

/// `--ui building` selects the building nearest the avatar, so a capture has the
/// card and the glass to photograph; `--ui inside` does that and then presses the
/// card's Look inside (`systems::cutaway`), so a capture from outside shows the
/// building with its roof off and the floor picker beside it.
///
/// **Nearest, not clicked.** The pick is a ray through a cursor and a headless run
/// has neither, so the thing under test — the card's layout, the highlight's
/// placement on a real register outline — is reachable only by naming a building
/// some other way. Picking the nearest one is the only choice a scripted camera
/// position can make that is repeatable: which building *is* nearest depends on
/// where the shot put the avatar, which the preset already states.
///
/// It runs every frame rather than once per shot, because the facts arrive over
/// several seconds and a selection made before the BAG cell landed would name
/// nothing. Once something is selected it leaves it alone — reselecting each frame
/// would rebuild the glass mesh at frame rate.
pub fn select_scripted_building(
    harness: Res<ShotHarness>,
    progress: Res<ShotProgress>,
    mut selection: ResMut<crate::systems::building_select::BuildingSelection>,
    facts: Res<crate::systems::building_facts::BuildingFacts>,
    avatar: Res<crate::resources::AvatarState>,
    anchor: Res<crate::resources::MapAnchor>,
    mut cutaway: ResMut<crate::systems::cutaway::Cutaway>,
) {
    let panels = harness.ui_for(progress.index);
    let all = panels.iter().any(|u| u == "all");
    let inside = panels.iter().any(|u| u == "inside");
    if !(all || inside || panels.iter().any(|u| u == "building")) {
        selection.clear();
        return;
    }
    if let Some(id) = selection.selected.clone() {
        if inside && cutaway.looking.is_none() {
            cutaway.look_inside(id);
        }
        return;
    }
    let Some(geo) = anchor.geo else {
        return;
    };
    let (lat, lng) = crate::systems::geo_nav::world_to_lat_lng(
        geo,
        f64::from(avatar.position.x),
        f64::from(avatar.position.z),
    );
    // Distance to the outline's first vertex, which is what `BuildingFacts::near`
    // already measures against and is exact enough at these ranges — a building is
    // metres across and the nearest two are rarely a tie.
    let nearest = facts
        .near(lat, lng, 80.0)
        .into_iter()
        .filter_map(|(id, b)| {
            let &(blat, blng) = b.footprint.first()?;
            let d = (blat - lat).powi(2) + (blng - lng).powi(2);
            Some((d, id))
        })
        .min_by(|a, b| a.0.total_cmp(&b.0));
    if let Some((_, id)) = nearest {
        selection.selected = Some(std::sync::Arc::from(id));
    }
}

/// `--ui locate` lights the device-tracking switch and hands it a fix, so a
/// capture can photograph the arrow that marks where the phone is
/// (`systems::locate::render_locate_arrow`).
///
/// **The fix is invented, and it has to be.** There is no location provider on a
/// desktop or in a container — `LocationWatch::available()` is `false` and
/// nothing is ever published — so without this the arrow is a feature that
/// cannot be looked at on any machine that can take a picture of it.
///
/// **Where** it is invented took one wasted capture to get right. The obvious
/// answer, `--at` itself, is the *camera's* position in Fly and Earth, not the
/// point it is looking at: from 900 m with the view pitched 55° down, a marker
/// at `--at` is directly beneath the lens and lands off the bottom of the frame.
/// The picture came back with the locate button lit and a bare map, which reads
/// exactly like a broken overlay. So the fix goes where the look ray meets the
/// ground — `alt / tan(pitch)` along the yaw bearing — and a level or upward
/// look, which never meets it, falls back to the camera's own coordinate rather
/// than to an infinity.
///
/// The heading is invented too (north-east, so a mirrored or transposed rotation
/// is obvious in the image — due north would look identical to a broken one).
/// Follow mode is skipped for the same reason the real system skips it: there the
/// avatar is the marker.
///
/// Every frame rather than once per shot, because the position it seeds is read
/// from the shot and a teleport can arrive after the seeding would have run.
pub fn track_scripted_device(
    harness: Res<ShotHarness>,
    progress: Res<ShotProgress>,
    mut tracking: ResMut<crate::systems::locate::LocateTracking>,
    mut cam_state: ResMut<CameraState>,
) {
    let panels = harness.ui_for(progress.index);
    let all = panels.iter().any(|u| u == "all");
    if !(all || panels.iter().any(|u| u == "locate")) {
        if tracking.active {
            tracking.exit();
        }
        if cam_state.device_rotation.is_some() {
            cam_state.device_rotation = None;
        }
        return;
    }
    let Some(shot) = harness.shots.get(progress.index) else {
        return;
    };
    let down = (-shot.pitch_deg).to_radians();
    let (lat, lng) = if down > 0.02 {
        let ahead = shot.alt / f64::from(down.tan());
        crate::systems::locate::step_along_bearing(
            shot.lat,
            shot.lng,
            f64::from(shot.yaw_deg),
            ahead,
        )
    } else {
        (shot.lat, shot.lng)
    };
    // Aiming the camera is the consumer's job (`locate::track_device_pose`) and
    // in a scripted run that system finds no provider and clears it — so with
    // `--ui firstperson,locate` the seed has to make the same write, or the one
    // path a phone exists for is the one a capture cannot photograph. Snapped
    // rather than eased: a capture is a settled frame, and there is nothing to
    // ease from.
    let attitude = crate::platform::orientation::DeviceAttitude {
        accuracy_deg: Some(10.0),
        // Held up and aimed along the shot's own yaw, tipped a little above the
        // horizon — the pose a person points a phone at a building in, and the one
        // `--ui firstperson` needs to photograph anything but the pavement. The
        // elevation is *not* the shot's pitch: a scripted `--at` is usually
        // looking down at the map, and a phone is not.
        ..crate::platform::orientation::DeviceAttitude::pointing(f64::from(shot.yaw_deg), 5.0, 0.0)
    };
    let aimed = crate::systems::locate::device_aims_the_view(&cam_state)
        .then(|| attitude.camera_rotation());
    if cam_state.device_rotation != aimed {
        cam_state.device_rotation = aimed;
    }
    tracking.seed_for_capture(
        crate::platform::location::GpsFix {
            lat,
            lng,
            accuracy_m: 8.0,
            speed_ms: Some(1.4),
            heading_deg: Some(45.0),
        },
        Some(attitude),
    );
}

/// `--shot-mode earth` above the space threshold: put the orbit camera straight
/// into its space tier over the target. The ordinary teleport path caps the
/// surface orbit at 250 km (`ORBIT_MAX_DISTANCE_SURFACE`) and lets the handoff
/// promote it from there, which is right for a user jump but leaves a scripted
/// "show me the globe from 15,000 km" run parked just above the atmosphere.
/// Runs after the teleport so it has the last word.
pub fn place_space_orbit(
    harness: Res<ShotHarness>,
    progress: Res<ShotProgress>,
    globe: Res<GlobeState>,
    mut cam_state: ResMut<CameraState>,
    mut done: Local<usize>,
) {
    let Some(shot) = harness.shots.get(progress.index) else {
        return;
    };
    // Once per shot, and only for an earth-mode run that wants to be in space.
    if !progress.placed
        || *done > progress.index
        || harness.mode_for(progress.index) != "earth"
        || !globe.ready
    {
        return;
    }
    *done = progress.index + 1;
    if shot.alt <= 60_000.0 {
        return;
    }
    cam_state.orbit_space = true;
    cam_state.orbit_initialized = true;
    cam_state.orbit_dir = globe.direction_local(shot.lat, shot.lng).as_dvec3();
    cam_state.orbit_distance = globe.radius as f64 + shot.alt;
    cam_state.orbit_distance_target = cam_state.orbit_distance;
    // Heading 0: the globe comes up north-up, like every other space arrival.
    cam_state.orbit_heading = 0.0;
}

/// Everything [`drive_capture`] reads that is neither the camera nor the
/// harness's own bookkeeping: the streamers the settle test waits on, and the
/// resources each [`ShotMetrics`] field is measured from.
///
/// Bundled because `drive_capture` is at Bevy's sixteen-parameter ceiling, and a
/// new inspectable fact is exactly the kind of thing that keeps being added to
/// it. It was a **positional tuple** until 2026-08-21, which made that ceiling
/// bite twice over: new readouts had to be *appended* so the existing `.0`–`.15`
/// indices kept their meaning, the sixteenth had to nest a tuple inside itself
/// (`readouts.15 .0`), and every use site named a number instead of a thing. A
/// `SystemParam` struct has no element limit and no positions to preserve — add a
/// field wherever it belongs and read it by name. `RenderCensus`, `AvatarReadout`
/// and [`ScriptedCards`] are the same move; this was the one holdout.
#[derive(bevy::ecs::system::SystemParam)]
pub struct ShotReadouts<'w, 's> {
    globe_albedo: Res<'w, GlobeSurfaceAlbedo>,
    vegetation: Res<'w, crate::systems::vegetation::VegetationLod>,
    height: Res<'w, crate::systems::height::HeightLayer>,
    terrain: Res<'w, crate::systems::terrain::TerrainField>,
    road_mask: Res<'w, crate::systems::terrain::RoadMask>,
    map_anchor: Res<'w, MapAnchor>,
    basemap_holes: Res<'w, crate::systems::map_stream::BasemapHoles>,
    ground_detail: Res<'w, crate::systems::ground_detail::GroundDetail>,
    water_levels: Res<'w, crate::systems::water_levels::WaterLevels>,
    furniture: Res<'w, crate::systems::street_furniture::FurnitureLod>,
    lod22: Res<'w, crate::systems::lod22::Lod22Buildings>,
    adapter: Res<'w, bevy::render::renderer::RenderAdapterInfo>,
    /// The resolved graphics preferences, for the columns that report what a
    /// setting *decided* rather than what the scene ended up looking like. The
    /// two are not the same question and conflating them is what made the light
    /// shafts take four GPU runs to diagnose.
    prefs: Res<'w, crate::resources::AppPrefs>,
    avatar: AvatarReadout<'w, 's>,
    avatar_state: Res<'w, crate::resources::AvatarState>,
    census: crate::systems::render_census::RenderCensus<'w, 's>,
    /// The wall clock, for the frame-time columns only. Everything else the
    /// harness schedules against is deliberately the *virtual* clock — see
    /// [`ShotProgress::frame_time`] for why the two must not be confused.
    real: Res<'w, Time<Real>>,
    /// The run's worst upload frame, which is a high-water mark and therefore the
    /// one metric here that describes a moment other than the shutter.
    uploads: Res<'w, crate::systems::upload_meter::UploadMeter>,
    /// wgpu's own allocator totals.
    render_device: Res<'w, bevy::render::renderer::RenderDevice>,
    /// The grass pool, which is both a metric and a settle input: a capture taken
    /// while its coverage mask is still on a worker photographs a lawn that has
    /// not arrived.
    grass: Res<'w, crate::systems::grass::GrassPatches>,
    /// The Overpass street-detail streamer and its coarse sibling, the
    /// tall-structure lane — settle inputs (a ground-level capture without its
    /// lamps and trees photographs a half-built street) and the `prop_meshes`
    /// metric.
    street_detail: Res<'w, crate::systems::street_detail::StreetDetail>,
    tall_structures: Res<'w, crate::systems::tall_structures::TallStructures>,
    /// The BAG attribute index (`systems::building_facts`) — a metric only, never
    /// a settle input. Nothing waits for it: a building whose facts have not
    /// landed is drawn with the generated palette, which is a different picture
    /// but not an unfinished one.
    bag: Res<'w, crate::systems::building_facts::BuildingFacts>,
    /// The per-cell places store, for the surveyed counts alone — see
    /// `ShotMetrics::surveyed_trees`.
    places: Res<'w, crate::systems::world_places::WorldPlaces>,
    /// The near-camera interior layer, for its room count alone.
    interiors: Res<'w, crate::systems::interiors::Interiors>,
    tunnels: Res<'w, crate::systems::tunnels::Tunnels>,
    /// The raised decks, for how well they meet their approaches.
    decks: Res<'w, crate::systems::map_geometry::DeckPlates>,
    /// The same layer's support field, for its storey tally.
    interior_floors: Res<'w, crate::systems::interiors::InteriorFloors>,
    /// What is selected and what the avatar is standing in — see
    /// `ShotMetrics::building_selected`.
    building_selection: Res<'w, crate::systems::building_select::BuildingSelection>,
    /// What is cut away — see `ShotMetrics::cutaway_storey`.
    cutaway: Res<'w, crate::systems::cutaway::Cutaway>,
    /// The flood defences, for their two counts alone — see
    /// `ShotMetrics::flood_defences`.
    dikes: Res<'w, crate::systems::dikes::Dikes>,
    /// The sewers — see `ShotMetrics::sewer_pipes`.
    sewers: Res<'w, crate::systems::sewers::Sewers>,
    /// The other underground layers — see `ShotMetrics::underground_shapes`.
    underground: Res<'w, crate::systems::underground::Underground>,
}

/// Wait for the view to settle, capture, then move to the next shot (or exit).
#[allow(clippy::too_many_arguments)]
/// The three "is it actually on screen" queries, in **one** parameter.
///
/// `drive_capture` sits on Bevy's 16-parameter ceiling — the same wall
/// `camera_controls` and `update_map_surfaces` hit, and for the same reason:
/// there is one of everything to measure. A new fact therefore arrives bundled,
/// not as its own argument. Adding `agents` bare took the system to seventeen
/// and it silently stopped being a system at all: the compiler's complaint was
/// that `.after()` did not exist on it, which is several steps from the cause.
#[derive(bevy::ecs::system::SystemParam)]
pub struct VisualCounts<'w, 's> {
    globe: Query<
        'w,
        's,
        (
            &'static bevy::camera::visibility::ViewVisibility,
            &'static Visibility,
        ),
        With<GlobeMesh>,
    >,
    route: Query<
        'w,
        's,
        &'static bevy::camera::visibility::ViewVisibility,
        With<crate::components::RouteVisual>,
    >,
    /// The tunnels' own geometry. See `ShotMetrics::tunnel_meshes`.
    tunnels: Query<
        'w,
        's,
        &'static bevy::camera::visibility::ViewVisibility,
        With<crate::components::TunnelSurface>,
    >,
    /// The glass shells over the selected and occupied buildings
    /// (`systems::building_select`). See `ShotMetrics::building_shell_meshes`.
    shells: Query<
        'w,
        's,
        &'static bevy::camera::visibility::ViewVisibility,
        With<crate::systems::building_select::BuildingShell>,
    >,
    agents: Query<
        'w,
        's,
        (
            &'static crate::systems::traffic::AgentKind,
            &'static Visibility,
        ),
    >,
    /// The particle emitters in the world (`systems::particles`). Bundled here
    /// rather than added as its own argument for the reason above — the system is
    /// at Bevy's parameter ceiling.
    particles: Query<'w, 's, &'static crate::systems::particles::ParticleEmitter>,
    /// The light-shaft fog volumes and where they actually are in render space
    /// (`systems::light_shafts`). `GlobalTransform`, not `Transform`, precisely
    /// because the bug this metric exists for is the difference between the two.
    shafts:
        Query<'w, 's, &'static GlobalTransform, With<crate::systems::light_shafts::ShaftVolume>>,
}

/// Every streamer the settle test watches, as one `SystemParam`.
///
/// **"Every" is load-bearing and was learned the hard way.** The test used to
/// watch only the two tile streams — and `MapStream::streaming_busy` was stuck
/// true for the life of the process (a fetch was never dropped from `requested`),
/// so every ground shot silently ran to the `--wait` cap instead. Fixing that bug
/// turned each README capture into a half-built city: the imagery goes quiet in
/// ~4 s, while the vector surfaces, the OSM detail patch and the 3DBAG cells are
/// still arriving. The accidental 45 s sleep had been doing the waiting, so the
/// test has to name the streamers it was covering for.
///
/// It is no longer a list written here. It was, and it fell behind — the water
/// levels, the dikes, the BAG facts and the data layers were never on it — so
/// the list is now [`StreamRegistry`](crate::systems::stream_watch::StreamRegistry),
/// filled by the plugin that initialises each resource
/// (`app.watch_stream::<R>()`, `systems::stream_watch`). The relief backlog, the
/// route solve and the live weather sample are registered the same way.
///
/// It is a parameter of its own rather than a block inside [`drive_capture`]
/// because it has a second caller: `systems::mcp`'s `state` tool answers "is the
/// world finished, and if not, what is it still doing" **without taking a
/// picture** — the cheapest way there is to check a world view. A second list of
/// streamers maintained beside this one would answer it differently within a
/// month.
#[derive(bevy::ecs::system::SystemParam)]
pub struct StreamWatch<'w> {
    registry: Res<'w, crate::systems::stream_watch::StreamRegistry>,
}

impl StreamWatch<'_> {
    /// Which streamers are still working, by name.
    ///
    /// **Names, not a single `||` chain.** When a capture gives up at `--wait`,
    /// *which* streamer was still going is the entire diagnosis, and a bare "not
    /// settled" sends you back to re-run with a longer cap just to learn it.
    ///
    /// As of the last [`StreamReport`](crate::systems::stream_watch::StreamReport):
    /// this frame's for `drive_capture`, which is ordered after it.
    pub fn busy(&self) -> Vec<&'static str> {
        self.registry.busy()
    }
}

pub fn drive_capture(
    mut commands: Commands,
    time: Res<Time>,
    harness: Res<ShotHarness>,
    target: Option<Res<ShotTarget>>,
    mut progress: ResMut<ShotProgress>,
    mut shot_state: ResMut<ScreenshotState>,
    mut exit: MessageWriter<AppExit>,
    globe: Res<GlobeState>,
    stream: Res<MapStream>,
    // The settle test's inputs, as one parameter — see `StreamWatch`. The globe
    // and the tile stream are named above because the metrics read altitude,
    // tier and tile counts off them, which is a different question from "is it
    // still working".
    watch: StreamWatch,
    cam_state: Res<CameraState>,
    readouts: ShotReadouts,
    // `Option<&Bloom>`: a tier that wants no glare omits the component rather than
    // zeroing it (`free_camera::setup_camera`), and naming it bare here would drop
    // the camera out of this whole query — every metric below it, not just
    // `bloom`, would silently report its default. A metric that goes quiet is
    // worse than one that reads 0.
    camera: Query<
        (
            &GridCell,
            &Transform,
            &Projection,
            &Exposure,
            Option<&Bloom>,
            // The volumetric pass itself. `Has`, not `Option<&…>`: nothing here
            // reads its settings, only whether `render_features` put it on.
            Has<bevy::light::VolumetricFog>,
        ),
        With<FreeCamera>,
    >,
    visuals: VisualCounts,
) {
    if let Some(frames) = progress.exit_in {
        finish_run(frames, &mut progress, &mut exit);
        return;
    }
    // Capture landed: on to the next viewpoint, or done. The CPU pool was paused
    // for the readback (see below); release it here so the next shot's streamers —
    // and the jobs parked during this one — start again.
    //
    // `perf_release` is the second way in, for a `--perf` run: the suite holds the
    // run for a measurement window after the shutter, and by the time that window
    // closes the one-shot `saved` flag is long consumed. See `ShotProgress::perf_hold`.
    let landed = progress.saved.swap(false, Ordering::Relaxed);
    if landed || progress.perf_release {
        advance_after_capture(&harness, &mut progress);
        return;
    }
    if !progress.placed {
        return;
    }
    progress.elapsed += time.delta_secs();
    // Only count settled frames: the first second after a teleport is dominated
    // by tile decode/upload, not by the steady-state render cost being measured.
    //
    // `real`, not `time`: the virtual clock is clamped at `max_delta` and, under
    // `--shot`, deliberately stepped — it measures the scripted run's intent, which
    // is what `elapsed`/`quiet` above want and exactly what a frame time does not.
    if progress.quiet > 0.0 {
        let frame = readouts.real.delta_secs();
        progress.frames += 1;
        progress.frame_time += frame;
        progress.frame_time_max = progress.frame_time_max.max(frame);
    }
    if progress.fired {
        await_readback(
            &mut commands,
            &harness,
            &mut progress,
            target.as_deref(),
            &mut shot_state,
            &mut exit,
        );
        return;
    }

    if !settle_verdict(&watch, &harness, &time, &mut progress) {
        return;
    }
    let verdict = progress.verdict.clone().expect("set on the branch above");
    let settled = verdict.settled;

    // Let the paused pool drain before firing, so the readback finds a free thread
    // rather than the tail of the backlog. On a settled shot the streamers are
    // already quiet and this passes on the first frame; on a `--wait`-capped one it
    // is the difference between an image and `capture never completed`.
    let outstanding = crate::platform::workers::outstanding();
    if outstanding > 0 {
        if progress.elapsed - verdict.at < QUIESCE_MAX_S {
            return;
        }
        tracing::warn!(
            outstanding,
            budget_s = QUIESCE_MAX_S,
            "shot harness: CPU pool still busy at the quiesce cap — firing anyway"
        );
    }

    // The numbers behind the image, so a black frame can be diagnosed without
    // re-running: a black *scene* still reports visible, rendered globe meshes.
    // Built once and reused for the log line, the `--csv` row and the JSON dump.
    if let Ok((cell, tf, projection, exposure, bloom, shaft_fog)) = camera.single() {
        record_metrics(
            ShotCamera {
                cell,
                tf,
                projection,
                exposure,
                bloom,
                shaft_fog,
            },
            &harness,
            &mut progress,
            &verdict,
            settled,
            &globe,
            &stream,
            &cam_state,
            &readouts,
            &visuals,
        );
    }

    fire_capture(
        &mut commands,
        &harness,
        &mut progress,
        target.as_deref(),
        &mut shot_state,
    );
}

/// The run is over: count down the frames left before exiting, then exit with
/// the verdict the expectations reached.
fn finish_run(frames: u32, progress: &mut ShotProgress, exit: &mut MessageWriter<AppExit>) {
    let left = frames.saturating_sub(1);
    progress.exit_in = Some(left);
    if left == 0 {
        #[cfg(not(target_arch = "wasm32"))]
        crate::systems::aerial_reference::wait_all();
        // The one place the harness gets to have an opinion about the world it
        // photographed. Without `--expect` this is always Success and only a
        // render failure can fail the run — which is exactly why a caller that
        // cannot look at the image needs the flag.
        if progress.unmet > 0 {
            tracing::error!(
                unmet = progress.unmet,
                "shot harness: expectations not met — see the `shot expect failed` lines above"
            );
            exit.write(AppExit::error());
        } else {
            exit.write(AppExit::Success);
        }
    }
}

/// A capture has landed (or a `--perf` window closed): move on to the next
/// viewpoint, park a live harness, or start the countdown to exit.
fn advance_after_capture(harness: &ShotHarness, progress: &mut ShotProgress) {
    if progress.perf_hold {
        // Measuring; hand the suite the shutter edge rather than letting it
        // race for `saved`, which has just been consumed above.
        progress.perf_captured = true;
        return;
    }
    progress.perf_release = false;
    crate::platform::workers::set_paused(false);
    progress.index += 1;
    // Reset unconditionally, including when the list has run out: a live
    // harness is handed its next viewpoint later, and it must not arrive to
    // find the previous one's `placed`/`fired`/`verdict` still set.
    progress.placed = false;
    progress.fired = false;
    progress.retried = false;
    progress.verdict = None;
    progress.perf_captured = false;
    if progress.index >= harness.shots.len() {
        if harness.live {
            // Park. `shot_in_progress` is false from here, so the holds let
            // go of the camera and the client is the user's again until the
            // next `view` call pushes a viewpoint.
            progress.trim_metrics();
            return;
        }
        // Every viewpoint captured — flush the metrics before draining to exit.
        harness.write_metrics(&progress.metrics);
        progress.exit_in = Some(2);
    }
}

/// The shutter has fired and the readback has not landed: wait for it, re-fire
/// once, or give up on the capture.
fn await_readback(
    mut commands: &mut Commands,
    harness: &ShotHarness,
    mut progress: &mut ShotProgress,
    target: Option<&ShotTarget>,
    mut shot_state: &mut ScreenshotState,
    exit: &mut MessageWriter<AppExit>,
) {
    // The readback normally lands within a frame or two; don't hang forever.
    //
    // Budgeted from the moment it fired, and *not* as `wait + N`: the two
    // measure unrelated things, so shortening `--wait` to capture a
    // deliberately half-streamed world also shortened the time allowed for the
    // readback. Measured on a software rasteriser (lavapipe, ground level,
    // frames running 0.7–2 s): `--wait 5` gave the readback 20 s, it needed
    // more, and the run died with `capture never completed` and no PNG — a
    // failure invented entirely by the coupling.
    if progress.elapsed - progress.fired_at > CAPTURE_GRACE_S {
        // Re-fire once before giving up. A `Screenshot` gets exactly **one**
        // frame: `bevy_render`'s `extract_screenshots` inserts `Capturing` and
        // its own query is `Without<Capturing>`, while `prepare_screenshots`
        // clears its prepared set every frame — so if that single frame's
        // prepare skips the entity (an unknown render-target image, a
        // pipeline not yet compiled) the request is dropped silently and no
        // later frame reconsiders it. Nothing distinguishes that from a slow
        // readback except trying again, and a second shutter costs one frame.
        if !progress.retried {
            progress.retried = true;
            tracing::warn!(
                grace_s = CAPTURE_GRACE_S,
                outstanding = crate::platform::workers::outstanding(),
                "shot harness: no readback within the grace — re-firing the shutter once"
            );
            fire_capture(
                &mut commands,
                &harness,
                &mut progress,
                target.as_deref(),
                &mut shot_state,
            );
            return;
        }
        tracing::error!(
            grace_s = CAPTURE_GRACE_S,
            outstanding = crate::platform::workers::outstanding(),
            "shot harness: capture never completed"
        );
        // Leaving the pool paused would strand every job parked behind this
        // capture; the run is ending, but `AppExit` is not immediate.
        crate::platform::workers::set_paused(false);
        // A live harness outlives its failures. The caller still gets an
        // answer — the metrics row is pushed at the shutter, so a capture
        // that never read back has its numbers and only lacks its pixels —
        // and the next `view` call is not asked to pay for this one.
        if harness.live {
            progress.index += 1;
            progress.placed = false;
            progress.fired = false;
            progress.retried = false;
            progress.verdict = None;
            progress.trim_metrics();
            return;
        }
        // Hand back whatever the run *did* measure, including this shot's own
        // row — the metrics are pushed at the shutter, so a capture that never
        // read back still has its numbers. Only the completion path used to
        // write them, which threw the whole set away in exactly the case that
        // needs it most: `ci.yml` catches a failed smoke run and `cat`s the
        // dump to explain it, and got "(no dump: the run died before it
        // captured)" for a run that had captured four of six viewpoints.
        //
        // Nothing measured is left as no file at all, rather than as an empty
        // array: that is the case the CI message is already right about, and a
        // present-but-empty dump would read as "measured, and found nothing".
        if !progress.metrics.is_empty() {
            harness.write_metrics(&progress.metrics);
        }
        exit.write(AppExit::error());
    }
}

/// The settle test: has every streamer been quiet for `--settle`, or has
/// `--wait` run out? `false` while neither; on the frame one first holds, records
/// the verdict and the streaming account for the shot and pauses the CPU pool.
fn settle_verdict(
    watch: &StreamWatch,
    harness: &ShotHarness,
    time: &Time,
    progress: &mut ShotProgress,
) -> bool {
    // Settle: capture once *every* streamer has been quiet for `--settle`
    // seconds, or when `--wait` runs out (whichever comes first). Which streamers
    // that is, and why the list has to be exhaustive, is `StreamWatch`.
    let busy = watch.busy();
    if progress.verdict.is_none() {
        let wall_s = progress
            .placed_at
            .map_or(0.0, |t| t.elapsed().as_secs_f32());
        for name in &busy {
            if !progress.was_busy.contains(name) {
                tracing::info!(
                    streamer = name,
                    wall_s = format!("{wall_s:.1}"),
                    "streamer busy"
                );
            }
        }
        for name in &progress.was_busy {
            if !busy.contains(name) {
                tracing::info!(
                    streamer = name,
                    wall_s = format!("{wall_s:.1}"),
                    "streamer quiet"
                );
            }
        }
        progress.was_busy.clone_from(&busy);
    }
    if busy.is_empty() {
        progress.quiet += time.delta_secs();
    } else {
        progress.quiet = 0.0;
    }
    if progress.verdict.is_none() {
        let (settle, wait) = (
            harness.settle_for(progress.index),
            harness.wait_for(progress.index),
        );
        if progress.quiet < settle && progress.elapsed < wait {
            return false;
        }
        // Which arm of the test above let us through. Everything measured below
        // describes a partially streamed world when this is false.
        progress.verdict = Some(SettleVerdict {
            settled: progress.quiet >= settle,
            busy: busy.clone(),
            at: progress.elapsed,
        });
        // The streaming backlog, sampled here and only here. The quiesce below
        // drains the pool to zero before the shutter, so the same reading taken
        // with the rest of the metrics would always be 0 and would describe the
        // harness rather than the world.
        progress.worker_outstanding = crate::platform::workers::outstanding();
        // The streaming account for this shot's window, closed at the same
        // moment and for the same reason: the quiesce that follows finishes
        // decodes on the harness's clock, not the world's.
        progress.fetch_window = crate::platform::http::stats::snapshot().since(progress.fetch_base);
        // Cumulative for the process rather than per shot: these answer "what
        // did this load fetch and what did the cache hand back", which is a
        // question about a run, and a one-shot run is how a load is timed.
        for (route, r) in crate::platform::http::stats::routes() {
            tracing::info!(
                route = %route,
                requests = r.requests,
                failed = r.failed,
                kb = r.bytes / 1024,
                ms = r.us / 1000,
                "fetch route"
            );
        }
        for (kind, hits, misses) in crate::platform::storage::stats::reads() {
            tracing::info!(kind = %kind, hits, misses, "cache reads");
        }
        progress.worker_window = crate::platform::workers::stats::snapshot()
            .into_iter()
            .map(|(label, totals)| {
                let base = progress
                    .worker_base
                    .iter()
                    .find(|(l, _)| *l == label)
                    .map_or_else(Default::default, |(_, t)| *t);
                (label, totals.since(base))
            })
            .filter(|(_, t)| t.jobs > 0)
            .collect();
        // Stop feeding the CPU pool *before* the shutter. On native the pool that
        // rasterises tiles is `AsyncComputeTaskPool`, and so is the pool Bevy
        // detaches the screenshot readback onto — with blocking 120 ms jobs and
        // three threads on an 8-core host, the readback is FIFO'd behind a queue the
        // streamers keep refilling and can go unpolled indefinitely. Everything the
        // streamers do from here is invisible in this frame's image anyway.
        crate::platform::workers::set_paused(true);
    }
    true
}

/// The free camera's components `record_metrics` reads, out of `drive_capture`'s
/// camera query.
struct ShotCamera<'a> {
    cell: &'a GridCell,
    tf: &'a Transform,
    projection: &'a Projection,
    exposure: &'a Exposure,
    bloom: Option<&'a Bloom>,
    shaft_fog: bool,
}

/// Measure this shot's [`ShotMetrics`], log the `shot state` line, check the
/// `--expect`s against it and push it onto the run's metrics.
#[allow(clippy::too_many_arguments)]
fn record_metrics(
    camera: ShotCamera,
    harness: &ShotHarness,
    progress: &mut ShotProgress,
    verdict: &SettleVerdict,
    settled: bool,
    globe: &GlobeState,
    stream: &MapStream,
    cam_state: &CameraState,
    readouts: &ShotReadouts,
    visuals: &VisualCounts,
) {
    let ShotCamera {
        cell,
        tf,
        projection,
        exposure,
        bloom,
        shaft_fog,
    } = camera;
    let (near, far) = match projection {
        Projection::Perspective(p) => (p.near, p.far),
        _ => (0.0, 0.0),
    };
    let shot = &harness.shots[progress.index];
    // **Where on the ground this capture is actually pointed**, in the
    // anchored world frame the tunnels are kept in. The camera is the floating
    // origin, so its own `GlobalTransform` is near zero and the grid cell is
    // the whole of its position — the same conversion `building_select` makes
    // to turn a click into a place.
    let cam_world = crate::grid::anchored_position(cell, tf.translation);
    let fwd = tf.forward();
    let aim = (fwd.y < -1e-3 && cam_world.y > 0.0).then(|| {
        let t = cam_world.y / -fwd.y;
        Vec2::new(cam_world.x + fwd.x * t, cam_world.z + fwd.z * t)
    });
    // The aerial photograph of what this capture looks at, beside it
    // (`aerial_reference`). Aimed at the ground point in the middle of the
    // view, or — looking at the horizon — at the ground 150 m ahead.
    #[cfg(not(target_arch = "wasm32"))]
    if harness.aerial_reference {
        if let Some(anchor) = readouts.map_anchor.geo {
            let flat = Vec2::new(fwd.x, fwd.z).normalize_or_zero();
            let target = aim.unwrap_or(Vec2::new(cam_world.x, cam_world.z) + flat * 150.0);
            let ll = |p: Vec2| {
                crate::systems::geo_nav::world_to_lat_lng(anchor, f64::from(p.x), f64::from(p.y))
            };
            let pose = match projection {
                Projection::Perspective(p) => Some(crate::systems::aerial_reference::Pose {
                    anchor,
                    position: cam_world.to_array(),
                    right: tf.right().to_array(),
                    up: tf.up().to_array(),
                    forward: fwd.to_array(),
                    fov_y: p.fov,
                    size: (harness.size.x, harness.size.y),
                    ground_y: readouts.terrain.at(target.x, target.y),
                }),
                _ => None,
            };
            crate::systems::aerial_reference::spawn_write(
                harness.path_for(progress.index),
                crate::systems::aerial_reference::View {
                    camera: ll(Vec2::new(cam_world.x, cam_world.z)),
                    aim: ll(target),
                    pose,
                },
            );
        }
    }
    // Where the nearest tunnel actually goes under, as something you can type
    // into `--at`. See `Tunnels::nearest_entrance` for why this is a
    // reported number and not an arithmetic exercise at the call site.
    let entrance = aim
        .and_then(|p| readouts.tunnels.nearest_entrance(p))
        .and_then(|(at, heading)| {
            readouts.map_anchor.geo.map(|anchor| {
                let (lat, lng) = crate::systems::geo_nav::world_to_lat_lng(
                    anchor,
                    f64::from(at.x),
                    f64::from(at.y),
                );
                (lat, lng, heading)
            })
        });
    if let Some((lat, lng, heading)) = entrance {
        tracing::info!(
            lat = format!("{lat:.6}"),
            lng = format!("{lng:.6}"),
            heading_deg = format!("{heading:.0}"),
            "tunnel entrance"
        );
    }
    // Where the camera and its aim actually are on Earth, out of the frame the
    // world is drawn in — not the `--at` that was asked for.
    let geo_of = |p: Vec2| {
        readouts
            .map_anchor
            .geo
            .map_or((f64::NAN, f64::NAN), |anchor| {
                crate::systems::geo_nav::world_to_lat_lng(anchor, f64::from(p.x), f64::from(p.y))
            })
    };
    let camera_geo = geo_of(Vec2::new(cam_world.x, cam_world.z));
    let aim_geo = aim.map_or((f64::NAN, f64::NAN), geo_of);
    let nearest_tunnel = aim.and_then(|p| readouts.tunnels.nearest_tunnel(p));
    // Walked once here rather than twice in the struct literal below: the
    // descendant walk is the only metric that costs a traversal.
    let avatar_counts = readouts.avatar.counts();
    // Likewise: one walk of every mesh entity, resolving each handle into
    // `Assets<Mesh>`. Expensive per frame, free once per capture — and this is
    // the frame the pool is already quiesced for.
    let render = readouts.census.counts();
    // Likewise, and it was called three times in the struct literal below —
    // once per column — each walking every pool.
    let grass = readouts.grass.census();
    // One walk of the emitters, for two columns.
    let particles = crate::systems::particles::particle_census(&visuals.particles);
    // Absent on a backend that keeps no allocator report; reported as zeros
    // rather than omitted, so a CSV row never changes width.
    let vram = readouts
        .render_device
        .wgpu_device()
        .generate_allocator_report()
        .map_or((0, 0), |r| {
            (
                r.total_allocated_bytes / 1024,
                r.total_reserved_bytes / 1024,
            )
        });
    let deck_steps = readouts.decks.approach_steps(&readouts.terrain);
    let m = ShotMetrics {
        shot: progress.index + 1,
        name: shot.name.clone(),
        lat: shot.lat,
        lng: shot.lng,
        camera_lat: camera_geo.0,
        camera_lng: camera_geo.1,
        aim_lat: aim_geo.0,
        aim_lng: aim_geo.1,
        settled,
        settled_s: verdict.at,
        busy: verdict.busy.iter().map(|s| (*s).to_string()).collect(),
        frame_ms: 1000.0 * progress.frame_time / progress.frames.max(1) as f32,
        frame_ms_max: 1000.0 * progress.frame_time_max,
        // Patched by the capture observer once the pair's second readback
        // lands — the row is pushed at the shutter, before any pixels exist.
        flicker_px: 0,
        draws: render.draws,
        tris_total: render.tris_total,
        tris_shadow: render.tris_shadow,
        shadow_casters: render.shadow_casters,
        tris_buildings: render.tris_buildings,
        tris_lod22: render.tris_lod22,
        tris_tiles: render.tris_tiles,
        tris_globe: render.tris_globe,
        worker_outstanding: progress.worker_outstanding,
        fetch_outstanding: crate::platform::fetch_gate::outstanding(),
        radius_bias: crate::systems::load_control::bias(),
        tiles_occluded: stream.occluded.len(),
        aerial_tiles: stream.aerial_tiles(),
        alt_m: globe.altitude_m(cell, tf) as i64,
        space_factor: globe.space_factor(cell, tf),
        in_space: globe.in_space(cell, tf),
        orbit_space: cam_state.orbit_space,
        near_m: near as i64,
        far_m: far as i64,
        exposure_ev100: exposure.ev100,
        // No glare means no `Bloom` component at all, and 0.0 is the honest
        // reading for it.
        bloom: bloom.map_or(0.0, |b| b.intensity),
        globe_albedo: readouts.globe_albedo.0,
        veg_factor: readouts.vegetation.factor,
        furniture_factor: readouts.furniture.factor,
        route_meshes: visuals.route.iter().count(),
        route_rendered: visuals.route.iter().filter(|vv| vv.get()).count(),
        shaft_fog,
        shaft_volumes: visuals.shafts.iter().count(),
        // Distance from the camera to the volume's centre, in render space.
        // The camera is the floating origin, so its own render position is
        // ~0 and this is just the volume's magnitude — which is the point:
        // it is the one number that tells a cell-local translation from a
        // render-space one without knowing either frame's convention.
        shaft_volume_m: visuals
            .shafts
            .iter()
            .map(|t| t.translation().length())
            .fold(f32::NAN, f32::min),
        shaft_pref: readouts.prefs.light_shafts_on(),
        shadows_pref: readouts.prefs.shadows_on(),
        particle_emitters: particles.0 as usize,
        particle_quads: particles.1 as usize,
        grass_patches: grass.0,
        grass_slots: grass.1,
        grass_rendered: grass.2,
        bag_buildings: readouts.bag.len(),
        bag_addresses: readouts.bag.addresses().count(),
        bag_berths: readouts.bag.berths().count(),
        surveyed_trees: readouts.places.surveyed().0,
        municipal_trees: readouts.places.surveyed().7,
        municipal_props: readouts.places.surveyed().8,
        surveyed_props: readouts.places.surveyed().1,
        surveyed_lines: readouts.places.surveyed().2,
        surveyed_paving: readouts.places.surveyed().3,
        crop_parcels: readouts.places.surveyed().4,
        nav_marks: readouts.places.surveyed().5,
        level_crossings: readouts.places.surveyed().6,
        prop_colliders: readouts.places.props().count(),
        flood_defences: readouts.dikes.len(),
        flood_defences_measured: readouts.dikes.measured(),
        sewer_pipes: readouts.sewers.pipe_count(),
        sewer_meshes: readouts.sewers.meshes(),
        underground_shapes: readouts.underground.shapes(),
        underground_meshes: readouts.underground.meshes(),
        tunnel_count: readouts.tunnels.tunnel_count(),
        tunnel_road_m: readouts.tunnels.deepest_road(),
        tunnel_meshes: visuals.tunnels.iter().count(),
        tunnel_rendered: visuals.tunnels.iter().filter(|vv| vv.get()).count(),
        tunnel_entrance_lat: entrance.map_or(0.0, |(lat, _, _)| lat),
        tunnel_entrance_lng: entrance.map_or(0.0, |(_, lng, _)| lng),
        tunnel_entrance_heading: entrance.map_or(-1.0, |(_, _, h)| h),
        tunnel_ahead_m: nearest_tunnel.map_or(-1.0, |(d, _)| d),
        tunnel_ahead_road_m: nearest_tunnel.map_or(0.0, |(_, y)| y),
        basemap_open_tiles: readouts.basemap_holes.0,
        interior_rooms: readouts.interiors.rooms(),
        interior_storeys: readouts.interior_floors.storeys(),
        interior_designed: readouts.interiors.designed(),
        cutaway_storey: if readouts.cutaway.active() {
            readouts.cutaway.storey as i32
        } else {
            -1
        },
        building_selected: readouts.building_selection.is_selected(),
        building_indoors: readouts.building_selection.is_indoors(),
        building_shell_meshes: visuals.shells.iter().count(),
        building_shell_rendered: visuals.shells.iter().filter(|vv| vv.get()).count(),
        building_doors: readouts.lod22.doors(),
        building_scaffolds: readouts.lod22.scaffolds(),
        building_fences: readouts.lod22.fences(),
        lod22_dated: readouts.lod22.dated().0,
        lod22_elements: readouts.lod22.dated().1,
        prop_meshes: readouts.street_detail.spawned_meshes + readouts.tall_structures.spawned(),
        traffic_cars: visuals
            .agents
            .iter()
            .filter(|(k, v)| {
                **k == crate::systems::traffic::AgentKind::Car && **v != Visibility::Hidden
            })
            .count(),
        traffic_walkers: visuals
            .agents
            .iter()
            .filter(|(k, v)| {
                **k == crate::systems::traffic::AgentKind::Walker && **v != Visibility::Hidden
            })
            .count(),
        avatar_meshes: avatar_counts.0,
        avatar_rendered: avatar_counts.1,
        avatar_floor_m: readouts.avatar_state.floor_lift,
        globe_entities: visuals.globe.iter().count(),
        globe_visible: visuals
            .globe
            .iter()
            .filter(|(_, v)| **v == Visibility::Visible)
            .count(),
        globe_rendered: visuals.globe.iter().filter(|(vv, _)| vv.get()).count(),
        globe_tiles_loaded: globe.tiles_loaded(),
        globe_tiles_desired: globe.tiles_desired(),
        globe_texture_mb: globe.tile_texture_bytes() as f32 / (1024.0 * 1024.0),
        globe_tile_px: crate::systems::globe::GLOBE_TILE_PX,
        ground_m: readouts.height.ground_m.unwrap_or(f32::NAN),
        height_patches: readouts.height.fetched,
        terrain_ready: readouts.terrain.is_ready(),
        terrain_relief_m: readouts.terrain.range_m.1 - readouts.terrain.range_m.0,
        // The live spacing, not the constant: `quality::Budget::terrain_grid`
        // lowers it on a handset, and a metric that reported the nominal number
        // there would say the phone measured ground it never asked for.
        terrain_sample_m: readouts.terrain.sample_m(),
        terrain_graded_samples: readouts.road_mask.marked_samples(),
        approach_abutments: readouts.terrain.approach_samples.0,
        approach_raised: readouts.terrain.approach_samples.1,
        deck_count: deck_steps.0,
        decks_unapproached: deck_steps.1,
        deck_step_max_m: deck_steps.2,
        deck_step_mean_m: deck_steps.3,
        deck_profiled: readouts.decks.profiled(),
        deck_piers: readouts.decks.piers(),
        ground_detail_ready: readouts.ground_detail.slope.is_some(),
        ground_detail_sample_m: readouts.ground_detail.sample_m,
        fine_terrain_ready: readouts.terrain.fine_ready(),
        fine_terrain_sample_m: if readouts.terrain.fine_ready() {
            crate::systems::terrain::FINE_SAMPLE_M
        } else {
            0.0
        },
        water_rings: readouts.water_levels.len(),
        water_carved_coarse: readouts.terrain.carved_coarse,
        water_carved_fine: readouts.terrain.carved_fine,
        water_drop_m: readouts.terrain.water_drop_m(),
        upload_peak_kb: readouts.uploads.peak_kb(),
        upload_peak_asset_kb: readouts.uploads.peak_asset_kb(),
        gpu_resident_kb: readouts.uploads.resident_kb(),
        gpu_resident_peak_kb: readouts.uploads.peak_resident_kb(),
        warm_tile_hits: stream.warm_stats().0,
        warm_tile_mb: stream.warm_stats().1 as f32 / (1024.0 * 1024.0),
        warm_cell_restores: readouts.lod22.warm_stats().0,
        warm_cell_mb: readouts.lod22.warm_stats().1 as f32 / (1024.0 * 1024.0),
        fetch_count: progress.fetch_window.requests,
        fetch_errors: progress.fetch_window.errors,
        fetch_mb: progress.fetch_window.bytes as f32 / (1024.0 * 1024.0),
        fetch_ttfb_ms: progress.fetch_window.ttfb_us / 1000,
        fetch_body_ms: progress.fetch_window.body_us / 1000,
        worker_jobs: progress.worker_window.iter().map(|(_, t)| t.jobs).sum(),
        worker_busy_ms: progress
            .worker_window
            .iter()
            .map(|(_, t)| t.busy_us)
            .sum::<u64>()
            / 1000,
        worker_wait_ms: progress
            .worker_window
            .iter()
            .map(|(_, t)| t.wait_us)
            .sum::<u64>()
            / 1000,
        worker_threads: crate::platform::workers::worker_count(),
        // `busy / (threads × window)`. The window is the shot's own elapsed
        // time, which is what the worker account above was deltaed over, so
        // the two describe the same seconds. Guarded rather than clamped: a
        // zero-length window or an unsized pool would otherwise divide by
        // zero and report `inf` as a performance result.
        worker_utilisation: {
            let threads = crate::platform::workers::worker_count().max(1) as f64;
            let window_us = f64::from(verdict.at) * 1_000_000.0;
            let busy_us = progress
                .worker_window
                .iter()
                .map(|(_, t)| t.busy_us)
                .sum::<u64>() as f64;
            if window_us > 0.0 {
                (busy_us / (threads * window_us)) as f32
            } else {
                0.0
            }
        },
        worker_split: progress
            .worker_window
            .iter()
            .map(|(label, t)| {
                format!(
                    "{label}:{}:{}:{}",
                    t.jobs,
                    t.busy_us / 1000,
                    t.wait_us / 1000
                )
            })
            .collect(),
        software_renderer: readouts.adapter.device_type == egui_wgpu::wgpu::DeviceType::Cpu,
        vram_allocated_kb: vram.0,
        vram_reserved_kb: vram.1,
    };
    // One line, but its *level* carries whether the shot is trustworthy: a
    // capture that timed out describes a half-built world, and at INFO it
    // looked exactly like a slow success. The macro is here so the field list
    // isn't duplicated once per level.
    macro_rules! shot_state {
        ($level:ident) => {
            tracing::$level!(
                shot = m.shot,
                name = m.name.as_deref().unwrap_or("-"),
                settled = m.settled,
                busy = m.busy.join("|"),
                settled_in = format!("{:.1}s", m.settled_s),
                frame_ms = format!("{:.1}", m.frame_ms),
                frame_ms_max = format!("{:.1}", m.frame_ms_max),
                draws = m.draws,
                ktris_total = m.tris_total / 1000,
                ktris_shadow = m.tris_shadow / 1000,
                shadow_casters = m.shadow_casters,
                worker_outstanding = m.worker_outstanding,
                fetch_outstanding = m.fetch_outstanding,
                radius_bias = m.radius_bias,
                tiles_occluded = m.tiles_occluded,
                aerial_tiles = m.aerial_tiles,
                alt_m = m.alt_m,
                space_factor = format!("{:.3}", m.space_factor),
                in_space = m.in_space,
                orbit_space = m.orbit_space,
                near_m = m.near_m,
                far_m = m.far_m,
                exposure_ev100 = format!("{:.2}", m.exposure_ev100),
                bloom = format!("{:.3}", m.bloom),
                globe_albedo = format!("{:.3}", m.globe_albedo),
                veg_factor = format!("{:.3}", m.veg_factor),
                furniture_factor = format!("{:.3}", m.furniture_factor),
                route_meshes = m.route_meshes,
                route_rendered = m.route_rendered,
                particle_emitters = m.particle_emitters,
                particle_quads = m.particle_quads,
                grass_patches = m.grass_patches,
                grass_slots = m.grass_slots,
                grass_rendered = m.grass_rendered,
                nav_marks = m.nav_marks,
                level_crossings = m.level_crossings,
                prop_colliders = m.prop_colliders,
                prop_meshes = m.prop_meshes,
                traffic_cars = m.traffic_cars,
                traffic_walkers = m.traffic_walkers,
                avatar_meshes = m.avatar_meshes,
                avatar_rendered = m.avatar_rendered,
                globe_entities = m.globe_entities,
                globe_visible = m.globe_visible,
                globe_rendered = m.globe_rendered,
                ground_m = format!("{:.2}", m.ground_m),
                height_patches = m.height_patches,
                terrain_ready = m.terrain_ready,
                terrain_relief_m = format!("{:.2}", m.terrain_relief_m),
                terrain_sample_m = format!("{:.2}", m.terrain_sample_m),
                fine_terrain_ready = m.fine_terrain_ready,
                fine_terrain_sample_m = format!("{:.2}", m.fine_terrain_sample_m),
                ground_detail_ready = m.ground_detail_ready,
                ground_detail_sample_m = format!("{:.2}", m.ground_detail_sample_m),
                water_rings = m.water_rings,
                water_carved_coarse = m.water_carved_coarse,
                water_carved_fine = m.water_carved_fine,
                water_drop_m = format!("{:.2}", m.water_drop_m),
                upload_peak_kb = m.upload_peak_kb,
                upload_peak_asset_kb = m.upload_peak_asset_kb,
                gpu_resident_kb = m.gpu_resident_kb,
                gpu_resident_peak_kb = m.gpu_resident_peak_kb,
                warm_tile_hits = m.warm_tile_hits,
                warm_tile_mb = m.warm_tile_mb,
                warm_cell_restores = m.warm_cell_restores,
                warm_cell_mb = m.warm_cell_mb,
                fetch_count = m.fetch_count,
                fetch_errors = m.fetch_errors,
                fetch_mb = format!("{:.2}", m.fetch_mb),
                fetch_ttfb_ms = m.fetch_ttfb_ms,
                fetch_body_ms = m.fetch_body_ms,
                worker_jobs = m.worker_jobs,
                worker_busy_ms = m.worker_busy_ms,
                worker_wait_ms = m.worker_wait_ms,
                worker_threads = m.worker_threads,
                worker_util = format!("{:.2}", m.worker_utilisation),
                worker_split = m.worker_split.join("|"),
                software_renderer = m.software_renderer,
                "shot state"
            )
        };
    }
    if m.settled {
        shot_state!(info);
    } else {
        shot_state!(warn);
    }
    for e in &harness.expect {
        // `flicker_px` does not exist until the pair's second readback lands,
        // which is after this row is pushed — the capture observer patches
        // the field and runs these there instead. Checking here would gate
        // on the placeholder 0 and pass everything.
        if e.key == "flicker_px" {
            continue;
        }
        if let Err(why) = e.check(&m) {
            progress.unmet += 1;
            tracing::error!(shot = m.shot, expect = %e, why = %why, "shot expect failed");
        }
    }
    progress.metrics.push(m);
}

/// Spawn the capture request for the current shot and start its readback budget.
///
/// Split out of [`drive_capture`] because it has two callers: the shutter, and the
/// one retry the grace timeout allows.
fn fire_capture(
    commands: &mut Commands,
    harness: &ShotHarness,
    progress: &mut ShotProgress,
    target: Option<&ShotTarget>,
    shot_state: &mut ScreenshotState,
) {
    // `--hide-ui`: drop this frame's egui geometry (ui::suppress_egui_for_screenshot
    // reads `hide_ui` in PostUpdate; ui::drive_screenshots clears it next frame).
    shot_state.hide_ui = harness.hide_ui_for(progress.index);
    progress.fired = true;
    progress.fired_at = progress.elapsed;
    let saved = progress.saved.clone();
    let flicker_prev = progress.flicker_prev.clone();
    let index = progress.index;
    let path = harness.path_for(progress.index);
    let flicker = harness.flicker;
    let screenshot = match target {
        Some(t) => Screenshot::image(t.0.clone()),
        None => Screenshot::primary_window(),
    };
    commands.spawn(screenshot).observe(
        // An observer is a system, so it may take resources beside the event —
        // which is what lets the flicker diff patch this shot's metrics row:
        // the row is pushed at the shutter, and only this closure ever sees the
        // pixels it describes.
        move |captured: On<ScreenshotCaptured>,
              harness: Res<ShotHarness>,
              mut progress: ResMut<ShotProgress>| {
            let (saved, path) = (saved.clone(), path.clone());
            // A preset writes to a named directory (`docs/img/`); create it so a
            // fresh checkout doesn't lose the run to a missing folder.
            if let Some(dir) = path.parent().filter(|d| !d.as_os_str().is_empty()) {
                let _ = std::fs::create_dir_all(dir);
            }
            match captured.image.clone().try_into_dynamic() {
                // Alpha carries HDR brightness rather than transparency here
                // (as in the README capture path), so drop it.
                Ok(img) => {
                    let rgb = img.to_rgb8();
                    match rgb.save(&path) {
                        Ok(()) => {
                            tracing::info!(path = %path.display(), "shot harness: saved");
                            if flicker {
                                let (w, h) = (rgb.width(), rgb.height());
                                let mut prev =
                                    flicker_prev.lock().unwrap_or_else(|p| p.into_inner());
                                // Odd index = the nudged twin (`from_cli` builds
                                // the pairs); diff it against its reference.
                                if index % 2 == 1 {
                                    if let Some((pw, ph, pdata)) = prev.as_ref() {
                                        if *pw == w && *ph == h {
                                            let flips = solid_flips(
                                                w as usize,
                                                h as usize,
                                                pdata,
                                                rgb.as_raw(),
                                            );
                                            if let Some(m) = progress.metrics.last_mut() {
                                                m.flicker_px = flips;
                                            }
                                            tracing::info!(
                                                shot = index + 1,
                                                flicker_px = flips,
                                                "shot harness: flicker pair measured"
                                            );
                                        }
                                    }
                                }
                                *prev = Some((w, h, rgb.into_raw()));
                            }
                            // The flicker expectations run here, not at the
                            // shutter (see `drive_capture`): the field is
                            // patched a few lines up, and only this closure
                            // ever sees the pixels. A flicker expectation on a
                            // run without `--flicker` was refused at startup.
                            let mut failed = Vec::new();
                            if let Some(m) = progress.metrics.last() {
                                for e in harness.expect.iter().filter(|e| e.key == "flicker_px") {
                                    if let Err(why) = e.check(m) {
                                        failed.push((m.shot, e.to_string(), why));
                                    }
                                }
                            }
                            for (shot, expect, why) in failed {
                                progress.unmet += 1;
                                tracing::error!(shot, expect, why, "shot expect failed");
                            }
                            saved.store(true, Ordering::Relaxed);
                        }
                        Err(e) => {
                            tracing::error!(path = %path.display(), error = %e, "shot save failed")
                        }
                    }
                }
                Err(e) => tracing::error!(error = %e, "shot decode failed"),
            }
        },
    );
}

/// Threshold on the per-pixel max-channel difference that counts as a flip.
/// Well above lighting noise and dither, well below the palette distance between
/// any two surface groups.
const FLIP_THRESHOLD: u8 = 40;

/// Count the pixels that changed *as a region* between two same-sized RGB8
/// frames: a per-pixel threshold on the max channel difference, then a 3×3
/// erosion so only pixels whose whole neighbourhood flipped survive.
///
/// The erosion is what separates the signal from the nudge's own artefacts: a
/// ~5 cm camera move crawls the aliasing on every silhouette by a fraction of a
/// pixel, which flips isolated edge pixels in one-pixel-wide lines — while a
/// depth-order flip repaints a *patch* of one surface in the other's colour.
/// One-pixel lines cannot contain a 3×3-solid pixel; patches are made of them.
fn solid_flips(w: usize, h: usize, a: &[u8], b: &[u8]) -> u32 {
    if w < 3 || h < 3 || a.len() != w * h * 3 || a.len() != b.len() {
        return 0;
    }
    let flip: Vec<bool> = (0..w * h)
        .map(|i| {
            let (pa, pb) = (&a[i * 3..i * 3 + 3], &b[i * 3..i * 3 + 3]);
            pa.iter()
                .zip(pb)
                .map(|(x, y)| x.abs_diff(*y))
                .max()
                .unwrap_or(0)
                > FLIP_THRESHOLD
        })
        .collect();
    let mut n = 0;
    for y in 1..h - 1 {
        for x in 1..w - 1 {
            let solid = (y - 1..=y + 1).all(|yy| (x - 1..=x + 1).all(|xx| flip[yy * w + xx]));
            if solid {
                n += 1;
            }
        }
    }
    n
}

#[cfg(test)]
mod tests {
    use super::*;

    fn spec(lat: f64, lng: f64, alt: f64) -> ShotSpec {
        ShotSpec {
            lat,
            lng,
            alt,
            pitch_deg: -90.0,
            yaw_deg: 0.0,
            ..default()
        }
    }

    /// A one-pixel line is the aliasing crawl a 5 cm nudge legitimately causes
    /// on every silhouette; a patch is a surface repainted in another's colour.
    /// The erosion is the whole difference between an instrument and a noise
    /// meter, so it gets its own pin.
    #[test]
    fn solid_flips_counts_patches_not_edge_crawl() {
        let (w, h) = (16usize, 16usize);
        let base = vec![0u8; w * h * 3];
        // A 5×5 patch of a different colour: 3×3-solid interior = 3×3 = 9.
        let mut patch = base.clone();
        for y in 4..9 {
            for x in 4..9 {
                patch[(y * w + x) * 3] = 255;
            }
        }
        assert_eq!(solid_flips(w, h, &base, &patch), 9);
        // A one-pixel-wide line of any length: no 3×3-solid pixel at all.
        let mut line = base.clone();
        for y in 0..h {
            line[(y * w + 8) * 3] = 255;
        }
        assert_eq!(solid_flips(w, h, &base, &line), 0);
        // Sub-threshold change is not a flip.
        let dim = vec![FLIP_THRESHOLD; w * h * 3];
        assert_eq!(solid_flips(w, h, &base, &dim), 0);
        // Degenerate sizes and mismatched buffers answer 0, never panic.
        assert_eq!(solid_flips(2, 2, &[0; 12], &[255; 12]), 0);
        assert_eq!(solid_flips(w, h, &base, &base[..3]), 0);
    }

    #[test]
    fn flicker_doubles_the_shots_into_nudged_pairs() {
        let h = ShotHarness::from_cli(ShotArgs {
            path: PathBuf::from("/tmp/f.png"),
            at: &["53.0,6.0,100".into(), "53.1,6.1,200".into()],
            flicker: true,
            ..default()
        })
        .expect("valid");
        assert_eq!(h.shots.len(), 4);
        assert_eq!(h.shots[0].lat, 53.0);
        assert!((h.shots[1].lat - (53.0 + FLICKER_NUDGE_DEG)).abs() < 1e-12);
        assert_eq!(h.shots[1].lng, h.shots[0].lng);
        assert_eq!(h.shots[2].lat, 53.1);
    }

    /// Without `--flicker` the metric is a constant 0 and any `flicker_px`
    /// expectation holds vacuously — a green gate that measured nothing. That
    /// has to be a startup error, same as an unknown key.
    #[test]
    fn a_flicker_expectation_without_flicker_is_refused() {
        let err = ShotHarness::from_cli(ShotArgs {
            path: PathBuf::from("/tmp/f.png"),
            at: &["53.0,6.0,100".into()],
            expect: &["flicker_px<=200".into()],
            ..default()
        })
        .err()
        .expect("must refuse");
        assert!(err.contains("--flicker"), "{err}");
    }

    #[test]
    fn densify_passthrough_when_off_or_single() {
        let wp = vec![spec(0.0, 0.0, 2000.0), spec(1.0, 1.0, 4000.0)];
        assert_eq!(
            densify_path(&wp, 0).len(),
            2,
            "frames=0 leaves waypoints as-is"
        );
        let one = vec![spec(0.0, 0.0, 2000.0)];
        assert_eq!(
            densify_path(&one, 8).len(),
            1,
            "a single waypoint can't be swept"
        );
    }

    #[test]
    fn densify_hits_both_endpoints() {
        let wp = vec![spec(10.0, 20.0, 2000.0), spec(11.0, 22.0, 4_000_000.0)];
        let out = densify_path(&wp, 16);
        assert_eq!(out.len(), 16);
        // First and last samples are the waypoints themselves.
        assert!((out[0].lat - 10.0).abs() < 1e-9 && (out[0].alt - 2000.0).abs() < 1e-6);
        assert!((out[15].lat - 11.0).abs() < 1e-9 && (out[15].alt - 4_000_000.0).abs() < 1.0);
    }

    #[test]
    fn altitude_is_geometric_not_linear() {
        // Midpoint of a 2 km → 200 km sweep: geometric mean (20 km), not the
        // linear midpoint (101 km). This is what keeps samples off the ceiling.
        let wp = vec![spec(0.0, 0.0, 2_000.0), spec(0.0, 0.0, 200_000.0)];
        let out = densify_path(&wp, 3);
        assert!((out[1].alt - 20_000.0).abs() < 1.0, "got {}", out[1].alt);
    }

    #[test]
    fn densify_spans_multiple_segments() {
        // Three waypoints, even frame count: the middle waypoint is passed through
        // exactly at the sweep's centre.
        let wp = vec![
            spec(0.0, 0.0, 1000.0),
            spec(5.0, 0.0, 1000.0),
            spec(10.0, 0.0, 1000.0),
        ];
        let out = densify_path(&wp, 5);
        assert_eq!(out.len(), 5);
        assert!(
            (out[2].lat - 5.0).abs() < 1e-9,
            "centre sample is the middle waypoint"
        );
    }

    #[test]
    fn frames_requires_two_waypoints() {
        let result = ShotHarness::from_cli(ShotArgs {
            path: "/tmp/s.png".into(),
            at: &["0,0,1000".into()],
            frames: 8,
            ..default()
        });
        match result {
            Err(e) => assert!(e.contains("--frames"), "got {e:?}"),
            Ok(_) => panic!("expected --frames with one waypoint to error"),
        }
    }

    /// A live harness starts with nothing to do, and each viewpoint it is handed
    /// brings its own patience — which is the difference between "re-photograph
    /// what you are already looking at" and "fly to a city you have never been
    /// to".
    #[test]
    fn a_live_harness_starts_empty_and_takes_patience_per_shot() {
        let mut h = ShotHarness::live_harness(UVec2::new(1280, 720), true);
        assert!(h.live);
        assert!(h.windowed);
        assert!(
            h.shots.is_empty(),
            "nothing to photograph until it is asked"
        );
        // Falls back to the run-wide values while nothing says otherwise.
        assert_eq!(h.settle_for(0), DEFAULT_SETTLE);
        assert_eq!(h.wait_for(0), DEFAULT_WAIT);

        h.shots.push(ShotSpec {
            settle: Some(0.0),
            in_place: true,
            out: Some(PathBuf::from("/tmp/here.png")),
            ..default()
        });
        assert_eq!(h.settle_for(0), 0.0, "the shot's own settle wins");
        assert_eq!(h.wait_for(0), DEFAULT_WAIT, "and the rest still falls back");
        assert_eq!(
            h.path_for(0),
            PathBuf::from("/tmp/here.png"),
            "a live shot names its own file, as a preset does"
        );
    }

    /// `harness_active` means "a script owns this run", and a windowed live
    /// harness does not: the user store, the frame limiter and the load
    /// controller all belong to the person whose session it is.
    #[test]
    fn a_windowed_live_harness_does_not_own_the_run() {
        let size = UVec2::new(640, 360);
        assert!(
            !ShotHarness::live_harness(size, true).owns_run(),
            "a client somebody is watching keeps its user store, its frame limiter \
             and its load controller"
        );
        assert!(
            ShotHarness::live_harness(size, false).owns_run(),
            "a windowless one is nobody's session"
        );
        assert!(
            ShotHarness::from_cli(ShotArgs {
                path: "/tmp/s.png".into(),
                at: &["0,0,1000".into()],
                ..default()
            })
            .unwrap()
            .owns_run(),
            "and a scripted run always has"
        );
    }

    #[test]
    fn path_for_indexes_and_pads_a_sweep() {
        let h = ShotHarness::from_cli(ShotArgs {
            path: "/tmp/sweep.png".into(),
            at: &["0,0,2000".into(), "0,0,4000000".into()],
            frames: 12,
            ..default()
        })
        .unwrap();
        // 12 frames → indices 00..11, zero-padded to width 2, altitude-tagged.
        assert_eq!(h.path_for(0), PathBuf::from("/tmp/sweep_00_2000m.png"));
        assert!(h
            .path_for(11)
            .to_string_lossy()
            .starts_with("/tmp/sweep_11_"));
    }

    #[test]
    fn manifest_run_names_its_own_files_and_carries_per_shot_state() {
        // The checked-in manifest is the docs' source of truth — if it stops
        // loading, `cargo shots` silently stops regenerating the README images.
        let path = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../docs/shots.toml");
        let h = ShotHarness::from_manifest(&path, &[], None, Some(0.1), Some(1.0), None, None, &[])
            .expect("checked-in docs/shots.toml must load");
        assert!(
            h.shots.len() >= 2,
            "manifest should describe a set of views"
        );
        for (i, s) in h.shots.iter().enumerate() {
            let name = s.name.clone().expect("preset shots are named");
            let out = h.path_for(i);
            assert!(
                out.to_string_lossy().contains(&name),
                "{name} should own its filename, got {}",
                out.display()
            );
            // The docs images live beside the manifest, not in the CWD.
            assert!(out.starts_with(path.parent().unwrap()), "{}", out.display());
        }
        // A bare run is the published set, so nothing it writes may sit in a
        // group's directory. The subject shots are reached by `--only <group>`.
        let m = crate::systems::shot_presets::Manifest::load(&path).unwrap();
        for g in m.groups() {
            for i in 0..h.shots.len() {
                let out = h.path_for(i);
                assert!(
                    !out.to_string_lossy().contains(&format!("/{g}/")),
                    "a bare `cargo shots` wrote into the {g} group: {}",
                    out.display()
                );
            }
        }
        assert!(
            !m.groups().is_empty(),
            "the manifest should carry at least one subject group, or the \
             grouping machinery has no content and nothing exercises it"
        );
        // `--only` narrows the same set without changing anything else.
        let first = h.shots[0].name.clone().unwrap();
        let one =
            ShotHarness::from_manifest(&path, &[first.clone()], None, None, None, None, None, &[])
                .unwrap();
        assert_eq!(one.shots.len(), 1);
        assert_eq!(one.shots[0].name.as_deref(), Some(first.as_str()));
    }

    #[test]
    fn manifest_size_can_be_overridden_from_the_cli() {
        let path = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../docs/shots.toml");
        let h =
            ShotHarness::from_manifest(&path, &[], Some("640x360"), None, None, None, None, &[])
                .unwrap();
        assert_eq!(h.size, UVec2::new(640, 360));
    }

    /// A capture that measured nothing: every count zero, nothing settled, and
    /// `ground_m` the `NaN` a run without the height layer really produces.
    fn sample_metrics() -> ShotMetrics {
        ShotMetrics {
            shot: 1,
            name: Some("city".into()),
            lat: 0.0,
            lng: 0.0,
            camera_lat: 0.0,
            camera_lng: 0.0,
            aim_lat: 0.0,
            aim_lng: 0.0,
            flicker_px: 0,
            settled: false,
            // Two entries on purpose: `busy` is the one list-valued column, and
            // joining it with a comma would add columns to this row and silently
            // shift every header after it.
            busy: vec!["map_tiles".into(), "lod22".into()],
            settled_s: 1.0,
            frame_ms: 16.6,
            frame_ms_max: 42.0,
            draws: 0,
            tris_total: 0,
            tris_shadow: 0,
            shadow_casters: 0,
            tris_buildings: 0,
            tris_lod22: 0,
            tris_tiles: 0,
            tris_globe: 0,
            worker_outstanding: 0,
            fetch_outstanding: 0,
            radius_bias: 0,
            tiles_occluded: 0,
            aerial_tiles: 0,
            alt_m: 1000,
            space_factor: 0.0,
            in_space: false,
            orbit_space: false,
            near_m: 1,
            far_m: 1000,
            exposure_ev100: 14.0,
            bloom: 0.15,
            globe_albedo: 1.0,
            veg_factor: 0.0,
            grass_patches: 0,
            bag_buildings: 0,
            bag_addresses: 0,
            bag_berths: 0,
            surveyed_trees: 0,
            municipal_trees: 0,
            municipal_props: 0,
            surveyed_props: 0,
            surveyed_lines: 0,
            surveyed_paving: 0,
            crop_parcels: 0,
            nav_marks: 0,
            level_crossings: 0,
            prop_colliders: 0,
            flood_defences: 0,
            flood_defences_measured: 0,
            sewer_pipes: 0,
            sewer_meshes: 0,
            underground_shapes: 0,
            underground_meshes: 0,
            tunnel_count: 0,
            tunnel_road_m: 0.0,
            tunnel_meshes: 0,
            tunnel_rendered: 0,
            tunnel_entrance_lat: 0.0,
            tunnel_entrance_lng: 0.0,
            tunnel_entrance_heading: -1.0,
            tunnel_ahead_m: -1.0,
            tunnel_ahead_road_m: 0.0,
            basemap_open_tiles: 0,
            interior_rooms: 0,
            interior_storeys: 0,
            interior_designed: 0,
            cutaway_storey: -1,
            building_selected: false,
            building_indoors: false,
            building_shell_meshes: 0,
            building_shell_rendered: 0,
            building_doors: 0,
            building_scaffolds: 0,
            building_fences: 0,
            lod22_dated: 0,
            lod22_elements: 0,
            grass_slots: 0,
            grass_rendered: 0,
            prop_meshes: 0,
            traffic_cars: 0,
            traffic_walkers: 0,
            furniture_factor: 0.0,
            globe_entities: 0,
            globe_visible: 0,
            globe_rendered: 0,
            globe_tiles_loaded: 0,
            globe_tiles_desired: 0,
            globe_texture_mb: 0.0,
            globe_tile_px: crate::systems::globe::GLOBE_TILE_PX,
            route_meshes: 0,
            route_rendered: 0,
            shaft_fog: false,
            shaft_volumes: 0,
            // A real number, not the `NaN` the no-volume case reports: this fixture
            // exists to prove the column is plumbed, and `ground_m` is already the
            // one field carrying the unmeasured-reads-as-null case.
            shaft_volume_m: 0.0,
            shaft_pref: false,
            shadows_pref: false,
            particle_emitters: 0,
            particle_quads: 0,
            avatar_meshes: 0,
            avatar_floor_m: 0.0,
            avatar_rendered: 0,
            ground_m: f32::NAN,
            height_patches: 0,
            fine_terrain_ready: false,
            fine_terrain_sample_m: 0.0,
            ground_detail_ready: false,
            ground_detail_sample_m: 0.0,
            water_rings: 0,
            water_carved_coarse: 0,
            water_carved_fine: 0,
            water_drop_m: 0.0,
            terrain_ready: false,
            terrain_relief_m: 0.0,
            terrain_sample_m: 0.0,
            terrain_graded_samples: 0,
            approach_abutments: 0,
            approach_raised: 0,
            deck_count: 0,
            decks_unapproached: 0,
            deck_step_max_m: 0.0,
            deck_step_mean_m: 0.0,
            deck_profiled: 0,
            deck_piers: 0,
            upload_peak_kb: 0,
            upload_peak_asset_kb: 0,
            gpu_resident_kb: 0,
            gpu_resident_peak_kb: 0,
            warm_tile_hits: 0,
            warm_tile_mb: 0.0,
            warm_cell_restores: 0,
            warm_cell_mb: 0.0,
            software_renderer: true,
            vram_allocated_kb: 0,
            vram_reserved_kb: 0,
            fetch_count: 0,
            fetch_errors: 0,
            fetch_mb: 0.0,
            fetch_ttfb_ms: 0,
            fetch_body_ms: 0,
            worker_jobs: 0,
            worker_busy_ms: 0,
            worker_wait_ms: 0,
            // Two entries for the same reason as `busy` above: this is the other
            // list-valued column, and the `|` join is what keeps it one cell.
            worker_split: vec!["map_tiles:2:10:5".into(), "lod22:1:8:3".into()],
            worker_threads: 0,
            worker_utilisation: 0.0,
        }
    }

    /// [`UI_ELEMENTS`] and `open_scripted_ui`'s handlers are two lists that have
    /// to agree, and nothing in the type system relates them — a handler arm is a
    /// string comparison against a literal. They drifted for real: `settings` and
    /// `graphics` were handled for months while the vocabulary called them
    /// unknown, so `--ui graphics` did its job and logged that it hadn't.
    ///
    /// So the test reads this file back and checks the one direction that matters.
    /// Unusual, and cheaper than the alternative: the only other way to relate
    /// them is to run the system, which needs a built `App` with two dozen
    /// resources in it.
    #[test]
    fn ui_vocabulary_covers_every_handler() {
        // Assembled rather than written out, so this needle does not match the
        // line that defines it — the scan reads the whole file, this one included.
        let call = format!("want({}", '"');
        let source = include_str!("shot_harness.rs");
        let mut missing: Vec<&str> = source
            .match_indices(&call)
            .map(|(i, _)| {
                let rest = &source[i + call.len()..];
                &rest[..rest.find('"').expect("unterminated --ui handler name")]
            })
            .filter(|name| !UI_ELEMENTS.contains(name))
            .collect();
        missing.sort_unstable();
        missing.dedup();
        assert!(
            missing.is_empty(),
            "open_scripted_ui handles {missing:?}, which UI_ELEMENTS does not list \
             — `--ui` would warn that they were ignored while acting on them"
        );
        // And the scan found the handlers at all, so a rename of `want` can't turn
        // this into a test that passes by looking at nothing.
        assert!(
            source.matches(&call).count() > 10,
            "found no --ui handlers to check"
        );
    }

    #[test]
    fn csv_header_and_row_have_matching_arity() {
        assert_eq!(
            CSV_HEADER.split(',').count(),
            sample_metrics().csv_row().split(',').count(),
            "CSV header and row column counts drifted"
        );
    }

    /// The arity test above is necessary and was never sufficient: it passed for a
    /// header that named a field the struct doesn't have, because the missing cell
    /// is still a cell. Rows are built by name now, so *that* is the failure left
    /// to guard — a renamed or removed field turns its column into blanks for
    /// every row of every run, and nothing else says so.
    #[test]
    fn csv_columns_all_resolve() {
        let value = serde_json::to_value(sample_metrics()).unwrap();
        for key in CSV_HEADER.split(',') {
            assert!(
                value.get(key).is_some(),
                "CSV column {key:?} names no ShotMetrics field"
            );
        }
    }

    /// Columns land under their own headers, and the cells that aren't plain
    /// numbers keep their shape: `busy` stays one `|`-joined cell rather than
    /// spilling into the columns after it, and a `NaN` metric is blank — the same
    /// "not measured" `measure()` reports for it.
    #[test]
    fn csv_cells_line_up_with_their_headers() {
        let m = sample_metrics();
        let header: Vec<&str> = CSV_HEADER.split(',').collect();
        let row = m.csv_row();
        let cells: Vec<&str> = row.split(',').collect();
        let cell = |key: &str| {
            cells[header
                .iter()
                .position(|h| *h == key)
                .unwrap_or_else(|| panic!("{key} is not a column"))]
        };
        assert_eq!(cell("shot"), "1");
        assert_eq!(cell("name"), "city");
        assert_eq!(cell("busy"), "map_tiles|lod22");
        assert_eq!(cell("settled"), "false");
        assert_eq!(cell("alt_m"), "1000");
        assert_eq!(
            cell("space_factor"),
            "0.0000",
            "float precision is per column"
        );
        assert_eq!(cell("frame_ms"), "16.60");
        assert_eq!(cell("ground_m"), "", "a NaN metric reads as unmeasured");
        assert_eq!(cell("software_renderer"), "true");
        // The last column is populated, i.e. nothing shifted off the end.
        assert_eq!(cell("vram_reserved_kb"), "0");
    }

    /// The `--expect` key set is derived from `CSV_HEADER`, and the values are read
    /// back out of `Serialize`. Those are two different views of the struct, so
    /// this is the test that keeps them one: a field renamed on one side and not
    /// the other would otherwise make every assertion on it silently unmeasurable.
    #[test]
    fn expectation_keys_match_the_metrics() {
        let m = sample_metrics();
        for key in metric_keys() {
            // `ground_m` is the deliberate exception: it is `NaN` here, which is
            // JSON null, which is "not measured" — and that is the answer.
            if key == "ground_m" {
                assert!(m.measure(key).is_none(), "NaN should read as unmeasured");
                continue;
            }
            assert!(
                m.measure(key).is_some(),
                "{key} is an --expect key but has no value"
            );
        }
        // The two non-numeric fields are excluded on purpose, not by accident.
        assert!(!metric_keys().any(|k| k == "name" || k == "busy"));
    }

    #[test]
    fn expectations_parse_every_operator_and_booleans() {
        let cases = [
            ("globe_rendered>0", 0.0),
            ("globe_rendered >= 1", 1.0),
            ("space_factor<0.5", 0.5),
            ("alt_m<=180000", 180000.0),
            ("settled==true", 1.0),
            ("software_renderer!=false", 0.0),
        ];
        for (spec, want) in cases {
            let e = Expectation::parse(spec).unwrap_or_else(|err| panic!("{spec:?}: {err}"));
            assert_eq!(e.want, want, "{spec:?}");
            // The log line shows what was typed, not the parsed form.
            assert_eq!(e.to_string(), spec);
        }
    }

    /// A typo'd assertion that quietly holds is worse than no assertion — a green
    /// job then claims something nobody checked. Every malformed spec must die at
    /// parse time, before the app is even built.
    #[test]
    fn malformed_expectations_are_rejected_at_parse_time() {
        for bad in [
            "globe_renderd>0",
            "settled=true",
            "globe_rendered",
            "alt_m>banana",
            "alt_m>NaN",
        ] {
            assert!(Expectation::parse(bad).is_err(), "{bad:?} should not parse");
        }
        // And the error names the metrics, so a typo is one message from a fix.
        let err = Expectation::parse("globe_renderd>0").unwrap_err();
        assert!(err.contains("globe_rendered"), "{err}");
    }

    #[test]
    fn expectations_hold_or_report_the_measured_value() {
        let mut m = sample_metrics();
        m.globe_rendered = 42;

        assert!(Expectation::parse("globe_rendered>0")
            .unwrap()
            .check(&m)
            .is_ok());
        assert!(Expectation::parse("settled==true")
            .unwrap()
            .check(&m)
            .is_err());

        // A failure carries the number, so a red run needs no second run to say why.
        let why = Expectation::parse("globe_rendered>100")
            .unwrap()
            .check(&m)
            .unwrap_err();
        assert!(why.contains("42"), "{why}");

        // An unmeasured metric is its own message, not a comparison against nothing.
        let why = Expectation::parse("ground_m>0")
            .unwrap()
            .check(&m)
            .unwrap_err();
        assert!(why.contains("not measured"), "{why}");
    }

    #[test]
    fn a_bad_expectation_fails_the_whole_run_setup() {
        let result = ShotHarness::from_cli(ShotArgs {
            path: "/tmp/s.png".into(),
            at: &["0,0,1000".into()],
            expect: &["globe_rendered>0".into(), "nonsense>1".into()],
            ..default()
        });
        match result {
            Err(e) => assert!(e.starts_with("--expect"), "got {e:?}"),
            Ok(_) => panic!("an unknown metric must not build a harness"),
        }
    }
}
