# ADR-012: Interest Management and OSM Tiles

---
**Metadata:**
- **ID**: ADR-012
- **Status**: Accepted
- **Date**: 2025-03-21
- **Tags**: [interest, aoi, osm, networking]
- **Decision Type**: architecture_pattern
- **Related**: [ADR-004, ADR-006, ADR-010, ADR-011]
- **Supersedes**: []
- **Superseded By**: []
---

## Context

**Problem**: Sending **all** regions/prims to every client wastes bandwidth as the world grows. AOI (area-of-interest) filtering must decide which world DTOs (ADR-011) and avatar updates (ADR-010) a client receives, using the same anchor math as OSM tile selection (ADR-004, ADR-006). v0 keeps this simple (radius) before any prioritization queues.

## Decision

**Interest management v0**: the server tracks each client’s **observer position** in sim space and includes only regions/prims within a **configurable radius** (`aoi_radius`, default 500 m). **OSM tiles** are **not** proxied through the sim; clients derive `TileKey`s (ADR-006) client-side and fetch over HTTP (ADR-004). The server’s role stays **world + avatars** only.

**Implementation** (`cartopolis_simulator/src/state.rs`, `net.rs`):
- `SimWorld::set_observer` records the observer; `snapshot()` filters regions by `(origin - observer).length_squared() <= aoi_radius²`, then prims by the surviving region set.
- **Current limitation**: avatars are replicated to **all** clients regardless of AOI — only regions/prims are distance-filtered. AOI for avatars is future work.
- A server-side tile cache or CDN redirect is deferred to a future ADR.

## Rationale

**Primary Reasoning**:
1. **Scales world size**: Reduces replication fan-out.
2. **OSM alignment**: Tile choice follows the same spatial truth as AOI when coords are shared (ADR-006).
3. **Incremental**: Radius AOI is easy; a distance prioritizer can plug in later.

**Alternatives Considered**:

| Alternative | Pros | Cons | Rejected Because |
|------------|------|------|------------------|
| No AOI v0 | Fastest | Breaks at medium worlds | Small extra design now saves pain |
| Server proxies all map tiles | Single choke point | Bandwidth + ToU complexity | Defer |
| Grid-only AOI | Fast queries | Rough at region boundaries | Can combine with radius |

## Consequences

**Positive**:
- Clear knob for perf tuning (radius, rates)
- Keeps tile load on edge clients (ADR-004)

**Negative**:
- Desync if observer updates lag (mitigation: server extrapolate or clamp max speed)
- Edge cases at AOI boundary (mitigation: hysteresis / overlap regions)

## Related

- [ADR-006](./006-world-coordinate-and-osm-anchor.md): Shared space for AOI + tiles
- [ADR-004](./004-osm-tile-integration.md): Client tile fetch
- [ADR-010](./010-authoritative-avatar-state-v0.md): Observer may track camera/avatar anchor
- [ADR-011](./011-static-world-replication-v0.md): Filtered snapshot/delta
