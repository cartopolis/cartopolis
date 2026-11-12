//! The sea coloured by its depth, on the painted map.
//!
//! One flat blue made the globe a Christmas bauble (asked 2026-11-12, "de
//! oceaan mag dynamischer"): every satellite mosaic shows the shelves light and
//! the abyss dark, with the ridges and trenches between. The depth is a 2048 px
//! Web-Mercator image of the whole world (`assets/globe/ocean_depth.png`, written
//! by `atlas ocean-depth` from ETOPO1/GEBCO via the open Terrarium tiles), and
//! [`tint_sea`] paints a rendered tile's water towards a depth ramp through its
//! water mask. Both tiers call it with the same tile, so the globe and the flat
//! map agree where they meet.

use std::sync::OnceLock;

use crate::vector_tiles::{palette, SubTile};

/// The depth image, embedded once: ~0.6 MB.
static PNG: &[u8] = include_bytes!("../../../assets/globe/ocean_depth.png");

/// The deepest depth the image tells apart, metres; `atlas::ocean_depth`
/// writes `255·√(d / FULL_DEPTH_M)`.
const FULL_DEPTH_M: f32 = 8000.0;

/// The decoded image: one byte a pixel, square, Web Mercator.
struct Depth {
    edge: usize,
    data: Vec<u8>,
}

fn depth_image() -> Option<&'static Depth> {
    static DEPTH: OnceLock<Option<Depth>> = OnceLock::new();
    DEPTH
        .get_or_init(|| {
            let img = image::load_from_memory(PNG).ok()?.to_luma8();
            (img.width() == img.height()).then(|| Depth {
                edge: img.width() as usize,
                data: img.into_raw(),
            })
        })
        .as_ref()
}

/// The sea's depth in metres at a point of Web Mercator's unit square (x east,
/// y south, the world is 1), bilinear; 0 on land and on lakes above sea level.
#[must_use]
pub fn depth_m(merc: [f64; 2]) -> f32 {
    let Some(d) = depth_image() else {
        return 0.0;
    };
    let n = d.edge as f64;
    let fx = (merc[0].rem_euclid(1.0) * n - 0.5).max(0.0);
    let fy = (merc[1].clamp(0.0, 1.0) * n - 0.5).clamp(0.0, n - 1.0);
    let (x0, y0) = (fx.floor() as usize, fy.floor() as usize);
    let (x1, y1) = ((x0 + 1) % d.edge, (y0 + 1).min(d.edge - 1));
    let (tx, ty) = ((fx - x0 as f64) as f32, (fy - y0 as f64) as f32);
    let at = |x: usize, y: usize| f32::from(d.data[y * d.edge + x]) / 255.0;
    let v = (at(x0, y0) * (1.0 - tx) + at(x1, y0) * tx) * (1.0 - ty)
        + (at(x0, y1) * (1.0 - tx) + at(x1, y1) * tx) * ty;
    v * v * FULL_DEPTH_M
}

/// The painted sea's colour at `depth_m`, along [`palette::painted::SEA_RAMP`].
#[must_use]
pub fn sea_colour(depth_m: f32) -> [u8; 3] {
    let ramp = palette::painted::SEA_RAMP;
    let rgb = |c: u32| [(c >> 16) as u8, (c >> 8) as u8, c as u8].map(f32::from);
    let mut out = rgb(ramp[ramp.len() - 1].1);
    for w in ramp.windows(2) {
        let ((d0, c0), (d1, c1)) = (w[0], w[1]);
        if depth_m <= d1 {
            let t = ((depth_m - d0) / (d1 - d0)).clamp(0.0, 1.0);
            let (a, b) = (rgb(c0), rgb(c1));
            out = [0, 1, 2].map(|i| a[i] + (b[i] - a[i]) * t);
            break;
        }
    }
    out.map(|v| v.round() as u8)
}

/// Paint the sea in `rgba` (a `size`-pixel rendered tile, RGBA) by its depth,
/// through `water` (one byte a pixel, 255 = water). `source` is the tile the
/// pixels were drawn from (zoom, x, y) and `sub` the part of it they show.
///
/// Water where the depth image has none — a lake, a river, a canal, which lie
/// above sea level — keeps its paint, and so does a coast's last few
/// kilometres, which the image is too coarse to place: the ramp starts at
/// [`palette::painted::WATER`]'s own tone.
pub fn tint_sea(rgba: &mut [u8], size: u32, water: &[u8], source: (u32, u32, u32), sub: SubTile) {
    let n = size as usize;
    if water.len() != n * n || rgba.len() != n * n * 4 {
        return;
    }
    let (z, tx, ty) = source;
    let world = f64::from(1u32 << z.min(30));
    let f = f64::from(sub.factor.max(1));
    let px = f64::from(size);
    // The depth image is ~20 km a pixel, so one sample per 8 px of a tile is
    // finer than it holds, and the step is invisible after the bilinear blend.
    const STEP: usize = 8;
    let cols = n.div_ceil(STEP) + 1;
    let merc = |i: usize, j: usize| {
        let u = (f64::from(sub.x) * px + i as f64) / (px * f);
        let v = (f64::from(sub.y) * px + j as f64) / (px * f);
        [(f64::from(tx) + u) / world, (f64::from(ty) + v) / world]
    };
    let grid: Vec<f32> = (0..cols)
        .flat_map(|gj| (0..cols).map(move |gi| (gi, gj)))
        .map(|(gi, gj)| depth_m(merc(gi * STEP, gj * STEP)))
        .collect();
    if grid.iter().all(|&d| d <= 0.0) {
        return;
    }
    let plain = sea_colour(0.0);
    for j in 0..n {
        let (gj, tj) = (j / STEP, (j % STEP) as f32 / STEP as f32);
        for i in 0..n {
            let w = water[j * n + i];
            if w == 0 {
                continue;
            }
            let (gi, ti) = (i / STEP, (i % STEP) as f32 / STEP as f32);
            let g = |a: usize, b: usize| grid[b * cols + a];
            let d = (g(gi, gj) * (1.0 - ti) + g(gi + 1, gj) * ti) * (1.0 - tj)
                + (g(gi, gj + 1) * (1.0 - ti) + g(gi + 1, gj + 1) * ti) * tj;
            if d <= 0.0 {
                continue;
            }
            // Shifted by the sea's change from the plain paint rather than
            // replaced, so what is drawn over the water — a border at sea, a
            // ferry — keeps its contrast instead of being painted out.
            let sea = sea_colour(d);
            let k = f32::from(w) / 255.0;
            let p = &mut rgba[(j * n + i) * 4..(j * n + i) * 4 + 3];
            for c in 0..3 {
                let shift = (f32::from(sea[c]) - f32::from(plain[c])) * k;
                p[c] = (f32::from(p[c]) + shift).round().clamp(0.0, 255.0) as u8;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Web Mercator of a longitude and latitude, for the probes below.
    fn merc(lng: f64, lat: f64) -> [f64; 2] {
        let s = lat.to_radians().sin();
        [
            (lng + 180.0) / 360.0,
            0.5 - ((1.0 + s) / (1.0 - s)).ln() / (4.0 * std::f64::consts::PI),
        ]
    }

    #[test]
    fn the_image_holds_the_sea_floor() {
        // The North Sea is a shelf, the mid-Atlantic is abyss, the Mariana
        // trench is the deepest, Munich is land. (Not Groningen: polders below
        // sea level read as a metre or two of "depth", which only ever tints
        // water the mask already calls water, at the ramp's coastal end.)
        let north_sea = depth_m(merc(3.0, 55.0));
        let atlantic = depth_m(merc(-40.0, 30.0));
        let mariana = depth_m(merc(142.2, 11.35));
        assert!(
            (10.0..150.0).contains(&north_sea),
            "North Sea {north_sea} m"
        );
        assert!(atlantic > 2500.0, "Atlantic {atlantic} m");
        assert!(mariana > 6000.0, "Mariana {mariana} m");
        assert_eq!(depth_m(merc(11.58, 48.14)), 0.0);
    }

    #[test]
    fn the_ramp_starts_at_the_painted_water_and_darkens() {
        assert_eq!(sea_colour(0.0), sea_colour(-5.0));
        let lum = |c: [u8; 3]| u32::from(c[0]) + u32::from(c[1]) + u32::from(c[2]);
        assert!(lum(sea_colour(50.0)) > lum(sea_colour(4000.0)));
        assert!(lum(sea_colour(1000.0)) > lum(sea_colour(6000.0)));
    }

    #[test]
    fn what_is_drawn_over_the_sea_keeps_its_contrast() {
        // A z3 tile of the open Atlantic (45°W–0, 0–41°N): plain sea, and one grey pixel
        // of a border drawn over it.
        let size = 16u32;
        let w = palette::painted::WATER;
        let plain = [(w >> 16) as u8, (w >> 8) as u8, w as u8, 255];
        let mut rgba: Vec<u8> = (0..size * size).flat_map(|_| plain).collect();
        rgba[..4].copy_from_slice(&[150, 150, 150, 255]);
        let water = vec![255u8; (size * size) as usize];
        tint_sea(&mut rgba, size, &water, (3, 3, 3), SubTile::WHOLE);
        let (line, sea) = (&rgba[..3], &rgba[4..7]);
        assert_ne!(sea, &plain[..3], "the open sea is tinted");
        let gap = |a: &[u8], b: &[u8]| {
            (0..3)
                .map(|c| i32::from(a[c]) - i32::from(b[c]))
                .sum::<i32>()
        };
        assert!(
            gap(line, sea) > 150,
            "the border still stands out: {line:?} on {sea:?}"
        );
    }

    #[test]
    fn a_lake_keeps_its_paint() {
        // A z6 tile over the Alps' lakes: no sea in it, nothing changes.
        let size = 16;
        let mut rgba = vec![9u8; (size * size * 4) as usize];
        let before = rgba.clone();
        let water = vec![255u8; (size * size) as usize];
        tint_sea(&mut rgba, size, &water, (6, 33, 22), SubTile::WHOLE);
        assert_eq!(rgba, before);
    }
}
