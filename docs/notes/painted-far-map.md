---
topic: painted-far-map
triggers: [painted.rs, painted_style, FarDetail, far_buildings, far_detail, FAR_STYLE_TILE_M, "palette::painted", place_names.rs, extract_place_labels, PlaceKind, "place names", "city banner", "macro view", "zoomed out"]
updated: 2026-10-31
---

# The far tiles are a painted map, and names come from the same tiles

Built 2026-10-31 from the macro-view design (`docs/mockups/macro-view/`, v3 and
v4, decided 2026-10-30). What follows is what the code does not say.

## Which tiles are painted: by tile, so by distance

A raster whose tile shows more than `FAR_STYLE_TILE_M` (5 km) of ground is drawn
by `vector_tiles::painted` rather than the realistic paint loop. The tile tree
picks coarser tiles further from the camera (screen-space error), so this
follows **distance from the camera, not its height** — which is what the design
asked for: in a tilted view at 3 km the ground near the camera keeps today's
style and only the band towards the horizon is painted. Over the Netherlands
that is z12 (~5.9 km) and coarser, and every globe tile.

The edge between the two styles is a tile edge for now; the crossfade on
distance is the next step of the design.

## Footprints on a far tile cost sixteen fetches

Shortbread carries `buildings` **only at z14** (checked 2026-10-30 over Utrecht:
z10–z13 have none; z14 is ~650 KB with ~9,000 footprints and a `height` on
each). So a painted z12 tile gathers its sixteen z14 descendants' footprints
(`tile_loader::far_detail`), and nothing coarser does: a z11 tile would be 64
fetches for buildings under a pixel each. In practice footprints therefore reach
as far as the tree still picks z12 — around 10–15 km straight down — and the
design's "buildings to ~60 km" is **not** met; beyond z12 built-up land carries
a soft lifted edge instead (`drop_edge`). Getting the grain further out would
need a coarser building layer made by atlas, which is new data the design said
it did not want.

The descendants come through the shared byte cache and `parse_shared`, so where
the near map already fetched them they cost a parse.

## Shadows are quantised because they are in the cache key

A footprint's shadow is `height / tan(sun elevation)` long, away from the sun
(`shadow_step`), with the elevation held to at least 15°. The sun is read from
`FAR_SUN` (kept by `tile_loader::track_far_sun`), quantised to 15° of azimuth and
5° of elevation, because the shadow is drawn into the pixels and so is part of
the rendered tile's cache key: an unquantised sun would make every stored tile a
miss each minute. A tile already drawn keeps its shadows until it is re-streamed.

## Colours are the mockup's, lit

`palette::painted` holds the mockup's screen colours, used as albedo. Lit by the
sun and seen through the atmosphere they come out darker and bluer than on the
sheet (mac-1, 2026-10-31, noon, 40 km): the hues hold, the brightness does not.
The haze is the larger difference at 10 km and below; it belongs to the
distance fade, not to the palette.

## Place names

`systems::place_names` reads `place_labels` and `public_transport` from the
vector tiles at z10 (towns, kept to 250 km up), z12 (districts, to 30 km) and z13
(stations, to 20 km) around the camera, and paints them as an egui overlay. Each
kind has a distance it is named to (`shows`), and names are placed biggest place
first with room around district names, because at 3 km a tile holds dozens of
neighbourhoods and the first build wrote all of them. Villages, hamlets and
localities are left out. A station gets its name only when it starts with a
loaded town's name (rail stations do; metro and tram stops get the icon).
