---
topic: painted-far-map
triggers: [painted.rs, lights_mask, GlobeNight, update_globe_night, GlobeCloudMode, CARTO_GLOBE_CLOUDS, CITY_LIGHT, globe_ocean.wesl, PAINTED_TWIN_ZOOM, PAINTED_FADE_M, PaintedTwin, twin_cache_key, painted_style, FarDetail, far_buildings, far_detail, FAR_STYLE_TILE_M, "palette::painted", place_names.rs, extract_place_labels, PlaceKind, "place names", "city banner", "macro view", "zoomed out"]
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

## The two styles crossfade on distance, through a painted twin

A tile cannot be both styles at once, so a z13 tile (`tile_loader::PAINTED_TWIN_ZOOM`)
is drawn twice: as itself and as a painted twin, kept on disk under its own
key (`twin_cache_key`) and bound beside it in the ground material (106–107).
`ground_detail.wesl` mixes the two on the fragment's distance from the camera,
4.5 to 8 km (`map_stream::PAINTED_FADE_M`). Why z13 and those numbers: the tree
picks z14 and finer within ~4 km and z12 and coarser past ~8–10 km, so z14 is
never asked to be part-painted and z12 is always fully painted. The band ends
before 10 km because v4's straight-down 10 km frame is fully painted.

What it costs: a second raster and texture for each z13 tile — 49 at most,
~70 MB of graphics memory on a desktop, a quarter of that at a phone's 256 px —
and four z14 fetches each for its footprints.

Not done, and why: v2's chart sinks the 3D buildings into the ground between 15
and 50 km. Only the far OSM tier has a height ramp (`building_fade`); the 3DBAG
shells and blocks would still vanish in one step at 50 km, so sinking one tier
of three was left out. And the haze over the ground at 10 km is left as it is:
v3 keeps it to ~15 km, and above that `CITY_ATMOSPHERE_ALT` already thins it.

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

## From above: clouds as a setting, the night side

**Clouds** from above are Layers ▸ Map ▸ From above, off / light / full, **off by
default** (`GlobeCloudMode`). It governs the globe's shell only; Settings ▸ World
▸ Clouds still turns every cloud off, the street's included. A scripted capture
never reads the saved setting, so `CARTO_GLOBE_CLOUDS=off|light|full` sets it
for one.

**The night side** follows the real sun's direction (`update_globe_night`), not
the directional light: at night over the map anchor the sun light is dimmed to
nothing, and a terminator read from it would put the whole globe in the dark.

**City lights have no data of their own.** A globe tile is painted, and built-up
land is painted in exactly three tones, from the land cover and OSM alike; so
`globe::lights_mask` reads them off the tile's own pixels into an R8 mip chain.
That also means a tile read back from the disk cache has its lights, with no
second cache entry. The design drew them from Natural Earth's urban areas; this
is WorldCover's built-up class instead, which is denser in the east.

**Brightness, measured** (mac-1, 2026-10-31, 8,000 km, 17:30 UTC): the emissive
is added before exposure and blooms, so it saturates long before it looks
bright. At 9000/6500/3400 the lights were balls and the night side an orange
wash; cutting it 8× dimmed the frame by only a quarter. `CITY_LIGHT` at
160/115/60 gives separate points of light on a dark ground.

The flat map near the anchor (drawn to 250 km up) has no night side or lights;
from orbit it is a small patch, not looked at yet.
