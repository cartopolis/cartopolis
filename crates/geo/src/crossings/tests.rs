
use super::*;
use crate::vector_tiles::{INDEX_BYTES, VERTEX_BYTES};

fn crossing(kind: CrossingKind, tracks: u8) -> LevelCrossing {
    LevelCrossing {
        x: 40.0,
        z: 40.0,
        yaw: 0.0,
        kind,
        tracks,
        road_half: None,
    }
}

/// How far out one installation's mast stands from the crossing point, and
/// how long its bar is — both metres, **read back off the meshed geometry**,
/// not recomputed, so this measures what is drawn rather than what
/// `push_crossing` was asked for.
///
/// At `yaw = 0` the object frame is the world frame, so local +x is across
/// the road and local +z is along it. The two installations stand at
/// `±clearance` along, so `z > 0` selects one of them, the `+x` one; within
/// it the mast is the only box reaching the ground (`y < 0.5`), and its
/// outer face gives the mast centre.
///
/// The bar stands **raised**, so its length is a height, not a tip: the
/// bands stack in y from [`BOOM_Y`] up, at a fixed x beside the mast, and
/// the length is the top of that stack above `BOOM_Y`. The bar's inner face
/// is the most inboard geometry an installation has — 0.37 m in from the
/// mast centre, against the lamp head's 0.30 m and the mast's own 0.11 m —
/// so one x test picks the bands out without having to know which vertices
/// belong to which box, and answers `None` for a form that has no boom.
fn across(crossing: &LevelCrossing) -> (f32, Option<f32>) {
    assert_eq!(crossing.yaw, 0.0, "the frame this reads in");
    let meshes = build_crossing_meshes(std::slice::from_ref(crossing), None);
    let mine: Vec<[f32; 3]> = meshes[0]
        .positions
        .iter()
        .filter(|p| p[2] - crossing.z > 0.0)
        .map(|p| [p[0] - crossing.x, p[1], p[2] - crossing.z])
        .collect();
    let mast = mine
        .iter()
        .filter(|p| p[1] < 0.5)
        .map(|p| p[0])
        .fold(f32::MIN, f32::max)
        - MAST_HALF_W;
    // The 1 cm sits well inside the 7 cm between the bar's inner face and
    // the lamp head's, so it separates them without being a knife edge.
    let bar_face = mast - (MAST_HALF_W + 2.0 * BOOM_HALF_FACE) + 0.01;
    let reach = mine
        .iter()
        .filter(|p| p[0] < bar_face)
        .map(|p| p[1])
        .reduce(f32::max)
        .map(|top| top - BOOM_Y);
    (mast, reach)
}

/// The two mast centres **read back off the meshed geometry**, in tile-local
/// metres, at any yaw — the independent measurement [`mast_centres`] is
/// checked against.
///
/// Only the masts reach the ground: a lamp head sits at `mast_h` and a boom
/// band starts at [`BOOM_Y`], so `y < 0.5` picks out the two mast boxes and
/// nothing else. Each contributes twelve vertices at `y = 0`, symmetric about
/// its own centre, so a group's centroid *is* that centre.
///
/// The two groups are separated without consulting the helper — the masts sit
/// diametrically opposite through the crossing point, so the sign of the dot
/// product against the farthest ground vertex splits them — or this would
/// measure the helper against itself.
fn meshed_mast_centres(crossing: &LevelCrossing) -> [[f32; 2]; 2] {
    let meshes = build_crossing_meshes(std::slice::from_ref(crossing), None);
    assert_eq!(meshes.len(), 1);
    let ground: Vec<[f32; 2]> = meshes[0]
        .positions
        .iter()
        .filter(|p| p[1] < 0.5)
        .map(|p| [p[0] - crossing.x, p[2] - crossing.z])
        .collect();
    assert_eq!(
        ground.len(),
        24,
        "two mast boxes, twelve ground corners each"
    );

    let axis = *ground
        .iter()
        .max_by(|a, b| (a[0] * a[0] + a[1] * a[1]).total_cmp(&(b[0] * b[0] + b[1] * b[1])))
        .expect("a mast reaches the ground");
    let mut sides: [Vec<[f32; 2]>; 2] = [Vec::new(), Vec::new()];
    for p in &ground {
        let i = usize::from(p[0] * axis[0] + p[1] * axis[1] < 0.0);
        sides[i].push(*p);
    }
    sides.map(|group| {
        assert_eq!(group.len(), 12, "a mast box lost corners to the split");
        let n = group.len() as f32;
        let sum = group
            .iter()
            .fold([0.0f32; 2], |a, p| [a[0] + p[0], a[1] + p[1]]);
        [crossing.x + sum[0] / n, crossing.z + sum[1] / n]
    })
}

/// A pair of points equals another pair, in either order, to a millimetre.
fn same_pair(got: [[f32; 2]; 2], want: [[f32; 2]; 2]) -> bool {
    let near = |a: [f32; 2], b: [f32; 2]| (a[0] - b[0]).hypot(a[1] - b[1]) < 1e-3;
    (near(got[0], want[0]) && near(got[1], want[1]))
        || (near(got[0], want[1]) && near(got[1], want[0]))
}

/// The collider follows the mast, at a yaw where getting the rotation the
/// wrong way round would show.
///
/// This is the whole safety of the collider channel: `mast_centres` publishes
/// numbers no capture can see, so nothing but this test says the circle is on
/// the post rather than across the road from it. A yaw-0 check would pass with
/// the rotation's sign flipped — the failure this module's siblings have
/// already paid for twice (`interiors`' door normals).
#[test]
fn the_mast_centres_are_where_the_masts_are_meshed() {
    for yaw in [0.0, 0.7, -1.9, 2.6] {
        for kind in [
            CrossingKind::HalfBarrier,
            CrossingKind::MiniBarrier,
            CrossingKind::FullBarrier,
            CrossingKind::Lights,
        ] {
            let c = LevelCrossing {
                yaw,
                road_half: Some(5.5),
                ..crossing(kind, 3)
            };
            let want = meshed_mast_centres(&c);
            let got = mast_centres(&c);
            assert!(
                same_pair(got, want),
                "{kind:?} at yaw {yaw}: collider at {got:?}, mast at {want:?}"
            );
        }
    }
}

/// Every form gets two mast colliders — `Lights` included, which is the one
/// that could plausibly be skipped: it has no boom, but it has the same two
/// masts and lamp heads standing on the same verge.
#[test]
fn every_form_has_two_mast_colliders() {
    for kind in [
        CrossingKind::HalfBarrier,
        CrossingKind::MiniBarrier,
        CrossingKind::FullBarrier,
        CrossingKind::Lights,
    ] {
        let c = crossing(kind, 2);
        let [a, b] = mast_centres(&c);
        // Across the road at the verge, along it at the clearance — the two
        // numbers the masts are placed from, in the frame yaw 0 makes the
        // object frame.
        let (_, fallback) = kind.spec();
        let across = fallback + VERGE_M;
        let along = clearance_m(2);
        assert!(
            (a[0] - (c.x + across)).abs() < 1e-4 && (a[1] - (c.z + along)).abs() < 1e-4,
            "{kind:?}: {a:?}"
        );
        assert!(
            (b[0] - (c.x - across)).abs() < 1e-4 && (b[1] - (c.z - along)).abs() < 1e-4,
            "{kind:?}: {b:?}"
        );
    }
}

/// A wider road moves the collider out with the mast — the width is carried
/// through one derivation, so a crossing on an 11 m road cannot get a mast on
/// the verge and a circle in the carriageway.
#[test]
fn the_collider_takes_the_roads_width_too() {
    let narrow = mast_centres(&crossing(CrossingKind::HalfBarrier, 1));
    let wide = mast_centres(&LevelCrossing {
        road_half: Some(5.5),
        ..crossing(CrossingKind::HalfBarrier, 1)
    });
    assert!((narrow[0][0] - 44.0).abs() < 1e-3, "{narrow:?}");
    assert!((wide[0][0] - 46.0).abs() < 1e-3, "{wide:?}");
}

/// The table's rule, and the acceptance test for it: a code nobody wrote a
/// form for places **nothing**, rather than the nearest thing in the table.
/// `furniture`'s module note states why, and this register makes it sharper —
/// half of it is codes for installations that do not exist.
#[test]
fn an_unnamed_code_places_nothing() {
    for code in [
        "WILO", "VKL", "HAVIO", "EBO", "ADOB", "", "   ", "AHOB2", "overweg",
    ] {
        assert_eq!(crossing_kind(code), None, "{code} should draw nothing");
    }
}

/// The two the register uses for "there is nothing here" and "I do not know",
/// which between them are 2,075 of its 3,917 features. Drawing an
/// installation on either is the biggest single lie this layer could tell.
#[test]
fn geen_and_onbekend_place_nothing() {
    assert_eq!(crossing_kind("Geen"), None);
    assert_eq!(crossing_kind("Onbekend"), None);
    assert_eq!(crossing_kind("GEEN"), None);
}

/// The codes that do have a form, so a table edit that drops one is caught.
#[test]
fn the_named_codes_keep_their_forms() {
    assert_eq!(crossing_kind("AHOB"), Some(CrossingKind::HalfBarrier));
    assert_eq!(crossing_kind("AHOB-MINI"), Some(CrossingKind::MiniBarrier));
    assert_eq!(crossing_kind("AOB"), Some(CrossingKind::FullBarrier));
    assert_eq!(crossing_kind("AKI"), Some(CrossingKind::Lights));
    assert_eq!(crossing_kind("ali"), Some(CrossingKind::Lights));
}

/// A boom lies across the **road**, so the yaw has to come from the road —
/// and the register's point is on the rail centreline, so the nearest way to
/// it is always the track.
///
/// This is the one piece of logic in the module that is not borrowed, and its
/// failure mode is a crossing that looks fine from above and has its booms
/// down the middle of the railway. The field here holds exactly the two ways
/// the real case has: a track running east–west and a road crossing it
/// north–south, with the crossing point on the track.
#[test]
fn orientation_comes_from_the_road_not_the_track() {
    let mut field = WayField::empty(400.0);
    // The rail, east–west through the crossing point, and *nearest* to it.
    field.push_line(
        WayFilter::ExcludingRail,
        "rail",
        &[[0.0, 100.0], [400.0, 100.0]],
    );
    // The road, north–south, crossing it at x = 200.
    field.push_line(
        WayFilter::ExcludingRail,
        "residential",
        &[[200.0, 0.0], [200.0, 400.0]],
    );

    let placed = place_crossings_against(
        &field,
        &[CrossingPoint {
            x: 200.0,
            z: 100.0,
            kind: CrossingKind::HalfBarrier,
            tracks: 1,
        }],
    );
    assert_eq!(placed.len(), 1, "the crossing should be placed");

    // Local +z along the road, which runs +z here: yaw 0 (or its half turn).
    let yaw = placed[0].yaw;
    let along = (yaw.sin(), yaw.cos());
    assert!(
        along.0.abs() < 1e-3,
        "the booms took the track's direction, not the road's: yaw {yaw}"
    );
    assert!(along.1.abs() > 0.99, "yaw {yaw} is not along the road");
}

/// The same field with the filter off answers the *track*, which is what
/// makes the test above a test of the filter rather than of the geometry.
#[test]
fn without_the_filter_the_track_wins() {
    let mut field = WayField::empty(400.0);
    field.push_line(WayFilter::All, "rail", &[[0.0, 100.0], [400.0, 100.0]]);
    field.push_line(
        WayFilter::All,
        "residential",
        &[[200.0, 0.0], [200.0, 400.0]],
    );
    let (yaw, _) = field
        .direction_and_width(200.0, 100.0, ORIENT_RADIUS_M)
        .unwrap();
    assert!(
        yaw.sin().abs() > 0.99,
        "expected the east–west track, got yaw {yaw}"
    );
}

/// Every form emits geometry, indexed inside its own vertex list, in the
/// group the furniture shares.
#[test]
fn every_form_meshes() {
    for kind in [
        CrossingKind::HalfBarrier,
        CrossingKind::MiniBarrier,
        CrossingKind::FullBarrier,
        CrossingKind::Lights,
    ] {
        let meshes = build_crossing_meshes(&[crossing(kind, 2)], None);
        assert_eq!(meshes.len(), 1, "{kind:?}");
        assert_eq!(meshes[0].group, SurfaceGroup::Furniture);
        assert!(!meshes[0].indices.is_empty(), "{kind:?}");
        assert_eq!(meshes[0].positions.len(), meshes[0].normals.len());
        assert_eq!(meshes[0].positions.len(), meshes[0].colors.len());
        for i in &meshes[0].indices {
            assert!((*i as usize) < meshes[0].positions.len(), "{kind:?}");
        }
    }
}

/// The byte bound, on the same terms `build_mark_meshes` is held to: a cap
/// splits the cell into pieces, each piece holds at least one crossing, and
/// no piece is empty.
#[test]
fn the_byte_cap_splits_between_crossings() {
    let many: Vec<LevelCrossing> = (0..12)
        .map(|_| crossing(CrossingKind::HalfBarrier, 2))
        .collect();
    let whole = build_crossing_meshes(&many, None);
    assert_eq!(whole.len(), 1);

    let cap = CROSSING_MAX_BYTES * 3;
    let cut = build_crossing_meshes(&many, Some(cap));
    assert!(cut.len() > 1, "the cap did not split anything");
    for mesh in &cut {
        assert!(!mesh.positions.is_empty(), "an empty piece was emitted");
        let bytes = mesh.positions.len() * VERTEX_BYTES + mesh.indices.len() * INDEX_BYTES;
        assert!(bytes <= cap, "a piece overran the cap: {bytes} > {cap}");
    }
    let cut_verts: usize = cut.iter().map(|m| m.positions.len()).sum();
    assert_eq!(cut_verts, whole[0].positions.len(), "the cut lost geometry");
}

/// A cap under one installation's cost still emits it, rather than looping on
/// empty flushes.
#[test]
fn a_cap_below_one_crossing_still_draws_it() {
    let meshes = build_crossing_meshes(&[crossing(CrossingKind::FullBarrier, 4)], Some(1));
    assert_eq!(meshes.len(), 1);
    assert!(!meshes[0].positions.is_empty());
}

/// The same rule again, but through the path production takes: a real tile,
/// whose `streets` features carry their class as a `kind` tag. The test above
/// drives [`WayFilter::admits`] directly and cannot catch a build that never
/// reads the tag at all — which would admit every railway silently.
#[test]
fn the_tile_side_filter_reads_the_kind_tag() {
    use crate::vector_tiles::synth;

    // A tile carrying one railway and nothing else, running east–west across
    // the middle of it. `tile_m` 400 over a 4096 extent.
    let rail_only = synth::tile(&[synth::tagged_line_layer(
        "streets",
        4096,
        ("kind", "rail"),
        &[(0, 1024), (4095, 1024)],
    )]);
    let tile = TileData::parse(&rail_only).expect("the tile parses");
    let excluding = WayField::build_with(&tile, 400.0, WayFilter::ExcludingRail);
    assert!(
        excluding
            .direction_and_width(200.0, 100.0, ORIENT_RADIUS_M)
            .is_none(),
        "the rail was indexed despite the filter"
    );
    let everything = WayField::build_with(&tile, 400.0, WayFilter::All);
    assert!(
        everything
            .direction_and_width(200.0, 100.0, ORIENT_RADIUS_M)
            .is_some(),
        "the unfiltered field lost the way too, so this proves nothing"
    );
}

/// How many booms an installation has comes from its **form**, never from
/// the register's `aantal_ahob`: the Peizerweg crossing is a single-track
/// AHOB and reports 15 of them, so that field is an asset tally rather than a
/// count of what is standing there. [`CrossingPoint`] does not carry it, and
/// two barrier forms with wildly different tallies mesh identically.
#[test]
fn the_boom_count_comes_from_the_form_not_the_register() {
    assert!(CrossingKind::HalfBarrier.boom_span().is_some());
    assert!(CrossingKind::Lights.boom_span().is_none());
    // Same form, same geometry — there is no per-instance boom count to vary.
    let a = build_crossing_meshes(&[crossing(CrossingKind::HalfBarrier, 1)], None);
    let b = build_crossing_meshes(&[crossing(CrossingKind::HalfBarrier, 1)], None);
    assert_eq!(a[0].positions.len(), b[0].positions.len());
    // …and a boom form is strictly more geometry than a lights-only one.
    let lights = build_crossing_meshes(&[crossing(CrossingKind::Lights, 1)], None);
    assert!(a[0].positions.len() > lights[0].positions.len());
}

/// A boom stands **up**, not across.
///
/// This is the assertion the module was missing: a boom lying flat is a
/// crossing that reads as permanently closed to traffic with nothing coming,
/// and it is what shipped — the bands were stacked along local x at a
/// constant `BOOM_Y`, ending within half a metre of the road's centreline.
/// Every existing test passes with the bar in either attitude
/// (`every_form_meshes` counts triangles, `the_byte_cap_splits_between_crossings`
/// counts bytes, `the_boom_count_comes_from_the_form_not_the_register` counts
/// vertices), so nothing but a shot could tell them apart.
///
/// Two facts separate them without having to identify which vertices belong
/// to which box: nothing stands over the carriageway, and the mesh reaches
/// the top of the raised bar in y. A `Lights` crossing tops out at its lamp
/// head, which is what makes the second one a statement about the boom.
#[test]
fn a_raised_boom_stands_clear_of_the_road() {
    for kind in [
        CrossingKind::HalfBarrier,
        CrossingKind::MiniBarrier,
        CrossingKind::FullBarrier,
    ] {
        let (mast_h, road_half) = kind.spec();
        // The reach `push_crossing` derives, on its own terms
        // (`crossings.rs:394-397`): a multiple of the half-carriageway, plus
        // the verge the mast stands back by.
        let reach = kind.boom_span().expect("a barrier form has a boom") * road_half + VERGE_M;
        let c = crossing(kind, 2);
        // Yaw 0, so the object's local x is the world's: `push_box`'s
        // rotation is the identity and the across-road axis is `x - c.x`.
        let meshes = build_crossing_meshes(&[c], None);
        assert_eq!(meshes.len(), 1, "{kind:?}");

        let clear = road_half - 0.25;
        let top = meshes[0]
            .positions
            .iter()
            .fold(f32::MIN, |hi, p| hi.max(p[1]));
        for p in &meshes[0].positions {
            assert!(
                (p[0] - c.x).abs() >= clear,
                "{kind:?}: a vertex at x {} is over the carriageway (clear {clear} m)",
                p[0] - c.x
            );
        }
        assert!(
            top >= BOOM_Y + reach - 0.05,
            "{kind:?}: the mesh tops out at {top} m, below a raised boom's \
                 {} m — the bar is still lying flat",
            BOOM_Y + reach
        );

        // The bar stands flush beside its mast, never inside it. Measured on
        // the +side installation, where inboard is −x: the mast's inner face
        // against the bar's outer face.
        let mx = road_half + VERGE_M;
        let mast_inner = mx - MAST_HALF_W;
        let bar_outer = (mx - (MAST_HALF_W + BOOM_HALF_FACE)) + BOOM_HALF_FACE;
        assert!(
            mast_inner - bar_outer >= -1e-4,
            "{kind:?}: the bar overlaps the mast by {} m",
            bar_outer - mast_inner
        );
        // …and the lamp head, not the boom, is what a `Lights` crossing
        // reaches, so the check above cannot be satisfied by the mast.
        assert!(
            BOOM_Y + reach > mast_h + 0.32,
            "{kind:?}: the boom does not clear the lamp head"
        );
    }
}

/// A crossing with no boom is untouched by the attitude change, pinned as an
/// absolute box rather than against a golden file: two masts and two lamp
/// heads, topping out at the lamp rather than at a bar. It is the control for
/// the test above — a form that reaches only `mast_h + 0.32` in y is what
/// makes "the mesh reaches `BOOM_Y + reach`" a statement about the boom.
#[test]
fn a_lights_crossing_is_two_masts_and_two_lamps() {
    let (mast_h, road_half) = CrossingKind::Lights.spec();
    assert!(CrossingKind::Lights.boom_span().is_none());
    let c = crossing(CrossingKind::Lights, 2);
    let meshes = build_crossing_meshes(&[c], None);
    // Four boxes, 24 vertices each — no boom bands.
    assert_eq!(meshes[0].positions.len(), 4 * 24);

    let mut lo = [f32::MAX; 3];
    let mut hi = [f32::MIN; 3];
    for p in &meshes[0].positions {
        for a in 0..3 {
            lo[a] = lo[a].min(p[a]);
            hi[a] = hi[a].max(p[a]);
        }
    }
    // Across the road, the lamp head's 0.30 m half-width off the mast at
    // `road_half + 0.5`; up, the top of that head; along, the clearance plus
    // the mast's own half-width.
    let across = road_half + VERGE_M + 0.30;
    let along = clearance_m(2) + MAST_HALF_W;
    for (got, want) in [
        (lo[0], c.x - across),
        (hi[0], c.x + across),
        (lo[1], 0.0),
        (hi[1], mast_h + 0.32),
        (lo[2], c.z - along),
        (hi[2], c.z + along),
    ] {
        assert!((got - want).abs() < 1e-4, "bounds moved: {got} != {want}");
    }
}

/// The track count only ever moves the two installations apart, and it is
/// bounded at both ends: zero tracks is a crossing nobody counted, and a
/// number past [`MAX_TRACKS`] would strew masts over a neighbourhood.
#[test]
fn the_track_count_is_bounded_at_both_ends() {
    assert_eq!(clearance_m(0), clearance_m(1));
    assert!(clearance_m(2) > clearance_m(1));
    assert_eq!(clearance_m(u8::MAX), clearance_m(MAX_TRACKS as u8));
    assert!(clearance_m(MAX_TRACKS as u8) < 40.0);
}

/// The whole point of the change: a crossing takes its width off the road it
/// bars, through the path production takes.
///
/// The Peizerweg is OSM `highway = secondary`, which `road_width_m` answers
/// 11.0 m for — the ribbon this client actually draws. Sized for the old
/// tabulated 7 m road, both masts stood ~1.5 m inside the kerb and each boom
/// covered under half the carriageway.
///
/// Driven through `synth` + `WayField::build_with` rather than through
/// `push_line`, so a build that never reads the tags is caught here as
/// `the_tile_side_filter_reads_the_kind_tag` catches it for the filter.
#[test]
fn a_wide_road_gets_a_wide_crossing() {
    use crate::vector_tiles::synth;

    // One `secondary` road running north–south down the middle of a 400 m
    // cell, and the crossing point on it.
    let tile = TileData::parse(&synth::tile(&[synth::tagged_line_layer(
        "streets",
        4096,
        ("kind", "secondary"),
        &[(2048, 0), (2048, 4095)],
    )]))
    .expect("the tile parses");
    let roads = WayField::build_with(&tile, 400.0, WayFilter::ExcludingRail);
    let placed = place_crossings_against(
        &roads,
        &[CrossingPoint {
            x: 200.0,
            z: 100.0,
            kind: CrossingKind::HalfBarrier,
            tracks: 1,
        }],
    );
    assert_eq!(placed.len(), 1);
    assert_eq!(
        placed[0].road_half,
        Some(5.5),
        "the width did not come off the tile"
    );

    // `place_crossings` leaves the yaw sign unresolved, and `across` reads
    // in the object frame, so normalise to yaw 0 — the geometry is the same
    // under a half turn by construction.
    let crossing = LevelCrossing {
        yaw: 0.0,
        ..placed[0]
    };
    let (mast, reach) = across(&crossing);
    assert!(
        (mast - 6.0).abs() < 1e-3,
        "the mast should stand 0.5 m outside an 11 m road's kerb, got {mast}"
    );
    // A half barrier bars the entry half, so the bar is as long as the mast
    // stands out: lowered, its tip would meet the centreline.
    assert!(
        (reach.expect("a half barrier has a boom") - 6.0).abs() < 1e-3,
        "the bar should be 6.0 m long, so it meets the centreline when \
             lowered, got {reach:?}"
    );
}

/// The same derivation the other way round: the commonest small form was
/// sized for a 3.2 m path and stands on cycleways two metres wide.
#[test]
fn a_narrow_road_gets_a_narrow_crossing() {
    let mut field = WayField::empty(400.0);
    field.push_line(
        WayFilter::ExcludingRail,
        "cycleway",
        &[[200.0, 0.0], [200.0, 400.0]],
    );
    let placed = place_crossings_against(
        &field,
        &[CrossingPoint {
            x: 200.0,
            z: 100.0,
            kind: CrossingKind::MiniBarrier,
            tracks: 1,
        }],
    );
    assert_eq!(placed[0].road_half, Some(1.1), "cycleway is 2.2 m wide");
    let (mast, reach) = across(&LevelCrossing {
        yaw: 0.0,
        ..placed[0]
    });
    assert!(
        (mast - 1.6).abs() < 1e-3,
        "the mini form should get narrower, not just the big forms wider: {mast}"
    );
    assert!(
        (reach.expect("a boom") - 1.6).abs() < 1e-3,
        "the bar should shrink with the mast, to 1.6 m: {reach:?}"
    );
}

/// A full barrier bars the **whole** road, so its bar is long enough to
/// reach the far kerb rather than the centreline — the one form whose reach
/// is not the same multiple of the half-width.
#[test]
fn a_full_barrier_reaches_the_far_kerb() {
    let crossing = LevelCrossing {
        road_half: Some(5.5),
        ..crossing(CrossingKind::FullBarrier, 1)
    };
    let (mast, reach) = across(&crossing);
    assert!((mast - 6.0).abs() < 1e-3, "{mast}");
    // 2 × 5.5 + the verge: from a mast 6.0 m out, that is the far kerb at
    // −5.5 m once the bar is lowered.
    assert!(
        (reach.expect("a boom") - 11.5).abs() < 1e-3,
        "the bar should be 11.5 m long, enough to cross the whole road, got {reach:?}"
    );
}

/// Where the field cannot answer a width — an untagged way, a kind
/// `road_class` refuses, a tunnel — every form is placed exactly where it
/// was before the width was carried. This is the compatibility half, and it
/// is what makes the change safe on a tile with no usable `kind` tags at all.
#[test]
fn an_unknown_width_falls_back_to_the_old_numbers() {
    for (kind, mast_x, reach) in [
        (CrossingKind::HalfBarrier, 4.0, Some(4.0)),
        (CrossingKind::MiniBarrier, 2.1, Some(2.1)),
        // 7.5 against the old table's 7.6: the 10 cm is the accepted cost of
        // deriving the reach from the half-width.
        (CrossingKind::FullBarrier, 4.0, Some(7.5)),
        (CrossingKind::Lights, 4.0, None),
    ] {
        let (mast, drawn) = across(&crossing(kind, 1));
        assert!((mast - mast_x).abs() < 1e-3, "{kind:?}: mast at {mast}");
        match reach {
            Some(r) => {
                let drawn = drawn.expect("a boom");
                assert!((drawn - r).abs() < 1e-3, "{kind:?}: bar {drawn} m long");
            }
            None => assert!(drawn.is_none(), "{kind:?} should have no boom"),
        }
    }
}

/// A tunnel and an unclassified kind are both "no width known", not zero — a
/// zero would put both masts on the centreline and collapse the booms.
#[test]
fn a_kind_with_no_road_class_answers_no_width() {
    let mut field = WayField::empty(400.0);
    // Admitted by the filter (it is not rail), refused by `road_class`.
    field.push_line(
        WayFilter::ExcludingRail,
        "elevator",
        &[[200.0, 0.0], [200.0, 400.0]],
    );
    let placed = place_crossings_against(
        &field,
        &[CrossingPoint {
            x: 200.0,
            z: 100.0,
            kind: CrossingKind::HalfBarrier,
            tracks: 1,
        }],
    );
    assert_eq!(placed.len(), 1, "the way still orients the crossing");
    assert_eq!(placed[0].road_half, None);
}

/// A crossing with no road near it is dropped rather than drawn at a guessed
/// angle — the module note's reason, and the thing that keeps the
/// `level_crossings` count honest about what is on screen.
#[test]
fn a_crossing_with_no_road_is_dropped() {
    let mut field = WayField::empty(400.0);
    field.push_line(
        WayFilter::ExcludingRail,
        "rail",
        &[[0.0, 100.0], [400.0, 100.0]],
    );
    let placed = place_crossings_against(
        &field,
        &[CrossingPoint {
            x: 200.0,
            z: 100.0,
            kind: CrossingKind::HalfBarrier,
            tracks: 1,
        }],
    );
    assert!(placed.is_empty());
}
