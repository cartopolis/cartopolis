//! The **BAG** — what the buildings *are*, as opposed to what shape they are
//! (`building_facts.rs` and the places readout in the client read it).
//!
//! Ported from the earlier TypeScript pipeline (`bag.ts`, itself from
//! `tools/bag_attributes.py`). Year, use, floor areas, addresses, entrance points
//! and heritage status, every record keyed on the BAG **identificatie** — the
//! string `lod22` writes as each mesh's `id` — so the join needs no geometry.
//!
//! Source is PDOK's keyless BAG WFS: `bag:pand` (one row per building),
//! `bag:verblijfsobject` (one per addressable unit, with a point at or near its
//! entrance) and `bag:ligplaats` / `bag:standplaats` (houseboat berths and
//! mobile-home plots, which have no pand at all). Two heritage layers come from the
//! RCE's own WFS and are joined here, the only place holding both the monument
//! points and the footprints to test them against.
//!
//! Two files per cell, split by who pays: `lod22` fetches the buildings inside its
//! mesh job and has no use for two megabytes of addresses.
//!
//! Data: Kadaster BAG via PDOK — open data, attribution requested.

use std::collections::{HashMap, HashSet};

use serde::Serialize;
use serde_json::{json, Value};

use super::geom::{
    geom_coords, point_in_rings, polygon_rings, ring_area, rings_of, round, simplify, Pt,
};
use super::http::{get_json, get_json_with, get_text, query};
use super::rd::rd_to_lat_lng;
use super::tile::{cell_path, lat_lng_to_cell, sorted_cells, Cell, CACHE_ZOOM};
use super::{in_order, Count, Ctx, DatasetResult, Outcome, PipelineError, Progress};

const WFS_BASE: &str = "https://service.pdok.nl/lv/bag/wfs/v2_0";
/// The heritage register: not on PDOK, but the RCE's own WFS, with English layer
/// names and Dutch attributes.
const RCE_WFS: &str = "https://services.rce.geovoorziening.nl/rce/wfs";
/// How far a monument point may sit from a footprint and still describe it.
const MONUMENT_SNAP_M: f64 = 12.0;
/// The same 10 cm the paving uses.
const FOOTPRINT_SIMPLIFY_M: f64 = 0.1;
/// Below this a pand is a meter cupboard or a bin store, and a door in it would
/// be a door to nowhere.
const MIN_FOOTPRINT_M2: f64 = 6.0;
/// **Measured, not the documented cap**: this service truncates GeoJSON at 1000
/// features however large a `count` you ask for, and usually omits
/// `numberMatched` — so "fewer than I asked for" would end the walk after the
/// first thousand rows. Hence this number, and `hits` to check the total.
const PAGE: usize = 1_000;
/// `startIndex` is refused past 50,000 (HTTP 400), and Groningen alone has 94,052
/// panden, so a bbox is split until each piece pages to the end.
const MAX_START_INDEX: usize = 50_000;
const MAX_PER_BBOX: u64 = 40_000;
/// Four levels is a 256-fold reduction; anything still over the ceiling is not a
/// density problem.
const MAX_SPLIT_DEPTH: u32 = 4;

const ATTRIBUTION: &str = "Building attributes © Kadaster BAG via PDOK (open data)";

/// The BAG's closed set of `gebruiksdoel` values as a bitmask. Order **is** the
/// wire format — append, never renumber — mirrored by `BuildingUse` in the client.
const USE_BITS: [&str; 11] = [
    "woonfunctie",
    "winkelfunctie",
    "kantoorfunctie",
    "bijeenkomstfunctie",
    "industriefunctie",
    "onderwijsfunctie",
    "gezondheidszorgfunctie",
    "logiesfunctie",
    "sportfunctie",
    "celfunctie",
    "overige gebruiksfunctie",
];

const STATUS_IN_USE: &str = "Pand in gebruik";

fn status_code(status: &str) -> Option<&'static str> {
    Some(match status {
        "Bouwvergunning verleend" => "permit",
        "Bouw gestart" => "building",
        "Pand in gebruik (niet ingemeten)" => "in_use_unmeasured",
        "Verbouwing pand" => "renovation",
        "Pand buiten gebruik" => "out_of_use",
        "Sloopvergunning verleend" => "demolition_permit",
        "Pand gesloopt" => "demolished",
        "Niet gerealiseerd pand" => "not_realised",
        "Pand ten onrechte opgevoerd" => "withdrawn",
        _ => return None,
    })
}

type Bbox = [f64; 4];

// ---- JavaScript's coercions, which the formats were written against ----

/// `String(v)`: a number without a `.0`, a string as is, an array joined by
/// commas, `null`/absent as "".
fn js_string(v: &Value) -> String {
    match v {
        Value::Null => String::new(),
        Value::String(s) => s.clone(),
        Value::Number(n) => match (n.as_i64(), n.as_f64()) {
            (Some(i), _) => i.to_string(),
            (None, Some(f)) if f.fract() == 0.0 && f.abs() < 1e15 => format!("{}", f as i64),
            (None, Some(f)) => f.to_string(),
            _ => n.to_string(),
        },
        Value::Bool(b) => b.to_string(),
        Value::Array(a) => a.iter().map(js_string).collect::<Vec<_>>().join(","),
        Value::Object(_) => "[object Object]".into(),
    }
}

/// `Number(v)` — `NaN` where JavaScript would give it.
fn js_number(v: &Value) -> f64 {
    match v {
        Value::Null => 0.0,
        Value::Bool(b) => f64::from(u8::from(*b)),
        Value::Number(n) => n.as_f64().unwrap_or(f64::NAN),
        Value::String(s) => {
            let t = s.trim();
            if t.is_empty() {
                0.0
            } else {
                t.parse().unwrap_or(f64::NAN)
            }
        }
        _ => f64::NAN,
    }
}

/// `Math.trunc(Number(v))`, as an integer, or `None` for NaN.
fn js_trunc(v: &Value) -> Option<i64> {
    let n = js_number(v).trunc();
    n.is_finite().then_some(n as i64)
}

/// JavaScript truthiness.
fn truthy(v: &Value) -> bool {
    match v {
        Value::Null => false,
        Value::Bool(b) => *b,
        Value::Number(n) => n.as_f64().is_some_and(|f| f != 0.0 && !f.is_nan()),
        Value::String(s) => !s.is_empty(),
        _ => true,
    }
}

/// `"winkelfunctie,woonfunctie"` → the OR of their bits (0 if unknown). An
/// unrecognised word is skipped rather than mapped to "overige": the mask is read
/// as evidence, and inventing a use would be worse than saying nothing.
#[must_use]
pub fn use_mask(gebruiksdoel: &Value) -> u32 {
    js_string(gebruiksdoel)
        .split(',')
        .filter_map(|part| USE_BITS.iter().position(|b| *b == part.trim()))
        .fold(0, |m, i| m | (1 << i))
}

/// `62` + `A` + `bis` → `"62Abis"`, the way the address is written.
#[must_use]
pub fn house_number(props: &Value) -> Option<String> {
    let nr = props.get("huisnummer")?;
    if nr.is_null() {
        return None;
    }
    Some(format!(
        "{}{}{}",
        js_string(nr),
        js_string(props.get("huisletter").unwrap_or(&Value::Null)),
        js_string(props.get("toevoeging").unwrap_or(&Value::Null))
    ))
}

fn js_num_str(v: f64) -> String {
    if v.fract() == 0.0 && v.abs() < 1e15 {
        format!("{}", v as i64)
    } else {
        format!("{v}")
    }
}

fn bbox_param(b: Bbox) -> String {
    format!(
        "{},{},{},{},EPSG:28992",
        js_num_str(b[0]),
        js_num_str(b[1]),
        js_num_str(b[2]),
        js_num_str(b[3])
    )
}

fn feature_url(base: &str, layer: &str, bbox: Bbox, start: usize) -> String {
    format!(
        "{base}?{}",
        query(&[
            ("service", "WFS"),
            ("version", "2.0.0"),
            ("request", "GetFeature"),
            ("typeNames", layer),
            ("count", &PAGE.to_string()),
            ("startIndex", &start.to_string()),
            ("outputFormat", "application/json"),
            ("srsName", "EPSG:28992"),
            ("bbox", &bbox_param(bbox)),
        ])
    )
}

/// How many features the layer has over the bbox, or `None` if unreadable.
/// `resultType=hits` answers in GML whatever format was asked for, so this is a
/// tiny XML read, and it is what makes a truncated walk detectable.
fn hits(ctx: &Ctx, layer: &str, bbox: Bbox) -> Outcome<Option<u64>> {
    let url = format!(
        "{WFS_BASE}?{}",
        query(&[
            ("service", "WFS"),
            ("version", "2.0.0"),
            ("request", "GetFeature"),
            ("typeNames", layer),
            ("resultType", "hits"),
            ("srsName", "EPSG:28992"),
            ("bbox", &bbox_param(bbox)),
        ])
    );
    match get_text(ctx, &url, &format!("{layer} hits")) {
        Ok(head) => Ok(head
            .split_once("numberMatched=\"")
            .and_then(|(_, rest)| rest.split_once('"'))
            .and_then(|(n, _)| n.parse().ok())),
        Err(PipelineError::Cancelled) => Err(PipelineError::Cancelled),
        Err(e) => {
            ctx.log(format!("{layer}: hits unavailable ({e})"));
            Ok(None)
        }
    }
}

/// Sub-bboxes each holding few enough features to page to the end, quartered
/// recursively. A WFS bbox is an intersects test, so a building straddling a cut
/// comes back from both halves — every collector dedupes on identificatie.
fn split_bbox(
    ctx: &Ctx,
    layer: &str,
    bbox: Bbox,
    depth: u32,
    out: &mut Vec<(Bbox, Option<u64>)>,
) -> Outcome<()> {
    let n = hits(ctx, layer, bbox)?;
    if n.is_none_or(|n| n <= MAX_PER_BBOX) || depth >= MAX_SPLIT_DEPTH {
        if let Some(n) = n.filter(|n| *n > MAX_PER_BBOX) {
            ctx.log(format!(
                "WARNING: {layer} still has {n} features in [{}] at split depth {depth}; the walk will \
                 stop at {MAX_START_INDEX} and this area will be incomplete.",
                bbox.map(js_num_str).join(",")
            ));
        }
        out.push((bbox, n));
        return Ok(());
    }
    let [xmin, ymin, xmax, ymax] = bbox;
    let (xmid, ymid) = ((xmin + xmax) / 2.0, (ymin + ymax) / 2.0);
    for quad in [
        [xmin, ymin, xmid, ymid],
        [xmid, ymin, xmax, ymid],
        [xmin, ymid, xmid, ymax],
        [xmid, ymid, xmax, ymax],
    ] {
        split_bbox(ctx, layer, quad, depth + 1, out)?;
    }
    Ok(())
}

/// Every feature of a BAG layer over the bbox, walking each split piece to a short
/// page and checking the running total against `hits`, since both server limits
/// fail quietly.
fn pages(ctx: &Ctx, layer: &str, mut visit: impl FnMut(&Value)) -> Outcome<()> {
    let mut pieces = Vec::new();
    split_bbox(ctx, layer, ctx.bbox, 0, &mut pieces)?;
    if pieces.len() > 1 {
        ctx.log(format!(
            "{layer}: split into {} bboxes to stay under the {MAX_START_INDEX}-row paging limit",
            pieces.len()
        ));
    }
    let grand: u64 = pieces.iter().map(|(_, n)| n.unwrap_or(0)).sum();
    let mut seen_total = 0u64;
    for (piece, total) in pieces {
        // **With the count known, every page's `startIndex` is too**, so the
        // pages are fetched a few at a time and visited in order. One at a time,
        // Groningen's ~225 pages were 62 of the dataset's 67 s (2026-09-24), each
        // page mostly the server's latency. A page that comes back short no longer
        // ends the walk early: the pages after it are asked for anyway, so a
        // server that truncates one page loses that page rather than the rest.
        if let Some(t) = total {
            let reachable = usize::try_from(t)
                .unwrap_or(usize::MAX)
                .min(MAX_START_INDEX);
            let starts: Vec<usize> = (0..reachable).step_by(PAGE).collect();
            let mut got = 0usize;
            in_order(
                ctx,
                &starts,
                ctx.upstream(),
                |&start| {
                    get_json(
                        ctx,
                        &feature_url(WFS_BASE, layer, piece, start),
                        &format!("{layer} page {start}"),
                    )
                },
                |_, page| {
                    let page = page?;
                    let feats = page["features"].as_array().map_or(&[][..], Vec::as_slice);
                    feats.iter().for_each(&mut visit);
                    got += feats.len();
                    seen_total += feats.len() as u64;
                    ctx.progress(Progress {
                        phase: Some(layer.into()),
                        done: Some(seen_total),
                        total: Some((grand > 0).then_some(grand)),
                    });
                    Ok(())
                },
            )?;
            if (got as u64) < t {
                ctx.log(format!(
                    "WARNING: {layer} yielded {got} of {t} features for [{}] — that piece is incomplete.",
                    piece.map(js_num_str).join(",")
                ));
            }
            continue;
        }
        // No count: walk until a short page, the only end there is to see.
        let mut start = 0usize;
        loop {
            ctx.check()?;
            let page = get_json(
                ctx,
                &feature_url(WFS_BASE, layer, piece, start),
                &format!("{layer} page {start}"),
            )?;
            let feats = page["features"].as_array().map_or(&[][..], Vec::as_slice);
            if feats.is_empty() {
                break;
            }
            feats.iter().for_each(&mut visit);
            let got = feats.len();
            start += got;
            seen_total += got as u64;
            ctx.progress(Progress {
                phase: Some(layer.into()),
                done: Some(seen_total),
                total: Some((grand > 0).then_some(grand)),
            });
            if got < PAGE || start >= MAX_START_INDEX {
                break;
            }
        }
    }
    Ok(())
}

/// A feature's RD bbox, from the server's own `bbox` or from its coordinates.
fn rd_bbox(feat: &Value) -> Option<Bbox> {
    if let Some(bb) = feat["bbox"].as_array().filter(|b| b.len() >= 4) {
        return Some([
            bb[0].as_f64()?,
            bb[1].as_f64()?,
            bb[2].as_f64()?,
            bb[3].as_f64()?,
        ]);
    }
    let pts = geom_coords(&feat["geometry"]);
    if pts.is_empty() {
        return None;
    }
    let f =
        |i: usize, pick: fn(f64, f64) -> f64, init: f64| pts.iter().map(|p| p[i]).fold(init, pick);
    Some([
        f(0, f64::min, f64::INFINITY),
        f(1, f64::min, f64::INFINITY),
        f(0, f64::max, f64::NEG_INFINITY),
        f(1, f64::max, f64::NEG_INFINITY),
    ])
}

/// Every z14 cell an RD bbox touches, from all four corners (RD → WGS84 rotates).
fn cells_of_bbox(bb: Bbox) -> Vec<Cell> {
    let [xmin, ymin, xmax, ymax] = bb;
    let corners =
        [(xmin, ymin), (xmax, ymin), (xmin, ymax), (xmax, ymax)].map(|(x, y)| rd_to_lat_lng(x, y));
    let max_lat = corners
        .iter()
        .map(|c| c.0)
        .fold(f64::NEG_INFINITY, f64::max);
    let min_lat = corners.iter().map(|c| c.0).fold(f64::INFINITY, f64::min);
    let max_lng = corners
        .iter()
        .map(|c| c.1)
        .fold(f64::NEG_INFINITY, f64::max);
    let min_lng = corners.iter().map(|c| c.1).fold(f64::INFINITY, f64::min);
    // Tile y grows southward, so the north-west corner is (min lng, max lat).
    let (x0, y0) = lat_lng_to_cell(max_lat, min_lng);
    let (x1, y1) = lat_lng_to_cell(min_lat, max_lng);
    let mut out = Vec::new();
    for cx in x0..=x1 {
        for cy in y0..=y1 {
            out.push((cx, cy));
        }
    }
    out
}

/// A feature's representative `[lat, lng]` — its bbox centre.
fn point_of(feat: &Value) -> Option<[f64; 2]> {
    let bb = rd_bbox(feat)?;
    let (lat, lng) = rd_to_lat_lng((bb[0] + bb[2]) / 2.0, (bb[1] + bb[3]) / 2.0);
    Some([round(lat, 7), round(lng, 7)])
}

#[derive(Serialize, Default)]
struct Building {
    #[serde(skip_serializing_if = "Option::is_none")]
    area: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    area_max: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    district: Option<usize>,
    #[serde(skip_serializing_if = "Option::is_none")]
    doors: Option<Vec<[f64; 2]>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    fp: Option<Vec<[f64; 2]>>,
    id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    mcat: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    mon: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    status: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    units: Option<i64>,
    #[serde(rename = "use", skip_serializing_if = "Option::is_none")]
    use_: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    year: Option<i64>,
}

#[derive(Serialize)]
struct Address {
    #[serde(skip_serializing_if = "Option::is_none")]
    area: Option<i64>,
    at: [f64; 2],
    #[serde(skip_serializing_if = "Option::is_none")]
    id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    nr: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    postcode: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    street: Option<usize>,
    #[serde(rename = "use", skip_serializing_if = "Option::is_none")]
    use_: Option<u32>,
}

#[derive(Serialize)]
struct Berth {
    at: [f64; 2],
    kind: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    nr: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    postcode: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    street: Option<usize>,
}

#[derive(Default)]
struct CellData {
    /// Indices into the one building list: a building spanning several cells is
    /// one record, so the later heritage and door joins reach all its copies.
    buildings: Vec<usize>,
    addresses: Vec<Address>,
    berths: Vec<Berth>,
    streets: Vec<String>,
    street_ix: HashMap<String, usize>,
}

/// The per-cell accumulator, in first-touched order, with a street table per cell.
#[derive(Default)]
struct Cells {
    order: Vec<Cell>,
    cells: HashMap<Cell, CellData>,
}

impl Cells {
    fn cell(&mut self, key: Cell) -> &mut CellData {
        if !self.cells.contains_key(&key) {
            self.order.push(key);
        }
        self.cells.entry(key).or_default()
    }

    /// Intern a street name, returning its index. The cell is created either way,
    /// as the TypeScript did.
    fn street(&mut self, key: Cell, name: &Value) -> Option<usize> {
        let cell = self.cell(key);
        let name = name.as_str().filter(|s| !s.is_empty())?;
        if let Some(&ix) = cell.street_ix.get(name) {
            return Some(ix);
        }
        cell.streets.push(name.to_string());
        cell.street_ix
            .insert(name.to_string(), cell.streets.len() - 1);
        Some(cell.streets.len() - 1)
    }
}

/// RD footprints in a 100 m grid, for "which building is this point in?". Holds
/// rings, not only bboxes, because a monument beside a long terrace would
/// otherwise attach to whichever bbox it clipped. Buckets and the building order
/// keep insertion order: ties are settled by it.
#[derive(Default)]
struct FootprintIndex {
    grid: HashMap<(i64, i64), Vec<usize>>,
    /// `(building index, bbox, rings)` in insertion order.
    rings: Vec<(usize, Bbox, Vec<Vec<Pt>>)>,
    slot: HashMap<usize, usize>,
}

const GRID_M: f64 = 100.0;

impl FootprintIndex {
    fn add(&mut self, building: usize, bb: Bbox, rings: Vec<Vec<Pt>>) {
        if rings.is_empty() {
            return;
        }
        self.slot.insert(building, self.rings.len());
        self.rings.push((building, bb, rings));
        let (gx0, gx1) = (
            (bb[0] / GRID_M).floor() as i64,
            (bb[2] / GRID_M).floor() as i64,
        );
        let (gy0, gy1) = (
            (bb[1] / GRID_M).floor() as i64,
            (bb[3] / GRID_M).floor() as i64,
        );
        for gx in gx0..=gx1 {
            for gy in gy0..=gy1 {
                self.grid.entry((gx, gy)).or_default().push(building);
            }
        }
    }

    /// The building containing `(x, y)`, else the nearest whose bbox is within
    /// `snap_m`. Containment always beats distance.
    fn at(&self, x: f64, y: f64, snap_m: f64) -> Option<usize> {
        let mut best = None;
        let mut best_d = snap_m * snap_m;
        let reach = (snap_m / GRID_M).floor() as i64 + 1;
        let (gx0, gy0) = ((x / GRID_M).floor() as i64, (y / GRID_M).floor() as i64);
        for gx in gx0 - reach..=gx0 + reach {
            for gy in gy0 - reach..=gy0 + reach {
                for &b in self.grid.get(&(gx, gy)).map(Vec::as_slice).unwrap_or(&[]) {
                    let (_, bb, rings) = &self.rings[self.slot[&b]];
                    if point_in_rings(x, y, rings) {
                        return Some(b);
                    }
                    let dx = (bb[0] - x).max(0.0).max(x - bb[2]);
                    let dy = (bb[1] - y).max(0.0).max(y - bb[3]);
                    let d = dx * dx + dy * dy;
                    if d < best_d {
                        best_d = d;
                        best = Some(b);
                    }
                }
            }
        }
        best
    }
}

/// A pand's outer ring, simplified, as open `[[lat, lng], …]`: the *largest* ring
/// of a multipolygon (the rest are courtyard slivers and porches), because the
/// client's other footprint for a 3DBAG building is the convex hull of a triangle
/// soup, which fills in the inner corner of every L-shaped building.
fn footprint_ring(geom: &Value) -> Option<Vec<[f64; 2]>> {
    let mut best: Option<Vec<Pt>> = None;
    let mut best_area = 0.0;
    for rings in polygon_rings(geom) {
        let Some(ring) = rings.first() else {
            continue;
        };
        let mut pts = ring.clone();
        if pts.len() > 1 && pts[0] == pts[pts.len() - 1] {
            pts.pop();
        }
        if pts.len() < 3 {
            continue;
        }
        let a = ring_area(&pts);
        if a > best_area {
            best = Some(pts);
            best_area = a;
        }
    }
    let best = best.filter(|_| best_area >= MIN_FOOTPRINT_M2)?;
    let kept = simplify(&best, FOOTPRINT_SIMPLIFY_M);
    if kept.len() < 3 {
        return None;
    }
    Some(
        kept.iter()
            .map(|&[x, y]| {
                let (lat, lng) = rd_to_lat_lng(x, y);
                [round(lat, 7), round(lng, 7)]
            })
            .collect(),
    )
}

struct Collected {
    buildings: Vec<Building>,
    by_pand: HashMap<String, usize>,
    index: FootprintIndex,
}

fn collect_buildings(ctx: &Ctx, cells: &mut Cells) -> Outcome<Collected> {
    let mut c = Collected {
        buildings: Vec::new(),
        by_pand: HashMap::new(),
        index: FootprintIndex::default(),
    };
    pages(ctx, "bag:pand", |feat| {
        let props = &feat["properties"];
        let Some(pand) = props["identificatie"].as_str().filter(|s| !s.is_empty()) else {
            return;
        };
        let Some(bb) = rd_bbox(feat) else {
            return;
        };
        if c.by_pand.contains_key(pand) {
            return;
        }
        let mut rec = Building {
            id: pand.to_string(),
            ..Default::default()
        };
        if truthy(&props["bouwjaar"]) {
            rec.year = js_trunc(&props["bouwjaar"]);
        }
        let mask = use_mask(&props["gebruiksdoel"]);
        if mask != 0 {
            rec.use_ = Some(mask);
        }
        // `_min`/`_max` bracket the units inside and are equal for most buildings,
        // so `area_max` is only worth the bytes when it says something new.
        if !props["oppervlakte_min"].is_null() {
            rec.area = js_trunc(&props["oppervlakte_min"]);
            if !props["oppervlakte_max"].is_null() {
                let max = js_trunc(&props["oppervlakte_max"]);
                if max != rec.area {
                    rec.area_max = max;
                }
            }
        }
        if truthy(&props["aantal_verblijfsobjecten"]) {
            rec.units = js_trunc(&props["aantal_verblijfsobjecten"]);
        }
        if let Some(status) = props["status"]
            .as_str()
            .filter(|s| !s.is_empty() && *s != STATUS_IN_USE)
        {
            rec.status = Some(status_code(status).unwrap_or(status).to_string());
        }
        rec.fp = footprint_ring(&feat["geometry"]);
        let ix = c.buildings.len();
        c.buildings.push(rec);
        for key in cells_of_bbox(bb) {
            cells.cell(key).buildings.push(ix);
        }
        c.by_pand.insert(pand.to_string(), ix);
        c.index.add(ix, bb, rings_of(&feat["geometry"]));
    })?;
    Ok(c)
}

fn collect_addresses(ctx: &Ctx, cells: &mut Cells, c: &mut Collected) -> Outcome<usize> {
    let mut n = 0;
    let mut seen = HashSet::new();
    pages(ctx, "bag:verblijfsobject", |feat| {
        let props = &feat["properties"];
        let Some(at) = point_of(feat) else {
            return;
        };
        let vbo = props["identificatie"].as_str().filter(|s| !s.is_empty());
        if let Some(v) = vbo {
            if !seen.insert(v.to_string()) {
                return;
            }
        }
        let key = lat_lng_to_cell(at[0], at[1]);
        let pand = props["pandidentificatie"]
            .as_str()
            .filter(|s| !s.is_empty());
        let street = cells.street(key, &props["openbare_ruimte"]);
        let mask = use_mask(&props["gebruiksdoel"]);
        let rec = Address {
            area: (!props["oppervlakte"].is_null())
                .then(|| js_trunc(&props["oppervlakte"]))
                .flatten(),
            at,
            id: pand.map(str::to_string),
            nr: house_number(props).filter(|s| !s.is_empty()),
            postcode: truthy(&props["postcode"]).then(|| js_string(&props["postcode"])),
            street,
            use_: (mask != 0).then_some(mask),
        };
        cells.cell(key).addresses.push(rec);
        // The unit's point is also its building's **door**: the mesh lane needs
        // only the entrances and must not pull two megabytes of addresses.
        if let Some(&b) = pand.and_then(|p| c.by_pand.get(p)) {
            c.buildings[b].doors.get_or_insert_with(Vec::new).push(at);
        }
        n += 1;
    })?;
    Ok(n)
}

fn collect_berths(ctx: &Ctx, cells: &mut Cells) -> Outcome<usize> {
    let mut n = 0;
    let mut seen = HashSet::new();
    for (layer, kind) in [("bag:ligplaats", "mooring"), ("bag:standplaats", "plot")] {
        pages(ctx, layer, |feat| {
            let props = &feat["properties"];
            let Some(at) = point_of(feat) else {
                return;
            };
            if let Some(oid) = props["identificatie"].as_str().filter(|s| !s.is_empty()) {
                if !seen.insert(oid.to_string()) {
                    return;
                }
            }
            let key = lat_lng_to_cell(at[0], at[1]);
            let street = cells.street(key, &props["openbare_ruimte"]);
            cells.cell(key).berths.push(Berth {
                at,
                kind,
                nr: house_number(props).filter(|s| !s.is_empty()),
                postcode: truthy(&props["postcode"]).then(|| js_string(&props["postcode"])),
                street,
            });
            n += 1;
        })?;
    }
    Ok(n)
}

/// Every feature of an RCE layer. Heritage data is a bonus, not a gate: a failure
/// is a map that does not know which houses are monuments, not a failed run.
fn rce_features(ctx: &Ctx, layer: &str, mut visit: impl FnMut(&Value)) -> Outcome<()> {
    let mut start = 0;
    loop {
        ctx.check()?;
        let page = match get_json_with(
            ctx,
            &feature_url(RCE_WFS, layer, ctx.bbox, start),
            layer,
            std::time::Duration::from_secs(120),
            4,
        ) {
            Ok(p) => p,
            Err(PipelineError::Cancelled) => return Err(PipelineError::Cancelled),
            Err(e) => {
                ctx.log(format!("{layer}: unavailable ({e}); skipping"));
                return Ok(());
            }
        };
        let feats = page["features"].as_array().cloned().unwrap_or_default();
        if feats.is_empty() {
            return Ok(());
        }
        feats.iter().for_each(&mut visit);
        start += feats.len();
        ctx.log(format!("{layer}: {start}"));
        if feats.len() < PAGE {
            return Ok(());
        }
    }
}

fn collect_monuments(ctx: &Ctx, c: &mut Collected) -> Outcome<usize> {
    let mut n = 0;
    rce_features(ctx, "rce:NationalListedMonumentPoints", |feat| {
        let props = &feat["properties"];
        let number = &props["rijksmonument_nummer"];
        let pts = geom_coords(&feat["geometry"]);
        if !truthy(number) || pts.is_empty() {
            return;
        }
        let Some(b) = c.index.at(pts[0][0], pts[0][1], MONUMENT_SNAP_M) else {
            return;
        };
        let rec = &mut c.buildings[b];
        rec.mon = js_trunc(number);
        if truthy(&props["hoofdcategorie"]) {
            rec.mcat = Some(js_string(&props["hoofdcategorie"]));
        }
        n += 1;
    })?;
    Ok(n)
}

/// The protected district each building stands in: few districts, many buildings,
/// so this walks the districts and asks each building's own centre. The first
/// district a building falls in keeps it.
fn collect_townscapes(ctx: &Ctx, c: &mut Collected, districts: &mut Vec<String>) -> Outcome<usize> {
    let mut n = 0;
    rce_features(ctx, "rce:Townscapes", |feat| {
        let props = &feat["properties"];
        let name = props["NAAM"]
            .as_str()
            .or_else(|| props["naam"].as_str())
            .filter(|s| !s.is_empty());
        let rings = rings_of(&feat["geometry"]);
        let Some(name) = name else {
            return;
        };
        if rings.is_empty() {
            return;
        }
        let ix = match districts.iter().position(|d| d == name) {
            Some(i) => i,
            None => {
                districts.push(name.to_string());
                districts.len() - 1
            }
        };
        for (b, bb, _) in &c.index.rings {
            let rec = &mut c.buildings[*b];
            if rec.district.is_some() {
                continue;
            }
            if point_in_rings((bb[0] + bb[2]) / 2.0, (bb[1] + bb[3]) / 2.0, &rings) {
                rec.district = Some(ix);
                n += 1;
            }
        }
    })?;
    Ok(n)
}

pub fn run(ctx: &Ctx) -> Outcome<DatasetResult> {
    let mut cells = Cells::default();
    ctx.log("panden…");
    let mut c = collect_buildings(ctx, &mut cells)?;
    let n_bld = c.buildings.len();
    ctx.log("verblijfsobjecten…");
    let n_adr = collect_addresses(ctx, &mut cells, &mut c)?;
    ctx.log("ligplaatsen + standplaatsen…");
    let n_bth = collect_berths(ctx, &mut cells)?;
    ctx.log("rijksmonumenten…");
    let n_mon = collect_monuments(ctx, &mut c)?;
    ctx.log("beschermde stads- en dorpsgezichten…");
    let mut districts = Vec::new();
    let n_dis = collect_townscapes(ctx, &mut c, &mut districts)?;

    let mut done = Vec::new();
    for key in &cells.order {
        let cell = &cells.cells[key];
        if cell.buildings.is_empty() && cell.addresses.is_empty() && cell.berths.is_empty() {
            continue;
        }
        done.push(*key);
        let (cx, cy) = *key;
        if !cell.buildings.is_empty() {
            // The district table goes whole into every cell: a handful of names,
            // and a cell that explains its own indices can be read on its own.
            let buildings: Vec<&Building> =
                cell.buildings.iter().map(|&i| &c.buildings[i]).collect();
            ctx.write_json(
                &cell_path("bag", *key),
                &json!({"districts": districts, "buildings": buildings}),
            )?;
        }
        if !cell.addresses.is_empty() || !cell.berths.is_empty() {
            ctx.write_json(
                &format!("bag/{CACHE_ZOOM}/{cx}/{cy}.places.json"),
                &json!({"streets": cell.streets, "addresses": cell.addresses, "berths": cell.berths}),
            )?;
        }
    }
    ctx.write_json(
        "bag/coverage.json",
        &json!({
            "name": ctx.name,
            "zoom": CACHE_ZOOM,
            "attribution": ATTRIBUTION,
            "source": "Kadaster BAG via PDOK WFS",
            "bbox_rd": ctx.bbox,
            "cells": sorted_cells(done.iter().copied()),
            "building_count": n_bld,
            "address_count": n_adr,
            "berth_count": n_bth,
            "monument_count": n_mon,
            "districts": districts,
        }),
    )?;
    ctx.log(format!(
        "done: {n_bld} buildings, {n_adr} addresses, {n_bth} berths, {n_mon} monuments, {n_dis} buildings in \
         {} protected districts, across {} cells",
        districts.len(),
        done.len()
    ));
    Ok(DatasetResult {
        cells: done.len(),
        counts: vec![
            Count::new("buildings", n_bld),
            Count::new("addresses", n_adr),
            Count::new("berths", n_bth),
            Count::new("monuments", n_mon),
            Count::new("districts", districts.len()),
        ],
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn uses_are_a_bitmask_and_unknown_words_say_nothing() {
        assert_eq!(use_mask(&json!("woonfunctie")), 1);
        assert_eq!(use_mask(&json!("winkelfunctie, woonfunctie")), 3);
        assert_eq!(use_mask(&json!("overige gebruiksfunctie")), 1 << 10);
        assert_eq!(use_mask(&json!("iets anders")), 0);
        assert_eq!(use_mask(&Value::Null), 0);
    }

    #[test]
    fn a_house_number_is_written_as_an_address_is() {
        assert_eq!(
            house_number(&json!({"huisnummer": 62, "huisletter": "A", "toevoeging": "bis"}))
                .as_deref(),
            Some("62Abis")
        );
        assert_eq!(
            house_number(&json!({"huisnummer": 7, "huisletter": null})).as_deref(),
            Some("7")
        );
        assert_eq!(house_number(&json!({"huisletter": "A"})), None);
    }

    #[test]
    fn javascript_coercions_hold() {
        assert_eq!(js_trunc(&json!("1985")), Some(1985));
        assert_eq!(js_trunc(&json!(42.9)), Some(42));
        assert_eq!(js_trunc(&json!("abc")), None);
        assert!(!truthy(&json!(0)) && !truthy(&json!("")) && truthy(&json!("0")));
    }

    #[test]
    fn containment_beats_distance_and_the_snap_is_bounded() {
        let mut idx = FootprintIndex::default();
        let square = |x: f64, y: f64| {
            vec![vec![
                [x, y],
                [x + 10.0, y],
                [x + 10.0, y + 10.0],
                [x, y + 10.0],
            ]]
        };
        idx.add(0, [0.0, 0.0, 10.0, 10.0], square(0.0, 0.0));
        idx.add(1, [11.0, 0.0, 21.0, 10.0], square(11.0, 0.0));
        assert_eq!(idx.at(15.0, 5.0, 12.0), Some(1));
        assert_eq!(idx.at(10.4, 5.0, 12.0), Some(0));
        assert_eq!(idx.at(60.0, 5.0, 12.0), None);
    }
}
