---
topic: near-ground-layers-at-altitude
triggers: [map_geometry.rs, street_furniture.rs, vegetation.rs, globe.rs, traffic.rs,
           tall_structures.rs, stream_ledger.rs, quality.rs, FURNITURE_BUILD_ON_M,
           TREES_BUILD_ON_M, near_ground, update_globe_visibility, HideInSpace,
           pending_meshes, upload_pending, TurbineRotor, "flying higher", altitude,
           residency, "browser rung", SWEEP_RATE, ClimbRate, CARTO_NO_SWEEP_HOLD,
           "zoom in", "zooming in", "descent", RING_RETEARS]
updated: 2026-10-20
---

# What a climb holds, and why it used to hold the street

Reported 2026-10-17 on a Mac: the web build "crashes when flying higher", with
the halt card after both automatic reloads. The page's own error text was not
captured, so the cause of that loss is **not established**
([web-device-loss.md](web-device-loss.md) has what the card says now). What was
measured is what the client asked of the GPU on the way up.

## Method

Native client on an M5 Pro with the browser's own rung and the web's device
limits: `CARTO_QUALITY=browser CARTO_WEB_LIMITS=1`, `--shot-mode earth`,
`--look=-55,0`, over Groningen (53.2194, 6.5665), warm cache, 1600×900.
`gpu_resident_kb`, `upload_peak_kb` and `draws` from `--dump-state`; the split by
layer came from a throwaway census of live `Mesh3d` entities grouped by
material type, deleted afterwards. These columns count bytes and entities and
are machine-independent; no timing here is evidence.

| | 1.5 km before | 1.5 km after | 33 km before | 33 km after |
|---|---|---|---|---|
| streamed content resident | 1,033 MB | 481 MB | 1,021 MB | 399 MB |
| worst single frame of uploads | 93–214 MB | 15.8–17.4 MB | | |
| meshes on screen (`draws`) | 687 | 170 | 1,779 | 1,130 |

The upload peak varies run to run, hence ranges (three runs before, two after;
the part above the 16 MB ceiling is assets that do not go through the ledger).
`draws` counts visible mesh entities, not draw calls.

## What the gigabyte was

- **Street furniture: 531 MB in 166 meshes** — lampposts, bins, benches, the
  surveyed garden boundaries, parked cars, crossings, road marks; about 11 M
  vertices. Its fade hides it from 450 m, and it was built at every altitude.
  Now built only below `FURNITURE_BUILD_ON_M` (= the fade end) and dropped above
  500 m, on the flat ground's own edge so a climb retears the ring once. Trees
  likewise below their fade end (70 MB). Descending retore at 2 km, 450 m *and*
  250 m — the price, until the hold below.
- **And it was drawn, from 450 m to the space tier.** `update_globe_visibility`
  writes `Inherited` to every `HideInSpace` entity every frame below 50 km; the
  furniture and street-detail fades write `Hidden` only when their band flips,
  so the globe always won. Same for traffic, whose unplaced agents it made
  visible at the origin. The exclusions are in that query now, with a comment —
  **any layer that owns its visibility needs one.**
- **Traffic tested `cam_tf.translation.y`**, the height inside a 10 km grid cell,
  so above the first cell 400 agents came back on, simulated and drawn.
- **612 turbine rotors were 612 meshes** (the ±30 km tall-structure ring), 490 on
  screen from 33 km. One shared mesh per shape now, sized and phased by its
  `Transform`, so one batch; off-screen rotors no longer spin.

What remains at 33 km is 3DBAG buildings (262 MB, 51 meshes) and OSM buildings
(46 MB). From 15 km the LoD2.2 cells still texture the city in a capture, so they
were left alone — that is a judgement about the picture, not a measurement.

## Uploads: a cell was one admission

The browser rung now has a 16 MB per-frame ceiling (`Budget::browser`), and on
its own that held at 1.5 km — but **street level still put 173 MB in a frame**.
`map_geometry` priced a finished cell as one item, and `stream_ledger` always
passes a frame's first item whole; at 40 m a cell (flat ground refined to the
terrain, furniture, trees) is ~170 MB. Cells now land with their meshes on
reserved handles and the data goes in piece by piece as the ledger admits it
(`MapSurfaces::upload_pending`); the next cell lands only when the queue is
empty, and a cell counts as complete ground — hiding the basemap under it — only
once its last mesh is in. At 40 m: worst frame 173 MB → 26.9 MB (the rest of the
frame is assets that do not go through the ledger), settle 10.1 s → 16.6 s.
Without a ceiling the queue drains in the frame the cell landed, as before.

**Street level is still 1.65 GB resident on the browser rung**, against its
700 MB `resident_bytes_target` — `load_control` cannot narrow `map_geometry`
below its radius of 1, and furniture is about a third of it. That is the next
place to look, not a solved problem.

## A zoom holds the ring until it slows (2026-10-20)

A descent from 40 km crosses all three bands, and each retear re-ran every
cell's job — road cut and all — while the orphans of the band before kept
running: a job past its download is never interrupted, so the cells that
replaced them queued behind work whose result would be thrown away. None of it
could land before the camera was past the band that asked for it.

Now, while the altitude changes faster than `SWEEP_RATE` (0.3 e-folds a second,
about ×1.35/s; a smoothed rate, `ClimbRate`), the ring keeps the detail it has
and starts no cells, and a torn-down job stops at its next stage boundary
(`torn()` in the surface worker). A slow zoom still gains detail band by band.
A one-frame jump of more than ×2 is a teleport, not a zoom, and is not held.

Measured with `cargo perf --only descent` on `deck-1` (web-release, 1280×800,
warm cache, Groningen 40 km → 150 m in 8 s; `CARTO_NO_SWEEP_HOLD=1` is the
before, from the same binary), two runs each:

| | before | after |
|---|---|---|
| ground ring retears during the zoom | 3 (27 cells) | 0 |
| everything quiet, seconds after arrival | 50.9 / 51.0 | 25.1 / 27.5 |
| worker time after arrival | 212 / 211 s | 87 / 97 s |
| worst frame during the zoom | 185 / 202 ms | 83 / 93 ms |

At arrival and at +5 s the two look the same — basemap image, LoD2.2 shells
(already in before the camera arrived, in both), trees; what moved is when the
street surface lands. The one retear left is at arrival, of the cells built at
the zoom's starting altitude.
