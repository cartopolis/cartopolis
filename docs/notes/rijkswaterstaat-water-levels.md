---
topic: rijkswaterstaat-water-levels
triggers: [Rijkswaterstaat, waterwebservices, ddapi20, WATHTE, Waterhoogte, NAP,
  "water level", "canal level", "river level", peilgebied, "water at its real height",
  OphalenCatalogus, OphalenWaarnemingen, WaterBody, "3d basisvoorziening", CityJSON,
  vastpeil, zomerpeil, winterpeil, IMWA, "canal depth", quay, fill_holes, water_levels.rs]
updated: 2026-09-16
---

# Water levels: where they are, and what still stops the canals

> **The extractor exists now, 2026-09-16.** Everything below described a client
> that streamed `/water/{z}/{x}/{y}.json` and a data source that could fill it —
> and nothing ever did. The route answered **404 on the deployed host**, so the
> layer shipped with no rings at all and every canal stayed at quay height for
> three days while the notes said the problem was solved. cartopy's
> `server/pipeline/water.ts` writes the tree now, out of the same 2 km blocks the
> wall heights come from ([walls-have-a-measured-height.md](walls-have-a-measured-height.md)).
> Measured over the study pond in Groningen: **−0.95 m NAP** from the 3D
> product against **−0.85 m** read independently off the AHN surface, and the
> client reports `water_rings=247`, 112,525 samples carved and `water_drop_m=2.33`
> where it reported zero for all three.
>
> The lesson is the one worth keeping: **a client layer with no extractor behind it
> looks exactly like a client layer that does not work**, and `water_rings=0` in
> `--dump-state` was saying so the whole time.

> **Superseded in part, 2026-09-13.** Two of this note's conclusions were wrong; the
> corrections are at the bottom under *What 2026-09-13 changed*. A measured,
> per-polygon water level **does** exist nationally (the Kadaster's `WaterBody`, not
> Rijkswaterstaat's stations) and is wired in as `systems::water_levels`; and the
> geometry obstacle is not the one described here. Read that section first.

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

## What 2026-09-13 changed

Re-checked while answering "the canals in Groningen are at quay height — can't you
deduct that from the data sources?". Four findings; two correct this note, and one is
now shipped as `crates/cartopolis/src/systems/map/water_levels.rs`.

### The `peilgebied` pointer above is wrong, and it was worth checking

The closing paragraph suggested the water boards' `peilgebied` areas would answer the
join question a station cannot. **They carry no level.** PDOK publishes them nationally
(`https://api.pdok.nl/hwh/waterschappen-waterbeheergebieden-imwa/ogc/v1`, collections
`peilgebied`, `peilbesluitgebied`, `peilafwijkinggebied`); over Groningen `peilgebied`
returns "Eemsboezem", Waterschap Hunze en Aa's, and every attribute is a code, a name
or a status. `peilbesluitgebied` the same.

Rijkswaterstaat's own **`peilgebieden`** layer, a different dataset
(`https://geo.rijkswaterstaat.nl/services/ogc/gdr/water_en_bodem_ro/ogc/features/v1`),
*does* carry `vastpeil` / `zomerpeil` / `winterpeil`. They are **mostly null**: sampled
over six city centres, only Utrecht had values (`vastpeil` 0.55 and −0.40 m NAP);
Groningen, Amsterdam, Leiden, Rivierenland and Zeeland were null. The right field
exists and the data behind it does not, nationally.

### The Kadaster ships a measured level per polygon, and that settles the join

`basisbestand_gebouwen_terreinen` in the 3D basisvoorziening
([3d-basisvoorziening.md](3d-basisvoorziening.md)) — the collection that note lists as
"ground, water, roads" — contains **`WaterBody` objects with an AHN-draped z**.

Measured on the Groningen centre 2 km block (`volledig_2025_232000_582000.zip`,
14.1 MB → 77.7 MB of CityJSON, fetched 2026-09-11):

| | |
|---|---|
| `WaterBody` objects in the block | **53** |
| geometry | `MultiSurface`, LoD 1.2, `3df_class: "Water"`, BGT `Waterdeel_vlak` attributes |
| each body's own z spread | flat: 2,541 vertices over 0.61–0.64 m; 444 vertices all at −0.69 |
| median z across the 53 | **−0.46 m NAP** (range −1.47 to +3.29) |

Against the quays this note measured at +1.1 to +4.6 m NAP, and Rijkswaterstaat's
`groningen` station at −0.91 m. **These are polygons carrying a level**, which is what
the station register could not supply, and they are national, CC BY 4.0, in the same
2 km sheets the buildings already come in. Over four Groningen blocks: 528 rings,
214 ha of water, **40–150 kB per cell** after a quarter-metre Douglas-Peucker.

**Extract the boundary with directed edges.** An edge is on the boundary when its
reverse is absent; counting them undirected loses the winding, so a vertex where two
parts of one body touch gets several outgoing edges and the walk chains through it into
a self-crossing polygon spanning both. Even-odd point-in-polygon then answers nonsense
for that ring, silently — the first version of this had 40 such rings and produced an
A/B capture that looked plausible and was junk.

### The geometry obstacle is a different one

This note says the blocker is that "the basemap clipmap is eight opaque layers deep and
nothing cuts holes in it", and gives the finest level's resting height as y = −0.03 m.
**That describes the flat case only.** With the height layer on,
`map_stream::apply_terrain_to_tiles` rebuilds every clipmap tile under
`RELIEF_MAX_TILE_M` (2,500 m) as a grid displaced by the terrain field — the ground
already follows measured relief, and nothing needs a hole cut in it. The canal has to
be *in* the field.

Two things stopped that, and neither is the one above:

1. **The field actively filled the canal in.** AHN's DTM is no-data over water (the
   Verbindingskanaal box sampled 2026-09-11 is 100 % no-data, the Schuitendiep 64 %), so
   a canal arrives as a hole — and `terrain::fill_holes` closes holes by averaging their
   neighbours inward over six dilations, written so a cut-out viaduct does not read as a
   pit. Over a canal that averages the two quays together. **Fixed**: `terrain::carve_water`
   runs *before* the fill.
2. **25 m cannot express a canal.** `TERRAIN_SAMPLE_M` is ~25 m and the water board's own
   recorded width for the Reitdiep is 25.3 m — one sample across, which cannot have two
   banks. **Partly fixed**: `terrain::FINE_HALF_M`, a 512 m half-metre tier under the
   camera, one WCS request per re-centre. It feeds **point** consumers only
   (`TerrainField::ground_at`), because subdivision everywhere else is sized off
   `TERRAIN_SAMPLE_M` and a mesh with a vertex every 24 m sampling a half-metre field
   would alias it.

### What is still not solved

The basemap is displaced by the *coarse* field, so it dips by only as much as 25 m
sampling can express — measured 2026-09-12, the waterline on screen moved **1 px** where
the drop asked for five. The water surface is therefore clamped to the ground drawn
under it (`map_geometry::water_surface`): the measured level wins in the middle of a
body, and at the bank the water rises to meet the quay rather than vanishing under an
opaque basemap. That is the honest picture of what the ground model can currently say,
and it deepens on its own as the field gets finer.

Making a canal actually *look* like a canal needs the clipmap relief grid refined near a
registered ring, or the quads inside one omitted and the staircase edge covered by an
apron earcut from the real outline. Neither is done. The arithmetic that rules out the
naive version: refining the finest level uniformly to 4 m is 343k triangles against the
**708** the whole clipmap uses today, and `map_stream.rs` already records a uniform
relief grid taking the frame rate from 26 fps to 10.

### One trap in the metrics

`terrain_relief_m` is **not** evidence about the carve. It is the range over the whole
6.4 km box, which already spans 37.7 m of the Hondsrug, so a canal cut to −4 m never
widens it — an identical value proves nothing either way. That misreading cost half a
day. `water_carved_coarse` / `water_carved_fine` / `water_drop_m` in `--dump-state`
count it where it happens, and are what found the real bug.

### How the new numbers were measured

```bash
# Water-board peilgebied areas: present over Groningen, no level attribute.
curl -s "https://api.pdok.nl/hwh/waterschappen-waterbeheergebieden-imwa/ogc/v1\
/collections/peilgebied/items?bbox=6.560,53.215,6.575,53.225&limit=5&f=json"

# Rijkswaterstaat's peilgebieden: the level fields exist, mostly unpopulated.
curl -s "https://geo.rijkswaterstaat.nl/services/ogc/gdr/water_en_bodem_ro/ogc\
/features/v1/collections/peilgebieden/items?bbox=5.105,52.082,5.135,52.100&f=json"

# The Kadaster sheet index, then the block. Note -L: the download 302s.
curl -s "https://api.pdok.nl/kadaster/3d-basisvoorziening/ogc/v1/collections\
/basisbestand_gebouwen_terreinen/items?bbox=6.560,53.215,6.570,53.222&limit=60&f=json"
curl -sL -o t.zip "https://download.pdok.nl/kadaster/basisvoorziening-3d/v1_0\
/2025/volledig/volledig_2025_232000_582000.zip"
```

The `WaterBody` z values come from that CityJSON directly: every object of
`type == "WaterBody"`, its boundaries walked to vertex indices, each index read out of
`vertices` and put back through `transform.scale[2]` and `transform.translate[2]`.

**The extractor is not written yet.** These cells were produced by a throwaway script
against a loopback host to develop the client side; the real one belongs beside the
other four in [cartopy](where-the-map-data-is-built.md), and until it exists
`water_levels` finds no coverage and every consumer behaves exactly as it did before.
