---
topic: performance
triggers: [build_tunnels, unmeasured_profiles, road_levels.rs, tessellate_roads, road_stitch, cut_under_roads, stitch_some, print_tile_pipeline_time, perf_budgets.rs, "tile is slow", "map is slow", "streaming is slow", "vector tile performance", Buckets, SPANS_PER_TRI, MIN_BUCKET_M, covered_weight, RoadMask, dilate_mask]
updated: 2026-10-30
---

# What one tile costs, stage by stage — and where the instrument does not reach

`docs/notes/tile-raster-cost.md` ends by saying rasterisation dominates the tile
pipeline by ~20× over parsing. **That stopped being true in September 2026** and
nobody noticed for a month, because the thing that overtook it was not measured
by anything.

## The measurements

Release build, `crates/geo/tests/fixtures/groningen_z14.mvt`, this container,
2026-09-21, via `print_tile_pipeline_time`:

| stage | 2026-08-24 | 2026-09-21, before | after |
|---|---|---|---|
| `tessellate_roads` | **3.1 ms** | **103.1 ms** | **38.4 ms** |
| `cut_under_roads` | not timed | not timed* | **47.5 ms**, ×2 per cell |
| render 512² whole | — | 68.9 ms | **31.4 ms** |
| render 256² whole | — | 50.2 ms | **29.0 ms** |
| `water_shore_field` | 19.4 ms | 34.6 ms | **24.5 ms** |
| `tessellate_surfaces` | 0.8 ms | 14.0 ms | **8.2 ms** |
| parse + decode all layers | 14.7 ms | 15.6 ms | 15.4 ms |
| mip chain | 9.9 ms | 11.4 ms | 10.2 ms |
| `build_tunnels` | not timed | not timed | 3.0 ms |
| `ocean_mask` 256² | 0.3 ms | 0.3 ms | 0.3 ms |

\* `cut_under_roads` had no timing row at all until one was added on
2026-09-21, and by then the bounded cut had already landed; its true
pre-September-21 cost is unmeasured and was higher than the 128.5 ms the first
reading gave.

Summing the stages a dense cell actually runs — and `cut_under_roads` runs
twice — that is **~478 ms of CPU before, ~198 ms after**.

**`ocean_mask` is the control, and it is now the only one.** A wall-clock
comparison on a shared box is worth nothing without a stage that nothing
touched reading the same on both sides, and that row has read 0.30–0.32 ms
across a month and across every change here. The 1/64 overzoom sub-tile used to
serve the same purpose and **no longer can**: the raster banding changed it too
(8.4 → 7.0 ms). Anyone quoting a timing from this hatch owes a control row
beside it, and owes a check that the control is still untouched by whatever they
changed.

`tessellate_roads` overtook the whole 512² rasteriser. That is the inversion the
first note describes the other way round, and any reasoning that starts "the
cost is scan conversion" is now wrong about a city tile.

## Where it went, measured rather than guessed

Timing the phases *inside* `tessellate_roads` (`perf::passes`, recorded in the
real function — not a reconstruction, which is how an earlier per-layer
attribution came to sum to 282 ms against a measured 121 ms):

| phase | share |
|---|---|
| `road_stitch::stitch` (road sheet) | **91 %** |
| build loop (`ribbon` + `kerb_band`) | 3.7 % |
| `building_passages` | 3.3 % |
| `assign_road_ties` | 1.4 % |
| `street_nodes` | 0.7 % |

Three hypotheses died here and are worth recording so they are not re-proposed:

- **It is not an unindexed all-pairs loop over road segments.** A dense z14 cell
  holds 95 street features, 901 linestrings and 3,496 vertices. Any honest n²
  over *that* is single-digit milliseconds.
- **It is not a mis-sized bucket grid.** Over the road sheet the index places a
  triangle in 4.0 cells — which is what `SPANS_PER_TRI` asks for.
- **It is not the duplicated building work.** `building_passages` rebuilds all
  8,552 building footprints before testing whether any street is underground,
  and does it from two call sites per cell. Real waste, genuinely fixable, and
  it measures **2.8 ms against 103**. Two separate audits ranked it first;
  measuring moved it last.

What it *is*: volume. 21,557 triangles reach the cut, from 3,496 street vertices
— roughly six per source vertex, from `round_corners` fillets multiplied by
`kerb_band`'s three ribbons per road.

## `cut_under_roads` is a different shape of expensive, and it is the one left

It is now the single largest item in the pipeline and it runs **twice** per cell
(`map_geometry`, once for paving and once for the landuse fills). Its subjects
are earcut'd ground fills, which arrive as a handful of triangles spanning whole
city blocks:

| | road sheet | `cut_under_roads` |
|---|---|---|
| candidates examined per query | 55 | **412** |
| clip contours per boolean | 4.2 | **17.2** |

Those 412 are mostly *genuine* — a block-sized subject really does overlap
hundreds of road triangles — so a finer grid does not remove them.

**Dicing the subjects was the obvious fix and it is refuted.** Timing the
phases inside the call gives `i_overlay` 66 % of it, and across the two call
sites the solver costs **~1.43 µs per clip contour with near-zero fixed
overhead** — 4.2 contours cost 5.3 µs on the road sheet, 17.2 cost 23.9 µs
here, and that line predicts both totals within 14 %. Cost therefore tracks the
*total* number of contours inserted, and dicing a subject into sixteen pieces
leaves that total alone: each blocker simply meets more, smaller subjects.
Reducing it means regrouping blockers or subjects, which does change how a cut
sheet is triangulated and so has to be argued against the no-hairline-cracks
invariant on a real graphics card.

Two cheaper things were measured and are worth not repeating. Only **19 of
6,157** subject triangles are wholly buried by a single blocker, so hoisting the
containment test to avoid wasted contour insertions saves four contours in
total. And the bucket grid is *not* mis-sized here either: 9.6 cells per query
at a 24 m edge.

What did work was **threading it**: the cut is order-independent by
construction, so it runs on four threads and the stage went 128.5 → 47.5 ms
with byte-identical output. The remaining 47.5 ms is still the largest single
item in the pipeline.

**And at street level it is the whole load (2026-10-20).** A sampling profile of
the `web-release` build filling the `street` scenario on `deck-1` (nine cells
with surveyed paving, gap fills and the street objects) puts
`road_stitch::cut_ribbon_inner` at **62 % of all worker time**, `i_overlay`'s
`CrossSolver::cross` and `tree_split` at 46 % between them, and the terrain
refinement at 7 %. A surface cell there is ~21 s of worker CPU; the fixture
tile's 47.5 ms is not what a street-level cell costs.

**What it was: cuts cutting the output of cuts.** Recording every
`stitch_some` input of one street-level fill and replaying them (the profile
cannot attribute the cut to a caller: it runs on scoped threads, whose stacks
start at their own closure) showed the road sheet's own cut costing 1.4 s of
277 and the sheet-against-sheet cuts the rest. Each cut re-triangulates what
it leaves into fans of slivers, and the next took those as subjects or
blockers: in one gap-fill cut 107,480 of a sheet's 142,499 blocker triangles
were under 0.05 m², each solve meeting up to 11,587 of them. A smaller gap-fill
quad did not help (8 m quads still met ~2,600). What did: a cut only removes
overlaps, so the union of the sheets is unchanged by it, and the fills below the
paving are now cut once against the roads and the paving *as laid*
(`paving::cut_ground_under_roads_and_paving`), and the gap fill is asked of the
laid sheets. `i_overlay` is held to its list solver (`road_stitch::new_overlay`;
`Auto`'s tree, chosen past 4,000 segments, was slower). The recorded set went
56.8 → 18.1 s optimised; street, city and Earth settle roughly twice as fast
(`render-performance-survey.md`). The old two-pass cut had also left ~30 m² of
`Built` under the paving on the fixture.

**Still open:** `cut_into_ground` cuts the crops against the already-cut sheets
above them, the same cascade on farmland; and the paving itself is dense (300 k
triangles a cell as a blocker), so every fill triangle laid under it still meets
thousands.

## `build_tunnels` grew tenfold, and it is the road-level solve (2026-10-30)

Same hatch, `mac-1` (Apple M5 Pro VM), release: `build_tunnels` read **30.8 ms**
on the fixture against the 3.0 ms above — the most expensive row in the hatch,
ahead of `cut_under_roads` (20.7 ms). Since September it plans every structure
on the solved road network (`tunnels::unmeasured_profiles` →
`road_levels::solve` + `structures`), and that is 26 of the 31 ms:

| phase, per call | ms |
|---|---|
| `solve`: the outlier median (`Net::within` per measured node, ~28 k calls) | 10.1 |
| `solve`: `Net::build` | 6.3 |
| `structures`: `Net::build` again, over the same lines | 6.3 |
| `solve`: the four grade envelopes | 2.8 |
| everything else in both | 1.4 |

The median's search now runs in reused dense buffers instead of a `HashMap`
and heap per call: `build_tunnels` **30.8 → 22.9 ms**, `ocean_mask` 0.13 ms on
both sides, and solved heights bit-identical over 31,607 points with noisy
measured inputs. **Still open:** `unmeasured_profiles` builds the same network
twice (`solve`, then `structures`); sharing one saves another ~6 ms a cell,
and atlas's grading (`pipeline/terrain_grade.rs`) calls both as well.

## Batching the bank field's building fills — tried, measured, reverted

`water_shore_field`'s second pass fills all 8,552 building footprints into the
bank mask **one `fill_path` at a time**, which is exactly the shape the paint
loop was fixed for in August (118,905 allocations → 1,137, at ~4 µs of
per-call tiny-skia setup per feature). Batching them into shared paths was
built and measured: **34.6 ms → 29.9 ms**.

It was reverted, because an equivalence test against the per-feature
rasterisation failed and kept failing:

- `EvenOdd` across a batch XORs two overlapping footprints back to background.
- `Winding` fixes that, but needs the batch to agree which way round solid is,
  and the source does not guarantee ring orientation. Imposing orientation per
  ring did **not** fix it either.
- What remains is irreducible: where two buildings share a wall, their
  coincident edges run opposite ways and cancel, so a pixel centre lying
  exactly on that wall comes out unpainted. Filled separately, each polygon
  claims it.

Three texels of 65,536 on the fixture, always in the direction of *erasing*
built ground. Small, but this mask feeds the reflection term, and 4.7 ms on a
30 ms stage does not buy a mask that quietly loses buildings. The test that
caught it was written because the existing bank-field test only asserts that
*some* texel reads zero and *some* reads the far value — both still true of a
mask with holes punched in it.

**The same construction is live in the paint loop** (`flush_batch` fills
buildings `Winding`), whose comment records a max channel delta of 8/255
against the per-feature raster and attributes it to anti-aliased edges. That
attribution is worth re-checking against the coincident-wall case above before
it is relied on.

## What the parallelism rests on, and what it costs

Three stages are threaded now and the argument is not the same in each.

**`road_stitch`** is exact. A ribbon is cut against the *uncut* geometry of its
superiors, never a running union, so no result feeds another — the same fact the
module doc gives for why walk order cannot matter. Two splits exist because the
callers are shaped differently: the roads are 1,214 ribbons of ~18 triangles
(split across ribbons), the fill sheets are four ribbons of thousands (split
inside one). `stitch_is_the_same_cut_split_across_threads` compares both against
a forced-serial run byte for byte.

**The shore field's second pass** is exact for a narrower reason: the y-sweeps
carry a running minimum between rows and are left serial, while the parabola
envelope reads and writes one row with row-local scratch.

**The raster is not exact**, and that is the one to know about. Bands change the
magnitudes coordinates are computed at, so f32 rounds a few edge samples
differently: up to 8/255 on isolated anti-aliased pixels, which is the figure
`flush_batch` already records for the paint batching. Bleed rows remove the part
that is *clipping* (2,443 differing bytes → 312) and no amount removes the rest
(256 rows of bleed still leaves 43). The band count is a constant rather than
the core count, so at least the basemap does not vary between machines.

Two instrument consequences, both live traps. `perf::paths` and `perf::passes`
are **thread-local**, so a banded render records most of its work where the
measuring code cannot see it — every budget and hatch that reads them goes
through `render_mvt_serial` instead. And the 1/64 overzoom row stopped being a
usable control the moment banding touched it.

## Threading this crate breaks the web build, and the native suite cannot see it

Adding `std::thread::scope` behind `#[cfg(not(target_arch = "wasm32"))]` broke
`wasm32` twice in one change, both times invisibly to 1,647 passing tests.

**A dead branch still has to resolve.** `cfg` strips items *before* name
resolution and knows nothing about reachability, so calling a
`cfg(not(wasm32))` function from inside an `if let` whose condition is bound to
`None` on wasm is still `error[E0425]`. Put the *call* behind the cfg, not just
the value it branches on — or give the serial path a name both arms can reach.

**And this crate builds `-D unused`**, so anything left unreachable when the
threading is stripped — a thread-count ceiling, a merge helper, a band count —
is a hard error on wasm and nothing at all natively.

Run the lane before believing a threading change. It is **nightly with specific
flags**, and a plain `cargo check --target wasm32` is worse than useless here:
`-p cartopolis_geo` alone resolves a different feature set and dies inside
`uuid`, which reads exactly like your bug and is not.

```bash
rustup target add wasm32-unknown-unknown
RUSTUP_TOOLCHAIN=nightly \
RUSTFLAGS="--cfg=web_sys_unstable_apis -C target-feature=+atomics,+bulk-memory,+mutable-globals,+simd128" \
cargo check --locked -p cartopolis --no-default-features --features audio \
    --target wasm32-unknown-unknown
```

The target is not installed in the agent container by default and `rustup
target add` fails as the agent user (the download cache directory is missing);
it installs as root through the host, `ssh host "docker exec -u root
viberfox-agent …"`, with `RUSTUP_HOME=/usr/local/rustup`.

## Work that is still on the main thread

Tile-stage timings above are worker time. These are not, and no hatch here
measures them:

- ~~**`build_mesh`, called from `update_map_surfaces`**~~ — **moved to the
  surface worker 2026-09-23.** It needed no second stage: a terrain bake retears
  the ring and resets the stream, so a job handed the field at dispatch can never
  deliver a cell placed on a superseded one. Measured cold over Groningen at
  600 m on `deck-1`: before, each retear froze the main loop at 3–4 s a frame
  for five frames running (8 watchdog stalls, 12 slow frames, **92 s** launch to
  settled); after, 0 stalls, 1 slow frame (the startup one), **58 s and 56 s**
  over two runs, identical capture. The work did not get cheaper — `surfaces`
  worker time went 26 s → 95 s in the dev profile — it stopped blocking
  everything else. Two consequences: the worker places a cell against *its own*
  tunnels only (the main thread saw whichever neighbours happened to have
  landed), and `RoadMask` is fed the road triangles picked out **before**
  refinement, or marking would pay for every vertex the refinement adds. Decks
  still stand on the ground on the main thread, since their height arrives
  there.
- `RoadMask::mark_cell` still rasterises road triangles into the grid there
  (the dilation moved to the worker on 2026-09-21, the marking did not).
- `RoadMask::for_gen` clones the whole 4.2 MB mask per bake; an `Arc` removes
  the copy.

Two vectorisation candidates, both now measured as hot: `dilate_mask` over
4.2 M `bool`s is a **bitset** problem (shifts and ORs, 525 KB instead of
4.2 MB), and `road_grade::flatten` is 24 four-neighbour relaxation passes over
~1.9 M samples. `road_stitch`'s candidate filter is already ~8 ns per test and
tiny-skia vectorises internally; neither is worth hand-tuning.

**A GPU compute path is not obviously safe here** and the reason is in
`docs/notes/device-loss-strands-the-swapchain.md`: every per-frame cap in the
streaming path exists because upload and mesh churn has lost the device on an
Intel integrated GPU and on an Adreno 840, a lost device is terminal in this
client, and a readback would contend with the same worker pool that already
starved the screenshot path once. `road_stitch` being order-independent by
construction makes CPU parallelism the cheaper first move.

## The instrument is the actual lesson

A hundredfold regression reached `main` with every test green, because:

- `perf_budgets.rs`'s **allocation budgets stop at `render_mvt_sized`**. No
  tessellator has one, and the whole regression is downstream of the last gate.
- `print_tile_pipeline_time` **stopped at `tessellate_roads`**, so the stages
  added on 18–19 September were timed by nothing at all. `cut_under_roads`
  became the most expensive thing in the pipeline without ever printing a
  number.
- The *one* budget constant ever restated upwards in the file's history moved by
  **ten allocations**. The budget file was clean throughout.

`cut_under_roads` and `build_tunnels` have rows now. **A new stage on the
per-cell worker owes itself a row in that hatch in the same commit**, or it is
invisible in exactly the way these were.

## Two main-thread costs that are not in any of this

Tile-stage timings are worker time and show up as streaming latency. These were
main-thread and show up as stutter, which no hatch here measures:

- `Tunnels::covered_weight` walked every tunnel in every cell × every segment,
  with an early exit that only fires *inside* a tunnel — so a vertex near
  nothing was the worst case. It is the crease field, sampled 3× per edge test
  across 5 refinement passes plus twice per vertex. Fixed with per-cell and
  per-tunnel bounding boxes.
- `RoadMask::mark_cell` dilated the **accumulated** mask, so the road margin
  compounded across cells (46 % of a 6.4 km box marked as road) and did six
  full 2,049² passes per cell on the main thread. Fixed by making `mark_roads`
  pure and widening once, on the worker, in `bake`.

## Running any of it

```bash
cargo test -p cartopolis_geo --release print_tile_pipeline_time -- --ignored --nocapture
cargo test -p cartopolis_geo print_tile_pipeline_cost -- --ignored --nocapture
```

`--release` is not optional for the timings: the crate is at optimisation level
1 in the dev profile and the hatch prints a warning saying so. Read `min`
against `median` on every row — a wide gap means the box was busy and the run
should be retaken. There is no `perf` and no `valgrind` in this container and
`perf_event_paranoid` is 4, so sampling profilers are not an option; phase
timing via `perf::passes` and the allocation counters are what there is.
