//! Sun position and photometry math shared by the sky, lighting, window-glow, and
//! fog systems. The photometry follows the same extinction physics the Nishita sky
//! shader integrates, so lights lit from these curves match what the sky shows.

use bevy::math::{Quat, Vec3};

/// Sun-arc angle for a time of day (0–24 h): 0 = sunrise (east), π/2 = noon (up),
/// π = sunset (west), −π/2 = midnight (below).
pub fn day_angle(hours: f32) -> f32 {
    let t = (hours / 24.0).rem_euclid(1.0);
    (t - 0.25) * std::f32::consts::TAU
}

/// Unit direction toward the sun for a time of day. The constant z tips the whole
/// arc away from the zenith like a mid-latitude sky: noon tops out near 61°
/// elevation (Amsterdam in June) instead of passing overhead, so even midday light
/// arrives at an angle — facades stay modeled and shadows never collapse to dots.
pub fn sun_direction(hours: f32) -> Vec3 {
    let a = day_angle(hours);
    Vec3::new(a.cos(), a.sin(), 0.55).normalize()
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

/// Per-channel atmospheric transmittance of the direct sun beam for a time of day.
/// Encodes both the sun's color (channel ratios) and its dimming (magnitude).
pub fn sun_transmittance(hours: f32) -> Vec3 {
    let depth = RAYLEIGH + AEROSOL * haze(hours);
    (-depth * air_mass(sun_direction(hours).y)).exp()
}

/// Fraction of the solar disc's light clearing the horizon — the soft boundary for
/// the *direct* beam around sunrise/sunset (no hard on/off at 6:00/18:00).
pub fn sun_disc(hours: f32) -> f32 {
    smoothstep(-0.03, 0.06, sun_direction(hours).y)
}

/// Smooth 0–1 daylight factor for sky-driven light (ambient, fog color). Rises
/// through civil twilight (sun still below the horizon) and saturates by
/// mid-morning, because the scattered sky lights the scene well before and after
/// the sun itself crosses the horizon.
pub fn daylight(hours: f32) -> f32 {
    smoothstep(-0.15, 0.40, sun_direction(hours).y)
}

/// How close the sun is to the horizon, 1 at the horizon → 0 away from it
/// (gaussian in elevation). Gates the warm sunrise/sunset cast on ambient and fog.
pub fn horizon_proximity(hours: f32) -> f32 {
    let y = sun_direction(hours).y;
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
pub fn moon_sunlight_direction(hours: f32, age_days: f32) -> Vec3 {
    let moon = moon_direction(hours, age_days);
    let axis = moon
        .cross(sun_direction(hours))
        .try_normalize()
        .unwrap_or_else(|| moon.any_orthonormal_vector());
    Quat::from_axis_angle(axis, moon_phase_angle(age_days)) * moon
}

/// Ambient sky color (linear RGB, unit scale): deep-blue skyglow at night, warm
/// around sunrise/sunset, near-white with a cool cast at midday (a clear sky is a
/// blue ambient source — it's the sun that is warm). Dawn and dusk differ: the
/// clean morning air gives a rosier, cooler twilight, the hazy evening an amber one.
pub fn ambient_sky_color(hours: f32) -> Vec3 {
    let night = Vec3::new(0.35, 0.45, 0.80);
    let noon = Vec3::new(0.88, 0.93, 1.00);
    let dawn = Vec3::new(0.98, 0.72, 0.60);
    let dusk = Vec3::new(1.00, 0.60, 0.35);
    // The sun's x picks which horizon it sits at: +1 east (morning), −1 west (evening).
    let east = smoothstep(-0.5, 0.5, sun_direction(hours).x);
    let twilight = dusk.lerp(dawn, east);
    let base = night.lerp(noon, daylight(hours));
    base.lerp(twilight, 0.6 * horizon_proximity(hours))
}
