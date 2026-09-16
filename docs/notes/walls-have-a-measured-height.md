---
topic: map
triggers: [surveyed.rs, bgt.ts, bv3d.ts, LineKind, SurveyedLine, scheiding_vlak, scheiding_lijn, kademuur, walbescherming, quay wall, retaining wall, 3D basisvoorziening, "why is this wall two metres tall", "why is there no wall here"]
updated: 2026-09-16
---

# A wall is drawn at the height somebody measured

The BGT records that a wall exists and exactly where it runs. It does not record
how tall it is — plain BGT is flat geometry — so until 2026-09-16 every surveyed
boundary in this client was drawn at its form's nominal height, and
`LineKind::Wall` is a 2.0 m slab.

That single constant is why most of the register's walls were never shipped at
all. Measured over one 2 km block of Groningen (PDOK's BGT OGC API against the
Kadaster 3D basisvoorziening 2025, 2026-09-16):

| `Scheiding_vlak` type | n | median | p90 | max | ≥ 0.5 m |
|---|---:|---:|---:|---:|---:|
| `muur` | 240 | **0.23 m** | 1.46 | 3.75 | 35 % |
| `kademuur` | 65 | **1.29 m** | 2.81 | 4.45 | 83 % |

Two metres is wrong at both ends: it turns a third of the register's `muur` —
which is garden edging and kerb lines — into walls, and it is the reason the
extractor's `LINE_TYPE_FORM` deliberately excluded `kademuur`, `damwand` and
`walbescherming`, with the note that drawing them "would stand a metre of masonry
along every canal". That objection was correct **about the form** and wrong as a
permanent exclusion of the data, and it cost the map every quay wall in the
country.

## The join is exact, and that is the whole reason this is cheap

The Kadaster's 3D basisvoorziening is the same BGT, draped and extruded against
AHN by 3dfier and published as CityJSON. Its objects carry `gml_id`, which is the
flat API's `lokaal_id` **verbatim** (`G0014.404f925a43be0015e053660013ac6945`) —
so the height is a string-keyed lookup, with no geometry matching and no
tolerance to tune. Measured over the study box: 60 of the 65 current
`scheiding_vlak` polygons joined.

## Polygons only, and the consequence is stated rather than worked around

Every table in that file ends in `_vlak`. There is **no `Scheiding_lijn`**, so a
boundary surveyed as a line has no counterpart and can get no height.

The rule that follows is in `bgt.ts` as `MEASURED_ONLY_TYPES`: the retaining
structures at the water's edge are emitted *only* when a measurement was found.
In practice they therefore still do not come through `scheiding_lijn` — which is
the old behaviour, now derived from a principle instead of from a hand-maintained
deny-list, and which turns itself on the day a height source for lines exists.

What *does* come through is `scheiding_vlak`, the same register's boundaries
surveyed as areas, which this pipeline had never read. The client half had been
waiting since 2026-08: `LINE_KINDS` position 1 was already `Wall`. The polygons
are reduced to a centreline by the same principal-axis code the hedges use — a
wall polygon is the same long thin strip, and walking its outline draws every
wall as a hollow rectangle of wall.

**The wall at the study pond in Groningen is recorded only as
`scheiding_vlak`/`kademuur`**, which is why a 2.06 m wall a person can stand and
touch was absent from the map through both gaps at once.

## A low number means flat, not low

The 3D z is a drape, so an object with no real elevation comes back as a few
centimetres of AHN noise — `OverigeConstructie_vlak`'s "lage trafo" has a median
of 0.08 m. Anything under about 0.3 m should be read as *no measurable height*,
which is what `furniture::MIN_MEASURED_LINE_M` does: below it the boundary is
skipped rather than drawn flat, because four centimetres of masonry is a strip of
z-fighting on the pavement and not a low wall. Nothing is filtered in the
extractor — its job is to report the measurement, and the client's is to decide
what is worth a mesh.

`MAX_MEASURED_LINE_M` (6 m) clamps almost nothing (the block's tallest quay wall
is 4.45 m) and exists so a bad join cannot put a building-height slab down a
street.

## Rejected

* **Deriving a height for the `scheiding_lijn` walls from AHN in the extractor.**
  It is the only way to measure those, and it needs a WCS GeoTIFF sampler in
  cartopy, which has no AHN access at all today. Worth doing; not worth blocking
  the walls that *can* be measured on.
* **Matching a line to a nearby measured polygon.** A tolerance where an exact key
  already exists for the objects that matter. The same rule the rest of this
  codebase follows: remove the conflict rather than arbitrate it.
* **Shipping a per-type constant instead of a measurement** (a `kademuur` is
  1.3 m, a `muur` is 0.25 m). It is the median of a distribution whose p90 is six
  times its median, and it would draw the study pond wall at less than two
  thirds of its real height while claiming to know better than the register.
