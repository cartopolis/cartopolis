use super::*;
use crate::systems::routing::format_duration;

/// The whole point of the sheet: whatever the screen, most of it is still map,
/// and the sheet never reaches past the top of it.
///
/// The floor is the deliberate exception — on a viewport too short to honour
/// the fraction the sheet overflows visibly rather than collapsing to nothing,
/// the trade every narrow control here makes.
#[test]
fn the_sheet_leaves_the_map_visible() {
    // A phone in portrait, a phone in landscape, a laptop, a big monitor.
    for viewport_h in [640.0_f32, 320.0, 800.0, 1400.0] {
        let bottom = 60.0;
        let sheet = lib_sheet_max_height(viewport_h, bottom);
        assert!(
            sheet <= viewport_h * SHEET_MAX_FRACTION + LIB_SHEET_HEADER_PTS + 0.01,
            "a {viewport_h} pt viewport gave the sheet {sheet} pt"
        );
        assert!(
            sheet <= viewport_h - bottom - theme::OSD_MARGIN + 0.01 || sheet == SHEET_BODY_MIN_PTS,
            "the sheet reached past the top of a {viewport_h} pt viewport"
        );
        assert!(
            sheet >= SHEET_BODY_MIN_PTS,
            "the sheet collapsed at {viewport_h}"
        );
    }
}

/// The two cases where the sheet is not where the user last left it, both of
/// them "it is standing in front of the next thing you have to do".
#[test]
fn the_sheet_gets_out_of_the_way_by_itself() {
    // Nothing to collapse to: the fields are the content.
    assert_eq!(
        effective_detent(SheetDetent::Peek, false, false),
        SheetDetent::Full
    );
    // A map pick needs the map, so it wins even over a planner with no route.
    assert_eq!(
        effective_detent(SheetDetent::Full, false, true),
        SheetDetent::Peek
    );
    assert_eq!(
        effective_detent(SheetDetent::Full, true, true),
        SheetDetent::Peek
    );
    // Otherwise the user's own answer stands, both ways round.
    assert_eq!(
        effective_detent(SheetDetent::Full, true, false),
        SheetDetent::Full
    );
    assert_eq!(
        effective_detent(SheetDetent::Peek, true, false),
        SheetDetent::Peek
    );
}

/// The ribbon must be a proper triangle mesh whose width matches the request —
/// the route is unusable if the line is inverted, degenerate, or hairline.
#[test]
fn ribbon_has_the_requested_width() {
    let points = vec![Vec2::new(0.0, 0.0), Vec2::new(100.0, 0.0)];
    let mesh = ribbon(&points, &vec![0.0; points.len()], 3.0);
    let positions = match mesh.attribute(Mesh::ATTRIBUTE_POSITION).unwrap() {
        bevy::render::mesh::VertexAttributeValues::Float32x3(v) => v.clone(),
        _ => panic!("positions are not Float32x3"),
    };
    // A due-east segment is offset purely in z, by the half-width each way.
    let max_z = positions.iter().map(|p| p[2]).fold(f32::MIN, f32::max);
    let min_z = positions.iter().map(|p| p[2]).fold(f32::MAX, f32::min);
    assert!(
        (max_z - 3.0).abs() < 1e-3 && (min_z + 3.0).abs() < 1e-3,
        "width off: {min_z}..{max_z}"
    );
    assert!(positions.iter().all(|p| p[1] == 0.0), "ribbon must be flat");

    let Some(Indices::U32(indices)) = mesh.indices() else {
        panic!("expected u32 indices")
    };
    assert!(!indices.is_empty() && indices.len() % 3 == 0);
    assert!(indices.iter().all(|&i| (i as usize) < positions.len()));
}

/// A downward-wound ribbon is backface-culled: no line on the map, no error
/// anywhere. This is the test for the bug that shipped in the first draft.
#[test]
fn ribbon_faces_up() {
    let points = vec![
        Vec2::new(0.0, 0.0),
        Vec2::new(100.0, 0.0),
        Vec2::new(100.0, 80.0),
    ];
    let mesh = ribbon(&points, &vec![0.0; points.len()], 3.0);
    let positions = match mesh.attribute(Mesh::ATTRIBUTE_POSITION).unwrap() {
        bevy::render::mesh::VertexAttributeValues::Float32x3(v) => v.clone(),
        _ => panic!("positions are not Float32x3"),
    };
    let Some(Indices::U32(indices)) = mesh.indices() else {
        panic!("expected u32 indices")
    };
    for t in indices.chunks_exact(3) {
        let (a, b, c) = (
            positions[t[0] as usize],
            positions[t[1] as usize],
            positions[t[2] as usize],
        );
        // Y component of (b−a)×(c−a): positive means the triangle faces up.
        let cross = (b[2] - a[2]) * (c[0] - a[0]) - (b[0] - a[0]) * (c[2] - a[2]);
        assert!(cross > 0.0, "triangle {t:?} wound downward (cross {cross})");
    }
}

/// The bevel join must never spike, whatever the turn: no ribbon vertex may sit
/// further than `half_width` from the centreline. This is the regression for the
/// "giant arrowhead" — a mitre offset shooting across a junction. A near-hairpin
/// on a *short* segment (the exact case the noded graph produces) is the worst
/// case, so use one.
#[test]
fn bevel_join_never_spikes() {
    const HW: f32 = 3.0;
    let points = vec![
        Vec2::new(0.0, 0.0),
        Vec2::new(100.0, 0.0),
        Vec2::new(100.8, 0.3), // a 0.85 m segment doubling back ~150°
        Vec2::new(0.0, 5.0),
    ];
    let mesh = ribbon(&points, &vec![0.0; points.len()], HW);
    let positions = match mesh.attribute(Mesh::ATTRIBUTE_POSITION).unwrap() {
        bevy::render::mesh::VertexAttributeValues::Float32x3(v) => v.clone(),
        _ => panic!("positions are not Float32x3"),
    };
    // Every vertex must lie within half_width of the nearest input point (the
    // bevel offsets from vertices only), give or take rounding.
    for p in &positions {
        let v = Vec2::new(p[0], p[2]);
        let nearest = points
            .iter()
            .map(|q| q.distance(v))
            .fold(f32::MAX, f32::min);
        assert!(
            nearest <= HW + 1e-3,
            "spike: vertex {v:?} is {nearest:.2} m from the centreline (half_width {HW})"
        );
    }
}

/// The whole path end to end — corridor, real tile fetches, graph, A* — against
/// a live vector source. Not part of the suite: it needs the network (or a warm
/// `cache/tiles`) and a `CARTO_TILE_URL`, so it is the manual check for "does
/// routing actually work against the tiles we ship", the way `vector_tiles`'
/// `dump_png` is the manual check for the styling.
///
/// ```text
/// CARTO_TILE_URL=https://tiles.cartopolis.org/tiles/osm/{z}/{x}/{y} \
///   cargo test -p cartopolis solves_a_real_route -- --ignored --nocapture
/// ```
#[test]
#[ignore = "fetches real tiles over the network"]
fn solves_a_real_route() {
    let template = std::env::var("CARTO_TILE_URL")
        .unwrap_or_else(|_| "https://tiles.cartopolis.org/tiles/osm/{z}/{x}/{y}".to_string());
    // Two points ~1 km apart in the fixture city, by coordinate.
    let from = (53.2194, 6.5665);
    let to = (53.2109, 6.5642);
    let progress = (AtomicUsize::new(0), AtomicUsize::new(0));
    let cache = Mutex::new(None);

    for mode in TravelMode::ALL {
        let t0 = Instant::now();
        // The two halves the live solve runs on two executors: the corridor
        // fetch on the `IoTaskPool`, the graph build + A* on a worker. Driving
        // them back to back here is the same sequence, minus the hand-off.
        let route =
            futures_lite::future::block_on(fetch_corridor(from, to, &template, &progress, &cache))
                .and_then(|input| build_and_search(input, from, to, mode, &cache))
                .unwrap_or_else(|e| panic!("{mode:?}: {e}"));
        println!(
            "{:<5} {:>8} {:>10} in {:>5} ms  ({} steps, {} tiles)",
            mode.label(),
            format_duration(route.duration_s),
            format_distance(route.distance_m),
            t0.elapsed().as_millis(),
            route.steps.len(),
            progress.1.load(Ordering::Relaxed),
        );
        for step in &route.steps {
            println!(
                "    {:<14} {:<52} {}",
                format!("{:?}", step.maneuver),
                step.instruction(),
                format_distance(step.distance_m)
            );
        }
        // These two points are about 1.2 km apart on foot; nothing sane is
        // under 800 m or over 4 km.
        assert!(
            (800.0..4_000.0).contains(&route.distance_m),
            "{mode:?}: implausible distance {:.0} m",
            route.distance_m
        );
    }
}

/// A bench for the *cost model*, against live tiles. Prints distance, time and
/// the implied average speed for a spread of real corridors, which is the only
/// way to tell whether a change to the speed table or the junction delay left
/// city driving plausible — a route that is merely *a* route tells you nothing.
///
/// Not an assertion beyond a sanity band: the right answer is a judgement about
/// a city, so this exists to be read.
///
/// ```text
/// cargo test -p cartopolis cost_model_bench -- --ignored --nocapture
/// ```
#[test]
#[ignore = "printing hatch: it reports numbers, it does not assert them"]
fn cost_model_bench() {
    let template = std::env::var("CARTO_TILE_URL")
        .unwrap_or_else(|_| "https://tiles.cartopolis.org/tiles/osm/{z}/{x}/{y}".to_string());
    // Groningen: across the centre, centre→ring road, and a long haul out to
    // the motorway — a residential-grid case, a mixed case and a case that
    // should end up on the trunk network.
    let pairs: [(&str, (f64, f64), (f64, f64)); 7] = [
        ("centre → station", (53.2194, 6.5665), (53.2109, 6.5642)),
        ("centre → Paddepoel", (53.2194, 6.5665), (53.2405, 6.5455)),
        ("centre → Ring West", (53.2194, 6.5665), (53.2230, 6.5170)),
        ("centre → Hoogkerk", (53.2194, 6.5665), (53.2100, 6.4700)),
        // Across the Westerschelde: only the ferry connects these, so a walk
        // or a bike route existing at all is the ferry layer working, and the
        // car having none is the deliberate refusal (`RoadKind::Ferry`).
        (
            "Vlissingen → Breskens",
            (51.4450, 3.5700),
            (51.3990, 3.5570),
        ),
        // Past street-level reach: these take `plan_corridor`'s staged path —
        // coarse haul, detailed ends. The snap column is the one to read,
        // because driver mode calls anything past 60 m off-route, and a
        // coarse-only long route snaps ~281 m out.
        (
            "Groningen → Assen (26 km)",
            (53.2194, 6.5665),
            (52.9950, 6.5620),
        ),
        (
            "Groningen → Amsterdam (180 km)",
            (53.2194, 6.5665),
            (52.3730, 4.8920),
        ),
    ];
    for (name, from, to) in pairs {
        let progress = (AtomicUsize::new(0), AtomicUsize::new(0));
        let cache = Mutex::new(None);
        for mode in TravelMode::ALL {
            let input = futures_lite::future::block_on(fetch_corridor(
                from, to, &template, &progress, &cache,
            ));
            // Timed from here, so the figure is the graph build plus the
            // search rather than the network — the second and third mode of a
            // pair reuse the cached graph and so time the search alone.
            let t0 = Instant::now();
            let route = input.and_then(|i| build_and_search(i, from, to, mode, &cache));
            let solve = t0.elapsed();
            match route {
                Ok(r) => {
                    // Distance from each pin to the road the route actually
                    // starts and ends on — driver mode's off-route threshold
                    // is 60 m, so this column is a pass/fail.
                    let snap = |pin: [f64; 2], road: [f64; 2]| {
                        let mid = (pin[0] + road[0]) * 0.5;
                        let dx = (road[1] - pin[1]) * 111_320.0 * mid.to_radians().cos();
                        let dy = (road[0] - pin[0]) * 110_540.0;
                        dx.hypot(dy)
                    };
                    let last = r.points.len() - 1;
                    println!(
                        "{name:<32} {:<5} {:>8} {:>10}  {:>5.1} km/h  snap {:>4.0}/{:<4.0} m  \
                             ({} steps, {} tiles, {} ms)",
                        mode.label(),
                        format_distance(r.distance_m),
                        format_duration(r.duration_s),
                        r.distance_m / r.duration_s * 3.6,
                        snap(r.points[0], r.points[1]),
                        snap(r.points[last], r.points[last - 1]),
                        r.steps.len(),
                        progress.1.load(Ordering::Relaxed),
                        solve.as_millis(),
                    );
                }
                Err(e) => println!("{name:<32} {:<5} FAILED: {e}", mode.label()),
            }
        }
        println!();
    }
}

/// What a *coarse* graph can actually do, at each zoom, on a route far beyond
/// street-level reach. The question this answers is whether a long haul needs
/// the fine network stitched onto its ends at all, or whether the generalised
/// hierarchy routes end to end on its own.
///
/// ```text
/// cargo test -p cartopolis coarse_zoom_probe -- --ignored --nocapture
/// ```
#[test]
#[ignore = "printing hatch: it reports numbers, it does not assert them"]
fn coarse_zoom_probe() {
    let template = std::env::var("CARTO_TILE_URL")
        .unwrap_or_else(|_| "https://tiles.cartopolis.org/tiles/osm/{z}/{x}/{y}".to_string());
    let cases: [(&str, (f64, f64), (f64, f64)); 3] = [
        (
            "Groningen → Assen (26 km)",
            (53.2194, 6.5665),
            (52.9950, 6.5620),
        ),
        (
            "Groningen → Zwolle (95 km)",
            (53.2194, 6.5665),
            (52.5168, 6.0830),
        ),
        (
            "Groningen → Amsterdam (180 km)",
            (53.2194, 6.5665),
            (52.3730, 4.8920),
        ),
    ];
    for (name, from, to) in cases {
        println!("── {name}");
        for zoom in [14u32, 12, 10] {
            let tiles = match routing::corridor_tiles_at(zoom, from, to, routing::MAX_ROUTE_TILES) {
                Ok(t) => t,
                Err(e) => {
                    println!("   z{zoom:<2} corridor refused: {e}");
                    continue;
                }
            };
            let t0 = Instant::now();
            let mut bytes = 0usize;
            let mut graph = RoadGraph::new_at(zoom, (from.0 + to.0) / 2.0);
            for tile in &tiles {
                let key = TileKey::new(tile.0, tile.1, zoom);
                let Ok(body) = futures_lite::future::block_on(load_tile_image(&key, &template))
                else {
                    continue;
                };
                bytes += body.len();
                if let Ok(td) = crate::systems::vector_tiles::TileData::parse(&body) {
                    let _ = graph.add_tile(&td, *tile);
                }
            }
            graph.finish();
            let fetched = t0.elapsed();
            let t1 = Instant::now();
            let route = graph.route(from, to, TravelMode::Car);
            // How far the pins are from the road the route actually starts
            // and ends on. This is the number driver mode lives or dies by:
            // its off-route threshold is 60 m, so a coarse snap further out
            // than that puts a driver off-route on the first frame.
            let snap = |pin: [f64; 2], road: [f64; 2]| {
                let mid = (pin[0] + road[0]) * 0.5;
                let dx = (road[1] - pin[1]) * 111_320.0 * mid.to_radians().cos();
                let dy = (road[0] - pin[0]) * 110_540.0;
                dx.hypot(dy)
            };
            match route {
                    Ok(r) => println!(
                        "   z{zoom:<2} {:>3} tiles {:>6.1} MB  {:>8} {:>10}  {:>5.1} km/h  \
                         {:>3} steps  snap {:>5.0}/{:<5.0} m  (fetch+build {:>5} ms, search {:>4} ms)",
                        tiles.len(),
                        bytes as f64 / 1024.0 / 1024.0,
                        format_distance(r.distance_m),
                        format_duration(r.duration_s),
                        r.distance_m / r.duration_s * 3.6,
                        r.steps.len(),
                        snap(r.points[0], r.points[1]),
                        snap(r.points[r.points.len() - 1], r.points[r.points.len() - 2]),
                        fetched.as_millis(),
                        t1.elapsed().as_millis(),
                    ),
                    Err(e) => println!(
                        "   z{zoom:<2} {:>3} tiles {:>6.1} MB  FAILED: {e}",
                        tiles.len(),
                        bytes as f64 / 1024.0 / 1024.0
                    ),
                }
        }
    }
}

#[test]
fn dedupe_merges_sub_segment_points() {
    // Exact duplicates and points closer than MIN_SEGMENT_M collapse; points
    // further apart survive. Here: a cluster near the origin (all within the
    // threshold of each other) → one point, then a distant one → two total.
    let pts = dedupe(
        [
            Vec2::new(0.0, 0.0),
            Vec2::new(0.0, 0.0),
            Vec2::new(MIN_SEGMENT_M * 0.5, 0.0), // sub-segment: merges
            Vec2::new(50.0, 0.0),                // far: kept
        ]
        .into_iter(),
    );
    let (pts, kept) = pts;
    assert_eq!(pts.len(), 2, "got {pts:?}");
    assert_eq!(pts[0], Vec2::new(0.0, 0.0));
    assert_eq!(pts[1], Vec2::new(50.0, 0.0));
    // The surviving indices are what keeps a parallel array (the per-point
    // grade) in step with the thinned geometry.
    assert_eq!(
        kept,
        vec![0, 3],
        "the kept indices must name the input points"
    );
}

#[test]
fn a_disc_is_closed_and_flat() {
    let mesh = disc(4.0);
    let Some(Indices::U32(indices)) = mesh.indices() else {
        panic!("expected u32 indices")
    };
    assert_eq!(indices.len() % 3, 0);
    // Every rim vertex is used, so the disc has no gap in it.
    let used: std::collections::HashSet<u32> = indices.iter().copied().collect();
    assert_eq!(used.len(), 25, "centre plus 24 rim vertices");
}

/// Editing a field must drop the coordinate, so a half-typed name can never be
/// solved against the place the user has just typed over.
#[test]
fn editing_an_endpoint_invalidates_it() {
    let mut route = RouteState::default();
    route.from.set("Start", (53.2194, 6.5665));
    route.to.set("Destination", (53.2110, 6.5645));
    assert!(route.from.coord.is_some());

    route.solved_for = Some(((53.2194, 6.5665), (53.2110, 6.5645), TravelMode::Car));
    route.invalidate();
    assert_eq!(route.solved_for, None, "a new solve must be due");
}

/// A route has to stay visible from the altitude that frames it, and at
/// street level it still has to look like a road marking rather than a
/// motorway.
///
/// The regression this pins: the drawn line is world geometry a fixed 6.4 m
/// wide, which is a third of a pixel from the 30 km a cross-province route is
/// framed at — the route was *there*, correct, and invisible.
#[test]
fn the_route_line_stays_wide_enough_to_see() {
    // Ground level and a city view keep the real road width.
    // Behind a walking avatar the line is a footpath…
    assert!((route_width_scale(0.0) - STREET_LEVEL_SCALE).abs() < f32::EPSILON);
    assert!((route_width_scale(10.0) - STREET_LEVEL_SCALE).abs() < f32::EPSILON);
    // …and from driver mode's chase rig and above, the road it always was.
    assert!((route_width_scale(65.0) - 1.0).abs() < f32::EPSILON);
    assert!((route_width_scale(500.0) - 1.0).abs() < f32::EPSILON);

    // Every altitude a route is looked at from lands within a factor of √2 of
    // one line's worth of screen — which is what the power-of-two bands buy in
    // exchange for not re-triangulating the mesh on every frame of a zoom.
    for altitude in [2_000.0_f32, 5_000.0, 16_000.0, 30_000.0, 45_000.0] {
        let drawn = ROUTE_HALF_WIDTH * route_width_scale(altitude);
        let ideal = altitude * ROUTE_WIDTH_PER_ALT;
        let ratio = drawn / ideal;
        assert!(
            (0.70..=1.42).contains(&ratio),
            "at {altitude} m the line is {drawn} m against an ideal {ideal} m",
        );
    }

    // Monotone, so a climb never *narrows* the line, and bounded.
    let mut previous = 0.0;
    for step in 0..200 {
        let scale = route_width_scale(step as f32 * 1_000.0);
        assert!(scale >= previous, "the line narrowed at {step} km");
        assert!(scale <= ROUTE_WIDTH_MAX_SCALE);
        previous = scale;
    }
    // Nothing pathological from a camera that has gone wrong.
    assert!((route_width_scale(f32::NAN) - 1.0).abs() < f32::EPSILON);
    assert!(route_width_scale(f32::INFINITY) <= ROUTE_WIDTH_MAX_SCALE);
}

/// The framing altitude has to scale with the route, and stay inside the band
/// where the map tier still streams detail.
#[test]
fn framing_scales_with_the_route() {
    let mut route = RouteState::default();
    let mk = |span: f64| Route {
        mode: TravelMode::Car,
        points: vec![[53.20, 6.55], [53.20 + span, 6.55]],
        levels: vec![0; 2],
        distance_m: span * 111_320.0,
        duration_s: 60.0,
        steps: Vec::new(),
    };

    route.route = Some(mk(0.001));
    let short = frame_route(&route).unwrap().altitude.unwrap();
    route.route = Some(mk(0.1));
    let long = frame_route(&route).unwrap().altitude.unwrap();

    assert!(long > short, "a longer route must be framed from higher up");
    assert!((200.0..=FRAME_MAX_ALT).contains(&short));
    assert!((200.0..=FRAME_MAX_ALT).contains(&long));

    // A route far longer than the tier can frame is still framed *inside* the
    // tier — above `GLOBE_SHOW_ALT` the line is not drawn, so pulling further
    // back would frame an empty map. `MAX_STAGED_KM` is 360.
    route.route = Some(mk(3.5));
    let cross_country = frame_route(&route).unwrap().altitude.unwrap();
    assert!(
        cross_country < f64::from(crate::systems::globe::GLOBE_SHOW_ALT),
        "framed at {cross_country} m, where the route is hidden",
    );
}

/// "Route to" is the primary act: the start defaults to where you are, so the
/// user only ever supplies a destination.
#[test]
fn the_start_defaults_to_the_current_position() {
    let mut route = RouteState::default();
    assert!(route.default_start(Some((53.22, 6.57))));
    assert_eq!(route.from.label, HERE_LABEL);
    assert_eq!(route.from.coord, Some((53.22, 6.57)));
}

/// "My position" is where the avatar is now, not where the camera was when
/// the sheet first opened: a start frozen at Winsum refused "My position →
/// Winsum" as the same place from Groningen (2026-11-02). A start the user
/// chose stays put, and a few metres of walking re-solve nothing.
#[test]
fn my_position_follows_the_avatar() {
    let mut route = RouteState::default();
    route.default_start(Some((53.3284, 6.5164)));
    assert!(route.follow_here((53.2194, 6.5665)));
    assert_eq!(route.from.coord, Some((53.2194, 6.5665)));
    assert!(route.solved_for.is_none(), "a moved start re-solves");

    assert!(!route.follow_here((53.21942, 6.56652)), "2 m is not a move");

    route.from.set("Vismarkt", (53.2165, 6.5640));
    assert!(!route.follow_here((53.3284, 6.5164)));
    assert_eq!(route.from.coord, Some((53.2165, 6.5640)));
}

/// The planner, not the router, knows which end is "My position", so a
/// refusal for being too close says where you are rather than blaming the
/// pair.
#[test]
fn too_close_says_you_are_there() {
    let mut route = RouteState::default();
    route.default_start(Some((53.2194, 6.5665)));
    route.to.set("Grote Markt", (53.2195, 6.5668));
    let msg = failure_message(&route, &RouteError::TooClose);
    assert!(msg.starts_with("You are already at Grote Markt"), "{msg}");
    assert_eq!(
        failure_message(&route, &RouteError::NoPath),
        RouteError::NoPath.to_string()
    );
}

/// `set_endpoint_at` fires the reverse geocode on the `IoTaskPool`, which a
/// unit test has no Bevy app to create for it.
fn with_task_pool() {
    bevy::tasks::IoTaskPool::get_or_init(Default::default);
}

fn a_route() -> Route {
    Route {
        mode: TravelMode::Car,
        points: vec![[53.20, 6.55], [53.205, 6.55], [53.21, 6.55]],
        levels: vec![0; 3],
        distance_m: 1000.0,
        duration_s: 60.0,
        steps: Vec::new(),
    }
}

/// A reroute must not blank the display. It keeps the previous route to draw
/// and measure against, and — the part that does the actual work — it leaves
/// the generation alone, which is what stops `update_route_visuals` despawning
/// the line and rebuilding it from nothing.
#[test]
fn a_reroute_holds_the_previous_route_and_the_drawn_line() {
    with_task_pool();
    let mut route = RouteState::default();
    route.route = Some(a_route());
    let gen_before = route.generation;

    route.reroute_from(53.2001, 6.5502);

    assert!(route.route.is_none(), "the solved route is gone");
    assert!(route.held.is_some(), "…but the display still has one");
    assert!(route.displayed().is_some());
    assert_eq!(
        route.generation, gen_before,
        "an unchanged generation is what keeps the line on the map"
    );
    assert!(route.solved_for.is_none(), "and a solve is due");
    assert_eq!(route.to.coord, None, "only the start moved");
    assert_eq!(route.from.coord, Some((53.2001, 6.5502)));
    assert_eq!(
        route.from.label, HERE_LABEL,
        "the start is the vehicle, not a coordinate to reverse-geocode"
    );
}

/// Everything that is *not* a reroute must still take the line away — the whole
/// reason `held` is set by one method rather than by `invalidate` itself.
#[test]
fn an_ordinary_edit_drops_a_held_route() {
    with_task_pool();
    let mut route = RouteState::default();
    route.route = Some(a_route());
    route.reroute_from(53.2001, 6.5502);
    assert!(route.held.is_some());

    // The user picks a new destination mid-reroute.
    route.set_endpoint_at(Endpoint::To, 53.30, 6.60);
    assert!(
        route.held.is_none(),
        "the held line must not outlive the trip"
    );
    assert!(route.displayed().is_none());
}

/// A reroute's corridor is a shorter, narrower version of the one already
/// cached, so it must hit the cache — a full refetch is the seconds of blank
/// display this exists to remove. And an overlapping corridor that is *not*
/// contained must miss, or a solve would run on a graph missing its own roads.
#[test]
fn a_contained_corridor_reuses_the_cached_graph() {
    let wide = routing::Corridor::Direct(vec![(0, 0), (1, 0), (2, 0), (3, 0)]);
    let narrow = routing::Corridor::Direct(vec![(1, 0), (2, 0)]);
    assert!(covers(&wide, &narrow));
    assert!(covers(&wide, &wide), "equality is containment");
    assert!(!covers(&narrow, &wide));
    assert!(
        !covers(&wide, &routing::Corridor::Direct(vec![(2, 0), (4, 0)])),
        "one tile short is a miss, not a nearly-hit"
    );
    // Staged is three graphs on two lattices; containment is not asked there.
    assert!(!covers(
        &wide,
        &routing::Corridor::Staged {
            start: vec![(0, 0)],
            end: vec![(1, 0)],
            coarse: vec![(2, 0)],
        }
    ));
}

/// …but it must never stamp over a start the user chose. This is the one that
/// would ruin a route: "Route from here" on the map, then opening the panel,
/// and the picked start silently becoming the camera position again.
#[test]
fn defaulting_the_start_never_overwrites_one() {
    with_task_pool();
    let mut route = RouteState::default();
    route.set_endpoint_at(Endpoint::From, 53.10, 6.10);
    assert!(!route.default_start(Some((53.22, 6.57))));
    assert_eq!(route.from.coord, Some((53.10, 6.10)));

    // A half-typed label with no coordinate yet is still the user's.
    let mut route = RouteState::default();
    route.from.label = "Gron".into();
    assert!(!route.default_start(Some((53.22, 6.57))));
    assert_eq!(route.from.label, "Gron");
}

/// With nowhere to resolve "here" to (no anchor, no camera) it does nothing
/// rather than inventing a start at null island.
#[test]
fn defaulting_the_start_needs_a_position() {
    let mut route = RouteState::default();
    assert!(!route.default_start(None));
    assert!(route.from.label.is_empty() && route.from.coord.is_none());
}

/// Setting an endpoint from the map — whether by "pick on map" or by the map's
/// own "Route to here" — must drop the drawn route with it. Leaving the old
/// line up under a new destination is the failure this shares between the two
/// call sites, which is why they share the method.
#[test]
fn setting_an_endpoint_from_the_map_invalidates_the_route() {
    with_task_pool();
    let mut route = RouteState::default();
    route.route = Some(Route {
        mode: TravelMode::Car,
        points: vec![[53.20, 6.55], [53.21, 6.55]],
        levels: vec![0; 2],
        distance_m: 1000.0,
        duration_s: 60.0,
        steps: Vec::new(),
    });
    route.picking = Some(Endpoint::To);
    let gen_before = route.generation;

    route.set_endpoint_at(Endpoint::To, 53.30, 6.60);

    assert!(route.route.is_none(), "the stale line must go");
    assert_ne!(route.generation, gen_before, "so the world visuals rebuild");
    assert!(route.picking.is_none(), "and the map-pick arming is spent");
    assert_eq!(route.to.coord, Some((53.30, 6.60)));
    assert!(route.solved_for.is_none(), "the solver must run again");
}
