# ADR-011: Static World Replication (v0)

---
**Metadata:**
- **ID**: ADR-011
- **Status**: Accepted
- **Date**: 2025-03-21
- **Tags**: [replication, networking, regions, prims]
- **Decision Type**: architecture_pattern
- **Related**: [ADR-002, ADR-009, ADR-010, ADR-012, ADR-013]
- **Supersedes**: []
- **Superseded By**: []
---

## Context

**Problem**: Clients need **regions** and **prims** from the authoritative store ([ADR-002](./002-sqlite-storage.md)) without opening SQLite over the network (ADR-013).

**Requirements**:
- **Snapshot** on join (after handshake, ADR-009)
- **Minimal deltas** for v0 (coarse-grained acceptable)
- **Server-assigned ids** for any entity created at runtime
- No **CRDT** / full distributed editing in this ADR

## Decision

Replicate **static world** as **DTOs** (`RegionDto` + `PrimDto`) over ADR-009: an initial `WorldSnapshot { tick, regions, prims, avatars }`, then **small delta** messages as edits occur.

**As built** (`crates/vibe_core/src/protocol.rs`):
- **Deltas**: `PrimUpsert { prim: PrimDto }` (whole-prim replace after create/update) and `PrimRemoved { id: i64 }` (delete-by-id) — no field-level bitfields.
- **Ids**: `i64`, stable in the DB and server-assigned; the server never trusts client-proposed ids for new objects.
- **Serialization**: `serde` DTOs in `vibe_core`, postcard-encoded (same codec as ADR-009).
- **Evolution**: per-client dirty masks / fine-grained component sync are out of scope here.

## Rationale

**Primary Reasoning**:
1. **Matches current data model**: regions and prims already map to SQLite rows.
2. **Fast to ship**: DTO snapshot is simpler than full component-attribute replication.
3. **Room to optimize**: field-level sync can come later without blocking v0.

**Alternatives Considered**:

| Alternative | Pros | Cons | Rejected Because |
|------------|------|------|------------------|
| Full EC-attribute replication | Fine-grained | Huge spec + code | Too ambitious for v0 |
| CRDT scene graph | Offline-friendly | Complexity | Wrong problem for authoritative sim |
| Filesync assets | Simple | Not real-time | Separate asset ADR later |

## Consequences

**Positive**:
- Client can spawn Bevy entities from DTOs (ADR-003)
- Clear extension toward fine-grained sync

**Negative**:
- Large worlds = large snapshots (mitigation: ADR-012 interest + chunking)
- Duplicate data in memory on server and clients (mitigation: acceptable for early scale)

## Related

- [ADR-002](./002-sqlite-storage.md): Source schema for DTOs
- [ADR-009](./009-application-protocol-envelope-v0.md): Message kinds for snapshot/delta
- [ADR-010](./010-authoritative-avatar-state-v0.md): Dynamic avatars alongside static world
- [ADR-012](./012-interest-management-and-osm-tiles.md): What subset to send
- [ADR-013](./013-sqlite-migrations-and-server-writer.md): Sim reads/writes DB
