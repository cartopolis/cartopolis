---
topic: the-server-stack
triggers: [crates/server, deploy/server-package, server-package.sh, atlas_proxy.rs, static_files.rs, pipeline.tsx, crates/atlas/src/geocoder.rs, crates/core/src/services.rs, crates/cartopolis/src/services.rs, crates/atlas/src/osm, layers.rs, route_ahead.rs, street_detail.rs, "/api/services", ServiceDirectory, geo_nav.rs, docs/guides/server.md]
updated: 2026-11-12
---

# What a Cartopolis server is made of, and where its clients find it

## The client asks the server where its geo services are

`GET /api/services` (`cartopolis_core::services::ServiceDirectory`) names the
geocoder, the OSM query endpoint, the map-data cell tree, terrain and height. The
simulator answers from `geocoder_url`, `osm_query_url`, `map_data_url`,
`terrain_url` and `height_url` (`CARTO_*` in its environment); each defaults to
cartopolis.org's. The client asks once per session, after the handshake, and each
resolver reads its own `CARTO_*` override first, then the server's answer, then
its compiled-in default.

Before this every client asked cartopolis.org whatever it had signed in to, so a
second server could not serve its own search or map data. The tile template was
the one exception: it has been in the handshake since ADR-014.

**An HTTP document, not a handshake field**, so it moved no protocol version: an
older server answers 404 (or its web host's page) and the client keeps its
defaults. **Only a value that differs from the default counts** — the web build's
defaults for terrain and height are relative paths, and the server spelling the
same place absolutely would change only the cache key. On the hosted server Caddy
must pass `/api/services` to the simulator, or the web client gets the page and
keeps its defaults.

## What the hosted stack held (measured 2026-11-10, garage44.eu)

| Part | Runs as | Disk | Memory |
|---|---|---|---|
| Simulator | systemd, native | 0.55 GB | 109 MB |
| Atlas | systemd, native | 65 GB (AHN6 sheets 62 GB) + 1.9 GB site tree | 6.7 GB |
| Nominatim (NL, `address` style) | Docker, PostgreSQL | 23 GB | 2.5 GB |
| Overpass (NL, daily diffs) | Docker | 19 GB | 63 MB |
| VersaTiles (planet z0–14 + land cover) | Docker | 68.6 GB | 237 MB |
| Caddy | Docker | — | 88 MB |

Nominatim was frozen at its 2026-07-11 import: its update node cache (105 GB)
was deleted, so updating it means a full re-import. The Compose file and the
Caddyfile exist only on the host; `docs/guides/server.md` holds an older copy.

## What the client actually uses Nominatim and Overpass for

- **Nominatim:** `/search` (search bar, route endpoints, the MCP agent) and
  `/reverse` (route endpoints, bookmarks, camera photos). Nothing else.
- **Overpass, from the client:** building footprints where 3DBAG has no cell,
  tall structures where atlas has not cut the cell, street detail, speed limits
  along a route, stations and shops.
- **Overpass, from atlas:** `tall`, the backdrop's roads and water, artworks for
  `registers`, building material for `bag`.

A fixed set of questions about one country, which is why both are candidates to be
answered from files atlas builds from one OSM extract rather than kept as services.

## Overpass answers cut from the extract (`crates/atlas/src/osm`)

Atlas's `tall`, `street`, `transit`, `shops` and `speeds` datasets read a Geofabrik
`.osm.pbf` (`ATLAS_OSM_EXTRACT`, a URL or a path; the Netherlands by default,
re-downloaded after a week into `<data_dir>/osm/`) and write, per cell, the
document Overpass would have answered to the client's query. The client reads a
covered cell from the map-data host and asks Overpass only for the rest.

Checked 2026-11-10 against Overpass, Groningen, Netherlands extract of that day:

- **tall**: 19 of 35 z11 cells identical; every difference was an element
  added to or deleted from OSM after the published files were made (2026-09-26).
- **street**, the Grote Markt cell (z14 8490/5320): all 1044 nodes identical;
  **our Overpass returned 285 ways, the extract 452**, none the other way round.
  The missing ones are old (a wall from 2021, way 1001079088) and absent from
  `overpass.cartopolis.org` entirely, even asked by id: that instance lost ways,
  so the extract is not merely as good but more complete.
- Building the first five layers for Groningen from the 1.4 GB extract took
  2.5 min on mac-1 and 2.2 GB, almost all of it `speeds`: a way cannot be placed
  before its nodes are read, so every speed-limited way in the country (966k)
  was held until the third pass. The first pass now also collects the id of
  every node in the area, and a way with none of them is dropped on sight:
  8,298 ways, and `osmbuildings` + `speeds` together peak at 0.54 GB
  (199,656 buildings over 289 cells in 112 s).
- **An area is read in z9 tiles** (about 70 × 45 km), one at a time, so the
  whole country is a sequence of province-sized reads rather than one that
  holds every building in it. Each costs three passes over the extract.
- Atlas asks no Overpass at all since 2026-11-10: `registers` (artworks), `bag`
  (facade materials, relations by tag) and the backdrop route read the extract.

## Options looked at for replacing them (2026-11-10, desk research)

- **Photon** (komoot): Java + OpenSearch; Europe dump 32 GB. A service, not a
  library — the shape being removed.
- **Pelias, carmen, addok**: Elasticsearch + Node, stalled, Python + Redis.
- **Nominatim's SQLite export**: still needs the PostgreSQL import to make it.
- **open-geocode** (Rust, MIT): OSM PBF → memory-mapped packs, Tantivy text index,
  R-tree reverse. Its own benchmark for every Dutch OSM address: 0.36 GB on disk,
  1.1 GiB serving, built in under 2 minutes. Young; a design reference.
- **overpass-rust**: AGPL-3.0 and nine commits; not a dependency.
- **Headway** (Photon + Valhalla + PMTiles in Compose): one command, many services.
- **Public tiles**: `tiles.versatiles.org` serves the same Shortbread schema as the
  mirror, so a server can point `osm_tile_url_template` there and host no tiles.

## Place search without Nominatim (`crates/atlas/src/geocoder.rs`)

Atlas's `geocoder` dataset indexes every address, street, named place and named
amenity, shop, station and sight in the extract into one file,
`<data_dir>/geocoder/index.bin`, which atlas answers `/geocode/search` and
`/geocode/reverse` from in Nominatim's `jsonv2` shape; the server's
`geocoder_url` points clients at `…/geocode`. Search is by word prefix, so it
matches while typed, which Nominatim could not (`geocoding-latency.md`).

Measured 2026-11-11, whole Netherlands, mac-1, release build:

- 9,950,479 addresses on 248,602 streets, 281,216 places; a 201 MB file built in
  14 min at 3.4 GB peak; loaded in 1.3–2.5 s.
- Every query of a test set of 20 (towns, streets, addresses, postcodes,
  stations, museums) answered in under 5 ms, most under 0.3 ms; a reverse
  lookup in under 0.2 ms.
- Ranking: a city before a hamlet of the same name, a station before a street
  named after it; nearness to the asker (`viewbox`) outweighs an exact name, so
  "Zuiderdiep" from Groningen finds the Gedempte Zuiderdiep, not the Zuiderdiep
  40 km away.
- **Not found:** anything mapped only as a relation — Schiphol's aerodrome is
  one, so "Schiphol" finds the station and the hamlet but not the airport.

## One program, two processes (`crates/server`, since 2026-11-11)

`cartopolis-server` is what a new server installs: one binary and one
`cartopolis.toml`, the simulator's keys at the top and atlas's under `[atlas]`.
It runs the simulator and starts itself again as `cartopolis-server atlas`, a
second process it restarts when it ends (a doubling pause up to a minute, reset
after a long run) and that ends when it does (it holds the child's stdin). The
simulator's HTTP listener answers atlas's paths by passing them on
(`atlas_proxy`, `SimConfig::atlas_url`), so a TLS proxy in front needs one
upstream.

**Rejected: one process.** Merging was easy — no path is claimed by both, atlas
pulls in none of the simulator's encryption or call code — but `atlas.md` keeps
atlas out of the tick's runtime, and the numbers agree: the whole-country layer
run peaked at 4.3 GB and the geocoder build at 3.5 GB. A crash there must not
end the world.

It serves the web UI (`web_root`) and atlas's map-data tree under `/data/`
(`static_files`, with the `.br` copies atlas writes), and points every service
the file leaves at cartopolis.org's address at its own `public_base_url` —
tiles at the public `tiles.versatiles.org`, same schema as the mirror. Still the
operator's: TLS in front. The hosted server still runs the two as separate
systemd units behind Caddy; it does not use this binary yet.

**The download** (`deploy/build/server-package.sh` → `dist/server/`, published
by `midas deploy server` at `https://cartopolis.org/server/` since 2026-11-12) is
the binary, the built web UI in `web/` beside it — found there when the file
names no `web_root` — an example `cartopolis.toml`, the systemd units and
`install.sh`. That one script is both `curl … | sh` and the unpacked package's
installer: it puts them in `/opt/cartopolis-server`, asks for the address and the
first admin on `/dev/tty` (so a piped script can still ask; `cartopolis-server
create-admin` is the only way to an admin without the separate simulator
binary), and adds the address to Caddy when Caddy is installed. The binary is
`server.sh`'s, linked against glibc ≤ 2.35. Not in it: calls, which need a
public UDP address (`calls_listen`, `calls_public_addr`) the installer cannot
know.

**Self-update** is a daily timer running `install.sh --update`; the version is
the tarball's sha256 (`installed.sha256`). The health check after an update is
`GET /api/services`, not `systemctl is-active`: a server that crashes on start
is restarted every 3 s and reads as active in between. A version rolled back is
written to `failed.sha256` and skipped until a newer one is published. Forgejo
releases were not used for this: the last one was a July 2026 prerelease, and
Forgejo 1.22 here has no `latest/download` URL. Tested 2026-11-12 in a
throwaway Ubuntu 24.04 with systemctl and caddy stubbed (install via sudo, idle
update, update, broken update rolled back and skipped, bad checksum refused);
not yet under real systemd.

Admin › Data shows the extract (source, size, when downloaded) and runs every
OSM layer over the country in one job (`POST /api/pipeline/osm`): a route of its
own because a typed area is held to 400 km².
