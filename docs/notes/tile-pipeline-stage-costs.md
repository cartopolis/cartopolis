---
topic: performance
triggers: [tessellate_roads, road_stitch, cut_under_roads, stitch_some, print_tile_pipeline_time, perf_budgets.rs, "tile is slow", "map is slow", "streaming is slow", "vector tile performance", Buckets, SPANS_PER_TRI, MIN_BUCKET_M, covered_weight, RoadMask, dilate_mask]
updated: 2026-09-21
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
| `tessellate_roads` | **3.1 ms** | **103.1 ms** | 82.5 ms |
| `tessellate_surfaces` | 0.8 ms | 14.0 ms | 10.6 ms |
| `cut_under_roads` | not timed | not timed | **124.8 ms**, ×2 per cell |
| `build_tunnels` | not timed | not timed | 3.0 ms |
| `water_shore_field` | 19.4 ms | 35.1 ms | 34.6 ms |
| render 512² whole | — | 68.9 ms | 69.5 ms |
| render 256² 1/64 sub | 8.2 ms | 8.4 ms | 8.1 ms |
| `ocean_mask` 256² | 0.3 ms | 0.3 ms | 0.3 ms |

**The last two rows are the method, not decoration.** They are stages nothing in
the window touched, and they read the same across a month and across the fix, so
the large movements above them are the code and not the box. Quote a stage
timing here only with a pair of unchanged rows beside it.

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
hundreds of road triangles — so a finer grid does not remove them. The fix is to
dice the subject triangles to the bucket grid before cutting, so each boolean
meets a handful of blockers instead of seventeen. **That was deliberately not
done on 2026-09-21**: it changes how a cut sheet is triangulated, which has to
be argued against `road_stitch`'s no-hairline-cracks invariant, and this
container's software rasteriser cannot judge whether a crack opened. It needs a
real graphics card.

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

## Work that is still on the main thread

Tile-stage timings above are worker time. These are not, and no hatch here
measures them:

- **`build_mesh`, called from `update_map_surfaces`** — the terrain
  refinement, the height displacement and a full normal recomputation, for
  every surface of every cell, two cells a frame. Pure buffer work; it is on
  the main thread only because the height field arrives after the worker job
  has returned flat meshes. Moving it wants a second worker stage gated on the
  field. **The largest main-thread item in the map pipeline.**
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
