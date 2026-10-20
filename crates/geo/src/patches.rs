//! Putting what will be cut into upload-sized pieces in an order that keeps each
//! piece in one place.
//!
//! A cell's furniture, its trees and every ground surface over the byte cap are
//! cut into pieces of at most so many bytes, and **each piece is one entity, culled
//! as a whole by its bounding box** — against the camera, and, for whatever casts,
//! against each of the sun's four cascades. A piece filled in source order is
//! filled from wherever the source happened to list things, so its box spans the
//! cell: at street level over Groningen (`deck-1`, 2026-10-20) a cell's furniture
//! was ~12 pieces each about as wide as the cell, and every one of them was drawn
//! into all four cascades and the main pass, ~4.8 M triangles five times a frame.
//!
//! Visiting the items along a Hilbert curve over [`PATCH_M`] squares first fills
//! each piece from one compact patch instead. Nothing is added, dropped or moved,
//! and the number of pieces is the same; only their boxes shrink, which is what
//! lets the culling bevy already does take the pieces out of view.
//!
//! **Hilbert and not Z order**, which is the obvious cheaper key: a Z curve jumps
//! diagonally across its square at every quadrant boundary, so the one piece that
//! straddles the jump between the cell's second and third quadrants spans the whole
//! cell again. Consecutive squares on a Hilbert curve always touch.

/// Side of the squares the curve visits, metres. Small against a piece (a 4 MB
/// furniture piece holds a few hundred objects), so a piece is a patch of many
/// squares rather than a square cut in half.
pub const PATCH_M: f32 = 16.0;

/// Squares per side of the curve's frame: 65,536 × 16 m, far past any cell.
const SIDE: u32 = 1 << 16;

/// Position on the Hilbert curve of a point in a cell's local frame.
///
/// Offset so the buffer a tile carries past its edges stays positive, and clamped
/// so a point outside every frame this is used in sorts to an end rather than
/// wrapping into the middle.
#[must_use]
pub fn curve_key(x: f32, z: f32) -> u64 {
    const OFFSET_M: f32 = (SIDE / 2) as f32 * PATCH_M;
    let q = |v: f32| ((v + OFFSET_M) / PATCH_M).clamp(0.0, (SIDE - 1) as f32) as u32;
    hilbert(q(x), q(z))
}

/// The classic xy → d walk over a [`SIDE`]² grid.
fn hilbert(mut x: u32, mut y: u32) -> u64 {
    let mut d = 0u64;
    let mut s = SIDE / 2;
    while s > 0 {
        let rx = u32::from(x & s != 0);
        let ry = u32::from(y & s != 0);
        d += u64::from(s) * u64::from(s) * u64::from((3 * rx) ^ ry);
        // Rotate the quadrant so the sub-curve enters where the last one left.
        if ry == 0 {
            if rx == 1 {
                x = SIDE - 1 - x;
                y = SIDE - 1 - y;
            }
            std::mem::swap(&mut x, &mut y);
        }
        s /= 2;
    }
    d
}

/// Indices of `items` in curve order of `xz(item)`. Stable, so items in one square
/// keep their source order.
#[must_use]
pub fn spatial_order<T>(items: &[T], xz: impl Fn(&T) -> [f32; 2]) -> Vec<usize> {
    let mut keyed: Vec<(u64, u32)> = items
        .iter()
        .enumerate()
        .map(|(i, item)| {
            let [x, z] = xz(item);
            (curve_key(x, z), i as u32)
        })
        .collect();
    // The index is the second half of the key, so an unstable sort is stable here.
    keyed.sort_unstable();
    keyed.into_iter().map(|(_, i)| i as usize).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Box area of each consecutive run of `per` items, as a share of the cell's.
    fn run_boxes(pts: &[[f32; 2]], order: &[usize], per: usize) -> Vec<f32> {
        order
            .chunks(per)
            .map(|run| {
                let (mut lo, mut hi) = ([f32::MAX; 2], [f32::MIN; 2]);
                for &i in run {
                    for k in 0..2 {
                        lo[k] = lo[k].min(pts[i][k]);
                        hi[k] = hi[k].max(pts[i][k]);
                    }
                }
                (hi[0] - lo[0]) * (hi[1] - lo[1]) / (1490.0 * 1490.0)
            })
            .collect()
    }

    /// A 1.5 km cell of points on a 10 m grid, **listed in no particular order** —
    /// which is how a tile lists its benches — cut into runs of 1,000, about a
    /// 22nd of the cell each. In source order every run's box is the cell; along
    /// the curve each must be a patch.
    #[test]
    fn neighbours_on_the_curve_are_neighbours_on_the_ground() {
        let mut pts: Vec<[f32; 2]> = (0..150)
            .flat_map(|j| (0..150).map(move |i| [i as f32 * 10.0, j as f32 * 10.0]))
            .collect();
        let mut seed = 0x2545_F491_4F6C_DD1Du64;
        for i in (1..pts.len()).rev() {
            seed ^= seed << 13;
            seed ^= seed >> 7;
            seed ^= seed << 17;
            pts.swap(i, (seed % (i as u64 + 1)) as usize);
        }
        let source: Vec<usize> = (0..pts.len()).collect();
        let before = run_boxes(&pts, &source, 1_000);
        assert!(
            before.iter().all(|&a| a > 0.9),
            "the shuffle left order behind"
        );

        let order = spatial_order(&pts, |p| *p);
        assert_eq!(order.len(), pts.len());
        let after = run_boxes(&pts, &order, 1_000);
        let worst = after.iter().copied().fold(0.0f32, f32::max);
        let mean = after.iter().sum::<f32>() / after.len() as f32;
        assert!(worst < 0.25, "a run's box covers {worst} of the cell");
        assert!(
            mean < 0.12,
            "the runs' boxes cover {mean} of the cell on average"
        );
    }

    #[test]
    fn consecutive_squares_touch() {
        let mut cells: Vec<(u64, u32, u32)> = (0..64u32)
            .flat_map(|y| (0..64u32).map(move |x| (hilbert(x, y), x, y)))
            .collect();
        cells.sort_unstable();
        for w in cells.windows(2) {
            let (dx, dy) = (w[0].1.abs_diff(w[1].1), w[0].2.abs_diff(w[1].2));
            assert_eq!(dx + dy, 1, "{:?} → {:?} is a jump", w[0], w[1]);
        }
    }

    #[test]
    fn every_item_is_visited_once_and_ties_keep_their_order() {
        let pts = [
            [5.0, 5.0],
            [-30.0, 2000.0],
            [1.0, 1.0],
            [5.0, 5.0],
            [1e9, -1e9],
        ];
        let order = spatial_order(&pts, |p| *p);
        let mut seen = order.clone();
        seen.sort_unstable();
        assert_eq!(seen, vec![0, 1, 2, 3, 4]);
        let a = order.iter().position(|&i| i == 0).unwrap();
        let b = order.iter().position(|&i| i == 3).unwrap();
        assert!(a < b, "two items in one square swapped places");
    }
}
