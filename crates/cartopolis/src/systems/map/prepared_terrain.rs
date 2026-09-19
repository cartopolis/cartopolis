//! Ground heights the pipeline already worked out, instead of the survey.
//!
//! `carto-pipeline terrain` fetches the national height survey once, closes the
//! holes it has over water, and **flattens it along the roads** so that everything
//! drawn on that ground reads one number and no two surfaces can disagree about
//! where it is. This is the other end: where prepared cells exist, the client
//! assembles its height field out of them and never touches the survey.
//!
//! # What it replaces
//!
//! One 12 MB request to a national service per map anchor, per device, per
//! session, plus the bake that follows it. The bake is the smaller half — 4.5 s of
//! one measured capture — but the request is somebody's data allowance and, more
//! to the point, the *grading* only exists on the prepared side. A client baking
//! its own survey gets the ground the laser saw, bumps and all.
//!
//! # An override, never a requirement
//!
//! No source, a missing cell, a damaged one, or one written by a format this build
//! cannot read: all of them fall through to the survey exactly as before. Every
//! source this draws on is Dutch, so most of the world will never have prepared
//! cells, and a half-built city must degrade to the old behaviour rather than to a
//! hole.
//!
//! # Why its own little queue
//!
//! `platform::cell_source` is the shared seven steps for a per-cell layer and
//! parses **text**; these are binary rasters. The queue here is deliberately
//! smaller than that machinery rather than a second copy of it: one request per
//! missing cell, answered once, with the answer — including "there is none" —
//! remembered so a cell outside the prepared area is asked for once and not per
//! frame.

use std::collections::{HashMap, HashSet};
use std::sync::{Arc, Mutex};

use bevy::prelude::*;
use bevy::tasks::IoTaskPool;

use cartopolis_geo::terrain_file::{self, TerrainFile};

use crate::platform::http;
use crate::systems::geo_nav::WorldFrame;

/// A z14 cell, matching the pipeline's own output.
type Cell = (i64, i64);

/// The zoom prepared terrain is written at. It has to match
/// `carto-pipeline terrain --zoom`, and the two are the same constant the ground
/// cells use — a height file and a surface file describing different pieces of
/// ground would be worse than no height file at all.
const PREPARED_ZOOM: u32 = cartopolis_geo::vector_tiles::MAX_SOURCE_ZOOM;

/// Where prepared heights are, and what has come back.
#[derive(Resource, Default)]
pub struct PreparedTerrain {
    /// `{z}/{x}/{y}` template, or `None` to use the survey as before.
    source: Option<String>,
    /// Answered cells. `None` is a remembered *absence* — the cell is outside the
    /// prepared area, or its file was refused — and is what stops a request per
    /// frame for ground the pipeline has never covered.
    cells: HashMap<Cell, Option<TerrainFile>>,
    inflight: HashSet<Cell>,
    results: Arc<Mutex<Vec<(Cell, Option<TerrainFile>)>>>,
}

impl PreparedTerrain {
    /// Point the client at prepared heights. `CARTO_TERRAIN_URL` is read at
    /// startup; anything else is a caller's decision.
    pub fn set_source(&mut self, template: Option<String>) {
        if self.source != template {
            self.cells.clear();
            self.inflight.clear();
            self.source = template;
        }
    }

    #[must_use]
    pub fn is_enabled(&self) -> bool {
        self.source.is_some()
    }

    /// How many cells have been answered, and how many of those had a file. The
    /// pair, because "the source is configured" and "the source covers this
    /// ground" are different claims and only both together tell them apart.
    #[must_use]
    pub fn counts(&self) -> (usize, usize) {
        (
            self.cells.len(),
            self.cells.values().filter(|c| c.is_some()).count(),
        )
    }
}

/// Drain finished fetches. Cheap enough to run every frame and does nothing at all
/// with no source configured.
pub fn poll_prepared_terrain(mut prepared: ResMut<PreparedTerrain>) {
    let drained: Vec<(Cell, Option<TerrainFile>)> = match prepared.results.lock() {
        Ok(mut g) if !g.is_empty() => std::mem::take(&mut *g),
        _ => return,
    };
    for (cell, file) in drained {
        prepared.inflight.remove(&cell);
        prepared.cells.insert(cell, file);
    }
}

/// The cells a box needs, queueing any that are missing.
///
/// `None` until every one of them has been answered — a field assembled from half
/// its cells would have a cliff down the middle, and the survey it replaces
/// arrives all at once or not at all.
pub fn cells_for(
    prepared: &mut PreparedTerrain,
    anchor: (f64, f64, u32),
    half_m: f32,
) -> Option<HashMap<Cell, TerrainFile>> {
    let template = prepared.source.clone()?;
    let frame = WorldFrame::new(anchor);
    // The box's four corners are enough: a tile grid is axis-aligned in tile
    // space, so the corners bound every cell between them.
    let mut lo = (i64::MAX, i64::MAX);
    let mut hi = (i64::MIN, i64::MIN);
    for (dx, dz) in [
        (-half_m, -half_m),
        (half_m, -half_m),
        (-half_m, half_m),
        (half_m, half_m),
    ] {
        let (lat, lng) = (frame.lat_at(f64::from(dz)), frame.lng_at(f64::from(dx)));
        let (x, y) = cartopolis_core::world::lat_lng_to_tile(lat, lng, PREPARED_ZOOM);
        lo = (lo.0.min(x), lo.1.min(y));
        hi = (hi.0.max(x), hi.1.max(y));
    }

    let mut out = HashMap::new();
    let mut waiting = false;
    for y in lo.1..=hi.1 {
        for x in lo.0..=hi.0 {
            let cell = (x, y);
            match prepared.cells.get(&cell) {
                Some(Some(file)) => {
                    out.insert(cell, file.clone());
                }
                // Answered, and there is nothing there. Not a reason to wait.
                Some(None) => {}
                None => {
                    waiting = true;
                    queue(prepared, &template, cell);
                }
            }
        }
    }
    if waiting {
        return None;
    }
    // Every cell answered and none of them had a file: this ground is not prepared,
    // so say so rather than handing back an empty field that would read as flat.
    if out.is_empty() {
        return None;
    }
    Some(out)
}

fn queue(prepared: &mut PreparedTerrain, template: &str, cell: Cell) {
    if !prepared.inflight.insert(cell) {
        return;
    }
    let url = template
        .replace("{z}", &PREPARED_ZOOM.to_string())
        .replace("{x}", &cell.0.to_string())
        .replace("{y}", &cell.1.to_string());
    let sink = prepared.results.clone();
    IoTaskPool::get()
        .spawn(async move {
            let file = match http::fetch(http::Request::get(url.clone())).await {
                Ok(bytes) => match terrain_file::decode(&bytes) {
                    Ok(f) => Some(f),
                    Err(e) => {
                        tracing::warn!(url = %url, error = %e, "prepared terrain refused");
                        None
                    }
                },
                // A miss at the edge of a prepared area is the ordinary case and
                // not worth a line per cell.
                Err(_) => None,
            };
            if let Ok(mut g) = sink.lock() {
                g.push((cell, file));
            }
        })
        .detach();
}

/// Assemble a field's samples out of prepared cells.
///
/// Returns the same shape the survey bake does — samples relative to the anchor's
/// own ground, that ground's absolute level, and a count of what was filled — so
/// the caller substitutes one for the other and nothing downstream knows.
///
/// A sample with no prepared cell under it, or one the cell has no reading for,
/// comes back `NaN` and is counted. The caller treats those exactly as the survey's
/// own holes.
#[must_use]
pub fn assemble(
    cells: &HashMap<Cell, TerrainFile>,
    anchor: (f64, f64, u32),
    half_m: f32,
    grid: usize,
) -> Option<(Vec<f32>, f32, usize)> {
    if grid < 2 {
        return None;
    }
    let frame = WorldFrame::new(anchor);
    let n = f64::from(1u32 << PREPARED_ZOOM);
    let step = f64::from(half_m) * 2.0 / (grid - 1) as f64;

    let mut samples = vec![f32::NAN; grid * grid];
    let mut missing = 0usize;
    for z in 0..grid {
        let wz = -f64::from(half_m) + z as f64 * step;
        let lat = frame.lat_at(wz);
        for x in 0..grid {
            let wx = -f64::from(half_m) + x as f64 * step;
            let lng = frame.lng_at(wx);
            let (fx, fy) = cartopolis_core::world::lat_lng_to_tile_frac(lat, lng, PREPARED_ZOOM);
            let _ = n;
            let cell = (fx.floor() as i64, fy.floor() as i64);
            let Some(file) = cells.get(&cell) else {
                missing += 1;
                continue;
            };
            #[allow(clippy::cast_possible_truncation)]
            match file.at_fraction((fx - fx.floor()) as f32, (fy - fy.floor()) as f32) {
                // Back to an absolute level: each cell carries its own reference,
                // and two neighbouring cells need not share one.
                Some(v) => samples[z * grid + x] = v + file.datum,
                None => missing += 1,
            }
        }
    }

    // The anchor's own ground is the datum, so the world's existing `y = 0` keeps
    // meaning "here" — the same rule the survey bake follows, for the same reason.
    let centre = samples[(grid / 2) * grid + grid / 2];
    let datum = if centre.is_finite() {
        centre
    } else {
        let mut finite: Vec<f32> = samples.iter().copied().filter(|v| v.is_finite()).collect();
        if finite.is_empty() {
            return None;
        }
        finite.sort_by(f32::total_cmp);
        finite[finite.len() / 2]
    };
    for v in &mut samples {
        if v.is_finite() {
            *v -= datum;
        }
    }
    Some((samples, datum, missing))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn flat_cell(x: u32, y: u32, height: f32, datum: f32) -> TerrainFile {
        TerrainFile {
            z: PREPARED_ZOOM,
            x,
            y,
            grid: 4,
            datum,
            samples: vec![height; 16],
        }
    }

    /// **Two cells with different reference levels must not step at the seam.**
    /// Each file's heights are relative to its own datum, so adding it back is
    /// what makes neighbours comparable — and forgetting to would put a cliff
    /// down every cell boundary, which is exactly the fault this whole area is
    /// about.
    #[test]
    fn neighbouring_cells_are_brought_to_one_level() {
        let anchor = (53.2194, 6.5665, 14);
        let (x, y) = cartopolis_core::world::lat_lng_to_tile(anchor.0, anchor.1, PREPARED_ZOOM);
        let mut cells = HashMap::new();
        // Same real height, stated against two different references.
        for dx in -1..=1 {
            for dy in -1..=1 {
                #[allow(clippy::cast_sign_loss, clippy::cast_possible_truncation)]
                let (cx, cy) = ((x + dx) as u32, (y + dy) as u32);
                let datum = if dx < 0 { 3.0 } else { 7.0 };
                let rel = 5.0 - datum;
                cells.insert(
                    (i64::from(cx), i64::from(cy)),
                    flat_cell(cx, cy, rel, datum),
                );
            }
        }
        let (samples, datum, missing) = assemble(&cells, anchor, 300.0, 33).expect("assembled");
        assert_eq!(missing, 0, "every sample had a cell");
        assert!((datum - 5.0).abs() < 1.0e-3, "datum {datum}");
        for v in &samples {
            assert!(v.abs() < 1.0e-3, "a seam of {v} m between cells");
        }
    }

    /// Ground the pipeline has not covered comes back as a hole rather than as
    /// zero. Zero is a *height*, and a caller that took it for one would draw a
    /// cliff at the edge of the prepared area.
    #[test]
    fn ground_with_no_prepared_cell_is_a_hole() {
        let anchor = (53.2194, 6.5665, 14);
        let (x, y) = cartopolis_core::world::lat_lng_to_tile(anchor.0, anchor.1, PREPARED_ZOOM);
        let mut cells = HashMap::new();
        #[allow(clippy::cast_sign_loss, clippy::cast_possible_truncation)]
        cells.insert((x, y), flat_cell(x as u32, y as u32, 0.0, 2.0));
        // Far wider than the one cell supplied.
        let (samples, _, missing) = assemble(&cells, anchor, 4000.0, 33).expect("assembled");
        assert!(missing > 0, "nothing was reported missing");
        assert!(
            samples.iter().any(|v| !v.is_finite()),
            "a missing sample came back as a number"
        );
    }

    /// A fresh client uses the survey, and says so.
    #[test]
    fn no_source_means_the_survey() {
        let mut p = PreparedTerrain::default();
        assert!(!p.is_enabled());
        assert_eq!(cells_for(&mut p, (53.2194, 6.5665, 14), 500.0), None);
        p.set_source(Some("https://example.test/{z}/{x}/{y}.ctrn".into()));
        assert!(p.is_enabled());
        assert_eq!(p.counts(), (0, 0));
    }
}
