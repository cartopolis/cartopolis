---
topic: map
triggers: [surveyed.rs, bgt_objects.py, bgt.ts, scatter.rs, furniture.rs, BGT, trees, lampposts, street lighting, PDOK]
updated: 2026-08-28
---

# Streets that were actually surveyed

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
- **Every historical version is returned.** This is the one that silently ruins an
  extract. A page of Groningen tree points is 1000 features of which 485 are
  current; the rest were retired years ago and carry an entirely ordinary
  `status: bestaand`. The discriminator is `termination_date`, and without it the
  map grows a second, ghost set of trees standing where trees used to be. The
  `datetime` parameter looks like the right filter and returns nothing at all.

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
That ordering is what makes the mixture safe rather than a z-fight: a surveyed
pavement and a nominal-width OSM carriageway do not agree to the centimetre about
where the kerb is, so they overlap here and there — and with the road above, the
overlap reads as tarmac meeting the kerb, which is what it looks like from the
pavement.

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
- **`scheiding_vlak`** is the boundaries that were surveyed as areas rather than
  lines. Its `type` is empty on every feature seen so far, so there is nothing to
  say whether one is a wall, a bank or a planter.
- **The bank protections** in `scheiding_lijn` — `walbescherming`, `damwand`,
  `kademuur` — are dropped on purpose. They are retaining structures at the
  water's edge, and drawing them as walls would stand a metre of masonry along
  every canal in the city, on the side the map already draws as a bank.
- `slagboom`, `fietsenrek`, `hectometerpaal` and the traffic-signal posts are
  real and surveyed, and there is no form here that is honestly them. The rule the
  furniture table and the tree scatter already follow — an unstyled thing beats a
  wrongly styled one — applies to a data source as much as to a tag.
