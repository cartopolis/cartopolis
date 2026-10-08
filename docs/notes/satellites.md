---
topic: satellites
triggers: [satellites.rs, satellites.wgsl, --satellites, CelesTrak, SGP4, sgp4, TLE, "element sets", "orbit", "in orbit", CARTO_SATELLITES_URL, CARTO_FOLLOW_SATELLITE, "/satellites/active.tle", satellites_source, "far plane", "geostationary"]
updated: 2026-10-08
---

# Satellites around the globe

The layer draws every satellite in CelesTrak's `active` group around the globe,
from space only. This note holds what the code cannot.

## The source, and why it goes through our server

- **CelesTrak, `gp.php?GROUP=active&FORMAT=tle`.** No key and no account, and it
  is the copy of the US Space Force catalogue that everybody uses. Measured
  2026-10-08: **15,944 satellites, 10,660 of them Starlink**, 2.7 MB as
  three-line text. The JSON form of the same query is about twice that size,
  which is why the text form is read.
- **CelesTrak asks clients not to download more often than every two hours,
  and enforces it per address.** Measured 2026-10-08: the third full download
  from the VPS within an hour answered `403`.
- **It goes down.** On 2026-10-08 every page on the site, its home page
  included, answered `500` for several minutes, then recovered.
- **It sends no CORS header**, so a browser cannot read it at all.
- So **atlas mirrors it** (`crates/atlas/src/satellites.rs`): it fetches the
  catalogue every two hours, keeps the last good copy on disk under
  `data_dir/satellites/`, and serves it at `/satellites/active.tle`. Every app
  and browser reads from there (`cartopolis.org/satellites/active.tle`, or the
  page's own origin on the web). CelesTrak sees one polite client, and the map
  keeps its satellites while CelesTrak is down. An answer holding fewer than
  1,000 element sets (an error page, a cut-off body) never replaces the copy,
  and a failed refresh waits an hour, because asking again after a refusal only
  extends it. The `satellites_source` key in atlas's configuration (empty turns
  refreshing off) and `CARTO_SATELLITES_URL` on the client are the overrides.
- A per-client proxy (`/up/celestrak/`, the way NOMADS and PDOK are reached) was
  the first plan and was dropped: Caddy's `reverse_proxy` does not cache, so it
  would have put every browser user behind the server's one address and been
  refused within minutes of real use.
- Space-Track has the same data but needs an account. A key-free JSON mirror
  (`tle.ivanstanojevic.me`) was tried on the same day and answered
  `508 Resource Limit Is Reached`. Neither is a fallback.

## Computing positions here, not fetching them

The positions are computed on the device from the published orbits, using SGP4,
the model those orbits are published for (the `sgp4` crate, pure Rust, so it
also builds for Android and the web). A service that serves live positions would
be one request per view per user. This way it is one 0.9 MB download (compressed)
every six hours per device, and the layer works when the source is down.

## Rejected

- **Rebuilding the mesh every frame.** About 16,000 quads at 48 bytes a vertex is
  2.5 MB per frame, which is the upload churn `upload-bursts.md` is about. The
  mesh is rebuilt every ten seconds instead, and the shader moves each point on
  under gravity in between. Over those ten seconds that drifts about 50 m from the
  exact answer for the space station (the frame's rotation, not gravity, is the
  error).
- **One entity per satellite.** 16,000 transforms written every frame, for
  points that have no other per-entity state.
- **Showing them from the ground.** The ground camera's far plane is 200 km and
  the nearest satellites are 400 km up. Drawing them from the ground would need
  a sky-shell treatment of their own, like the stars. Not done.
