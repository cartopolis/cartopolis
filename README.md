# Viberfox

A networked 3D virtual world built with Rust and Bevy, anchored in the real planet. Walk a street of buildings imported from [OpenStreetMap](https://www.openstreetmap.org/), then keep flying up — through the atmosphere, past the planet turning below you, into space — and drop back down anywhere on Earth. Build in place, and share the world with others in real time.

- **The real world, streamed** — live OSM tiles for the ground; move the anchor and the world re-streams around you.
- **Street to orbit, seamlessly** — one continuous space from the pavement to a full planetary globe, on a floating-origin coordinate model with no loading screens.
- **3D from the map** — buildings raised from OSM footprints, toggleable data layers, live transit boards.
- **Build in place** — create, texture, and reshape prims (path-cut, hollow, twist, taper, slice) with a gizmo and edit dialog.
- **A sky that moves** — physically-based atmosphere with a day/night cycle, sun, moon, and stars.
- **Shared, in real time** — an authoritative headless sim keeps avatars and world state in sync across clients.
- **An AI copilot** — an in-client panel for LLM-assisted world building.

## Screenshots

| | | |
|---|---|---|
| ![Groningen city centre](docs/img/city.png) | ![The same streets at dusk](docs/img/city-dusk.png) | ![Third-person avatar](docs/img/avatar.png) |
| ![Planned route](docs/img/route.png) | ![Atmosphere limb from 120 km](docs/img/horizon.png) | ![Earth from orbit](docs/img/globe.png) |

_Generated with `cargo shots` from the camera positions in [`docs/shots.toml`](docs/shots.toml) — headless, no window, same framing every time._

## Prerequisites

- **Rust** — install via [rustup.rs](https://rustup.rs/).
- **A C toolchain** (for the bundled SQLite):
  - **Linux** — `clang` + `lld`; the workspace links with them (see `.cargo/config.toml`). On Arch: `pacman -S clang lld`. For faster links, switch to `mold` (`fuse-ld=mold`).
  - **macOS** — Xcode Command Line Tools (`xcode-select --install`). No linker config is needed; the default toolchain is used.
  - **Windows** — the MSVC build tools.
- **A GPU with working drivers** — rendering is through `wgpu` (Vulkan on Linux, Metal on macOS, DX12/Vulkan on Windows). On Linux, Wayland and X11 are both supported.

macOS and Windows use no platform-specific code and should build with a stock Rust toolchain; Linux is the primary development target.

## Running

```bash
cargo run -p viberfox
```

Starts in **solo mode**: an in-process sim runs on loopback and the client connects over the same authenticated path an external server uses. Your world is persisted to `data/world.db`.

```bash
cargo run -p viberfox -- --connect <host:port>   # split mode: connect to a live sim
cargo run -p viberfox -- --offline               # no sim: single user, no auth or persistence
```

New here? The [user manual](docs/manual.md) covers controls, getting around, authoring objects, the developer panel, and screenshots.

## Building

```bash
cargo build -p viberfox --release   # → target/release/viberfox (LTO; slow to compile, fastest at runtime)
```

Scope checks to the crate you changed:

```bash
cargo check -p vibe_core     # shared protocol types
cargo check -p vibe_sim      # server only (no Bevy — fast)
cargo check -p viberfox      # Bevy client
```

Faster iteration: `--profile dev-bevy` (deps at full opt) for smoother runtime, or `--features fast-dev` (Bevy dynamic linking, local dev only — never with `--release`).

The sim server runs standalone via `cargo run -p vibe_sim`; configure it with `vibe.toml`, `VIBE_*` env vars, or CLI flags. Deploying? See [`docs/guides/server.md`](docs/guides/server.md).

## Assets

Binary assets — models, textures, fonts, icons, audio, the `docs/img` screenshots — are stored in **Git LFS** by default (`.gitattributes` matches by extension, so a new one needs no setup). Run `git lfs install` before cloning: without it you get pointer files, and since the font, app icon and star catalog are compiled *into* the binary, the build fails at runtime rather than at `cargo build`. See [`assets/README.md`](assets/README.md).

## Architecture

| Crate | Role |
|-------|------|
| `vibe_core` | Shared protocol types and tile/coordinate math |
| `vibe_sim` | Headless TCP simulation server (Tokio + SQLite), no Bevy |
| `viberfox` | Bevy 0.19 game client (rendering, ECS systems, UI) |
| `big_space` | Vendored fork of `big_space` 0.10, ported to Bevy 0.19 for planet-scale floating-origin transforms |

Design docs and the reasoning behind major decisions live under [`docs/`](docs/) — read them before large or cross-cutting changes.

## Troubleshooting

- **Wayland flicker (GLES):** force Vulkan — `WGPU_BACKEND=vulkan cargo run -p viberfox`.
- **Build errors after a toolchain/dep bump:** `cargo clean` and rebuild.

## Credits

- **Avatar characters** — [Quaternius](https://quaternius.com/) *Ultimate Modular Men/Women*, CC0. Consider supporting the work on [Patreon](https://www.patreon.com/quaternius).
- **Map data** — © [OpenStreetMap](https://www.openstreetmap.org/copyright) contributors, under the ODbL.
- **Engine** — built with [Bevy](https://bevy.org/).
