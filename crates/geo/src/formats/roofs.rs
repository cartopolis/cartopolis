//! `roofs/14/{x}/{y}.json` — what each building's roof is made of and what
//! colour it is, measured from the aerial photograph.
//!
//! Written by atlas's `pipeline/roofs.rs`, which samples PDOK's aerial imagery
//! inside every LoD2.2 roof; read by the client, which draws the roof with a
//! material texture tinted to the measured colour rather than with the photo
//! itself (`docs/notes/roof-colours-from-aerial-imagery.md` says why).
//!
//! Keyed by the BAG pand identificatie, the key `lod22` and `bag` carry. A
//! building has up to two measurements: its **pitched** planes and its **flat**
//! ones, because the ordinary Dutch terraced house is a tiled roof with a
//! bitumen extension behind it and one colour for both is wrong for both.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

/// What a roof surface looks like it is made of.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Material {
    /// Roof tiles or slate — every pitched roof that is not one of the below.
    /// The colour says which (red clay, anthracite glazed, grey concrete).
    Tile,
    /// A dark flat roof: bitumen or dark membrane.
    Bitumen,
    /// A mid-grey flat roof: ballast gravel, concrete, grey membrane.
    Gravel,
    /// A pale flat roof: white or light-grey membrane, light gravel.
    Light,
    /// Mostly vegetation: a sedum roof, or moss on an old tiled one.
    Green,
}

/// One measured surface of one building.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Surface {
    /// sRGB colour of the roof covering itself — with shadows and overhanging
    /// trees left out.
    pub c: [u8; 3],
    pub m: Material,
    /// Photo pixels the measurement rests on (~0.23 m each) — a confidence.
    pub n: u32,
}

/// One building's roof.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Roof {
    /// The flat planes, if it has any that could be measured.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub f: Option<Surface>,
    /// The pitched planes, if it has any that could be measured.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub p: Option<Surface>,
}

/// A paved surface's colour, measured from the photo where enough of it was
/// open to the sky.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct PavingColour {
    pub c: [u8; 3],
    /// Photo pixels it rests on.
    pub n: u32,
}

/// One cell file.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Cell {
    /// Paved surfaces from the `bgt` tree's paving file, keyed by
    /// [`paving_key`] of their first vertex — the surfaces carry no id, and an
    /// index into that file would silently point at another surface after the
    /// next `bgt` run.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub paving: BTreeMap<String, PavingColour>,
    #[serde(default)]
    pub roofs: BTreeMap<String, Roof>,
}

/// The key a paved surface is measured under: its outer ring's first vertex,
/// to the 7 decimals the paving file is written to. Atlas and the client both
/// format the same parsed numbers, so they agree.
#[must_use]
pub fn paving_key(lat: f64, lng: f64) -> String {
    format!("{lat:.7},{lng:.7}")
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

    const WRITTEN: &str = r#"{"roofs":{"0014100010903891":{"f":{"c":[62,61,60],"m":"bitumen","n":410},"p":{"c":[151,82,61],"m":"tile","n":1203}}}}"#;

    fn sample() -> Cell {
        let mut roofs = BTreeMap::new();
        roofs.insert(
            "0014100010903891".to_string(),
            Roof {
                f: Some(Surface {
                    c: [62, 61, 60],
                    m: Material::Bitumen,
                    n: 410,
                }),
                p: Some(Surface {
                    c: [151, 82, 61],
                    m: Material::Tile,
                    n: 1203,
                }),
            },
        );
        Cell {
            roofs,
            paving: BTreeMap::new(),
        }
    }

    #[test]
    fn a_cell_round_trips_byte_for_byte() {
        assert_eq!(parse(WRITTEN).unwrap(), sample());
        assert_eq!(
            String::from_utf8(super::super::to_vec(&sample()).unwrap()).unwrap(),
            WRITTEN
        );
    }
}
