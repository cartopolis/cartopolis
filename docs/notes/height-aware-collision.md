---
topic: height-aware-collision
triggers: [collision.rs, Footprint, Body, BuildingColliders, segment_fraction, with_extent, "camera inside building", "collision on bridges", "flying through buildings", CAMERA_COLLIDE_MAX_Y, systems/player/avatar.rs, systems/net/network.rs, systems/player/free_camera.rs, systems/map/osm_buildings.rs, systems/map/lod22.rs, steer_velocity, eject_avatar_from_buildings, buildings_enabled, AIRBORNE_EPS]
updated: 2026-10-17
---

# Colliders have a vertical extent, and the avatar and camera are tested against it

Until 2026-09-28 a `Footprint` was a 2D polygon with no height. Everything that
followed from that was a workaround: movement, steering and the boom skipped
colliders whenever the avatar was "airborne" (`avatar::AIRBORNE_EPS`, since
removed; so flight went through every building), the follow camera's boom test
was gated below 15 m
(`CAMERA_COLLIDE_MAX_Y`, so it went inside anything taller than a house), and on
a bridge the deck counted as ground while the buildings beside and under it
still blocked and ejected the avatar.

Now a footprint carries `bottom..top` in the same frame as the avatar's feet
(metres above the anchor ground; `terrain.ground_at` includes bridge decks), and
a `Body { feet, ground }` says where the avatar is. A footprint blocks when
`feet < top - STEP && feet + HEIGHT > bottom` (1.8 m tall, 0.35 m step), so
there is no airborne special case: a flier over a roof is clear, a flier at wall
height is stopped, a bridge deck over a lower building is clear, and a kerb-sized
solid is stepped over.

- Sources set extents: `lod22` (base .. base + ridge; IFC wall hulls likewise),
  `osm_buildings` (base + `min_height` .. base + height, which also opens
  archways without the old `min_height < 2.2` filter), `street_detail`
  (ground .. ground + object height).
- Every footprint also blocks by *crossing a wall* between two positions
  (anti-tunnelling at flight speed, where a step is tens of metres).
- Online steering probes `max(0.6, speed * 0.12)` ahead, so it scales with the
  altitude speed factor.
- The follow camera does a pre-lerp boom test and a post-lerp sight-line resolve
  from the head to the real camera position, both through
  `segment_fraction(from, from_y, to, to_y)`, which is height-aware.

Known gap: descending onto a roof passes into the building and is ejected; the
sim is flat and cannot land the avatar on a roof, offline could (a `roof_under`
clamp) but is not done.

## Collision is client-side prediction, and acts on whichever channel owns movement

The sim is authoritative for the avatar's position and holds **no building
geometry** — the OSM and 3DBAG footprints stream only on the client, and the sim
holds no map data and never will. So the streamers publish into the client's
`BuildingColliders`, in the anchored world-XZ frame: `osm_buildings` the exact
rings of the near detail patch, `lod22` the BAG outline where the register has one
and otherwise the convex hull of each building's projected vertices (a 3DBAG cell
survives only as a triangle soup; `doors-and-rooms.md` has why the hull is the
fallback), per cell and cleared on a re-anchor. `AvatarState::position.xz` is the
avatar's absolute world-XZ in that same frame — the avatar sits near the origin
and the world re-anchors around it — so it compares with the footprints directly.

What collision then changes depends on the run mode, because it has to act on
whatever owns movement:

- **Online** it steers the *velocity wish* in `network::send_network_intent`
  (`BuildingColliders::steer_velocity`): it slides along walls, and an avatar
  already inside a footprint is given a push outward at the wish's speed so it
  auto-walks clear. A snap-eject cannot work here — the next snapshot overwrites
  the transform with where the sim put it.
- **Offline** the transform *is* the state, so `avatar::handle_avatar_movement`
  resolves the move directly (`slide`), and `collision::eject_avatar_from_buildings`
  is the snap-out backstop for a teleport into a building or one that streams in
  on top of the avatar. It returns early whenever an `OnlineSession` exists.

Both are gated on `DataLayers::buildings_enabled`, so collision matches what is
drawn: a building that is not on screen does not stop anybody.
