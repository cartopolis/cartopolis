---
topic: routing
triggers: [plan_corridor, route_staged, LONG_HAUL_ZOOM, Corridor, "long route", "too far", MAX_ROUTE_TILES, MAX_STAGED_KM, coarse_zoom_probe, "cross-country", "zoom hierarchy"]
updated: 2026-08-08
---

# Routing further than one zoom can reach

Street-level routing tops out around 22 km, because the z14 corridor for anything
longer blows past `MAX_ROUTE_TILES`. `plan_corridor` drops a tier instead of
refusing: a coarse corridor for the haul, a detailed patch at each end, spliced.

## The tile source has a road hierarchy across zooms

**Surveyed 2026-08-08** against the VersaTiles Shortbread build, by dumping the
`streets` layer's `kind` dictionary per zoom:

| zoom | classes present |
|---|---|
| z8 | motorway, trunk, primary |
| z10 | + secondary, tertiary, residential |
| z12 | + unclassified, service, busway |
| z13 | + footway, cycleway, path, steps, pedestrian, living_street, track |
| z14 | everything |

Two properties do **not** survive generalisation, and both were checked rather
than assumed:

- **`oneway` is z14-only** (confirmed on Groningen *city-centre* ancestor tiles,
  which certainly have one-ways). A coarse graph is therefore effectively
  undirected. Acceptable on the haul — a dual carriageway is two separate ways
  anyway — and not acceptable in a city, which is why the ends stay at z14.
- **`bicycle` is z14-only** too, and z10 carries no cycleways or footways at all,
  so a long bike or walk route is planned on roads. It shows: Groningen →
  Amsterdam comes out 219 km by bike against 184 by car.

## Why z10, and why only two tiers

**Measured, live tiles, one corridor per zoom** (`coarse_zoom_probe`):

| route | z14 | z12 | z10 |
|---|---|---|---|
| Groningen → Assen (26 km) | refused | 28 tiles, 34 km | 12 tiles, **30 km** |
| Groningen → Zwolle (95 km) | refused | 86 tiles, 101 km | 21 tiles, 106 km |
| Groningen → Amsterdam (180 km) | refused (~1,340 tiles / 648 MB) | refused | **35 tiles / 2.7 MB, 180 km in 1 h 55** |

z12 is not a useful middle rung, and the reason is not obvious: at the same tile
budget a *coarser* corridor is **wider in ground terms**, so z10 both costs less
and has more room to find the motorway. Assen came out 30 km at z10 against 34 km
at z12. Two tiers, not four.

## The detailed ends are not a refinement — they are the feature

A coarse-only long route snapped its start **281 m** from the pin, every time.
Driver mode calls anything past 60 m off-route, so that route puts a driver off
route on the first frame and into a reroute it can never satisfy.

With the patches spliced in, the same route snaps at **25 m**, and Groningen →
Amsterdam by car reads 184 km in 2 h 04 — the real drive is about 184 km and two
hours.

## The two endpoint patches must be separate graphs

This is the trap. `RoadGraph::route` snaps into the **routable core**, which is the
largest strongly-connected component. Put both patches in one graph and the core
is whichever patch has more streets in it; the other end cannot be snapped to at
all, `route_staged` takes its fallback, and the only symptom is the endpoint snap
staying at its coarse 281 m. Nothing logs. The first cut did exactly this.

The same rule bites in tests: a fixture with two equal-sized components leaves
which one wins to Kosaraju's finish order.

## The graphs are spliced, not merged

Merging is the tempting design and it does not work. The lattices belong to
different zooms so there is no shared node identity to weld on, and the
generalised geometry of a road sits tens of metres from its detailed geometry —
far outside the sub-metre tolerance `RoadGraph::finish` nodes with. Welding would
mean inventing transfer edges and letting the search hop between two
representations of the same road wherever it liked.

Splicing routes instead fails *visibly* (a seam at the handover) rather than
subtly (a zig-zag). Two approximations are deliberate and documented in the code:
the coarse middle's duration is taken as its share of the coarse route's total,
because coarse steps are not indexed by polyline position; and the middle's steps
are selected by distance from the pins rather than by index, which is safe only
because a route long enough to be staged never comes back within `HANDOVER_M` of
its own ends.

## Reach

Pinned by a test, not derived — the gate counts grid cells, not area. Measured
2026-08-08: **22 km** direct (matching `MAX_ROUTE_KM`), **360 km** staged
(`MAX_STAGED_KM`), and past that `TooFar { km }` quotes the staged figure.
