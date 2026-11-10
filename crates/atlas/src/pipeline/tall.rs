//! The **tall structures** — wind turbines, windmills, masts, towers, chimneys,
//! water towers and cranes — pre-cut into the z11 cells the client's tall lane
//! streams (`systems::tall_structures` reads them).
//!
//! The client used to ask Overpass for every one of those cells itself, and on a
//! cold start that made this lane the last thing to finish: timed on the Steam
//! Deck at 600 m over Groningen (2026-09-26) it landed at 24.3 s, ~8 s after
//! everything else, because each of its 25 cells was a live query costing
//! 0.7–2.6 s of server time. Everything else the client streams is a static file
//! this crate wrote; now this is too.
//!
//! **The file is the Overpass answer itself**, for the same query the client
//! would have sent (`cartopolis_geo::tall::query`): an `out center` document,
//! nodes with `lat`/`lon`, ways with a `center`. Since 2026-11 it is cut from the
//! OpenStreetMap extract (`crate::osm`, `osm::layers::tall`) rather than fetched
//! from Overpass, so building it needs no Overpass server.
//!
//! The area is grown by the client's ring ([`tall::RING`]) so that a client
//! standing anywhere inside it finds its whole ring pre-cut, not just the cell
//! under it.
//!
//! Data: © OpenStreetMap contributors, ODbL.

use cartopolis_core::world::lat_lng_to_tile;
use cartopolis_geo::tall;
use serde_json::Value;

use super::rd::wgs84_bbox;
use super::tile::Cell;
use super::{Ctx, DatasetResult, Outcome};

/// Our own Overpass. `ATLAS_OVERPASS_URL` overrides it. Still asked by
/// `backdrop`, `registers` and `bag`, until they read the extract too.
pub const DEFAULT_OVERPASS: &str = "https://overpass.cartopolis.org/api/interpreter";

const ATTRIBUTION: &str = "Tall structures © OpenStreetMap contributors (ODbL)";

/// RD metres the area is padded by before it is binned: the client picks its
/// centre cell from a *rounded* base-zoom tile index, which can land one z14
/// tile (~1.5 km) past where the camera actually stands.
const EDGE_PAD_M: f64 = 1_500.0;

/// `ATLAS_OVERPASS_URL`, or [`DEFAULT_OVERPASS`].
#[must_use]
pub fn overpass_url() -> String {
    std::env::var("ATLAS_OVERPASS_URL").unwrap_or_else(|_| DEFAULT_OVERPASS.to_string())
}

/// Every z11 cell a client standing anywhere in `bbox_rd` asks for: the cells
/// the (padded) area touches, grown by the client's ring on every side, in row
/// order.
#[must_use]
pub fn area_cells(bbox_rd: [f64; 4]) -> Vec<Cell> {
    let [xmin, ymin, xmax, ymax] = bbox_rd;
    let [west, south, east, north] = wgs84_bbox([
        xmin - EDGE_PAD_M,
        ymin - EDGE_PAD_M,
        xmax + EDGE_PAD_M,
        ymax + EDGE_PAD_M,
    ]);
    let (x0, y0) = lat_lng_to_tile(north, west, tall::ZOOM);
    let (x1, y1) = lat_lng_to_tile(south, east, tall::ZOOM);
    let mut cells = Vec::new();
    for y in y0 - tall::RING..=y1 + tall::RING {
        for x in x0 - tall::RING..=x1 + tall::RING {
            cells.push((x, y));
        }
    }
    cells
}

/// `tall/11/{x}/{y}.json` — where the client looks.
#[must_use]
pub fn cell_path((x, y): Cell) -> String {
    format!("tall/{}/{x}/{y}.json", tall::ZOOM)
}

/// The answer as a document worth writing, or why it is not one.
pub fn validate(body: &[u8]) -> Result<Value, String> {
    let text = std::str::from_utf8(body).map_err(|_| "not UTF-8".to_string())?;
    if text.trim_start().starts_with('<') {
        return Err(format!("an HTML error page ({})", html_reason(text)));
    }
    let doc: Value = serde_json::from_str(text).map_err(|e| format!("not JSON: {e}"))?;
    if let Some(remark) = doc.get("remark").and_then(Value::as_str) {
        return Err(format!("remark: {}", remark.trim()));
    }
    if !doc.get("elements").is_some_and(Value::is_array) {
        return Err("no elements".into());
    }
    Ok(doc)
}

/// Which of Overpass's error pages this is, for the log line.
fn html_reason(text: &str) -> &'static str {
    const KNOWN: [&str; 5] = [
        "duplicate_query",
        "rate_limited",
        "timeout",
        "Too Many Requests",
        "out of memory",
    ];
    KNOWN
        .into_iter()
        .find(|k| text.contains(k))
        .unwrap_or("unrecognised")
}

pub fn run(ctx: &Ctx) -> Outcome<DatasetResult> {
    let cells = area_cells(ctx.bbox);
    crate::osm::build_cells(
        ctx,
        "tall",
        tall::ZOOM,
        &cells,
        &crate::osm::layers::tall::WANTED,
        crate::osm::layers::tall::cell,
        ATTRIBUTION,
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    use std::collections::HashSet;
    const GRONINGEN: [f64; 4] = [229_000.0, 578_000.0, 238_000.0, 587_000.0];

    /// What `overpass.cartopolis.org` answers when the same query is already
    /// running, verbatim but for whitespace.
    const DUPLICATE_QUERY: &str = r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE html PUBLIC "-//W3C//DTD XHTML 1.0 Strict//EN" "http://www.w3.org/TR/xhtml1/DTD/xhtml1-strict.dtd">
<html xmlns="http://www.w3.org/1999/xhtml" xml:lang="en" lang="en">
<head><meta http-equiv="content-type" content="text/html; charset=utf-8" lang="en"/><title>OSM3S Response</title></head>
<body>
<p>The data included in this document is from www.openstreetmap.org. The data is made available under ODbL.</p>
<p><strong style="color:#FF0000">Error</strong>: runtime error: open64: 0 Success /osm3s_osm_base Dispatcher_Client::request_read_and_idx::duplicate_query </p>
</body>
</html>"#;

    const ANSWER: &str = r#"{"version":0.6,"generator":"Overpass API","osm3s":{"timestamp_osm_base":"2026-09-26T08:00:00Z","copyright":"ODbL"},"elements":[
        {"type":"node","id":1,"lat":53.3,"lon":6.5,"tags":{"power":"generator","generator:source":"wind","height:hub":"98"}},
        {"type":"way","id":2,"center":{"lat":53.31,"lon":6.51},"nodes":[1,2,3],"tags":{"man_made":"windmill"}}
    ]}"#;

    /// **Every cell of every ring a client inside the area can stand at is
    /// covered** — the corners' cells, grown by the client's ring on each side.
    #[test]
    fn the_area_is_grown_by_the_client_s_ring() {
        let cells: HashSet<Cell> = area_cells(GRONINGEN).into_iter().collect();
        let [xmin, ymin, xmax, ymax] = GRONINGEN;
        for (x, y) in [(xmin, ymin), (xmax, ymin), (xmin, ymax), (xmax, ymax)] {
            let (lat, lng) = super::super::rd::rd_to_lat_lng(x, y);
            let (cx, cy) = lat_lng_to_tile(lat, lng, tall::ZOOM);
            for dy in -tall::RING..=tall::RING {
                for dx in -tall::RING..=tall::RING {
                    assert!(
                        cells.contains(&(cx + dx, cy + dy)),
                        "{:?}",
                        (cx + dx, cy + dy)
                    );
                }
            }
        }
        // The Grote Markt's own ring, and not a continent: Groningen spans one or
        // three z11 cells a side once padded, so the grown block is five to seven, and
        // it is a block — no holes, no duplicates.
        let markt = lat_lng_to_tile(53.2194, 6.5665, tall::ZOOM);
        assert!(cells.contains(&markt));
        let (xs, ys): (Vec<i64>, Vec<i64>) = cells.iter().copied().unzip();
        let w = xs.iter().max().unwrap() - xs.iter().min().unwrap() + 1;
        let h = ys.iter().max().unwrap() - ys.iter().min().unwrap() + 1;
        assert!((5..=7).contains(&w) && (5..=7).contains(&h), "{w}×{h}");
        assert_eq!(cells.len() as i64, w * h);
        assert_eq!(area_cells(GRONINGEN).len(), cells.len());
    }

    #[test]
    fn an_error_page_or_a_remark_is_refused_and_an_answer_is_not() {
        let e = validate(DUPLICATE_QUERY.as_bytes()).unwrap_err();
        assert!(e.contains("duplicate_query"), "{e}");
        assert!(validate(b"<html><body>rate_limited</body></html>")
            .unwrap_err()
            .contains("rate_limited"));
        assert!(validate(br#"{"elements":[],"remark":"runtime error: Query timed out"}"#).is_err());
        assert!(validate(br#"{"version":0.6}"#).is_err());
        assert_eq!(
            validate(ANSWER.as_bytes()).unwrap()["elements"]
                .as_array()
                .unwrap()
                .len(),
            2
        );
    }
}
