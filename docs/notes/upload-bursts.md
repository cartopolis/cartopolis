---
topic: upload-bursts
triggers: [device lost, DEVICE_LOST, TDR, command buffer timeout, upload, upload_meter, RenderAssetBytesPerFrame, upload_bytes_per_frame, MAX_CELL_MESH_BYTES, lod22, streaming burst, churn, "slow frame", peak_asset, resident, residency, gpu_resident_kb, handset_guarded, tile_px, out of memory, allocation]
updated: 2026-09-26
---

# A per-frame cap on *items* is not a cap on bytes

Every streamer in this client bounds how much it finalises per frame —
`map_stream::MAX_UPLOADS_PER_FRAME`, `globe::MAX_GLOBE_TILE_BUILDS_PER_FRAME`,
`lod22::MAX_BUILDS_PER_FRAME`, `osm_buildings::MAX_FAR_BUILDS_PER_FRAME` — and
CLAUDE.md states the rule that every new streaming path must do the same. All of
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
(`android-build.md`) and it has the same shape as the Intel iGPU loss CLAUDE.md
has documented for much longer.

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
settle test can see, and cannot desynchronise anything. `RenderAssetBytesPerFrame`
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
