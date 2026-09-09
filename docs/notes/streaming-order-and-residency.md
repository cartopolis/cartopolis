---
topic: streaming-order-and-residency
triggers: [fetch_gate, cell_source, cell_bundle, load_control, "CellStream", "Dispatcher", request_ranked, ring_distance, resident_bytes_target, radius_bias, "streaming radius", "cell bundle", door_detail, "Detail::Far"]
updated: 2026-09-08
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

## A per-cell bundle is a cache prefill, not a data path

`map_geometry::spawn_geometry_task` awaited three of our own host's datasets one
after another before it could mesh anything. `systems::cell_bundle` asks for all
three at once and writes each part into the storage cache **under the key that
part's own loader already looks for** — so no loader, parser or coverage gate
changed, and a host that has not been redeployed costs one 404 per cell until the
support flag latches.

**A part the host omits is not cached at all.** An empty document written under a
real key is never re-fetched, so "no trees in this cell" would become permanent
rather than merely current. The same rule is why nothing is cached on a failed
answer anywhere in `platform::cell_source`.

The LoD2.2 shells, the BAG attributes and the LoD3 models are deliberately **not**
in the bundle: each has its own ring, concurrency bound and retry policy, and none
of them is holding up the ground.

## The two per-cell seams are different on purpose

`platform::cell_source` is the **additive inline** shape — the layer's absence is
the map as it was before it existed, so a failure is forgotten and the answer is
`Option`. `platform::stream::CellStream` is the **streamed** shape, where a failure
feeds an attempt count, a backoff and a cooldown, so the answer is `Result`.
Flattening one into the other would silently delete a retry policy.
`systems::municipal` is in neither: its key and URL are functions of which register
and which source, and every `CellSource` field is a bare function pointer so the
descriptor can be a `const`.
