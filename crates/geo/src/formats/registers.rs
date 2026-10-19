//! `registers/14/{x}/{y}.json` — what a city surveys of its own streets, and the
//! public art Wikidata registers, for one cell.
//!
//! Written by atlas's `pipeline/registers.rs`; read by the client's
//! `municipal.rs`. Until 2026-10-19 the client asked each city's own server for
//! these, per cell, and carried that city's layer names, field names and
//! vocabulary in its own code. Now atlas asks once per run and the client reads
//! only this shape, which is why nothing in it is any city's vocabulary: a prop
//! is a [`FurnitureKind`](crate::furniture::FurnitureKind) by its slug, a tree a
//! position and a height.
//!
//! **`supplies` is the part that is not data about objects.** A register stands
//! in for the national one *per kind* (the client's `municipal.rs` says why), and
//! which kinds it stands in for is a fact about the cell, not about the objects
//! in it: a covered cell with no lamp posts in it still has the city's word that
//! there are none, and the BGT's copies must not be drawn there either. So every
//! cell in a run's area is written, empty or not, and says what it speaks for.
//!
//! Rows are JSON arrays rather than objects, because a dense cell carries
//! thousands; trailing elements are optional both ways, as in `bgt`.

use serde::{Deserialize, Serialize};

use super::LatLng;

/// A tree: `[lat, lng, height?]`, the height in metres where the register
/// gives one.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Tree(
    pub f64,
    pub f64,
    #[serde(default, skip_serializing_if = "Option::is_none")] pub Option<f32>,
);

/// A street object: `[lat, lng, kind, paint?]` — kind a `FurnitureKind` slug,
/// paint an sRGB body colour where the register says something the form can
/// show (a container's waste fraction).
///
/// A slug and not an index: an index would be one more wire order to keep
/// append-only for ever, and a slug this build does not know simply drops out.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Prop(
    pub f64,
    pub f64,
    pub String,
    #[serde(default, skip_serializing_if = "Option::is_none")] pub Option<u32>,
);

/// One cell.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Cell {
    /// Surveyed parking bays, one outer ring each — one bay, not a strip of
    /// them, so a bay holds at most one car.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub bays: Vec<Vec<LatLng>>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub props: Vec<Prop>,
    /// What this cell's registers are the authority on: `"trees"`, or a
    /// `FurnitureKind` slug. Always written, even empty.
    #[serde(default)]
    pub supplies: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub trees: Vec<Tree>,
}

/// The `supplies` entry for trees.
pub const SUPPLIES_TREES: &str = "trees";

/// Parse a cell file.
///
/// # Errors
///
/// When the text is not JSON or not this document.
pub fn parse(text: &str) -> Result<Cell, String> {
    super::from_str(text)
}

#[cfg(test)]
mod tests {
    use super::*;

    const CELL: &str = concat!(
        r#"{"bays":[[[53.2,6.5],[53.2,6.5001],[53.20005,6.5001],[53.20005,6.5]]],"#,
        r#""props":[[53.2194,6.5665,"lamp"],[53.2195,6.5666,"recycling",4087180]],"#,
        r#""supplies":["trees","lamp"],"trees":[[53.2196,6.5667,16.5],[53.2197,6.5668]]}"#
    );

    fn cell() -> Cell {
        Cell {
            bays: vec![vec![
                (53.2, 6.5),
                (53.2, 6.5001),
                (53.20005, 6.5001),
                (53.20005, 6.5),
            ]],
            props: vec![
                Prop(53.2194, 6.5665, "lamp".into(), None),
                Prop(53.2195, 6.5666, "recycling".into(), Some(0x3E_5D_8C)),
            ],
            supplies: vec!["trees".into(), "lamp".into()],
            trees: vec![
                Tree(53.2196, 6.5667, Some(16.5)),
                Tree(53.2197, 6.5668, None),
            ],
        }
    }

    #[test]
    fn a_cell_round_trips_byte_for_byte() {
        let text = String::from_utf8(super::super::to_vec(&cell()).unwrap()).unwrap();
        assert_eq!(text, CELL);
        assert_eq!(parse(CELL).unwrap(), cell());
    }

    /// An empty covered cell still says what it speaks for — that is the only
    /// thing it is written to say.
    #[test]
    fn an_empty_cell_keeps_its_supplies() {
        let empty = Cell {
            supplies: vec!["trees".into()],
            ..Cell::default()
        };
        let text = String::from_utf8(super::super::to_vec(&empty).unwrap()).unwrap();
        assert_eq!(text, r#"{"supplies":["trees"]}"#);
        assert_eq!(parse(&text).unwrap(), empty);
    }
}
