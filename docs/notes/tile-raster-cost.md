---
topic: tile-raster-cost
triggers: [vector_tiles.rs, map_geometry.rs, map_stream.rs, tile_source.rs, render_mvt, tiny-skia, TaskPoolOptions, "rasteris", "rasteriz", "tile cache", "map loads slowly"]
updated: 2026-08-10
---

# Vector-tile rasterisation cost

Measured 2026-07-25 on the Groningen z14 fixture
(`crates/cartopolis/tests/fixtures/groningen_z14.mvt`, 495 KB, 8552 buildings /
95 streets), `dev-bevy` with `cartopolis` forced to `opt-level = 3`, min-of-30
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

## Why the overzoom row was so expensive, and what fixed it (2026-08-10)

The `~35 ms` overzoom row above covers **1/64 of the ground** of the `~121 ms`
whole-tile row. That ratio was recorded and never explained. The cause: the
sub-tile transform was correct, and the feature loop ignored it — every feature
in the source body was built into a path and handed to tiny-skia regardless of
whether it could land on the pixmap.

tiny-skia does not rescue this. In 0.11 `fill_path` clones and transforms the
whole path *before* any bounds test (its `// TODO: ignore paths outside the
pixmap` is still there) and `stroke_path` builds the complete stroke outline
first. Worse, stroke widths scale with the overzoom, so `buildings`' 0.6 px
outline is a cheap hairline at factor 1 and a 4.8 px stroked outline at factor 8
— across ~8,500 building features, listed twice in `PAINT_ORDER`.

`render_mvt_sized` now bounds each feature and skips it when it misses the
sub-tile window (plus the stroke's reach and an anti-alias pixel). Measured with
the `bench_overzoom_cull` hatch in `vector_tiles.rs`, all sub-tiles of each
factor, on the CI-style container (so compare the ratio, not the absolutes,
against the table above):

| factor | before | after |
|---|---|---|
| 2 (z15) | 144.4 ms | 140.6 ms |
| 4 (z16) | 123.1 ms | 78.5 ms |
| 8 (z17) | 84.9 ms | 41.0 ms |

**Factor 2 barely moves**, and that is the shape of the thing: a quarter-tile
window still catches most features, so the saving arrives exactly where the
tiles are deep and numerous. 179 of a fresh anchor's 424 requests are
overzoomed.

The gate is `culling_off_screen_features_changes_no_pixels`, which renders seven
sub-tiles across factors 2/4/8 with the cull on and off and asserts byte-identical
rasters — a margin that is too tight drops a feature that should bleed in from
just outside the window, which reads as a seam along a tile border. **Still not
measured: whether this moves `ShotMetrics::settled_s`,** for the same reason as
the section below — it needs a GPU.

## The raster edge is a budget, not a constant (2026-08-10)

Every row in the table above rasterises at 512², which is what the flat clipmap
did on every tier for the life of this note. It now takes its edge from
`quality::Budget::tile_px` — **512 on a desktop, 256 on a phone** — and
`TilePayload::decode` takes the size as an argument rather than reading a
constant, so the caller owns the trade.

This is the globe's own conclusion, arriving on the flat path a release late.
[globe-tile-cost.md](globe-tile-cost.md) measured the globe's leaves at 170–340
screen px against a 512² raster — 2–3× oversampled per axis, for 4–9× the scan
conversion — and answered with `GLOBE_TILE_PX = 256` plus `render_mvt_sized`.
The flat path is the *larger* of the two consumers and never got it.

The arithmetic that decides it: a clipmap is **224 tiles**, and 512² RGBA with
mips is ~1.4 MB each — **~310 MB of texture against ~78 MB at 256²**. And since
scan conversion dominates parsing by ~20× (top of this note) and scales with
area, the same halving is also most of the time-to-first-map.

**The honest cost, and why it is only payable here.** At street level the finest
ring drops from ~2.8 to ~1.4 texel/m, which is a visibly softer basemap if you
look for it. That is acceptable *at ground level specifically* because the
imagery is not what you are looking at there — the vector `surfaces` geometry is
drawn over it and the imagery is the backdrop between the polygons. It would not
be acceptable on a tier where the imagery *is* the map, so this is not a knob to
turn down further "for performance" without saying which tier.

A **raster** source is left at whatever size it was served at. There is nothing
to re-rasterise, only pixels to resample, and a CPU resample costs more than the
memory it saves.

## How wide the pipeline actually runs (measured 2026-07-31)

Bevy's `TaskPoolOptions` defaults cap **both** the io and async-compute pools at
**4 threads** regardless of core count (`bevy_app/src/task_pool_plugin.rs`), and
the map streamer lives entirely in those two — `platform::http` is a blocking
`ureq` call parking an io thread, `platform::workers` forwards every
rasterisation to `AsyncComputeTaskPool`. On a 22-core workstation the map
therefore filled 4-wide with 14 cores idle. `main` now overrides the widths
(io ≈ 30%, async-compute ≈ 45%, `compute` keeps the rest).

Seconds for `MapStream::streaming_busy` to fall, `--shot` over Groningen on
`dev-bevy` (which leaves `cartopolis` itself at opt-level 1 — a release build is
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

## HTTP/3 was considered for the tile fetch and rejected (2026-08-08)

Asked directly: would QUIC speed up tile streaming? No, and the reasons are worth
keeping so it is not re-proposed.

* **On the web build it is already there and the client has no say.** The origin
  answers `alt-svc: h3=":443"` (Caddy), so browsers upgrade after first contact.
  `ehttp` → `fetch` chooses nothing.
* **On native, `ureq` 2.9 is HTTP/1.1 only**, but the wins QUIC brings are already
  bought another way: head-of-line blocking is sidestepped by the shared
  `Agent`'s 16 idle connections per host, and the handshake is amortised by that
  same pool (the ~90 ms measurement in `platform::http`'s doc comment).
* **The remaining time is bytes, not round trips.** Measured against
  `tiles.garage44.eu` from inside the VPS (so RTT ≈ 0 — a floor, not a client
  figure): TLS handshake ~47 ms, server ~13 ms, body ~70 KB. A 245-tile fill is
  ~17 MB. On a consumer link that is bandwidth, and no protocol makes a link
  faster.
* **The cost is a whole HTTP stack.** `ureq` has no h3; it would mean `reqwest` +
  its unstable `http3` feature (behind `RUSTFLAGS --cfg reqwest_unstable`) or
  hand-rolled `quinn` + `h3` (0.0.x), plus rustls, re-earning gzip and pooling,
  under the wasm32 / pure-Rust constraint.

If protocol work is ever wanted anyway, **HTTP/2 is the better trade than
HTTP/3**: the same multiplexing win, stable in `reqwest`, and the origin already
speaks it. What has *not* been measured is the split of the cold-minus-warm 5.7 s
above into TTFB versus body time; that number decides the whole question and
nothing in the tree records it.

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

## How often it ran, before anyone made it cheaper

2026-07-30. The first thing to do with a 121 ms operation is stop repeating it,
and the tile path was repeating a great deal. The counts are now asserted in
`map_stream`'s tests rather than written down here, because the arithmetic is in
the code and a radius or zoom change has to restate them: a fresh anchor at
`DEFAULT_ANCHOR_ZOOM` wants **424** tiles across the z10–z17 clipmap, of which
**179** sit above `MAX_SOURCE_ZOOM` and resolve onto z14 ancestors the *same pass*
is already requesting — leaving 245 distinct source tiles. On top of that,
`map_geometry` parsed each body four times, the globe twice, and `render_mvt`
decoded `streets` and `buildings` twice each because `PAINT_ORDER` lists them
twice.

So the ~2.7 ms parse and ~4 ms `get_features` above were being paid several times
per body while the ~121 ms rasterise was paid once. That does **not** change the
conclusion that rasterisation dominates — it means the parse figures were never
the whole parse bill, and any future comparison must say how many parses it is
counting.

How much that repetition was worth, measured on the same Groningen z14 fixture
(2026-08-03, in the CI container): one decode 41.8 ms, and the five tessellators
that share it 85.7 ms together. At seven decodes per cell that is 379 ms, of which
**251 ms — two thirds — was the six redundant decodes**. Worker time, not frame
time, but it is what a burst of 400 cells after a teleport is made of.

**Do not compare 41.8 ms against the ~2.7 ms parse in the table above.** Different
box and different profile (that table forced `cartopolis` to `opt-level = 3`; this
was a `dev-bevy` container build), so only the *ratio* carries over. The harness
that produced it was a `dump_parse_cost` test on the branch this measurement came
from, which did not merge — nothing in the tree measures it now, exactly as the
top of this note says of the original benchmarks.

**Not measured: whether removing that duplication moves `ShotMetrics::settled_s`.**
It is a `--csv` column and a fresh-anchor capture is exactly the 424-request
burst, so the before/after is one command — but it needs a GPU, and the container
this work was done in has none. Until someone runs it, the deduplication is
justified by the counts above, not by a latency number.

## Two things about the byte cache that are guesses, not measurements

`tile_source::MAX_CACHE_BYTES` is **32 MiB, chosen from the ~500 KB/tile body size
and the live neighbourhood the consumers ask for — nobody has measured the actual
working set.** Byte-bounded rather than entry-bounded so a dense tile cannot blow
the budget; LRU with a linear-scan recency order, which is fine at tens of entries
and was never profiled at the globe quadtree's ~900 tiny z2–z8 bodies.

**Parsed tiles are deliberately not cached across tasks.** A `Reader` holds
`geo_types` f32 geometry for every feature in the tile and nothing in the tree
measures how much that is, whereas a body's size is exactly known — so caching
bytes is the half that can be reasoned about. `vector_tiles::TileData` removes the
duplicate parses *within* one task instead. An `Arc<TileData>` cache would
additionally collapse the repeat parses *across* tasks (the 179 overzoomed
requests, and the same z14 bodies `map_geometry` and the router want); it is the
obvious next step and it is blocked on one number: how big a parsed tile is.
