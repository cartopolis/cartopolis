//! The render feature a person can switch on and off, and the one place that puts
//! it on the scene.
//!
//! Volumetric light shafts are **not a tier setting**. That was the first shape of
//! it and it was the wrong one: a tier is a guess about the class of machine,
//! resolved before a renderer exists, and this has a real capability behind it
//! that the device can simply be asked about — a march through a fog volume needs
//! an adapter that rasterises and a shadow map to cut the shafts out of.
//! `AppPrefs::light_shafts_on` holds that resolution: the adapter says what is
//! possible, the person says what they want within it, and neither
//! `Budget::desktop()` nor `Budget::phone()` has an opinion.
//!
//! # Why one system and not an insertion at spawn
//!
//! Because the answer changes while the app runs. Settings ▸ Graphics writes the
//! preference and this reconciles the world to it on the next frame, which means
//! the camera spawn must not *also* insert these components — two places that can
//! disagree about the same component is how a setting comes to read "on" over a
//! scene that does not have it.
//!
//! Three things move together for the shafts and that is the whole reason they are
//! in one function: the camera's `VolumetricFog`, the sun's `VolumetricLight` and
//! the `FogVolume` itself — none of which draws anything without the other two
//! (`systems::light_shafts`).
//!
//! # What was removed, and what came back with it (2026-09-21)
//!
//! This file used to own **ambient occlusion** as well, and with it a chain of
//! things that had no switch of their own:
//!
//! * `ScreenSpaceAmbientOcclusion` carries `#[require(DepthPrepass, NormalPrepass)]`,
//!   and those two passes existed in this app for no other reason.
//! * **Contact shadows** were the depth prepass's only other consumer, and
//!   deliberately had no preference of their own — a third toggle would have been
//!   one that silently turned a second full-screen pass on.
//! * **Multisampling was forced off**, because bevy's occlusion node requires
//!   `Msaa::Off` and merely warns if it does not get it, and an **edge filter**
//!   stood in for the samples it took away.
//!
//! All four are gone. Multisampling is therefore available again wherever the
//! surface permits it (`Budget::main_pass_msaa`), and the edge filter now appears
//! only when *that* is what took the samples — a restricted surface.
//!
//! **The absence of the prepass is now load-bearing, not incidental.** With no
//! prepass there is exactly one pipeline drawing each mesh, which is what lets a
//! material move vertices in its own vertex stage without a matching
//! `prepass_vertex_shader` — see `docs/notes/the-city-stops-at-a-line.md`, which
//! is the change this unblocks. Anything that reintroduces a prepass owes that
//! note a read first. `road_depth::specialize` keeps its prepass guard for the
//! same reason: it is cheap, and it is correct either way.
//!
//! # Removing these components is safe, and that is not true of the sky
//!
//! `AppPrefs::sky` is startup-only because removing an `Atmosphere` frees GPU
//! resources that are still in flight and hangs the device (see
//! `globe::update_globe_visibility`). Nothing here owns a resource of that kind:
//! a fog volume is pipeline state, and bevy respecialises on the next frame. So
//! this setting applies immediately, and the Graphics tab says so.

use bevy::anti_alias::fxaa::Fxaa;
use bevy::light::{VolumetricFog, VolumetricLight};
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
    camera: Query<(Entity, Has<VolumetricFog>, Has<Fxaa>), With<FreeCamera>>,
    sun: Query<(Entity, Has<VolumetricLight>), With<SunLight>>,
    volume: Query<Entity, With<ShaftVolume>>,
    root: Query<Entity, With<big_space::prelude::BigSpace>>,
) {
    let Ok((cam, has_fog, has_fxaa)) = camera.single() else {
        return;
    };
    let want_shafts = prefs.light_shafts_on();
    let budget = crate::quality::budget();

    let want_fxaa = budget.wants_edge_filter(restricted.0);
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
