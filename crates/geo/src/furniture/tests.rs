
use super::*;
use crate::vector_tiles::synth;

/// Every triangle of a box faces the way its normal does, turned or not:
/// counter-clockwise seen from outside, which is what bevy's back-face
/// culling keeps.
#[test]
fn a_box_is_wound_outward() {
    for (sn, cs) in [(0.0, 1.0), (0.6, 0.8), (-1.0, 0.0)] {
        let mut buf = MeshBuf::default();
        let p = part([0.0, 0.5, 0.0], [1.0, 0.5, 0.2], Tone::Roof);
        push_box(&mut buf, 3.0, -2.0, (sn, cs), &p, [1.0; 4]);
        for t in buf.indices.chunks(3) {
            let [a, b, c] = [0, 1, 2].map(|k| buf.positions[t[k] as usize]);
            let (u, v) = (
                [b[0] - a[0], b[1] - a[1], b[2] - a[2]],
                [c[0] - a[0], c[1] - a[1], c[2] - a[2]],
            );
            let x = [
                u[1] * v[2] - u[2] * v[1],
                u[2] * v[0] - u[0] * v[2],
                u[0] * v[1] - u[1] * v[0],
            ];
            let n = buf.normals[t[0] as usize];
            let facing = x[0] * n[0] + x[1] * n[1] + x[2] * n[2];
            assert!(facing > 0.0, "{t:?} faces away from {n:?}");
        }
    }
}

#[test]
fn a_surveyed_object_replaces_its_osm_twin_and_nothing_else() {
    let at = |x: f32, kind| FurnitureInstance {
        x,
        z: 0.0,
        yaw: 0.0,
        kind,
        luma: 1.0,
        paint: palette::FURNITURE,
        over_water: false,
        hull_only: false,
    };
    let surveyed = [at(0.0, FurnitureKind::Bin)];
    let mut pois = vec![
        at(1.5, FurnitureKind::Bin),   // the same bin, traced a little off
        at(6.0, FurnitureKind::Bin),   // the next bin along
        at(0.5, FurnitureKind::Bench), // a bench beside the bin
    ];
    drop_surveyed_twins(&mut pois, &surveyed);
    let left: Vec<(f32, FurnitureKind)> = pois.iter().map(|p| (p.x, p.kind)).collect();
    assert_eq!(
        left,
        [(6.0, FurnitureKind::Bin), (0.5, FurnitureKind::Bench)]
    );
}

/// The forms and the shared placeholder vocabulary are one list seen from two
/// crates, and nothing but this test keeps them equal. A form missing a slug
/// is a placeholder a curator can set and that overrides nothing; a slug with
/// no form is a row the client silently drops.
#[test]
fn every_form_has_a_slug_the_wire_knows() {
    use cartopolis_core::placeholders::PLACEHOLDER_KINDS;
    let ours: std::collections::BTreeSet<&str> =
        ALL_FURNITURE_KINDS.iter().map(|k| k.slug()).collect();
    let theirs: std::collections::BTreeSet<&str> = PLACEHOLDER_KINDS.iter().copied().collect();
    assert_eq!(ours, theirs);
    assert_eq!(
        ours.len(),
        ALL_FURNITURE_KINDS.len(),
        "a slug is duplicated"
    );
}

/// The forms into thirty-two bits (a `u16` until the marina boats made
/// seventeen). The test that fails the day a form too many is added, which
/// is the only warning the mask can give.
#[test]
fn the_mask_holds_every_form() {
    assert!(
        ALL_FURNITURE_KINDS.len() <= 32,
        "FurnitureMask is a u32 and there are now {} forms",
        ALL_FURNITURE_KINDS.len()
    );
    let mut mask = FurnitureMask::NONE;
    assert!(mask.is_empty());
    for kind in ALL_FURNITURE_KINDS {
        assert!(!mask.contains(*kind));
        mask = mask.with(*kind);
        assert!(mask.contains(*kind));
    }
    assert!(!mask.is_empty());
}

#[test]
fn a_slug_round_trips() {
    for kind in ALL_FURNITURE_KINDS {
        assert_eq!(FurnitureKind::from_slug(kind.slug()), Some(*kind));
    }
    assert_eq!(FurnitureKind::from_slug("lamppost"), None);
}

const TILE_M: f32 = 1000.0;

// `extract_furniture` takes a `TileData` because on the worker it shares one
// with every other consumer of the same tile body.
use crate::testdata::groningen as fixture;

#[test]
fn the_fixture_furnishes_a_street() {
    let items = extract_furniture(&fixture(), TILE_M, 7).expect("extract");
    assert!(
        items.len() > 100,
        "a city tile should furnish more than a handful: {}",
        items.len()
    );
    let benches = items
        .iter()
        .filter(|i| i.kind == FurnitureKind::Bench)
        .count();
    assert!(benches > 50, "benches are the bulk of the layer: {benches}");
    // Every piece lands inside the cell it was read from — the tile buffer
    // duplicate would otherwise be drawn by two neighbouring cells at once.
    for i in &items {
        assert!(
            (0.0..TILE_M).contains(&i.x) && (0.0..TILE_M).contains(&i.z),
            "{:?} at ({}, {}) is outside the tile",
            i.kind,
            i.x,
            i.z
        );
    }
}

#[test]
fn indoor_pois_are_left_undrawn() {
    // The layer is mostly shops, doctors and restaurants: points inside
    // buildings we already draw. Standing a box on each would put furniture
    // through the floor of every terrace on the map.
    let items = extract_furniture(&fixture(), TILE_M, 7).expect("extract");
    assert!(
        items.len() < 400,
        "the whole `pois` layer is 467 features; {} of them were furnished",
        items.len()
    );
}

#[test]
fn extraction_is_stable_for_a_cell() {
    let a = extract_furniture(&fixture(), TILE_M, 11).expect("extract");
    let b = extract_furniture(&fixture(), TILE_M, 11).expect("extract");
    assert_eq!(a.len(), b.len());
    for (x, y) in a.iter().zip(&b) {
        assert_eq!(x.kind, y.kind);
        assert!(
            (x.yaw - y.yaw).abs() < 1e-6,
            "a re-streamed cell re-rolled an angle"
        );
    }
}

#[test]
fn benches_line_up_with_the_streets_they_stand_on() {
    // The claim this layer lives or dies on: a bench takes its angle from the
    // nearest path, not from a hash. Measured as "how many benches are within
    // 15° of *some* street direction" — a hashed angle would land ~17% of them
    // there by chance.
    let items = extract_furniture(&fixture(), TILE_M, 3).expect("extract");
    let ways = WayField::build(&fixture(), TILE_M);

    let benches: Vec<_> = items
        .iter()
        .filter(|i| i.kind == FurnitureKind::Bench)
        .collect();
    assert!(benches.len() > 50, "not enough benches to measure");

    // Independently of how the yaw was produced: find the nearest segment by
    // brute force, and check the bench's *long axis* ended up parallel to it.
    // A hashed angle would clear a 15° window about one in six times.
    let mut aligned = 0;
    let mut oriented = 0;
    for b in &benches {
        let Some((_, seg)) = ways
            .segs
            .iter()
            .map(|&(p, q)| (point_seg_dist2(b.x, b.z, p, q), (p, q)))
            .filter(|(d2, _)| *d2 <= ORIENT_RADIUS_M * ORIENT_RADIUS_M)
            .min_by(|a, c| a.0.partial_cmp(&c.0).unwrap())
        else {
            continue;
        };
        oriented += 1;
        // The bench's long axis is local +x, which the yaw maps to this.
        let (sn, cs) = b.yaw.sin_cos();
        let axis = (cs, -sn);
        let way = (seg.1[0] - seg.0[0], seg.1[1] - seg.0[1]);
        let len = (way.0 * way.0 + way.1 * way.1).sqrt();
        // |cos| of the angle between them: 1 is parallel, 0 perpendicular.
        let para = ((axis.0 * way.0 + axis.1 * way.1) / len).abs();
        if para > 0.966 {
            aligned += 1;
        }
    }
    assert!(oriented > 40, "only {oriented} benches had a way in range");
    assert!(
        aligned * 10 > oriented * 9,
        "only {aligned} of {oriented} benches lie along their nearest way"
    );
}

/// Where the object's front (+z) and long axis (+x) end up in world XZ.
fn frame(yaw: f32) -> ((f32, f32), (f32, f32)) {
    let (sn, cs) = yaw.sin_cos();
    ((sn, cs), (cs, -sn))
}

#[test]
fn a_bench_looks_at_the_path_from_either_side_of_it() {
    // One way running east along z = 0. A bench two metres south of it must
    // look north, and one two metres north of it must look south — in both
    // cases with its long axis running east–west, along the way.
    let mut ways = WayField::empty(200.0);
    ways.push([0.0, 0.0], [100.0, 0.0], None);

    let south = ways.facing(50.0, 2.0, ORIENT_RADIUS_M).expect("a path");
    let (front, axis) = frame(south);
    assert!(front.0.abs() < 1e-4 && front.1 < -0.99, "front {front:?}");
    assert!(axis.0.abs() > 0.99 && axis.1.abs() < 1e-4, "axis {axis:?}");

    let north = ways.facing(50.0, -2.0, ORIENT_RADIUS_M).expect("a path");
    let (front, axis) = frame(north);
    assert!(front.0.abs() < 1e-4 && front.1 > 0.99, "front {front:?}");
    assert!(axis.0.abs() > 0.99 && axis.1.abs() < 1e-4, "axis {axis:?}");
}

#[test]
fn facing_is_a_quarter_turn_off_the_way_not_a_half_one() {
    // The bug this module was written around: lining a bench up *with* the
    // street direction points its long axis at the kerb. Whatever the way's
    // bearing, the front must come out perpendicular to it.
    for (dx, dz) in [(1.0, 0.0), (0.0, 1.0), (1.0, 1.0), (-3.0, 1.0), (2.0, -5.0)] {
        // Wide enough that the grid clamp cannot reach the ±150 m
        // segments below; this test is about angles, not bounds.
        let mut ways = WayField::empty(600.0);
        ways.push([-dx * 50.0, -dz * 50.0], [dx * 50.0, dz * 50.0], None);
        let yaw = ways.facing(3.0, 3.0, ORIENT_RADIUS_M).expect("a path");
        let (front, _) = frame(yaw);
        let len = (dx * dx + dz * dz).sqrt();
        let along = (front.0 * dx + front.1 * dz) / len;
        assert!(
            along.abs() < 1e-4,
            "way ({dx}, {dz}): the front has a {along} component along it"
        );
    }
}

/// MVT declares an extent **per layer**, and the way field reads `streets`
/// while its caller is walking `pois`. Handing it the caller's numbers put
/// every street of such a tile at a fraction of its true distance, so a bench
/// lined itself up with whatever happened to land near the wrong position —
/// invisible on Shortbread, where every layer is 4096, and wrong on the first
/// source that is not.
#[test]
fn the_way_field_is_scaled_by_the_streets_layers_own_extent() {
    let bytes = synth::tile(&[
        synth::line_layer("streets", 512, &[(0, 0), (512, 0)]),
        synth::line_layer("pois", 4096, &[(0, 0), (4096, 0)]),
    ]);
    let tile = TileData::parse(&bytes).expect("parse");
    let ways = WayField::build(&tile, 1000.0);

    assert_eq!(ways.segs.len(), 1, "one street segment expected");
    let (a, b) = ways.segs[0];
    assert!(a[0].abs() < 1e-3, "the street starts at {a:?}");
    // The street spans its whole tile, so in metres it must span the whole
    // 1000 m cell. Scaled by the `pois` extent instead it stops at 125 m.
    assert!(
        (b[0] - 1000.0).abs() < 1e-3,
        "the street ends at {b:?}, not at 1000 m"
    );
}

/// The field carries a half-carriageway per segment, and **only for a filter
/// that was already reading the tags**.
///
/// `All` is the furniture's path over every streamed cell and must keep
/// costing no tag lookup per way, which is the rule stated on
/// [`WayFilter::admits_feature`]. So it answers no width at all, and
/// `crossings` — the one caller that wants one — is also the one already
/// paying for the `kind`.
#[test]
fn only_a_filtered_field_carries_a_width() {
    let bytes = synth::tile(&[synth::tagged_line_layer(
        "streets",
        4096,
        ("kind", "secondary"),
        &[(0, 2048), (4095, 2048)],
    )]);
    let tile = TileData::parse(&bytes).expect("parse");

    let filtered = WayField::build_with(&tile, 400.0, WayFilter::ExcludingRail);
    assert_eq!(
        filtered.direction_and_width(200.0, 200.0, 30.0),
        Some((std::f32::consts::FRAC_PI_2, Some(5.5))),
        "a secondary road is 11 m wide, so half of it is 5.5"
    );

    let unfiltered = WayField::build_with(&tile, 400.0, WayFilter::All);
    assert_eq!(
        unfiltered
            .direction_and_width(200.0, 200.0, 30.0)
            .map(|(_, half)| half),
        Some(None),
        "the unfiltered field read a kind tag it must not pay for"
    );
}

/// One wild endpoint has to be enough to drop a segment.
///
/// `mvt-reader` clips its coordinate cursor at `i32::MAX` and never to the
/// layer's extent, so a few bytes of crafted varint put a vertex at ~2.1e9.
/// The old test demanded that *both* endpoints be out of range before it
/// rejected anything, so a segment from a plausible vertex to that one was
/// kept — and the bbox loop in `push` then asked for ~10^14 buckets, each an
/// allocation. The map is drawn from bytes a server we do not run handed us.
#[test]
fn a_way_running_off_to_two_billion_is_dropped() {
    let bytes = synth::tile(&[synth::line_layer(
        "streets",
        4096,
        &[(100, 100), (2_000_000_000, 5)],
    )]);
    let tile = TileData::parse(&bytes).expect("parse");
    let ways = WayField::build(&tile, TILE_M);
    assert!(ways.segs.is_empty(), "a segment running to 2e9 was kept");

    // …and the grid clamp bounds `push` even if a segment ever gets past the
    // window, which is the half that makes this a guarantee rather than a
    // filter that has to be complete.
    let mut field = WayField::empty(TILE_M);
    field.push([0.0, 0.0], [2.1e9, 2.1e9], None);
    assert!(
        field.buckets.len() < 20_000,
        "{} buckets from one segment",
        field.buckets.len()
    );
}

#[test]
fn a_fountain_ignores_the_street_grid() {
    // Radially symmetric forms in open spaces take a hashed angle instead, so
    // a fountain does not square up to whichever side street passes nearest.
    assert!(!FurnitureKind::Fountain.takes_angle_from_path());
    assert!(!FurnitureKind::Artwork.takes_angle_from_path());
    assert!(FurnitureKind::Bench.takes_angle_from_path());
}

#[test]
fn a_seat_is_the_top_of_a_slat_that_is_really_meshed() {
    // The avatar's sitting pose is built around `Seat::height`, and the only
    // thing that makes it land on the timber rather than through it is that
    // this number is the slat's own top. Nothing but this test connects the
    // two: `seat` restates the geometry, so a re-measured bench that did not
    // update it would seat the avatar in mid-air.
    for kind in [FurnitureKind::Bench, FurnitureKind::PicnicTable] {
        let s = seat(kind).expect("a form the prompt offers to sit at");
        for (z, _) in s.rows {
            let slat = parts(kind)
                .iter()
                .find(|p| (p.at[2] - z).abs() < 1e-3 && p.half[0] > s.half_span)
                .unwrap_or_else(|| panic!("{kind:?} has no slat at z {z}"));
            assert!(
                (slat.at[1] + slat.half[1] - s.height).abs() < 1e-3,
                "{kind:?} seat at {} but the slat's top is {}",
                s.height,
                slat.at[1] + slat.half[1]
            );
        }
    }
    assert!(seat(FurnitureKind::Bin).is_none());
}

#[test]
fn every_form_stands_on_the_ground_and_none_of_it_is_a_skyscraper() {
    for (_, _, kind) in FURNITURE_TABLE {
        let ps = parts(*kind);
        assert!(!ps.is_empty(), "{kind:?} has no geometry");
        let lowest = ps
            .iter()
            .map(|p| p.at[1] - p.half[1])
            .fold(f32::MAX, f32::min);
        let highest = ps
            .iter()
            .map(|p| p.at[1] + p.half[1])
            .fold(f32::MIN, f32::max);
        assert!(
            lowest >= -0.01,
            "{kind:?} reaches {lowest} m, below the ground plane"
        );
        assert!(
            lowest < 0.05,
            "{kind:?} floats: its lowest part is at {lowest} m"
        );
        assert!(
            (0.5..4.0).contains(&highest),
            "{kind:?} tops out at {highest} m, which is not street furniture"
        );
    }
}

#[test]
fn a_merged_cell_is_one_mesh_with_matching_attribute_counts() {
    let items = extract_furniture(&fixture(), TILE_M, 5).expect("extract");
    let meshes = build_furniture_meshes(&items, None);
    assert_eq!(meshes.len(), 1, "furniture must merge into one draw");
    let m = &meshes[0];
    assert_eq!(m.group, SurfaceGroup::Furniture);
    assert_eq!(m.positions.len(), m.normals.len());
    assert_eq!(m.positions.len(), m.colors.len());
    assert!(m.uvs.is_empty(), "the group's material has no texture");
    assert_eq!(m.indices.len() % 3, 0);
    for &i in &m.indices {
        assert!((i as usize) < m.positions.len(), "index out of range");
    }
    // 24 verts per box, and every listed form is at least two boxes.
    assert!(m.positions.len() >= items.len() * 48);
}

#[test]
fn empty_input_builds_nothing() {
    assert!(build_furniture_meshes(&[], None).is_empty());
}

#[test]
fn a_rotated_box_keeps_its_size() {
    // The rotation is written out by hand; this is the check that it is a
    // rotation and not a shear.
    let one = [FurnitureInstance {
        x: 0.0,
        z: 0.0,
        yaw: 0.9,
        kind: FurnitureKind::Bin,
        luma: 1.0,
        paint: palette::FURNITURE,
        over_water: false,
        hull_only: false,
    }];
    let m = &build_furniture_meshes(&one, None)[0];
    let extent = |axis: usize| {
        let lo = m.positions.iter().map(|p| p[axis]).fold(f32::MAX, f32::min);
        let hi = m.positions.iter().map(|p| p[axis]).fold(f32::MIN, f32::max);
        hi - lo
    };
    // Every part of a bin is square in xz, so the widest one rotated by θ spans
    // its own width times (|cos θ| + |sin θ|). Derived from the form rather
    // than hardcoded, so re-proportioning a bin does not silently rot this into
    // a test of nothing — the claim under test is the *rotation*.
    let widest = form::BIN
        .iter()
        .map(|p| p.half[0].max(p.half[2]) * 2.0)
        .fold(0.0f32, f32::max);
    let span = widest * (0.9f32.cos().abs() + 0.9f32.sin().abs());
    assert!((extent(0) - span).abs() < 0.01, "x span {}", extent(0));
    assert!((extent(2) - span).abs() < 0.01, "z span {}", extent(2));
    // Height is unaffected by yaw: a shear would show up here and nowhere else.
    let tall = form::BIN
        .iter()
        .map(|p| p.at[1] + p.half[1])
        .fold(f32::MIN, f32::max);
    assert!((extent(1) - tall).abs() < 0.01, "y span {}", extent(1));
}

/// A measured height is what the wall is drawn at, and the nominal 2.0 m is
/// only the answer when the register has nothing to say. Without this the
/// median surveyed `muur` — 0.23 m of garden edging — is a two-metre slab, and
/// every quay wall is the same two-metre slab as well.
#[test]
fn a_measured_boundary_is_drawn_at_its_measured_height() {
    let line = |height_m: Option<f32>| SurveyedLine {
        points: vec![[0.0, 0.0], [10.0, 0.0]],
        kind: LineKind::Wall,
        height_m,
    };
    let top = |l: SurveyedLine| {
        build_line_meshes(&[l], None)
            .first()
            .map(|m| m.positions.iter().fold(f32::MIN, |a, p| a.max(p[1])))
    };
    let nominal = top(line(None)).expect("the nominal form draws");
    assert!(
        (nominal - 0.9).abs() < 0.01,
        "an unmeasured wall keeps the form's height, got {nominal}"
    );
    assert!(
        nominal < 1.2,
        "the nominal wall must stay low enough to see over: at 2 m it walls off \
             every canal in the country, which is what it used to do"
    );
    let measured = top(line(Some(1.29))).expect("a measured wall draws");
    assert!(
        (measured - 1.29).abs() < 0.01,
        "a measured wall stands at its measurement, got {measured}"
    );
}

/// **A bank wall's top meets the ground behind it.** The register measures a
/// wall from its foot, and beside water the foot is the lower side: a 2 m
/// wall along a pond whose water lies 2 m down is flush with the walkway, not
/// two metres above it. On level ground the foot is the ground itself and the
/// wall is unchanged; a sink is never deeper than the wall is tall; and a long
/// segment is cut into pieces so each can find its own foot.
#[test]
fn a_measured_wall_stands_on_its_lower_side() {
    let wall = SurveyedLine {
        points: vec![[0.0, 0.0], [50.0, 0.0]],
        kind: LineKind::Wall,
        height_m: Some(2.06),
    };
    let span = |m: &[SurfaceMesh]| {
        m.iter()
            .flat_map(|m| m.positions.iter())
            .fold((f32::MAX, f32::MIN), |(lo, hi), p| {
                (lo.min(p[1]), hi.max(p[1]))
            })
    };
    let level = build_line_meshes_footed(&[wall.clone()], None, &|_, _| 0.0, &|_, _| 0.0);
    let (lo, hi) = span(&level);
    assert!(
        lo.abs() < 1e-4 && (hi - 2.06).abs() < 1e-4,
        "level ground: {lo}..{hi}"
    );
    let bank = build_line_meshes_footed(&[wall.clone()], None, &|_, _| 2.0, &|_, _| 0.0);
    let (lo, hi) = span(&bank);
    assert!((lo + 2.0).abs() < 1e-4, "the foot is the water side: {lo}");
    assert!((hi - 0.06).abs() < 1e-4, "the top meets the walkway: {hi}");
    let deep = build_line_meshes_footed(&[wall.clone()], None, &|_, _| 9.0, &|_, _| 0.0);
    let (_, hi) = span(&deep);
    assert!(hi.abs() < 1e-4, "never sunk past its own height: {hi}");
    let pieces: usize = bank.iter().map(|m| m.positions.len()).sum::<usize>() / 24;
    assert_eq!(pieces, 9, "50 m in pieces of at most {WALL_PIECE_M} m");
    let unmeasured = SurveyedLine {
        height_m: None,
        ..wall
    };
    let (lo, _) = span(&build_line_meshes_footed(
        &[unmeasured.clone()],
        None,
        &|_, _| 2.0,
        &|_, _| 0.0,
    ));
    assert!(lo.abs() < 1e-4, "a nominal wall has no foot to find");
    // A quay wall is flush with the quay whatever the ground beside it says:
    // the survey line and the map's water rarely agree on where the edge is.
    let quay = SurveyedLine {
        kind: LineKind::Quay,
        height_m: Some(4.25),
        ..unmeasured
    };
    let (lo, hi) = span(&build_line_meshes_footed(
        &[quay.clone()],
        None,
        &|_, _| 0.0,
        &|_, _| 0.0,
    ));
    assert!(hi.abs() < 1e-4 && (lo + 4.25).abs() < 1e-4, "{lo}..{hi}");
    // …and topped by the walk it holds up, where that lies under the ground
    // at its line; the foot still plays no part.
    let (lo, hi) = span(&build_line_meshes_footed(
        &[quay],
        None,
        &|_, _| 9.0,
        &|_, _| 1.5,
    ));
    assert!(
        (hi + 1.5).abs() < 1e-4 && (lo + 5.75).abs() < 1e-4,
        "{lo}..{hi}"
    );
}

/// Below the floor there is no wall to draw. The 3D product drapes its objects
/// on AHN, so a flat one comes back as a few centimetres of ground noise, and
/// drawing that is a strip of z-fighting down the pavement rather than a low
/// wall.
#[test]
fn a_boundary_measured_flat_is_not_drawn() {
    let flat = SurveyedLine {
        points: vec![[0.0, 0.0], [10.0, 0.0]],
        kind: LineKind::Wall,
        height_m: Some(0.08),
    };
    assert!(
        build_line_meshes(&[flat], None).is_empty(),
        "a boundary with no measurable height is skipped, not drawn flat"
    );
}

/// The jet and the basin are the same surveyed kind, and the water is what
/// tells them apart. A basin at the waterline of a pond is what this replaced.
#[test]
fn a_fountain_in_water_is_drawn_as_a_jet() {
    let at = |over_water: bool| FurnitureInstance {
        x: 0.0,
        z: 0.0,
        yaw: 0.0,
        kind: FurnitureKind::Fountain,
        luma: 1.0,
        paint: palette::FURNITURE,
        over_water,
        hull_only: false,
    };
    assert_eq!(
        parts_for(&at(false)).len(),
        form::FOUNTAIN.len(),
        "on a square it is still the basin"
    );
    assert_eq!(
        parts_for(&at(true)).len(),
        form::FOUNTAIN_JET.len(),
        "in a pond it is a jet"
    );
    // The basin is a ring 3 m across; the jet is not. Its widest part must stay
    // under a metre or it reads as a basin that has lost its rim.
    let widest = form::FOUNTAIN_JET
        .iter()
        .fold(0.0f32, |a, p| a.max(p.half[0].max(p.half[2])));
    assert!(widest < 0.5, "the jet is narrow, got half-extent {widest}");
    // And the flag is only ever read for a fountain: a bench does not become a
    // different bench by standing near water.
    let bench = FurnitureInstance {
        kind: FurnitureKind::Bench,
        ..at(true)
    };
    assert_eq!(parts_for(&bench).len(), form::BENCH.len());
}

/// The water test against a real recorded tile, because the part that can be
/// silently wrong is the frame: MVT declares an extent **per layer**, so a
/// point arriving in the `pois` layer's coordinates has to be carried into the
/// water layer's. Shortbread emits 4096 everywhere, which is exactly what makes
/// the mistake invisible — the `WayField` trap in this module's own history is
/// the same assumption, and it put benches at angles taken from streets that
/// were nowhere near them.
#[test]
fn water_is_found_inside_a_real_canal() {
    use crate::testdata;
    let tile = testdata::groningen();

    // A point that is provably inside some water polygon in this tile: walk the
    // water layers, and for each exterior ring try its own vertices' mean. A
    // canal is convex enough that this lands inside for at least one of them.
    let mut inside: Option<(f32, f32)> = None;
    for meta in tile.layers() {
        if !WATER_LAYERS.contains(&meta.name.as_str()) {
            continue;
        }
        let extent = TileData::extent(meta);
        for feature in tile.features(meta) {
            let rings: Vec<&geo_types::Polygon<f32>> = match &feature.geometry {
                geo_types::Geometry::Polygon(p) => vec![p],
                geo_types::Geometry::MultiPolygon(mp) => mp.iter().collect(),
                _ => continue,
            };
            for poly in rings {
                let pts: Vec<(f32, f32)> = poly.exterior().coords().map(|c| (c.x, c.y)).collect();
                if pts.len() < 3 {
                    continue;
                }
                let n = pts.len() as f32;
                let cx = pts.iter().map(|p| p.0).sum::<f32>() / n;
                let cy = pts.iter().map(|p| p.1).sum::<f32>() / n;
                let (u, v) = (cx / extent, cy / extent);
                if (0.0..1.0).contains(&u) && (0.0..1.0).contains(&v) && water_under(&tile, u, v) {
                    inside = Some((u, v));
                    break;
                }
            }
            if inside.is_some() {
                break;
            }
        }
        if inside.is_some() {
            break;
        }
    }
    let (u, v) = inside.expect("Groningen z14 has canals in it");
    assert!(water_under(&tile, u, v), "the point we just found is wet");

    // And the tile is not simply answering yes: most of a city is not water.
    let mut wet = 0;
    let mut total = 0;
    for i in 1..12 {
        for j in 1..12 {
            total += 1;
            wet += usize::from(water_under(&tile, i as f32 / 12.0, j as f32 / 12.0));
        }
    }
    assert!(
        wet < total / 2,
        "{wet} of {total} sample points wet — the frame is probably wrong"
    );
}

/// An operator's model replaces the basin and leaves the jet alone — and the
/// jet must still be *meshed*, because the mask's whole meaning is "a model
/// will stand here instead". Without the second half a fountain placeholder
/// deletes every jet in the country.
#[test]
fn a_jet_refuses_a_placeholder_model_and_keeps_its_geometry() {
    let at = |over_water: bool| FurnitureInstance {
        x: 0.0,
        z: 0.0,
        yaw: 0.0,
        kind: FurnitureKind::Fountain,
        luma: 1.0,
        paint: palette::FURNITURE,
        over_water,
        hull_only: false,
    };
    assert!(at(false).takes_placeholder_model(), "a basin takes a model");
    assert!(!at(true).takes_placeholder_model(), "a jet does not");

    let skip = FurnitureMask::NONE.with(FurnitureKind::Fountain);
    assert!(
        build_furniture_meshes_except(&[at(false)], skip, None).is_empty(),
        "the basin is left to the model"
    );
    assert!(
        !build_furniture_meshes_except(&[at(true)], skip, None).is_empty(),
        "the jet is still drawn"
    );
}

#[test]
fn normals_stay_unit_length_through_the_rotation() {
    let one = [FurnitureInstance {
        x: 3.0,
        z: -2.0,
        yaw: 2.1,
        kind: FurnitureKind::Shelter,
        luma: 1.0,
        paint: palette::FURNITURE,
        over_water: false,
        hull_only: false,
    }];
    let m = &build_furniture_meshes(&one, None)[0];
    for n in &m.normals {
        let len = (n[0] * n[0] + n[1] * n[1] + n[2] * n[2]).sqrt();
        assert!((len - 1.0).abs() < 1e-4, "normal {n:?} has length {len}");
    }
}

#[test]
fn the_fade_walks_every_tone_toward_the_paving() {
    // What the ramp has to buy: by the top of the band each object is much
    // closer in tone to the ground it stands on than it was at the bottom, so
    // hiding the geometry is a non-event rather than a pop. It does not reach
    // zero — see `update_furniture_lod` for why that residue is affordable.
    let ground = srgb_to_linear_rgb(palette::ROAD_FILL);
    let gap = |tone: Tone, factor: f32| {
        let base = paving_base(factor);
        let m = tone.multiplier();
        (0..3)
            .map(|i| (m[i] * base[i] - ground[i]).abs())
            .fold(0.0f32, f32::max)
    };
    for tone in [Tone::Wood, Tone::Metal, Tone::Roof, Tone::Stone] {
        let (near, far) = (gap(tone, 0.0), gap(tone, 1.0));
        assert!(
            far < near,
            "{tone:?} does not close on the paving: {near} → {far}"
        );
    }
    // The base itself lands exactly on the paving at the top of the band, and
    // starts exactly on the group's own colour at the bottom.
    assert_eq!(paving_base(1.0), ground);
    assert_eq!(paving_base(0.0), srgb_to_linear_rgb(palette::FURNITURE));
}

#[test]
fn the_tones_are_muted_against_the_paper() {
    // The palette rule this module inherits: nothing here may be more saturated
    // than the buildings already on the map, or a bench becomes the loudest
    // thing on screen.
    let saturation = |rgb: u32| {
        let c = [
            ((rgb >> 16) & 0xFF) as f32,
            ((rgb >> 8) & 0xFF) as f32,
            (rgb & 0xFF) as f32,
        ];
        let hi = c[0].max(c[1]).max(c[2]);
        let lo = c[0].min(c[1]).min(c[2]);
        if hi <= 0.0 {
            0.0
        } else {
            (hi - lo) / hi
        }
    };
    // Water is exempt: it is the canal colour, which the map already carries.
    for tone in [Tone::Wood, Tone::Metal, Tone::Roof, Tone::Stone] {
        let s = saturation(tone.rgb());
        assert!(
            s <= saturation(palette::TRUNK) + 0.02,
            "{tone:?} at saturation {s} is louder than a tree trunk"
        );
    }
}
