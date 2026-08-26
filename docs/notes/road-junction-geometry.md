---
topic: rendering
triggers: [vector_tiles.rs, road_depth.rs, road_depth.wgsl, road_stitch.rs, "junction", "ribbon", "roundabout", "junction_extent", "street_nodes", "roads don't connect", "road z-fight", "roads flicker", "road_rung", "kerb", "casing", i_overlay]
updated: 2026-08-26
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

## What this did not fix at the time

Roads still overlapped heavily at real junctions, and that was thought to be
correct — the ladder in `road_rung` decided who was on top, not the geometry.
**That premise is what turned out to be wrong**; see the last section. Self-overlap within one
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

## The ladder was the wrong tool, and the overlap is gone instead (2026-08-26)

Every section above is an attempt to *order* an overlap. All of them work in the
sense they claim and none of them stopped the flicker, because a depth tie-break
only holds while the separation it asks for is larger than the rasteriser's own
interpolation rounding — and `ROAD_RUNG_ULP` was **four** depth units, asserted
to "clear the noise" and never measured. The sibling ladder that orders grass
against water against road uses sixty-four, and `DEPTH_BIAS_STEP`'s own comment
records sixteen being tried and found wanting at the shallow angles a low camera
looks at the middle distance with. A road is the surface you almost always see at
a shallow angle.

Measured on the fixture before the change, as a share of road-covered pixels:

| held apart by | share |
|---|---|
| nothing at all (same rung) | 0.51 % |
| one rung (4 depth units) | 1.9 % |
| three rungs or fewer (≤ 12 units) | 3.3 % |
| **covered by two or more triangles at all** | **76.1 %** |

That last row is the one that had never been measured, and it is the shape of the
problem: **the kerb band was a slab.** Every road laid a casing ribbon the full
width of road-plus-kerb and then drew its tarmac on top of it, so every
carriageway pixel in the tile was two opaque coplanar surfaces. Junction crossings
are the visible part; the slab is the bulk.

Confirmed in a render before touching anything: the same camera, the same scene,
with only the shader's `RUNG_ULP` raised from 4 to 32, changed 2.2 % of the frame
— all of it distant roads at shallow angles, where a street that came out broken
and patchy at 4 came out solid at 32
(`/tmp` only; the method is `--shot` twice with one constant edited, since the
shader is an asset and needs no rebuild).

So `cartopolis_geo::road_stitch` cuts the ribbons against each other instead.
Each ribbon keeps only the ground no higher-priority ribbon claims; what is drawn
is a partition. **Road pixels covered twice: 76.1 % → 0.81 %**, and the covered
area itself moves by 4 pixels in 423,028 (0.001 %) — nothing was given up but the
double coverage. `the_road_sheet_is_a_partition` prints both figures.

Standing facts about the cut:

- **`road_rung` survives, as the order the cut runs in.** The hierarchy — bridge,
  then class, then width band, then the tie colouring — is unchanged and still
  worth its tests; it simply no longer has to be expressed in the depth buffer.
- **The depth ladder is two rungs**, ground and bridge, at sixty-four units each.
  A flyover and the road under it are the one overlap deliberately kept, because
  they are two real surfaces in one place. Everything else is geometry now.
- **Collapsing the ladder fixed a second bug for free.** Five things drawn *over*
  roads carry hard-coded rungs on the shared ladder — zebra paint at 10.5, the
  route casing and fill at 18 and 20, street labels at 23, the driver arrow at 24
  — and the 399-rung ladder's ceiling of 1,916 depth units had swallowed all of
  them. Zebra crossings on anything wider than a residential street were being
  drawn under the tarmac. The old test checked four *other* things and stayed
  green; `everything_drawn_over_a_road_clears_the_road_ladder` checks all nine.
- **The kerb is a band, not a slab** (`vector_tiles::kerb_band`). Zipped between
  two runs of `ribbon` at `half` and `half + ROAD_CASING_M` rather than offset by
  hand, so its inner edge is the carriageway's outer edge by construction — same
  join code, same miter, same bevel decision, same clamp.
- **No gaps, and the reason is the lattice.** Every road vertex is snapped to
  integer millimetres and the cut runs on `i_overlay`'s integer interface, so a
  boundary between two pieces is the same two points for both of them rather than
  two floating-point opinions about one line. A triangle nothing reaches is passed
  through untouched, which is also what keeps the texture registered down a curve.
- **What it costs.** `tessellate_roads` over the fixture: 4.7 ms → 88 ms. Three
  things got it there from 1.4 s, and all three are about not asking the boolean
  solver questions it does not need: a bucket index over the ribbons' *triangles*
  (a road's bounding box is a hopeless proxy for a road), clipping against the
  blockers' individual triangles rather than their whole contours, and an exact
  integer separating-axis test before each call. It runs on a `platform::workers`
  thread beside a ~120 ms tile rasterise.
- **`i_overlay` is a new dependency**, chosen because it is pure Rust with no C
  in the tree — the Android and wasm lanes have only the NDK's clang — and
  because its integer interface is what the no-gaps argument rests on.

Rejected on the way: **raising `ROAD_RUNG_ULP` and leaving the overlap there.** It
does work (the render above is the proof), but the whole ladder cannot simply be
scaled up — its top holds the tree trunks, the benches and the bridge decks, and
at 399 rungs × 64 units those would be pulled a metre toward the camera at a
kilometre. It would also have left 76 % of the road sheet shaded twice, which is
fill rate a handset does not have.
