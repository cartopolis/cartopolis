---
topic: macro-view
triggers: [globe, "zoomed out", macro, "strategy map", lens, lenses, scenario, nuclear, fallout, landcover, space_factor, hex]
updated: 2026-10-30
---

# The macro view as a strategy map, and scenario lenses: mockup

A design proposal, not code. Everything on these sheets is new except the
"today" capture.

Above about 40 km the 3D map would crossfade (on `space_factor`) into a
painted, flat map in the manner of a strategy game's strategic view: eight
land-cover classes, roads by rank, cities as banners with their population,
glowing borders, a faint hex grid. A rail of **lenses** re-colours it
(terrain, built-up, mobility, communities, weather), and a **scenario** is a
lens with a clock that plays a modelled event over it.

The maps are drawn from real data by `gen.ts` (ESA WorldCover 2021 from
`tiles.cartopolis.org/tiles/landcover` at z6, Natural Earth 1:10m, OSM
`place=city` populations). People, community and event counts in the panels
are invented.

## Decided (2026-10-30)

- **The map style is v4 on v3's palette**: cities from the client's own tiles
  (buildings with height shadows, land use, streets by rank, stations,
  district names), fading on distance from the camera, so near ground keeps
  today's styling.
- **The hex grid is an optional layer for now**, off by default — not part of
  the base map at any height. Where v1 and v3 draw it, read it as switched on.
- Clouds are a setting (off / light / full); the globe gets a night side.
- The nuclear-exchange scenario (v1-nuclear) is set aside.

## v4-cities: cities with detail from a distance

![v4-cities](v4-cities.png)

Asked for 2026-10-30: give cities more detail far out, from elements the map
already has. Everything comes from the client's own OSM tiles (Shortbread):
building footprints with a shadow from each one's `height` and the real sun,
land use split into housing / industry / shops / parks, streets by rank,
stations and district names. Each stays to a further height than today and
they give way one after another: buildings to ~60 km, local streets to 20 km,
then land use with a lifted built-up edge until WorldCover takes over. Drawn by
`city.ts` against today's captures at 10 km (Utrecht, Amsterdam), 40 km and
120 km. Roof tones are a hash per building (OSM rarely has roof colours);
3DBAG roofs and heights could replace both in the Netherlands.

## v3-transition: closer colours, clouds as a setting, a night side

![v3-transition](v3-transition.png)

After review (2026-10-30): the painted palette moved towards the client's
(muted greens, navy sea, grey towns) so the two blend rather than switch; the
painted look follows distance from the camera, so what is near always keeps
today's styling; clouds are a setting (off / light / full, off by default from
above); the globe has a night side from the real sun with city lights from
Natural Earth urban areas. The v1 sheets were re-rendered in this palette; the
v2 picture keeps the old one.

## v2-transition: from the street to the globe

![v2-transition](v2-transition.png)

No switch: every layer fades on altitude, like `space_factor` today. The 3D city
holds to 15 km and is gone by 50 km, the painted ground comes in over the
same band, banners appear from 2 km, the hex grid lives between about 100 and
4,000 km. The 3, 15 and 40 km frames are today's client (mac-1); 40 km and up
are drawn from data framed to the client's camera (45° vertical fov matched
at 350 km; the 3,000 and 8,000 km frames were scaled by eye to match the
captures). Up to 50 km the painted ground is a second style of the OSM
Shortbread tiles the client already streams (`frame-40km`, `gen.ts`), above
that the WorldCover landcover; the globes come from `gen-globe.ts` (d3-geo
`geoSatellite`). Open: clouds (a Weather lens?), tilted views, the night side.

## v1-nuclear: a nuclear exchange

![v1-nuclear](v1-nuclear.png)

Follows Princeton SGS's *Plan A* (2019): its phases, timings and casualty
figures. The six bases are the publicly named NATO nuclear-sharing bases; the
cloud of other strikes is illustrative. Each base is assumed hit by one 100 kt
surface burst; rings are Glasstone & Dolan scaling, plumes simplified
WSEG-style ellipses for a 24 km/h south-westerly. Principles: sober (no score,
no choosing targets), opened only from the Scenarios lens behind a content
note, one clock across every zoom.

## v1-macro: the strategy map and its lenses

![v1-macro](v1-macro.png)

Today at 350 km (mac-1, real GPU) against the proposal at the same altitude,
then three lenses on a ~9 km hex grid.

## Found while drawing it

`tiles/landcover` has no `forest` polygons over the Netherlands from z7 up
(930 at z6 over the same box, none at z7–z10), while Germany has forest at
every zoom. Not yet checked against what the client draws.
