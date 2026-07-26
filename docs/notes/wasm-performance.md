---
topic: wasm-performance
triggers: [platform/workers.rs, lod22.rs, osm_buildings.rs, IoTaskPool, cdp_probe, "wasm", "web build"]
updated: 2026-07-26
---

# What actually costs frames on the web build

The standing rule — **on wasm `IoTaskPool` runs on the main thread**, because
Bevy disables `multi_threaded` there, so parse and mesh work must go to
`platform::workers` — is documented where it bites, at `lod22.rs:349`. This note
holds only what the code cannot.

## The measurement

2026-07-26, `tools/cdp_probe.ts --gpu --profile` against the real adapter
(`vendor=intel architecture=xe-lpg`, Meteor Lake Arc, Mesa 26.1.5) — **not**
SwiftShader, which headless Chromium falls back to silently and which makes any
frame number meaningless.

Before: **median 1–2 fps, 500–2000 ms frames.** Cause was `serde_json` on the
main thread — ~32% of total samples, ~46% of non-idle, led by
`drop_glue::<serde_json::Value>` (14.2%) and `Value as Deserialize::deserialize`
(10.6% + 4.9% visitor).

After moving parse+mesh to `platform::workers` (`1ba27b0`, `11f8d7e`, `9a9b67d`):
**median 52 fps, 16–20 ms frames**, and `serde_json` is absent from the
main-thread profile.

## Ruled out — do not re-test these first

A/B'd on the real GPU, all within noise of baseline: **atmosphere, env-map IBL,
haze, clouds, rain**. GPU load is implausible as a cause at ~520 draws / 1.5 M
triangles on this device. Shadows were never tested and are ~79% of submitted
triangles — `?no_shadows` exists to check that one.

## Still true, still costing

Parsing is still into **untyped `serde_json::Value`** rather than derived structs
(`lod22.rs:459`, `osm_buildings.rs:108` and `:1845`), so dropping the tree costs
more than building it. That cost merely moved off the main thread; it was not
removed. Typing these is the next win if 3DBAG streaming latency matters.

check: `grep -q "serde_json::Value" crates/viberfox/src/systems/lod22.rs`

Overpass's validate-before-persist survived the move via a derived
`OverpassRemark` struct (serde skips `elements` without allocating). Its
200-status `remark` error channel must still be rejected before caching —
a 200 response is not a success there.
