//! `lod22/14/{x}/{y}.json` — the JSON half of a LoD2.2 building cell.
//!
//! atlas's `pipeline/lod22.rs` writes every cell twice: this JSON, which every
//! installed client reads, and beside it the binary in
//! [`crate::lod22_cell`], which clients from 2026-09-26 on ask for first. Both
//! halves now go through `lod22_cell::Cell`: atlas builds that, and the JSON is
//! [`File::from`] it; the client parses the JSON with [`parse`] into the same
//! `Cell` the binary decodes to, so the two formats are compared element for
//! element. The imported-model (LoD3) files `tools/ifc_lod3.py` writes are the
//! same shape plus `class` and `pands`, and parse with the same code.
//!
//! An element is `{"id", "p": [[lat, lng, h], …], "part"?, "t": [[i, j, k,
//! roof], …]}`: `part` only when non-zero, `h` metres above the building's own
//! base.
//!
//! **Heights are `f32`**, as `lod22_cell` holds them, where the old writer
//! printed the `f64` it had rounded to a centimetre. For every height a building
//! has (under 10 km, two decimals: at most six significant digits) the shortest
//! `f32` representation is the same decimal, so the bytes are the same; the tests
//! pin a sample.

use serde::{Deserialize, Serialize};

use super::is_zero;
use crate::lod22_cell;

/// One cell file.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct File {
    pub elements: Vec<Element>,
    /// Only an imported-model cell names the shells it replaces.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub pands: Vec<String>,
}

/// One building part.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Element {
    /// The IFC class, on an imported model only.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub class: String,
    #[serde(default)]
    pub id: Id,
    /// Vertices: latitude°, longitude°, metres above the building's own base.
    #[serde(default)]
    pub p: Vec<(f64, f64, f32)>,
    /// Which solid of a split pand; written only when non-zero.
    #[serde(default, skip_serializing_if = "is_zero")]
    pub part: u32,
    /// The element's own colour, sRGB 0–255, on a model only (`lod3`). Absent
    /// from every shell cell, which therefore reads and writes byte for byte as
    /// before.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rgb: Option<[u8; 3]>,
    #[serde(default)]
    pub t: Vec<Tri>,
}

/// The element's BAG identificatie.
///
/// Written as a **string**. Read as a string or, from a cell written before the
/// extractor stopped mangling it into an integer, as a number — which joins with
/// nothing (it was the identificatie's digits shifted by the part suffix) and so
/// reads as no id. A strict `String` would fail every such cell, and a cache
/// entry that fails to parse is evicted and re-fetched: the first run after an
/// upgrade would have re-downloaded every cell.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Id(pub String);

impl Serialize for Id {
    fn serialize<S: serde::Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        s.serialize_str(&self.0)
    }
}

impl<'de> Deserialize<'de> for Id {
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        struct V;
        impl serde::de::Visitor<'_> for V {
            type Value = Id;
            fn expecting(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
                f.write_str("a BAG identificatie as a string, or a legacy number")
            }
            fn visit_str<E: serde::de::Error>(self, v: &str) -> Result<Id, E> {
                Ok(Id(v.to_string()))
            }
            fn visit_u64<E: serde::de::Error>(self, _: u64) -> Result<Id, E> {
                Ok(Id(String::new()))
            }
            fn visit_i64<E: serde::de::Error>(self, _: i64) -> Result<Id, E> {
                Ok(Id(String::new()))
            }
        }
        d.deserialize_any(V)
    }
}

/// A triangle: three indices into the element's `p` and a roof flag (1 = roof,
/// 0 = wall).
///
/// Always written with four elements. Read by a hand-written sequence visitor —
/// an `untagged` enum would buffer every row of a ~4 MB cell — and a row of
/// three reads as a wall, as the oldest files and the old lenient parser had it.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Tri(pub u32, pub u32, pub u32, pub u8);

impl Serialize for Tri {
    fn serialize<S: serde::Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        (self.0, self.1, self.2, self.3).serialize(s)
    }
}

impl<'de> Deserialize<'de> for Tri {
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        struct V;
        impl<'de> serde::de::Visitor<'de> for V {
            type Value = Tri;
            fn expecting(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
                f.write_str("[i, j, k, roof?]")
            }
            fn visit_seq<A: serde::de::SeqAccess<'de>>(self, mut seq: A) -> Result<Tri, A::Error> {
                let missing = || serde::de::Error::custom("bad triangle");
                let i = seq.next_element()?.ok_or_else(missing)?;
                let j = seq.next_element()?.ok_or_else(missing)?;
                let k = seq.next_element()?.ok_or_else(missing)?;
                let rf = seq.next_element()?.unwrap_or(0);
                Ok(Tri(i, j, k, rf))
            }
        }
        d.deserialize_seq(V)
    }
}

impl From<&lod22_cell::Element> for Element {
    fn from(e: &lod22_cell::Element) -> Self {
        Self {
            class: e.class.clone(),
            id: Id(e.id.clone()),
            p: e.p.clone(),
            part: e.part,
            rgb: e.rgb,
            t: e.t.iter().map(|&(i, j, k, rf)| Tri(i, j, k, rf)).collect(),
        }
    }
}

impl From<Element> for lod22_cell::Element {
    fn from(e: Element) -> Self {
        Self {
            id: e.id.0,
            part: e.part,
            class: e.class,
            p: e.p,
            rgb: e.rgb,
            t: e.t
                .into_iter()
                .map(|Tri(i, j, k, rf)| (i, j, k, rf))
                .collect(),
        }
    }
}

impl From<&lod22_cell::Cell> for File {
    fn from(c: &lod22_cell::Cell) -> Self {
        Self {
            elements: c.elements.iter().map(Element::from).collect(),
            pands: c.pands.clone(),
        }
    }
}

impl From<File> for lod22_cell::Cell {
    fn from(f: File) -> Self {
        Self {
            elements: f
                .elements
                .into_iter()
                .map(lod22_cell::Element::from)
                .collect(),
            pands: f.pands,
        }
    }
}

/// Parse a JSON cell into the shape the binary decodes to. Nothing is
/// validated here — out-of-range triangle indices included — so the consumer's
/// validation sees the same thing whichever format a cell came in.
///
/// # Errors
///
/// When the text is not JSON or not this document.
pub fn parse(text: &str) -> Result<lod22_cell::Cell, String> {
    super::from_str::<File>(text).map(lod22_cell::Cell::from)
}

/// The JSON for a cell, byte for byte what atlas has always written.
///
/// # Errors
///
/// Only what `serde_json` itself can fail on.
pub fn to_json(cell: &lod22_cell::Cell) -> Result<Vec<u8>, serde_json::Error> {
    super::to_vec(&File::from(cell))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The old writer's `json!({"id", "p", "t"})` plus `part` when non-zero, keys
    /// sorted, heights `f64` rounded to a centimetre.
    const WRITTEN: &str = concat!(
        r#"{"elements":[{"id":"0014100010904218","p":[[53.2262221,6.5540593,0.0],"#,
        r#"[53.2262429,6.5540435,6.32],[53.2262345,6.5541072,3.7]],"part":2,"t":[[0,1,2,1],[2,1,0,0]]},"#,
        r#"{"id":"0014100010904219","p":[[53.2,6.5,0.0],[53.2,6.5001,12.34],[53.2001,6.5,123.45]],"t":[[0,1,2,0]]}]}"#
    );

    fn sample() -> lod22_cell::Cell {
        lod22_cell::Cell {
            elements: vec![
                lod22_cell::Element {
                    id: "0014100010904218".into(),
                    part: 2,
                    class: String::new(),
                    p: vec![
                        (53.226_222_1, 6.554_059_3, 0.0),
                        (53.226_242_9, 6.554_043_5, 6.32),
                        (53.226_234_5, 6.554_107_2, 3.7),
                    ],
                    t: vec![(0, 1, 2, 1), (2, 1, 0, 0)],
                    rgb: None,
                },
                lod22_cell::Element {
                    id: "0014100010904219".into(),
                    part: 0,
                    class: String::new(),
                    p: vec![
                        (53.2, 6.5, 0.0),
                        (53.2, 6.5001, 12.34),
                        (53.2001, 6.5, 123.45),
                    ],
                    t: vec![(0, 1, 2, 0)],
                    rgb: None,
                },
            ],
            pands: Vec::new(),
        }
    }

    #[test]
    fn a_cell_round_trips() {
        let text = String::from_utf8(to_json(&sample()).unwrap()).unwrap();
        assert_eq!(parse(&text).unwrap(), sample());
    }

    #[test]
    fn the_old_writer_s_bytes_parse_and_are_reproduced() {
        assert_eq!(parse(WRITTEN).unwrap(), sample());
        assert_eq!(
            String::from_utf8(to_json(&sample()).unwrap()).unwrap(),
            WRITTEN
        );
    }

    /// An `f32` height prints as the centimetre decimal the old `f64` did, over
    /// every centimetre up to 1 km.
    #[test]
    fn f32_heights_print_as_their_centimetre() {
        for cm in 0..100_000u32 {
            let h = f64::from(cm) / 100.0;
            let as_f64 = serde_json::to_string(&h).unwrap();
            let as_f32 = serde_json::to_string(&(h as f32)).unwrap();
            assert_eq!(as_f32, as_f64, "{cm} cm");
        }
    }

    /// The legacy shapes: a numeric id, no id, a three-index triangle, and an
    /// imported model's `class` and `pands`.
    #[test]
    fn legacy_and_imported_cells_read() {
        let cell = parse(
            r#"{"elements":[{"id":14100010903891,"p":[[53.0,6.0,1.5]],"t":[[0,0,0]]},
                {"p":[],"t":[]},
                {"id":"x","class":"IfcWall","p":[],"t":[]}],"pands":["0014100010903891"]}"#,
        )
        .unwrap();
        assert_eq!(cell.elements[0].id, "");
        assert_eq!(cell.elements[0].t, vec![(0, 0, 0, 0)]);
        assert_eq!(cell.elements[1].id, "");
        assert_eq!(cell.elements[2].class, "IfcWall");
        assert_eq!(cell.pands, vec!["0014100010903891".to_string()]);
    }
}
