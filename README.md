# Cartopolis

**A social map of the real world: see how a place is built, and talk about it with the people who are there.**

![Groningen city centre, built from open public data](docs/img/city.png)

A walkable 3D copy of the real world, built from open public data and shared between everyone looking at it. Runs in the browser, on the desktop and on Android. Free, open source, self-hostable.

## Public infrastructure

- **Built from the registers** — buildings from [BAG](https://www.kadaster.nl/zakelijk/registraties/basisregistraties/bag) and [3DBAG](https://3dbag.nl/), ground from [AHN](https://www.ahn.nl/), street trees, lighting and paving from the national and municipal object registers, [PDOK](https://www.pdok.nl/) aerial photography, live transit.
- **Every building has a card** — year built, use, units, floor area, status, monument listing, addresses. Walk in and its storeys are there.
- **The world around it** — real sun, real weather, routing on foot, by bike or by car, and one continuous space from a doorway to orbit.

## Sharing a place

- **Talk where you stand** — avatars, local chat scoped by distance, proximity voice, world chat, end-to-end encrypted private chats.
- **Point at it** — a link opens your exact view in the browser or the app; saved places with photos and map pins.
- **Put something there** — place and script objects and 3D models, swap a building for an IFC model, export and import content over an HTTP API.
- **Moderation** — blocking, reporting and account deletion, enforced on the server; a web console for accounts and data pipelines.

For residents, communities, municipalities and governments, and planners and architects.

## Screenshots

| | |
|---|---|
| ![Bird's-eye over Groningen's canal ring](docs/img/canals.png) | ![The southern ring road in its cut beside DUO](docs/img/tunnel.png) |
| _Canal ring_ | _Ring road tunnel by DUO_ |
| ![The main station and rail yard from above](docs/img/station.png) | ![The Grote Markt and the Martinitoren](docs/img/grote-markt.png) |
| _Main station and rail yard_ | _Grote Markt and Martinitoren_ |
| ![Forum Groningen beside the Martinitoren](docs/img/forum.png) | ![The town hall selected, with its register card](docs/img/building.png) |
| _Forum Groningen_ | _Building card: the town hall_ |
| ![Groningen from 3 km south-west](docs/img/skyline.png) | ![The whole city from 1.2 km up](docs/img/city-far.png) |
| _Groningen from 3 km_ | _The city from 1.2 km up_ |
| ![Eemshaven's harbour basins and wind turbines](docs/img/eemshaven.png) | ![Eemshaven from over the Wadden Sea](docs/img/eemshaven-sea.png) |
| _Eemshaven_ | _Eemshaven from the Wadden Sea_ |
| ![Two people meeting on the Grote Markt](docs/img/meet.png) | ![Earth from orbit](docs/img/globe.png) |
| _Meeting on the Grote Markt_ | _Earth from orbit_ |

_Generated with `cargo shots` from [`docs/shots.toml`](docs/shots.toml)._

## Where it works

Map, sky, weather, routing and the shared world: anywhere on Earth. Register detail (measured buildings and their records, relief, street objects, aerial photos, transit): the Netherlands, plus municipal data where a city publishes it. Elsewhere the ground is flat and buildings come from OpenStreetMap. Each register is one pipeline in [`crates/atlas`](crates/atlas).

Written in Rust on [Bevy](https://bevy.org/).

## Try it

- **Browser** — **[cartopolis.org](https://cartopolis.org)**. Needs WebGPU.
- **Desktop** — Linux and Windows on the [releases page](https://code.garage44.eu/jeroen/cartopolis/releases), or build from source (macOS too). Published binaries need AVX2 and FMA (~2013+).
- **Android** — APK at **[cartopolis.org/apk](https://cartopolis.org/apk/)** (arm64, Android 9+). The full client, least tested of the three ([notes](docs/notes/android-build.md)).

## Running from source

```bash
cargo run -p cartopolis
```

Starts offline. Sign in to a server from ☰ ▸ Sign in….

```bash
cargo run -p cartopolis -- --connect <host>        # split mode: connect to a live sim (TLS)
cargo run -p cartopolis -- --offline               # no sim: single user, no auth or persistence
cargo run -p cartopolis_simulator                  # the sim server, standalone
```

Needs Rust, a C compiler and a GPU driver; see the [development guide](docs/guides/development.md). **Run `git lfs install` before cloning** — assets are in Git LFS and some are compiled in.

## Documentation

- [User manual](docs/manual.md) — controls, getting around, authoring, the developer panel.
- [Changelog](CHANGELOG.md) — what changed, per release.
- [Development guide](docs/guides/development.md) — prerequisites, dev loop, tests, code map, conventions, CI.
- [Server guide](docs/guides/server.md) — deploying the sim and the self-hosted geo services.
- [`docs/notes/`](docs/notes/) — constraints, measurements and the traps that fail silently, one topic per file; [`CLAUDE.md`](CLAUDE.md) lists the rules that hold everywhere.

## Architecture

| Crate | Role |
|-------|------|
| `cartopolis_core` | Shared protocol types and tile/coordinate math |
| `cartopolis_geo` | Bevy-free map geodata: MVT decode, road classes, tree scatter, the routing graph |
| `cartopolis_e2ee` | End-to-end encrypted private chats (MLS); the server relays sealed bytes it cannot open |
| `cartopolis_simulator` | The shared-world server (Tokio + SQLite): avatars, chat, voice, objects, accounts; no Bevy |
| `cartopolis_atlas` | The register pipelines (BAG, 3DBAG, AHN, BGT, …), the terrain server and the operator web console |
| `cartopolis` | Bevy client — rendering, ECS systems, UI |
| `cartopolis_android` | The Android app shell around the client |
| `big_space` | Vendored floating-origin fork, for planet-scale coordinates |

## Licence

Source: **[MIT](LICENSE-MIT) OR [Apache-2.0](LICENSE-APACHE)** ([`LICENSE`](LICENSE)). Data, avatars and font carry their own terms; **[`LICENSE-THIRD-PARTY.md`](LICENSE-THIRD-PARTY.md)** is the canonical notice list (the credits below mirror it — add new sources there).

## Credits

- **Engine** — built with [Bevy](https://bevy.org/).
- **Floating origin** — [`big_space`](https://github.com/aevyrie/big_space) by Aevyrie, MIT OR Apache-2.0. Vendored here as a fork ported to the Bevy release we track.
- **Avatar characters** — [Quaternius](https://quaternius.com/) *Ultimate Modular Men/Women*, CC0. Consider supporting the work on [Patreon](https://www.patreon.com/quaternius).
- **Map data** — © [OpenStreetMap](https://www.openstreetmap.org/copyright) contributors, under the ODbL. Building detail and POIs come from [Overpass](https://overpass-api.de/), place search and reverse geocoding from [Nominatim](https://nominatim.org/) — both self-hosted, and usable against the public instances under the [OSMF usage policies](https://operations.osmfoundation.org/policies/).
- **Vector tiles** — [VersaTiles](https://versatiles.org/), in the [Shortbread](https://shortbread-tiles.org/) schema. Self-hosted mirror of their OSM-derived planet build.
- **Buildings (LoD2.2)** — [3DBAG](https://3dbag.nl/) by the 3D geoinformation group, TU Delft, CC BY 4.0 — derived from the Kadaster's [BAG](https://www.kadaster.nl/zakelijk/registraties/basisregistraties/bag) registry and AHN, both via [PDOK](https://www.pdok.nl/). Netherlands only.
- **Aerial photography** — [Beeldmateriaal Nederland](https://www.beeldmateriaal.nl/), served as Luchtfoto RGB by [PDOK](https://www.pdok.nl/), CC BY 4.0. Netherlands only.
- **Street objects** — street trees, lighting and containers from [gemeente Groningen open data](https://data.groningen.nl/), CC BY 4.0. Groningen only.
- **Elevation** — [AHN](https://www.ahn.nl/) height rasters (DTM/DSM), served by [PDOK](https://www.pdok.nl/), CC0. Netherlands only.
- **Public transport** — live departures and vehicle positions from [OVapi](https://ovapi.nl/), the open key-free feed of Dutch [NDOV](https://ndovloket.nl/) operator data. Netherlands only.
- **Weather** — forecasts from [MET Norway](https://api.met.no/), CC BY 4.0, and global cloud cover from the [NOAA Global Forecast System](https://nomads.ncep.noaa.gov/), public domain.
- **Night sky** — star positions and magnitudes from the [HYG Database](https://github.com/astronexus/HYG-Database) v4.1 by David Nash, CC BY-SA 4.0 — a compilation of the Hipparcos, Yale Bright Star and Gliese catalogs.
- **Type** — [Plus Jakarta Sans](https://fonts.google.com/specimen/Plus+Jakarta+Sans) by Tokotype, SIL Open Font License 1.1.
