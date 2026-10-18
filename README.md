# Cartopolis

A shared, walkable 3D copy of the real world, built from open public data. Rust + [Bevy](https://bevy.org/). Runs on the desktop, in the browser and on Android, and you can host your own.

**[cartopolis.org](https://cartopolis.org)** (WebGPU) · [desktop releases](https://code.garage44.eu/jeroen/cartopolis/releases) (x86_64 needs AVX2 + FMA) · [Android APK](https://cartopolis.org/apk/)

[![Six screens of the app: a building card, local chat, a private chat's map, map layers, route planning, the globe](docs/img/readme.jpg)](docs/img/readme.jpg)

## Quick start

Install [Git LFS](https://git-lfs.com/) **before cloning**. Fonts, icons and test fixtures are compiled in, and without LFS the build embeds pointer files and fails at runtime.

```bash
git lfs install
git clone https://code.garage44.eu/jeroen/cartopolis && cd cartopolis
cargo run -p cartopolis                  # client; starts offline, sign in from ☰ ▸ Sign in…
```

Needs Rust, a C compiler and a GPU driver. Linux links with `clang` + `lld` (`.cargo/config.toml`).

```bash
cargo run -p cartopolis_simulator        # sim server on 127.0.0.1:4747 (HTTP API on :4748)
cargo run -p cartopolis -- --connect <host>   # client against a remote sim (TLS)
cargo run -p cartopolis --features fast-dev   # dynamic linking for faster rebuilds; dev only

cargo test --locked --workspace --exclude cartopolis_android   # what CI runs
cargo shots                              # regenerate the image above (docs/shots.toml)
```

Web: `crates/cartopolis/serve-threads.sh`. Android: `crates/cartopolis_android/build-apk.sh`. The [development guide](docs/guides/development.md) covers both, plus conventions and CI.

## Workspace

| Crate | Role |
|---|---|
| `cartopolis` | The Bevy client: rendering, ECS systems, egui UI |
| `cartopolis_core` | Wire protocol (`NetMessage`), tile and coordinate math |
| `cartopolis_geo` | Map geodata without Bevy: MVT decode, roads, routing graph, terrain grading, per-cell file formats |
| `cartopolis_simulator` | The shared-world server (Tokio + SQLite): avatars, chat, voice, prims, accounts, admin HTTP API |
| `cartopolis_atlas` | Data pipelines (BAG, 3DBAG, AHN, BGT, …), terrain tiles, and the web UI in `crates/atlas/web` |
| `cartopolis_e2ee`, `_chat`, `_web_chat` | MLS end-to-end encrypted chats; the engine, and its wasm build for the web UI |
| `cartopolis_calls` | Encrypted calls: codecs and SFrame for the client, a blind forwarder for the server |
| `cartopolis_materials` | The client's custom render materials |
| `cartopolis_mcp` | MCP server for driving a running client from an editor |
| `cartopolis_vio`, `_gnss` | Visual-inertial odometry; external RTK GNSS receivers |
| `cartopolis_viewer` | `carto-view`: draws a tile's geometry in seconds, without the client |
| `cartopolis_android` | The Android app shell |
| `big_space`, `web_task` | Vendored forks: floating origin on our Bevy version; the browser task executor |

## Where it works

The map, sky, weather, routing and shared world work anywhere on Earth. Register detail is available for the Netherlands only: measured buildings and their records, relief, street objects, aerial photography and transit. Elsewhere the ground is flat and buildings come from OpenStreetMap.

## Docs

- [Development guide](docs/guides/development.md): prerequisites, dev loop, tests, code map, CI
- [Server guide](docs/guides/server.md): deploying the sim and the self-hosted geo services
- [User manual](docs/manual.md): controls and authoring · [Changelog](CHANGELOG.md)
- [`docs/notes/`](docs/notes/): measurements, constraints and the traps that fail silently, one topic per file. [`CLAUDE.md`](CLAUDE.md) lists the rules that hold everywhere.

## Licence

Code is **[MIT](LICENSE-MIT) OR [Apache-2.0](LICENSE-APACHE)** ([`LICENSE`](LICENSE)). Data, models and the font carry their own terms. **[`LICENSE-THIRD-PARTY.md`](LICENSE-THIRD-PARTY.md)** is the canonical list; the credits below mirror it, so add new sources there.

## Credits

- **Engine**: [Bevy](https://bevy.org/). **Floating origin**: [`big_space`](https://github.com/aevyrie/big_space) by Aevyrie, MIT OR Apache-2.0, vendored and ported.
- **Avatars**: bodies, clothes and hair from [MakeHuman](http://www.makehumancommunity.org/), CC0; motion from [100STYLE](https://www.ianxmason.com/100style/) (Ian Mason, CC BY 4.0) and the [CMU Graphics Lab Motion Capture Database](http://mocap.cs.cmu.edu/).
- **Map data**: © [OpenStreetMap](https://www.openstreetmap.org/copyright) contributors, ODbL. Detail from [Overpass](https://overpass-api.de/), search from [Nominatim](https://nominatim.org/), both self-hosted (public instances under the [OSMF policies](https://operations.osmfoundation.org/policies/)).
- **Vector tiles**: [VersaTiles](https://versatiles.org/), [Shortbread](https://shortbread-tiles.org/) schema, self-hosted mirror.
- **Buildings (LoD2.2)**: [3DBAG](https://3dbag.nl/) by the 3D geoinformation group, TU Delft, CC BY 4.0, derived from the Kadaster's [BAG](https://www.kadaster.nl/zakelijk/registraties/basisregistraties/bag) and AHN via [PDOK](https://www.pdok.nl/). NL.
- **Aerial photography**: [Beeldmateriaal Nederland](https://www.beeldmateriaal.nl/), Luchtfoto RGB via [PDOK](https://www.pdok.nl/), CC BY 4.0. NL.
- **Street objects**: [gemeente Groningen open data](https://data.groningen.nl/), CC BY 4.0. Groningen.
- **Elevation**: [AHN](https://www.ahn.nl/) DTM/DSM via [PDOK](https://www.pdok.nl/), CC0. NL.
- **Public transport**: [OVapi](https://ovapi.nl/), from Dutch [NDOV](https://ndovloket.nl/) operator data. NL.
- **Weather**: [MET Norway](https://api.met.no/), CC BY 4.0; cloud cover from [NOAA GFS](https://nomads.ncep.noaa.gov/), public domain.
- **Night sky**: [HYG Database](https://github.com/astronexus/HYG-Database) v4.1 by David Nash, CC BY-SA 4.0 (Hipparcos, Yale Bright Star, Gliese).
- **Type**: [Plus Jakarta Sans](https://fonts.google.com/specimen/Plus+Jakarta+Sans) by Tokotype, SIL OFL 1.1.
