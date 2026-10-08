# Development

Onboarding for new contributors. The [README](../../README.md) says what Cartopolis
is and how to start it; this is what comes after it.

## Before anything else: Git LFS

`git lfs install` **before** cloning. Binary assets — models, textures, fonts,
icons, audio, the `docs/img` screenshots — are in LFS by default (`.gitattributes`
matches by extension, so a new one needs no setup), and four of them (brand font,
app icon, star catalog, tile fixtures) are `include_bytes!`-ed at compile time —
without the smudge filter `cargo build` embeds 130-byte pointer files and
succeeds, then panics at runtime (the icon font `systems::icons` bundles is a
fifth). Same reason every CI job sets `lfs: true`. See
[`assets/README.md`](../../assets/README.md).

## Prerequisites

- **Rust** — install via [rustup.rs](https://rustup.rs/).
- **A C toolchain** (for the bundled SQLite):
  - **Linux** — `clang` + `lld`; the workspace links with them (see
    `.cargo/config.toml`). On Arch: `pacman -S clang lld`. For faster links,
    switch to `mold` (`fuse-ld=mold`).
  - **macOS** — Xcode Command Line Tools (`xcode-select --install`). No linker
    config needed; the default toolchain is used.
  - **Windows** — the MSVC build tools.
- **A GPU with working drivers** — rendering is through `wgpu` (Vulkan on Linux,
  Metal on macOS, DX12/Vulkan on Windows). Wayland and X11 are both supported.

macOS and Windows use no platform-specific code and should build with a stock
Rust toolchain; Linux is the primary development target.

## The loop

| Task | Command |
|------|---------|
| Type-check what you touched | `cargo check -p cartopolis_core \| simulator \| cartopolis` |
| Play it (already playable — `dev` builds deps at opt-level 3) | `cargo run -p cartopolis` |
| Fast incremental links (local only) | `cargo run -p cartopolis --features fast-dev` |
| Client tests | `cargo test -p cartopolis` |
| Geodata tests (no Bevy — ~2 s) | `cargo test -p cartopolis_geo` |
| Server tests | `cargo test -p cartopolis_simulator` |
| Floating-origin tests | `cargo test -p big_space --lib` |
| Regenerate README images | `cargo shots` |

One trap: `cargo check` does **not** compile `cfg(test)` code, so a green check
says nothing about the suite. Run the tests when touching egui, geometry or tile
parsing.

`#[ignore]`-d tests are diagnostics, not tests — they dump a PNG, print a tile's
layers, time a cell, or solve a real route against the network. Run one by name:
`cargo test -p cartopolis dump_png -- --ignored --nocapture`.

Streaming performance has a measuring kit of its own, in three layers
(`docs/notes/tile-raster-cost.md` and `docs/notes/allocation-budgets.md` carry
the current numbers and the doctrine):

```bash
# exact, machine-independent: allocation counts, retained heap, rasteriser input
cargo test -p cartopolis_geo print_tile_pipeline_cost -- --ignored --nocapture
# wall clock per pipeline stage — --release, or the numbers are opt-level 1's
cargo test -p cartopolis_geo --release print_tile_pipeline_time -- --ignored --nocapture
# end to end: any --shot reports per-capture fetch_* (count/MB/TTFB/body) and
# worker_* (jobs/busy/wait, split per stream) columns in the log, --csv and
# --dump-state — cold-vs-warm-cache pairs over one viewpoint are the experiment
```

Verify a visual change **headless** rather than by flying the camera there:
`cargo shots --only globe` for a README preset, or `--shot` for an arbitrary
viewpoint, sweep or CSV of the ramp scalars. Full flag list in
[`CLAUDE.md`](../../CLAUDE.md#scripted-screenshots---shot).

## Putting things in the world

Prims, and the Luau scripts on them, can be authored from a shell — no viewer, no
GPU, no session. It is the same set of operations the create dialog and the
script editor drive, over `/api/admin/prims` with an admin token
(`docs/notes/authoring-content-from-outside-the-viewer.md`).

```bash
cargo run -p cartopolis_simulator -- --create-admin dev   # once; CARTO_ADMIN_PASSWORD
cargo run -p cartopolis_simulator -- --http-listen 127.0.0.1:8080

export CARTO_SIM=http://127.0.0.1:8080
export CARTO_ADMIN_USER=dev CARTO_ADMIN_PASSWORD=...

tools/author.py capacity                # the world's prim/script budget and usage
tools/author.py content                 # what the server can install
tools/author.py install firebird        # the worked example, over Groningen
tools/author.py create --lat 53.2194 --lng 6.5665 --alt 20 \
    --shape sphere --particles '{"color_start": [0.2, 0.6, 4.0]}'
tools/author.py script 3 mine.lua
tools/author.py --print patch 3 --color 1,0.4,0.1   # show the request, send nothing

tools/author.py export --out my-build.json         # everything you own
tools/author.py import my-build.json               # bring it back
```

**Export and import are owner-scoped, not admin-scoped.** They sign in as an
ordinary account (`CARTO_USER` / `CARTO_PASSWORD`) and carry only prims that
account built — an admin token gets no reach into somebody else's work. Import
*adds* rather than replaces, because a document cannot say which live prim each
entry was.

The **content the server installs is the server's**, not the tool's:
`crates/simulator/src/content.rs` defines each pack and embeds its Luau from
`crates/simulator/content/` at compile time, because a deployed simulator has no
repository beside it. Installing a pack twice replaces what it installed.

The `firebird` pack is the worked example for the script API and is two prims: a
burning thing that flies a circle with `set_geo`, watches for company with
`sensor` and goes off with `particle_burst`, and a beacon on the Grote Markt
that calls it in and sends it back out over `send` / `on_message` — which is
the only way one prim can reach another. Walk up to the beacon and press E.

The in-editor reference is generated from `cartopolis_core::SCRIPT_API`, which is
the catalogue the server is tested against — so it cannot drift from what a
script can actually call.

A 200 from the script route means *stored*, not *compiled*: the host compiles on
its own thread and reports through `ScriptOutput` on the wire, so the script
console in a connected viewer is still where the answer appears.

## Where things live

| Path | Contents |
|------|----------|
| `crates/core/` | Wire protocol (`protocol.rs`), tile/coordinate math (`world.rs`, `geo.rs`) — the `cartopolis_core` crate |
| `crates/geo/` | Bevy-free map geodata (`cartopolis_geo`): `vector_tiles`, `road_texture`, `routing`, `scatter`. Must never depend on `bevy` |
| `crates/simulator/` | Authoritative server: `net.rs` (tick loop), `state.rs` (world), `db.rs`, `auth.rs`, `config.rs`, `http.rs` + `authoring.rs` (the admin/authoring API) |
| `crates/cartopolis/` | Bevy client — everything below |
| `crates/big_space/` | Vendored floating-origin fork; re-sync `bevy_compat.rs` on every Bevy bump |
| `crates/simulator/content/` | Luau for the content packs the server carries, embedded into the binary by `content.rs` |

Inside the client, `components.rs` / `resources.rs` hold the ECS types and
`systems/` is one file per concern, grouped into folders and registered in ordered
phases in `lib.rs`:

| Folder | Files |
|--------|-------|
| `systems/map/` | `map_stream`, `map_geometry`, `tile_loader`, `tile_source`, `rendering`, `globe`, `globe_ocean`, `globe_weather`, `osm_buildings`, `lod22`, `terrain`, `height`, `water`, `vegetation`, `street_furniture`, `street_labels`, `weather`, `particles` |
| `systems/nav/` | `navigation`, `route_ahead`, `driver`, `geo_nav`, `transit_live`, `transit_vehicles` |
| `systems/player/` | `avatar`, `collision`, `free_camera`, `picking`, `gizmo`, `prim_geo` |
| `systems/gui/` | `egui_manager`, `theme`, `icons`, `ui`, `hud`, `dock`, `bookmarks`, `place_pins`, `chat`, `ai_assistant`, `splash`, `data_layers` |
| `systems/net/` | `network`, `user_store` (plus `../login.rs`) |
| `systems/dev/` | `shot_harness`, `shot_presets`, `debug`, `desktop_integration`, `window_icon` |
| `platform/` | `http`, `storage`, `location`, `stream`, `work`, `workers` (native vs wasm behind one API) |

**The folders group files; they are not the import path.** `systems/mod.rs`
re-exports every module flat, so a system is named for what it is
(`systems::water`) and not for where it is filed. Adding one means a `pub mod` in
the group's `mod.rs` and a name in the `pub use` list — both alphabetical. The
group is `gui`, not `ui`, because `systems::ui` is one of the panels.

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
- **Run `cargo fmt` before committing.** The four owned crates are rustfmt-clean
  as of `d5c301d` and CI checks it. `crates/big_space` is excluded — it is a
  vendored fork kept diff-comparable with upstream. If a bulk reformat is ever
  needed again, add its SHA to `.git-blame-ignore-revs`.
- **Dependency versions live in the root `Cargo.toml`** `[workspace.dependencies]`.
- **Protocol changes append a `PROTOCOL_HISTORY` row**; `PROTOCOL_VERSION` is
  derived from it — never write the number anywhere else.

## Configuration

Client env vars (also read from `.env`):

| Variable | Effect |
|----------|--------|
| `CARTO_TILE_URL` / `CARTO_TILE_FORMAT` | Basemap template; `vector\|raster` override for the source kind. Routing needs a vector source |
| `CARTO_OVERPASS_URL` / `CARTO_NOMINATIM_URL` | Point at your own geo services ([server guide](server.md)) |
| `CARTO_CACHE_DIR` | Tile/asset cache root (defaults to the workspace root) |
| `CARTO_USER` / `CARTO_PASSWORD` | Handshake credentials; `--login` and a login file take precedence |
| `CARTO_PLACE` | Start at `lat,lng,alt,pitch,yaw`, or at a whole `https://cartopolis.org/go?place=…` link — what **🔍 Search ▸ Copy link** writes ([place links](../notes/place-links.md)) |
| `CARTO_NO_ATMOSPHERE\|_HAZE\|_ENVMAP\|_OCEAN\|_CLOUDS\|_RAIN\|_SHADOWS\|_SPACE` | Bisect a render problem by removing one layer |
| `CARTO_MCP_TOKEN` | The bearer an editor presents to `--mcp-listen`. Overrides the one in `user.json`, which is what Settings ▸ General shows |

The server takes `cartopolis.toml` → `CARTO_*` env → CLI flags, in that order.

## Shipping: three builds from one codebase, plus two services

**Desktop (native).** `cargo build -p cartopolis --release` → `target/release/cartopolis`
(fat LTO: slow to compile, fastest at runtime). `release.yml` builds the Linux
x86_64 tarball and cross-compiles the Windows x86_64 zip in one job, publishing
both to a Forgejo release — a versioned one on a `v*` tag, or the rolling
`continuous` prerelease on demand.

**Web (WebAssembly).** `crates/cartopolis/serve-threads.sh` builds and serves the wasm
client with real threads (shared memory + `-Z build-std` + COOP/COEP headers — the
script's header comment explains what breaks if one of the three is missing).
`deploy-wasm.yml` does the same with LTO and publishes to
[cartopolis.org](https://cartopolis.org) on every push to `main`; it is a
separate workflow because rebuilding std and linking ~70 MB would block the test
lanes. The web target runs against WebGL2 (no storage buffers — hence the CPU
fallbacks the `dev` profile's opt-level-3 dependencies exist to make bearable) and
proxies genuinely third-party upstreams under its own origin (`Trunk.toml`).
Performance background: [`docs/notes/wasm-performance.md`](../notes/wasm-performance.md).

**Android.** `crates/cartopolis_android/build-apk.sh` → a signed debug APK. There is
no Gradle project and no Java source: the app is `AndroidManifest.xml`, a `cdylib`
holding `android_main`, and `assets/`, assembled with `cargo ndk` → `aapt2` →
`zipalign` → `apksigner`. It needs an NDK, an SDK (build-tools + one platform) and
a JDK; the script's header lists the environment variables. Nothing is published
and nothing has run on a device yet — read
[`docs/notes/android-build.md`](../notes/android-build.md) before touching it,
particularly the part about NativeActivity costing the soft keyboard.

**The hosted simulator.** `deploy-sim.yml` builds `cartopolis_simulator` in
release and publishes the binary to the VPS, where a systemd unit runs it. It
serves the wire and the account admin API from one process and one account store,
both behind Caddy's TLS: `--connect cartopolis.org` from any build — desktop,
Android or browser — resolves to `wss://cartopolis.org/ws`. **Give it a hostname,
not a host:port**; `network::resolve_target` supplies the scheme and the path, and
that is what keeps credentials off the wire in the clear. Its TCP listener is
loopback-only, so there is nothing plaintext to reach.
[`docs/notes/hosted-simulator.md`](../notes/hosted-simulator.md) has the shape and
what is still open.

**cartopolis.org is the atlas web UI, `crates/atlas/web`.** Mithril
(`@garage44/mithril`, our fork of Mithril.js with signals), Bun, no Rust — it manages accounts on the hosted simulator and drives
atlas's map data pipelines, and `deploy-web.yml` publishes it to
[cartopolis.org](https://cartopolis.org/) (operator pages at `/admin`) on every push to
`main` that touches it. It was a separate repository, on its own
hostname (`web.cartopolis.org`, retired), until 2026-09. It talks
to the rest of the tree only through the simulator's `/api/admin` and atlas's
`/api/pipeline/*` responses; its README has the dev loop, and its `bun run dev`
proxies to a local simulator so it runs against the real API rather than a mock.

Platform differences live behind one API in `platform/` — native and wasm
implementations of `http`, `storage`, `location`, `stream`, `work`, `workers`.
**Android answers "native" to every one of those `cfg`s**, which is where a port
finds its bugs: `arboard` has no Android backend, stderr on a phone goes nowhere,
and a NativeActivity process has neither a usable `argv` nor a writable working
directory. The note has the list.

### Running the web build locally

Two rungs. Take the first unless you actually need a browser.

**1. Does it still compile for wasm?** No trunk, no `-Z build-std`, no linking —
seconds once warm, and it is what CI's `cargo check` job runs:

```bash
rustup target add wasm32-unknown-unknown --toolchain nightly     # once
RUSTUP_TOOLCHAIN=nightly \
  cargo check -p cartopolis --no-default-features --features audio \
    --target wasm32-unknown-unknown
```

**Nightly is required even for the check**, and this is not the `-Z build-std` the
full build needs — it is `wasm_thread`, which opens `feature(stdarch_wasm_atomic_wait)`
in a `cfg_attr(target_arch = "wasm32")`. On stable that is a hard `E0554` from a
dependency, before any of our code is reached, and no flag turns it off.

The two feature flags mirror `index.html` exactly, which is the point — the
check and the real build must not drift. (They date from the since-removed
`solo` default; dropping the defaults without naming `audio` back is how the
browser client spent months with the soundscape compiled out —
`docs/notes/audio-off-platform.md`.) This rung catches essentially everything that rots, because what
rots is `cfg(target_arch = "wasm32")` code that no other lane ever compiles.

**2. Run it in a browser:**

```bash
crates/cartopolis/serve-threads.sh                  # → http://127.0.0.1:8195
crates/cartopolis/serve-threads.sh build            # build only, no server
PROFILE=web-release crates/cartopolis/serve-threads.sh build   # what CI ships
```

The script sets everything up itself — nightly, `-Z build-std`, the shared-memory
link flags, `--cfg=web_sys_unstable_apis`, and `PATH` — so run it rather than
assembling the flags by hand; its header comment says what breaks when one goes
missing. Trunk fetches a `wasm-bindgen` matching the lockfile on its own, so only
`trunk` itself has to be installed (`cargo install trunk`, or drop it in
`~/.wasm-toolchain/bin`, which the script also puts on `PATH`).

Expect a **cold first build of many minutes** — it rebuilds `std` and every
dependency at `opt-level = 3` for a target nothing else in the workspace shares.
It is incremental afterwards. Note that *any* `RUSTFLAGS` change invalidates the
whole graph, so treat editing that line as a "go and do something else" event.

**If `cargo` cannot find the wasm target, you have two rusts.** A distro-installed
rust (`/usr/bin/cargo`, e.g. Arch's `rust` package) shadows rustup's shims on
`PATH`, and it carries neither the wasm32 `std` nor `rust-src`. The tell is
`error: can't find crate for 'std' ... the wasm32-unknown-unknown target may not be
installed` *immediately after* `rustup target add` reported success — the two
commands were talking about different toolchains. `RUSTUP_TOOLCHAIN=nightly` and
`cargo +nightly` do **not** fix it; both are read by rustup's shim, which is the
binary being shadowed. Put rustup first instead:

```bash
export PATH="$HOME/.cargo/bin:$PATH"      # what serve-threads.sh already does
rustc --print sysroot                     # must NOT say /usr
```

## Dependencies

`Cargo.lock` is committed and **every cargo invocation in `.forgejo/workflows/`
passes `--locked`**. Taking a newer version of anything — including a patch
release — is therefore a deliberate act:

```bash
cargo update -p <crate>     # one crate
cargo update                # everything, within the semver ranges in Cargo.toml
```

and the resulting `Cargo.lock` is reviewed like any other diff. Nothing updates
dependencies automatically: there is no Renovate or Dependabot configured here.

The lockfile alone was not enough to guarantee this. Without `--locked`, a
`Cargo.toml` edit committed without its lockfile update does not fail CI — cargo
re-resolves to whatever is newest at that moment, builds *that*, and throws the
modified lockfile away, so the artifact does not correspond to the lockfile and
nothing reports it. With `--locked` the same situation is a one-line error naming
the crate.

The one thing still unpinned is the **toolchain**: there is no
`rust-toolchain.toml`, and `cartopolis-ci:latest` installs whatever `stable` was
current when the image was last built. A toolchain bump also invalidates the CI
cache, since rust-cache keys on the rustc version.

## CI

[`.forgejo/workflows/ci.yml`](../../.forgejo/workflows/ci.yml) runs on **pull
requests and pushes to `main`** — not on branch pushes. A branch with a PR open
would otherwise be compiled from the same commit twice, once per ref, and then a
third time as the merge commit; `concurrency` cannot collapse those, because a
branch ref and a PR ref are different strings. Open the PR, which is where a
verdict gets read.

What runs where:

- **`test`** — every PR and every `main` push, and on a PR the only job. The
  suites in cost order (`cartopolis_geo` first: no Bevy in its graph, so a geodata
  regression fails in seconds rather than after the client's full codegen), then
  `cartopolis_core`, `cartopolis_simulator`, `cartopolis`, `big_space --lib`, and
  `cargo fmt --check` over the owned crates last. There is no separate
  `cargo check --workspace --all-targets` — the tests compile every crate that has
  code, this workspace has no benches or examples, and `cargo check`'s
  metadata-only output is not reusable by the tests' codegen, so it was a second
  cold Bevy build for nothing.
- **the headless scene smoke** — a step of `test`, `main` only. An orbit `--shot`
  asserting `settled==true` and `globe_rendered>0`, which catches the failure class
  that type-checks perfectly and ships a black screen. It is the one real *build*
  in the file (`cargo run` links the client), so it is also the one thing a PR does
  not pay for. On failure it dumps the metrics that explain it.
- **`targets`** — `main` only. The two targets a host build cannot see: **wasm32**,
  and **aarch64-linux-android** (whose crate is *empty* off Android, its
  dependencies being target-gated). The Android lane downloads and caches an NDK,
  because a build script's `cc` runs at check time — see the comments in the
  workflow.

Gating the cross-target lanes on `main` is a deliberate weakening, and the reason
it is affordable is that **nothing here gates a merge on CI**: `main` is allowed to
be red, so a PR lane informs whoever merges rather than stopping them. Rot is
caught within one merge instead of at the next deploy. If a change is knowingly
wasm- or Android-shaped, run that lane locally first —
[`container-android-toolchain.md`](../notes/container-android-toolchain.md) has the
two environment variables.

Both jobs use the prebuilt `cartopolis-ci` image and check out with `lfs: true` — add
that to any new job. `deploy-wasm.yml`, `deploy-android.yml` and `deploy-sim.yml`
(all three `workflow_dispatch` only) and `release.yml` are separate workflows.

Every other deploy lane in this repo is manual, because publishing is either
expensive (a ~70 MB wasm link competing for a runner slot) or disruptive
(`deploy-sim.yml` restarts the live simulator, dropping every connected session).
The web UI's lane, `deploy-web.yml`, is the opposite — seconds, and it
interrupts nobody — so it runs on every push to `main` that touches
`crates/atlas/web`. It publishes to `/data/cartopolis/admin`, which has been
in the runner's `valid_volumes` since the web UI was its own repository.

**A step may only use a tool the image already has.** The image is built by hand on
the VPS (`~/build/cartopolis-ci-image/Dockerfile`, then `docker build -t
cartopolis-ci:latest .`) and the runner never pulls it, so a new tool in a workflow is
an image change — nothing in this repo will tell you it is missing. The `cargo fmt`
gate landed against a toolchain installed `--profile minimal`, which omits rustfmt,
and every run since died on `'cargo-fmt' is not installed for the toolchain`. Rebuild
`cartopolis-agent:latest` after the CI image: it is `FROM cartopolis-ci:latest`, so the
autonomous agent otherwise still can't run what CI demands.

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
