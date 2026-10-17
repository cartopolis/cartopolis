---
topic: globe-ocean
triggers: [globe_ocean.rs, globe_ocean.wesl, systems::globe_ocean, tile_loader.rs,
  ocean_mask, GlobeOcean, GlobeSurfaceMaterial, GlobeOceanGloss, has_mask,
  CARTO_NO_OCEAN, "ocean from orbit", "sun glint", "the sea from space"]
updated: 2026-10-17
---

# The globe's ocean is a BRDF split, not water

From orbit a wave is orders of magnitude below a pixel, so nothing in
`water.wesl` transfers (`water-surface.md`). What makes an ocean read as an ocean
in every photograph from space is that it is dark and glossy where land is
bright and matte, plus a broad sun glint. So `systems::globe_ocean`
(`assets/shaders/globe_ocean.wesl`, the material in
`cartopolis_materials::globe_ocean`) is one water mask per texel driving
roughness, specular and tint on the globe's own tile materials, and it shares
nothing with the local water.

## The mask rides in the imagery's alpha

The globe's tiles are opaque, so their alpha was free: the mask costs no extra
texture, binding or upload, and the mip chain averages it with the colour so a
coastline softens with distance instead of aliasing. Two things are load-bearing:

* **Read it from `pbr_input.material.base_color.a` before `alpha_discard`**,
  which flattens alpha to 1 for an opaque material.
* **The `has_mask` flag (`ocean.y`) is not optional.** An unbound base-colour
  texture samples as opaque white, so without it the backstop sphere — which has
  no imagery at all — turns the whole planet into ocean.

The globe's convention is **255 = water**. The flat clipmap tiles pack the
opposite into the same channel — land coverage, 255 = land — so that a source
with no mask reads as land there (`water-surface.md`, "The ring's edge"). Do not
share code between the two on the assumption they agree.

Vector sources get an exact, anti-aliased mask from geometry
(`vector_tiles::ocean_mask_sized`, which must be rasterised at the size the
imagery was, or the caller drops it and the tile silently loses its ocean).
Raster sources are classified by colour, `blue − red` on a soft ramp, which
separates map water and real sea from land where a hue test trips over the green
end; a misread costs a little gloss in the wrong place, never a wrong shape.

## It ramps with the rest of the tier

Strength is `space_factor` (`GlobeOceanGloss`, written by `update_globe_albedo`
on the same throttle as the albedo and read at tile spawn). The flat map tiles
just below the tier boundary are matte and at full brightness, so gloss that
arrived early would be a step in the middle of a climb.
`the-atmosphere-from-the-street-to-orbit.md` has the general rule.

The roughness is 0.34, not a mirror: a mirror-smooth sphere concentrates the sun
into a few pixels that crawl as the globe turns, where the real glint is the sum
of a whole sea of facets. `OCEAN_REFLECTANCE` is 0.25 with a comment calling that
water's F0 of 0.02; bevy's `F0 = 0.16 · reflectance²` makes it 0.01, the error
`water-surface.md` corrected for the local surface and this one still carries.

`CARTO_NO_OCEAN=1` strips the split, leaving the matte globe, for bisecting.
