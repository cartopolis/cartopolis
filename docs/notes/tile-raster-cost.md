---
topic: tile-raster-cost
triggers: [vector_tiles.rs, map_geometry.rs, render_mvt, tiny-skia, "rasteris", "rasteriz"]
updated: 2026-07-26
---

# Vector-tile rasterisation cost

Measured 2026-07-25 on the Groningen z14 fixture
(`crates/viberfox/tests/fixtures/groningen_z14.mvt`, 495 KB, 8552 buildings /
95 streets), `dev-bevy` with `viberfox` forced to `opt-level = 3`, min-of-30
with warmup:

| Stage | Time |
|---|---|
| `render_mvt`, whole 512 px tile | **~121 ms** |
| `render_mvt`, overzoom z17 sub-tile | ~35 ms |
| `water_shore_field` | ~30 ms |
| `tessellate_surfaces` / `tessellate_roads` | ~10 ms each |
| `ocean_mask` | ~9 ms |
| `get_features` | ~4 ms |
| MVT parse | ~2.7 ms |

**Rasterisation dominates the tile pipeline by ~20× over MVT parsing** — the cost
is tiny-skia's scan conversion, not protobuf decoding.

**Do not trust the per-layer breakdown.** A reconstruction of the per-layer loop
summed to ~282 ms against a measured 121 ms total, so that attribution was never
validated. Re-measure by instrumenting the real `render_mvt` before acting on it.

The benchmarks were reverted, not committed — nothing in the tree measures this.

## The open question, and what any answer must beat

Whether to move rasterisation off the CPU. **Deferred** 2026-07-25 to finish the
wasm build; still open.

This is not per-frame work. It runs on `platform::workers` (native = Bevy
`AsyncComputeTaskPool`, wasm = own Web Worker pool), gated by `Dispatcher` at
`TILE_WORKERS = 3`, and each tile is rasterised once then cached as a texture. So
it is a **streaming-latency and worker-occupancy** cost, not a frame-time cost —
and see [wasm-performance.md](wasm-performance.md), where the thing that actually
cost frames on web was elsewhere entirely.

Any "move it to the GPU" argument has to beat that, and must survive wasm32 plus
the pure-Rust / no-C-dependency constraint inherited from the web target.
