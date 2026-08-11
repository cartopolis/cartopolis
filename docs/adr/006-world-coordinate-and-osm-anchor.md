# ADR-006: World Coordinate Model and OSM Anchor

---
**Metadata:**
- **ID**: ADR-006
- **Status**: Superseded
- **Date**: 2025-03-21
- **Tags**: [coordinates, osm, regions, networking]
- **Decision Type**: architecture_pattern
- **Related**: [ADR-004, ADR-010, ADR-012]
- **Supersedes**: []
- **Superseded By**: [ADR-019, ADR-020, ADR-021]
---

> **Superseded.** The absolute-f32 sim-unit region frame here proved too imprecise at
> planet scale (ADR-019) and too coupled (map vs sim, ADR-020). Only the Web-Mercator
> **tile math** survives, implemented in `cartopolis_core::world` (`lat_lng_to_tile`,
> `tile_key_from_lat_lng`, `REGION_ZOOM_LEVEL = 17`). The region model itself is being
> retired — see ADR-021 and the capacity-bounded asset direction. Kept for history.

## Context

**Problem**: Regions, avatar movement, OSM tile fetching, and a future server must share one **authoritative** idea of where things are. Today rendering and SQLite use implicit assumptions; without a documented contract, client and server will drift (tile keys, AOI, handoff).

**Requirements**:
- Single mapping from **region / geographic intent** → Bevy **world space** → **OSM tile keys** (`z/x/y`)
- Explicit statement of **approximation** (tiles, projection) vs **gameplay authority** (server sim space)
- Compatible with [ADR-004](./004-osm-tile-integration.md) (tile cache, `TileKey`)

## Decision

Define and document one **canonical coordinate contract** in shared types (library code as implemented): region origin in sim units, optional lat/lon metadata for tooling, and a deterministic function from that origin to default `TileKey` set at a chosen zoom level.

**Approach**:
- **Simulation space**: Right-handed Y-up; region ground plane extent and origin fixed per ADR-002 region row (e.g. center + half-extents in meters or fixed cell size).
- **Geographic anchor**: Store WGS84 (lat/lon) or a named CRS only where needed for OSM; convert to tile indices using the same library/version on client and server.
- **Tile derivation**: `TileKey(x, y, z)` computed from anchor + `lod_level` (ADR-004); document rounding and zoom default.
- **Non-goal**: Sub-centimeter survey accuracy; **good enough** for aligned ground textures and consistent AOI.

**Rust ecosystem** (implement in `cartopolis_core`, ADR-015):
- **`serde`** for any coordinate / region-anchor DTOs shared with the wire (already used in the project).
- **`glam`** for `Vec3` / transforms in protocol and sim when they must round-trip with Bevy (`glam` with `serde` feature); avoids ad hoc tuple types.
- Prefer **small, explicit** Web Mercator → tile math in-tree for v0; add **`geo-types`** or **`proj`** only if CRS needs exceed slippy-map assumptions (keeps deps lean until required).
- Use **`approx`** (or similar) in **unit tests** for float comparisons on tile indices.

## Rationale

**Primary Reasoning**:
1. **OSM as differentiator**: The server and interest logic (ADR-012) must agree on which tiles matter without re-deriving ad hoc.
2. **Future replication**: Avatar and static world updates reference the same frame as tiles (ADR-010, ADR-011).
3. **Prevents silent bugs**: “Approximate” vs “authoritative” is written down once.

**Alternatives Considered**:

| Alternative | Pros | Cons | Rejected Because |
|------------|------|------|------------------|
| Client-only lat/lon | Simple now | Server cannot validate AOI | Multi-user needs shared model |
| Full proj pipeline everywhere | Accurate | Heavy dependency for v0 | Defer until needed |
| Separate tile space from sim | Flexible | Two sources of truth | Harder to keep aligned |

## Consequences

**Positive**:
- Clear handoff between DB, sim, rendering, and tile loader
- AOI and tile policy can reference one spec (ADR-012)

**Negative**:
- Must keep conversion code in sync on all binaries (mitigation: single `cartopolis_core` per ADR-015)
- Projection upgrades may shift textures slightly (mitigation: version the conversion helper)

## Related

- [ADR-004](./004-osm-tile-integration.md): Tile fetch and cache
- [ADR-002](./002-sqlite-storage.md): Region records carry anchor fields
- [ADR-010](./010-authoritative-avatar-state-v0.md): Avatar state lives in this frame
- [ADR-012](./012-interest-management-and-osm-tiles.md): AOI uses same space as tiles
