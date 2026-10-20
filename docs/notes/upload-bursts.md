---
topic: upload-bursts
triggers: [wgpu::meter, meter.rs, render_census.rs, firefox_probe.ts, gpu_log.ts, "mesh owners", MapSurface, "gpu memory", "GPU memory", device lost, DEVICE_LOST, TDR, command buffer timeout, upload, upload_meter, RenderAssetBytesPerFrame, upload_bytes_per_frame, MAX_CELL_MESH_BYTES, lod22, streaming burst, churn, "slow frame", peak_asset, resident, residency, gpu_resident_kb, handset_guarded, tile_px, out of memory, allocation, upload_meter.rs, stream_ledger.rs, quality.rs, lod22.rs, osm_buildings.rs, globe.rs, map_stream.rs, map_geometry.rs, DETAIL_FREEZE_ALT, get_mapped_range, CARTO_UPLOAD_KB, report_budget]
updated: 2026-10-20
---

# A per-frame cap on *items* is not a cap on bytes

## The first device it lost: an Intel iGPU

Mass mesh and texture churn hangs the Intel iGPU with `VK_ERROR_DEVICE_LOST`:
bursts of large uploads and despawns packed into a few frames overrun the
driver's command-buffer timeout and the device dies. After that every render
system panics in `Buffer::get_mapped_range` ("Buffer with '' label is invalid") —
**that panic is downstream noise, not the cause**; look for what was uploaded in
the frames before it.

Three item guards came out of it: the flat-tile build cap
(`map_stream::MAX_UPLOADS_PER_FRAME`), `globe::MAX_GLOBE_TILE_BUILDS_PER_FRAME`,
and the OSM detail patch's altitude freeze plus refetch cooldown
(`osm_buildings::DETAIL_FREEZE_ALT`, 3 km, and `DETAIL_REFETCH_COOLDOWN`, 2 s). The
last one has a measurement: a zoom-out dragged the patch's fetch centre sideways
and fired **13 full patch rebuilds (6,776 → 46 buildings) in 2.4 s**, and the
device was lost at 4.8 km — far below the space tier, so nothing about altitude
had protected it.

**The rule for every new streaming path: bound it per frame — in bytes, by asking
`stream_ledger::admit` before creating the frame's assets, not only in items — and
gate it on drift and altitude**, so a camera sweep cannot turn into a rebuild per
frame. The rest of this note is why the bytes half is not optional.

## A count is not the quantity

Every streamer in this client bounds how much it finalises per frame —
`map_stream::MAX_UPLOADS_PER_FRAME`, `globe::MAX_GLOBE_TILE_BUILDS_PER_FRAME`,
`lod22::MAX_BUILDS_PER_FRAME`, `osm_buildings::MAX_FAR_BUILDS_PER_FRAME`. All of
those bound a **count**. The quantity that decides whether a mobile driver's
command buffer survives the frame is bytes, and until 2026-08-18 nothing in this
process measured or bounded that.

The two are not proxies for each other, because the items differ in size by three
orders of magnitude: a 256² tile mip chain is ~350 kB and a merged LoD2.2 cell is
a whole z14 triangle soup. `lod22`'s cap was the strictest in the codebase — one
cell per frame — and it was also the path that handed the GPU 63 MB in one frame.

## The measurement

`systems::upload_meter` counts the bytes of `Image` and `Mesh` assets created or
modified per frame, by the same arithmetic bevy's own upload throttle uses
(`RenderAsset::byte_len`). A cold cache over Groningen at 1,678 m, phone tier,
2026-08-18:

| what | before | after |
|---|---|---|
| largest single asset queued | 64,892 kB | 7,919 kB |
| draws at the settled frame | 221 | 236 |
| triangles at the settled frame | 2,506 k | 2,506 k |
| worst frame (lavapipe, indicative) | 593–605 ms | 135–222 ms |

The nine LoD2.2 cells of the opening view queued 5.8, 5.5, 17, 17.4, 8.3, 24.7,
31.7, 14, 25.8 and 63.4 MB — one cell per frame, ~215 MB over about three and a
half seconds. That is the burst behind the Adreno 840 device loss of 2026-08-17
(`android-build.md`) and it has the same shape as the Intel iGPU loss above.

**Frame times on lavapipe are a property of the container**, not of the change;
they are quoted because the direction was consistent across paired runs, not as
evidence. The byte, draw and triangle columns are exact and machine-independent.

## Two bounds, and they do different jobs

**`lod22::MAX_CELL_MESH_BYTES` (8 MB) caps a single asset.** A cell's triangles
are cut into byte-bounded pieces by `MeshData::into_meshes`, each a self-contained
mesh with its own re-indexed vertices. This is not redundant with the budget
below: bevy's `RenderAssetBytesPerFrame` always writes at least one *whole* asset
per frame so uploads can never stall, so a 63 MB asset is a 63 MB frame whatever
the budget says. Splitting is what gives a throttle something to spread.

The cost is draw calls — 15 more for the whole opening view, each with its own
bind group, on the platform where Bevy issues them from the CPU. That is the knob
to turn if a device still hitches on a dense cell, and its price shows up in
`ShotMetrics::draws`.

**`quality::Budget::upload_bytes_per_frame` (4 MB, phone only) caps the frame.**
It bounds every asset path at once, including ones nobody has enumerated, and it
defers rather than drops: what does not fit is re-queued for the next frame, so a
low ceiling costs fill *latency* and never content. `CARTO_UPLOAD_KB` overrides it
(`0` = no ceiling), which on Android is `setprop debug.carto.upload_kb` — the
point being that whoever holds the device can A/B a number nobody can pick from a
desk.

## Bevy's throttle is off, because it drew the wrong geometry (2026-09-26)

The budget above was first enforced by bevy's `RenderAssetBytesPerFrame`, and on
the handset that is what broke the picture. In bevy 0.19 the throttle defers a
mesh's `RenderMesh` to a later frame, but `MeshAllocator` has **already written
that mesh's vertex and index data** in the frame it was extracted
(`allocate_and_free_meshes` runs before `prepare_assets::<RenderMesh>` and reads
every extracted mesh). So the throttle never bounded mesh bytes at all, and a mesh
modified or retired while its `RenderMesh` waited was drawn against buffers that no
longer matched it — or not drawn.

What that looked like on the phone: ground cells missing, the flat basemap
showing through metres below the roads, cars and people (reported 2026-09-26 at
Europaplein), slabs across the view, buildings standing on roads. Reproduced on
the Deck with `CARTO_QUALITY=balanced CARTO_HANDSET=1 CARTO_UPLOAD_KB=4096`,
descending from 900 m to 40 m: three runs broken three different ways, with
bevy's own debug log showing the backlog drained 26 s before the capture — so it
was not an unfinished upload. Every run with the throttle off and the ledger on
was right.

This section used to be "a throttled run must never be photographed", on the
evidence that the first throttled capture was missing the city centre's buildings
while reporting `settled=true`. That was read as "partly uploaded". It was almost
certainly this.

The frame budget itself stands: `stream_ledger` asks every streamer before it
*creates* a frame's assets, so a deferral there is latency in a streamer the
settle test can see, and cannot desynchronise anything — which is also why a
scripted capture keeps the ledger **in force** rather than exempting itself, as it
once exempted itself from bevy's budget: a capture should measure the code path
the handset runs. `RenderAssetBytesPerFrame`
is inserted with `max_bytes: None` on every tier; `CARTO_UPLOAD_KB` reaches the
ledger through `quality::upload_ceiling`. Do not turn bevy's throttle back on
without a capture showing it draws the same picture as without it.

## What is still not measured

The meter counts what the client **queues**, which is what the client controls. It
is not a GPU measurement: pipeline compilation, binning and overdraw can each hang
a frame with this reading at zero, and the Adreno 840's own three-second stall has
never been attributed to a specific one of them — the burst is a demonstrated
cause of device loss on this hardware class, not a proven cause of that particular
stall. `upload_peak_kb` (frame) and `upload_peak_asset_kb` (single asset) ride in
`--csv` / `--dump-state`, so `--expect 'upload_peak_asset_kb<=8192'` is available
as a gate; the frame column is deliberately *not* one, since with the budget on it
describes the queue rather than the upload.


## The other half: a rate is not a total (2026-08-19)

Everything above bounds how fast bytes are handed over. It says nothing about how
many are **held**, and on 2026-08-19 an Adreno 840 lost its device *with the 4 MB
ceiling in force*, three seconds into a cold start. It could not have been the
burst: deferring an upload changes when a tile arrives, never whether it stays,
and a driver refusing an allocation is failing on the total.

`UploadMeter::resident_bytes` is that number — every live `Image` and `Mesh` by
the same `byte_len` arithmetic, maintained by difference so a removal takes its own
contribution back off. It rides in the `slow frame` line and the ten-second census
as `resident_mb`, and in `--csv` / `--dump-state` as `gpu_resident_kb` (and
`gpu_resident_peak_kb`). Like the columns beside it, it counts asset bytes and is
therefore machine-independent — a legitimate `--expect` gate.

It is **not** a VRAM figure: no render targets, shadow maps, atmosphere LUTs,
swapchain images, pipeline memory or allocator overhead. It is the streamed content
this client creates and drops, which is the term this code controls.

Groningen at 150 m, warm cache, `--dump-state`:

| configuration | resident | draws | settled |
|---|---|---|---|
| the handset rung as shipped 2026-08-18 | **991 MB** | 695 | 45.1 s (hit the cap) |
| + `tile_px` 256 | 567 MB | 695 | 27.3 s |
| + `radius_bias` 1 (what ships now) | **371 MB** | 527 | 15.7 s |
| `Budget::phone`, for scale | 307 MB | 225 | 12.9 s |

A gigabyte arrived by accident. Moving Android to `balanced` on 2026-08-18 took
`tile_px` 256 → 512 — four times the texture bytes of each of the clipmap's 424
tiles, 424 MB by itself — and `radius_bias` 1 → 0, which adds tiles and LoD2.2
cells, the other half of the total. Both are *memory* costs, in a change reasoned
about entirely in fill and frame time. `quality::handset_guarded` caps both on
every rung a handset can reach, and leaves everything that decides the picture
(shadows, bloom, detail layers, render scale) to the rung.

**The frame times in that table are lavapipe's** and belong to the container; the
byte and draw columns are exact. And 991 → 371 MB is measured while "and therefore
the device survives" is not — no harness here can lose a Vulkan device.

## The guard a settings screen could delete

`quality::Budget::upload_bytes_per_frame` was kept for the handset in
`android_budget()`, i.e. on the rung `auto()` picks. Settings ▸ Graphics ▸ Detail
does not go through `auto()`: it writes a `QualityPreset`, which `quality::parse`
turns into `Budget::balanced()` / `Budget::desktop()` **verbatim** — and those have
no ceiling. Choosing "Balanced" or "Full detail" on a phone therefore deleted the
one guard written for phones, from the screen that exists to help a struggling
device.

Two reports from the same phone five minutes apart show it, and the tell is the
largest single asset, since `lod22::max_cell_mesh_bytes` derives its cut from that
field: `peak_asset_kb` 4,095 on `Auto`, 7,920 on an explicit rung, with eleven
`single asset above the frame upload budget` lines the first run never printed.

The fix is that the guards belong to the **machine**: `detect` passes every answer
through `handset_guarded` on Android, whatever chose it. `CARTO_UPLOAD_KB` still
overrides the ceiling downstream (`0` = none), so the one person who can A/B it —
whoever holds the device — keeps that.

Two things made this expensive to find, and both are fixed:

- **The resolved rung had never appeared in an Android log.** `quality::init` runs
  at the top of `run()`, before `DefaultPlugins` installs `LogPlugin`, and on
  Android that is the only thing writing to logcat. The tier had to be *inferred*
  from `peak_asset_kb`. `quality::report_budget` is a `Startup` system that says it
  again where a phone can read it, with `chose=` naming which of the three answers
  won and `upload_kb=` the ceiling actually in force.
- **A rule that only exists in the Android build is a rule no test can see.** This
  is the second time (`android_budget` was the first): `handset_guarded` is
  compiled on every target and `detect` reaches it through `cfg!`, not `#[cfg]`.

## The ceiling reached the streamers that were never given one

`lod22::max_cell_mesh_bytes` bounds a 3DBAG cell on **every** rung — the raw
budget where there is one, 8 MB where there is not. The vector ground stack was
handed `quality::upload_ceiling()` raw instead (`map_geometry`'s one `mesh_bytes`,
which feeds `tessellate_surfaces`, `tessellate_roads` *and*
`scatter::build_vegetation_meshes`, plus `street_detail`'s surveyed trees). That
value is `None` on every rung above the phone, so those four paths were cut into
byte-bounded pieces on a handset and left whole everywhere else — while
`street_detail` was already taking the derived cap for its barrier pieces 450
lines further down the same file. They take it now too.

**This bounds the tail; it does not change the picture, and the measurement is
the point.** Groningen at 150 m (2026-08-21, desktop rung) peaks at a 6,074 KB /
89,712-vertex canopy mesh — real, and flagged by `upload_meter` as the asset no
per-frame throttle can spread, but *under* the 8 MB cap, so the splitter
correctly leaves it whole. Before and after are identical to the byte:
`upload_peak_asset_kb` 7,920, `draws` 737, `gpu_resident_kb` ~1.04 GB, and the
same 45 `single asset above the frame upload budget` lines. What changed is that
a denser cell than this one can no longer produce an unbounded mesh off the
phone; what did not change is anything you can see. Do not quote this fix as a
measured win — the honest claim is bounded-versus-unbounded.

## Counted at wgpu: the ground cells hold 1.3 GB at street level (2026-10-20)

`upload_meter` counts the streamed images and meshes it sees created. It cannot
see bevy's own buffers or render targets, and a mesh uploaded `RENDER_WORLD`-only
is only its size at birth. The vendored wgpu counts at the API instead
(`wgpu::meter`, `crates/wgpu/src/meter.rs`): every buffer and texture from
creation to drop or `destroy`, and every byte written. That works on every
backend, the browser's WebGPU included. It is what the client asked for: driver
padding and the browser's own copies come on top.

The client logs it every five seconds, so a phone, a browser and a native run all
report it the same way:

- `gpu`: resident MB (textures, buffers), counts, peak, MB/s written, and the most
  written in any one frame;
- `mesh memory`: resident mesh bytes by streamer (3DBAG, OSM buildings, tiles,
  globe, other, held by no entity);
- `mesh owners`: the `other` bucket by marker component, checked on the mesh
  entity and up its parents, with the surface-cell count and the camera altitude.
  It uses a fixed list in `render_census::mesh_owner`, because a release build
  keeps no component names (bevy's `debug` feature). Add to the list when a new
  streamer owns meshes.

`tools/cdp_probe.ts` and `tools/firefox_probe.ts` collect the lines into one table
beside the camera script's moves (`--drive-script zoomout`, `--zoom-out`). The
shot harness has `gpu_buffers_kb` / `gpu_textures_kb` too.

**The first reading: the ground surface ring.** Chrome on `mac-1`, Groningen,
with the camera taken down to 12 m. Total residency went from ~500 MB at 1.5 km to
**1.84 GB**. Of that, **1.27 GB was `MapSurface`**: map_geometry's 3×3 ring of
z14 cells, 532 meshes, about 140 MB a cell. The ring only builds flat ground below
`FLAT_GROUND_ON_M` (250 m), and the browser-wide per-frame upload cap does not
bound a total. At street level the client therefore holds over a gigabyte of
terrain-refined ground geometry. That is the leading suspect for the web viewer
freezing in Firefox near the ground (reported on a Mac mini, 2026-10-19). It was
not reproduced, but nothing else in the census comes close. The cells are only
released once the camera leaves for orbit (`GLOBE_SHOW_ALT`).

Two traps from taking it:

- **The wheel direction differs per driver.** In Chrome via CDP the first burst of
  negative `deltaY` took this camera *down* to 12 m. In Firefox via BiDi a
  positive `deltaY` zooms out. Read the altitude on the `mesh owners` line before
  reading a run as a zoom-out.
- **A fresh Firefox profile on the Mac nodes never fires `requestAnimationFrame`
  once a page touches WebGPU.** A twenty-line clear-the-canvas page does the same,
  so it is not the client. A reused profile works. Warm the profile with one short
  run before measuring.

### The ground ring has a ceiling (2026-10-20)

`Budget::surface_ring_bytes` caps what map_geometry's cells hold together:
**320 MB** on the default rung (Android, and the browser, which inherits it),
**160 MB** on the phone rung, the handset guard keeps one on any rung a handset
picks, and the desktop has none. `CARTO_SURFACE_MB` overrides it and `0` is none.
`load_control` could not do this: it narrows rings by radius, and this one is
already at radius 1.

`within_ceiling` keeps the cells nearest the stream centre that fit. The cell
under you is always kept. The test uses each cell's measured cost, remembered
after it leaves, so the kept set does not flip as cells come and go. An unmeasured
cell counts as the mean of the measured ones. A cell left out is simply not built,
and the basemap image carries that ground, as it does beyond the ring. That cell is
not "complete", so `update_tile_occlusion` keeps drawing the tile there.

Measured with the meter, same viewpoints, cap off against cap on:

| | GPU resident | ground cells |
|---|---|---|
| web, Chrome, camera at 12 m | 1.84 GB → 0.75 GB | 9 → 1 (192 MB) |
| native, 40 m over the Oosterpoort | 1.94 GB → 0.65 GB | 9 → 1 (183 MB) |
| native, 15 m looking across a cell edge | 1.93 GB → 0.65 GB | 8 → 1 (183 MB) |

The native captures could not be told apart from 40 m or 15 m, with buildings in
the foreground. At ~180 MB a cell the 320 MB ceiling holds only the cell you are
in. When the cells get cheaper (another session's vertex weld before
`refine_for_terrain`), more of the ring fits under the same ceiling without any
change here.
