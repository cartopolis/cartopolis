# ADR-020: Two-Tier World — Continuous Map Surface and Discrete Sim Regions

---
**Metadata:**
- **ID**: ADR-020
- **Status**: Accepted (map tier); sim tier partly revised — see note
- **Date**: 2026-06-19
- **Tags**: [coordinates, map, regions, osm, large-world, architecture]
- **Decision Type**: architecture_pattern
- **Aligns with**: [G-04, G-02, P-03, P-05]
- **Roadmap tier**: P2 — planet-scale map and OSM building import
- **Doc resolution**: R2
- **Related**: [ADR-019, ADR-006, ADR-004]
- **Supersedes**: []
- **Superseded By**: []
---

> **Update.** The **map tier** and the one-shared-ruler principle below are implemented
> (streamed multi-zoom tiles, OSM buildings, the planetary globe — ADR-025). The
> **sim tier's shape has changed**: content is no longer *fixed discrete region squares*
> (ADR-021, being retired) but a **capacity-bounded set of assets placed at any
> real-world position**. The map-vs-content separation and the shared floating-origin
> frame still hold; wherever this ADR says "region", read "server-authoritative content
> anchored by lat/lng". A forthcoming ADR specifies the capacity-bounded asset model.
>
> **Update 2 (ADR-035).** The map tier is moving from **raster** tiles to **self-hosted vector
> tiles (MVT)**, so wherever this ADR calls the map tier a "stack of photos" or read-only
> "wallpaper", read **streamed geometry styled on the client** — which is what lets the basemap
> respond to the day/night cycle and weather. The two-tier split, the map/sim decoupling and the
> one-shared-ruler principle are unchanged.

## Context

**Problem (in plain terms)**: Today the world is built from fixed squares of ground, each square textured with **one** OpenStreetMap (OSM) map image at a single zoom level (zoom 17, ~256 m). That square does double duty: it is both *the map you look at* **and** *the area the game simulates*. This was a shortcut to get the real-world scale roughly right.

We want two things that the shortcut can't give us:
1. A proper **map you can browse at any zoom** — zoom out to see a city, zoom in to see a street — like Google Maps or Cesium, eventually with 3D buildings from OSM.
2. **Solid, well-defined game areas** ("regions" in the Second Life / OpenSim sense) that have a fixed, real-world size and crisp boundaries, where avatars, objects, and physics live.

The shortcut couples these two ideas together, so improving one breaks the other. A separate precision spike (ADR-019) proved we can place things anywhere on Earth accurately, which removes the only reason the shortcut existed.

**Requirements**:
- Browse a continuous map at multiple zoom levels.
- Keep game regions a fixed real-world size everywhere on Earth (not stretched by where they are on the map).
- Be able to drop OSM 3D buildings onto the map later without redesigning this.
- Keep the simulation server (`vibe_sim`) as the authority over game regions.

## Decision

Split the world into **two independent layers** that share one coordinate system:

1. **Map tier** — a read-only "wallpaper" of the world. A grid of OSM map tiles, streamed in at whatever zoom suits the camera. Pretty to look at; nothing happens here.
2. **Sim tier** — the actual game. Fixed-size regions (e.g. 256 m × 256 m of *real* meters) with crisp edges, where objects and avatars live and the server is in charge.

The map tier is the backdrop; the sim tier sits on top of it. They are **not** tied to each other — the map's tiles and the game's regions are two different ways of dividing up space.

**Approach** (the simple version):
- **One shared ruler.** Everything is positioned in a single real-meter coordinate system covering the whole planet, using a "floating origin" technique (ADR-019) so distant places stay accurate. Think of it as keeping the math centred on wherever you currently are.
- **Map tier = stack of photos.** Each map tile is a flat image placed at its true real-world spot. Near the camera we show high-detail (zoomed-in) tiles; far away we show coarse (zoomed-out) ones, swapping them in and out as you move. This is the standard "slippy map" / quadtree approach.
- **Sim tier = game board squares.** A region is a fixed real-meter square anchored to a real-world location (a latitude/longitude). Its size never changes no matter where on Earth it is. Inside a region, positions are plain local meters — east/north/up — so physics and movement behave normally.
- **They overlap freely.** A region does not have to line up with the map tiles underneath it; the tiles are just the picture on the ground beneath the game board.

## Rationale

**Primary Reasoning (why this shape)**:
1. **Each layer can improve on its own.** We can make the map prettier (more zooms, 3D buildings) without touching the game rules, and vice versa — they have clean, separate jobs (P-05).
2. **Regions stay honest.** Map tiles change real-world size depending on latitude; game regions must not. Decoupling lets regions be a fixed real size everywhere, so a 256 m plot is always 256 m.
3. **It matches how the pros do it.** Cesium (and Cesium-for-Unreal) keep a continuous, streamed, level-of-detail map completely separate from any game/semantic content placed on top via an "anchor." Our region anchor is the same idea. Their map tiles are units of *display*, never units of *gameplay* — which is exactly the separation we want.
4. **It unlocks buildings cleanly.** OSM 3D buildings are just more content placed by real-world position; with one shared ruler they drop onto the map without caring about tiles or regions.

**Alternatives Considered**:

| Alternative | Pros | Cons | Rejected Because |
|------------|------|------|------------------|
| Keep one square = map + game (today) | Simple now | Can't browse zooms; region size stretches with latitude | Fails both goals |
| One giant textured ground that swaps its picture | No grid to manage | Hits hardware texture-size limits; can't stream per-area or mix zooms | Doesn't scale past a small area |
| Make game regions follow the map tiles | Auto-aligned art | Region size then varies with latitude/zoom (the bug we're removing) | Regions must be a fixed real size |
| Adopt the 3D Tiles standard for everything now | Battle-tested streaming/LOD | Heavy spec; overkill before basics exist | Defer; revisit for buildings |

## Consequences

**Positive**:
- A browsable, multi-zoom map, and a clear path to 3D buildings on it.
- Game regions are a consistent real-world size anywhere on Earth, with crisp boundaries and a normal local coordinate frame.
- Map work and simulation work stay independent and can progress in parallel.
- Server authority over regions (`vibe_sim`) is unchanged in spirit.

**Negative**:
- **More moving parts** than one square: a streaming map system plus a region-placement system (mitigation: build the map tier in stages — static first, streaming later).
- **Migration**: region size stops being "a zoom-17 tile" and becomes a fixed real-meter value; code assuming tile-sized regions (e.g. `REGION_SIZE_METERS`, region/tile coupling in rendering) must change (mitigation: phase in behind the new coordinate model).
- **New concept to document**: "map tile" vs "sim region" must be clearly explained so contributors don't re-couple them (mitigation: this ADR).

## Related

- [ADR-019](./019-planet-scale-floating-origin.md): The "one shared ruler" / floating-origin precision model this design relies on.
- [ADR-006](./006-world-coordinate-and-osm-anchor.md): Refines its "region == tile at a fixed zoom" assumption; regions become fixed real-meter areas decoupled from tiles.
- [ADR-004](./004-osm-tile-integration.md): The map tier generalises today's single-tile fetch into a multi-zoom, streamed tile grid.
