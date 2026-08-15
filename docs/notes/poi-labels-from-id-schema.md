---
topic: poi-labels-from-id-schema
triggers: [places.rs, presets.rs, preset_table.rs, id_presets.py, PoiCategory, interact.rs, "poi label", "tag name", "id-tagging-schema", "preset", "what is this place", "icon for a shop"]
updated: 2026-08-15
---

# POI labels come from iD's tagging schema, not from us

`places::PoiCategory` sorts a POI into twelve buckets and that is all it can say:
a bakery, a butcher and a phone shop are each `Shop`, so the proximity prompt
read `Look at Bakkerij Klaas (shop)` for all three. The specific tag was read to
pick the bucket and then dropped.

The vocabulary for naming those things is not ours to write. It is
[`@openstreetmap/id-tagging-schema`](https://github.com/openstreetmap/id-tagging-schema),
the catalog behind the iD editor on openstreetmap.org — community-maintained,
ISC licensed, ~1,700 presets, each a tag pattern plus a human name, synonyms and
an icon, translated into 30-odd languages. `tools/id_presets.py` flattens it into
`crates/geo/src/preset_table.rs`; `geo::presets::label_for` is the lookup.

Measured on the Groningen z14 fixture, 2026-08-15
(`places::tests::presets_name_more_kinds_of_place_than_the_categories_can`):
**163 of 174 named POIs (94%) get a specific label, 43 distinct kinds where the
categories offered 10.**

## What was left upstream, and why

- **The icons.** iD's are maki/temaki/fontawesome sprite ids and this client has
  no atlas to draw them from. Its glyphs must be characters the bundled font
  stack actually contains — the whole subject of `PoiCategory::glyph`'s doc
  comment and of the tofu bug before it. Importing icon *names* we cannot render
  would be a table of empty boxes. So 660 labels are paired with 12 glyphs on
  purpose: the prompt names the place in words beside the mark, so the mark only
  has to be distinguishable.
- **The edit fields** (`fields`, `moreFields`, `fields.json` — 596 form
  definitions). They describe forms for *editing* OSM, which this client does
  not do, and they are most of the schema's bulk.
- **The other 29 locales.** There is no language setting to reach them. One line
  (`LOCALES`) in the generator when there is.
- **The search synonyms** ("bistro", "coffee", "espresso" for a cafe). The
  natural fuel for searching the POIs already loaded around you, which this
  client cannot do — its search is Nominatim-only and never looks at
  `WorldPlaces`. The generator still builds them and `render()` drops them;
  switch both on together when there is something to search.
- **Multi-tag presets** (407 of them touching our keys). A tile POI carries raw
  tags and we match one pair; `amenity=fast_food` + `cuisine=burger` needs a
  matcher this table does not have, and the single-tag parent already covers the
  common case.

## Constraints that will bite

- **The label and the category must come from the same tag.** Derived
  independently, a feature tagged `shop=bakery` *and* `leisure=garden` reads
  `Bakery (leisure)`. `places::matched_row` is the single place that decides
  which of a feature's tags the thing *is*; both outputs hang off it.
- **The version is pinned** (`SCHEMA_VERSION`). An unpinned bump would rewrite
  1,700 labels under a diff nobody reviewed.
- **The table is `#[rustfmt::skip]`.** Left alone, rustfmt explodes two dozen
  over-width rows into five lines each, the file stops matching what the script
  emits, and `--check` reports drift forever.
- **`--check` is not wired into CI.** It needs network to fetch the tarball, and
  nothing else in the workflows does. Run it by hand after touching the script.
- **Casing is inconsistent on purpose.** `PoiCategory::label` is lower case
  because it was written to sit inside a sentence; iD's labels are titles and
  contain acronyms ("DIY Store", "ATM"), which lower-casing would mangle for a
  consistency no reader is checking for.

## Where it came from

An OsmGo comparison (2026-08-15). OsmGo is a phone-first OSM *field editor* and
ships this same catalog flattened into two 8.4 MB files; almost nothing else
about it transfers to a 3D client, but the observation that the vocabulary is a
build artifact you import rather than a table you maintain does. Its other
transferable ideas — the device-compass heading (ours is course-over-ground and
therefore undefined standing still), an explicit "download this area" with the
downloaded envelope drawn on the map, and declutter-with-tappability as a
setting — are not implemented here.
