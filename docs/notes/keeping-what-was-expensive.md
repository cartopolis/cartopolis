---
topic: keeping-what-was-expensive
triggers: [warm.rs, "WarmStore", "WarmSize", warm_bytes, warm_gpu_bytes, warm_ceiling, warm_gpu_ceiling, CARTO_WARM_MB, CARTO_WARM_GPU_MB, take_shed, SHED_AFTER_S, "retained", "warm_tile_hits", "warm_cell_restores", "map reloads slowly", "revisit", "cache the mesh", platform::warm, load_control.rs, map_stream.rs, lod22.rs, quality.rs]
updated: 2026-10-17
---

# Caching the finished thing, not the bytes it was made from

Added 2026-09-11. Two caches already sat in front of the streamers before this
and both held **source bytes**: `tile_source`'s 32 MiB in-memory table and the
512 MiB disk tree `utils::cache` bounds. Everything derived from those bytes was
dropped the moment a cell left the ring.

## The measurement that says why more of the old kind would not have helped

[tile-raster-cost.md](tile-raster-cost.md) already had it, in the cold/warm pair
over Groningen at 420 m: **a fully warm disk cache buys 2 s of a 27.8 s fill.**
The other 26 s is scan conversion — ~48 ms per 256² tile, ~68 ms at 512², plus
~19 ms of shore field per surface cell — and no amount of byte caching touches
it, because the bytes were never what was slow. `map_tiles` alone is ~29 s of the
~38 s of worker time in that run, and `lod22` is most of the rest.

So the question was never "cache more"; it was "cache further down the pipeline".

## Two stores, because there are two scarcities

`platform::warm::WarmStore` is one byte-bounded LRU type used twice, and the two
uses are not variations of each other:

| | `map_stream` | `lod22` |
|---|---|---|
| holds | the rasterised mip chain | the `Handle<Mesh>` |
| costs | system memory | **graphics** memory |
| bounded by | `Budget::warm_bytes` | `Budget::warm_gpu_bytes` |
| on a handset | small (128 MB cap) | **off** |

The ground tiles have to be held as bytes because `TILE_TEXTURE_USAGE` is
`RENDER_WORLD` only: once a tile is uploaded there is no CPU copy left, so the
one moment it can be kept is just before it is moved into the `Image`. That costs
one ~1.4 MB memcpy per tile upload, against the 48–68 ms of scan conversion it
saves if the tile is ever wanted again — a ratio of roughly 350:1, and the copy
is skipped entirely on a rung that holds nothing.

The buildings are the opposite shape and it is worth knowing why, because the
obvious symmetry is wrong. Holding a LoD2.2 cell as *data* would mean cloning its
merged meshes, and Groningen's densest cell is **63 MB** — a visible stall on the
main thread, every time a cell is retired, to buy a return that may never happen.
Holding the handle instead copies nothing at all and costs no system memory,
because the mesh's CPU-side copy is already gone.

## The one cache here that spends graphics memory, and the three rules that make it safe

Retained meshes are the only thing in the client that holds the quantity
`Budget::resident_bytes_target` bounds — the quantity that lost an Adreno 840 its
Vulkan device on 2026-08-19 with every rate guard in force. So:

1. **It is inside the target, not added to it.** `upload_meter` lowers its
   residency figure only on `AssetEvent::Removed`, i.e. when an asset is really
   dropped, so a retained mesh is counted exactly as a drawn one is. Nothing had
   to be taught about the pool for this to be true; it already was.
2. **It is the first thing shed under pressure.** `load_control` used to answer
   being over budget by narrowing the ring — taking away map the person is
   looking at. It now drops the pool at `SHED_AFTER_S` (1 s) before considering a
   radius step at `TIGHTEN_AFTER_S` (3 s), outside the dwell that rations radius
   changes, because dropping cells nobody is drawing retears nothing. This makes
   the controller strictly better off: it finally has something cheap to free.
3. **It is zero on every handset**, forced in `quality::handset_guarded` rather
   than merely reduced. Every other guard there takes the smaller of two numbers;
   this one takes none of it, because no amount of a faster return journey is
   worth a crash report.

### The shed rule got written wrong first, and the test is what said so

The first version reset `over_since` when it shed, reasoning that the shed
deserved a fresh countdown. It does not: a machine whose pressure is the **live
ring** rather than the pool then re-sheds an already-empty pool every second, for
ever, and never reaches the tighten at all — the map never narrows and the device
stays over its ceiling. What actually gives a shed its chance is the residency
*falling*, which ends the run of over-ceiling samples on its own. The shed is now
once per run of pressure and leaves the countdown alone
(`pressure_that_outlives_the_shed_still_tightens` is the regression).

## What this does not fix

**Ground you have never visited.** Moving fast through new territory still pays a
full rasterise per tile and a full parse-and-merge per building cell; nothing
here is a prefetch. The frame-rate dip on genuinely new ground is unchanged, and
the fix for it is a different change — streaming ahead along the direction of
travel.

## Rejected: keeping cells drawn one ring further out

The first plan for the boundary-churn half of this was a retirement margin — keep
`map_geometry`'s cells alive one ring past the request ring, the way `lod22`
already keeps one. Measured against what it would cost rather than what it would
save: `map_geometry` runs at radius 1, so a one-cell margin takes the drawn set
from 9 cells to 25, which is close to doubling the scene's ground draw calls
(662 draws total over Groningen, of which the surfaces are a large share). That
is the wrong direction for a change whose motivating complaint was frame rate.

The warm store gets the same benefit for the return trip and draws nothing extra,
which is why the margin was dropped rather than added beside it. If it comes back
it should be as *hidden* entities with their own residency budget, not as drawn
ones.

## `map_geometry` is not wired to either store, and that is a measurement not an oversight

Its per-cell work is ~23 ms (tessellation 0.8 + 3.1 ms, shore field 19.4 ms)
across 9 cells, against `map_stream`'s ~50–68 ms across hundreds of tiles. It
would need the same by-value clone the buildings avoid — meshes, shore field,
labels, places, decks, grass, roads — for the smallest of the three shares of the
pipeline. Do the source-document caching for the per-cell JSON layers first if
this area is revisited: `lod22`, `building_facts`, `surveyed` and `street_detail`
all go straight to `storage::get_string` and a serde parse on every cell entry,
with no memory cache of any kind in front of them.

## Reading a run

`--dump-state` carries both pairs, and all four are machine-independent so they
may be gated with `--expect`:

- `warm_tile_hits` / `warm_tile_mb` — ground tiles re-uploaded rather than
  re-rasterised, and the system memory that is costing.
- `warm_cell_restores` / `warm_cell_mb` — building cells re-spawned rather than
  rebuilt, and their **share of** `gpu_resident_kb` (not an addition to it).

`CARTO_WARM_MB` and `CARTO_WARM_GPU_MB` override the two budgets (`0` is off),
which is the A/B for either store without a rebuild.

A `cargo shots` manifest run is the case these were added for: six viewpoints
over one city in one launch, where `warm_tile_hits` tells "the second shot
settled quickly" apart from "the second shot settled quickly *because* it
re-rasterised nothing".
