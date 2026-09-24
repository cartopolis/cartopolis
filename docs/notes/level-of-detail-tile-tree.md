---
topic: level-of-detail-tile-tree
triggers: [LOAD_RADIUS, COARSE_RADIUS, MIN_ZOOM, CELL_RADIUS, LOD22_RADIUS, FAR_CELL_RADIUS, TERRAIN_HALF_M, GLOBE_SHOW_ALT, REANCHOR_DISTANCE, "screen-space error", "tile tree", "level of detail", "LOD", "Google Earth", "ring retear", "loads slowly", "time to first image"]
updated: 2026-09-24
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
   - **2c. Retear per cell, not per ring.** **Partly built** (2026-09-24): the
     ground cells record the field revision they stand on
     (`MapSurfaces::built_rev`) and a new field retires only the older ones, and
     over graded ground (2d) the first build waits for its ground instead of
     building flat — the cells fetch meanwhile and are handed the field when it
     lands (`TerrainField::ground_later`). Still whole-ring: the building cells
     (`lod22`), which hold their dispatch rather than their meshing.
   - **2d. Grade on atlas.** **Built** (2026-09-24), in place of grading in
     windows on the client. See "Atlas grades" below.
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

## What the load was actually waiting on (Deck, 2026-09-24)

Measured on `deck-1` over the Grote Markt, 1600×900, `--wait 180` and the
wall-clock time from the first log line to the capture. **Pass `--wait`**: the
default is 15 s, and a first round of this A/B captured every run at 15 s with the
map still loading, which looks like a result and is not one.

| | main | this branch |
|---|---|---|
| 600 m, 35° down, cold | 155 s | **67 s** |
| 600 m, warm | 125 s | **40 s** |
| 3 km, 60° down, cold | 120 s | **52 s** |
| 3 km, warm | 145 s | **48 s** |

The steps above were not what moved it. With them alone the branch settled in
the same ~130 s as `main`: the field arrived 25–30 s in instead of 36–116 s, and
nothing else changed. **The long pole was the height service's queue**, which
`--dump-state` shows as `height_patches` (~600 on `main`) and `worker_util` (0.2 —
the workers were idle 80 % of the time):

- **One request per object, one at a time.** Each bridge deck asked for its own
  surface and ground patch, each cell waited for one deck's answer before asking
  about the next, and every named dike asked for its own crest box — 446 of 553
  requests. They now share patches per ~120–256 m cluster
  (`map_geometry::cluster_decks`, `dikes::crest_clusters`).
- **One first-come queue for all of it**, three in flight and 0.15 s apart, so the
  terrain field's patch waited behind a couple of hundred crest boxes that only
  feed a label. The field and the decks now go first
  (`HeightLayer::patch_ranked`); that alone was 101 s → 67 s cold.
- Lifting the throttle instead (8 in flight, no spacing) took `main` from 155 s
  to 72 s, which is what pointed here. It was not kept: PDOK is a shared public
  service, and fewer requests is the same win without the load.

**Then the rebuilds.** With the queue fixed, every ground and building cell was
still built three times: flat, after a first bake that gave up waiting for the
ring at `RING_HOLD_MAX_S` = 10 s with part of the bridge decks unpublished, and
after the second bake their arrival caused. A 30 s hold gives a stationary view
one bake: 600 m cold 67 → 50 s, 3 km cold 52 → 48 s, 3 km warm 48 → 41 s.

Two things the A/B also caught. The branch's `bake_owed` never cleared when an
input moved and then moved back (the deck digest does, while a ring republishes),
so no capture on it settled at all until that was fixed. And one warm run lost
the 12 MB field patch to two PDOK timeouts, after which `HeightLayer` never asks
again: `main` settles with flat ground there, this branch reports the field as
owed. Atlas tiles (2b) are the fix for that, being thirty small requests instead
of one large one.

## What was still slow once the queue was fixed (Deck, 2026-09-24, later)

Same view and method, release build. A capture now logs three things that make
this readable without guessing: `streamer quiet … wall_s` as each layer finishes
(wall clock, unlike `settled_in`), `fetch route` per host and first path segment,
and `cache reads` per cache-key kind.

| | morning | now |
|---|---|---|
| 600 m, cold | 39 s (map image 11.7 s) | **32 s** (map image 8.6 s) |
| 600 m, warm | 30 s | **15 s** (map image 3.8 s) |

The device cache was already doing its job for everything drawn: on a warm start
all 213 finished basemap tiles, all 145 vector tiles, the buildings, the BGT, the
water and the dikes were read back, 0 misses. Four things were not:

- **The height patches were never kept on the device.** A warm start re-asked
  PDOK for ~170 deck and crest boxes (15 MB, 25 s of requests), and the surface
  ring waited 13.5 s on the decks before the terrain could bake. They are kept
  now, fetched on whole-metre boxes so a launch asks for the same file as the
  last, and read *before* the spaced queue, which would otherwise make 170 disk
  reads take as long as 170 requests. The cache fills over several launches:
  the dike crests are the lowest rank, and a capture ends before all are asked.
- **A cold start never used atlas's terrain tiles.** The anchor landed before
  the coverage manifest and latched the WCS patch. The field now waits for the
  manifest's answer, up to 5 s.
- **An early bake meant two bakes.** With the decks off the disk the ring
  finished in 3 s, and the bake ran with 20 of 50 water levels in; the rest
  cost a second bake and a second rebuild of everything. The bake now also
  holds while the levels are loading. The half-metre tier had the same fault
  worse: it re-baked its box for every level that landed, 28 times and 143 s of
  worker time in one start, because its patch now came off the disk.
- **Four io threads on an 8-core machine.** Network calls block their thread
  and the cache is read on the same threads; a cold fill held them for ~117 s
  inside ~40 s. Sixteen now.

What is left is structural, and it is step 2c/2d. On a warm start the ring is
drawn flat at ~2 s, baked at ~8 s and then **rebuilt**, surfaces and buildings,
until ~14 s. Laying a surface cell onto the field is 1.1–1.8 s of a worker, of
which 0.8–1.5 s is `refine_for_terrain` (ground lookups ~10 % of that; the
default hasher was a third, and is replaced). Cold, the long pole is still the
~185 PDOK deck and crest requests the ring waits for.

## Atlas grades (2026-09-24)

The client's field bake carved the water, filled the holes, flattened the roads
and ramped the bridge approaches — and could not start until every ground cell
around the camera had been meshed (the road shapes come out of that) and every
deck measured against PDOK. It was the whole of the cold start's critical path.
Atlas now does it once per city with the same code (`cartopolis_geo::terrain_grade`,
which the client's own bake also calls where atlas has not graded), and serves
the result: `/terrain/14/x/y?grid=513&graded=1&v=<stamp>`, listed in
`terrain/coverage.json` as `graded` with the run's `graded_stamp`. It also
publishes the deck heights it measured (`decks/14/x/y.json`), which the ground
cells use instead of asking PDOK.

**Heights only.** Atlas reads the roads, decks and water to decide where the
ground goes, and sends nothing but heights. Every road, bridge and building still
reaches the client as vector data, so selecting a building, the route line and
the labels are unchanged. Nor is this the rejected image-tile set: it is the same
~0.5 MB height tile atlas already stored, graded — Groningen is 49 tiles, 25 MB,
graded in 43 s on the Deck including measuring 149 decks. The Netherlands would
be ~19,000 tiles (an estimate), and nowhere else has the survey.

Each tile is graded over itself plus 350 m of its neighbours, on its own sample
lattice, and keeps its own 513²; neighbours agree along their shared edge to
≤ 1 cm (`graded_store_report`, measured over the Groningen run). One knowing
difference from the client: it grades under every footpath, including the ones
the client leaves to the BGT's surveyed paving.

Measured on `deck-1` over the Grote Markt at 600 m, release build, the same
binary against the same live data with the manifest listing the graded tiles or
not, time until everything has loaded:

| | client grades | atlas grades |
|---|---|---|
| cold | 31.6 s | **13.6 s** |
| warm | 14.4 s | **7.4 s** |

Cold PDOK requests fell 183 → 61 (the rest are dike crests, which only feed a
label, and the half-metre tier). The map image is complete at ~2.4 s warm and
~9 s cold. The picture is the same; relief differs by 2.4 m at the box's extremes
and the client's approach counters read 0, because that grading happens on atlas.

## Rejected

- **A world raster tile set on the server.** The server serves the vector map
  only; a planet of rendered imagery is not something this project can host.
- **Grading on the client, in windows** (the first plan for step 2d). Rejected
  2026-09-24 for atlas grading: its reason — that the server would need a second
  copy of the road code in TypeScript — went away when the pipelines became Rust
  in this workspace, and grading on every device for every visit is the cost the
  cold start was paying.
- **Dropping the vector ground meshes outright.** Up close they carry the water
  shader, sharp kerbs and tunnel openings. Whether images are good enough at
  walking height is step 5's question, answered with captures, not assumed.
