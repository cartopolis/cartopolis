---
topic: streaming
triggers: [workers.rs, "workers::submit", submit_ranked, submit_heavy_ranked, Urgency, urgency_for_ring, AGING_JOBS, fetch_gate, Dispatcher, reprice, wanted_shared, tile_rank_at, "tiles queue", "tile stall", "unseen tiles keep loading", wait_us]
updated: 2026-09-29
---

# Where a tile waits, and who decides the order

A tile passes four queues on the way to the screen. Only the last one used to be
unordered, and it was the one that hid the others' work.

1. **`CellStream`'s `Dispatcher`** (`platform/work.rs`): per-stream, ranked, capped by
   `max_in_flight` (map tiles: 8). Lowest rank first, arrival order among equals.
2. **`fetch_gate`** (`platform/fetch_gate.rs`): one global cap across every streamer,
   with a reserve of 4 that only rank `<= NEAR` (1) may spend. Rank is Chebyshev ring
   distance from the camera's cell.
3. **The IO task pool**: fetch and storage read. Not ordered, and not the bottleneck.
4. **`platform::workers`**: the CPU stage (decode, scan conversion, mip chain,
   meshing). A FIFO pool that cannot be reordered from outside.

**The finding (2026-09-28, from reading, not from a capture).** Priority was honoured
at 1 and 2 and thrown away at 4. A tile under the camera and a tile the camera had
left were the same to the pool: whichever job was submitted first started first.
Eight map-tile jobs cannot flood it alone; the contention is the other streamers
sharing it.

## What replaced it

`workers` keeps waiting jobs in its own queue and hands the pool **no more than its
width**, so the order is decided in `pick`, not by arrival. Urgency is `BASEMAP` 0,
`NEAR` 1, `NORMAL` 2 (the default of plain `submit`), `FAR` 3. `urgency_for_ring` maps
a ring distance onto them.

- **Ageing counts submissions, not time** (`AGING_JOBS` = 48): `std::time::Instant`
  panics on wasm, and a counter is deterministic in a test. A job's effective urgency
  drops one step per 48 later submissions, so `FAR` work cannot starve.
- **The heavy lane still applies first.** A heavy job is skipped when the lane is full,
  whatever its urgency, so ordinary jobs keep their reserved threads.
- **A running job is never interrupted.** A heavy job that has started holds its thread
  for seconds; urgency only chooses who starts next.
- `wait_us` per label in `workers::stats` is now a true queue wait. Before, it also
  contained the pool's own FIFO queue and could not be told apart from a slow job.

Who states an urgency: map tiles `BASEMAP`; `surfaces` and `lod22` by ring;
`terrain_field` and `terrain_fine` `NEAR` (ground and building cells wait on the
field); `globe` `FAR`. Everything else is `NORMAL`. **That is a choice made without a
measurement** — look at the wait column for the other labels before ranking more.

## Two more rules in `map_stream`

- **A retired tile is not decoded.** `forget` frees a tile's slot at once but cannot
  stop its task. `MapStream::wanted_shared` is the clipmap's current wanted set, read
  by the fetch task and again by the worker; a tile that left it (or whose generation
  is stale) returns before the decode. Safe because a late result for an unknown key
  is ignored anyway.
- **Queued tiles are re-ranked when the view moves** (`Dispatcher::reprice`). Rank was
  fixed when a tile was asked for, and equal ranks keep arrival order, so a tile that
  became visible later queued behind ones wanted a second ago. `tile_rank_at` gives the
  near 3x3 `NEAR` and everything else `NEAR + 1 + index in spawn_order`, which keeps the
  measured coarse-first, nearest-first order but re-derives it for the current view.
  Only the queue is repriced; started jobs are untouched.

## Not done

- **In-flight fetches are not cancelled**, only their decode is skipped. Cancelling the
  request would save bandwidth, not CPU.
- The other twelve streamers still state no urgency.
- Nothing here has been timed against a cold fill. It is expected to help
  `time-to-first-map` and to leave a warm start alone; check both.
