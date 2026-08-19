---
topic: unused-capacity
triggers: ["what should we build", "ideas", "roadmap", "unexplored", routing.rs, soundscape.rs, weather.rs, terrain.rs, lod22.rs, place_link.rs, "place_labels", "isochrone", "shadow study"]
updated: 2026-08-19
---

# Data and systems already in memory that nothing reads

A companion to [vector-tile-unused-layers.md](vector-tile-unused-layers.md),
which asks the same question of one MVT body. Surveyed 2026-08-19 by reading the
systems tree, the manual and the open board.

The *facts* below are the note: each is a capability the client already pays for
— a fetch it already makes, a graph it already builds, geometry it already
uploads — with no consumer. **The directions under them are not plans of
record.** Per this directory's own rule, a proposal lives in an issue; these are
parked here so the survey does not have to be redone before someone files one.

## The wind has exactly one consumer

`WeatherState`'s `wind_ms` reaches `water.wgsl`'s sea state and stops there.
Vegetation does not sway, rain falls vertically, and `soundscape`'s wind loop is
a constant regardless of the forecast driving the sea a hundred metres away.
Nothing about the plumbing prevents it — the value is a resource on the main
world.

→ One wind, read by the trees, the rain slant and the wind gain.

## `RoadGraph` answers point-to-point and nothing else

`routing::route` snaps both ends into `routable_core` and runs a search. The same
graph, the same core and the same ground-ribbon painting would answer
*reachability* — a search with no target — per mode, with no extra request and no
new data source. Nothing in the tree computes an isochrone or a catchment.

→ "How far in 15 minutes", walkable and drawn on the ground you can then walk.

## 3DBAG roof planes are rendered and never measured

`lod22` streams real triangulated roof geometry with each triangle tagged
roof/wall, and the only thing that reads the tag is the material. The sun is
physically positioned, the terrain under the building is measured, and the
shadow cascades exist — so orientation, pitch and occlusion are all present at
once and no code asks them anything.

→ A time scrub ("is this spot in sun at 4pm in January") and a per-roof
insolation tint. The tint is a pure function of triangle normal, latitude and
date, so it belongs in `cartopolis_geo` and is testable with no GPU.

## The world loses its names above the street

`street_labels` writes names into the road surface up to its top height band;
above that, through the whole climb to orbit, nothing is labelled. The
`place_labels` layer (`kind`, `name`, `population`, 21 features on the Groningen
fixture) is decoded-never-read — see the companion note.

## Every cell already knows what would make a sound

`soundscape` synthesises footsteps and wind. Per cell, in memory, unread by it:
the water shore distance field (`vector_tiles::water_shore_field`), road classes
and their geometry, and the `pois` kind histogram — `playground`×40,
`fountain`×2, `bench`×129. Sounds here are synthesised into WAV bytes at startup
rather than shipped (`.gitattributes` would make a shipped clip an LFS object
with the documented silent failure), and the synthesis is pure, so a city
soundscape costs no assets and stays testable on a host with no audio device.

## A shared camera path exists, but only as a build tool

`shots.toml` plus `--frames` is a keyframed camera path with log-interpolated
altitude, and the dev panel's **📋 Copy as preset** already puts one viewpoint on
the clipboard. `place_link` already carries one camera through a URL to a phone
or the web. A recorded, shareable tour is those two meeting; neither was built
with the other in mind.

## Terrain is Netherlands-only by source, not by design

`TerrainField`'s contract — returns 0 when the layer is off, outside coverage or
still fetching, and tapers to zero at its own boundary — is source-agnostic
already (see [terrain-relief.md](terrain-relief.md)). Terrarium and Copernicus
DEM tiles are free, global and tile-shaped, i.e. the streaming shape this client
is built out of. The stated degradation ("the ground goes flat outside NL") is a
data gap, not a structural one.

## The night hemisphere is unlit

The globe's ocean mask and `GlobeSurfaceAlbedo` both ramp on `space_factor` and
share the tile imagery's channels; the night side has no equivalent and renders
black, while the flat tier below it has window glow. VIIRS night lights are
public domain and the same shape as the mask that is already there.

## Places are private, on a server built for sharing them

Bookmarks and pins live in `user.json`. The sim has accounts, replication, and
the full report/block/moderation surface described in
[user-safety.md](user-safety.md) — which is the expensive half of publishing a
pin, and it is already built.

## Two more, small

- **No ruler.** Distance, bearing, area and building height are all derivable
  from picking plus the terrain field, in a world whose whole premise is
  measurement.
- **Opening hours.** `WorldClock` learns the real local offset from the OVapi
  feed; Overpass carries `opening_hours` on the shops whose markers are already
  drawn. Nothing connects them.

## Rejected while surveying

**Sim-driven NPC traffic or pedestrians.** The simulator holds no map data and
[the terrain note](terrain-relief.md) says it never will — "the sim stays flat" —
so authoritative agents walking the routing graph would move the graph server-side
and give the sim a map. A client-side ambient traffic layer avoids that and fails
the other test: it is invented geography in a project whose first claim is that
none of it is invented. Vegetation scatter is the existing exception and is worth
not widening.
