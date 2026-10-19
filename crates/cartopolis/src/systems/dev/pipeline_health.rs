//! Whether the GPU pipelines this frame depends on actually built.
//!
//! **A pipeline that fails to compile is a silent hole in the picture.** Bevy's
//! `PipelineCache` stores the error and every render node that asks for the
//! pipeline just skips its pass — no panic, no per-frame log, nothing in the ECS.
//! wgpu logs the shader error once, at creation time, which on a phone means it
//! scrolls out of logd's ring long before anybody thinks to look. The 2026-08-19
//! Adreno 840 report is the motivating case: `sky: Atmosphere` (the decision),
//! black sky (the picture), and no way to tell from the report whether the
//! atmosphere's five pipelines ever compiled on that driver. The diagnostics
//! report carries the answer now: a `pipelines:` line with the errored ones named.
//!
//! The scan runs in the **render app** (the cache lives there) and hands its
//! findings across through a process-global, the same crossing
//! `crate::quality::capabilities()` makes — the render world has no other seam to
//! the diagnostics report, which composes in the main world. Each errored
//! pipeline is also logged at ERROR once, from here, with its label: bevy's own
//! creation-time log line names the *shader*, and by the time somebody reads
//! logcat that line is gone.
//!
//! `CARTO_PIPELINE_LOG` (`?pipeline_log` on the web) also logs every pipeline
//! the cache gains: its label, its shaders and their defs. On the web each one is
//! a WESL compile on the page thread, hundreds of milliseconds, so a pipeline that
//! first appears mid-session is a hitch. This log says which one it was, and which
//! def differs from the variant before it.

use crate::platform::sync::{Mutex, OnceLock};

use bevy::prelude::*;
use bevy::render::render_resource::{CachedPipelineState, PipelineCache, PipelineDescriptor};
use bevy::render::{Render, RenderApp};

/// What the last scan saw. `errored` keeps the label and the error's first line,
/// capped — a driver that rejects one shader family tends to reject dozens, and
/// the report has a binder-transaction budget (`diagnostics::LOG_MAX_BYTES`).
#[derive(Default, Clone)]
pub struct PipelineHealth {
    pub ready: usize,
    pub building: usize,
    pub errored: Vec<String>,
}

/// A process-global rather than a resource, because the reader
/// (`diagnostics::compose`) lives in the main world and the writer in the render
/// world, and the only alternative crossing is an extract system — machinery this
/// one-way, low-rate handoff does not need.
static HEALTH: OnceLock<Mutex<PipelineHealth>> = OnceLock::new();

/// Cap on recorded errors. Distinct failures matter, the four-hundredth
/// specialization of the same broken material does not.
const MAX_ERRORS: usize = 8;

/// The main-world read.
pub fn snapshot() -> Option<PipelineHealth> {
    HEALTH.get().map(|h| {
        h.lock()
            .map(|g| g.clone())
            .unwrap_or_else(|p| p.into_inner().clone())
    })
}

pub struct PipelineHealthPlugin;

impl Plugin for PipelineHealthPlugin {
    fn build(&self, app: &mut App) {
        // Headless test worlds have no render app; a diagnostics line that says
        // "(not measured)" is the honest answer there.
        if let Some(render_app) = app.get_sub_app_mut(RenderApp) {
            render_app.add_systems(Render, watch_pipelines);
        }
    }
}

/// Scan the cache and publish the tally. Cheap — a state match per entry, a few
/// hundred entries — but throttled anyway: nothing reading it needs per-frame
/// freshness.
fn watch_pipelines(
    cache: Res<PipelineCache>,
    mut frame: Local<u32>,
    mut logged: Local<Option<(bool, usize)>>,
) {
    const EVERY: u32 = 30;
    *frame = frame.wrapping_add(1);
    if !frame.is_multiple_of(EVERY) {
        return;
    }
    let (log_new, seen) =
        logged.get_or_insert_with(|| (crate::debug_switches::env_flag("CARTO_PIPELINE_LOG"), 0));
    if *log_new {
        for pipeline in cache.pipelines().skip(*seen) {
            tracing::info!(pipeline = %describe(&pipeline.descriptor), "new GPU pipeline");
        }
        *seen = cache.pipelines().count();
    }

    let mut ready = 0usize;
    let mut building = 0usize;
    let mut errored: Vec<String> = Vec::new();
    for pipeline in cache.pipelines() {
        match &pipeline.state {
            CachedPipelineState::Ok(_) => ready += 1,
            CachedPipelineState::Queued | CachedPipelineState::Creating(_) => building += 1,
            CachedPipelineState::Err(err) => {
                if errored.len() < MAX_ERRORS {
                    let label = match &pipeline.descriptor {
                        PipelineDescriptor::RenderPipelineDescriptor(d) => d.label.as_deref(),
                        PipelineDescriptor::ComputePipelineDescriptor(d) => d.label.as_deref(),
                        PipelineDescriptor::MeshPipelineDescriptor(d) => d.label.as_deref(),
                    }
                    .unwrap_or("(unlabelled)");
                    // First line only: a naga validation error is a page, and the
                    // page is in the creation-time log; the report needs the *name*.
                    let first = err.to_string();
                    let first = first.lines().next().unwrap_or("").to_string();
                    errored.push(format!("{label}: {first}"));
                }
            }
        }
    }

    let health = HEALTH.get_or_init(Mutex::default);
    let mut guard = health
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    // Log each *new* error once, from here: bevy's own line named the shader at
    // creation time and is long gone from a phone's log ring by the time anyone
    // asks. Growth of the list is the edge; labels are stable.
    if errored.len() > guard.errored.len() {
        for e in &errored[guard.errored.len()..] {
            tracing::error!(pipeline = %e, "GPU pipeline failed to build — its pass is silently skipped");
        }
    }
    *guard = PipelineHealth {
        ready,
        building,
        errored,
    };
}

/// One pipeline for the `CARTO_PIPELINE_LOG` line: its label, then each stage's
/// shader and that stage's defs.
fn describe(descriptor: &PipelineDescriptor) -> String {
    use bevy::shader::{Shader, ShaderDefVal};
    fn stage(name: &str, shader: &Handle<Shader>, defs: &[ShaderDefVal]) -> String {
        let path = shader
            .path()
            .map_or_else(|| format!("{:?}", shader.id()), ToString::to_string);
        let defs: Vec<String> = defs
            .iter()
            .map(|d| match d {
                ShaderDefVal::Bool(k, true) => k.to_string(),
                ShaderDefVal::Bool(k, false) => format!("!{k}"),
                ShaderDefVal::Int(k, v) => format!("{k}={v}"),
                ShaderDefVal::UInt(k, v) => format!("{k}={v}"),
            })
            .collect();
        format!(" {name}={path} [{}]", defs.join(" "))
    }
    let label = |l: &Option<std::borrow::Cow<'static, str>>| {
        l.as_deref().unwrap_or("(unlabelled)").to_string()
    };
    match descriptor {
        PipelineDescriptor::RenderPipelineDescriptor(d) => {
            let mut out = label(&d.label) + &stage("vs", &d.vertex.shader, &d.vertex.shader_defs);
            if let Some(f) = &d.fragment {
                out += &stage("fs", &f.shader, &f.shader_defs);
            }
            out
        }
        PipelineDescriptor::ComputePipelineDescriptor(d) => {
            label(&d.label) + &stage("cs", &d.shader, &d.shader_defs)
        }
        PipelineDescriptor::MeshPipelineDescriptor(d) => {
            label(&d.label) + &stage("ms", &d.mesh.shader, &d.mesh.shader_defs)
        }
    }
}
