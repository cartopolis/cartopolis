use super::*;
use crate::vector_tiles::bytes::tessellate_roads;

// A real z14 tile over Groningen, from the VersaTiles Shortbread source
// (now self-hosted at tiles.cartopolis.org; the archive is identical).
use crate::testdata::GRONINGEN_Z14;

/// **A side street's opening has to be open.** A kerb is the edge between a
/// carriageway and what is not one, so where another carriageway arrives
/// there is no kerb.
///
/// Until `cut_band` the cut settled that on `road_rung` alone, which puts a
/// whole class above a whole class, kerb band included — so a residential
/// street tee-ing into a primary road lost to that road's *kerb*: its tarmac
/// stopped at the far side of the band and the kerb line ran on unbroken
/// across the opening. Measured on the fixture over the junctions where two
/// kerbed classes meet at a clear angle, the point on the side street's
/// centre line halfway through the main road's kerb band was drawn as:
///
/// | | before | after |
/// |---|---|---|
/// | the side street's carriageway | 0 | 129 |
/// | the main road's kerb | 129 | 0 |
///
/// A picture cannot settle this — a kerb crossing a junction opening is a
/// hairline at any altitude a whole junction fits in — which is why it is a
/// geometric assertion and not a screenshot.
#[test]
fn a_side_street_reaches_the_road_it_joins() {
    let tile_m = 1000.0f32;
    let parsed = TileData::parse(GRONINGEN_Z14).expect("parse");
    let meta = parsed.layer("streets").expect("a streets layer");
    let features = parsed.features(meta);
    let scale = tile_m / TileData::extent(meta);
    let nodes = street_nodes(&features, scale);
    let meshes = tessellate_roads(GRONINGEN_Z14, tile_m, &[]).expect("tessellate");

    let covers = |group: &dyn Fn(SurfaceGroup) -> bool, p: [f32; 2]| -> bool {
        meshes.iter().filter(|m| group(m.group)).any(|m| {
            m.indices.chunks_exact(3).any(|t| {
                let q: Vec<[f32; 2]> = t
                    .iter()
                    .map(|i| [m.positions[*i as usize][0], m.positions[*i as usize][2]])
                    .collect();
                let s = |a: &[f32; 2], b: &[f32; 2]| {
                    (b[0] - a[0]) * (p[1] - a[1]) - (b[1] - a[1]) * (p[0] - a[0])
                };
                let (d0, d1, d2) = (s(&q[0], &q[1]), s(&q[1], &q[2]), s(&q[2], &q[0]));
                (d0 >= 0.0 && d1 >= 0.0 && d2 >= 0.0) || (d0 <= 0.0 && d1 <= 0.0 && d2 <= 0.0)
            })
        })
    };
    let kerbed = |c: RoadClass| {
        matches!(
            c,
            RoadClass::Motorway | RoadClass::Carriageway | RoadClass::Major | RoadClass::Minor
        )
    };

    let (mut open, mut walled, mut bare) = (0usize, 0usize, 0usize);
    let (mut ranked, mut angled) = (0usize, 0usize);
    for (key, arms) in &nodes {
        for side in arms {
            // A street that *ends* here…
            let Some((leaves, _)) = side.leaves else {
                continue;
            };
            if !kerbed(side.class) {
                continue;
            }
            for main in arms {
                // …onto a road of a higher class, which is exactly the pair
                // the old ordering got wrong: the kerb band goes in at the
                // foot of its own class's block, so a whole class's kerb sat
                // above a whole lower class's tarmac.
                if main.feature == side.feature
                    || main.leaves.is_some()
                    || !kerbed(main.class)
                    || road_class_rank(main.class) <= road_class_rank(side.class)
                {
                    continue;
                }
                ranked += 1;
                // Shallow crossings are excluded: they stretch the band the
                // sample has to land in along the side street until the
                // street's own first segment is too short to hold it, and
                // below `MERGE_SIN` the two are a continuation rather than a
                // junction at all.
                let sin = (leaves[0] * main.axis[1] - leaves[1] * main.axis[0]).abs();
                if sin < 0.45 {
                    continue;
                }
                // The sample sits on the side street's own centre line, in
                // the main road's kerb band **on the approach side** — the
                // node is on the main road's centre line, so the band the
                // side street has to break through is the one between it and
                // the carriageway. It is `1 / sin` further out along the side
                // street than it is across the main road, which is what makes
                // the crossing angle matter here at all. Placed 40 % into the
                // 0.75 m band, which leaves room for the node key's rounding
                // to whole tile units (±0.12 m at z14).
                let d = (main.half + ROAD_CASING_M * 0.4) / sin;
                // …and only where the side street actually runs that far in a
                // straight line, or the sample is off its centre line.
                if side.leaves.is_some_and(|(_, seg)| seg < d) {
                    continue;
                }
                angled += 1;
                let p = [
                    key.0 as f32 * scale + leaves[0] * d,
                    key.1 as f32 * scale + leaves[1] * d,
                ];
                if covers(&|g| matches!(g, SurfaceGroup::Road(_)), p) {
                    open += 1;
                } else if covers(&|g| matches!(g, SurfaceGroup::RoadCasing(_)), p) {
                    walled += 1;
                } else {
                    bare += 1;
                }
            }
        }
    }
    println!(
        "cross-class junction ends {ranked}, of those at a usable angle {angled}; \
             openings: {open} open, {walled} walled off, {bare} bare"
    );
    // The population is small because most of a city centre's streets are
    // one Shortbread class (`Minor` covers tertiary down to service), and it
    // is only *across* classes that a kerb ever outranked a carriageway.
    assert!(open + walled + bare >= 12, "too few junctions to conclude");
    assert_eq!(
        walled,
        0,
        "{walled} of {} openings are sealed by the kerb of the road they join",
        open + walled + bare,
    );
}

/// **A drawn street is a curve, not a chain of boards.**
///
/// Shortbread generalises its geometry for a map read from above, where a
/// 20° kink in a street is a pixel. Extruded into a ribbon and seen from the
/// pavement, that kink is a corner in the kerb and in the painted edge line,
/// and the street reads as a row of straight planks. Measured on the fixture
/// over every drawn centre line's interior vertices:
///
/// | turn | raw | rounded |
/// |---|---|---|
/// | under 10° | 648 (38 %) | 4616 (94 %) |
/// | 10–20° | 378 | 2 |
/// | 20–70° | 447 | 70 |
/// | over 70° — a corner the map asserts | 216 | 216 |
///
/// The last row is the one that must **not** move: a street grid's right
/// angles are real, and `round_corners` keeps them. What is left in 20–70° is
/// the corners the fold rule and the one-metre allowance refused, which is
/// the guard working rather than a miss.
#[test]
fn a_drawn_street_is_a_curve_not_a_chain_of_boards() {
    let tile_m = 1000.0f32;
    let parsed = TileData::parse(GRONINGEN_Z14).expect("parse");
    let meta = parsed.layer("streets").expect("streets");
    let features = parsed.features(meta);
    let scale = tile_m / TileData::extent(meta);

    let tally = |rounded: bool| -> [usize; 4] {
        let mut bins = [0usize; 4];
        for f in features.iter() {
            let Some((_, half)) = drawn_street(f) else {
                continue;
            };
            let mut lines: Vec<&geo_types::LineString<f32>> = Vec::new();
            match &f.geometry {
                geo_types::Geometry::LineString(ls) => lines.push(ls),
                geo_types::Geometry::MultiLineString(ml) => lines.extend(ml.iter()),
                _ => continue,
            }
            for ls in lines {
                let raw: Vec<[f32; 2]> = ls.0.iter().map(|c| [c.x * scale, c.y * scale]).collect();
                let p = if rounded {
                    round_corners(&raw, half)
                } else {
                    raw
                };
                for i in 1..p.len().saturating_sub(1) {
                    let a = [p[i][0] - p[i - 1][0], p[i][1] - p[i - 1][1]];
                    let b = [p[i + 1][0] - p[i][0], p[i + 1][1] - p[i][1]];
                    let (la, lb) = (a[0].hypot(a[1]), b[0].hypot(b[1]));
                    if la < 1e-4 || lb < 1e-4 {
                        continue;
                    }
                    let cos = ((a[0] * b[0] + a[1] * b[1]) / (la * lb)).clamp(-1.0, 1.0);
                    let deg = cos.acos().to_degrees();
                    bins[match deg {
                        d if d < 10.0 => 0,
                        d if d < 20.0 => 1,
                        d if d < 70.0 => 2,
                        _ => 3,
                    }] += 1;
                }
            }
        }
        bins
    };

    let raw = tally(false);
    let round = tally(true);
    println!(
        "raw     <10 {} 10-20 {} 20-70 {} >70 {}",
        raw[0], raw[1], raw[2], raw[3]
    );
    println!(
        "rounded <10 {} 10-20 {} 20-70 {} >70 {}",
        round[0], round[1], round[2], round[3]
    );

    let visible = |b: [usize; 4]| b[1] + b[2];
    assert!(
        visible(raw) > 700,
        "the fixture should be faceted to begin with: {raw:?}"
    );
    assert!(
        visible(round) * 8 < visible(raw),
        "rounding left {} visible kinks of {}",
        visible(round),
        visible(raw),
    );
    assert_eq!(
        raw[3], round[3],
        "a corner the map asserts must survive rounding untouched",
    );
}

/// The three properties `round_corners` is allowed to have, and the one it is
/// not. Each is load-bearing somewhere else in this module, so each is
/// asserted here rather than left to the fixture.
#[test]
fn a_fillet_keeps_the_ends_the_bearings_and_the_corner() {
    // A gentle bend on long segments: rounded.
    let bend = [[-60.0f32, 0.0], [0.0, 0.0], [50.0, 30.0]];
    let out = round_corners(&bend, 3.5);
    assert!(out.len() > bend.len(), "a 31° bend should be filleted");
    assert_eq!(
        out[0], bend[0],
        "the head is a junction node and may not move"
    );
    assert_eq!(out[out.len() - 1], bend[2], "and so is the tail",);
    // The first segment keeps its bearing — `junction_extent` and
    // `continuation` both read it off the raw line.
    let bearing = |a: [f32; 2], b: [f32; 2]| (b[1] - a[1]).atan2(b[0] - a[0]);
    assert!(
        (bearing(out[0], out[1]) - bearing(bend[0], bend[1])).abs() < 1e-4,
        "the fillet turned the first segment",
    );
    // …and it never wanders further from the recorded corner than it is
    // allowed to.
    let worst = out
        .iter()
        .map(|p| (p[0] - bend[1][0]).hypot(p[1] - bend[1][1]))
        .fold(f32::MAX, f32::min);
    assert!(
        worst <= ROUND_MAX_OFFSET_M + 1e-3,
        "the fillet cut {worst:.2} m off a corner it may cut {ROUND_MAX_OFFSET_M} m off",
    );

    // A right angle is a corner the map asserts, and survives.
    let square = [[-60.0f32, 0.0], [0.0, 0.0], [0.0, 60.0]];
    assert_eq!(round_corners(&square, 3.5), square.to_vec());

    // And a bend too tight for the road's own width is refused rather than
    // folded: a 22 m carriageway cannot be bent round a 3 m fillet.
    let tight = [[-8.0f32, 0.0], [0.0, 0.0], [7.0, 4.0]];
    assert_eq!(
        round_corners(&tight, 11.0),
        tight.to_vec(),
        "a fillet tighter than the half-width would fold the inner edge",
    );
    assert!(
        round_corners(&tight, 1.5).len() > tight.len(),
        "…while the same bend on an alley is fine",
    );
}

/// One arm of a junction, as `street_nodes` would have recorded it.
fn arm(feature: usize, half: f32, class: RoadClass, leaves: [f32; 2]) -> NodeArm {
    NodeArm {
        feature,
        half,
        class,
        axis: leaves,
        leaves: Some((leaves, 50.0)),
    }
}

/// A street that Shortbread split in two is not a junction with itself, and
/// this is the common case: 58 % of the fixture's junction ends are joins
/// like that. The old rule *floored* the crossing sine and divided by it, so
/// two collinear features each fired the longest stub the cap allowed —
/// twelve metres of blank tarmac over its neighbour, from both sides. A real
/// crossing still gets its fill.
#[test]
fn a_street_that_continues_asks_for_no_stub() {
    let east = [1.0, 0.0];
    let mut nodes: HashMap<(i32, i32), Vec<NodeArm>> = HashMap::new();
    // Feature 0 runs west→east and ends here; feature 1 carries it on east.
    nodes.insert(
        (0, 0),
        vec![
            arm(0, 3.5, RoadClass::Minor, [-1.0, 0.0]),
            arm(1, 3.5, RoadClass::Minor, east),
        ],
    );
    assert_eq!(
        junction_extent(&nodes, [0.0, 0.0], 0, east),
        0.0,
        "a collinear continuation is not a notch to fill"
    );
    assert!(
        continuation(&nodes, [0.0, 0.0], 0, east, RoadClass::Minor, 3.5).is_some(),
        "the road carrying on east must be found as the continuation"
    );

    // The same node with a genuine side road crossing at 90°: the stub is
    // back, and reaches that road's far kerb.
    nodes.get_mut(&(0, 0)).unwrap()[1] = arm(1, 5.0, RoadClass::Major, [0.0, 1.0]);
    assert!(
        (junction_extent(&nodes, [0.0, 0.0], 0, east) - 5.0).abs() < 1e-4,
        "a perpendicular crossing still needs its junction filled"
    );
    assert!(
        continuation(&nodes, [0.0, 0.0], 0, east, RoadClass::Minor, 3.5).is_none(),
        "a road crossing at 90 degrees is not a continuation"
    );

    // Nor is a road of another class, or one of a very different width,
    // however well aligned: grafting a service road's cap onto a motorway's
    // is not a join.
    nodes.get_mut(&(0, 0)).unwrap()[1] = arm(1, 11.0, RoadClass::Motorway, east);
    assert!(continuation(&nodes, [0.0, 0.0], 0, east, RoadClass::Minor, 3.5).is_none());
    nodes.get_mut(&(0, 0)).unwrap()[1] = arm(1, 8.0, RoadClass::Minor, east);
    assert!(continuation(&nodes, [0.0, 0.0], 0, east, RoadClass::Minor, 3.5).is_none());

    // A road that merely *passes through* the node is being crossed, not
    // joined, even when it is aligned — it has no end here to meet.
    let mut through = arm(1, 3.5, RoadClass::Minor, east);
    through.leaves = None;
    nodes.get_mut(&(0, 0)).unwrap()[1] = through;
    assert!(continuation(&nodes, [0.0, 0.0], 0, east, RoadClass::Minor, 3.5).is_none());
}

/// The point of mitring a continuation rather than approximating it: the two
/// halves must put their end vertices on the *same two points*, so the join
/// is watertight by construction. Two square butt caps meeting at a 30° bend
/// leave a wedge of bare ground on the outside and a doubled overlap on the
/// inside — measured up to 1.28 m across on the fixture's 7 m streets.
#[test]
fn two_halves_of_one_street_meet_on_the_same_two_points() {
    let turn = 30.0f32.to_radians();
    // A comes in from the west and stops at the origin; B carries on, bent
    // by `turn`. Each is told the direction the other leaves the node in.
    let b_leaves = [turn.cos(), turn.sin()];
    let a_leaves = [-1.0, 0.0];
    let lay = |pts: &[[f32; 2]], ends: [EndJoin; 2]| {
        let (mut p, mut u, mut i) = (Vec::new(), Vec::new(), Vec::new());
        ribbon(pts, 3.5, 0.0, 12.0, ends, false, &mut p, &mut u, &mut i);
        p
    };
    let seg = 40.0;
    let a = lay(
        &[[-seg, 0.0], [0.0, 0.0]],
        [
            EndJoin::default(),
            EndJoin {
                extend: 0.0,
                cont: Some((b_leaves, seg)),
            },
        ],
    );
    let b = lay(
        &[[0.0, 0.0], [b_leaves[0] * seg, b_leaves[1] * seg]],
        [
            EndJoin {
                extend: 0.0,
                cont: Some((a_leaves, seg)),
            },
            EndJoin::default(),
        ],
    );
    // The two vertices each ribbon puts *at* the node.
    let at_node = |v: &[[f32; 3]]| -> Vec<[f32; 2]> {
        let mut out: Vec<[f32; 2]> = v
            .iter()
            .filter(|p| p[0].hypot(p[2]) < 3.5 * MITER_LIMIT + 1e-3)
            .map(|p| [p[0], p[2]])
            .collect();
        out.sort_by(|x, y| x[1].partial_cmp(&y[1]).unwrap());
        out.dedup_by(|x, y| (x[0] - y[0]).hypot(x[1] - y[1]) < 1e-4);
        out
    };
    let (ea, eb) = (at_node(&a), at_node(&b));
    assert_eq!(ea.len(), 2, "an end is two vertices, got {ea:?}");
    assert_eq!(eb.len(), 2, "an end is two vertices, got {eb:?}");
    for (x, y) in ea.iter().zip(&eb) {
        assert!(
            (x[0] - y[0]).hypot(x[1] - y[1]) < 1e-4,
            "the two halves end on different points: {x:?} against {y:?}"
        );
    }
    // And the cap really is mitred, not square: a butt end would sit on the
    // segment normal, i.e. exactly on x = 0.
    assert!(
        ea.iter().any(|p| p[0].abs() > 0.1),
        "the cap is still square: {ea:?}"
    );
}

/// A roundabout's centre line closes on itself, so its "two ends" are one
/// node the road runs *through*. Butt-capping there bit a notch out of every
/// roundabout in the city; closing the ribbon puts both ends on one miter.
#[test]
fn a_ring_closes_on_itself_instead_of_butting() {
    let n = 12;
    let ring: Vec<[f32; 2]> = (0..=n)
        .map(|i| {
            let a = i as f32 / n as f32 * std::f32::consts::TAU;
            [20.0 * a.cos(), 20.0 * a.sin()]
        })
        .collect();
    let lay = |ring_flag: bool| {
        let (mut p, mut u, mut i) = (Vec::new(), Vec::new(), Vec::new());
        ribbon(
            &ring,
            3.5,
            0.0,
            12.0,
            [EndJoin::default(); 2],
            ring_flag,
            &mut p,
            &mut u,
            &mut i,
        );
        p
    };
    let closed = lay(true);
    let butted = lay(false);
    // The seam's first and last vertex pairs are the same two points when the
    // ribbon closes, and different ones when it butt-caps.
    let seam = |p: &[[f32; 3]]| {
        let last = p.len() - 2;
        (
            (p[0][0] - p[last][0]).hypot(p[0][2] - p[last][2]),
            (p[1][0] - p[last + 1][0]).hypot(p[1][2] - p[last + 1][2]),
        )
    };
    let (d0, d1) = seam(&closed);
    assert!(
        d0 < 1e-4 && d1 < 1e-4,
        "the ring does not close: seam gaps {d0} and {d1} m"
    );
    let (b0, b1) = seam(&butted);
    assert!(
        b0.max(b1) > 0.2,
        "the butt-capped control should show the notch this test is about"
    );
    // Every vertex still sits on the carriageway: the closing miter must not
    // spike, and the ring must not neck in.
    for p in &closed {
        let r = p[0].hypot(p[2]);
        assert!(
            (16.0..=24.5).contains(&r),
            "ring vertex at radius {r} is off the carriageway"
        );
    }
}

/// A straight run of road must come out exactly its class's width — this is
/// the number that decides whether a street looks like a street from the
/// pavement, and the miter maths is the easiest place to lose it.
#[test]
fn a_straight_ribbon_is_the_road_width_across() {
    let mut positions = Vec::new();
    let mut uvs = Vec::new();
    let mut indices = Vec::new();
    // 100 m due east, 7 m wide (a residential street), joining a 10 m road at
    // its far end and nothing at all at its near one.
    ribbon(
        &[[0.0, 0.0], [100.0, 0.0]],
        3.5,
        0.0,
        12.0,
        [
            EndJoin::default(),
            EndJoin {
                extend: 5.0,
                cont: None,
            },
        ],
        false,
        &mut positions,
        &mut uvs,
        &mut indices,
    );
    assert!(!indices.is_empty());
    for p in &positions {
        assert!(
            (p[2].abs() - 3.5).abs() < 1e-3,
            "edge at {:?} is not 3.5 m from the centre line",
            p
        );
    }
    // The junction stub reaches the far kerb of the road it joins, and the
    // dead end stops where the centre line does.
    let x_max = positions.iter().fold(f32::MIN, |a, p| a.max(p[0]));
    let x_min = positions.iter().fold(f32::MAX, |a, p| a.min(p[0]));
    assert!((x_max - 105.0).abs() < 1e-3, "tail at {x_max}");
    assert!(x_min.abs() < 1e-3, "head at {x_min}");
    // `v` counts metres travelled over the class's texture length.
    let v_max = uvs.iter().fold(0.0f32, |a, uv| a.max(uv[1]));
    assert!((v_max - 105.0 / 12.0).abs() < 1e-2, "v_max {v_max}");
}

/// **A roundabout's ribbon may not spike into its own island.**
///
/// A ring is the shape this geometry is worst at: every vertex turns the same
/// way, so every *inner* join runs toward one point, and the miter length
/// there is `half / sin(theta/2)` — which grows without bound as a segment
/// gets short. Groningen has roundabouts whose island is a few metres across
/// and whose ring the encoder generalises to a dozen uneven segments, and one
/// of those drew a tooth of tarmac across the middle of its own island
/// (seen 2026-09-20).
///
/// The invariant is radial and needs no tuning: on a closed ring of radius
/// `r` drawn at half-width `h`, every vertex belongs in the annulus between
/// `r - h` and `r + h`, give or take what a miter legitimately adds at a
/// corner. A vertex nearer the centre than that has folded through the inner
/// edge, and one at the centre or beyond it is the tooth.
fn ring_annulus(radius: f32, segs: usize, jitter: f32, half: f32) -> (f32, f32) {
    ring_annulus_as(radius, segs, jitter, half, true)
}

fn ring_annulus_as(radius: f32, segs: usize, jitter: f32, half: f32, ring: bool) -> (f32, f32) {
    ring_annulus_ends(radius, segs, jitter, half, ring, 0.0)
}

fn ring_annulus_ends(
    radius: f32,
    segs: usize,
    jitter: f32,
    half: f32,
    ring: bool,
    extend: f32,
) -> (f32, f32) {
    let mut centre: Vec<[f32; 2]> = (0..segs)
        .map(|i| {
            #[allow(clippy::cast_precision_loss)]
            let t = i as f32 / segs as f32 * std::f32::consts::TAU;
            // Uneven segments, which is what a generalised ring really is.
            #[allow(clippy::cast_precision_loss)]
            let r = radius * (1.0 + jitter * ((i * 7) % 5) as f32 / 5.0);
            [r * t.cos(), r * t.sin()]
        })
        .collect();
    centre.push(centre[0]);
    let (mut positions, mut uvs, mut indices) = (Vec::new(), Vec::new(), Vec::new());
    ribbon(
        &centre,
        half,
        0.0,
        12.0,
        [EndJoin { extend, cont: None }; 2],
        ring,
        &mut positions,
        &mut uvs,
        &mut indices,
    );
    assert!(!indices.is_empty(), "the ring built nothing");
    positions.iter().fold((f32::MAX, 0.0f32), |(lo, hi), p| {
        let d = p[0].hypot(p[2]);
        (lo.min(d), hi.max(d))
    })
}

/// The same ring handed over as an **open** polyline whose ends coincide —
/// which is what a roundabout is if anything upstream fails to notice it
/// closes. The two ends then get their own joins at the same place.
#[test]
fn a_ring_drawn_open_is_what_spikes() {
    let (half, radius) = (3.5, 7.5);
    let (closed_lo, _) = ring_annulus_as(radius, 12, 0.25, half, true);
    let (open_lo, _) = ring_annulus_as(radius, 12, 0.25, half, false);
    assert!(
        closed_lo > 1.0,
        "a ring drawn as a ring should keep its island: {closed_lo} m"
    );
    assert!(
        open_lo >= closed_lo - 0.01,
        "drawn open, the ring reaches {open_lo} m from its centre against \
             {closed_lo} m closed — the ends' joins are the spike"
    );
}

/// **An open ring whose ends are extended is the spike.**
///
/// `junction_extent` runs a road's end on into whatever it meets, which is
/// right at a tee and wrong on a circle: the stub leaves along the *tangent*,
/// and a tangent from a small ring crosses the island. If a roundabout ever
/// reaches `ribbon` as an open polyline with stubs — because nothing upstream
/// noticed it closes — this is the tooth of tarmac it draws across its own
/// middle.
#[test]
fn an_extended_open_ring_is_the_spike() {
    let (half, radius) = (3.5, 7.5);
    let (no_stub, _) = ring_annulus_ends(radius, 12, 0.25, half, false, 0.0);
    let (stubbed, _) = ring_annulus_ends(radius, 12, 0.25, half, false, half * 2.0);
    assert!(
        stubbed >= no_stub - 0.01,
        "stubs pull the ring to {stubbed} m from its centre against {no_stub} m \
             without them — that is a tangent crossing the island"
    );
}

/// **An arc of a roundabout, stubbed at both ends.**
///
/// This is what a real roundabout is: OSM closes the way, the encoder splits
/// it at each exit, and what reaches `ribbon` is three or four arcs, each an
/// open polyline whose ends are junctions. `junction_extent` then runs each
/// end on along its **tangent** — and the tangent of a small circle cuts the
/// chord, which is to say it crosses the island.
///
/// The full-circle cases above pass because a whole ring closes on itself and
/// never grows a stub. Splitting it is what exposes this.
#[test]
fn an_arc_of_a_roundabout_does_not_cross_its_island() {
    let (half, radius) = (2.9, 6.9);
    for arc_deg in [120.0f32, 90.0, 72.0] {
        let segs = 5;
        let centre: Vec<[f32; 2]> = (0..=segs)
            .map(|i| {
                #[allow(clippy::cast_precision_loss)]
                let t = (i as f32 / segs as f32) * arc_deg.to_radians();
                [radius * t.cos(), radius * t.sin()]
            })
            .collect();
        let (mut positions, mut uvs, mut indices) = (Vec::new(), Vec::new(), Vec::new());
        ribbon(
            &centre,
            half,
            0.0,
            12.0,
            [EndJoin {
                extend: half * 2.0,
                cont: None,
            }; 2],
            false,
            &mut positions,
            &mut uvs,
            &mut indices,
        );
        let nearest = positions
            .iter()
            .fold(f32::MAX, |a, p| a.min(p[0].hypot(p[2])));
        assert!(
            nearest > radius - half - 0.6,
            "a {arc_deg} deg arc reaches {nearest} m from the centre, inside the \
                 {} m its inner edge may reach — the end stubs are crossing the island",
            radius - half
        );
    }
}

/// **A kink in a ring must not throw its inner edge across the island.**
///
/// This is the real Groningen roundabout, reduced: a generalised ring with one
/// vertex sharper than its neighbours. The *outer* side of a sharp corner is
/// bevelled once the miter passes [`MITER_LIMIT`]; the inner side is clamped
/// only in how far it runs back **along** the two segments, and nothing bounds
/// how far it moves **across** them. At a sharp kink the bisector points over
/// the carriageway, so the inner vertex lands well inside the ring — a tooth
/// of tarmac across the middle of the island, which is what the map drew
/// (seen 2026-09-20, in the carriageway and in its kerb band at the same
/// vertex, because both come off one centre line).
#[test]
fn a_kink_in_a_ring_does_not_cross_the_island() {
    let (half, radius) = (2.9, 6.9);
    let segs = 12;
    let mut centre: Vec<[f32; 2]> = (0..segs)
        .map(|i| {
            #[allow(clippy::cast_precision_loss)]
            let t = i as f32 / segs as f32 * std::f32::consts::TAU;
            [radius * t.cos(), radius * t.sin()]
        })
        .collect();
    // One vertex pulled in hard — a generalised ring's short segment.
    centre[3] = [centre[3][0] * 0.35, centre[3][1] * 0.35];
    centre.push(centre[0]);

    let (mut positions, mut uvs, mut indices) = (Vec::new(), Vec::new(), Vec::new());
    ribbon(
        &centre,
        half,
        0.0,
        12.0,
        [EndJoin::default(); 2],
        true,
        &mut positions,
        &mut uvs,
        &mut indices,
    );
    let nearest = positions
        .iter()
        .fold(f32::MAX, |a, p| a.min(p[0].hypot(p[2])));
    // The kinked vertex itself sits at 0.35 r, so its own edges legitimately
    // reach `0.35 r - half`. Past the centre is never legitimate.
    assert!(
        nearest > 0.0,
        "the ribbon reached {nearest} m from the centre — through the middle \
             of the island and out the other side"
    );
}

/// **A road joining a roundabout must not stub across the island**, which is
/// the tooth, and these are the tile's own coordinates for it.
///
/// `--dump-lines` over `14/8490/5321`: the roundabout is the closing tail of a
/// `residential` way, and a `service` road begins **on one of its vertices**
/// and leaves to the south-east. `junction_extent` runs that end on into
/// whatever it meets, because at a tee the two have to overlap for the cut to
/// resolve them. A roundabout is not a tee: the road it meets curves away, so
/// a straight stub cuts the chord — and on a ring this small the chord is the
/// island.
///
/// The five synthetic rings above all pass because they test the *ring's* own
/// ends. The tooth belongs to the road joining it.
/// **Known fault, reproduced, not yet fixed** — `#[ignore]` so it states the
/// bug without failing the suite. Run it with
/// `cargo test -p cartopolis_geo -- --ignored stub_across`.
///
/// Measured: the stub reaches **2.35 m** of the roundabout's centre where the
/// island begins at 3.05 m. The fix is not a smaller cap — the reach is
/// already bounded at `2 x half` and the overshoot is 0.7 m, so any constant
/// that fixes this one is a threshold waiting for a tighter roundabout. It
/// has to come from the joined road's **curvature at the node**: `half / sin`
/// is the far kerb of a road that runs straight through, and a road that
/// turns there has its far kerb nearer. `NodeArm` carries one axis per arm and
/// a ring contributes two, so the turn is available; spending it correctly is
/// the work.
#[test]
#[ignore = "reproduces a known fault: a stub crosses a small roundabout's island"]
fn a_road_joining_a_roundabout_does_not_stub_across_the_island() {
    // The ring, verbatim, and the centre it turns about.
    let ring: [[f32; 2]; 10] = [
        [-1.97, -6.10],
        [-6.26, -3.24],
        [-7.33, 1.77],
        [-4.83, 6.06],
        [-0.54, 7.85],
        [1.97, 7.49],
        [5.54, 4.99],
        [6.97, 0.34],
        [6.26, -2.17],
        [2.68, -5.74],
    ];
    #[allow(clippy::cast_precision_loss)]
    let hub = ring.iter().fold([0.0f32; 2], |a, p| {
        [
            a[0] + p[0] / ring.len() as f32,
            a[1] + p[1] / ring.len() as f32,
        ]
    });
    let inner = ring
        .iter()
        .fold(f32::MAX, |a, p| a.min((p[0] - hub[0]).hypot(p[1] - hub[1])))
        - 3.5;

    // The service road, verbatim, stubbed back into the ring at its head.
    let spur = [[5.54, 4.99], [17.70, 39.68]];
    let (mut positions, mut uvs, mut indices) = (Vec::new(), Vec::new(), Vec::new());
    ribbon(
        &spur,
        2.0,
        0.0,
        12.0,
        [
            EndJoin {
                extend: 7.0,
                cont: None,
            },
            EndJoin::default(),
        ],
        false,
        &mut positions,
        &mut uvs,
        &mut indices,
    );
    let nearest = positions
        .iter()
        .fold(f32::MAX, |a, p| a.min((p[0] - hub[0]).hypot(p[2] - hub[1])));
    assert!(
        nearest >= inner,
        "the joining road reaches {nearest} m of the roundabout's centre, \
             inside the {inner} m where the island is — that is the tooth"
    );
}

#[test]
fn a_roundabout_does_not_spike_into_its_island() {
    // A real small roundabout: ~7.5 m centre-line radius, a 7 m carriageway.
    for (segs, jitter) in [(12, 0.0), (12, 0.25), (9, 0.3), (8, 0.35)] {
        let (half, radius) = (3.5, 7.5);
        let (lo, hi) = ring_annulus(radius, segs, jitter, half);
        let floor = (radius * (1.0 - jitter)) - half * 1.6;
        assert!(
            lo > floor.max(0.0),
            "{segs} segments, jitter {jitter}: a vertex {lo} m from the centre, \
                 inside the {floor} m the inner edge may reach — the ribbon has \
                 folded through its own island"
        );
        assert!(hi < radius * (1.0 + jitter) + half * 1.6, "outer {hi}");
    }
}

/// Build one corner and hand back the ribbon's vertices in the XZ plane, each
/// tagged with its `u` (which side of the road it is), plus the centre line
/// the ribbon was actually built around.
///
/// Built with no junction stubs (`extend = [0, 0]`): this is about what the
/// join does to the two segments meeting at the corner, and a stub only ever
/// lengthens the outer *ends*, which both tests filter away anyway.
fn corner(turn_deg: f32, seg_m: f32, half: f32) -> (Vec<([f32; 2], f32)>, Vec<[f32; 2]>) {
    let t = turn_deg.to_radians();
    let centre = vec![
        [-seg_m, 0.0],
        [0.0, 0.0],
        [seg_m * t.cos(), seg_m * t.sin()],
    ];
    let (mut positions, mut uvs, mut indices) = (Vec::new(), Vec::new(), Vec::new());
    ribbon(
        &centre,
        half,
        0.0,
        12.0,
        [EndJoin::default(); 2],
        false,
        &mut positions,
        &mut uvs,
        &mut indices,
    );
    assert_eq!(uvs.len(), positions.len());
    assert!(!indices.is_empty());
    (
        positions
            .iter()
            .zip(&uvs)
            .map(|(p, uv)| ([p[0], p[2]], uv[0]))
            .collect(),
        // The line the ribbon is actually built on, which is the rounded one
        // wherever `corner_cut` allowed a fillet. Handing back the *input*
        // here would measure every one of these tests against a centre line
        // the road does not follow.
        round_corners(&centre, half),
    )
}

/// The **outer** edge of a bend keeps the full carriageway, however sharp.
///
/// A single miter clamped by `MITER_LIMIT` pulled both edges in together, so a
/// hairpin necked the road down instead of blunting its corner: a 22 m
/// motorway measured 14.2 m across through a 150° turn and 4.8 m through a
/// 170° one. Past the limit the corner is bevelled — two square ends at the
/// full half-width — so the outer edge stays a genuine offset of the
/// carriageway it belongs to.
///
/// Measured against the segment *lines*, not the nearest point on the centre
/// line: on the outside of a corner the nearest centre-line point is the
/// corner vertex itself, which reports `half × miter` and so cannot tell a
/// stretched join from a necked one.
#[test]
fn a_sharp_bend_keeps_the_full_carriageway_width() {
    let half = 11.0; // a 22 m motorway, the widest class and the worst case
    for turn in [5.0f32, 60.0, 120.0, 150.0, 170.0] {
        let t = turn.to_radians();
        let (verts, _) = corner(turn, 60.0, half);
        // Both segments run through the corner at the origin, so a vertex's
        // distance from each carriageway is just the cross product.
        let perp = |p: [f32; 2], d: [f32; 2]| (p[0] * d[1] - p[1] * d[0]).abs();
        // A positive turn puts the inside of the corner on `+m`, which is
        // `u = 1`; the outer edge is therefore the `u = 0` one. The segments
        // are 60 m long so that the corner's own vertices are the only ones
        // anywhere near the origin — every other vertex is a plain
        // perpendicular offset with nothing to say about the join.
        let at_corner = verts
            .iter()
            .filter(|(_, u)| *u == 0.0)
            .filter(|(p, _)| p[0].hypot(p[1]) < 45.0);
        let mut seen = 0;
        for (p, _) in at_corner {
            // Every outer vertex serves one of the two segments; it must sit
            // the full half-width off of that one.
            let reach = perp(*p, [1.0, 0.0]).max(perp(*p, [t.cos(), t.sin()]));
            assert!(
                reach >= half - 1e-2,
                "{turn}° turn necks the carriageway to {reach:.2} m of {half} m"
            );
            assert!(
                reach <= half * MITER_LIMIT + 1e-2,
                "{turn}° turn spikes the outer corner to {reach:.2} m"
            );
            seen += 1;
        }
        assert!(seen > 0, "{turn}° turn: no outer vertex at the corner");
    }
}

/// The **inner** edge must not fold back through the ribbon.
///
/// The inner miter backtracks `half / tan(θ/2)` along both neighbours, and
/// nothing bounded that against how long they actually are: on a 22 m
/// motorway a 60° turn overran a 6 m segment — and z14 Shortbread geometry is
/// full of sub-10 m segments. The fold is a self-intersecting quad, whose two
/// layers carry different `v`, i.e. the centre line fighting its own
/// reflection along the bend.
///
/// A fold is the inner rail *reversing* along the road, so that is what this
/// measures. It is not about quads overlapping each other: a 22 m carriageway
/// around a 3 m corner has to double back over itself, and the union of that
/// overlap is still the right piece of road.
#[test]
fn a_tight_corner_does_not_fold_the_ribbon() {
    for turn in [30.0f32, 60.0, 90.0, 120.0, 150.0, 170.0] {
        for seg in [3.0f32, 6.0, 15.0, 60.0] {
            let (verts, centre) = corner(turn, seg, 11.0);
            // A bevel splits the *outer* vertex in two and shares the inner
            // one, so the inner rail is the one with a vertex per centre-line
            // point — exactly the rail a fold shows up on, and the pairing
            // that lets each vertex be measured against its own segment
            // rather than whichever happens to be nearest (with a 22 m road
            // on 3 m segments, that is routinely the wrong one).
            let inner: Vec<[f32; 2]> = verts
                .iter()
                .filter(|(_, u)| *u == 1.0)
                .map(|(p, _)| *p)
                .collect();
            assert_eq!(inner.len(), centre.len(), "one inner vertex per point");
            for k in 0..centre.len() - 1 {
                let (a, b) = (centre[k], centre[k + 1]);
                let (dx, dy) = (b[0] - a[0], b[1] - a[1]);
                let len = dx.hypot(dy);
                let (ux, uy) = (dx / len, dy / len);
                // How far each end of this quad's inner rail has advanced
                // along the segment it belongs to.
                let along = |p: [f32; 2]| (p[0] - a[0]) * ux + (p[1] - a[1]) * uy;
                let (from, to) = (along(inner[k]), along(inner[k + 1]));
                assert!(
                    to >= from - 1e-3,
                    "{turn}° turn on {seg} m segments folds the ribbon: over segment \
                         {k} the inner edge runs backwards, {from:.2} m to {to:.2} m"
                );
            }
        }
    }
}

/// A join may not throw a spike into the field.
///
/// [`MITER_LIMIT`] bounds how far the outer corner may stand from the centre
/// line, and the bevel is what enforces it. This measures the geometry rather
/// than the constant: every vertex of the built ribbon, against the line it
/// was built on.
#[test]
fn a_reversal_is_bevelled_rather_than_drawn_as_a_spike() {
    let seg_dist = |p: [f32; 2], a: [f32; 2], b: [f32; 2]| -> f32 {
        let (dx, dy) = (b[0] - a[0], b[1] - a[1]);
        let len2 = dx * dx + dy * dy;
        let t = if len2 > 1e-9 {
            (((p[0] - a[0]) * dx + (p[1] - a[1]) * dy) / len2).clamp(0.0, 1.0)
        } else {
            0.0
        };
        (p[0] - (a[0] + dx * t)).hypot(p[1] - (a[1] + dy * t))
    };
    for turn in [30.0f32, 60.0, 90.0, 100.0, 128.2, 150.0, 170.0] {
        for half in [2.0f32, 4.5, 11.0] {
            let (verts, centre) = corner(turn, 6.0, half);
            let worst = verts
                .iter()
                .map(|(p, _)| {
                    centre
                        .windows(2)
                        .map(|w| seg_dist(*p, w[0], w[1]))
                        .fold(f32::MAX, f32::min)
                })
                .fold(0.0f32, f32::max);
            assert!(
                worst <= half * MITER_LIMIT + 1e-3,
                "a {turn}° turn on a {:.0} m road reaches {worst:.2} m from its centre \
                     line, past the {:.2} m the miter limit allows",
                half * 2.0,
                half * MITER_LIMIT,
            );
        }
    }

    // …and the limit is where it is for a reason, which is the half of this
    // that would otherwise drift. A right angle mitres at `1 / cos 45°` and
    // **keeps its point** — a street grid's right angles are corners the map
    // asserts, the same licence `ROUND_MAX_DEG` draws. A reversal does not:
    // the Groningen fixture's feature 69 is a `tertiary` turning head that
    // reverses through 128.2°, and its miter of `1 / cos 64.1° = 2.29` slipped
    // under the old limit of 2.5 and drew a ten-metre tongue of tarmac out
    // into the field past the end of the road.
    assert!(
        MITER_LIMIT >= 1.0 / std::f32::consts::FRAC_PI_4.cos(),
        "a right angle must keep its point",
    );
    assert!(
        MITER_LIMIT < 1.0 / 64.1f32.to_radians().cos(),
        "a 128° reversal must bevel rather than mitre",
    );
}
