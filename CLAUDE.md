# CLAUDE.md

Guidance for agents working in this repository.

**Most of what this codebase knows lives in `docs/notes/`, not here** — one topic
per file: measurements with their date and method, rejected options with why they
lost, traps that fail silently. Each note's `triggers:` front-matter names the
files it is about, and a hook (`.claude/hooks/note-triggers`) lists the matching
notes after you read or edit one of those files. **Read the ones it names before
changing that code.** For a topic you have no file for yet, search the triggers:
`grep -l 'triggers:.*routing' docs/notes/*.md`. This file holds only what applies
before you have opened anything: how to build, how the pieces fit, and the rules
that hold everywhere.

## Build and test

Scope builds to the crate you changed — avoid `cargo build` without `-p` unless you
need an integration check.

**Builds run on a midas build node in the agent container**, where `cargo` refuses
to build locally; the midas session hook says which node you hold and how to take
one (`midas take`, then `ssh <node> 'cargo …'`). On a workstation, build locally.
**Prefer the workspace-scoped commands on a node.** A node is warmed with
`cargo check --workspace --exclude cartopolis_android --all-targets` and
`cargo test --workspace --exclude cartopolis_android --no-run`, so those are already
built; `-p` skips only packages the node has. It matters most for
`cargo check -p cartopolis`, which resolves 19 units' worth of different features
from the warmed `--all-targets` check and so builds its own artifact set
(`docs/notes/test-suite-build-cost.md`).

```bash
cargo test --locked --workspace --exclude cartopolis_android   # what CI runs, what a node is warm for

cargo check -p cartopolis_core          # shared protocol types
cargo check -p cartopolis_simulator     # server only, no Bevy — fast
cargo check -p cartopolis               # the Bevy client
cargo run -p cartopolis                 # one dev profile, deps at opt-level 3: already playable
cargo run -p cartopolis --features fast-dev   # Bevy dynamic linking; never for --release or a distributable
cargo run -p cartopolis_simulator       # the headless sim (`--create-admin dev` + CARTO_ADMIN_PASSWORD once)
cargo build -p cartopolis --release

# Android: the crate is *empty* off Android, so `cargo check --workspace` says
# nothing about it. The check needs only the NDK's clang; the APK an SDK and a JDK.
cargo check -p cartopolis_android --target aarch64-linux-android
crates/cartopolis_android/build-apk.sh  # → $CARGO_TARGET_DIR/android/cartopolis-arm64-v8a.apk
```

**`cargo check` does not compile `cfg(test)` code.** The suite silently rotted
through the egui 0.35 migration until someone ran it, so run the tests, not just a
check, when touching egui or the building geometry. Unit tests live in
`#[cfg(test)]` modules; `cartopolis` is a lib plus a shim bin, both shim bins carry
`test = false` and the owned libs `doctest = false`, so `cargo test` links a harness
only for targets with tests in them.

**Integration tests go in `tests/integration/`, as a module of its `main.rs` — not a
new `tests/*.rs`.** Every direct child of `tests/` is a separate binary, linked whole
and run one after another: the simulator's ten files were 364.9 MB of links and a
16.0 s run for 58 tests, and as one target 38.4 MB and 4.4 s. That holds only while
no suite touches process-global state (each binds `127.0.0.1:0`, uses its own
`TempDir`, installs no global logger); a suite that needs isolation gets its own
target *and* a comment saying why.

**Recorded tiles live in `crates/geo/src/testdata.rs`, embedded once.** Use
`testdata::groningen()`, never a fresh `include_bytes!` — the macro does not
deduplicate, and eleven modules naming one 495 KB fixture put twelve copies in the
test binary. It is a `static`, not a `const`, for the same reason.

**Git LFS is a build prerequisite, and its failure is silent.** `.gitattributes`
tracks binary assets by extension, and four are `include_bytes!`-ed at compile time
(the brand font, the app icon, `assets/sky/bright_stars.bin`, the tile fixtures).
Without `git lfs install`, `cargo build` embeds 130-byte pointer files: the font
panics in epaint and the fixture tests assert against garbage. Every
`actions/checkout` in `.forgejo/workflows/` sets `lfs: true` — add it to any new job.

**Never export `CARGO_TARGET_DIR` on the workstation.** sccache hashes that variable
into its cache key, so setting it silently zeroes build sharing between worktrees
(`docs/notes/sccache-parallel-builds.md`). The container is the exception: it bakes
one constant value. The linker is `clang + lld` on Linux (`.cargo/config.toml`).

Wayland + GLES flickers: run with `WGPU_BACKEND=vulkan`. Copy `data/world.db` before
a schema upgrade; `refinery` migrations run on simulator startup.

**Deploying is `midas deploy sim|atlas|web|wasm|apk`**: it builds the tip of `main`
on a node and publishes it, once `verify-branch`'s pass is on record at the hub
(`docs/notes/hosted-simulator.md`). `finish-branch`'s `merge-to-main --deploy`
does it after a landing; the Forgejo deploy workflows are a manual fallback, and
`ci.yml` runs on `main` nightly.

## Seeing it run

When a change has to be looked at rather than tested, use the **`shots` skill**:
headless `--shot` captures, sweeps and their metrics, `--expect` gates, `--flicker`
for z-fights, `--aerial-reference`, the `cargo shots` README set, and the client's
MCP server for driving a running client. The one rule to know before reaching for
it: **a node without a GPU renders on the CPU, which is evidence for geometry,
placement and visibility and none at all for colour, lighting or frame time.**

## Architecture

A Cargo workspace of seventeen crates. Each row's note has the history and numbers.

| Crate | Role |
|---|---|
| `cartopolis_core` | Shared protocol types, `NetMessage`, tile/coordinate math, `motion` (the avatar's rates, shared with the sim) |
| `cartopolis_geo` (`crates/geo`) | Bevy-free map geodata: MVT decode/raster, roads, routing graph, terrain grading, building designs, and `formats` — the per-cell files atlas writes and the client reads. **No `bevy`**: a `Component` goes in the client, the pure part it wraps here (`client-build-cost.md`) |
| `cartopolis_vio` (`crates/vio`) | Visual-inertial odometry for the high-precision mode, developed on its `sim.rs` bench. **No `bevy`** (`camera-tracking.md`) |
| `cartopolis_gnss` (`crates/gnss`) | An external RTK receiver over a byte port: NMEA, RTCM 3, NTRIP. **No `bevy`** (`rtk-receiver.md`) |
| `cartopolis_e2ee` (`crates/e2ee`) | MLS device keys, groups and sealed messages, no I/O. **The simulator must never link it outside its tests** (`encrypted-chat.md`) |
| `cartopolis_chat` (`crates/chat`) | The private-chat `Engine` the client and the web UI share. **No `bevy`**; the simulator does not link it (`web-chat.md`) |
| `cartopolis_web_chat` (`crates/web_chat`) | That engine as wasm for the web UI's chat worker (`web-chat.md`) |
| `cartopolis_calls` (`crates/calls`) | E2EE calls: feature `client` for the app, `sfu` (a forwarder of bytes it cannot open) for the simulator — **which takes only `sfu` outside its tests** (`calls.md`) |
| `cartopolis_simulator` (`crates/simulator`) | Headless Tokio server: SQLite authority, tick loop, the wire, the HTTP API. **No `bevy`** |
| `cartopolis_atlas` (`crates/atlas`) | Map-data pipelines, terrain tiles, and the web UI in `crates/atlas/web` (`atlas.md`) |
| `cartopolis_viewer` (`crates/viewer`, `carto-view`) | Draws what `cartopolis_geo` builds, in seconds, with stock materials; must not depend on `cartopolis`, and is a second Bevy binary in every workspace build |
| `cartopolis` | The Bevy 0.20 client: rendering, ECS systems, egui 0.36 UI |
| `cartopolis_android` | The APK's `cdylib`; both dependencies target-gated, so empty off Android (`android-build.md`) |
| `cartopolis_mcp` (`crates/mcp_server`) | The client's MCP server, Unix desktops only. **No `bevy` or `cartopolis`** (`driving-the-client-from-an-ide.md`) |
| `cartopolis_materials` (`crates/materials`) | The custom material types and `MaterialsPlugin` only, split for build cost; never depends on `cartopolis` |
| `big_space` | Vendored fork of `big_space` 0.10 (floating origin); upstream stops at Bevy 0.18, so re-sync `bevy_compat.rs` on every Bevy bump (`vendored-big-space.md`) |
| `web-task` (`crates/web_task`) | Vendored `web-task` 1.1.3, the browser executor under `bevy_tasks`, patched for a freed-callback bug; drop it when upstream fixes it (`web-task-stale-waiters.md`) |

**The client** (`crates/cartopolis`). `components.rs` and `resources.rs` hold the ECS
types. `systems/` is one file per concern in group folders (`geo` `map` `nav`
`player` `gui` `net` `dev`), each group registered by its `plugin.rs`; `lib.rs`
assembles the app, with the CLI (`cli.rs`), render-error policy (`render_halt.rs`),
crash reports (`crash.rs`) and the `CARTO_*` switches (`debug_switches.rs`) beside
it. **The folders are not the import path**: `systems/mod.rs` re-exports every
module flat, so it is `systems::water`, never `systems::map::water` — adding a system
is a `pub mod` in the group's `mod.rs` plus a name in its `pub use` list. **Every
`Update` system sits in one `ClientPhase` set** (`phase.rs`: `Input → Anchor → Stream
→ Move → Camera → Present`), so `.after()` is only needed within a phase. The group
is `gui`, not `ui`, because `systems::ui` is one of the panels.

**The server** (`crates/simulator`): `config.rs` layers `cartopolis.toml` → `CARTO_*`
→ flags; `net/request.rs`'s `ClientRequest` is the one place a `NetMessage` is
classified as client→server, with a handler file per topic; `prims.rs`'s
`PrimService` serves both the wire and `/api/admin/prims`, so their checks cannot
disagree; `state.rs`'s `SimWorld` is the authority and holds the world in memory.

**Run modes: exactly two** (`session-modes.md`). Online is server-authoritative —
the client sends `ClientIntent` and applies `WorldSnapshot`s; offline (`--offline`,
and every `--shot`) runs no sim and has no avatar. The mode switches at runtime
(`session::drive_session`), `connect`/`teardown` are an exact pair, and the avatar
belongs to the session. There is no client-side database.

**The wire** (`wire-protocol-versions.md`, `hosted-simulator.md`). Length-delimited
frames (`u32` LE) whose body is an 8-byte header — protocol version, message kind,
request id — and a postcard payload. **Bump the version by appending a row to
`PROTOCOL_HISTORY`; never write the number anywhere else.** TCP and WebSocket carry
the same body through one `handle_connection` over `FrameTransport`, so a new
carrier is not a protocol revision. A bare hostname means `wss://host/ws`; only
loopback stays plain TCP. TLS is rustls, never native-tls (the Android lane has
only the NDK's clang), and `install_crypto_provider` must run or rustls panics at
the first connection.

**Operator actions are HTTP (`http.rs`), never `NetMessage`**: an authorisation
decision must not share a path with a movement intent, and the admin surface must
not move every client's protocol version. Disabling an account gates the next
handshake, not the current session; the last enabled admin cannot be demoted,
disabled or deleted. **Player safety actions are on the wire and every refusal is
enforced by the server** — a block applied only in the UI is not a block
(`user-safety.md`).

**cartopolis.org is the atlas web UI** (`crates/atlas/web`, Mithril on Bun; the
Community | World | Admin page and the players' pages — `atlas-web-ui.md`). Build
its controls from the fork's component library (`@garage44/mithril/components`),
and `bun run mock` there shows every page with no backend. **`src/types.ts`
hand-mirrors `auth::AccountRow`**: change an account field and change it there too
(the simulator's `web_mirror` test fails until you do).

## Rules that hold everywhere

Each of these bit someone, usually silently. The note behind each has the story.

**Bevy and the ECS**
- A command aimed at an entity another system may despawn uses `try_insert` / `try_remove` / `try_despawn`; a release build only warns, a debug build panics — `entity-commands-outlive-entities.md`
- A system over Bevy's 16-parameter cap fails to compile as "not a system set"; bundle parameters into a `SystemParam` — `selecting-a-building.md`
- Blocking CPU work goes through `platform::workers`, never `AsyncComputeTaskPool`, which Bevy's shader compiles and screenshot readbacks share — `headless-shots-software-renderer.md`
- Moving the avatar or camera an arbitrary distance goes through `Teleport`, never a transform write — a move is a write, a jump is a teleport — `device-location.md`
- A mode that borrows persisted state (the camera, the sky) exposes the pre-mode value to the autosave, because a killed process never runs the restore — `navigation-display.md`
- Avatar movement rates exist once, in `cartopolis_core::motion`, shared by the sim, prediction and the offline path; never copy one locally — `avatar-game-feel.md`
- A one-shot replicated event (jump, emote, burst) is an edge that skips every intent throttle, held as a level with a `sequence` that receivers diff on — `avatar-game-feel.md`

**Rendering and the GPU**
- Write a material through `get`, compare, then `get_mut` only what differs: `get_mut` re-prepares it on the render thread even when the value is unchanged — `render-performance-survey.md`
- A ring streamer centres on `AnchoredCamera::stream_xz_at` (`grid::StreamCentre`), never the camera: in Earth mode a rotate-drag swings the camera round a 2 km circle — `render-performance-survey.md`
- Bound every streaming path per frame **in bytes** (`stream_ledger::admit`), not only in items, and gate it on drift or altitude: mass mesh churn loses the GPU device — `upload-bursts.md`
- `NoFrustumCulling` also enters an entity into every shadow cascade; add `NotShadowCaster` unless it really casts — `render-performance-survey.md`
- Never destroy a texture or buffer in the frame that recorded work on it; defer the free past `Queue::submit` — `egui-texture-lifetime.md`
- Altitude-driven visuals crossfade on `GlobeState::space_factor`, never switch on `in_space`, or a climb steps mid-zoom — `the-atmosphere-from-the-street-to-orbit.md`
- Key any world-fixed shader pattern to anchor metres (UV0, `MeshTag`), never `world_position`: `big_space` jumps the render origin a whole cell — `water-surface.md`
- Shaders are WESL (`import a::b;`, `@if`, bevy modules by file path); a shader error shows only in a run's log, never in `cargo check` or `cargo test` — `shaders-are-wesl.md`
- Anything drawn flat on the ground takes its depth bias and height from a `GroundLayer` variant, never a hand-picked number — `the-ground-stack.md`
- Anything standing on the ground adds `TerrainField::at` and rebuilds on the field's `revision`, or it floats or sinks when the field lands — `terrain-relief.md`
- Compare a prim with the avatar through its geo anchor, never its `Transform`: that is `big_space` cell-local, the avatar is anchored world XZ — `prim-scripting.md`
- Find a ring's outward side by probing 5 cm off the edge, never from its signed area: +z runs south, so winding rules come out inside-out — `doors-and-rooms.md`
- Anything reading `Query<&Window>` falls back to `resources::HeadlessSurface`; a scissor that does not match the target quits a headless run silently — `headless-shots-software-renderer.md`

**egui and input**
- Systems reach egui through `EguiRef` / `EguiMut`, never `Res<EguiManager>` — it is non-send on wasm, so the mistake breaks only the web check — `egui-shared-root-ui.md`
- Every system drawing into the shared root `Ui` goes in the panel-chain `.chain()` tuple in `systems/gui/plugin.rs`, or panels steal each other's clicks — `egui-shared-root-ui.md`
- Floating `Area`s paint over panels: a world-anchored overlay culls (not clips) itself to `place_pins::map_area` and runs at the chain's tail — `egui-shared-root-ui.md`
- UI colour, spacing, type, overlay frames, panel headers and close buttons come from `systems::theme`; never a fresh `Color32` in a panel — `ui-vocabulary.md`
- Gate a world click on `EguiManager::owns_click`; `egui_wants_pointer_input()` is only right while a button is down — `touch-input.md`
- Game keys yield only to `EguiManager::ui_owns_keyboard` (a focused text field), never `egui_wants_keyboard_input()` — `egui-shared-root-ui.md`
- Touch emits no mouse events: world clicks get an emulated mouse, gestures read `TouchGestures`, egui reads the event stream — `touch-input.md`
- A clickable widget with a context menu guards its click with `!response.long_touched()`, because a long press also fires `clicked()` — `touch-input.md`
- An anchored, size-to-content `Area` pins its content width, or it grows every frame until it spans the screen — `map-chrome.md`

**Platforms**
- All persistence goes through `platform::storage`, never `std::fs`; on the web a read can be `Loading`, and treating that as `Missing` never recovers — `web-persistence.md`
- Android answers "native" to every `cfg(not(target_arch = "wasm32"))`: gate desktop-only crates with `not(target_os = "android")` too, and give per-OS `cfg` chains a catch-all arm — `android-build.md`
- A build lane that passes `--no-default-features` must name `audio` back, or that platform ships silent — `audio-off-platform.md`

**The server and the world**
- Change prims through `SimWorld` (or `/api/admin/prims`), never by writing `world.db`: the world holds them in memory and a row written behind it is never seen — `authoring-content-from-outside-the-viewer.md`
- Prim edits pass `may_edit` / `may_script` (`prims.rs`), and a refused wire edit is followed by the true prim (`refuse_and_correct`), or the client draws what never happened — `authoring-content-from-outside-the-viewer.md`

## Documentation

- `docs/notes/` — **standing facts** the code and `git log` don't hold, one topic per
  file, edited in place when a fact stops being true; `docs/notes/README.md` has the
  format. Give a new note `triggers:` that name its files (`routing.rs`,
  `crates/atlas`, `systems::water`) — those are what the hook matches.
- `docs/guides/development.md` — the contributor guide: dev loop, tests, code map, config, CI.
- `docs/mockups/` — screen designs drawn before a feature is built, one folder per topic.
- `docs/adr/`, `docs/abstract/` — the **frozen** ADR archive, retired 2026-07-26
  (`docs/notes/retired/adr-system.md`). Read for history; never add to it.

**Where a fact goes:** explains this diff → the commit body (required for
`feat`/`fix`/`perf` and any 5+-file commit — the `commit` skill); constrains all
future work → a note; would be falsified by a code change → a comment next to the
code. **Not here** — a fact added to this file is loaded into every session forever,
which is how it reached 163 KB. A new rule belongs in this file only if it holds
everywhere and no file trigger could deliver it.
