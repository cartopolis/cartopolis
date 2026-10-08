//! The BRO's point registers under the street: **boreholes and soundings**
//! (layer `boreholes`) and **groundwater monitoring wells** with their latest
//! measured level (layer `groundwater`), for `systems::underground`.
//!
//! * Boreholes come from three registers — geotechnical (BHR-GT), soil (BHR-P)
//!   and geological (BHR-G) — found by one bounding-box search each on
//!   `publiek.broservices.nl` and then read **one object at a time**, since only
//!   the object carries its layers. Each layer's soil name is read by its last
//!   word (`underground::soil_kind`). Groningen: ~1,630 boreholes, ~27 MB.
//! * Soundings (CPT) need no per-object read: the search hit carries the depth
//!   the cone reached. A sounding is drawn as a thin rod to that depth.
//! * Wells come from PDOK's OGC API in three bulk calls (wells, tubes, level
//!   dossiers). The latest level needs two small calls per dossier: its
//!   observation summary, then the most recent value of the newest regular
//!   observation. Everything there is **m NAP**, and a depth is the well's
//!   ground level minus it.
//!
//! **A search over 2,000 objects is refused whole** (HTTP 400, "Het aantal
//! objecten is groter dan 2000"), not cut short, so a refused box is split into
//! quarters and asked again. Layer depths in the borehole registers are metres
//! below the ground already.
//!
//! Data: Basisregistratie Ondergrond (BRO) via TNO and PDOK — CC0.

use serde_json::{json, Value};

use super::http::{get_json, get_text_retried, post_text};
use super::rd::wgs84_bbox;
use super::underground::{merge_runs, soil_kind, xml, Cells};
use super::{in_order, Ctx, DatasetResult, Outcome, PipelineError, Progress};
use cartopolis_geo::formats::underground::Kind;

const BRO: &str = "https://publiek.broservices.nl";
const PDOK: &str =
    "https://api.pdok.nl/tno/bro-grondwatermonitoring-in-samenhang-karakteristieken/ogc/v1";
/// Splits before a search gives up: 4⁶ boxes.
const MAX_SPLITS: u32 = 6;
const BOREHOLE_RADIUS_M: f32 = 0.6;
const SOUNDING_RADIUS_M: f32 = 0.15;
const TUBE_RADIUS_M: f32 = 0.25;
const WATER_MARK_M: f32 = 0.9;

const BOREHOLE_ATTRIBUTION: &str = "Boreholes and soundings © BRO (TNO) — CC0";
const GROUNDWATER_ATTRIBUTION: &str = "Groundwater wells and levels © BRO (TNO) via PDOK — CC0";

/// The registers a borehole can come from: search path, object path.
const BOREHOLE_REGISTERS: [(&str, &str); 3] = [
    ("sr/bhrgt/v2", "sr/bhrgt/v2/objects"),
    ("sr/bhrp/v2", "sr/bhrp/v2/objects"),
    ("sr/bhrg/v3", "sr/bhrg/v3/objects"),
];

/// One hit of a BRO search: its id, where it is, and the hit's own XML.
struct Hit {
    id: String,
    lat: f64,
    lng: f64,
    doc: String,
}

/// Every hit of a bounding-box search, splitting the box whenever the BRO
/// refuses it for being too full.
fn search(ctx: &Ctx, path: &str, bbox: [f64; 4], depth: u32) -> Outcome<Vec<Hit>> {
    let [lng0, lat0, lng1, lat1] = bbox;
    let body = json!({"area": {"boundingBox": {
        "lowerCorner": {"lat": lat0, "lon": lng0},
        "upperCorner": {"lat": lat1, "lon": lng1},
    }}});
    let url = format!("{BRO}/{path}/characteristics/searches");
    match post_text(ctx, &url, &body, path)? {
        Ok(text) => Ok(hits(&text)),
        Err((400, reason)) if reason.contains("2000") && depth < MAX_SPLITS => {
            let (mx, my) = ((lng0 + lng1) / 2.0, (lat0 + lat1) / 2.0);
            let mut out = Vec::new();
            for q in [
                [lng0, lat0, mx, my],
                [mx, lat0, lng1, my],
                [lng0, my, mx, lat1],
                [mx, my, lng1, lat1],
            ] {
                out.extend(search(ctx, path, q, depth + 1)?);
            }
            Ok(out)
        }
        Err((code, reason)) => Err(PipelineError::Failed(format!(
            "{path}: refused ({code}): {}",
            reason.chars().take(300).collect::<String>()
        ))),
    }
}

fn hits(text: &str) -> Vec<Hit> {
    xml::all(text, "dispatchDocument")
        .into_iter()
        .filter_map(|doc| {
            let id = xml::first(doc, "broId")?.trim().to_string();
            let pos = xml::first(xml::first(doc, "standardizedLocation")?, "pos")?;
            let mut it = pos.split_whitespace().filter_map(|v| v.parse::<f64>().ok());
            let (lat, lng) = (it.next()?, it.next()?);
            Some(Hit {
                id,
                lat,
                lng,
                doc: doc.to_string(),
            })
        })
        .collect()
}

/// A borehole object's layers as `(top_m, bottom_m, kind)`, whichever of the
/// three registers it came from.
#[must_use]
pub fn borehole_layers(doc: &str) -> Vec<(f32, f32, Kind)> {
    let mut blocks = xml::all(doc, "layer");
    blocks.extend(xml::all(doc, "soilLayer"));
    let mut layers: Vec<(f32, f32, Kind)> = blocks
        .into_iter()
        .filter_map(|b| {
            let top = xml::number(b, "upperBoundary")? as f32;
            let bottom = xml::number(b, "lowerBoundary")? as f32;
            let made = xml::first(b, "anthropogenic").is_some_and(|v| v.trim() == "ja");
            let name = xml::first(b, "geotechnicalSoilName")
                .or_else(|| xml::first(b, "standardSoilName"))
                .or_else(|| xml::first(b, "soilNameNEN5104"))
                .unwrap_or("");
            let kind = if made {
                Kind::Anthropogenic
            } else {
                soil_kind(name.trim(), xml::first(b, "sandMedianClass").map(str::trim))
            };
            (bottom > top).then_some((top, bottom, kind))
        })
        .collect();
    layers.sort_by(|a, b| a.0.total_cmp(&b.0));
    layers.dedup_by(|a, b| (a.0 - b.0).abs() < 1e-3);
    merge_runs(layers)
}

pub fn run_boreholes(ctx: &Ctx) -> Outcome<DatasetResult> {
    let bbox = wgs84_bbox(ctx.bbox);
    let mut cells = Cells::default();

    for (search_path, object_path) in BOREHOLE_REGISTERS {
        let found = search(ctx, search_path, bbox, 0)?;
        ctx.log(format!("{search_path}: {} boreholes", found.len()));
        let total = found.len();
        let mut done = 0u64;
        in_order(
            ctx,
            &found,
            ctx.upstream(),
            |hit| get_text_retried(ctx, &format!("{BRO}/{object_path}/{}", hit.id), "borehole"),
            |hit, doc| {
                done += 1;
                // One unreadable borehole costs that borehole, not the run.
                if let Ok(doc) = doc {
                    let layers = borehole_layers(&doc);
                    if !layers.is_empty() {
                        cells.column(hit.lat, hit.lng, BOREHOLE_RADIUS_M, layers);
                        cells.count("boreholes");
                    }
                }
                ctx.progress(Progress {
                    phase: Some(search_path.to_string()),
                    done: Some(done),
                    total: Some(Some(total as u64)),
                });
                Ok(())
            },
        )?;
    }

    let soundings = search(ctx, "sr/cpt/v1", bbox, 0)?;
    for s in &soundings {
        let Some(depth) = xml::number(&s.doc, "finalDepth").filter(|d| *d > 0.0) else {
            continue;
        };
        cells.column(
            s.lat,
            s.lng,
            SOUNDING_RADIUS_M,
            vec![(0.0, depth as f32, Kind::Sounding)],
        );
        cells.count("soundings");
    }
    cells.write(
        ctx,
        "boreholes",
        BOREHOLE_ATTRIBUTION,
        "BRO BHR-GT, BHR-P, BHR-G and CPT",
    )
}

/// Every feature of one PDOK collection in the box, following `next` links.
fn collection(ctx: &Ctx, name: &str, bbox: [f64; 4]) -> Outcome<Vec<Value>> {
    let [lng0, lat0, lng1, lat1] = bbox;
    let mut url = Some(format!(
        "{PDOK}/collections/{name}/items?f=json&limit=1000&bbox={lng0},{lat0},{lng1},{lat1}"
    ));
    let mut out = Vec::new();
    while let Some(u) = url.take() {
        ctx.check()?;
        let page = get_json(ctx, &u, name)?;
        out.extend(page["features"].as_array().cloned().unwrap_or_default());
        url = page["links"]
            .as_array()
            .and_then(|l| l.iter().find(|l| l["rel"] == "next"))
            .and_then(|l| l["href"].as_str())
            .map(str::to_string);
    }
    Ok(out)
}

/// A `dd-mm-yyyy` date as a sortable number.
fn day(d: &str) -> Option<u32> {
    let mut it = d.split('-').filter_map(|p| p.parse::<u32>().ok());
    let (dd, mm, yy) = (it.next()?, it.next()?, it.next()?);
    Some(yy * 10_000 + mm * 100 + dd)
}

/// The latest measured level of a dossier, m NAP: the newest regular
/// observation's most recent value.
fn latest_level(ctx: &Ctx, gld: &str) -> Option<f64> {
    let summary = get_json(
        ctx,
        &format!("{BRO}/gm/gld/v1/objects/{gld}/observationsSummary"),
        "gld summary",
    )
    .ok()?;
    let newest = summary
        .as_array()?
        .iter()
        .filter(|o| o["observationType"] == "reguliereMeting")
        .max_by_key(|o| o["endDate"].as_str().and_then(day).unwrap_or(0))?;
    let id = newest["observationId"].as_str()?;
    // The flag must be `ja`; `true` is refused.
    let doc = get_text_retried(
        ctx,
        &format!("{BRO}/gm/gld/v1/objects/{gld}/observations/{id}?mostRecentTVP=ja"),
        "gld value",
    )
    .ok()?;
    xml::number(xml::first(&doc, "MeasurementTVP")?, "value")
}

pub fn run_groundwater(ctx: &Ctx) -> Outcome<DatasetResult> {
    let bbox = wgs84_bbox(ctx.bbox);
    let wells = collection(ctx, "gm_gmw", bbox)?;
    let tubes = collection(ctx, "gm_gmw_monitoringtube", bbox)?;
    let glds = collection(ctx, "gm_gld", bbox)?;
    ctx.log(format!(
        "{} putten, {} buizen, {} grondwaterstanddossiers",
        wells.len(),
        tubes.len(),
        glds.len()
    ));

    // Wells by their key: ground level and position.
    let mut well_at = std::collections::HashMap::new();
    for w in &wells {
        let p = &w["properties"];
        if !p["well_removal_date"].is_null() {
            continue;
        }
        let (Some(pk), Some(ground)) =
            (p["gm_gmw_pk"].as_i64(), p["ground_level_position"].as_f64())
        else {
            continue;
        };
        let c = &w["geometry"]["coordinates"];
        let (Some(lng), Some(lat)) = (c[0].as_f64(), c[1].as_f64()) else {
            continue;
        };
        well_at.insert(pk, (lat, lng, ground));
    }

    // Every tube of a standing well: blind tube to its screen, then the screen.
    let mut tube_well = std::collections::HashMap::new();
    let mut cells = Cells::default();
    for t in &tubes {
        let p = &t["properties"];
        let Some(&(lat, lng, ground)) = p["gm_gmw_fk"].as_i64().and_then(|k| well_at.get(&k))
        else {
            continue;
        };
        if let Some(pk) = p["gm_gmw_monitoringtube_pk"].as_i64() {
            tube_well.insert(pk, (lat, lng, ground));
        }
        let (Some(top), Some(bottom)) = (
            p["screen_top_position"].as_f64(),
            p["screen_bottom_position"].as_f64(),
        ) else {
            continue;
        };
        let (screen_top, screen_bottom) = ((ground - top) as f32, (ground - bottom) as f32);
        if screen_top < 0.0 || screen_bottom <= screen_top {
            continue;
        }
        cells.column(
            lat,
            lng,
            TUBE_RADIUS_M,
            vec![
                (0.0, screen_top, Kind::WellTube),
                (screen_top, screen_bottom, Kind::WellFilter),
            ],
        );
        cells.count("monitoring tubes");
    }

    // The latest level of every dossier measured in the last few years.
    let recent: Vec<&Value> = glds
        .iter()
        .filter(|g| {
            g["properties"]["research_last_date"]
                .as_str()
                .is_some_and(|d| d >= "2023-01-01")
        })
        .collect();
    let total = recent.len();
    let mut done = 0u64;
    in_order(
        ctx,
        &recent,
        ctx.upstream(),
        |g| {
            let id = g["properties"]["bro_id"].as_str().unwrap_or_default();
            latest_level(ctx, id)
        },
        |g, level| {
            done += 1;
            let tube = g["properties"]["gm_gmw_monitoringtube_fk"].as_i64();
            if let (Some(level), Some(&(lat, lng, ground))) =
                (level, tube.and_then(|k| tube_well.get(&k)))
            {
                let depth = (ground - level) as f32;
                // A level above the ground is artesian, or a datum slip; drawn at
                // the surface rather than in the air.
                cells.point(Kind::WaterTable, lat, lng, depth.max(0.0), WATER_MARK_M);
                cells.count("groundwater levels");
            }
            ctx.progress(Progress {
                phase: Some("grondwaterstanden".into()),
                done: Some(done),
                total: Some(Some(total as u64)),
            });
            Ok(())
        },
    )?;
    cells.write(
        ctx,
        "groundwater",
        GROUNDWATER_ATTRIBUTION,
        "BRO GMW and GLD (PDOK OGC API, publiek.broservices.nl)",
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_search_answer_yields_its_hits() {
        let text = r#"<x:dispatchDocument><BHR_GT_O><brocom:broId>BHR000000336330</brocom:broId>
<brocom:standardizedLocation srsName="urn:ogc:def:crs:EPSG::4258"><gml:pos>53.205910850 6.574489130</gml:pos></brocom:standardizedLocation>
<finalDepthBoring uom="m">2.00</finalDepthBoring></BHR_GT_O></x:dispatchDocument>"#;
        let h = hits(text);
        assert_eq!(h.len(), 1);
        assert_eq!(h[0].id, "BHR000000336330");
        assert!((h[0].lat - 53.20591085).abs() < 1e-9);
        assert!((h[0].lng - 6.57448913).abs() < 1e-9);
    }

    #[test]
    fn a_geotechnical_borehole_reads_its_layers() {
        let doc = r#"<bhrgtcom:layer><bhrgtcom:upperBoundary uom="m">0.00</bhrgtcom:upperBoundary><bhrgtcom:lowerBoundary uom="m">0.20</bhrgtcom:lowerBoundary><bhrgtcom:anthropogenic>ja</bhrgtcom:anthropogenic></bhrgtcom:layer>
<bhrgtcom:layer><bhrgtcom:upperBoundary uom="m">0.20</bhrgtcom:upperBoundary><bhrgtcom:lowerBoundary uom="m">1.50</bhrgtcom:lowerBoundary><bhrgtcom:anthropogenic>nee</bhrgtcom:anthropogenic><bhrgtcom:soil><bhrgtcom:geotechnicalSoilName>siltigZand</bhrgtcom:geotechnicalSoilName><bhrgtcom:sandMedianClass>fijn</bhrgtcom:sandMedianClass></bhrgtcom:soil></bhrgtcom:layer>
<bhrgtcom:layer><bhrgtcom:upperBoundary uom="m">1.50</bhrgtcom:upperBoundary><bhrgtcom:lowerBoundary uom="m">2.00</bhrgtcom:lowerBoundary><bhrgtcom:soil><bhrgtcom:geotechnicalSoilName>zwakZandigVeen</bhrgtcom:geotechnicalSoilName></bhrgtcom:soil></bhrgtcom:layer>"#;
        assert_eq!(
            borehole_layers(doc),
            vec![
                (0.0, 0.2, Kind::Anthropogenic),
                (0.2, 1.5, Kind::FineSand),
                (1.5, 2.0, Kind::Peat)
            ]
        );
    }

    #[test]
    fn a_soil_borehole_reads_its_first_component() {
        let doc = r#"<bhrcommon:soilLayer><bhrcommon:upperBoundary uom="m">0.000</bhrcommon:upperBoundary><bhrcommon:lowerBoundary uom="m">0.200</bhrcommon:lowerBoundary><bhrcommon:layerComponent><bhrcommon:soilType><bhrcommon:standardSoilName>zwakKleiigVeen</bhrcommon:standardSoilName></bhrcommon:soilType></bhrcommon:layerComponent></bhrcommon:soilLayer>"#;
        assert_eq!(borehole_layers(doc), vec![(0.0, 0.2, Kind::Peat)]);
    }

    #[test]
    fn dossier_dates_sort_by_day() {
        assert!(day("14-10-2025") > day("16-08-2025"));
        assert_eq!(day("bad"), None);
    }
}
