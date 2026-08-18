---
topic: upload-bursts
triggers: [device lost, DEVICE_LOST, TDR, command buffer timeout, upload, upload_meter, RenderAssetBytesPerFrame, upload_bytes_per_frame, MAX_CELL_MESH_BYTES, lod22, streaming burst, churn, "slow frame", peak_asset]
updated: 2026-08-18
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

## A throttled run must never be photographed

`--shot` and `cargo shots` disable the budget outright, and the reason is worth
keeping: the settle test watches the **streamers**, and the budget defers work
that is already past them. Everything has been fetched, meshed and handed over —
every streamer is quiet — while bevy still holds meshes for later frames. The
first throttled capture of this viewpoint was missing the city centre's buildings
and reported `settled=true`. A green run that documents something untrue is worse
than a red one, and the README set and the `scene-smoke` gate are both built on
these captures.

If the settle test ever learns to see the upload backlog, this exemption can go.
It cannot today: the backlog lives in the render world (`PrepareNextFrameAssets`)
and the harness runs in the main one.

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
