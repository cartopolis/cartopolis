---
topic: scenarios
triggers: [crates/scenario, cartopolis_scenario, scenario.rs, official_areas.rs, xlsx.rs, ladder.rs, cbs-2024.toml, ses-2023.toml, pbl-2050.toml, wlo-2060.toml, night-wave.toml, ne-admin1-europe.bin, assets/scenarios, CARTO_SCENARIO, "atlas areas", "region state", "region_state", "areas/", "cbs-buurt-2024", SES-WOA, statusscores, "regionale prognose", WLO, COROP, "change_since", "field_at"]
updated: 2026-11-01
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
roughly 30–300 points across). A layer lists boards on several rungs.

**Which board a layer reads follows the screen, not the camera's rung**
(`Play::set_scale`, since 2026-11-01): the finest board whose typical area (the
median box width) is at least `MIN_AREA_PX` = 12 points wide, else the coarsest
while it is 3 points or more. Reading by rung (`Play::set_rung`: nearest board,
the coarser on a tie, nothing beyond two rungs) put a phone that sees the whole
country on the country rung, and every figure lens became one flat patch the
size of the Netherlands — reported from a phone the day the lenses landed. At
28 points that view showed the 12 provinces, at 14 it flipped between provinces
and municipalities with the load order, at 12 it shows the 342 municipalities.
The board shown is kept down to 0.8 of the threshold, so a zoom resting near it
does not flicker. Area edges are a thin dark line (`ink::AREA_EDGE`), not the
fill's own colour, or neighbours in one band merge into one patch.
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
<set> --out /data/3dbag/site/areas` for each of `cbs-2024`, `ses-2023`,
`pbl-2050` and `wlo-2025` on the host once (`atlas-run-on-the-host`
in memory has the identity it needs).

## Figures over time: a series per year, played by the clock

Added 2026-11-01 for the planning offices' figures (the person asked for SCP
and CPB as the next examples, "generic so the other uses fit later"). A figure
over time is a **series**: one `Field` per year under one key, and
`AreaSet::field_at(key, year)` gives the latest year at or before the one
asked, else the earliest. The scenario's clock of years picks the year
(`Clock::year_at`), so the same layer plays a forecast forward and a measured
series back; **between published years the last one holds** — nothing is
interpolated, which matters for the WLO's 2021/2040/2050/2060. `roll_up`
carries a series year by year, each share weighted by its own year's weight.

Two layer options, both only on a figure layer: `change_since = "2023"` reads
the figure as its change in % since that year (growth, not size), and
`range = [low, high]` names the fields that bound it (a forecast's 67 %
interval, a score's 95 %), shown as "likely … – …" under the value and never
coloured. The clock's `forecast = "2024"` marks where measurement stops: the
clock bar hatches from there and the strip says "2050 forecast". Two futures
are **two figures on one clock** (High and Low under one heading), so switching
keeps the year.

| lens | set (`atlas areas …`) | boards | years | took (mac-2, 2026-11-01) |
|---|---|---|---|---|
| `ses-2023` | `ses-2023` | buurt → land | 2014–2023, measured | 14 min; buurt 4.7 MB, wijk 1.7 MB, gemeente 280 KB |
| `pbl-2050` | `pbl-2050` | gemeente → land | 2023–2050, forecast | 2.5 min; gemeente 300 KB |
| `wlo-2060` | `wlo-2025` | COROP → land | 2021, 2040, 2050, 2060 | 2 s, 43 KB + 6 KB |

What each source is, and the traps:

- **SCP is no longer a publisher of area figures.** Its open data are survey
  microdata at DANS; its *statusscores* per postcode stopped after 2017 and CBS
  took them over in 2022 as **SES-WOA** (StatLine 86092NED: buurt, wijk,
  gemeente, every year recalculated to the 2024 boundaries). That table is the
  SCP example. A score is relative (0 ≈ the Dutch average), so its lens follows
  v5's rule for status: behind its own question, one cool ramp, no red, bands
  named by distance from the average. Negative scores are scores — unlike the
  Kerncijfers, where a negative is one of CBS's "kept back" codes; here null is
  kept back. The three parts are on wijk and up only: as series they would
  double the buurt file.
- **PBL/CBS's regional forecast 2023–2050 is the last one** (85173NED, with its
  67 % interval). PBL and CBS announced they will make no more; the WLO's
  regional scenarios replace it. It is on the 2021 municipalities: fourteen have
  merged since (`MERGED_SINCE_2021`), each summed into its successor, whose
  interval is then left empty. A 2024 municipality with no forecast fails the
  run, so a new merger cannot slip through unsummed.
- **CPB's WLO futures are national; PBL regionalises them** per COROP region
  (2021 division) in `WLO2025_PBL_Regio.xlsx`, CC BY, in thousands — a small
  region's 65-plus share is good to about a point. Read with `atlas::xlsx`, a
  minimal zip-and-cells reader, rather than a first spreadsheet crate. COROP
  regions stand on the province rung, so they also read from a municipality's
  height.
- **CBS's OData v3 refuses any query of 10,000 rows or more** with a 500 and
  pages no further (`$skip` is refused too). `cbs_rows` asks by region-code
  prefix and splits a refused prefix into its ten longer ones (`BU` → `BU0`…).
  The v4 service pages, but in long format with opaque measure codes.

The geometry is copied into each source's files (a `ses-buurt-2024` holds the
same outlines as `cbs-buurt-2024`): simple, and every board stays one file.
It costs: ten years of four figures make the SES buurt file 4.7 MB against
the Kerncijfers' 2.9 MB, all fetched when the lens opens. If
a third source lands on the buurt rung, the next step is figure-only files
joined to one geometry board by code.

## A lens, not a layer

Opened from its own disc under Layers, not from the Layers panel
(`docs/mockups/lenses/`, built 2026-11-01). Ticking a figure tile that closed
the other was a radio dressed as a checkbox, and on a phone the panel covered
the map you were choosing for. The lens is one surface (`scenario::Sheet`): a
bottom sheet in portrait (strip, figures, place, shelf), a column on the left
otherwise. Its rect goes to `place_pins::map_area` through egui's memory
(`scenario::lens_rect`), so pins under it do not eat its taps. Switching to
another figure of the same file keeps the loaded areas (`switch_figure`); only
another file fetches. A scenario file may give itself a `question` (the
shelf's heading) and each region state a `group` (the figures' heading).

On the sheet the raised heights get out of the way (asked 2026-11-02: "the
dialog is in the way on mobile"): a pick — a lens off the shelf, a figure
from the list or the place card — drops it to the strip, and a drag that
starts on the bare map (`EguiManager::map_dragged`, decided by where the
press landed) lowers whatever is raised, as a flick down on the grabber
does. The column, beside the map rather than over it, stays where it is.

The lens answers back (asked 2026-11-01, "too static"): the area being read
is outlined on the map — under the pointer on a desktop, with a card beside
it, else under the crosshair; the scale's bands single theirs out (pointing
previews, a tap keeps it, `CARTO_LENS_BAND=n` for a capture) and a marker
stands over the band here, as a tick does on each figure's small scale; and
an area's colour eases to its state's (`Open::colours`), so a played year or
a switched figure flows over and a finer board fades in. **The map paints
before the chrome**, so a band picked on the scale reaches the map a frame
later, and the chrome asks for that frame itself. A finger has no hover: on
a phone the crosshair is the pointer.

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
