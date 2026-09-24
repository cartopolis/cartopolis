//! **A bridge deck's height along its length**, measured rather than assumed
//! level.
//!
//! The client stood every deck at one number — the median of the surface model
//! over its outline — and a viaduct is not level. The Emmaviaduct's deck falls
//! from 7.3 m NAP over the railway to 1.9 m where it meets the street 221 m on
//! (the 3D basisvoorziening's drape of it, 2026-09-24), so one median put its low
//! end metres in the air and its high end in the tracks, and the approach graded
//! up to meet a height the deck only had in the middle.
//!
//! The same samples already fetched for that median carry the slope: a profile is
//! their median per stretch of the deck's long axis. Median per stretch rather
//! than a fit, because the samples on a deck include its parapets, lamp posts and
//! lorries; and along the axis rather than per sample, because a deck is a road
//! and is level *across* itself — a height per sample would copy every parapet
//! into the surface.

/// Length of one stretch of the axis a median is taken over, metres.
pub const STATION_M: f64 = 4.0;

/// How far apart [`DeckProfile::fit_over`] reads the surface, metres: AHN's own
/// half metre would be four times the work for a median that does not move.
pub const SAMPLE_M: f32 = 1.0;

/// Fewest samples a stretch needs to count. At the 1 m the decks are measured at,
/// a stretch across a two-lane deck holds ~40; one holding a handful is a sliver
/// of the outline past the end of the road.
const MIN_SAMPLES: usize = 6;

/// What the survey says about one deck — the same shape whether atlas measured it
/// while grading or the client measured it itself.
#[derive(Clone, Debug, PartialEq)]
pub struct DeckMeasure {
    /// Metres above the ground beside it (`terrain_grade::deck_clearance`); 0 for
    /// a deck laid flat.
    pub height: f32,
    /// The median surface over it, NAP; `None` for a deck laid flat.
    pub top: Option<f32>,
    /// Its top along its length, tile-local, where the survey covers it well
    /// enough.
    pub profile: Option<DeckProfile>,
}

/// A deck's surface height as a function of distance along its long axis, in the
/// frame its samples were given in — a deck's own tile-local metres, for both
/// the client and atlas — with NAP heights.
#[derive(Clone, Debug, PartialEq)]
pub struct DeckProfile {
    origin: [f64; 2],
    /// Unit vector along the deck.
    axis: [f64; 2],
    /// `(distance along the axis, height)`, strictly increasing in distance.
    stations: Vec<(f64, f32)>,
}

impl DeckProfile {
    /// Fit a profile to a deck outline and the height samples inside it.
    ///
    /// The axis is the outline's principal direction. `None` when fewer than two
    /// stretches hold enough samples — a deck too short to have a slope worth
    /// drawing, or one the survey barely covers; the caller keeps its one height.
    #[must_use]
    pub fn fit(
        outline: &[[f64; 2]],
        samples: impl IntoIterator<Item = ([f64; 2], f32)>,
    ) -> Option<Self> {
        let (origin, axis) = principal_axis(outline)?;
        let along = |p: [f64; 2]| (p[0] - origin[0]) * axis[0] + (p[1] - origin[1]) * axis[1];
        let (lo, hi) = outline
            .iter()
            .map(|p| along(*p))
            .fold((f64::MAX, f64::MIN), |(a, b), s| (a.min(s), b.max(s)));
        if !(hi > lo) {
            return None;
        }
        let bins = ((hi - lo) / STATION_M).ceil().max(1.0) as usize;
        let mut buckets: Vec<Vec<f32>> = vec![Vec::new(); bins];
        for (p, h) in samples {
            if !h.is_finite() {
                continue;
            }
            let k = (((along(p) - lo) / STATION_M) as usize).min(bins - 1);
            buckets[k].push(h);
        }
        let mut stations: Vec<(f64, f32)> = buckets
            .into_iter()
            .enumerate()
            .filter(|(_, b)| b.len() >= MIN_SAMPLES)
            .map(|(k, mut b)| {
                b.sort_by(f32::total_cmp);
                (lo + (k as f64 + 0.5) * STATION_M, b[b.len() / 2])
            })
            .collect();
        if stations.len() < 2 {
            return None;
        }
        // A three-station running median: one stretch where a bus stood, or where
        // the outline's end caught the ground beside the abutment, is not a hump.
        if stations.len() >= 3 {
            let raw: Vec<f32> = stations.iter().map(|s| s.1).collect();
            for i in 1..raw.len() - 1 {
                let mut w = [raw[i - 1], raw[i], raw[i + 1]];
                w.sort_by(f32::total_cmp);
                stations[i].1 = w[1];
            }
        }
        Some(Self {
            origin,
            axis,
            stations,
        })
    }

    /// Fit a deck from a surface lookup in its own tile-local frame: every
    /// [`SAMPLE_M`] inside its outline, measured where `surface_at` answers.
    ///
    /// The one sampling both atlas and the client use, so a deck measured on the
    /// server and one measured on a device are the same profile.
    #[must_use]
    pub fn fit_over(
        deck: &crate::vector_tiles::BridgeDeck,
        surface_at: impl Fn([f32; 2]) -> Option<f32>,
    ) -> Option<Self> {
        let outline: Vec<[f64; 2]> = deck
            .rings
            .first()?
            .iter()
            .map(|p| [f64::from(p[0]), f64::from(p[1])])
            .collect();
        let mut samples = Vec::new();
        let mut z = deck.min[1] + SAMPLE_M * 0.5;
        while z < deck.max[1] {
            let mut x = deck.min[0] + SAMPLE_M * 0.5;
            while x < deck.max[0] {
                if deck.contains([x, z]) {
                    if let Some(h) = surface_at([x, z]) {
                        samples.push(([f64::from(x), f64::from(z)], h));
                    }
                }
                x += SAMPLE_M;
            }
            z += SAMPLE_M;
        }
        Self::fit(&outline, samples)
    }

    /// The same profile in a frame `s` times larger — atlas grades a window of
    /// cells in the frame of the one in the middle, and a neighbour's metres
    /// differ from it by the cosine of a kilometre of latitude.
    #[must_use]
    pub fn scaled(&self, s: f64) -> Self {
        Self {
            origin: [self.origin[0] * s, self.origin[1] * s],
            axis: self.axis,
            stations: self.stations.iter().map(|&(d, h)| (d * s, h)).collect(),
        }
    }

    /// The profile as numbers, in tile fractions — for atlas to publish and the
    /// client to read back in its own metres (`from_wire`): `[origin x, origin z,
    /// axis x, axis z, then distance and height per station]`.
    #[must_use]
    pub fn to_wire(&self, tile_m: f64) -> Vec<f64> {
        let mut out = vec![
            self.origin[0] / tile_m,
            self.origin[1] / tile_m,
            self.axis[0],
            self.axis[1],
        ];
        for &(d, h) in &self.stations {
            out.push(d / tile_m);
            out.push(f64::from(h));
        }
        out
    }

    /// [`Self::to_wire`], read back at `tile_m`. `None` for anything that is not
    /// a profile of at least two stations.
    #[must_use]
    pub fn from_wire(v: &[f64], tile_m: f64) -> Option<Self> {
        if v.len() < 8 || v.len() % 2 != 0 || !v.iter().all(|x| x.is_finite()) {
            return None;
        }
        let stations: Vec<(f64, f32)> = v[4..]
            .chunks_exact(2)
            .map(|c| (c[0] * tile_m, c[1] as f32))
            .collect();
        stations.windows(2).all(|w| w[1].0 > w[0].0).then(|| Self {
            origin: [v[0] * tile_m, v[1] * tile_m],
            axis: [v[2], v[3]],
            stations,
        })
    }

    /// The height at a point: linear between stations, level past the ends.
    #[must_use]
    pub fn at(&self, p: [f64; 2]) -> f32 {
        let s = (p[0] - self.origin[0]) * self.axis[0] + (p[1] - self.origin[1]) * self.axis[1];
        let first = self.stations[0];
        let last = self.stations[self.stations.len() - 1];
        if s <= first.0 {
            return first.1;
        }
        if s >= last.0 {
            return last.1;
        }
        let k = self.stations.partition_point(|st| st.0 <= s);
        let (a, b) = (self.stations[k - 1], self.stations[k]);
        let t = ((s - a.0) / (b.0 - a.0)) as f32;
        a.1 + (b.1 - a.1) * t
    }

    /// Lowest and highest station.
    #[must_use]
    pub fn range(&self) -> (f32, f32) {
        self.stations
            .iter()
            .fold((f32::MAX, f32::MIN), |(a, b), s| (a.min(s.1), b.max(s.1)))
    }

    /// Every station's height, for a digest that must move when the profile does.
    pub fn heights(&self) -> impl Iterator<Item = f32> + '_ {
        self.stations.iter().map(|s| s.1)
    }
}

/// The centroid of a polygon's vertices and the direction they spread furthest
/// along — for a deck, the direction of travel.
fn principal_axis(points: &[[f64; 2]]) -> Option<([f64; 2], [f64; 2])> {
    if points.len() < 2 {
        return None;
    }
    let n = points.len() as f64;
    let cx = points.iter().map(|p| p[0]).sum::<f64>() / n;
    let cy = points.iter().map(|p| p[1]).sum::<f64>() / n;
    let (mut sxx, mut syy, mut sxy) = (0.0, 0.0, 0.0);
    for p in points {
        let (dx, dy) = (p[0] - cx, p[1] - cy);
        sxx += dx * dx;
        syy += dy * dy;
        sxy += dx * dy;
    }
    let angle = 0.5 * (2.0 * sxy).atan2(sxx - syy);
    let axis = [angle.cos(), angle.sin()];
    axis.iter()
        .all(|v| v.is_finite())
        .then_some(([cx, cy], axis))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A 200 × 20 m deck running north-east, as RD metres.
    fn deck() -> (Vec<[f64; 2]>, impl Fn(f64, f64) -> [f64; 2]) {
        let (c, s) = (
            std::f64::consts::FRAC_PI_4.cos(),
            std::f64::consts::FRAC_PI_4.sin(),
        );
        let at = move |along: f64, across: f64| {
            [
                233_000.0 + along * c - across * s,
                581_000.0 + along * s + across * c,
            ]
        };
        let outline = vec![
            at(0.0, -10.0),
            at(200.0, -10.0),
            at(200.0, 10.0),
            at(0.0, 10.0),
        ];
        (outline, at)
    }

    /// Samples every metre over the deck, from a height function of distance.
    fn samples(
        at: &impl Fn(f64, f64) -> [f64; 2],
        height: impl Fn(f64, f64) -> f32,
    ) -> Vec<([f64; 2], f32)> {
        let mut out = Vec::new();
        for i in 0..200 {
            for j in -9..=9 {
                let (u, v) = (f64::from(i) + 0.5, f64::from(j));
                out.push((at(u, v), height(u, v)));
            }
        }
        out
    }

    #[test]
    fn a_ramp_is_followed_from_end_to_end() {
        let (outline, at) = deck();
        // 7.3 m at the start falling to 1.9 m at the end, as the Emmaviaduct does.
        let h = |u: f64, _| (7.3 - 5.4 * u / 200.0) as f32;
        let p = DeckProfile::fit(&outline, samples(&at, h)).unwrap();
        for u in [10.0, 60.0, 100.0, 150.0, 190.0] {
            let got = p.at(at(u, 3.0));
            assert!(
                (got - h(u, 0.0)).abs() < 0.1,
                "at {u} m: {got} vs {}",
                h(u, 0.0)
            );
        }
        let (lo, hi) = p.range();
        assert!(lo < 2.1 && hi > 7.1, "{lo}..{hi}");
    }

    /// Parapets along both edges and a lorry parked on it are not the deck.
    #[test]
    fn parapets_and_traffic_do_not_lift_the_surface() {
        let (outline, at) = deck();
        let h = |u: f64, v: f64| {
            let deck = 6.0;
            if v.abs() >= 8.0 {
                deck + 1.2
            } else if (80.0..92.0).contains(&u) && (-3.0..0.0).contains(&v) {
                deck + 3.8
            } else {
                deck
            }
        };
        let p = DeckProfile::fit(&outline, samples(&at, h)).unwrap();
        for u in [20.0, 85.0, 150.0] {
            assert!((p.at(at(u, 0.0)) - 6.0).abs() < 0.05, "at {u} m");
        }
    }

    /// One stretch the survey read wrong — the ground beside an abutment caught
    /// in the outline — is voted down by its neighbours.
    #[test]
    fn a_single_bad_stretch_is_not_a_dip() {
        let (outline, at) = deck();
        let h = |u: f64, _| {
            if (100.0..104.0).contains(&u) {
                1.0
            } else {
                6.0
            }
        };
        let p = DeckProfile::fit(&outline, samples(&at, h)).unwrap();
        assert!((p.at(at(102.0, 0.0)) - 6.0).abs() < 0.05);
    }

    #[test]
    fn a_deck_with_no_samples_has_no_profile() {
        let (outline, _) = deck();
        assert_eq!(DeckProfile::fit(&outline, Vec::new()), None);
    }

    /// What atlas publishes the client reads back as the same profile, and a
    /// profile scaled into a neighbour's frame answers at the scaled point.
    #[test]
    fn a_profile_survives_the_wire_and_a_change_of_frame() {
        let (outline, at) = deck();
        let p = DeckProfile::fit(&outline, samples(&at, |u, _| (2.0 + u * 0.03) as f32)).unwrap();
        let tile_m = 1470.0;
        let back = DeckProfile::from_wire(&p.to_wire(tile_m), tile_m).unwrap();
        for u in [5.0, 77.0, 190.0] {
            assert!((back.at(at(u, 0.0)) - p.at(at(u, 0.0))).abs() < 1e-4);
        }
        let s = 1.0003;
        let big = p.scaled(s);
        let q = at(120.0, 2.0);
        assert!((big.at([q[0] * s, q[1] * s]) - p.at(q)).abs() < 1e-4);
        assert_eq!(DeckProfile::from_wire(&[1.0, 2.0, 3.0], tile_m), None);
    }

    /// `fit_over` reads the surface inside a deck's outline and nowhere else.
    #[test]
    fn a_deck_is_fitted_from_inside_its_outline() {
        let deck = crate::vector_tiles::BridgeDeck {
            positions: Vec::new(),
            indices: Vec::new(),
            rings: vec![vec![
                [0.0, 0.0],
                [100.0, 0.0],
                [100.0, 10.0],
                [0.0, 10.0],
                [0.0, 0.0],
            ]],
            min: [0.0, 0.0],
            max: [100.0, 10.0],
        };
        // A slope over the deck and a wall of 30 m everywhere outside it.
        let p = DeckProfile::fit_over(&deck, |q| {
            Some(if deck.contains(q) {
                1.0 + q[0] * 0.05
            } else {
                30.0
            })
        })
        .unwrap();
        assert!((p.at([10.0, 5.0]) - 1.5).abs() < 0.15);
        assert!((p.at([90.0, 5.0]) - 5.5).abs() < 0.15);
    }

    #[test]
    fn past_the_ends_the_profile_is_level() {
        let (outline, at) = deck();
        let p = DeckProfile::fit(&outline, samples(&at, |u, _| u as f32 * 0.01)).unwrap();
        assert!((p.at(at(-50.0, 0.0)) - p.at(at(0.0, 0.0))).abs() < 1e-4);
        assert!((p.at(at(260.0, 0.0)) - p.at(at(200.0, 0.0))).abs() < 1e-4);
    }
}
