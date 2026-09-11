//! The two render features a person can switch on and off, and the one place
//! that puts them — and the contact shadows that ride the first of them — on the
//! scene.
//!
//! Ambient occlusion and volumetric light shafts are **not tier settings**. That
//! was the first shape of them and it was the wrong one: a tier is a guess about
//! the class of machine, resolved before a renderer exists, and both of these
//! have a real capability behind them that the device can simply be asked about —
//! bevy's occlusion pass is a compute shader, and a march through a fog volume
//! needs an adapter that rasterises and a shadow map to cut the shafts out of.
//! `AppPrefs::ambient_occlusion_on` and `AppPrefs::light_shafts_on` hold that
//! resolution: the adapter says what is possible, the person says what they want
//! within it, and neither `Budget::desktop()` nor `Budget::phone()` has an
//! opinion.
//!
//! # Why one system and not three insertions at spawn
//!
//! Because the answer changes while the app runs. Settings ▸ Graphics writes the
//! preference and this reconciles the world to it on the next frame, which means
//! the camera spawn must not *also* insert these components — two places that can
//! disagree about the same component is how a setting comes to read "on" over a
//! scene that does not have it.
//!
//! Three things move together and that is the whole reason they are in one
//! function:
//!
//! * **The occlusion pass** on the camera. It carries `#[require(DepthPrepass,
//!   NormalPrepass)]`, and bevy does *not* remove a required component when the
//!   requiring one goes, so switching occlusion off has to take both prepasses
//!   with it by hand or the run keeps paying for two passes nothing reads.
//! * **The contact shadows**, which are the depth prepass's other consumer. They
//!   have no preference of their own on purpose — see the gate in the function —
//!   and they have to come off *before* the prepass they read does.
//! * **The sample count**, because `ScreenSpaceAmbientOcclusion` requires
//!   `Msaa::Off` and merely warns if it does not get it. A flip that changed one
//!   without the other would silently produce multisampling and no occlusion.
//! * **The edge filter**, because the samples it replaces are exactly the ones
//!   the occlusion pass took away (`Budget::wants_edge_filter`).
//!
//! and for the shafts, the camera's `VolumetricFog`, the sun's `VolumetricLight`
//! and the `FogVolume` itself — none of which draws anything without the other
//! two (`systems::light_shafts`).
//!
//! # Removing these components is safe, and that is not true of the sky
//!
//! `AppPrefs::sky` is startup-only because removing an `Atmosphere` frees GPU
//! resources that are still in flight and hangs the device (see
//! `globe::update_globe_visibility`). Nothing here owns a resource of that kind:
//! an occlusion pass and a fog volume are pipeline state, and bevy respecialises
//! on the next frame. So these two settings apply immediately, and the Graphics
//! tab says so.

use bevy::anti_alias::fxaa::Fxaa;
use bevy::core_pipeline::prepass::{DepthPrepass, NormalPrepass};
use bevy::light::{VolumetricFog, VolumetricLight};
use bevy::pbr::{ContactShadows, ScreenSpaceAmbientOcclusion};
use bevy::prelude::*;

use crate::components::FreeCamera;
use crate::resources::AppPrefs;
use crate::systems::light_shafts::{self, ShaftVolume};
use crate::systems::sky::SunLight;

/// Steps the volumetric march takes between the near plane and what it hits.
///
/// Bevy's default is 64. The volume here is hundreds of metres across rather than
/// a room, so the samples are spread thin and banding shows; 48 with the jitter
/// below reads cleaner than 64 without it, and costs less. `light_shafts` has a
/// test tying this to the volume's size — grow one and the other has to grow.
pub const MARCH_STEPS: u32 = 48;

/// Reconcile the camera, the sun and the fog volume with the current preference.
///
/// Runs every frame. It costs two boolean resolutions and a pair of `contains`
/// checks in the steady state, and it must run unconditionally rather than on
/// `AppPrefs` change detection: the *capability* half of the answer is only known
/// once `setup_camera` has measured it, so the first correct answer arrives a
/// frame or two after startup with nothing having changed the preference.
pub fn apply_render_features(
    mut commands: Commands,
    prefs: Res<AppPrefs>,
    restricted: Res<RestrictedSampleCounts>,
    camera: Query<
        (
            Entity,
            Has<ScreenSpaceAmbientOcclusion>,
            Has<ContactShadows>,
            Has<VolumetricFog>,
            Has<Fxaa>,
        ),
        With<FreeCamera>,
    >,
    sun: Query<(Entity, Has<VolumetricLight>), With<SunLight>>,
    volume: Query<Entity, With<ShaftVolume>>,
    root: Query<Entity, With<big_space::prelude::BigSpace>>,
) {
    let Ok((cam, has_ao, has_contact, has_fog, has_fxaa)) = camera.single() else {
        return;
    };
    let want_ao = prefs.ambient_occlusion_on();
    let want_shafts = prefs.light_shafts_on();
    let budget = crate::quality::budget();

    // Contact shadows ride the occlusion setting, and are gated on shadows as
    // well. Two gates, each of which is the honest one for its half.
    //
    // The **prepass** is why they cannot have a switch of their own: bevy's
    // implementation is `#[require(DepthPrepass)]`, and the depth prepass exists
    // in this app only because the occlusion pass asked for it. A third
    // preference would therefore be a preference that silently turns a second
    // full-screen pass on, which is not a thing a Graphics tab should be able to
    // hide. They also *are* the same cue — the note that asked for occlusion here
    // put it as "nothing is darker where it meets anything else" — and this is the
    // half of that cue a screen-space occlusion pass is worst at, because the
    // contact is where its own radius has the least to work with.
    //
    // The **shadow** gate is the plainer one: these are shadows. Somebody who has
    // turned shadows off to get their frames back has not asked for a
    // per-fragment ray march instead.
    let want_contact = want_ao && prefs.shadows_on();

    if want_ao != has_ao {
        let mut cmd = commands.entity(cam);
        if want_ao {
            cmd.insert(ScreenSpaceAmbientOcclusion::default());
        } else {
            // Both prepasses by hand: bevy adds a `#[require]`d component on
            // insert and leaves it behind on removal, so dropping only the
            // occlusion component would keep a depth and a normal pass running
            // for the rest of the session with nothing reading either.
            cmd.remove::<ScreenSpaceAmbientOcclusion>();
            // Before the prepasses go, and in the same flush: this is the other
            // consumer of the depth one, and leaving it behind would be a march
            // reading a buffer nothing writes.
            cmd.remove::<ContactShadows>();
            cmd.remove::<DepthPrepass>();
            cmd.remove::<NormalPrepass>();
        }
        // …and the sample count with it, in the same flush. `Msaa` is a plain
        // component, so this is a write rather than a pipeline rebuild request;
        // bevy notices and re-creates the main texture.
        cmd.insert(budget.main_pass_msaa(restricted.0, want_ao));
    }

    // Separate from the block above because the two answers can diverge: the
    // shadow preference moves without the occlusion one.
    if want_contact != has_contact {
        let mut cmd = commands.entity(cam);
        if want_contact {
            cmd.insert(ContactShadows {
                // Bevy's default is 16. Halved, because the ray is 30 cm long:
                // eight steps put a sample every ~4 cm, which is finer than the
                // depth buffer's own answer about where a surface is at any range
                // where a 30 cm shadow is visible at all. Steps are the whole cost
                // of this feature.
                linear_steps: 8,
                // Bevy's defaults for the other two. The length is the point — it
                // is short deliberately: this darkens the join where a lamppost,
                // a bench or a wall meets the ground, which is exactly the range
                // the cascade maps cannot resolve and the occlusion pass reads as
                // one surface.
                ..default()
            });
        } else {
            cmd.remove::<ContactShadows>();
        }
    }

    let want_fxaa = budget.wants_edge_filter(restricted.0, want_ao);
    if want_fxaa != has_fxaa {
        let mut cmd = commands.entity(cam);
        if want_fxaa {
            cmd.insert(Fxaa::default());
        } else {
            cmd.remove::<Fxaa>();
        }
    }

    if want_shafts != has_fog {
        let mut cmd = commands.entity(cam);
        if want_shafts {
            cmd.insert(VolumetricFog {
                step_count: MARCH_STEPS,
                // Dithers the ray start so banding becomes noise. Bevy's own note
                // says this is meant to pair with temporal anti-aliasing, which
                // this client deliberately does not have — but the fog is
                // low-frequency and slow, so it reads as grain rather than crawl.
                jitter: 0.5,
                ..default()
            });
        } else {
            cmd.remove::<VolumetricFog>();
        }
    }

    // The sun is what casts them. `try_insert`/`try_remove` because the light is
    // respawned whenever shadows are toggled (`sky::spawn_sun_light` — the
    // component cannot be mutated in place without panicking bevy_light), so the
    // entity this frame named may be gone by the time the command applies.
    if let Ok((sun, has_marker)) = sun.single() {
        if want_shafts != has_marker {
            let mut cmd = commands.entity(sun);
            if want_shafts {
                cmd.try_insert(VolumetricLight);
            } else {
                cmd.try_remove::<VolumetricLight>();
            }
        }
    }

    // …and the volume they are marched through.
    match (want_shafts, volume.single()) {
        (true, Err(_)) => {
            if let Ok(root) = root.single() {
                light_shafts::spawn_volume(&mut commands, root);
            }
        }
        (false, Ok(volume)) => {
            commands.entity(volume).try_despawn();
        }
        _ => {}
    }
}

/// Whether this run's surface and adapter permit only 1 or 4 samples.
///
/// Measured once in `free_camera::setup_camera` and kept, because
/// [`apply_render_features`] has to answer the same sample-count question the
/// camera spawn did and cannot re-derive it — `RenderAdapterInfo` says what the
/// adapter is, but the restriction is a fact about this *surface* as well.
#[derive(Resource, Clone, Copy)]
pub struct RestrictedSampleCounts(pub bool);
