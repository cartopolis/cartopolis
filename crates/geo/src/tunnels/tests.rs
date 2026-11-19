use super::*;
use crate::road_texture::{road_class, StreetFlags};
use crate::vector_tiles::kind_of;

/// The ring tile's own ground size in metres. Derived from the tile's
/// coordinates rather than written down: a hand-typed span puts every
/// distance measured here out by a fraction of a per cent.
fn ring_tile_m() -> f32 {
    let (_, y) = crate::testdata::GRONINGEN_RING_TILE;
    let n = 2f64.powi(14);
    #[allow(clippy::cast_precision_loss)]
    let lat = (std::f64::consts::PI * (1.0 - 2.0 * y as f64 / n))
        .sinh()
        .atan();
    #[allow(clippy::cast_possible_truncation)]
    let m = (40_075_016.686 * lat.cos() / n) as f32;
    m
}

fn fixtures() -> [(TileData, f32, &'static str); 2] {
    [
        (crate::testdata::groningen(), 1471.0, "groningen"),
        (
            crate::testdata::groningen_ring(),
            ring_tile_m(),
            "groningen_ring",
        ),
    ]
}

/// A straight run from `a` to `b`, densified the way a tile's runs are.
fn run(level: i8, a: [f32; 2], b: [f32; 2], feature: usize) -> StreetRun {
    StreetRun {
        key: feature as u64,
        feature,
        level,
        class: RoadClass::Major,
        half_w: 5.0,
        points: road_levels::densify(&[a, b], 3.0),
        cut: [false; 2],
    }
}

/// A 200 m tunnel with 300 m of road out of each end.
fn a_tunnel_and_its_roads() -> Vec<StreetRun> {
    vec![
        run(0, [-300.0, 0.0], [0.0, 0.0], 0),
        run(-1, [0.0, 0.0], [200.0, 0.0], 1),
        run(0, [200.0, 0.0], [500.0, 0.0], 2),
    ]
}

/// **A tunnel and its ramps are one structure, and the ramps are open.** With
/// nothing measured the roads out of each end are graded down to the tunnel on
/// the network — the same solve atlas runs — so the structure reaches out along
/// them to where they have come back up, open to the sky; the tunnel itself is
/// roofed end to end, with a headwall at each mouth.
#[test]
fn a_tunnel_and_its_ramps_are_one_structure() {
    let runs = a_tunnel_and_its_roads();
    let profiles = unmeasured_profiles(&runs);
    assert!(
        profiles.iter().all(Option::is_some),
        "a road out of the tunnel is not part of it"
    );
    let out = build_structures(&runs, &profiles, None);
    let bore = out
        .tunnels
        .iter()
        .find(|t| t.covered.iter().any(|&c| c))
        .expect("the tunnel is roofed");
    assert!(
        bore.covered.iter().all(|&c| c),
        "the tunnel has an open stretch"
    );
    assert!(
        bore.road_y
            .iter()
            .all(|&y| (y + LEVEL_DEPTH_M).abs() < 0.05),
        "the tunnel is not a level down: {:?}",
        bore.road_y
    );
    let ramps: Vec<&Tunnel> = out
        .tunnels
        .iter()
        .filter(|t| t.covered.iter().all(|&c| !c))
        .collect();
    assert_eq!(ramps.len(), 2, "expected a ramp at each end");
    for r in &ramps {
        let len: f32 = polyline_len(&r.centre);
        // 7 m at 6 % is 117 m, and the structure stops a metre short of the
        // top, plus the point beyond.
        assert!((95.0..125.0).contains(&len), "a ramp {len:.0} m long");
        let shallowest = r.road_y.iter().copied().fold(f32::MIN, f32::max);
        assert!(
            shallowest > -SUNK_M_TEST - 0.01,
            "a ramp stops {shallowest:.2} m down"
        );
    }
    assert_eq!(out.openings.len(), 2, "each ramp opens the ground over it");
    // One headwall per mouth: two quads whose top edge is at the land.
    let heads = out
        .meshes
        .iter()
        .filter(|m| m.group == SurfaceGroup::TunnelStructure)
        .flat_map(|m| m.normals.iter())
        .filter(|n| n[0].abs() > 0.99)
        .count();
    assert_eq!(
        heads,
        2 * 6,
        "expected a headwall over each of the two mouths"
    );
}

const SUNK_M_TEST: f32 = road_levels::SUNK_M;

/// **The mouths are where the headwalls are**, facing out of the tunnel: one
/// at each end of a tunnel with a ramp at each end.
#[test]
fn a_tunnel_has_a_mouth_at_each_end_facing_out() {
    let runs = a_tunnel_and_its_roads();
    let m = mouths(&runs, &unmeasured_profiles(&runs));
    assert_eq!(m.len(), 2, "{m:?}");
    for mouth in &m {
        // The tunnel runs x = 0..200; out of it is towards the nearer end.
        let expect = if mouth.at[0] < 100.0 { -1.0 } else { 1.0 };
        assert!((mouth.out[0] - expect).abs() < 1e-3, "{mouth:?}");
        assert!(mouth.road_y < mouth.land_y - 5.0);
    }
}

/// **The measured shape is drawn as measured.** A cutting the survey put nine
/// metres under land two metres above the datum has its carriageway at −7 and
/// its wall tops at +2 — the heights are final, and nothing lifts them.
#[test]
fn the_published_heights_are_the_drawn_ones() {
    let r = run(0, [0.0, 0.0], [120.0, 0.0], 0);
    let n = r.points.len();
    let profile = Profile {
        road: vec![-7.0; n],
        land: vec![2.0; n],
        walls: Vec::new(),
    };
    let out = build_structures(&[r], &[Some(profile)], None);
    let walls = out
        .meshes
        .iter()
        .find(|m| m.group == SurfaceGroup::TunnelStructure)
        .expect("walls");
    let (lo, hi) = walls
        .positions
        .iter()
        .fold((f32::MAX, f32::MIN), |(a, b), p| (a.min(p[1]), b.max(p[1])));
    assert!(
        (lo + 7.0).abs() < 1e-4 && (hi - 2.0).abs() < 1e-4,
        "walls span {lo}..{hi}"
    );
    assert!(
        out.tunnels[0].covered.iter().all(|&c| !c),
        "a level-0 cutting was roofed"
    );
    assert_eq!(out.openings.len(), 1);
}

/// **Two halves of one road in a cutting share its floor.** Twelve metres
/// apart, five metres down: the walls stand on the outside of each, and
/// between them there is a floor under the road and a barrier no taller than
/// a barrier — not two walls the depth of the cutting with a void between, as
/// the DUO cutting was drawn until 2026-09-28.
#[test]
fn a_dual_carriageway_in_a_cutting_shares_its_floor() {
    let runs = [
        run(0, [0.0, 0.0], [120.0, 0.0], 0),
        run(0, [0.0, 12.0], [120.0, 12.0], 1),
    ];
    let profiles: Vec<Option<Profile>> = runs
        .iter()
        .map(|r| {
            Some(Profile {
                road: vec![-5.0; r.points.len()],
                land: vec![0.0; r.points.len()],
                walls: Vec::new(),
            })
        })
        .collect();
    let out = build_structures(&runs, &profiles, None);
    let shell = out
        .meshes
        .iter()
        .find(|m| m.group == SurfaceGroup::TunnelStructure)
        .expect("walls");
    let between = shell
        .positions
        .iter()
        .filter(|p| p[2] > 5.5 && p[2] < 6.5)
        .map(|p| p[1])
        .fold(f32::MIN, f32::max);
    assert!(
        between <= -5.0 + BARRIER_M + 1e-3,
        "something {between} m high stands in the median"
    );
    assert!(
        shell.normals.iter().any(|n| n[1] > 0.99),
        "no floor was laid across the median"
    );
    let outer = shell
        .positions
        .iter()
        .filter(|p| p[2] < -4.9 || p[2] > 16.9)
        .map(|p| p[1]);
    assert!(
        outer.fold(f32::MIN, f32::max) >= -1e-3,
        "the outer walls do not reach the land"
    );
}

/// **The walls stand where the survey put them.** A ramp three metres down
/// whose trench is sixteen metres wide on one side of its line and a bank on
/// the other: a wall eight metres out on the left with floor between it and
/// the carriageway, nothing standing on the right, and the ground opened out
/// to the wall — not a slot the width of the road (2026-09-28).
#[test]
fn the_walls_stand_where_the_survey_put_them() {
    let r = run(0, [0.0, 0.0], [60.0, 0.0], 0);
    let n = r.points.len();
    let profile = Profile {
        road: vec![-3.0; n],
        land: vec![0.0; n],
        walls: vec![[8.0, 0.0]; n],
    };
    let out = build_structures(&[r], &[Some(profile)], None);
    let shell = out
        .meshes
        .iter()
        .find(|m| m.group == SurfaceGroup::TunnelStructure)
        .expect("a wall");
    // Left of a run heading east is +y (south) in this frame.
    assert!(
        shell
            .positions
            .iter()
            .any(|p| (p[2] - 8.0).abs() < 1e-3 && p[1] > -1e-3),
        "no wall at eight metres reaching the land"
    );
    assert!(
        shell.positions.iter().all(|p| p[2] > -5.0 - 1e-3),
        "something stands on the side that is a bank"
    );
    assert!(
        shell
            .positions
            .iter()
            .zip(&shell.normals)
            .any(|(p, n)| n[1] > 0.99 && (p[2] - 8.0).abs() < 1e-3),
        "no floor out to the wall"
    );
    let ring = &out.openings[0];
    let (lo, hi) = ring.iter().fold((f32::MAX, f32::MIN), |(lo, hi), p| {
        (lo.min(p[1]), hi.max(p[1]))
    });
    assert!(
        (hi - 8.5).abs() < 1e-3,
        "opening reaches {hi}, not past the wall"
    );
    assert!(
        (lo + 5.5).abs() < 1e-3,
        "opening reaches {lo} on the bank side"
    );
}

/// **A shallow underpass is roofed under the street it passes below.** A
/// path 2.5 m under its land has no room for its headroom and a slab, and
/// its open stretch cut a slot through the road over it (2026-09-29). Under
/// the road it is roofed, with the ceiling held under the land, and the
/// ground is opened only either side of it.
#[test]
fn a_shallow_underpass_is_roofed_under_the_road_it_passes_below() {
    let mut path = run(-1, [0.0, 0.0], [60.0, 0.0], 0);
    path.class = RoadClass::Path;
    path.half_w = 1.5;
    let road = run(0, [30.0, -40.0], [30.0, 40.0], 1);
    let n = path.points.len();
    let profiles = vec![
        Some(Profile {
            road: vec![-2.5; n],
            land: vec![0.0; n],
            walls: Vec::new(),
        }),
        None,
    ];
    let out = build_structures(&[path, road], &profiles, None);
    let t = &out.tunnels[0];
    let under = (0..t.centre.len() - 1)
        .find(|&k| t.centre[k][0] <= 30.0 && t.centre[k + 1][0] >= 30.0)
        .expect("a segment under the road");
    assert!(
        t.covered[under] && t.covered[under + 1],
        "not roofed under the road"
    );
    let shell = out
        .meshes
        .iter()
        .find(|m| m.group == SurfaceGroup::TunnelStructure)
        .expect("a shell");
    assert!(
        shell
            .positions
            .iter()
            .filter(|p| (p[0] - 30.0).abs() < 6.0)
            .all(|p| p[1] <= -ROOF_SLAB_M + 1e-3),
        "something under the road stands above the slab under the land"
    );
    for ring in &out.openings {
        assert!(
            !ring.iter().any(|p| (p[0] - 30.0).abs() < 4.0) || ring.len() < 3,
            "the ground was opened under the road"
        );
    }
}

/// **A tunnel under a span is open.** Atlas measures walls only for a
/// stretch open to the sky, which is how a path OSM tags as a tunnel beneath
/// a bridge reaches the client: deep under its land, and never to be given a
/// lid (2026-09-28).
#[test]
fn a_tunnel_under_a_span_is_not_roofed() {
    let bore = run(-1, [0.0, 0.0], [60.0, 0.0], 0);
    let n = bore.points.len();
    let profile = Profile {
        road: vec![-7.0; n],
        land: vec![0.0; n],
        walls: vec![[9.0, 9.0]; n],
    };
    let out = build_structures(&[bore], &[Some(profile)], None);
    assert!(
        out.tunnels.iter().all(|t| t.covered.iter().all(|c| !c)),
        "a stretch atlas measured walls for was roofed"
    );
    assert!(!out.openings.is_empty(), "and its ground was not opened");
}

/// **A trench wider than the bore it runs into is closed where they meet**,
/// from the bore's walls out to its own, or the mouth shows the void behind
/// the earth the opening took away.
#[test]
fn a_wide_trench_meeting_a_narrow_bore_is_closed() {
    let ramp = run(0, [0.0, 0.0], [60.0, 0.0], 0);
    let bore = run(-1, [60.0, 0.0], [160.0, 0.0], 1);
    let profiles = vec![
        Some(Profile {
            road: vec![-4.0; ramp.points.len()],
            land: vec![0.0; ramp.points.len()],
            walls: vec![[8.0, 8.0]; ramp.points.len()],
        }),
        Some(Profile {
            road: vec![-7.0; bore.points.len()],
            land: vec![0.0; bore.points.len()],
            walls: Vec::new(),
        }),
    ];
    let out = build_structures(&[ramp, bore], &profiles, None);
    let shell = out
        .meshes
        .iter()
        .find(|m| m.group == SurfaceGroup::TunnelStructure)
        .expect("walls");
    for side in [1.0f32, -1.0] {
        assert!(
            shell.positions.iter().zip(&shell.normals).any(|(p, n)| {
                (p[0] - 60.0).abs() < 1e-3 && (p[2] * side - 8.0).abs() < 1e-3 && n[0] < -0.99
            }),
            "no wing on the {side} side where the ramp meets the bore"
        );
    }
}

/// The most low roofs either fixture had when [`audit`] was written
/// (2026-09-29): a ceiling on how far this can drift unnoticed.
const LOW_ROOFS_MAX: usize = 6;

/// **The fixtures cut no street and step into nothing** — on flat land, as a
/// tile with no atlas behind it is built. Low roofs are reported rather than
/// refused: on flat land a path junction at grade seven metres before a
/// carriageway leaves the underpass under it less than a metre of headroom,
/// and a low roof is the honest drawing of that (`MIN_HEADROOM_M`).
#[test]
fn the_fixtures_pass_the_audit() {
    let mut tally = Vec::new();
    for (tile, tile_m, name) in fixtures() {
        let runs = road_levels::street_runs(&tile, tile_m);
        let found = audit(&runs, &unmeasured_profiles(&runs));
        for f in &found {
            let street = runs.iter().find(|r| {
                r.points
                    .iter()
                    .any(|p| (p[0] - f.at[0]).hypot(p[1] - f.at[1]) < 0.01)
            });
            println!(
                "{name}: {:?} at {:?} on {:?}",
                f.flaw,
                f.at,
                street.map(|r| (
                    r.level,
                    r.class,
                    r.half_w,
                    r.points[0],
                    r.points[r.points.len() - 1]
                ))
            );
        }
        let broken = found
            .iter()
            .filter(|f| f.flaw != Flaw::CeilingOutOfPlace)
            .count();
        tally.push((name, broken, found.len() - broken));
    }
    println!("{tally:?}");
    for (name, broken, low) in tally {
        assert_eq!(
            broken, 0,
            "{name}: {broken} streets cut or steps into nothing"
        );
        assert!(
            low <= LOW_ROOFS_MAX,
            "{name}: {low} low roofs, more than {LOW_ROOFS_MAX}"
        );
    }
}

/// **A roofed deck keeps its headroom under the land.**
#[test]
fn a_roofed_deck_keeps_its_headroom() {
    for (tile, tile_m, name) in fixtures() {
        let out = build_tunnels(&tile, tile_m, None);
        for t in &out.tunnels {
            for (i, covered) in t.covered.iter().enumerate() {
                if *covered {
                    let ceiling = t.ceiling(i);
                    assert!(
                        ceiling <= t.land_y[i] - ROOF_SLAB_M + 0.01
                            && ceiling >= t.road_y[i] + MIN_HEADROOM_M - 0.01,
                        "{name}: a lid at {ceiling} m is not under the land at {} m",
                        t.land_y[i]
                    );
                }
            }
        }
    }
}

/// **A tunnel's carriageway must face up**, or the road vanishes at the
/// portal. Same rule and same test as `vector_tiles`' `ribbon_faces_up`: in
/// this frame — x east, y south — an up-facing triangle has a non-positive
/// cross product.
#[test]
fn a_tunnel_deck_faces_up() {
    let out = build_tunnels(&crate::testdata::groningen(), 1471.0, None);
    let mut checked = 0usize;
    for m in &out.meshes {
        if !matches!(m.group, SurfaceGroup::TunnelRoad(_)) {
            continue;
        }
        for t in m.indices.chunks_exact(3) {
            let (a, b, c) = (
                m.positions[t[0] as usize],
                m.positions[t[1] as usize],
                m.positions[t[2] as usize],
            );
            let cross = (b[0] - a[0]) * (c[2] - a[2]) - (b[2] - a[2]) * (c[0] - a[0]);
            assert!(
                cross <= 1e-4,
                "a tunnel deck triangle is wound downward and will be culled \
                     from above: {a:?} {b:?} {c:?}"
            );
            checked += 1;
        }
    }
    assert!(checked > 0, "the fixture built no tunnel deck to check");
}

/// **Every face of a structure is wound toward the side its normal says**,
/// which for the roof and the walls is the *inside*. The material culls back
/// faces, so a winding that disagrees with the normal is a surface that is not
/// there from the only side anybody sees it from — from inside a lid the
/// tunnel once had no ceiling and no sides (2026-09-23).
#[test]
fn every_tunnel_face_is_wound_toward_its_normal() {
    let mut checked = [0usize; 2];
    for (tile, tile_m, _) in fixtures() {
        let out = build_tunnels(&tile, tile_m, None);
        for m in &out.meshes {
            let slot = match m.group {
                SurfaceGroup::TunnelRoad(_) => 0,
                SurfaceGroup::TunnelStructure => 1,
                _ => continue,
            };
            for t in m.indices.chunks_exact(3) {
                let [a, b, c] = [0, 1, 2].map(|k| m.positions[t[k] as usize]);
                let (u, v) = (
                    [b[0] - a[0], b[1] - a[1], b[2] - a[2]],
                    [c[0] - a[0], c[1] - a[1], c[2] - a[2]],
                );
                let g = [
                    u[1] * v[2] - u[2] * v[1],
                    u[2] * v[0] - u[0] * v[2],
                    u[0] * v[1] - u[1] * v[0],
                ];
                let len = (g[0] * g[0] + g[1] * g[1] + g[2] * g[2]).sqrt();
                // The notch triangles on the inside of a bend collapse to a
                // hair and have no winding worth asking about.
                if len < 1e-3 {
                    continue;
                }
                let n = m.normals[t[0] as usize];
                let dot = (g[0] * n[0] + g[1] * n[1] + g[2] * n[2]) / len;
                assert!(
                    dot > 0.5,
                    "a {:?} triangle is wound away from its normal {n:?}: {a:?} {b:?} {c:?}",
                    m.group
                );
                checked[slot] += 1;
            }
        }
    }
    assert!(
        checked[0] > 0 && checked[1] > 0,
        "no tunnel faces to check: {checked:?}"
    );
}

/// **Under a lid the walls stop at the ceiling.** Above it is earth, and a
/// wall carried on to the land stood out of the Oude Winschoterdiep as two
/// long slivers along the lid under it (2026-09-23).
#[test]
fn no_wall_rises_through_a_lid() {
    let out = build_tunnels(&crate::testdata::groningen_ring(), ring_tile_m(), None);
    let mut checked = 0usize;
    for m in out
        .meshes
        .iter()
        .filter(|m| m.group == SurfaceGroup::TunnelStructure)
    {
        for p in &m.positions {
            for t in out.tunnels.iter().filter(|t| t.half_w >= 3.0) {
                for k in 1..t.centre.len().saturating_sub(1) {
                    if !(t.covered[k - 1] && t.covered[k] && t.covered[k + 1]) {
                        continue;
                    }
                    let c = t.centre[k];
                    if (p[0] - c[0]).hypot(p[2] - c[1]) > t.half_w + 0.05 {
                        continue;
                    }
                    let ceiling = t.ceiling(k);
                    checked += 1;
                    assert!(
                        p[1] <= ceiling + 0.01,
                        "a tunnel wall reaches {:.2} m under a lid whose ceiling is {ceiling:.2} m",
                        p[1]
                    );
                }
            }
        }
    }
    assert!(checked > 0, "no structure under a lid was checked");
}

/// **No open structure ends below its land with nothing beyond it.** Every
/// open end of every structure either comes back up to within a walled road's
/// depth of its land, or is cut by the tile border, or is met by another
/// structure at the same point and the same height. A step is none of those —
/// it is the carriageway falling off a cliff, which is what the ring did six
/// times until 2026-09-22, what a slip road out of a portal did at grade beside
/// a trench seven metres deep (2026-09-23), and what a cycle path's ramp did
/// where it ran into the side of a street that had no vertex there
/// (`road_levels::TEE_M`). A roofed end with nothing beyond is a tunnel into
/// something the map does not draw — a garage under a building — and is
/// closed by its headwall.
#[test]
fn no_open_structure_ends_below_its_land() {
    for (tile, tile_m, name) in fixtures() {
        let runs = road_levels::street_runs(&tile, tile_m);
        let plans = plan(&runs, &unmeasured_profiles(&runs));
        let ends: Vec<([f32; 2], f32, f32, bool, bool)> = plans
            .iter()
            .flat_map(|t| {
                let n = t.centre.len() - 1;
                [
                    (
                        t.centre[0],
                        t.road_y[0],
                        t.land_y[0],
                        t.roofed[0] || t.cut[0],
                        t.dead[0],
                    ),
                    (
                        t.centre[n],
                        t.road_y[n],
                        t.land_y[n],
                        t.roofed[n] || t.cut[1],
                        t.dead[1],
                    ),
                ]
            })
            .collect();
        let (mut deep, mut closed) = (0usize, 0usize);
        for (i, &(p, y, g, roofed_or_cut, dead)) in ends.iter().enumerate() {
            if y > g - SUNK_M_TEST - 0.01 || roofed_or_cut {
                continue;
            }
            if dead {
                closed += 1;
                continue;
            }
            deep += 1;
            let met = ends.iter().enumerate().any(|(j, &(q, oy, ..))| {
                j != i && (q[0] - p[0]).hypot(q[1] - p[1]) < 0.02 && (oy - y).abs() < 0.05
            });
            assert!(
                met,
                "{name}: a structure ends {:.1} m under its land at {p:?} with nothing beyond it",
                g - y
            );
        }
        println!("{name}: {deep} deep open ends met, {closed} closed dead ends");
    }
}

/// **A road that ends in nothing below its land is closed by a wall** facing
/// back along it: a cutting stub, sixty metres long and seven deep, with no
/// other street anywhere near either end.
#[test]
fn a_dead_end_below_the_land_is_walled() {
    let r = run(0, [0.0, 0.0], [60.0, 0.0], 0);
    let n = r.points.len();
    let profile = Profile {
        road: vec![-7.0; n],
        land: vec![0.0; n],
        walls: Vec::new(),
    };
    let out = build_structures(&[r], &[Some(profile)], None);
    let ends = out
        .meshes
        .iter()
        .filter(|m| m.group == SurfaceGroup::TunnelStructure)
        .flat_map(|m| m.normals.iter())
        .filter(|n| n[0].abs() > 0.99)
        .count();
    assert_eq!(ends, 2 * 6, "expected an end wall at each end of the stub");
}

/// **A structure ending below its land at a street it does not meet is
/// closed.** A cutting three metres down that stops where a street at grade
/// tees into its end is not alone, so it is not a dead end — and was left
/// open onto the void. It gets the same end wall.
#[test]
fn an_unmet_end_below_the_land_is_walled() {
    let cut = run(0, [0.0, 0.0], [60.0, 0.0], 0);
    let street = run(0, [60.0, 0.0], [60.0, 30.0], 1);
    let n = cut.points.len();
    let profiles = vec![
        Some(Profile {
            road: vec![-3.0; n],
            land: vec![0.0; n],
            walls: Vec::new(),
        }),
        None,
    ];
    let out = build_structures(&[cut, street], &profiles, None);
    let walls_at_end = out
        .meshes
        .iter()
        .filter(|m| m.group == SurfaceGroup::TunnelStructure)
        .flat_map(|m| m.positions.iter().zip(&m.normals))
        .filter(|(p, n)| n[0] < -0.99 && (p[0] - 60.0).abs() < 0.01)
        .count();
    assert!(walls_at_end > 0, "the cutting's end was left open");
}

/// **A building in a covered tunnel's old trench is the tunnel's, and one on
/// the lid is not.** The rule needs both halves: the place, and the ground
/// under it being well below the land around.
#[test]
fn a_building_in_a_covered_trench_is_told_from_one_on_the_lid() {
    let out = build_tunnels(&crate::testdata::groningen_ring(), ring_tile_m(), None);
    let t = out
        .tunnels
        .iter()
        .find(|t| t.half_w >= 3.0 && t.covered.iter().all(|c| *c) && t.centre.len() >= 3)
        .expect("the ring has a lid");
    let (a, b) = (t.centre[0], t.centre[t.centre.len() - 1]);
    let over = [(a[0] + b[0]) * 0.5, (a[1] + b[1]) * 0.5];
    let over = t
        .centre
        .iter()
        .copied()
        .min_by(|p, q| {
            (p[0] - over[0])
                .hypot(p[1] - over[1])
                .total_cmp(&(q[0] - over[0]).hypot(q[1] - over[1]))
        })
        .unwrap();
    assert!(stands_in_a_covered_trench(
        &out.tunnels,
        &out.openings,
        over,
        7.0
    ));
    assert!(!stands_in_a_covered_trench(
        &out.tunnels,
        &out.openings,
        over,
        0.5
    ));
    let beside = [over[0] + 60.0, over[1] + 60.0];
    assert!(!stands_in_a_covered_trench(
        &out.tunnels,
        &out.openings,
        beside,
        7.0
    ));
}

/// **The survey's own tunnel walls are recognised, and a wall elsewhere is
/// not.** Measured on the ring tile's BGT (2026-09-23), in this tile's metres.
#[test]
fn the_surveyed_walls_of_the_ring_are_the_tunnels() {
    let out = build_tunnels(&crate::testdata::groningen_ring(), ring_tile_m(), None);
    // The east portal's headwall, measured 7.02 m, across both mouths.
    let headwall = [[956.0f32, 319.0], [969.0, 352.0]];
    // The north cutting's retaining wall, measured 9.11 m.
    let retaining = [[691.0f32, 556.0], [779.0, 478.0]];
    // A wall running along the lid.
    let along_lid = [[772.0f32, 474.0], [961.0, 333.0]];
    for (name, w) in [
        ("headwall", &headwall[..]),
        ("retaining wall", &retaining[..]),
        ("wall along the lid", &along_lid[..]),
    ] {
        assert!(
            wall_of_a_sunken_road(&out.tunnels, &out.openings, w),
            "the ring's {name} is not recognised as the tunnel's"
        );
    }
    let elsewhere = [[691.0f32, 656.0], [779.0, 578.0]];
    assert!(!wall_of_a_sunken_road(
        &out.tunnels,
        &out.openings,
        &elsewhere
    ));
}

/// **No crown hangs over an open trench** once the caller's filter has run.
#[test]
fn no_tree_crown_reaches_into_an_opening() {
    let tile_m = ring_tile_m();
    let tile = crate::testdata::groningen_ring();
    let out = build_tunnels(&tile, tile_m, None);
    let mut trees = crate::scatter::scatter_vegetation(&tile, tile_m, 7).expect("scatters");
    // The trees this filter used to find near an opening here stood by
    // "tunnels" less than a metre under flat land, which are at grade now
    // (`road_levels::structures`); the check that matters is below.
    assert!(
        !out.openings.is_empty(),
        "the fixture cuts no opening to test against"
    );
    trees.retain(|t| !near_an_opening(&out.openings, [t.x, t.z], t.radius * 0.8));
    for t in &trees {
        for ring in &out.openings {
            assert!(
                !point_in_ring(ring, [t.x, t.z]),
                "a tree stands in an opening at {},{}",
                t.x,
                t.z
            );
        }
    }
}

/// **A sunken road under water is covered.** A road can be open to the sky
/// or under a canal, never both — the phone once showed the ring's carriageway
/// crossing the Oude Winschoterdiep at water level (2026-09-23).
#[test]
fn a_road_under_water_has_a_lid_over_it() {
    let water = |tile: &TileData, tile_m: f32| -> Vec<Vec<[f32; 2]>> {
        let Some(meta) = tile.layer("water_polygons") else {
            return Vec::new();
        };
        let scale = tile_m / TileData::extent(meta);
        let ring = |p: &geo_types::Polygon<f32>| -> Vec<[f32; 2]> {
            p.exterior()
                .0
                .iter()
                .map(|c| [c.x * scale, c.y * scale])
                .collect()
        };
        tile.features(meta)
            .iter()
            .flat_map(|f| match &f.geometry {
                Geometry::Polygon(p) => vec![ring(p)],
                Geometry::MultiPolygon(mp) => mp.0.iter().map(ring).collect(),
                _ => Vec::new(),
            })
            .collect()
    };
    let mut covered_under_water = 0usize;
    for (tile, tile_m, name) in fixtures() {
        let pools = water(&tile, tile_m);
        let out = build_tunnels(&tile, tile_m, None);
        for t in &out.tunnels {
            for i in 0..t.centre.len().saturating_sub(1) {
                let (a, b) = (t.centre[i], t.centre[i + 1]);
                let deep =
                    (t.road_y[i] - t.land_y[i]).min(t.road_y[i + 1] - t.land_y[i + 1]) < -1.0;
                for f in [0.25f32, 0.5, 0.75] {
                    let p = [a[0] + (b[0] - a[0]) * f, a[1] + (b[1] - a[1]) * f];
                    if !pools.iter().any(|r| point_in_ring(r, p)) {
                        continue;
                    }
                    if t.covered[i] && t.covered[i + 1] {
                        covered_under_water += 1;
                    } else {
                        assert!(
                            !deep,
                            "{name}: a sunken road at {p:?} runs open under water"
                        );
                    }
                }
            }
        }
    }
    assert!(
        covered_under_water > 0,
        "no tunnel passes under water in either fixture"
    );
}

/// **The ground may not be cut away over a roofed stretch.** The hole and the
/// absent ceiling are decided by one test, so a roofed segment must not lie
/// inside any opening — and `merge_parallel_openings`, which replaces two
/// rings with their hull, is what could break that.
/// How many openings each fixture cuts, and their total area.
#[test]
#[ignore = "diagnostic"]
fn print_opening_area() {
    for (tile, tile_m, name) in fixtures() {
        let out = build_tunnels(&tile, tile_m, None);
        let area: f32 = out
            .openings
            .iter()
            .map(|r| {
                let mut a = 0.0;
                for i in 0..r.len() {
                    let (p, q) = (r[i], r[(i + 1) % r.len()]);
                    a += p[0] * q[1] - q[0] * p[1];
                }
                (a * 0.5).abs()
            })
            .sum();
        println!("OPEN {name} {} rings {area:.0} m2", out.openings.len());
    }
}

/// Every ring-fixture structure within 40 m of `CARTO_AT` = "x,z", vertex by
/// vertex: road, land, ceiling and roof — for reading a failing geometry
/// test's coordinates back to the structure it is about.
#[test]
#[ignore = "diagnostic"]
fn print_tunnels_near_a_point() {
    let (tile, tile_m) = if std::env::var("CARTO_FIXTURE").as_deref() == Ok("groningen") {
        (crate::testdata::groningen(), 1471.0)
    } else {
        (crate::testdata::groningen_ring(), ring_tile_m())
    };
    let out = build_tunnels(&tile, tile_m, None);
    let runs = road_levels::street_runs(&tile, tile_m);
    let q: Vec<f32> = std::env::var("CARTO_AT")
        .unwrap()
        .split(',')
        .map(|v| v.parse().unwrap())
        .collect();
    for t in &out.tunnels {
        if t.centre
            .iter()
            .any(|p| (p[0] - q[0]).hypot(p[1] - q[1]) < 40.0)
        {
            println!("half {} clearance {}", t.half_w, t.clearance);
            for (k, p) in t.centre.iter().enumerate() {
                println!(
                    "  {:.1},{:.1} road {:.2} land {:.2} ceil {:.2} {}",
                    p[0],
                    p[1],
                    t.road_y[k],
                    t.land_y[k],
                    t.ceiling(k),
                    if t.covered[k] { "R" } else { "." }
                );
            }
        }
    }
    for r in &runs {
        if r.points
            .iter()
            .any(|p| (p[0] - q[0]).hypot(p[1] - q[1]) < 15.0)
        {
            println!(
                "street level {} {:?} half {} {:?} .. {:?}",
                r.level,
                r.class,
                r.half_w,
                r.points[0],
                r.points[r.points.len() - 1]
            );
        }
    }
}

#[test]
fn no_opening_swallows_a_roofed_stretch() {
    for (tile, tile_m, name) in fixtures() {
        let out = build_tunnels(&tile, tile_m, None);
        for t in &out.tunnels {
            for i in 0..t.covered.len().saturating_sub(1) {
                if !(t.covered[i] && t.covered[i + 1]) {
                    continue;
                }
                let (a, b) = (t.centre[i], t.centre[i + 1]);
                for f in [0.1f32, 0.5, 0.9] {
                    let p = [a[0] + (b[0] - a[0]) * f, a[1] + (b[1] - a[1]) * f];
                    for ring in &out.openings {
                        assert!(
                            !point_in_ring(ring, p),
                            "{name}: the ground is cut away over a lid at {p:?}"
                        );
                    }
                }
            }
        }
    }
}

/// **The ring's cuttings between the lids are open, and the lids are not**,
/// and neither is a four-metre seam in the tagging ([`MIN_OPEN_M`]).
#[test]
fn the_cuttings_are_open_and_the_lids_are_not() {
    let out = build_tunnels(&crate::testdata::groningen_ring(), ring_tile_m(), None);
    let roads: Vec<&Tunnel> = out.tunnels.iter().filter(|t| t.half_w >= 4.0).collect();
    let (mut open, mut lidded) = (0, 0);
    for t in &roads {
        if t.covered.iter().all(|c| !c) {
            let len = polyline_len(&t.centre);
            open += 1;
            let deep = t.road_y.iter().zip(&t.land_y).any(|(y, g)| y - g < -3.0);
            assert!(
                deep || len >= MIN_OPEN_M,
                "a {len:.0} m shallow stretch was opened"
            );
        } else {
            lidded += 1;
        }
    }
    assert!(open >= 4, "expected the ring's open cuttings, got {open}");
    assert!(lidded >= 6, "expected the ring's lids, got {lidded}");
}

/// **Each tunnel keeps its own class**, so a cell with a cycle underpass and a
/// road tunnel draws gravel under one and tarmac with lane markings under the
/// other. Over Groningen's ring a single class for the cell drew the motorway
/// in a footpath's gravel in the next cell (2026-09-20).
#[test]
fn each_tunnel_keeps_its_own_class() {
    let tile = crate::testdata::groningen();
    let out = build_tunnels(&tile, 1471.0, None);
    let meta = tile.layer("streets").expect("streets");
    let features = tile.features(meta).to_vec();
    let passages = building_passages(&tile, &features);
    let mut wanted: Vec<RoadClass> = Vec::new();
    for (fi, feature) in features.iter().enumerate() {
        if !underground(feature) || passages.contains(&fi) {
            continue;
        }
        let flags = StreetFlags {
            oneway: flag_of(feature, "oneway") || flag_of(feature, "oneway_reverse"),
            oneway_reverse: flag_of(feature, "oneway_reverse"),
            link: flag_of(feature, "link"),
        };
        if let Some(c) = road_class(kind_of(feature).unwrap_or(""), flags) {
            if !wanted.contains(&c) {
                wanted.push(c);
            }
        }
    }
    let mut drawn: Vec<RoadClass> = Vec::new();
    for m in &out.meshes {
        if let SurfaceGroup::TunnelRoad(c) = m.group {
            if !drawn.contains(&c) {
                drawn.push(c);
            }
        }
    }
    assert!(
        drawn.len() >= wanted.len().min(2),
        "tunnels of {wanted:?} are drawn as {drawn:?} — the decks were merged onto one class"
    );
}

#[test]
fn the_deck_is_found_under_the_carriageway_and_not_beside_it() {
    let tunnel = Tunnel {
        centre: vec![[0.0, 0.0], [100.0, 0.0]],
        road_y: vec![-7.0, -7.0],
        land_y: vec![0.0, 0.0],
        half_w: 6.0,
        clearance: CLEARANCE_M,
        covered: vec![true; 2],
    };
    assert_eq!(tunnel.road_under([50.0, 0.0]), Some(-7.0));
    assert_eq!(tunnel.road_under([50.0, 5.9]), Some(-7.0));
    assert!(tunnel.road_under([50.0, 8.0]).is_none());
    assert!(tunnel.road_under([-20.0, 0.0]).is_none());
    assert_eq!(tunnel.depth_near([50.0, 0.0], 0.0), Some(-7.0));
}

#[test]
fn the_hole_in_the_ground_covers_exactly_the_stretch_with_no_roof() {
    let centre: Vec<[f32; 2]> = (0..=60).map(|i| [i as f32 * 10.0, 0.0]).collect();
    let roofed: Vec<bool> = (0..=60).map(|i| (12..=48).contains(&i)).collect();
    let rings = opening_rings(&centre, &roofed, 6.0, &[]);
    assert_eq!(
        rings.len(),
        2,
        "an open stretch at each end should open two holes"
    );
    for ring in &rings {
        assert!(ring.len() >= 4);
        assert_eq!(ring.first(), ring.last(), "the hole is not a closed ring");
    }
    assert!(opening_rings(&centre[..2], &[true, true], 6.0, &[]).is_empty());
}

/// What the live tiles actually carry, read off the Groningen fixture: the
/// one claim about the data rather than about the geometry that the module
/// rests on.
#[test]
fn the_tiles_say_which_streets_are_tunnels() {
    let tile = crate::testdata::groningen();
    let meta = tile.layer("streets").expect("a streets layer");
    let features = tile.features(meta);
    let flagged = features.iter().filter(|f| flag_of(f, "tunnel")).count();
    let below = features.iter().filter(|f| underground(f)).count();
    assert!(flagged > 0, "no street in the fixture is flagged a tunnel");
    assert_eq!(
        below, flagged,
        "`underground` is the bare flag — the tiles carry no `layer`"
    );
    // **No `layer` anywhere in the layer**: if a tileset ever starts carrying
    // one, this fails and the depth convention has something better to read.
    assert!(
        features
            .iter()
            .all(|f| crate::vector_tiles::layer_of(f) == 0),
        "a street carries a `layer` now"
    );
    let passages = building_passages(&tile, &features);
    assert!(
        passages.len() < flagged,
        "every flagged way was called a building passage"
    );
}

/// The cut must actually take ground away: a no-op here looks exactly like a
/// feature that was never built.
#[test]
fn the_cut_removes_the_ground_over_an_opening() {
    let mesh = SurfaceMesh {
        group: SurfaceGroup::Grass,
        positions: vec![
            [0.0, 0.0, 0.0],
            [100.0, 0.0, 0.0],
            [100.0, 0.0, 100.0],
            [0.0, 0.0, 100.0],
        ],
        indices: vec![0, 1, 2, 0, 2, 3],
        uvs: Vec::new(),
        normals: Vec::new(),
        colors: Vec::new(),
        rungs: Vec::new(),
    };
    let before = mesh_area(&mesh);
    let opening = vec![
        [40.0, -10.0],
        [60.0, -10.0],
        [60.0, 110.0],
        [40.0, 110.0],
        [40.0, -10.0],
    ];
    // **Both ways round**: an opening's winding follows its tunnel's direction.
    for reversed in [false, true] {
        let mut ring = opening.clone();
        if reversed {
            ring.reverse();
        }
        let mut m = mesh.clone();
        cut_ground(std::slice::from_mut(&mut m), &[ring]);
        let after = mesh_area(&m);
        assert!(
            (before - after - 2000.0).abs() < 200.0,
            "reversed={reversed}: expected ~2000 m² gone, {:.0} went",
            before - after
        );
    }
}

/// **The basemap takes the same hole, and keeps its texture registered.**
#[test]
fn the_basemap_sheet_takes_the_hole_with_its_texture_intact() {
    let mut sheet = SurfaceMesh {
        group: SurfaceGroup::Built,
        positions: vec![
            [-92.0, 0.0, -92.0],
            [92.0, 0.0, -92.0],
            [92.0, 0.0, 92.0],
            [-92.0, 0.0, 92.0],
        ],
        indices: vec![0, 1, 2, 0, 2, 3],
        uvs: vec![[0.0, 0.0], [1.0, 0.0], [1.0, 1.0], [0.0, 1.0]],
        normals: vec![[0.0, 1.0, 0.0]; 4],
        colors: Vec::new(),
        rungs: Vec::new(),
    };
    let before = mesh_area(&sheet);
    let opening = vec![
        [-10.0, -100.0],
        [10.0, -100.0],
        [10.0, 100.0],
        [-10.0, 100.0],
        [-10.0, -100.0],
    ];
    assert!(cut_sheet(&mut sheet, &[opening]));
    let gone = before - mesh_area(&sheet);
    assert!((gone - 20.0 * 184.0).abs() < 100.0, "{gone:.0} m² went");
    for (p, uv) in sheet.positions.iter().zip(sheet.uvs.iter()) {
        let want = [(p[0] + 92.0) / 184.0, (p[2] + 92.0) / 184.0];
        assert!(
            (uv[0] - want[0]).abs() < 1e-3 && (uv[1] - want[1]).abs() < 1e-3,
            "vertex at {p:?} carries uv {uv:?}, not {want:?}"
        );
    }
}

/// **Ground over a lid is whole and ground over the open ramp is not**, and
/// the fill fades out with distance rather than stopping at a crease.
#[test]
fn the_fill_covers_the_lid_and_stops_at_the_ramp() {
    let tunnel = Tunnel {
        centre: vec![[0.0, 0.0], [50.0, 0.0], [100.0, 0.0]],
        road_y: vec![0.0, -0.5, -7.0],
        land_y: vec![0.0, 0.0, 0.0],
        half_w: 6.0,
        clearance: CLEARANCE_M,
        covered: vec![false, true, true],
    };
    let taper = 30.0;
    assert_eq!(tunnel.fill_weight([75.0, 0.0], taper), 1.0);
    assert_eq!(tunnel.fill_weight([75.0, 5.0], taper), 1.0);
    assert_eq!(
        tunnel.fill_weight([10.0, 0.0], taper),
        0.0,
        "the shallow ramp was filled in"
    );
    assert_eq!(tunnel.fill_weight([75.0, 400.0], taper), 0.0);
    let near = tunnel.fill_weight([75.0, 12.0], taper);
    let far = tunnel.fill_weight([75.0, 30.0], taper);
    assert!(
        (0.0..1.0).contains(&near) && near > far,
        "{near} then {far}"
    );
    assert_eq!(tunnel.land_near([75.0, 20.0]).1, 0.0);
}

/// **The fill is part of the surface the refinement resolves**: over a lid
/// the survey's nine-metre trench is put back to the land, and the lift is
/// big enough under a chord across the corridor for the crease rule to see.
#[test]
fn the_lid_is_visible_to_the_crease_rule() {
    struct Trench;
    impl GroundField for Trench {
        fn min_ground_edge_m(&self) -> f32 {
            crate::terrain_refine::MIN_GROUND_EDGE_M
        }
        fn at(&self, _x: f32, _z: f32) -> f32 {
            -9.0
        }
    }
    let t = Tunnel {
        centre: vec![[0.0, 0.0], [100.0, 0.0], [200.0, 0.0]],
        road_y: vec![-7.0; 3],
        land_y: vec![0.0; 3],
        half_w: 5.0,
        clearance: CLEARANCE_M,
        covered: vec![true; 3],
    };
    let around = |_x: f32, _z: f32| 0.0;
    let weight = |p: [f32; 2]| t.fill_weight(p, DATUM_M);
    let field = Lidded {
        ground: &Trench,
        around: &around,
        weight: &weight,
    };
    assert!(
        field.at(100.0, 0.0).abs() < 1e-3,
        "the ground over a lid stayed down"
    );
    assert!((field.at(100.0, 5.0 + DATUM_M + 1.0) + 9.0).abs() < 1e-3);
    let chord = f32::midpoint(field.at(100.0, -50.0), field.at(100.0, 50.0));
    let sag = (field.at(100.0, 0.0) - chord).abs();
    assert!(
        sag > crate::terrain_refine::CREASE_SAG_M,
        "the lid sags only {sag} m"
    );
}

/// Plan area of a mesh's triangles.
fn mesh_area(m: &SurfaceMesh) -> f32 {
    m.indices
        .chunks_exact(3)
        .map(|t| {
            let p = [
                m.positions[t[0] as usize],
                m.positions[t[1] as usize],
                m.positions[t[2] as usize],
            ];
            ((p[1][0] - p[0][0]) * (p[2][2] - p[0][2]) - (p[2][0] - p[0][0]) * (p[1][2] - p[0][2]))
                .abs()
                * 0.5
        })
        .sum()
}

/// **Nothing a tunnel emits may ask the depth buffer for more than the road
/// band holds**, and the deck asks for the ground rung — "inside the band"
/// would still be satisfied by the bridge rung, which would put a tunnel over
/// a flyover.
#[test]
fn a_tunnel_deck_draws_on_the_ground_rung() {
    let out = build_tunnels(&crate::testdata::groningen(), 1471.0, None);
    assert!(!out.meshes.is_empty(), "fixture grew no tunnels");
    let ceiling = crate::vector_tiles::ROAD_DEPTH_RUNGS as f32;
    for m in &out.meshes {
        for &r in &m.rungs {
            assert!(
                (0.0..ceiling).contains(&r),
                "{:?} emitted rung {r}",
                m.group
            );
        }
        if matches!(m.group, SurfaceGroup::TunnelRoad(_)) {
            assert!(m.rungs.iter().all(|&r| r == road_depth_rung(false)));
        }
    }
}

/// A side view of each structure in a tile, in text — how far down the road
/// goes, over what distance, under how much land and which part has a lid.
/// `CARTO_TUNNEL_TILE=/path/to.mvt` to read a live tile instead.
#[test]
#[ignore = "diagnostic"]
fn dump_the_fixture_structures() {
    let owned = std::env::var("CARTO_TUNNEL_TILE")
        .ok()
        .map(|p| std::fs::read(p).expect("tile reads"));
    let tile = match &owned {
        Some(b) => TileData::parse(b).expect("tile parses"),
        None => crate::testdata::groningen_ring(),
    };
    let out = build_tunnels(&tile, ring_tile_m(), None);
    println!(
        "structures={} openings={}",
        out.tunnels.len(),
        out.openings.len()
    );
    for (i, t) in out.tunnels.iter().enumerate() {
        println!(
            "structure {i}: half_w={:.1} len={:.0} m from {:?}",
            t.half_w,
            polyline_len(&t.centre),
            t.centre[0]
        );
        let mut along = 0.0f32;
        for k in 0..t.centre.len() {
            if k > 0 {
                along += (t.centre[k][0] - t.centre[k - 1][0])
                    .hypot(t.centre[k][1] - t.centre[k - 1][1]);
            }
            println!(
                "  {along:6.0} m  road {:6.2}  land {:6.2}  {}",
                t.road_y[k],
                t.land_y[k],
                if t.covered[k] { "lid" } else { "open" }
            );
        }
    }
}
