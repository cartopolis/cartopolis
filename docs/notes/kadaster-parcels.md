---
topic: kadaster-parcels
triggers: [Kadaster, kadastralekaart, Perceel, KadastraleGrens, "parcel boundaries", "plot boundaries", cadastre, PDOK, "garden fence", scheiding_lijn, LineKind, pipeline/mod.rs, pipeline/gardens.rs, systems::surveyed]
updated: 2026-08-29
---

# Kadaster parcels: checked against the live service, and not taken

The frontier row promised "plot boundaries — the gardens between the buildings".
**Both halves are wrong.** A parcel is the whole plot — house, drive and all —
and the cadastral boundary that bounds it is a *legal* line with no physical
object on it. Checked 2026-08-29 against
`https://service.pdok.nl/kadaster/kadastralekaart/wfs/v5_0` (WFS 2.0), five
feature types: `Perceel`, `KadastraleGrens`, `Bebouwing`,
`Nummeraanduidingreeks`, `OpenbareRuimteNaam`. A daily-refreshed bulk download API
exists beside it (`https://api.pdok.nl/kadaster/kadastralekaart/download/v5_0/dataset`,
`perceel` current to 2026-08-28), so the interface was never the obstacle either.

The licence is **CC0 1.0**, read off the service's own GetCapabilities —
`ows:AccessConstraints` is `https://creativecommons.org/publicdomain/zero/1.0/deed.nl`
and `ows:Fees` is `none` — not off a wiki. So this was rejected on content, as NWB
was. (Cadastral *ownership* is separately not open;
[building-identity-and-bag.md](building-identity-and-bag.md) records that.)

## Why it lost: 93% of the network has nothing standing on it

The physical boundaries — fences, walls, hedges — are already surveyed by the BGT,
already streamed and already drawn, as `LineKind::{Fence,Wall,Hedge}` in
`crates/geo/src/furniture.rs`, meshed by `furniture::build_line_meshes` from
`surveyed::surveyed_lines`. Measured over the same box on 2026-08-29: **312.4 km**
of cadastral boundary (`KadastraleGrens`) against **21.9 km** of surveyed physical
boundary (BGT `scheiding_lijn`, 1,232 current features after the
`termination_date` filter — consistent with the "~18 km at the worst"
[surveyed-street-objects.md](surveyed-street-objects.md) records).

So drawing the cadastre lays roughly **290 km of fence per cell where none was
surveyed**. That inverts value 1, *Measured beats plausible*: an invented object
standing where the register says nothing at all.

The gardens the row wanted are the BGT's `begroeidterreindeel` — vegetated ground,
deferred with a reason under "What is not read, and why" in
[surveyed-street-objects.md](surveyed-street-objects.md), and belonging to the
*"BGT, the remaining layers"* frontier row. Nothing in the cadastre distinguishes
garden from building from driveway.

## What it would cost

Over `bbox=6.555,53.207,6.580,53.222` — the ~1.7 × 1.7 km of Groningen centre the
NWB check used, comparable to the z14 cell the surveyed layers bin into
(`crates/cartopolis/src/systems/map/surveyed.rs`) — on 2026-08-29:

| Feature type | Features | Raw GeoJSON | Requests |
|---|---|---|---|
| `Perceel` | 8,855 | 14.0 MB | 12 |
| `KadastraleGrens` | 29,676 unique (30,459 matched) | 30.4 MB | 34 |

`Perceel` alone is **3.7× the paving layer's densest cell** (3.8 MB), which is
already the second-heaviest thing the client streams. The other three feature
types were counted, not fetched whole; their counts are in the pointers below.

## Four paging traps, all verified

Worse than the BAG WFS truncation `building-identity-and-bag.md` records, in two
distinct ways.

- **`numberMatched` in the GeoJSON body is cumulative, not a total.** It reports
  `startIndex + returned`. Page 0 says `941`; `startIndex=941` says `1919`;
  `startIndex=1800` says `2755`. The true total is 8,855.
- **A short page is not the last page.** Page 0 returns 941 against `count=1000`
  with 7,914 still to come, and raising `count` changes nothing (`CountDefault`
  is 1000; `count=5000` returns byte-identical responses). The proof it is
  truncation and not an honest total: a bbox *strictly inside* the big one
  returns **more** features (972) than the box containing it (941).
- **`resultType=hits` is the only honest total** — a `wfs:FeatureCollection`
  header with a real `numberMatched` and no features.
- **Paging is not transaction-safe, and the capabilities say so**
  (`PagingIsTransactionSafe = FALSE`). Walking `Perceel` by `startIndex` returned
  8,855 features of which only **8,638 were unique** — it both duplicates and
  drops. Any extractor built on this must de-duplicate on `id` and check the
  result against `resultType=hits`.

## Pointers, with what was and was not verified

- **Cadastral building footprints** (`Bebouwing`, 7,148 — *verified count only*).
  Already superseded: the BAG outline ships on the building's own record and
  carries the `identificatie` that joins it to everything else
  ([building-identity-and-bag.md](building-identity-and-bag.md)); the cadastral
  footprint does not.
- **House-number label placement** (`Nummeraanduidingreeks`, 7,263 — *verified
  count only*). **Not verified:** whether it beats the BAG's per-unit entrance
  points. This client draws no house-number labels at all; numbers reach the user
  through the proximity readout.
- **Street-name label placement** (`OpenbareRuimteNaam`, 387 — *verified count
  only*). **Not verified:** whether a cartographic anchor point would improve
  `systems::street_labels`, which places view-independently from world footprints
  on purpose and would have to give that up to use one.
- **Plot area** — `kadastraleGrootteWaarde` is on every `Perceel` (164.0 m² on the
  first sampled). Data behind the picture, not picture; value 6 rules it out.
- **The honest counter-argument, so it is not re-derived.** BGT `scheiding_lijn`
  is optional "IMGeo plus" content like the tree points, so some of that 93% gap
  may be under-registration rather than absence. The answer if it ever matters is
  better handling of BGT coverage, not inventing a fence from a legal line — and
  the number that would settle it is the surveyed-boundary length in a
  municipality with known-complete `scheiding_lijn` coverage, unmeasured by
  anyone.

## How every number above was measured

All read-only, all run 2026-08-29 from the agent container.

```bash
B=https://service.pdok.nl/kadaster/kadastralekaart/wfs/v5_0
BB=53.207,6.555,53.222,6.580,urn:ogc:def:crs:EPSG::4326
G="service=WFS&version=2.0.0&request=GetFeature&typeNames=kadastralekaart"

# Licence, fees, the five feature types, CountDefault=1000, paging FALSE.
curl -sL "$B?request=GetCapabilities&service=WFS" > caps.xml
grep -oE 'publicdomain/zero/1.0[^<]*|<ows:Fees>[^<]*|<Name>kadastralekaart:[^<]*' caps.xml
grep -A2 'name="CountDefault"\|PagingIsTransactionSafe' caps.xml

# The only honest total. → 8855 and 30459.
for T in Perceel KadastraleGrens; do
  curl -sL "$B?$G:$T&resultType=hits&bbox=$BB" | grep -o 'numberMatched="[0-9]*"'
done

# The cumulative-numberMatched trap. → 941 / 1919 / 2755, against a true 8855.
J="$G:Perceel&outputFormat=application/json&srsName=urn:ogc:def:crs:EPSG::4326"
for S in 0 941 1800; do
  curl -sL "$B?$J&bbox=$BB&count=1000&startIndex=$S" \
    | jq '{returned: (.features|length), numberMatched}'
done

# Volume, uniqueness and boundary length: page by startIndex to the hits total,
# summing response bytes, then jq the ids for uniqueness (8855 → 8638) and the
# KadastraleGrens coordinates for the 312.4 km.
```
