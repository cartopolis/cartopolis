# ADR-037: Routing on the Vector Tiles

---
**Metadata:**
- **ID**: ADR-037
- **Status**: Accepted
- **Date**: 2026-07-23
- **Tags**: [routing, navigation, map, vector, mvt, ui, offline, data-layers]
- **Decision Type**: architecture_pattern
- **Aligns with**: [G-04, P-03, P-01]
- **Roadmap tier**: P1
- **Doc resolution**: R2
- **Related**: [ADR-035, ADR-020, ADR-023, ADR-025, ADR-034, ADR-033]
- **Supersedes**: []
- **Superseded By**: []
---

## Context

**Problem**: The map now streams **Shortbread-schema MVT** (ADR-035), which means the client
already holds the road network as *geometry with attributes* rather than as pixels — `streets`
carries `kind`, `oneway`, `oneway_reverse`, `bicycle`, `surface`, and `street_labels` carries
`name`. A map you can search and fly around but not ask "how do I get there?" is missing the one
thing every map application is expected to do, and the data for it is already being fetched and
cached on disk.

**Requirements**:
- Plan a route between two points for driving, cycling and walking.
- Reuse the tiles the map already streams — no second data source, no extra bytes for an area
  the user has already looked at, and no new backend to operate.
- Work offline for any area whose tiles are in `cache/tiles`, matching the client's
  offline-capable posture (ADR-033's web target has the same constraint in a browser cache).
- Bounded cost. Tile fetching is the app's dominant byte volume (ADR-035) and mass GPU churn is
  its dominant crash (the `VK_ERROR_DEVICE_LOST` pattern), so a route must not be able to trigger
  an unbounded fetch or an unbounded mesh burst.
- Turn-by-turn output, with street names, that a person can follow.

## Decision

**Route client-side, on the streamed vector tiles**, with an A* over a graph stitched from the
`streets` layer, and a mobile-navigator-style planner panel driving it.

**Approach**:

- `systems/routing.rs` — pure, Bevy-free: MVT → `RoadGraph` → A* → `Route` (polyline, distance,
  duration, turn list). Fully unit-tested against the checked-in Groningen z14 fixture.
- `systems/navigation.rs` — the Bevy half: the corridor's tile fetches on a worker thread, the
  drawn route (a mitred ribbon plus start/finish discs, under the `BigSpace`), and the panel.
- The planner is a right-hand panel (`RightPanel::Route`), reached from a 🧭 button beside the
  address bar, laid out the way a mobile navigator is: mode tabs, a two-field endpoint card with a swap,
  one summary line, then the turn list. Each endpoint resolves from the current position, a
  Nominatim search, or a click on the map.

Five decisions carry the design:

**1. Node identity is exact, not fuzzy.** Every tile at a zoom shares one integer lattice
(`global = tile_index · extent + local`). A way crossing a tile border is clipped into both tiles
with a buffer, so the same OSM vertex quantises to the *same* global integer in both. Stitching
tiles therefore needs no distance tolerance and no way ids — which the tiles do not carry.

**2. The graph must be noded.** Shared vertices are necessary but not sufficient: Shortbread
merges ways by attribute and generalises each merged feature independently, so where a side street
tees into a main road the tee is an interior vertex of the main road's line and simplification may
drop it. Measured on the fixture that is **14% of all nodes** left lying *on* a segment while
being a vertex of neither, and it shatters the network — the largest connected car component
covered **58%** of usable nodes. `RoadGraph::finish` splits every segment at the nodes lying on it
(within ~0.55 m), taking that to **92%** (walking 60% → 96%). This failure is silent: without it,
ordinary cross-town routes simply return `NoPath`.

**3. Endpoints snap to the routable core, not the nearest node.** The nearest node to an address
may sit on a genuine one-way street whose forward-reachable set, for that mode, is a tiny dead-end
pocket — a car starting there could arrive but never leave, and the route returned `NoPath` even
though a road ran past the door (the same pair routed fine for bikes, which ignore one-ways). In a
road network the drivable through-network is one giant strongly-connected component and the stubs,
service loops and one-way pockets are tiny ones, so `route` computes the largest SCC of the
mode-filtered graph (`routable_core`, iterative Kosaraju) and snaps **both** endpoints into it.
Both in one SCC ⇒ a path exists, so any surviving `NoPath` is a genuinely disconnected corridor.

**4. The tile corridor is a capsule, capped, and refused by distance.** Tiles within a half-width
of the straight line, where the half-width grows with `√direct` (**not** linearly — a linear term
made a long route both long and wide, and a cross-province query asked for ~700 tiles). A route
whose corridor exceeds `MAX_ROUTE_TILES` is **refused with its straight-line distance** (`TooFar {
km }`, against an advertised `MAX_ROUTE_KM` reach), because the kilometres are the thing the user
chose — a raw tile count is not actionable. This is the design range limit, not a bug: routing the
long haul is the coarse-zoom router's job (see *Consequences*). The graph is cached against its
tile list, so switching travel mode re-runs only the A*.

**5. Modes are access filters over one graph.** Cars are barred from footways, cycleways and bus
lanes and bound by one-ways; bikes are barred from motorways and trunk roads; walkers use
everything but those and rails. One-ways bind cars only — the tiles carry no `oneway:bicycle`, and
honouring them for bikes invents detours no rider makes in a dense city centre.

The turn list only emits an instruction where the network **branches** (a node with ≥ 3 usable
neighbours) or the street name changes, and measures the angle over a 25 m window either side of
the junction. Without both, a curving street narrates as "bear right, bear left, bear right…" for
300 m — the graph has a node per vertex, and curvature is not a decision.

## Rationale

**Primary Reasoning**:

1. **The data is already here.** Routing on the basemap costs no new fetches for anywhere the user
   has been, no new service to run, and it inherits the offline behaviour of the tile cache. This
   is P-03 (locality) applied to a capability, not just to bytes.
2. **It is the payoff for ADR-035.** Going vector was justified by size and by lit geometry; being
   able to route is the third dividend, and it is unavailable to a raster basemap at any price.
3. **A router is latency-sensitive and the network is not.** A local A* over a warm cache answers
   in single-digit milliseconds (measured: 1–4 ms for a 1.5 km city route once the graph is
   built), which is what lets the mode tabs re-solve on click.
4. **Bounded by construction.** One worker thread, a capped corridor, a cached graph, and exactly
   four mesh entities per route — none of the streaming hazards this codebase has been bitten by.

**Alternatives Considered**:

| Alternative | Pros | Cons | Rejected Because |
|------------|------|------|------------------|
| Self-host OSRM / Valhalla and call it | Best-in-class routing quality; turn restrictions, traffic profiles, alternatives | A second service to build, feed and operate; a second copy of OSM data; no offline; a network round-trip per re-solve | The whole point of ADR-035 was to stop depending on someone else's derived-data service. Re-adding one for routing, over data we already ship, inverts that. |
| Public routing API (OSRM demo, GraphHopper) | Zero work | Usage policies at least as strict as OSM's tile policy; a browser cannot identify itself (ADR-033); no offline | Same objection ADR-035 raised against the OSM tile CDN, for the same reasons. |
| Route on the Overpass detail patches (ADR-022) | Real OSM ways with ids and full tags — no noding needed | Only covers the small patch around the camera; an Overpass call per route; nothing outside the patch exists | Cannot answer a route that leaves the current patch, which is most routes. |
| Ship a prebuilt routing graph alongside the tiles | Compact; fast; correct topology | A second artefact to build and version; another 60 GiB-class pipeline; drifts from the tiles | Not worth a second pipeline while the tiles already imply the graph — revisit if the noding proves insufficient in other regions. |
| A bounding box instead of a corridor | Trivial | A diagonal route fetches the whole square it spans | 3–4× the tiles for the routes where tiles are most expensive. |

## Consequences

**Positive**:
- Routing works offline, wherever the tiles are cached, for all three modes.
- No new dependency and no new service: `std::collections::BinaryHeap` over the existing
  `mvt-reader` decode.
- Mode switching is instant (cached graph), and the fetch/solve split is visible in the panel
  rather than a bare spinner.
- The planner's endpoint plumbing (search, my-position, pick-on-map) is a reusable place-picking
  affordance for later features.

**Negative**:
- **Range is capped at ~`MAX_ROUTE_KM` (≈ 22 km).** Beyond `MAX_ROUTE_TILES` the planner refuses,
  reporting the distance. This is inherent: a route that far needs street-detail tiles for the
  whole corridor, and the through-line alone is dozens of tiles. *Mitigation and the named next
  step*: route the long haul over the **coarser zooms**, where Shortbread keeps the major-road
  network at a fraction of the tiles — a hierarchical search (coarse zoom for the middle, the top
  zoom near the endpoints) is the standard answer and lifts the ceiling without lifting the byte
  budget. Raising the constant is not the fix.
- **The graph is inferred, not authoritative.** No turn restrictions, no barriers, no access tags
  beyond `bicycle`, no speed limits — the speed table is a ranking, not a promise, and generalised
  geometry costs a little distance accuracy. Routes are plausible and followable, not
  survey-grade. *Monitoring*: the fixture tests pin connectivity share and the mode asymmetries;
  the `solves_a_real_route` manual check prints a full turn list against the live source.
- **Noding is load-bearing and its absence is silent** (`NoPath`, not an error). *Mitigation*:
  `finish` is the only way to get edges at all — a graph that skipped it has none — and
  `noding_reconnects_the_network` pins both halves of the property.
- **A cold corridor is a real wait**: up to 96 tile fetches at ~500 KB. *Mitigation*: the corridor
  is fetched nearest-endpoint-first and the panel reports `n/total`; in practice the map streamer
  has already cached the area the user is looking at.
- Routing requires a vector tile source; against a raster template the planner says so instead of
  failing obscurely.
- **The drawn line is subject to the codebase's standing coplanar-ground hazard.** It is not part
  of `SurfaceGroup`'s stack but competes with it, the road surfaces and the buildings, and with
  `near = 0` against a 200 km far plane the depth buffer cannot resolve a tenth of a metre at
  street range — at 0.12 m the line rendered as horizontal bands. It sits at 0.5 m with a
  `depth_bias` well clear of the ground stack's. *Monitoring*: `ShotMetrics` gained
  `route_meshes`/`route_rendered`, which separates "not spawned" from "spawned but not rendered"
  without a recompile, the same job the globe counts already do.

## Related

- [ADR-035](./035-self-hosted-map-tiles.md): supplies the tiles, the decode path and the on-disk
  cache this builds on. Routing is the capability that justifies the schema choice.
- [ADR-020](./020-map-tier-and-sim-tier-world-model.md): the anchored Mercator frame the drawn
  route is placed in; a re-anchor rebuilds it.
- [ADR-025](./025-planetary-globe-and-space-tier.md): the route carries `HideInSpace` with the
  rest of the ground detail.
- [ADR-023](./023-data-layers.md): the sibling pattern of an OSM-derived overlay with its own
  panel and its own streaming budget.
- [ADR-034](./034-public-data-relay.md): the relay pattern this deliberately does *not* need,
  because there is no upstream to adapt.
