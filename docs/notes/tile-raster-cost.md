---
topic: tile-raster-cost
triggers: [vector_tiles.rs, map_geometry.rs, map_stream.rs, render_mvt, tiny-skia, TaskPoolOptions, "rasteris", "rasteriz", "map loads slowly"]
updated: 2026-07-31
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

## How wide the pipeline actually runs (measured 2026-07-31)

Bevy's `TaskPoolOptions` defaults cap **both** the io and async-compute pools at
**4 threads** regardless of core count (`bevy_app/src/task_pool_plugin.rs`), and
the map streamer lives entirely in those two — `platform::http` is a blocking
`ureq` call parking an io thread, `platform::workers` forwards every
rasterisation to `AsyncComputeTaskPool`. On a 22-core workstation the map
therefore filled 4-wide with 14 cores idle. `main` now overrides the widths
(io ≈ 30%, async-compute ≈ 45%, `compute` keeps the rest).

Seconds for `MapStream::streaming_busy` to fall, `--shot` over Groningen on
`dev-bevy` (which leaves `viberfox` itself at opt-level 1 — a release build is
faster in absolute terms; the ratio is what matters here):

| Case | 4/4 (stock) | io=7, cpu=10 |
|---|---|---|
| 3 km, cold cache (277 tiles) | 12.6 s | 7.5 s |
| street level, cold (424 tiles) | 15.7 s | 9.8 s |
| street level, **warm** cache | 13.3 s | 4.1 s |
| street level, warm, frame ms | 33.4 | 29.8 |

**The warm row is the load-bearing one.** A fully populated on-disk cache barely
helped at 4/4 — 13.3 s against a cold 15.7 s — which says directly that what you
wait for is scan conversion, not the network, and corroborates the table above
from the other end. Anyone about to optimise tile *fetching* should read that row
first.

Two secondary findings from the same run, both against theories that sound right:

* **Uploads are not the limit.** `MAX_UPLOADS_PER_FRAME = 8` allows ~450 tiles/s
  at these frame rates and the decoded backlog stayed under 8 throughout, i.e.
  the GPU-upload cap (a device-loss guard) never bound. Leave it alone.
* **Do not starve Bevy's `compute` pool to feed this.** Pushing async-compute to
  12 threads left `compute` (the parallel system executor, which runs every frame
  forever, unlike a one-off fill) just 2 of 22, and *both* numbers got worse:
  warm street 5.6 s and 34.7 ms/frame, against 4.1 s and 29.8 ms at the balanced
  split. More rasterisation threads stop paying well before the cores run out.

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
