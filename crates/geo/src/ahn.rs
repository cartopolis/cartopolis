//! Measured Dutch ground, from AHN, without an engine around it.
//!
//! The client fetches this through its own async plumbing and a Bevy resource
//! (`systems::map::height`). The **harness cannot**: it does not depend on the
//! client, and the whole point of it is to look at the ground in seconds. So the
//! two halves that are not about scheduling — what to ask AHN for, and how to
//! read the answer — live here and are shared.
//!
//! Everything here is pure and blocking. There is no cache, no retry and no task
//! pool: one request, one raster. The client keeps all of that, because it is
//! streaming while somebody flies a camera; a harness run asks once and exits.

/// Ground metres between samples in the client's coarse height field (3.125 m).
///
/// **The resolution is what decides whether a slope is visible, and a range
/// cannot tell you.** Measured over Groningen: each halving of the spacing roughly
/// doubles the median facet tilt, and at 25 m the Hondsrug's full 7 m runs through
/// the middle of the city shaded within ±4 % of flat — present, correct and
/// invisible.
///
/// It lives here because two programs stand meshes on this ground and they must
/// resolve it alike. A harness asking for a coarser raster than the client bakes
/// does not draw a smoother version of the same city; it draws a *different* one,
/// with a trench aliased into folds and creases the client never has — which is
/// the harness reporting a fault that belongs to its own instrument
/// (measured 2026-09-21, the Groningen ring at 5.73 m against 3.125 m).
/// `systems::terrain::TERRAIN_SAMPLE_M` is pinned to this by a test.
pub const SAMPLE_M: f32 = 3.125;

/// WGS84 → **RD New** (EPSG:28992), the Dutch national grid, in metres.
///
/// The published polynomial about the Amersfoort datum point — a few centimetres
/// inside the Netherlands and meaningless outside it, which is exactly AHN's own
/// domain. Duplicated from nothing: the client re-exports this one.
#[must_use]
#[allow(clippy::unreadable_literal)]
pub fn wgs84_to_rd(lat: f64, lng: f64) -> [f64; 2] {
    let d_lat = 0.36 * (lat - 52.15517440);
    let d_lng = 0.36 * (lng - 5.38720621);
    let x = 155000.0 + 190094.945 * d_lng
        - 11832.228 * d_lat * d_lng
        - 114.221 * d_lng * d_lat * d_lat
        - 32.391 * d_lng * d_lng * d_lng
        - 0.705 * d_lat
        - 2.340 * d_lat * d_lat * d_lat * d_lng
        - 0.608 * d_lat * d_lng * d_lng * d_lng
        - 0.008 * d_lng * d_lng
        + 0.148 * d_lat * d_lat * d_lat * d_lng * d_lng * d_lng;
    let y = 463000.0 + 309056.544 * d_lat + 3638.893 * d_lng * d_lng + 73.077 * d_lat * d_lat
        - 157.984 * d_lat * d_lng * d_lng
        + 59.788 * d_lat * d_lat * d_lat
        + 0.433 * d_lng
        - 6.439 * d_lat * d_lat * d_lng * d_lng
        - 0.032 * d_lat * d_lng
        + 0.092 * d_lng * d_lng * d_lng * d_lng
        - 0.054 * d_lat * d_lng * d_lng * d_lng * d_lng;
    [x, y]
}

/// The WCS request for one square of ground, in RD metres.
///
/// Built as a whole URL rather than pieces, so a harness can print it and a
/// person can paste it into a browser — which is how a refusal gets diagnosed.
#[must_use]
pub fn coverage_url(min: [f64; 2], max: [f64; 2], grid: u32) -> String {
    coverage_url_of(DTM, min, max, grid)
}

/// AHN's bare ground: buildings and bridge decks cut out.
pub const DTM: &str = "dtm_05m";
/// AHN's surface: everything the laser hit, bridge decks at their real height.
pub const DSM: &str = "dsm_05m";

/// [`coverage_url`] for either model ([`DTM`], [`DSM`]).
#[must_use]
pub fn coverage_url_of(coverage: &str, min: [f64; 2], max: [f64; 2], grid: u32) -> String {
    format!(
        "https://service.pdok.nl/rws/ahn/wcs/v1_0?SERVICE=WCS&VERSION=1.0.0\
         &REQUEST=GetCoverage&COVERAGE={coverage}&CRS=EPSG:28992&FORMAT=GEOTIFF\
         &BBOX={:.2},{:.2},{:.2},{:.2}&WIDTH={grid}&HEIGHT={grid}",
        min[0], min[1], max[0], max[1]
    )
}

/// Heights above the service's own no-data floor are holes, not ground.
const NODATA_ABOVE: f32 = 1.0e30;

/// Read a float GeoTIFF into `grid²` samples, `NaN` where the service has no data.
///
/// **An OGC service answers a refused request with an XML exception and HTTP
/// 200**, so "not a TIFF" is the ordinary error path rather than a corruption
/// case, and the message has to carry what the service actually said.
pub fn decode_float_tiff(bytes: &[u8], grid: usize) -> Result<Vec<f32>, String> {
    if bytes.len() < 4 || !(bytes.starts_with(b"II*\0") || bytes.starts_with(b"MM\0*")) {
        let text = String::from_utf8_lossy(&bytes[..bytes.len().min(400)]).to_string();
        return Err(format!("not a GeoTIFF: {}", text.trim()));
    }
    let mut decoder = tiff::decoder::Decoder::new(std::io::Cursor::new(bytes))
        .map_err(|e| format!("tiff open: {e}"))?;
    let (w, h) = decoder
        .dimensions()
        .map_err(|e| format!("tiff dims: {e}"))?;
    if w as usize != grid || h as usize != grid {
        return Err(format!("asked for {grid}², got {w}x{h}"));
    }
    let image = decoder
        .read_image()
        .map_err(|e| format!("tiff read: {e}"))?;
    let tiff::decoder::DecodingResult::F32(mut v) = image else {
        return Err("height raster is not float32".to_string());
    };
    for s in &mut v {
        if !s.is_finite() || *s >= NODATA_ABOVE {
            *s = f32::NAN;
        }
    }
    Ok(v)
}

/// AHN's own envelope in RD metres — outside it there is no data to ask for.
#[must_use]
pub fn covers(lat: f64, lng: f64) -> bool {
    let [x, y] = wgs84_to_rd(lat, lng);
    (0.0..=290000.0).contains(&x) && (300000.0..=630000.0).contains(&y)
}

/// How far [`fill_holes`] reaches from the nearest measurement, in metres.
///
/// A building footprint or a canal is tens of metres across and is invented ground
/// either way; a lake is hundreds, and inventing the middle of one is worse than
/// leaving it at the anchor's level.
pub const FILL_REACH_M: f32 = 150.0;

/// Relaxation passes over the cells [`fill_holes`] invented.
const FILL_SMOOTH_PASSES: usize = 4;

/// Close no-data holes out to `reach_cells` from the nearest measurement, in two
/// sweeps plus a short relaxation. `None` reaches as far as it takes.
///
/// A chamfer distance transform carrying the nearest measured value (two sweeps,
/// O(n)), then a few Jacobi passes restricted to the cells that were filled. The
/// transform alone leaves a ridge along the Voronoi boundary down the middle of
/// anything wide, and the relaxation is what turns that crease into a smooth
/// interpolation. Measurements are never written, only holes.
///
/// **The reach is in cells and every caller derives it from metres**, because this
/// replaced a fixed count of one-cell dilations that was documented as "~150 m" and
/// was only ever 150 m at the one sampling it was written against. A fill whose reach
/// is a unit mistake away from the resolution is how a change to the field's grid
/// leaves every building footprint an unfilled pit — and the pass-count form could
/// not be scaled to the fine tier at all, which would have wanted ~300 passes over a
/// million samples.
pub fn fill_holes(grid: &mut [f32], n: usize, reach_cells: Option<f32>) {
    if grid.iter().all(|v| v.is_finite()) {
        return;
    }
    // `best` is the squared chamfer distance to the nearest measurement; `FAR` is "no
    // source seen yet" and must be finite, because it is compared and added to.
    const FAR: i32 = i32::MAX / 4;
    const ORTHO: i32 = 3;
    const DIAG: i32 = 4;
    let mut best = vec![FAR; grid.len()];
    let mut val = vec![0.0f32; grid.len()];
    let filled: Vec<bool> = grid.iter().map(|v| !v.is_finite()).collect();
    for i in 0..grid.len() {
        if grid[i].is_finite() {
            best[i] = 0;
            val[i] = grid[i];
        }
    }
    let relax =
        |x: usize, z: usize, nx: i32, nz: i32, w: i32, best: &mut [i32], val: &mut [f32]| {
            if nx < 0 || nz < 0 || nx >= n as i32 || nz >= n as i32 {
                return;
            }
            let from = nz as usize * n + nx as usize;
            let to = z * n + x;
            let cand = best[from].saturating_add(w);
            if cand < best[to] {
                best[to] = cand;
                val[to] = val[from];
            }
        };
    for z in 0..n {
        for x in 0..n {
            let (xi, zi) = (x as i32, z as i32);
            relax(x, z, xi - 1, zi, ORTHO, &mut best, &mut val);
            relax(x, z, xi, zi - 1, ORTHO, &mut best, &mut val);
            relax(x, z, xi - 1, zi - 1, DIAG, &mut best, &mut val);
            relax(x, z, xi + 1, zi - 1, DIAG, &mut best, &mut val);
        }
    }
    for z in (0..n).rev() {
        for x in (0..n).rev() {
            let (xi, zi) = (x as i32, z as i32);
            relax(x, z, xi + 1, zi, ORTHO, &mut best, &mut val);
            relax(x, z, xi, zi + 1, ORTHO, &mut best, &mut val);
            relax(x, z, xi + 1, zi + 1, DIAG, &mut best, &mut val);
            relax(x, z, xi - 1, zi + 1, DIAG, &mut best, &mut val);
        }
    }
    // `ORTHO` is one cell, so the transform's units are thirds of a cell.
    let limit = reach_cells.map_or(FAR, |r| ((r * ORTHO as f32).round() as i32).max(0));
    let mut filled = filled;
    for (i, v) in grid.iter_mut().enumerate() {
        if v.is_finite() {
            continue;
        }
        if best[i] <= limit {
            *v = val[i];
        } else {
            // Out of reach: unknown ground, which the bake reads as the anchor's own
            // level. Un-marking it keeps the relaxation off cells that are still NaN,
            // where averaging a neighbourhood of NaNs poisons the ones beside it.
            filled[i] = false;
        }
    }
    // Smooth the invented ground only. Four passes takes the Voronoi crease out of a
    // canal-width hole without moving anything measured.
    let mut prev = vec![0.0f32; grid.len()];
    for _ in 0..FILL_SMOOTH_PASSES {
        prev.copy_from_slice(grid);
        for z in 0..n {
            for x in 0..n {
                let i = z * n + x;
                if !filled[i] {
                    continue;
                }
                let mut sum = 0.0;
                let mut count = 0.0;
                for (dx, dz) in [(-1i32, 0i32), (1, 0), (0, -1), (0, 1)] {
                    let (nx, nz) = (x as i32 + dx, z as i32 + dz);
                    if nx < 0 || nz < 0 || nx >= n as i32 || nz >= n as i32 {
                        continue;
                    }
                    let v = prev[nz as usize * n + nx as usize];
                    if !v.is_finite() {
                        continue;
                    }
                    sum += v;
                    count += 1.0;
                }
                if count > 0.0 {
                    grid[i] = sum / count;
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The datum point maps to the grid origin it is defined by.
    #[test]
    fn amersfoort_is_the_origin_of_the_grid() {
        let [x, y] = wgs84_to_rd(52.15517440, 5.38720621);
        assert!((x - 155000.0).abs() < 0.5, "x {x}");
        assert!((y - 463000.0).abs() < 0.5, "y {y}");
    }

    /// Groningen is inside the envelope and Berlin is not.
    #[test]
    fn the_envelope_is_the_netherlands() {
        assert!(covers(53.2194, 6.5665), "Groningen");
        assert!(!covers(52.52, 13.405), "Berlin");
    }

    /// An XML exception is the ordinary refusal and must be reported as itself.
    #[test]
    fn a_service_exception_is_not_a_corrupt_tiff() {
        let err = decode_float_tiff(b"<ServiceExceptionReport>no such coverage", 4)
            .expect_err("XML is not a TIFF");
        assert!(err.contains("ServiceException"), "{err}");
    }
}
