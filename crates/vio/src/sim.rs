//! A street, a phone in a hand, and the readings it would produce: the test
//! bench for the tracker, with the true path known.
//!
//! The street is points on a façade 6 m ahead, the pavement, and a side wall.
//! The phone is held upright in portrait, looking at the façade, and moved the
//! way somebody plays a theremin — sways of tens of centimetres a second or so
//! apart, with the wrist turning a few degrees. The gyroscope and
//! accelerometer readings are derived from that path exactly, then given the
//! noise and bias of a phone's; the corners are the street's points projected
//! through the camera with half a pixel of noise.

use glam::{DQuat, DVec3, Vec2};

use crate::attitude::G;
use crate::geom::Camera;
use crate::odometry::Observation;
use crate::{Imu, State, Tracker};

/// A deterministic pseudo-random stream.
pub struct Rng(u64);

impl Rng {
    pub fn new(seed: u64) -> Self {
        Self(seed)
    }
    pub fn uniform(&mut self) -> f64 {
        self.0 = self
            .0
            .wrapping_mul(6364136223846793005)
            .wrapping_add(1442695040888963407);
        (self.0 >> 11) as f64 / (1u64 << 53) as f64
    }
    pub fn normal(&mut self) -> f64 {
        let (u, v) = (self.uniform().max(1e-12), self.uniform());
        (-2.0 * u.ln()).sqrt() * (std::f64::consts::TAU * v).cos()
    }
}

/// The phone camera of the first device log: 2795 px on a 4096-wide array,
/// i.e. ~437 px at 640×480, sensor turned 90°.
pub fn phone_camera() -> Camera {
    let s = 640.0 / 4096.0;
    Camera::back(2795.0 * s, 2795.0 * s, 320.0, 240.0, 640, 480, 90)
}

/// The street's points, world axes (z up), the phone starting near the origin
/// looking along +y.
pub fn street(rng: &mut Rng) -> Vec<DVec3> {
    let mut pts = Vec::new();
    for _ in 0..400 {
        // Façade 6 m ahead, 8 m wide, up to 6 m high.
        pts.push(DVec3::new(
            rng.uniform() * 8.0 - 4.0,
            6.0,
            rng.uniform() * 6.0,
        ));
    }
    for _ in 0..250 {
        // Pavement.
        pts.push(DVec3::new(
            rng.uniform() * 6.0 - 3.0,
            1.0 + rng.uniform() * 5.0,
            0.0,
        ));
    }
    for _ in 0..200 {
        // A wall to the left, receding.
        pts.push(DVec3::new(-3.0, rng.uniform() * 6.0, rng.uniform() * 4.0));
    }
    pts
}

/// The true device position and orientation at `t` seconds: held at 1.4 m,
/// swaying.
pub fn hand(t: f64) -> (DVec3, DQuat) {
    use std::f64::consts::TAU;
    // Held still for the first second — as somebody does when they switch the
    // mode on — then the sway eases in over the next.
    let ease = ((t - 1.0).clamp(0.0, 1.0) * std::f64::consts::PI / 2.0)
        .sin()
        .powi(2);
    let p = DVec3::new(
        0.25 * (TAU * 0.6 * t).sin() * ease,
        0.12 * (TAU * 0.35 * t + 1.0).sin() * ease,
        1.4 + 0.10 * (TAU * 0.8 * t).sin() * ease,
    );
    // Upright in portrait facing +y: device x → world x, device y → world z,
    // device z (out of the screen) → world −y.
    let upright = DQuat::from_mat3(&glam::DMat3::from_cols(DVec3::X, DVec3::Z, DVec3::NEG_Y));
    let wrist = DQuat::from_rotation_z(0.10 * (TAU * 0.4 * t).sin() * ease)
        * DQuat::from_rotation_x(0.06 * (TAU * 0.5 * t + 0.3).sin() * ease);
    (p, (wrist * upright).normalize())
}

/// A brightness pattern that has corners at every scale: value noise summed
/// over three octaves, by world coordinate on a surface.
fn pattern(u: f64, v: f64) -> f64 {
    fn hash(x: i64, y: i64) -> f64 {
        let mut h = (x as u64).wrapping_mul(0x9E37_79B9_7F4A_7C15)
            ^ (y as u64).wrapping_mul(0xC2B2_AE3D_27D4_EB4F);
        h ^= h >> 29;
        h = h.wrapping_mul(0xBF58_476D_1CE4_E5B9);
        h ^= h >> 32;
        (h >> 11) as f64 / (1u64 << 53) as f64
    }
    fn noise(u: f64, v: f64) -> f64 {
        let (x0, y0) = (u.floor(), v.floor());
        let (fx, fy) = (u - x0, v - y0);
        let (sx, sy) = (fx * fx * (3.0 - 2.0 * fx), fy * fy * (3.0 - 2.0 * fy));
        let (x0, y0) = (x0 as i64, y0 as i64);
        let a = hash(x0, y0) + (hash(x0 + 1, y0) - hash(x0, y0)) * sx;
        let b = hash(x0, y0 + 1) + (hash(x0 + 1, y0 + 1) - hash(x0, y0 + 1)) * sx;
        a + (b - a) * sy
    }
    noise(u * 4.0, v * 4.0) * 0.5 + noise(u * 9.0, v * 9.0) * 0.3 + noise(u * 21.0, v * 21.0) * 0.2
}

/// What the camera sees at a pose: the façade, the pavement and the side wall
/// of [`street`], textured, by casting a ray per pixel. Sky is flat grey.
pub fn render(cam: &Camera, p: DVec3, q: DQuat) -> crate::image::Gray {
    let cam_q = q * cam.device_from_camera;
    let (w, h) = (cam.width as usize, cam.height as usize);
    let mut px = Vec::with_capacity(w * h);
    for y in 0..h {
        for x in 0..w {
            let d = cam_q * cam.bearing(Vec2::new(x as f32, y as f32));
            let mut best = (f64::INFINITY, 0.55);
            // Façade, y = 6.
            if d.y > 1e-6 {
                let t = (6.0 - p.y) / d.y;
                let hit = p + d * t;
                if t > 0.0 && hit.z >= 0.0 && hit.z < 8.0 && t < best.0 {
                    best = (t, pattern(hit.x, hit.z));
                }
            }
            // Pavement, z = 0.
            if d.z < -1e-6 {
                let t = -p.z / d.z;
                let hit = p + d * t;
                if t > 0.0 && hit.y < 6.0 && t < best.0 {
                    best = (t, 0.3 + 0.5 * pattern(hit.x + 50.0, hit.y));
                }
            }
            // Wall, x = −3.
            if d.x < -1e-6 {
                let t = (-3.0 - p.x) / d.x;
                let hit = p + d * t;
                if t > 0.0 && hit.y < 6.0 && hit.z >= 0.0 && hit.z < 5.0 && t < best.0 {
                    best = (t, 0.2 + 0.6 * pattern(hit.y + 100.0, hit.z));
                }
            }
            px.push((best.1 * 255.0).clamp(0.0, 255.0) as u8);
        }
    }
    crate::image::Gray::new(w, h, px)
}

/// Run the tracker over `seconds` of the bench, returning the true and tracked
/// positions at every frame it tracked in metres.
pub fn run(seconds: f64, noise: bool) -> (Vec<(DVec3, DVec3, DQuat, DQuat)>, Vec<State>) {
    run_with(seconds, noise, false)
}

/// [`run`], optionally through rendered images and the whole image front end
/// instead of corners projected straight from the street's points.
pub fn run_with(
    seconds: f64,
    noise: bool,
    images: bool,
) -> (Vec<(DVec3, DVec3, DQuat, DQuat)>, Vec<State>) {
    let mut rng = Rng::new(42);
    let cam = phone_camera();
    let pts = street(&mut rng);
    let mut tracker = Tracker::new(cam);
    let gyro_bias = if noise {
        DVec3::new(0.002, -0.001, 0.0015)
    } else {
        DVec3::ZERO
    };
    let accel_bias = if noise {
        DVec3::new(0.04, -0.03, 0.05)
    } else {
        DVec3::ZERO
    };
    let imu_dt = 1.0 / 240.0;
    let frame_dt = 1.0 / 30.0;
    let h = 1e-4;
    let mut t = 0.0;
    let mut next_frame = 0.2;
    let mut out = Vec::new();
    let mut states = Vec::new();
    while t < seconds {
        let ns = (t * 1e9) as i64;
        let (p, q) = hand(t);
        // Angular velocity in device axes, by finite difference.
        let (_, q2) = hand(t + h);
        let dq = q.inverse() * q2;
        let (axis, angle) = dq.to_axis_angle();
        let omega = axis * (angle / h);
        // Specific force: R⁻¹ (p̈ + g ẑ).
        let (pa, _) = hand(t - h);
        let (pb, _) = hand(t + h);
        let acc_world = (pb - 2.0 * p + pa) / (h * h);
        let f = q.inverse() * (acc_world + DVec3::Z * G);
        let jitter = |rng: &mut Rng, sigma: f64| {
            if noise {
                DVec3::new(rng.normal(), rng.normal(), rng.normal()) * sigma
            } else {
                DVec3::ZERO
            }
        };
        let g = omega + gyro_bias + jitter(&mut rng, 0.003);
        let a = f + accel_bias + jitter(&mut rng, 0.02);
        tracker.imu(ns, Imu::Gyro(g));
        tracker.imu(ns, Imu::Accel(a));
        if t >= next_frame && images {
            next_frame += frame_dt;
            let state = tracker.frame(ns, render(&cam, p, q));
            states.push(state);
            if let Some(pose) = tracker.pose() {
                out.push((p, pose.position, q, pose.orientation));
            }
        } else if t >= next_frame {
            next_frame += frame_dt;
            let cam_q = q * cam.device_from_camera;
            let seen: Vec<Observation> = pts
                .iter()
                .enumerate()
                .filter_map(|(i, w)| {
                    let local = cam_q.inverse() * (*w - p);
                    let px = cam.project(local)?;
                    let px = if noise {
                        px + Vec2::new(rng.normal() as f32, rng.normal() as f32) * 0.5
                    } else {
                        px
                    };
                    cam.contains(px).then_some(Observation { id: i as u64, px })
                })
                .collect();
            let state = tracker.observe(ns, &seen);
            states.push(state);
            if let Some(pose) = tracker.pose() {
                out.push((p, pose.position, q, pose.orientation));
            }
        }
        t += imu_dt;
    }
    (out, states)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Align the tracker's world to the truth's — it starts with the same up
    /// but its own heading and origin — from the first tracked frame, then
    /// measure every later frame's error.
    fn errors(samples: &[(DVec3, DVec3, DQuat, DQuat)]) -> Vec<f64> {
        let (p0, e0, q0, r0) = samples[0];
        let align = q0 * r0.inverse();
        samples
            .iter()
            .map(|(p, e, _, _)| ((*p - p0) - align * (*e - e0)).length())
            .collect()
    }

    #[test]
    fn the_bench_hand_is_upright_and_facing_the_street() {
        let (p, q) = hand(0.0);
        assert!(
            (q * DVec3::Y - DVec3::Z).length() < 0.1,
            "device up is world up"
        );
        assert!((q * DVec3::NEG_Z).y > 0.9, "the back faces the façade");
        assert!((p.z - 1.4).abs() < 0.2);
        let cam = phone_camera();
        assert!(
            ((q * cam.device_from_camera) * DVec3::Z).y > 0.9,
            "the lens looks ahead"
        );
    }

    /// Noise-free sensors. Not millimetres: the rotation is the gyroscope's
    /// (refined slowly), and the scale is a running fit that wanders a percent
    /// or two — measured 2026-10-08 at 1.2 cm worst over the run.
    #[test]
    fn it_tracks_a_clean_hand_to_a_centimetre_and_a_half() {
        let (samples, states) = run(10.0, false);
        assert!(
            samples.len() > 100,
            "tracked {} frames; states {:?}",
            samples.len(),
            &states[states.len().saturating_sub(5)..]
        );
        let e = errors(&samples);
        let worst = e.iter().copied().fold(0.0, f64::max);
        println!("clean: worst {worst:.4} m over {} frames", e.len());
        assert!(worst < 0.015, "worst error {worst:.4} m");
    }

    /// The whole pipeline: rendered frames through FAST and Lucas–Kanade, with
    /// noisy motion sensors — the bench closest to a phone.
    #[test]
    fn it_tracks_rendered_frames_to_a_centimetre() {
        let (samples, states) = run_with(12.0, true, true);
        assert!(
            samples.len() > 100,
            "tracked {} frames; last states {:?}",
            samples.len(),
            &states[states.len().saturating_sub(5)..]
        );
        let e = errors(&samples);
        let mut sorted = e.clone();
        sorted.sort_by(f64::total_cmp);
        let median = sorted[sorted.len() / 2];
        let p95 = sorted[sorted.len() * 95 / 100];
        println!(
            "rendered: median {median:.4} m, 95th percentile {p95:.4} m over {} frames",
            e.len()
        );
        assert!(median < 0.015, "median error {median:.4} m");
        assert!(p95 < 0.03, "95th percentile error {p95:.4} m");
    }

    #[test]
    fn it_tracks_a_noisy_hand_to_a_centimetre() {
        let (samples, states) = run(15.0, true);
        assert!(
            samples.len() > 150,
            "tracked {} frames; last states {:?}",
            samples.len(),
            &states[states.len().saturating_sub(5)..]
        );
        let e = errors(&samples);
        let mut sorted = e.clone();
        sorted.sort_by(f64::total_cmp);
        let median = sorted[sorted.len() / 2];
        let p95 = sorted[sorted.len() * 95 / 100];
        println!(
            "noisy: median {median:.4} m, 95th percentile {p95:.4} m over {} frames",
            e.len()
        );
        assert!(median < 0.01, "median error {median:.4} m");
        assert!(p95 < 0.02, "95th percentile error {p95:.4} m");
    }
}
