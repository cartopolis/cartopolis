//! The `terrain` stage: a cell's ground, flattened along its roads.
//!
//! The client fetches the height survey itself today — one large request per map
//! anchor, per device, per session — and then every surface it draws is bent onto
//! that survey against its own triangles. Two surfaces over one piece of ground
//! therefore disagree by the ground's curvature between their corners, which on a
//! handset reaches 2.8 m; that is the tearing.
//!
//! This does the fetch once and, while it is here, removes the cause: inside a
//! road corridor the survey is replaced by the smoothest surface that still meets
//! the measured ground at the corridor's edge ([`cartopolis_geo::road_grade`]).
//! Where the ground under a road has no curvature, nothing drawn on it can
//! disagree — including the pairs a cut can never reach, like a surface from the
//! neighbouring cell.

use anyhow::{Context, Result};
use std::sync::atomic::{AtomicUsize, Ordering};

use cartopolis_core::world::{self, TileKey};
use cartopolis_geo::height_survey::{self, HeightModel, HeightRequest};
use cartopolis_geo::road_grade;
use cartopolis_geo::terrain_file::{self, TerrainFile};
use cartopolis_geo::vector_tiles::{SurfaceGroup, TileData};

use crate::ground::{self, Cell};
use crate::{Progress, Tiles};

/// How far past the cell's own edge the survey is fetched.
///
/// A road runs out of the cell and its grading must not stop at the boundary, or
/// two neighbouring cells answer differently for the same metre of ground and the
/// seam is exactly the fault this removes. The margin is discarded before writing:
/// it exists to make the relaxation see the road's continuation, not to be stored.
const FETCH_MARGIN_M: f64 = 120.0;

/// Samples per side of the written cell. 513 over a ~1.5 km cell is 2.9 m ground,
/// a little finer than the client's own 3.125 m field and the same order as what
/// the surfaces can express.
pub const DEFAULT_GRID: u32 = 513;

/// Samples of margin to grow the road mark by before relaxing.
///
/// Eight at the grid above is ~23 m, so a 9 m carriageway relaxes inside a ~55 m
/// corridor. It is not a tuning constant: with the survey pinned at the kerb, both
/// kerbs are in whatever the road is crossing and the road can only reproduce it
/// (measured on a 4 m valley, 3.80 m of dip became 3.52 m). Pinned further out,
/// that band becomes the cutting or the embankment a built road actually has.
const ROAD_MARGIN_SAMPLES: usize = 8;

/// How far below the land the water surface is set, in metres.
///
/// **An approximation, and a deliberately shallow one.** The survey gets no return
/// off water, so after [`height_survey::fill_holes`] a canal sits at the level of
/// its own quays — two to five metres too high in a canal city, and with the water
/// drawn level it left the land standing above it with nothing between, which is
/// the open gap along every bank.
///
/// The true levels are a national dataset the client already reads
/// (`systems::water_levels`) and this stage does not yet. Until it does, dropping
/// the water a metre below the land it meets is wrong by less than leaving it at
/// quay height, and — the point — it is *continuous*: the ground descends into the
/// water across the mask edge, which is a bank.
const WATER_FREEBOARD_M: f32 = 1.0;

pub fn run(
    cells: &[Cell],
    tiles: &Tiles,
    out: &std::path::Path,
    grid: u32,
    jobs: usize,
    force: bool,
) -> Result<(usize, usize)> {
    let next = AtomicUsize::new(0);
    let done = AtomicUsize::new(0);
    let failed = AtomicUsize::new(0);
    let agent = ureq::AgentBuilder::new()
        .timeout_connect(std::time::Duration::from_secs(10))
        // A 513² coverage request is ~1 MB and the service has been measured at
        // 4.5 s for a 2049². Generous, because a slow answer is still an answer and
        // a re-run costs the whole cell.
        .timeout_read(std::time::Duration::from_secs(120))
        .build();

    std::thread::scope(|scope| {
        for _ in 0..jobs.max(1) {
            scope.spawn(|| loop {
                let i = next.fetch_add(1, Ordering::Relaxed);
                let Some(cell) = cells.get(i) else { return };
                let at = std::time::Instant::now();
                let dest = path_for(out, cell);

                if !force && std::fs::read(&dest).is_ok_and(|b| terrain_file::decode(&b).is_ok()) {
                    done.fetch_add(1, Ordering::Relaxed);
                    Progress::Skipped {
                        z: cell.z,
                        x: cell.x,
                        y: cell.y,
                    }
                    .emit();
                    continue;
                }

                match build_cell(cell, tiles, &agent, grid, &dest) {
                    Ok(report) => {
                        done.fetch_add(1, Ordering::Relaxed);
                        Progress::Terrain {
                            z: cell.z,
                            x: cell.x,
                            y: cell.y,
                            grid,
                            relief_m: report.relief_m,
                            road_samples: report.road_samples,
                            flattened: report.flattened,
                            no_data: report.no_data,
                            bytes: report.bytes,
                            millis: at.elapsed().as_millis(),
                        }
                        .emit();
                    }
                    Err(e) => {
                        failed.fetch_add(1, Ordering::Relaxed);
                        Progress::Failed {
                            z: cell.z,
                            x: cell.x,
                            y: cell.y,
                            error: format!("{e:#}"),
                        }
                        .emit();
                    }
                }
            });
        }
    });

    Ok((done.load(Ordering::Relaxed), failed.load(Ordering::Relaxed)))
}

#[must_use]
pub fn path_for(out: &std::path::Path, cell: &Cell) -> std::path::PathBuf {
    out.join(cell.z.to_string())
        .join(cell.x.to_string())
        .join(format!("{}.ctrn", cell.y))
}

struct Report {
    relief_m: f32,
    road_samples: usize,
    flattened: usize,
    no_data: usize,
    bytes: usize,
}

fn build_cell(
    cell: &Cell,
    tiles: &Tiles,
    agent: &ureq::Agent,
    grid: u32,
    dest: &std::path::Path,
) -> Result<Report> {
    // The cell's own corners in degrees, then in the national grid the survey
    // speaks. North-west and south-east, because tile y runs south.
    let (north, west) = world::tile_to_lat_lng(i64::from(cell.x), i64::from(cell.y), cell.z);
    let (south, east) =
        world::tile_to_lat_lng(i64::from(cell.x) + 1, i64::from(cell.y) + 1, cell.z);
    anyhow::ensure!(
        height_survey::covers(north, west) && height_survey::covers(south, east),
        "outside the height survey's coverage"
    );

    let nw = height_survey::wgs84_to_rd(north, west);
    let se = height_survey::wgs84_to_rd(south, east);
    let cell_min = [nw[0].min(se[0]), nw[1].min(se[1])];
    let cell_max = [nw[0].max(se[0]), nw[1].max(se[1])];

    // Fetched with a margin so a road leaving the cell is still graded against its
    // own continuation; the margin is cropped out before writing. The grid grows
    // with the box so the sample spacing is unchanged by the margin.
    #[allow(clippy::cast_precision_loss)]
    let span = (cell_max[0] - cell_min[0]).max(cell_max[1] - cell_min[1]);
    #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
    let fetch_grid = ((f64::from(grid) * (span + 2.0 * FETCH_MARGIN_M) / span).ceil() as u32) | 1;
    let req = HeightRequest {
        model: HeightModel::Terrain,
        min: cell_min,
        max: cell_max,
        grid,
    }
    .padded(FETCH_MARGIN_M);
    let req = HeightRequest {
        grid: fetch_grid.min(height_survey::MAX_GRID),
        ..req
    };

    let url = format!("{}{}", height_survey::AHN_ORIGIN, req.path());
    let resp = agent
        .get(&url)
        .call()
        .with_context(|| format!("fetching the height survey for {}/{}", cell.x, cell.y))?;
    let mut body = Vec::new();
    std::io::copy(&mut resp.into_reader(), &mut body).context("reading the height survey")?;
    let patch = height_survey::decode(&req, &body).map_err(anyhow::Error::msg)?;

    // The roads, from the same tessellation the surfaces stage uses — the drawn
    // ribbons rather than centre lines widened by a nominal number, so a junction
    // is marked as the shape it actually is.
    let tile_bytes = tiles.fetch(&TileKey::new(i64::from(cell.x), i64::from(cell.y), cell.z))?;
    let tile = TileData::parse(&tile_bytes).map_err(anyhow::Error::msg)?;
    let meshes = ground::build_meshes(&tile, cell.tile_m)?.meshes;

    let n = patch.w;
    let mut samples: Vec<f32> = patch.samples.clone();

    // **Close the survey's holes before anything reads it.** The laser gets no
    // return off water at all, so every canal arrives as a hole, and a hole is not
    // a height: the grading would relax the road down into it, and every surface
    // drawn over it would have nothing to stand on. `fill_holes` closes each one
    // with its own banks and smooths the crease that leaves.
    //
    // What water then sits at is the **quay level**, two to five metres above the
    // real surface in a canal city. That is a known approximation rather than an
    // oversight — the true levels are a separate national dataset the client
    // already reads (`systems::water_levels`) and this stage does not yet — and it
    // is the right failure while it lasts: too high and continuous beats absent.
    let before_fill = samples.iter().filter(|v| !v.is_finite()).count();
    cartopolis_geo::height_survey::fill_holes(&mut samples, n, None);
    let after_fill = samples.iter().filter(|v| !v.is_finite()).count();
    let mut road = vec![false; n * n];

    // The road triangles are in cell-local metres with +z running south; the grid
    // is in national-grid metres with north up. Mark in the grid's own frame by
    // converting each triangle, which is three points rather than a per-sample
    // transform.
    #[allow(clippy::cast_precision_loss)]
    let step_m = (patch.max[0] - patch.min[0]) as f32 / (n - 1) as f32;
    let tri_to_grid = |p: [f32; 3]| -> [f32; 2] {
        // Cell-local x east, z south → grid metres east, *south* from the patch's
        // north edge, which is the row order the survey arrives in.
        #[allow(clippy::cast_possible_truncation)]
        let origin_x = (cell_min[0] - patch.min[0]) as f32;
        #[allow(clippy::cast_possible_truncation)]
        let origin_z = (patch.max[1] - cell_max[1]) as f32;
        [origin_x + p[0], origin_z + p[2]]
    };
    let mut triangles: Vec<[[f32; 2]; 3]> = Vec::new();
    for m in &meshes {
        if !matches!(m.group, SurfaceGroup::Road(_) | SurfaceGroup::RoadCasing(_)) {
            continue;
        }
        for t in m.indices.chunks_exact(3) {
            triangles.push([
                tri_to_grid(m.positions[t[0] as usize]),
                tri_to_grid(m.positions[t[1] as usize]),
                tri_to_grid(m.positions[t[2] as usize]),
            ]);
        }
    }
    // **The water goes in before the roads are graded**, so a road running beside a
    // canal grades against the water's own level rather than against the quay it
    // has not been dropped from yet.
    let mut water = vec![false; n * n];
    let mut water_tris: Vec<[[f32; 2]; 3]> = Vec::new();
    for m in &meshes {
        if m.group != SurfaceGroup::Water {
            continue;
        }
        for t in m.indices.chunks_exact(3) {
            water_tris.push([
                tri_to_grid(m.positions[t[0] as usize]),
                tri_to_grid(m.positions[t[1] as usize]),
                tri_to_grid(m.positions[t[2] as usize]),
            ]);
        }
    }
    let mut water_samples = 0usize;
    if !water_tris.is_empty() {
        road_grade::mark_roads(&mut water, n, (0.0, 0.0), step_m, &water_tris, 0);
        // One level for all the water in the cell. Crude — a city can hold canals
        // at several levels, and this cannot tell them apart — but consistent,
        // which is what stops a gap: the water surface and the ground beneath it
        // are the same number, so the land descends into it rather than ending
        // above it.
        let mut around: Vec<f32> = water
            .iter()
            .zip(&samples)
            .filter(|(w, v)| **w && v.is_finite())
            .map(|(_, v)| *v)
            .collect();
        if !around.is_empty() {
            around.sort_by(f32::total_cmp);
            let level = around[around.len() / 2] - WATER_FREEBOARD_M;
            for (i, w) in water.iter().enumerate() {
                if *w {
                    samples[i] = level;
                    water_samples += 1;
                }
            }
        }
    }

    road_grade::mark_roads(
        &mut road,
        n,
        (0.0, 0.0),
        step_m,
        &triangles,
        ROAD_MARGIN_SAMPLES,
    );
    let road_samples = road.iter().filter(|m| **m).count();
    let flattened = road_grade::flatten(&mut samples, n, &road, road_grade::DEFAULT_PASSES);

    // Crop the margin away: it was fetched so the relaxation could see a road's
    // continuation, and storing it would overlap the neighbour's own answer.
    let cropped = crop(&samples, n, &patch, cell_min, cell_max, grid);
    let datum = median_finite(&cropped).unwrap_or(0.0);
    let relative: Vec<f32> = cropped.iter().map(|v| v - datum).collect();
    let no_data = relative.iter().filter(|v| !v.is_finite()).count();
    tracing::debug!(
        cell = ?(cell.x, cell.y),
        before_fill,
        after_fill,
        water_samples,
        "closed the survey's holes and set the water"
    );
    #[allow(clippy::cast_precision_loss)]
    let relief_m = {
        let finite: Vec<f32> = relative.iter().copied().filter(|v| v.is_finite()).collect();
        match (
            finite.iter().copied().fold(f32::INFINITY, f32::min),
            finite.iter().copied().fold(f32::NEG_INFINITY, f32::max),
        ) {
            (lo, hi) if lo.is_finite() && hi.is_finite() => hi - lo,
            _ => 0.0,
        }
    };

    let file = TerrainFile {
        z: cell.z,
        x: cell.x,
        y: cell.y,
        grid,
        datum,
        samples: relative,
    };
    let encoded = terrain_file::encode(&file);
    if let Some(dir) = dest.parent() {
        std::fs::create_dir_all(dir).with_context(|| format!("creating {}", dir.display()))?;
    }
    let tmp = dest.with_extension("ctrn.part");
    std::fs::write(&tmp, &encoded).with_context(|| format!("writing {}", tmp.display()))?;
    std::fs::rename(&tmp, dest).with_context(|| format!("renaming into {}", dest.display()))?;

    Ok(Report {
        relief_m,
        road_samples,
        flattened,
        no_data,
        bytes: encoded.len(),
    })
}

/// Resample the fetched box down to exactly the cell, at `grid` samples a side.
///
/// Nearest sample rather than bilinear: these are measurements and one of the four
/// neighbours is routinely a hole, so averaging would either spread the hole or
/// invent a height beside it. The fetch is deliberately a little finer than the
/// written grid so this is a decimation rather than a magnification.
fn crop(
    samples: &[f32],
    n: usize,
    patch: &cartopolis_geo::height_survey::HeightPatch,
    cell_min: [f64; 2],
    cell_max: [f64; 2],
    grid: u32,
) -> Vec<f32> {
    let g = grid as usize;
    let mut out = vec![f32::NAN; g * g];
    let (dx, dy) = (patch.max[0] - patch.min[0], patch.max[1] - patch.min[1]);
    if dx <= 0.0 || dy <= 0.0 || n < 2 {
        return out;
    }
    for z in 0..g {
        // Row 0 is the cell's north edge, matching the patch's own order.
        #[allow(clippy::cast_precision_loss)]
        let wy = cell_max[1] - (z as f64) / (g - 1) as f64 * (cell_max[1] - cell_min[1]);
        let fy = (patch.max[1] - wy) / dy;
        for x in 0..g {
            #[allow(clippy::cast_precision_loss)]
            let wx = cell_min[0] + (x as f64) / (g - 1) as f64 * (cell_max[0] - cell_min[0]);
            let fx = (wx - patch.min[0]) / dx;
            if !(0.0..=1.0).contains(&fx) || !(0.0..=1.0).contains(&fy) {
                continue;
            }
            #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
            let col = ((fx * (n - 1) as f64).round() as usize).min(n - 1);
            #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
            let row = ((fy * (n - 1) as f64).round() as usize).min(n - 1);
            out[z * g + x] = samples[row * n + col];
        }
    }
    out
}

/// The median of the finite samples — the cell's own reference level.
///
/// Median rather than mean for the reason the survey's own readers use it: a
/// handful of samples off a tower block or a crane drag a mean and leave a median
/// where the ground is.
fn median_finite(samples: &[f32]) -> Option<f32> {
    let mut v: Vec<f32> = samples.iter().copied().filter(|s| s.is_finite()).collect();
    if v.is_empty() {
        return None;
    }
    v.sort_by(f32::total_cmp);
    Some(v[v.len() / 2])
}
