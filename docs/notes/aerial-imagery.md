---
topic: aerial-imagery
triggers: [aerial.rs, map_stream.rs, tile_source.rs, Luchtfoto, PDOK, ortho25, orthoHR, "aerial", "satellite", "photo basemap", CARTO_AERIAL_LAYER, beeldmateriaal]
updated: 2026-08-29
---

# The aerial basemap (PDOK Luchtfoto RGB)

The flat tier's ground is a *render* of vector geometry, and it stops being one
at z14: Shortbread's `MAX_SOURCE_ZOOM` is four levels short of the clipmap's z17
anchor zoom, so 179 of a fresh anchor's 424 tiles are magnified z14 renders
(`tile-raster-cost.md`). PDOK serves those zooms natively, in true colour. The
layer is `systems::aerial` plus one branch in `map_stream`'s dispatch; it is off
by default and it is the only consumer that changes source.

## What was measured, and when

Probed from the agent container on **2026-08-29**, `curl` against
`https://service.pdok.nl/hwh/luchtfotorgb/wmts/v1_0/…`:

```bash
curl -s -o /dev/null -D - -w 'size=%{size_download}\n' \
  'https://service.pdok.nl/hwh/luchtfotorgb/wmts/v1_0/Actueel_ortho25/EPSG:3857/14/8425/5405.jpeg'
```

| fact | measured |
|---|---|
| status / type | `200`, `image/jpeg`, 256×256 |
| body size | 32,946 B (ortho25 z14, Groningen); 27,700 B (ortho25 z17) |
| CORS | `Access-Control-Allow-Origin: *` |
| caching | `Cache-Control: public, max-age=259200`, plus `ETag` / `Last-Modified` |
| zoom identifiers | unpadded — `14`, `17`, `20`, `21` all resolve |
| **outside NL** (Berlin, z14, tile 8791/5364) | `200` and a **1,651 B near-uniform JPEG** — not a 404 |

`GetCapabilities` lists `Actueel_orthoHR`, `Actueel_ortho25` and the per-year
archive back to `2016_ortho25`, all `image/jpeg`, all offering `EPSG:3857` and
`OGC:1.0:GoogleMapsCompatible` at z00–z21, 256 px.

## Coverage is an envelope, and it cannot be anything else

Two candidates were rejected, both by measurement.

**HTTP status.** The Berlin row above is why: a client that decided coverage
from the response would paper the whole planet in grey rather than falling back.

**The service's own `WGS84BoundingBox`.** It reads
`-1.657 48.041 12.432 56.111`, which is most of the Low Countries plus Germany
and half of France. That is the *tile matrix set*'s extent — the EPSG:3857 grid
the RD projection is republished on — not where imagery exists. Reading it as
coverage would be the status check with extra steps.

So the envelope is AHN's (`systems::height`'s `AHN_LON`/`AHN_LAT`, out of that
service's `lonLatEnvelope`). Same country, same publisher, and already the one
constant this client trusts for "is this the Netherlands".

## Licence

CC BY 4.0, from [PDOK Luchtfoto RGB (Open)][pdok]: free for all applications,
with a request to reference `beeldmateriaal.nl`. Caching and redistributing
derived pixels are permitted; the obligation is the credit, which is the
Beeldmateriaal record in `LICENSE-THIRD-PARTY.md` that the Settings panel
renders.

[pdok]: https://www.pdok.nl/introductie/-/article/pdok-luchtfoto-rgb-open-

## ortho25 rather than orthoHR

`Actueel_ortho25` is 25 cm and flown in summer; `Actueel_orthoHR` is 8 cm and
flown leaf-off in winter. The default is ortho25 for two reasons, neither of them
about file size.

The clipmap tops out at z17, whose ground scale at 53° N is ~0.36 m/px at
`tile_px = 512` and ~0.71 m/px at 256 — so **25 cm source is already finer than
anything this renderer can show**, and orthoHR would be oversampled 4–5×. And the
summer flight matches the foliage `systems::vegetation` and `systems::grass` draw
over it; on winter imagery the drawn trees would stand over their own bare
branches. `CARTO_AERIAL_LAYER` switches layers for a side-by-side, which is a
workstation call — colour is not evidence on the container's software rasteriser.

## Why there is no extractor, and no mirror

The surveyed layers each have a pipeline stage in atlas (`crates/atlas/src/pipeline/`)
because their sources are WFS/GML needing RD→WGS84 reprojection and tessellation
(`where-the-map-data-is-built.md`). Imagery needs none of that: already tiled,
already Web Mercator, already CDN-cached, already CORS-open. The in-tree
precedent is the height layer — reach the service from the client, behind a
Layers toggle that is off by default — and this is the same case with a simpler
payload.

Mirroring is not affordable either: the Netherlands at z20 is on the order of
78 million tiles at ~20 KB, i.e. **~1.5 TB**, on a VPS that also hosts nominatim,
overpass and the tile server. `systems::coverage`'s `CoverageSlot` therefore
gains no slot.

## What is still unjudged

Three things a headless capture cannot answer, all deferred to a workstation:

- Whether a photographic backdrop reads well under the drawn `land` / `water` /
  `sites` polygons at street level. `tessellate_surfaces` keeps drawing them —
  deciding whether a park should stop being painted green over a photograph of
  itself is a separate look pass.
- The **coarse-zoom border**. A tile takes its source from its own centre, so one
  tile is never half photograph and half drawing — but a z10 tile is ~39 km
  across, and one whose centre is inside the envelope can still reach over
  Germany or the North Sea, where PDOK renders blank. How that reads at the rim
  of the clipmap is a look call, and tightening it (whole-tile containment, or a
  zoom floor) is the fix if it reads badly.
- The texel-density trade: PDOK's 256 px tile is ~0.71 m/px at z17 against the
  vector render's ~0.36 m/px at `tile_px = 512`. Real ground detail at half the
  sampling — and the input to whether this should ever become default-on.
