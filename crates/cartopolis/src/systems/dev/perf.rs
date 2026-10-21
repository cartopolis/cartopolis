//! The performance suite: named scenarios, measured windows, and a report.
//!
//! **Built on Bevy's own diagnostics wherever they exist.** An earlier version of
//! this module hand-rolled a `/proc` reader for process CPU and memory and its own
//! frame-time statistics, both of which Bevy already publishes — that is a second
//! implementation of something the engine maintains, and it was deleted. What is
//! left here is only the part Bevy does not have: *when* to measure, *what a
//! scenario is*, and the two columns (allocation traffic, per-system attribution)
//! that no engine plugin provides.
//!
//! What comes from Bevy, all through one `DiagnosticsStore`:
//!
//! | plugin | what it answers |
//! |---|---|
//! | `FrameTimeDiagnosticsPlugin` | `fps`, `frame_time` |
//! | `SystemInformationDiagnosticsPlugin` | `process/cpu_usage`, `process/mem_usage` |
//! | `EntityCountDiagnosticsPlugin` | `entity_count` |
//! | `MeshAllocatorDiagnosticPlugin` | slabs, slab bytes, mesh allocations |
//! | `RenderDiagnosticsPlugin` | per render pass: `elapsed_gpu`, `elapsed_cpu`, and pipeline statistics |
//!
//! The last of those is the one worth adding for: `fragment_shader_invocations` is
//! a **count** of how many times the fragment shader ran — fill and overdraw
//! measured rather than timed — and a count is the same arithmetic on this
//! container's software rasteriser as on an Adreno. It is the only GPU-side number
//! here that transfers. Timestamp queries and pipeline statistics are Vulkan and
//! DX12 only, which covers this container and Android and excludes Metal and the
//! web; where they are missing the columns come back absent rather than zero.
//!
//! None of these plugins asks anything new of the device. `WgpuSettings` defaults
//! to `Functionality` priority, under which `initialize_renderer` already enables
//! every feature the adapter reports — so adding them cannot fail device creation.
//!
//! ## The three windows
//!
//! Each scenario is flown, settled and captured by the existing `--shot` harness,
//! then measured over spans that harness already delimits — two still, one turning:
//!
//! * **`load`** — teleport to settle verdict. The streaming burst: fetches,
//!   decodes, mesh builds, uploads. What a teleport or a cold start costs.
//! * **`steady`** — a run of frames after the shutter, with the world *meant* to be
//!   settled. Check [`Window::quiet`] before reading it as an idle number: at
//!   ground level the settle test passes while the vector-tile streamer is still
//!   trickling, and a window that was still building the city is a streaming
//!   measurement wearing the wrong label.
//!
//! * **`turn`** — the same settled world while the camera yaws on the spot,
//!   [`TURN_DEG_PER_FRAME`] every frame for [`TURN_FRAMES`] frames. Nothing moves
//!   but the view direction, so whatever this window costs over `steady` is what
//!   turning costs: the frustum sweeping new entities into view, the shadow
//!   cascades refitting, and anything the client keys on the heading. A person
//!   reported the frame rate collapsing the moment they turned the camera while
//!   every still view measured well — `steady` cannot see that by construction.
//!
//! The yaw advances by a fixed angle per *frame*, not per second, so two runs
//! sweep the same views whatever their frame rate. `load` and `steady` leave the
//! camera where the harness put it.
//!
//! ## Running it
//!
//! ```bash
//! cargo perf                                    # every scenario → /tmp/perf.json
//! cargo perf --only orbit                       # a subset
//! cargo perf --repeat 3 --only orbit            # fly it three times: median and range
//! cargo perf --features perf                    # + allocations and the GPU passes
//! ```
//!
//! ## Repeats
//!
//! One flight is one sample, and one sample differenced against another is how
//! every withdrawn result in `docs/notes/performance-suite.md` was produced. So
//! the report never carries a bare number: each window is a [`WindowSummary`]
//! whose columns are a [`Stat`] — runs, min, median, max — over however many
//! flights `--repeat` asked for. `--repeat 1` (the default) gives `min == median
//! == max`, by construction rather than by a special case.
//!
//! Repeats are flown **round-robin** (`a b c a b c`), not blocked (`a a b b`):
//! the drift worth exposing is the shared box, and blocked repeats put a slow
//! patch entirely inside one scenario's samples, where it reads as the scene.
//! And a repeat's `load` window is **not a cold one** — the tiles are already in
//! the process's memory and on disk — so `load` medians describe a re-teleport.
//! `steady` is what the flag is for.
//!
//! ## Patrols
//!
//! A scenario is a place somebody already suspected. A `[[patrol]]` is how to
//! find the ones nobody did: a grid of stops over an area, at one or more
//! altitudes and headings, expanded into ordinary scenarios when the manifest
//! loads (`docs/patrol.toml`). Each stop is flown and measured like any other,
//! and the report gains a [`PatrolReport`] per patrol — for each column that
//! transfers, the dearest stops and how far above the patrol's own median they
//! sit. That ranking is the whole product: a stop that draws three times what
//! its neighbours draw is a question with a pose attached, and `--only <stop>`
//! flies it again on its own.
//!
//! The stops are flown serpentine, so each move is one grid step and the
//! streamers keep most of what they hold. A stop's `load` window is therefore
//! the cost of a short move, not of a teleport, and is left out of the ranking.

use std::path::{Path, PathBuf};

use bevy::diagnostic::DiagnosticsStore;
use bevy::prelude::*;
use serde::{Deserialize, Serialize};

use crate::systems::shot_harness::{ShotEnv, ShotHarness, ShotProgress, ShotSpec};

/// Manifest path used when `--perf` is passed without a value.
pub const DEFAULT_MANIFEST: &str = "docs/perf.toml";

/// Frames in the `steady` window when neither manifest nor CLI says.
///
/// Long enough that one hitch does not own the mean, short enough that a ground
/// scenario on a CPU rasteriser does not spend minutes producing a number whose
/// spread is already wider than anything it could detect.
pub const DEFAULT_STEADY_FRAMES: u32 = 60;

/// Frames in the `turn` window.
pub const TURN_FRAMES: u32 = 90;

/// How far the `turn` window yaws the camera each frame: 180° over the window.
/// About what a middle-drag across a third of the screen does in a frame, and
/// wide enough that the sweep brings ground into view that `steady` never saw.
pub const TURN_DEG_PER_FRAME: f32 = 2.0;

// ---------------------------------------------------------------------------
// The manifest
// ---------------------------------------------------------------------------

/// `docs/perf.toml`. Unknown keys are a hard error, for the reason the shot
/// manifest gives: a typo'd key that silently does nothing turns into a baseline
/// nobody can reproduce.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PerfManifest {
    #[serde(default)]
    pub size: Option<String>,
    #[serde(default)]
    pub settle: Option<f32>,
    #[serde(default)]
    pub wait: Option<f32>,
    /// Pinned for the reason `docs/shots.toml` pins it: a scripted run never sees
    /// the server handshake that supplies a tile source, and the built-in fallback
    /// is the *raster* basemap, on which a ground scenario measures a different
    /// scene from the one it names.
    #[serde(default)]
    pub tile_url: Option<String>,
    #[serde(default)]
    pub steady_frames: Option<u32>,
    #[serde(default, rename = "scenario")]
    pub scenarios: Vec<PerfScenario>,
    /// Expanded into `scenarios` by [`PerfManifest::load`]; see the module doc.
    #[serde(default, rename = "patrol")]
    pub patrols: Vec<PerfPatrol>,
}

/// Most stops one patrol may expand to. A `step_m` a decimal place too small
/// over a city is tens of thousands of stops — days of flying — and the run
/// should refuse that at startup rather than an hour in.
pub const MAX_PATROL_STOPS: usize = 1000;

/// A grid of stops over an area. See the module doc.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PerfPatrol {
    /// Prefix of every stop's name, and what `--only` takes to fly them all.
    pub name: String,
    pub description: Option<String>,
    /// `[south, west, north, east]` in degrees. The grid starts at the
    /// south-west corner and stops short of the far edges by less than a step.
    pub area: [f64; 4],
    /// Grid spacing, in metres on the ground.
    pub step_m: f64,
    /// Every grid point is visited at each of these, in metres above the ground.
    pub altitudes: Vec<f64>,
    /// `[pitch, yaw]` per stop, as a scenario's `look`. Default straight down.
    #[serde(default)]
    pub looks: Option<Vec<[f32; 2]>>,
    #[serde(default)]
    pub mode: Option<String>,
    /// Required, for the reason [`PerfScenario::time`] gives.
    #[serde(default)]
    pub time: Option<f32>,
    #[serde(default)]
    pub fog: Option<f32>,
    #[serde(default)]
    pub clouds: Option<f32>,
    #[serde(default)]
    pub rain: Option<f32>,
    #[serde(default)]
    pub steady_frames: Option<u32>,
}

impl PerfPatrol {
    /// The stops, in the order they are flown: altitude by altitude, each grid
    /// row walked in the opposite direction to the last, so every move is one
    /// step and the streamers keep most of what they hold.
    pub fn expand(&self) -> Result<Vec<PerfScenario>, String> {
        let who = &self.name;
        let [south, west, north, east] = self.area;
        if !(south < north && west < east) {
            return Err(format!(
                "patrol {who:?}: area wants [south, west, north, east], got {:?}",
                self.area
            ));
        }
        if !(self.step_m > 0.0) {
            return Err(format!("patrol {who:?}: step_m must be above zero"));
        }
        if self.altitudes.is_empty() {
            return Err(format!("patrol {who:?}: no altitudes"));
        }
        if self.time.is_none() {
            return Err(format!(
                "patrol {who:?} leaves the time of day to the clock, so its shadow \
                 cascades differ stop to stop and the ranking is noise; set `time`"
            ));
        }
        let looks = self.looks.clone().unwrap_or_else(|| vec![[-90.0, 0.0]]);
        if looks.is_empty() {
            return Err(format!("patrol {who:?}: `looks` is empty"));
        }
        let (per_lat, per_lng) = cartopolis_core::geo::degree_scale_m((south + north) / 2.0);
        let (dlat, dlng) = (self.step_m / per_lat, self.step_m / per_lng);
        // A millionth of a step of slack, so an area drawn as an exact multiple
        // of the step keeps its far edge instead of losing it to rounding.
        let rows = ((north - south) / dlat + 1e-6).floor() as usize + 1;
        let cols = ((east - west) / dlng + 1e-6).floor() as usize + 1;
        let total = rows * cols * self.altitudes.len() * looks.len();
        if total > MAX_PATROL_STOPS {
            return Err(format!(
                "patrol {who:?} expands to {total} stops ({rows}×{cols} points × {} \
                 altitudes × {} looks), over the {MAX_PATROL_STOPS} cap; widen step_m",
                self.altitudes.len(),
                looks.len()
            ));
        }
        let mut stops = Vec::with_capacity(total);
        for &alt in &self.altitudes {
            for row in 0..rows {
                for k in 0..cols {
                    let col = if row % 2 == 0 { k } else { cols - 1 - k };
                    for (i, look) in looks.iter().enumerate() {
                        let mut name = format!("{who}-{alt:.0}m-r{row}c{col}");
                        if looks.len() > 1 {
                            name.push_str(&format!("-l{i}"));
                        }
                        stops.push(PerfScenario {
                            name,
                            description: self.description.clone(),
                            at: [south + row as f64 * dlat, west + col as f64 * dlng, alt],
                            look: Some(*look),
                            mode: self.mode.clone(),
                            time: self.time,
                            fog: self.fog,
                            clouds: self.clouds,
                            rain: self.rain,
                            ui: None,
                            hide_ui: None,
                            steady_frames: self.steady_frames,
                            patrol: Some(who.clone()),
                        });
                    }
                }
            }
        }
        Ok(stops)
    }
}

/// One measured viewpoint.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PerfScenario {
    pub name: String,
    /// Which cost this scenario is here to exercise. Documentation only, and the
    /// most important field in the file: a scenario nobody can justify is one that
    /// gets deleted the first time it flakes.
    #[serde(default)]
    pub description: Option<String>,
    pub at: [f64; 3],
    #[serde(default)]
    pub look: Option<[f32; 2]>,
    #[serde(default)]
    pub mode: Option<String>,
    /// **Pin the time of day and the weather in every scenario.** The day/night
    /// cycle moves the shadow cascades and the cloud cover moves the fill, so a
    /// scenario that leaves them to the clock measures a different picture each run
    /// and its numbers are noise. A test enforces it.
    #[serde(default)]
    pub time: Option<f32>,
    #[serde(default)]
    pub fog: Option<f32>,
    #[serde(default)]
    pub clouds: Option<f32>,
    #[serde(default)]
    pub rain: Option<f32>,
    #[serde(default)]
    pub ui: Option<Vec<String>>,
    #[serde(default)]
    pub hide_ui: Option<bool>,
    #[serde(default)]
    pub steady_frames: Option<u32>,
    /// The patrol this stop was expanded from; never written in a manifest.
    #[serde(skip)]
    pub patrol: Option<String>,
}

impl PerfManifest {
    pub fn load(path: &Path) -> Result<Self, String> {
        let text = std::fs::read_to_string(path).map_err(|e| format!("{}: {e}", path.display()))?;
        let mut manifest: PerfManifest =
            toml::from_str(&text).map_err(|e| format!("{}: {e}", path.display()))?;
        for patrol in &manifest.patrols {
            if manifest
                .patrols
                .iter()
                .filter(|p| p.name == patrol.name)
                .count()
                > 1
            {
                return Err(format!(
                    "{}: duplicate patrol {:?}",
                    path.display(),
                    patrol.name
                ));
            }
            let stops = patrol
                .expand()
                .map_err(|e| format!("{}: {e}", path.display()))?;
            manifest.scenarios.extend(stops);
        }
        if manifest.scenarios.is_empty() {
            return Err(format!(
                "{}: no [[scenario]] or [[patrol]] entries",
                path.display()
            ));
        }
        for (i, s) in manifest.scenarios.iter().enumerate() {
            if manifest.scenarios[..i].iter().any(|o| o.name == s.name) {
                return Err(format!(
                    "{}: duplicate scenario {:?}",
                    path.display(),
                    s.name
                ));
            }
        }
        Ok(manifest)
    }

    /// The scenarios `--only` names, or all of them. A patrol's name selects
    /// every one of its stops.
    ///
    /// An unknown name is an error rather than an empty run: `--only orbti` is a
    /// typo, and a suite that answers a typo with "0 scenarios, nothing wrong" is
    /// reporting green for the wrong reason.
    pub fn select(&self, only: &[String]) -> Result<Vec<&PerfScenario>, String> {
        let named =
            |s: &PerfScenario, name: &str| s.name == name || s.patrol.as_deref() == Some(name);
        if only.is_empty() {
            return Ok(self.scenarios.iter().collect());
        }
        let wanted: Vec<&str> = only
            .iter()
            .flat_map(|s| s.split(','))
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .collect();
        for name in &wanted {
            if !self.scenarios.iter().any(|s| named(s, name)) {
                // A patrol's stops would bury the list, so name the patrol.
                let mut known: Vec<&str> = self
                    .scenarios
                    .iter()
                    .filter(|s| s.patrol.is_none())
                    .map(|s| s.name.as_str())
                    .collect();
                known.extend(self.patrols.iter().map(|p| p.name.as_str()));
                return Err(format!(
                    "no scenario or patrol named {name:?}; the manifest has {}",
                    known.join(", ")
                ));
            }
        }
        Ok(self
            .scenarios
            .iter()
            .filter(|s| wanted.iter().any(|name| named(s, name)))
            .collect())
    }
}

// ---------------------------------------------------------------------------
// The run
// ---------------------------------------------------------------------------

/// Everything the run needs that is not per-scenario state.
#[derive(Resource, Clone, Debug)]
pub struct PerfRun {
    /// Scenario names in visiting order, so a window can be labelled by index.
    pub names: Vec<String>,
    /// `steady` window length per scenario, parallel to `names`.
    pub steady_frames: Vec<u32>,
    pub out: PathBuf,
    pub size: String,
    /// How many times each scenario is flown. `names` and `steady_frames` are
    /// already expanded by it, round-robin; this is kept for the report.
    pub repeat: u32,
    /// Every patrol stop selected, once each, in manifest order — what the
    /// ranking needs to say where a stop is.
    pub stops: Vec<PatrolStop>,
}

/// Where one patrol stop is, for the ranking.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct PatrolStop {
    pub patrol: String,
    pub scenario: String,
    /// `[lat, lng, metres above the ground]`, as the manifest's `at`.
    pub at: [f64; 3],
    /// `[pitch, yaw]`, as the manifest's `look`.
    pub look: [f32; 2],
}

/// Which window is open.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
enum Phase {
    #[default]
    Between,
    Load,
    /// Settle verdict reached, shutter not yet fired. Nothing is measured here on
    /// purpose: what happens between those two points is the harness quiescing the
    /// worker pool and waiting on a GPU readback, neither of which is the client's
    /// cost.
    Quiescing,
    Steady,
    /// The camera yawing on the spot; see the module doc.
    Turn,
}

/// Counters a window is measured against, taken at its start.
#[derive(Clone, Debug, Default)]
struct Base {
    upload_bytes: u64,
    /// `(stalls, stalled_ms)` at the window's start — a delta, like the rest.
    stalls: (u64, u64),
    workers: Vec<(&'static str, crate::platform::workers::stats::Totals)>,
    mesh_added: u64,
    materials_modified: u64,
    frames: u32,
    wall_s: f32,
    /// The longest single frame in the window, seconds.
    frame_max_s: f32,
    /// Per-system CPU totals at the window's start, so the window's own cost is a
    /// subtraction. Exactly the same shape as `workers` above and read the same
    /// way — see `systems::dev::system_timings` for why this is counted in process
    /// rather than written to a trace file.
    #[cfg(feature = "perf")]
    systems: std::collections::HashMap<Box<str>, super::system_timings::Cost>,
}

/// Live state of the suite. Present only under `--perf`.
#[derive(Resource, Default)]
pub struct PerfProgress {
    phase: Phase,
    left: u32,
    base: Base,
    samples: Vec<Window>,
    index: usize,
    written: bool,
    /// Smallest and largest `fragment_shader_invocations` seen per pass this
    /// window. See [`GpuPass::frag_min`].
    frag_seen: std::collections::HashMap<String, (f64, f64, u32)>,
    /// Whether the scenario's settle verdict was an actual settle or the `wait`
    /// cap running out. Recorded when the `load` window closes and carried into
    /// the `steady` window, which inherits the same doubt.
    settled: bool,
}

/// Running count of mesh asset events, for [`Window::mesh_added_per_frame`].
#[derive(Resource, Default, Debug, Clone, Copy)]
pub struct MeshChurn {
    pub added: u64,
    pub modified: u64,
    /// See [`count_material_churn`].
    pub materials_modified: u64,
}

/// Count `Modified` events on the material families that exist per cell or per
/// tile — hundreds of instances each — into [`MeshChurn::materials_modified`].
///
/// **A modified material is re-prepared on the render thread**: a new uniform
/// buffer and a new bind group, which on Metal is a kernel round trip apiece. That
/// is cheap for one material and is a frame for all of them, and "all of them" is
/// what a system writing a shared value into every instance produces —
/// `Assets::get_mut` marks the asset changed on the write whether or not the value
/// moved. Three systems did exactly that on every frame of a camera turn and cost
/// 12–17 ms of render thread between them (2026-10-16), invisible to every other
/// column here. A still window should read ~0; a `turn` window a few per frame,
/// for the tiles that genuinely arrive.
pub fn count_material_churn(
    mut ground: MessageReader<AssetEvent<crate::systems::ground_detail::GroundMaterial>>,
    mut roads: MessageReader<AssetEvent<crate::systems::road_depth::RoadMaterial>>,
    mut globe: MessageReader<AssetEvent<cartopolis_materials::globe_ocean::GlobeSurfaceMaterial>>,
    mut standard: MessageReader<AssetEvent<StandardMaterial>>,
    mut churn: ResMut<MeshChurn>,
) {
    fn modified<A: Asset>(events: &mut MessageReader<AssetEvent<A>>) -> u64 {
        events
            .read()
            .filter(|e| matches!(e, AssetEvent::Modified { .. }))
            .count() as u64
    }
    churn.materials_modified += modified(&mut ground)
        + modified(&mut roads)
        + modified(&mut globe)
        + modified(&mut standard);
}

/// Count mesh asset events.
///
/// A `MessageReader` has its own cursor, so this does not consume the events any
/// other reader — `extract_render_asset` above all — depends on.
pub fn count_mesh_churn(mut events: MessageReader<AssetEvent<Mesh>>, mut churn: ResMut<MeshChurn>) {
    for event in events.read() {
        match event {
            AssetEvent::Added { .. } => churn.added += 1,
            AssetEvent::Modified { .. } => churn.modified += 1,
            _ => {}
        }
    }
}

// ---------------------------------------------------------------------------
// The report
// ---------------------------------------------------------------------------

/// How many systems a window reports. The tail is a long list of sub-microsecond
/// rows that nobody reads and that would dominate the report's size.
///
/// Gated with its only consumer: without `perf` there is no counter, so an
/// ungated constant is dead code and this crate denies that.
#[cfg(feature = "perf")]
pub const TOP_SYSTEMS: usize = 12;

/// One system's CPU cost over a window.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct SystemCost {
    /// The system's full type path, off bevy's `info_span!("system", name = …)`.
    /// The span itself is called `system` for every one of them, so reading the
    /// span name gives a single row covering millions of calls.
    pub name: String,
    pub ms: f64,
    pub calls: u64,
    /// `ms` over the window's frame count — the per-frame cost, which is what a
    /// steady window is asking about.
    pub ms_per_frame: f64,
}

/// One render pass as `RenderDiagnosticsPlugin` reports it.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct GpuPass {
    /// The diagnostic path minus its measurement, e.g. `render/main_opaque_pass_3d`.
    pub path: String,
    /// A real GPU timestamp on Vulkan and DX12, and CPU time wearing a GPU label
    /// everywhere else — including on this container, where "the GPU" is llvmpipe.
    /// Structure yes, magnitude no.
    pub gpu_ms: Option<f64>,
    pub cpu_ms: Option<f64>,
    /// **The column that transfers**, because it is a count rather than a time.
    pub fragment_invocations: Option<f64>,
    /// Smallest and largest seen across the window, and how many frames were
    /// sampled.
    ///
    /// **A point sample of this is not reproducible and will mislead you.**
    /// Measured: three runs of one unchanged configuration reported 160 836,
    /// 218 436 and 218 436 — two values differing by exactly one full screen. An
    /// A/B of the sky shells run against single samples therefore measured that
    /// flip and not the shells, and read as a clean result. So the window reports
    /// the spread, and `frag_min != frag_max` says the scene has more than one
    /// steady state and that differencing two runs is meaningless until that is
    /// understood.
    pub frag_min: Option<f64>,
    pub frag_max: Option<f64>,
    pub frag_samples: u32,
}

/// One measured window.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct Window {
    pub scenario: String,
    /// `load`, `steady` or `turn`.
    pub phase: String,
    pub frames: u32,
    pub wall_s: f32,

    // ---- straight from Bevy's diagnostics -------------------------------
    /// `FrameTimeDiagnosticsPlugin`. Reported, never gated: a frame time here is
    /// a property of a shared eight-core box with a software rasteriser.
    pub fps: Option<f64>,
    pub frame_time_ms: Option<f64>,
    /// The window's own mean and worst frame, from its wall clock and frame
    /// count. `frame_time_ms` above is Bevy's exponentially smoothed reading at
    /// the moment the window closed, which is dominated by its last few frames —
    /// fine for a still view and wrong for a `turn`, whose cost is in the hitches.
    #[serde(default)]
    pub frame_ms_mean: f64,
    #[serde(default)]
    pub frame_ms_max: f64,
    /// `SystemInformationDiagnosticsPlugin`. **Both are percentages** — CPU of
    /// one core, memory of total system memory. Bevy's plugin documents them that
    /// way and an earlier draft here labelled the second one `rss_mb`, which read
    /// as a 2 MB process. Its own docs also warn the sample is not taken every
    /// frame and may be stale, so read it as a trend rather than as a reading.
    pub process_cpu_percent: Option<f64>,
    pub process_mem_percent: Option<f64>,
    /// `EntityCountDiagnosticsPlugin`.
    pub entity_count: Option<f64>,
    /// `MeshAllocatorDiagnosticPlugin`: what the engine actually holds in mesh
    /// slabs, as opposed to what this client believes it uploaded.
    pub mesh_slabs: Option<f64>,
    pub mesh_slab_mb: Option<f64>,
    /// Per render pass. Empty without the `perf` feature.
    pub gpu_passes: Vec<GpuPass>,
    /// The dearest systems *in this window*, dearest first. Empty without the
    /// `perf` feature.
    ///
    /// **Per window, not per run, and that distinction is the whole point.** A
    /// whole-run total cannot separate a streaming cost from a frame cost:
    /// `update_map_surfaces` is 170 ms/frame while tiles arrive and absent from
    /// the top twelve once the scene settles, and a single number covering both
    /// reads as a catastrophic frame cost that does not exist.
    ///
    /// CPU time summed across the pool's threads, so it is **not** a share of the
    /// frame — two systems each billing 4 ms may have run at the same moment on
    /// different cores. Truncated to the dearest `TOP_SYSTEMS`; a full table is a
    /// trace file's job and this is deliberately not one.
    pub systems: Vec<SystemCost>,

    // ---- this client's own -----------------------------------------------
    /// GPU upload traffic over the window, in kB.
    pub upload_kb: u64,
    /// Tile-worker jobs finished in the window.
    pub worker_jobs: u64,
    /// Time spent inside those jobs, and time they sat queued before starting,
    /// summed over every stream, in milliseconds.
    pub worker_busy_ms: u64,
    pub worker_wait_ms: u64,
    /// How wide the tile worker pool is. Every number above is a sum *over
    /// threads* and is uninterpretable without it — 32 s of busy is saturation
    /// on one thread and near-idleness on eight.
    pub worker_threads: usize,
    /// Main-loop stalls in this window, and how many milliseconds of it they
    /// account for (`platform::watchdog`).
    ///
    /// **Read this beside `worker_utilisation`, because one explains the other.**
    /// A stalled main loop dispatches nothing, so every streamer's queue stops
    /// being refilled and the worker pool drains — the utilisation column then
    /// reports starvation whose cause is not the network at all.
    ///
    /// Measured 2026-09-22 on `deck-1`, ground level over Groningen: 22 stalls
    /// of ~2.9 s in a 101 s fill, strictly alternating with the "large GPU
    /// upload queued in one frame" warning, with the main thread parked in the
    /// kernel's `__drm_exec_lock_obj` — i.e. blocked in the graphics driver on
    /// buffer locks, not computing a large frame. Utilisation over that window
    /// reads 0.16 against wall clock and 0.44 against the time the loop was
    /// alive.
    pub stalls: u64,
    pub stalled_ms: u64,
    /// Whether the scenario actually settled, or the `wait` cap ran out.
    ///
    /// **Read this before any rate in the window.** Everything here divided by
    /// `wall_s` — `fps`, `mesh_added_per_frame`, `worker_utilisation` — is
    /// measured against a window that a timeout stretches to the cap. Measured
    /// 2026-09-22 on `deck-1`: all four scenarios timed out at 90 s blocked on
    /// `terrain`, against a city fill that settles in ~31 s when the height
    /// service answers, so utilisation read roughly a third of its true value
    /// and looked like a finding. A window with this `false` is a report about
    /// a world still being built.
    pub settled: bool,
    /// Fraction of the pool's available thread-time actually spent working, over
    /// this window: `busy / (threads × wall_s)`.
    ///
    /// **The column to read first on a `load` window.** A large `worker_wait_ms`
    /// has two completely different causes that no other number here separates:
    /// jobs queued behind a saturated pool, and jobs queued waiting for their
    /// bytes to arrive. Near 1.0 the cores are the wall, and only cheaper work
    /// helps. Low, with a large wait, the pool is starved and the fill is bound
    /// by fetches or dispatch order — where making the work cheaper buys nothing
    /// at all.
    ///
    /// Measured 2026-09-22, `deck-1`, 8 threads, handset rung at 3 km over
    /// Groningen: **0.13**. The fill was waiting, not computing, which is the
    /// opposite of what the per-cell costs on their own suggest.
    pub worker_utilisation: f32,
    /// Mesh assets arriving per frame. On a settled window this should be ~0;
    /// whatever it is, is what the mesh allocator is being paid to process.
    pub mesh_added_per_frame: f64,
    /// Ground, road, globe-tile and standard materials marked modified per frame,
    /// each one a re-prepare on the render thread. See [`count_material_churn`].
    #[serde(default)]
    pub material_modified_per_frame: f64,
    /// What the streamed content occupies, in kB, split by what it is made of.
    ///
    /// The split is what makes it actionable: `tile_px` moves only the textures,
    /// while `radius_bias` moves both. Measured over Groningen, geometry is the
    /// majority and on the handset rung it is 79 % of the total.
    pub resident_kb: usize,
    pub resident_image_kb: usize,
    pub resident_mesh_kb: usize,
    pub draws: usize,
    pub tris_total: usize,
    /// Whether a `steady` window was genuinely quiet — nothing uploading, no
    /// worker jobs, no mesh churn. See the module doc.
    pub quiet: bool,
}

/// What the run was, so a report says what it is comparable with.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ReportMeta {
    pub adapter: String,
    /// Every timing column in a report with this set is a property of a CPU.
    pub software_renderer: bool,
    pub quality: String,
    pub size: String,
    /// Whether the deep instruments were compiled in.
    pub perf_feature: bool,
    /// Whether this was a `debug_assertions` build — i.e. **not** what ships.
    /// This workspace's dev profile builds our crates at `opt-level = 1` and
    /// debug assertions switch on `BigSpaceValidationPlugin`, which release
    /// compiles out entirely.
    pub debug_assertions: bool,
    /// Flights per scenario (`--repeat`).
    pub repeat: u32,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Report {
    pub meta: ReportMeta,
    /// One per `(scenario, phase)`, aggregated over the repeats. The individual
    /// flights are in the `perf window` log lines, not here.
    pub windows: Vec<WindowSummary>,
    /// One per patrol flown; see the module doc.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub patrols: Vec<PatrolReport>,
}

// ---------------------------------------------------------------------------
// Aggregating repeats
// ---------------------------------------------------------------------------

/// One numeric column over the repeats of one window.
#[derive(Clone, Copy, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Stat {
    /// Flights that carried a value for *this column* — fewer than the window's
    /// `runs` when a diagnostic was absent in some of them. Absent is not zero.
    pub runs: u32,
    pub min: f64,
    /// The middle sample; for an even count, the mean of the two middle ones —
    /// the ordinary definition, so a reader recomputing it by hand from the
    /// `perf window` lines gets the same number.
    pub median: f64,
    pub max: f64,
}

impl Stat {
    /// `None` when no flight carried a value.
    #[must_use]
    pub fn of(values: impl IntoIterator<Item = f64>) -> Option<Stat> {
        let mut v: Vec<f64> = values.into_iter().collect();
        if v.is_empty() {
            return None;
        }
        v.sort_by(f64::total_cmp);
        let n = v.len();
        let median = if n % 2 == 1 {
            v[n / 2]
        } else {
            (v[n / 2 - 1] + v[n / 2]) / 2.0
        };
        Some(Stat {
            runs: n as u32,
            min: v[0],
            median,
            max: v[n - 1],
        })
    }
}

/// One render pass over the repeats of one window.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct GpuPassSummary {
    pub path: String,
    /// Flights in which this pass reported at all.
    pub runs: u32,
    pub gpu_ms: Option<Stat>,
    pub cpu_ms: Option<Stat>,
    pub fragment_invocations: Option<Stat>,
    /// Min of the per-flight mins, max of the per-flight maxes, and every frame
    /// sampled across them. See [`GpuPass::frag_min`].
    pub frag_min: Option<f64>,
    pub frag_max: Option<f64>,
    pub frag_samples: u32,
}

/// One system's per-frame cost over the repeats of one window.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct SystemSummary {
    pub name: String,
    /// Its `runs` is the flights in which it made the window's top list.
    pub ms_per_frame: Stat,
}

/// One `(scenario, phase)` window over every flight of it. The columns are
/// [`Window`]'s, each as a [`Stat`]; see there for what each one means.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct WindowSummary {
    pub scenario: String,
    pub phase: String,
    /// Flights aggregated here.
    pub runs: u32,
    /// How many of them passed [`Window::quiet`].
    pub quiet_runs: u32,
    /// How many of them actually settled rather than running out of `wait`.
    pub settled_runs: u32,
    pub worker_threads: usize,
    pub frames: Stat,
    pub wall_s: Stat,
    pub fps: Option<Stat>,
    pub frame_time_ms: Option<Stat>,
    pub frame_ms_mean: Stat,
    pub frame_ms_max: Stat,
    pub process_cpu_percent: Option<Stat>,
    pub process_mem_percent: Option<Stat>,
    pub entity_count: Option<Stat>,
    pub mesh_slabs: Option<Stat>,
    pub mesh_slab_mb: Option<Stat>,
    pub upload_kb: Stat,
    pub worker_jobs: Stat,
    pub worker_busy_ms: Stat,
    pub worker_wait_ms: Stat,
    pub worker_utilisation: Stat,
    pub stalls: Stat,
    pub stalled_ms: Stat,
    pub mesh_added_per_frame: Stat,
    pub material_modified_per_frame: Stat,
    pub resident_kb: Stat,
    pub resident_image_kb: Stat,
    pub resident_mesh_kb: Stat,
    pub draws: Stat,
    pub tris_total: Stat,
    pub gpu_passes: Vec<GpuPassSummary>,
    /// Dearest first by median.
    pub systems: Vec<SystemSummary>,
}

/// Group flights by `(scenario, phase)` and aggregate each group.
///
/// Output order is first appearance, which for a run is manifest order with
/// `load` before `steady` — the same order a single-flight report has always
/// had.
#[must_use]
pub fn summarise(samples: &[Window]) -> Vec<WindowSummary> {
    let mut groups: Vec<(&str, &str, Vec<&Window>)> = Vec::new();
    for w in samples {
        match groups
            .iter_mut()
            .find(|(s, p, _)| *s == w.scenario && *p == w.phase)
        {
            Some((_, _, g)) => g.push(w),
            None => groups.push((&w.scenario, &w.phase, vec![w])),
        }
    }
    groups
        .into_iter()
        .map(|(scenario, phase, g)| summarise_group(scenario, phase, &g))
        .collect()
}

fn summarise_group(scenario: &str, phase: &str, g: &[&Window]) -> WindowSummary {
    // A group is never empty, so a column every flight carries always has a
    // value; `unwrap_or_default` only satisfies the type.
    let all = |f: fn(&Window) -> f64| Stat::of(g.iter().map(|w| f(w))).unwrap_or_default();
    let some = |f: fn(&Window) -> Option<f64>| Stat::of(g.iter().filter_map(|w| f(w)));

    let mut passes: Vec<(String, Vec<&GpuPass>)> = Vec::new();
    for pass in g.iter().flat_map(|w| &w.gpu_passes) {
        match passes.iter_mut().find(|(p, _)| *p == pass.path) {
            Some((_, v)) => v.push(pass),
            None => passes.push((pass.path.clone(), vec![pass])),
        }
    }
    let mut gpu_passes: Vec<GpuPassSummary> = passes
        .into_iter()
        .map(|(path, v)| GpuPassSummary {
            path,
            runs: v.len() as u32,
            gpu_ms: Stat::of(v.iter().filter_map(|p| p.gpu_ms)),
            cpu_ms: Stat::of(v.iter().filter_map(|p| p.cpu_ms)),
            fragment_invocations: Stat::of(v.iter().filter_map(|p| p.fragment_invocations)),
            frag_min: v.iter().filter_map(|p| p.frag_min).reduce(f64::min),
            frag_max: v.iter().filter_map(|p| p.frag_max).reduce(f64::max),
            frag_samples: v.iter().map(|p| p.frag_samples).sum(),
        })
        .collect();
    // By fill, as `gpu_passes` sorts a single window; no statistics last.
    gpu_passes.sort_by(|a, b| {
        let key = |p: &GpuPassSummary| p.fragment_invocations.map_or(-1.0, |s| s.median);
        key(b).total_cmp(&key(a))
    });

    let mut sys: Vec<(String, Vec<f64>)> = Vec::new();
    for s in g.iter().flat_map(|w| &w.systems) {
        match sys.iter_mut().find(|(n, _)| *n == s.name) {
            Some((_, v)) => v.push(s.ms_per_frame),
            None => sys.push((s.name.clone(), vec![s.ms_per_frame])),
        }
    }
    let mut systems: Vec<SystemSummary> = sys
        .into_iter()
        .filter_map(|(name, v)| {
            Stat::of(v).map(|ms_per_frame| SystemSummary { name, ms_per_frame })
        })
        .collect();
    systems.sort_by(|a, b| b.ms_per_frame.median.total_cmp(&a.ms_per_frame.median));

    WindowSummary {
        scenario: scenario.to_string(),
        phase: phase.to_string(),
        runs: g.len() as u32,
        quiet_runs: g.iter().filter(|w| w.quiet).count() as u32,
        settled_runs: g.iter().filter(|w| w.settled).count() as u32,
        worker_threads: g.iter().map(|w| w.worker_threads).max().unwrap_or(0),
        frames: all(|w| f64::from(w.frames)),
        wall_s: all(|w| f64::from(w.wall_s)),
        fps: some(|w| w.fps),
        frame_time_ms: some(|w| w.frame_time_ms),
        frame_ms_mean: all(|w| w.frame_ms_mean),
        frame_ms_max: all(|w| w.frame_ms_max),
        process_cpu_percent: some(|w| w.process_cpu_percent),
        process_mem_percent: some(|w| w.process_mem_percent),
        entity_count: some(|w| w.entity_count),
        mesh_slabs: some(|w| w.mesh_slabs),
        mesh_slab_mb: some(|w| w.mesh_slab_mb),
        upload_kb: all(|w| w.upload_kb as f64),
        worker_jobs: all(|w| w.worker_jobs as f64),
        worker_busy_ms: all(|w| w.worker_busy_ms as f64),
        worker_wait_ms: all(|w| w.worker_wait_ms as f64),
        worker_utilisation: all(|w| f64::from(w.worker_utilisation)),
        stalls: all(|w| w.stalls as f64),
        stalled_ms: all(|w| w.stalled_ms as f64),
        mesh_added_per_frame: all(|w| w.mesh_added_per_frame),
        material_modified_per_frame: all(|w| w.material_modified_per_frame),
        resident_kb: all(|w| w.resident_kb as f64),
        resident_image_kb: all(|w| w.resident_image_kb as f64),
        resident_mesh_kb: all(|w| w.resident_mesh_kb as f64),
        draws: all(|w| w.draws as f64),
        tris_total: all(|w| w.tris_total as f64),
        gpu_passes,
        systems,
    }
}

// ---------------------------------------------------------------------------
// Ranking a patrol
// ---------------------------------------------------------------------------

/// Stops listed per column in a [`PatrolReport`].
pub const PATROL_DEAREST: usize = 5;

/// One patrol's stops, ranked column by column.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct PatrolReport {
    pub patrol: String,
    pub stops: usize,
    pub columns: Vec<PatrolColumn>,
}

/// One column across a patrol: what is ordinary there, and the dearest stops.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct PatrolColumn {
    pub phase: String,
    pub column: String,
    /// The median of the stops' medians — what this column reads at an
    /// ordinary stop of this patrol.
    pub patrol_median: f64,
    /// Dearest first; stops reading zero are left out.
    pub dearest: Vec<Hotspot>,
}

/// One stop that stood out.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Hotspot {
    pub scenario: String,
    pub at: [f64; 3],
    pub look: [f32; 2],
    /// The stop's own reading over its flights. A range wider than the gap to
    /// the next stop down means the order between them is not a finding.
    pub value: Stat,
    /// `value.median` over [`PatrolColumn::patrol_median`]. Absent when that
    /// median is zero: a column that is zero almost everywhere (material churn
    /// on a still view) is read by its value, not by a ratio to nothing.
    pub ratio: Option<f64>,
    /// Whether every flight of this stop settled and, for a `steady` window,
    /// went quiet. A stop that did not is a streaming measurement, and may rank
    /// high for that reason alone.
    pub settled: bool,
    pub quiet: bool,
}

/// A column a patrol is ranked on.
struct RankedColumn {
    phase: &'static str,
    column: &'static str,
    read: fn(&WindowSummary) -> Option<Stat>,
}

/// The counts lead, because they are the same arithmetic on a software
/// rasteriser as on a handset (`docs/notes/performance-suite.md`). The frame
/// times are ranked too, being what a person feels, but say something only
/// when `meta.software_renderer` is false.
const RANKED: &[RankedColumn] = &[
    RankedColumn {
        phase: "steady",
        column: "draws",
        read: |w| Some(w.draws),
    },
    RankedColumn {
        phase: "steady",
        column: "tris_total",
        read: |w| Some(w.tris_total),
    },
    RankedColumn {
        phase: "steady",
        column: "resident_kb",
        read: |w| Some(w.resident_kb),
    },
    RankedColumn {
        phase: "steady",
        column: "fill",
        read: fill,
    },
    RankedColumn {
        phase: "steady",
        column: "material_modified_per_frame",
        read: |w| Some(w.material_modified_per_frame),
    },
    RankedColumn {
        phase: "steady",
        column: "frame_ms_mean",
        read: |w| Some(w.frame_ms_mean),
    },
    RankedColumn {
        phase: "turn",
        column: "frame_ms_max",
        read: |w| Some(w.frame_ms_max),
    },
    RankedColumn {
        phase: "turn",
        column: "material_modified_per_frame",
        read: |w| Some(w.material_modified_per_frame),
    },
    RankedColumn {
        phase: "turn",
        column: "mesh_added_per_frame",
        read: |w| Some(w.mesh_added_per_frame),
    },
    RankedColumn {
        phase: "turn",
        column: "upload_kb",
        read: |w| Some(w.upload_kb),
    },
    RankedColumn {
        phase: "turn",
        column: "stalls",
        read: |w| Some(w.stalls),
    },
];

/// Fragment-shader invocations over every pass that reports them: the
/// window's whole fill, summed pass by pass (so its median is the sum of the
/// passes' medians). Absent without the `perf` feature.
fn fill(w: &WindowSummary) -> Option<Stat> {
    let passes: Vec<Stat> = w
        .gpu_passes
        .iter()
        .filter_map(|p| p.fragment_invocations)
        .collect();
    (!passes.is_empty()).then(|| Stat {
        runs: passes.iter().map(|s| s.runs).min().unwrap_or(0),
        min: passes.iter().map(|s| s.min).sum(),
        median: passes.iter().map(|s| s.median).sum(),
        max: passes.iter().map(|s| s.max).sum(),
    })
}

/// Rank each patrol's stops against each other, one patrol at a time — stops
/// from two patrols were picked for different reasons and are not comparable.
#[must_use]
pub fn rank_patrols(stops: &[PatrolStop], windows: &[WindowSummary]) -> Vec<PatrolReport> {
    let mut patrols: Vec<&str> = Vec::new();
    for stop in stops {
        if !patrols.contains(&stop.patrol.as_str()) {
            patrols.push(&stop.patrol);
        }
    }
    patrols
        .into_iter()
        .map(|patrol| {
            let mine: Vec<&PatrolStop> = stops.iter().filter(|s| s.patrol == patrol).collect();
            let columns = RANKED
                .iter()
                .filter_map(|c| {
                    let mut rows: Vec<(&PatrolStop, &WindowSummary, Stat)> = mine
                        .iter()
                        .filter_map(|stop| {
                            let w = windows
                                .iter()
                                .find(|w| w.scenario == stop.scenario && w.phase == c.phase)?;
                            Some((*stop, w, (c.read)(w)?))
                        })
                        .collect();
                    let patrol_median = Stat::of(rows.iter().map(|r| r.2.median))?.median;
                    rows.sort_by(|a, b| b.2.median.total_cmp(&a.2.median));
                    let dearest = rows
                        .into_iter()
                        .filter(|r| r.2.median > 0.0)
                        .take(PATROL_DEAREST)
                        .map(|(stop, w, value)| Hotspot {
                            scenario: stop.scenario.clone(),
                            at: stop.at,
                            look: stop.look,
                            value,
                            ratio: (patrol_median > 0.0).then(|| value.median / patrol_median),
                            settled: w.settled_runs == w.runs,
                            quiet: w.quiet_runs == w.runs,
                        })
                        .collect();
                    Some(PatrolColumn {
                        phase: c.phase.to_string(),
                        column: c.column.to_string(),
                        patrol_median,
                        dearest,
                    })
                })
                .collect();
            PatrolReport {
                patrol: patrol.to_string(),
                stops: mine.len(),
                columns,
            }
        })
        .collect()
}

/// The top of each ranked column as log lines, so a shell learns where to look
/// without opening the JSON.
fn log_patrol(p: &PatrolReport) {
    for c in &p.columns {
        for h in c.dearest.iter().take(3) {
            tracing::info!(
                patrol = %p.patrol,
                phase = %c.phase,
                column = %c.column,
                scenario = %h.scenario,
                value = format!("{:.1} ({:.1}..{:.1})", h.value.median, h.value.min, h.value.max),
                ratio = h.ratio.map_or_else(|| "-".into(), |r| format!("{r:.2}")),
                patrol_median = format!("{:.1}", c.patrol_median),
                settled = h.settled,
                quiet = h.quiet,
                at = format!("{:.5},{:.5},{:.0}", h.at[0], h.at[1], h.at[2]),
                look = format!("{:.0},{:.0}", h.look[0], h.look[1]),
                "perf patrol"
            );
        }
    }
}

// ---------------------------------------------------------------------------
// Reading Bevy's diagnostics
// ---------------------------------------------------------------------------

/// One diagnostic by path, smoothed.
fn diag(store: &DiagnosticsStore, path: &str) -> Option<f64> {
    store
        .iter()
        .find(|d| d.path().as_str() == path)
        .and_then(bevy::diagnostic::Diagnostic::smoothed)
}

/// Every render pass in the store, recovered by stripping the trailing
/// measurement from each path and grouping.
#[must_use]
pub fn gpu_passes(store: &DiagnosticsStore) -> Vec<GpuPass> {
    let mut out: Vec<GpuPass> = Vec::new();
    for diagnostic in store.iter() {
        let Some((path, measurement)) = diagnostic.path().as_str().rsplit_once('/') else {
            continue;
        };
        if !matches!(
            measurement,
            "elapsed_gpu" | "elapsed_cpu" | "fragment_shader_invocations"
        ) {
            continue;
        }
        let Some(value) = diagnostic.smoothed() else {
            continue;
        };
        let row = match out.iter_mut().find(|p| p.path == path) {
            Some(row) => row,
            None => {
                out.push(GpuPass {
                    path: path.to_string(),
                    gpu_ms: None,
                    cpu_ms: None,
                    fragment_invocations: None,
                    frag_min: None,
                    frag_max: None,
                    frag_samples: 0,
                });
                out.last_mut().expect("just pushed")
            }
        };
        match measurement {
            "elapsed_gpu" => row.gpu_ms = Some(value),
            "elapsed_cpu" => row.cpu_ms = Some(value),
            _ => row.fragment_invocations = Some(value),
        }
    }
    // By fill, because that is the column that transfers. Passes with no
    // statistics fall to the end rather than being dropped.
    out.sort_by(|a, b| {
        b.fragment_invocations
            .unwrap_or(-1.0)
            .total_cmp(&a.fragment_invocations.unwrap_or(-1.0))
    });
    out
}

// ---------------------------------------------------------------------------
// Building the run
// ---------------------------------------------------------------------------

/// Turn a perf manifest into the capture harness that will fly it, plus the
/// suite's own state.
///
/// The suite rides the existing `--shot` path rather than growing a second one:
/// teleporting, waiting for the streams to go quiet, holding the camera mode,
/// pinning the weather and quiescing the worker pool are all solved there. What
/// this adds is *when to measure*.
pub fn build(
    manifest_path: &Path,
    only: &[String],
    size_override: Option<&str>,
    steady_frames_override: Option<u32>,
    repeat: u32,
    out: PathBuf,
) -> Result<(ShotHarness, PerfRun), String> {
    use crate::systems::shot_harness::{parse_size, DEFAULT_SETTLE, DEFAULT_WAIT};

    if repeat == 0 {
        return Err("--repeat wants at least 1".into());
    }
    let manifest = PerfManifest::load(manifest_path)?;
    let selected = manifest.select(only)?;
    let default_frames = manifest.steady_frames.unwrap_or(DEFAULT_STEADY_FRAMES);

    // Round-robin, `a b c a b c`: see the module doc for why not `a a b b`.
    let flights: Vec<(u32, &PerfScenario)> = (1..=repeat)
        .flat_map(|r| selected.iter().map(move |s| (r, *s)))
        .collect();

    let shots: Vec<ShotSpec> = flights
        .iter()
        .map(|&(r, s)| {
            // A single flight keeps the name it has always had; repeats each get
            // their own, since a picture per flight is what explains a spread
            // that turns out to be "a tile had not arrived that time".
            let png = if repeat == 1 {
                format!("perf-{}.png", s.name)
            } else {
                format!("perf-{}-r{r}.png", s.name)
            };
            let look = s.look.unwrap_or([-90.0, 0.0]);
            ShotSpec {
                lat: s.at[0],
                lng: s.at[1],
                alt: s.at[2],
                pitch_deg: look[0],
                yaw_deg: look[1],
                name: Some(s.name.clone()),
                look_at: None,
                description: s.description.clone(),
                // Every scenario still writes its PNG — a number nobody can see
                // the scene behind is how "the avatar isn't rendering" cost a year
                // — but beside the report rather than in `docs/img`, which belongs
                // to `cargo shots`.
                out: Some(std::env::temp_dir().join(png)),
                mode: s.mode.clone(),
                // Chrome is part of the cost: egui is a full-resolution pass over
                // the swapchain every frame, so a suite that hid the UI would
                // measure a client nobody runs.
                hide_ui: s.hide_ui.or(Some(false)),
                ui: s.ui.clone(),
                // The suite's patience is manifest-wide: every scenario is the
                // same kind of measurement, and one that settled differently
                // would not be comparable with the rest.
                settle: None,
                wait: None,
                // Every scenario is flown to; the suite measures what a teleport
                // costs, so capturing in place would measure nothing.
                in_place: false,
                env: ShotEnv {
                    time: s.time,
                    fog: s.fog,
                    clouds: s.clouds,
                    rain: s.rain,
                },
                ..ShotSpec::default()
            }
        })
        .collect();

    let size = size_override
        .map(str::to_owned)
        .or_else(|| manifest.size.clone())
        .unwrap_or_else(|| "640x360".to_string());
    let parsed = parse_size(&size).ok_or_else(|| format!("size wants WxH, got {size:?}"))?;

    let harness = ShotHarness {
        path: manifest_path.with_extension("png"),
        shots,
        size: parsed,
        settle: manifest.settle.unwrap_or(DEFAULT_SETTLE),
        wait: manifest.wait.unwrap_or(DEFAULT_WAIT),
        hide_ui: false,
        ui: Vec::new(),
        mode: "free".to_string(),
        tile_url: manifest.tile_url.clone(),
        csv: None,
        dump_state: None,
        expect: Vec::new(),
        flicker: false,
        aerial_reference: false,
        video_fps: None,
        live: false,
        windowed: false,
        clip: None,
        clip_frames: 0,
    };
    let run = PerfRun {
        names: flights.iter().map(|(_, s)| s.name.clone()).collect(),
        steady_frames: flights
            .iter()
            .map(|(_, s)| {
                steady_frames_override
                    .or(s.steady_frames)
                    .unwrap_or(default_frames)
            })
            .collect(),
        out,
        size: format!("{}x{}", parsed.x, parsed.y),
        repeat,
        stops: selected
            .iter()
            .filter_map(|s| {
                Some(PatrolStop {
                    patrol: s.patrol.clone()?,
                    scenario: s.name.clone(),
                    at: s.at,
                    look: s.look.unwrap_or([-90.0, 0.0]),
                })
            })
            .collect(),
    };
    Ok((harness, run))
}

// ---------------------------------------------------------------------------
// The measuring system
// ---------------------------------------------------------------------------

/// Everything the window boundaries read, in one parameter.
#[derive(bevy::ecs::system::SystemParam)]
pub struct PerfReadouts<'w, 's> {
    real: Res<'w, Time<Real>>,
    uploads: Res<'w, crate::systems::upload_meter::UploadMeter>,
    census: crate::systems::render_census::RenderCensus<'w, 's>,
    adapter: Res<'w, bevy::render::renderer::RenderAdapterInfo>,
    churn: Res<'w, MeshChurn>,
    diagnostics: Res<'w, DiagnosticsStore>,
}

/// Open and close the measurement windows, and write the report when the run ends.
///
/// Ordered **before** `shot_harness::drive_capture`, which is load-bearing: the
/// capture path consumes its `saved` flag, so a system that wants to see the
/// capture land has to look first.
pub fn drive_perf_window(
    run: Res<PerfRun>,
    mut progress: ResMut<ShotProgress>,
    mut perf: ResMut<PerfProgress>,
    readouts: PerfReadouts,
    mut camera: ResMut<crate::resources::CameraState>,
) {
    // The report is out and the capture path is draining to exit. `placed` is not
    // cleared on the final viewpoint, so without this the `load` arm below opens a
    // window on a scenario that does not exist.
    if perf.written {
        return;
    }
    let index = progress.shot_index();

    if perf.phase == Phase::Between && progress.is_placed() {
        perf.index = index;
        perf.phase = Phase::Load;
        perf.base = take_base(&readouts);
        return;
    }

    if matches!(perf.phase, Phase::Load | Phase::Steady | Phase::Turn) {
        perf.base.frames += 1;
        perf.base.wall_s += readouts.real.delta_secs();
        perf.base.frame_max_s = perf.base.frame_max_s.max(readouts.real.delta_secs());
        // Sample the fill counters every frame, not once at the end — a point
        // reading is not reproducible. See `GpuPass::frag_min`.
        for pass in gpu_passes(&readouts.diagnostics) {
            let Some(frag) = pass.fragment_invocations else {
                continue;
            };
            let entry = perf.frag_seen.entry(pass.path).or_insert((frag, frag, 0));
            entry.0 = entry.0.min(frag);
            entry.1 = entry.1.max(frag);
            entry.2 += 1;
        }
    }

    if perf.phase == Phase::Load && progress.verdict_reached() {
        // A verdict is reached either way: the settle test passing, or `wait`
        // running out. Record which, because every rate in the window below is
        // divided by its wall clock and a timeout stretches that to the cap.
        perf.settled = progress.verdict_settled();
        let window = close(&mut perf, "load", &run, &readouts);
        perf.samples.push(window);
        perf.phase = Phase::Quiescing;
        // Claim the run now, before the shutter, rather than when the capture
        // lands: `drive_capture` reads this on the frame it would otherwise
        // advance, so the hold is in place however the readback is timed.
        progress.perf_hold = true;
        return;
    }

    if perf.phase == Phase::Quiescing && progress.perf_captured() {
        // The pool was paused for the readback; a steady window has to be a real
        // idle frame, streamers running and quiet, or it measures the harness.
        crate::platform::workers::set_paused(false);
        perf.left = run
            .steady_frames
            .get(perf.index)
            .copied()
            .unwrap_or(DEFAULT_STEADY_FRAMES);
        perf.phase = Phase::Steady;
        perf.base = take_base(&readouts);
        return;
    }

    if perf.phase == Phase::Steady {
        perf.left = perf.left.saturating_sub(1);
        if perf.left > 0 {
            return;
        }
        let window = close(&mut perf, "steady", &run, &readouts);
        perf.samples.push(window);
        perf.left = TURN_FRAMES;
        perf.phase = Phase::Turn;
        perf.base = take_base(&readouts);
        return;
    }

    if perf.phase == Phase::Turn {
        turn_camera(&mut camera, TURN_DEG_PER_FRAME.to_radians());
        perf.left = perf.left.saturating_sub(1);
        if perf.left > 0 {
            return;
        }
        let window = close(&mut perf, "turn", &run, &readouts);
        perf.samples.push(window);
        perf.phase = Phase::Between;
        // The last scenario's steady window is the end of the measurement, and the
        // only place the report can be written from: `drive_capture` drains two
        // more frames and then writes `AppExit`.
        if perf.index + 1 >= run.names.len() {
            finish(&run, &mut perf, &readouts.adapter);
        }
        progress.perf_hold = false;
        progress.perf_release = true;
    }
}

/// Yaw the view by `step` radians in whichever camera mode is live, through the
/// same angle the mouse writes — so `camera_controls` places the camera exactly as
/// it would for a person turning it.
fn turn_camera(camera: &mut crate::resources::CameraState, step: f32) {
    use crate::resources::CameraMode;
    match camera.mode {
        CameraMode::Free => camera.free_camera_rotation.y += step,
        CameraMode::Orbit if camera.orbit_space => camera.orbit_heading += step,
        CameraMode::Orbit => camera.orbit_yaw += step,
        CameraMode::Avatar => camera.azimuth += step,
    }
}

fn take_base(readouts: &PerfReadouts) -> Base {
    Base {
        upload_bytes: readouts.uploads.total_bytes,
        stalls: crate::platform::watchdog::stall_totals(),
        workers: crate::platform::workers::stats::snapshot(),
        mesh_added: readouts.churn.added,
        materials_modified: readouts.churn.materials_modified,
        frames: 0,
        wall_s: 0.0,
        frame_max_s: 0.0,
        #[cfg(feature = "perf")]
        systems: super::system_timings::snapshot(),
    }
}

/// Turn the accumulated counters into a finished [`Window`].
fn close(perf: &mut PerfProgress, phase: &str, run: &PerfRun, readouts: &PerfReadouts) -> Window {
    let base = std::mem::take(&mut perf.base);
    let frag_seen = std::mem::take(&mut perf.frag_seen);
    let frames = base.frames;
    // A window with no frames divides by zero everywhere below, and it is
    // reachable: a scenario that settles on the frame it is placed has a one-frame
    // `load` window. Guard once, here.
    let per_frame = if frames == 0 { 1.0 } else { f64::from(frames) };

    let workers = crate::platform::workers::stats::snapshot();
    let worker_window = workers.iter().fold(
        crate::platform::workers::stats::Totals::default(),
        |acc, (label, totals)| {
            let then = base
                .workers
                .iter()
                .find(|(l, _)| l == label)
                .map(|(_, t)| *t)
                .unwrap_or_default();
            let d = totals.since(then);
            crate::platform::workers::stats::Totals {
                jobs: acc.jobs + d.jobs,
                busy_us: acc.busy_us + d.busy_us,
                wait_us: acc.wait_us + d.wait_us,
            }
        },
    );
    let worker_jobs = worker_window.jobs;
    let worker_threads = crate::platform::workers::worker_count();
    // `busy / (threads × wall)`. Guarded rather than clamped: a zero-length
    // window would otherwise report `inf` as a performance result.
    let worker_utilisation = {
        let denom = worker_threads.max(1) as f64 * f64::from(base.wall_s) * 1_000_000.0;
        if denom > 0.0 {
            (worker_window.busy_us as f64 / denom) as f32
        } else {
            0.0
        }
    };
    let counts = readouts.census.counts();
    let store = &readouts.diagnostics;

    let mut passes = gpu_passes(store);
    for pass in &mut passes {
        if let Some((lo, hi, n)) = frag_seen.get(&pass.path) {
            pass.frag_min = Some(*lo);
            pass.frag_max = Some(*hi);
            pass.frag_samples = *n;
        }
    }

    // The window's own per-system cost: the totals now, less the totals when it
    // opened. Without the feature there is no counter and the list is empty,
    // which is the difference between "not instrumented" and "costs nothing" that
    // the module docs insist on everywhere else.
    #[cfg(feature = "perf")]
    let systems: Vec<SystemCost> = {
        let now = super::system_timings::snapshot();
        super::system_timings::delta(&base.systems, &now)
            .into_iter()
            .take(TOP_SYSTEMS)
            .map(|(name, cost)| {
                #[allow(clippy::cast_precision_loss)]
                let ms = cost.nanos as f64 / 1.0e6;
                SystemCost {
                    name,
                    ms,
                    calls: cost.calls,
                    ms_per_frame: ms / per_frame,
                }
            })
            .collect()
    };
    #[cfg(not(feature = "perf"))]
    let systems: Vec<SystemCost> = Vec::new();

    let mut window = Window {
        scenario: run
            .names
            .get(perf.index)
            .cloned()
            .unwrap_or_else(|| format!("shot{}", perf.index)),
        phase: phase.to_string(),
        frames,
        wall_s: base.wall_s,
        fps: diag(store, "fps"),
        frame_time_ms: diag(store, "frame_time"),
        frame_ms_mean: f64::from(base.wall_s) * 1000.0 / per_frame,
        frame_ms_max: f64::from(base.frame_max_s) * 1000.0,
        process_cpu_percent: diag(store, "process/cpu_usage"),
        process_mem_percent: diag(store, "process/mem_usage"),
        entity_count: diag(store, "entity_count"),
        mesh_slabs: diag(store, "mesh_allocator_slabs"),
        mesh_slab_mb: diag(store, "mesh_allocator_slabs_size").map(|b| b / 1_048_576.0),
        gpu_passes: passes,
        systems,
        upload_kb: readouts
            .uploads
            .total_bytes
            .saturating_sub(base.upload_bytes)
            / 1024,
        settled: perf.settled,
        stalls: {
            let (n, _) = crate::platform::watchdog::stall_totals();
            n.saturating_sub(base.stalls.0)
        },
        stalled_ms: {
            let (_, ms) = crate::platform::watchdog::stall_totals();
            ms.saturating_sub(base.stalls.1)
        },
        worker_jobs,
        worker_busy_ms: worker_window.busy_us / 1000,
        worker_wait_ms: worker_window.wait_us / 1000,
        worker_threads,
        worker_utilisation,
        mesh_added_per_frame: readouts.churn.added.saturating_sub(base.mesh_added) as f64
            / per_frame,
        material_modified_per_frame: readouts
            .churn
            .materials_modified
            .saturating_sub(base.materials_modified) as f64
            / per_frame,
        resident_kb: readouts.uploads.resident_kb(),
        resident_image_kb: readouts.uploads.resident_image_kb(),
        resident_mesh_kb: readouts.uploads.resident_mesh_kb(),
        draws: counts.draws,
        tris_total: counts.tris_total,
        quiet: false,
    };
    // Quiet means the three streaming signals are all at rest. Mesh churn is in
    // the test because an arriving mesh is work for the allocator whether or not
    // its bytes were counted as an upload this frame.
    window.quiet =
        window.upload_kb == 0 && window.worker_jobs == 0 && window.mesh_added_per_frame < 0.5;
    if !window.settled {
        tracing::warn!(
            scenario = %window.scenario,
            phase = %window.phase,
            wall_s = format!("{:.1}", window.wall_s),
            worker_util = format!("{:.2}", window.worker_utilisation),
            "perf: this scenario never settled — `wait` ran out. Every rate in \
             this window is divided by a wall clock the timeout stretched to the \
             cap, so `worker_utilisation` and `fps` understate and \
             `mesh_added_per_frame` overstates. Find what `busy` names in the \
             shot state line above (a stalled external layer will sit there) \
             before reading any number here as a measurement."
        );
    }
    if phase == "steady" && !window.quiet {
        tracing::warn!(
            scenario = %window.scenario,
            upload_kb = window.upload_kb,
            worker_jobs = window.worker_jobs,
            mesh_added_per_frame = format!("{:.1}", window.mesh_added_per_frame),
            "perf: the steady window was still streaming — its numbers describe a \
             world being built, not an idle one. Raise `wait` for this scenario, or \
             read it as a streaming measurement."
        );
    }
    if phase == "steady" && window.material_modified_per_frame >= 1.0 {
        tracing::warn!(
            scenario = %window.scenario,
            material_modified_per_frame = format!("{:.1}", window.material_modified_per_frame),
            "perf: materials are being marked modified on a still view — each one is \
             re-prepared on the render thread every frame. Something is writing through \
             `Assets::get_mut` without comparing first; see `count_material_churn`."
        );
    }
    log_window(&window);
    window
}

/// One window as a handful of log lines, so a shell that never opens the JSON
/// still learns something.
fn log_window(w: &Window) {
    let opt = |v: Option<f64>, p: usize| {
        v.map(|v| format!("{v:.p$}", p = p))
            .unwrap_or_else(|| "-".into())
    };
    tracing::info!(
        scenario = %w.scenario,
        phase = %w.phase,
        quiet = w.quiet,
        frames = w.frames,
        wall_s = format!("{:.1}", w.wall_s),
        fps = opt(w.fps, 1),
        frame_ms = opt(w.frame_time_ms, 1),
        frame_ms_mean = format!("{:.1}", w.frame_ms_mean),
        frame_ms_max = format!("{:.1}", w.frame_ms_max),
        cpu_percent = opt(w.process_cpu_percent, 0),
        mem_percent = opt(w.process_mem_percent, 1),
        entities = opt(w.entity_count, 0),
        mesh_slab_mb = opt(w.mesh_slab_mb, 0),
        upload_kb = w.upload_kb,
        worker_jobs = w.worker_jobs,
        worker_busy_s = format!("{:.1}", w.worker_busy_ms as f64 / 1000.0),
        worker_wait_s = format!("{:.1}", w.worker_wait_ms as f64 / 1000.0),
        worker_threads = w.worker_threads,
        worker_util = format!("{:.2}", w.worker_utilisation),
        stalls = w.stalls,
        stalled_s = format!("{:.1}", w.stalled_ms as f64 / 1000.0),
        mesh_added_per_frame = format!("{:.1}", w.mesh_added_per_frame),
        material_modified_per_frame = format!("{:.1}", w.material_modified_per_frame),
        resident_mb = format!(
            "{:.0} ({:.0} tex / {:.0} geo)",
            w.resident_kb as f64 / 1024.0,
            w.resident_image_kb as f64 / 1024.0,
            w.resident_mesh_kb as f64 / 1024.0
        ),
        draws = w.draws,
        "perf window"
    );
    for p in w.gpu_passes.iter().take(8) {
        tracing::info!(
            scenario = %w.scenario,
            phase = %w.phase,
            pass = %p.path,
            fragment_invocations = opt(p.fragment_invocations, 0),
            frag_range = match (p.frag_min, p.frag_max) {
                (Some(lo), Some(hi)) if hi > lo => format!("{lo:.0}..{hi:.0} !unstable"),
                (Some(lo), Some(_)) => format!("{lo:.0} stable"),
                _ => "-".into(),
            },
            gpu_ms = opt(p.gpu_ms, 3),
            cpu_ms = opt(p.cpu_ms, 3),
            "perf gpu pass"
        );
    }
    // The dearest systems, one line each, beside the passes. This is the column
    // that says whether a frame is the render thread's or the main world's, and
    // it is worth reading before any pass: on `deck-1` the settled `city` frame is
    // 14.6 ms of render schedule against 5.9 ms of main world, of which only
    // 4.0 ms is in a pass the GPU timers can see.
    for s in &w.systems {
        tracing::info!(
            scenario = %w.scenario,
            phase = %w.phase,
            system = %s.name,
            ms = format!("{:.1}", s.ms),
            ms_per_frame = format!("{:.3}", s.ms_per_frame),
            calls = s.calls,
            "perf system"
        );
    }
}

/// One aggregated window as a log line, beside the per-flight `perf window`
/// lines it was computed from.
fn log_summary(w: &WindowSummary) {
    let range = |s: Option<Stat>, p: usize| {
        s.map(|s| format!("{:.p$} ({:.p$}..{:.p$})", s.median, s.min, s.max, p = p))
            .unwrap_or_else(|| "-".into())
    };
    tracing::info!(
        scenario = %w.scenario,
        phase = %w.phase,
        runs = w.runs,
        quiet_runs = w.quiet_runs,
        settled_runs = w.settled_runs,
        frame_ms = range(w.frame_time_ms, 1),
        frame_ms_mean = range(Some(w.frame_ms_mean), 1),
        frame_ms_max = range(Some(w.frame_ms_max), 1),
        fps = range(w.fps, 1),
        draws = range(Some(w.draws), 0),
        "perf summary"
    );
}

/// Write the report. Called once, from the last scenario's steady window.
fn finish(
    run: &PerfRun,
    perf: &mut PerfProgress,
    adapter: &bevy::render::renderer::RenderAdapterInfo,
) {
    if perf.written {
        return;
    }
    perf.written = true;
    let windows = summarise(&std::mem::take(&mut perf.samples));
    let report = Report {
        meta: ReportMeta {
            adapter: format!("{:?} {}", adapter.device_type, adapter.name),
            software_renderer: adapter.device_type == egui_wgpu::wgpu::DeviceType::Cpu
                || is_software(&adapter.name),
            quality: format!("{:?}", crate::quality::budget().tier),
            size: run.size.clone(),
            perf_feature: cfg!(feature = "perf"),
            debug_assertions: cfg!(debug_assertions),
            repeat: run.repeat,
        },
        patrols: rank_patrols(&run.stops, &windows),
        windows,
    };
    for w in &report.windows {
        log_summary(w);
    }
    for p in &report.patrols {
        log_patrol(p);
    }
    match serde_json::to_string_pretty(&report) {
        Ok(json) => match std::fs::write(&run.out, json) {
            Ok(()) => tracing::info!(
                path = %run.out.display(),
                windows = report.windows.len(),
                "perf: wrote report"
            ),
            Err(e) => {
                tracing::error!(path = %run.out.display(), error = %e, "perf: report write failed");
            }
        },
        Err(e) => tracing::error!(error = %e, "perf: report serialize failed"),
    }
}

/// Whether an adapter name is one of the CPU rasterisers.
///
/// Matched on the name because `DeviceType::Cpu` is not reliably reported —
/// lavapipe answers `Cpu`, SwiftShader has been seen answering `Other`.
fn is_software(name: &str) -> bool {
    let name = name.to_ascii_lowercase();
    ["llvmpipe", "lavapipe", "swiftshader", "softpipe"]
        .iter()
        .any(|n| name.contains(n))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_unknown_scenario_name_is_an_error_not_an_empty_run() {
        let manifest = PerfManifest {
            size: None,
            settle: None,
            wait: None,
            tile_url: None,
            steady_frames: None,
            scenarios: vec![PerfScenario {
                name: "orbit".into(),
                description: None,
                at: [0.0, 0.0, 1000.0],
                look: None,
                mode: None,
                time: None,
                fog: None,
                clouds: None,
                rain: None,
                ui: None,
                hide_ui: None,
                steady_frames: None,
                patrol: None,
            }],
            patrols: Vec::new(),
        };
        assert!(manifest.select(&["orbti".into()]).is_err());
        assert_eq!(manifest.select(&["orbit".into()]).unwrap().len(), 1);
        assert_eq!(manifest.select(&[]).unwrap().len(), 1);
    }

    #[test]
    fn the_shipped_manifest_parses_and_pins_its_weather() {
        let path = Path::new(DEFAULT_MANIFEST);
        if !path.exists() {
            return; // running from somewhere other than the repo root
        }
        let manifest = PerfManifest::load(path).expect("docs/perf.toml parses");
        assert!(!manifest.scenarios.is_empty());
        for s in &manifest.scenarios {
            assert!(
                s.time.is_some(),
                "scenario {:?} leaves the time of day to the clock, so its shadow \
                 cascades differ run to run and its numbers are noise",
                s.name
            );
            assert!(
                s.description.is_some(),
                "scenario {:?} has no description saying which cost it exercises",
                s.name
            );
        }
    }

    fn flight(scenario: &str, phase: &str, frame_ms: Option<f64>, draws: usize) -> Window {
        Window {
            scenario: scenario.into(),
            phase: phase.into(),
            frame_time_ms: frame_ms,
            draws,
            ..Default::default()
        }
    }

    fn pass(path: &str, frag: f64, lo: f64, hi: f64, n: u32) -> GpuPass {
        GpuPass {
            path: path.into(),
            gpu_ms: None,
            cpu_ms: None,
            fragment_invocations: Some(frag),
            frag_min: Some(lo),
            frag_max: Some(hi),
            frag_samples: n,
        }
    }

    #[test]
    fn three_flights_give_one_window_with_the_middle_as_median() {
        let s = summarise(&[
            flight("orbit", "steady", Some(30.0), 10),
            flight("orbit", "steady", Some(10.0), 12),
            flight("orbit", "steady", Some(20.0), 11),
        ]);
        assert_eq!(s.len(), 1);
        assert_eq!(s[0].runs, 3);
        let ft = s[0].frame_time_ms.unwrap();
        assert_eq!((ft.runs, ft.min, ft.median, ft.max), (3, 10.0, 20.0, 30.0));
        assert_eq!(
            (s[0].draws.min, s[0].draws.median, s[0].draws.max),
            (10.0, 11.0, 12.0)
        );
    }

    #[test]
    fn one_flight_is_its_own_min_median_and_max() {
        let s = summarise(&[flight("orbit", "load", Some(7.5), 3)]);
        let ft = s[0].frame_time_ms.unwrap();
        assert_eq!(s[0].runs, 1);
        assert_eq!((ft.runs, ft.min, ft.median, ft.max), (1, 7.5, 7.5, 7.5));
    }

    #[test]
    fn an_even_count_takes_the_mean_of_the_middle_two() {
        let s = Stat::of([4.0, 1.0, 3.0, 2.0]).unwrap();
        assert_eq!((s.runs, s.min, s.median, s.max), (4, 1.0, 2.5, 4.0));
    }

    #[test]
    fn an_absent_column_is_not_counted_as_zero() {
        let s = summarise(&[
            flight("orbit", "steady", Some(10.0), 1),
            flight("orbit", "steady", None, 1),
            flight("orbit", "steady", Some(20.0), 1),
        ]);
        let ft = s[0].frame_time_ms.unwrap();
        assert_eq!((ft.runs, ft.min, ft.median, ft.max), (2, 10.0, 15.0, 20.0));
        // Never measured in any flight: absent, not a zero.
        assert!(s[0].fps.is_none());
    }

    #[test]
    fn windows_keep_first_appearance_order() {
        // Round-robin order, as `build` flies two repeats of two scenarios.
        let s = summarise(&[
            flight("a", "load", None, 0),
            flight("a", "steady", None, 0),
            flight("b", "load", None, 0),
            flight("b", "steady", None, 0),
            flight("a", "load", None, 0),
            flight("a", "steady", None, 0),
            flight("b", "load", None, 0),
            flight("b", "steady", None, 0),
        ]);
        let order: Vec<(&str, &str, u32)> = s
            .iter()
            .map(|w| (w.scenario.as_str(), w.phase.as_str(), w.runs))
            .collect();
        assert_eq!(
            order,
            [
                ("a", "load", 2),
                ("a", "steady", 2),
                ("b", "load", 2),
                ("b", "steady", 2)
            ]
        );
    }

    #[test]
    fn gpu_passes_merge_by_path_and_widen_the_fill_range() {
        let mut one = flight("orbit", "steady", None, 0);
        one.gpu_passes = vec![
            pass("render/main", 100.0, 90.0, 110.0, 5),
            pass("render/ui", 5.0, 5.0, 5.0, 5),
        ];
        let mut two = flight("orbit", "steady", None, 0);
        two.gpu_passes = vec![pass("render/main", 120.0, 80.0, 130.0, 6)];
        let s = summarise(&[one, two]);
        let main = &s[0].gpu_passes[0];
        assert_eq!(main.path, "render/main");
        assert_eq!(main.runs, 2);
        assert_eq!(
            (main.frag_min, main.frag_max, main.frag_samples),
            (Some(80.0), Some(130.0), 11)
        );
        assert_eq!(main.fragment_invocations.unwrap().median, 110.0);
        let ui = &s[0].gpu_passes[1];
        assert_eq!((ui.path.as_str(), ui.runs), ("render/ui", 1));
    }

    #[test]
    fn repeats_fly_round_robin_and_name_each_picture() {
        let dir = tempfile::tempdir().unwrap();
        let manifest = dir.path().join("perf.toml");
        std::fs::write(
            &manifest,
            "[[scenario]]\nname = \"a\"\nat = [53.2, 6.5, 1000]\n\
             [[scenario]]\nname = \"b\"\nat = [53.2, 6.5, 2000]\n",
        )
        .unwrap();
        let out = dir.path().join("perf.json");
        let (harness, run) = build(&manifest, &[], None, None, 2, out.clone()).unwrap();
        assert_eq!(run.names, ["a", "b", "a", "b"]);
        assert_eq!(run.steady_frames.len(), 4);
        assert_eq!(run.repeat, 2);
        let pngs: Vec<String> = harness
            .shots
            .iter()
            .map(|s| {
                s.out
                    .as_ref()
                    .unwrap()
                    .file_name()
                    .unwrap()
                    .to_string_lossy()
                    .into_owned()
            })
            .collect();
        assert_eq!(
            pngs,
            [
                "perf-a-r1.png",
                "perf-b-r1.png",
                "perf-a-r2.png",
                "perf-b-r2.png"
            ]
        );

        // One flight keeps the name it always had.
        let (harness, run) = build(&manifest, &["a".into()], None, None, 1, out.clone()).unwrap();
        assert_eq!(run.names, ["a"]);
        assert_eq!(
            harness.shots[0].out.as_ref().unwrap().file_name().unwrap(),
            "perf-a.png"
        );

        assert!(build(&manifest, &[], None, None, 0, out).is_err());
    }

    fn patrol(area: [f64; 4], step_m: f64) -> PerfPatrol {
        PerfPatrol {
            name: "p".into(),
            description: None,
            area,
            step_m,
            altitudes: vec![100.0],
            looks: None,
            mode: None,
            time: Some(12.0),
            fog: None,
            clouds: None,
            rain: None,
            steady_frames: None,
        }
    }

    #[test]
    fn a_patrol_walks_its_grid_serpentine_and_keeps_the_far_edge() {
        // 200 m by 300 m at a 100 m step: 3 rows of 4, edges included. Drawn
        // with the scale at the area's middle, which is the one `expand` uses.
        let north = 53.2 + 200.0 / cartopolis_core::geo::degree_scale_m(53.2).0;
        let (per_lat, per_lng) = cartopolis_core::geo::degree_scale_m((53.2 + north) / 2.0);
        let area = [53.2, 6.5, 53.2 + 200.0 / per_lat, 6.5 + 300.0 / per_lng];
        let stops = patrol(area, 100.0).expand().unwrap();
        let names: Vec<&str> = stops.iter().map(|s| s.name.as_str()).collect();
        assert_eq!(
            names,
            [
                "p-100m-r0c0",
                "p-100m-r0c1",
                "p-100m-r0c2",
                "p-100m-r0c3", //
                "p-100m-r1c3",
                "p-100m-r1c2",
                "p-100m-r1c1",
                "p-100m-r1c0", //
                "p-100m-r2c0",
                "p-100m-r2c1",
                "p-100m-r2c2",
                "p-100m-r2c3",
            ]
        );
        // Every move is one step.
        for pair in stops.windows(2) {
            let (a, b) = (pair[0].at, pair[1].at);
            let d = cartopolis_core::geo::ground_distance_m(a[0], a[1], b[0], b[1]);
            assert!(
                (d - 100.0).abs() < 1.0,
                "{} → {}: {d} m",
                pair[0].name,
                pair[1].name
            );
        }
        assert!(stops
            .iter()
            .all(|s| s.patrol.as_deref() == Some("p") && s.time == Some(12.0)));
    }

    #[test]
    fn several_looks_name_each_stop_and_altitudes_go_outermost() {
        let mut p = patrol([53.2, 6.5, 53.2001, 6.5001], 100.0);
        p.altitudes = vec![50.0, 500.0];
        p.looks = Some(vec![[-10.0, 0.0], [-10.0, 180.0]]);
        let names: Vec<String> = p.expand().unwrap().into_iter().map(|s| s.name).collect();
        assert_eq!(
            names,
            [
                "p-50m-r0c0-l0",
                "p-50m-r0c0-l1",
                "p-500m-r0c0-l0",
                "p-500m-r0c0-l1"
            ]
        );
    }

    #[test]
    fn a_patrol_that_cannot_be_flown_is_refused_at_load() {
        let mut no_time = patrol([53.2, 6.5, 53.21, 6.51], 100.0);
        no_time.time = None;
        assert!(no_time.expand().is_err());
        // A city at a metre's step.
        assert!(patrol([53.2, 6.5, 53.25, 6.6], 1.0).expand().is_err());
        assert!(patrol([53.21, 6.5, 53.2, 6.51], 100.0).expand().is_err());
        assert!(patrol([53.2, 6.5, 53.21, 6.51], 0.0).expand().is_err());
    }

    #[test]
    fn a_patrols_name_selects_all_its_stops_and_one_stop_alone() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("patrol.toml");
        std::fs::write(
            &path,
            "[[scenario]]\nname = \"a\"\nat = [53.2, 6.5, 1000]\n\
             [[patrol]]\nname = \"grid\"\narea = [53.2, 6.5, 53.2005, 6.5001]\n\
             step_m = 50\naltitudes = [100]\ntime = 12.0\n",
        )
        .unwrap();
        let manifest = PerfManifest::load(&path).unwrap();
        assert_eq!(manifest.select(&[]).unwrap().len(), 3);
        assert_eq!(manifest.select(&["grid".into()]).unwrap().len(), 2);
        assert_eq!(
            manifest.select(&["grid-100m-r1c0".into()]).unwrap().len(),
            1
        );
        let err = manifest.select(&["gird".into()]).unwrap_err();
        assert!(err.contains("a, grid"), "{err}");

        let (_, run) = build(
            &path,
            &["grid".into()],
            None,
            None,
            2,
            dir.path().join("o.json"),
        )
        .unwrap();
        assert_eq!(run.names.len(), 4);
        // Once per stop, however many times it is flown.
        assert_eq!(run.stops.len(), 2);
    }

    #[test]
    fn the_ranking_puts_the_outlier_first_with_its_ratio() {
        let stop = |name: &str| PatrolStop {
            patrol: "grid".into(),
            scenario: name.into(),
            at: [53.2, 6.5, 100.0],
            look: [-90.0, 0.0],
        };
        let stops = [stop("s1"), stop("s2"), stop("s3"), stop("s4"), stop("s5")];
        let mut windows = Vec::new();
        for (name, draws) in [
            ("s1", 100),
            ("s2", 110),
            ("s3", 400),
            ("s4", 90),
            ("s5", 100),
        ] {
            windows.push(flight(name, "steady", None, draws));
            windows.push(flight(name, "turn", None, draws));
        }
        let report = rank_patrols(&stops, &summarise(&windows));
        assert_eq!(report.len(), 1);
        assert_eq!(report[0].stops, 5);
        let draws = report[0]
            .columns
            .iter()
            .find(|c| c.phase == "steady" && c.column == "draws")
            .unwrap();
        assert_eq!(draws.patrol_median, 100.0);
        assert_eq!(draws.dearest[0].scenario, "s3");
        assert_eq!(draws.dearest[0].ratio, Some(4.0));
        assert_eq!(draws.dearest.len(), PATROL_DEAREST);
        // Zero everywhere: ranked, with nothing to list and no ratio to give.
        let churn = report[0]
            .columns
            .iter()
            .find(|c| c.phase == "steady" && c.column == "material_modified_per_frame")
            .unwrap();
        assert_eq!(churn.patrol_median, 0.0);
        assert!(churn.dearest.is_empty());
        // No `perf` feature in these windows: no fill column at all.
        assert!(!report[0].columns.iter().any(|c| c.column == "fill"));
    }

    #[test]
    fn the_shipped_patrol_parses_and_fits_under_the_cap() {
        let path = Path::new("docs/patrol.toml");
        if !path.exists() {
            return; // running from somewhere other than the repo root
        }
        let manifest = PerfManifest::load(path).expect("docs/patrol.toml parses");
        assert!(!manifest.patrols.is_empty());
        for p in &manifest.patrols {
            assert!(
                p.description.is_some(),
                "patrol {:?} says nothing of why",
                p.name
            );
        }
    }

    #[test]
    fn the_cpu_rasterisers_are_recognised() {
        assert!(is_software("llvmpipe (LLVM 17.0.6, 256 bits)"));
        assert!(is_software("SwiftShader Device (LLVM 16)"));
        assert!(!is_software("Adreno (TM) 840"));
        assert!(!is_software("NVIDIA GeForce RTX 4070"));
    }
}
