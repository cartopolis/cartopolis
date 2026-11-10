---
topic: the-server-stack
triggers: [crates/core/src/services.rs, crates/cartopolis/src/services.rs, "/api/services", ServiceDirectory, geo_nav.rs, docs/guides/server.md]
updated: 2026-11-10
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
