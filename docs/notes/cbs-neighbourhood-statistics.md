---
topic: cbs-neighbourhood-statistics
triggers: [CBS, wijkenbuurten, buurten, wijken, gemeenten, "neighbourhood statistics", "population density", aantalInwoners, bevolkingsdichtheid, personenautosPerKm2, percentageOnbewoond, PDOK, place_labels]
updated: 2026-08-29
---

# CBS neighbourhood statistics: checked against the live service, and not taken

The frontier row asked "how many people are actually there". The data exists, is
free, and is tiny — and it still loses, because **there is no picture in it**.
Checked 2026-08-29 against
`https://service.pdok.nl/cbs/wijkenbuurten/2024/wfs/v1_0` (WFS 2.0), three feature
types: `buurten` (neighbourhoods), `wijken` (districts), `gemeenten`
(municipalities). A 2023 service of the same shape sits beside it.

The licence is **CC0 1.0**, read off the service's own GetCapabilities —
`ows:AccessConstraints` is
`https://creativecommons.org/publicdomain/zero/1.0/deed.nl` and `ows:Fees` is
`none` — not off a wiki. As with Kadaster and NWB, the licence was never the
obstacle; this was rejected on content.

## What it costs, and what is in the bytes

Over `bbox=6.555,53.207,6.580,53.222` — the same ~1.7 × 1.7 km of Groningen centre
the [Kadaster](kadaster-parcels.md) and [NWB](nwb-road-network.md) checks used,
comparable to a z14 surveyed cell:

| Feature type | Features |
|---|---|
| `buurten` | 23 |
| `wijken` | 6 |
| `gemeenten` | 1 |

The `buurten` GeoJSON is **257,021 bytes**, of which the geometry is **27,582** —
696 coordinate pairs across 22 features, i.e. heavily generalised administrative
outlines. So **89% of the payload is the statistics table**: 244 attributes per
feature, 5,368 values in the response, of which **1,614 (30%) are the suppression
sentinel `-99997`**. That ratio is the finding: this is not a heavy source that
has to be trimmed, it is a light one that is almost entirely table.

## Why it lost: no object, and every consumer already reads something finer

The only geometry is the buurt/wijk/gemeente outline, which is an
**administrative line with no physical object standing on it** — precisely why
the cadastre lost (value 1, [kadaster-parcels.md](kadaster-parcels.md)). Everything
else is data behind the picture rather than picture, which
[kadaster-parcels.md](kadaster-parcels.md) already applied value 6 to when it ruled
out plot area.

The strongest case for taking it is value 1 the other way round — ground an
invented density in a measured one. It fails at all three candidate consumers:

- **Ambient traffic.** The pool is fixed at `MAX_CARS = 180`
  (`crates/cartopolis/src/systems/nav/traffic.rs:69`) placed within
  `PLACE_RADIUS_M = 260.0` (`:77`), and the constant says of itself that it is
  *"Sized for 'the street I am looking at is alive', not for a traffic census"*
  (`:59`) — the module note puts the question as *"is this street alive"* (`:43`).
  That is ≈ **850 cars/km²**, already below the 1,971 `personenautosPerKm2`
  measured in Binnenstad-Noord, so a measured figure could only ever place
  *fewer* cars. And CBS counts a **stock of registered vehicles, not a flow**,
  while the layer draws moving traffic. Flow was checked separately and lost too
  ([ndw-traffic.md](ndw-traffic.md), same day) — which does not promote this one:
  the two fail differently, and neither failure makes the other's number usable.
- **Lit windows.** Block liveliness hashes at `BLOCK_M = 25.0` m
  (`crates/cartopolis/src/systems/map/building_windows.rs:99`) — a buurt is one to
  two orders of magnitude coarser. The occupancy channel it multiplies
  (`assets/shaders/building_windows.wesl:163-164`) is already fed **per building**
  by the BAG's use bits through `osm_buildings::night_lit_scale`
  (`crates/cartopolis/src/systems/map/osm_buildings.rs:515`).
  `percentageOnbewoond` is a neighbourhood average of the same fact the client
  already reads per object.
- **Walkers.** Same fixed-pool shape, `MAX_WALKERS = 220`
  (`crates/cartopolis/src/systems/nav/traffic.rs:508`). CBS measures where people
  **live**, not who is on the pavement — the shops, the station and the university
  are. Turning a residence count into a street-presence count is an inference
  wearing a measurement's clothes, which is the one thing value 1 exists to stop.

Parked cars are already sized from the lot's own surveyed area
(`crates/geo/src/parking.rs:11-12`), which is finer than a buurt aggregate.

## Paging: the same family as the Kadaster service

`PagingIsTransactionSafe = FALSE`, `CountDefault = 1000`. `resultType=hits` says 23
`buurten`; page 0 returns **22** (all ids unique), and `startIndex=22` returns
`Sterrebosbuurt` — the last feature of page 0 again, a duplicate rather than the
missing 23rd. Any extractor would have to de-duplicate on `id` and reconcile
against `hits`, exactly as the paging section of
[kadaster-parcels.md](kadaster-parcels.md) records. Not restated here.

## Pointers, with what was and was not verified

- **The vector tile already carries a population figure, and nothing reads it.**
  The `place_labels` layer — 21 features on the Groningen z14 fixture, properties
  `kind` / `name` / `population` — is listed as used for *nothing* in
  [vector-tile-unused-layers.md](vector-tile-unused-layers.md), and the only
  mention of it in the crates is an unrelated buffer-width comment
  (`crates/geo/src/vector_tiles.rs:282`). *Verified:* it is in the bytes
  `map_geometry` already fetches, so it costs no request and no extra decode, and
  it is global rather than Dutch-only. **Not verified:** what its `population`
  actually resolves to per feature, or whether it is usable at cell resolution.
  That is its own ticket off that note, not this one.
- **The honest counter-argument, so it is not re-derived.** CBS is the *only*
  source here for how many people live in a specific block. If this project ever
  grows a layer whose subject is residents rather than streets — occupancy that
  varies by neighbourhood instead of by hash, or a readout naming the buurt you
  are standing in — the data exists, is CC0, and is small. **Not verified, by
  anyone:** whether a buurt-resolution figure visibly beats the per-building BAG
  occupancy already in the shader. That is the number that would reopen this.

## How every number above was measured

All read-only, all run 2026-08-29 from the agent container.

```bash
B=https://service.pdok.nl/cbs/wijkenbuurten/2024/wfs/v1_0
BB=53.207,6.555,53.222,6.580,urn:ogc:def:crs:EPSG::4326
G="service=WFS&version=2.0.0&request=GetFeature&typeNames=wijkenbuurten"

# Licence, fees, the three feature types, CountDefault=1000, paging FALSE.
curl -sL "$B?request=GetCapabilities&service=WFS" > caps.xml
grep -oE 'publicdomain/zero/1.0[^<]*|<ows:Fees>[^<]*|<Name>wijkenbuurten:[^<]*' caps.xml
grep -A3 'PagingIsTransactionSafe\|name="CountDefault"' caps.xml

# The only honest total. → 23, 6, 1.
for T in buurten wijken gemeenten; do
  curl -sL "$B?$G:$T&resultType=hits&bbox=$BB" | grep -o 'numberMatched="[0-9]*"'
done

# Volume and the table/geometry split. → 257021 / 22 / 27582 / 5368 / 1614 / 244.
J="$G:buurten&outputFormat=application/json&srsName=urn:ogc:def:crs:EPSG::4326"
curl -sL "$B?$J&bbox=$BB&count=1000" -o buurten.json -w '%{size_download}\n'
jq '.features|length'                                                buurten.json
jq '[.features[]|.geometry|tostring|length]|add'                     buurten.json
jq '[.features[].properties|to_entries[]]|length'                    buurten.json
jq '[.features[].properties|to_entries[]|select(.value==-99997)]|length' buurten.json
jq '.features[0].properties|length'                                  buurten.json

# The duplicate page. → one feature, "Sterrebosbuurt", same as page 0's last.
curl -sL "$B?$J&bbox=$BB&count=1000&startIndex=22" \
  | jq '[.features[].properties.buurtnaam]'
```
