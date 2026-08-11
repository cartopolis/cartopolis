# ADR-017: Prim Authoring and Editing (v0)

---
**Metadata:**
- **ID**: ADR-017
- **Status**: Accepted
- **Date**: 2025-07-14
- **Tags**: [authoring, prims, ui, protocol, persistence, textures, replication]
- **Decision Type**: architecture_pattern
- **Aligns with**: [G-01, G-02, P-01, P-02, P-04, P-05]
- **Roadmap tier**: P0 — core authoring loop
- **Doc resolution**: R2
- **Related**: [ADR-002, ADR-009, ADR-011, ADR-013, ADR-015, ADR-018]
- **Supersedes**: []
- **Superseded By**: []
---

## Context

**Problem**: The prim pipeline was **read-only** — prims loaded from SQLite at startup, broadcast inside `WorldSnapshot`, rendered on clients, with no mutation path at any layer. That blocks G-01 (authoring): without placing and editing objects the world is static.

**Requirements**:
- Authenticated client can **create** a prim (right-click ground → "Create prim"), **select** one (click), **edit** it (context menu → dialog + transform gizmo), and **delete** it.
- Edits replicate live to all clients (~one tick, 50 ms at 20 Hz).
- All mutations are **server-authoritative** (P-01) and persisted to SQLite (ADR-013).
- Assignable texture from a standard library.

## Decision

**Add client→server prim CRUD to the protocol, apply it server-side with immediate DB persistence and delta broadcast, and drive it from an in-house egui UI** (context menu, edit dialog, transform gizmo) with hand-rolled raycast selection.

**Protocol** (`cartopolis_core/src/protocol.rs`) — variants and wire kinds:

| Variant | Dir | Kind | Purpose |
|---------|-----|------|---------|
| `CreatePrim` | C→S | 8 | New prim with full initial state (pos/rot/scale/color/shape/texture/surface/geometry) |
| `UpdatePrim` | C→S | 9 | Updated fields (transform, color, texture, name, surface, geometry) |
| `DeletePrim` | C→S | 10 | Delete by server-assigned id |
| `PrimUpsert` | S→C | 11 | Authoritative full prim state after a create/update |
| `PrimRemoved` | S→C | 7 | Emitted on delete |
| `TextureCatalog*` / `TextureRequest` / `TextureData` | C↔S | 12–16 | Standard texture library, streamed from the sim |

`PrimDto` gained `texture_id: Option<String>` and (later, ADR-018) the `PrimSurface` + `PrimGeometry` fields, all `#[serde(default)]` for rollout. This ADR introduced the CRUD kinds; ADR-018 (v8) added surface + geometry. The current wire version and the full revision log live in `PROTOCOL_HISTORY` (`crates/cartopolis_core/src/protocol.rs`).

**Server** (`cartopolis_simulator`): `SimWorld` holds an `Arc<Mutex<Connection>>` (connection kept alive past `load_world`) and gains `add_prim`/`update_prim`/`remove_prim`, each persisting via `db.rs` and returning the authoritative `PrimDto`. `net.rs` handles the CRUD messages and broadcasts `PrimUpsert`/`PrimRemoved` **immediately** on mutation (not at the next tick); the full `WorldSnapshot` remains the consistency fallback. Migration `V2` added the `texture_id` column.

**Client** (`cartopolis`):
- **Selection** — hand-rolled ray/mesh picking in `picking.rs` (no external raycast crate); `Selected` marker + `ContextMenuState`.
- **UI** — an **in-house egui plugin** (`egui_manager.rs`, egui 0.35 + egui-wgpu; not `bevy_egui`): context menu ("Create/Edit/Delete") and an edit dialog (name, shape, transform, color, texture grid). Field changes send `UpdatePrim` live.
- **Gizmo** — in-house translate/rotate/scale handles + toolbar (`gizmo.rs`), keyboard **T/R/S**, updates throttled to the tick rate.
- **Textures** — a standard library streamed from the sim (`TextureCatalog`/`TextureData`), keyed by string id stored in `PrimDto::texture_id`, applied as `base_color_texture` on `StandardMaterial`.

Implemented across `cartopolis_core/src/protocol.rs`, `cartopolis_simulator/src/{state,net,db}.rs`, and `cartopolis/src/systems/{picking,ui,gizmo,prim_ops,rendering,egui_manager}.rs`.

## Rationale

1. **Server-authoritative (P-01)**: all changes flow client→server→broadcast; the client never optimistically mutates shared state, avoiding desync (ADR-010/011).
2. **Immediate delta broadcast**: `PrimUpsert`/`PrimRemoved` on mutation give sub-tick edit latency; the full snapshot stays as the fallback.
3. **In-house egui plugin**: an off-the-shelf integration was avoided so the UI tracks our own Bevy version and render-graph wiring (the egui pass lives in `Core3d`); Bevy's native `bevy_ui` lacks editor widgets (text/dropdown/colour).
4. **String-keyed textures**: human-readable ids that survive serialization and DB storage; no UUIDs for a bundled/served library.

**Alternatives Considered**:

| Alternative | Pros | Cons | Rejected Because |
|------------|------|------|------------------|
| Client-authoritative prims | Lower edit latency | Breaks P-01; multi-editor desync | Violates the authority model |
| Full snapshot per change (no deltas) | No new messages | 50 ms floor; re-sends unchanged prims | `PrimRemoved` already set the delta precedent |
| Off-the-shelf UI/gizmo crates (`bevy_egui`, `bevy_transform_gizmo`, `bevy_mod_raycast`) | Less code upfront | Version lag against our Bevy bumps; thinner control of render wiring | Built in-house for control; each was replaced by a local module |
| User-uploaded custom textures | More powerful | Needs asset pipeline, storage, abuse moderation (P-04) | Deferred to a future Asset Pipeline ADR |
| Collaborative per-prim locking | Prevents conflicts | UX friction; lock/crash recovery | Overkill for v0; last-write-wins is acceptable at low user counts |

## Consequences

**Positive**:
- The core authoring loop (G-01) works: create, transform, texture, delete in a shared world, with live replication to other clients (G-02).
- The client-intent → server-mutation → delta-broadcast pattern is reused for later mutation types.

**Negative**:
- **Last-write-wins**: simultaneous edits of one prim resolve to the last `UpdatePrim` (acceptable at v0 scale; locking can layer on later).
- **Live DB connection**: the load-once-and-drop pattern became `Arc<Mutex<Connection>>` (SQLite serializes writes via the mutex).
- **Protocol bumps** require coordinated client+server deploys (version check enforced).

## Related

- [ADR-002](./002-sqlite-storage.md): Persistence layer; V2 migration adds `texture_id`.
- [ADR-009](./009-application-protocol-envelope-v0.md): Envelope carries the new kinds (7–16).
- [ADR-011](./011-static-world-replication-v0.md): Graduates prims from static to mutable via delta messages.
- [ADR-013](./013-sqlite-migrations-and-server-writer.md): Single-writer; adds the migration and write functions.
- [ADR-015](./015-workspace-module-boundaries.md): Shared types in `cartopolis_core`, mutation in `cartopolis_simulator`, UI in `cartopolis`.
- [ADR-018](./018-prim-texture-surface-params.md): Extends prims with surface + geometry params (protocol v8).
