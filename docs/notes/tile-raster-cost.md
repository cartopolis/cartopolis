---
topic: tile-raster-cost
triggers: [vector_tiles.rs, map_geometry.rs, map_stream.rs, tile_source.rs, render_mvt, tiny-skia, TaskPoolOptions, "rasteris", "rasteriz", "tile cache", "map loads slowly", "paint batching", "fill rule", vello_cpu, "benchmark", "streaming performance", print_tile_pipeline_time]
updated: 2026-08-24
---

# Vector-tile rasterisation cost

Measured 2026-07-25 on the Groningen z14 fixture
(`crates/geo/tests/fixtures/groningen_z14.mvt`, 495 KB, 8552 buildings /
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

The benchmarks from that session were reverted, not committed — and the same
thing happened to every later measurement in this note, each taken with a
throwaway harness rebuilt for the occasion. **Since 2026-08-24 the instrument is
in-tree**:

```bash
cargo test -p cartopolis_geo --release print_tile_pipeline_time -- --ignored --nocapture
```

prints the per-stage wall clock (min over adaptive repeats, beside its median as
a noise check) for parse, decode, both render sizes, the overzoom sub-tile, the
mip chain, the ocean mask, the shore field and the three tessellators, over both
fixtures. It is a printing hatch on the same terms as `print_tile_pipeline_cost`
— an instrument, never a CI gate (the allocation-budget doctrine in
`crates/geo/src/perf.rs` explains why a timer threshold cannot live in CI here).
Numbers below marked with a date are readings from it or its predecessors;
re-take them with the command rather than trusting a table to stay true.

## Reading from the committed hatch, 2026-08-24 (agent container, `--release`)

Taken **with the paint batching of the section below already in** — the 78.5 ms
per-feature render it replaced is in that section's table.

| Stage | min | note |
|---|---|---|
| parse (Groningen z14) | 4.7 ms | |
| parse + decode all layers | 14.7 ms | decode alone ≈ 10 ms |
| `render_mvt_sized` 512² whole | 67.6 ms | |
| `render_mvt_sized` 256² whole | **48.6 ms** | see below |
| render 256², 1/64 overzoom sub | 8.2 ms | the cull at work |
| mip chain over the 512² render | 9.9 ms | |
| `water_shore_field` | 19.4 ms | |
| `ocean_mask` 256² | 0.3 ms | |
| `tessellate_surfaces` / `_roads` / `_bridges` | 0.8 / 3.1 / 0.01 ms | |

And the end-to-end pair the harness columns were built for, same day, release
build, lavapipe, Groningen at 420 m (`--settle 2 --wait 90`,
`CARTO_CACHE_DIR` fresh vs reused). Taken on the client build **just before the
paint batching landed** — the `map_tiles` busy column is the stage batching
made ~13% cheaper per tile, so re-take the pair rather than quoting these
absolutes; the attribution and the cold/warm shape are what this table holds:

| | cold | warm |
|---|---|---|
| settled | **27.8 s** | **25.7 s** |
| fetches / MB | 310 / 103.6 | 6 / 0.8 |
| fetch TTFB / body (summed) | 32.7 s / 1.8 s | 1.8 s / 0.04 s |
| worker busy / wait (summed) | 36.7 s / 0.3 s | 48.0 s / 12.8 s |
| `map_tiles` busy | 28.3 s | 28.9 s |
| draws / `gpu_resident_kb` | 662 / 969,279 | 662 / 969,279 |

Three readings. **A fully warm cache buys 2 s of a 27.8 s fill** — the 2026-07-31
conclusion (you wait for scan conversion, not the network) reproduced, now with
per-stream attribution: `map_tiles` rasterisation alone is ~29 s of worker time,
three quarters of the pipeline, `lod22` most of the rest. **Warm wait *rises***
(0.3 → 12.8 s): with nothing pacing the decodes, the pool saturates — so more
raster throughput, not more fetch throughput, is what would move `settled_s`.
And **the machine-independent columns are bit-identical across the pair**
(draws, resident bytes), which is the property the `--expect` gates rely on.
TTFB dominates body 18:1 even cold — measured from the VPS where RTT ≈ 0, so
that is TLS plus server time per request, already hidden by concurrency; it is
not a bandwidth story on this link.

**That 2 s is the reading the warm stores were built from** — see
[keeping-what-was-expensive.md](keeping-what-was-expensive.md). Caching the
*source* bytes cannot move a fill dominated by scan conversion, so since
2026-09-11 `map_stream` keeps the rasterised mip chain of a retired tile and
`lod22` keeps its merged meshes' handles. Neither touches a cold fill; both
remove the rebuild on a return.

**A 256² render costs ~72% of a 512² one, not 25% — and batching did not
change that.** Scan conversion scales with area (≈25 ms of the 67.6 at 512²);
the rest is size-independent, per-path work. Before the batching it was
per-*feature* (the pre-batching pair read 56.5/76.8 = 74%, and the two sizes
allocated identically); after it the per-call floor is paid per *run* and the
residual fixed share is dominated by the path geometry itself plus the capped
fill chunks. Two things follow: the phone tier's `tile_px = 256` buys less
rasterise time than its area ratio suggests on dense tiles, and further wins in
this stage are in the batching's own knobs (`FILL_BATCH_POINTS`, run lengths) —
the section below has what batching bought and what it must keep. A related
attempt from the same day is worth recording so it is not repeated: sharing the
built path between a layer's two paint passes (fill+outline, casing+fill) cut
allocations 25% and moved the clock **not at all** — path construction was
never the time, and batching then removed those builds anyway.

## The validated per-pass split, and what batching bought (2026-08-24)

The re-measurement the paragraph above asked for exists now: `perf::passes`
times each `PAINT_ORDER` pass inside the real loop (`cfg(test)` only), and
`cargo test -p cartopolis_geo --release dump_paint_split -- --ignored
--nocapture` prints it. It sums to the loop total, unlike the 282 ms
reconstruction. On the container (min of 5, whole 512² tile, so compare ratios
against the workstation table above):

| Pass | per-feature | batched |
|---|---|---|
| buildings fill | 29.8 ms | 27.5 ms |
| buildings outline | 32.1 ms | 24.6 ms |
| streets casing + fill | ~11 ms | ~10 ms |
| land | ~7 ms | ~6 ms |
| everything else | ~1.5 ms | ~1.5 ms |
| **paint loop total** | **78.5 ms** | **68.6 ms** |

The two `buildings` passes are **~77% of the paint**, at ~4 µs per
`fill_path`/`stroke_path` call across 8,552 features — per-call setup (clone +
transform the path, build and sort an edge list, raise a blitter), not scan
conversion. So the paint loop now batches consecutive same-(colour, width)
features into one path per submission (`flush_batch`). Overzoomed sub-tiles win
most: factor 8 went 27.8 → 16.0 ms/tile (−42%), because the cull leaves few
features and the per-call floor dominated. Allocations per render fell from
~118,900 to ~1,130 (`RENDER_DENSE_ALLOCS`).

Four things the batching must keep, learned the hard way:

* **Fill batches are capped (`FILL_BATCH_POINTS`), stroke batches are not.**
  One path holding every building made the fill *slower* than per-feature —
  tiny-skia's active-edge table then spans the whole city on every scanline.
  4,096 points measured best of {1,024, 4,096, 16,384, ∞}.
* **Batched fills use `FillRule::Winding`, not `EvenOdd`.** Shortbread's `land`
  layer has overlapping same-colour polygons; under a batched EvenOdd the
  overlap XORs back to the paper background (a pale wedge in a park, found by
  image diff). Winding unions overlaps and still cuts courtyard holes because
  MVT mandates opposite winding for interior rings — verified on the fixture.
* **A culled feature still advances the batch** (`count_points`): chunk
  boundaries must not depend on the cull, or the AA overlap blends inside each
  chunk shift and `culling_off_screen_features_changes_no_pixels` fails by a
  few thousand bytes.
* The batched raster is not byte-identical to the per-feature one — max channel
  delta 8/255 on the fixture, all of it anti-aliased edges. That direction is an
  improvement: one path renders shared edges watertight, so adjacent buildings
  lost their faint AA seam and same-style strokes no longer double-blend at
  junctions.

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
  `tiles.cartopolis.org` from inside the VPS (so RTT ≈ 0 — a floor, not a client
  figure): TLS handshake ~47 ms, server ~13 ms, body ~70 KB. A 245-tile fill is
  ~17 MB. On a consumer link that is bandwidth, and no protocol makes a link
  faster.
* **The cost is a whole HTTP stack.** `ureq` has no h3; it would mean `reqwest` +
  its unstable `http3` feature (behind `RUSTFLAGS --cfg reqwest_unstable`) or
  hand-rolled `quinn` + `h3` (0.0.x), plus rustls, re-earning gzip and pooling,
  under the wasm32 / pure-Rust constraint.

If protocol work is ever wanted anyway, **HTTP/2 is the better trade than
HTTP/3**: the same multiplexing win, stable in `reqwest`, and the origin already
speaks it. The number that decides the whole question is the split of a cold
fill's fetch time into TTFB versus body — and since 2026-08-24 the tree records
it: `platform::http::stats` splits every native fetch at the header boundary,
and a `--shot` reports the per-capture sums as the `fetch_ttfb_ms` /
`fetch_body_ms` columns (with `fetch_count` / `fetch_mb` beside them). A
cold-vs-warm pair of captures over one viewpoint is now the whole experiment.

## vello_cpu was measured against tiny-skia and is not a win here (2026-08-24)

linebender's sparse-strips CPU renderer (`vello_cpu` 0.2, from the org that now
hosts tiny-skia) was the obvious candidate replacement: SIMD with *runtime*
dispatch, NEON and wasm-simd128 lanes, optional per-render multithreading, and
benchmarks aiming at "fastest CPU 2D renderer in Rust". `bench_vello_cpu`
(dev-dependency only, nothing shipped links it) renders the same whole z14 tile
through both — same paint order, colours, widths, caps, joins and batching —
and the answer on the container is **parity**: 77.0 vs 76.4, 78.7 vs 84.9,
79.2 vs 83.6 ms/tile across three runs, i.e. inside this box's noise. The
output is visually correct (`/tmp/vt_vello.png`).

Why the marketing gap: this workload is thousands of tiny solid-colour
polygons — per-call setup and short edge lists — which is tiny-skia's sweet
spot once the paint is batched, and our x86 builds already hand tiny-skia AVX2
via `target-cpu=x86-64-v3`. Sparse strips earn their keep on large/complex
paths, gradients and blend stacks, none of which the tile paint uses. What
would reopen this: a phone/wasm measurement showing a different ratio there
(vello's NEON and simd128 lanes are newer than tiny-skia's), wanting per-tile
*multithreading* (vello has it, tiny-skia never will), or text-on-raster
(vello_cpu has a glyph pipeline; tiny-skia has none). Worth knowing: egui 0.35's
`epaint` already depends on `vello_cpu` 0.0.9 via `glifo`, so the workspace
compiles one regardless.

## The open question, and what any answer must beat

Whether to move rasterisation off the CPU. **Deferred** 2026-07-25 to finish the
wasm build; still open — and the CPU-side alternative (`vello_cpu`, above)
measured at parity, so a GPU path remains the only candidate for a step change.

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

**Parsed tiles are now shared across tasks** — `tile_loader::parse_shared`
holds a `Weak<TileData>` per source tile, so the 179 overzoomed requests and the
z14 bodies `map_geometry` and the router want resolve to one parse while any
task holds it. The number this paragraph used to say the decision was blocked on
— how big a parsed tile is — is measured now and pinned as a budget
(`perf_budgets::a_parsed_and_decoded_tile_retains_a_bounded_footprint`, off
`Cost::retained_bytes`): the dense Groningen z14 retains **3.3 MiB parsed** and
a further **12.5 MiB with every layer decoded**, so each concurrently-held tile
is ~15.8 MiB of heap. That is the figure to multiply by worker width when
reasoning about a teleport burst's memory, and the budget is what stops it
growing unremarked.
