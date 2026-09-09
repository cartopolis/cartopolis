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
//! `--ui dev` or `--ui dev,layers,stats` (or `--ui all`); see [`open_scripted_ui`].
//! `--ui search` opens the Search panel *and* seeds it a query, since an empty
//! field documents nothing; `--ui hud` brings back the status readout, which is
//! off by default (`hud::StatusHud`); and `--ui menu` opens the app mark's card,
//! which is where the entries that left the map live.
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
use crate::systems::weather::WeatherState;

/// The world state a viewpoint wants to be shot in — the parts of "what does it
/// look like from there" that aren't the camera. Set per shot by a preset
/// manifest (`--shots`); a plain `--shot` run leaves them `None` and the whole
/// run keeps whatever `--time` / `--fog` / `--clouds` / `--rain` seeded.
#[derive(Clone, Default, Debug, PartialEq)]
pub struct ShotEnv {
    /// Hour of day, 0–24.
    pub time: Option<f32>,
    pub fog: Option<f32>,
    pub clouds: Option<f32>,
    pub rain: Option<f32>,
}

impl ShotEnv {
    /// Nothing to apply — the run-wide state stands.
    fn is_empty(&self) -> bool {
        *self == ShotEnv::default()
    }
}

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
    /// Per-shot camera mode, overriding `--shot-mode`.
    pub mode: Option<String>,
    /// Per-shot `--hide-ui`.
    pub hide_ui: Option<bool>,
    /// Per-shot `--ui` panel list.
    pub ui: Option<Vec<String>>,
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
            description: None,
            out: None,
            mode: a.mode.clone(),
            hide_ui: a.hide_ui,
            ui: a.ui.clone(),
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
    /// UI panels to open for the capture (`dev`, `layers`, `bookmarks`, `ai`,
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
    /// Doors drawn on facades from the register's entrance points
    /// (`lod22::DoorCount`).
    pub building_doors: usize,
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
const CSV_HEADER: &str = "shot,name,lat,lng,settled,settled_s,busy,frame_ms,frame_ms_max,flicker_px,\
draws,tris_total,tris_shadow,tris_buildings,tris_lod22,tris_tiles,tris_globe,worker_outstanding,fetch_outstanding,radius_bias,\
tiles_occluded,aerial_tiles,\
alt_m,space_factor,in_space,\
orbit_space,near_m,far_m,exposure_ev100,bloom,globe_albedo,veg_factor,furniture_factor,globe_entities,\
globe_visible,globe_rendered,globe_tiles_loaded,globe_tiles_desired,globe_texture_mb,globe_tile_px,\
route_meshes,route_rendered,particle_emitters,particle_quads,\
grass_patches,grass_slots,grass_rendered,\
bag_buildings,bag_addresses,bag_berths,surveyed_trees,municipal_trees,surveyed_props,municipal_props,surveyed_lines,surveyed_paving,crop_parcels,nav_marks,level_crossings,prop_colliders,flood_defences,flood_defences_measured,interior_rooms,interior_storeys,building_doors,\
lod22_dated,lod22_elements,prop_meshes,\
traffic_cars,traffic_walkers,\
avatar_meshes,avatar_rendered,\
ground_m,height_patches,\
terrain_ready,terrain_relief_m,upload_peak_kb,upload_peak_asset_kb,gpu_resident_kb,\
gpu_resident_peak_kb,software_renderer,vram_allocated_kb,vram_reserved_kb,\
fetch_count,fetch_errors,fetch_mb,fetch_ttfb_ms,fetch_body_ms,\
worker_jobs,worker_busy_ms,worker_wait_ms,worker_split";

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
    ("frame_ms", 2),
    ("frame_ms_max", 2),
    ("space_factor", 4),
    ("exposure_ev100", 3),
    ("bloom", 4),
    ("globe_albedo", 4),
    ("veg_factor", 4),
    ("furniture_factor", 4),
    ("globe_texture_mb", 2),
    ("ground_m", 2),
    ("terrain_relief_m", 2),
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
            shots.push(ShotSpec {
                lat: p[0],
                lng: p[1],
                alt: p[2],
                pitch_deg: angles[0] as f32,
                yaw_deg: angles[1] as f32,
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
        })
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
                    out: Some(manifest.out_path(manifest_path, p)),
                    mode: p.mode.clone().or_else(|| manifest.mode.clone()),
                    hide_ui: p.hide_ui.or(manifest.hide_ui),
                    ui: p.ui.clone(),
                    env: ShotEnv {
                        time: p.time,
                        fog: p.fog,
                        clouds: p.clouds,
                        rain: p.rain,
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
        })
    }

    /// Camera mode for shot `i` — the preset's own, else the run-wide one.
    pub fn mode_for(&self, i: usize) -> String {
        self.shots
            .get(i)
            .and_then(|s| s.mode.clone())
            .unwrap_or_else(|| self.mode.clone())
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
}

/// True while a scripted capture is configured — the run condition the user
/// store uses to keep out of the way.
pub fn harness_active(harness: Option<Res<ShotHarness>>) -> bool {
    harness.is_some()
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
/// `prefs` joined them when the stats overlay became a preference rather than a
/// developer-panel field. A scripted run never reads or writes `user.json`, so
/// writing it here is an in-memory override for the capture and nothing more.
#[derive(bevy::ecs::system::SystemParam)]
pub struct ScriptedCards<'w, 's> {
    menu: ResMut<'w, crate::systems::ui::MainMenu>,
    sign_in: ResMut<'w, crate::systems::login_screen::LoginScreen>,
    prefs: ResMut<'w, crate::resources::AppPrefs>,
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
    "stats",
    "hud",
    "menu",
    "ai",
    "avatar",
    "appearance",
    "layers",
    "places",
    "bookmarks",
    "route",
    "driver",
    "search",
    "place",
    "settings",
    "graphics",
    "world",
    "signin",
    "chat",
    "sit",
    "locate",
    "fire",
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
    mut ai: ResMut<crate::resources::AiAssistantState>,
    mut avatar: ResMut<crate::resources::AvatarChooserState>,
    mut layers: ResMut<crate::systems::data_layers::DataLayers>,
    mut bookmarks: ResMut<crate::systems::bookmarks::BookmarksState>,
    mut right_panel: ResMut<crate::resources::RightPanel>,
    mut route: ResMut<crate::systems::navigation::RouteState>,
    mut driver: ResMut<crate::systems::driver::DriverState>,
    mut nav: ResMut<crate::systems::geo_nav::GeoNavState>,
    mut status: ResMut<crate::systems::hud::StatusHud>,
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
    dev.open = want("dev");
    cards.prefs.show_stats = want("stats");
    // The status readout is off by default now (`hud::StatusHud`), so a capture
    // that wants to document it has to ask — `--ui hud`.
    status.open = want("hud");
    // `--ui signin` opens the sign-in card. It is a modal over everything else,
    // so it is written before the panels below rather than beside them — and it
    // is here for the reason `--ui menu` is: a card nothing can screenshot is a
    // card nobody reviews, and this one is a form that only opens by hand.
    cards.sign_in.open = want("signin");
    // `--ui menu` opens the app mark's card. It is the one piece of chrome a
    // scripted run could not reach at all, and it is where the entries that left
    // the map now live — Set Home, Copy location and the status readout's own
    // switch. A control nothing can screenshot is a control nobody reviews.
    cards.menu.open = want("menu");
    ai.open = want("ai");
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
    } else if want("ai") {
        crate::resources::RightPanel::Assistant
    } else if want("search") {
        crate::resources::RightPanel::Search
    } else if want("avatar") || want("appearance") {
        // The chooser moved into the `RightPanel` slot with the rest; the
        // `avatar.open` bool above is what the HUD reads, not the panel, so
        // `--ui avatar` opened nothing at all until this arm existed.
        crate::resources::RightPanel::Appearance
    } else if want("settings") || want("graphics") || want("world") {
        // `--ui graphics` opens Settings on the Graphics tab, `--ui world` on the
        // World tab. A tab is not reachable by opening the panel alone, so naming
        // the panel would capture whichever tab was last selected — which for a
        // fresh scripted world is always General, i.e. never the one being
        // reviewed.
        cards.settings.tab = if want("graphics") {
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
    if want("route") || want("driver") {
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
            pending_emote: None,
            last_emote_sequence: None,
            seated: None,
            seat_blend: 0.0,
        },
        crate::components::RemoteAvatarMotionHint::default(),
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
                    region_id: 0,
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
    camera: Query<
        &Transform,
        (
            With<FreeCamera>,
            Without<crate::systems::chat_overlay::ScriptedSpeaker>,
        ),
    >,
    mut speaker: Query<&mut Transform, With<crate::systems::chat_overlay::ScriptedSpeaker>>,
) {
    if !overlay.demo {
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
) {
    let panels = harness.ui_for(progress.index);
    let all = panels.iter().any(|u| u == "all");
    if !(all || panels.iter().any(|u| u == "locate")) {
        if tracking.active {
            tracking.exit();
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
    tracking.seed_for_capture(
        crate::platform::location::GpsFix {
            lat,
            lng,
            accuracy_m: 8.0,
            speed_ms: Some(1.4),
            heading_deg: Some(45.0),
        },
        Some(crate::platform::orientation::DeviceHeading {
            bearing_deg: 45.0,
            accuracy_deg: Some(10.0),
        }),
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
    furniture: Res<'w, crate::systems::street_furniture::FurnitureLod>,
    surfaces: Res<'w, crate::systems::map_geometry::MapSurfaces>,
    osm_buildings: Res<'w, crate::systems::osm_buildings::OsmBuildings>,
    lod22: Res<'w, crate::systems::lod22::Lod22Buildings>,
    adapter: Res<'w, bevy::render::renderer::RenderAdapterInfo>,
    avatar: AvatarReadout<'w, 's>,
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
    /// The same layer's support field, for its storey tally.
    interior_floors: Res<'w, crate::systems::interiors::InteriorFloors>,
    /// The flood defences, for their two counts alone — see
    /// `ShotMetrics::flood_defences`.
    dikes: Res<'w, crate::systems::dikes::Dikes>,
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
    weather: Res<WeatherState>,
    route: Res<crate::systems::navigation::RouteState>,
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
        ),
        With<FreeCamera>,
    >,
    visuals: VisualCounts,
) {
    if let Some(frames) = progress.exit_in {
        let left = frames.saturating_sub(1);
        progress.exit_in = Some(left);
        if left == 0 {
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
        return;
    }
    // Capture landed: on to the next viewpoint, or done. The CPU pool was paused
    // for the readback (see below); release it here so the next shot's streamers —
    // and the jobs parked during this one — start again.
    if progress.saved.swap(false, Ordering::Relaxed) {
        crate::platform::workers::set_paused(false);
        progress.index += 1;
        if progress.index >= harness.shots.len() {
            // Every viewpoint captured — flush the metrics before draining to exit.
            harness.write_metrics(&progress.metrics);
            progress.exit_in = Some(2);
        } else {
            progress.placed = false;
            progress.fired = false;
            progress.retried = false;
            progress.verdict = None;
        }
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
        return;
    }

    // Settle: capture once *every* streamer has been quiet for `--settle`
    // seconds, or when `--wait` runs out (whichever comes first).
    //
    // "Every" is load-bearing and was learned the hard way. This used to watch
    // only the two tile streams — and `MapStream::streaming_busy` was stuck true
    // for the life of the process (a fetch was never dropped from `requested`),
    // so every ground shot silently ran to the `--wait` cap instead. Fixing that
    // bug turned each README capture into a half-built city: the imagery goes
    // quiet in ~4 s, while the vector surfaces, the OSM detail patch and the
    // 3DBAG cells are still arriving. The accidental 45 s sleep was doing the
    // waiting, so the settle test has to name the streamers it was covering for.
    //
    // A `--weather-live` run also holds until the first met.no sample lands, so the
    // capture shows real conditions rather than the default clear sky, and a
    // `--ui route` run holds until the solve lands — otherwise the shot documents
    // an empty planner and a map with no line on it (ADR-037).
    let routing = matches!(
        route.status,
        crate::systems::navigation::SolveStatus::Fetching(..)
            | crate::systems::navigation::SolveStatus::Solving
    );
    let (surfaces, osm, lod22) = (&readouts.surfaces, &readouts.osm_buildings, &readouts.lod22);
    // Named, not a single `||` chain: when a capture gives up at `--wait`, *which*
    // streamer was still going is the entire diagnosis, and a bare "not settled"
    // sends you back to re-run with a longer cap just to learn it.
    let busy: Vec<&'static str> = [
        ("globe", globe.streaming_busy()),
        ("map_tiles", stream.streaming_busy()),
        ("surfaces", surfaces.streaming_busy()),
        ("osm_buildings", osm.streaming_busy()),
        ("lod22", lod22.streaming_busy()),
        ("grass", readouts.grass.streaming_busy()),
        ("street_detail", readouts.street_detail.streaming_busy()),
        ("tall_structures", readouts.tall_structures.streaming_busy()),
        ("routing", routing),
        ("weather", weather.live && !weather.ready()),
    ]
    .into_iter()
    .filter_map(|(name, busy)| busy.then_some(name))
    .collect();
    if busy.is_empty() {
        progress.quiet += time.delta_secs();
    } else {
        progress.quiet = 0.0;
    }
    if progress.verdict.is_none() {
        if progress.quiet < harness.settle && progress.elapsed < harness.wait {
            return;
        }
        // Which arm of the test above let us through. Everything measured below
        // describes a partially streamed world when this is false.
        progress.verdict = Some(SettleVerdict {
            settled: progress.quiet >= harness.settle,
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
    if let Ok((cell, tf, projection, exposure, bloom)) = camera.single() {
        let (near, far) = match projection {
            Projection::Perspective(p) => (p.near, p.far),
            _ => (0.0, 0.0),
        };
        let shot = &harness.shots[progress.index];
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
        let m = ShotMetrics {
            shot: progress.index + 1,
            name: shot.name.clone(),
            lat: shot.lat,
            lng: shot.lng,
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
            // A tier with no glare has no `Bloom` component at all, and 0.0 is the
            // honest reading for it — the same number the component would have
            // carried, since `bloom_intensity` returns exactly 0.0 on that tier.
            bloom: bloom.map_or(0.0, |b| b.intensity),
            globe_albedo: readouts.globe_albedo.0,
            veg_factor: readouts.vegetation.factor,
            furniture_factor: readouts.furniture.factor,
            route_meshes: visuals.route.iter().count(),
            route_rendered: visuals.route.iter().filter(|vv| vv.get()).count(),
            particle_emitters: particles.0 as usize,
            particle_quads: particles.1 as usize,
            grass_patches: grass.0,
            grass_slots: grass.1,
            grass_rendered: grass.2,
            bag_buildings: readouts.bag.len(),
            bag_addresses: readouts.bag.addresses().len(),
            bag_berths: readouts.bag.berths().len(),
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
            interior_rooms: readouts.interiors.rooms(),
            interior_storeys: readouts.interior_floors.storeys(),
            building_doors: readouts.lod22.doors(),
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
            upload_peak_kb: readouts.uploads.peak_kb(),
            upload_peak_asset_kb: readouts.uploads.peak_asset_kb(),
            gpu_resident_kb: readouts.uploads.resident_kb(),
            gpu_resident_peak_kb: readouts.uploads.peak_resident_kb(),
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
                    upload_peak_kb = m.upload_peak_kb,
                    upload_peak_asset_kb = m.upload_peak_asset_kb,
                    gpu_resident_kb = m.gpu_resident_kb,
                    gpu_resident_peak_kb = m.gpu_resident_peak_kb,
                    fetch_count = m.fetch_count,
                    fetch_errors = m.fetch_errors,
                    fetch_mb = format!("{:.2}", m.fetch_mb),
                    fetch_ttfb_ms = m.fetch_ttfb_ms,
                    fetch_body_ms = m.fetch_body_ms,
                    worker_jobs = m.worker_jobs,
                    worker_busy_ms = m.worker_busy_ms,
                    worker_wait_ms = m.worker_wait_ms,
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

    fire_capture(
        &mut commands,
        &harness,
        &mut progress,
        target.as_deref(),
        &mut shot_state,
    );
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
            interior_rooms: 0,
            interior_storeys: 0,
            building_doors: 0,
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
            particle_emitters: 0,
            particle_quads: 0,
            avatar_meshes: 0,
            avatar_rendered: 0,
            ground_m: f32::NAN,
            height_patches: 0,
            terrain_ready: false,
            terrain_relief_m: 0.0,
            upload_peak_kb: 0,
            upload_peak_asset_kb: 0,
            gpu_resident_kb: 0,
            gpu_resident_peak_kb: 0,
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
