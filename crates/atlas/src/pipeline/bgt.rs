//! The **BGT** — the Netherlands' large-scale topography, ~20 cm — as the
//! surveyed street objects the client would otherwise have to invent
//! (`surveyed.rs` in the client reads them). Ported from the earlier
//! TypeScript pipeline (`bgt.ts`, itself from `tools/bgt_objects.py`).
//!
//! The objects come from **one PDOK extract** of the area (`bgt_extract.rs`), not
//! from the OGC feature API it was first written against — paging that API was
//! ~13 of the ~15 minutes a Groningen import took, and the extract is ~20 s. The
//! extract is handed on in that API's shape and names, so the code below is
//! unchanged by the switch, and the collection names it asks for are the API's.
//!
//! The register keeps **every historical version of every object**, all with an
//! ordinary `status: bestaand`. An object removed from reality has an end date
//! (`objectEindTijd`, the OGC API's `termination_date`); a version a later edit
//! replaced has only an end of registration (`eindRegistratie`). A feature is
//! current only when **both** are empty — the first filter alone kept seven
//! copies of one footway and covered 79 % of a Groningen cell's paving twice,
//! which the client draws as a z-fight down every street. The extract applies
//! the filter (`bgt_extract::is_historical`); `items` still counts duplicate ids.
//!
//! These objects are **added** to the generated layers, never substituted: the
//! BGT's tree points are optional content, and Groningen's own centre has none.
//!
//! Data: Kadaster BGT via PDOK — open data.

use std::collections::{HashMap, HashSet};

use cartopolis_geo::formats::bgt::{self, Line, Surface};
use cartopolis_geo::formats::coverage::Manifest;
use cartopolis_geo::formats::LatLng;
use serde_json::Value;

use super::bgt_extract::Extract;
use super::bv3d::separation_heights_for;
use super::geom::{lat_lng_rings, polygon_rings, round, simplify, Pt, M_PER_DEG};
use super::tile::{cell_path, lat_lng_to_cell, sorted_cells, Cell, CACHE_ZOOM};
use super::{Count, Ctx, DatasetResult, Outcome, Progress};

const ATTRIBUTION: &str = "Street objects © Kadaster BGT via PDOK (open data)";

/// Point forms in wire order, mirrored by `PROP_KINDS` in the client — **append,
/// never renumber**.
const PROP_KINDS: [&str; 12] = [
    "lamp",
    "bench",
    "shelter",
    "picnic_table",
    "artwork",
    "swing",
    "cabinet",
    // Claimed for `bord`, which is not read yet; held so the positions after it
    // match the client.
    "board",
    "bin",
    "recycling",
    "bike_rack",
    "toilet",
];
/// Linear forms in wire order, mirrored by `LINE_KINDS`.
const LINE_KINDS: [&str; 5] = ["fence", "wall", "hedge", "quay", "parapet"];
/// Bridge parts in wire order, mirrored by `BRIDGE_PARTS` in the client.
///
/// The BGT's `type_overbruggingsdeel` is `dek`, `pijler` (a pier) or `landhoofd`
/// (an abutment); a part it leaves untyped at level 1 is a deck.
const BRIDGE_PARTS: [&str; 3] = ["deck", "pier", "abutment"];

fn bridge_part(props: &Value) -> Option<&'static str> {
    match js_str(&props["type_overbruggingsdeel"]).as_str() {
        "dek" => Some("deck"),
        "pijler" => Some("pier"),
        "landhoofd" => Some("abutment"),
        "" if props["relatieve_hoogteligging"].as_i64() == Some(1) => Some("deck"),
        _ => None,
    }
}

/// Paved forms in wire order, mirrored by `SURFACE_KINDS`.
const SURFACE_KINDS: [&str; 13] = [
    "footway",
    "cycleway",
    "parking",
    "drive",
    "verge",
    "island",
    "paved",
    "steps",
    "street",
    "living_street",
    "road",
    "yard",
    // A private plot (`erf`). Its own form so `roofs` can split it along the
    // infrared photograph; a client that predates it drops the surface and
    // draws the ground under it.
    "garden",
];
/// Materials in wire order, mirrored by `SURFACE_MATERIALS` and `PavingMaterial`.
const SURFACE_MATERIALS: [&str; 12] = [
    "asphalt",
    "concrete",
    "brick",
    "concrete_paver",
    "tiles",
    "stone",
    "grass_paver",
    "grass",
    "planting",
    "gravel",
    "sand",
    "unpaved",
];

fn ix(table: &[&str], name: &str) -> usize {
    table.iter().position(|t| *t == name).unwrap_or(0)
}

/// `scheiding_*.type` → our linear form. The bank protections are in the table,
/// but gated on a *measured* height (`MEASURED_ONLY_TYPES`): drawn at a nominal
/// two metres they were canal-side masonry that is not there; drawn at their
/// measurement they are the quay walls they are.
fn line_form(t: &str) -> Option<&'static str> {
    Some(match t {
        "hek" | "draadraster" => "fence",
        "muur" | "geluidsscherm" => "wall",
        // Retaining walls: their top is the ground behind them and their
        // measured height is the face below it, down to the water. Drawn as a
        // `wall` they stood that height *above* the quay (the Westerhaven's
        // 243 m, 4.25 m `kademuur`, 2026-10-01). Appended to `LINE_KINDS`, so
        // a client that predates it drops the line rather than misdrawing it.
        "kademuur" | "damwand" | "walbescherming" => "quay",
        _ => return None,
    })
}

const MEASURED_ONLY_TYPES: [&str; 3] = ["kademuur", "damwand", "walbescherming"];

/// `wegdeel.functie` / `ondersteunendwegdeel.functie` → our paved form.
///
/// The carriageways were left out at first, on the ground that OpenStreetMap
/// draws them with lane markings, kerbs and a depth ladder the BGT cannot
/// supply. That is true of a through road and false of a residential street,
/// which has no markings, is often brick, and is a surveyed shape where OSM has
/// a nominal width — so the gaps between the two were plain fill, and a brick
/// street was drawn as tarmac (2026-10-01). Local streets and `woonerven` now
/// come from here; the client draws the residential classes from them and
/// keeps OSM's ribbons for the roads that carry markings. Regional roads are
/// here too, but only ever as the strip the ribbon does not cover.
fn surface_form(f: &str) -> Option<&'static str> {
    Some(match f {
        "voetpad" | "voetgangersgebied" | "ruiterpad" => "footway",
        "voetpad op trap" => "steps",
        "fietspad" => "cycleway",
        "parkeervlak" => "parking",
        "inrit" => "drive",
        // A transition has no function of its own — hence the neutral form.
        "transitie" => "paved",
        "berm" => "verge",
        "verkeerseiland" => "island",
        "rijbaan lokale weg" | "overweg" => "street",
        "woonerf" => "living_street",
        "rijbaan regionale weg" | "rijbaan autoweg" | "rijbaan autosnelweg" | "OV-baan" => "road",
        _ => return None,
    })
}

/// `onbegroeidterreindeel.fysiek_voorkomen` → a form, for the hard and the
/// bare ground: squares, forecourts and yards. Its `plus` field names the
/// material the same way a road's does.
fn yard_form(v: &str) -> Option<&'static str> {
    Some(match v {
        "gesloten verharding" | "open verharding" | "half verhard" | "onverhard" => "yard",
        "erf" => "garden",
        _ => return None,
    })
}

/// `plus_fysiek_voorkomen` → our material. Anything unlisted stays unknown rather
/// than being given a nearest-looking substance.
fn plus_material(v: &str) -> Option<&'static str> {
    Some(match v {
        "asfalt" => "asphalt",
        "cementbeton" | "beton element" => "concrete",
        "gebakken klinkers" => "brick",
        "betonstraatstenen" => "concrete_paver",
        "tegels" => "tiles",
        "sierbestrating" | "natuursteen" => "stone",
        "grasklinkers" => "grass_paver",
        "gras- en kruidachtigen" => "grass",
        "bosplantsoen" | "heesters" | "planten" | "boomschors" => "planting",
        "grind" | "split" | "schelpen" | "puin" => "gravel",
        "zand" => "sand",
        _ => return None,
    })
}

/// `fysiek_voorkomen` → material, where the fine field is silent.
///
/// `open`/`gesloten verharding` name a construction rather than a substance,
/// and were left unmapped as a guess. They are a better guess than the kind's
/// default, though, which is what an unmapped surface fell back to: the
/// Vismarkt is `voetpad` / `open verharding` with nothing finer, was drawn as
/// pale pavement tiles, and is grey pavers in the 2026 aerial photo. Open
/// paving is laid in elements with joints — in this country overwhelmingly
/// concrete pavers — and closed paving is poured: asphalt, mostly.
fn coarse_material(v: &str) -> Option<&'static str> {
    Some(match v {
        "groenvoorziening" => "grass",
        "onverhard" => "unpaved",
        "open verharding" => "concrete_paver",
        "gesloten verharding" => "asphalt",
        "half verhard" => "gravel",
        _ => return None,
    })
}

fn js_str(v: &Value) -> String {
    match v {
        Value::Null => String::new(),
        Value::String(s) => s.clone(),
        other => other.to_string(),
    }
}

/// The material index for one surface, or `None` where the survey is silent. The
/// fine field wins where it is filled.
#[must_use]
pub fn surface_material(props: &Value) -> Option<usize> {
    plus_material(&js_str(&props["plus_fysiek_voorkomen"]))
        .or_else(|| coarse_material(&js_str(&props["fysiek_voorkomen"])))
        .map(|name| ix(&SURFACE_MATERIALS, name))
}

/// Metres. **Measured**: a page of `ondersteunendwegdeel` is 77,202 vertices of
/// which 4,792 survive 10 cm — 94 % is collinear filler along straight kerbs.
const SIMPLIFY_M: f64 = 0.1;
/// Below this a polygon is an invisible sliver that still costs a triangle.
const MIN_AREA_M2: f64 = 1.0;

/// A GeoJSON polygon (`[lng, lat]` rings) as `[lat, lng]` rings simplified to
/// [`SIMPLIFY_M`], holes after the outer ring. Empty when the outer ring is a
/// sliver under [`MIN_AREA_M2`].
fn simplified_rings(poly: &[Vec<Pt>]) -> Vec<Vec<[f64; 2]>> {
    lat_lng_rings(poly, SIMPLIFY_M, MIN_AREA_M2)
}

/// `[lat, lng]` points as the file's pairs.
fn pairs(pts: &[Pt]) -> Vec<LatLng> {
    pts.iter().map(|p| (p[0], p[1])).collect()
}

/// `[lat, lng]` rings as the file's pairs.
fn ring_pairs(rings: &[Vec<[f64; 2]>]) -> Vec<Vec<LatLng>> {
    rings.iter().map(|r| pairs(r)).collect()
}

/// BGT `plus_type` → our point form. Unlisted objects are dropped rather than
/// given a nearest-looking shape.
fn prop_form(t: &str) -> Option<&'static str> {
    Some(match t {
        "lichtmast" | "lichtpunt" => "lamp",
        "bank" => "bench",
        "abri" => "shelter",
        "picknicktafel" => "picnic_table",
        "kunstobject" => "artwork",
        "speelvoorziening" => "swing",
        // A box on a plinth, which is what `FurnitureKind::Cabinet` draws.
        "reclamezuil" | "betaalautomaat" => "cabinet",
        // `straatmeubilair`.
        "fietsenrek" => "bike_rack",
        "openbaar toilet" => "toilet",
        // `bak`. A collection point for separated waste is the big containers,
        // which is the bottle bank's form, not the litter bin's.
        "afvalbak" => "bin",
        "afval apart plaats" | "container" => "recycling",
        _ => return None,
    })
}

fn truthy(v: &Value) -> bool {
    match v {
        Value::Null => false,
        Value::Bool(b) => *b,
        Value::Number(n) => n.as_f64().is_some_and(|f| f != 0.0 && !f.is_nan()),
        Value::String(s) => !s.is_empty(),
        _ => true,
    }
}

/// The object types the extract is asked for.
const FEATURETYPES: [&str; 9] = [
    "vegetatieobject",
    "paal",
    "straatmeubilair",
    "bak",
    "scheiding",
    "overbruggingsdeel",
    "wegdeel",
    "ondersteunendwegdeel",
    "onbegroeidterreindeel",
];

/// The OGC API collection names this module was written against, as an object
/// type of the extract and the geometry types that collection held: the API
/// split `vegetatieobject` and `scheiding` by geometry, the extract does not.
fn source(collection: &str) -> (&str, &'static [&'static str]) {
    const POINTS: &[&str] = &["Point", "MultiPoint"];
    const LINES: &[&str] = &["LineString", "MultiLineString"];
    const AREAS: &[&str] = &["Polygon", "MultiPolygon"];
    match collection {
        "vegetatieobject_punt" => ("vegetatieobject", POINTS),
        "vegetatieobject_vlak" => ("vegetatieobject", AREAS),
        "scheiding_lijn" => ("scheiding", LINES),
        "scheiding_vlak" => ("scheiding", AREAS),
        other => (other, &[]),
    }
}

/// Every **current** feature of a BGT collection over the area. A kept id seen
/// twice is a loud warning: it means the current-version filter no longer matches
/// the register's lifecycle model, which would otherwise surface months later as
/// a shimmering pavement.
fn items(
    ctx: &Ctx,
    extract: &Extract,
    collection: &str,
    mut visit: impl FnMut(&Value),
) -> Outcome<()> {
    let (featuretype, geometries) = source(collection);
    let mut kept = 0usize;
    let mut kept_ids: HashSet<String> = HashSet::new();
    let mut duplicates = 0usize;
    let mut first_duplicate: Option<String> = None;
    // The old versions are already gone (`bgt_extract::is_historical`).
    let result = extract.features(ctx, featuretype, |feat| {
        let kind = feat["geometry"]["type"].as_str().unwrap_or_default();
        if !geometries.is_empty() && !geometries.contains(&kind) {
            return;
        }
        let id = js_str(&feat["properties"]["lokaal_id"]);
        if !id.is_empty() && !kept_ids.insert(id.clone()) {
            duplicates += 1;
            first_duplicate.get_or_insert(id);
        }
        kept += 1;
        visit(feat);
    });
    ctx.log(format!("{collection}: {kept} current"));
    ctx.progress(Progress {
        phase: Some(collection.into()),
        done: Some(kept as u64),
        total: Some(None),
    });
    if duplicates > 0 {
        ctx.log(format!(
            "WARNING: {collection} kept {duplicates} features whose lokaal_id was already kept (e.g. {}) — the \
             current-version filter (bgt_extract::is_historical) no longer matches the register's lifecycle \
             model, and every duplicate is a coplanar z-fight on the client.",
            first_duplicate.unwrap_or_default()
        ));
    }
    result
}

/// A point feature's `[lat, lng]` — its first coordinate.
fn point_of(feat: &Value) -> Option<[f64; 2]> {
    let mut coords = &feat["geometry"]["coordinates"];
    while let Some(first) = coords
        .as_array()
        .and_then(|a| a.first())
        .filter(|f| f.is_array())
    {
        coords = first;
    }
    let a = coords.as_array().filter(|a| a.len() >= 2)?;
    Some([round(a[1].as_f64()?, 7), round(a[0].as_f64()?, 7)])
}

/// A (Multi)LineString's vertices as `[lat, lng]` per part.
fn line_points(geom: &Value) -> Vec<Vec<Pt>> {
    let parts: Vec<&Value> = match geom["type"].as_str() {
        Some("LineString") => vec![&geom["coordinates"]],
        Some("MultiLineString") => geom["coordinates"]
            .as_array()
            .map(|a| a.iter().collect())
            .unwrap_or_default(),
        _ => Vec::new(),
    };
    parts
        .into_iter()
        .filter_map(|part| {
            let pts: Vec<Pt> = part
                .as_array()?
                .iter()
                .filter_map(|p| {
                    let a = p.as_array().filter(|a| a.len() >= 2)?;
                    Some([round(a[1].as_f64()?, 7), round(a[0].as_f64()?, 7)])
                })
                .collect();
            (pts.len() >= 2).then_some(pts)
        })
        .collect()
}

/// A long thin polygon reduced to the line down its middle. Right for the strip
/// a hedge or a wall almost always is; for a rare blobby one it is a line
/// across it, which is a worse hedge, not a wrong place. Output `[lat, lng]`.
///
/// **The middle follows the strip round its bends.** It was the principal axis
/// through the centroid, cut at the extremes — one straight segment — and a
/// quay wall turning a corner was drawn as a wall across the corner: the
/// retaining wall under Gorechtkade by the Linnaeusplein (`kademuur`
/// `G0014.404f925a61cc0015e053660013ac6945`, 65 m round a right angle, 0.34 m
/// thick) stood as one diagonal slab across the walk below it (2026-11-05). Now
/// the outline is cut into its two long sides where they pair up best — the two
/// ends — and the line runs between them, simplified back to two points
/// wherever the strip is straight.
#[must_use]
pub fn centreline(geom: &Value) -> Vec<Vec<Pt>> {
    let rings: Vec<&Value> = match geom["type"].as_str() {
        Some("Polygon") => geom["coordinates"].get(0).into_iter().collect(),
        Some("MultiPolygon") => geom["coordinates"]
            .as_array()
            .map(|ps| ps.iter().filter_map(|p| p.get(0)).collect())
            .unwrap_or_default(),
        _ => Vec::new(),
    };
    let mut out = Vec::new();
    for ring in rings {
        let mut pts: Vec<Pt> = ring
            .as_array()
            .map(|r| {
                r.iter()
                    .filter_map(|p| {
                        let a = p.as_array().filter(|a| a.len() >= 2)?;
                        Some([a[0].as_f64()?, a[1].as_f64()?])
                    })
                    .collect()
            })
            .unwrap_or_default();
        if pts.len() > 1 && pts.first() == pts.last() {
            pts.pop();
        }
        if pts.len() < 3 {
            continue;
        }
        // Local metres about the first point, `[lng, lat]` → `[x, y]`.
        let [lng0, lat0] = pts[0];
        let scale = lat0.to_radians().cos() * M_PER_DEG;
        let m: Vec<Pt> = pts
            .iter()
            .map(|p| [(p[0] - lng0) * scale, (p[1] - lat0) * M_PER_DEG])
            .collect();
        let Some(line) = strip_middle(&m) else {
            continue;
        };
        out.push(
            line.iter()
                .map(|p| {
                    [
                        round(lat0 + p[1] / M_PER_DEG, 7),
                        round(lng0 + p[0] / scale, 7),
                    ]
                })
                .collect(),
        );
    }
    out
}

/// [`centreline`] for one open ring in metres: the middle line, or `None` for a
/// ring shorter than about a metre, which is not worth an object.
fn strip_middle(ring: &[Pt]) -> Option<Vec<Pt>> {
    let n = ring.len();
    let seg = |i: usize| {
        let (a, b) = (ring[i], ring[(i + 1) % n]);
        (b[0] - a[0]).hypot(b[1] - a[1])
    };
    let perimeter: f64 = (0..n).map(seg).sum();
    if perimeter < 2.0 {
        return None;
    }
    // The outline at even spacing, about a metre apart.
    let k = ((perimeter / 1.0).ceil() as usize).clamp(12, 64);
    let even = resample_closed(ring, k);
    // The chain from `i` to `j` and the one back, each at `m` points from `i`.
    let chains = |i: usize, j: usize, m: usize| -> (Vec<Pt>, Vec<Pt>) {
        let a: Vec<Pt> = (i..=j).map(|t| even[t % k]).collect();
        let b: Vec<Pt> = (j..=i + k).rev().map(|t| even[t % k]).collect();
        (resample_open(&a, m), resample_open(&b, m))
    };
    let mut best = (f64::MAX, 0usize, 0usize);
    for i in 0..k {
        for j in i + 2..i + k - 1 {
            let (a, b) = chains(i, j, 16);
            let cost: f64 = a
                .iter()
                .zip(&b)
                .map(|(p, q)| (p[0] - q[0]).hypot(p[1] - q[1]))
                .sum();
            if cost < best.0 {
                best = (cost, i, j);
            }
        }
    }
    let (a, b) = chains(best.1, best.2, k);
    // The two ends are where the outline was cut, which can be a corner of an
    // end rather than its middle: the line runs between the sides' midpoints,
    // and is carried straight on out to as far as each cut reaches.
    let mid: Vec<Pt> = a
        .iter()
        .zip(&b)
        .map(|(p, q)| [(p[0] + q[0]) / 2.0, (p[1] + q[1]) / 2.0])
        .collect();
    // Within about a width of a cut the two sides are still turning round the
    // end, and their midpoints hook towards the corner; those are dropped.
    let width = a
        .iter()
        .zip(&b)
        .map(|(p, q)| (p[0] - q[0]).hypot(p[1] - q[1]))
        .sum::<f64>()
        / k as f64;
    let spacing = resample_length(&a) / (k - 1) as f64;
    let d = ((width / spacing.max(1e-9)).ceil() as usize + 1).min((mid.len() - 2) / 2);
    let mut inner = mid[d..mid.len() - d].to_vec();
    let carry = |cut: Pt, end: Pt, next: Pt| -> Pt {
        let (dx, dy) = (end[0] - next[0], end[1] - next[1]);
        let len = dx.hypot(dy);
        if len < 1e-9 {
            return end;
        }
        let (ux, uy) = (dx / len, dy / len);
        let reach = ((cut[0] - end[0]) * ux + (cut[1] - end[1]) * uy).max(0.0);
        [end[0] + ux * reach, end[1] + uy * reach]
    };
    let first = carry(even[best.1 % k], inner[0], inner[1]);
    let last = carry(
        even[best.2 % k],
        inner[inner.len() - 1],
        inner[inner.len() - 2],
    );
    inner.insert(0, first);
    inner.push(last);
    let line = simplify(&inner, STRIP_SIMPLIFY_M);
    let length: f64 = line
        .windows(2)
        .map(|w| (w[1][0] - w[0][0]).hypot(w[1][1] - w[0][1]))
        .sum();
    (length >= 1.0).then_some(line)
}

/// How far the middle line may stray from the strip's when it is simplified,
/// metres: a straight wall comes back as its two ends.
const STRIP_SIMPLIFY_M: f64 = 0.1;

/// The length of an open polyline.
fn resample_length(line: &[Pt]) -> f64 {
    line.windows(2)
        .map(|w| (w[1][0] - w[0][0]).hypot(w[1][1] - w[0][1]))
        .sum()
}

/// `m ≥ 2` points spaced evenly along an open polyline, both ends included.
fn resample_open(line: &[Pt], m: usize) -> Vec<Pt> {
    let lengths: Vec<f64> = line
        .windows(2)
        .map(|w| (w[1][0] - w[0][0]).hypot(w[1][1] - w[0][1]))
        .collect();
    let total: f64 = lengths.iter().sum();
    if total <= 0.0 || line.len() < 2 {
        return vec![line[0]; m];
    }
    let (mut out, mut seg, mut walked) = (Vec::with_capacity(m), 0usize, 0.0f64);
    for s in 0..m {
        let want = total * s as f64 / (m - 1) as f64;
        while seg + 1 < lengths.len() && walked + lengths[seg] < want {
            walked += lengths[seg];
            seg += 1;
        }
        let t = if lengths[seg] > 0.0 {
            ((want - walked) / lengths[seg]).clamp(0.0, 1.0)
        } else {
            0.0
        };
        let (a, b) = (line[seg], line[seg + 1]);
        out.push([a[0] + (b[0] - a[0]) * t, a[1] + (b[1] - a[1]) * t]);
    }
    out
}

/// `k` points spaced evenly round a closed ring.
fn resample_closed(ring: &[Pt], k: usize) -> Vec<Pt> {
    let mut closed = ring.to_vec();
    closed.push(ring[0]);
    let mut even = resample_open(&closed, k + 1);
    even.pop();
    even
}

/// Finest raster a walk is measured on, samples a side: 512 m at AHN's own half
/// metre, and coarser for a longer one rather than not at all.
const QUAY_RASTER_MAX: usize = 1024;

/// A paved surface (`[lng, lat]` rings, as the extract has them) measured
/// against AHN (`lower_quay`): the level of the walk, metres above NAP, where it
/// is a lower quay. `Ok(None)` where it is not; `Err` when AHN did not answer,
/// which leaves the surface drawn on the field as it always was.
fn lower_quay_of(
    ahn: &dyn crate::terrain::HeightSource,
    rings: &[Vec<[f64; 2]>],
) -> Result<Option<super::lower_quay::LowerQuay>, String> {
    // The rings as shipped, `[lat, lng]`, so the edges `measure` classifies
    // are the ones the client reads.
    let rd: Vec<Vec<Pt>> = rings
        .iter()
        .map(|r| {
            r.iter()
                .map(|p| cartopolis_geo::ahn::wgs84_to_rd(p[0], p[1]))
                .collect()
        })
        .collect();
    if !super::lower_quay::wide_enough(&rd) {
        return Ok(None);
    }
    let (min, max) = super::lower_quay::window(&rd);
    Ok(super::lower_quay::measure(
        &rd,
        &raster_of(ahn, crate::terrain::Model::Terrain, min, max)?,
    ))
}

/// AHN over a square at its own half metre, or coarser past [`QUAY_RASTER_MAX`].
fn raster_of(
    ahn: &dyn crate::terrain::HeightSource,
    model: crate::terrain::Model,
    min: Pt,
    max: Pt,
) -> Result<super::lower_quay::Raster, String> {
    let n = (((max[0] - min[0]) / 0.5).ceil() as usize).clamp(8, QUAY_RASTER_MAX);
    let h = ahn.raster_of(model, min, max, n)?;
    Ok(super::lower_quay::Raster {
        min,
        cell: (max[0] - min[0]) / n as f64,
        n,
        h,
    })
}

/// How far a quay wall stands above the street it holds up, metres, where
/// AHN's surface model sees it standing [`super::lower_quay::MIN_PARAPET_M`] or
/// more (`lower_quay::parapet`). `line` is its centreline, `[lat, lng]`.
fn parapet_of(
    ahn: &dyn crate::terrain::HeightSource,
    line: &[Pt],
    street: f64,
) -> Result<Option<f64>, String> {
    let rd: Vec<Pt> = line
        .iter()
        .map(|p| cartopolis_geo::ahn::wgs84_to_rd(p[0], p[1]))
        .collect();
    let (min, max) = super::lower_quay::window(std::slice::from_ref(&rd));
    let dsm = raster_of(ahn, crate::terrain::Model::Surface, min, max)?;
    Ok(super::lower_quay::parapet(&rd, &dsm, street).map(|h| round(h, 2)))
}

#[derive(Default)]
struct CellData {
    trees: Vec<LatLng>,
    props: Vec<bgt::Prop>,
    lines: Vec<Line>,
    surfaces: Vec<Surface>,
    /// `[part, level, movable, [ring, …]]`, rings `[lat, lng]` — see
    /// [`BRIDGE_PARTS`].
    bridges: Vec<bgt::BridgePart>,
    /// The lower quays, for the paving file (`lower_quay`).
    terraces: Vec<bgt::Terrace>,
}

pub fn run(ctx: &Ctx) -> Outcome<DatasetResult> {
    // The wall heights come from the 3D basisvoorziening and owe the extract
    // nothing, so they are measured while PDOK builds it and it downloads —
    // ~20 s for Groningen that the run otherwise spent waiting (2026-09-24).
    // Never fatal: with no measurements every wall falls back to its nominal form.
    let (extract, heights) = std::thread::scope(|s| {
        let heights = s.spawn(|| separation_heights_for(ctx));
        ctx.log("extract aanvragen…");
        let extract = Extract::fetch(ctx, &FEATURETYPES);
        let heights = heights
            .join()
            .unwrap_or_else(|panic| std::panic::resume_unwind(panic));
        (extract, heights)
    });
    let extract = &extract?;
    let heights = heights?;
    let mut cells: HashMap<Cell, CellData> = HashMap::new();
    let cell_of = |at: Pt| -> Cell { lat_lng_to_cell(at[0], at[1]) };

    ctx.log("bomen…");
    let mut n_trees = 0;
    items(ctx, extract, "vegetatieobject_punt", |feat| {
        if feat["properties"]["plus_type"] != "boom" {
            return;
        }
        if let Some(at) = point_of(feat) {
            cells
                .entry(cell_of(at))
                .or_default()
                .trees
                .push((at[0], at[1]));
            n_trees += 1;
        }
    })?;

    let mut n_props = 0;
    for collection in ["paal", "straatmeubilair", "bak"] {
        ctx.log(format!("{collection}…"));
        items(ctx, extract, collection, |feat| {
            let Some(form) = prop_form(&js_str(&feat["properties"]["plus_type"])) else {
                return;
            };
            if let Some(at) = point_of(feat) {
                cells.entry(cell_of(at)).or_default().props.push((
                    at[0],
                    at[1],
                    ix(&PROP_KINDS, form),
                ));
                n_props += 1;
            }
        })?;
    }

    // A measured height rides as a **trailing** third element, like the paving's
    // material: unknown costs no bytes, and every cached cell keeps parsing.
    let (mut n_lines, mut n_measured) = (0usize, 0usize);
    let mut push_line =
        |cells: &mut HashMap<Cell, CellData>, form: &str, t: &str, id: &str, pts: &[Pt]| {
            let height = heights.get(id);
            if height.is_none() && MEASURED_ONLY_TYPES.contains(&t) {
                return;
            }
            let kind = ix(&LINE_KINDS, form);
            let line = Line(kind, pairs(pts), height.map(|h| round(*h, 2)));
            cells.entry(cell_of(pts[0])).or_default().lines.push(line);
            n_lines += 1;
            n_measured += usize::from(height.is_some());
        };

    ctx.log("hekken, muren en hagen…");
    items(ctx, extract, "scheiding_lijn", |feat| {
        let t = js_str(&feat["properties"]["type"]);
        let Some(form) = line_form(&t) else {
            return;
        };
        let id = js_str(&feat["properties"]["lokaal_id"]);
        for pts in line_points(&feat["geometry"]) {
            push_line(&mut cells, form, &t, &id, &pts);
        }
    })?;
    // The same register's boundaries surveyed as *areas* — the collection the 3D
    // product covers, so where a measured wall comes from, reduced to its
    // centreline for the reason a hedge is.
    //
    // A quay's outline is kept, in RD metres, for the paving below: a walk
    // that runs along one may lie below the street it holds up (`lower_quay`).
    ctx.log("muren als vlak…");
    let mut quays: Vec<(Vec<Pt>, Vec<Vec<Pt>>)> = Vec::new();
    items(ctx, extract, "scheiding_vlak", |feat| {
        let t = js_str(&feat["properties"]["type"]);
        let Some(form) = line_form(&t) else {
            return;
        };
        let id = js_str(&feat["properties"]["lokaal_id"]);
        let lines = centreline(&feat["geometry"]);
        if form == "quay" {
            for poly in polygon_rings(&feat["geometry"]) {
                if let Some(outer) = poly.first() {
                    quays.push((
                        outer
                            .iter()
                            .map(|p| cartopolis_geo::ahn::wgs84_to_rd(p[1], p[0]))
                            .collect(),
                        lines.clone(),
                    ));
                }
            }
        }
        for pts in lines {
            push_line(&mut cells, form, &t, &id, &pts);
        }
    })?;
    items(ctx, extract, "vegetatieobject_vlak", |feat| {
        if feat["properties"]["plus_type"] != "haag" {
            return;
        }
        for pts in centreline(&feat["geometry"]) {
            cells.entry(cell_of(pts[0])).or_default().lines.push(Line(
                ix(&LINE_KINDS, "hedge"),
                pairs(&pts),
                None,
            ));
            n_lines += 1;
        }
    })?;
    ctx.log(format!(
        "{n_lines} scheidingen, waarvan {n_measured} met gemeten hoogte"
    ));

    // The parts a bridge is surveyed as: its decks, and — what nothing else in
    // the map carries — the piers under them and the abutments at their ends.
    // The client stands a pier from the ground to the deck it holds up.
    ctx.log("brugdelen…");
    let mut n_bridges = 0usize;
    items(ctx, extract, "overbruggingsdeel", |feat| {
        let props = &feat["properties"];
        let Some(part) = bridge_part(props) else {
            return;
        };
        // The BGT's levels run −6…5, so an `i8` holds every one; anything else
        // is not a level and reads as the ground.
        let level = props["relatieve_hoogteligging"]
            .as_i64()
            .and_then(|l| i8::try_from(l).ok())
            .unwrap_or(0);
        let movable = js_str(&props["overbrugging_is_beweegbaar"]) == "true";
        for poly in polygon_rings(&feat["geometry"]) {
            let rings = simplified_rings(&poly);
            let Some(first) = rings.first().and_then(|r| r.first()).copied() else {
                continue;
            };
            cells
                .entry(lat_lng_to_cell(first[0], first[1]))
                .or_default()
                .bridges
                .push((ix(&BRIDGE_PARTS, part), level, movable, ring_pairs(&rings)));
            n_bridges += 1;
        }
    })?;

    let quays = super::lower_quay::QuayIndex::new(quays);
    let (n_surf, n_terraces) = collect_surfaces(ctx, extract, &mut cells, &quays)?;

    let written: Vec<(Cell, CellData)> = cells
        .into_iter()
        .filter(|(_, c)| {
            !c.trees.is_empty()
                || !c.props.is_empty()
                || !c.lines.is_empty()
                || !c.surfaces.is_empty()
                || !c.terraces.is_empty()
                || !c.bridges.is_empty()
        })
        .collect();
    let covered = sorted_cells(written.iter().map(|(c, _)| *c));
    let n_written = written.len();
    for (cell, data) in written {
        let (cx, cy) = cell;
        let CellData {
            trees,
            props,
            lines,
            surfaces,
            bridges,
            terraces,
        } = data;
        // Two files, as the BAG splits its own: the paving is the heavy part.
        if !trees.is_empty() || !props.is_empty() || !lines.is_empty() || !bridges.is_empty() {
            ctx.write_doc(
                &cell_path("bgt", cell),
                &bgt::Objects {
                    bridges,
                    lines,
                    props,
                    trees,
                },
            )?;
        }
        if !surfaces.is_empty() || !terraces.is_empty() {
            ctx.write_doc(
                &format!("bgt/{CACHE_ZOOM}/{cx}/{cy}.surfaces.json"),
                &bgt::Paving { surfaces, terraces },
            )?;
        }
    }
    ctx.write_coverage(
        "bgt/coverage.json",
        &Manifest::new(CACHE_ZOOM, covered)
            .described(
                ctx.name.as_str(),
                ATTRIBUTION,
                "Kadaster BGT via PDOK download API",
                ctx.bbox,
            )
            .with("kinds", &PROP_KINDS[..])
            .with("line_kinds", &LINE_KINDS[..])
            .with("surface_kinds", &SURFACE_KINDS[..])
            .with("surface_materials", &SURFACE_MATERIALS[..])
            .with("bridge_parts", &BRIDGE_PARTS[..])
            .with("tree_count", n_trees)
            .with("prop_count", n_props)
            .with("line_count", n_lines)
            // How much of the boundary layer carries a real measurement, so "the
            // walls look wrong" has a number attached.
            .with("measured_line_count", n_measured)
            .with("surface_count", n_surf)
            .with("terrace_count", n_terraces)
            .with("bridge_count", n_bridges),
    )?;
    ctx.log(format!(
        "done: {n_trees} surveyed trees, {n_props} street objects, {n_lines} fences, walls and hedges, {n_surf} \
         paved surfaces across {} cells",
        n_written
    ));
    Ok(DatasetResult {
        cells: n_written,
        counts: vec![
            Count::new("trees", n_trees),
            Count::new("props", n_props),
            Count::new("lines", n_lines),
            Count::new("surfaces", n_surf),
            Count::new("bridges", n_bridges),
        ],
    })
}

/// `wegdeel` + `ondersteunendwegdeel` + `onbegroeidterreindeel` → the paving:
/// pavements, cycle paths, the local streets themselves, and the squares.
fn collect_surfaces(
    ctx: &Ctx,
    extract: &Extract,
    cells: &mut HashMap<Cell, CellData>,
    quays: &super::lower_quay::QuayIndex,
) -> Outcome<(usize, usize)> {
    let (mut n, mut dropped_level, mut n_material) = (0usize, 0usize, 0usize);
    // A walk along a quay wall is measured against AHN, and shipped with its
    // level where it lies below the street the wall holds up (`lower_quay`).
    let ahn = ctx
        .data_dir()
        .ok()
        .map(|dir| crate::ahn6::Ahn6::new(dir, std::sync::Arc::new(crate::terrain::Pdok::new())));
    let (mut n_terraces, mut n_measured, mut n_parapets, mut ahn_errors) =
        (0usize, 0usize, 0usize, 0usize);
    let mut parapeted: HashSet<usize> = HashSet::new();
    for collection in ["wegdeel", "ondersteunendwegdeel", "onbegroeidterreindeel"] {
        ctx.log(format!("{collection}…"));
        items(ctx, extract, collection, |feat| {
            let props = &feat["properties"];
            // Bare ground has no `functie`: what it is, is what it is paved with.
            let form = if collection == "onbegroeidterreindeel" {
                yard_form(&js_str(&props["fysiek_voorkomen"]))
            } else {
                surface_form(&js_str(&props["functie"]))
            };
            let Some(form) = form else {
                return;
            };
            // Ground level only: paving on a bridge or in a tunnel would otherwise
            // run a pavement through whatever is underneath.
            if truthy(&props["relatieve_hoogteligging"]) {
                dropped_level += 1;
                return;
            }
            for poly in polygon_rings(&feat["geometry"]) {
                let out_rings = simplified_rings(&poly);
                let Some(first) = out_rings.first().and_then(|r| r.first()).copied() else {
                    continue;
                };
                // Appended, never inserted, so the two-element form still reads.
                let material = surface_material(props);
                let kind = ix(&SURFACE_KINDS, form);
                let rings = ring_pairs(&out_rings);
                let measured = match (&ahn, poly.first()) {
                    (Some(ahn), Some(outer)) => {
                        let along = quays.along_which(
                            &outer
                                .iter()
                                .map(|p| cartopolis_geo::ahn::wgs84_to_rd(p[1], p[0]))
                                .collect::<Vec<_>>(),
                        );
                        if along.is_empty() {
                            None
                        } else {
                            n_measured += 1;
                            lower_quay_of(ahn, &out_rings)
                                .unwrap_or_else(|_| {
                                    ahn_errors += 1;
                                    None
                                })
                                .map(|q| (ahn, q, along))
                        }
                    }
                    _ => None,
                };
                let cell = lat_lng_to_cell(first[0], first[1]);
                if let Some((ahn, q, along)) = measured {
                    cells.entry(cell).or_default().terraces.push(bgt::Terrace(
                        rings.clone(),
                        round(q.level, 2),
                        q.seaward,
                    ));
                    n_terraces += 1;
                    // The walls it runs along, where they stand above the
                    // street they hold up: each once, whichever walk finds it.
                    for k in along {
                        if !parapeted.insert(k) {
                            continue;
                        }
                        for line in quays.lines(k) {
                            let Some(&at) = line.first() else { continue };
                            match parapet_of(ahn, line, q.street) {
                                Ok(Some(h)) => {
                                    cells
                                        .entry(lat_lng_to_cell(at[0], at[1]))
                                        .or_default()
                                        .lines
                                        .push(Line(
                                            ix(&LINE_KINDS, "parapet"),
                                            pairs(line),
                                            Some(h),
                                        ));
                                    n_parapets += 1;
                                }
                                Ok(None) => {}
                                Err(_) => ahn_errors += 1,
                            }
                        }
                    }
                }
                cells
                    .entry(cell)
                    .or_default()
                    .surfaces
                    .push(Surface(kind, rings, material));
                n_material += usize::from(material.is_some());
                n += 1;
            }
        })?;
    }
    if dropped_level > 0 {
        ctx.log(format!("{dropped_level} surfaces above or below ground level were dropped (bridges and tunnels)"));
    }
    if n > 0 {
        ctx.log(format!(
            "{n_material} of {n} surfaces carry a material ({}%)",
            100 * n_material / n
        ));
    }
    ctx.log(format!(
        "{n_measured} surfaces along a quay wall measured, {n_terraces} lie below the street, \
         {n_parapets} walls stand above it"
    ));
    if ahn_errors > 0 {
        ctx.log(format!(
            "{ahn_errors} of them unmeasured: AHN did not answer"
        ));
    }
    Ok((n, n_terraces))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    /// Every point form lands on the position the client reads it at. `ix`
    /// answers 0 for a name it does not find, so a form missing from the table
    /// would be drawn as a lamppost rather than refused.
    #[test]
    fn every_point_form_has_its_wire_position() {
        for (t, at) in [
            ("lichtmast", 0),
            ("bank", 1),
            ("reclamezuil", 6),
            ("afvalbak", 8),
            ("afval apart plaats", 9),
            ("container", 9),
            ("fietsenrek", 10),
            ("openbaar toilet", 11),
        ] {
            let form = prop_form(t).expect(t);
            assert!(
                PROP_KINDS.contains(&form),
                "{t} → {form} is not in PROP_KINDS"
            );
            assert_eq!(ix(&PROP_KINDS, form), at, "{t}");
        }
    }

    /// Retaining walls are their own form, at the end of the table, so a wall
    /// that holds a quay up is not drawn standing on it.
    #[test]
    fn quay_walls_are_their_own_form() {
        for t in ["kademuur", "damwand", "walbescherming"] {
            assert_eq!(line_form(t), Some("quay"), "{t}");
        }
        assert_eq!(line_form("muur"), Some("wall"));
        assert_eq!(ix(&LINE_KINDS, "quay"), 3, "appended, never renumbered");
    }

    /// The register's part types, as the API answered them over the Emmaviaduct
    /// (2026-09-24): a deck, a pier, an abutment, an untyped part over something,
    /// and an untyped part on the ground — which is not guessed at.
    #[test]
    fn bridge_parts_are_named_as_the_register_types_them() {
        let part = |t: Value, level: i64| {
            bridge_part(&json!({"type_overbruggingsdeel": t, "relatieve_hoogteligging": level}))
        };
        assert_eq!(part(json!("dek"), 1), Some("deck"));
        assert_eq!(part(json!("pijler"), 0), Some("pier"));
        assert_eq!(part(json!("landhoofd"), 0), Some("abutment"));
        assert_eq!(part(Value::Null, 1), Some("deck"));
        assert_eq!(part(Value::Null, 0), None);
        assert_eq!(part(json!("brugleuning"), 1), None);
        for p in ["deck", "pier", "abutment"] {
            assert!(BRIDGE_PARTS.contains(&p));
        }
    }

    /// A polygon comes back as `[lat, lng]` rings, its closing vertex dropped, and
    /// a sliver not at all.
    #[test]
    fn a_polygon_is_simplified_into_lat_lng_rings() {
        let square = |d: f64| {
            vec![vec![
                [6.56, 53.21],
                [6.56 + d, 53.21],
                [6.56 + d, 53.21 + d],
                [6.56, 53.21 + d],
                [6.56, 53.21],
            ]]
        };
        let rings = simplified_rings(&square(0.0002));
        assert_eq!(rings.len(), 1);
        assert_eq!(rings[0].len(), 4);
        assert_eq!(rings[0][0], [53.21, 6.56]);
        assert!(simplified_rings(&square(0.000_001)).is_empty());
    }

    /// The fine field wins; where it is silent the coarse one decides, open
    /// paving as pavers and closed as asphalt (see `coarse_material` for why
    /// that guess was taken after all).
    #[test]
    fn the_fine_material_wins_and_the_coarse_one_fills_in() {
        assert_eq!(
            surface_material(&json!({"plus_fysiek_voorkomen": "tegels"})),
            Some(4)
        );
        assert_eq!(
            surface_material(
                &json!({"plus_fysiek_voorkomen": "", "fysiek_voorkomen": "onverhard"})
            ),
            Some(11)
        );
        assert_eq!(
            surface_material(&json!({"fysiek_voorkomen": "gesloten verharding"})),
            Some(0)
        );
        assert_eq!(
            surface_material(&json!({"fysiek_voorkomen": "open verharding"})),
            Some(3)
        );
        assert_eq!(
            surface_material(
                &json!({"plus_fysiek_voorkomen": "gebakken klinkers", "fysiek_voorkomen": "open verharding"})
            ),
            Some(2)
        );
    }

    /// A long thin strip becomes one segment along its length.
    #[test]
    fn a_hedge_strip_becomes_its_long_axis() {
        let strip = json!({"type": "Polygon", "coordinates": [[[6.0, 53.0], [6.001, 53.0], [6.001, 53.00001], [6.0, 53.00001], [6.0, 53.0]]]});
        let lines = centreline(&strip);
        assert_eq!(lines.len(), 1);
        let [a, b] = [lines[0][0], lines[0][1]];
        assert!(((b[1] - a[1]).abs() - 0.001).abs() < 1e-4, "{a:?} {b:?}");
        assert!((b[0] - a[0]).abs() < 1e-5, "{a:?} {b:?}");
    }

    #[test]
    fn a_point_is_the_first_coordinate_at_any_depth() {
        assert_eq!(
            point_of(&json!({"geometry": {"coordinates": [[6.5, 53.2]]}})),
            Some([53.2, 6.5])
        );
        assert_eq!(
            point_of(&json!({"geometry": {"coordinates": [6.5, 53.2]}})),
            Some([53.2, 6.5])
        );
    }

    /// The walk below Gorechtkade by the Linnaeusplein, as the register and AHN
    /// have it: a footway along a quay wall, a metre and a half under the street
    /// that wall holds up, and the wall itself drawn round its corner rather than
    /// across it. Prints what the files carry for both.
    /// `cargo test -p cartopolis_atlas gorechtkade -- --ignored --nocapture`.
    #[test]
    #[ignore = "fetches the BGT and AHN"]
    fn the_gorechtkade_walk_is_a_lower_quay() {
        let fetch = |collection: &str| -> Vec<Value> {
            let url = format!(
                "https://api.pdok.nl/lv/bgt/ogc/v1/collections/{collection}/items\
                 ?bbox=6.5813,53.2221,6.5828,53.2230&limit=200&f=json"
            );
            let body = super::super::http::fetch(&url, std::time::Duration::from_secs(60)).unwrap();
            let doc: Value = serde_json::from_slice(&body).unwrap();
            doc["features"]
                .as_array()
                .unwrap()
                .iter()
                .filter(|f| f["properties"]["eind_registratie"].is_null())
                .cloned()
                .collect()
        };
        let by_id = |feats: &[Value], id: &str| -> Value {
            feats
                .iter()
                .find(|f| f["properties"]["lokaal_id"] == id)
                .unwrap_or_else(|| panic!("{id}"))
                .clone()
        };
        let rd = |ring: &[Pt]| -> Vec<Pt> {
            ring.iter()
                .map(|p| cartopolis_geo::ahn::wgs84_to_rd(p[1], p[0]))
                .collect()
        };
        let walls = fetch("scheiding_vlak");
        let quays = super::super::lower_quay::QuayIndex::new(
            walls
                .iter()
                .filter(|f| f["properties"]["type"] == "kademuur")
                .flat_map(|f| {
                    let lines = centreline(&f["geometry"]);
                    polygon_rings(&f["geometry"])
                        .into_iter()
                        .map(move |p| (rd(&p[0]), lines.clone()))
                })
                .collect(),
        );
        let walk = by_id(&fetch("wegdeel"), "G0014.404f925c4b580015e053660013ac6945");
        let poly = &polygon_rings(&walk["geometry"])[0];
        let along = quays.along_which(&rd(&poly[0]));
        assert!(!along.is_empty(), "the walk runs along a quay wall");
        let dir = tempfile::tempdir().unwrap();
        let ahn =
            crate::ahn6::Ahn6::new(dir.path(), std::sync::Arc::new(crate::terrain::Pdok::new()));
        let rings = simplified_rings(poly);
        let q = lower_quay_of(&ahn, &rings).unwrap().expect("a lower quay");
        let level = q.level;
        let parapets: Vec<(Vec<LatLng>, f64)> = along
            .iter()
            .flat_map(|&k| quays.lines(k).to_vec())
            .filter_map(|l| {
                parapet_of(&ahn, &l, q.street)
                    .unwrap()
                    .map(|h| (pairs(&l), h))
            })
            .collect();
        let wall = by_id(&walls, "G0014.404f925a61cc0015e053660013ac6945");
        let line = &centreline(&wall["geometry"])[0];
        println!(
            "{}",
            json!({
                "terrace": bgt::Terrace(ring_pairs(&rings), round(level, 2), q.seaward.clone()),
                "parapets": parapets,
                "walls": walls
                    .iter()
                    .filter(|f| f["properties"]["type"] == "kademuur")
                    .map(|f| (
                        js_str(&f["properties"]["lokaal_id"]),
                        centreline(&f["geometry"]).iter().map(|l| pairs(l)).collect::<Vec<_>>(),
                    ))
                    .collect::<HashMap<_, _>>(),
            })
        );
        assert!((-1.1..-0.5).contains(&level), "the walk at {level} m NAP");
        assert!(q.seaward.iter().any(|s| *s), "an edge faces the water");
        // The wall under the street stands above it (0.38 m, 2026-11-05), and so
        // does its next length by the steps; the one on the water does not.
        assert!(
            parapets
                .iter()
                .any(|(l, h)| l[0] == pairs(line)[0] && (0.3..1.5).contains(h)),
            "{parapets:?}"
        );
        assert_eq!(parapets.len(), 2, "{parapets:?}");
        // Round the corner: every point of the line on the wall, none across
        // the walk. The wall is 0.34 m thick.
        let wall_rd = rd(&polygon_rings(&wall["geometry"])[0][0]);
        assert!(line.len() > 2, "{line:?}");
        for p in line {
            let q = cartopolis_geo::ahn::wgs84_to_rd(p[0], p[1]);
            let off = wall_rd
                .windows(2)
                .map(|w| super::super::lower_quay::distance_to_segment(q, w[0], w[1]))
                .fold(f64::MAX, f64::min);
            assert!(off < 0.5, "{p:?} is {off:.2} m off the wall");
        }
    }

    /// A strip turning a right angle keeps to itself round the corner, and a
    /// straight one is still its two ends.
    #[test]
    fn a_strip_round_a_corner_keeps_to_the_strip() {
        // 0.3 m thick, 20 m east then 20 m north, in degrees near 53°N.
        let (x, y) = (
            1.0 / (M_PER_DEG * 53f64.to_radians().cos()),
            1.0 / M_PER_DEG,
        );
        let at = |e: f64, n: f64| json!([6.5 + e * x, 53.0 + n * y]);
        let l = json!({"type": "Polygon", "coordinates": [[
            at(0.0, 0.0), at(20.0, 0.0), at(20.0, 20.0), at(19.7, 20.0),
            at(19.7, 0.3), at(0.0, 0.3), at(0.0, 0.0)
        ]]});
        let line = &centreline(&l)[0];
        // Its middle is (0, 0.15) → (19.85, 0.15) → (19.85, 20): every point of
        // the line within a few centimetres of it, the corner included.
        let middle = [[0.0, 0.15], [19.85, 0.15], [19.85, 20.0]];
        assert!((3..=5).contains(&line.len()), "{line:?}");
        let mut turned = false;
        for p in line {
            let q = [(p[1] - 6.5) / x, (p[0] - 53.0) / y];
            let off = middle
                .windows(2)
                .map(|w| super::super::lower_quay::distance_to_segment(q, w[0], w[1]))
                .fold(f64::MAX, f64::min);
            assert!(off < 0.15, "{q:?} is {off:.2} m off the middle");
            turned |= (q[0] - 19.85).abs() < 0.5 && q[1] < 1.0;
        }
        assert!(turned, "no point at the corner: {line:?}");
        let straight = json!({"type": "Polygon", "coordinates": [[
            at(0.0, 0.0), at(30.0, 0.0), at(30.0, 0.3), at(0.0, 0.3), at(0.0, 0.0)
        ]]});
        assert_eq!(centreline(&straight)[0].len(), 2);
    }
}
