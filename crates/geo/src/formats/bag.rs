//! `bag/14/{x}/{y}.json` and `bag/14/{x}/{y}.places.json` — what the BAG says
//! about the buildings in a cell, and the addressed places in it.
//!
//! Written by atlas's `pipeline/bag.rs`; read by the client's
//! `building_facts::parse_bag_cell`. **Two files, one schema**: the buildings
//! file carries `buildings` and `districts`, the places file `addresses`,
//! `berths` and `streets`, and the reader parses either with [`Cell`]. The split
//! is by who pays — `lod22` fetches the buildings inside its mesh job and has no
//! use for two megabytes of addresses — so each array is optional here, written
//! (even empty) in the file it belongs to and absent from the other.
//!
//! Numbers are typed as narrowly as the client reads them. The old writer held
//! them as `i64`, which printed the same digits; a value out of the narrow range
//! now leaves the field out instead of failing the whole cell for every client.
//! The reader matches from the other side: every number is read leniently,
//! so a value that does not fit (negative, too large, fractional, or not a
//! number at all) is `None` for that field and the rest of the cell still reads.

use serde::{Deserialize, Deserializer, Serialize};

use super::LatLng;

/// An optional non-negative integer that becomes `None`, rather than an error,
/// when what the file holds does not fit `T`.
fn lenient<'de, D: Deserializer<'de>, T: TryFrom<u64>>(d: D) -> Result<Option<T>, D::Error> {
    Ok(Option::<serde_json::Value>::deserialize(d)?
        .and_then(|v| v.as_u64())
        .and_then(|v| T::try_from(v).ok()))
}

/// Either of a cell's two files.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Cell {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub addresses: Option<Vec<Address>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub berths: Option<Vec<Berth>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub buildings: Option<Vec<Building>>,
    /// The protected districts [`Building::district`] indexes. The whole table
    /// goes into every buildings file, so a cell explains its own indices.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub districts: Option<Vec<String>>,
    /// The street names [`Address::street`] and [`Berth::street`] index.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub streets: Option<Vec<String>>,
}

impl Cell {
    /// The buildings file.
    #[must_use]
    pub fn buildings(buildings: Vec<Building>, districts: Vec<String>) -> Self {
        Self {
            buildings: Some(buildings),
            districts: Some(districts),
            ..Self::default()
        }
    }

    /// The places file.
    #[must_use]
    pub fn places(streets: Vec<String>, addresses: Vec<Address>, berths: Vec<Berth>) -> Self {
        Self {
            addresses: Some(addresses),
            berths: Some(berths),
            streets: Some(streets),
            ..Self::default()
        }
    }
}

/// One pand.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Building {
    /// Floor area of the smallest unit inside, m².
    #[serde(
        default,
        deserialize_with = "lenient",
        skip_serializing_if = "Option::is_none"
    )]
    pub area: Option<u32>,
    /// Floor area of the largest, only when it differs from `area`.
    #[serde(
        default,
        deserialize_with = "lenient",
        skip_serializing_if = "Option::is_none"
    )]
    pub area_max: Option<u32>,
    /// Index into [`Cell::districts`].
    #[serde(
        default,
        deserialize_with = "lenient",
        skip_serializing_if = "Option::is_none"
    )]
    pub district: Option<usize>,
    /// The register's point for each addressed unit inside.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub doors: Vec<LatLng>,
    /// The outline, simplified, open.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub fp: Vec<LatLng>,
    /// The BAG identificatie, verbatim.
    pub id: String,
    /// The monument's main category, in the register's Dutch.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mcat: Option<String>,
    /// Rijksmonument number.
    #[serde(
        default,
        deserialize_with = "lenient",
        skip_serializing_if = "Option::is_none"
    )]
    pub mon: Option<u32>,
    /// A status code other than "in use" (`demolished`, `permit`, …).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub status: Option<String>,
    /// Addressable units inside.
    #[serde(
        default,
        deserialize_with = "lenient",
        skip_serializing_if = "Option::is_none"
    )]
    pub units: Option<u16>,
    /// `gebruiksdoel` bitmask — atlas's `USE_BITS` order.
    #[serde(
        default,
        deserialize_with = "lenient",
        rename = "use",
        skip_serializing_if = "Option::is_none"
    )]
    pub uses: Option<u16>,
    #[serde(
        default,
        deserialize_with = "lenient",
        skip_serializing_if = "Option::is_none"
    )]
    pub year: Option<u16>,
}

/// One addressable unit (`verblijfsobject`).
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Address {
    #[serde(
        default,
        deserialize_with = "lenient",
        skip_serializing_if = "Option::is_none"
    )]
    pub area: Option<u32>,
    pub at: LatLng,
    /// The pand it is in.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub id: Option<String>,
    /// House number with letter and suffix.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub nr: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub postcode: Option<String>,
    /// Index into [`Cell::streets`].
    #[serde(
        default,
        deserialize_with = "lenient",
        skip_serializing_if = "Option::is_none"
    )]
    pub street: Option<usize>,
    #[serde(
        default,
        deserialize_with = "lenient",
        rename = "use",
        skip_serializing_if = "Option::is_none"
    )]
    pub uses: Option<u16>,
}

/// A houseboat berth or a mobile-home plot.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Berth {
    pub at: LatLng,
    /// [`KIND_MOORING`] or [`KIND_PLOT`]; anything else reads as a mooring.
    pub kind: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub nr: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub postcode: Option<String>,
    #[serde(
        default,
        deserialize_with = "lenient",
        skip_serializing_if = "Option::is_none"
    )]
    pub street: Option<usize>,
}

/// [`Berth::kind`] for a `ligplaats`.
pub const KIND_MOORING: &str = "mooring";
/// [`Berth::kind`] for a `standplaats`.
pub const KIND_PLOT: &str = "plot";

/// Parse either file.
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

    const BUILDINGS: &str = concat!(
        r#"{"buildings":[{"area":62,"area_max":140,"district":0,"doors":[[53.2194001,6.5665002]],"#,
        r#""fp":[[53.2193,6.5664],[53.2195,6.5664],[53.2195,6.5667]],"id":"0014100010903891","#,
        r#""mcat":"Woningen en woningbouwcomplexen","mon":466587,"status":"demolished","units":3,"#,
        r#""use":5,"year":1885},{"id":"0014100010903892"}],"districts":["Groningen - Binnenstad"]}"#
    );

    const PLACES: &str = concat!(
        r#"{"addresses":[{"area":62,"at":[53.2194001,6.5665002],"id":"0014100010903891","nr":"62A","#,
        r#""postcode":"9712AB","street":0,"use":1},{"at":[53.2,6.5]}],"#,
        r#""berths":[{"at":[53.21,6.56],"kind":"mooring","nr":"1","street":0},"#,
        r#"{"at":[53.22,6.57],"kind":"plot"}],"streets":["Paterswoldseweg"]}"#
    );

    fn buildings() -> Cell {
        Cell::buildings(
            vec![
                Building {
                    area: Some(62),
                    area_max: Some(140),
                    district: Some(0),
                    doors: vec![(53.219_400_1, 6.566_500_2)],
                    fp: vec![(53.2193, 6.5664), (53.2195, 6.5664), (53.2195, 6.5667)],
                    id: "0014100010903891".into(),
                    mcat: Some("Woningen en woningbouwcomplexen".into()),
                    mon: Some(466_587),
                    status: Some("demolished".into()),
                    units: Some(3),
                    uses: Some(5),
                    year: Some(1885),
                },
                Building {
                    id: "0014100010903892".into(),
                    ..Building::default()
                },
            ],
            vec!["Groningen - Binnenstad".into()],
        )
    }

    fn places() -> Cell {
        Cell::places(
            vec!["Paterswoldseweg".into()],
            vec![
                Address {
                    area: Some(62),
                    at: (53.219_400_1, 6.566_500_2),
                    id: Some("0014100010903891".into()),
                    nr: Some("62A".into()),
                    postcode: Some("9712AB".into()),
                    street: Some(0),
                    uses: Some(1),
                },
                Address {
                    at: (53.2, 6.5),
                    ..Address::default()
                },
            ],
            vec![
                Berth {
                    at: (53.21, 6.56),
                    kind: KIND_MOORING.into(),
                    nr: Some("1".into()),
                    postcode: None,
                    street: Some(0),
                },
                Berth {
                    at: (53.22, 6.57),
                    kind: KIND_PLOT.into(),
                    nr: None,
                    postcode: None,
                    street: None,
                },
            ],
        )
    }

    fn text(cell: &Cell) -> String {
        String::from_utf8(super::super::to_vec(cell).unwrap()).unwrap()
    }

    #[test]
    fn both_files_round_trip() {
        for cell in [buildings(), places()] {
            assert_eq!(parse(&text(&cell)).unwrap(), cell);
        }
    }

    #[test]
    fn the_old_writer_s_bytes_parse_and_are_reproduced() {
        assert_eq!(parse(BUILDINGS).unwrap(), buildings());
        assert_eq!(parse(PLACES).unwrap(), places());
        assert_eq!(text(&buildings()), BUILDINGS);
        assert_eq!(text(&places()), PLACES);
    }

    /// Empty arrays are still written in the file they belong to — the old
    /// writer's `json!` always wrote them.
    #[test]
    fn an_empty_table_is_still_written() {
        assert_eq!(
            text(&Cell::buildings(Vec::new(), Vec::new())),
            r#"{"buildings":[],"districts":[]}"#
        );
    }

    /// A number that does not fit its field costs that field, never the cell.
    #[test]
    fn an_out_of_range_number_is_only_that_field() {
        let cell = parse(
            r#"{"buildings":[{"id":"a","year":70000,"units":-1,"use":2.5,"area":"x","mon":12,
                "district":null}],
                "addresses":[{"at":[53.2,6.5],"street":-3,"use":1}]}"#,
        )
        .unwrap();
        let buildings = cell.buildings.unwrap();
        let b = &buildings[0];
        assert_eq!((b.year, b.units, b.uses, b.area), (None, None, None, None));
        assert_eq!((b.mon, b.district), (Some(12), None));
        let addresses = cell.addresses.unwrap();
        let a = &addresses[0];
        assert_eq!((a.street, a.uses), (None, Some(1)));
    }
}
