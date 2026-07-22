# ADR-013: SQLite Migrations and Single Server Writer

---
**Metadata:**
- **ID**: ADR-013
- **Status**: Accepted
- **Date**: 2025-03-21
- **Tags**: [sqlite, storage, migrations, server]
- **Decision Type**: architecture_pattern
- **Related**: [ADR-002, ADR-007, ADR-011]
- **Supersedes**: []
- **Superseded By**: []
---

## Context

**Problem**: [ADR-002](./002-sqlite-storage.md) chose SQLite for regions and prims. A networked sim (ADR-007) must avoid **corruption** from concurrent writers and must **evolve schema** without breaking deployed worlds.

**Requirements**:
- **Exactly one** process opens the DB for **writes** in production (the sim)
- **Versioned migrations** forward (and optionally documented rollback strategy)
- Clients **never** mount the remote SQLite file; they receive **DTOs** (ADR-011)

## Decision

**`vibe_sim`** is the **sole writer** to `regions.db` (or configured path). Schema changes go through checked-in migrations applied on startup.

**Implementation** (`vibe_sim/src/db.rs`, `state.rs`):
- **`rusqlite`** (bundled, ADR-002) + **`refinery`** `embed_migrations!("migrations")` for versioned, transactional upgrades. Migrations run on startup before accepting clients; errors surface via `anyhow`/`tracing`. Current versions: `V1__initial`, `V2__prim_texture`, `V3__prim_geometry_and_surface`.
- **Single writer**: the connection is held as `Arc<Mutex<rusqlite::Connection>>` shared by `SimWorld` and `net.rs` — one serialized write path.
- **Connection discipline**: the client does not open the production DB path in networked mode; it receives DTOs (ADR-011). Local single-player keeps its own SQLite.
- **Backup**: operator copies `.db` before a major upgrade (documented in ADR-014).

## Rationale

**Primary Reasoning**:
1. **SQLite expectation**: Single writer is the safe default; WAL helps readers but not multi-writer apps.
2. **Operational clarity**: The sim owns the world (ADR-007).
3. **Growth**: Migrations unlock new columns for networking ids, timestamps, etc.

**Alternatives Considered**:

| Alternative | Pros | Cons | Rejected Because |
|------------|------|------|------------------|
| Multi-writer SQLite | Theoretical scale | Fragile | Rejected |
| Client direct DB over NFS | “Simple” | Corruption + latency | Rejected |
| Move to Postgres (**`sqlx`**) in v0 | Robust multi-node | Big jump | Defer until needed |

## Consequences

**Positive**:
- Clear ownership with ADR-007/011
- Repeatable upgrades

**Negative**:
- Migration mistakes brick data (mitigation: tests + backup docs)
- Single-node DB until replicated storage ADR (future)

## Related

- [ADR-002](./002-sqlite-storage.md): Original storage choice
- [ADR-007](./007-simulation-vs-client-process-model.md): Sim owns DB
- [ADR-011](./011-static-world-replication-v0.md): Clients get DTOs, not SQL
