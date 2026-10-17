---
topic: one-pass-cells
triggers: [CoverageSlot, coverage_revision, fetch_optional, "surface ring retear", "lod22 ring retear", "terrain field bake queued", ROAD_SETTLE_S, WATER_SETTLE_S, terrain_revision, bgt_revision, bag_revision, "rebuilt", "retear", "slow start", "map loads slowly", coverage.rs, platform/http.rs, terrain.rs, map_geometry.rs, lod22.rs]
updated: 2026-09-23
---

# A cell should be tessellated once, and was being tessellated three or four times

A `map_geometry` cell job is the most expensive thing this client runs on a
worker. Measured cold over Groningen at 600 m on the Deck (a real GPU), 2026-09-21,
from `--dump-state`'s `worker_split`:

| stream | jobs | busy | share |
|---|---|---|---|
| `surfaces` | 31 | **111.6 s** | 54.7 % |
| `map_tiles` | 375 | 45.9 s | 22.5 % |
| `lod22` | 59 | 23.0 s | 11.2 % |
| `terrain_fine` | 1 | 14.0 s | 6.8 % |

The ring is **9 cells** and it ran **31 jobs** — about 3.6 s per cell, paid 3.4
times over. `lod22`'s ring is 25 cells and it ran 59. This is the number to watch,
and `tile-raster-cost.md`'s "rasterisation is three quarters of the pipeline" is no
longer true of a whole fill: it was measured in August, before paving, crops,
tunnels, the BGT street objects and the traffic network joined the cell job.

## Why it happened, from the log rather than from reasoning

`map_geometry` and `lod22` each rebuild their **whole ring** when one of a few
revisions moves — the terrain field, the BGT/crops coverage, the BAG/LoD3 coverage.
Each of those is a fact that arrives on its own clock. Nothing said *when* they
arrived, so `surface ring retear` / `lod22 ring retear` / `terrain field bake
queued` were added to say it. The first cold run with them:

```
+25 s  terrain bake #1              surfaces retear at 7 cells, lod22 at 5
+67 s  crops coverage manifest      surfaces retear at 9 cells
+70 s  LoD3 coverage manifest       lod22 retear at 25 cells
+90 s  water levels → bake #2       surfaces 9, lod22 25
```

**Two of those four were a bug, not an ordering problem.** The crops and LoD3
trees are not served by this host, and it says so promptly:

```
bgt  200 0.07s     brp   404 0.07s
bag  200 0.07s     lod3  404 0.05s
```

`CoverageSlot` sent a 404 down the same ladder as a dead socket — `MAX_ATTEMPTS`
tries `RETRY_SECS` apart — so a 67 ms answer was not believed for **60 seconds**,
and `revision()` moved then. A minute into the fill, with the ring fully built, it
was torn up and re-tessellated for a fact the host had stated before the first tile
landed.

`http::fetch_optional` is the fix and the distinction is the whole of it: **a 404
is the host answering, a timeout is not having been able to ask**, and the two want
opposite retry policies. Only 404 and 410 latch; a 5xx is the host failing rather
than answering, and a 403 may be a misconfiguration that is fixed while the client
runs, so both keep the ladder.

The same run afterwards:

```
+1.0 s  crops coverage: no coverage on this host     surfaces retear at 0 cells
+1.1 s  LoD3 coverage:  no coverage on this host     lod22 retear at 0 cells
+2.0 s  every manifest has answered
+13 s   terrain bake #1                              surfaces retear at 8 cells
+59 s   terrain bake #2 (road mask 5 → 9)            surfaces retear at 9 cells
```

Every manifest-driven retear is now at **`cells = 0`** — the answer arrives before a
cell has been tessellated, so it costs nothing. `settled_s` for that capture was
**46.0 s** against the earlier **134.3 s**; do not read the whole of that gap as this
change, because the two runs were not served the same data (the second got no water
levels at all, so its fine terrain tier was 1.5 s rather than 14.0 s — see
`terrain-relief.md` on the ring index).

## What is left, and why it is not a bug

The terrain field genuinely cannot be known early: a cell bakes ground heights into
its vertices, and the AHN box is a 12.6 MB GeoTIFF. The upstream is not the problem
— measured against PDOK from the VPS the same day, **2049² is 12.6 MB in 4.7 s** —
it is that those megabytes share a link with ~54 MB of tiles, so the field lands
10–25 s in. Waiting for it before building anything would mean a city with no
roads, trees or labels for that whole time; building flat and correcting once is
the better trade and is what happens.

So **one rebuild per cold start is the floor while the relief is baked into
vertices**, and the second one that remains is a cycle rather than an arrival: the
field's road grading is fed by `RoadMask::mark_cell`, which is fed by the cells,
which are rebuilt by the field. Bake #1 runs against however much of the mask had
arrived by then, the rest of the ring marks itself afterwards, and `ROAD_SETTLE_S`
lets the grown mask through into bake #2. Breaking it needs the mask fed from
something that is not a finished cell — `tessellate_roads` over the parsed tile is
~3 ms against a 3.6 s cell job — which is the shape of the pre-pass, not done here.

The two bakes were at least made not to cost more than one each:

* `WATER_SETTLE_S` (2 s) mirrors `ROAD_SETTLE_S` for the measured water levels.
  `WaterLevels` bumps its revision per station — 2 → 51 over one fill — and every
  bump the field had not baked was a bake, hence a retear of every cell standing on
  it.
* A bake now **records the revisions it actually used**, not the settled ones it was
  allowed to bake at. Recording the settled numbers (which is what the road half
  did) makes the field report an older input than it used, so it bakes a second
  time over identical data the moment the settle passes.

## The direction that would make it genuinely one pass

Split the cell job at the seam the terrain actually touches. Everything expensive
in it — the parse, the earcut, the road raster, the paving simplification, the
shore field, the grass mask, the tree and furniture extraction — is independent of
the ground's *height*; only the vertex Y depends on it, plus the subdivision
`refine_for_terrain` does, and that depends on the field's **existence and
resolution**, not on its values. A cell tessellated already-refined could take its
heights afterwards without any of the rest being redone.

Not attempted here. It means holding a cell's CPU-side geometry until the field
settles, which is the cost `map_geometry`'s own comment rejected when the rebuild
was believed to happen once — and it happens once now, which is exactly when that
judgement becomes worth re-testing rather than assuming.

**What a retear costs changed on 2026-09-23**, which moves this trade: the
placement onto the terrain now runs on the surface worker, so a retear is worker
time rather than a frozen main loop (`tile-pipeline-stage-costs.md` has the
before/after). The rebuilds themselves still happen, once per bake.
