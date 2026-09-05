//! The volume the sun's light shafts are marched through, and the rules that keep
//! it over the camera and out of the way.
//!
//! Bevy's volumetric fog is not a global setting: the camera's `VolumetricFog`
//! turns the *pass* on, a directional light's `VolumetricLight` says which light
//! casts shafts (`sky::spawn_sun_light`), and a [`FogVolume`] is the only place
//! either of them has any effect. Without the volume the pass runs and renders
//! nothing, which is the most expensive way to draw an unchanged frame.
//!
//! # Why it follows the camera rather than covering the world
//!
//! A `FogVolume` is a unit cube placed by its `Transform`, and the march samples
//! it at `VolumetricFog::step_count` steps between the near plane and whatever it
//! hits. Two facts decide the size. The steps are spread over the *volume*, so a
//! box big enough to hold the streamed map (tens of kilometres) puts them
//! hundreds of metres apart and the fog turns to slabs. And the effect is a
//! near-field one anyway — a shaft is light picked out by air between you and the
//! thing occluding it, and past a kilometre the distance fog
//! (`free_camera::distance_fog`) has already taken over. So the volume is a few
//! hundred metres on a side and travels with the viewer, which is the same
//! bargain the star shell and the moon disc strike in `systems::sky`.
//!
//! It is placed from the camera's **`GlobalTransform`**, exactly as
//! `sky::update_star_sky` places the star shell, and that is the `big_space`
//! detail worth stating: the camera is the floating origin, so its global
//! translation is ~0 whatever the grid cell says, and copying it keeps the volume
//! centred on the viewer without ever touching a `GridCell`. Copying the
//! *rotation* would be wrong — a box of air does not turn when you turn your head
//! — so only the translation is taken.
//!
//! # What drives the density
//!
//! The same two knobs the rest of the weather already answers to, so the shafts
//! cannot disagree with the haze around them: `DevPanelState::fog_density` (the
//! distance fog's own control, and what `--fog` seeds) and
//! `DevPanelState::precipitation` (what `--rain` seeds, and what the live weather
//! writes when it is on). Rain is the case worth having — shafts through a downpour
//! are most of what makes rain read as weather rather than as a particle effect
//! over a dry city.
//!
//! Night is not a special case here and must not become one: the sun's
//! `VolumetricLight` stops casting when the sun light itself goes dim, so the
//! shafts fade out with the light that makes them rather than on a clock.

use bevy::light::FogVolume;
use bevy::prelude::*;
use big_space::prelude::GridCell;

use crate::components::FreeCamera;
use crate::resources::DevPanelState;

/// Marks the single fog volume this module owns.
#[derive(Component)]
pub struct ShaftVolume;

/// Edge length of the volume, in metres.
///
/// Chosen against the step count rather than against the scene: at
/// `render_features::MARCH_STEPS` = 48 this is a sample every ~12 m, which is fine
/// for something as low-frequency as fog. Doubling it would halve the sample
/// density and put visible slabs across the frame — so if this grows, the step
/// count has to grow with it.
const VOLUME_EDGE_M: f32 = 600.0;

/// Fog density in clear air.
///
/// Deliberately low. The volume is the whole near field, so this multiplies
/// against every metre between the eye and the geometry; the value that makes a
/// *shaft* visible is far below the value that makes the air look foggy.
const CLEAR_DENSITY: f32 = 0.012;

/// Density at the heaviest rain, before `fog_density` is applied.
const RAIN_DENSITY: f32 = 0.055;

/// Spawn the volume, inside the big-space root so it shares the camera's frame.
///
/// Called by `render_features::apply_render_features` rather than from a
/// `Startup` system of its own, because the answer it depends on — the adapter's
/// capability and the person's preference — can change while the app runs, and
/// because a volume with no camera pass to consume it is not free: bevy extracts
/// and uniform-buffers every `FogVolume` in the world whether or not anything
/// marches through it.
pub fn spawn_volume(commands: &mut Commands, root: Entity) {
    commands.entity(root).with_children(|parent| {
        parent.spawn((
            FogVolume {
                density_factor: CLEAR_DENSITY,
                // Warm and very slightly desaturated, so a shaft reads as sunlit
                // air rather than as a white beam. The sun's own colour is
                // already in the light, and multiplying two saturated warms
                // makes a stage effect.
                fog_color: Color::srgb(0.94, 0.93, 0.90),
                // Scattering above absorption: this is air, which redirects far
                // more light than it swallows. The reverse reads as smoke.
                scattering: 0.55,
                absorption: 0.14,
                ..default()
            },
            Transform::from_scale(Vec3::splat(VOLUME_EDGE_M)),
            // The grid cell is written every frame by `follow_camera`, but it has
            // to exist from the first frame or the volume renders one frame at
            // the origin of whatever cell it was spawned into.
            GridCell::default(),
            ShaftVolume,
        ));
    });
}

/// Keep the volume over the camera and its density in step with the weather.
///
/// Cheap enough to run unconditionally — one transform write and one field write
/// on one entity — and it returns immediately when the volume does not exist,
/// which is every tier below Desktop.
pub fn follow_camera(
    camera: Query<&GlobalTransform, (With<FreeCamera>, Without<ShaftVolume>)>,
    dev: Res<DevPanelState>,
    mut volume: Query<(&mut Transform, &mut FogVolume), With<ShaftVolume>>,
) {
    let Ok((mut tf, mut fog)) = volume.single_mut() else {
        return;
    };
    let Ok(cam) = camera.single() else {
        return;
    };
    // Translation only: see the module note. The volume is axis-aligned in the
    // world and stays that way.
    tf.translation = cam.translation();

    // `fog_density` is the distance fog's control and runs 0..~0.02; normalising
    // it against its own useful range keeps this in step with the haze the rest of
    // the scene draws instead of inventing a second scale for the same weather.
    let haze = (dev.fog_density / 0.02).clamp(0.0, 1.5);
    let wet = dev.precipitation.clamp(0.0, 1.0);
    fog.density_factor = (CLEAR_DENSITY + (RAIN_DENSITY - CLEAR_DENSITY) * wet) * (0.5 + haze);
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The sample spacing the step count assumes. A volume that grows without the
    /// march growing with it is the banding failure the constant's own comment
    /// warns about, so the relation is pinned rather than described.
    #[test]
    fn the_volume_is_sampled_at_least_every_fifteen_metres() {
        let steps = f64::from(crate::systems::render_features::MARCH_STEPS);
        let per_sample = f64::from(VOLUME_EDGE_M) / steps;
        assert!(
            per_sample <= 15.0,
            "{VOLUME_EDGE_M} m over {steps} steps is {per_sample} m per sample"
        );
    }

    /// Rain must not make the air *clearer*, which is the sign error this
    /// interpolation would fail silently on — the picture would simply look
    /// slightly wrong in the weather nobody screenshots.
    #[test]
    fn rain_is_thicker_than_clear_air() {
        assert!(RAIN_DENSITY > CLEAR_DENSITY);
    }
}
