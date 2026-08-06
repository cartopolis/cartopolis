//! Heading helpers: `atan2` is discontinuous across ±π; keep a continuous heading for replication.

const TAU: f32 = std::f32::consts::TAU;

/// Pick `raw + k·2π` with integer `k` so the result is closest to `prev` on the real line.
/// Works for large accumulated `prev` (unwrapped yaw), unlike a fixed k window.
#[inline]
pub fn snap_yaw_continuation(prev: f32, raw: f32) -> f32 {
    if !prev.is_finite() || !raw.is_finite() {
        return if raw.is_finite() { raw } else { 0.0 };
    }
    let k = ((prev - raw) / TAU).round();
    let mut best = raw + k * TAU;
    for dk in [-1.0f32, 1.0] {
        let c = raw + (k + dk) * TAU;
        if (prev - c).abs() < (prev - best).abs() {
            best = c;
        }
    }
    best
}

/// Wrap angle to (−π, π].
#[inline]
pub fn wrap_angle_pi(mut a: f32) -> f32 {
    use std::f32::consts::PI;
    if !a.is_finite() {
        return 0.0;
    }
    a = a.rem_euclid(TAU);
    if a > PI {
        a -= TAU;
    }
    a
}
