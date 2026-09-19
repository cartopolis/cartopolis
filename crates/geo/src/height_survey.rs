//! The Dutch height survey, as far as it is arithmetic.
//!
//! AHN publishes the country's ground and surface models through an OGC coverage
//! service: ask for a box in the national grid at an exact sample count and it
//! answers with a single-band float GeoTIFF of metres above datum. This module is
//! everything about that which is not fetching — building the request, converting
//! between degrees and the national grid, decoding the answer, and reading heights
//! out of it.
//!
//! **The fetching is deliberately not here.** The client fetches through its own
//! throttled queue, on a task pool, with a cache keyed for eviction; the pipeline
//! fetches with a blocking call in a loop. Those are genuinely different problems
//! and neither belongs in a shared crate. What must not differ is the arithmetic:
//! if the server and the client work out where the ground is separately, they
//! disagree by metres over a canal bank, and every surface drawn on that ground
//! tears through every other one. That is the fault this whole area exists to
//! remove, so the sum is written once.

/// PDOK's public service host. The client reaches it through its own origin on the
/// web, because the browser's cross-origin rules are not ours to negotiate; a
/// server-side caller goes direct.
pub const AHN_ORIGIN: &str = "https://service.pdok.nl";

/// The coverage service entry point. Version 1.0.0 is what it speaks.
pub const AHN_WCS_PATH: &str = "/rws/ahn/wcs/v1_0";

/// AHN's coverage envelope, from its own capabilities document. Asking outside it
/// costs a round trip to be told no.
const AHN_LON: (f64, f64) = (3.20, 7.28);
const AHN_LAT: (f64, f64) = (50.72, 53.56);

/// The service writes a very large value where the model has nothing — the
/// bridge-shaped holes in the ground model, and everything outside the survey's
/// flight coverage. Compared against a threshold rather than for equality: it
/// survives the round trip exactly today, and a resampling upstream is entitled to
/// smear it.
const NODATA_ABOVE: f32 = 1.0e30;

/// Largest grid one request may ask for. Measured against the live service
/// 2026-09-11/13: 256² is 99 kB in 0.6 s, 1024² is 1.5 MB in 0.5 s, 2048² is
/// 6.4 MB in 1.5 s, 2049² is 12.3 MB in 4.5 s. So this is what the service will
/// actually serve rather than a guess — and note it is not a memory bound, since a
/// 1024² patch is 4 MB decoded and the caller owns it.
pub const MAX_GRID: u32 = 4096;

/// Whether the survey covers this point at all. Everywhere outside is not merely
/// unmeasured — the coordinate conversion below is meaningless there too.
#[must_use]
pub fn covers(lat: f64, lng: f64) -> bool {
    (AHN_LON.0..=AHN_LON.1).contains(&lng) && (AHN_LAT.0..=AHN_LAT.1).contains(&lat)
}

/// Which raster to sample.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum HeightModel {
    /// Bare ground: buildings and bridge decks are cut out of it, so a deck reads
    /// as no-data and what is under the deck reads as ground.
    Terrain,
    /// Everything the laser hit — roofs, trees, and the point of it, bridge decks
    /// at their real height.
    Surface,
}

impl HeightModel {
    #[must_use]
    pub fn coverage(self) -> &'static str {
        match self {
            Self::Terrain => "dtm_05m",
            Self::Surface => "dsm_05m",
        }
    }
}

/// A box of ground to measure, in national-grid metres.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct HeightRequest {
    pub model: HeightModel,
    /// South-west corner (easting, northing).
    pub min: [f64; 2],
    /// North-east corner.
    pub max: [f64; 2],
    /// Samples across and down. The service resamples, so this is free to be small.
    pub grid: u32,
}

impl HeightRequest {
    /// A box centred on `centre` with `half` metres to each side.
    #[must_use]
    pub fn around(model: HeightModel, centre: [f64; 2], half: f64, grid: u32) -> Self {
        Self {
            model,
            min: [centre[0] - half, centre[1] - half],
            max: [centre[0] + half, centre[1] + half],
            grid,
        }
    }

    /// Grow the box by `m` metres on every side — how a caller asks about the
    /// ground *around* a thing rather than the thing itself.
    #[must_use]
    pub fn padded(mut self, m: f64) -> Self {
        self.min = [self.min[0] - m, self.min[1] - m];
        self.max = [self.max[0] + m, self.max[1] + m];
        self
    }

    #[must_use]
    pub fn is_valid(&self) -> bool {
        self.max[0] > self.min[0] && self.max[1] > self.min[1] && self.grid >= 2
    }

    /// The service query for this request, path and all.
    ///
    /// Shared rather than formatted at each call site because the box in the query
    /// and the box the answer is interpreted against have to be the same box — that
    /// is the whole reason [`decode`] ignores the file's own georeferencing.
    #[must_use]
    pub fn path(&self) -> String {
        format!(
            "{AHN_WCS_PATH}?SERVICE=WCS&VERSION=1.0.0&REQUEST=GetCoverage\
             &COVERAGE={coverage}&CRS=EPSG:28992&FORMAT=GEOTIFF\
             &BBOX={x0:.2},{y0:.2},{x1:.2},{y1:.2}&WIDTH={grid}&HEIGHT={grid}",
            coverage = self.model.coverage(),
            x0 = self.min[0],
            y0 = self.min[1],
            x1 = self.max[0],
            y1 = self.max[1],
            grid = self.grid.clamp(2, MAX_GRID),
        )
    }
}

/// One measured box: a regular grid of metres above datum, row 0 at the **north**
/// edge — the order the file arrives in — with `NaN` for no-data.
#[derive(Debug, Clone)]
pub struct HeightPatch {
    pub min: [f64; 2],
    pub max: [f64; 2],
    pub w: usize,
    pub h: usize,
    pub samples: Vec<f32>,
}

impl HeightPatch {
    /// Height at a national-grid point, or `None` outside the patch or on no-data.
    ///
    /// Nearest sample, not interpolated: these are measurements, and interpolating
    /// across the edge of a bridge deck would invent a slope that is not there.
    #[must_use]
    pub fn at(&self, rd: [f64; 2]) -> Option<f32> {
        let (dx, dy) = (self.max[0] - self.min[0], self.max[1] - self.min[1]);
        if dx <= 0.0 || dy <= 0.0 {
            return None;
        }
        let fx = (rd[0] - self.min[0]) / dx;
        // North is row 0.
        let fy = (self.max[1] - rd[1]) / dy;
        if !(0.0..1.0).contains(&fx) || !(0.0..1.0).contains(&fy) {
            return None;
        }
        #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
        let col = (fx * self.w as f64) as usize;
        #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
        let row = (fy * self.h as f64) as usize;
        let v = *self.samples.get(row * self.w + col)?;
        v.is_finite().then_some(v)
    }

    /// The median of every valid sample the predicate accepts, by position.
    ///
    /// Median rather than mean, and it is not a detail: a deck's samples include
    /// its parapets, the lamp posts on it and the occasional lorry, and a mean is
    /// dragged upward by all three. Half the samples being junk still leaves the
    /// median on the deck.
    #[must_use]
    pub fn median_where(&self, keep: impl Fn([f64; 2]) -> bool) -> Option<f32> {
        let mut vals: Vec<f32> = Vec::new();
        let (dx, dy) = (self.max[0] - self.min[0], self.max[1] - self.min[1]);
        for row in 0..self.h {
            for col in 0..self.w {
                let v = self.samples[row * self.w + col];
                if !v.is_finite() {
                    continue;
                }
                // Sample centres, so a point never lands on a cell boundary.
                #[allow(clippy::cast_precision_loss)]
                let rd = [
                    self.min[0] + (col as f64 + 0.5) / self.w as f64 * dx,
                    self.max[1] - (row as f64 + 0.5) / self.h as f64 * dy,
                ];
                if keep(rd) {
                    vals.push(v);
                }
            }
        }
        if vals.is_empty() {
            return None;
        }
        vals.sort_by(f32::total_cmp);
        Some(vals[vals.len() / 2])
    }

    /// The median of every valid sample in the patch.
    #[must_use]
    pub fn median(&self) -> Option<f32> {
        self.median_where(|_| true)
    }

    /// How much of the patch carries a measurement. A ground-model patch over a
    /// bridge deck is mostly holes, and a caller wants to know that before trusting
    /// a median taken from the handful of samples that survived.
    #[must_use]
    pub fn coverage(&self) -> f32 {
        if self.samples.is_empty() {
            return 0.0;
        }
        #[allow(clippy::cast_precision_loss)]
        {
            self.samples.iter().filter(|v| v.is_finite()).count() as f32 / self.samples.len() as f32
        }
    }
}

/// Decode a single-band float GeoTIFF into a patch for `req`.
///
/// **The file's own georeferencing is ignored on purpose.** We asked for an exact
/// box at an exact grid size and the service honours both, so the request *is* the
/// transform; reading it back out of the file would only add a way for the two to
/// disagree.
pub fn decode(req: &HeightRequest, bytes: &[u8]) -> Result<HeightPatch, String> {
    let grid = req.grid.clamp(2, MAX_GRID) as usize;
    let samples = decode_float_tiff(bytes, grid)?;
    Ok(HeightPatch {
        min: req.min,
        max: req.max,
        w: grid,
        h: grid,
        samples,
    })
}

fn decode_float_tiff(bytes: &[u8], grid: usize) -> Result<Vec<f32>, String> {
    // A coverage service answers a refused request with an XML exception and a
    // perfectly ordinary 200, so "not a TIFF" is the normal error path rather than
    // a corruption case. Say what it said, rather than "invalid magic".
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
        return Err(format!("asked for {grid}², got {w}×{h}"));
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

/// Passes of relaxation over the invented ground. Four takes the crease out of a
/// canal-width hole without moving anything measured.
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
/// is a unit mistake away from the resolution is how a change to the grid size
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

/// Degrees → the Dutch national grid, in metres.
///
/// The published polynomial approximation about the Amersfoort datum point, good
/// to a few centimetres inside the Netherlands and meaningless outside it — which
/// is exactly the domain the survey covers, so [`covers`] is what keeps callers
/// inside it. A real projection library would be several megabytes of dependency
/// to answer one question a dozen coefficients answer here.
#[must_use]
pub fn wgs84_to_rd(lat: f64, lng: f64) -> [f64; 2] {
    let dl = 0.36 * (lat - 52.155_174_40);
    let dp = 0.36 * (lng - 5.387_206_21);
    let x = 155_000.0 + 190_094.945 * dp
        - 11_832.228 * dl * dp
        - 114.221 * dp * dl * dl
        - 32.391 * dp * dp * dp
        - 0.705 * dl
        - 2.340 * dp * dl * dl * dl
        - 0.608 * dp * dp * dp * dl
        + 0.008 * dl * dl
        - 0.192 * dp * dp * dp * dl * dl;
    let y = 463_000.0 + 309_056.544 * dl + 3_638.893 * dp * dp + 73.077 * dl * dl
        - 157.984 * dl * dp * dp
        + 59.788 * dl * dl * dl
        + 0.433 * dp
        - 6.439 * dp * dp * dl * dl
        - 0.032 * dl * dp
        + 0.092 * dp * dp * dp * dp
        - 0.054 * dp * dp * dp * dp * dl;
    [x, y]
}

/// The national grid → degrees, the same approximation run the other way.
///
/// It earns its place in the round-trip test, whose two coefficient sets are
/// transcribed independently — so it catches a mistyped forward coefficient that a
/// one-way check would happily accept.
#[must_use]
pub fn rd_to_wgs84(x: f64, y: f64) -> (f64, f64) {
    let dx = (x - 155_000.0) / 100_000.0;
    let dy = (y - 463_000.0) / 100_000.0;
    let lat = 52.155_174_40
        + (3_235.65389 * dy
            - 32.58297 * dx * dx
            - 0.24750 * dy * dy
            - 0.84978 * dx * dx * dy
            - 0.06550 * dy * dy * dy
            - 0.01709 * dx * dx * dy * dy
            - 0.00738 * dx
            + 0.00530 * dx * dx * dx * dx
            - 0.00039 * dx * dx * dy * dy * dy
            + 0.00033 * dx * dx * dx * dx * dy
            - 0.00012 * dx * dy)
            / 3600.0;
    let lng = 5.387_206_21
        + (5_260.52916 * dx + 105.94684 * dx * dy + 2.45656 * dx * dy * dy
            - 0.81885 * dx * dx * dx
            + 0.05594 * dx * dy * dy * dy
            - 0.05607 * dx * dx * dx * dy
            + 0.01199 * dy
            + 0.00256 * dx * dx * dx * dy * dy
            + 0.00128 * dx * dy * dy * dy * dy
            + 0.00022 * dy * dy
            + 0.00026 * dx * dx * dx * dx * dx)
            / 3600.0;
    (lat, lng)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The datum point the whole approximation is expanded about: the tower in
    /// Amersfoort is (155000, 463000) by construction.
    #[test]
    fn the_amersfoort_datum_point_is_the_grid_origin() {
        let rd = wgs84_to_rd(52.155_174_40, 5.387_206_21);
        assert!((rd[0] - 155_000.0).abs() < 0.01, "x {}", rd[0]);
        assert!((rd[1] - 463_000.0).abs() < 0.01, "y {}", rd[1]);
    }

    /// Both directions against each other, across the whole diagonal of the domain.
    /// The two coefficient sets are transcribed independently, so this catches a
    /// mistyped digit in either.
    #[test]
    fn the_conversion_round_trips_across_the_country() {
        for (lat, lng) in [
            (53.2194, 6.5665), // Groningen
            (50.8514, 5.6910), // Maastricht
            (52.3676, 4.9041), // Amsterdam
            (51.9244, 4.4777), // Rotterdam
        ] {
            let rd = wgs84_to_rd(lat, lng);
            let (back_lat, back_lng) = rd_to_wgs84(rd[0], rd[1]);
            assert!(
                (back_lat - lat).abs() < 1.0e-5 && (back_lng - lng).abs() < 1.0e-5,
                "{lat},{lng} -> {rd:?} -> {back_lat},{back_lng}"
            );
        }
    }

    /// Outside the survey there is nothing to ask for, and the conversion above is
    /// meaningless there too — so this is the gate, not a politeness.
    #[test]
    fn the_envelope_is_the_survey_and_not_the_world() {
        assert!(covers(53.2194, 6.5665), "Groningen");
        assert!(covers(50.8514, 5.6910), "Maastricht");
        assert!(!covers(52.5200, 13.4050), "Berlin");
        assert!(!covers(48.8566, 2.3522), "Paris");
    }

    /// A refused request comes back as XML with a 200, so the decoder has to say
    /// what the service said rather than complain about magic bytes.
    #[test]
    fn a_service_refusal_is_reported_as_itself() {
        let xml = b"<ServiceExceptionReport><ServiceException>\
                    msWCSGetCoverage(): out of range</ServiceException></ServiceExceptionReport>";
        let err = decode_float_tiff(xml, 48).unwrap_err();
        assert!(err.contains("out of range"), "{err}");
    }

    /// The request is the transform, so the query has to carry the same box the
    /// answer will be read against — to the same precision.
    #[test]
    fn the_query_carries_the_box_it_will_be_read_against() {
        let req = HeightRequest::around(HeightModel::Terrain, [155_000.0, 463_000.0], 100.0, 64);
        let path = req.path();
        assert!(
            path.contains("BBOX=154900.00,462900.00,155100.00,463100.00"),
            "{path}"
        );
        assert!(path.contains("WIDTH=64&HEIGHT=64"), "{path}");
        assert!(path.contains("dtm_05m"), "{path}");
        assert!(req.is_valid());
        assert!(
            !HeightRequest { grid: 1, ..req }.is_valid(),
            "a 1-sample grid"
        );
    }
}
