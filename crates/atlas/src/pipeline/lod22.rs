//! 3DBAG **LoD2.2** buildings — the real roof geometry, as per-cell triangle
//! meshes (`lod22.rs` in the client reads them).
//!
//! Ported from the earlier TypeScript pipeline (`lod22.ts`, itself from
//! `tools/3dbag_lod22.py`):
//!
//!   1. Ask the `BAG3D:tiles` WFS which CityJSON tiles cover the RD bbox.
//!   2. For each CityObject's LoD2.2 Solid: dereference the quantised vertices,
//!      drop the GroundSurface (the underside, never seen on a flat map), and
//!      triangulate each Wall/Roof surface — planar, possibly with holes — by
//!      earcutting it on its own plane.
//!   3. Reproject RD → WGS84; height is `z −` the building's own minimum z, so it
//!      sits on the client's ground at y = 0 like the LoD1.2 tier does.
//!   4. Bin by the first vertex's z14 cell and write `lod22/14/{x}/{y}.json`.
//!
//! Wire format per cell: `{"elements":[{"id":"0014100010903891","part":0,
//! "p":[[lat,lng,h],…],"t":[[i,j,k,roof],…]}, …]}`. `id` is the BAG pand
//! identificatie **verbatim, as a string** — the key every other register joins on —
//! and `part` is omitted when 0.
//!
//! The one place this cannot match the TypeScript byte for byte is triangulation:
//! `earcutr` and JavaScript's `earcut` may pick different diagonals across the same
//! polygon, as the Python binding and `earcut` already did. Same buildings, same
//! vertices, the same surface covered.
//!
//! Data: 3DBAG (https://3dbag.nl), TU Delft — CC BY 4.0; footprints Kadaster BAG.

use std::collections::{HashMap, HashSet};

use cartopolis_geo::lod22_cell;
use serde_json::{json, Value};

use super::geom::round;
use super::http::{get_json_with, get_maybe_gzipped_json, get_text, query};
use super::rd::rd_to_lat_lng;
use super::tile::{cell_path, lat_lng_to_cell, sorted_cells, Cell, CACHE_ZOOM};
use super::{in_order, Count, Ctx, DatasetResult, Outcome, PipelineError, Progress};

const WFS_BASE: &str = "https://data.3dbag.nl/api/BAG3D/wfs";

/// Preferred name of the tile-index layer. **Upstream renamed it** (`BAG3D:Tiles`
/// → `BAG3D:tiles`, 2026-08), and asking for the wrong one earns a bare 400, so
/// the name is resolved against the service's capabilities and only the
/// case-insensitive match is assumed stable.
const TILES_LAYER: &str = "BAG3D:tiles";

const ATTRIBUTION: &str = "3D data © 3DBAG by TU Delft (3D geoinformation), CC BY 4.0; \
                           building footprints © Kadaster BAG";

type V3 = [f64; 3];

fn cross(a: V3, b: V3) -> V3 {
    [
        a[1] * b[2] - a[2] * b[1],
        a[2] * b[0] - a[0] * b[2],
        a[0] * b[1] - a[1] * b[0],
    ]
}

fn len(v: V3) -> f64 {
    (v[0] * v[0] + v[1] * v[1] + v[2] * v[2]).sqrt()
}

/// Triangulate one planar surface (outer ring, then holes) given as 3D points and
/// per-ring lengths: index triples into `pts`, or nothing if degenerate.
///
/// The plane comes from a **Newell normal** over the outer ring, not a cross of the
/// first three points: roof surfaces are routinely non-convex with nearly collinear
/// first vertices, and Newell is robust to both.
#[must_use]
pub fn triangulate_surface(pts: &[V3], ring_lens: &[usize]) -> Vec<[usize; 3]> {
    if pts.len() < 3 || ring_lens.is_empty() {
        return Vec::new();
    }
    let outer = &pts[..ring_lens[0].min(pts.len())];
    let mut n = [0.0; 3];
    for i in 0..outer.len() {
        let a = outer[i];
        let b = outer[(i + 1) % outer.len()];
        n[0] += (a[1] - b[1]) * (a[2] + b[2]);
        n[1] += (a[2] - b[2]) * (a[0] + b[0]);
        n[2] += (a[0] - b[0]) * (a[1] + b[1]);
    }
    let ln = len(n);
    if ln < 1e-9 {
        return Vec::new();
    }
    let n = [n[0] / ln, n[1] / ln, n[2] / ln];

    // An in-plane basis: the first edge with the normal removed, or a cross with
    // an axis when that edge is parallel to the normal.
    let mut u = [
        pts[1][0] - pts[0][0],
        pts[1][1] - pts[0][1],
        pts[1][2] - pts[0][2],
    ];
    let dot = u[0] * n[0] + u[1] * n[1] + u[2] * n[2];
    u = [u[0] - n[0] * dot, u[1] - n[1] * dot, u[2] - n[2] * dot];
    if len(u) < 1e-9 {
        u = cross(n, [1.0, 0.0, 0.0]);
        if len(u) < 1e-9 {
            u = cross(n, [0.0, 1.0, 0.0]);
        }
    }
    let ul = len(u);
    if ul < 1e-9 {
        return Vec::new();
    }
    let u = [u[0] / ul, u[1] / ul, u[2] / ul];
    let v = cross(n, u);

    let o = pts[0];
    let mut flat = Vec::with_capacity(pts.len() * 2);
    for p in pts {
        let d = [p[0] - o[0], p[1] - o[1], p[2] - o[2]];
        flat.push(d[0] * u[0] + d[1] * u[1] + d[2] * u[2]);
        flat.push(d[0] * v[0] + d[1] * v[1] + d[2] * v[2]);
    }
    // Hole *start* indices: the running sum of ring lengths without its last term.
    let mut holes = Vec::new();
    let mut acc = 0;
    for l in &ring_lens[..ring_lens.len() - 1] {
        acc += l;
        holes.push(acc);
    }
    let Ok(idx) = earcutr::earcut(&flat, &holes, 2) else {
        return Vec::new();
    };
    idx.chunks_exact(3).map(|t| [t[0], t[1], t[2]]).collect()
}

/// `NL.IMBAG.Pand.0014100010903891-0` → `("0014100010903891", 0)`. The 16 digits
/// are the BAG identificatie; the `-N` suffix numbers the solids one pand was split
/// into. Anything else passes through verbatim with part 0 — still a stable
/// identifier, where an invented hash would differ between runs.
#[must_use]
pub fn object_id(oid: &str) -> (String, u32) {
    let (body, part) = match oid.rsplit_once('-') {
        Some((b, p)) if !p.is_empty() && p.bytes().all(|c| c.is_ascii_digit()) => {
            (b, p.parse().ok())
        }
        _ => (oid, None),
    };
    let digits = body.rsplit('.').next().unwrap_or(body);
    let at_boundary =
        digits.len() == body.len() || body.as_bytes()[body.len() - digits.len() - 1] == b'.';
    if digits.len() == 16 && digits.bytes().all(|c| c.is_ascii_digit()) && at_boundary {
        (digits.to_string(), part.unwrap_or(0))
    } else {
        (oid.to_string(), 0)
    }
}

fn resolve_tiles_layer(ctx: &Ctx) -> String {
    match get_text(
        ctx,
        &format!("{WFS_BASE}?service=WFS&request=GetCapabilities"),
        "capabilities",
    ) {
        Ok(caps) => {
            for chunk in caps.split("<Name>").skip(1) {
                let Some((name, _)) = chunk.split_once("</Name>") else {
                    continue;
                };
                if name.eq_ignore_ascii_case(TILES_LAYER) {
                    if name != TILES_LAYER {
                        ctx.log(format!(
                            "tile layer is '{name}' upstream, not '{TILES_LAYER}'"
                        ));
                    }
                    return name.to_string();
                }
            }
            ctx.log(format!(
                "no tile layer in the capabilities; assuming {TILES_LAYER}"
            ));
        }
        Err(e) => ctx.log(format!(
            "capabilities unreadable ({e}); assuming {TILES_LAYER}"
        )),
    }
    TILES_LAYER.to_string()
}

fn tile_urls(ctx: &Ctx) -> Outcome<Vec<String>> {
    let layer = resolve_tiles_layer(ctx);
    let [xmin, ymin, xmax, ymax] = ctx.bbox;
    let url = format!(
        "{WFS_BASE}?{}",
        query(&[
            ("service", "WFS"),
            ("version", "2.0.0"),
            ("request", "GetFeature"),
            ("typeNames", &layer),
            ("count", "10000"),
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
    );
    let data = get_json_with(
        ctx,
        &url,
        "tile index",
        std::time::Duration::from_secs(120),
        4,
    )?;
    let mut urls: Vec<String> = Vec::new();
    for feat in data["features"].as_array().into_iter().flatten() {
        if let Some(cj) = feat["properties"]["cj_download"].as_str() {
            if !cj.is_empty() && !urls.iter().any(|u| u == cj) {
                urls.push(cj.to_string());
            }
        }
    }
    Ok(urls)
}

fn js(v: f64) -> String {
    if v.fract() == 0.0 && v.abs() < 1e15 {
        format!("{}", v as i64)
    } else {
        format!("{v}")
    }
}

/// One LoD2.2 Solid → deduplicated `[lat, lng, h]` vertices and `[i,j,k,roof]`
/// triangles, or `None` when it has no usable wall or roof geometry.
fn process_object(
    g: &Value,
    verts: &[Value],
    scale: V3,
    translate: V3,
) -> Option<(Vec<[f64; 3]>, Vec<[u32; 4]>)> {
    let shell = g["boundaries"].get(0)?.as_array()?;
    let sem_values = g["semantics"]["values"]
        .get(0)
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    let sem_surfaces = g["semantics"]["surfaces"]
        .as_array()
        .cloned()
        .unwrap_or_default();
    let real = |vi: usize| -> Option<V3> {
        let v = verts.get(vi)?.as_array()?;
        Some([
            v.first()?.as_f64()? * scale[0] + translate[0],
            v.get(1)?.as_f64()? * scale[1] + translate[1],
            v.get(2)?.as_f64()? * scale[2] + translate[2],
        ])
    };
    let rings = |surf: &Value| -> Vec<Vec<usize>> {
        surf.as_array()
            .map(|rs| {
                rs.iter()
                    .map(|r| {
                        r.as_array()
                            .map(|ix| {
                                ix.iter()
                                    .filter_map(|i| i.as_u64().map(|i| i as usize))
                                    .collect()
                            })
                            .unwrap_or_default()
                    })
                    .collect()
            })
            .unwrap_or_default()
    };

    // The building's own ground datum, so every height is above its own base.
    let mut ground_z = f64::INFINITY;
    let mut any = false;
    for surf in shell {
        for ring in rings(surf) {
            for vi in ring {
                if let Some(p) = real(vi) {
                    any = true;
                    ground_z = ground_z.min(p[2]);
                }
            }
        }
    }
    if !any {
        return None;
    }

    let mut local: HashMap<usize, u32> = HashMap::new();
    let mut p: Vec<[f64; 3]> = Vec::new();
    let mut tris: Vec<[u32; 4]> = Vec::new();

    for (si, surf) in shell.iter().enumerate() {
        let stype = sem_values
            .get(si)
            .and_then(Value::as_u64)
            .and_then(|sv| sem_surfaces.get(sv as usize))
            .and_then(|s| s["type"].as_str());
        if stype == Some("GroundSurface") {
            continue;
        }
        let is_roof = u32::from(stype == Some("RoofSurface"));
        let mut pts = Vec::new();
        let mut ring_lens = Vec::new();
        let mut gidx = Vec::new();
        for ring in rings(surf) {
            ring_lens.push(ring.len());
            for vi in ring {
                gidx.push(vi);
                pts.push(real(vi)?);
            }
        }
        for [a, b, c] in triangulate_surface(&pts, &ring_lens) {
            let mut local_of = |vi: usize| -> u32 {
                *local.entry(vi).or_insert_with(|| {
                    let [x, y, z] = real(vi).unwrap_or([0.0; 3]);
                    let (lat, lng) = rd_to_lat_lng(x, y);
                    // ~1 cm: 7 dp is 1.1 cm of latitude, height to the centimetre —
                    // plenty for building geometry, and roughly halves the JSON.
                    p.push([round(lat, 7), round(lng, 7), round(z - ground_z, 2)]);
                    (p.len() - 1) as u32
                })
            };
            let t = [
                local_of(gidx[a]),
                local_of(gidx[b]),
                local_of(gidx[c]),
                is_roof,
            ];
            tris.push(t);
        }
    }
    if tris.is_empty() {
        None
    } else {
        Some((p, tris))
    }
}

fn v3(v: &Value) -> Option<V3> {
    Some([
        v.get(0)?.as_f64()?,
        v.get(1)?.as_f64()?,
        v.get(2)?.as_f64()?,
    ])
}

/// One building part's LoD2.2 shell, triangulated.
struct Solid {
    pand: String,
    part: u32,
    p: Vec<[f64; 3]>,
    t: Vec<[u32; 4]>,
}

/// Every LoD2.2 solid in one CityJSON tile, in the tile's own object order —
/// the work of a tile, done where it was downloaded.
fn solids(cj: &Value) -> Result<Vec<Solid>, String> {
    let (Some(scale), Some(translate)) = (
        v3(&cj["transform"]["scale"]),
        v3(&cj["transform"]["translate"]),
    ) else {
        return Err("no transform".into());
    };
    let verts = cj["vertices"].as_array().map_or(&[][..], Vec::as_slice);
    let mut out = Vec::new();
    for (oid, obj) in cj["CityObjects"].as_object().into_iter().flatten() {
        let Some(g22) = obj["geometry"].as_array().and_then(|gs| {
            gs.iter()
                .find(|g| g["lod"] == "2.2" && g["type"] == "Solid")
        }) else {
            continue;
        };
        let Some((p, t)) = process_object(g22, verts, scale, translate) else {
            continue;
        };
        let (pand, part) = object_id(oid);
        out.push(Solid { pand, part, p, t });
    }
    Ok(out)
}

/// One solid as the binary encoding's element — the same numbers the JSON
/// element carries (already rounded to 10⁻⁷° and 1 cm by [`process_object`]).
fn element(pand: String, part: u32, p: &[[f64; 3]], t: &[[u32; 4]]) -> lod22_cell::Element {
    lod22_cell::Element {
        id: pand,
        part,
        class: String::new(),
        p: p.iter().map(|v| (v[0], v[1], v[2] as f32)).collect(),
        // The roof flag is 0 or 1 by construction.
        t: t.iter().map(|x| (x[0], x[1], x[2], x[3] as u8)).collect(),
    }
}

/// `lod22/14/{x}/{y}.bin` — the binary twin of [`cell_path`]'s JSON.
fn bin_path(cell: Cell) -> String {
    let (x, y) = cell;
    format!("lod22/{CACHE_ZOOM}/{x}/{y}.bin")
}

/// Write one cell in both formats.
///
/// The JSON exactly as it always was, because every installed APK and every
/// cached web build reads it and only a client from 2026-09-26 on knows the
/// binary exists. The binary beside it, in `cartopolis_geo::lod22_cell`'s
/// encoding on [`Grid::JSON`] — the grid the JSON is already rounded to, so it
/// holds the same numbers — plus its `.bin.gz` for the host to serve
/// compressed. A cell whose binary cannot be encoded (a non-finite coordinate,
/// which the JSON would have written as `null`) keeps its JSON and is logged:
/// the client falls back to the JSON for a missing binary.
///
/// Both are written from the one `lod22_cell::Cell`, the JSON through
/// `cartopolis_geo::formats::lod22` — the type the client parses it with.
///
/// [`Grid::JSON`]: lod22_cell::Grid::JSON
fn write_cell(ctx: &Ctx, cell: Cell, bin: &lod22_cell::Cell) -> Outcome<()> {
    ctx.write_doc(
        &cell_path("lod22", cell),
        &cartopolis_geo::formats::lod22::File::from(bin),
    )?;
    match lod22_cell::encode(bin, lod22_cell::Grid::JSON) {
        Ok(bytes) => ctx.write_bytes_gzipped(&bin_path(cell), &bytes),
        Err(e) => {
            ctx.log(format!("cell {cell:?}: no binary written: {e}"));
            Ok(())
        }
    }
}

pub fn run(ctx: &Ctx) -> Outcome<DatasetResult> {
    let urls = tile_urls(ctx)?;
    ctx.log(format!("{} CityJSON tiles cover the bbox", urls.len()));
    let progress = |done: usize| {
        ctx.progress(Progress {
            phase: Some("tiles".into()),
            done: Some(done as u64),
            total: Some(Some(urls.len() as u64)),
        });
    };
    progress(0);

    // Each cell as the binary encoding's plain data, from which `write_cell`
    // writes both formats.
    let mut cells: HashMap<Cell, lod22_cell::Cell> = HashMap::new();
    let mut seen: HashSet<String> = HashSet::new();
    let (mut total, mut tiles_done) = (0usize, 0usize);

    // Downloaded and triangulated a few tiles at a time, deduplicated and binned
    // in tile order — the order decides which tile's copy of a building on a
    // tile edge is kept, so it must be the serial walk's.
    in_order(
        ctx,
        &urls,
        ctx.upstream(),
        |url| -> Outcome<Result<Vec<Solid>, String>> {
            match get_maybe_gzipped_json(ctx, url, "tile") {
                Ok(cj) => Ok(solids(&cj)),
                Err(PipelineError::Cancelled) => Err(PipelineError::Cancelled),
                Err(e) => Ok(Err(e.to_string())),
            }
        },
        |url, answer| {
            let n_before = total;
            tiles_done += 1;
            match answer? {
                // One unreadable tile is a hole in the coverage, not a failed run.
                Err(e) => ctx.log(format!("tile {url}: {e}")),
                Ok(found) => {
                    for Solid { pand, part, p, t } in found {
                        if !seen.insert(format!("{pand}-{part}")) {
                            continue;
                        }
                        let cell = lat_lng_to_cell(p[0][0], p[0][1]);
                        cells
                            .entry(cell)
                            .or_default()
                            .elements
                            .push(element(pand, part, &p, &t));
                        total += 1;
                    }
                    ctx.log(format!(
                        "[{tiles_done}/{}] {}: {} buildings ({total} total, {} cells)",
                        urls.len(),
                        url.rsplit('/').next().unwrap_or(url),
                        total - n_before,
                        cells.len()
                    ));
                }
            }
            progress(tiles_done);
            Ok(())
        },
    )?;

    for (cell, bin) in &cells {
        write_cell(ctx, *cell, bin)?;
    }
    ctx.write_json(
        "lod22/coverage.json",
        &json!({
            "name": ctx.name,
            "zoom": CACHE_ZOOM,
            "lod": "2.2",
            "attribution": ATTRIBUTION,
            "source": "3DBAG CityJSON LoD2.2",
            "bbox_rd": ctx.bbox,
            "cells": sorted_cells(cells.keys().copied()),
            "building_count": total,
        }),
    )?;
    ctx.log(format!(
        "done: {total} LoD2.2 buildings across {} cells",
        cells.len()
    ));
    Ok(DatasetResult {
        cells: cells.len(),
        counts: vec![Count::new("buildings", total)],
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pand_ids_keep_their_leading_zeros_and_split_the_part() {
        assert_eq!(
            object_id("NL.IMBAG.Pand.0014100010903891-0"),
            ("0014100010903891".into(), 0)
        );
        assert_eq!(
            object_id("NL.IMBAG.Pand.0014100010903891-2"),
            ("0014100010903891".into(), 2)
        );
        assert_eq!(
            object_id("0014100010903891"),
            ("0014100010903891".into(), 0)
        );
        assert_eq!(object_id("something-else"), ("something-else".into(), 0));
        assert_eq!(
            object_id("X0014100010903891"),
            ("X0014100010903891".into(), 0)
        );
    }

    /// A vertical wall with a window: a square outer ring and a square hole,
    /// triangulated on its own plane. Covers the area and never uses a triangle
    /// inside the hole.
    #[test]
    fn a_wall_with_a_hole_triangulates_on_its_plane() {
        let pts = [
            [0.0, 0.0, 0.0],
            [4.0, 0.0, 0.0],
            [4.0, 0.0, 4.0],
            [0.0, 0.0, 4.0],
            [1.0, 0.0, 1.0],
            [1.0, 0.0, 3.0],
            [3.0, 0.0, 3.0],
            [3.0, 0.0, 1.0],
        ];
        let tris = triangulate_surface(&pts, &[4, 4]);
        let area: f64 = tris
            .iter()
            .map(|&[a, b, c]| {
                let (p, q, r) = (pts[a], pts[b], pts[c]);
                len(cross(
                    [q[0] - p[0], q[1] - p[1], q[2] - p[2]],
                    [r[0] - p[0], r[1] - p[1], r[2] - p[2]],
                )) / 2.0
            })
            .sum();
        assert!(
            (area - 12.0).abs() < 1e-9,
            "covered {area} m², the wall less its window is 12"
        );
    }

    #[test]
    fn a_degenerate_surface_is_nothing() {
        assert!(
            triangulate_surface(&[[0.0; 3], [1.0, 0.0, 0.0], [2.0, 0.0, 0.0]], &[3]).is_empty()
        );
    }

    struct Quiet;
    impl crate::pipeline::Sink for Quiet {
        fn log(&self, _: String) {}
        fn progress(&self, _: crate::pipeline::Progress) {}
    }

    /// A cell is written three times over — the JSON as before, the binary, and
    /// the binary gzipped for the host to serve — and the binary decodes to the
    /// numbers the JSON spells out.
    #[test]
    fn a_cell_is_written_as_json_binary_and_gzipped_binary() {
        use std::io::Read;
        let dir = tempfile::tempdir().unwrap();
        let ctx = Ctx::new(
            [0.0; 4],
            "t",
            dir.path(),
            std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false)),
            std::sync::Arc::new(Quiet),
        );
        let p = vec![
            [round(53.226_222_14, 7), round(6.554_059_31, 7), 0.0],
            [
                round(53.226_242_9, 7),
                round(6.554_043_5, 7),
                round(6.318, 2),
            ],
            [
                round(53.226_234_5, 7),
                round(6.554_107_2, 7),
                round(3.704_9, 2),
            ],
        ];
        let t = vec![[0u32, 1, 2, 1]];
        let pand = "0014100010904218".to_string();
        let el = json!({"id": pand, "p": p, "t": t, "part": 2});
        let bin = lod22_cell::Cell {
            elements: vec![element(pand.clone(), 2, &p, &t)],
            pands: vec![],
        };
        write_cell(&ctx, (8490, 5319), &bin).unwrap();

        let root = dir.path().join("lod22/14/8490");
        let text = std::fs::read(root.join("5319.json")).unwrap();
        // Byte for byte what the `json!` writer this replaced produced.
        assert_eq!(
            text,
            serde_json::to_vec(&json!({"elements": [el]})).unwrap(),
            "the JSON cell changed shape"
        );
        let json: Value = serde_json::from_slice(&text).unwrap();
        assert_eq!(json["elements"][0]["id"], "0014100010904218");
        let raw = std::fs::read(root.join("5319.bin")).unwrap();
        let mut unzipped = Vec::new();
        flate2::read::GzDecoder::new(std::fs::read(root.join("5319.bin.gz")).unwrap().as_slice())
            .read_to_end(&mut unzipped)
            .unwrap();
        assert_eq!(
            unzipped, raw,
            "the sidecar is not the file it stands beside"
        );

        let decoded = lod22_cell::decode(&raw).unwrap();
        let e = &decoded.elements[0];
        assert_eq!((e.id.as_str(), e.part), ("0014100010904218", 2));
        assert_eq!(e.t, vec![(0, 1, 2, 1)]);
        for (got, want) in e.p.iter().zip(json["elements"][0]["p"].as_array().unwrap()) {
            assert_eq!(got.0, want[0].as_f64().unwrap());
            assert_eq!(got.1, want[1].as_f64().unwrap());
            assert_eq!(got.2, want[2].as_f64().unwrap() as f32);
        }
    }
}
