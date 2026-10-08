//! **The Groningen gas field** — its edge at the depth of the reservoir, the
//! wells drilled into it along their real paths, where each meets the
//! reservoir, and the earthquakes it has caused (`systems::underground`, layer
//! `gasfield`). Usually run over the province rather than the city: the field
//! lies north-east of Groningen and its earthquakes cluster around Loppersum.
//!
//! * **Earthquakes** — KNMI's FDSN event service, one request, pipe-separated
//!   text; only `induced or triggered event`s. **Depth is almost always the
//!   catalogue's fixed 3 km** (1,887 of 1,903 in the province), so the depth
//!   drawn is KNMI's convention, not a measurement. Size grows with magnitude.
//! * **Wells** — NLOG's GeoServer WFS for the surface positions, then NLOG's
//!   map-viewer API for each well's directional survey (true vertical depth and
//!   offsets east/north in metres from the rotary table, NAP datum) and its
//!   lithostratigraphy (along-hole depths). A well is drawn along its survey;
//!   where the preferred interpretation reaches a unit whose code starts `RO`
//!   (Rotliegend, the reservoir sandstone) a marker sits at that depth.
//! * **The field's edge** — the WFS's field polygons, drawn at the **median**
//!   reservoir top of that field's wells. The field layer itself has no depth,
//!   and TNO's national depth grid is ED50/UTM in a zip; the wells already say
//!   where the reservoir is.
//!
//! Depths are below NAP, which in Groningen is within a couple of metres of the
//! ground — nothing at three kilometres.
//!
//! Data: KNMI (CC BY 4.0); NLOG / TNO Geologische Dienst Nederland.

use std::collections::HashMap;

use serde_json::{json, Value};

use super::http::{get_json, get_text_retried, post_json, query};
use super::rd::wgs84_bbox;
use super::underground::Cells;
use super::{in_order, Ctx, DatasetResult, Outcome, PipelineError, Progress};
use cartopolis_geo::formats::underground::Kind;

const KNMI: &str = "https://rdsa.knmi.nl/fdsnws/event/1/query";
const NLOG_WFS: &str = "https://www.gdngeoservices.nl/geoserver/nlog/ows";
const NLOG_API: &str = "https://www.nlog.nl/nlog-mapviewer/rest/brh";
/// A well shallower than this is not a gas well.
const MIN_WELL_DEPTH_M: f64 = 500.0;
const WELL_RADIUS_M: f32 = 4.0;
const RESERVOIR_MARK_M: f32 = 30.0;
/// Most survey points kept per well; a straight hole needs few.
const MAX_SURVEY_POINTS: usize = 60;
const M_PER_DEG: f64 = 111_320.0;

const ATTRIBUTION: &str = "Earthquakes © KNMI (CC BY 4.0); gas field and wells © NLOG / TNO";

/// An earthquake's drawn radius: ten metres at magnitude zero, a hundred and
/// twenty at Huizinge's 3.6.
#[must_use]
pub fn quake_radius(magnitude: f64) -> f32 {
    (10.0 * 10f64.powf(0.3 * magnitude)).max(5.0) as f32
}

/// Induced earthquakes from KNMI's text answer: `(lat, lng, depth_m, magnitude)`.
pub fn parse_events(text: &str) -> Vec<(f64, f64, f32, f64)> {
    text.lines()
        .filter(|l| !l.starts_with('#'))
        .filter_map(|l| {
            let f: Vec<&str> = l.split('|').collect();
            if f.len() < 14 || f[13] != "induced or triggered event" {
                return None;
            }
            let lat = f[2].parse().ok()?;
            let lng = f[3].parse().ok()?;
            let depth = f[4].parse::<f64>().ok()? * 1000.0;
            let mag = f[10].parse().ok()?;
            Some((lat, lng, depth as f32, mag))
        })
        .collect()
}

/// A WFS layer's features in the area, as GeoJSON in WGS84 (lng, lat).
fn wfs(ctx: &Ctx, layer: &str, bbox: [f64; 4]) -> Outcome<Vec<Value>> {
    let [lng0, lat0, lng1, lat1] = bbox;
    let filter = format!("BBOX(GEOMETRY,{lng0},{lat0},{lng1},{lat1},'EPSG:4326')");
    let url = format!(
        "{NLOG_WFS}?{}",
        query(&[
            ("service", "WFS"),
            ("version", "2.0.0"),
            ("request", "GetFeature"),
            ("typeNames", layer),
            ("outputFormat", "application/json"),
            ("srsName", "EPSG:4326"),
            ("CQL_FILTER", &filter),
        ])
    );
    let doc = get_json(ctx, &url, layer)?;
    Ok(doc["features"].as_array().cloned().unwrap_or_default())
}

/// One survey point: along-hole depth, true vertical depth, east and north
/// offsets, all metres.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Station {
    pub ah: f64,
    pub tvd: f64,
    pub dx: f64,
    pub dy: f64,
}

/// The longest survey of a well.
pub fn survey(doc: &Value) -> Vec<Station> {
    doc["dirSurveys"]
        .as_array()
        .into_iter()
        .flatten()
        .map(|s| {
            s["dirSurveyPoints"]
                .as_array()
                .into_iter()
                .flatten()
                .filter_map(|p| {
                    Some(Station {
                        ah: p["ahDepth"].as_f64()?,
                        tvd: p["tvDepth"].as_f64()?,
                        dx: p["dx"].as_f64().unwrap_or(0.0),
                        dy: p["dy"].as_f64().unwrap_or(0.0),
                    })
                })
                .collect::<Vec<_>>()
        })
        .max_by_key(Vec::len)
        .unwrap_or_default()
}

/// Where the preferred interpretation first reaches the reservoir (a unit
/// coded `RO…`), along-hole metres.
pub fn reservoir_top_ah(doc: &Value) -> Option<f64> {
    let interpretations = doc["stratIntprts"].as_array()?;
    let preferred = interpretations
        .iter()
        .find(|i| i["preferredBln"] == "J")
        .or_else(|| interpretations.first())?;
    preferred["stratIntvals"]
        .as_array()?
        .iter()
        .filter(|v| {
            v["stratUnitId"]
                .as_str()
                .is_some_and(|u| u.starts_with("RO"))
        })
        .filter_map(|v| v["topDepth"].as_f64())
        .min_by(f64::total_cmp)
}

/// The station at an along-hole depth, interpolated.
#[must_use]
pub fn at_ah(stations: &[Station], ah: f64) -> Option<Station> {
    let i = stations.iter().position(|s| s.ah >= ah)?;
    if i == 0 {
        return stations.first().copied();
    }
    let (a, b) = (stations[i - 1], stations[i]);
    let t = if b.ah > a.ah {
        (ah - a.ah) / (b.ah - a.ah)
    } else {
        0.0
    };
    let lerp = |p: f64, q: f64| p + (q - p) * t;
    Some(Station {
        ah,
        tvd: lerp(a.tvd, b.tvd),
        dx: lerp(a.dx, b.dx),
        dy: lerp(a.dy, b.dy),
    })
}

fn offset(lat: f64, lng: f64, dx: f64, dy: f64) -> (f64, f64) {
    (
        lat + dy / M_PER_DEG,
        lng + dx / (M_PER_DEG * lat.to_radians().cos()),
    )
}

fn median(values: &mut [f32]) -> Option<f32> {
    if values.is_empty() {
        return None;
    }
    values.sort_by(f32::total_cmp);
    Some(values[values.len() / 2])
}

/// A well the WFS lists, with what its two API answers say.
struct Well {
    id: u64,
    lat: f64,
    lng: f64,
    /// Rotary table above NAP.
    reference_m: f64,
    field: String,
    end_tvd: f64,
}

pub fn run(ctx: &Ctx) -> Outcome<DatasetResult> {
    let bbox = wgs84_bbox(ctx.bbox);
    let [lng0, lat0, lng1, lat1] = bbox;
    let mut cells = Cells::default();

    // Earthquakes.
    let url = format!(
        "{KNMI}?starttime=1986-01-01&minlatitude={lat0}&maxlatitude={lat1}\
         &minlongitude={lng0}&maxlongitude={lng1}&format=text"
    );
    let events = parse_events(&get_text_retried(ctx, &url, "knmi")?);
    for &(lat, lng, depth, mag) in &events {
        cells.point(Kind::Earthquake, lat, lng, depth, quake_radius(mag));
        cells.count("earthquakes");
    }
    ctx.log(format!("{} geïnduceerde bevingen", events.len()));

    // Wells.
    let wells: Vec<Well> = wfs(ctx, "nlog:gdw_ng_wll_all_utm", bbox)?
        .iter()
        .filter_map(|f| {
            let p = &f["properties"];
            let id = p["URL"].as_str()?.rsplit('/').next()?.parse().ok()?;
            let c = &f["geometry"]["coordinates"];
            let end_tvd = p["END_DEPTH_MTVD"].as_f64()?;
            (end_tvd >= MIN_WELL_DEPTH_M).then(|| Well {
                id,
                lat: c[1].as_f64().unwrap_or_default(),
                lng: c[0].as_f64().unwrap_or_default(),
                reference_m: p["REFERENCE_LEVEL_HEIGHT"].as_f64().unwrap_or(0.0),
                field: p["FIELD_CODE"].as_str().unwrap_or_default().to_string(),
                end_tvd,
            })
        })
        .collect();
    ctx.log(format!("{} diepe putten", wells.len()));

    let mut reservoir_tops: HashMap<String, Vec<f32>> = HashMap::new();
    let total = wells.len();
    let mut done = 0u64;
    in_order(
        ctx,
        &wells,
        ctx.upstream().min(4),
        |w| {
            let body = json!(w.id);
            let s = post_json(ctx, &format!("{NLOG_API}/dirsurveys"), &body, "dirsurveys").ok();
            let t = post_json(
                ctx,
                &format!("{NLOG_API}/stratinterpretations"),
                &body,
                "strat",
            )
            .ok();
            (s, t)
        },
        |w, (s, t)| {
            done += 1;
            let mut stations = s.as_ref().map(survey).unwrap_or_default();
            if stations.len() < 2 {
                // No survey: straight down to its stated depth.
                stations = vec![
                    Station {
                        ah: 0.0,
                        tvd: 0.0,
                        dx: 0.0,
                        dy: 0.0,
                    },
                    Station {
                        ah: w.end_tvd,
                        tvd: w.end_tvd,
                        dx: 0.0,
                        dy: 0.0,
                    },
                ];
            }
            let below = |tvd: f64| (tvd - w.reference_m).max(0.0) as f32;
            let step = (stations.len() / MAX_SURVEY_POINTS).max(1);
            let mut kept: Vec<Station> = stations.iter().copied().step_by(step).collect();
            if kept.last() != stations.last() {
                kept.push(*stations.last().expect("two or more"));
            }
            let pts: Vec<(f64, f64)> = kept
                .iter()
                .map(|s| offset(w.lat, w.lng, s.dx, s.dy))
                .collect();
            let depths = kept.iter().map(|s| below(s.tvd)).collect();
            cells.line(Kind::GasWell, WELL_RADIUS_M, &pts, depths);
            cells.count("gas wells");
            if let Some(top) = t
                .as_ref()
                .and_then(reservoir_top_ah)
                .and_then(|ah| at_ah(&stations, ah))
            {
                let (la, ln) = offset(w.lat, w.lng, top.dx, top.dy);
                cells.point(Kind::GasField, la, ln, below(top.tvd), RESERVOIR_MARK_M);
                reservoir_tops
                    .entry(w.field.clone())
                    .or_default()
                    .push(below(top.tvd));
                cells.count("reservoir tops");
            }
            ctx.progress(Progress {
                phase: Some("putten".into()),
                done: Some(done),
                total: Some(Some(total as u64)),
            });
            Ok(())
        },
    )?;

    // Field edges at their wells' median reservoir top.
    let mut all: Vec<f32> = reservoir_tops.values().flatten().copied().collect();
    let fallback = median(&mut all);
    for f in wfs(ctx, "nlog:gdw_ng_field_utm", bbox)? {
        let code = f["properties"]["FIELD_CODE"].as_str().unwrap_or_default();
        let depth = reservoir_tops
            .get_mut(code)
            .and_then(|v| median(v))
            .or(fallback);
        let Some(depth) = depth else { continue };
        let g = &f["geometry"];
        let polygons: Vec<&Value> = match g["type"].as_str() {
            Some("Polygon") => vec![&g["coordinates"]],
            Some("MultiPolygon") => g["coordinates"].as_array().into_iter().flatten().collect(),
            _ => Vec::new(),
        };
        for poly in polygons {
            let Some(outer) = poly.get(0).and_then(Value::as_array) else {
                continue;
            };
            let mut ring: Vec<(f64, f64)> = outer
                .iter()
                .filter_map(|p| Some((p.get(1)?.as_f64()?, p.get(0)?.as_f64()?)))
                .collect();
            if ring.len() > 1 && ring.first() == ring.last() {
                ring.pop();
            }
            cells.ring(Kind::GasField, depth, &ring);
            cells.count("field outlines");
        }
    }

    if events.is_empty() && total == 0 {
        return Err(PipelineError::Failed(
            "no earthquakes and no wells in the area — wrong area, or a source is down".into(),
        ));
    }
    cells.write(
        ctx,
        "gasfield",
        ATTRIBUTION,
        "KNMI FDSN; NLOG WFS and map-viewer API",
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_induced_events_are_read() {
        let text = "#EventID|Time|Latitude|Longitude|Depth/km|Author|Catalog|Contributor|ContributorID|MagType|Magnitude|MagAuthor|EventLocationName|EventType\n\
knmi2026qmut|2026-08-21T03:50:39.0000|53.378|6.667|3.0|||KNMI|knmi2026qmut|MLn|2.75655436993489||Zandeweer|induced or triggered event\n\
knmi2026x|2026-08-21T03:50:39.0000|53.3|6.6|0.0|||KNMI|x|MLn|1.0||Ergens|controlled explosion\n";
        let e = parse_events(text);
        assert_eq!(e.len(), 1);
        assert_eq!(e[0].2, 3000.0);
        assert!((e[0].3 - 2.7565).abs() < 1e-3);
    }

    #[test]
    fn bigger_quakes_are_drawn_bigger() {
        assert!(quake_radius(3.6) > 100.0);
        assert!(quake_radius(0.0) >= 10.0 - 1e-3);
        assert_eq!(quake_radius(-2.0), 5.0);
    }

    #[test]
    fn the_reservoir_top_is_the_first_ro_unit_of_the_preferred_reading() {
        let doc = json!({"stratIntprts": [
            {"preferredBln": "N", "stratIntvals": [{"topDepth": 10.0, "stratUnitId": "ROCLT"}]},
            {"preferredBln": "J", "stratIntvals": [
                {"topDepth": 0.0, "stratUnitId": "NU"},
                {"topDepth": 2823.0, "stratUnitId": "ROSLU"},
                {"topDepth": 2763.0, "stratUnitId": "ROCLT"},
                {"topDepth": 2994.0, "stratUnitId": "DCCR"}]}]});
        assert_eq!(reservoir_top_ah(&doc), Some(2763.0));
    }

    #[test]
    fn a_survey_interpolates_along_the_hole() {
        let s = [
            Station {
                ah: 0.0,
                tvd: 0.0,
                dx: 0.0,
                dy: 0.0,
            },
            Station {
                ah: 100.0,
                tvd: 90.0,
                dx: 10.0,
                dy: 20.0,
            },
        ];
        let m = at_ah(&s, 50.0).unwrap();
        assert!((m.tvd - 45.0).abs() < 1e-9 && (m.dx - 5.0).abs() < 1e-9);
        assert_eq!(at_ah(&s, 500.0), None);
    }
}
