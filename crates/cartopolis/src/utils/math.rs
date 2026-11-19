//! Small arithmetic the client's systems share.

/// Hermite smoothstep: 0 below `e0`, 1 above `e1`, smooth in between. `e0 > e1`
/// runs it backwards (1 below `e1`, 0 above `e0`).
#[must_use]
pub fn smoothstep(e0: f32, e1: f32, x: f32) -> f32 {
    let t = ((x - e0) / (e1 - e0)).clamp(0.0, 1.0);
    t * t * (3.0 - 2.0 * t)
}
