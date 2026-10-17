---
topic: streaming-order-and-residency
triggers: [fetch_gate, cell_source, spawn_geometry_task, load_control, take_shed, "shed", "CellStream", "Dispatcher", request_ranked, ring_distance, resident_bytes_target, radius_bias, "streaming radius", "cell bundle", door_detail, "Detail::Far", fetch_gate.rs, cell_source.rs, platform/stream.rs, platform/work.rs, load_control.rs, coverage.rs, tile_loader.rs, map_geometry.rs, lod22.rs]
updated: 2026-10-06
---

# What decides the order things stream in, and how much of them stays

Four seams, added 2026-09-08. Each answers a question that had no owner before,
and the reason each is written down is that the obvious simplification of it is
wrong.

## The order was decided by Bevy's system order, and that is not a scheduler

Twelve `CellStream`s bound their own concurrency. The aggregate was about
**fifty-five** simultaneous jobs against an `IoTaskPool` four threads wide on a
handset. Natively `http::fetch` is a blocking call, so the surplus queued inside
the pool, first come first served, where nothing could reorder it — and which
streamer got there first was decided by which Bevy system happened to run first
that frame. The cell under the camera routinely waited behind coarse clipmap
tiles from eight kilometres away.

`platform::fetch_gate` is a global cap plus a **reserve** that only near work may
spend, and `Dispatcher` orders each queue by the same rank. The rank is Chebyshev
ring distance in cells, which is comparable across streamers even though they cut
cells at different zooms.

**A tight global cap was rejected.** Most jobs are a cache read that never touches
the network, so a cap near the pool's width would slow a warm start to fix a cold
one. The cap is generous (four per io thread, floor 16); the *reserve* is what
does the work.

**`map_stream` is deliberately only two-banded** — near or not — rather than fully
distance-ranked. Its spawn order is coarse-level-first on purpose, because a
blurry whole ground beats a sharp patch of one, and that decision was made against
a measurement. Ranking all 424 tiles by distance would overturn it. Equal ranks
keep arrival order, so the coarse-first sequence survives inside both bands.

## A radius is a proxy for residency, and the proxy is what was being bounded

`quality::handset_guarded`'s table measures 991 MB → 371 MB over Groningen and
ends by saying the honest thing: *and therefore the device survives* is not
something it can claim. It cannot, because a ring of a given size holds whatever
the ground under it happens to hold, and the ground varies by more than the rungs
do.

`systems::load_control` closes the loop with the number `upload_meter` was already
measuring: `Budget::resident_bytes_target` is a target the ring is steered toward,
not a setting anything reads. Tighten after 3 s over, relax after 30 s under 60 %
of it, at most two steps, never less than a 15 s dwell between changes — because a
change retears rings in every streamer at once, which is the mass-churn pattern
`VK_ERROR_DEVICE_LOST` comes from. The asymmetry is deliberate: over budget is a
device about to be lost, under budget is a view that could be a little richer.

**It is off under the shot harness**, unlike `stream_ledger`, and the difference
is what each does when it acts. A deferred upload arrives a frame later and the
settle test waits for it; a narrower ring means content never fetched at all, so
two runs of `cargo shots` would differ for a reason no image could show.
`radius_bias` is in `--dump-state` so a capture states what it was given.

**It has a cheaper move than the ring now.** Since 2026-09-11 `lod22` may hold
the meshes of cells that have left its ring
([keeping-what-was-expensive.md](keeping-what-was-expensive.md)), and those bytes
count toward the same residency figure while being drawn by nothing — so the
controller sheds them first (`SHED_AFTER_S`, 1 s, outside the dwell) and only
narrows the ring if that was not enough. Every other thing it could free is on
screen; this is the first that is not.

## Ring distance is a poor detail axis for a small ring, and that is the whole rule

A z14 cell is about 1.5 km at Dutch latitudes. "One ring out" therefore does **not**
mean "far away": a rank-1 cell is *adjacent*, and its near edge is under your feet.
Only from rank 2 is the whole cell at least a cell's width away.

So `load_control::Detail` is worth applying only to a streamer whose ring has a
rank 2 at all. `lod22` (radius 2) uses it, for door panels; `map_geometry`
(radius 1) has no such rank and does not. The two things this rules out, both of
which looked obviously right first:

- **Grass and shore-field extraction on outer `map_geometry` cells.** The grass
  mask is 96 m and the shore field feeds water shading, so both look like pure
  waste two kilometres out — but at radius 1 every non-centre cell is adjacent,
  and its near edge is where you are standing.
- **Anything with a rebuild on every cell crossing.** A cell's detail is baked
  into its merged mesh on a worker, so a promotion is a rebuild. `lod22`'s door
  boundary sits one ring inside the edge precisely so a promotion needs the camera
  to cross a whole cell boundary — the same event that brings five new cells into
  the ring anyway — and it promotes at most one cell per frame.

A demotion is never acted on: a cell drawn with more detail than it now needs
costs a few thousand triangles, and rebuilding it to take them away pays the
expensive half of the trade for the cheap half of the benefit.

## A cell's inputs download side by side, and there is no bundle

`map_geometry::spawn_geometry_task` needs up to nine documents before it can mesh
a cell — the street tile, the BGT objects and paving, the crop parcels, the city
register, the atlas decks, the nav marks and the level crossings. Natively
`http::fetch` is a blocking `ureq` call with no await point, so awaiting them in
turn (or `zip`ping them, which is only presentation there) ran them end to end: a
round trip each before the mesh could start. Since 2026-09-26 each is its own
`IoTaskPool` task and the job awaits the handles, so they overlap; what bounds
that is the io pool's width. The marks and crossings still start after the tile,
because whether to ask at all is read from its body.

A `/cell/14/{x}/{y}.json` bundle that prefilled three of those parts in one
request was built on the client and **never on the host** — atlas wrote no such
file, so every start spent nine 404s on it (Deck, 2026-09-26) before the support
flag latched. With the parts overlapping it would buy one request instead of
three, not a shorter wait, so it was removed rather than built.

The LoD2.2 shells, the BAG attributes and the LoD3 models are not part of this
job: each has its own ring, concurrency bound and retry policy, and none of them
is holding up the ground.

## The two per-cell seams are different on purpose

`platform::cell_source` is the **additive inline** shape — the layer's absence is
the map as it was before it existed, so a failure is forgotten and the answer is
`Option`. `platform::stream::CellStream` is the **streamed** shape, where a failure
feeds an attempt count, a backoff and a cooldown, so the answer is `Result`.
Flattening one into the other would silently delete a retry policy.
`systems::municipal` is in neither: its key and URL are functions of which register
and which source, and every `CellSource` field is a bare function pointer so the
descriptor can be a `const`.

## What a launch waited on before it fetched anything (2026-10-06)

Three things held the opening ring back that had nothing to do with its own data:

- **The coverage manifests were asked for on every launch, and the map waited.**
  About ten `coverage.json` files decide which trees a cell is asked for; the
  basemap holds every tile (even ones finished on disk) and the ground holds every
  cell until the BGT, crop and roof answers are in (3 s cap each), and the two
  3DBAG manifests were asked one after the other. Each is now kept on the device
  (`coverage::stored_key`) and answers from there at once; the host is still asked
  right after, and only an answer that **differs** moves the revision
  (`CoverageSlot::revision` counts distinct answers now, not 0/1), so the usual
  launch rebuilds nothing. A host that answers 404 is kept as `{}`.
- **Each basemap tile rebuilt its source's overlays.** Up to 64 tiles share a z14
  source, and each re-read and re-parsed the crop parcels, the land cover and the
  paving under it (2.3 MB of JSON on the densest cell) before it could even look up
  its finished image on disk. `tile_loader::source_overlays` builds them once per
  source, held by `Weak` and single-flight.
- **Parked jobs held their slots.** Over graded ground every ground and building
  job parks after downloading until the terrain bakes, keeping its in-flight slot,
  so the ring's last cells could not start downloading. The caps are the ring
  (`SURFACE_MAX_IN_FLIGHT` 9, `LOD22_CONCURRENCY` 9), the terrain tiles go eight at
  a time and the water levels sixteen. `lod22`'s four inputs download side by side
  as `map_geometry`'s already did.

Raising the terrain concurrency alone made the field bake before the last water
levels were in (the stream dispatches a round per frame), and the late levels then
re-baked it and rebuilt the ground cells over water; the water concurrency is what
removed that second bake.

Measured on `vps-1` (llvmpipe, dev build, 320×200, warm cache, Grote Markt at
600 m, wall seconds after the camera was placed until each layer went quiet; the
box was shared, so read the direction, not the digits):

| | `main` (two runs) | this change (two runs) |
|---|---|---|
| terrain | 27.6 / 13.3 | 5.3 / 5.5 |
| ground cells | 65.4 / 31.4 | 12.9 / 12.2 |
| LoD2.2 buildings | 80.6 / 44.8 | 21.3 / 23.1 |
| map image | 143.7 / 188.7 | 93.3 / 85.4 |

At ~1.5–2.5 s a frame here, every per-frame dispatch round is expensive, which
exaggerates anything that saves rounds; on a real device expect the same order and
much smaller numbers.
