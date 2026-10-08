//! Which way the phone is turned: the gyroscope integrated, with the
//! accelerometer holding "up" in place.
//!
//! A phone's gyroscope is excellent over seconds and drifts over minutes; its
//! accelerometer knows where gravity is whenever the phone is not being shaken.
//! The complementary filter here leans on the first and lets the second pull
//! only tilt, gently, and only when the measured acceleration is close to one
//! g — a hand swinging the phone is not gravity. Heading is left free: nothing
//! here knows north, and the compass's 5–15° would wreck a centimetre tracker.
//! The world's heading is wherever the phone pointed when tracking began.
//!
//! **The gyroscope's bias is measured, not assumed away.** A phone gyroscope
//! reads a few thousandths of a rad/s while lying still; integrated, that is a
//! degree every ten seconds, and the bench showed a degree of tilt costing
//! about a decimetre of position. So while the phone is held still — at the
//! start, and whenever it is again — the gyroscope's mean is its bias, and is
//! subtracted from then on.

use glam::{DQuat, DVec3};

/// Standard gravity, m/s².
pub const G: f64 = 9.80665;

/// How hard one accelerometer sample pulls tilt back, per second of samples.
/// Gentle on purpose: a hand playing an instrument accelerates the phone by a
/// metre a second squared and more, and every sample that passes the band
/// below still carries some of that. The first bench run used 0.5 and read the
/// orientation 2–4° wrong with perfect sensors, which is a decimetre of
/// position at the façade's distance.
const TILT_GAIN_PER_S: f64 = 0.05;
/// Accelerometer readings further than this from one g are not used for tilt.
const STILL_BAND: f64 = 0.03 * G;
/// Readings in a row inside the band before "up" is set: a fifth of a second
/// held still, averaged, rather than one sample that may be mid-swing.
const START_SAMPLES: u32 = 48;
/// A gyroscope reading further than this (rad/s) from the bias is motion, not
/// stillness, for re-measuring the bias.
const STILL_GYRO: f64 = 0.03;
/// How fast a still phone's gyroscope readings move the bias estimate.
const BIAS_GAIN: f64 = 0.002;
/// Gyroscope readings in a row that must be still before the bias is touched
/// again: half a second at 240 Hz. A swaying hand passes through stillness at
/// every turn of the sway, and re-measuring there read the turn's own rotation
/// as bias — the bench's error tripled before this was a run.
const STILL_RUN: u32 = 120;

/// The phone's orientation in the gravity-aligned world (z up).
#[derive(Debug, Clone, Copy)]
pub struct Attitude {
    /// World from device.
    pub q: DQuat,
    started: bool,
    last_gyro_ns: Option<i64>,
    /// Still readings gathered towards the start, and how many.
    gathering: DVec3,
    gathered: u32,
    /// The gyroscope's bias (rad/s), and the readings gathered towards the
    /// first measurement of it.
    pub gyro_bias: DVec3,
    gyro_gathering: DVec3,
    gyro_gathered: u32,
    /// Whether the newest accelerometer reading was inside the stillness band.
    still: bool,
    /// Consecutive still gyroscope readings.
    still_run: u32,
}

impl Default for Attitude {
    fn default() -> Self {
        Self {
            q: DQuat::IDENTITY,
            started: false,
            last_gyro_ns: None,
            gathering: DVec3::ZERO,
            gathered: 0,
            gyro_bias: DVec3::ZERO,
            gyro_gathering: DVec3::ZERO,
            gyro_gathered: 0,
            still: false,
            still_run: 0,
        }
    }
}

impl Attitude {
    /// Whether the first accelerometer reading has set "up".
    #[must_use]
    pub fn started(&self) -> bool {
        self.started
    }

    /// Turn by a gyroscope reading (rad/s, device axes) taken at `t_ns`, over
    /// the time since the previous one.
    pub fn gyro(&mut self, t_ns: i64, omega: DVec3) {
        // Bias: averaged over the stillness that sets "up", then nudged
        // whenever the phone is still again.
        if self.still {
            if !self.started {
                self.gyro_gathering += omega;
                self.gyro_gathered += 1;
                self.gyro_bias = self.gyro_gathering / f64::from(self.gyro_gathered);
            } else if (omega - self.gyro_bias).length() < STILL_GYRO {
                self.still_run += 1;
                if self.still_run > STILL_RUN {
                    self.gyro_bias += (omega - self.gyro_bias) * BIAS_GAIN;
                }
            } else {
                self.still_run = 0;
            }
        } else {
            self.still_run = 0;
            if !self.started {
                self.gyro_gathering = DVec3::ZERO;
                self.gyro_gathered = 0;
            }
        }
        let omega = omega - self.gyro_bias;
        if let Some(prev) = self.last_gyro_ns {
            let dt = (t_ns - prev) as f64 * 1e-9;
            // A gap longer than a tenth of a second is the sensor stalling (a
            // backgrounded app), not motion to integrate.
            if dt > 0.0 && dt < 0.1 {
                let angle = omega.length() * dt;
                if angle > 0.0 {
                    self.q = (self.q * DQuat::from_axis_angle(omega / omega.length(), angle))
                        .normalize();
                }
            }
        }
        self.last_gyro_ns = Some(t_ns);
    }

    /// Pull tilt towards an accelerometer reading (m/s², device axes, the
    /// specific force — +g up when the phone lies still).
    pub fn accel(&mut self, a: DVec3, dt: f64) {
        let len = a.length();
        self.still = (len - G).abs() <= STILL_BAND;
        if (len - G).abs() > STILL_BAND {
            if !self.started {
                // Moving: start the stillness over.
                self.gathering = DVec3::ZERO;
                self.gathered = 0;
            }
            return;
        }
        if !self.started {
            self.gathering += a;
            self.gathered += 1;
            if self.gathered >= START_SAMPLES {
                // The rotation taking the measured "up" to world z.
                self.q = DQuat::from_rotation_arc(self.gathering.normalize(), DVec3::Z);
                self.started = true;
            }
            return;
        }
        let measured_up = self.q * (a / len);
        let correction = DQuat::from_rotation_arc(measured_up, DVec3::Z);
        let gain = (TILT_GAIN_PER_S * dt).clamp(0.0, 1.0);
        self.q = (DQuat::IDENTITY.slerp(correction, gain) * self.q).normalize();
    }

    /// Gravity-free acceleration in world axes, from a device-axes reading.
    #[must_use]
    pub fn linear_acceleration(&self, a: DVec3) -> DVec3 {
        self.q * a - DVec3::Z * G
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn still(att: &mut Attitude, a: DVec3) {
        for _ in 0..START_SAMPLES {
            att.accel(a, 0.005);
        }
    }

    #[test]
    fn holding_still_sets_up() {
        let mut att = Attitude::default();
        // Lying flat, screen up: device z is up.
        still(&mut att, DVec3::new(0.0, 0.0, G));
        assert!(att.started());
        assert!((att.q * DVec3::Z - DVec3::Z).length() < 1e-9);
        // Held upright: device y is up.
        let mut att = Attitude::default();
        still(&mut att, DVec3::new(0.0, G, 0.0));
        assert!((att.q * DVec3::Y - DVec3::Z).length() < 1e-9);
        assert!(att.linear_acceleration(DVec3::new(0.0, G, 0.0)).length() < 1e-9);
    }

    #[test]
    fn a_swinging_phone_does_not_set_up() {
        let mut att = Attitude::default();
        for i in 0..200 {
            let swing = if i % 2 == 0 { 4.0 } else { -4.0 };
            att.accel(DVec3::new(swing, G, 0.0), 0.005);
        }
        assert!(!att.started());
    }

    #[test]
    fn the_gyroscope_turns_it() {
        let mut att = Attitude::default();
        still(&mut att, DVec3::new(0.0, 0.0, G));
        // A quarter turn about device z, in 200 samples over a second.
        for i in 0..=200 {
            att.gyro(i * 5_000_000, DVec3::Z * std::f64::consts::FRAC_PI_2);
        }
        assert!((att.q * DVec3::X - DVec3::Y).length() < 1e-6);
    }

    #[test]
    fn tilt_drift_is_pulled_back_but_heading_is_not() {
        let mut att = Attitude::default();
        still(&mut att, DVec3::new(0.0, 0.0, G));
        // A false 10° tilt and a 30° heading, as if the gyro had drifted.
        att.q = DQuat::from_rotation_z(0.5) * DQuat::from_rotation_x(10f64.to_radians());
        for _ in 0..20000 {
            att.accel(DVec3::new(0.0, 0.0, G), 0.005);
        }
        assert!(
            (att.q * DVec3::Z - DVec3::Z).length() < 0.01,
            "tilt corrected"
        );
        let heading = (att.q * DVec3::X).y.atan2((att.q * DVec3::X).x);
        assert!((heading - 0.5).abs() < 0.02, "heading kept: {heading}");
    }

    #[test]
    fn a_still_gyroscope_s_bias_is_measured_and_removed() {
        let mut att = Attitude::default();
        let bias = DVec3::new(0.002, -0.001, 0.0015);
        for i in 0..START_SAMPLES as i64 + 10 {
            att.accel(DVec3::new(0.0, 0.0, G), 0.005);
            att.gyro(i * 5_000_000, bias);
        }
        assert!(att.started());
        assert!((att.gyro_bias - bias).length() < 1e-9);
        let before = att.q;
        for i in 100..1100 {
            att.gyro(i * 5_000_000, bias);
        }
        assert!(
            before.angle_between(att.q) < 1e-6,
            "five still seconds, no drift"
        );
    }

    #[test]
    fn a_swing_is_not_mistaken_for_gravity() {
        let mut att = Attitude::default();
        still(&mut att, DVec3::new(0.0, 0.0, G));
        let before = att.q;
        att.accel(DVec3::new(15.0, 0.0, G), 0.005);
        assert!(before.angle_between(att.q) < 1e-12);
    }
}
