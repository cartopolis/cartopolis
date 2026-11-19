use super::*;
use crate::vector_tiles::synth;

// The same real z14 Groningen tile the renderer tests use, and its tile
// coordinates.
use crate::testdata::{GRONINGEN_TILE as TILE, GRONINGEN_Z14};

fn graph() -> RoadGraph {
    let mut g = RoadGraph::new(53.2194);
    g.add_tile(&TileData::parse(GRONINGEN_Z14).expect("parse"), TILE)
        .expect("add tile");
    g.finish();
    g
}

/// The centre of the fixture tile, and a point near its north-west quarter —
/// both well inside Groningen's street network.
fn tile_point(fx: f64, fy: f64) -> (f64, f64) {
    let n = 2.0_f64.powi(ROUTING_ZOOM as i32);
    let x = TILE.0 as f64 + fx;
    let y = TILE.1 as f64 + fy;
    let lng = x / n * 360.0 - 180.0;
    let lat = ((1.0 - 2.0 * y / n) * std::f64::consts::PI)
        .sinh()
        .atan()
        .to_degrees();
    (lat, lng)
}

/// Add one segment straight to the raw list, bypassing the tile decode — the
/// way to build a graph whose shape is the thing under test.
fn seg(g: &mut RoadGraph, a: (f64, f64), b: (f64, f64), kind: RoadKind) {
    seg_at(g, a, b, kind, 0);
}

/// As [`seg`], at an explicit grade.
fn seg_at(g: &mut RoadGraph, a: (f64, f64), b: (f64, f64), kind: RoadKind, level: i8) {
    line_at(g, &[a, b], kind, level);
}

/// Add a whole polyline the way a tile's line arrives: its two ends are dangles
/// and every vertex between them is interior.
///
/// That distinction is what `finish` routes on, so a test about crossings has to
/// be built out of lines and not out of segments — a chain of [`seg_at`] calls
/// marks every joint a dangle and quietly tests the opposite thing.
fn line_at(g: &mut RoadGraph, pts: &[(f64, f64)], kind: RoadKind, level: i8) {
    let ids: Vec<u32> = pts.iter().map(|&p| g.node(p, level)).collect();
    for w in ids.windows(2) {
        g.raw.push(RawSeg {
            a: w[0],
            b: w[1],
            kind,
            fwd_blocked: false,
            rev_blocked: false,
            bike_denied: false,
            unpaved: false,
            level,
        });
    }
    g.node_terminal[ids[0] as usize] = true;
    g.node_terminal[*ids.last().expect("a line has at least one vertex") as usize] = true;
}

/// A viaduct and the street beneath it are not a junction, however exactly the
/// noding pass finds one lying on the other.
///
/// The geometry is the one that bites: the bridge carries a *vertex* at the
/// crossing (its own generalisation put one there) while the ground road runs
/// straight through, so the sweep sees a node sitting on a segment and — before
/// the grade guard — spliced them into a slip road nobody can drive.
#[test]
fn a_bridge_does_not_join_the_street_it_crosses() {
    let base = (TILE.0 as f64 * EXTENT, TILE.1 as f64 * EXTENT);
    let at = |x: f64, y: f64| (base.0 + x, base.1 + y);

    let mut g = RoadGraph::new(53.2194);
    // The street, straight through, with no vertex at the crossing.
    seg(&mut g, at(0.0, 0.0), at(800.0, 0.0), RoadKind::Residential);
    // The viaduct over it, hinged exactly on the street's centre line.
    seg_at(
        &mut g,
        at(400.0, -300.0),
        at(400.0, 0.0),
        RoadKind::Primary,
        1,
    );
    seg_at(
        &mut g,
        at(400.0, 0.0),
        at(400.0, 300.0),
        RoadKind::Primary,
        1,
    );
    g.finish();

    assert_eq!(g.cross_level_refused, 1, "the guard did not fire");
    let street = g.index[&(base.0 as i32, base.1 as i32)];
    let on_the_bridge = g.index[&((base.0 + 400.0) as i32, (base.1 + 300.0) as i32)];
    assert!(
        !g.reachable(street, TravelMode::Car)
            .contains(&on_the_bridge),
        "a car drove off the street and onto the viaduct crossing over it"
    );
}

/// …and it still meets the ground at its abutments.
///
/// Same shape, one difference that is the whole distinction available in this
/// data: here the *ground road* carries the vertex too, because the two really
/// do share an OSM node. That shared vertex is a shared graph node, so the
/// junction exists without the noding pass being asked — which is why
/// [`RoadGraph::node`] keys on the coordinate alone. Key the level in and every
/// bridge in the network is severed from both its ends; this test is what
/// notices.
#[test]
fn a_bridge_meets_the_ground_at_its_abutments() {
    let base = (TILE.0 as f64 * EXTENT, TILE.1 as f64 * EXTENT);
    let at = |x: f64, y: f64| (base.0 + x, base.1 + y);

    let mut g = RoadGraph::new(53.2194);
    seg(&mut g, at(0.0, 0.0), at(400.0, 0.0), RoadKind::Residential);
    seg(
        &mut g,
        at(400.0, 0.0),
        at(800.0, 0.0),
        RoadKind::Residential,
    );
    seg_at(
        &mut g,
        at(400.0, 0.0),
        at(400.0, 300.0),
        RoadKind::Primary,
        1,
    );
    g.finish();

    let street = g.index[&(base.0 as i32, base.1 as i32)];
    let on_the_bridge = g.index[&((base.0 + 400.0) as i32, (base.1 + 300.0) as i32)];
    assert!(
        g.reachable(street, TravelMode::Car)
            .contains(&on_the_bridge),
        "the ramp is unreachable from the road it ramps off"
    );
}

/// Both guards fire on real tiles, and the grade one fires on levels that came
/// out of the attributes rather than out of a default.
///
/// The second assertion is the one that fails if Shortbread renames `bridge`, or
/// if a decode change drops the flag: `level_of` would answer 0 everywhere,
/// every grade refusal would disappear, and that guard would sit inert while
/// looking installed. Measured 2026-08-20 on the Groningen z14 fixture: 59
/// mid-span and 2 cross-level junctions refused, 6348 → 6226 undirected edges,
/// car component share 0.9199 → 0.9195. The grade count is low because
/// Groningen's bridges mostly cross *water*, where there is no road to be fused
/// with; `docs/notes/grade-separation.md` has the full A/B.
#[test]
fn both_noding_guards_bite_on_real_tiles() {
    let g = graph();
    assert!(
        g.mid_span_refused > 0,
        "no crossing was refused over a whole city tile — the dangle guard is \
             not seeing which vertices end a line"
    );
    assert!(
        g.cross_level_refused > 0,
        "no cross-level junction was refused over a whole city tile — the \
             bridge/tunnel attributes are not reaching the graph"
    );
}

/// `layer` decides where the tile carries one, the flags decide where it does
/// not — and Shortbread is the second case, so the flags are what does the work
/// today (its `streets` layer ships `bridge` and `tunnel` and no `layer`,
/// checked 2026-08-20 on the fixture). The consequence is worth knowing: every
/// bridge lands on level 1, which separates a viaduct from the ground beneath it
/// but says nothing about a second viaduct crossing it at an interchange — those
/// are told apart by the dangle rule instead
/// (`two_viaducts_crossing_are_not_a_junction`).
#[test]
fn a_level_comes_from_layer_first_and_the_flags_after() {
    let props = |pairs: &[(&str, Value)]| -> HashMap<String, Value> {
        pairs
            .iter()
            .map(|(k, v)| ((*k).to_string(), v.clone()))
            .collect()
    };

    let flat = props(&[]);
    assert_eq!(level_of(Some(&flat)), 0);

    let bridge = props(&[("bridge", Value::Bool(true))]);
    assert_eq!(level_of(Some(&bridge)), 1);

    let tunnel = props(&[("tunnel", Value::Bool(true))]);
    assert_eq!(level_of(Some(&tunnel)), -1);

    // An explicit `layer` outranks the flag, so a stack of decks separates.
    let upper = props(&[("bridge", Value::Bool(true)), ("layer", Value::SInt(2))]);
    assert_eq!(level_of(Some(&upper)), 2);

    // …in whichever numeric shape the encoder picked. Reading only one of them
    // is how `housenumber` lost most of its values.
    for v in [
        Value::Int(-2),
        Value::SInt(-2),
        Value::Float(-2.0),
        Value::Double(-2.0),
        Value::String("-2".into()),
    ] {
        let sunk = props(&[("layer", v.clone())]);
        assert_eq!(level_of(Some(&sunk)), -2, "layer as {v:?}");
    }

    // A `layer` past the mask's window still separates from the ground rather
    // than wrapping onto some other level's bit.
    let absurd = props(&[("layer", Value::SInt(99))]);
    assert_eq!(level_of(Some(&absurd)), MAX_LEVEL);
    assert_ne!(level_bit(MAX_LEVEL), level_bit(0));
}

/// A route's grades line up with its geometry, and the abutments read as ground.
///
/// This is what stops the drawn line climbing the wrong viaduct. `points` and
/// `levels` are parallel arrays and every splice has to keep them so; a route
/// that crosses *under* a bridge has the same ground coordinates as one crossing
/// over it, so if these two ever drift apart nothing downstream can notice.
#[test]
fn a_route_carries_the_grade_of_the_road_it_is_on() {
    let base = (TILE.0 as f64 * EXTENT, TILE.1 as f64 * EXTENT);
    let at = |x: f64, y: f64| (base.0 + x, base.1 + y);

    let mut g = RoadGraph::new(53.2194);
    // Ground, then a bridge span, then ground again — one continuous road, with
    // its abutments at x = 400 and x = 800 shared between the runs.
    line_at(
        &mut g,
        &[at(0.0, 0.0), at(400.0, 0.0)],
        RoadKind::Residential,
        0,
    );
    line_at(
        &mut g,
        &[at(400.0, 0.0), at(600.0, 0.0), at(800.0, 0.0)],
        RoadKind::Residential,
        1,
    );
    line_at(
        &mut g,
        &[at(800.0, 0.0), at(1200.0, 0.0)],
        RoadKind::Residential,
        0,
    );
    g.finish();

    let ends = |p: (f64, f64)| {
        let c = g.index[&(p.0 as i32, p.1 as i32)] as usize;
        g.lattice_to_lat_lng(g.coords[c].0, g.coords[c].1)
    };
    let route = g
        .route(ends(at(0.0, 0.0)), ends(at(1200.0, 0.0)), TravelMode::Car)
        .expect("one road end to end");

    assert_eq!(
        route.levels.len(),
        route.points.len(),
        "the grades must stay parallel to the geometry"
    );
    // The mid-span vertex is the only one wholly on the structure; both
    // abutments are shared with a ground run and count as ground, which is where
    // the deck's own fascia puts the step too.
    assert!(
        route.levels.contains(&1),
        "the span was routed over but no point reports being on it: {:?}",
        route.levels
    );
    assert_eq!(route.levels[0], 0, "the start pin is on the ground");
    assert_eq!(
        *route.levels.last().expect("levels"),
        0,
        "and so is the finish pin"
    );
}

/// Two viaducts crossing each other at an interchange are not a junction — and
/// this is the case no attribute in the tile can settle, because both of them
/// are simply `bridge`. Only the dangle rule separates them.
///
/// This was written off as unfixable when the grade guard went in ("nothing in
/// this repo can recover it"). It was fixable; the recoverable fact was never
/// the height, it was which line *ends* at the contact.
#[test]
fn two_viaducts_crossing_are_not_a_junction() {
    let base = (TILE.0 as f64 * EXTENT, TILE.1 as f64 * EXTENT);
    let at = |x: f64, y: f64| (base.0 + x, base.1 + y);

    let mut g = RoadGraph::new(53.2194);
    // Both decks. Neither carries a vertex the other could be tested against —
    // except that the upper one is hinged over the lower, exactly as a
    // generalised line is.
    line_at(
        &mut g,
        &[at(0.0, 0.0), at(800.0, 0.0)],
        RoadKind::Primary,
        1,
    );
    line_at(
        &mut g,
        &[at(400.0, -300.0), at(400.0, 0.0), at(400.0, 300.0)],
        RoadKind::Primary,
        1,
    );
    g.finish();

    assert_eq!(g.mid_span_refused, 1, "the dangle guard did not fire");
    assert_eq!(
        g.cross_level_refused, 0,
        "both decks are level 1 — the grade guard cannot see this one"
    );
    let lower = g.index[&(base.0 as i32, base.1 as i32)];
    let upper = g.index[&((base.0 + 400.0) as i32, (base.1 + 300.0) as i32)];
    assert!(
        !g.reachable(lower, TravelMode::Car).contains(&upper),
        "a car changed decks in mid-air"
    );
}

/// The tee the noding pass exists for still joins, which is the half that a
/// guard is always one step from breaking: refuse a little too much and the
/// network shatters again, silently, into `NoPath` for ordinary routes.
#[test]
fn a_side_street_that_stops_dead_on_a_road_still_joins_it() {
    let base = (TILE.0 as f64 * EXTENT, TILE.1 as f64 * EXTENT);
    let at = |x: f64, y: f64| (base.0 + x, base.1 + y);

    let mut g = RoadGraph::new(53.2194);
    // The through road, generalised past the tee — it has no vertex there.
    line_at(
        &mut g,
        &[at(0.0, 0.0), at(800.0, 0.0)],
        RoadKind::Residential,
        0,
    );
    // The side street, ending on it.
    line_at(
        &mut g,
        &[at(400.0, 0.0), at(400.0, 300.0)],
        RoadKind::Residential,
        0,
    );
    g.finish();

    assert_eq!(g.mid_span_refused + g.cross_level_refused, 0);
    let road = g.index[&(base.0 as i32, base.1 as i32)];
    let up_the_side_street = g.index[&((base.0 + 400.0) as i32, (base.1 + 300.0) as i32)];
    assert!(
        g.reachable(road, TravelMode::Car)
            .contains(&up_the_side_street),
        "the side street is unreachable from the road it tees into"
    );
}

/// A/B of the two noding guards on the real fixture: the same tile noded with
/// each combination of them, against the flat pass they replaced. Diagnostic,
/// not a gate — the numbers it prints are quoted in
/// `docs/notes/grade-separation.md`.
#[test]
#[ignore = "printing hatch: it reports numbers, it does not assert them"]
fn grade_separation_diagnostic() {
    // Each variant is expressed by *relaxing* the finished graph's inputs
    // before noding, so there is no knob in production code: dropping every
    // level to the ground disables the grade guard, and marking every node a
    // dangle disables the mid-span one.
    let build = |levels: bool, dangles: bool| {
        let mut g = RoadGraph::new(53.2194);
        g.add_tile(&TileData::parse(GRONINGEN_Z14).expect("parse"), TILE)
            .expect("add tile");
        if !levels {
            for s in &mut g.raw {
                s.level = 0;
            }
            for l in &mut g.node_levels {
                *l = level_bit(0);
            }
        }
        if !dangles {
            for t in &mut g.node_terminal {
                *t = true;
            }
        }
        g.finish();
        g
    };

    println!(
        "{:<14} {:>7} {:>9} {:>10} {:>8} {:>8} {:>8}",
        "variant", "edges", "mid-span", "cross-lvl", "car", "bike", "foot"
    );
    for (name, levels, dangles) in [
        ("flat", false, false),
        ("level only", true, false),
        ("dangle only", false, true),
        ("both", true, true),
    ] {
        let g = build(levels, dangles);
        let share = |mode| {
            let (l, u) = g.largest_component(mode);
            l as f64 / u as f64
        };
        println!(
            "{name:<14} {:>7} {:>9} {:>10} {:>8.4} {:>8.4} {:>8.4}",
            g.edge_count(),
            g.mid_span_refused,
            g.cross_level_refused,
            share(TravelMode::Car),
            share(TravelMode::Bike),
            share(TravelMode::Foot),
        );
    }
}

/// A vertex no tile can legitimately carry must never reach the graph.
///
/// Tile bytes come from a server we do not run, and `mvt-reader` clips its
/// coordinate cursor at `i32::MAX` rather than at the tile's extent, so a few
/// bytes of crafted varint deliver ~2.1e9. What follows is not a wrong route:
/// `finish`'s noding sweep and the label index are both loops over a
/// segment's bucket bounding box, and at that scale each is ~10^14 iterations
/// with a `HashMap` probe or a fresh `Vec` in every one.
#[test]
fn a_street_running_off_to_two_billion_never_enters_the_graph() {
    let hostile = synth::tile(&[synth::line_layer(
        "streets",
        4096,
        &[(10, 10), (2_000_000_000, 20), (30, 30)],
    )]);
    let mut g = RoadGraph::new(53.2194);
    g.add_tile(&TileData::parse(&hostile).expect("parse"), TILE)
        .expect("add tile");
    g.finish();
    assert!(
        g.is_empty(),
        "{} nodes interned from a street that leaves the planet",
        g.node_count()
    );

    // …and an ordinary street from the same encoder does enter it, so what
    // was tested above is a window and not a wall.
    let ordinary = synth::tile(&[synth::line_layer("streets", 4096, &[(10, 10), (2000, 20)])]);
    let mut g = RoadGraph::new(53.2194);
    g.add_tile(&TileData::parse(&ordinary).expect("parse"), TILE)
        .expect("add tile");
    g.finish();
    assert_eq!(g.node_count(), 2, "an ordinary street was refused");
    assert_eq!(g.edge_count(), 2, "both traversals of one segment");
}

/// The backstop under that window: a bucket sweep must be bounded whatever it
/// is handed, because `bucket`'s `as i32` saturates rather than wrapping.
#[test]
fn the_bucket_span_refuses_what_no_tile_could_span() {
    // A whole tile is 16 buckets, comfortably inside the ceiling.
    assert_eq!(bucket_span(0.0, EXTENT), Some((0, 16)));
    assert!(bucket_span(0.0, 2.1e9).is_none());
    assert!(bucket_span(-2.1e9, 2.1e9).is_none());
}

/// The junction cost has to change *route choice*, not just the clock.
///
/// Two ways between the same pair, both residential so the class speed cannot
/// decide it: a straight run crossed by a side street every 50 m, and a detour
/// 19% longer with nothing on it. On length alone the straight one wins, which
/// is the rat-run the old cost model had no way to avoid. Charging for the
/// junctions flips it — and the test asserts *both* halves, because a penalty
/// that changes nothing and a penalty that changes everything look the same
/// from one route.
#[test]
fn a_junction_free_detour_beats_a_grid_of_side_streets() {
    // Placed on the fixture tile's patch of lattice so the latitude the graph
    // was scaled for is the latitude the geometry is at.
    let base = (TILE.0 as f64 * EXTENT, TILE.1 as f64 * EXTENT);
    let at = |x: f64, y: f64| (base.0 + x, base.1 + y);
    const SPAN: f64 = 2800.0; // ~1 km at z14
    const STEPS: usize = 20;

    let mut g = RoadGraph::new(53.2194);
    // The straight run, with a stub off every interior vertex.
    for i in 0..STEPS {
        let (x0, x1) = (
            SPAN * i as f64 / STEPS as f64,
            SPAN * (i + 1) as f64 / STEPS as f64,
        );
        seg(&mut g, at(x0, 0.0), at(x1, 0.0), RoadKind::Residential);
        if i > 0 {
            seg(&mut g, at(x0, 0.0), at(x0, 300.0), RoadKind::Residential);
        }
    }
    // The clear detour: two long legs, one interior vertex, no side streets.
    seg(
        &mut g,
        at(0.0, 0.0),
        at(SPAN / 2.0, -900.0),
        RoadKind::Residential,
    );
    seg(
        &mut g,
        at(SPAN / 2.0, -900.0),
        at(SPAN, 0.0),
        RoadKind::Residential,
    );
    g.finish();

    let a = g.lattice_to_lat_lng(base.0, base.1);
    let b = g.lattice_to_lat_lng(base.0 + SPAN, base.1);
    let route = g.route(a, b, TravelMode::Car).expect("a car route");

    // The detour is measurably longer, so taking it can only be the junctions.
    let straight_m = SPAN * g.m_per_unit;
    assert!(
        route.distance_m > straight_m * 1.10,
        "took the grid: {:.0} m against a {:.0} m straight run",
        route.distance_m,
        straight_m
    );

    // …and the same graph with the penalty removed takes the straight run.
    // Asserted through `junction_delay` rather than by rebuilding the graph, so
    // the two answers differ in exactly one thing.
    assert!(
        junction_delay(RoadKind::Residential, TravelMode::Car) > 0.0,
        "the premise of this test is that a residential junction costs something"
    );
    let grid_junctions = (STEPS - 1) as f64;
    let detour_extra_m = route.distance_m - straight_m;
    let detour_extra_s =
        detour_extra_m / (RoadKind::Residential.speed_kmh(TravelMode::Car).unwrap() / 3.6);
    assert!(
        grid_junctions * junction_delay(RoadKind::Residential, TravelMode::Car) > detour_extra_s,
        "the junctions ({grid_junctions} of them) must outweigh the {detour_extra_s:.0} s detour \
             or this test proves nothing"
    );
}

/// The junction delay's whole job is the *ranking*: priority roads pay less
/// than the streets that give way to them, motorways pay nothing at all, and a
/// pedestrian is barely delayed by a side road. Get the order wrong and the
/// router pushes walkers onto arterials.
#[test]
fn junction_delay_follows_priority_and_mode() {
    use RoadKind::{Motorway, Primary, Residential, Secondary, Tertiary, Trunk};
    let car = |k| junction_delay(k, TravelMode::Car);
    assert_eq!(
        car(Motorway),
        0.0,
        "a motorway has no junctions to give way at"
    );
    assert_eq!(car(Trunk), 0.0);
    assert!(car(Primary) < car(Secondary));
    assert!(car(Secondary) < car(Tertiary));
    assert!(car(Tertiary) < car(Residential));
    for kind in [Primary, Tertiary, Residential] {
        assert!(
            junction_delay(kind, TravelMode::Foot) < junction_delay(kind, TravelMode::Bike),
            "{kind:?}: a walker waits less than a cyclist"
        );
        assert!(
            junction_delay(kind, TravelMode::Bike) < car(kind),
            "{kind:?}: a cyclist waits less than a driver"
        );
    }
}

/// Ground kilometres between two `[lat, lng]`, for the reach tests below.
fn km_between(a: (f64, f64), b: (f64, f64)) -> f64 {
    ground_m([a.0, a.1], [b.0, b.1]) / 1000.0
}

/// A point `km` due east of another, near enough for a tangent-plane step.
fn east_of(from: (f64, f64), km: f64) -> (f64, f64) {
    (
        from.0,
        from.1 + km * 1000.0 / (111_320.0 * from.0.to_radians().cos()),
    )
}

/// The unit is picked from the *rounded* number, so nothing prints "1000 m".
#[test]
fn a_distance_never_rounds_past_its_own_unit() {
    assert_eq!(format_distance(450.0), "450 m");
    assert_eq!(format_distance(994.0), "990 m");
    // The regression: these round to 1000, which is not a metres value.
    assert_eq!(format_distance(995.0), "1.0 km");
    assert_eq!(format_distance(999.0), "1.0 km");
    assert_eq!(format_distance(1_000.0), "1.0 km");
    assert_eq!(format_distance(4_300.0), "4.3 km");
    assert_eq!(format_distance(42_000.0), "42 km");
}

/// A hopeless corridor is refused from the endpoints alone. The old code
/// walked the endpoints' whole bounding box first — ~3·10⁷ iterations for an
/// intercontinental pair — to reach the same answer.
///
/// The time budget is an order-of-magnitude guard, not a benchmark: the
/// rejection is now a handful of arithmetic ops, while the loop it replaces
/// measured in hundreds of milliseconds.
#[test]
fn an_impossible_corridor_is_refused_without_walking_the_bounding_box() {
    let amsterdam = (52.3676, 4.9041);
    let sydney = (-33.8688, 151.2093);

    let started = std::time::Instant::now();
    let refused = corridor_tiles_at(ROUTING_ZOOM, amsterdam, sydney, MAX_ROUTE_TILES);
    let elapsed = started.elapsed();

    assert!(
        matches!(refused, Err(RouteError::TooFar { .. })),
        "an intercontinental corridor must be refused, got {refused:?}"
    );
    assert!(
        elapsed < std::time::Duration::from_millis(200),
        "the refusal walked the bounding box: took {elapsed:?}"
    );

    // …and the early test must not refuse anything the loop would have taken:
    // a corridor that fits is still built. (`the_planner_steps_down_a_tier…`
    // sweeps this boundary properly; this is the cheap direct check.)
    let here = (53.2194, 6.5665);
    assert!(
        corridor_tiles_at(ROUTING_ZOOM, here, east_of(here, 8.0), MAX_ROUTE_TILES).is_ok(),
        "a city-scale corridor was refused by the early bound"
    );
}

/// Street level while it fits, two tiers once it does not, and a refusal only
/// past the coarse budget. The thresholds are the product, not the mechanism:
/// a change that quietly moved either would change which routes exist.
#[test]
fn the_planner_steps_down_a_tier_rather_than_refusing() {
    let here = (53.2194, 6.5665);

    // Well inside street-level reach.
    assert!(
        matches!(
            plan_corridor(here, east_of(here, 8.0)),
            Ok(Corridor::Direct(_))
        ),
        "a city route must stay on one street-level graph"
    );

    // Past it — the old hard stop, now a staged route.
    let long = east_of(here, 60.0);
    let Ok(Corridor::Staged { start, end, coarse }) = plan_corridor(here, long) else {
        panic!("60 km should stage, not refuse");
    };
    assert!(!start.is_empty() && !end.is_empty() && !coarse.is_empty());
    assert!(
        coarse.len() <= MAX_ROUTE_TILES,
        "the coarse corridor is the budget now: {} tiles",
        coarse.len()
    );
    // The patches are separate lists *and* must not have been merged: at this
    // range they cannot overlap, and sharing one graph is the bug that made
    // every staged route fall back (see `Corridor::Staged`).
    assert!(
        start.iter().all(|t| !end.contains(t)),
        "the endpoint patches ran into each other"
    );

    // The transition is where `MAX_ROUTE_KM` says it is, give or take the
    // corridor being a count of grid cells rather than a smooth area.
    let direct_reach = (1..)
        .map(|i| f64::from(i) * 2.0)
        .take_while(|&km| {
            matches!(
                plan_corridor(here, east_of(here, km)),
                Ok(Corridor::Direct(_))
            )
        })
        .last()
        .expect("some distance routes directly");
    assert!(
        (direct_reach - MAX_ROUTE_KM).abs() < 2.0,
        "street-level reach measured {direct_reach:.0} km against MAX_ROUTE_KM {MAX_ROUTE_KM:.0}"
    );

    // …and the staged reach is what `MAX_STAGED_KM` claims, which is the
    // number the refusal message quotes at the user.
    let staged_reach = (1..)
        .map(|i| f64::from(i) * 10.0)
        .take_while(|&km| plan_corridor(here, east_of(here, km)).is_ok())
        .last()
        .expect("some distance stages");
    // Both sampled on their own step, so the true limits sit inside one step
    // above these; the constants name the guaranteed reach, not the ceiling.
    assert!(
        (staged_reach - MAX_STAGED_KM).abs() < 10.0,
        "staged reach measured {staged_reach:.0} km against MAX_STAGED_KM {MAX_STAGED_KM:.0}"
    );

    // Past everything, the refusal reports a distance the user can act on.
    let Err(RouteError::TooFar { km }) = plan_corridor(here, east_of(here, 900.0)) else {
        panic!("900 km must be refused");
    };
    assert!(
        (km - km_between(here, east_of(here, 900.0))).abs() < 60.0,
        "reported {km:.0} km"
    );
}

/// A ferry carries walkers and cyclists and — deliberately — not cars. The
/// tile says only `kind = ferry`; every car ferry takes foot passengers and no
/// foot ferry takes cars, so this is the only assignment that cannot send
/// someone to a crossing they can't board.
#[test]
fn a_ferry_carries_everyone_but_the_car() {
    assert_eq!(RoadKind::Ferry.speed_kmh(TravelMode::Car), None);
    assert!(RoadKind::Ferry.speed_kmh(TravelMode::Foot).is_some());
    assert!(RoadKind::Ferry.speed_kmh(TravelMode::Bike).is_some());
    // Its wait is already inside its speed, so it pays no junction delay on
    // top — the slipways would otherwise be charged as give-ways.
    assert_eq!(junction_delay(RoadKind::Ferry, TravelMode::Foot), 0.0);
    // Slower than walking, or the router would treat open water as a shortcut
    // wherever a ferry happens to run parallel to a road.
    let ferry = RoadKind::Ferry.speed_kmh(TravelMode::Foot).unwrap();
    assert!(ferry > RoadKind::Footway.speed_kmh(TravelMode::Foot).unwrap());
    assert!(ferry < RoadKind::Cycleway.speed_kmh(TravelMode::Bike).unwrap());
}

/// "No road near the destination" must mean there is no road there — not that
/// there is a whole town there which this mode cannot reach.
///
/// The case is real and the ferry work is what surfaced it: a car asked to
/// cross an estuary snapped its destination fine, but the far shore is its own
/// component (only the ferry links them, and a car may not use one), so the
/// core snap failed and the planner announced there was no road in Breskens.
#[test]
fn an_unreachable_town_is_not_a_town_without_roads() {
    let base = (TILE.0 as f64 * EXTENT, TILE.1 as f64 * EXTENT);
    let at = |x: f64, y: f64| (base.0 + x, base.1 + y);

    let mut g = RoadGraph::new(53.2194);
    // Two street networks, far apart, with nothing between them — and
    // deliberately *different sizes*. The routable core is the largest
    // component, so two equal islands leave which one wins to the order
    // Kosaraju happens to finish in, and the test would be asserting against
    // a coin flip (it did: the first draft passed by reporting NoPath for the
    // *start* being outside the core).
    for x in 0..6 {
        let x = f64::from(x) * 400.0;
        seg(
            &mut g,
            at(x, 0.0),
            at(x + 400.0, 0.0),
            RoadKind::Residential,
        );
    }
    seg(
        &mut g,
        at(20_000.0, 0.0),
        at(20_400.0, 0.0),
        RoadKind::Residential,
    );
    g.finish();

    let here = g.lattice_to_lat_lng(base.0, base.1);
    let across = g.lattice_to_lat_lng(base.0 + 20_000.0, base.1);
    assert_eq!(
        g.route(here, across, TravelMode::Car).err(),
        Some(RouteError::NoPath),
        "a road that exists but does not connect is NoPath, not NoEnd"
    );

    // …and somewhere with genuinely no road still reports the absence.
    let empty = g.lattice_to_lat_lng(base.0 + 3000.0, base.1 + 40_000.0);
    assert_eq!(
        g.route(here, empty, TravelMode::Car).err(),
        Some(RouteError::NoEnd)
    );
}

/// A* is only correct while the heuristic never over-estimates. Tightening it
/// to the graph's own fastest edge is the whole speed-up, and it is also
/// exactly where admissibility could be lost — so check it against every edge
/// in a real graph rather than by reading the code.
#[test]
fn the_heuristic_never_overestimates() {
    let g = graph();
    for mode in TravelMode::ALL {
        let top = g.top_speed_ms(mode);
        for edge in g.edges.iter().flatten() {
            let Some(kmh) = edge.kind.speed_kmh(mode) else {
                continue;
            };
            assert!(
                kmh / 3.6 <= top + 1e-9,
                "{mode:?}: {:?} runs at {kmh} km/h, above the heuristic's {:.1} km/h ceiling",
                edge.kind,
                top * 3.6
            );
        }
        // …and it must actually be tighter than the mode's global ceiling, or
        // there was no point (the fixture is a city: no 130 km/h anything).
        assert!(
            top < mode.max_speed_ms(),
            "{mode:?}: the graph ceiling is no tighter than the mode's"
        );
    }
}

#[test]
fn builds_a_connected_graph_from_one_tile() {
    let g = graph();
    assert!(g.node_count() > 1_000, "only {} nodes", g.node_count());
    assert!(g.edge_count() > g.node_count(), "graph is not connected up");
}

/// Node identity across a tile seam is *exact*, because every tile at a zoom
/// shares one integer lattice. This is the property the whole stitching model
/// rests on, so pin it directly rather than trusting the graph counts.
#[test]
fn the_global_lattice_is_shared_between_neighbouring_tiles() {
    // A point one unit inside tile (8490, 5319)'s eastern edge, expressed from
    // that tile and from its eastern neighbour's buffer, lands on one integer.
    let from_left = TILE.0 as f64 * EXTENT + 4095.0;
    let from_right = (TILE.0 + 1) as f64 * EXTENT + -1.0;
    assert_eq!(from_left, from_right);
}

#[test]
fn routes_across_the_city_by_car() {
    let g = graph();
    let a = tile_point(0.3, 0.35);
    let b = tile_point(0.7, 0.65);
    let route = g.route(a, b, TravelMode::Car).expect("car route");

    // The straight-line distance between the endpoints, for a sanity band: a
    // real route is longer than the crow flies but not by a factor of three.
    let direct = {
        let pa = RoadGraph::lat_lng_to_lattice(a.0, a.1);
        let pb = RoadGraph::lat_lng_to_lattice(b.0, b.1);
        ((pb.0 - pa.0).powi(2) + (pb.1 - pa.1).powi(2)).sqrt() * g.m_per_unit
    };
    assert!(
        route.distance_m >= direct * 0.9 && route.distance_m < direct * 3.0,
        "route {:.0} m vs direct {:.0} m",
        route.distance_m,
        direct
    );
    assert!(route.duration_s > 0.0);
    assert!(route.points.len() > 4);
    assert!(route.steps.len() >= 2, "expected a turn list");
    assert_eq!(route.steps.last().unwrap().maneuver, Maneuver::Arrive);
    // Steps must add up to the route (the Arrive step carries no distance).
    let stepped: f64 = route.steps.iter().map(|s| s.distance_m).sum();
    assert!((stepped - route.distance_m).abs() < 1.0);
}

/// Walking uses paths a car may not, so the same pair routes differently — the
/// point of having modes at all.
#[test]
fn modes_differ_and_all_three_find_a_route() {
    let g = graph();
    let a = tile_point(0.35, 0.4);
    let b = tile_point(0.6, 0.55);
    let mut seen = Vec::new();
    for mode in TravelMode::ALL {
        let r = g
            .route(a, b, mode)
            .unwrap_or_else(|e| panic!("{mode:?}: {e}"));
        seen.push((mode, r.distance_m, r.duration_s));
    }
    let car = seen.iter().find(|(m, ..)| *m == TravelMode::Car).unwrap();
    let foot = seen.iter().find(|(m, ..)| *m == TravelMode::Foot).unwrap();
    assert!(foot.2 > car.2, "walking must be slower than driving");
    assert!(
        foot.1 <= car.1 * 1.05,
        "walking may cut through where a car may not: foot {:.0} m vs car {:.0} m",
        foot.1,
        car.1
    );
}

/// A car must never be routed onto a footway or a cycleway, and a bike must
/// never be sent down a motorway. This is the access filter, checked at the
/// table rather than through a route that might happen to avoid them anyway.
#[test]
fn access_filter_matches_the_mode() {
    for kind in [
        RoadKind::Footway,
        RoadKind::Cycleway,
        RoadKind::Steps,
        RoadKind::Pedestrian,
    ] {
        assert!(
            kind.speed_kmh(TravelMode::Car).is_none(),
            "{kind:?} open to cars"
        );
    }
    for kind in [RoadKind::Motorway, RoadKind::Trunk] {
        assert!(
            kind.speed_kmh(TravelMode::Bike).is_none(),
            "{kind:?} open to bikes"
        );
        assert!(
            kind.speed_kmh(TravelMode::Foot).is_none(),
            "{kind:?} open to walkers"
        );
    }
    assert!(RoadKind::Rail.speed_kmh(TravelMode::Foot).is_none());
    // …and a motorway is the fastest thing a car has.
    let car_speeds: Vec<f64> = [
        RoadKind::Motorway,
        RoadKind::Residential,
        RoadKind::LivingStreet,
    ]
    .iter()
    .map(|k| k.speed_kmh(TravelMode::Car).unwrap())
    .collect();
    assert!(car_speeds[0] > car_speeds[1] && car_speeds[1] > car_speeds[2]);
}

/// One-ways bind cars and not pedestrians — the asymmetry that keeps a walking
/// route from taking an absurd loop around a city-centre one-way system.
#[test]
fn oneway_binds_only_the_modes_that_respect_it() {
    assert!(TravelMode::Car.respects_oneway());
    assert!(!TravelMode::Bike.respects_oneway());
    assert!(!TravelMode::Foot.respects_oneway());

    let g = graph();
    let edge = Edge {
        to: 0,
        len_m: 100.0,
        kind: RoadKind::Residential,
        against_oneway: true,
        bike_denied: false,
        unpaved: false,
        level: 0,
    };
    assert!(g.edge_cost(&edge, TravelMode::Car).is_none());
    assert!(g.edge_cost(&edge, TravelMode::Foot).is_some());
}

#[test]
fn steps_carry_street_names_from_the_label_layer() {
    let g = graph();
    let route = g
        .route(
            tile_point(0.3, 0.35),
            tile_point(0.7, 0.65),
            TravelMode::Car,
        )
        .expect("route");
    let named = route
        .steps
        .iter()
        .filter(|s| {
            s.name
                .as_deref()
                .is_some_and(|n| !n.starts_with("the ") && !n.is_empty())
        })
        .count();
    assert!(
        named > 0,
        "no step picked up a name from `street_labels`: {:?}",
        route.steps.iter().map(|s| &s.name).collect::<Vec<_>>()
    );
}

/// The routable core must be strongly connected: from any node in it, every
/// other core node is reachable. This is the property `route` leans on when it
/// snaps both endpoints into the core — both in one SCC ⇒ a path exists — and it
/// is what stops a one-way stub near an address from trapping the search (the
/// car failure where a two-way bike route worked but the car got `NoPath`). Also
/// check the core actually excludes something, or it would not be doing its job.
#[test]
fn routable_core_is_strongly_connected() {
    let g = graph();
    for mode in TravelMode::ALL {
        let core = g.routable_core(mode);
        let members: Vec<u32> = (0..core.len() as u32)
            .filter(|&i| core[i as usize])
            .collect();
        assert!(
            members.len() > 100,
            "{mode:?}: core is only {} nodes",
            members.len()
        );

        // From an arbitrary core node, the forward-reachable set covers the core.
        let reach: std::collections::HashSet<u32> =
            g.reachable(members[0], mode).into_iter().collect();
        for &m in &members {
            assert!(
                reach.contains(&m),
                "{mode:?}: core node {m} not reachable from the core"
            );
        }

        // The car network has one-way stubs and service loops the core drops;
        // if the core equalled every usable node it would not be filtering them.
        let usable = (0..core.len() as u32)
            .filter(|&i| {
                g.edges[i as usize]
                    .iter()
                    .any(|e| g.edge_cost(e, mode).is_some())
            })
            .count();
        if mode == TravelMode::Car {
            assert!(
                members.len() < usable,
                "car core {} == usable {usable}: no stubs excluded?",
                members.len()
            );
        }
    }
}

/// A start snapped to a one-way stub must still route: `route` snaps into the
/// core, not onto the nearest node. Regression for a one-way-stub start that
/// returned `NoPath` for cars while routing fine for bikes.
#[test]
fn a_stub_start_still_routes() {
    let g = graph();
    // Find a node whose forward-reachable car set is a tiny pocket — a one-way
    // stub — and confirm a car route starting on top of it still succeeds.
    let core = g.routable_core(TravelMode::Car);
    let stub = (0..g.coords.len() as u32).find(|&i| {
        g.edges[i as usize]
            .iter()
            .any(|e| g.edge_cost(e, TravelMode::Car).is_some())
            && !core[i as usize]
            && g.reachable(i, TravelMode::Car).len() < 100
    });
    let Some(stub) = stub else {
        // No such pocket in this fixture — nothing to regress against, and the
        // strongly-connected-core test already covers the mechanism.
        return;
    };
    let (slat, slng) = g.node_lat_lng(stub);
    let route = g
        .route((slat, slng), tile_point(0.7, 0.65), TravelMode::Car)
        .expect("a stub start must snap into the core and route");
    assert!(route.distance_m > 0.0 && !route.steps.is_empty());
}

/// The noding pass is load-bearing, and its absence does not announce itself —
/// it just returns `NoPath` for ordinary routes. Pin both halves of the fix:
/// every tee it was built for is a real vertex afterwards, and the network is
/// one piece.
///
/// "Every tee", not "every contact". The pass deliberately leaves nodes lying on
/// segments now — a street passing under a viaduct is exactly that, and joining
/// the two is the bug the guards exist to prevent. What must not be left lying
/// is a **dangle at a shared grade**: a line that stops dead on another line it
/// could really have met. That is the invariant, and stating it this way is what
/// keeps the test from being satisfied by a pass that simply joins less.
#[test]
fn noding_reconnects_the_network() {
    let g = graph();

    let mut segs = Vec::new();
    for (i, adj) in g.edges.iter().enumerate() {
        for e in adj {
            if e.to as usize > i {
                segs.push((i, e.to as usize));
            }
        }
    }
    let dangling = (0..g.coords.len())
        .filter(|&i| g.node_terminal[i])
        .filter(|&i| {
            segs.iter().any(|&(a, b)| {
                a != i
                        && b != i
                        // A grade all three share, or they were never in contact.
                        && g.node_levels[i] & g.node_levels[a] & g.node_levels[b] != 0
                        && point_seg_distance(g.coords[i], g.coords[a], g.coords[b])
                            <= NODE_TOL
            })
        })
        .count();
    assert!(
        dangling <= g.coords.len() / 100,
        "{dangling} dangling ends still sit on a segment they could have joined"
    );

    // …and the usable network is essentially one component per mode. Before
    // noding this was 58% for cars, which is what made a cross-town route fail.
    for (mode, floor) in [
        (TravelMode::Car, 0.85),
        (TravelMode::Bike, 0.90),
        (TravelMode::Foot, 0.90),
    ] {
        let (largest, usable) = g.largest_component(mode);
        assert!(
            largest as f64 >= usable as f64 * floor,
            "{mode:?}: largest component {largest} of {usable} usable nodes"
        );
    }
}

#[test]
fn turn_classification() {
    assert_eq!(Maneuver::from_turn(0.0), Maneuver::Continue);
    assert_eq!(Maneuver::from_turn(-90.0), Maneuver::Left);
    assert_eq!(Maneuver::from_turn(90.0), Maneuver::Right);
    assert_eq!(Maneuver::from_turn(-25.0), Maneuver::SlightLeft);
    assert_eq!(Maneuver::from_turn(175.0), Maneuver::UTurn);
    assert_eq!(Maneuver::from_turn(-170.0), Maneuver::UTurn);
    // Bearings wrap: north-ish to north-ish across 0° is a small turn, not the
    // 340° one a naive subtraction would report.
    assert_eq!(signed_turn(350.0, 5.0), 15.0);
    assert_eq!(
        Maneuver::from_turn(signed_turn(350.0, 5.0)),
        Maneuver::Continue
    );
}

#[test]
fn a_missing_area_fails_with_a_reason_not_a_panic() {
    let g = graph();
    // Mid-Atlantic: nothing to snap onto.
    assert_eq!(
        g.route((0.0, -30.0), (0.1, -30.1), TravelMode::Car).err(),
        Some(RouteError::NoStart)
    );
    assert_eq!(
        RoadGraph::new(0.0)
            .route((0.0, 0.0), (1.0, 1.0), TravelMode::Car)
            .err(),
        Some(RouteError::NoData)
    );
}

/// The corridor is a capsule, not a bounding box: a diagonal route must cost
/// far fewer tiles than the square it spans.
#[test]
fn corridor_is_narrower_than_a_bounding_box() {
    let a = (53.16, 6.45);
    let b = (53.26, 6.66);
    let tiles = corridor_tiles(a, b, MAX_ROUTE_TILES).expect("within budget");
    let ta = lat_lng_to_tile_frac(a.0, a.1, ROUTING_ZOOM);
    let tb = lat_lng_to_tile_frac(b.0, b.1, ROUTING_ZOOM);
    let bbox =
        ((ta.0 - tb.0).abs().floor() as usize + 3) * ((ta.1 - tb.1).abs().floor() as usize + 3);
    assert!(
        tiles.len() < bbox,
        "corridor {} tiles vs bbox {bbox}",
        tiles.len()
    );
    // Both endpoints' own tiles must be in it, or nothing can snap.
    for p in [ta, tb] {
        assert!(tiles.contains(&(p.0.floor() as i64, p.1.floor() as i64)));
    }
    // Nearest-first ordering, so a partial fetch is still useful.
    assert_eq!(tiles[0], (ta.0.floor() as i64, ta.1.floor() as i64));
}

/// The capsule's width must grow sub-linearly, so a long route stays thin — the
/// regression for a cross-province query that once asked for hundreds of tiles.
/// A route four times as long must not cost four times the *width*.
#[test]
fn the_corridor_stays_thin_as_it_lengthens() {
    // Two mostly-east–west spans from a common origin, one ~4× the other, both
    // within the tile budget.
    let origin = (53.20, 6.00);
    let short = corridor_tiles(origin, (53.20, 6.05), MAX_ROUTE_TILES).unwrap();
    let long = corridor_tiles(origin, (53.20, 6.20), MAX_ROUTE_TILES).unwrap();
    // Width ≈ tiles per unit of length. The long route is ~4× longer; if width
    // grew with length its tiles-per-length would climb, and it must not.
    let span = |ts: &[(i64, i64)]| {
        let xs: Vec<i64> = ts.iter().map(|t| t.0).collect();
        (xs.iter().max().unwrap() - xs.iter().min().unwrap()).max(1) as f64
    };
    let density = |ts: &[(i64, i64)]| ts.len() as f64 / span(ts);
    assert!(
        density(&long) <= density(&short) * 1.2,
        "long corridor is wider per km: {:.1} vs {:.1} tiles/tile-of-length",
        density(&long),
        density(&short)
    );
}

#[test]
fn an_overlong_route_is_refused_with_a_distance() {
    // ~150 km apart — far past what a single-zoom corridor can hold.
    let err = corridor_tiles((52.09, 5.11), (53.22, 6.57), MAX_ROUTE_TILES).unwrap_err();
    let RouteError::TooFar { km } = err else {
        panic!("expected TooFar, got {err:?}");
    };
    assert!(
        (100.0..250.0).contains(&km),
        "implausible distance {km:.0} km"
    );
    // The message leads with the distance, not a tile count.
    let msg = err.to_string();
    assert!(msg.contains("Too far") && msg.contains("km"), "{msg}");
}

#[test]
fn formatting() {
    assert_eq!(format_distance(432.0), "430 m");
    assert_eq!(format_distance(4321.0), "4.3 km");
    assert_eq!(format_distance(43_210.0), "43 km");
    assert_eq!(format_duration(30.0), "1 min");
    assert_eq!(format_duration(510.0), "9 min");
    assert_eq!(format_duration(4_320.0), "1 h 12 min");
}
