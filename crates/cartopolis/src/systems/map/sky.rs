//! The sky as lighting and weather: the sun and moon `DirectionalLight`s, the
//! day-night cycle (exposure, ambient, distance fog, window glow), the star
//! field, the moon disc, the horizon haze, the cloud decks, rain, and the
//! globe-tier cloud shell. `systems::sky_dome` is a different thing — the
//! analytic fallback dome for GPUs that cannot run bevy_pbr's atmosphere.
//!
//! Moved out of the crate root on 2026-08-19: this block was ~2,700 lines of
//! feature code in `lib.rs`, which put it in the same codegen unit as the whole
//! app assembly (`codegen-units` partitions by module), so tuning one cloud
//! constant re-codegened every system registration in the crate. Registration
//! stays in `lib.rs`; only the implementation lives here.

use bevy::camera::Exposure;
use bevy::light::light_consts::lux::RAW_SUNLIGHT;
use bevy::light::CascadeShadowConfigBuilder;
use bevy::pbr::{Material, MaterialPipeline, MaterialPipelineKey};
use bevy::prelude::*;
use bevy::render::mesh::MeshVertexBufferLayoutRef;
use bevy::render::render_resource::{
    AsBindGroup, RenderPipelineDescriptor, SpecializedMeshPipelineError,
};
use bevy::shader::ShaderRef;

use big_space::prelude::GridCell;

use crate::env_flag;
use crate::quality;
use crate::resources;
use crate::resources::DevPanelState;
use crate::systems;
use crate::utils;
use crate::utils::sun::sun_direction;

pub(crate) fn setup_sky(
    mut commands: Commands,
    dev: Res<DevPanelState>,
    prefs: Res<resources::AppPrefs>,
) {
    // The built-in atmosphere derives the sky from the sun's `DirectionalLight`
    // direction and the camera position, so there's no atmosphere model to poke
    // here — we just spawn the sun/moon lights. `AmbientLight` is a per-camera
    // component now (set in setup_camera, driven by apply_day_night_cycle).
    let shadows = prefs.shadows_on();
    spawn_sun_light(&mut commands, &dev, shadows);

    // Moonlight is a second directional light — no longer a shadow-casting one
    // (`spawn_moon_light`). It exists only while it actually contributes (moon up
    // at night); `manage_lights` spawns and despawns it rather than leaving a
    // light at zero illuminance in the scene all day, since bevy still computes
    // its cascade frusta and still walks it in every fragment's light loop.
    if moon_light_wanted(&dev) {
        spawn_moon_light(&mut commands, &dev, shadows);
    }
}

/// Spawn the sun's directional light. Shadows follow the dev panel toggle
/// (`dev.shadows_enabled`), applied at spawn — the toggle respawns the light
/// rather than mutating `shadow_maps_enabled`, which panics bevy_light.
/// Shadow cascades for the sun *and* the moon.
///
/// One constant rather than two literals because bevy_light panics outright if the
/// two directional lights disagree (see CLAUDE.md) — a shared value makes that
/// class of mistake unrepresentable rather than merely avoided.
///
/// The web gets 1: WebGPU reports a supported cascade limit of 1 here, and Bevy
/// clamps to it while warning. Asking for 4 anyway does not buy four cascades, it
/// just builds a config the backend then discards — so this asks for what the
/// platform can actually deliver.
///
/// Below that ceiling it is the tier's call, not a `cfg`'s. It used to be a bare
/// `if cfg!(target_arch = "wasm32") { 1 } else { 4 }`, and Android answers
/// "native" to every `cfg` in this tree — so a handset that turned shadows on got
/// a workstation's four cascades over two lights, i.e. up to eight shadow passes
/// across the ~79% of submitted triangles that cast. Latent today, because the
/// phone tier defaults `shadows: false`; live the moment anyone uses the dev
/// panel's toggle, and live for the Balanced rung as it stands.
///
/// Since 2026-09-11 only the sun casts, so "over two lights" is now at most four
/// passes rather than eight — but the constant still answers for both lights,
/// because the panic it exists to prevent is about the *configs* matching and does
/// not care which of them is casting.
///
/// Reading it as a `min` keeps the platform ceiling and the budget in the right
/// relationship: neither can raise what the other refuses.
pub(crate) fn shadow_cascades() -> usize {
    let platform_max = if cfg!(target_arch = "wasm32") { 1 } else { 4 };
    platform_max.min(quality::budget().shadow_cascades)
}

pub(crate) fn spawn_sun_light(commands: &mut Commands, dev: &DevPanelState, shadows: bool) {
    let sun_dir = sun_direction(dev.time_of_day, dev.sun_declination_rad);
    let (sun_color, illuminance) =
        sun_light(dev.time_of_day, dev.sun_declination_rad, dev.fog_density);
    commands.spawn((
        DirectionalLight {
            color: sun_color,
            illuminance,
            shadow_maps_enabled: shadows && !env_flag("CARTO_NO_SHADOWS"),
            ..default()
        },
        // Cascades sized to cover the visible city while keeping crisp near shadows.
        CascadeShadowConfigBuilder {
            num_cascades: shadow_cascades(),
            minimum_distance: 2.0,
            maximum_distance: quality::budget().shadow_distance_m,
            first_cascade_far_bound: 60.0,
            overlap_proportion: 0.2,
        }
        .build(),
        Transform::from_translation(Vec3::ZERO).looking_to(-sun_dir, Vec3::Y),
        // Bevy's procedural sun disk (drawn by the atmosphere's sky pass, so it
        // shows both from the ground and — raymarched — from orbit). Stated
        // explicitly rather than left to the `SunDisk::EARTH` default because
        // the size is a deliberate departure from it: at the true 0.53° the disc
        // is ~6 px wide at 1080p, a dot you have to hunt for. Widened to ~1.4°,
        // which is roughly what the eye reports anyway once glare is included,
        // and left at physical intensity so bloom does the rest.
        bevy::light::SunDisk {
            angular_size: SUN_DISK_ANGULAR_SIZE,
            intensity: 1.0,
        },
        SunLight,
    ));
    // `VolumetricLight` — the marker that makes this light cast shafts — is
    // **not** set here. It rides the Graphics setting, which can change while the
    // app runs, so `render_features::apply_render_features` owns adding and
    // removing it; this function is called again on every shadow toggle and would
    // be a second place that could disagree.
}

/// Apparent diameter of the sun disk, in radians (the true value is 0.0093).
///
/// Shared with `systems::sky_dome`, which draws its own disc on the adapters
/// where bevy's sky pass — and so bevy's `SunDisk` — never runs.
pub(crate) const SUN_DISK_ANGULAR_SIZE: f32 = 0.024;

/// Threshold on `moonlight_factor` below which the moon light is despawned: its
/// illuminance would be imperceptible, so paying for a second directional light
/// in every fragment's lighting loop is pure waste. (It stopped being a shadow
/// cost on 2026-09-11 — the moon does not cast — but it is still a light.)
pub(crate) const MOON_LIGHT_MIN: f32 = 0.002;

/// Whether the moon light should currently exist (moon up, illuminated, at night).
pub(crate) fn moon_light_wanted(dev: &DevPanelState) -> bool {
    moonlight_factor(dev.time_of_day, dev.moon_age_days, dev.sun_declination_rad) > MOON_LIGHT_MIN
}

/// Spawn the moon's directional light. Perceptually blue-gray, and — since
/// 2026-09-11 — **not a shadow caster**.
///
/// It used to cast, which doubled every shadow cost in the scene for the whole of
/// the night: a second set of cascades to rasterise every caster into, a second
/// set of shadow maps in memory at the same `DirectionalLightShadowMap` size, and
/// a second nine-tap lookup in every lit fragment. For a light at `MOONLIGHT_ILLUMINANCE`,
/// which is four orders of magnitude under the sun.
///
/// This is the same argument `systems::light_shafts` already makes about the same
/// light and settles the same way — "a second volumetric light doubles the march's
/// shadow work for something nobody would see" — and it should have been made
/// here first: a shaft is an optional effect, a shadow map is not.
///
/// What is lost is real and is worth naming so nobody re-adds it by accident:
/// under a full moon on a clear night a building no longer lays a faint shadow
/// across the street. That is a thing you have to go and look for; the frame it
/// costs is a thing you feel.
///
/// The `shadows` argument is therefore unused for the map itself and kept only to
/// keep the two spawners' signatures matched — `manage_lights` calls both from one
/// command buffer and a divergence there is how the two lights come to disagree.
/// The cascade config stays, identical to the sun's: bevy_light panics outright if
/// two directional lights carry different `num_cascades`, whether or not either is
/// casting.
pub(crate) fn spawn_moon_light(commands: &mut Commands, dev: &DevPanelState, _shadows: bool) {
    let moon_dir = utils::sun::moon_direction(dev.time_of_day, dev.moon_age_days);
    let illuminance = MOONLIGHT_ILLUMINANCE
        * moonlight_factor(dev.time_of_day, dev.moon_age_days, dev.sun_declination_rad)
        * (1.0 - 0.75 * dev.fog_density);
    commands.spawn((
        DirectionalLight {
            color: Color::linear_rgb(0.55, 0.72, 1.0),
            illuminance,
            // Never. See this function's own note — and note that bevy answers a
            // light with this off by clearing its cascade visible-entity lists
            // outright (`check_dir_light_mesh_visibility`), so it is a real
            // saving in the caster pass and not only in the fragment lookup.
            shadow_maps_enabled: false,
            ..default()
        },
        // Match the sun's cascade count (4): Bevy 0.19's bevy_light panics
        // (cascade-visibility index out of bounds) when two directional lights in
        // the scene have *different* numbers of shadow cascades. Spawning/despawning
        // the whole light (rather than toggling `shadow_maps_enabled`, which also
        // panics) keeps the two live lights' cascade counts equal at all times.
        CascadeShadowConfigBuilder {
            num_cascades: shadow_cascades(),
            minimum_distance: 2.0,
            maximum_distance: 1200.0,
            first_cascade_far_bound: 80.0,
            overlap_proportion: 0.2,
        }
        .build(),
        Transform::from_translation(Vec3::ZERO).looking_to(-moon_dir, Vec3::Y),
        // Explicit, because absent is not off: the atmosphere's sky pass reads
        // `Option<&SunDisk>` per directional light and its `unwrap_or_default()`
        // resolves to `SunDisk::EARTH` — so a moon light without this component
        // is drawn as a second sun-sized disc at the moon's position. The moon's
        // visible disc is `systems::moon`'s quad, not a sun disk.
        bevy::light::SunDisk::OFF,
        MoonLight,
    ));
}

/// Owns the sun/moon light lifecycle whenever the dev panel changes:
///   - moon spawned at dusk / despawned at dawn, so a second directional light is
///     only in the scene while moonlight is actually visible;
///   - both lights respawned when the shadow toggle flips (Bevy 0.19's bevy_light
///     panics if `shadow_maps_enabled` is mutated on a live light, so it
///     despawns/respawns instead — keeping their cascade counts matched, the
///     other panic condition).
/// One system, one command buffer: the despawn-then-spawn on a shadow flip can't
/// race the moon's dusk/dawn spawn into two moons.
pub(crate) fn manage_lights(
    mut commands: Commands,
    dev: Res<DevPanelState>,
    // Shadows are a *preference* now (Settings ▸ Graphics), so this system's
    // change gate has to watch both resources — with `dev.is_changed()` alone,
    // flipping the checkbox did nothing until the clock or the weather next moved.
    prefs: Res<resources::AppPrefs>,
    sun: Query<Entity, With<SunLight>>,
    moon: Query<Entity, With<MoonLight>>,
    mut last_shadows: Local<Option<bool>>,
) {
    if !dev.is_changed() && !prefs.is_changed() {
        return;
    }
    // A shadow-toggle flip (not the first run, where setup_sky already spawned
    // the lights correctly) respawns both lights with the new shadow state.
    let shadows = prefs.shadows_on();
    let prev = last_shadows.replace(shadows);
    let shadows_flipped = prev.is_some() && prev != Some(shadows);

    if shadows_flipped {
        for e in sun.iter() {
            commands.entity(e).despawn();
        }
        spawn_sun_light(&mut commands, &dev, shadows);
        for e in moon.iter() {
            commands.entity(e).despawn();
        }
        if moon_light_wanted(&dev) {
            spawn_moon_light(&mut commands, &dev, shadows);
        }
    } else {
        match (moon_light_wanted(&dev), moon.single()) {
            // wanted but absent
            (true, Err(_)) => spawn_moon_light(&mut commands, &dev, shadows),
            (false, Ok(entity)) => {
                commands.entity(entity).despawn(); // present but no longer wanted
            }
            _ => {}
        }
    }
}

/// Global trim on the sun's illuminance: the OSM basemap is near-white, so full
/// physical daylight drives most of the frame into the tonemapper's shoulder
/// (Bevy's default TonyMcMapface) and everything reads flat and washed out.
///
/// Shared with `systems::sky_dome`, whose sky scatters the same sunlight: a sky
/// brighter than the light lighting it is the sort of thing two private copies of
/// one number produce.
pub(crate) const SUN_SCALE: f32 = 0.75;

/// Camera exposure (EV100) at full daylight. Exposure adapts toward night by this
/// many stops, mimicking eye/camera adaptation so twilight and the lit city aren't
/// crushed by the bright-noon exposure. Lower ev100 = brighter.
///
/// Kept modest (2.0 stops = 4×): the old 3.5-stop lift (≈11×) multiplied the
/// night ambient floor and moon terms so hard that the near-white OSM ground read
/// as bright daylight, not night. Night should be dark — lit by the moon's
/// directional shaping and the glowing windows, not a lifted exposure.
pub(crate) const DAY_EXPOSURE_EV100: f32 = 13.6;
pub(crate) const NIGHT_EXPOSURE_LIFT: f32 = 2.0;
/// Extra stops around sunrise/sunset, on top of the night lift. The direct beam
/// there is ~3% of noon (air mass ~38 strips most of it), so the two lifts the
/// night curve gives are nowhere near enough — dawn rendered as a black city
/// under a dark red band. Real twilight *looks* bright because the eye adapts to
/// it; this is that adaptation. Gated on `horizon_proximity`, so it fades out
/// well before midnight and leaves the night floor untouched.
pub(crate) const TWILIGHT_EXPOSURE_LIFT: f32 = 2.5;
/// Stops of exposure lift at full overcast — the eye adapting to a duller sky,
/// exactly as [`TWILIGHT_EXPOSURE_LIFT`] is the eye adapting to dusk.
///
/// It has to *pay for the light the overcast model removes*, and at 0.4 it did
/// not come close. [`overcast_lighting`] leaves ~20 klux of ground illuminance at
/// noon against a clear sky's ~93 klux — 2.2 stops — so 0.4 of a lift rendered a
/// grey day 1.8 stops (3.5×) under. That, and not the cloud shader, is most of
/// what "the lighting is bad" was on the phone: a grey day is *soft*, not dark,
/// and the eye does not experience it as three stops down from a sunny one.
///
/// Not the full 2.2 either: an overcast day should still read a little duller
/// than a sunny one, and adapting the whole way would make the two identical.
/// The residual ~0.6 stop is that difference, and `overcast_tests` holds it
/// there — the three constants here only mean anything as a set.
pub(crate) const OVERCAST_EXPOSURE_LIFT: f32 = 1.6;
/// Fraction of the direct beam a full overcast takes away.
///
/// A stratus deck does not thin the sun, it *replaces* it: at full cover there is
/// no beam and no shadow left, only a sky-wide light source. At the old 0.72 the
/// sun still carried 28 % of a noon beam through a solid deck — a directional key
/// light under a sky that by construction has no direction.
pub(crate) const OVERCAST_BEAM_CUT: f32 = 0.92;
/// Illuminance (lux) the deck itself contributes at full overcast, on top of the
/// clear-sky ambient.
///
/// This is where the beam goes. A cut with nowhere for the energy to land is just
/// a dimmer, and that is what the old model was: 0.72 off the sun against 1.8× on
/// an ambient of 900 lux put back 720 of the ~66,000 lux it took. An overcast sky
/// is the brightest thing in the frame and lights the ground from every direction
/// at once, so the overcast look is this term *dominating* the beam — soft,
/// shadowless, and bright. There is a test for that ordering, because it is the
/// one that makes a grey day read as a grey day rather than as a dim sunny one.
///
/// 12 klux against the ~7 klux of beam left puts a full overcast at ~20 klux of
/// ground illuminance, about two thirds of it diffuse. Real overcast daylight is
/// 15–25 klux and effectively all diffuse; the beam that survives here is the
/// price of `cloud_cover` being one scalar for every kind of cloud there is.
pub(crate) const OVERCAST_SKY_FILL: f32 = 12_000.0;

/// How saturated the sun's *light* colour is (1 = full transmittance colour,
/// 0 = white). Kept well below 1 because the built-in atmosphere already reddens
/// the sky at low sun and the near-white basemap takes the cast readily: at 0.7
/// the whole city went maroon at dawn, ambient and all.
pub(crate) const SUN_COLOR_SATURATION: f32 = 0.25;
/// Exponent softening our own extinction of the sun's *magnitude* (1 = the full
/// Kasten–Young air-mass curve, 0 = none).
///
/// Bevy's atmosphere is fed by the sun `DirectionalLight`'s colour and
/// illuminance and runs its own scattering/extinction on top (`bevy_pbr`
/// atmosphere docs). Handing it a beam we had already extincted double-counted
/// the air mass: at sunrise the raw curve leaves 9% of the beam, the atmosphere
/// then took its own cut, and the sky came out near-black maroon instead of a
/// lit dawn. Softening the exponent (0.09 → 0.32 at the horizon) leaves noon
/// essentially untouched — the curve only bends where the air mass is large.
pub(crate) const SUN_EXTINCTION_SOFTNESS: f32 = 0.45;
/// Peak added ambient brightness at noon. Low on purpose: the sky's own IBL
/// (`AtmosphereEnvironmentMapLight`) already fills the shadows, so ambient piled
/// on top only flattens midday — the shadows are where the depth comes from, and
/// at 1800 the near-white basemap and the near-white buildings washed into one
/// pale sheet.
pub(crate) const AMBIENT_DAY_BRIGHTNESS: f32 = 900.0;
/// …and what stands in for it on a run that has **no** environment map.
///
/// The constant above is a trim, and the sentence justifying it — "the sky's own
/// IBL already fills the shadows" — is a statement about a component that half
/// the tiers do not carry. `quality::Budget::sky_ibl` is false on the laptop and
/// the phone, and `SkyMode::Dome` has no atmosphere to bake a cubemap from at
/// all, so on every one of those runs 900 lux was the *entire* fill: the sun, and
/// then nothing. It shows exactly where you would predict — a facade turned away
/// from the sun goes to near-black, and water (albedo `#0F2935`, roughness 0.06,
/// i.e. a mirror with nothing to mirror) renders as a black hole in the map. Both
/// are in the Fairphone screenshots this was tuned against; `water.rs`'s own
/// module header names the environment map as what its sea reflects.
///
/// The magnitude is the analytic sky's own hemispherical irradiance, discounted.
/// Integrating the sky-dome model over the hemisphere at noon gives ~16 klux
/// (luminance-weighted) — which is the right order for a real clear sky, and is
/// what bevy's `AtmosphereEnvironmentMapLight` delivers on the tiers that have
/// it. `AmbientLight` cannot be handed that number: it is unoccluded and arrives
/// from every direction including below, where the IBL it replaces has a ground
/// half that is dark and a sky half that is not. A quarter of it is the honest
/// discount for that, and it is still a *fill* rather than a key — the sun is
/// ~60 klux beside it.
///
/// This also puts water back, and for a reason worth knowing: bevy's ambient is
/// not diffuse-only. `ambient.wgsl` runs `EnvBRDFApprox` for the specular lobe
/// too, so raising it restores the grazing-angle sheen a water surface is almost
/// entirely made of — no cubemap, no per-cell material churn.
pub(crate) const AMBIENT_DAY_BRIGHTNESS_NO_IBL: f32 = 4_000.0;
/// Ambient floor at night — a faint blue city skyglow so the world doesn't vanish
/// into pure black between the lit windows. Deliberately low: this is a *floor*,
/// not the main night light (that's the moon + window glow). At the old value of
/// 80 the near-white OSM ground lit up almost like day once the exposure lift
/// (see `NIGHT_EXPOSURE_LIFT`) amplified it.
pub(crate) const AMBIENT_NIGHT_BRIGHTNESS: f32 = 15.0;
/// Peak moonlight illuminance (full moon at the zenith), on the same trimmed
/// scale as the sun. Deliberately well above the night ambient floor: for the
/// directional shaping to *read* — lit roofs and moon-facing facades standing
/// out, soft shadows behind buildings — moonlight must dominate the ambient,
/// which is also how a real moonlit night looks (the full moon is ~2500× any
/// other natural night-sky light).
pub(crate) const MOONLIGHT_ILLUMINANCE: f32 = 180.0;
/// Peak added ambient from a full moon overhead — the sky-scattered part. Halved
/// alongside the night-ambient trim so a full moon lifts the scene enough to read
/// by without flattening it back to daylight brightness.
pub(crate) const MOON_AMBIENT_BRIGHTNESS: f32 = 40.0;
/// Ambient in the space tier — Earthshine scale, not sky scale (see
/// `apply_day_night_cycle`). There is no atmosphere overhead to scatter light
/// into the night side from orbit, so this is only a floor that keeps the dark
/// half legible against the starfield.
pub(crate) const SPACE_AMBIENT_BRIGHTNESS: f32 = 120.0;
/// Stops the camera stops down by in deep space (positive = darker), faded in
/// from the globe threshold on `space_factor`. Held near 0: the near-white map
/// imagery sits on the tonemapper's shoulder, where stopping the camera down
/// barely darkens the surface but does dim the limb and stars. The surface
/// darkening is a tint on the imagery instead (`globe::GLOBE_DEEP_ALBEDO`), which
/// bites where exposure can't; this is left as a small trim knob.
pub(crate) const SPACE_EXPOSURE_STOPS: f32 = 0.0;

/// Marker for the sun's directional light.
#[derive(Component)]
pub(crate) struct SunLight;

/// Marker for the moon's directional light.
#[derive(Component)]
pub(crate) struct MoonLight;

/// Moonlight strength 0–1: moon elevation × illuminated fraction, faded out by
/// day (moonlight is negligible next to any amount of daylight).
pub(crate) fn moonlight_factor(hours: f32, moon_age_days: f32, decl_rad: f32) -> f32 {
    let up = utils::sun::moon_direction(hours, moon_age_days).y.max(0.0);
    up * utils::sun::moon_illuminated_fraction(moon_age_days)
        * (1.0 - utils::sun::daylight(hours, decl_rad))
}

/// Sun light color and illuminance for a time of day. The hue and magnitude both
/// come from the atmosphere's transmittance toward the sun (see `utils::sun`), so
/// the light warms and dims exactly as the sky it hangs in: warm white at noon,
/// gold in the morning/evening, deep red and faint as the disc grazes the horizon.
pub(crate) fn sun_light(hours: f32, decl_rad: f32, fog_density: f32) -> (Color, f32) {
    let t = utils::sun::sun_transmittance(hours, decl_rad);
    let peak = t.max_element().max(1e-4);
    // Hue in the color (peak-normalized), magnitude in the illuminance. Desaturated
    // toward white so the sun light doesn't compound the atmosphere's own sunset red.
    let hue = Vec3::ONE.lerp(
        Vec3::new(t.x / peak, t.y / peak, t.z / peak),
        SUN_COLOR_SATURATION,
    );
    let color = Color::linear_rgb(hue.x, hue.y, hue.z);
    // Fog diffuses sunlight: dim the direct sun (softening shadows) as density rises.
    let sun_through_fog = 1.0 - 0.75 * fog_density;
    // Physical sunlight (RAW_SUNLIGHT ≈ 130k lux, unfiltered) so the built-in
    // atmosphere is lit correctly — its sky brightness/colour scale with the sun's
    // illuminance. The camera's Exposure (see setup_camera) brings this back to a
    // sensible scene brightness; tune `ATMOSPHERE_EXPOSURE_EV100` to taste.
    let illuminance = RAW_SUNLIGHT
        * SUN_SCALE
        * peak.powf(SUN_EXTINCTION_SOFTNESS)
        * utils::sun::sun_disc(hours, decl_rad)
        * sun_through_fog;
    (color, illuminance)
}

/// Ambient light for a time of day: brightness follows the sky through twilight
/// (the sky lights the city before sunrise and after sunset), color follows the
/// sky's hue — blue night, warm dawn/dusk, cool-white day.
pub(crate) fn ambient_light(
    hours: f32,
    moon_age_days: f32,
    decl_rad: f32,
    sky_ibl: bool,
) -> AmbientLight {
    let c = utils::sun::ambient_sky_color(hours, decl_rad);
    let day = if sky_ibl {
        AMBIENT_DAY_BRIGHTNESS
    } else {
        AMBIENT_DAY_BRIGHTNESS_NO_IBL
    };
    AmbientLight {
        color: Color::linear_rgb(c.x, c.y, c.z),
        brightness: AMBIENT_NIGHT_BRIGHTNESS
            + day * utils::sun::daylight(hours, decl_rad)
            + MOON_AMBIENT_BRIGHTNESS * moonlight_factor(hours, moon_age_days, decl_rad),
        affects_lightmapped_meshes: true,
    }
}

/// How much of the sky is cloud, 0 (clear) to 1 (overcast): thick cloud, and a
/// little of the rain. One definition, because the ground lighting and the
/// water's stand-in sky reflection (`water::update_water_waves`) both grey out on
/// it and would disagree about the weather otherwise.
pub(crate) fn overcast(dev: &DevPanelState) -> f32 {
    dev.cloud_cover.max(dev.precipitation * 0.9).clamp(0.0, 1.0)
}

/// How a cloud deck redistributes the ground's light: what is left of the direct
/// beam, and the diffuse fill that replaces what it took.
///
/// Split out of [`apply_day_night_cycle`] because it is the whole overcast look
/// in two numbers and because the balance between them — not either one alone —
/// is what has to hold (there is a test). The failure it replaces was a model
/// that only ever *subtracted*: it cut the beam hard, added almost nothing back,
/// and left [`OVERCAST_EXPOSURE_LIFT`] to pay for a three-stop hole with 0.4 of a
/// stop. An overcast day is not a dim day, it is a day whose light arrives from
/// the whole sky instead of from one direction.
pub(crate) fn overcast_lighting(overcast: f32) -> (f32, f32) {
    (
        1.0 - OVERCAST_BEAM_CUT * overcast,
        OVERCAST_SKY_FILL * overcast,
    )
}

/// Marker for the catalog star field (spun around the celestial pole).
#[derive(Component)]
pub(crate) struct StarSky;

/// Global brightness multiplier for the star field.
///
/// Calibrated on the **ground at night**, and that is the whole of why the next
/// constant exists.
pub(crate) const STAR_BRIGHTNESS: f32 = 3.5;

/// What the star field is multiplied by in the space tier, to undo the exposure
/// that tier deliberately holds.
///
/// [`STAR_BRIGHTNESS`] is judged against the ground's *night* exposure, which is
/// [`DAY_EXPOSURE_EV100`] lifted by [`NIGHT_EXPOSURE_LIFT`] — the eye adaptation
/// that makes a night scene read. The space tier has no local clock to adapt to
/// (the lit half of the planet is in full daylight whatever the time is at the
/// anchor), so `apply_day_night_cycle` pins it at the *day* exposure
/// ([`SPACE_EXPOSURE_STOPS`]). Stars therefore arrive `NIGHT_EXPOSURE_LIFT` stops
/// under the exposure they were tuned for, and a stop is a factor of two.
///
/// Measured before the fix (2026-08-19, orbit at 5,000 km on the balanced tier):
/// of 120,000 sky pixels, **78 exceeded luminance 32/255 and none reached 128**.
/// The field was rendering correctly and was simply below the threshold of a
/// screen — reported from a phone as "there are no stars", and reproducible on
/// the desktop the moment anyone counted pixels instead of looking.
///
/// Derived from the constant rather than typed as a number, so it stays correct
/// if either exposure constant moves. It is a compensation, not a taste knob:
/// the stars end up as bright *relative to their exposure* as they are at night
/// on the ground, which is where they were judged.
///
/// A function rather than a `const` only because `f32::exp2` is not `const fn`.
/// Writing the `4.0` out by hand instead would be the drift this is built to
/// avoid — the number is only correct while `NIGHT_EXPOSURE_LIFT` is 2.0.
#[must_use]
pub(crate) fn space_star_exposure_compensation() -> f32 {
    // A stop is a factor of two in luminance, so undoing N stops is 2^N.
    NIGHT_EXPOSURE_LIFT.exp2()
}

/// Star-shell radius: beyond all world geometry (so buildings and the ground
/// occlude stars) but inside the far plane and the atmosphere skybox faces.
pub(crate) const STAR_SHELL_RADIUS: f32 = 150_000.0;

/// Material for the catalog star field (see `shaders/starfield.wgsl`): additive
/// gaussian points, with per-star tint and twinkle seed in the vertex colors.
/// `params.x` = overall brightness (night factor × fog veil), `.y` = twinkle clock.
#[derive(Asset, TypePath, AsBindGroup, Clone)]
pub(crate) struct StarFieldMaterial {
    #[uniform(0)]
    params: Vec4,
}

impl Material for StarFieldMaterial {
    fn fragment_shader() -> ShaderRef {
        "shaders/starfield.wgsl".into()
    }

    fn alpha_mode(&self) -> AlphaMode {
        AlphaMode::Add
    }

    fn specialize(
        _pipeline: &MaterialPipeline,
        descriptor: &mut RenderPipelineDescriptor,
        _layout: &MeshVertexBufferLayoutRef,
        _key: MaterialPipelineKey<Self>,
    ) -> Result<(), SpecializedMeshPipelineError> {
        use bevy::render::render_resource::{
            BlendComponent, BlendFactor, BlendOperation, BlendState,
        };
        // Quad winding depends on each star's tangent basis; draw both faces.
        descriptor.primitive.cull_mode = None;
        // Don't write depth — it must not occlude the atmosphere or anything else.
        if let Some(depth) = descriptor.depth_stencil.as_mut() {
            depth.depth_write_enabled = Some(false);
        }
        // Force pure additive blending (src + dst): black gaps add nothing, stars add light.
        if let Some(fragment) = descriptor.fragment.as_mut() {
            if let Some(Some(target)) = fragment.targets.get_mut(0) {
                target.blend = Some(BlendState {
                    color: BlendComponent {
                        src_factor: BlendFactor::One,
                        dst_factor: BlendFactor::One,
                        operation: BlendOperation::Add,
                    },
                    alpha: BlendComponent {
                        src_factor: BlendFactor::Zero,
                        dst_factor: BlendFactor::One,
                        operation: BlendOperation::Add,
                    },
                });
            }
        }
        Ok(())
    }
}

/// Marker for the moon disc quad.
#[derive(Component)]
pub(crate) struct Moon;

/// Angular radius of the moon *quad* in radians. The disc proper fills the inner
/// ~42% of it (`DISC_R` in moon.wgsl) — ~0.7°, ~2.5× the real moon, which is how
/// cameras exaggerate it too; the rest of the quad carries the radiant glow halo.
pub(crate) const MOON_ANGULAR_RADIUS: f32 = 0.028;
/// Moon shell distance: inside the star shell so the disc occludes stars behind it.
pub(crate) const MOON_DISTANCE: f32 = 140_000.0;

/// Material for the moon disc (see `shaders/moon.wgsl`): a sphere-shaded quad lit
/// by the sun direction expressed in the quad's local frame (xyz), so the lunar
/// phase and the terminator's orientation emerge from the real sun–moon geometry.
/// `sun_local.w` = overall brightness; alpha-blended so the disc covers stars.
#[derive(Asset, TypePath, AsBindGroup, Clone)]
pub(crate) struct MoonMaterial {
    #[uniform(0)]
    sun_local: Vec4,
}

impl Material for MoonMaterial {
    fn fragment_shader() -> ShaderRef {
        "shaders/moon.wgsl".into()
    }

    fn alpha_mode(&self) -> AlphaMode {
        AlphaMode::Blend
    }

    fn specialize(
        _pipeline: &MaterialPipeline,
        descriptor: &mut RenderPipelineDescriptor,
        _layout: &MeshVertexBufferLayoutRef,
        _key: MaterialPipelineKey<Self>,
    ) -> Result<(), SpecializedMeshPipelineError> {
        descriptor.primitive.cull_mode = None;
        // Don't write depth — it must not occlude the atmosphere or the stars' pass.
        if let Some(depth) = descriptor.depth_stencil.as_mut() {
            depth.depth_write_enabled = Some(false);
        }
        Ok(())
    }
}

/// Spawns the moon disc: a single quad in its local XY plane with UVs spanning
/// [-1, 1]²; `update_moon` points it at the camera and shades it via the material.
pub(crate) fn setup_moon(
    mut commands: Commands,
    mut meshes: ResMut<Assets<Mesh>>,
    mut materials: ResMut<Assets<MoonMaterial>>,
) {
    let s = MOON_DISTANCE * MOON_ANGULAR_RADIUS;
    let mut mesh = Mesh::new(
        bevy::render::mesh::PrimitiveTopology::TriangleList,
        bevy::asset::RenderAssetUsages::RENDER_WORLD,
    );
    let corners = [(-1.0f32, -1.0f32), (1.0, -1.0), (1.0, 1.0), (-1.0, 1.0)];
    mesh.insert_attribute(
        Mesh::ATTRIBUTE_POSITION,
        corners
            .iter()
            .map(|&(u, v)| [u * s, v * s, 0.0])
            .collect::<Vec<_>>(),
    );
    mesh.insert_attribute(Mesh::ATTRIBUTE_NORMAL, vec![[0.0, 0.0, 1.0]; 4]);
    mesh.insert_attribute(
        Mesh::ATTRIBUTE_UV_0,
        corners.iter().map(|&(u, v)| [u, v]).collect::<Vec<_>>(),
    );
    mesh.insert_indices(bevy::render::mesh::Indices::U32(vec![0, 1, 2, 0, 2, 3]));

    commands.spawn((
        Mesh3d(meshes.add(mesh)),
        MeshMaterial3d(materials.add(MoonMaterial {
            sun_local: Vec4::ZERO, // set each frame; starts invisible
        })),
        Transform::default(),
        bevy::light::NotShadowCaster,
        bevy::light::NotShadowReceiver,
        Moon,
    ));
}

/// Places the moon disc on the sky shell around the camera and feeds the material
/// the sun direction in the quad's local frame (phase shading) plus the overall
/// brightness: fading in through dusk, veiled by fog, extinguished near the horizon.
#[allow(clippy::type_complexity)]
pub(crate) fn update_moon(
    dev: Res<DevPanelState>,
    globe: Res<systems::globe::GlobeState>,
    camera: Query<&GlobalTransform, With<Camera3d>>,
    free_camera: Query<
        (&GridCell, &Transform),
        (With<crate::components::FreeCamera>, Without<Moon>),
    >,
    mut moon: Query<(&mut Transform, &MeshMaterial3d<MoonMaterial>), With<Moon>>,
    mut materials: ResMut<Assets<MoonMaterial>>,
) {
    let Ok(cam) = camera.single() else {
        return;
    };
    let Ok((mut tf, mat_handle)) = moon.single_mut() else {
        return;
    };
    let moon_dir = utils::sun::moon_direction(dev.time_of_day, dev.moon_age_days);
    let rotation = Quat::from_rotation_arc(Vec3::Z, -moon_dir);
    // Same shell rescale as the stars: at its ground distance the disc is nearer
    // than the planet, so from orbit an opaque moon quad hung in front of Earth.
    let space_shell = free_camera
        .single()
        .ok()
        .and_then(|(cell, cam_tf)| globe.sky_shell_radius(cell, cam_tf));
    let scale = space_shell.map_or(1.0, |r| r / MOON_DISTANCE);
    // The disc follows the camera every frame so it stays fixed on the sky.
    tf.translation = cam.translation() + moon_dir * MOON_DISTANCE * scale;
    tf.rotation = rotation;
    tf.scale = Vec3::splat(scale);

    // The material (phase lighting + brightness) depends only on time of day, so
    // rewrite its uniform only when that changed — not every frame.
    if !dev.is_changed() {
        return;
    }

    // Visible from twilight on (the moon shows before full dark), dimmed by fog
    // and by atmospheric extinction as it nears the horizon.
    let sun_elev = sun_direction(dev.time_of_day, dev.sun_declination_rad).y;
    let dusk = ((0.05 - sun_elev) / 0.15).clamp(0.0, 1.0);
    let fog_veil = (-3.0 * dev.fog_density).exp();
    let extinction = 0.3 + 0.7 * utils::sun::smoothstep(0.0, 0.25, moon_dir.y);
    let brightness = dusk * fog_veil * extinction;

    if let Some(mut mat) = materials.get_mut(&mat_handle.0) {
        // Phase-consistent sunlight (not the raw sun direction — see sun.rs).
        let sun_local = rotation.inverse()
            * utils::sun::moon_sunlight_direction(
                dev.time_of_day,
                dev.moon_age_days,
                dev.sun_declination_rad,
            );
        mat.sun_local = sun_local.extend(brightness);
    }
}

/// Marker for the fog-colored horizon-haze shell (see `shaders/horizonhaze.wgsl`).
#[derive(Component)]
pub(crate) struct HorizonHaze;

/// Alpha-blended band of fog color around the horizon, bridging the fogged ground
/// and the atmosphere sky (whose single-scattering model darkens at grazing angles).
/// Colored from the camera's `DistanceFog` each frame (see `update_horizon_haze`).
#[derive(Asset, TypePath, AsBindGroup, Clone)]
pub(crate) struct HorizonHazeMaterial {
    #[uniform(0)]
    color: LinearRgba,
    /// x: elevation where the haze fades out fully, y: peak alpha,
    /// z: directional-light exponent (w unused).
    #[uniform(1)]
    params: Vec4,
    /// `DistanceFog::directional_light_color` — rgb tint, alpha = strength.
    #[uniform(2)]
    dir_light_color: LinearRgba,
    /// x: view-direction `y` where the shell's own rim sits (negative — the ray
    /// that grazes the shell exactly where the ground does). The haze fades to
    /// nothing there, so the rim itself never draws an edge. yzw unused.
    #[uniform(3)]
    rim: Vec4,
}

impl Material for HorizonHazeMaterial {
    fn fragment_shader() -> ShaderRef {
        "shaders/horizonhaze.wgsl".into()
    }

    fn alpha_mode(&self) -> AlphaMode {
        AlphaMode::Blend
    }

    fn specialize(
        _pipeline: &MaterialPipeline,
        descriptor: &mut RenderPipelineDescriptor,
        _layout: &MeshVertexBufferLayoutRef,
        _key: MaterialPipelineKey<Self>,
    ) -> Result<(), SpecializedMeshPipelineError> {
        // Camera is inside the shell; render its interior, and don't occlude
        // anything behind it (depth test still lets world geometry cover it).
        descriptor.primitive.cull_mode = None;
        if let Some(depth) = descriptor.depth_stencil.as_mut() {
            depth.depth_write_enabled = Some(false);
        }
        Ok(())
    }
}

/// When Settings ▸ World's "Run the day" toggle is on, advances the time of day
/// smoothly, looping the full 24 h over `cycle_seconds`.
/// Drive `time_of_day` from the sun-mode switcher (HUD Auto/Day/Night). `Auto`
/// tracks the real local *solar* time at the map anchor (so the sun is where it
/// really is right now); `Day`/`Night` pin noon/midnight; `Manual` hands the
/// clock to Settings ▸ World. Runs before `advance_day_cycle` and
/// `apply_day_night_cycle`, so the whole sun/moon/fog pipeline sees the value.
pub(crate) fn apply_sun_mode(
    clock: Res<systems::transit_live::WorldClock>,
    globe: Res<systems::globe::GlobeState>,
    mut dev: ResMut<DevPanelState>,
) {
    // Solar declination depends only on the date, not the sun mode — so track it
    // from the real clock in *every* mode, including Manual (the dev panel drives
    // the time of day, but the season is still today's). This is what gives the
    // sun its correct seasonal day length; without it the arc set sunset at 18:00
    // year-round (a July evening in NL read as dark hours too early).
    let day_of_year = jiff::Timestamp::from_second(clock.now_unix as i64)
        .map(|ts| ts.to_zoned(jiff::tz::TimeZone::UTC).date().day_of_year() as f32)
        .unwrap_or(80.0); // equinox fallback
    let target_decl = utils::sun::solar_declination_rad(day_of_year);
    if (dev.sun_declination_rad - target_decl).abs() > 1e-5 {
        dev.sun_declination_rad = target_decl;
    }

    // Manual hands the rest of the day/night group — time *and* moon — to the dev
    // panel (slider / presets / Quick-cycle).
    if dev.sun_mode == resources::SunMode::Manual {
        return;
    }
    let target_time = match dev.sun_mode {
        resources::SunMode::Day => 12.0,
        resources::SunMode::Night => 0.0,
        // Quantize to the minute: the sun moves imperceptibly between, and this
        // keeps `apply_day_night_cycle` (gated on `dev.is_changed()`) from
        // re-lighting the whole scene every single frame.
        resources::SunMode::Auto => {
            let solar = utils::sun::solar_time_of_day(clock.now_unix, globe.anchor_lng);
            (solar * 60.0).round() / 60.0
        }
        resources::SunMode::Manual => unreachable!("returned above"),
    };
    // The cycle includes the moon: in every automatic mode its phase and
    // position ride the real date, not a fixed dev-panel value.
    let target_moon = utils::sun::lunar_age_days(clock.now_unix);
    // Only touch the resource when a value actually moves, so change detection
    // stays quiet once Day/Night settle (and between Auto's minute ticks).
    if (dev.time_of_day - target_time).abs() > 1e-4 {
        dev.time_of_day = target_time;
    }
    if (dev.moon_age_days - target_moon).abs() > 1e-4 {
        dev.moon_age_days = target_moon;
    }
}

pub(crate) fn advance_day_cycle(time: Res<Time>, mut dev: ResMut<DevPanelState>) {
    // Quick-cycle animates `time_of_day`/`moon_age_days` directly, so it only
    // runs in Manual — any automatic mode overwrites them from the real clock.
    if !dev.auto_cycle || dev.sun_mode != resources::SunMode::Manual {
        return;
    }
    let speed = 24.0 / dev.cycle_seconds.max(0.1); // hours per real second
    let dh = time.delta_secs() * speed;
    dev.time_of_day = (dev.time_of_day + dh).rem_euclid(24.0);
    // The moon ages with the cycling days, so moonrise drifts ~50 min later per
    // day and the phase creeps through the month, just like the real moon.
    dev.moon_age_days = (dev.moon_age_days + dh / 24.0).rem_euclid(utils::sun::LUNAR_CYCLE_DAYS);
}

#[allow(clippy::type_complexity)]
pub(crate) fn apply_day_night_cycle(
    dev: Res<DevPanelState>,
    globe: Res<systems::globe::GlobeState>,
    mut sun: Query<(&mut Transform, &mut DirectionalLight), (With<SunLight>, Without<MoonLight>)>,
    mut moon: Query<(&mut Transform, &mut DirectionalLight), With<MoonLight>>,
    // AmbientLight is a per-camera component in Bevy 0.19 (attached in setup_camera).
    mut ambient: Query<&mut AmbientLight>,
    camera: Query<
        (
            &GridCell,
            &Transform,
            &mut Exposure,
            // `Option`, because a tier that does not want glare omits `Bloom`
            // rather than zeroing it (`free_camera::setup_camera` says why: the
            // component carries `#[require(Hdr)]`, so keeping it costs 8 B/px of
            // main texture even at zero intensity). Naming it bare here would take
            // the camera out of *this whole query* on such a tier — no exposure
            // adaptation, no day/night cycle at all — which is a lighting failure
            // that looks nothing like a missing bloom.
            Option<&mut bevy::post_process::bloom::Bloom>,
            // Whether anything but `AmbientLight` is filling the shadowed side.
            // Asked of the camera rather than of `quality::Budget`, because two
            // independent things can withhold it — the tier (`sky_ibl`) and the
            // adapter (no atmosphere at all ⇒ nothing to bake a cubemap from,
            // `SkyMode::Dome`) — and `setup_camera` has already resolved both
            // into this one component's presence. See
            // `AMBIENT_DAY_BRIGHTNESS_NO_IBL`.
            Option<&bevy::light::AtmosphereEnvironmentMapLight>,
        ),
        (
            With<crate::components::FreeCamera>,
            Without<SunLight>,
            Without<MoonLight>,
        ),
    >,
    // The space tier changes the whole lighting model, so the `dev.is_changed()`
    // gate has to let a climb through it too — `space_factor` at the last apply.
    mut was_space: Local<f32>,
) {
    let mut camera = camera;
    // 0 on the ground, 1 in deep space, smooth in between: every lighting term
    // below crossfades on it. As a boolean these all stepped at once, mid-zoom.
    let space = camera
        .single()
        .map(|(cell, tf, _, _, _)| globe.space_factor(cell, tf))
        .unwrap_or(0.0);
    let sky_ibl = camera.single().is_ok_and(|(_, _, _, _, env)| env.is_some());
    if !dev.is_changed() && (space - *was_space).abs() < SPACE_RAMP_STEP {
        return;
    }
    *was_space = space;

    // Overcast: thick cloud (and, a little, rain) dims and flattens the ground
    // lighting — the direct sun drops, the sky's fill light rises, colours grey
    // out. Ground tier only (space has its own lighting model, and the planet
    // cloud sphere shades the globe there), so it fades out with `space`.
    let overcast = overcast(&dev) * (1.0 - space);

    let sun_dir = sun_direction(dev.time_of_day, dev.sun_declination_rad);

    // Adapt exposure to the light level (eye/camera adaptation): brighten toward
    // twilight and night by up to NIGHT_EXPOSURE_LIFT stops so dusk and the lit city
    // aren't crushed by the bright-noon exposure that suits the physical daytime sun.
    // From space there's nothing to adapt to — the lit half of the planet is in full
    // daylight whatever the local clock says — so the day exposure holds.
    let day = utils::sun::daylight(dev.time_of_day, dev.sun_declination_rad);
    let ground_ev = DAY_EXPOSURE_EV100
        - NIGHT_EXPOSURE_LIFT * (1.0 - day)
        - TWILIGHT_EXPOSURE_LIFT * utils::sun::horizon_proximity(dev.time_of_day, dev.sun_declination_rad)
        // A small lift so an overcast scene reads dim-but-legible rather than
        // being crushed once the direct sun is pulled down.
        - OVERCAST_EXPOSURE_LIFT * overcast;
    if let Ok((_, _, mut exp, bloom, _)) = camera.single_mut() {
        // The globe's map imagery is bright (near-white paper); from orbit the
        // sunlit hemisphere blows out, so stop the camera down as you climb. This
        // rides `space_factor`, so at the tier boundary it's exactly the ground
        // exposure — the globe there renders as bright as the flat tiles it
        // replaces, no step — and only deep in space does it darken to a
        // planetary look. (The darkening lives here, not baked into the tile
        // material, precisely so it can be zero at the boundary; a fixed material
        // tint can't be, because exposure is global and can't tell the globe
        // above the boundary from the flat tiles just below it.)
        let space_ev = DAY_EXPOSURE_EV100 + SPACE_EXPOSURE_STOPS;
        exp.ev100 = ground_ev.lerp(space_ev, space);
        // More glare the farther out you go (see the constants): from deep space
        // the limb is a few pixels of arc, and glare is most of what makes it
        // read as an atmosphere rather than a drawn outline. Near the ground
        // that much bloom just smears the map.
        if let Some(mut bloom) = bloom {
            bloom.intensity = systems::free_camera::bloom_intensity(space);
        }
    }

    // Above the atmosphere the sun is unfiltered: no air mass to redden or dim
    // it, and no local horizon to set behind. Attenuating it by the *anchor's*
    // ground-level extinction (`sun_light`) is what left the globe a dim red disc
    // at 18:00 and unlit at all at night, when what the view should show is a
    // fully sunlit day side and a dark night side, terminator between. Crossfaded
    // rather than switched, or climbing out at dusk changed the light in a step.
    let (ground_color, ground_lux) =
        sun_light(dev.time_of_day, dev.sun_declination_rad, dev.fog_density);
    let mut sun_color = Color::from(LinearRgba::from_vec4(
        LinearRgba::from(ground_color)
            .to_vec4()
            .lerp(LinearRgba::WHITE.to_vec4(), space),
    ));
    let mut illuminance = ground_lux.lerp(RAW_SUNLIGHT * SUN_SCALE, space);
    let (beam_factor, sky_fill) = overcast_lighting(overcast);
    if overcast > 0.0 {
        // Cloud replaces the direct beam with sky (`overcast_lighting`) and greys
        // what is left of its colour toward the sky's.
        illuminance *= beam_factor;
        let c = LinearRgba::from(sun_color).to_vec4();
        let lum = c.x * 0.2126 + c.y * 0.7152 + c.z * 0.0722;
        sun_color = Color::from(LinearRgba::from_vec4(
            c.lerp(Vec4::new(lum, lum, lum, 1.0), 0.6 * overcast),
        ));
    }
    for (mut tf, mut light) in sun.iter_mut() {
        *tf = Transform::from_translation(Vec3::ZERO).looking_to(-sun_dir, Vec3::Y);
        light.color = sun_color;
        light.illuminance = illuminance;
    }

    // Moonlight tracks the moon disc; fog diffuses it like it does the sun.
    let moon_dir = utils::sun::moon_direction(dev.time_of_day, dev.moon_age_days);
    let moonlight = MOONLIGHT_ILLUMINANCE
        * moonlight_factor(dev.time_of_day, dev.moon_age_days, dev.sun_declination_rad)
        * (1.0 - 0.75 * dev.fog_density);
    for (mut tf, mut light) in moon.iter_mut() {
        *tf = Transform::from_translation(Vec3::ZERO).looking_to(-moon_dir, Vec3::Y);
        light.illuminance = moonlight;
        // NB: don't toggle `shadow_maps_enabled` at runtime — Bevy 0.19's bevy_light
        // panics (cascade-visibility index out of bounds) when a directional light's
        // shadows are switched on/off after spawn. Moon shadows stay enabled; the
        // near-zero daytime illuminance makes them invisible anyway.
    }

    if let Ok(mut a) = ambient.single_mut() {
        // Space has no sky to scatter light back into the shadowed side, so the
        // ambient falls off toward Earthshine scale — just enough that the night
        // side reads as unlit land rather than a hole cut out of the starfield.
        let ground = ambient_light(
            dev.time_of_day,
            dev.moon_age_days,
            dev.sun_declination_rad,
            sky_ibl,
        );
        let mut ground_rgb = LinearRgba::from(ground.color).to_vec4();
        // Overcast fills shadows: the deck *is* the light source now, so what the
        // beam lost above arrives here instead — flat, shadowless and cool rather
        // than sunlit-and-shadowed. Gated on daylight so a cloudy night is not lit
        // by a sky that has nothing to scatter.
        let ground_brightness = ground.brightness + sky_fill * day;
        if overcast > 0.0 {
            let lum = ground_rgb.x * 0.2126 + ground_rgb.y * 0.7152 + ground_rgb.z * 0.0722;
            ground_rgb = ground_rgb.lerp(Vec4::new(lum, lum, lum, 1.0), 0.5 * overcast);
        }
        let space_rgb = LinearRgba::rgb(0.55, 0.62, 0.85).to_vec4();
        *a = AmbientLight {
            color: Color::from(LinearRgba::from_vec4(ground_rgb.lerp(space_rgb, space))),
            brightness: ground_brightness.lerp(SPACE_AMBIENT_BRIGHTNESS, space),
            affects_lightmapped_meshes: true,
        };
    }
}

/// Change in `GlobeState::space_factor` worth re-applying the lighting for. The
/// terms it drives are all smooth, so a coarse step is invisible — and it keeps a
/// slow climb from rewriting the sun light (and its shadow cascades) every frame.
pub(crate) const SPACE_RAMP_STEP: f32 = 0.01;

/// Drives the camera's atmospheric [`DistanceFog`] from the dev panel's fog slider,
/// the time of day (fog darkens at night), and the camera altitude — the fog thins
/// as you climb (see `fog_altitude_factor`) so a high-flight or orbit view looks
/// down through clear air rather than a grey wall. Runs each frame because the
/// altitude term is continuous.
pub(crate) fn apply_distance_fog(
    dev: Res<DevPanelState>,
    globe: Res<systems::globe::GlobeState>,
    mut camera: Query<
        (&GridCell, &Transform, &mut DistanceFog),
        With<crate::components::FreeCamera>,
    >,
    // The (density, time, declination) this last wrote. `DistanceFog` is not
    // `PartialEq`, so the inputs are what gets compared — the same shape
    // `water::update_water_waves` uses for the sea state.
    mut applied: Local<Option<(f32, f32, f32)>>,
) {
    let Ok((cell, tf, mut fog)) = camera.single_mut() else {
        return;
    };
    // The atmosphere itself stays a clear sky: boosting Nishita's Mie scattering
    // with density was tried and abandoned — single scattering *darkens* the low
    // sky instead of whitening it. The horizon-haze shell fogs the sky instead
    // (see update_horizon_haze), veiling the whole dome as density rises.
    let altitude = globe.altitude_m(cell, tf) as f32;
    let density = dev.fog_density * systems::free_camera::fog_altitude_factor(altitude);

    // Assigning through the `Mut` marks `DistanceFog` changed, which re-extracts
    // and re-uploads the camera's fog uniform. This ran unconditionally, so that
    // happened on every frame of the run for three inputs that move slowly at
    // most: the dev panel's density, the clock, and the sun's declination. Only
    // `density` moves continuously, and only while the camera is changing
    // altitude — so on a still frame this now writes nothing at all.
    let next = (density, dev.time_of_day, dev.sun_declination_rad);
    // Epsilons rather than exact equality: `density` is a product of a float
    // altitude factor, so it dithers in its last bits while hovering, and an exact
    // test would call that a change every frame and buy nothing.
    const FOG_EPS: f32 = 1e-4;
    if applied.is_some_and(|(d, t, s)| {
        (d - next.0).abs() < FOG_EPS && (t - next.1).abs() < FOG_EPS && (s - next.2).abs() < FOG_EPS
    }) {
        return;
    }
    *applied = Some(next);
    *fog = systems::free_camera::distance_fog(density, dev.time_of_day, dev.sun_declination_rad);
}

/// Spawns the star-map night sky: a box centred on the viewer whose custom material
/// samples the equirectangular star map by view direction (no UV-sphere poles or seams).
/// Fade the emissive building windows in as the sun drops below the horizon, so the
/// city lights up at night (and windows read as dark glass by day).
pub(crate) fn update_window_glow(
    dev: Res<DevPanelState>,
    windows: Query<(
        &systems::osm_buildings::WindowGlow,
        &MeshMaterial3d<StandardMaterial>,
    )>,
    // Windows stream in continuously as you move; newly-added ones need their glow
    // set even on frames where the time of day hasn't changed.
    new_windows: Query<
        (
            &systems::osm_buildings::WindowGlow,
            &MeshMaterial3d<StandardMaterial>,
        ),
        Added<systems::osm_buildings::WindowGlow>,
    >,
    // **The far lane's windows are a second material type**, because that lane's
    // buildings carry the height ramp that takes the outermost ring down into the
    // map and a window has to travel with its wall (`systems::building_fade`).
    // Same buckets, same ramp; the only difference is which `Assets` the handle
    // lives in, so the *glow* is described once below and applied twice.
    far_windows: Query<(
        &systems::osm_buildings::WindowGlow,
        &MeshMaterial3d<crate::systems::building_fade::FarBuildingMaterial>,
    )>,
    new_far_windows: Query<
        (
            &systems::osm_buildings::WindowGlow,
            &MeshMaterial3d<crate::systems::building_fade::FarBuildingMaterial>,
        ),
        Added<systems::osm_buildings::WindowGlow>,
    >,
    mut materials: ResMut<Assets<StandardMaterial>>,
    mut fade_materials: ResMut<Assets<crate::systems::building_fade::FarBuildingMaterial>>,
) {
    let elevation = sun_direction(dev.time_of_day, dev.sun_declination_rad).y;
    // Begin glowing as the sun lowers in the evening (elevation ~0.18), reaching
    // full a little after dusk — a gentle ramp, not an on/off at full dark.
    let glow = ((0.18 - elevation) / 0.33).clamp(0.0, 1.0);
    // Subtle, warm, per-bucket. One place, so two lanes drawing one city cannot
    // drift into lighting it two different colours.
    let emissive = |bucket: &systems::osm_buildings::WindowGlow| {
        let e = 1.2 * glow * bucket.0;
        LinearRgba::rgb(e, 0.72 * e, 0.34 * e)
    };
    let mut apply = |bucket: &systems::osm_buildings::WindowGlow,
                     mat: &MeshMaterial3d<StandardMaterial>| {
        if let Some(mut m) = materials.get_mut(&mat.0) {
            m.emissive = emissive(bucket);
        }
    };
    let mut apply_far =
        |bucket: &systems::osm_buildings::WindowGlow,
         mat: &MeshMaterial3d<crate::systems::building_fade::FarBuildingMaterial>| {
            if let Some(mut m) = fade_materials.get_mut(&mat.0) {
                m.base.emissive = emissive(bucket);
            }
        };
    // Only rewrite every window's emissive when the time of day actually changed;
    // otherwise just catch up newly-streamed windows.
    if dev.is_changed() {
        for (bucket, mat) in windows.iter() {
            apply(bucket, mat);
        }
        for (bucket, mat) in far_windows.iter() {
            apply_far(bucket, mat);
        }
    } else {
        for (bucket, mat) in new_windows.iter() {
            apply(bucket, mat);
        }
        for (bucket, mat) in new_far_windows.iter() {
            apply_far(bucket, mat);
        }
    }
}

/// Builds the star field from the embedded bright-star catalog (~9k naked-eye
/// stars, HYG database — see `tools/gen_stars.py`): one tiny sphere-tangent quad
/// per star at its real equatorial position, weighted by magnitude and tinted by
/// B−V color temperature, with a per-star twinkle seed in the vertex alpha. The
/// whole sky is one mesh and one draw call; `update_star_sky` spins it around the
/// celestial pole, fades it with the night, and drives the twinkle clock.
pub(crate) fn setup_stars(
    mut commands: Commands,
    mut meshes: ResMut<Assets<Mesh>>,
    mut materials: ResMut<Assets<StarFieldMaterial>>,
) {
    static CATALOG: &[u8] = include_bytes!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../assets/sky/bright_stars.bin"
    ));
    let n = CATALOG.len() / 16;
    let mut positions: Vec<[f32; 3]> = Vec::with_capacity(n * 4);
    let mut normals: Vec<[f32; 3]> = Vec::with_capacity(n * 4);
    let mut uvs: Vec<[f32; 2]> = Vec::with_capacity(n * 4);
    let mut colors: Vec<[f32; 4]> = Vec::with_capacity(n * 4);
    let mut indices: Vec<u32> = Vec::with_capacity(n * 6);

    for (i, rec) in CATALOG.chunks_exact(16).enumerate() {
        let f = |k: usize| f32::from_le_bytes(rec[4 * k..4 * k + 4].try_into().unwrap());
        let (ra, dec, mag, bv) = (f(0), f(1), f(2), f(3));

        // Equatorial frame with +Y at the north celestial pole. RA winds so that a
        // larger right ascension culminates *later* under the westward diurnal spin,
        // which keeps constellations un-mirrored for a ground observer.
        let dir = Vec3::new(dec.cos() * ra.cos(), dec.sin(), -(dec.cos() * ra.sin()));
        // Tangent basis for the quad (X fallback for stars at the poles).
        let up = if dir.y.abs() > 0.99 { Vec3::X } else { Vec3::Y };
        let t1 = up.cross(dir).normalize();
        let t2 = dir.cross(t1);

        // Compressed magnitude scale: real flux spans ~1500:1 from Sirius to the
        // naked-eye limit, far more than additive blending can usefully show.
        let lum = 10f32.powf(-0.24 * mag).min(2.5);
        // Bright stars also get a slightly larger quad (their glow bleeds wider).
        let half_angle = 0.0010 + 0.0009 * ((2.5 - mag) / 4.0).clamp(0.0, 1.0);
        let tint = star_color(bv) * lum;
        let seed = (i as f32 * 0.618_034).fract(); // per-star twinkle phase

        let base = positions.len() as u32;
        for (su, sv) in [(-1.0, -1.0), (1.0, -1.0), (1.0, 1.0), (-1.0, 1.0)] {
            let p = (dir + (t1 * su + t2 * sv) * half_angle) * STAR_SHELL_RADIUS;
            positions.push([p.x, p.y, p.z]);
            normals.push([dir.x, dir.y, dir.z]);
            uvs.push([su, sv]);
            colors.push([tint.x, tint.y, tint.z, seed]);
        }
        indices.extend_from_slice(&[base, base + 1, base + 2, base, base + 2, base + 3]);
    }

    let mut mesh = Mesh::new(
        bevy::render::mesh::PrimitiveTopology::TriangleList,
        bevy::asset::RenderAssetUsages::RENDER_WORLD,
    );
    mesh.insert_attribute(Mesh::ATTRIBUTE_POSITION, positions);
    mesh.insert_attribute(Mesh::ATTRIBUTE_NORMAL, normals);
    mesh.insert_attribute(Mesh::ATTRIBUTE_UV_0, uvs);
    mesh.insert_attribute(Mesh::ATTRIBUTE_COLOR, colors);
    mesh.insert_indices(bevy::render::mesh::Indices::U32(indices));

    let material = materials.add(StarFieldMaterial {
        params: Vec4::ZERO, // brightness set each frame; starts invisible (day)
    });
    commands.spawn((
        Mesh3d(meshes.add(mesh)),
        MeshMaterial3d(material),
        Transform::default(),
        bevy::light::NotShadowCaster,
        bevy::light::NotShadowReceiver,
        StarSky,
    ));
}

/// Approximate display tint for a stellar B−V color index: blue-white for hot
/// stars (Rigel, −0.3) through white and yellow to orange-red for cool ones
/// (Betelgeuse, ~1.85). Piecewise-linear over perceptual anchor stops.
pub(crate) fn star_color(bv: f32) -> Vec3 {
    const STOPS: [(f32, Vec3); 6] = [
        (-0.3, Vec3::new(0.62, 0.72, 1.0)),
        (0.0, Vec3::new(0.79, 0.86, 1.0)),
        (0.4, Vec3::new(1.0, 0.98, 0.92)),
        (0.8, Vec3::new(1.0, 0.90, 0.76)),
        (1.5, Vec3::new(1.0, 0.80, 0.60)),
        (2.0, Vec3::new(1.0, 0.70, 0.46)),
    ];
    let bv = bv.clamp(STOPS[0].0, STOPS[STOPS.len() - 1].0);
    for w in STOPS.windows(2) {
        let ((a, ca), (b, cb)) = (w[0], w[1]);
        if bv <= b {
            return ca.lerp(cb, (bv - a) / (b - a));
        }
    }
    STOPS[STOPS.len() - 1].1
}

/// Spawns the fog-colored horizon-haze shell: a slightly smaller sibling of the star
/// box whose shader paints a band of fog color at the horizon (see horizonhaze.wgsl).
pub(crate) fn setup_horizon_haze(
    mut commands: Commands,
    mut meshes: ResMut<Assets<Mesh>>,
    mut materials: ResMut<Assets<HorizonHazeMaterial>>,
) {
    // Startup switch, alongside CARTO_NO_ATMOSPHERE: leave the haze shell out
    // entirely, to tell "the scene is dark" from "a sky shell is covering it"
    // when bisecting a render problem with `--shot`.
    if env_flag("CARTO_NO_HAZE") {
        return;
    }
    // Unit shell: `update_horizon_haze` scales it to the fog's saturation
    // distance each frame, so it stands *in front of* the ground the fog has
    // already hidden rather than behind it (see `fog_saturation_distance`).
    //
    // A sphere, not a box: the rim where the shell stops covering the ground is
    // the one edge the haze can't hide, so it has to sit at the same elevation in
    // every direction. A cube's rim runs from `r` at the face to `r√2` at the
    // corner, which swept up and down as a ring as the camera pivoted.
    // `ico(3)` — 1,280 triangles, not `ico(5)`'s 20,480. `horizonhaze.wgsl` samples
    // by per-pixel view direction and says so in its own header ("the mesh shape is
    // irrelevant — it's just a cheap shell around the camera"), so the subdivision
    // buys no shading quality whatsoever; it only decides how round the *rim* is,
    // which is the one thing the paragraph above cares about.
    //
    // Not `ico(2)`, and the difference is the rim rather than the silhouette. A
    // geodesic sphere's radius dips between its vertices by `1 - cos θ`, where θ is
    // the angular radius of a face: ~6.4° at `ico(2)` (0.6 %) against ~3.2° at
    // `ico(3)` (0.15 %). Both are nothing next to the cube's 41 % that the rejected
    // option above was rejected for, but the rim is a ring the eye tracks as the
    // camera pivots — the exact artefact that argument is about — so this takes the
    // 16× and leaves the last 4× on the table rather than spending the rim on it.
    let mesh = meshes.add(Sphere::new(1.0).mesh().ico(3).unwrap());
    let material = materials.add(HorizonHazeMaterial {
        color: LinearRgba::WHITE,
        params: Vec4::ZERO, // set each frame from the distance fog
        dir_light_color: LinearRgba::NONE,
        rim: Vec4::ZERO,
    });
    commands.spawn((
        Mesh3d(mesh),
        MeshMaterial3d(material),
        Transform::default(),
        bevy::light::NotShadowCaster,
        bevy::light::NotShadowReceiver,
        HorizonHaze,
    ));
}

/// Keeps the horizon-haze shell on the camera and matches its color and strength to
/// the camera's `DistanceFog`, so fogged ground and sky meet seamlessly at the horizon.
pub(crate) fn update_horizon_haze(
    dev: Res<DevPanelState>,
    globe: Res<systems::globe::GlobeState>,
    camera: Query<
        (&GridCell, &GlobalTransform, &Transform, &DistanceFog),
        (With<Camera3d>, Without<HorizonHaze>),
    >,
    mut haze: Query<
        (
            &mut Transform,
            &mut Visibility,
            &MeshMaterial3d<HorizonHazeMaterial>,
        ),
        With<HorizonHaze>,
    >,
    mut materials: ResMut<Assets<HorizonHazeMaterial>>,
) {
    let Ok((cell, cam, cam_local, fog)) = camera.single() else {
        return;
    };
    let Ok((mut tf, mut vis, mat_handle)) = haze.single_mut() else {
        return;
    };
    tf.translation = cam.translation();

    // The haze thins with altitude alongside the ground fog, so an orbit view
    // sees the map through clear air instead of a grey band at the horizon.
    let altitude = globe.altitude_m(cell, cam_local) as f32;
    let alt = systems::free_camera::fog_altitude_factor(altitude);
    let d = dev.fog_density * alt;

    // Peak alpha is the fog's *own* opacity where the map ends, read off the same
    // `DistanceFog` the ground wears rather than approximated: the shell is that
    // fog wall continued past the last tile, and anything short of it draws a step
    // at the horizon. The old `0.35 * alt + 1.2 * d` only reached 1.0 at density
    // 0.54, so at the default 0.15 the ground went to full fog colour while the
    // sky above it got 53% — a visible grey line at every ordinary altitude.
    // In practice this is ~1.0 for any density worth calling fog (82 km of it
    // against 7–15 km of visibility) and exactly 0 when the fog is off.
    let peak = systems::free_camera::fog_factor_at(
        &fog.falloff,
        systems::free_camera::HORIZON_FOG_DISTANCE,
    );

    // Stand the shell just inside the distance where the fog has hidden the ground
    // completely, so it draws *over* the far map instead of behind it. Bevy's
    // atmosphere composites over geometry only (and its in-scattering is clamped
    // at 32 km while its extinction is not), so ground past that reads as a warm
    // band that no amount of fog colour behind it can cover — see
    // `fog_saturation_distance`. Capped at the ground disc so a thin, high-altitude
    // haze never pulls the shell in over map the fog has genuinely left visible.
    let radius =
        systems::free_camera::fog_saturation_distance(&fog.falloff).clamp(1_000.0, 150_000.0);
    tf.scale = Vec3::splat(radius);

    // Once it carries no haze at all, drop the shell out of the frame rather than
    // drawing a fully transparent shell over the whole view every frame.
    let want = if peak <= 0.0 {
        Visibility::Hidden
    } else {
        Visibility::Inherited
    };
    if *vis != want {
        *vis = want;
    }

    if let Some(mut mat) = materials.get_mut(&mat_handle.0) {
        mat.color = fog.color.to_linear();
        // Density-driven only: the built-in atmosphere (Bevy 0.19) has no
        // single-scattering horizon artifact to cover, so there's no base haze —
        // a base veil just tinted the (already correct) sky with the warm fog colour.
        // The zenith floor (w) grows quadratically so light haze leaves the sky
        // blue while dense fog closes over the whole dome.
        mat.params = Vec4::new(0.25 * d, peak, fog.directional_light_exponent, d * d);
        mat.dir_light_color = fog.directional_light_color.to_linear();
        // Where the shell's rim meets the ground, in view-direction `y`: the ray
        // that grazes the sphere at `radius` is the same one that hits the ground
        // plane there, so it is `-altitude / radius` (small, and always below the
        // horizon). The shader ramps the haze from nothing at the rim to full at
        // the horizon, which is what keeps the rim from being an edge of its own:
        // the ground just inside it wears the atmosphere's own aerial perspective,
        // and at sunset that is a strong warm tint for flat fog colour to butt up
        // against. Fading in over that span also means the haze only takes over
        // where the ground is far enough for the atmosphere to have got it wrong.
        mat.rim = Vec4::new(-(altitude / radius).clamp(1e-4, 1.0), 0.0, 0.0, 0.0);
    }
}

/// Keeps the star field centred on the camera (so it reads as infinitely far),
/// spins it around the celestial pole with the time of day, and drives the
/// material's brightness (night factor, veiled by fog) and twinkle clock.
#[allow(clippy::type_complexity)]
pub(crate) fn update_star_sky(
    dev: Res<DevPanelState>,
    time: Res<Time>,
    globe: Res<systems::globe::GlobeState>,
    camera: Query<&GlobalTransform, With<Camera3d>>,
    free_camera: Query<
        (&GridCell, &Transform),
        (With<crate::components::FreeCamera>, Without<StarSky>),
    >,
    mut star: Query<
        (
            &mut Transform,
            &mut Visibility,
            &MeshMaterial3d<StarFieldMaterial>,
        ),
        With<StarSky>,
    >,
    mut materials: ResMut<Assets<StarFieldMaterial>>,
) {
    let Ok(cam) = camera.single() else {
        return;
    };
    let Ok((mut tf, mut vis, mat_handle)) = star.single_mut() else {
        return;
    };
    // In space the shell moves out past the far limb so stars sit *behind* the
    // planet instead of blending over it (see `GlobeState::sky_shell_radius`).
    let space_shell = free_camera
        .single()
        .ok()
        .and_then(|(cell, cam_tf)| globe.sky_shell_radius(cell, cam_tf));
    tf.scale = Vec3::splat(space_shell.map_or(1.0, |r| r / STAR_SHELL_RADIUS));
    tf.translation = cam.translation();
    // Diurnal rotation of the celestial sphere (shared with the moon, so they
    // move together). Quick-cycle mode becomes a star-trail time-lapse.
    tf.rotation = utils::sun::celestial_rotation(dev.time_of_day);

    // Fade in once the sun is below the horizon; full when it's well below (dark sky).
    // Above the atmosphere there is no daylight to drown them: the stars are out
    // whatever the clock at the anchor says, and no fog can veil them. Without
    // this the space view was an unlit black void whenever it was daytime below.
    //
    // In space that factor also carries the exposure compensation: this tier
    // holds the *day* exposure while `STAR_BRIGHTNESS` was judged against the
    // ground's night one, so an uncompensated field is two stops under and
    // measures at 78 pixels out of 120,000 above luminance 32 — present, and
    // invisible on a screen. See `space_star_exposure_compensation`.
    let elevation = sun_direction(dev.time_of_day, dev.sun_declination_rad).y;
    let night = match space_shell {
        Some(_) => space_star_exposure_compensation(),
        None => ((-0.05 - elevation) / 0.2).clamp(0.0, 1.0),
    };
    // Fog is opaque to starlight long before it swallows the city: veil the star
    // field with density so a foggy night reads as murk, not crisp stars over murk.
    let fog_veil = match space_shell {
        Some(_) => 1.0,
        None => (-3.0 * dev.fog_density).exp(),
    };
    // Drop the field out of the frame entirely by day, rather than blending
    // ~8,900 additive quads at a brightness of zero. `bright_stars.bin` is 16
    // bytes a star, so that is ~17,800 triangles in the transparent phase — with
    // `AlphaMode::Add` and no depth write, so not one of their fragments is
    // rejected early — multiplied by nothing, for the whole daylight half of the
    // cycle. The clouds, the rain and the haze shell each already do exactly
    // this, each with its own note saying why; the star field and the moon were
    // simply missed when they grew their fades.
    let brightness = night * STAR_BRIGHTNESS * fog_veil;
    let want = if brightness <= 0.0 {
        Visibility::Hidden
    } else {
        Visibility::Inherited
    };
    if *vis != want {
        *vis = want;
    }
    if let Some(mut mat) = materials.get_mut(&mat_handle.0) {
        mat.params = Vec4::new(
            brightness,
            time.elapsed_secs(),
            // Space flag: drops the shader's horizon cut, air extinction and
            // twinkle, none of which exist above the atmosphere.
            space_shell.is_some() as u32 as f32,
            0.0,
        );
    }
}

/// Marker for the procedural cloud-ceiling shell (see `shaders/clouds.wgsl`).
#[derive(Component)]
pub(crate) struct Clouds;

/// Height of the low cloud layer above the camera, in metres — a mid-level deck.
pub(crate) const CLOUD_LAYER_HEIGHT: f32 = 2_500.0;
/// Height of the high (cirrus) deck. The gap between the two is what produces
/// parallax as the camera translates — see the shader's header.
pub(crate) const CLOUD_HIGH_LAYER_HEIGHT: f32 = 7_000.0;
/// Vertical extent of the low deck, in metres. The sun march traverses it, so it
/// sets how far a shadow is thrown across the field at a given sun elevation.
pub(crate) const CLOUD_DECK_THICKNESS: f32 = 900.0;
/// Overall cloud opacity before the coverage/altitude/space fades.
pub(crate) const CLOUD_OPACITY: f32 = 0.92;
/// How prominent the high cirrus deck is relative to the low one.
pub(crate) const CLOUD_HIGH_OPACITY: f32 = 0.55;
/// Edge softness of the coverage cutoff (0 = hard, higher = wispier margins).
///
/// Widened from 0.18 when the alpha curve became Beer's law: at an optical depth
/// of 7 a column is 60 % opaque a quarter of the way up the coverage ramp, where
/// the old `smoothstep().powf(0.75)` needed half of it, so the *visible* edge band
/// halved. Roughly doubling the ramp restores it.
pub(crate) const CLOUD_SOFTNESS: f32 = 0.30;
/// Wind velocity in metres/second (drifts the deck). Faster than any real wind —
/// it is tuned so the field visibly crosses the sky in well under a minute rather
/// than to be meteorologically honest. (`systems::water` reuses the *direction*
/// so the waves travel with the clouds; only that is load-bearing there.)
pub const CLOUD_WIND: Vec2 = Vec2::new(118.0, 44.0);
/// Gain on the sunlit scattering term. The scattering model is not
/// energy-conserving, so this is the one knob that sets the deck's absolute
/// brightness; the *shape* of the lighting comes from the shader.
pub(crate) const CLOUD_SUN_GAIN: f32 = 1.0;
/// Gain on the ambient sky term — how much the shadowed side is filled in.
pub(crate) const CLOUD_AMBIENT_GAIN: f32 = 1.0;
/// Floor under the deck's ambient colour, for the hours when the sky it is taken
/// from has gone dark. See the argument at the `sky_rgb` it clamps.
pub(crate) const CLOUD_NIGHT_FLOOR: Vec3 = Vec3::new(0.045, 0.045, 0.055);
/// Fraction of the ambient sky an opaque cloud core still receives, at the two
/// ends of the coverage range — the shader lerps between them.
///
/// One number could not do this job. A lone cumulus on a blue sky has a *dark*
/// base: it is optically deep, lit from above, and standing over ground that
/// bounces very little back. A solid overcast has a bright one: the deck is
/// thinner, and it is the sky — it has no darker sky beside it to be dark
/// against. The old single 0.35 was the cumulus answer applied to both, which is
/// how a fully overcast day came out as a grey ceiling with the horizon glowing
/// white underneath it. At full cover the base now lands on the haze colour it
/// meets at the horizon, give or take the 15 % of modulation left so the deck
/// still has some structure in it rather than being one flat field of grey.
pub(crate) const CLOUD_SKY_FLOOR_SCATTERED: f32 = 0.35;
pub(crate) const CLOUD_SKY_FLOOR_OVERCAST: f32 = 0.85;
/// Clouds are at full strength up to this camera altitude, then fade out so a
/// high flight or orbit view sees clear sky (the shell is a ceiling seen from
/// below; flying *above* the deck to look down on the tops is a follow-up).
pub(crate) const CLOUD_FADE_LOW: f32 = 2_000.0;
/// Camera altitude past which the cloud deck has fully faded out.
pub(crate) const CLOUD_FADE_HIGH: f32 = 9_000.0;

/// Camera-centred procedural cloud ceiling, alpha-blended over the sky. Coverage
/// is driven by the dev panel (`DevPanelState::cloud_cover`); the sun and sky
/// colours are the *same* day/night model the sunlight and the fog are built
/// from, so the deck can't drift off the sky's palette (see `update_clouds`).
#[derive(Asset, TypePath, AsBindGroup, Clone)]
pub(crate) struct CloudMaterial {
    /// x: coverage 0–1, y: softness, z: time (wind clock), w: low deck height (m).
    #[uniform(0)]
    params0: Vec4,
    /// xy: wind velocity (m/s), z: space fade (1 = hidden), w: altitude fade.
    #[uniform(1)]
    params1: Vec4,
    /// xyz: world direction to the sun. w: deck thickness (m), for the sun march.
    #[uniform(2)]
    sun_dir: Vec4,
    /// Sunlight reaching the deck's altitude; alpha carries the overall opacity.
    #[uniform(3)]
    sun_color: LinearRgba,
    /// Ambient sky light on the deck; alpha scales the high deck's opacity.
    #[uniform(4)]
    sky_color: LinearRgba,
    /// xy: world-anchor offset in metres (camera displacement from the anchor), so
    /// the deck stays fixed over the ground. z: the camera fog's per-metre density
    /// (aerial perspective). w: high deck height (m).
    #[uniform(5)]
    params2: Vec4,
    /// The colour the deck recedes into with distance — the camera's fog colour,
    /// so cloud and sky reach the horizon together.
    #[uniform(6)]
    haze_color: LinearRgba,
}

impl Material for CloudMaterial {
    fn fragment_shader() -> ShaderRef {
        "shaders/clouds.wgsl".into()
    }

    fn alpha_mode(&self) -> AlphaMode {
        AlphaMode::Blend
    }

    fn specialize(
        _pipeline: &MaterialPipeline,
        descriptor: &mut RenderPipelineDescriptor,
        _layout: &MeshVertexBufferLayoutRef,
        _key: MaterialPipelineKey<Self>,
    ) -> Result<(), SpecializedMeshPipelineError> {
        // Camera is inside the shell; draw its interior and don't occlude the
        // world behind it (depth test still lets geometry cover the clouds).
        descriptor.primitive.cull_mode = None;
        if let Some(depth) = descriptor.depth_stencil.as_mut() {
            depth.depth_write_enabled = Some(false);
        }
        // The phone's cheaper cloud variant. A shader def rather than a uniform
        // branch, so the desktop program is byte-identical to what it was and the
        // handset's is genuinely shorter — a dynamic branch would still cost the
        // register pressure of the octaves it skips.
        //
        // Resolved from the tier here rather than passed through the material,
        // because it cannot change while the process runs and one extra pipeline
        // variant is exactly what specialization is for.
        if quality::is_phone() {
            if let Some(fragment) = descriptor.fragment.as_mut() {
                fragment.shader_defs.push("CLOUDS_LOW".into());
            }
        }
        Ok(())
    }
}

/// Spawns the cloud shell (mirrors `setup_horizon_haze`). `CARTO_NO_CLOUDS`
/// leaves it out for bisecting a render problem with `--shot`.
pub(crate) fn setup_clouds(
    mut commands: Commands,
    mut meshes: ResMut<Assets<Mesh>>,
    mut materials: ResMut<Assets<CloudMaterial>>,
) {
    if env_flag("CARTO_NO_CLOUDS") {
        return;
    }
    let mesh = meshes.add(Cuboid::from_length(360_000.0).mesh());
    let material = materials.add(CloudMaterial {
        params0: Vec4::ZERO, // set each frame from the dev panel
        params1: Vec4::ZERO,
        sun_dir: Vec4::Y,
        sun_color: LinearRgba::WHITE,
        sky_color: LinearRgba::BLACK,
        params2: Vec4::ZERO,
        haze_color: LinearRgba::BLACK,
    });
    commands.spawn((
        Mesh3d(mesh),
        MeshMaterial3d(material),
        Transform::default(),
        bevy::light::NotShadowCaster,
        bevy::light::NotShadowReceiver,
        Clouds,
    ));
}

/// Keeps the cloud shell centred on the camera, drives coverage from the dev
/// panel and the lighting from the day/night cycle, and fades the deck out with
/// altitude (high flight / orbit) and the space tier.
///
/// The two light colours are the load-bearing part: a cloud's colour has to be an
/// *output of the sky model* rather than a palette of its own
/// (`docs/notes/cloud-lighting.md`). So the sun colour is `utils::sun`'s
/// transmittance — the very function the sun light and the fog colour are built
/// from — and the haze colour is read straight off the camera's own
/// `DistanceFog`, which is what the horizon shell paints. Hand-authored
/// lit/shadow constants used to live here, and they drifted.
#[allow(clippy::type_complexity)]
pub(crate) fn update_clouds(
    dev: Res<DevPanelState>,
    time: Res<Time>,
    globe: Res<systems::globe::GlobeState>,
    camera: Query<
        (&GridCell, &GlobalTransform, &Transform, &DistanceFog),
        (With<Camera3d>, Without<Clouds>),
    >,
    mut clouds: Query<
        (
            &mut Transform,
            &mut Visibility,
            &MeshMaterial3d<CloudMaterial>,
        ),
        With<Clouds>,
    >,
    mut materials: ResMut<Assets<CloudMaterial>>,
    mut clouds_disabled: Local<Option<bool>>,
) {
    let Ok((cell, cam, cam_local, fog)) = camera.single() else {
        return;
    };
    let Ok((mut tf, mut vis, mat_handle)) = clouds.single_mut() else {
        return;
    };
    tf.translation = cam.translation();

    let altitude = globe.altitude_m(cell, cam_local) as f32;
    let alt_fade =
        (1.0 - ((altitude - CLOUD_FADE_LOW) / (CLOUD_FADE_HIGH - CLOUD_FADE_LOW))).clamp(0.0, 1.0);
    let space = globe.space_factor(cell, cam_local);

    // Drop the shell out of the frame entirely once it carries no cloud, rather
    // than blending a fully transparent 360 km box over the whole view.
    // `CARTO_NO_CLOUDS` covers both tiers, or the switch would only half mean what
    // it says — this shell owns the ground and `GlobeClouds` owns the space tier.
    // Frame rate never hides it: the clouds are the user's setting (see the
    // globe shell's note on the removed `adaptive_quality`).
    let disabled = *clouds_disabled.get_or_insert_with(|| crate::env_flag("CARTO_NO_CLOUDS"))
        || !dev.clouds_enabled;
    let want = if disabled || dev.cloud_cover <= 0.0 || alt_fade <= 0.0 || space >= 1.0 {
        Visibility::Hidden
    } else {
        Visibility::Inherited
    };
    if *vis != want {
        *vis = want;
    }

    let hours = dev.time_of_day;
    let decl = dev.sun_declination_rad;

    // Sunlight *at deck altitude* — less air above the deck than above the city,
    // so it is brighter and less reddened than the beam the ground gets. That
    // difference is what makes a sunset deck read as lit from above rather than
    // as a grey lid, and it is why the sun colour can't just be reused from
    // `sun_light`. Faded out through sunrise/sunset by the disc's own occlusion,
    // so the scattering term goes to nothing rather than snapping off.
    let beam = utils::sun::sun_transmittance_at(hours, decl, CLOUD_LAYER_HEIGHT);
    let sun_rgb = beam * utils::sun::sun_disc(hours, decl) * CLOUD_SUN_GAIN;

    // Ambient sky light filling the shadowed side — and, at full cover, the deck's
    // whole colour.
    //
    // It **is** the camera's fog colour now, not a second hand-authored palette
    // built "in the same shape" as it. The two were meant to agree and did not:
    // the fog colour is ~0.5 linear by day and the deck's base worked out at ~0.15,
    // so an overcast sky rendered as a mid-grey lid sitting over a near-white
    // horizon band, with a hard step between them — the single ugliest thing in
    // the Fairphone screenshots. Both are written into the same post-exposure
    // linear space (bevy's `apply_fog` mixes toward `DistanceFog::color` after
    // exposure, and `horizonhaze.wgsl` paints it un-exposed to match), so this is
    // not a units mismatch to be scaled away — they were simply two different
    // greys, and taking the one already on the camera makes them one.
    //
    // What survives from the old code is the *floor*: a cloud base over a city
    // catches its glow from below, so an overcast night sky must read as a dim lid
    // rather than as a void. Matching the fog's night colour exactly would make
    // the deck darker than the sky behind it and turn it into black holes in the
    // star field — the one place cloud and fog should not agree, and now the only
    // one.
    let haze = fog.color.to_linear().to_vec3();
    let sky_rgb = haze.max(CLOUD_NIGHT_FLOOR) * CLOUD_AMBIENT_GAIN;

    if let Some(mut mat) = materials.get_mut(&mat_handle.0) {
        mat.params0 = Vec4::new(
            dev.cloud_cover,
            CLOUD_SOFTNESS,
            time.elapsed_secs(),
            CLOUD_LAYER_HEIGHT,
        );
        mat.params1 = Vec4::new(CLOUD_WIND.x, CLOUD_WIND.y, space, alt_fade);
        mat.sun_dir = sun_direction(hours, decl).extend(CLOUD_DECK_THICKNESS);
        mat.sun_color = LinearRgba::new(sun_rgb.x, sun_rgb.y, sun_rgb.z, CLOUD_OPACITY);
        mat.sky_color = LinearRgba::new(sky_rgb.x, sky_rgb.y, sky_rgb.z, CLOUD_HIGH_OPACITY);
        // The haze colour's alpha was documented "unused" and is the deck's sky
        // floor — a Rust-side decision precisely because it is a *lighting* one
        // (`cloud_sky_floor`), and the shader is where shape lives.
        mat.haze_color = fog
            .color
            .to_linear()
            .with_alpha(cloud_sky_floor(dev.cloud_cover));
        // World-anchor: the camera's horizontal position in the local anchor
        // frame, in metres, so the deck stays fixed over the ground as you fly
        // under it rather than sliding with the camera. Metres, not the shader's
        // UV units, because the two decks have different frequencies — and it
        // retires the `CLOUD_FREQ` constant that existed only to mirror the
        // shader's, one duplicated number less to keep in step.
        let edge = crate::grid::WORLD_CELL_EDGE as f32;
        let world_x = cell.x as f32 * edge + cam_local.translation.x;
        let world_z = cell.z as f32 * edge + cam_local.translation.z;
        mat.params2 = Vec4::new(
            world_x,
            world_z,
            fog_density_coefficient(&fog.falloff),
            CLOUD_HIGH_LAYER_HEIGHT,
        );
    }
}

/// How much of the ambient sky an opaque cloud core keeps, for a coverage.
///
/// Ramped over the *top* of the coverage range rather than linearly across it:
/// "overcast" is a state the sky arrives at near the end, and a lone cumulus at
/// 30 % cover should look no different from one at 10 %.
pub(crate) fn cloud_sky_floor(cover: f32) -> f32 {
    CLOUD_SKY_FLOOR_SCATTERED.lerp(
        CLOUD_SKY_FLOOR_OVERCAST,
        utils::sun::smoothstep(0.55, 1.0, cover),
    )
}

/// Per-metre density of a [`FogFalloff`], for the cloud deck's aerial perspective
/// — 0 whenever the fog is off.
///
/// `free_camera::distance_fog` only ever builds `ExponentialSquared` (fog on) or
/// `Linear` at infinity (fog off), and the shader hard-codes the squared curve to
/// match. `Exponential` is mapped anyway so a future falloff change degrades to
/// "slightly wrong haze" rather than to no haze at all.
pub(crate) fn fog_density_coefficient(falloff: &FogFalloff) -> f32 {
    match falloff {
        FogFalloff::ExponentialSquared { density } => *density,
        FogFalloff::Exponential { density } => *density,
        _ => 0.0,
    }
}

/// Marker for the screen-space rain quad (see `shaders/rain.wgsl`).
#[derive(Component)]
pub(crate) struct Rain;

/// Wind slant of the rain streaks (screen-space shear).
pub(crate) const RAIN_SLANT: f32 = 0.12;
/// Overall opacity of the rain streaks.
pub(crate) const RAIN_OPACITY: f32 = 0.5;
/// Rain is full-strength up to this altitude, then fades (you climb out of it).
pub(crate) const RAIN_FADE_LOW: f32 = 1_500.0;
/// Camera altitude past which the rain has fully faded out.
pub(crate) const RAIN_FADE_HIGH: f32 = 6_000.0;

/// Screen-space rain: an animated streak overlay on a small camera-centred shell
/// (`update_rain` keeps it on the camera). The shell's interior sits ~5 m out —
/// nearer than any world geometry — so with depth-write off the streaks draw over
/// the whole scene; the egui HUD still draws in its own later pass. The pattern is
/// keyed off the framebuffer coordinate, so the mesh shape only has to cover view.
#[derive(Asset, TypePath, AsBindGroup, Clone)]
pub(crate) struct RainMaterial {
    /// x: intensity 0–1, y: time (fall clock), z: wind slant, w unused.
    #[uniform(0)]
    params: Vec4,
    /// Streak colour; alpha carries the overall opacity.
    #[uniform(1)]
    tint: LinearRgba,
}

impl Material for RainMaterial {
    fn fragment_shader() -> ShaderRef {
        "shaders/rain.wgsl".into()
    }

    fn alpha_mode(&self) -> AlphaMode {
        AlphaMode::Blend
    }

    fn specialize(
        _pipeline: &MaterialPipeline,
        descriptor: &mut RenderPipelineDescriptor,
        _layout: &MeshVertexBufferLayoutRef,
        _key: MaterialPipelineKey<Self>,
    ) -> Result<(), SpecializedMeshPipelineError> {
        descriptor.primitive.cull_mode = None;
        if let Some(depth) = descriptor.depth_stencil.as_mut() {
            depth.depth_write_enabled = Some(false);
        }
        Ok(())
    }
}

/// Spawns the rain overlay quad. `CARTO_NO_RAIN` leaves it out for `--shot`
/// bisecting.
pub(crate) fn setup_rain(
    mut commands: Commands,
    mut meshes: ResMut<Assets<Mesh>>,
    mut materials: ResMut<Assets<RainMaterial>>,
) {
    if env_flag("CARTO_NO_RAIN") {
        return;
    }
    let mesh = meshes.add(Cuboid::from_length(10.0).mesh());
    let material = materials.add(RainMaterial {
        params: Vec4::ZERO,
        tint: LinearRgba::rgb(0.72, 0.78, 0.88),
    });
    commands.spawn((
        Mesh3d(mesh),
        MeshMaterial3d(material),
        Transform::default(),
        bevy::light::NotShadowCaster,
        bevy::light::NotShadowReceiver,
        Rain,
    ));
}

/// Keeps the rain shell centred on the camera, drives intensity from the dev
/// panel's precipitation slider, and fades it out with altitude.
#[allow(clippy::type_complexity)]
pub(crate) fn update_rain(
    dev: Res<DevPanelState>,
    time: Res<Time>,
    globe: Res<systems::globe::GlobeState>,
    camera: Query<(&GridCell, &GlobalTransform, &Transform), (With<Camera3d>, Without<Rain>)>,
    mut rain: Query<
        (
            &mut Transform,
            &mut Visibility,
            &MeshMaterial3d<RainMaterial>,
        ),
        With<Rain>,
    >,
    mut materials: ResMut<Assets<RainMaterial>>,
) {
    let Ok((cell, cam, cam_local)) = camera.single() else {
        return;
    };
    let Ok((mut tf, mut vis, mat_handle)) = rain.single_mut() else {
        return;
    };

    let altitude = globe.altitude_m(cell, cam_local) as f32;
    let alt_fade =
        (1.0 - ((altitude - RAIN_FADE_LOW) / (RAIN_FADE_HIGH - RAIN_FADE_LOW))).clamp(0.0, 1.0);
    let intensity = dev.precipitation * alt_fade;

    // Drop the shell out of the frame when it carries no rain, the same way
    // `update_clouds` does with its own — dry weather is the common case (and
    // every scripted `--shot` run), and the shader is not cheap to run for a
    // guaranteed-zero result: it is an alpha-blended box drawn from the inside,
    // so it covers the viewport, and each fragment runs three `rain_layer`
    // octaves of hashes and smoothsteps before being multiplied by an intensity
    // of 0 at the very end.
    let want = if intensity <= 0.0 {
        Visibility::Hidden
    } else {
        Visibility::Inherited
    };
    if *vis != want {
        *vis = want;
    }
    if intensity <= 0.0 {
        // Also skips the material write below, which was re-uploading a uniform
        // every frame to say the same thing.
        return;
    }

    tf.translation = cam.translation();

    if let Some(mut mat) = materials.get_mut(&mat_handle.0) {
        mat.params = Vec4::new(intensity, time.elapsed_secs(), RAIN_SLANT, 0.0);
        mat.tint = LinearRgba::new(0.72, 0.78, 0.88, RAIN_OPACITY);
    }
}

/// Marker for the planet-wrapping cloud sphere (see `shaders/globe_clouds.wgsl`).
#[derive(Component)]
pub(crate) struct GlobeClouds;

/// Height of the planet cloud shell above the globe surface, in metres. Well
/// clear of the surface so it doesn't z-fight the globe from orbit (the depth
/// test is also forced Always — see the material's `specialize`).
pub(crate) const GLOBE_CLOUD_ALT: f32 = 25_000.0;
/// 3D noise frequency for the planet cloud sphere (feature scale on the globe) —
/// low, so cloud systems are large and connected rather than a field of puffs.
pub(crate) const GLOBE_CLOUD_FREQ: f32 = 1.8;
/// Overall opacity of the planet cloud sphere.
pub(crate) const GLOBE_CLOUD_OPACITY: f32 = 0.85;
/// Fixed global cloudiness for the planet layer — a realistic partly-cloudy Earth
/// (the shader's latitude banding pushes this up in the ITCZ, down in the
/// subtropics). Deliberately NOT the ground weather slider, which is one point.
pub(crate) const GLOBE_CLOUD_COVER: f32 = 0.5;

/// Planet-scale cloud layer for the globe / space tier: a sphere just above the
/// surface, sampled by a 3D noise field and shaded by the sun terminator. Fades
/// in with `space_factor` (absent on the ground, where the ceiling shell rules).
#[derive(Asset, TypePath, AsBindGroup, Clone)]
pub(crate) struct GlobeCloudMaterial {
    /// xyz: sphere centre in render space, w: radius (m).
    #[uniform(0)]
    centre: Vec4,
    /// World direction to the sun (local anchor frame).
    #[uniform(1)]
    sun_dir: Vec4,
    /// x: coverage, y: opacity (space fade), z: time, w: noise frequency.
    #[uniform(2)]
    params: Vec4,
    /// Sunlit cloud colour; alpha carries the overall opacity.
    #[uniform(3)]
    lit_color: LinearRgba,
    /// Night-side cloud colour.
    #[uniform(4)]
    shadow_color: LinearRgba,
    /// Earth's spin axis in the local frame (latitude banding + zonal drift).
    #[uniform(5)]
    north_axis: Vec4,
    /// xyz: local-frame direction to geographic (0°,0°), used to recover longitude
    /// for the data lookup. w: data weight (0 = procedural fallback, 1 = live
    /// GFS/TCDC coverage). See `systems::globe_weather` and ADR-032.
    #[uniform(6)]
    frame_ref: Vec4,
    /// Live Total Cloud Cover grid (equirectangular, r = cloud fraction). A
    /// mid-grey placeholder until the first GFS fetch lands (weight stays 0 so it's
    /// unused meanwhile).
    #[texture(7)]
    #[sampler(8)]
    cloud_field: Handle<Image>,
}

impl Material for GlobeCloudMaterial {
    fn fragment_shader() -> ShaderRef {
        "shaders/globe_clouds.wgsl".into()
    }

    fn alpha_mode(&self) -> AlphaMode {
        AlphaMode::Blend
    }

    fn specialize(
        _pipeline: &MaterialPipeline,
        descriptor: &mut RenderPipelineDescriptor,
        _layout: &MeshVertexBufferLayoutRef,
        _key: MaterialPipelineKey<Self>,
    ) -> Result<(), SpecializedMeshPipelineError> {
        use bevy::render::render_resource::CompareFunction;
        // Seen from outside in orbit: keep back-face culling (draw the outer
        // shell). Don't write depth, and force the depth test to Always — from
        // 10,000 km the ~25 km shell is within the globe's depth precision, so a
        // normal test z-fights and the clouds show only as scattered dots; Always
        // makes the near hemisphere's clouds draw cleanly over the surface.
        if let Some(depth) = descriptor.depth_stencil.as_mut() {
            depth.depth_write_enabled = Some(false);
            depth.depth_compare = Some(CompareFunction::Always);
        }
        Ok(())
    }
}

/// Spawns the planet cloud sphere (hidden until the globe/space tier). A unit
/// sphere scaled to the shell radius each frame in `update_globe_clouds`.
pub(crate) fn setup_globe_clouds(
    mut commands: Commands,
    mut meshes: ResMut<Assets<Mesh>>,
    mut materials: ResMut<Assets<GlobeCloudMaterial>>,
    mut images: ResMut<Assets<Image>>,
) {
    if env_flag("CARTO_NO_CLOUDS") {
        return;
    }
    // Placeholder cloud-cover grid (mid-grey) at the live grid's dimensions, so the
    // texture binding is valid from frame 0. `globe_weather` overwrites its pixels
    // once a GFS fetch lands and ramps the material's data weight up from 0.
    let cloud_field = images.add(systems::globe_weather::placeholder_grid());
    let mesh = meshes.add(Sphere::new(1.0).mesh().ico(6).unwrap());
    let material = materials.add(GlobeCloudMaterial {
        centre: Vec4::ZERO,
        sun_dir: Vec4::Y,
        params: Vec4::ZERO,
        lit_color: LinearRgba::WHITE,
        shadow_color: LinearRgba::BLACK,
        north_axis: Vec4::Y,
        frame_ref: Vec4::new(1.0, 0.0, 0.0, 0.0),
        cloud_field: cloud_field.clone(),
    });
    commands.insert_resource(systems::globe_weather::GlobeCloudData::new(cloud_field));
    commands.spawn((
        Mesh3d(mesh),
        MeshMaterial3d(material),
        Transform::default(),
        Visibility::Hidden,
        bevy::light::NotShadowCaster,
        bevy::light::NotShadowReceiver,
        GlobeClouds,
    ));
}

/// Positions the planet cloud sphere at Earth's render-space centre (like
/// `globe::update_atmosphere_origin`), sizes it to the shell radius, and drives
/// coverage / lighting / space fade. Hidden below the globe threshold.
#[allow(clippy::type_complexity)]
pub(crate) fn update_globe_clouds(
    dev: Res<DevPanelState>,
    time: Res<Time>,
    globe: Res<systems::globe::GlobeState>,
    camera: Query<
        (&GridCell, &Transform),
        (With<crate::components::FreeCamera>, Without<GlobeClouds>),
    >,
    mut clouds: Query<
        (
            &mut Transform,
            &mut Visibility,
            &MeshMaterial3d<GlobeCloudMaterial>,
        ),
        With<GlobeClouds>,
    >,
    mut materials: ResMut<Assets<GlobeCloudMaterial>>,
    cloud_data: Option<Res<systems::globe_weather::GlobeCloudData>>,
    // Resolved once, not every frame: `std::env::var` takes a process-global lock
    // and allocates, and this is a debug seed that cannot change while running.
    // Same shape `frame_stats` uses for its own flag.
    mut cloud_time_override: Local<Option<Option<f32>>>,
    // Same shape, and for the same reason: a bisecting switch cannot change while
    // the process runs, and `env_flag` takes a process-global lock.
    mut clouds_disabled: Local<Option<bool>>,
) {
    let Ok((mut tf, mut vis, mat_handle)) = clouds.single_mut() else {
        return;
    };
    let Ok((cell, cam_local)) = camera.single() else {
        return;
    };
    let space = if globe.ready {
        globe.space_factor(cell, cam_local)
    } else {
        0.0
    };

    // Absent on the ground; the camera-relative ceiling shell covers that tier.
    //
    // …and absent for the whole run under `CARTO_NO_CLOUDS`, which joins
    // `CARTO_NO_ATMOSPHERE` / `NO_HAZE` / `NO_ENVMAP` / `NO_OCEAN` as a bisecting
    // switch. It is the *only* way to take this shell out of a frame, and that is
    // why it exists: coverage here is the `GLOBE_CLOUD_COVER` constant blended
    // with live GFS, deliberately not the weather slider (see the `params` write
    // below), so neither `--clouds` nor Settings ▸ World moves it — and
    // `globe_clouds.wgsl` evaluates nine 6-octave `fbm3` fields before it ever
    // consults coverage, so there is no cheap value to set it to either. A shell
    // at planet radius fills the screen from low orbit, so this is the largest
    // single per-fragment cost in the space tier and nothing could measure it.
    //
    // Frame rate is deliberately *not* a way it can be off: whether there are
    // clouds is the user's choice (Settings ▸ World), not something the app takes
    // away when a frame runs long. `systems::adaptive_quality` used to, and did it
    // during every launch's loading frames and never gave them back (removed
    // 2026-09-28, `docs/notes/render-performance-survey.md`).
    let disabled = *clouds_disabled.get_or_insert_with(|| crate::env_flag("CARTO_NO_CLOUDS"))
        || !dev.clouds_enabled;
    let want = if space <= 0.0 || disabled {
        Visibility::Hidden
    } else {
        Visibility::Inherited
    };
    if *vis != want {
        *vis = want;
    }
    if space <= 0.0 || disabled {
        return;
    }

    // Earth's centre in render coords (camera is the floating origin), same as
    // the planet atmosphere entity.
    let edge = crate::grid::WORLD_CELL_EDGE as f64;
    let centre = globe.center_local.as_dvec3()
        - bevy::math::DVec3::new(cell.x as f64, cell.y as f64, cell.z as f64) * edge;
    let centre = centre.as_vec3();
    let radius = globe.radius + GLOBE_CLOUD_ALT;
    tf.translation = centre;
    tf.scale = Vec3::splat(radius);

    // Sunlit cloud colour is white lightly warmed by the current sun tint; the
    // shader's terminator handles the day→night falloff across the sphere.
    let (sun_c, _) = sun_light(dev.time_of_day, dev.sun_declination_rad, dev.fog_density);
    let sun_v = LinearRgba::from(sun_c).to_vec4();
    let mut lit = Vec4::splat(1.0).lerp(sun_v, 0.35);
    lit.w = GLOBE_CLOUD_OPACITY;

    if let Some(mut mat) = materials.get_mut(&mat_handle.0) {
        mat.centre = centre.extend(radius);
        mat.sun_dir = sun_direction(dev.time_of_day, dev.sun_declination_rad).extend(0.0);
        // Coverage: live global Total Cloud Cover (GFS/TCDC, ADR-032) when it has
        // landed, else the fixed procedural baseline. `GLOBE_CLOUD_COVER` is the
        // fallback the shader's latitude banding varies (heavier ITCZ, clear
        // subtropics); the ground weather slider is deliberately NOT used here (one
        // met.no point shouldn't overcast the planet). `frame_ref` carries the
        // geographic longitude reference for the data lookup and the data weight
        // (0 until the first fetch lands, then ramped to 1 in `globe_weather`).
        let weight = cloud_data.as_ref().map_or(0.0, |d| d.weight);
        mat.frame_ref = globe.direction_local(0.0, 0.0).extend(weight);
        // Cloud animation phase. Normally the elapsed wall-clock; a `--shot`
        // captures only ~2 s in, so cloud MOTION is otherwise untestable offscreen
        // — `CARTO_CLOUD_TIME=<seconds>` seeds the phase to inspect the field
        // at any age (e.g. verify it doesn't smear after 10 min), like `--time`
        // does for the day-night lighting.
        let seeded = *cloud_time_override.get_or_insert_with(|| {
            std::env::var("CARTO_CLOUD_TIME")
                .ok()
                .and_then(|s| s.parse::<f32>().ok())
        });
        let cloud_t = seeded.unwrap_or_else(|| time.elapsed_secs());
        mat.params = Vec4::new(GLOBE_CLOUD_COVER, space, cloud_t, GLOBE_CLOUD_FREQ);
        mat.lit_color = LinearRgba::from_vec4(lit);
        mat.shadow_color = LinearRgba::rgb(0.06, 0.09, 0.16);
        mat.north_axis = globe.north_axis_local().extend(0.0);
    }
}

/// The overcast look, as arithmetic.
///
/// None of it can be checked by rendering here — a software rasteriser is not
/// evidence about colour or exposure (`docs/notes/headless-shots-software-renderer.md`)
/// — but all of it is decided on the CPU before a fragment runs, so all of it can
/// be checked as numbers. These pin the two balances the tuning rests on: what an
/// overcast sky does to the *ground*, and where the deck's own base tone lands
/// against the horizon it has to meet.
#[cfg(test)]
mod overcast_tests {
    use super::*;

    /// Ground illuminance under this model, for a coverage — beam plus fill, at
    /// the sun's noon elevation.
    fn ground_lux(overcast: f32) -> f32 {
        let (hours, decl) = (12.0, 0.0);
        let (beam, fill) = overcast_lighting(overcast);
        let (_, lux) = sun_light(hours, decl, 0.0);
        let day = utils::sun::daylight(hours, decl);
        lux * beam + ambient_light(hours, 0.0, decl, true).brightness + fill * day
    }

    /// A clear sky must be untouched: every overcast term is a multiple of the
    /// coverage, so zero coverage is the old scene exactly.
    #[test]
    fn a_clear_sky_is_not_an_overcast_one_with_the_knobs_at_zero() {
        assert_eq!(overcast_lighting(0.0), (1.0, 0.0));
        assert_eq!(cloud_sky_floor(0.0), CLOUD_SKY_FLOOR_SCATTERED);
    }

    /// The exposure lift has to pay for the light the model removes. It need not
    /// pay *all* of it — an overcast day should still read a little duller — but
    /// the gap is what the phone was rendering three times too dark through.
    #[test]
    fn the_exposure_lift_covers_most_of_the_light_the_deck_takes() {
        let stops_lost = (ground_lux(0.0) / ground_lux(1.0)).log2();
        // The model itself must stay a real dimming — an overcast sky that lit the
        // ground as brightly as the sun would be no overcast at all.
        assert!(
            (1.0..3.0).contains(&stops_lost),
            "overcast should cost 1–3 stops, costs {stops_lost}"
        );
        let residual = stops_lost - OVERCAST_EXPOSURE_LIFT;
        assert!(
            (0.0..=0.75).contains(&residual),
            "an overcast scene should land within ¾ of a stop of a sunny one, \
             lands {residual} stops under"
        );
    }

    /// …and it has to arrive as *fill*, not as a dimmer. At full cover the sky
    /// must out-light the beam, because that — not the brightness — is what makes
    /// a grey day read as a grey day: no key light, no shadow, no direction.
    #[test]
    fn a_full_overcast_is_lit_by_the_sky_rather_than_by_the_sun() {
        let (hours, decl) = (12.0, 0.0);
        let (beam, fill) = overcast_lighting(1.0);
        let (_, lux) = sun_light(hours, decl, 0.0);
        let diffuse = ambient_light(hours, 0.0, decl, true).brightness + fill;
        assert!(
            diffuse > lux * beam,
            "the deck should out-light the beam it replaced: sky {diffuse}, sun {}",
            lux * beam
        );
    }

    /// The deck and the horizon are one sky. At full cover the base tone the
    /// shader lands on must sit within a small fraction of the fog colour the
    /// horizon-haze shell paints, or the two meet in the step the Fairphone
    /// screenshots showed — a grey lid over a white band.
    ///
    /// This is the arithmetic of `update_clouds` + `clouds.wgsl`'s ambient term
    /// for an opaque core (`shape_low == 1`), which is what a solid overcast is
    /// made of; the sun term is negligible under an optical depth of 7.
    #[test]
    fn an_overcast_deck_meets_the_horizon_it_recedes_into() {
        let fog = systems::free_camera::distance_fog(0.2, 12.0, 0.0);
        let haze = fog.color.to_linear().to_vec3();
        let base = haze.max(CLOUD_NIGHT_FLOOR) * CLOUD_AMBIENT_GAIN * cloud_sky_floor(1.0);
        let ratio = base.length() / haze.length();
        assert!(
            (0.8..=1.0).contains(&ratio),
            "an overcast deck should land on the haze colour, lands at {ratio}× it"
        );
        // The old constants, for the record: a base of 0.35 × (0.38, 0.44, 0.55)
        // against a fog colour of ~(0.46, 0.52, 0.65) — under a third of it.
        let was = Vec3::new(0.38, 0.44, 0.55) * 0.35;
        assert!(was.length() / haze.length() < 0.4);
    }

    /// …and scattered cloud must *not* get that treatment. A lone cumulus has a
    /// dark base against a bright sky; lifting it to the haze colour would erase
    /// the one cue that says the thing is solid.
    #[test]
    fn scattered_cloud_keeps_a_dark_base() {
        assert!(cloud_sky_floor(0.3) < 0.5 * cloud_sky_floor(1.0));
        // Monotonic across the range, so no coverage between the two ends is
        // darker than one below it.
        let mut last = -1.0;
        for i in 0..=20 {
            let f = cloud_sky_floor(i as f32 / 20.0);
            assert!(f >= last, "sky floor dipped at cover {}", i as f32 / 20.0);
            last = f;
        }
    }

    /// A run with no environment map has to make the ambient stand in for it, and
    /// the difference is the whole reason the phone's shaded facades and its water
    /// came out black. Night is untouched: the env map is a *sky* light.
    #[test]
    fn a_run_without_sky_ibl_gets_a_real_ambient_fill() {
        let ibl = ambient_light(12.0, 0.0, 0.0, true).brightness;
        let none = ambient_light(12.0, 0.0, 0.0, false).brightness;
        assert!(
            none > 3.0 * ibl,
            "the fill that replaces the env map should dominate the trim it \
             replaces: {none} vs {ibl}"
        );
        // …but stay a fill rather than a key light: the sun is still the sun.
        let (_, sun) = sun_light(12.0, 0.0, 0.0);
        assert!(none < 0.25 * sun, "ambient {none} rivals the sun {sun}");
        assert_eq!(
            ambient_light(1.0, 0.0, 0.0, true).brightness,
            ambient_light(1.0, 0.0, 0.0, false).brightness
        );
    }
}
