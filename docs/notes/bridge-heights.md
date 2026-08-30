---
topic: bridge-heights
triggers: [height.rs, map_geometry.rs, vector_tiles.rs, road_stitch.rs, AHN, PDOK, bridge, deck, viaduct, "grey slab", "markings stop", elevation, "EPSG:28992", RD New, terrain]
updated: 2026-08-30
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
approach ribbons with it. Not done.

There is a second place a deck height already exists, and it does not help
either: the Kadaster's 3D basisvoorziening ships the BGT's
`Overbruggingsdeel_vlak` with an AHN-draped z (45 decks in the 2 km Groningen
block, one viaduct running 0.96 → 6.62 m, measured 2026-08-30). It is the same
AHN measured above, so it supplies a number this note already has, and the
constraint was never the height — it is the flat approach roads.
[3d-basisvoorziening.md](3d-basisvoorziening.md)

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
  `TerrainField::level_over` across the whole deck mesh once and hands that number
  to both, rather than letting the ribbon ask the field again over its own narrower
  footprint and land a few centimetres off the thing it was cut out of.

There is no kerb casing on a deck: what lies beside the carriageway up there is the
structure, and that is the part of the slab the road was cut out of.

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
