# ADR-004: OSM Tile Integration for Region Textures

---
**Metadata:**
- **ID**: ADR-004
- **Status**: Accepted
- **Date**: 2025-03-18
- **Tags**: [rendering, tiles, osm]
- **Decision Type**: technology_choice
- **Related**: [ADR-001, ADR-003]
- **Supersedes**: []
- **Superseded By**: []
---

## Context

**Problem**: Regions need textured ground planes. Use real-world map imagery for geographic context.

**Requirements**:
- Fetch map tiles by (x, y, z) coordinates
- Load asynchronously
- Cache for performance
- Integrate with Bevy's asset system

## Decision

Use OpenStreetMap tiles fetched over HTTP (`ureq`) on a background worker thread, with a `TileCache` resource.

**Approach**:
- `TileKey(x, y, z)` for lookup; regions anchored at `REGION_ZOOM_LEVEL = 17` (`cartopolis_core/world.rs`)
- `TileCache` holds image handles + in-flight state; fetches run on a worker thread and post results back over an mpsc channel (non-blocking)
- `RegionTile` component on region entities; `load_region_tiles` drains results, `update_region_materials` (in `rendering.rs`) applies them
- On-disk tile cache (`tile_cache_path`) and a self-hosted OSM template via `OsmTileUrlTemplate`; `map_stream.rs` streams tiles for the globe/map view

## Rationale

**Primary Reasoning**:
1. **OSM**: free, legal tiles
2. **ureq**: simple blocking HTTP on a worker thread — no async runtime pulled in for this
3. **TileCache**: dedupe fetches (memory + on-disk)
4. **LOD**: `RegionTile::lod_level` reserved for future LOD (currently unused)

**Alternatives Considered**:

| Alternative | Pros | Cons | Rejected Because |
|------------|------|------|------------------|
| reqwest | Async | Extra deps | ureq is simpler |
| Pre-bake tiles | Faster load | Large assets | Dynamic tiles preferred |
| Mapbox | Better quality | Cost, API key | OSM is free |

## Consequences

**Positive**:
- Realistic ground textures
- Caching reduces network traffic
- Standard tile format (z/x/y)

**Negative**:
- Network dependency (mitigation: memory + on-disk cache)
- OSM ToU: respect usage policy (mitigation: reasonable rate, attribution, self-hosted template option)

## Related

- [ADR-001](./001-bevy-game-engine.md): Bevy provides rendering
- [ADR-003](./003-ecs-architecture.md): Tile loading as a system
- Systems: `crates/cartopolis/src/systems/tile_loader.rs`, `map_stream.rs`, `rendering.rs`
