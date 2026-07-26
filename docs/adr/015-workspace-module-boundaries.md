# ADR-015: Workspace Module and Crate Boundaries

---
**Metadata:**
- **ID**: ADR-015
- **Status**: Accepted
- **Date**: 2025-03-21
- **Tags**: [workspace, crates, architecture]
- **Decision Type**: architecture_pattern
- **Related**: [ADR-003, ADR-007, ADR-008, ADR-009]
- **Supersedes**: []
- **Superseded By**: []
---

## Context

**Problem**: Networking (ADR-008/009), coordinates (ADR-006), and protocol types **outgrow** a single `main.rs` + flat `src/`. Without crate boundaries, sim and client **duplicate** logic or create import cycles.

**Requirements**:
- **`vibe_core`**: shared types, protocol, coordinate math — **no** Bevy dependency
- **`vibe_sim`**: I/O, DB, tick loop — depends on `vibe_core` + Tokio
- **`viberfox`**: Bevy client — depends on `vibe_core`
- Clear rule: **where new message types live**

## Decision

The Cargo workspace has **four members** (`Cargo.toml`), with **strict dependency direction** — both binaries depend on `vibe_core`, never on each other:

- **`vibe_core`** — shared protocol (`vibe_core::protocol`), coordinate/tile math (`vibe_core::world`, ADR-006). No Bevy.
- **`vibe_sim`** — headless Tokio server, SQLite authority, tick loop. Depends on `vibe_core`. Exposes both a **lib** (`serve`, `SimHandle`, `ensure_account`) and a thin bin, so the server can run standalone *or* embedded in-process by the client's solo/dev mode (ADR-016).
- **`viberfox`** — Bevy 0.19 client; all Bevy systems live here. Depends on `vibe_core`, and — behind a default-on **`solo`** feature — on the `vibe_sim` lib to run an in-process server for one-command dev (ADR-016). This is the **one sanctioned exception** to "binaries never depend on each other"; it is feature-gated and off for lean/release client builds. Solo mode still talks to the embedded sim over loopback TCP, so the boundary stays a real client/server split.
- **`big_space`** — **vendored fork** of `big_space` 0.10 ported to Bevy 0.19 (floating-origin transform propagation, ADR-019). Kept in-tree because upstream tops out at Bevy 0.18.

A fifth crate **`vibe_geo`** (geo/place lookups) is **planned, not yet built** (ADR-028).

**Rust ecosystem**: root `[workspace.dependencies]` pins aligned versions of `tokio`, `serde`, `tracing`, `thiserror`, `glam`, etc. Protocol/codec hot paths could later carry `criterion` benches in `vibe_core`.

## Rationale

**Primary Reasoning**:
1. **Compile times**: Smaller crates rebuild faster during client-only iteration.
2. **Correct layering**: Prevents accidental renderer imports in the sim.
3. **Matches ADR-007**: Two binaries naturally map to two crate roots + shared lib.

**Alternatives Considered**:

| Alternative | Pros | Cons | Rejected Because |
|------------|------|------|------------------|
| Monorepo single crate | Simplest now | Becomes tangled | Will hurt at ADR-008 |
| Many micro-crates | Max isolation | Overhead | Too many for current size |
| Feature flags only | One package | Still couples deps | Weak boundary vs crates |

## Consequences

**Positive**:
- Enforces ADR-008/009 ownership (codec in core)
- Easier testing of protocol without GPU

**Negative**:
- Initial refactor cost (mitigation: incremental move starting with types)
- Public API on `vibe_core` needs semver care (mitigation: keep internal until stable)

## Related

- [ADR-007](./007-simulation-vs-client-process-model.md): Binaries map to crates
- [ADR-008](./008-network-transport-layer.md): Transport impl likely `vibe_sim` (or `vibe_core` if shared with test harness)
- [ADR-009](./009-application-protocol-envelope-v0.md): Envelope types in `vibe_core`
- [ADR-003](./003-ecs-architecture.md): ECS remains client-focused unless sim adopts Bevy later
