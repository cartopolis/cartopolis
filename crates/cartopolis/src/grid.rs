//! Constants of the floating-origin world frame (ADR-019/021), and the camera's
//! place in it.
//!
//! These describe the `BigSpace` grid itself, not any system that uses it. They
//! lived in `systems::free_camera` because that is where the `BigSpace` is
//! spawned, which made 20 modules import the *camera* to find out how wide a
//! world cell is — and made `free_camera` the highest-in-degree module in the
//! client for a reason that had nothing to do with cameras.
//!
//! Anything here is a plain scalar describing the frame, a pure conversion into
//! it, or [`AnchoredCamera`] — the one read of where the camera is that some
//! forty systems make. A value that a system owns and tunes belongs with that
//! system.

use bevy::ecs::system::SystemParam;
use bevy::math::{DVec3, Vec2, Vec3};
use bevy::prelude::{Entity, Query, Transform, With};
use big_space::prelude::{BigSpace, GridCell};

use crate::components::FreeCamera;

/// Grid cell edge for the world `BigSpace` (ADR-019/021). Large enough that local
/// play rarely recenters; small enough to keep within-cell f32 precise.
pub const WORLD_CELL_EDGE: f32 = 10_000.0;

/// Hysteresis buffer past a cell edge before big_space recenters an entity.
pub const WORLD_SWITCH_THRESHOLD: f32 = 100.0;

/// Where an entity is in the anchored world frame, in metres: its `big_space`
/// cell scaled by [`WORLD_CELL_EDGE`] plus its cell-local translation.
///
/// This is the frame every streamer places its cell origins in, the frame the
/// avatar's `position` is measured in, and the one a camera has to be brought
/// into before it can be compared with either. It was written out by hand at
/// every site that needed it. The arithmetic is exactly what those copies did
/// (an `f32` multiply, then an `f32` add, per axis), so a replaced site
/// produces the same bits — `matches_the_hand_written_form_bit_for_bit` pins it.
pub fn anchored_position(cell: &GridCell, translation: Vec3) -> Vec3 {
    Vec3::new(
        cell.x as f32 * WORLD_CELL_EDGE + translation.x,
        cell.y as f32 * WORLD_CELL_EDGE + translation.y,
        cell.z as f32 * WORLD_CELL_EDGE + translation.z,
    )
}

/// [`anchored_position`]'s ground-plane half: world X and Z, as most consumers
/// (the cell rings, the footprint index, the masks) want it.
pub fn anchored_xz(cell: &GridCell, translation: Vec3) -> Vec2 {
    Vec2::new(
        cell.x as f32 * WORLD_CELL_EDGE + translation.x,
        cell.z as f32 * WORLD_CELL_EDGE + translation.z,
    )
}

/// [`anchored_position`] in `f64`, for the consumers that turn it into an
/// altitude or a geodetic position: from orbit the cell term is millions of
/// metres and `f32` has no centimetres left. Every operand is widened *before*
/// the arithmetic, as the hand-written copies did.
pub fn anchored_position_f64(cell: &GridCell, translation: Vec3) -> DVec3 {
    DVec3::new(
        cell.x as f64 * WORLD_CELL_EDGE as f64 + translation.x as f64,
        cell.y as f64 * WORLD_CELL_EDGE as f64 + translation.y as f64,
        cell.z as f64 * WORLD_CELL_EDGE as f64 + translation.z as f64,
    )
}

/// The free camera's cell and cell-local transform, read-only.
///
/// `Query<(&GridCell, &Transform), With<FreeCamera>>` was spelled out in some
/// forty systems, most of them followed by the same conversion into the
/// anchored frame. This is that query and that conversion. Every accessor goes
/// through `Query::single`, so "no camera" and "more than one camera" both read
/// as `None` — what the `let Ok(..) = camera.single() else { return }` it
/// replaces did. A site that wants anything else reaches `query` directly.
///
/// A system that also writes `Transform` or `GridCell` on other entities still
/// needs the `Without<FreeCamera>` on *its* query: Bevy proves two queries
/// disjoint from a `Without`, never from two different `With`s.
#[derive(SystemParam)]
pub struct AnchoredCamera<'w, 's> {
    pub query: Query<'w, 's, (&'static GridCell, &'static Transform), With<FreeCamera>>,
}

impl AnchoredCamera<'_, '_> {
    /// The camera's cell and transform, or `None` unless exactly one exists.
    pub fn get(&self) -> Option<(&GridCell, &Transform)> {
        self.query.single().ok()
    }

    /// The camera's anchored world X and Z ([`anchored_xz`]).
    pub fn world_xz(&self) -> Option<Vec2> {
        self.get()
            .map(|(cell, tf)| anchored_xz(cell, tf.translation))
    }

    /// The camera in the anchored world frame, in `f64`
    /// ([`anchored_position_f64`]).
    pub fn world_position_f64(&self) -> Option<DVec3> {
        self.get()
            .map(|(cell, tf)| anchored_position_f64(cell, tf.translation))
    }
}

/// [`AnchoredCamera`] plus the `BigSpace` root entity — the pair a streamer
/// takes to decide what to build (where is the camera?) and to parent what it
/// built (under which grid?). One parameter, because several of those systems
/// sit at Bevy's sixteen-parameter cap.
#[derive(SystemParam)]
pub struct CameraAndRoot<'w, 's> {
    pub camera: AnchoredCamera<'w, 's>,
    pub root: Query<'w, 's, Entity, With<BigSpace>>,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cell(x: i64, y: i64, z: i64) -> GridCell {
        GridCell {
            x: x as _,
            y: y as _,
            z: z as _,
        }
    }

    #[test]
    fn the_origin_cell_is_the_translation() {
        let t = Vec3::new(12.5, -3.0, 7.25);
        assert_eq!(anchored_position(&cell(0, 0, 0), t), t);
        assert_eq!(anchored_xz(&cell(0, 0, 0), t), Vec2::new(12.5, 7.25));
        assert_eq!(
            anchored_position_f64(&cell(0, 0, 0), t),
            DVec3::new(12.5, -3.0, 7.25)
        );
    }

    #[test]
    fn each_axis_takes_its_own_cell_index() {
        let t = Vec3::new(1.0, 2.0, 3.0);
        let c = cell(2, -1, 5);
        let p = anchored_position(&c, t);
        assert_eq!(p.x, 2.0 * WORLD_CELL_EDGE + 1.0);
        assert_eq!(p.y, -WORLD_CELL_EDGE + 2.0);
        assert_eq!(p.z, 5.0 * WORLD_CELL_EDGE + 3.0);
        assert_eq!(anchored_xz(&c, t), Vec2::new(p.x, p.z));
    }

    /// The helpers replaced hand-written copies; this pins that they compute
    /// the same bits, including at an offset where `f32` rounding shows.
    #[test]
    fn matches_the_hand_written_form_bit_for_bit() {
        let c = cell(-731, 412, 98_765);
        let t = Vec3::new(4_999.937, -0.000_123, 3_141.592_7);
        let p = anchored_position(&c, t);
        assert_eq!(
            p.x.to_bits(),
            (c.x as f32 * WORLD_CELL_EDGE + t.x).to_bits()
        );
        assert_eq!(
            p.y.to_bits(),
            (c.y as f32 * WORLD_CELL_EDGE + t.y).to_bits()
        );
        assert_eq!(
            p.z.to_bits(),
            (c.z as f32 * WORLD_CELL_EDGE + t.z).to_bits()
        );
        let d = anchored_position_f64(&c, t);
        assert_eq!(
            d.y.to_bits(),
            (c.y as f64 * WORLD_CELL_EDGE as f64 + t.y as f64).to_bits()
        );
    }

    #[test]
    fn f64_keeps_what_f32_rounds_away() {
        // 600 cells up is 6,000 km, where an f32 step is half a metre.
        let c = cell(0, 600, 0);
        let t = Vec3::new(0.0, 0.25, 0.0);
        assert_eq!(anchored_position_f64(&c, t).y, 6_000_000.25);
        assert_ne!(f64::from(anchored_position(&c, t).y), 6_000_000.25);
    }
}
