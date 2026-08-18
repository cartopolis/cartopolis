//! How many bytes this frame handed the GPU, which is the number every
//! device-loss report so far has been missing.
//!
//! **Why bytes and not counts.** Every streamer in this client is already bounded
//! per frame — `map_stream::MAX_UPLOADS_PER_FRAME`, `lod22::MAX_BUILDS_PER_FRAME`,
//! `globe::MAX_GLOBE_TILE_BUILDS_PER_FRAME`, the OSM patch cap — and each of those
//! bounds a **count of items**. The items are not the same size, and not within an
//! order of magnitude of each other: a 256² tile mip chain is ~350 kB, a LoD2.2
//! cell is a whole z14 triangle soup, an OSM detail patch is thousands of extruded
//! footprints. "Two builds this frame" can mean 700 kB or 40 MB, and the number
//! that decides whether a mobile driver's command buffer survives the frame is the
//! second one. Nothing in this process measured it, which is why every account of
//! the device losses — including the ones in `docs/notes/android-build.md` —
//! reasons about *counts* and then guesses about bytes.
//!
//! **What it measures.** Bytes of `Image` and `Mesh` assets added or modified in
//! the main world this frame, by exactly the rule bevy's own upload throttle uses
//! (`RenderAsset::byte_len` for `GpuImage` and `RenderMesh`), so this number and
//! `quality::Budget::upload_bytes_per_frame` are in the same units and can be
//! compared directly. It is what was *queued*: bevy uploads it during the next
//! extract, spread over frames if a budget is set. That is the honest thing to
//! measure here — the queue is what the client controls, and the spreading is what
//! the budget does about it.
//!
//! **What it is not.** Not a GPU measurement. It says nothing about pipeline
//! compilation, binning or overdraw, and a frame can hang on any of those with
//! this reading at zero. It is one term of the budget, and the one that was dark.
//!
//! Free enough to run in every build (a message drain and a hash lookup per asset,
//! on frames where assets landed at all) — and running everywhere is the point,
//! since the machine that needs the number is the one nobody can attach a profiler
//! to. It reaches a report three ways: the `slow frame` warning names it, so the
//! line that already fires on a hitch says how much of it was upload;
//! `ShotMetrics::upload_peak_kb` carries the run's worst frame into `--csv` /
//! `--dump-state`, where `--expect` can gate on it; and the ten-second asset
//! census logs it alongside the asset counts it already prints.

use bevy::asset::AssetEvent;
use bevy::prelude::*;

/// Bytes queued for upload, this frame and at the worst frame so far.
#[derive(Resource, Default, Debug)]
pub struct UploadMeter {
    /// Queued during the frame that just ran.
    pub frame_bytes: usize,
    /// The largest single frame since launch. This is the number a device loss is
    /// about: a run that averages 2 MB/frame and spikes to 60 MB once is a run
    /// that loses the device once.
    pub peak_frame_bytes: usize,
    /// Everything queued since launch, for "did we upload the city twice".
    pub total_bytes: u64,
    /// The largest *single* asset queued this run.
    ///
    /// Tracked apart from [`Self::peak_frame_bytes`] because it is the one number
    /// no throttle downstream can do anything about: bevy's
    /// `RenderAssetBytesPerFrame` always writes at least one whole asset per frame
    /// so that uploads can never stall, so an asset larger than the budget *is* a
    /// frame larger than the budget. It is therefore the quantity to gate on —
    /// `lod22::MAX_CELL_MESH_BYTES` exists to hold it down, and this is what says
    /// whether it did.
    pub peak_asset_bytes: usize,
    /// Meshes this run whose bytes could not be counted because bevy had already
    /// moved the data to the render world.
    ///
    /// Present so the reading cannot lie by omission. A mesh created with
    /// `RenderAssetUsages::RENDER_WORLD` (the transit and vehicle meshes here) has
    /// its attributes *taken* at extraction, and anything reading them afterwards
    /// panics outright — which is what the first version of this module did. The
    /// measurement is arranged so this stays zero (see [`measure_uploads`]); a
    /// non-zero count means the schedule moved and the totals below are a floor,
    /// not a figure.
    pub unmeasured_meshes: usize,
}

impl UploadMeter {
    /// The worst frame so far, in kB — the unit the metrics and the log lines use,
    /// because a byte count of a mip chain is six digits of noise.
    #[must_use]
    pub fn peak_kb(&self) -> usize {
        self.peak_frame_bytes / 1024
    }

    /// This frame, in kB.
    #[must_use]
    pub fn frame_kb(&self) -> usize {
        self.frame_bytes / 1024
    }

    /// The largest single asset of the run, in kB.
    #[must_use]
    pub fn peak_asset_kb(&self) -> usize {
        self.peak_asset_bytes / 1024
    }
}

/// A frame that queues more than this logs a line of its own.
///
/// Not a budget and not enforcement — [`crate::quality::Budget::upload_bytes_per_frame`]
/// is both of those. This is the threshold above which a frame is worth a sentence
/// in a log somebody will read on a phone, and it is deliberately well under the
/// bursts that have actually lost a device (tens of MB) so the line appears
/// *before* the crash it explains rather than only in the frame that dies.
const NOTABLE_FRAME_BYTES: usize = 4 * 1024 * 1024;

/// Sum this frame's new image and mesh bytes into [`UploadMeter`].
///
/// **Runs in `Last`, and has to.** A mesh whose `RenderAssetUsages` omits
/// `MAIN_WORLD` has its vertex data *moved out* at extraction, and every accessor
/// for it panics from then on (`Mesh::attributes` is an `expect`, which is how the
/// first version of this took the app down on its first run). Extraction happens
/// after the whole main schedule, so `Last` is the point where every asset created
/// this frame is both *visible* — the streamers finalise their tiles and cells
/// throughout `Update` — and *still readable*. In `Update` it would be neither
/// reliably: events from systems ordered after it arrive a frame late, by which
/// time the data they describe is gone.
///
/// The `Last` placement also lines the reading up with `frame_time_watchdog`,
/// which reads it from `Update` on the *next* frame. That is not a lag: the
/// watchdog's own `Time<Real>::delta` is likewise the duration of the frame that
/// just ended, so the bytes and the milliseconds it logs together describe the
/// same frame.
///
/// `Added` **and** `Modified`: a modified image is re-uploaded in full (bevy's
/// `prepare_asset` writes the whole texture again), and the map's re-anchor path
/// rewrites meshes in place rather than replacing them — counting only `Added`
/// would report zero for exactly the burst a re-anchor causes, which is one of the
/// two bursts on record.
pub fn measure_uploads(
    mut meter: ResMut<UploadMeter>,
    mut image_events: MessageReader<AssetEvent<Image>>,
    mut mesh_events: MessageReader<AssetEvent<Mesh>>,
    images: Res<Assets<Image>>,
    meshes: Res<Assets<Mesh>>,
) {
    let mut bytes = 0usize;
    for event in image_events.read() {
        let (AssetEvent::Added { id } | AssetEvent::Modified { id }) = *event else {
            continue;
        };
        // `data` is `None` for a render target or a texture bevy fills on the GPU;
        // nothing is uploaded for those, which is also what `byte_len` answers.
        let asset = images
            .get(id)
            .and_then(|image| image.data.as_ref().map(Vec::len))
            .unwrap_or(0);
        meter.peak_asset_bytes = meter.peak_asset_bytes.max(asset);
        bytes += asset;
    }
    let mut unmeasured = 0usize;
    for event in mesh_events.read() {
        let (AssetEvent::Added { id } | AssetEvent::Modified { id }) = *event else {
            continue;
        };
        let Some(mesh) = meshes.get(id) else {
            continue;
        };
        match mesh_bytes(mesh) {
            Some(n) => {
                meter.peak_asset_bytes = meter.peak_asset_bytes.max(n);
                bytes += n;
            }
            None => unmeasured += 1,
        }
    }
    meter.unmeasured_meshes += unmeasured;

    meter.frame_bytes = bytes;
    meter.peak_frame_bytes = meter.peak_frame_bytes.max(bytes);
    meter.total_bytes += bytes as u64;

    if bytes >= NOTABLE_FRAME_BYTES {
        tracing::warn!(
            frame_kb = bytes / 1024,
            peak_kb = meter.peak_kb(),
            peak_asset_kb = meter.peak_asset_kb(),
            "large GPU upload queued in one frame — this is the burst that loses a \
             mobile device (quality::Budget::upload_bytes_per_frame spreads it)"
        );
    }
}

/// What a mesh costs to upload: vertex buffer plus index buffer, or `None` if its
/// data has already gone to the render world and cannot be read.
///
/// Deliberately the same arithmetic as `RenderMesh::byte_len` (bevy_render's
/// `mesh/mod.rs`) rather than an approximation of it: `size_of(vertex layout) ×
/// vertices + index bytes`. If the two drift, this module reports a number the
/// throttle does not act on, which is worse than having neither.
///
/// Every accessor is the `try_` one. The plain `Mesh::attributes`,
/// `count_vertices` and `get_index_buffer_bytes` are all `expect(…)` on the
/// extracted case, and a diagnostic that can panic is not a diagnostic — least of
/// all this one, whose whole job is to survive the frame that goes wrong. The
/// vertex count is derived here rather than through `count_vertices` for the same
/// reason, and takes the minimum over attributes exactly as bevy does.
fn mesh_bytes(mesh: &Mesh) -> Option<usize> {
    let mut vertex_size = 0usize;
    let mut vertices: Option<usize> = None;
    for (attribute, values) in mesh.try_attributes().ok()? {
        vertex_size += attribute.format.size() as usize;
        vertices = Some(vertices.map_or(values.len(), |n: usize| n.min(values.len())));
    }
    let index_bytes = match mesh.try_indices_option().ok()? {
        Some(bevy::mesh::Indices::U16(indices)) => indices.len() * 2,
        Some(bevy::mesh::Indices::U32(indices)) => indices.len() * 4,
        None => 0,
    };
    Some(vertex_size * vertices.unwrap_or(0) + index_bytes)
}

#[cfg(test)]
mod tests {
    use super::*;
    use bevy::asset::RenderAssetUsages;
    use bevy::mesh::PrimitiveTopology;

    /// The meter's arithmetic is bevy's, and the test states it in numbers so a
    /// bevy upgrade that changes the vertex layout shows up here rather than as a
    /// budget that quietly means something else.
    #[test]
    fn a_mesh_costs_its_vertex_and_index_buffers() {
        let mut mesh = Mesh::new(
            PrimitiveTopology::TriangleList,
            RenderAssetUsages::default(),
        );
        // Position and normal are 3×f32 each = 24 B per vertex, over four vertices.
        mesh.insert_attribute(
            Mesh::ATTRIBUTE_POSITION,
            vec![
                [0.0f32, 0.0, 0.0],
                [1.0, 0.0, 0.0],
                [1.0, 1.0, 0.0],
                [0.0, 1.0, 0.0],
            ],
        );
        mesh.insert_attribute(Mesh::ATTRIBUTE_NORMAL, vec![[0.0f32, 0.0, 1.0]; 4]);
        mesh.insert_indices(bevy::mesh::Indices::U32(vec![0, 1, 2, 0, 2, 3]));
        assert_eq!(mesh_bytes(&mesh), Some(4 * 24 + 6 * 4));
    }

    /// A mesh with no indices is not a mesh with no cost — the surfaces earcut
    /// emits are exactly this shape.
    #[test]
    fn an_unindexed_mesh_still_costs_its_vertices() {
        let mut mesh = Mesh::new(
            PrimitiveTopology::TriangleList,
            RenderAssetUsages::default(),
        );
        mesh.insert_attribute(Mesh::ATTRIBUTE_POSITION, vec![[0.0f32, 0.0, 0.0]; 3]);
        assert_eq!(mesh_bytes(&mesh), Some(3 * 12));
    }

    /// The peak is a high-water mark and never falls back with a quiet frame; the
    /// whole point is to survive the thousand frames after the burst.
    #[test]
    fn the_peak_outlives_a_quiet_frame() {
        let mut meter = UploadMeter::default();
        for bytes in [1024, 40 * 1024 * 1024, 0] {
            meter.frame_bytes = bytes;
            meter.peak_frame_bytes = meter.peak_frame_bytes.max(bytes);
            meter.total_bytes += bytes as u64;
        }
        assert_eq!(meter.frame_kb(), 0);
        assert_eq!(meter.peak_kb(), 40 * 1024);
        assert_eq!(meter.total_bytes, 40 * 1024 * 1024 + 1024);
    }
}
