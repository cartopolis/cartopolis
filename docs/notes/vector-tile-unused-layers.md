---
topic: vector-tile-unused-layers
triggers: [vector_tiles.rs, street_furniture.rs, PAINT_ORDER, pois, addresses, "shortbread", "what else is in the tile", "osm data", "more realism"]
updated: 2026-07-31
---

# What else is already in the vector tile

Surveyed 2026-07-31 on the Groningen z14 fixture
(`crates/viberfox/tests/fixtures/groningen_z14.mvt`, 495 KB), by dumping every
layer's feature count, property keys and `kind` histogram. `map_geometry` fetches
this body once per cell and decodes all of it, so anything listed here costs **no
request** — the only question is whether we build something out of it.

| Layer | Features | Used for |
|---|---|---|
| `buildings` | 8552 | flat fill (heights come from 3DBAG/Overpass, not here — see below) |
| `addresses` | 6211 | **nothing** (`housenumber`, `unit`) |
| `pois` | 467 | street furniture (`street_furniture.rs`) — partially |
| `street_labels` | 244 | street names, routing turn names |
| `streets` | 95 | roads, routing graph |
| `sites` | 84 | flat fill only — `parking`×56, `bicycle_parking`×15, `school`×7, `hospital`, `university` |
| `public_transport` | 25 | **nothing** — transit stops come from Overpass instead |
| `water_polygons` | 23 | water surfaces |
| `place_labels` | 21 | **nothing** (`kind`, `name`, `population`) |
| `land` | 15 | landuse fills, tree scatter |
| `street_polygons` | 11 | flat fill |
| `bridges` | 5 | bridge decks |
| `water_lines` | 2 | flat fill — carries `kind` = `canal` / `ditch`, which the shore field does not read |
| `street_labels_points` | 3 | **nothing** — all `motorway_junction`, i.e. exit numbers |

## `pois` has no `kind`

Every other Shortbread layer discriminates on one `kind` property. `pois` carries
the raw OSM tags through instead — `amenity`, `shop`, `leisure`, `tourism`,
`man_made`, `historic`, `information`, `sport`, `tower:type`, `religion`,
`recycling:*` — 69 distinct `key=value` pairs in this one tile. Any consumer has
to match on key *and* value, and a feature routinely carries several.

The outdoor, free-standing share is roughly 280 of the 467: `bench`×129,
`waste_basket`×40, `playground`×40, `shelter`×19, `recycling`×12, `artwork`×10,
`vending_machine`×9, `picnic_site`×6, `post_box`×6, `information`×10,
`fountain`×2. The rest is shops, restaurants and doctors — points *inside*
buildings we already draw, which is why `street_furniture.rs` skips everything its
table does not name.

## The `buildings` layer carries a `height`, and it is 95% a placeholder

Worth knowing before anyone reaches for it as a global skyline: every one of the
8552 features has a numeric `height`, but the distribution is

```
{0: 2, 4: 52, 5: 8115, 8: 248, 11: 78, 15: 36, 19: 7, 20: 1, 22: 1, 23: 1,
 25: 1, 26: 1, 28: 1, 29: 1, 30: 1, 31: 2, 33: 1, 41: 1, 45: 1, 70: 1}
```

— 5 m on 94.9% of buildings, and only 437 (5.1%) differ from it. Extruding it
verbatim gives a uniform 5 m block field, not a city. It is a usable *fallback*
outside 3DBAG and Overpass coverage; it is not a substitute for either, and it
cannot be told apart from real data by looking at one building.

## Not in the tile at all

Anything per-object and vertical needs Overpass: `natural=tree` (real tree
positions and species — `vegetation.rs` scatters because the schema has canopy
*areas* only), `natural=tree_row`, `highway=street_lamp`, `barrier=fence|wall|
hedge`, `power=tower`, `man_made=mast|chimney|water_tower`, `highway=crossing`,
and the `lanes` / `width` tags that would give a street its real width instead of
one derived from its class.
