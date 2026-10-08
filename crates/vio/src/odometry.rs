//! Where the camera is, up to scale, from tracked corners and a known rotation.
//!
//! **The rotation comes from the gyroscope, not from the image.** That is the
//! decision the rest of this file rests on. With the camera's orientation known
//! every frame, both of the classic problems become linear and small:
//!
//! * **starting** (two views): the baseline `t` between the first frame and the
//!   current one is perpendicular to `a × b` for every corner seen in both,
//!   with `a`, `b` its bearings in world axes — so `t` is the smallest
//!   eigenvector of `Σ (a×b)(a×b)ᵀ`, a 3×3 problem, with no essential matrix,
//!   no RANSAC over five-point solutions and no rotation ambiguity;
//! * **following** (one view): the camera centre `c` minimising the distance
//!   from each landmark to its viewing ray solves
//!   `Σ(I − bbᵀ) c = Σ(I − bbᵀ) L` — 3×3 again.
//!
//! The gyroscope's rotation is then **refined** against the landmarks every
//! frame ([`refine`]): position and a small rotation correction solved
//! together, with the gyroscope as a prior. Without it a gyroscope bias of a
//! few thousandths of a rad/s tilts the estimate a degree in ten seconds and
//! the bench's position error grew past five centimetres; with it, the
//! landmarks hold the orientation still for as long as they are in view, and
//! the correction is handed back to the attitude filter.
//!
//! Positions are in **visual units**: the first baseline is defined as length
//! 1. [`crate::scale`] turns them into metres with the accelerometer.

use std::collections::HashMap;

use glam::{DMat3, DQuat, DVec3, Vec2};

use crate::geom::{outer, smallest_eigenvector, solve, triangulate, Camera};

/// One corner seen in one frame: who it is and where.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Observation {
    pub id: u64,
    pub px: Vec2,
}

/// Parallax, in radians, before the two-view start is attempted (~2°).
const START_PARALLAX: f64 = 0.035;
/// Corners in common before the two-view start is attempted.
const START_MIN_COMMON: usize = 20;
/// Landmarks needed to locate the camera from one view.
const LOCATE_MIN: usize = 8;
/// Parallax, in radians, before a new corner becomes a landmark (~1.5°).
const TRIANGULATE_PARALLAX: f64 = 0.026;
/// A landmark whose ray misses by more than this many pixels is an outlier.
const OUTLIER_PX: f64 = 3.0;

#[derive(Debug, Clone)]
struct Track {
    /// Where the camera was, and the corner's world bearing, when it was first
    /// seen after the start — what a later view triangulates against.
    first: Option<(DVec3, DVec3)>,
    landmark: Option<DVec3>,
}

/// What [`Odometry::update`] concluded this frame.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Fix {
    /// Not started: waiting for enough parallax since the reference frame.
    Starting { common: usize, parallax_deg: f64 },
    /// Started this frame, or located: the camera centre in visual units, and
    /// the rotation (world axes) the landmarks say the orientation was off by —
    /// for the caller to apply to its attitude.
    Located {
        position: DVec3,
        landmarks: usize,
        correction: DQuat,
    },
    /// Too few landmarks to say; the odometry has restarted.
    Lost,
}

/// Visual odometry up to scale.
#[derive(Debug, Default)]
pub struct Odometry {
    tracks: HashMap<u64, Track>,
    /// The two-view start's reference: world bearings at the first frame.
    reference: HashMap<u64, DVec3>,
    position: Option<DVec3>,
    /// How many times it has started — each start is a new visual scale.
    pub epoch: u32,
}

impl Odometry {
    /// Feed one frame: the camera's orientation (world from camera) and the
    /// corners seen in it.
    pub fn update(&mut self, cam: &Camera, world_from_camera: DQuat, seen: &[Observation]) -> Fix {
        let bearings: HashMap<u64, DVec3> = seen
            .iter()
            .map(|o| (o.id, world_from_camera * cam.bearing(o.px)))
            .collect();
        // Forget what is no longer seen.
        self.tracks.retain(|id, _| bearings.contains_key(id));
        self.reference.retain(|id, _| bearings.contains_key(id));

        let Some(position) = self.position else {
            return self.start(cam, &bearings);
        };
        for id in bearings.keys() {
            self.tracks.entry(*id).or_insert(Track {
                first: None,
                landmark: None,
            });
        }
        match self.locate(cam, position, &bearings) {
            Some((c, landmarks, correction)) => {
                self.position = Some(c);
                let corrected: HashMap<u64, DVec3> = bearings
                    .iter()
                    .map(|(id, b)| (*id, correction * *b))
                    .collect();
                self.extend(cam, c, &corrected);
                Fix::Located {
                    position: c,
                    landmarks,
                    correction,
                }
            }
            None => {
                self.restart(&bearings);
                Fix::Lost
            }
        }
    }

    /// Begin again from this frame as the reference.
    fn restart(&mut self, bearings: &HashMap<u64, DVec3>) {
        self.position = None;
        self.tracks.clear();
        self.reference = bearings.clone();
    }

    fn start(&mut self, cam: &Camera, bearings: &HashMap<u64, DVec3>) -> Fix {
        if self.reference.is_empty() {
            self.reference = bearings.clone();
        }
        let pairs: Vec<(u64, DVec3, DVec3)> = self
            .reference
            .iter()
            .filter_map(|(id, a)| bearings.get(id).map(|b| (*id, *a, *b)))
            .collect();
        // New corners join the reference so a start can use them later.
        for (id, b) in bearings {
            self.reference.entry(*id).or_insert(*b);
        }
        let mut angles: Vec<f64> = pairs.iter().map(|(_, a, b)| a.angle_between(*b)).collect();
        angles.sort_by(f64::total_cmp);
        let parallax = angles.get(angles.len() / 2).copied().unwrap_or(0.0);
        if pairs.len() < START_MIN_COMMON || parallax < START_PARALLAX {
            return Fix::Starting {
                common: pairs.len(),
                parallax_deg: parallax.to_degrees(),
            };
        }
        let m = pairs.iter().fold(DMat3::ZERO, |m, (_, a, b)| {
            let n = a.cross(*b);
            m + outer(n, n)
        });
        let (t, _) = smallest_eigenvector(m);
        // `t` and `−t` both satisfy the constraint; the right one puts the
        // points in front of both cameras.
        let ahead = |t: DVec3| {
            pairs
                .iter()
                .filter(|(_, a, b)| triangulate(DVec3::ZERO, *a, t, *b).is_some())
                .count()
        };
        let t = if ahead(t) >= ahead(-t) { t } else { -t };
        let mut landmarks = 0;
        self.tracks.clear();
        for (id, a, b) in &pairs {
            let landmark = triangulate(DVec3::ZERO, *a, t, *b)
                .filter(|l| ray_miss_px(cam, t, *b, *l) < OUTLIER_PX);
            landmarks += usize::from(landmark.is_some());
            self.tracks.insert(
                *id,
                Track {
                    first: Some((DVec3::ZERO, *a)),
                    landmark,
                },
            );
        }
        if landmarks < LOCATE_MIN {
            // A degenerate start (the corners all on one ray, a pure rotation
            // the gyro got wrong): try again from here.
            self.reference = bearings.clone();
            return Fix::Starting {
                common: pairs.len(),
                parallax_deg: parallax.to_degrees(),
            };
        }
        for id in bearings.keys() {
            self.tracks.entry(*id).or_insert(Track {
                first: Some((t, bearings[id])),
                landmark: None,
            });
        }
        self.reference.clear();
        self.position = Some(t);
        self.epoch += 1;
        Fix::Located {
            position: t,
            landmarks,
            correction: DQuat::IDENTITY,
        }
    }

    /// The camera centre from the landmarks in view, rejecting the ones whose
    /// rays miss, twice.
    fn locate(
        &mut self,
        cam: &Camera,
        guess: DVec3,
        bearings: &HashMap<u64, DVec3>,
    ) -> Option<(DVec3, usize, DQuat)> {
        let mut used: Vec<(u64, DVec3, DVec3)> = self
            .tracks
            .iter()
            .filter_map(|(id, t)| Some((*id, t.landmark?, *bearings.get(id)?)))
            .collect();
        let mut c = guess;
        for round in 0..3 {
            if used.len() < LOCATE_MIN {
                return None;
            }
            let (mut a, mut rhs) = (DMat3::ZERO, DVec3::ZERO);
            for (_, l, b) in &used {
                let p = DMat3::IDENTITY - outer(*b, *b);
                a += p;
                rhs += p * *l;
            }
            if a.determinant().abs() < 1e-12 {
                return None;
            }
            c = a.inverse() * rhs;
            if round == 2 {
                break;
            }
            let before = used.len();
            used.retain(|(_, l, b)| ray_miss_px(cam, c, *b, *l) < OUTLIER_PX);
            if used.len() == before {
                break;
            }
        }
        // Landmarks that missed are dropped for good: a moving car, a corner
        // that slid along an edge.
        let kept: std::collections::HashSet<u64> = used.iter().map(|(id, _, _)| *id).collect();
        for (id, t) in &mut self.tracks {
            if t.landmark.is_some() && !kept.contains(id) {
                t.landmark = None;
                t.first = None;
            }
        }
        let (c, correction) = refine(cam, &used, c);
        Some((c, used.len(), correction))
    }

    /// Give the corners without a landmark one, once they have parallax.
    fn extend(&mut self, cam: &Camera, c: DVec3, bearings: &HashMap<u64, DVec3>) {
        for (id, t) in &mut self.tracks {
            let b = bearings[id];
            if t.landmark.is_some() {
                continue;
            }
            let Some((p, a)) = t.first else {
                t.first = Some((c, b));
                continue;
            };
            if a.angle_between(b) < TRIANGULATE_PARALLAX {
                continue;
            }
            if let Some(l) = triangulate(p, a, c, b) {
                if ray_miss_px(cam, c, b, l) < OUTLIER_PX && ray_miss_px(cam, p, a, l) < OUTLIER_PX
                {
                    t.landmark = Some(l);
                }
            }
        }
    }
}

/// Pixel noise the refinement assumes.
const PIXEL_SIGMA: f64 = 0.7;
/// How far the gyroscope's orientation may be off in one frame, as the
/// refinement's prior: half a degree.
const ROTATION_PRIOR_RAD: f64 = 0.0087;
/// The share of the landmarks' rotation correction applied each frame.
const CORRECTION_FRACTION: f64 = 0.1;

/// Position and a small orientation correction together, by Gauss–Newton over
/// the landmarks' angular misses, with the gyroscope's orientation as a prior.
/// The Jacobian is numerical: six parameters and a few hundred residuals.
fn refine(cam: &Camera, used: &[(u64, DVec3, DVec3)], c0: DVec3) -> (DVec3, DQuat) {
    let sigma = cam.radians_per_pixel() * PIXEL_SIGMA;
    // Each residual is measured in a tangent basis fixed by the gyroscope's
    // bearing, so the basis cannot flip under the step.
    let bases: Vec<(DVec3, DVec3)> = used
        .iter()
        .map(|(_, _, b)| {
            let t1 = b.any_orthonormal_vector();
            (t1, b.cross(t1))
        })
        .collect();
    let residuals = |x: &[f64; 6], out: &mut Vec<f64>| {
        out.clear();
        let c = DVec3::new(x[0], x[1], x[2]);
        let w = DVec3::new(x[3], x[4], x[5]);
        let rot = DQuat::from_scaled_axis(w);
        for ((_, l, b), (t1, t2)) in used.iter().zip(&bases) {
            let e = (*l - c).normalize() - rot * *b;
            out.push(e.dot(*t1) / sigma);
            out.push(e.dot(*t2) / sigma);
        }
        out.extend([x[3], x[4], x[5]].map(|v| v / ROTATION_PRIOR_RAD));
    };
    let mut x = [c0.x, c0.y, c0.z, 0.0, 0.0, 0.0];
    let (mut r, mut rh) = (Vec::new(), Vec::new());
    for _ in 0..4 {
        residuals(&x, &mut r);
        let mut jac = vec![[0.0f64; 6]; r.len()];
        for k in 0..6 {
            let h = 1e-7;
            let mut xh = x;
            xh[k] += h;
            residuals(&xh, &mut rh);
            for (i, row) in jac.iter_mut().enumerate() {
                row[k] = (rh[i] - r[i]) / h;
            }
        }
        let mut jtj = [[0.0f64; 6]; 6];
        let mut jtr = [0.0f64; 6];
        for (row, ri) in jac.iter().zip(&r) {
            for a in 0..6 {
                jtr[a] -= row[a] * ri;
                for b in 0..6 {
                    jtj[a][b] += row[a] * row[b];
                }
            }
        }
        let Some(dx) = solve(jtj, jtr) else {
            break;
        };
        for k in 0..6 {
            x[k] += dx[k];
        }
        if dx.iter().map(|v| v * v).sum::<f64>() < 1e-16 {
            break;
        }
    }
    // Only a fraction of the correction is taken, so the landmarks act on the
    // orientation as a slow integrator — removing a gyroscope's bias over
    // seconds — and not as a per-frame override. With most landmarks at one
    // depth (a façade), a two-centimetre shift and a fifth of a degree's turn
    // look almost the same, and taking the whole correction each frame traded
    // one for the other: the bench's error tripled on noise-free data.
    let correction = DQuat::from_scaled_axis(DVec3::new(x[3], x[4], x[5]) * CORRECTION_FRACTION);
    // And the position is solved again under the rotation actually taken. The
    // joint solution's position belongs to the whole correction, and pairing
    // it with a tenth of one was the other half of that error.
    let (mut a, mut rhs) = (DMat3::ZERO, DVec3::ZERO);
    for (_, l, b) in used {
        let b = correction * *b;
        let p = DMat3::IDENTITY - outer(b, b);
        a += p;
        rhs += p * *l;
    }
    let c = if a.determinant().abs() > 1e-12 {
        a.inverse() * rhs
    } else {
        DVec3::new(x[0], x[1], x[2])
    };
    (c, correction)
}

/// How far, in pixels, a landmark lies from the ray it was seen along.
fn ray_miss_px(cam: &Camera, centre: DVec3, bearing: DVec3, landmark: DVec3) -> f64 {
    let to = landmark - centre;
    if to.dot(bearing) <= 0.0 {
        return f64::INFINITY;
    }
    to.normalize().angle_between(bearing) / cam.radians_per_pixel()
}
