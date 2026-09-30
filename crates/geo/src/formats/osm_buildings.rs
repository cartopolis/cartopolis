//! `buildings/14/{x}/{y}.json` — 3DBAG LoD1.2 buildings in the shape of an
//! Overpass `out geom` answer.
//!
//! Written by atlas's `pipeline/buildings.rs`; read by the client's
//! `osm_buildings::parse_buildings`, which reads **real Overpass answers with the
//! same types** — that is why the host serves this shape at all. So the reader
//! half is shaped by what Overpass sends, not only by what atlas writes, and it
//! is **lenient per element**: one odd element never costs the document.
//!
//! * An [`Element`] is read field by field out of a JSON value and cannot fail:
//!   a point missing `lat` or `lon` (or a clipped `out geom` node, which is
//!   `null`) is `None` in `geometry`, a non-integer `id` is 0, a missing or
//!   non-object `tags` is empty, a missing `type` is empty (and so not a way).
//!   Nodes carry no `geometry` and relations carry `members`; the reader skips
//!   anything that is not a `way`.
//! * `elements` is optional when reading, so a document can be told apart from
//!   one without it: the client answers "no elements" with an error, and a
//!   `remark` (Overpass's in-band timeout report, HTTP 200) with its own error,
//!   whether or not `elements` came with it. `remark` is read only if it is a
//!   string. atlas never writes it.
//! * `tags` is a JSON object of JSON values rather than of strings: Overpass
//!   sends strings, but the client's tag readers already accept a number where a
//!   height is expected.

use serde::{Deserialize, Deserializer, Serialize};
use serde_json::Value;

/// A way's OSM tags.
pub type Tags = serde_json::Map<String, Value>;

/// One cell file (or one Overpass answer).
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Cell {
    /// Always written. `None` on reading means the document had no `elements`
    /// array, which is not an answer and must not be cached as an empty cell.
    #[serde(default)]
    pub elements: Option<Vec<Element>>,
    /// Overpass's in-band error report. Never written by atlas.
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "lenient_remark"
    )]
    pub remark: Option<String>,
}

/// One OSM element. atlas writes only ways.
#[derive(Clone, Debug, Default, PartialEq, Serialize)]
pub struct Element {
    /// The way's nodes, in order; `None` where Overpass clipped one away or a
    /// point was unreadable.
    pub geometry: Vec<Option<LatLon>>,
    pub id: i64,
    pub tags: Tags,
    /// `"way"`, `"node"` or `"relation"`.
    #[serde(rename = "type")]
    pub kind: String,
}

impl Element {
    /// Read an element out of whatever JSON Overpass sent, taking what is
    /// readable — the rules the client's `serde_json::Value` walk always had.
    #[must_use]
    pub fn from_value(v: &Value) -> Self {
        let point = |p: &Value| {
            Some(LatLon {
                lat: p.get("lat")?.as_f64()?,
                lon: p.get("lon")?.as_f64()?,
            })
        };
        Self {
            geometry: v
                .get("geometry")
                .and_then(Value::as_array)
                .map(|g| g.iter().map(point).collect())
                .unwrap_or_default(),
            id: v.get("id").and_then(Value::as_i64).unwrap_or(0),
            tags: v
                .get("tags")
                .and_then(Value::as_object)
                .cloned()
                .unwrap_or_default(),
            kind: v
                .get("type")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_string(),
        }
    }
}

impl<'de> Deserialize<'de> for Element {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        Ok(Self::from_value(&Value::deserialize(d)?))
    }
}

/// `remark` when it is a string; anything else is not a remark.
fn lenient_remark<'de, D: Deserializer<'de>>(d: D) -> Result<Option<String>, D::Error> {
    Ok(Option::<Value>::deserialize(d)?.and_then(|v| v.as_str().map(str::to_string)))
}

/// A node position as Overpass spells it.
#[derive(Clone, Copy, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct LatLon {
    pub lat: f64,
    pub lon: f64,
}

/// Parse a cell file or an Overpass answer. Fails only on text that is not a
/// JSON object of this shape; `elements` and `remark` are for the caller to
/// judge.
///
/// # Errors
///
/// When the text is not JSON, or `elements` is present and not an array.
pub fn parse(text: &str) -> Result<Cell, String> {
    super::from_str(text)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// What `pipeline/buildings.rs` wrote before this module existed: a `json!`
    /// value, so every object's keys sorted.
    const WRITTEN: &str = concat!(
        r#"{"elements":[{"geometry":[{"lat":53.2194,"lon":6.5665},{"lat":53.21945,"lon":6.56655},"#,
        r#"{"lat":53.2195,"lon":6.5665},{"lat":53.2194,"lon":6.5665}],"id":10777377,"#,
        r#""tags":{"building":"yes","building:levels":"2","height":"9.00","roof:height":"3.00","#,
        r#""roof:shape":"gabled"},"type":"way"}]}"#
    );

    fn sample() -> Cell {
        let mut tags = Tags::new();
        for (k, v) in [
            ("building", "yes"),
            ("building:levels", "2"),
            ("height", "9.00"),
            ("roof:shape", "gabled"),
            ("roof:height", "3.00"),
        ] {
            tags.insert(k.into(), v.into());
        }
        let at = |lat, lon| Some(LatLon { lat, lon });
        Cell {
            elements: Some(vec![Element {
                geometry: vec![
                    at(53.2194, 6.5665),
                    at(53.21945, 6.56655),
                    at(53.2195, 6.5665),
                    at(53.2194, 6.5665),
                ],
                id: 10_777_377,
                tags,
                kind: "way".into(),
            }]),
            remark: None,
        }
    }

    #[test]
    fn a_cell_round_trips() {
        let cell = sample();
        let text = String::from_utf8(super::super::to_vec(&cell).unwrap()).unwrap();
        assert_eq!(parse(&text).unwrap(), cell);
    }

    #[test]
    fn the_old_writer_s_bytes_parse_and_are_reproduced() {
        assert_eq!(parse(WRITTEN).unwrap(), sample());
        assert_eq!(
            String::from_utf8(super::super::to_vec(&sample()).unwrap()).unwrap(),
            WRITTEN
        );
    }

    /// The reader also reads Overpass: nodes, relations, clipped geometry, a
    /// numeric tag, and the in-band remark.
    #[test]
    fn an_overpass_answer_parses() {
        let text = r#"{"version":0.6,"generator":"Overpass API","elements":[
            {"type":"node","id":1,"lat":53.0,"lon":6.0},
            {"type":"relation","id":2,"members":[{"type":"way","ref":3,"role":"outer"}]},
            {"type":"way","id":4,"geometry":[null,{"lat":53.0,"lon":6.0}],"tags":{"height":12}}
        ],"remark":"runtime error: Query timed out"}"#;
        let cell = parse(text).unwrap();
        let elements = cell.elements.unwrap();
        assert_eq!(elements.len(), 3);
        assert!(elements[0].geometry.is_empty());
        assert_eq!(elements[2].kind, "way");
        assert_eq!(elements[2].geometry[0], None);
        assert_eq!(elements[2].tags["height"], serde_json::json!(12));
        assert_eq!(
            cell.remark.as_deref(),
            Some("runtime error: Query timed out")
        );
    }

    /// One odd element costs itself, never the document: a point without `lon`
    /// is a hole in its way, a fractional id reads as 0, a non-object element
    /// reads as nothing. A remark with no elements still parses, so the caller
    /// can report the remark rather than a parse error.
    #[test]
    fn an_odd_element_costs_only_itself() {
        let cell = parse(
            r#"{"elements":[
                {"type":"way","id":1.5,"geometry":[{"lat":53.0},{"lat":53.0,"lon":6.0}],"tags":"x"},
                7
            ]}"#,
        )
        .unwrap();
        let elements = cell.elements.unwrap();
        assert_eq!(elements.len(), 2);
        assert_eq!(elements[0].id, 0);
        assert_eq!(
            elements[0].geometry,
            vec![
                None,
                Some(LatLon {
                    lat: 53.0,
                    lon: 6.0
                })
            ]
        );
        assert!(elements[0].tags.is_empty());
        assert_eq!(elements[1], Element::default());

        let remark = parse(r#"{"remark":"x"}"#).unwrap();
        assert_eq!(
            (remark.elements, remark.remark.as_deref()),
            (None, Some("x"))
        );
        assert_eq!(
            parse("{}").unwrap().elements,
            None,
            "no elements is no answer"
        );
    }
}
