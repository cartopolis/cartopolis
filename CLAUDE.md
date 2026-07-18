# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## Build commands

Scope builds to the crate you changed — avoid `cargo build` without `-p` unless you need an integration check.

```bash
# Type-check shared protocol types
cargo check -p vibe_core

# Type-check/check server only (no Bevy — fast)
cargo check -p vibe_sim

# Type-check Bevy client
cargo check -p viberfox

# Run the game client (debug, default profile)
cargo run -p viberfox

# Run with playable framerate in debug (deps at opt-level=3; slower cold builds)
cargo run -p viberfox --profile dev-bevy

# Run with fast incremental links via Bevy dynamic linking (local dev only, not for release)
cargo run -p viberfox --features fast-dev

# Run the headless sim server
cargo run -p vibe_sim

# Release build
cargo build -p viberfox --release
```

There are no test suites currently. Use `cargo check -p <crate>` to validate changes.

**Linker:** `.cargo/config.toml` uses `clang + lld` on Linux. If linking is slow, switch to `mold` (`fuse-ld=mold`).

## Architecture

This is a **Cargo workspace** with four crates:

| Crate | Role | Notable deps |
|-------|------|-------------|
| `vibe_core` | Shared protocol types, `NetMessage`, tile/coordinate math | `serde`, `postcard`, `glam` |
| `vibe_sim` | Headless TCP simulation server; SQLite authority; tick loop | `tokio`, `rusqlite`, `refinery` — **no Bevy** |
| `viberfox` | Bevy 0.19 client; rendering, ECS systems, UI | Bevy 0.19 + `big_space` (path dep), `egui` 0.35 |
| `big_space` | **Vendored fork** of `big_space` 0.10 ported to Bevy 0.19 (floating-origin transform propagation, ADR-019). Upstream tops out at Bevy 0.18, so we keep a copy under `crates/big_space` and advance it on each Bevy bump — re-sync the vendored transform-propagation code (`bevy_compat.rs`) against Bevy when upgrading. | Bevy sub-crates |

### Client (`viberfox`) — Bevy ECS layout

- **`components.rs`** — Bevy `Component` types: `Region`, `Prim`, `Avatar`, `RemoteAvatar`, `Selected`, etc.
- **`resources.rs`** — Bevy `Resource` types: `Database`, `GameState`, `AvatarState`, `CameraState`, `ContextMenuState`, `EditDialogState`, `MouseState`, `OsmTileUrlTemplate`, `ConnectAddr`
- **`systems/`** — one file per concern, all registered in `main.rs`:
  - `avatar` — movement, animation, remote avatar interpolation
  - `camera` / `free_camera` — orbit camera, mode toggle
  - `database` — local SQLite load of regions/prims (offline mode)
  - `network` — TCP connection to `vibe_sim`, snapshot apply, intent send
  - `rendering` — spawn/update Bevy meshes for regions and prims
  - `tile_loader` — async OSM tile fetch → region texture
  - `picking` — raycasting for prim selection
  - `ui` — egui context menu + prim edit dialog
  - `gizmo` — transform gizmo toolbar and drag interaction
  - `egui_manager` — custom `EguiPlugin` (wraps `egui-wgpu`); renders egui as the `egui_pass` **system** in the `Core3d` render schedule (Bevy 0.19's schedule-based render graph replaced the old `ViewNode`/render-graph API), drawing onto the post-upscaling sRGB output. Panels use a shared root `Ui` (egui 0.35 shows panels inside a `Ui`, not the `Context`).
- **`db/`** — local SQLite schema init (client-side offline mode only)

**Online vs offline mode:** the client runs in offline mode (local SQLite) by default. Pass `--connect <host:port>` to connect to `vibe_sim`; this replaces the `Database` resource with a network snapshot path.

### Server (`vibe_sim`) — async Tokio

- `config.rs` — layered config: `vibe.toml` → env (`VIBE_*`) → CLI flags
- `db.rs` — SQLite open, `refinery` migrations, world load
- `net.rs` — TCP `handle_connection`, tick loop, `WorldSnapshot` broadcast
- `state.rs` — `SimWorld`: authoritative avatar + prim state, AoI filtering

### Shared (`vibe_core`)

- `protocol.rs` — `NetMessage` enum, `encode/decode_app_frame`, `PROTOCOL_VERSION` (currently **8**)
- `world.rs` — `TileKey`, lat/lng ↔ tile math, `REGION_SIZE_METERS`
- `yaw.rs` — angle helpers for avatar direction continuity

**Wire format (ADR-008–009):** TCP length-delimited frames, little-endian `u32` length; frame body = `protocol_version` (u8) + `message_kind` (u8) + `request_id` (u64) + postcard payload.

## Architecture docs (meta → ADR → code)

Architecture documentation lives under `meta/` (not `docs/`):

- `meta/abstract/` — **why**: vision (`G-*` goals), principles (`P-*`), roadmap tiers (P0–P2), ADR resolution depth (R0–R3), capability map
- `meta/adr/` — **what we chose**: one file per decision; `index.md` lists all ADRs

**Before large or cross-cutting changes**, read in this order:
1. `meta/abstract/vision.md` — goals G-01…G-08
2. `meta/abstract/principles.md` — principles P-01…P-07
3. `meta/abstract/roadmap.md` — P0/P1/P2 priorities
4. `meta/abstract/adr-resolution.md` — R0–R3 depth guide
5. `meta/adr/index.md` — existing decisions

**Create or supersede an ADR** for: wire protocol changes, storage schema, security boundaries, major new dependencies, sim/client process boundary changes. Skip for bug fixes or refactors within an existing ADR's scope.

New ADR checklist: copy `meta/adr/template.md`, set `Aligns with: G-*, P-*`, add a row to `meta/adr/index.md` in the same commit. Match depth to R0–R3 (not every change needs a full R2 ADR).

## Known issues / workarounds

- **Atmosphere is Bevy 0.19's built-in `bevy_pbr` atmosphere** (not `bevy_atmosphere`, which was dropped). It's planetary-scale, so `free_camera::setup_camera` spawns it on a **planet-centered entity** 6,360 km below the local ground (surface at `y = 0`). The camera carries `AtmosphereSettings` + `AtmosphereEnvironmentMapLight` + a physical `Exposure`, the sun runs at `RAW_SUNLIGHT`, and `apply_day_night_cycle` adapts exposure by time of day. **Space tier:** `globe::update_globe_visibility` **removes** both `AtmosphereSettings` and `AtmosphereEnvironmentMapLight` from the camera once the globe is shown, and re-adds them on descent. This was a deliberate walk-back from switching the atmosphere to `AtmosphereMode::Raymarched` in space: near-ground `LookupTexture` washes the distant globe out (its aerial-perspective LUT clamps at ~32 km), but Raymarched integrates per-fragment and, at the space-tier far plane, overran the GPU command-buffer timeout on an Intel iGPU (`VK_ERROR_DEVICE_LOST`). Dropping the camera from the atmosphere + env-map passes entirely is cheap and leaves the globe rendering untinted against black space with the star field. The globe-tier near/far override lives in `free_camera::camera_controls` **after** the mode match so it applies in every camera mode (a far plane stuck at 200 km otherwise clips the whole globe → black screen when high).
- **Two directional lights must have matching shadow cascades:** Bevy 0.19's `bevy_light` panics (`index out of bounds` in cascade-visibility) if the sun and moon `DirectionalLight`s have **different `num_cascades`**, or if `shadow_maps_enabled` is toggled at runtime. Keep both at `num_cascades: 4` and never toggle shadows after spawn (see `setup_sky` / `apply_day_night_cycle`).
- **`reflect_auto_register` bevy feature is required:** glTF scenes load as `WorldAsset`s spawned by `bevy_world_serialization`, which reflect-spawns every component; without it, spawning the avatar fox panics on the first unregistered type.
- **Fox glTF animations don't play (Bevy 0.19):** the glTF animation asset labels changed; `GltfAssetLabel::Animation(n)` no longer resolves for `Fox.glb`, so the avatar mesh spawns (via `WorldAssetRoot`) but stays in its rest pose (see `avatar::spawn_avatar`). TODO.
- **Wayland + GLES flicker:** force Vulkan with `WGPU_BACKEND=vulkan cargo run -p viberfox`.
- **Database migrations:** copy `data/regions.db` before schema upgrades; `refinery` migrations run automatically on `vibe_sim` startup (ADR-013).
- **`fast-dev` feature** uses Bevy dynamic linking — never use it for `--release` or distributable binaries.
