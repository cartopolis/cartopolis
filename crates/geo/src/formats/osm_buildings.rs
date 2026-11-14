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

// ---------------------------------------------------------------------------
// The binary cell: `buildings/14/{x}/{y}.bin`
// ---------------------------------------------------------------------------
//
// atlas writes every 3DBAG cell twice, as the JSON above and as this encoding
// beside it; the client asks for the binary first and falls back to the JSON,
// as it does for the LoD2.2 cells (`lod22_cell`). The JSON spells every point
// as `{"lat":53.22630817120953,"lon":6.547981787831522}`: Groningen's densest
// cell, 8490/5319 (7,940 buildings), is 4.18 MB of it and 621 KB after Brotli
// (2026-11-14), which made these far blocks the largest JSON on a cold start.
//
// Little-endian. `b"CBB1"`, then three varints giving the byte lengths of three
// streams, then the streams — `meta`, `lat`, `lng` — which must fill the rest
// of the file exactly. Every number in them is an unsigned LEB128 varint, a
// signed one zigzagged first; a string is a varint byte length and its UTF-8.
//
// ```text
// meta: n_strings { string }            tag keys, tag values as JSON text, kinds
//       n_elements { kind  id_delta  n_points  gaps: u8  [present: u8]*n if gaps
//                    n_tags { key value } }
// lat:  one signed delta in 10⁻⁷° per present point, from the point before,
//       running through the whole file
// lng:  the same for longitude
// ```
//
// Points are on a 10⁻⁷° grid (~1 cm), the grid the LoD2.2 cells use; atlas's
// JSON carries more digits than its source measured. Tag values are JSON text
// so a number stays a number. Streams rather than records for the reason
// `lod22_cell` gives: each column is like itself and unlike the others.

const BIN_MAGIC: &[u8; 4] = b"CBB1";

/// Steps per degree on the binary cell's grid.
const PER_DEGREE: f64 = 1e7;

/// Whether `bytes` starts like a binary cell — the check before a body is
/// cached, so an HTML error page from a proxy is never stored as one.
#[must_use]
pub fn looks_like_bin(bytes: &[u8]) -> bool {
    bytes.starts_with(BIN_MAGIC)
}

/// Write a cell in the binary encoding. A `remark` is not written: atlas never
/// has one, and a cell with one is not an answer to cache.
#[must_use]
pub fn encode_bin(cell: &Cell) -> Vec<u8> {
    let elements = cell.elements.as_deref().unwrap_or_default();
    let mut strings: Vec<String> = Vec::new();
    let mut index: std::collections::HashMap<String, u64> = std::collections::HashMap::new();
    let mut intern = |s: String| -> u64 {
        *index.entry(s.clone()).or_insert_with(|| {
            strings.push(s);
            strings.len() as u64 - 1
        })
    };
    let (mut body, mut lat, mut lng) = (Vec::new(), Vec::new(), Vec::new());
    put_varint(&mut body, elements.len() as u64);
    let (mut prev_id, mut prev) = (0i64, (0i64, 0i64));
    for el in elements {
        put_varint(&mut body, intern(el.kind.clone()));
        put_zigzag(&mut body, el.id.wrapping_sub(prev_id));
        prev_id = el.id;
        put_varint(&mut body, el.geometry.len() as u64);
        let gaps = el.geometry.iter().any(Option::is_none);
        body.push(u8::from(gaps));
        for p in &el.geometry {
            if gaps {
                body.push(u8::from(p.is_some()));
            }
            if let Some(p) = p {
                let q = (
                    (p.lat * PER_DEGREE).round() as i64,
                    (p.lon * PER_DEGREE).round() as i64,
                );
                put_zigzag(&mut lat, q.0 - prev.0);
                put_zigzag(&mut lng, q.1 - prev.1);
                prev = q;
            }
        }
        put_varint(&mut body, el.tags.len() as u64);
        for (k, v) in &el.tags {
            put_varint(&mut body, intern(k.clone()));
            put_varint(&mut body, intern(v.to_string()));
        }
    }
    let mut meta = Vec::new();
    put_varint(&mut meta, strings.len() as u64);
    for s in &strings {
        put_varint(&mut meta, s.len() as u64);
        meta.extend_from_slice(s.as_bytes());
    }
    meta.extend_from_slice(&body);
    let mut out = BIN_MAGIC.to_vec();
    for s in [&meta, &lat, &lng] {
        put_varint(&mut out, s.len() as u64);
    }
    out.extend_from_slice(&meta);
    out.extend_from_slice(&lat);
    out.extend_from_slice(&lng);
    out
}

/// Read what [`encode_bin`] wrote.
///
/// # Errors
///
/// A wrong magic, streams that do not fill the file, a string or tag value
/// that does not read, a reference past the string table, or a stream that
/// ends early or has bytes left over.
pub fn decode_bin(bytes: &[u8]) -> Result<Cell, String> {
    if !looks_like_bin(bytes) {
        return Err("not a binary building cell".into());
    }
    let mut head = Bin {
        bytes: &bytes[4..],
        at: 0,
    };
    let lens = [head.len()?, head.len()?, head.len()?];
    let mut rest = &head.bytes[head.at..];
    if lens.iter().sum::<usize>() != rest.len() {
        return Err("streams do not fill the file".into());
    }
    let mut streams = lens.map(|n| {
        let (s, r) = rest.split_at(n);
        rest = r;
        Bin { bytes: s, at: 0 }
    });
    let [meta, lat, lng] = &mut streams;
    let strings: Vec<String> = (0..meta.len()?)
        .map(|_| meta.string())
        .collect::<Result<_, _>>()?;
    let string = |i: usize| strings.get(i).ok_or("string past the table");
    let n = meta.len()?;
    let mut elements = Vec::with_capacity(n.min(1 << 20));
    let (mut id, mut prev) = (0i64, (0i64, 0i64));
    for _ in 0..n {
        let kind = string(meta.len()?)?.clone();
        id = id.wrapping_add(meta.zigzag()?);
        // Bounded by the coordinates, not the meta stream: a present point
        // costs at least a byte of each, and a point costs no meta at all.
        let points = usize::try_from(meta.varint()?)
            .ok()
            .filter(|&p| p <= lat.bytes.len() + meta.bytes.len())
            .ok_or("point count past the file")?;
        let gaps = meta.u8()? != 0;
        let mut geometry = Vec::with_capacity(points.min(1 << 16));
        for _ in 0..points {
            if gaps && meta.u8()? == 0 {
                geometry.push(None);
                continue;
            }
            prev = (prev.0 + lat.zigzag()?, prev.1 + lng.zigzag()?);
            geometry.push(Some(LatLon {
                lat: prev.0 as f64 / PER_DEGREE,
                lon: prev.1 as f64 / PER_DEGREE,
            }));
        }
        let mut tags = Tags::new();
        for _ in 0..meta.len()? {
            let k = string(meta.len()?)?.clone();
            let v = serde_json::from_str(string(meta.len()?)?).map_err(|e| e.to_string())?;
            tags.insert(k, v);
        }
        elements.push(Element {
            geometry,
            id,
            tags,
            kind,
        });
    }
    if streams.iter().any(|s| s.at != s.bytes.len()) {
        return Err("bytes left in a stream".into());
    }
    Ok(Cell {
        elements: Some(elements),
        remark: None,
    })
}

fn put_varint(out: &mut Vec<u8>, mut v: u64) {
    while v >= 0x80 {
        out.push((v as u8) | 0x80);
        v >>= 7;
    }
    out.push(v as u8);
}

fn put_zigzag(out: &mut Vec<u8>, v: i64) {
    put_varint(out, ((v << 1) ^ (v >> 63)) as u64);
}

struct Bin<'a> {
    bytes: &'a [u8],
    at: usize,
}

impl Bin<'_> {
    fn u8(&mut self) -> Result<u8, String> {
        let b = *self.bytes.get(self.at).ok_or("binary cell ends early")?;
        self.at += 1;
        Ok(b)
    }
    fn varint(&mut self) -> Result<u64, String> {
        let mut v = 0u64;
        for shift in (0..64).step_by(7) {
            let b = self.u8()?;
            v |= u64::from(b & 0x7f) << shift;
            if b & 0x80 == 0 {
                return Ok(v);
            }
        }
        Err("varint too long".into())
    }
    /// A count, bounded by what is left: a count no stream could hold is a
    /// corrupt file, not an allocation to attempt.
    fn len(&mut self) -> Result<usize, String> {
        let v = self.varint()?;
        usize::try_from(v)
            .ok()
            .filter(|&v| v <= self.bytes.len().saturating_mul(8))
            .ok_or_else(|| format!("count {v} past the stream"))
    }
    fn zigzag(&mut self) -> Result<i64, String> {
        let v = self.varint()?;
        Ok(((v >> 1) as i64) ^ -((v & 1) as i64))
    }
    fn string(&mut self) -> Result<String, String> {
        let n = self.len()?;
        let end = self.at.checked_add(n).filter(|&e| e <= self.bytes.len());
        let s = &self.bytes[self.at..end.ok_or("binary cell ends early")?];
        self.at += n;
        String::from_utf8(s.to_vec()).map_err(|e| e.to_string())
    }
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

    /// The binary cell reads back as the JSON does, to its 10⁻⁷° grid — gaps,
    /// numeric tags and kinds included — and a damaged one is refused.
    #[test]
    fn a_binary_cell_round_trips() {
        let mut cell = sample();
        let mut other = cell.elements.as_ref().unwrap()[0].clone();
        other.id = 10_777_000;
        other.geometry[1] = None;
        other.tags.insert("height".into(), serde_json::json!(12.5));
        other.kind = "relation".into();
        cell.elements.as_mut().unwrap().push(other);
        let bytes = encode_bin(&cell);
        assert!(looks_like_bin(&bytes));
        let back = decode_bin(&bytes).unwrap();
        assert_eq!(back, cell);
        assert_eq!(encode_bin(&back), bytes);
        assert!(decode_bin(&bytes[..bytes.len() - 1]).is_err());
        assert!(decode_bin(WRITTEN.as_bytes()).is_err());

        let fine = LatLon {
            lat: 53.226_308_171_209_53,
            lon: 6.547_981_787_831_522,
        };
        let one = Cell {
            elements: Some(vec![Element {
                geometry: vec![Some(fine)],
                kind: "way".into(),
                ..Element::default()
            }]),
            remark: None,
        };
        let p = decode_bin(&encode_bin(&one)).unwrap().elements.unwrap()[0].geometry[0].unwrap();
        assert!((p.lat - fine.lat).abs() < 1e-7 && (p.lon - fine.lon).abs() < 1e-7);
    }

    /// A long way in a cell of few elements: its points cost no meta bytes, so
    /// the meta stream alone must not bound them.
    #[test]
    fn a_long_way_is_not_a_corrupt_count() {
        let geometry = (0..5000)
            .map(|i| {
                Some(LatLon {
                    // On the 10⁻⁷° grid, so it reads back exactly.
                    lat: f64::from(530_000_000 + i * 100) / 1e7,
                    lon: 6.0,
                })
            })
            .collect();
        let cell = Cell {
            elements: Some(vec![Element {
                geometry,
                id: 1,
                kind: "way".into(),
                ..Element::default()
            }]),
            remark: None,
        };
        assert_eq!(decode_bin(&encode_bin(&cell)).unwrap(), cell);
    }
}
