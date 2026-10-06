//! Garden plots split into planted and hard ground, measured from the infrared
//! aerial photograph (`roofs/14/{x}/{y}.json`'s `gardens`,
//! `cartopolis_geo::formats::roofs::Garden`).
//!
//! The BGT records a back garden only as `erf` — private ground — and nothing
//! about what is on it, so the client drew every one as paving. PDOK's
//! colour-infrared photograph carries near-infrared in its red channel and red
//! light in its green, so `(R − G) / (R + G)` is a vegetation index measured on
//! the day the plane flew. That answers *planted or hard* well; it does not
//! answer lawn, shrub or tree (measured 2026-10-06 over Groningen, Den Haag and
//! Leidsche Rijn: planted-versus-hard matched the photograph by eye, lawn-versus-
//! tree did not, and AHN's height cannot tell them apart either, being flown
//! leaf-off). So only the two-way split is made, and the trees stay with the
//! layers that already draw them.
//!
//! Per garden:
//!
//!   1. Average the index over ~1 m cells inside the plot (four by four of the
//!      cell photo's ~0.23 m pixels). JPEG keeps colour at reduced resolution, so
//!      finer than that is noise.
//!   2. Smooth once with a 3×3 majority, so a single dark cell in a lawn is not
//!      a paving slab.
//!   3. A plot that is nearly all one or the other is left whole.
//!   4. Union the planted cells, straighten the staircase, and cut the plot by
//!      it: the planted side and the hard side come out of one overlay, so they
//!      cover the plot exactly once.
//!
//! The photograph is not a true orthophoto: a tall building leans over the
//! garden behind it, and its roof reads as hard ground. That is the known error
//! of this measurement and is left in rather than guessed around.
//!
//! Imagery: Beeldmateriaal Nederland via PDOK, CC BY 4.0.

use std::time::Duration;

use cartopolis_geo::formats::roofs::Garden;
use i_overlay::core::fill_rule::FillRule;
use i_overlay::core::overlay::{Overlay, ShapeType};
use i_overlay::core::overlay_rule::OverlayRule;
use i_overlay::i_float::int::point::IntPoint;
use image::RgbImage;

use super::http::{get_parsed_with, query};
use super::tile::{Cell, CACHE_ZOOM};
use super::{Ctx, Outcome};

const WMS: &str = "https://service.pdok.nl/hwh/luchtfotocir/wms/v1_0";
/// Summer, leaf-on, 25 cm. The spring high-resolution flight is sharper and
/// better registered, but its long shadows and bare trees read as hard ground.
const LAYER: &str = "2025_ortho25IR";

fn layer() -> String {
    std::env::var("ATLAS_GARDENS_LAYER").unwrap_or_else(|_| LAYER.to_string())
}

/// The vegetation index above which a cell is planted. Calibrated on the 2025
/// summer flight; the photograph is an uncalibrated JPEG, so another year's
/// flight needs its own.
const PLANTED: f32 = 0.10;
/// Photo pixels per side of one classification cell.
const STEP: u32 = 4;
/// A cell must have this many of its pixels inside the plot to vote.
const MIN_INSIDE: u32 = 4;
/// A plot under this share planted is drawn all hard, over the complement all
/// planted: a split that small is more likely the photograph than the garden.
const WHOLE: f32 = 0.08;
/// How far the planted outline may move when its staircase is straightened,
/// in photo pixels (~0.7 m). It has to clear a diagonal staircase of `STEP`
/// cells, whose corners stand `STEP / √2` ≈ 2.8 px off the line: at the first
/// setting, 2 px, nothing was straightened and Groningen's centre cell shipped
/// 186,000 garden vertices.
const SIMPLIFY_PX: f64 = 3.0;
/// A planted or hard piece smaller than this, in photo pixels (~4 m² at the
/// cell photo's 0.23 m), goes to the other side. The cut leaves crumbs wherever
/// the straightened outline runs along the plot's own edge — 16,581 of the
/// 21,563 pieces in Groningen's centre cell were under 4 m² — and a crumb is
/// nothing anybody sees but is a polygon every client triangulates.
const MIN_PIECE_PX: f64 = 76.0;
/// Sub-pixel lattice for the overlay.
const LATTICE: f64 = 8.0;

/// What a block of the cell photo is fetched from, in the same Web Mercator
/// layout `roofs` uses for its own photograph, so one pixel means the same
/// ground in both.
pub fn block_url(cell: Cell, i: u32, j: u32, blocks: u32, block_px: u32) -> String {
    const HALF: f64 = 20_037_508.342_789_244;
    let n = f64::from(1u32 << CACHE_ZOOM) * f64::from(blocks);
    let tx = (cell.0 * i64::from(blocks) + i64::from(i)) as f64;
    let ty = (cell.1 * i64::from(blocks) + i64::from(j)) as f64;
    let mx = |x: f64| x / n * 2.0 * HALF - HALF;
    let my = |y: f64| HALF - y / n * 2.0 * HALF;
    let bbox = format!("{},{},{},{}", mx(tx), my(ty + 1.0), mx(tx + 1.0), my(ty));
    let px = block_px.to_string();
    format!(
        "{WMS}?{}",
        query(&[
            ("service", "WMS"),
            ("version", "1.3.0"),
            ("request", "GetMap"),
            ("layers", &layer()),
            ("styles", ""),
            ("crs", "EPSG:3857"),
            ("bbox", &bbox),
            ("width", &px),
            ("height", &px),
            ("format", "image/jpeg"),
        ])
    )
}

pub fn fetch_block(ctx: &Ctx, url: &str, block_px: u32) -> Outcome<RgbImage> {
    get_parsed_with(
        ctx,
        url,
        "infrared photo",
        Duration::from_secs(120),
        4,
        |b| {
            let img = image::load_from_memory_with_format(b, image::ImageFormat::Jpeg)
                .map_err(|e| format!("not a JPEG ({e})"))?
                .to_rgb8();
            if img.dimensions() == (block_px, block_px) {
                Ok(img)
            } else {
                Err(format!("block is {:?}", img.dimensions()))
            }
        },
    )
}

/// The vegetation index of one infrared pixel: near-infrared is the red
/// channel, red light the green one.
#[must_use]
pub fn index(p: [u8; 3]) -> f32 {
    let (nir, red) = (f32::from(p[0]), f32::from(p[1]));
    if nir + red < 1.0 {
        0.0
    } else {
        (nir - red) / (nir + red)
    }
}

/// Split one plot, given in photo pixels (outer ring first), by the index.
/// `None` when the plot is too small to say anything about. Otherwise the
/// planted and hard polygons, in photo pixels.
#[must_use]
pub fn split(
    rings: &[Vec<[f64; 2]>],
    size: u32,
    ndvi: impl Fn(u32, u32) -> f32,
) -> Option<(Vec<Vec<Vec<[f64; 2]>>>, Vec<Vec<Vec<[f64; 2]>>>)> {
    let outer = rings.first()?;
    let (mut x0, mut y0, mut x1, mut y1) = (f64::MAX, f64::MAX, f64::MIN, f64::MIN);
    for p in outer {
        x0 = x0.min(p[0]);
        y0 = y0.min(p[1]);
        x1 = x1.max(p[0]);
        y1 = y1.max(p[1]);
    }
    let clamp = |v: f64| v.clamp(0.0, f64::from(size - 1)) as u32;
    let (gx0, gy0) = (clamp(x0) / STEP, clamp(y0) / STEP);
    let (gx1, gy1) = (clamp(x1) / STEP, clamp(y1) / STEP);
    let (w, h) = ((gx1 - gx0 + 1) as usize, (gy1 - gy0 + 1) as usize);
    // Per classification cell: pixels inside, and the index summed over them.
    let mut inside = vec![0u32; w * h];
    let mut sum = vec![0f32; w * h];
    fill(rings, size, |x, y| {
        let (gx, gy) = (x / STEP, y / STEP);
        if gx < gx0 || gy < gy0 || gx > gx1 || gy > gy1 {
            return;
        }
        let i = (gy - gy0) as usize * w + (gx - gx0) as usize;
        inside[i] += 1;
        sum[i] += ndvi(x, y);
    });
    let vote: Vec<Option<bool>> = (0..w * h)
        .map(|i| (inside[i] >= MIN_INSIDE).then(|| sum[i] / inside[i] as f32 > PLANTED))
        .collect();
    let voters = vote.iter().flatten().count();
    if voters == 0 {
        return None;
    }
    // One pass of a 3×3 majority among the cells that voted.
    let smooth: Vec<bool> = (0..w * h)
        .map(|i| {
            let (cx, cy) = ((i % w) as i64, (i / w) as i64);
            let (mut yes, mut all) = (0, 0);
            for dy in -1..=1 {
                for dx in -1..=1 {
                    let (nx, ny) = (cx + dx, cy + dy);
                    if nx < 0 || ny < 0 || nx >= w as i64 || ny >= h as i64 {
                        continue;
                    }
                    if let Some(v) = vote[ny as usize * w + nx as usize] {
                        all += 1;
                        yes += i32::from(v);
                    }
                }
            }
            vote[i].is_some() && yes * 2 > all
        })
        .collect();
    let planted = smooth.iter().filter(|&&v| v).count() as f32 / voters as f32;
    let whole = |r: &[Vec<[f64; 2]>]| vec![r.to_vec()];
    if planted < WHOLE {
        return Some((Vec::new(), whole(rings)));
    }
    if planted > 1.0 - WHOLE {
        return Some((whole(rings), Vec::new()));
    }
    // The planted cells as one outline.
    let mut union: Overlay<i32> = Overlay::new(64);
    let q = |v: f64| (v * LATTICE).round() as i32;
    for (i, _) in smooth.iter().enumerate().filter(|(_, &v)| v) {
        let x = f64::from(gx0 + (i % w) as u32) * f64::from(STEP);
        let y = f64::from(gy0 + (i / w) as u32) * f64::from(STEP);
        let s = f64::from(STEP);
        union.add_contour(
            &[
                IntPoint::new(q(x), q(y)),
                IntPoint::new(q(x + s), q(y)),
                IntPoint::new(q(x + s), q(y + s)),
                IntPoint::new(q(x), q(y + s)),
            ],
            ShapeType::Subject,
        );
    }
    let outline: Vec<Vec<IntPoint<i32>>> = union
        .overlay(OverlayRule::Subject, FillRule::NonZero)
        .into_iter()
        .flatten()
        .map(|c| simplify(&c, SIMPLIFY_PX * LATTICE))
        .filter(|c| c.len() >= 3)
        .collect();
    let plot: Vec<Vec<IntPoint<i32>>> = rings
        .iter()
        .map(|r| r.iter().map(|p| IntPoint::new(q(p[0]), q(p[1]))).collect())
        .collect();
    // Cut the plot, then hand every crumb to the other side. Each side is
    // recomputed from the plot rather than patched, so the two still cover it
    // exactly once: drop the small planted pieces, take the hard side as what is
    // left, drop its small pieces, and the planted side is what is left of that.
    let min = MIN_PIECE_PX * LATTICE * LATTICE;
    let big = |shapes: Vec<Vec<Vec<IntPoint<i32>>>>| -> Vec<Vec<Vec<IntPoint<i32>>>> {
        shapes
            .into_iter()
            .filter(|s| shape_area(s) >= min)
            .collect()
    };
    let planted = big(cut(&plot, outline.iter(), OverlayRule::Intersect));
    if planted.is_empty() {
        return Some((Vec::new(), whole(rings)));
    }
    let paved = big(cut(
        &plot,
        planted.iter().flatten(),
        OverlayRule::Difference,
    ));
    if paved.is_empty() {
        return Some((whole(rings), Vec::new()));
    }
    let planted = cut(&plot, paved.iter().flatten(), OverlayRule::Difference);
    let px = |shapes: Vec<Vec<Vec<IntPoint<i32>>>>| -> Vec<Vec<Vec<[f64; 2]>>> {
        shapes
            .into_iter()
            .map(|shape| {
                shape
                    .into_iter()
                    .map(|c| {
                        c.into_iter()
                            .map(|p| [f64::from(p.x) / LATTICE, f64::from(p.y) / LATTICE])
                            .collect()
                    })
                    .collect()
            })
            .collect()
    };
    Some((px(planted), px(paved)))
}

/// The plot overlaid with `clip` by `rule`, as shapes (outer contour first).
fn cut<'a>(
    plot: &[Vec<IntPoint<i32>>],
    clip: impl Iterator<Item = &'a Vec<IntPoint<i32>>>,
    rule: OverlayRule,
) -> Vec<Vec<Vec<IntPoint<i32>>>> {
    let mut o: Overlay<i32> = Overlay::new(64);
    for c in plot {
        o.add_contour(c, ShapeType::Subject);
    }
    for c in clip {
        o.add_contour(c, ShapeType::Clip);
    }
    o.overlay(rule, FillRule::NonZero)
        .into_iter()
        .map(|shape| {
            shape
                .into_iter()
                .filter(|c| c.len() >= 3)
                .collect::<Vec<_>>()
        })
        .filter(|s| !s.is_empty())
        .collect()
}

/// A shape's area — outer contour less its holes — in lattice units.
fn shape_area(shape: &[Vec<IntPoint<i32>>]) -> f64 {
    let ring = |c: &Vec<IntPoint<i32>>| {
        let mut s = 0i64;
        for i in 0..c.len() {
            let (a, b) = (c[i], c[(i + 1) % c.len()]);
            s += i64::from(a.x) * i64::from(b.y) - i64::from(b.x) * i64::from(a.y);
        }
        (s as f64 / 2.0).abs()
    };
    ring(&shape[0]) - shape[1..].iter().map(ring).sum::<f64>()
}

/// Douglas–Peucker on a closed contour.
fn simplify(c: &[IntPoint<i32>], tol: f64) -> Vec<IntPoint<i32>> {
    if c.len() < 4 {
        return c.to_vec();
    }
    // Split the ring at its first point and the point farthest from it.
    let far = (1..c.len())
        .max_by_key(|&i| {
            let (dx, dy) = (i64::from(c[i].x - c[0].x), i64::from(c[i].y - c[0].y));
            dx * dx + dy * dy
        })
        .unwrap_or(1);
    let mut keep = vec![false; c.len()];
    keep[0] = true;
    keep[far] = true;
    dp(c, 0, far, tol, &mut keep);
    let mut tail: Vec<IntPoint<i32>> = c[far..].to_vec();
    tail.push(c[0]);
    let mut keep_tail = vec![false; tail.len()];
    keep_tail[0] = true;
    let last = tail.len() - 1;
    keep_tail[last] = true;
    dp(&tail, 0, last, tol, &mut keep_tail);
    for (i, k) in keep_tail.iter().enumerate().take(last) {
        if *k {
            keep[far + i] = true;
        }
    }
    c.iter()
        .zip(keep)
        .filter_map(|(p, k)| k.then_some(*p))
        .collect()
}

fn dp(c: &[IntPoint<i32>], a: usize, b: usize, tol: f64, keep: &mut [bool]) {
    if b <= a + 1 {
        return;
    }
    let (ax, ay) = (f64::from(c[a].x), f64::from(c[a].y));
    let (bx, by) = (f64::from(c[b].x), f64::from(c[b].y));
    let len = ((bx - ax).powi(2) + (by - ay).powi(2)).sqrt().max(1e-9);
    let (mut worst, mut at) = (0.0, a);
    for (i, p) in c.iter().enumerate().take(b).skip(a + 1) {
        let d = ((bx - ax) * (ay - f64::from(p.y)) - (ax - f64::from(p.x)) * (by - ay)).abs() / len;
        if d > worst {
            worst = d;
            at = i;
        }
    }
    if worst > tol {
        keep[at] = true;
        dp(c, a, at, tol, keep);
        dp(c, at, b, tol, keep);
    }
}

/// Every pixel inside a polygon (even-odd over all rings), by scanline.
fn fill(rings: &[Vec<[f64; 2]>], size: u32, mut visit: impl FnMut(u32, u32)) {
    let (mut y0, mut y1) = (f64::MAX, f64::MIN);
    for p in rings.iter().flatten() {
        y0 = y0.min(p[1]);
        y1 = y1.max(p[1]);
    }
    let ys = y0.floor().max(0.0) as u32;
    let ye = (y1.ceil().max(0.0) as u32).min(size);
    let mut xs: Vec<f64> = Vec::new();
    for y in ys..ye {
        let cy = f64::from(y) + 0.5;
        xs.clear();
        for r in rings {
            for i in 0..r.len() {
                let (a, b) = (r[i], r[(i + 1) % r.len()]);
                if (a[1] > cy) != (b[1] > cy) {
                    xs.push(a[0] + (cy - a[1]) / (b[1] - a[1]) * (b[0] - a[0]));
                }
            }
        }
        xs.sort_by(f64::total_cmp);
        for pair in xs.chunks_exact(2) {
            let from = (pair[0] - 0.5).ceil().max(0.0) as u32;
            let to = ((pair[1] - 0.5).floor().max(-1.0) as i64).min(i64::from(size) - 1);
            for x in from..=to.max(0) as u32 {
                if i64::from(x) <= to {
                    visit(x, y);
                }
            }
        }
    }
}

/// A split garden in the file's `[lat, lng]`, from photo pixels.
#[must_use]
pub fn to_lat_lng(
    cell: Cell,
    cell_px: u32,
    planted: &[Vec<Vec<[f64; 2]>>],
    paved: &[Vec<Vec<[f64; 2]>>],
) -> Garden {
    let n = f64::from(1u32 << CACHE_ZOOM);
    let ll = |p: &[f64; 2]| -> (f64, f64) {
        let fx = cell.0 as f64 + p[0] / f64::from(cell_px);
        let fy = cell.1 as f64 + p[1] / f64::from(cell_px);
        let lng = fx / n * 360.0 - 180.0;
        let lat = (std::f64::consts::PI * (1.0 - 2.0 * fy / n))
            .sinh()
            .atan()
            .to_degrees();
        let r = |v: f64| (v * 1e7).round() / 1e7;
        (r(lat), r(lng))
    };
    let conv = |polys: &[Vec<Vec<[f64; 2]>>]| -> Vec<Vec<Vec<(f64, f64)>>> {
        polys
            .iter()
            .map(|poly| {
                poly.iter()
                    .map(|ring| ring.iter().map(ll).collect())
                    .collect()
            })
            .collect()
    };
    Garden {
        planted: conv(planted),
        paved: conv(paved),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn area(polys: &[Vec<Vec<[f64; 2]>>]) -> f64 {
        let ring = |r: &Vec<[f64; 2]>| {
            let mut s = 0.0;
            for i in 0..r.len() {
                let (a, b) = (r[i], r[(i + 1) % r.len()]);
                s += a[0] * b[1] - b[0] * a[1];
            }
            (s / 2.0).abs()
        };
        polys
            .iter()
            .map(|p| ring(&p[0]) - p[1..].iter().map(ring).sum::<f64>())
            .sum()
    }

    /// A 40 × 40 px plot whose left half the photograph shows green: the split
    /// lands down the middle, and the two halves cover the plot once.
    #[test]
    fn a_half_green_plot_splits_down_the_middle() {
        let plot = vec![vec![[10.0, 10.0], [50.0, 10.0], [50.0, 50.0], [10.0, 50.0]]];
        let (planted, paved) = split(&plot, 100, |x, _| if x < 30 { 0.4 } else { -0.05 }).unwrap();
        let (a, b) = (area(&planted), area(&paved));
        assert!((a + b - 1600.0).abs() < 1.0, "{a} + {b}");
        // Within one classification cell of the line: the cell straddling it votes
        // as a whole.
        assert!((a - 800.0).abs() <= f64::from(STEP) * 40.0, "planted {a}");
    }

    /// A lawn with a ragged diagonal edge and a speckle of noise: no piece on
    /// either side is a crumb, the two still cover the plot once, and the
    /// staircase is straightened rather than shipped cell by cell.
    #[test]
    fn a_ragged_split_leaves_no_crumbs() {
        let plot = vec![vec![[3.0, 5.0], [203.0, 5.0], [203.0, 155.0], [3.0, 155.0]]];
        let hash = |x: u32, y: u32| {
            (x / STEP).wrapping_mul(73_856_093) ^ (y / STEP).wrapping_mul(19_349_663)
        };
        let (planted, paved) = split(&plot, 256, |x, y| {
            let lawn = x + y / 2 < 140;
            let speck = hash(x, y) % 23 == 0;
            if lawn != speck {
                0.4
            } else {
                -0.05
            }
        })
        .unwrap();
        let (a, b) = (area(&planted), area(&paved));
        assert!((a + b - 30_000.0).abs() < 1.0, "{a} + {b}");
        for p in planted.iter().chain(&paved) {
            assert!(
                area(std::slice::from_ref(p)) >= MIN_PIECE_PX,
                "crumb {}",
                area(std::slice::from_ref(p))
            );
        }
        let vertices: usize = planted.iter().chain(&paved).flatten().map(Vec::len).sum();
        assert!(vertices < 40, "{vertices} vertices");
    }

    /// Nearly all green is left whole rather than nibbled.
    #[test]
    fn a_lawn_stays_whole() {
        let plot = vec![vec![[0.0, 0.0], [40.0, 0.0], [40.0, 40.0], [0.0, 40.0]]];
        let (planted, paved) = split(
            &plot,
            100,
            |x, y| if x == 20 && y == 20 { -0.2 } else { 0.4 },
        )
        .unwrap();
        assert!(paved.is_empty());
        assert_eq!(planted.len(), 1);
    }

    /// The index reads near-infrared from red and red light from green.
    #[test]
    fn the_index_reads_the_infrared_channels() {
        assert!(index([200, 60, 80]) > 0.5, "bright NIR, little red: leaves");
        assert!(index([90, 100, 100]).abs() < 0.1, "paving");
    }
}

/// The split over a real neighbourhood, drawn over its photograph: the check
/// that the planted side is where the photograph is green, which no synthetic
/// test can make. Live: fetches the BGT plots and both photographs.
/// `ATLAS_GARDEN_BBOX=lng0,lat0,lng1,lat1` (keep it ~150 m),
/// `ATLAS_GARDEN_OUT=plan.png`.
#[cfg(test)]
mod real_area {
    use super::*;

    fn get(url: &str) -> Vec<u8> {
        let mut out = Vec::new();
        std::io::Read::read_to_end(
            &mut ureq::get(url).call().expect(url).into_reader(),
            &mut out,
        )
        .unwrap();
        out
    }

    #[test]
    #[ignore = "network; draws a plan to look at"]
    fn plan_of_real_gardens() {
        let b: Vec<f64> = std::env::var("ATLAS_GARDEN_BBOX")
            .expect("ATLAS_GARDEN_BBOX")
            .split(',')
            .map(|v| v.parse().unwrap())
            .collect();
        let out = std::env::var("ATLAS_GARDEN_OUT").expect("ATLAS_GARDEN_OUT");
        // Web Mercator metres of the box, and a 0.23 m pixel like `roofs`.
        const R: f64 = 6_378_137.0;
        let mx = |lng: f64| lng.to_radians() * R;
        let my = |lat: f64| (lat.to_radians().tan() + 1.0 / lat.to_radians().cos()).ln() * R;
        let (x0, y0, x1, y1) = (mx(b[0]), my(b[1]), mx(b[2]), my(b[3]));
        let px_m = 0.23 / b[1].to_radians().cos();
        let (w, h) = (((x1 - x0) / px_m) as u32, ((y1 - y0) / px_m) as u32);
        let photo = |wms: &str, layer: &str| {
            let url = format!(
                "{wms}?service=WMS&version=1.3.0&request=GetMap&layers={layer}&styles=&crs=EPSG:3857\
                 &bbox={x0},{y0},{x1},{y1}&width={w}&height={h}&format=image/jpeg"
            );
            image::load_from_memory(&get(&url)).unwrap().to_rgb8()
        };
        let cir = photo(WMS, LAYER);
        let rgb = photo(
            "https://service.pdok.nl/hwh/luchtfotorgb/wms/v1_0",
            "2025_ortho25",
        );
        let pixel = |lng: f64, lat: f64| [(mx(lng) - x0) / px_m, (y1 - my(lat)) / px_m];
        let mut url = format!(
            "https://api.pdok.nl/lv/bgt/ogc/v1/collections/onbegroeidterreindeel/items\
             ?bbox={},{},{},{}&limit=1000&f=json",
            b[0], b[1], b[2], b[3]
        );
        let mut plots: Vec<Vec<Vec<[f64; 2]>>> = Vec::new();
        loop {
            let page: serde_json::Value = serde_json::from_slice(&get(&url)).unwrap();
            for f in page["features"].as_array().unwrap() {
                let p = &f["properties"];
                if !p["termination_date"].is_null()
                    || !p["eind_registratie"].is_null()
                    || p["fysiek_voorkomen"] != "erf"
                {
                    continue;
                }
                let polys: Vec<&serde_json::Value> = match f["geometry"]["type"].as_str() {
                    Some("Polygon") => vec![&f["geometry"]["coordinates"]],
                    Some("MultiPolygon") => f["geometry"]["coordinates"]
                        .as_array()
                        .unwrap()
                        .iter()
                        .collect(),
                    _ => continue,
                };
                for poly in polys {
                    plots.push(
                        poly.as_array()
                            .unwrap()
                            .iter()
                            .map(|ring| {
                                ring.as_array()
                                    .unwrap()
                                    .iter()
                                    .map(|c| pixel(c[0].as_f64().unwrap(), c[1].as_f64().unwrap()))
                                    .collect()
                            })
                            .collect(),
                    );
                }
            }
            let next = page["links"]
                .as_array()
                .and_then(|l| l.iter().find(|l| l["rel"] == "next"))
                .and_then(|l| l["href"].as_str())
                .map(str::to_string);
            match next {
                Some(n) => url = n,
                None => break,
            }
        }
        let size = w.max(h);
        let at = |x: u32, y: u32| {
            if x < w && y < h {
                index(cir.get_pixel(x, y).0)
            } else {
                -1.0
            }
        };
        let mut plan = rgb.clone();
        let (mut planted_area, mut paved_area) = (0.0, 0.0);
        let (mut pieces, mut vertices, mut plot_vertices) = (0, 0, 0);
        for plot in &plots {
            let Some((planted, paved)) = split(plot, size, at) else {
                continue;
            };
            pieces += planted.len() + paved.len();
            plot_vertices += plot.iter().map(Vec::len).sum::<usize>();
            vertices += planted
                .iter()
                .chain(&paved)
                .flatten()
                .map(Vec::len)
                .sum::<usize>();
            for (polys, colour, total) in [
                (&planted, [60u8, 200, 60], &mut planted_area),
                (&paved, [150u8, 150, 150], &mut paved_area),
            ] {
                for poly in polys.iter() {
                    fill(poly, size, |x, y| {
                        if x < w && y < h {
                            let p = plan.get_pixel_mut(x, y);
                            for k in 0..3 {
                                p.0[k] = ((u16::from(p.0[k]) + u16::from(colour[k]) * 2) / 3) as u8;
                            }
                            *total += 1.0;
                        }
                    });
                }
            }
            // The plot's outline, in yellow.
            for ring in plot {
                for i in 0..ring.len() {
                    let (a, c) = (ring[i], ring[(i + 1) % ring.len()]);
                    let n = ((c[0] - a[0]).abs().max((c[1] - a[1]).abs()) as usize).max(1);
                    for s in 0..=n {
                        let t = s as f64 / n as f64;
                        let (x, y) = (a[0] + (c[0] - a[0]) * t, a[1] + (c[1] - a[1]) * t);
                        if x >= 0.0 && y >= 0.0 && (x as u32) < w && (y as u32) < h {
                            plan.put_pixel(x as u32, y as u32, image::Rgb([255, 220, 0]));
                        }
                    }
                }
            }
        }
        let side =
            image::imageops::resize(&rgb, w / 2, h / 2, image::imageops::FilterType::Triangle);
        let plan_s =
            image::imageops::resize(&plan, w / 2, h / 2, image::imageops::FilterType::Triangle);
        let mut both = RgbImage::new(w, h / 2);
        image::imageops::replace(&mut both, &side, 0, 0);
        image::imageops::replace(&mut both, &plan_s, i64::from(w / 2), 0);
        both.save(&out).unwrap();
        let m2 = 0.23f64 * 0.23;
        println!(
            "{} plots, {pieces} pieces, {vertices} vertices (plots {plot_vertices}): planted {:.0} m², hard {:.0} m² ({:.0} % planted)",
            plots.len(),
            planted_area * m2,
            paved_area * m2,
            100.0 * planted_area / (planted_area + paved_area).max(1.0)
        );
    }
}
