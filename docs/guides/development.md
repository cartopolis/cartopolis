# Development

Onboarding for new contributors. Prerequisites, first run and the crate layout are
in the [README](../../README.md); this is what comes after it.

## Before anything else: Git LFS

`git lfs install` **before** cloning. Binary assets are in LFS, and four of them
(brand font, app icon, star catalog, tile fixtures) are `include_bytes!`-ed at
compile time — without the smudge filter `cargo build` embeds 130-byte pointer
files and succeeds, then panics at runtime. Same reason every CI job sets
`lfs: true`.

## The loop

| Task | Command |
|------|---------|
| Type-check what you touched | `cargo check -p vibe_core \| vibe_sim \| viberfox` |
| Play it | `cargo run -p viberfox` |
| Playable framerate in debug | `cargo run -p viberfox --profile dev-bevy` |
| Fast incremental links (local only) | `cargo run -p viberfox --features fast-dev` |
| Client tests | `cargo test -p viberfox --bin viberfox` |
| Server tests | `cargo test -p vibe_sim` |
| Floating-origin tests | `cargo test -p big_space --lib` |
| Regenerate README images | `cargo shots` |

Two traps: `viberfox` has **no lib target**, so `--lib` fails and `--bin viberfox`
is required; and `cargo check` does **not** compile `cfg(test)` code, so a green
check says nothing about the suite. Run the tests when touching egui, geometry or
tile parsing.

`#[ignore]`-d tests are diagnostics, not tests — they dump a PNG, print a tile's
layers, time a cell, or solve a real route against the network. Run one by name:
`cargo test -p viberfox --bin viberfox dump_png -- --ignored --nocapture`.

Verify a visual change **headless** rather than by flying the camera there:
`cargo shots --only globe` for a README preset, or `--shot` for an arbitrary
viewpoint, sweep or CSV of the ramp scalars. Full flag list in
[`CLAUDE.md`](../../CLAUDE.md#scripted-screenshots---shot).

## Where things live

| Path | Contents |
|------|----------|
| `crates/vibe_core/` | Wire protocol (`protocol.rs`), tile/coordinate math (`world.rs`, `geo.rs`) |
| `crates/vibe_sim/` | Authoritative server: `net.rs` (tick loop), `state.rs` (world), `db.rs`, `auth.rs`, `config.rs` |
| `crates/viberfox/` | Bevy client — everything below |
| `crates/big_space/` | Vendored floating-origin fork; re-sync `bevy_compat.rs` on every Bevy bump |

Inside the client, `components.rs` / `resources.rs` hold the ECS types and
`systems/` is one file per concern, registered in ordered phases in `main.rs`:

| Group | Files |
|-------|-------|
| Map streaming | `map_stream`, `tile_loader`, `vector_tiles`, `map_geometry`, `osm_buildings`, `lod22`, `road_texture`, `vegetation`, `water` |
| Planet & sky | `globe`, `globe_ocean`, `globe_weather`, `weather` |
| Avatar & camera | `avatar`, `free_camera`, `collision`, `picking`, `gizmo` |
| Networking | `network`, `chat`, `../login.rs`, `../solo.rs` |
| Navigation | `geo_nav`, `routing`, `navigation`, `driver`, `transit_live`, `transit_vehicles` |
| UI | `egui_manager`, `theme`, `ui`, `hud`, `splash`, `data_layers`, `bookmarks`, `place_pins`, `ai_assistant`, `debug` |
| Headless | `shot_harness`, `shot_presets` |
| Platform | `platform/` — `http`, `storage`, `location`, `work`, `workers` (native vs wasm behind one API) |

**System order is load-bearing**, not cosmetic — e.g. `place_pins` must run after
every panel system to cull itself against the leftover map area. Add new systems to
the phase they belong to, not the end.

## Conventions

- **Commits** — Conventional Commits (`type(scope): subject`). A body is required
  for `feat` / `fix` / `perf` and any 5+-file commit; it is where measurements,
  rejected options and silent-failure findings are kept. See
  [`.claude/skills/commit`](../../.claude/skills/commit/SKILL.md).
- **Standing facts go in [`docs/notes/`](../notes/)**, one file per topic, edited in
  place. Rule of thumb: explains one diff → commit body; constrains future work →
  note. `docs/adr/` and `docs/abstract/` are a **frozen archive** — read them for
  history, don't add to them.
- **UI colour, spacing and type come from `systems::theme`.** A fresh
  `Color32::from_rgb` in a panel is how the chrome drifted last time.
- **Dependency versions live in the root `Cargo.toml`** `[workspace.dependencies]`.
- **Protocol changes append a `PROTOCOL_HISTORY` row**; `PROTOCOL_VERSION` is
  derived from it — never write the number anywhere else.

## Configuration

Client env vars (also read from `.env`):

| Variable | Effect |
|----------|--------|
| `VIBE_TILE_URL` / `VIBE_TILE_FORMAT` | Basemap template; `vector\|raster` override for the source kind. Routing needs a vector source |
| `VIBE_OVERPASS_URL` / `VIBE_NOMINATIM_URL` | Point at your own geo services ([server guide](server.md)) |
| `VIBE_CACHE_DIR` | Tile/asset cache root (defaults to the workspace root) |
| `VIBE_USER` / `VIBE_PASSWORD` | Handshake credentials; `--login` and a login file take precedence |
| `VIBERFOX_PLACE` | Start at `lat,lng,alt,pitch,yaw` — the value the HUD's ⧉ button copies |
| `VIBERFOX_NO_ATMOSPHERE\|_HAZE\|_ENVMAP\|_OCEAN\|_CLOUDS\|_RAIN\|_SHADOWS` | Bisect a render problem by removing one layer |
| `ANTHROPIC_KEY` | AI assistant panel — copy `.env.example` to `.env` |

The server takes `vibe.toml` → `VIBE_*` env → CLI flags, in that order.

## Web build

`crates/viberfox/serve-threads.sh` builds and serves the wasm client with real
threads (shared memory + `-Z build-std` + COOP/COEP headers — the script's header
comment explains what breaks if one of the three is missing). Performance
background: [`docs/notes/wasm-performance.md`](../notes/wasm-performance.md).

## CI

[`.forgejo/workflows/ci.yml`](../../.forgejo/workflows/ci.yml) runs on every push:
`cargo check --workspace --all-targets` in a fast lane, plus the client and
`big_space` test suites. Both jobs use the prebuilt `viberfox-ci` image and check
out with `lfs: true` — add that to any new job.

That image carries Mesa **lavapipe** (a CPU Vulkan driver) and `WGPU_BACKEND=vulkan`,
so `--shot` / `cargo shots` work in a job with no GPU and no `/dev/dri`. What a
software capture can and cannot be trusted to tell you — and why `--wait` means
longer than it says there — is
[`docs/notes/headless-shots-software-renderer.md`](../notes/headless-shots-software-renderer.md).

## When something is weird

`CLAUDE.md`'s [**Known issues / workarounds**](../../CLAUDE.md#known-issues--workarounds)
is the accumulated list of traps in this codebase (egui layering, atmosphere in
space, GPU device loss under mesh churn, avatar heights, routing graph noding). It
is worth skimming once end to end before your first non-trivial change — most of
the entries were expensive to find and cheap to read.

Then: [`docs/notes/`](../notes/) for standing constraints and measurements, and
[`docs/manual.md`](../manual.md) for what the app does from a user's seat.
