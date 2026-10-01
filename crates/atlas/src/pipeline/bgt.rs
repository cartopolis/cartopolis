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
use cartopolis_geo::formats::LatLng;
use serde_json::{json, Value};

use super::bgt_extract::Extract;
use super::bv3d::separation_heights_for;
use super::geom::{lat_lng_rings, polygon_rings, round, Pt};
use super::tile::{cell_path, lat_lng_to_cell, sorted_cells, Cell, CACHE_ZOOM};
use super::{Count, Ctx, DatasetResult, Outcome, Progress};

const ATTRIBUTION: &str = "Street objects © Kadaster BGT via PDOK (open data)";

/// Point forms in wire order, mirrored by `PROP_KINDS` in the client — **append,
/// never renumber**.
const PROP_KINDS: [&str; 7] = [
    "lamp",
    "bench",
    "shelter",
    "picnic_table",
    "artwork",
    "swing",
    "cabinet",
];
/// Linear forms in wire order, mirrored by `LINE_KINDS`.
const LINE_KINDS: [&str; 3] = ["fence", "wall", "hedge"];
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
const SURFACE_KINDS: [&str; 12] = [
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
        "muur" | "geluidsscherm" | "kademuur" | "damwand" | "walbescherming" => "wall",
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
        "gesloten verharding" | "open verharding" | "half verhard" | "erf" | "onverhard" => "yard",
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
const FEATURETYPES: [&str; 8] = [
    "vegetatieobject",
    "paal",
    "straatmeubilair",
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

/// A long thin polygon reduced to the line down its middle — its **principal
/// axis** through the centroid, cut at the extremes. Right for the strip a hedge
/// or a wall almost always is; an arbitrary diagonal for a rare blobby one, which
/// is a worse hedge, not a wrong place. Output `[lat, lng]`.
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
        let pts: Vec<Pt> = ring
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
        if pts.len() < 3 {
            continue;
        }
        let n = pts.len() as f64;
        let cx = pts.iter().map(|p| p[0]).sum::<f64>() / n;
        let cy = pts.iter().map(|p| p[1]).sum::<f64>() / n;
        let (mut sxx, mut syy, mut sxy) = (0.0, 0.0, 0.0);
        for &[px, py] in &pts {
            sxx += (px - cx).powi(2);
            syy += (py - cy).powi(2);
            sxy += (px - cx) * (py - cy);
        }
        let theta = 0.5 * (2.0 * sxy).atan2(sxx - syy);
        let (ux, uy) = (theta.cos(), theta.sin());
        let ts: Vec<f64> = pts
            .iter()
            .map(|&[px, py]| (px - cx) * ux + (py - cy) * uy)
            .collect();
        let lo = ts.iter().copied().fold(f64::INFINITY, f64::min);
        let hi = ts.iter().copied().fold(f64::NEG_INFINITY, f64::max);
        // In degrees; shorter than about a metre is not worth an object.
        if hi - lo < 1.0e-5 {
            continue;
        }
        out.push(vec![
            [round(cy + lo * uy, 7), round(cx + lo * ux, 7)],
            [round(cy + hi * uy, 7), round(cx + hi * ux, 7)],
        ]);
    }
    out
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
    for collection in ["paal", "straatmeubilair"] {
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
    ctx.log("muren als vlak…");
    items(ctx, extract, "scheiding_vlak", |feat| {
        let t = js_str(&feat["properties"]["type"]);
        let Some(form) = line_form(&t) else {
            return;
        };
        let id = js_str(&feat["properties"]["lokaal_id"]);
        for pts in centreline(&feat["geometry"]) {
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

    let n_surf = collect_surfaces(ctx, extract, &mut cells)?;

    let written: Vec<(Cell, CellData)> = cells
        .into_iter()
        .filter(|(_, c)| {
            !c.trees.is_empty()
                || !c.props.is_empty()
                || !c.lines.is_empty()
                || !c.surfaces.is_empty()
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
        if !surfaces.is_empty() {
            ctx.write_doc(
                &format!("bgt/{CACHE_ZOOM}/{cx}/{cy}.surfaces.json"),
                &bgt::Paving { surfaces },
            )?;
        }
    }
    ctx.write_json(
        "bgt/coverage.json",
        &json!({
            "name": ctx.name,
            "zoom": CACHE_ZOOM,
            "attribution": ATTRIBUTION,
            "source": "Kadaster BGT via PDOK download API",
            "bbox_rd": ctx.bbox,
            "kinds": PROP_KINDS,
            "line_kinds": LINE_KINDS,
            "surface_kinds": SURFACE_KINDS,
            "surface_materials": SURFACE_MATERIALS,
            "bridge_parts": BRIDGE_PARTS,
            "cells": covered,
            "tree_count": n_trees,
            "prop_count": n_props,
            "line_count": n_lines,
            // How much of the boundary layer carries a real measurement, so "the
            // walls look wrong" has a number attached.
            "measured_line_count": n_measured,
            "surface_count": n_surf,
            "bridge_count": n_bridges,
        }),
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
) -> Outcome<usize> {
    let (mut n, mut dropped_level, mut n_material) = (0usize, 0usize, 0usize);
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
                let surface = Surface(kind, ring_pairs(&out_rings), material);
                cells
                    .entry(lat_lng_to_cell(first[0], first[1]))
                    .or_default()
                    .surfaces
                    .push(surface);
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
    Ok(n)
}

#[cfg(test)]
mod tests {
    use super::*;

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
}
