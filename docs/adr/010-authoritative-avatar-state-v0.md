# ADR-010: Authoritative Avatar State (v0)

---
**Metadata:**
- **ID**: ADR-010
- **Status**: Accepted
- **Date**: 2025-03-21
- **Tags**: [avatar, networking, authority, simulation]
- **Decision Type**: architecture_pattern
- **Related**: [ADR-003, ADR-006, ADR-009, ADR-012, ADR-016, ADR-031]
- **Supersedes**: []
- **Superseded By**: []
---

> **Tier (ADR-031).** Avatars are a **Simulation-tier** feature: they exist only for a
> logged-in session. The anonymous **Viewer** tier has no avatar — it is a map/lookup
> viewer. Avatar traffic is served only after the ADR-016 auth handshake.

## Context

**Problem**: In a multi-user world, **avatar transform** (position, orientation) must have a single **source of truth** or clients will disagree.

**Requirements**:
- Server **owns** pose at a defined tick rate
- Client sends **intent** (input), not authoritative teleport (except debug tools, gated)
- Same coordinate frame as ADR-006
- Explicit **non-goals** for v0 to avoid scope creep

## Decision

The **simulation** integrates **movement intent** each tick and **broadcasts** authoritative avatar state in the `WorldSnapshot`. Clients **never** set final pose for networked avatars without server confirmation.

**As built** (`crates/vibe_core/src/protocol.rs`, `crates/vibe_sim/src/state.rs`):
- **Intent**: `NetMessage::ClientIntent { request_id, move_x, move_z, display_yaw, fly_up, fly_down }` — a movement vector plus the replicated facing yaw and fly flags. `SimWorld::apply_intent` integrates it; rate-limited server-side to one per **50 ms** (`net.rs` `MIN_INTENT_INTERVAL`).
- **Replicated pose**: `AvatarStateDto { id: u64, position: Vec3, yaw: f32 }` (`glam::Vec3` with serde, aligned with Bevy 0.19), sent inside `WorldSnapshot.avatars`.
- **Tick rate**: fixed sim step, default **20 Hz** (`tick_hz`, config ADR-014).
- **Out of scope v0**: rigid-body physics sync, animation skeleton replication, vehicle controllers.

## Rationale

**Primary Reasoning**:
1. **Anti-cheat / consistency**: server authority is standard for shared worlds.
2. **Matches ECS client**: client-side `Avatar` still exists for rendering; network layer overwrites from server state (ADR-003).
3. **Incremental**: intent starts as a coarse velocity before a full input map.

**Alternatives Considered**:

| Alternative | Pros | Cons | Rejected Because |
|------------|------|------|------------------|
| Client-authoritative pose | Low latency | Trivial exploit | Unacceptable for shared world |
| Lockstep / input history | Fair FPS | Complex | Overkill for v0 |
| Full physics on server | Realistic | Cost + ADR scope | Deferred |

## Consequences

**Positive**:
- Clear contract for ADR-011 (what to include in snapshots/deltas)
- AOI (ADR-012) can key off avatar positions

**Negative**:
- Perceived latency without client prediction (mitigation: optional prediction ADR later)
- Server CPU for all avatars (mitigation: low tick + simple kinematics)

## Related

- [ADR-006](./006-world-coordinate-and-osm-anchor.md): Pose in sim space
- [ADR-009](./009-application-protocol-envelope-v0.md): Intent message kinds
- [ADR-012](./012-interest-management-and-osm-tiles.md): Who receives pose updates
- [ADR-003](./003-ecs-architecture.md): Client ECS applies replicated state
