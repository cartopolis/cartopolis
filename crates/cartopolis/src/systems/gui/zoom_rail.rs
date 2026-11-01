//! The zoom rail: a thin ladder on the right edge naming the rung of places the
//! camera is on, from the street to the edge of the observable universe
//! (`docs/mockups/zoom-ladder/`, v2). Earth mode only, on every device.
//!
//! At rest it is a 4 pt track between a small − (out, towards the universe) and
//! a + (in, towards the street), with the current rung's name beside the thumb.
//! Hovering or a finger on the track shows every rung's name as plain text;
//! dragging along it sets the height, and a tap on a name goes to that rung. It
//! replaced the + / − pair Earth mode had on a touchscreen, and the pair's
//! held-button rate is what its − and + still are.
//!
//! **Every rung gets the same length of track**, whatever span of heights it
//! covers, and within a rung the height runs on a log scale. The street's 2.5 km
//! and the universe's 10²⁷ m are each one step of the ladder, which is how people
//! think of them; a log scale over the whole range would give the street a
//! sliver and the empty light years most of the rail.
//!
//! The bands are `cartopolis_scenario::ladder::Rung::altitudes`, read rather
//! than copied, so the rail and a layer's choice of board cannot disagree about
//! which rung the camera is on.

use cartopolis_scenario::ladder::Rung;

/// One step of the rail, as drawn: the ladder's rung and the name it wears here.
pub struct RailRung {
    pub rung: Rung,
    pub label: &'static str,
}

/// The rail from the top (furthest out) to the bottom (the street). "City" for
/// the municipality: on a map, that is what a gemeente reads as.
pub const RAIL: &[RailRung] = &[
    RailRung {
        rung: Rung::Universe,
        label: "Universe",
    },
    RailRung {
        rung: Rung::CosmicWeb,
        label: "Cosmic web",
    },
    RailRung {
        rung: Rung::NearbyGalaxies,
        label: "Nearby galaxies",
    },
    RailRung {
        rung: Rung::LocalGroup,
        label: "Local Group",
    },
    RailRung {
        rung: Rung::Galaxy,
        label: "Milky Way",
    },
    RailRung {
        rung: Rung::Stars,
        label: "Stars",
    },
    RailRung {
        rung: Rung::SolarSystem,
        label: "Solar system",
    },
    RailRung {
        rung: Rung::EarthMoon,
        label: "Earth & Moon",
    },
    RailRung {
        rung: Rung::World,
        label: "World",
    },
    RailRung {
        rung: Rung::Continent,
        label: "Continent",
    },
    RailRung {
        rung: Rung::Country,
        label: "Country",
    },
    RailRung {
        rung: Rung::Province,
        label: "Province",
    },
    RailRung {
        rung: Rung::Municipality,
        label: "City",
    },
    RailRung {
        rung: Rung::District,
        label: "District",
    },
    RailRung {
        rung: Rung::Neighbourhood,
        label: "Neighbourhood",
    },
    RailRung {
        rung: Rung::Street,
        label: "Street",
    },
];

/// The lowest height the rail stands for: Earth mode's closest orbit.
pub const FLOOR_M: f64 = 15.0;
/// The highest: the observable universe's edge (4.4 × 10²⁶ m) inside the
/// screen, a phone's narrow side included.
pub const TOP_M: f64 = 3e27;

/// A rung's band on the rail, its open ends closed at the floor and the top.
fn band(r: &RailRung) -> (f64, f64) {
    let (lo, hi) = r.rung.altitudes();
    (lo.max(FLOOR_M), hi.min(TOP_M))
}

/// Where on the rail a height sits: 0 at the top, 1 at the bottom.
#[must_use]
pub fn position_of(alt_m: f64) -> f32 {
    let alt = alt_m.clamp(FLOOR_M, TOP_M);
    let n = RAIL.len() as f64;
    for (i, r) in RAIL.iter().enumerate() {
        let (lo, hi) = band(r);
        if alt >= lo {
            // Within the rung, higher is nearer its top edge.
            let t = ((alt.ln() - lo.ln()) / (hi.ln() - lo.ln())).clamp(0.0, 1.0);
            return ((i as f64 + 1.0 - t) / n) as f32;
        }
    }
    1.0
}

/// The height at a place on the rail, the inverse of [`position_of`].
#[must_use]
pub fn altitude_at(pos: f32) -> f64 {
    let n = RAIL.len() as f64;
    let p = f64::from(pos.clamp(0.0, 1.0)) * n;
    let i = (p.floor() as usize).min(RAIL.len() - 1);
    let t = 1.0 - (p - i as f64).clamp(0.0, 1.0);
    let (lo, hi) = band(&RAIL[i]);
    (lo.ln() + (hi.ln() - lo.ln()) * t).exp()
}

/// Which step of the rail a height is on.
#[must_use]
pub fn index_of(alt_m: f64) -> usize {
    let p = position_of(alt_m) * RAIL.len() as f32;
    (p.floor() as usize).min(RAIL.len() - 1)
}

/// The height a tap on a rung's name goes to: the middle of its band, on the
/// log scale — where its places are a comfortable size.
#[must_use]
pub fn rung_altitude(i: usize) -> f64 {
    altitude_at((i as f32 + 0.5) / RAIL.len() as f32)
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
    let (lo, hi) = band(&RAIL[index_of(alt_m)]);
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
    fn every_rung_on_the_way_is_on_screen_for_a_while() {
        // The street to the edge of the universe: no rung passed in under a
        // tenth of a second, no power of ten in under a fourteenth, and the
        // whole of it in a few seconds rather than a few frames.
        let from = rung_altitude(RAIL.len() - 1);
        let to = rung_altitude(0);
        let seen = fly(from, to);
        let secs = seen.len() as f64 / 60.0;
        assert!(
            (3.0..7.0).contains(&secs),
            "the whole ladder took {secs:.2} s"
        );
        for i in 1..RAIL.len() - 1 {
            let frames = seen.iter().filter(|a| index_of(**a) == i).count();
            assert!(frames >= 6, "{} passed in {frames} frames", RAIL[i].label);
        }
        for w in seen.windows(2) {
            let decades = (w[1] / w[0]).log10();
            assert!(decades <= FLIGHT_DECADES_PER_S / 60.0 + 1e-9, "{decades}");
        }
    }

    #[test]
    fn a_short_hop_is_short() {
        let seen = fly(rung_altitude(3), rung_altitude(4));
        assert!(seen.len() < 60, "one rung took {} frames", seen.len());
    }

    #[test]
    fn a_new_goal_bends_the_movement_without_a_jolt() {
        let (mut at, mut speed) = (rung_altitude(RAIL.len() - 1), 0.0);
        for _ in 0..30 {
            (at, speed) = flight_step(at, speed, rung_altitude(0), 1.0 / 60.0);
        }
        let before = speed;
        (_, speed) = flight_step(at, speed, rung_altitude(RAIL.len() - 1), 1.0 / 60.0);
        let most = FLIGHT_DECADES_PER_S2 * std::f64::consts::LN_10 / 60.0;
        assert!((before - speed).abs() <= most + 1e-9);
    }

    #[test]
    fn the_rail_runs_from_the_universe_to_the_street() {
        assert_eq!(RAIL.first().map(|r| r.rung), Some(Rung::Universe));
        assert_eq!(RAIL.last().map(|r| r.rung), Some(Rung::Street));
        // Every rung of the ladder is on it, once.
        for r in Rung::ALL {
            assert_eq!(RAIL.iter().filter(|x| x.rung == r).count(), 1, "{r:?}");
        }
    }

    #[test]
    fn position_and_height_are_inverses() {
        for alt in [
            20.0, 900.0, 5_000.0, 150_000.0, 2e6, 8e6, 3.8e7, 4e8, 1e12, 1e17, 5e20, 2e22, 1e24,
            1e26,
        ] {
            let back = altitude_at(position_of(alt));
            // The position is an f32: one step of it is a few millionths of a
            // rung, which is what a round trip may lose.
            assert!((back / alt - 1.0).abs() < 1e-5, "{alt} came back as {back}");
        }
    }

    #[test]
    fn higher_is_further_up_the_rail() {
        let mut last = 2.0;
        for alt in [
            20.0, 3e3, 2e4, 1e5, 5e5, 2e6, 5e6, 2e7, 1e9, 1e13, 1e17, 1e20,
        ] {
            let p = position_of(alt);
            assert!(p < last, "{alt} at {p}, not above {last}");
            last = p;
        }
    }

    #[test]
    fn a_rung_name_lands_on_its_rung() {
        for i in 0..RAIL.len() {
            assert_eq!(index_of(rung_altitude(i)), i, "{}", RAIL[i].label);
            assert_eq!(Rung::at_altitude(rung_altitude(i)), RAIL[i].rung);
        }
    }
}
