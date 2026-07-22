# ADR-007: Simulation vs Client Process Model

---
**Metadata:**
- **ID**: ADR-007
- **Status**: Accepted
- **Date**: 2025-03-21
- **Tags**: [server, client, headless, architecture]
- **Decision Type**: architecture_pattern
- **Related**: [ADR-003, ADR-008, ADR-013, ADR-015]
- **Supersedes**: []
- **Superseded By**: []
---

## Context

**Problem**: A virtual world **server** needs an **authoritative** loop (time step, world mutations) separate from **presentation** (rendering). A single Bevy process with local input and SQLite can't host multiple clients.

**Requirements**:
- Clear ownership: which process reads/writes SQLite, which accepts network clients
- Headless operation for hosting (no GPU)
- Room to grow without microservices or duplicate business logic

## Decision

Split into **simulation** (headless, authoritative state, DB writer) and **client** (Bevy render + input + network consumer) as **separate binaries** sharing a **`vibe_core`** library.

- **`vibe_sim`**: Tokio; fixed-tick loop; loads config (ADR-014); owns network listen; applies intents; persists via ADR-013.
- **`viberfox`** (Bevy client): **offline by default** (local SQLite); `--connect host:port` switches to online — connect to sim, spawn entities from snapshots, send intent.
  - **Update (2026, ADR-016/031):** the default is now **solo** — an in-process `vibe_sim` on loopback reached through the authenticated online path, so the client is server-authoritative in every mode. `--connect host:port` = external sim; `--offline` = no sim at all (single user, no auth). The old client-side local-SQLite world was removed (no `Database`/`db/` on the client); persistence lives only in `vibe_sim`.
- **Shared `vibe_core`**: types, coordinate model (ADR-006), protocol codec (ADR-008/009), region/prim DTOs.

Both binaries use **`tracing`** for structured logs (ADR-014).

## Rationale

**Primary Reasoning**:
1. **Authority boundary**: Standard "sim vs viewer" split.
2. **Deployment**: VPS can run `vibe_sim` only (ADR-014).
3. **Testing**: Sim binary can run integration tests without a window.

**Alternatives Considered**:

| Alternative | Pros | Cons | Rejected Because |
|------------|------|------|------------------|
| One binary + `--server` flag | Fewer crates | Tangles features | Becomes unmaintainable as net code grows |
| Microservices | Scale story | Ops complexity | Out of scope for gradual build |
| Client-authoritative | Easiest multiplayer hack | Cheating, divergence | Virtual world expectation is server authority |

## Consequences

**Positive**:
- Forced clean API between sim and render paths
- Aligns with ADR-008 (transport owned by sim) and ADR-013 (single DB writer)

**Negative**:
- Two binaries to build and version together (mitigation: workspace crate, shared semver)
- Refactor cost from monolithic `main.rs` (mitigation: ADR-015 boundaries)

## Related

- [ADR-003](./003-ecs-architecture.md): Client keeps Bevy ECS; sim may use ECS or minimal state (implementation choice after this ADR)
- [ADR-008](./008-network-transport-layer.md): Sim accepts connections
- [ADR-013](./013-sqlite-migrations-and-server-writer.md): Only sim opens DB for writes
- [ADR-015](./015-workspace-module-boundaries.md): Crate layout for `vibe_core` / `vibe_sim` / `viberfox`
