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
//! # Every Graphics setting applies here, and at once (2026-09-29)
//!
//! Until this date the sky, its reflections and the whole preset were
//! startup-only, and bloom, tonemapping and the shadow filter were
//! whatever the camera was spawned with (the sample count still is — see
//! [`apply_render_features`]). They all live here now, reconciled every
//! frame against `AppPrefs` and against `quality::budget()` — which a preset
//! picked in Settings swaps while the app runs (`quality::select`). Most of them
//! are a component inserted, removed or rewritten, and bevy respecialises the
//! pipelines on the next frame.
//!
//! # The sky is the exception, and [`apply_sky`] is why it works anyway
//!
//! `AppPrefs::sky` used to be startup-only on the strength of "removing an
//! `Atmosphere` frees GPU resources still in flight and hangs the device". Read
//! against bevy 0.19's source, the danger is not freeing — wgpu defers the real
//! destruction of anything dropped until the GPU is done with it — but what bevy
//! **leaves behind** in the render world when the camera's atmosphere components
//! go:
//!
//! * `extract_atmosphere` only inserts `ExtractedAtmosphere` onto the camera's
//!   render entity; it removes it only when *no* `Atmosphere` exists anywhere. So
//!   a camera that loses `AtmosphereSettings` keeps every atmosphere pass gated on
//!   that component, now bound to uniforms nothing writes.
//! * `prepare_probe_textures` hands the sky-reflection probe the view's lookup
//!   tables **once** (`Without<AtmosphereProbeTextures>`) and never again. A probe
//!   brought back after a teardown bakes from tables the atmosphere no longer
//!   writes, into an image the lighting no longer reads — black reflections, and
//!   `prepare_atmosphere_probe_bind_groups` `unwrap`s the atmosphere uniforms, so
//!   a probe left over with no atmosphere is a panic.
//! * The component carrying the probe's image, `AtmosphereEnvironmentMap`, is
//!   private to bevy_pbr, so it cannot be removed by name ([`EnvMapComponent`]).
//!
//! The answer to the first two is to **re-sync the camera**: remove and re-add
//! `SyncToRenderWorld`, which despawns its render entity and gives it a fresh one
//! that extraction fills from what the main world says *now*. Nothing in this
//! client holds a camera's render entity, and everything bevy keeps per view is
//! rebuilt every frame by the prepare systems. The third is
//! [`find_env_map_component`].

use std::any::TypeId;

use bevy::anti_alias::fxaa::Fxaa;
use bevy::camera::Hdr;
use bevy::core_pipeline::tonemapping::Tonemapping;
use bevy::ecs::component::{ComponentId, Components};
use bevy::light::{
    AtmosphereEnvironmentMapLight, GeneratedEnvironmentMapLight, ShadowFilteringMethod,
    VolumetricFog, VolumetricLight,
};
use bevy::pbr::AtmosphereSettings;
use bevy::post_process::bloom::Bloom;
use bevy::prelude::*;
use bevy::render::sync_world::{RenderEntity, SyncToRenderWorld};
use bevy::render::view::Msaa;

use crate::components::FreeCamera;
use crate::resources::AppPrefs;
use crate::systems::light_shafts::{self, ShaftVolume};
use crate::systems::sky::SunLight;
use crate::systems::sky_dome::SkyMode;
use crate::systems::sun_rays::SunRays;

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
///
/// It also owns the camera's per-pixel settings — bloom, the edge filter,
/// tonemapping and the shadow filter — which the preset or a preference can move
/// at any time.
///
/// **The sample count is the one thing here that is not moved at runtime.**
/// Measured 2026-09-29 under a scripted toggle run: taking a live camera from 4×
/// to 1× (with a preset swap in the same frame) left a `pbr_opaque_mesh_pipeline`
/// specialised for 4 samples drawing into a 1-sample pass — a validation error,
/// which halts the renderer for the rest of the process. Bevy's own view-key
/// check looks right, so the fault was not pinned down; the switch is built so
/// it does not need to be. `setup_camera` picks the count from the preference at
/// launch, and the Anti-aliasing switch acts *now* through `Fxaa` — a
/// post-process whose insertion and removal are already proven safe — whenever
/// the main pass has one sample.
#[allow(clippy::type_complexity)]
pub fn apply_render_features(
    mut commands: Commands,
    prefs: Res<AppPrefs>,
    mut camera: Query<
        (
            Entity,
            Has<VolumetricFog>,
            Has<Fxaa>,
            Has<SunRays>,
            Has<Bloom>,
            Has<Hdr>,
            &Msaa,
            &mut Tonemapping,
            &mut ShadowFilteringMethod,
        ),
        With<FreeCamera>,
    >,
    sun: Query<(Entity, Has<VolumetricLight>), With<SunLight>>,
    volume: Query<Entity, With<ShaftVolume>>,
    root: Query<Entity, With<big_space::prelude::BigSpace>>,
) {
    let Ok((
        cam,
        has_fog,
        has_fxaa,
        has_rays,
        has_bloom,
        has_hdr,
        msaa,
        mut tonemapping,
        mut shadow_filter,
    )) = camera.single_mut()
    else {
        return;
    };
    let want_shafts = prefs.light_shafts_on();
    let budget = crate::quality::budget();

    // Written only on a difference, so the steady state marks nothing changed
    // and nothing respecialises.
    if *tonemapping != budget.tonemapping {
        *tonemapping = budget.tonemapping;
    }
    if *shadow_filter != budget.shadow_filter {
        *shadow_filter = budget.shadow_filter;
    }

    // Glare: the halo that makes the sun disc, the night windows and the limb from
    // orbit read as bright. Removed rather than zeroed when off, which skips its
    // thirteen passes; `apply_day_night_cycle` gives a new `Bloom` the strength for
    // the current altitude. It carries `#[require(Hdr)]`, and `Hdr` is fixed at
    // launch ([`hdr_at_launch`]), so a run launched without it waits for a restart.
    let want_bloom = prefs.bloom_on() && has_hdr;
    if want_bloom != has_bloom {
        let mut cmd = commands.entity(cam);
        if want_bloom {
            cmd.insert(Bloom {
                intensity: crate::systems::free_camera::bloom_intensity(0.0),
                ..Bloom::NATURAL
            });
        } else {
            cmd.remove::<Bloom>();
        }
    }

    // Edges were asked for and the main pass has no samples to give them: either
    // the surface took them at launch, or the switch was flipped since.
    let want_fxaa = prefs.anti_aliasing_on() && *msaa == Msaa::Off;
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

    // The sunbeams are a pass over the finished frame and need nothing else
    // (`systems::sun_rays`); `update_sun_rays` fills the component in.
    let want_rays = prefs.sun_rays_on();
    if want_rays != has_rays {
        let mut cmd = commands.entity(cam);
        if want_rays {
            cmd.insert(SunRays::default());
        } else {
            cmd.remove::<SunRays>();
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

/// What [`apply_sky`] and [`find_env_map_component`] know about the sky
/// reflections' private half.
///
/// `AtmosphereEnvironmentMap` is the component bevy_pbr puts on the camera to
/// hold the cubemap its atmosphere bakes into, beside the public
/// `GeneratedEnvironmentMapLight` that filters and lights with it. It is not
/// exported, so there is no type to name in a `remove::<…>()` — but it has a
/// `ComponentId` like any other, and [`find_env_map_component`] finds it.
///
/// Both halves matter when the reflections go off. Left on the camera, the map
/// keeps being baked every frame for nothing, and it stops bevy from ever making
/// a new one: `prepare_atmosphere_probe_components` only acts on a camera
/// *without* it, so switching reflections back on would never re-add the
/// `GeneratedEnvironmentMapLight`. If the id cannot be found, [`apply_sky`] falls
/// back to keeping the map and restoring the light it removed — `stashed` — which
/// is correct and only costs the bake.
#[derive(Resource, Default)]
pub struct EnvMapComponent {
    /// The camera's components on a frame with no reflections at all, and the
    /// archetype they were read from.
    baseline: Option<(bevy::ecs::archetype::ArchetypeId, Vec<ComponentId>)>,
    /// The private component, once found.
    id: Option<ComponentId>,
    /// Set once the search has failed, so it warns once and stops looking.
    gave_up: bool,
    /// The light removed while its map could not be — put back on re-enable.
    stashed: Option<GeneratedEnvironmentMapLight>,
}

/// The layout the private component has: a `Handle<Image>` and a `UVec2`.
///
/// Only a filter on top of the archetype difference, never the identification on
/// its own — two unrelated components can share a size.
#[allow(dead_code)]
struct EnvMapMirror {
    environment_map: Handle<Image>,
    size: UVec2,
}

/// Find `AtmosphereEnvironmentMap`'s `ComponentId` by what bevy adds alongside the
/// light it creates.
///
/// On a frame with no reflections the camera's component set is kept as a
/// baseline. `prepare_atmosphere_probe_components` then inserts the map and
/// `GeneratedEnvironmentMapLight` together, so on the first frame the light is
/// there, the map is among the components that were not in the baseline. The ones
/// this module inserts itself are known by type and set aside, and what remains is
/// filtered to the map's layout. **Exactly one** candidate is an answer; anything
/// else is logged once and the search stops, which leaves [`apply_sky`] on its
/// fallback rather than removing the wrong component from a camera.
pub fn find_env_map_component(
    components: &Components,
    // `Without<IsResource>`: in bevy 0.19 a resource is an entity, so an
    // `EntityRef` query reads every resource too and conflicts with the `ResMut`
    // below (B0002, a panic at startup) unless it is told it cannot match one.
    camera: Query<EntityRef, (With<FreeCamera>, Without<bevy::ecs::resource::IsResource>)>,
    mut env: ResMut<EnvMapComponent>,
) {
    if env.id.is_some() || env.gave_up {
        return;
    }
    let Ok(cam) = camera.single() else {
        return;
    };
    let has_light = cam.contains::<AtmosphereEnvironmentMapLight>();
    let has_generated = cam.contains::<GeneratedEnvironmentMapLight>();
    let archetype = cam.archetype();
    if !has_light && !has_generated {
        if env.baseline.as_ref().map(|(id, _)| *id) != Some(archetype.id()) {
            env.baseline = Some((archetype.id(), archetype.components().to_vec()));
        }
        return;
    }
    if !has_generated {
        // Asked for, not made yet: bevy's system has not run since the insert.
        return;
    }
    let Some((_, baseline)) = env.baseline.as_ref() else {
        // Reflections were on from the first frame this looked, so there is no
        // frame without them to difference against. Nothing is lost: the search
        // resumes the first time they are switched off.
        return;
    };
    let ours = [
        TypeId::of::<AtmosphereEnvironmentMapLight>(),
        TypeId::of::<GeneratedEnvironmentMapLight>(),
        TypeId::of::<AtmosphereSettings>(),
        TypeId::of::<Hdr>(),
        TypeId::of::<Bloom>(),
        TypeId::of::<Fxaa>(),
        TypeId::of::<VolumetricFog>(),
    ];
    let mirror = std::alloc::Layout::new::<EnvMapMirror>();
    let candidates: Vec<ComponentId> = archetype
        .components()
        .iter()
        .copied()
        .filter(|id| !baseline.contains(id))
        .filter(|id| {
            components.get_info(*id).is_some_and(|info| {
                info.type_id().is_some_and(|t| !ours.contains(&t))
                    && info.layout().size() == mirror.size()
                    && info.layout().align() == mirror.align()
            })
        })
        .collect();
    if let [only] = candidates[..] {
        env.id = Some(only);
        debug!("found the sky reflections' private map component");
    } else {
        env.gave_up = true;
        warn!(
            candidates = candidates.len(),
            "could not single out the sky reflections' map component; switching \
             reflections off will stop the lighting but not the bake"
        );
    }
}

/// Put the sky — atmosphere or dome — and its reflections on the camera as the
/// preference and the adapter say, and keep [`SkyMode`] current.
///
/// Runs every frame, like [`apply_render_features`], and does nothing in the
/// steady state beyond resolving two answers and comparing them with what the
/// camera carries. See the module note for why a teardown re-syncs the camera.
#[allow(clippy::type_complexity, clippy::too_many_arguments)]
pub fn apply_sky(
    mut commands: Commands,
    prefs: Res<AppPrefs>,
    mut sky: ResMut<SkyMode>,
    mut env: ResMut<EnvMapComponent>,
    mut camera: Query<
        (
            Entity,
            Option<&mut AtmosphereSettings>,
            Has<AtmosphereEnvironmentMapLight>,
            Option<&GeneratedEnvironmentMapLight>,
            Has<Hdr>,
        ),
        With<FreeCamera>,
    >,
) {
    let Ok((cam, atmosphere, has_reflections, generated, has_hdr)) = camera.single_mut() else {
        return;
    };
    // The atmosphere requires `Hdr`, and `Hdr` is never added or removed on a
    // live camera (see [`hdr_at_launch`]): a run launched without it keeps the dome
    // until the next start, and the Graphics tab says so.
    let mut want_sky = SkyMode::resolve(prefs.sky);
    if want_sky == SkyMode::Atmosphere && !has_hdr {
        want_sky = SkyMode::Dome;
    }
    if *sky != want_sky {
        info!(from = ?*sky, to = ?want_sky, preference = ?prefs.sky, "sky switched");
        *sky = want_sky;
    }
    let want_atmosphere = want_sky == SkyMode::Atmosphere;
    let want_reflections = want_sky.allows_reflections() && prefs.sky_lighting_on();
    let luts = crate::quality::budget().atmosphere_luts;

    // Anything that leaves bevy's per-view atmosphere state behind (module note)
    // ends in a re-sync.
    let mut resync = false;
    let mut cmd = commands.entity(cam);

    if has_reflections && !want_reflections {
        cmd.remove::<(AtmosphereEnvironmentMapLight, GeneratedEnvironmentMapLight)>();
        match env.id {
            Some(id) => {
                cmd.remove_by_id(id);
                env.stashed = None;
            }
            // The map stays; keep the light it fed so re-enabling can restore it,
            // since bevy will not make a second one for a camera that has a map.
            None => env.stashed = generated.cloned(),
        }
        resync = true;
    }

    match (atmosphere, want_atmosphere) {
        (Some(mut settings), true) => {
            // The preset moves the lookup-table sizes. The rendering method and
            // the sample count are `globe::update_globe_visibility`'s and are left
            // alone. The reflection probe borrowed the old tables once and would
            // keep baking from them, so a size change re-syncs as well.
            if settings.transmittance_lut_size != luts.transmittance
                || settings.multiscattering_lut_size != luts.multiscattering
                || settings.sky_view_lut_size != luts.sky_view
                || settings.aerial_view_lut_size != luts.aerial_view
            {
                settings.transmittance_lut_size = luts.transmittance;
                settings.multiscattering_lut_size = luts.multiscattering;
                settings.sky_view_lut_size = luts.sky_view;
                settings.aerial_view_lut_size = luts.aerial_view;
                resync = true;
            }
        }
        (None, true) => {
            // The LUT sizes are the tier's; everything else starts at bevy's
            // defaults (see `quality::AtmosphereLuts` for why only the sizes).
            cmd.insert(AtmosphereSettings {
                transmittance_lut_size: luts.transmittance,
                multiscattering_lut_size: luts.multiscattering,
                sky_view_lut_size: luts.sky_view,
                aerial_view_lut_size: luts.aerial_view,
                ..default()
            });
        }
        (Some(_), false) => {
            cmd.remove::<AtmosphereSettings>();
            resync = true;
        }
        (None, false) => {}
    }

    // Held back until `find_env_map_component` has seen the camera once without
    // them — at startup that is one frame — so there is a baseline to find the
    // private map against. Without it the first switch-off could only stop the
    // lighting and never the bake.
    let search_ready = env.id.is_some() || env.gave_up || env.baseline.is_some();
    if want_reflections && !has_reflections && search_ready {
        // Sky-driven image-based lighting: surfaces lit by, and water reflecting,
        // the atmosphere itself. A 256² cubemap rather than bevy's 512² quarters
        // the bake it re-runs every frame; the light it gives is low-frequency, so
        // the difference does not show.
        cmd.insert(AtmosphereEnvironmentMapLight {
            size: UVec2::splat(256),
            ..default()
        });
        if let Some(light) = env.stashed.take() {
            cmd.insert(light);
        }
    }

    if resync {
        // `On<Remove, SyncToRenderWorld>` reads `RenderEntity` to queue the old
        // render entity's despawn, and the re-insert queues a new one — which
        // `entity_sync_system` refuses (a panic) while `RenderEntity` is still
        // there. Both go in this order, in one command each.
        cmd.remove::<(SyncToRenderWorld, RenderEntity)>();
        cmd.insert(SyncToRenderWorld);
    }
}

/// Whether this run's camera was launched with `Hdr` — the 16-bit float main
/// texture both bloom and the atmosphere require.
///
/// **Fixed for the process, and that is measured, not cautious** (2026-09-29, a
/// scripted run flipping every Graphics setting): changing the shape of a live
/// camera's render target — its format when `Hdr` came or went, its sample count
/// when `Msaa` did — intermittently left a `pbr_opaque_mesh_pipeline` built for
/// the old shape drawing into the new one. That is a validation error, and a
/// validation error halts the renderer for the rest of the process. So
/// `setup_camera` decides it once, from what the launch wants: `Hdr` whenever the
/// atmosphere or bloom is on. A run launched with neither — the simple sky and no
/// glare — cannot switch either on until it restarts, and the Graphics tab says
/// so rather than pretending.
/// A static rather than a resource, like `quality::capabilities`: the Graphics
/// tab reads it and its system is already at bevy's parameter limit.
static HDR_AT_LAUNCH: std::sync::OnceLock<bool> = std::sync::OnceLock::new();

/// Record this run's answer. First call wins; `setup_camera` makes it.
pub fn set_hdr_at_launch(hdr: bool) {
    let _ = HDR_AT_LAUNCH.set(hdr);
}

/// This run's answer, or `None` before the camera exists.
#[must_use]
pub fn hdr_at_launch() -> Option<bool> {
    HDR_AT_LAUNCH.get().copied()
}
