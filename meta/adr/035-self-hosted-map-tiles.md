# ADR-035: Self-Hosted Vector Map Tiles

---
**Metadata:**
- **ID**: ADR-035
- **Status**: Proposed (implementation underway in a separate workstream)
- **Date**: 2026-07-23
- **Tags**: [osm, tiles, vector, mvt, map, ops, rendering, dependency, hosting, web]
- **Decision Type**: technology_choice
- **Aligns with**: [G-04, G-02, P-03]
- **Roadmap tier**: P1
- **Doc resolution**: R2
- **Related**: [ADR-004, ADR-020, ADR-022, ADR-023, ADR-025, ADR-027, ADR-033, ADR-034, ADR-014]
- **Supersedes**: []
- **Superseded By**: []
---

## Context

**Problem**: The client renders the map from raster tiles fetched straight from
`tile.openstreetmap.org` (the `osm_tile_url_template` default in `vibe_sim/src/config.rs:42`),
consumed by `tile_loader`, `map_stream` and the globe. Three things make that untenable for the
web target (ADR-033) and increasingly untenable generally:

1. **OSM's tile CDN is not a base layer for a product.** Their tile usage policy expects heavy or
   commercial consumers to run their own infrastructure, and **explicitly discourages proxying** —
   so ADR-034's normal answer (an L2 passthrough) is off the table here.
2. **A browser cannot identify itself.** `User-Agent` is a forbidden header in `fetch()`, so
   `TILE_USER_AGENT` — our compliance with the policy's identification requirement — does not
   apply on the web build.
3. **Tiles are by far our largest byte volume**, across both the map tier and the globe tier
   (ADR-020, ADR-025). This is where caching and locality matter most (P-03).

A fourth, smaller issue: the tile template is currently handed to the client by the **sim** in
`ServerHelloAck`. ADR-031's anonymous Viewer has no sim, so it needs the template from
configuration instead (there is a `Default` in `resources.rs:581`, but it should be explicit).

We already self-host the rest of the geo stack — Nominatim, Overpass, 3DBAG (ADR-027) — behind the
garage44 Caddy, so this follows an established pattern rather than starting one.

## Decision

**Serve our own OSM *vector* tiles** (Mapbox Vector Tile / MVT) from our origin at
`/tiles/{z}/{x}/{y}.mvt`, generated offline with **planetiler** and stored as **PMTiles**. The
client decodes MVT and renders the map as geometry; styling happens client-side.

### Why vector rather than raster

Raster was the conservative choice because it preserved the existing PNG → texture path. Vector
wins on the axes that matter most for *this* project:

- **Styling is client-side and dynamic.** viberfox has a live day/night cycle, atmosphere and
  weather (ADR-005, ADR-032). A raster basemap is a photograph — it cannot respond to any of it, and
  a night city lit by a midday basemap looks wrong. Vector geometry can be styled, lit and shaded by
  the systems already running.
- **The map stops being wallpaper and becomes geometry**, which fits a 3D world far better than a
  projected image: roads as ribbons, water and landuse as tessellated polygons, at real elevation.
- **Overzoom removes the expensive zooms.** Vector tiles stop at ~z14 and the client renders
  deeper levels from the same data. The raster plan's costly z15–z19 regional pyramid simply does
  not exist.
- **Far smaller over the wire**, which matters most for the wasm build.
- **Pure-Rust decode** (`geozero`/protobuf) → wasm-safe, no C dependency (cf. the `openjpeg-sys`
  lesson in ADR-033).
- **The pipeline gets simpler**, not harder: planetiler emits vector tiles directly, so the
  rasterisation step the raster plan needed disappears. This is what let this ADR move from R1 to
  R2 — the pipeline is no longer an open spike.

### Coverage

| Tier | Zooms | Extent |
|---|---|---|
| **Globe / space** (ADR-025) | z0–~z6 | whole planet |
| **Map / city** | to ~z14, then client **overzoom** | regional extracts first (Netherlands, matching 3DBAG), expanding per region |

Rough order of magnitude: a country extract is single-digit GB; the full planet to z14 is tens of
GB — feasible later, but regional extracts plus global low zoom is the sensible start. *(Estimates
to be confirmed by the implementation workstream.)*

### Serving

A **PMTiles archive** (single file, HTTP range reads) rather than millions of small files. A small
Rust handler (`pmtiles` crate) reads `z/x/y` out of the archive; this lives in `vibe_relay`
(ADR-034) as a first-class route rather than an `UPSTREAMS` adapter — it has no upstream, it reads
local data. Caddy fronts it at `/tiles/*` with long-lived `Cache-Control`; the native client keeps
an on-disk cache.

### Client work (the substantial part)

This is a real client change, not a URL swap, and is being built in a separate workstream:

- **Decode** MVT (protobuf) instead of PNG.
- **Tessellate** polygons — `earcutr` is already in the client for OSM building footprints
  (ADR-022), so the machinery exists; lines (roads) become ribbon geometry.
- **Style** client-side: a style definition maps MVT layers/classes to colours, widths and heights,
  and can respond to time of day and weather.
- `tile_loader` / `map_stream` / the globe move from "fetch image → texture" to "fetch tile →
  geometry", keeping the existing streaming, LOD and cache-eviction structure.

The globe tier may keep a rasterised or simplified representation at very low zoom, where per-feature
geometry buys nothing — decided during implementation.

### Client configuration

The tile URL template becomes **configuration** (ADR-014 layering) rather than something only the
sim supplies, so the anonymous Viewer (ADR-031) has a base map with no session. `ServerHelloAck`
may still override it, letting a deployment point clients at its own tile source.

### Downstream consolidation (noted, not decided)

MVT tiles carry buildings, roads, landuse and POIs — overlapping what we currently fetch from
Overpass for building footprints (ADR-022) and data layers (ADR-023). There is a real opportunity to
retire some Overpass traffic later. **Not decided here**: Overpass gives live, queryable, arbitrarily
fresh data, while tiles are a periodic snapshot at fixed zoom, and the data layers depend on tag
filtering that MVT schemas flatten away. Revisit once the vector client path is real.

## Rationale

**Primary Reasoning**:
1. **Policy leaves no alternative to self-hosting.** OSM discourages proxying and expects heavy
   users to self-host; the browser cannot send our identifying User-Agent.
2. **Vector suits a lit, 3D, time-varying world.** Our basemap needs to respond to the day/night and
   weather systems; a raster image structurally cannot.
3. **Overzoom collapses the cost.** The dominant expense in the raster plan was the high-zoom
   regional pyramid, which vector tiles do not need.
4. **It follows the existing pattern** — one more self-hosted geo service behind the same Caddy
   (ADR-027 precedent).
5. **Pure-Rust decode keeps the wasm target clean**, unlike the C-dependency trap ADR-033 hit.
6. **We control caching and locality** for our highest-volume traffic (P-03).

**Alternatives Considered**:

| Alternative | Pros | Cons | Rejected Because |
|------------|------|------|------------------|
| Keep hitting `tile.openstreetmap.org` | Zero work | Against their policy at our scale; no UA from a browser | Not viable for a public web build |
| Proxy/cache OSM tiles through our relay | Little infrastructure | OSM **explicitly discourages proxying**; still their bandwidth | Policy, not cost |
| **Self-hosted raster tiles** (the earlier plan) | No client change; preserves PNG→texture path | Static appearance under a live day/night cycle; expensive high-zoom pyramid; larger payloads; needs a rasterisation step | Vector is a better fit for a lit 3D world and cheaper to store and serve |
| Third-party keyed provider (MapTiler, Stadia, …) | No ops at all | Per-tile cost; key custody in a browser; vendor dependency | Recurring cost and dependency for our highest-volume asset |
| Directory of tile files | Dead simple | Millions of inodes; slow sync/backup | PMTiles is one file with the same access pattern |
| Full planet at full depth now | Uniform | Tens of GB and long build before it's needed | Regional extracts + global low zoom is the sensible start |

## Consequences

**Positive**:
- The web build gets a base map that is ours, compliant, and same-origin (ADR-034).
- The basemap participates in lighting, day/night and weather instead of fighting them.
- Overzoom removes the high-zoom storage pyramid; payloads shrink, which most helps wasm.
- Pure-Rust decode — no C dependency on the wasm path.
- The anonymous Viewer gets a base map with no sim session (ADR-031).
- Removes a third-party runtime dependency from the critical path.
- A future path to retiring some Overpass traffic (noted above).

**Negative**:
- **A substantial client rewrite** of the tile path — decode, tessellation, styling — replacing a
  working PNG→texture pipeline (mitigation: separate workstream; `earcutr` and the streaming/LOD
  structure are reused).
- **We now own map styling**, which is real design work and will not resemble
  `tile.openstreetmap.org` (mitigation: eventually desirable — a viberfox style — but it is work
  that raster would not have required).
- **A generation pipeline to run and refresh** (mitigation: planetiler is a single tool with no
  PostGIS/render-daemon stack; refresh cadence can be weeks).
- **Storage** of single-digit GB per region, growing as regions are added.
- **Staleness**: tiles lag OSM edits by the refresh cadence (mitigation: acceptable for a base map;
  Overpass-driven layers remain live).
- **Attribution remains mandatory** under ODbL — self-hosting changes nothing there.
- **Low-zoom/globe rendering is unresolved** — per-feature geometry may not suit the space tier
  (mitigation: decide during implementation; a simplified or rasterised globe representation is
  allowed).
- Until the pipeline and client path exist, the web build has no compliant base map — this is on
  ADR-033's Phase 1 critical path.

## Related

- [ADR-004](./004-osm-tile-integration.md): Revises its raster-tile-as-texture mechanism; tiles become vector geometry from our own origin.
- [ADR-020](./020-map-tier-and-sim-tier-world-model.md): The map tier stops being a "stack of photos" and becomes streamed geometry; the tier split itself is unchanged.
- [ADR-034](./034-public-data-relay.md): Single-origin layout; tiles are served locally at `/tiles/*`, not proxied.
- [ADR-033](./033-build-targets-and-wasm-packaging.md): The web target that forces the issue; this is on its Phase 1 path.
- [ADR-027](./027-3dbag-buildings.md): The precedent — a self-hosted geo dataset behind the same Caddy.
- [ADR-022](./022-osm-building-import.md), [ADR-023](./023-data-layers.md): Overlap with MVT content; possible future consolidation, not decided here.
- [ADR-025](./025-planetary-globe-and-space-tier.md): Consumes the global low-zoom set.
- [ADR-014](./014-runtime-configuration-and-operations.md): Tile template moves into config layering.
