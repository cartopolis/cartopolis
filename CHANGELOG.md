# Changelog

Notable changes, newest first. Written from the Conventional Commit log — `git
log` is the record, this is the summary a person can read — and versioned by
[Semantic Versioning](https://semver.org/), with the wire protocol tracked
separately in `PROTOCOL_HISTORY` (`crates/core/src/protocol.rs`) because a client
and a sim can be different versions of this file and must not be different
versions of that one.

Entries are grouped by what changed for a user, not by crate. A line here should
be findable in the log; where the *reason* matters it is in the commit body, and
where it constrains future work it is in `docs/notes/`.

## 1.0.0 — 2026-08-10

The first tagged release. There was one rolling `continuous` prerelease before
this and no versioned build at all, so everything below is "since the beginning"
rather than "since last time" — grouped by area, and deliberately not one line
per commit.

### The world

- A single continuous space from street level to orbit: walk a pavement, keep
  climbing, and the flat streamed map hands over to a full planetary globe with
  no loading screen. Floating-origin coordinates (a vendored `big_space` fork)
  are what make metre-scale detail and planetary distance coexist.
- Buildings raised from OpenStreetMap footprints, with LoD2.2 roof geometry from
  3DBAG where it exists, streamed per cell around the camera.
- Ground surfaces from vector tiles rather than imagery: roads, water, land use
  and bridge decks as lit geometry, with street names written into the road
  surface as world objects rather than screen labels.
- Measured terrain relief from AHN height rasters — one field per map anchor,
  sampled by everything that stands on the ground.
- Trees and street furniture scattered from the tile's own layers, POI markers
  and house numbers read out of the `pois` and `addresses` layers.
- Build in place: create, texture, move and reshape prims with a gizmo and an
  edit dialog, persisted by the simulator.

### Sky, light and weather

- Physically-based atmosphere with a day/night cycle, a sun disk, a moon, and a
  real star field from the HYG catalog.
- Live weather from MET Norway and global cloud cover from NOAA GFS, driving
  clouds, fog, rain and wind — and the sea state under the water shader.
- A space tier that crossfades rather than switches: exposure, bloom, ambient
  light, atmosphere thickness and the globe's own albedo all ride one altitude
  scalar, so a climb has no step in it.

### Moving around

- Server-authoritative avatars with client prediction; Quaternius modular
  characters, mixed per slot (head / top / legs / feet) and phase-locked across
  skeletons.
- A speed model shared with the simulator: stroll, jog, sprint and flight are
  four magnitudes of one wish vector, ramped in m/s², with the animation clip
  chosen by ground speed and played at the speed the ground is moving.
- Jump, six emotes, avatar↔building collision, a follow camera whose boom
  collides, and interaction prompts for the street furniture you walk past.
- Routing over the map's own road graph — car, bike, foot and ferry — with turn
  instructions at real junctions, staged long-haul routes out to ~360 km, and a
  driver mode that strips the map to a navigation display and follows the
  device's own GPS where there is one.
- Saved places as a colour-coded folder tree, with matching map pins.

### Together

- An authoritative headless simulator (Tokio + SQLite) replicating avatars,
  prims and world state, with accounts, sessions and per-region areas of
  interest.
- Proximity chat and proximity voice over the same wire protocol.
- In-world scripting: Luau on prims, budgeted per region with a fuel bank that
  yields rather than kills, edited in-world with its console beside it.
- An AI copilot panel for LLM-assisted world building.
- Run modes switch at runtime, not at launch: solo (an in-process sim on
  loopback), split (`--connect`), or fully offline.

### Where it runs

- **Desktop** — Linux and Windows binaries, macOS from source.
- **Web** — a WebAssembly build with real threads and SIMD, published on every
  push to `main`, gated on WebGPU before it downloads anything.
- **Android** — an installable APK, built with no Gradle and no Java: a
  `cdylib`, a manifest and the assets directory. Published at
  <https://cartopolis.garage44.eu/apk/>.

### Tools

- `cargo shots` regenerates the documentation image set from named camera
  presets, headless, with no window and no GPU required.
- `--shot` / `--dump-state` / `--expect` make a scripted capture an instrument:
  every camera state and render scalar is measurable from a shell, assertable,
  and gated in CI on a frame that actually had a planet in it.

### Fixed in the run-up to this release

- **The Android build was broken and CI was hiding it.** `cargo fmt --check` ran
  first in the check job, so an unformatted merge ended the job before the
  Android lane — the only lane that compiles the APK's `--no-default-features`
  configuration — ever ran. Behind it sat a real compile error, and
  `deploy-android.yml` had failed on every push to `main` for two months. The
  format gate now runs last, on the principle that whatever gates last cannot
  hide anything.
- **A hostile or corrupt vector tile could abort the client.** Tile bytes are
  network-controlled and `panic = "abort"` makes an allocation failure a SIGABRT
  on a user's device; a 3 MB body of repeated geometry commands asked for
  terabytes. Bounded, on the default offline path.
- **A one-line script could hang the simulator forever.** The script's top-level
  chunk ran under `pcall`, which is not yieldable, and mlua silently discards a
  yield it cannot honour — so the load-time fuel limit never fired. With `solo`
  in the shipped Linux build, that was the user's own client.
- **Four leaks.** A dropped stream cell kept its concurrency slot forever; a
  dying connection kept its avatar; and the phone build held six of the largest
  mesh and texture assets in both the main and render worlds, and rasterised
  every flat-map tile at 512² on every tier (~310 MB where ~78 MB does).
- **The phone's render budget was mostly unreachable.** 2× MSAA is optional in
  Vulkan's guaranteed minimums, so on Android it could fail to create the render
  target and draw nothing at all; the worker cap did not apply; present mode
  rendered frames SurfaceFlinger discarded, paid for in heat.
- World state written by the simulator now survives the cases that previously
  lost it.
- Android grew a real location provider, an on-screen keyboard (NativeActivity
  supplies no IME, so every text field on the phone was previously focusable and
  untypeable), a screen that stays lit, and working external links.
- The launcher icon, the cold-start screen and the APK's version are no longer
  placeholders: real density buckets and an adaptive icon, a branded window
  background instead of ten to thirty seconds of black, and a monotonic
  `versionCode` so one build can update another.

### Licensing

- Dual-licensed `MIT OR Apache-2.0` (`LICENSE`, `LICENSE-MIT`,
  `LICENSE-APACHE`).
- `LICENSE-THIRD-PARTY.md` is the canonical notice for everything that is not
  our source — the ODbL, CC BY 4.0, CC0 and OFL obligations that travel with the
  map data, the building models, the height rasters, the avatars and the font.
  It ships in the release tarball, the Windows zip and inside the APK.
