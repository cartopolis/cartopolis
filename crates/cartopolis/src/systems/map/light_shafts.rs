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
//! It is placed by copying the camera's **`GridCell` and cell-local
//! `Transform`**, both, because this entity lives in the big_space grid and a
//! cell-local transform means nothing without the cell it is local to. Taking
//! only the translation — from the camera's `GlobalTransform`, on the reasoning
//! that the floating origin makes it ~0 — leaves the volume pinned to cell
//! (0, 0, 0) and therefore a whole number of cells away from the viewer as soon
//! as they leave it; see [`follow_camera`] for why that reads as the feature
//! doing nothing rather than as anything being wrong. Copying the *rotation*
//! would be wrong — a box of air does not turn when you turn your head.
//!
//! # What drives the density
//!
//! The same two knobs the rest of the weather already answers to, so the shafts
//! cannot disagree with the haze around them: `DevPanelState::fog_density` (the
//! distance fog's own control, and what `--fog` seeds) and
//! `DevPanelState::precipitation` (what `--rain` seeds, and what the live weather
//! writes when it is on). **Both run 0..1** — `fog_density` is the Weather slider
//! in Settings ▸ World and defaults to 0.15. An earlier version of this module
//! read it as if it ran 0..0.02, which is the scale `FogFalloff`'s *coefficient*
//! uses rather than the scale of the control, and the consequence was not subtle:
//! the multiplier saturated at its ceiling for any weather at all, and see below
//! for what that did.
//!
//! # A fog volume darkens what you look at through it, and that is the trap
//!
//! The march is Beer's law twice over. It adds the sunlight scattered into the
//! ray — the shafts, which is what this is for — and it also *attenuates the
//! background* by `exp(-density × (absorption + scattering) × distance)` along the
//! way. The second half is easy to forget because it is not what the feature is
//! called, and it is measured in the volume's own metres: with the density this
//! module shipped with, the volume passed 19% of the background at 100 m and
//! **0.7% at 300 m** — i.e. wherever the volume was actually over the viewer, it
//! turned the world off. That qualifier is the placement bug above, and the two
//! together are why the symptom was reported as intermittent: the darkness only
//! arrived while the camera was in the one grid cell the volume was pinned to.
//! Bevy's own default of 0.1 is not a counter-example — it is meant for a volume
//! a few metres across, where the same number is a tenth of the optical depth.
//!
//! So the densities here are not written down directly. They are stated as *how
//! much of the background the air may swallow* over the volume's own half-edge
//! ([`CLEAR_EXTINCTION`], [`RAIN_EXTINCTION`]) and [`density_for`] inverts Beer's
//! law to get the number bevy wants. That way the constant cannot silently mean
//! something else when [`VOLUME_EDGE_M`] or the scattering coefficients move, and
//! the value that is easy to sanity-check is the one written in the file.
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

/// Main-pass pixels above which "Automatic" leaves the shafts off.
///
/// Bevy's march runs at full main-pass resolution, `MARCH_STEPS` shadow-map
/// samples per pixel, with no reduced-resolution option — so its cost is pixels
/// times steps and nothing else. Measured 2026-10-15 on an M5 Pro over Groningen
/// at street level (`--shot --hide-ui --time 12 --clouds 0 --rain 0`, two runs
/// each): 38 ms a frame with the shafts at 3134×1952, a MacBook's Retina panel,
/// against 12.5 ms without — ~4 ms per megapixel. 4 MP keeps them on at 1440p
/// and below, where they cost what they were judged worth, and drops them on
/// Retina and 4K, where they were most of the frame. A person who asks for them
/// in Settings still gets them; this only decides what nobody asked about.
pub const AUTO_MAX_PIXELS: u64 = 4_000_000;

/// Whether "Automatic" can afford the shafts at a main pass of `pixels`.
///
/// 0 — the camera has no target yet — answers yes, so the first frames of a
/// small window are not drawn without them only to have them appear.
#[must_use]
pub fn affordable_at(pixels: u64) -> bool {
    pixels <= AUTO_MAX_PIXELS
}

/// How much light the medium takes out of a ray per metre per unit of density:
/// the `absorption + scattering` sum the march multiplies by. Written once here
/// so [`density_for`] and [`spawn_volume`] cannot drift apart about it.
const SCATTERING: f32 = 0.55;
const ABSORPTION: f32 = 0.14;

/// The longest a view ray can run inside the volume before leaving it.
///
/// The box is centred on the eye, so looking straight along an axis that is half
/// an edge. (A corner is √3 times further, but a ray that leaves through a corner
/// has spent most of its length far off to the side of what you are looking at,
/// and sizing the air for it would make the ordinary view too thin.)
const HALF_EDGE_M: f32 = VOLUME_EDGE_M * 0.5;

/// Fraction of the background the air may swallow over [`HALF_EDGE_M`] of clear
/// air, at the heaviest fog the weather can ask for.
///
/// This is the number to argue about, and it is the one real trade in the file:
/// the in-scattered light that *is* a shaft is proportional to the same density
/// as the extinction that dims the scene, so a brighter beam and a darker world
/// are the same knob. Which is why it is small. The volume is the whole near
/// field, so whatever it takes it takes from *everything* — the buildings, the
/// map, the avatar — while a shaft only has to out-read the air beside it. The
/// distance fog (`free_camera::distance_fog`) is what is supposed to draw haze,
/// and it does it without touching what you can see through.
///
/// At the weather's default (`fog_density` 0.15) this works out at ~5% over the
/// half-edge and under 1% across a street, which is the shape wanted: the near
/// scene is untouched and the length of a boulevard is where a beam gathers.
const CLEAR_EXTINCTION: f32 = 0.12;

/// …and the same fraction in the heaviest rain. Higher because a downpour really
/// does close the near field down, and because rain is the case the shafts are
/// most worth having in.
const RAIN_EXTINCTION: f32 = 0.35;

/// The `density_factor` that swallows `extinction` of the background over
/// [`HALF_EDGE_M`] — Beer's law, `exp(-d·σ·L) = 1 − extinction`, solved for `d`.
///
/// Not a constant because `ln` is not a `const fn`. That is the whole reason this
/// is a function rather than two numbers in the file, and it is worth the
/// indirection: the numbers bevy wants are ~3 × 10⁻⁴, where nothing about the
/// value tells you whether it is right and a factor of forty looks the same as a
/// factor of one.
fn density_for(extinction: f32) -> f32 {
    -(1.0 - extinction).ln() / (HALF_EDGE_M * (SCATTERING + ABSORPTION))
}

/// The density to give the volume for a given weather.
///
/// Pure, and separated from [`follow_camera`] so the relation it encodes can be
/// asserted without a `World`: both inputs are 0..1 (see the module note — reading
/// `fog_density` on any other scale is the bug this module shipped with).
fn density_factor(fog_density: f32, precipitation: f32) -> f32 {
    let haze = fog_density.clamp(0.0, 1.0);
    let wet = precipitation.clamp(0.0, 1.0);
    let extinction = CLEAR_EXTINCTION + (RAIN_EXTINCTION - CLEAR_EXTINCTION) * wet;
    // Weather scales the air between a third of the stated ceiling and the whole
    // of it. Not down to zero: the shafts are the reason the volume exists, and
    // clear air at noon is exactly when a beam through a colonnade reads best.
    density_for(extinction) * (0.35 + 0.65 * haze)
}

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
                // The first frame's worth. `follow_camera` overwrites it from the
                // weather on the next one, and spawning at the clear-air value
                // rather than bevy's default is what keeps that first frame from
                // being a dark one.
                density_factor: density_factor(0.0, 0.0),
                // Warm and very slightly desaturated, so a shaft reads as sunlit
                // air rather than as a white beam. The sun's own colour is
                // already in the light, and multiplying two saturated warms
                // makes a stage effect.
                fog_color: Color::srgb(0.94, 0.93, 0.90),
                // Scattering above absorption: this is air, which redirects far
                // more light than it swallows. The reverse reads as smoke.
                scattering: SCATTERING,
                absorption: ABSORPTION,
                ..default()
            },
            Transform::from_scale(Vec3::splat(VOLUME_EDGE_M)),
            // Overwritten from the camera every frame by `follow_camera`, but it
            // has to exist from the first frame: without the component the entity
            // is not in the grid at all, and its cell-local translation would be
            // read as a render-space one.
            GridCell::default(),
            ShaftVolume,
        ));
    });
}

/// Keep the volume over the camera and its density in step with the weather.
///
/// Cheap enough to run unconditionally — two component writes on one entity — and
/// it returns immediately when the volume does not exist, which is every run that
/// has the setting off or cannot afford it.
///
/// # The cell and the translation are one answer and must be taken together
///
/// This entity is a **child of the big_space root and carries a [`GridCell`]**, so
/// its `Transform` is cell-local: where it actually renders is
/// `(its cell − the origin cell) × cell edge + its translation`. Copy the
/// camera's *`GlobalTransform`* into that translation and leave the cell at
/// `default()`, and the volume lands on the camera only while the camera happens
/// to sit in cell (0, 0, 0) — anywhere else it is a whole number of cells away,
/// contributes no fragments at all, and light shafts silently do nothing.
///
/// That is exactly what this system did until 2026-09-10, and it is why the
/// module note used to claim `sky::update_star_sky` as the precedent for a bare
/// `tf.translation = cam.translation()`. The star shell is **not** the same case:
/// it is spawned as a plain root entity with no `GridCell` at all, so its
/// `Transform` *is* its `GlobalTransform` and copying a render-space translation
/// into it is right. Here the two frames differ, so both halves have to be taken
/// from the camera — the cell and the cell-local translation — which is the shape
/// `sky::update_globe_clouds` already reads the camera in.
///
/// The **rotation is still deliberately not copied**: a box of air does not turn
/// when you turn your head.
pub fn follow_camera(
    camera: Query<(&GridCell, &Transform), (With<FreeCamera>, Without<ShaftVolume>)>,
    dev: Res<DevPanelState>,
    mut volume: Query<(&mut GridCell, &mut Transform, &mut FogVolume), With<ShaftVolume>>,
) {
    let Ok((mut cell, mut tf, mut fog)) = volume.single_mut() else {
        return;
    };
    let Ok((cam_cell, cam_tf)) = camera.single() else {
        return;
    };
    // Written through the change-detection guard rather than unconditionally: a
    // standing camera would otherwise mark the volume's transform changed every
    // frame, and `big_space`'s propagation is what pays for that.
    if *cell != *cam_cell {
        *cell = *cam_cell;
    }
    if tf.translation != cam_tf.translation {
        tf.translation = cam_tf.translation;
    }

    let density = density_factor(dev.fog_density, dev.precipitation);
    if fog.density_factor != density {
        fog.density_factor = density;
    }
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

    /// Retina and 4K lose the shafts on Automatic; 1080p and 1440p keep them.
    #[test]
    fn automatic_drops_the_shafts_only_on_a_large_main_pass() {
        assert!(affordable_at(0));
        assert!(affordable_at(1920 * 1080));
        assert!(affordable_at(2560 * 1440));
        assert!(!affordable_at(3134 * 1952));
        assert!(!affordable_at(3840 * 2160));
    }

    /// Rain must not make the air *clearer*, which is the sign error this
    /// interpolation would fail silently on — the picture would simply look
    /// slightly wrong in the weather nobody screenshots.
    #[test]
    fn rain_is_thicker_than_clear_air() {
        assert!(density_factor(0.5, 1.0) > density_factor(0.5, 0.0));
    }

    /// What fraction of the background survives `metres` of this air. The
    /// background half of the march (`background_alpha` in bevy's shader), which
    /// is the half that is not the feature and is the half that broke it.
    fn transmittance(density: f32, metres: f32) -> f32 {
        (-density * (SCATTERING + ABSORPTION) * metres).exp()
    }

    /// **The bug this module shipped with.** Switching light shafts on must not
    /// turn the world off: across the volume's own half-edge, in the *worst*
    /// weather the sliders can ask for, most of the background has to survive.
    ///
    /// The value that failed here was 0.7% at 300 m — so this is not a tight
    /// tolerance guarding a subtle regression, it is a floor under a mistake that
    /// is two orders of magnitude wide and reads as "the renderer is broken".
    #[test]
    fn the_volume_does_not_swallow_the_scene() {
        for (fog, rain) in [(0.0, 0.0), (0.15, 0.0), (1.0, 0.0), (1.0, 1.0)] {
            let survives = transmittance(density_factor(fog, rain), HALF_EDGE_M);
            assert!(
                survives >= 0.6,
                "fog {fog}, rain {rain}: only {survives} of the background survives \
                 {HALF_EDGE_M} m of air"
            );
        }
    }

    /// The stated extinction is what the volume actually does, so the constants
    /// can be read as the fractions they claim to be. This is what makes
    /// [`CLEAR_EXTINCTION`] reviewable — the `density_factor` it becomes is not.
    #[test]
    fn the_extinction_constants_mean_what_they_say() {
        for wanted in [CLEAR_EXTINCTION, RAIN_EXTINCTION] {
            let got = 1.0 - transmittance(density_for(wanted), HALF_EDGE_M);
            assert!(
                (got - wanted).abs() < 1e-4,
                "asked for {wanted} extinction and got {got}"
            );
        }
    }

    /// Both inputs are 0..1 controls. Reading `fog_density` on the 0..0.02 scale
    /// that `FogFalloff`'s coefficient uses is exactly how this went wrong, and
    /// the symptom was silence: the multiplier simply sat at its ceiling for every
    /// weather, so no slider position ever looked different from any other.
    #[test]
    fn the_weather_sliders_are_read_on_their_own_scale() {
        let clear = density_factor(0.0, 0.0);
        let default_weather = density_factor(0.15, 0.0);
        let thick = density_factor(1.0, 0.0);
        assert!(
            clear < default_weather && default_weather < thick,
            "fog 0.0/0.15/1.0 gave {clear}/{default_weather}/{thick}"
        );
        // …and the top of the slider is not reached a fifteenth of the way up it.
        assert!(
            density_factor(0.07, 0.0) < thick * 0.9,
            "the haze knob saturates within the first tenth of its travel"
        );
    }
}
