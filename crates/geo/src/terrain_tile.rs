//! One map tile's worth of measured ground: the format `cartopolis_atlas` serves at
//! `/terrain/{z}/{x}/{y}` and the client reads (`docs/notes/atlas.md`,
//! `docs/notes/level-of-detail-tile-tree.md`).
//!
//! **It is here, in the crate both sides depend on, so the writer and the reader
//! are one type.** Every other Dutch data format this client reads is written by a
//! TypeScript pipeline in another repository and kept in step by hand; this one
//! cannot drift, because a change to it is a change to both.
//!
//! A tile is a [`GRID`]² lattice of heights in metres above NAP, sampled on the
//! Web Mercator tile's own grid — corners and edges included, so a sample on a
//! shared edge is the *same* point in both neighbours and the two agree there by
//! construction. Heights are stored to the centimetre as `u16` steps above the
//! tile's own minimum, which is ~33 KB a tile and exact enough for ground that is
//! later flattened to roads anyway.

use cartopolis_core::world::tile_frac_to_lat_lng;

use crate::ahn::wgs84_to_rd;

/// Samples per tile edge. `2ⁿ + 1`, so a tile's samples are exactly every other
/// sample of its four children along each shared line — the property a tile tree
/// needs for a parent and a child to meet without a step.
pub const GRID: usize = 129;

/// What the header starts with, so a stray body (an HTML error page, an older
/// format) is refused rather than read as heights.
const MAGIC: &[u8; 4] = b"CTT1";

/// The step value that means "no measurement here" — open water AHN does not
/// cover, or a gap the hole fill did not reach.
const NO_DATA: u16 = u16::MAX;

/// Header: magic, grid (u16), min (f32), step (f32). Little-endian throughout.
const HEADER_LEN: usize = 4 + 2 + 4 + 4;

/// One tile of heights, row-major, north row first, west to east.
#[derive(Clone, Debug, PartialEq)]
pub struct TerrainTile {
    pub grid: usize,
    /// Metres above NAP; `NaN` where nothing was measured.
    pub heights: Vec<f32>,
}

impl TerrainTile {
    /// Height at a sample, `NaN` where nothing was measured.
    #[must_use]
    pub fn at(&self, col: usize, row: usize) -> f32 {
        self.heights[row * self.grid + col]
    }

    /// Whether any sample was measured at all. A tile with none is not served —
    /// it is answered as "no data here", which is a different answer from "flat".
    #[must_use]
    pub fn has_data(&self) -> bool {
        self.heights.iter().any(|h| h.is_finite())
    }

    /// The wire form. Quantised to steps of at most 1 cm; a tile spanning more than
    /// ~655 m of relief gets coarser steps rather than a wrong answer.
    #[must_use]
    pub fn encode(&self) -> Vec<u8> {
        let finite = self.heights.iter().copied().filter(|h| h.is_finite());
        let min = finite.clone().fold(f32::INFINITY, f32::min);
        let max = finite.fold(f32::NEG_INFINITY, f32::max);
        let (min, max) = if min.is_finite() {
            (min, max)
        } else {
            (0.0, 0.0)
        };
        // One step short of `NO_DATA`, so a measured height can never read as a hole.
        let step = ((max - min) / f32::from(NO_DATA - 1)).max(0.01);
        let mut out = Vec::with_capacity(HEADER_LEN + self.heights.len() * 2);
        out.extend_from_slice(MAGIC);
        out.extend_from_slice(&(self.grid as u16).to_le_bytes());
        out.extend_from_slice(&min.to_le_bytes());
        out.extend_from_slice(&step.to_le_bytes());
        for h in &self.heights {
            let q = if h.is_finite() {
                (((h - min) / step).round() as u32).min(u32::from(NO_DATA - 1)) as u16
            } else {
                NO_DATA
            };
            out.extend_from_slice(&q.to_le_bytes());
        }
        out
    }

    /// Read the wire form back. Refuses anything that is not exactly one tile.
    pub fn decode(bytes: &[u8]) -> Result<Self, String> {
        if bytes.len() < HEADER_LEN || &bytes[..4] != MAGIC {
            return Err("not a terrain tile".to_string());
        }
        let grid = usize::from(u16::from_le_bytes([bytes[4], bytes[5]]));
        let min = f32::from_le_bytes([bytes[6], bytes[7], bytes[8], bytes[9]]);
        let step = f32::from_le_bytes([bytes[10], bytes[11], bytes[12], bytes[13]]);
        let body = &bytes[HEADER_LEN..];
        if body.len() != grid * grid * 2 {
            return Err(format!(
                "a {grid}² terrain tile carries {} bytes of heights, not {}",
                body.len(),
                grid * grid * 2
            ));
        }
        let heights = body
            .chunks_exact(2)
            .map(|b| match u16::from_le_bytes([b[0], b[1]]) {
                NO_DATA => f32::NAN,
                q => min + f32::from(q) * step,
            })
            .collect();
        Ok(Self { grid, heights })
    }
}

/// Where each sample of tile `z/x/y` is, as WGS84 `(lat, lng)`, row-major.
#[must_use]
pub fn sample_points(z: u32, x: i64, y: i64) -> Vec<(f64, f64)> {
    let last = (GRID - 1) as f64;
    let mut out = Vec::with_capacity(GRID * GRID);
    for row in 0..GRID {
        for col in 0..GRID {
            out.push(tile_frac_to_lat_lng(
                x as f64 + col as f64 / last,
                y as f64 + row as f64 / last,
                z,
            ));
        }
    }
    out
}

/// The RD box to ask AHN for so that every sample of `points` falls inside it,
/// grown by `margin_m` so the bilinear lookup at the edge has a neighbour.
///
/// A Web Mercator tile is not an RD rectangle — the two grids are rotated and
/// scaled against each other — so this is the bounding box of the samples' own RD
/// positions rather than a transform of the tile's corners alone.
#[must_use]
pub fn rd_box(points: &[(f64, f64)], margin_m: f64) -> ([f64; 2], [f64; 2]) {
    let mut min = [f64::INFINITY; 2];
    let mut max = [f64::NEG_INFINITY; 2];
    for &(lat, lng) in points {
        let [e, n] = wgs84_to_rd(lat, lng);
        min = [min[0].min(e), min[1].min(n)];
        max = [max[0].max(e), max[1].max(n)];
    }
    (
        [min[0] - margin_m, min[1] - margin_m],
        [max[0] + margin_m, max[1] + margin_m],
    )
}

/// Sample a square AHN raster at each point, bilinearly.
///
/// `raster` is `n × n` over the RD box `min..max`, **north row first** — the order
/// a GeoTIFF from the WCS arrives in. A sample whose neighbours include no-data is
/// no-data: inventing a height under a building edge is the hole fill's job, and it
/// has already run over `raster` if the caller wanted it.
#[must_use]
pub fn resample(
    raster: &[f32],
    n: usize,
    min: [f64; 2],
    max: [f64; 2],
    points: &[(f64, f64)],
) -> Vec<f32> {
    let span = [max[0] - min[0], max[1] - min[1]];
    let last = (n - 1) as f64;
    points
        .iter()
        .map(|&(lat, lng)| {
            let [e, north] = wgs84_to_rd(lat, lng);
            // Pixel centres: column 0 is `min` east, row 0 is `max` north.
            let fx = ((e - min[0]) / span[0] * last).clamp(0.0, last);
            let fy = ((max[1] - north) / span[1] * last).clamp(0.0, last);
            let (x0, y0) = (fx.floor() as usize, fy.floor() as usize);
            let (x1, y1) = ((x0 + 1).min(n - 1), (y0 + 1).min(n - 1));
            let (tx, ty) = ((fx - x0 as f64) as f32, (fy - y0 as f64) as f32);
            let v = |x: usize, y: usize| raster[y * n + x];
            let top = v(x0, y0) + (v(x1, y0) - v(x0, y0)) * tx;
            let bottom = v(x0, y1) + (v(x1, y1) - v(x0, y1)) * tx;
            top + (bottom - top) * ty
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A tile survives the wire to the centimetre, holes included.
    #[test]
    fn a_tile_round_trips_to_the_centimetre() {
        let mut heights: Vec<f32> = (0..GRID * GRID)
            .map(|i| -6.7 + (i % 997) as f32 * 0.137)
            .collect();
        heights[5] = f32::NAN;
        heights[GRID * GRID - 1] = f32::NAN;
        let tile = TerrainTile {
            grid: GRID,
            heights,
        };
        let bytes = tile.encode();
        assert_eq!(bytes.len(), HEADER_LEN + GRID * GRID * 2);
        let back = TerrainTile::decode(&bytes).expect("decodes");
        assert_eq!(back.grid, GRID);
        for (a, b) in tile.heights.iter().zip(&back.heights) {
            if a.is_nan() {
                assert!(b.is_nan(), "a hole came back as {b}");
            } else {
                assert!((a - b).abs() <= 0.005 + 1e-4, "{a} came back as {b}");
            }
        }
    }

    #[test]
    fn a_body_that_is_not_a_tile_is_refused() {
        assert!(TerrainTile::decode(b"<html>502</html>").is_err());
        let mut short = TerrainTile {
            grid: GRID,
            heights: vec![1.0; GRID * GRID],
        }
        .encode();
        short.pop();
        assert!(TerrainTile::decode(&short).is_err());
    }

    /// Neighbours share their edge samples exactly, which is what lets two tiles
    /// meet without a crack.
    #[test]
    fn neighbours_share_their_edge_samples() {
        let (z, x, y) = (14, 8431, 5374);
        let here = sample_points(z, x, y);
        let east = sample_points(z, x + 1, y);
        for row in 0..GRID {
            assert_eq!(here[row * GRID + GRID - 1], east[row * GRID]);
        }
    }

    /// A plane measured in RD comes back as the same plane at every sample: the
    /// resampling goes through the right projection, the right corner and the
    /// right row order.
    #[test]
    fn a_plane_in_rd_resamples_exactly() {
        let points = sample_points(15, 16863, 10749);
        let (min, max) = rd_box(&points, 10.0);
        let n = 257;
        let plane = |e: f64, north: f64| (0.01 * (e - min[0]) - 0.02 * (north - min[1])) as f32;
        let mut raster = vec![0.0f32; n * n];
        for row in 0..n {
            for col in 0..n {
                let e = min[0] + (max[0] - min[0]) * col as f64 / (n - 1) as f64;
                let north = max[1] - (max[1] - min[1]) * row as f64 / (n - 1) as f64;
                raster[row * n + col] = plane(e, north);
            }
        }
        let got = resample(&raster, n, min, max, &points);
        for (&(lat, lng), h) in points.iter().zip(&got) {
            let [e, north] = wgs84_to_rd(lat, lng);
            let want = plane(e, north);
            assert!(
                (h - want).abs() < 1e-3,
                "sampled {h}, the plane says {want}"
            );
        }
    }
}
