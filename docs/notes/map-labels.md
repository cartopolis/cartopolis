---
topic: rendering
triggers: [street_labels.rs, vector_tiles.rs, "street names", "labels", "place_labels", "text on the map"]
updated: 2026-07-31
---

# Text on the map lies on the ground, and is not baked into the tiles

The Shortbread tiles carry a full set of label layers we do not draw as part of
the tile: `street_labels`, `place_labels`, `pois`, `addresses`,
`water_lines_labels`, `street_labels_points`. `vector_tiles::PAINT_ORDER`
deliberately skips all of them. Street names arrived (2026-07-31) as
`systems::street_labels`, which reads `street_labels` off the same tile body the
surfaces are tessellated from and lays each name flat on its street as a textured
quad in the world.

Two other placements were tried or considered, and both lost:

* **Baked into the tile raster** (tiny-skia has no text, so this also costs a
  glyph rasteriser). Killed by overzoom and by decluttering. A z14 tile magnified
  to z17 magnifies its lettering eight times with it, so the type size would ride
  the zoom instead of staying put; and a tile is rendered alone on a worker, so
  collisions can only ever be resolved *within* one tile — the neighbour's names
  are not there to collide with, and a name near an edge is clipped by it.
* **Screen-space egui text**, projected onto each street. Built first, and
  rejected on sight: it is legible at every altitude and it looks wrong. A label
  that keeps its size while the city changes scale under it, and that draws over
  the buildings standing in front of it, reads as a note stuck to the glass
  rather than as a name written on the road.

So the rule the pins follow — *a bookmark about a spot is an overlay* — does not
extend to street names. A street name is a property **of the street**, and it
belongs in the world with it: it takes the perspective of the road it lies in, a
building in front of it hides it, and it does not move.

## What lying on the ground costs

- **Size has to be banded.** A truly fixed size is unreadable from 600 m and a
  billboard from 40 m. `HEIGHT_BANDS` steps the texture height (3.4 m → 14 m)
  across four altitude ranges. It has to be *stepped*: a continuous ramp would
  re-place every label on every frame of a climb, and the whole point of putting
  the names in the world is that they hold still.
- **Ground text has a front and a back.** The baseline is laid to read
  left-to-right for the camera's current heading, and the layer is re-placed once
  the heading has moved ~20° (`YAW_REPLACE`). Labels flip as you swing the camera
  round, the way they do in every 3D map.
- **Placement can be view-independent, so it never flickers.** A name goes where
  its street's own geometry puts it (arc midpoint, chord across the word), it is
  drawn only where the street is long enough *in metres* to carry it, and the
  declutter pass compares world footprints. The pass therefore only runs when the
  loaded cells change, the band changes, or the camera turns — not per frame.
- **The text is a texture** (`fontdue`, the same rasteriser the departure boards
  use, in `theme::BRAND_TTF` — no new asset). Ink with a pale halo dilated out of
  the glyph coverage: a street name crosses tarmac, grass, water and a terracotta
  roof inside one word, and no flat colour survives all four. Textures are cached
  by name and built at most a few per frame — mass texture uploads are the
  documented way to lose the device on a weak iGPU.
- **The raster needs weight and mips, and neither is optional.** A label is 7–28 m
  of world laid at a shallow angle, so its texture is nearly always *minified*:
  level 0 alone drops half of every stroke, and the first attempt (egui's light
  body face, no mip chain) read as thin grey smears that got worse with distance.
  Semibold + a premultiplied box-filtered chain fixed it.
- **Which names are shown does follow the camera.** Selection is a disc around the
  ground point the camera is aimed at, refreshed once the view has moved ~120 m.
  Spreading the pool evenly over the whole loaded 4 km ring instead put *one*
  label in a city view — the rest were behind the camera or a kilometre out.

Adding the other label layers is the same machinery with a different anchor
(`place_labels` is points, not lines), though a *place* name is arguably the case
where the overlay argument does win — a city is not a surface you can write on.
