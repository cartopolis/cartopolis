# ADR-026: Live Transit Vehicle Positions (OVapi GTFS-Realtime)

---
**Metadata:**
- **ID**: ADR-026
- **Status**: Proposed
- **Date**: 2026-07-19
- **Tags**: [data-layers, transit, live-data, rendering, gtfs-rt]
- **Decision Type**: technology_choice
- **Aligns with**: [G-04, P-03]
- **Roadmap tier**: P1
- **Doc resolution**: R1
- **Related**: [ADR-023, ADR-024]
- **Supersedes**: []
- **Superseded By**: []
---

## Context

**Problem**: The transit layer shows *where stops are* (ADR-023) and *when the
next bus leaves* (ADR-024), but not *where the buses are right now*. Live moving
vehicles on the map (as https://transit.reppie.org/ does for NL) make the layer
feel alive and are the natural companion to the departure boards.

**Requirements**:
- Open, key-free source of live vehicle GPS positions, coherent with the OSM/
  OVapi data the layer already uses.
- One cheap request per interval; per-frame-bounded spawning (the Intel-iGPU
  device-loss discipline in CLAUDE.md forbids mass mesh churn).
- Smooth motion between sparse fixes, not teleporting markers.
- No heavy new dependency for a client-side overlay.

## Decision

Poll **OVapi's GTFS-Realtime vehicle-position feed**
(`https://gtfs.ovapi.nl/nl/vehiclePositions.pb`) — one small protobuf covering
all live Dutch vehicles — on a worker thread, and drive one **coloured box mesh
per nearby vehicle** that glides between successive fixes.

**Approach** (`systems/transit_vehicles.rs`):
- A worker fetches the feed every 15 s (gated on the transit layer being on, the
  camera being near the ground, and vehicles in range).
- A **hand-rolled minimal protobuf reader** extracts the few fields needed
  (entity id, position lat/lng, timestamp) — no `prost`/codegen. Stale fixes
  (timestamp > 10 min old; the feed leaves ghosts) are dropped.
- Each fix is converted to the world frame via the shared map anchor, exactly
  like the POI pins (`lat_lng_to_local`), so vehicles follow re-anchors.
- Vehicles are reconciled into at most 80 boxes within 3 km of the camera
  (nearest first), each interpolated from its previous shown position to the
  new fix over the poll interval. Heading is **derived from travel direction**
  (the feed carries no bearing/speed field).
- **Line number + mode**: the feed's `route_id` is opaque, so the worker fetches
  OVapi's `/line/` list once (disk-cached, 24 h TTL) and maps the operator
  line-planning number carried in the vehicle id (`…:GVB:293:…`) to the public
  line (`N93`) and its `TransportType`. The number is painted (fontdue, egui's
  embedded font — same technique as the departure boards) onto a small badge
  texture, shared per line, and shown on a camera-facing label above the box;
  the mode drives the box/label colour. Verified live: 139/140 vehicles resolve.
- Toggled by a "Live vehicles" sub-checkbox under the transit layer; hidden in
  the space tier.

## Rationale

**Primary Reasoning**:
1. **Same provider, complementary data**: OVapi already backs the boards; its
   GTFS-Realtime side supplies positions with no new account or key.
2. **A tiny hand-rolled decoder beats a codegen dependency**: only ~6 field
   reads off a stable schema; matches the codebase's other hand-rolled parsers
   and avoids `prost` + a `build.rs`. Verified against the live feed (parses all
   ~140 active vehicles, valid coordinates).
3. **Bounded and smooth**: one shared cuboid + two materials, ≤80 entities, no
   per-frame asset churn — safe under the device-loss guardrails; motion
   interpolation hides the sparse, bearing-less fixes.

**Alternatives Considered**:

| Alternative | Pros | Cons | Rejected Because |
|------------|------|------|------------------|
| `prost` + generated GTFS-RT schema | Standard, complete | New heavy dep + build codegen for 6 fields; own ADR for the dependency | Overkill; a minimal reader is enough and lighter |
| NDOV KV6 push feed | Authoritative, includes speed/bearing | Registration, ZeroMQ, per-operator formats | Far too heavy for a client overlay (same reason ADR-024 rejected it) |
| GPS bearing from the feed | Exact heading | Field is absent in this feed (0/140 carry it) | Not available; heading derived from motion instead |
| Real bus glTF model | Diegetic, recognisable | Modelling/animation work | Placeholder box ships now; the renderer can be swapped without touching the data path |

## Consequences

**Positive**:
- The transit layer now shows live buses moving along the streets, gliding
  between fixes, aimed along their travel direction, and labelled with the
  public line number in the mode colour.
- The parse → anchor → interpolate path is reusable for any future GTFS-RT
  vehicle feed (worldwide coverage becomes a data-source swap).

**Negative**:
- **Netherlands-only** and **unofficial** (OVapi, no SLA), like ADR-024;
  failures degrade to stale-then-absent vehicles (TTL prune). Untrusted display
  data only — never influences world/sim state.
- **Line/mode come from a second endpoint** (`/line/`), joined on the operator
  planning number; a vehicle whose planning number isn't in the list (≈1%) gets
  no label and is drawn as a bus. The mode is the line's, not resolved per
  individual metro/tram branch.
- **Placeholder look**: coloured boxes, not bus models.

## Related

- [ADR-023](./023-data-layers.md): The transit layer supplying stop positions and the anchor these vehicles are placed against.
- [ADR-024](./024-transit-live-boards.md): The departure boards; this is the vehicle-position companion it anticipated ("GTFS-RT integration (new ADR)").
