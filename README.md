# Viberfox

A networked 3D virtual world built with Rust and Bevy, anchored in the real planet. Walk a street of buildings imported from [OpenStreetMap](https://www.openstreetmap.org/), then keep flying up — through the atmosphere, past the planet turning below you, into space — and drop back down anywhere on Earth. Build in place, and share the world with others in real time.

- **The real world, streamed** — live map data for the ground; move the anchor and the world re-streams around you.
- **Street to orbit, seamlessly** — one continuous space from the pavement to a full planetary globe, with no loading screens.
- **3D from the map** — buildings raised from OSM footprints, measured terrain relief, toggleable data layers, routing and live transit boards.
- **Build in place** — create, texture, and reshape objects with a gizmo and edit dialog.
- **A sky that moves** — physically-based atmosphere with a day/night cycle, sun, moon, stars and weather.
- **Shared, in real time** — an authoritative headless sim keeps avatars and world state in sync across clients.
- **An AI copilot** — an in-client panel for LLM-assisted world building.

## Screenshots

| | | |
|---|---|---|
| ![Groningen city centre](docs/img/city.png) | ![The same streets at dusk](docs/img/city-dusk.png) | ![Third-person avatar](docs/img/avatar.png) |
| ![Planned route](docs/img/route.png) | ![Atmosphere limb from 120 km](docs/img/horizon.png) | ![Earth from orbit](docs/img/globe.png) |

_Generated with `cargo shots` from the camera positions in [`docs/shots.toml`](docs/shots.toml) — headless, no window, same framing every time._

## Three builds

The same client ships three ways, from one codebase:

- **Web (WebAssembly)** — play in a browser at **[viberfox.garage44.eu](https://viberfox.garage44.eu)**. Built for wasm with real threads and SIMD, published on every push to `main`. Needs WebGPU.
- **Desktop (native)** — Linux and Windows binaries on the [releases page](https://code.garage44.eu/jeroen/viberfox/releases), or build from source. macOS builds from source too; Linux is the primary development target. The published binaries need a CPU with **AVX2 and FMA** (Haswell/Excavator or newer) — they are compiled for the `x86-64-v3` baseline, and on anything older they stop with an illegal instruction; build from source there.
- **Android** — an installable APK at **[viberfox.garage44.eu/apk](https://viberfox.garage44.eu/apk/)** (arm64, Android 9+), rebuilt on every push to `main`; that page walks through the two dialogs Android puts in front of an app it did not get from the Play Store. Or build it yourself with `crates/viberfox_android/build-apk.sh`. Early: it is the offline map viewer, typing goes through the app's own on-screen keyboard, and it is the least-tested of the three. [What that means](docs/notes/android-build.md).

Native is the faster and more capable of the three — the web build runs against WebGL2 and a browser's threading rules. Where the platforms differ, one API hides it (`platform/`).

## Running from source

```bash
cargo run -p viberfox
```

Starts in **solo mode**: an in-process sim runs on loopback and the client connects over the same authenticated path an external server uses. Your world is persisted to `data/world.db`.

```bash
cargo run -p viberfox -- --connect <host:port>   # split mode: connect to a live sim
cargo run -p viberfox -- --offline               # no sim: single user, no auth or persistence
cargo run -p viberfox_simulator                  # the sim server, standalone
```

You need a Rust toolchain, a C compiler and a working GPU driver — details, release builds and the web build are in the [development guide](docs/guides/development.md). **Run `git lfs install` before cloning**: binary assets live in Git LFS and several are compiled into the binary, so a pointer file fails at runtime rather than at build time.

## Documentation

- [User manual](docs/manual.md) — controls, getting around, authoring, the developer panel.
- [Changelog](CHANGELOG.md) — what changed, per release.
- [Development guide](docs/guides/development.md) — prerequisites, dev loop, tests, code map, conventions, CI.
- [Server guide](docs/guides/server.md) — deploying the sim and the self-hosted geo services.
- [`docs/notes/`](docs/notes/) — standing constraints and measurements; [`CLAUDE.md`](CLAUDE.md)'s *Known issues* is the trap list worth skimming first. `docs/adr/` and `docs/abstract/` are a frozen archive of the earlier decision log.

## Architecture

| Crate | Role |
|-------|------|
| `viberfox_core` | Shared protocol types and tile/coordinate math |
| `viberfox_geo` | Bevy-free map geodata: MVT decode, road classes, tree scatter, the routing graph |
| `viberfox_simulator` | Headless TCP simulation server (Tokio + SQLite), no Bevy |
| `viberfox` | Bevy game client — rendering, ECS systems, UI |
| `big_space` | Vendored floating-origin fork, for planet-scale coordinates |

## Licence

Viberfox is dual-licensed under **[MIT](LICENSE-MIT) OR [Apache-2.0](LICENSE-APACHE)**, at your option — the Rust ecosystem's default pair, and the licence of the vendored `big_space` fork, so the two sets of notice obligations line up. [`LICENSE`](LICENSE) has the detail.

That covers the source. The map data, the geodata services, the avatars and the font each carry their own terms, and several of them — the ODbL, CC BY 4.0, the OFL — require their notice to travel with anything built from the work. **[`LICENSE-THIRD-PARTY.md`](LICENSE-THIRD-PARTY.md) is that notice, and it is the canonical list**: it ships in the release tarball, in the Windows zip and inside the APK, and the in-app About panel renders it. The Credits below are the same list written for a reader; add a source to the file, not only here.

## Credits

- **Engine** — built with [Bevy](https://bevy.org/).
- **Floating origin** — [`big_space`](https://github.com/aevyrie/big_space) by Aevyrie, MIT OR Apache-2.0. Vendored here as a fork ported to the Bevy release we track.
- **Avatar characters** — [Quaternius](https://quaternius.com/) *Ultimate Modular Men/Women*, CC0. Consider supporting the work on [Patreon](https://www.patreon.com/quaternius).
- **Map data** — © [OpenStreetMap](https://www.openstreetmap.org/copyright) contributors, under the ODbL. Building detail and POIs come from [Overpass](https://overpass-api.de/), place search and reverse geocoding from [Nominatim](https://nominatim.org/) — both self-hosted, and usable against the public instances under the [OSMF usage policies](https://operations.osmfoundation.org/policies/).
- **Vector tiles** — [VersaTiles](https://versatiles.org/), in the [Shortbread](https://shortbread-tiles.org/) schema. Self-hosted mirror of their OSM-derived planet build.
- **Buildings (LoD2.2)** — [3DBAG](https://3dbag.nl/) by the 3D geoinformation group, TU Delft, CC BY 4.0 — derived from the Kadaster's [BAG](https://www.kadaster.nl/zakelijk/registraties/basisregistraties/bag) registry and AHN, both via [PDOK](https://www.pdok.nl/). Netherlands only.
- **Elevation** — [AHN](https://www.ahn.nl/) height rasters (DTM/DSM), served by [PDOK](https://www.pdok.nl/), CC0. Netherlands only.
- **Public transport** — live departures and vehicle positions from [OVapi](https://ovapi.nl/), the open key-free feed of Dutch [NDOV](https://ndovloket.nl/) operator data. Netherlands only.
- **Weather** — forecasts from [MET Norway](https://api.met.no/), CC BY 4.0, and global cloud cover from the [NOAA Global Forecast System](https://nomads.ncep.noaa.gov/), public domain.
- **Night sky** — star positions and magnitudes from the [HYG Database](https://github.com/astronexus/HYG-Database) v4.1 by David Nash, CC BY-SA 4.0 — a compilation of the Hipparcos, Yale Bright Star and Gliese catalogs.
- **Type** — [Plus Jakarta Sans](https://fonts.google.com/specimen/Plus+Jakarta+Sans) by Tokotype, SIL Open Font License 1.1.
