---
topic: satellites
triggers: [satellites.rs, satellites.wgsl, --satellites, CelesTrak, SGP4, sgp4, TLE, "element sets", "orbit", "in orbit", CARTO_SATELLITES_URL, "far plane", "geostationary"]
updated: 2026-10-08
---

# Satellites around the globe

The layer draws every satellite in CelesTrak's `active` group around the globe,
from space only. This note holds what the code cannot.

## The source

- **CelesTrak, `gp.php?GROUP=active&FORMAT=tle`.** No key and no account, and it
  is the copy of the US Space Force catalogue that everybody uses. Measured
  2026-10-08: **15,944 satellites, 10,660 of them Starlink**, 2.7 MB as
  three-line text. The JSON form of the same query is about twice that size,
  which is why the client reads text.
- **CelesTrak asks clients not to download more often than every two hours** and
  blocks addresses that do. The client caches the file for twelve hours
  (`CATALOGUE_TTL`) and keeps using the stale copy if a refresh fails. Twelve
  hours of drift is a few kilometres for a low orbit, which is under a pixel from
  space.
- **It enforces that limit per address, and it is strict.** Measured
  2026-10-08: the third full download of the `active` group from this VPS within
  an hour answered `403`. One phone is one address and fetches twice a day, so
  the native app is fine. **The web build is not.** Its `/up/celestrak/` proxy
  sends every browser user from the server's single address, and Caddy's
  `reverse_proxy` does not cache, so that proxy would be blocked within minutes
  of real use. The web build needs a server-side copy (refreshed every two
  hours, served as a static file) before it ships this layer.
- **It goes down.** On 2026-10-08 every page on the site, its home page
  included, answered `500` for several minutes, then recovered. The stale-cache
  fallback is there because of this, not just in theory.
- **The site sends no CORS header**, so the web build goes through its own
  origin at `/up/celestrak/` (`Trunk.toml` for `trunk serve`; production needs
  the matching `handle_path /up/celestrak/*` block in the host Caddyfile).
- Space-Track has the same data but needs an account per user. A key-free JSON
  mirror (`tle.ivanstanojevic.me`) was tried on the same day and answered
  `508 Resource Limit Is Reached`. So neither is a fallback.

## Computing positions here, not fetching them

The positions are computed on the device from the published orbits, using SGP4,
the model those orbits are published for (the `sgp4` crate, pure Rust, so it
also builds for Android and the web). A service that serves live positions would
be one request per view per user, all against somebody else's rate limit. This
way it is one download a day, and the layer keeps working when the source is
down.

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
