# ADR-023: Data Layers (Togglable OSM Point-of-Interest Overlays)

---
**Metadata:**
- **ID**: ADR-023
- **Status**: Accepted
- **Date**: 2026-07-10
- **Tags**: [osm, data-layers, rendering, ui, import]
- **Decision Type**: architecture_pattern
- **Aligns with**: [G-04, P-03]
- **Roadmap tier**: P1
- **Doc resolution**: R1
- **Related**: [ADR-004, ADR-020, ADR-022]
- **Supersedes**: []
- **Superseded By**: []
---

## Context

**Problem**: The world renders OSM basemap tiles (ADR-004) and 3D buildings
(ADR-022), but carries no thematic point data — transit stops, businesses,
amenities. We want an open-ended set of such overlays that the user can switch
on and off individually, without each one re-inventing fetching, caching,
streaming and disposal.

**Requirements**:
- Data must be **open** and redistributable, with attribution we can satisfy.
- Layers must stream with the camera at planet scale (no global preload) and
  respect the floating-origin frame (ADR-019).
- Adding a layer should be a declarative entry, not a new subsystem.
- Toggling a layer off must be cheap; toggling it back on must be instant.
- The upstream API must not be hammered — it is a free, shared community service.

## Decision

Introduce a **client-side data-layer registry** (`systems/data_layers.rs`), sourced
from the **OSM Overpass API**. Shipped layers: **transit stops** and **shops**.

**Approach**: A `LAYER_DEFS` table declares each layer as `{key, name, Overpass
query with a {bbox} placeholder, categories with marker styles, classify fn}`.
Shared machinery streams every *enabled* layer per fixed zoom-14 cell within
`LAYER_CELL_RADIUS` (3) of the camera: disk-cached Overpass fetch → POI classify →
one merged marker mesh per cell, all on a worker thread; the main thread only
spawns entities. Disabling a layer flips `Visibility` rather than despawning.

## Rationale

**Primary Reasoning**:
1. **Overpass is already a dependency** (ADR-022), so this adds no new vendor,
   crate, credential, or rate-limit budget to reason about — only new queries.
   Its data is OpenStreetMap: worldwide, ODbL, redistributable with attribution.
2. **The building far-cell streamer is a proven template** for exactly this
   shape of problem (fetch-once/keep-while-in-range/anchor-to-BigSpace-cell), so
   the layer streamer reuses its cache layout, throttle, and cell geometry.
3. **Transit stops are the right first layer**: dense in Dutch cities, node-only
   (no geometry assembly), and split into tag-obvious categories that exercise the
   styling path. Shops followed as a denser, pin-headed second layer.

**Alternatives Considered**:

| Alternative | Pros | Cons | Rejected Because |
|------------|------|------|------------------|
| NL-only sources (NDOV/OVapi, CBS, BAG) | Richer, authoritative, real-time departures | Netherlands-only; each has its own schema, endpoint, and terms | Layers must work worldwide; a NL-only first layer would bake NL assumptions into the registry's shape |
| Wikidata / Wikimedia SPARQL | Worldwide, CC0, rich entity links | Uneven street-level geo coverage; public SPARQL endpoint is rate-limited | Coverage too thin for the street-level POIs this layer set targets |
| Overpass, one query per POI type | Simple per-layer queries | N× the requests against a shared free service | Union queries fetch a whole layer's categories in one request per cell |
| Bake POIs into the SQLite world DB server-side | No client network path; server can enrich | Requires an import pipeline and schema before we know what layers we want | Premature; revisit once layer set stabilises (see Consequences) |

## Consequences

**Positive**:
- A new layer is one `LAYER_DEFS` entry plus a `classify` fn (~20 lines).
- Cells are cached to `cache/layers/{key}/{z}/{x}/{y}.json`, so a revisited area
  costs no network and no throttle wait.
- Markers are unlit vertex-coloured meshes, one draw call per cell, legible
  against both the day basemap and the night city.

**Negative**:
- **Free-service load**: layer fetches share the Overpass budget with buildings.
  Mitigated by the same 800 ms inter-query throttle, the disk cache, bounded
  retries (5 rounds per cell, with backoff), and only streaming enabled layers.
- **Attribution obligation**: ODbL requires credit. The Data Layers panel
  carries "Data © OpenStreetMap contributors (ODbL), via Overpass API".
- **Client-side fetching** means each client queries Overpass independently. If
  the layer set grows or player counts rise, move the fetch behind `vibe_sim`
  (a shared cache and one upstream identity) — that would supersede this ADR.
- **Node-only POIs**: stops mapped as ways/relations (large station areas) are
  skipped. Acceptable for v0; `out center;` would cover them later.

## Related

- [ADR-004](./004-osm-tile-integration.md): Establishes OSM as the basemap source and the tile/cache addressing this reuses.
- [ADR-020](./020-map-tier-and-sim-tier-world-model.md): Data layers are map-tier decoration, not sim-tier state.
- [ADR-022](./022-osm-building-import.md): Supplies the Overpass client, throttle, disk-cache layout, and cell-streaming pattern reused here.
