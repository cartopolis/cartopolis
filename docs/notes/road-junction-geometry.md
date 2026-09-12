---
topic: rendering
triggers: [vector_tiles.rs, road_depth.rs, road_depth.wgsl, road_stitch.rs, "junction", "ribbon", "roundabout", "junction_extent", "street_nodes", "roads don't connect", "road z-fight", "roads flicker", "road_rung", "kerb", "casing", i_overlay, cut_band, round_corners, corner_cut, "faceted road", "roads look polygonal", "road is a curve", "junction mouth", "side street", "roads look wrong", ribbon_kinds, MITER_LIMIT, "spike", "road seam", "tarmac in the field"]
updated: 2026-09-11
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
separate. It was also called the benign case — one material, one colour, and the
same UV on both sides of the overlap. **The UV half of that is false**, and the
last sections of this note are what it cost.

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
double coverage. `the_road_sheet_is_a_partition` prints both figures. (The 0.81 %
residue was excused here as a ribbon lapping over *itself*, which no ladder can
separate. That excuse did not survive being looked at either — see below.)

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

## The 2026-08-29 recurrence was not the roads at all — check the data before the ladder

"The streets still flicker" came back after everything above, and none of it was
wrong: the fight was the **surveyed paving layer against itself**. The BGT
extractor kept every superseded *version* of every pavement (marked only by
`eind_registratie`, which `termination_date` does not cover), so up to 7
coplanar copies of one footway shared one depth rung — no ladder can order a
layer against itself. The full story, the fix and the tripwires are in
`surveyed-street-objects.md`; the client-side floor is the exact-repeat drop in
`parse_surveyed_paving` / `parse_surveyed_cell`.

Two standing lessons for the next flicker report:

- **Measure duplication in the data before tuning depth.** Rasterise the layer
  and count multi-covered area; 79 % of the paved area was covered twice and no
  amount of rung arithmetic could have said so.
- **`--flicker` is the instrument now** (see CLAUDE.md): two captures ~5 cm
  apart, solid flipped pixels reported as `flicker_px`, gated in CI at 1,500
  against a measured ~500 animation floor. Its caveat is the reason the gate is
  a disaster gate: identical-geometry copies inside one mesh rasterise
  deterministically, so a coin flip only shows where the copies *differ* — the
  sensitive views are shallow, grazing, mid-distance ones, exactly where a low
  camera looks at a street.

## A kerb is not allowed to seal the street that joins it (2026-09-10)

Everything above is about which ribbon wins a patch of ground. `road_rung` is
the answer, and it is **class-major**: a whole class sits above a whole class.
The kerb band goes in at the *foot* of its own class's block (`road_casing_rung`)
so that a road's tarmac beats its own kerb — which is right, and which is also
why the band is a band rather than the old slab.

What nobody checked is what that says about two *different* classes. A `Major`
road's casing rung is 130; a `Minor` road's fill tops out at 129. So the main
road's **kerb** outranked the side street's **carriageway**, and once
`road_stitch` started cutting rather than ordering, that stopped being an
invisible depth decision and became geometry: the side street's tarmac was cut
away at the far side of the kerb band, and the kerb line ran on unbroken across
the mouth. Every residential street tee-ing into a primary road was walled off
from the road it joins. It is a 0.75 m band, so from any altitude a whole
junction fits in it is a hairline — which is why three rounds of junction work
went past it — and from the pavement it is the thing that makes the junctions
look wrong.

The rule it was missing is one sentence: **a kerb is the edge between a
carriageway and what is not one, so wherever another carriageway abuts, there is
no kerb.** That is a junction mouth, and every side street in a city has one.

So the cut order gained a leading term, `vector_tiles::cut_band`, above the whole
class hierarchy:

| band | | |
|---|---|---|
| 2 | rails | a level crossing is track laid *over* a road; `road_rung` already had rail at the top and this keeps it there rather than promoting every carriageway over it |
| 1 | carriageways | every class a kerb is drawn for |
| 0 | kerb bands, and footways | a footway is not a carriageway and does not break a kerb; inside the band `road_rung` decides exactly as before |

**A band, not a "may block" predicate, and that is the load-bearing part.** The
obvious way to write this is a rule — *a carriageway blocks a kerb whatever the
classes say* — and it is wrong in a way that produces a worse artefact than the
one it fixes. `stitch` cuts each ribbon against everything that outranks it, all
at once; if the major's kerb outranks the minor's carriageway *and* the minor's
carriageway blocks the major's kerb, then both are cut out of the ground they
share and the junction gets a **hole in the road**. The order has to stay total,
which is what a band gives and what an asymmetric predicate cannot.

Measured on the fixture (`a_side_street_reaches_the_road_it_joins`). The tile
holds 23 ends where a street of one class stops on a *through* road of a higher
one; 15 of those cross at an angle steep enough to sample cleanly. The sample is
the point on the side street's own centre line 40 % of the way into the main
road's kerb band — which is on the **approach** side, since the node sits on the
main road's centre line, and `1 / sin` further along the side street than it is
across the main road:

| what is drawn there | before | after |
|---|---|---|
| the side street's carriageway | 8 | **15** |
| the main road's kerb, across the mouth | **7** | 0 |

The road sheet is still a partition — the cut is unchanged, only its order is —
and `the_road_sheet_is_a_partition` measures 0.81 % both ways, the same figure it
has measured since 2026-08-26.

Note the eight that were already open. This is not a defect that fires at every
cross-class junction: it needs the main road's *casing* rung to sit above the
side street's *fill* rung, which is what the class-major ladder does whenever the
classes differ, but the sample also has to land where the main road's kerb band
actually reaches. Half of them is enough — a street in four having its mouth
bricked up is what "the joints look ugly" is made of.

The road sheet is still a partition — the cut is unchanged, only its order is —
and `the_road_sheet_is_a_partition` still measures 0.8 %.

The picture that shows it is `dump_road_ribbons`' second output. The overlap
image it already wrote cannot answer this question — a kerb crossing a
carriageway is a partition either way and both come out as one clean pixel — so
the dump now also writes `/tmp/ribbon_kinds.png`, carriageways pale and kerb
bands dark. A walled mouth is a dark bar ruled across a side street; an open one
is tarmac meeting tarmac.

```bash
CARTO_DUMP_WIN="300,300,220" cargo test -p cartopolis_geo \
    --lib dump_road_ribbons -- --ignored --nocapture
```

## A road is a curve, and a generalised vertex is not a corner (2026-09-10)

Everything above is about where two ribbons meet. This is about the ribbon
itself, and it is the thing you actually see standing on the pavement.

Shortbread generalises its geometry for a map read from above, where a 20° kink
in a street is a pixel. Extrude that into a ribbon two lanes wide, put a painted
edge line down both sides of it, and look at it from eye height: every one of
those kinks is a visible corner, and the street reads as a row of straight
boards bolted end to end. Measured over the fixture's drawn centre lines,
**79 % of interior vertices turn by 2–70°, and 49 % by ten degrees or more**.
This is the ordinary case, not an edge one.

So `round_corners` replaces each moderate kink with a circular fillet before the
ribbon is extruded, and `a_drawn_street_is_a_curve_not_a_chain_of_boards`
measures the result:

| turn at an interior vertex | raw | rounded |
|---|---|---|
| under 10° | 648 | **4616** |
| 10–20° | 378 | 2 |
| 20–70° | 447 | 70 |
| over 70° — a corner the map asserts | 216 | 216 |

Five rules, and every one of them is a thing that broke a test when it was
missing:

- **The endpoints never move.** A road's ends are its junction nodes, and the
  mitred continuation, the junction stub and the half-open tile ownership are
  all anchored on them.
- **The end *bearings* never change.** A fillet at the second vertex pulls that
  vertex back *along* the first segment, so the segment shortens and keeps its
  direction. `junction_extent` and `continuation` read the bearing and are
  untouched.
- **A fillet tighter than the road is wide is refused.** The ribbon is offset
  `half` either side of the centre line, so on the inside of an arc of radius
  `r` the edge has radius `r − half`; below `r = half` the strip turns inside
  out. The rule is one line — round only while `t ≥ half · tan(θ/2)` — and it is
  what makes this safe on a 22 m motorway and generous on an alley without a
  width table. `a_tight_corner_does_not_fold_the_ribbon` is the guard.
- **A fillet may not move the road more than a metre** (`ROUND_MAX_OFFSET_M`)
  from the vertex the map recorded, and a turn over 60° is kept sharp. The
  licence this takes is that a generalised vertex is an artefact of drawing the
  map small; that licence runs out at a corner the map is asserting, and a
  street grid's right angles are exactly that.
- **`street_nodes` must report the segment length *after* the fillet.** This is
  the subtle one. `ribbon` clamps its inner corner against the shorter of the
  two segments meeting there, and at a continuation one of those comes over from
  the neighbouring feature as `NodeArm::leaves`. Shorten the segment `ribbon`
  measures while `NodeArm` still reports the raw one and the two halves of one
  street clamp against different numbers and land on different points — which is
  the hairline `two_halves_of_one_street_meet_on_the_same_two_points` forbids.
  Both sides go through `corner_cut`, so there is one answer rather than two.

**Rings are not rounded.** A roundabout's polygon is not a generalisation
artefact the way a kink in a street is: its vertices lie *on* the circle the
junction actually is, so cutting its corners would shrink the roundabout rather
than smooth it. Its seam is already mitred closed.

**What it costs, measured on this container** (median of three, on a box that
was also running a render, so read the ratio and not the absolute):
`tessellate_roads` over the fixture **481 ms → 590 ms (+23 %)** and the road
sheet **15,163 → 25,445 triangles (+68 %)**. The step between fillet points is
the knob: at 22° the residual kinks are still 10–20° and cost 20,655 triangles;
at 8° they collapse to nothing extra; at 6° the count keeps climbing for no
further angular gain. `ROUND_STEP_DEG` is 8 for that reason and not by taste.

`the_road_sheet_is_a_partition` is unchanged at 0.81 %, and every one of the
join tests above still passes — the fillet changes what the centre line *is*,
not how two of them meet.

## A ribbon laps over *itself*, and that is not benign either (2026-09-11)

Twice above, a residual overlap is excused on the grounds that a ribbon
overlapping itself cannot be seen: "one material, one colour, and the same UV on
both sides". The first two clauses are true. **The third is not.** `v` is metres
travelled along the road, so where a centre line doubles back the returning leg
carries different lane dashes and a different edge line from the outgoing one, at
the same depth and on the same rung. Which one draws is a per-pixel coin flip
that re-rolls with the view matrix — the exact failure `road_stitch` was built to
remove, surviving inside the one case it exempted.

So `cut_ribbon` no longer skips `j == i`. A triangle is cut against the earlier
triangles of its own ribbon as well as against its superiors, `Tri::seq` being
that order. It has to be strict and total for the same reason `priority` does: an
order with a draw in it cuts both sides out of the ground they share and opens a
hole. Adjacent strip triangles share an edge, and `tris_overlap` reads a shared
edge as *not* overlapping, so an honest strip passes through untouched.
`the_shared_edges_of_one_strip_are_not_a_fold` guards that, and it matters more
than it looks: read a shared edge as an overlap and the cut nibbles a hairline
out of every triangle in the tile.

Measured on the fixture:

| | before | after |
|---|---|---|
| road pixels covered twice | 3,501 (0.81 %) | **1,574 (0.37 %)** |
| `Road(Minor)` drawn twice by its own class | 919 | **1** |
| `Road(Carriageway)` | 1,394 | 408 |
| in one 100 m window on a tee junction | 18,637 (4.2 %) | **12** |
| **covered ribbon pixels** | 421,555 | **421,555** |

The covered area does not move by a single pixel, which is the measurement that
says a fold was removed rather than road deleted. What is left is the overlap
kept on purpose: every remaining pair at the interchange is `bridge=false`
against `bridge=true`.

## A miter limit of 2.5 is a stroke default, not a number about roads (2026-09-11)

The same fixture street exposed a second defect, and the two look nothing alike
until you find the vertex they share. Feature 69's first linestring is a
`tertiary` turning head:

```
0   382.080  411.377
1   382.080  416.260   turn  23.2°
2   384.277  421.387   turn 128.2°   <- reverses here
3   387.207  416.016   turn  28.6°
4   387.207  410.889
```

It runs ten metres, reverses, and comes back five metres to the east. Tagged
`tertiary` it is drawn **9 m wide** — wider than the gap between its own two
legs, which is why the legs cover each other outright and why it was the worst
self-overlap in the tile. And a mitred join at 128.2° puts the outer corner at
`1 / cos 64.1° = 2.29` half-widths from the centre line: **10.3 m**. `MITER_LIMIT`
was 2.5, so 2.29 slipped under it, and the street ended in a ten-metre tongue of
tarmac thrown out into the field past the road it serves.

2.5 is roughly what a stroke renderer defaults to (SVG's is 4). At that bound a
9 m street may throw an 11 m spike and a 22 m motorway a 27 m one, which is not a
shape any road has. **1.5 instead**, and the number is set by one fact: a right
angle mitres at `1 / cos 45° = 1.414`, and a street grid's right angles are
corners the map is asserting — the same licence `ROUND_MAX_DEG` draws. So every
corner up to ~96° keeps its point and nothing in an ordinary city moves; past
that the join bevels, and the chord between the two square corners is what a
turning head actually looks like.

**It has to stay a ratio, not a distance**, and that is the load-bearing part. A
metric bound ("no join may reach more than 2 m past the square corner") is the
tempting form and it scales correctly with width, but `kerb_band` zips the
carriageway against a second, wider run of the *same* join code and relies on
every one of those decisions being independent of the width. Make the limit
metric and the fill and its kerb bevel at different corners, the two chains
desynchronise, and the band silently drops to its slab fallback.

Cost, on the fixture: the road sheet gives up 2,612 px of a 446,044 px window,
which at that scale is 18 m² — the area of the spike, and nothing else.
`a_reversal_is_bevelled_rather_than_drawn_as_a_spike` measures every ribbon
vertex against the centre line it was built on over seven turn angles and three
widths, and pins the constant from both sides so neither bound can drift.

**Two lessons worth keeping.** A self-overlap is only invisible if *every*
per-vertex attribute agrees across it, and on a textured road one of them never
does. And a join limit inherited from stroke rendering is a number about ink, not
about tarmac: the bevel is not a fallback for pathological input, it is the
normal shape of a road that turns hard.

The pictures behind all of this are `dump_road_ribbons`' two outputs, windowed
with `CARTO_DUMP_WIN="333,354,100"` on the fixture's tee junction.
`CARTO_NO_STUB=1` was added alongside: it lays every ribbon with a square end, so
a shape at a junction can be told apart from the stub that fills it. That switch
is what ruled the stub out here — the spike was unchanged without it.
