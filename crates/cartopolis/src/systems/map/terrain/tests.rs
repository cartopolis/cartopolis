use super::*;
use cartopolis_geo::ahn::FILL_REACH_M;

/// Every row is visited once, with its own index, whether the grid is big
/// enough to be split across threads or not — including a last band that is
/// shorter than the others.
#[test]
fn for_rows_visits_every_row_once_with_its_index() {
    for (rows, width) in [(3, 5), (257, 3), (1001, 7), (2049, 2)] {
        let mut grid = vec![usize::MAX; rows * width];
        for_rows(&mut grid, width, |z, row| {
            for (x, slot) in row.iter_mut().enumerate() {
                assert_eq!(*slot, usize::MAX, "row {z} visited twice");
                *slot = z * width + x;
            }
        });
        assert!(
            grid.iter().enumerate().all(|(i, &v)| v == i),
            "{rows}x{width}"
        );
    }
}

/// A field of `n`² samples centred at `centre`, each the height of a fixed
/// function at its own world place — what two bakes over the same tiles give.
fn field_at(centre: Vec2, n: usize, bump: Option<Vec2>) -> TerrainSnapshot {
    let half = TERRAIN_HALF_M;
    let step = half * 2.0 / (n - 1) as f32;
    let mut samples = Vec::with_capacity(n * n);
    for z in 0..n {
        for x in 0..n {
            let (wx, wz) = (
                centre.x - half + x as f32 * step,
                centre.y - half + z as f32 * step,
            );
            let mut h = (wx * 0.013).sin() * 3.0 + (wz * 0.007).cos() * 2.0;
            if bump.is_some_and(|b| (b.x - wx).abs() < 60.0 && (b.y - wz).abs() < 60.0) {
                h += 1.0;
            }
            samples.push(h);
        }
    }
    TerrainSnapshot {
        samples: std::sync::Arc::new(samples),
        half_m: half,
        grid: n,
        centre,
        floors: std::sync::Arc::default(),
        banks: std::sync::Arc::default(),
    }
}

#[test]
fn a_field_that_moved_by_whole_samples_agrees_about_the_ground_it_shares() {
    let (lo, hi) = (Vec2::new(0.0, 0.0), Vec2::new(1_400.0, 1_400.0));
    let a = field_at(Vec2::ZERO, 129, None);
    let b = field_at(Vec2::new(FOLLOW_LATTICE_M, -FOLLOW_LATTICE_M), 129, None);
    assert_eq!(a.digest_over(lo, hi), b.digest_over(lo, hi));
    // …and not about ground that is different.
    let c = field_at(
        Vec2::new(FOLLOW_LATTICE_M, 0.0),
        129,
        Some(Vec2::new(700.0, 700.0)),
    );
    assert_ne!(a.digest_over(lo, hi), c.digest_over(lo, hi));
}

/// Build a coarse-shaped snapshot out of a height function, for the refinement
/// tests below. Small on purpose — 6.4 km at the real spacing is four million
/// samples and these tests care about one wall.
/// A canal 20 m wide running north–south through a street at +2 m, carved to
/// a −3.5 m bed, on a 3.125 m field — the desktop's spacing.
fn canal() -> (
    TerrainSnapshot,
    crate::systems::water_levels::WaterLevelField,
) {
    let water = crate::systems::water_levels::WaterLevelField::build_for_test(
        -3.0,
        &[
            [-10.0, -300.0],
            [10.0, -300.0],
            [10.0, 300.0],
            [-10.0, 300.0],
        ],
    );
    let mut field = snapshot_of(100.0, 3.125, |x, _| if x.abs() < 10.0 { -3.5 } else { 2.0 });
    field.banks = Arc::new(bank_flags(field.grid, field.half_m, field.centre, &water));
    (field, water)
}

/// The street reads dry ground right up to the quay, the canal side reads the
/// bed right up to it, and both are the field itself away from the water.
#[test]
fn the_street_is_level_to_the_quay_and_the_canal_is_level_to_its_wall() {
    let (field, _) = canal();
    // Between the last sample in the canal (x = 9.375) and the first on the
    // street (12.5) the carved field is a ramp — the sag this exists to remove.
    assert!(field.at(10.5, 0.0) < 0.0, "the carve reaches past the quay");
    for x in [10.0, 10.5, 12.0, 14.0] {
        assert!(
            (field.land_at(x, 0.0) - 2.0).abs() < 1e-4,
            "street at x = {x}: {}",
            field.land_at(x, 0.0)
        );
    }
    // At the drawn edge the canal side is still the bed, so a sheet at the
    // registered level is clear of the basemap all the way to its wall.
    assert!((field.sunk_at(10.0, 0.0) + 3.5).abs() < 1e-4);
    for x in [-60.0, 40.0, 90.0] {
        assert_eq!(field.land_at(x, 5.0), field.at(x, 5.0));
        assert_eq!(field.sunk_at(x, 5.0), field.at(x, 5.0));
    }
    let mut x = -99.0;
    while x < 99.0 {
        assert!(
            field.sunk_at(x, 1.0) <= field.at(x, 1.0) + 1e-5,
            "sunk above at x = {x}"
        );
        x += 0.7;
    }
}

/// A point consumer stands where the sheets are drawn: on the street right up
/// to the quay, not in the carve the register's ring reaches under it — with the
/// half-metre tier on, which is carved too. The avatar and the grass stood
/// 1.5 m under the study pond's lawn that way.
#[test]
fn the_footing_beside_the_water_is_the_land_the_sheets_stand_on() {
    let (coarse, _) = canal();
    let fine = snapshot_of(100.0, 0.5, |x, _| if x.abs() < 10.0 { -3.5 } else { 2.3 });
    let field = TerrainField {
        ground: coarse,
        fine,
        ..Default::default()
    };
    assert!(field.fine_ready());
    for x in [10.0, 10.5, 12.0] {
        let h = field.ground_at(x, 0.0);
        assert!((h - 2.0).abs() < 1e-3, "footing at x = {x}: {h}");
    }
    // Away from the water too it is the land the sheets stand on, not the
    // half-metre tier, on which nothing is drawn.
    let h = field.ground_at(25.0, 0.0);
    assert!((h - 2.0).abs() < 1e-3, "away from the bank: {h}");
}

/// The street beside the water is the dry ground next to it, not a bank
/// further off: a street at 2 m along the quay and a lawn falling to -1 m two
/// spacings behind it — a canal bridge's end with the bank beside it.
#[test]
fn the_street_at_the_quay_is_not_averaged_with_the_bank_behind_it() {
    let water = crate::systems::water_levels::WaterLevelField::build_for_test(
        -3.0,
        &[
            [-10.0, -300.0],
            [10.0, -300.0],
            [10.0, 300.0],
            [-10.0, 300.0],
        ],
    );
    let mut field = snapshot_of(100.0, 3.125, |x, _| {
        if x.abs() < 10.0 {
            -3.5
        } else if x < 16.0 {
            2.0
        } else {
            -1.0
        }
    });
    field.banks = Arc::new(bank_flags(field.grid, field.half_m, field.centre, &water));
    for x in [10.0, 11.0, 12.5] {
        let h = field.land_at(x, 0.0);
        assert!((h - 2.0).abs() < 1e-4, "street at x = {x}: {h}");
    }
}

/// The quay's line is by the water, the street a few samples back is not.
#[test]
fn the_quay_is_by_the_water_and_the_street_behind_it_is_not() {
    let (field, _) = canal();
    for x in [-10.0, 0.0, 10.0, 11.0] {
        assert!(field.by_the_water(x, 0.0), "x = {x}");
    }
    for x in [20.0, -25.0, 60.0] {
        assert!(!field.by_the_water(x, 0.0), "x = {x}");
    }
    let dry = snapshot_of(50.0, 3.125, |_, _| 1.0);
    assert!(!dry.by_the_water(0.0, 0.0));
}

/// With nothing registered, both readings are the field to the bit.
#[test]
fn without_water_the_street_and_the_canal_side_are_the_field() {
    let field = snapshot_of(50.0, 3.125, |x, z| (x * 0.1).sin() + z * 0.02);
    assert!(field.banks.is_empty());
    for (x, z) in [(0.3, 1.7), (-20.0, 33.3), (41.0, -12.5)] {
        assert_eq!(field.land_at(x, z), field.at(x, z));
        assert_eq!(field.sunk_at(x, z), field.at(x, z));
    }
}

fn snapshot_of(half: f32, sample: f32, height: impl Fn(f32, f32) -> f32) -> TerrainSnapshot {
    let n = (half * 2.0 / sample) as usize + 1;
    let mut samples = Vec::with_capacity(n * n);
    for zi in 0..n {
        for xi in 0..n {
            samples.push(height(
                -half + xi as f32 * sample,
                -half + zi as f32 * sample,
            ));
        }
    }
    let floors = build_floors(&samples, n, sample);
    TerrainSnapshot {
        samples: std::sync::Arc::new(samples),
        half_m: half,
        grid: n,
        centre: Vec2::ZERO,
        floors: std::sync::Arc::new(floors),
        banks: std::sync::Arc::default(),
    }
}

/// One 12.5 m quad, refined, and how far the drawn surface ends up from the
/// measured ground under it.
fn worst_error(field: &TerrainSnapshot, crease: bool) -> (f32, usize) {
    // A single quad straddling the step, in the frame the field is in.
    let mut positions = vec![
        [-6.25, 0.0, -6.25],
        [6.25, 0.0, -6.25],
        [6.25, 0.0, 6.25],
        [-6.25, 0.0, 6.25],
    ];
    let mut indices = vec![0, 1, 2, 0, 2, 3];
    refine_for_terrain(
        &mut positions,
        &mut indices,
        &mut vec![],
        &mut vec![],
        &mut vec![],
        &mut vec![],
        MAX_GROUND_EDGE_M,
        crease.then_some(Crease {
            field,
            origin: [0.0, 0.0],
        }),
    );
    for p in positions.iter_mut() {
        p[1] = field.at(p[0], p[2]);
    }
    // Sample the drawn surface at the centre of every triangle — that is where a
    // flat chord is furthest from a step it straddles — and compare with the
    // ground there.
    let mut worst = 0.0f32;
    for tri in indices.chunks_exact(3) {
        let (a, b, c) = (
            positions[tri[0] as usize],
            positions[tri[1] as usize],
            positions[tri[2] as usize],
        );
        let cx = (a[0] + b[0] + c[0]) / 3.0;
        let cz = (a[2] + b[2] + c[2]) / 3.0;
        let drawn = (a[1] + b[1] + c[1]) / 3.0;
        worst = worst.max((drawn - field.at(cx, cz)).abs());
    }
    (worst, indices.len() / 3)
}

/// A square of ground as an `n`×`n` grid of quads, refined and lifted onto the
/// field. `unshared` is the layout `flat_fill` and `road_stitch` emit: three
/// fresh vertices per triangle, no sharing anywhere.
///
/// A grid rather than one quad because the thing under test is what happens to
/// an **interior** edge — the edge two triangles own between them — and a single
/// quad has exactly one of those.
fn drawn_grid(
    field: &TerrainSnapshot,
    half: f32,
    n: usize,
    unshared: bool,
) -> (Vec<[f32; 3]>, Vec<u32>) {
    let step = half * 2.0 / n as f32;
    let corner = |i: usize, j: usize| [-half + i as f32 * step, 0.0, -half + j as f32 * step];
    let (mut positions, mut indices) = (Vec::new(), Vec::new());
    if unshared {
        for j in 0..n {
            for i in 0..n {
                let (a, b, c, d) = (
                    corner(i, j),
                    corner(i + 1, j),
                    corner(i + 1, j + 1),
                    corner(i, j + 1),
                );
                let base = positions.len() as u32;
                positions.extend_from_slice(&[a, b, c, a, c, d]);
                indices.extend((0..6).map(|k| base + k));
            }
        }
    } else {
        for j in 0..=n {
            for i in 0..=n {
                positions.push(corner(i, j));
            }
        }
        let at = |i: usize, j: usize| (j * (n + 1) + i) as u32;
        for j in 0..n {
            for i in 0..n {
                indices.extend_from_slice(&[at(i, j), at(i + 1, j), at(i + 1, j + 1)]);
                indices.extend_from_slice(&[at(i, j), at(i + 1, j + 1), at(i, j + 1)]);
            }
        }
    }
    refine_for_terrain(
        &mut positions,
        &mut indices,
        &mut vec![],
        &mut vec![],
        &mut vec![],
        &mut vec![],
        MAX_GROUND_EDGE_M,
        Some(Crease {
            field,
            origin: [0.0, 0.0],
        }),
    );
    for p in positions.iter_mut() {
        p[1] = field.at(p[0], p[2]);
    }
    (positions, indices)
}

/// Plan-grid buckets, so the two layout tests below look only at what is near
/// a point or an edge. Scanning every triangle for each of 29k probes was
/// 7.7 s of the client suite's 7.9 s: this module is built at `opt-level = 1`.
struct PlanGrid {
    lo: [f32; 2],
    cell: f32,
    cols: usize,
    rows: usize,
    buckets: Vec<Vec<u32>>,
}

impl PlanGrid {
    fn over(positions: &[[f32; 3]], cell: f32) -> Self {
        let (mut lo, mut hi) = ([f32::MAX; 2], [f32::MIN; 2]);
        for p in positions {
            lo = [lo[0].min(p[0]), lo[1].min(p[2])];
            hi = [hi[0].max(p[0]), hi[1].max(p[2])];
        }
        let cols = ((hi[0] - lo[0]) / cell) as usize + 1;
        let rows = ((hi[1] - lo[1]) / cell) as usize + 1;
        Self {
            lo,
            cell,
            cols,
            rows,
            buckets: vec![Vec::new(); cols * rows],
        }
    }

    /// The buckets under a plan box, clamped to the grid.
    fn cells(&self, min: [f32; 2], max: [f32; 2]) -> impl Iterator<Item = usize> + '_ {
        let at = |v: f32, lo: f32, n: usize| {
            (((v - lo) / self.cell).floor().max(0.0) as usize).min(n - 1)
        };
        let (c0, c1) = (
            at(min[0], self.lo[0], self.cols),
            at(max[0], self.lo[0], self.cols),
        );
        let (r0, r1) = (
            at(min[1], self.lo[1], self.rows),
            at(max[1], self.lo[1], self.rows),
        );
        (r0..=r1).flat_map(move |r| (c0..=c1).map(move |c| r * self.cols + c))
    }

    fn insert(&mut self, min: [f32; 2], max: [f32; 2], item: u32) {
        let cells: Vec<usize> = self.cells(min, max).collect();
        for k in cells {
            self.buckets[k].push(item);
        }
    }
}

/// The height of one triangle at a plan position, or `None` off it.
fn height_in(positions: &[[f32; 3]], tri: &[u32], x: f32, z: f32) -> Option<f32> {
    let (a, b, c) = (
        positions[tri[0] as usize],
        positions[tri[1] as usize],
        positions[tri[2] as usize],
    );
    let det = (b[2] - c[2]) * (a[0] - c[0]) + (c[0] - b[0]) * (a[2] - c[2]);
    if det.abs() < 1e-12 {
        return None;
    }
    let l0 = ((b[2] - c[2]) * (x - c[0]) + (c[0] - b[0]) * (z - c[2])) / det;
    let l1 = ((c[2] - a[2]) * (x - c[0]) + (a[0] - c[0]) * (z - c[2])) / det;
    let l2 = 1.0 - l0 - l1;
    (l0 >= -1e-4 && l1 >= -1e-4 && l2 >= -1e-4).then(|| l0 * a[1] + l1 * b[1] + l2 * c[1])
}

/// A triangulated surface that answers its height at a plan position, or
/// `None` off it.
///
/// Each triangle is filed under every bucket its box touches, padded well past
/// the barycentric tolerance (1e-4 of an edge, under a millimetre here), so any
/// triangle a scan of the whole mesh would accept is in the probe's bucket; and
/// buckets fill in index order, so the first hit is the one that scan found.
struct Surface<'a> {
    positions: &'a [[f32; 3]],
    indices: &'a [u32],
    grid: PlanGrid,
}

impl<'a> Surface<'a> {
    fn new(positions: &'a [[f32; 3]], indices: &'a [u32]) -> Self {
        const PAD: f32 = 0.01;
        let mut grid = PlanGrid::over(positions, 2.0);
        for (t, tri) in indices.chunks_exact(3).enumerate() {
            let (mut min, mut max) = ([f32::MAX; 2], [f32::MIN; 2]);
            for &i in tri {
                let p = positions[i as usize];
                min = [min[0].min(p[0]), min[1].min(p[2])];
                max = [max[0].max(p[0]), max[1].max(p[2])];
            }
            grid.insert(
                [min[0] - PAD, min[1] - PAD],
                [max[0] + PAD, max[1] + PAD],
                t as u32,
            );
        }
        Self {
            positions,
            indices,
            grid,
        }
    }

    fn at(&self, x: f32, z: f32) -> Option<f32> {
        let k = self.grid.cells([x, z], [x, z]).next()?;
        self.grid.buckets[k]
            .iter()
            .find_map(|&t| height_in(self.positions, &self.indices[t as usize * 3..][..3], x, z))
    }
}

/// **A vertex layout may not change the shape of the ground.**
///
/// `flat_fill` gives the surveyed paving and the crop parcels three fresh
/// vertices per triangle, and `road_stitch` re-emits every ribbon its junction
/// cut touched the same way. Before 2026-09-18 the refinement keyed its split
/// decision on the vertex index pair, so an interior edge of one of those meshes
/// was two separate edges: tested twice, charged to `CREASE_VERTEX_BUDGET`
/// twice, and answerable differently for its two owners. A sheet drawn with
/// unshared vertices therefore came out a different shape from the identical
/// sheet drawn with shared ones — which is half of why the paving and the
/// carriageway tore through each other on a slope, and it is invisible to every
/// test that refines one mesh and measures it against the field.
///
/// **The budget is what makes this visible, so the test has to spend it.** With
/// room to spare, both owners of a doubly-charged edge reach the same answer and
/// the two layouts agree; the divergence is the moment `CREASE_VERTEX_BUDGET`
/// runs out *between* them, after which one owner has a midpoint and the other
/// does not. An unshared sheet also burns the budget at twice the rate, so it
/// stops refining sooner and over different ground — the measured "about a
/// quarter of a dense cell's paving gets no crease refinement, in an arbitrary
/// spatial order".
#[test]
fn a_sheet_draws_the_same_ground_whether_or_not_its_vertices_are_shared() {
    // Ground that creases at every sample, so every edge has sag and the budget
    // is the only thing that can stop the refinement.
    let field = snapshot_of(512.0, TERRAIN_SAMPLE_M, |x, z| {
        ((x / TERRAIN_SAMPLE_M) as i32 % 2) as f32 * 1.5
            + ((z / TERRAIN_SAMPLE_M) as i32 % 2) as f32 * 1.5
    });
    let (half, n) = (256.0f32, 96);
    let (sp, si) = drawn_grid(&field, half, n, false);
    let (up, ui) = drawn_grid(&field, half, n, true);
    let (shared, unshared) = (Surface::new(&sp, &si), Surface::new(&up, &ui));

    let mut worst = 0.0f32;
    let mut compared = 0;
    for i in 0..=120 {
        for j in 0..=120 {
            let x = -half + 0.5 + i as f32 * (half * 2.0 - 1.0) / 120.0;
            let z = -half + 0.5 + j as f32 * (half * 2.0 - 1.0) / 120.0;
            if let (Some(a), Some(b)) = (shared.at(x, z), unshared.at(x, z)) {
                compared += 1;
                worst = worst.max((a - b).abs());
            }
        }
    }
    assert!(compared > 10_000, "sampled too little of the square");
    assert!(
        worst < 1e-4,
        "the same ground came out {worst} m apart depending on whether the \
             vertices were shared, over {compared} sampled points"
    );
}

/// And the refined mesh may not contain a T-junction, whatever the layout.
///
/// A vertex lying in the middle of another triangle's edge is a crack you can
/// see the sky through once the ground tilts. The per-edge design prevents it by
/// construction; keying the decision on the index pair quietly reintroduced it
/// for the unshared meshes, because the two owners of one geometric edge could
/// be answered differently.
#[test]
fn refining_unshared_vertices_leaves_no_t_junction() {
    let field = snapshot_of(512.0, TERRAIN_SAMPLE_M, |x, z| {
        ((x / TERRAIN_SAMPLE_M) as i32 % 2) as f32 * 1.5
            + ((z / TERRAIN_SAMPLE_M) as i32 % 2) as f32 * 1.5
    });
    let (positions, indices) = drawn_grid(&field, 48.0, 16, true);
    // Every vertex filed where it stands. One the check below would flag lies
    // within 1 mm of the edge, so it is in a bucket under the edge's box padded
    // by more than that: every pair that could fail is still compared, without
    // comparing every edge with every vertex (1.6 s).
    let mut near = PlanGrid::over(&positions, 2.0);
    for (v, p) in positions.iter().enumerate() {
        near.insert([p[0], p[2]], [p[0], p[2]], v as u32);
    }
    const PAD: f32 = 0.01;
    for tri in indices.chunks_exact(3) {
        for k in 0..3 {
            let (a, b) = (
                positions[tri[k] as usize],
                positions[tri[(k + 1) % 3] as usize],
            );
            let (dx, dz) = (b[0] - a[0], b[2] - a[2]);
            let len_sq = dx * dx + dz * dz;
            if len_sq < 1e-9 {
                continue;
            }
            let min = [a[0].min(b[0]) - PAD, a[2].min(b[2]) - PAD];
            let max = [a[0].max(b[0]) + PAD, a[2].max(b[2]) + PAD];
            let candidates = near
                .cells(min, max)
                .flat_map(|c| &near.buckets[c])
                .map(|&v| positions[v as usize]);
            for p in candidates {
                let t = ((p[0] - a[0]) * dx + (p[2] - a[2]) * dz) / len_sq;
                if !(0.02..=0.98).contains(&t) {
                    continue;
                }
                let off = (p[0] - (a[0] + dx * t)).hypot(p[2] - (a[2] + dz * t));
                assert!(
                    off > 1e-3,
                    "vertex {p:?} sits inside the edge {a:?}–{b:?}: a T-junction"
                );
            }
        }
    }
}

/// The whole point of the crease rule, as a number.
///
/// A 1.7 m quay wall inside a 12.5 m quad: the long-edge rule has nothing to say
/// (the quad is already at the limit), so the chord averages straight across the
/// step and the wall is drawn as a ramp. The crease rule splits it because the
/// ground under the midpoint is nowhere near the chord.
#[test]
fn a_step_inside_one_quad_is_refined_only_by_the_crease_rule() {
    // A wall running north-south at x = 1.2 m, 1.7 m high — the study pond
    // quay wall's measured face, at a spacing the coarse field can carry.
    let field = snapshot_of(
        64.0,
        TERRAIN_SAMPLE_M,
        |x, _z| if x > 1.2 { 1.7 } else { 0.0 },
    );

    let (flat_err, flat_tris) = worst_error(&field, false);
    let (crease_err, crease_tris) = worst_error(&field, true);

    assert!(
        flat_err > 0.5,
        "without the crease rule the step should be badly averaged, got {flat_err} m"
    );
    assert!(
        crease_err < flat_err * 0.5,
        "the crease rule must at least halve the error: {flat_err} m -> {crease_err} m"
    );
    assert!(
        crease_tris > flat_tris,
        "and it pays for it in triangles: {flat_tris} -> {crease_tris}"
    );
}

/// And it must cost nothing where there is nothing to resolve, or every flat
/// Dutch cell — which is most of them — pays for a wall it does not have.
#[test]
fn flat_ground_is_not_refined_by_the_crease_rule() {
    let level = snapshot_of(64.0, TERRAIN_SAMPLE_M, |_, _| 0.0);
    assert_eq!(
        worst_error(&level, true).1,
        worst_error(&level, false).1,
        "level ground must produce the same mesh with the crease rule on"
    );
    // A gentle even slope is not a crease either: a chord across a plane is the
    // plane. This is the case that separates "splits where the ground is not
    // flat" from "splits where the ground is not *straight*".
    let ramp = snapshot_of(64.0, TERRAIN_SAMPLE_M, |x, _| x * 0.10);
    assert_eq!(
        worst_error(&ramp, true).1,
        worst_error(&ramp, false).1,
        "an even 10 % slope has no sag and must not be refined"
    );
}

/// The bound that keeps a crease-everywhere cell from reintroducing the upload
/// burst `MAX_GROUND_EDGE_M` exists to avoid.
#[test]
fn the_crease_rule_stops_at_its_budget() {
    // Ground that is nothing but steps, so every edge at every level has sag.
    let field = snapshot_of(512.0, TERRAIN_SAMPLE_M, |x, z| {
        ((x / TERRAIN_SAMPLE_M) as i32 % 2) as f32 * 1.5
            + ((z / TERRAIN_SAMPLE_M) as i32 % 2) as f32 * 1.5
    });
    let mut positions = vec![
        [-512.0, 0.0, -512.0],
        [512.0, 0.0, -512.0],
        [512.0, 0.0, 512.0],
        [-512.0, 0.0, 512.0],
    ];
    let mut indices = vec![0, 1, 2, 0, 2, 3];
    let before = positions.len();
    refine_for_terrain(
        &mut positions,
        &mut indices,
        &mut vec![],
        &mut vec![],
        &mut vec![],
        &mut vec![],
        MAX_GROUND_EDGE_M,
        Some(Crease {
            field: &field,
            origin: [0.0, 0.0],
        }),
    );
    // The long-edge rule is unbounded by design (it is bounded by the polygon),
    // so the assertion is on the crease rule's share: it may not add more than
    // its budget on top of whatever the long-edge rule asked for.
    let added = positions.len() - before;
    assert!(
        added <= cartopolis_geo::terrain_refine::CREASE_VERTEX_BUDGET * 2,
        "the crease rule ran away: {added} vertices added"
    );
    // And it is deterministic: the same cell rebuilt must be the same mesh, or
    // the ground twitches every time a cell is retorn.
    let mut again = vec![
        [-512.0, 0.0, -512.0],
        [512.0, 0.0, -512.0],
        [512.0, 0.0, 512.0],
        [-512.0, 0.0, 512.0],
    ];
    let mut again_idx = vec![0, 1, 2, 0, 2, 3];
    refine_for_terrain(
        &mut again,
        &mut again_idx,
        &mut vec![],
        &mut vec![],
        &mut vec![],
        &mut vec![],
        MAX_GROUND_EDGE_M,
        Some(Crease {
            field: &field,
            origin: [0.0, 0.0],
        }),
    );
    assert_eq!(again_idx, indices, "a rebuilt cell must be the same mesh");
}

/// The fine bake replaces a million `wgs84_to_rd` evaluations with one affine map
/// fitted about the box centre. That is only allowed while the error stays far
/// below the sample spacing it is addressing — so measure it, at four corners of
/// the country, rather than trusting that a polynomial "is smooth".
///
/// A failure here does not look like a failure: every sample would land a little
/// off the ground it claims to measure, which reads as a slightly wrong height and
/// nothing else.
/// **The harness must resolve the ground as finely as this does.**
///
/// `carto-view` sizes its one AHN request off `cartopolis_geo::ahn::SAMPLE_M`,
/// and a harness on a coarser raster does not draw a smoother version of the
/// same city — it draws a different one. Asked for 5.73 m where this bakes
/// 3.125 m, it aliased the Groningen ring's trench into folds and creases the
/// client does not have, and those read as a fault in the map (2026-09-21).
///
/// So the two numbers are one number. Change the box or the grid here and this
/// fails until the shared constant is changed with it.
#[test]
fn the_harness_resolves_the_ground_as_finely_as_the_client() {
    assert!(
        (TERRAIN_SAMPLE_M - cartopolis_geo::ahn::SAMPLE_M).abs() < 1e-6,
        "the client bakes {TERRAIN_SAMPLE_M} m and the harness asks for {} m",
        cartopolis_geo::ahn::SAMPLE_M
    );
}

#[test]
fn the_affine_rd_map_is_exact_enough_for_half_metre_samples() {
    // Comfortably under FINE_SAMPLE_M (~0.5 m); measured worst case is ~15 mm.
    const TOLERANCE_M: f64 = 0.1;
    for anchor in [
        (53.2194, 6.5665, 14u32),
        (52.0907, 5.1214, 14),
        (51.4400, 5.4800, 14),
        (53.4400, 6.9000, 14),
    ] {
        let centre = Vec2::new(1_500.0, -900.0);
        let map = RdAffine::fit(anchor, centre);
        let mut worst: f64 = 0.0;
        for dz in [-FINE_HALF_M, -128.0, 0.0, 128.0, FINE_HALF_M] {
            for dx in [-FINE_HALF_M, -128.0, 0.0, 128.0, FINE_HALF_M] {
                let (lat, lng) = world_to_lat_lng(
                    anchor,
                    f64::from(centre.x) + f64::from(dx),
                    f64::from(centre.y) + f64::from(dz),
                );
                let exact = wgs84_to_rd(lat, lng);
                let approx = map.at(f64::from(dx), f64::from(dz));
                worst = worst.max((exact[0] - approx[0]).hypot(exact[1] - approx[1]));
            }
        }
        assert!(
            worst < TOLERANCE_M,
            "affine RD map is {worst:.3} m off at {anchor:?}; the fine tier samples \
                 every {FINE_SAMPLE_M} m, so this is no longer a shortcut"
        );
    }
}

/// The sliding window must be the same window the re-summation formed.
///
/// `encode_slope`'s box blur went from 25 taps a texel to two, which is a rewrite
/// of the arithmetic and not just of the loop — clamp-to-edge at both ends, a
/// running sum that survives 1024 add/subtract steps, and a vertical pass that
/// walks rows while holding one accumulator per column. Any of those getting the
/// index by one is a residual that is wrong everywhere and looks like lighting.
///
/// The oracle is the direct form. The output is `Rg8Snorm`, so the comparison is
/// on the quantised byte: a running sum accumulates float error the re-summation
/// does not, and what has to hold is that it stays inside a quantisation step
/// (1/127 of the slope range), not that the two agree bit for bit.
#[test]
fn the_running_box_blur_matches_summing_the_window_per_texel() {
    // Not smooth: a residual filter's job is the high-frequency part, so the test
    // field has to have one. Deterministic, and with a plateau, a cliff and a
    // ripple so the clamped edges are exercised from both sides.
    const N: usize = 96;
    let mut samples = vec![0.0f32; N * N];
    for z in 0..N {
        for x in 0..N {
            let (fx, fz) = (x as f32, z as f32);
            let ripple = ((fx * 0.7).sin() + (fz * 1.3).cos()) * 0.35;
            let cliff = if x > N / 2 { 4.0 } else { 0.0 };
            let slope = fz * 0.05;
            samples[z * N + x] = ripple + cliff + slope;
        }
    }
    let got = encode_slope(&samples, N, FINE_SAMPLE_M);

    // The direct form, exactly as it was written before the slide.
    let mut gx = vec![0.0f32; N * N];
    let mut gz = vec![0.0f32; N * N];
    let inv = 1.0 / (2.0 * FINE_SAMPLE_M);
    for z in 0..N {
        for x in 0..N {
            let (xm, xp) = (x.saturating_sub(1), (x + 1).min(N - 1));
            let (zm, zp) = (z.saturating_sub(1), (z + 1).min(N - 1));
            gx[z * N + x] = (samples[z * N + xp] - samples[z * N + xm]) * inv;
            gz[z * N + x] = (samples[zp * N + x] - samples[zm * N + x]) * inv;
        }
    }
    let r = DETAIL_CARRIED_TEXELS / 2;
    let mut want = vec![0u8; N * N * 2];
    let mut lo = vec![0.0f32; N * N];
    for (channel, g) in [&gx, &gz].into_iter().enumerate() {
        for z in 0..N {
            for x in 0..N {
                let mut sum = 0.0;
                for d in 0..DETAIL_CARRIED_TEXELS {
                    sum += g[z * N + (x + d).saturating_sub(r).min(N - 1)];
                }
                lo[z * N + x] = sum / DETAIL_CARRIED_TEXELS as f32;
            }
        }
        for x in 0..N {
            for z in 0..N {
                let mut sum = 0.0;
                for d in 0..DETAIL_CARRIED_TEXELS {
                    sum += lo[(z + d).saturating_sub(r).min(N - 1) * N + x];
                }
                let carried = sum / DETAIL_CARRIED_TEXELS as f32;
                let residual = (g[z * N + x] - carried) / DETAIL_SLOPE_RANGE;
                want[(z * N + x) * 2 + channel] =
                    ((residual.clamp(-1.0, 1.0) * 127.0).round() as i8) as u8;
            }
        }
    }

    assert_eq!(got.len(), want.len());
    let mut worst = 0i32;
    for (i, (&a, &b)) in got.iter().zip(&want).enumerate() {
        let d = (a as i8 as i32 - b as i8 as i32).abs();
        assert!(
            d <= 1,
            "texel {i} is {d} apart: running {} vs direct {}",
            a as i8,
            b as i8
        );
        worst = worst.max(d);
    }
    assert!(worst <= 1, "worst quantised difference was {worst}");
}

/// Nothing is drawn on the half-metre tier, so nothing stands on it: a body
/// on it stood 1.7 m over the drawn path on the Noorderplantsoen rampart
/// (2026-11-16, `foot-placement.md`).
#[test]
fn the_fine_tier_is_not_stood_on() {
    let mut field = ramp_field(0.0);
    let centre = Vec2::new(100.0, -50.0);
    // A tier that disagrees with the (flat, zero) coarse field everywhere.
    field.fine = TerrainSnapshot {
        samples: std::sync::Arc::new(vec![10.0; 4 * 4]),
        half_m: FINE_HALF_M,
        grid: 4,
        centre,
        floors: std::sync::Arc::default(),
        banks: std::sync::Arc::default(),
    };
    for x in [0.0, FINE_HALF_M - 30.0, FINE_HALF_M + 1.0] {
        let h = field.ground_at(centre.x + x, centre.y);
        assert!(h.abs() < 1e-6, "{x}: {h}");
    }
}

/// **The property the whole coarse-level scheme rests on.** A clipmap level whose
/// quads are coarser than the field draws its surface through `floor_at`; if that
/// surface can rise above the measured ground anywhere, the level pokes through
/// the detailed one drawn over it and the blurry imagery shows in the middle of
/// the map. Checked over ground far rougher than the Netherlands has, at every
/// quad size a relieved tile can ask for, and not only at the sample points —
/// between them, where linear interpolation is what a mesh actually draws.
#[test]
fn a_coarse_quad_never_draws_above_the_measured_ground() {
    let n = 257;
    let half = 400.0f32;
    let sample = half * 2.0 / (n - 1) as f32;
    // Sharp ridges and pits at every scale, so a min filter has something to bite.
    let height = |x: f32, z: f32| {
        (x * 0.31).sin() * 4.0 + (z * 0.17).cos() * 3.0 + (x * 0.03 + z * 0.05).sin() * 12.0
    };
    let mut samples = Vec::with_capacity(n * n);
    for zi in 0..n {
        for xi in 0..n {
            let (x, z) = (-half + xi as f32 * sample, -half + zi as f32 * sample);
            samples.push(height(x, z));
        }
    }
    let floors = build_floors(&samples, n, sample);
    let field = TerrainSnapshot {
        samples: std::sync::Arc::new(samples),
        half_m: half,
        grid: n,
        centre: Vec2::ZERO,
        floors: std::sync::Arc::new(floors),
        banks: std::sync::Arc::default(),
    };

    // Within one rung there is no envelope at all: the finest relieved level has
    // nothing drawn over it and must carry the measurements.
    for quad in [sample, sample * 2.0] {
        let (x, z) = (37.0f32, -61.0f32);
        assert_eq!(
            field.floor_at(x, z, quad),
            field.at(x, z),
            "{quad} m quads should stand on the measurements, not an envelope"
        );
    }

    // From the first quad size that does take an envelope up to the coarsest a
    // relieved tile can ask for.
    for quad in [sample * 2.01, 12.5, 25.0, 50.0, 90.0] {
        // A mesh of this quad size, laid over the middle of the field where a
        // lookup has measurements on every side.
        let mut worst = f32::MIN;
        let mut steps = 0;
        let reach = half * 0.6;
        let mut z = -reach;
        while z <= reach {
            let mut x = -reach;
            while x <= reach {
                // Where the mesh's own linear interpolation lands, between the
                // vertices — a quarter, a half and three quarters along a quad.
                for (fx, fz) in [(0.0, 0.0), (0.25, 0.5), (0.5, 0.5), (0.75, 0.25)] {
                    let (px, pz) = (x + fx * quad, z + fz * quad);
                    let a = field.floor_at(x, z, quad);
                    let b = field.floor_at(x + quad, z, quad);
                    let c = field.floor_at(x, z + quad, quad);
                    let d = field.floor_at(x + quad, z + quad, quad);
                    let top = a.max(b).max(c).max(d);
                    worst = worst.max(top - field.at(px, pz));
                    steps += 1;
                }
                x += quad;
            }
            z += quad;
        }
        assert!(steps > 0);
        assert!(
            worst <= 1e-3,
            "{quad} m quads draw {worst} m above the measured ground; a level with \
                 quads this size would poke through the one over it"
        );
    }
}

/// The fill's reach is a distance in metres, whatever the sampling is. This is the
/// test the constant needed and did not have: it was six one-cell dilations
/// documented as "~150 m", which silently became 37 m the moment the field's
/// resolution moved.
#[test]
fn the_fill_reaches_the_same_distance_at_any_sampling() {
    // The same 40 m hole in the middle of flat ground, sampled two ways.
    for (n, sample_m) in [(64usize, 4.0f32), (128, 2.0)] {
        let hole = (40.0 / sample_m) as usize;
        let lo = n / 2 - hole / 2;
        let mut grid = vec![1.0f32; n * n];
        for z in lo..lo + hole {
            for x in lo..lo + hole {
                grid[z * n + x] = f32::NAN;
            }
        }
        fill_holes(&mut grid, n, Some(FILL_REACH_M / sample_m));
        assert!(
            grid.iter().all(|v| v.is_finite()),
            "a 40 m hole is well inside {FILL_REACH_M} m of reach, but {sample_m} m \
                 sampling left it open"
        );
        let (lo_v, hi_v) = grid
            .iter()
            .fold((f32::MAX, f32::MIN), |(l, h), v| (l.min(*v), h.max(*v)));
        assert!(
            (lo_v - 1.0).abs() < 1e-3 && (hi_v - 1.0).abs() < 1e-3,
            "filling a hole in flat ground invented relief: {lo_v}..{hi_v}"
        );
    }
}

/// The carve has to reach ground AHN *did* return a value for. At 25 m the value
/// over a canal is the banks averaged in by resampling, not a bed — skipping those
/// samples is what kept the basemap flat for a day.
#[test]
fn the_carve_overwrites_resampled_ground_but_keeps_a_deeper_reading() {
    let n = 8;
    let half = 100.0f32;
    // One ring covering the whole grid, surface at 0 m NAP.
    let field = crate::systems::water_levels::WaterLevelField::build_for_test(
        0.0,
        &[
            [-half * 2.0, -half * 2.0],
            [half * 2.0, -half * 2.0],
            [half * 2.0, half * 2.0],
            [-half * 2.0, half * 2.0],
        ],
    );
    let bed = cartopolis_geo::terrain_grade::WATER_BED_M;

    let mut grid = vec![5.0f32; n * n];
    grid[0] = f32::NAN; // a genuine hole
    grid[1] = -9.0; // a dredged channel: deeper than the surface
    let carved = carve_water(&mut grid, n, half, Vec2::ZERO, &field, false);

    assert!(carved > 0, "nothing was carved at all");
    assert!(
        (grid[0] - (0.0 - bed)).abs() < 1e-5,
        "a hole inside the ring must take the water's own level"
    );
    assert!(
        (grid[1] - -9.0).abs() < 1e-5,
        "a deeper measurement must survive: the bed can be lower than the surface"
    );
    assert!(
        (grid[5] - (0.0 - bed)).abs() < 1e-5,
        "resampled bank height inside the ring must be overwritten, not skipped"
    );
}

/// The half-metre tier keeps what it measured standing above the water.
///
/// The mirror of the test above, and the two together are the whole rule: at
/// 3.125 m a value inside a canal is the banks averaged in by a resampling
/// service and must be cut through; at 0.5 m it is a laser return off something
/// solid, because the beam does not come back off water at all.
///
/// What it saves is not hypothetical. Sampling inside the registered water
/// polygons over the study quay block at the native half metre: 78.3 % no-data,
/// 21.6 % measured above the surface — 5,052 m² of quay path, jetty and lock —
/// and 0.1 % measured at or below it.
#[test]
fn the_fine_carve_keeps_a_structure_standing_above_the_water() {
    let n = 8;
    let half = 100.0f32;
    let field = crate::systems::water_levels::WaterLevelField::build_for_test(
        0.0,
        &[
            [-half * 2.0, -half * 2.0],
            [half * 2.0, -half * 2.0],
            [half * 2.0, half * 2.0],
            [-half * 2.0, half * 2.0],
        ],
    );
    let bed = cartopolis_geo::terrain_grade::WATER_BED_M;

    let mut grid = vec![f32::NAN; n * n];
    grid[1] = 0.10; // a quay path: a real return, 10 cm above the surface
    grid[2] = -9.0; // a dredged channel: a real return, below the surface
    let carved = carve_water(&mut grid, n, half, Vec2::ZERO, &field, true);

    assert!(carved > 0, "the open water still has to be carved");
    assert!(
        (grid[0] - (0.0 - bed)).abs() < 1e-5,
        "no-data is open water and still takes the bed"
    );
    assert!(
        (grid[1] - 0.10).abs() < 1e-5,
        "a measured surface above the water must survive, got {}",
        grid[1]
    );
    assert!(
        (grid[2] - -9.0).abs() < 1e-5,
        "a deeper measurement must still survive"
    );

    // …and the coarse tier must not do this, or a resampled bank keeps the
    // basemap flat across the canal.
    let mut coarse = vec![f32::NAN; n * n];
    coarse[1] = 0.10;
    carve_water(&mut coarse, n, half, Vec2::ZERO, &field, false);
    assert!(
        (coarse[1] - (0.0 - bed)).abs() < 1e-5,
        "the coarse tier must still cut through a resampled value, got {}",
        coarse[1]
    );
}

/// Nothing registered must leave the field byte-identical.
#[test]
fn an_empty_water_field_changes_no_sample() {
    let n = 8;
    let mut grid = vec![5.0f32; n * n];
    let before = grid.clone();
    let carved = carve_water(
        &mut grid,
        n,
        100.0,
        Vec2::ZERO,
        &crate::systems::water_levels::WaterLevelField::default(),
        true,
    );
    assert_eq!(carved, 0);
    assert_eq!(grid, before);
}

/// Samples per side for the synthetic fields below. Not [`TERRAIN_GRID`]: these
/// tests are about sampling and blending, and the real grid is four million
/// samples plus a pyramid over them, built once per test that touches one.
const RAMP_GRID: usize = 513;

/// A field whose samples are a known ramp, bypassing the fetch.
fn ramp_field(per_metre: f32) -> TerrainField {
    let n = RAMP_GRID;
    let half = TERRAIN_HALF_M;
    let step = half * 2.0 / (n - 1) as f32;
    let mut samples = vec![0.0; n * n];
    for z in 0..n {
        for x in 0..n {
            samples[z * n + x] = (-half + x as f32 * step) * per_metre;
        }
    }
    let floors = build_floors(&samples, n, half * 2.0 / (n - 1) as f32);
    TerrainField {
        ground: TerrainSnapshot {
            samples: std::sync::Arc::new(samples),
            half_m: half,
            grid: n,
            centre: Vec2::ZERO,
            floors: std::sync::Arc::new(floors),
            banks: std::sync::Arc::default(),
        },
        ..Default::default()
    }
}

/// A flat quad with a vertex at each corner, in the XZ plane — the shape a
/// route ribbon is made of.
fn flat_quad(size: f32) -> Mesh {
    use bevy::render::mesh::Indices;
    let mut mesh = Mesh::new(
        bevy::render::mesh::PrimitiveTopology::TriangleList,
        bevy::asset::RenderAssetUsages::default(),
    );
    mesh.insert_attribute(
        Mesh::ATTRIBUTE_POSITION,
        vec![
            [0.0, 0.0, 0.0],
            [size, 0.0, 0.0],
            [size, 0.0, size],
            [0.0, 0.0, size],
        ],
    );
    mesh.insert_indices(Indices::U32(vec![0, 1, 2, 0, 2, 3]));
    mesh
}

/// Lift every vertex of a mesh, the way the route ribbon arrives off a bridge
/// deck.
fn raise(mesh: &mut Mesh, by: f32) {
    let Some(bevy::render::mesh::VertexAttributeValues::Float32x3(p)) =
        mesh.attribute_mut(Mesh::ATTRIBUTE_POSITION)
    else {
        panic!("no positions");
    };
    for v in p.iter_mut() {
        v[1] += by;
    }
}

fn heights_of(mesh: &Mesh) -> Vec<f32> {
    let Some(bevy::render::mesh::VertexAttributeValues::Float32x3(p)) =
        mesh.attribute(Mesh::ATTRIBUTE_POSITION)
    else {
        panic!("no positions");
    };
    p.iter().map(|v| v[1]).collect()
}

/// The drape **adds** to the height a mesh already carries. That is what lets the
/// route line arrive with a bridge deck baked into its vertices and still follow
/// the ground the bridge stands on — a drape that assigned would flatten the
/// viaduct back onto the terrain.
#[test]
fn draping_adds_to_the_height_a_mesh_already_has() {
    let field = ramp_field(0.01).snapshot();
    let at = Vec2::ZERO;

    let mut ground = flat_quad(10.0);
    drape_mesh(&mut ground, at, &field);

    let mut raised = flat_quad(10.0);
    raise(&mut raised, 6.0);
    drape_mesh(&mut raised, at, &field);

    let (a, b) = (heights_of(&ground), heights_of(&raised));
    assert_eq!(a.len(), b.len());
    assert!(
        a.iter().any(|y| *y != 0.0),
        "the ramp field should have moved the plain drape at all"
    );
    for (g, r) in a.iter().zip(&b) {
        assert!((r - g - 6.0).abs() < 1e-3, "ground {g}, raised {r}");
    }
}

/// An absent field leaves a mesh exactly as it was — including a height it
/// brought with it. The old wording for this was "a no-op", which is only the
/// same thing while nothing arrives pre-lifted.
#[test]
fn an_absent_field_leaves_a_raised_mesh_raised() {
    let mut mesh = flat_quad(10.0);
    raise(&mut mesh, 6.0);
    drape_mesh(&mut mesh, Vec2::ZERO, &TerrainField::default().snapshot());
    assert!(heights_of(&mesh).iter().all(|y| (*y - 6.0).abs() < 1e-6));
}

#[test]
fn an_absent_field_is_flat_everywhere() {
    let f = TerrainField::default();
    assert!(!f.is_ready());
    assert_eq!(f.at(0.0, 0.0), 0.0);
    assert_eq!(f.at(1_000.0, -2_500.0), 0.0);
    // The point of returning zero rather than an Option: every consumer adds it
    // unconditionally, so "layer off" costs a branch, not a code path.
    assert_eq!(f.at(f32::MAX, f32::MIN), 0.0);
}

#[test]
fn sampling_is_bilinear_and_bounded() {
    let f = ramp_field(0.01); // 1 cm per metre east
    assert!((f.at(0.0, 0.0)).abs() < 1e-3);
    assert!((f.at(100.0, 0.0) - 1.0).abs() < 0.01);
    assert!((f.at(-100.0, 250.0) + 1.0).abs() < 0.01);
    // Between two samples, not snapped to one of them.
    let step = TERRAIN_HALF_M * 2.0 / (RAMP_GRID - 1) as f32;
    let mid = f.at(step * 0.5, 0.0);
    assert!(mid > f.at(0.0, 0.0) && mid < f.at(step, 0.0));
    // Outside the box is flat world, not an extrapolated ramp.
    assert_eq!(f.at(TERRAIN_HALF_M + 1.0, 0.0), 0.0);
    assert_eq!(f.at(0.0, -TERRAIN_HALF_M - 1.0), 0.0);
}

#[test]
fn the_edge_taper_reaches_zero_at_the_boundary() {
    let n = RAMP_GRID;
    assert_eq!(edge_taper(n / 2, n / 2, n), 1.0);
    assert_eq!(edge_taper(0, n / 2, n), 0.0);
    assert_eq!(edge_taper(n - 1, n - 1, n), 0.0);
    // Monotone on the way out from the middle — a taper with a bump in it is a
    // ridge that isn't there.
    let mut last = 1.0;
    for i in n / 2..n {
        let t = edge_taper(i, n / 2, n);
        assert!(t <= last + 1e-6, "taper rose at {i}");
        last = t;
    }
}

#[test]
fn holes_fill_from_their_neighbours_and_big_voids_stay_flat() {
    // 10 m sampling, so the reach is 15 cells and the two holes straddle it.
    let sample_m = 10.0f32;
    let n = 128;
    let mut grid = vec![5.0f32; n * n];
    // A bridge-sized cut-out.
    for z in 10..14 {
        for x in 10..14 {
            grid[z * n + x] = f32::NAN;
        }
    }
    // …and a void far larger than the fill can reach: 600 m across, so its
    // middle is 300 m from the nearest measurement.
    for z in 50..110 {
        for x in 50..110 {
            grid[z * n + x] = f32::NAN;
        }
    }
    fill_holes(&mut grid, n, Some(FILL_REACH_M / sample_m));
    assert!(grid[12 * n + 12].is_finite(), "small hole not filled");
    assert!(
        (grid[12 * n + 12] - 5.0).abs() < 1e-3,
        "fill invented a slope"
    );
    assert!(
        !grid[80 * n + 80].is_finite(),
        "a 600 m void must not be smeared shut"
    );
}

/// Signed area doubled — the sign is the winding, and the winding is whether
/// the triangle is visible at all.
fn winding(p: &[[f32; 3]], t: &[u32]) -> f32 {
    let (a, b, c) = (p[t[0] as usize], p[t[1] as usize], p[t[2] as usize]);
    (b[0] - a[0]) * (c[2] - a[2]) - (c[0] - a[0]) * (b[2] - a[2])
}

#[test]
fn refinement_splits_long_edges_and_leaves_short_ones_alone() {
    // Two triangles sharing the diagonal of a 200 m square — earcut's answer
    // for a field, and the shape that cannot follow a hill.
    let mut positions = vec![
        [0.0, 0.0, 0.0],
        [200.0, 0.0, 0.0],
        [200.0, 0.0, 200.0],
        [0.0, 0.0, 200.0],
    ];
    let mut indices = vec![0, 1, 2, 0, 2, 3];
    let before = winding(&positions, &indices[0..3]).signum();
    let (mut uvs, mut normals, mut colors) = (vec![], vec![], vec![]);
    refine_for_terrain(
        &mut positions,
        &mut indices,
        &mut uvs,
        &mut normals,
        &mut colors,
        &mut vec![],
        24.0,
        None,
    );
    // Every edge now short enough for the field to be expressed on it…
    for tri in indices.chunks_exact(3) {
        for (i, j) in [(0, 1), (1, 2), (2, 0)] {
            let (a, b) = (positions[tri[i] as usize], positions[tri[j] as usize]);
            let len = ((a[0] - b[0]).powi(2) + (a[2] - b[2]).powi(2)).sqrt();
            assert!(len <= 24.0 + 1e-3, "edge of {len} m survived refinement");
        }
        // …and still facing the way it was.
        assert_eq!(
            winding(&positions, tri).signum(),
            before,
            "a sub-triangle flipped"
        );
    }

    // Already-fine geometry is returned untouched: refinement is a cost paid
    // only where the ground is coarse.
    let mut small = vec![[0.0, 0.0, 0.0], [5.0, 0.0, 0.0], [0.0, 0.0, 5.0]];
    let mut small_idx = vec![0, 1, 2];
    refine_for_terrain(
        &mut small,
        &mut small_idx,
        &mut vec![],
        &mut vec![],
        &mut vec![],
        &mut vec![],
        24.0,
        None,
    );
    assert_eq!(small.len(), 3);
    assert_eq!(small_idx, vec![0, 1, 2]);
}

#[test]
fn refinement_shares_midpoints_so_a_slope_has_no_cracks() {
    // Two triangles across one long shared edge. If each invented its own
    // midpoint, displacement would pull them apart along that seam.
    let mut positions = vec![
        [0.0, 0.0, 0.0],
        [100.0, 0.0, 0.0],
        [0.0, 0.0, 100.0],
        [100.0, 0.0, 100.0],
    ];
    let mut indices = vec![0, 1, 2, 1, 3, 2];
    refine_for_terrain(
        &mut positions,
        &mut indices,
        &mut vec![],
        &mut vec![],
        &mut vec![],
        &mut vec![],
        24.0,
        None,
    );
    // Every position is used by some triangle, and no position is duplicated:
    // both are what "the seam is stitched" means in index terms.
    let used: std::collections::HashSet<u32> = indices.iter().copied().collect();
    assert_eq!(
        used.len(),
        positions.len(),
        "orphaned or duplicated vertices"
    );
    let mut seen = std::collections::HashSet::new();
    for p in &positions {
        let key = (
            (p[0] * 100.0).round() as i64,
            (p[1] * 100.0).round() as i64,
            (p[2] * 100.0).round() as i64,
        );
        assert!(
            seen.insert(key),
            "duplicate vertex at {p:?} — that is a crack"
        );
    }
}

#[test]
fn refinement_interpolates_the_attributes_it_carries() {
    let mut positions = vec![[0.0, 0.0, 0.0], [100.0, 0.0, 0.0], [0.0, 0.0, 100.0]];
    let mut indices = vec![0, 1, 2];
    let mut uvs = vec![[0.0, 0.0], [1.0, 0.0], [0.0, 1.0]];
    let mut normals = vec![[0.0, 1.0, 0.0]; 3];
    let mut colors = vec![
        [1.0, 0.0, 0.0, 1.0],
        [0.0, 1.0, 0.0, 1.0],
        [1.0, 0.0, 0.0, 1.0],
    ];
    refine_for_terrain(
        &mut positions,
        &mut indices,
        &mut uvs,
        &mut normals,
        &mut colors,
        &mut vec![],
        24.0,
        None,
    );
    // A road's markings register with its geometry through UV0; dropping the
    // attribute arrays at refinement would strip the markings off every long
    // straight in the city.
    assert_eq!(uvs.len(), positions.len());
    assert_eq!(normals.len(), positions.len());
    assert_eq!(colors.len(), positions.len());
    for n in &normals {
        assert!((Vec3::from(*n).length() - 1.0).abs() < 1e-4);
    }
    for c in &colors {
        assert!(c.iter().all(|v| (0.0..=1.0).contains(v)));
    }
}

#[test]
fn water_sits_level_over_the_polygon_it_covers() {
    let f = ramp_field(0.01);
    // A canal running up the slope: per-vertex would tilt it, the median holds
    // it level at the middle.
    let level = f.level_over([[-100.0, 0.0], [0.0, 0.0], [100.0, 0.0]]);
    assert!((level - f.at(0.0, 0.0)).abs() < 0.01);
    assert_eq!(TerrainField::default().level_over([[0.0, 0.0]]), 0.0);
}

/// **The embankment is built, and it climbs at the design grade.**
///
/// The wiring test for the thing that existed for a day and was reachable
/// only from `carto-view` (`bridge-heights.md`). A flat box, a deck standing
/// six metres over it, and an approach running west out of the abutment: the
/// ground along that approach has to rise to meet the deck at no more than
/// `MAX_APPROACH_GRADE`, and the ground either side of it must not move at
/// all.
#[test]
fn an_approach_climbs_to_its_deck_at_the_design_grade() {
    let anchor = (53.2194, 6.5665, 14);
    let centre = wgs84_to_rd(anchor.0, anchor.1);
    let (w, h) = (64, 64);
    let half_rd = 4_000.0f64;
    // Dead flat at 20 m, so anything the output carries is the grading.
    let patch = HeightPatch {
        min: [centre[0] - half_rd, centre[1] - half_rd],
        max: [centre[0] + half_rd, centre[1] + half_rd],
        w,
        h,
        samples: vec![20.0f32; w * h],
    };
    let n = 129;
    let step = TERRAIN_HALF_M * 2.0 / (n - 1) as f32;
    let mid = n / 2;

    // The structure: a 3-sample block at the middle, six metres up. The
    // approach: the row west of it, out to the rim.
    let mut deck_lift = vec![f32::NAN; n * n];
    let mut ramp = vec![false; n * n];
    let mut road = vec![false; n * n];
    for dz in -1i32..=1 {
        let z = (mid as i32 + dz) as usize;
        for dx in 0..3 {
            deck_lift[z * n + mid + dx] = 6.0;
        }
        for x in 1..mid {
            ramp[z * n + x] = true;
            road[z * n + x] = true;
        }
    }

    let (baked, approach) = bake(
        &patch,
        anchor,
        Vec2::ZERO,
        None,
        TERRAIN_HALF_M,
        n,
        Some(Grading {
            water: &crate::systems::water_levels::WaterLevelField::default(),
            road,
            ramp,
            deck_lift: &deck_lift,
            // No measured top: the clearance path, which is what this pins.
            deck_top: &[],
        }),
    );
    let (samples, _datum, _carved) = baked.expect("patch covers the field");

    assert!(
        approach.0 > 0,
        "no abutment sample was pinned, so no embankment was built at all — \
             which is exactly the state this feature was in while the grading \
             sat wired into carto-view alone"
    );
    assert!(approach.1 > 0, "the embankment raised nothing");

    // At the abutment the ground has climbed most of the way to the deck…
    let at = |x: usize| samples[mid * n + x];
    let abutment = at(mid - 1);
    assert!(
        abutment > 4.0,
        "the ground at the abutment reached {abutment:.2} m of the deck's 6"
    );
    // …and it got there no faster than the design grade allows.
    for x in 2..mid {
        let rise = at(x) - at(x - 1);
        assert!(
            rise <= cartopolis_geo::road_grade::MAX_APPROACH_GRADE * step + 1e-3,
            "the embankment climbed {rise:.3} m over {step:.2} m at sample {x}, \
                 steeper than the {:.0} % ceiling",
            cartopolis_geo::road_grade::MAX_APPROACH_GRADE * 100.0
        );
    }
    // And the ground away from the corridor is untouched, or the ramp has
    // travelled through everything the mask happens to connect.
    let away = samples[(mid + 20) * n + (mid - 20)];
    assert!(
        away.abs() < 0.05,
        "ground 20 samples off the approach moved to {away:.2} m"
    );
}

#[test]
fn baking_is_relative_to_the_anchor_and_flat_at_the_rim() {
    // A patch that rises 1 m per 100 m east, over a box around Groningen.
    let anchor = (53.2194, 6.5665, 14);
    let centre = wgs84_to_rd(anchor.0, anchor.1);
    let (w, h) = (64, 64);
    let (half_rd, mut samples) = (4_000.0f64, Vec::with_capacity(w * h));
    for _row in 0..h {
        for col in 0..w {
            let east = -half_rd + col as f64 / (w - 1) as f64 * half_rd * 2.0;
            samples.push(20.0 + (east / 100.0) as f32);
        }
    }
    let patch = HeightPatch {
        min: [centre[0] - half_rd, centre[1] - half_rd],
        max: [centre[0] + half_rd, centre[1] + half_rd],
        w,
        h,
        samples,
    };
    // No registered water: this test is about the ramp the patch carries, and an
    // empty field is also the everywhere-outside-the-Netherlands case.
    // A test grid, not `TERRAIN_GRID`: the bake runs a `wgs84_to_rd` polynomial
    // per sample and the real field is four million of them.
    let n = 129;
    let (baked, approach) = bake(
        &patch,
        anchor,
        Vec2::ZERO,
        None,
        TERRAIN_HALF_M,
        n,
        Some(Grading {
            water: &crate::systems::water_levels::WaterLevelField::default(),
            // No roads either: this test is about the ramp the patch carries,
            // and an empty mask is what a box with no streamed cell in it yet
            // has.
            road: Vec::new(),
            // …and no decks, for the same reason.
            ramp: Vec::new(),
            deck_lift: &[],
            deck_top: &[],
        }),
    );
    assert_eq!(
        approach,
        (0, 0),
        "a box with no viaduct in it must build no embankment"
    );
    let (baked, datum, carved) = baked.expect("patch covers the field");
    assert_eq!(carved, 0, "an empty water field must carve nothing");
    // The datum is the ground at the anchor, and the field is zero there. The
    // tolerance is the synthetic patch's own 125 m sample spacing: `at` takes
    // the nearest measurement, deliberately, so the centre reads the cell it
    // lands in rather than an interpolation of four.
    assert!(
        (datum - 20.0).abs() < 1.0,
        "datum {datum} should be ~20 m NAP"
    );
    assert!(baked[(n / 2) * n + n / 2].abs() < 0.5);
    // A kilometre east is ~10 m up, relative — not 30 m absolute.
    let one_km = ((1_000.0 + TERRAIN_HALF_M) / (TERRAIN_HALF_M * 2.0) * (n - 1) as f32) as usize;
    let east = baked[(n / 2) * n + one_km];
    assert!((east - 10.0).abs() < 1.5, "1 km east read {east} m");
    // Every edge sample is flattened, so the field meets the flat world outside.
    for i in 0..n {
        assert_eq!(baked[i], 0.0, "north rim");
        assert_eq!(baked[(n - 1) * n + i], 0.0, "south rim");
        assert_eq!(baked[i * n], 0.0, "west rim");
        assert_eq!(baked[i * n + n - 1], 0.0, "east rim");
    }
}

/// **A road cell marks the field it will be drawn on.**
///
/// The mask is the whole of what makes the bake able to grade, and nothing in a
/// picture distinguishes "the grading ran and this ground is genuinely rough"
/// from "the mask was empty, so it did nothing" — which is what the client did
/// on every platform until 2026-09-21. So this asserts the two facts the bake
/// depends on: a cell with carriageways in it marks samples, and it marks them
/// where the road actually is.
#[test]
fn a_cell_of_roads_marks_the_field_under_them() {
    use crate::systems::vector_tiles::{SurfaceGroup, SurfaceMesh};

    let grid = 129;
    let step = TERRAIN_HALF_M * 2.0 / (grid - 1) as f32;
    // A 40 m square of carriageway at the field's centre, given as a cell whose
    // own origin is the anchor — so tile-local and world metres coincide and the
    // expected samples are arithmetic rather than a second implementation.
    let quad = |half: f32| SurfaceMesh {
        group: SurfaceGroup::Road(cartopolis_geo::road_texture::RoadClass::Carriageway),
        positions: vec![
            [-half, 0.0, -half],
            [half, 0.0, -half],
            [half, 0.0, half],
            [-half, 0.0, half],
        ],
        indices: vec![0, 1, 2, 0, 2, 3],
        uvs: Vec::new(),
        normals: Vec::new(),
        colors: Vec::new(),
        rungs: Vec::new(),
    };

    let mut mask = RoadMask::default();
    assert_eq!(mask.marked_samples(), 0, "nothing is marked before a cell");
    mask.mark_cell(7, grid, (0, 0), Vec2::ZERO, &[quad(20.0)]);

    let marked = mask.marked_samples();
    assert!(marked > 0, "a cell of carriageway marked nothing");
    // The centre is road; a point well outside the square *and* outside the
    // margin is not. The margin is in samples, so this is stated in samples.
    let at = |x: i32, z: i32| {
        let c = (grid / 2) as i32;
        mask.marks[((c + z) as usize) * grid + (c + x) as usize]
    };
    assert!(at(0, 0), "the middle of the carriageway is not marked");
    let clear =
        (20.0 / step).ceil() as i32 + cartopolis_geo::road_grade::DEFAULT_MARGIN_SAMPLES as i32 + 2;
    assert!(!at(clear, 0), "ground {clear} samples away was marked");

    // **The same cell twice must not count twice.** Every cell is re-tessellated
    // whenever the terrain revision moves, and the terrain revision is what this
    // mask drives — so a bump per re-tessellation is a bake that never settles.
    let rev = mask.revision;
    mask.mark_cell(7, grid, (0, 0), Vec2::ZERO, &[quad(20.0)]);
    assert_eq!(mask.revision, rev, "the same cell was marked twice");

    // A re-anchor is a different frame, so the marks go.
    mask.mark_cell(8, grid, (0, 0), Vec2::ZERO, &[]);
    assert_eq!(
        mask.marked_samples(),
        0,
        "marks from before a re-anchor survived it"
    );
}

/// A tunnel's carriageway is metres below the ground and must not drag the
/// ground down to meet it — the mask is the surface network, not every road.
#[test]
fn a_tunnel_deck_does_not_grade_the_ground_over_it() {
    use crate::systems::vector_tiles::{SurfaceGroup, SurfaceMesh};

    let mut mask = RoadMask::default();
    mask.mark_cell(
        1,
        129,
        (0, 0),
        Vec2::ZERO,
        &[SurfaceMesh {
            group: SurfaceGroup::TunnelRoad(cartopolis_geo::road_texture::RoadClass::Carriageway),
            positions: vec![
                [-20.0, -7.0, -20.0],
                [20.0, -7.0, -20.0],
                [20.0, -7.0, 20.0],
            ],
            indices: vec![0, 1, 2],
            uvs: Vec::new(),
            normals: Vec::new(),
            colors: Vec::new(),
            rungs: Vec::new(),
        }],
    );
    assert_eq!(
        mask.marked_samples(),
        0,
        "a bore graded the ground above itself"
    );
}
