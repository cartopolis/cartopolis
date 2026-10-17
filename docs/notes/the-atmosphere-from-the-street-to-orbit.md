---
topic: atmosphere-and-space-tier
triggers: [globe.rs, sky.rs, free_camera.rs, render_features.rs, starfield.wesl, moon.wesl,
  systems::globe, systems::sky, PlanetAtmosphere, update_atmosphere_origin,
  update_globe_visibility, AtmosphereSettings, AtmosphereMode, Raymarched,
  AtmosphereEnvironmentMapLight, ScatteringMedium, AtmosphereMedia, space_factor,
  in_space, apply_day_night_cycle, sun_light, SunDisk, sky_shell_radius, Bloom,
  "space tier", "black globe", "the limb", "maroon dawn", "stars from orbit"]
updated: 2026-10-17
---

# Bevy's atmosphere, from the street to orbit

The sky is bevy's built-in atmosphere — the `bevy_pbr` passes over a
`bevy::light::Atmosphere` component — not the `bevy_atmosphere` crate, which was
dropped. One atmosphere serves the city and the planet, so most of what follows
is about making a model of a ground observer's sky survive a camera at
14,000 km. Where the adapter cannot run it at all, `sky-without-an-atmosphere.md`;
the driver that copied its struct wrong, `adreno-atmosphere-struct-copy.md`; why
the raymarch starts at 2 km, `two-tiers-one-sphere.md`.

## The planet entity has to sit at Earth's real centre

The atmosphere is planetary-scale, so `free_camera::setup_camera` spawns a plain
entity — outside the `BigSpace`, its `Transform` is render coordinates — carrying
`Atmosphere::earth` and the `PlanetAtmosphere` marker, 6,360 km below the ground
so the air's surface is `y = 0`. It is spawned wherever the atmosphere is
*possible*, not only when the run starts with it, because the sky is switched
while the app runs. `PlanetAtmosphere` is a bare marker and holds no copy of the
component, whatever older text says. The light is physical — a `RAW_SUNLIGHT`
sun (× `SUN_SCALE`) against a physical `Exposure`, EV100 13.6 by day, which
`sky::apply_day_night_cycle` lifts toward night and twilight.

The camera is the floating origin, so its `GlobalTransform` stays near zero at
any real altitude. With the planet parked at a fixed `(0, −R, 0)` bevy always saw
a ground-level camera, and "raymarched from orbit" integrated a *surface* sky
dome over the whole screen — a washed or black globe, at full cost. That was
misread as Raymarched being unusable from space, on a measurement (every frame
pinned at ~1024 ms) that was in fact the occluded-window Wayland FIFO stall.
`globe::update_atmosphere_origin` now moves the entity to Earth's true centre in
render space every frame; offscreen with `--shot`, the raymarch from orbit costs
a few milliseconds.

It also sets `inner_radius` to the globe's own radius (geocentric at the anchor,
6,364.5 km at 53° N). Once the centre is right, bevy's 6,360 km is not rounding
but an altitude error — a camera 365 m up read as 4.9 km — and the step between
the two branches of `sample_transmittance_lut_segment` drew a thin blue ring
~2.2° below the horizon. Matching the radii squares two near-equal numbers at
1 m up, which is what the patched `functions.wesl`'s `sqrt` guard is for.

## The render mode is the only runtime switch globe code may make

`update_globe_visibility` picks `Raymarched` whenever the globe is drawn and
`LookupTexture` (16 samples) below. `GLOBE_SKY_SAMPLES` is 8, half bevy's
default: on an M5 Pro at 3134×1952 (2026-10-16, `cargo perf`, two flights each)
16 → 8 took `city` 10.2 → 7.0 ms and `orbit` 14.7 → 11.4 ms with no visible
difference; 4 reads hazier near the camera. `space_raymarch()` keeps the space
tier in `LookupTexture` (no limb) when the `SPACE_ATMOSPHERE` constant,
`Budget::space_raymarch` or `CARTO_NO_SPACE_RAYMARCH` says so — the tier answers
first because a phone cannot set an env flag.

**Writing fields on the live components is safe; adding or removing them from
here hangs the device.** An earlier version dropped the camera's
`AtmosphereEnvironmentMapLight` in space and got `VK_ERROR_DEVICE_LOST` with
kernel `GPU HANG` logs. Only `render_features::apply_sky` adds and removes the
camera's atmosphere components, and anything it removes ends in a re-sync
(remove `SyncToRenderWorld` + `RenderEntity`, re-insert), because of what bevy
leaves in the render world. In 0.19 `extract_atmosphere` removed
`ExtractedAtmosphere` only when *no* `Atmosphere` existed anywhere, so stripping
`AtmosphereSettings` from the camera left every pass running on stale uniforms —
an earlier off-switch did exactly that and was a render-world no-op; stripping
the planet's `Atmosphere` was then the only real off. bevy_pbr 0.20.0 clears it
per camera (`atmosphere/mod.rs`, read 2026-10-17), but `prepare_probe_textures`
still hands the reflection probe the view's lookup tables once
(`Without<AtmosphereProbeTextures>`), so the re-sync is still needed.

## Sunlight is the atmosphere's input, so don't pre-extinct it

bevy scatters the sun `DirectionalLight`'s colour and illuminance and applies
its own air-mass extinction. `sky::sun_light` extincts too (`utils::sun`'s
Kasten–Young curve, for the surface lighting bevy does not attenuate), so the two
compound: at the full curve dawn kept 9 % of the beam, the atmosphere took its
cut of that, and sunrise was a near-black maroon sky over a maroon city.
`SUN_EXTINCTION_SOFTNESS` (0.45) bends only the large-air-mass end (9 % → 32 % at
the horizon, noon untouched), `SUN_COLOR_SATURATION` (0.25; at 0.7 the city went
maroon) keeps the hue mild, and `TWILIGHT_EXPOSURE_LIFT` (2.5 stops) is the eye
adaptation that makes twilight read bright. Every one of them moves the sky and
the ground together; check both.

The disc is bevy's procedural `SunDisk` on that light, drawn by the sky pass, so
it needs no geometry and works raymarched from orbit. The true 0.53° is a ~6 px
dot at 1080p; `SUN_DISK_ANGULAR_SIZE` is 0.024 rad (~1.4°), and `Bloom` (the
`bevy_post_process` feature since 0.19, enabled for this) supplies the glare. A
`SkyMode::Dome` run has no sky pass and so no `SunDisk`: `sky_dome.wesl` draws
its own at the same shared constant.

## The space tier fades in on `space_factor`, never on `in_space`

`GlobeState::space_factor` is 0 at `GLOBE_SHOW_ALT` (50 km) and 1 by
`SPACE_FULL_ALT` (3,000 km). Exposure, bloom (0.12 → `SPACE_BLOOM_INTENSITY`
0.35), sun colour and illuminance, ambient, the shell height and density, the
globe's albedo and its ocean gloss all ride it; switched on the boolean, a zoom
stepped brightness, haze and glare at once. `apply_day_night_cycle` is gated on
`dev.is_changed()`, so it carries `was_space: Local<f32>` and re-applies when
the factor moves by `SPACE_RAMP_STEP` (0.01) — a climb changes nothing the gate
watches.

What it fades *to* is its own lighting model: a white, unattenuated sun
(`RAW_SUNLIGHT × SUN_SCALE`), Earthshine-scale ambient
(`SPACE_AMBIENT_BRIGHTNESS`, 120, bluish), the day exposure plus
`SPACE_EXPOSURE_STOPS` (0), and no overcast. The ground curves from orbit dimmed
the planet by the *anchor's* horizon: an 18:00 fly-out showed a dim red globe
instead of a lit day side and a dark night side.

## The space shell is taller than Earth's, and thinner

100 km of air on a 6,371 km ball is 1.5 % of the radius: a four-pixel limb that
reads as an outline. `SPACE_ATMOSPHERE_THICKNESS` (2.5×) raises `outer_radius`
with `space_factor`, and since `Falloff::Exponential`'s scale is a fraction of
the shell, the profile stretches with it. Density is rewritten on the shared
`AtmosphereMedia` medium (`with_density_multiplier`, rebuilt only past a 0.05
step, as it re-bakes LUTs) in three blended bands: full Earth density below
`CITY_ATMOSPHERE_ALT` (15 km), because this is also the city's sky and dawn;
down to `ATMOSPHERE_DENSITY_NEAR` (0.12) at the threshold so the aerial wash
lifts off the detailed map — the old constant column buried it in blue; up to
`SPACE_ATMOSPHERE_DENSITY` (0.15) in deep space. 0.4, one Earth column in the
taller shell, drew the Sahara grey-lavender (Deck, 14,000 km, table in
`land-cover.md`); 0.06 lost the air; at 0.15 the limb still glows because its
path through the shell is long.

## Sky shells are sized for the ground

The star field (`STAR_SHELL_RADIUS`, 150 km) and the moon (140 km) are
camera-centred shells, nearer than the planet from orbit: stars blended over the
Earth disc and the moon quad hung in front of it. `sky_shell_radius` moves them
past the far limb (`dist + 1.5 R`, inside the far plane) and `update_star_sky` /
`update_moon` scale the whole transform, which keeps every angular size. The star
shader's space flag (`params.z`) drops its horizon cut — `dir.y` is the anchor's
up, so from orbit every star on screen was "below the horizon" — with the air
extinction and twinkle. In space the field also ignores the clock (the day/night
fade left a black void at noon) and is multiplied by `2^NIGHT_EXPOSURE_LIFT`,
because it was tuned at the night exposure and space holds the day one: on
2026-08-19 at 5,000 km, 78 of 120,000 sky pixels passed luminance 32/255.

The flag is set whenever `sky_shell_radius` is `Some`, which since the two-tier
change includes the 2–50 km band. Whether that puts stars in a daytime sky there
has not been looked at.
