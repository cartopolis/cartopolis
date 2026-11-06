//! Measuring a facade from a photograph (`docs/notes/landmark-kit.md`).
//!
//! The score sees a kit from above only; a face's arches, ledges and windows
//! are read off photographs. Read by eye, a photograph's scale drifts with
//! height and with the angle it was taken at, so here a photograph is first
//! **posed**: four or more points whose place in the kit's frame is known (a
//! corner from `facts.md`, a ledge at its laser height) are clicked in it, and
//! the camera that sees them there is solved for. With the pose:
//!
//! - **measure** turns a pixel on a wall into metres along the wall and height;
//! - **rectify** redraws a wall straight on, at a known scale, with a metre
//!   grid on it, so every opening's width and height reads off directly;
//! - **overlay** draws the kit's edges over the photograph, to see where the
//!   model and the building part.
//!
//! Everything is in the kit's local frame: `x`, `y` metres as a kit writes
//! them, `z` metres above the ground. A pixel is `(u, v)` from the picture's
//! top-left corner, `v` growing downward, so pixel `(i, j)` has its centre at
//! `(i + 0.5, j + 0.5)`.

use std::collections::HashMap;
use std::fmt::Write as _;
use std::path::Path;

use image::{Rgb, RgbImage};
use serde::{Deserialize, Serialize};

use super::kit::{Kit, Piece};
use crate::pipeline::models::Plane;

/// Where a photograph was taken from and how, written as `pose.json`.
///
/// A pinhole camera: the principal point at the picture's centre, square
/// pixels, no lens distortion. A phone or a Commons photograph that was not
/// cropped off-centre fits that within a pixel or two at the middle; a wide
/// lens bends straight lines near the corners, so pick points away from them.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Pose {
    /// The camera, local metres; `z` above the ground.
    pub x: f64,
    pub y: f64,
    pub z: f64,
    /// Which way it looks, degrees clockwise from local `+y` — a compass
    /// bearing in the kit's frame.
    pub yaw_deg: f64,
    /// Up is positive.
    pub pitch_deg: f64,
    /// Positive turns the camera clockwise as seen from behind it, so a level
    /// line in the picture leans down to the left.
    pub roll_deg: f64,
    /// Focal length in pixels of this picture: its width over twice the tangent
    /// of half the horizontal field of view.
    pub focal_px: f64,
    pub width: u32,
    pub height: u32,
    /// The photograph's and the kit's file names this was solved for, so a pose
    /// is not used on another picture of a different size or crop by mistake.
    #[serde(default)]
    pub photo: String,
    #[serde(default)]
    pub kit: String,
}

impl Pose {
    /// From the solver's parameters: position, yaw, pitch, roll (radians),
    /// focal length.
    fn from_params(v: &[f64; 7], size: (u32, u32)) -> Self {
        Self {
            x: v[0],
            y: v[1],
            z: v[2],
            yaw_deg: v[3].to_degrees(),
            pitch_deg: v[4].to_degrees(),
            roll_deg: v[5].to_degrees(),
            focal_px: v[6],
            width: size.0,
            height: size.1,
            photo: String::new(),
            kit: String::new(),
        }
    }

    fn eye(&self) -> [f64; 3] {
        [self.x, self.y, self.z]
    }

    /// The camera's right, up and forward unit vectors, local.
    fn axes(&self) -> [[f64; 3]; 3] {
        let (sy, cy) = self.yaw_deg.to_radians().sin_cos();
        let (sp, cp) = self.pitch_deg.to_radians().sin_cos();
        let (sr, cr) = self.roll_deg.to_radians().sin_cos();
        let forward = [sy * cp, cy * cp, sp];
        let right = [cy, -sy, 0.0];
        let up = cross(right, forward);
        [
            add(scale(right, cr), scale(up, -sr)),
            add(scale(right, sr), scale(up, cr)),
            forward,
        ]
    }

    /// A local point in camera coordinates: right, up, forward (depth).
    fn camera(&self, p: [f64; 3]) -> [f64; 3] {
        let d = sub(p, self.eye());
        let [r, u, f] = self.axes();
        [dot(d, r), dot(d, u), dot(d, f)]
    }

    /// Camera coordinates in front of it to a pixel.
    fn to_pixel(&self, c: [f64; 3]) -> [f64; 2] {
        [
            f64::from(self.width) / 2.0 + self.focal_px * c[0] / c[2],
            f64::from(self.height) / 2.0 - self.focal_px * c[1] / c[2],
        ]
    }

    /// Where a local point lands in the picture; `None` behind the camera.
    /// It may land outside the picture.
    #[must_use]
    pub fn project(&self, p: [f64; 3]) -> Option<[f64; 2]> {
        let c = self.camera(p);
        (c[2] > 1e-9).then(|| self.to_pixel(c))
    }

    /// The ray through a pixel: the camera's position and a unit direction.
    #[must_use]
    pub fn ray(&self, px: [f64; 2]) -> ([f64; 3], [f64; 3]) {
        let [r, u, f] = self.axes();
        let a = (px[0] - f64::from(self.width) / 2.0) / self.focal_px;
        let b = -(px[1] - f64::from(self.height) / 2.0) / self.focal_px;
        (self.eye(), unit(add(f, add(scale(r, a), scale(u, b)))))
    }
}

/// Eye height of a photograph taken standing, metres.
const EYE_M: f64 = 1.6;
/// Starting focal lengths, times the picture's width: from a wide phone lens
/// (~70° across) to a short telephoto (~23°).
const FOCALS: [f64; 4] = [0.7, 1.0, 1.5, 2.5];
/// Starting distances round the points, times their spread.
const RING: [f64; 4] = [1.5, 3.0, 6.0, 12.0];
const BEARINGS: usize = 16;
/// Where a solved camera may stand, metres above the ground: a little under
/// it (a sunken square, the laser's ground a step off), and as high as a
/// tower's gallery or a drone.
const MIN_EYE_M: f64 = -3.0;
const MAX_EYE_M: f64 = 300.0;
/// How far from the points, metres: a building photographed from further is
/// a few pixels wide in a 1280 px thumbnail.
const MAX_FAR_M: f64 = 2000.0;
/// The longest lens, in picture widths of focal length: about 3° across, a
/// 600 mm lens on a full-frame camera.
const MAX_ZOOM: f64 = 20.0;
/// The residual of a point behind the camera: large and flat, so a step that
/// puts one there is never taken.
const BEHIND_PX: f64 = 1e4;
const MAX_STEPS: usize = 300;

/// Solve the camera that sees each `(pixel, local point)` pair, for a picture
/// of `size` pixels. `from` is local `x, y` the photograph was taken near (its
/// Commons coordinate); without it the camera is searched for all round.
///
/// Seven unknowns — position, three angles, focal length — so four points or
/// more (eight equations). Levenberg–Marquardt from many starts, because one
/// start converges to whatever valley it is in: a camera on the far side, or
/// one with the focal length traded for distance. The lowest residual wins
/// among cameras that have every point in front and stand outside the points'
/// plan box (a camera inside the building is a fit, not a photograph).
///
/// Answers the pose and each point's residual in pixels.
pub fn solve_pose(
    points: &[([f64; 2], [f64; 3])],
    size: (u32, u32),
    from: Option<[f64; 2]>,
) -> Result<(Pose, Vec<f64>), String> {
    if points.len() < 4 {
        return Err(format!(
            "a pose needs four points or more (eight equations for seven unknowns); got {}",
            points.len()
        ));
    }
    if size.0 == 0 || size.1 == 0 {
        return Err("the picture has no size".into());
    }
    let n = points.len() as f64;
    let centre = points
        .iter()
        .fold([0.0; 3], |m, q| add(m, scale(q.1, 1.0 / n)));
    let spread = points
        .iter()
        .map(|q| (q.1[0] - centre[0]).hypot(q.1[1] - centre[1]))
        .fold(1.0, f64::max);
    let (lo, hi) = points.iter().fold(
        ([f64::INFINITY; 2], [f64::NEG_INFINITY; 2]),
        |(lo, hi), q| {
            (
                [lo[0].min(q.1[0]), lo[1].min(q.1[1])],
                [hi[0].max(q.1[0]), hi[1].max(q.1[1])],
            )
        },
    );
    // `from` is one more start, not the only one: a Commons coordinate is the
    // uploader's, often tens of metres off, and alone it led the fit astray
    // where the ring found a camera (2026-11-06).
    let eyes: Vec<[f64; 3]> = from
        .map(|q| [q[0], q[1], EYE_M])
        .into_iter()
        .chain((0..BEARINGS).flat_map(|k| {
            let (s, c) = (std::f64::consts::TAU * k as f64 / BEARINGS as f64).sin_cos();
            RING.map(|m| {
                [
                    centre[0] + m * spread * s,
                    centre[1] + m * spread * c,
                    EYE_M,
                ]
            })
        }))
        .collect();
    let mut best: Option<(f64, [f64; 7])> = None;
    // The closest fit refused, and why: said in the refusal, since "no
    // camera" alone leaves the builder guessing which point is wrong.
    let mut refused: Option<(f64, [f64; 7], &'static str)> = None;
    for eye in &eyes {
        let d = sub(centre, *eye);
        for k in FOCALS {
            let start = [
                eye[0],
                eye[1],
                eye[2],
                d[0].atan2(d[1]),
                d[2].atan2(d[0].hypot(d[1])),
                0.0,
                k * f64::from(size.0),
            ];
            let (v, cost) = refine(start, points, size);
            let pose = Pose::from_params(&v, size);
            let outside = !(lo[0]..=hi[0]).contains(&pose.x) || !(lo[1]..=hi[1]).contains(&pose.y);
            let in_front = points.iter().all(|q| pose.camera(q.1)[2] > 0.0);
            // **A camera somebody could have held.** Points that disagree
            // (one clicked on the wrong ledge) let the fit run off to a
            // camera kilometres away with a lens of a thousand degrees'
            // worth of zoom, where every point lines up as on a drawing: the
            // Martinitoren's first real try solved 30 km off, under the
            // ground, at 0.2° across (2026-11-06).
            let far = (pose.x - centre[0]).hypot(pose.y - centre[1]);
            let why = if !cost.is_finite() || pose.focal_px <= 0.0 {
                Some("no lens at all")
            } else if !outside {
                Some("the camera inside the building")
            } else if !in_front {
                Some("a point behind the camera")
            } else if pose.z < MIN_EYE_M {
                Some("the camera under the ground")
            } else if pose.z > MAX_EYE_M {
                Some("the camera higher than anybody stands")
            } else if far > MAX_FAR_M {
                Some("the camera too far off")
            } else if pose.focal_px > MAX_ZOOM * f64::from(size.0) {
                Some("a lens longer than any camera's")
            } else {
                None
            };
            match why {
                None if best.is_none_or(|b| cost < b.0) => best = Some((cost, v)),
                Some(why) if cost.is_finite() && refused.is_none_or(|r| cost < r.0) => {
                    refused = Some((cost, v, why));
                }
                _ => {}
            }
        }
    }
    let Some((_, v)) = best else {
        let closest = refused.map_or(String::new(), |(cost, v, why)| {
            let p = Pose::from_params(&v, size);
            format!(
                " The closest fit ({:.0} px RMS) had {why}: at x {:.0}, y {:.0}, z {:.0} m, \
                 {:.1}° across.",
                (cost / points.len() as f64).sqrt(),
                p.x,
                p.y,
                p.z,
                2.0 * (f64::from(size.0) / 2.0 / p.focal_px).atan().to_degrees()
            )
        });
        return Err(format!(
            "no camera a person could have held — on the ground or up to {MAX_EYE_M} m, \
             within {MAX_FAR_M} m, a lens no longer than {MAX_ZOOM} picture widths — sees \
             every point in front of it from outside them.{closest} The points disagree: \
             check each pixel is paired with the right point and height, or give `--from` \
             where the photograph was taken."
        ));
    };
    let pose = Pose::from_params(&v, size);
    let residuals = points
        .iter()
        .map(|(px, p)| {
            pose.project(*p)
                .map_or(f64::INFINITY, |q| (q[0] - px[0]).hypot(q[1] - px[1]))
        })
        .collect();
    Ok((pose, residuals))
}

/// Pixel errors, two per point, of the camera `v`.
fn residuals(v: &[f64; 7], points: &[([f64; 2], [f64; 3])], size: (u32, u32)) -> Vec<f64> {
    let pose = Pose::from_params(v, size);
    let mut r = Vec::with_capacity(points.len() * 2);
    for (px, p) in points {
        match pose.project(*p).filter(|_| pose.focal_px > 0.0) {
            Some(q) => r.extend([q[0] - px[0], q[1] - px[1]]),
            None => r.extend([BEHIND_PX, BEHIND_PX]),
        }
    }
    r
}

/// Levenberg–Marquardt from `start`, with a central-difference Jacobian and
/// the damping scaled by its diagonal (so metres, radians and pixels of focal
/// length need no hand-picked weights). Answers the camera and its summed
/// squared residual.
fn refine(start: [f64; 7], points: &[([f64; 2], [f64; 3])], size: (u32, u32)) -> ([f64; 7], f64) {
    let sum_sq = |r: &[f64]| r.iter().map(|x| x * x).sum::<f64>();
    let mut v = start;
    let mut r = residuals(&v, points, size);
    let mut cost = sum_sq(&r);
    let m = r.len();
    let mut lambda = 1e-3;
    for _ in 0..MAX_STEPS {
        let mut j = vec![0.0; m * 7];
        for k in 0..7 {
            let h = 1e-6 * v[k].abs().max(1.0);
            let (mut a, mut b) = (v, v);
            a[k] += h;
            b[k] -= h;
            let (ra, rb) = (residuals(&a, points, size), residuals(&b, points, size));
            for i in 0..m {
                j[i * 7 + k] = (ra[i] - rb[i]) / (2.0 * h);
            }
        }
        let mut jtj = [[0.0; 7]; 7];
        let mut jtr = [0.0; 7];
        for i in 0..m {
            let row = &j[i * 7..i * 7 + 7];
            for p in 0..7 {
                jtr[p] -= row[p] * r[i];
                for q in 0..7 {
                    jtj[p][q] += row[p] * row[q];
                }
            }
        }
        let mut gain = None;
        for _ in 0..16 {
            let mut a = jtj;
            for p in 0..7 {
                a[p][p] += lambda * jtj[p][p].max(1e-12);
            }
            let Some(d) = solve7(a, jtr) else {
                lambda *= 10.0;
                continue;
            };
            let mut next = v;
            for p in 0..7 {
                next[p] += d[p];
            }
            let rn = residuals(&next, points, size);
            let cn = sum_sq(&rn);
            if cn < cost {
                gain = Some(cost - cn);
                (v, r, cost) = (next, rn, cn);
                lambda = (lambda / 3.0).max(1e-15);
                break;
            }
            lambda *= 4.0;
        }
        match gain {
            Some(g) if g > 1e-12 * cost && cost > 1e-18 => {}
            _ => break,
        }
    }
    (v, cost)
}

/// `a x = b` by Gaussian elimination with partial pivoting.
fn solve7(mut a: [[f64; 7]; 7], mut b: [f64; 7]) -> Option<[f64; 7]> {
    for col in 0..7 {
        let pivot = (col..7).max_by(|&i, &k| a[i][col].abs().total_cmp(&a[k][col].abs()))?;
        if a[pivot][col].abs() < 1e-300 {
            return None;
        }
        a.swap(col, pivot);
        b.swap(col, pivot);
        for row in col + 1..7 {
            let f = a[row][col] / a[col][col];
            for k in col..7 {
                a[row][k] -= f * a[col][k];
            }
            b[row] -= f * b[col];
        }
    }
    let mut x = [0.0; 7];
    for row in (0..7).rev() {
        let s: f64 = (row + 1..7).map(|k| a[row][k] * x[k]).sum();
        x[row] = (b[row] - s) / a[row][row];
    }
    x.iter().all(|q| q.is_finite()).then_some(x)
}

/// Where each pixel's ray meets the vertical plane through the wall's two
/// local points: `(metres along the wall from the first point, height)`.
/// `None` where the ray runs parallel to the wall or meets it behind the
/// camera. Along may be negative or past the second point: the plane goes on.
#[must_use]
pub fn measure_on_wall(
    pose: &Pose,
    wall: ([f64; 2], [f64; 2]),
    pixels: &[[f64; 2]],
) -> Vec<Option<(f64, f64)>> {
    let (a, b) = wall;
    let len = (b[0] - a[0]).hypot(b[1] - a[1]);
    if len < 1e-9 {
        return vec![None; pixels.len()];
    }
    let along = [(b[0] - a[0]) / len, (b[1] - a[1]) / len, 0.0];
    let normal = [-along[1], along[0], 0.0];
    let a3 = [a[0], a[1], 0.0];
    pixels
        .iter()
        .map(|&px| {
            let (o, d) = pose.ray(px);
            let den = dot(normal, d);
            if den.abs() < 1e-12 {
                return None;
            }
            let t = dot(normal, sub(a3, o)) / den;
            if t <= 0.0 {
                return None;
            }
            let hit = add(o, scale(d, t));
            Some((dot(sub(hit, a3), along), hit[2]))
        })
        .collect()
}

/// Mid-grey: where the photograph does not reach.
const UNSEEN: [u8; 3] = [128, 128, 128];

/// The wall seen straight on: column `c` is `c × m_per_px` metres along the
/// wall from its first point, row `r` is `bottom + (rows − 1 − r) × m_per_px`
/// metres high, each pixel sampled from the photograph where the pose sees
/// that point of the wall. A light line every metre and a stronger one every
/// five, counted from the first point and from the ground, so a width or a
/// height is read off it by counting.
///
/// Whatever stands proud of the wall (a buttress, a balcony) is drawn where
/// its ray meets the wall's plane, so measure only what lies in it.
#[must_use]
pub fn rectify(
    pose: &Pose,
    photo: &RgbImage,
    wall: ([f64; 2], [f64; 2]),
    bottom: f64,
    top: f64,
    m_per_px: f64,
) -> RgbImage {
    let (a, b) = wall;
    let len = (b[0] - a[0]).hypot(b[1] - a[1]);
    let dir = if len > 1e-9 {
        [(b[0] - a[0]) / len, (b[1] - a[1]) / len]
    } else {
        [1.0, 0.0]
    };
    let cols = (len / m_per_px + 1e-9).floor() as u32 + 1;
    let rows = ((top - bottom).max(0.0) / m_per_px + 1e-9).floor() as u32 + 1;
    let mut out = RgbImage::new(cols, rows);
    for r in 0..rows {
        let h = bottom + f64::from(rows - 1 - r) * m_per_px;
        for c in 0..cols {
            let s = f64::from(c) * m_per_px;
            let p = [a[0] + dir[0] * s, a[1] + dir[1] * s, h];
            let colour = pose
                .project(p)
                .and_then(|q| sample(photo, q))
                .unwrap_or(UNSEEN);
            out.put_pixel(c, r, Rgb(colour));
        }
    }
    // Every metre first, every five metres over it.
    let lines = |lo: f64, hi: f64| (lo.ceil() as i64..=hi.floor() as i64).collect::<Vec<_>>();
    for strong in [false, true] {
        let (to, k) = if strong {
            ([255, 215, 0], 0.75)
        } else {
            ([255, 255, 255], 0.35)
        };
        for m in lines(0.0, len) {
            if (m.rem_euclid(5) == 0) != strong {
                continue;
            }
            let c = (m as f64 / m_per_px).round() as u32;
            if c < cols {
                for r in 0..rows {
                    blend(&mut out, c, r, to, k);
                }
            }
        }
        for m in lines(bottom, top) {
            if (m.rem_euclid(5) == 0) != strong {
                continue;
            }
            let up = ((m as f64 - bottom) / m_per_px).round() as u32;
            if up < rows {
                for c in 0..cols {
                    blend(&mut out, c, rows - 1 - up, to, k);
                }
            }
        }
    }
    out
}

fn blend(img: &mut RgbImage, x: u32, y: u32, to: [u8; 3], k: f64) {
    let p = img.get_pixel_mut(x, y);
    for i in 0..3 {
        p.0[i] = (f64::from(p.0[i]) * (1.0 - k) + f64::from(to[i]) * k).round() as u8;
    }
}

/// The photograph at a pixel position, bilinearly; `None` off the picture.
fn sample(photo: &RgbImage, q: [f64; 2]) -> Option<[u8; 3]> {
    let (w, h) = (f64::from(photo.width()), f64::from(photo.height()));
    if !(0.0..=w).contains(&q[0]) || !(0.0..=h).contains(&q[1]) || w < 1.0 || h < 1.0 {
        return None;
    }
    let x = (q[0] - 0.5).clamp(0.0, w - 1.0);
    let y = (q[1] - 0.5).clamp(0.0, h - 1.0);
    let (x0, y0) = (x.floor() as u32, y.floor() as u32);
    let (x1, y1) = (
        (x0 + 1).min(photo.width() - 1),
        (y0 + 1).min(photo.height() - 1),
    );
    let (fx, fy) = (x - f64::from(x0), y - f64::from(y0));
    let at = |x, y, i: usize| f64::from(photo.get_pixel(x, y).0[i]);
    let mut out = [0u8; 3];
    for (i, o) in out.iter_mut().enumerate() {
        let top = at(x0, y0, i) * (1.0 - fx) + at(x1, y0, i) * fx;
        let bot = at(x0, y1, i) * (1.0 - fx) + at(x1, y1, i) * fx;
        *o = (top * (1.0 - fy) + bot * fy).round() as u8;
    }
    Some(out)
}

/// A kit's pieces, which [`Kit::mesh`] makes east/north/up, in its local
/// frame — the inverse of [`Kit::to_en`].
#[must_use]
pub fn local_pieces(kit: &Kit, pieces: &[Piece]) -> Vec<Piece> {
    pieces
        .iter()
        .map(|piece| Piece {
            p: piece
                .p
                .iter()
                .map(|q| {
                    let l = to_local(kit, [q[0], q[1]]);
                    [l[0], l[1], q[2]]
                })
                .collect(),
            ..piece.clone()
        })
        .collect()
}

/// East/north of the kit's plane to its local frame.
fn to_local(kit: &Kit, en: [f64; 2]) -> [f64; 2] {
    let (s, c) = kit.rotation_deg.to_radians().sin_cos();
    [en[0] * c - en[1] * s, en[0] * s + en[1] * c]
}

/// Two faces meeting at more than this are a crease worth drawing; less is
/// one surface cut into triangles (or a gently leaning stage's facets).
const CREASE_DEG: f64 = 25.0;
/// Vertices closer than this are one, metres: a kit's pieces repeat a corner
/// for every face that uses it.
const WELD_M: f64 = 0.001;

/// The model's feature edges: those used by one triangle only, or shared by
/// faces turned more than [`CREASE_DEG`] from each other. The lines a
/// photograph shows — corners, ledges, an opening's outline — and not the
/// diagonals that cut a flat face into triangles.
fn feature_edges(pieces: &[Piece]) -> Vec<([f64; 3], [f64; 3])> {
    let mut ids: HashMap<[i64; 3], u32> = HashMap::new();
    let mut at: Vec<[f64; 3]> = Vec::new();
    let mut weld = |q: [f64; 3]| {
        let key = q.map(|x| (x / WELD_M).round() as i64);
        *ids.entry(key).or_insert_with(|| {
            at.push(q);
            (at.len() - 1) as u32
        })
    };
    let mut normals: HashMap<(u32, u32), Vec<[f64; 3]>> = HashMap::new();
    let mut order: Vec<(u32, u32)> = Vec::new();
    for piece in pieces {
        for t in &piece.t {
            let q = t.map(|i| piece.p[i as usize]);
            let n = cross(sub(q[1], q[0]), sub(q[2], q[0]));
            if dot(n, n).sqrt() < 1e-12 {
                continue;
            }
            let n = unit(n);
            let v = q.map(&mut weld);
            for (i, k) in [(0, 1), (1, 2), (2, 0)] {
                if v[i] == v[k] {
                    continue;
                }
                let key = (v[i].min(v[k]), v[i].max(v[k]));
                normals
                    .entry(key)
                    .or_insert_with(|| {
                        order.push(key);
                        Vec::new()
                    })
                    .push(n);
            }
        }
    }
    let crease = CREASE_DEG.to_radians().cos();
    order
        .into_iter()
        .filter(|key| {
            let ns = &normals[key];
            ns.len() == 1
                || ns
                    .iter()
                    .enumerate()
                    .any(|(i, a)| ns[i + 1..].iter().any(|b| dot(*a, *b) < crease))
        })
        .map(|(a, b)| (at[a as usize], at[b as usize]))
        .collect()
}

/// Nothing nearer the camera than this is drawn, metres.
const NEAR_M: f64 = 0.05;
/// An edge is seen where it is no deeper than the nearest surface plus this
/// share of its depth plus [`DEPTH_SLACK_M`]: the surface it lies on is
/// sampled at pixel centres, not on the edge.
const DEPTH_SLACK: f64 = 0.01;
const DEPTH_SLACK_M: f64 = 0.05;
/// A Sobel magnitude, on 0–255 luma, at or over this is a strong image edge:
/// a step of about 40 grey levels.
const STRONG_EDGE: f64 = 160.0;
/// How near a model edge a strong image edge must lie to count as matched.
const MATCH_PX: i32 = 3;
const EDGE_RGB: [u8; 3] = [255, 0, 0];

/// The kit's visible feature edges drawn over the photograph in red, two
/// pixels wide, and a crude figure of the fit, 0 to 1: the harmonic mean of
/// the share of the photograph's strong edges where the model stands (its
/// silhouette, widened by [`MATCH_PX`]) within [`MATCH_PX`] of a drawn edge,
/// and the share of drawn edge within [`MATCH_PX`] of a strong edge. Counted
/// over the model's own area because over the whole picture the trees, sky
/// and neighbours would swamp it and it would hardly move as the model
/// improves.
///
/// `pieces` are in the kit's local frame ([`local_pieces`]). Hidden edges are
/// not drawn at all: the back of a tower with its arches would cross the very
/// face being compared, and the figure counts only what is drawn.
#[must_use]
pub fn overlay(pose: &Pose, photo: &RgbImage, pieces: &[Piece]) -> (RgbImage, f64) {
    let (w, h) = (photo.width() as usize, photo.height() as usize);
    let mut depth = vec![f64::INFINITY; w * h];
    for piece in pieces {
        for t in &piece.t {
            let cam = t.map(|i| pose.camera(piece.p[i as usize]));
            let poly = clip_near(&cam);
            for k in 1..poly.len().saturating_sub(1) {
                let s = [poly[0], poly[k], poly[k + 1]].map(|c| {
                    let q = pose.to_pixel(c);
                    [q[0], q[1], c[2]]
                });
                raster(&mut depth, w, h, s);
            }
        }
    }

    let mut drawn = vec![false; w * h];
    for (a, b) in feature_edges(pieces) {
        let (mut ca, mut cb) = (pose.camera(a), pose.camera(b));
        if ca[2] < NEAR_M && cb[2] < NEAR_M {
            continue;
        }
        if ca[2] < NEAR_M {
            ca = lerp(ca, cb, (NEAR_M - ca[2]) / (cb[2] - ca[2]));
        } else if cb[2] < NEAR_M {
            cb = lerp(cb, ca, (NEAR_M - cb[2]) / (ca[2] - cb[2]));
        }
        let (pa, pb) = (pose.to_pixel(ca), pose.to_pixel(cb));
        let steps =
            (((pb[0] - pa[0]).hypot(pb[1] - pa[1]) / 0.5).ceil() as usize).clamp(1, 100_000);
        for i in 0..=steps {
            let c = lerp(ca, cb, i as f64 / steps as f64);
            let q = pose.to_pixel(c);
            if q[0] < 0.0 || q[1] < 0.0 || q[0] >= w as f64 || q[1] >= h as f64 {
                continue;
            }
            let (x, y) = (q[0] as usize, q[1] as usize);
            let slack = c[2] * DEPTH_SLACK + DEPTH_SLACK_M;
            let seen = neighbours(x, y, w, h, 1).any(|k| c[2] <= depth[k] + slack);
            if seen {
                drawn[y * w + x] = true;
            }
        }
    }

    // The fit, on the photograph as it was.
    let luma: Vec<f64> = photo
        .pixels()
        .map(|p| 0.299 * f64::from(p.0[0]) + 0.587 * f64::from(p.0[1]) + 0.114 * f64::from(p.0[2]))
        .collect();
    let covered: Vec<bool> = depth.iter().map(|d| d.is_finite()).collect();
    let area = dilate(&covered, w, h, MATCH_PX);
    let near = dilate(&drawn, w, h, MATCH_PX);
    let mut edge = vec![false; w * h];
    let (mut strong, mut matched) = (0usize, 0usize);
    for y in 1..h.saturating_sub(1) {
        for x in 1..w.saturating_sub(1) {
            let k = y * w + x;
            let l = |dx: isize, dy: isize| {
                luma[(y as isize + dy) as usize * w + (x as isize + dx) as usize]
            };
            let gx = l(1, -1) + 2.0 * l(1, 0) + l(1, 1) - l(-1, -1) - 2.0 * l(-1, 0) - l(-1, 1);
            let gy = l(-1, 1) + 2.0 * l(0, 1) + l(1, 1) - l(-1, -1) - 2.0 * l(0, -1) - l(1, -1);
            edge[k] = gx.hypot(gy) >= STRONG_EDGE;
            if edge[k] && area[k] {
                strong += 1;
                matched += usize::from(near[k]);
            }
        }
    }
    // **Both ways round.** The share of the photograph's edges a model edge
    // lands on rewards drawing lines everywhere: a pose kilometres off drew
    // every arch front and back over the tower and scored 86% (2026-11-06).
    // So it is weighed against the share of the model's drawn edge that lands
    // on an edge of the photograph — the harmonic mean, which either half
    // being poor pulls down.
    let edge_near = dilate(&edge, w, h, MATCH_PX);
    let drawn_n = drawn.iter().filter(|d| **d).count();
    let drawn_on = drawn
        .iter()
        .zip(&edge_near)
        .filter(|(d, e)| **d && **e)
        .count();
    let recall = if strong == 0 {
        0.0
    } else {
        matched as f64 / strong as f64
    };
    let precision = if drawn_n == 0 {
        0.0
    } else {
        drawn_on as f64 / drawn_n as f64
    };
    let fit = if recall + precision == 0.0 {
        0.0
    } else {
        2.0 * recall * precision / (recall + precision)
    };

    let mut out = photo.clone();
    for y in 0..h {
        for x in 0..w {
            if !drawn[y * w + x] {
                continue;
            }
            for (dx, dy) in [(0, 0), (1, 0), (0, 1), (1, 1)] {
                let (px, py) = (x + dx, y + dy);
                if px < w && py < h {
                    out.put_pixel(px as u32, py as u32, Rgb(EDGE_RGB));
                }
            }
        }
    }
    (out, fit)
}

/// A polygon in camera coordinates cut to the part in front of [`NEAR_M`].
fn clip_near(poly: &[[f64; 3]]) -> Vec<[f64; 3]> {
    let mut out = Vec::with_capacity(poly.len() + 1);
    for i in 0..poly.len() {
        let (a, b) = (poly[i], poly[(i + 1) % poly.len()]);
        let (ina, inb) = (a[2] >= NEAR_M, b[2] >= NEAR_M);
        if ina {
            out.push(a);
        }
        if ina != inb {
            out.push(lerp(a, b, (NEAR_M - a[2]) / (b[2] - a[2])));
        }
    }
    out
}

/// One triangle `[u, v, depth]` into the depth buffer, nearest kept; depth
/// interpolated as its reciprocal, which is what is linear on the picture.
fn raster(depth: &mut [f64], w: usize, h: usize, s: [[f64; 3]; 3]) {
    let edge = |a: [f64; 3], b: [f64; 3], p: [f64; 2]| {
        (b[0] - a[0]) * (p[1] - a[1]) - (b[1] - a[1]) * (p[0] - a[0])
    };
    let area = edge(s[0], s[1], [s[2][0], s[2][1]]);
    if area.abs() < 1e-12 {
        return;
    }
    let span = |k: usize, n: usize| {
        let lo = s.iter().map(|q| q[k]).fold(f64::INFINITY, f64::min);
        let hi = s.iter().map(|q| q[k]).fold(f64::NEG_INFINITY, f64::max);
        let clamp = |x: f64| x.clamp(0.0, n as f64) as usize;
        (clamp((lo - 0.5).floor()), clamp((hi + 0.5).ceil()))
    };
    let ((x0, x1), (y0, y1)) = (span(0, w), span(1, h));
    for y in y0..y1 {
        for x in x0..x1 {
            let p = [x as f64 + 0.5, y as f64 + 0.5];
            let l0 = edge(s[1], s[2], p) / area;
            let l1 = edge(s[2], s[0], p) / area;
            let l2 = 1.0 - l0 - l1;
            if l0 < -1e-9 || l1 < -1e-9 || l2 < -1e-9 {
                continue;
            }
            let z = 1.0 / (l0 / s[0][2] + l1 / s[1][2] + l2 / s[2][2]);
            let k = y * w + x;
            if z < depth[k] {
                depth[k] = z;
            }
        }
    }
}

/// Indices of the pixels within `r` (a square) of `(x, y)`, on the picture.
fn neighbours(x: usize, y: usize, w: usize, h: usize, r: usize) -> impl Iterator<Item = usize> {
    let (x0, x1) = (x.saturating_sub(r), (x + r).min(w - 1));
    let (y0, y1) = (y.saturating_sub(r), (y + r).min(h - 1));
    (y0..=y1).flat_map(move |yy| (x0..=x1).map(move |xx| yy * w + xx))
}

/// A mask widened by a disc of radius `r` pixels.
fn dilate(mask: &[bool], w: usize, h: usize, r: i32) -> Vec<bool> {
    let disc: Vec<(i32, i32)> = (-r..=r)
        .flat_map(|dy| (-r..=r).map(move |dx| (dx, dy)))
        .filter(|(dx, dy)| dx * dx + dy * dy <= r * r)
        .collect();
    let mut out = vec![false; mask.len()];
    for y in 0..h {
        for x in 0..w {
            if !mask[y * w + x] {
                continue;
            }
            for (dx, dy) in &disc {
                let (px, py) = (x as i32 + dx, y as i32 + dy);
                if px >= 0 && py >= 0 && (px as usize) < w && (py as usize) < h {
                    out[py as usize * w + px as usize] = true;
                }
            }
        }
    }
    out
}

// --- The commands -----------------------------------------------------------

/// `atlas landmark pose`: solve where `photo` was taken from, given points
/// `"u,v=x,y,z"` (a pixel and its local place), and optionally the `(lat, lng)`
/// it was taken near. Writes `<out>/pose.json` and `<out>/pose.md` (the pose,
/// and each point's residual: a large one names a point mis-clicked or
/// mis-measured). Answers a short summary.
pub fn pose_cmd(
    kit_path: &Path,
    photo: &Path,
    points: &[String],
    from_lat_lng: Option<(f64, f64)>,
    out: &Path,
) -> Result<String, String> {
    let kit = read_kit(kit_path)?;
    let size = image::image_dimensions(photo).map_err(|e| format!("{}: {e}", photo.display()))?;
    let pairs: Vec<([f64; 2], [f64; 3])> = points
        .iter()
        .map(|s| {
            let (px, p) = s
                .split_once('=')
                .ok_or_else(|| format!("point {s:?}: write it u,v=x,y,z"))?;
            let px = numbers(px, 2, s)?;
            let p = numbers(p, 3, s)?;
            Ok(([px[0], px[1]], [p[0], p[1], p[2]]))
        })
        .collect::<Result<_, String>>()?;
    let from = from_lat_lng.map(|(lat, lng)| {
        let plane = Plane::at(kit.origin.0, kit.origin.1);
        to_local(&kit, plane.to_en(lat, lng))
    });
    // A refused solve must not leave an earlier one behind for `overlay` and
    // `rectify` to read as this photograph's.
    for stale in ["pose.json", "pose.md"] {
        let _ = std::fs::remove_file(out.join(stale));
    }
    let (mut pose, res) = solve_pose(&pairs, size, from)?;
    pose.photo = file_name(photo);
    pose.kit = file_name(kit_path);
    let rms = (res.iter().map(|r| r * r).sum::<f64>() / res.len() as f64).sqrt();
    let hfov = 2.0
        * (f64::from(pose.width) / 2.0 / pose.focal_px)
            .atan()
            .to_degrees();

    let mut md = format!(
        "# Pose of {} for {}\n\n\
         camera at x {:.2}, y {:.2}, z {:.2} m (local); looking {:.1}° clockwise from local +y, \
         pitch {:+.1}°, roll {:+.1}°; focal {:.0} px on a {}×{} picture ({hfov:.1}° across)\n\n\
         residual: {rms:.2} px RMS over {} points\n",
        pose.photo,
        pose.kit,
        pose.x,
        pose.y,
        pose.z,
        pose.yaw_deg,
        pose.pitch_deg,
        pose.roll_deg,
        pose.focal_px,
        pose.width,
        pose.height,
        res.len()
    );
    if let Some(f) = from {
        let _ = writeln!(
            md,
            "\n{:.1} m from where the photograph says it was taken (local {:.1}, {:.1})",
            (pose.x - f[0]).hypot(pose.y - f[1]),
            f[0],
            f[1]
        );
    }
    md.push_str(
        "\n| # | pixel | local x, y, z | lands at | residual px |\n|---|---|---|---|---|\n",
    );
    for (i, ((px, p), r)) in pairs.iter().zip(&res).enumerate() {
        let at = pose.project(*p).map_or("behind".to_string(), |q| {
            format!("{:.1}, {:.1}", q[0], q[1])
        });
        let _ = writeln!(
            md,
            "| {} | {:.1}, {:.1} | {:.2}, {:.2}, {:.2} | {at} | {r:.2} |",
            i + 1,
            px[0],
            px[1],
            p[0],
            p[1],
            p[2]
        );
    }
    md.push_str(
        "\nA residual of a few pixels is clicking; one well over the rest names a point \
         clicked on the wrong feature or placed at the wrong height.\n",
    );
    std::fs::create_dir_all(out).map_err(|e| e.to_string())?;
    let json = serde_json::to_string_pretty(&pose).map_err(|e| e.to_string())?;
    std::fs::write(out.join("pose.json"), json).map_err(|e| e.to_string())?;
    std::fs::write(out.join("pose.md"), &md).map_err(|e| e.to_string())?;
    let worst = res
        .iter()
        .enumerate()
        .max_by(|a, b| a.1.total_cmp(b.1))
        .map_or(0, |(i, _)| i + 1);
    Ok(format!(
        "camera at {:.1}, {:.1}, {:.1} m local, yaw {:.1}°, pitch {:+.1}°, {hfov:.0}° across; \
         {rms:.2} px RMS, worst point {worst} at {:.2} px",
        pose.x,
        pose.y,
        pose.z,
        pose.yaw_deg,
        pose.pitch_deg,
        res.iter().copied().fold(0.0, f64::max)
    ))
}

/// `atlas landmark measure`: each pixel `"u,v"` on the wall `"x1,y1:x2,y2"`
/// (local), as metres along the wall from its first point and height, and the
/// distance from each to the one before.
pub fn measure_cmd(pose: &Path, wall: &str, pixels: &[String]) -> Result<String, String> {
    let pose = read_pose(pose)?;
    let wall = parse_wall(wall)?;
    let px: Vec<[f64; 2]> = pixels
        .iter()
        .map(|s| numbers(s, 2, s).map(|v| [v[0], v[1]]))
        .collect::<Result<_, String>>()?;
    let hits = measure_on_wall(&pose, wall, &px);
    let mut text = String::from(
        "| # | pixel | along m | height m | from the one before m |\n|---|---|---|---|---|\n",
    );
    let mut last: Option<(f64, f64)> = None;
    for (i, (q, hit)) in px.iter().zip(&hits).enumerate() {
        match hit {
            Some((s, h)) => {
                let step = last.map_or(String::new(), |(s0, h0)| {
                    format!("{:.2}", (s - s0).hypot(h - h0))
                });
                let _ = writeln!(
                    text,
                    "| {} | {:.1}, {:.1} | {s:.2} | {h:.2} | {step} |",
                    i + 1,
                    q[0],
                    q[1]
                );
                last = Some((*s, *h));
            }
            None => {
                let _ = writeln!(
                    text,
                    "| {} | {:.1}, {:.1} | — | — | the ray misses the wall |",
                    i + 1,
                    q[0],
                    q[1]
                );
                last = None;
            }
        }
    }
    Ok(text)
}

/// `atlas landmark rectify`: the wall `"x1,y1:x2,y2"` from `bottom` to `top`
/// metres, straight on at `cm_per_px`. Writes `<out>/elevation.png` and
/// `<out>/elevation.md`, which says how to read it.
pub fn rectify_cmd(
    pose: &Path,
    photo: &Path,
    wall: &str,
    bottom: f64,
    top: f64,
    cm_per_px: f64,
    out: &Path,
) -> Result<String, String> {
    let pose = read_pose(pose)?;
    let picture = read_photo(&pose, photo)?;
    let (a, b) = parse_wall(wall)?;
    if top <= bottom || cm_per_px <= 0.0 {
        return Err("give a top above the bottom and a positive scale".into());
    }
    let m = cm_per_px / 100.0;
    let len = (b[0] - a[0]).hypot(b[1] - a[1]);
    // A typo in the scale must not ask for a gigapixel picture.
    if len / m > 20_000.0 || (top - bottom) / m > 20_000.0 {
        return Err(format!(
            "{len:.1} × {:.1} m at {cm_per_px} cm a pixel is over 20000 pixels a side",
            top - bottom
        ));
    }
    let img = rectify(&pose, &picture, (a, b), bottom, top, m);
    std::fs::create_dir_all(out).map_err(|e| e.to_string())?;
    img.save(out.join("elevation.png"))
        .map_err(|e| e.to_string())?;
    let md = format!(
        "# Elevation of the wall {:.2}, {:.2} → {:.2}, {:.2} (local) from {}\n\n\
         {} × {} pixels at {cm_per_px} cm a pixel. The left edge is the first wall point, the \
         right edge {len:.2} m along the wall.\n\n\
         - metres along the wall = column × {m}\n\
         - height above ground, m = {bottom} + ({} − row) × {m}\n\n\
         A light line every metre and a yellow one every five, counted from the first point \
         and from the ground. Mid-grey is wall the photograph does not show. Anything \
         standing proud of the wall is drawn where its ray meets the wall's plane, so \
         measure only what lies in it.\n",
        a[0],
        a[1],
        b[0],
        b[1],
        pose.photo,
        img.width(),
        img.height(),
        img.height() - 1
    );
    std::fs::write(out.join("elevation.md"), md).map_err(|e| e.to_string())?;
    Ok(format!(
        "elevation.png: {} × {} px, {len:.2} m along by {:.2} m high at {cm_per_px} cm a pixel",
        img.width(),
        img.height(),
        top - bottom
    ))
}

/// `atlas landmark overlay`: the kit's visible edges over the photograph,
/// written to `<out>/overlay.png`. Answers the fit figure.
pub fn overlay_cmd(
    kit_path: &Path,
    pose: &Path,
    photo: &Path,
    out: &Path,
) -> Result<String, String> {
    let kit = read_kit(kit_path)?;
    let pose = read_pose(pose)?;
    if !pose.kit.is_empty() && pose.kit != file_name(kit_path) {
        return Err(format!(
            "the pose was solved for the kit {}, not {}",
            pose.kit,
            file_name(kit_path)
        ));
    }
    let picture = read_photo(&pose, photo)?;
    let pieces = local_pieces(&kit, &kit.mesh()?);
    let (img, fit) = overlay(&pose, &picture, &pieces);
    std::fs::create_dir_all(out).map_err(|e| e.to_string())?;
    img.save(out.join("overlay.png"))
        .map_err(|e| e.to_string())?;
    Ok(format!(
        "overlay.png: fit {:.0}% (100% is every strong edge of the photograph where the \
         model stands met by a model edge within {MATCH_PX} px, and every model edge on one)",
        fit * 100.0
    ))
}

/// A magnified crop of a photograph round one pixel, for placing a point to
/// the pixel: `zoom.png`, `half` source pixels either side of `at`, each drawn
/// `times` wide, with a line every ten source pixels and a cross on `at`.
///
/// **Why it exists.** A pose is only as good as its points, and on a 1280 px
/// thumbnail a ledge's corner is a smudge: the first real Martinitoren try,
/// its points placed on the whole picture, solved to 38 px of disagreement
/// and an overlay that missed the tower (2026-11-06). Answers how to read a
/// zoomed pixel back.
pub fn zoom_cmd(
    photo: &Path,
    at: &str,
    half: u32,
    times: u32,
    out: &Path,
) -> Result<String, String> {
    let at = numbers(at, 2, at)?;
    let img = image::open(photo)
        .map_err(|e| format!("{}: {e}", photo.display()))?
        .to_rgb8();
    let times = times.clamp(2, 16);
    let (u0, v0) = (
        (at[0].round() as i64 - i64::from(half)).max(0),
        (at[1].round() as i64 - i64::from(half)).max(0),
    );
    let (u1, v1) = (
        (at[0].round() as i64 + i64::from(half)).min(i64::from(img.width())),
        (at[1].round() as i64 + i64::from(half)).min(i64::from(img.height())),
    );
    if u1 <= u0 || v1 <= v0 {
        return Err(format!(
            "{at:?} is outside the {}×{} picture",
            img.width(),
            img.height()
        ));
    }
    let (w, h) = ((u1 - u0) as u32 * times, (v1 - v0) as u32 * times);
    let mut z = RgbImage::new(w, h);
    for y in 0..h {
        for x in 0..w {
            let (u, v) = (u0 as u32 + x / times, v0 as u32 + y / times);
            let mut p = *img.get_pixel(u, v);
            // A line every ten source pixels, dark on light and light on dark.
            if (x % times == 0 && u % 10 == 0) || (y % times == 0 && v % 10 == 0) {
                let l = (u32::from(p[0]) + u32::from(p[1]) + u32::from(p[2])) / 3;
                p = if l > 128 {
                    Rgb([0, 0, 0])
                } else {
                    Rgb([255, 255, 255])
                };
            }
            z.put_pixel(x, y, p);
        }
    }
    let (cx, cy) = (
        ((at[0] - u0 as f64) * f64::from(times)) as i64,
        ((at[1] - v0 as f64) * f64::from(times)) as i64,
    );
    for d in -12..=12 {
        for (x, y) in [(cx + d, cy), (cx, cy + d)] {
            if x >= 0 && y >= 0 && (x as u32) < w && (y as u32) < h {
                z.put_pixel(x as u32, y as u32, Rgb(EDGE_RGB));
            }
        }
    }
    std::fs::create_dir_all(out).map_err(|e| e.to_string())?;
    z.save(out.join("zoom.png")).map_err(|e| e.to_string())?;
    Ok(format!(
        "zoom.png: source pixels {u0}–{u1} across, {v0}–{v1} down, each {times} px wide; a \
         line every 10 source pixels (at multiples of 10); the red cross is {:.1}, {:.1}. A \
         point at zoom.png (col, row) is the photograph's ({u0} + col / {times}, {v0} + row / \
         {times}).",
        at[0], at[1]
    ))
}

fn read_kit(path: &Path) -> Result<Kit, String> {
    let text = std::fs::read_to_string(path).map_err(|e| format!("{}: {e}", path.display()))?;
    Kit::parse(&text)
}

fn read_pose(path: &Path) -> Result<Pose, String> {
    let text = std::fs::read_to_string(path).map_err(|e| format!("{}: {e}", path.display()))?;
    serde_json::from_str(&text).map_err(|e| format!("{}: {e}", path.display()))
}

/// The photograph a pose was solved for, refused when it is another one.
fn read_photo(pose: &Pose, path: &Path) -> Result<RgbImage, String> {
    if !pose.photo.is_empty() && pose.photo != file_name(path) {
        return Err(format!(
            "the pose was solved for the photograph {}, not {}",
            pose.photo,
            file_name(path)
        ));
    }
    let img = image::open(path)
        .map_err(|e| format!("{}: {e}", path.display()))?
        .to_rgb8();
    if (img.width(), img.height()) != (pose.width, pose.height) {
        return Err(format!(
            "{} is {}×{}; the pose was solved on {}×{}",
            path.display(),
            img.width(),
            img.height(),
            pose.width,
            pose.height
        ));
    }
    Ok(img)
}

fn file_name(path: &Path) -> String {
    path.file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default()
}

/// `n` comma-separated numbers out of `s`; `whole` names it in the error.
fn numbers(s: &str, n: usize, whole: &str) -> Result<Vec<f64>, String> {
    let v: Vec<f64> = s
        .split(',')
        .map(|x| x.trim().parse::<f64>())
        .collect::<Result<_, _>>()
        .map_err(|e| format!("{whole:?}: {e}"))?;
    if v.len() != n {
        return Err(format!("{whole:?}: expected {n} numbers, got {}", v.len()));
    }
    Ok(v)
}

/// `"x1,y1:x2,y2"`.
fn parse_wall(s: &str) -> Result<([f64; 2], [f64; 2]), String> {
    let (a, b) = s
        .split_once(':')
        .ok_or_else(|| format!("wall {s:?}: write it x1,y1:x2,y2"))?;
    let (a, b) = (numbers(a, 2, s)?, numbers(b, 2, s)?);
    Ok(([a[0], a[1]], [b[0], b[1]]))
}

fn add(a: [f64; 3], b: [f64; 3]) -> [f64; 3] {
    [a[0] + b[0], a[1] + b[1], a[2] + b[2]]
}
fn sub(a: [f64; 3], b: [f64; 3]) -> [f64; 3] {
    [a[0] - b[0], a[1] - b[1], a[2] - b[2]]
}
fn scale(a: [f64; 3], k: f64) -> [f64; 3] {
    [a[0] * k, a[1] * k, a[2] * k]
}
fn dot(a: [f64; 3], b: [f64; 3]) -> f64 {
    a[0] * b[0] + a[1] * b[1] + a[2] * b[2]
}
fn cross(a: [f64; 3], b: [f64; 3]) -> [f64; 3] {
    [
        a[1] * b[2] - a[2] * b[1],
        a[2] * b[0] - a[0] * b[2],
        a[0] * b[1] - a[1] * b[0],
    ]
}
fn unit(a: [f64; 3]) -> [f64; 3] {
    scale(a, 1.0 / dot(a, a).sqrt().max(1e-300))
}
fn lerp(a: [f64; 3], b: [f64; 3], t: f64) -> [f64; 3] {
    add(a, scale(sub(b, a), t))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A camera 48 m south-east of a 30 m tower, looking up at it.
    fn truth() -> Pose {
        Pose {
            x: 25.0,
            y: -40.0,
            z: 1.6,
            yaw_deg: -28.0,
            pitch_deg: 14.0,
            roll_deg: 1.5,
            focal_px: 1000.0,
            width: 1280,
            height: 960,
            photo: String::new(),
            kit: String::new(),
        }
    }

    fn seen(pose: &Pose, points: &[[f64; 3]]) -> Vec<([f64; 2], [f64; 3])> {
        points
            .iter()
            .map(|p| (pose.project(*p).unwrap(), *p))
            .collect()
    }

    #[test]
    fn a_projected_point_lies_on_its_pixels_ray() {
        let pose = truth();
        for p in [[0.0, 0.0, 10.0], [4.0, -4.0, 1.0], [-4.0, 4.0, 30.0]] {
            let q = pose.project(p).unwrap();
            let (o, d) = pose.ray(q);
            let t = dot(sub(p, o), d);
            let off = sub(add(o, scale(d, t)), p);
            assert!(
                dot(off, off).sqrt() < 1e-9,
                "{p:?} misses its ray by {off:?}"
            );
            assert!((dot(d, d) - 1.0).abs() < 1e-12);
        }
        // Behind the camera is nowhere in the picture; up is up the picture.
        assert!(pose.project([40.0, -70.0, 1.6]).is_none());
        let low = pose.project([0.0, 0.0, 1.0]).unwrap();
        let high = pose.project([0.0, 0.0, 20.0]).unwrap();
        assert!(high[1] < low[1]);
    }

    #[test]
    fn six_points_give_back_the_camera() {
        let pose = truth();
        let points = seen(
            &pose,
            &[
                [-4.0, -4.0, 1.0],
                [4.0, -4.0, 1.0],
                [4.0, 4.0, 3.0],
                [4.0, -4.0, 20.0],
                [-4.0, -4.0, 25.0],
                [4.0, 4.0, 30.0],
            ],
        );
        let (got, res) = solve_pose(&points, (1280, 960), None).unwrap();
        assert!(
            (got.x - pose.x).hypot(got.y - pose.y).hypot(got.z - pose.z) < 0.05,
            "{got:?}"
        );
        for (a, b) in [
            (got.yaw_deg, pose.yaw_deg),
            (got.pitch_deg, pose.pitch_deg),
            (got.roll_deg, pose.roll_deg),
        ] {
            let d = (a - b + 180.0).rem_euclid(360.0) - 180.0;
            assert!(d.abs() < 0.2, "{got:?}");
        }
        assert!((got.focal_px - pose.focal_px).abs() < 5.0, "{got:?}");
        assert!(res.iter().all(|r| *r < 0.01), "{res:?}");
    }

    #[test]
    fn four_noisy_points_still_converge() {
        let pose = truth();
        let mut points = seen(
            &pose,
            &[
                [-4.0, -4.0, 1.0],
                [4.0, -4.0, 1.0],
                [4.0, 4.0, 30.0],
                [-4.0, -4.0, 25.0],
            ],
        );
        for (i, (px, _)) in points.iter_mut().enumerate() {
            px[0] += [0.4, -0.3, 0.2, -0.5][i];
            px[1] += [-0.2, 0.5, -0.4, 0.3][i];
        }
        let near = solve_pose(&points, (1280, 960), Some([20.0, -35.0])).unwrap();
        let blind = solve_pose(&points, (1280, 960), None).unwrap();
        for (got, res) in [near, blind] {
            assert!(res.iter().all(|r| *r < 1.0), "{res:?}");
            assert!(
                (got.x - pose.x).hypot(got.y - pose.y) < 3.0,
                "{got:?} is far from the camera"
            );
        }
    }

    #[test]
    fn too_few_points_are_refused() {
        let pose = truth();
        let points = seen(
            &pose,
            &[[-4.0, -4.0, 1.0], [4.0, -4.0, 1.0], [4.0, 4.0, 30.0]],
        );
        assert!(solve_pose(&points, (1280, 960), None)
            .unwrap_err()
            .contains("four points"));
    }

    #[test]
    fn a_point_on_a_wall_measures_back() {
        let pose = truth();
        // The tower's south face, west corner to east corner.
        let wall = ([-4.0, -4.0], [4.0, -4.0]);
        let p = [1.3, -4.0, 17.25];
        let px = pose.project(p).unwrap();
        let got = measure_on_wall(&pose, wall, &[px]);
        let (along, height) = got[0].unwrap();
        assert!(
            (along - 5.3).abs() < 0.01 && (height - 17.25).abs() < 0.01,
            "{got:?}"
        );
        // A wall behind the camera is met by no ray in the picture.
        let behind = ([-100.0, -60.0], [100.0, -60.0]);
        assert_eq!(measure_on_wall(&pose, behind, &[px]), vec![None]);
    }

    /// A photograph drawn from a known pose of a white wall with a red square
    /// on it; the elevation puts the square back at its metres.
    #[test]
    fn rectify_puts_a_square_back_at_its_metres() {
        let pose = Pose {
            x: 5.0,
            y: -12.0,
            z: 1.6,
            yaw_deg: 10.0,
            pitch_deg: 8.0,
            roll_deg: 2.0,
            focal_px: 600.0,
            width: 640,
            height: 480,
            photo: String::new(),
            kit: String::new(),
        };
        let wall = ([0.0, 0.0], [10.0, 0.0]);
        let mut photo = RgbImage::new(640, 480);
        for v in 0..480 {
            for u in 0..640 {
                let px = [f64::from(u) + 0.5, f64::from(v) + 0.5];
                let colour = match measure_on_wall(&pose, wall, &[px])[0] {
                    Some((s, h)) if (3.2..4.6).contains(&s) && (2.3..3.7).contains(&h) => {
                        [220, 20, 20]
                    }
                    Some(_) => [200, 200, 210],
                    None => [100, 150, 250],
                };
                photo.put_pixel(u, v, Rgb(colour));
            }
        }
        let m = 0.05;
        let img = rectify(&pose, &photo, wall, 0.0, 8.0, m);
        assert_eq!((img.width(), img.height()), (201, 161));
        let at = |s: f64, h: f64| {
            let c = (s / m).round() as u32;
            let r = img.height() - 1 - (h / m).round() as u32;
            img.get_pixel(c, r).0
        };
        let red = |p: [u8; 3]| p[0] > 180 && p[1] < 80;
        for (s, h) in [(3.5, 2.6), (4.4, 3.6), (3.3, 2.4)] {
            assert!(red(at(s, h)), "{s}, {h}: {:?}", at(s, h));
        }
        for (s, h) in [(2.9, 2.6), (4.8, 2.6), (3.5, 3.9), (3.5, 2.1)] {
            assert!(!red(at(s, h)), "{s}, {h}: {:?}", at(s, h));
        }
    }

    /// A box's feature edges are its twelve: the diagonals across its faces
    /// are not creases. Through a turned frame, it comes back square.
    #[test]
    fn a_box_has_twelve_feature_edges() {
        let kit = Kit::parse(
            r#"{"name": "t", "pand": "0014100010924795", "origin": [53.2, 6.5],
                "rotation_deg": 30,
                "materials": {"b": {"rgb": [1, 2, 3]}},
                "parts": [{"name": "p", "material": "b", "shape": {"rect": {"w": 4, "d": 2}},
                           "rings": [[0, 1], [3, 1]]}]}"#,
        )
        .unwrap();
        let pieces = local_pieces(&kit, &kit.mesh().unwrap());
        for q in &pieces[0].p {
            assert!(
                (q[0].abs() - 2.0).abs() < 1e-9 && (q[1].abs() - 1.0).abs() < 1e-9,
                "{q:?}"
            );
        }
        let edges = feature_edges(&pieces);
        assert_eq!(edges.len(), 12, "{edges:?}");
        for (a, b) in edges {
            let d = sub(b, a);
            // Each runs along one axis.
            assert_eq!(
                d.iter().filter(|x| x.abs() > 1e-9).count(),
                1,
                "{a:?} → {b:?}"
            );
        }
    }
}
