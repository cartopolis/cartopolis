//! Sun position and photometry math shared by the sky, lighting, window-glow, and
//! fog systems. The photometry follows the same extinction physics the Nishita sky
//! shader integrates, so lights lit from these curves match what the sky shows.

use bevy::math::{Quat, Vec3};

/// Local apparent-solar time of day (0–24 h) for a Unix timestamp at a longitude
/// (degrees east). The sun is overhead at local solar noon, and each 15° of
/// longitude shifts that noon one hour from UTC — so this is the time that
/// *physically* places the sun in the sky, with no timezone or DST table. Feed
/// it into `time_of_day` (see `main::apply_sun_mode`) to make the sky track the
/// real clock at wherever the map is anchored.
pub fn solar_time_of_day(now_unix: f64, lng_deg: f64) -> f32 {
    let utc_hours = (now_unix / 3600.0).rem_euclid(24.0);
    (utc_hours + lng_deg / 15.0).rem_euclid(24.0) as f32
}

/// Sun-arc angle for a time of day (0–24 h): 0 = sunrise (east), π/2 = noon (up),
/// π = sunset (west), −π/2 = midnight (below).
pub fn day_angle(hours: f32) -> f32 {
    let t = (hours / 24.0).rem_euclid(1.0);
    (t - 0.25) * std::f32::consts::TAU
}

/// Solar declination in radians for a day of the year (1–366): the sun's angle
/// north/south of the celestial equator, swinging ±23.44° across the year. This
/// is the seasonal term the old equinox-only arc was missing — it's what makes
/// summer days long and winter days short at mid-latitudes. Cooper's 1969
/// approximation (within ~0.5° of the true value, ample for lighting).
pub fn solar_declination_rad(day_of_year: f32) -> f32 {
    const AXIAL_TILT: f32 = 0.409_105; // 23.44° in radians
    AXIAL_TILT * (std::f32::consts::TAU * (day_of_year + 284.0) / 365.0).sin()
}

/// Unit direction toward the sun for a time of day and solar declination, in real
/// horizontal coordinates at the observer latitude:
/// `sin(elevation) = sinφ·sinδ + cosφ·cosδ·cos H` (H = hour angle, 0 at solar
/// noon). Because elevation now carries the declination δ, the day length,
/// sunrise/sunset times and peak elevation all track the season: at the June
/// solstice (δ ≈ +23.4°, φ = 52°) noon tops out near 61° and the sun stays up past
/// 20:00 solar time; at the December solstice the day collapses to ~7½ h. The old
/// arc ignored δ entirely and set the sun at the horizon at 06:00/18:00 every day
/// of the year. World frame: +x east, +y up, +z south (noon sun sits south, so
/// even midday light arrives at an angle — facades stay modeled, shadows never
/// collapse to dots).
pub fn sun_direction(hours: f32, decl_rad: f32) -> Vec3 {
    let lat = OBSERVER_LATITUDE_DEG.to_radians();
    let hour_angle = (hours - 12.0) / 12.0 * std::f32::consts::PI; // 0 at solar noon
    let (sin_h, cos_h) = hour_angle.sin_cos();
    let (sin_d, cos_d) = decl_rad.sin_cos();
    let (sin_l, cos_l) = lat.sin_cos();
    let east = -sin_h * cos_d;
    let up = sin_l * sin_d + cos_l * cos_d * cos_h;
    let south = cos_h * cos_d * sin_l - sin_d * cos_l;
    Vec3::new(east, up, south).normalize()
}

/// Hermite smoothstep: 0 below `e0`, 1 above `e1`, smooth in between.
pub fn smoothstep(e0: f32, e1: f32, x: f32) -> f32 {
    let t = ((x - e0) / (e1 - e0)).clamp(0.0, 1.0);
    t * t * (3.0 - 2.0 * t)
}

/// Zenith optical depth of Rayleigh scattering per RGB channel (λ⁻⁴, so blue is
/// stripped from the beam first). Overhead this leaves warm white; through a
/// horizon-grazing path it leaves deep orange-red — the classic low sun.
const RAYLEIGH: Vec3 = Vec3::new(0.041, 0.100, 0.250);

/// Baseline aerosol (haze) optical depth per RGB channel. Gentler wavelength slope
/// than Rayleigh (Ångström exponent ~1.3): haze dims the sun and reddens it a
/// little more on top of the Rayleigh red.
const AEROSOL: Vec3 = Vec3::new(0.037, 0.048, 0.063);

/// Diurnal aerosol swing: the day's convection stirs dust and moisture into the
/// air and it settles again overnight, so dawn comes through clean air (0.6×,
/// bright golden sunrise) while dusk comes through the day's accumulated haze
/// (1.8×, deeper red and dimmer sunset). This is what breaks the sunrise/sunset
/// symmetry of a pure position model.
fn haze(hours: f32) -> f32 {
    let phase = ((hours - 6.0) / 24.0 * std::f32::consts::TAU).cos(); // 1 at 06:00, −1 at 18:00
                                                                      // Swing kept moderate (0.75–1.45×): at 0.6–1.8× the evening beam was so far
                                                                      // into the red that dusk read as a single saturated red sheet once the sky,
                                                                      // the fog glow and the sunlight all took their cut of the same tint.
    1.1 - 0.35 * phase
}

/// Relative optical air mass toward the sun (Kasten–Young 1989): ~1 overhead,
/// ~38 at the horizon. Held at the horizon value below it, so the last light
/// stays deep red while the disc sets instead of snapping to a different hue.
fn air_mass(sin_elevation: f32) -> f32 {
    let s = sin_elevation.max(0.0);
    let h_deg = s.asin().to_degrees();
    1.0 / (s + 0.50572 * (h_deg + 6.079_95).powf(-1.636_4))
}

/// Scale height of the atmosphere (metres): the altitude over which air density
/// falls by 1/e. Everything above an observer at height `h` is a column of
/// `exp(-h/H)` of the sea-level one, which is why a cloud deck is lit by a
/// warmer, brighter sun than the ground beneath it.
const SCALE_HEIGHT_M: f32 = 8_500.0;

/// Per-channel atmospheric transmittance of the direct sun beam for a time of day.
/// Encodes both the sun's color (channel ratios) and its dimming (magnitude).
pub fn sun_transmittance(hours: f32, decl_rad: f32) -> Vec3 {
    sun_transmittance_at(hours, decl_rad, 0.0)
}

/// [`sun_transmittance`] for an observer `alt_m` metres above sea level.
///
/// Only the air *above* the observer extincts the beam, so the optical depth is
/// scaled by the barometric factor `exp(-alt/H)`. This is what makes a cloud deck
/// catch pink and gold while the city under it has already gone blue and dim —
/// the term real-time sky models usually approximate with a flat fudge factor
/// (see `docs/notes/cloud-lighting.md`).
///
/// The factorisation is the standard plane-parallel approximation: for a slant
/// path through a stratified atmosphere the density scaling and the air-mass
/// factor don't strictly separate, but at cloud altitudes the error is far below
/// anything visible.
pub fn sun_transmittance_at(hours: f32, decl_rad: f32, alt_m: f32) -> Vec3 {
    let column = (-alt_m.max(0.0) / SCALE_HEIGHT_M).exp();
    let depth = (RAYLEIGH + AEROSOL * haze(hours)) * column;
    (-depth * air_mass(sun_direction(hours, decl_rad).y)).exp()
}

/// Fraction of the solar disc's light clearing the horizon — the soft boundary for
/// the *direct* beam around sunrise/sunset (no hard on/off at 6:00/18:00).
pub fn sun_disc(hours: f32, decl_rad: f32) -> f32 {
    smoothstep(-0.03, 0.06, sun_direction(hours, decl_rad).y)
}

/// Smooth 0–1 daylight factor for sky-driven light (ambient, fog color). Rises
/// through civil twilight (sun still below the horizon) and saturates by
/// mid-morning, because the scattered sky lights the scene well before and after
/// the sun itself crosses the horizon.
pub fn daylight(hours: f32, decl_rad: f32) -> f32 {
    smoothstep(-0.15, 0.40, sun_direction(hours, decl_rad).y)
}

/// How close the sun is to the horizon, 1 at the horizon → 0 away from it
/// (gaussian in elevation). Gates the warm sunrise/sunset cast on ambient and fog.
pub fn horizon_proximity(hours: f32, decl_rad: f32) -> f32 {
    let y = sun_direction(hours, decl_rad).y;
    (-(y / 0.16) * (y / 0.16)).exp()
}

/// Observer latitude (degrees north): sets the celestial pole's elevation above
/// the north horizon. Fixed for the current NL-centric world; planet-scale
/// streaming (ADR-019) can derive it from the region later.
const OBSERVER_LATITUDE_DEG: f32 = 52.0;

/// Diurnal rotation of the celestial sphere for a time of day: maps equatorial
/// directions (+Y at the north celestial pole) into world space. The pole is
/// tilted (90° − latitude) from the zenith toward the north horizon (−Z), and
/// the dome turns at the sun's angular rate in the direction that makes stars
/// rise in the east and set in the west. Everything pinned to the celestial
/// sphere (star field, moon) moves through this one rotation.
pub fn celestial_rotation(hours: f32) -> Quat {
    Quat::from_rotation_x(OBSERVER_LATITUDE_DEG.to_radians() - std::f32::consts::FRAC_PI_2)
        * Quat::from_rotation_y(-day_angle(hours))
}

/// One synodic month — the full new → full → new phase cycle, in days.
pub const LUNAR_CYCLE_DAYS: f32 = 29.53;

/// A known new moon, Unix seconds: 2000-01-06 18:14 UTC. Lunar age counts
/// forward from here modulo the synodic month.
pub const NEW_MOON_UNIX: f64 = 947_182_440.0;

/// Real lunar age in days (0 = new, ~14.8 = full) for a Unix timestamp, so the
/// automatic day/night cycle carries the true moon phase for today's date.
pub fn lunar_age_days(now_unix: f64) -> f32 {
    ((now_unix - NEW_MOON_UNIX) / 86_400.0).rem_euclid(LUNAR_CYCLE_DAYS as f64) as f32
}

/// Phase angle for a lunar age in days: 0 = new (moon beside the sun), π = full.
fn moon_phase_angle(age_days: f32) -> f32 {
    age_days.rem_euclid(LUNAR_CYCLE_DAYS) / LUNAR_CYCLE_DAYS * std::f32::consts::TAU
}

/// Moon direction for a time of day and lunar age. The moon sits on the
/// celestial sphere at declination +18°, and its right ascension advances
/// ~12° per day of age — which *is* the phase mechanism, exactly as in the
/// real sky: at age 0 it travels beside the sun (up by day, dark side toward
/// us), at ~14.8 days it rises at sunset opposite the sun and is up all
/// night, full; moonrise drifts later each day in between.
pub fn moon_direction(hours: f32, age_days: f32) -> Vec3 {
    let dec = 18f32.to_radians();
    // RA 0 culminates at noon beside the sun, so age 0 lines up as new moon.
    let ra = moon_phase_angle(age_days);
    let equatorial = Vec3::new(dec.cos() * ra.cos(), dec.sin(), -(dec.cos() * ra.sin()));
    celestial_rotation(hours) * equatorial
}

/// Illuminated fraction of the lunar disc (0 new → 1 full) for a lunar age.
pub fn moon_illuminated_fraction(age_days: f32) -> f32 {
    (1.0 - moon_phase_angle(age_days).cos()) * 0.5
}

/// Direction of the sunlight falling on the moon: exactly the phase angle away
/// from the moon's direction, tipped toward the real sun's side of the sky so
/// the terminator faces the way you'd expect. (The disc can't be lit by the raw
/// sun direction: our sun rides its own arc rather than the celestial sphere,
/// so at full-moon age the raw geometry would never quite reach a full disc.)
pub fn moon_sunlight_direction(hours: f32, age_days: f32, decl_rad: f32) -> Vec3 {
    let moon = moon_direction(hours, age_days);
    let axis = moon
        .cross(sun_direction(hours, decl_rad))
        .try_normalize()
        .unwrap_or_else(|| moon.any_orthonormal_vector());
    Quat::from_axis_angle(axis, moon_phase_angle(age_days)) * moon
}

/// Ambient sky color (linear RGB, unit scale): deep-blue skyglow at night, warm
/// around sunrise/sunset, near-white with a cool cast at midday (a clear sky is a
/// blue ambient source — it's the sun that is warm). Dawn and dusk differ: the
/// clean morning air gives a rosier, cooler twilight, the hazy evening an amber one.
pub fn ambient_sky_color(hours: f32, decl_rad: f32) -> Vec3 {
    let night = Vec3::new(0.35, 0.45, 0.80);
    let noon = Vec3::new(0.88, 0.93, 1.00);
    let dawn = Vec3::new(0.98, 0.72, 0.60);
    let dusk = Vec3::new(1.00, 0.60, 0.35);
    // The sun's x picks which horizon it sits at: +1 east (morning), −1 west (evening).
    let east = smoothstep(-0.5, 0.5, sun_direction(hours, decl_rad).x);
    let twilight = dusk.lerp(dawn, east);
    let base = night.lerp(noon, daylight(hours, decl_rad));
    base.lerp(twilight, 0.6 * horizon_proximity(hours, decl_rad))
}

#[cfg(test)]
mod tests {
    use super::solar_time_of_day;

    /// A cloud deck sees less air than the ground does, so it keeps more of the
    /// beam — and keeps *disproportionately* more blue, because Rayleigh strips
    /// blue first and there is less Rayleigh left to strip. That colour
    /// difference, not the brightness one, is what makes a sunset deck read as
    /// lit from above rather than as a grey lid.
    #[test]
    fn the_sun_is_brighter_and_bluer_above_the_haze() {
        use super::{sun_transmittance, sun_transmittance_at};

        // Low sun: the long slant path is where altitude buys the most.
        let (hours, decl) = (17.5, 0.0);
        let ground = sun_transmittance(hours, decl);
        let deck = sun_transmittance_at(hours, decl, 2_500.0);

        // Every channel survives better up there.
        assert!(deck.x > ground.x && deck.y > ground.y && deck.z > ground.z);
        // …and the blue end gains proportionally more, so the deck is less red.
        let ground_ratio = ground.z / ground.x;
        let deck_ratio = deck.z / deck.x;
        assert!(
            deck_ratio > ground_ratio,
            "deck b/r {deck_ratio} should exceed ground b/r {ground_ratio}"
        );

        // Sea level must be exactly the old behaviour — this is the same function.
        let sea = sun_transmittance_at(hours, decl, 0.0);
        assert!((sea - ground).length() < 1e-6);
    }

    /// The sun's elevation must carry the season: at the summer solstice the day
    /// is long (sun still well up at 19:00 solar time and past the horizon at
    /// 20:00) and noon is high; at the winter solstice the day is short (sun
    /// already set by 17:00). The old equinox-only arc set every day's sunset at
    /// exactly 18:00, which is what read as "dark far too early" in NL summer.
    #[test]
    fn sun_elevation_tracks_the_season() {
        use super::{solar_declination_rad, sun_direction};
        let summer = solar_declination_rad(172.0); // ~June 21
        let winter = solar_declination_rad(355.0); // ~Dec 21

        // Summer noon is much higher than winter noon (lat 52°: ~61.4° vs ~14.6°).
        assert!(sun_direction(12.0, summer).y > sun_direction(12.0, winter).y + 0.5);

        // 19:00 solar time: sun still up in summer, long set in winter.
        assert!(sun_direction(19.0, summer).y > 0.05);
        assert!(sun_direction(19.0, winter).y < 0.0);

        // 20:00 solar time in summer the disc is still at/above the horizon —
        // the equinox arc had it 30° below by then.
        assert!(sun_direction(20.0, summer).y > -0.02);

        // Sanity: at the equinox (δ≈0) sunrise/sunset land at 06:00/18:00.
        let equinox = solar_declination_rad(80.0); // ~Mar 21
        assert!(sun_direction(6.0, equinox).y.abs() < 0.02);
        assert!(sun_direction(18.0, equinox).y.abs() < 0.02);
    }

    /// Lunar age counts from the reference new moon, wraps at one synodic month,
    /// and reads full at the half-cycle — so the automatic cycle shows a real
    /// phase for the date.
    #[test]
    fn lunar_age_wraps_and_reaches_full() {
        use super::{lunar_age_days, moon_illuminated_fraction, LUNAR_CYCLE_DAYS, NEW_MOON_UNIX};
        let day = 86_400.0;
        assert!(lunar_age_days(NEW_MOON_UNIX) < 1e-3);
        let after = lunar_age_days(NEW_MOON_UNIX + LUNAR_CYCLE_DAYS as f64 * day);
        assert!(after < 1e-2 || (LUNAR_CYCLE_DAYS - after) < 1e-2); // back to new
        let half = lunar_age_days(NEW_MOON_UNIX + (LUNAR_CYCLE_DAYS as f64 / 2.0) * day);
        assert!(moon_illuminated_fraction(half) > 0.999); // full
    }

    /// The sun is overhead at local solar noon: at 12:00 UTC it is noon on the
    /// prime meridian, one hour later per 15° east, one earlier per 15° west,
    /// and the clock wraps across the date line.
    #[test]
    fn solar_time_tracks_longitude() {
        let noon_utc = 12.0 * 3600.0; // 12:00:00 UTC
        let approx = |a: f32, b: f32| (a - b).abs() < 1e-3;
        assert!(approx(solar_time_of_day(noon_utc, 0.0), 12.0));
        assert!(approx(solar_time_of_day(noon_utc, 15.0), 13.0)); // 15°E → +1h
        assert!(approx(solar_time_of_day(noon_utc, -15.0), 11.0)); // 15°W → −1h
        assert!(approx(solar_time_of_day(noon_utc, 180.0), 0.0)); // date line → midnight
                                                                  // Wraps cleanly at UTC midnight on the prime meridian.
        assert!(approx(solar_time_of_day(0.0, 0.0), 0.0));
    }
}
