---
topic: bridge-heights
triggers: [road_levels.rs, conform_to_roads, "road heights", height.rs, map_geometry.rs, vector_tiles/bridges.rs, road_stitch.rs, AHN, PDOK, bridge, deck, viaduct, "grey slab", "markings stop", elevation, "EPSG:28992", RD New, terrain, railing, parapet, railing_runs, abutment, spanned_water, "pipeline/water.rs", land_at]
updated: 2026-10-20
---

# Where a bridge's height comes from, and what it costs

## Roads carry their own height now, and the ground follows them (2026-09-27)

Everything from "The flat road network is the real constraint" down to the
abutment band was one approach: the road had no height of its own, and a joint
was the *terrain field* bent towards a deck — raised along the approach, held at
the deck's top under its end. It could not be made exact. The field is a
raster sampled every 2.86 m by atlas and resampled onto 3.125 m by the client,
so where a deck met its road the road's height was an interpolation that
included the ground the deck spans. Every fix moved the error: the Europaplein
kept a step of a few tenths at each joint through four of them.

`cartopolis_geo::road_levels` makes the road the thing with a height. Streets
are runs with a level (0 ground, 1 bridge, -1 tunnel); ways that meet share a
node (a dangle meets what it lands on, two through-lines only at the same level
— the routing graph's rule), and a node has one height, so the deck and the
road off its end are the same number at the joint by construction. Atlas solves
it per tile over the same 350 m window it grades, which makes a ramp across a
tile edge one ramp:

- **Measured wherever the survey has it.** A ground road reads the raw bare
  ground under it — which contains the real embankments — and a span on a lifted
  deck is fixed to the deck's surveyed top end to end. A hole in the bare ground
  (a structure AHN cut out: the Europaplein's is ~200 m where OSM tags 60 m of
  bridge) is left unknown and bridged along the network from both sides.
- **One operation for embankments and cuttings.** Outliers go by a median along
  the network, gaps are interpolated, and the grade is limited by the midpoint of
  the tightest upper and lower envelopes (6 % for roads, 10 % paths, 2.5 % rail);
  fixed heights are then honoured exactly. A deck pulls its approach up and a
  tunnel floor would pull its approach down by the same code.
- **The ground is laid to the roads**, not the roads draped on the ground:
  `terrain_grade::conform_to_roads` puts it 5 cm under each ground-level road
  and its verge and blends back to the survey over a 1:2 shoulder. The old road
  flattening and ramp raising are not run by atlas any more.

Atlas publishes `roads/14/x/y.bin` (`RoadHeights`, CRH1) keyed by run name: FNV
over the run's points in tile units, because atlas and the client each work
out a tile's size in metres for themselves. The client matches its runs by name
and stands road ribbons and casings on their street's height (`OnRoads`). Over
Groningen the Europaplein span runs 8.20 → 9.24 m and the roads off its ends
start at exactly those numbers.

**Tunnels are on it too since 2026-09-28** ([tunnels.md](tunnels.md), first
section): a tunnel is a ceiling on its road as a deck is a pin, the structure
around a road below its land is found on the same network
(`road_levels::structures`), and atlas's grading is one rule with three cases —
laid to the road at grade, left alone under a span, put back to the land over and
beside a structure below the ground. What is still separate is the *shape* of
the two structures: a deck is an area (OSM's bridge outline, or one built from
the bridge ways), a tunnel a corridor along its road, and each has its own
builder. The client's own grading where atlas has not graded still flattens and
ramps the old way. Cars, the route line and the avatar still read the terrain and
the deck, which agree with the road to within the conform's 5 cm.

## The tiles do not carry it

Shortbread's `streets` layer has `bridge` (bool) and nothing else — no `layer`, no
clearance, no elevation. Its `bridges` layer is **areas**: the outline of the
structure, carriageway and parapets together. Measured on the Groningen station
tile (14/8490/5321, fetched 2026-07-30) the Emmaviaduct arrives as two polygons,
34 × 149 m and 68 × 176 m; canal bridges are 20 × 20 m or so. That is geometry you
can stand a deck on, but it says nothing about how high.

## AHN answers it, and the DTM's holes are the trick

PDOK serves the national height model at
`https://service.pdok.nl/rws/ahn/wcs/v1_0` — WCS 1.0.0, coverages `dsm_05m` and
`dtm_05m`, AHN4 (flown 2020–22), CC0, `fees NONE`, **EPSG:28992 only, GeoTIFF
only**, no-data written as `f32::MAX`. There is also a WMS whose `GetFeatureInfo`
returns a single value as JSON, for point probes with no TIFF decode.

Verified over the Emmaviaduct 2026-07-30 by rendering both models over the same
300 m box: **the DSM contains the deck** at its real height, and **the DTM cuts it
out** as no-data while keeping the ground of the rail yard underneath. So

    clearance = median(DSM inside the bridge outline) − median(DTM around it)

is a measurement, not a guess, and the DTM needs no "exclude the deck" predicate
because AHN already excluded it. Median, not mean: the DSM over a deck also caught
the parapets, the lamp posts and whatever was driving across.

Sampled heights, Groningen, 2026-07-30: Emmaviaduct **6.6 m**, ring-road flyovers
**7.9–8.3 m**, canal bridges **1.3–3.9 m**.

## The flat road network is the real constraint

Everything else in the client stands on one plane at `y = 0`. A measured deck is
therefore a *step* at both of its ends, because the approach roads that climb to it
in reality are ordinary unflagged ways drawn flat. Two consequences:

- Decks under `map_geometry::MIN_LIFT_M` (2.5 m) are laid flat deliberately. A
  Dutch canal bridge measures a metre or so above its quays while being level with
  the street it carries — lifting it built a slab floating over its own approaches.
- Above that the step is accepted, because something genuinely passes underneath
  and a flyover flush with the yard is worse than a flyover with abrupt ends.

The fix that removes the constraint is **sampling the approach profile from AHN
too** — the embankments are solid earth, so they are in the DTM — and lifting the
approach ribbons with it. Done 2026-09-22, and the shape of how it was *not* done
for a day is worth keeping: see below.

## The approach is graded now, and the grading existed a day before anything called it

`road_grade::ramp_from_pins`, `flatten_pinned` and `MAX_APPROACH_GRADE` landed on
2026-09-21 with their own tests. The only thing that ever called them was
`carto-view`, so the client's bake went on calling plain `flatten` and every
viaduct in the thing people actually look at kept its vertical step. A feature
can be complete, tested and unreachable at the same time, and nothing in a green
suite says so.

Three pieces sat between the two and each is load-bearing.

**The approach corridor is not the road corridor.** `RoadTessellation::approaches`
— the ground stretch of each bridge-flagged street either side of its span — was
handed to the client's worker and dropped. It is the only thing that separates
the road climbing onto a structure from the road passing under it: both are at
grade, both are unflagged, so neither `level_of` nor the survey can tell them
apart, and what does is that the approach is the *same street feature* as the
span. Ramping through the whole road mask instead raised 18 % of the Groningen
fixture's streets for five bridges, because a raster corridor is one connected
shape and a gradient travels through everything touching it.

**The pins arrive after the field that has to reach them.** A deck's height is a
measurement, resolved some frames after its cell lands, so the first field over a
city is always baked before any viaduct has one. The decks therefore get the
treatment the road marks and the water levels already had — a revision, a settle
window, and an `answered_deck_rev` so a bake records what it used.

**And a re-published deck must not count as a change.** A bake bumps
`TerrainField::revision`, every streamer answers that by re-tessellating its
ring, and every rebuilt cell with a viaduct lands back in `DeckPlates::set_cell`.
Bumping there unconditionally closes a loop that never settles and retears the
city about once a second, with nothing in the log to say why. `set_cell` compares
a digest now; it is sound because a retear produces the same decks, the height
being a fact about the survey rather than about the field being graded.

`approach_abutments` and `approach_raised` are in `--dump-state`, because an
embankment is 150 m of ground rising six metres and from any angle that also
shows the viaduct that reads as ground. Zero abutments with decks standing is
precisely the failure above, and no picture would have reported it.

There is a second place a deck height already exists, and it does not help
either: the Kadaster's 3D basisvoorziening ships the BGT's
`Overbruggingsdeel_vlak` with an AHN-draped z (45 decks in the 2 km Groningen
block, one viaduct running 0.96 → 6.62 m, measured 2026-08-30). It is the same
AHN measured above, so it supplies a number this note already has, and the
constraint was never the height — it is the flat approach roads.
[3d-basisvoorziening.md](3d-basisvoorziening.md)

## Three faults the grading still had, measured 2026-09-23

A research pass over the four bridge views in `docs/grade-shots.toml` found none
of them right, and all three causes were numbers rather than pictures — at the
manifest's altitudes an 8.7 m cliff is a few pixels.

- **The approach was a sliver.** `split_by_deck` only knew the part of the
  *bridge* way that overhung the deck outline, and OSM's bridge way stops at the
  abutment: every approach over four Groningen tiles was 0.0–0.3 m, and many decks
  had none. `vector_tiles::extend_approaches` now follows the street that carries
  on from each abutment, the walk `tunnels::find_approaches` does for a tunnel
  ramp, up to `APPROACH_REACH_M` (160 m).
- **The target was counted twice.** The pin was the ground at the abutment plus
  the clearance, and the survey's ground there is routinely the real embankment
  already: approaches graded 4–5 m above their deck. `deck_height` keeps the
  measured top now, and the grading pins to it.
- **One level per tile.** Every deck in a cell stood on one median ground level
  plus its clearance, floating a small canal deck 3.4 m and burying another. Each
  deck is drawn at its own measured top; traffic on it stands on that
  (`DeckPlates::top_at`).

`deck_step_max_m` / `deck_step_mean_m` in `--dump-state` are the check: a deck's
drawn top against the ground three metres past where each approach leaves the
deck's outline, skipping stubs under a metre (a deck the tile border cut, which
continues next door). Measured on the Deck after the fixes: mean 0.1–0.3 m in
every bridge view. What still steps is an approach the **tile border** cuts: the
next tile does not know the deck is there, so the ramp stops at the edge.

## Not every bridge has an outline

Everything above hangs off the `bridges` layer, and that layer is only the
bridges somebody mapped as an **area** (`man_made=bridge`). Plenty are mapped as
nothing but `bridge=yes` ways: the Europaplein flyover, where the ring road
crosses the junction, is four such carriageways and no outline, so no deck was
built, nothing was measured and the flyover was drawn on the ground across the
roads it passes over (reported from the phone 2026-09-26). The two Groningen
fixture tiles counted 4–5 outlines against 9–20 flagged stretches no outline
covers.

`vector_tiles::street_decks` builds the missing deck from the main-road and rail
ways: each
uncovered stretch buffered to its carriageway plus a metre either side, merged
with its neighbours, clipped to the tile, less any outline it runs onto. From
there it is an ordinary deck — measured, profiled, ramped. Three things about it:

- `split_by_deck` only asks at a way's vertices, so a two-vertex way whose ends
  lie just off an outline reads as never touching it; the stretch is sampled
  every two metres before asking. Without that the station fixture's rail bridge
  got a second deck over its own outline. The road tessellator still asks only
  at vertices.
- Paths and minor lanes are left out: over a narrow strip under trees the
  survey is the canopy — Groningen's boardwalks and Stadspark lanes measured
  11–23 m. So a residential bridge with no outline still lies flat.
- Atlas matches decks to the client by position, and a cell with a deck atlas
  never measured is measured by the client instead — so a client with this
  change over ground graded *before* it lifts the new decks but gets no ramp to
  them until atlas grades that ground again.

## A deck is the structure, not the road surface

Until 2026-08-29 the span of a bridge-flagged street was simply **dropped** and the
deck stood in for it. The deck is a flat, untextured `#575659` polygon, so what that
produced was a bare grey slab at every crossing, with the tarmac, the lane markings
and the kerb casing stopping dead at one edge and starting again at the other — the
whole Shortbread `bridges` footprint, parapets included, painted as one blank
shape. It was invisible as a *bug* because it looked deliberate, and because the
slab sits at the top of the ground depth ladder (`SurfaceGroup::depth_rung`), so it
covered the ribbon that was still drawn underneath it.

The span is now kept and lifted: `tessellate_roads` returns it in
`RoadTessellation::on_deck`, tagged with the deck, and `resolve_bridge_decks` gives
it the deck's height once AHN answers. Three things make that safe.

- **The carriageway is cut out of the slab**, by the same `road_stitch` pass that
  makes the ground sheet a partition — the deck goes into the cut as the weakest
  claim, so it loses exactly the ground the road covers and keeps the verge and the
  parapet either side. Two coplanar opaque surfaces are otherwise a fight the deck
  always wins, which is the old bare slab with extra steps.
- **The split lands on the deck's edge, not on the nearest vertex.** A vertex is
  where the street bends and a deck edge is where the structure stops; cutting at
  the vertex left up to a segment of road belonging to neither piece, which only
  went unnoticed while the slab was drawn over the top of it. `split_by_deck`
  bisects on `BridgeDeck::contains` — a hole, a self-touching outline and two
  abutting decks are then all one case.
- **One height for the road and the slab.** `resolve_bridge_decks` reads
  `TerrainField::level_over` across the deck outlines once and hands that number
  to both — the slab through `Stand::Lifted`, not the field again: the field's
  answer depends on the points it is asked over, and the slab's (its fascia, a
  profiled deck's subdivision) are not the outline's.

There is no kerb casing on a deck: what lies beside the carriageway up there is the
structure, and that is the part of the slab the road was cut out of.

## A deck has a slope, and piers

**One median height per deck was wrong for every viaduct that is not level**, and
most are not: the 3D basisvoorziening's drape of the old Emmaviaduct fell from
7.3 m NAP to 1.9 m over 221 m, and the rebuilt one in AHN6 rises 6.4 → 9.5 → 8.0
from end to end. The surface samples already fetched for the median carry the
slope, so `cartopolis_geo::deck_profile` takes their median per 4 m of the deck's
long axis — median, because parapets, lamp posts and lorries are in the samples;
along the axis, because a deck is level across itself. The slab is subdivided to
3 m edges for it (an OSM outline is often four corners), and the carriageway, the
cars (`top_at`), the route line (`lift_at`) and the approach grading all read it.

**The piers are the BGT's** (`overbruggingsdeel`, type `pijler`, level 0): a
footprint on the ground and nothing else. Their height is what the deck over them
already knows, so each is stood from the ground to the underside of the lowest
lifted deck its centre is under. The same collection gives the abutments
(`landhoofd`) and which spans open (`overbrugging_is_beweegbaar`); neither is drawn
yet. Rijkswaterstaat's Vaarweginformatie WFS (`app:Opening`) adds the mechanism of
each opening span and its clearance, for when they are.

**Atlas and the client measure a deck the same way** (`DeckMeasure`,
`DeckProfile::fit_over`: the surface every metre inside the outline, at the deck's
own tile-local points), and atlas publishes the result so a client standing on its
graded ground asks nothing. **`carto-view --atlas URL --site URL` draws exactly
that** — the graded ground, each deck on its profile, the piers — and prints each
approach's step from the deck's top to the surface three metres out (another
deck's top where the road runs straight onto one, in any loaded cell). Measured
over 20 cells of Groningen (2026-09-24, AHN6, RD 230000–236500 × 578500–583000
graded): 36 lifted decks, all profiled; 146 approaches, mean step 0.26 m, 140 of
them within 0.61 m — where the flat median had left the Emmaviaduct view at 1.98 m
mean.

**Over the whole of Groningen as published** (49 graded tiles, 44 lifted decks,
175 approaches, measured with `carto-view` against the live atlas the same day):
mean 0.70 m, median 0.35 m, 10 over 2 m. Two causes were fixed at once. Two of
the 44 profiles ended on an outlier — 22.1 m against a 6.2 m deck, 5.9 m against
10.2 — which the running median cannot reach, so `DeckProfile::fit` now checks
each end against the line through its two neighbours. And `extend_approaches`
stopped at the first fork, so an interchange arm splitting at its abutment got a
few metres of ramp; every branch carrying on ahead is now an approach of its own.
After both, graded again locally, and **measured with two instrument faults
fixed** — an approach that never leaves its deck (a slip road inside a wide
viaduct's outline, cut by the tile edge) is skipped rather than probed at its end
over the ground under the deck, and the ground is read from each cell's own graded
tile rather than a field built at the first cell's metres, which sat 8 m off six
columns east: **178 approaches, mean 0.13 m, median 0.09 m, 176 within 0.5 m.** Most
of the earlier outliers were the instrument. The two left (2.8 and 3.7 m, Laan
1940–1945 south) are real: two low slip-road decks (~3 m) inside the big viaduct's
outline, whose approaches run into the ground the viaduct's own ramp raised to
7–8 m. Where two ramps of different heights overlap, which one owns the ground is
the open question.

The 3D basisvoorziening's own bridge objects were checked for this and not taken:
LoD1.2 drapes of the same BGT polygons, dated (one new deck came out 2.83 m against
a DSM of 9.03), and on small viaducts over the ring the drape pulls outline corners
down to the trench floor.

## A deck laid flat stands on its own street (2026-10-11)

"One level per tile" (above) was fixed for the *lifted* decks only. A deck under
`MIN_LIFT_M` — every canal bridge — still took the cell's shared `deck_level`
plus zero, and that level is the median ground over **every** deck in the cell,
viaducts and embankments included. Logged per deck round the Herebrug (9 cells,
51 flat decks): 40 stood more than 0.5 m off the street they carry, 28 more than
1 m, the worst 5.45 m. In the pictures that was the Trompbrug, the Oosterbrug and
the Plantsoenbrug half or wholly under the canal, the low Euvelgunnerbrug below
its quay, and the Herebrug as two slabs at different heights.

`map_geometry::flat_deck_road_level` stands a flat deck (and the carriageway cut
out of it) on **the solved height of the street it carries** (`road_levels`,
level 1, nearest each outline vertex within 6 m). That is the first choice
because a tile border cuts bridges as readily as anything else, and the solver
works over a window wider than a tile: the Herebrug is cut 7 m from its north
end, and its two halves now meet within 4 cm. Where atlas has solved no street
(7 of the 51), `flat_deck_level` takes the ground 3 m past where each approach
leaves the outline, which agrees with the solved road to 0.13 m on average. The
deck metrics in `--dump-state` say nothing about any of this — `DeckPlates` keeps
lifted decks only — so the check is `docs/bridge-shots.toml`, eleven bridges
with the aerial photograph beside each.

**A flat deck can still climb, and then the street is the wrong answer.** The
H.N. Werkmanbrug — the footbridge from the station to the Groninger Museum,
which Groningen calls the Museumbrug — rises from 3.3 to 4.4 m NAP over a
2.5 m street and carries on at 4.5 m straight across the museum's middle
(AHN's DSM along the walkway, 2026-10-12). Its clearance over the median ground
around it is under `MIN_LIFT_M`, so it is flat, and the solved road bridged the
museum's hole in the DTM at about 3.4 m: a metre short at the far end. Atlas
now keeps a flat deck's measured profile when
`terrain_grade::flat_profile_fits` says the surface is the deck's — it rises
less than a lift end to end, and every end whose bare ground the survey has is
within 1 m of it, with at least one such end — and pins the span's road to it
as it pins a lifted deck's, without counting it as a span anything passes under.
The check exists for trees: a boardwalk under a canopy measured 11–23 m. The
profile is published in `decks/14/x/y.json` like a lifted deck's, and every
client already stands a deck on a published profile whatever its height.

**A building standing in water was measured from its foundation.** 3DBAG takes
the museum's walls down to −8.0 m NAP; atlas stored heights above a shell's
lowest point and the client stands that point on the surface it draws — the
canal at 0.8 m — so the museum stood 8.8 m too high and the walkway ran into its
wall. Atlas's `lod22` now measures a building whose foot is mostly inside a
registered water body, and whose shell reaches at least half a metre below it,
from the water's level (`Water` in `pipeline/lod22.rs`), and the client's cell
cache keys moved to `v3` so a cached cell does not keep the old one.

## The heights are right; what read as "too low" was a bare slab (2026-10-19)

A phone report ("in reality this bridge is higher", the Berlagebrug from its
quay) was checked bridge by bridge before anything was changed. A temporary log
of every deck `resolve_bridge_decks` placed, over 31 of Groningen's largest
named bridges' cells, matched to OSM outlines and set against the AHN surface's
median inside each outline: **53 of 56 drawn within 0.5 m of the survey**,
lifted and flat alike — the Berlagebrug at 5.35 m NAP, exactly. The three out
were the Emmaviaduct (−1.2 m, its median against a profile that runs 6.7 → 9.5),
the Visserbrug (+0.8, flat on its street) and one mismatch of the instrument.
Two traps in checking it:

- **A shot's altitude counts from the field's datum, not the ground, unless the
  shot says `on_ground = true`.** Over Groningen the datum is ~7.2 m NAP, so a
  "2 m above the canal" camera stood at 9 m, above the deck it was meant to look
  up at, and the deck read as low.
- The log fires once before the terrain field is ready (`levels` are then the
  bare clearance) and again after; only the `terrain.is_ready()` pass is the
  drawn height.

What the comparison against the aerial photographs did show is that a deck was a
slab and nothing else: no railing, no structure above the road. From the street
a slab over a canal reads as a pontoon. So decks carry **railings** now
(`vector_tiles::railing_runs`, meshed by `furniture::build_railing_meshes`):
posts every 2.5 m with a handrail and a mid rail, 1.1 m, along the outline —
which is the parapet's line, since an outline is the structure — and open where
the road runs off. The ends are found from the carriageways on the deck **and**
from the approaches: the Oosterbrug's bridge way stops a few metres inside its
outline at both ends, its carriageway reached no edge, and the first cut railed
it shut on all four sides. A border cut, a pavement beside an end and a road
only grazing a corner stay open; a surveyed wall or fence running along the
line (within 20°, not just near it) wins over the railing; a notch in the
outline under 3 m deep and 8 m of outline long is crossed straight — the
Oosterbrug's sides step in ~2.6 m where the bascule's mechanism stands, and a
railing following that in and out read from the quay as gaps in it; a deck no road reaches at all is railed down its long sides
only, or not at all when about square. The meshes ride `PendingDecks::roads`
with the carriageways, so they stand on the deck's height and bend along its
profile, and they are `SurfaceGroup::Furniture` — the street furniture's
material and its altitude fade.

## Where OSM's bridge way, its outline and its water disagree (2026-10-19)

The three are drawn by different hands and rarely meet. At the Oosterbrug the
bridge way stops a few metres inside the outline at the north, and starts a
metre over the canal before the outline does at the south. Each mismatch had a
visible fault, all three hidden by one another until the first was fixed:

- **Mounds under the deck.** The street at grade that carries on from a bridge
  way inside its outline was conformed to like any road, so the canal bed under
  the deck rose to it with a 1:2 shoulder: two humps meeting at 1.37 m NAP over
  a bed at 0.30, an arch seen from the water. Atlas's grading now leaves the
  ground **in registered water inside any bridge's outline** alone — every
  outline, `Cell::outlines`, not only the decks it pins roads to: the
  Oosterbrug is flat with no profile kept, and the first cut missed it.
- **A hole across the deck's end.** That same street was drawn on the ground,
  cut out of the slab like any road on it, and once the mound was gone it lay
  on the canal bed under open water. `roads::carried_onto_deck` puts a street
  on the deck from an end that meets a bridge way's end inside the outline, to
  the outline's edge. A street meeting no bridge end — one passing under a
  viaduct — stays on the ground.
- **A strip of water between the deck and its quay.** The bridge way's own
  overhang past the outline stood on nothing. Atlas lays the ground to a bridge
  way's end stretch outside every outline that runs into one, up to
  `ABUTMENT_MAX_M` (6 m) — an abutment. A footbridge with no outline is not
  dammed.

Checked by grading 25 cells round the Oosterbrug on a build node into a scratch
tree (`atlas run --out … --bbox … --datasets terrain`, water and BGT paving
copied in from the live host first) and pointing the client at a loopback
stand-in for `/terrain`, `/roads` and `/decks` that proxies everything else.
Over that window the node's AHN6 index answered 400 and atlas fell back to
AHN4, which the live grading does not use; a local grade of unchanged code
matched the live tile under the Oosterbrug to 2 cm, so the edition did not
matter there, but a stand-in elsewhere should be compared against a local
"before", not the live host.

## A flat bridge stands on abutments, over its canal, flush with its street (2026-10-20)

What a canal bridge laid flat needs, from the water up, and where each piece is:

- **The canal runs on under it.** The Kadaster's water (`pipeline/water.rs`)
  stops at a bridge's outline, since the BGT's water and its bridge decks tile
  the ground between them, while OSM's canal, which the client draws, runs on
  underneath. So under the Oosterbrug the drawn water had no level and stood at
  the ground's height, and atlas graded that ground as land up to its quays:
  mounds and slanted walls under the span. The water dataset now files a ring
  at the canal's level over every bridge outline with the same water beyond two
  opposite sides (`vector_tiles::spanned_water`, `water::bridge_gaps`: 94 over
  the Groningen window). Not between two levels more than 0.25 m apart, which
  is a lock. **The client's water and the grading both read it, so it needs
  the water dataset re-run before the terrain grade.**
- **An abutment at each end**: a wall from the deck's top 12 m down along each
  end (`abutment_wall_mesh`), with **one face, toward the canal**. A second
  face toward the street showed wherever the pavement dipped a few
  centimetres between its vertices, as a brown band along the end. An end
  that stops short of the register's water is first carried out over it to the
  quay, up to 3 m (`reach_the_quay`), with the slab continued as a lip.
- **The street meets the deck's top.** Atlas lays the ground off each flat end
  to the height the client draws the deck at: its measured top, else its
  street's (`road_levels::flat_deck_level`, shared by both sides so the two
  medians cannot differ). Then it holds a landing at that height, the end's
  width and 6 m deep (`ABUTMENT_LANDING_M`). That is still not exact on the
  client's 3.125 m field: the first sample past the end is carved with the
  canal under the deck, and `land_at` averaged dry samples two spacings away,
  which reached the grass bank falling to the water beside the bridge. So
  `land_at` now takes the nearest dry ring first, and the surfaces off a flat
  end are stood on the deck's height for 3 m, fading into the field over 2 m
  more (`map_geometry::Landing`). Measured off the Oosterbrug's south end before
  this: the street 0.1–0.5 m under a deck at 4.66 m NAP.
- **Walkable**: `walk_deck_top` puts a flat deck's top into `ground_at`, so the
  avatar stands on the slab, not on the canal bed beneath it.

Checked with the same loopback stand-in as above, now also serving `/water`
from the scratch tree after `water::bridge_gaps` ran over it.

## Storage, if the raster is ever baked instead of proxied

Measured from real 200 × 200 m patches (a dense rail yard and open farmland;
**rural compresses worse**, 455 KB vs 329 KB, because an urban DTM is full of
no-data holes where buildings were cut out):

| Form | Resolution | Netherlands, per model |
|---|---|---|
| AHN GeoTIFF as PDOK serves it | 0.5 m float32 | ~385 GB (11.4 MB/km²) |
| Terrarium PNG pyramid | 0.72 m/px (z17) | ~120–160 GB |
| Terrarium PNG pyramid | 1.43 m/px (z16) | ~40 GB |
| Terrarium PNG pyramid | 2.86 m/px (z15) | ~10 GB |

Only the DTM needs a full pyramid; the DSM is only interesting on deck footprints.
Rejected for now in favour of proxying PDOK per request: a deck-sized question is a
48×48 resampled grid — a couple of KB — so the pyramid buys nothing until the layer
has a second consumer that wants whole cells.

## Coverage is a bounding box

`HeightLayer::covers` is AHN's own `lonLatEnvelope` (3.20–7.28 E, 50.72–53.56 N),
which also claims slices of Belgium and Germany the flights never covered.
Tightening it would mean inventing an outline of the Netherlands; the honest answer
there is a patch of pure no-data, which consumers must handle anyway.
