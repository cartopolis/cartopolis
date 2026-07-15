# ADR-025: Planetary Globe and Space Tier

---
**Metadata:**
- **ID**: ADR-025
- **Status**: Proposed
- **Date**: 2026-07-14
- **Tags**: [rendering, coordinates, globe, osm, large-world, camera]
- **Decision Type**: architecture_pattern
- **Aligns with**: [G-01, P-01] — a real Earth you can zoom from a city to orbit
- **Roadmap tier**: P2
- **Doc resolution**: R1
- **Related**: [ADR-019, ADR-020, ADR-021, ADR-005]
- **Supersedes**: []
- **Superseded By**: []
---

## Context

**Problem**: The "Earth" camera mode orbits a flat Web-Mercator tangent plane. Pulling
back reveals a flat map with a straight horizon, never a globe. Users want the
Google-Earth experience: pull back into space, see the whole planet as a sphere,
spin it, and dive back down into the detailed streamed-tile city view.

**Requirements**:
- A true-scale Earth sphere, positioned so the existing flat city view sits on its
  surface at the sim anchor (no re-anchoring of the local scene).
- A camera that can retreat to orbit the Earth's centre from space and return to the
  local surface orbit continuously (no visible jump at the handoff).
- Reuse the existing WGS84 ECEF/ENU math (`vibe_core::geo`) rather than a parallel
  coordinate system, and the existing OSM tile source for the globe skin.
- Render across a ~10⁷ m depth range without wrecking the near-field city precision.

## Decision

Add a **space tier**: a single true-scale WGS84 globe mesh placed under the world
`BigSpace`, and a space sub-mode of the Earth camera that orbits the Earth centre.

**Approach**:
- **Placement.** The local scene is an ENU tangent frame at the sim anchor
  (ADR-020/021). Build the globe's vertices in that frame: each `(lat, lng)` →
  `geodetic_to_ecef` → `TangentFrame::world_to_local` → Bevy axes `(E, U, −N)`. The
  anchor lands at the local origin on the sphere surface, and Earth's centre sits at
  ≈ `(0, −R, 0)`. The mesh is one entity at grid cell 0; `big_space` rebases it for
  render precision at any camera distance.
- **Imagery: whole-globe quadtree LOD.** Every visible OSM tile is draped on the
  ellipsoid as a small sphere-patch mesh; tiles split into their four children
  whenever they would exceed a screen-size threshold (z2 root → z12, where the
  flat detail tier takes over below the space altitude). Far-side tiles are
  culled, off-screen tiles aren't refined, a parent stays until its children are
  loaded (no holes), and fetches are generation-tagged and disk-cached like all
  tile traffic. A plain ocean-toned base sphere backstops unloaded tiles and the
  polar caps beyond Web-Mercator's ±85°. (Supersedes the interim baked-skin +
  regional-caps approach, which read as a patch on a flat-toned ball.)
- **Camera.** The Earth orbit camera keeps its `focus / distance / yaw / pitch`
  parameters. Above a space-entry altitude the pivot switches from the ground focus
  to the Earth centre; the switch reparameterises `distance/yaw/pitch` from the
  unchanged camera world position, so the camera does not move. Descending past a
  lower threshold switches back to a surface focus under the camera. Near/far planes
  scale with altitude (near grows in space, far reaches past the far limb).
- **Anchor-less re-anchoring.** There is no fixed "region": the detailed tier
  (`map_stream` tiles + `osm_buildings` + the globe mesh) shares one movable
  `(lat, lng)` origin. Any teleport (search, bookmark, startup restore) or a globe
  descent onto a distant spot **re-anchors** that origin to the destination — the
  loaded detail is dropped and re-streamed at the new origin (kept cheap by the
  on-disk tile cache), and the globe is rebuilt in the new tangent frame (the baked
  skin is reused). This keeps the camera near the origin (where the flat frame is
  exact) and lets detail stream anywhere on the planet — the substrate the coming
  claim-based feature replacement attaches to. A generation counter on the anchor
  drives the globe rebuild and the building-patch reset.

## Rationale

**Primary Reasoning**:
1. Reusing `geo.rs` means the globe and the flat scene share one truth; the anchor
   already defines the tangent frame, so the surface view and the globe agree by
   construction.
2. Baking the skin from OSM keeps one visual identity (the map *is* the planet) and
   needs no bundled multi-MB asset.
3. Reparameterising at the pivot handoff (instead of blending two cameras) keeps the
   camera position exactly continuous with almost no code.

**Alternatives Considered**:

| Alternative | Pros | Cons | Rejected Because |
|------------|------|------|------------------|
| Faked curvature shader (bend distant ground) | Cheap, no new geometry | Only curves the horizon; never a whole globe | Doesn't meet the "spin the planet" requirement |
| Full ECEF re-anchor of the whole scene onto the sphere | "Correct" everywhere | Rewrites tile/building placement and streaming | Too large; the tangent-plane city view already works |
| Bundled Blue-Marble texture | Best fidelity | Multi-MB binary asset, off-brand vs OSM | OSM bake is consistent and asset-free |

## Consequences

**Positive**:
- Zoom continuously from a rooftop to Earth orbit and spin the planet.
- No new coordinate system; the space tier is additive over ADR-019/020/021.

**Negative**:
- Poles (beyond Web-Mercator ±85°) are absent in the skin: the bake clamps and the
  polar caps read as flat colour. Mitigation: acceptable for P2; a dedicated polar
  fill or a real Blue-Marble base can be layered later.
- The Nishita atmosphere/skybox and the local horizon props are tuned for the surface
  view and are not yet correct from space (thin-limb-over-black). Mitigation: fog and
  haze already fade with altitude (ADR-005 addendum); a space-appropriate sky is a
  follow-up.
- Phase 1 is a coarse globe (low-zoom bake, no per-tile spherical streaming). Higher
  detail on the sphere is deferred.

## Related

- [ADR-019](./019-planet-scale-floating-origin.md): the floating-origin model the globe rebases in.
- [ADR-020](./020-map-tier-and-sim-tier-world-model.md) / [ADR-021](./021-region-tessellation-flat-clusters-quad-index.md): the ENU tangent frame the globe is built in.
- [ADR-005](./005-sky-lighting-system.md): the sky/atmosphere the space view will eventually adapt.
