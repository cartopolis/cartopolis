//! The prepared-terrain file: one map cell's ground heights, as bytes.
//!
//! What `carto-pipeline terrain` writes beside the cell's surfaces — the height
//! survey, resampled onto the cell and **flattened along the roads**
//! ([`crate::road_grade`]), so that everything drawn on this ground reads the same
//! number and no two sheets can disagree about where it is.
//!
//! # Why the heights are a separate file from the surfaces
//!
//! They change for different reasons and at different rates. A city's roads move
//! when the map does — weekly, with every edit upstream — and its ground moves
//! when the survey is re-flown, which is years apart. Shipping them together would
//! mean re-sending a megabyte of unchanged heights every time a bus lane was
//! redrawn.
//!
//! # Layout
//!
//! Fixed-width little-endian, for the reason [`crate::ground_file`] gives at
//! length: reading is a bounds check and a copy rather than a parse.
//!
//! ```text
//! magic "CTRN" | version u16 | flags u16 | z u32 | x u32 | y u32
//! grid u32 | datum f32 | samples f32[grid × grid]
//! ```
//!
//! `grid × grid` samples span the cell corner to corner, row 0 at the **north**
//! edge — the order the survey arrives in, kept so nothing has to flip it twice.
//! Heights are metres **relative to `datum`**, which is the cell's own reference
//! level.
//!
//! # Relative heights, and why the datum travels with them
//!
//! Absolute heights above the national datum would be correct and useless: a city
//! centre sits a metre or two above it and a hill town eighty, so a renderer
//! placing cells by their absolute height would drop the whole world by whatever
//! the local ground happens to be, for an identical picture. The datum is carried
//! so a caller that *does* want the real level — a flood model, a viaduct's
//! clearance — can still have it, without every ordinary caller paying for it.
//!
//! `NaN` marks a sample the survey has no reading for. It has none at all over
//! water, and none under a bridge deck in the ground model, so this is the common
//! case at a canal rather than an error. A reader fills or ignores; it must not
//! treat it as a height.

/// `CTRN` — cartopolis terrain.
const MAGIC: [u8; 4] = *b"CTRN";

/// Bumped whenever a reader would misread an older file, and refused rather than
/// guessed — the same rule as the prepared surfaces.
pub const VERSION: u16 = 1;

const HEADER_BYTES: usize = 4 + 2 + 2 + 4 + 4 + 4 + 4 + 4;

/// One cell's prepared ground heights.
#[derive(Debug, Clone, PartialEq)]
pub struct TerrainFile {
    pub z: u32,
    pub x: u32,
    pub y: u32,
    /// Samples per side, corner to corner.
    pub grid: u32,
    /// The level `samples` are measured from, in metres above the national datum.
    pub datum: f32,
    /// Row-major, row 0 at the north edge. `NaN` where the survey has no reading.
    pub samples: Vec<f32>,
}

/// Why a file could not be read.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ReadError {
    NotTerrain,
    Version(u16),
    Flags(u16),
    /// A grid this reader will not allocate for. Checked before the length, so a
    /// corrupt size cannot ask for a terabyte before being refused.
    Grid(u32),
    Truncated {
        wanted: usize,
        had: usize,
    },
}

impl std::fmt::Display for ReadError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::NotTerrain => write!(f, "not a prepared-terrain file"),
            Self::Version(v) => write!(f, "prepared terrain version {v}, this reads {VERSION}"),
            Self::Flags(b) => write!(f, "prepared terrain sets unknown flags {b:#06x}"),
            Self::Grid(g) => write!(f, "prepared terrain claims a {g}² grid"),
            Self::Truncated { wanted, had } => {
                write!(f, "prepared terrain wants {wanted} bytes and has {had}")
            }
        }
    }
}

impl std::error::Error for ReadError {}

/// Largest grid a file may declare. A cell is ~1.5 km across, so 2049² is 0.7 m
/// samples — finer than the survey's own half-metre source after resampling, and
/// 16 MB decoded. Past that the number is a mistake rather than a request.
pub const MAX_GRID: u32 = 2049;

#[must_use]
pub fn encoded_len(grid: u32) -> usize {
    HEADER_BYTES + (grid as usize) * (grid as usize) * 4
}

/// One cell's heights, as the bytes a client reads.
#[must_use]
pub fn encode(file: &TerrainFile) -> Vec<u8> {
    let mut out = Vec::with_capacity(encoded_len(file.grid));
    out.extend_from_slice(&MAGIC);
    out.extend_from_slice(&VERSION.to_le_bytes());
    out.extend_from_slice(&0u16.to_le_bytes()); // flags
    out.extend_from_slice(&file.z.to_le_bytes());
    out.extend_from_slice(&file.x.to_le_bytes());
    out.extend_from_slice(&file.y.to_le_bytes());
    out.extend_from_slice(&file.grid.to_le_bytes());
    out.extend_from_slice(&file.datum.to_le_bytes());
    for s in &file.samples {
        out.extend_from_slice(&s.to_le_bytes());
    }
    out
}

/// Read a cell's heights back.
pub fn decode(bytes: &[u8]) -> Result<TerrainFile, ReadError> {
    if bytes.len() < HEADER_BYTES {
        return Err(ReadError::Truncated {
            wanted: HEADER_BYTES,
            had: bytes.len(),
        });
    }
    if bytes[..4] != MAGIC {
        return Err(ReadError::NotTerrain);
    }
    let u16_at = |i: usize| u16::from_le_bytes([bytes[i], bytes[i + 1]]);
    let u32_at =
        |i: usize| u32::from_le_bytes([bytes[i], bytes[i + 1], bytes[i + 2], bytes[i + 3]]);

    let version = u16_at(4);
    if version != VERSION {
        return Err(ReadError::Version(version));
    }
    let flags = u16_at(6);
    if flags != 0 {
        return Err(ReadError::Flags(flags));
    }
    let (z, x, y) = (u32_at(8), u32_at(12), u32_at(16));
    let grid = u32_at(20);
    if !(2..=MAX_GRID).contains(&grid) {
        return Err(ReadError::Grid(grid));
    }
    let datum = f32::from_le_bytes([bytes[24], bytes[25], bytes[26], bytes[27]]);

    let wanted = encoded_len(grid);
    if bytes.len() < wanted {
        return Err(ReadError::Truncated {
            wanted,
            had: bytes.len(),
        });
    }
    let samples = bytes[HEADER_BYTES..wanted]
        .chunks_exact(4)
        .map(|c| f32::from_le_bytes([c[0], c[1], c[2], c[3]]))
        .collect();
    Ok(TerrainFile {
        z,
        x,
        y,
        grid,
        datum,
        samples,
    })
}

impl TerrainFile {
    /// Height at a point given as a fraction of the cell, `(0,0)` at the
    /// north-west corner — which is the frame a cell's surface positions are
    /// already in.
    ///
    /// Bilinear, and `None` where any corner of the sample square has no reading.
    /// Interpolating across a missing sample would invent ground at the edge of a
    /// canal, which is precisely where this is most often asked.
    #[must_use]
    pub fn at_fraction(&self, fx: f32, fz: f32) -> Option<f32> {
        let n = self.grid as usize;
        if n < 2 || self.samples.len() < n * n {
            return None;
        }
        if !(0.0..=1.0).contains(&fx) || !(0.0..=1.0).contains(&fz) {
            return None;
        }
        #[allow(clippy::cast_precision_loss)]
        let (gx, gz) = (fx * (n - 1) as f32, fz * (n - 1) as f32);
        #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
        let (x0, z0) = (gx.floor() as usize, gz.floor() as usize);
        let (x1, z1) = ((x0 + 1).min(n - 1), (z0 + 1).min(n - 1));
        #[allow(clippy::cast_precision_loss)]
        let (tx, tz) = (gx - x0 as f32, gz - z0 as f32);
        let s = |x: usize, z: usize| self.samples[z * n + x];
        let (a, b, c, d) = (s(x0, z0), s(x1, z0), s(x0, z1), s(x1, z1));
        if !(a.is_finite() && b.is_finite() && c.is_finite() && d.is_finite()) {
            return None;
        }
        let top = a + (b - a) * tx;
        let bottom = c + (d - c) * tx;
        Some(top + (bottom - top) * tz)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn a_file() -> TerrainFile {
        let grid = 5u32;
        let n = grid as usize;
        let mut samples = vec![0.0f32; n * n];
        for z in 0..n {
            for x in 0..n {
                #[allow(clippy::cast_precision_loss)]
                {
                    samples[z * n + x] = x as f32 + z as f32 * 10.0;
                }
            }
        }
        // One hole, because a canal is the common case rather than an exception.
        samples[2 * n + 2] = f32::NAN;
        TerrainFile {
            z: 14,
            x: 8489,
            y: 5321,
            grid,
            datum: 1.25,
            samples,
        }
    }

    #[test]
    fn a_cell_survives_the_round_trip_exactly() {
        let file = a_file();
        let bytes = encode(&file);
        assert_eq!(bytes.len(), encoded_len(file.grid));
        let back = decode(&bytes).expect("round trip");
        assert_eq!(back.z, file.z);
        assert_eq!(back.grid, file.grid);
        assert_eq!(back.datum, file.datum);
        // `NaN != NaN`, so compare the bits: a hole has to survive as a hole.
        assert!(back.samples[2 * 5 + 2].is_nan(), "the hole was filled");
        for (a, b) in back.samples.iter().zip(&file.samples) {
            assert!(a == b || (a.is_nan() && b.is_nan()), "{a} != {b}");
        }
    }

    /// Every refusal, including the one that matters: a corrupt grid size is
    /// checked *before* the length, so it cannot ask for a terabyte on the way to
    /// being rejected.
    #[test]
    fn a_damaged_file_is_refused_rather_than_read() {
        let good = encode(&a_file());
        assert_eq!(decode(b"no").unwrap_err().to_string().is_empty(), false);
        assert_eq!(decode(&[0u8; 64]).unwrap_err(), ReadError::NotTerrain);

        let mut wrong_version = good.clone();
        wrong_version[4] = 9;
        assert!(matches!(
            decode(&wrong_version).unwrap_err(),
            ReadError::Version(9)
        ));

        let mut wrong_flags = good.clone();
        wrong_flags[6] = 4;
        assert!(matches!(
            decode(&wrong_flags).unwrap_err(),
            ReadError::Flags(4)
        ));

        let mut silly_grid = good.clone();
        silly_grid[20..24].copy_from_slice(&u32::MAX.to_le_bytes());
        assert!(matches!(
            decode(&silly_grid).unwrap_err(),
            ReadError::Grid(_)
        ));

        assert!(matches!(
            decode(&good[..good.len() - 4]).unwrap_err(),
            ReadError::Truncated { .. }
        ));
        for cut in 0..good.len() {
            let _ = decode(&good[..cut]);
        }
    }

    /// The lookup is in cell fractions because that is the frame a cell's surfaces
    /// are already in, and it refuses rather than interpolating across a hole —
    /// which would invent ground at the edge of a canal, exactly where it is most
    /// often asked.
    #[test]
    fn a_height_is_read_in_cell_fractions_and_stops_at_a_hole() {
        let f = a_file();
        assert_eq!(f.at_fraction(0.0, 0.0), Some(0.0), "north-west corner");
        assert_eq!(f.at_fraction(1.0, 0.0), Some(4.0), "north-east corner");
        assert_eq!(f.at_fraction(0.0, 1.0), Some(40.0), "south-west corner");
        // Midway along the top row, between samples 2 and 3 of a 0..4 ramp.
        assert_eq!(f.at_fraction(0.625, 0.0), Some(2.5));
        // Straddling the hole at (2,2).
        assert_eq!(f.at_fraction(0.5, 0.5), None, "read across a hole");
        assert_eq!(f.at_fraction(-0.1, 0.5), None, "outside the cell");
        assert_eq!(f.at_fraction(0.5, 1.1), None, "outside the cell");
    }
}
