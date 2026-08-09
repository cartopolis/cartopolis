//! The two tile layers that were being decoded and dropped: `pois` (467 features
//! in the Groningen z14 fixture) and `addresses` (6,211).
//!
//! `map_geometry` already fetches and parses this tile body for the surfaces, the
//! roads, the trees, the furniture and the street labels, so reading two more
//! layers out of the same `TileData` costs **no request and no parse** — see
//! `docs/notes/vector-tile-unused-layers.md`, which surveyed exactly this and
//! listed both layers as used for "nothing".
//!
//! What they buy is the thing an avatar walking around a city most conspicuously
//! lacked: something to walk *up to*. A POI here is a place with a name and a
//! position; the client turns it into a proximity prompt. An address is a house
//! number, which is what lets the client answer "where am I standing" with a
//! street number instead of a lat/lng.
//!
//! # Why `pois` needs matching on key *and* value
//!
//! Every other Shortbread layer discriminates on a single `kind` property. `pois`
//! carries the raw OSM tags through instead — `amenity`, `shop`, `leisure`,
//! `tourism`, `historic`, … — and one feature routinely holds several of them. So
//! classification is an ordered table of `(key, value)` pairs, first hit wins,
//! exactly as [`crate::furniture`] does for the subset it can stand in the street.

use crate::vector_tiles::TileData;
use mvt_reader::feature::{Feature, Value};

/// A named place a player can walk up to, in the tile-local metre frame (origin
/// at the cell's north-west corner, +x east, +z south) — the same frame
/// [`crate::furniture::FurnitureInstance`] uses, so the client places both
/// through one transform.
#[derive(Debug, Clone)]
pub struct Poi {
    pub x: f32,
    pub z: f32,
    /// The OSM `name`. A POI without one is dropped: the whole feature is a
    /// prompt that says what the thing is, and "unnamed shop" is not worth
    /// walking to.
    pub name: String,
    pub category: PoiCategory,
}

/// A street address point: a house number, and the street it is on when the tile
/// carries one.
#[derive(Debug, Clone)]
pub struct Address {
    pub x: f32,
    pub z: f32,
    pub housenumber: String,
    pub street: Option<String>,
}

/// Coarse grouping for a POI, chosen for what the prompt should *say* and which
/// glyph it should carry — not for OSM fidelity. Anything unrecognised but named
/// still comes through as [`PoiCategory::Place`], because a name on the map is
/// worth more than a correct taxonomy.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PoiCategory {
    Food,
    Drink,
    Shop,
    Culture,
    Lodging,
    Health,
    Education,
    Transport,
    Leisure,
    Worship,
    Money,
    Place,
}

impl PoiCategory {
    /// A short word for the prompt ("café", "museum"). Deliberately lower case:
    /// it is read as part of a sentence — "Press E — Vismarkt (café)".
    #[must_use]
    pub fn label(self) -> &'static str {
        match self {
            Self::Food => "restaurant",
            Self::Drink => "café",
            Self::Shop => "shop",
            Self::Culture => "culture",
            Self::Lodging => "lodging",
            Self::Health => "health",
            Self::Education => "school",
            Self::Transport => "transport",
            Self::Leisure => "leisure",
            Self::Worship => "place of worship",
            Self::Money => "bank",
            Self::Place => "place",
        }
    }

    /// One glyph for the HUD prompt. Restricted to characters the bundled brand
    /// font actually has — `systems::theme` documents what happens when it isn't
    /// (a tofu box), and a prompt is exactly where that would show.
    #[must_use]
    pub fn glyph(self) -> &'static str {
        match self {
            Self::Food => "🍴",
            Self::Drink => "☕",
            Self::Shop => "🛍",
            Self::Culture => "🎭",
            Self::Lodging => "🛏",
            Self::Health => "✚",
            Self::Education => "🎓",
            Self::Transport => "🚉",
            Self::Leisure => "⚽",
            Self::Worship => "⛪",
            Self::Money => "€",
            Self::Place => "📍",
        }
    }
}

/// OSM tag → category, matched on key **and** value, in order, first hit wins.
/// A `*` value matches any value for that key, which is how whole families
/// (`shop=*`, `historic=*`) come through without enumerating them.
const CATEGORY_TABLE: &[(&str, &str, PoiCategory)] = &[
    ("amenity", "restaurant", PoiCategory::Food),
    ("amenity", "fast_food", PoiCategory::Food),
    ("amenity", "food_court", PoiCategory::Food),
    ("amenity", "ice_cream", PoiCategory::Food),
    ("amenity", "cafe", PoiCategory::Drink),
    ("amenity", "bar", PoiCategory::Drink),
    ("amenity", "pub", PoiCategory::Drink),
    ("amenity", "biergarten", PoiCategory::Drink),
    ("amenity", "bank", PoiCategory::Money),
    ("amenity", "pharmacy", PoiCategory::Health),
    ("amenity", "hospital", PoiCategory::Health),
    ("amenity", "clinic", PoiCategory::Health),
    ("amenity", "doctors", PoiCategory::Health),
    ("amenity", "dentist", PoiCategory::Health),
    ("amenity", "veterinary", PoiCategory::Health),
    ("amenity", "school", PoiCategory::Education),
    ("amenity", "kindergarten", PoiCategory::Education),
    ("amenity", "university", PoiCategory::Education),
    ("amenity", "college", PoiCategory::Education),
    ("amenity", "library", PoiCategory::Culture),
    ("amenity", "theatre", PoiCategory::Culture),
    ("amenity", "cinema", PoiCategory::Culture),
    ("amenity", "arts_centre", PoiCategory::Culture),
    ("amenity", "place_of_worship", PoiCategory::Worship),
    ("tourism", "museum", PoiCategory::Culture),
    ("tourism", "gallery", PoiCategory::Culture),
    ("tourism", "artwork", PoiCategory::Culture),
    ("tourism", "attraction", PoiCategory::Culture),
    ("tourism", "hotel", PoiCategory::Lodging),
    ("tourism", "hostel", PoiCategory::Lodging),
    ("tourism", "guest_house", PoiCategory::Lodging),
    ("historic", "*", PoiCategory::Culture),
    ("shop", "*", PoiCategory::Shop),
    ("leisure", "*", PoiCategory::Leisure),
    ("sport", "*", PoiCategory::Leisure),
    ("public_transport", "*", PoiCategory::Transport),
    ("railway", "*", PoiCategory::Transport),
];

/// Classification tags are always strings in this schema, so this borrows.
fn tag_str<'a>(feature: &'a Feature, key: &str) -> Option<&'a str> {
    match feature.properties.as_ref()?.get(key)? {
        Value::String(s) => Some(s.as_str()),
        _ => None,
    }
}

/// …whereas a house number is routinely encoded as one of MVT's several numeric
/// value types rather than as a string ("26" and 26 are both common in the same
/// layer), so the value has to be rendered rather than borrowed. Reading only
/// `Value::String` here silently dropped every numeric house number, which is
/// most of them.
fn tag_string(feature: &Feature, key: &str) -> Option<String> {
    match feature.properties.as_ref()?.get(key)? {
        Value::String(s) => Some(s.clone()),
        Value::Float(v) => Some(v.to_string()),
        Value::Double(v) => Some(v.to_string()),
        Value::Int(v) | Value::SInt(v) => Some(v.to_string()),
        Value::UInt(v) => Some(v.to_string()),
        Value::Bool(_) | Value::Null => None,
    }
}

/// Classify one `pois` feature, or `None` if no table row matches.
#[must_use]
pub fn category_of(feature: &Feature) -> Option<PoiCategory> {
    CATEGORY_TABLE
        .iter()
        .find(|(key, value, _)| match tag_str(feature, key) {
            Some(v) => *value == "*" || v == *value,
            None => false,
        })
        .map(|(_, _, c)| *c)
}

/// The first point of a feature's geometry, in tile-local metres.
fn point_of(feature: &Feature, scale: f32) -> Option<(f32, f32)> {
    match &feature.geometry {
        geo_types::Geometry::Point(p) => Some((p.x() * scale, p.y() * scale)),
        geo_types::Geometry::MultiPoint(mp) => {
            mp.0.first().map(|p| (p.x() * scale, p.y() * scale))
        }
        _ => None,
    }
}

/// Read a tile's `pois` layer as named, positioned places.
///
/// Unnamed features are dropped — including every one [`crate::furniture`] stands
/// in the street, since a bench has no name and gets its prompt from its form
/// instead. So the two extractions overlap in the tile and not in the output.
pub fn extract_pois(tile: &TileData, tile_m: f32) -> Result<Vec<Poi>, String> {
    let Some(meta) = tile.layer("pois") else {
        return Ok(Vec::new());
    };
    let features = tile.features(meta);
    let scale = tile_m / TileData::extent(meta);

    let mut out = Vec::new();
    for feature in features.iter() {
        let Some(name) = tag_string(feature, "name") else {
            continue;
        };
        if name.trim().is_empty() {
            continue;
        }
        let Some((x, z)) = point_of(feature, scale) else {
            continue;
        };
        out.push(Poi {
            x,
            z,
            name,
            category: category_of(feature).unwrap_or(PoiCategory::Place),
        });
    }
    Ok(out)
}

/// Read a tile's `addresses` layer.
///
/// This is the single densest layer in the tile after the buildings and it was
/// entirely unread. It costs nothing to carry and it is what turns "you are at
/// 53.2194, 6.5665" into "you are outside Oude Kijk in 't Jatstraat 26".
pub fn extract_addresses(tile: &TileData, tile_m: f32) -> Result<Vec<Address>, String> {
    let Some(meta) = tile.layer("addresses") else {
        return Ok(Vec::new());
    };
    let features = tile.features(meta);
    let scale = tile_m / TileData::extent(meta);

    let mut out = Vec::new();
    for feature in features.iter() {
        let Some(housenumber) = tag_string(feature, "housenumber") else {
            continue;
        };
        let Some((x, z)) = point_of(feature, scale) else {
            continue;
        };
        out.push(Address {
            x,
            z,
            housenumber,
            street: tag_string(feature, "street"),
        });
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    const FIXTURE: &[u8] = include_bytes!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/tests/fixtures/groningen_z14.mvt"
    ));

    fn fixture() -> TileData {
        TileData::parse(FIXTURE).expect("fixture parses")
    }

    /// The layer really is there and really does carry names. This is the claim
    /// the whole feature rests on, and it is about *data*, not code — so it is
    /// asserted against the real tile rather than a hand-built one.
    #[test]
    fn the_tile_carries_named_places() {
        let pois = extract_pois(&fixture(), 611.0).expect("extracts");
        assert!(
            pois.len() > 20,
            "a city-centre tile should hold plenty of named places, got {}",
            pois.len()
        );
        assert!(
            pois.iter().all(|p| !p.name.trim().is_empty()),
            "an unnamed POI must never reach the prompt"
        );
        // Every category the table can produce should be reachable in principle;
        // at minimum this tile must show more than one kind of thing.
        let mut kinds: Vec<PoiCategory> = pois.iter().map(|p| p.category).collect();
        kinds.dedup();
        kinds.sort_by_key(|c| c.label());
        kinds.dedup();
        assert!(kinds.len() > 3, "expected variety, got {kinds:?}");
    }

    /// Addresses come through with house numbers, which is the only field the
    /// readout actually needs.
    #[test]
    fn the_tile_carries_house_numbers() {
        let addresses = extract_addresses(&fixture(), 611.0).expect("extracts");
        assert!(
            addresses.len() > 500,
            "the addresses layer is the densest in the tile, got {}",
            addresses.len()
        );
        assert!(addresses.iter().all(|a| !a.housenumber.is_empty()));
    }

    /// Positions land in metres within the cell rather than in raw tile units —
    /// the bug that would put every place at the north-west corner, or a
    /// kilometre away.
    ///
    /// The bound is the cell **plus a buffer**, not the cell: an MVT tile is
    /// generated with a margin of surrounding geometry so that symbols and labels
    /// straddling an edge can be drawn without a seam, so a legitimately-decoded
    /// POI can sit a few metres outside its own tile. Asserting the exact cell
    /// fails on the real fixture (a place at x = −2.98 m), and "clamp it into the
    /// cell" would be the wrong fix — the coordinate is correct, the cell is
    /// simply not the whole of what the tile carries. The client dedupes across
    /// cells for the same reason.
    #[test]
    fn positions_are_metres_within_the_cell_and_its_buffer() {
        let tile_m = 611.0_f32;
        let buffer = tile_m * 0.1;
        for (x, z) in extract_pois(&fixture(), tile_m)
            .unwrap()
            .iter()
            .map(|p| (p.x, p.z))
        {
            assert!(
                (-buffer..=tile_m + buffer).contains(&x)
                    && (-buffer..=tile_m + buffer).contains(&z),
                "({x}, {z}) is well outside a {tile_m} m cell"
            );
        }
    }

    /// A wildcard row matches the whole family, and an unmatched-but-named
    /// feature still becomes a `Place` rather than vanishing.
    #[test]
    fn classification_falls_back_rather_than_dropping() {
        let pois = extract_pois(&fixture(), 611.0).unwrap();
        assert!(
            pois.iter().any(|p| p.category == PoiCategory::Shop),
            "`shop=*` should match a city tile's shops"
        );
    }
}
