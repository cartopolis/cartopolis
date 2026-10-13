//! One epoch's answer, assembled from the sentences that describe it.
//!
//! GGA is the epoch: each one produces a [`Solution`]. RMC and GST arrive beside
//! it, in an order the receiver chooses and sometimes at a lower rate, so the
//! latest of each is attached when its time is close enough to the GGA's to be
//! the same epoch (or, for GST, the one before — an error estimate a second old
//! still describes the solution, a speed a second old does not).

use crate::nmea::{FixQuality, Gga, Gst, Rmc, Sentence};

/// How far an RMC may be from the GGA it is attached to, seconds.
const RMC_SAME_EPOCH_S: f64 = 0.15;
/// How far a GST may be, seconds.
const GST_RECENT_S: f64 = 1.5;

/// A position, and everything known about how good it is.
#[derive(Clone, Debug, PartialEq)]
pub struct Solution {
    pub lat: f64,
    pub lng: f64,
    pub quality: FixQuality,
    /// Above mean sea level, metres.
    pub altitude_m: Option<f64>,
    pub geoid_separation_m: Option<f64>,
    pub satellites: Option<u32>,
    pub hdop: Option<f64>,
    pub correction_age_s: Option<f64>,
    pub utc_s: Option<f64>,
    /// The receiver's own 1σ errors (GST), metres: latitude, longitude, height.
    pub sigma_m: Option<(f64, f64, Option<f64>)>,
    pub speed_ms: Option<f64>,
    pub course_deg: Option<f64>,
}

impl Solution {
    /// Horizontal accuracy in metres, at roughly the 1σ (68 %) radius Android
    /// reports — so a receiver fix and a phone fix compare like for like.
    ///
    /// From the receiver's GST when it gave one: √(σ_lat² + σ_lng²), the
    /// distance root mean square. Otherwise estimated from the fix quality —
    /// the typical figure for each kind of solution, widened by HDOP where the
    /// solution is code-based and geometry is what limits it.
    #[must_use]
    pub fn accuracy_m(&self) -> f64 {
        if let Some((lat, lng, _)) = self.sigma_m {
            return lat.hypot(lng).max(0.005);
        }
        let hdop = self.hdop.unwrap_or(1.5).max(0.5);
        match self.quality {
            FixQuality::RtkFixed => 0.02,
            FixQuality::RtkFloat => 0.3,
            FixQuality::Dgps => (0.8 * hdop).max(0.5),
            FixQuality::Single => (2.5 * hdop).max(1.5),
            FixQuality::Estimated => 15.0,
            FixQuality::None => f64::INFINITY,
        }
    }

    /// Ellipsoidal height (WGS84), when both parts are known.
    #[must_use]
    pub fn ellipsoid_height_m(&self) -> Option<f64> {
        Some(self.altitude_m? + self.geoid_separation_m?)
    }
}

/// What the receiver said last, short of a position: enough to show "searching,
/// 4 satellites" while there is no fix.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Tracking {
    pub satellites: Option<u32>,
    pub quality: FixQuality,
}

/// Builds [`Solution`]s from a stream of sentences.
#[derive(Default)]
pub struct Assembler {
    rmc: Option<Rmc>,
    gst: Option<Gst>,
    /// The raw body of the last GGA with a position, for the caster.
    pub last_gga: Option<String>,
    pub tracking: Tracking,
}

impl Assembler {
    /// Take one parsed sentence (and its raw body); returns a solution when the
    /// sentence completes an epoch with a position.
    pub fn push(&mut self, sentence: Sentence, body: &str) -> Option<Solution> {
        match sentence {
            Sentence::Rmc(r) => {
                self.rmc = Some(r);
                None
            }
            Sentence::Gst(g) => {
                self.gst = Some(g);
                None
            }
            Sentence::Gga(g) => self.epoch(g, body),
            Sentence::Reply(_) | Sentence::Other => None,
        }
    }

    fn epoch(&mut self, g: Gga, body: &str) -> Option<Solution> {
        self.tracking = Tracking {
            satellites: g.satellites,
            quality: g.quality,
        };
        let (Some(lat), Some(lng)) = (g.lat, g.lng) else {
            return None;
        };
        if !g.quality.has_position() {
            return None;
        }
        self.last_gga = Some(body.to_string());
        let near = |t: Option<f64>, limit: f64| match (g.utc_s, t) {
            (Some(a), Some(b)) => {
                // Across midnight the difference wraps.
                let mut d = a - b;
                if d < -43_200.0 {
                    d += 86_400.0;
                }
                d.abs() <= limit
            }
            // A receiver with no time yet: take what there is.
            _ => true,
        };
        let rmc = self
            .rmc
            .as_ref()
            .filter(|r| r.valid && near(r.utc_s, RMC_SAME_EPOCH_S));
        let gst = self.gst.as_ref().filter(|s| near(s.utc_s, GST_RECENT_S));
        Some(Solution {
            lat,
            lng,
            quality: g.quality,
            altitude_m: g.altitude_m,
            geoid_separation_m: g.geoid_separation_m,
            satellites: g.satellites,
            hdop: g.hdop,
            correction_age_s: g.correction_age_s,
            utc_s: g.utc_s,
            sigma_m: gst.map(|s| (s.lat_sigma_m, s.lng_sigma_m, s.alt_sigma_m)),
            speed_ms: rmc.and_then(|r| r.speed_ms),
            course_deg: rmc.and_then(|r| r.course_deg),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::nmea::parse;

    fn feed(a: &mut Assembler, body: &str) -> Option<Solution> {
        a.push(parse(body), body)
    }

    #[test]
    fn an_epoch_takes_its_own_rmc_and_a_recent_gst() {
        let mut a = Assembler::default();
        assert!(feed(
            &mut a,
            "GNGST,101529.00,0.9,0.02,0.01,45.0,0.012,0.009,0.021"
        )
        .is_none());
        assert!(feed(
            &mut a,
            "GNRMC,101530.00,A,5313.1,N,00633.9,E,2.0,180.0,131026,,,R,V"
        )
        .is_none());
        let s = feed(
            &mut a,
            "GNGGA,101530.00,5313.1,N,00633.9,E,4,30,0.6,8.1,M,40.5,M,1.0,0000",
        )
        .unwrap();
        assert_eq!(s.quality, FixQuality::RtkFixed);
        assert_eq!(s.course_deg, Some(180.0));
        // The GST's ellipse (0.02 × 0.01 m) is what the accuracy comes from.
        assert!((s.accuracy_m() - 0.02f64.hypot(0.01)).abs() < 1e-12);
        assert_eq!(s.ellipsoid_height_m(), Some(8.1 + 40.5));
    }

    #[test]
    fn a_stale_rmc_gives_no_speed() {
        let mut a = Assembler::default();
        feed(
            &mut a,
            "GNRMC,101520.00,A,5313.1,N,00633.9,E,2.0,180.0,131026,,,R,V",
        );
        let s = feed(
            &mut a,
            "GNGGA,101530.00,5313.1,N,00633.9,E,1,12,1.0,8.1,M,40.5,M,,",
        )
        .unwrap();
        assert_eq!(s.speed_ms, None);
        // No GST: estimated from the quality and HDOP.
        assert!((s.accuracy_m() - 2.5).abs() < 1e-12);
    }

    #[test]
    fn no_fix_is_tracking_not_a_solution() {
        let mut a = Assembler::default();
        assert!(feed(&mut a, "GNGGA,101530.00,,,,,0,04,99.0,,,,,,").is_none());
        assert_eq!(a.tracking.satellites, Some(4));
        assert!(a.last_gga.is_none());
    }
}
