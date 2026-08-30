---
topic: navigation-marks
triggers: [nav_marks.rs, vaarwegmarkeringen, Rijkswaterstaat, buoy, buoys, beacon, "navigation marks", boei, kribbaken, topmark, obj_vorm, obj_kleur, naut_funct, body_mentions_water, MarkColour]
updated: 2026-08-30
---

# Navigation marks (Rijkswaterstaat vaarwegmarkeringen)

Every buoy and fixed beacon on the Dutch national waterways, each with the shape
and colour pattern that say which side of it to pass, the topmark that says what
it marks, and the colour of the light it shows. `cartopolis_geo::nav_marks` is
the pure half, `systems::nav_marks` the fetching half.

## The interface, and what it costs

`https://api.pdok.nl/rws/vaarwegmarkeringen-nederland/ogc/v1` — **keyless** OGC
API Features, `bbox` query, GeoJSON out (storage CRS EPSG:28992, **CRS84 is what
comes back by default**, longitude first). Note the path: the obvious
`…/rws/vaarwegmarkeringen/…` is a 404.

Two collections: `vaarweg_markeringen_drijvend_rd` (floating — buoys) and
`vaarweg_markeringen_vast_rd` (fixed — beacons, groyne marks, shore lights).

`Access-Control-Allow-Origin: *` and `Cache-Control: public, max-age=3600`, so
the web build reaches it directly like `systems::aerial` and unlike
`systems::height`. `limit` is capped at 1000. **`properties=` subsetting is a
400**, so the whole ~40-property (floating) or ~90-property (fixed) feature comes
down whatever you ask for — which is why the client caches a compacted form
rather than the response.

**Licence: CC0 1.0**, stated by the service's own `license` link. No obstacle to
caching or redistributing what we hold.

### Volume, measured 2026-08-30

National bbox `3.0,50.6,7.4,55.7`:

| collection | features | pages | raw GeoJSON | wall clock |
|---|---|---|---|---|
| floating | 10,105 | 11 | 9.6 MB | 3 s |
| fixed | 8,369 | 9 | 18.2 MB | 4 s |

Per z14 cell — the zoom `map_geometry` streams geometry at, and the one a mark is
meshed into:

| cell | marks | raw |
|---|---|---|
| Nijmegen, the Waal (8458, 5422) | 43 | 57.3 KB |
| Hollands Diep (8399, 5434) | 3 | 4.6 KB |
| Amsterdam, het IJ (8415, 5383) | 2 | 3.7 KB |
| Rotterdam, Nieuwe Maas (8396, 5418) | 1 | 2.8 KB |
| Groningen, Grote Markt (8490, 5320) | 0 | 1.9 KB (two empty answers) |

The Groningen row is the one the design turns on: an empty cell is not free, and
most of a streamed ring is empty. Hence the two gates below.

## The vocabularies, censused

998 floating and 260 fixed marks over five boxes (the Waal, the IJsselmeer, het
IJ, the Nieuwe Maas, Hollands Diep), 2026-08-30. The numbers matter because
`nav_marks.rs` parses these by rule rather than from a lookup table, and the
census is what says the rule covers the register.

* `obj_vorm` (floating shape) — 5 values: `spar` 702, `stomp` 163, `spits` 113,
  `Pilaar` 12, `bol` 8. **`Pilaar` is real and easy to miss**: the ticket that
  proposed this layer listed only four.
* `obj_kleur` — 15 floating values and 14 fixed, every one of the form `A`,
  `A/B`, `A/B/A` or `A/B repeterend` over {Rood, Groen, Geel, Wit, Zwart, Grijs}.
  Parsed by splitting on `/` and stripping a trailing ` repeterend`, so the
  sixteenth works too.
* `tt_toptek` (floating topmark) — 11 values. One of them,
  `2 Kegels punten van elkaar af`, is written **without** the comma the others
  carry; matching it as if it had one drops the safe-water topmark.
* `v_toptek` — the *fixed* collection's key for the same vocabulary. Reading only
  `tt_toptek` leaves every river beacon bare, which looks exactly like a register
  that records no topmarks.
* `licht_klr` (floating) / `licht_kl` (fixed) — the light's colour, on 166/625
  and 133/267 respectively. `Amber` appears only on fixed marks.
* `naut_funct` (fixed) — 14 values: `Kribbaken` 129, `Oeverlicht` 24,
  `Havenlicht` 23, `Bermverlichting` 22, `Bakenlijn` 16, `Lichtopstand` 15,
  `Lichtenlijn` 8, down to `Aanstraalverlichting` 1.
* Absent is written **three** ways: `X`, `Niet toegewezen` and `#`. All three
  must read as nothing; `X` taken for a colour paints a mark that means nothing.
* Geometry is **`MultiPoint` with one coordinate**, never `Point`. A reader that
  matches only `Point` finds nothing at all, which is indistinguishable from a
  region with no marks in it.
* `opgeheven` (decommissioned) is `#` on all 1,258 sampled — unlike the BGT, this
  API is not serving historical versions. The field is checked anyway.
* `obj_hoogte` is `0,00000` on **all 260** fixed marks. The register carries no
  object height, which is why a light tower is drawn as the same post as a groyne
  mark (see below).

## Two gates, and why they are the whole cost argument

`load_marks_cell` asks the register for a cell only when both hold:

1. The cell overlaps the collections' own `extent.spatial.bbox`
   (`2.354, 50.714, 7.555, 55.669`). Coverage is an envelope, never a status
   code — the rule `aerial.rs` states for the same service.
2. The cell's tile carries a water layer. A mark stands in a fairway.

The second one reads the **undecoded** MVT body
(`vector_tiles::body_mentions_water`) rather than a parse, and that is a
deliberate departure from how every other consumer of a tile works here.
`map_geometry` fetches a cell's tile on the I/O pool and hands the bytes to a
worker to parse (ADR-033); a fetch decision that waited for the parse would move
a ~120 ms decode onto the frame loop on wasm. The probe is a substring search for
the layer names, and its asymmetry is what makes it safe: a layer that exists
cannot fail to be mentioned, because MVT stores each layer's name verbatim in the
body. A false positive costs one request; a false negative is impossible.

The Furniture toggle is a third gate, applied by `map_geometry`: with the layer
off, nothing is fetched at all.

## Two rejected alternatives

**A cartopy extractor.** The four other Dutch registers are extracted by
[cartopy](https://code.garage44.eu/jeroen/cartopy) and served per cell by our own
host, and `where-the-map-data-is-built.md` is explicit that nothing but a person
keeps the two repositories in step. That seam is worth its cost for a
2 MB-per-cell dataset needing reprojection and triangulation; it is not worth it
for 18,474 points behind a keyless `bbox` query with CORS `*` and an hour of
cache control. The in-tree precedent for reaching PDOK from the client is
`systems::height` (AHN) and `systems::aerial`; for a per-cell third-party
streamer it is `systems::tall_structures`.

**A coverage manifest.** `systems::coverage`'s `CoverageSlot` answers "which
cells does *our host* serve", which is not a question here. Coverage is the
envelope above.

## What is deliberately not drawn

* **The lights lighting.** `sign_kar` gives the character (`Iso` 77, `LFl` 45,
  `Fl` 28, `Q` 7 …) and `sign_perio` the period, on the marks that carry a light.
  The Furniture group is one shared `StandardMaterial` lit by vertex colour with
  no per-object emissive anywhere in it, so a flashing light is a second material
  or an emitter and is its own ticket. This paints the lantern in the registered
  colour and stops.
* **Light towers as structures.** With `obj_hoogte` zero on every fixed mark
  there is no height to build one from, so `Lichttoren (vuurtoren)` and
  `Lichtopstand` are the same post as the rest. The tall-landmark lane is
  `systems::tall_structures`, off OSM heights.
* **The register's non-marks.** `Bermverlichting`, `Bordverlichting`,
  `Aanstraalverlichting` and `Walkast` are verge lighting, sign lighting,
  floodlighting and a shore cabinet. The BGT already surveys street lighting
  column by column (`systems::surveyed`), so drawing these too would stand two
  lampposts on one spot. 33 of 260 sampled fixed features are dropped this way.
* **A mark with no readable colour.** The colour *is* the meaning; a grey buoy
  asserts a mark that says nothing. Three of 260 fixed marks (`Niet toegewezen`)
  go undrawn.
* **Colliders.** A mark is not a `FurnitureInstance`, so nothing reaches
  `collision::sync_prop_colliders`. This is the same outcome a moored boat gets
  by the other route (`prop_radius` = 0): an invisible wall in a fairway is worse
  than a buoy you can walk through.

## The palette bends here, on purpose

`furniture.rs` states the rule the rest of that group follows — the map is pale
cartographic paper and a saturated bench is the loudest thing on screen. A
navigation mark is the one form in the group where the colour is the datum: a red
can and a green cone mean opposite things and differ in nothing else at 40 m. So
`MarkColour::rgb` is a set of muted *signal* colours rather than the furniture
greys, still knocked well back from the fluorescent paint of the real thing.

## Not verified on a screen

Written and tested in a container with no GPU beyond lavapipe, where colour and
lighting are not evidence
(`headless-shots-software-renderer.md`). The geometry half is judgeable there —
count, placement on the water rather than the bank, the fade band — and
`nav_marks` in `--dump-state` is the number that separates "the layer arrived"
from "this water has none". Whether the tones read correctly, and whether a 1.4 m
buoy is legible at the distance the Furniture fade starts, wants an eye on real
hardware.
