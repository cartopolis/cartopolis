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
