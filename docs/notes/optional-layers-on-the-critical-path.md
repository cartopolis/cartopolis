---
topic: optional-layers-on-the-critical-path
triggers: [OPTIONAL_LAYER_TIMEOUT, BREAKER_COOLDOWN, "http::Breaker", should_fetch, spawn_geometry_task, nav_marks.rs, crossings.rs, api.pdok.nl, "third party", DEFAULT_TIMEOUT]
updated: 2026-09-02
---

# An optional layer must cost its own layer, not the map

`map_geometry::spawn_geometry_task` fetches a cell's tile, then awaits several
per-cell datasets, and only then submits the tessellation job. Everything the
cell is made of — ground fills, roads, water, bridges, trees, street labels,
building footprints — is behind that last await. Two of those datasets are
`api.pdok.nl`'s: the navigation marks (`systems::nav_marks`) and the level
crossings (`systems::crossings`). They are decoration, they are optional, and
they are owned by somebody else.

Healthy, they are cheap. A cold start over Groningen makes 18 requests to the
marks service (9 cells × 2 collections): **1.70 s in total, median 82 ms, worst
186 ms** — about 0.17 s per cell (measured for #267, `CARTO_CACHE_DIR` pointed at
an empty directory). That is not the problem. The problem is the other end: on
the inherited `DEFAULT_TIMEOUT` a service that accepts a connection and then goes
quiet costs **20 s per request**, the native fetch is a blocking `ureq` call that
holds one of at most eight `IoTaskPool` threads for all of it, and the surface
stream allows eight cells in flight — so a PDOK incident does not read as "no
buoys today", it reads as no map.

## The rule

A per-cell await that reaches a host we do not own, for data the cell can be
drawn without, carries two bounds. Both live in `platform::http` so a third such
layer copies nothing:

* **`OPTIONAL_LAYER_TIMEOUT`, 3 s.** Sixteen times the worst request observed
  above, so a service that is merely slow still delivers, and a dead one costs
  three seconds instead of twenty.
* **An `http::Breaker`, 60 s.** One instance per *service*, not per host: RWS
  publishes the marks and ProRail the crossings, they answer from the same
  `api.pdok.nl`, and they can fail apart. Any fetch or parse failure trips it;
  while it is open the layer's `should_fetch` answers `false` and no request is
  issued. The cooldown matches `tall_structures::RETRY_AFTER_SECS`, which is the
  in-tree precedent for a third party saying no.

The breaker exists because failure is otherwise **unremembered**: neither layer
writes a cache entry on a failed answer (deliberately — a poisoned key is never
re-fetched, so half a cell cached under one is worse than a cell asked for
again), and `map_geometry` re-runs the whole task on a re-anchor, a terrain-field
revision, a coverage revision or any detail-layer toggle. Without the gate every
retear re-pays the full wait for every cell in the ring.

Two constraints on the gate itself, both load-bearing:

* Its clock is **`web_time`** — `std::time::Instant::now()` panics on wasm, and
  wasm is the one platform where the breaker is the *only* bound, since `ehttp`
  ignores `Request::timeout` in the browser (a pre-existing gap shared with
  `transit_vehicles`, `weather` and `globe_weather`).
* The decision is a **pure predicate over an injected instant**
  (`Breaker::is_open_at`), and each layer's `should_fetch` takes the breaker and
  the instant as arguments. "No request was issued" is not observable from the
  other side of an `async fn` that returns the same empty cell either way, and a
  test that reached for the wall clock would have to sleep a minute to see the
  far side of the cooldown.

The cache is read **before** the breaker is consulted, and the cell's own gates
(coverage, water/rail) before both. A cell already on disk costs the service
nothing, so an outage must not blank cells that were complete before it started;
a foreign or dry cell must cost neither a request nor a storage lookup.

## What this does not fix

**Native concurrency.** `map_geometry` awaits the two layers through
`futures_lite::future::zip`, and `load_marks_cell` does the same with its two
collections, but `http::fetch` is a blocking call with no await point in it on
native, so the requests still land in sequence there. The overlap is real only on
wasm. Natively what caps a cell's wait is the 3 s bound per request plus the
breaker, and the worst case for a first cell during an outage is the three
requests end to end. Making it genuine would mean spawning each fetch as its own
pool task, which trades a shorter wait for twice the peak thread occupancy in the
pool this whole note is about — not obviously the right trade, and not measured.

## Directions left open

* **Take the layers off the critical path**, as `systems::tall_structures` does —
  build the cell, add the marks when they land. Not symmetric between the two:
  crossings are *placed* against the parsed tile's `streets` layer, so a
  late-arriving crossing cell needs the parse back, while a mark does not. That
  is a restructure, not a bound.
* **Fetch each register once instead of per cell** — #212 measures the whole
  national marks register at ~2.1 MB in the client's compacted form, which would
  delete both awaits along with both gates.

Neither is foreclosed by the bounds above.
