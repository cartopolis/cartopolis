---
topic: retired/regions
triggers: ["region", "region_id", "RegionDto", "RegionCapacity", "ObserverUpdate", "AoI", "area of interest", "estate"]
updated: 2026-09-11
---

# Retired: regions

A region was a 256 m square of ground with a raster z17 tile painted on it,
spawned from a row in `regions` and drawn by `rendering::spawn_regions`. It came
from before this client streamed a map — it *was* the ground. Retired
2026-09-11, protocol 23, schema V17. Do not re-propose it: an object's place is
a `GeoAnchor`, and the ground is the map.

## What it was still holding, and where each went

**Placement.** Overtaken by ADR-029 in 2026: a prim's horizontal position has
been an absolute lat/lng since then, and `position.x/z` is a client-frame echo.
The region contributed nothing but a `region_id` column.

**The ground plane.** The streamed OSM/3DBAG map. The region quad had already
been reduced to a special case that hid itself as soon as the anchor left its own
tile (`data_layers`' `anchor_tile`), because it does not follow a re-anchor and
otherwise appeared as a patch of the *old* city sitting where the camera landed.

**Capacity** (`max_prims`, `max_scripts`, schema V7). World-wide, in
`cartopolis.toml`. The stated reason for per-region was that a region is the unit
an operator hands out — no simulator ever ran more than one, so the ceiling was
always the server's. `RegionCapacity` → `WorldCapacity`; `GET
/api/admin/regions` → `GET /api/admin/capacity`.

**Script channels.** `vfSend` now reaches every listening script on the server.
The property lost is real and was deliberate — a channel scoped to a region meant
one estate's button could not work its neighbour's door — but with regions gone
there are no estates to divide. If it is wanted back, the way is a **radius**
around the sender's `GeoAnchor`, measured the way `vfSensor` already measures;
not a new container.

**Area of interest** (ADR-012). Removed outright, and this is the part worth
knowing. The filter excluded whole regions by the distance from `SimWorld`'s
`observer` field to a synthetic 300 m grid origin. But `observer` is *one field
on a world shared by every connection*, written by whichever client last sent an
`ObserverUpdate`, while `snapshot()` is broadcast to all — so what it did in
practice was hide every prim from *everybody* as soon as one client walked 500 m
from a grid that no longer corresponded to anything on the map. That is how it
was found: a signed-in player right-clicked the ground and the "Create object…"
entry was not there, because the client had been sent no region to put a prim in.

A real AoI filter has to be computed per connection, which means the snapshot
stops being a broadcast. That is a piece of work, not a line; until it is done
the sim sends every prim to every client. `ObserverUpdate` (message kind 5) went
with it, and **5 stays unassigned** — a stale client sending one should be
rejected, not misread as whatever took the slot.

## The offline avatar went in the same change

Not because it was a region, but because it was the same kind of leftover: an
avatar spawned at startup and never despawned, so `--offline` put a body on the
map with no sim behind it — nothing owned its position, nobody else could see
it, and the Follow camera that is its whole point was already refused
(`free_camera::follow_available`). Offline is the streamed map and a camera.
`lib::sync_local_avatar` spawns it with a session and `session::teardown` takes
it away. The one exception is the shot harness, which is always offline and has
to photograph the avatar: `ShotHarness::wants_avatar` reads what each shot
already says it is of (`--shot-mode avatar`, `--ui sit`, `--ui driver`) rather
than taking a flag, so a capture cannot ask for an avatar without saying what it
wants one for.
