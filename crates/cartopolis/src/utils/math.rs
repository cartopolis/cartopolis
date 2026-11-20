//! Small arithmetic the client's systems share.

/// Hermite smoothstep: 0 below `e0`, 1 above `e1`, smooth in between. `e0 > e1`
/// runs it backwards (1 below `e1`, 0 above `e0`).
#[must_use]
pub fn smoothstep(e0: f32, e1: f32, x: f32) -> f32 {
    let t = ((x - e0) / (e1 - e0)).clamp(0.0, 1.0);
    t * t * (3.0 - 2.0 * t)
}

/// Where a looping clip is in its loop, 0..1, from the player's seek time.
#[must_use]
pub fn loop_phase(seek_secs: f32, loop_secs: f32) -> f32 {
    if loop_secs <= 0.0 || !seek_secs.is_finite() {
        return 0.0;
    }
    (seek_secs / loop_secs).rem_euclid(1.0)
}
