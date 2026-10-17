---
topic: ndw-traffic
triggers: [NDW, trafficspeed, traveltime, "measurement site table", measurement_current,
  actueel_beeld, DATEX, AlertC, OpenLR, VILD, "vehicle counts", "traffic counts",
  "ambient traffic", "loop detector", opendata.ndw.nu, mijn.ndw.nu,
  traffic.rs, nav/traffic.rs, geo/src/traffic.rs, transit_live.rs, pipeline/mod.rs]
updated: 2026-10-17
---

# NDW traffic: checked against the live feeds, and not taken

The frontier row promised "real vehicle counts in place of invented ambient
traffic". The counts are real, they are CC0, and they are **not where the cars
are drawn**. Checked 2026-08-29 against `https://opendata.ndw.nu/`.

The question the layer asks is narrow, which is what settles this: cars are
placed on drivable centrelines within **260 m of the camera**
(`PLACE_RADIUS_M`, `crates/cartopolis/src/systems/nav/traffic.rs:77`) and
recycled at 390 m. So NDW has to answer *is there a measurement on the street
200 m in front of the camera*, not *is there a measurement in this country*.

## The interface is a directory of national files

`https://opendata.ndw.nu/` is a static file listing. **There is no area or bbox
query anywhere on it**: a `?bbox=` parameter is ignored (byte-identical
`Content-Length` with and without it), and `/api/` and `/ogc/v1` are both 404.
Every consumer downloads the whole country.

| File | gzipped | uncompressed | contents |
|---|---|---|---|
| `trafficspeed.xml.gz` | 1,040,873 B | 52,516,313 B | 20,519 site measurements, flow + average speed, restamped every minute |
| `measurement_current.xml.gz` | 11,165,196 B | 388,573,075 B | 101,275 site records; last modified 10 days earlier, so ~weekly |
| `traveltime.xml.gz` | 2,681,102 B | 73,614,528 B | 80,756 sites, `TravelTimeData` only |
| `actueel_beeld.xml.gz` | 319,114 B | 3,113,591 B | 391 DATEX II v3 situations |

`trafficspeed.xml.gz` is restamped every minute, so its size is a per-minute
figure and not a constant — three reads in six minutes gave 1,040,873 /
1,040,968 / 1,043,867 bytes.

## "Vehicle counts" means the loops, and only the loops

The site table splits **20,519 `Point`** records against **80,756
`ItineraryByIndexedLocations`** (stretches). The reporting set in
`trafficspeed.xml.gz` is *exactly* the 20,519 `Point` ids — cross-referenced by
id, no others report and none of the 20,519 is missing from the table. By
equipment the whole table is `fcd` 73,050, `lus` 27,816, `anpr` 333, `radar` 75,
`microwave` 1; the reporting subset is `lus` 20,443, `radar` 75, `microwave` 1.
Counts are loop detectors and nothing else.

## The coverage is absent where the layer runs

Great-circle distance from a city centre to the nearest **reporting** site,
against the site table's own `locationForDisplay` coordinates:

| Point | ≤260 m | ≤390 m | ≤1 km | nearest |
|---|---|---|---|---|
| Groningen, Grote Markt (53.2194, 6.5665) | **0** | 4 | 20 | 311 m |
| Amsterdam, Dam (52.3731, 4.8926) | **0** | **0** | 2 | 919 m |
| Utrecht, Domplein (52.0907, 5.1214) | **0** | **0** | **0** | 1,942 m |
| Rotterdam, Coolsingel (51.9225, 4.4792) | 2 | 4 | 12 | 106 m |
| A2 south of Utrecht (52.0578, 5.0966) | 0 | 2 | 30 | 368 m |

Rotterdam is the honest counterexample and it is a thin one: two loops inside
the placement radius, against a pool of 180 cars. Over the Groningen centre box
the NWB note used (`6.555,53.207,6.580,53.222`, ~1.7 × 1.7 km): 97 site records,
of which **22 report**.

The loops are dense on motorway mainlines and near-absent on city streets —
the inverse of where the pool puts cars, and the motorway case is the one the
existing speed weighting (`cartopolis_geo::traffic::drive_speed`) already prefers.

## There is no key to join on either

A `Point` record locates itself three ways and none of them is a road id.
Measured over all 20,519: `locationForDisplay` lat/lng on **20,519**, an
`alertCPoint` (AlertC/TMC table 6.13 version A, a `specificLocation` code plus
an offset) on **20,519**, and an `openlrExtendedPoint` on **1,833 — 8.9 %**.
**No NWB `wvk_id`, no OSM id**: `wvk`, `osm_id` and `openStreetMap` do not occur
anywhere in the 388 MB file.

So attaching a loop to a drawn Shortbread centreline is a nearest-line-plus-
heading match, or an AlertC decode against the VILD location table
(`VILD6.13.A.zip`, in the same directory), or an OpenLR decode — against a road
network this client does not hold, and which was rejected the day before
([nwb-road-network.md](nwb-road-network.md)). OpenLR is not even generally
available: 91 % of the loops do not carry one.

## The licence is not the obstacle

`https://www.ndw.nu/copyright` states Creative Commons Zero unless a part says
otherwise, with an exception for images. The feeds say it themselves, in the
payload: `<headerInformation><confidentiality>noRestriction</confidentiality>`.
Same shape as NWB — **rejected on content, not on terms.**

## And it would cost a national re-slice every minute

Every surveyed layer here is a static per-cell file built once by atlas's
pipeline ([where-the-map-data-is-built.md](where-the-map-data-is-built.md)). The
one live feed that exists, OVapi transit, is polled by the client directly
because it answers a batched query by stop code
(`crates/cartopolis/src/systems/nav/transit_live.rs:45`, 30 s). NDW offers
neither shape: with no area query, a per-cell NDW service means re-slicing a
national 52 MB XML **every minute**, for a value that is zero in three of the
five places measured above.

## Which value settled it

`docs/direction.md` **value 4, "add, don't replace"**. The row's own wording is
"*in place of* invented ambient traffic", and substituting measurement for the
generated cars empties every street with no loop on it — which is nearly every
street in every city centre measured. **Value 6, "ship the smallest thing a
machine can judge"**, agrees: with zero reporting loops inside the placement
radius at three of the five points, there is no count, placement or settle
number a `--dump-state` assertion could hold.

Value 1, "measured beats plausible", is the case *for* it. It loses on coverage,
not on principle: where a loop exists the measurement is better than the
invention, and there is no loop where anybody is standing.

## Pointers, with what was and was not verified

- **`traveltime.xml.gz` is the well-covered feed, and it carries no counts.**
  **Verified:** 80,756 itinerary sites, `TravelTimeData` only — no `TrafficFlow`
  anywhere in it — each with a `staticReferenceValue` beside the live duration,
  i.e. a congestion *ratio*. 92.5 % report a live value this minute (6,084 answer
  `-1`). City coverage is far better than the loops: 73 `fcd` against 24 `lus`
  in the Groningen centre box, 746 against 461 over the ~15 km city box.
  **Not verified:** whether those AlertC/OpenLR itineraries can be placed on a
  drawn street at acceptable cost, or what a VILD decode would take.
- **`actueel_beeld.xml.gz` is directly placeable.** **Verified:** DATEX II v3
  situations with plain `loc:pointByCoordinates` lat/lng — 391 situations, 558
  coordinates, 3.1 MB uncompressed, no location table needed. **Not verified:**
  its refresh cadence, its density over one city, or whether an incidents layer
  is wanted at all. It is a different frontier row, not this one.
- **The `mijn.ndw.nu` account-gated APIs (OTM, Voertuigpassage) were not
  checked.** They need a credential nobody here has. It does not change the
  answer: the content behind them is the same loop network, and the finding
  above is about where the loops *are*, not about how they are served.

## How every number above was measured

All read-only, all run 2026-08-29 from the agent container.

```bash
# Interface: a static directory, no bbox parameter anywhere.
curl -sL https://opendata.ndw.nu/ | grep -o 'href="[^"]*\.\(gz\|zip\|xml\)"'
curl -sI "https://opendata.ndw.nu/trafficspeed.xml.gz?bbox=6.5,53.2,6.6,53.3"   # same length
curl -s -o /dev/null -w '%{http_code}\n' https://opendata.ndw.nu/api/           # 404

# Licence, both halves.
curl -sL https://www.ndw.nu/copyright | sed 's/<[^>]*>//g' | grep -i "creative commons"
curl -sL https://opendata.ndw.nu/trafficspeed.xml.gz | gzip -dc | grep -o '<confidentiality>[^<]*'

# Volumes.
for f in trafficspeed measurement_current traveltime actueel_beeld; do
  curl -sI "https://opendata.ndw.nu/$f.xml.gz" | grep -i 'content-length\|last-modified'
  curl -sL "https://opendata.ndw.nu/$f.xml.gz" | gzip -dc | wc -c
done

# Reporting sites, and the travel-time feed's data types.
curl -sL https://opendata.ndw.nu/trafficspeed.xml.gz -o ts.xml.gz
gzip -dc ts.xml.gz | grep -o '<siteMeasurements' | wc -l          # 20519
gzip -dc traveltime.xml.gz | grep -o 'xsi:type="[A-Za-z]*"' | sort | uniq -c

# Site table: 388 MB. Split on the token `<measurementSiteRecord id="` — splitting
# on `<measurementSiteRecord` alone also cuts at the nested
# `<measurementSiteRecordVersionTime>` and silently halves every record.
curl -sL https://opendata.ndw.nu/measurement_current.xml.gz -o mst.xml.gz
gzip -dc mst.xml.gz | grep -oi 'wvk_id\|osm_id\|openStreetMap' | sort | uniq -c   # empty
# Then a streaming scan pulling per record: id, <locationForDisplay> lat/lng,
# <measurementSiteLocation xsi:type>, <measurementEquipmentTypeUsed>, and whether
# <alertCPoint> / <openlrExtendedPoint> are present. Cross-reference the ids
# against the <measurementSiteReference id="…"> set in trafficspeed for the
# reporting subset, then take the great-circle distance from each city point.
```
