//! Rain, on the ground rather than only in the air.
//!
//! Until this, precipitation reached the picture in exactly one place —
//! `sky::update_rain`'s screen-space streaks — so a downpour was something drawn
//! *over* a city that stayed bone dry underneath it. The streaks are the least
//! convincing half of rain; what actually reads as wet is the ground going dark
//! and glossy and starting to hold a reflection of the sky.
//!
//! # What it changes, and why those three fields
//!
//! For each shared ground material, three numbers ramp with
//! `DevPanelState::precipitation`:
//!
//! * **Roughness down.** A wet surface is a rough one with the pits filled in.
//!   This is the whole effect — it is what turns a matte sheet into something
//!   that takes a highlight from the sun and a sheen from the sky. Every material
//!   here starts at 0.95, which is as matte as the parameter goes.
//! * **Reflectance up.** Bevy's dielectric default (0.5, i.e. ~4 % at normal
//!   incidence) is the reflectance of the *material*; a water film is a second
//!   interface on top of it and reflects more, most visibly at grazing angles
//!   down a street.
//! * **Base colour down.** Wet tarmac is darker than dry tarmac, and this is the
//!   half people notice without being able to name: the film is transparent, so
//!   light enters, bounces inside and comes back less of it.
//!
//! # Why the ratio and not a colour
//!
//! The darkening is a **multiplier on whatever the material already holds**,
//! recomputed from a remembered dry value rather than accumulated. Two reasons,
//! and the second is the one that would have bitten. Applying a factor to the
//! live value each frame compounds — a shower that lasted a minute would leave
//! the roads black for the rest of the session. And `systems::vegetation` already
//! writes `base_color` on some of these same handles as its altitude ramp moves,
//! so this cannot own the field outright; it has to be able to say what the
//! colour would be with no rain and rebuild from there.
//!
//! # Roads are an extended material, everything else is not
//!
//! The road groups carry `RoadDepthExtension` (a vertex shader that spreads the
//! coplanar road band over a depth ladder), so their handles live in a different
//! asset store from the plain ground's, with the `StandardMaterial` nested
//! inside. That is the only reason this module walks two maps instead of one; the
//! fields written are identical.
//!
//! # What this deliberately does not do
//!
//! No puddles, no ripples, no wet-only normal map. A puddle is a mask — where
//! water *collects* — and nothing in the streamed data says where the ground is
//! low. Faking it with noise puts standing water on a camber and up a kerb, which
//! is worse than a uniformly damp street. The water shader's own wave field is
//! the model for how that would have to be done, and it is a project of its own.

use bevy::asset::AssetId;
use bevy::prelude::*;
use std::collections::HashMap;

use crate::resources::DevPanelState;
use crate::systems::map_geometry::MapSurfaces;
use crate::systems::road_depth::RoadMaterial;
use cartopolis_geo::vector_tiles::SurfaceGroup;

/// Roughness at the heaviest rain.
///
/// Not zero, and not near it. A mirror-smooth street is a stage set: real wet
/// tarmac keeps its texture between the films of water, and at 0.95 → 0.35 the
/// sun already carries a long highlight down the carriageway. Dropping to 0.1
/// made every road a black mirror with a sun in it, which is the failure this
/// number exists to stay above.
const WET_ROUGHNESS: f32 = 0.35;

/// Reflectance at the heaviest rain, against bevy's 0.5 dielectric default.
const WET_REFLECTANCE: f32 = 0.62;

/// How much of the dry colour survives at the heaviest rain.
///
/// 0.72 is a little over a third of a stop. Measured against nothing — this is a
/// look, not a quantity — but chosen on the low side deliberately: the roads are
/// already the darkest thing in a daylit frame and taking them much further makes
/// a rainy city read as a night one.
const WET_DARKEN: f32 = 0.72;

/// Rain below this is not worth touching every material for.
///
/// The write itself is cheap, but a `get_mut` on an asset marks it changed, and a
/// changed `StandardMaterial` is re-prepared and its bind group rebuilt. Doing
/// that every frame for a precipitation of 0.001 is pure churn on the asset path
/// this codebase is otherwise careful about.
const RAIN_EPSILON: f32 = 0.01;

/// The dry appearance of each material, so the ramp is always computed from the
/// unmodified value instead of compounding on the last frame's.
///
/// Keyed by asset id rather than by group, because a group's handle is recreated
/// on every re-anchor and the *values* are what must not be inherited: a material
/// rebuilt while it is raining is created dry by its own recipe, and remembering
/// the previous handle's already-wetted look under the same group key would take
/// the ramp from the wrong place. Ids of materials that no longer exist are
/// dropped on each pass, which is what keeps this from growing with the session.
#[derive(Resource, Default)]
pub struct DrySurfaces {
    plain: HashMap<AssetId<crate::systems::ground_detail::GroundMaterial>, DryLook>,
    roads: HashMap<AssetId<RoadMaterial>, DryLook>,
    /// The wetness the materials were last written for, so a steady state costs
    /// nothing at all.
    applied: f32,
    /// How many materials that wetness was written to. A cell landing mid-shower
    /// creates a dry material that nothing would otherwise revisit, because the
    /// rain itself has not moved — this is what notices.
    applied_to: usize,
}

#[derive(Clone, Copy)]
struct DryLook {
    base_color: Color,
    roughness: f32,
    reflectance: f32,
}

/// Ramp every ground material between its dry look and a wet one.
///
/// Runs every frame and returns immediately unless the rain has actually moved,
/// which is the common case: precipitation changes when the weather fetch lands,
/// when a slider moves, or when `--rain` seeds a capture.
pub fn apply_wet_surfaces(
    dev: Res<DevPanelState>,
    surfaces: Res<MapSurfaces>,
    mut dry: ResMut<DrySurfaces>,
    mut plain: ResMut<Assets<crate::systems::ground_detail::GroundMaterial>>,
    mut roads: ResMut<Assets<RoadMaterial>>,
) {
    let wet = dev.precipitation.clamp(0.0, 1.0);
    let wet = if wet < RAIN_EPSILON { 0.0 } else { wet };
    let count = surfaces.ground_materials().count() + surfaces.road_materials().count();
    // The equality test is exact on purpose: this value is assigned from one
    // place and copied, not accumulated, so a tolerance would only add a way for
    // a small change to be silently dropped. The count beside it is what catches
    // a material created after the last write — see `applied_to`.
    if wet == dry.applied && count == dry.applied_to {
        return;
    }
    dry.applied = wet;
    dry.applied_to = count;

    let mut live_plain = Vec::new();
    let mut live_roads = Vec::new();
    for (group, handle) in surfaces.ground_materials() {
        live_plain.push(handle.id());
        if !hard_surface(group) {
            continue;
        }
        let Some(mut mat) = plain.get_mut(handle) else {
            continue;
        };
        let look = *dry
            .plain
            .entry(handle.id())
            .or_insert_with(|| DryLook::of(&mat.base));
        look.apply(&mut mat.base, wet);
    }
    for (group, handle) in surfaces.road_materials() {
        live_roads.push(handle.id());
        if !hard_surface(group) {
            continue;
        }
        let Some(mut mat) = roads.get_mut(handle) else {
            continue;
        };
        let look = *dry
            .roads
            .entry(handle.id())
            .or_insert_with(|| DryLook::of(&mat.base));
        look.apply(&mut mat.base, wet);
    }
    dry.plain.retain(|id, _| live_plain.contains(id));
    dry.roads.retain(|id, _| live_roads.contains(id));
}

/// Which surfaces get wet.
///
/// The hard ones, and nothing else. Wet grass and wet foliage do darken in life,
/// but they darken without becoming *glossy* — dropping a canopy to
/// [`WET_ROUGHNESS`] makes a tree look moulded rather than rained on, and the
/// group's colour is already owned by `systems::vegetation`'s altitude ramp, so
/// two systems would be writing the same field from different ideas of what it
/// should be. Sand and water are excluded for their own obvious reasons: one is
/// wet only at the edge, the other has a shader of its own.
fn hard_surface(group: SurfaceGroup) -> bool {
    matches!(
        group,
        SurfaceGroup::Road(_)
            | SurfaceGroup::RoadCasing(_)
            | SurfaceGroup::Paving
            | SurfaceGroup::Built
            | SurfaceGroup::BridgeDeck
    )
}

impl DryLook {
    fn of(mat: &StandardMaterial) -> Self {
        Self {
            base_color: mat.base_color,
            roughness: mat.perceptual_roughness,
            reflectance: mat.reflectance,
        }
    }

    /// Write this material's appearance at a given wetness, always from the dry
    /// values — see the module note on why this must not compound.
    fn apply(self, mat: &mut StandardMaterial, wet: f32) {
        let darken = 1.0 - (1.0 - WET_DARKEN) * wet;
        let dry = self.base_color.to_linear();
        mat.base_color = Color::linear_rgba(
            dry.red * darken,
            dry.green * darken,
            dry.blue * darken,
            dry.alpha,
        );
        mat.perceptual_roughness = self.roughness + (WET_ROUGHNESS - self.roughness) * wet;
        mat.reflectance = self.reflectance + (WET_REFLECTANCE - self.reflectance) * wet;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn matte() -> StandardMaterial {
        StandardMaterial {
            base_color: Color::linear_rgb(0.4, 0.4, 0.4),
            perceptual_roughness: 0.95,
            ..default()
        }
    }

    /// Dry must be a no-op, or a clear day slowly darkens every time this runs.
    #[test]
    fn no_rain_leaves_the_material_alone() {
        let mut mat = matte();
        let look = DryLook::of(&mat);
        look.apply(&mut mat, 0.0);
        assert_eq!(mat.perceptual_roughness, 0.95);
        assert_eq!(mat.base_color.to_linear().red, 0.4);
    }

    /// The bug the remembered dry look exists to prevent: applying the ramp over
    /// and over must land on the same place, not walk toward black.
    #[test]
    fn repeated_application_does_not_compound() {
        let mut mat = matte();
        let look = DryLook::of(&mat);
        look.apply(&mut mat, 1.0);
        let once = mat.base_color.to_linear().red;
        for _ in 0..30 {
            look.apply(&mut mat, 1.0);
        }
        assert!((mat.base_color.to_linear().red - once).abs() < 1e-6);
    }

    /// Wet is darker and glossier than dry, in that order of obviousness.
    #[test]
    fn rain_darkens_and_smooths() {
        let mut mat = matte();
        let look = DryLook::of(&mat);
        look.apply(&mut mat, 1.0);
        assert!(mat.base_color.to_linear().red < 0.4);
        assert!(mat.perceptual_roughness < 0.95);
        assert!(mat.reflectance > 0.5);
    }

    /// A mirror is not the target — see [`WET_ROUGHNESS`].
    #[test]
    fn the_wettest_road_is_not_a_mirror() {
        assert!(WET_ROUGHNESS > 0.2);
    }

    /// The vegetation groups must stay out of this: their colour belongs to
    /// `systems::vegetation`, and two systems writing one field is the bug.
    #[test]
    fn only_hard_surfaces_get_wet() {
        assert!(hard_surface(SurfaceGroup::Paving));
        assert!(hard_surface(SurfaceGroup::BridgeDeck));
        assert!(!hard_surface(SurfaceGroup::Canopy));
        assert!(!hard_surface(SurfaceGroup::Grass));
        assert!(!hard_surface(SurfaceGroup::Water));
    }
}
