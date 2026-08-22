//! The avatar's speed model: which gait it is in, how fast it accelerates into
//! that gait, and how fast its clip has to play so the feet match the ground.
//!
//! **Everything here is pure**, and that is the point. The rates themselves live
//! one level down in [`cartopolis_core::motion`], shared with the sim so the
//! authority, the prediction and the `--offline` path cannot drift apart; this
//! module is the *client's* half — which gait the keys ask for, how the speed
//! ramps toward it, and which clip has to play how fast to match.
//!
//! # Why the throttle is a magnitude
//!
//! `SimWorld::apply_intent` derives speed from the *magnitude* of the wish
//! vector, so the client already owns the throttle without a protocol field.
//! Stroll, walk and run are therefore three magnitudes, not three messages: the
//! sim needs no gait concept, and prediction stays exact because both ends call
//! `motion::sim_speed_for_wish`. (Flight has always worked this way; it sends a
//! magnitude of 4.)
//!
//! # Why acceleration is in m/s², not a normalized ease
//!
//! An exponential ease on the throttle reaches every gait in the same time, so a
//! run start feels exactly as light as a stroll start. A fixed rate in metres per
//! second squared gives the fast gaits the longer run-up they should have:
//! 1.1 m/s arrives in 0.06 s, 6 m/s takes 0.33 s.

pub use cartopolis_core::motion::{wish_for_speed, MAX_SPEED};

// ── Gaits ────────────────────────────────────────────────────────────────────

/// Alt held: a stroll (m/s). Slow enough to read as ambling rather than
/// travelling.
pub const STROLL_SPEED: f32 = 1.1;
/// **The default, and it is a walk.** It used to be a 4.2 m/s jog, which is
/// above [`GAIT_CLIP_SWITCH`] — so the everyday, no-modifier gait played the
/// `Run` clip, and the only way to see the avatar *walk* was to hold Alt (a key
/// several desktop compositors swallow before winit ever sees it). Pressing W
/// and getting a sprinting figure is the thing that reads as wrong first, so the
/// unmodified key is now an ordinary brisk walk — 6.5 km/h, the `Walk` clip at
/// 1.33×.
pub const WALK_SPEED: f32 = 1.8;
/// Held-Shift run. This used to be 8 m/s, which is a world-class *sprint*: the
/// `Run` clip is authored for half of it, so it hit [`CLIP_SPEED_MAX`] and slid
/// its feet even after the playback rate started tracking the ground. At 6 m/s
/// the clip plays at 1.5× and stays inside the band.
pub const RUN_SPEED: f32 = 6.0;

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
    Stroll,
    /// The default.
    #[default]
    Walk,
    /// Shift held.
    Run,
}

impl Gait {
    /// Target ground speed for this gait (m/s).
    #[must_use]
    pub fn speed(self) -> f32 {
        match self {
            Gait::Stroll => STROLL_SPEED,
            Gait::Walk => WALK_SPEED,
            Gait::Run => RUN_SPEED,
        }
    }

    /// The gait the held modifiers ask for. Run wins a both-held tie: it is the
    /// deliberate, transient one, and a stuck Alt should not silently cap it at
    /// strolling pace.
    #[must_use]
    pub fn from_modifiers(run: bool, stroll: bool) -> Self {
        match (run, stroll) {
            (true, _) => Gait::Run,
            (false, true) => Gait::Stroll,
            (false, false) => Gait::Walk,
        }
    }
}

/// Target ground speed for an analogue push — the on-screen thumbstick
/// (`systems::map_osd`), where `0` is at rest and `1` is full deflection.
///
/// **The same ladder as [`Gait`], climbed continuously instead of in rungs.** A
/// touchscreen has no Shift and no Alt to hold, so a phone that mapped the stick
/// onto a single gait would have exactly one speed; the push carries the choice
/// instead, which is what a proportional stick is *for* and the reason
/// `map_osd`'s reading is a magnitude rather than a direction. The ends are the
/// keyboard's own ends, so neither input can reach a speed the other cannot.
///
/// Note the bottom of the range is not reachable: the stick's deadzone
/// (`map_osd::STICK_DEADZONE`, which keeps a resting thumb from walking the
/// avatar across town) means the smallest real push already asks for a stroll
/// rather than a creep. That is the deadzone's cost and it is paid here
/// rather than hidden — a stick that started at 0.1 m/s would spend its first
/// millimetre of travel on speeds nobody wants.
#[must_use]
pub fn speed_for_throttle(throttle: f32) -> f32 {
    let t = throttle.clamp(0.0, 1.0);
    if t <= 0.0 {
        0.0
    } else {
        STROLL_SPEED + (RUN_SPEED - STROLL_SPEED) * t
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

/// Length of one full `Walk` / `Run` cycle (two footfalls) at 1.0× playback, in
/// seconds. Measured off the glTF samplers (`docs/notes/avatar-game-feel.md`
/// lists the whole 24-clip set) and every character file agrees, which is what
/// makes them constants rather than an asset read.
///
/// They are here, beside the speeds derived from them, because a *second*
/// consumer needs them: `avatar::apply_pose` carries the stride phase across a
/// walk↔run change by mapping one clip's seek time onto the other's cycle. Both
/// clips are two steps long, so the normalized phase means the same thing in
/// each, and the cross-fade lands mid-stride instead of restarting one.
pub const WALK_CLIP_SECS: f32 = 1.333;
pub const RUN_CLIP_SECS: f32 = 0.8;

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
///
/// It also has to sit clear of [`WALK_SPEED`], and that is the load-bearing
/// part: the switch is the one discontinuity in the whole locomotion path, so
/// the *default* gait must not be near it. At 2.4 the old 4.2 m/s default was
/// above the band, meaning a start from rest crossed idle → walk → run inside a
/// quarter of a second and paid two 180 ms cross-fades to do it. Now a walk
/// never reaches the switch at all: one transition per start, and the second one
/// only when Shift is actually held.
pub const GAIT_CLIP_SWITCH: f32 = 3.0;
pub const GAIT_CLIP_HYSTERESIS: f32 = 0.5;

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
    use cartopolis_core::motion::sim_speed_for_wish;

    /// The throttle round-trips through the sim's own formula: what the client
    /// asks for is what the sim computes. This is the single property the whole
    /// no-protocol-change gait split rests on — if it fails, prediction and
    /// authority disagree on speed and the avatar rubber-bands.
    #[test]
    fn every_gait_survives_the_round_trip_through_the_sim() {
        for gait in [Gait::Stroll, Gait::Walk, Gait::Run] {
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
        for gait in [Gait::Stroll, Gait::Walk, Gait::Run] {
            assert!(gait.speed() <= MAX_SPEED, "{gait:?} outruns the sim");
        }
        assert!(STROLL_SPEED < WALK_SPEED && WALK_SPEED < RUN_SPEED);
    }

    /// **The default gait is on the walk clip, with room to spare.** This is the
    /// property the ladder was retuned for: pressing W with nothing held used to
    /// land above `GAIT_CLIP_SWITCH`, so the everyday avatar ran everywhere and
    /// the walk clip was reachable only through Alt. Asserted rather than left to
    /// the constants, because it is a *relation* between two numbers that live in
    /// different sections of this file and either one can be tuned alone.
    #[test]
    fn the_unmodified_gait_is_a_walk() {
        assert!(
            !wants_run_clip(WALK_SPEED, false),
            "the default gait must start on the walk clip"
        );
        assert!(
            !wants_run_clip(WALK_SPEED, true),
            "…and must not stay on the run clip once it has been there"
        );
        assert!(
            wants_run_clip(RUN_SPEED, false),
            "and Shift must actually reach the run clip"
        );
        // Ramping from rest to the default crosses no clip boundary at all, so a
        // start costs one cross-fade rather than two.
        let mut running = false;
        let mut v = 0.0_f32;
        for _ in 0..120 {
            v = advance_speed(v, WALK_SPEED, 1.0 / 60.0, GROUND_ACCEL, GROUND_DECEL);
            running = wants_run_clip(v, running);
            assert!(
                !running,
                "the ramp to a walk touched the run clip at {v} m/s"
            );
        }
    }

    /// No gait is parked on [`CLIP_SPEED_MIN`]/[`CLIP_SPEED_MAX`]. A clamped rate
    /// is foot-slide by definition — the ground moves at a speed the stride was
    /// not scaled to — and the old 8 m/s sprint sat on the ceiling permanently.
    #[test]
    fn no_gait_plays_its_clip_at_the_clamp() {
        for gait in [Gait::Stroll, Gait::Walk, Gait::Run] {
            let speed = gait.speed();
            let reference = if wants_run_clip(speed, false) {
                RUN_CLIP_SPEED
            } else {
                WALK_CLIP_SPEED
            };
            let rate = speed / reference;
            assert!(
                rate > CLIP_SPEED_MIN && rate < CLIP_SPEED_MAX,
                "{gait:?} plays its clip at {rate}×, on the clamp"
            );
        }
    }

    /// Speed ramps rather than stepping, converges from both directions, and
    /// settles exactly — the last part matters because a speed that creeps
    /// forever keeps re-sending intent and re-marking the transform changed.
    #[test]
    fn speed_ramps_and_settles_exactly() {
        let dt = 1.0 / 60.0;
        let mut v = 0.0_f32;
        let first = advance_speed(v, RUN_SPEED, dt, GROUND_ACCEL, GROUND_DECEL);
        assert!(first > 0.0 && first < RUN_SPEED, "eased, not a step");

        let mut frames = 0;
        while v < RUN_SPEED && frames < 600 {
            v = advance_speed(v, RUN_SPEED, dt, GROUND_ACCEL, GROUND_DECEL);
            frames += 1;
        }
        assert_eq!(v, RUN_SPEED, "settles exactly on the target");
        assert!(
            (0.2..=0.6).contains(&(frames as f32 * dt)),
            "a run takes a fraction of a second to wind up: {frames} frames"
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

    /// Strolling arrives far sooner than running. This is the difference a
    /// normalized ease could not express, and the reason the ramp is in m/s².
    #[test]
    fn a_stroll_winds_up_faster_than_a_run() {
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
            to(STROLL_SPEED) * 4 < to(RUN_SPEED),
            "a stroll should start almost immediately next to a run"
        );
    }

    /// The clip choice has hysteresis, so holding a speed on the boundary does
    /// not flip the avatar between walking and running every frame.
    #[test]
    fn clip_choice_does_not_chatter_on_the_boundary() {
        assert!(!wants_run_clip(GAIT_CLIP_SWITCH, false), "walk holds walk");
        assert!(wants_run_clip(GAIT_CLIP_SWITCH, true), "run holds run");
        assert!(
            wants_run_clip(RUN_CLIP_SPEED, false),
            "the run clip's own speed is a run clip"
        );
        assert!(
            !wants_run_clip(STROLL_SPEED, true),
            "a stroll drops to walk"
        );
    }

    /// The whole point: at the speed a clip was authored for, it plays at 1.0×,
    /// and at twice that it plays faster rather than sliding. Checked at the
    /// three gaits, since those are the speeds that actually occur.
    #[test]
    fn clip_speed_tracks_the_ground() {
        assert!((clip_speed_for(WALK_CLIP_SPEED, WALK_CLIP_SPEED) - 1.0).abs() < 1e-6);
        assert!((clip_speed_for(RUN_CLIP_SPEED, RUN_CLIP_SPEED) - 1.0).abs() < 1e-6);

        // A run is faster than the clip's authored speed; it must speed the clip
        // up, not slide the feet — but only as far as the clamp allows.
        let run = clip_speed_for(RUN_SPEED, RUN_CLIP_SPEED);
        assert!(run > 1.0, "a run speeds the clip up: {run}");
        assert!(run <= CLIP_SPEED_MAX);

        // And the band is never left, however absurd the input.
        for s in [0.0, 0.01, 1000.0] {
            let r = clip_speed_for(s, RUN_CLIP_SPEED);
            assert!((CLIP_SPEED_MIN..=CLIP_SPEED_MAX).contains(&r), "{s} → {r}");
        }
    }

    /// The stick spans the keyboard's own range and nothing beyond it: a phone
    /// can reach every speed a desk can, and no speed a desk cannot.
    #[test]
    fn the_throttle_spans_the_gait_ladder() {
        assert_eq!(speed_for_throttle(0.0), 0.0, "a resting thumb stands still");
        assert!((speed_for_throttle(1.0) - RUN_SPEED).abs() < 1e-6);
        // The lightest push past the deadzone is a stroll, not a creep.
        assert!(speed_for_throttle(0.001) >= STROLL_SPEED);
        // Monotone, so the avatar never slows down as the thumb pushes further.
        let mut prev = 0.0;
        for i in 0..=20 {
            let s = speed_for_throttle(i as f32 / 20.0);
            assert!(s >= prev, "push {i}/20 slowed down: {prev} → {s}");
            prev = s;
        }
        // Out-of-band pushes clamp rather than outrunning the sim's cap.
        assert_eq!(speed_for_throttle(4.0), RUN_SPEED);
        assert_eq!(speed_for_throttle(-1.0), 0.0);
        assert!(speed_for_throttle(1.0) <= MAX_SPEED);
    }

    /// Both modifiers held is a run, so a stuck Alt cannot quietly cap one.
    #[test]
    fn running_wins_a_tie() {
        assert_eq!(Gait::from_modifiers(true, true), Gait::Run);
        assert_eq!(Gait::from_modifiers(false, true), Gait::Stroll);
        assert_eq!(Gait::from_modifiers(false, false), Gait::Walk);
        assert_eq!(Gait::default(), Gait::Walk, "the default is a walk");
    }
}
