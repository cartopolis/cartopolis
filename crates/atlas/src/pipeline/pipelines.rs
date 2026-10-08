//! **Transport pipelines** for hazardous substances — Gasunie's high-pressure
//! gas network and the oil and chemical lines — under the ground
//! (`systems::underground`, layer `pipelines`).
//!
//! The Register Externe Veiligheid publishes them as one WFS layer,
//! `rev_public:bl_buisleidingreferentie`, answering a whole area in one request
//! (Groningen: 495 segments, 492 of them Gasunie gas). Fetched in RD, the
//! layer's own system, so no axis-order question arises.
//!
//! **Depth is mostly unregistered.** `bovenkantbuis`, the depth of the pipe's
//! top, is empty for every segment in Groningen (filled on ~46,000 elsewhere,
//! in centimetres as far as the values show). Where it is missing the pipe is
//! drawn at [`DEFAULT_COVER_M`], the cover a transport line is laid under; the
//! layer's note says so rather than the drawing pretending to know.
//!
//! Every number arrives as a string, and a segment crossing the area comes back
//! whole, reaching beyond it.
//!
//! Data: Register Externe Veiligheid (RIVM / IPO), rev-portaal.nl.

use serde_json::Value;

use super::http::{get_json, query};
use super::rd::rd_to_lat_lng;
use super::underground::Cells;
use super::{Ctx, DatasetResult, Outcome, PipelineError};
use cartopolis_geo::formats::underground::Kind;

const WFS: &str = "https://rev-portaal.nl/geoserver/wfs";
const LAYER: &str = "rev_public:bl_buisleidingreferentie";
/// Metres of cover over a pipe whose depth the register does not give.
pub const DEFAULT_COVER_M: f32 = 1.0;
const ATTRIBUTION: &str = "Transport pipelines © Register Externe Veiligheid (RIVM)";

fn text<'a>(p: &'a Value, key: &str) -> &'a str {
    p.get(key).and_then(Value::as_str).unwrap_or_default()
}

fn number(p: &Value, key: &str) -> Option<f64> {
    match p.get(key)? {
        Value::Number(n) => n.as_f64(),
        Value::String(s) => s.trim().parse().ok(),
        _ => None,
    }
}

/// What a pipe carries.
#[must_use]
pub fn kind_of(product: &str, substances: &str) -> Kind {
    let p = product.to_ascii_lowercase();
    let s = substances.to_ascii_lowercase();
    if p.contains("naturalgas") || s.contains("aardgas") {
        Kind::PipeGas
    } else if p.is_empty() && s.is_empty() {
        Kind::PipeOther
    } else {
        Kind::PipeChemical
    }
}

/// The depth of a pipe's centre: its registered top (cm) plus its radius, or
/// the default cover.
#[must_use]
pub fn centre_depth(top_cm: Option<f64>, radius_m: f32) -> f32 {
    let top = top_cm
        .filter(|v| (10.0..=3000.0).contains(v))
        .map_or(DEFAULT_COVER_M, |cm| (cm / 100.0) as f32);
    top + radius_m
}

/// A `LineString`/`MultiLineString`'s parts, in RD.
fn parts(geometry: &Value) -> Vec<Vec<[f64; 2]>> {
    let coords = &geometry["coordinates"];
    let as_line = |v: &Value| -> Vec<[f64; 2]> {
        v.as_array()
            .into_iter()
            .flatten()
            .filter_map(|p| Some([p.get(0)?.as_f64()?, p.get(1)?.as_f64()?]))
            .collect()
    };
    match geometry["type"].as_str() {
        Some("LineString") => vec![as_line(coords)],
        Some("MultiLineString") => coords
            .as_array()
            .into_iter()
            .flatten()
            .map(as_line)
            .collect(),
        _ => Vec::new(),
    }
}

pub fn run(ctx: &Ctx) -> Outcome<DatasetResult> {
    let [x0, y0, x1, y1] = ctx.bbox;
    let bbox = format!("{x0},{y0},{x1},{y1},urn:ogc:def:crs:EPSG::28992");
    let url = format!(
        "{WFS}?{}",
        query(&[
            ("service", "WFS"),
            ("version", "2.0.0"),
            ("request", "GetFeature"),
            ("typeNames", LAYER),
            ("outputFormat", "application/json"),
            ("srsName", "EPSG:28992"),
            ("bbox", &bbox),
        ])
    );
    let doc = get_json(ctx, &url, "buisleidingen")?;
    let features = doc["features"]
        .as_array()
        .ok_or_else(|| PipelineError::Failed("buisleidingen: no features array".into()))?;
    let mut cells = Cells::default();
    for f in features {
        let p = &f["properties"];
        let kind = kind_of(
            text(p, "buisleidingproduct"),
            text(p, "buisleidingcategoriestof"),
        );
        let radius = number(p, "diameter")
            .filter(|d| *d > 0.0)
            .map_or(0.15, |mm| (mm / 2000.0) as f32)
            .max(0.15);
        let depth = centre_depth(number(p, "bovenkantbuis"), radius);
        for part in parts(&f["geometry"]) {
            if part.len() < 2 {
                continue;
            }
            let pts: Vec<(f64, f64)> = part.iter().map(|&[x, y]| rd_to_lat_lng(x, y)).collect();
            let depths = vec![depth; pts.len()];
            cells.line(kind, radius, &pts, depths);
            cells.count(match kind {
                Kind::PipeGas => "gas pipeline segments",
                Kind::PipeChemical => "chemical pipeline segments",
                _ => "other pipeline segments",
            });
        }
    }
    cells.write(
        ctx,
        "pipelines",
        ATTRIBUTION,
        "REV bl_buisleidingreferentie (WFS)",
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn products_map_to_kinds() {
        assert_eq!(kind_of("naturalGas", "[\"aardgas\"]"), Kind::PipeGas);
        assert_eq!(kind_of("liquidHydrocarbon", ""), Kind::PipeChemical);
        assert_eq!(kind_of("", ""), Kind::PipeOther);
    }

    #[test]
    fn a_missing_top_takes_the_default_cover() {
        assert_eq!(centre_depth(None, 0.2), DEFAULT_COVER_M + 0.2);
        assert_eq!(centre_depth(Some(160.0), 0.2), 1.6 + 0.2);
        // A value that cannot be centimetres of cover is not believed.
        assert_eq!(centre_depth(Some(0.0), 0.2), DEFAULT_COVER_M + 0.2);
    }

    #[test]
    fn numbers_arrive_as_strings() {
        let p = json!({"diameter": "457.2", "x": 3});
        assert_eq!(number(&p, "diameter"), Some(457.2));
        assert_eq!(number(&p, "x"), Some(3.0));
        assert_eq!(number(&p, "missing"), None);
    }
}
