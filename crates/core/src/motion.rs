//! The numbers the avatar moves by — shared, because three separate copies of
//! them have to agree or the avatar rubber-bands.
//!
//! An avatar's motion is computed in three places:
//!
//! 1. `cartopolis_simulator::state::SimWorld` — the authority.
//! 2. `cartopolis`'s `network::send_network_intent` — client-side prediction, which
//!    integrates what it believes the sim is about to do.
//! 3. `cartopolis`'s `avatar::handle_avatar_movement` — the `--offline` path, where
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
///
/// Raised from 12 on 2026-08-25. The altitude ramp below is what makes flight
/// cover ground, and 12 m/s spent forty seconds getting into the part of it
/// worth being in — long enough that "fly somewhere" read as a chore rather
/// than as travel.
pub const FLY_VERTICAL_SPEED: f32 = 20.0;

/// How hard the vertical thrust builds and sheds (m/s²).
///
/// The climb used to be a *step*: the key went down and the avatar was doing
/// [`FLY_VERTICAL_SPEED`] on that frame, the key came up and it was doing zero
/// on the next. At 12 m/s that was merely abrupt; at 20 m/s scaled by
/// [`fly_altitude_factor`] it is a hundreds-of-metres-per-second discontinuity
/// in the middle of the view, which is most of what "the flight is stuttery"
/// meant. Ramping it is the vertical half of what [`crate::motion`]'s horizontal
/// speed model already does.
///
/// Brisk enough that the ramp is felt as thrust rather than as lag — full climb
/// in half a second — and applied identically by all three motion sites.
pub const FLY_VERTICAL_ACCEL: f32 = 40.0;

/// Step the vertical thrust one frame toward `target`, at [`FLY_VERTICAL_ACCEL`].
///
/// Settles *exactly* on the target rather than approaching it asymptotically:
/// a hover that keeps creeping by a hair rewrites the avatar transform every
/// frame, and under `big_space` that re-propagates the whole model subtree.
///
/// The sim ramps this per 20 Hz tick while the client ramps it per frame, so the
/// two differ during the ~0.5 s a thrust change takes — by at most one tick of
/// one ramp, i.e. well under a metre, which reconciliation absorbs without ever
/// reaching the snap threshold. Putting the ramped value on the wire instead
/// would be a protocol revision to remove an error smaller than the jitter it
/// travels through.
#[must_use]
pub fn step_fly_vertical(current: f32, target: f32, dt: f32) -> f32 {
    let step = FLY_VERTICAL_ACCEL * dt;
    if (target - current).abs() <= step {
        target
    } else if target > current {
        current + step
    } else {
        current - step
    }
}

/// Altitude (m) at which the flight speeds are taken literally; above it they
/// grow in proportion to altitude — the same Google-Earth rule the Fly camera
/// applies (`FREE_SPEED_ALT_REF` over there, deliberately the same number), so
/// how fast the ground slides past stays roughly constant as you climb. Without
/// it, flying the *avatar* up was 12 m/s forever: reaching 1 km took a minute
/// and a half, and at that height the 32 m/s horizontal cap reads as standing
/// still.
pub const FLY_ALT_REF: f32 = 120.0;
/// Ceiling on that scaling — 640 m/s across, 400 m/s of climb, reached by
/// ~2.4 km up. Far below the camera's own ceiling on purpose: an avatar is
/// followed from at most 25 m, so past this the map streams for a viewpoint
/// that outruns it, and a fall from higher than this already exceeds anything
/// [`TERMINAL_FALL_SPEED`] was tuned to land.
pub const FLY_ALT_MAX_FACTOR: f32 = 20.0;

/// How much faster flight is at height `y` (m above the sim's flat ground).
///
/// Applied at *integration* — where a velocity becomes a displacement — never
/// baked into the wish on the wire: the wire's magnitude encoding saturates at
/// [`MAX_SPEED`], so a client that pre-multiplied would clamp, and the sim
/// derives the factor from the authoritative `y` it already holds. All three
/// motion sites (sim, prediction, offline) multiply by this in their flying
/// branch and nowhere else — a walker, a jumper and a faller are below
/// [`FLY_ALT_REF`] anyway, where the factor is exactly 1.
#[must_use]
pub fn fly_altitude_factor(y: f32) -> f32 {
    (y / FLY_ALT_REF).clamp(1.0, FLY_ALT_MAX_FACTOR)
}

/// Ground level for a standing avatar (m). The avatar bodies stand
/// origin-at-feet (each clip is stood on its soles), so the sim's floor
/// is exactly zero.
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

    /// The altitude factor leaves the ground untouched and is capped above:
    /// anything a walker or a jumper can reach multiplies by exactly 1, so the
    /// ramp cannot change grounded movement by a rounding error.
    #[test]
    fn flight_rides_altitude_between_a_floor_and_a_ceiling() {
        for y in [-5.0, 0.0, 1.2, FLY_ALT_REF] {
            assert_eq!(fly_altitude_factor(y), 1.0, "at y={y}");
        }
        assert!((fly_altitude_factor(FLY_ALT_REF * 2.0) - 2.0).abs() < 1e-5);
        assert_eq!(fly_altitude_factor(1e9), FLY_ALT_MAX_FACTOR);
    }

    /// The vertical thrust ramps, settles exactly, and takes about half a second
    /// to reach full climb — the property that turns a key press from a step in
    /// the world velocity into a push.
    #[test]
    fn vertical_thrust_ramps_and_settles_exactly() {
        let dt = 1.0 / 60.0;
        let first = step_fly_vertical(0.0, FLY_VERTICAL_SPEED, dt);
        assert!(
            first > 0.0 && first < FLY_VERTICAL_SPEED,
            "eased, not a step: {first}"
        );

        let (mut v, mut frames) = (0.0_f32, 0);
        while v < FLY_VERTICAL_SPEED && frames < 600 {
            v = step_fly_vertical(v, FLY_VERTICAL_SPEED, dt);
            frames += 1;
        }
        assert_eq!(v, FLY_VERTICAL_SPEED, "settles exactly on the target");
        assert!(
            (0.25..=0.75).contains(&(frames as f32 * dt)),
            "full climb takes about half a second: {frames} frames"
        );

        // …and back to a hover, symmetrically, from either direction.
        let mut down = 0.0_f32;
        for _ in 0..600 {
            down = step_fly_vertical(down, -FLY_VERTICAL_SPEED, dt);
        }
        assert_eq!(down, -FLY_VERTICAL_SPEED);
        for _ in 0..600 {
            down = step_fly_vertical(down, 0.0, dt);
        }
        assert_eq!(down, 0.0, "a released key returns to an exact hover");
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
