---
topic: atlas
triggers: [cartopolis_atlas, crates/atlas, atlas, "terrain tile", "/terrain/", pipeline, "user management", registration, accounts, "ROBUST", OpenSim, "service design"]
updated: 2026-09-23
---

# `cartopolis_atlas`: everything around the world that is not the simulation

Agreed 2026-09-23. A separate crate and binary in this workspace (`crates/atlas`),
an axum/tokio service depending on `cartopolis_core` and `cartopolis_geo`. It
owns the map data pipelines (ported from a separate TypeScript repository), the terrain tiles
that `level-of-detail-tile-tree.md` needs, and later registration and user
management.

## Why it exists

The pipelines were TypeScript in a separate repository while their readers (`lod22.rs`,
`building_facts.rs`, `surveyed.rs`, `osm_buildings.rs`, `dikes.rs`, `crops.rs`)
are Rust here, and nothing checked that the two agreed
(`where-the-map-data-is-built.md`). In one workspace one commit changes both
sides and one gate tests them, and the writer and the reader *can* share one Rust
type per format, which would make a mismatch a compile error — the port did not
do that yet. It also lets server-side work use `cartopolis_geo` directly.

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
   from PDOK on first request and kept on disk; 404 outside coverage. Road
   flattening server-side came later, as a pipeline step rather than on request:
   the `terrain` dataset grades every tile it cuts (`pipeline/terrain_grade.rs`)
   into `terrain-graded-v1/`, served with `&graded=1`, and writes the deck
   heights it measured to `decks/14/x/y.json`
   (`level-of-detail-tile-tree.md`, "Atlas grades"). It reads the vector tiles
   from `ATLAS_VECTOR_TILES` (default `tiles.cartopolis.org`), the water dataset's
   own output from the site tree — so run `water` before `terrain` — and the
   surface model for the decks, from AHN6 where it is published and PDOK's AHN4
   elsewhere (`ahn6.rs`, `ahn-editions.md`). Each deck record is `[x, y, height,
   top, profile]` — the profile is `DeckProfile::to_wire`, trailing so an older
   client still reads the height. **Built** (`crates/atlas`, format
   `cartopolis_geo::terrain_tile`): z8–z17, 129² samples, 33 KB a tile. Against
   live PDOK from the VPS (2026-09-23): a first cut takes 0.2–0.3 s and a repeat
   is ~1 ms from disk. North Groningen (`14/8490/5318`) spans −1.0 to +10.6 m
   NAP. A tile over the Markermeer is a remembered 404, not a flat tile.
2. Port the TypeScript pipelines. **Done 2026-09-23, all at once rather than one at
   a time**, because the goal became retiring that repository altogether. All six datasets
   (`crates/atlas/src/pipeline/`), each diffed against the TypeScript output for
   the same square (`where-the-map-data-is-built.md` has the results), and
   the TypeScript server's surface route for route: the one-job runner (`jobs.rs`), the
   admin check against the simulator (`auth.rs`), coverage (`coverage.rs`), the
   autopilot control file (`autopilot.rs`), all under `/api/pipeline/*`
   (`api.rs`). The admin GUI moved into atlas unchanged except for the log
   cursor (below) and its address, and is now **the atlas web UI**
   (`crates/atlas/web`): static files behind Caddy at the site root of
   cartopolis.org, published by `deploy-web.yml`. It is the front page (which
   atlas served from a string until 2026-09-24), the operator's pages at
   `/admin` (they were `/console/`, which redirects) and the players' pages
   (`/register`, `/verify`, `/house`), so the simulator's
   `CARTO_PUBLIC_BASE_URL` is just `https://cartopolis.org`.
   web.cartopolis.org, its old hostname, was dropped rather than redirected —
   one site, not two.
3. Move accounts once the token design is agreed. The in-world player actions
   (block, report, delete my account) stay on the wire, with the simulator
   passing them on.

## The pipeline surface, as ported

- **A dataset is a blocking function over a `Ctx`**, and the runner gives the one
  running job a plain thread of its own rather than tokio's blocking pool. The
  `Ctx` is the only way a dataset reaches the world: log, progress, a cancel
  checked in every paging loop, and atomic tmp-and-rename writes into a tree
  Caddy serves live.
- **A panicking dataset is a failed step, not a dead runner** (`catch_unwind`).
  There is one runner thread and every later job would otherwise queue behind it
  forever. The TypeScript server it replaced had no equivalent failure to guard.
- **The log cursor is lines ever written, not lines held.** Each job keeps the
  last 2,000 lines. The web UI used to ask for `since = lines it held`, and its
  server read that as an index into the buffer, so once a long BAG run filled the
  buffer the followed log stopped updating. The web UI now keeps the server's
  `total` as its cursor.
- **Source answers are kept a month** in `<data_dir>/source-cache/`
  (`pipeline/http.rs`), so a re-run after a pipeline change does not page the BGT
  and download the 3D basisvoorziening again: over a 1 km square the BGT dataset
  went from 14 s to 3 s with identical output (2026-09-24), and a whole city's
  run is dominated by that paging. Every run first deletes what has expired, so
  the cache holds at most a month of what runs actually fetched — Groningen's
  3D sheets alone are ~550 MB. `atlas run --fresh` and the web UI's *Fetch
  fresh source data* ask everything anew. A 200 whose body does not parse is
  evicted rather than served back.
- **It writes as `cartoatlas` in group `ubuntu`, umask 0002**
  (`deploy/cartopolis-atlas.service`). The site tree and the autopilot's control
  file belong to `ubuntu`, and the autopilot writes that file too, so anything
  atlas creates must stay writable by the group.

## Contracts a port must keep

Client code already depends on these shapes, and a port that changes them breaks
it silently (reported by the tunnel/bridge work, 2026-09-23):

- `lod22/{14}/{x}/{y}` cell files and `bag/{14}/{x}/{y}.json`: `identificatie`,
  `status` and the footprint are read per building, and `lod22` also reads the
  cell's street tile on the worker to skip buildings standing in a covered
  tunnel's trench.
- The z14 `streets` layer is **not** a pipeline product. It is the VersaTiles
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
