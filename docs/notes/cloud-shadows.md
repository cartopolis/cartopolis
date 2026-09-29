---
topic: cloud-shadows
triggers: [cloud shadows, "shadows of the clouds", DirectionalLightTexture,
  pbr_light_textures, pbr_clustered_decals, LIGHT_TEXTURES, "light cookie", gobo,
  "clouds don't shade the ground"]
updated: 2026-09-29
---

# Cloud shadows on the ground: not built, and the one route that fits

The deck casts no shadow. Moving patches of shade under a broken sky are one of
the strongest outdoor cues there are, and they were proposed on 2026-09-29 beside
cloud colour from the atmosphere, live wind and clouds in the water — the other
three were built, this one was deliberately left as a note. Nothing below has
been run.

## The route: bevy's light texture on the sun

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

## What building it would take

Rasterise the low deck into that image from `cloud_field.wgsl`'s field — the
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
