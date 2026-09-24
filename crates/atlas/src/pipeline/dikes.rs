//! The **flood defences** — dikes, dunes and dams — as per-cell lines the client
//! can name (`dikes.rs` in the client reads them).
//!
//! Ported from the earlier TypeScript pipeline (`dikes.ts`, itself from `tools/dikes.py`).
//! The relief is already in the height model; what this adds is the *knowing* —
//! that the ridge is the Lauwersmeerdijk and what it holds back. The water boards
//! register every defence and PDOK republishes them as IMWA; the `waterkering`
//! collection is a line per defence with a name, a type and a category.
//!
//! The API pages by opaque cursor with no total, so a page ceiling is the runaway
//! guard. `statusobject` is the filter: a planned defence is not one you can stand
//! on.
//!
//! Wire format per cell — `names` interned per cell because one defence arrives as
//! many segments under one name; `lines` is `[type, category, name_index,
//! [[lat, lng], …]]` with `name_index` −1 for an unnamed one; `crests`, by the
//! same index, is the measured crest in metres NAP, or `null` for a line that
//! carries no label or stands no crest (a lock, a pumping station).
//!
//! Data: IMWA Keringen — the Dutch water boards via PDOK, open data.

use std::collections::{BTreeMap, HashMap};

use serde_json::{json, Value};

use super::geom::{round, Pt, M_PER_DEG};
use super::http::{get_json, query};
use super::rd::wgs84_bbox;
use super::tile::{cell_path, lat_lng_to_cell, sorted_cells, Cell, CACHE_ZOOM};
use super::{in_order, Count, Ctx, DatasetResult, Outcome, PipelineError, Progress};

const OGC_BASE: &str = "https://api.pdok.nl/hwh/waterschappen-keringen-imwa/ogc/v1";
const COLLECTION: &str = "waterkering";
/// Features per page; the service answers with a `next` cursor.
const PAGE: usize = 1_000;
/// Pages before the walk gives up, since there is no total to check it against.
const MAX_PAGES: usize = 400;

const ATTRIBUTION: &str = "Flood defences © the Dutch water boards (IMWA) via PDOK (open data)";

/// Defence forms, in wire order. Mirrored by `DIKE_KINDS` in the client —
/// **append, never renumber**.
const DIKE_KINDS: [&str; 6] = ["dike", "dune", "dam", "high_ground", "structure", "unknown"];
/// `categoriewaterkering`, in wire order — how much country stands behind it.
const DIKE_CATEGORIES: [&str; 3] = ["primary", "regional", "other"];

/// Metres. A route to be named, not an outline to be filled, with vertices ~18 m
/// apart — one metre strips the collinear runs along a straight sea wall.
const SIMPLIFY_M: f64 = 1.0;
/// Shortest piece worth keeping: below this it is the stub where a line clipped a
/// cell corner.
const MIN_LENGTH_M: f64 = 5.0;

/// `typewaterkering` → our form. Empty on about one defence in five, and an
/// untyped one is still a defence, so it maps to `unknown` rather than being
/// dropped. `Kunstwerk` is a lock or pumping station standing in the line.
fn kind_of(t: &str) -> &'static str {
    match t {
        "Dijk" => "dike",
        "Duin" => "dune",
        "Dam" => "dam",
        "Hoge grond" => "high_ground",
        "Kunstwerk" => "structure",
        _ => "unknown",
    }
}

fn category_of(c: &str) -> &'static str {
    match c {
        "primair" => "primary",
        "regionaal" => "regional",
        _ => "other",
    }
}

/// Douglas-Peucker measuring to the **segment**, not its infinite line: an open
/// polyline that doubles back would otherwise have its far vertices measured
/// against a line they are nowhere near.
#[must_use]
pub fn simplify_segment(points: &[Pt], tolerance: f64) -> Vec<Pt> {
    if points.len() < 3 {
        return points.to_vec();
    }
    let mut keep = vec![false; points.len()];
    keep[0] = true;
    keep[points.len() - 1] = true;
    let mut stack = vec![(0usize, points.len() - 1)];
    while let Some((lo, hi)) = stack.pop() {
        if hi <= lo + 1 {
            continue;
        }
        let [ax, ay] = points[lo];
        let [bx, by] = points[hi];
        let (dx, dy) = (bx - ax, by - ay);
        let span2 = dx * dx + dy * dy;
        let mut worst = tolerance;
        let mut pick = None;
        for (i, &[px, py]) in points.iter().enumerate().take(hi).skip(lo + 1) {
            let d = if span2 == 0.0 {
                (px - ax).hypot(py - ay)
            } else {
                let t = (((px - ax) * dx + (py - ay) * dy) / span2).clamp(0.0, 1.0);
                (px - (ax + t * dx)).hypot(py - (ay + t * dy))
            };
            if d > worst {
                worst = d;
                pick = Some(i);
            }
        }
        if let Some(p) = pick {
            keep[p] = true;
            stack.push((lo, p));
            stack.push((p, hi));
        }
    }
    points
        .iter()
        .zip(keep)
        .filter(|(_, k)| *k)
        .map(|(p, _)| *p)
        .collect()
}

/// Ground length of a `[lat, lng]` polyline, in metres.
#[must_use]
pub fn length_m(points: &[Pt]) -> f64 {
    points
        .windows(2)
        .map(|w| {
            let ([lat1, lng1], [lat2, lng2]) = (w[0], w[1]);
            let scale = ((lat1 + lat2) * 0.5).to_radians().cos();
            ((lng2 - lng1) * scale).hypot(lat2 - lat1) * M_PER_DEG
        })
        .sum()
}

/// Cut a `[lat, lng]` polyline into per-cell pieces that join: every piece
/// **repeats the vertex where the cell changed**, so a line crossing a province is
/// drawn in every cell it passes through with no gap at an edge — not handed whole
/// to the cell of its first vertex, as the other extractors do.
#[must_use]
pub fn split_by_cell(points: &[Pt]) -> Vec<(Cell, Vec<Pt>)> {
    let mut out = Vec::new();
    let mut current: Option<Cell> = None;
    let mut piece: Vec<Pt> = Vec::new();
    for &pt in points {
        let cell = lat_lng_to_cell(pt[0], pt[1]);
        match current {
            None => {
                current = Some(cell);
                piece = vec![pt];
            }
            Some(c) if c != cell => {
                piece.push(pt); // carry the crossing into this piece…
                let prev = piece[piece.len() - 2];
                out.push((c, std::mem::take(&mut piece)));
                current = Some(cell);
                piece = vec![prev, pt]; // …and into the next one
            }
            Some(_) => piece.push(pt),
        }
    }
    if let (Some(c), true) = (current, piece.len() >= 2) {
        out.push((c, piece));
    }
    out
}

fn line_strings(geom: &Value) -> Vec<Vec<Pt>> {
    let line = |l: &Value| -> Vec<Pt> {
        l.as_array()
            .map(|pts| {
                pts.iter()
                    .filter_map(|p| {
                        let a = p.as_array()?;
                        (a.len() >= 2)
                            .then(|| Some([round(a[1].as_f64()?, 7), round(a[0].as_f64()?, 7)]))?
                    })
                    .collect()
            })
            .unwrap_or_default()
    };
    match geom["type"].as_str() {
        Some("LineString") => vec![line(&geom["coordinates"])],
        Some("MultiLineString") => geom["coordinates"]
            .as_array()
            .map(|ls| ls.iter().map(line).collect())
            .unwrap_or_default(),
        _ => Vec::new(),
    }
}

#[derive(Default)]
struct CellData {
    names: Vec<String>,
    lines: Vec<Value>,
    /// Per line, where its crest is measured (RD, the line's middle vertex), for a
    /// named line whose crest is ground; `None` for the rest.
    mids: Vec<Option<[f64; 2]>>,
    /// Per line, the measured crest in metres NAP ([`measure_crests`]).
    crests: Vec<Option<f32>>,
}

/// Half-width of the box a crest is measured in, and the model's own half metre
/// it is measured at — the client's `dikes::CREST_HALF_M` and `CREST_GRID`, which
/// this replaces where atlas has measured.
const CREST_HALF_M: f64 = 30.0;
const CREST_SAMPLE_M: f64 = 0.5;
/// Lines whose midpoints share a cell of this size share one request.
const CREST_CLUSTER_M: f64 = 120.0;
/// Bound on a cluster's raster; a wider cluster is sampled more coarsely.
const CREST_RASTER_MAX: usize = 400;

/// The highest bare ground within [`CREST_HALF_M`] of each measurable line's
/// middle — what the client used to ask PDOK for itself, ~450 boxes over Groningen,
/// which kept the height service's queue busy for the whole of a cold start.
fn measure_crests(
    ctx: &Ctx,
    cells: &mut HashMap<Cell, CellData>,
    ground: &dyn crate::terrain::HeightSource,
) -> Outcome<(usize, usize)> {
    let mut groups: HashMap<(i64, i64), Vec<(Cell, usize, [f64; 2])>> = HashMap::new();
    for (cell, data) in cells.iter_mut() {
        data.crests = vec![None; data.lines.len()];
        for (i, mid) in data.mids.iter().enumerate() {
            if let Some(m) = mid {
                let key = (
                    (m[0] / CREST_CLUSTER_M).floor() as i64,
                    (m[1] / CREST_CLUSTER_M).floor() as i64,
                );
                groups.entry(key).or_default().push((*cell, i, *m));
            }
        }
    }
    // A few clusters at a time: each is one WCS request of well under a second,
    // and 311 of them one after another were all of Groningen's 23 s (2026-09-24).
    let groups: Vec<_> = groups.into_values().collect();
    let (mut asked, mut measured) = (0usize, 0usize);
    in_order(
        ctx,
        &groups,
        ctx.upstream(),
        |members| cluster_crests(members, ground),
        |_, tops| {
            asked += 1;
            match tops {
                // A cluster PDOK would not answer is labels the client measures itself.
                Err(e) => ctx.log(format!("kruin niet gemeten: {e}")),
                Ok(tops) => {
                    for (cell, i, top) in tops {
                        if let Some(data) = cells.get_mut(&cell) {
                            data.crests[i] = Some(top);
                            measured += 1;
                        }
                    }
                }
            }
            Ok(())
        },
    )?;
    Ok((asked, measured))
}

/// The crest of every line in one cluster: the highest measured ground within
/// `CREST_HALF_M` of its midpoint, from one raster over the whole cluster.
fn cluster_crests(
    members: &[(Cell, usize, [f64; 2])],
    ground: &dyn crate::terrain::HeightSource,
) -> Result<Vec<(Cell, usize, f32)>, String> {
    let (mut lo, mut hi) = ([f64::MAX; 2], [f64::MIN; 2]);
    for (_, _, m) in members {
        lo = [
            lo[0].min(m[0] - CREST_HALF_M),
            lo[1].min(m[1] - CREST_HALF_M),
        ];
        hi = [
            hi[0].max(m[0] + CREST_HALF_M),
            hi[1].max(m[1] + CREST_HALF_M),
        ];
    }
    let span = (hi[0] - lo[0]).max(hi[1] - lo[1]);
    let n = ((span / CREST_SAMPLE_M).ceil() as usize + 1).clamp(2, CREST_RASTER_MAX);
    let max = [lo[0] + span, lo[1] + span];
    let px = span / n as f64;
    let raster = ground.raster(lo, max, n)?;
    let mut out = Vec::new();
    for &(cell, i, m) in members {
        let mut top = f32::NEG_INFINITY;
        for r in 0..n {
            // North row first.
            let y = max[1] - (r as f64 + 0.5) * px;
            if (y - m[1]).abs() > CREST_HALF_M {
                continue;
            }
            for c in 0..n {
                let x = lo[0] + (c as f64 + 0.5) * px;
                let v = raster[r * n + c];
                if (x - m[0]).abs() <= CREST_HALF_M && v.is_finite() {
                    top = top.max(v);
                }
            }
        }
        if top.is_finite() {
            out.push((cell, i, top));
        }
    }
    Ok(out)
}

pub fn run(ctx: &Ctx) -> Outcome<DatasetResult> {
    let bbox = wgs84_bbox(ctx.bbox);
    let mut cells: HashMap<Cell, CellData> = HashMap::new();
    let mut kinds: BTreeMap<&str, usize> = BTreeMap::new();
    let (mut n_lines, mut n_pieces) = (0usize, 0usize);

    ctx.log("keringen…");
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
    let (mut pages, mut kept, mut total) = (0usize, 0usize, 0usize);
    while let Some(u) = url.take() {
        if pages >= MAX_PAGES {
            ctx.log(format!(
                "WARNING: hit the {MAX_PAGES}-page guard; this area is incomplete."
            ));
            break;
        }
        ctx.check()?;
        let page = match get_json(ctx, &u, COLLECTION) {
            Ok(p) => p,
            Err(PipelineError::Cancelled) => return Err(PipelineError::Cancelled),
            Err(e) => {
                ctx.log(format!("giving up ({e})"));
                break;
            }
        };
        let feats = page["features"].as_array().cloned().unwrap_or_default();
        if feats.is_empty() {
            break;
        }
        for feat in &feats {
            total += 1;
            let props = &feat["properties"];
            if props["statusobject"] != "gerealiseerd" {
                continue;
            }
            kept += 1;
            let kind = kind_of(props["typewaterkering"].as_str().unwrap_or(""));
            let category = category_of(props["categoriewaterkering"].as_str().unwrap_or(""));
            let label = props["naam"]
                .as_str()
                .map(str::trim)
                .filter(|s| !s.is_empty());
            let kind_ix = DIKE_KINDS.iter().position(|k| *k == kind).unwrap_or(5);
            let cat_ix = DIKE_CATEGORIES
                .iter()
                .position(|c| *c == category)
                .unwrap_or(2);

            for pts in line_strings(&feat["geometry"]) {
                if pts.len() < 2 {
                    continue;
                }
                n_lines += 1;
                *kinds.entry(kind).or_default() += 1;
                for (cell, piece) in split_by_cell(&pts) {
                    if length_m(&piece) < MIN_LENGTH_M {
                        continue;
                    }
                    // In local metres: a tolerance in degrees is a different
                    // distance north-south than east-west, by 1.7× here.
                    let [lat0, lng0] = piece[0];
                    let scale = lat0.to_radians().cos();
                    let local: Vec<Pt> = piece
                        .iter()
                        .map(|&[lat, lng]| {
                            [(lng - lng0) * scale * M_PER_DEG, (lat - lat0) * M_PER_DEG]
                        })
                        .collect();
                    let simplified = simplify_segment(&local, SIMPLIFY_M);
                    if simplified.len() < 2 {
                        continue;
                    }
                    let back: Vec<[f64; 2]> = simplified
                        .iter()
                        .map(|&[x, y]| {
                            [
                                round(lat0 + y / M_PER_DEG, 7),
                                round(lng0 + x / (scale * M_PER_DEG), 7),
                            ]
                        })
                        .collect();
                    let bucket = cells.entry(cell).or_default();
                    let name_ix: i64 = match label {
                        Some(l) => match bucket.names.iter().position(|n| n == l) {
                            Some(i) => i as i64,
                            None => {
                                bucket.names.push(l.to_string());
                                (bucket.names.len() - 1) as i64
                            }
                        },
                        None => -1,
                    };
                    // The client's rule for which lines carry a label worth a
                    // crest: named, and not a lock or a pumping station, whose
                    // highest point is its roof.
                    let mid = (label.is_some() && kind != "structure").then(|| {
                        let [lat, lng] = back[back.len() / 2];
                        cartopolis_geo::ahn::wgs84_to_rd(lat, lng)
                    });
                    bucket.mids.push(mid);
                    bucket.lines.push(json!([kind_ix, cat_ix, name_ix, back]));
                    n_pieces += 1;
                }
            }
        }
        pages += 1;
        ctx.log(format!("{kept} realised of {total} seen"));
        ctx.progress(Progress {
            phase: Some("keringen".into()),
            done: Some(kept as u64),
            total: Some(None),
        });
        url = page["links"]
            .as_array()
            .and_then(|ls| ls.iter().find(|l| l["rel"] == "next"))
            .and_then(|l| l["href"].as_str())
            .map(str::to_string);
    }

    ctx.log("kruinhoogtes…");
    let (asked, measured) = measure_crests(ctx, &mut cells, &crate::terrain::Pdok::new())?;
    ctx.log(format!("{measured} kruinen gemeten in {asked} verzoeken"));
    for (cell, data) in &cells {
        // `crests` beside `lines` rather than inside each line: a client that
        // reads a line as exactly four fields would refuse a fifth, and every
        // installed one does.
        let crests: Vec<Value> = data
            .crests
            .iter()
            .map(|c| c.map_or(Value::Null, |v| json!(round(f64::from(v), 2))))
            .collect();
        ctx.write_json(
            &cell_path("dikes", *cell),
            &json!({"names": data.names, "lines": data.lines, "crests": crests}),
        )?;
    }
    ctx.write_json(
        "dikes/coverage.json",
        &json!({
            "name": ctx.name,
            // The run: the client keys its cache of these cells on it, so a
            // re-run (the one that added `crests`) is never read from an old copy.
            "stamp": std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map_or(0, |d| d.as_secs()),
            "zoom": CACHE_ZOOM,
            "attribution": ATTRIBUTION,
            "source": "IMWA Keringen via PDOK OGC API",
            "bbox_rd": ctx.bbox,
            "kinds": DIKE_KINDS,
            "categories": DIKE_CATEGORIES,
            "cells": sorted_cells(cells.keys().copied()),
            "line_count": n_lines,
            "piece_count": n_pieces,
        }),
    )?;
    let breakdown: Vec<String> = kinds.iter().map(|(k, v)| format!("{v} {k}")).collect();
    ctx.log(format!(
        "done: {n_lines} flood defences ({}) cut into {n_pieces} pieces across {} cells",
        breakdown.join(", "),
        cells.len()
    ));
    Ok(DatasetResult {
        cells: cells.len(),
        counts: vec![
            Count::new("defences", n_lines),
            Count::new("pieces", n_pieces),
            Count::new("crests measured", measured),
        ],
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    struct Quiet;
    impl crate::pipeline::Sink for Quiet {
        fn log(&self, _: String) {}
        fn progress(&self, _: crate::pipeline::Progress) {}
    }

    /// Ground that rises eastward a metre per metre, so the highest sample in a
    /// crest box is its east edge: 30 m past the line's middle.
    struct Ramp(std::sync::atomic::AtomicUsize);
    impl crate::terrain::HeightSource for Ramp {
        fn raster(&self, min: [f64; 2], max: [f64; 2], n: usize) -> Result<Vec<f32>, String> {
            self.0.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            let px = (max[0] - min[0]) / n as f64;
            Ok((0..n * n)
                .map(|i| (min[0] + ((i % n) as f64 + 0.5) * px) as f32)
                .collect())
        }
    }

    #[test]
    fn crests_are_the_highest_ground_near_each_line_and_neighbours_share_a_request() {
        let dir = tempfile::tempdir().unwrap();
        let ctx = Ctx::new(
            [0.0; 4],
            "t",
            dir.path(),
            std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false)),
            std::sync::Arc::new(Quiet),
        );
        let mut cells: HashMap<Cell, CellData> = HashMap::new();
        let data = cells.entry((1, 1)).or_default();
        data.lines = vec![Value::Null; 3];
        // Two measurable lines 20 m apart, and one that is not measured.
        data.mids = vec![Some([1000.0, 5000.0]), Some([1020.0, 5000.0]), None];
        let source = Ramp(std::sync::atomic::AtomicUsize::new(0));
        let (asked, measured) = measure_crests(&ctx, &mut cells, &source).unwrap();
        assert_eq!((asked, measured), (1, 2));
        assert_eq!(source.0.load(std::sync::atomic::Ordering::SeqCst), 1);
        let crests = &cells[&(1, 1)].crests;
        assert!((crests[0].unwrap() - 1030.0).abs() < 0.5, "{crests:?}");
        assert!((crests[1].unwrap() - 1050.0).abs() < 0.5, "{crests:?}");
        assert_eq!(crests[2], None);
    }

    /// A line crossing a cell edge becomes two pieces that share the crossing
    /// segment, so there is no gap where the cells meet.
    #[test]
    fn pieces_share_the_segment_that_crosses_a_cell_edge() {
        let (x0, y0) = (8490.0f64, 5320.0f64);
        let lat = |ty: f64| cartopolis_core::world::tile_frac_to_lat_lng(x0, ty, CACHE_ZOOM).0;
        let lng = |tx: f64| cartopolis_core::world::tile_frac_to_lat_lng(tx, y0, CACHE_ZOOM).1;
        let pts = [
            [lat(y0 + 0.5), lng(x0 + 0.2)],
            [lat(y0 + 0.5), lng(x0 + 0.8)],
            [lat(y0 + 0.5), lng(x0 + 1.3)],
            [lat(y0 + 0.5), lng(x0 + 1.6)],
        ];
        let pieces = split_by_cell(&pts);
        assert_eq!(pieces.len(), 2);
        assert_eq!(pieces[0].0, (8490, 5320));
        assert_eq!(pieces[1].0, (8491, 5320));
        assert_eq!(pieces[0].1.last(), Some(&pts[2]));
        assert_eq!(pieces[1].1[..2], [pts[1], pts[2]]);
    }

    /// Measured to the segment: a line that doubles back keeps its far end.
    #[test]
    fn a_line_that_doubles_back_keeps_its_far_vertex() {
        let pts = [[0.0, 0.0], [10.0, 0.0], [20.0, 0.0], [5.0, 0.5]];
        let kept = simplify_segment(&pts, 1.0);
        assert!(kept.contains(&[20.0, 0.0]), "{kept:?}");
    }
}
