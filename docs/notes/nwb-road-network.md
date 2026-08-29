---
topic: nwb-road-network
triggers: [NWB, wegvakken, hectopunten, rel_hoogte, WEGGEG, rijstroken, wvk_id, road_width_m, "measured carriageway", "bridge geometry", PDOK, Rijkswaterstaat]
updated: 2026-08-29
---

# NWB: checked against the live service, and not taken

The frontier row promised "measured carriageways, and bridge and tunnel geometry".
**Both halves are wrong.** Checked 2026-08-29 against
`https://api.pdok.nl/rws/nationaal-wegenbestand-wegen/ogc/v1` (OGC API Features),
which has exactly two collections: `wegvakken` (road sections) and `hectopunten`
(hectometre posts).

The licence is **CC0 1.0**, read off the landing page's own `rel="license"` link,
not off a wiki — so the licence was never the obstacle. This was rejected on
content, not on terms.

## What it actually carries

`wegvakken` geometry is **multilinestring** — `/collections/wegvakken/schema?f=json`
gives `.properties.geometry` as `{"x-ogc-role": "primary-geometry", "format":
"geometry-multilinestring"}`. `hectopunten` is multipoint. **There is no polygon
anywhere in the service.**

The 60-odd properties are administrative: street name, road number, route number,
road authority, house-number ranges, municipality, `openlr`, `wvk_id` /
`jte_id_beg` / `jte_id_end`, `bst_code`, `fow`, `frc`, `rel_hoogte`. So:

- **No measured carriageways.** No width, no lane count, no carriageway polygon —
  a centreline and an authority code. The drawn ribbon's width comes from a
  per-class constant table, `crates/geo/src/road_texture.rs:164` (`road_width_m`:
  motorway 22 m, residential 7 m, …), and NWB carries nothing that could replace it.
- **No bridge or tunnel geometry.** Not a polygon, not an outline, not a clearance.
  `bst_code` is a carriageway subtype (`HR` main carriageway, `AFR`/`OPR` exit and
  entry, `VBR` connecting curve, `FP` cycle path, …) and names no structure. The
  only structure-adjacent field in the whole schema is `rel_hoogte`, an integer
  grade level.

## The three gaps it might have filled are already closed

- **Bridge geometry and height, from elsewhere.**
  [bridge-heights.md](bridge-heights.md) — Shortbread's `bridges` polygon layer
  supplies the outline, AHN's DSM-minus-DTM supplies a *measured* clearance, and
  `resolve_bridge_decks` lifts the deck and the road span onto it.
- **Surveyed carriageway polygons lost this argument already, on a better
  source.** `crates/geo/src/paving.rs:7-35` argues it for the BGT: replacing the
  OSM ribbons loses the road texture, the kerb casing and the depth ladder, and
  "the BGT cannot supply any of that". NWB's centrelines are strictly weaker than
  the BGT polygons that lost.
- **Grade separation is recovered without `rel_hoogte`.**
  [grade-separation.md](grade-separation.md) records that Shortbread ships no
  `layer`, that this was expected to need an external source, and that the
  expectation was wrong: the *dangle* rule carries the fix (59 refusals to the
  grade rule's 4 on the Groningen fixture), and
  `two_viaducts_crossing_are_not_a_junction` covers the interchange case.

`rel_hoogte` is mostly absent anyway. Measured 2026-08-29, unpaged: **363 of 400**
null at Knooppunt Oudenrijn (`bbox=5.045,52.060,5.085,52.085`), **969 of 1000**
null over Groningen centre (`bbox=6.555,53.207,6.580,53.222`).

## What it would cost, and what it would change

Paged on the `rel="next"` link over `bbox=6.555,53.207,6.580,53.222` — ~1.7 × 1.7 km
of Groningen centre, comparable to the z14 cell the additive trees bin at
(`crates/cartopolis/src/systems/map/surveyed.rs:36`) — on 2026-08-29:
**1,392 features, 2,033,782 bytes (2.03 MB) of raw GeoJSON, in two requests.**
That is the same order as the heaviest thing the client streams today (paving,
3.8 MB for the densest cell), spent on a second copy of centrelines the MVT
already delivers for free.

Two paging traps, both verified. **The page cap is 1000 features whatever `limit`
asks for**, and the response carries `numberReturned` but **no `numberMatched`** —
so the ordinary "a short page means the last page" loop stops at a thousand and
reports nothing, which is the truncation trap `CLAUDE.md` already records for the
BAG WFS. And **`offset` is not supported**: it answers `400 unknown query
parameter(s) found: offset`, so the only way through is the `rel="next"` link.

And it changes only the data behind the picture, not the picture. `geobron_nm`
shows the geometry is largely BGT-derived and so more accurate than OSM's — but
the client draws a constant-width ribbon around whatever centreline it is given,
so a decimetre-accurate centreline under an invented width is invisible.

## Two pointers, with what was and was not verified

Both are what somebody will reach for NWB expecting; neither is wired.

- **Lane counts exist, in a different service.** WEGGEG,
  `https://api.pdok.nl/rws/weggegevens/ogc/v1`, collections `wegvak_max_snelheden`
  and `wegvak_rijstroken`, joined to NWB on `wvk_id`. **Verified 2026-08-29:** its
  `rel="license"` is CC0 1.0, `wegvak_rijstroken` returns `omschr: "2 -> 2"` beside
  a `wvk_id`, and it covers **Rijkswaterstaat roads only** — the same Groningen box
  returns **2** lane features against NWB's 1,392, so it is a motorway dataset, not
  a city one. **Not verified:** whether those widths would improve `road_width_m`,
  or what a national fetch costs.
- **Surveyed bridge polygons are in the BGT**, which this project already extracts.
  **Verified 2026-08-29:** `https://api.pdok.nl/lv/bgt/ogc/v1` exposes
  `overbruggingsdeel` and `kunstwerkdeel_vlak` / `_lijn` / `_punt`. **Not
  verified:** their volume, their coverage, or whether they beat the Shortbread
  outline + AHN clearance [bridge-heights.md](bridge-heights.md) already measures.
  If surveyed bridge geometry is ever wanted it belongs to the "BGT, the remaining
  layers" frontier row, not to this one.

## How every number above was measured

All read-only, all run 2026-08-29 from the agent container.

```bash
B=https://api.pdok.nl/rws/nationaal-wegenbestand-wegen/ogc/v1

curl -sL "$B?f=json" | jq '.links[] | select(.rel=="license")'   # CC0 1.0
curl -sL "$B/collections?f=json" | jq -r '.collections[].id'     # two collections
curl -sL "$B/collections/wegvakken/schema?f=json" | jq '.properties.geometry'

# Volume: follow rel="next" until absent, summing .features|length and the byte
# count of each response body. 2 requests, 1000 + 392 features, 2,033,782 bytes.
curl -sL "$B/collections/wegvakken/items?bbox=6.555,53.207,6.580,53.222&limit=1000&f=json"

# rel_hoogte, first page only (no paging — the null share is the point).
curl -sL "$B/collections/wegvakken/items?bbox=5.045,52.060,5.085,52.085&limit=400&f=json" \
  | jq '[.features[].properties.rel_hoogte] | group_by(.) | map({v:.[0],n:length})'

# WEGGEG lane coverage over the same city box → 2.
curl -sL "https://api.pdok.nl/rws/weggegevens/ogc/v1/collections/wegvak_rijstroken/items?bbox=6.555,53.207,6.580,53.222&limit=500&f=json" \
  | jq '.features | length'
```
