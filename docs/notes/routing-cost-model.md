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

## Ferries carry everyone but the car, on purpose

Shortbread ships a separate `ferries` layer, present wherever there is a crossing
(**verified 2026-08-08** on the Westerschelde z14 tiles — its absence from the
Groningen fixture is a fact about Groningen, not about the schema). It carries
`kind = ferry` and a `name`, and **nothing that separates a car ferry from a
passenger pontje**.

Every car ferry takes foot passengers and no foot ferry takes cars, so foot and
bike are safe on any crossing and a car is safe on none. That asymmetry is the
whole rule: routing a driver to a crossing their vehicle cannot board is a failure
they discover at the water, and it is worse than the detour they would otherwise
be given. Same reasoning as refusing the implicit `maxspeed` zone codes.

The speed (8 km/h) is an *effective* one, not a sailing speed: what dominates a
crossing is the wait, and there is no timetable in a vector tile. It puts a 400 m
pontje at 3 minutes and a 4 km estuary at half an hour. A fixed boarding penalty
would model it better and cannot be expressed — a ferry line arrives as many
segments and the penalty would be charged once per segment.

Measured, live tiles: Vlissingen → Breskens (5.3 km straight, all water) routes
for bike at 10 km / 1 h 05 and on foot at 9.8 km / 1 h 32; the car is refused.

## "No road near the destination" must mean there is no road there

Snapping into the routable core can fail two ways and they used to look the same.
The ferry work surfaced it: a car asked to cross an estuary snapped fine
geometrically, but the far shore is its own component — only the ferry links them
and a car may not use one — so the core snap failed and the planner announced
there was no road in a town full of streets.

`route` now distinguishes them: no usable node nearby at all is still
`NoStart`/`NoEnd`, and a usable node that merely isn't in the same component is
`NoPath`. Note when testing this that **the core is the largest component**, so a
fixture with two equal-sized islands leaves which one wins to Kosaraju's finish
order — the first draft of the test passed by reporting `NoPath` for the *start*.

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
