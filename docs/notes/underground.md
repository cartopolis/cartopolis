---
topic: underground
triggers: [underground.rs, formats/underground, geotop, GeoTOP, BRO, publiek.broservices.nl, bhrgt, cpt, gmw, gld, GUF, wko, buisleiding, rev-portaal, Bodemloket, Tankbestand, KNMI, fdsnws, NLOG, gasfield, dirsurveys, stratinterpretations, Rotliegend]
updated: 2026-10-08
---

# The underground layers: where each comes from, and what it cannot say

Seven layers under Map ▸ Underground besides the sewers (`sewers.md`). All
write one format, `cartopolis_geo::formats::underground` (columns, lines,
points, rings; depth in metres below the ground), and one client module
streams them (`systems::underground`). Researched and measured 2026-10-08 over
Groningen, RD 229000–238000 × 578000–587000.

## Soil model — `geotop`

TNO's OPeNDAP server, `https://www.dinodata.nl/opendap/GeoTOP/geotop.nc` —
**the `www.` host**; the bare one fails the TLS handshake. Subset by index
`lithok[x0:1:x1][y0:1:y1][0:1:312]`, order **[x][y][z]**, and the `.ascii`
suffix answers plain text, so no NetCDF reader is needed (`.nc` is classic
NetCDF if one ever is). Grid: x from 13600, y from 338500, both 100 m; z from
−50 to +106 m NAP in 0.5 m. Coordinates are a voxel's **lower-left corner and
bottom face**; libraries disagree (nlmod reads z as the centre), the extents
settle it. No-data −127 everywhere above the ground, so the top valid voxel is
the ground. Over water it is the **bed**. CC0. One request covers a city in
well under a second; PDOK only offers a 4.6 GB national zip.

## Boreholes and soundings — `boreholes`

`publiek.broservices.nl` — POST `…/characteristics/searches` with a lat/lon
(EPSG:4258) bounding box; XML back. **Over 2,000 hits the search is refused
whole** (HTTP 400, "Het aantal objecten is groter dan 2000") — split the box.
Paths: `sr/bhrgt/v2`, `sr/bhrp/v2`, `sr/bhrg/v3` (v1/v2 answer 503),
`sr/cpt/v1`. Groningen: 346 + 1,145 + 142 boreholes, 3,029 soundings. A
borehole's layers need one GET of its object (~15–25 KB); a sounding's depth is
in the hit. Depths below ground already. CC0.

## Groundwater — `groundwater`

PDOK OGC API `bro-grondwatermonitoring-in-samenhang-karakteristieken`
(the `/bzk/` path redirects to `/tno/`): `gm_gmw` (857), `gm_gmw_monitoringtube`
(958), `gm_gld` (526) in three bulk calls; an RD `bbox-crs` is accepted and
returns nothing, so use WGS84. All levels are **m NAP**. The latest value takes
two GETs per dossier — `observationsSummary`, then
`observations/{id}?mostRecentTVP=ja` (`true` is a 400). **Never the plain
`objects/{GLD}`**: one answered 41 MB.

## Heat storage — `wko`

The BRO's GUF register is the only open source with depths and has **no WFS**:
a daily national GeoPackage (57 MB) from PDOK's atom feed. Relational, not
simple features (atlas's `pipeline/wko.rs` has the joins). Migrated LGR
screens are untidy and coverage is incomplete — 114 loops here against 865 in
RVO's ArcGIS WKO-tool, which has loop depths but **no filter depth for open
wells** and no licence. Rejected as a second source for that reason.

## Transport pipelines — `pipelines`

REV GeoServer, `rev_public:bl_buisleidingreferentie`, one request (495 in
Groningen). **`bovenkantbuis` is empty for every segment here**; drawn at 1 m
cover, and it is the one layer whose depth is a convention rather than data.
Numbers arrive as strings. No licence stated.

## Gas field — `gasfield`

* KNMI FDSN (`rdsa.knmi.nl/fdsnws/event/1/query`, `format=text`): 1,903
  induced events in the province, **1,887 at the fixed 3.0 km** — the drawn
  depth is KNMI's convention. CC BY 4.0.
* NLOG WFS (`gdngeoservices.nl/geoserver/nlog/ows`, EPSG:4326 out gives
  lng,lat): wells (378 in the field) and field polygons. The WFS trajectories
  are 2D; the 3D path is the map viewer's
  `POST nlog.nl/nlog-mapviewer/rest/brh/dirsurveys` (body: the borehole id
  from the well's `URL`), and the lithostratigraphy `…/stratinterpretations`
  gives along-hole depths. The reservoir is the first `RO…` unit of the
  `preferredBln: "J"` interpretation; the field outline is drawn at the median
  of its wells' reservoir tops (TNO's DGM-deep grid would do it too, but is
  ED50/UTM in a zip, and its `RO_tvd` is the reservoir's **base**, not its top).
  NLOG states no open licence.
* **Subsidence is not drawn**: the only machine-readable source is NAM's
  ArcGIS on Shell's servers, no licence, and benchmark values whose units do
  not add up. Bodemdalingskaart.nl needs registration.

## Soil contamination — `contamination`

Bodemloket's ArcGIS (`lks_blk_rd_v1/MapServer/0`) has assessments
(`STATUS_OORD`) but **the City of Groningen does not deliver to it** — over the
city it holds nothing. The city's own `Bodeminformatie` layers carry no status
at all. What it does publish is `Tankbestand` (2,549 underground tanks, with
whether they were removed, filled, or left), which is what is drawn there; a
removed tank is not. The city's filled-in canals (`Dempingen`, 2,595) are an
obvious next layer and are not drawn yet.

## Two things in front of anything deep, both found the hard way (2026-10-08)

* **The ground skirt.** `free_camera` lays a paper-toned cap 25 m down to carry
  the ground past the last tile (`GROUND_SKIRT_SINK`). It is a plain material,
  so it does not take the screen door, and everything deeper than it — the
  backdrop, a soil column's foot, every gas well — was hidden, with the
  street's own shadows on it so it read as a surface under the opening. The
  sewers never met it (their backdrop sat at 14 m). While the ground is open,
  `sewers::follow_backdrop` sinks it under the backdrop. Diagnosed by making the
  opening discard every pixel: what remained was blank paper with shadows.
* **The atmosphere's aerial perspective.** Bevy's sky pass hazes every pixel by
  the length of the camera ray to it, material fog settings or not, so three
  kilometres "of air" turned the opening blue-grey. `render_sky.wgsl` is now a
  fourth patched copy (`atmosphere_patch`): a pixel well below the surface for
  its distance is hazed only to where its ray enters the ground. A/B over
  Loppersum: blue-grey without, soil-brown with.

`--dump-state` carries `backdrop_present`, `backdrop_visible`,
`backdrop_depth_m` and `ground_unopened` (materials still solid while the ground
should be open — must be 0), which separate these failures without a picture.
