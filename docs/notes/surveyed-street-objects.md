---
topic: map
triggers: [surveyed.rs, bgt_objects.py, bgt.ts, scatter.rs, furniture.rs, BGT, trees, lampposts, street lighting, PDOK, plus_type, niet-bgt, scheiding_vlak]
updated: 2026-09-16
---

# Streets that were actually surveyed

> **The boundary half of this note has moved on.** `scheiding_lijn`'s bank
> protections were excluded here because a fixed 2 m form would stand masonry
> along every canal, and `scheiding_vlak` was described as a client half waiting
> for an extractor. Both were resolved on 2026-09-16 by giving a wall the height
> somebody measured — see
> [walls-have-a-measured-height.md](walls-have-a-measured-height.md).

Two of this client's layers have always said, in their own module notes, that
they are inventing something. `scatter.rs`: Shortbread's `land` layer carries
canopy *areas*, "so the trees here are scattered, not surveyed". `furniture.rs`
reads the `pois` layer, which has no street lighting in it at all — so until now
the map had no lampposts anywhere.

The BGT — the Netherlands' large-scale topography, ~20 cm — registers both,
object by object. cartopy's `server/pipeline/bgt.ts` reads it and `systems::surveyed` streams
it.

## The API behaves nothing like the BAG WFS next door

Same organisation, same portal, opposite design, and the differences are not
cosmetic:

- **Cursor paging, not `startIndex`.** So there is no 50 000-row wall and no bbox
  to split, which is the whole of what the BAG extractor had to be built around.
- **No total, ever.** `numberMatched` is absent, so a walk cannot be checked
  against a count the way the BAG extract checks itself. The guard here is a page
  ceiling that warns when it trips, which is weaker, and there is nothing better
  available.
- **Every historical version is returned, in two shapes, and one filter is not
  enough.** This is the one that silently ruins an extract — twice. A page of
  Groningen tree points is 1000 features of which 485 are current; the rest were
  retired years ago and carry an entirely ordinary `status: bestaand`, so
  `termination_date` looked like the discriminator and became the filter. It only
  marks objects *removed from reality*. An object that was merely **edited** gets
  a new version, and every superseded version is marked solely by
  `eind_registratie` — no termination, `status: bestaand` — so the filter kept
  them all. On the trees that was invisible (a felled tree is terminated, a
  re-surveyed one rare); on the paving it was the map's worst z-fight: up to 7
  coplanar copies of one footway, 79 % of Groningen's paved area covered at
  least twice, over half of it in *different* tones once materials landed —
  every copy on the same depth rung, so the pavements shimmered under any
  camera motion (reported repeatedly as "layers fight for z-index while moving").
  A feature is current only when **both** fields are empty; exactly one version
  per `lokaal_id` then survives (measured 2026-08-29: 990 `ondersteunendwegdeel`
  features → 302 current, all ids distinct, and the remaining overlap between
  current surfaces is *zero* — BGT is a true partition). Fixed in cartopy's
  `items()` (every collection walks through it); `parse_surveyed_paving` /
  `parse_surveyed_cell` also drop exact repeats, because a cell cached with the
  ghosts in it is never refetched — which is also why the client's cache keys
  moved to `bgt/v2/`. The `datetime` parameter looks like the right filter and
  returns nothing at all.

## Added to the generated layers, never replacing them

This was the design decision worth getting right, and the second reason is the one
that settles it.

The scatter plants inside park, wood and garden polygons; a surveyed street tree
stands on the pavement, which is none of those. So the two barely overlap by
construction, and adding them does not double-plant.

More importantly, **the BGT's tree points are optional content** ("IMGeo plus")
and coverage varies by municipality — including *within* one city. Groningen's
own centre has none: all 86 tree records in the middle of town are retired, while
the wider municipality has 2,340 live ones. A design that replaced the scatter
where surveyed data existed would delete a park's canopy in exchange for a handful
of limes, and would do it in exactly the cells where the city is densest.

So: 2,340 surveyed trees and 1,781 lampposts over Groningen, laid *beside* the
generated layers. Where the register is silent the map looks exactly as it did.

## What it plugs into, and what had to be added

Both `TreeInstance` and `FurnitureInstance` are plain position-and-size structs in
the Bevy-free crate, so surveyed objects go through the *same* builders,
per-cell merging, upload caps, distance LOD, colliders and interaction as the
generated ones. Two small additions were all it took:

- `scatter::surveyed_trees` sizes a tree from its position hash in the
  street-tree band rather than the woodland one — a lime on a canal is not a tree
  in a wood, and reads wrong at forest scale. Only the *position* is surveyed;
  the BGT records no species or height.
- `furniture::place_surveyed` orients each object against the same `WayField` the
  `pois` furniture uses, which is why it takes the tile. That is the reason it
  lives in the geo crate rather than in the client: the field is built from the
  *`streets`* layer of the very tile being meshed, and exporting it would mean
  exporting its scale rule and the trap in its doc comment too.

`FurnitureKind::Lamp` is new — a 4 m column with the head on a short bracket, so
it reads as a street light rather than as a post from any distance.

The **boundaries** — fences, walls and hedges — needed a builder of their own,
because they are lines and everything else here is a point. `build_line_meshes`
stands **one box per surveyed segment**, at that segment's own length. Walking
each line in fixed steps was the obvious alternative and is both dearer and worse:
dearer because the survey's segments average 6.4 m over Groningen, so a 2 m step
would triple the box count for the same fence; worse because a fixed step cannot
land on a corner, so every bend gains a gap on the outside and an overlap on the
inside.

Hedges are surveyed as *areas*, and the extractor reduces each to the line down
its middle — the polygon's principal axis through its centroid, cut at its
extremes. Laying boxes round a hedge's outline would draw every hedge as a hollow
rectangle of hedge. `Tone::Foliage` is new for them, and it is `palette::CANOPY`
so a clipped hedge is recognisably the same substance as the tree crowns above it.

None of the boundaries are solid. A cell holds ~18 km of them at the worst, which
is thousands of collider polygons where the existing index takes circles for props
and footprints for buildings — the same call `moorings` makes about boats, for the
same reason, and the same one to revisit if walking through a garden wall starts
to matter more than the cost.

**Solid and interactable stopped being the same list.** Every furniture instance
used to become both a collider and a prompt, which was fine while the only source
was `pois` — a layer of things people visit. A city's worth of lampposts breaks
that: a column is firmly in the way and there is nothing whatever to do with one.
`world_places::prop_interactable` is the split.

## The paving, and why it does not draw the road

`wegdeel` and `ondersteunendwegdeel` are the road surfaces, and the obvious way
to use them replaces `tessellate_roads` with the survey's own polygons. That was
rejected, and not on effort.

What the OSM path draws is not just a shape. Each ribbon carries a road *texture*
— lane dashes, edge lines, wheel tracks, ballast and sleepers — a kerb casing, and
a rung on the per-vertex depth ladder that keeps a slip road under its motorway
and a level crossing under its rails. **The BGT cannot supply any of it**: its own
road-furniture collections (`weginrichtingselement_lijn` / `_vlak`) come back
`niet-bgt` on every feature over Groningen, so there are no markings in there to
draw. Swapping ribbons for polygons trades a marked, ordered network for an
unmarked flat one and calls it an upgrade because the outline is more accurate.

So the sources are split by what each is good at, and the split is clean because
they then describe *different ground*:

- **OSM keeps the vehicle network** — carriageways, railways — as textured
  ribbons, unchanged, everywhere in the world.
- **The BGT supplies everything beside it**: pavements, cycle paths, parking bays,
  driveways, verges and traffic islands. OSM draws those as a thin constant-width
  line or not at all. A pavement is a shape, not a width, and its edge against the
  tarmac *is* the kerb line.

The one overlap is the footway and cycle network, which OSM does draw as
`RoadClass::Path`. Over a covered cell those ribbons are suppressed
(`tessellate_roads`' `surveyed_paths`) and the polygons stand in their place, so
no ground is drawn twice. `surveyed_paths_drop_the_path_ribbons_and_nothing_else`
pins both halves: the paths go, everything else stays.

Paving takes its own depth rung, above the ground fills and below the road band.
A surveyed pavement and a nominal-width OSM carriageway do not agree to the
centimetre about where the kerb is, so they lap over each other here and there.

**The rung was what held that apart, and it stopped being enough on measured
ground.** It is 1.0 mm of height, and once the Height layer bends each sheet onto
the survey against its own triangles the two disagree about where the ground *is*
by decimetres — so on 2026-09-17 the pavement was photographed standing through
the middle of the carriageway on Weg der Verenigde Naties. Since 2026-09-18 the
carriageways are **cut out of the paving** (`paving::cut_paving_under_roads`),
which removes the overlap instead of ordering it, and the rung is the backstop it
is everywhere else in `the-ground-stack.md`. Every road wins, because the ribbon
is the piece carrying the lane markings, the kerb casing and the depth rung and a
hole cut in it has nowhere to get those back from.

**Simplification is not optional.** The BGT is a 20 cm survey and its rings are
correspondingly dense: one page of `ondersteunendwegdeel` over Groningen is 77,202
vertices, of which **4,792 survive a 10 cm Douglas-Peucker** — 94% of the data is
collinear filler along straight kerbs. Ten centimetres is far below anything
visible at any distance this layer is drawn at, and without it a city's paving is
hundreds of megabytes. With it, Groningen is 95,967 surfaces in 44 MB, and the
densest cell is 3.8 MB — about what a LoD2.2 building cell costs, which makes this
the second-heaviest thing the client streams.

Surfaces at a `relatieve_hoogteligging` other than zero are dropped: that is
paving on a bridge or in a tunnel, and this layer lies flat on the ground, so
drawing a viaduct's footway at street level would run a pavement through whatever
is underneath it.

## What the paving is made of is a second axis, and it was already being fetched

Added 2026-08-29. `PavingKind` answers *what the ground is for* — pavement,
parking bay, verge — and the survey answers *what it is made of* in a different
field, which this extractor had been reading past. Colour is a property of the
substance rather than of the purpose: a brick pavement and a brick parking bay
are the same colour, so where the register names a material it decides the tone
and the form's own tone stands down.

Two fields carry it and only one of them is usable as a substance:

- **`plus_fysiek_voorkomen`** names one. Across `wegdeel` as a whole it is filled
  on about a third of features — but the carriageways are most of that whole and
  are exactly what this pipeline leaves to OpenStreetMap. On the surfaces it
  actually keeps, the rate is far better: **2,813 of 3,393, i.e. 83%**, measured
  on a square kilometre of central Groningen, 2026-08-29.
- **`fysiek_voorkomen`** is always filled and mostly names a *construction*, not
  a substance. `open verharding` means "laid in separate elements" and
  `gesloten verharding` means "continuous" — neither says what the elements are,
  and over three cities `gesloten verharding` resolved to asphalt 94 times and to
  concrete 59, so calling it either is wrong a third of the time. Only
  `groenvoorziening` and `onverhard` are read from it, because those two do name
  a substance.

What that square kilometre turned out to be, by surface count: 885 concrete
pavers, 699 fired brick, 558 tiles, 248 asphalt, 200 decorative stone, 128
concrete, 55 grass, 34 gravel, 580 with nothing said. So the pavements alone
split three ways — brick, concrete block, slab — where the map had one grey.

The remaining sixth still arrives with no material, and **that has to cost
nothing**: `None` reproduces the form's tone exactly, which is what keeps the
layer safe outside the country that publishes the register. It is the same
contract `building_facts` built on its alpha channel, and there is a test on it.

The material is a **trailing** wire element rather than a field
(`[kind, rings, material?]`), so the two-element form still reads — which matters
because a paving cell cached before this existed would otherwise fail to parse,
and that is every stored cell in the country re-downloading the second heaviest
thing the client streams.

**One form keeps its own colour: the cycle path.** The register describes
construction and never colour, and a Dutch cycle path is dyed red whether it was
laid in asphalt or in brick — `asfalt` being the commonest thing the survey says
about one. Letting the material speak there would turn the most recognisable
surface in the country grey along every street. The price is the occasional
genuinely grey cycle path drawn red, which is much the cheaper error.

## What is not read, and why

- **The terrain parts** (`begroeidterreindeel`, `onbegroeidterreindeel`) and
  `waterdeel` are the ground and water *fills*. Those are a different proposition
  from the paving: the vector tile's own water is what feeds the shore-distance
  field, the grass mask and the globe's ocean mask, and its `land` polygons are
  what the tree scatter plants inside. Replacing the fills means keeping the tile
  parse for all of that anyway, and then reconciling two descriptions of the same
  ground.
- **The bank protections** in `scheiding_lijn` — `walbescherming`, `damwand`,
  `kademuur` — are dropped on purpose. They are retaining structures at the
  water's edge, and drawing them as walls would stand a metre of masonry along
  every canal in the city, on the side the map already draws as a bank.
- `slagboom`, `fietsenrek`, `hectometerpaal` and the traffic-signal posts are
  real and surveyed, and there is no form here that is honestly them. The rule the
  furniture table and the tree scatter already follow — an unstyled thing beats a
  wrongly styled one — applies to a data source as much as to a tag.

## The other forty-two collections, gone through once

The BGT publishes **49 collections** and the layer above reads **seven**. All 49
were measured on **2026-08-29** — one page (`limit=1000`) each over 1 km² of
central Groningen (`bbox=6.5590,53.2149,6.5740,53.2239`), and a second page over
roughly 10 × 9 km of the city (`bbox=6.4800,53.1800,6.6300,53.2600`) for the ones
the small box returned empty. Current versions only, i.e. after the
`termination_date` filter.

**Every "over the city" number below is one page, so it is a floor and not a
count.** The API returns no total, so a collection whose page came back full at
1000 features has more behind it than the figure says — `bord`, `put`, `paal`,
`spoor` and the terrain parts are all in that state. The figures are the right
order of magnitude for deciding whether a collection is worth reading, and the
wrong thing to size a cache against.

Two facts confirmed while doing it, and they are what a "should we?" question
actually turns on: the interface is unchanged (`https://api.pdok.nl/lv/bgt/ogc/v1`,
keyless, cursor-paged, no `numberMatched`, every historical version returned), and the
licence is **CC0 1.0**, stated by the API itself in the `collections` document's
`license` link. Caching and redistributing the cells is unambiguously allowed;
the attribution line in `surveyed.rs` is courtesy, not obligation.

### `type` is not the classifier for optional content — `plus_type` is

This is the trap, and it is the one that would silently delete a whole layer.
The core BGT collections carry their class in `type`. The **optional** ones
(IMGeo "plus" content) carry `type: "niet-bgt"` on *every* feature and put the
real class in `plus_type`. Measured:

| Collection | `type` | `plus_type` |
|---|---|---|
| `scheiding_vlak` | `muur` 140, `kademuur` 71, `transitie` 12 | empty on all 223 |
| `bord` | `niet-bgt` on all 843 | `verkeersbord` 629, `wegwijzer` 80, `informatiebord` 52, `verklikker transportleiding` 34, `scheepvaartbord` 20, `straatnaambord` 17, `plaatsnaambord` 10 |
| `kast` | `niet-bgt` on all 162 | `elektrakast` 75, `openbare verlichtingkast` 29, `CAI-kast` 24 |
| `bak` | `niet-bgt` on all 34 | `afvalbak` 15, `container` 11, `afval apart plaats` 7 |
| `vegetatieobject_lijn` | `niet-bgt` on all 33 | `haag` 32 |

So **"drop everything that is `niet-bgt`" is not a filter, it is a way to read
nothing.** Four of the five collections worth reading are `niet-bgt` end to end.
The rule is per-collection: read `type` where the register fills it, `plus_type`
where it does not, and drop a feature only when *both* are empty.

This also corrects a claim this note used to make: `scheiding_vlak`'s `type` was
said to be "empty on every feature seen so far". It is filled on all 223 current
features over that 1 km². It is `plus_type` that is empty there.

### Central Groningen has no surveyed point objects at all

Over that same 1 km² box, **every point collection returns zero current
features**: `paal`, `straatmeubilair`, `bak`, `bord`, `put`, `kast`, `mast`,
`sensor_punt` and `vegetatieobject_punt`. The wider city has all of them. It is
not a bbox mistake: the *same* box returns 331 `begroeidterreindeel`, 816
`onbegroeidterreindeel`, 223 `scheiding_vlak` and 97 `vegetatieobject_vlak`, so
the query works and the area is well surveyed — it is the point registers
specifically that are absent. This is the same municipality-level coverage hole
recorded above for trees, except that it is wider than trees: it is every point
collection there is. The densest part of the city is the part with no surveyed
street objects in it, so a capture of the centre is the wrong place to check
whether any of this arrived.

### Worth reading: five

Each has to clear the same four bars — nothing the client already reads supplies
it, `furniture.rs` already has an honest form for it, it is visible at the scale
the layer is drawn at, and it goes *beside* the generated layers rather than
replacing them.

| Collection | Geometry | Current, measured | Form |
|---|---|---|---|
| `scheiding_vlak` | Polygon | 223 over the 1 km² | `LineKind::Wall` |
| `vegetatieobject_lijn` | LineString | 33 over the city | `LineKind::Hedge` |
| `bord` | Point | ≥843 over the city | `FurnitureKind::Board` |
| `kast` | Point | 162 over the city | `FurnitureKind::Cabinet` |
| `bak` | Point | 34 over the city | `FurnitureKind::Bin` / `Recycling` |

Two shape notes. `scheiding_vlak` is boundaries surveyed as **areas**, so it takes
the same centreline reduction the area hedges take — laying boxes round the
outline would draw both faces of every wall. Its `kademuur` and `transitie` are
dropped: `kademuur` for exactly the reason `scheiding_lijn` already drops it (a
metre of masonry along every canal, on the side the map draws as a bank), and
`transitie` because it is a joining segment between two boundaries rather than a
boundary. `vegetatieobject_lijn`, by contrast, is a hedge already surveyed as a
**line**, so it skips that reduction and reaches `LineKind::Hedge` directly.

**Open, and deliberately not decided here: `verkeersbord`.** 629 of the 843
`bord` records are traffic signs. `FurnitureKind::Board` is an information board
on two posts, and a traffic sign is a plate on one pole — drawing 629 of the
first where the second stands is exactly the "wrongly styled thing" this note
keeps arguing against, and at a density where it would be the first thing anyone
noticed. The honest subset is `wegwijzer`, `informatiebord`, `straatnaambord`,
`plaatsnaambord` and `reclamebord`, which is about 160 over the city — still
worth reading. So either `bord` is taken as that subset, or `verkeersbord` waits
for a sign form of its own. Either way it is not a reason to hold up the other
four collections.

**The client half of this has landed; the extractor half has not.** `PROP_KINDS`
in `surveyed.rs` now names positions **7 `Board`, 8 `Bin`, 9 `Recycling`** — the
forms `bord` and `bak` need, `kast` reusing position 6 `Cabinet`, and
`scheiding_vlak` and `vegetatieobject_lijn` reusing `LINE_KINDS` 1 `Wall` and 2
`Hedge`, so `LINE_KINDS` did not move. Nothing emits 7–9 yet. That order is
deliberate and is the only safe one across this seam: a client naming a form no
host sends costs nothing, while a host sending a position no client has is
content that vanishes with no error anywhere. cartopy's `bgt.ts` must match those
positions exactly when the walk is added there.

### Not worth reading: the rest

- **Something the client already reads supplies it.** `pand` and
  `pand_nummeraanduiding` (BAG / 3DBAG), `spoor` (OSM's textured ribbons),
  `waterdeel`, `ondersteunendwaterdeel`, `begroeidterreindeel`,
  `onbegroeidterreindeel` (the tile's own fills, for the reason above), `mast` (1
  current over the city) and `kunstwerkdeel_punt` (99 `hoogspanningsmast`) —
  `systems::tall_structures` already streams masts and pylons from Overpass at z11
  over ±30 km, a wider ring than a z14 cell can give them.
- **Administrative, changes no picture.** `functioneelgebied` (69, mostly
  `infrastructuur waterstaatswerken`), and `buurt`, `wijk`, `stadsdeel`,
  `waterschap`, `openbareruimte` — which return no features at all over either
  box, so there is not even a boundary to consider drawing.
- **Too near empty to be worth a walk.** `weginrichtingselement_punt` (8 over the
  city, and `plus_type` is empty on all 8, so there is nothing to say what they
  are); `ongeclassificeerdobject` (2); `overigescheiding` (5, four of them with
  no `plus_type`). The last two are *not* retired, as an earlier reading of this
  had it — their survivors carry an ordinary `status: bestaand`. They are simply
  almost never used.
- **No honest form at this scale.** `put` (973 in one page over the city, 787 of
  them `kolk` — a gully grate is a texture, not geometry), `weginrichtingselement_vlak`
  (101, 100 of them `rooster`, gratings, for the same reason — and note it reads
  as *empty* over the 1 km² box, which is the coverage hole and not the truth),
  `weginrichtingselement_lijn` (15 / km², all `molgoot` — a gutter channel finer
  than the paving layer's own 10 cm simplification), `sensor_punt` (71, mostly
  cameras), `sensor_lijn` (88, all `detectielus` — induction loops buried in the
  road surface), `installatie` (23, all `zonnepaneel`, rooftop),
  `kunstwerkdeel_lijn` (511, 434 `duiker` — culverts, underground),
  `kunstwerkdeel_vlak` (5 / km², a mix of `keermuur` and `steiger`),
  `overigbouwwerk` (34, split between `transitie` and `lage trafo`),
  `gebouwinstallatie` (16 `toegangstrap` / `bordes`),
  `waterinrichtingselement_lijn` (24, all `remmingswerk`).
- **A layer, not a collection.** `overbruggingsdeel` (20 / km²) and `tunneldeel`
  (45 / city). This is the one rejection with real picture value: the paving
  extractor drops every surface at `relatieve_hoogteligging != 0`, so a bridge
  gets no surveyed paving today and a deck drawn flat would lie in the water it
  crosses. Drawing it needs a deck *height* the BGT does not carry and a rung on
  the depth ladder the flat paving layer does not have.
- **Would replace rather than add.** `openbareruimtelabel` (217 / km², points
  carrying a rotation — surveyed street-name placements). `systems::street_labels`
  places labels view-independently from world footprints on purpose, and that is
  the argument to reopen before this is an option.
- **Belongs to another module.** `begroeidterreindeel_kruinlijn`,
  `onbegroeidterreindeel_kruinlijn`, `ondersteunendwegdeel_kruinlijn`,
  `wegdeel_kruinlijn` — crest lines, whose consumer is `systems::dikes`.

**One near miss, recorded so the next pass does not have to find it again:**
`waterinrichtingselement_punt` is 906 features in one page over the city, 898 of
them `meerpaal` — mooring posts along the canals. That is more numerous than the
five accepted collections put together, and it is rejected only because there is
no form for a short post: `FurnitureKind::Lamp` is a column with a head on it and
nothing else here is close. It is the obvious partner for `crate::moorings`,
which currently infers where a boat could tie up from the *shape* of the water
polygon and would rather be told.

The carriageways are not in either list because they are settled elsewhere:
`wegdeel`'s `rijbaan *`, `spoorbaan` and `OV-baan` stay with OpenStreetMap for
the reason `crates/geo/src/paving.rs` gives.

### What has not been measured

**The per-cell byte cost of the five.** Paving is already the second heaviest
thing the client streams at 3.8 MB for the densest cell; these five go into the
*objects* file, 60 kB for that same cell. The measurement is a full extractor run
over the 1 km² box and `du -b` on the produced `bgt/14/*/*.json`, and it wants
doing before the pipeline is run at scale.

**Whether a client that has already visited a city would ever see the new
content.** It would not, as things stand: `load_surveyed_cell` returns the cached
copy whenever one exists, and the coverage manifest carries no data version —
`zoom`, `cells`, `attribution`, and `CoverageSlot::revision` can only change once
per process. So re-running the extractor reaches only clients that have not been
there yet. The three ways out are to accept it and let cache eviction handle it,
to put a version in `coverage.json` and mix it into the cache key (a change to the
shared `CoverageSlot`, so to every layer that uses one), or to serve new content
under a new path so the old keys simply miss. That is a decision, not a detail.
