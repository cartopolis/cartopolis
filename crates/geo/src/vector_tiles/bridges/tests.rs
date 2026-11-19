
use super::*;
use crate::vector_tiles::bytes::{roads_and_decks, tessellate_bridges};

// A real z14 tile over Groningen, from the VersaTiles Shortbread source
// (now self-hosted at tiles.cartopolis.org; the archive is identical).
use crate::testdata::GRONINGEN_Z14;

fn rect_deck(ring: &[[f32; 2]]) -> BridgeDeck {
    let mut min = [f32::MAX, f32::MAX];
    let mut max = [f32::MIN, f32::MIN];
    for p in ring {
        min = [min[0].min(p[0]), min[1].min(p[1])];
        max = [max[0].max(p[0]), max[1].max(p[1])];
    }
    let mut closed = ring.to_vec();
    closed.push(ring[0]);
    BridgeDeck {
        positions: Vec::new(),
        indices: Vec::new(),
        rings: vec![closed],
        min,
        max,
    }
}

fn rect_road(x0: f32, z0: f32, x1: f32, z1: f32) -> SurfaceMesh {
    SurfaceMesh {
        group: SurfaceGroup::Road(RoadClass::Minor),
        positions: vec![[x0, 0.0, z0], [x1, 0.0, z0], [x1, 0.0, z1], [x0, 0.0, z1]],
        indices: vec![0, 1, 2, 0, 2, 3],
        uvs: Vec::new(),
        normals: Vec::new(),
        colors: Vec::new(),
        rungs: Vec::new(),
    }
}

/// **A canal bridge's railings run down its sides and stop at its ends.** A
/// 30 × 12 m deck carrying a 6 m road end to end: two runs, each the whole
/// length of a long side, and nothing across the road at either end.
#[test]
fn railings_run_down_the_sides_not_across_the_road() {
    let deck = rect_deck(&[
        [100.0, 100.0],
        [130.0, 100.0],
        [130.0, 112.0],
        [100.0, 112.0],
    ]);
    let road = rect_road(100.0, 103.0, 130.0, 109.0);
    let runs = railing_runs(&deck, &[&road], &[], 1465.0);
    assert_eq!(runs.len(), 2, "{runs:?}");
    for r in &runs {
        let len = crate::tunnels::polyline_len(r);
        assert!((len - 30.0).abs() < 1e-3, "a side is {len} m: {r:?}");
        assert!(
            r.iter()
                .all(|p| (p[1] - 100.0).abs() < 1e-3 || (p[1] - 112.0).abs() < 1e-3),
            "a run crosses an end: {r:?}"
        );
    }
}

/// **A deck spanning a canal whose register stops at its outline takes the
/// canal's level under it** — the Oosterbrug's shape: the canal runs along x,
/// the deck crosses it, water beyond both its sides and land beyond its ends.
/// Not between two levels (a lock), and not where water lies beside one side.
#[test]
fn a_deck_across_a_canal_spans_its_water() {
    let deck = rect_deck(&[
        [100.0, 100.0],
        [112.0, 100.0],
        [112.0, 130.0],
        [100.0, 130.0],
    ]);
    let canal = |p: [f32; 2]| (p[1] > 100.0 && p[1] < 130.0).then_some(0.8);
    assert_eq!(spanned_water(&deck, canal), Some(0.8));
    let lock = |p: [f32; 2]| {
        (p[1] > 100.0 && p[1] < 130.0).then_some(if p[0] < 106.0 { 0.8 } else { 2.0 })
    };
    assert_eq!(spanned_water(&deck, lock), None);
    let beside = |p: [f32; 2]| (p[0] > 112.0).then_some(0.8);
    assert_eq!(spanned_water(&deck, beside), None);
    assert_eq!(spanned_water(&deck, |_| None), None);
}

/// **The abutments are the ends the railings leave open**, and each stands as
/// a wall facing both ways from the deck down.
#[test]
fn a_deck_stands_on_an_abutment_at_each_end() {
    let deck = rect_deck(&[
        [100.0, 100.0],
        [130.0, 100.0],
        [130.0, 112.0],
        [100.0, 112.0],
    ]);
    let road = rect_road(100.0, 103.0, 130.0, 109.0);
    let ends = abutment_runs(&deck, &[&road], &[], 1465.0);
    assert_eq!(ends.len(), 2, "{ends:?}");
    for e in &ends {
        assert!(
            e.iter()
                .all(|p| (p[0] - 100.0).abs() < 1e-3 || (p[0] - 130.0).abs() < 1e-3),
            "{e:?}"
        );
        assert!(
            (crate::tunnels::polyline_len(e) - 12.0).abs() < 1e-3,
            "{e:?}"
        );
        let wall = abutment_wall_mesh(&deck, e);
        assert_eq!(wall.group, SurfaceGroup::QuayWall);
        // One segment, one face of two triangles, facing in under the deck.
        assert_eq!(wall.indices.len(), 6);
        assert!(wall
            .positions
            .iter()
            .all(|p| p[1] <= 0.0 && p[1] >= -ABUTMENT_WALL_DEPTH_M));
        let inward = if (e[0][0] - 100.0).abs() < 1e-3 {
            1.0
        } else {
            -1.0
        };
        assert!(
            wall.normals.iter().all(|v| v[0] * inward > 0.9),
            "{:?}",
            wall.normals
        );
    }
}

/// **An end over water is carried out to its quay**, a probe onto the land,
/// and no further than the reach; an end on land stays where it is.
#[test]
fn an_end_short_of_its_quay_reaches_it() {
    let deck = rect_deck(&[
        [100.0, 100.0],
        [130.0, 100.0],
        [130.0, 112.0],
        [100.0, 112.0],
    ]);
    let east = [[130.0, 100.0], [130.0, 112.0]];
    // Water for a metre and a half past the east end, then the quay.
    let water = |p: [f32; 2]| p[0] > 130.0 && p[0] < 131.5;
    let out = reach_the_quay(&deck, &east, &water);
    for p in &out {
        assert!((p[0] - 131.5).abs() <= 0.26, "{out:?}");
    }
    let lip = quay_lip_mesh(&east, &out).expect("a lip");
    assert_eq!(lip.group, SurfaceGroup::BridgeDeck);
    assert_eq!(lip.indices.len(), 6);
    // An open canal past it is crossed no further than the reach.
    let sea = |p: [f32; 2]| p[0] > 130.0;
    let far = reach_the_quay(&deck, &east, &sea);
    assert!(
        far.iter()
            .all(|p| p[0] <= 130.0 + ABUTMENT_REACH_M + ABUTMENT_PROBE_M + 1e-3),
        "{far:?}"
    );
    // An end already on its quay stays put, and makes no lip.
    let dry = |_: [f32; 2]| false;
    let same = reach_the_quay(&deck, &east, &dry);
    assert_eq!(same, east.to_vec());
    assert!(quay_lip_mesh(&east, &same).is_none());
}

/// The pavement either side of the road at an end is part of that end: an
/// outline whose end is three edges (verge, carriageway, verge) gets no
/// railing across any of them.
#[test]
fn an_end_split_into_several_edges_stays_open() {
    let deck = rect_deck(&[
        [100.0, 100.0],
        [130.0, 100.0],
        [130.0, 103.0],
        [130.0, 109.0],
        [130.0, 112.0],
        [100.0, 112.0],
        [100.0, 109.0],
        [100.0, 103.0],
    ]);
    let road = rect_road(100.0, 103.0, 130.0, 109.0);
    let runs = railing_runs(&deck, &[&road], &[], 1465.0);
    let total: f32 = runs.iter().map(|r| crate::tunnels::polyline_len(r)).sum();
    assert!((total - 60.0).abs() < 1e-3, "railing {total} m: {runs:?}");
}

/// **A bridge way that stops short of its outline still opens the ends.** The
/// Oosterbrug's road ends a few metres inside the deck on both sides, and the
/// deck was railed shut all round (2026-10-19); the street running on off
/// it — the approach — is what crosses the end.
#[test]
fn an_approach_crossing_an_end_opens_it() {
    let deck = rect_deck(&[
        [100.0, 100.0],
        [124.0, 100.0],
        [124.0, 122.0],
        [100.0, 122.0],
    ]);
    // The carriageway stops 3 m short of both ends.
    let road = rect_road(103.0, 106.0, 121.0, 116.0);
    let west: &[[f32; 2]] = &[[103.0, 111.0], [80.0, 111.0]];
    let east: &[[f32; 2]] = &[[121.0, 111.0], [150.0, 111.0]];
    let runs = railing_runs(&deck, &[&road], &[west, east], 1465.0);
    assert_eq!(runs.len(), 2, "{runs:?}");
    for r in &runs {
        assert!(
            r.iter()
                .all(|p| (p[1] - 100.0).abs() < 1e-3 || (p[1] - 122.0).abs() < 1e-3),
            "a railing closes an end: {r:?}"
        );
    }
}

/// **A footbridge whose path the survey draws is open where the path leaves
/// it, and railed past its side platforms.** The H.N. Werkmanbrug's shape:
/// a 4.5 × 37 m deck with a platform stepping out 1.9 m from each side, 8 m
/// in from the south end. No carriageway is laid on it; the ends of the
/// path's stretch over it stand in for the approaches (2026-11-05).
#[test]
fn a_surveyed_path_opens_its_ends_and_platforms_stay_railed() {
    let deck = rect_deck(&[
        [100.0, 100.0],
        [104.5, 100.0],
        [104.5, 107.9],
        [106.4, 107.9],
        [106.4, 115.9],
        [104.5, 115.9],
        [104.5, 137.0],
        [100.0, 137.0],
        [100.0, 115.9],
        [98.1, 115.9],
        [98.1, 107.9],
        [100.0, 107.9],
    ]);
    let path = [[102.2, 100.0], [102.2, 136.9]];
    let ends: [&[[f32; 2]]; 2] = [&path[..1], &path[1..]];
    let runs = railing_runs(&deck, &[], &ends, 1465.0);
    // One unbroken run down each side, end to end — a platform face opened
    // as part of the end splits its side in two — and neither end crossed.
    assert_eq!(runs.len(), 2, "{runs:?}");
    for r in &runs {
        let (lo, hi) = r.iter().fold((f32::MAX, f32::MIN), |(lo, hi), p| {
            (lo.min(p[1]), hi.max(p[1]))
        });
        assert!(lo <= 100.0 + 1e-3 && hi >= 137.0 - 1e-3, "{r:?}");
        assert!(
            r.windows(2).all(|w| !((w[0][1] - 100.0).abs() < 1e-3
                && (w[1][1] - 100.0).abs() < 1e-3)
                && !((w[0][1] - 137.0).abs() < 1e-3 && (w[1][1] - 137.0).abs() < 1e-3)),
            "a railing closes an end: {r:?}"
        );
    }
}

/// **A notch in the outline is not a break in the railing.** The Oosterbrug's
/// sides step in about two and a half metres where the bascule's mechanism
/// stands; the railing runs straight past, and keeps its real corners.
#[test]
fn a_railing_runs_straight_across_a_shallow_notch() {
    // The Oosterbrug's south side as logged, tile-local metres.
    let side = [
        [253.0, 1345.0],
        [247.0, 1338.0],
        [247.0, 1339.0],
        [245.0, 1339.0],
        [244.0, 1338.0],
        [245.0, 1337.0],
        [240.0, 1329.0],
    ];
    let s = skip_notches(&side);
    for notch in [[247.0, 1339.0], [245.0, 1339.0], [244.0, 1338.0]] {
        assert!(
            !s.contains(&notch),
            "the railing goes into the notch: {s:?}"
        );
    }
    assert_eq!((s[0], s[s.len() - 1]), (side[0], side[side.len() - 1]));
    // The north side, whose notch starts 8.6 m along it: no step left.
    let north = [
        [256.0, 1315.0],
        [261.0, 1322.0],
        [262.0, 1322.0],
        [263.0, 1324.0],
        [263.0, 1325.0],
        [262.0, 1326.0],
        [267.0, 1334.0],
    ];
    let s = skip_notches(&north);
    // Every turn the railing makes along it is a few degrees at most: the
    // kink into the notch's first vertex was 16°, and read as a step.
    for w in s.windows(3) {
        let (u, v) = (
            [w[1][0] - w[0][0], w[1][1] - w[0][1]],
            [w[2][0] - w[1][0], w[2][1] - w[1][1]],
        );
        let cos = (u[0] * v[0] + u[1] * v[1]) / (u[0].hypot(u[1]) * v[0].hypot(v[1]));
        assert!(cos > 0.99, "a {}° kink: {s:?}", cos.acos().to_degrees());
    }
    let corner = [[0.0, 0.0], [10.0, 0.0], [10.0, 10.0]];
    assert_eq!(skip_notches(&corner), corner.to_vec());
    // A gentle curve, 2 m vertices round a 20 m radius, stays within
    // half a metre of itself.
    let arc: Vec<[f32; 2]> = (0..16)
        .map(|k| {
            let t = k as f32 * 0.1;
            [20.0 * t.cos(), 20.0 * t.sin()]
        })
        .collect();
    for p in &arc {
        let s = skip_notches(&arc);
        let off = s
            .windows(2)
            .map(|w| segment_distance(*p, w[0], w[1]))
            .fold(f32::MAX, f32::min);
        assert!(off < 0.5, "{p:?} is {off} m off the railing");
    }
}

/// A street along the quay that only touches a corner of the deck does not
/// open the side it touches.
#[test]
fn a_road_grazing_a_corner_leaves_the_side_railed() {
    let deck = rect_deck(&[
        [100.0, 100.0],
        [130.0, 100.0],
        [130.0, 112.0],
        [100.0, 112.0],
    ]);
    let road = rect_road(100.0, 103.0, 130.0, 109.0);
    // A quay street running across the deck's west end, reaching 0.5 m onto
    // both long sides at their corners.
    let quay = rect_road(94.0, 90.0, 100.5, 122.0);
    let runs = railing_runs(&deck, &[&road, &quay], &[], 1465.0);
    let total: f32 = runs.iter().map(|r| crate::tunnels::polyline_len(r)).sum();
    assert!(total > 55.0, "railing {total} m: {runs:?}");
}

/// A deck no road reaches is railed down its long sides, and not at all if
/// it is about square: enclosing it would wall off a road the tiles did not
/// draw onto it.
#[test]
fn a_deck_no_road_reaches_is_railed_down_its_long_sides_only() {
    let long = rect_deck(&[
        [100.0, 100.0],
        [130.0, 100.0],
        [130.0, 106.0],
        [100.0, 106.0],
    ]);
    let runs = railing_runs(&long, &[], &[], 1465.0);
    assert_eq!(runs.len(), 2, "{runs:?}");
    assert!(
        runs.iter()
            .all(|r| (crate::tunnels::polyline_len(r) - 30.0).abs() < 1e-3),
        "{runs:?}"
    );
    let square = rect_deck(&[
        [100.0, 100.0],
        [120.0, 100.0],
        [120.0, 118.0],
        [100.0, 118.0],
    ]);
    assert!(railing_runs(&square, &[], &[], 1465.0).is_empty());
}

/// Where the tile border cuts a deck, the deck goes on next door: no railing
/// along the cut.
#[test]
fn no_railing_along_a_tile_border() {
    let tile_m = 1465.0;
    let deck = rect_deck(&[
        [tile_m - 20.0, 100.0],
        [tile_m, 100.0],
        [tile_m, 112.0],
        [tile_m - 20.0, 112.0],
    ]);
    let road = rect_road(tile_m - 20.0, 103.0, tile_m, 109.0);
    let runs = railing_runs(&deck, &[&road], &[], tile_m);
    assert_eq!(runs.len(), 2, "{runs:?}");
    assert!(runs.iter().flatten().all(|p| p[1] != 105.0));
    let total: f32 = runs.iter().map(|r| crate::tunnels::polyline_len(r)).sum();
    assert!((total - 40.0).abs() < 1e-3, "railing {total} m: {runs:?}");
}

/// A railing gives way where `keep` says, and keeps its corners elsewhere.
#[test]
fn cut_runs_removes_a_stretch_and_keeps_corners() {
    let run = vec![[0.0, 0.0], [10.0, 0.0], [10.0, 10.0]];
    let cut = cut_runs(vec![run], |p, _| !(p[0] > 3.9 && p[0] < 6.1 && p[1] < 0.5));
    assert_eq!(cut.len(), 2, "{cut:?}");
    assert!(
        (cut[0][0][0]).abs() < 1e-4 && (cut[0][cut[0].len() - 1][0] - 3.5).abs() < 1e-4,
        "{cut:?}"
    );
    // The second piece turns the corner at (10, 0).
    assert!(
        cut[1]
            .iter()
            .any(|p| (p[0] - 10.0).abs() < 1e-4 && p[1].abs() < 1e-4),
        "{cut:?}"
    );
    assert!((cut[1][cut[1].len() - 1][1] - 10.0).abs() < 1e-4, "{cut:?}");
    assert_eq!(cut[1].len(), 3, "samples along a segment collapse: {cut:?}");
}

/// Over the fixture tile every deck the tiles draw a road onto gets a
/// railing, and none of it lies on the road.
#[test]
fn every_fixture_deck_with_a_road_gets_a_railing_beside_it() {
    let tile_m = 1465.3;
    let decks = tessellate_bridges(GRONINGEN_Z14, tile_m).expect("bridges");
    let (roads, decks) = roads_and_decks(GRONINGEN_Z14, tile_m, &decks).expect("roads");
    let mut railed = 0;
    let mut with_road = 0;
    for (i, d) in decks.iter().enumerate() {
        let ways: Vec<&SurfaceMesh> = roads
            .on_deck
            .iter()
            .filter(|(di, _)| *di as usize == i)
            .map(|(_, m)| m)
            .collect();
        if ways.is_empty() {
            continue;
        }
        with_road += 1;
        let runs = railing_runs(d, &ways, &[], tile_m);
        if !runs.is_empty() {
            railed += 1;
        }
    }
    assert!(with_road > 0);
    assert!(
        railed * 10 >= with_road * 8,
        "{railed} of {with_road} decks with a road got a railing"
    );
}

/// **A bridge OSM maps only as flagged ways still gets a deck.** The ring
/// fixture's Europaplein flyover is four `bridge=yes` trunk carriageways with
/// no `bridges` outline, and was drawn on the ground across the junction it
/// crosses (reported 2026-09-26). Every bridge-flagged road here must now
/// stand on a deck along its whole length, and the flyover's must carry its
/// carriageway and have an approach to ramp.
#[test]
fn a_bridge_mapped_only_as_ways_gets_a_deck() {
    let tile = crate::testdata::groningen_ring();
    let tile_m = 1465.3;
    let areas = super::area_decks(&tile, tile_m);
    let mut decks = super::tessellate_bridges(&tile, tile_m).expect("bridges");
    assert!(
        decks.len() > areas.len(),
        "no deck was built from a street: {} decks, {} outlines",
        decks.len(),
        areas.len()
    );

    let meta = tile.layer("streets").expect("streets");
    let extent = TileData::extent(meta);
    let scale = tile_m / extent;
    let mut checked = 0;
    for f in tile.features(meta) {
        if !flag_of(&f, "bridge")
            || !drawn_street(&f)
                .is_some_and(|(c, _)| !matches!(c, RoadClass::Path | RoadClass::Minor))
        {
            continue;
        }
        let Geometry::LineString(ls) = &f.geometry else {
            continue;
        };
        let pts: Vec<[f32; 2]> = ls.0.iter().map(|c| [c.x, c.y]).collect();
        for run in clip_polyline(&pts, 0.0, extent) {
            let m: Vec<[f32; 2]> = run.iter().map(|p| [p[0] * scale, p[1] * scale]).collect();
            if crate::tunnels::polyline_len(&m) < STREET_DECK_MIN_M {
                continue;
            }
            // Every metre of it, bar the ends: a way may overhang an outline by
            // up to `STREET_DECK_MIN_M`, and that sliver is left on the ground.
            for s in m.windows(2) {
                let (a, b) = (s[0], s[1]);
                let l = (b[0] - a[0]).hypot(b[1] - a[1]);
                let steps = l.floor() as usize;
                for k in 1..steps {
                    let t = k as f32 / l;
                    let p = [a[0] + (b[0] - a[0]) * t, a[1] + (b[1] - a[1]) * t];
                    let near_end = (p[0] - m[0][0]).hypot(p[1] - m[0][1]) < STREET_DECK_MIN_M
                        || (p[0] - m[m.len() - 1][0]).hypot(p[1] - m[m.len() - 1][1])
                            < STREET_DECK_MIN_M;
                    assert!(
                        near_end || decks.iter().any(|d| d.contains(p)),
                        "a bridge street is off every deck at {p:?}"
                    );
                }
            }
            checked += 1;
        }
    }
    assert!(checked >= 4, "only {checked} bridge streets checked");

    // Europaplein, between the two main carriageways' spans.
    let europaplein = [1247.9, 125.7];
    let di = decks
        .iter()
        .position(|d| d.contains(europaplein))
        .expect("no deck at the Europaplein flyover");
    assert!(
        di >= areas.len(),
        "the flyover's deck is an outline after all"
    );
    // A clean outline: no edge that runs out and straight back. Those are the
    // zero-width spikes a union of abutting rectangles leaves, and the fascia
    // is extruded from every one of them.
    for d in &decks[areas.len()..] {
        for ring in &d.rings {
            let pts = &ring[..ring.len() - 1];
            for k in 0..pts.len() {
                let (a, b, c) = (pts[k], pts[(k + 1) % pts.len()], pts[(k + 2) % pts.len()]);
                let (u, v) = ([b[0] - a[0], b[1] - a[1]], [c[0] - b[0], c[1] - b[1]]);
                let back = u[0] * v[0] + u[1] * v[1] < 0.0
                    && (u[0] * v[1] - u[1] * v[0]).abs()
                        < 0.01 * u[0].hypot(u[1]) * v[0].hypot(v[1]);
                assert!(!back, "a deck's outline doubles back on itself at {b:?}");
            }
        }
    }
    let roads = super::tessellate_roads(&tile, tile_m, &mut decks, None, false).expect("roads");
    assert!(
        roads.on_deck.iter().any(|(d, _)| *d as usize == di),
        "no carriageway rides the flyover"
    );
    // Both of the N7's own carriageways ride it, not only the slip lane.
    // The midpoints of the westbound and eastbound spans, tile-local.
    let c = [europaplein[0] + 6.4, europaplein[1] + 3.7];
    for (name, mid) in [
        ("westbound", [c[0] - 6.0, c[1] - 3.5]),
        ("eastbound", [c[0] + 2.0, c[1] + 5.5]),
    ] {
        let carried = roads
            .on_deck
            .iter()
            .filter(|(d, _)| *d as usize == di)
            .any(|(_, m)| {
                m.indices.chunks(3).any(|t| {
                    let p = [0, 1, 2].map(|k| m.positions[t[k] as usize]);
                    let tri = [[p[0][0], p[0][2]], [p[1][0], p[1][2]], [p[2][0], p[2][2]]];
                    let side = |a: [f32; 2], b: [f32; 2]| {
                        (b[0] - a[0]) * (mid[1] - a[1]) - (b[1] - a[1]) * (mid[0] - a[0])
                    };
                    let (s0, s1, s2) = (
                        side(tri[0], tri[1]),
                        side(tri[1], tri[2]),
                        side(tri[2], tri[0]),
                    );
                    (s0 >= 0.0 && s1 >= 0.0 && s2 >= 0.0) || (s0 <= 0.0 && s1 <= 0.0 && s2 <= 0.0)
                })
            });
        assert!(
            carried,
            "the N7 {name} runs under its own deck instead of on it"
        );
    }
    let ramp = roads
        .approaches
        .iter()
        .filter(|a| a.deck as usize == di)
        .map(|a| crate::tunnels::polyline_len(&a.points))
        .fold(0.0f32, f32::max);
    assert!(
        ramp >= 40.0,
        "the flyover's longest approach is {ramp:.1} m"
    );
}

/// **A bridge's own walls are not drawn standing on the road under it.** The
/// Europaplein viaduct's screens measured 7–9.5 m from the Europaweg to their
/// top and its parapets were unmeasured 48 m walls along the deck; both stood
/// on the road the deck crosses (2026-09-26). A measured metre on a deck is a
/// canal bridge's parapet and stays, and so does a low wall beside a ramp.
#[test]
fn a_bridge_keeps_its_walls_off_the_road_beneath() {
    // A 60 × 30 m deck along x, and a ramp running on east from it.
    let ring = vec![
        [0.0, 0.0],
        [60.0, 0.0],
        [60.0, 30.0],
        [0.0, 30.0],
        [0.0, 0.0],
    ];
    let deck = BridgeDeck {
        positions: Vec::new(),
        indices: Vec::new(),
        rings: vec![ring],
        min: [0.0, 0.0],
        max: [60.0, 30.0],
    };
    let decks = [deck];
    let ramp = [Approach {
        deck: 0,
        points: vec![[60.0, 15.0], [220.0, 15.0]],
    }];
    let along_deck = [[0.0, 1.0], [60.0, 1.0]];
    let along_ramp = [[40.0, 5.0], [220.0, 5.0]];
    let elsewhere = [[0.0, 80.0], [60.0, 80.0]];
    // Beside the deck, past the drawn carriageway: a wide viaduct's parapet.
    let beside_deck = [[0.0, -9.0], [60.0, -9.0]];

    assert!(wall_of_a_bridge(&decks, &ramp, &along_deck, None));
    assert!(wall_of_a_bridge(&decks, &ramp, &along_deck, Some(9.5)));
    assert!(wall_of_a_bridge(&decks, &ramp, &beside_deck, None));
    assert!(
        !wall_of_a_bridge(&decks, &ramp, &along_deck, Some(1.0)),
        "a measured parapet on a deck stays"
    );
    assert!(wall_of_a_bridge(&decks, &ramp, &along_ramp, Some(7.0)));
    assert!(
        wall_of_a_bridge(&decks, &ramp, &along_ramp, None),
        "an unmeasured parapet running on past the bridge way is the bridge's"
    );
    assert!(!wall_of_a_bridge(&decks, &ramp, &along_ramp, Some(1.2)));
    assert!(wall_of_a_bridge(&decks, &ramp, &along_ramp, Some(2.4)));
    assert!(!wall_of_a_bridge(&decks, &ramp, &elsewhere, Some(9.0)));
    assert!(!wall_of_a_bridge(&[], &[], &along_deck, None));
}

/// The bridge-flagged streets, decks and approaches of one tile within
/// `CARTO_BRIDGE_NEAR` metres (default 150) of `CARTO_BRIDGE_AT=lat,lng`, in
/// tile-local metres — for an approach step `carto-view --site` reports, which
/// says where it probed but not what the tile holds there.
///
/// ```text
/// CARTO_BRIDGE_TILE=/tmp/t.mvt CARTO_BRIDGE_XYZ=14/8489/5321 CARTO_BRIDGE_AT=53.2126,6.5422 \
///     cargo test -p cartopolis_geo dump_the_bridges_near -- --ignored --nocapture
/// ```
#[test]
#[ignore = "a diagnostic: needs CARTO_BRIDGE_TILE, CARTO_BRIDGE_XYZ and CARTO_BRIDGE_AT"]
fn dump_the_bridges_near() {
    let bytes = std::fs::read(std::env::var("CARTO_BRIDGE_TILE").expect("tile")).expect("reads");
    let tile = TileData::parse(&bytes).expect("parses");
    let v: Vec<f64> = std::env::var("CARTO_BRIDGE_XYZ")
        .expect("xyz")
        .split('/')
        .filter_map(|p| p.parse().ok())
        .collect();
    let (z, x, y) = (v[0], v[1], v[2]);
    let at: Vec<f64> = std::env::var("CARTO_BRIDGE_AT")
        .expect("at")
        .split(',')
        .filter_map(|p| p.parse().ok())
        .collect();
    let near: f32 = std::env::var("CARTO_BRIDGE_NEAR")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(150.0);
    let n = 2f64.powf(z);
    let lat0 = (std::f64::consts::PI * (1.0 - 2.0 * y / n)).sinh().atan();
    let tile_m = (40_075_016.686 * lat0.cos() / n) as f32;
    let fx = (at[1] + 180.0) / 360.0 * n;
    let fy = (1.0
        - (at[0].to_radians().tan() + 1.0 / at[0].to_radians().cos()).ln() / std::f64::consts::PI)
        / 2.0
        * n;
    let p = [
        ((fx - x) * f64::from(tile_m)) as f32,
        ((fy - y) * f64::from(tile_m)) as f32,
    ];
    let close = |q: &[f32; 2]| (q[0] - p[0]).hypot(q[1] - p[1]) <= near;
    println!("tile_m={tile_m:.1} point={p:?}");
    let mut decks = super::tessellate_bridges(&tile, tile_m).expect("bridges");
    for (i, d) in decks.iter().enumerate() {
        if d.rings.iter().flatten().any(close) {
            println!("deck {i}: min {:?} max {:?}", d.min, d.max);
        }
    }
    for l in crate::tunnels::surface_lines(&tile, tile_m) {
        if l.bridge && l.pts.iter().any(close) {
            let f = |q: [f32; 2]| [q[0].round(), q[1].round()];
            println!(
                "bridge way: {:?} (own {:?}) .. {:?} (own {:?}), {} points",
                f(l.pts[0]),
                l.own[0],
                f(*l.pts.last().unwrap()),
                l.own[1],
                l.pts.len()
            );
        }
    }
    let roads = super::tessellate_roads(&tile, tile_m, &mut decks, None, false).expect("roads");
    for a in &roads.approaches {
        if a.points.iter().any(close) {
            let f = |q: [f32; 2]| [q[0].round(), q[1].round()];
            println!(
                "approach of deck {}: {:?} .. {:?}, {:.0} m",
                a.deck,
                f(a.points[0]),
                f(*a.points.last().unwrap()),
                crate::tunnels::polyline_len(&a.points)
            );
        }
    }
}

/// Where every bridge deck in a tile is, **on Earth**.
///
/// The sibling of `tunnels::tests::dump_the_fixture_bores`, and it exists for
/// the same reason: `docs/grade-shots.toml`'s own header records that half
/// its first bridge cameras missed, one of them photographing the city centre
/// with the viaduct behind it, and re-aiming them by hand missed again on
/// 2026-09-22. A deck's position is a thing the tiles know exactly, so it
/// should be printed rather than estimated off a map.
///
/// ```text
/// CARTO_BRIDGE_TILE=/tmp/t.mvt CARTO_BRIDGE_XYZ=14/8490/5321 \
///     cargo test -p cartopolis_geo dump_the_bridge_decks -- --ignored --nocapture
/// ```
#[test]
#[ignore = "diagnostic: prints where the tile's viaducts are"]
fn dump_the_bridge_decks() {
    let owned = std::env::var("CARTO_BRIDGE_TILE")
        .ok()
        .map(|p| std::fs::read(p).expect("tile reads"));
    let tile = match &owned {
        Some(b) => TileData::parse(b).expect("tile parses"),
        None => crate::testdata::groningen(),
    };
    let key: Option<(f64, f64, f64)> = std::env::var("CARTO_BRIDGE_XYZ").ok().and_then(|s| {
        let v: Vec<f64> = s.split('/').filter_map(|p| p.parse().ok()).collect();
        (v.len() == 3).then(|| (v[0], v[1], v[2]))
    });
    // The tile's own ground size, derived from its coordinates rather than
    // written down — the same care the tunnel dump takes.
    let tile_m = key.map_or(1471.0, |(z, _, y)| {
        let n = 2f64.powf(z);
        let lat0 = (std::f64::consts::PI * (1.0 - 2.0 * y / n)).sinh().atan();
        (40_075_016.686 * lat0.cos() / n) as f32
    });
    let decks = super::tessellate_bridges(&tile, tile_m).expect("bridges tessellate");
    println!("decks={}", decks.len());
    let mut sorted: Vec<&BridgeDeck> = decks.iter().collect();
    // Biggest first: a viaduct worth photographing is a large structure, and a
    // tile is mostly twenty-metre canal bridges.
    sorted.sort_by(|a, b| {
        let area = |d: &BridgeDeck| (d.max[0] - d.min[0]) * (d.max[1] - d.min[1]);
        area(b).total_cmp(&area(a))
    });
    for (i, d) in sorted.iter().take(8).enumerate() {
        let (cx, cz) = ((d.min[0] + d.max[0]) * 0.5, (d.min[1] + d.max[1]) * 0.5);
        let here = key.map_or(String::new(), |(z, x, y)| {
            let n = 2f64.powf(z);
            let lat0 = (std::f64::consts::PI * (1.0 - 2.0 * y / n)).sinh().atan();
            let span = 40_075_016.686 * lat0.cos() / n;
            let (fx, fy) = (x + f64::from(cx) / span, y + f64::from(cz) / span);
            let lat = (std::f64::consts::PI * (1.0 - 2.0 * fy / n))
                .sinh()
                .atan()
                .to_degrees();
            format!("  at {lat:.6},{:.6}", fx / n * 360.0 - 180.0)
        });
        println!(
            "deck {i}: {:.0}x{:.0} m{here}",
            d.max[0] - d.min[0],
            d.max[1] - d.min[1]
        );
    }
}
/// The `bridges` layer is areas, and those areas are the deck a raised bridge
/// stands on. Nothing drew them before this, which is why a viaduct was only
/// ever the road ribbon crossing it.
#[test]
fn bridges_tessellate_into_decks_with_outlines() {
    let decks = tessellate_bridges(GRONINGEN_Z14, 1000.0).expect("bridges");
    assert!(!decks.is_empty(), "a city tile should hold bridge decks");
    for d in &decks {
        assert_eq!(d.indices.len() % 3, 0);
        assert!(!d.rings.is_empty(), "a deck with no outline has no fascia");
        assert!(
            d.indices.iter().all(|i| (*i as usize) < d.positions.len()),
            "index out of range"
        );
        // Inside the tile, flat, and a plausible size for a structure.
        for p in &d.positions {
            assert!((0.0..=1000.0).contains(&p[0]) && (0.0..=1000.0).contains(&p[2]));
            assert_eq!(
                p[1], 0.0,
                "a deck leaves the worker flat; height is measured"
            );
        }
        let span = d.span_m();
        assert!((1.0..=600.0).contains(&span), "deck span {span} m");
        // The centre of the bounding box is a weak test of `contains`, but a
        // deck that answers "no" everywhere would silently disable both the
        // ribbon suppression and the height sampling.
        let mid = [(d.min[0] + d.max[0]) * 0.5, (d.min[1] + d.max[1]) * 0.5];
        assert!(
            !d.contains([d.min[0] - 5.0, mid[1]]),
            "outside is not inside"
        );
    }
    assert!(
        decks
            .iter()
            .any(|d| d.contains([(d.min[0] + d.max[0]) * 0.5, (d.min[1] + d.max[1]) * 0.5])),
        "no deck contains its own centre — point-in-polygon is broken"
    );
}

/// The carriageway is cut **out of** the slab it stands on, so the two share a
/// plane with nothing left to arbitrate. If the subtraction silently did
/// nothing, the road and the deck would be two coplanar opaque surfaces and
/// the deck — which sits at the top of the ground ladder — would win, i.e. the
/// bare slab would be back with no other symptom.
#[test]
fn the_carriageway_is_cut_out_of_the_deck() {
    let decks = tessellate_bridges(GRONINGEN_Z14, 1000.0).expect("bridges");
    let (roads, carved) = roads_and_decks(GRONINGEN_Z14, 1000.0, &decks).expect("roads");
    let area = |d: &BridgeDeck| -> f64 {
        d.indices
            .chunks_exact(3)
            .map(|t| {
                let p: Vec<[f32; 3]> = t.iter().map(|i| d.positions[*i as usize]).collect();
                f64::from(
                    ((p[1][0] - p[0][0]) * (p[2][2] - p[0][2])
                        - (p[2][0] - p[0][0]) * (p[1][2] - p[0][2]))
                        .abs(),
                ) * 0.5
            })
            .sum()
    };
    let mut carried = 0;
    for (di, (before, after)) in decks.iter().zip(&carved).enumerate() {
        let rides = roads.on_deck.iter().any(|(d, _)| *d as usize == di);
        if !rides {
            assert!(
                (area(after) - area(before)).abs() < 1e-3,
                "deck {di} carries no road but was cut anyway"
            );
            continue;
        }
        carried += 1;
        assert!(
            area(after) < area(before),
            "deck {di} carries a road and kept its whole surface"
        );
        // …and it is not cut away entirely: what is left is the verge and the
        // parapet either side of the carriageway, which is what makes a bridge
        // read as a structure rather than as a strip of tarmac in mid-air.
        assert!(
            area(after) > 0.05 * area(before),
            "deck {di} lost {:.0}% of its surface to the road on it",
            100.0 * (1.0 - area(after) / area(before))
        );
    }
    assert!(carried > 0, "no deck in the fixture carries a road");
}
