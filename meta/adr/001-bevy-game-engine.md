# ADR-001: Bevy as Game Engine

---
**Metadata:**
- **ID**: ADR-001
- **Status**: Accepted
- **Date**: 2025-03-18
- **Tags**: [technology, rendering, ecs]
- **Decision Type**: technology_choice
- **Related**: [ADR-003]
- **Supersedes**: []
- **Superseded By**: []
---

## Context

**Problem**: Need a 3D game engine for building a virtual world/metaverse-style application with regions, prims, and avatar movement.

**Requirements**:
- 3D rendering with PBR materials
- Entity-Component-System architecture for scalability
- Rust ecosystem (native performance, memory safety)
- Active development and community

## Decision

Use Bevy as the game engine for viberfox.

**Approach**: Bevy 0.19 with a slimmed, explicit feature set (`default-features = false`, see `crates/viberfox/Cargo.toml`) — only the plugins used: `bevy_pbr`, `bevy_render`, `bevy_core_pipeline`, `bevy_post_process` (bloom), `bevy_gltf`, `bevy_animation`, `bevy_scene`, `bevy_winit`, `reflect_auto_register`, etc.

## Rationale

**Primary Reasoning**:
1. **Rust-native**: no FFI overhead
2. **ECS-first**: aligns with data-oriented design for many entities
3. **Modular**: opt into only the features needed
4. **PBR rendering** + built-in atmosphere, bloom, cascaded shadows
5. **Active development**: regular releases

**Alternatives Considered**:

| Alternative | Pros | Cons | Rejected Because |
|------------|------|------|------------------|
| Amethyst | Mature ECS | No longer actively developed | Not a Rust ECS we can build on long-term |
| Godot | Mature, full-featured | GDScript/C++ not Rust | Rust bindings are third-party, not first-class |
| Raw wgpu | Full control | Low-level, boilerplate | Too much work for scope |
| Three.js (WASM) | Web target | JS overhead, different runtime | Not native Rust |

## Consequences

**Positive**:
- Clear separation of concerns via systems
- Built-in camera, lighting, transforms, PBR

**Negative**:
- Bevy API churn between versions (mitigation: pin version, migrate on each bump — the vendored `big_space` fork is re-synced then)
- Smaller ecosystem than Unity/Unreal (mitigation: sufficient for current scope)

## Related

- [ADR-003](./003-ecs-architecture.md): ECS architecture builds on Bevy's ECS
