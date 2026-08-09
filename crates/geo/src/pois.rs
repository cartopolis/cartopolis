//! Point-of-interest extraction from the vector tile — the pure half of the data
//! layers (`viberfox`'s `systems::data_layers` owns the ECS half).
//!
//! The transit and shop markers were streamed from the **Overpass API**: one live
//! bbox query per z14 cell, 49 cells per layer per anchor, against a service that
//! rate-limits, answers a timeout with HTTP 200 and needs a 15 s → 30 s → 1 m →
//! 2 m → 4 m backoff ladder in the client to survive. Meanwhile `map_geometry`
//! had already fetched, cached and decoded the very tile those markers stand on,
//! and Shortbread puts the same features in it — `public_transport` (25 in the
//! Groningen z14 fixture) and `pois` (467, of which 55 carry a `shop`).
//!
//! So the *placement* costs no request at all, and this module is the reader for
//! it. What the tile does **not** carry is the identity and the contact detail:
//! no OSM id, no `website`, no `addr:street`, and no `ref:IFOPT` — which is the
//! key the live departure boards (`transit_live`) are fetched by. Measured on
//! that one tile, Overpass answers 65 shops to the tile's 55, and 32 of them have
//! a website. That is why the Overpass path is **demoted rather than deleted**:
//! the tile decides where the markers are and what they are, and Overpass fills
//! in the details for the cells near the camera. A marker therefore appears the
//! moment its tile lands and never waits on — or disappears with — a query
//! service.
//!
//! Two rules are inherited from [`furniture`](crate::furniture), which reads the
//! same `pois` layer for benches and bins:
//!
//! * **`pois` has no `kind`.** Every other Shortbread layer discriminates on one
//!   `kind` property; this one passes the raw OSM tags through, so a classifier
//!   matches on key *and* value. `public_transport` is the ordinary shape and
//!   does have `kind`.
//! * **A tile buffers features past its own edge**, so the same shop is in two
//!   neighbouring tiles. Owning exactly `[0, extent)` is the dedup — every point
//!   lands in exactly one cell, with no tolerance to tune.

use mvt_reader::feature::{Feature, Value};

use crate::vector_tiles::TileData;

/// One POI as the tile places it: tile-local metres (origin at the cell's
/// north-west corner, +x east, +z south) plus the little the tile knows about it.
///
/// Deliberately not the client's `PoiInstance` — that carries a BigSpace cell, a
/// world anchor and the Overpass-only detail fields, none of which exist here.
#[derive(Debug, Clone, PartialEq)]
pub struct TilePoi {
    pub x: f32,
    pub z: f32,
    /// Index into the layer's category table, from the classifier.
    pub category: usize,
    /// `name`, when the tile carries one. Shortbread also emits `name_nl` (and
    /// occasionally `name_en`); `name` is the localised-neutral one and the only
    /// one read here.
    pub name: Option<String>,
}

/// Read one of a tile's point layers as classified POIs.
///
/// `classify` maps a feature to a category index, and `None` skips it — the same
/// "an unstyled feature beats a wrongly styled one" rule the fills, the tree
/// scatter and the furniture table follow.
///
/// Takes the cell's already-parsed [`TileData`] rather than the tile body, for
/// the reason [`extract_furniture`](crate::furniture::extract_furniture) does:
/// every consumer on this worker reads a different layer out of the same bytes,
/// and `Reader::new` is a whole-tile decode.
#[must_use]
pub fn extract_pois(
    tile: &TileData,
    layer: &str,
    tile_m: f32,
    classify: fn(&Feature) -> Option<usize>,
) -> Vec<TilePoi> {
    let Some(meta) = tile.layer(layer) else {
        return Vec::new();
    };
    let extent = TileData::extent(meta);
    let scale = tile_m / extent;

    let mut out = Vec::new();
    for feature in tile.features(meta) {
        let Some(category) = classify(feature) else {
            continue;
        };
        let Some((tx, ty)) = single_point(&feature.geometry) else {
            continue;
        };
        if tx < 0.0 || ty < 0.0 || tx >= extent || ty >= extent {
            continue;
        }
        out.push(TilePoi {
            x: tx * scale,
            z: ty * scale,
            category,
            name: tag_str(feature, "name").map(str::to_string),
        });
    }
    out
}

/// The one coordinate of a point feature. Shortbread emits them as single-point
/// `MultiPoint`s, so both spellings are accepted and anything with more than one
/// point is not a POI we know how to place.
#[must_use]
pub fn single_point(geometry: &geo_types::Geometry<f32>) -> Option<(f32, f32)> {
    use geo_types::Geometry;
    match geometry {
        Geometry::Point(p) => Some((p.x(), p.y())),
        Geometry::MultiPoint(mp) if mp.0.len() == 1 => Some((mp.0[0].x(), mp.0[0].y())),
        _ => None,
    }
}

/// One string-valued property of a feature. Non-string values (the `recycling:*`
/// booleans, `housenumber`'s occasional integer) answer `None` — every caller
/// here matches on a string vocabulary.
#[must_use]
pub fn tag_str<'a>(feature: &'a Feature, key: &str) -> Option<&'a str> {
    match feature.properties.as_ref()?.get(key)? {
        Value::String(s) => Some(s.as_str()),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const GRONINGEN_Z14: &[u8] = include_bytes!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/tests/fixtures/groningen_z14.mvt"
    ));

    /// z14/8490/5319 — the tile the fixture is, at 53.23° N.
    const TILE_M: f32 = 1_463.0;

    fn tile() -> TileData {
        TileData::parse(GRONINGEN_Z14).expect("parse")
    }

    fn shop(f: &Feature) -> Option<usize> {
        tag_str(f, "shop").map(|_| 0)
    }

    #[test]
    fn the_transit_layer_yields_placed_stops() {
        let stops = extract_pois(&tile(), "public_transport", TILE_M, |f| {
            tag_str(f, "kind").map(|_| 0)
        });
        assert!(
            (20..=25).contains(&stops.len()),
            "the layer holds 25 features before the edge-ownership cut; got {}",
            stops.len()
        );
        assert!(
            stops.iter().any(|s| s.name.is_some()),
            "no stop carried a name"
        );
        for s in &stops {
            assert!(
                (0.0..=TILE_M).contains(&s.x) && (0.0..=TILE_M).contains(&s.z),
                "{s:?} placed outside its own cell"
            );
        }
    }

    #[test]
    fn shops_come_out_of_the_pois_layer() {
        let shops = extract_pois(&tile(), "pois", TILE_M, shop);
        assert!(
            (45..=55).contains(&shops.len()),
            "55 of the 467 `pois` features carry a shop tag; got {}",
            shops.len()
        );
    }

    /// The edge-ownership rule is the whole dedup between neighbouring cells, so
    /// it has to actually reject something: `addresses` is the layer with the most
    /// buffered features (the fixture's first three all sit at negative x).
    #[test]
    fn features_buffered_past_the_edge_are_not_ours() {
        let t = tile();
        let meta = t.layer("addresses").expect("addresses layer");
        let extent = TileData::extent(meta);
        let outside = t
            .features(meta)
            .iter()
            .filter(|f| {
                single_point(&f.geometry)
                    .is_some_and(|(x, y)| x < 0.0 || y < 0.0 || x >= extent || y >= extent)
            })
            .count();
        assert!(
            outside > 0,
            "the fixture has no buffered features, so this rule is untested here"
        );

        let all = extract_pois(&t, "addresses", TILE_M, |_| Some(0));
        assert_eq!(
            all.len(),
            t.features(meta).len() - outside,
            "buffered features were not excluded"
        );
    }

    /// An unknown layer is empty, not an error: a raster source, an older tileset
    /// or a tile with nothing in it are all ordinary.
    #[test]
    fn a_missing_layer_is_empty() {
        assert!(extract_pois(&tile(), "no_such_layer", TILE_M, |_| Some(0)).is_empty());
    }
}
