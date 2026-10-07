//! The **sewer network** — pipes and manholes, with how deep each lies — as
//! per-cell files for the client's underground layer (`systems::sewers`).
//!
//! The municipalities publish their sewer registers in one national vocabulary
//! (GWSW, kept by Stichting RIONED) and PDOK serves all of them through one
//! WFS, CC0. It is the only part of what lies under a Dutch street that is
//! open: gas, water, power and telecom are in KLIC, which is handed out per dig
//! and may not be republished. Groningen was the pilot (2021) and is complete —
//! ~35,000 pipe sections around the city, 83 % of them with a measured invert.
//!
//! **What is written is a depth below the ground, not a level.** The register's
//! levels are the ground at each manhole (`maaiveldhoogte`) and the bottom
//! inside each pipe end (`bob`), and a pipe names its two manholes (`uriB`,
//! `uriE`). Ground minus invert is a depth in metres whatever datum both are in,
//! and the client hangs it under its own terrain — which is relative to the
//! ground under the map anchor and could not use a level anyway. Measured over
//! central Groningen (2026-10-07, 147 pipe ends): 1.23 m at the 5th percentile,
//! 1.72 m median, 3.13 m at the 95th, which is what a Dutch street sewer is.
//!
//! The service pages at 1,000 features whatever `count` asks for, and pages
//! stably only with a `sortBy`; a short page is the last one. A feature on a
//! page boundary can arrive twice, so features are de-duplicated by their id.
//!
//! Data: Stedelijk Water (Riolering) GWSW — the municipalities and water boards
//! via Stichting RIONED and PDOK, CC0.

use std::collections::{BTreeMap, HashMap, HashSet};

use cartopolis_geo::formats::coverage::Manifest;
use cartopolis_geo::formats::sewers::{self as wire, SewerKind};
use serde_json::Value;

use super::dikes::simplify_segment;
use super::geom::{round, Pt};
use super::http::{get_json, query};
use super::rd::rd_to_lat_lng;
use super::tile::{cell_path, lat_lng_to_cell, sorted_cells, Cell, CACHE_ZOOM};
use super::{Count, Ctx, DatasetResult, Outcome, PipelineError, Progress};

const WFS: &str = "https://service.pdok.nl/rioned/beheerstedelijkwater/wfs/v1_0";
const PIPES: &str = "beheerstedelijkwater:BeheerLeiding";
const MANHOLES: &str = "beheerstedelijkwater:BeheerPut";
/// The service's own page ceiling; asking for more returns this many.
const PAGE: usize = 1_000;
/// Runaway guard: a million features is ten times the country's largest city.
const MAX_PAGES: usize = 1_000;

const ATTRIBUTION: &str =
    "Sewers © the municipalities and water boards (GWSW, Stichting RIONED) via PDOK (CC0)";

/// Metres, in RD. Pipes run straight between manholes; this only strips the
/// collinear filler some registers digitise long mains with.
const SIMPLIFY_M: f64 = 0.10;
/// Depths outside this are a missing level read as zero, or a datum mix-up
/// inside one register, and are dropped rather than drawn — a pipe drawn 40 m
/// down is worse than one drawn at the default depth.
const DEPTH_RANGE_M: (f64, f64) = (0.1, 15.0);

/// Every feature of one type inside the area, in RD, de-duplicated.
fn fetch_all(ctx: &Ctx, type_name: &str, label: &str) -> Outcome<Vec<Value>> {
    let [x0, y0, x1, y1] = ctx.bbox;
    let bbox = format!("{x0},{y0},{x1},{y1},urn:ogc:def:crs:EPSG::28992");
    let page = PAGE.to_string();
    let mut seen: HashSet<String> = HashSet::new();
    let mut out = Vec::new();
    for n in 0..MAX_PAGES {
        ctx.check()?;
        let start = (n * PAGE).to_string();
        let url = format!(
            "{WFS}?{}",
            query(&[
                ("service", "WFS"),
                ("version", "2.0.0"),
                ("request", "GetFeature"),
                ("typeNames", type_name),
                ("outputFormat", "application/json"),
                ("srsName", "EPSG:28992"),
                ("bbox", &bbox),
                ("sortBy", "uri"),
                ("count", &page),
                ("startIndex", &start),
            ])
        );
        let doc = get_json(ctx, &url, label)?;
        let features = doc
            .get("features")
            .and_then(Value::as_array)
            .ok_or_else(|| PipelineError::Failed(format!("{label}: no features array")))?;
        let len = features.len();
        for f in features {
            let id = f.get("id").and_then(Value::as_str).unwrap_or_default();
            if id.is_empty() || seen.insert(id.to_string()) {
                out.push(f.clone());
            }
        }
        ctx.progress(Progress {
            phase: Some(label.to_string()),
            done: Some(out.len() as u64),
            total: Some(None),
        });
        if len < PAGE {
            return Ok(out);
        }
    }
    Err(PipelineError::Failed(format!(
        "{label}: more than {MAX_PAGES} pages; refusing to guess where it ends"
    )))
}

fn prop<'a>(f: &'a Value, key: &str) -> Option<&'a Value> {
    f.get("properties").and_then(|p| p.get(key))
}

fn prop_str<'a>(f: &'a Value, key: &str) -> &'a str {
    prop(f, key).and_then(Value::as_str).unwrap_or_default()
}

fn prop_f64(f: &Value, key: &str) -> Option<f64> {
    prop(f, key).and_then(Value::as_f64)
}

/// The key a pipe uses to name its manhole: the register it came from plus the
/// manhole's `uri`, which is unique only inside one register.
fn manhole_key(f: &Value, uri: &str) -> String {
    format!("{}#{uri}", prop_str(f, "dataset"))
}

/// The first point of a `MultiPoint`/`Point`, in RD.
fn point_of(f: &Value) -> Option<Pt> {
    let g = f.get("geometry")?;
    let c = g.get("coordinates")?;
    let p = match g.get("type").and_then(Value::as_str)? {
        "Point" => c,
        "MultiPoint" => c.get(0)?,
        _ => return None,
    };
    Some([p.get(0)?.as_f64()?, p.get(1)?.as_f64()?])
}

/// A `LineString`/`MultiLineString` as one RD polyline. A multi-line's parts
/// are joined in order — in this register a pipe is one segment between two
/// manholes, and the parts of the few multi-part ones follow on.
fn line_of(f: &Value) -> Vec<Pt> {
    let Some(g) = f.get("geometry") else {
        return Vec::new();
    };
    let Some(c) = g.get("coordinates").and_then(Value::as_array) else {
        return Vec::new();
    };
    let parts: Vec<&Value> = match g.get("type").and_then(Value::as_str) {
        Some("LineString") => vec![g.get("coordinates").unwrap_or(&Value::Null)],
        Some("MultiLineString") => c.iter().collect(),
        _ => Vec::new(),
    };
    let mut out: Vec<Pt> = Vec::new();
    for part in parts {
        for p in part.as_array().into_iter().flatten() {
            if let (Some(x), Some(y)) = (
                p.get(0).and_then(Value::as_f64),
                p.get(1).and_then(Value::as_f64),
            ) {
                if out.last() != Some(&[x, y]) {
                    out.push([x, y]);
                }
            }
        }
    }
    out
}

/// Ground minus invert, when both are known and the answer is a plausible depth.
#[must_use]
pub fn depth_below(ground: Option<f64>, invert: Option<f64>) -> Option<f32> {
    let d = ground? - invert?;
    (DEPTH_RANGE_M.0..=DEPTH_RANGE_M.1)
        .contains(&d)
        .then(|| round(d, 2) as f32)
}

/// A dimension in millimetres, clamped into what the wire carries.
fn millimetres(v: Option<f64>) -> u16 {
    v.filter(|v| v.is_finite() && *v > 0.0)
        .map_or(0, |v| v.round().min(f64::from(u16::MAX)) as u16)
}

pub fn run(ctx: &Ctx) -> Outcome<DatasetResult> {
    let manholes = fetch_all(ctx, MANHOLES, "putten")?;
    ctx.log(format!("{} putten", manholes.len()));
    let pipes = fetch_all(ctx, PIPES, "leidingen")?;
    ctx.log(format!("{} leidingen", pipes.len()));

    // The ground at every manhole, for the pipes that name it.
    let ground: HashMap<String, f64> = manholes
        .iter()
        .filter_map(|f| {
            let uri = prop_str(f, "uri");
            let g = prop_f64(f, "maaiveldhoogte")?;
            (!uri.is_empty()).then(|| (manhole_key(f, uri), g))
        })
        .collect();

    let mut cells: BTreeMap<Cell, wire::Cell> = BTreeMap::new();
    let (mut n_pipes, mut n_measured, mut n_manholes) = (0usize, 0usize, 0usize);
    let mut kinds = [0usize; SewerKind::ALL.len()];

    for f in &manholes {
        let Some([x, y]) = point_of(f) else {
            continue;
        };
        let (lat, lng) = rd_to_lat_lng(x, y);
        let depth = prop_f64(f, "hoogtePut")
            .map(|mm| mm / 1000.0)
            .filter(|d| (DEPTH_RANGE_M.0..=DEPTH_RANGE_M.1).contains(d))
            .map(|d| round(d, 2) as f32);
        cells
            .entry(lat_lng_to_cell(lat, lng))
            .or_default()
            .manholes
            .push((round(lat, 7), round(lng, 7), depth));
        n_manholes += 1;
    }

    for f in &pipes {
        let rd = line_of(f);
        if rd.len() < 2 {
            continue;
        }
        let rd = simplify_segment(&rd, SIMPLIFY_M);
        let kind = SewerKind::from_gwsw(prop_str(f, "type").rsplit('/').next().unwrap_or(""));
        let ground_at = |key: &str| {
            let uri = prop_str(f, key);
            (!uri.is_empty())
                .then(|| ground.get(&manhole_key(f, uri)).copied())
                .flatten()
        };
        let start = depth_below(ground_at("uriB"), prop_f64(f, "bobBeginpuntLeiding"));
        let end = depth_below(ground_at("uriE"), prop_f64(f, "bobEindpuntLeiding"));
        if start.is_some() || end.is_some() {
            n_measured += 1;
        }
        let width = millimetres(prop_f64(f, "breedteLeiding"));
        let height = millimetres(prop_f64(f, "hoogteLeiding"));
        let flat: Vec<f64> = rd
            .iter()
            .flat_map(|&[x, y]| {
                let (lat, lng) = rd_to_lat_lng(x, y);
                [round(lat, 7), round(lng, 7)]
            })
            .collect();
        // Filed by midpoint (module note on the format).
        let [mx, my] = [
            (rd[0][0] + rd[rd.len() - 1][0]) / 2.0,
            (rd[0][1] + rd[rd.len() - 1][1]) / 2.0,
        ];
        let (mlat, mlng) = rd_to_lat_lng(mx, my);
        cells
            .entry(lat_lng_to_cell(mlat, mlng))
            .or_default()
            .pipes
            .push((kind.index(), width, height, start, end, flat));
        kinds[usize::from(kind.index())] += 1;
        n_pipes += 1;
    }

    let covered = sorted_cells(cells.keys().copied());
    let n_cells = cells.len();
    for (cell, doc) in &cells {
        ctx.check()?;
        ctx.write_doc(&cell_path("sewers", *cell), doc)?;
    }
    ctx.write_coverage(
        "sewers/coverage.json",
        &Manifest::new(CACHE_ZOOM, covered)
            .described(
                ctx.name.as_str(),
                ATTRIBUTION,
                "Stedelijk Water (Riolering) GWSW via PDOK",
                ctx.bbox,
            )
            .with("pipe_count", n_pipes)
            .with("manhole_count", n_manholes),
    )?;
    ctx.log(format!(
        "{n_pipes} leidingen ({n_measured} met diepte), {n_manholes} putten over {n_cells} cellen"
    ));
    let mut counts = vec![
        Count::new("pipes", n_pipes),
        Count::new("pipes with a depth", n_measured),
        Count::new("manholes", n_manholes),
    ];
    for (k, n) in SewerKind::ALL.iter().zip(kinds) {
        counts.push(Count::new(&format!("{k:?} pipes").to_lowercase(), n));
    }
    Ok(DatasetResult {
        cells: n_cells,
        counts,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn a_depth_is_ground_minus_invert_and_implausible_ones_are_dropped() {
        assert_eq!(depth_below(Some(6.37), Some(4.65)), Some(1.72));
        // A missing ground level the register wrote as 0.
        assert_eq!(depth_below(Some(0.0), Some(5.88)), None);
        assert_eq!(depth_below(None, Some(5.88)), None);
        assert_eq!(depth_below(Some(30.0), Some(1.0)), None);
    }

    #[test]
    fn a_multi_line_is_one_polyline_without_repeated_joints() {
        let f = json!({"geometry": {"type": "MultiLineString",
            "coordinates": [[[0.0, 0.0], [10.0, 0.0]], [[10.0, 0.0], [20.0, 5.0]]]}});
        assert_eq!(line_of(&f), vec![[0.0, 0.0], [10.0, 0.0], [20.0, 5.0]]);
    }

    #[test]
    fn a_pipe_finds_its_manhole_only_in_its_own_register() {
        let a = json!({"properties": {"dataset": "x?dataset=Groningen"}});
        let b = json!({"properties": {"dataset": "x?dataset=Eemsdelta"}});
        assert_ne!(manhole_key(&a, "Put_1"), manhole_key(&b, "Put_1"));
    }

    #[test]
    fn dimensions_are_millimetres_on_the_wire() {
        assert_eq!(millimetres(Some(500.0)), 500);
        assert_eq!(millimetres(None), 0);
        assert_eq!(millimetres(Some(-3.0)), 0);
        assert_eq!(millimetres(Some(1e9)), u16::MAX);
    }
}
