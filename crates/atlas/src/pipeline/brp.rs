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
//! * **A parcel is binned whole, on its centroid.** A quarter of the fields a
//!   farmland cell owns leave it, so the paving's "outer ring inside the cell"
//!   rule would delete a quarter of the country's fields. Exactly one cell draws
//!   each parcel, overhang included.
//!
//! Data: RVO BRP Gewaspercelen via PDOK — Public Domain Mark 1.0.

use std::collections::{BTreeMap, HashMap};

use serde_json::{json, Value};

use super::geom::{lat_lng_rings, polygon_rings};
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

/// The cell a parcel belongs to: the one holding its outer ring's area centroid,
/// falling back to the vertex mean for a ring with no area. `ring` is
/// `[lat, lng]`, open.
fn owner(ring: &[[f64; 2]]) -> Option<Cell> {
    let n = ring.len();
    if n < 3 {
        return None;
    }
    // Relative to the first vertex, so the products keep their precision.
    let [lat0, lng0] = ring[0];
    let (mut a, mut cx, mut cy) = (0.0, 0.0, 0.0);
    for i in 0..n {
        let [y1, x1] = [ring[i][0] - lat0, ring[i][1] - lng0];
        let [y2, x2] = [ring[(i + 1) % n][0] - lat0, ring[(i + 1) % n][1] - lng0];
        let cross = x1 * y2 - x2 * y1;
        a += cross;
        cx += (x1 + x2) * cross;
        cy += (y1 + y2) * cross;
    }
    let (lat, lng) = if a.abs() > 1e-18 {
        (lat0 + cy / (3.0 * a), lng0 + cx / (3.0 * a))
    } else {
        let k = n as f64;
        (
            ring.iter().map(|p| p[0]).sum::<f64>() / k,
            ring.iter().map(|p| p[1]).sum::<f64>() / k,
        )
    };
    Some(lat_lng_to_cell(lat, lng))
}

/// One feature's parcels as `(cell, wire record)`, or the reason it is not
/// drawn.
fn parcels_of(feat: &Value) -> Result<Vec<(Cell, Value)>, Skip> {
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
    let mut out = Vec::new();
    for poly in polygon_rings(&feat["geometry"]) {
        let rings = lat_lng_rings(&poly, SIMPLIFY_M, MIN_AREA_M2);
        let Some(cell) = rings.first().and_then(|r| owner(r)) else {
            continue;
        };
        out.push((cell, json!([ci, code, rings])));
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
    let mut by_year: BTreeMap<i64, HashMap<Cell, Vec<Value>>> = BTreeMap::new();
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
    let cells = year.and_then(|y| by_year.remove(&y)).unwrap_or_default();

    let mut n_parcels = 0usize;
    for (cell, parcels) in &cells {
        n_parcels += parcels.len();
        ctx.write_json(&cell_path("brp", *cell), &json!({ "parcels": parcels }))?;
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
        assert_eq!(rec[0], 1);
        assert_eq!(rec[1], 2014);
        let outer = rec[2][0].as_array().unwrap();
        assert_eq!(outer.len(), 4, "open ring, corners only");
        assert_eq!(outer[0], json!([53.25, 6.5]), "lat before lng");
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

    /// Binned on the centroid, not on the first vertex: a field whose first
    /// corner sits across a cell edge still belongs to the cell it mostly covers.
    #[test]
    fn a_straddling_field_belongs_to_its_centroid() {
        // Find a cell edge, then start a long field just west of it.
        let (cx, _) = lat_lng_to_cell(53.25, 6.5);
        let mut edge = 6.5;
        while lat_lng_to_cell(53.25, edge).0 == cx {
            edge += 0.000_1;
        }
        let ring = vec![
            [edge - 0.000_2, 53.25],
            [edge + 0.01, 53.25],
            [edge + 0.01, 53.251],
            [edge - 0.000_2, 53.251],
            [edge - 0.000_2, 53.25],
        ];
        let parcels = parcels_of(&feature("Bouwland", json!(259), vec![ring])).unwrap();
        assert_eq!(parcels[0].0 .0, cx + 1);
    }

    #[test]
    fn a_crop_code_sent_as_text_still_reads() {
        let parcels =
            parcels_of(&feature("Bouwland", json!("233"), vec![field(6.5, 53.25)])).unwrap();
        assert_eq!(parcels[0].1[1], 233);
    }

    /// The client's `CATEGORIES` is positional; these are its positions.
    #[test]
    fn the_wire_order_is_the_client_s() {
        assert_eq!(CATEGORIES[0], "Grasland");
        assert_eq!(CATEGORIES[1], "Bouwland");
        assert_eq!(CATEGORIES[LANDSCAPE_ELEMENT], "Landschapselement");
    }
}
