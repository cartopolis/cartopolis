---
topic: rendering
triggers: [vector_tiles.rs, "junction", "ribbon", "roundabout", "junction_extent", "street_nodes", "roads don't connect"]
updated: 2026-08-20
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
colouring. Note also that neither the hash nor the assignment is stable across a
tile seam, for the reason the ribbon joins are not: each tile clips its own copy.
