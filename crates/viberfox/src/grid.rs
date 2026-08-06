//! Constants of the floating-origin world frame (ADR-019/021).
//!
//! These describe the `BigSpace` grid itself, not any system that uses it. They
//! lived in `systems::free_camera` because that is where the `BigSpace` is
//! spawned, which made 20 modules import the *camera* to find out how wide a
//! world cell is — and made `free_camera` the highest-in-degree module in the
//! client for a reason that had nothing to do with cameras.
//!
//! Anything here is a plain scalar describing the frame. A value that a system
//! owns and tunes belongs with that system.

/// Grid cell edge for the world `BigSpace` (ADR-019/021). Large enough that local
/// play rarely recenters; small enough to keep within-cell f32 precise.
pub const WORLD_CELL_EDGE: f32 = 10_000.0;

/// Hysteresis buffer past a cell edge before big_space recenters an entity.
pub const WORLD_SWITCH_THRESHOLD: f32 = 100.0;
