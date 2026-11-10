//! The Overpass questions this project asks, answered from the extract: per
//! layer, what [`super::load`] keeps and the document one cell gets.
//!
//! Each predicate restates one Overpass filter of the query its client sends,
//! named beside it, so the two can be read side by side. A layer's document must
//! hold exactly the elements Overpass would answer for that cell, because the
//! client cannot tell which of the two wrote it.

use std::collections::HashSet;

use serde_json::Value;

use super::{document, tag, Bbox, Node, Out, Store, TagView, Tags, TileIndex, Wanted, Way};

pub(crate) fn view(tags: &Tags) -> Vec<(&str, &str)> {
    tags.iter().map(|(k, v)| (k.as_str(), v.as_str())).collect()
}

fn is(tags: &TagView<'_>, key: &str, values: &[&str]) -> bool {
    tag(tags, key).is_some_and(|v| values.contains(&v))
}

/// The cell's nodes and ways that pass `node` and `way`, as one document.
fn select(
    store: &Store,
    index: &TileIndex,
    bb: &Bbox,
    node: impl Fn(&Node) -> bool,
    way: impl Fn(&Way) -> bool,
    out: Out,
) -> Value {
    let (nodes, ways) = index.query(store, bb);
    let nodes: Vec<&Node> = nodes.into_iter().filter(|n| node(n)).collect();
    let ways: Vec<&Way> = ways.into_iter().filter(|w| way(w)).collect();
    document(&nodes, &ways, out)
}

/// `cartopolis_geo::tall::query`: windmills, water towers, chimneys, masts,
/// towers and cranes, and wind turbines, nodes and ways, `out center`.
pub mod tall {
    use super::*;

    const MAN_MADE: &[&str] = &[
        "windmill",
        "water_tower",
        "chimney",
        "mast",
        "tower",
        "communications_tower",
        "crane",
    ];

    /// `["man_made"~"^(…)$"]` or `["power"="generator"]["generator:source"="wind"]`.
    pub fn wanted(tags: &TagView<'_>) -> bool {
        is(tags, "man_made", MAN_MADE)
            || (tag(tags, "power") == Some("generator")
                && tag(tags, "generator:source") == Some("wind"))
    }

    pub const WANTED: Wanted = Wanted {
        node: wanted,
        way: wanted,
        ..Wanted::NOTHING
    };

    #[must_use]
    pub fn cell(store: &Store, index: &TileIndex, x: i64, y: i64) -> Value {
        let bb = Bbox::tile(x, y, cartopolis_geo::tall::ZOOM);
        select(
            store,
            index,
            &bb,
            |n| wanted(&view(&n.tags)),
            |w| wanted(&view(&w.tags)),
            Out::Center,
        )
    }
}

/// `systems::street_detail::detail_query`: trees, lamps, bike racks,
/// billboards, bollards, walls and fences, piers, weirs, the signals and zebra
/// crossings with the roads through them, and roads closed to some traffic;
/// `out geom`, z14.
pub mod street {
    use super::*;

    pub const ZOOM: u32 = 14;

    /// `node["highway"="traffic_signals"]->.sig;
    /// node["highway"="crossing"]["crossing"~"^(zebra|marked|uncontrolled)$"]->.zeb;`
    pub fn joining(tags: &TagView<'_>) -> bool {
        tag(tags, "highway") == Some("traffic_signals")
            || (tag(tags, "highway") == Some("crossing")
                && is(tags, "crossing", &["zebra", "marked", "uncontrolled"]))
    }

    /// `node["natural"="tree"]`, `node["highway"="street_lamp"]`,
    /// `node["amenity"="bicycle_parking"]`, `node["advertising"="billboard"]`,
    /// `node["barrier"="bollard"]`.
    pub fn node(tags: &TagView<'_>) -> bool {
        tag(tags, "natural") == Some("tree")
            || tag(tags, "highway") == Some("street_lamp")
            || tag(tags, "amenity") == Some("bicycle_parking")
            || tag(tags, "advertising") == Some("billboard")
            || tag(tags, "barrier") == Some("bollard")
    }

    const BARRIERS: &[&str] = &[
        "wall",
        "city_wall",
        "fence",
        "hedge",
        "retaining_wall",
        "guard_rail",
    ];
    const ROADS: &[&str] = &[
        "motorway",
        "trunk",
        "primary",
        "secondary",
        "tertiary",
        "unclassified",
        "residential",
        "living_street",
        "service",
    ];
    const ACCESS_KEYS: &[&str] = &["access", "vehicle", "motor_vehicle", "motorcar"];

    /// `way["natural"="tree_row"]`, `way["barrier"~"^(…)$"]`,
    /// `way["man_made"="pier"]`, `way["waterway"~"^(weir|lock_gate)$"]`, and
    /// `way["highway"~"^(…)$"][~"^(access|vehicle|motor_vehicle|motorcar)$"~"."]`.
    pub fn way(tags: &TagView<'_>) -> bool {
        tag(tags, "natural") == Some("tree_row")
            || is(tags, "barrier", BARRIERS)
            || tag(tags, "man_made") == Some("pier")
            || is(tags, "waterway", &["weir", "lock_gate"])
            || (is(tags, "highway", ROADS)
                && tags
                    .iter()
                    .any(|(k, v)| ACCESS_KEYS.contains(k) && !v.is_empty()))
    }

    /// `way(bn.sig)["highway"]; way(bn.zeb)["highway"];`
    pub fn join_way(tags: &TagView<'_>) -> bool {
        tag(tags, "highway").is_some()
    }

    pub const WANTED: Wanted = Wanted {
        node,
        way,
        join_node: joining,
        join_way,
    };

    #[must_use]
    pub fn cell(store: &Store, index: &TileIndex, x: i64, y: i64) -> Value {
        let bb = Bbox::tile(x, y, ZOOM);
        let (nodes, ways) = index.query(store, &bb);
        let joins: HashSet<i64> = nodes.iter().filter(|n| n.joins).map(|n| n.id).collect();
        let nodes: Vec<&Node> = nodes
            .into_iter()
            .filter(|n| n.joins || node(&view(&n.tags)))
            .collect();
        let ways: Vec<&Way> = ways
            .into_iter()
            .filter(|w| {
                let t = view(&w.tags);
                way(&t) || (join_way(&t) && w.refs.iter().any(|r| joins.contains(r)))
            })
            .collect();
        document(&nodes, &ways, Out::Geom)
    }
}

/// `systems::data_layers`' transit and shops queries: nodes only, `out;`, z14.
pub mod pois {
    use super::*;

    pub const ZOOM: u32 = 14;

    /// `node["railway"~"^(station|halt|tram_stop)$"]`, `node["highway"="bus_stop"]`.
    pub fn transit(tags: &TagView<'_>) -> bool {
        is(tags, "railway", &["station", "halt", "tram_stop"])
            || tag(tags, "highway") == Some("bus_stop")
    }

    /// `node["shop"]`.
    pub fn shop(tags: &TagView<'_>) -> bool {
        tag(tags, "shop").is_some()
    }

    pub const TRANSIT: Wanted = Wanted {
        node: transit,
        ..Wanted::NOTHING
    };

    pub const SHOPS: Wanted = Wanted {
        node: shop,
        ..Wanted::NOTHING
    };

    #[must_use]
    pub fn cell(
        store: &Store,
        index: &TileIndex,
        x: i64,
        y: i64,
        wanted: fn(&TagView<'_>) -> bool,
    ) -> Value {
        let bb = Bbox::tile(x, y, ZOOM);
        select(
            store,
            index,
            &bb,
            |n| wanted(&view(&n.tags)),
            |_| false,
            Out::Body,
        )
    }
}

/// `systems::route_ahead`'s corridor query, `way["highway"]["maxspeed"]`, cut per
/// z14 cell for the client to gather along its route; `out geom`.
pub mod speeds {
    use super::*;

    pub const ZOOM: u32 = 14;

    pub fn way(tags: &TagView<'_>) -> bool {
        tag(tags, "highway").is_some() && tag(tags, "maxspeed").is_some()
    }

    pub const WANTED: Wanted = Wanted {
        way,
        ..Wanted::NOTHING
    };

    #[must_use]
    pub fn cell(store: &Store, index: &TileIndex, x: i64, y: i64) -> Value {
        let bb = Bbox::tile(x, y, ZOOM);
        select(
            store,
            index,
            &bb,
            |_| false,
            |w| way(&view(&w.tags)),
            Out::Geom,
        )
    }
}

/// `osm_buildings`' query outside 3DBAG's cells:
/// `way["building"]`, `way["building:part"]`, `out geom`, z14.
pub mod buildings {
    use super::*;

    pub const ZOOM: u32 = 14;

    pub fn way(tags: &TagView<'_>) -> bool {
        tag(tags, "building").is_some() || tag(tags, "building:part").is_some()
    }

    pub const WANTED: Wanted = Wanted {
        way,
        ..Wanted::NOTHING
    };

    #[must_use]
    pub fn cell(store: &Store, index: &TileIndex, x: i64, y: i64) -> Value {
        let bb = Bbox::tile(x, y, ZOOM);
        select(
            store,
            index,
            &bb,
            |_| false,
            |w| way(&view(&w.tags)),
            Out::Geom,
        )
    }
}

/// `registers::osm_art_query`: `nwr["tourism"="artwork"]`, `out center tags`.
/// Nodes and ways; an artwork mapped as a relation is not read.
pub mod artworks {
    use super::*;

    pub fn wanted(tags: &TagView<'_>) -> bool {
        tag(tags, "tourism") == Some("artwork")
    }

    pub const WANTED: Wanted = Wanted {
        node: wanted,
        way: wanted,
        ..Wanted::NOTHING
    };
}

/// `bag::materials_query`: buildings that name their pand (`ref:bag`) and say
/// what they are built of or what colour they are, ways and relations, tags only.
pub mod materials {
    use super::*;

    pub fn wanted(tags: &TagView<'_>) -> bool {
        tag(tags, "building").is_some()
            && tag(tags, "ref:bag").is_some()
            && (tag(tags, "building:material").is_some() || tag(tags, "building:colour").is_some())
    }

    pub const WANTED: Wanted = Wanted {
        way: wanted,
        ..Wanted::NOTHING
    };
}

/// `backdrop::query`: the streets, paths, rail and water a backdrop draws.
pub mod backdrop {
    use super::*;

    const HIGHWAYS: &[&str] = &[
        "motorway",
        "trunk",
        "primary",
        "secondary",
        "tertiary",
        "unclassified",
        "residential",
        "living_street",
        "pedestrian",
        "cycleway",
        "service",
        "footway",
        "path",
    ];

    /// `["waterway"~"canal|river"]` is unanchored: `riverbank` matches too.
    pub fn way(tags: &TagView<'_>) -> bool {
        is(tags, "highway", HIGHWAYS)
            || tag(tags, "waterway").is_some_and(|w| w.contains("canal") || w.contains("river"))
            || tag(tags, "natural") == Some("water")
            || tag(tags, "railway") == Some("rail")
    }

    pub const WANTED: Wanted = Wanted {
        way,
        ..Wanted::NOTHING
    };
}

#[cfg(test)]
mod tests {
    use super::*;
    use cartopolis_core::world::lat_lng_to_tile;

    fn tags(pairs: &[(&str, &str)]) -> Tags {
        pairs
            .iter()
            .map(|(k, v)| ((*k).to_string(), (*v).to_string()))
            .collect()
    }

    fn node(id: i64, lat: f64, lon: f64, t: &[(&str, &str)]) -> Node {
        let tags = tags(t);
        let joins = street::joining(&view(&tags));
        Node {
            id,
            lat,
            lon,
            tags,
            joins,
        }
    }

    fn way(id: i64, refs: &[i64], geom: &[(f64, f64)], t: &[(&str, &str)]) -> Way {
        Way {
            id,
            refs: refs.to_vec(),
            geom: geom.to_vec(),
            tags: tags(t),
        }
    }

    #[test]
    fn tall_takes_the_query_s_values_and_no_others() {
        let t = |pairs: &[(&str, &str)]| tall::wanted(&pairs.to_vec());
        assert!(t(&[("man_made", "windmill")]));
        assert!(t(&[("power", "generator"), ("generator:source", "wind")]));
        assert!(!t(&[("power", "generator"), ("generator:source", "solar")]));
        assert!(!t(&[("man_made", "towers")]), "the regex is anchored");
        assert!(!t(&[("man_made", "water_tower;tower")]));
    }

    #[test]
    fn a_road_comes_with_the_signal_in_this_cell_and_not_the_next() {
        let (lat, lon) = (53.2195, 6.5665);
        let (x, y) = lat_lng_to_tile(lat, lon, street::ZOOM);
        let mut store = Store::default();
        store
            .nodes
            .push(node(1, lat, lon, &[("highway", "traffic_signals")]));
        store
            .nodes
            .push(node(2, lat, lon + 0.0005, &[("barrier", "gate")]));
        // Through the signal; plain residential, so only the join brings it.
        store.ways.push(way(
            10,
            &[3, 1, 4],
            &[(lat, lon - 0.001), (lat, lon), (lat, lon + 0.001)],
            &[("highway", "residential")],
        ));
        // A road in the cell that passes through no signal.
        store.ways.push(way(
            11,
            &[5, 6],
            &[(lat + 0.0003, lon - 0.001), (lat + 0.0003, lon + 0.001)],
            &[("highway", "residential")],
        ));
        // Closed to cars: kept on its own tags.
        store.ways.push(way(
            12,
            &[7, 8],
            &[(lat - 0.0003, lon - 0.001), (lat - 0.0003, lon + 0.001)],
            &[("highway", "service"), ("access", "private")],
        ));
        let index = TileIndex::new(&store, street::ZOOM);
        let doc = street::cell(&store, &index, x, y);
        let ids: Vec<i64> = doc["elements"]
            .as_array()
            .unwrap()
            .iter()
            .map(|e| e["id"].as_i64().unwrap())
            .collect();
        assert_eq!(ids, [1, 10, 12]);
        assert_eq!(doc["elements"][1]["nodes"], serde_json::json!([3, 1, 4]));
    }

    #[test]
    fn transit_and_shops_are_nodes_only() {
        let (lat, lon) = (53.2105, 6.5640);
        let (x, y) = lat_lng_to_tile(lat, lon, pois::ZOOM);
        let mut store = Store::default();
        store.nodes.push(node(
            1,
            lat,
            lon,
            &[("highway", "bus_stop"), ("name", "Hoofdstation")],
        ));
        store.nodes.push(node(2, lat, lon, &[("shop", "bakery")]));
        let index = TileIndex::new(&store, pois::ZOOM);
        let transit = pois::cell(&store, &index, x, y, pois::transit);
        assert_eq!(transit["elements"].as_array().unwrap().len(), 1);
        assert_eq!(transit["elements"][0]["tags"]["name"], "Hoofdstation");
        let shops = pois::cell(&store, &index, x, y, pois::shop);
        assert_eq!(shops["elements"][0]["id"], 2);
    }
}
