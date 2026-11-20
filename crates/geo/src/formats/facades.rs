//! `facades/14/{x}/{y}.json` — what each building's walls are made of and what
//! colour they are, measured from a municipality's textured 3D city model.
//!
//! Written by atlas's `pipeline/facades.rs`; read by the client, which bakes the
//! colour into the wall's vertices and hands the material to the facade shader
//! in place of the guess it would otherwise make from the colour. The texture
//! itself is never shown: it carries the sun and the shadows of the day it was
//! flown, and parts of it are other buildings' roofs
//! (`docs/notes/facades-from-municipal-3d-models.md`).
//!
//! Keyed by the BAG pand identificatie, the key `lod22`, `bag` and `roofs`
//! carry. A building appears only where at least one of its walls could be
//! measured; every other building keeps the palette its era gives it.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

/// What a wall's face is, as far as a photograph at 20 cm a pixel can say.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Material {
    /// Fired clay brick: a warm, saturated face.
    Brick,
    /// Plaster or paint over the brick: a pale, unsaturated face.
    Render,
    /// Concrete, stone or metal panels: a mid grey.
    Panel,
}

/// One building's walls.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Facade {
    /// sRGB colour of the wall itself, measured on its sunlit side with the
    /// windows, the shadows and the glints left out.
    pub c: [u8; 3],
    pub m: Material,
    /// Texture pixels the measurement rests on (~0.2 m each) — a confidence.
    pub n: u32,
    /// Where it was measured when not from the city model: `mapillary` for
    /// street photographs, which a user asked for from the building's card.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub s: Option<String>,
}

/// What a roof face is covered with, as the city model's oblique photographs
/// at 10 cm a pixel show it, weighed against the building's age.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Covering {
    /// Fired clay pantiles, red to brown: courses ~30 cm apart.
    Pantile,
    /// Glazed or dark-fired tiles, anthracite to black.
    Glazed,
    /// Concrete tiles, grey, larger and flatter than clay.
    Concrete,
    /// Natural or fibre-cement slate, blue-grey, in small courses.
    Slate,
    /// Zinc, copper or steel sheet in standing seams.
    Metal,
    /// Photovoltaic panels covering most of the face.
    Solar,
    /// A flat roof's dark membrane.
    Bitumen,
    /// A flat roof's ballast gravel.
    Gravel,
    /// A flat roof's pale membrane.
    Light,
    /// Sedum or moss.
    Green,
    /// A covering a newer writer names that this reader does not know: drawn
    /// as the roofs layer would draw it.
    #[serde(other)]
    Unknown,
}

/// The sector a roof face falls in that is flat rather than facing a compass
/// direction.
pub const FLAT: u8 = 8;

/// One face of a building's roof: all of its planes that face one way.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct RoofFace {
    /// sRGB colour of the covering, the haze and the shadows left out.
    pub c: [u8; 3],
    /// Which way it faces: 0 north, clockwise in 45° sectors to 7, or
    /// [`FLAT`].
    pub d: u8,
    pub k: Covering,
    /// Texture pixels it rests on (~0.1 m each).
    pub n: u32,
}

/// The windows of one wall, as a lattice: the network's window blobs on the
/// city model's photograph, columns and storeys fitted through them.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Windows {
    /// The wall's left end at its foot, as somebody in front of it sees it.
    pub a: super::LatLng,
    /// …and its right end.
    pub b: super::LatLng,
    /// Metres from one column of windows to the next.
    pub bay: f32,
    /// A pane's height, metres.
    pub h: f32,
    /// Rows of windows seen, the lowest and every storey above it: none above
    /// the last, where the top is a parapet, an attic or a sign. 0 in files
    /// written before it was counted, which draws them to the eaves.
    #[serde(default, skip_serializing_if = "is_zero_u8")]
    pub n: u8,
    /// Metres from one storey's windows to the next; 0 where only one row
    /// was seen.
    pub st: f32,
    /// A pane's width, metres.
    pub w: f32,
    /// Metres from `a` along the wall to the first column's centre.
    pub x0: f32,
    /// Metres above the ground to the lowest row's centre.
    pub z0: f32,
}

#[allow(clippy::trivially_copy_pass_by_ref)] // serde passes a reference
fn is_zero_u8(v: &u8) -> bool {
    *v == 0
}

/// The sector a horizontal facing (east, north) falls in: 0 north, clockwise.
#[must_use]
pub fn sector(east: f64, north: f64) -> u8 {
    let deg = east.atan2(north).to_degrees().rem_euclid(360.0);
    ((deg / 45.0).round() as u8) % 8
}

/// One cell file.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Cell {
    #[serde(default)]
    pub facades: BTreeMap<String, Facade>,
    /// Roof faces by building. Absent from files written before roofs were
    /// measured, which a reader takes as "none measured".
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub roofs: BTreeMap<String, Vec<RoofFace>>,
    /// The measured window lattice of each wall the network read, by
    /// building. Absent from files written before windows were measured.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub windows: BTreeMap<String, Vec<Windows>>,
}

impl Cell {
    /// The face of `pand`'s roof facing sector `d` ([`FLAT`] for flat), or
    /// the nearest measured sector within 45° of it.
    #[must_use]
    pub fn roof_face(&self, pand: &str, d: u8) -> Option<&RoofFace> {
        let faces = self.roofs.get(pand)?;
        let exact = faces.iter().find(|f| f.d == d);
        if exact.is_some() || d == FLAT {
            return exact;
        }
        faces
            .iter()
            .find(|f| f.d != FLAT && (f.d + 1) % 8 == d || f.d != FLAT && (d + 1) % 8 == f.d)
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

#[cfg(test)]
mod tests {
    use super::*;

    const WRITTEN: &str =
        r#"{"facades":{"0014100010904448":{"c":[142,84,62],"m":"brick","n":3120}}}"#;

    fn sample() -> Cell {
        let mut facades = BTreeMap::new();
        facades.insert(
            "0014100010904448".to_string(),
            Facade {
                c: [142, 84, 62],
                m: Material::Brick,
                n: 3120,
                s: None,
            },
        );
        Cell {
            facades,
            roofs: BTreeMap::new(),
            windows: BTreeMap::new(),
        }
    }

    #[test]
    fn roof_faces_round_trip_and_an_unknown_covering_is_not_an_error() {
        let text = r#"{"facades":{},"roofs":{"p":[{"c":[150,70,50],"d":4,"k":"pantile","n":900},{"c":[30,35,60],"d":0,"k":"solar","n":400},{"c":[60,60,60],"d":8,"k":"bitumen","n":300}]}}"#;
        let cell = parse(text).unwrap();
        assert_eq!(
            String::from_utf8(super::super::to_vec(&cell).unwrap()).unwrap(),
            text
        );
        assert_eq!(cell.roof_face("p", 4).unwrap().k, Covering::Pantile);
        // South-west takes the south face; east has none within 45°.
        assert_eq!(cell.roof_face("p", 5).unwrap().k, Covering::Pantile);
        assert!(cell.roof_face("p", 2).is_none());
        assert_eq!(cell.roof_face("p", FLAT).unwrap().k, Covering::Bitumen);
        let newer = r#"{"roofs":{"p":[{"c":[1,2,3],"d":2,"k":"thatch","n":5}]}}"#;
        assert_eq!(parse(newer).unwrap().roofs["p"][0].k, Covering::Unknown);
    }

    #[test]
    fn sectors_run_clockwise_from_north() {
        assert_eq!(sector(0.0, 1.0), 0);
        assert_eq!(sector(1.0, 0.0), 2);
        assert_eq!(sector(0.0, -1.0), 4);
        assert_eq!(sector(-1.0, 0.0), 6);
    }

    #[test]
    fn a_cell_round_trips_byte_for_byte() {
        assert_eq!(parse(WRITTEN).unwrap(), sample());
        assert_eq!(
            String::from_utf8(super::super::to_vec(&sample()).unwrap()).unwrap(),
            WRITTEN
        );
    }

    #[test]
    fn an_empty_document_is_an_empty_cell() {
        assert_eq!(parse("{}").unwrap(), Cell::default());
    }
}
