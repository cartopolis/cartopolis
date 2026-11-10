//! OpenStreetMap read straight from a Geofabrik extract, in place of Overpass.
//!
//! Every Overpass question this project asks is a fixed one about one country
//! (`docs/notes/the-server-stack.md`), so the answers are cut from the `.osm.pbf`
//! file into the per-cell documents the client already reads, and nobody runs an
//! Overpass server for them. **A cell's file is the Overpass answer itself** —
//! the same elements, the same JSON shape, `out center` or `out geom` — so the
//! client parses it with the code it already has, the way `tall` began
//! (`pipeline/tall.rs`).
//!
//! The extract is read in three passes, each in parallel ([`load`]): the tagged
//! nodes a layer wants, then the ways (some of which are wanted only because they
//! pass through a wanted node, Overpass's `way(bn.…)`), then the coordinates of
//! those ways' nodes. Only what the layer asked for is held, and only inside the
//! area being built; a whole-country street-detail run holds tens of megabytes,
//! not the extract.
//!
//! Data: © OpenStreetMap contributors, ODbL.

pub mod layers;

use std::collections::{BTreeMap, HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

use cartopolis_core::world::{lat_lng_to_tile, tile_to_lat_lng};
use osmpbf::{Element, ElementReader};
use serde_json::{json, Map, Value};

use crate::pipeline::{http, Ctx, Outcome, PipelineError};

/// The extract read when nothing else is configured: the Netherlands, rebuilt by
/// Geofabrik every day (about 1.3 GB).
pub const DEFAULT_EXTRACT: &str = "https://download.geofabrik.de/europe/netherlands-latest.osm.pbf";

/// An extract older than this is downloaded again before a run reads it.
pub const MAX_AGE: Duration = Duration::from_secs(7 * 24 * 3600);

/// `ATLAS_OSM_EXTRACT`, or [`DEFAULT_EXTRACT`]: a URL, or a path to a `.osm.pbf`
/// already on disk (read as it is, never downloaded).
#[must_use]
pub fn extract_source() -> String {
    std::env::var("ATLAS_OSM_EXTRACT").unwrap_or_else(|_| DEFAULT_EXTRACT.to_string())
}

/// The extract on disk, downloaded into `<data_dir>/osm/` when it is missing or
/// older than [`MAX_AGE`]. A failed refresh of a file already there keeps the
/// old one: a week-old map beats no map.
pub fn ensure_extract(ctx: &Ctx) -> Outcome<PathBuf> {
    let source = extract_source();
    if !source.starts_with("http://") && !source.starts_with("https://") {
        let path = PathBuf::from(&source);
        if !path.is_file() {
            return Err(PipelineError::Failed(format!("{source}: no such extract")));
        }
        return Ok(path);
    }
    let name = source.rsplit('/').next().unwrap_or("extract.osm.pbf");
    let dir = ctx.data_dir()?.join("osm");
    std::fs::create_dir_all(&dir).map_err(|e| PipelineError::Failed(e.to_string()))?;
    let path = dir.join(name);
    let age = std::fs::metadata(&path)
        .and_then(|m| m.modified())
        .ok()
        .and_then(|t| SystemTime::now().duration_since(t).ok());
    if age.is_some_and(|a| a < MAX_AGE) {
        return Ok(path);
    }
    let part = dir.join(format!("{name}.part"));
    ctx.log(format!("OSM-extract ophalen: {source}"));
    match http::download_to(ctx, &source, &part, "osm extract") {
        Ok(bytes) => {
            std::fs::rename(&part, &path).map_err(|e| PipelineError::Failed(e.to_string()))?;
            ctx.log(format!("{} MB opgehaald", bytes / 1_000_000));
            Ok(path)
        }
        Err(PipelineError::Failed(e)) if path.is_file() => {
            let _ = std::fs::remove_file(&part);
            ctx.log(format!("{e} — de vorige extract wordt gebruikt"));
            Ok(path)
        }
        Err(e) => {
            let _ = std::fs::remove_file(&part);
            Err(e)
        }
    }
}

/// Up to this many square degrees (a z9 tile here is 0.70° × 0.42°, 0.30) [`load`]
/// collects the ids of every node in the area to drop ways outside it early:
/// tens of millions of ids at most, a few hundred megabytes. A whole country
/// is read in such pieces ([`CHUNK_ZOOM`]) rather than letting the set grow.
const SMALL_AREA_DEG2: f64 = 0.4;

/// An element's tags while the extract is read: borrowed, in file order.
pub type TagView<'a> = [(&'a str, &'a str)];

/// The value of `key`, if the element has it.
#[must_use]
pub fn tag<'a>(tags: &TagView<'a>, key: &str) -> Option<&'a str> {
    tags.iter().find(|(k, _)| *k == key).map(|(_, v)| *v)
}

/// What one layer keeps from the extract.
pub struct Wanted {
    /// Tagged nodes kept for their own sake.
    pub node: fn(&TagView<'_>) -> bool,
    /// Ways kept for their own sake.
    pub way: fn(&TagView<'_>) -> bool,
    /// Nodes whose parent ways are kept when those pass [`Self::join_way`] —
    /// Overpass's `way(bn.set)[…]`. The nodes themselves are kept too.
    pub join_node: fn(&TagView<'_>) -> bool,
    pub join_way: fn(&TagView<'_>) -> bool,
}

fn never(_: &TagView<'_>) -> bool {
    false
}

impl Wanted {
    /// Nothing; fill in what the layer wants.
    pub const NOTHING: Self = Self {
        node: never,
        way: never,
        join_node: never,
        join_way: never,
    };
}

pub type Tags = BTreeMap<String, String>;

fn owned(tags: &TagView<'_>) -> Tags {
    tags.iter()
        .map(|(k, v)| ((*k).to_string(), (*v).to_string()))
        .collect()
}

#[derive(Debug, Clone)]
pub struct Node {
    pub id: i64,
    pub lat: f64,
    pub lon: f64,
    pub tags: Tags,
    /// Kept as a [`Wanted::join_node`] (Overpass's `->.set`).
    pub joins: bool,
}

#[derive(Debug, Clone)]
pub struct Way {
    pub id: i64,
    pub refs: Vec<i64>,
    /// `(lat, lon)` per ref; a ref the extract lacks (cut at its border) is
    /// dropped from both, as Overpass drops it from `geometry`.
    pub geom: Vec<(f64, f64)>,
    pub tags: Tags,
}

/// `south, west, north, east`, as Overpass writes a bbox.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Bbox {
    pub south: f64,
    pub west: f64,
    pub north: f64,
    pub east: f64,
}

impl Bbox {
    /// One slippy-map tile.
    #[must_use]
    pub fn tile(x: i64, y: i64, zoom: u32) -> Self {
        let (north, west) = tile_to_lat_lng(x, y, zoom);
        let (south, east) = tile_to_lat_lng(x + 1, y + 1, zoom);
        Self {
            south,
            west,
            north,
            east,
        }
    }

    #[must_use]
    pub fn from_swne([south, west, north, east]: [f64; 4]) -> Self {
        Self {
            south,
            west,
            north,
            east,
        }
    }

    #[must_use]
    pub fn contains(&self, lat: f64, lon: f64) -> bool {
        lat >= self.south && lat <= self.north && lon >= self.west && lon <= self.east
    }

    /// Whether the segment `a`–`b` touches the box (Liang–Barsky).
    #[must_use]
    pub fn crosses(&self, a: (f64, f64), b: (f64, f64)) -> bool {
        if self.contains(a.0, a.1) || self.contains(b.0, b.1) {
            return true;
        }
        let (dy, dx) = (b.0 - a.0, b.1 - a.1);
        let (mut t0, mut t1) = (0.0_f64, 1.0_f64);
        for (p, q) in [
            (-dx, a.1 - self.west),
            (dx, self.east - a.1),
            (-dy, a.0 - self.south),
            (dy, self.north - a.0),
        ] {
            if p == 0.0 {
                if q < 0.0 {
                    return false;
                }
            } else {
                let r = q / p;
                if p < 0.0 {
                    t0 = t0.max(r);
                } else {
                    t1 = t1.min(r);
                }
                if t0 > t1 {
                    return false;
                }
            }
        }
        true
    }
}

impl Way {
    /// Overpass's test for a way in a bbox: a node inside, or a segment across.
    #[must_use]
    pub fn touches(&self, bb: &Bbox) -> bool {
        match self.geom.as_slice() {
            [] => false,
            [only] => bb.contains(only.0, only.1),
            g => g.windows(2).any(|w| bb.crosses(w[0], w[1])),
        }
    }

    /// The middle of its bounding box — what Overpass's `out center` gives.
    #[must_use]
    pub fn center(&self) -> Option<(f64, f64)> {
        let b = self.bounds()?;
        Some(((b.south + b.north) / 2.0, (b.west + b.east) / 2.0))
    }

    #[must_use]
    pub fn bounds(&self) -> Option<Bbox> {
        let mut it = self.geom.iter();
        let &(lat, lon) = it.next()?;
        let mut b = Bbox {
            south: lat,
            west: lon,
            north: lat,
            east: lon,
        };
        for &(lat, lon) in it {
            b.south = b.south.min(lat);
            b.north = b.north.max(lat);
            b.west = b.west.min(lon);
            b.east = b.east.max(lon);
        }
        Some(b)
    }
}

/// What [`load`] kept, with a per-tile index to find it by.
#[derive(Debug, Default)]
pub struct Store {
    pub nodes: Vec<Node>,
    pub ways: Vec<Way>,
}

/// Read what `wanted` names from the extract at `path`, inside `area`.
pub fn load(ctx: &Ctx, path: &Path, area: Bbox, wanted: &Wanted) -> Outcome<Store> {
    let fail = |e: osmpbf::Error| PipelineError::Failed(format!("{}: {e}", path.display()));
    let reader = || ElementReader::from_path(path).map_err(fail);

    // 1. Tagged nodes, kept or joining, inside the area; and, for a city-sized
    //    area, the id of every node in it, so the next pass can drop a way that
    //    has no node there before its geometry is ever looked up. Without it every
    //    speed-limited road in the country was held until the third pass. The
    //    cost: a way that crosses the area with no node inside it is missed,
    //    which the cells' ring of padding keeps to the area's outer edge.
    ctx.check()?;
    ctx.log("OSM: knopen lezen");
    let small = (area.north - area.south) * (area.east - area.west) <= SMALL_AREA_DEG2;
    let (nodes, inside): (Vec<Node>, Vec<i64>) = reader()?
        .par_map_reduce(
            |el| {
                let (id, lat, lon, tags): (i64, f64, f64, Vec<(&str, &str)>) = match &el {
                    Element::Node(n) => (n.id(), n.lat(), n.lon(), n.tags().collect()),
                    Element::DenseNode(n) => (n.id(), n.lat(), n.lon(), n.tags().collect()),
                    _ => return (Vec::new(), Vec::new()),
                };
                if !area.contains(lat, lon) {
                    return (Vec::new(), Vec::new());
                }
                let inside = if small { vec![id] } else { Vec::new() };
                if tags.is_empty() {
                    return (Vec::new(), inside);
                }
                let keep = (wanted.node)(&tags);
                let joins = (wanted.join_node)(&tags);
                if !keep && !joins {
                    return (Vec::new(), inside);
                }
                let node = Node {
                    id,
                    lat: round7(lat),
                    lon: round7(lon),
                    tags: owned(&tags),
                    joins,
                };
                (vec![node], inside)
            },
            || (Vec::new(), Vec::new()),
            |(a, b), (c, d)| (concat(a, c), concat(b, d)),
        )
        .map_err(fail)?;
    let inside: Option<HashSet<i64>> = small.then(|| inside.into_iter().collect());
    let joining: HashSet<i64> = nodes.iter().filter(|n| n.joins).map(|n| n.id).collect();

    // 2. Ways, kept for their tags or for passing through a joining node.
    ctx.check()?;
    ctx.log(format!("OSM: {} knopen; wegen lezen", nodes.len()));
    let mut ways: Vec<Way> = reader()?
        .par_map_reduce(
            |el| {
                let Element::Way(w) = &el else {
                    return Vec::new();
                };
                let tags: Vec<(&str, &str)> = w.tags().collect();
                if tags.is_empty() {
                    return Vec::new();
                }
                let keep = (wanted.way)(&tags)
                    || (!joining.is_empty()
                        && (wanted.join_way)(&tags)
                        && w.refs().any(|r| joining.contains(&r)));
                if !keep
                    || inside
                        .as_ref()
                        .is_some_and(|s| !w.refs().any(|r| s.contains(&r)))
                {
                    return Vec::new();
                }
                vec![Way {
                    id: w.id(),
                    refs: w.refs().collect(),
                    geom: Vec::new(),
                    tags: owned(&tags),
                }]
            },
            Vec::new,
            concat,
        )
        .map_err(fail)?;

    // 3. Where those ways' nodes are.
    ctx.check()?;
    let needed: HashSet<i64> = ways.iter().flat_map(|w| w.refs.iter().copied()).collect();
    ctx.log(format!(
        "OSM: {} wegen; {} knooppunten plaatsen",
        ways.len(),
        needed.len()
    ));
    let located: HashMap<i64, (f64, f64)> = reader()?
        .par_map_reduce(
            |el| {
                let (id, lat, lon) = match &el {
                    Element::Node(n) => (n.id(), n.lat(), n.lon()),
                    Element::DenseNode(n) => (n.id(), n.lat(), n.lon()),
                    _ => return Vec::new(),
                };
                if needed.contains(&id) {
                    vec![(id, (round7(lat), round7(lon)))]
                } else {
                    Vec::new()
                }
            },
            Vec::new,
            concat,
        )
        .map_err(fail)?
        .into_iter()
        .collect();
    for way in &mut ways {
        let (refs, geom): (Vec<i64>, Vec<(f64, f64)>) = way
            .refs
            .iter()
            .filter_map(|r| located.get(r).map(|&p| (*r, p)))
            .unzip();
        way.refs = refs;
        way.geom = geom;
    }
    ways.retain(|w| w.touches(&area));
    let mut store = Store { nodes, ways };
    store.nodes.sort_by_key(|n| n.id);
    store.ways.sort_by_key(|w| w.id);
    Ok(store)
}

fn concat<T>(mut a: Vec<T>, mut b: Vec<T>) -> Vec<T> {
    if a.len() < b.len() {
        std::mem::swap(&mut a, &mut b);
    }
    a.append(&mut b);
    a
}

/// Overpass writes seven decimals; so does this, so a file compares equal.
fn round7(v: f64) -> f64 {
    (v * 1e7).round() / 1e7
}

/// Which of the store's elements lie in each tile at one zoom, so a run over
/// thousands of cells does not scan the store once per cell.
pub struct TileIndex {
    pub zoom: u32,
    nodes: HashMap<(i64, i64), Vec<usize>>,
    ways: HashMap<(i64, i64), Vec<usize>>,
}

impl TileIndex {
    #[must_use]
    pub fn new(store: &Store, zoom: u32) -> Self {
        let mut nodes: HashMap<(i64, i64), Vec<usize>> = HashMap::new();
        for (i, n) in store.nodes.iter().enumerate() {
            nodes
                .entry(lat_lng_to_tile(n.lat, n.lon, zoom))
                .or_default()
                .push(i);
        }
        let mut ways: HashMap<(i64, i64), Vec<usize>> = HashMap::new();
        for (i, w) in store.ways.iter().enumerate() {
            let Some(b) = w.bounds() else { continue };
            let (x0, y0) = lat_lng_to_tile(b.north, b.west, zoom);
            let (x1, y1) = lat_lng_to_tile(b.south, b.east, zoom);
            for y in y0..=y1 {
                for x in x0..=x1 {
                    ways.entry((x, y)).or_default().push(i);
                }
            }
        }
        Self { zoom, nodes, ways }
    }

    /// The nodes inside `bb` and the ways touching it, each once, by id —
    /// Overpass's order within a type.
    #[must_use]
    pub fn query<'s>(&self, store: &'s Store, bb: &Bbox) -> (Vec<&'s Node>, Vec<&'s Way>) {
        let (x0, y0) = lat_lng_to_tile(bb.north, bb.west, self.zoom);
        let (x1, y1) = lat_lng_to_tile(bb.south, bb.east, self.zoom);
        let (mut ni, mut wi) = (Vec::new(), Vec::new());
        for y in y0..=y1 {
            for x in x0..=x1 {
                if let Some(v) = self.nodes.get(&(x, y)) {
                    ni.extend(v.iter().copied());
                }
                if let Some(v) = self.ways.get(&(x, y)) {
                    wi.extend(v.iter().copied());
                }
            }
        }
        ni.sort_unstable();
        ni.dedup();
        wi.sort_unstable();
        wi.dedup();
        let nodes = ni
            .into_iter()
            .map(|i| &store.nodes[i])
            .filter(|n| bb.contains(n.lat, n.lon))
            .collect();
        let ways = wi
            .into_iter()
            .map(|i| &store.ways[i])
            .filter(|w| w.touches(bb))
            .collect();
        (nodes, ways)
    }
}

/// How a way is written: Overpass's `out center` or `out geom`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Out {
    Center,
    Geom,
    /// `out;` — nodes only in the layers that use it; ways as bare bodies.
    Body,
}

fn tags_value(tags: &Tags) -> Value {
    Value::Object(
        tags.iter()
            .map(|(k, v)| (k.clone(), Value::String(v.clone())))
            .collect::<Map<_, _>>(),
    )
}

#[must_use]
pub fn node_value(n: &Node) -> Value {
    let mut v = json!({"type": "node", "id": n.id, "lat": n.lat, "lon": n.lon});
    if !n.tags.is_empty() {
        v["tags"] = tags_value(&n.tags);
    }
    v
}

#[must_use]
pub fn way_value(w: &Way, out: Out) -> Value {
    let mut v = json!({"type": "way", "id": w.id});
    match out {
        Out::Center => {
            if let Some((lat, lon)) = w.center() {
                v["center"] = json!({"lat": round7(lat), "lon": round7(lon)});
            }
            v["nodes"] = json!(w.refs);
        }
        Out::Geom => {
            if let Some(b) = w.bounds() {
                v["bounds"] = json!({
                    "minlat": b.south, "minlon": b.west, "maxlat": b.north, "maxlon": b.east
                });
            }
            v["nodes"] = json!(w.refs);
            v["geometry"] = Value::Array(
                w.geom
                    .iter()
                    .map(|&(lat, lon)| json!({"lat": lat, "lon": lon}))
                    .collect(),
            );
        }
        Out::Body => v["nodes"] = json!(w.refs),
    }
    if !w.tags.is_empty() {
        v["tags"] = tags_value(&w.tags);
    }
    v
}

/// A whole answer: nodes first, then ways, each by id, as Overpass orders the
/// output of a union.
#[must_use]
pub fn document(nodes: &[&Node], ways: &[&Way], out: Out) -> Value {
    let mut elements: Vec<Value> = nodes.iter().map(|n| node_value(n)).collect();
    elements.extend(ways.iter().map(|w| way_value(w, out)));
    json!({
        "version": 0.6,
        "generator": "cartopolis atlas (OpenStreetMap extract)",
        "osm3s": {
            "copyright": "The data included in this document is from www.openstreetmap.org. The data is made available under ODbL."
        },
        "elements": elements,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn way(id: i64, geom: &[(f64, f64)]) -> Way {
        Way {
            id,
            refs: (0..geom.len() as i64).collect(),
            geom: geom.to_vec(),
            tags: Tags::new(),
        }
    }

    const BOX: Bbox = Bbox {
        south: 53.0,
        west: 6.0,
        north: 53.1,
        east: 6.1,
    };

    #[test]
    fn a_way_crossing_the_box_without_a_node_inside_touches_it() {
        let across = way(1, &[(53.05, 5.9), (53.05, 6.2)]);
        assert!(across.touches(&BOX));
        let beside = way(2, &[(53.2, 5.9), (53.2, 6.2)]);
        assert!(!beside.touches(&BOX));
        let diagonal_miss = way(3, &[(53.15, 6.0), (53.2, 6.05)]);
        assert!(!diagonal_miss.touches(&BOX));
    }

    #[test]
    fn center_is_the_middle_of_the_bounds_not_the_mean_of_the_nodes() {
        let w = way(1, &[(53.0, 6.0), (53.0, 6.1), (53.0, 6.1), (53.2, 6.1)]);
        assert_eq!(w.center(), Some((53.1, 6.05)));
    }

    #[test]
    fn the_index_finds_a_long_way_from_every_tile_it_crosses() {
        let mut store = Store::default();
        store.ways.push(way(7, &[(53.21, 6.50), (53.21, 6.62)]));
        let index = TileIndex::new(&store, 14);
        let (x, y) = lat_lng_to_tile(53.21, 6.56, 14);
        let (_, ways) = index.query(&store, &Bbox::tile(x, y, 14));
        assert_eq!(ways.len(), 1, "the middle tile holds no node of it");
    }

    #[test]
    fn a_document_puts_nodes_before_ways() {
        let n = Node {
            id: 9,
            lat: 53.0,
            lon: 6.0,
            tags: Tags::new(),
            joins: false,
        };
        let w = way(1, &[(53.0, 6.0), (53.1, 6.1)]);
        let doc = document(&[&n], &[&w], Out::Geom);
        let types: Vec<&str> = doc["elements"]
            .as_array()
            .unwrap()
            .iter()
            .map(|e| e["type"].as_str().unwrap())
            .collect();
        assert_eq!(types, ["node", "way"]);
        assert!(
            doc["elements"][0].get("tags").is_none(),
            "no empty tags object"
        );
        assert_eq!(doc["elements"][1]["geometry"][1]["lon"], 6.1);
    }
}

/// Build one layer over the area: read the extract once, write each cell's
/// document as `<dataset>/<zoom>/<x>/<y>.json` and the coverage beside them.
///
/// An empty cell is written too: it is the answer "nothing here", which the
/// client must not mistake for a cell nobody built and go and ask Overpass.
pub fn build_cells(
    ctx: &Ctx,
    dataset: &str,
    zoom: u32,
    cells: &[(i64, i64)],
    wanted: &Wanted,
    cell: impl Fn(&Store, &TileIndex, i64, i64) -> Value,
    attribution: &str,
) -> Outcome<crate::pipeline::DatasetResult> {
    use crate::pipeline::{Count, DatasetResult, Progress};
    use cartopolis_geo::formats::coverage::Manifest;

    if cells.is_empty() {
        return Err(PipelineError::Failed("no cells in the area".into()));
    }
    let path = ensure_extract(ctx)?;
    let total = cells.len();
    let chunks = chunked(cells, zoom);
    ctx.log(format!(
        "{total} z{zoom}-cellen schrijven, in {} stuk(ken)",
        chunks.len()
    ));
    let (mut elements, mut done) = (0usize, 0usize);
    for chunk in &chunks {
        let Some(area) = cells_bounds(chunk, zoom) else {
            continue;
        };
        let store = load(ctx, &path, area, wanted)?;
        let index = TileIndex::new(&store, zoom);
        for &(x, y) in chunk {
            ctx.check()?;
            let doc = cell(&store, &index, x, y);
            elements += doc["elements"].as_array().map_or(0, Vec::len);
            ctx.write_json(&format!("{dataset}/{zoom}/{x}/{y}.json"), &doc)?;
            done += 1;
            if done % 256 == 0 || done == total {
                ctx.progress(Progress {
                    phase: Some("cellen".into()),
                    done: Some(done as u64),
                    total: Some(Some(total as u64)),
                });
            }
        }
    }
    ctx.write_coverage(
        &format!("{dataset}/coverage.json"),
        &Manifest::new(
            zoom,
            crate::pipeline::tile::sorted_cells(cells.iter().copied()),
        )
        .described(
            ctx.name.as_str(),
            attribution,
            "OpenStreetMap extract (Geofabrik)",
            ctx.bbox,
        )
        .with("element_count", elements),
    )?;
    ctx.log(format!("{elements} elementen over {total} cellen"));
    Ok(DatasetResult {
        cells: total,
        counts: vec![Count::new("OSM elements", elements)],
    })
}

/// The zoom whose tiles an area is read in, one at a time: a z9 tile is about
/// 70 × 45 km here, small enough for [`load`]'s early way filter, so a
/// whole-country run holds one province-sized piece of the map at a time
/// rather than every building in the country.
const CHUNK_ZOOM: u32 = 9;

/// The cells grouped by the [`CHUNK_ZOOM`] tile they fall in, in row order.
fn chunked(cells: &[(i64, i64)], zoom: u32) -> Vec<Vec<(i64, i64)>> {
    let shift = zoom.saturating_sub(CHUNK_ZOOM);
    let mut groups: BTreeMap<(i64, i64), Vec<(i64, i64)>> = BTreeMap::new();
    for &(x, y) in cells {
        groups
            .entry((y >> shift, x >> shift))
            .or_default()
            .push((x, y));
    }
    groups.into_values().collect()
}

/// The area the cells cover together.
fn cells_bounds(cells: &[(i64, i64)], zoom: u32) -> Option<Bbox> {
    let x0 = cells.iter().map(|c| c.0).min()?;
    let x1 = cells.iter().map(|c| c.0).max()?;
    let y0 = cells.iter().map(|c| c.1).min()?;
    let y1 = cells.iter().map(|c| c.1).max()?;
    let (north, west) = tile_to_lat_lng(x0, y0, zoom);
    let (south, east) = tile_to_lat_lng(x1 + 1, y1 + 1, zoom);
    Some(Bbox {
        south,
        west,
        north,
        east,
    })
}

/// Every cell at `zoom` the RD area touches, grown by `ring` on every side —
/// the cells a client streaming that many around itself asks for.
#[must_use]
pub fn area_cells(bbox_rd: [f64; 4], zoom: u32, ring: i64) -> Vec<(i64, i64)> {
    let [west, south, east, north] = crate::pipeline::rd::wgs84_bbox(bbox_rd);
    let (x0, y0) = lat_lng_to_tile(north, west, zoom);
    let (x1, y1) = lat_lng_to_tile(south, east, zoom);
    let mut cells = Vec::new();
    for y in y0 - ring..=y1 + ring {
        for x in x0 - ring..=x1 + ring {
            cells.push((x, y));
        }
    }
    cells
}

/// Cells out from the client's own cell its z14 lanes stream (3×3), and atlas
/// grows an area by: `street_detail::CELL_RADIUS`, `data_layers`'
/// `DETAIL_CELL_RADIUS`.
const Z14_RING: i64 = 1;

const ATTRIBUTION: &str = "© OpenStreetMap contributors (ODbL)";

pub fn run_street(ctx: &Ctx) -> Outcome<crate::pipeline::DatasetResult> {
    let cells = area_cells(ctx.bbox, layers::street::ZOOM, Z14_RING);
    build_cells(
        ctx,
        "street",
        layers::street::ZOOM,
        &cells,
        &layers::street::WANTED,
        layers::street::cell,
        ATTRIBUTION,
    )
}

pub fn run_transit(ctx: &Ctx) -> Outcome<crate::pipeline::DatasetResult> {
    let cells = area_cells(ctx.bbox, layers::pois::ZOOM, Z14_RING);
    build_cells(
        ctx,
        "transit",
        layers::pois::ZOOM,
        &cells,
        &layers::pois::TRANSIT,
        |s, i, x, y| layers::pois::cell(s, i, x, y, layers::pois::transit),
        ATTRIBUTION,
    )
}

pub fn run_shops(ctx: &Ctx) -> Outcome<crate::pipeline::DatasetResult> {
    let cells = area_cells(ctx.bbox, layers::pois::ZOOM, Z14_RING);
    build_cells(
        ctx,
        "shops",
        layers::pois::ZOOM,
        &cells,
        &layers::pois::SHOPS,
        |s, i, x, y| layers::pois::cell(s, i, x, y, layers::pois::shop),
        ATTRIBUTION,
    )
}

pub fn run_speeds(ctx: &Ctx) -> Outcome<crate::pipeline::DatasetResult> {
    let cells = area_cells(ctx.bbox, layers::speeds::ZOOM, 0);
    build_cells(
        ctx,
        "speeds",
        layers::speeds::ZOOM,
        &cells,
        &layers::speeds::WANTED,
        layers::speeds::cell,
        ATTRIBUTION,
    )
}

/// Cells out from the camera's the client streams OSM buildings over, to the
/// skyline (`osm_buildings::FAR_SKYLINE_RADIUS`); atlas grows an area by as many.
const BUILDINGS_RING: i64 = 5;

pub fn run_buildings(ctx: &Ctx) -> Outcome<crate::pipeline::DatasetResult> {
    let cells = area_cells(ctx.bbox, layers::buildings::ZOOM, BUILDINGS_RING);
    build_cells(
        ctx,
        "osmbuildings",
        layers::buildings::ZOOM,
        &cells,
        &layers::buildings::WANTED,
        layers::buildings::cell,
        ATTRIBUTION,
    )
}

/// The tags of every relation that passes `pred`, wherever it is: no geometry,
/// so no area test. For a question answered by tags alone (`bag`'s materials),
/// where an element outside the area costs a lookup that finds nothing.
pub fn relation_tags(
    ctx: &Ctx,
    path: &Path,
    pred: fn(&TagView<'_>) -> bool,
) -> Outcome<Vec<(i64, Tags)>> {
    ctx.check()?;
    ElementReader::from_path(path)
        .and_then(|r| {
            r.par_map_reduce(
                |el| {
                    let Element::Relation(rel) = &el else {
                        return Vec::new();
                    };
                    let tags: Vec<(&str, &str)> = rel.tags().collect();
                    if pred(&tags) {
                        vec![(rel.id(), owned(&tags))]
                    } else {
                        Vec::new()
                    }
                },
                Vec::new,
                concat,
            )
        })
        .map_err(|e| PipelineError::Failed(format!("{}: {e}", path.display())))
}

/// One Overpass-shaped answer for an area given as `[west, south, east, north]`
/// (what `rd::wgs84_bbox` returns): the nodes and ways `wanted` names that lie
/// in it, written as `out`.
pub fn area_document(ctx: &Ctx, wgs84: [f64; 4], wanted: &Wanted, out: Out) -> Outcome<Value> {
    let [west, south, east, north] = wgs84;
    let area = Bbox {
        south,
        west,
        north,
        east,
    };
    let path = ensure_extract(ctx)?;
    let store = load(ctx, &path, area, wanted)?;
    let nodes: Vec<&Node> = store
        .nodes
        .iter()
        .filter(|n| (wanted.node)(&layers::view(&n.tags)))
        .collect();
    let ways: Vec<&Way> = store.ways.iter().collect();
    Ok(document(&nodes, &ways, out))
}

/// A [`crate::pipeline::Sink`] for a [`Ctx`] made outside a job (the backdrop
/// route): its lines go to the log.
pub struct TracingSink;

impl crate::pipeline::Sink for TracingSink {
    fn log(&self, line: String) {
        tracing::info!("{line}");
    }
    fn progress(&self, _: crate::pipeline::Progress) {}
}

#[cfg(test)]
mod chunk_tests {
    use super::*;

    #[test]
    fn a_city_is_one_chunk_and_a_country_is_many() {
        let city = area_cells([229_000.0, 578_000.0, 238_000.0, 587_000.0], 14, 1);
        assert!(
            chunked(&city, 14).len() <= 2,
            "Groningen straddles at most one edge"
        );
        let country = area_cells([13_000.0, 306_000.0, 278_000.0, 620_000.0], 14, 0);
        let chunks = chunked(&country, 14);
        assert!(chunks.len() > 20, "{}", chunks.len());
        assert_eq!(chunks.iter().map(Vec::len).sum::<usize>(), country.len());
        for c in &chunks {
            let b = cells_bounds(c, 14).unwrap();
            assert!((b.north - b.south) * (b.east - b.west) <= SMALL_AREA_DEG2);
        }
    }
}
