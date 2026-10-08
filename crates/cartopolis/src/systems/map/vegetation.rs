//! The vegetation layer's ECS half: the altitude ramp, spawn visibility and
//! materials. The scattering and meshing it drives are pure and live in
//! [`crate::systems::scatter`].
//!
//! The altitude transition is the part that needs care, and it is the reason
//! nothing here switches on a boolean — see [`update_vegetation_lod`].

use crate::components::VegetationSurface;
use bevy::prelude::*;

use crate::grid::AnchoredCamera;
use crate::systems::globe::GlobeState;
use crate::systems::map_geometry::MapSurfaces;
use crate::systems::scatter::{mix, mix_rgb, srgb_to_linear_rgb, TREE_FADE_END, TREE_FADE_START};
use crate::systems::vector_tiles::{palette, SurfaceGroup};

// ─── Altitude ramp ───────────────────────────────────────────────────────────

/// How far the trees have merged into the flat fill: 0 = individual trees, 1 =
/// fully merged. Read at spawn by [`crate::systems::map_geometry`] so a cell that
/// streams in above the band does not flash a forest for one frame.
#[derive(Resource)]
pub struct VegetationLod {
    pub factor: f32,
    /// Mirror of `DataLayers::trees_enabled`, so [`Self::visibility`] (read at
    /// spawn) reflects the Trees toggle and a new cell doesn't flash a forest
    /// that the user has switched off.
    pub enabled: bool,
}

impl Default for VegetationLod {
    fn default() -> Self {
        Self {
            factor: 0.0,
            enabled: true,
        }
    }
}

impl VegetationLod {
    /// Whether newly spawned vegetation should be visible at all: off if the Trees
    /// layer is disabled, or if the altitude LOD has fully merged the canopy away.
    #[must_use]
    pub fn visibility(&self) -> Visibility {
        if !self.enabled || self.factor >= 1.0 {
            Visibility::Hidden
        } else {
            Visibility::Inherited
        }
    }
}

/// Ramp the trees into the fill with altitude.
///
/// Nothing here switches on a threshold, for the same reason the space tier
/// crossfades on `space_factor` instead of on `in_space`: a step in brightness in
/// the middle of a zoom is far more noticeable than the detail it is hiding.
///
/// The trick that makes it seam-free is that **two** colours move at once:
///
/// * the `Forest` fill ramps `FOREST → FOREST_CANOPIED`, because a forest seen
///   from far enough that its trees have merged *is* darker than bare woodland
///   floor — hiding the trees without this brightens every wood by a visible step;
/// * the canopy ramps toward whatever the fill currently is, so by the end of the
///   band a canopy is exactly the colour of the ground under it. It has no
///   silhouette left to lose, which is what lets the geometry be hidden at
///   `factor == 1` with nothing visibly happening.
///
/// No transparency is involved: alpha on a hundred thousand triangles would cost a
/// sort order and a pass, to fade something that is by then sub-pixel anyway.
pub fn update_vegetation_lod(
    mut lod: ResMut<VegetationLod>,
    dl: Res<crate::systems::data_layers::DataLayers>,
    mut materials: ResMut<Assets<crate::systems::ground_detail::GroundMaterial>>,
    surfaces: Res<MapSurfaces>,
    globe: Res<GlobeState>,
    camera: AnchoredCamera,
    mut veg: Query<&mut Visibility, With<VegetationSurface>>,
    mut last: Local<Option<(f32, bool)>>,
) {
    let Some((cell, tf)) = camera.get() else {
        return;
    };
    let alt = globe.altitude_m(cell, tf) as f32;
    let t = ((alt - TREE_FADE_START) / (TREE_FADE_END - TREE_FADE_START)).clamp(0.0, 1.0);
    // Smoothstep: the ends of the band matter more than its middle, because that
    // is where a linear ramp shows its corners.
    let factor = t * t * (3.0 - 2.0 * t);
    let enabled = dl.trees_enabled;

    let previous = *last;
    // Re-run when the ramp moves *or* the Trees toggle flips (same altitude, but
    // the geometry has to appear/disappear).
    if previous.is_some_and(|(p, e)| (p - factor).abs() < 0.002 && e == enabled) {
        return;
    }
    *last = Some((factor, enabled));
    lod.factor = factor;
    lod.enabled = enabled;

    let fill = mix(palette::FOREST, palette::FOREST_CANOPIED, factor);
    set_base_color(&mut materials, &surfaces, SurfaceGroup::Forest, fill);
    set_base_color(
        &mut materials,
        &surfaces,
        SurfaceGroup::Canopy,
        mix_rgb(srgb_to_linear_rgb(palette::CANOPY), fill, factor),
    );
    set_base_color(
        &mut materials,
        &surfaces,
        SurfaceGroup::Trunk,
        mix_rgb(srgb_to_linear_rgb(palette::TRUNK), fill, factor),
    );

    // Only touch visibility when the hidden state actually flips — the far end of
    // the band, or the Trees toggle — leaving thousands of entities alone otherwise.
    let was_hidden = previous.is_some_and(|(p, e)| !e || p >= 1.0);
    let is_hidden = !enabled || factor >= 1.0;
    if previous.is_none() || was_hidden != is_hidden {
        let v = if is_hidden {
            Visibility::Hidden
        } else {
            Visibility::Inherited
        };
        for mut vis in &mut veg {
            *vis = v;
        }
    }
}

fn set_base_color(
    materials: &mut Assets<crate::systems::ground_detail::GroundMaterial>,
    surfaces: &MapSurfaces,
    group: SurfaceGroup,
    linear: [f32; 3],
) {
    let Some(handle) = surfaces.material(group) else {
        return;
    };
    let Some(mut mat) = materials.get_mut(handle) else {
        return;
    };
    // Times the headroom the group's vertex tints were divided by when they
    // were packed into bytes (`mesh_format::tint_headroom`).
    let h = crate::systems::mesh_format::tint_headroom(group);
    mat.base.base_color = Color::linear_rgb(linear[0] * h, linear[1] * h, linear[2] * h);
}
