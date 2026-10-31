---
topic: scenarios
triggers: [crates/scenario, cartopolis_scenario, scenario.rs, official_areas.rs, ladder.rs, cbs-2024.toml, night-wave.toml, ne-admin1-europe.bin, assets/scenarios, CARTO_SCENARIO, "atlas areas", "region state", "region_state", "areas/", "cbs-buurt-2024"]
updated: 2026-10-31
---

# Scenarios: generic layers on a ladder of places

Built 2026-10-31 from `docs/mockups/scenarios/` (v1-engine, v4-whole,
v5-areas). The person steering asked for **generic layers from the solar
system down to the wijk**, with CBS figures as the example and no specific
application yet; a drone exercise built first was taken out of the app and
kept as a test. What the code does not say:

## One engine, everything else is files

`cartopolis_scenario` knows primitives, boards, a ladder and a clock, never a
subject. A scenario file (TOML) names its sources, its mode and its clock, and
holds tracks and region-state layers. A layer's states are **listed** (a feed,
a table: place, time, state), **derived from tracks** (soonest to enter an
area), or **read off a figure** the areas carry, by bands (`below = …`).
`assets/scenarios/cbs-2024.toml` is six such figure layers; the drone night is
`crates/scenario/testdata/night-wave.toml`.

Rules the parser enforces, from the sheets: every scenario names its sources;
a Live scenario may not derive states from a model; bands rise; a figure layer
names a field every one of its area sets has.

## The ladder: a layer stands on several rungs

`ladder::Rung` runs stars → solar system → world → continent → country →
province → municipality → district (wijk) → neighbourhood (buurt) → street,
each with the camera heights it is read at (`Rung::altitudes`; a place is then
roughly 30–300 points across). A layer lists boards on several rungs; the
client tells `Play::set_rung` the camera's rung and each layer reads its
nearest board, the coarser of two equally near, and **nothing beyond two rungs**
(`MAX_REACH`) — municipal figures have nothing to say from beyond the Moon.
Cells (H3) and networks can be named as boards already; only official areas
carry geometry, so the rungs above the country wait for those boards.

## Figures roll up; a kept-back figure stays empty

An area set carries fields (`Field`: key, label, unit, how it rolls up) and a
value per area, NaN where nothing is published. `AreaSet::roll_up` carries a
child set into its parent by each child's `parent` code: counts are summed,
shares and means weighted by another field (65+ by inhabitants, home value by
homes, density by land area — which makes it total people over total land).
A published parent figure is never overwritten. CBS's negative codes (−99997
"kept back", −99995 …) become NaN, and a NaN area is left uncoloured: v5's
"small numbers hidden", never guessed.

## The CBS sets, and where they live

`atlas areas cbs-2024 --out <site>/areas` (11 min on mac-2, 2026-10-31, almost
all of it PDOK paging 1,000 features at a time):

| set | areas | bytes | tolerance |
|---|---|---|---|
| `cbs-buurt-2024` | 14,574 | 2.9 MB | 0.0001° |
| `cbs-wijk-2024` | 3,393 | 0.9 MB | 0.0002° |
| `cbs-gemeente-2024` | 342 | 195 KB | 0.0005° |
| `cbs-provincie-2024` | 12 | 17 KB | 0.001° |
| `cbs-land-2024` | 1 | 5 KB | 0.002° |

Buurt, wijk and gemeente come from CBS's `wijkenbuurten` WFS, land parts only
(`water = NEE`), linked by CBS's own codes. CBS publishes no province or
country here, so those polygons are the Kadaster's (`bestuurlijkegebieden`),
a gemeente placed in a province by a point inside it (`Area::inner_point`:
all 342 landed in one) and the figures rolled up.

The client fetches them from `{CARTO_3DBAG_URL}/areas/<set>.bin` when a layer
opens and decodes them off the frame loop; nothing is embedded. Like every
atlas dataset, **deploying atlas does not write them**: run `atlas areas
cbs-2024 --out /data/3dbag/site/areas` on the host once (`atlas-run-on-the-host`
in memory has the identity it needs).

## Drawn as an overlay, culled by a box

`systems::scenario` paints the layer as egui shapes projected through
`place_pins::place_delta`, so the flat map and the globe agree. Areas whose
box lies outside a box of four camera heights round the camera are skipped
(at a buurt's height that leaves hundreds of 14,574); triangles are made once
per area (earcut, in degrees) and dropped wholesale past 6,000.

## Tracks, against the drone sheet

The engine samples a track every 2 km to find its stays per area, so a state
at any time is a lookup. Against v1-drones at 02:10 it names the same next
three provinces as the sheet, 0/2/7 minutes out where the sheet says 2/6/9,
because the sheet's `gen.ts` sampled every four minutes
(`tests::the_drone_exercise_at_0210_matches_the_sheet`).

For a capture: `CARTO_SCENARIO=cbs-2024/share_65_plus`, and a
`CARTO_3DBAG_URL` that serves `areas/` (a loopback stand-in on the node works).
