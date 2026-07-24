# Viberfox

A networked 3D virtual world built with Rust and Bevy, anchored in the real planet. Walk a street lined with buildings imported straight from [OpenStreetMap](https://www.openstreetmap.org/), then just keep flying up — through the atmosphere, past the whole planet turning below you, out into space — and drop back down anywhere on Earth. Build in place, and share the world with others in real time.

What's in the box:

- **The real world, streamed** — the ground is live OSM map tiles; move the anchor and the world re-streams around wherever you land.
- **Street to orbit, seamlessly** — one continuous space from the pavement to a full planetary globe and space, held together by a floating-origin coordinate model with no precision loss and no loading screens.
- **3D from the map** — buildings raised from OpenStreetMap footprints, plus toggleable data layers and live transit boards.
- **Build in place** — create, texture, and reshape prims (path-cut, hollow, twist, taper, slice) with a transform gizmo and edit dialog.
- **A sky that moves** — a physically-based atmosphere with a day/night cycle, sun, moon, and stars.
- **Shared, in real time** — an authoritative headless sim server keeps avatars and world state in sync across connected clients.
- **An AI copilot** — an in-client panel for LLM-assisted world building.

## Screenshots

| | | |
|---|---|---|
| ![Groningen city centre](docs/img/city.png) | ![The same streets at dusk](docs/img/city-dusk.png) | ![Third-person avatar](docs/img/avatar.png) |
| ![Planned route](docs/img/route.png) | ![Atmosphere limb from 120 km](docs/img/horizon.png) | ![Earth from orbit](docs/img/globe.png) |

_Generated with `cargo shots` from the remembered camera positions in
[`docs/shots.toml`](docs/shots.toml) — headless, no window, same framing every
time (see the [manual](docs/manual.md#taking-screenshots))._

## Prerequisites

- **Rust** — install via [rustup.rs](https://rustup.rs/).
- **Clang + LLD** — the workspace links with `clang` + `lld` (see `.cargo/config.toml`). On Arch: `pacman -S clang lld` (install the equivalents for your distro). For faster links on large binaries you can switch to `mold` (`pacman -S mold`) and set `fuse-ld=mold` in `.cargo/config.toml`.
- **A Vulkan-capable GPU + drivers** — the client renders through `wgpu`. Make sure your Vulkan loader and GPU driver are installed (e.g. Mesa for AMD/Intel, the proprietary driver for NVIDIA).

Wayland and X11 are both supported.

## Running

```bash
cargo run -p viberfox
```

This starts the client in **solo mode**: an in-process sim runs on loopback and the client connects to it over the same authenticated path an external server uses. Your world (avatar, objects) is authoritative in that sim and persisted to `data/world.db`.

New here? The [user manual](docs/manual.md) covers getting around, authoring objects, the developer panel, and taking screenshots.

To connect to a live sim server instead (split mode):

```bash
cargo run -p viberfox -- --connect <host:port>
```

To run with no sim at all — single user, no auth, no persistence (what `--shot` uses):

```bash
cargo run -p viberfox -- --offline
```

### Controls

| Input | Action |
|-------|--------|
| W / S (or ↑ / ↓) | Move forward / backward (camera-relative) |
| A / D (or ← / →) | Strafe left / right |
| Space / Shift | Fly up / down (while flying) |
| F | Toggle fly / walk |
| Tab | Cycle camera: Avatar → Free → Orbit (Earth) |
| Left-drag | Orbit camera |
| Left-click | Select a prim |
| Shift + Left-click | Add / remove a prim from the selection |
| Ctrl + Left-drag | Marquee (rubber-band) multi-select |
| Right-click | Context menu (create / edit / delete prims) |
| T / R / S | Gizmo mode: translate / rotate / scale |
| Delete / Backspace | Delete selected prims |
| Ctrl + Shift + D | Toggle the developer panel (day/night slider, etc.) |
| Ctrl + Shift + A | Toggle the AI assistant panel |

## Building

```bash
# Release binary (LTO + single codegen unit — slow to compile, fastest at runtime)
cargo build -p viberfox --release
# Output: target/release/viberfox
```

## Development

### Faster iteration

If the client feels sluggish in debug mode, use the `dev-bevy` profile to compile dependencies at full optimization (slower first build, smoother runtime):

```bash
cargo run -p viberfox --profile dev-bevy
```

For faster incremental **links** during UI/gameplay work:

```bash
cargo run -p viberfox --features fast-dev   # Bevy dynamic linking — local dev only
```

Do not combine `fast-dev` with `--release`.

### Scoped builds

The workspace has four crates. Build only what you changed — avoid a bare `cargo build` unless you need a full integration check:

```bash
cargo check -p vibe_core      # shared protocol types
cargo check -p vibe_sim       # server only (no Bevy — fast)
cargo check -p viberfox       # Bevy client
```

There are no test suites currently; use `cargo check -p <crate>` to validate changes.

### Running the sim server

```bash
cargo run -p vibe_sim
```

Config can be set via `vibe.toml`, environment variables (`VIBE_*`), or CLI flags:

```bash
cargo run -p vibe_sim -- --listen 0.0.0.0:4747 --database-path ./data/regions.db
```

Deploying the server to a host? See [`meta/guides/server.md`](meta/guides/server.md).

### Assets

The client loads from `assets/` at the workspace root. The avatar character models
(`assets/models/avatar/characters/`) are stored in **Git LFS**, so install it once
(`git lfs install`) before cloning — otherwise you get pointer files and avatars
won't render. See [`assets/README.md`](assets/README.md) for details.

## Architecture

Four crates in the workspace:

| Crate | Role |
|-------|------|
| `vibe_core` | Shared protocol types and tile/coordinate math |
| `vibe_sim` | Headless TCP simulation server (Tokio + SQLite), no Bevy |
| `viberfox` | Bevy 0.19 game client (rendering, ECS systems, UI) |
| `big_space` | Vendored fork of `big_space` 0.10, ported to Bevy 0.19 for planet-scale floating-origin transforms |

The client uses Bevy's ECS. Adding a feature means defining components/resources and registering a system in `viberfox/src/main.rs`. Design docs and the reasoning behind major decisions live under [`meta/`](meta/); read them before large or cross-cutting changes.

## Troubleshooting

**Wayland rendering issues (flicker under GLES):** force the Vulkan backend:

```bash
WGPU_BACKEND=vulkan cargo run -p viberfox
```

**Build errors after a toolchain or dependency bump:** `cargo clean` and rebuild usually clears stale incremental state.

## Credits

Viberfox stands on generous open work:

- **Avatar characters** — [Quaternius](https://quaternius.com/), *Ultimate Modular Men* and *Ultimate Modular Women* (character meshes + animations), released under **CC0**. They're a joy to build on — consider supporting the work on [Patreon](https://www.patreon.com/quaternius).
- **Map data** — © [OpenStreetMap](https://www.openstreetmap.org/copyright) contributors, under the ODbL. Building footprints, streets, and place names are derived from OSM.
- **Engine** — built with [Bevy](https://bevy.org/).
