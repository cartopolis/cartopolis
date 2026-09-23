---
topic: level-of-detail-tile-tree
triggers: [LOAD_RADIUS, COARSE_RADIUS, MIN_ZOOM, CELL_RADIUS, LOD22_RADIUS, FAR_CELL_RADIUS, TERRAIN_HALF_M, GLOBE_SHOW_ALT, REANCHOR_DISTANCE, "screen-space error", "tile tree", "level of detail", "LOD", "Google Earth", "ring retear", "loads slowly", "time to first image"]
updated: 2026-09-23
---

# The world streams as one screen-driven tile tree

Agreed 2026-09-23 as the direction for streaming, to get Google-Earth-like
responsiveness at every altitude. It is being built in steps (below); until the
last step lands, part of what this describes is the target, not the tree.

## Why the fixed rings had to go

Each layer streamed its own fixed-size ring on its own grid, whatever the camera
was looking at: 424 flat image tiles (9×9 at z17, then 7×7 per level down to z10),
3×3 z14 cells of ground meshes, 5×5 z14 cells of LoD2.2 buildings, a 6.4 km
terrain field around the map anchor, a separate globe quadtree above 50 km, and a
full re-anchor past 120 km. Measured cold over Groningen at 600 m on `deck-1`
(2026-09-23):

- the same 424 tiles and 25 building cells load looking straight down or at the
  horizon;
- the terrain field is baked into every ground vertex, so each bake rebuilt the
  whole ring: three bakes per cold fill, two after the hold for the surface ring;
- the basemap queued behind multi-second cell jobs and took **45 s** to finish.

## The design

**One tree** of Web Mercator tiles, z0 to about z19, placed on the globe at every
altitude. There is no flat tier, no globe tier and no re-anchor.

**Selection by screen-space error.** A tile is split while its projected error is
over ~2 px, and only when it is in the frustum and above the horizon. The tile
count stays roughly constant at every height. As a rough guide for a 1080 px,
60° view at 53°N, one pixel covers about 1/1000 of the viewing distance: z16
imagery is right at ~670 m, z14 at ~2.7 km.

**A parent is drawn until all four children are ready,** then cross-faded. No
holes and no half-drawn areas.

**What a tile carries, by distance:**

| distance | image | buildings | roads, water |
|---|---|---|---|
| over ~5 km | z12–14 | none (footprints in the image) | image |
| ~1.5–5 km | z14–15 | LoD1.2 blocks, merged per tile | image |
| ~0.3–1.5 km | z16–17 | LoD2.2 near, blocks further | image |
| under ~300 m | z17–19 | LoD2.2 | image, plus vector meshes near an avatar on the ground if the comparison in step 5 says they are worth it |

**Vector data stays the source of truth** for 3D, routing, the route line,
labels, places and collision. The images are drawn on the client from the vector
tile and kept in the device's cache (`tile_loader::rendered_cache_key`). They are
a per-device cache, never a server-side raster set.

**Terrain per tile.** Each tile carries its own height grid with skirts, from a
terrain tile endpoint (`atlas.md`). The client flattens it to the roads per tile,
on a worker. Neighbouring tiles agree at the seam because an MVT tile carries a
buffer past its edge.

**One scheduler,** ordered by screen importance, cancelling tiles that leave the
view, within the existing byte and upload budgets, evicting the least recently
seen.

## Steps

1. Screen-driven selection for the flat image tiles only.
2. Per-tile terrain, which retires the global field and the ring retears.
3. Fold the globe into the tree, which retires the 50 km switch and the re-anchor.
4. Building levels of detail per tile.
5. A switch that stops drawing flat features as meshes while keeping the data
   extraction; compare 600 m and walking height before deciding.

Each step is judged on time to first image and time to sharp at orbit, 3 km,
600 m and street level, cold and warm, **wall-clock on a real GPU**. The
container's lavapipe cannot measure load time: it is frame-bound at seconds per
frame, and `settled_in` counts virtual seconds, so it hides exactly the freezes
this is about.

## Rejected

- **A world raster tile set on the server.** The server serves the vector map
  only; a planet of rendered imagery is not something this project can host.
- **Baking road flattening into the terrain tiles on the server.** It needs the
  client's road shapes (`cartopolis_geo`) on the server, either as a second copy
  in TypeScript that drifts or as a new cross-repository dependency. The client
  does it per tile instead.
- **Dropping the vector ground meshes outright.** Up close they carry the water
  shader, sharp kerbs and tunnel openings. Whether images are good enough at
  walking height is step 5's question, answered with captures, not assumed.
