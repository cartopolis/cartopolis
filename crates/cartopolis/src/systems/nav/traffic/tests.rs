use super::*;

fn line(points: &[[f32; 2]], flow: Flow) -> DriveLine {
    DriveLine {
        points: points.to_vec(),
        flow,
        speed: 10.0,
        half_width: 3.0,
        uses: Uses {
            drive: true,
            walk: true,
            cycle: true,
        },
        deck: vec![f32::NAN; points.len()],
        on_deck: false,
        pavements: Pavements::BOTH,
    }
}

fn net_with(points: &[[f32; 2]]) -> DriveNetwork {
    let mut net = DriveNetwork::default();
    net.insert_cell((0, 0), 0.0, 0.0, vec![line(points, Flow::Both)]);
    net
}

/// A cell's lines land at the cell's own origin, and carry a running length —
/// which is what lets a car be advanced without walking the line each frame.
#[test]
fn a_cell_places_its_roads_and_measures_them() {
    let mut net = DriveNetwork::default();
    net.insert_cell(
        (1, 2),
        1000.0,
        -500.0,
        vec![line(&[[0.0, 0.0], [30.0, 0.0], [30.0, 40.0]], Flow::Both)],
    );
    let l = net
        .get(LineKey {
            cell: (1, 2),
            index: 0,
        })
        .expect("the road is where the cell put it");
    assert_eq!(l.points[0], Vec2::new(1000.0, -500.0));
    assert!((l.length() - 70.0).abs() < 1e-3, "length {}", l.length());
}

/// Walking a line must give a position that advances monotonically and a
/// heading that is a unit vector — a car steers by both.
#[test]
fn walking_a_line_advances_and_keeps_a_unit_heading() {
    let net = net_with(&[[0.0, 0.0], [100.0, 0.0], [100.0, 100.0]]);
    let l = net
        .get(LineKey {
            cell: (0, 0),
            index: 0,
        })
        .expect("one road");
    let mut last = f32::MIN;
    for step in 0..=20 {
        let s = step as f32 * 10.0;
        let (pos, dir) = l.at(s);
        assert!((dir.length() - 1.0).abs() < 1e-3, "heading not unit: {dir}");
        let travelled = pos.x + pos.y;
        assert!(travelled >= last - 1e-3, "went backwards at {s}");
        last = travelled;
    }
}

/// Past the end the walk clamps rather than wrapping. A car about to be handed
/// off should sit at the end of its road, not teleport to the start of it.
#[test]
fn walking_past_the_end_clamps() {
    let net = net_with(&[[0.0, 0.0], [50.0, 0.0]]);
    let l = net
        .get(LineKey {
            cell: (0, 0),
            index: 0,
        })
        .expect("one road");
    let (end, _) = l.at(l.length());
    let (past, _) = l.at(l.length() * 4.0);
    assert_eq!(end, past);
    let (start, _) = l.at(-20.0);
    assert_eq!(start, l.points[0]);
}

/// Retiring a cell takes its roads with it, and a car holding a key into it
/// gets `None` rather than a stale road — which is the whole reason a car
/// stores a key and not a reference.
#[test]
fn retiring_a_cell_invalidates_its_keys() {
    let mut net = net_with(&[[0.0, 0.0], [80.0, 0.0]]);
    let key = LineKey {
        cell: (0, 0),
        index: 0,
    };
    assert!(net.get(key).is_some());
    let before = net.revision;
    net.remove_cell(&(0, 0));
    assert!(net.get(key).is_none(), "a retired road must not answer");
    assert_ne!(net.revision, before, "consumers must be told");
}

/// `near` has to find roads by any of their points, not only their start — a
/// long road whose far end is beside the camera is a road beside the camera.
#[test]
fn near_finds_a_road_by_its_far_end() {
    let net = net_with(&[[0.0, 0.0], [500.0, 0.0]]);
    assert_eq!(
        net.near(Vec2::new(495.0, 0.0), 30.0, AgentKind::Car).len(),
        1
    );
    assert_eq!(
        net.near(Vec2::new(250.0, 400.0), 30.0, AgentKind::Car)
            .len(),
        0
    );
}

/// The two pools must not see each other's network. A footway is not a road
/// and a motorway is not a pavement; `near` filters by use so a recycled agent
/// is never offered a line it may not travel.
#[test]
fn each_pool_only_sees_its_own_network() {
    let mut net = DriveNetwork::default();
    let mut road = line(&[[0.0, 0.0], [100.0, 0.0]], Flow::Both);
    road.uses = Uses {
        drive: true,
        walk: false,
        cycle: false,
    };
    let mut path = line(&[[0.0, 50.0], [100.0, 50.0]], Flow::Both);
    path.uses = Uses {
        drive: false,
        walk: true,
        cycle: true,
    };
    net.insert_cell((0, 0), 0.0, 0.0, vec![road, path]);

    let here = Vec2::new(50.0, 25.0);
    assert_eq!(net.near(here, 60.0, AgentKind::Car).len(), 1, "one road");
    assert_eq!(net.near(here, 60.0, AgentKind::Walker).len(), 1, "one path");
    // …and they are not the same line.
    assert_ne!(
        net.near(here, 60.0, AgentKind::Car)[0].index,
        net.near(here, 60.0, AgentKind::Walker)[0].index
    );
}

/// A walker keeps to the pavement, which is *outside* the carriageway — the
/// offset is the whole of what stops a crowd walking down the middle of the
/// road, since the tile does not model pavements at all.
#[test]
fn a_walker_stands_outside_the_carriageway_and_a_car_inside_it() {
    let net = net_with(&[[0.0, 0.0], [100.0, 0.0]]);
    let l = net
        .get(LineKey {
            cell: (0, 0),
            index: 0,
        })
        .expect("one line");
    let car = AgentKind::Car.offset(l);
    let walker = AgentKind::Walker.offset(l);
    assert!(car < l.half_width, "a car drives inside its own kerb");
    assert!(walker > l.half_width, "a walker stands beyond it");
    let cyclist = AgentKind::Cyclist.offset(l);
    assert!(
        cyclist > car && cyclist < l.half_width,
        "a cyclist rides between the car and the kerb: {cyclist} (car {car}, kerb {})",
        l.half_width
    );
}

/// A cyclist rides the cycle track and the street, and nobody else's
/// network: not the footpath, not the motorway. A cycle track draws more
/// riders than a street of its length.
#[test]
fn cyclists_ride_their_own_network() {
    let mut net = DriveNetwork::default();
    let mut motorway = line(&[[0.0, 0.0], [100.0, 0.0]], Flow::Both);
    motorway.uses = Uses {
        drive: true,
        walk: false,
        cycle: false,
    };
    let mut footway = line(&[[0.0, 20.0], [100.0, 20.0]], Flow::Both);
    footway.uses = Uses {
        drive: false,
        walk: true,
        cycle: false,
    };
    let mut track = line(&[[0.0, 40.0], [100.0, 40.0]], Flow::Both);
    track.uses = Uses {
        drive: false,
        walk: true,
        cycle: true,
    };
    let street = line(&[[0.0, 60.0], [100.0, 60.0]], Flow::Both);
    net.insert_cell((0, 0), 0.0, 0.0, vec![motorway, footway, track, street]);
    let mut rides: Vec<usize> = net
        .near(Vec2::new(50.0, 30.0), 80.0, AgentKind::Cyclist)
        .iter()
        .map(|k| k.index)
        .collect();
    rides.sort_unstable();
    assert_eq!(rides, vec![2, 3], "the track and the street");
    let key = |index| LineKey {
        cell: (0, 0),
        index,
    };
    let (track, street) = (net.get(key(2)).unwrap(), net.get(key(3)).unwrap());
    assert!(AgentKind::Cyclist.importance(track) > AgentKind::Cyclist.importance(street));
    assert_eq!(AgentKind::Cyclist.speed(street), CYCLE_SPEED);
    assert!(
        AgentKind::Cyclist.offset(track) < track.half_width,
        "on a track, on it"
    );
    // A fatbike rides all of that and the footpath too — never the
    // motorway — and is quicker than a cyclist everywhere.
    let mut fat: Vec<usize> = net
        .near(Vec2::new(50.0, 30.0), 80.0, AgentKind::Fatbike)
        .iter()
        .map(|k| k.index)
        .collect();
    fat.sort_unstable();
    assert_eq!(fat, vec![1, 2, 3], "the footway, the track and the street");
    let footway = net.get(key(1)).unwrap();
    assert!(AgentKind::Fatbike.speed(street) > CYCLE_SPEED);
    assert!(AgentKind::Fatbike.speed(footway) > CYCLE_SPEED);
    assert!(AgentKind::Fatbike.speed(footway) < AgentKind::Fatbike.speed(street));
}

/// A fatbike rides a street's pavement or its cycle lane; on the pavement
/// it is out among the walkers, beyond the kerb, on whichever side has
/// one. It may ride a one-way street either way.
#[test]
fn a_fatbike_takes_the_pavement_or_the_lane() {
    let mut net = DriveNetwork::default();
    let mut street = line(&[[0.0, 0.0], [100.0, 0.0]], Flow::Forward);
    street.pavements = Pavements {
        right: false,
        left: true,
    };
    net.insert_cell((0, 0), 0.0, 0.0, vec![street]);
    let key = LineKey {
        cell: (0, 0),
        index: 0,
    };
    let l = net.get(key).unwrap();
    let mut bike = walker_on(key, 1.0, 0.0);
    // Riding the line's own way, its right is the line's right: no
    // pavement there, so the pavement it takes is on its left.
    bike.choose_way(AgentKind::Fatbike, l, 1.0, 0.0, 0.9);
    assert!(bike.pavement);
    assert_eq!(bike.side, -1.0);
    let out = bike.offset(AgentKind::Fatbike, l);
    assert!(out < -l.half_width, "beyond the left kerb: {out}");
    // Against the one-way flow, the line's left is its right.
    bike.choose_way(AgentKind::Fatbike, l, -1.0, 0.0, 0.9);
    assert!(bike.pavement);
    assert_eq!(bike.side, 1.0);
    // Most of the rest of the time, the lane.
    bike.choose_way(AgentKind::Fatbike, l, 1.0, 0.9, 0.0);
    assert!(!bike.pavement);
    let lane = bike.offset(AgentKind::Fatbike, l);
    assert!(lane > 0.0 && lane < l.half_width, "in the lane: {lane}");
    // A cyclist is never moved.
    let mut cyclist = walker_on(key, 1.0, 0.0);
    cyclist.choose_way(AgentKind::Cyclist, l, 1.0, 0.0, 0.0);
    assert!(!cyclist.pavement);
    // Off the end of a one-way street's exit, the fatbike may ride it
    // back; a cyclist may not.
    let mut other = DriveNetwork::default();
    other.insert_cell(
        (0, 0),
        0.0,
        0.0,
        vec![
            line(&[[0.0, 0.0], [50.0, 0.0]], Flow::Both),
            line(&[[100.0, 0.0], [50.0, 0.0]], Flow::Forward),
        ],
    );
    let keys = other.near(Vec2::new(50.0, 0.0), 80.0, AgentKind::Fatbike);
    let first = LineKey {
        cell: (0, 0),
        index: 0,
    };
    let at = Vec2::new(50.0, 0.0);
    let east = Vec2::X;
    assert!(hand_off(&other, &keys, first, at, east, AgentKind::Fatbike, 0.5).is_some());
    assert!(hand_off(&other, &keys, first, at, east, AgentKind::Cyclist, 0.5).is_none());
}

/// A rider leans into a turn, the other way to a car body, by the angle the
/// turn's lateral acceleration makes with gravity, and no further than a
/// pedal clears the road.
#[test]
fn a_rider_leans_into_the_turn() {
    let mut lean = 0.0;
    for _ in 0..200 {
        lean = bike_lean(lean, 2.0, 0.02);
    }
    assert!((lean - (2.0f32 / 9.81).atan()).abs() < 1e-3, "{lean}");
    let car = body_tilt(Vec2::ZERO, 0.0, 2.0, 1.0).y;
    assert!(
        lean > 0.0 && car < 0.0,
        "the bike leans in, the car body out"
    );
    let mut hard = 0.0;
    for _ in 0..200 {
        hard = bike_lean(hard, -40.0, 0.02);
    }
    assert!((hard + MAX_BIKE_LEAN).abs() < 1e-3);
}

/// A person walks; a car drives at the road's speed. The pace is not the same
/// number and a walker must not inherit the carriageway's.
#[test]
fn a_walker_keeps_its_own_pace() {
    let net = net_with(&[[0.0, 0.0], [100.0, 0.0]]);
    let l = net
        .get(LineKey {
            cell: (0, 0),
            index: 0,
        })
        .expect("one line");
    assert_eq!(AgentKind::Car.speed(l), l.speed);
    assert_eq!(AgentKind::Walker.speed(l), WALK_SPEED);
    assert!(WALK_SPEED < l.speed, "walking is slower than driving");
}

/// The place and recycle radii must not be the same number, or a car sitting
/// on the boundary is recycled and immediately re-placed on the wrong side of
/// it, every frame.
#[test]
fn the_recycle_radius_is_outside_the_place_radius() {
    assert!(RECYCLE_RADIUS_M > PLACE_RADIUS_M * 1.2);
}

// ── Trajectory ───────────────────────────────────────────────────────────

/// A tight corner has to be taken slower than a gentle one, and a straight
/// road must impose no limit at all — otherwise the whole cornering model is
/// a constant slow-down applied everywhere.
#[test]
fn a_tighter_corner_is_a_slower_one() {
    let straight = corner_radius(Vec2::X, Vec2::X, 5.0);
    let gentle = corner_radius(Vec2::X, Vec2::new(0.95, 0.31).normalize(), 5.0);
    let square = corner_radius(Vec2::X, Vec2::Y, 5.0);
    assert_eq!(straight, f32::MAX, "a straight road bends nowhere");
    assert!(gentle > square, "{gentle} should be wider than {square}");
    assert!(
        corner_speed(square) < corner_speed(gentle),
        "a right angle must be slower than a bend"
    );
    // …and a motorway's speed must survive a motorway's curves: a 300 m
    // radius may not be a reason to brake.
    assert!(corner_speed(300.0) > 26.0);
}

/// The bend a car slows for is the one **in front of it**. A car reads its
/// line in whichever direction it drives it, so the same vertex is a reason
/// to brake for one of them and already behind the other.
#[test]
fn only_the_bend_ahead_is_a_reason_to_slow() {
    // Straight for 100 m, then a right angle.
    let net = net_with(&[[0.0, 0.0], [100.0, 0.0], [100.0, 100.0]]);
    let l = net
        .get(LineKey {
            cell: (0, 0),
            index: 0,
        })
        .expect("one road");
    // 10 m in, driving forwards: the corner is 90 m away and out of sight.
    assert_eq!(l.min_turn_radius(10.0, 30.0, 1.0), f32::MAX);
    // 80 m in: it is 20 m away and must be seen.
    assert!(l.min_turn_radius(80.0, 30.0, 1.0) < 60.0);
    // 80 m in going the other way: it is behind.
    assert_eq!(l.min_turn_radius(80.0, 30.0, -1.0), f32::MAX);
}

/// The junction curve has to begin exactly where the agent is, pointing
/// where it is pointing, and finish exactly on the new road's lane at its
/// heading. That is the whole reason it exists: anything approximate at
/// either end is the pop it was built to remove.
#[test]
fn a_junction_turn_is_exact_at_both_ends() {
    let mut turn = Turn {
        from: Vec2::new(10.0, 0.0),
        from_dir: Vec2::X,
        to: Vec2::new(14.0, 4.0),
        to_dir: Vec2::Y,
        len: 8.0,
        t: 0.0,
        from_y: 0.0,
        to_y: 0.0,
    };
    let (p0, d0) = turn.pose();
    assert!(p0.distance(turn.from) < 1e-3, "{p0} != {}", turn.from);
    assert!(d0.distance(Vec2::X) < 1e-3, "leaves along the old road");
    turn.t = 1.0;
    let (p1, d1) = turn.pose();
    assert!(p1.distance(turn.to) < 1e-3, "{p1} != {}", turn.to);
    assert!(d1.distance(Vec2::Y) < 1e-3, "joins along the new road");
    // And it stays a curve in between: every sample is a unit heading and
    // the path does not double back on itself.
    let mut last = p0;
    for step in 1..=20 {
        turn.t = step as f32 / 20.0;
        let (p, d) = turn.pose();
        assert!((d.length() - 1.0).abs() < 1e-3, "heading not unit at {p}");
        assert!(p.distance(last) > 0.0, "the curve stalled");
        last = p;
    }
}

/// A right-angle turn must be taken slower than a straight-through
/// continuation, or the corner curve is decoration.
#[test]
fn a_sharp_junction_turn_is_taken_slowly() {
    let corner = Turn {
        from: Vec2::ZERO,
        from_dir: Vec2::X,
        to: Vec2::new(6.0, 6.0),
        to_dir: Vec2::Y,
        len: 9.0,
        t: 0.0,
        from_y: 0.0,
        to_y: 0.0,
    };
    let ahead = Turn {
        to: Vec2::new(9.0, 0.0),
        to_dir: Vec2::X,
        ..corner
    };
    assert!(corner.speed_limit() < 8.0, "{}", corner.speed_limit());
    assert!(
        ahead.speed_limit() > corner.speed_limit() * 4.0,
        "driving straight on through a node is not cornering"
    );
}

/// The drawn heading chases the path's and never jumps to it. The rate limit
/// is the whole mechanism, so both halves are asserted: it moves, and it
/// moves by no more than it is allowed to.
#[test]
fn a_heading_turns_at_a_bounded_rate() {
    let mut h = Vec2::X;
    let target = Vec2::new(-1.0, 0.0);
    let mut steps = 0;
    while h.distance(target) > 1e-2 && steps < 1000 {
        let next = rotate_toward(h, target, 0.1);
        let turned = (h.x * next.y - h.y * next.x).atan2(h.dot(next)).abs();
        assert!(turned <= 0.1 + 1e-4, "turned {turned} rad in one step");
        h = next;
        steps += 1;
    }
    assert!(steps > 20, "a half turn at 0.1 rad a step cannot be quick");
    assert!(h.distance(target) < 1e-2, "never got there");
}

// ── Spread ───────────────────────────────────────────────────────────────

/// Placement is a distribution, not an argmax. A long fast road takes the
/// larger share and a short slow one still gets picked — the old
/// three-draws-keep-the-fastest rule failed both halves of that.
#[test]
fn placement_favours_the_bigger_road_without_starving_the_smaller() {
    let mut net = DriveNetwork::default();
    let mut fast = line(&[[0.0, 0.0], [400.0, 0.0]], Flow::Both);
    fast.speed = 22.0;
    let mut alley = line(&[[0.0, 20.0], [40.0, 20.0]], Flow::Both);
    alley.speed = 4.5;
    net.insert_cell((0, 0), 0.0, 0.0, vec![fast, alley]);
    let keys = [
        LineKey {
            cell: (0, 0),
            index: 0,
        },
        LineKey {
            cell: (0, 0),
            index: 1,
        },
    ];
    let cum = weigh(&net, &keys, AgentKind::Car);
    let mut tally = [0usize; 2];
    for i in 0..1000 {
        let r = i as f32 / 1000.0;
        let k = draw_line(&keys, &cum, r).expect("a line");
        tally[k.index] += 1;
    }
    assert!(tally[0] > tally[1] * 4, "{tally:?} — the avenue should win");
    assert!(tally[1] > 20, "{tally:?} — but the alley is not starved");
}

/// An agent is dropped on the part of a road that is in front of the camera.
/// The candidate list holds every line with *any* point in range, so without
/// this a long road clipping the edge of the disc receives agents anywhere
/// along its whole length — most of them outside the recycle radius and
/// thrown away on the next frame.
#[test]
fn a_recycled_agent_lands_near_the_camera() {
    // 2 km of road; the camera sits beside one end of it.
    let pts: Vec<[f32; 2]> = (0..=200).map(|i| [i as f32 * 10.0, 0.0]).collect();
    let net = net_with(&pts);
    let keys = [LineKey {
        cell: (0, 0),
        index: 0,
    }];
    let cum = weigh(&net, &keys, AgentKind::Car);
    let cam = Vec2::new(1500.0, 30.0);
    let occupied = HashMap::new();
    let l = net.get(keys[0]).expect("one road");
    for agent in 0..50u64 {
        let (key, dir, s) = place_agent(
            &net,
            &keys,
            &cum,
            cam,
            AgentKind::Car,
            &occupied,
            agent,
            3,
            &|_, _, _| true,
        )
        .expect("a spot");
        let along = if dir > 0.0 { s } else { l.length() - s };
        let (p, _) = net.get(key).expect("placed line").at(along);
        assert!(
            p.distance(cam) <= AgentKind::Car.place_radius() + 1.0,
            "placed {:.0} m out, outside the disc the pool is sized for",
            p.distance(cam)
        );
    }
}

/// Two agents may not be dropped on the same spot. One car drawn twice a
/// metre apart in the same paint reads as a rendering fault, not as traffic.
#[test]
fn placement_avoids_a_spot_that_is_taken() {
    let pts: Vec<[f32; 2]> = (0..=40).map(|i| [i as f32 * 10.0, 0.0]).collect();
    let net = net_with(&pts);
    let key = LineKey {
        cell: (0, 0),
        index: 0,
    };
    let keys = [key];
    let cum = weigh(&net, &keys, AgentKind::Car);
    let cam = Vec2::new(200.0, 0.0);
    let mut occupied: HashMap<(LineKey, i8), Vec<f32>> = HashMap::new();
    let mut placed: Vec<(i8, f32)> = Vec::new();
    for agent in 0..12u64 {
        let Some((k, dir, s)) = place_agent(
            &net,
            &keys,
            &cum,
            cam,
            AgentKind::Car,
            &occupied,
            agent,
            11,
            &|_, _, _| true,
        ) else {
            continue;
        };
        for (d, o) in &placed {
            if *d == lane_key(dir) {
                assert!(
                    (o - s).abs() > 1.0,
                    "two agents {:.2} m apart on one lane",
                    (o - s).abs()
                );
            }
        }
        placed.push((lane_key(dir), s));
        occupied.entry((k, lane_key(dir))).or_default().push(s);
    }
    assert!(placed.len() > 6, "hardly anything was placed");
}

/// A car follows the one in front by the IDM: it keeps about its time gap
/// behind one at its own speed, brakes for one stopped ahead in time to
/// stand its standing gap short of it, and a leader is not held up by
/// the car behind it, nor by one going the other way.
#[test]
fn a_car_keeps_its_distance() {
    let key = LineKey {
        cell: (0, 0),
        index: 0,
    };
    let idm = Idm::of(AgentKind::Car, false);
    let length = 2.0 * reach_of(AgentKind::Car);
    let mut leaders: HashMap<(LineKey, i8), Vec<(f32, f32)>> = HashMap::new();
    leaders.insert((key, 1), vec![(110.0, 0.0)]);
    // The leader is not held up.
    assert_eq!(
        follow_speed(&leaders, key, 1.0, 110.0, 10.0, 14.0, length, idm, 0.05).0,
        14.0
    );
    // Driving at 10 m/s at a car stopped 60 m ahead: it ends up standing
    // its standing gap behind it, and never inside it.
    let (mut s, mut v) = (50.0f32, 10.0f32);
    leaders.insert((key, 1), vec![(110.0, 0.0), (s, v)]);
    for _ in 0..1200 {
        let (want, _) = follow_speed(&leaders, key, 1.0, s, v, 14.0, length, idm, 0.05);
        v += (want - v).clamp(-CAR_BRAKE * 0.05, CAR_ACCEL * 0.05);
        v = v.max(0.0);
        s += v * 0.05;
        assert!(110.0 - s - length > 0.5, "into it at {s}");
    }
    let gap = 110.0 - s - length;
    assert!(
        (gap - idm.s0).abs() < 0.6 && v < 0.1,
        "stood {gap} m back at {v}"
    );
    // Following one at its own 10 m/s, it settles at about its time gap.
    let (mut s, mut v, mut lead) = (0.0f32, 10.0f32, 60.0f32);
    for _ in 0..2400 {
        leaders.insert((key, 1), vec![(lead, 10.0)]);
        let (want, _) = follow_speed(&leaders, key, 1.0, s, v, 14.0, length, idm, 0.05);
        v += (want.min(14.0) - v).clamp(-CAR_BRAKE * 0.05, CAR_ACCEL * 0.05);
        s += v * 0.05;
        lead += 10.0 * 0.05;
    }
    let gap = lead - s - length;
    assert!(
        (gap - (idm.s0 + 10.0 * idm.t)).abs() < 4.0,
        "follows {gap} m back"
    );
    // One coming the other way on the same line is not in the way.
    leaders.insert((key, -1), vec![(100.5, 10.0)]);
    leaders.remove(&(key, 1));
    assert_eq!(
        follow_speed(&leaders, key, 1.0, 100.0, 10.0, 14.0, length, idm, 0.05).0,
        14.0
    );
}

/// At a junction a car goes **straight on** unless it is choosing to turn.
/// Taking whichever line end is nearest — which is what this used to do —
/// makes the choice on a metre of survey accuracy, so a car running down a
/// main road turned off it whenever a side street happened to be noded a
/// fraction closer than the continuation.
#[test]
fn a_hand_off_prefers_the_road_straight_ahead() {
    let mut net = DriveNetwork::default();
    // Arriving eastbound at (100, 0). The continuation runs on east; the
    // side street turns south and its end is nearer.
    let straight = line(&[[100.4, 0.0], [300.0, 0.0]], Flow::Both);
    let side = line(&[[100.1, 0.0], [100.1, 200.0]], Flow::Both);
    net.insert_cell((0, 0), 0.0, 0.0, vec![straight, side]);
    let keys: Vec<LineKey> = (0..2)
        .map(|index| LineKey {
            cell: (0, 0),
            index,
        })
        .collect();
    let from = LineKey {
        cell: (9, 9),
        index: 0,
    };
    // Over the whole range of the draw: mostly straight on, whichever end
    // is nearer — and sometimes the turn.
    let mut straight = 0;
    for k in 0..100 {
        let (key, dir) = hand_off(
            &net,
            &keys,
            from,
            Vec2::new(100.0, 0.0),
            Vec2::X,
            AgentKind::Car,
            k as f32 / 100.0,
        )
        .expect("a road to go on to");
        if key.index == 0 {
            straight += 1;
            assert_eq!(dir, 1.0);
        }
    }
    assert!(
        (65..=80).contains(&straight),
        "{straight} in 100 straight on"
    );
}

/// A main road keeps most of its traffic past a residential side street.
#[test]
fn a_main_road_keeps_its_traffic() {
    let mut net = DriveNetwork::default();
    let mut main = line(&[[100.0, 0.0], [300.0, 0.0]], Flow::Both);
    main.speed = 13.9;
    let mut side = line(&[[100.0, 0.0], [100.0, 200.0]], Flow::Both);
    side.speed = RESIDENTIAL_SPEED;
    net.insert_cell((0, 0), 0.0, 0.0, vec![main, side]);
    let keys: Vec<LineKey> = (0..2)
        .map(|index| LineKey {
            cell: (0, 0),
            index,
        })
        .collect();
    let from = LineKey {
        cell: (9, 9),
        index: 0,
    };
    let on = (0..100)
        .filter(|k| {
            let r = *k as f32 / 100.0;
            hand_off(
                &net,
                &keys,
                from,
                Vec2::new(100.0, 0.0),
                Vec2::X,
                AgentKind::Car,
                r,
            )
            .is_some_and(|(key, _)| key.index == 0)
        })
        .count();
    assert!((75..=85).contains(&on), "{on} in 100 stay on the main road");
}

/// …but a dead end is still a dead end. Where the only road on offer doubles
/// back, the car takes it rather than vanishing.
#[test]
fn a_hand_off_doubles_back_only_when_it_must() {
    let mut net = DriveNetwork::default();
    net.insert_cell(
        (0, 0),
        0.0,
        0.0,
        vec![line(&[[100.0, 0.0], [100.0, -80.0]], Flow::Both)],
    );
    let keys = [LineKey {
        cell: (0, 0),
        index: 0,
    }];
    let from = LineKey {
        cell: (9, 9),
        index: 0,
    };
    let got = hand_off(
        &net,
        &keys,
        from,
        Vec2::new(100.0, 0.0),
        Vec2::new(0.0, 1.0),
        AgentKind::Car,
        0.5,
    );
    assert!(got.is_some(), "a car at a dead end has to go somewhere");
}

/// The projection has to be onto the segments, not onto the vertices: a
/// simplified line has a vertex every few metres and the placement radius is
/// what the answer is compared against.
#[test]
fn nearest_along_projects_onto_the_line() {
    let net = net_with(&[[0.0, 0.0], [100.0, 0.0], [100.0, 100.0]]);
    let l = net
        .get(LineKey {
            cell: (0, 0),
            index: 0,
        })
        .expect("one road");
    assert!((l.nearest_along(Vec2::new(37.0, 12.0)) - 37.0).abs() < 1e-2);
    assert!((l.nearest_along(Vec2::new(140.0, 60.0)) - 160.0).abs() < 1e-2);
    // Past either end it clamps to the end.
    assert!(l.nearest_along(Vec2::new(-50.0, 0.0)).abs() < 1e-2);
}

/// A time when group 0 shows stop and one when it shows go.
fn red_and_green() -> (f32, f32) {
    use crate::systems::street_detail::signal_says_stop;
    let ts = || (0..480).map(|t| t as f32 * 0.1);
    (
        ts().find(|&t| signal_says_stop(t, 0)).unwrap(),
        ts().find(|&t| !signal_says_stop(t, 0)).unwrap(),
    )
}

/// Heading north (−z), which is phase group 0, with a light 30 m ahead.
const NORTH: Vec2 = Vec2::new(0.0, -1.0);

#[test]
fn a_car_stops_at_red_and_drives_on_green() {
    let (red, green) = red_and_green();
    let light = [(Vec2::new(0.0, -30.0), 0u8, false)];
    let mut grace = 0.0;
    let at_red = signal_speed(&light, red, Vec2::ZERO, NORTH, 8.0, 4.0, &mut grace);
    // A braking curve ending at the stop line, not an instant stop.
    let curve = (2.0 * SIGNAL_BRAKE * (30.0 - STOP_SHORT_M)).sqrt();
    assert!(
        (at_red - curve).abs() < 1e-3,
        "a red light 30 m ahead slows the car: {at_red}"
    );
    let at_green = signal_speed(&light, green, Vec2::ZERO, NORTH, 8.0, 4.0, &mut grace);
    assert_eq!(at_green, f32::MAX);
    // Waiting at the line it stays put until the light changes.
    let line = Vec2::new(0.0, -30.0 + STOP_SHORT_M);
    assert_eq!(
        signal_speed(&light, red, line, NORTH, 0.0, 4.0, &mut grace),
        0.0
    );
    assert_eq!(
        signal_speed(&light, green, line, NORTH, 0.0, 4.0, &mut grace),
        f32::MAX
    );
}

/// A light a pressed button holds stops the car on its green too, and
/// lets it go once the press is over.
#[test]
fn a_pressed_button_holds_a_car_at_its_green() {
    let (_, green) = red_and_green();
    let held = [(Vec2::new(0.0, -30.0), 0u8, true)];
    let mut grace = 0.0;
    let line = Vec2::new(0.0, -30.0 + STOP_SHORT_M);
    assert_eq!(
        signal_speed(&held, green, line, NORTH, 0.0, 4.0, &mut grace),
        0.0
    );
    let released = [(Vec2::new(0.0, -30.0), 0u8, false)];
    assert_eq!(
        signal_speed(&released, green, line, NORTH, 0.0, 4.0, &mut grace),
        f32::MAX
    );
}

/// The cross street's lights stand at the same junction, beside this road
/// and in the other phase group; neither may stop a car.
#[test]
fn a_cross_streets_light_does_not_stop_a_car() {
    let (red, _) = red_and_green();
    let mut grace = 0.0;
    let beside = [(Vec2::new(10.0, -30.0), 0u8, false)];
    assert_eq!(
        signal_speed(&beside, red, Vec2::ZERO, NORTH, 8.0, 4.0, &mut grace),
        f32::MAX
    );
    let other_group = [(Vec2::new(0.0, -30.0), 1u8, false)];
    assert_eq!(
        signal_speed(&other_group, red, Vec2::ZERO, NORTH, 8.0, 4.0, &mut grace),
        f32::MAX
    );
}

/// Too close to stop and it goes, as a driver caught by the amber does —
/// a stop forced anyway would leave it in the junction.
#[test]
fn a_car_too_close_to_stop_drives_through() {
    let (red, _) = red_and_green();
    let light = [(Vec2::new(0.0, -10.0), 0u8, false)];
    let mut grace = 0.0;
    assert_eq!(
        signal_speed(&light, red, Vec2::ZERO, NORTH, 14.0, 4.0, &mut grace),
        f32::MAX
    );
}

/// Having crossed one stop line, the opposite approach's line beyond the
/// junction — same road, same phase group — is not this car's.
#[test]
fn a_car_through_a_junction_ignores_the_far_stop_line() {
    let (red, _) = red_and_green();
    let near_and_far = [
        (Vec2::new(0.0, 1.0), 0u8, false),
        (Vec2::new(0.0, -15.0), 0u8, false),
    ];
    let mut grace = 0.0;
    let v = signal_speed(&near_and_far, red, Vec2::ZERO, NORTH, 6.0, 4.0, &mut grace);
    assert_eq!(v, f32::MAX);
    assert_eq!(grace, SIGNAL_GRACE_M);
}

#[test]
fn the_body_dips_braking_and_leans_out_of_a_turn() {
    let mut t = Vec2::ZERO;
    for _ in 0..60 {
        t = body_tilt(t, -5.0, 0.0, 1.0 / 60.0);
    }
    assert!(t.x > 0.02, "the nose dips under braking: {t:?}");
    let mut t = Vec2::ZERO;
    for _ in 0..60 {
        // Turning right at speed: the outside of the turn is the car's left,
        // which is the model's +x — a negative roll about z.
        t = body_tilt(t, 0.0, 2.5, 1.0 / 60.0);
    }
    assert!(t.y < -0.02, "the roof leans out of the turn: {t:?}");
    assert!(
        body_tilt(Vec2::ZERO, -100.0, 100.0, 10.0)
            .abs()
            .max_element()
            <= MAX_TILT + 1e-6
    );
}

#[test]
fn brake_lamps_hold_at_a_stop_and_do_not_flicker() {
    assert!(brake_lamps(false, 0.0, true), "lit while waiting");
    assert!(brake_lamps(false, -2.0, false), "lit braking");
    assert!(!brake_lamps(false, -0.5, false), "not lit easing off");
    assert!(
        brake_lamps(true, -0.5, false),
        "stays lit easing off a stop"
    );
    assert!(!brake_lamps(true, 1.0, false), "off pulling away");
}

/// A pavement is two-way, and people going each way use both of them.
#[test]
fn walkers_keep_to_either_pavement() {
    let net = net_with(&[[0.0, 0.0], [100.0, 0.0]]);
    let key = LineKey {
        cell: (0, 0),
        index: 0,
    };
    let l = net.get(key);
    let walker = |side: f32| Car {
        line: Some(key),
        s: 50.0,
        dir: 1.0,
        speed_scale: 1.0,
        speed: 1.0,
        heading: Vec2::ZERO,
        turn: None,
        side,
        lateral: 0.0,
        phase: 0.0,
        tilt: Vec2::ZERO,
        roll: 0.0,
        signal_grace: 0.0,
        braking: false,
        parts: AgentParts::Walker,
        react: Default::default(),
        pinned: None,
        pavement: false,
        defiant: false,
        crossing: None,
        last_cross: None,
        foot_down: 0.0,
        ordinal: 0,
        size: None,
        body: None,
        crash: None,
        waited: 0.0,
        driven: 0.0,
        drawn: Vec2::ZERO,
        drawn_step: Vec2::ZERO,
        why: ("free", f32::NAN),
        stuck: 0.0,
    };
    let (right, ..) = pose_of(&walker(1.0), l, AgentKind::Walker).unwrap();
    let (left, ..) = pose_of(&walker(-1.0), l, AgentKind::Walker).unwrap();
    assert!(right.y > 3.0 && left.y < -3.0, "{right} {left}");
    assert!((right.y + left.y).abs() < 1e-3, "the two pavements mirror");
}

fn walker_on(key: LineKey, side: f32, lateral: f32) -> Car {
    Car {
        line: Some(key),
        s: 50.0,
        dir: 1.0,
        speed_scale: 1.0,
        speed: 1.0,
        heading: Vec2::ZERO,
        turn: None,
        side,
        lateral,
        phase: 0.0,
        tilt: Vec2::ZERO,
        roll: 0.0,
        signal_grace: 0.0,
        braking: false,
        parts: AgentParts::Walker,
        react: Default::default(),
        pinned: None,
        pavement: false,
        defiant: false,
        crossing: None,
        last_cross: None,
        foot_down: 0.0,
        ordinal: 0,
        size: None,
        body: None,
        crash: None,
        waited: 0.0,
        driven: 0.0,
        drawn: Vec2::ZERO,
        drawn_step: Vec2::ZERO,
        why: ("free", f32::NAN),
        stuck: 0.0,
    }
}

/// A street with a pavement on its right only: a walker meant for the left
/// crosses to the right, in either direction of travel.
#[test]
fn a_walker_crosses_to_the_side_with_a_pavement() {
    let mut one_sided = line(&[[0.0, 0.0], [100.0, 0.0]], Flow::Both);
    one_sided.pavements = Pavements {
        right: true,
        left: false,
    };
    let mut net = DriveNetwork::default();
    net.insert_cell((0, 0), 0.0, 0.0, vec![one_sided]);
    let key = LineKey {
        cell: (0, 0),
        index: 0,
    };
    let l = net.get(key).unwrap();
    for dir in [1.0f32, -1.0] {
        for side in [1.0f32, -1.0] {
            let mut w = walker_on(key, side, 0.0);
            w.keep_to_pavement(AgentKind::Walker, l, dir);
            assert_eq!(w.side * dir, 1.0, "dir {dir} side {side}");
        }
    }
}

/// On a path a walker is on the path, not on the grass beside it.
#[test]
fn a_walker_walks_on_a_path() {
    let mut path = line(&[[0.0, 0.0], [100.0, 0.0]], Flow::Both);
    path.uses.drive = false;
    path.half_width = 1.2;
    let mut net = DriveNetwork::default();
    net.insert_cell((0, 0), 0.0, 0.0, vec![path]);
    let key = LineKey {
        cell: (0, 0),
        index: 0,
    };
    let l = net.get(key);
    for side in [1.0, -1.0] {
        for lateral in [-WALK_SPREAD_M, 0.0, WALK_SPREAD_M] {
            let (p, ..) = pose_of(&walker_on(key, side, lateral), l, AgentKind::Walker).unwrap();
            assert!(p.y.abs() <= 1.2, "off the path at {p}");
        }
    }
}
