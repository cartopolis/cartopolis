---
topic: map
triggers: [surveyed.rs, bgt_objects.py, scatter.rs, furniture.rs, BGT, trees, lampposts, street lighting, PDOK]
updated: 2026-08-28
---

# Trees and lampposts that were actually surveyed

Two of this client's layers have always said, in their own module notes, that
they are inventing something. `scatter.rs`: Shortbread's `land` layer carries
canopy *areas*, "so the trees here are scattered, not surveyed". `furniture.rs`
reads the `pois` layer, which has no street lighting in it at all — so until now
the map had no lampposts anywhere.

The BGT — the Netherlands' large-scale topography, ~20 cm — registers both,
object by object. `tools/bgt_objects.py` reads it and `systems::surveyed` streams
it.

## The API behaves nothing like the BAG WFS next door

Same organisation, same portal, opposite design, and the differences are not
cosmetic:

- **Cursor paging, not `startIndex`.** So there is no 50 000-row wall and no bbox
  to split, which is the whole of what `bag_attributes.py` had to be built around.
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

**Solid and interactable stopped being the same list.** Every furniture instance
used to become both a collider and a prompt, which was fine while the only source
was `pois` — a layer of things people visit. A city's worth of lampposts breaks
that: a column is firmly in the way and there is nothing whatever to do with one.
`world_places::prop_interactable` is the split.

## What is not read, and why

- **`wegdeel` and the terrain parts** are the road and ground *surfaces*. Taking
  those means replacing the map's own tessellated geometry rather than adding to
  it — a far larger change, and one where the OSM path still has to exist beside
  it for everywhere that is not the Netherlands. It is the single biggest
  remaining upgrade available from this dataset: real pavements, kerbs and
  carriageway edges at 20 cm.
- **`scheiding_lijn`/`scheiding_vlak`** is surveyed fencing and walling, and there
  is no form to draw a fence with yet — it is a line, and everything here is a
  point prop.
- **`vegetatieobject_vlak`** is mostly hedges (853 in one Groningen page). Same
  problem: an area, not a point.
- `slagboom`, `fietsenrek`, `hectometerpaal` and the traffic-signal posts are
  real and surveyed, and there is no form here that is honestly them. The rule the
  furniture table and the tree scatter already follow — an unstyled thing beats a
  wrongly styled one — applies to a data source as much as to a tag.
