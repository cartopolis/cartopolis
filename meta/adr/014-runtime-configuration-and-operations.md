# ADR-014: Runtime Configuration and Operations

---
**Metadata:**
- **ID**: ADR-014
- **Status**: Accepted
- **Date**: 2025-03-21
- **Tags**: [config, ops, deployment]
- **Decision Type**: architecture_pattern
- **Related**: [ADR-007, ADR-008, ADR-012, ADR-013, ADR-016]
- **Supersedes**: []
- **Superseded By**: []
---

## Context

**Problem**: Running `vibe_sim` on a host (ADR-007) requires **listen address**, **database path**, **tile URL template**, **texture dir**, **tick rate**, and **AOI radius** (ADR-012) without recompiling.

**Requirements**:
- Sensible defaults for local dev
- **Overrides** via env vars and/or config file
- Documented for minimal VPS deployment

## Decision

**Layered config**: defaults in code → optional `vibe.toml` → `VIBE_*` env vars → CLI flags (`clap`), each layer overriding the previous.

**Implementation** (`vibe_sim/src/config.rs`, `cli.rs`):
- **`figment`** with `Serialized::defaults` + `Toml` + `Env::prefixed("VIBE_")` providers on a `serde`-derived `SimConfig`. `apply_cli` layers `clap` flag overrides on top.
- Logging: **`tracing`** + **`tracing-subscriber`** with `EnvFilter`.

**Config keys** (all with in-code defaults):
- `listen` (`127.0.0.1:4747`) — bind host:port, ADR-008
- `database_path` (`data/world.db`) — ADR-013
- `tick_hz` (`20.0`), `aoi_radius` (`500.0`) — ADR-012
- `osm_tile_url_template` (`https://tile.openstreetmap.org/{z}/{x}/{y}.png`) — client tile handoff, ADR-004
- `texture_dir` (`data/textures`) — directory of `*.png` textures served to clients

`vibe.toml` is loaded only if present; env keys are `VIBE_<field>` (e.g. `VIBE_listen`).

## Rationale

**Primary Reasoning**:
1. **12-factor** style env overrides are standard for containers.
2. **File config** matches operator expectations for a dedicated server.
3. **Separation**: Keeps secrets out of source control.

**Alternatives Considered**:

| Alternative | Pros | Cons | Rejected Because |
|------------|------|------|------------------|
| Env only | Simple | Tedious for many keys | File + env is nicer |
| CLI only | Explicit | Poor for services | Not sufficient alone |
| Dynamic remote config | Fancy | Ops burden | Future |

## Consequences

**Positive**:
- Repeatable deploys; easy to document “minimum viable server”
- Ties ADR-007–013 together operationally

**Negative**:
- Config schema must be versioned with protocol (mitigation: document breaking changes in CHANGELOG)

## Related

- [ADR-007](./007-simulation-vs-client-process-model.md): Sim binary reads config
- [ADR-008](./008-network-transport-layer.md): Listen address/port
- [ADR-012](./012-interest-management-and-osm-tiles.md): AOI defaults
- [ADR-013](./013-sqlite-migrations-and-server-writer.md): DB path
