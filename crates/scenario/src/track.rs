//! Tracks: a moving thing or group — where it was, where it is, a cone of where
//! it may go, and times along the way (v1-engine). A drone group, a storm, an
//! evacuation, a convoy, a coronal mass ejection along the Sun–Earth lane.
//!
//! A track is a path, a start time and a speed; its position at a time is
//! walked along the path. The cone is how unsure the future is: it widens a
//! fixed number of kilometres per hour ahead of the clock.

use crate::board::LonLat;
use crate::time::{Stamp, Time};
use crate::Confidence;
use serde::Deserialize;

/// The Earth's mean radius, km.
const EARTH_KM: f64 = 6_371.0;

/// The cone's half-width at the head itself, km: a group is not a point.
pub const CONE_BASE_KM: f64 = 6.0;

/// Great-circle distance, km.
#[must_use]
pub fn km_between(a: LonLat, b: LonLat) -> f64 {
    let (la1, la2) = (a[1].to_radians(), b[1].to_radians());
    let dla = la2 - la1;
    let dlo = (b[0] - a[0]).to_radians();
    let h = (dla / 2.0).sin().powi(2) + la1.cos() * la2.cos() * (dlo / 2.0).sin().powi(2);
    2.0 * EARTH_KM * h.sqrt().asin()
}

/// A point `km` from `p` towards compass bearing `bearing_deg` (0 north, 90
/// east), on a local flat approximation: right for the tens of kilometres a
/// cone is wide.
#[must_use]
pub fn offset(p: LonLat, bearing_deg: f64, km: f64) -> LonLat {
    let b = bearing_deg.to_radians();
    let dlat = km * b.cos() / 110.574;
    let dlon = km * b.sin() / (111.320 * p[1].to_radians().cos().max(0.01));
    [p[0] + dlon, p[1] + dlat]
}

/// Compass bearing from `a` to `b`, degrees, on the same local approximation.
#[must_use]
pub fn bearing(a: LonLat, b: LonLat) -> f64 {
    let kx = 111.320 * ((a[1] + b[1]) / 2.0).to_radians().cos();
    let dx = (b[0] - a[0]) * kx;
    let dy = (b[1] - a[1]) * 110.574;
    dx.atan2(dy).to_degrees().rem_euclid(360.0)
}

/// Some of a group stopped: brought down, landed, turned back.
#[derive(Clone, Debug, Deserialize)]
pub struct DownSpec {
    pub t: Stamp,
    pub n: u32,
}

/// A track as the file writes it.
#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TrackSpec {
    pub id: String,
    /// How many things move together; none for a single thing (a storm).
    #[serde(default)]
    pub count: Option<u32>,
    pub speed_kmh: f64,
    /// Longitude, latitude pairs.
    pub path: Vec<LonLat>,
    /// When it sets off along the path.
    pub t0: Stamp,
    /// How fast the cone of where it may be widens, km per hour ahead.
    #[serde(default)]
    pub cone_km_per_h: f64,
    #[serde(default)]
    pub confidence: Confidence,
    #[serde(default)]
    pub down: Vec<DownSpec>,
}

/// A track on its scenario's clock.
#[derive(Clone, Debug)]
pub struct Track {
    pub id: String,
    pub count: Option<u32>,
    pub speed_kmh: f64,
    pub path: Vec<LonLat>,
    /// Distance from the path's start to each of its points, km.
    along: Vec<f64>,
    pub t0: Time,
    pub cone_km_per_h: f64,
    pub confidence: Confidence,
    /// When some stopped, and how many; sorted by time.
    pub down: Vec<(Time, u32)>,
}

impl Track {
    pub(crate) fn new(spec: TrackSpec, resolve: impl Fn(Stamp) -> Time) -> Result<Track, String> {
        if spec.path.len() < 2 {
            return Err(format!("track {:?}: a path needs two points", spec.id));
        }
        if !(spec.speed_kmh > 0.0) {
            return Err(format!("track {:?}: speed must be above zero", spec.id));
        }
        let mut along = vec![0.0];
        for w in spec.path.windows(2) {
            along.push(along.last().copied().unwrap_or(0.0) + km_between(w[0], w[1]));
        }
        let mut down: Vec<(Time, u32)> = spec.down.iter().map(|d| (resolve(d.t), d.n)).collect();
        down.sort_by_key(|d| d.0);
        Ok(Track {
            t0: resolve(spec.t0),
            id: spec.id,
            count: spec.count,
            speed_kmh: spec.speed_kmh,
            path: spec.path,
            along,
            cone_km_per_h: spec.cone_km_per_h,
            confidence: spec.confidence,
            down,
        })
    }

    /// The path's length, km.
    #[must_use]
    pub fn length_km(&self) -> f64 {
        self.along.last().copied().unwrap_or(0.0)
    }

    /// Distance from the path's start to each of its points, km.
    #[must_use]
    pub fn along(&self) -> &[f64] {
        &self.along
    }

    /// When it reaches the path's end.
    #[must_use]
    pub fn end(&self) -> Time {
        self.t0.plus(Time(
            (self.length_km() / self.speed_kmh * 3600.0).round() as i64
        ))
    }

    /// Kilometres along the path at `t`; `None` before it sets off or after it
    /// has reached the end.
    #[must_use]
    pub fn km_at(&self, t: Time) -> Option<f64> {
        let d = t.minus(self.t0).hours() * self.speed_kmh;
        (0.0..=self.length_km()).contains(&d).then_some(d)
    }

    /// The point `km` along the path, clamped to its ends.
    #[must_use]
    pub fn point_at_km(&self, km: f64) -> LonLat {
        let i = self
            .along
            .partition_point(|&a| a <= km)
            .clamp(1, self.path.len() - 1);
        let (a, b) = (self.path[i - 1], self.path[i]);
        let seg = self.along[i] - self.along[i - 1];
        let f = if seg > 0.0 {
            ((km - self.along[i - 1]) / seg).clamp(0.0, 1.0)
        } else {
            0.0
        };
        [a[0] + (b[0] - a[0]) * f, a[1] + (b[1] - a[1]) * f]
    }

    /// Where it is at `t`, while it is on its path.
    #[must_use]
    pub fn position(&self, t: Time) -> Option<LonLat> {
        self.km_at(t).map(|km| self.point_at_km(km))
    }

    /// The compass bearing it travels on `km` along the path.
    #[must_use]
    pub fn heading_at_km(&self, km: f64) -> f64 {
        let i = self
            .along
            .partition_point(|&a| a <= km)
            .clamp(1, self.path.len() - 1);
        bearing(self.path[i - 1], self.path[i])
    }

    /// How many have stopped by `t`.
    #[must_use]
    pub fn downed(&self, t: Time) -> u32 {
        self.down.iter().take_while(|d| d.0 <= t).map(|d| d.1).sum()
    }

    /// How many are still moving at `t`: none before the start or after the end.
    #[must_use]
    pub fn moving(&self, t: Time) -> Option<u32> {
        self.km_at(t)?;
        Some(self.count.map_or(1, |n| n.saturating_sub(self.downed(t))))
    }

    /// The cone's half-width `ahead` of the clock, km.
    #[must_use]
    pub fn cone_km(&self, ahead: Time) -> f64 {
        CONE_BASE_KM + self.cone_km_per_h * ahead.hours().max(0.0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn track() -> Track {
        let spec: TrackSpec = toml::from_str(
            r#"
            id = "A"
            count = 10
            speed_kmh = 100.0
            path = [[0.0, 0.0], [1.0, 0.0], [1.0, 1.0]]
            t0 = "+1:00"
            cone_km_per_h = 30.0
            down = [{ t = "+2:00", n = 3 }]
            "#,
        )
        .unwrap();
        Track::new(spec, |s| match s {
            Stamp::After(d) => Time(d),
            Stamp::Wall(m) => Time(i64::from(m) * 60),
            Stamp::Year(_) => Time::ZERO,
        })
        .unwrap()
    }

    #[test]
    fn a_track_is_walked_along_its_path() {
        let t = track();
        assert!((t.length_km() - 222.39).abs() < 0.1, "{}", t.length_km());
        assert_eq!(t.position(Time(0)), None);
        let p = t.position(Time(3600 + 1800)).unwrap();
        assert!((p[0] - 0.4497).abs() < 1e-3 && p[1].abs() < 1e-9, "{p:?}");
        // Past the corner it heads north.
        let km = t.km_at(Time(3600 + 7200)).unwrap();
        assert!((t.heading_at_km(km) - 0.0).abs() < 1e-6);
        assert_eq!(t.position(t.end().plus(Time(60))), None);
    }

    #[test]
    fn a_group_thins_as_it_is_brought_down() {
        let t = track();
        assert_eq!(t.moving(Time(0)), None);
        assert_eq!(t.moving(Time(3600 + 60)), Some(10));
        assert_eq!(t.moving(Time(7200)), Some(7));
    }

    #[test]
    fn the_cone_widens_with_time_ahead() {
        let t = track();
        assert_eq!(t.cone_km(Time(0)), CONE_BASE_KM);
        assert_eq!(t.cone_km(Time(7200)), CONE_BASE_KM + 60.0);
    }
}
