//! Overrides of Bevy's own render setup, each one a guard against a device loss:
//! the cap on the GPU-driven draw path and the forced CPU light clustering.

use bevy::prelude::*;

use crate::env_value;

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
pub(crate) struct GpuPreprocessingOverridePlugin;

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
pub(crate) struct ForceCpuClusteringPlugin;

/// Cap Bevy's mesh slabs at [`MAX_MESH_SLAB_BYTES`].
///
/// **Why.** Bevy packs every mesh's vertices and indices into shared GPU buffers
/// ("slabs") that grow by 1.5× up to 512 MiB, and each growth allocates the new
/// buffer and copies the old one into it, so for a moment both are resident. A
/// streamed city fills those slabs fast. On the Steam Deck — 1 GiB of VRAM, most
/// of it already taken when a second client is running — a capture over Groningen
/// froze for good, every thread parked, 0 % CPU, with the render thread's
/// `Queue::submit` from `AllocationStage::commit` sitting in a RADV ioctl and the
/// image uploads queued behind wgpu's queue lock (gdb, 2026-10-09). The same view
/// froze on main and on the web-fps branch, i.e. it predates both. A cap keeps
/// each growth, and so each copy and each eviction the kernel has to make room
/// for, small. The cost is more slabs and so more vertex-buffer rebinds between
/// batches, which at 64 MiB is a handful for a whole city.
pub(crate) struct MeshSlabCapPlugin;

/// The largest mesh slab, and the size above which a mesh gets a slab of its own.
const MAX_MESH_SLAB_BYTES: u64 = 64 * 1024 * 1024;

impl Plugin for MeshSlabCapPlugin {
    fn build(&self, app: &mut App) {
        use bevy::render::mesh::allocator::MeshAllocatorSettings;
        use bevy::render::RenderApp;

        let Some(render_app) = app.get_sub_app_mut(RenderApp) else {
            return;
        };
        let mut settings = MeshAllocatorSettings::default();
        settings.max_slab_size = MAX_MESH_SLAB_BYTES;
        settings.large_threshold = MAX_MESH_SLAB_BYTES / 2;
        render_app.insert_resource(settings);
    }
}

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

/// A name for a [`GpuPreprocessingMode`], which upstream derives no `Debug` for.
pub(crate) fn gpu_preprocessing_name(
    mode: bevy::render::batching::gpu_preprocessing::GpuPreprocessingMode,
) -> &'static str {
    use bevy::render::batching::gpu_preprocessing::GpuPreprocessingMode as M;
    match mode {
        M::None => "cpu (no preprocessing)",
        M::PreprocessingOnly => "gpu preprocessing, direct draws",
        M::Culling => "gpu preprocessing and gpu culling (indirect draws)",
    }
}
