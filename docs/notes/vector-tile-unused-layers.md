---
topic: vector-tile-unused-layers
triggers: [vector_tiles.rs, street_furniture.rs, data_layers.rs, pois.rs, places.rs, PAINT_ORDER, pois, addresses, "shortbread", "what else is in the tile", "osm data", "more realism", "overpass"]
updated: 2026-08-26
---

# What else is already in the vector tile

Surveyed 2026-07-31 on the Groningen z14 fixture
(`crates/geo/tests/fixtures/groningen_z14.mvt`, 495 KB), by dumping every
layer's feature count, property keys and `kind` histogram. `map_geometry` fetches
this body once per cell, so anything listed here costs **no request** — the only
question is whether we build something out of it.

*Corrected 2026-08-16:* this used to say the cell "decodes all of it". It does
not, and has not since `TileData::features` became a lazy per-layer `OnceCell` —
every "loop over all layers" site filters *before* touching `features()`, so an
unwanted layer is never decoded and the two layers below that nothing reads cost
nothing at all. The conclusions are unaffected; the cost model was.

| Layer | Features | Used for |
|---|---|---|
| `buildings` | 8552 | flat fill (heights come from 3DBAG/Overpass, not here — see below) |
| `addresses` | 6211 | the "where am I" readout (`systems::places`) — `housenumber`, `street`; `unit` still unused |
| `pois` | 467 | **three** readers of one layer: street furniture (`street_furniture.rs`), the map's shop markers (`data_layers.rs` via `pois.rs` — 55 features carry a `shop`), and every *named* feature as a proximity prompt (`places.rs`) |
| `street_labels` | 244 | street names, routing turn names |
| `streets` | 95 | roads, routing graph |
| `sites` | 84 | flat fill, **and the parked vehicles filled into it** (`parking.rs`) — `parking`×56, `bicycle_parking`×15, `school`×7, `hospital`, `university`; the last three are still fill only |
| `public_transport` | 25 | the transit markers (`data_layers.rs`) — Overpass now only enriches them, see below |
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
table does not name. The shop markers are the second reader of the same layer, so
both go through `geo::pois::extract_pois` rather than each matching tags its own
way.

## What the point layers do *not* carry: identity and contact detail

Measured 2026-08-10 on the same fixture tile (z14/8490/5319), against Overpass for
the same bbox. This is the reason the markers come from the tile but Overpass is
demoted rather than deleted (`data_layers.rs`).

The tile has no OSM id, no `website`, no `addr:street`, and no `ref:IFOPT` — the
key `transit_live` fetches departure boards by. Overpass answers **65 shops to the
tile's 55**, and 32 of those carry a website the tile has no field for.

`Feature::id` looks like an OSM id and is not one: **zero** of the tile's 55 shop
features intersect the node ids Overpass returns for the same bbox. So it is
neither a join key nor a link, and the two sources are matched on position and
category instead (`DETAIL_MATCH_M`, 10 m — an MVT coordinate at z14 with a 4096
extent quantises to ~0.36 m, so the gap being matched is quantisation, not
disagreement).

A **raster** tile source has no point layers at all, so there Overpass stays the
whole marker source. That path is live, not vestigial: a `--shot` run with no
manifest `tile_url` lands on it.

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

## Two of these are read now (2026-08-09)

`pois` and `addresses` were both listed above as used for nothing. `systems::places`
reads them out of the same `TileData` parse `map_geometry` already does, so they
still cost no request and no extra decode; `world_places::WorldPlaces` holds the
result per cell and drops it with the cell. See `docs/notes/avatar-game-feel.md`
for what they feed, plus two traps found in the data: a tile's buffer puts some
features slightly outside their own cell, and `housenumber` is frequently a
numeric MVT value rather than a string.

`public_transport`, `place_labels` and `street_labels_points` are still unread.

## And the `sites` polygons are filled now (2026-08-26)

`sites` was listed above as a flat fill and nothing else. `cartopolis_geo::parking`
lays parked cars into its `parking` polygons and bikes into its `bicycle_parking`
ones, off the same parse, into the same mesh group as the street furniture. It is
the first thing here derived from a polygon's *area* rather than from a surveyed
point, which changes what bounds it — see `docs/notes/parked-vehicles.md`.
