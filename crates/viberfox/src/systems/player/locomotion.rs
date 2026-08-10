//! The avatar's speed model: which gait it is in, how fast it accelerates into
//! that gait, and how fast its clip has to play so the feet match the ground.
//!
//! **Everything here is pure**, and that is the point. The rates themselves live
//! one level down in [`viberfox_core::motion`], shared with the sim so the
//! authority, the prediction and the `--offline` path cannot drift apart; this
//! module is the *client's* half — which gait the keys ask for, how the speed
//! ramps toward it, and which clip has to play how fast to match.
//!
//! # Why the throttle is a magnitude
//!
//! `SimWorld::apply_intent` derives speed from the *magnitude* of the wish
//! vector, so the client already owns the throttle without a protocol field. Walk,
//! jog and sprint are therefore three magnitudes, not three messages: the sim
//! needs no gait concept, and prediction stays exact because both ends call
//! `motion::sim_speed_for_wish`. (Flight has always worked this way; it sends a
//! magnitude of 4.)
//!
//! # Why acceleration is in m/s², not a normalized ease
//!
//! An exponential ease on the throttle reaches every gait in the same time, so a
//! sprint start feels exactly as light as a walk start. A fixed rate in metres per
//! second squared gives the fast gaits the longer run-up they should have: 1.4 m/s
//! arrives in 0.08 s, 8 m/s takes 0.44 s.

pub use viberfox_core::motion::{wish_for_speed, MAX_SPEED};

// ── Gaits ────────────────────────────────────────────────────────────────────

/// Ordinary walking pace (m/s). Roughly 5 km/h.
pub const WALK_SPEED: f32 = 1.4;
/// The default: a jog. Fast enough to cross a city without the map feeling like
/// a treadmill, slow enough to read as a person.
pub const JOG_SPEED: f32 = 4.2;
/// Held-Shift sprint. This used to be the *only* speed, which is why the avatar
/// read as ice-skating: it is a world-class sprint, and the `Run` clip is
/// authored for about half of it.
pub const SPRINT_SPEED: f32 = 8.0;

/// Horizontal flight speed — the sim's cap, reached via the wish magnitude.
pub const FLY_SPEED: f32 = MAX_SPEED;

/// How hard the avatar picks up speed on the ground, and sheds it (m/s²).
/// Deceleration is the brisker of the two; the other way round reads as walking
/// on grease.
pub const GROUND_ACCEL: f32 = 18.0;
pub const GROUND_DECEL: f32 = 26.0;
/// Flight trades the legs for thrust, so it ramps faster than it could ever
/// accelerate on foot but still takes about ⅔ of a second to reach full speed.
pub const FLY_ACCEL: f32 = 45.0;
pub const FLY_DECEL: f32 = 55.0;

/// Which gait the avatar is travelling in. Chosen by held modifiers, not by
/// speed — the speed is the *consequence* (see [`Gait::speed`]).
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub enum Gait {
    /// Alt held: a stroll.
    Walk,
    /// The default.
    #[default]
    Jog,
    /// Shift held.
    Sprint,
}

impl Gait {
    /// Target ground speed for this gait (m/s).
    #[must_use]
    pub fn speed(self) -> f32 {
        match self {
            Gait::Walk => WALK_SPEED,
            Gait::Jog => JOG_SPEED,
            Gait::Sprint => SPRINT_SPEED,
        }
    }

    /// The gait the held modifiers ask for. Sprint wins a both-held tie: it is
    /// the deliberate, transient one, and a stuck Alt should not silently cap a
    /// sprint at walking pace.
    #[must_use]
    pub fn from_modifiers(sprint: bool, walk: bool) -> Self {
        match (sprint, walk) {
            (true, _) => Gait::Sprint,
            (false, true) => Gait::Walk,
            (false, false) => Gait::Jog,
        }
    }
}

/// Step a speed (m/s) toward `target` for one frame at a fixed rate, using the
/// acceleration rate when speeding up and the deceleration rate when slowing.
/// Never overshoots, and settles exactly on the target so a cruising avatar
/// stops rewriting its intent every frame.
#[must_use]
pub fn advance_speed(current: f32, target: f32, dt: f32, accel: f32, decel: f32) -> f32 {
    let rate = if target > current { accel } else { decel };
    let step = rate * dt;
    if (target - current).abs() <= step {
        target
    } else if target > current {
        current + step
    } else {
        current - step
    }
}

// ── Matching the clip to the ground ──────────────────────────────────────────

/// Ground speed the `Walk` clip was authored for (m/s), derived from its cycle:
/// 1.333 s for two steps → 1.5 steps/s at a ~0.9 m stride.
pub const WALK_CLIP_SPEED: f32 = 1.35;
/// Ground speed the `Run` clip was authored for (m/s): a 0.8 s cycle → 2.5
/// steps/s at a ~1.6 m stride.
///
/// This constant is the whole reason the old avatar looked wrong. The `Run` clip
/// played at a hard-coded 1.0× while the avatar travelled at 8 m/s, so the feet
/// moved at half the speed of the ground — the classic ice-skate. Nothing about
/// it was a bug in the animation; the two numbers were simply never related.
pub const RUN_CLIP_SPEED: f32 = 4.0;

/// Speed (m/s) at which the walk clip gives way to the run clip, and the width of
/// the band that keeps the choice from chattering when you hold a speed right on
/// the boundary. Placed above [`WALK_CLIP_SPEED`] and below [`RUN_CLIP_SPEED`],
/// so whichever clip is chosen is being stretched rather than doubled.
pub const GAIT_CLIP_SWITCH: f32 = 2.4;
pub const GAIT_CLIP_HYSTERESIS: f32 = 0.35;

/// Below this ground speed the avatar is standing still as far as animation is
/// concerned. Deliberately above zero: reconciliation and terrain lift leave a few
/// centimetres per second of residue, and an idle avatar must not twitch.
pub const MOVING_EPS: f32 = 0.15;

/// Playback speed is clamped to this band. A clip stretched much past it stops
/// reading as the gait it is — a walk at 0.4× is a mime, a run at 2.5× is a
/// cartoon — so the far ends of the speed range accept a little foot-slide rather
/// than a lot of wrongness.
pub const CLIP_SPEED_MIN: f32 = 0.65;
pub const CLIP_SPEED_MAX: f32 = 1.75;

/// Which locomotion clip suits `speed`, given what is playing now. `running`
/// carries the previous answer so the switch has hysteresis.
#[must_use]
pub fn wants_run_clip(speed: f32, running: bool) -> bool {
    if running {
        speed > GAIT_CLIP_SWITCH - GAIT_CLIP_HYSTERESIS
    } else {
        speed > GAIT_CLIP_SWITCH + GAIT_CLIP_HYSTERESIS
    }
}

/// Playback rate that makes a clip's stride cover the ground actually travelled.
#[must_use]
pub fn clip_speed_for(ground_speed: f32, clip_reference_speed: f32) -> f32 {
    (ground_speed / clip_reference_speed).clamp(CLIP_SPEED_MIN, CLIP_SPEED_MAX)
}

#[cfg(test)]
mod tests {
    use super::*;
    use viberfox_core::motion::sim_speed_for_wish;

    /// The throttle round-trips through the sim's own formula: what the client
    /// asks for is what the sim computes. This is the single property the whole
    /// no-protocol-change gait split rests on — if it fails, prediction and
    /// authority disagree on speed and the avatar rubber-bands.
    #[test]
    fn every_gait_survives_the_round_trip_through_the_sim() {
        for gait in [Gait::Walk, Gait::Jog, Gait::Sprint] {
            let asked = gait.speed();
            let got = sim_speed_for_wish(wish_for_speed(asked));
            assert!(
                (got - asked).abs() < 1e-4,
                "{gait:?}: asked {asked} m/s, sim would move at {got} m/s"
            );
        }
        // …and flight, which rides the same path at the sim's cap.
        let flown = sim_speed_for_wish(wish_for_speed(FLY_SPEED));
        assert!((flown - FLY_SPEED).abs() < 1e-4, "flight: {flown}");
    }

    /// Every gait is inside the range the sim can express, so none of them is
    /// silently clamped into another one.
    #[test]
    fn no_gait_is_clipped_by_the_sim_cap() {
        for gait in [Gait::Walk, Gait::Jog, Gait::Sprint] {
            assert!(gait.speed() <= MAX_SPEED, "{gait:?} outruns the sim");
        }
        assert!(WALK_SPEED < JOG_SPEED && JOG_SPEED < SPRINT_SPEED);
    }

    /// Speed ramps rather than stepping, converges from both directions, and
    /// settles exactly — the last part matters because a speed that creeps
    /// forever keeps re-sending intent and re-marking the transform changed.
    #[test]
    fn speed_ramps_and_settles_exactly() {
        let dt = 1.0 / 60.0;
        let mut v = 0.0_f32;
        let first = advance_speed(v, SPRINT_SPEED, dt, GROUND_ACCEL, GROUND_DECEL);
        assert!(first > 0.0 && first < SPRINT_SPEED, "eased, not a step");

        let mut frames = 0;
        while v < SPRINT_SPEED && frames < 600 {
            v = advance_speed(v, SPRINT_SPEED, dt, GROUND_ACCEL, GROUND_DECEL);
            frames += 1;
        }
        assert_eq!(v, SPRINT_SPEED, "settles exactly on the target");
        assert!(
            (0.3..=0.6).contains(&(frames as f32 * dt)),
            "a sprint takes about half a second to wind up: {frames} frames"
        );

        // …and stopping is the brisker of the two.
        let mut stop = 0;
        while v > 0.0 && stop < 600 {
            v = advance_speed(v, 0.0, dt, GROUND_ACCEL, GROUND_DECEL);
            stop += 1;
        }
        assert_eq!(v, 0.0);
        assert!(stop < frames, "decel outpaces accel: {stop} vs {frames}");
    }

    /// Walking arrives far sooner than sprinting. This is the difference a
    /// normalized ease could not express, and the reason the ramp is in m/s².
    #[test]
    fn a_walk_winds_up_faster_than_a_sprint() {
        let dt = 1.0 / 60.0;
        let to = |target: f32| {
            let (mut v, mut n) = (0.0_f32, 0);
            while v < target && n < 600 {
                v = advance_speed(v, target, dt, GROUND_ACCEL, GROUND_DECEL);
                n += 1;
            }
            n
        };
        assert!(
            to(WALK_SPEED) * 4 < to(SPRINT_SPEED),
            "a stroll should start almost immediately next to a sprint"
        );
    }

    /// The clip choice has hysteresis, so holding a speed on the boundary does
    /// not flip the avatar between walking and running every frame.
    #[test]
    fn clip_choice_does_not_chatter_on_the_boundary() {
        assert!(!wants_run_clip(GAIT_CLIP_SWITCH, false), "walk holds walk");
        assert!(wants_run_clip(GAIT_CLIP_SWITCH, true), "run holds run");
        assert!(wants_run_clip(RUN_CLIP_SPEED, false), "a jog is a run clip");
        assert!(!wants_run_clip(WALK_SPEED, true), "a stroll drops to walk");
    }

    /// The whole point: at the speed a clip was authored for, it plays at 1.0×,
    /// and at twice that it plays faster rather than sliding. Checked at the
    /// three gaits, since those are the speeds that actually occur.
    #[test]
    fn clip_speed_tracks_the_ground() {
        assert!((clip_speed_for(WALK_CLIP_SPEED, WALK_CLIP_SPEED) - 1.0).abs() < 1e-6);
        assert!((clip_speed_for(RUN_CLIP_SPEED, RUN_CLIP_SPEED) - 1.0).abs() < 1e-6);

        // A sprint is twice the run clip's authored speed; it must speed the clip
        // up, not slide the feet — but only as far as the clamp allows.
        let sprint = clip_speed_for(SPRINT_SPEED, RUN_CLIP_SPEED);
        assert!(sprint > 1.0, "sprint speeds the clip up: {sprint}");
        assert!(sprint <= CLIP_SPEED_MAX);

        // And the band is never left, however absurd the input.
        for s in [0.0, 0.01, 1000.0] {
            let r = clip_speed_for(s, RUN_CLIP_SPEED);
            assert!((CLIP_SPEED_MIN..=CLIP_SPEED_MAX).contains(&r), "{s} → {r}");
        }
    }

    /// Both modifiers held is a sprint, so a stuck Alt cannot quietly cap one.
    #[test]
    fn sprint_wins_a_tie() {
        assert_eq!(Gait::from_modifiers(true, true), Gait::Sprint);
        assert_eq!(Gait::from_modifiers(false, true), Gait::Walk);
        assert_eq!(Gait::from_modifiers(false, false), Gait::Jog);
        assert_eq!(Gait::default(), Gait::Jog, "the default is a jog");
    }
}
