//! `<dataset>/coverage.json` — the manifest that says which cells a tree holds.
//!
//! Every dataset atlas publishes has one beside its cells, and every streamer in
//! the client reads one before it asks for a cell (`systems::coverage`'s
//! `CoverageSlot`, `osm_buildings`' two 3DBAG manifests, and the stamped
//! `lod3` and `designs` manifests that are polled while the app runs). They are
//! **one shape with per-dataset extras**, surveyed 2026-10-07 across the fourteen
//! writers:
//!
//! * `zoom` and `cells` — every manifest. A cell is `[x, y]`, or `[x, y, stamp]`
//!   in the two trees edited while clients run (`atlas::stamped`), where the
//!   stamp goes into the cell's cache key.
//! * `name`, `attribution`, `source`, `bbox_rd` — every extraction run's manifest
//!   (the stamped ones carry only `attribution`, and `roads`/`decks` no `source`
//!   or `bbox_rd`).
//! * `graded` and `graded_stamp` (terrain: the tiles atlas also graded, and the
//!   run that did) and `stamp` (dikes: the extraction run) — read by the client,
//!   so typed here.
//! * Everything else is the dataset's own and no client reads it: the
//!   `*_count` totals (atlas's web UI reads those generically, by suffix, in file
//!   order — `atlas::coverage`), the wire-order tables a dataset publishes for a
//!   person to read (`kinds`, `categories`, `line_kinds`, …), `lod`, `year`,
//!   `districts`, and the lod3 manifest's `no_interior` and `pands`. They ride in
//!   [`Manifest::extra`].
//!
//! # Byte compatibility
//!
//! The old writers were `json!` objects, so the files have their keys sorted.
//! A struct with a flattened map cannot emit that order itself (its own fields
//! come first), so [`to_vec`] goes through a `serde_json::Value` — which sorts —
//! exactly as the `json!` did. That is safe here and not in the cell formats
//! because a manifest carries no `f32` to widen. Key order is not a promise to
//! readers, though: manifests from the TypeScript extractors that came before
//! atlas were written in insertion order, and may still be on a host.
//!
//! Readers are lenient about cells the way the hand-written ones were: a `cells`
//! entry that is not two integers is skipped, not the manifest; a negative stamp
//! reads as 0, and a missing one as `None`.

use serde::{Deserialize, Deserializer, Serialize, Serializer};
use serde_json::{Map, Value};

/// One listed cell: `[x, y]`, or `[x, y, stamp]` in a stamped tree.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub struct CellRef {
    pub x: i64,
    pub y: i64,
    pub stamp: Option<u64>,
}

impl CellRef {
    /// An unstamped cell.
    #[must_use]
    pub const fn new(x: i64, y: i64) -> Self {
        Self { x, y, stamp: None }
    }

    /// A stamped cell.
    #[must_use]
    pub const fn stamped(x: i64, y: i64, stamp: u64) -> Self {
        Self {
            x,
            y,
            stamp: Some(stamp),
        }
    }

    /// The cell as `(x, y)`.
    #[must_use]
    pub const fn xy(&self) -> (i64, i64) {
        (self.x, self.y)
    }

    /// One entry, leniently: `None` unless it is an array starting with two
    /// integers.
    fn from_entry(entry: &Value) -> Option<Self> {
        let a = entry.as_array()?;
        Some(Self {
            x: a.first()?.as_i64()?,
            y: a.get(1)?.as_i64()?,
            stamp: a.get(2).and_then(Value::as_i64).map(|s| s.max(0) as u64),
        })
    }
}

impl From<[i64; 2]> for CellRef {
    fn from([x, y]: [i64; 2]) -> Self {
        Self::new(x, y)
    }
}

impl From<(i64, i64)> for CellRef {
    fn from((x, y): (i64, i64)) -> Self {
        Self::new(x, y)
    }
}

impl Serialize for CellRef {
    fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        match self.stamp {
            Some(stamp) => (self.x, self.y, stamp).serialize(s),
            None => (self.x, self.y).serialize(s),
        }
    }
}

fn lenient_cells<'de, D: Deserializer<'de>>(d: D) -> Result<Vec<CellRef>, D::Error> {
    // A `cells` that is not an array reads as none, as every reader before this
    // treated it.
    let v = Value::deserialize(d)?;
    Ok(v.as_array()
        .map(|a| a.iter().filter_map(CellRef::from_entry).collect())
        .unwrap_or_default())
}

fn lenient_cells_opt<'de, D: Deserializer<'de>>(d: D) -> Result<Option<Vec<CellRef>>, D::Error> {
    lenient_cells(d).map(Some)
}

/// One manifest. Optional keys are omitted when `None`; fields are declared in
/// alphabetical order for the reader's benefit, though [`to_vec`] sorts anyway.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Manifest {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub attribution: Option<String>,
    /// The extracted area in RD New metres, `[min_x, min_y, max_x, max_y]`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub bbox_rd: Option<Vec<f64>>,
    #[serde(default, deserialize_with = "lenient_cells")]
    pub cells: Vec<CellRef>,
    /// Terrain only: the tiles atlas has graded as well as cut.
    #[serde(
        default,
        deserialize_with = "lenient_cells_opt",
        skip_serializing_if = "Option::is_none"
    )]
    pub graded: Option<Vec<CellRef>>,
    /// Terrain only: the grading run, which goes into a graded tile's URL and
    /// cache key.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub graded_stamp: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source: Option<String>,
    /// The extraction run, for a tree whose client keys its cache on it (dikes).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub stamp: Option<u64>,
    /// Always written; a reader decides what a manifest without one means.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub zoom: Option<u32>,
    /// The dataset's own keys — see the module note.
    #[serde(flatten)]
    pub extra: Map<String, Value>,
}

impl Manifest {
    /// A manifest at `zoom` listing `cells`, with nothing else in it yet.
    #[must_use]
    pub fn new<C: Into<CellRef>>(zoom: u32, cells: impl IntoIterator<Item = C>) -> Self {
        Self {
            zoom: Some(zoom),
            cells: cells.into_iter().map(Into::into).collect(),
            ..Self::default()
        }
    }

    /// The four keys every extraction run writes.
    #[must_use]
    pub fn described(
        mut self,
        name: impl Into<String>,
        attribution: impl Into<String>,
        source: impl Into<String>,
        bbox_rd: [f64; 4],
    ) -> Self {
        self.name = Some(name.into());
        self.attribution = Some(attribution.into());
        self.source = Some(source.into());
        self.bbox_rd = Some(bbox_rd.to_vec());
        self
    }

    /// Add one of the dataset's own keys — a `*_count`, a wire-order table.
    #[must_use]
    pub fn with(mut self, key: &str, value: impl Into<Value>) -> Self {
        self.extra.insert(key.to_string(), value.into());
        self
    }

    /// The listed cells as `(x, y)`, stamps dropped.
    pub fn cell_set(&self) -> impl Iterator<Item = (i64, i64)> + '_ {
        self.cells.iter().map(CellRef::xy)
    }
}

/// Serialise a manifest the way the files are written: compact, keys sorted.
///
/// # Errors
///
/// Only what `serde_json` itself can fail on, which a `Manifest` cannot.
pub fn to_vec(manifest: &Manifest) -> Result<Vec<u8>, serde_json::Error> {
    serde_json::to_vec(&serde_json::to_value(manifest)?)
}

/// Parse a manifest.
///
/// # Errors
///
/// When the text is not JSON, or not an object, or a typed key has the wrong
/// type.
pub fn parse(text: &str) -> Result<Manifest, String> {
    super::from_str(text)
}

/// [`parse`], from bytes.
///
/// # Errors
///
/// As [`parse`].
pub fn parse_slice(bytes: &[u8]) -> Result<Manifest, String> {
    serde_json::from_slice(bytes).map_err(|e| e.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    const BBOX: [f64; 4] = [229_000.0, 578_000.0, 238_000.0, 587_000.0];

    fn text(m: &Manifest) -> String {
        String::from_utf8(to_vec(m).unwrap()).unwrap()
    }

    fn old(v: &Value) -> String {
        serde_json::to_string(v).unwrap()
    }

    /// The common extraction-run shape (bag, brp, bgt, buildings, lod22, roofs,
    /// water, tall), against the `json!` it replaced.
    #[test]
    fn an_extraction_manifest_is_byte_identical() {
        let cells = vec![[8490_i64, 5319_i64], [8490, 5320]];
        let was = json!({
            "name": "groningen",
            "zoom": 14,
            "attribution": "x",
            "source": "y",
            "bbox_rd": BBOX,
            "year": Some(2025_i64),
            "categories": ["a", "b"],
            "cells": cells,
            "parcel_count": 12_usize,
        });
        let now = Manifest::new(14, cells.clone())
            .described("groningen", "x", "y", BBOX)
            .with("year", Some(2025_i64))
            .with("categories", &["a", "b"][..])
            .with("parcel_count", 12_usize);
        assert_eq!(text(&now), old(&was));
        assert_eq!(parse(&old(&was)).unwrap(), now);
    }

    /// Terrain's grading keys, and a `None` year written as `null` as before.
    #[test]
    fn the_terrain_and_dikes_keys_are_byte_identical() {
        let was = json!({
            "attribution": "a",
            "bbox_rd": BBOX,
            "cells": [[3, 4], [1, 2]],
            "graded": [[1, 2]],
            "graded_stamp": 1_700_000_000_u64,
            "name": "n",
            "source": "s",
            "stamp": 5_u64,
            "tile_count": 2_usize,
            "year": Option::<i64>::None,
            "zoom": 14,
        });
        let now = Manifest {
            graded: Some(vec![CellRef::new(1, 2)]),
            graded_stamp: Some(1_700_000_000),
            stamp: Some(5),
            ..Manifest::new(14, [[3_i64, 4], [1, 2]]).described("n", "a", "s", BBOX)
        }
        .with("tile_count", 2_usize)
        .with("year", Option::<i64>::None);
        assert_eq!(text(&now), old(&was));
    }

    /// `roads` and `decks`: no source, no bbox.
    #[test]
    fn a_bare_manifest_is_byte_identical() {
        let was = json!({"attribution": "AHN", "cells": [[1, 2]], "name": "n", "zoom": 14});
        let now = Manifest {
            attribution: Some("AHN".into()),
            name: Some("n".into()),
            ..Manifest::new(14, [(1_i64, 2_i64)])
        };
        assert_eq!(text(&now), old(&was));
    }

    /// The stamped trees: `[x, y, stamp]` and whatever extras the tree carries.
    #[test]
    fn a_stamped_manifest_is_byte_identical() {
        let mut extra = Map::new();
        extra.insert(
            "attribution".into(),
            "Building designs by their owners".into(),
        );
        let mut was = extra.clone();
        was.insert("zoom".into(), 14.into());
        let listed: Vec<(i64, i64, u64)> = vec![(1, 2, 7), (3, 4, 9)];
        was.insert("cells".into(), serde_json::to_value(&listed).unwrap());
        let now = Manifest {
            zoom: Some(14),
            cells: vec![CellRef::stamped(1, 2, 7), CellRef::stamped(3, 4, 9)],
            extra,
            ..Manifest::default()
        };
        assert_eq!(text(&now), old(&Value::Object(was)));
        let back = parse(&text(&now)).unwrap();
        assert_eq!(
            back.attribution.as_deref(),
            Some("Building designs by their owners")
        );
        assert_eq!(back.cells, now.cells);
    }

    #[test]
    fn a_bad_cell_is_skipped_not_the_manifest() {
        let m = parse(r#"{"zoom":14,"cells":[[1,2],[3],"x",[5,6,-1],[7,8,9]]}"#).unwrap();
        assert_eq!(
            m.cells,
            vec![
                CellRef::new(1, 2),
                CellRef::stamped(5, 6, 0),
                CellRef::stamped(7, 8, 9)
            ]
        );
    }

    /// What a host that does not serve a tree is remembered as.
    #[test]
    fn an_empty_object_is_an_empty_manifest() {
        assert_eq!(parse("{}").unwrap(), Manifest::default());
    }

    /// Insertion-ordered manifests (the TypeScript extractors') read the same.
    #[test]
    fn key_order_does_not_matter_to_a_reader() {
        let a = parse(r#"{"zoom":14,"cells":[[1,2]],"attribution":"x"}"#).unwrap();
        let b = parse(r#"{"attribution":"x","cells":[[1,2]],"zoom":14}"#).unwrap();
        assert_eq!(a, b);
        assert_eq!(a.zoom, Some(14));
    }
}
