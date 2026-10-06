//! Roof colour and material, measured from the aerial photograph
//! (`roofs/14/{x}/{y}.json`, `cartopolis_geo::formats::roofs`).
//!
//! The client draws a roof with a material texture tinted to a colour, never
//! with the photograph itself: a 25 cm pixel is a whole roof tile, the photo's
//! shadows fall the way the sun fell on the day it was flown, and a photo roof
//! over an engine-lit wall reads as two games stitched together. What the
//! photograph *can* give reliably is the one thing a generated palette gets
//! wrong — that this roof is red clay, that one anthracite, the one behind it
//! black bitumen. So this measures that, here,
//! once per city, and the client carries two small numbers per building.
//!
//! Per z14 cell that has a LoD2.2 file in the site tree (`lod22` must have run):
//!
//!   1. Fetch the cell's photograph from PDOK's WMS in Web Mercator, as 4×4
//!      blocks of 1600 px (~0.23 m per pixel at Dutch latitudes) — mercator so a
//!      building's pixel is its tile fraction, exactly, with no reprojection.
//!   2. Rasterise every building's roof triangles into a mask, split into flat
//!      and pitched planes by their normal.
//!   3. Move the mask to where the photograph shows the roof. The photograph is
//!      not a true orthophoto: a roof is displaced from its footprint by its
//!      height times the view angle, ~3 m for a 20 m block, which puts an
//!      unmoved mask half on its own facade. The displacement per metre of
//!      height is measured on the tall flat roofs of each block
//!      ([`lean_field`]) and applied to every roof vertex by its own height.
//!   4. Erode the mask a couple of pixels off its rim, backing off for a roof
//!      too small to survive it.
//!   5. Of a pitched roof keep only the planes facing the sun: a north slope is
//!      the same tiles in shade, and a median over both is neither.
//!   6. Take the photograph's colour cast out ([`neutral_gains`]), set aside
//!      vegetation (a crown over the roof, unless most of it is sedum), and take
//!      the trimmed median of the covering, dropping the darkest quarter (cast shadows, chimneys)
//!      and the brightest twentieth (glints).
//!
//! `ATLAS_ROOFS_DEBUG=<dir>` writes each cell's photograph beside the same
//! photograph with every roof painted its measured colour, and again with every
//! roof painted a flat colour per material — the way to judge a run by eye.
//!
//! Imagery: Beeldmateriaal Nederland via PDOK, CC BY 4.0.

use std::collections::BTreeMap;
use std::time::Duration;

use cartopolis_geo::formats::roofs::{Cell as RoofCell, Material, Roof, Surface};
use cartopolis_geo::lod22_cell::{self, Element};
use image::RgbImage;
use serde_json::json;

use super::http::{get_parsed_with, query};
use super::rd::wgs84_bbox;
use super::tile::{cell_path, lat_lng_to_cell, sorted_cells, Cell, CACHE_ZOOM};
use super::{in_order, Count, Ctx, DatasetResult, Outcome, Progress};

const WMS: &str = "https://service.pdok.nl/hwh/luchtfotorgb/wms/v1_0";

/// The 8 cm early-spring mosaic, not the summer 25 cm one the client's photo
/// basemap shows. Measured over Groningen's centre on 2026-09-30, both through
/// this same pipeline: the 25 cm layer is colour-dead in the warm direction
/// (`docs/notes/roof-colours-from-aerial-imagery.md`), so its red clay tiles
/// came out a brownish pink barely apart from the grey concrete ones, while the
/// 8 cm layer separates them plainly. Its lower sun costs longer shadows, which
/// the sunlit-plane selection and the trimmed median already discard; being
/// leaf-off, it also has fewer crowns over roofs.
const LAYER: &str = "Actueel_orthoHR";

/// `ATLAS_ROOFS_LAYER` picks another of the service's layers — a year of the
/// archive, or the summer `Actueel_ortho25` — for comparing what each says about the same
/// roofs. The layer measured is recorded in the coverage manifest.
fn layer() -> String {
    std::env::var("ATLAS_ROOFS_LAYER").unwrap_or_else(|_| LAYER.to_string())
}

const ATTRIBUTION: &str =
    "Luchtfoto and infrared luchtfoto © Beeldmateriaal Nederland (PDOK), CC BY 4.0; \
                           roof geometry © 3DBAG by TU Delft, CC BY 4.0";

/// Blocks per cell edge, i.e. the photograph is fetched as z16 tiles.
const BLOCKS: u32 = 4;
/// Pixels per block edge. PDOK's WMS allows 2500.
const BLOCK_PX: u32 = 1600;
const CELL_PX: u32 = BLOCKS * BLOCK_PX;

/// A plane whose normal is within ~12° of vertical is flat.
const FLAT_COS: f64 = 0.978;

/// Where the sun stands in a typical PDOK spring flight: south, 40° up. Only
/// used to pick which planes of a pitched roof are lit, so it need not be exact.
const SUN: [f64; 3] = [0.0, -0.766_044, 0.642_788];

/// Pixels a surface needs before its measurement means anything (~1.6 m²).
const MIN_PIXELS: usize = 30;

/// Pixels eroded off the rim once the lean is modelled: what is left is the
/// photo's own blur and the ~0.1 m the lean model is off by.
const ERODE_PX: u32 = 2;

/// A building this tall or taller is one the lean can be measured on.
const LEAN_PROBE_M: f64 = 12.0;
/// The steepest lean searched, in metres of displacement per metre of height —
/// ~21° off nadir, past the edge of any flight strip.
const MAX_LEAN: f64 = 0.38;
/// Tall buildings a block needs before its own lean is trusted over the cell's.
const MIN_PROBES: usize = 5;

/// One roof triangle in cell-photo pixels.
#[derive(Clone, Copy, Debug)]
struct Tri {
    /// Where each corner stands on the ground, before any lean.
    v: [[f64; 2]; 3],
    /// Each corner's height above the building's base, in metres.
    h: [f64; 3],
    flat: bool,
    /// Cosine between the plane's normal and [`SUN`].
    sun: f32,
}

impl Tri {
    /// This triangle as the photograph shows it: every corner displaced by its
    /// own height along the local lean, `lean` pixels per metre.
    fn leaned(&self, lean: [f64; 2]) -> Self {
        let mut t = *self;
        for (v, h) in t.v.iter_mut().zip(self.h) {
            v[0] += h * lean[0];
            v[1] += h * lean[1];
        }
        t
    }
}

/// The cell's photograph, a Web Mercator square `CELL_PX` on a side.
fn block_url(cell: Cell, i: u32, j: u32) -> String {
    const HALF: f64 = 20_037_508.342_789_244;
    let z = CACHE_ZOOM + 2;
    let n = f64::from(1u32 << z);
    let tx = (cell.0 * i64::from(BLOCKS) + i64::from(i)) as f64;
    let ty = (cell.1 * i64::from(BLOCKS) + i64::from(j)) as f64;
    let mx = |x: f64| x / n * 2.0 * HALF - HALF;
    let my = |y: f64| HALF - y / n * 2.0 * HALF;
    let bbox = format!("{},{},{},{}", mx(tx), my(ty + 1.0), mx(tx + 1.0), my(ty));
    let px = BLOCK_PX.to_string();
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

fn fetch_block(ctx: &Ctx, url: &str) -> Outcome<RgbImage> {
    get_parsed_with(ctx, url, "photo", Duration::from_secs(120), 4, |b| {
        let img = image::load_from_memory_with_format(b, image::ImageFormat::Jpeg)
            .map_err(|e| {
                format!(
                    "not a JPEG ({e}): {}",
                    String::from_utf8_lossy(&b[..b.len().min(200)])
                )
            })?
            .to_rgb8();
        if img.dimensions() == (BLOCK_PX, BLOCK_PX) {
            Ok(img)
        } else {
            Err(format!("block is {:?}", img.dimensions()))
        }
    })
}

/// A building's roof triangles in the cell photo's pixels, and its height.
fn roof_tris(parts: &[&Element], cell: Cell) -> (Vec<Tri>, f64) {
    let (lat0, lng0) = parts
        .first()
        .and_then(|e| e.p.first())
        .map_or((0.0, 0.0), |p| (p.0, p.1));
    let east = lat0.to_radians().cos() * 111_320.0;
    let pixel = |lat: f64, lng: f64| {
        let (fx, fy) = cartopolis_core::lat_lng_to_tile_frac(lat, lng, CACHE_ZOOM);
        [
            (fx - cell.0 as f64) * f64::from(CELL_PX),
            (fy - cell.1 as f64) * f64::from(CELL_PX),
        ]
    };
    let mut tris = Vec::new();
    let mut top = 0.0f64;
    for e in parts {
        for &(a, b, c, roof) in &e.t {
            if roof == 0 {
                continue;
            }
            let Some(v) = [a, b, c]
                .iter()
                .map(|&i| e.p.get(i as usize).copied())
                .collect::<Option<Vec<_>>>()
            else {
                continue;
            };
            let m: Vec<[f64; 3]> = v
                .iter()
                .map(|p| {
                    [
                        (p.1 - lng0) * east,
                        (p.0 - lat0) * 110_574.0,
                        f64::from(p.2),
                    ]
                })
                .collect();
            let (u, w) = (sub(m[1], m[0]), sub(m[2], m[0]));
            let mut n = [
                u[1] * w[2] - u[2] * w[1],
                u[2] * w[0] - u[0] * w[2],
                u[0] * w[1] - u[1] * w[0],
            ];
            let len = (n[0] * n[0] + n[1] * n[1] + n[2] * n[2]).sqrt();
            if len < 1e-6 {
                continue;
            }
            if n[2] < 0.0 {
                n = [-n[0], -n[1], -n[2]];
            }
            let n = [n[0] / len, n[1] / len, n[2] / len];
            top = top.max(v.iter().map(|p| f64::from(p.2)).fold(0.0, f64::max));
            tris.push(Tri {
                v: [
                    pixel(v[0].0, v[0].1),
                    pixel(v[1].0, v[1].1),
                    pixel(v[2].0, v[2].1),
                ],
                h: [v[0].2, v[1].2, v[2].2].map(f64::from),
                flat: n[2] >= FLAT_COS,
                sun: (n[0] * SUN[0] + n[1] * SUN[1] + n[2] * SUN[2]) as f32,
            });
        }
    }
    (tris, top)
}

fn sub(a: [f64; 3], b: [f64; 3]) -> [f64; 3] {
    [a[0] - b[0], a[1] - b[1], a[2] - b[2]]
}

const NONE: u8 = 0;
const FLAT: u8 = 1;
const PITCHED: u8 = 2;

/// A building's roof rasterised over its own pixel window of the photo.
struct Mask {
    x0: u32,
    y0: u32,
    w: u32,
    label: Vec<u8>,
    sun: Vec<f32>,
    /// Chessboard distance, in pixels, to the nearest pixel that is not roof.
    rim: Vec<u32>,
}

/// Larger than any building the country has — a guard against a corrupt
/// vertex stretching one mask across the cell.
const MAX_MASK_PX: u64 = 16_000_000;

fn rasterise(tris: &[Tri]) -> Option<Mask> {
    let (mut lo, mut hi) = ([f64::INFINITY; 2], [f64::NEG_INFINITY; 2]);
    for t in tris {
        for v in t.v {
            for k in 0..2 {
                lo[k] = lo[k].min(v[k]);
                hi[k] = hi[k].max(v[k]);
            }
        }
    }
    let edge = f64::from(CELL_PX);
    let x0 = lo[0].floor().clamp(0.0, edge) as u32;
    let y0 = lo[1].floor().clamp(0.0, edge) as u32;
    let x1 = hi[0].ceil().clamp(0.0, edge) as u32;
    let y1 = hi[1].ceil().clamp(0.0, edge) as u32;
    if x1 <= x0 || y1 <= y0 || u64::from(x1 - x0) * u64::from(y1 - y0) > MAX_MASK_PX {
        return None;
    }
    let (w, h) = (x1 - x0, y1 - y0);
    let mut label = vec![NONE; (w * h) as usize];
    let mut sun = vec![0.0f32; (w * h) as usize];
    for t in tris {
        let [a, b, c] = t.v;
        let area = (b[0] - a[0]) * (c[1] - a[1]) - (b[1] - a[1]) * (c[0] - a[0]);
        if area.abs() < 1e-9 {
            continue;
        }
        let tx0 = a[0].min(b[0]).min(c[0]).floor().max(f64::from(x0)) as u32;
        let tx1 = a[0].max(b[0]).max(c[0]).ceil().min(f64::from(x1)) as u32;
        let ty0 = a[1].min(b[1]).min(c[1]).floor().max(f64::from(y0)) as u32;
        let ty1 = a[1].max(b[1]).max(c[1]).ceil().min(f64::from(y1)) as u32;
        for y in ty0..ty1 {
            for x in tx0..tx1 {
                let p = [f64::from(x) + 0.5, f64::from(y) + 0.5];
                let e = |s: [f64; 2], d: [f64; 2]| {
                    ((d[0] - s[0]) * (p[1] - s[1]) - (d[1] - s[1]) * (p[0] - s[0])) * area.signum()
                };
                if e(a, b) >= 0.0 && e(b, c) >= 0.0 && e(c, a) >= 0.0 {
                    let i = ((y - y0) * w + (x - x0)) as usize;
                    label[i] = if t.flat { FLAT } else { PITCHED };
                    sun[i] = t.sun;
                }
            }
        }
    }
    let rim = chessboard(&label, w, h);
    Some(Mask {
        x0,
        y0,
        w,
        label,
        sun,
        rim,
    })
}

/// Two-pass chessboard distance to the nearest `NONE` pixel; the window's own
/// border counts as outside, since the window is the roof's bounding box.
fn chessboard(label: &[u8], w: u32, h: u32) -> Vec<u32> {
    let (w, h) = (w as usize, h as usize);
    let mut d: Vec<u32> = label
        .iter()
        .map(|&l| if l == NONE { 0 } else { u32::MAX / 2 })
        .collect();
    let at = |d: &Vec<u32>, x: isize, y: isize| -> u32 {
        if x < 0 || y < 0 || x >= w as isize || y >= h as isize {
            0
        } else {
            d[y as usize * w + x as usize]
        }
    };
    for y in 0..h as isize {
        for x in 0..w as isize {
            let i = y as usize * w + x as usize;
            if d[i] == 0 {
                continue;
            }
            let m = at(&d, x - 1, y)
                .min(at(&d, x - 1, y - 1))
                .min(at(&d, x, y - 1))
                .min(at(&d, x + 1, y - 1));
            d[i] = d[i].min(m + 1);
        }
    }
    for y in (0..h as isize).rev() {
        for x in (0..w as isize).rev() {
            let i = y as usize * w + x as usize;
            if d[i] == 0 {
                continue;
            }
            let m = at(&d, x + 1, y)
                .min(at(&d, x + 1, y + 1))
                .min(at(&d, x, y + 1))
                .min(at(&d, x - 1, y + 1));
            d[i] = d[i].min(m + 1);
        }
    }
    d
}

impl Mask {
    /// The photo pixels of one kind of plane, eroded as far as the roof allows.
    fn pixels(&self, photo: &RgbImage, kind: u8, erode: u32) -> Vec<[u8; 3]> {
        let mut r = erode;
        loop {
            let keep: Vec<usize> = (0..self.label.len())
                .filter(|&i| self.label[i] == kind && self.rim[i] > r)
                .collect();
            if keep.len() >= MIN_PIXELS || r == 0 {
                // Of a pitched roof, only the planes the sun lit.
                let best = keep
                    .iter()
                    .map(|&i| self.sun[i])
                    .fold(f32::NEG_INFINITY, f32::max);
                return keep
                    .into_iter()
                    .filter(|&i| kind != PITCHED || self.sun[i] >= best - 0.1)
                    .map(|i| {
                        let (x, y) = (i as u32 % self.w, i as u32 / self.w);
                        photo.get_pixel(self.x0 + x, self.y0 + y).0
                    })
                    .collect();
            }
            r /= 2;
        }
    }

    fn paint(&self, out: &mut RgbImage, roof: &Roof, colour: impl Fn(&Surface) -> [u8; 3]) {
        for (i, &l) in self.label.iter().enumerate() {
            let s = match l {
                FLAT => roof.f.as_ref(),
                PITCHED => roof.p.as_ref(),
                _ => None,
            };
            if let Some(s) = s {
                let (x, y) = (i as u32 % self.w, i as u32 / self.w);
                out.put_pixel(self.x0 + x, self.y0 + y, image::Rgb(colour(s)));
            }
        }
    }
}

/// A flat colour per material, for the debug image's third panel.
fn material_key(m: Material) -> [u8; 3] {
    match m {
        Material::Tile => [230, 110, 40],
        Material::Bitumen => [20, 20, 20],
        Material::Gravel => [140, 140, 140],
        Material::Light => [250, 250, 250],
        Material::Green => [40, 200, 60],
    }
}

fn lum(p: [u8; 3]) -> f32 {
    0.2126 * f32::from(p[0]) + 0.7152 * f32::from(p[1]) + 0.0722 * f32::from(p[2])
}

fn is_vegetation(p: [u8; 3]) -> bool {
    let [r, g, b] = p.map(i32::from);
    g > r + 8 && g > b + 8
}

fn median(mut v: Vec<[u8; 3]>) -> [u8; 3] {
    let mid = v.len() / 2;
    let mut out = [0u8; 3];
    for (k, o) in out.iter_mut().enumerate() {
        v.select_nth_unstable_by_key(mid, |p| p[k]);
        *o = v[mid][k];
    }
    out
}

/// What a set of roof pixels says the roof is.
/// What a set of roof pixels says the roof is.
///
/// Solar panels are deliberately not a material. At the 0.23 m this samples
/// the panel grid is below the pixel, and a panel field, a slate roof and a
/// bitumen roof reflecting the sky are the same dark blue-grey: over
/// Groningen's centre a colour test flagged 7,211 roof surfaces, and a
/// grid-contrast test did not separate the slate from the panels
/// (`docs/notes/roof-colours-from-aerial-imagery.md`). A dark roof is given
/// its measured colour, which is right whichever it is.
fn classify(pixels: &[[u8; 3]], flat: bool) -> Option<Surface> {
    let n = pixels.len();
    if n < MIN_PIXELS {
        return None;
    }
    let veg: Vec<[u8; 3]> = pixels
        .iter()
        .copied()
        .filter(|&p| is_vegetation(p))
        .collect();
    let surface = |c, m| Surface { c, m, n: n as u32 };
    if veg.len() * 2 > n {
        return Some(surface(median(veg), Material::Green));
    }
    let mut rest: Vec<[u8; 3]> = pixels
        .iter()
        .copied()
        .filter(|&p| !is_vegetation(p))
        .collect();
    if rest.len() < MIN_PIXELS / 2 {
        rest = pixels.to_vec();
    }
    rest.sort_by(|a, b| lum(*a).total_cmp(&lum(*b)));
    let lo = rest.len() / 4;
    let hi = rest.len() - rest.len() / 20;
    let c = median(rest[lo..hi.max(lo + 1)].to_vec());
    let m = if !flat {
        Material::Tile
    } else if lum(c) < 75.0 {
        Material::Bitumen
    } else if lum(c) < 150.0 {
        Material::Gravel
    } else {
        Material::Light
    };
    Some(surface(c, m))
}

/// Where a building's roof sits in the photo, as a pixel.
fn centre(tris: &[Tri]) -> [f64; 2] {
    let n = (tris.len() * 3).max(1) as f64;
    let s = tris
        .iter()
        .flat_map(|t| t.v)
        .fold([0.0, 0.0], |a, v| [a[0] + v[0], a[1] + v[1]]);
    [s[0] / n, s[1] / n]
}

/// How far one tall flat-roofed building's roof lies from its footprint in the
/// photograph, in pixels per metre of height — or `None` when it cannot say.
///
/// PDOK's mosaic is ordinary orthophotos, not true ones: a roof is drawn where
/// the camera saw it, displaced from its footprint away from the photo's nadir
/// by its height times the tangent of the view angle. Over Groningen's centre
/// a 20 m block is off by ~3 m, which puts the mask half on its own facade and
/// half on the street. The offset is found as the shift at which the roof mask
/// covers the most uniform patch of photo: a flat roof is one surface, and a
/// mask straddling a roof edge is two.
fn probe_lean(photo: &RgbImage, tris: &[Tri], top: f64, px_m: f64) -> Option<[f64; 2]> {
    let mask = rasterise(tris)?;
    let inside: Vec<usize> = (0..mask.label.len())
        .filter(|&i| mask.label[i] != NONE && mask.rim[i] > 1)
        .collect();
    let flat = inside.iter().filter(|&&i| mask.label[i] == FLAT).count();
    if inside.len() < 400 || flat * 5 < inside.len() * 4 {
        return None;
    }
    let stride = inside.len().div_ceil(1500);
    let pts: Vec<(i64, i64)> = inside
        .iter()
        .step_by(stride)
        .map(|&i| {
            let (x, y) = (i as u32 % mask.w, i as u32 / mask.w);
            (i64::from(mask.x0 + x), i64::from(mask.y0 + y))
        })
        .collect();
    let edge = i64::from(CELL_PX);
    let score = |dx: i64, dy: i64| -> f64 {
        let (mut s, mut s2) = (0.0f64, 0.0f64);
        for &(x, y) in &pts {
            let (x, y) = (x + dx, y + dy);
            if x < 0 || y < 0 || x >= edge || y >= edge {
                return f64::INFINITY;
            }
            let l = f64::from(lum(photo.get_pixel(x as u32, y as u32).0));
            s += l;
            s2 += l * l;
        }
        let n = pts.len() as f64;
        s2 / n - (s / n).powi(2)
    };
    let r = (top * MAX_LEAN / px_m).ceil() as i64;
    let mut best = (f64::INFINITY, 0, 0);
    for dy in (-r..=r).step_by(2) {
        for dx in (-r..=r).step_by(2) {
            let s = score(dx, dy);
            if s < best.0 {
                best = (s, dx, dy);
            }
        }
    }
    let (_, cx, cy) = best;
    for dy in cy - 1..=cy + 1 {
        for dx in cx - 1..=cx + 1 {
            let s = score(dx, dy);
            if s < best.0 {
                best = (s, dx, dy);
            }
        }
    }
    // A best fit on the edge of the search is the search running out, not a
    // lean: it found some uniform patch of street or shadow to sit on.
    let (_, dx, dy) = best;
    (best.0.is_finite() && dx.abs() < r - 1 && dy.abs() < r - 1)
        .then(|| [dx as f64 / top, dy as f64 / top])
}

/// The lean over each block of the cell, from the tall buildings standing in
/// it. One photograph covers a block or more, so its lean is one direction and
/// one rate there; where a block has too few tall buildings the cell's median
/// stands in, and where the cell has none there is no lean to correct.
fn lean_field(
    photo: &RgbImage,
    buildings: &[(&str, Vec<Tri>, f64)],
    px_m: f64,
) -> ([[f64; 2]; (BLOCKS * BLOCKS) as usize], usize) {
    let block_of = |c: [f64; 2]| {
        let b = |v: f64| (v / f64::from(BLOCK_PX)).clamp(0.0, f64::from(BLOCKS - 1)) as usize;
        b(c[1]) * BLOCKS as usize + b(c[0])
    };
    let mut probes: Vec<Vec<[f64; 2]>> = vec![Vec::new(); (BLOCKS * BLOCKS) as usize];
    for (_, tris, top) in buildings {
        if *top >= LEAN_PROBE_M {
            if let Some(d) = probe_lean(photo, tris, *top, px_m) {
                probes[block_of(centre(tris))].push(d);
            }
        }
    }
    let med = |v: &[[f64; 2]]| -> [f64; 2] {
        let m = |k: usize| {
            let mut c: Vec<f64> = v.iter().map(|d| d[k]).collect();
            c.sort_by(f64::total_cmp);
            c[c.len() / 2]
        };
        [m(0), m(1)]
    };
    let all: Vec<[f64; 2]> = probes.iter().flatten().copied().collect();
    let cell = if all.is_empty() { [0.0; 2] } else { med(&all) };
    let mut field = [cell; (BLOCKS * BLOCKS) as usize];
    for (f, p) in field.iter_mut().zip(&probes) {
        if p.len() >= MIN_PROBES {
            *f = med(p);
        }
    }
    (field, all.len())
}

/// The share of a photo's darkest pixels taken as its haze floor.
const VEIL_PERCENTILE: f64 = 0.005;

fn srgb_to_linear(c: f64) -> f64 {
    if c <= 0.04045 {
        c / 12.92
    } else {
        ((c + 0.055) / 1.055).powf(2.4)
    }
}

fn linear_to_srgb(c: f64) -> f64 {
    if c <= 0.003_130_8 {
        c * 12.92
    } else {
        1.055 * c.powf(1.0 / 2.4) - 0.055
    }
}

/// Take the haze out of the photograph, in place, and say how much there was.
///
/// Shot from a few kilometres up, the photo carries a veil of scattered light
/// on every pixel: over Groningen the darkest half-percent of orthoHR — alley
/// floors and courtyard shadows, which are near black on the ground — measured
/// (33, 45, 60), bluish, rather than black. That veil is added, not multiplied,
/// so it greys and lightens a dark roof far more than a pale one, and no white
/// balance undoes it. Dark-object subtraction, the standard correction for
/// aerial and satellite imagery, removes it: the floor is taken per channel in
/// linear light and subtracted, and the range rescaled so white stays white.
fn dehaze(photo: &mut RgbImage) -> [u8; 3] {
    let mut veil = [0u8; 3];
    for (k, v) in veil.iter_mut().enumerate() {
        let mut hist = [0u64; 256];
        let mut n = 0u64;
        for y in (0..photo.height()).step_by(4) {
            for x in (0..photo.width()).step_by(4) {
                hist[usize::from(photo.get_pixel(x, y).0[k])] += 1;
                n += 1;
            }
        }
        let floor = (n as f64 * VEIL_PERCENTILE) as u64;
        let mut seen = 0;
        *v = hist
            .iter()
            .position(|&h| {
                seen += h;
                seen > floor
            })
            .unwrap_or(0) as u8;
    }
    let lut: [[u8; 256]; 3] = veil.map(|v| {
        let v = srgb_to_linear(f64::from(v) / 255.0);
        std::array::from_fn(|c| {
            let l = srgb_to_linear(c as f64 / 255.0);
            let out = ((l - v) / (1.0 - v)).clamp(0.0, 1.0);
            (linear_to_srgb(out) * 255.0).round() as u8
        })
    });
    for p in photo.pixels_mut() {
        p.0 = [
            lut[0][usize::from(p.0[0])],
            lut[1][usize::from(p.0[1])],
            lut[2][usize::from(p.0[2])],
        ];
    }
    veil
}

/// Per-channel gains that make the photograph's neutral grey neutral.
///
/// PDOK's mosaic is not colour-neutral: over Groningen a grey roof measures
/// ~(118, 125, 116) on the 25 cm layer, green-cyan, which on its own classified
/// a fifth of the city's flat roofs as sedum. Comparing a roof against a
/// threshold is only meaningful once the photograph's own cast is gone, and
/// the reference has to come from the same photograph
/// (`docs/notes/roof-colours-from-aerial-imagery.md`). Flat roofs are the
/// reference: gravel, bitumen and membrane are grey, they are most of the flat
/// roofs in any Dutch cell, and a median ignores the sedum and panels among them.
fn neutral_gains(flat: &[&[[u8; 3]]]) -> [f32; 3] {
    let mut all: Vec<[u8; 3]> = flat.iter().flat_map(|v| v.iter().copied()).collect();
    if all.len() < 10 * MIN_PIXELS {
        return [1.0; 3];
    }
    let m = median(std::mem::take(&mut all)).map(|c| f32::from(c).max(1.0));
    let grey = (m[0] + m[1] + m[2]) / 3.0;
    m.map(|c| (grey / c).clamp(0.7, 1.4))
}

fn balance(p: [u8; 3], gains: [f32; 3]) -> [u8; 3] {
    [0, 1, 2].map(|k| (f32::from(p[k]) * gains[k]).round().clamp(0.0, 255.0) as u8)
}

/// The cells of the run's bbox that `lod22` has written.
fn lod22_cells(ctx: &Ctx) -> Vec<(Cell, lod22_cell::Cell)> {
    let [min_lng, min_lat, max_lng, max_lat] = wgs84_bbox(ctx.bbox);
    let (x0, y0) = lat_lng_to_cell(max_lat, min_lng);
    let (x1, y1) = lat_lng_to_cell(min_lat, max_lng);
    let mut out = Vec::new();
    for x in x0..=x1 {
        for y in y0..=y1 {
            let Some(bytes) = ctx.read_site(&format!("lod22/{CACHE_ZOOM}/{x}/{y}.bin")) else {
                continue;
            };
            match lod22_cell::decode(&bytes) {
                Ok(c) if !c.elements.is_empty() => out.push(((x, y), c)),
                Ok(_) => {}
                Err(e) => ctx.log(format!("lod22 cell {x}/{y}: {e}")),
            }
        }
    }
    out
}

pub fn run(ctx: &Ctx) -> Outcome<DatasetResult> {
    let cells = lod22_cells(ctx);
    if cells.is_empty() {
        return Err(
            "no LoD2.2 cells in the site tree for this area — run lod22 first"
                .to_string()
                .into(),
        );
    }
    ctx.log(format!("{} cells with LoD2.2 roofs", cells.len()));
    let debug = std::env::var_os("ATLAS_ROOFS_DEBUG").map(std::path::PathBuf::from);
    let blocks: Vec<(u32, u32)> = (0..BLOCKS)
        .flat_map(|j| (0..BLOCKS).map(move |i| (i, j)))
        .collect();
    let (mut measured, mut pitched, mut flat, mut written) = (0usize, 0usize, 0usize, Vec::new());
    let mut gardens = 0usize;

    for (done, (cell, lod)) in cells.iter().enumerate() {
        ctx.progress(Progress {
            phase: Some("cells".into()),
            done: Some(done as u64),
            total: Some(Some(cells.len() as u64)),
        });
        let mut photo = RgbImage::new(CELL_PX, CELL_PX);
        in_order(
            ctx,
            &blocks,
            ctx.upstream(),
            |&(i, j)| fetch_block(ctx, &block_url(*cell, i, j)),
            |&(i, j), block| {
                image::imageops::replace(
                    &mut photo,
                    &block?,
                    i64::from(i * BLOCK_PX),
                    i64::from(j * BLOCK_PX),
                );
                Ok(())
            },
        )?;

        let veil = dehaze(&mut photo);
        let (lat, _) = cartopolis_core::tile_to_lat_lng(cell.0, cell.1, CACHE_ZOOM);
        let px_m = cartopolis_core::tile_to_meters(CACHE_ZOOM, lat) / f64::from(CELL_PX);
        let mut by_pand: BTreeMap<&str, Vec<&Element>> = BTreeMap::new();
        for e in &lod.elements {
            by_pand.entry(e.id.as_str()).or_default().push(e);
        }
        let buildings: Vec<(&str, Vec<Tri>, f64)> = by_pand
            .iter()
            .map(|(pand, parts)| {
                let (tris, top) = roof_tris(parts, *cell);
                (*pand, tris, top)
            })
            .collect();
        let (lean, probes) = lean_field(&photo, &buildings, px_m);
        ctx.log(format!(
            "cell {}/{}: lean from {probes} tall buildings, {:.2?} px/m per block",
            cell.0,
            cell.1,
            lean.map(|d| [
                (d[0] * 100.0).round() / 100.0,
                (d[1] * 100.0).round() / 100.0
            ])
        ));
        let sampled: Vec<(&str, Mask, [Vec<[u8; 3]>; 2])> = buildings
            .iter()
            .filter_map(|(pand, tris, _)| {
                let c = centre(tris);
                let b =
                    |v: f64| (v / f64::from(BLOCK_PX)).clamp(0.0, f64::from(BLOCKS - 1)) as usize;
                let d = lean[b(c[1]) * BLOCKS as usize + b(c[0])];
                let leaned: Vec<Tri> = tris.iter().map(|t| t.leaned(d)).collect();
                let mask = rasterise(&leaned)?;
                let px = [
                    mask.pixels(&photo, FLAT, ERODE_PX),
                    mask.pixels(&photo, PITCHED, ERODE_PX),
                ];
                Some((*pand, mask, px))
            })
            .collect();
        let gains = neutral_gains(
            &sampled
                .iter()
                .map(|s| s.2[0].as_slice())
                .collect::<Vec<_>>(),
        );
        // The photo with its cast taken out, then the same with every roof
        // painted its measured colour, then painted by material.
        let mut panels = debug.as_ref().map(|_| {
            let mut p = photo.clone();
            p.pixels_mut().for_each(|px| px.0 = balance(px.0, gains));
            [p.clone(), p.clone(), p]
        });
        let mut doc = RoofCell::default();
        for (pand, mask, [f, p]) in &sampled {
            let fix = |v: &[[u8; 3]]| v.iter().map(|&p| balance(p, gains)).collect::<Vec<_>>();
            let roof = Roof {
                f: classify(&fix(f), true),
                p: classify(&fix(p), false),
            };
            if roof.f.is_none() && roof.p.is_none() {
                continue;
            }
            pitched += usize::from(roof.p.is_some());
            flat += usize::from(roof.f.is_some());
            if let Some([_, colour, material]) = panels.as_mut() {
                mask.paint(colour, &roof, |s| s.c);
                mask.paint(material, &roof, |s| material_key(s.m));
            }
            doc.roofs.insert((*pand).to_string(), roof);
        }
        measured += doc.roofs.len();
        // The paving, where `bgt` has written this cell's surfaces: every roof
        // as the photo shows it is masked out first, so a street is measured
        // where the camera saw the street.
        let mut roofed = vec![false; (CELL_PX * CELL_PX) as usize];
        for (_, mask, _) in &sampled {
            for (i, &l) in mask.label.iter().enumerate() {
                if l != NONE {
                    let (x, y) = (mask.x0 + i as u32 % mask.w, mask.y0 + i as u32 / mask.w);
                    roofed[(y * CELL_PX + x) as usize] = true;
                }
            }
        }
        let paving_total = match ctx
            .read_site(&format!(
                "bgt/{CACHE_ZOOM}/{}/{}.surfaces.json",
                cell.0, cell.1
            ))
            .and_then(|b| String::from_utf8(b).ok())
            .and_then(|t| cartopolis_geo::formats::bgt::parse_paving(&t).ok())
        {
            Some(paving) => {
                doc.paving = measure_paving(&photo, &roofed, gains, &paving.surfaces, *cell);
                doc.gardens = measure_gardens(ctx, *cell, &blocks, &paving.surfaces)?;
                gardens += doc.gardens.len();
                paving.surfaces.len()
            }
            None => 0,
        };
        ctx.log(format!(
            "cell {}/{}: {} of {} buildings measured, {} of {paving_total} paved surfaces, \
             haze {veil:?}, colour gains {gains:.3?}",
            cell.0,
            cell.1,
            doc.roofs.len(),
            by_pand.len(),
            doc.paving.len(),
        ));
        if !doc.gardens.is_empty() {
            ctx.log(format!(
                "cell {}/{}: {} gardens split",
                cell.0,
                cell.1,
                doc.gardens.len()
            ));
        }
        if let (Some(dir), Some(panels)) = (&debug, panels) {
            write_debug(ctx, dir, *cell, &panels);
        }
        if !doc.roofs.is_empty() || !doc.paving.is_empty() || !doc.gardens.is_empty() {
            ctx.write_doc(&cell_path("roofs", *cell), &doc)?;
            written.push(*cell);
        }
    }

    ctx.write_json(
        "roofs/coverage.json",
        &json!({
            "name": ctx.name,
            "zoom": CACHE_ZOOM,
            "attribution": ATTRIBUTION,
            "source": format!("PDOK luchtfotorgb {}", layer()),
            "bbox_rd": ctx.bbox,
            "cells": sorted_cells(written.iter().copied()),
            "building_count": measured,
        }),
    )?;
    ctx.log(format!(
        "done: {measured} roofs ({pitched} pitched, {flat} flat) across {} cells",
        written.len()
    ));
    Ok(DatasetResult {
        cells: written.len(),
        counts: vec![
            Count::new("roofs", measured),
            Count::new("pitched", pitched),
            Count::new("flat", flat),
            Count::new("gardens", gardens),
        ],
    })
}

// ─── Paving ──────────────────────────────────────────────────────────────────

/// Pixels a paved surface must show the camera — ~7 m² — and the share of it
/// that must be open to the sky, before its colour is believed.
const MIN_PAVING_PIXELS: usize = 130;
const MIN_PAVING_VISIBLE: f64 = 0.25;

/// Surface kinds and materials not worth measuring: verges, lawns and beds are
/// drawn green whatever the photo says, and a garden is lawn and paving
/// together — `gardens` cuts it in two instead.
const UNMEASURED_KINDS: [usize; 2] = [4, GARDEN];
/// `SURFACE_KINDS` position of a garden plot (`bgt.rs`).
const GARDEN: usize = 12;
const UNMEASURED_MATERIALS: [usize; 2] = [7, 8];

/// Every pixel inside a polygon (outer ring and holes, even-odd), by scanline.
fn fill_rings(rings: &[Vec<[f64; 2]>], mut visit: impl FnMut(u32, u32)) {
    let edge = f64::from(CELL_PX);
    let (lo, hi) = rings
        .iter()
        .flatten()
        .fold((f64::INFINITY, f64::NEG_INFINITY), |(a, b), p| {
            (a.min(p[1]), b.max(p[1]))
        });
    let y0 = lo.floor().clamp(0.0, edge) as u32;
    let y1 = hi.ceil().clamp(0.0, edge) as u32;
    let mut xs: Vec<f64> = Vec::new();
    for y in y0..y1 {
        let cy = f64::from(y) + 0.5;
        xs.clear();
        for ring in rings {
            for i in 0..ring.len() {
                let (a, b) = (ring[i], ring[(i + 1) % ring.len()]);
                if (a[1] > cy) != (b[1] > cy) {
                    xs.push(a[0] + (cy - a[1]) / (b[1] - a[1]) * (b[0] - a[0]));
                }
            }
        }
        xs.sort_by(f64::total_cmp);
        for pair in xs.chunks_exact(2) {
            let x0 = (pair[0] - 0.5).ceil().clamp(0.0, edge) as u32;
            let x1 = (pair[1] - 0.5).floor().clamp(-1.0, edge - 1.0);
            if x1 < f64::from(x0) {
                continue;
            }
            for x in x0..=x1 as u32 {
                visit(x, y);
            }
        }
    }
}

/// What colour each paved surface of a cell is, from the photo.
///
/// Measured where it is open to the sky: under no roof (with a pixel's margin,
/// for the eaves) and no tree crown. Of what is left, the colour is the median
/// of its sunlit pixels with the top tenth (markings, cars, glare) dropped; a
/// surface with too little sun on it — an alley, a street along a tall north
/// face — is measured in its shade and brought back into the sun by the
/// photo's own shade model ([`shade_model`]). A surface that showed the camera
/// too little of itself is not measured at all, and keeps its material's tone.
///
/// This is the question `aerial-imagery-cannot-give-paving-colour` answered no
/// to on 2026-09-10, and the answer there still stands for what it asked:
/// telling one red brick from another between districts. What this asks is
/// coarser — yellow brick or red, grey pavers or pale tiles — which the 2026
/// 5 cm photo shows at a glance on the Vismarkt.
fn measure_paving(
    photo: &RgbImage,
    roofed: &[bool],
    gains: [f32; 3],
    surfaces: &[cartopolis_geo::formats::bgt::Surface],
    cell: Cell,
) -> BTreeMap<String, cartopolis_geo::formats::roofs::PavingColour> {
    let pixel = |lat: f64, lng: f64| {
        let (fx, fy) = cartopolis_core::lat_lng_to_tile_frac(lat, lng, CACHE_ZOOM);
        [
            (fx - cell.0 as f64) * f64::from(CELL_PX),
            (fy - cell.1 as f64) * f64::from(CELL_PX),
        ]
    };
    let near_roof = |x: u32, y: u32| {
        let at = |x: i64, y: i64| {
            x >= 0
                && y >= 0
                && x < i64::from(CELL_PX)
                && y < i64::from(CELL_PX)
                && roofed[(y as u32 * CELL_PX + x as u32) as usize]
        };
        let (x, y) = (i64::from(x), i64::from(y));
        (-1..=1).any(|dy| (-1..=1).any(|dx| at(x + dx, y + dy)))
    };
    // First every surface's visible pixels…
    let mut seen: Vec<(String, Option<usize>, Vec<[u8; 3]>)> = Vec::new();
    for s in surfaces {
        if UNMEASURED_KINDS.contains(&s.0) || s.2.is_some_and(|m| UNMEASURED_MATERIALS.contains(&m))
        {
            continue;
        }
        let Some(&first) = s.1.first().and_then(|r| r.first()) else {
            continue;
        };
        let rings: Vec<Vec<[f64; 2]>> =
            s.1.iter()
                .map(|r| r.iter().map(|&(lat, lng)| pixel(lat, lng)).collect())
                .collect();
        let (mut total, mut kept) = (0usize, Vec::new());
        fill_rings(&rings, |x, y| {
            total += 1;
            if near_roof(x, y) {
                return;
            }
            let p = photo.get_pixel(x, y).0;
            if !is_vegetation(p) {
                kept.push(balance(p, gains));
            }
        });
        if kept.len() < MIN_PAVING_PIXELS || (kept.len() as f64) < MIN_PAVING_VISIBLE * total as f64
        {
            continue;
        }
        seen.push((
            cartopolis_geo::formats::roofs::paving_key(first.0, first.1),
            s.2,
            kept,
        ));
    }
    // …then the light. A street between buildings lies partly or wholly in
    // their shadow, and an alley wholly: measured as it stands, a passage came
    // out nearly black and drew as a hole in the ground (Deck, 2026-10-01).
    // The cell's paving pixels split into sunlit and shaded at the darkest
    // point between the two humps of their luminance, and the ratio of the two
    // populations' medians, per channel, is what shade does to paving in this
    // photograph — darker, and bluer, being lit by the sky alone.
    let all: Vec<[u8; 3]> = seen
        .iter()
        .flat_map(|(_, _, v)| v.iter().copied())
        .collect();
    let (threshold, shade) = shade_model(&all);
    let mut out = BTreeMap::new();
    for (key, material, kept) in seen {
        let (sunlit, shaded): (Vec<[u8; 3]>, Vec<[u8; 3]>) =
            kept.iter().partition(|&&p| lum(p) >= threshold);
        let c = if sunlit.len() >= (kept.len() / 4).max(MIN_PAVING_PIXELS / 3) {
            trimmed_median(sunlit, 0.0, 0.9)
        } else {
            out_of_shade(trimmed_median(shaded, 0.1, 0.9), shade)
        };
        let c = if material == Some(BRICK) {
            brick_colour(c)
        } else {
            c
        };
        out.insert(
            key,
            cartopolis_geo::formats::roofs::PavingColour {
                c,
                n: kept.len() as u32,
            },
        );
    }
    out
}

/// `SURFACE_MATERIALS`' index for fired clay brick.
const BRICK: usize = 2;

/// A brick surface's colour, from what the photo measured of it.
///
/// The photo keeps the *direction* of a brick's colour and loses most of its
/// size. Measured over Groningen's centre (2026-10-01): the Grote Markt, which
/// is red brick to anyone standing on it, came out (161, 151, 144) — red minus
/// green of 10 where the cell's 644 brick surfaces have a median of 3 and a
/// 90th percentile of 11, and every non-brick material sits near 0. Scaling
/// the measured chroma up to a believable red would scale the noise in every
/// grey up with it, so the measurement chooses the *kind* of brick instead and
/// the kind supplies the colour, at the measured lightness: red where it is
/// clearly redder than neutral, yellow where it leans green-yellow instead
/// (the Herestraat), anthracite where it is dark, buff otherwise.
fn brick_colour(c: [u8; 3]) -> [u8; 3] {
    let [r, g, b] = c.map(i32::from);
    // A weathered red clay paver, not a new facing brick: the first value here,
    // (150, 82, 66) lifted by the photo's lightness, put the Grote Markt at
    // (188, 103, 83), which read on the phone as painted red (2026-10-01).
    // Red is red-over-green *more than* green-over-blue. Warm yellow brick is
    // warm too — over the Vismarkt eleven yellow surfaces measured red minus
    // green of 7–14 and were drawn red — but what makes it yellow is that its
    // green stands well clear of its blue: (198, 191, 172) is 7 and 19, where
    // the Grote Markt's red is 10 and 7.
    // …and weathered further, to the photograph: (128, 76, 64) still drew the
    // square as a red carpet beside PDOK's own picture of it, which is a taupe
    // with a red lean (about (130, 135, 120) under the photo's cool cast,
    // 2026-10-01). Half the chroma keeps it a red brick and puts it with the
    // photo rather than against it.
    let base: [f64; 3] = if r - g >= 7 && r - g > g - b {
        [132.0, 102.0, 90.0]
    } else if g - b >= 9 && r - g >= -2 {
        [196.0, 170.0, 122.0]
    } else if lum(c) < 95.0 {
        [92.0, 86.0, 84.0]
    } else {
        [176.0, 150.0, 126.0]
    };
    // At the measured lightness, within reason: a brick street in a dull photo
    // is not a different brick. Darkened by a shaded photo, never brightened
    // past the brick's own colour — a bright open square is sunlit, which the
    // engine already lights, and lifting the colour as well lit it twice.
    let target = f64::from(lum(c));
    let own = 0.2126 * base[0] + 0.7152 * base[1] + 0.0722 * base[2];
    let k = (target / own).clamp(0.8, 1.0);
    base.map(|v| (v * k).round().clamp(0.0, 255.0) as u8)
}

/// The median colour of the pixels between two luminance quantiles.
fn trimmed_median(mut px: Vec<[u8; 3]>, lo: f64, hi: f64) -> [u8; 3] {
    px.sort_by(|a, b| lum(*a).total_cmp(&lum(*b)));
    let n = px.len();
    let (a, b) = ((n as f64 * lo) as usize, (n as f64 * hi) as usize);
    median(px[a.min(n.saturating_sub(1))..b.max(a + 1).min(n)].to_vec())
}

/// The luminance that splits sunlit from shaded paving (Otsu's threshold over
/// the cell's paving pixels), and the per-channel gain, in linear light, that
/// takes a shaded colour to its sunlit one.
fn shade_model(px: &[[u8; 3]]) -> (f32, [f64; 3]) {
    let mut hist = [0u64; 256];
    for &p in px {
        hist[lum(p).round().clamp(0.0, 255.0) as usize] += 1;
    }
    let total: u64 = hist.iter().sum();
    if total == 0 {
        return (0.0, [1.0; 3]);
    }
    let sum: f64 = hist
        .iter()
        .enumerate()
        .map(|(i, &h)| i as f64 * h as f64)
        .sum();
    let (mut w0, mut s0, mut best, mut at) = (0.0f64, 0.0f64, 0.0f64, 0usize);
    for (i, &h) in hist.iter().enumerate() {
        w0 += h as f64;
        s0 += i as f64 * h as f64;
        let w1 = total as f64 - w0;
        if w0 == 0.0 || w1 == 0.0 {
            continue;
        }
        let (m0, m1) = (s0 / w0, (sum - s0) / w1);
        let between = w0 * w1 * (m0 - m1).powi(2);
        if between > best {
            best = between;
            at = i;
        }
    }
    let t = at as f32 + 0.5;
    let (sun, shade): (Vec<[u8; 3]>, Vec<[u8; 3]>) = px.iter().partition(|&&p| lum(p) >= t);
    if sun.is_empty() || shade.is_empty() {
        return (t, [1.0; 3]);
    }
    let (ms, mh) = (median(sun), median(shade));
    let gain = [0, 1, 2].map(|k| {
        let a = srgb_to_linear(f64::from(ms[k]) / 255.0);
        let b = srgb_to_linear(f64::from(mh[k]) / 255.0).max(1e-4);
        (a / b).clamp(1.0, 8.0)
    });
    (t, gain)
}

/// A colour measured in shade, brought into the sun by `gain` (linear light,
/// per channel).
fn out_of_shade(c: [u8; 3], gain: [f64; 3]) -> [u8; 3] {
    [0, 1, 2].map(|k| {
        let l = srgb_to_linear(f64::from(c[k]) / 255.0) * gain[k];
        (linear_to_srgb(l.min(1.0)) * 255.0).round() as u8
    })
}

/// The debug panels side by side, at half size.
fn write_debug(ctx: &Ctx, dir: &std::path::Path, cell: Cell, panels: &[RgbImage]) {
    let half = CELL_PX / 2;
    let mut out = RgbImage::new(half * panels.len() as u32, half);
    for (k, p) in panels.iter().enumerate() {
        let small = image::imageops::resize(p, half, half, image::imageops::FilterType::Triangle);
        image::imageops::replace(&mut out, &small, i64::from(half) * k as i64, 0);
    }
    let path = dir.join(format!("roofs_{}_{}.jpg", cell.0, cell.1));
    if let Err(e) = std::fs::create_dir_all(dir)
        .map_err(|e| e.to_string())
        .and_then(|()| {
            out.save_with_format(&path, image::ImageFormat::Jpeg)
                .map_err(|e| e.to_string())
        })
    {
        ctx.log(format!("debug image {}: {e}", path.display()));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn noisy(base: [u8; 3], n: usize) -> Vec<[u8; 3]> {
        (0..n)
            .map(|i| {
                let d = (i % 7) as i32 - 3;
                base.map(|c| (i32::from(c) + d).clamp(0, 255) as u8)
            })
            .collect()
    }

    /// Red tiles with a quarter of the roof in a chimney's shadow and a tree's
    /// crown over a corner: the colour is the tiles'.
    #[test]
    fn shadows_and_a_tree_do_not_colour_a_tiled_roof() {
        let mut px = noisy([160, 85, 60], 300);
        px.extend(noisy([40, 30, 28], 90));
        px.extend(noisy([70, 110, 60], 60));
        let s = classify(&px, false).unwrap();
        assert_eq!(s.m, Material::Tile);
        let [r, g, b] = s.c.map(i32::from);
        assert!(
            (r - 160).abs() <= 6 && (g - 85).abs() <= 6 && (b - 60).abs() <= 6,
            "{:?}",
            s.c
        );
    }

    #[test]
    fn flat_roofs_are_named_by_their_tone() {
        assert_eq!(
            classify(&noisy([50, 50, 52], 100), true).unwrap().m,
            Material::Bitumen
        );
        assert_eq!(
            classify(&noisy([120, 118, 115], 100), true).unwrap().m,
            Material::Gravel
        );
        assert_eq!(
            classify(&noisy([210, 210, 205], 100), true).unwrap().m,
            Material::Light
        );
        assert_eq!(
            classify(&noisy([80, 115, 60], 100), true).unwrap().m,
            Material::Green
        );
    }

    /// The haze floor goes to black, white stays white, and a colour between
    /// comes out darker and more saturated.
    #[test]
    fn dehazing_takes_the_veil_off_and_keeps_white() {
        let mut photo = RgbImage::from_pixel(400, 400, image::Rgb([150, 100, 85]));
        for x in 0..400 {
            for y in 0..8 {
                photo.put_pixel(x, y, image::Rgb([33, 45, 60]));
            }
        }
        photo.put_pixel(399, 399, image::Rgb([255, 255, 255]));
        assert_eq!(dehaze(&mut photo), [33, 45, 60]);
        assert_eq!(photo.get_pixel(0, 0).0, [0, 0, 0]);
        assert_eq!(photo.get_pixel(399, 399).0, [255, 255, 255]);
        let [r, g, b] = photo.get_pixel(200, 200).0;
        assert!(r <= 150 && g < 100 && b < 85, "{r} {g} {b}");
        assert!(i32::from(r) - i32::from(b) > 150 - 85, "not more saturated");
    }

    /// Sunlit and shaded paving split where they should, and a shaded colour
    /// comes back to the sunlit one.
    #[test]
    fn shade_is_taken_out_of_an_alley() {
        // Shade about three times darker than sun, as measured over Groningen.
        let mut px = noisy([200, 190, 170], 600);
        px.extend(noisy([112, 108, 112], 400));
        let (t, gain) = shade_model(&px);
        // Anywhere in the gap between the two clusters splits them.
        assert!(t > 66.0 && t < 186.0, "threshold {t}");
        let back = out_of_shade([112, 108, 112], gain);
        for k in 0..3 {
            assert!(
                (i32::from(back[k]) - i32::from([200, 190, 170][k])).abs() <= 6,
                "{back:?}"
            );
        }
    }

    /// The Grote Markt's measurement reads as red brick, the Herestraat's
    /// green-yellow lean as yellow, and a dark one as anthracite.
    #[test]
    fn brick_is_told_by_the_way_it_leans() {
        let red = brick_colour([161, 151, 144]);
        let lean = i32::from(red[0]) - i32::from(red[1]);
        // A red brick, and a weathered one: redder than any grey the photo
        // measures, well short of the painted red the aerial contradicts.
        assert!((20..=40).contains(&lean), "{red:?}");
        // The Vismarkt's warm yellow, which leans red-over-green as well.
        let warm = brick_colour([198, 191, 172]);
        assert!(warm[1] > warm[2] + 30 && warm[0] - warm[1] < 40, "{warm:?}");
        let yellow = brick_colour([170, 168, 155]);
        assert!(
            yellow[1] > yellow[2] + 30 && yellow[0] >= yellow[1],
            "{yellow:?}"
        );
        let dark = brick_colour([80, 82, 82]);
        assert!(lum(dark) < 100.0, "{dark:?}");
    }

    /// A 10 × 10 px square with a 4 × 4 hole fills 84 pixels, each once.
    #[test]
    fn a_ring_with_a_hole_fills_what_is_between() {
        let outer = vec![[10.0, 10.0], [20.0, 10.0], [20.0, 20.0], [10.0, 20.0]];
        let hole = vec![[13.0, 13.0], [17.0, 13.0], [17.0, 17.0], [13.0, 17.0]];
        let mut seen = std::collections::HashSet::new();
        let mut count = 0;
        fill_rings(&[outer, hole], |x, y| {
            count += 1;
            seen.insert((x, y));
        });
        assert_eq!((count, seen.len()), (84, 84));
        assert!(!seen.contains(&(15, 15)) && seen.contains(&(11, 11)));
    }

    #[test]
    fn too_few_pixels_is_no_measurement() {
        assert!(classify(&noisy([160, 85, 60], MIN_PIXELS - 1), false).is_none());
    }

    fn square(x: f64, y: f64, side: f64, flat: bool) -> Vec<Tri> {
        let (a, b, c, d) = ([x, y], [x + side, y], [x + side, y + side], [x, y + side]);
        vec![
            Tri {
                v: [a, b, c],
                h: [0.0; 3],
                flat,
                sun: 0.8,
            },
            Tri {
                v: [a, c, d],
                h: [0.0; 3],
                flat,
                sun: 0.8,
            },
        ]
    }

    /// The rim is eroded; a roof too small for the erosion keeps what it can.
    #[test]
    fn erosion_backs_off_for_a_small_roof() {
        let photo = RgbImage::from_pixel(CELL_PX, CELL_PX, image::Rgb([1, 2, 3]));
        let big = rasterise(&square(100.0, 100.0, 40.0, true)).unwrap();
        assert_eq!(big.pixels(&photo, FLAT, 4).len(), 32 * 32);
        let small = rasterise(&square(100.0, 100.0, 8.0, true)).unwrap();
        // 8×8 eroded by 4 is empty, by 2 is 4×4 = 16 < MIN, by 1 is 6×6 = 36: the
        // chessboard distance counts the rim pixel as 1.
        assert_eq!(small.pixels(&photo, FLAT, 4).len(), 36);
    }

    /// Of a gable roof only the sunlit slope is sampled.
    #[test]
    fn only_the_sunlit_slope_of_a_pitched_roof_is_sampled() {
        let mut photo = RgbImage::from_pixel(CELL_PX, CELL_PX, image::Rgb([150, 80, 60]));
        for y in 100..120 {
            for x in 100..140 {
                photo.put_pixel(x, y, image::Rgb([60, 30, 25]));
            }
        }
        let mut tris = square(100.0, 100.0, 40.0, false);
        let mut south = square(100.0, 120.0, 40.0, false);
        for t in &mut tris {
            t.v.iter_mut()
                .for_each(|v| v[1] = 100.0 + (v[1] - 100.0) / 2.0);
            t.sun = 0.3;
        }
        for t in &mut south {
            t.v.iter_mut()
                .for_each(|v| v[1] = 120.0 + (v[1] - 120.0) / 2.0);
            t.sun = 0.95;
        }
        tris.extend(south);
        let mask = rasterise(&tris).unwrap();
        let px = mask.pixels(&photo, PITCHED, 1);
        assert!(!px.is_empty());
        assert!(
            px.iter().all(|&p| p == [150, 80, 60]),
            "sampled the shaded slope"
        );
    }
}

// ─── Gardens ─────────────────────────────────────────────────────────────────

/// Every garden plot in this cell, split along the infrared photograph
/// (`gardens`). Fetches that photograph only for a cell that has a garden.
fn measure_gardens(
    ctx: &Ctx,
    cell: Cell,
    blocks: &[(u32, u32)],
    surfaces: &[cartopolis_geo::formats::bgt::Surface],
) -> Outcome<BTreeMap<String, cartopolis_geo::formats::roofs::Garden>> {
    let mut out = BTreeMap::new();
    let plots: Vec<_> = surfaces.iter().filter(|s| s.0 == GARDEN).collect();
    if plots.is_empty() {
        return Ok(out);
    }
    let mut cir = RgbImage::new(CELL_PX, CELL_PX);
    in_order(
        ctx,
        blocks,
        ctx.upstream(),
        |&(i, j)| {
            super::gardens::fetch_block(
                ctx,
                &super::gardens::block_url(cell, i, j, BLOCKS, BLOCK_PX),
                BLOCK_PX,
            )
        },
        |&(i, j), block| {
            image::imageops::replace(
                &mut cir,
                &block?,
                i64::from(i * BLOCK_PX),
                i64::from(j * BLOCK_PX),
            );
            Ok(())
        },
    )?;
    let pixel = |lat: f64, lng: f64| {
        let (fx, fy) = cartopolis_core::lat_lng_to_tile_frac(lat, lng, CACHE_ZOOM);
        [
            (fx - cell.0 as f64) * f64::from(CELL_PX),
            (fy - cell.1 as f64) * f64::from(CELL_PX),
        ]
    };
    // A garden is found by its first vertex, and neighbouring plots can start
    // on the same corner (14 of Groningen centre's 1,838). A split written under
    // a shared key is drawn over both plots, so those stay unsplit — hard
    // ground, as every garden was before the split.
    let mut starts: BTreeMap<String, usize> = BTreeMap::new();
    for s in &plots {
        if let Some(&(lat, lng)) = s.1.first().and_then(|r| r.first()) {
            *starts
                .entry(cartopolis_geo::formats::roofs::paving_key(lat, lng))
                .or_default() += 1;
        }
    }
    for s in plots {
        let Some(&first) = s.1.first().and_then(|r| r.first()) else {
            continue;
        };
        let key = cartopolis_geo::formats::roofs::paving_key(first.0, first.1);
        if starts.get(&key) != Some(&1) {
            continue;
        }
        let rings: Vec<Vec<[f64; 2]>> =
            s.1.iter()
                .map(|r| r.iter().map(|&(lat, lng)| pixel(lat, lng)).collect())
                .collect();
        let Some((planted, paved)) = super::gardens::split(&rings, CELL_PX, |x, y| {
            super::gardens::index(cir.get_pixel(x, y).0)
        }) else {
            continue;
        };
        // A plot the photograph shows all hard is drawn from the `bgt` file as
        // it stands — an unsplit garden is hard ground — so writing its ring
        // again here would only be a second copy of it. Most plots in a city
        // centre are this: 1,063 of the 1,824 in Groningen's.
        if planted.is_empty() {
            continue;
        }
        out.insert(
            key,
            super::gardens::to_lat_lng(cell, CELL_PX, &planted, &paved),
        );
    }
    Ok(out)
}
