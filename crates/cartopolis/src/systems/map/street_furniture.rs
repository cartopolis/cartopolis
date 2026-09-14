//! Street furniture's ECS half: the distance LOD and the materials. The
//! extraction and meshing it drives are pure and live in `systems::furniture`.

use crate::components::FurnitureSurface;
use bevy::prelude::*;
use big_space::prelude::GridCell;

use crate::components::FreeCamera;
use crate::systems::furniture::{paving_base, FURNITURE_FADE_END, FURNITURE_FADE_START};
use crate::systems::globe::GlobeState;
use crate::systems::map_geometry::MapSurfaces;
use crate::systems::vector_tiles::SurfaceGroup;

#[derive(Resource)]
pub struct FurnitureLod {
    pub factor: f32,
    /// Mirror of `DataLayers::furniture_enabled`, for the same reason
    /// `VegetationLod::enabled` exists.
    pub enabled: bool,
}

impl Default for FurnitureLod {
    fn default() -> Self {
        Self {
            factor: 0.0,
            enabled: true,
        }
    }
}

impl FurnitureLod {
    /// Whether newly spawned furniture should be visible at all.
    #[must_use]
    pub fn visibility(&self) -> Visibility {
        if !self.enabled || self.factor >= 1.0 {
            Visibility::Hidden
        } else {
            Visibility::Inherited
        }
    }
}

/// Ramp the furniture into the ground with altitude.
///
/// The same shape as `vegetation::update_vegetation_lod` and for the same reason —
/// nothing in this codebase switches on a threshold — but with two honest
/// differences.
///
/// **Only one colour moves.** A canopy fades toward the forest fill it is *part
/// of*, because hiding a wood's trees without darkening the wood steps the whole
/// polygon brighter. Street furniture aggregates to nothing: a bench is one speck
/// on a pale pavement and removing it changes no area's mean brightness. So there
/// is no second colour to keep in step — the group's base just walks to the paving
/// tone via [`paving_base`].
///
/// **The convergence is against the ground, not within the object.** The per-part
/// tones are baked into the mesh as vertex-colour multipliers, so a bin's dark body
/// stays darker than its rim all the way up the band; only the whole object's
/// contrast *against the pavement* collapses. That is the part that matters, and
/// the residue is affordable for a measurable reason: at [`FURNITURE_FADE_END`] a
/// 1.8 m bench subtends well under a pixel, so its internal contrast is below what
/// the display can resolve before the geometry is hidden at all.
pub fn update_furniture_lod(
    mut lod: ResMut<FurnitureLod>,
    dl: Res<crate::systems::data_layers::DataLayers>,
    mut materials: ResMut<Assets<crate::systems::ground_detail::GroundMaterial>>,
    surfaces: Res<MapSurfaces>,
    globe: Res<GlobeState>,
    camera: Query<(&GridCell, &Transform), With<FreeCamera>>,
    mut furniture: Query<&mut Visibility, With<FurnitureSurface>>,
    mut last: Local<Option<(f32, bool)>>,
) {
    let Ok((cell, tf)) = camera.single() else {
        return;
    };
    let alt = globe.altitude_m(cell, tf) as f32;
    let t = ((alt - FURNITURE_FADE_START) / (FURNITURE_FADE_END - FURNITURE_FADE_START))
        .clamp(0.0, 1.0);
    let factor = t * t * (3.0 - 2.0 * t);
    let enabled = dl.furniture_enabled;

    let previous = *last;
    if previous.is_some_and(|(p, e)| (p - factor).abs() < 0.002 && e == enabled) {
        return;
    }
    *last = Some((factor, enabled));
    lod.factor = factor;
    lod.enabled = enabled;

    let paving = paving_base(factor);
    if let Some(handle) = surfaces.material(SurfaceGroup::Furniture) {
        if let Some(mut mat) = materials.get_mut(handle) {
            mat.base.base_color = Color::linear_rgb(paving[0], paving[1], paving[2]);
        }
    }

    let was_hidden = previous.is_some_and(|(p, e)| !e || p >= 1.0);
    let is_hidden = !enabled || factor >= 1.0;
    if previous.is_none() || was_hidden != is_hidden {
        let v = if is_hidden {
            Visibility::Hidden
        } else {
            Visibility::Inherited
        };
        for mut vis in &mut furniture {
            *vis = v;
        }
    }
}
