//! Which step of the ground cut makes the hairline triangles: build one live
//! cell's sheets stage by stage and histogram the triangle widths after each.
//!
//! `cargo run -p cartopolis_atlas --example ground_stages -- 8490 5319`

use std::io::Read;

use cartopolis_core::world::{tile_frac_to_lat_lng, tile_to_meters};
use cartopolis_geo::formats::roofs;
use cartopolis_geo::surveyed_ground::{self, CropCell};
use cartopolis_geo::vector_tiles::{self, SurfaceMesh, TileData};
use cartopolis_geo::{ground_sheets, paving, paving_edges};

fn get(url: &str) -> Option<Vec<u8>> {
    let r = ureq::get(url).call().ok()?;
    let mut v = Vec::new();
    r.into_reader().read_to_end(&mut v).ok()?;
    Some(v)
}

fn width(m: &SurfaceMesh, t: usize) -> f64 {
    let p = |i: usize| m.positions[m.indices[t * 3 + i] as usize];
    let (a, b, c) = (p(0), p(1), p(2));
    let e = |u: [f32; 3], v: [f32; 3]| {
        (f64::from(u[0] - v[0]).powi(2) + f64::from(u[2] - v[2]).powi(2)).sqrt()
    };
    let longest = e(a, b).max(e(b, c)).max(e(c, a));
    let area2 = (f64::from(b[0] - a[0]) * f64::from(c[2] - a[2])
        - f64::from(b[2] - a[2]) * f64::from(c[0] - a[0]))
    .abs();
    if longest > 0.0 {
        area2 / longest
    } else {
        0.0
    }
}

fn hist(label: &str, meshes: &[SurfaceMesh]) {
    let bounds = [0.00025f64, 0.001, 0.005, 0.02, f64::INFINITY];
    let mut h = [0usize; 5];
    let mut tris = 0;
    let mut verts = 0;
    for m in meshes {
        verts += m.positions.len();
        for t in 0..m.indices.len() / 3 {
            tris += 1;
            let w = width(m, t);
            h[bounds.iter().position(|&b| w < b).unwrap()] += 1;
        }
    }
    println!(
        "{label:<44} {tris:>8} tris {verts:>8} verts | <0.25mm {:>6} <1mm {:>6} <5mm {:>6} <2cm {:>6}",
        h[0], h[1], h[2], h[3]
    );
}

/// Ring vertices, and how many lie within 1 mm of the chord between their
/// neighbours.
fn ring_stats(label: &str, rings: impl Iterator<Item = Vec<[f32; 2]>>) {
    let (mut n, mut near, mut rings_n) = (0usize, 0usize, 0usize);
    for r in rings {
        rings_n += 1;
        let m = r.len();
        if m < 3 {
            continue;
        }
        for i in 0..m {
            let (a, b, c) = (r[(i + m - 1) % m], r[i], r[(i + 1) % m]);
            let (dx, dy) = (c[0] - a[0], c[1] - a[1]);
            let l = (dx * dx + dy * dy).sqrt();
            let d = if l > 0.0 {
                (dx * (b[1] - a[1]) - dy * (b[0] - a[0])).abs() / l
            } else {
                ((b[0] - a[0]).powi(2) + (b[1] - a[1]).powi(2)).sqrt()
            };
            n += 1;
            if d < 0.001 {
                near += 1;
            }
        }
    }
    println!("{label:<44} {rings_n:>8} rings {n:>8} verts, {near} within 1 mm of their chord");
}

fn main() {
    let x: i64 = std::env::args().nth(1).unwrap().parse().unwrap();
    let y: i64 = std::env::args().nth(2).unwrap().parse().unwrap();
    let h = "https://3dbag.cartopolis.org";
    let body = get(&format!(
        "https://tiles.cartopolis.org/tiles/osm/14/{x}/{y}"
    ))
    .expect("tile");
    let text = |p: String| get(&format!("{h}/{p}")).and_then(|b| String::from_utf8(b).ok());
    let paving_in = text(format!("bgt/14/{x}/{y}.surfaces.json"))
        .and_then(|t| surveyed_ground::parse_surveyed_paving(&t).ok())
        .unwrap_or_default();
    let measured: roofs::Cell = text(format!("roofs/14/{x}/{y}.json"))
        .and_then(|t| roofs::parse(&t).ok())
        .unwrap_or_default();
    let crops = CropCell::default();
    let decks = text(format!("decks/14/{x}/{y}.json"))
        .and_then(|t| cartopolis_geo::formats::decks::parse(&t).ok())
        .map(surveyed_ground::atlas_decks);
    let tile = TileData::parse(&body).unwrap();
    let (lat, _) = tile_frac_to_lat_lng(x as f64 + 0.5, y as f64 + 0.5, 14);
    let tile_m = tile_to_meters(14, lat) as f32;
    let paved = surveyed_ground::surveyed_paving(&paving_in, &measured, x, y, tile_m);
    let parcels = surveyed_ground::crop_parcels(&crops, x, y, tile_m);
    println!("cell {x}/{y}: {} paved areas, tile_m {tile_m}", paved.len());

    ring_stats(
        "paved rings as surveyed",
        paved.iter().flat_map(|a| a.rings.iter().cloned()),
    );
    let pieces = paving_edges::refine(&paved);
    let with_edge = pieces.iter().filter(|p| p.edge.is_some()).count();
    println!("refine: {} pieces, {with_edge} bands", pieces.len());
    ring_stats(
        "rings after refine (bands + rests)",
        pieces.iter().flat_map(|p| p.area.rings.iter().cloned()),
    );
    ring_stats(
        "  bands only",
        pieces
            .iter()
            .filter(|p| p.edge.is_some())
            .flat_map(|p| p.area.rings.iter().cloned()),
    );
    ring_stats(
        "  rests only",
        pieces
            .iter()
            .filter(|p| p.edge.is_none())
            .flat_map(|p| p.area.rings.iter().cloned()),
    );

    let paving_meshes = paving::build_paving_meshes(&paved, None);
    hist("A paving meshes (after refine + earcut)", &paving_meshes);
    // The same without edges: areas no band applies to are unchanged by refine,
    // so build from the pieces that are rests of areas with no band at all.
    let plain: Vec<_> = paved
        .iter()
        .map(|a| paving::PavedArea {
            kind: paving::PavingKind::Footway,
            material: None,
            rings: a.rings.clone(),
            measured: a.measured,
        })
        .collect();
    let plain_meshes = paving::build_paving_meshes(&plain, None);
    hist("A0 paving as footway only (no kerb/worn)", &plain_meshes);

    let landuse = vector_tiles::tessellate_surfaces(&tile, tile_m, None).unwrap();
    hist("B landuse fills (tessellate_surfaces)", &landuse);

    let mut decks_t = vector_tiles::tessellate_bridges(&tile, tile_m).unwrap_or_default();
    let cover = ground_sheets::surveyed_cover(&paved);
    let roads =
        vector_tiles::tessellate_roads_surveyed(&tile, tile_m, &mut decks_t, None, cover).unwrap();
    hist("C roads.ground", &roads.ground);

    let mut cut_paving = paving_meshes.clone();
    paving::cut_paving_under_roads(&mut cut_paving, &roads.ground);
    hist("D paving cut under roads", &cut_paving);

    let mut ground = landuse.clone();
    paving::cut_ground_under_roads_and_paving(&mut ground, &roads.ground, &paving_meshes);
    hist("E landuse cut under roads + paving", &ground);

    let all = ground_sheets::build_sheets(&tile, tile_m, None, &paved, &parcels, decks.as_deref())
        .unwrap();
    hist("F build_sheets (the file)", &all);
    let file = ground_sheets::encode(ground_sheets::GROUND_REVISION, 0, tile_m, &all);
    let params = brotli::enc::BrotliEncoderParams {
        quality: 10,
        lgwin: 22,
        size_hint: file.len(),
        ..Default::default()
    };
    let mut out = Vec::new();
    brotli::BrotliCompress(&mut &file[..], &mut out, &params).unwrap();
    println!(
        "file: {} bytes raw, {} after Brotli q10",
        file.len(),
        out.len()
    );
    for g in ["Paving", "Built", "Grass"] {
        let sel: Vec<_> = all
            .iter()
            .filter(|m| format!("{:?}", m.group) == g)
            .cloned()
            .collect();
        hist(&format!("  F {g}"), &sel);
    }
}
