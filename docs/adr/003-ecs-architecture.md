# ADR-003: ECS Architecture with Bevy

---
**Metadata:**
- **ID**: ADR-003
- **Status**: Accepted
- **Date**: 2025-03-18
- **Tags**: [architecture, ecs]
- **Decision Type**: architecture_pattern
- **Related**: [ADR-001, ADR-002]
- **Supersedes**: []
- **Superseded By**: []
---

## Context

**Problem**: Organize game logic, rendering, and data loading in a scalable way that supports regions, prims, avatar, camera, and tile loading.

**Requirements**:
- Clear separation of concerns
- Deterministic system ordering
- Resources for shared state (DB, cache)
- Components for entities (Avatar, RegionTile, etc.)

## Decision

Use Bevy's ECS with one system file per concern under `crates/cartopolis/src/systems/` (database, rendering, tile_loader, network, avatar, camera/free_camera, picking, gizmo, ui, globe, …), all registered in `main.rs`.

**Approach**:
- **Resources** (`resources.rs`, plus a few local to their system): `Database`, `GameState`, `AvatarState`, `CameraState`, `MouseState`, `TileCache`
- **Components** (`components.rs`): `Region`, `Prim`, `Avatar`, `RemoteAvatar`, `Selected`, `RegionTile`, …
- **Systems**: Startup for init, Update for per-frame logic; explicit ordering via `.after()` and `run_if`

## Rationale

**Primary Reasoning**:
1. **Data-oriented**: Entities as data, systems as pure logic
2. **Parallelism**: Bevy schedules systems in parallel when possible
3. **Ordering**: Explicit `.after()` for dependencies (e.g. `spawn_regions` after `load_regions`)
4. **Run conditions**: `run_if` for optional systems (e.g. database systems when DB present)

**Alternatives Considered**:

| Alternative | Pros | Cons | Rejected Because |
|------------|------|------|------------------|
| OOP | Familiar | Tight coupling | Bevy is ECS |
| Pure functions | Simple | Hard to share state | ECS is standard |
| Event-driven only | Loose coupling | Harder to reason about order | Bevy systems are clearer |

## Consequences

**Positive**:
- Systems are modular and testable
- Clear dependencies and ordering in `main.rs`
- New concerns drop in as a system file (e.g. `network.rs` added without touching existing systems)

**Negative**:
- ECS learning curve (mitigation: Bevy docs)
- System ordering must be explicit

## Related

- [ADR-001](./001-bevy-game-engine.md): Bevy provides the ECS
- [ADR-002](./002-sqlite-storage.md): Database loads into ECS via systems
- Registration/ordering: `crates/cartopolis/src/main.rs`
