---
topic: cloud-shadows
triggers: [cloud shadows, "shadows of the clouds", sun_shadow, cloud_shadows,
  CARTO_NO_CLOUD_SHADOWS, directional_shadow_factor, DirectionalLightTexture,
  pbr_light_textures, pbr_clustered_decals, LIGHT_TEXTURES, "light cookie", gobo,
  "clouds don't shade the ground", cloud_field.wesl, clouds.wesl, sky.rs,
  ground_detail.wesl, building_windows.wesl, weather.rs, overcast_lighting,
  OVERCAST_BEAM_CUT, DECK_FILL, cloud_high, "shadows under an overcast sky",
  systems::atmosphere_patch, assets/shaders/patched/bevy_pbr_lighting.wesl]
updated: 2026-11-17
---

# Cloud shadows on the ground, and no sun under a closed deck

## How the clouds cast shadows (built 2026-11-17)

Per fragment, in the materials that already know the deck: `cloud_field::sun_shadow`
follows the ray to the sun up to the low deck's base, reads the field there as the
shell draws it (4 octaves, the warp on the full tier, `low_threshold` for the
cover) and returns how much sun gets through. The ground (`ground_detail.wesl`,
which the roads, trees and street furniture share) and the 3DBAG buildings
(`building_windows.wesl`) multiply `PbrInput::directional_shadow_factor` by it.
Bevy multiplies that into the directional light's shadow alone
(`pbr_functions.wesl`), so the sky's light and the reflections stay, as they do
under a real cloud. The ground already bound the deck (`CLOUD_FIELD_BUFFER`) for
its water's mirror; the buildings bind the same buffer at 102. Avatars, cars and
anything on `StandardMaterial` get none.

Off with Settings ▸ World ▸ Weather ▸ Cloud shadows (`DevPanelState::cloud_shadows`,
persisted) or `CARTO_NO_CLOUD_SHADOWS=1`; either writes 0 into the deck's
`light.w`, which the shader reads as strength. A deck that is not drawn casts none.

**This route works everywhere** (web, Apple, Android), which the light-texture route
below does not; it was missed in October because the shared `pbr_lighting` was the
only place considered, and the per-material shadow factor was not.

What was measured, mac-1, 1600×900, half cloud, three flights each: steady frame
time 38.6 → 39.3 ms over the centre at 120 m and 53.9 → 55.5 ms on the Grote Markt
(medians; the ranges overlap). Not measured on a phone.

### What it looks like, and the trap in the first version

The deck's base wavelength is ~3 km (`FREQ_LOW`), so at half cover one cloud's
shadow is over a kilometre across. In the street you are under one or out of it
for minutes, and a capture reads as "duller", not as a cloud shadow; the patches
show from a few hundred metres up, and as an edge drifting by. The owner, shown
it from 2.5 km: "ziet er wel mooi uit". Shrinking the shadows alone would put them
under clouds that are not there; smaller shadows mean smaller clouds.

The first version took the shell's opacity, `1 − exp(−7 · smoothstep(edge))` over a
widened edge: the field is narrow about its threshold, so nearly every point came
out half in shade and the city was dimmed evenly, with no patches. A plain
smoothstep 0.015 either side of the threshold is what draws the patches.

## No sun under a closed deck (2026-11-17)

Until this date `OVERCAST_BEAM_CUT` (0.92) applied to all cloud, so a full cover
left 8 % of the beam: ~7 klux against ~13 klux of fill, a sharp shadow under a
solid grey sky. It was kept because `cloud_cover` was one number, and MET's
`cloud_area_fraction` counts a veil of cirrus the same as a stratus deck. The live
weather now also reads `cloud_area_fraction_low` and `_medium`: the larger of them
is a deck, which takes all of the beam (`overcast_lighting`), and its light goes
into the fill (`DECK_FILL`, 7 klux) so the ground keeps its brightness. What is
high cloud only (`DevPanelState::cloud_high`) keeps the old cut. A slider,
`--clouds` and a shot's `clouds` mean a deck. Groningen at 14:00 UTC that day:
100 % total, 100 % low.

## The other route, not taken: bevy's light texture on the sun

Read against bevy 0.19.1 on 2026-10-17, when it looked like the only route. It
still would not run on the web or on Apple, which is why the route above won.

`bevy_light::DirectionalLightTexture` masks a directional light with an image
(R channel only). Put on the sun, a picture of the low deck becomes its shadow.
What reading bevy 0.19.1 established:

* **Two cargo features, and both gate at runtime too.** `pbr_light_textures` and
  `pbr_clustered_decals`: the texture rides the clustered-decal machinery
  (`decal/clustered.rs`), and `clustered_decals_are_usable` also requires
  binding arrays. So **no cloud shadows on the web** (WebGPU has none), none on
  Apple, and on Android it depends on the driver reporting partially bound
  binding arrays — unknown for the Adreno 840. Flipping a bevy feature rebuilds
  the whole bevy stack in every build slot.
* **Only the sun's direct term is masked**, after its BRDF
  (`color *= light.color * texture_sample` in `pbr_lighting.wgsl`). The ambient,
  the environment map and the sky are untouched, which is what a cloud shadow
  physically is.
* **The projection is along the light**: the fragment goes into the light's local
  frame and `local.xy` picks the texel, so the pattern lands where the clouds'
  shadow would. The tile is two units of the light's transform **scale** across.
  Scaling the sun's transform is safe for its shadows: the cascade code builds its
  matrix from the rotation alone (`cascade.rs`, `Mat4::from_quat`).
* **It must be `tiled: true`.** A single tile is black *outside* itself
  (`texture_sample = 0`), i.e. no sunlight anywhere beyond it.
* **It is sampled at mip 0.** A detailed image would shimmer at range. Cloud
  shadow is soft and kilometre-scale anyway: 256² over a 20–30 km tile is ~100 m
  a texel, bilinear.

### What it would have taken

Rasterise the low deck into that image from `cloud_field.wesl`'s field — the
function exists on the CPU side only in the shader, so either port `fbm` /
`hash_i` to Rust bit-for-bit (they are integer hashes, so this is possible and
testable) or render it with a small compute pass. Drift is then the light's
translation, anchored through `big_space` like everything else, and the
reshaping needs a re-bake every few seconds — a 64 KB `R8` upload, not churn.
Coverage, the deck's fade with altitude and `CARTO_NO_CLOUDS` must switch the
texture off, not just lighten it.

## Rejected

* **In our patched `pbr_lighting.wgsl`.** We already ship a copy of it
  (`atmosphere_patch`), so multiplying the sun by the field there looked free. It
  is not: that function is shared by every material, the cloud parameters (cover,
  drift, the anchor) live in *material* bindings, and there is no view binding to
  carry them without patching bevy's layout.
* **A screen-space darkening pass.** Needs world positions, hence a depth prepass
  this client no longer has (`surface-finish-and-occlusion.md`), and would darken
  the ambient and the reflections along with the sun.
