# ADR-022: OSM 3D Building Import (Overpass → Extruded Footprints)

---
**Metadata:**
- **ID**: ADR-022
- **Status**: Accepted
- **Date**: 2026-06-19
- **Tags**: [osm, buildings, rendering, import, dependency]
- **Decision Type**: architecture_pattern
- **Aligns with**: [G-04, P-03]
- **Roadmap tier**: P2 — map tier content
- **Doc resolution**: R1
- **Related**: [ADR-004, ADR-020, ADR-021]
- **Supersedes**: []
- **Superseded By**: []
---

## Context

**Problem**: The map tier (ADR-020) should show OSM **3D buildings**, not just flat tiles. OSM stores buildings as 2D footprints plus tags (`height`, `building:levels`, roof shape) under the *Simple 3D Buildings* schema — no ready-made meshes. We fetch that data and build geometry client-side.

**Requirements**:
- Pull footprints + heights for a geographic bbox.
- Extrude each footprint into a placed 3D mesh.
- Keep it client-side and additive (no protocol/sim changes).

## Decision

Fetch buildings from the **Overpass API**, parse the JSON, and **extrude each footprint** client-side into a placed mesh.

**Approach** (`systems/osm_buildings.rs`):
- **Fetch**: `way["building"](bbox); out geom;` on a **background thread** (Overpass takes seconds; the window must not block). Camera-following, one coarse cell at a time, **disk-cached** per cell; default endpoint is the self-hosted `overpass.cartopolis.org`, with a 3DBAG cell source tried first for NL coverage (ADR-027).
- **Height**: `height` tag → else `building:levels × 3 m` → else 6 m default; tolerant of `"12 m"` strings.
- **Placement**: vertices via the same Web Mercator math as the tiles, in the floating-origin frame (ADR-019).
- **Extrusion**: triangulated top cap (via **`earcutr`**, a pure-Rust ear-clipper — handles non-convex footprints) + vertical walls; wall normals oriented outward from the centroid (OSM ring winding is not guaranteed).

**New dependency**: `earcutr` — small, pure Rust, no native build step.

## Rationale

1. **Overpass is the standard query path** for selective OSM data by bbox + tag (sibling to the tile fetch, ADR-004), reusing the existing `ureq` client.
2. **Client-side extrusion keeps the prototype additive** — no wire/sim changes; buildings are map-tier content.
3. **`earcutr`** is the minimal way to triangulate non-convex footprints; alternatives (`geo` + heavier triangulation, or hand-rolled) add weight or bugs.
4. **Validated on real data** — Groningen buildings render aligned to the streets.

**Alternatives Considered**:

| Alternative | Pros | Cons | Rejected Because |
|------------|------|------|------------------|
| Cesium OSM Buildings / 3D Tiles | Prebuilt global LOD tilesets | Heavy spec; less control; external dependency | Revisit for planet-scale streaming, overkill now |
| Convex-only / fan triangulation | No dependency | Wrong for L-shaped/non-convex footprints | Most buildings are non-convex |
| Server-side extrusion | Thin client | Premature; needs protocol/sim work | Prototype is client-only |

## Consequences

**Positive**:
- Real 3D buildings from live OSM data; loader + extruder carry into the streamed/planet pipeline unchanged.
- No protocol, sim, or storage changes.

**Negative**:
- **Overpass load** — mitigated by the self-hosted endpoint, per-cell disk cache, an inter-query throttle, and background-thread fetches that log-and-skip on failure.
- **Fidelity** — flat roofs, no roof shapes / `building:part` (later iteration).

## Related

- [ADR-004](./004-osm-tile-integration.md): Tile fetch; buildings reuse the bbox/`ureq` path.
- [ADR-020](./020-map-tier-and-sim-tier-world-model.md): Buildings are map-tier content.
- [ADR-021](./021-region-tessellation-flat-clusters-quad-index.md): Will re-home buildings onto the real-metre region frame.
