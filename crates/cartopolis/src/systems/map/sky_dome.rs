//! The sky for adapters that cannot run bevy's atmosphere.
//!
//! bevy_pbr's atmosphere is the sky everywhere it will run, and this module is
//! not an alternative to it — it is what draws when the device refuses it.
//!
//! **Why a device refuses it.** Bevy 0.19 moved the scattering coefficients into
//! a `ScatteringMedium` asset, baked into two lookup textures
//! (`bevy_pbr::medium`) that are `Rgba32Float` and are read through a
//! `FilterMode::Linear` sampler. wgpu allows filtering a 32-bit float texture
//! only on an adapter reporting `FLOAT32_FILTERABLE`, which desktop GPUs offer
//! and a great many mobile ones do not — the Adreno 619 in the Fairphone 4, the
//! first phone this client ever ran on, among them. There every atmosphere bind
//! group failed validation and Bevy's default answer to a validation error took
//! the whole app down with it, so `free_camera::setup_camera` gates the
//! atmosphere on the feature. That fixed the crash and left a black sky at noon,
//! which is what this module is for. Nothing here can be worked around in our
//! own code: the format, the sampler and the bind-group layout are all bevy's,
//! and the fix for the *atmosphere itself* is upstream (16-bit LUTs, or a
//! non-filtering sampler where the feature is missing).
//!
//! **What it is.** One shell around the camera and about twenty instructions of
//! arithmetic per pixel (`assets/shaders/sky_dome.wgsl`): single scattering
//! through a uniform slab, per channel, with the Rayleigh and aerosol optical
//! depths [`crate::utils::sun`] already uses for the sun beam. Sharing those is
//! the point — the sun's colour, the fog's colour, the clouds' lighting and now
//! the sky all come out of one model, so a low sun reddens all four together
//! instead of three of them agreeing and the sky doing something else. No
//! compute pass, no lookup table, no float32 texture, no storage buffer: there
//! is nothing in it for an adapter to refuse.
//!
//! **What it is not.** It has no multiple scattering (hence the explicit
//! twilight and night floor below — a single-scattering sky goes black the
//! moment the sun sets, which is the same artefact the horizon-haze shell exists
//! to cover for the real atmosphere), no aerial perspective on geometry (the
//! camera's `DistanceFog` is still what hazes distance), and no planet: it is a
//! flat-earth sky over a flat local tier, which is why it is hidden below the
//! altitude the globe appears at rather than being carried into space.
//!
//! `CARTO_SKY_DOME=1` forces it on an adapter that could run the real thing,
//! which is the only way to look at it without the phone. `CARTO_NO_ATMOSPHERE`
//! keeps its meaning of *no sky at all* — it is a bisecting switch, and a switch
//! that silently swapped one sky for another would not bisect anything.
//!
//! **The refusal is now the *only* way to get here**, and it took two attempts to
//! arrive at that. A `quality::Budget::sky_dome` used to choose this shell on a
//! machine that could run the atmosphere and should not spend the frame on it —
//! which is a fair distinction and an unanswerable question, because the budget is
//! resolved before a renderer exists and so on Android it meant "this is a
//! Fairphone 4" for every handset. A flagship ended up here on that technicality
//! (an Adreno 840, 2026-08-17, whose only fault was running Android). The patch
//! was a table of GPU families; the fix, on 2026-08-18, was deleting both and
//! asking the adapter (`quality::Capabilities::float32_filterable`). A device that
//! should not spend the frame is now put on a lower rung by a person, which is a
//! decision somebody can be wrong about out loud.
//!
//! When reading a report, take the reason from `setup_camera`'s `sky chosen` log
//! line and not from the presence of this module.

use bevy::light::light_consts::lux::RAW_SUNLIGHT;
use bevy::pbr::{MaterialPipeline, MaterialPipelineKey};
use bevy::prelude::*;
use bevy::render::mesh::MeshVertexBufferLayoutRef;
use bevy::render::render_resource::{
    AsBindGroup, RenderPipelineDescriptor, SpecializedMeshPipelineError,
};
use bevy::shader::ShaderRef;
use big_space::prelude::GridCell;

use crate::resources::DevPanelState;
use crate::utils::sun;

/// Which sky this run got, decided once by [`crate::systems::free_camera::setup_camera`]
/// from the adapter's features and the startup switches.
///
/// A resource rather than a re-test at each use, because the decision has to be
/// the *same* one in three places — whether the camera carries
/// `AtmosphereSettings`, whether the planet entity exists, and whether the dome
/// below is spawned — and two of those are already load-bearing enough that
/// discovering the answer twice is how they come apart.
#[derive(Resource, Clone, Copy, Debug, PartialEq, Eq)]
pub enum SkyMode {
    /// bevy_pbr's atmosphere: the real thing, and what nearly every machine gets.
    Atmosphere,
    /// This module: the sky was wanted, and the adapter could not draw it.
    Dome,
    /// No sky at all — `CARTO_NO_ATMOSPHERE`, or the dev panel's toggle off.
    None,
}

/// The dome shell.
#[derive(Component)]
pub struct SkyDome;

/// Radius of the shell, in metres.
///
/// It has to sit **outside** [`crate::systems::free_camera::GROUND_DISC_RADIUS`]:
/// the shell is opaque and the ground disc is the farthest thing in the flat
/// tier, so a smaller radius would put sky in front of the last 10 km of ground
/// at every horizon-grazing angle. It also has to stay inside the flat tier's
/// 200 km far plane, or the whole dome is clipped away and the fix reads as
/// having done nothing at all.
pub const SKY_DOME_RADIUS: f32 = 160_000.0;

/// Above this altitude (metres) the shell is hidden outright, and over the run up
/// to it from [`SKY_DOME_FADE_ALT`] the sky is faded to nothing.
///
/// The barometric column has already taken all but a fraction of a percent of the
/// sky by 30 km, but "almost nothing" is not nothing: switched off at that point
/// the sky would still step, and a step in the middle of a climb is the one thing
/// every ramp in this renderer is written to avoid. The fade takes it to exactly
/// zero at the cut, so hiding the shell there is provably invisible (there is a
/// test), and the hide is then just the frames after it not being drawn.
///
/// The bound that decides the number is the other one: it must stay below
/// [`crate::systems::globe::GLOBE_SHOW_ALT`], because the dome is a flat-earth
/// sky and the globe is the moment the world stops being flat.
pub const SKY_DOME_MAX_ALT: f32 = 45_000.0;

/// Where that fade begins (metres) — above the highest air worth scattering in,
/// and well below the cut.
pub const SKY_DOME_FADE_ALT: f32 = 30_000.0;

/// Mie asymmetry factor: aerosols scatter strongly forward, which is what draws
/// the bright halo around the sun and the white band along the horizon.
const MIE_ASYMMETRY: f32 = 0.76;

/// Altitude (metres) whose sunlight stands in for "the light that lit this sky".
///
/// Not the observer's own: at dusk the ground is in shadow while the air above it
/// is still in full sun, which is precisely why the sky stays bright for half an
/// hour after sunset and why [`sun::sun_transmittance_at`] exists in this tree
/// (`docs/notes/cloud-lighting.md` makes the same argument for a cloud deck).
/// Taking the ground's own beam instead makes dusk a black sky over a lit city.
const SKY_SCATTER_ALT_M: f32 = 10_000.0;

/// Deep-night sky colour, and the luminance (cd/m²) it is worth.
///
/// Far above the real night sky, and deliberately: the value is chosen so that
/// under the night exposure lift the dome reads as a *very* dark blue rather than
/// as the black clear colour, which is the difference between a night sky and a
/// hole. It has to stay well under the star field's brightness or it washes the
/// stars out — they are the thing worth looking at up there.
const NIGHT_SKY_COLOR: Vec3 = Vec3::new(0.22, 0.32, 0.62);
const NIGHT_SKY_LUMINANCE: f32 = 45.0;

/// How much brighter than the sky the sun's own disc is drawn.
///
/// The physical ratio is ~10⁵ and would be pointless here: the source radiance is
/// already several times the tonemapper's white point, so anything past "clearly
/// blown out" buys nothing without bloom — and the tier this draws on is the one
/// that has no bloom (`quality::Budget`).
const SUN_DISC_GAIN: f32 = 2.0;

/// Everything the shader needs, as one struct so it can be computed — and
/// tested — without a GPU. Radiances are photometric and pre-exposure; the
/// shader multiplies by `view.exposure`, the same convention `horizonhaze.wgsl`
/// follows for its light term.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct SkyDomeParams {
    /// Rayleigh scattering optical depth at the zenith, per channel, already
    /// scaled by the air remaining above the camera.
    pub rayleigh: Vec3,
    /// The same for the aerosol term.
    pub aerosol: Vec3,
    /// Radiance of the light being scattered (lux/π at the scattering altitude).
    pub source: Vec3,
    /// What the sky is worth once the sun can no longer account for it.
    pub night: Vec3,
    /// Sun-disc brightness, as a multiplier on `source`; 0 once it has set.
    pub disc: f32,
}

/// How much daylight the *sky* still has, as against [`sun::daylight`], which is
/// what the ground gets.
///
/// The sky is lit from above and is the last thing to go dark: the ground has
/// been in shadow for half an hour while the air ten kilometres up is still in
/// sunlight. So this runs from the sun 12° below the horizon (the end of
/// nautical twilight) rather than `daylight`'s 8.6°, and reaches full daylight
/// later, keeping some colour in the sky through the whole of dusk. Using
/// `daylight` here instead snapped the sky to night about twenty minutes early,
/// with the city below it still lit.
fn sky_daylight(hours: f32, decl_rad: f32) -> f32 {
    sun::smoothstep(-0.21, 0.25, sun::sun_direction(hours, decl_rad).y)
}

/// Everything the dome's shader is driven by, for a time of day and a camera
/// altitude.
pub fn sky_dome_params(hours: f32, decl_rad: f32, altitude_m: f32) -> SkyDomeParams {
    let (rayleigh, aerosol) = sun::zenith_scattering(hours);

    // Only the air *above* the camera scatters into the view ray, and the sky
    // darkening as you climb out of it is the whole altitude behaviour — no
    // separate fade constant, and none of the "why did it fade there?" that a
    // hand-placed one invites. Same barometric factor `sun_transmittance_at`
    // applies to the beam.
    let column = sun::barometric_column(altitude_m);

    // The light being scattered: unattenuated sunlight, filtered by the air above
    // the scattering altitude rather than above the ground (see
    // SKY_SCATTER_ALT_M), and faded out through twilight. `/ π` converts the
    // illuminance the sun light is specified in to the radiance a surface — here,
    // a patch of sky — emits under it, which is what puts the dome on the same
    // scale as everything else the camera's exposure was tuned against.
    let day = sky_daylight(hours, decl_rad);
    let tint = sun::sun_transmittance_at(hours, decl_rad, SKY_SCATTER_ALT_M.max(altitude_m));
    // …taken the rest of the way to zero before the shell is hidden, so that the
    // hide is not a cut (see SKY_DOME_MAX_ALT).
    let fade = 1.0 - sun::smoothstep(SKY_DOME_FADE_ALT, SKY_DOME_MAX_ALT, altitude_m);
    let source =
        tint * (RAW_SUNLIGHT * crate::systems::sky::SUN_SCALE * day * fade / std::f32::consts::PI);

    SkyDomeParams {
        rayleigh: rayleigh * column,
        aerosol: aerosol * column,
        source,
        night: NIGHT_SKY_COLOR * (NIGHT_SKY_LUMINANCE * (1.0 - day) * column * fade),
        // The same disc `sun_light` fades the beam out with, so the sun does not
        // linger in the sky after its light has gone.
        disc: sun::sun_disc(hours, decl_rad) * SUN_DISC_GAIN,
    }
}

/// The dome's material. One shell, five uniforms, no textures — see the module
/// header for why that list matters.
#[derive(Asset, TypePath, AsBindGroup, Clone)]
pub struct SkyDomeMaterial {
    /// xyz: world direction to the sun. w: sun-disc brightness.
    #[uniform(0)]
    pub sun: Vec4,
    /// xyz: Rayleigh optical depth at the zenith. w: Mie asymmetry.
    #[uniform(1)]
    pub rayleigh: Vec4,
    /// xyz: aerosol optical depth at the zenith. w: cosine of the sun disc's
    /// angular radius.
    #[uniform(2)]
    pub mie: Vec4,
    /// xyz: the radiance being scattered. w unused.
    #[uniform(3)]
    pub source: Vec4,
    /// xyz: the twilight/night floor. w unused.
    #[uniform(4)]
    pub night: Vec4,
}

impl Material for SkyDomeMaterial {
    fn fragment_shader() -> ShaderRef {
        "shaders/sky_dome.wgsl".into()
    }

    fn specialize(
        _pipeline: &MaterialPipeline,
        descriptor: &mut RenderPipelineDescriptor,
        _layout: &MeshVertexBufferLayoutRef,
        _key: MaterialPipelineKey<Self>,
    ) -> Result<(), SpecializedMeshPipelineError> {
        // The camera is inside the shell, so it is the interior that is wanted.
        descriptor.primitive.cull_mode = None;
        // Opaque (so it composites before every transparent shell — stars, moon,
        // haze and cloud all have to land on top of it, and they are all
        // camera-centred, which makes their distance sort a tie the dome must not
        // be entered into), but writing no depth: the shell is farther than
        // anything in the flat tier, so the depth *test* is what keeps it behind
        // the world, and writing depth would only give it the chance to reject
        // something drawn later.
        if let Some(depth) = descriptor.depth_stencil.as_mut() {
            depth.depth_write_enabled = Some(false);
        }
        Ok(())
    }
}

/// Spawns the shell, if this run's sky is the dome.
///
/// Startup, and ordered after `setup_camera` because that is where [`SkyMode`] is
/// decided — the adapter's features are not known before the render device
/// exists.
pub fn setup_sky_dome(
    mut commands: Commands,
    mut meshes: ResMut<Assets<Mesh>>,
    mut materials: ResMut<Assets<SkyDomeMaterial>>,
    sky: Res<SkyMode>,
) {
    if *sky != SkyMode::Dome {
        return;
    }
    // Deliberately says *that* the dome was chosen, not why: there are three
    // reasons (no `FLOAT32_FILTERABLE`, the render budget, `CARTO_SKY_DOME`) and
    // `setup_camera` logs the one that applied a few lines earlier. This line used
    // to assert the first of them unconditionally, which sent one report chasing a
    // missing adapter feature on a phone that had it and had merely been tiered
    // down (Adreno 840, 2026-08-17).
    info!("drawing the analytic sky dome (systems::sky_dome); setup_camera logged which reason");
    // Shading is per-pixel from the view direction, so the subdivision only
    // decides how round the shell is; `ico(3)` is 1,280 triangles and dips 0.15 %
    // inside its own radius, which is the number that has to stay clear of the
    // ground disc (see SKY_DOME_RADIUS).
    let mesh = meshes.add(Sphere::new(1.0).mesh().ico(3).unwrap());
    let material = materials.add(SkyDomeMaterial {
        sun: Vec4::Y,
        rayleigh: Vec4::ZERO, // written each frame by update_sky_dome
        mie: Vec4::ZERO,
        source: Vec4::ZERO,
        night: Vec4::ZERO,
    });
    commands.spawn((
        Mesh3d(mesh),
        MeshMaterial3d(material),
        Transform::from_scale(Vec3::splat(SKY_DOME_RADIUS)),
        bevy::light::NotShadowCaster,
        bevy::light::NotShadowReceiver,
        SkyDome,
    ));
}

/// Keeps the dome on the camera and its uniforms on the clock.
#[allow(clippy::type_complexity)]
pub fn update_sky_dome(
    dev: Res<DevPanelState>,
    globe: Res<crate::systems::globe::GlobeState>,
    camera: Query<
        (&GridCell, &GlobalTransform, &Transform),
        (With<crate::components::FreeCamera>, Without<SkyDome>),
    >,
    mut dome: Query<
        (
            &mut Transform,
            &mut Visibility,
            &MeshMaterial3d<SkyDomeMaterial>,
        ),
        With<SkyDome>,
    >,
    mut materials: ResMut<Assets<SkyDomeMaterial>>,
    // The (time, declination, altitude) the uniforms were last written for.
    // Writing through the `Mut` re-uploads all five every frame otherwise, for
    // three inputs that move slowly at most — and this is the tier where that is
    // worth caring about, since it is by definition the weakest GPU in the room.
    mut applied: Local<Option<(f32, f32, f32)>>,
) {
    let Ok((cell, cam, cam_local)) = camera.single() else {
        return;
    };
    let Ok((mut tf, mut vis, mat_handle)) = dome.single_mut() else {
        return;
    };
    // Camera-centred, so the sky reads as infinitely far however far the avatar
    // has walked from the anchor.
    tf.translation = cam.translation();

    let altitude = globe.altitude_m(cell, cam_local) as f32;
    let want = if altitude > SKY_DOME_MAX_ALT {
        Visibility::Hidden
    } else {
        Visibility::Inherited
    };
    if *vis != want {
        *vis = want;
    }
    if want == Visibility::Hidden {
        return;
    }

    // Altitude dithers in its last bits while hovering; an exact test would call
    // that a change every frame (the same epsilon `apply_distance_fog` uses, and
    // for the same reason).
    const EPS: f32 = 1e-4;
    let next = (dev.time_of_day, dev.sun_declination_rad, altitude);
    if applied.is_some_and(|(t, d, a)| {
        (t - next.0).abs() < EPS && (d - next.1).abs() < EPS && (a - next.2).abs() < 1.0
    }) {
        return;
    }
    *applied = Some(next);

    let p = sky_dome_params(dev.time_of_day, dev.sun_declination_rad, altitude);
    let sun_dir = sun::sun_direction(dev.time_of_day, dev.sun_declination_rad);
    if let Some(mut mat) = materials.get_mut(&mat_handle.0) {
        mat.sun = sun_dir.extend(p.disc);
        mat.rayleigh = p.rayleigh.extend(MIE_ASYMMETRY);
        // The disc is drawn at exactly the size bevy's `SunDisk` would have drawn
        // it, so a device that gains the real atmosphere does not gain a
        // different sun with it.
        mat.mie = p
            .aerosol
            .extend((crate::systems::sky::SUN_DISK_ANGULAR_SIZE * 0.5).cos());
        mat.source = p.source.extend(0.0);
        mat.night = p.night.extend(0.0);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Noon, at sea level. The sky has to be blue — which in these terms means
    /// the blue channel scatters hardest — and bright enough to matter against
    /// the exposure the scene is tuned for.
    #[test]
    fn a_clear_noon_sky_is_blue_and_bright() {
        let p = sky_dome_params(12.0, 0.0, 0.0);
        assert!(p.rayleigh.z > p.rayleigh.y && p.rayleigh.y > p.rayleigh.x);
        let zenith = zenith_radiance(p);
        assert!(zenith.z > zenith.x, "the zenith is blue: {zenith:?}");
        // A clear day sky is a few thousand cd/m²; anything near zero here is the
        // black sky this module exists to remove.
        assert!(zenith.z > 1_000.0, "too dark for noon: {zenith:?}");
    }

    /// A low sun reddens the light the sky is scattering, exactly as it reddens
    /// the beam — the two come off one model, which is the point of taking the
    /// coefficients from `utils::sun`.
    #[test]
    fn dusk_reddens_the_source() {
        let noon = sky_dome_params(12.0, 0.0, 0.0);
        let dusk = sky_dome_params(18.0, 0.0, 0.0);
        assert!(dusk.source.x / dusk.source.z.max(1e-6) > noon.source.x / noon.source.z);
    }

    /// The sky is the last thing to go dark. Half an hour after the sun sets it
    /// still carries light; deep night hands over to the floor without ever
    /// passing through black.
    #[test]
    fn twilight_hands_over_to_the_night_floor() {
        let sunset = sky_dome_params(18.6, 0.0, 0.0);
        let dusk = sky_dome_params(19.3, 0.0, 0.0);
        let night = sky_dome_params(1.0, 0.0, 0.0);
        assert!(sunset.source.length() > dusk.source.length());
        assert!(dusk.source.length() > night.source.length());
        // Nothing along the way is unlit: the floor rises as the sun's share falls.
        for p in [sunset, dusk, night] {
            assert!(
                (p.source + p.night).length() > 1.0,
                "the sky went black: {p:?}"
            );
        }
        assert!(night.night.length() > dusk.night.length());
        // …and the night floor stays dark enough to leave the stars visible.
        assert!(night.source.length() < 1.0);
    }

    /// Climbing out of the air darkens the sky, and at the altitude the shell is
    /// hidden there is exactly nothing left in it to hide — which is what makes
    /// the hide invisible rather than a step (see [`SKY_DOME_MAX_ALT`]).
    #[test]
    fn the_sky_thins_to_nothing_before_the_shell_is_hidden() {
        let ground = sky_dome_params(12.0, 0.0, 0.0);
        let high = sky_dome_params(12.0, 0.0, 20_000.0);
        let top = sky_dome_params(12.0, 0.0, SKY_DOME_MAX_ALT);
        // The barometric column does most of the work on its own.
        assert!(high.rayleigh.z < 0.25 * ground.rayleigh.z);
        // ~11 % of the sea-level sky by 20 km, which is where a flight out stops
        // looking like a day and starts looking like the edge of the air.
        assert!(zenith_radiance(high).length() < 0.15 * zenith_radiance(ground).length());
        // …and the fade finishes it, at the cut and for the whole night side too.
        assert_eq!(zenith_radiance(top), Vec3::ZERO);
        assert_eq!(
            sky_dome_params(1.0, 0.0, SKY_DOME_MAX_ALT).night,
            Vec3::ZERO
        );
    }

    /// Zenith radiance, the way the shader computes it for an overhead ray
    /// (air mass 1), so a test can talk about what the sky is *worth*.
    fn zenith_radiance(p: SkyDomeParams) -> Vec3 {
        let ext = p.rayleigh + p.aerosol;
        (Vec3::ONE - (-ext).exp()) * p.source + p.night
    }

    /// The sun sets with the beam it lights the world with, rather than hanging
    /// in the sky after dark.
    #[test]
    fn the_disc_sets() {
        assert!(sky_dome_params(12.0, 0.0, 0.0).disc > 0.0);
        assert_eq!(sky_dome_params(23.0, 0.0, 0.0).disc, 0.0);
    }

    /// The two bounds the shell's geometry rests on, as a test rather than as a
    /// comment: it stands outside the ground disc it must not cover, and it is
    /// gone before the world stops being flat.
    #[test]
    fn the_shell_is_bounded_by_the_ground_disc_and_the_globe() {
        assert!(SKY_DOME_RADIUS > crate::systems::free_camera::GROUND_DISC_RADIUS);
        assert!(SKY_DOME_MAX_ALT < crate::systems::globe::GLOBE_SHOW_ALT);
    }
}
