//! **The ladder of places** (v4-whole): one ladder from the stars to the street,
//! each rung a board — cells, a graph, or official areas. A layer can hold a
//! board on several rungs (CBS figures per buurt, wijk, gemeente, provincie and
//! country); the one drawn is the one whose rung fits the camera's height, and
//! the rungs a layer has no board for read the nearest it does.

use serde::{Deserialize, Serialize};

/// How many rungs away a layer is still read from.
pub const MAX_REACH: i64 = 2;

/// One rung, coarsest first.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Rung {
    /// The Milky Way from above, the Sun a point in one of its arms.
    Galaxy,
    /// Real star systems and the lanes between them.
    Stars,
    /// The Sun and the planets.
    SolarSystem,
    /// The Earth and the Moon's orbit, to scale.
    EarthMoon,
    World,
    Continent,
    Country,
    /// A province, a state, a safety region.
    Province,
    /// A municipality (gemeente).
    Municipality,
    /// A district (CBS wijk).
    District,
    /// A neighbourhood (CBS buurt).
    Neighbourhood,
    /// You, your building, the next corner.
    Street,
}

impl Rung {
    /// Every rung, coarsest first.
    pub const ALL: [Rung; 12] = [
        Rung::Galaxy,
        Rung::Stars,
        Rung::SolarSystem,
        Rung::EarthMoon,
        Rung::World,
        Rung::Continent,
        Rung::Country,
        Rung::Province,
        Rung::Municipality,
        Rung::District,
        Rung::Neighbourhood,
        Rung::Street,
    ];

    /// The camera heights, metres, at which this rung is the one read: its
    /// places are then roughly 30 to 300 points across on a phone or a desktop.
    #[must_use]
    pub fn altitudes(self) -> (f64, f64) {
        match self {
            Rung::Street => (0.0, 2_500.0),
            Rung::Neighbourhood => (2_500.0, 12_000.0),
            Rung::District => (12_000.0, 40_000.0),
            Rung::Municipality => (40_000.0, 220_000.0),
            Rung::Province => (220_000.0, 800_000.0),
            Rung::Country => (800_000.0, 3_000_000.0),
            Rung::Continent => (3_000_000.0, 9_000_000.0),
            Rung::World => (9_000_000.0, 60_000_000.0),
            // Above the World rung a "height" is how far out the view is: the
            // Moon's orbit (384,400 km) fits from a few hundred thousand km, the
            // planets from a few million, the nearest stars from a few hundred
            // AU, the galaxy from a thousand light years or so.
            Rung::EarthMoon => (60_000_000.0, 3e9),
            Rung::SolarSystem => (3e9, 1e14),
            Rung::Stars => (1e14, 1e18),
            Rung::Galaxy => (1e18, f64::INFINITY),
        }
    }

    /// The rung read at a camera height.
    #[must_use]
    pub fn at_altitude(alt_m: f64) -> Rung {
        Rung::ALL
            .into_iter()
            .find(|r| {
                let (lo, hi) = r.altitudes();
                (lo..hi).contains(&alt_m)
            })
            .unwrap_or(Rung::Street)
    }

    /// What a place on this rung is called, for "per …" ("per buurt" reads
    /// better in the publisher's own word, which the area set carries).
    #[must_use]
    pub fn label(self) -> &'static str {
        match self {
            Rung::Galaxy => "galaxy",
            Rung::Stars => "star system",
            Rung::SolarSystem => "body",
            Rung::EarthMoon => "body",
            Rung::World => "world region",
            Rung::Continent => "continent",
            Rung::Country => "country",
            Rung::Province => "province",
            Rung::Municipality => "municipality",
            Rung::District => "district",
            Rung::Neighbourhood => "neighbourhood",
            Rung::Street => "street",
        }
    }

    fn index(self) -> usize {
        Rung::ALL.iter().position(|r| *r == self).unwrap_or(0)
    }

    /// Of the rungs given, the one to read when the camera is at `self`: the
    /// same rung if present, else the nearest, and the coarser of two equally
    /// near (a coarser place is a summary; a finer one would be a crowd).
    /// Nothing more than [`MAX_REACH`] rungs away: municipal figures have
    /// nothing to say from beyond the Moon.
    #[must_use]
    pub fn nearest_of(self, have: &[Rung]) -> Option<usize> {
        let me = self.index() as i64;
        have.iter()
            .enumerate()
            .filter(|(_, r)| (r.index() as i64 - me).abs() <= MAX_REACH)
            .min_by_key(|(_, r)| {
                let d = r.index() as i64 - me;
                // Distance first; on a tie, coarser (negative) first.
                (d.abs(), d > 0)
            })
            .map(|(i, _)| i)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_rungs_tile_the_heights() {
        for w in Rung::ALL.windows(2) {
            // Coarsest first, so each rung sits above the next.
            assert_eq!(w[0].altitudes().0, w[1].altitudes().1, "{w:?}");
        }
        assert_eq!(Rung::at_altitude(5_000.0), Rung::Neighbourhood);
        assert_eq!(Rung::at_altitude(100_000.0), Rung::Municipality);
        assert_eq!(Rung::at_altitude(2_200_000.0), Rung::Country);
    }

    #[test]
    fn a_missing_rung_reads_the_nearest() {
        let have = [
            Rung::Neighbourhood,
            Rung::District,
            Rung::Municipality,
            Rung::Country,
        ];
        assert_eq!(Rung::District.nearest_of(&have), Some(1));
        // No provinces: country and municipality are both one away; the coarser.
        assert_eq!(Rung::Province.nearest_of(&have), Some(3));
        assert_eq!(Rung::Street.nearest_of(&have), Some(0));
        assert_eq!(Rung::SolarSystem.nearest_of(&have), None);
        assert_eq!(Rung::Street.nearest_of(&[]), None);
    }
}
