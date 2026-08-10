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
//!
//! **`Feature::id` is not the OSM id, and looks exactly like one.** Every feature
//! in both layers carries one, in the right numeric range (`290288112`). It is
//! not: intersecting the 55 shop ids in the fixture against the 65 node ids
//! Overpass answers for the same bbox gives **zero** matches, so the tileset
//! renumbers. So it cannot be a join key for the detail pass and it cannot build
//! an `openstreetmap.org/node/…` link — both of which it would silently get
//! wrong rather than fail at. [`TilePoi`] deliberately does not carry it.

use mvt_reader::feature::Value;

use crate::vector_tiles::TileData;

/// One decoded MVT feature. Re-exported because a classifier is written *by the
/// client* (`systems::data_layers::LAYER_DEFS`) and takes one — and `viberfox`
/// itself does not depend on `mvt_reader`. Keeping it that way is the point of
/// this crate: the tile decoder is a detail of the geodata layer, not something
/// every consumer links against.
pub use mvt_reader::feature::Feature;

/// One POI as the tile places it, plus the little the tile knows about it.
///
/// Position is **normalised across the tile** — `u`/`v` in `[0, 1)` from the
/// north-west corner, east and south — rather than tile-local metres like
/// [`FurnitureInstance`](crate::furniture::FurnitureInstance). Furniture is
/// meshed into the cell it came from, where metres are the working unit; a POI
/// marker is placed into a Mercator frame anchored somewhere else entirely, and a
/// fraction is the form that survives that without picking up a
/// metres-per-degree of its own. The consumer adds the cell's own origin.
///
/// Deliberately not the client's `PoiInstance` either — that carries a BigSpace
/// cell, a world anchor and the Overpass-only detail fields, none of which exist
/// here.
#[derive(Debug, Clone, PartialEq)]
pub struct TilePoi {
    /// Fraction across the tile, west → east.
    pub u: f32,
    /// Fraction down the tile, north → south.
    pub v: f32,
    /// Index into the layer's category table, from the classifier.
    pub category: usize,
    /// `name`, when the tile carries one. Shortbread also emits `name_nl` (and
    /// occasionally `name_en`); `name` is the localisation-neutral one and the
    /// only one read here.
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
    classify: fn(&Feature) -> Option<usize>,
) -> Vec<TilePoi> {
    let Some(meta) = tile.layer(layer) else {
        return Vec::new();
    };
    let extent = TileData::extent(meta);

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
            u: tx / extent,
            v: ty / extent,
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

/// A point feature with the given string properties, at the origin.
///
/// A test fixture that has to live in production code: the classifiers are
/// written in `viberfox` (`systems::data_layers::LAYER_DEFS`), which depends on
/// neither `mvt_reader` nor `geo_types` and should not start doing so to build
/// one of these by hand. Exporting a constructor is cheaper than exporting
/// `Value` and `Geometry` and cheaper than leaving those classifiers untested.
#[must_use]
pub fn point_feature(properties: &[(&str, &str)]) -> Feature {
    Feature {
        geometry: geo_types::Geometry::Point(geo_types::Point::new(0.0, 0.0)),
        id: None,
        properties: Some(
            properties
                .iter()
                .map(|(k, v)| ((*k).to_string(), Value::String((*v).to_string())))
                .collect(),
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const GRONINGEN_Z14: &[u8] = include_bytes!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/tests/fixtures/groningen_z14.mvt"
    ));

    fn tile() -> TileData {
        TileData::parse(GRONINGEN_Z14).expect("parse")
    }

    fn shop(f: &Feature) -> Option<usize> {
        tag_str(f, "shop").map(|_| 0)
    }

    #[test]
    fn the_transit_layer_yields_placed_stops() {
        let stops = extract_pois(&tile(), "public_transport", |f| {
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
                (0.0..1.0).contains(&s.u) && (0.0..1.0).contains(&s.v),
                "{s:?} placed outside its own cell"
            );
        }
    }

    #[test]
    fn shops_come_out_of_the_pois_layer() {
        let shops = extract_pois(&tile(), "pois", shop);
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

        let all = extract_pois(&t, "addresses", |_| Some(0));
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
        assert!(extract_pois(&tile(), "no_such_layer", |_| Some(0)).is_empty());
    }
}
