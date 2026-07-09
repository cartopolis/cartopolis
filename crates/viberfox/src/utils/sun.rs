//! Sun position math shared by the sky, lighting, window-glow, and fog systems.

use bevy::math::Vec3;

/// Sun-arc angle for a time of day (0–24 h): 0 = sunrise (east), π/2 = noon (up),
/// π = sunset (west), −π/2 = midnight (below).
pub fn day_angle(hours: f32) -> f32 {
    let t = (hours / 24.0).rem_euclid(1.0);
    (t - 0.25) * std::f32::consts::TAU
}

/// Unit direction toward the sun for a time of day.
pub fn sun_direction(hours: f32) -> Vec3 {
    let a = day_angle(hours);
    Vec3::new(a.cos(), a.sin(), 0.2).normalize()
}
