# ADR-002: SQLite for Region and Prim Storage

---
**Metadata:**
- **ID**: ADR-002
- **Status**: Accepted
- **Date**: 2025-03-18
- **Tags**: [storage, database]
- **Decision Type**: technology_choice
- **Related**: []
- **Supersedes**: []
- **Superseded By**: []
---

## Context

**Problem**: Need persistent storage for regions (geographic areas with tile coordinates) and prims (3D primitives: boxes, spheres, cylinders, cones, toruses) with position, rotation, scale, and color.

**Requirements**:
- Local persistence
- Simple schema (regions, prims with FK to regions)
- No extra server process
- Rust ecosystem (rusqlite)

## Decision

Use SQLite via rusqlite (`bundled`) for region and prim storage.

**Approach**: Single file `data/world.db`, **owned exclusively by `cartopolis_simulator`** and versioned with `refinery` SQL migrations (`crates/cartopolis_simulator/migrations/`, applied on startup). It holds prims (geo-anchored, ADR-029), accounts (ADR-016), chat scrollback (ADR-028), and textures. The **client keeps no local SQLite**: the anonymous Viewer tier (ADR-031) is a streamed map viewer, and all authored state is online and server-authoritative. (Solo/dev mode runs the same `cartopolis_simulator` in-process against `data/world.db` — ADR-016.)

## Rationale

**Primary Reasoning**:
1. **Zero-config**: Single file, no server
2. **Mature**: SQLite is battle-tested and reliable
3. **rusqlite**: Rust bindings, no extra dependencies
4. **ACID**: Transactions for consistency
5. **Portable**: Copy `data/` folder to move data

**Alternatives Considered**:

| Alternative | Pros | Cons | Rejected Because |
|------------|------|------|------------------|
| JSON files | Simple | No schema, no querying | Need relational queries |
| PostgreSQL | Full-featured | Requires server | Overkill for local use |
| sled | Embedded, Rust | Key-value | Need relational model |
| SQLite | Embedded, SQL | Single writer | Acceptable for single-user |

## Consequences

**Positive**:
- Easy to inspect with `sqlite3 data/world.db`
- Versioned via `refinery` migrations on the server (ADR-013)

**Negative**:
- Single writer (mitigation: acceptable for single-user client / server serializes writes)
- No built-in replication (mitigation: out of scope)

## Related

- Client schema: `crates/cartopolis/src/db/schema.rs`; load system: `crates/cartopolis/src/systems/database.rs`
- Server: `crates/cartopolis_simulator/src/db.rs`, `crates/cartopolis_simulator/migrations/`
