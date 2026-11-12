---
topic: globe-ocean
triggers: [globe_ocean.rs, ocean_depth.rs, ocean_depth, SEA_RAMP, tint_sea, globe_ocean.wesl, systems::globe_ocean, tile_loader.rs,
  ocean_mask, GlobeOcean, GlobeSurfaceMaterial, GlobeOceanGloss, has_mask,
  CARTO_NO_OCEAN, "ocean from orbit", "sun glint", "the sea from space", sea_sheen, map_light.wesl, sunglint]
updated: 2026-11-12
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

## The glint survives the map light

From 20 km of camera distance `map_light.wesl` relights the ground as if every
light stood overhead (`painted-far-map.md`), and with the angle it took the whole
specular term: from 60 km out to orbit the sea was the flat painted blue with no
sun on it at all, and the split above drew nothing (mac-1, 2026-11-03, the
Mediterranean from 700 km at 11:30 UTC). `map_light::sea_sheen` puts it back on
the water texels at the strength the map light took over, so below 20 km the
material's own specular is the glint and past 60 km this is.

**It is painted, not specular.** The first cut added a GGX highlight to the flat
sea, and it was a white hotspot with a smooth falloff that read as "plastic, or
metal" (asked 2026-11-03). MODIS true colour of a real glint (Terra, 2026-07-15,
the western Mediterranean, from NASA's Worldview snapshot API) has no white in
it: a broad warm-grey sheen, crop mean sRGB 136,130,113, about as bright as the
land beside it, broken into long patches where the wind is calmer or rougher. So
the sea is mixed towards that colour, as an albedo through the same map light
the land gets, over the *shape* of a GGX lobe (1 at its peak, 0.85 of the way),
with the lobe's width wandering ±35 % over a value-noise field in Web Mercator
(the tile key and UV the hex grid uses, so the globe and the flat tier draw one
field). Its width is its own — Cox & Munk at 12 m/s, α 0.25 — not either
surface's, and it reads the unramped mask on the globe, because the flat tier's
water beside a globe tile has its sheen at every height and the two must match
where they meet.

The same pass found a seam: under the map light the flat tier's water texels were
lit as their body colour (`water::far_water`, near-black) while the globe tiles
past the clipmap's edge used the painted sea, so from 25 km a straight line ran
across the North Sea at ~80 km out. `ground_detail.wesl` now hands the map light
the tile's own paint for water; below 20 km nothing changes.

## The sea has depth, drift and sky (2026-11-12)

One flat blue read as a Christmas bauble (asked: "de oceaan mag dynamischer",
"een mooie shader voor de zee"). Three layers now, each from something real:

- **Depth, in the tile's paint.** `cartopolis_geo::ocean_depth` embeds a 2048 px
  Web-Mercator image of the sea floor (`assets/globe/ocean_depth.png`, ~1 MB,
  `atlas ocean-depth` from the AWS Terrain Tiles, whose sea is ETOPO1 and
  GEBCO). `tile_loader::tint_sea` paints a painted tile's water along
  `palette::painted::SEA_RAMP` through the tile's own water mask, on the flat
  tier, its twins and the globe alike, so they meet without a step. It starts
  at `WATER`, so a coast closer than the image resolves (~20 km a pixel) and
  every lake keep the old paint; polders below sea level read as a metre or
  two of depth, which only tints water the mask already calls water. The
  rendered-tile cache keys carry `RENDERED_FORMAT` 4 for it, since the tint is
  on the client's side of the render revision.
- **The sheen is a smooth, cool silver oval now.** Painted 85 % of the way to
  MODIS's warm grey (136,130,113) it replaced the blue with a beige, grainy
  patch that read as another material; the crop's warmth was the haze. It goes
  55 % towards a blue-silver. Its streaks (one octave stretched 3:1 along a
  wind) read as big stripes laid over the sea, and a round three-octave mottle
  at ±30 % read as clouds; at ±10 % strength and ±6 % width (`MOTTLE_*`) the
  glint is a soft oval with the sea's variation in it (all asked 2026-11-12).
- **Drift.** The mottle moves downwind, its finest patches (~50 km) in ~90 s, and
  a coarse field (`ROWS_SCALE`, ~400 km) moves the sea's tone ±9 %.
- **Sky at the limb.** Towards grazing the sea goes towards a lit sky albedo
  (Fresnel's `(1 − n·v)⁴`, 55 % at most), so the oceans lighten at the globe's
  edge instead of staying one dark ball.

Not done: real swell or wave normals from orbit (sub-pixel there, see above),
and anything from the sea's real colour (chlorophyll, sediment), which would
need imagery.
