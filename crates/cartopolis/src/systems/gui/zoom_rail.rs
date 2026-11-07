//! The zoom rail: a thin ladder on the right edge for the **scale** the camera
//! is in — the globe, the solar system, the Milky Way or the universe — with a
//! disc on top that switches between them (`docs/mockups/zoom-ladder/`, v4).
//! Earth mode only, on every device.
//!
//! It is a 4 pt track between a small − (out) and a + (in), with the current
//! step's name beside the thumb while it moves. A tap on the track goes to the
//! step tapped, and dragging along it sets the height
//! (`docs/notes/zoom-ladder.md`, "The rail names one rung"). The track spans one
//! scale only, and any zoom stops at its ends: at an end the − (or +) becomes the
//! next scale's button, and a tap on it, or half a second of zooming on, crosses
//! (`free_camera`, "the scale's edge"). Until 2026-11-07 one track ran from the
//! street to the edge of the universe, sixteen rungs of 11 pt each, and a slip
//! of the thumb threw the view out of the Earth.
//!
//! **Every step of a scale gets the same length of track**, whatever span of
//! heights it covers, and within a step the height runs on a log scale. The
//! street's 2.5 km and the Earth's 60,000 km are each one step, which is how
//! people think of them; a log scale over the whole range would give the street
//! a sliver.
//!
//! The steps are `cartopolis_scenario::ladder::Rung`s, and above the Earth parts
//! of them: a step's band is its rung's (`Rung::altitudes`, read rather than
//! copied) or a span inside it, so the rail and a layer's choice of board cannot
//! disagree about which rung the camera is on (tested).

use cartopolis_scenario::ladder::Rung;

/// One step of a scale's rail: the ladder's rung it lies on, the name it wears
/// here, and the part of the rung's band it covers, if not all of it.
pub struct Step {
    pub rung: Rung,
    pub label: &'static str,
    span: Option<(f64, f64)>,
    /// Where a tap on it goes, if not the middle of its band.
    tap: Option<f64>,
}

const fn step(rung: Rung, label: &'static str) -> Step {
    Step {
        rung,
        label,
        span: None,
        tap: None,
    }
}

const fn part(rung: Rung, label: &'static str, lo: f64, hi: f64) -> Step {
    Step {
        rung,
        label,
        span: Some((lo, hi)),
        tap: None,
    }
}

/// The globe's steps, from the top (furthest out) to the street. "City" for the
/// municipality: on a map, that is what a gemeente reads as.
const GLOBE: &[Step] = &[
    step(Rung::World, "World"),
    step(Rung::Continent, "Continent"),
    step(Rung::Country, "Country"),
    step(Rung::Province, "Province"),
    step(Rung::Municipality, "City"),
    step(Rung::District, "District"),
    step(Rung::Neighbourhood, "Neighbourhood"),
    step(Rung::Street, "Street"),
];

/// The Sun's: the heliopause is at about 120 AU (1.8·10¹³ m), the outer planets'
/// orbits from Jupiter's 5 AU to Neptune's 30, the inner ones' within Mars's 1.5.
const SOLAR_SYSTEM: &[Step] = &[
    part(Rung::SolarSystem, "Heliosphere", 1e13, 1e14),
    part(Rung::SolarSystem, "Outer planets", 5e11, 1e13),
    part(Rung::SolarSystem, "Inner planets", 2e10, 5e11),
    step(Rung::EarthMoon, "Earth & Moon"),
];

/// The galaxy's: Alpha Centauri is 4.1·10¹⁶ m away, the 46 star systems
/// `near_stars` draws within 16 ly (1.5·10¹⁷ m), the Local arm a few thousand
/// light years across.
const MILKY_WAY: &[Step] = &[
    Step {
        // In the middle of this band the Sun is still the middle and the disc
        // is squeezed into a crescent round it; it reads as a spiral once its
        // centre is the middle and the squeeze has eased (`cosmos::View::new`).
        tap: Some(6.0e20),
        ..part(Rung::Galaxy, "Whole galaxy", 1e20, 1e21)
    },
    part(Rung::Galaxy, "Spiral arms", 1e18, 1e20),
    part(Rung::Stars, "Star neighbourhood", 1e17, 1e18),
    part(Rung::Stars, "Nearest stars", 1e14, 1e17),
];

const UNIVERSE: &[Step] = &[
    step(Rung::Universe, "Observable universe"),
    step(Rung::CosmicWeb, "Cosmic web"),
    step(Rung::NearbyGalaxies, "Nearby galaxies"),
    step(Rung::LocalGroup, "Local Group"),
];

/// The four scales, each the stretch of the ladder whose subject stays in the
/// middle of the view: your place, the Sun, the galaxy, the cosmic web.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Scale {
    #[default]
    Globe,
    SolarSystem,
    MilkyWay,
    Universe,
}

impl Scale {
    /// From the street outwards.
    pub const ALL: [Scale; 4] = [
        Scale::Globe,
        Scale::SolarSystem,
        Scale::MilkyWay,
        Scale::Universe,
    ];

    #[must_use]
    pub fn label(self) -> &'static str {
        match self {
            Scale::Globe => "Globe",
            Scale::SolarSystem => "Solar system",
            Scale::MilkyWay => "Milky Way",
            Scale::Universe => "Universe",
        }
    }

    /// Its steps, from the top (furthest out) to the bottom.
    #[must_use]
    pub fn steps(self) -> &'static [Step] {
        match self {
            Scale::Globe => GLOBE,
            Scale::SolarSystem => SOLAR_SYSTEM,
            Scale::MilkyWay => MILKY_WAY,
            Scale::Universe => UNIVERSE,
        }
    }

    /// The heights it spans: its bottom step's floor to its top step's ceiling.
    #[must_use]
    pub fn bounds(self) -> (f64, f64) {
        let steps = self.steps();
        (band(&steps[steps.len() - 1]).0, band(&steps[0]).1)
    }

    /// The scale a height is in: a boundary belongs to the scale above it.
    #[must_use]
    pub fn of(alt_m: f64) -> Scale {
        Scale::ALL
            .into_iter()
            .rev()
            .find(|s| alt_m >= s.bounds().0)
            .unwrap_or(Scale::Globe)
    }

    /// Whether a height is within it, its ends included and open where the
    /// ladder ends (below the street, above the universe's edge).
    #[must_use]
    pub fn holds(self, alt_m: f64) -> bool {
        let (lo, hi) = self.bounds();
        let lo = if self == Scale::Globe { 0.0 } else { lo * 0.99 };
        let hi = if self == Scale::Universe {
            f64::INFINITY
        } else {
            hi * 1.01
        };
        (lo..=hi).contains(&alt_m)
    }

    /// The next scale out, if any.
    #[must_use]
    pub fn outward(self) -> Option<Scale> {
        let i = Scale::ALL.iter().position(|s| *s == self)?;
        Scale::ALL.get(i + 1).copied()
    }

    /// The next scale in, if any.
    #[must_use]
    pub fn inward(self) -> Option<Scale> {
        let i = Scale::ALL.iter().position(|s| *s == self)?;
        i.checked_sub(1).map(|i| Scale::ALL[i])
    }

    /// Where its whole subject is in view: the Earth, the eight planets, the
    /// spiral, the redshift survey's clusters inside the edge's ring.
    #[must_use]
    pub fn overview(self) -> f64 {
        let i = match self {
            Scale::Globe => 0,
            Scale::SolarSystem => 1,
            Scale::MilkyWay => 0,
            Scale::Universe => 1,
        };
        step_altitude(self, i)
    }

    /// Where a jump to it lands: the overview, but the globe returns to the
    /// height it was left at by a jump (`CameraState::globe_return_alt`).
    #[must_use]
    pub fn arrival(self, globe_return: Option<f64>) -> f64 {
        match (self, globe_return) {
            (Scale::Globe, Some(alt)) => alt,
            _ => self.overview(),
        }
    }

    /// The height its edge with the next scale out (`up`) or in leads to: the
    /// first step over the line, as a tap there would.
    #[must_use]
    pub fn across(self, up: bool) -> Option<f64> {
        if up {
            let s = self.outward()?;
            Some(step_altitude(s, s.steps().len() - 1))
        } else {
            Some(step_altitude(self.inward()?, 0))
        }
    }
}

/// The lowest height the rail stands for: Earth mode's closest orbit.
pub const FLOOR_M: f64 = 15.0;
/// The highest: the observable universe's edge (4.4 × 10²⁶ m) inside the
/// screen, a phone's narrow side included.
pub const TOP_M: f64 = 3e27;

/// A step's band, its open ends closed at the floor and the top.
fn band(s: &Step) -> (f64, f64) {
    let (lo, hi) = s.span.unwrap_or_else(|| s.rung.altitudes());
    (lo.max(FLOOR_M), hi.min(TOP_M))
}

/// Where on a scale's rail a height sits: 0 at the top, 1 at the bottom, and
/// held to the ends for a height outside it.
#[must_use]
pub fn position_of(scale: Scale, alt_m: f64) -> f32 {
    let steps = scale.steps();
    let (lo, hi) = scale.bounds();
    let alt = alt_m.clamp(lo, hi);
    let n = steps.len() as f64;
    for (i, s) in steps.iter().enumerate() {
        let (lo, hi) = band(s);
        if alt >= lo {
            // Within the step, higher is nearer its top edge.
            let t = ((alt.ln() - lo.ln()) / (hi.ln() - lo.ln())).clamp(0.0, 1.0);
            return ((i as f64 + 1.0 - t) / n) as f32;
        }
    }
    1.0
}

/// The height at a place on a scale's rail, the inverse of [`position_of`].
#[must_use]
pub fn altitude_at(scale: Scale, pos: f32) -> f64 {
    let steps = scale.steps();
    let n = steps.len() as f64;
    let p = f64::from(pos.clamp(0.0, 1.0)) * n;
    let i = (p.floor() as usize).min(steps.len() - 1);
    let t = 1.0 - (p - i as f64).clamp(0.0, 1.0);
    let (lo, hi) = band(&steps[i]);
    (lo.ln() + (hi.ln() - lo.ln()) * t).exp()
}

/// Which step of a scale's rail a height is on.
#[must_use]
pub fn index_of(scale: Scale, alt_m: f64) -> usize {
    let n = scale.steps().len();
    let p = position_of(scale, alt_m) * n as f32;
    (p.floor() as usize).min(n - 1)
}

/// The step a height is on, whichever scale it is in.
#[must_use]
pub fn step_at(alt_m: f64) -> &'static Step {
    let scale = Scale::of(alt_m);
    &scale.steps()[index_of(scale, alt_m)]
}

/// The height a tap on a step of the rail goes to: the middle of its band, on
/// the log scale — where its places are a comfortable size — unless the step
/// says otherwise (the whole galaxy).
#[must_use]
pub fn step_altitude(scale: Scale, i: usize) -> f64 {
    let steps = scale.steps();
    steps[i]
        .tap
        .unwrap_or_else(|| altitude_at(scale, (i as f32 + 0.5) / steps.len() as f32))
}

/// The most rungs a jump along the rail passes in a second.
const FLIGHT_RUNGS_PER_S: f64 = 5.0;
/// And the most powers of ten: the rungs past the Earth span up to four each,
/// and the view turns and changes its middle within one or two of them.
const FLIGHT_DECADES_PER_S: f64 = 7.0;
/// How fast a jump picks up speed and slows to a stop, powers of ten per
/// second squared.
const FLIGHT_DECADES_PER_S2: f64 = 10.0;

/// How fast a jump may go at a height, in natural-log units of height per
/// second: no faster than [`FLIGHT_RUNGS_PER_S`] through the rung it is on, nor
/// than [`FLIGHT_DECADES_PER_S`].
fn flight_speed_cap(alt_m: f64) -> f64 {
    let (lo, hi) = band(step_at(alt_m));
    ((hi / lo).ln() * FLIGHT_RUNGS_PER_S).min(FLIGHT_DECADES_PER_S * std::f64::consts::LN_10)
}

/// One frame of a jump to `goal_m`: where the height it flies through is now,
/// and how fast it moves (natural-log units a second, up positive), from where
/// it was and how fast it moved.
///
/// A jump along the rail **flies through every rung on the way**, rather than
/// gliding at a rate proportional to what is left: that covered twenty powers
/// of ten in its first quarter second, and the view's turns, its moving middle
/// and the catalogues fading in all flashed past in a few frames. This one
/// picks up speed, keeps to the caps above and slows to a stop on the goal; a
/// new goal mid-way (a finger moving along the track) bends the same movement
/// rather than starting another.
#[must_use]
pub fn flight_step(at_m: f64, speed: f64, goal_m: f64, dt: f64) -> (f64, f64) {
    let accel = FLIGHT_DECADES_PER_S2 * std::f64::consts::LN_10;
    let (at, goal) = (at_m.ln(), goal_m.ln());
    let left = goal - at;
    // As fast as allowed here, and no faster than can still stop on the goal.
    let want = left.signum() * flight_speed_cap(at_m).min((2.0 * accel * left.abs()).sqrt());
    let speed = speed + (want - speed).clamp(-accel * dt, accel * dt);
    let next = at + speed * dt;
    if (goal - next) * left <= 0.0 {
        (goal_m, 0.0)
    } else {
        (next.exp(), speed)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Every step from the top of the universe to the street, with its scale.
    fn every_step() -> impl Iterator<Item = (Scale, usize)> {
        Scale::ALL
            .into_iter()
            .rev()
            .flat_map(|s| (0..s.steps().len()).map(move |i| (s, i)))
    }

    /// Flies a jump at 60 frames a second and returns the heights it passed
    /// through, one a frame, until it stops.
    fn fly(from: f64, to: f64) -> Vec<f64> {
        let (mut at, mut speed) = (from, 0.0);
        let mut seen = vec![at];
        while at != to {
            (at, speed) = flight_step(at, speed, to, 1.0 / 60.0);
            seen.push(at);
            assert!(seen.len() < 60 * 60, "{from} → {to} never arrived");
        }
        seen
    }

    #[test]
    fn a_jump_stops_on_its_goal_from_either_side() {
        for (from, to) in [(200.0, 2.4e26), (2.4e26, 200.0), (5e6, 6e6), (1e12, 1e11)] {
            let seen = fly(from, to);
            assert_eq!(*seen.last().unwrap(), to);
            // Never past it on the way.
            let (lo, hi) = (from.min(to), from.max(to));
            assert!(seen.iter().all(|a| *a >= lo * 0.999 && *a <= hi * 1.001));
        }
    }

    #[test]
    fn every_step_on_the_way_is_on_screen_for_a_while() {
        // The street to the edge of the universe: no step passed in under a
        // tenth of a second, no power of ten in under a fourteenth, and the
        // whole of it in a few seconds rather than a few frames.
        let from = step_altitude(Scale::Globe, GLOBE.len() - 1);
        let to = step_altitude(Scale::Universe, 0);
        let seen = fly(from, to);
        let secs = seen.len() as f64 / 60.0;
        assert!(
            (3.0..7.0).contains(&secs),
            "the whole ladder took {secs:.2} s"
        );
        for (scale, i) in every_step() {
            if (scale, i) == (Scale::Universe, 0) || (scale, i) == (Scale::Globe, GLOBE.len() - 1)
            {
                continue;
            }
            let frames = seen
                .iter()
                .filter(|a| Scale::of(**a) == scale && index_of(scale, **a) == i)
                .count();
            assert!(frames >= 6, "{} passed in {frames} frames", scale.steps()[i].label);
        }
        for w in seen.windows(2) {
            let decades = (w[1] / w[0]).log10();
            assert!(decades <= FLIGHT_DECADES_PER_S / 60.0 + 1e-9, "{decades}");
        }
    }

    #[test]
    fn a_short_hop_is_short() {
        let seen = fly(step_altitude(Scale::Globe, 3), step_altitude(Scale::Globe, 4));
        assert!(seen.len() < 60, "one step took {} frames", seen.len());
    }

    #[test]
    fn a_new_goal_bends_the_movement_without_a_jolt() {
        let street = step_altitude(Scale::Globe, GLOBE.len() - 1);
        let (mut at, mut speed) = (street, 0.0);
        for _ in 0..30 {
            (at, speed) = flight_step(at, speed, Scale::Universe.overview(), 1.0 / 60.0);
        }
        let before = speed;
        (_, speed) = flight_step(at, speed, street, 1.0 / 60.0);
        let most = FLIGHT_DECADES_PER_S2 * std::f64::consts::LN_10 / 60.0;
        assert!((before - speed).abs() <= most + 1e-9);
    }

    #[test]
    fn the_scales_tile_the_ladder() {
        // Each scale starts where the one below ends, the globe at the floor
        // and the universe at the top.
        assert_eq!(Scale::Globe.bounds().0, FLOOR_M);
        assert_eq!(Scale::Universe.bounds().1, TOP_M);
        for w in Scale::ALL.windows(2) {
            assert_eq!(w[0].bounds().1, w[1].bounds().0, "{:?} | {:?}", w[0], w[1]);
        }
        // Within a scale the steps adjoin, top to bottom.
        for s in Scale::ALL {
            for w in s.steps().windows(2) {
                assert_eq!(band(&w[0]).0, band(&w[1]).1, "{} | {}", w[0].label, w[1].label);
            }
        }
        // Every rung of the ladder is on some rail.
        for r in Rung::ALL {
            assert!(every_step().any(|(s, i)| s.steps()[i].rung == r), "{r:?}");
        }
    }

    #[test]
    fn a_step_lies_on_its_rung() {
        // A part of a rung stays inside it, so the rail and a layer's board
        // agree on the rung.
        for (s, i) in every_step() {
            let st = &s.steps()[i];
            let (lo, hi) = band(st);
            let (rlo, rhi) = st.rung.altitudes();
            assert!(lo >= rlo.max(FLOOR_M) && hi <= rhi.min(TOP_M), "{}", st.label);
        }
    }

    #[test]
    fn position_and_height_are_inverses() {
        for alt in [
            20.0, 900.0, 5_000.0, 150_000.0, 2e6, 8e6, 3.8e7, 4e8, 1e12, 1e17, 5e20, 2e22, 1e24,
            1e26,
        ] {
            let s = Scale::of(alt);
            let back = altitude_at(s, position_of(s, alt));
            // The position is an f32: one step of it is a few millionths of a
            // rung, which is what a round trip may lose.
            assert!((back / alt - 1.0).abs() < 1e-5, "{alt} came back as {back}");
        }
    }

    #[test]
    fn higher_is_further_up_the_rail() {
        for s in Scale::ALL {
            let (lo, hi) = s.bounds();
            let mut last = 2.0;
            let mut alt = lo * 1.01;
            while alt < hi {
                let p = position_of(s, alt);
                assert!(p < last, "{s:?}: {alt} at {p}, not above {last}");
                last = p;
                alt *= 3.0;
            }
        }
    }

    #[test]
    fn a_rail_holds_its_ends() {
        // A height outside the scale sits at the end it is beyond.
        assert_eq!(position_of(Scale::SolarSystem, 1e20), 0.0);
        assert_eq!(position_of(Scale::SolarSystem, 100.0), 1.0);
    }

    #[test]
    fn a_step_name_lands_on_its_step() {
        for (s, i) in every_step() {
            let alt = step_altitude(s, i);
            let st = &s.steps()[i];
            assert_eq!(Scale::of(alt), s, "{}", st.label);
            assert_eq!(index_of(s, alt), i, "{}", st.label);
            assert_eq!(Rung::at_altitude(alt), st.rung, "{}", st.label);
        }
    }

    #[test]
    fn crossing_an_edge_lands_over_the_line() {
        for s in Scale::ALL {
            if let Some(alt) = s.across(true) {
                assert_eq!(Some(Scale::of(alt)), s.outward());
            }
            if let Some(alt) = s.across(false) {
                assert_eq!(Some(Scale::of(alt)), s.inward());
            }
        }
        assert_eq!(Scale::Universe.across(true), None);
        assert_eq!(Scale::Globe.across(false), None);
        // The globe comes back to where a jump left it.
        assert_eq!(Scale::Globe.arrival(Some(1234.0)), 1234.0);
        assert_eq!(Scale::MilkyWay.arrival(Some(1234.0)), 6.0e20);
    }
}
