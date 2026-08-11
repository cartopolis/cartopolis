# ADR-027: Self-Hosted 3DBAG Buildings for Covered Areas

---
**Metadata:**
- **ID**: ADR-027
- **Status**: Accepted
- **Date**: 2026-07-19
- **Tags**: [buildings, osm, 3dbag, rendering, import, dependency, ops]
- **Decision Type**: architecture_pattern
- **Aligns with**: [G-04, P-03]
- **Roadmap tier**: P1
- **Doc resolution**: R1
- **Related**: [ADR-004, ADR-020, ADR-022, ADR-023]
- **Supersedes**: []
- **Superseded By**: []
---

## Context

**Problem**: OSM 3D buildings (ADR-022) carry sparse, guessed heights — most Dutch
footprints have no `height`/`building:levels`, so we estimate from type. The
Netherlands has an authoritative open dataset, **3DBAG** (TU Delft, from Kadaster
BAG + AHN LiDAR), with real per-building heights and ground level. Its live API
is slow, and downloading + parsing 3D CityJSON on the client is heavy.

**Requirements**:
- Real heights for covered NL areas without slowing the client or adding a heavy
  client-side geo dependency (no CRS/CityJSON parsing on the client).
- Reuse the existing building streaming/caching/rendering path (ADR-022) — not a
  parallel subsystem.
- Degrade cleanly to OSM everywhere the dataset doesn't cover.
- Open data, redistributable with attribution.

## Decision

**Preprocess 3DBAG server-side, self-host it as static per-cell files, and route
covered cells to it — OSM elsewhere.** Two tiers, both keyed on the client's
`CACHE_ZOOM`=14 cells and served from `https://3dbag.garage44.eu/`:

- **LoD2.2 real meshes** (primary for covered cells): the actual triangulated BAG
  roof geometry — sloped/hipped/complex roofs, not a guess.
- **LoD1.2 footprints** (fallback / wider-but-cheaper coverage): Overpass-shaped
  footprint + height for the client to extrude, reusing the ADR-022 pipeline.

**Approach**:
- **Server, LoD2.2** (`tools/3dbag_lod22.py`): fetch the 3DBAG **CityJSON** tiles
  for an RD bbox, parse each building's LoD2.2 `Solid`, drop the GroundSurface,
  triangulate every Wall/Roof surface (`mapbox_earcut` on the surface plane),
  reproject vertices RD→WGS84 (`pyproj`), and set each vertex height relative to
  the building's own base. Emit per z14 cell `lod22/{14}/{x}/{y}.json` as
  `{id, p:[[lat,lng,h]], t:[[i,j,k,roof]]}` + `lod22/coverage.json`.
- **Server, LoD1.2** (`tools/3dbag_extract.py`): page the WFS `BAG3D:lod12`
  layer, reproject footprints, derive `height`/`roof:shape`/`roof:height` from BAG
  attributes, emit **Overpass-shaped** `buildings/14/{x}/{y}.json` + a manifest.
- **Client**:
  - **LoD2.2** (`lod22.rs`): a camera-following streamer modelled on the building
    far-cell worker — fetches a covered cell, builds one merged mesh (wall/roof
    vertex colours; grey for near-flat roof triangles by slope), anchors it to the
    cell's BigSpace grid cell, **one mesh upload per frame** (iGPU device-loss
    guard). `osm_buildings` skips any cell `lod22_covers()` reports, so the real
    meshes are the only buildings there (no z-fight with extrusions).
  - **LoD1.2** (`osm_buildings.rs`): `fetch_cell` routes a covered z14 cell to
    `/buildings/14/{x}/{y}.json`, falling back to Overpass on any miss.
  - Coverage manifests load once (`CARTO_3DBAG_URL`, default the host above); cells
    disk-cached per source (`cache/buildings/{3dbag,lod22}/…`).

## Rationale

**Primary Reasoning**:
1. **Because the server emits the client's Overpass shape, `parse_buildings` and
   the whole extrude/window/far-cell pipeline (ADR-022) are reused verbatim** —
   the only client change is *where* a cell is fetched. No CityJSON, no CRS math,
   no mesh loader on the client.
2. **All NL-specific complexity (WFS paging, RD→WGS84, LoD1.2 height model) lives
   server-side and runs once per data release**, mirroring the self-hosted
   Overpass/Nominatim precedent (ADR-014, `server.md`). The client stays in
   lat/lng + metres, so the floating-origin frame (ADR-019) is untouched.
3. **Static hosting is far lighter than the live API or a database**: 3DBAG is
   download-once (≈yearly releases). Groningen is ~90k buildings / 44 MB.

**Alternatives Considered**:

| Alternative | Pros | Cons | Rejected Because |
|------------|------|------|------------------|
| Client fetches the live 3DBAG API | No hosting | Slow; CityJSON + RD parsing on client | Latency and client weight are the whole problem |
| Ship LoD2.2 meshes (real roofs) as 3D Tiles/glTF | True roof geometry | New client mesh/3D-Tiles path; bigger data | Larger change; LoD1.2 heights are the high-value first win — LoD2.2 is a fast follow |
| Bake 3DBAG into the sim world DB | Server-authoritative | Import pipeline + schema before need; NL-only in a global schema | Premature; static files match the OSM-cell pattern already in use |
| New client building schema (not Overpass-shaped) | Cleaner wire format | New client parser + a second rendering path | Reusing the Overpass shape makes the client change ~routing only |

## Consequences

**Positive**:
- Covered NL areas get authoritative heights and ground level; untouched code
  everywhere else.
- Adding a city is a bbox + re-run of the extract script; no client change and no
  Caddy change.
- `CARTO_3DBAG_URL=""` (or an unreachable host) transparently reverts to OSM.

**Negative / risks**:
- **Coverage seam**: at the bbox edge, cells inside use 3DBAG and just-outside use
  OSM; a building straddling the boundary could appear from both datasets (they
  have separate id spaces, so cross-source dedup doesn't catch it). Edge-only;
  mitigated by a generous bbox around each city.
- **LoD2.2 payload**: real meshes are heavier — Groningen is 114 MB (vs 44 MB for
  LoD1.2), the densest z14 cell 14.7 MB (~3.5 MB gzipped, ~240k triangles). The
  one-cell-per-frame upload cap keeps that off the render loop; shadow cost is
  bounded by a `SHADOW_RADIUS` LOD (`lod22.rs`) — cells past it are marked
  `NotShadowCaster`.
- **No render if LoD2.2 fetch fails**: covered cells skip the OSM path even if the
  mesh fetch fails, so a persistent host failure leaves those cells empty rather
  than falling back. Acceptable for a static, reliable host; revisit if it bites.
- **Attribution obligation**: 3DBAG is **CC BY 4.0**. The Layers panel shows the
  credit from the coverage manifest whenever 3DBAG coverage is active.
- **Second data source to keep fresh**: re-run the extracts on new 3DBAG releases
  (documented in `server.md` §8).

## Related

- [ADR-022](./022-osm-building-import.md): Supplies the building pipeline, cell cache layout, and `parse_buildings` this reuses.
- [ADR-004](./004-osm-tile-integration.md): Cell/zoom addressing shared with the basemap.
- [ADR-020](./020-map-tier-and-sim-tier-world-model.md): Buildings are map-tier decoration.
- [ADR-023](./023-data-layers.md): Same self-hosted-geo, attribution, and cell-streaming shape.
