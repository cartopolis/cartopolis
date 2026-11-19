use super::*;

/// Another player arriving slowly inside a wall it can climb climbed it:
/// the clip plays for the climb's length with the lift held, then the top
/// is its ground; walking off it, it drops. One walking through at a walk
/// is passing through a wall its own client does not draw, and stays on
/// the ground; a wall too low to stop anybody is a step.
#[test]
fn a_remote_inside_a_wall_climbed_it_unless_it_walked_through() {
    use crate::systems::climb::CLIMB_SECS;
    let dt = 0.05;
    let (mut lift, mut climb) = remote_ledge_step(0.0, None, 0.8, 0.4, dt);
    assert_eq!((lift, climb), (0.0, Some((0.0, 0.8))), "a climb starts");
    let mut t = 0.0;
    while climb.is_some() {
        (lift, climb) = remote_ledge_step(lift, climb, 0.8, 0.0, dt);
        t += dt;
        if climb.is_some() {
            assert_eq!(lift, 0.0, "held while the clip climbs");
        }
    }
    assert!((t - CLIMB_SECS).abs() <= dt + 1e-4, "{t}");
    assert_eq!(lift, 0.8, "on the top");
    let (off, _) = remote_ledge_step(lift, None, 0.0, 1.8, dt);
    assert!(off < 0.8 && off > 0.0, "drops off the edge: {off}");
    assert_eq!(
        remote_ledge_step(0.0, None, 0.8, 1.8, dt),
        (0.0, None),
        "walked through"
    );
    assert_eq!(
        remote_ledge_step(0.0, None, 0.3, 1.8, dt),
        (0.3, None),
        "a step"
    );
}

/// Every emote has a slot of its own, and the slot is the pack clip that
/// `--clip` calls by the emote's name — the greetings and the throw were
/// appended after the sit and the swim, so the slots are not one run.
#[test]
fn every_emote_plays_its_own_clip() {
    for (slot, emote) in EMOTE_ORDER.iter().enumerate() {
        assert_eq!(emote_slot(*emote), slot, "{emote:?}");
        let clip = human_avatar::CLIP_ORDER[human_avatar::EMOTE_CLIPS[slot]];
        assert_eq!(
            ClipName::parse(clip),
            Some(ClipName::Emote(*emote)),
            "{clip}"
        );
    }
    for name in ClipName::NAMES {
        assert!(ClipName::parse(name).is_some(), "{name}");
    }
}

#[test]
fn a_climb_to_orbit_logs_a_handful_of_bands() {
    assert_eq!(distance_band(1478.0), 1400);
    assert_eq!(distance_band(37.0), 30);
    assert_eq!(distance_band(f32::NAN), -1);
    // From 1 km to 30,000 km in 16 km steps: a line per band, not per step.
    let bands: std::collections::BTreeSet<i64> = (0..1875)
        .map(|i| distance_band(1000.0 + i as f32 * 16_000.0))
        .collect();
    assert!(bands.len() < 200, "{} bands", bands.len());
}

/// A canal two metres below the quay, thirty metres across.
fn canal(x: f32) -> f32 {
    if (100.0..130.0).contains(&x) {
        -2.0
    } else {
        0.0
    }
}

/// Footsteps follow the clip that is playing: the captured walk (1.65 m/s on
/// a 0.9 s cycle) at its own speed steps twice per cycle, and faster ground
/// speed steps faster in proportion. Before, the rate was the Quaternius
/// walk's, about 18 % behind this one's feet.
#[test]
fn footsteps_land_twice_per_cycle_of_the_clip_playing() {
    let walk = ClipGait {
        secs: 0.9,
        speed: 1.65,
    };
    assert!((walk.steps_per_sec(1.65) - 2.0 / 0.9).abs() < 1e-5);
    assert!((walk.steps_per_sec(1.98) - 1.2 * 2.0 / 0.9).abs() < 1e-4);
}

#[test]
fn a_recentring_camera_does_not_steer_a_held_key() {
    let mut latch = None;
    let a = Vec2::new(-1.0, 0.0);
    assert_eq!(steer_azimuth(&mut latch, 0.3, 5.0, a), 0.3);
    // The camera swung 0.4 rad by itself; the key still means what it did.
    assert!((steer_azimuth(&mut latch, 0.7, 5.4, a) - 0.3).abs() < 1e-6);
    // A hand adds 0.2 on top: that part steers.
    assert!((steer_azimuth(&mut latch, 0.9, 5.4, a) - 0.5).abs() < 1e-6);
    // A new direction reads against the camera as it now is.
    assert_eq!(
        steer_azimuth(&mut latch, 0.9, 5.4, Vec2::new(0.0, 1.0)),
        0.9
    );
    assert_eq!(steer_azimuth(&mut latch, 0.9, 5.4, Vec2::ZERO), 0.9);
    assert!(latch.is_none());
}

#[test]
fn an_avatar_high_up_does_not_dip_into_the_canals_below() {
    // 100 m/s at 300 m, rendered at 60 Hz: the old lift was `canal(x)`
    // itself, a two-metre drop for a third of a second.
    let dt = 1.0 / 60.0;
    let (mut lift, mut worst) = (0.0_f32, 0.0_f32);
    for i in 0..120 {
        let x = i as f32 * 100.0 * dt;
        lift = airborne_lift(lift, canal(x), 300.0, dt);
        worst = worst.max(lift.abs());
    }
    assert!(worst < 0.2, "the canal still reached the avatar: {worst} m");
}

#[test]
fn a_remote_avatar_stands_on_this_viewers_ground() {
    // A stranger standing still in the canal-side street two metres below
    // the anchor's datum: the sim says y = 0, and the drawn feet must be on
    // the measured ground, every frame — the lift must not accumulate.
    let dt = 1.0 / 60.0;
    let net = Vec3::new(110.0, 0.0, 5.0);
    let (mut shown, mut lift) = (net, 0.0);
    for _ in 0..240 {
        (shown, lift) =
            remote_display_on_ground(shown, lift, net, Vec3::ZERO, 0.0, dt, |x, _| canal(x));
        assert!(
            (shown.y - -2.0).abs() < 1e-4,
            "drawn at {} over ground at -2",
            shown.y
        );
    }
    assert!((lift - -2.0).abs() < 1e-4);
    assert!((shown.x - 110.0).abs() < 1e-3);
}

#[test]
fn a_remote_walking_off_the_quay_steps_down_with_the_ground() {
    let dt = 1.0 / 60.0;
    let v = Vec3::new(2.0, 0.0, 0.0);
    let (mut shown, mut lift, mut net) =
        (Vec3::new(96.0, 0.0, 0.0), 0.0, Vec3::new(96.0, 0.0, 0.0));
    for i in 0..240 {
        // A fresh snapshot each tick (every third frame), as at 20 Hz.
        let age = if i % 3 == 0 {
            net += v * 3.0 * dt;
            0.0
        } else {
            (i % 3) as f32 * dt
        };
        (shown, lift) = remote_display_on_ground(shown, lift, net, v, age, dt, |x, _| canal(x));
        assert!(
            (shown.y - canal(shown.x)).abs() < 1e-3,
            "at x = {} the feet are at {}, the ground at {}",
            shown.x,
            shown.y,
            canal(shown.x)
        );
    }
    assert!(shown.x > 100.0, "never reached the low street: {}", shown.x);
    assert!((lift - -2.0).abs() < 1e-3);
}

#[test]
fn walking_and_jumping_follow_the_ground_exactly() {
    for height in [
        0.0,
        0.5,
        motion::JUMP_SPEED.powi(2) / (2.0 * motion::GRAVITY),
    ] {
        assert_eq!(airborne_lift(7.0, -2.0, height, 1.0 / 60.0), -2.0);
    }
}

/// Follow a remote avatar walking a straight line at `speed`, with snapshots
/// every 50 ms rendered at 60 Hz, and return how far the drawn avatar ends
/// up from where it truly is.
fn remote_follow_error(speed: f32, velocity_known: bool) -> f32 {
    let v = Vec3::new(speed, 0.0, 0.0);
    let dt = 1.0 / 60.0;
    let (mut t, mut next_snapshot) = (0.0_f32, 0.0_f32);
    let (mut net_pos, mut age) = (Vec3::ZERO, 0.0_f32);
    let mut display = Vec3::ZERO;
    for _ in 0..240 {
        t += dt;
        age += dt;
        if t >= next_snapshot {
            net_pos = v * next_snapshot;
            age = t - next_snapshot;
            next_snapshot += 0.05;
        }
        let net_v = if velocity_known { v } else { Vec3::ZERO };
        display = remote_display_step(display, net_pos, net_v, age, dt);
    }
    display.distance(v * t)
}

/// The whole point of replicating velocity: a remote at a steady run is
/// drawn where it is, not a fixed distance behind it.
#[test]
fn a_remote_at_a_steady_run_is_drawn_where_it_is() {
    let run = 6.0;
    let with = remote_follow_error(run, true);
    let without = remote_follow_error(run, false);
    assert!(with < 0.05, "{with} m behind with the velocity");
    assert!(
        without > 0.3,
        "the position-only follower should trail a run ({without} m) — the test is not \
             measuring what it claims"
    );
}

/// A link that goes quiet leaves the avatar where the extrapolation stopped
/// rather than carrying it on down the street.
#[test]
fn a_stale_remote_stops_being_carried() {
    let v = Vec3::new(6.0, 0.0, 0.0);
    let dt = 1.0 / 60.0;
    let mut display = Vec3::ZERO;
    let mut age = 0.0;
    for _ in 0..600 {
        age += dt;
        display = remote_display_step(display, Vec3::ZERO, v, age, dt);
    }
    let limit = v.x * REMOTE_MAX_EXTRAPOLATION;
    assert!(
        display.x <= limit + 0.3,
        "carried {} m on a snapshot 10 s old (cap {limit} m)",
        display.x
    );
    assert!(display.is_finite());
}

/// Extrapolating the fall at the end of a jump must not push the feet
/// under the floor the sim clamps them to.
#[test]
fn a_falling_remote_is_not_carried_through_the_floor() {
    let net = Vec3::new(0.0, 0.1, 0.0);
    let v = Vec3::new(0.0, -8.0, 0.0);
    let mut display = net;
    let dt = 1.0 / 60.0;
    let mut age = 0.0;
    for _ in 0..30 {
        age += dt;
        display = remote_display_step(display, net, v, age, dt);
        assert!(display.y >= -1e-5, "sank to {}", display.y);
    }
}

/// Turn `yaw` back into the direction the model then faces (+Z rotated by it).
fn facing(yaw: f32) -> Vec3 {
    Quat::from_rotation_y(yaw) * Vec3::Z
}

fn assert_dir(actual: Vec3, expected: Vec3) {
    assert!(
        actual.distance(expected) < 1e-4,
        "expected {expected:?}, got {actual:?}"
    );
}

/// The yaw a direction asks for is the yaw that points the model at it — the
/// property the whole facing chain (render transform, replicated `display_yaw`)
/// rests on.
#[test]
fn yaw_faces_the_direction_it_came_from() {
    for dir in [
        Vec3::Z,
        Vec3::X,
        -Vec3::Z,
        -Vec3::X,
        Vec3::new(1.0, 0.0, 1.0).normalize(),
    ] {
        let yaw = yaw_from_dir(dir).expect("non-zero direction");
        assert_dir(facing(yaw), dir);
    }
    assert!(
        yaw_from_dir(Vec3::ZERO).is_none(),
        "no input must not steer"
    );
}

/// Every WASD combination turns the avatar onto the direction it travels:
/// pressing A walks *and* faces left, S about-faces. Checked against the
/// camera's own frame so it holds at any azimuth.
#[test]
fn every_key_turns_the_avatar_onto_its_travel_direction() {
    for &az in &[0.0, 0.7, 2.5, -1.9] {
        let f = camera_plane_forward(az);
        let r = camera_plane_right(az);
        // (forward, backward, left, right) → expected travel/facing direction.
        let cases: [(bool, bool, bool, bool, Vec3); 5] = [
            (true, false, false, false, f),
            (false, true, false, false, -f),
            (false, false, true, false, -r),
            (false, false, false, true, r),
            (true, false, false, true, (f + r).normalize()),
        ];
        for (fwd, back, left, right, want) in cases {
            let wish = wish_dir_camera_relative(az, fwd, back, left, right);
            assert_dir(wish, want);
            let yaw = yaw_from_dir(wish).expect("a key is held");
            assert_dir(facing(yaw), want);
        }
    }
}

/// The stick steers in the same camera frame the keys do, at any azimuth:
/// pushing it up-screen is W, pushing it right is D. If these two ever
/// disagreed the avatar would walk one way on a desk and another on a phone.
#[test]
fn the_stick_steers_in_the_same_frame_as_the_keys() {
    for &az in &[0.0, 0.7, 2.5, -1.9] {
        let f = camera_plane_forward(az);
        let r = camera_plane_right(az);
        for (stick, want) in [
            (Vec2::new(0.0, 1.0), f),
            (Vec2::new(0.0, -1.0), -f),
            (Vec2::new(1.0, 0.0), r),
            (Vec2::new(-1.0, 0.0), -r),
            (Vec2::new(1.0, 1.0).normalize(), (f + r).normalize()),
        ] {
            let (dir, _) = travel_wish(az, Vec3::ZERO, stick);
            assert_dir(dir, want);
            // …and the avatar turns onto it, exactly as a key makes it.
            assert_dir(facing(yaw_from_dir(dir).expect("pushed")), want);
        }
    }
}

/// The push is a throttle, not a switch: the direction is unit whatever the
/// deflection, and the magnitude comes back beside it for the speed model.
#[test]
fn the_stick_reports_direction_and_push_separately() {
    let (dir, push) = travel_wish(0.0, Vec3::ZERO, Vec2::new(0.0, 0.4));
    assert!((dir.length() - 1.0).abs() < 1e-4, "unit direction: {dir:?}");
    assert!((push - 0.4).abs() < 1e-5, "push carried through: {push}");
    // Clamped to the disc even if a caller hands over a longer vector.
    let (_, over) = travel_wish(0.0, Vec3::ZERO, Vec2::new(3.0, 4.0));
    assert!((over - 1.0).abs() < 1e-5, "{over}");
    // A resting thumb is no input at all — not a slow crawl.
    assert_eq!(travel_wish(0.0, Vec3::ZERO, Vec2::ZERO), (Vec3::ZERO, 0.0));
}

/// Keys beat the stick, and at full throttle, so a thumb resting inside the
/// deadzone (or an OSD frame that arrived late) cannot cap a sprint.
#[test]
fn the_keys_win_the_tie_at_full_throttle() {
    let keys = wish_dir_camera_relative(0.0, true, false, false, false);
    let (dir, push) = travel_wish(0.0, keys, Vec2::new(0.9, -0.9));
    assert_dir(dir, keys);
    assert_eq!(push, 1.0);
}

/// The turn eases rather than snapping, but it always converges — and settles
/// exactly, so a standing avatar stops rewriting its transform.
#[test]
fn facing_eases_toward_the_target_and_settles() {
    let dt = 1.0 / 60.0;
    let target = std::f32::consts::PI; // the worst case: an about-face
    let mut yaw = 0.0_f32;
    let first = advance_facing(yaw, target, dt);
    assert!(
        first > 0.0 && first < target,
        "turn is eased, not a snap: {first}"
    );
    assert!(
        first <= MAX_TURN_RATE * dt + 1e-6,
        "first frame respects the rate cap: {first}"
    );
    let mut frames = 0;
    while wrap_angle_pi(target - yaw).abs() > 1e-6 && frames < 600 {
        yaw = advance_facing(yaw, target, dt);
        frames += 1;
    }
    assert!(
        frames < 60,
        "an about-face lands within a second: {frames} frames"
    );
    assert_eq!(
        advance_facing(yaw, target, dt),
        wrap_angle_pi(target),
        "settled, so no further movement"
    );
}

/// One frame of the online display loop, as `smooth_online_avatar_display`
/// runs it: integrate the prediction, then reconcile toward the last
/// authoritative pose extrapolated by its own age.
fn step_display(
    display: Vec3,
    authority: Vec3,
    velocity: Vec3,
    age: f32,
    error: &mut Vec3,
    dt: f32,
) -> Vec3 {
    let target = extrapolated_target(authority, velocity, age);
    reconcile_display(
        display + velocity * dt,
        target,
        error,
        velocity.length(),
        is_stalled(age),
        dt,
    )
}

/// **Snapshots arrive when the network says so, and that must not become
/// motion.** This is the wobble the reconciler is tuned against, and it has
/// nothing to do with the server disagreeing: here the sim integrates exactly
/// the velocity the client predicts, and the only thing moving the reconcile
/// target about is arrival times drifting either side of the tick.
///
/// A stiff correction turns each late arrival into a shove — at 9/s a 40 ms
/// gap in the news was worth a third of the walking speed, twenty times a
/// second. The assertion is on the *displacement per frame*, because that is
/// what a person sees.
/// Run a steady journey at `speed` against a sim that agrees with us
/// completely, and report the worst a single frame's travel deviated from
/// the nominal — plus the largest error the arrival noise ever opened up.
///
/// The two clocks are kept apart on purpose, because that is where the noise
/// comes from and it is easy to model it away by accident: the sim's
/// position advances on its own 20 Hz tick, and the news of a given tick
/// arrives a latency *plus a varying delay* later. Tie the two together —
/// derive the authority from the arrival time — and the extrapolation
/// cancels the jitter exactly, the test passes at any tuning, and it is
/// measuring nothing.
fn journey(
    speed: f32,
    reconcile: impl Fn(Vec3, Vec3, &mut Vec3, f32, bool, f32) -> Vec3,
) -> (f32, f32) {
    let dt = 1.0 / 60.0;
    let tick = 1.0 / 20.0;
    let v = Vec3::new(0.0, 0.0, -speed);
    let latency = 0.08;
    // What the link adds on top of the latency, per snapshot, cycling.
    let jitter = [0.0, 0.04, -0.01, 0.03, 0.0, 0.05, -0.02, 0.02];

    let mut t = 0.0f32;
    let mut sent = 0usize;
    let mut authority = Vec3::ZERO;
    let mut age = 0.0f32;
    let mut display = Vec3::ZERO;
    let mut error = Vec3::ZERO;
    let (mut worst_step, mut worst_error) = (0.0f32, 0.0f32);
    for frame in 0..1800 {
        t += dt;
        age += dt;
        loop {
            let at = sent as f32 * tick;
            if at + latency + jitter[sent % jitter.len()] > t {
                break;
            }
            authority = v * at;
            age = t - (at + latency + jitter[sent % jitter.len()]);
            sent += 1;
        }
        let target = extrapolated_target(authority, v, age);
        let before = display;
        display = reconcile(
            display + v * dt,
            target,
            &mut error,
            speed,
            is_stalled(age),
            dt,
        );
        // Skip the opening seconds, where the display is still catching up to
        // a lag it started with none of.
        if frame > 300 {
            worst_step =
                worst_step.max(((display - before).length() - speed * dt).abs() / (speed * dt));
            worst_error = worst_error.max((target - display).length());
        }
    }
    (worst_step, worst_error)
}

#[test]
fn arrival_jitter_does_not_reach_the_picture() {
    for speed in [
        crate::systems::locomotion::WALK_SPEED,
        crate::systems::locomotion::RUN_SPEED,
        // The fastest the sim will move an avatar.
        motion::MAX_SPEED,
    ] {
        let (worst, error) = journey(speed, reconcile_display);
        assert!(
            worst < 0.05,
            "at {speed} m/s a frame's travel wobbled by {:.1}% of itself",
            worst * 100.0
        );

        // …and the *filter* is what achieved that, not the correction cap.
        // The cap is a ceiling on a transient; if it were binding on ordinary
        // arrival noise it would be shaping the everyday motion, which is the
        // job this design gives to the filter alone. Measured 2026-09-11:
        // the noise opens 6 cm at a walk against a 57 cm knee.
        let knee = online_correction_speed(speed) / ONLINE_RECONCILE_RATE;
        assert!(
            error < knee,
            "at {speed} m/s arrival noise reaches {error} m, past the {knee} m \
                 where the correction cap starts to bite"
        );
    }
}

/// The same journey under the old single-pole correction, to pin *why* the
/// rates changed rather than leaving them as two numbers somebody will round
/// off. Measured 2026-09-11: 30.5 % against 2.3 %.
#[test]
fn the_old_stiff_correction_is_what_the_wobble_was() {
    /// What `smooth_online_avatar_display` did before 2026-09-11: one pole at
    /// 9/s, straight onto the raw error.
    fn old(
        display: Vec3,
        target: Vec3,
        _error: &mut Vec3,
        speed: f32,
        _stalled: bool,
        dt: f32,
    ) -> Vec3 {
        if display.distance(target) > online_snap_error(speed) {
            target
        } else {
            display.lerp(target, 1.0 - (-9.0f32 * dt).exp())
        }
    }
    let (was, _) = journey(crate::systems::locomotion::WALK_SPEED, old);
    let (now, _) = journey(crate::systems::locomotion::WALK_SPEED, reconcile_display);
    assert!(
        was > 0.2,
        "the 9/s correction used to pass {:.1}% of each frame's travel as wobble",
        was * 100.0
    );
    assert!(now * 5.0 < was, "now {now}, was {was}");
}

/// …and it is still reconciliation: a server that genuinely disagrees is
/// agreed with, inside about a second, without ever tugging.
#[test]
fn a_real_disagreement_is_corrected_without_a_tug() {
    let dt = 1.0 / 60.0;
    let speed = crate::systems::locomotion::WALK_SPEED;
    let v = Vec3::new(0.0, 0.0, -speed);
    // Three metres off, and under the snap threshold, so this is drift to be
    // eased out rather than a teleport to be arrived at.
    let offset = Vec3::new(3.0, 0.0, 0.0);
    assert!(offset.length() < online_snap_error(speed));

    let mut display = offset;
    let mut error = Vec3::ZERO;
    let mut worst_step = 0.0f32;
    let mut t = 0.0f32;
    for _ in 0..90 {
        t += dt;
        let before = display;
        // Snapshots on time, so the only error is the one we started with.
        display = step_display(display, v * t, v, 0.0, &mut error, dt);
        worst_step = worst_step.max((display - before).length() / dt);
    }
    let left = (display - v * t).length();
    assert!(left < 0.3, "1.5 s later the display is still {left} m out");
    assert!(
        worst_step <= speed + online_correction_speed(speed) + 1e-3,
        "the correction added {worst_step} m/s to a {speed} m/s walk"
    );
}

/// A teleport still arrives rather than being eased into — and takes the
/// filter's memory with it, or the next frames would correct for a
/// disagreement that was resolved by moving.
#[test]
fn a_teleport_snaps_and_forgets() {
    let speed = crate::systems::locomotion::WALK_SPEED;
    let mut error = Vec3::new(0.4, 0.0, 0.0);
    let here = Vec3::ZERO;
    let there = Vec3::new(0.0, 0.0, -2_000.0);
    let landed = reconcile_display(here, there, &mut error, speed, false, 1.0 / 60.0);
    assert_eq!(landed, there);
    assert_eq!(error, Vec3::ZERO);
}

/// The snap threshold is a *duration* of travel, not a fixed distance.
///
/// This is the property avatar flight stuttered without: reconciliation's own
/// standing error is `speed / ONLINE_RECONCILE_RATE`, so a threshold that does
/// not grow with speed is eventually crossed by nothing having gone wrong, and
/// the display hard-sets to a stale extrapolation every frame instead of
/// easing.
#[test]
fn the_snap_threshold_outruns_reconciliations_own_error() {
    // Standing and walking keep the old threshold.
    assert_eq!(online_snap_error(0.0), ONLINE_SNAP_ERROR);
    assert_eq!(
        online_snap_error(crate::systems::locomotion::WALK_SPEED),
        ONLINE_SNAP_ERROR
    );

    // At a run, at the sim's cap, and at a speed nothing reaches today (so
    // the rule stays a duration for whatever moves faster next), the
    // threshold stays clear of the error reconciliation settles at, with
    // room for the latency the extrapolation cap misses.
    for speed in [
        crate::systems::locomotion::RUN_SPEED,
        motion::MAX_SPEED,
        250.0,
    ] {
        let settled = speed / ONLINE_RECONCILE_RATE;
        assert!(
            online_snap_error(speed) > settled * 2.0,
            "at {speed} m/s the threshold is {} against a standing error of {settled}",
            online_snap_error(speed)
        );
    }

    // …and never so generous that a teleport eases instead of snapping. The
    // shortest jump anyone makes is to another part of the same city.
    assert!(online_snap_error(250.0) < 1_000.0);
}

/// The prediction keeps moving through the two ordinary events that used to
/// park it, and still stops for a link that has genuinely gone.
///
/// Past [`ONLINE_MAX_EXTRAPOLATION`] the reconcile target stands still while
/// the display keeps integrating, so the ease brakes the avatar to a halt —
/// which at a walk costs 20 cm and nobody sees, and at speed is a dead stop
/// and a lurch. The window therefore has to outlast a hiccup and
/// nothing more.
#[test]
fn the_prediction_rides_out_a_late_snapshot() {
    // The two events that reach this code as a late snapshot and are not
    // faults. Both are why 0.15 s (three server ticks) was too tight.
    let server_tick = 1.0 / 20.0;
    assert!(
        !is_stalled(server_tick * 4.0),
        "four ticks of jitter on a mobile link is ordinary, not a stall"
    );
    assert!(
        !is_stalled(0.2),
        "a 200 ms frame hitch delays our own mailbox drain by its own length"
    );

    // …and it still parks a link that has actually stopped. The brake takes
    // about `1 / ONLINE_RECONCILE_RATE` on top, so this is what decides
    // whether a dropped connection reads as stopping or as flying on.
    assert!(is_stalled(1.0), "a whole second with nothing is a stall");
    assert!(
        ONLINE_MAX_EXTRAPOLATION + 1.0 / ONLINE_RECONCILE_RATE < 1.0,
        "an unconfirmed avatar should be stopped inside a second"
    );

    // The window must stay under the snap's own duration, or a server that has
    // really stopped agreeing with us is extrapolated away instead of
    // surfacing as divergence and then as a snap.
    assert!(ONLINE_MAX_EXTRAPOLATION < ONLINE_SNAP_SECONDS);

    // What the window buys, at a run: the target keeps pace with the
    // prediction through a late snapshot instead of standing still.
    let v = Vec3::new(0.0, 0.0, -crate::systems::locomotion::RUN_SPEED);
    let here = Vec3::ZERO;
    for age in [server_tick, 0.2, 0.4] {
        let moved = extrapolated_target(here, v, age).distance(here);
        let expected = v.length() * age;
        assert!(
            (moved - expected).abs() < 1e-3,
            "at {age}s the target should have travelled {expected} m, not {moved}"
        );
    }
    // Past the window it stops, which is the brake being applied on purpose.
    let capped = extrapolated_target(here, v, 5.0).distance(here);
    assert!(
        (capped - v.length() * ONLINE_MAX_EXTRAPOLATION).abs() < 1e-3,
        "the window is not being clamped: {capped}"
    );

    // A standing avatar has no extrapolation to do at any age, so a bad link
    // can never move one.
    assert_eq!(extrapolated_target(here, Vec3::ZERO, 9.0), here);
}

/// The arrival grows out of a thin column, overshoots once, and finishes at
/// exactly the identity — a residue would leave every teleported avatar
/// slightly the wrong size for the rest of the run.
#[test]
fn the_teleport_arrival_settles_on_standing_height() {
    assert_eq!(teleport_scale(1.0), Vec3::ONE);
    assert_eq!(teleport_scale(2.0), Vec3::ONE, "and stays there");

    let start = teleport_scale(0.0);
    assert!(start.x < 0.2 && start.z < 0.2, "starts thin: {start:?}");
    assert!(start.y > 1.5, "and tall: {start:?}");
    assert_eq!(start.x, start.z, "round, not flattened: {start:?}");

    // Monotone widening, and a single overshoot in height — several would
    // read as a wobble rather than as a body settling.
    let steps: Vec<Vec3> = (0..=40).map(|i| teleport_scale(i as f32 / 40.0)).collect();
    for w in steps.windows(2) {
        assert!(w[1].x >= w[0].x - 1e-5, "width went backwards: {w:?}");
    }
    // Height comes *down* to standing from the column, overshooting it once
    // on the way — so it approaches 1.0 from above and never dips under, which
    // is a body settling rather than a knee buckling.
    assert!(
        steps.iter().any(|s| s.y > 1.0 + 0.05),
        "the overshoot is too small to see"
    );
    assert!(
        steps.iter().all(|s| s.y >= 1.0 - 1e-4),
        "the arrival must not squash below standing height"
    );
    assert!(
        steps.iter().all(|s| s.y < 3.0 && s.x > 0.0),
        "nothing absurd along the way"
    );
    // The last stretch is monotone down onto 1.0: no second bounce.
    for w in steps[steps.len() * 3 / 4..].windows(2) {
        assert!(w[1].y <= w[0].y + 1e-5, "bounced on the way in: {w:?}");
    }
}

/// **The arrival is applied to the real holder, and taken back off it.**
///
/// The pure curve is checked above; this checks the system that carries it,
/// because both of the defects this feature actually had were in the
/// *plumbing* rather than in the arithmetic, and neither could fail a test on
/// `teleport_scale`:
///
/// 1. `Local<f32>` starts at zero. With the timer counting *up*, zero meant
///    "just started" and every launch opened on the first frame of an arrival
///    nobody asked for.
/// 2. The pose is written only while something is happening, so the frame the
///    arrival *ends* has to write the resting value. Without that the holder
///    keeps the last non-identity scale for the rest of the run — a teleported
///    avatar left permanently a few per cent thin, which nothing would report.
#[test]
fn a_teleport_scales_the_holder_and_then_restores_it() {
    // No `TimePlugin`: it drives `Time` off the wall clock, which between two
    // `app.update()` calls in a test is microseconds — the arrival would still
    // be on its first frame at the end of the test and every assertion below
    // would pass or fail for the wrong reason. An inserted `Time` is nobody
    // else's to overwrite, so the test owns the clock outright.
    let mut app = App::new();
    app.insert_resource(Time::<()>::default())
        .init_resource::<AvatarState>()
        .add_systems(Update, update_avatar_model_pose);
    let holder = app
        .world_mut()
        .spawn((AvatarHolder, Transform::IDENTITY))
        .id();
    app.world_mut().spawn(Avatar).add_child(holder);

    let scale_of = |app: &App| {
        app.world()
            .get::<Transform>(holder)
            .expect("the holder spawned above")
            .scale
    };
    let step = |app: &mut App, secs: f32| {
        app.world_mut()
            .resource_mut::<Time<()>>()
            .advance_by(std::time::Duration::from_secs_f32(secs));
        app.update();
    };

    // Startup: nobody has teleported, so the holder must be left alone.
    app.update();
    assert_eq!(
        scale_of(&app),
        Vec3::ONE,
        "an avatar that has not teleported must not play an arrival"
    );

    // A teleport lands: the body is pinched and tall on the next frame.
    app.world_mut()
        .resource_mut::<AvatarState>()
        .teleport_sequence = 1;
    step(&mut app, 1.0 / 60.0);
    let arriving = scale_of(&app);
    assert!(
        arriving.x < 0.5 && arriving.y > 1.2,
        "the arrival should start as a thin column: {arriving:?}"
    );

    // …and once it is over the holder is back at exactly its spawn identity,
    // not merely near it.
    step(&mut app, TELEPORT_ARRIVE_SECS);
    assert_eq!(
        scale_of(&app),
        Vec3::ONE,
        "the holder must be restored on the frame the arrival ends"
    );
    // Still identity a frame later, i.e. it settled rather than passing through.
    step(&mut app, 1.0 / 60.0);
    assert_eq!(scale_of(&app), Vec3::ONE);
}

/// The jump tuck tips the avatar *forward* — head toward its own +Z, feet
/// back — about the chest, and is the identity when grounded. Checked here
/// because a pose has no other way to fail loudly: a sign slip would throw
/// the avatar on its back and only a screenshot would say so.
#[test]
fn the_tuck_pitches_forward_about_the_chest() {
    assert_eq!(body_pose(0.0, 1.0), Transform::IDENTITY);

    let pose = body_pose(JUMP_TUCK, 1.0);
    let pivot = Vec3::Y * LEAN_PIVOT_Y;
    assert!(
        pose.transform_point(pivot).distance(pivot) < 1e-5,
        "the chest is the pivot, so it must not move: {:?}",
        pose.transform_point(pivot)
    );

    let head = Vec3::new(0.0, 1.7, 0.0);
    let leant = pose.transform_point(head);
    assert!(leant.z > 0.1, "head leads, toward model-forward: {leant:?}");
    assert!(
        leant.y < head.y,
        "and drops below standing height: {leant:?}"
    );
    let feet = pose.transform_point(Vec3::ZERO);
    assert!(feet.z < 0.0, "feet trail behind: {feet:?}");
}

/// The path bends rather than steps: a quarter turn at a walk takes a few
/// frames and stays a unit direction throughout, lands exactly, and a run
/// comes round wider than a walk.
#[test]
fn the_path_bends_into_a_turn() {
    let dt = 1.0 / 60.0;
    let (north, east) = (Vec3::Z, Vec3::X);
    let first = turn_travel(north, east, travel_turn_rate(1.8, false) * dt);
    assert!((first.length() - 1.0).abs() < 1e-5);
    assert!(
        first.dot(east) > 0.0 && first.dot(east) < 0.5,
        "bent, not stepped: {first}"
    );
    let frames_at = |speed: f32| {
        let (mut d, mut n) = (north, 0);
        while d != east && n < 600 {
            d = turn_travel(d, east, travel_turn_rate(speed, false) * dt);
            n += 1;
        }
        n
    };
    let (walk, run) = (frames_at(1.8), frames_at(6.0));
    assert!(walk < run, "a run turns wider: {walk} vs {run} frames");
    assert!(
        walk as f32 * dt < 0.25,
        "a walk turns within a step: {walk} frames"
    );
    assert!(travel_turn_rate(1.0, true) < travel_turn_rate(1.0, false));
}

/// Turning takes the short way round the circle: from just left of −π the
/// target just right of +π is a hair away, not a full sweep back.
#[test]
fn facing_turns_the_short_way_across_the_wrap() {
    let pi = std::f32::consts::PI;
    let stepped = advance_facing(pi - 0.05, -pi + 0.05, 1.0 / 60.0);
    assert!(
        stepped.abs() > pi - 0.05,
        "should step across ±π, not unwind through 0: {stepped}"
    );
}
