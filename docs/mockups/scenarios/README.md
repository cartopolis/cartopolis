---
topic: scenarios
triggers: [scenario, globe, stars, "star lanes", "master of orion", graph, sandbox, H3, h3o, hex_grid.wesl, "hex grid", scenarios, simulation, nuclear, fallout, drones, "air alert", "early warning", NL-Alert, lens, lenses, "strategy map", civilization, hex, "region state"]
updated: 2026-10-31
---

# Scenarios: one engine for many simulations, and the macro map towards Civilization

A design proposal, not code. Asked for 2026-10-31: make the macro map work more
like Civilization, and show a nuclear war at macro scale and a drone war
between Russia and Europe — possibly an early-warning tool for citizens later —
generic enough for many kinds of simulation. It builds on `../macro-view/`
(the painted far map, landed 2026-10-31) and brings back its v1-nuclear
scenario, which was set aside on 2026-10-30.

The core idea (`v1-engine`): a scenario is a lens with a clock that plays a
timeline of eight **primitives** — point, rings, plume, track, region state,
arc, field, counter — produced by an outside model and drawn by the client at
every zoom. Three modes: **Replay** (a published study or a past event),
**Exercise** (what-if, striped on every screen), **Live** (official feeds only;
Cartopolis never issues a warning of its own).

Maps are drawn from real data by `gen.ts` (Natural Earth 1:10m including the
admin-1 polygons, ESA WorldCover via our tile server, OSM city populations),
run from the macro-view data directory; the first ~240 lines are macro-view's
base-map code, copied. Community territories, people, counts, drone tracks and
alert texts are invented and marked so on the sheets.

Nothing decided yet.

## v3-stars: beyond the globe, nodes and lanes

![v3-stars](v3-stars.png)

Asked for 2026-10-31: in the end a Civilization, but intergalactic, using Master
of Orion's structure of nodes between planets. Above the hex globe the Earth
becomes a node: the solar system and the 45 real star systems within 16 light
years (HYG v4.1, CC BY-SA 4.0, as `hyg.csv` in the data dir) as a graph board
of nodes and lanes. The engine gains a second board kind (cells + graph); the
graph serves on Earth too (cities, rail, power). Fiction lives only in a
sandbox world marked as such. Drawn by `gen-stars.ts`.

## v2-globe: one hex board from the macro map to the globe

![v2-globe](v2-globe.png)

Asked for 2026-10-31: zoomed further out the hexes get too small; a globe view
that follows on from the macro map, in the Civilization idea. Today's hex layer
is one ~9 km grid in Web Mercator (`hex_grid.wesl`, `painted-far-map.md`). The
proposal is H3: cells merge seven into one per ~2.6× of altitude, so a cell stays
30–70 px across from 200 km to the whole planet; children crossfade into their
parent, and values roll up (sum, worst + share, majority). H3 puts all twelve
pentagons at sea. Drawn by `gen-globe.ts` (h3-js, d3-geo `geoSatellite` with the
client's 45° camera); the world frame carries v1-nuclear's fallout rolled up
per 365 km cell.

## v1-strategy: what to borrow from Civilization

![v1-strategy](v1-strategy.png)

Hex card on hover (tile yields), community reach as glowing territories
(culture borders), people counters (unit stacks), a "what changed" stack (turn
notifications), fog of data (fog of war), one time bar, sources on every number,
a minimap. Score, victory and rivals left out.

## v1-engine: primitives, modes, the pipeline

![v1-engine](v1-engine.png)

## v1-nuclear: the continent two days after

![v1-nuclear](v1-nuclear.png)

Fallout as a hex field (bands named for what a person should do), a card for
where you are. Strikes: five publicly named sharing bases (100 kt assumed) and
an illustrative scatter of ~110 smaller ones in the east. The fallout model is
a power law along the wind fitted to v1-nuclear's 100 kt contours; shape and
order of magnitude only.

## v1-drones: a night wave, as an exercise

![v1-drones](v1-drones.png)

Tracks with widening cones; each province coloured by when a group's path
enters it (now / < 1 h / < 3 h / passed), computed against Natural Earth
admin-1. Speed 170 km/h; everything else invented.

## v1-warning: early warning on a phone, later

![v1-warning](v1-warning.png)

Live (an official NL-Alert shown as sent, with the map's reading of it),
Exercise (yellow stripes, no sound), and check-ins with family and the
neighbourhood community after an alert.

## Found while drawing it

NL-Alert is a cell broadcast with no public machine-readable feed that we know
of; Live in the Netherlands depends on getting one.
