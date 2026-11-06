//! The aerial photograph turned into a measuring sheet.
//!
//! `gather`'s `aerial.jpg` is north-up in the Dutch grid at whatever scale the
//! WMS answered, so reading a dormer's width off it meant guessing a scale and
//! un-turning the building in your head. This one is drawn **in the kit's
//! local frame**: `+y` up, a stated number of centimetres a pixel, a grid every
//! metre and five, the register outline, the laser's height contours (a roof's
//! ridges and set-backs as lines) and, with a kit, the kit's own plan in red —
//! so a pixel position is a kit coordinate by one multiplication, and a kit
//! that is too wide is seen to be.

use std::fmt::Write as _;
use std::path::Path;

use super::measure::{line, put, Site};

/// Metres of ground shown past the outline on every side.
const MARGIN_M: f64 = 8.0;
/// The largest picture made, pixels a side; the scale gives way above it.
const MAX_PX: u32 = 3000;
/// About this many laser contours over the building's height, at whole
/// metres apart: two metres drew the Martinitoren's flank as one cyan blot.
const CONTOURS: f64 = 10.0;

/// `aerial.png` and `aerial.md` into `out`; answers the headline.
pub fn aerial_cmd(target: &str, cm_per_px: f64, out: &Path) -> Result<String, String> {
    let site = Site::load(target, MARGIN_M + 10.0)?;
    let lo = site
        .outline
        .iter()
        .fold([f64::INFINITY; 2], |m, q| [m[0].min(q[0]), m[1].min(q[1])]);
    let hi = site.outline.iter().fold([f64::NEG_INFINITY; 2], |m, q| {
        [m[0].max(q[0]), m[1].max(q[1])]
    });
    let (x0, y1) = ((lo[0] - MARGIN_M).floor(), (hi[1] + MARGIN_M).ceil());
    let (x1, y0) = ((hi[0] + MARGIN_M).ceil(), (lo[1] - MARGIN_M).floor());
    let mut s = cm_per_px.clamp(1.0, 100.0) / 100.0;
    let longest = (x1 - x0).max(y1 - y0);
    if longest / s > f64::from(MAX_PX) {
        s = longest / f64::from(MAX_PX);
    }
    let w = ((x1 - x0) / s).round() as u32;
    let h = ((y1 - y0) / s).round() as u32;

    // The photograph over the same ground, in the Dutch grid, a little finer
    // than the sheet so the resampling does not blur it twice.
    let corners: Vec<[f64; 2]> = [[x0, y0], [x1, y0], [x1, y1], [x0, y1]]
        .iter()
        .map(|&q| rd_of(&site, q))
        .collect();
    let rd_min = corners
        .iter()
        .fold([f64::INFINITY; 2], |m, q| [m[0].min(q[0]), m[1].min(q[1])]);
    let rd_max = corners.iter().fold([f64::NEG_INFINITY; 2], |m, q| {
        [m[0].max(q[0]), m[1].max(q[1])]
    });
    // **Square, to match the square picture asked for.** PDOK answers a box
    // whose shape differs from the picture's with a blank white JPEG and a 200:
    // 42.66 × 43.25 m at 1082 × 1082 px came back as 7 KB of white
    // (2026-11-06).
    let side = (rd_max[0] - rd_min[0]).max(rd_max[1] - rd_min[1]).ceil();
    let mid = [(rd_min[0] + rd_max[0]) / 2.0, (rd_min[1] + rd_max[1]) / 2.0];
    let rd_lo = [mid[0] - side / 2.0, mid[1] - side / 2.0];
    let rd_hi = [mid[0] + side / 2.0, mid[1] + side / 2.0];
    let src_px = (side / (s * 0.8)).ceil().min(4000.0) as u32;
    let bbox = format!(
        "{:.2},{:.2},{:.2},{:.2}",
        rd_lo[0], rd_lo[1], rd_hi[0], rd_hi[1]
    );
    let url = format!(
        "https://service.pdok.nl/hwh/luchtfotorgb/wms/v1_0?{}",
        crate::pipeline::http::query(&[
            ("service", "WMS"),
            ("version", "1.3.0"),
            ("request", "GetMap"),
            ("layers", "Actueel_orthoHR"),
            ("styles", ""),
            ("crs", "EPSG:28992"),
            ("bbox", &bbox),
            ("width", &src_px.to_string()),
            ("height", &src_px.to_string()),
            ("format", "image/jpeg"),
        ])
    );
    let bytes = super::gather::get(&url)?;
    std::fs::create_dir_all(out).map_err(|e| e.to_string())?;
    std::fs::write(out.join("aerial-source.jpg"), &bytes).map_err(|e| e.to_string())?;
    let photo = image::load_from_memory(&bytes)
        .map_err(|e| format!("the aerial photograph: {e}"))?
        .to_rgb8();
    let (pw, ph) = (f64::from(photo.width()), f64::from(photo.height()));

    let local_of = |col: u32, row: u32| {
        [
            x0 + (f64::from(col) + 0.5) * s,
            y1 - (f64::from(row) + 0.5) * s,
        ]
    };
    let mut img = image::RgbImage::new(w, h);
    let mut laser = vec![f64::NAN; (w * h) as usize];
    let mut model = vec![f64::NAN; (w * h) as usize];
    for row in 0..h {
        for col in 0..w {
            let q = local_of(col, row);
            let rd = rd_of(&site, q);
            let u = (rd[0] - rd_lo[0]) / (rd_hi[0] - rd_lo[0]) * pw;
            let v = (rd_hi[1] - rd[1]) / (rd_hi[1] - rd_lo[1]) * ph;
            img.put_pixel(col, row, bilinear(&photo, u, v));
            let k = (row * w + col) as usize;
            laser[k] = smooth_laser(&site, q).unwrap_or(f64::NAN);
            if !site.pieces.is_empty() {
                model[k] = site.model(q).filter(|z| *z > 0.05).unwrap_or(f64::NAN);
            }
        }
    }

    // A metre grid, light, and five metres and the axes darker: drawn by
    // blending so the photograph still shows through.
    for m in (x0 as i64)..=(x1 as i64) {
        let col = ((m as f64 - x0) / s).round() as i64;
        let a = if m == 0 {
            0.7
        } else if m % 5 == 0 {
            0.45
        } else {
            0.18
        };
        for row in 0..h as i64 {
            blend(&mut img, col, row, [255, 255, 255], a);
        }
    }
    for m in (y0 as i64)..=(y1 as i64) {
        let row = ((y1 - m as f64) / s).round() as i64;
        let a = if m == 0 {
            0.7
        } else if m % 5 == 0 {
            0.45
        } else {
            0.18
        };
        for col in 0..w as i64 {
            blend(&mut img, col, row, [255, 255, 255], a);
        }
    }
    let tallest = site
        .outline
        .iter()
        .chain(std::iter::once(&[0.0, 0.0]))
        .filter_map(|&q| site.laser(q))
        .chain(laser.iter().copied().filter(|z| z.is_finite()))
        .fold(1.0, f64::max);
    let contour_m = (tallest / CONTOURS).round().max(1.0);
    // The laser's contours, cyan; the kit's plan edges, red.
    for row in 0..h.saturating_sub(1) {
        for col in 0..w.saturating_sub(1) {
            let k = (row * w + col) as usize;
            let band = |z: f64| {
                if z.is_nan() {
                    -1
                } else {
                    (z / contour_m).floor() as i64
                }
            };
            let here = band(laser[k]);
            // Only on the building and a few metres round it: over the whole sheet
            // every tree and parked van drew rings, and the roof's were lost in them.
            let q = local_of(col, row);
            let near = q[0] > lo[0] - 3.0
                && q[0] < hi[0] + 3.0
                && q[1] > lo[1] - 3.0
                && q[1] < hi[1] + 3.0;
            if (band(laser[k + 1]) != here || band(laser[k + w as usize]) != here)
                && laser[k] > 1.0
                && near
            {
                put(&mut img, i64::from(col), i64::from(row), [0, 230, 255]);
            }
            if !site.pieces.is_empty() {
                let (a, b, c) = (model[k], model[k + 1], model[k + w as usize]);
                let edge = |p: f64, q: f64| p.is_nan() != q.is_nan() || (p - q).abs() > 0.75;
                if edge(a, b) || edge(a, c) {
                    for d in 0..2 {
                        put(&mut img, i64::from(col) + d, i64::from(row), [255, 30, 30]);
                    }
                }
            }
        }
    }
    let px = |q: [f64; 2]| {
        [
            ((q[0] - x0) / s).round() as i64,
            ((y1 - q[1]) / s).round() as i64,
        ]
    };
    for i in 0..site.outline.len() {
        let (a, b) = (site.outline[i], site.outline[(i + 1) % site.outline.len()]);
        line(&mut img, px(a), px(b), [255, 220, 0], 2);
    }

    std::fs::create_dir_all(out).map_err(|e| e.to_string())?;
    img.save(out.join("aerial.png"))
        .map_err(|e| e.to_string())?;
    let origin = px([0.0, 0.0]);
    let mut md = String::new();
    let _ = writeln!(
        md,
        "# {} — the aerial photograph in the local frame\n\n\
         `aerial.png` is {w} × {h} px at **{:.1} cm a pixel**, local `+y` up \
         (compass {:.1}°). A pixel's local position:\n\n\
         - x = {x0:.2} + (col + 0.5) × {s:.4}\n\
         - y = {y1:.2} − (row + 0.5) × {s:.4}\n\n\
         so the local origin is pixel ({}, {}). Lines: white every metre, brighter every \
         5 m and on the axes; **yellow** the register outline; **cyan** the laser's \
         height every {contour_m} m above the ground (a ridge or a set-back is where they \
         bunch or turn){}.\n\n\
         The photograph is PDOK's orthophoto (Beeldmateriaal Nederland, CC BY 4.0): \
         straight down at the ground, so a roof's edge leans outward from the picture's \
         centre by its height — measure a wall's foot, or use the cyan lines, not a \
         roof's edge, for a plan. It was read from:\n\n<{url}>\n",
        site.name,
        s * 100.0,
        site.rotation_deg,
        origin[0],
        origin[1],
        if site.pieces.is_empty() {
            ""
        } else {
            "; **red** the kit's edges seen from above"
        }
    );
    std::fs::write(out.join("aerial.md"), &md).map_err(|e| e.to_string())?;
    Ok(format!(
        "wrote {}: aerial.png ({w} × {h} px, {:.1} cm a pixel), aerial.md",
        out.display(),
        s * 100.0
    ))
}

/// The laser at a local point, averaged over the four half-metre samples
/// round it: the surface model's pixels, turned into the kit's frame, draw
/// every contour as a staircase otherwise.
fn smooth_laser(site: &Site, q: [f64; 2]) -> Option<f64> {
    let hs: Vec<f64> = [[-0.25, -0.25], [0.25, -0.25], [-0.25, 0.25], [0.25, 0.25]]
        .iter()
        .filter_map(|d| site.laser([q[0] + d[0], q[1] + d[1]]))
        .collect();
    (!hs.is_empty()).then(|| hs.iter().sum::<f64>() / hs.len() as f64)
}

/// The Dutch grid position of a local point.
fn rd_of(site: &Site, q: [f64; 2]) -> [f64; 2] {
    let (lat, lng) = site.plane.to_lat_lng(site.to_en(q));
    cartopolis_geo::ahn::wgs84_to_rd(lat, lng)
}

fn bilinear(img: &image::RgbImage, u: f64, v: f64) -> image::Rgb<u8> {
    let (w, h) = (img.width() as i64, img.height() as i64);
    let (x, y) = (u - 0.5, v - 0.5);
    let (x0, y0) = (x.floor() as i64, y.floor() as i64);
    let (fx, fy) = (x - x0 as f64, y - y0 as f64);
    let at = |x: i64, y: i64| {
        let p = img.get_pixel(x.clamp(0, w - 1) as u32, y.clamp(0, h - 1) as u32);
        [f64::from(p[0]), f64::from(p[1]), f64::from(p[2])]
    };
    let (a, b, c, d) = (
        at(x0, y0),
        at(x0 + 1, y0),
        at(x0, y0 + 1),
        at(x0 + 1, y0 + 1),
    );
    let mut out = [0u8; 3];
    for k in 0..3 {
        let top = a[k] * (1.0 - fx) + b[k] * fx;
        let bottom = c[k] * (1.0 - fx) + d[k] * fx;
        out[k] = (top * (1.0 - fy) + bottom * fy).round().clamp(0.0, 255.0) as u8;
    }
    image::Rgb(out)
}

fn blend(img: &mut image::RgbImage, x: i64, y: i64, c: [u8; 3], a: f64) {
    if x < 0 || y < 0 || x as u32 >= img.width() || y as u32 >= img.height() {
        return;
    }
    let p = img.get_pixel_mut(x as u32, y as u32);
    for k in 0..3 {
        p[k] = (f64::from(p[k]) * (1.0 - a) + f64::from(c[k]) * a).round() as u8;
    }
}
