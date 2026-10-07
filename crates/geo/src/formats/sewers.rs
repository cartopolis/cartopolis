//! `sewers/14/{x}/{y}.json` — the sewer network under the street: its pipes and
//! its manholes, each with how far below the ground it lies.
//!
//! Written by atlas's `pipeline/sewers.rs` from the municipalities' GWSW
//! publication on PDOK; read by the client's `systems::sewers`.
//!
//! * `pipes` — `[kind, width_mm, height_mm, depth_start, depth_end, [lat, lng, …]]`.
//!   `kind` indexes [`SewerKind::ALL`]. The depths are metres from the ground
//!   to the **bottom inside the pipe** (its invert, the register's *bob*) at
//!   the first and last point, or `null` where the register gave no level. The
//!   line is flattened into alternating coordinates, open.
//! * `manholes` — `[lat, lng, depth]`, the depth of the shaft below its cover,
//!   or `null`.
//!
//! **Depths, never levels.** The register gives the ground at each manhole and
//! the invert at each pipe end in one datum, so their difference is a depth
//! whatever that datum is, and the client hangs it under its own terrain. A
//! level would have to agree with the client's terrain datum to the
//! centimetre, and the terrain is relative to the ground under the map anchor,
//! not to NAP.
//!
//! A pipe is filed under the cell holding its **midpoint**, once: they are tens
//! of metres long against a 1.5 km cell, and the client streams a ring, so the
//! neighbour a pipe pokes into is loaded too.

use serde::{Deserialize, Serialize};

/// What a pipe carries, in wire order — **append, never renumber**: the number
/// is what every cell file on the host holds.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum SewerKind {
    /// Foul water and rain in one pipe (`GemengdRiool`, and storage pipes on a
    /// combined system) — most of an old city centre.
    Combined,
    /// Foul water only (`Vuilwaterriool`).
    Foul,
    /// Rainwater only (`Hemelwaterriool`, overflow pipes).
    Storm,
    /// Water let into the ground rather than carried away: drains, infiltration
    /// pipes and culverts.
    Drainage,
    /// A pumped main (`Persleiding`, pressure and vacuum sewers). Not a gravity
    /// sewer, so its depth says nothing about the fall of the network.
    Pressure,
    /// Abandoned pipes and anything the register does not classify.
    Other,
}

impl SewerKind {
    /// Wire order.
    pub const ALL: [Self; 6] = [
        Self::Combined,
        Self::Foul,
        Self::Storm,
        Self::Drainage,
        Self::Pressure,
        Self::Other,
    ];

    /// The wire number.
    #[must_use]
    pub fn index(self) -> u8 {
        Self::ALL.iter().position(|k| *k == self).unwrap_or(5) as u8
    }

    /// From the wire number; one a newer writer added reads as [`Self::Other`],
    /// so a newer host costs an older client the *colour* of a pipe, never the
    /// pipe.
    #[must_use]
    pub fn from_index(i: u8) -> Self {
        Self::ALL
            .get(usize::from(i))
            .copied()
            .unwrap_or(Self::Other)
    }

    /// From the last segment of a GWSW type URI (`…/totaal/GemengdRiool`).
    #[must_use]
    pub fn from_gwsw(name: &str) -> Self {
        match name {
            "GemengdRiool" | "Bergingsleiding" => Self::Combined,
            "Vuilwaterriool" => Self::Foul,
            "Hemelwaterriool" | "Overstortleiding" => Self::Storm,
            "Drain" | "Infiltratieriool" | "Duiker" => Self::Drainage,
            "Persleiding" | "Drukleiding" | "Drukriool" | "Vacuumleiding" => Self::Pressure,
            _ => Self::Other,
        }
    }
}

/// `(kind, width_mm, height_mm, depth_start_m, depth_end_m, flat_lat_lng)`.
pub type Pipe = (u8, u16, u16, Option<f32>, Option<f32>, Vec<f64>);

/// `(lat, lng, depth_m)`.
pub type Manhole = (f64, f64, Option<f32>);

/// One cell file. Fields alphabetical (`formats` module note).
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Cell {
    #[serde(default)]
    pub manholes: Vec<Manhole>,
    #[serde(default)]
    pub pipes: Vec<Pipe>,
}

/// Parse a cell file.
///
/// # Errors
///
/// When the text is not JSON or not this document.
pub fn parse(text: &str) -> Result<Cell, String> {
    super::from_str(text)
}

/// A pipe's flattened coordinates as points, or `None` when they cannot be a
/// line — fewer than two points, or an odd count.
#[must_use]
pub fn line_points(flat: &[f64]) -> Option<Vec<super::LatLng>> {
    (flat.len() >= 4 && flat.len() % 2 == 0)
        .then(|| flat.chunks_exact(2).map(|p| (p[0], p[1])).collect())
}

#[cfg(test)]
mod tests {
    use super::*;

    const WRITTEN: &str = r#"{"manholes":[[53.2,6.5,2.25],[53.21,6.51,null]],"pipes":[[0,500,750,1.72,null,[53.2,6.5,53.2001,6.5002]]]}"#;

    fn sample() -> Cell {
        Cell {
            manholes: vec![(53.2, 6.5, Some(2.25)), (53.21, 6.51, None)],
            pipes: vec![(
                0,
                500,
                750,
                Some(1.72),
                None,
                vec![53.2, 6.5, 53.2001, 6.5002],
            )],
        }
    }

    #[test]
    fn a_cell_round_trips_byte_for_byte() {
        assert_eq!(parse(WRITTEN).unwrap(), sample());
        let text = String::from_utf8(super::super::to_vec(&sample()).unwrap()).unwrap();
        assert_eq!(text, WRITTEN);
    }

    #[test]
    fn an_empty_document_is_an_empty_cell() {
        assert_eq!(parse("{}").unwrap(), Cell::default());
    }

    #[test]
    fn kinds_keep_their_wire_numbers() {
        for (i, k) in SewerKind::ALL.iter().enumerate() {
            assert_eq!(usize::from(k.index()), i);
            assert_eq!(SewerKind::from_index(i as u8), *k);
        }
        assert_eq!(SewerKind::from_index(200), SewerKind::Other);
        assert_eq!(SewerKind::from_gwsw("GemengdRiool"), SewerKind::Combined);
        assert_eq!(SewerKind::from_gwsw("Persleiding"), SewerKind::Pressure);
        assert_eq!(SewerKind::from_gwsw("Mantelbuis"), SewerKind::Other);
    }

    #[test]
    fn a_short_or_odd_line_is_not_a_line() {
        assert_eq!(line_points(&[53.0, 6.0]), None);
        assert_eq!(line_points(&[53.0, 6.0, 53.1]), None);
        assert_eq!(
            line_points(&[53.0, 6.0, 53.1, 6.1]).map(|l| l.len()),
            Some(2)
        );
    }
}
