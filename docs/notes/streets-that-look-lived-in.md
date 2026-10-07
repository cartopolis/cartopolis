---
topic: map
triggers: ["soul problem", "lived in", "sterile", "too clean", "square", weathering, wear, kerb, kerbs, gardens, "bike rack", fietsenrek, "street furniture", paving_edges.rs, gardens.rs, "real data"]
updated: 2026-10-07
---

# Streets that look lived in — from real data only

Started 2026-10-05 from Radical Elements' "City building games have a soul
problem, pt. 2": city builders feel lifeless because everything is square and
clean — hard edges between surfaces, no wear, no stairs, manicured gardens, no
clutter, and poverty you can only read in a statistics panel. Cartopolis starts
from registers rather than a brush, so the question became which of those the
registers can *honestly* supply.

**The rule (Jeroen, 2026-10-05): world content is built from real data, and the
real data stays the reference.** Where a register is silent, draw nothing rather
than a guess. Prims and simulator content are separate and not bound by it.
Rendering texture (a gradient, a stone pattern) is not content; an object or a
placement is.

## What landed, and where it lives

| What | Source | Where |
|---|---|---|
| Kerbstone band + 10 cm stone along pavement↔carriageway | BGT surface boundaries (a partition) | `geo::paving_edges`, `LineKind::Kerb` |
| Worn soil/moss edge where paving meets surveyed green | BGT boundaries | `paving_edges`, `flat_fill::Shade` |
| Gardens split planted/hard | BGT `erf` + PDOK infrared photo | `atlas pipeline/gardens.rs`, [gardens-from-the-infrared-photo.md](gardens-from-the-infrared-photo.md) |
| Grass blades on surveyed green | BGT verges/grass + garden split | `turf::GrassArea::add_surveyed` |
| Bike racks, litter bins, waste points, public toilets | BGT `straatmeubilair`, `bak` | `atlas pipeline/bgt.rs`, `FurnitureKind::{BikeRack,Toilet}` |
| OSM furniture dropped where the BGT has the same object ≤2.5 m away | — | `furniture::drop_surveyed_twins` |
| Upload a model straight into a street-object kind | — | atlas web Library page |

Measured 2026-10-05/06 (details in the linked notes and
[surveyed-street-objects.md](surveyed-street-objects.md)): one dense Groningen
cell gives ~66 km of kerb and ~10 km of worn edge in 0.7 s debug; Groningen has
17,124 gardens; BGT coverage of racks/bins/toilets is the municipality's choice
(Den Haag ~400 racks in its centre, Groningen 14 city-wide; of the boxes
sampled, only Amsterdam's had toilets).

## Rejected, with the reason

- **Raising the pavement** for a true kerb step: buries verge grass blades,
  sinks every lamppost and bench on it, and takes paving off its ground-stack
  rung. The kerb is a stone on a flat pavement instead — right from the road, a
  10 cm lip from the pavement.
- **Lawn vs shrub vs tree in gardens** from the infrared photo or AHN's
  DSM−DTM: AHN4 is flown leaf-off and the 0.5 m raster interpolates through bare
  crowns; a Den Haag canopy classified as lawn. Only planted/hard is drawn.
- **Weeds, toys, washing lines in particular places**: no register has them.
  Players can place them as prims.
- **Showing poorer neighbourhoods as run-down** (CBS income per buurt): real data,
  but a judgement about real people. Not done.

## Not done yet, and what is known about each

- **Building weathering by age.** BAG `bouwjaar` is already on every building and
  drives the era palette; grime, moss and streak intensity by age is a shader
  change with no new data. Colour is not evidence on lavapipe — judge on the Deck.
- **Stairs as stairs.** BGT `voetpad op trap` reaches the client as
  `PavingKind::Steps` and is drawn flat; `gebouwinstallatie` has `toegangstrap`/
  `bordes` (16 over Groningen). Riser count would come from AHN.
- **Worn edge against OSM green.** The pass only sees BGT surfaces, so a pavement
  beside an OSM park fill gets no edge.
- **Road wear by traffic.** OSM class everywhere, NDW counts on main roads only.
- **Trees in gardens** from AHN *point clouds* (LAZ, CC0, highest return), or a
  municipal tree register — not the WCS raster.
- **Still unread BGT objects with a real form**: `bord` (sign subset, see
  surveyed-street-objects.md), `kast`, `bloembak` (planters, 91 in Den Haag's
  centre), `waterinrichtingselement_punt` mooring posts (898 in Groningen).
- **Inferred layers predating the rule**: scattered trees in canopy polygons,
  parked cars/bikes in lots, pedestrians. Not reviewed against it yet.

## How to check work like this

The kerbs and the gardens were verified against the aerial photograph, not by
metrics alone (the bike racks, bins and toilets were not looked at yet): `paving_edges::real_cell::plan_of_a_real_cell` and
`gardens::real_area::plan_of_real_gardens` (both ignored tests) draw the result
over real data; before/after captures belong on the **Deck** (GPU), with any
unshipped atlas output copied there behind a small proxy to the live host.
Three traps cost hours here: a stand-in host whose port another session already
held (the check request reached *theirs*), a `--bbox` atlas run that measured
only part of each cell and so changed building colours in the "after", and a
scratch client build that filled the shared cargo-target disk.
