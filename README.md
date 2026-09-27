# Cartopolis

**A social map of the real world — see how a place is built, and talk about it with the people who are there.**

![Groningen city centre, built from open public data](docs/img/city.png)

A map tells you where things are. It does not tell you what a building is, when it was built or what is planned for the empty lot next to it, and it has nowhere to ask. Cartopolis is a walkable 3D copy of the real world, built from open public data — the buildings, the roads, the ground, the trees and street lights, the tram that is actually due — and it is shared: the people looking at a place can meet there, talk about it and point each other at it. Like a map, but social.

It opens in a browser or on a phone with nothing to install, it is free and open source, and anyone can run their own.

## A view on public infrastructure

**Built from the registers, not modelled by hand.** Every building comes from the [BAG](https://www.kadaster.nl/zakelijk/registraties/basisregistraties/bag) and its measured LoD2.2 shape from [3DBAG](https://3dbag.nl/); the ground is the [AHN](https://www.ahn.nl/) height survey; street trees, street lighting, pavements and garden boundaries come from the national and municipal object registers; the basemap can be switched to [PDOK](https://www.pdok.nl/)'s 25 cm aerial photography; departure boards and vehicle positions come from the live transit feed. When a register changes, the world is regenerated from it rather than redrawn.

**Every building answers questions.** Select one and a card shows what the public record holds — year of construction, use, number of units, floor area, status, monument listing, district and every address in it — while the building itself turns to glass so you can see where it stands. Walk inside and its storeys are there, reached by stairs.

**The rest of the world around it.** The sun stands where it really stands, the weather is the real forecast, and a route planner finds the way on foot, by bike or by car. Keep going up and the street gives way to the whole planet, in one continuous space.

## A place to share what you know about a place

**Talk where you stand.** An authoritative server keeps everyone in one shared world. People see each other's avatars and talk in local chat — scoped by how far away someone is standing, so the conversation belongs to the place — in proximity voice, across the whole world, or in private conversations that are end-to-end encrypted.

**Point at it.** A link carries a place *and* the way you were looking at it, and whoever opens it — in the browser or in the app — stands exactly where you stood. Saved places keep a photo, and are organised in folders of pins on the map.

**Put something there.** Objects and 3D models can be placed, shaped and scripted in the world; an architect's IFC model can replace a building's surveyed shell; and a whole set of content can be exported and imported as a document through an HTTP API — so a proposal can stand where it would be built, be looked at by the people it concerns, and be taken down again.

Blocking, reporting and account deletion are enforced on the server, and a web console lets an operator manage accounts and run the data pipelines.

## Who it is for

- **Anyone curious about where they live** — what that building is, what the registers say about it, what is due at the stop around the corner.
- **Neighbours and communities** — a shared place to meet, point at things and talk about them, rather than a comment thread with no map.
- **Municipalities and governments** — a twin assembled from the base registers they already maintain, a way to put a plan in front of the people it affects, and a server they can host themselves.
- **Planners, architects and builders** — somewhere to stand a design in its real surroundings and let other people walk around it.

## Screenshots

| | |
|---|---|
| ![Bird's-eye over Groningen's canal ring](docs/img/canals.png) | ![The southern ring road in its cut beside DUO](docs/img/tunnel.png) |
| _The canal ring, its quays and bridges — water, ground and buildings from the registers._ | _The southern ring road sunk into its cut beside DUO, on the measured ground._ |
| ![The main station and rail yard from above](docs/img/station.png) | ![The Grote Markt and the Martinitoren](docs/img/grote-markt.png) |
| _The main station, its tracks and the rail yard._ | _The Grote Markt and the Martinitoren (1482)._ |
| ![Forum Groningen beside the Martinitoren](docs/img/forum.png) | ![The town hall selected, with its register card](docs/img/building.png) |
| _Forum Groningen (2020) on the Nieuwe Markt._ | _Select a building — here the town hall — and read what the public registers hold about it._ |
| ![Groningen from 3 km south-west](docs/img/skyline.png) | ![The whole city from 1.2 km up](docs/img/city-far.png) |
| _The old town from 3 km out, the Martinitoren above it._ | _The whole city from above: the rings, the canals and the land around._ |
| ![Eemshaven's harbour basins and wind turbines](docs/img/eemshaven.png) | ![Eemshaven from over the Wadden Sea](docs/img/eemshaven-sea.png) |
| _Eemshaven: harbour basins, quays, power stations and wind turbines._ | _The same port from over the Wadden Sea._ |
| ![Two people meeting on the Grote Markt](docs/img/meet.png) | ![Earth from orbit](docs/img/globe.png) |
| _Meet people in the place itself, and talk about it there._ | _From one street to the whole planet, without a loading screen._ |

_Generated with `cargo shots` from the camera positions in [`docs/shots.toml`](docs/shots.toml) — headless, no window, same framing every time._

## Where it works

The map, the sky, the weather, routing and the shared world work anywhere on Earth. The register-based detail — measured buildings and their records, ground relief, surveyed street objects, aerial photography and live transit — comes from **Dutch** open data, and today covers the **Netherlands**; municipal registers add more where a city publishes them (Groningen does). Elsewhere those layers degrade rather than break: the ground goes flat, buildings are raised from their OpenStreetMap footprints, and the departure boards stay empty. Each register is one pipeline in [`crates/atlas`](crates/atlas), so bringing in another country's is a pipeline, not a rewrite.

It is written in Rust on the [Bevy](https://bevy.org/) engine, runs in the browser, on the desktop and on Android from one codebase.

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
| `cartopolis_e2ee` | End-to-end encrypted private chats (MLS); the server relays sealed bytes it cannot open |
| `cartopolis_simulator` | The shared-world server (Tokio + SQLite): avatars, chat, voice, objects, accounts; no Bevy |
| `cartopolis_atlas` | The register pipelines (BAG, 3DBAG, AHN, BGT, …), the terrain server and the operator web console |
| `cartopolis` | Bevy client — rendering, ECS systems, UI |
| `cartopolis_android` | The Android app shell around the client |
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
- **Aerial photography** — [Beeldmateriaal Nederland](https://www.beeldmateriaal.nl/), served as Luchtfoto RGB by [PDOK](https://www.pdok.nl/), CC BY 4.0. Netherlands only.
- **Street objects** — street trees, lighting and containers from [gemeente Groningen open data](https://data.groningen.nl/), CC BY 4.0. Groningen only.
- **Elevation** — [AHN](https://www.ahn.nl/) height rasters (DTM/DSM), served by [PDOK](https://www.pdok.nl/), CC0. Netherlands only.
- **Public transport** — live departures and vehicle positions from [OVapi](https://ovapi.nl/), the open key-free feed of Dutch [NDOV](https://ndovloket.nl/) operator data. Netherlands only.
- **Weather** — forecasts from [MET Norway](https://api.met.no/), CC BY 4.0, and global cloud cover from the [NOAA Global Forecast System](https://nomads.ncep.noaa.gov/), public domain.
- **Night sky** — star positions and magnitudes from the [HYG Database](https://github.com/astronexus/HYG-Database) v4.1 by David Nash, CC BY-SA 4.0 — a compilation of the Hipparcos, Yale Bright Star and Gliese catalogs.
- **Type** — [Plus Jakarta Sans](https://fonts.google.com/specimen/Plus+Jakarta+Sans) by Tokotype, SIL Open Font License 1.1.
