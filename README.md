# Cartopolis

A shared, walkable 3D copy of the real world, built from open public data. Rust + [Bevy](https://bevy.org/). Runs on the desktop, in the browser and on Android, and you can host your own.

**[cartopolis.org](https://cartopolis.org)** (WebGPU) · [desktop releases](https://code.garage44.eu/jeroen/cartopolis/releases) (x86_64 needs AVX2 + FMA) · [Android APK](https://cartopolis.org/apk/)

[![Six screens of the app: a building card, local chat, a private chat's map, map layers, route planning, the globe](docs/img/readme.jpg)](docs/img/readme.jpg)

## Use it

Nothing to install for the web: open **[cartopolis.org](https://cartopolis.org)** in a browser with WebGPU.

- **Desktop:** download from [releases](https://code.garage44.eu/jeroen/cartopolis/releases). Linux: unpack `cartopolis-linux-x86_64.tar.gz` and run `./run.sh`. Windows: unpack the zip and run `cartopolis.exe`. Needs a GPU driver; x86_64 needs AVX2 + FMA.
- **Android:** install the APK from [cartopolis.org/apk](https://cartopolis.org/apk/).

The app starts with the map. To meet people, choose **☰ ▸ Sign in…**: it signs in to cartopolis.org, or to your own server if you type its address under **Server**.

## Host your own server

A server is one program, `cartopolis-server`: the shared world, accounts and chat, the website, and the map data and place search for its area. You need a Linux machine with systemd and a domain name pointed at it. Install [Caddy](https://caddyserver.com/docs/install) first for HTTPS; the installer then sets it up too.

```bash
curl -fsSL https://cartopolis.org/server/install.sh | sh
```

It asks for the server's address and an admin account, starts the server and adds the address to Caddy. Open `https://maps.example.org`, or sign in from the app with `maps.example.org` as the server. The map data for your area is made from the website: sign in as the admin and open **Admin ▸ Data**.

The server updates itself once a day from cartopolis.org. It keeps the previous version and its database, and goes back to them when a new version does not start. To turn that off: `sudo systemctl disable --now cartopolis-server-update.timer`.

The download is also at [cartopolis.org/server](https://cartopolis.org/server/cartopolis-server-linux-x86_64.tar.gz); unpacked, `./install.sh` does the same. To build it from source: `deploy/build/server-package.sh`. The [server guide](docs/guides/server.md) has the settings and how the hosted server is set up.

## Build it yourself

Install [Git LFS](https://git-lfs.com/) **before cloning**: fonts, icons and test fixtures are compiled in, and without LFS the build embeds pointer files and fails at runtime. Then `cargo run -p cartopolis` runs the app. Rust, a C compiler and a GPU driver are needed; Linux links with `clang` + `lld`. The [development guide](docs/guides/development.md) covers the dev loop, tests, the web and Android builds, the code map and CI.

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
