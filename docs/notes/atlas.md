---
topic: atlas
triggers: [cartopolis_atlas, crates/atlas, atlas, cartopy, "terrain tile", "/terrain/", pipeline, "user management", registration, accounts, "ROBUST", OpenSim, "service design"]
updated: 2026-09-23
---

# `cartopolis_atlas`: everything around the world that is not the simulation

Agreed 2026-09-23. A separate crate and binary in this workspace (`crates/atlas`),
an axum/tokio service depending on `cartopolis_core` and `cartopolis_geo`. It
owns the map data pipelines (ported from `jeroen/cartopy`), the terrain tiles
that `level-of-detail-tile-tree.md` needs, and later registration and user
management.

## Why it exists

The pipelines are TypeScript in cartopy while their readers (`lod22.rs`,
`building_facts.rs`, `surveyed.rs`, `osm_buildings.rs`, `dikes.rs`, `crops.rs`)
are Rust here, and nothing checks that the two agree
(`where-the-map-data-is-built.md`). In one workspace the writer and the reader
share one Rust type per format, so a mismatch is a compile error. It also lets
server-side work use `cartopolis_geo` directly.

## Rules, several learned from OpenSimulator's ROBUST

- **Not inside the simulator process.** The simulator is one tokio runtime shared
  with the 20 Hz tick (`simulator-tick-budget.md`). Pipeline jobs and on-demand
  tile cutting are seconds to minutes of CPU, and a crashed import must not take
  the world down.
- **Service traits with a local and a remote implementation**, as OpenSim's
  connectors are. The dev loop and the tests run standalone in one process;
  production runs atlas separately.
- **Accounts split into credentials, account and presence.** Sign-in is a
  handoff: atlas authenticates and issues a short-lived signed token, and the
  simulator verifies it with a public key. It never sees a password or the
  account table. The known cost: a disabled account keeps working until its token
  expires, hence short-lived tokens.
- **Service-to-service calls are authenticated,** never trusted by network
  location. OpenSim grids that relied on a "private port" exposed it by accident,
  and this VPS also hosts nominatim, overpass and Forgejo.
- **Short sign-in path, nothing on the tick or movement path.** OpenSim logins
  fan out into many synchronous service calls and fail when any one does.
- **Assets are content-addressed and immutable** (already hash-named in
  `crates/simulator/src/assets.rs`), so they cache anywhere.
- **Lazy loading** of inventory and library data. OpenSim pulled whole inventory
  trees at login, and logins were slow.
- **Typed configuration that rejects unknown keys**, against OpenSim's `.ini`
  sprawl.
- OpenSim's region grid does not apply: regions are retired here
  (`retired/regions.md`).

## Order

1. Crate skeleton plus `/terrain/{z}/{x}/{y}`: AHN height grids per tile, cut
   from PDOK on first request and kept on disk; 404 outside coverage. No road
   flattening server-side. **Built** (`crates/atlas`, format
   `cartopolis_geo::terrain_tile`): z8–z17, 129² samples, 33 KB a tile. Against
   live PDOK from the VPS (2026-09-23): a first cut takes 0.2–0.3 s and a repeat
   is ~1 ms from disk. Central Groningen (`14/8490/5318`) spans −1.0 to +10.6 m
   NAP. A tile over the Markermeer is a remembered 404, not a flat tile.
2. Port the cartopy pipelines one at a time, when each next needs changing. Each
   is diffed against the TypeScript output for the same area, as the
   Python-to-TypeScript port was. The admin GUI stays a separate web app pointed
   at atlas's API.
3. Move accounts once the token design is agreed. The in-world player actions
   (block, report, delete my account) stay on the wire, with the simulator
   passing them on.

## Contracts a port must keep

Client code already depends on these shapes, and a port that changes them breaks
it silently (reported by the tunnel/bridge work, 2026-09-23):

- `lod22/{14}/{x}/{y}` cell files and `bag/{14}/{x}/{y}.json`: `identificatie`,
  `status` and the footprint are read per building, and `lod22` also reads the
  cell's street tile on the worker to skip buildings standing in a covered
  tunnel's trench.
- The z14 `streets` layer is **not** a cartopy product. It is the VersaTiles
  planet mirror at `tiles.cartopolis.org`, so a pipeline port does not touch it.
  The tunnel code relies on its `kind`, `tunnel`, `bridge`, `link` and `oneway`
  attributes, and on way endpoints shared at tunnel portal nodes (within 1.5 m).
  Keep that in mind if atlas ever builds its own vector tiles.

## Rejected

- **Inside the simulator.** The tick budget, and independent failure (above).
- **Sharing the account database between the simulator and atlas.** Two writers
  to one SQLite file and migrations coordinated across two services: the kind of
  seam this is meant to remove.
- **The simulator asking atlas at every sign-in.** Then no one can join while
  atlas is down.
- **The name `cartopolis-web`.** "Web" already means the browser build of the
  client and the admin GUI at `web.cartopolis.org`.
