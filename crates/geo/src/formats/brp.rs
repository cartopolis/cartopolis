//! `brp/14/{x}/{y}.json` — the crop parcels of the BRP (Basisregistratie
//! Gewaspercelen).
//!
//! Written by atlas's `pipeline/brp.rs`; read by the client's
//! `crops::parse_crop_cell`. Each parcel is a row `[category, code, rings]`: the
//! category's **position** in the wire order both ends keep (`CATEGORIES` in
//! atlas and in the client — append, never renumber), the register's
//! `gewascode` (0 for "no tone of its own"), and the rings as `[lat, lng]`
//! points, outer first and holes after, each open (the last point is not
//! repeated). A parcel crossing a cell edge is written as the piece inside each
//! cell.

use serde::{Deserialize, Serialize};

use super::LatLng;

/// One parcel: `(category_position, gewascode, rings)`.
pub type Parcel = (usize, u32, Vec<Vec<LatLng>>);

/// One cell file.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Cell {
    #[serde(default)]
    pub parcels: Vec<Parcel>,
}

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

    /// Captured from the old `json!({ "parcels": parcels })` writer, each parcel
    /// `json!([ci, code, rings])`.
    const WRITTEN: &str = r#"{"parcels":[[1,2014,[[[53.25,6.5],[53.25,6.501],[53.251,6.501],[53.251,6.5]]]],[0,265,[[[53.2,6.4],[53.2,6.41],[53.21,6.41]],[[53.201,6.401],[53.201,6.402],[53.202,6.402]]]]]}"#;

    fn sample() -> Cell {
        Cell {
            parcels: vec![
                (
                    1,
                    2014,
                    vec![vec![
                        (53.25, 6.5),
                        (53.25, 6.501),
                        (53.251, 6.501),
                        (53.251, 6.5),
                    ]],
                ),
                (
                    0,
                    265,
                    vec![
                        vec![(53.2, 6.4), (53.2, 6.41), (53.21, 6.41)],
                        vec![(53.201, 6.401), (53.201, 6.402), (53.202, 6.402)],
                    ],
                ),
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

    /// The old writer itself, rebuilt: `json!([ci, code, rings])` per parcel with
    /// rings as `[f64; 2]` points, wrapped in `json!({ "parcels": … })`.
    #[test]
    fn the_typed_writer_matches_the_json_macro_it_replaced() {
        let parcels: Vec<serde_json::Value> = sample()
            .parcels
            .iter()
            .map(|(ci, code, rings)| {
                let rings: Vec<Vec<[f64; 2]>> = rings
                    .iter()
                    .map(|r| r.iter().map(|&(a, b)| [a, b]).collect())
                    .collect();
                serde_json::json!([ci, u64::from(*code), rings])
            })
            .collect();
        let old = serde_json::to_vec(&serde_json::json!({ "parcels": parcels })).unwrap();
        assert_eq!(text(&sample()).into_bytes(), old);
    }

    #[test]
    fn an_empty_document_is_an_empty_cell() {
        assert_eq!(parse("{}").unwrap(), Cell::default());
    }
}
