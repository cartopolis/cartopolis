---
topic: routing
triggers: [routing.rs, edge_cost, astar, "junction penalty", "speed table", RoadKind, speed_kmh, heuristic, admissible, "why this route", "rat run", "route is slow", cost_model_bench]
updated: 2026-08-08
---

# What the router is actually minimising

`RoadGraph::astar` minimises **time**, and time is two things: the edge
(`length / class speed`) and the junctions on the way
(`junction_delay`). Facts about that model which the code cannot state on its own.

## The junction cost is what tells a through-road from a grid

Before 2026-08-08 the cost was purely `length / class speed`, so a kilometre of
residential street cost the same whether it crossed two side roads or twenty. The
only thing keeping a route off the back streets was the class speed itself, and
between two roads of the *same* class nothing distinguished them at all — the
router took the shortest, which is the rat run.

The delay is charged **on arrival at a junction node, against the class of the
road you arrive on** (`RoadKind::junction_share`). Priority is the whole model: a
car on a primary crosses a side street without slowing, the same car on that side
street waits to get out, and a motorway pays nothing because its "junctions" are
slip roads the geometry already charges for. Mode scales it again — a driver waits
for a gap, a walker barely breaks step. Charging all three modes equally would
push walking routes onto arterials, which is exactly backwards.

**The junction mask is mode-dependent and has to be.** A residential street
crossed by a footpath is a junction for a walker and a straight run for a driver;
a mode-agnostic node degree would charge a car a give-way at every zebra crossing
in the city.

### Measured, 2026-08-08, Groningen, live tiles (`cost_model_bench`)

Same four corridors, junction delay off → on. Distances are essentially unchanged
— the *routes* barely move on these pairs — so what this buys on a city corridor
is mostly a truthful clock, with the ranking effect showing up where two roads of
one class compete:

| corridor | car before | car after |
|---|---|---|
| centre → station (2.3 km) | 32.4 km/h | **27.9 km/h** |
| centre → Paddepoel (3.8 km) | 31.5 km/h | **26.5 km/h** |
| centre → Ring West (5.0 km) | 39.8 km/h | **35.5 km/h** |
| centre → Hoogkerk (11 km) | 53.5 km/h | **51.1 km/h** |

The gradient is the point: the most urban corridor lost 14% and the
trunk-dominated one 4.5%. Bikes went 14–17 → 11–15 km/h, walking was unchanged.
Every figure is *more* plausible than before, which is why the class speed table
was **not** recalibrated to compensate — the speeds already read as free-flow
enough for the junction cost to sit on top of.

`JUNCTION_S = 6.0` s is the give-way case for a car. It is a judgement, not a
measurement; the bench is how to re-judge it.

## The heuristic is bounded by the graph, not by the mode

`TravelMode::max_speed_ms` is the mode's global ceiling — 130 km/h for a car. A
city corridor holds nothing above 55, so dividing the straight-line distance by
the global ceiling made the heuristic roughly half what it could be and A*
degenerated toward Dijkstra over the whole corridor. `RoadGraph::top_speed_ms`
takes the maximum over the edges actually present, which is still admissible (no
edge can beat an edge-wise maximum) and as tight as the data allows.

Measured on the same corridors: identical routes (an admissible heuristic cannot
change the optimum), warm-search time down 25–40% on the larger ones (Hoogkerk
walk 24 → 15 ms, bike 29 → 22 ms). Small in absolute terms at city scale — the
solve is tens of milliseconds against hundreds for the tile fetch — and it matters
where corridors get long.

Adding the junction delay does not threaten admissibility: it only ever *adds* to
the real cost.

## The reported duration must include what the search paid

`build_route` adds the same junction delay to each hop's time. Leaving it out
would under-report every city ETA and, worse, report a *time* for a route that was
chosen to minimise a different number.

## Deliberately not modelled

- **Turn angle.** A left across oncoming traffic costs more than a right. Modelling
  it means an edge-expanded (turn-graph) search, which is a different data
  structure, not a different constant.
- **Traffic signals.** Not in the Shortbread tiles at all — `pois` has no
  `highway=traffic_signals`. The junction delay is the average of signalised and
  unsignalised, because nothing here can tell them apart.
- **Turn restrictions.** OSM relations; not in vector tiles in any form.
- **Live traffic.** No source.
</content>
