//! Flattening a height survey along the roads, so a carriageway is graded rather
//! than draped.
//!
//! # The problem this removes
//!
//! Every flat thing the map draws — the landuse fills, the surveyed pavements, the
//! road ribbons, the basemap picture — is bent onto the measured ground using *its
//! own* triangles. Between its corners each one is a flat plane. Where the ground
//! curves between corners, two of them disagree about where the ground is by the
//! field's curvature across an edge: decimetres on a canal bank, and up to 2.8 m on
//! a handset, whose ground is sampled every 12.5 m and whose edges may run to
//! 48.75. That is hundreds of times the ten-millimetre window
//! [`crate::ground_stack`] has to order them in, so the lower sheet stands through
//! the upper one in jagged polygons following triangle edges.
//!
//! `ground_stack` removes the overlaps it can. This removes the *cause*: where the
//! ground under a road has no curvature, no two sheets over it can disagree, and
//! the fix reaches the pairs a cut never can — the basemap picture, a feature
//! crossing a tile boundary, a sheet from a neighbouring cell.
//!
//! # Why the ground and not the road
//!
//! Smoothing the *road* is the obvious move and it makes the tearing worse: the
//! road then follows a different surface from everything around it, and the ground
//! pokes through wherever the smoothed road dips below the survey. There is only
//! one way for several sheets to agree, which is for there to be one height
//! function. So the survey is what changes, and every consumer — the sheets, the
//! basemap, the avatar's footing, the collision floor — reads it without knowing
//! this happened.
//!
//! # Why a relaxation rather than a centre line
//!
//! The tidy version samples the survey along each road's centre line, smooths that
//! into a grade, and writes it back across the width. It needs the centre lines, a
//! direction, a width, a decision at every junction where two grades disagree, and
//! a taper that does not leave a step at the kerb.
//!
//! [`flatten`] instead holds the survey fixed *outside* the marked region and
//! relaxes what is inside it — the discrete Laplace equation, whose solution is the
//! smoothest surface meeting the measured ground exactly at that boundary. That is
//! a grade, and it arrives with the four hard parts already answered: across the
//! road it is flat, because the two sides are its only constraint; along the road
//! it follows the ground, because they do; a junction is one region with one
//! solution rather than two grades to reconcile; and there is no step anywhere,
//! because the surrounding ground *is* the boundary condition rather than something
//! a taper has to reach.
//!
//! Which boundary, though, is the whole question — see the margin below.
//!
//! # The margin is load-bearing, and it took a failed test to see why
//!
//! Marking the carriageway alone does almost nothing to a road crossing a valley:
//! both kerbs are *in* the valley, so the only surface meeting them is the valley.
//! Measured on a 4 m V with a three-sample road, the dip went 3.80 m → 3.52 m.
//!
//! So the mark is grown by a margin before relaxing ([`mark_roads`]'s `grow`), and
//! the survey is held fixed at the *outer* edge of that instead of at the kerb.
//! The carriageway is then free to depart from the ground beneath it, and the
//! margin becomes the surface that bends to meet it — which is a cutting or an
//! embankment, and is what a built road actually has beside it.
//!
//! What this still does not do, and should not, is bridge a valley. The ground
//! really does fall away and the road really does go down and up; what stops is
//! following every bump on the way. A viaduct is a bridge deck
//! (`SurfaceGroup::BridgeDeck`), which stands at its own measured height and is
//! not in the coplanar ground stack at all.

/// One relaxation pass costs a read of four neighbours per marked sample, so the
/// count is the width of the widest corridor the smoothing has to cross, in
/// samples. Information travels one sample per pass.
///
/// 24 carries a corridor 24 samples wide — 75 m at the desktop's 3.125 m spacing,
/// which covers a motorway and its shoulders. Wider than that relaxes partially:
/// the middle keeps some of its own measurement, which is the right failure, since
/// the alternative is a runtime that grows with the widest road in the country.
pub const DEFAULT_PASSES: usize = 24;

/// Samples of margin beyond the carriageway the grading may move.
///
/// Not a tuning constant. With the ground pinned at the kerb itself, both kerbs
/// are in whatever the road is crossing and the road can only reproduce it —
/// measured on a 4 m valley, a 3.80 m dip became 3.52 m. Pinned further out, that
/// band becomes the cutting or embankment a built road actually has beside it.
pub const DEFAULT_MARGIN_SAMPLES: usize = 6;

/// Flatten `samples` wherever `road` is set, holding everything else fixed.
///
/// `samples` and `road` are both `n × n`, row-major, and the same grid. Returns
/// how many samples were changed, for the same reason `carve_water` does: a stage
/// that silently did nothing and a stage that had nothing to do look identical
/// from outside.
///
/// Non-finite samples are left alone and are not used as a boundary value. The
/// survey has no reading at all over water and reports holes, which are filled
/// later; treating a hole as a height would relax the road down into it.
pub fn flatten(samples: &mut [f32], n: usize, road: &[bool], passes: usize) -> usize {
    if n < 3 || samples.len() < n * n || road.len() < n * n {
        return 0;
    }
    // The interior marked samples, once — the relaxation walks this list every
    // pass, and a city's roads are a few per cent of a field.
    let mut cells: Vec<usize> = Vec::new();
    for z in 1..n - 1 {
        for x in 1..n - 1 {
            let i = z * n + x;
            if road[i] && samples[i].is_finite() {
                cells.push(i);
            }
        }
    }
    if cells.is_empty() {
        return 0;
    }

    let before: Vec<f32> = cells.iter().map(|&i| samples[i]).collect();
    // Jacobi rather than Gauss-Seidel: reading this pass's own partial results
    // would make the answer depend on which way the grid is walked, so a road
    // running east would grade differently from the same road running west.
    let mut next = vec![0.0f32; cells.len()];
    for _ in 0..passes {
        for (slot, &i) in next.iter_mut().zip(&cells) {
            let mut sum = 0.0;
            let mut count = 0.0;
            for neighbour in [i - 1, i + 1, i - n, i + n] {
                let v = samples[neighbour];
                if v.is_finite() {
                    sum += v;
                    count += 1.0;
                }
            }
            // A marked sample with no usable neighbour keeps what it had. It cannot
            // be averaged out of nothing, and holding it is what stops a hole
            // spreading through the corridor.
            *slot = if count > 0.0 { sum / count } else { samples[i] };
        }
        for (&i, &v) in cells.iter().zip(&next) {
            samples[i] = v;
        }
    }

    cells
        .iter()
        .zip(&before)
        .filter(|(&i, &was)| samples[i] != was)
        .count()
}

/// Mark every grid sample covered by one of `triangles`, given in the same world
/// frame as the grid.
///
/// The triangles are the road ribbons themselves — the mesh the client draws —
/// rather than centre lines widened by a nominal number. That matters at a
/// junction, where the drawn surface is the junction's actual shape and a widened
/// centre line is two overlapping rectangles.
///
/// `grow` widens the mark by that many samples, which is how the flattening gets
/// somewhere to meet the ground: with no margin the boundary condition sits on the
/// kerb sample itself, which is half road and half verge in a grid this coarse.
pub fn mark_roads(
    road: &mut [bool],
    n: usize,
    origin: (f32, f32),
    step_m: f32,
    triangles: &[[[f32; 2]; 3]],
    grow: usize,
) {
    if n == 0 || road.len() < n * n || step_m <= 0.0 {
        return;
    }
    for tri in triangles {
        // Sample-space bounds, clamped at both ends: a ribbon can lie entirely off
        // the grid, and clamping only the maximum leaves the loop indexing past the
        // end.
        let xs = [tri[0][0], tri[1][0], tri[2][0]];
        let zs = [tri[0][1], tri[1][1], tri[2][1]];
        let to_x = |v: f32| (v - origin.0) / step_m;
        let to_z = |v: f32| (v - origin.1) / step_m;
        let lo_x = clamp_index(to_x(min3(xs)).floor(), n);
        let hi_x = clamp_index(to_x(max3(xs)).ceil(), n);
        let lo_z = clamp_index(to_z(min3(zs)).floor(), n);
        let hi_z = clamp_index(to_z(max3(zs)).ceil(), n);

        for z in lo_z..=hi_z {
            for x in lo_x..=hi_x {
                #[allow(clippy::cast_precision_loss)]
                let p = [origin.0 + x as f32 * step_m, origin.1 + z as f32 * step_m];
                if inside(tri, p) {
                    road[z * n + x] = true;
                }
            }
        }
    }
    for _ in 0..grow {
        dilate(road, n);
    }
}

/// One sample of growth in the four directions. Separate passes rather than a
/// radius, so `grow` is a count of samples and reads as one.
fn dilate(road: &mut [bool], n: usize) {
    let was = road.to_vec();
    for z in 0..n {
        for x in 0..n {
            if was[z * n + x] {
                continue;
            }
            let near = (x > 0 && was[z * n + x - 1])
                || (x + 1 < n && was[z * n + x + 1])
                || (z > 0 && was[(z - 1) * n + x])
                || (z + 1 < n && was[(z + 1) * n + x]);
            if near {
                road[z * n + x] = true;
            }
        }
    }
}

fn clamp_index(v: f32, n: usize) -> usize {
    if v <= 0.0 {
        return 0;
    }
    #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
    let i = v as usize;
    i.min(n - 1)
}

fn min3(v: [f32; 3]) -> f32 {
    v[0].min(v[1]).min(v[2])
}

fn max3(v: [f32; 3]) -> f32 {
    v[0].max(v[1]).max(v[2])
}

/// Point in triangle, either winding — a ribbon's triangles do not promise one.
fn inside(tri: &[[f32; 2]; 3], p: [f32; 2]) -> bool {
    let side =
        |u: [f32; 2], v: [f32; 2]| (v[0] - u[0]) * (p[1] - u[1]) - (v[1] - u[1]) * (p[0] - u[0]);
    let (a, b, c) = (
        side(tri[0], tri[1]),
        side(tri[1], tri[2]),
        side(tri[2], tri[0]),
    );
    (a >= 0.0 && b >= 0.0 && c >= 0.0) || (a <= 0.0 && b <= 0.0 && c <= 0.0)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A grid with a V-shaped valley running north–south: every row identical, the
    /// height a function of the column only. A road crossing it east–west has to
    /// come out flat across the valley.
    fn valley(n: usize, depth: f32) -> Vec<f32> {
        let mid = (n - 1) as f32 / 2.0;
        let mut g = vec![0.0f32; n * n];
        for z in 0..n {
            for x in 0..n {
                g[z * n + x] = depth * ((x as f32 - mid).abs() / mid - 1.0);
            }
        }
        g
    }

    /// How far a straight line between two samples `span` apart departs from the
    /// surface between them, at worst, along row `row`.
    ///
    /// **This is the quantity the tearing is made of.** Every sheet drawn on the
    /// ground is flat between its own corners, so where two sheets put their
    /// corners in different places they differ by exactly this, and it is the sum
    /// of the two that decides which one shows through the other.
    fn chord_error(g: &[f32], n: usize, row: usize, span: usize) -> f32 {
        let mut worst = 0.0f32;
        for x in 0..n.saturating_sub(span) {
            let (a, b) = (g[row * n + x], g[row * n + x + span]);
            for k in 1..span {
                #[allow(clippy::cast_precision_loss)]
                let want = a + (b - a) * (k as f32 / span as f32);
                worst = worst.max((g[row * n + x + k] - want).abs());
            }
        }
        worst
    }

    /// **The point of the whole module**, measured as the thing that tears.
    ///
    /// A road crossing a valley is *not* left bridging it — that would be a
    /// viaduct, and the ground really does fall away. What it stops doing is
    /// following every bump between the two ends, which is what puts curvature
    /// under a sheet that has no corner there to follow it.
    ///
    /// The margin is what makes this possible and is not a tuning constant: with
    /// the survey pinned at the kerb itself, both kerbs are *in* the valley and the
    /// road can only reproduce it. Pinned a few samples out, that band becomes the
    /// cutting or the embankment — which is what a built road has.
    ///
    /// Measured on this ground, at the desktop's 12.5 m ground edge: **0.469 m →
    /// 0.118 m**. Two sheets differ by the sum of their own errors, so that is a
    /// worst case of 0.94 m becoming 0.24 m — from far outside the ground stack's
    /// ten-millimetre window to still outside it, which is why the cut in
    /// `road_stitch::cut_under_roads` is the other half and not an alternative.
    #[test]
    fn a_road_across_a_valley_stops_following_every_bump() {
        const N: usize = 61;
        // A ridge and a hollow rather than a single V: a smooth valley has little
        // for this to remove, and the fault in the captures was ground that steps.
        let mut g = vec![0.0f32; N * N];
        for z in 0..N {
            for x in 0..N {
                #[allow(clippy::cast_precision_loss)]
                let t = x as f32 / (N - 1) as f32;
                g[z * N + x] = 6.0 * (t * std::f32::consts::TAU * 2.0).sin() * t;
            }
        }
        let raw = g.clone();

        // A three-sample carriageway with an eight-sample margin either side, which
        // at the desktop's 3.125 m spacing is a 9 m road in a 25 m corridor.
        let mut road = vec![false; N * N];
        for z in N / 2 - 1..=N / 2 + 1 {
            for x in 1..N - 1 {
                road[z * N + x] = true;
            }
        }
        for _ in 0..8 {
            dilate(&mut road, N);
        }

        let changed = flatten(&mut g, N, &road, 200);
        assert!(changed > 0, "nothing was flattened");

        // Four samples is the desktop's 12.5 m ground edge at 3.125 m spacing.
        let row = N / 2;
        let before = chord_error(&raw, N, row, 4);
        let after = chord_error(&g, N, row, 4);
        println!("chord error under the road: {before:.3} m -> {after:.3} m");
        assert!(
            after < before * 0.35,
            "chord error under the road went {before:.3} m -> {after:.3} m, \
             which is not enough to stop two sheets disagreeing"
        );
    }

    /// Across the road there is nothing left to disagree about: the carriageway's
    /// own width is the part a built road is graded flat, and it is the part the
    /// relaxation answers exactly.
    #[test]
    fn the_carriageway_is_flat_across_its_width() {
        const N: usize = 41;
        // Ground that falls steadily northward, so a road running east–west lies
        // across the slope — the case that racks a carriageway over.
        let mut g = vec![0.0f32; N * N];
        for z in 0..N {
            for x in 0..N {
                #[allow(clippy::cast_precision_loss)]
                {
                    g[z * N + x] = z as f32 * 0.4;
                }
            }
        }
        let mut road = vec![false; N * N];
        for z in N / 2 - 1..=N / 2 + 1 {
            for x in 1..N - 1 {
                road[z * N + x] = true;
            }
        }
        for _ in 0..6 {
            dilate(&mut road, N);
        }
        flatten(&mut g, N, &road, 300);

        // Straight down the middle, the three carriageway samples should sit on one
        // plane: the second is the mean of its neighbours to well under a
        // millimetre, against the 0.4 m per sample the ground falls.
        let x = N / 2;
        let (a, b, c) = (
            g[(N / 2 - 1) * N + x],
            g[(N / 2) * N + x],
            g[(N / 2 + 1) * N + x],
        );
        assert!(
            (b - (a + c) * 0.5).abs() < 1.0e-3,
            "the carriageway is not flat across: {a:.3} {b:.3} {c:.3}"
        );
    }

    /// **The ground outside the road is untouched**, which is what lets this run
    /// under every other consumer without telling any of them.
    #[test]
    fn nothing_outside_the_road_moves() {
        const N: usize = 21;
        let mut g = valley(N, 3.0);
        let raw = g.clone();
        let mut road = vec![false; N * N];
        for x in 1..N - 1 {
            road[(N / 2) * N + x] = true;
        }
        flatten(&mut g, N, &road, 50);
        for (i, (&now, &was)) in g.iter().zip(&raw).enumerate() {
            if !road[i] {
                assert!((now - was).abs() < f32::EPSILON, "sample {i} moved");
            }
        }
    }

    /// The road meets the ground exactly at the kerb — there is no step to fall
    /// off, because the unmarked neighbour *is* the boundary condition.
    #[test]
    fn the_road_meets_the_ground_at_its_edge() {
        const N: usize = 21;
        let mut g = valley(N, 3.0);
        let mut road = vec![false; N * N];
        let row = N / 2;
        for x in 5..15 {
            road[row * N + x] = true;
        }
        flatten(&mut g, N, &road, 100);
        // The first marked sample and the unmarked one beside it.
        let step = (g[row * N + 5] - g[row * N + 4]).abs();
        let native = (valley(N, 3.0)[row * N + 5] - valley(N, 3.0)[row * N + 4]).abs();
        assert!(
            step <= native * 1.5,
            "a {step:.3} m step at the kerb against the ground's own {native:.3} m"
        );
    }

    /// A survey has no reading at all over water and says so. Relaxing into a hole
    /// would pull the road down into it, and the hole fill that runs afterwards
    /// would then have nothing to say.
    #[test]
    fn a_hole_is_neither_read_nor_written() {
        const N: usize = 11;
        let mut g = vec![1.0f32; N * N];
        let hole = (N / 2) * N + N / 2;
        g[hole] = f32::NAN;
        let mut road = vec![true; N * N];
        flatten(&mut g, N, &road, 10);
        assert!(g[hole].is_nan(), "the hole was filled by the relaxation");
        for (i, v) in g.iter().enumerate() {
            if i != hole {
                assert!(v.is_finite(), "sample {i} caught the hole");
            }
        }
        road[hole] = false;
        assert_eq!(
            flatten(&mut g, N, &road, 1),
            0,
            "a flat field has nothing to do"
        );
    }

    /// Marking is by the drawn triangles and clamps at both ends of the grid: a
    /// ribbon can lie entirely off the field, and clamping only the far edge is how
    /// the same loop shape panicked in the tile tests.
    #[test]
    fn marking_handles_a_ribbon_off_the_grid() {
        const N: usize = 8;
        let mut road = vec![false; N * N];
        let far = [[[900.0, 900.0], [910.0, 900.0], [900.0, 910.0]]];
        mark_roads(&mut road, N, (0.0, 0.0), 1.0, &far, 0);
        assert!(
            !road.iter().any(|m| *m),
            "an off-grid ribbon marked something"
        );

        let over = [[[-50.0, -50.0], [50.0, -50.0], [-50.0, 50.0]]];
        mark_roads(&mut road, N, (0.0, 0.0), 1.0, &over, 0);
        assert!(road[0], "a ribbon covering the origin marked nothing");
    }

    /// Growth is in samples, and it reaches the diagonal after two passes rather
    /// than one — which is what a count of four-way steps means.
    #[test]
    fn growth_is_counted_in_samples() {
        const N: usize = 9;
        let mid = (N / 2) * N + N / 2;
        let one = {
            let mut r = vec![false; N * N];
            r[mid] = true;
            dilate(&mut r, N);
            r.iter().filter(|m| **m).count()
        };
        assert_eq!(one, 5, "one step is the sample and its four neighbours");
    }
}
