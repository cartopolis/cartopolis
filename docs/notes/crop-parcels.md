---
topic: crop-parcels
triggers: [crops.rs, BRP, gewaspercelen, gewascode, RVO, brpgewas, "crop parcels", "farmland", SurfaceGroup::Crop, turf.rs, arable]
updated: 2026-08-30
---

# Crop parcels (RVO BRP)

Every agricultural field in the Netherlands with the crop grown on it that year,
from the RVO's Basisregistratie Gewaspercelen. `cartopolis_geo::crops` is the
pure half, `systems::crops` the streaming half, and the extractor is not written
yet — it belongs in atlas's `crates/atlas/src/pipeline/` (the TypeScript
pipeline, where it was meant to go, never had one), so the layer draws nothing anywhere until it is.

## The interface, and what it costs

`https://api.pdok.nl/rvo/gewaspercelen/ogc/v1` — one collection, `brpgewas`, OGC
API Features, **keyless**, `bbox` query, GeoJSON out (storage CRS EPSG:28992,
CRS84 offered). Note the path: there is no `brpgewaspercelen` under `/rvo/`.

**Licence: Public Domain Mark 1.0**, named by the collections document itself
(`creativecommons.org/publicdomain/mark/1.0/deed.nl`). Caching and redistributing
what we serve needs nobody's agreement.

One year is served — `jaar: 2025`, `status: "Definitief"`, snapshot 15 May — with
no superseded versions in the response, so this does **not** have the BGT's
ghost-version trap (`systems::surveyed`). The parser folds exact duplicates
anyway, because a cached cell outlives the extractor that wrote it.

Measured **2026-08-30** by `curl` against the live service, one request per exact
z14 cell (the zoom `map_geometry` streams at):

| Cell | Features | GeoJSON | Vertices | Drawn (crop) features / vertices |
|---|---|---|---|---|
| 8488/5315, farmland N of Groningen | 308 | 921 KB | 22,824 | 122 / 13,220 |
| 8449/5374, Flevoland arable | 199 | 232 KB | 4,954 | 120 / 3,475 |
| 8490/5320, Groningen Grote Markt | 0 | 1 KB | 0 | 0 / 0 |

Douglas–Peucker at 0.5 m keeps 31–32 % of the drawn vertices, so the heaviest
cell measured is ~40 KB of coordinates — against the 3.8 MB of surveyed paving
that is already the second-heaviest thing this client streams. Volume was never
the obstacle. Zero inside the city and zero outside the Netherlands is the
degrade-to-nothing shape the other streamers have.

The frontier row's own figures (163 parcels / 501 KB) do not reproduce on a z14
cell; the table above replaces them.

## Two things about the data that decide the implementation

**Most features are not crops.** Over the Groningen cell 186 of 308 are
`Landschapselement`, and 316 of 340 of those sampled over the wider box are
`Sloot` — ditches. Those are *water*, and Shortbread's `water_polygons` /
`water_lines` already draw them; filling one with land puts ground where the
register says there is none. So `CropCategory` has no variant for it, the wire
position is reserved and non-drawable, and the parser drops the record. Category
counts across four sampled regions: `Landschapselement` 1616, `Grasland` 1428,
`Bouwland` 728, `Natuurterrein` 226, `Braakland` 1, `Overige` 1.

**Parcels straddle cells, so the paving's binning rule cannot be copied.**
`surveyed_paving` drops any surface whose outer ring leaves its cell. Measured
2026-08-30, binning each parcel to the cell containing its centroid and then
asking whether any vertex falls outside that cell: **22 %** over the Groningen
farmland cell and **24 %** over Flevoland. Copying the rule would delete a
quarter of the country's fields, so a parcel is projected and drawn *whole* by
the one cell that owns it — hence `surveyed::cell_local_unclamped`. The overhang
is flat fill over a neighbour drawing the same ground in the same tone.

## The honest limit

Over the Groningen farmland cell **94 % of the parcel area is `Grasland`**, and
about half the country's agricultural area is grassland nationally. That is green
repainted a more accurate green: half the countryside gains a confirmed fact and
very little picture. Where it does change the picture it changes it a lot — over
the Flevoland cell 93 % of the parcel area is `Bouwland` carrying 33 distinct
crops, all of it drawn in one `palette::GRASS` before this.

## The crop tones are a curated subset, deliberately

`crops::rgb` maps ~60 `gewascode`s to fourteen families; everything else falls
back to the parcel's *category* tone, because the register carries several hundred
codes and adds more each year, and an unlisted one must never take a neighbouring
crop's colour (the rule `land_group` follows for an unlisted OSM kind). The
curated set came from a national sample — eight bboxes, 2026-08-30 — ranked by
area: permanent grassland 18 %, consumption potato 12 %, nature 9.5 %, ley 8.6 %,
winter wheat 5.3 %, maize 5.3 %, sugar beet 4.2 %, then a long tail. Every code in
the table was read off the live service; one that looks like it belongs to a
family but was not observed is absent rather than guessed.

**No season.** A tulip field is red for a fortnight in April; the register records
the crop, not the flower. Every tone is a muted year-round one.

## Depth

`SurfaceGroup::Crop` sits at rung **2.5**, `y_offset` **−0.0080** — half a step
above `Grass` (2.0 / −0.0085) and below `Forest` (3.0 / −0.0075). Half the usual
step because a parcel only ever overlaps `Grass`: the register covers farmland,
which OSM draws as grass, and it is never coplanar with a wood or a beach. At
`DEPTH_BIAS_STEP` that is 32 ULP of margin. **Unverified on real hardware** — the
`--flicker` instrument is what would confirm it, and it needs a host serving
`/brp/coverage.json` before there is anything to photograph.
