//! Metres from visual units, with the accelerometer.
//!
//! Over a short window the camera's metric path is `s · p_v(t)`, and the same
//! path from the accelerometer is `p₀ + v₀τ + ½bτ² + D(τ)`, where `D` is the
//! measured gravity-free acceleration integrated twice from the window's start.
//! Setting them equal at every frame gives three linear equations per frame in
//! ten unknowns — the scale `s`, the start position `p₀` and velocity `v₀`,
//! and `b`, the accelerometer's bias (plus whatever tilt error leaks gravity
//! into it) — solved by least squares over the last [`WINDOW_S`].
//!
//! The bias term is not optional. A phone accelerometer is off by a few
//! hundredths of m/s², and over two and a half seconds that alone is ten
//! centimetres of phantom motion — as much as the hand moves.
//!
//! The answer is only as good as the motion: holding the phone still says
//! nothing about scale. A window is used only when the accelerometer saw real
//! movement, and successive answers are blended, weighted by how much.

use std::collections::VecDeque;

use glam::DVec3;

/// The least-squares window. Longer than the slowest sway it must measure: at
/// 2.5 s the bench's 2.9-second sway read 2–3 % short, at 4 s within one or two.
pub const WINDOW_S: f64 = 4.0;
/// Motion (metres, accelerometer-integrated) a window must show to count.
const MIN_EXCURSION_M: f64 = 0.04;
/// A window whose fit misses by more than this RMS is not trusted.
const MAX_RMS_M: f64 = 0.03;
/// What the fit assumes about the accelerometer's bias, m/s² — a prior, not a
/// free parameter. Left free, the bias term's ½bτ² soaked up part of any sway
/// slower than the window and took the scale with it: the bench read 2–3 %
/// short on a sway of 2.9 s in a 2.5 s window.
const BIAS_PRIOR: f64 = 0.05;
/// The fit's own noise, metres, against which the prior is weighed.
const FIT_SIGMA_M: f64 = 0.005;

/// The scale estimate, as it stands.
#[derive(Debug, Default)]
pub struct Scale {
    /// Gravity-free world acceleration samples: (time s, m/s²).
    accel: VecDeque<(f64, DVec3)>,
    /// Visual positions at frames: (time s, visual units).
    frames: VecDeque<(f64, DVec3)>,
    /// Running weighted mean of good windows' `s`, and the weight behind it.
    sum: f64,
    weight: f64,
}

impl Scale {
    pub fn accel(&mut self, t: f64, a: DVec3) {
        self.accel.push_back((t, a));
        while self
            .accel
            .front()
            .is_some_and(|(t0, _)| t - t0 > WINDOW_S + 1.0)
        {
            self.accel.pop_front();
        }
    }

    /// Record a frame's visual position, and re-fit.
    pub fn frame(&mut self, t: f64, p: DVec3) {
        self.frames.push_back((t, p));
        while self.frames.front().is_some_and(|(t0, _)| t - t0 > WINDOW_S) {
            self.frames.pop_front();
        }
        if let Some((s, excursion)) = self.fit() {
            let w = excursion * excursion;
            self.sum += s * w;
            self.weight += w;
            // Forget slowly, so a scale that drifts (the odometry's does) is
            // followed rather than averaged away.
            self.sum *= 0.98;
            self.weight *= 0.98;
        }
    }

    /// Start over: the odometry restarted, and its visual units with it.
    pub fn reset(&mut self) {
        self.frames.clear();
        self.sum = 0.0;
        self.weight = 0.0;
    }

    /// Metres per visual unit, once there has been enough motion to say.
    #[must_use]
    pub fn metres_per_unit(&self) -> Option<f64> {
        (self.weight > MIN_EXCURSION_M * MIN_EXCURSION_M * 3.0).then(|| self.sum / self.weight)
    }

    /// One window's least-squares answer: `(s, excursion)`.
    fn fit(&self) -> Option<(f64, f64)> {
        let (t0, _) = *self.frames.front()?;
        let t_end = self.frames.back()?.0;
        if t_end - t0 < WINDOW_S * 0.8 || self.frames.len() < 10 {
            return None;
        }
        // Integrate the accelerometer twice from t0, sample by sample.
        let samples: Vec<(f64, DVec3)> = self
            .accel
            .iter()
            .filter(|(t, _)| *t >= t0 && *t <= t_end + 0.05)
            .copied()
            .collect();
        if samples.len() < 20 {
            return None;
        }
        let mut integrated = Vec::with_capacity(samples.len());
        let (mut v, mut d) = (DVec3::ZERO, DVec3::ZERO);
        let mut prev = samples[0];
        integrated.push((prev.0, d));
        for &(t, a) in &samples[1..] {
            let dt = t - prev.0;
            let a_mid = (a + prev.1) * 0.5;
            d += v * dt + a_mid * (0.5 * dt * dt);
            v += a_mid * dt;
            integrated.push((t, d));
            prev = (t, a);
        }
        let d_at = |t: f64| -> DVec3 {
            let i = integrated.partition_point(|(ti, _)| *ti < t);
            if i == 0 {
                return integrated[0].1;
            }
            if i >= integrated.len() {
                return integrated[integrated.len() - 1].1;
            }
            let (ta, da) = integrated[i - 1];
            let (tb, db) = integrated[i];
            let f = if tb > ta { (t - ta) / (tb - ta) } else { 0.0 };
            da + (db - da) * f
        };
        // Normal equations for x = [s, p0(3), v0(3), b(3)].
        let mut ata = [[0.0f64; 10]; 10];
        let mut atb = [0.0f64; 10];
        let mut lo = DVec3::splat(f64::INFINITY);
        let mut hi = DVec3::splat(f64::NEG_INFINITY);
        let mut rows = Vec::new();
        for &(t, p) in &self.frames {
            let tau = t - t0;
            let d = d_at(t);
            lo = lo.min(d);
            hi = hi.max(d);
            for axis in 0..3 {
                let mut row = [0.0f64; 10];
                row[0] = p[axis];
                row[1 + axis] = -1.0;
                row[4 + axis] = -tau;
                row[7 + axis] = -0.5 * tau * tau;
                let rhs = d[axis];
                for i in 0..10 {
                    atb[i] += row[i] * rhs;
                    for j in 0..10 {
                        ata[i][j] += row[i] * row[j];
                    }
                }
                rows.push((row, rhs));
            }
        }
        for axis in 0..3 {
            ata[7 + axis][7 + axis] += (FIT_SIGMA_M / BIAS_PRIOR).powi(2);
        }
        let excursion = (hi - lo).length();
        if excursion < MIN_EXCURSION_M {
            return None;
        }
        let x = crate::geom::solve(ata, atb)?;
        let s = x[0];
        if !s.is_finite() || s <= 0.0 {
            return None;
        }
        let rms = (rows
            .iter()
            .map(|(row, rhs)| {
                let fit: f64 = row.iter().zip(&x).map(|(a, b)| a * b).sum();
                (fit - rhs).powi(2)
            })
            .sum::<f64>()
            / rows.len() as f64)
            .sqrt();
        (rms < MAX_RMS_M).then_some((s, excursion))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn it_recovers_the_scale_and_ignores_a_bias() {
        let true_s = 0.37;
        let bias = DVec3::new(0.04, -0.03, 0.05);
        let mut sc = Scale::default();
        // A hand moving the phone: sinusoids, so acceleration is analytic.
        let pos = |t: f64| {
            DVec3::new(
                0.2 * (2.0 * std::f64::consts::PI * 0.8 * t).sin(),
                0.1 * (2.0 * std::f64::consts::PI * 0.5 * t).cos(),
                0.15 * (2.0 * std::f64::consts::PI * 1.1 * t).sin(),
            )
        };
        let acc = |t: f64| {
            let w = |f: f64| (2.0 * std::f64::consts::PI * f).powi(2);
            DVec3::new(
                -w(0.8) * 0.2 * (2.0 * std::f64::consts::PI * 0.8 * t).sin(),
                -w(0.5) * 0.1 * (2.0 * std::f64::consts::PI * 0.5 * t).cos(),
                -w(1.1) * 0.15 * (2.0 * std::f64::consts::PI * 1.1 * t).sin(),
            )
        };
        let mut t = 0.0;
        let mut next_frame = 0.0;
        while t < 8.0 {
            sc.accel(t, acc(t) + bias);
            if t >= next_frame {
                // Visual units: metric position divided by the true scale.
                sc.frame(t, pos(t) / true_s);
                next_frame += 1.0 / 30.0;
            }
            t += 1.0 / 240.0;
        }
        let s = sc.metres_per_unit().expect("enough motion to say");
        assert!(
            (s - true_s).abs() / true_s < 0.02,
            "scale {s}, want {true_s}"
        );
    }

    #[test]
    fn holding_still_says_nothing() {
        let mut sc = Scale::default();
        let mut t = 0.0;
        while t < 5.0 {
            sc.accel(t, DVec3::new(0.01, 0.0, 0.0));
            sc.frame(t, DVec3::ZERO);
            t += 1.0 / 30.0;
        }
        assert!(sc.metres_per_unit().is_none());
    }
}
