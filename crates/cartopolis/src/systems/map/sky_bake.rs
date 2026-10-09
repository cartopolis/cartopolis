//! Re-bake the sky's reflections when the sky has changed, not every frame.
//!
//! Bevy's `AtmosphereEnvironmentMapLight` renders the sky into a cubemap and then
//! filters it into the diffuse and specular maps every surface is lit by — and it
//! does all of it **every frame**, whether or not anything moved. On the Steam
//! Deck that is the single largest cost in the frame: 17.5 ms of a 30 ms city
//! frame, more than every other graphics setting together (measured 2026-09-30,
//! `docs/notes/render-performance-survey.md`). None of it shows in
//! `RenderDiagnosticsPlugin`'s pass list, which is how it went unseen.
//!
//! What the bake reads is the sun and moon, and the camera's height in the
//! atmosphere and its place on the planet (the cubemap is world-axis aligned, so
//! turning the camera changes nothing). At the real clock the sun moves a quarter
//! of a degree a minute, so the map a frame bakes is the map the last one baked.
//!
//! So [`decide_sky_bake`] watches those inputs and asks for a bake only when one
//! has moved past a threshold below anything visible, plus a slow heartbeat for
//! the inputs it does not watch; and [`hold_sky_bake`], in the render world, takes
//! the two components that drive the bake off the camera's render entity on every
//! other frame. Extraction puts both back the next frame, so holding is
//! per-frame and nothing has to be restored. The maps the lighting samples are
//! assets of their own and keep their last contents.
//!
//! The camera's components are never touched — adding or removing the reflection
//! components on a live camera is the hazard `render_features` documents.
//!
//! One of the two is private to bevy_pbr (`AtmosphereEnvironmentMap`, the raw sky
//! render). [`render_features::find_env_map_component`] already finds its
//! `ComponentId` in the main world; its `TypeId` carries across to the render world,
//! which registered the same type. If it was never found only the filtering is
//! held, which is most of the cost.

use std::any::TypeId;

use bevy::ecs::component::Components;
use bevy::light::GeneratedEnvironmentMapLight;
use bevy::pbr::generate::RenderEnvironmentMap;
use bevy::prelude::*;
use bevy::render::extract_resource::ExtractResource;
use bevy::render::sync_world::RenderEntity;
use bevy::render::RenderApp;

use crate::components::{FreeCamera, PlanetAtmosphere};
use crate::systems::render_features::EnvMapComponent;
use crate::systems::sky::{MoonLight, SunLight};

/// A light or the local vertical turning by more than this re-bakes. A fifth of a
/// degree is under a minute of the real sun, and well under anything the eye can
/// pick out in a sky gradient.
const TURN_DEG: f32 = 0.2;

/// Climbing or descending by more than this fraction of the height above the
/// ground re-bakes — the sky's colour is a function of how much air is overhead,
/// and that changes by ratio, not by metres.
const CLIMB_FRACTION: f32 = 0.03;

/// …but never for less than this, or a walking avatar's bob would bake.
const CLIMB_MIN_M: f32 = 25.0;

/// Bake at least this often, for what the thresholds do not watch (the medium's
/// density, the lights' colour and strength — all of which follow the sun or the
/// height, so this is a backstop and not the mechanism).
const HEARTBEAT_S: f32 = 10.0;

/// Bake every frame for this long after the reflections appear. The first frames
/// can run before the bake's compute pipelines have compiled, and a bake that finds
/// no pipeline does nothing; holding then would leave the maps black until the
/// next trigger.
const WARM_UP_S: f32 = 3.0;

/// The render world's instruction for this frame.
#[derive(Resource, Clone, Default)]
pub struct SkyBakeOrder {
    /// Bake this frame.
    pub bake: bool,
    /// `TypeId` of bevy's private sky-render component, when it has been found.
    pub map_type: Option<TypeId>,
}

impl ExtractResource<RenderApp> for SkyBakeOrder {
    type Source = Self;

    fn extract_resource(source: &Self) -> Self {
        source.clone()
    }
}

/// What the last bake saw.
#[derive(Clone, Copy, PartialEq, Debug)]
pub(crate) struct BakeInputs {
    /// Unit vectors the sun and the moon shine along.
    pub sun: Vec3,
    pub moon: Vec3,
    /// The local vertical at the camera: away from the planet's centre.
    pub up: Vec3,
    /// Height above the planet's surface, metres.
    pub height_m: f32,
}

/// Main-world bookkeeping for [`decide_sky_bake`].
#[derive(Resource, Default)]
pub struct SkyBake {
    last: Option<BakeInputs>,
    last_bake_s: f32,
    render_entity: Option<Entity>,
    warm_until_s: f32,
    /// Frames baked and held, for the log line.
    baked: u32,
    held: u32,
}

/// `CARTO_SKY_BAKE_EVERY_FRAME=1` restores bevy's own behaviour, for an A/B of
/// this module's saving — or of its picture — without a rebuild.
fn every_frame() -> bool {
    static EVERY_FRAME: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *EVERY_FRAME.get_or_init(|| std::env::var_os("CARTO_SKY_BAKE_EVERY_FRAME").is_some())
}

/// Whether the sky has moved far enough since `last` to be worth a bake.
pub(crate) fn sky_moved(last: &BakeInputs, now: &BakeInputs) -> bool {
    let turn = TURN_DEG.to_radians().cos();
    let climb = (CLIMB_FRACTION * last.height_m.max(0.0)).max(CLIMB_MIN_M);
    last.sun.dot(now.sun) < turn
        || last.moon.dot(now.moon) < turn
        || last.up.dot(now.up) < turn
        || (last.height_m - now.height_m).abs() > climb
}

/// Decide whether this frame re-bakes the sky's reflections.
#[allow(clippy::type_complexity, clippy::too_many_arguments)]
pub fn decide_sky_bake(
    time: Res<Time<Real>>,
    components: &Components,
    env: Res<EnvMapComponent>,
    mut state: ResMut<SkyBake>,
    mut order: ResMut<SkyBakeOrder>,
    camera: Query<
        (
            &GlobalTransform,
            Option<&RenderEntity>,
            Has<GeneratedEnvironmentMapLight>,
        ),
        With<FreeCamera>,
    >,
    planet: Query<(&GlobalTransform, &bevy::light::Atmosphere), With<PlanetAtmosphere>>,
    sun: Query<&GlobalTransform, (With<SunLight>, Without<FreeCamera>)>,
    moon: Query<&GlobalTransform, (With<MoonLight>, Without<FreeCamera>, Without<SunLight>)>,
) {
    let now_s = time.elapsed_secs();
    order.map_type = env
        .id()
        .and_then(|id| components.get_info(id))
        .and_then(|info| info.type_id());

    let Ok((cam, render_entity, has_bake)) = camera.single() else {
        order.bake = true;
        return;
    };
    let render_entity = render_entity.map(|r| r.id());
    if !has_bake || render_entity != state.render_entity {
        // No reflections, or a fresh render entity (`apply_sky` re-syncs the camera
        // after a teardown): bake straight through the warm-up again.
        state.render_entity = render_entity;
        state.last = None;
        state.warm_until_s = now_s + WARM_UP_S;
        order.bake = true;
        return;
    }
    let (Ok((planet, atmosphere)), Ok(sun)) = (planet.single(), sun.single()) else {
        order.bake = true;
        return;
    };
    let from_centre = cam.translation() - planet.translation();
    let now = BakeInputs {
        sun: sun.forward().as_vec3(),
        // No moon is no moon movement; the sun's direction stands in.
        moon: moon
            .single()
            .map_or(sun.forward().as_vec3(), |m| m.forward().as_vec3()),
        up: from_centre.normalize_or(Vec3::Y),
        height_m: from_centre.length() - atmosphere.inner_radius,
    };

    let bake = every_frame()
        || now_s < state.warm_until_s
        || now_s - state.last_bake_s >= HEARTBEAT_S
        || state.last.as_ref().is_none_or(|last| sky_moved(last, &now));
    order.bake = bake;
    if bake {
        state.last = Some(now);
        state.last_bake_s = now_s;
        state.baked += 1;
    } else {
        state.held += 1;
    }
    if state.baked + state.held >= 3600 {
        debug!(
            baked = state.baked,
            held = state.held,
            "sky reflections re-baked"
        );
        state.baked = 0;
        state.held = 0;
    }
}

/// Render world: on a frame that does not bake, take the bake's inputs off every
/// entity that would run it. Runs after the extract commands have put them back.
pub fn hold_sky_bake(world: &mut World) {
    let Some(order) = world.get_resource::<SkyBakeOrder>().cloned() else {
        return;
    };
    if order.bake {
        return;
    }
    let map_id = order.map_type.and_then(|t| world.components().get_id(t));
    let probes: Vec<Entity> = world
        .query_filtered::<Entity, With<RenderEnvironmentMap>>()
        .iter(world)
        .collect();
    for probe in probes {
        let mut probe = world.entity_mut(probe);
        probe.remove::<RenderEnvironmentMap>();
        if let Some(id) = map_id {
            probe.remove_by_id(id);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn inputs() -> BakeInputs {
        BakeInputs {
            sun: Vec3::new(0.3, -0.8, 0.5).normalize(),
            moon: Vec3::new(-0.2, -0.4, 0.9).normalize(),
            up: Vec3::Y,
            height_m: 400.0,
        }
    }

    /// `v` turned through exactly `deg` — about an axis square to it, since
    /// turning about one it leans on moves it by less.
    fn turned(v: Vec3, deg: f32) -> Vec3 {
        Quat::from_axis_angle(v.any_orthonormal_vector(), deg.to_radians()) * v
    }

    #[test]
    fn a_still_sky_does_not_bake() {
        assert!(!sky_moved(&inputs(), &inputs()));
    }

    #[test]
    fn the_real_sun_bakes_about_once_a_minute() {
        // 15° an hour is 0.25° a minute.
        let mut now = inputs();
        now.sun = turned(now.sun, 0.25 * 0.5);
        assert!(!sky_moved(&inputs(), &now), "half a minute of sun");
        now.sun = turned(inputs().sun, 0.25);
        assert!(sky_moved(&inputs(), &now), "a minute of sun");
    }

    #[test]
    fn height_counts_by_ratio_with_a_floor() {
        let mut now = inputs();
        now.height_m = 400.0 + 20.0;
        assert!(!sky_moved(&inputs(), &now), "a 20 m bob at 400 m");
        now.height_m = 400.0 + 30.0;
        assert!(sky_moved(&inputs(), &now), "30 m is past the floor");

        let high = BakeInputs {
            height_m: 100_000.0,
            ..inputs()
        };
        let mut now = high;
        now.height_m = 102_000.0;
        assert!(!sky_moved(&high, &now), "2 % at 100 km");
        now.height_m = 104_000.0;
        assert!(sky_moved(&high, &now), "4 % at 100 km");
    }

    #[test]
    fn moving_round_the_planet_bakes() {
        let mut now = inputs();
        now.up = Quat::from_rotation_x(0.3_f32.to_radians()) * Vec3::Y;
        assert!(sky_moved(&inputs(), &now));
    }

    #[test]
    fn the_moon_counts_too() {
        let mut now = inputs();
        now.moon = turned(now.moon, 1.0);
        assert!(sky_moved(&inputs(), &now));
    }
}
