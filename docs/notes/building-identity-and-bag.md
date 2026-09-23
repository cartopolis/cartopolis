---
topic: buildings
triggers: [building_facts.rs, bag_attributes.py, 3dbag_lod22.py, bag.ts, lod22.ts, BAG, identificatie, bouwjaar, gebruiksdoel, PDOK, Kadaster, BIM, IFC]
updated: 2026-08-27
---

# The building id, and what the Dutch registers hang off it

## The id was not an id

The LoD2.2 extractor has always written an `id` on each mesh element, and until
2026-08-27 it was made like this:

```python
digits = "".join(ch for ch in oid if ch.isdigit())
return int(digits[-15:]) if digits else abs(hash(oid)) % (10 ** 15)
```

The comment called it "a stable-ish int", which is exactly what it was — a dedupe
key inside one build, and nothing else. 3DBAG names an object
`NL.IMBAG.Pand.0014100010903891-0`, where the sixteen digits are the BAG
**identificatie** and `-0` numbers the parts one pand was split into. The part
suffix contributes a digit of its own, so keeping "the last fifteen digits"
shifted the number by a decade for every object that had one, and dropped the
leading zeros that say which municipality it is. The digitless fallback was worse:
Python salts `hash` per process, so two runs of the same build produced different
ids.

The client then threw it away regardless — `El` in `lod22.rs` deserialised `p` and
`t` only.

None of that mattered while the id was internal. It mattered the moment anyone
wanted to ask a question about a *particular* building, because the identificatie
is the key the whole Dutch open-data estate is joined on: the BAG's own rows,
addresses, the monuments register, energy labels — and any BIM model of the
building, whoever holds it. Matching a mass to a model by geometry is the
alternative, and it is not one.

So the id is now the identificatie verbatim, as a **string** (leading zeros are
load-bearing), with `part` beside it rather than inside it.

## Where the facts come from, and why not the obvious place

`api.bag.kadaster.nl` is the Kadaster's own BAG API and needs a key. PDOK's
`service.pdok.nl/lv/bag/wfs/v2_0` serves the same register, open, keyless, and
that is what the BAG extractor pages.

The layer split is worth knowing because it is not what you would guess:
**`bag:pand` already carries the attributes per building** — `bouwjaar`, `status`,
`gebruiksdoel` (comma-joined for a mixed-use block), the floor-area range of the
units inside and how many there are — so the common case needs no join at all.
`bag:verblijfsobject` is the per-unit layer, and its value is the *point*: the
register's own position for each addressable unit, at or near its entrance, which
is the closest thing to a national door-position dataset that exists.
`bag:ligplaats` and `bag:standplaats` are addressed places with no building on
them, which is precisely why a map built from buildings has never shown them.

### The 1000-feature truncation

Measured 2026-08-27. This service caps a GeoJSON response at **1000 features**
however large a `count` you ask for, and usually omits `numberMatched` from
GeoJSON entirely. That combination defeats the ordinary page-until-short-page
loop: asking for 10 000 returns a *full* page that looks like a short one, the
walk ends after the first thousand rows, and nothing anywhere reports a problem —
the output is a city that quietly knows about a tenth of its buildings.

Note the sibling LoD1.2 extractor pages the **3DBAG** WFS with
`PAGE = 10_000`, and that is correct for *that* server. The two are different
services with different caps; the number is not shareable.

It therefore pages at 1000 and calls `resultType=hits` (a small
XML read) once per layer, purely so the walk can be checked against a total it did
not compute itself. It warns when the two disagree.

### `startIndex` stops at 50 000

Also measured 2026-08-27, and this one is not survivable by paging at all:
`startIndex=50000` answers, `startIndex=50001` is a flat HTTP 400. Groningen's
default bbox holds 94 052 panden and 129 270 addressed units, so *no* page size
reaches the end of it.

It therefore quarters the bbox recursively until every piece is
under 40 000 features and pages each piece. A WFS bbox is an intersects test, so
a building straddling a cut comes back from both halves — every collector dedupes
on `identificatie`, which it can do precisely because the id is now a real one.

### Two files per cell, and which one is heavy

Measured on Groningen's densest z14 cell: **8 000 buildings are 0.53 MB, and the
17 785 addressed units in the same cell are 2.04 MB.** `lod22` fetches a cell's
facts inside its own mesh job and reads only the years and the uses; with one
file it was pulling five times what it reads, on the lane that already owns the
largest assets in the client. So `y.json` is buildings and `y.places.json` is
addresses plus berths plus their street table, and the mesh lane fetches only the
first. Either file may be absent — an industrial estate has no addresses, a canal
bank has berths and no buildings — so a 404 on one is not a failed cell.

### Why a pand is written into several cells

The mesh pipeline bins a building by the first vertex of its 3D solid. The
attribute pipeline has only a 2D footprint and cannot reproduce that rule, and a
building whose attributes landed one cell over from its mesh would never join —
silently, and only for buildings near a cell edge, which is the worst possible
distribution for noticing. So atlas's `pipeline/bag.rs` writes each pand into every z14
cell its footprint bbox touches. The duplication is a few bytes on a handful of
buildings; the alternative is a bug that reproduces on a tenth of a percent of
the city.

## Two consumers, two shapes

A colour baked into a vertex has to be known on the worker that builds the mesh.
So `lod22`'s job fetches its own cell's facts inside itself (`load_bag_cell`),
which is a second request that lands on the same storage cache key as the
streamer's — at worst a cache read. In exchange, a mesh and the facts it is
coloured by cannot arrive in the wrong order.

The alternative was to share one parse through the `BuildingFacts` resource and
rebuild meshes when a facts cell landed. That is the terrain-revision retear, and
it is affordable for a 6.4 km field that arrives once per anchor — not for a
stream that lands 25 cells and re-lands them as you walk.

`BuildingFacts` still exists, streamed on its own ring, because everything that is
*not* a vertex colour — the readout, a lookup, a panel — wants a resource it can
ask at any time.

## What the facts are used for

- **Facade palette by era** (`osm_buildings::building_palette_dated`). Dutch
  building stock is legible by age from across the street, and the position hash
  cannot know any of it. `year: None` reproduces the old generated palette
  exactly, which is what every building outside the coverage still gets.
- **Night window lighting by use** (`osm_buildings::night_lit_scale`, read by
  `building_windows.wgsl`). The shader already varied liveliness by a per-block
  hash so whole buildings stand dark; this makes it true rather than random — the
  office park goes quiet after six, the flats above the shops do not, the hospital
  stays on. It rides in the **alpha of the existing vertex colour**, which was a
  constant 1.0 and costs nothing to use: the material is opaque, so `alpha_discard`
  flattens it and no pixel is affected. Same spare-channel trick as
  `globe_ocean`'s sea mask, and read at the same point — before `alpha_discard`.
  **1.0 means "nothing known" and reproduces the old look**, which is what makes
  the change safe outside the Netherlands.
- **Buildings the register has struck off are not drawn.** Only `Demolished` and
  `NotRealised`, which are unambiguous. 3DBAG's solids are a dated snapshot and
  the BAG is refreshed far more often, so this is the one case where the two
  disagree and the register is certainly right. A permit is not a demolition and
  an empty building is still a building; nothing weaker qualifies.
- **The "where am I" readout** (`interact::bag_address` → the status HUD).
  `InteractState::address` had been written every frame since the interact layer
  was built and drawn nowhere; it is drawn now.

## What is still unused, and where it would go

The pipeline ships more than is consumed, deliberately — the data is free once the
request is made, and a second pass over the WFS to add a column is not.

Unconsumed today: per-unit floor areas beyond the readout, `units` beyond its
small lift to the window lighting, postcodes beyond the readout, and every
`BuildingStatus` other than the two that suppress a building.

Not built at all, in rough order of value:

- **Picking a building.** The id makes it possible; the obstacle is that a cell is
  merged into one mesh (deliberately — it is the heaviest asset the client frees).
  The answer is an `id → triangle range` table beside the mesh, resolved by binary
  search from a raycast's triangle index, **not** splitting buildings into
  entities. `MeshData::into_meshes` chunks in triangle order, so the ranges stay
  contiguous across the byte-bounded pieces.
- **Other registers, all reachable and all verified live on 2026-08-27:** the BGT
  (`api.pdok.nl/lv/bgt/ogc/v1`, 49 collections — road surfaces, pavements, kerbs,
  individual trees, street furniture, fences) is a straight upgrade over the
  OSM-derived street surfaces and is a project of its own; the Kadastrale kaart
  (`service.pdok.nl/kadaster/kadastralekaart/wfs/v5_0`) has parcel boundaries, so
  gardens and plot edges (ownership is **not** open — geometry only); the national
  monuments register is at `services.rce.geovoorziening.nl/rce/wfs`
  (`rce:NationalListedMonumentPolygons`, `rce:Townscapes`), not on PDOK, which is
  where you will look for it first.
- **IFC.** An IFC model is per building and the identificatie is how it says which
  one. The import belongs server-side, in the shape ADR-027 chose: convert to the
  same `{id, p, t}` cell format with an element/storey tag beside the roof flag,
  so the client stays a triangle consumer. That gives a per-building LoD ladder —
  1.2 → 2.2 → an imported interior — swapped in for the one building that has one.

## Coverage is Netherlands-only, and fails quiet

`bag_covers()` answers `false` until the manifest lands and for ever on a host
with no `bag/` tree, so every consumer's fallback is the behaviour that was there
before. One difference from the 3DBAG manifests next door: those retry an
unreachable host indefinitely, because they are the difference between real
buildings and guessed ones. This tree is additive, and a host that has never run
the BAG extractor answers 404 permanently — so it gives up after three attempts
rather than asking the same question every 30 seconds for the life of the process.

`--dump-state` carries `bag_buildings` / `bag_addresses` / `bag_berths`, because
the failure mode here is *silence*: a missing coverage manifest produces a
perfectly good-looking city with the generated palette, and no capture can tell
that apart from the facts having landed and changed little.
