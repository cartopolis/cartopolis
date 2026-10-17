---
topic: directional-lights
triggers: [systems/map/sky.rs, setup_sky, spawn_sun_light, spawn_moon_light, manage_lights, apply_day_night_cycle, shadow_cascades, num_cascades, shadow_maps_enabled, CascadeShadowConfigBuilder, DirectionalLight, "index out of bounds", "moon shadow"]
updated: 2026-10-17
---

# The sun and the moon must agree on their shadow cascades

An external constraint in `bevy_light`, found on Bevy 0.19 and **not re-tested on
0.20**: with two `DirectionalLight`s in the scene it panics with `index out of
bounds` in its cascade visibility if

- the two lights carry **different `num_cascades`**, or
- `shadow_maps_enabled` is **mutated on a live light**.

Neither is a configuration error a test would notice; both are a crash the first
time the moon rises over a scene whose lights were set up differently, or the
first time somebody flips the shadow checkbox.

## How the client stays on the right side of it

- **One answer for both lights.** `sky::shadow_cascades()` is what both
  `spawn_sun_light` and `spawn_moon_light` pass to their
  `CascadeShadowConfigBuilder`, so a mismatch is unrepresentable rather than
  merely avoided. It is not a constant 4: it is the platform ceiling (1 on the
  web, where WebGPU reports a cascade limit of 1 and Bevy clamps to it with a
  warning; 4 natively) capped by the tier's `Budget::shadow_cascades`.
- **Shadows change by respawning, never by writing the flag.** Shadows are a
  preference (Settings ▸ Graphics) and a preset swap moves the cascade count and
  reach, so they *do* change at runtime — `manage_lights` despawns and respawns
  **both** lights from one command buffer, which also keeps the moon's own
  dusk/dawn spawn from racing it into two moons. Keep the two spawners'
  signatures matched (the moon's `_shadows` argument is unused for that reason).
- **The moon keeps a full cascade config although it never casts.** Since
  2026-09-11 `spawn_moon_light` sets `shadow_maps_enabled: false`
  unconditionally (`render-performance-survey.md` has why: a second set of shadow
  maps for a light four orders of magnitude under the sun). The config stays
  identical to the sun's because the panic is about the two *configs* agreeing
  and does not care which light is casting.

The moon light exists only while it contributes (moon up at night):
`manage_lights` spawns and despawns it rather than leaving a zero-illuminance
light in the scene, because bevy still computes its cascade frusta and walks it in
every fragment's light loop.

A comment in `apply_day_night_cycle` still says "Moon shadows stay enabled", which
has not been true since 2026-09-11; the rule beside it — do not toggle the flag at
runtime — still holds.

If this is ever re-tested on a newer Bevy, the test is a scene with a sun and a
moon at different cascade counts, and a run that flips `shadow_maps_enabled` on a
spawned light. Until then, treat both as panics.
