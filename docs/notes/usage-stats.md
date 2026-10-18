---
topic: usage-stats
triggers: [crates/simulator/src/stats, stats.db, migrations_stats, "/api/stats/hit", "/api/admin/stats", "/api/communities/{id}/stats", stats_view, stats_positions, WsQuery, docs/mockups/stats]
updated: 2026-10-18
---

# Usage statistics count what the server already sees, and keep counts

`crate::stats` counts how the world is used and who visits the sites, for
Admin › Stats and a community's moderators (`docs/mockups/stats/`, decided
2026-10-18). The rules the design rests on — break one and the page stops
being cookie-free and consent-free:

- **No IP address, cookie or browser id is ever stored.** A web visitor is a
  hash of a secret made at midnight (UTC), the address, the browser and the
  site. The secret lives in memory only, so no visit can be joined to the next
  day's, and the address goes no further than the hash. A refused sign-in's
  "devices" use the same hash.
- **Nothing about fewer than 5 people is shown** (`MIN_PEOPLE`): a 250 m square
  on the map, a topic's message and writer counts, a cohort's return rates,
  "active members". A square is kept for a closed day only if 5 people stood
  in it that day; thinner rows are deleted when the day closes.
- **Per-account rows (sessions, community activity) are kept 90 days**
  (`KEEP_DAYS`), then only daily totals remain. Retention cohorts and the
  funnel's last steps need them; the user chose to keep them over dropping
  those numbers.
- **Message content is never counted beyond "one was relayed".** Chats and
  topics are sealed; the server sees a send and a group id.

## Where it is

`stats.db` beside `world.db`, its own file with its own migrations
(`crates/simulator/migrations_stats/`, numbered from V1 independently of the
world's — the two histories never collide). Kept apart because stats grow
every minute, need no backup with the world, and can be wiped alone. Its
migrations are exercised by `--create-admin`, which the deploy smoke test runs
on a copy of the live files.

Recording is a mutex and a hash insert, never the disk: everything is tallied
in memory and written once a minute (`stats::spawn`). **What the last minute
counted is lost on a stop**, and a restart mid-day makes that day's distinct
counts (visitors, people per square) a little low, because the in-memory sets
start over and the stored value is the larger of the two. Days are UTC; hour
rows are unix hours, for the page to draw in its reader's zone.

## Which app opened a session

From `/ws?client=android|desktop|web|chat&build=<commit>`. **Not a field in
`ClientHello`**: postcard is positional, so a field there is a protocol
revision, and every installed app older than it would be refused at sign-in.
An app that sends no parameter is `unknown`; so is plain TCP (local tooling).

## The pages

Admin › Stats (`crates/atlas/web/src/components/pages/stats/`) and a
community's Stats pane (`community-stats.tsx`) draw the answers; their types in
`types.ts` are checked against the live answers by the integration test
`stats::the_web_pages_types_match_what_the_server_sends`. The site counts a
page view on every change of address (`main.tsx`'s route resolvers), the
viewer once on load (`crates/cartopolis/index.html`, before the WebGPU gate).
Busy squares are named by our own `nominatim.cartopolis.org`, from the
operator's browser, only for the ranked lists (never per square on the map).
There is no raster base map: the tile server serves vector tiles only, and
loading map images from a third party would send operators' browsers
elsewhere.

## Cannot be counted, and why

- **Places shared in chats**: a `Point` travels inside the sealed message. Only
  public posts' places are visible, and those are what a community's page
  shows.
- **Building floor-plan saves** happen in atlas (`put_design`), which asks the
  simulator `GET /api/buildings/{pand}/access` for opening, saving and
  deleting alike, so the simulator cannot tell a save from a view.

## Counted from outside the simulator

- **about.cartopolis.org** (`jeroen/about-cartopolis`, branch `presentation`)
  sends `site: "about"` with `navigator.sendBeacon`, only when served from that
  host. A beacon posts `text/plain`, the one cross-site request a browser sends
  without a CORS preflight, so `/api/stats/hit` reads the body as JSON whatever
  its content type and answers no CORS at all.
- **Countries** come from DB-IP's free country table (CC BY 4.0: the Web
  visitors page carries the attribution), a CSV beside the database fetched
  monthly when `geoip_download` is on (`CARTO_GEOIP_DOWNLOAD=true`). The CSV
  rather than the MaxMind-format file because reading that needs a crate this
  tree does not have. Without the table every visitor is `unknown`.
- **APK downloads** come from Caddy, which serves the file itself: the
  `cartopolis.org` block logs only `/apk/cartopolis.apk`, deletes the address and
  headers from each line, and streams the lines over TCP (`output net`) to
  `apk_log_listen` on the docker gateway (`CARTO_APK_LOG_LISTEN=172.17.0.1:4750`).
  No log file exists; a line is counted and gone. Only a `GET` answered `200`
  counts, so a resumed download is one, and a download cannot be told from
  another by who made it — the address is gone before it arrives.

## Deploying

The public paths must be in the host Caddyfile's API matchers, or they answer
the web UI's `index.html` with a 200 (`hosted-simulator.md`):
`/api/stats/hit` and `/api/communities/*/stats`. `/api/admin/stats/*` is
already covered by `/api/admin/*`. The two settings above live in
`/etc/cartopolis-sim.env`.
