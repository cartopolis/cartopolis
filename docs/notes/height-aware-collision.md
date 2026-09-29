---
topic: height-aware-collision
triggers: [collision.rs, Footprint, Body, BuildingColliders, segment_fraction, with_extent, "camera inside building", "collision on bridges", "flying through buildings", CAMERA_COLLIDE_MAX_Y]
updated: 2026-09-28
---

# Colliders have a vertical extent, and the avatar and camera are tested against it

Until 2026-09-28 a `Footprint` was a 2D polygon with no height. Everything that
followed from that was a workaround: movement, steering and the boom skipped
colliders whenever the avatar was "airborne" (so flight went through every
building), the follow camera's boom test was gated below 15 m
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
