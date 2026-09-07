---
topic: map
triggers: [municipal.rs, Register, Source, Yields, Supply, REGISTERS, bomen, lichtmasten, municipal trees, street lighting, Groningen, maps.groningen.nl, WFS, city register, the study quay, BOOMHOOGTE, cache fingerprint]
updated: 2026-09-07
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

## What a register produces: shapes the client already has

The rule that stops this growing a vocabulary of its own. The client can already
stand four kinds of thing on the ground — a tree with a height, a prop with a
`FurnitureKind`, a paved polygon, a boundary line — and each has a builder. A
register's parser normalises into one of those and nothing else; whatever the city
knows that does not fit is dropped **at the parser**, where the city's vocabulary
is still understood, not carried inward as a field only one city fills.

So `RegisteredCell` mirrors `SurveyedCell`: one vector per shape, not one enum
with variants. The consumers genuinely differ — trees are merged into the canopy
meshes, props go through `furniture::place_surveyed`, which faces each one at the
nearest street. A city's lamp post is turned to the kerb by the code that turns a
bench, for free.

Street lighting was the second source added, and it is what proved the shape. It
is also the largest hole this fills: the client drew no street lighting anywhere,
because the tile schema has none and the national register's pole collection is
empty over central Groningen — the same hole as the trees, one layer over. The
city's own carries 493 columns in one cell, of which 481 are `Lichtmast`; the
8 ground spots, 3 tunnel lights and 1 clock are **refused**, because a ground spot
drawn as a four-metre column is worse than not drawing it. That is why the reader
returns an `Option`.

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

Suppression is **per kind, not per cell**, which is what `Source::supplies`
declares. Groningen registers trees and lamp posts and says nothing about
benches, so suppressing the whole BGT cell over its ground would delete the
benches in order to fix the trees.

The counts are reported separately (`surveyed_trees` / `municipal_trees` and
`surveyed_props` / `municipal_props` in `--dump-state`) for the same reason they
are not added: one number could not tell "the city answered" from "no city
register here and the BGT's cell is full".

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

## The cache invalidates itself

Nothing revalidates a stored cell — the sweep is by size, not by age — so a reader
that changes and a key that does not is a permanent wrong answer. This tree has
shipped exactly that: `surveyed::cell_cache_key` carries a hand-bumped `v2`
because a fix to the extractor could not reach cells already written under `v1`,
and every install kept its ghost pavements until the key moved.

A constant somebody has to remember to bump is the same bug waiting. So the key
carries a **fingerprint of the reader's own behaviour**: each source's reader is
run at startup over a fixture page embedded beside its register, the normalised
result is hashed together with the layer and attribute names, and that hash is a
path segment. Change the layer name, the attribute, the height bands, or which
lamp types are refused, and the key moves and the old entries are never read
again. Nothing to remember.

The fingerprint is FNV-1a rather than `DefaultHasher`, deliberately: the number
goes into a cache key that must mean the same thing on every platform and every
build, and std makes no stability promise about its hashers across releases. It
is logged at startup per source, because "why did every cell re-download after
that release?" is otherwise unanswerable from a log, and the answer is always
this number.

**Its one limit, stated plainly: the fixture is the contract.** A reader change on
an input the fixture does not contain will not move the fingerprint. The fixture
is sized to carry every branch each source decides on, and two tests hold that
line — one asserting every source reads something out of its register's fixture,
one asserting an altered reader fingerprints differently from the real one.

`CACHE_GENERATION` survives as the second axis and stays a hand-bumped constant,
because it is for what a fingerprint cannot see: a change to the stored *format*,
which invalidates every source of every city at once.

## What it costs — and the cost is latency, not bytes

The first version of this note said a dense cell was 1.3 MB. That was the
decompressed size and it is not what crosses the network: the server gzips, and
`ureq` is built with its `gzip` feature so it asks for that and inflates
transparently. Measured 2026-09-07 over the densest Groningen tree cell:

| | decompressed | on the wire |
|---|---|---|
| GeoJSON, all 15 fields | 751 kB | — |
| GeoJSON, `propertyName` trimmed to two | 324 kB | **27 kB** |
| the same as CSV | 120 kB | 17 kB |

So bytes are a non-issue, and CSV was measured and **not** adopted: it saves
10 kB on the wire and costs a hand-rolled WKT parser, against a JSON parse that is
already far below the ~120 ms an MVT rasterise costs on the same worker.

What is real is that every source is one more request standing between a cell's
ground and its mesh job, so they are fetched **together**. Natively that is
presentation rather than overlap — `http::fetch` is a blocking `ureq` call with no
await point in it, the same thing `map_geometry` records about the two optional
layers it already zips — and on wasm it is a real overlap. On both it is what
stops a second source doubling the wait.

Two more things chosen for cost: the request carries `propertyName` so it asks for
the geometry and the one attribute the reader looks at (615 bytes a tree against
265), and `CARTO_NO_MUNICIPAL=1` turns the whole layer off for a bisecting
capture, read once through a `LazyLock` rather than as an environment lookup per
cell on the mesh path.

**Multiple layers in one request does not work here.** GeoServer answers a
`typeNames` list with two entries with a 400, so one request per source is forced
and concurrency is the only lever.

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
