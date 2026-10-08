//! The camera model, and the little linear algebra the estimator needs.
//!
//! **Frames.** *Device* is Android's sensor frame: x right, y up, z out of the
//! screen, with the phone in its natural (portrait) orientation — what the
//! accelerometer and gyroscope report in. *Camera* is the computer-vision
//! frame of the raw sensor image: x right, y down, z forward, along the lens.
//! *World* is gravity-aligned: z up, x and y level, with a heading fixed by
//! wherever the tracker started (the compass aligns it later).

use glam::{DMat3, DQuat, DVec3, Vec2};

/// A pinhole camera in the pixels of the frames the tracker is given, and how
/// it sits in the phone.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Camera {
    pub fx: f64,
    pub fy: f64,
    pub cx: f64,
    pub cy: f64,
    pub width: u32,
    pub height: u32,
    /// Camera axes expressed in device axes.
    pub device_from_camera: DQuat,
}

impl Camera {
    /// A back camera whose raw image must be turned `orientation_deg`
    /// clockwise to stand upright with the phone held in portrait
    /// (`SENSOR_ORIENTATION`, 90 on almost every phone).
    ///
    /// At 0° the image's x is the device's x, its y (down) is the device's −y,
    /// and the lens looks along the device's −z (out of the back): a half turn
    /// about x. Each 90° of sensor orientation turns that a quarter about the
    /// device's z, the other way — at 90° the image's x is the device's −y and
    /// its y is the device's −x. Checked against the phone's own report in the
    /// first log, which said 90°.
    #[must_use]
    pub fn back(
        fx: f64,
        fy: f64,
        cx: f64,
        cy: f64,
        width: u32,
        height: u32,
        orientation_deg: u32,
    ) -> Self {
        let turn = DQuat::from_rotation_z(-(f64::from(orientation_deg)).to_radians());
        Self {
            fx,
            fy,
            cx,
            cy,
            width,
            height,
            device_from_camera: turn * DQuat::from_rotation_x(std::f64::consts::PI),
        }
    }

    /// The unit bearing, in camera axes, of a pixel.
    #[must_use]
    pub fn bearing(&self, px: Vec2) -> DVec3 {
        DVec3::new(
            (f64::from(px.x) - self.cx) / self.fx,
            (f64::from(px.y) - self.cy) / self.fy,
            1.0,
        )
        .normalize()
    }

    /// The pixel a camera-axes point lands on, if it is in front of the lens.
    #[must_use]
    pub fn project(&self, p: DVec3) -> Option<Vec2> {
        (p.z > 1e-6).then(|| {
            Vec2::new(
                (self.fx * p.x / p.z + self.cx) as f32,
                (self.fy * p.y / p.z + self.cy) as f32,
            )
        })
    }

    /// Whether a pixel is inside the frame.
    #[must_use]
    pub fn contains(&self, px: Vec2) -> bool {
        px.x >= 0.0 && px.y >= 0.0 && px.x < self.width as f32 && px.y < self.height as f32
    }

    /// The angle one pixel subtends, roughly — for turning pixel tolerances
    /// into angles.
    #[must_use]
    pub fn radians_per_pixel(&self) -> f64 {
        1.0 / self.fx.max(self.fy)
    }
}

/// The eigenvector of a symmetric 3×3 matrix with the smallest eigenvalue, and
/// that eigenvalue — by Jacobi rotations, which are exact enough for 3×3 and
/// need nothing beyond `glam`.
#[must_use]
pub fn smallest_eigenvector(m: DMat3) -> (DVec3, f64) {
    let mut a = m.to_cols_array_2d();
    let mut v = [[1.0, 0.0, 0.0], [0.0, 1.0, 0.0], [0.0, 0.0, 1.0]];
    for _ in 0..50 {
        // The largest off-diagonal element.
        let (mut p, mut q) = (0, 1);
        for (i, j) in [(0, 2), (1, 2)] {
            if a[i][j].abs() > a[p][q].abs() {
                (p, q) = (i, j);
            }
        }
        if a[p][q].abs() < 1e-15 {
            break;
        }
        let theta = (a[q][q] - a[p][p]) / (2.0 * a[p][q]);
        let t = theta.signum() / (theta.abs() + (theta * theta + 1.0).sqrt());
        let t = if theta == 0.0 { 1.0 } else { t };
        let c = 1.0 / (t * t + 1.0).sqrt();
        let s = t * c;
        for k in 0..3 {
            let (akp, akq) = (a[k][p], a[k][q]);
            a[k][p] = c * akp - s * akq;
            a[k][q] = s * akp + c * akq;
        }
        for k in 0..3 {
            let (apk, aqk) = (a[p][k], a[q][k]);
            a[p][k] = c * apk - s * aqk;
            a[q][k] = s * apk + c * aqk;
        }
        for row in &mut v {
            let (vp, vq) = (row[p], row[q]);
            row[p] = c * vp - s * vq;
            row[q] = s * vp + c * vq;
        }
    }
    let mut best = 0;
    for i in 1..3 {
        if a[i][i] < a[best][best] {
            best = i;
        }
    }
    (
        DVec3::new(v[0][best], v[1][best], v[2][best]).normalize(),
        a[best][best],
    )
}

/// Gaussian elimination with partial pivoting: `a x = b` for small dense
/// systems (the normal equations of the scale fit and the pose refinement).
#[must_use]
pub fn solve<const N: usize>(mut a: [[f64; N]; N], mut b: [f64; N]) -> Option<[f64; N]> {
    for col in 0..N {
        let pivot = (col..N).max_by(|&i, &j| a[i][col].abs().total_cmp(&a[j][col].abs()))?;
        if a[pivot][col].abs() < 1e-12 {
            return None;
        }
        a.swap(col, pivot);
        b.swap(col, pivot);
        for row in col + 1..N {
            let f = a[row][col] / a[col][col];
            for k in col..N {
                a[row][k] -= f * a[col][k];
            }
            b[row] -= f * b[col];
        }
    }
    let mut x = [0.0; N];
    for row in (0..N).rev() {
        let s: f64 = (row + 1..N).map(|k| a[row][k] * x[k]).sum();
        x[row] = (b[row] - s) / a[row][row];
    }
    Some(x)
}

/// The outer product `a bᵀ`.
#[must_use]
pub fn outer(a: DVec3, b: DVec3) -> DMat3 {
    DMat3::from_cols(a * b.x, a * b.y, a * b.z)
}

/// The closest point to two rays `p + s·a` and `q + t·b` (unit `a`, `b`): the
/// midpoint of the shortest segment between them, if they are not parallel and
/// it lies in front of both.
#[must_use]
pub fn triangulate(p: DVec3, a: DVec3, q: DVec3, b: DVec3) -> Option<DVec3> {
    let w = p - q;
    let ab = a.dot(b);
    let denom = 1.0 - ab * ab;
    if denom < 1e-9 {
        return None;
    }
    let s = (ab * b.dot(w) - a.dot(w)) / denom;
    let t = (b.dot(w) - ab * a.dot(w)) / denom;
    (s > 0.0 && t > 0.0).then(|| (p + a * s + q + b * t) * 0.5)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn close(a: DVec3, b: DVec3) -> bool {
        (a - b).length() < 1e-9
    }

    #[test]
    fn a_back_camera_at_ninety_degrees_looks_out_of_the_back() {
        let cam = Camera::back(500.0, 500.0, 320.0, 240.0, 640, 480, 90);
        let r = cam.device_from_camera;
        assert!(
            close(r * DVec3::Z, DVec3::NEG_Z),
            "the lens looks out of the back"
        );
        assert!(
            close(r * DVec3::X, DVec3::NEG_Y),
            "image right is device down"
        );
        assert!(
            close(r * DVec3::Y, DVec3::NEG_X),
            "image down is device left"
        );
        let upright = Camera::back(500.0, 500.0, 320.0, 240.0, 640, 480, 0);
        assert!(close(upright.device_from_camera * DVec3::X, DVec3::X));
        assert!(close(upright.device_from_camera * DVec3::Y, DVec3::NEG_Y));
    }

    #[test]
    fn a_pixel_round_trips_through_its_bearing() {
        let cam = Camera::back(437.0, 437.0, 320.0, 240.0, 640, 480, 90);
        let px = Vec2::new(100.5, 377.25);
        let b = cam.bearing(px);
        let back = cam.project(b * 7.0).unwrap();
        assert!((back - px).length() < 1e-3);
        assert!(cam.project(-b).is_none(), "behind the lens");
    }

    #[test]
    fn jacobi_finds_the_smallest_axis() {
        let axis = DVec3::new(1.0, 2.0, -0.5).normalize();
        // A matrix with eigenvalue 0.1 along `axis` and 3, 5 across it.
        let u = axis.any_orthonormal_vector();
        let w = axis.cross(u);
        let m = outer(axis, axis) * 0.1 + outer(u, u) * 3.0 + outer(w, w) * 5.0;
        let (v, l) = smallest_eigenvector(m);
        assert!((l - 0.1).abs() < 1e-9);
        assert!(v.dot(axis).abs() > 1.0 - 1e-9);
    }

    #[test]
    fn two_rays_meet_where_they_cross() {
        let target = DVec3::new(1.0, 5.0, 2.0);
        let (p, q) = (DVec3::ZERO, DVec3::new(0.5, 0.0, 0.0));
        let got = triangulate(p, (target - p).normalize(), q, (target - q).normalize()).unwrap();
        assert!(close(got, target));
        assert!(
            triangulate(p, DVec3::X, q, DVec3::X).is_none(),
            "parallel rays"
        );
    }
}
