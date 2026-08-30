---
topic: rijkswaterstaat-water-levels
triggers: [Rijkswaterstaat, waterwebservices, ddapi20, WATHTE, Waterhoogte, NAP,
  "water level", "canal level", "river level", peilgebied, "water at its real height",
  OphalenCatalogus, OphalenWaarnemingen]
updated: 2026-08-30
---

# Rijkswaterstaat water levels: checked and not taken

The frontier row promised "canal and river surfaces at their real height". The
levels are real, they are CC0, they are keyless — and **the client cannot draw
water below the ground it sits in**. Checked 2026-08-30 against the live service;
the geometry finding is the one that settles it, and it is about this tree, not
about Rijkswaterstaat.

## The interface exists, and it moved

`https://waterwebservices.rijkswaterstaat.nl/…` now answers **301** to
`https://rijkswaterstaatdata.nl/projecten/waterwebservices-overschakeling/`, a
migration notice. The live host is `https://ddapi20-waterwebservices.rijkswaterstaat.nl/`.
Two POST endpoints, both keyless — an `X-API-KEY` header is documented and **not
enforced**; every request below was sent without one and answered 200.

| Request | Body | Answer |
|---|---|---|
| `POST /METADATASERVICES/OphalenCatalogus` | `{"CatalogusFilter":{"Compartimenten":true,"Grootheden":true}}` | 200, **1,573,954 B** |
| `POST /ONLINEWAARNEMINGENSERVICES/OphalenWaarnemingen` | one station, `WATHTE`, `ProcesType: meting`, one hour | 200, **3,374 B**, 7 measurements |

The catalogue holds **2,498** locations, each with `Lat`/`Lon` in ETRS89 (≈WGS84).
Joining `AquoMetadataLijst` (`Grootheid.Code == "WATHTE"`, Waterhoogte) to
`LocatieLijst` through `AquoMetadataLocatieLijst` gives **705** water-height
stations. A station-hour is 10-minute cadence, `Eenheid: cm`, `Hoedanigheid: NAP`
— the `groningen` station read **−91 cm NAP** over the sample hour. Volume is not
an obstacle either way.

## The licence is not the obstacle

`https://rijkswaterstaatdata.nl/waterdata/` states: *"Op de inhoud van de
WaterWebservices is de Creative Commons zero verklaring (CC0) van toepassing."*
Redistributing a cached extract is allowed. Same shape as NWB, NDW and the BGT —
**rejected on geometry, not on terms.**

## The stations are present but they are points, not a field

Great-circle distance from a city centre to the nearest `WATHTE` station, over
the catalogue's own 705 coordinates, computed 2026-08-30:

| Centre (lat, lng) | nearest | ≤1 km | ≤3.2 km | ≤10 km |
|---|---|---|---|---|
| Groningen Grote Markt (53.2194, 6.5665) | 1.95 km (`groningen`) | 0 | 1 | 3 |
| Amsterdam Dam (52.3731, 4.8926) | 1.53 km (`buiksloot`) | 0 | 1 | 7 |
| Utrecht Domplein (52.0907, 5.1214) | 6.84 km (`nieuwegein.doorslag`) | 0 | 0 | 4 |
| Rotterdam Blaak (51.9200, 4.4890) | 0.56 km (`rotterdam.nieuwemaas.boompjes`) | 2 | 5 | 12 |
| Leiden centre (52.1601, 4.4970) | 8.74 km (`katwijk`) | 0 | 0 | 2 |
| Nijmegen Waal (51.8460, 5.8640) | 0.55 km (`nijmegen`) | 1 | 4 | 10 |

Unlike the NDW loops ([ndw-traffic.md](ndw-traffic.md)) the stations are not
absent from cities — the two river cities have one within a kilometre. But this
is a register of Rijkswaterstaat's own main-water network, and a city canal is
usually a water board's boezem, which is why four of the six centres have nothing
inside a kilometre.

## A station says nothing about which water it measures

A `LocatieLijst` entry carries exactly seven fields: `Locatie_MessageID`, `Code`,
`Coordinatenstelsel`, `Lat`, `Lon`, `Naam`, `Omschrijving`. **There is no water-body
identifier of any kind** — no waterway code, no geometry, no reference to a BGT or
Top10NL water polygon. 469 of the 705 codes carry a dot-separated component that
often reads as a water body (`rotterdam.nieuwemaas.boompjes`, `andel.maas`), but it
is freeform text, 236 have none at all, and `groningen` — the one whose −0.91 m
would be attached to the Diepenring — is one of those. It sits at 53.2281, 6.5919,
about 2 km out on the Van Starkenborghkanaal.

So attaching a level to a drawn polygon is a **nearest-station guess against
value 1**: the register measures 705 points and does not say which surface each
one governs.

## What actually stops it is this tree's ground stack

Even given a perfect level per polygon, the client has nowhere to put it.

- A water surface takes **one height for the whole cell** — every water polygon in
  a tile is merged into one mesh before levelling, and the level is the *median* of
  the terrain field under it (`crates/cartopolis/src/systems/map/map_geometry.rs:1786`,
  `crates/cartopolis/src/systems/map/terrain.rs:218`; see
  [terrain-relief.md](terrain-relief.md)).
- Over a canal that median is **the quay**. AHN's DTM is no-data over water
  (`crates/cartopolis/src/systems/map/height.rs:583`) and `FILL_PASSES` dilates the
  banks inward over ~150 m (`crates/cartopolis/src/systems/map/terrain.rs:47`, `:84`).
- Water is one rung of a **millimetre-thick coplanar stack**: `y_offset` −0.0035 m
  (`crates/geo/src/vector_tiles.rs:1283`), `depth_rung` 4.0
  (`crates/geo/src/vector_tiles.rs:1350`). The whole ladder spans 8 mm.
- Under it, **the basemap clipmap is eight opaque layers deep and nothing cuts holes
  in it** (`crates/cartopolis/src/systems/map/map_stream.rs:884`). Its finest level
  rests at y = **−0.03 m** (`crates/cartopolis/src/systems/map/map_stream.rs:627`) —
  2.65 cm under the water sheet.
- The depth bias cannot rescue it: water's 256 units read as "~15 cm at 5 km and
  ~150 µm at 10 m" and "cannot punch through a building, which is separated by
  metres of honest depth" (`crates/geo/src/vector_tiles.rs:1329`).

And there is no canal wall anywhere in this tree. The surface stack is a **sheet,
not a solid** — drop the water and you see under the world, not into a canal.

## How far the drop actually is (measured 2026-08-30)

Method: PDOK's AHN WCS directly (`dtm_05m`, EPSG:28992 GeoTIFF, the request shape
of `HeightRequest` at `crates/cartopolis/src/systems/map/height.rs:534`), no-data
being FLT_MAX (`3.4028e38`, which is what `NODATA_ABOVE = 1.0e30` at
`height.rs:97` catches). Statistics over the finite samples only.

| Box | no-data | median of the rest |
|---|---|---|
| Grote Markt, 100 m box at 16² — the anchor probe the client itself makes (`height.rs:518`, `GROUND_PROBE_HALF_M`/`_GRID`) | 36 % | **+7.57 m NAP** |
| Noorderhaven, 20 m | 0 % | +4.61 m NAP |
| Zuiderhaven, 20 m | 45 % | +3.39 m NAP |
| Verbindingskanaal, 20 m | 25 % | +3.57 m NAP |
| Schuitendiep, 20 m | 78 % | +1.66 m NAP |
| Oosterhaven, 20 m | 25 % | +1.06 m NAP |

The water itself is unmeasured, as expected; the surviving samples are bank and
quay and run **+1.1 to +4.6 m NAP** across the Diepenring, on a city centre whose
anchor datum is +7.6 m (Groningen sits on the Hondsrug). The register says the
water is at **−0.91 m NAP**.

So the drop is **two to five metres**, not millimetres. That lands the
measurement firmly on the wrong side of the 8 mm the coplanar stack can hold and
the 2.65 cm to the clipmap: the canal would go under an opaque, continuous
basemap plane, with no bank geometry anywhere to close the gap. It does not
change the conclusion — it only says how far from workable it is.

## Which value settled it

`docs/direction.md` **value 5, "the frame budget is not negotiable"**, and
**value 3, "degrade, never break"**, as a pair. What the row asks for is not a
data wire-in: it is a rewrite of the ground sheet into a solid — a hole in the
clipmap under every water polygon, plus bank and quay geometry. The intermediate
state is not a degraded picture, it is a broken one, with the water hidden under
the basemap and the world visible through the gap where the bank should be.

**Value 1, "measured beats plausible"**, is the case *for* the row and loses on
the join: nothing in the register says which polygon a station governs, so every
canal's level would be a guess.

**Value 6, "ship the smallest thing a machine can judge"**, agrees: with no
visible change available there is no `--dump-state` field an `--expect` could
hold.

## What would have to exist before this is revisited

Two things, in this order. Neither is this ticket.

1. **A hole in the clipmap under water polygons.** `hidden_tiles`
   (`map_stream.rs:884`) hides a tile only when finer ready tiles paint over it
   completely; nothing subtracts geometry. Water at a real level needs the ground
   to *stop* under it.
2. **Bank and quay geometry.** A dropped sheet with no wall is a hole in the
   world. The BGT already surveys the kerbs and the water's edge that this would
   be built from ([surveyed-street-objects.md](surveyed-street-objects.md)).

With both standing, the row is a data question again, and this note is the
starting point rather than a repeat of the check.

**One pointer, not checked here:** the water boards publish `peilgebied` /
`peilbesluit` — target levels as *areas*, alongside the `waterkering` lines
`systems::dikes` already streams ([flood-defences.md](flood-defences.md)). An area
answers the join question a station cannot. It is a different source with a
different provider and would be its own frontier row; nothing about it was
verified, and the geometry obstacle above applies to it identically.

## How every number above was measured

All read-only, all run 2026-08-30 from the agent container.

```bash
# The old host is being retired: 301 to the migration notice.
curl -s -o /dev/null -w '%{http_code} %{redirect_url}\n' \
  https://waterwebservices.rijkswaterstaat.nl/METADATASERVICES/OphalenCatalogus

# Catalogue: status and bytes. No X-API-KEY sent.
curl -s -o /tmp/cat.json -w '%{http_code} %{size_download}\n' \
  -X POST https://ddapi20-waterwebservices.rijkswaterstaat.nl/METADATASERVICES/OphalenCatalogus \
  -H 'Content-Type: application/json' \
  -d '{"CatalogusFilter":{"Compartimenten":true,"Grootheden":true}}'

# One station-hour of measured water height.
curl -s -o /tmp/obs.json -w '%{http_code} %{size_download}\n' \
  -X POST https://ddapi20-waterwebservices.rijkswaterstaat.nl/ONLINEWAARNEMINGENSERVICES/OphalenWaarnemingen \
  -H 'Content-Type: application/json' \
  -d '{"Locatie":{"Code":"groningen"},
       "AquoPlusWaarnemingMetadata":{"AquoMetadata":{"Compartiment":{"Code":"OW"},
         "Grootheid":{"Code":"WATHTE"},"ProcesType":"meting"}},
       "Periode":{"Begindatumtijd":"2026-08-29T20:00:00.000+02:00",
                  "Einddatumtijd":"2026-08-29T21:00:00.000+02:00"}}'

# Licence.
curl -sL https://rijkswaterstaatdata.nl/waterdata/ | sed 's/<[^>]*>/ /g' | grep -i "creative commons"
```

Station counts and distances come from `/tmp/cat.json` alone: take the
`AquoMetadata_MessageID`s whose `Grootheid.Code` is `WATHTE`, map them to
`Locatie_MessageID`s through `AquoMetadataLocatieLijst`, look those up in
`LocatieLijst`, then haversine against the six centres.

The AHN boxes are one WCS `GetCoverage` each, converting WGS84 to RD with the
standard EPSG:28992 polynomial:

```
https://service.pdok.nl/rws/ahn/wcs/v1_0?SERVICE=WCS&VERSION=1.0.0&REQUEST=GetCoverage
  &COVERAGE=dtm_05m&CRS=EPSG:28992&FORMAT=GEOTIFF&BBOX=<x0,y0,x1,y1>&WIDTH=N&HEIGHT=N
```

The GeoTIFF comes back **Deflate-compressed with the floating-point predictor**
(compression 8, predictor 3), so a naive strip read gives nonsense — undo the
horizontal byte difference per row, then de-interleave the four byte planes
MSB-first before reading each float.
