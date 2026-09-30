//! `water/14/{x}/{y}.json` — water bodies and the level each stands at.
//!
//! Written by atlas's `pipeline/water.rs`; read by the client's
//! `water_levels::parse_water_cell` and by atlas's own `terrain_grade`. Each ring
//! is a row `[level, [lat, lng, lat, lng, …]]`: the level in metres NAP, the
//! outline flattened into one array of alternating coordinates (open — the last
//! point is not repeated). A ring touching several cells is written into each.

use serde::{Deserialize, Serialize};

/// One water body: `(level_nap_m, flat_lat_lng)`.
pub type Ring = (f64, Vec<f64>);

/// One cell file.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Cell {
    #[serde(default)]
    pub rings: Vec<Ring>,
}

/// Parse a cell file.
///
/// # Errors
///
/// When the text is not JSON or not this document.
pub fn parse(text: &str) -> Result<Cell, String> {
    super::from_str(text)
}

/// A ring's flattened coordinates as points, or `None` when they cannot be a
/// ring — fewer than three points, or an odd count. The extractor never writes
/// one; a truncated cache entry can.
#[must_use]
pub fn ring_points(flat: &[f64]) -> Option<Vec<super::LatLng>> {
    (flat.len() >= 6 && flat.len() % 2 == 0)
        .then(|| flat.chunks_exact(2).map(|p| (p[0], p[1])).collect())
}

#[cfg(test)]
mod tests {
    use super::*;

    const WRITTEN: &str = r#"{"rings":[[-0.93,[53.2,6.5,53.2,6.501,53.201,6.501]],[0.0,[53.3,6.6,53.3,6.601,53.301,6.601]]]}"#;

    fn sample() -> Cell {
        Cell {
            rings: vec![
                (-0.93, vec![53.2, 6.5, 53.2, 6.501, 53.201, 6.501]),
                (0.0, vec![53.3, 6.6, 53.3, 6.601, 53.301, 6.601]),
            ],
        }
    }

    fn text(cell: &Cell) -> String {
        String::from_utf8(super::super::to_vec(cell).unwrap()).unwrap()
    }

    #[test]
    fn a_cell_round_trips() {
        assert_eq!(parse(&text(&sample())).unwrap(), sample());
    }

    #[test]
    fn the_old_writer_s_bytes_parse_and_are_reproduced() {
        assert_eq!(parse(WRITTEN).unwrap(), sample());
        assert_eq!(text(&sample()), WRITTEN);
    }

    #[test]
    fn a_short_or_odd_ring_is_not_a_ring() {
        assert_eq!(ring_points(&[53.0, 6.0, 53.1, 6.1]), None);
        assert_eq!(ring_points(&[53.0, 6.0, 53.1, 6.1, 53.2]), None);
        assert_eq!(
            ring_points(&[53.0, 6.0, 53.1, 6.1, 53.2, 6.2]).map(|r| r.len()),
            Some(3)
        );
    }
}
