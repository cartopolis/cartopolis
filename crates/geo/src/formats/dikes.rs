//! `dikes/14/{x}/{y}.json` — the registered flood defences in a cell.
//!
//! Written by atlas's `pipeline/dikes.rs`; read by the client's
//! `dikes::parse_dike_cell`. Each line is a row, `[kind, category, name_index,
//! [[lat, lng], …]]`, with `name_index` −1 for an unnamed defence and the two
//! small integers indexing the wire-order `DIKE_KINDS` / `DIKE_CATEGORIES`
//! tables. `crests` stands *beside* `lines` rather than inside each row because
//! every client installed before it existed reads a row as exactly four
//! elements and would refuse a fifth.

use serde::{Deserialize, Serialize};

use super::LatLng;

/// One defence: `[kind, category, name_index, points]`.
pub type Line = (usize, usize, i64, Vec<LatLng>);

/// One cell file.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Cell {
    /// The crest atlas measured for each line, metres NAP, `null` where it did
    /// not. Absent from files written before it measured any; a reader measures
    /// those itself.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub crests: Option<Vec<Option<f64>>>,
    #[serde(default)]
    pub lines: Vec<Line>,
    /// The names `lines` index.
    #[serde(default)]
    pub names: Vec<String>,
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

    const WRITTEN: &str = concat!(
        r#"{"crests":[6.34,null],"lines":[[0,1,0,[[53.4,6.2],[53.4001,6.2002]]],"#,
        r#"[3,4,-1,[[53.41,6.21],[53.411,6.211]]]],"names":["Lauwersmeerdijk"]}"#
    );

    fn sample() -> Cell {
        Cell {
            crests: Some(vec![Some(6.34), None]),
            lines: vec![
                (0, 1, 0, vec![(53.4, 6.2), (53.4001, 6.2002)]),
                (3, 4, -1, vec![(53.41, 6.21), (53.411, 6.211)]),
            ],
            names: vec!["Lauwersmeerdijk".into()],
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
    fn a_file_from_before_the_crests_reads() {
        let old = parse(r#"{"names":[],"lines":[[0,0,-1,[[53.0,6.0],[53.1,6.1]]]]}"#).unwrap();
        assert_eq!(old.crests, None);
        assert_eq!(old.lines.len(), 1);
    }
}
