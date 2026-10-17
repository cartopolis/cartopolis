---
topic: routing
triggers: [routing.rs, cartopolis_geo::routing, RoadGraph, routable_core, build_route, is_junction, junction_mask, NODE_TOL, TURN_WINDOW_M, MIN_STEP_M, noding, NoPath, "turn list", "turn instructions", "bear right", solves_a_real_route, CARTO_ROUTE_FROM, CARTO_ROUTE_TO, "--ui route", "Routing needs a vector tile source"]
updated: 2026-10-17
---

# The routing graph: noded, snapped into its core, narrated at junctions

`cartopolis_geo::routing` (ADR-037, `crates/geo/src/routing.rs`) builds its graph
straight from the Shortbread `streets` layer the map already fetches. Three
properties make that graph routable at all, and the absence of each fails the
same way: an ordinary route comes back `NoPath`, or as a turn list nobody could
follow, with nothing in the log. Bridges and tunnels are
[grade-separation.md](grade-separation.md), the cost being minimised is
[routing-cost-model.md](routing-cost-model.md), and anything past ~22 km is
[long-routes.md](long-routes.md).

## Seams need no tolerance; tees do

Every tile at a zoom shares one integer lattice (`global = tile · extent +
local`), so a way clipped into two tiles quantises to the *same* global node in
both. Stitching tiles is exact node identity, with no distance tolerance.

The tee is what is not exact. Shortbread merges ways by attribute and generalises
each merged feature on its own, so a side street's junction is an interior vertex
of the main road's merged line — and simplification is free to drop it. The side
street then ends exactly *on* a segment that has no vertex there.

**Measured on the Groningen z14 fixture** (pinned by the module tests): 14 % of all
nodes lie on a segment while being a vertex of neither, and the largest connected
car component covered **58 %** of the usable nodes. `RoadGraph::finish` splits every
segment at the nodes lying on it before any edge is emitted, which takes the car
component to **92 %** (walking 60 % → 96 %). Skipping it is not an error anybody
sees: cross-town routes simply return `NoPath`.

The tolerance is `NODE_TOL`, 1.5 lattice units (~0.55 m), chosen from the data:
widening it from 0.5 to 1.5 units picks up another 273 on-segment nodes, and going
on to 3.0 finds just 28 more, so 1.5 sits on the flat of the curve — past the real
coincidences, short of fusing roads that merely pass close. Which of those splits
are allowed at all (dangles only, same grade) is grade-separation.md.

## Snap into the routable core, not onto the nearest node

`RoadGraph::route` snaps both ends into `routable_core`: the largest strongly
connected component of the mode-filtered directed graph (iterative Kosaraju — a
corridor's chains are too deep for a recursive DFS).

The failure that forced it: a car snapped to the geometrically nearest node landed
on a genuinely one-way residential street (`oneway=yes` in the source, so the parse
was right) whose forward-reachable set was a three-node dead-end pocket. The
cross-town route returned `NoPath`, while the same pair routed for bikes, which
ignore one-ways. In a road network the drivable through-network is one giant SCC
and the stubs, service loops and one-way pockets are tiny ones, so two ends in the
largest SCC guarantee a path exists. A stub a few metres off the through-road is
never a valid start.

Two consequences live in other notes: "no road near the point" and "a road, but not
in the core" are different errors (routing-cost-model.md), and a graph holding two
disjoint patches has only one core (long-routes.md).

## A step begins only at a real junction

The graph has a node per vertex, so a curving street is a run of small direction
changes, and a per-vertex classifier narrates it as "bear right, bear left, bear
right…" for 300 m. `build_route` needs two rules to stop that, and either alone
still narrates curves:

- A step may begin only where the network branches for the mode — `is_junction`,
  three or more distinct neighbours the mode may use, so a footpath crossing a
  residential street is a junction for a walker and a straight run for a car — or
  where the street name changes.
- The turn angle is measured over `TURN_WINDOW_M` (25 m) either side of the junction,
  not between the two touching segments: junctions are often modelled with short
  stubs whose own direction says nothing about the manoeuvre.

A last pass folds steps shorter than `MIN_STEP_M` (25 m) into the one before.

## It needs a vector source

A raster basemap carries no geometry to route on. Against a raster
`CARTO_TILE_URL` the planner fails at once with "Routing needs a vector tile source
(CARTO_TILE_URL)" (`navigation::drive_route_solver`) instead of spinning on an
empty graph. A scripted capture meets this whenever its manifest does not pin a
vector `tile_url` — the built-in fallback is the raster basemap.

## Reproducing a route

- `cargo test -p cartopolis solves_a_real_route -- --ignored --nocapture` solves
  against live tiles (default `tiles.cartopolis.org`, or `CARTO_TILE_URL`) and
  prints the full turn list.
- `CARTO_ROUTE_FROM` / `CARTO_ROUTE_TO` ("lat,lng") override the `--ui route` demo
  seed (53.2194,6.5665 → 53.2109,6.5642, `shot_harness.rs`), which reproduces a
  specific route in a headless capture.
