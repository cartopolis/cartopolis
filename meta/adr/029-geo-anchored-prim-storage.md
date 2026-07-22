# ADR-029: Geo-Anchored Prim Storage

---
**Metadata:**
- **ID**: ADR-029
- **Status**: Proposed
- **Date**: 2026-07-22
- **Tags**: [prims, coordinates, storage, protocol, precision, large-world, persistence]
- **Decision Type**: architecture_pattern
- **Aligns with**: [G-02, P-03] — a planet-scale world where authored content keeps its real-world position
- **Roadmap tier**: P1
- **Doc resolution**: R2
- **Related**: [ADR-002, ADR-011, ADR-017, ADR-019, ADR-020, ADR-025]
- **Supersedes**: []
- **Superseded By**: []
---

## Context

**Problem**: Prim positions are stored and transmitted as a single absolute `f32`
`Vec3` in "sim-space" metres measured from the world origin `(0,0,0)`, at every
layer — the `Prim` component's Bevy `Transform`, `PrimDto.position` on the wire,
the `position_x/y/z REAL` columns read back into `f32`, and both SQLite databases
(server and offline client). Prims are spawned with only `(Prim, Transform)` —
**no big_space `GridCell`, no region parenting** (`rendering.rs::spawn_prims`) —
so they render at grid cell `(0,0,0)` and their world position *is* the raw `f32`.

This does not fit the world-size coordinate system the client now uses:

1. **Precision.** The world frame is a big_space `i64` grid of 10 km cells with an
   `f32` local `Transform` inside each cell (ADR-019), giving ~mm precision
   anywhere on the planet. A lone absolute `f32` holds only ~7 significant
   digits, so a prim more than ~10–100 km from the origin degrades to
   decimetre/metre precision. Regions already dodge this — `spawn_regions` runs
   `Grid::translation_to_grid` and spawns them with a `GridCell` + `ChildOf(root)`
   — but prims do not.

2. **Reference frame.** The world is no longer pinned to a fixed sim origin. Since
   the movable-OSM-anchor model (ADR-006 → ADR-020) and the planetary globe
   (ADR-025), the ground truth is a re-anchorable Web-Mercator frame whose origin
   is *wherever the anchor currently sits*: `MapStream::geo_anchor() ->
   (lat, lng, zoom)`, and `lat_lng_to_world(anchor, clat, clng) == (0, 0)`. A prim
   stored as absolute metres-from-origin has no tie to a lat/lng, so the moment
   the anchor moves — the entire point of the anchor-less direction — every prim's
   world position silently refers to a different place on Earth, with no stored
   geo coordinate to re-project it from.

**Requirements**:
- A prim's real-world position must survive a re-anchor (the anchor is expected to
  move across the globe within a session).
- Placement precision must be independent of distance from the current origin
  (planet-wide ~mm, matching ADR-019).
- Geo placement is authoritative and anchor-independent at every layer (wire,
  storage, render).

## Decision

Store each prim's horizontal placement as an **absolute geographic coordinate**
(`GeoAnchor { latitude, longitude }`, `f64`) plus an altitude carried in
`position.y`; derive the render-space `GridCell` + local `Transform` on the client
from the *current* OSM anchor, and re-derive it whenever the anchor moves.

Prims are **online-only and geo-anchored**: the server is the sole authority for
prim state, and every prim carries an absolute `GeoAnchor`.

**Approach**:
- **`vibe_core`** — `GeoAnchor { latitude: f64, longitude: f64 }` and a mandatory
  `geo: GeoAnchor` field on `PrimDto`, `CreatePrim`, and `UpdatePrim`.
  `PROTOCOL_VERSION` is 10.
- **`vibe_sim`** — the `prims` table carries `latitude`/`longitude REAL NOT NULL`;
  `db.rs` round-trips them as required `f64`. The sim stores whatever geo the
  client authored — it never converts, having no anchor of its own.
- **`viberfox`** — a `PrimGeo { latitude, longitude }` component mirrors the DTO
  field onto every prim entity. `place_geo_prims` converts
  `geo → lat_lng_to_world(anchor, …) → GridCell + local Transform`, parents the
  prim to the big_space root, and re-projects all prims when
  `MapStream::geo_anchor()` changes. The outbound `CreatePrim`/`UpdatePrim`
  senders (gizmo drag, edit dialog, AI assistant) derive `geo` from the prim's
  world position via `prim_geo::geo_for` (cell + local) or `geo_from_world`.
  Prim authoring requires an `OnlineSession`.

`f64` lat/lng gives sub-millimetre resolution globally; the `f32` local
`Transform` stays cell-local (≤10 km) so it keeps mm precision — the same
guarantee regions already have.

## Rationale

**Primary Reasoning**:
1. **Geo is the anchor-independent invariant.** Two clients with different anchors
   each convert their own local frame to the *same* lat/lng, and a re-anchor is a
   pure re-projection of an unchanged stored coordinate. Nothing else in the
   system survives an anchor move — this does.
2. **It matches how regions already work.** Regions carry `latitude/longitude` and
   are rebased into the grid via `translation_to_grid`; this makes prims
   first-class citizens of the same anchored frame instead of an exception that
   only renders correctly while everything sits near the origin.
3. **Precision follows from cell-locality, not bigger floats.** Keeping the `f32`
   `Transform` cell-relative (via `GridCell`) is what ADR-019 already relies on;
   widening prim coordinates to `f64` everywhere would fight big_space instead of
   using it.
4. **Decouples prims from region lifetime.** The anchor-less direction expects
   fixed sim regions to go away; an absolute geo coordinate needs no region to be
   placed.

**Alternatives Considered**:

| Alternative | Pros | Cons | Rejected Because |
|------------|------|------|------------------|
| Parent prims to their `Region` entity (`ChildOf(region)`), keep `f32` region-relative | Smallest change; no protocol/schema churn; reuses region `GridCell` | Ties prims to region lifetime; still bound to the fixed-origin frame; breaks when regions are dropped for the movable anchor | Contradicts the anchor-less direction (ADR-020); doesn't survive re-anchor on its own |
| Widen `position` to `f64` `DVec3` absolute metres | Conceptually simple; planet-wide precision | Still frameless — absolute from *which* origin? A re-anchor still invalidates it; doubles wire/storage width; duplicates what big_space already does with `i64` cells | Solves precision but not the reference-frame problem, which is the real blocker |
| Store `GridCell (i64×3)` + `f32` local on the wire | Directly matches the render model; mm precision | Cell indices are meaningless across a re-anchor (they're relative to the current origin); leaks a client rendering concept into the persisted authority | Not anchor-independent; couples storage to the floating-origin implementation |

## Consequences

**Positive**:
- Prims keep their real-world location across re-anchors, teleports, and the
  space/globe tier — placement is defined in the same frame as the imagery.
- Planet-wide ~mm placement precision, independent of distance from origin.
- Prims stop depending on regions for a reference frame (aligns with anchor-less).
- Server storage becomes geo-authoritative; no anchor knowledge needed sim-side.

**Negative**:
- **Prims require the server.** Authoring needs an `OnlineSession`; offline is a
  map viewer, not an authoring mode. The AI assistant's `create_prim` /
  `update_prim` / `delete_prim` tools return "requires an online session" when
  disconnected, and `update_prim` can't change shape (no `shape` field on
  `UpdatePrim`).
- **Altitude is `f32` in `position.y`.** Acceptable — altitude range is small and
  cell-local; only the horizontal plane needs geo precision.
- **Authoring reconstructs world from cell + local before converting to geo.**
  Because `transform.translation` is cell-relative, gizmo sends add the cell back
  in (`prim_geo::geo_for`); the edit-dialog / AI paths author near the origin and
  convert the world position directly (`prim_geo::geo_from_world`). *Monitoring*: a
  prim that jumps on the next re-anchor means a send site sent a stale/local
  coordinate — worth a round-trip test (author → snapshot → re-anchor → same
  lat/lng).

## Related

- [ADR-019](./019-planet-scale-floating-origin.md): the `i64`-cell floating-origin
  model this makes prims participate in.
- [ADR-020](./020-map-tier-and-sim-tier-world-model.md) / [ADR-006](./006-world-coordinate-and-osm-anchor.md):
  the movable-anchor frame prims are now expressed in.
- [ADR-025](./025-planetary-globe-and-space-tier.md): re-anchor on globe descent —
  the event that makes re-projection necessary.
- [ADR-017](./017-prim-authoring-and-editing.md): the `CreatePrim`/`UpdatePrim`
  authoring path extended with `geo`.
- [ADR-011](./011-static-world-replication-v0.md): `WorldSnapshot` carrying
  `PrimDto`.
- [ADR-002](./002-sqlite-storage.md): the prim table extended by migration `V5`.
