# Cartopolis

**A digital twin of the real world that you can walk around in.**

Cartopolis assembles a living 3D city out of open data. The streets, the buildings, the ground relief and the trees are real, imported from [OpenStreetMap](https://www.openstreetmap.org/) and public geodata registries. So is the weather, the position of the sun, and the tram that is genuinely due in four minutes. Stand on a pavement in a real place and keep going up — through the atmosphere, past the planet turning below you, into orbit — then drop back down anywhere on Earth. No loading screens, and no invented geography.

It is written in Rust on the [Bevy](https://bevy.org/) engine, runs in a browser, on the desktop and on Android, and is free and open source.

- **Real places, streamed** — the world loads around you as you move, anywhere on the planet.
- **Pavement to orbit** — one continuous space, from a doorway to the whole globe.
- **Raised from the map** — buildings from real footprints, measured terrain, routing, street names and points of interest.
- **A sky that behaves** — physically-based atmosphere, day and night, sun, moon, stars and real forecast weather.
- **Live city data** — departure boards and vehicle positions from the actual transit feed.
- **Shared and editable** — an authoritative server keeps people, avatars and edits in sync; objects can be placed and reshaped in the world, with an AI panel to help.

The map, the sky and the weather work anywhere on Earth. Three things do not, because they come from national open-data registries rather than a global one: **measured ground relief, high-detail (LoD2.2) buildings and live transit are the Netherlands only.** Everywhere else those degrade rather than break — the ground goes flat, buildings are extruded from their OpenStreetMap footprints, and the departure boards stay empty.

## Screenshots

| | | |
|---|---|---|
| ![Groningen city centre](docs/img/city.png) | ![The same streets at dusk](docs/img/city-dusk.png) | ![Third-person avatar](docs/img/avatar.png) |
| ![Planned route](docs/img/route.png) | ![Atmosphere limb from 120 km](docs/img/horizon.png) | ![Earth from orbit](docs/img/globe.png) |

_Generated with `cargo shots` from the camera positions in [`docs/shots.toml`](docs/shots.toml) — headless, no window, same framing every time._

## Try it

One codebase, three builds:

- **Browser** — open **[cartopolis.org](https://cartopolis.org)**. Nothing to install; needs a WebGPU-capable browser.
- **Desktop** — Linux and Windows binaries on the [releases page](https://code.garage44.eu/jeroen/cartopolis/releases), or build from source (macOS included). This is the fastest and most capable of the three. The published binaries need a CPU with **AVX2 and FMA** — roughly 2013 or newer; on anything older, build from source.
- **Android** — an APK at **[cartopolis.org/apk](https://cartopolis.org/apk/)** (arm64, Android 9+); the page walks through the dialogs Android shows for an app that did not come from the Play Store. Early days: it is the map viewer only, and the least-tested of the three. [What that means](docs/notes/android-build.md).

Where the platforms differ, one API hides it (`platform/`).

## Running from source

```bash
cargo run -p cartopolis
```

Starts **offline**: the streamed map and a local avatar, no simulator and no account. Sign in to a server from ☰ ▸ Sign in… to share a world with other people.

```bash
cargo run -p cartopolis -- --connect <host>        # split mode: connect to a live sim (TLS)
cargo run -p cartopolis -- --offline               # no sim: single user, no auth or persistence
cargo run -p cartopolis_simulator                  # the sim server, standalone
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
| `cartopolis_core` | Shared protocol types and tile/coordinate math |
| `cartopolis_geo` | Bevy-free map geodata: MVT decode, road classes, tree scatter, the routing graph |
| `cartopolis_simulator` | Headless TCP simulation server (Tokio + SQLite), no Bevy |
| `cartopolis` | Bevy game client — rendering, ECS systems, UI |
| `big_space` | Vendored floating-origin fork, for planet-scale coordinates |

## Licence

Cartopolis is dual-licensed under **[MIT](LICENSE-MIT) OR [Apache-2.0](LICENSE-APACHE)**, at your option — the Rust ecosystem's default pair, and the licence of the vendored `big_space` fork, so the two sets of notice obligations line up. [`LICENSE`](LICENSE) has the detail.

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
