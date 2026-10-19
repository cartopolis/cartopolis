---
topic: map
triggers: [municipal.rs, pipeline/registers.rs, formats/registers.rs, "registers/", Register, Source, Yields, Supply, REGISTERS, bomen, lichtmasten, municipal trees, street lighting, Groningen, maps.groningen.nl, WFS, city register, the study quay, BOOMHOOGTE, FRACTIE, containers, laadpalen, fonteinen, parkeervakken, parking bays, park_in_bays, Wikidata, public art, statues, artwork, benches, speelplekken, Kunstpunt, kunstsoort, Overpass, osm_has]
updated: 2026-10-19
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

## What is read today, and from where

Atlas's `registers` dataset (`crates/atlas/src/pipeline/registers.rs`) reads
these, counted over the whole municipality, and writes them per cell to
`registers/14/{x}/{y}.json` (`cartopolis_geo::formats::registers`):

| source | count | becomes | notes |
|---|---|---|---|
| trees | 126,074 | tree with a height | eight height bands |
| street lighting | — | `Lamp` | 481 columns per 493 points in one cell; ground spots, tunnel lights and a clock refused |
| waste containers | 1,962 | `Recycling`, coloured | 1,277 household, 379 paper, 208 glass, 98 textile |
| charge points | 611 | `Cabinet` | one status value today; anything else refused |
| fountains | 34 | `Fountain` | all `Fontein` |
| bus shelters | 286 | `Shelter` | all `Abri` |
| traffic lights | 2,047 | `TrafficLight`, `Cabinet` | the controller cabinet is a box |
| ticket machines | 357 | `Cabinet` | OSM has 68 |
| parking bays | 34,994 | one parked car each | see below; OSM has 1,741 `parking_space` |
| statues and memorials (Wikidata) | 16 added in the area | `Artwork` | only those OSM lacks; see below |

## Atlas reads the city; the client reads atlas (2026-10-19)

Until 2026-10-19 the client asked the city's GeoServer itself — one request per
source per cell, nine sources, carrying the city's layer names, field names and
vocabulary in `municipal.rs`, and a *reader fingerprint* hashed into every cache
key so that changing any of those invalidated what installs had stored. All of
that moved into atlas, for three reasons:

- **The server answers a whole layer at once.** All 126,074 trees in one request,
  ~2 s, 2.9 MB gzipped (measured 2026-10-19); the client's per-cell path was nine
  requests standing between every cell's ground and its mesh job.
- **One path for every Dutch layer.** Every other register reaches the client as
  an atlas cell behind a coverage manifest; this was the only one that went to a
  third party from the phone, with its own breaker and timeout.
- **The city's vocabulary no longer ships in the APK.** A city that renames a
  layer or adds a height band is an atlas re-run, not a release.

Two things about the request, each of which broke something once:

- **`srsName=EPSG:28992` is load-bearing.** By 2026-10-01 the GeoServer answered
  in Web Mercator metres when nobody asked, the reader took them as RD, and every
  tree and lamp post landed in another country — and because the city's register
  *replaces* the BGT's, Groningen drew street trees from neither. A page that is
  not RD fails the run rather than being written.
- **No paging.** GeoServer refuses `startIndex` on these layers without a
  `sortBy` ("Cannot do natural order without a primary key"), and each layer
  names its columns differently. Not needed: every layer comes back whole, and a
  response short of its own `numberMatched` fails the run rather than writing a
  thinned city. The geometry property is `GEOM` on most layers and **`geom`** on
  the parking bays, and `propertyName` with the wrong case is a 400.

**Which kinds a register speaks for travels in the cell file** (`supplies`), and
every cell in a run's area is written even when it holds nothing: a covered cell
with no lamp posts in it is the city's word that there are none. The client's
cache key carries the run's stamp from the manifest, as the dikes' does, so a
re-run (a felled tree, a new statue) is never answered from an old copy — the
role the reader fingerprint used to play.

**Coverage is now what atlas ran**, not the city's whole extent: a cell the
operator has not run `registers` over gets the BGT's trees, as anywhere else.

## Parking bays: the cars stand where the bays are

The city surveys every public parking bay as its own polygon — measured over a
square kilometre of the centre on 2026-10-19: 727 parallel bays (median 5.7 ×
2.0 m), 122 perpendicular (4.9 × 2.3), 38 herringbone (6.8 × 2.2). The layer
also says what each is for (707 electric, 471 disabled, 131 loading, 76 drop-off
across the city); none of that changes what is drawn, so atlas drops it.

`parking::park_in_bays` puts one car in each bay, along its long side (true of
all three layouts), centred, a quarter of bays empty. Whether a given bay is
taken is the one invented fact, hashed off the bay's position, so the claim the
lot fill makes (`parked-vehicles.md`) holds: the cars are vehicles, not
interactables. Bays too small for a car (under 4.3 × 1.65 m) or too long to be
one bay (over 12 m) get none.

**A car park with a surveyed bay in it is not filled as well**
(`extract_parking_around`): two fills of one lot put two cars in every bay. By
containment of the bay's centre in the lot's tessellation, which is exact and per
lot, so a supermarket's private car park that no city surveys is still filled.

## Public art: Wikidata's statues, added to OpenStreetMap's

Gemeente Groningen publishes no register of its public art (checked 2026-10-19:
the GeoServer's 75 layers and the city's datasets on data.overheid.nl; its
`Monumenten` layer is buildings only). Wikidata has 562 items of the broad art
classes (sculpture, work of art, memorial, monument, …) inside the municipality,
most imported from Kunstpunt Groningen's "Kunst op straat" catalogue.

**The broad classes cannot be used: half of them are not in the street.** The
first capture of this layer stood "Wandsculpturen met torsende mannen" — two
figures on a façade, filed as a plain `sculpture` — in the middle of a street. A
hand-checked sample of 40 items against Kunstpunt's pages: 18 free-standing, the
rest on or in a building, wall sculptures, reliefs, light and new-media works,
glass, paintings, or set in the paving. Nothing on Wikidata separates them.

**Rejected: reading Kunstpunt's pages for their art type** (`Kunstsoort`). It was
built and ran once on a build node (2026-10-19, ~480 page reads) and was taken
out the same day on the owner's instruction: we do not scrape third-party sites.
Kunstpunt's REST API is closed and it publishes no export. The proper routes are
asking Kunstpunt for an export or permission, or adding the distinction to
Wikidata — both decisions for a person, not for atlas.

So atlas takes only the **unambiguous classes** — statue, bust, group of
sculptures, war memorial, memorial stone: 64 over the municipality — and drops
any that is also a relief, plaque, gable stone, mural, tile panel, mosaic,
memorial wall, Stolperstein, grave monument or fountain by class, or
architectural sculpture, light, new media or land art by genre (`P136`).

These **add to** the tile's `tourism=artwork` rather than replace it (OSM has 173
artworks over the city, far more than 64). A Wikidata work OSM already has is left
to OSM: atlas asks our own Overpass once for the area's artworks and skips a work
an OSM object names in its `wikidata` tag, or one standing within 25 m of an OSM
artwork. That 25 m sits in a measured gap — the unlinked works nearest an OSM
artwork are 0–21 m from it, the next 36 m — so it separates two registers placing
one statue from two statues. Over the `groningen` area (2026-10-19): 27 kept, 11
already in OSM, **16 added**; 30 dropped as on a wall or in the paving.

Wikidata's coordinates are rougher than a survey — usually metres, sometimes
tens — so a statue is on the right square, not necessarily on its exact spot.

## Asking external services as little as possible

A `registers` run asks each source **once**: one WFS request per city layer (nine
for Groningen), one Wikidata query, one query to our own Overpass. Every answer
goes through atlas's source cache (`pipeline/http.rs`), so a re-run within a month
asks nobody anything; `--fresh` is the deliberate exception. The client asks only
our host. Before 2026-10-19 every client asked the city's server nine times per
cell it streamed.

## What else is available, and what was not taken (2026-10-19)

- **Benches have no better source than OpenStreetMap's 2,169.** The city publishes
  no bench register, and the BGT's `straatmeubilair` over the whole city holds
  only what the *province* registered on its own grounds (P0020): 249 objects,
  8 benches. The city's play-area register (`Speelplekken`, 3,840 items) does
  carry 385 benches, but only at playgrounds, and 141 of them are within 10 m of
  an OSM bench — the same benches or their neighbours, which cannot be told apart
  without a tolerance. Not taken.
- **Play equipment** (`Speelplekken`): swings (`1.2*`, ~400), slides, climbing
  frames, goals, 56 table-tennis tables. Not taken: it needs a judgement about
  forms the furniture table does not have.
- **The national monument register** (RCE, `ps-ch:rce_inspire_points` on PDOK): 858
  points over the city, with no name or type, so statues cannot be picked out.
  Wikidata already carries the listed memorials with their classes.
- **RDW's parking open data** (`b3us-f26s`, area manager 14): 17 facilities with
  capacities — garages and P+R, not bays.
- **The city's own copy of the BGT** (`geo_info:BGT_*`): not read; we have the
  national one.

## The fraction is a colour, not a kind

The container layer is the case that shaped how attributes are carried. Glass,
paper, textile and household waste differ by **colour, not by form** — a bottle
bank and a paper bank are the same six boxes — so four kinds would have meant
four meshes and four entries in the furniture table for one object.

`FurnitureInstance::paint` already existed for exactly this: a per-instance sRGB
body colour on a shared mesh and material, added so a car park's worth of cars
would not all be one grey. `nav_marks` already paints from a register's own
colour vocabulary rather than from the furniture table's tones, which is the same
footing these colours sit on. Two changes made it reach containers: the recycling
form's body tone became `Paint`, and `furniture::default_paint` returns the
painted steel that body used to be — so an *unpainted* container, which is every
one that comes from OpenStreetMap, looks exactly as it did.

**Household waste keeps the steel**, which matters because it is 1,277 of the
1,962: the commonest container in the city does not change appearance at all, and
the three sorted fractions are the ones that gain a colour.

## Suppression has to reach the tile, not only the national register

Trees and lamp posts exist in the national register and not in the map tiles, so
suppressing the national copy was enough. A container is in **both** — the tile's
points layer carries `amenity=recycling` — so filtering only the national
register would still have drawn every container twice. The kinds a city supplies
are now filtered out of the tile-derived furniture as well.

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

## What is still open

- **One city.** Every other municipality with the same hole needs its own entry
  in atlas's `REGISTERS`: its endpoint, its layer and field names, its readers.
- **Artwork names.** Wikidata has the title of every statue; the cell format
  carries none yet, so the proximity prompt says "Artwork".
- **Most of the city's sculpture is not drawn.** ~370 works Wikidata files as
  sculpture or work of art are left out until something open says which stand
  free (above).
- **Bay markings.** The register says which bays are marked (`MARKERING`); the
  white lines are not drawn.
- **The parked cars on bays have not been seen on a device.**
