//! The **tall structures** — wind turbines, windmills, masts, towers, chimneys,
//! water towers and cranes — pre-cut into the z11 cells the client's tall lane
//! streams (`systems::tall_structures` reads them).
//!
//! The client used to ask Overpass for every one of those cells itself, and on a
//! cold start that made this lane the last thing to finish: timed on the Steam
//! Deck at 600 m over Groningen (2026-09-26) it landed at 24.3 s, ~8 s after
//! everything else, because each of its 25 cells is a live query costing 0.7–2.6 s
//! of server time on an Overpass instance every other user shares, paced four in
//! flight — and a query already running for somebody else is answered with
//! `duplicate_query`, an HTML page, rather than waited for. Everything else the
//! client streams is a static file this crate wrote; now this is too.
//!
//! **The file is the Overpass answer itself**, for the same query the client
//! would have sent (`cartopolis_geo::tall::query`, shared so the two cannot
//! drift): an `out center` document, nodes with `lat`/`lon`, ways with a
//! `center`, tags as Overpass gave them. The client parses it with the code it
//! already had and caches it under the key it already used, so which source
//! answered is invisible past the fetch.
//!
//! **An HTML body is never a result.** Overpass reports `duplicate_query`,
//! `rate_limited` and timeouts as an HTML page, sometimes under a 200, and a
//! runtime error as a JSON `remark` beside a partial element list. Either written
//! to disk would be served as an authoritative empty (or half-empty) cell for as
//! long as the file stood, so every answer is validated before it is written, a
//! refused one is evicted from the source cache, and the cell is asked again
//! after a doubling backoff. A cell that never answers is left out of the
//! coverage, so the client asks Overpass for it as it always did.
//!
//! The area is grown by the client's ring ([`tall::RING`]) so that a client
//! standing anywhere inside it finds its whole ring pre-cut, not just the cell
//! under it.
//!
//! Data: © OpenStreetMap contributors, ODbL.

use std::time::Duration;

use cartopolis_core::world::lat_lng_to_tile;
use cartopolis_geo::tall;
use serde_json::{json, Value};

use super::http;
use super::rd::wgs84_bbox;
use super::tile::{sorted_cells, Cell};
use super::{in_order, Count, Ctx, DatasetResult, Outcome, PipelineError, Progress};

/// Our own Overpass, the one the client defaults to. `ATLAS_OVERPASS_URL`
/// overrides it, as `ATLAS_VECTOR_TILES` does the tile server.
pub const DEFAULT_OVERPASS: &str = "https://overpass.cartopolis.org/api/interpreter";

const ATTRIBUTION: &str = "Tall structures © OpenStreetMap contributors (ODbL)";

/// Queries kept in flight. Below [`super::UPSTREAM_FETCHES`] on purpose: the
/// server is shared with every client still asking it for the rest of the map,
/// and each query costs it up to a few seconds of work.
const OVERPASS_FETCHES: usize = 2;
/// Attempts per cell, and the wait after the first refusal, doubled each time —
/// 5 + 10 + 20 + 40 s, long enough for a `duplicate_query` somebody else is
/// running to finish, or a rate limit to lift.
const ATTEMPTS: u32 = 5;
const BACKOFF: Duration = Duration::from_secs(5);
/// Per request. The query asks the server for at most 25 s of work.
const TIMEOUT: Duration = Duration::from_secs(90);
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

/// A GET, so the URL alone names the answer and the source cache can key on it.
fn cell_url(endpoint: &str, (x, y): Cell) -> String {
    format!(
        "{endpoint}?{}",
        http::query(&[("data", &tall::query(x, y))])
    )
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

type Get<'a> = dyn Fn(&str) -> Result<Vec<u8>, String> + Sync + 'a;

/// One cell's document, asked until it validates: a transport error, an error
/// status (429, 504) and a refused body are all retried after the doubling
/// backoff. A refused body is evicted from the source cache first, so the retry
/// — and next month's run — ask the server rather than read it back.
fn fetch_cell(ctx: &Ctx, url: &str, get: &Get<'_>, backoff: Duration) -> Outcome<Value> {
    let mut last = String::new();
    for attempt in 0..ATTEMPTS {
        ctx.check()?;
        match get(url).and_then(|body| validate(&body)) {
            Ok(doc) => return Ok(doc),
            Err(e) => {
                ctx.check()?;
                http::evict(ctx, url);
                last = e;
                if attempt + 1 < ATTEMPTS {
                    ctx.sleep(backoff * (1 << attempt))?;
                }
            }
        }
    }
    Err(PipelineError::Failed(last))
}

pub fn run(ctx: &Ctx) -> Outcome<DatasetResult> {
    let endpoint = overpass_url();
    let get = |url: &str| http::cached(ctx, url, || http::fetch(url, TIMEOUT));
    extract(ctx, &endpoint, &get, BACKOFF)
}

fn extract(ctx: &Ctx, endpoint: &str, get: &Get<'_>, backoff: Duration) -> Outcome<DatasetResult> {
    let cells = area_cells(ctx.bbox);
    let total = cells.len();
    ctx.log(format!("{total} z11-cellen via {endpoint}"));
    let (mut written, mut failed, mut structures) = (Vec::new(), 0usize, 0usize);
    in_order(
        ctx,
        &cells,
        OVERPASS_FETCHES.min(ctx.upstream()),
        |&cell| fetch_cell(ctx, &cell_url(endpoint, cell), get, backoff),
        |&cell, answer| {
            match answer {
                Ok(doc) => {
                    structures += doc["elements"].as_array().map_or(0, Vec::len);
                    ctx.write_json(&cell_path(cell), &doc)?;
                    written.push(cell);
                }
                Err(PipelineError::Cancelled) => return Err(PipelineError::Cancelled),
                Err(PipelineError::Failed(e)) => {
                    failed += 1;
                    let (x, y) = cell;
                    ctx.log(format!(
                        "{x}/{y}: {e} — niet gedekt, de client vraagt Overpass zelf"
                    ));
                }
            }
            ctx.progress(Progress {
                phase: Some("cellen".into()),
                done: Some((written.len() + failed) as u64),
                total: Some(Some(total as u64)),
            });
            Ok(())
        },
    )?;
    // Nothing answered is an outage, not an area with no towers: keep whatever
    // coverage an earlier run published rather than replace it with none.
    if written.is_empty() {
        return Err(PipelineError::Failed(format!(
            "no cell could be fetched from {endpoint}; the coverage was left as it was"
        )));
    }
    ctx.write_json(
        "tall/coverage.json",
        &json!({
            "attribution": ATTRIBUTION,
            "bbox_rd": ctx.bbox,
            "cells": sorted_cells(written.iter().copied()),
            "name": ctx.name,
            "source": "OpenStreetMap via Overpass",
            "structure_count": structures,
            "zoom": tall::ZOOM,
        }),
    )?;
    ctx.log(format!(
        "{structures} hoge bouwwerken over {} cellen, {failed} mislukt",
        written.len()
    ));
    Ok(DatasetResult {
        cells: written.len(),
        counts: vec![Count::new("tall structures", structures)],
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashSet;
    use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
    use std::sync::{Arc, Mutex};

    struct Quiet;
    impl crate::pipeline::Sink for Quiet {
        fn log(&self, _: String) {}
        fn progress(&self, _: Progress) {}
    }

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

    fn ctx(out: &std::path::Path) -> Ctx {
        Ctx::new(
            GRONINGEN,
            "groningen",
            out,
            Arc::new(AtomicBool::new(false)),
            Arc::new(Quiet),
        )
    }

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

    /// A `duplicate_query` page is retried, never written; the answer that
    /// follows it is, and it reads back as the Overpass document it was.
    #[test]
    fn an_error_page_is_retried_and_only_the_answer_is_written() {
        let dir = tempfile::tempdir().unwrap();
        let c = ctx(dir.path());
        let asked: Mutex<HashSet<String>> = Mutex::new(HashSet::new());
        let refusals = AtomicUsize::new(0);
        // Each cell's first request is refused; its second is answered.
        let get = |url: &str| {
            if asked.lock().unwrap().insert(url.to_string()) {
                refusals.fetch_add(1, Ordering::SeqCst);
                Ok(DUPLICATE_QUERY.as_bytes().to_vec())
            } else {
                Ok(ANSWER.as_bytes().to_vec())
            }
        };
        let result = extract(
            &c,
            "https://o.invalid/api/interpreter",
            &get,
            Duration::ZERO,
        )
        .unwrap();
        let cells = area_cells(GRONINGEN);
        assert_eq!(result.cells, cells.len());
        assert_eq!(refusals.load(Ordering::SeqCst), cells.len());

        let markt = lat_lng_to_tile(53.2194, 6.5665, tall::ZOOM);
        let body = std::fs::read(dir.path().join(cell_path(markt))).unwrap();
        assert!(!String::from_utf8_lossy(&body).contains("html"));
        let doc: Value = serde_json::from_slice(&body).unwrap();
        assert!(doc.get("remark").is_none());
        let elements = doc["elements"].as_array().unwrap();
        assert_eq!(elements.len(), 2);
        assert_eq!(elements[0]["lat"], 53.3);
        assert_eq!(elements[0]["lon"], 6.5);
        assert_eq!(elements[0]["tags"]["generator:source"], "wind");
        assert_eq!(elements[1]["center"]["lat"], 53.31);
        assert_eq!(elements[1]["tags"]["man_made"], "windmill");

        let coverage: Value =
            serde_json::from_slice(&std::fs::read(dir.path().join("tall/coverage.json")).unwrap())
                .unwrap();
        assert_eq!(coverage["zoom"], 11);
        assert_eq!(coverage["cells"].as_array().unwrap().len(), cells.len());
        assert_eq!(coverage["structure_count"], 2 * cells.len());
    }

    /// A cell that only ever answers with an error page is left out: no file,
    /// and not in the coverage, so the client asks Overpass for it itself.
    #[test]
    fn a_cell_that_never_answers_is_not_covered() {
        let dir = tempfile::tempdir().unwrap();
        let c = ctx(dir.path());
        let markt = lat_lng_to_tile(53.2194, 6.5665, tall::ZOOM);
        let bad = cell_url("https://o.invalid/i", markt);
        let get = |url: &str| {
            Ok(if url == bad { DUPLICATE_QUERY } else { ANSWER }
                .as_bytes()
                .to_vec())
        };
        let result = extract(&c, "https://o.invalid/i", &get, Duration::ZERO).unwrap();
        assert_eq!(result.cells, area_cells(GRONINGEN).len() - 1);
        assert!(!dir.path().join(cell_path(markt)).exists());
        let coverage: Value =
            serde_json::from_slice(&std::fs::read(dir.path().join("tall/coverage.json")).unwrap())
                .unwrap();
        let (mx, my) = markt;
        assert!(!coverage["cells"]
            .as_array()
            .unwrap()
            .contains(&json!([mx, my])));
    }

    /// An outage writes nothing — least of all an empty coverage over the last
    /// good one.
    #[test]
    fn an_outage_leaves_the_coverage_alone() {
        let dir = tempfile::tempdir().unwrap();
        let c = ctx(dir.path());
        let get = |_: &str| Err("HTTP 504".to_string());
        assert!(extract(&c, "https://o.invalid/i", &get, Duration::ZERO).is_err());
        assert!(!dir.path().join("tall").exists());
    }

    /// Against the real server, three cells around Groningen — the city's own and
    /// two turbine-heavy neighbours. Network, so ignored by default:
    /// `cargo test -p cartopolis_atlas live_overpass -- --ignored --nocapture`.
    #[test]
    #[ignore = "asks the live Overpass"]
    fn live_overpass_cells_validate_and_write() {
        let dir = tempfile::tempdir().unwrap();
        let c = ctx(dir.path()).with_data_dir(dir.path());
        let endpoint = overpass_url();
        let get = |url: &str| http::cached(&c, url, || http::fetch(url, TIMEOUT));
        let markt = lat_lng_to_tile(53.2194, 6.5665, tall::ZOOM);
        for cell in [markt, (markt.0 + 1, markt.1 - 1), (markt.0 + 1, markt.1)] {
            let started = std::time::Instant::now();
            let doc = fetch_cell(&c, &cell_url(&endpoint, cell), &get, BACKOFF).unwrap();
            c.write_json(&cell_path(cell), &doc).unwrap();
            let back: Value =
                serde_json::from_slice(&std::fs::read(dir.path().join(cell_path(cell))).unwrap())
                    .unwrap();
            let elements = back["elements"].as_array().unwrap();
            let located = elements
                .iter()
                .filter(|e| e.get("lat").is_some() || e.get("center").is_some())
                .count();
            println!(
                "{cell:?}: {} elements, {located} located, {} bytes, {:.1} s",
                elements.len(),
                std::fs::metadata(dir.path().join(cell_path(cell)))
                    .unwrap()
                    .len(),
                started.elapsed().as_secs_f64()
            );
            assert!(!elements.is_empty());
            assert_eq!(located, elements.len());
        }
    }
}
