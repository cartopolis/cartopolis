//! `bgt/14/{x}/{y}.json` and `bgt/14/{x}/{y}.surfaces.json` — the BGT's
//! surveyed street objects, and its paving.
//!
//! Written by atlas's `pipeline/bgt.rs`; read by the client's `surveyed.rs`
//! (and the paving, again, by atlas's own `terrain_grade`). Every object is a
//! **row**, a JSON array rather than an object, because there are tens of
//! thousands per cell. Two rows gained a trailing element after cells were
//! already cached — a line's measured height, a surface's material — so those
//! trailing elements are optional both ways: omitted when unknown, defaulted
//! when absent.
//!
//! The integers in the rows index wire-order tables — `PROP_KINDS`,
//! `LINE_KINDS`, `SURFACE_KINDS`, `SURFACE_MATERIALS`, `BRIDGE_PARTS` — declared
//! in atlas and mirrored by the client. Those tables stay where they are: each
//! side maps them onto its own vocabulary, and the order is pinned by tests on
//! both.

use serde::{Deserialize, Serialize};

use super::LatLng;

/// A street object: `[lat, lng, kind]`, kind indexing `PROP_KINDS`.
pub type Prop = (f64, f64, usize);

/// A bridge part: `[part, level, movable, [ring, …]]` — part indexing
/// `BRIDGE_PARTS`, level the BGT's `relatieve_hoogteligging`, rings outer first.
pub type BridgePart = (usize, i8, bool, Vec<Vec<LatLng>>);

/// A boundary: `[kind, [[lat, lng], …], height?]`, kind indexing `LINE_KINDS`,
/// and the height in metres only where the 3D product measured one.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Line(
    pub usize,
    pub Vec<LatLng>,
    #[serde(default, skip_serializing_if = "Option::is_none")] pub Option<f64>,
);

/// A paved surface: `[kind, [ring, …], material?]`, kind indexing
/// `SURFACE_KINDS`, material `SURFACE_MATERIALS` where the register names one.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Surface(
    pub usize,
    pub Vec<Vec<LatLng>>,
    #[serde(default, skip_serializing_if = "Option::is_none")] pub Option<usize>,
);

/// The objects file. All four arrays are always written.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Objects {
    #[serde(default)]
    pub bridges: Vec<BridgePart>,
    #[serde(default)]
    pub lines: Vec<Line>,
    #[serde(default)]
    pub props: Vec<Prop>,
    /// Surveyed trees, `[lat, lng]`.
    #[serde(default)]
    pub trees: Vec<LatLng>,
}

/// A walk below the street beside it: `[[ring, …], level, [seaward, …]]` — the
/// rings of one of the file's surfaces, outer first, its level in metres above
/// NAP as AHN measured it, and which of its edges face the water.
///
/// A lower quay is paving like any other in the register, which says nothing of
/// its height; atlas finds it by measuring the surfaces along a quay wall
/// against AHN (`pipeline/lower_quay.rs`). A key of its own rather than a
/// trailing element on [`Surface`], because a reader that predates it must
/// still read the file: serde ignores an unknown key and refuses an unknown
/// array element.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Terrace(
    pub Vec<Vec<LatLng>>,
    pub f64,
    /// Per edge of the outer ring (vertex `i` to `i + 1`, wrapping), whether it
    /// faces the water: the client lowers the ground between it and the map's
    /// water to the walk too, since that water need not reach the register's.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub Vec<bool>,
);

/// The paving file.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Paving {
    #[serde(default)]
    pub surfaces: Vec<Surface>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub terraces: Vec<Terrace>,
}

/// Parse an objects file.
///
/// # Errors
///
/// When the text is not JSON or not this document.
pub fn parse_objects(text: &str) -> Result<Objects, String> {
    super::from_str(text)
}

/// Parse a paving file.
///
/// # Errors
///
/// When the text is not JSON or not this document.
pub fn parse_paving(text: &str) -> Result<Paving, String> {
    super::from_str(text)
}

#[cfg(test)]
mod tests {
    use super::*;

    const OBJECTS: &str = concat!(
        r#"{"bridges":[[0,1,true,[[[53.21,6.56],[53.21,6.561],[53.211,6.561]]]]],"#,
        r#""lines":[[0,[[53.2,6.5],[53.2001,6.5001]]],[1,[[53.2,6.5],[53.2002,6.5]],2.35]],"#,
        r#""props":[[53.2194,6.5665,0],[53.2195,6.5666,6]],"trees":[[53.2196,6.5667]]}"#
    );

    const PAVING: &str = concat!(
        r#"{"surfaces":[[0,[[[53.2,6.5],[53.2,6.501],[53.201,6.501]]]],"#,
        r#"[2,[[[53.2,6.5],[53.2,6.501],[53.201,6.501]],[[53.2001,6.5001],[53.2002,6.5002],[53.2001,6.5002]]],4]]}"#
    );

    fn objects() -> Objects {
        Objects {
            bridges: vec![(
                0,
                1,
                true,
                vec![vec![(53.21, 6.56), (53.21, 6.561), (53.211, 6.561)]],
            )],
            lines: vec![
                Line(0, vec![(53.2, 6.5), (53.2001, 6.5001)], None),
                Line(1, vec![(53.2, 6.5), (53.2002, 6.5)], Some(2.35)),
            ],
            props: vec![(53.2194, 6.5665, 0), (53.2195, 6.5666, 6)],
            trees: vec![(53.2196, 6.5667)],
        }
    }

    fn paving() -> Paving {
        let outer = vec![(53.2, 6.5), (53.2, 6.501), (53.201, 6.501)];
        Paving {
            surfaces: vec![
                Surface(0, vec![outer.clone()], None),
                Surface(
                    2,
                    vec![
                        outer,
                        vec![(53.2001, 6.5001), (53.2002, 6.5002), (53.2001, 6.5002)],
                    ],
                    Some(4),
                ),
            ],
            terraces: Vec::new(),
        }
    }

    fn text<T: Serialize>(doc: &T) -> String {
        String::from_utf8(super::super::to_vec(doc).unwrap()).unwrap()
    }

    #[test]
    fn both_files_round_trip() {
        assert_eq!(parse_objects(&text(&objects())).unwrap(), objects());
        assert_eq!(parse_paving(&text(&paving())).unwrap(), paving());
    }

    #[test]
    fn the_old_writer_s_bytes_parse_and_are_reproduced() {
        assert_eq!(parse_objects(OBJECTS).unwrap(), objects());
        assert_eq!(parse_paving(PAVING).unwrap(), paving());
        assert_eq!(text(&objects()), OBJECTS);
        assert_eq!(text(&paving()), PAVING);
    }

    /// A lower quay rides beside the surfaces, and a file without one is written
    /// exactly as before.
    #[test]
    fn terraces_round_trip_and_cost_nothing_when_absent() {
        let mut p = paving();
        p.terraces
            .push(Terrace(
            vec![vec![(53.2, 6.5), (53.2, 6.501), (53.2001, 6.501)]],
            -0.82,
            vec![true, false, false],
        ));
        let t = text(&p);
        assert!(t.contains(r#""terraces":[[[[[53.2,6.5]"#), "{t}");
        assert_eq!(parse_paving(&t).unwrap(), p);
        assert!(!text(&paving()).contains("terraces"));
    }

    /// A file with only some arrays, as a cell cached before the bridges existed
    /// is, still reads.
    #[test]
    fn missing_arrays_default() {
        let old = parse_objects(r#"{"props":[[53.2,6.5,0]]}"#).unwrap();
        assert_eq!(old.props.len(), 1);
        assert!(old.trees.is_empty() && old.lines.is_empty() && old.bridges.is_empty());
    }
}
