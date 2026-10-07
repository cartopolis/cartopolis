//! 3DBAG **LoD1.2** buildings — a footprint plus a height per building, shaped so
//! the client's Overpass parser (`osm_buildings.rs`) consumes it unchanged.
//!
//! Ported from the earlier TypeScript pipeline (`buildings.ts`, itself from
//! `tools/3dbag_extract.py`):
//!
//!   1. Page the 3DBAG WFS `BAG3D:lod12` layer over an RD bbox — a footprint
//!      polygon plus the height attributes, so no CityJSON is needed here.
//!   2. Reproject every footprint vertex RD → WGS84.
//!   3. Turn each building into an Overpass-shaped way with `height` /
//!      `roof:shape` / `roof:height` tags derived from the BAG attributes.
//!   4. Bin each building into the z14 cell of its **first vertex** — the client's
//!      ownership rule — and write `buildings/14/{x}/{y}.json` plus the root
//!      `coverage.json` that tells the client which cells to source here.
//!
//! Data: 3DBAG (https://3dbag.nl), 3D geoinformation group, TU Delft — CC BY 4.0;
//! building footprints from Kadaster BAG.

use std::collections::{HashMap, HashSet};

use cartopolis_geo::formats::coverage::Manifest;
use cartopolis_geo::formats::osm_buildings::{self, LatLon, Tags};
use serde_json::{json, Value};

use super::http::{get_json_with, query};
use super::rd::rd_to_lat_lng;
use super::tile::{cell_path, lat_lng_to_cell, sorted_cells, Cell, CACHE_ZOOM};
use super::{Count, Ctx, DatasetResult, Outcome, Progress};

const WFS_BASE: &str = "https://data.3dbag.nl/api/BAG3D/wfs";
const WFS_LAYER: &str = "BAG3D:lod12";
/// The WFS hard cap per GetFeature request.
const PAGE: usize = 10_000;
/// Fallback metres per floor when only `b3_bouwlagen` is known.
const STOREY_M: f64 = 3.0;

const ATTRIBUTION: &str = "3D data © 3DBAG by TU Delft (3D geoinformation), CC BY 4.0; \
                           building footprints © Kadaster BAG";

fn num(props: &Value, key: &str) -> Option<f64> {
    props.get(key).and_then(Value::as_f64)
}

/// Overpass `height` / `roof:*` tags from the BAG height attributes, or `None`
/// when the building has no usable height at all. `height` is the total to the
/// ridge and the roof is carved off the top of it, which is what the client's
/// parser expects.
#[must_use]
pub fn height_tags(props: &Value) -> Option<Tags> {
    let mut ground = num(props, "b3_h_maaiveld");
    let mut roof = num(props, "b3_h_70p")
        .or_else(|| num(props, "b3_h_50p"))
        .or_else(|| num(props, "b3_h_max"));
    let nok = num(props, "b3_h_nok");
    let dak = props.get("b3_dak_type").and_then(Value::as_str);
    // Truthiness, as the originals tested it: zero storeys estimates nothing.
    let lagen = num(props, "b3_bouwlagen").filter(|l| *l != 0.0);

    if let (None, Some(r), Some(l)) = (ground, roof, lagen) {
        ground = Some(r - l * STOREY_M);
    }
    if let (None, Some(l)) = (roof, lagen) {
        roof = Some(ground.unwrap_or(0.0) + l * STOREY_M);
    }
    let (roof, ground) = (roof?, ground?);

    let mut tags = Tags::new();
    tags.insert("building".into(), "yes".into());
    // **Who the block is, and when it was built**, so the client colours it as
    // it colours the same building's LoD2.2 shell: by the roof atlas measured
    // from the photo (keyed by this id, `roofs` dataset) and by its era. Without
    // them every block fell back to a random terracotta, and a city's grey flat
    // roofs turned red the moment the camera rose past the shells' ring.
    if let Some(oid) = props.get("identificatie").and_then(Value::as_str) {
        tags.insert("ref:bag".into(), super::lod22::object_id(oid).0.into());
    }
    if let Some(year) = num(props, "oorspronkelijkbouwjaar").filter(|y| *y > 1000.0) {
        tags.insert("start_date".into(), format!("{}", year as i64).into());
    }
    if let Some(l) = lagen {
        tags.insert(
            "building:levels".into(),
            format!("{}", l.trunc() as i64).into(),
        );
    }
    match (dak, nok) {
        (Some("slanted"), Some(nok)) if nok > roof => {
            // Walls to the LoD1.2 roof level (h_70p), a gable up to the ridge (nok).
            let total = (nok - ground).max(2.0);
            let roof_h = (nok - roof).max(0.5).min(total * 0.8);
            tags.insert("height".into(), format!("{total:.2}").into());
            tags.insert("roof:shape".into(), "gabled".into());
            tags.insert("roof:height".into(), format!("{roof_h:.2}").into());
        }
        _ => {
            tags.insert(
                "height".into(),
                format!("{:.2}", (roof - ground).max(2.0)).into(),
            );
            tags.insert("roof:shape".into(), "flat".into());
        }
    }
    Some(tags)
}

/// Each polygon's **exterior** ring as RD `[x, y]`. Interior rings are dropped:
/// the client extrudes a single outer ring and ignores holes anyway.
fn outer_rings(geom: &Value) -> Vec<Vec<[f64; 2]>> {
    let ring = |r: &Value| -> Vec<[f64; 2]> {
        r.as_array()
            .map(|pts| {
                pts.iter()
                    .filter_map(|p| Some([p.get(0)?.as_f64()?, p.get(1)?.as_f64()?]))
                    .collect()
            })
            .unwrap_or_default()
    };
    let coords = &geom["coordinates"];
    match geom["type"].as_str() {
        Some("Polygon") => coords.get(0).map(ring).into_iter().collect(),
        Some("MultiPolygon") => coords
            .as_array()
            .map(|ps| ps.iter().filter_map(|p| p.get(0)).map(ring).collect())
            .unwrap_or_default(),
        _ => Vec::new(),
    }
}

/// Stable integer id from the WFS feature id (`lod12.10777377`), else `fid`.
#[must_use]
pub fn feature_id(feat: &Value) -> i64 {
    let fid = feat["id"].as_str().unwrap_or("");
    let tail = fid.rsplit('.').next().unwrap_or("");
    if !tail.is_empty() && tail.bytes().all(|b| b.is_ascii_digit()) {
        if let Ok(n) = tail.parse() {
            return n;
        }
    }
    feat["properties"]["fid"].as_i64().unwrap_or(0)
}

fn page_url(bbox: [f64; 4], start: usize) -> String {
    let [xmin, ymin, xmax, ymax] = bbox;
    format!(
        "{WFS_BASE}?{}",
        query(&[
            ("service", "WFS"),
            ("version", "2.0.0"),
            ("request", "GetFeature"),
            ("typeNames", WFS_LAYER),
            ("count", &PAGE.to_string()),
            ("startIndex", &start.to_string()),
            ("outputFormat", "application/json"),
            ("srsName", "EPSG:28992"),
            (
                "bbox",
                &format!(
                    "{},{},{},{},EPSG:28992",
                    js(xmin),
                    js(ymin),
                    js(xmax),
                    js(ymax)
                )
            ),
        ])
    )
}

/// A number as JavaScript's `String(n)` writes it — `229000`, not `229000.0` —
/// so the request is the one the TypeScript sent.
fn js(v: f64) -> String {
    if v.fract() == 0.0 && v.abs() < 1e15 {
        format!("{}", v as i64)
    } else {
        format!("{v}")
    }
}

pub fn run(ctx: &Ctx) -> Outcome<DatasetResult> {
    let mut cells: HashMap<Cell, Vec<osm_buildings::Element>> = HashMap::new();
    let mut seen: HashSet<i64> = HashSet::new();
    let (mut total_in, mut total_out, mut start) = (0usize, 0usize, 0usize);
    let mut matched: Option<u64> = None;

    ctx.progress(Progress {
        phase: Some("buildings".into()),
        done: Some(0),
        total: Some(None),
    });

    loop {
        ctx.check()?;
        let page = get_json_with(
            ctx,
            &page_url(ctx.bbox, start),
            &format!("lod12 page {start}"),
            std::time::Duration::from_secs(120),
            4,
        )?;
        let feats = page["features"].as_array().cloned().unwrap_or_default();
        if matched.is_none() {
            matched = page["numberMatched"].as_u64();
        }
        if feats.is_empty() {
            break;
        }
        for feat in &feats {
            total_in += 1;
            let fid = feature_id(feat);
            let Some(tags) = height_tags(&feat["properties"]) else {
                continue;
            };
            for (ri, ring) in outer_rings(&feat["geometry"]).into_iter().enumerate() {
                // Fewer than four points including the closing one is not a ring.
                if ring.len() < 4 {
                    continue;
                }
                let geometry: Vec<(f64, f64)> =
                    ring.iter().map(|&[x, y]| rd_to_lat_lng(x, y)).collect();
                // One way per polygon; the id is suffixed for MultiPolygon parts.
                let wid = if ri == 0 { fid } else { fid * 100 + ri as i64 };
                if !seen.insert(wid) {
                    continue;
                }
                let cell = lat_lng_to_cell(geometry[0].0, geometry[0].1);
                let way = osm_buildings::Element {
                    geometry: geometry
                        .iter()
                        .map(|&(lat, lon)| Some(LatLon { lat, lon }))
                        .collect(),
                    id: wid,
                    tags: tags.clone(),
                    kind: "way".into(),
                };
                cells.entry(cell).or_default().push(way);
                total_out += 1;
            }
        }
        let got = feats.len();
        ctx.log(format!(
            "page start={start}: {got} features ({total_out} buildings, {} cells)",
            cells.len()
        ));
        start += got;
        ctx.progress(Progress {
            phase: Some("buildings".into()),
            done: Some(total_out as u64),
            total: Some(matched),
        });
        if matched.is_some_and(|m| m > 0 && start as u64 >= m) || got < PAGE {
            break;
        }
    }

    let covered = sorted_cells(cells.keys().copied());
    let n_cells = cells.len();
    for (cell, elements) in cells {
        ctx.write_doc(
            &cell_path("buildings", cell),
            &osm_buildings::Cell {
                elements: Some(elements),
                remark: None,
            },
        )?;
    }
    ctx.write_coverage(
        "coverage.json",
        &Manifest::new(CACHE_ZOOM, covered)
            .described(
                ctx.name.as_str(),
                ATTRIBUTION,
                "3DBAG WFS BAG3D:lod12",
                ctx.bbox,
            )
            .with("building_count", total_out),
    )?;
    ctx.log(format!(
        "done: {total_out} buildings from {total_in} features across {n_cells} z{CACHE_ZOOM} cells (matched={})",
        matched.map_or("?".to_string(), |m| m.to_string())
    ));
    Ok(DatasetResult {
        cells: n_cells,
        counts: vec![Count::new("buildings", total_out)],
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_slanted_roof_becomes_a_gable_to_the_ridge() {
        let t = height_tags(&json!({
            "b3_h_maaiveld": 1.0, "b3_h_70p": 7.0, "b3_h_nok": 10.0,
            "b3_dak_type": "slanted", "b3_bouwlagen": 2
        }))
        .unwrap();
        assert_eq!(t["height"], "9.00");
        assert_eq!(t["roof:shape"], "gabled");
        assert_eq!(t["roof:height"], "3.00");
        assert_eq!(t["building:levels"], "2");
    }

    #[test]
    fn a_ground_is_estimated_from_storeys_and_nothing_is_not_a_building() {
        let t = height_tags(&json!({"b3_h_70p": 9.0, "b3_bouwlagen": 3})).unwrap();
        assert_eq!(t["height"], "9.00");
        assert_eq!(t["roof:shape"], "flat");
        assert!(height_tags(&json!({"b3_h_70p": 9.0})).is_none());
        assert!(height_tags(&json!({"b3_h_70p": 9.0, "b3_bouwlagen": 0})).is_none());
    }

    /// The block carries the key the roofs file is written under, and its year,
    /// so the client can colour it as it colours the building's shell.
    #[test]
    fn a_block_names_its_building_and_its_year() {
        let t = height_tags(&json!({
            "b3_h_maaiveld": 1.0, "b3_h_70p": 7.0, "b3_bouwlagen": 2,
            "identificatie": "NL.IMBAG.Pand.0014100010920520",
            "oorspronkelijkbouwjaar": 1931
        }))
        .unwrap();
        assert_eq!(t["ref:bag"], "0014100010920520");
        assert_eq!(t["start_date"], "1931");
        let bare = height_tags(&json!({"b3_h_maaiveld": 1.0, "b3_h_70p": 7.0})).unwrap();
        assert!(bare.get("ref:bag").is_none() && bare.get("start_date").is_none());
    }

    #[test]
    fn feature_ids_come_from_the_wfs_id_first() {
        assert_eq!(feature_id(&json!({"id": "lod12.10777377"})), 10_777_377);
        assert_eq!(feature_id(&json!({"id": "x", "properties": {"fid": 5}})), 5);
    }

    #[test]
    fn a_bbox_is_written_as_javascript_wrote_it() {
        let url = page_url([229_000.0, 578_000.0, 238_000.0, 587_000.0], 0);
        assert!(
            url.contains("bbox=229000%2C578000%2C238000%2C587000%2CEPSG%3A28992"),
            "{url}"
        );
    }
}
