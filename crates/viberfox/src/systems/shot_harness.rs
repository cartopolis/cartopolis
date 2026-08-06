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
//! cargo run -p viberfox --profile dev-bevy -- \
//!     --shot /tmp/shot.png --at 53.2194,6.5665,180000 --look=-90,0
//!
//! # a sweep: one app launch, one PNG per --at (index- and altitude-tagged)
//! cargo run -p viberfox --profile dev-bevy -- --shot /tmp/sweep.png \
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
//! `--ui search` is the same idea for the auto-hiding address bar, which reveals
//! on a pointer at the top edge and so has no other way to appear in a capture.
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
//! cargo run -p viberfox --profile dev-bevy -- --shot /tmp/sweep.png \
//!     --at 53.2194,6.5665,2000 --at 53.2194,6.5665,4000000 --frames 24 \
//!     --look=-90,0 --csv /tmp/ramps.csv
//! ```

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

use crate::resources::{CameraState, ScreenshotState};
use crate::systems::free_camera::FreeCamera;
use crate::systems::geo_nav::{CameraPose, GeoNavState, Teleport};
use crate::systems::globe::{GlobeMesh, GlobeState, GlobeSurfaceAlbedo};
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
            let t = if frames == 1 { 0.0 } else { k as f64 / (frames - 1) as f64 };
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
    /// `VIBE_TILE_URL` still wins). Applied in `main` when the resource is built.
    pub tile_url: Option<String>,
    /// Dump the full per-capture [`ShotMetrics`] as JSON on exit — a single
    /// object for one shot, an array for a sweep. The "inspect the settled world
    /// from a shell" snapshot.
    pub dump_state: Option<PathBuf>,
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
    /// Mean frame time over the settled frames (ms) — the space tier's render cost.
    pub frame_ms: f32,
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
    /// Drawn-route entities (ADR-037) and how many the view actually rendered.
    /// The same "black image vs black scene" question the globe counts answer: a
    /// route that is built but not rendered is a visibility or culling problem,
    /// one that is rendered but unseen is a depth or colour problem.
    pub route_meshes: usize,
    pub route_rendered: usize,
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
    /// The capture ran on a CPU rasteriser (Mesa lavapipe in a container or CI)
    /// rather than a GPU. Geometry, placement, culling and UI layout are the same
    /// picture either way; the tone-mapped sky is not, so a colour or lighting
    /// judgement made against a shot with this set is not evidence.
    pub software_renderer: bool,
}

/// Field order for the `--csv` header and rows. Kept adjacent to [`ShotMetrics::csv_row`]
/// so the two never drift; a mismatch would silently mislabel columns.
/// `busy` is `|`-joined rather than comma-joined for the obvious reason.
const CSV_HEADER: &str = "shot,name,lat,lng,settled,settled_s,busy,frame_ms,alt_m,space_factor,in_space,\
orbit_space,near_m,far_m,exposure_ev100,bloom,globe_albedo,veg_factor,furniture_factor,globe_entities,\
globe_visible,globe_rendered,route_meshes,route_rendered,ground_m,height_patches,\
terrain_ready,terrain_relief_m,software_renderer";

impl ShotMetrics {
    /// One CSV line, columns in [`CSV_HEADER`] order.
    fn csv_row(&self) -> String {
        format!(
            "{},{},{},{},{},{:.2},{},{:.2},{},{:.4},{},{},{},{},{:.3},{:.4},{:.4},{:.4},{:.4},{},{},{},{},{},{:.2},{},{},{:.2},{}",
            self.shot,
            self.name.as_deref().unwrap_or(""),
            self.lat,
            self.lng,
            self.settled,
            self.settled_s,
            self.busy.join("|"),
            self.frame_ms,
            self.alt_m,
            self.space_factor,
            self.in_space,
            self.orbit_space,
            self.near_m,
            self.far_m,
            self.exposure_ev100,
            self.bloom,
            self.globe_albedo,
            self.veg_factor,
            self.furniture_factor,
            self.globe_entities,
            self.globe_visible,
            self.globe_rendered,
            self.route_meshes,
            self.route_rendered,
            self.ground_m,
            self.height_patches,
            self.terrain_ready,
            self.terrain_relief_m,
            self.software_renderer,
        )
    }
}

impl ShotHarness {
    /// Build from the raw CLI strings: `at` is one or more "lat,lng,alt", `look`
    /// zero or more "pitch,yaw" (paired by index; the last one carries over).
    /// `frames > 0` densifies the waypoints into an N-frame sweep (see
    /// [`densify_path`]).
    #[allow(clippy::too_many_arguments)]
    pub fn from_cli(
        path: PathBuf,
        at: &[String],
        look: &[String],
        size: &str,
        settle: f32,
        wait: f32,
        hide_ui: bool,
        ui: Vec<String>,
        mode: String,
        frames: u32,
        csv: Option<PathBuf>,
        dump_state: Option<PathBuf>,
    ) -> Result<Self, String> {
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
        let shots = densify_path(&shots, frames);
        let size = parse_size(size).ok_or_else(|| format!("--size wants WxH, got {size:?}"))?;
        Ok(Self { path, shots, size, settle, wait, hide_ui, ui, mode, tile_url: None, csv, dump_state })
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
            settle: settle_override.or(manifest.settle).unwrap_or(DEFAULT_SETTLE),
            wait: wait_override.or(manifest.wait).unwrap_or(DEFAULT_WAIT),
            hide_ui: manifest.hide_ui.unwrap_or(false),
            ui: Vec::new(),
            mode: manifest.mode.clone().unwrap_or_else(|| "free".to_string()),
            tile_url: manifest.tile_url.clone(),
            csv,
            dump_state,
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
        self.shots.get(i).and_then(|s| s.hide_ui).unwrap_or(self.hide_ui)
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
                Ok(()) => tracing::info!(path = %path.display(), rows = metrics.len(), "shot harness: wrote CSV"),
                Err(e) => tracing::error!(path = %path.display(), error = %e, "shot harness: CSV write failed"),
            }
        }
        if let Some(path) = &self.dump_state {
            let json = match metrics {
                [one] => serde_json::to_string_pretty(one),
                many => serde_json::to_string_pretty(many),
            };
            match json {
                Ok(s) => match std::fs::write(path, s) {
                    Ok(()) => tracing::info!(path = %path.display(), shots = metrics.len(), "shot harness: wrote state dump"),
                    Err(e) => tracing::error!(path = %path.display(), error = %e, "shot harness: dump write failed"),
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
        let stem = self.path.file_stem().map(|s| s.to_string_lossy().into_owned());
        let ext = self.path.extension().map(|s| s.to_string_lossy().into_owned());
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
    /// Frames and wall-clock seconds since placement, for the mean frame time
    /// reported with each capture (how the space tier's render cost is A/B'd).
    frames: u32,
    frame_time: f32,
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
    /// Frames left before exiting (drains after the last save).
    exit_in: Option<u32>,
    /// One [`ShotMetrics`] per captured viewpoint, flushed to `--csv`/`--dump-state`
    /// when the run completes.
    metrics: Vec<ShotMetrics>,
}

/// True while a scripted capture is configured — the run condition the user
/// store uses to keep out of the way.
pub fn harness_active(harness: Option<Res<ShotHarness>>) -> bool {
    harness.is_some()
}

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
    dev.show_stats = want("stats");
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
    } else if want("avatar") || want("appearance") {
        // The chooser moved into the `RightPanel` slot with the rest; the
        // `avatar.open` bool above is what the HUD reads, not the panel, so
        // `--ui avatar` opened nothing at all until this arm existed.
        crate::resources::RightPanel::Appearance
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
    // chrome). `VIBE_ROUTE_FROM` / `VIBE_ROUTE_TO` ("lat,lng") override the pair, so
    // a specific route can be reproduced headlessly for debugging (the `VIBE_*` env
    // convention).
    if want("route") || want("driver") {
        let seed = |var: &str, default: (f64, f64)| {
            std::env::var(var)
                .ok()
                .and_then(|s| crate::systems::geo_nav::parse_lat_lng(&s))
                .unwrap_or(default)
        };
        route.from.set("Route start", seed("VIBE_ROUTE_FROM", (53.2194, 6.5665)));
        route.to.set("Route end", seed("VIBE_ROUTE_TO", (53.2109, 6.5642)));
    }
    // The address bar is the one piece of chrome that cannot be *asked* for: it
    // auto-hides and reveals on a pointer at the top edge, which a headless run
    // has none of. `--ui search` seeds a query instead, which both pins the bar
    // open (the field takes focus) and fills the suggestion dropdown — so the
    // capture shows the thing the bar exists for, not an empty field.
    // `VIBE_SEARCH_QUERY` overrides the demo query, per the `VIBE_*` convention.
    if want("search") {
        let query = std::env::var("VIBE_SEARCH_QUERY").unwrap_or_else(|_| "Amsterdam".into());
        nav.seed_search(query);
    }
    // Surface typos: anything that matched nothing is neither a panel nor `all`.
    let known = [
        "dev", "stats", "ai", "avatar", "appearance", "layers", "places", "bookmarks", "route",
        "driver", "search", "all",
    ];
    for u in &panels {
        if !known.contains(&u.as_str()) {
            tracing::warn!(name = %u, "shot harness: unknown --ui element (ignored)");
        }
    }
    tracing::info!(shot = progress.index + 1, ui = ?panels, "shot harness: opened scripted UI panels");
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
    image.texture_descriptor.usage = TextureUsages::TEXTURE_BINDING
        | TextureUsages::COPY_SRC
        | TextureUsages::RENDER_ATTACHMENT;
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
    stream: Res<MapStream>,
    mut nav: ResMut<GeoNavState>,
    mut dev: ResMut<crate::resources::DevPanelState>,
) {
    if progress.placed || stream.geo_anchor().is_none() {
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

/// Wait for the view to settle, capture, then move to the next shot (or exit).
#[allow(clippy::too_many_arguments)]
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
    // Resources read into `ShotMetrics`, plus the streamers the settle test waits
    // on (`.5`–`.7`), tupled into one `SystemParam`: this system is at Bevy's
    // 16-parameter ceiling, and a new inspectable fact is exactly the kind of
    // thing that keeps getting added here.
    readouts: (
        Res<GlobeSurfaceAlbedo>,
        Res<crate::systems::vegetation::VegetationLod>,
        Res<crate::systems::height::HeightLayer>,
        Res<crate::systems::terrain::TerrainField>,
        Res<crate::systems::street_furniture::FurnitureLod>,
        Res<crate::systems::map_geometry::MapSurfaces>,
        Res<crate::systems::osm_buildings::OsmBuildings>,
        Res<crate::systems::lod22::Lod22Buildings>,
        Res<bevy::render::renderer::RenderAdapterInfo>,
    ),
    camera: Query<(&GridCell, &Transform, &Projection, &Exposure, &Bloom), With<FreeCamera>>,
    globe_meshes: Query<(&bevy::camera::visibility::ViewVisibility, &Visibility), With<GlobeMesh>>,
    route_meshes: Query<
        &bevy::camera::visibility::ViewVisibility,
        With<crate::systems::navigation::RouteVisual>,
    >,
) {
    if let Some(frames) = progress.exit_in.as_mut() {
        *frames = frames.saturating_sub(1);
        if *frames == 0 {
            exit.write(AppExit::Success);
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
    if progress.quiet > 0.0 {
        progress.frames += 1;
        progress.frame_time += time.delta_secs();
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
                fire_capture(&mut commands, &harness, &mut progress, target.as_deref(), &mut shot_state);
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
    // `--weather-live` run also holds until the first met.no sample lands, so the
    // capture shows real conditions rather than the default clear sky, and a
    // `--ui route` run holds until the solve lands — otherwise the shot documents
    // an empty planner and a map with no line on it (ADR-037).
    let routing = matches!(
        route.status,
        crate::systems::navigation::SolveStatus::Fetching(..)
            | crate::systems::navigation::SolveStatus::Solving
    );
    let (surfaces, osm, lod22) = (&readouts.5, &readouts.6, &readouts.7);
    // Named, not a single `||` chain: when a capture gives up at `--wait`, *which*
    // streamer was still going is the entire diagnosis, and a bare "not settled"
    // sends you back to re-run with a longer cap just to learn it.
    let busy: Vec<&'static str> = [
        ("globe", globe.streaming_busy()),
        ("map_tiles", stream.streaming_busy()),
        ("surfaces", surfaces.streaming_busy()),
        ("osm_buildings", osm.streaming_busy()),
        ("lod22", lod22.streaming_busy()),
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
        let m = ShotMetrics {
            shot: progress.index + 1,
            name: shot.name.clone(),
            lat: shot.lat,
            lng: shot.lng,
            settled,
            settled_s: verdict.at,
            busy: verdict.busy.iter().map(|s| (*s).to_string()).collect(),
            frame_ms: 1000.0 * progress.frame_time / progress.frames.max(1) as f32,
            alt_m: globe.altitude_m(cell, tf) as i64,
            space_factor: globe.space_factor(cell, tf),
            in_space: globe.in_space(cell, tf),
            orbit_space: cam_state.orbit_space,
            near_m: near as i64,
            far_m: far as i64,
            exposure_ev100: exposure.ev100,
            bloom: bloom.intensity,
            globe_albedo: readouts.0 .0,
            veg_factor: readouts.1.factor,
            furniture_factor: readouts.4.factor,
            route_meshes: route_meshes.iter().count(),
            route_rendered: route_meshes.iter().filter(|vv| vv.get()).count(),
            globe_entities: globe_meshes.iter().count(),
            globe_visible: globe_meshes.iter().filter(|(_, v)| **v == Visibility::Visible).count(),
            globe_rendered: globe_meshes.iter().filter(|(vv, _)| vv.get()).count(),
            ground_m: readouts.2.ground_m.unwrap_or(f32::NAN),
            height_patches: readouts.2.fetched,
            terrain_ready: readouts.3.is_ready(),
            terrain_relief_m: readouts.3.range_m.1 - readouts.3.range_m.0,
            software_renderer: readouts.8.device_type == egui_wgpu::wgpu::DeviceType::Cpu,
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
            globe_entities = m.globe_entities,
            globe_visible = m.globe_visible,
            globe_rendered = m.globe_rendered,
            ground_m = format!("{:.2}", m.ground_m),
            height_patches = m.height_patches,
            terrain_ready = m.terrain_ready,
            terrain_relief_m = format!("{:.2}", m.terrain_relief_m),
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
        progress.metrics.push(m);
    }

    fire_capture(&mut commands, &harness, &mut progress, target.as_deref(), &mut shot_state);
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
    let path = harness.path_for(progress.index);
    let screenshot = match target {
        Some(t) => Screenshot::image(t.0.clone()),
        None => Screenshot::primary_window(),
    };
    commands
        .spawn(screenshot)
        .observe(move |captured: On<ScreenshotCaptured>| {
            let (saved, path) = (saved.clone(), path.clone());
            // A preset writes to a named directory (`docs/img/`); create it so a
            // fresh checkout doesn't lose the run to a missing folder.
            if let Some(dir) = path.parent().filter(|d| !d.as_os_str().is_empty()) {
                let _ = std::fs::create_dir_all(dir);
            }
            match captured.image.clone().try_into_dynamic() {
                // Alpha carries HDR brightness rather than transparency here
                // (as in the README capture path), so drop it.
                Ok(img) => match img.to_rgb8().save(&path) {
                    Ok(()) => {
                        tracing::info!(path = %path.display(), "shot harness: saved");
                        saved.store(true, Ordering::Relaxed);
                    }
                    Err(e) => {
                        tracing::error!(path = %path.display(), error = %e, "shot save failed")
                    }
                },
                Err(e) => tracing::error!(error = %e, "shot decode failed"),
            }
        });
}

#[cfg(test)]
mod tests {
    use super::*;

    fn spec(lat: f64, lng: f64, alt: f64) -> ShotSpec {
        ShotSpec { lat, lng, alt, pitch_deg: -90.0, yaw_deg: 0.0, ..default() }
    }

    #[test]
    fn densify_passthrough_when_off_or_single() {
        let wp = vec![spec(0.0, 0.0, 2000.0), spec(1.0, 1.0, 4000.0)];
        assert_eq!(densify_path(&wp, 0).len(), 2, "frames=0 leaves waypoints as-is");
        let one = vec![spec(0.0, 0.0, 2000.0)];
        assert_eq!(densify_path(&one, 8).len(), 1, "a single waypoint can't be swept");
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
        let wp = vec![spec(0.0, 0.0, 1000.0), spec(5.0, 0.0, 1000.0), spec(10.0, 0.0, 1000.0)];
        let out = densify_path(&wp, 5);
        assert_eq!(out.len(), 5);
        assert!((out[2].lat - 5.0).abs() < 1e-9, "centre sample is the middle waypoint");
    }

    #[test]
    fn frames_requires_two_waypoints() {
        let result = ShotHarness::from_cli(
            "/tmp/s.png".into(),
            &["0,0,1000".into()],
            &[],
            "64x64",
            0.1,
            1.0,
            false,
            vec![],
            "free".into(),
            8,
            None,
            None,
        );
        match result {
            Err(e) => assert!(e.contains("--frames"), "got {e:?}"),
            Ok(_) => panic!("expected --frames with one waypoint to error"),
        }
    }

    #[test]
    fn path_for_indexes_and_pads_a_sweep() {
        let h = ShotHarness::from_cli(
            "/tmp/sweep.png".into(),
            &["0,0,2000".into(), "0,0,4000000".into()],
            &[],
            "64x64",
            0.1,
            1.0,
            false,
            vec![],
            "free".into(),
            12,
            None,
            None,
        )
        .unwrap();
        // 12 frames → indices 00..11, zero-padded to width 2, altitude-tagged.
        assert_eq!(h.path_for(0), PathBuf::from("/tmp/sweep_00_2000m.png"));
        assert!(h.path_for(11).to_string_lossy().starts_with("/tmp/sweep_11_"));
    }

    #[test]
    fn manifest_run_names_its_own_files_and_carries_per_shot_state() {
        // The checked-in manifest is the docs' source of truth — if it stops
        // loading, `cargo shots` silently stops regenerating the README images.
        let path = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../docs/shots.toml");
        let h = ShotHarness::from_manifest(&path, &[], None, Some(0.1), Some(1.0), None, None)
            .expect("checked-in docs/shots.toml must load");
        assert!(h.shots.len() >= 2, "manifest should describe a set of views");
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
            ShotHarness::from_manifest(&path, &[first.clone()], None, None, None, None, None)
                .unwrap();
        assert_eq!(one.shots.len(), 1);
        assert_eq!(one.shots[0].name.as_deref(), Some(first.as_str()));
    }

    #[test]
    fn manifest_size_can_be_overridden_from_the_cli() {
        let path = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../docs/shots.toml");
        let h = ShotHarness::from_manifest(&path, &[], Some("640x360"), None, None, None, None)
            .unwrap();
        assert_eq!(h.size, UVec2::new(640, 360));
    }

    #[test]
    fn csv_header_and_row_have_matching_arity() {
        let m = ShotMetrics {
            shot: 1,
            name: Some("city".into()),
            lat: 0.0,
            lng: 0.0,
            settled: false,
            // Two entries on purpose: `busy` is the one list-valued column, and
            // joining it with a comma would add columns to this row and silently
            // shift every header after it.
            busy: vec!["map_tiles".into(), "lod22".into()],
            settled_s: 1.0,
            frame_ms: 16.6,
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
            furniture_factor: 0.0,
            globe_entities: 0,
            globe_visible: 0,
            globe_rendered: 0,
            route_meshes: 0,
            route_rendered: 0,
            ground_m: f32::NAN,
            height_patches: 0,
            terrain_ready: false,
            terrain_relief_m: 0.0,
            software_renderer: true,
        };
        assert_eq!(
            CSV_HEADER.split(',').count(),
            m.csv_row().split(',').count(),
            "CSV header and row column counts drifted"
        );
    }
}
