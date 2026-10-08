//! `<layer>/14/{x}/{y}.json` — what lies under the ground, for every
//! underground layer but the sewers, in four shapes.
//!
//! Written by atlas's underground datasets (`pipeline/geotop.rs`,
//! `pipeline/bro.rs`, `pipeline/gasfield.rs`, `pipeline/wko.rs`,
//! `pipeline/pipelines.rs`, `pipeline/contamination.rs`); read by the
//! client's `systems::underground`. Each dataset writes its own tree in this one
//! format, so the client streams and meshes them all with one piece of code.
//!
//! * `columns` — `[lat, lng, radius_m, [[top_m, bottom_m, kind], …]]`: a
//!   vertical stack of coloured segments at a point — a soil column, a borehole,
//!   a well with its filters.
//! * `lines` — `[kind, radius_m, [depth_m, …], [lat, lng, …]]`: a tube with a
//!   depth at every vertex — a pipeline.
//! * `points` — `[kind, lat, lng, depth_m, size_m]`: one thing at a depth — an
//!   earthquake's hypocentre, a groundwater level.
//! * `rings` — `[kind, depth_m, [lat, lng, …]]`: an outline at a depth — the
//!   edge of a gas field, a contaminated site.
//!
//! **Every depth is metres below the ground, positive downwards**, for the
//! reason the sewers' format gives: the client hangs it under its own terrain,
//! which is relative to the ground under the map anchor and knows nothing of
//! NAP. A dataset whose source gives levels converts them on atlas.
//!
//! A shape is filed under the cell holding its first point (a column, point)
//! or its midpoint (a line, a ring), once.

use serde::{Deserialize, Serialize};

/// What a shape is, which decides its colour. One list for every layer, so a
/// number means the same thing in every tree — **append, never renumber**.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Kind {
    Unknown,
    // Soil, GeoTOP's lithoclasses and a borehole's main soil names.
    /// Made ground: fill, rubble, raised streets.
    Anthropogenic,
    Peat,
    Clay,
    /// Sandy clay and clayey sand, loam.
    ClayeySand,
    FineSand,
    MediumSand,
    CoarseSand,
    Gravel,
    Shells,
    // Groundwater.
    /// The measured groundwater level at a monitoring well.
    WaterTable,
    /// A well's screen, where it takes in water.
    WellFilter,
    /// A well's blind tube.
    WellTube,
    // Ground energy (heat and cold storage).
    HeatWarm,
    HeatCold,
    /// A closed loop, which has no warm or cold side.
    HeatLoop,
    // Transport pipelines.
    PipeGas,
    PipeChemical,
    PipeOther,
    // The gas field.
    GasWell,
    Earthquake,
    /// The edge of the field, at the depth of the reservoir's top.
    GasField,
    // Soil contamination.
    ContaminationSerious,
    Contamination,
    /// A cone penetration test: how deep the probe went.
    Sounding,
    /// An underground storage tank still in the ground, not cleaned up.
    FuelTank,
    /// A tank cleaned up by filling it, so still in the ground.
    FuelTankFilled,
}

impl Kind {
    /// Wire order.
    pub const ALL: [Self; 27] = [
        Self::Unknown,
        Self::Anthropogenic,
        Self::Peat,
        Self::Clay,
        Self::ClayeySand,
        Self::FineSand,
        Self::MediumSand,
        Self::CoarseSand,
        Self::Gravel,
        Self::Shells,
        Self::WaterTable,
        Self::WellFilter,
        Self::WellTube,
        Self::HeatWarm,
        Self::HeatCold,
        Self::HeatLoop,
        Self::PipeGas,
        Self::PipeChemical,
        Self::PipeOther,
        Self::GasWell,
        Self::Earthquake,
        Self::GasField,
        Self::ContaminationSerious,
        Self::Contamination,
        Self::Sounding,
        Self::FuelTank,
        Self::FuelTankFilled,
    ];

    /// The wire number.
    #[must_use]
    pub fn index(self) -> u8 {
        Self::ALL.iter().position(|k| *k == self).unwrap_or(0) as u8
    }

    /// From the wire number; one a newer writer added reads as
    /// [`Self::Unknown`], which costs an older client a colour, not the shape.
    #[must_use]
    pub fn from_index(i: u8) -> Self {
        Self::ALL
            .get(usize::from(i))
            .copied()
            .unwrap_or(Self::Unknown)
    }
}

/// `(lat, lng, radius_m, [(top_m, bottom_m, kind)])`.
pub type Column = (f64, f64, f32, Vec<(f32, f32, u8)>);
/// `(kind, radius_m, depths_m, flat_lat_lng)` — one depth per point.
pub type Line = (u8, f32, Vec<f32>, Vec<f64>);
/// `(kind, lat, lng, depth_m, size_m)`.
pub type Point = (u8, f64, f64, f32, f32);
/// `(kind, depth_m, flat_lat_lng)`, open.
pub type Ring = (u8, f32, Vec<f64>);

/// One cell file. Fields alphabetical (`formats` module note).
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Cell {
    #[serde(default)]
    pub columns: Vec<Column>,
    #[serde(default)]
    pub lines: Vec<Line>,
    #[serde(default)]
    pub points: Vec<Point>,
    #[serde(default)]
    pub rings: Vec<Ring>,
}

impl Cell {
    /// Nothing in it — the extractor writes no file for an empty cell.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.columns.is_empty()
            && self.lines.is_empty()
            && self.points.is_empty()
            && self.rings.is_empty()
    }

    /// The deepest depth anything in the cell reaches, for the client's
    /// backdrop, which must lie under all of it.
    #[must_use]
    pub fn deepest_m(&self) -> f32 {
        let columns = self.columns.iter().flat_map(|c| c.3.iter().map(|s| s.1));
        let lines = self.lines.iter().flat_map(|l| l.2.iter().copied());
        let points = self.points.iter().map(|p| p.3 + p.4);
        let rings = self.rings.iter().map(|r| r.1);
        columns
            .chain(lines)
            .chain(points)
            .chain(rings)
            .fold(0.0, f32::max)
    }
}

/// Parse a cell file.
///
/// # Errors
///
/// When the text is not JSON or not this document.
pub fn parse(text: &str) -> Result<Cell, String> {
    super::from_str(text)
}

/// Flattened coordinates as points, or `None` with fewer than `min` points or
/// an odd count.
#[must_use]
pub fn points_of(flat: &[f64], min: usize) -> Option<Vec<super::LatLng>> {
    (flat.len() >= 2 * min && flat.len() % 2 == 0)
        .then(|| flat.chunks_exact(2).map(|p| (p[0], p[1])).collect())
}

#[cfg(test)]
mod tests {
    use super::*;

    const WRITTEN: &str = r#"{"columns":[[53.2,6.5,1.0,[[0.0,1.5,1],[1.5,4.0,2]]]],"lines":[[16,0.4,[1.2,1.3],[53.2,6.5,53.21,6.51]]],"points":[[20,53.3,6.8,3000.0,80.0]],"rings":[[21,2900.0,[53.2,6.5,53.3,6.5,53.3,6.6]]]}"#;

    fn sample() -> Cell {
        Cell {
            columns: vec![(53.2, 6.5, 1.0, vec![(0.0, 1.5, 1), (1.5, 4.0, 2)])],
            lines: vec![(16, 0.4, vec![1.2, 1.3], vec![53.2, 6.5, 53.21, 6.51])],
            points: vec![(20, 53.3, 6.8, 3000.0, 80.0)],
            rings: vec![(21, 2900.0, vec![53.2, 6.5, 53.3, 6.5, 53.3, 6.6])],
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
        assert!(parse("{}").unwrap().is_empty());
    }

    #[test]
    fn the_deepest_shape_is_found_whatever_its_kind() {
        // The earthquake's sphere reaches 3,080 m.
        assert_eq!(sample().deepest_m(), 3080.0);
    }

    #[test]
    fn kinds_keep_their_wire_numbers() {
        for (i, k) in Kind::ALL.iter().enumerate() {
            assert_eq!(usize::from(k.index()), i);
            assert_eq!(Kind::from_index(i as u8), *k);
        }
        assert_eq!(Kind::from_index(250), Kind::Unknown);
        // Pinned, because the files on the host hold the numbers.
        assert_eq!(Kind::Peat.index(), 2);
        assert_eq!(Kind::Earthquake.index(), 20);
        assert_eq!(Kind::Sounding.index(), 24);
    }
}
