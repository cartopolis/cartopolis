# ADR-019: Planet-Scale Floating-Origin Coordinate Model

---
**Metadata:**
- **ID**: ADR-019
- **Status**: Accepted
- **Date**: 2026-06-19
- **Tags**: [coordinates, precision, rendering, osm, large-world]
- **Decision Type**: architecture_pattern
- **Aligns with**: [G-04, G-02, P-03]
- **Roadmap tier**: P2 — planet-scale map surface and OSM building import
- **Doc resolution**: R2
- **Related**: [ADR-006, ADR-004, ADR-001]
- **Supersedes**: []
- **Superseded By**: []
---

## Context

**Problem**: The roadmap direction is a **two-tier world**: a planet-scale, read-only OSM **map surface** (streamed tiles + procedurally extruded OSM buildings) onto which discrete, mutable **simulation regions** are activated ("converting" a building turns a map area into an authoritative sim region). Today every entity renders in **absolute f32 world space** clustered near the origin (regions at `sim_origin` or a ±300 m grid; camera, avatar, and ground-height math all raw f32). That is fine at ±a few km and breaks at planet scale.

Bevy renders transforms in **f32**. Earth's circumference is ~40,000 km; the f32 ULP (smallest expressible position step) near 4×10⁷ m is **~4.7 m**. Absolute planet coordinates in f32 therefore produce meter-scale jitter, z-fighting, and broken picking — the model in ADR-006 (region origins in sim units, "good enough" approximation) has no answer for content placed far from the origin.

**Requirements**:
- Place and render content anywhere on Earth without f32 jitter (target: sub-cm stability anywhere).
- Keep the existing f32 render path, ECS, camera, and `picking.rs` raycasts working with minimal disruption.
- Preserve real-meter scale for the simulation tier (physics, avatar speed) — see ADR-006's Mercator scale caveat.
- Be compatible with Bevy 0.19 (ADR-001).

## Decision

Adopt a **floating-origin (camera-relative) coordinate model**: store each entity's true position in high precision (global frame) and continuously rebase the Bevy render origin near the camera so all rendered f32 transforms stay within a few km of `(0,0,0)`, where f32 precision is ample.

**Approach**:
- **Global frame**: high-precision position as `i64` tile cell + `f32` (or `f64`) intra-cell offset (the production shape; reuses the `TileKey` notion from ADR-004/006). A spike used `DVec3` (f64) meters to prove the mechanism.
- **Render frame**: `Transform.translation = (global − floating_origin)`, recomputed when the origin shifts.
- **Rebase**: when the camera drifts past a threshold from the current origin, snap the origin to the camera and translate everything uniformly (including the camera) — a uniform shift, so no visible pop.
- **Map vs sim scale** (extends ADR-006): the map tier lives in Web Mercator units; an activated sim region reprojects to a **local ENU tangent frame in true meters** at conversion time, so the map→sim boundary is where "map units → real meters" happens once.
- **Build vs adopt → adopted `big_space`** (its grid + floating-origin propagation) over a hand-rolled `GlobalPos`. Upstream `big_space` 0.10 tops out at Bevy 0.18, so we keep a **vendored fork** at `crates/big_space` and advance its transform-propagation compat (`bevy_compat.rs`) on each Bevy bump — currently Bevy 0.19.

## Rationale

**Primary Reasoning**:
1. **Measured, not assumed**: a throwaway spike (`--spike-floating-origin`, module `crates/viberfox/src/systems/floating_origin.rs`, 2026-06-19) directly measured the f32 ULP at planet scale and the floating-origin remedy. The result (below) closes the precision risk that gates the whole planet-scale direction.
2. **Minimal blast radius**: rebasing keeps the existing f32 render/ECS/picking path; only placement and a per-frame sync are added. The spike confirmed the existing free camera cooperated.
3. **Honest scale**: pairing a global frame with per-region ENU tangent frames keeps physics in real meters despite Mercator distortion (the unresolved tension in ADR-006).

**Spike evidence** (markers at global X = 0, 1e6, 1e7, 2e7 m):

| Mode | Distance | f32 ULP (`ulp_m`) | Observed |
|------|----------|-------------------|----------|
| Absolute f32 (no rebase) | 1×10⁷ m | ~1.0 m | visible ~1 m jitter |
| Absolute f32 (no rebase) | 2×10⁷ m | ~2.0 m | ~2 m jitter; two cubes 0.1 m apart merged into one |
| Floating origin | up to 2×10⁷ m | ~2.4×10⁻⁷ m | sub-micron, rock stable |

**Alternatives Considered**:

| Alternative | Pros | Cons | Rejected Because |
|------------|------|------|------------------|
| Keep absolute f32 (ADR-006 as-is) | No new code | ~1–2 m jitter at planet scale (measured) | Fails the planet-scale requirement |
| f64 everywhere in render | Conceptually simple | Bevy renders f32; GPU is f32; pervasive conversion | Doesn't remove the f32 GPU limit |
| Per-region local origins only (no global frame) | Simple per island | No continuous browsable planet surface between regions | Map tier needs a continuous global frame |
| Hand-rolled `GlobalPos` + sync | Full control; exposes ULP for measurement | Reinvents grid/rebase machinery | Fallback the spike used; not needed once `big_space` fit |
| Adopt `big_space` | Grid + rebase for free | New dependency; must fit camera/picking/atmosphere; upstream lags Bevy | **Adopted** — vendored fork advanced to Bevy 0.19 |

## Consequences

**Positive**:
- Content can be placed anywhere on Earth with sub-cm render stability (measured to 2×10⁷ m).
- Unblocks tile-quadtree streaming and OSM building import on a continuous map surface.
- Establishes a clear map(Mercator)→sim(ENU meters) scale boundary, resolving the ADR-006 caveat.

**Negative**:
- **Invasiveness**: placement, camera, and `picking.rs` must read/write the global frame, not absolute `Transform` (mitigation: rebase keeps the camera near origin so most f32 math is unchanged; spike showed light touch).
- **Vendored `big_space` fork** must be re-synced against Bevy each upgrade (`crates/big_space/.../bevy_compat.rs`); upstream tracks only up to Bevy 0.18.
- **Coordinate-model migration**: `sim_origin` / region placement (ADR-006, `rendering.rs`) and any code hard-coding `REGION_SIZE_METERS = 256` must move onto the new frame (mitigation: phase in; sim regions can keep local ENU frames internally).

## Related

- [ADR-006](./006-world-coordinate-and-osm-anchor.md): Extends the canonical coordinate contract for planet scale; sim regions keep a local frame, now explicitly ENU meters.
- [ADR-004](./004-osm-tile-integration.md): `TileKey` informs the `i64`-tile global cell; tile streaming builds on this model.
- [ADR-001](./001-bevy-game-engine.md): f32 render constraint is the root cause; the vendored `big_space` fork is re-synced on each Bevy bump.
