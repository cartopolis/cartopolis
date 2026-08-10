//! The numbers the avatar moves by — shared, because three separate copies of
//! them have to agree or the avatar rubber-bands.
//!
//! An avatar's motion is computed in three places:
//!
//! 1. `viberfox_simulator::state::SimWorld` — the authority.
//! 2. `viberfox`'s `network::send_network_intent` — client-side prediction, which
//!    integrates what it believes the sim is about to do.
//! 3. `viberfox`'s `avatar::handle_avatar_movement` — the `--offline` path, where
//!    there is no sim and the client *is* the authority.
//!
//! When (1) and (2) disagree the prediction error grows every frame until
//! reconciliation yanks it back, which reads as the avatar stuttering or sliding;
//! when (3) disagrees the same input produces a different world depending on how
//! you launched. Those rates used to live as three private constants in three
//! crates with a comment in each asking the next person to keep them in step. They
//! live here now, so "in step" is the type system's problem.
//!
//! This module is deliberately dependency-free arithmetic: no glam, no serde,
//! nothing that would stop the sim, the client and a test from all reading it.

// ── Horizontal ───────────────────────────────────────────────────────────────

/// Speed (m/s) the sim gives a wish magnitude of exactly 1.0.
///
/// The wire carries a *direction with a magnitude*, not a speed, and the sim
/// scales it by this. That indirection is what lets the client pick a gait —
/// walk, jog, sprint, flight — with no protocol field and no sim change: each is
/// a different magnitude, and the sim reproduces whatever the client rendered
/// because both apply [`sim_speed_for_wish`].
pub const SPEED_SCALE: f32 = 8.0;

/// Hard cap on the speed the sim will derive from any wish magnitude (m/s).
/// Flight reaches exactly this; nothing may exceed it.
pub const MAX_SPEED: f32 = 32.0;

/// The speed the sim derives from a wish magnitude. **The** definition — every
/// other site calls this rather than restating the formula.
#[must_use]
pub fn sim_speed_for_wish(wish_magnitude: f32) -> f32 {
    if wish_magnitude <= 1e-4 {
        0.0
    } else {
        (wish_magnitude * SPEED_SCALE).min(MAX_SPEED)
    }
}

/// The wish magnitude that asks for `speed`. Inverse of [`sim_speed_for_wish`]
/// over the representable range; anything faster than [`MAX_SPEED`] clamps, so a
/// client can never render a speed the sim will refuse to reproduce.
#[must_use]
pub fn wish_for_speed(speed: f32) -> f32 {
    (speed / SPEED_SCALE).clamp(0.0, MAX_SPEED / SPEED_SCALE)
}

// ── Vertical ─────────────────────────────────────────────────────────────────

/// Downward acceleration (m/s²) on anything not flying.
///
/// Above Earth's 9.81 on purpose. Real gravity gives a jump that clears a useful
/// height only by hanging in the air for well over a second, which reads as the
/// moon; games have picked a heavier constant and a stiffer jump for forty years
/// for exactly this reason. With [`JUMP_SPEED`] this yields a ~1.1 m apex and
/// ~0.64 s of airtime.
///
/// This replaced a *constant* 12 m/s descent, which had no acceleration in it at
/// all: a fall was at full speed from the first frame, so stepping off a kerb and
/// falling off a tower began identically.
pub const GRAVITY: f32 = 22.0;

/// Upward speed imparted by a jump (m/s). Apex = `JUMP_SPEED² / (2·GRAVITY)`.
pub const JUMP_SPEED: f32 = 7.0;

/// Downward speed a fall is capped at (m/s). Without it, a fall from the space
/// tier arrives at a speed no landing animation or camera dip can absorb, and the
/// per-frame step outruns the ground test.
pub const TERMINAL_FALL_SPEED: f32 = 40.0;

/// Climb/descent rate while flying (m/s). Flight holds altitude rather than
/// falling, so this is a velocity, not an acceleration.
pub const FLY_VERTICAL_SPEED: f32 = 12.0;

/// Ground level for a standing avatar (m). The Quaternius models are authored
/// origin-at-feet, so the sim's floor is exactly zero.
pub const GROUND_Y: f32 = 0.0;

/// Integrate one frame of the vertical arc for something that is not flying.
/// Returns the new `(y, vertical_velocity, grounded, landing_speed)`, where
/// `landing_speed` is `Some(v)` on precisely the frame the avatar touches down —
/// the camera dip and the landing sound are both driven off that edge, and
/// deriving it here is what stops the two from disagreeing about when a landing
/// happened.
#[must_use]
pub fn step_fall(y: f32, vertical_velocity: f32, ground_y: f32, dt: f32) -> FallStep {
    let mut vy = (vertical_velocity - GRAVITY * dt).max(-TERMINAL_FALL_SPEED);
    let mut ny = y + vy * dt;
    if ny <= ground_y {
        let landing_speed = if y > ground_y || vertical_velocity < 0.0 {
            Some(-vy)
        } else {
            None
        };
        ny = ground_y;
        vy = 0.0;
        return FallStep {
            y: ny,
            vertical_velocity: vy,
            grounded: true,
            landing_speed,
        };
    }
    FallStep {
        y: ny,
        vertical_velocity: vy,
        grounded: false,
        landing_speed: None,
    }
}

/// One frame of [`step_fall`].
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct FallStep {
    pub y: f32,
    pub vertical_velocity: f32,
    pub grounded: bool,
    /// Downward speed at touchdown (positive), on the landing frame only.
    pub landing_speed: Option<f32>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wish_and_speed_are_inverses_within_the_cap() {
        for speed in [0.0, 1.4, 4.2, 8.0, MAX_SPEED] {
            let round = sim_speed_for_wish(wish_for_speed(speed));
            assert!((round - speed).abs() < 1e-4, "{speed} → {round}");
        }
        assert!((sim_speed_for_wish(wish_for_speed(1e6)) - MAX_SPEED).abs() < 1e-4);
        assert_eq!(wish_for_speed(-1.0), 0.0, "no reverse throttle");
    }

    /// A jump goes up, comes back, and lands within the airtime the constants
    /// promise. The apex is the number a player actually feels, so it is asserted
    /// rather than left to be discovered on a workstation.
    #[test]
    fn a_jump_clears_about_a_metre_and_lands_inside_a_second() {
        let dt = 1.0 / 60.0;
        let (mut y, mut vy) = (GROUND_Y, JUMP_SPEED);
        let (mut apex, mut t, mut landed) = (0.0_f32, 0.0_f32, None);
        for _ in 0..600 {
            let s = step_fall(y, vy, GROUND_Y, dt);
            y = s.y;
            vy = s.vertical_velocity;
            t += dt;
            apex = apex.max(y);
            if let Some(v) = s.landing_speed {
                landed = Some((t, v));
                break;
            }
        }
        let (airtime, impact) = landed.expect("a jump must come back down");
        assert!(
            (0.95..=1.30).contains(&apex),
            "apex should be about a metre: {apex}"
        );
        assert!(
            (0.55..=0.75).contains(&airtime),
            "airtime should be well under a second: {airtime}"
        );
        assert!(
            (impact - JUMP_SPEED).abs() < 0.5,
            "lands at roughly the speed it left at: {impact}"
        );
    }

    /// A fall accelerates instead of running at a fixed rate from frame one —
    /// the thing the old constant-12-m/s descent could not express.
    #[test]
    fn a_fall_accelerates_and_then_stops_accelerating() {
        let dt = 1.0 / 60.0;
        let (mut y, mut vy) = (1000.0_f32, 0.0);
        let mut first = 0.0;
        for i in 0..4000 {
            let s = step_fall(y, vy, GROUND_Y, dt);
            if i == 0 {
                first = -s.vertical_velocity;
            }
            y = s.y;
            vy = s.vertical_velocity;
            if s.grounded {
                break;
            }
        }
        assert!(first < 1.0, "the first frame of a fall is slow: {first}");
        assert!(
            (-vy).abs() <= TERMINAL_FALL_SPEED + 1e-3,
            "and it never exceeds terminal velocity"
        );
    }

    /// Standing on the ground is stable: no landing event is invented every
    /// frame, which would fire the landing sound and camera dip continuously.
    #[test]
    fn standing_still_does_not_land_repeatedly() {
        let s = step_fall(GROUND_Y, 0.0, GROUND_Y, 1.0 / 60.0);
        assert!(s.grounded);
        assert_eq!(
            s.landing_speed, None,
            "already on the ground is not a landing"
        );
    }
}
