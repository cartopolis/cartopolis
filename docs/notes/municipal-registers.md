---
topic: map
triggers: [municipal.rs, TreeRegister, REGISTERS, bomen, municipal trees, Groningen trees, maps.groningen.nl, WFS, city register, the study quay, BOOMHOOGTE]
updated: 2026-09-05
---

# The trees a city surveys, where the country's register is empty

`systems::surveyed` reads the BGT and is right to: it is national, it is CC0, and
one extractor covers every street in the Netherlands. What it is not is complete.
The BGT's *point* collections are optional content, filled per municipality, and
`surveyed-street-objects.md` already records the consequence — "central Groningen
has no surveyed point objects at all", every point collection returning zero over
a 1 km² box in the middle of the city.

That note treated it as a caveat for capture-taking: don't check the layer in the
centre, because the centre is where the data isn't. This one is what happened when
somebody looked at the map from the ground and asked why the study quay, an avenue of
limes, was a bare lawn.

## The size of the hole (measured 2026-09-05)

Counts of registered trees over the same z14 cells, national register against the
city's own:

| z14 cell | covers | BGT | Gemeente Groningen |
|---|---|---|---|
| 8491 / 5320 | the study quay | **0** | 3,018 |
| 8490 / 5319 | west of the centre | 112 | 5,014 |
| 8492 / 5319 | east of the centre | 230 | 4,415 |

126,060 trees in the municipality. The study quay itself carries 176 in the box
queried — 58 Dutch limes, 27 black poplars, most of them in the 15–18 m band,
germination years 1948 to 2017.

So this is not a gap at the margin. Where the BGT answers at all it answers with
two per cent of what is there, and the one place it answers with nothing is the
dense middle of the city, which is where a person walks.

**OpenStreetMap is not the fallback either.** 20 `natural=tree` nodes in the whole
700 × 500 m box around the study quay, and no `natural=tree_row` ways at all. Nor is
the tile schema: Shortbread's `pois` layer over that cell holds 690 features —
benches, shelters, waste baskets, playgrounds — and has no entry for a tree in it.
Its only vegetation is areas, in the `land` layer, and `scatter.rs` plants over
`forest`, `wood`, `park`, `garden`, `scrub` and `heath`. The strip along the kade
is tagged `grass`, which is on none of those lists, which is exactly why it
rendered as lawn.

## Why the abstraction is per city and not per country

There is no national municipal API and there is not going to be one. Each city
publishes on whatever it already runs, in its own coordinate system, with its own
field names, and its own vocabulary for how tall a tree is. Groningen is a
GeoServer answering WFS 2.0 in RD metres with heights as Dutch prose
(`"15 tot 18 m."`); the next city will not be.

So a `TreeRegister` owns the whole question — coverage, how to phrase a request
for one cell, and how to read the answer — and the shared part is only the far
end: every parser normalises to `RegisteredTree { lat, lng, height_m }`. Adding a
city is one entry in `REGISTERS` plus two functions beside it. No caller changes,
because no caller ever sees a city's own wire format.

Two consequences of that split are worth naming.

**The cache stores the normalised form, not the city's answer.** Groningen's
GeoJSON is ~265 bytes a tree even trimmed with `propertyName` to the geometry and
the height band (615 untrimmed, the layer carries fifteen fields); the normalised
form is ~30. Over a dense cell that is 1.3 MB against 150 kB, and the fat one buys
nothing, since every field it carries was discarded by the parser that wrote it.

**Coverage is a bounding box, not the municipal boundary.** Being too generous
costs one request that answers with nothing. Being exact costs a dependency on a
second dataset — 342 municipal boundaries — that would also have to be kept
current. The box is the layer's own published extent, so it is the city's reach
and not a guess at it.

## A city register replaces the BGT's trees, it does not add to them

This is the opposite of the rule `surveyed.rs` follows against the *scatter*, and
the difference is what is being described. The scatter fills park and wood
polygons while a surveyed tree stands on a pavement: different trees, so adding
them is right. Two **registers** describe the *same* trees — the BGT's 112 in a
cell are a subset of the city's 5,014 — so drawing both plants a second lime a
metre from the first and the pair z-fight their own canopies.

Matching them up by distance was the obvious alternative and is rejected on the
standing rule: it is a tolerance to tune, wrong in both directions, and there is
no threshold that is right for a row of pollards four metres apart and also for a
survey that moved a trunk by three. The overlap is removed instead. Over a cell a
register covers, the city is the source and the BGT's trees are not read; the
cell's BGT *props* and *lines* still are, because no city register here carries
those. The scatter is untouched either way.

The two counts are reported separately (`surveyed_trees` and `municipal_trees` in
`--dump-state`) for the same reason they are not added: one number could not tell
"the city answered" from "no city register here and the BGT's cell is full".

## The height band is the reason to bother beyond coverage

The BGT registers a tree as a point and says nothing about its size, so
`scatter::surveyed_trees` hashed a height out of a 6–11.5 m street-tree band. A
municipal register measures. Groningen publishes eight bands; a registered black
poplar on the study quay is routinely 21 m, which the old band could not express at
all — clamping a real measurement into a guessed range throws away the only thing
the better register knows.

So a measured height is used as given and the crown is scaled to it at the ratio
the band already implied (~0.3, jittered). `None` is ordinary — Groningen leaves
it blank on about one tree in thirty — and reproduces the old behaviour exactly,
so a register with no heights at all still works and looks like the BGT's trees do
today.

The two open bands are the only judgement in the parser. `"tot 6 m."` is taken at
three quarters of its ceiling, because a tree small enough to sit at the bottom of
it is a sapling the register does not carry; `"24 m. en hoger"` a little above its
floor. Bands are read off the numbers in the string rather than matched against a
table of the eight spellings seen today, because the register is a city's
operational database and a ninth band costs nothing to add there.

## Things that were checked and are not problems

- **CORS.** `maps.groningen.nl` echoes `Access-Control-Allow-Origin`, so the web
  build reaches it directly and needs no `/up/` proxy in the Caddyfile — unlike
  AHN, which needs one.
- **Truncation, and the ceiling that was wrong first.** The request carries a
  per-cell `count` so one cell cannot hand the streamer an unbounded download.
  It was `scatter::MAX_TREES_PER_CELL` (6,000) for one run, and that was a
  mistake worth recording: the scatter's cap bounds a *generated* lattice, where
  the number is a decision, while this one bounds a count of *real* trees, where
  any cell it cuts is a cell drawn wrong. Two Groningen cells were truncated at
  6,000 out of 6,263 and 6,808. `MAX_REGISTERED_TREES_PER_CELL` is 12,000 — about
  20× the municipal mean (126,060 trees over 447 km², against 2.14 km² in a z14
  cell here) and 1.8× the densest cell measured. GeoServer reports
  `numberMatched` alongside `numberReturned`, so a cell that hits the ceiling is
  logged rather than silently thinned — its page order is arbitrary, so a
  truncated avenue would otherwise read as the register's own answer.
- **The request box.** RD's grid is rotated against Web Mercator's, so the box is
  built from the cell's corners *and* its edge midpoints, the same care
  `terrain::field_request` takes. It is deliberately a little generous;
  containment is settled afterwards in the cell's own frame.

## What is still open

- **One city.** Every other municipality with the same hole needs its own entry.
  The pattern is there; the work is finding each city's endpoint and its field
  names.
- **Never seen on a device.** Verified headlessly and by direct queries against
  the register — a capture on the study quay pavement reports
  `municipal_trees = 32,398` standing in the loaded cells with `surveyed_trees = 0`
  beside it (the BGT correctly not read over covered ground), and the avenue is
  drawn. Colour and lighting are not evidence on this container's software
  rasteriser, so `municipal_trees` in `--dump-state` is the half that is.
