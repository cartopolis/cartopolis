//! Where the planets are: heliocentric positions from JPL's approximate
//! Keplerian elements (E. M. Standish, "Keplerian Elements for Approximate
//! Positions of the Major Planets", Table 1, valid 1800–2050 AD), good to a few
//! arcminutes for the inner planets and well under a degree for the outer ones —
//! far finer than the solar-system board draws them (`systems::cosmos`).
//!
//! Positions are in the ecliptic plane of J2000 (x towards the vernal equinox,
//! y 90° on, z towards the north ecliptic pole), in astronomical units.

use std::f64::consts::PI;

/// One planet's elements at J2000 and their rates per Julian century: semi-major
/// axis (AU), eccentricity, inclination (°), mean longitude (°), longitude of
/// perihelion (°), longitude of the ascending node (°).
pub struct Planet {
    pub name: &'static str,
    pub elements: [f64; 6],
    pub rates: [f64; 6],
}

/// Mercury to Neptune, in order out from the Sun. The Earth's row is the
/// Earth–Moon barycentre, which is where the Earth is to within 4,700 km.
pub const PLANETS: [Planet; 8] = [
    Planet {
        name: "Mercury",
        elements: [
            0.387_099_27,
            0.205_635_93,
            7.004_979_02,
            252.250_323_50,
            77.457_796_28,
            48.330_765_93,
        ],
        rates: [
            0.000_000_37,
            0.000_019_06,
            -0.005_947_49,
            149_472.674_111_75,
            0.160_476_89,
            -0.125_340_81,
        ],
    },
    Planet {
        name: "Venus",
        elements: [
            0.723_335_66,
            0.006_776_72,
            3.394_676_05,
            181.979_099_50,
            131.602_467_18,
            76.679_842_55,
        ],
        rates: [
            0.000_003_90,
            -0.000_041_07,
            -0.000_788_90,
            58_517.815_387_29,
            0.002_683_29,
            -0.277_694_18,
        ],
    },
    Planet {
        name: "Earth",
        elements: [
            1.000_002_61,
            0.016_711_23,
            -0.000_015_31,
            100.464_571_66,
            102.937_681_93,
            0.0,
        ],
        rates: [
            0.000_005_62,
            -0.000_043_92,
            -0.012_946_68,
            35_999.372_449_81,
            0.323_273_64,
            0.0,
        ],
    },
    Planet {
        name: "Mars",
        elements: [
            1.523_710_34,
            0.093_394_10,
            1.849_691_42,
            -4.553_432_05,
            -23.943_629_59,
            49.559_538_91,
        ],
        rates: [
            0.000_018_47,
            0.000_078_82,
            -0.008_131_31,
            19_140.302_684_99,
            0.444_410_88,
            -0.292_573_43,
        ],
    },
    Planet {
        name: "Jupiter",
        elements: [
            5.202_887_00,
            0.048_386_24,
            1.304_396_95,
            34.396_440_51,
            14.728_479_83,
            100.473_909_09,
        ],
        rates: [
            -0.000_116_07,
            -0.000_132_53,
            -0.001_837_14,
            3_034.746_127_75,
            0.212_526_68,
            0.204_691_06,
        ],
    },
    Planet {
        name: "Saturn",
        elements: [
            9.536_675_94,
            0.053_861_79,
            2.485_991_87,
            49.954_244_23,
            92.598_878_31,
            113.662_424_48,
        ],
        rates: [
            -0.001_250_60,
            -0.000_509_91,
            0.001_936_09,
            1_222.493_622_01,
            -0.418_972_16,
            -0.288_677_94,
        ],
    },
    Planet {
        name: "Uranus",
        elements: [
            19.189_164_64,
            0.047_257_44,
            0.772_637_83,
            313.238_104_51,
            170.954_276_30,
            74.016_925_03,
        ],
        rates: [
            -0.001_961_76,
            -0.000_043_97,
            -0.002_429_39,
            428.482_027_85,
            0.408_052_81,
            0.042_405_89,
        ],
    },
    Planet {
        name: "Neptune",
        elements: [
            30.069_922_76,
            0.008_590_48,
            1.770_043_47,
            -55.120_029_69,
            44.964_762_27,
            131.784_225_74,
        ],
        rates: [
            0.000_262_91,
            0.000_051_05,
            0.000_353_72,
            218.459_453_25,
            -0.322_414_64,
            -0.005_086_64,
        ],
    },
];

/// The Earth's index in [`PLANETS`].
pub const EARTH: usize = 2;

/// Seconds from the Unix epoch to J2000.0 (2000-01-01 12:00 TT, taken as UTC:
/// the minute of difference is nothing at this precision).
const J2000_UNIX: f64 = 946_728_000.0;

/// Julian centuries since J2000 at a Unix time.
#[must_use]
pub fn centuries_since_j2000(unix: f64) -> f64 {
    (unix - J2000_UNIX) / (36_525.0 * 86_400.0)
}

/// A planet's heliocentric ecliptic position (AU) at a Unix time.
#[must_use]
pub fn position(p: &Planet, unix: f64) -> [f64; 3] {
    let t = centuries_since_j2000(unix);
    let e: Vec<f64> = (0..6).map(|i| p.elements[i] + p.rates[i] * t).collect();
    let (a, ecc, inc, l, peri, node) = (
        e[0],
        e[1],
        e[2].to_radians(),
        e[3].to_radians(),
        e[4].to_radians(),
        e[5].to_radians(),
    );
    let w = peri - node;
    let m = (l - peri).rem_euclid(2.0 * PI);
    // Kepler's equation by Newton's method; five steps hold to 1e-12 for every
    // eccentricity here (Mercury's 0.21 is the largest).
    let mut ea = m + ecc * m.sin();
    for _ in 0..5 {
        ea -= (ea - ecc * ea.sin() - m) / (1.0 - ecc * ea.cos());
    }
    let x = a * (ea.cos() - ecc);
    let y = a * (1.0 - ecc * ecc).sqrt() * ea.sin();
    let (cw, sw, cn, sn, ci, si) = (
        w.cos(),
        w.sin(),
        node.cos(),
        node.sin(),
        inc.cos(),
        inc.sin(),
    );
    [
        (cw * cn - sw * sn * ci) * x + (-sw * cn - cw * sn * ci) * y,
        (cw * sn + sw * cn * ci) * x + (-sw * sn + cw * cn * ci) * y,
        (sw * si) * x + (cw * si) * y,
    ]
}

/// One astronomical unit, metres.
pub const AU_M: f64 = 149_597_870_700.0;
/// One light year, metres.
pub const LIGHT_YEAR_M: f64 = 9_460_730_472_580_800.0;
/// The speed of light, metres a second.
pub const LIGHT_M_S: f64 = 299_792_458.0;

#[cfg(test)]
mod tests {
    use super::*;

    fn longitude_deg(p: [f64; 3]) -> f64 {
        p[1].atan2(p[0]).to_degrees().rem_euclid(360.0)
    }

    /// On 1 November the Sun stands at about 219° of ecliptic longitude, so the
    /// Earth, seen from the Sun, at about 39°.
    #[test]
    fn the_earth_is_where_the_calendar_puts_it() {
        let nov_1_2026 = 1_793_534_400.0; // 2026-11-01 12:00 UTC
        let lon = longitude_deg(position(&PLANETS[EARTH], nov_1_2026));
        assert!((lon - 39.0).abs() < 1.5, "{lon}");
        let r = position(&PLANETS[EARTH], nov_1_2026);
        let d = (r[0] * r[0] + r[1] * r[1] + r[2] * r[2]).sqrt();
        assert!((d - 0.992).abs() < 0.005, "{d} AU");
    }

    /// Mars stood opposite the Sun (opposition) on 16 January 2025: the same
    /// heliocentric longitude as the Earth, give or take a degree.
    #[test]
    fn mars_at_its_2025_opposition() {
        let jan_16_2025 = 1_737_028_800.0;
        let earth = longitude_deg(position(&PLANETS[EARTH], jan_16_2025));
        let mars = longitude_deg(position(&PLANETS[3], jan_16_2025));
        assert!((earth - mars).abs() < 1.5, "earth {earth}, mars {mars}");
    }

    #[test]
    fn every_orbit_has_its_size() {
        let now = 1_793_534_400.0;
        for p in &PLANETS {
            let r = position(p, now);
            let d = (r[0] * r[0] + r[1] * r[1] + r[2] * r[2]).sqrt();
            let a = p.elements[0];
            let e = p.elements[1];
            assert!(
                d >= a * (1.0 - e) - 0.01 && d <= a * (1.0 + e) + 0.01,
                "{}",
                p.name
            );
        }
    }
}
