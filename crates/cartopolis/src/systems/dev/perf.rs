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
//! ## The two windows
//!
//! Each scenario is flown, settled and captured by the existing `--shot` harness,
//! then measured twice over spans that harness already delimits:
//!
//! * **`load`** — teleport to settle verdict. The streaming burst: fetches,
//!   decodes, mesh builds, uploads. What a teleport or a cold start costs.
//! * **`steady`** — a run of frames after the shutter, with the world *meant* to be
//!   settled. Check [`Window::quiet`] before reading it as an idle number: at
//!   ground level the settle test passes while the vector-tile streamer is still
//!   trickling, and a window that was still building the city is a streaming
//!   measurement wearing the wrong label.
//!
//! Neither window drives the camera. A motion window would have to fight
//! `place_camera` for the transform to measure what `load` already measures.
//!
//! ## Running it
//!
//! ```bash
//! cargo perf                                    # every scenario → /tmp/perf.json
//! cargo perf --only orbit                       # a subset
//! cargo perf --features perf                    # + allocations and the GPU passes
//! ```

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
}

impl PerfManifest {
    pub fn load(path: &Path) -> Result<Self, String> {
        let text = std::fs::read_to_string(path).map_err(|e| format!("{}: {e}", path.display()))?;
        let manifest: PerfManifest =
            toml::from_str(&text).map_err(|e| format!("{}: {e}", path.display()))?;
        if manifest.scenarios.is_empty() {
            return Err(format!("{}: no [[scenario]] entries", path.display()));
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

    /// The scenarios `--only` names, or all of them.
    ///
    /// An unknown name is an error rather than an empty run: `--only orbti` is a
    /// typo, and a suite that answers a typo with "0 scenarios, nothing wrong" is
    /// reporting green for the wrong reason.
    pub fn select(&self, only: &[String]) -> Result<Vec<&PerfScenario>, String> {
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
            if !self.scenarios.iter().any(|s| s.name == *name) {
                let known: Vec<&str> = self.scenarios.iter().map(|s| s.name.as_str()).collect();
                return Err(format!(
                    "no scenario named {name:?}; the manifest has {}",
                    known.join(", ")
                ));
            }
        }
        Ok(self
            .scenarios
            .iter()
            .filter(|s| wanted.contains(&s.name.as_str()))
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
}

/// Counters a window is measured against, taken at its start.
#[derive(Clone, Debug, Default)]
struct Base {
    upload_bytes: u64,
    workers: Vec<(&'static str, crate::platform::workers::stats::Totals)>,
    mesh_added: u64,
    frames: u32,
    wall_s: f32,
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
}

/// Running count of mesh asset events, for [`Window::mesh_added_per_frame`].
#[derive(Resource, Default, Debug, Clone, Copy)]
pub struct MeshChurn {
    pub added: u64,
    pub modified: u64,
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
    /// `load` or `steady`.
    pub phase: String,
    pub frames: u32,
    pub wall_s: f32,

    // ---- straight from Bevy's diagnostics -------------------------------
    /// `FrameTimeDiagnosticsPlugin`. Reported, never gated: a frame time here is
    /// a property of a shared eight-core box with a software rasteriser.
    pub fps: Option<f64>,
    pub frame_time_ms: Option<f64>,
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

    // ---- this client's own -----------------------------------------------
    /// GPU upload traffic over the window, in kB.
    pub upload_kb: u64,
    /// Tile-worker jobs finished in the window.
    pub worker_jobs: u64,
    /// Mesh assets arriving per frame. On a settled window this should be ~0;
    /// whatever it is, is what the mesh allocator is being paid to process.
    pub mesh_added_per_frame: f64,
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
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Report {
    pub meta: ReportMeta,
    pub windows: Vec<Window>,
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
    out: PathBuf,
) -> Result<(ShotHarness, PerfRun), String> {
    use crate::systems::shot_harness::{parse_size, DEFAULT_SETTLE, DEFAULT_WAIT};

    let manifest = PerfManifest::load(manifest_path)?;
    let selected = manifest.select(only)?;
    let default_frames = manifest.steady_frames.unwrap_or(DEFAULT_STEADY_FRAMES);

    let shots: Vec<ShotSpec> = selected
        .iter()
        .map(|s| {
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
                out: Some(std::env::temp_dir().join(format!("perf-{}.png", s.name))),
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
        live: false,
        windowed: false,
    };
    let run = PerfRun {
        names: selected.iter().map(|s| s.name.clone()).collect(),
        steady_frames: selected
            .iter()
            .map(|s| {
                steady_frames_override
                    .or(s.steady_frames)
                    .unwrap_or(default_frames)
            })
            .collect(),
        out,
        size: format!("{}x{}", parsed.x, parsed.y),
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

    if matches!(perf.phase, Phase::Load | Phase::Steady) {
        perf.base.frames += 1;
        perf.base.wall_s += readouts.real.delta_secs();
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

fn take_base(readouts: &PerfReadouts) -> Base {
    Base {
        upload_bytes: readouts.uploads.total_bytes,
        workers: crate::platform::workers::stats::snapshot(),
        mesh_added: readouts.churn.added,
        frames: 0,
        wall_s: 0.0,
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
    let worker_jobs = workers.iter().fold(0u64, |acc, (label, totals)| {
        let then = base
            .workers
            .iter()
            .find(|(l, _)| l == label)
            .map(|(_, t)| *t)
            .unwrap_or_default();
        acc + totals.since(then).jobs
    });
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
        process_cpu_percent: diag(store, "process/cpu_usage"),
        process_mem_percent: diag(store, "process/mem_usage"),
        entity_count: diag(store, "entity_count"),
        mesh_slabs: diag(store, "mesh_allocator_slabs"),
        mesh_slab_mb: diag(store, "mesh_allocator_slabs_size").map(|b| b / 1_048_576.0),
        gpu_passes: passes,
        upload_kb: readouts
            .uploads
            .total_bytes
            .saturating_sub(base.upload_bytes)
            / 1024,
        worker_jobs,
        mesh_added_per_frame: readouts.churn.added.saturating_sub(base.mesh_added) as f64
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
        cpu_percent = opt(w.process_cpu_percent, 0),
        mem_percent = opt(w.process_mem_percent, 1),
        entities = opt(w.entity_count, 0),
        mesh_slab_mb = opt(w.mesh_slab_mb, 0),
        upload_kb = w.upload_kb,
        worker_jobs = w.worker_jobs,
        mesh_added_per_frame = format!("{:.1}", w.mesh_added_per_frame),
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
    let report = Report {
        meta: ReportMeta {
            adapter: format!("{:?} {}", adapter.device_type, adapter.name),
            software_renderer: adapter.device_type == egui_wgpu::wgpu::DeviceType::Cpu
                || is_software(&adapter.name),
            quality: format!("{:?}", crate::quality::budget().tier),
            size: run.size.clone(),
            perf_feature: cfg!(feature = "perf"),
            debug_assertions: cfg!(debug_assertions),
        },
        windows: std::mem::take(&mut perf.samples),
    };
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
            }],
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

    #[test]
    fn the_cpu_rasterisers_are_recognised() {
        assert!(is_software("llvmpipe (LLVM 17.0.6, 256 bits)"));
        assert!(is_software("SwiftShader Device (LLVM 16)"));
        assert!(!is_software("Adreno (TM) 840"));
        assert!(!is_software("NVIDIA GeForce RTX 4070"));
    }
}
