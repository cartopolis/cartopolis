//! Adds the concept of hierarchical, nesting [`Grid`]s, to group entities that move through space
//! together, like entities on a planet, rotating about the planet's axis, and, orbiting a star.

use crate::prelude::*;
use bevy_ecs::prelude::*;
use bevy_math::{prelude::*, Affine3A, DAffine3, DMat3, DVec3};
use bevy_reflect::prelude::*;
use bevy_transform::prelude::*;

use local_origin::LocalFloatingOrigin;

pub mod cell;
pub mod local_origin;
pub mod propagation;

/// A component that defines a spatial grid that child entities are located on. Child entities are
/// located on this grid with the [`GridCell`] component.
///
/// All entities with a [`GridCell`] must be children of an entity with a [`Grid`].
///
/// Grids are hierarchical, allowing more precision for objects with similar relative velocities.
/// All entities in the same grid will move together, like standard transform propagation, but with
/// much more precision.
///
/// Entities in the same grid as the [`FloatingOrigin`] will be rendered with the most precision.
/// Transforms are propagated starting from the floating origin, ensuring that grids in a similar
/// point in the hierarchy have accumulated the least error. Grids are transformed relative to each
/// other using 64-bit float transforms.
#[derive(Debug, Clone, Reflect, Component)]
#[reflect(Component)]
// We do not require the Transform, GlobalTransform, or GridCell, because these are not required in
// all cases: e.g. BigSpace should not have a Transform or GridCell.
pub struct Grid {
    /// The high-precision position of the floating origin's current grid cell local to this grid.
    local_floating_origin: LocalFloatingOrigin,
    /// Defines the uniform scale of the grid by the length of the edge of a grid cell.
    cell_edge_length: f32,
    /// How far an entity can move from the origin before its grid cell is recomputed.
    maximum_distance_from_origin: f32,
}

impl Default for Grid {
    fn default() -> Self {
        Self::new(2_000f32, 100f32)
    }
}

impl Grid {
    /// Construct a new [`Grid`]. The properties of a grid cannot be changed after construction.
    pub fn new(cell_edge_length: f32, switching_threshold: f32) -> Self {
        Self {
            local_floating_origin: LocalFloatingOrigin::default(),
            cell_edge_length,
            maximum_distance_from_origin: cell_edge_length / 2.0 + switching_threshold,
        }
    }

    /// Get the position of the floating origin relative to the current grid.
    #[inline]
    pub fn local_floating_origin(&self) -> &LocalFloatingOrigin {
        &self.local_floating_origin
    }

    /// Get the size of each cell this grid's grid.
    #[inline]
    pub fn cell_edge_length(&self) -> f32 {
        self.cell_edge_length
    }

    /// Get the grid's [`Self::maximum_distance_from_origin`].
    #[inline]
    pub fn maximum_distance_from_origin(&self) -> f32 {
        self.maximum_distance_from_origin
    }

    /// Compute the double precision position of an entity's [`Transform`] with respect to the given
    /// [`GridCell`] within this grid.
    #[inline]
    pub fn grid_position_double(&self, pos: &GridCell, transform: &Transform) -> DVec3 {
        DVec3 {
            x: pos.x as f64 * self.cell_edge_length as f64 + transform.translation.x as f64,
            y: pos.y as f64 * self.cell_edge_length as f64 + transform.translation.y as f64,
            z: pos.z as f64 * self.cell_edge_length as f64 + transform.translation.z as f64,
        }
    }

    /// Compute the single precision position of an entity's [`Transform`] with respect to the given
    /// [`GridCell`].
    #[inline]
    pub fn grid_position(&self, pos: &GridCell, transform: &Transform) -> Vec3 {
        Vec3 {
            x: pos.x as f64 as f32 * self.cell_edge_length + transform.translation.x,
            y: pos.y as f64 as f32 * self.cell_edge_length + transform.translation.y,
            z: pos.z as f64 as f32 * self.cell_edge_length + transform.translation.z,
        }
    }

    /// Returns the floating point position of a [`GridCell`].
    pub fn cell_to_float(&self, pos: &GridCell) -> DVec3 {
        DVec3 {
            x: pos.x as f64,
            y: pos.y as f64,
            z: pos.z as f64,
        } * self.cell_edge_length as f64
    }

    /// Convert a large translation into a small translation relative to a grid cell.
    #[inline]
    pub fn translation_to_grid(&self, input: impl Into<DVec3>) -> (GridCell, Vec3) {
        let l = self.cell_edge_length as f64;
        let input = input.into();
        let DVec3 { x, y, z } = input;

        if input.abs().max_element() < self.maximum_distance_from_origin as f64 {
            return (GridCell::default(), input.as_vec3());
        }

        let x_r = round(x / l);
        let y_r = round(y / l);
        let z_r = round(z / l);
        let t_x = x - x_r * l;
        let t_y = y - y_r * l;
        let t_z = z - z_r * l;

        (
            GridCell {
                x: x_r as GridPrecision,
                y: y_r as GridPrecision,
                z: z_r as GridPrecision,
            },
            Vec3::new(t_x as f32, t_y as f32, t_z as f32),
        )
    }

    /// Convert a large translation into a small translation relative to a grid cell.
    #[inline]
    pub fn imprecise_translation_to_grid(&self, input: Vec3) -> (GridCell, Vec3) {
        self.translation_to_grid(input.as_dvec3())
    }

    /// Compute the [`GlobalTransform`] of an entity in this grid.
    #[inline]
    pub fn global_transform(
        &self,
        local_cell: &GridCell,
        local_transform: &Transform,
    ) -> GlobalTransform {
        // The grid transform from the floating origin's grid, to the local grid.
        let transform_origin = self.local_floating_origin().grid_transform();
        // The grid cell offset of this entity relative to the floating origin's cell in this local
        // grid.
        let cell_origin_relative = *local_cell - self.local_floating_origin().cell();
        let grid_offset = self.cell_to_float(&cell_origin_relative);
        let translation = local_transform.translation.as_dvec3() + grid_offset;

        // Fast path: the origin grid is not rotated relative to this one, so the
        // 3x3 half of the affine multiply below is a multiply by the identity.
        //
        // This is not an exotic case, it is the common one. `LocalFloatingOrigin`
        // resets the grid that *contains* the floating origin to `(ZERO,
        // IDENTITY)` every frame (see `compute_all`), so any app with a single
        // grid — or any entity in the same grid as the camera, which is where
        // the entities you can actually see live — lands here every time. The
        // work skipped is a `DMat3` product: nine f64 multiplies and six adds
        // per entity per frame, thrown away against an identity.
        //
        // Bit-identical to the general path, not an approximation. `DAffine3`'s
        // `Mul` computes `matrix3 = a.matrix3 * b.matrix3` and `translation =
        // a.matrix3 * b.translation + a.translation`; with `a.matrix3` identity
        // those are exactly `b.matrix3` and `b.translation + a.translation`.
        if transform_origin.matrix3 == DMat3::IDENTITY {
            let mut matrix3 = DMat3::from_quat(local_transform.rotation.as_dquat());
            // Most entities are unit-scaled, and scaling the basis is three more
            // `DVec3` multiplies that all produce their own input.
            if local_transform.scale != Vec3::ONE {
                let scale = local_transform.scale.as_dvec3();
                matrix3 = DMat3::from_cols(
                    matrix3.x_axis * scale.x,
                    matrix3.y_axis * scale.y,
                    matrix3.z_axis * scale.z,
                );
            }
            return Affine3A {
                matrix3: matrix3.as_mat3().into(),
                translation: (translation + transform_origin.translation).as_vec3a(),
            }
            .into();
        }

        let local_transform = DAffine3::from_scale_rotation_translation(
            local_transform.scale.as_dvec3(),
            local_transform.rotation.as_dquat(),
            translation,
        );
        let global_64 = transform_origin * local_transform;

        Affine3A {
            matrix3: global_64.matrix3.as_mat3().into(),
            translation: global_64.translation.as_vec3a(),
        }
        .into()
    }
}

fn round(x: f64) -> f64 {
    #[cfg(feature = "libm")]
    {
        libm::round(x)
    }

    #[cfg(all(not(feature = "libm"), feature = "std"))]
    {
        x.round()
    }

    #[cfg(all(not(feature = "libm"), not(feature = "std")))]
    {
        compile_error!("Must enable the `libm` and/or `std` feature.");
        f64::NAN
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use bevy_math::{DQuat, EulerRot};
    use bevy_platform::prelude::Vec;

    /// `global_transform` before the identity-origin fast path was added, kept
    /// verbatim as the oracle.
    fn reference(grid: &Grid, local_cell: &GridCell, local_transform: &Transform) -> Affine3A {
        let transform_origin = grid.local_floating_origin().grid_transform();
        let cell_origin_relative = *local_cell - grid.local_floating_origin().cell();
        let grid_offset = grid.cell_to_float(&cell_origin_relative);
        let local = DAffine3::from_scale_rotation_translation(
            local_transform.scale.as_dvec3(),
            local_transform.rotation.as_dquat(),
            local_transform.translation.as_dvec3() + grid_offset,
        );
        let global_64 = transform_origin * local;
        Affine3A {
            matrix3: global_64.matrix3.as_mat3().into(),
            translation: global_64.translation.as_vec3a(),
        }
    }

    fn grid_with(origin: LocalFloatingOrigin) -> Grid {
        Grid {
            local_floating_origin: origin,
            cell_edge_length: 2_000.0,
            maximum_distance_from_origin: 1_100.0,
        }
    }

    /// A spread of cells, offsets, rotations and scales — including the unit
    /// scale that gets its own branch, and cells far enough out that the f64
    /// translation is doing real work.
    fn samples() -> Vec<(GridCell, Transform)> {
        let mut out = Vec::new();
        for (i, cell) in [
            GridCell::ZERO,
            GridCell::new(1, 0, -1),
            GridCell::new(-3_000, 17, 900_001),
        ]
        .into_iter()
        .enumerate()
        {
            for (j, scale) in [Vec3::ONE, Vec3::splat(2.5), Vec3::new(1.0, 3.0, 0.25)]
                .into_iter()
                .enumerate()
            {
                let f = (i * 3 + j) as f32;
                out.push((
                    cell,
                    Transform {
                        translation: Vec3::new(f * 13.0 - 40.0, f * -7.0, f * 91.0),
                        rotation: Quat::from_euler(EulerRot::YXZ, f * 0.31, f * 0.11, f * -0.7),
                        scale,
                    },
                ));
            }
        }
        out
    }

    /// The fast path claims to be *identical*, not merely close: with an
    /// identity origin rotation the skipped `DMat3` product was a multiply by
    /// the identity. Anything less than bit equality means it is a different
    /// path, not a shortcut.
    #[test]
    fn identity_origin_fast_path_is_bit_identical() {
        // What `LocalFloatingOrigin::compute_all` installs on the grid that holds
        // the floating origin, every frame.
        let grid = grid_with(LocalFloatingOrigin::new(
            GridCell::new(7, -2, 4),
            Vec3::ZERO,
            DQuat::IDENTITY,
        ));
        assert_eq!(
            grid.local_floating_origin().grid_transform().matrix3,
            DMat3::IDENTITY,
            "this test is only meaningful if the fast path is actually taken"
        );
        for (cell, transform) in samples() {
            let got = grid.global_transform(&cell, &transform).affine();
            assert_eq!(got, reference(&grid, &cell, &transform), "cell {cell:?}, {transform:?}");
        }
    }

    /// A grid rotated relative to the origin's must still take the general path
    /// and produce exactly what it always did.
    #[test]
    fn rotated_origin_still_takes_the_general_path() {
        let grid = grid_with(LocalFloatingOrigin::new(
            GridCell::new(-1, 5, 0),
            Vec3::new(12.0, -3.0, 7.5),
            DQuat::from_euler(EulerRot::YXZ, 0.4, -0.2, 1.1),
        ));
        assert_ne!(
            grid.local_floating_origin().grid_transform().matrix3,
            DMat3::IDENTITY,
            "this test is only meaningful if the fast path is *not* taken"
        );
        for (cell, transform) in samples() {
            let got = grid.global_transform(&cell, &transform).affine();
            assert_eq!(got, reference(&grid, &cell, &transform), "cell {cell:?}, {transform:?}");
        }
    }
}
