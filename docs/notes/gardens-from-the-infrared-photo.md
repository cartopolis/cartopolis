---
topic: map
triggers: [gardens.rs, "erf", garden, gardens, PavingKind::Garden, luchtfotocir, ortho25IR, NDVI, "vegetation index", "planted", roofs.rs]
updated: 2026-10-06
---

# Gardens are split along the infrared photograph

The BGT records a private plot as `onbegroeidterreindeel` with
`fysiek_voorkomen = erf` and nothing about what is on it. Until 2026-10-06 the
extractor mapped `erf` to the same `yard` form as squares and forecourts, so
**every garden in the country was drawn as paving**. Over a few streets of
Groningen: 144 current `erf` plots, none with a material.

`erf` now has its own form (`garden`, wire position 12), and atlas's `roofs`
step — which already fetches each cell's photograph — also fetches PDOK's
colour-infrared flight and cuts every garden into a planted half and a hard half
(`pipeline/gardens.rs`, written into `roofs/14/x/y.json` as `gardens`, keyed like
`paving` by the plot's first vertex). The client draws the planted half as grass
— and grows blades on it, along with every surveyed verge and grassed surface
(`turf::GrassArea::add_surveyed`) — and the hard half as paving that wears at its
lawn edge (`paving_edges`). A client older than this drops the `garden` surfaces
and shows the ground under them.

## What was measured, and what the photograph cannot say

Over 150 m squares of Groningen (Oosterpoort/Helpman), Den Haag (Bomen- en
Bloemenbuurt) and Leidsche Rijn, 2026-10-06:

- **Planted versus hard matches the photograph** — schoolyards and car parks
  come out hard, lawns and planted back gardens green. Eyeball estimate 85–90 %
  per pixel in sunlight; there is no ground truth.
- **Lawn versus shrub versus tree does not.** A Den Haag block interior that is a
  canopy of crowns in the photograph classifies as lawn. AHN's height (DSM − DTM)
  cannot rescue it: AHN4 is flown leaf-off and the 0.5 m raster interpolates
  through bare crowns, and the lawn/tree split moved 20 points on one filter
  setting. So only the two-way split is made; trees stay with the layers that
  already draw them.
- The index is `(R − G) / (R + G)` of the infrared product alone — its red
  channel is near-infrared, its green is red light (the RGB product's red
  correlates 0.995 with it, same capture, no offset). Threshold **0.10** on the
  **2025 summer 25 cm** flight (`2025_ortho25IR`). It is an uncalibrated JPEG,
  so another year's flight needs its own threshold. The 2026 spring HR flight is
  sharper but leaf-off with long shadows, and reads as hard ground.
- JPEG stores colour at reduced resolution, so the index is only good to ~1 m:
  the split is voted on 4×4-pixel (~0.9 m) cells, smoothed once by 3×3 majority,
  and a plot under 8 % or over 92 % planted is left whole.

**The known error:** the 25 cm photograph is not a true orthophoto, so a tall
building leans over the strip of garden behind it and its roof reads as hard
ground. Seen beside Leidsche Rijn's apartment blocks. Those strips were drawn as
paving before, so this is no worse than it was; it is left in rather than guessed
around.

## Cost

The whole of Groningen (`--area groningen`): 17,124 gardens split across 49
cells, the whole `roofs` step taking **589 s** with the second photograph
included (run in this container, 2026-10-06). How that divides between the
fetch and the split was not measured.

**What it ships, and the crumbs it used to.** The first run wrote 4.76 MB of
gardens into Groningen's centre cell (`roofs/14/8490/5320.json`, 0.87 MB
before): 21,563 pieces and 186,000 vertices for 1,824 plots, and 16,581 of those
pieces under 4 m². The staircase tolerance (2 px) sat under the ~2.8 px a
diagonal one-metre staircase stands off its line, so nothing was straightened,
and the cut left a crumb wherever the outline ran along the plot's edge. The
phone parses and triangulates every one, which made loading visibly slower.
Now the tolerance is one classification cell (4 px, ~0.9 m) and a piece under
~4 m² goes to the other side (both sides recomputed from the plot, so they still
cover it once). A plot the photograph shows all hard is not written at all — the
client draws an unsplit garden from the `bgt` file as hard ground, which is the
same picture — and neither is a plot that shares its first vertex (the lookup
key) with a neighbour, since a split under a shared key lands on both. Same
cell, 2026-10-06: **1.54 MB, 754 gardens, 3,446 pieces, 60,000 vertices**; the
whole file 2.2 MB (455 kB compressed).

Rejected, measured the same day: **shipping only the planted side** and letting
the client derive the hard one as plot minus planted. It halves the gardens to
0.75 MB, but the overlay costs ~120 ms of worker time per cell, twice (meshes
and the far image), against ~10 ms to parse the half it replaces — a phone is
short of CPU while the map loads, not of bandwidth. And **10 m² crumbs** saved
another 0.2 MB by deleting real 3 × 3 m lawns.

`gardens::real_area::plan_of_real_gardens` (ignored, live network) draws the
split over the photograph for any `ATLAS_GARDEN_BBOX` — the way to judge a new
flight or a new threshold by eye.

Imagery: Beeldmateriaal Nederland via PDOK, CC BY 4.0 — credited in the
`roofs` coverage's attribution.
