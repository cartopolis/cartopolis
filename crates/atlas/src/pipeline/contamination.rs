//! **Soil contamination** and the **underground tanks** that cause most of it
//! (`systems::underground`, layer `contamination`).
//!
//! Two sources, because neither covers a city like Groningen alone:
//!
//! * **Bodemloket** — the national register of contaminated sites (Wbb), as an
//!   ArcGIS layer with each site's outline and its assessment (`STATUS_OORD`,
//!   "Ernstig" or "Niet ernstig", and whether it is remediated). Drawn as an
//!   outline just under the ground. **A municipality may not deliver to it**:
//!   the City of Groningen does not (it keeps its own viewer, whose records
//!   carry no assessment), so over the city this source is empty.
//! * **The City of Groningen's tank register** — every underground storage
//!   tank it knows of, with whether it was cleaned up. A tank that was removed
//!   is not drawn; one that was cleaned up by filling it is still in the ground
//!   and is drawn in a paler colour.
//!
//! Both are ArcGIS services that page at 1–2,000 records with `resultOffset`
//! and say `exceededTransferLimit` while there is more.
//!
//! Data: Bodemloket (Rijkswaterstaat and the competent authorities); Gemeente
//! Groningen. Neither states a licence; both are published openly.

use serde_json::Value;

use super::http::{get_json, query};
use super::rd::wgs84_bbox;
use super::underground::Cells;
use super::{Ctx, DatasetResult, Outcome};
use cartopolis_geo::formats::underground::Kind;

const BODEMLOKET: &str =
    "https://gis.gdngeoservices.nl/standalone/rest/services/blk_gdn/lks_blk_rd_v1/MapServer/0";
const GRONINGEN_TANKS: &str = "https://services2.arcgis.com/chCSiGO4ORzXeSGk/arcgis/rest/services/Tankbestand/FeatureServer/0";
/// How far under the ground a site's outline is drawn.
const SITE_DEPTH_M: f32 = 0.5;
/// A domestic fuel tank lies about this deep, and is about this big.
const TANK_DEPTH_M: f32 = 1.5;
const TANK_RADIUS_M: f32 = 1.0;
const PAGE: usize = 1000;
const MAX_PAGES: usize = 500;

const ATTRIBUTION: &str = "Soil contamination © Bodemloket; underground tanks © Gemeente Groningen";

/// Every feature of an ArcGIS layer inside the area, as GeoJSON in WGS84.
fn arcgis(ctx: &Ctx, layer: &str, label: &str) -> Outcome<Vec<Value>> {
    let [lng0, lat0, lng1, lat1] = wgs84_bbox(ctx.bbox);
    let envelope = format!("{lng0},{lat0},{lng1},{lat1}");
    let mut out = Vec::new();
    for page in 0..MAX_PAGES {
        ctx.check()?;
        let offset = (page * PAGE).to_string();
        let count = PAGE.to_string();
        let url = format!(
            "{layer}/query?{}",
            query(&[
                ("where", "1=1"),
                ("geometry", &envelope),
                ("geometryType", "esriGeometryEnvelope"),
                ("inSR", "4326"),
                ("spatialRel", "esriSpatialRelIntersects"),
                ("outFields", "*"),
                ("outSR", "4326"),
                ("resultOffset", &offset),
                ("resultRecordCount", &count),
                ("f", "geojson"),
            ])
        );
        let doc = get_json(ctx, &url, label)?;
        let features = doc["features"].as_array().cloned().unwrap_or_default();
        let more = doc["properties"]["exceededTransferLimit"].as_bool() == Some(true)
            || doc["exceededTransferLimit"].as_bool() == Some(true);
        let n = features.len();
        out.extend(features);
        if !more || n == 0 {
            break;
        }
    }
    Ok(out)
}

/// A site's assessment as a kind: serious unless the register says otherwise.
#[must_use]
pub fn site_kind(status: &str) -> Kind {
    let s = status.to_ascii_lowercase();
    if s.contains("ernstig") && !s.contains("niet ernstig") {
        Kind::ContaminationSerious
    } else {
        Kind::Contamination
    }
}

/// A tank's status as a kind, or `None` for one that was removed.
#[must_use]
pub fn tank_kind(status: &str) -> Option<Kind> {
    let s = status.to_ascii_lowercase();
    if s.contains("gevuld") {
        Some(Kind::FuelTankFilled)
    } else if s.contains("niet gesaneerd") || s.trim().is_empty() {
        Some(Kind::FuelTank)
    } else {
        None
    }
}

/// The outer rings of a `Polygon`/`MultiPolygon`, as `(lat, lng)`.
fn outer_rings(geometry: &Value) -> Vec<Vec<(f64, f64)>> {
    let ring = |r: &Value| -> Vec<(f64, f64)> {
        r.as_array()
            .into_iter()
            .flatten()
            .filter_map(|p| Some((p.get(1)?.as_f64()?, p.get(0)?.as_f64()?)))
            .collect()
    };
    let c = &geometry["coordinates"];
    match geometry["type"].as_str() {
        Some("Polygon") => c.get(0).map(ring).into_iter().collect(),
        Some("MultiPolygon") => c
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(|poly| poly.get(0).map(ring))
            .collect(),
        _ => Vec::new(),
    }
}

pub fn run(ctx: &Ctx) -> Outcome<DatasetResult> {
    let mut cells = Cells::default();

    for site in arcgis(ctx, BODEMLOKET, "bodemloket")? {
        let p = &site["properties"];
        let kind = site_kind(p["STATUS_OORD"].as_str().unwrap_or_default());
        for mut ring in outer_rings(&site["geometry"]) {
            // GeoJSON closes its rings; the format's rings are open.
            if ring.len() > 1 && ring.first() == ring.last() {
                ring.pop();
            }
            cells.ring(kind, SITE_DEPTH_M, &ring);
            cells.count(if kind == Kind::ContaminationSerious {
                "serious contamination sites"
            } else {
                "other contamination sites"
            });
        }
    }

    for tank in arcgis(ctx, GRONINGEN_TANKS, "tanks")? {
        let p = &tank["properties"];
        let Some(kind) = tank_kind(p["STATUS"].as_str().unwrap_or_default()) else {
            continue;
        };
        let c = &tank["geometry"]["coordinates"];
        let (Some(lng), Some(lat)) = (c[0].as_f64(), c[1].as_f64()) else {
            continue;
        };
        cells.point(kind, lat, lng, TANK_DEPTH_M, TANK_RADIUS_M);
        cells.count("underground tanks");
    }

    cells.write(
        ctx,
        "contamination",
        ATTRIBUTION,
        "Bodemloket Wbb; Gemeente Groningen Tankbestand",
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn assessments_map_to_kinds() {
        assert_eq!(
            site_kind("Potentieel Ernstig en Urgent"),
            Kind::ContaminationSerious
        );
        assert_eq!(site_kind("Niet ernstig"), Kind::Contamination);
        assert_eq!(site_kind(""), Kind::Contamination);
    }

    #[test]
    fn a_removed_tank_is_not_drawn() {
        assert_eq!(tank_kind("Niet gesaneerd"), Some(Kind::FuelTank));
        assert_eq!(
            tank_kind("Gesaneerd, gevuld met Ecopearl"),
            Some(Kind::FuelTankFilled)
        );
        assert_eq!(tank_kind("Gesaneerd"), None);
    }

    #[test]
    fn outer_rings_come_back_as_lat_lng() {
        let g = json!({"type": "Polygon", "coordinates": [[[6.5, 53.2], [6.6, 53.2], [6.6, 53.3], [6.5, 53.2]], [[0.0, 0.0]]]});
        let rings = outer_rings(&g);
        assert_eq!(rings.len(), 1);
        assert_eq!(rings[0][0], (53.2, 6.5));
    }
}
