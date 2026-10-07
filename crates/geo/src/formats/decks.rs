//! `decks/14/{x}/{y}.json` — the bridge-deck heights atlas measured while
//! grading.
//!
//! Written by atlas's `pipeline/terrain_grade.rs::write_decks`; read by the
//! client's `terrain_tiles::load_atlas_decks` and by the viewer's
//! `bridges::cell_bridges`. `{"decks": [[x, y, height, top, profile], …]}`: each
//! deck's bounding-box centre in fractions of its tile, its clearance in metres
//! (0 for a deck laid flat), its median top in NAP (`null` for a flat deck) and
//! its profile (`DeckProfile::to_wire`, `null` without one). Every cell the
//! grading tessellated is written, including ones with no decks, so a reader can
//! tell "no decks here" from "not measured" — which is why `decks` is required
//! here rather than defaulted.
//!
//! The writer always writes all five elements; the top and the profile were
//! appended after cells were cached, so a reader takes a row of three. Rows are
//! read **one at a time**, as the hand-written readers before this did: a row
//! that is not a deck (no centre, no height) is dropped, a `top` or `profile`
//! that is not numbers reads as absent, and neither fails the cell.

use serde::{Deserialize, Serialize};

/// One measured deck.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Deck {
    /// Bounding-box centre, in fractions of the tile.
    pub at: [f64; 2],
    /// Clearance in metres; 0 for a deck laid flat.
    pub height: f64,
    /// Median top in metres NAP.
    pub top: Option<f64>,
    /// `DeckProfile::to_wire`, in tile fractions.
    pub profile: Option<Vec<f64>>,
}

impl Deck {
    /// One row, leniently: `None` for a row without a centre and a height.
    fn from_row(row: &serde_json::Value) -> Option<Self> {
        let d = row.as_array()?;
        Some(Self {
            at: [d.first()?.as_f64()?, d.get(1)?.as_f64()?],
            height: d.get(2)?.as_f64()?,
            top: d.get(3).and_then(serde_json::Value::as_f64),
            profile: d.get(4).and_then(|p| {
                p.as_array()?
                    .iter()
                    .map(serde_json::Value::as_f64)
                    .collect()
            }),
        })
    }
}

impl Serialize for Deck {
    fn serialize<S: serde::Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        use serde::ser::SerializeTuple;
        let mut t = s.serialize_tuple(5)?;
        t.serialize_element(&self.at[0])?;
        t.serialize_element(&self.at[1])?;
        t.serialize_element(&self.height)?;
        t.serialize_element(&self.top)?;
        t.serialize_element(&self.profile)?;
        t.end()
    }
}

/// One cell file.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Cell {
    #[serde(deserialize_with = "lenient_rows")]
    pub decks: Vec<Deck>,
}

fn lenient_rows<'de, D: serde::Deserializer<'de>>(d: D) -> Result<Vec<Deck>, D::Error> {
    let rows = Vec::<serde_json::Value>::deserialize(d)?;
    Ok(rows.iter().filter_map(Deck::from_row).collect())
}

/// Parse a cell file.
///
/// # Errors
///
/// When the text is not JSON, or has no `decks` array.
pub fn parse(text: &str) -> Result<Cell, String> {
    super::from_str(text)
}

/// [`parse`], from bytes.
///
/// # Errors
///
/// As [`parse`].
pub fn parse_slice(bytes: &[u8]) -> Result<Cell, String> {
    serde_json::from_slice(bytes).map_err(|e| e.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Captured from the old `json!({ "decks": rows })` writer: one lifted deck
    /// with a top and a profile, one laid flat with neither.
    const WRITTEN: &str = r#"{"decks":[[0.25,0.75,4.5,2.12345,[0.1,0.2,0.6,0.8,0.0,1.5,0.01,2.5]],[0.5,0.5,0.0,null,null]]}"#;

    fn sample() -> Cell {
        Cell {
            decks: vec![
                Deck {
                    at: [0.25, 0.75],
                    height: 4.5,
                    top: Some(2.12345),
                    profile: Some(vec![0.1, 0.2, 0.6, 0.8, 0.0, 1.5, 0.01, 2.5]),
                },
                Deck {
                    at: [0.5, 0.5],
                    height: 0.0,
                    top: None,
                    profile: None,
                },
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
        assert_eq!(parse_slice(WRITTEN.as_bytes()).unwrap(), sample());
    }

    /// The old writer itself, rebuilt: `json!([x, y, height, top, profile])`.
    #[test]
    fn the_typed_writer_matches_the_json_macro_it_replaced() {
        let rows: Vec<serde_json::Value> = sample()
            .decks
            .iter()
            .map(|d| serde_json::json!([d.at[0], d.at[1], d.height, d.top, d.profile]))
            .collect();
        let old = serde_json::to_vec(&serde_json::json!({ "decks": rows })).unwrap();
        assert_eq!(text(&sample()).into_bytes(), old);
    }

    /// A file from before the top and the profile still reads.
    #[test]
    fn a_three_element_row_reads() {
        let cell = parse(r#"{"decks":[[0.25,0.75,4.5]]}"#).unwrap();
        assert_eq!(
            cell.decks,
            vec![Deck {
                at: [0.25, 0.75],
                height: 4.5,
                top: None,
                profile: None,
            }]
        );
    }

    #[test]
    fn a_bad_row_is_dropped_not_the_cell() {
        let cell = parse(r#"{"decks":[[0.25],"x",[0.5,0.5,1.0,"t",[1,"p"]]]}"#).unwrap();
        assert_eq!(
            cell.decks,
            vec![Deck {
                at: [0.5, 0.5],
                height: 1.0,
                top: None,
                profile: None,
            }]
        );
    }

    /// "Measured, no decks" and "not a decks file" stay different answers.
    #[test]
    fn an_empty_list_is_an_answer_and_no_list_is_not() {
        assert_eq!(parse(r#"{"decks":[]}"#).unwrap(), Cell::default());
        assert!(parse("{}").is_err());
    }
}
