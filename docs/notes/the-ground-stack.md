---
topic: rendering
triggers: [ground_stack.rs, GroundLayer, depth_bias, depth_rung, y_offset, DEPTH_BIAS_STEP, road_rung_ceiling, "z-fight", "z fight", "bleeds through", "bleeding through", "draws over", "flicker", "shimmer", "layer order", "z-index", "surfaces flicker", "water through the grass", "grass through the road", tessellate_surfaces, road_stitch.rs, "coplanar"]
updated: 2026-09-15
---

# The ground stack: one list, and a sheet that does not overlap itself

Everything a map draws flat on the ground is coplanar with its neighbours by
construction. The rasteriser cannot separate coplanar surfaces — their depth
values agree to the last few bits and the winner is decided by rounding, which
re-rolls every frame the view matrix moves. So a map renderer needs two things,
and they are different things:

1. **An order**, for surfaces that genuinely overlap and must.
2. **No overlap**, everywhere that overlap is an accident of the data.

This tree has repeatedly had one and not the other, or has had the first written
down in several places that disagreed. Both halves live here now.

## Half one: the order is a list, not a set of numbers

`cartopolis_geo::ground_stack::GroundLayer` is an ordered `enum`, and the
declaration order *is* the ladder — a layer's rung is its discriminant. Both
denominations are derived from that one position:

* `depth_bias()` — depth-buffer ULPs, `rung × DEPTH_BIAS_STEP`, plus the whole
  road band for anything above the road.
* `y_offset()` — metres in the ten-millimetre window between the map tile's top
  face and the buildings' base, accumulated from each layer's `clearance()`.

Nobody writes a number down. That is the point.

### What it replaced, and what each of those cost

The order used to be stated in five places in three denominations:
`SurfaceGroup::depth_rung` (eleven hand-written rungs), `SurfaceGroup::y_offset`
(a second ladder in millimetres, written independently), `road_rung` (a third,
per vertex, inside a band the first had to leave room for), **six** free constants
in four client modules — zebra paint 10.5, route casing 18, route line 20, route
pins 22, street label 23, driver arrow 24 — and `assets/shaders/road_depth.wgsl`'s
hard-coded step.

Six, not the five anyone knew about. `navigation.rs` had a bare `rung(22.0)` for
the route's start and finish pins that was in no list, in no test, and in none of
the post-mortems; it turned up while replacing the constants either side of it. A
seventh could not exist now, because there is nothing to write down.

Every recurring failure in this area was two of those disagreeing:

| | |
|---|---|
| **Crossover** | The depth ladder grows with range (`1.19e-7 · bias · z`), the millimetre one shrinks as `1/z`. Where they ranked two roads differently the terms crossed in the middle distance and the winner flipped as the camera moved — reported as "part of the roads become invisible". |
| **Dissolution** | 24 µm is worth 2.0 depth units at 100 m, 1.0 at 200 m, nothing beyond. The entire within-class road ladder stopped meaning anything past a couple of hundred metres. |
| **Swallowing** | When the road band grew to 399 rungs its ceiling reached 1,916 depth units and climbed past all five hand-picked client constants at once. Every zebra crossing on a road wider than a residential street was drawn *underneath* the tarmac. |

None of those was a wrong idea. Each was somebody picking a number without being
able to see the other numbers.

### Why the tests enumerate

The test that should have caught the swallowed zebra crossing **existed and was
green throughout** — it checked four other things (bridge decks, canopy, trunks,
furniture) off a hand-written list. A list somebody maintains is a list somebody
forgets.

Every test in `ground_stack` and in `systems::road_depth` now walks
`GroundLayer::ALL`, and `the_list_is_the_ladder` pins `ALL` against the
discriminants so the list cannot fall out of step with the `enum` either. A flat
thing added tomorrow is checked tomorrow, by tests nobody edits.

### Where the millimetres go

The window is ten millimetres and the fights inside it are not equally visible.
Land against land is pale green against pale grey and can lose unnoticed; land
against water is dark against pale, and it is the one that came back as triangles
flickering up through a lake. So the land layers are packed half a millimetre
apart and the water takes seven — `clearance()` is where that judgement lives, and
it is a judgement about *contrast*, not about geometry.

## Half two: a layer may not overlap itself, because no ladder can help it

A ladder orders one layer against another. Two polygons of one layer share a rung
by definition, and the tile is full of them — a cemetery inside a park, a
playground inside a recreation ground, a village green inside a meadow all arrive
as separate `land` features resolving to `SurfaceGroup::Grass`.

**Measured on `crates/geo/tests/fixtures/groningen_z14.mvt`, 2026-09-15**, by
rasterising each group's own triangles at 1024² over the tile and counting
multi-covered texels:

| group | covered | covered twice | |
|---|---|---|---|
| Grass | 166,188 | 16,134 | **9.7 %** |
| Built | 825,757 | 25,283 | **3.1 %** |
| Forest | 48,906 | 137 | 0.3 % |
| Water | 34,594 | 86 | 0.2 % |

And across groups, where the ladder *was* load-bearing:

| | shared | |
|---|---|---|
| Forest under Grass | 9,321 | 19.1 % of the forest |
| Water over Grass | 8,101 | 23.4 % of the water |
| Built under Grass | 88,123 | 10.7 % of the built land |

`tessellate_surfaces` cuts all of it now, the same way and with the same machinery
as the road ribbons (`road_stitch`, whose module note carries the no-cracks
argument: one millimetre lattice, integer booleans, a blocker's own vertices).
Priority is the layer's rung, then the **smaller** polygon — the smaller feature is
the more specific statement about that ground, so a cemetery drawn inside a park
stays a cemetery — then the feature index, because the order has to be strict or
the cut takes both sides out and opens a hole.

Cutting across groups as well as within them costs nothing extra: same pass, same
index.

### What the cut costs, and the index bug it turned up

Measured on the same fixture and box, 2026-09-15, `print_tile_pipeline_time`:

| | before | after |
|---|---|---|
| `tessellate_surfaces` | 1.5 ms | **49 ms** |
| `tessellate_roads` | 534 ms | **~300 ms** |

The fills got 47 ms more expensive and the roads got 230 ms cheaper, because the
first attempt at the fill cut cost **317 ms** and finding out why fixed something
that had been quietly costing the roads nearly twice their run time.

`road_stitch`'s bucket index was a fixed 8 m grid — a little under the narrowest
carriageway plus its kerbs, which is right for a road triangle and absurd for a
landuse one. Earcut answers a polygon with as few triangles as it can, so a
residential block arrives as two triangles spanning half the tile, and each of
those was written into some 8,000 buckets and read back out of every one. The grid
is sized from the mean triangle extent now (`SPANS_PER_TRI`), with the 8 m as a
floor. It is an acceleration structure only — the separating-axis test still
decides — so the cut's output is unchanged and the road suite is untouched by it.

Two ground layers are **not** in this pass, because they are not built by it:
`Paving` (`crate::paving`, surveyed) and `Crop` (`crate::crops`, the BRP register).
Each is a single-source layer whose own extractor already promises a partition —
and the 2026-08-29 ghost pavements are what happens when one stops promising it,
which is why `parse_surveyed_paving` carries an exact-repeat drop. The ladder
orders them against everything else as before.

**Paving is cut against the roads, though, in a second pass of the same
machinery** (`road_stitch::cut_under_roads`, called from `map_geometry` once
`tessellate_roads` has answered). The paragraph above is about a layer overlapping
*itself*, which is still the extractor's promise; this is about it overlapping a
different source — see the relief section below. The roads go in as
blockers and come back untouched.

**`Crop` joined the partition on 2026-09-27** (`crops::cut_into_ground`, after
the road cut): every sheet above it is cut out of the parcels, and the parcels
out of `Built` and `Grass`. It had been left out on the reasoning that a parcel
and a carriageway do not share ground — true, and beside the point, because a
parcel and the `Grass` of OSM's `farmland` share *all* of it. The first capture
with relief on drew every field in blotches of the two.

**And so are the ground fills, through the same call.** That pair was described
here as settled and was not: `tessellate_surfaces` cuts the fills against each
other and against each other's groups, and the roads are built by a *later* pass
that needs the bridge decks first, so a landuse polygon and a carriageway had
never met. A residential block covers the street through it — that is what the
polygon means — so the overlap is not an accident of the data at all, it is the
data. Measured on `groningen_z14.mvt`, 2026-09-19:

| | shared with the road sheet | |
|---|---|---|
| Built under road | 207,098 | **67.1 % of the road's area** |
| Grass under road | 22,245 | 7.2 % |
| Forest under road | 5,219 | 1.7 % |
| Water under road | 46 | 0.0 % |

**About three quarters of every carriageway in the city had a second opaque
coplanar surface underneath it**, held apart by half a millimetre of `clearance()`
— which is the entire budget the land layers are packed into. It was the largest
overlap left anywhere in the stack, it was invisible for as long as the ground was
flat, and it is the one in the 2026-09-19 phone captures. `no_ground_fill_lies_
under_a_road` pins it, and it fails on the old code reporting the table above.

## What the cut does *not* retire

**The ladder is a backstop now, not the mechanism, and it is not optional.**

* The cut is **per tile**. A feature crossing a tile boundary is two features in
  two tiles that never see each other.
* A flyover and the road beneath it are two real surfaces in one place. Cutting
  one out of the other would be a lie about which is which; the rung is what holds
  them apart, and `road_stitch` deliberately exempts them.
* **A group cut out from under another may not be hidden on its own**, or a hole
  opens where the one above it used to be. No `DataLayers` toggle reaches the
  water or landuse groups today. One that did would have to exclude its group from
  the cut.

## The hole relief opens, and how much of it is closed

With the Height layer on, `refine_for_terrain` splits each mesh's *own* triangles
to `TerrainField::ground_edge_m()` and displaces the vertices. Two sheets over the
same ground agree exactly where they share a vertex and disagree between them by
the field's curvature across an edge — which on flat ground is nothing and on a
dike or a canal bank is decimetres, orders of magnitude past the millimetre window
and past any depth bias.

**It was reported on 2026-09-17**, from an Android build over Groningen's ring
road: the surveyed paving, the carriageways and the ground fills tearing through
each other in jagged polygons that follow triangle edges. The diagnosis above was
right, and the rule it states still holds — *if surfaces bleed only with Height
on, it is the refinement, not the ladder*. Three things were found underneath it.

**A handset is about four times worse than a desktop, and that is why this came
from a phone.** `quality::handset_guarded` forces `terrain_grid` to 513 on every
rung, so the field is sampled every 12.5 m rather than 3.125, and `ground_edge_m`
returns `sample_m × 3.9` = **48.75 m** rather than 12.5. Chord error goes as the
square of the edge, so against the ring trench's own measured gradient (it falls
from +5.5 m to −3.6 m across forty metres, ≈0.23) one sheet errs by 0.09 m fully
refined, 0.71 m at the desktop limit and **2.8 m at the phone's**. Two sheets
disagree by the sum.

**The crease rule made it worse before it made it better.** `09c0dc2` gave each
mesh a second, sag-driven splitting rule, so one sheet can now resolve a step
almost exactly while its neighbour, whose triangles fall elsewhere, still averages
across it. Its own commit body measures that recovery as 0.567 m → 0.027 m on one
quad — and that 0.54 m is exactly what two neighbouring sheets can now differ by.

**Two of the three torn layers emit unshared vertices.** `flat_fill` gives the
paving and the crop parcels three fresh vertices per triangle and `road_stitch`
re-emits every ribbon its junction cut touched, so an interior edge was two index
pairs over one piece of ground: tested twice, charged to `CREASE_VERTEX_BUDGET`
twice, and answerable *differently* for its two owners once the budget ran out
between them — a T-junction, reintroduced by the vertex layout. A dense cell's
paving is one mesh with one budget while the roads are a dozen meshes each with
their own, so about a quarter of it got no crease refinement at all, in triangle
order rather than any spatial order. That is the patchiness.

Closed on 2026-09-18, in three pieces:

* `refine_for_terrain` decides on the **quantised endpoints**, not the index pair.
  One geometric edge is one decision, charged once, and the answer is a pure
  function of the two endpoints and the field — so two different meshes sharing a
  boundary edge reach it independently and agree. Measured on the old key, one
  square of creased ground drawn twice came out **2.4759 m** apart depending only
  on whether its vertices were shared; `terrain.rs`'s
  `a_sheet_draws_the_same_ground_whether_or_not_its_vertices_are_shared` pins it.
* `MIN_GROUND_EDGE_M` follows the snapshot through `min_ground_edge_m`, as
  `ground_edge_m` already did. It was a constant cut for the desktop's field, so a
  handset refined to 1.5625 m against ground measured every 12.5 — eight times
  finer than any measurement, ~1000 vertices to take one quad to the floor.
* **Paving joined the partition** (`paving::cut_paving_under_roads`). It was the
  one pair the "remove the overlap" rule had been left off, and it is the pair in
  the captures.

What is **not** closed, and is the next thing to do if this is reported again: two
sheets that do not share an edge still approximate the same field on different
triangulations, so the disagreement is bounded by `CREASE_SAG_M` (0.15 m) per
sheet and not by anything smaller. Removing the overlap means the remaining
disagreement is usually invisible — nothing is drawn on top of anything — but a
fill and a road still meet along a cut boundary, and only the shared-edge case is
provably exact. The fix remains the one named here before: refine every ground
sheet against a **shared world lattice** aligned to the field's own sample grid,
with the subdivision level a function of the ground rather than of the mesh. What
it costs is the reason `MAX_GROUND_EDGE_M` is 12.5 m (peak single-asset upload
8 MB → 37 MB at 6.25 m), so it needs the ground meshes split by byte budget the
way `lod22::MAX_CELL_MESH_BYTES` does — that is the prerequisite, not the fix.

Two smaller ones also still stand. `ground_edge_m` takes the *larger* of 12.5 m
and 3.9 × the sample spacing, which is right for cost and wrong for agreement —
holding a handset at 12.5 m edges would cut its sag about fourfold, at the price
of the triangles the `.max` exists to save. And the finest relieved basemap level
takes no lower envelope (`floor_for` returns `None` at `quad_m ≤ sample_m × 2`) on
the stated grounds that it has nothing over it — in a city it has the entire
vector sheet stack over it. Giving it the envelope unconditionally would make it
provably under every sheet, and would cost the measured median 0.30 m / p95 1.95 m
the 6.25 m rung sits below the ground in a canal city, everywhere no sheet covers
it. Neither was changed, because neither can be judged on this container's
software rasteriser.

Water is a deliberate exception in the same area: a lake takes one level for the
whole polygon, so at a bank it interpenetrates the land by the bank's height. That
is what a water surface does.

## Grading the ground along a road is not the fix, and is measured

`cartopolis_geo::road_grade` flattens the height field inside a road corridor —
the idea being that where the ground under a road has no curvature, no two sheets
over it can disagree. It is written, tested, and **called by nothing**, on purpose.

Measured with `carto-view` on `14/8490/5321`, 2026-09-19:

* **It does not fix the tearing.** Applied without `cut_under_roads`, the street
  network is still broken into the same disconnected fragments, in a picture and
  in the overlap count. Removing the overlap is what fixes it; flattening the
  ground is not a substitute — which is what the top of this note has always said,
  and the grading was proposed three times anyway.
* **What it does buy is 0.125 m** of mean movement (0.463 m worst) — the
  metre-scale roughness a laser survey carries, which makes a draped road follow
  bumps a built road would never have. That is a *driving comfort* property, not a
  rendering one, and it is invisible in a frame: 12 cm on a road is sub-pixel
  shading from 35 m up.

What stops it being wired in is the seam. Grading per cell means two neighbouring
cells grade the same road against different extents of it — each tile clips at its
own buffer — and answer differently, which puts a **step at every cell boundary**.
Grading globally needs the field re-baked once the roads for its box have arrived,
and the field is baked per map anchor *before* any road cell lands.

So the trade is a visible step, or a re-bake of the whole field, against 12 cm of
smoothing. Neither is worth it today. If it is ever wanted, the thing that makes it
safe is the shared world lattice this note already prescribes — not a margin, and
not a per-cell pass.

## Instruments

* `--flicker` (see CLAUDE.md) — two captures ~5 cm apart, solid flipped pixels as
  `flicker_px`. Its caveat is why it is a disaster gate rather than a fine one:
  identical-geometry copies inside one mesh rasterise deterministically, so a coin
  flip only shows where the copies *differ*. The sensitive views are shallow,
  grazing and mid-distance — exactly where a low camera looks at a street.
* `vector_tiles`'s `no_group_overlaps_itself` / `no_two_groups_share_ground` —
  the measurements above, as tests with ceilings, run against the fixture on every
  suite. They are what would have caught the 2026-08-29 ghost pavements in the
  data rather than a fortnight later on a screen.
* **Measure duplication in the data before tuning depth.** That recurrence was
  seven superseded versions of one footway sharing a rung, and 79 % of the paved
  area covered twice. No rung arithmetic could have said so.
