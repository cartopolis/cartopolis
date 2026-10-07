//! The **BRP** — the RVO's Basisregistratie Gewaspercelen, every agricultural
//! field in the country with the crop grown on it that year — as the per-cell
//! parcel files the client's `systems::crops` reads and `cartopolis_geo::crops`
//! draws. `docs/notes/crop-parcels.md` has the endpoint, the licence and the
//! measurements; the client half shipped first, so the wire format below is the
//! one it already parses.
//!
//! Two decisions are the client's and are only carried out here:
//!
//! * **`Landschapselement` is dropped.** Most of those records are ditches, which
//!   the Shortbread water layers already draw; a parcel filled over one is land
//!   where the register says water. Its wire position (5) stays reserved in
//!   [`CATEGORIES`] so nothing re-uses it for a form that *is* drawn.
//! * **A parcel crossing a cell edge is cut along it**, and each cell gets the
//!   piece inside it. A quarter of a farmland cell's fields cross its edge, so the
//!   paving's "drop what leaves the cell" rule would delete a quarter of the
//!   country's fields. They used to be binned whole on their centroid instead,
//!   which drew them, but left the neighbouring cell unaware of the overhang: the
//!   client cuts each cell's grass out from under the fields it knows about
//!   (`cartopolis_geo::crops::cut_into_ground`), and grass under a field another
//!   cell owned stayed, drawn in blotches through it once relief was on. A cell's
//!   own edge is a straight line of latitude or longitude, so the cut is exact.
//!
//! Data: RVO BRP Gewaspercelen via PDOK — Public Domain Mark 1.0.

use std::collections::{BTreeMap, HashMap};

use cartopolis_geo::formats::brp as wire;
use serde_json::{json, Value};

use super::geom::{lat_lng_rings, polygon_rings, ring_area, round, M_PER_DEG};
use super::http::{get_json, query};
use super::rd::wgs84_bbox;
use super::tile::{cell_path, lat_lng_to_cell, sorted_cells, Cell, CACHE_ZOOM};
use super::{Count, Ctx, DatasetResult, Outcome, PipelineError, Progress};

const OGC_BASE: &str = "https://api.pdok.nl/rvo/gewaspercelen/ogc/v1";
const COLLECTION: &str = "brpgewas";
/// Features per page — the service's ceiling; it answers with a `next` cursor.
const PAGE: usize = 1_000;
/// Pages before the walk gives up, since the service gives no total to check
/// against. A thousand pages is a million parcels, more than the country has.
const MAX_PAGES: usize = 1_000;

const ATTRIBUTION: &str = "Crop parcels: RVO BRP Gewaspercelen via PDOK (public domain)";

/// The register's `category`, in wire order. Mirrored by `CATEGORIES` in the
/// client's `systems::crops` and by `CropCategory` — **append, never renumber**.
/// The last position is reserved and never written (see the module note).
const CATEGORIES: [&str; 6] = [
    "Grasland",
    "Bouwland",
    "Natuurterrein",
    "Braakland",
    "Overige",
    "Landschapselement",
];
const LANDSCAPE_ELEMENT: usize = 5;

/// Metres. **Measured** 2026-08-30: 0.5 m keeps 31–32 % of a farmland cell's
/// vertices, and a field edge is not surveyed to better than that anyway.
const SIMPLIFY_M: f64 = 0.5;
/// Below this a parcel is a sliver — a digitising artefact along a ditch.
const MIN_AREA_M2: f64 = 1.0;

/// A cell's extent as `[south, north, west, east]` in degrees.
fn cell_bounds((x, y): Cell) -> [f64; 4] {
    let (north, west) = cartopolis_core::tile_to_lat_lng(x, y, CACHE_ZOOM);
    let (south, east) = cartopolis_core::tile_to_lat_lng(x + 1, y + 1, CACHE_ZOOM);
    [south, north, west, east]
}

/// Sutherland–Hodgman against a lat/lng rectangle: the part of an open
/// `[lat, lng]` ring inside `[south, north, west, east]`. Exact for a concave
/// ring against a convex window, which a cell is; where the ring leaves and
/// re-enters along one edge the result carries a zero-width run along that edge,
/// which earcut and the fill tolerate.
fn clip_ring(ring: &[[f64; 2]], [south, north, west, east]: [f64; 4]) -> Vec<[f64; 2]> {
    // (axis, bound, keep-the-side-above)
    let planes = [
        (0, south, true),
        (0, north, false),
        (1, west, true),
        (1, east, false),
    ];
    let mut pts = ring.to_vec();
    for (axis, bound, above) in planes {
        if pts.len() < 3 {
            return Vec::new();
        }
        let inside = |p: &[f64; 2]| {
            if above {
                p[axis] >= bound
            } else {
                p[axis] <= bound
            }
        };
        let mut out = Vec::with_capacity(pts.len() + 4);
        for i in 0..pts.len() {
            let a = pts[i];
            let b = pts[(i + 1) % pts.len()];
            let (ia, ib) = (inside(&a), inside(&b));
            if ia {
                out.push(a);
            }
            if ia != ib {
                let t = (bound - a[axis]) / (b[axis] - a[axis]);
                let other = 1 - axis;
                let mut p = [0.0; 2];
                p[axis] = bound;
                p[other] = a[other] + t * (b[other] - a[other]);
                out.push(p);
            }
        }
        pts = out;
    }
    let mut out: Vec<[f64; 2]> = pts
        .iter()
        .map(|p| [round(p[0], 7), round(p[1], 7)])
        .collect();
    out.dedup();
    if out.len() > 1 && out.first() == out.last() {
        out.pop();
    }
    out
}

/// Area of an open `[lat, lng]` ring in square metres, for the sliver test.
fn area_m2(ring: &[[f64; 2]]) -> f64 {
    let Some(&[lat0, lng0]) = ring.first() else {
        return 0.0;
    };
    let scale = lat0.to_radians().cos() * M_PER_DEG;
    let m: Vec<[f64; 2]> = ring
        .iter()
        .map(|&[lat, lng]| [(lng - lng0) * scale, (lat - lat0) * M_PER_DEG])
        .collect();
    ring_area(&m)
}

/// One parcel's rings as the piece each cell draws. A parcel inside one cell —
/// three in four of them — goes through untouched.
fn pieces(rings: Vec<Vec<[f64; 2]>>) -> Vec<(Cell, Vec<Vec<[f64; 2]>>)> {
    let Some(outer) = rings.first().filter(|r| r.len() >= 3) else {
        return Vec::new();
    };
    let (mut s, mut n, mut w, mut e) = (f64::MAX, f64::MIN, f64::MAX, f64::MIN);
    for &[lat, lng] in outer {
        s = s.min(lat);
        n = n.max(lat);
        w = w.min(lng);
        e = e.max(lng);
    }
    // Tile y grows southward, so the north-west corner has the smallest x and y.
    let (x0, y0) = lat_lng_to_cell(n, w);
    let (x1, y1) = lat_lng_to_cell(s, e);
    if (x0, y0) == (x1, y1) {
        return vec![((x0, y0), rings)];
    }
    let mut out = Vec::new();
    for x in x0..=x1 {
        for y in y0..=y1 {
            let bounds = cell_bounds((x, y));
            let clipped = clip_ring(outer, bounds);
            if clipped.len() < 3 || area_m2(&clipped) < MIN_AREA_M2 {
                continue;
            }
            let mut piece = vec![clipped];
            piece.extend(
                rings[1..]
                    .iter()
                    .map(|h| clip_ring(h, bounds))
                    .filter(|h| h.len() >= 3),
            );
            out.push(((x, y), piece));
        }
    }
    out
}

/// One feature's parcels as `(cell, wire record)`, or the reason it is not
/// drawn.
fn parcels_of(feat: &Value) -> Result<Vec<(Cell, wire::Parcel)>, Skip> {
    let props = &feat["properties"];
    let category = props["category"].as_str().unwrap_or_default();
    let Some(ci) = CATEGORIES.iter().position(|c| *c == category) else {
        return Err(Skip::UnknownCategory);
    };
    if ci == LANDSCAPE_ELEMENT {
        return Err(Skip::LandscapeElement);
    }
    // Zero is not a code the register uses; the client reads it as "no tone of
    // its own" and falls back to the category's.
    let code = props["gewascode"]
        .as_u64()
        .or_else(|| {
            props["gewascode"]
                .as_str()
                .and_then(|s| s.trim().parse().ok())
        })
        .unwrap_or(0);
    // The client reads the code as a `u32`; one past it would fail the whole
    // cell's parse there, so it is written as "no code" instead. The register's
    // codes are three- and four-digit numbers.
    let code = u32::try_from(code).unwrap_or(0);
    let mut out = Vec::new();
    for poly in polygon_rings(&feat["geometry"]) {
        for (cell, rings) in pieces(lat_lng_rings(&poly, SIMPLIFY_M, MIN_AREA_M2)) {
            let rings = rings
                .into_iter()
                .map(|r| r.into_iter().map(|[lat, lng]| (lat, lng)).collect())
                .collect();
            out.push((cell, (ci, code, rings)));
        }
    }
    if out.is_empty() {
        Err(Skip::Sliver)
    } else {
        Ok(out)
    }
}

#[derive(Debug, PartialEq)]
enum Skip {
    LandscapeElement,
    UnknownCategory,
    Sliver,
}

pub fn run(ctx: &Ctx) -> Outcome<DatasetResult> {
    let bbox = wgs84_bbox(ctx.bbox);
    // Held per year until the walk ends: the service serves one year today, and
    // if it ever serves two, only the newest is drawn — two years of one field
    // on one rung of the depth ladder is a z-fight.
    let mut by_year: BTreeMap<i64, HashMap<Cell, Vec<wire::Parcel>>> = BTreeMap::new();
    let mut seen_categories: BTreeMap<String, usize> = BTreeMap::new();
    let (mut total, mut ditches, mut unknown, mut slivers) = (0usize, 0usize, 0usize, 0usize);

    ctx.log("gewaspercelen…");
    let mut url = Some(format!(
        "{OGC_BASE}/collections/{COLLECTION}/items?{}",
        query(&[
            ("limit", &PAGE.to_string()),
            (
                "bbox",
                &bbox
                    .iter()
                    .map(|v| format!("{v:.6}"))
                    .collect::<Vec<_>>()
                    .join(",")
            ),
            ("f", "json"),
        ])
    ));
    let mut pages = 0usize;
    while let Some(u) = url.take() {
        if pages >= MAX_PAGES {
            return Err(PipelineError::Failed(format!(
                "hit the {MAX_PAGES}-page guard; the area would be incomplete"
            )));
        }
        ctx.check()?;
        // A failed page fails the run: a half-walked area would publish a
        // coverage manifest claiming cells whose fields are missing.
        let page = get_json(ctx, &u, COLLECTION)?;
        let feats = page["features"].as_array().cloned().unwrap_or_default();
        for feat in &feats {
            total += 1;
            let props = &feat["properties"];
            *seen_categories
                .entry(props["category"].as_str().unwrap_or("").to_string())
                .or_default() += 1;
            let year = props["jaar"].as_i64().unwrap_or(0);
            match parcels_of(feat) {
                Ok(parcels) => {
                    let cells = by_year.entry(year).or_default();
                    for (cell, parcel) in parcels {
                        cells.entry(cell).or_default().push(parcel);
                    }
                }
                Err(Skip::LandscapeElement) => ditches += 1,
                Err(Skip::UnknownCategory) => unknown += 1,
                Err(Skip::Sliver) => slivers += 1,
            }
        }
        pages += 1;
        ctx.progress(Progress {
            phase: Some("gewaspercelen".into()),
            done: Some(total as u64),
            total: Some(None),
        });
        if feats.is_empty() {
            break;
        }
        url = page["links"]
            .as_array()
            .and_then(|ls| ls.iter().find(|l| l["rel"] == "next"))
            .and_then(|l| l["href"].as_str())
            .map(str::to_string);
    }

    let breakdown: Vec<String> = seen_categories
        .iter()
        .map(|(c, n)| format!("{n} {}", if c.is_empty() { "(none)" } else { c }))
        .collect();
    ctx.log(format!(
        "{total} features in {pages} pages: {}",
        breakdown.join(", ")
    ));
    if unknown > 0 {
        // A category the client has no variant for is dropped on both sides; a
        // new one in the register is worth appending to `CATEGORIES`.
        ctx.log(format!(
            "WARNING: {unknown} features carry a category not in CATEGORIES and were dropped"
        ));
    }
    let year = by_year.keys().next_back().copied();
    if by_year.len() > 1 {
        ctx.log(format!(
            "the service answered {} years; drawing {} only",
            by_year.len(),
            year.unwrap_or(0)
        ));
    }
    let mut cells = year.and_then(|y| by_year.remove(&y)).unwrap_or_default();

    let mut n_parcels = 0usize;
    for (cell, parcels) in &mut cells {
        n_parcels += parcels.len();
        let doc = wire::Cell {
            parcels: std::mem::take(parcels),
        };
        ctx.write_doc(&cell_path("brp", *cell), &doc)?;
    }
    ctx.write_json(
        "brp/coverage.json",
        &json!({
            "name": ctx.name,
            "zoom": CACHE_ZOOM,
            "attribution": ATTRIBUTION,
            "source": "RVO BRP Gewaspercelen via PDOK OGC API",
            "bbox_rd": ctx.bbox,
            "year": year,
            "categories": CATEGORIES,
            "cells": sorted_cells(cells.keys().copied()),
            "parcel_count": n_parcels,
        }),
    )?;
    ctx.log(format!(
        "done: {n_parcels} parcels across {} cells; {ditches} landscape elements (ditches) and {slivers} slivers left \
         out",
        cells.len()
    ));
    Ok(DatasetResult {
        cells: cells.len(),
        counts: vec![
            Count::new("parcels", n_parcels),
            Count::new("landscape elements dropped", ditches),
        ],
    })
}

#[cfg(test)]
mod tests {
    use super::super::geom::Pt;
    use super::*;

    fn feature(category: &str, code: Value, rings: Vec<Vec<Pt>>) -> Value {
        json!({
            "type": "Feature",
            "properties": {"category": category, "gewascode": code, "jaar": 2025},
            "geometry": {"type": "Polygon", "coordinates": rings},
        })
    }

    /// A ~110 m square north of Groningen, `[lng, lat]`, closed as GeoJSON is.
    fn field(lng: f64, lat: f64) -> Vec<Pt> {
        let d = 0.001;
        vec![
            [lng, lat],
            [lng + d, lat],
            [lng + d, lat + d],
            [lng, lat + d],
            [lng, lat],
        ]
    }

    /// The client's wire shape: `[category, code, [[[lat, lng], …], …]]`.
    #[test]
    fn a_field_is_written_as_the_client_parses_it() {
        let parcels =
            parcels_of(&feature("Bouwland", json!(2014), vec![field(6.5, 53.25)])).unwrap();
        assert_eq!(parcels.len(), 1);
        let (cell, rec) = &parcels[0];
        assert_eq!(*cell, lat_lng_to_cell(53.2505, 6.5005));
        assert_eq!(rec.0, 1);
        assert_eq!(rec.1, 2014);
        let outer = &rec.2[0];
        assert_eq!(outer.len(), 4, "open ring, corners only");
        assert_eq!(outer[0], (53.25, 6.5), "lat before lng");
    }

    /// The typed record serialises to exactly what `json!([ci, code, rings])`
    /// did, so cells already on the host and the ones written now read alike.
    #[test]
    fn a_parcel_is_written_byte_for_byte_as_before() {
        let parcels =
            parcels_of(&feature("Bouwland", json!(2014), vec![field(6.5, 53.25)])).unwrap();
        let (_, rec) = &parcels[0];
        let rings: Vec<Vec<[f64; 2]>> = rec
            .2
            .iter()
            .map(|r| r.iter().map(|&(lat, lng)| [lat, lng]).collect())
            .collect();
        let old = json!({ "parcels": [json!([rec.0, u64::from(rec.1), rings])] });
        let new = wire::Cell {
            parcels: vec![rec.clone()],
        };
        assert_eq!(
            cartopolis_geo::formats::to_vec(&new).unwrap(),
            serde_json::to_vec(&old).unwrap()
        );
    }

    #[test]
    fn ditches_and_unknown_categories_are_not_drawn() {
        let f = |c| parcels_of(&feature(c, json!(1), vec![field(6.5, 53.25)]));
        assert_eq!(f("Landschapselement").unwrap_err(), Skip::LandscapeElement);
        assert_eq!(f("Iets nieuws").unwrap_err(), Skip::UnknownCategory);
    }

    #[test]
    fn a_sliver_is_dropped() {
        let sliver = vec![
            [6.5, 53.25],
            [6.500_001, 53.25],
            [6.500_001, 53.250_001],
            [6.5, 53.25],
        ];
        assert_eq!(
            parcels_of(&feature("Grasland", json!(265), vec![sliver])).unwrap_err(),
            Skip::Sliver
        );
    }

    /// A field across a cell edge becomes one piece per cell, meeting exactly on
    /// the edge and adding up to the whole field — so each cell knows every
    /// field on its ground, and none is drawn twice.
    #[test]
    fn a_straddling_field_is_cut_at_the_cell_edge() {
        let (cx, cy) = lat_lng_to_cell(53.2505, 6.5);
        let [.., east] = cell_bounds((cx, cy));
        let ring = vec![
            [east - 0.002, 53.25],
            [east + 0.01, 53.25],
            [east + 0.01, 53.251],
            [east - 0.002, 53.251],
            [east - 0.002, 53.25],
        ];
        let whole = area_m2(&[
            [53.25, east - 0.002],
            [53.25, east + 0.01],
            [53.251, east + 0.01],
            [53.251, east - 0.002],
        ]);
        let parcels = parcels_of(&feature("Bouwland", json!(259), vec![ring])).unwrap();
        let cells: Vec<Cell> = parcels.iter().map(|(c, _)| *c).collect();
        assert_eq!(cells, vec![(cx, cy), (cx + 1, cy)]);
        let mut total = 0.0;
        for (cell, rec) in &parcels {
            let [_, _, w, e] = cell_bounds(*cell);
            let outer: Vec<[f64; 2]> = rec.2[0].iter().map(|&(lat, lng)| [lat, lng]).collect();
            assert!(
                outer.iter().all(|p| p[1] >= w - 1e-7 && p[1] <= e + 1e-7),
                "{cell:?} holds ground outside itself"
            );
            total += area_m2(&outer);
        }
        assert!(
            (total - whole).abs() < 1.0,
            "pieces {total} m², field {whole} m²"
        );
    }

    /// A hole survives the cut in the cell that holds it.
    #[test]
    fn a_hole_stays_with_its_piece() {
        let (cx, cy) = lat_lng_to_cell(53.2505, 6.5);
        let [.., east] = cell_bounds((cx, cy));
        let outer = vec![
            [east - 0.004, 53.25],
            [east + 0.004, 53.25],
            [east + 0.004, 53.252],
            [east - 0.004, 53.252],
            [east - 0.004, 53.25],
        ];
        let hole = vec![
            [east - 0.003, 53.2505],
            [east - 0.002, 53.2505],
            [east - 0.002, 53.2515],
            [east - 0.003, 53.2515],
            [east - 0.003, 53.2505],
        ];
        let parcels = parcels_of(&feature("Bouwland", json!(259), vec![outer, hole])).unwrap();
        let west_piece = parcels.iter().find(|(c, _)| *c == (cx, cy)).unwrap();
        let east_piece = parcels.iter().find(|(c, _)| *c == (cx + 1, cy)).unwrap();
        assert_eq!(west_piece.1 .2.len(), 2);
        assert_eq!(east_piece.1 .2.len(), 1);
    }

    #[test]
    fn a_crop_code_sent_as_text_still_reads() {
        let parcels =
            parcels_of(&feature("Bouwland", json!("233"), vec![field(6.5, 53.25)])).unwrap();
        assert_eq!(parcels[0].1 .1, 233);
    }

    /// The client's `CATEGORIES` is positional; these are its positions.
    #[test]
    fn the_wire_order_is_the_client_s() {
        assert_eq!(CATEGORIES[0], "Grasland");
        assert_eq!(CATEGORIES[1], "Bouwland");
        assert_eq!(CATEGORIES[LANDSCAPE_ELEMENT], "Landschapselement");
    }
}
