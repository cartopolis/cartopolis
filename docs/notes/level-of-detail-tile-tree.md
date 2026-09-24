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
terrain tile endpoint (`atlas.md`). The client flattens it to the roads on a
worker. It cannot do that one tile at a time (step 2 below says why), so heights
are *fetched* per tile and *graded* over a window wider than any one tile.

**One scheduler,** ordered by screen importance, cancelling tiles that leave the
view, within the existing byte and upload budgets, evicting the least recently
seen.

## Steps

1. Screen-driven selection for the flat image tiles only. **Built**
   (`map_stream::select_tiles`), with the old ring count as its ceiling (424
   desktop, 224 phone) so no view can ask for more than the rings did. Over
   Groningen at 600 m, pitched 35° down: **375 → 213 tiles, 1.02 → 0.80 GB of
   graphics memory**, the same picture (2026-09-23, `--dump-state`).
2. Per-tile terrain from `cartopolis_atlas`'s `/terrain/{z}/{x}/{y}` (`atlas.md`,
   its first step), which retires the global field and the ring retears. Mapped
   2026-09-23 before starting, and it is four steps, not one, because two things
   the first design assumed are false:

   - **Road grading does not fit in a tile.** `flatten_pinned` spreads ~75 m,
     a bridge approach ramps ~120 m at 6 %, and both run over the whole 6.4 km
     grid (`cartopolis_geo::road_grade`). An MVT tile's buffer is ~23 m at z14,
     so grading each tile from its own roads puts a step at every tile edge a
     road crosses.
   - **A cold fetch gets slower, not faster.** Today's field is one ~12 MB WCS
     request (~4.5 s). The same 3.125 m over the same box is ~324 z16 tiles,
     and atlas cuts each from PDOK on first request, three at a time — ~25 s
     for an area nobody has visited. What tiles win is *reuse*: a tile's key
     does not depend on the map anchor, so a re-anchor or a second visit hits
     the device cache, where today's patch (keyed on an anchor-shaped box)
     misses.

   So:

   - **2a. Atlas pre-cuts an area.** **Built.** A `terrain` dataset in the
     pipeline cuts every tile over an area into atlas's store, like the other
     extractors, so a covered city is never cut on a player's request, and
     writes `terrain/coverage.json`. Atlas serves the tiles **unfilled** (holes
     as `NaN`, which the format already carries — 42–69 % of a city-centre
     tile), because the client must carve the water *before* it fills, and a
     pre-filled tile has already smeared the banks into the water.
   - **2b. The field is assembled from tiles.** **Built** (`terrain_tiles`),
     where the coverage manifest says atlas has cut the area, at the desktop
     grid only; WCS everywhere else and as the fallback when a tile fails.
     Nothing downstream changes: `terrain::bake` reads a `GroundSource`, which
     is either the WCS patch or the tiles.

     **The tiles are z14 at 513² (`terrain_tile::DENSE_GRID`), not z16 at
     129².** The same sample points, but ~30 requests a field instead of ~360.
     The z16 version was built first and measured at ~3 tiles a second on the
     container: each small request queues for a native pool thread behind the
     rest of the streaming, and the field had not arrived when the capture
     fired. Groningen is 42 dense tiles, 22 MB, cut in 52 s.

     Measured 2026-09-23 over the Grote Markt at 600 m, both runs against the
     same live sources: the 30 tiles arrived 1–11 s after startup, and the
     field matches WCS to 2 cm at the anchor, with relief 39.30 m against
     39.85 m (the extremes differ by 0.6 m because the two sample the survey on
     different lattices). **Neither run was waiting on the fetch**: both baked
     ~55 s in, held by the surface ring (`RING_HOLD_MAX_S` on a software
     renderer). So what 2b buys is not the first bake but the others — the old
     patch was dropped after each bake and a re-bake downloaded the 12 MB
     again — and every revisit. The wall-clock effect on a real GPU is still
     to be measured.
   - **2c. Retear per cell, not per ring.** One global `revision` makes ~10
     layers rebuild their whole ring on every bake. Each cell instead keys its
     rebuild on the heights *under it*, so a bake that changes nothing under a
     cell does not rebuild it, and the first build waits for its ground instead
     of building flat and tearing.
   - **2d. Grade in windows.** Grading moves from one global bake to a job per
     z14 cell over the cell plus a 150 m margin (the grading reach), fed with
     the road triangles of every loaded cell the window touches. A cell is
     graded only once its neighbours' roads are known, so two cells always
     grade their shared margin from the same roads and agree at the seam. That
     retires the global `RoadMask`, the settle timers and the ring hold.
3. Fold the globe into the tree, which retires the 50 km switch and the re-anchor.
4. Building levels of detail per tile. The LoD1.2 blocks already exist per z14
   cell; any coarser merged level is built by atlas, which runs the building
   pipeline (`crates/atlas/src/pipeline/`).
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
