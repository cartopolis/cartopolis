---
topic: bridge-heights
triggers: [height.rs, map_geometry.rs, vector_tiles.rs, road_stitch.rs, AHN, PDOK, bridge, deck, viaduct, "grey slab", "markings stop", elevation, "EPSG:28992", RD New, terrain]
updated: 2026-09-24
---

# Where a bridge's height comes from, and what it costs

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
mean. **The six left over 2 m are at interchange arms that fork at the abutment**
(Laan 1940–1945 north, 7.75 m): `extend_approaches` stops following a street at a
fork, so such an arm gets a few metres of ramp — 1.1 → 6.5 m in 14 m — and then
ground. Following the fork's continuation is the next step there.

The 3D basisvoorziening's own bridge objects were checked for this and not taken:
LoD1.2 drapes of the same BGT polygons, dated (one new deck came out 2.83 m against
a DSM of 9.03), and on small viaducts over the ring the drape pulls outline corners
down to the trench floor.

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
