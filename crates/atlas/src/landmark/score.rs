//! How far a kit is from what the laser measured.
//!
//! From above, every half-metre pixel inside the register outline has two
//! heights above the ground: the highest point of the model there, and AHN's
//! surface model there. Their difference is a number an agent can drive down
//! without anybody looking — stages too wide, too low, a spire too short — and
//! the map of where it is wrong says which part to move. It sees nothing of a
//! facade; photographs answer for those.

use super::kit::Piece;
use crate::pipeline::models::Surface;

/// Pixel edge, metres: AHN's own.
pub const STEP_M: f64 = 0.5;

/// A difference at or under this is a match: AHN's own vertical noise on a
/// sloped roof, plus a pixel's worth of plan error on a wall's edge.
pub const MATCH_M: f64 = 1.5;

pub struct Score {
    /// Pixels inside the outline with a laser height.
    pub pixels: usize,
    /// Of those, how many the model matches within [`MATCH_M`].
    pub matched: usize,
    pub median_abs_m: f64,
    pub p90_abs_m: f64,
    /// Mean of model − laser: positive is a model that stands too tall.
    pub mean_m: f64,
    /// Per metre of plan, from the north-west: `.` matched, `+` model too high,
    /// `-` too low, ` ` outside the outline; with the laser's height above
    /// ground in metres alongside, to say by how much.
    pub map: String,
    /// `(height, laser m², model m²)`: the plan area standing at least that
    /// high, every two metres — a stage too wide or too short shows as a
    /// ratio away from 1 at its heights, whatever a half-metre shift does to
    /// the pixels.
    pub by_height: Vec<(f64, f64, f64)>,
}

/// Model tops over a grid, by drawing every non-vertical triangle from above.
fn tops(pieces: &[Piece], min: [f64; 2], nx: usize, ny: usize) -> Vec<f64> {
    let mut top = vec![f64::NEG_INFINITY; nx * ny];
    for piece in pieces {
        for t in &piece.t {
            let [a, b, c] = t.map(|i| piece.p[i as usize]);
            let det = (b[0] - a[0]) * (c[1] - a[1]) - (c[0] - a[0]) * (b[1] - a[1]);
            if det.abs() < 1e-9 {
                continue;
            }
            let lo = [a[0].min(b[0]).min(c[0]), a[1].min(b[1]).min(c[1])];
            let hi = [a[0].max(b[0]).max(c[0]), a[1].max(b[1]).max(c[1])];
            let c0 = (((lo[0] - min[0]) / STEP_M).floor().max(0.0)) as usize;
            let r0 = (((lo[1] - min[1]) / STEP_M).floor().max(0.0)) as usize;
            let c1 = ((((hi[0] - min[0]) / STEP_M).ceil()) as usize).min(nx);
            let r1 = ((((hi[1] - min[1]) / STEP_M).ceil()) as usize).min(ny);
            for r in r0..r1 {
                for col in c0..c1 {
                    let p = [
                        min[0] + (col as f64 + 0.5) * STEP_M,
                        min[1] + (r as f64 + 0.5) * STEP_M,
                    ];
                    let l1 = ((b[1] - c[1]) * (p[0] - c[0]) + (c[0] - b[0]) * (p[1] - c[1])) / det;
                    let l2 = ((c[1] - a[1]) * (p[0] - c[0]) + (a[0] - c[0]) * (p[1] - c[1])) / det;
                    let l3 = 1.0 - l1 - l2;
                    if l1 < -1e-9 || l2 < -1e-9 || l3 < -1e-9 {
                        continue;
                    }
                    let z = l1 * a[2] + l2 * b[2] + l3 * c[2];
                    let k = r * nx + col;
                    top[k] = top[k].max(z);
                }
            }
        }
    }
    top
}

pub(crate) fn inside(ring: &[[f64; 2]], p: [f64; 2]) -> bool {
    let mut odd = false;
    let n = ring.len();
    for i in 0..n {
        let (a, b) = (ring[i], ring[(i + 1) % n]);
        if (a[1] > p[1]) != (b[1] > p[1])
            && p[0] < a[0] + (p[1] - a[1]) / (b[1] - a[1]) * (b[0] - a[0])
        {
            odd = !odd;
        }
    }
    odd
}

/// Score `pieces` (east/north/up of the surface's plane) against the surface
/// model, inside `outline` (east/north), with the ground at `ground` NAP.
#[must_use]
pub fn score(pieces: &[Piece], outline: &[[f64; 2]], surface: &Surface, ground: f64) -> Score {
    let lo = outline
        .iter()
        .fold([f64::INFINITY; 2], |m, q| [m[0].min(q[0]), m[1].min(q[1])]);
    let hi = outline.iter().fold([f64::NEG_INFINITY; 2], |m, q| {
        [m[0].max(q[0]), m[1].max(q[1])]
    });
    let nx = ((hi[0] - lo[0]) / STEP_M).ceil() as usize;
    let ny = ((hi[1] - lo[1]) / STEP_M).ceil() as usize;
    let top = tops(pieces, lo, nx, ny);
    let mut diffs = Vec::new();
    let mut pairs: Vec<(f64, f64)> = Vec::new();
    // (model − laser, laser above ground) per pixel; None outside or unmeasured.
    let mut cells: Vec<Option<(f64, f64)>> = vec![None; nx * ny];
    for r in 0..ny {
        for c in 0..nx {
            let p = [
                lo[0] + (c as f64 + 0.5) * STEP_M,
                lo[1] + (r as f64 + 0.5) * STEP_M,
            ];
            if !inside(outline, p) {
                continue;
            }
            let Some(h) = surface.at(p) else { continue };
            let laser = f64::from(h) - ground;
            let model = top[r * nx + c].max(0.0);
            let d = model - laser;
            diffs.push(d);
            pairs.push((model, laser));
            cells[r * nx + c] = Some((d, laser));
        }
    }
    let pixels = diffs.len();
    let matched = diffs.iter().filter(|d| d.abs() <= MATCH_M).count();
    let mut abs: Vec<f64> = diffs.iter().map(|d| d.abs()).collect();
    abs.sort_by(f64::total_cmp);
    let pick = |q: f64| {
        abs.get(((abs.len() as f64 - 1.0) * q).round() as usize)
            .copied()
            .unwrap_or(0.0)
    };
    let mean_m = if pixels == 0 {
        0.0
    } else {
        diffs.iter().sum::<f64>() / pixels as f64
    };

    let px_m2 = STEP_M * STEP_M;
    let top_h = pairs.iter().map(|p| p.0.max(p.1)).fold(0.0, f64::max);
    let by_height = (1..)
        .map(|k| f64::from(k) * 2.0)
        .take_while(|h| *h <= top_h)
        .map(|h| {
            let laser = pairs.iter().filter(|p| p.1 >= h).count() as f64 * px_m2;
            let model = pairs.iter().filter(|p| p.0 >= h).count() as f64 * px_m2;
            (h, laser, model)
        })
        .collect();

    // The map at a metre: two pixels by two, the worst of them.
    let mut map = String::new();
    for r in (0..ny).step_by(2).rev() {
        let mut marks = String::new();
        let mut heights = String::new();
        for c in (0..nx).step_by(2) {
            let block: Vec<(f64, f64)> = [(r, c), (r + 1, c), (r, c + 1), (r + 1, c + 1)]
                .into_iter()
                .filter(|&(r, c)| r < ny && c < nx)
                .filter_map(|(r, c)| cells[r * nx + c])
                .collect();
            let worst = block
                .iter()
                .copied()
                .max_by(|a, b| a.0.abs().total_cmp(&b.0.abs()));
            match worst {
                None => {
                    marks.push(' ');
                    heights.push_str("    ");
                }
                Some((d, laser)) => {
                    marks.push(if d.abs() <= MATCH_M {
                        '.'
                    } else if d > 0.0 {
                        '+'
                    } else {
                        '-'
                    });
                    heights.push_str(&format!("{:>4}", laser.round() as i64));
                }
            }
        }
        map.push_str(&format!("{marks}  |{heights}\n"));
    }
    Score {
        pixels,
        matched,
        median_abs_m: pick(0.5),
        p90_abs_m: pick(0.9),
        mean_m,
        map,
        by_height,
    }
}

impl Score {
    #[must_use]
    pub fn report(&self) -> String {
        let mut table =
            String::from("| ≥ m | laser m² | model m² | model ÷ laser |\n|---|---|---|---|\n");
        for (h, laser, model) in &self.by_height {
            let ratio = if *laser > 0.0 {
                format!("{:.2}", model / laser)
            } else {
                "—".into()
            };
            table.push_str(&format!("| {h:.0} | {laser:.1} | {model:.1} | {ratio} |\n"));
        }
        format!(
            "laser match: {:.1}% of {} pixels within {MATCH_M} m; |model − laser| median {:.2} m, \
             p90 {:.2} m; mean {:+.2} m (positive: model too tall)\n\n\
             ## Plan area by height\n\n{table}\n\
             ## Where\n\n\
             per metre from the north-west: . match, + model too high, - model too low; \
             right: laser height above ground, m\n\n{}",
            100.0 * self.matched as f64 / self.pixels.max(1) as f64,
            self.pixels,
            self.median_abs_m,
            self.p90_abs_m,
            self.mean_m,
            self.map
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::pipeline::models::Plane;

    /// A 10 m box scored against a laser that saw a 10 m box matches
    /// everywhere; against one that saw 14 m it is 4 m short everywhere.
    #[test]
    fn a_box_scores_against_its_own_height() {
        let kit = super::super::kit::Kit::parse(
            r#"{"name": "t", "pand": "0014100010924795", "origin": [53.2194, 6.5682],
                "materials": {"b": {"rgb": [1, 2, 3]}},
                "parts": [{"name": "p", "material": "b", "shape": {"rect": {"w": 8, "d": 8}},
                           "rings": [[0, 1], [10, 1]]}]}"#,
        )
        .unwrap();
        let pieces = kit.mesh().unwrap();
        let plane = Plane::at(53.2194, 6.5682);
        let outline = [[-4.0, -4.0], [4.0, -4.0], [4.0, 4.0], [-4.0, 4.0]];
        let flat = |h: f32| {
            let rd = cartopolis_geo::ahn::wgs84_to_rd(53.2194, 6.5682);
            Surface::from_raster(
                plane,
                [rd[0] - 20.0, rd[1] - 20.0],
                0.5,
                80,
                vec![h; 80 * 80],
            )
        };
        let s = score(&pieces, &outline, &flat(17.0), 7.0);
        assert!(s.pixels > 200, "{}", s.pixels);
        assert_eq!(s.matched, s.pixels);
        let s = score(&pieces, &outline, &flat(21.0), 7.0);
        assert_eq!(s.matched, 0);
        assert!((s.mean_m + 4.0).abs() < 1e-6, "{}", s.mean_m);
    }
}
