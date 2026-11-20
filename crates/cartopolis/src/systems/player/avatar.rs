use crate::components::AvatarHolder;
use crate::components::AvatarModelLoaded;
use crate::components::{Avatar, RemoteAvatar};
use crate::resources::AvatarState;
use crate::systems::human_avatar;
use crate::systems::locomotion;
use crate::systems::user_store::UserStore;
use bevy::animation::graph::{AnimationGraph, AnimationGraphHandle, AnimationNodeIndex};
use bevy::animation::transition::AnimationTransitions;
use bevy::ecs::query::Or;
use bevy::prelude::*;
use cartopolis_core::{motion, wrap_angle_pi, Emote};
use std::time::Duration;

/// Emotes in the order their clips sit in an avatar's table
/// (`human_avatar::EMOTE_CLIPS`), so an [`Emote`] is an index into
/// [`AvatarAnimations::emote_indices`].
const EMOTE_ORDER: [Emote; 9] = [
    Emote::Wave,
    Emote::Interact,
    Emote::Roll,
    Emote::Punch,
    Emote::Kick,
    Emote::Faint,
    Emote::Handshake,
    Emote::HighFive,
    Emote::Throw,
];

/// Where `emote` sits in [`EMOTE_ORDER`].
fn emote_slot(emote: Emote) -> usize {
    EMOTE_ORDER.iter().position(|e| *e == emote).unwrap_or(0)
}

/// Online: blend visual toward authoritative sim position (see `smooth_online_avatar_display`).
const ONLINE_DISPLAY_SMOOTHING: f32 = 14.0;
/// Online prediction reconcile rate: how quickly the locally-predicted display
/// position is corrected toward the authoritative sim position.
///
/// **Deliberately slow, and paired with [`ONLINE_ERROR_FILTER_RATE`].** It was
/// 9/s — brisk, on the reasoning that prediction integrates the same velocity
/// the sim does, so the residual error is small. What that reasoning misses is
/// that the residual error is *noisy*: the reconcile target is rebuilt every
/// time a snapshot lands, and snapshots do not land on a metronome. A snapshot
/// 40 ms late moves the target by 40 ms of travel — 7 cm at a walk, 24 cm at a
/// run — and a correction stiff enough to take that out inside a
/// frame or two feeds the whole of that jitter into the picture, twenty times a
/// second. That is the wobble: not divergence, not the link, just arrival times
/// being turned into motion.
///
/// Together the two rates make a second-order response with a damping ratio of
/// ~0.7 and a corner near 0.8 Hz, so snapshot-rate noise is attenuated by three
/// orders of magnitude while a real disagreement is still taken out inside about
/// a second. The client is therefore a little further from the server's opinion
/// at any instant and much smoother, which is the trade this is for.
const ONLINE_RECONCILE_RATE: f32 = 3.5;
/// How fast the correction *follows the error* (1/s) — the second pole.
///
/// Reconciliation acts on a low-passed error rather than on the raw one, so a
/// snapshot that arrives early or late moves the display by a fraction of its
/// lateness instead of by all of it. Set above [`ONLINE_RECONCILE_RATE`] so the
/// pair stays well damped (see there).
const ONLINE_ERROR_FILTER_RATE: f32 = 7.0;
/// Ceiling on the correction speed, as a fraction of the speed being travelled.
///
/// The filter above removes jitter; this bounds whatever is left. A correction
/// is only ever allowed to be a quarter of the avatar's own pace, so it reads as
/// the motion being slightly off rather than as a tug — the one thing a
/// correction must never look like.
const ONLINE_CORRECTION_FRACTION: f32 = 0.25;
/// …and the floor under it (m/s), so a standing avatar still converges: a
/// fraction of zero would leave a stopped avatar parked beside its own position
/// forever.
///
/// **This number decides how fast a real disagreement closes, and nothing
/// else.** Measured 2026-09-11: the clamp does not bind in the jitter regime at
/// any plausible setting — the worst error arrival noise produces is 6 cm at a
/// walk and 22 m at the top of the (since removed) avatar flight ramp, against a
/// knee (`floor / rate`) of 57 cm and 46 m. The filter above is what removes the wobble; this is only
/// the ceiling on a transient. So it is set by the other end: at 2 m/s a 3 m
/// desync closes in 1.4 s and the worst frame runs at 1.5× walking pace, where
/// 1 m/s took 2.8 s and 3 m/s reached twice walking pace.
const ONLINE_CORRECTION_FLOOR: f32 = 2.0;
/// Cap on how far the authoritative pose is extrapolated while waiting for the
/// next snapshot (see `smooth_online_avatar_display`) — i.e. **how long the
/// client will move on its own prediction with no confirmation**.
///
/// Past the cap the target stops advancing while the display keeps integrating,
/// so reconciliation brakes the display to a halt within `1 / ONLINE_RECONCILE_RATE`
/// seconds and holds it there until the next snapshot lands. That braking is the
/// point — an unconfirmed avatar should stop, not fly on — but the cap decides how
/// often an ordinary hiccup triggers it, and the consequence is proportional to
/// speed: at a walk the avatar loses 20 cm and nobody sees it, at 250 m/s it stops
/// dead and lurches.
///
/// It was 0.15 s, three server ticks, which assumed the only thing between the
/// two ends was a healthy local network. Two ordinary events on a phone exceed it
/// several times a minute and neither is a fault:
///
/// - **The mobile link.** Inter-arrival on cellular routinely spikes to a few
///   hundred milliseconds, and the connection carries chat, prims and voice over
///   one ordered stream, so a single retransmission delays the snapshots queued
///   behind it.
/// - **Our own frame.** `apply_network_snapshot` drains the mailbox once per Bevy
///   frame, so a 200 ms hitch — a tile upload burst — ages every queued snapshot by the length of the
///   hitch before any of them is read. The stall then looks like a network fault
///   from inside this function, and isn't one.
///
/// Half a second covers both while still parking a genuinely dead link inside a
/// second. It is deliberately below `ONLINE_SNAP_SECONDS`-worth of travel: a
/// server that has really stopped agreeing with us surfaces as divergence past
/// the cap and then as a snap, rather than being extrapolated away.
const ONLINE_MAX_EXTRAPOLATION: f32 = 0.5;
/// Prediction error (metres) beyond which the display snaps instead of easing —
/// a teleport, a server correction or a resumed connection, not drift. The floor
/// of [`online_snap_error`]; see there for why it cannot be the whole rule.
const ONLINE_SNAP_ERROR: f32 = 8.0;
/// …and the part of that threshold that has to grow with speed: how many seconds
/// of travel count as ordinary prediction lag rather than as a correction.
///
/// Raised from 0.6 s with the reconcile rate: a gentler correction settles at a
/// larger standing error, and a threshold that the ordinary steady state can
/// reach turns steady travel into alternating snaps — which is the judder
/// this whole band of constants exists to keep out of the picture.
const ONLINE_SNAP_SECONDS: f32 = 1.0;

/// Height above the sim's ground (m) below which the display lift follows the
/// measured ground exactly. Above a jump's apex (`motion::JUMP_SPEED` reaches
/// ~1.1 m) so a hop off a kerb still lands on the kerb.
const LIFT_EXACT_BELOW_M: f32 = 1.5;
/// Height (m) by which the lift has slowed to [`LIFT_GLIDE_RATE`].
const LIFT_GLIDE_ABOVE_M: f32 = 25.0;
/// How fast the lift tracks the ground just above [`LIFT_EXACT_BELOW_M`], and
/// how slowly it drifts once the avatar is properly up (1/s).
const LIFT_TRACK_RATE: f32 = 25.0;
const LIFT_GLIDE_RATE: f32 = 0.3;
/// Height (m) above which the storey state machine stops being stepped. An
/// avatar that high is not standing on a floor, and stepping it at altitude
/// dropped the floor into every tunnel cut it passed over and left it there.
const FLOOR_FREEZE_ABOVE_M: f32 = 2.0;

/// The terrain lift to draw an airborne avatar at, given last frame's and the
/// ground now.
///
/// **The ground under an avatar high in the air is not something it moves
/// with.** The sim is flat and the relief is added on the display side, so the
/// lift used to be the ground height under the avatar on every frame, at any
/// altitude: high over the city the avatar dropped two metres for every canal,
/// rose for every dike, and shivered over kerbs. The follow camera aims at the
/// avatar, so the whole view shook with it. Since avatar flight was removed
/// (2026-11-14) the only way up there is a fall from a height the sim already
/// held (`docs/notes/retired/avatar-flight.md`).
///
/// Exact on the ground and through a jump; above that the lift eases toward the
/// ground at a rate that falls geometrically with height, so a long rise of the
/// land is still followed and a ditch is not. A descent sweeps the rate back up
/// on the way down, so the lift arrives exact before the feet do.
#[must_use]
fn airborne_lift(current: f32, ground: f32, height: f32, dt: f32) -> f32 {
    if height <= LIFT_EXACT_BELOW_M {
        return ground;
    }
    let t =
        ((height - LIFT_EXACT_BELOW_M) / (LIFT_GLIDE_ABOVE_M - LIFT_EXACT_BELOW_M)).clamp(0.0, 1.0);
    let rate = LIFT_TRACK_RATE * (LIFT_GLIDE_RATE / LIFT_TRACK_RATE).powf(t);
    current + (ground - current) * (1.0 - (-rate * dt).exp())
}

/// The ground an avatar at this world-XZ point is drawn on, and whether that is
/// water it swims in.
///
/// In water it is the surface less [`locomotion::TREAD_DEPTH`], the feet of
/// someone treading water — not `ground_at`, which over a canal is the street
/// beside it (the land the quays are stood on), and walked the avatar across
/// the water at street height. The sim is flat and knows nothing of either, so
/// this is a display lift like the terrain's, on every viewer's own map.
#[must_use]
fn ground_or_water(terrain: &crate::systems::terrain::TerrainField, x: f32, z: f32) -> (f32, bool) {
    match terrain.swim_surface_at(x, z) {
        Some(surface) => (surface - locomotion::TREAD_DEPTH, true),
        None => (terrain.ground_at(x, z), false),
    }
}

/// How fast the drawn ground may move as the avatar goes into the water and
/// out of it (m/s): a quay is a metre or two over the water and the feet go
/// another one and a half under it, which as a step would be a jump cut. In,
/// it reads as a drop; out, as climbing up the side.
const WATER_SINK_RATE: f32 = 4.0;
const WATER_CLIMB_RATE: f32 = 2.0;

/// One frame of the lift while in the water or just in or out of it, rate
/// limited by [`WATER_SINK_RATE`] and [`WATER_CLIMB_RATE`].
#[must_use]
fn water_lift(prev: f32, target: f32, dt: f32) -> f32 {
    prev + (target - prev).clamp(-WATER_SINK_RATE * dt, WATER_CLIMB_RATE * dt)
}

/// The error beyond which the display snaps to authority instead of easing.
///
/// **A fixed distance is a speed-dependent time.** Reconciliation trails the
/// target by the latency the extrapolation cap cannot cover, which is
/// proportional to speed; a threshold that ordinary lag can reach hard-sets the
/// display to a stale extrapolation instead of easing, and around the crossing
/// point alternates between the two, which is a judder. Avatar flight at up to
/// 640 m/s is where that showed (`docs/notes/retired/avatar-flight.md`); since
/// the avatar only walks and runs (≤ `motion::MAX_SPEED`, 8 m/s) the floor is
/// what binds, but the rule stays a duration so a faster mover cannot bring the
/// judder back.
///
/// So the threshold is a distance with a *duration* in it: a second of travel
/// is far more than reconciliation's own lag at any speed and far less than any
/// teleport, which moves the avatar to another part of the world; the floor
/// keeps a standing avatar's threshold where it was.
#[must_use]
fn online_snap_error(speed: f32) -> f32 {
    (speed * ONLINE_SNAP_SECONDS).max(ONLINE_SNAP_ERROR)
}

/// The fastest a correction may move the display, at the speed it is travelling.
#[must_use]
fn online_correction_speed(speed: f32) -> f32 {
    (speed * ONLINE_CORRECTION_FRACTION).max(ONLINE_CORRECTION_FLOOR)
}

/// One frame of reconciliation: ease the predicted display toward the
/// authoritative target without letting the target's own arrival noise into the
/// picture.
///
/// `smoothed_error` is this reconciler's memory and belongs to nothing else. The
/// raw error — target minus display — is a jittery signal, because the target is
/// rebuilt whenever a snapshot lands and snapshots arrive when the network says
/// so. Correcting the *filtered* error instead makes the pair a second-order
/// low-pass (see [`ONLINE_RECONCILE_RATE`]), which is what separates "the server
/// disagrees with us" from "that snapshot was 40 ms late".
///
/// Two things deliberately bypass the filter, and they are the two cases where
/// the answer must arrive rather than be eased into:
///
/// - **A snap.** Past [`online_snap_error`] this is a teleport, a server
///   correction or a resumed stream, not drift; easing would slide the avatar
///   across the map instead of putting it where it is.
/// - **A stall.** Once the extrapolation window is spent
///   ([`ONLINE_MAX_EXTRAPOLATION`]) the target stands still on purpose so an
///   unconfirmed avatar brakes to a halt. Filtering and clamping that would make
///   the brake take a second and a half instead of a third of one, and a stall is
///   by definition not jitter.
#[must_use]
fn reconcile_display(
    display: Vec3,
    target: Vec3,
    smoothed_error: &mut Vec3,
    speed: f32,
    stalled: bool,
    dt: f32,
) -> Vec3 {
    let raw = target - display;
    if raw.length() > online_snap_error(speed) {
        *smoothed_error = Vec3::ZERO;
        return target;
    }
    *smoothed_error = if stalled {
        raw
    } else {
        let follow = 1.0 - (-ONLINE_ERROR_FILTER_RATE * dt).exp();
        *smoothed_error + (raw - *smoothed_error) * follow
    };
    let mut step = *smoothed_error * (1.0 - (-ONLINE_RECONCILE_RATE * dt).exp());
    if !stalled {
        step = step.clamp_length_max(online_correction_speed(speed) * dt);
    }
    display + step
}

/// Where the last authoritative pose has got to by now, given how long ago it
/// arrived — the thing reconciliation eases the predicted display toward.
///
/// `AvatarState::position` advances only at the sim's 20 Hz tick, so used raw it
/// is a staircase and the correction drags the display backwards on every frame
/// between two steps. Extrapolating it by its own age compares like with like: the
/// error signal is smooth and reconciliation only corrects real divergence.
///
/// The clamp is where the client stops believing itself
/// ([`ONLINE_MAX_EXTRAPOLATION`]). Past it the target stands still while the
/// display keeps integrating, so the ease brakes the avatar to a halt — deliberate
/// for a link that has actually stopped, and the reason the cap must be generous
/// enough that an ordinary hiccup does not reach it.
///
/// Note it extrapolates by the **predicted** velocity, which is the client's own
/// live input rather than anything the snapshot carried. That is what makes a
/// long window safe: waiting longer means trusting our own keys for longer, not
/// coasting on a stale reading of somebody else's.
#[must_use]
fn extrapolated_target(position: Vec3, predicted_velocity: Vec3, snapshot_age: f32) -> Vec3 {
    position + predicted_velocity * snapshot_age.min(ONLINE_MAX_EXTRAPOLATION)
}

/// Whether a snapshot age has run past the window, i.e. the display is being
/// braked rather than predicted. Counted into
/// [`crate::resources::NetPredictionHealth`], which is the only place this becomes
/// visible to somebody holding the device.
#[must_use]
fn is_stalled(snapshot_age: f32) -> bool {
    snapshot_age >= ONLINE_MAX_EXTRAPOLATION
}
/// Remote rotation should feel a bit snappier than position.
const REMOTE_ROT_SMOOTHING: f32 = 18.0;
/// Replicated ground speed (m/s) above which a remote avatar counts as moving:
/// it walks rather than idles, and an emote in progress is cut short.
const REMOTE_RUN_SPEED_THRESH: f32 = 0.12;

/// Turn rate for the avatar's facing: an exponential ease with a hard cap, so a
/// tap on A snaps crisply to the new heading while an about-face (S) sweeps
/// through the intervening angle in ~0.3 s instead of popping.
const TURN_SMOOTHING: f32 = 16.0;
const MAX_TURN_RATE: f32 = 12.0;

/// Height (m) of the pivot the jump tuck pitches the model about: chest height,
/// so the feet swing back rather than the model sinking through the ground.
const LEAN_PIVOT_Y: f32 = 0.95;

/// How long the arrival takes to play out (seconds).
///
/// A teleport is the one movement in this world with no travel in it: the avatar
/// is at one place on one frame and a hundred kilometres away on the next, and
/// nothing on screen distinguishes that from the map having been redrawn around a
/// figure that never moved. The arrival is what says a body went somewhere.
///
/// Short on purpose. It is played on every jump to a saved place, and anything
/// long enough to be admired is long enough to be waited through the tenth time.
const TELEPORT_ARRIVE_SECS: f32 = 0.45;
/// How thin the avatar is at the start of an arrival, across and along.
///
/// It re-forms out of a vertical column rather than fading in, because the model
/// has no dissolve to fade *with*: the bodies carry opaque materials, and
/// making them transparent for 0.45 s means a second material per part and a
/// place in the transparent sort — for an effect that is over before
/// it can be looked at. A scale is one transform on a holder that is already
/// being written every frame.
const TELEPORT_ARRIVE_PINCH: f32 = 0.06;
/// How tall the column is at the start, in standing heights. Stated rather than
/// derived from [`TELEPORT_ARRIVE_PINCH`]: preserving volume would want ~280×,
/// which is a light-beam to the stratosphere and not a person.
const TELEPORT_ARRIVE_COLUMN: f32 = 2.4;
/// …and how far it overshoots standing height on the way in, so the body settles
/// rather than stopping dead at 1.0.
const TELEPORT_ARRIVE_STRETCH: f32 = 1.2;

/// Peak pitch of the jump tuck (~11°), reached at full ascent speed and mirrored
/// to a backward opening at full descent. Small on purpose: the clip underneath
/// is still `Idle`, and a large angle turns a jump into a dive.
const JUMP_TUCK: f32 = 0.2;
/// The jump/fall pose has to keep up with a 0.64 s arc, so it eases fast.
const JUMP_TUCK_SMOOTHING: f32 = 18.0;

/// Horizontal forward from orbit [`CameraState::azimuth`] (into the screen / away from camera).
#[inline]
pub(crate) fn camera_plane_forward(azimuth: f32) -> Vec3 {
    Vec3::new(-azimuth.sin(), 0., -azimuth.cos())
}

#[inline]
pub(crate) fn camera_plane_right(azimuth: f32) -> Vec3 {
    Vec3::new(azimuth.cos(), 0., -azimuth.sin())
}

/// Normalized camera-relative WASD on XZ, or zero.
pub(crate) fn wish_dir_camera_relative(
    azimuth: f32,
    move_forward: bool,
    move_backward: bool,
    move_left: bool,
    move_right: bool,
) -> Vec3 {
    let mut w = Vec3::ZERO;
    let f = camera_plane_forward(azimuth);
    let r = camera_plane_right(azimuth);
    if move_forward {
        w += f;
    }
    if move_backward {
        w -= f;
    }
    if move_right {
        w += r;
    }
    if move_left {
        w -= r;
    }
    let l2 = w.length_squared();
    if l2 > 1e-8 {
        w / l2.sqrt()
    } else {
        Vec3::ZERO
    }
}

/// The travel direction and the throttle it carries, from the keys and the
/// on-screen thumbstick (`systems::map_osd`) together.
///
/// Returns a unit direction on XZ — or zero for "no input" — and a `0..=1`
/// throttle. The keys are all-or-nothing and answer `1.0`; the stick is
/// proportional and answers its own push, which is the whole difference between
/// a stick and four keys (`locomotion::speed_for_throttle` turns it into a
/// speed).
///
/// **Keys win when both are live**, and the tie is not hypothetical: a desktop
/// touchscreen draws the stick *and* has a keyboard. Blending them would let a
/// thumb resting inside the deadzone quietly cap a sprint — the same failure
/// `Gait::from_modifiers` avoids by letting Run win its tie.
pub(crate) fn travel_wish(azimuth: f32, keys: Vec3, stick: Vec2) -> (Vec3, f32) {
    if keys.length_squared() > 1e-8 {
        return (keys, 1.0);
    }
    let push = stick.length();
    if push <= 1e-4 {
        return (Vec3::ZERO, 0.0);
    }
    let unit = stick / push;
    // Same frame as the keys: `y` is forward (up-screen), `x` strafes right.
    let dir = camera_plane_forward(azimuth) * unit.y + camera_plane_right(azimuth) * unit.x;
    (dir.normalize_or_zero(), push.min(1.0))
}

/// How far (cosine) the held direction may turn before it counts as a new press.
const STEER_RELATCH_COS: f32 = 0.94; // ~20°

/// The camera bearing the held movement input is read against.
///
/// The camera's own azimuth, less whatever it has turned **by itself** since the
/// input was pressed (`CameraState::auto_yaw`). Without that the follow camera
/// recentring behind a sideways walk would rotate the frame the keys are read
/// in, the avatar would turn to follow, the camera would follow that — and
/// holding A would walk a circle (Nesky's "50 Game Camera Mistakes" #18–19). A
/// hand on the camera still steers, since only the automatic part is held back;
/// letting go, or changing direction by more than [`STEER_RELATCH_COS`], reads
/// the keys against the camera as it is now.
#[must_use]
fn steer_azimuth(latch: &mut Option<(f32, Vec2)>, azimuth: f32, auto_yaw: f32, held: Vec2) -> f32 {
    let Some(dir) = held.try_normalize() else {
        *latch = None;
        return azimuth;
    };
    match latch {
        Some((mark, was)) if was.dot(dir) >= STEER_RELATCH_COS => azimuth - (auto_yaw - *mark),
        _ => {
            *latch = Some((auto_yaw, dir));
            azimuth
        }
    }
}

/// World yaw that turns the model's forward (+Z) onto `dir` on the XZ plane, or
/// `None` for no input — an avatar standing still keeps the heading it has rather
/// than spinning with the camera.
#[inline]
pub(crate) fn yaw_from_dir(dir: Vec3) -> Option<f32> {
    if dir.length_squared() < 1e-8 {
        None
    } else {
        Some(wrap_angle_pi(f32::atan2(dir.x, dir.z)))
    }
}

/// Step the avatar's yaw toward `target` for one frame: the avatar turns to face
/// where it is walking, in every direction. It used to be pinned to camera-forward,
/// which made A/D a sideways moonwalk and S a backwards one.
pub(crate) fn advance_facing(current: f32, target: f32, dt: f32) -> f32 {
    let delta = wrap_angle_pi(target - current);
    // Settle exactly, or the ease creeps forever and keeps rewriting the transform.
    if delta.abs() < 1e-3 {
        return wrap_angle_pi(target);
    }
    let ease = delta * (1.0 - (-TURN_SMOOTHING * dt).exp());
    let cap = MAX_TURN_RATE * dt;
    wrap_angle_pi(current + ease.clamp(-cap, cap))
}

/// How fast the travel direction may turn (rad/s): sharp at a walk, where a
/// person turns within a step, wide at a run, where they lean round a curve,
/// and slow in the water.
const TRAVEL_TURN_WALK: f32 = 9.0;
const TRAVEL_TURN_RUN: f32 = 3.5;
const TRAVEL_TURN_SWIM: f32 = 1.8;
/// The least of the run speed a sharp turn brakes to.
const TURN_BRAKE_FLOOR: f32 = 0.3;

/// [`TRAVEL_TURN_WALK`] up to a walk, easing to [`TRAVEL_TURN_RUN`] at a run.
#[must_use]
fn travel_turn_rate(speed: f32, swimming: bool) -> f32 {
    if swimming {
        return TRAVEL_TURN_SWIM;
    }
    let t = ((speed - locomotion::WALK_SPEED) / (locomotion::RUN_SPEED - locomotion::WALK_SPEED))
        .clamp(0.0, 1.0);
    TRAVEL_TURN_WALK + (TRAVEL_TURN_RUN - TRAVEL_TURN_WALK) * t
}

/// Turn the unit travel direction `current` toward the unit `wish` on the XZ
/// plane by at most `max_angle` radians, landing exactly on it once within
/// reach. A wish dead behind turns the way the body is already turning
/// (either is right; the same one every frame is what matters).
#[must_use]
fn turn_travel(current: Vec3, wish: Vec3, max_angle: f32) -> Vec3 {
    let (Some(from), Some(to)) = (yaw_from_dir(current), yaw_from_dir(wish)) else {
        return wish;
    };
    let delta = wrap_angle_pi(to - from);
    if delta.abs() <= max_angle {
        return wish;
    }
    let yaw = from + max_angle.copysign(delta);
    Vec3::new(yaw.sin(), 0.0, yaw.cos())
}

/// Standing height on flat ground: the sim's floor (`motion::GROUND_Y`), where
/// the ground sheets are drawn (within a centimetre of the plane,
/// `GroundLayer::y_offset`). The avatar models are authored origin-at-feet, so
/// this *is* the resting height — not a half-height offset. It was 0.05, the
/// top of a region tile long gone, which stood every offline avatar — and every
/// capture of one — 5 cm in the air.
///
/// Public because collision's snap-out backstop has to reconstruct the same
/// ground the movement path stands on (`GROUND_HEIGHT + terrain.ground_at(..)`), and a
/// second copy of the number there is exactly the drift
/// `docs/notes/avatar-standing-height.md` is about.
pub const GROUND_HEIGHT: f32 = cartopolis_core::motion::GROUND_Y;

/// Animation data attached to an avatar (and each of its model "holder" children),
/// read once a scene is ready. `Clone` so the body and outfit holders can share it.
#[derive(Component, Clone)]
pub(crate) struct AvatarAnimations {
    graph_handle: Handle<AnimationGraph>,
    idle_index: AnimationNodeIndex,
    /// The stroll. Loading it is what lets the avatar have a slow gait at all —
    /// the client played `Run` at every speed before, which is why the feet slid.
    walk_index: AnimationNodeIndex,
    run_index: AnimationNodeIndex,
    /// One node per [`EMOTE_ORDER`] entry, in that order.
    emote_indices: Vec<AnimationNodeIndex>,
    /// The seated pose: a motion-capture loop of someone sitting still.
    sit_index: AnimationNodeIndex,
    /// The breaststroke, lying face down (CMU 125_01); treading water is the idle.
    swim_index: AnimationNodeIndex,
    /// The step up onto a wall (CMU 15_02, `systems::climb`), played once.
    climb_index: AnimationNodeIndex,
    /// Sitting on a high edge with the legs hanging (CMU 14_29).
    dangle_index: AnimationNodeIndex,
    /// Base playback speed for this model's clips. The locomotion clips are
    /// scaled on top of this to match ground speed.
    speed: f32,
    /// The walk and run clips' timing: each captured cycle's own length and speed.
    walk: ClipGait,
    run: ClipGait,
    /// How long each emote holds the body, in [`EMOTE_ORDER`]: the clip's own
    /// length, or [`EMOTE_DURATION`] for one the pack lacks.
    emote_secs: Vec<f32>,
    /// One stroke of the swim loop, seconds (the clip's own length).
    swim_secs: f32,
    /// How this body sits on its own legs, for a seat on an edge
    /// (`systems::climb::SeatFit`).
    seat_fit: crate::systems::climb::SeatFit,
}

/// A one-shot emote currently playing on an avatar. Present only while it runs;
/// [`update_avatar_animation`] removes it when the clip is done or movement
/// interrupts it.
#[derive(Component)]
pub(crate) struct EmotePlayback {
    emote: Emote,
    remaining: f32,
}

impl EmotePlayback {
    /// The emote playing — what the street crowd answers a wave by
    /// (`systems::crowd_react`).
    pub(crate) fn emote(&self) -> Emote {
        self.emote
    }
}

/// How long an emote holds the body before locomotion takes over again. The clips
/// run 1.27–1.67 s; a single duration for all of them is enough because the
/// transition back cross-fades anyway, and it saves reading every clip's length
/// out of the asset at runtime.
const EMOTE_DURATION: f32 = 1.6;

/// Cross-fade time between locomotion clips. Long enough to hide the pop that
/// `stop`/`play` used to make on every start and stop, short enough that the
/// avatar does not appear to change its mind — a gait change is a decision, not a
/// drift.
const GAIT_FADE: Duration = Duration::from_millis(180);
/// Sitting down is slower than either: it is a whole body lowering itself onto
/// something, and at a gait's fade the avatar snaps into the seat. Paired with
/// [`SEAT_SLIDE_SECS`], which moves it the last half-metre over the same time.
const SIT_FADE: Duration = Duration::from_millis(450);
/// Lying down into the stroke and coming upright to tread water: the whole body
/// turns through a right angle, so slower still. Paired with [`SWIM_LIE_SECS`],
/// which lifts the body to the surface over the same time.
const SWIM_FADE: Duration = Duration::from_millis(600);
const SWIM_LIE_SECS: f32 = 0.6;
/// Emotes cut in faster: a wave that eases in over a fifth of a second reads as
/// reluctance.
const EMOTE_FADE: Duration = Duration::from_millis(90);

pub fn spawn_avatar(
    mut commands: Commands,
    mut graphs: ResMut<Assets<AnimationGraph>>,
    mut clip_assets: ResMut<Assets<AnimationClip>>,
    store: Res<UserStore>,
    mut avatar_query: Query<
        (Entity, Has<Avatar>, &mut Transform, Option<&RemoteAvatar>),
        (
            Without<AvatarModelLoaded>,
            Or<(With<Avatar>, With<RemoteAvatar>)>,
        ),
    >,
    mut human: human_avatar::HumanAssets,
    mut told_failed: Local<bool>,
) {
    // Bodies built this frame. One at a time: a build is a few milliseconds of
    // shaping and fitting and then every mesh uploaded, so a crowd arriving at
    // once is built over a few frames, not in one.
    let mut built = 0u32;
    for (entity, is_local, mut transform, remote) in avatar_query.iter_mut() {
        if human.failed() {
            // There is no other body to fall back on since the characters were
            // retired: say so once, loudly, and stop asking.
            if !*told_failed {
                tracing::error!("avatar: the body pack did not load; avatars are not drawn");
                *told_failed = true;
            }
            commands.entity(entity).try_insert(AvatarModelLoaded);
            continue;
        }
        // Not loaded yet, or this frame's build spent: leave the avatar
        // unmarked and come back next frame.
        let Some(pack) = human.pack() else { continue };
        if built >= 1 {
            continue;
        }
        built += 1;
        // The local avatar wears its own look (or the designer's draft);
        // another wears the look the server sent (protocol 42).
        let (look, tints) = match remote {
            Some(r) => human.look_of_remote(r.sim_id),
            None => human.look(&store.data.appearance),
        };
        let spawned = human_avatar::spawn_human(
            &mut commands,
            &mut human,
            &mut clip_assets,
            human_avatar::prepare_human(&pack, &look),
            tints,
            entity,
            AvatarHolder,
            false,
        );
        let anims = animate_holder(&mut commands, &mut graphs, spawned, None);
        transform.scale = Vec3::ONE;
        // `try_insert`: the query includes remote avatars, which
        // `session::teardown` despawns wholesale when a link drops, and nothing
        // orders the two systems.
        commands
            .entity(entity)
            .try_insert((AvatarModelLoaded, anims));
        tracing::debug!(local = is_local, "avatar: body built");
    }
}

/// Give a spawned body's holder its animation graph and player, and return
/// the table its avatar carries.
///
/// `from` is the body it replaces (`human_avatar::finish_rebuilds`): the new
/// one starts on the same clip at the same point, playing at the same speed,
/// so a rebuild in mid-stride carries on the stride rather than snapping to
/// the clip's first frame. Both graphs are built from `CLIP_ORDER`, so a node
/// index means the same clip in each.
pub(crate) fn animate_holder(
    commands: &mut Commands,
    graphs: &mut Assets<AnimationGraph>,
    spawned: human_avatar::SpawnedHuman,
    from: Option<(&AnimationPlayer, &AnimationTransitions)>,
) -> AvatarAnimations {
    let (graph, indices) = AnimationGraph::from_clips(spawned.clips);
    let anims = AvatarAnimations {
        graph_handle: graphs.add(graph),
        idle_index: indices[0],
        walk_index: indices[1],
        run_index: indices[2],
        emote_indices: human_avatar::EMOTE_CLIPS
            .iter()
            .map(|&i| indices[i])
            .collect(),
        sit_index: indices[9],
        swim_index: indices[10],
        climb_index: indices[14],
        dangle_index: indices[15],
        speed: 1.0,
        walk: spawned.walk,
        run: spawned.run,
        emote_secs: spawned.emote_secs,
        swim_secs: spawned.swim_secs,
        seat_fit: spawned
            .seat_fit
            .map_or_else(crate::systems::climb::SeatFit::default, Into::into),
    };
    let mut player = AnimationPlayer::default();
    let mut transitions = AnimationTransitions::new();
    let playing = from.and_then(|(player, transitions)| {
        let node = transitions.get_main_animation()?;
        let active = player.animation(node)?;
        Some((
            node,
            active.repeat_mode(),
            active.speed(),
            active.seek_time(),
        ))
    });
    match playing {
        Some((node, repeat, speed, seek)) => {
            transitions
                .play(&mut player, node, Duration::ZERO)
                .set_repeat(repeat)
                .set_speed(speed)
                .set_seek_time(seek);
        }
        None => {
            transitions
                .play(&mut player, anims.idle_index, Duration::ZERO)
                .repeat();
        }
    }
    commands.entity(spawned.holder).insert((
        anims.clone(),
        player,
        transitions,
        AnimationGraphHandle(anims.graph_handle.clone()),
    ));
    anims
}

/// What an avatar's body is doing this frame: which clip, how fast, and whether
/// it loops. Derived from ground speed rather than from a boolean, which is the
/// whole of the fix — the clip used to be chosen by "is there input" and played at
/// a fixed 1.0×, so the legs ran at the animator's speed while the ground moved at
/// whatever the sim said.
#[derive(Clone, Copy, PartialEq)]
struct Pose {
    clip: PoseClip,
    /// Playback rate multiplier, before the model's own `anim_speed`.
    rate: f32,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum PoseClip {
    Idle,
    Walk,
    Run,
    /// Seated (a motion-capture loop).
    Sit,
    /// Swimming the breaststroke. Treading water still is [`PoseClip::Idle`].
    Swim,
    /// Climbing onto a wall: played once, while the avatar is carried onto it.
    Climb,
    /// Seated on a high edge, the legs hanging.
    Dangle,
    Emote(usize),
}

/// One locomotion clip's timing: its cycle (two footfalls) and the ground
/// speed it was made at, which the playback rate is scaled against.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct ClipGait {
    pub secs: f32,
    pub speed: f32,
}

impl ClipGait {
    /// The gait a body without its own walk or run clip falls back to.
    pub(crate) const CHARACTER_WALK: Self = Self {
        secs: locomotion::WALK_CLIP_SECS,
        speed: locomotion::WALK_CLIP_SPEED,
    };
    pub(crate) const CHARACTER_RUN: Self = Self {
        secs: locomotion::RUN_CLIP_SECS,
        speed: locomotion::RUN_CLIP_SPEED,
    };

    /// Footfalls per second at `ground_speed`: two per cycle, at the playback
    /// rate that speed gives the clip — so a footstep sound counted at this rate
    /// stays on the foot (`soundscape`).
    pub(crate) fn steps_per_sec(self, ground_speed: f32) -> f32 {
        2.0 * locomotion::clip_speed_for(ground_speed, self.speed) / self.secs
    }
}

/// Choose the locomotion pose for a ground speed. `running` is the previous
/// answer, for the hysteresis in `locomotion::wants_run_clip`; `gaits` are the
/// model's walk and run.
fn locomotion_pose(speed: f32, running: bool, gaits: (ClipGait, ClipGait)) -> Pose {
    if speed < locomotion::MOVING_EPS {
        return Pose {
            clip: PoseClip::Idle,
            rate: 1.0,
        };
    }
    if locomotion::wants_run_clip(speed, running) {
        Pose {
            clip: PoseClip::Run,
            rate: locomotion::clip_speed_for(speed, gaits.1.speed),
        }
    } else {
        Pose {
            clip: PoseClip::Walk,
            rate: locomotion::clip_speed_for(speed, gaits.0.speed),
        }
    }
}

/// The body in the water at `speed`: the stroke, played at the pace it carries,
/// or treading water upright — the idle, with the body under the surface — when
/// still.
fn swim_pose(speed: f32) -> Pose {
    if speed < locomotion::MOVING_EPS {
        return Pose {
            clip: PoseClip::Idle,
            rate: 1.0,
        };
    }
    Pose {
        clip: PoseClip::Swim,
        rate: (speed / locomotion::SWIM_CLIP_SPEED).clamp(0.6, 1.6),
    }
}

impl AvatarAnimations {
    /// How this body sits on its own legs.
    pub(crate) fn seat_fit(&self) -> crate::systems::climb::SeatFit {
        self.seat_fit
    }

    /// This model's walk and run.
    pub(crate) fn gaits(&self) -> (ClipGait, ClipGait) {
        (self.walk, self.run)
    }

    /// Where in its stroke this body's swim clip is, 0..1, when the stroke is
    /// the clip playing (the main animation of a holder's player); `None`
    /// treading water or on land. What a stroke's splash and sound are timed
    /// by (`systems::splashes`), so they land on the arms, not on a count.
    pub(crate) fn swim_phase(
        &self,
        player: &AnimationPlayer,
        transitions: &AnimationTransitions,
    ) -> Option<f32> {
        if transitions.get_main_animation() != Some(self.swim_index) {
            return None;
        }
        let seek = player.animation(self.swim_index)?.seek_time();
        Some(crate::utils::math::loop_phase(seek, self.swim_secs))
    }

    /// One cycle of `node` in seconds, for the two clips whose phase means the
    /// same thing (both `Walk` and `Run` are two footfalls long). `None` for
    /// everything else — idle, sitting and the emotes have no stride to carry.
    fn cycle_secs(&self, node: AnimationNodeIndex) -> Option<f32> {
        if node == self.walk_index {
            Some(self.walk.secs)
        } else if node == self.run_index {
            Some(self.run.secs)
        } else {
            None
        }
    }

    fn emote_secs(&self, emote: Emote) -> f32 {
        self.emote_secs
            .get(emote_slot(emote))
            .copied()
            .unwrap_or(EMOTE_DURATION)
    }

    fn node(&self, clip: PoseClip) -> AnimationNodeIndex {
        match clip {
            PoseClip::Idle => self.idle_index,
            PoseClip::Walk => self.walk_index,
            PoseClip::Run => self.run_index,
            PoseClip::Sit => self.sit_index,
            PoseClip::Swim => self.swim_index,
            PoseClip::Climb => self.climb_index,
            PoseClip::Dangle => self.dangle_index,
            PoseClip::Emote(slot) => self
                .emote_indices
                .get(slot)
                .copied()
                .unwrap_or(self.idle_index),
        }
    }
}

/// Drive one player to `pose`, cross-fading if the clip changed and only
/// adjusting the rate if it didn't.
///
/// The rate is re-applied every frame *without* restarting anything: it tracks a
/// continuously ramping ground speed, so it changes on almost every frame of an
/// acceleration, and calling `play` for that would restart the stride each time.
///
/// # The stride survives a gait change
///
/// `AnimationTransitions::play` starts the incoming clip at seek time **zero**,
/// which for a locomotion clip means "both feet together, about to lead with the
/// left". A walk→run change therefore cross-faded a mid-stride pose into a
/// standing-start one: for the 180 ms of [`GAIT_FADE`] the legs interpolated
/// toward a position they had no business being in, which is the hitch you see
/// the instant Shift goes down or comes up. Nothing is wrong with either clip —
/// they are simply spliced at an arbitrary point in each other's cycle.
///
/// Both clips are exactly two footfalls long ([`locomotion::WALK_CLIP_SECS`] /
/// [`locomotion::RUN_CLIP_SECS`]), so a *normalized* phase is directly
/// transferable: seek the incoming clip to the same fraction of its own cycle
/// and the cross-fade blends left foot into left foot. Idle, sitting and the
/// emotes carry no stride and are started from zero as before.
fn apply_pose(
    player: &mut AnimationPlayer,
    transitions: &mut AnimationTransitions,
    anim: &AvatarAnimations,
    pose: Pose,
) {
    let want = anim.node(pose.clip);
    let repeat = !matches!(pose.clip, PoseClip::Emote(_) | PoseClip::Climb);
    if transitions.get_main_animation() != Some(want) {
        // Where in its cycle the outgoing clip is, as a fraction — read before
        // `play` retires it. `None` whenever either side has no stride.
        let phase = transitions
            .get_main_animation()
            .zip(anim.cycle_secs(want))
            .and_then(|(previous, _)| {
                let secs = anim.cycle_secs(previous)?;
                let active = player.animation(previous)?;
                Some((active.seek_time() / secs).fract())
            });
        let main = transitions.get_main_animation();
        let fade = if main == Some(anim.climb_index) {
            // Out of the climb at once. It ends standing on the step it climbed,
            // its hips the step's height up, and the idle after it stands on the
            // top the floor has just been raised to: a cross-fade between the two
            // would sink the body by the step and lift it back.
            Duration::ZERO
        } else if matches!(pose.clip, PoseClip::Sit | PoseClip::Dangle)
            || main == Some(anim.sit_index)
            || main == Some(anim.dangle_index)
        {
            // Both ways, so getting up takes as long as sitting down did.
            SIT_FADE
        } else if pose.clip == PoseClip::Swim || main == Some(anim.swim_index) {
            SWIM_FADE
        } else if repeat {
            GAIT_FADE
        } else {
            EMOTE_FADE
        };
        let active = transitions.play(player, want, fade);
        if repeat {
            active.repeat();
        }
        if let Some((phase, secs)) = phase.zip(anim.cycle_secs(want)) {
            // `set_seek_time`, not `seek_to`: the latter records the jump as a
            // range to fire clip events over, and this jump is a splice rather
            // than playback.
            active.set_seek_time(phase * secs);
        }
    }
    if let Some(active) = player.animation_mut(want) {
        active.set_speed(anim.speed * pose.rate);
    }
}

/// A clip by the name a command line gives it (`--clip walk`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ClipName {
    Idle,
    Walk,
    Run,
    Sit,
    Swim,
    Climb,
    Dangle,
    Emote(Emote),
}

impl ClipName {
    /// Every name [`ClipName::parse`] accepts, for the error that refuses one.
    pub const NAMES: [&'static str; 16] = [
        "idle",
        "walk",
        "run",
        "sit",
        "swim",
        "climb",
        "dangle",
        "wave",
        "interact",
        "roll",
        "punch",
        "kick",
        "faint",
        "handshake",
        "highfive",
        "throw",
    ];

    #[must_use]
    pub fn parse(name: &str) -> Option<Self> {
        Some(match name.trim().to_ascii_lowercase().as_str() {
            "idle" => Self::Idle,
            "walk" => Self::Walk,
            "run" => Self::Run,
            "sit" => Self::Sit,
            "swim" => Self::Swim,
            "climb" => Self::Climb,
            "dangle" => Self::Dangle,
            "wave" => Self::Emote(Emote::Wave),
            "interact" => Self::Emote(Emote::Interact),
            "roll" => Self::Emote(Emote::Roll),
            "punch" => Self::Emote(Emote::Punch),
            "kick" => Self::Emote(Emote::Kick),
            "faint" => Self::Emote(Emote::Faint),
            "handshake" => Self::Emote(Emote::Handshake),
            "highfive" | "high-five" => Self::Emote(Emote::HighFive),
            "throw" => Self::Emote(Emote::Throw),
            _ => return None,
        })
    }

    fn pose_clip(self) -> PoseClip {
        match self {
            Self::Idle => PoseClip::Idle,
            Self::Walk => PoseClip::Walk,
            Self::Run => PoseClip::Run,
            Self::Sit => PoseClip::Sit,
            Self::Swim => PoseClip::Swim,
            Self::Climb => PoseClip::Climb,
            Self::Dangle => PoseClip::Dangle,
            Self::Emote(e) => PoseClip::Emote(emote_slot(e)),
        }
    }
}

/// A scripted capture's hold on the local avatar's animation (`--clip`): play
/// `clip`, **frozen** at `phase` of its length, so a run of captures can step
/// through one cycle a frame at a time (`shot_harness`'s clip contact sheet).
///
/// Frozen rather than left playing because the harness's frames are not the
/// clip's: a capture waits on streaming and on a readback for a number of frames
/// nobody chooses, so a playing clip would be photographed at arbitrary points.
#[derive(Resource, Clone, Copy, Debug, PartialEq)]
pub struct PinnedAnimation {
    pub clip: ClipName,
    /// Where in the clip, as a fraction of its length.
    pub phase: f32,
}

/// A second body under the local avatar, playing the same greeting, for a
/// capture of one (`--clip handshake`, `--clip highfive`): a greeting is two
/// people, and whether the hands meet can only be seen with both. Offline,
/// so there is no other avatar to borrow.
///
/// A holder of its own kind, not an [`AvatarHolder`], so the model pose
/// leaves its transform alone: it stands where `greet::Greeting::partner_at`
/// puts a partner, turned to face back. Being under the avatar, its player
/// is driven and pinned with the avatar's own.
#[derive(Component, Clone, Copy, Default)]
pub(crate) struct ClipPartner;

/// Spawn the [`ClipPartner`] once the avatar has its body, when the pinned
/// clip is a greeting.
pub fn spawn_clip_partner(
    mut commands: Commands,
    mut graphs: ResMut<Assets<AnimationGraph>>,
    mut clip_assets: ResMut<Assets<AnimationClip>>,
    pinned: Option<Res<PinnedAnimation>>,
    avatars: Query<Entity, (With<Avatar>, With<AvatarModelLoaded>)>,
    partners: Query<(), With<ClipPartner>>,
    store: Res<UserStore>,
    mut human: human_avatar::HumanAssets,
) {
    use cartopolis_core::Greeting;
    let greeting = match pinned.map(|p| p.clip) {
        Some(ClipName::Emote(Emote::Handshake)) => Greeting::Handshake,
        Some(ClipName::Emote(Emote::HighFive)) => Greeting::HighFive,
        _ => return,
    };
    let (Ok(avatar), true) = (avatars.single(), partners.is_empty()) else {
        return;
    };
    let Some(pack) = human.pack() else { return };
    let (look, tints) = human.look(&store.data.appearance);
    let spawned = human_avatar::spawn_human(
        &mut commands,
        &mut human,
        &mut clip_assets,
        human_avatar::prepare_human(&pack, &look),
        tints,
        avatar,
        ClipPartner,
        false,
    );
    let holder = spawned.holder;
    animate_holder(&mut commands, &mut graphs, spawned, None);
    let [x, z] = greeting.partner_at();
    commands.entity(holder).insert(Transform {
        translation: Vec3::new(x, 0.0, z),
        rotation: Quat::from_rotation_y(std::f32::consts::PI),
        ..default()
    });
}

/// Hold the local avatar at [`PinnedAnimation`], when a capture asks for it.
///
/// After [`update_avatar_animation`], which has already made the pinned clip the
/// main one (and set its speed, which this zeroes).
pub fn pin_avatar_animation(
    pinned: Option<Res<PinnedAnimation>>,
    avatars: Query<(Entity, &AvatarAnimations), With<Avatar>>,
    children: Query<&Children>,
    graphs: Res<Assets<AnimationGraph>>,
    clips: Res<Assets<AnimationClip>>,
    mut players: Query<&mut AnimationPlayer>,
) {
    let Some(pinned) = pinned else {
        return;
    };
    for (avatar, anim) in &avatars {
        let node = anim.node(pinned.clip.pose_clip());
        let duration = graphs
            .get(&anim.graph_handle)
            .and_then(|g| g.get(node))
            .and_then(|n| match &n.node_type {
                bevy::animation::graph::AnimationNodeType::Clip(h) => clips.get(h),
                _ => None,
            })
            .map(AnimationClip::duration);
        let Some(duration) = duration else {
            continue;
        };
        let t = pinned.phase.rem_euclid(1.0) * duration;
        for child in children.iter_descendants(avatar) {
            if let Ok(mut player) = players.get_mut(child) {
                if let Some(active) = player.animation_mut(node) {
                    active.set_speed(0.0);
                    active.set_seek_time(t);
                }
            }
        }
    }
}

/// Switch the local avatar's animation to match what it is actually doing:
/// standing, strolling, running (at the speed it is running), sitting, or playing
/// an emote.
pub fn update_avatar_animation(
    mut commands: Commands,
    time: Res<Time>,
    mut avatar_state: ResMut<AvatarState>,
    children: Query<&Children>,
    animations_to_play: Query<&AvatarAnimations, With<Avatar>>,
    mut players: Query<(&mut AnimationPlayer, &mut AnimationTransitions)>,
    mut avatar_query: Query<(Entity, Option<&mut EmotePlayback>), With<Avatar>>,
    pinned: Option<Res<PinnedAnimation>>,
    mut running: Local<bool>,
) {
    let dt = time.delta_secs();
    // The stride speed, not the wish: a GPS walk carries the avatar without a
    // wish, and an idle clip sliding down the street is what that looked like.
    let stride = avatar_state.stride_speed();
    let moving = stride >= locomotion::MOVING_EPS;
    let started = avatar_state.pending_emote.take();

    for (avatar_entity, emote_playback) in avatar_query.iter_mut() {
        let emote_secs = |e: Emote| {
            animations_to_play
                .get(avatar_entity)
                .map_or(EMOTE_DURATION, |a| a.emote_secs(e))
        };
        // An emote owns the body until it finishes — or until the avatar moves,
        // which cancels it. Standing still to wave is a rule players already know,
        // and it avoids a wave sliding across the pavement.
        let mut active_emote = None;
        match (started, emote_playback) {
            (Some(e), Some(mut playback)) => {
                playback.emote = e;
                playback.remaining = emote_secs(e);
                active_emote = Some(e);
            }
            (Some(e), None) => {
                commands.entity(avatar_entity).insert(EmotePlayback {
                    emote: e,
                    remaining: emote_secs(e),
                });
                active_emote = Some(e);
            }
            (None, Some(mut playback)) => {
                playback.remaining -= dt;
                if playback.remaining <= 0.0 || moving {
                    commands.entity(avatar_entity).remove::<EmotePlayback>();
                } else {
                    active_emote = Some(playback.emote);
                }
            }
            (None, None) => {}
        }

        // Sitting outranks both: it is a state the player is in rather than a
        // motion they asked for, and it is left by walking away, which is what
        // clears it before this is ever reached.
        // A climb outranks everything: the body is being carried up a wall.
        let dangling = avatar_state.seated.is_some_and(|s| s.dangle);
        let pose = match (avatar_state.is_seated(), active_emote) {
            _ if avatar_state.climb.is_some() => Pose {
                clip: PoseClip::Climb,
                rate: 1.0,
            },
            (true, _) => Pose {
                clip: if dangling {
                    PoseClip::Dangle
                } else {
                    PoseClip::Sit
                },
                rate: 1.0,
            },
            (false, Some(e)) => Pose {
                clip: PoseClip::Emote(emote_slot(e)),
                rate: 1.0,
            },
            (false, None) if avatar_state.swimming => swim_pose(stride),
            (false, None) => locomotion_pose(
                stride,
                *running,
                animations_to_play
                    .get(avatar_entity)
                    .map_or((ClipGait::CHARACTER_WALK, ClipGait::CHARACTER_RUN), |a| {
                        (a.walk, a.run)
                    }),
            ),
        };
        // A capture stepping through one clip outranks everything; the clip is
        // held still by `pin_avatar_animation`.
        let pose = match pinned.as_deref() {
            Some(p) => Pose {
                clip: p.clip.pose_clip(),
                rate: 1.0,
            },
            None => pose,
        };
        *running = pose.clip == PoseClip::Run;

        let Ok(anim) = animations_to_play.get(avatar_entity) else {
            continue;
        };
        for child in children.iter_descendants(avatar_entity) {
            if let Ok((mut player, mut transitions)) = players.get_mut(child) {
                apply_pose(&mut player, &mut transitions, anim, pose);
            }
        }
    }
}

/// Everything the local avatar's *model* does that its position does not: the
/// tuck of a jump, the arrival after a teleport, and the slide onto a bench.
///
/// All live on the model holders rather than on the avatar entity, and for the
/// same reason: the avatar's own transform is its authoritative position and yaw
/// (collision, the follow camera and the replicated facing all read it, and online
/// the sim owns it outright), so pitching or shifting *that* would move all of
/// them. A holder is a child of it, so writing one is a purely visual change.
///
/// **The tuck.** The motion-capture clip set has no jump animation
/// (`docs/notes/makehuman-avatars.md`), so a jump plays *idle* and is posed by a
/// small pitch driven off the vertical velocity. Holders pivot about chest height
/// so the feet swing back instead of the head diving through the ground.
///
/// **The seat.** A bench is a solid prop, so the avatar stops short of the place
/// it has to sit; the gap is carried here as [`AvatarState::seat_shift`], eased
/// over [`SEAT_SLIDE_SECS`] so sitting down is a movement rather than a jump cut.
/// The offset is in world XZ and the holder is under a yawed parent, so it is
/// turned back out of the avatar's frame before it is written.
///
/// **Other players sit here too.** The seat is replicated (protocol 19), so a
/// remote avatar's holders take the same treatment from
/// [`RemoteAvatar::seated`] — but not the tuck, which is not on the wire. Each
/// viewer runs the slide itself off its own clock rather than reading a blended
/// offset off the wire twenty times a second.
///
/// Every holder is compared before it is written, so the ordinary standing case
/// costs one comparison per model and marks nothing changed. That replaced an
/// early return, which had to be paired with a "did I write last frame" flag to
/// get the identity back on the frame a pose ended — one more piece of state than
/// the comparison it was avoiding.
pub fn update_avatar_model_pose(
    time: Res<Time>,
    mut avatar_state: ResMut<AvatarState>,
    avatars: Query<Entity, With<Avatar>>,
    mut remotes: Query<&mut RemoteAvatar>,
    mut holders: Query<(&ChildOf, &mut Transform), With<AvatarHolder>>,
    // The avatar designer is open: turn the model to face the camera, as
    // someone turns to a mirror. A turn of the model, not the avatar, so
    // neither the heading the sim holds nor the camera moves.
    designing: Option<Res<human_avatar::HumanDraft>>,
    designer_turn: Option<Res<human_avatar::DesignerTurn>>,
    // A selfie turns the model to the camera too: you face your phone.
    held: Option<Res<crate::systems::held_phone::HeldPhone>>,
    camera: Query<&GlobalTransform, With<crate::components::FreeCamera>>,
    avatar_at: Query<&GlobalTransform, With<Avatar>>,
    mut lean: Local<f32>,
    // The arrival: how much of it is *left*, and which teleport it belongs to.
    // Both live here because this is the one system that owns the holder
    // transform — two systems writing it would race for the frame.
    //
    // Counted down rather than up, because a `Local<f32>` starts at zero and zero
    // has to mean *finished*: counting up would have every launch open on the
    // first frame of an arrival nobody asked for.
    mut arrive_left: Local<f32>,
    mut arrive_seq: Local<u32>,
    // A capture holding the stroke (`--clip swim`) lies the body down too,
    // though the avatar stands still for it.
    pinned: Option<Res<PinnedAnimation>>,
) {
    // Advance the seat slide first, so the offset written below is this frame's.
    // A seat that has finished unwinding is dropped here rather than where the
    // player asked to get up — that is what lets `stand_up` be a one-liner
    // everywhere it is called from.
    let seat_shift = {
        let step = time.delta_secs() / SEAT_SLIDE_SECS;
        let mut done = false;
        if let Some(s) = &mut avatar_state.seated {
            if s.rising {
                s.blend = (s.blend - step).max(0.0);
                done = s.blend <= 0.0;
            } else {
                s.blend = (s.blend + step).min(1.0);
            }
        }
        if done {
            avatar_state.seated = None;
        }
        Quat::from_rotation_y(-avatar_state.facing_yaw) * avatar_state.seat_shift()
    };
    let target = if avatar_state.grounded {
        0.0
    } else {
        // A jump has no clip (`docs/notes/avatar-locomotion.md`), so it is posed:
        // the body tucks forward on the way up and opens out as it falls, driven
        // by the vertical velocity itself. Signed, so the rise and the fall do not
        // look identical — the give-away that an "airborne" pose is really a
        // single frozen frame.
        (-avatar_state.vertical_velocity / motion::JUMP_SPEED).clamp(-1.0, 1.0) * JUMP_TUCK
    };
    // A jump to a saved place restarts the arrival even if one is still playing:
    // the second jump is where the body is now.
    let was_arriving = *arrive_left > 0.0;
    if *arrive_seq != avatar_state.teleport_sequence {
        *arrive_seq = avatar_state.teleport_sequence;
        *arrive_left = TELEPORT_ARRIVE_SECS;
    } else if was_arriving {
        *arrive_left = (*arrive_left - time.delta_secs()).max(0.0);
    }
    let arrived = 1.0 - *arrive_left / TELEPORT_ARRIVE_SECS;

    let a = 1.0 - (-JUMP_TUCK_SMOOTHING * time.delta_secs()).exp();
    *lean += (target - *lean) * a;
    if (target - *lean).abs() < 1e-3 {
        *lean = target;
    }
    // The same slide, on everyone else's clock. A remote carries the seat it was
    // sent and eases onto it here; a seat that has gone eases back to nothing and
    // the blend simply reaches zero, so there is nothing to retire.
    let step = time.delta_secs() / SEAT_SLIDE_SECS;
    for mut r in remotes.iter_mut() {
        let want = if r.seated.is_some() { 1.0 } else { 0.0 };
        if r.seat_blend != want {
            r.seat_blend = if want > r.seat_blend {
                (r.seat_blend + step).min(1.0)
            } else {
                (r.seat_blend - step).max(0.0)
            };
        }
    }

    // Lying into the stroke lifts the body from treading water, feet
    // `TREAD_DEPTH` down, to the head at the surface; coming upright lowers it
    // again. Eased over the clip's cross-fade, on everyone's own clock.
    let lie_step = time.delta_secs() / SWIM_LIE_SECS;
    let ease_lie = |blend: f32, lying: bool| {
        if lying {
            (blend + lie_step).min(1.0)
        } else {
            (blend - lie_step).max(0.0)
        }
    };
    let stroking = pinned.is_some_and(|p| p.clip == ClipName::Swim)
        || (avatar_state.swimming && avatar_state.stride_speed() >= locomotion::MOVING_EPS);
    avatar_state.swim_blend = ease_lie(avatar_state.swim_blend, stroking);
    for mut r in remotes.iter_mut() {
        let stroking = r.swimming
            && Vec2::new(r.net_velocity.x, r.net_velocity.z).length() >= locomotion::MOVING_EPS;
        let blend = ease_lie(r.swim_blend, stroking);
        if r.swim_blend != blend {
            r.swim_blend = blend;
        }
    }

    let mut local = body_pose(*lean, arrived);
    local.translation += seat_shift;
    // Up a wall: the rest of its height above the clip's own step
    // (`climb::Climb::holder_lift`).
    if let Some(c) = &avatar_state.climb {
        local.translation.y += c.holder_lift();
    }
    let mut local = local * swim_lie(avatar_state.swim_blend);
    if designing.is_some_and(|d| d.0.is_some()) || held.is_some_and(|h| h.selfie) {
        // Where the camera is, in the avatar's own frame (the model faces +Z).
        if let (Ok(cam), Ok(me)) = (camera.single(), avatar_at.single()) {
            let (_, me_rot, me_at) = me.to_scale_rotation_translation();
            let to_cam = me_rot.inverse() * (cam.translation() - me_at);
            if to_cam.x.abs() + to_cam.z.abs() > 1.0e-3 {
                let yaw = to_cam.x.atan2(to_cam.z) + designer_turn.as_ref().map_or(0.0, |t| t.0);
                local.rotation = Quat::from_rotation_y(yaw) * local.rotation;
            }
        }
    }
    for (parent, mut tf) in holders.iter_mut() {
        let owner = parent.parent();
        let want = if avatars.contains(owner) {
            local
        } else if let Ok(r) = remotes.get(owner) {
            // No tuck and no arrival: neither is replicated. Only the seat, turned
            // out of the remote's own facing exactly as the local one is.
            let shift = r.seated.map_or(Vec3::ZERO, |o| {
                let o = o * r.seat_blend;
                Quat::from_rotation_y(-r.net_yaw) * Vec3::new(o.x, 0.0, o.y)
            });
            // The edge's lift is measured by this viewer (`smooth_remote_avatars`),
            // and eased with the seat; a climb is lifted as the local one is.
            let climbing = r.climb.map_or(0.0, |(t, h)| {
                crate::systems::climb::climb_lift(h, t / crate::systems::climb::CLIMB_SECS)
            });
            let lift = Vec3::Y * (r.seat_lift * r.seat_blend + climbing);
            Transform::from_translation(shift + lift) * swim_lie(r.swim_blend)
        } else {
            continue;
        };
        if *tf != want {
            *tf = want;
        }
    }
}

/// How far forward of the feet the swim's head is (m), on the default man: the
/// loop is stood with its hips over the origin, and the trunk reaches 0.54 m
/// ahead of them to the neck (`the_swim_lies_face_down_head_first`).
const SWIM_HEAD_Z: f32 = 0.75;
/// How far the swimmer is tipped feet-down about the head (rad). The take was
/// captured lying on a stool, where the kick draws the heels up into the air;
/// in water the legs ride lower than the head, and level they broke the
/// surface shins-first at every stroke (seen 2026-11-14).
const SWIM_TILT: f32 = 0.3;

/// The model's lift and tilt into the stroke at `blend` (0 treading water, 1
/// lying in the stroke): up by [`locomotion::SWIM_LIFT`] so the head rides at
/// the surface, and tipped feet-down about the head by [`SWIM_TILT`].
fn swim_lie(blend: f32) -> Transform {
    if blend <= 0.0 {
        return Transform::IDENTITY;
    }
    let pivot = Vec3::new(0.0, locomotion::SWIM_HEAD_Y, SWIM_HEAD_Z);
    let tilt = Quat::from_rotation_x(-SWIM_TILT * blend);
    Transform {
        translation: pivot - tilt * pivot + Vec3::Y * locomotion::SWIM_LIFT * blend,
        rotation: tilt,
        ..default()
    }
}

/// How long the model takes to slide onto a seat, and back off it. Matched to
/// [`SIT_FADE`] so the body finishes arriving as it finishes sitting.
const SEAT_SLIDE_SECS: f32 = 0.45;

/// The holder transform for a given lean (the jump tuck) and arrival progress.
///
/// The lean is a pitch about the model's X axis taken around a pivot at
/// [`LEAN_PIVOT_Y`], so the chest stays put and the feet swing back.
///
/// `arrival` runs 0 → 1 over [`TELEPORT_ARRIVE_SECS`] and is a **scale about the
/// feet**: the avatar re-forms out of a thin vertical column, overshoots standing
/// height and settles. Scaling about the feet rather than the origin is the whole
/// difference between materialising *on* the ground and materialising through it —
/// the models are authored origin-at-feet, so the holder's own origin already is
/// the sole of the foot and no pivot correction is needed. At `arrival >= 1` the
/// scale is exactly [`Vec3::ONE`], so an arrival that has finished composes with
/// the lean without leaving a residue.
fn body_pose(lean: f32, arrival: f32) -> Transform {
    let rotation = Quat::from_rotation_x(lean);
    let pivot = Vec3::Y * LEAN_PIVOT_Y;
    Transform {
        translation: pivot - rotation * pivot,
        rotation,
        scale: teleport_scale(arrival),
    }
}

/// The arrival's scale at `progress` (0 → 1), [`Vec3::ONE`] once it is done.
///
/// Two eases, because the two axes are telling different halves of the story.
/// **Across** (x and z) the body swells out of a [`TELEPORT_ARRIVE_PINCH`]-thin
/// column on a decelerating curve, so almost all of the widening happens in the
/// first third and the rest is the figure standing there. **Along** (y) it starts
/// stretched by the same amount the column is thin — volume, roughly — passes
/// through [`TELEPORT_ARRIVE_STRETCH`] and settles on 1 with a single soft
/// overshoot. One overshoot reads as a body arriving; a spring with several
/// reads as a bug.
#[must_use]
fn teleport_scale(progress: f32) -> Vec3 {
    if progress >= 1.0 {
        return Vec3::ONE;
    }
    let t = progress.max(0.0);
    // Cubic ease-out: fast at the start, flat at the end.
    let out = 1.0 - (1.0 - t).powi(3);
    let across = TELEPORT_ARRIVE_PINCH + (1.0 - TELEPORT_ARRIVE_PINCH) * out;
    let base = TELEPORT_ARRIVE_COLUMN + (1.0 - TELEPORT_ARRIVE_COLUMN) * out;
    // A half-sine bump on top of that ease, faded in by the ease itself so it
    // cannot lift the opening frame: `along` therefore crosses 1.0 late and comes
    // back down to it, instead of arriving at standing height from one side.
    let bump = (TELEPORT_ARRIVE_STRETCH - 1.0) * (std::f32::consts::PI * t).sin() * out;
    Vec3::new(across, base + bump, across)
}

/// How far past its last snapshot a remote avatar is carried on its replicated
/// velocity. Five snapshots at the sim's 20 Hz: enough to ride out jitter and a
/// dropped frame or two, short enough that a link which has really gone quiet
/// leaves a stranger standing a metre or so on rather than walking off down the
/// street on a velocity nobody is confirming.
const REMOTE_MAX_EXTRAPOLATION: f32 = 0.25;

/// One frame of a remote avatar's drawn position, given the snapshot it is
/// following. `age` is the snapshot's age *after* this frame.
///
/// Two halves, and the first is what the old follower lacked. The display is
/// **carried** by the replicated velocity, then **corrected** toward the
/// snapshot extrapolated to the present. An exponential follow of the raw
/// snapshot — which is what this was — tracks a moving target a constant
/// `speed / rate` behind (13 cm at a walk, 43 cm at a run) plus a whole
/// snapshot interval of staleness, and can never catch up. Carrying first makes
/// that steady-state error zero, so the correction only ever works on jitter and
/// on genuine changes of course.
///
/// Past [`REMOTE_MAX_EXTRAPOLATION`] both halves stop advancing and the avatar
/// settles where the extrapolation left it. Nothing is carried below the floor:
/// the sim clamps its own `y` there, and extrapolating the tail of a jump would
/// otherwise sink the feet for a frame before the landing snapshot arrives.
#[must_use]
fn remote_display_step(
    display: Vec3,
    net_position: Vec3,
    net_velocity: Vec3,
    age: f32,
    dt: f32,
) -> Vec3 {
    let horizon = age.min(REMOTE_MAX_EXTRAPOLATION);
    let mut target = net_position + net_velocity * horizon;
    let mut carried = if age <= REMOTE_MAX_EXTRAPOLATION {
        display + net_velocity * dt
    } else {
        display
    };
    target.y = target
        .y
        .max(cartopolis_core::motion::GROUND_Y.min(net_position.y));
    carried.y = carried
        .y
        .max(cartopolis_core::motion::GROUND_Y.min(net_position.y));
    let a = 1.0 - (-ONLINE_DISPLAY_SMOOTHING * dt).exp();
    carried.lerp(target, a)
}

/// One frame of a remote avatar's drawn position **on this viewer's ground**:
/// `shown` is last frame's drawn position, `lift` the ground it was raised by.
/// Returns the new drawn position and the new lift.
///
/// The sim is flat — `y = 0` is the ground everywhere, because it holds no map
/// data — so every viewer stands other people on its own measured field, the
/// way [`smooth_online_avatar_display`] stands its own avatar. Until this only
/// the local avatar was lifted, and a stranger standing where the ground is
/// lower than the anchor's datum floated above it (or sank where it is higher).
/// Last frame's lift comes off first for the reason it does there: smoothing a
/// lifted position toward a flat snapshot would drag the avatar into the
/// hillside every frame. The lift follows [`airborne_lift`], so a remote high in
/// the air over a canal does not dip into it any more than the local one does.
#[must_use]
fn remote_display_on_ground(
    shown: Vec3,
    lift: f32,
    net_position: Vec3,
    net_velocity: Vec3,
    age: f32,
    dt: f32,
    ground_at: impl Fn(f32, f32) -> f32,
) -> (Vec3, f32) {
    let flat = remote_display_step(shown - Vec3::Y * lift, net_position, net_velocity, age, dt);
    // `flat.y` is the height above the sim's ground.
    let lift = airborne_lift(lift, ground_at(flat.x, flat.z), flat.y, dt);
    (flat + Vec3::Y * lift, lift)
}

/// Draw other players' avatars where they are now, not where the last snapshot
/// said they were — see [`remote_display_step`] — and on the ground this client
/// measured under them ([`remote_display_on_ground`]).
pub fn smooth_remote_avatars(
    online: Option<Res<crate::resources::OnlineSession>>,
    time: Res<Time>,
    terrain: Res<crate::systems::terrain::TerrainField>,
    colliders: Res<crate::systems::collision::BuildingColliders>,
    mut q: Query<(&mut RemoteAvatar, &mut Transform, Option<&AvatarAnimations>), Without<Avatar>>,
) {
    if online.is_none() {
        return;
    }
    let dt = time.delta_secs();
    let a_rot = 1.0 - (-REMOTE_ROT_SMOOTHING * dt).exp();
    let ground = |p: Vec2| terrain.ground_at(p.x, p.y);
    let water = |p: Vec2| terrain.swim_surface_at(p.x, p.y);
    let map = crate::systems::climb::Surroundings {
        colliders: &colliders,
        ground: &ground,
        water: &water,
    };
    for (mut remote, mut tf, body) in q.iter_mut() {
        remote.net_age += dt;
        // Last frame's wall top comes off with the ground, for the reason the
        // ground does (`remote_display_on_ground`).
        let (mut shown, mut lift) = remote_display_on_ground(
            tf.translation - Vec3::Y * remote.ledge_lift,
            remote.terrain_lift,
            remote.net_position,
            remote.net_velocity,
            remote.net_age,
            dt,
            |x, z| ground_or_water(&terrain, x, z).0,
        );
        let swimming = terrain.swim_surface_at(shown.x, shown.z).is_some();
        if swimming || remote.swimming {
            let eased = water_lift(remote.terrain_lift, lift, dt);
            shown.y += eased - lift;
            lift = eased;
        }
        if remote.swimming != swimming {
            remote.swimming = swimming;
        }
        // On a wall top: nobody but a climber can be inside a wall, so one that
        // is there climbed it (`systems::climb`). Its climb is played here,
        // seen starting when it reaches the wall, and the top is its ground
        // from the end of it; off the edge it falls.
        let at = Vec2::new(shown.x, shown.z);
        let near_ground = shown.y - lift < 0.3;
        let top = colliders
            .ledge_at(at)
            .map(|t| t - lift)
            .filter(|h| *h > 0.05 && near_ground)
            .unwrap_or(0.0);
        let speed = remote_ground_speed(&remote);
        let (ledge, climb) = remote_ledge_step(remote.ledge_lift, remote.climb, top, speed, dt);
        if remote.climb != climb {
            remote.climb = climb;
        }
        remote.ledge_lift = ledge;
        shown.y += ledge;
        // Sitting on an edge: the lift and the clip, from the edge this viewer
        // finds under the replicated seat.
        let (seat_lift, seat_dangle) = remote
            .seated
            .and_then(|offset| {
                let out = Vec2::new(remote.net_yaw.sin(), remote.net_yaw.cos());
                crate::systems::climb::seat_on_edge(&map, at + offset, out)
            })
            .map_or((0.0, false), |e| {
                // On this remote's own legs, as its owner draws it.
                let fit = body.map_or_else(Default::default, AvatarAnimations::seat_fit);
                let pose = e.pose(shown.y, fit);
                (pose.lift, pose.dangle)
            });
        if remote.seated.is_some() || remote.seat_blend <= 0.0 {
            remote.seat_lift = seat_lift;
            remote.seat_dangle = seat_dangle;
        }
        tf.translation = shown;
        remote.terrain_lift = lift;
        // Replicated world yaw from each client’s camera-facing convention.
        let target_rot = Quat::from_rotation_y(wrap_angle_pi(remote.net_yaw));
        tf.rotation = tf.rotation.slerp(target_rot, a_rot);
    }
}

/// How fast a remote avatar that walked off a wall top comes down to the
/// ground, m/s — a drop, as stepping off a quay into the water is
/// ([`WATER_SINK_RATE`]); this viewer has no fall of its own to time it by.
const REMOTE_LEDGE_DROP_RATE: f32 = 4.0;
/// The fastest a remote avatar arriving inside a wall is taken to be climbing
/// it, m/s: a climb carries its avatar onto the top at
/// `(CLIMB_REACH_M + a footing) / CLIMB_SECS`, under 0.9 m/s.
const REMOTE_CLIMB_MAX_SPEED: f32 = 1.0;

/// One frame of a remote avatar's wall top: last frame's lift and climb, and
/// the top under it now (metres over the ground, zero for none), to this
/// frame's. Arriving at a top it can climb from the ground starts a climb,
/// which holds the lift where it was for [`climb::CLIMB_SECS`] while the
/// clip and the model's lift take it up (`update_avatar_model_pose`); then
/// the top is its ground. A top it is already on is followed, up at once
/// (a step) and down as a drop.
///
/// Only at a climber's pace (`speed`, the replicated ground speed, at most
/// [`REMOTE_CLIMB_MAX_SPEED`]): a climb carries its avatar at under 1 m/s,
/// and one walking into a wall at a walk is a player whose own client draws
/// no walls (street detail off), passing through it, not climbing.
///
/// [`climb::CLIMB_SECS`]: crate::systems::climb::CLIMB_SECS
#[must_use]
fn remote_ledge_step(
    lift: f32,
    climb: Option<(f32, f32)>,
    top: f32,
    speed: f32,
    dt: f32,
) -> (f32, Option<(f32, f32)>) {
    use crate::systems::climb;
    if let Some((t, height)) = climb {
        let t = t + dt;
        if t >= climb::CLIMB_SECS {
            return (if top > 0.0 { top } else { height }, None);
        }
        return (lift, Some((t, height)));
    }
    if lift < 0.05 {
        return if climb::climbable(top) && speed <= REMOTE_CLIMB_MAX_SPEED {
            (lift, Some((0.0, top)))
        } else if top < crate::systems::collision::CLIMB_MIN_M {
            // A step: a wall too low to stop anybody.
            (top, None)
        } else {
            (0.0, None)
        };
    }
    if top >= lift {
        (top, None)
    } else {
        ((lift - REMOTE_LEDGE_DROP_RATE * dt).max(top), None)
    }
}

/// Ground speed a remote avatar's gait is chosen from: the replicated
/// horizontal velocity, and zero once the snapshot is too old to vouch for it.
#[must_use]
fn remote_ground_speed(remote: &RemoteAvatar) -> f32 {
    if remote.net_age > REMOTE_MAX_EXTRAPOLATION {
        return 0.0;
    }
    Vec2::new(remote.net_velocity.x, remote.net_velocity.z).length()
}

/// Walk / run / idle / emote for other players' avatars.
///
/// The gait is chosen from the **replicated** velocity (protocol 24) by the
/// same rule the local avatar follows, so a remote strolling looks like it is
/// strolling. It used to be measured by differencing the smoothed transform,
/// which lags the truth by the smoothing itself and was therefore wrong at
/// exactly the moments the gait changes — a start and a stop — which is where
/// the feet slid.
pub fn update_remote_avatar_animation(
    mut commands: Commands,
    online: Option<Res<crate::resources::OnlineSession>>,
    time: Res<Time>,
    children: Query<&Children>,
    animations_to_play: Query<&AvatarAnimations, With<RemoteAvatar>>,
    mut players: Query<(&mut AnimationPlayer, &mut AnimationTransitions)>,
    mut remote_roots: Query<
        (Entity, &RemoteAvatar, Option<&mut EmotePlayback>),
        With<RemoteAvatar>,
    >,
    // Whether each remote avatar was running last frame, which is what makes the
    // walk/run choice hysteretic instead of flapping at the threshold.
    //
    // `EntityHashMap` rather than `std::collections::HashMap`: keys are entities,
    // and SipHash on an `Entity` is a lot of work to hash a pair of integers.
    mut running: Local<bevy::ecs::entity::EntityHashMap<bool>>,
) {
    if online.is_none() {
        return;
    }
    let dt = time.delta_secs();
    for (avatar_entity, remote, emote_playback) in remote_roots.iter_mut() {
        let Ok(anim) = animations_to_play.get(avatar_entity) else {
            continue;
        };
        let speed = remote_ground_speed(remote);
        let moving = speed > REMOTE_RUN_SPEED_THRESH;

        // `RemoteAvatar::pending_emote` is set by the snapshot apply when it sees a
        // sequence it hasn't played yet, so this is an edge, not a level.
        let mut active_emote = None;
        match (remote.pending_emote, emote_playback) {
            (Some(e), Some(mut playback)) => {
                playback.emote = e;
                playback.remaining = anim.emote_secs(e);
                active_emote = Some(e);
            }
            (Some(e), None) => {
                commands.entity(avatar_entity).insert(EmotePlayback {
                    emote: e,
                    remaining: anim.emote_secs(e),
                });
                active_emote = Some(e);
            }
            (None, Some(mut playback)) => {
                playback.remaining -= dt;
                if playback.remaining <= 0.0 || moving {
                    commands.entity(avatar_entity).remove::<EmotePlayback>();
                } else {
                    active_emote = Some(playback.emote);
                }
            }
            (None, None) => {}
        }

        let was_running = running.get(&avatar_entity).copied().unwrap_or(false);
        // Sitting outranks the emote here for the same reason it does locally: it
        // is a state this avatar is in, and the sim drops it the moment its owner
        // asks to move.
        let pose = match (remote.seated.is_some(), active_emote) {
            _ if remote.climb.is_some() => Pose {
                clip: PoseClip::Climb,
                rate: 1.0,
            },
            (true, _) => Pose {
                clip: if remote.seat_dangle {
                    PoseClip::Dangle
                } else {
                    PoseClip::Sit
                },
                rate: 1.0,
            },
            (false, Some(e)) => Pose {
                clip: PoseClip::Emote(emote_slot(e)),
                rate: 1.0,
            },
            (false, None) if remote.swimming => swim_pose(if moving { speed } else { 0.0 }),
            (false, None) => locomotion_pose(
                if moving { speed } else { 0.0 },
                was_running,
                (anim.walk, anim.run),
            ),
        };
        running.insert(avatar_entity, pose.clip == PoseClip::Run);

        for child in children.iter_descendants(avatar_entity) {
            if let Ok((mut player, mut transitions)) = players.get_mut(child) {
                apply_pose(&mut player, &mut transitions, anim, pose);
            }
        }
    }
    // Forget avatars that have gone. Remote avatars are despawned when they leave
    // the area of interest or disconnect, and without this their entries stayed
    // forever — an unbounded leak over a long session on a busy server, and worse
    // than a leak when Bevy recycles an entity index and hands a freshly-arrived
    // avatar the walk/run state of whoever used to hold it.
    running.retain(|entity, _| remote_roots.contains(*entity));
}

/// Clear the emote edge once every consumer has seen it (this runs after
/// [`update_remote_avatar_animation`] in the schedule). Kept separate so the
/// animation system can take `&RemoteAvatar` immutably alongside its other
/// queries.
pub fn clear_remote_emote_edges(mut remotes: Query<&mut RemoteAvatar>) {
    for mut remote in remotes.iter_mut() {
        if remote.pending_emote.is_some() {
            remote.pending_emote = None;
        }
    }
}

/// When online, the server updates `AvatarState::position` at tick rate; smooth `display_position`
/// and the avatar transform so the third-person camera and world do not jitter.
pub fn smooth_online_avatar_display(
    online: Option<Res<crate::resources::OnlineSession>>,
    mut avatar_state: ResMut<AvatarState>,
    time: Res<Time>,
    mut avatar_query: Query<&mut Transform, With<Avatar>>,
    terrain: Res<crate::systems::terrain::TerrainField>,
    footing: crate::systems::interiors::Footing,
    mut health: ResMut<crate::resources::NetPredictionHealth>,
    // The age this system saw last frame, so the gap a snapshot closed can be read
    // off the frame it resets on. A `Local` and not a field on the resource: it is
    // this system's own bookkeeping and nothing else may write it.
    mut prev_age: Local<f32>,
    // The low-passed prediction error `reconcile_display` corrects on. A `Local`
    // for the same reason: it is the reconciler's memory, and a second writer
    // would be a second opinion about how far off we are.
    mut reconcile_error: Local<Vec3>,
) {
    let dt = time.delta_secs();
    if online.is_some() {
        // A teleport holds authority off for up to `network::TELEPORT_HOLD_S`
        // while the sim's echo comes back, so the age climbs for a reason that has
        // nothing to do with the link. Measuring those frames would report a
        // two-second network gap every time somebody taps a saved place, which is
        // the fastest way to make an instrument not worth reading.
        //
        // **Device tracking sets the same hold about once a second**, because a
        // GNSS fix is placed through `NetMessage::Teleport` too
        // (`driver::place_avatar`). At a walking pace the next snapshot is well
        // inside `TELEPORT_ECHO_M` and clears it within a frame, so almost
        // nothing is skipped; in a vehicle the hold lasts a round trip and this
        // stops counting for perhaps a tenth of the frames. That is the right
        // answer rather than a hole in the instrument — while a fix owns the
        // position it is being *written*, not predicted, so those frames have no
        // prediction health to report. Worth knowing before reading a `parked`
        // share taken with the locate button lit or while driving.
        if avatar_state.teleport_hold.is_none() {
            // A snapshot landed since this system last ran: `apply_network_snapshot`
            // is ordered ahead of us and zeroes the age, and nothing else does. So
            // last frame's age is the gap that snapshot closed.
            if avatar_state.snapshot_age == 0.0 && *prev_age > 0.0 {
                health.last_gap_s = *prev_age;
                health.worst_gap_s = health.worst_gap_s.max(*prev_age);
                health.snapshots += 1;
            }
            health.frames += 1;
            if is_stalled(avatar_state.snapshot_age + dt) {
                health.stalled_frames += 1;
            }
        }
        // Client-side prediction: advance the display by the velocity we're
        // sending to the sim (frame-rate integration), then reconcile toward the
        // authoritative position. `predicted_velocity` already reflects collision
        // steering.
        //
        // The reconcile target is the last snapshot **extrapolated by its own
        // age**, not the raw snapshot. `AvatarState::position` only advances at
        // the 20 Hz tick, so it is a staircase: reconciling against it directly
        // meant that between two snapshots the prediction ran forward at v while
        // the correction pulled it back toward a target that was standing still.
        // The pull reaches ONLINE_RECONCILE_RATE · v · 0.05 ≈ 3.6 m/s against a
        // 8 m/s walk just before each snapshot lands, then resets — a 20 Hz
        // sawtooth in the apparent speed, and (since the follow camera tracks
        // `display_position`) in the whole view. Extrapolating the target by
        // `snapshot_age` compares like with like: the error signal is smooth, so
        // reconciliation only corrects genuine divergence.
        avatar_state.snapshot_age += dt;
        let mut target = extrapolated_target(
            avatar_state.position,
            avatar_state.predicted_velocity,
            avatar_state.snapshot_age,
        );
        let mut display = avatar_state.display_position + avatar_state.predicted_velocity * dt;
        // The sim has no terrain and never will — it holds no map data at all, and
        // the map streams only on the client. So the ground is added *here*, on the
        // display side: the wire, the prediction and the reconciliation all stay on
        // the flat plane the sim actually simulates, and only what you see stands on
        // the measured one. Last frame's lift comes off first for exactly that
        // reason — smoothing a lifted position toward a flat target would drag the
        // avatar back into the hillside every frame, and the two would fight for as
        // long as you stood on a slope.
        // Both lifts come off, for the reason the comment above gives: what is
        // smoothed toward a flat target has to be flat itself, or the avatar is
        // dragged back down through the storey it is standing on every frame.
        display.y -= avatar_state.terrain_lift + avatar_state.floor_lift;
        // Match the sim's ground clamp (`state::step`, y >= 0) so a falling
        // prediction doesn't dip through the floor and bounce back.
        if display.y < 0.0 {
            display.y = 0.0;
        }
        if target.y < 0.0 {
            target.y = 0.0;
        }
        // Measured against the speed actually being predicted, not the ramped
        // `ground_speed`: a fall moves the avatar with no ground speed at all.
        let flat = reconcile_display(
            display,
            target,
            &mut reconcile_error,
            avatar_state.predicted_velocity.length(),
            is_stalled(avatar_state.snapshot_age),
            dt,
        );
        // `flat.y` is the height above the sim's ground; see `airborne_lift`.
        let (ground, swimming) = ground_or_water(&terrain, flat.x, flat.z);
        let lift = airborne_lift(avatar_state.terrain_lift, ground, flat.y, dt);
        avatar_state.terrain_lift = if swimming || avatar_state.swimming {
            water_lift(avatar_state.terrain_lift, lift, dt)
        } else {
            lift
        };
        avatar_state.swimming = swimming;
        // The storey, on top of the ground. Sampled from `flat` — the reconciled
        // sim position, which is the frame `interiors` builds its rooms in and the
        // frame `send_network_intent` resolves collision in — so the floor the
        // avatar stands on is the floor it can see and the wall it just walked
        // through. Held while the avatar is well off it (`FLOOR_FREEZE_ABOVE_M`).
        // A roof is a floor that comes up under a descent from any height, so it
        // is asked about the frozen floor too.
        let feet = crate::systems::interiors::Feet {
            world: Vec2::new(flat.x, flat.z),
            ground: avatar_state.terrain_lift,
            above: flat.y,
            carried: flat.y,
            falling: (-avatar_state.predicted_velocity.y * dt).max(0.0),
        };
        let floor = if flat.y > FLOOR_FREEZE_ABOVE_M {
            footing.hold(&mut avatar_state, feet)
        } else {
            footing.step(&mut avatar_state, Vec2::new(flat.x, flat.z), dt, feet)
        };
        avatar_state.display_position = flat + Vec3::Y * (avatar_state.terrain_lift + floor);
        // What the follow camera leads by. The predicted velocity is the honest
        // answer online: it is precisely the rate `display_position` advances at
        // between snapshots, which is what the rig is trying to keep up with.
        avatar_state.velocity = avatar_state.predicted_velocity;
        *prev_age = avatar_state.snapshot_age;
        if let Ok(mut tf) = avatar_query.single_mut() {
            tf.translation = avatar_state.display_position;
        }
    } else {
        avatar_state.display_position = avatar_state.position;
        *prev_age = 0.0;
        // Offline the transform *is* the position, so there is no error to carry.
        // Zeroed rather than left standing: a session can be entered and left
        // while the app runs, and the first online frame must not correct for a
        // disagreement that belongs to a connection which has already gone.
        *reconcile_error = Vec3::ZERO;
    }
}

/// A travel wish from neither the keys nor the stick: an agent walking this
/// avatar over MCP (`systems::mcp::agent`).
///
/// **The third input source, and deliberately the weakest.** It is read by
/// [`handle_avatar_movement`] beside the keyboard and the on-screen stick, so a
/// walk an agent asked for runs through exactly the ramp, the facing, the
/// collision and — online — the intent a person's walk does, and it cannot
/// behave differently from one. Anybody at the keys wins: a held key or a
/// pushed stick replaces it for that frame.
///
/// A world direction rather than a camera-relative one like the other two: an
/// agent is steering toward a place, not pushing a stick in front of a screen
/// it may not be looking at. Rewritten every frame by the thing steering, and
/// zero when nothing is.
#[derive(Resource, Default, Debug, Clone, Copy, PartialEq)]
pub struct AgentWalk {
    /// Unit travel direction on world XZ, or zero for "not walking".
    pub dir: Vec3,
    pub gait: locomotion::Gait,
}

/// A travel wish from "Walk me there": the avatar following a planned walking
/// route by itself (`systems::walk_guide`).
///
/// The same shape and the same rules as [`AgentWalk`] — a world direction,
/// rewritten every frame by its steerer, and anybody at the keys wins — but a
/// resource of its own, because each steerer zeroes its slot on every frame it is
/// *not* walking, and two of them sharing one would cancel each other.
#[derive(Resource, Default, Debug, Clone, Copy, PartialEq)]
pub struct RouteWalk {
    /// Unit travel direction on world XZ, or zero for "not walking".
    pub dir: Vec3,
    pub gait: locomotion::Gait,
}

/// The two travel wishes that come from no person: an agent's walk and a walk
/// along a route. One parameter because [`handle_avatar_movement`] is at Bevy's
/// sixteen.
#[derive(bevy::ecs::system::SystemParam)]
pub struct SteeredWalks<'w> {
    agent: Res<'w, AgentWalk>,
    route: Res<'w, RouteWalk>,
}

impl SteeredWalks<'_> {
    /// The wish to follow when nobody is at the controls: the agent's if it is
    /// walking, else the route's.
    fn wish(&self) -> Option<(Vec3, locomotion::Gait)> {
        [
            (self.agent.dir, self.agent.gait),
            (self.route.dir, self.route.gait),
        ]
        .into_iter()
        .find(|(dir, _)| dir.length_squared() > 1e-8)
    }
}

pub fn handle_avatar_movement(
    online: Option<Res<crate::resources::OnlineSession>>,
    keyboard_input: Res<ButtonInput<KeyCode>>,
    // The on-screen thumbstick — a finger's WASD. Written by
    // `map_osd::render_map_osd`, which runs *after* this system (see its doc), so
    // this is last frame's push: a constant frame of latency on a held
    // translation control, not jitter.
    osd: Res<crate::systems::map_osd::OsdInput>,
    steered: SteeredWalks,
    time: Res<Time>,
    mut avatar_query: Query<(&mut Transform, &big_space::prelude::GridCell), With<Avatar>>,
    mut avatar_state: ResMut<AvatarState>,
    camera_state: Res<crate::resources::CameraState>,
    egui_manager: crate::systems::egui_manager::EguiRef,
    edit_dialog: Res<crate::resources::EditDialogState>,
    colliders: Res<crate::systems::collision::BuildingColliders>,
    layers: Res<crate::systems::layer_state::DataLayers>,
    terrain: Res<crate::systems::terrain::TerrainField>,
    footing: crate::systems::interiors::Footing,
) {
    // Don't move avatar unless the follow camera is active (Free and Earth
    // modes decouple the camera from the avatar).
    if camera_state.mode != crate::resources::CameraMode::Avatar {
        avatar_state.is_walking = false;
        // Let the avatar come to rest rather than freezing mid-stride: leaving
        // the speed latched would have it resume at full pace on returning to
        // Follow, and (online) keep sending that speed as intent.
        avatar_state.ground_speed = 0.0;
        // Same reasoning for the velocity the follow rig leads by: returning to
        // Follow snaps the camera onto its target, and a lead left over from
        // whatever the avatar was doing when the mode changed would place that
        // snap tens of metres off (`free_camera::FOLLOW_LEAD_MAX`).
        avatar_state.velocity = Vec3::ZERO;
        // …and stand it up. The seat is a Follow-mode arrangement: the prompt that
        // offers it only appears there, and a free camera flying away from a
        // seated avatar leaves nothing to get it off the bench.
        avatar_state.stand_up();
        return;
    }
    // Don't move while the UI owns the keyboard (text field focused, edit dialog open,
    // or a Ctrl chord like Ctrl+Shift+A); otherwise WASD/S leak into avatar movement.
    if egui_manager.ui_owns_keyboard(edit_dialog.visible, &keyboard_input) {
        avatar_state.is_walking = false;
        avatar_state.ground_speed = 0.0;
        return;
    }
    if avatar_query.is_empty() {
        return;
    }

    let Ok((mut transform, grid_cell)) = avatar_query.single_mut() else {
        return;
    };
    let delta_time = time.delta().as_secs_f32();

    let move_forward =
        keyboard_input.pressed(KeyCode::KeyW) || keyboard_input.pressed(KeyCode::ArrowUp);
    let move_backward =
        keyboard_input.pressed(KeyCode::KeyS) || keyboard_input.pressed(KeyCode::ArrowDown);
    let move_left =
        keyboard_input.pressed(KeyCode::KeyA) || keyboard_input.pressed(KeyCode::ArrowLeft);
    let move_right =
        keyboard_input.pressed(KeyCode::KeyD) || keyboard_input.pressed(KeyCode::ArrowRight);
    // Gait modifiers. Shift runs, Alt strolls, neither walks — see
    // `systems::locomotion`. Shift is free in Follow (the camera's boost is a Fly
    // binding) and Alt is unused anywhere in this client. Note the *unmodified*
    // gait is the walk: a modifier is for leaving the everyday pace, not for
    // reaching it.
    let run =
        keyboard_input.pressed(KeyCode::ShiftLeft) || keyboard_input.pressed(KeyCode::ShiftRight);
    let stroll =
        keyboard_input.pressed(KeyCode::AltLeft) || keyboard_input.pressed(KeyCode::AltRight);
    avatar_state.gait = locomotion::Gait::from_modifiers(run, stroll);

    // Space jumps, on the press. The avatar does not fly (removed 2026-11-14,
    // `docs/notes/retired/avatar-flight.md`), so a hold means nothing more.
    if keyboard_input.just_pressed(KeyCode::Space) {
        avatar_state.wants_jump = true;
    }
    // The on-screen jump, which the stick has no way to express. Grounded-only
    // for the same reason `SimWorld::step` grants it that way — holding the bit
    // must not climb — and taken as an edge, since `map_osd` sets it for the one
    // frame the button was released on.
    if osd.jump && avatar_state.grounded {
        avatar_state.wants_jump = true;
    }

    let held = {
        let k = wish_dir_camera_relative(0.0, move_forward, move_backward, move_left, move_right);
        if k.length_squared() > 1e-8 {
            Vec2::new(k.x, k.z)
        } else {
            osd.walk
        }
    };
    let az = steer_azimuth(
        &mut avatar_state.steer_latch,
        camera_state.azimuth,
        camera_state.auto_yaw,
        held,
    );
    // The keys and the on-screen stick are one input with two shapes: a
    // direction, and how hard it is being asked for. The keyboard's throttle is
    // always 1 and its *rung* comes from the modifiers; the stick has no
    // modifiers to hold, so its push carries the choice instead
    // (`locomotion::speed_for_throttle`).
    let keys = wish_dir_camera_relative(az, move_forward, move_backward, move_left, move_right);
    let from_keys = keys.length_squared() > 1e-8;
    let (mut wish, throttle) = travel_wish(az, keys, osd.walk);
    let from_person = wish.length_squared() > 1e-8;
    let walk_speed = if from_keys {
        avatar_state.gait.speed()
    } else if from_person {
        locomotion::speed_for_throttle(throttle)
    } else if let Some((dir, gait)) = steered.wish() {
        // Nobody at the controls: an agent's walk, or a walk along a route. Its
        // gait is the avatar's gait for the frame, so the clip and the speed
        // agree the way they do when somebody holds Shift.
        wish = dir;
        avatar_state.gait = gait;
        gait.speed()
    } else {
        locomotion::speed_for_throttle(throttle)
    };
    let has_input = wish.length_squared() > 1e-8;
    avatar_state.is_walking = has_input;
    // Face the way we're travelling, in both run modes. The wish direction — not
    // the camera — is the target, so A/D turn the avatar a quarter turn and S
    // brings it about; only the turn *rate* is smoothed, movement is immediate.
    if let Some(target) = yaw_from_dir(wish) {
        avatar_state.facing_yaw = advance_facing(avatar_state.facing_yaw, target, delta_time);
    }

    // ── Climbing (`systems::climb`) ──────────────────────────────────────────
    //
    // Asked for by the prompt ("Climb up", E), or by Space while walking into
    // a wall low enough to climb: then the climb, not a jump. Online the sim
    // is not told — it is carried there by the wish, as any walk is, with the
    // wall let through (`network::send_network_intent`); the top is the
    // client's floor, as a roof is.
    let online_now = online.is_some();
    let here = if online_now {
        avatar_state.position
    } else {
        transform.translation
    };
    let (at, _) = crate::systems::climb::avatar_frame(online_now, here, Some(grid_cell));
    let soles = if online_now {
        avatar_state.display_position.y
    } else {
        transform.translation.y - GROUND_HEIGHT
    };
    let can_start = avatar_state.climb.is_none()
        && !avatar_state.is_seated()
        && avatar_state.grounded
        && !avatar_state.swimming
        && avatar_state.holding.is_none();
    let ask = if avatar_state.wants_climb {
        let yaw = avatar_state.facing_yaw;
        Some(Vec2::new(yaw.sin(), yaw.cos()))
    } else if avatar_state.wants_jump && has_input {
        Some(Vec2::new(wish.x, wish.z))
    } else {
        None
    };
    avatar_state.wants_climb = false;
    if let (true, Some(dir)) = (can_start, ask) {
        let ledge = colliders
            .ledge_ahead(at, dir, soles, crate::systems::climb::CLIMB_REACH_M)
            .filter(|l| crate::systems::climb::climbable(l.height));
        if let Some(ledge) = ledge {
            avatar_state.climb = Some(crate::systems::climb::Climb::onto(at, soles, &ledge));
            avatar_state.wants_jump = false;
            avatar_state.steer_latch = None;
        }
    }
    if let Some(mut climb) = avatar_state.climb {
        let done = climb.step(delta_time);
        avatar_state.facing_yaw = advance_facing(avatar_state.facing_yaw, climb.yaw(), delta_time);
        let rotation = Quat::from_rotation_y(avatar_state.facing_yaw);
        if transform.rotation != rotation {
            transform.rotation = rotation;
        }
        avatar_state.is_walking = false;
        avatar_state.wants_jump = false;
        avatar_state.move_dir = Vec3::new(climb.dir.x, 0.0, climb.dir.y);
        avatar_state.ground_speed = if done { 0.0 } else { climb.speed };
        let before = transform.translation;
        if !online_now && !done {
            let step = climb.dir * climb.speed * delta_time;
            transform.translation.x += step.x;
            transform.translation.z += step.y;
        }
        if done {
            // On the top: it is the floor from here (`interiors::Footing`
            // holds it through `BuildingColliders::ledge_under`).
            avatar_state.climb = None;
            let ground = if online_now {
                avatar_state.terrain_lift
            } else {
                terrain.ground_at(transform.translation.x, transform.translation.z)
            };
            avatar_state.floor_lift = (climb.top - ground).max(0.0);
            avatar_state.floor_drop_speed = 0.0;
            avatar_state.move_dir = Vec3::ZERO;
            if !online_now {
                transform.translation.y = climb.top + GROUND_HEIGHT;
                avatar_state.vertical_velocity = 0.0;
                avatar_state.grounded = true;
            }
        } else {
            avatar_state.climb = Some(climb);
        }
        if !online_now {
            avatar_state.position = transform.translation;
            avatar_state.display_position = transform.translation;
            avatar_state.velocity = (transform.translation - before) / delta_time.max(1e-4);
        }
        return;
    }

    // Sitting is left by doing anything else. Either — a step or a jump — stands
    // the avatar up, and `stand_up` takes effect this frame, so the input that
    // ended the sit is also acted on rather than swallowed.
    if avatar_state.is_seated() && (has_input || avatar_state.wants_jump) {
        avatar_state.stand_up();
    }
    if avatar_state.is_seated() {
        // Held still and turned to face out of the bench. Everything below is
        // about going somewhere, and this avatar is not.
        avatar_state.is_walking = false;
        avatar_state.ground_speed = 0.0;
        avatar_state.move_dir = Vec3::ZERO;
        avatar_state.wants_jump = false;
        if let Some(seat) = avatar_state.seated {
            avatar_state.facing_yaw = advance_facing(avatar_state.facing_yaw, seat.yaw, delta_time);
        }
        let rotation = Quat::from_rotation_y(avatar_state.facing_yaw);
        if transform.rotation != rotation {
            transform.rotation = rotation;
        }
        // Offline the transform is the height, and nothing below runs: a sitter
        // seated the frame it landed, before the ground under it streamed in,
        // stayed at the height it landed at — a capture's edge seat drawn a
        // metre over the grass (2026-11-15). Stand it on its ground and floor.
        if online.is_none() {
            let (ground, _) =
                ground_or_water(&terrain, transform.translation.x, transform.translation.z);
            let y = GROUND_HEIGHT + ground + avatar_state.floor_lift;
            if (transform.translation.y - y).abs() > 1e-4 {
                transform.translation.y = y;
            }
            avatar_state.vertical_velocity = 0.0;
            avatar_state.grounded = true;
            avatar_state.position = transform.translation;
            avatar_state.display_position = transform.translation;
        }
        return;
    }

    // ── The speed model ──────────────────────────────────────────────────────
    //
    // Ramp a *speed*, and keep the direction across a key release. Both halves
    // matter: assigning the target speed outright is what made the avatar start and
    // stop like a mechanism, and zeroing the direction with the input is what made
    // it stop dead — the wish vector is already zero on the frame the key comes up,
    // so there is nothing left to coast along unless it is remembered here.
    //
    // This runs in *every* run mode, online included, because it is the one
    // movement system that always runs in Follow. `send_network_intent` consumes
    // the result rather than recomputing it, so there is exactly one ramp.
    // The travel direction turns toward the wish in an arc, never in a step: the
    // body turns first (`advance_facing`, above) and the path bends after it.
    // Taken whole, as it was until 2026-11-14, a key press slid the avatar
    // sideways at full speed while its body was still turning — the owner found
    // the turning unrealistic. From a standstill there is no path to bend, so a
    // start goes straight where it is asked.
    // How sharply a running avatar is asked to turn, for the brake below.
    let mut turn_cos = 1.0;
    if has_input {
        let speed = avatar_state.ground_speed;
        avatar_state.move_dir =
            if speed < locomotion::MOVING_EPS || avatar_state.move_dir.length_squared() < 1e-6 {
                wish
            } else {
                turn_cos = avatar_state.move_dir.dot(wish);
                let rate = travel_turn_rate(speed, avatar_state.swimming);
                turn_travel(avatar_state.move_dir, wish, rate * delta_time)
            };
    }
    // In the water every gait is a stroke: the easy one, or the fast one for a
    // run, built and shed slowly because water is thick. A swimmer has nothing
    // to jump off.
    let (target_speed, accel, decel) = if avatar_state.swimming {
        avatar_state.wants_jump = false;
        (
            if has_input {
                locomotion::swim_speed_for(walk_speed)
            } else {
                0.0
            },
            locomotion::SWIM_ACCEL,
            locomotion::SWIM_DECEL,
        )
    } else {
        // Faster than a walk, a sharp turn is braked into: the speed drops
        // with how far the path still has to come round, so an about-face at a
        // run is a stop and a start rather than a hairpin at full speed.
        let brake = if avatar_state.ground_speed > locomotion::WALK_SPEED {
            ((1.0 + turn_cos) / 2.0).max(TURN_BRAKE_FLOOR)
        } else {
            1.0
        };
        (
            if has_input { walk_speed * brake } else { 0.0 },
            locomotion::GROUND_ACCEL,
            locomotion::GROUND_DECEL,
        )
    };
    avatar_state.ground_speed = locomotion::advance_speed(
        avatar_state.ground_speed,
        target_speed,
        delta_time,
        accel,
        decel,
    );
    // Only on a real turn: writing the same rotation back would mark the avatar's
    // Transform changed every frame and re-propagate its whole model subtree.
    let rotation = Quat::from_rotation_y(avatar_state.facing_yaw);
    if transform.rotation != rotation {
        transform.rotation = rotation;
    }

    // Online: the sim owns translation (`network::send_network_intent` sends the
    // wish dir and this facing); nothing below applies.
    if online.is_some() {
        return;
    }

    // Always sync avatar state position with transform
    // This ensures camera can follow the avatar correctly
    avatar_state.position = transform.translation;
    avatar_state.display_position = transform.translation;
    // Where this frame's integration started, so the velocity the follow camera
    // leads by can be *measured* rather than restated. Collision steering, the
    // ground clamp and the altitude factor all move the avatar by less (or by
    // something other) than the nominal speed, and a camera led by a velocity the
    // avatar did not achieve overshoots into every wall it slides along.
    let before = transform.translation;

    if avatar_state.ground_speed > 1e-4 {
        let delta = avatar_state.move_dir * avatar_state.ground_speed * delta_time;
        // Collide against buildings when the buildings layer is on. A building is
        // in the way only at its own height (`collision::Body`), so a jump clears
        // a kerb and not a wall, and a bridge deck is not the house standing
        // under it. It used to be a boolean — anything off the ground passed
        // through everything — which could not tell a deck from the pavement
        // below it.
        //
        // Footprints live in the anchored world-XZ frame, so resolve the move in
        // absolute metres (cell + local). Last frame's storey is part of the
        // ground (`collision::eject_avatar_from_buildings` says why).
        let body = crate::systems::collision::Body::new(
            transform.translation.y - GROUND_HEIGHT,
            terrain.ground_at(transform.translation.x, transform.translation.z)
                + avatar_state.floor_lift,
        );
        // Underground, the surface world is not what is around you: the
        // building over a tunnel is seven metres up and its footprint is not a
        // wall down here, while the tunnel's own wall is the only thing that is
        // (`systems::tunnels`). So the two collision worlds are exclusive, and
        // which one applies is decided by the floor the avatar is standing on.
        let in_bore = crate::systems::tunnels::inside(avatar_state.floor_lift);
        if in_bore || (layers.buildings_enabled && !colliders.is_empty()) {
            use crate::grid::WORLD_CELL_EDGE;
            let base = Vec2::new(
                grid_cell.x as f32 * WORLD_CELL_EDGE,
                grid_cell.z as f32 * WORLD_CELL_EDGE,
            );
            let from = base + Vec2::new(transform.translation.x, transform.translation.z);
            let to = from + Vec2::new(delta.x, delta.z);
            let resolved = if in_bore {
                colliders.slide_in_bore(from, to)
            } else {
                colliders.slide(from, to, body)
            };
            transform.translation.x = resolved.x - base.x;
            transform.translation.z = resolved.y - base.y;
        } else {
            transform.translation.x += delta.x;
            transform.translation.z += delta.z;
        }
    }

    // Handle vertical movement. The avatar models are authored origin-at-feet, so
    // the standing height is the ground surface itself (`GROUND_HEIGHT`) — the old
    // `+ AVATAR_HEIGHT/2` suited only the fox, whose origin is its centre, and it
    // floated the humanoids ~0.45 m.
    //
    // The avatar is always on the gravity arc. Same function as the sim
    // (`motion::step_fall`), so a jump or a fall comes down at the same rate in
    // either run mode.
    // Offline, this transform *is* the avatar's state, so the terrain enters as the
    // ground level itself rather than as a display offset the way it does online
    // (`smooth_online_avatar_display`): lifting the transform after the fact would
    // read as airborne on the next frame and gravity would take it straight back.
    // The storey the avatar is standing on, if it is indoors. Same shape as the
    // terrain above it — the ground level is whatever is under your feet, and a
    // first-floor slab is as much "the ground" as a hillside is. Sampled in the
    // frame `interiors` built its rooms in, which is this transform's. A roof
    // is asked in the colliders' frame, absolute world XZ.
    // In water the ground is where a swimmer's feet hang (`ground_or_water`),
    // and gravity takes the avatar down to it off a quay.
    let (ground, swimming) =
        ground_or_water(&terrain, transform.translation.x, transform.translation.z);
    avatar_state.swimming = swimming;
    let feet = crate::systems::interiors::Feet {
        world: Vec2::new(
            grid_cell.x as f32 * crate::grid::WORLD_CELL_EDGE + transform.translation.x,
            grid_cell.z as f32 * crate::grid::WORLD_CELL_EDGE + transform.translation.z,
        ),
        ground,
        above: transform.translation.y - GROUND_HEIGHT - ground - avatar_state.floor_lift,
        carried: 0.0,
        falling: 0.0,
    };
    let floor = footing.step(
        &mut avatar_state,
        Vec2::new(transform.translation.x, transform.translation.z),
        delta_time,
        feet,
    );
    let ground_y = GROUND_HEIGHT + ground + floor;
    if avatar_state.wants_jump && avatar_state.grounded {
        avatar_state.vertical_velocity = motion::JUMP_SPEED;
    }
    let step = motion::step_fall(
        transform.translation.y,
        avatar_state.vertical_velocity,
        ground_y,
        delta_time,
    );
    transform.translation.y = step.y;
    avatar_state.vertical_velocity = step.vertical_velocity;
    avatar_state.grounded = step.grounded;
    avatar_state.landing_impact = step.landing_speed.unwrap_or(0.0);
    // Consumed either way: a jump asked for in mid-air is not banked until landing.
    avatar_state.wants_jump = false;

    // Update avatar state position to match transform (important for camera following)
    avatar_state.position = transform.translation;
    avatar_state.display_position = transform.translation;
    avatar_state.velocity = (transform.translation - before) / delta_time.max(1e-4);
}

/// How many of the avatar's meshes exist, and how many the view drew.
///
/// A `SystemParam` because the answer takes three queries and its first reader,
/// `shot_harness::drive_capture`, was already at Bevy's 16-parameter ceiling. The
/// three belong together anyway: they are one question asked in three steps, since
/// the model meshes carry no marker of their own. They are glTF nodes instantiated
/// under a holder, so the only thing identifying them as the avatar's is that the
/// avatar is their ancestor.
#[derive(bevy::ecs::system::SystemParam)]
pub struct AvatarReadout<'w, 's> {
    avatars: Query<'w, 's, Entity, With<Avatar>>,
    children: Query<'w, 's, &'static Children>,
    meshes: Query<
        'w,
        's,
        &'static bevy::camera::visibility::ViewVisibility,
        With<bevy::render::mesh::Mesh3d>,
    >,
}

impl AvatarReadout<'_, '_> {
    /// `(meshes under the local avatar, how many the view rendered)`.
    ///
    /// Zero for both when there is no avatar entity at all, which is the honest
    /// answer for a run that has none rather than a reason to fail.
    pub(crate) fn counts(&self) -> (usize, usize) {
        let Ok(avatar) = self.avatars.single() else {
            return (0, 0);
        };
        let (mut total, mut drawn) = (0, 0);
        for entity in self.children.iter_descendants(avatar) {
            if let Ok(visibility) = self.meshes.get(entity) {
                total += 1;
                if visibility.get() {
                    drawn += 1;
                }
            }
        }
        (total, drawn)
    }
}

/// Log the spawned-vs-drawn split whenever it changes.
///
/// `ShotMetrics` has carried `avatar_meshes`/`avatar_rendered` since 2026-08-14,
/// and they answer the invisible-avatar question exactly — `0/0` is "never
/// spawned", `n/0` is "spawned but not drawn". They were unreachable on the one
/// platform that needed them: a scripted capture is driven by CLI flags, and
/// Android has no CLI at all (`lib.rs::parse_cli` hands it a fixed `--offline`,
/// because the zygote's `argv` belongs to `app_process`). So the phone could
/// report `avatar: model ready nodes=78 hidden=0` — every mesh present and
/// unmasked — and still leave "is any of it on screen?" unanswerable without
/// attaching a debugger to a device nobody can attach a debugger to.
///
/// On change rather than per frame, which makes it an instrument instead of
/// chatter: it fires a handful of times while the model streams in and then goes
/// quiet, so a steady state costs one comparison and one log line total. That also
/// makes the *transition* legible — `10/10` dropping to `10/0` names the frame the
/// avatar stopped being drawn, which a periodic sample would blur.
///
/// Deliberately not gated to Android or to a dev build. It is three queries and a
/// tuple compare, the desktop log is where the phone's behaviour gets compared
/// against a known-good one, and a diagnostic that only exists on the platform you
/// cannot debug is a diagnostic nobody has ever seen working.
pub fn log_avatar_draw_counts(
    readout: AvatarReadout,
    camera_state: Res<crate::resources::CameraState>,
    // The camera is big_space's floating origin, so an entity's `GlobalTransform`
    // *is* its offset from the camera in metres — the camera↔avatar distance needs
    // no cell arithmetic, just the length of this.
    avatars: Query<&GlobalTransform, With<Avatar>>,
    mut last: Local<Option<(usize, usize, &'static str, i64)>>,
) {
    let (meshes, rendered) = readout.counts();
    // Skip the initial (0, 0): the avatar entity exists from `Startup` but its
    // glTF is asynchronous, so every run would open with a "no meshes" line that
    // is merely early rather than wrong.
    if (meshes, rendered) == (0, 0) && last.is_none() {
        return;
    }
    let mode = match camera_state.mode {
        crate::resources::CameraMode::Avatar => "follow",
        crate::resources::CameraMode::Free => "fly",
        crate::resources::CameraMode::Orbit => "earth",
    };
    let distance = avatars
        .single()
        .map(|t| t.translation().length())
        .unwrap_or(f32::NAN);
    let banded = distance_band(distance);
    let key = (meshes, rendered, mode, banded);
    if *last == Some(key) {
        return;
    }
    *last = Some(key);
    // `rendered` counts entities that passed view-visibility, which is a CPU-side
    // frustum test — it says "submitted", not "you can see it". An avatar the
    // camera is a kilometre above passes that test and is still sub-pixel, so the
    // distance is logged beside the counts: `10/10` at `distance_m = 1478` is the
    // shape of "drawn, but not where anyone is looking".
    tracing::info!(
        meshes,
        rendered,
        mode,
        distance_m = distance as i64,
        "avatar: meshes drawn (spawned/rendered; distance is camera→avatar)"
    );
}

/// The camera↔avatar distance as [`log_avatar_draw_counts`] compares it: a
/// decade of metres up to a kilometre, so a walking avatar does not re-log
/// every frame while a camera stranded a kilometre away still shows up as one
/// line, and two significant figures beyond. A fixed 10 m band logged **every
/// frame** of a climb to orbit — a line per frame into the phone's logcat for
/// as long as the camera moved (seen on the OnePlus 15, 2026-11-14).
fn distance_band(distance: f32) -> i64 {
    if !distance.is_finite() {
        return -1;
    }
    let d = distance as i64;
    if d < 1000 {
        return (d / 10) * 10;
    }
    let step = 10_i64.pow(d.ilog10() - 1);
    (d / step) * step
}

#[cfg(test)]
mod tests;
