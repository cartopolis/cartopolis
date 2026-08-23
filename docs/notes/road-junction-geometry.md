---
topic: rendering
triggers: [vector_tiles.rs, road_depth.rs, road_depth.wgsl, "junction", "ribbon", "roundabout", "junction_extent", "street_nodes", "roads don't connect", "road z-fight", "roads flicker", "road_rung"]
updated: 2026-08-23
---

# One street arrives as a chain of features, and most of its "junctions" are not

Shortbread merges ways by attribute, so an ordinary street reaches
`tessellate_roads` as several features that meet end to end — a name change, a
surface change, a `oneway` flag on one block. Every one of those meetings is a
shared node in `street_nodes`, and it looks exactly like a junction to anything
that only counts how many roads touch a node.

Measured on `crates/geo/tests/fixtures/groningen_z14.mvt` (2026-08-20, one z14
tile over the city):

| | |
|---|---|
| junction ends with another road at them | 722 |
| of those, at a crossing angle under 20° — i.e. a continuation | **418 (58 %)** |
| features that continue one another, same class and width | 272 ends (136 joins) |
| of those, turning by 10° or more at the join | 55 |
| widest such bend | 40°, a 1.28 m wedge across a 7 m street |

Two consequences, both of which had already been paid for once.

**A continuation is not a notch, and treating it as one is worse than doing
nothing.** `junction_extent` fills the wedge a tee-ing road leaves by running its
ribbon on to the far kerb of the road it joins — `half / sin θ`. That term runs
away as θ → 0, and the first fix for it *floored* the sine at 0.35. The floor
bounded the spear without stopping the code aiming one: two collinear features
each fired the longest stub the 12 m cap allowed, into each other, from both
sides. Since a stub deliberately samples marking-free tarmac
(`STUB_PLAIN_U`, so a side road does not rule its kerb lines across the road it
joins), what that produced on the ground was a street whose markings stop dead
for twenty metres every time the name changes, over a doubled ribbon. The stub
now ramps to **zero** below 15° instead. Anything else that reasons about a
road's ends owes the same distinction.

**Butt caps meeting at an angle leave a wedge**, and the old stub was hiding
that too — removing it exposed 55 visible kinks per tile. Both halves are now
mitred to the same bisector, which is why `NodeArm` carries the neighbour's
*segment length* as well as its direction: both sides then clamp the inner
corner (`INNER_BACKTRACK_BUDGET`) against the same pair of lengths and land on
the same two points. The join is watertight by construction, not by tolerance —
which is the only kind of watertight worth having, since the two halves are
tessellated independently and never see each other's output.

Rejected on the way: **snapping** the two ends together afterwards (needs the
neighbour's vertices, which are in another feature's buffer by then), and
**merging** the chain back into one polyline before tessellating (the features
differ in width and class exactly often enough that the merge would have to
decide which wins, which is the cartography this pipeline deliberately does not
do).

A closed ring — a roundabout, a turning circle — is the same mistake in a third
form: its two "ends" are one node the road runs *through*, so it took a stub
fired off along the tangent and butt-capped itself into a notch. `ribbon` takes
a `ring` flag now and mitres the seam. Only an *unclipped* run is still a ring;
a loop cut by the tile border is an ordinary open polyline.

## What this did not fix

Roads still overlap heavily at real junctions and that is correct — the ladder
in `road_rung` decides who is on top, not the geometry. Self-overlap within one
class over the fixture went 10.7 % → 6.0 % (`Minor`), 8.0 % → 0.0 % (`Major`),
21.4 % → 14.2 % (`Carriageway`); what is left is genuine crossings, slip roads
and the interchange at the north of the tile. `cargo test -p cartopolis_geo
dump_road_ribbons -- --ignored --nocapture` (with `CARTO_DUMP_WIN="x,z,size"`)
is how those numbers and the picture behind them are read.

# A hash cannot break a tie it draws independently

Overlapping is fine; landing on the *same rung* is not, and until 2026-08-21 one
crossing in four did. `road_rung` picks a tie from `line_tie_seed % 4`, drawn per
line with no reference to what it crosses, so two roads sharing a width band
collided at exactly the rate the modulus allows. A collision there is not a
settled order — both materials are opaque and coplanar, so it is a per-pixel
z-fight that flickers under any camera motion. Measured over the fixture
(`overlapping_road_features_are_separated_in_depth`, which prints the figure):
**1.60 % of road-covered pixels**. The test only ever asserted < 5 %, so it had
been passing over this the whole time.

**More rungs is not available, and that is the constraint that shapes the fix.**
`ROAD_LAYERS` × `ROAD_LAYER_STEP` is already 1.44 mm against the 1.5 mm the road
sheet has under the buildings' base, and the step cannot shrink — it exists to
clear the depth buffer's ~120 µm quantum at 1 km. The ladder is full.

So `assign_road_ties` coordinates the assignment instead of drawing it: within a
band the overlapping lines are a graph, and it is greedily coloured, most
constrained first (Welsh–Powell). A line keeps its hashed rung whenever that rung
is free, so the non-colliding majority does not move at all and the change is
invisible except where it had to act. Two things measured on the way:

| adjacency rule | coplanar overlap |
|---|---|
| hash alone (before) | 1.60 % |
| whole-line bounding boxes | 0.79 % |
| per-segment boxes | 0.64 % |
| per-segment + Welsh–Powell | **0.62 %** |

**Whole-line boxes are far too blunt**, which is why the first attempt only got
half of it: a long diagonal street's box covers a quarter of the tile and
"overlaps" nearly every road in its band, building an almost complete graph that
exhausts four colours and sends most lines back to the hash. The ordering, by
contrast, is worth little on its own (0.64 → 0.62) — it is the adjacency that
carries the fix.

The residue is genuine: places where more than `ROAD_TIE_LAYERS` ribbons of one
band really do overlap at one spot, where four colours cannot be enough and the
hash decides as before. Getting past it needs a different mechanism, not a better
colouring — **and the section below is that mechanism**, which is also why the
figures here are superseded. Note also that neither the hash nor the assignment is
stable across a tile seam, for the reason the ribbon joins are not: each tile
clips its own copy.

## There is one ladder, and it is denominated in depth-buffer ULPs

**This section replaced a longer one about keeping two ladders in agreement.**
Both are worth knowing about, because the second bug is the one that survived the
first fix and it is the reason the design changed rather than being tuned again.

Until 2026-08-23 the ordering was split across two ladders denominated
differently:

* `road_rung` → a **fixed metric** lift, `rung × 24 µm`. Its significance in the
  depth buffer *shrinks* as 1/z.
* `SurfaceGroup::depth_rung` → a **depth-buffer ULP** bias. Its apparent world
  shift is `1.19e-7 · bias · z`, so it *grows* with range.

They failed in both directions, and only the first was understood at the time.

**Crossover.** The metric ladder sorted by width across every class at once while
the bias sorted by class, so wherever they disagreed — a wide residential against
a narrow tertiary — the terms crossed somewhere in the middle distance and the
winner flipped as the camera moved. Not flicker at a seam: the loser's tarmac
vanished over whole patches. Reported from a handset as "sometimes part of the
roads become invisible, the order is reversed". Making the metric ladder
class-major (2026-08-21) fixed this, at the price of compressing width onto two
sub-bands to fit the ceiling.

**Dissolution**, which that fix could not touch and which no tie-colouring could
reach either. 24 µm is worth 2.0 ULP at 100 m, 1.0 at 200 m and *nothing* beyond,
so past a couple of hundred metres the whole within-class ladder — width, ties,
casing — sat below the depth buffer's resolution and every same-class overlap
went back to an exact per-pixel coin flip that re-rolls with the view matrix. On
the fixture that is 6 % of `Minor` and 14 % of `Carriageway` (see the self-overlap
figures above) fighting at any real distance. The rungs were still being assigned
carefully; they had simply stopped meaning anything.

**So the rung is no longer a height.** It is carried per vertex
(`SurfaceMesh::rungs` → `ATTRIBUTE_ROAD_RUNG`) and applied by
`systems::road_depth` in the vertex stage as `position.z *= 1 + rung ·
ROAD_RUNG_ULP · 1.19e-7`. Scaling clip-space z is a depth offset proportional to
the fragment's own depth, which is exactly what a hardware `depth_bias` unit is —
so the separation is measured in the same units the depth test resolves in and
**cannot decay with range**. Every road group now returns the *same*
`depth_bias()` (the foot of the road band), and the class hierarchy lives in the
rung alone. There is no second opinion left to contradict the first, which is a
stronger guarantee than the test that used to check the two for agreement.

Three consequences worth keeping:

* **The millimetre ceiling is gone.** A rung costs no height, so
  `ROAD_LAYERS × step < 1.5 mm` stopped being a constraint. Width went back to
  the full eight `ROAD_WIDTH_BANDS` and ties from 4 to 8.
* **A new ceiling replaced it, in ULPs.** The ladder has to stay inside the band
  between `Water` and the things drawn *over* a road, so `BridgeDeck`, `Canopy`,
  `Trunk` and `Furniture` are pinned above `road_rung_ceiling()` rather than at
  fixed rungs. `road_depth`'s
  `the_road_ladder_stays_under_everything_drawn_over_a_road` is what stops the
  ladder growing through them.
* **`ROAD_RUNG_ULP` is written in two languages.** The Rust constant and the
  WGSL one must match, and `the_shader_and_the_ladder_agree_on_the_step` reads
  the shader source to check it.

**Measured on the Groningen fixture** (`overlapping_road_features_are_separated_
in_depth`, which prints the figure):

| ladder | coplanar overlap |
|---|---|
| hash alone, width-major (pre-2026-08-21) | 1.60 % |
| tie colouring, width-major | 0.62 % |
| tie colouring, class-major, 2 sub-bands (the metric ceiling) | 0.89 % |
| **ULP rung, class-major, 8 sub-bands × 8 ties** | **0.51 %** |

**More rungs stops helping at 0.51 %, and that is not a tuning failure.** Ties
were swept at 8, 16 and 32 against eight sub-bands and every one measures 0.51 %:
what is left is a ribbon overlapping *itself* — shared triangle edges and the
bevel corner fills — which shares a rung by definition and no ladder can
separate. It is also the benign case: one material, one colour, and the same UV
on both sides of the overlap.

The prior figures are kept above because the *shape* of that table is the useful
part: the largest single win was the adjacency rule in `assign_road_ties`, not
any amount of extra ladder.
