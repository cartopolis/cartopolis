use super::*;
use crate::systems::routing::{RouteStep, TravelMode};
use std::time::Duration;

/// A straight 1 km route due east, with a turn in the middle, in metres that
/// are easy to check by hand.
fn fixture() -> Route {
    // ~0.001° of longitude at 53°N is ~67 m; use latitude steps instead, where
    // a degree is a constant 110.54 km, so the expected distances are exact.
    let pts: Vec<[f64; 2]> = (0..=10)
        .map(|i| [53.0 + f64::from(i) * 0.001, 6.0])
        .collect();
    let mut points = vec![[52.9995, 6.0]]; // start pin
    points.extend(pts.iter().copied());
    points.push([53.0105, 6.0]); // end pin
    let levels = vec![0; points.len()];
    Route {
        mode: TravelMode::Car,
        points,
        levels,
        distance_m: 1105.4,
        duration_s: 120.0,
        steps: vec![
            RouteStep {
                maneuver: Maneuver::Depart,
                name: Some("First street".into()),
                distance_m: 552.7,
                duration_s: 60.0,
                at: [53.0, 6.0],
            },
            RouteStep {
                maneuver: Maneuver::Left,
                name: Some("Second street".into()),
                distance_m: 552.7,
                duration_s: 60.0,
                at: [53.005, 6.0],
            },
            RouteStep {
                maneuver: Maneuver::Arrive,
                name: None,
                distance_m: 0.0,
                duration_s: 0.0,
                at: [53.01, 6.0],
            },
        ],
    }
}

/// The measured polyline must cover the *road*, not the pins: driving the
/// lead-in from a pin (which may be across a canal — the case
/// `navigation`'s thin connector exists for) is not part of the route.
#[test]
fn geometry_measures_the_road_only() {
    let route = fixture();
    let geom = RouteGeom::build(&route).expect("a routable fixture");
    assert_eq!(geom.points.len(), 11, "the two pins are excluded");
    assert!(
        (geom.total_m - 1105.4).abs() < 1.0,
        "0.01° of latitude is 1105.4 m, got {}",
        geom.total_m
    );
    // Steps land where their coordinates say, not where their distances do.
    assert!(geom.step_along[0].abs() < 1.0);
    assert!(
        (geom.step_along[1] - 552.7).abs() < 1.0,
        "{:?}",
        geom.step_along
    );
    assert!((geom.step_along[2] - 1105.4).abs() < 1.0);
}

/// Progress must fall out of the *projection*, not the nearest vertex: a
/// position mid-segment reports mid-segment, or the remaining distance freezes
/// on a long straight and then jumps.
#[test]
fn progress_projects_onto_the_segment() {
    let route = fixture();
    let geom = RouteGeom::build(&route).unwrap();
    // A quarter of the way along, 20 m off to the side.
    let side = 20.0 / (111_320.0 * 53.0_f64.to_radians().cos());
    let p = project_onto_route(&geom, &route, [53.0025, 6.0 + side]);
    assert!((p.along_m - 276.35).abs() < 2.0, "along {}", p.along_m);
    assert!((p.off_route_m - 20.0).abs() < 1.0, "off {}", p.off_route_m);
    assert!(
        (p.remaining_m - 829.05).abs() < 2.0,
        "remaining {}",
        p.remaining_m
    );
    // Time is scaled from the planner's answer, so three quarters left of 120 s.
    assert!((p.remaining_s - 90.0).abs() < 1.0, "eta {}", p.remaining_s);
}

/// The card must name the manoeuvre being *driven toward*, and must not flip
/// to the next one the instant the junction is reached — that is the moment it
/// is being read.
#[test]
fn the_next_manoeuvre_survives_arriving_at_it() {
    let route = fixture();
    let geom = RouteGeom::build(&route).unwrap();

    let early = project_onto_route(&geom, &route, [53.001, 6.0]);
    assert_eq!(early.step, 1, "the turn ahead, not the departure behind");
    assert!((early.step_distance_m - 442.16).abs() < 2.0);

    // Right at the turn: still the turn.
    let at_turn = project_onto_route(&geom, &route, [53.005, 6.0]);
    assert_eq!(at_turn.step, 1, "flipped away at the junction itself");

    // Well past it: the arrival.
    let past = project_onto_route(&geom, &route, [53.006, 6.0]);
    assert_eq!(past.step, 2);
}

/// At the start of a drive the card names the first real manoeuvre, never the
/// departure it is standing on ("◯ Now").
#[test]
fn the_card_never_names_the_departure() {
    let route = fixture();
    let geom = RouteGeom::build(&route).unwrap();
    let p = project_onto_route(&geom, &route, route.steps[0].at);
    assert_eq!(route.steps[0].maneuver, Maneuver::Depart);
    assert_eq!(p.step, 1, "the card opened on the departure");
    assert!(p.step_distance_m > NOW_M, "and counted it down as Now");
}

/// A route that comes back past itself — out along one street and home along
/// the next one over — must keep the vehicle on the leg it is driving. Nearest
/// over the whole line puts a fix nearer the return leg ten minutes ahead.
#[test]
fn a_hint_keeps_the_projection_on_its_own_leg() {
    let east = |m: f64| m / (111_320.0 * 53.0_f64.to_radians().cos());
    let north = |m: f64| m / 110_540.0;
    // North 1 km, east 30 m, back south 1 km.
    let points = vec![
        [53.0, 6.0],
        [53.0 + north(1000.0), 6.0],
        [53.0 + north(1000.0), 6.0 + east(30.0)],
        [53.0, 6.0 + east(30.0)],
    ];
    let mut cum = vec![0.0];
    for w in points.windows(2) {
        cum.push(cum.last().unwrap() + plane_distance_m(w[0], w[1]));
    }
    let geom = RouteGeom {
        total_m: *cum.last().unwrap(),
        points,
        cum,
        step_along: Vec::new(),
    };
    // Half-way up the first leg, drifted 20 m toward the return leg.
    let at = [53.0 + north(500.0), 6.0 + east(20.0)];

    let (anywhere, _) = project_near(&geom, at, None);
    assert!(
        anywhere > 1200.0,
        "the global nearest is the way back: {anywhere}"
    );

    let (near, off) = project_near(&geom, at, Some(490.0));
    assert!((near - 500.0).abs() < 2.0, "along {near}");
    assert!((off - 20.0).abs() < 1.0, "off {off}");
}

/// Being off the route must be *measured*, not silently projected — the card
/// switches to "Off route" from this number.
#[test]
fn off_route_distance_is_reported() {
    let route = fixture();
    let geom = RouteGeom::build(&route).unwrap();
    let far = 200.0 / (111_320.0 * 53.0_f64.to_radians().cos());
    let p = project_onto_route(&geom, &route, [53.005, 6.0 + far]);
    assert!(p.off_route_m > OFF_ROUTE_M, "off {}", p.off_route_m);
}

/// The compass cycle must be a cycle: three orientations and back, or the
/// button strands the user in one of them.
#[test]
fn the_orientation_cycle_returns_to_its_start() {
    let start = MapOrientation::default();
    assert_eq!(start.next().next().next(), start);
    assert_ne!(start.next(), start);
}

/// Arrival time is a wall clock, so it has to wrap at midnight rather than
/// print "25:10" — and it has to say when it is not the *local* wall clock,
/// which is the one error in it a driver could not spot.
#[test]
fn arrival_wraps_past_midnight_and_admits_utc() {
    assert_eq!(arrival_clock(18, 57, 6.0 * 60.0, true), "19:03");
    assert_eq!(arrival_clock(23, 50, 30.0 * 60.0, true), "00:20");
    assert_eq!(arrival_clock(9, 0, 0.0, true), "09:00");
    // `WorldClock` is UTC until the anchor's timezone resolves, and stays UTC
    // whenever the user asked for it. An unlabelled time is then wrong by hours
    // in a format that looks exactly right.
    assert_eq!(arrival_clock(18, 57, 6.0 * 60.0, false), "19:03 UTC");
}

/// The arrow must point along +Z (the avatar's forward, so a course yaw turns
/// it correctly) and face up, or it is backface-culled into nothing — the
/// failure that cost the route line two debugging sessions.
#[test]
fn the_arrow_points_forward_and_faces_up() {
    let mesh = arrow_mesh();
    let positions = match mesh.attribute(Mesh::ATTRIBUTE_POSITION).unwrap() {
        bevy::render::mesh::VertexAttributeValues::Float32x3(v) => v.clone(),
        _ => panic!("positions are not Float32x3"),
    };
    // Two flat layers: the navy rim at 0 and the white face over it.
    assert!(
        positions
            .iter()
            .all(|p| p[1] == 0.0 || p[1] == ARROW_FACE_LIFT),
        "the arrow must be two flat layers"
    );
    let nose = |y: f32| {
        positions
            .iter()
            .filter(|p| p[1] == y)
            .map(|p| p[2])
            .fold(f32::MIN, f32::max)
    };
    let (face, rim) = (nose(ARROW_FACE_LIFT), nose(0.0));
    assert!(
        (face - ARROW_HALF_LENGTH).abs() < 1e-3,
        "nose is not at +Z: {face}"
    );
    assert!(
        rim > face,
        "the rim must lead the face's nose: {rim} vs {face}"
    );
    let tail = |y: f32| {
        positions
            .iter()
            .filter(|p| p[1] == y)
            .map(|p| p[2])
            .fold(f32::MAX, f32::min)
    };
    assert!(tail(0.0) < tail(ARROW_FACE_LIFT), "…and trail its tails");

    let Some(Indices::U32(indices)) = mesh.indices() else {
        panic!("expected u32 indices")
    };
    for t in indices.chunks_exact(3) {
        let (a, b, c) = (
            positions[t[0] as usize],
            positions[t[1] as usize],
            positions[t[2] as usize],
        );
        let cross = (b[2] - a[2]) * (c[0] - a[0]) - (b[0] - a[0]) * (c[2] - a[2]);
        assert!(cross > 0.0, "triangle {t:?} wound downward (cross {cross})");
    }
}

/// Entering must arm the mode and leaving must release the subscription — a
/// watch left running keeps the browser's location indicator lit after the
/// user has stopped navigating.
#[test]
fn entering_and_leaving_are_symmetric() {
    let mut driver = DriverState::default();
    driver.enter();
    assert!(driver.active && driver.watch.is_some() && driver.snap_pending);
    driver.exit();
    assert!(!driver.active && driver.watch.is_none() && driver.progress.is_none());
}

/// The reroute cooldown is the loop breaker: a solve can legitimately return a
/// route the vehicle is still off, and without a floor between attempts the
/// display would re-solve every frame it stayed there.
#[test]
fn a_reroute_waits_out_its_cooldown() {
    let mut driver = DriverState::default();
    let now = Instant::now();
    assert!(
        driver.reroute_ready(now),
        "the first reroute is never blocked"
    );
    driver.reroute_at = Some(now);
    assert!(!driver.reroute_ready(now));
    assert!(!driver.reroute_ready(now + Duration::from_secs_f32(REROUTE_COOLDOWN * 0.5)));
    assert!(driver.reroute_ready(now + Duration::from_secs_f32(REROUTE_COOLDOWN + 0.1)));
}

/// Entering driver mode a second time must not inherit the previous drive's
/// cooldown — that would block the first reroute of the new one.
#[test]
fn entering_clears_the_reroute_state() {
    let mut driver = DriverState::default();
    driver.enter();
    driver.reroute_at = Some(Instant::now());
    driver.reroute_gen = Some(7);
    driver.rerouting = true;
    driver.exit();
    driver.enter();
    assert!(driver.reroute_at.is_none() && driver.reroute_gen.is_none());
    assert!(!driver.rerouting);
    assert!(driver.reroute_ready(Instant::now()));
}

/// The end of the route has to land inside [`ARRIVE_M`], or the arrival state
/// is unreachable and the card counts "Arrive · 0 m" forever — which is what
/// it did.
#[test]
fn the_end_of_the_route_is_within_reach_of_arriving() {
    let route = fixture();
    let geom = RouteGeom::build(&route).unwrap();
    let at_end = project_onto_route(&geom, &route, [53.01, 6.0]);
    assert!(
        at_end.remaining_m <= ARRIVE_M,
        "remaining {} at the last road vertex",
        at_end.remaining_m
    );
    // …and a few hundred metres short of it is emphatically not arriving.
    let short = project_onto_route(&geom, &route, [53.007, 6.0]);
    assert!(
        short.remaining_m > ARRIVE_M,
        "remaining {}",
        short.remaining_m
    );
}

/// The rig only closes in on things you actually *do*. A `Continue` reached
/// mid-leg would otherwise pull the camera in for a vertex of a curve, and
/// the view would breathe its way along every bend.
#[test]
fn only_a_real_manoeuvre_counts_as_a_turn_ahead() {
    let mut route = fixture();
    let geom = RouteGeom::build(&route).unwrap();
    assert!(
        project_onto_route(&geom, &route, [53.001, 6.0]).turn_ahead,
        "a Left is a turn"
    );
    route.steps[1].maneuver = Maneuver::Continue;
    assert!(!project_onto_route(&geom, &route, [53.001, 6.0]).turn_ahead);
    // Arriving is one: the destination is exactly where the detail matters.
    assert!(project_onto_route(&geom, &route, [53.008, 6.0]).turn_ahead);
}

/// The countdown ladder: coarse far out, fine near, and "Now" at the junction
/// — and never the "1000 m" that rounding to the nearest 50 produces if the
/// unit is chosen before the rounding.
#[test]
fn the_countdown_coarsens_with_distance() {
    assert_eq!(countdown(0.0), "Now");
    assert_eq!(countdown(NOW_M), "Now");
    assert_eq!(countdown(24.0), "20 m");
    assert_eq!(countdown(96.0), "100 m");
    assert_eq!(countdown(230.0), "250 m");
    assert_eq!(countdown(980.0), "1.0 km");
    assert_eq!(countdown(4_240.0), "4.2 km");
    assert_eq!(countdown(23_400.0), "23 km");
    // Monotonic, so the number never counts *up* as the junction approaches.
    let mut prev = f64::NEG_INFINITY;
    for i in (0..20_000).step_by(7) {
        let m = f64::from(i);
        let shown = parse_countdown(&countdown(m));
        assert!(
            shown >= prev - 1.0,
            "counted up at {m} m: {shown} after {prev}"
        );
        prev = shown;
    }
}

/// Metres back out of a rendered countdown, for the monotonicity check above.
fn parse_countdown(s: &str) -> f64 {
    if s == "Now" {
        return 0.0;
    }
    let (n, unit) = s.split_once(' ').unwrap();
    let n: f64 = n.parse().unwrap();
    if unit == "km" {
        n * 1000.0
    } else {
        n
    }
}

/// The rig rides the speedometer and closes onto a junction, and — the part
/// worth pinning — it is exactly the orientation's own pose in neither case,
/// so a standstill on a long leg is still the tuned view.
#[test]
fn the_rig_stretches_with_speed_and_tightens_onto_a_junction() {
    let far = Some(Progress {
        step_distance_m: 4_000.0,
        turn_ahead: true,
        ..default()
    });
    assert!(
        (rig_scale(0.0, far) - 1.0).abs() < 1e-6,
        "a standstill is the tuned pose"
    );
    assert!((rig_scale(0.0, None) - 1.0).abs() < 1e-6);

    // Speed only stretches, and it stops stretching past the reference.
    let at_ref = rig_scale(RIG_SPEED_REF_KMH / 3.6, far);
    assert!(
        (at_ref - (1.0 + RIG_SPEED_STRETCH)).abs() < 1e-5,
        "{at_ref}"
    );
    assert!(
        (rig_scale(300.0 / 3.6, far) - at_ref).abs() < 1e-5,
        "clamped past the reference"
    );
    assert!(rig_scale(50.0 / 3.6, far) > 1.0);

    // At the junction itself, the tightest the rig gets.
    let at_turn = Some(Progress {
        step_distance_m: 0.0,
        turn_ahead: true,
        ..default()
    });
    assert!((rig_scale(0.0, at_turn) - RIG_JUNCTION_TIGHTEN).abs() < 1e-6);
    // …but not for something that is not a manoeuvre.
    let straight_on = Some(Progress {
        step_distance_m: 0.0,
        turn_ahead: false,
        ..default()
    });
    assert!((rig_scale(0.0, straight_on) - 1.0).abs() < 1e-6);

    // Monotonic in the approach, so the zoom is a glide and never a bounce.
    let mut prev = 0.0;
    for i in 0..=200 {
        let d = RIG_JUNCTION_M * 2.0 * i as f32 / 200.0;
        let s = rig_scale(
            0.0,
            Some(Progress {
                step_distance_m: f64::from(d),
                turn_ahead: true,
                ..default()
            }),
        );
        assert!(s >= prev - 1e-6, "not monotonic at {d} m");
        prev = s;
    }
}

/// Every orientation must put the marker in the lower third — far enough down
/// that the screen is spent on the road ahead, not so far that it is under the
/// speed pill with nothing visible beside it. The flat rigs failed this at 83%
/// and nobody could see it, because the machine this is built on cannot take a
/// screenshot and the number is not in any log.
#[test]
fn every_orientation_puts_the_marker_in_the_lower_third() {
    // Bevy's `PerspectiveProjection::default()`, which `setup_camera` keeps.
    const FOV_Y: f32 = std::f32::consts::FRAC_PI_4;
    // Comfortably inside the band, not merely on the right side of it: the
    // rig scale moves the distance by ±55% and the framing must not drift out.
    const BAND: std::ops::RangeInclusive<f32> = 0.64..=0.78;

    for orientation in [
        MapOrientation::Perspective,
        MapOrientation::HeadingUp,
        MapOrientation::NorthUp,
    ] {
        let (base, pitch, lookahead) = orientation.rig();
        // Across everything `rig_scale` can do: standing still on a long leg,
        // at the reference speed, and at the tightest a junction pulls it.
        for scale in [RIG_JUNCTION_TIGHTEN, 1.0, 1.0 + RIG_SPEED_STRETCH] {
            let d = base * scale;
            let y = marker_screen_y(d, pitch, d * lookahead, FOV_Y);
            assert!(
                BAND.contains(&y),
                "{orientation:?} at ×{scale}: marker at {y:.3} down the screen",
            );
        }
    }
}

/// The projection itself, against two cases that can be reasoned about without
/// it — otherwise the framing test above is only checking its own arithmetic.
#[test]
fn the_marker_projection_agrees_with_its_degenerate_cases() {
    const FOV_Y: f32 = std::f32::consts::FRAC_PI_4;
    // Aiming at the vehicle all but centres it — "all but", because the look
    // target is the 1.5 m eye point and the marker is the ground under it, so
    // the marker sits a hair *below* centre and the hair shrinks with
    // distance. Never above it, at any tilt.
    let mut last = f32::INFINITY;
    for (d, p) in [(60.0, 0.9), (170.0, 0.38), (240.0, 1.35)] {
        let y = marker_screen_y(d, p, 0.0, FOV_Y);
        assert!((0.5..0.52).contains(&y), "d={d} p={p}: {y}");
        assert!(y < last, "the eye-height offset must shrink with distance");
        last = y;
    }
    // Aiming further ahead always pushes it further down, never back up.
    let mut prev = 0.0;
    for i in 0..=50 {
        let y = marker_screen_y(240.0, 1.35, 240.0 * 0.01 * i as f32, FOV_Y);
        assert!(y >= prev, "aiming further ahead moved the marker up at {i}");
        prev = y;
    }
}

/// A peek accumulates, clamps, and — the part that makes it safe to use while
/// driving — puts itself back without being asked.
#[test]
fn a_peek_expires_and_eases_home() {
    let mut peek = Peek::default();
    assert!(!peek.held(), "the driving pose is not a peek");

    apply_gesture(
        &mut peek,
        Gesture {
            zoom: 1.5,
            turn: 0.4,
            tilt: 0.1,
        },
        0.5,
    );
    assert!(peek.held());
    assert!((peek.zoom - 1.5).abs() < 1e-6);
    assert!((peek.azimuth - 0.4).abs() < 1e-6);

    // Zoom is bounded whatever the wheel does — a chase view is not a globe.
    for _ in 0..100 {
        apply_gesture(
            &mut peek,
            Gesture {
                zoom: 1.15,
                ..default()
            },
            0.5,
        );
    }
    assert!((peek.zoom - PEEK_ZOOM_MAX).abs() < 1e-4, "{}", peek.zoom);
    for _ in 0..200 {
        apply_gesture(
            &mut peek,
            Gesture {
                zoom: 1.0 / 1.15,
                ..default()
            },
            0.5,
        );
    }
    assert!((peek.zoom - PEEK_ZOOM_MIN).abs() < 1e-4, "{}", peek.zoom);

    // An idle frame while it is still held changes nothing.
    let before = (peek.zoom, peek.azimuth, peek.pitch);
    apply_gesture(&mut peek, Gesture::default(), 0.5);
    assert_eq!((peek.zoom, peek.azimuth, peek.pitch), before);

    // Released (the button, or the timeout), idle frames ease it home and it
    // snaps to exactly the driving pose rather than creeping at it forever.
    peek.since = None;
    for _ in 0..200 {
        apply_gesture(&mut peek, Gesture::default(), 0.5);
    }
    assert_eq!(peek.zoom, 1.0);
    assert_eq!(peek.azimuth, 0.0);
    assert_eq!(peek.pitch, 0.0);
    assert!(peek.since.is_none());
}

/// Leaving must hand back a camera with no driver-mode state left on it. The
/// lookahead is the one that bites: it is only consulted while driving, so a
/// stale value sits there invisibly until the *next* drive starts with the aim
/// point of the last one.
#[test]
fn leaving_clears_the_peek() {
    let mut driver = DriverState::default();
    driver.enter();
    apply_gesture(
        &mut driver.peek,
        Gesture {
            zoom: 2.0,
            turn: 1.0,
            tilt: 0.2,
        },
        0.5,
    );
    assert!(driver.peeking());
    driver.recentre();
    assert!(!driver.peeking(), "re-centring gives the rig back at once");
    driver.exit();
    assert_eq!(driver.peek.zoom, 1.0);
    assert_eq!(driver.peek.azimuth, 0.0);
}

/// The "then" preview keys off the gap to the manoeuvre *after* the next one,
/// so it appears for two turns in quick succession and stays away otherwise.
#[test]
fn the_then_preview_only_fires_on_a_close_pair() {
    let mut route = fixture();
    let far = Progress {
        step: 0,
        ..default()
    };
    route.steps[1].distance_m = 900.0;
    assert!(next_step_gap(&route, far) > THEN_PREVIEW_M);
    route.steps[1].distance_m = 120.0;
    assert!(next_step_gap(&route, far) <= THEN_PREVIEW_M);
    // No step after the last one: never previews, never panics.
    let last = Progress {
        step: route.steps.len() - 1,
        ..default()
    };
    assert!(next_step_gap(&route, last).is_infinite());
}

/// The snapped marker has to land at exactly the distance the card is counting
/// down from — the same projection, run forwards — or the arrow and the number
/// disagree about where the vehicle is.
#[test]
fn the_marker_snaps_onto_the_route_at_its_own_distance() {
    let route = fixture();
    let geom = RouteGeom::build(&route).unwrap();

    // A quarter of the way along, 20 m off to the side: the projection says
    // 276 m along, and the snapped point is 276 m along the line itself.
    let side = 20.0 / (111_320.0 * 53.0_f64.to_radians().cos());
    let p = project_onto_route(&geom, &route, [53.0025, 6.0 + side]);
    let (on, _) = point_along(&geom, p.along_m).expect("a point on the line");
    assert!(
        (on[1] - 6.0).abs() < 1e-9,
        "the snapped point must be on the line, not beside it: {on:?}"
    );
    let (along_again, off_again) = nearest_on_polyline(&geom.points, &geom.cum, on);
    assert!((along_again - p.along_m).abs() < 0.5, "{along_again}");
    assert!(off_again < 0.01, "the snapped point is on the route by 0");

    // The ends clamp rather than running off the polyline.
    let start = point_along(&geom, -50.0).unwrap().0;
    assert!((start[0] - geom.points[0][0]).abs() < 1e-9, "{start:?}");
    let end = point_along(&geom, geom.total_m * 2.0).unwrap().0;
    let last = *geom.points.last().unwrap();
    assert!((end[0] - last[0]).abs() < 1e-9, "{end:?} vs {last:?}");
}

/// …but the marker must not be *aimed* by the road when the vehicle is plainly
/// not going that way. A route driven backwards would otherwise show an arrow
/// pointing at the windscreen's opposite.
#[test]
fn a_snapped_yaw_defers_to_a_contradicting_course() {
    use std::f32::consts::{FRAC_PI_2, PI};
    // Agreeing within a quarter turn: take the road's angle, which is the
    // whole point — it removes the jitter of a measured course.
    assert!((snapped_yaw(0.0, 0.2) - 0.2).abs() < 1e-6);
    assert!((snapped_yaw(0.0, -0.2) - -0.2).abs() < 1e-6);
    assert!((snapped_yaw(0.0, FRAC_PI_2 - 0.01) - (FRAC_PI_2 - 0.01)).abs() < 1e-6);
    // Reversed: keep the measured course.
    assert!((snapped_yaw(0.0, PI) - 0.0).abs() < 1e-6);
    assert!((snapped_yaw(0.0, PI - 0.1) - 0.0).abs() < 1e-6);
    // …and it must wrap, or a course near ±π compares against the wrong branch.
    assert!((snapped_yaw(PI - 0.05, -PI + 0.05) - (-PI + 0.05)).abs() < 1e-6);
}

/// The fix's quality has to be a *state*, because "no fix yet", "50 m in a
/// tunnel" and "locked on" all rendered as the same confident navigation.
#[test]
fn a_fix_is_classified_before_it_is_believed() {
    // Desktop: no provider, so there is nothing to complain about and nothing
    // to suppress — driver mode narrates the avatar there.
    assert_eq!(
        gps_quality(false, false, None, None),
        GpsQuality::NoProvider
    );
    assert!(!GpsQuality::NoProvider.unreliable());
    assert!(GpsQuality::NoProvider.chip().is_none());

    // A watch running with nothing back yet is a wait worth showing.
    assert_eq!(gps_quality(true, false, None, None), GpsQuality::Waiting);
    assert!(GpsQuality::Waiting.chip().is_some());

    // A good fix says nothing at all *on the driving card* — and carries its
    // accuracy anyway, because the locate button's whole job is to say so.
    assert_eq!(
        gps_quality(true, false, Some(6.0), Some(0.5)),
        GpsQuality::Good(6.0)
    );
    assert!(GpsQuality::Good(6.0).chip().is_none());
    assert_eq!(GpsQuality::Good(6.0).caption().as_deref(), Some("±6 m"));

    // Poor and stale both stand the reroute down: a corridor refetch triggered
    // by tunnel-mouth noise costs seconds at the worst possible moment.
    let poor = gps_quality(true, false, Some(GPS_POOR_M + 20.0), Some(0.5));
    assert!(matches!(poor, GpsQuality::Poor(_)));
    assert!(poor.unreliable());
    assert!(poor.chip().unwrap().contains("80 m"));
    let stale = gps_quality(true, false, Some(5.0), Some(GPS_STALE_S + 1.0));
    assert_eq!(stale, GpsQuality::Stale);
    assert!(stale.unreliable());
    // An explicit provider error beats every inference about the fix.
    assert_eq!(
        gps_quality(true, true, Some(5.0), Some(0.1)),
        GpsQuality::Error
    );
}

/// The locate button's caption must say something in **every** tracking state.
///
/// This is the inversion of the chip's rule and the reason the two are
/// separate methods. A phone cannot reach a tooltip, so the caption is the
/// only place the button's state is legible there — and a healthy receiver
/// falling silent is exactly the complaint the caption exists to answer: a lit
/// button with nothing beside it looks identical whether the GPS is locked on
/// or was refused permission ten minutes ago.
#[test]
fn the_locate_caption_speaks_in_every_state_a_provider_can_reach() {
    for state in [
        GpsQuality::Waiting,
        GpsQuality::Error,
        GpsQuality::Stale,
        GpsQuality::Poor(80.0),
        GpsQuality::Good(4.0),
    ] {
        assert!(
            state.caption().is_some_and(|c| !c.is_empty()),
            "{state:?} left the button with no caption"
        );
    }
    // The one silence, and it is honest: a desktop has no receiver, so an
    // accuracy there would be a number nothing measured.
    assert!(GpsQuality::NoProvider.caption().is_none());

    // The accuracy is the positive evidence — "it is working, this well".
    assert_eq!(format_accuracy(0.014), "±1 cm");
    assert_eq!(format_accuracy(0.3), "±30 cm");
    assert_eq!(format_accuracy(0.999), "±1 m");
    assert_eq!(GpsQuality::Good(4.0).caption().as_deref(), Some("±4 m"));
    assert_eq!(GpsQuality::Poor(80.0).caption().as_deref(), Some("±80 m"));
    assert_eq!(GpsQuality::Good(4.0).accuracy_m(), Some(4.0));
    assert_eq!(GpsQuality::Waiting.accuracy_m(), None);
}

/// A kilometre-scale accuracy is an ordinary indoor reading, not a curiosity,
/// and "±1247 m" claims four significant digits about a confidence interval.
#[test]
fn a_coarse_accuracy_is_written_in_kilometres() {
    assert_eq!(format_accuracy(4.4), "±4 m");
    assert_eq!(format_accuracy(999.0), "±999 m");
    assert_eq!(format_accuracy(1247.0), "±1 km");
    assert_eq!(format_accuracy(12_400.0), "±12 km");
}

/// The cue ladder: once per band per manoeuvre, re-armed by the next one, and
/// silent on the frame driver mode starts — a "Now" chime for whatever junction
/// you happen to be beside is not a way to begin a drive.
/// The rule that decides a write from a teleport, in the units it is written
/// in. `GPS_TELEPORT_M` is metres and `lat_lng_to_world` answers in metres of
/// the anchored flat frame, and a threshold that silently compared degrees —
/// or tile fractions — would read as "always near", i.e. as exactly the
/// avatar-warping behaviour this replaced, with nothing to show for itself.
#[test]
fn a_far_fix_jumps_only_when_it_is_sure_of_itself() {
    // Near is a write whatever the accuracy: there is no jump to doubt.
    assert_eq!(reach_of(150.0, 5_000.0), FixReach::Near);
    // A good fix in another city is a jump.
    assert_eq!(reach_of(180_000.0, 12.0), FixReach::Far);
    // A ten-minute-old cached fix (`stale_accuracy_m`: + 600 s × 13 m/s)
    // five kilometres off says too little to move the person on.
    assert_eq!(reach_of(5_000.0, 7_800.0), FixReach::Doubtful);
    // So does a fix with no accuracy worth the name.
    assert_eq!(reach_of(50_000.0, f64::NAN), FixReach::Doubtful);
    assert_eq!(reach_of(50_000.0, f64::INFINITY), FixReach::Doubtful);
}

#[test]
fn a_distant_fix_is_a_teleport_and_a_nearby_one_is_not() {
    // Groningen, at the zoom the map anchors on.
    let anchor = (53.2194, 6.5665, 14);
    let here = DVec3::ZERO;

    // The anchor itself is zero metres away, by definition.
    assert!(gps_gap_m(anchor, anchor.0, anchor.1, here) < 1.0);

    // ~111 m north (0.001° of latitude) is an ordinary step between two fixes
    // and must stay a position write.
    let step = gps_gap_m(anchor, anchor.0 + 0.001, anchor.1, here);
    assert!(
        (80.0..160.0).contains(&step),
        "0.001° of latitude should read as ~111 m, got {step}"
    );
    assert!(step < GPS_TELEPORT_M);

    // Amsterdam is a different city and must not be reached by writing a
    // number into the avatar's transform.
    let city = gps_gap_m(anchor, 52.3676, 4.9041, here);
    assert!(
        city > GPS_TELEPORT_M,
        "a fix 150 km away must teleport, gap was {city} m"
    );

    // Symmetric in the avatar's own position: an avatar standing 10 km east of
    // the anchor is far from a fix *at* the anchor, which is the case that
    // matters — the map is built around the anchor, not around the fix.
    let away = DVec3::new(10_000.0, 0.0, 0.0);
    assert!(gps_gap_m(anchor, anchor.0, anchor.1, away) > GPS_TELEPORT_M);
}

/// What goes on the wire when a device fix is applied online is the **local**
/// half of the split, and this is the test that says so. The sim assigns a
/// snapshot position straight into `Transform::translation`, so handing it the
/// absolute world metres instead would look right on the client that sent it
/// and put the avatar a whole cell edge — ten kilometres — out for the sim and
/// for every other viewer. The two halves are only distinguishable once the
/// position is past a cell boundary, which is exactly the case a long drive
/// reaches and a test at the origin never would.
#[test]
fn the_wire_gets_the_cell_local_half_of_a_fix() {
    // Inside the first cell: the local translation *is* the world position, so
    // this arm cannot tell a correct implementation from a wrong one.
    let (cell, local) = placement(1_500.0, -2_500.0, 0.0);
    assert_eq!((cell.x, cell.z), (0, 0));
    assert!((local.x - 1_500.0).abs() < 1e-3 && (local.z + 2_500.0).abs() < 1e-3);

    // Past the first cell in both axes — the case a long drive reaches within
    // the hour, and the only one where the two halves differ at all. Cells are
    // *centred* on their index, so the local translation runs to half an edge
    // either side rather than from zero to a whole one.
    let (cell, local) = placement(23_000.0, -14_000.0, 0.0);
    assert_eq!((cell.x, cell.z), (2, -1));
    assert!(
        local.x.abs() <= WORLD_CELL_EDGE && local.z.abs() <= WORLD_CELL_EDGE,
        "a local translation may never exceed the cell edge, got {local:?}"
    );
    // …and the two halves still add back up to where the fix actually was,
    // which is the property the sim depends on: it is handed `local` alone and
    // the client keeps `cell` to itself.
    let rebuilt_x =
        f64::from(cell.x as f32).mul_add(f64::from(WORLD_CELL_EDGE), f64::from(local.x));
    let rebuilt_z =
        f64::from(cell.z as f32).mul_add(f64::from(WORLD_CELL_EDGE), f64::from(local.z));
    assert!(
        (rebuilt_x - 23_000.0).abs() < 1e-2,
        "x rebuilt to {rebuilt_x}"
    );
    assert!(
        (rebuilt_z + 14_000.0).abs() < 1e-2,
        "z rebuilt to {rebuilt_z}"
    );
}

/// The avatar stands on the ground, whatever the last position said. A fix
/// carries no height, so a `y` that had gone negative (a prediction dipping
/// through the floor before a correction) must not be handed back to the sim
/// as an underground placement.
#[test]
fn a_fix_never_places_the_avatar_below_the_ground() {
    let (_, local) = placement(0.0, 0.0, -12.0);
    assert!(local.y >= motion::GROUND_Y, "y was {}", local.y);
}

/// …and never *above* it either. An avatar standing on the sim's floor must
/// be placed on that floor: lifting it even five centimetres makes the
/// prediction drop it back every frame, and every drop is a landing — the
/// thud and the camera dip that played continuously while GPS walked the
/// avatar. Run through the same fall the prediction runs, so the property
/// pinned is "no landing", not a number.
#[test]
fn a_fix_leaves_a_standing_avatar_standing() {
    let (_, local) = placement(120.0, -40.0, motion::GROUND_Y);
    assert_eq!(local.y, motion::GROUND_Y);
    let mut y = local.y;
    let mut vy = 0.0;
    for _ in 0..240 {
        // Every frame re-places the avatar, as `locate` does between fixes.
        y = placement(120.0, -40.0, y).1.y;
        let step = motion::step_fall(y, vy, motion::GROUND_Y, 1.0 / 120.0);
        assert_eq!(step.landing_speed, None, "a standing avatar landed");
        assert!(step.grounded);
        y = step.y;
        vy = step.vertical_velocity;
    }
}

#[test]
fn a_cue_fires_once_per_band_and_re_arms_at_the_next_turn() {
    let mut mem = CueMemory::default();
    let at = |step: usize, m: f64| CueInputs {
        active: true,
        step,
        band: cue_band(m, true),
        rerouting: false,
        arrived: false,
    };

    // The first driving frame is silent, whatever it finds.
    assert_eq!(next_cue(&mut mem, at(0, 60.0)), None);
    // …and having recorded that band, it does not fire it late.
    assert_eq!(next_cue(&mut mem, at(0, 55.0)), None);
    // Deeper is a cue; the same band again is not, in either direction.
    assert_eq!(next_cue(&mut mem, at(0, 10.0)), Some(Cue::Now));
    assert_eq!(next_cue(&mut mem, at(0, 9.0)), None);
    assert_eq!(
        next_cue(&mut mem, at(0, 120.0)),
        None,
        "no cue counting back up"
    );

    // The next manoeuvre re-arms the whole ladder.
    assert_eq!(
        next_cue(&mut mem, at(1, 500.0)),
        None,
        "nothing to prepare for"
    );
    assert_eq!(next_cue(&mut mem, at(1, 380.0)), Some(Cue::Prepare));
    assert_eq!(next_cue(&mut mem, at(1, 300.0)), None);
    assert_eq!(next_cue(&mut mem, at(1, 140.0)), Some(Cue::Approach));
    assert_eq!(next_cue(&mut mem, at(1, 12.0)), Some(Cue::Now));

    // A `Continue` mid-leg is not a manoeuvre and must never chime — the noded
    // graph puts one at every vertex of a curve.
    assert_eq!(cue_band(10.0, false), None);
}

/// The two one-off cues are edges, not states: held for as long as the reroute
/// takes, they would otherwise fire every frame of it.
#[test]
fn the_reroute_and_arrival_cues_fire_once_each() {
    let mut mem = CueMemory::default();
    let frame = |rerouting, arrived| CueInputs {
        active: true,
        step: 0,
        band: None,
        rerouting,
        arrived,
    };
    assert_eq!(next_cue(&mut mem, frame(false, false)), None, "entering");
    assert_eq!(next_cue(&mut mem, frame(true, false)), Some(Cue::Reroute));
    assert_eq!(next_cue(&mut mem, frame(true, false)), None);
    assert_eq!(next_cue(&mut mem, frame(false, false)), None);
    assert_eq!(next_cue(&mut mem, frame(true, false)), Some(Cue::Reroute));

    assert_eq!(next_cue(&mut mem, frame(true, true)), Some(Cue::Arrived));
    assert_eq!(next_cue(&mut mem, frame(true, true)), None);

    // Leaving driver mode forgets everything, so the next drive starts silent
    // rather than announcing the previous trip's arrival.
    assert_eq!(
        next_cue(
            &mut mem,
            CueInputs {
                active: false,
                step: 0,
                band: None,
                rerouting: true,
                arrived: true,
            }
        ),
        None
    );
    assert_eq!(
        next_cue(&mut mem, frame(true, true)),
        None,
        "entering again"
    );
}

/// Every cue must be a decodable WAV, since nothing here has a decoder to fail
/// loudly — an unparseable clip is silence with no message anywhere.
#[test]
fn every_cue_is_a_well_formed_wav() {
    for cue in [
        Cue::Prepare,
        Cue::Approach,
        Cue::Now,
        Cue::Reroute,
        Cue::Arrived,
    ] {
        let wav = cue_wav(cue_notes(cue));
        assert_eq!(&wav[0..4], b"RIFF", "{cue:?}");
        assert_eq!(&wav[8..12], b"WAVE", "{cue:?}");
        let data_len = u32::from_le_bytes(wav[40..44].try_into().unwrap()) as usize;
        assert_eq!(wav.len(), 44 + data_len, "{cue:?}: header/payload disagree");
        assert_eq!(
            u32::from_le_bytes(wav[24..28].try_into().unwrap()),
            CUE_SAMPLE_RATE
        );
        // Long enough to be heard over a road, short enough not to still be
        // playing at the junction it announces.
        let secs = data_len as f32 / 2.0 / CUE_SAMPLE_RATE as f32;
        assert!((0.10..=0.60).contains(&secs), "{cue:?} runs {secs}s");
    }
    // The first sample is the attack's first step, never a full-amplitude
    // discontinuity — a click is what a driver hears as a fault.
    let wav = cue_wav(cue_notes(Cue::Now));
    assert_eq!(i16::from_le_bytes(wav[44..46].try_into().unwrap()), 0);
}

/// Guidance that has to be switched on before it works is guidance nobody has.
#[test]
fn the_guidance_cues_are_on_by_default() {
    assert!(DriverState::default().cues.0);
}

/// The chrome has to fit the screen it is on, down to the 274 pt an Android
/// phone reports: the card and the bar inside the margins, and the three
/// things stacked over the bar's ends — the speed disc, the view buttons and
/// the GPS note between them — side by side without touching.
#[test]
fn the_chrome_fits_every_viewport_it_is_drawn_on() {
    for width in [274.0_f32, 320.0, 400.0, 640.0, 800.0, 1280.0] {
        let compact = width < crate::systems::egui_manager::COMPACT_WIDTH_PTS;
        let l = driver_layout(width, compact);
        let usable = width - 2.0 * CARD_MARGIN + 1e-3;
        assert!(l.card_width <= usable, "the card overflows {width} pt");
        assert!(l.bar_width <= usable, "the bar overflows {width} pt");
        // Speed disc left, two round buttons right, the note's floor between.
        let row = SPEED_DISC + 80.0 + 2.0 * theme::ROUND_BUTTON_SIZE + 4.0 * theme::OSD_GAP;
        assert!(
            row <= l.bar_width + 1e-3,
            "the row over the bar overlaps at {width} pt"
        );
    }
    // A phone's card spans the screen, as the reference's does; a desktop's
    // is a card in the corner.
    assert_eq!(
        driver_layout(400.0, true).card_width,
        400.0 - 2.0 * CARD_MARGIN
    );
    assert_eq!(driver_layout(1280.0, false).card_width, CARD_WIDTH);
}

/// The overview fits the rest of the route: further back for a longer one,
/// further on a portrait phone than on a landscape window, never closer than
/// the flat chase view and never past its ceiling.
#[test]
fn the_overview_pulls_back_to_fit_the_route() {
    let frame = |half: f32| {
        OverviewFrame::around([Vec2::ZERO, Vec2::new(0.0, 2.0 * half)].into_iter()).unwrap()
    };
    let short = frame(300.0).distance(1.6);
    let long = frame(3_000.0).distance(1.6);
    assert!(long > short, "{long} vs {short}");
    assert!(
        frame(3_000.0).distance(0.45) > long,
        "a portrait screen fits less"
    );
    assert!(frame(1.0).distance(1.6) >= FLAT_DISTANCE);
    assert!(frame(500_000.0).distance(0.45) <= OVERVIEW_MAX_DISTANCE);
    // The centre is where the route is, not where the vehicle is.
    assert_eq!(frame(300.0).centre, Vec2::new(0.0, 300.0));
    assert!(OverviewFrame::around(std::iter::empty()).is_none());
}

/// The whole point of the glide: between two fixes the marker must cover the
/// ground *evenly*. An exponential ease passes every "it gets there" test and
/// still lurches, so this asserts the per-frame steps are alike — which is the
/// property a person actually sees.
#[test]
fn a_glide_spreads_one_fix_evenly_over_the_frames_after_it() {
    let mut glide = FixGlide::default();
    let mut t = Instant::now();
    let dt = 1.0 / 60.0;
    let step = Duration::from_secs_f32(dt);

    // The first fix of a session is placed outright, so pressing the button
    // still centres rather than sliding in from wherever the avatar stood.
    assert_eq!(glide.advance((0.0, 0.0), true, t, dt), (0.0, 0.0));

    // A second fix, a second later and 10 m east. It teaches the glide the
    // cadence *and* is the first one gliding applies to: from here the marker
    // is aimed to arrive as the next report is due.
    t += Duration::from_secs_f32(1.0);
    let mut x = glide.advance((10.0, 0.0), true, t, dt).0;
    let first_step = x;
    assert!(
        first_step > 0.0 && first_step < 0.5,
        "one frame took {first_step} m of a 10 m fix — that is the lurch this exists to remove"
    );
    for frame in 1..30 {
        t += step;
        let next = glide.advance((10.0, 0.0), false, t, dt).0;
        let moved = next - x;
        assert!(
            (moved - first_step).abs() < first_step * 0.5,
            "frame {frame} moved {moved} m against a first frame of {first_step} m"
        );
        x = next;
    }
    assert!(
        x > 4.0 && x < 10.0,
        "half a second in, the marker should be about half way, not {x}"
    );

    // Once the next fix is overdue the marker sits on the last known position
    // rather than inventing one — a receiver that has gone quiet is a receiver
    // that has gone quiet.
    t += Duration::from_secs_f32(1.0);
    assert_eq!(glide.advance((10.0, 0.0), false, t, dt).0, 10.0);
}

/// A re-anchor and a teleport both arrive here as an enormous jump in the
/// target. Gliding through one would sail the marker across the map.
#[test]
fn a_glide_arrives_rather_than_travelling_an_impossible_step() {
    let mut glide = FixGlide::default();
    let mut t = Instant::now();
    let dt = 1.0 / 60.0;
    glide.advance((0.0, 0.0), true, t, dt);
    t += Duration::from_secs_f32(1.0);
    glide.advance((5.0, 0.0), true, t, dt);
    t += Duration::from_secs_f32(1.0);
    // A whole grid cell away: not travel at any speed a vehicle reaches.
    assert_eq!(glide.advance((10_000.0, 0.0), true, t, dt), (10_000.0, 0.0));
}

/// A reset must make the next fix a placement again, or the marker glides in
/// from a coordinate that belonged to a frame which no longer exists.
#[test]
fn a_reset_makes_the_next_fix_a_placement() {
    let mut glide = FixGlide::default();
    let t = Instant::now();
    let dt = 1.0 / 60.0;
    glide.advance((0.0, 0.0), true, t, dt);
    glide.reset();
    assert_eq!(glide.advance((60.0, 40.0), true, t, dt), (60.0, 40.0));
}
