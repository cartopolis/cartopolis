//! **Which cells a streamer wants, nearest first** — the arithmetic every
//! ring-shaped [`CellStream`] consumer was writing out by hand.
//!
//! Seven streamers (`tall_structures`, `street_detail`, `dikes`, `water_levels`,
//! `building_facts`, `lod22`, `osm_buildings`' far ring) plus the data layers
//! each carried the same three steps: find the base-zoom tile the camera is
//! over from the anchor tile, step it up to the streamer's own zoom with a
//! `div_euclid`, and walk a square of cells around it ranked by
//! [`fetch_gate::ring_distance`]. This module is those three steps once.
//!
//! What a cell *is* and whether it is wanted (a coverage manifest, the LoD2.2
//! hand-over, a layer toggle) stays with the caller, as the `keep` predicate.
//!
//! Not every cell streamer fits: `map_geometry` derives its camera cell from the
//! fractional anchor tile with `floor` rather than from the rounded base-zoom
//! tile, and `map_stream` / `globe` are LOD pyramids rather than square rings.

use bevy::prelude::{Transform, Vec2};
use big_space::prelude::GridCell;

use crate::grid::anchored_xz;
use crate::platform::fetch_gate;
use crate::platform::stream::CellStream;

/// The base-zoom tile the camera is over: the anchor tile `(ax, ay)` plus the
/// camera's anchored world position in whole tiles of `tile_m` metres, rounded.
#[must_use]
pub fn camera_tile(anchor: (i64, i64), tile_m: f32, cell: &GridCell, tf: &Transform) -> (i64, i64) {
    let Vec2 {
        x: world_x,
        y: world_z,
    } = anchored_xz(cell, tf.translation);
    (
        anchor.0 + (world_x / tile_m).round() as i64,
        anchor.1 + (world_z / tile_m).round() as i64,
    )
}

/// The zoom-`zoom` cell containing base-zoom (`z`) tile `tile`. `div_euclid`,
/// not `/`, so a tile west or north of the origin rounds away from zero like
/// every other tile does.
#[must_use]
pub fn coarsen(tile: (i64, i64), z: u32, zoom: u32) -> (i64, i64) {
    let shift = z.saturating_sub(zoom);
    (
        tile.0.div_euclid(1i64 << shift),
        tile.1.div_euclid(1i64 << shift),
    )
}

/// The cells within `radius` (Chebyshev) of `centre` that `keep` accepts,
/// nearest first.
///
/// `keep` is handed each cell and its ring distance from `centre`. The order is
/// row-major (`dy` outer, `dx` inner) within a ring, because the sort is stable
/// over a row-major walk — the exact order every hand-written copy produced,
/// which matters where the list is used for more than requesting (`lod22` picks
/// the first held cell to restore off it).
#[must_use]
pub fn ring(
    centre: (i64, i64),
    radius: i64,
    mut keep: impl FnMut((i64, i64), i64) -> bool,
) -> Vec<(i64, i64)> {
    let side = usize::try_from(2 * radius + 1).unwrap_or(0);
    let mut cells: Vec<(i64, i64)> = Vec::with_capacity(side * side);
    for dy in -radius..=radius {
        for dx in -radius..=radius {
            let c = (centre.0 + dx, centre.1 + dy);
            if keep(c, dx.abs().max(dy.abs())) {
                cells.push(c);
            }
        }
    }
    cells.sort_by_key(|c| fetch_gate::ring_distance(*c, centre.0, centre.1));
    cells
}

/// Ask `stream` for every cell of [`ring`], each ranked by its ring distance.
///
/// Call it every frame the streamer is live: [`CellStream::request_ranked`] is
/// the de-dup *and* the retry pump — a cell inside its backoff is one that has
/// been asked for and refused, and skipping the ask because the ring has not
/// moved would strand it there for good.
pub fn request_ring<R>(
    stream: &mut CellStream<(i64, i64), R>,
    centre: (i64, i64),
    radius: i64,
    now: f64,
    keep: impl FnMut((i64, i64), i64) -> bool,
) {
    for c in ring(centre, radius, keep) {
        let rank = fetch_gate::ring_distance(c, centre.0, centre.1);
        stream.request_ranked(c, rank, now);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The order the hand-written copies produced: nearest ring first, row-major
    /// inside a ring. A change here changes which cells win the fetch budget on a
    /// fresh anchor.
    #[test]
    fn the_ring_is_nearest_first_and_row_major_within_a_ring() {
        let cells = ring((10, 20), 1, |_, _| true);
        assert_eq!(cells[0], (10, 20));
        assert_eq!(
            &cells[1..],
            &[
                (9, 19),
                (10, 19),
                (11, 19),
                (9, 20),
                (11, 20),
                (9, 21),
                (10, 21),
                (11, 21)
            ]
        );
    }

    #[test]
    fn keep_sees_the_ring_distance() {
        let cells = ring((0, 0), 2, |_, d| d == 2);
        assert_eq!(cells.len(), 16);
        assert!(cells.iter().all(|c| c.0.abs().max(c.1.abs()) == 2));
    }

    #[test]
    fn coarsening_rounds_towards_negative_infinity() {
        assert_eq!(coarsen((-1, 5), 16, 14), (-1, 1));
        assert_eq!(coarsen((7, 8), 14, 14), (7, 8));
        assert_eq!(
            coarsen((7, 8), 12, 14),
            (7, 8),
            "a coarser base zoom does not shift"
        );
    }
}
