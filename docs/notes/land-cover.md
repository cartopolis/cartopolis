---
topic: land-cover
triggers: [landcover, "land cover", WorldCover, landcover_fills, LAND_KINDS, "/tiles/landcover", GLOBE_DEEP_ALBEDO, SPACE_ATMOSPHERE_DENSITY, palette, "globe colour",
  landcover.rs, palette.rs, tile_loader.rs, globe.rs, update_globe_albedo, GlobeSurfaceAlbedo]
updated: 2026-10-17
---

# Land cover under the map

## Why it exists

The OSM tiles (VersaTiles `osm.versatiles`, Shortbread schema) carry **no `land`
layer below z7–z8**. Checked 2026-09-27 against `tiles.cartopolis.org`: z4 and
z6 hold `boundaries`, `ocean`, `water_polygons`, labels and (z6) `streets`; `land`
first appears at z8. So the globe and every far view drew continents as bare
`palette::PAPER`. No palette change could fix that: there was nothing to colour.

## The source

VersaTiles publishes `landcover-vectors.versatiles`: ESA WorldCover 2021, **CC BY
4.0**, z0–z10, 2.09 GB, vector (MVT, brotli). It has two layers:

- `land`, whose `kind` is one of `farmland`, `forest`, `grassland`, `heath`,
  `marsh`, `scrub`, `bare_rock` or `residential`.
- `water_polygons` (z0–z3 only), whose `kind` is `water` or `glacier`.

The names are close enough to Shortbread's that one table serves both. The
credit is a licence condition, so it is in `LICENSE-THIRD-PARTY.md` (About) and
on the Layers panel.

Hosting: the file is in `/srv/versatiles/` on the VPS beside `osm.versatiles`,
and the `tiles` container serves it as a second set (`[landcover]` in its
`command`, `/data/stack/docker-compose.yml`), so it is
`tiles.cartopolis.org/tiles/landcover/{z}/{x}/{y}`. There was no Caddy change,
because Caddy proxies the whole host.

Two alternatives were checked on 2026-09-27 and not taken:

- **`osm-landcover.versatiles`** is OSM with the land cover merged in, z0–z14,
  68 GB. It is a drop-in replacement, but it cannot sit beside the 66.5 GB
  `osm.versatiles` on a disk with 42 GB free. Both sets also call their layer
  `land`, so merging mixes the two sets' polygons in one layer.
- **`satellite.versatiles`** is 1.7 TB of WebP imagery.

## How it is drawn

`tile_loader::landcover_underlay` fetches the land-cover tile under each vector
basemap tile, for the flat map and the globe alike. A source deeper than z10
reads its z10 ancestor and keeps only the polygons that reach it.
`vector_tiles::render_mvt_layered` paints them straight onto the paper, **under
every layer of the tile**, so OSM's own land use covers them wherever OSM has
any. The URL is derived by swapping `/tiles/osm/` for `/tiles/landcover/` in
the map's template. `CARTO_LANDCOVER_URL` overrides it and `off` disables it.
Both rendered-tile cache keys hash the underlay.

**One colour table, `LAND_KINDS`, for both sources**, grouped the way
`land_group` groups kinds for the meshed ground: scrub, heath and marsh draw as
grass, bare rock and scree as sand. Distinguishing cropland from grassland was
considered and not done. OSM's `farmland` in the Netherlands is mostly pasture,
while WorldCover's is real cropland; one kind cannot take two colours, and giving
it a non-grass colour would also put a step at the edge of the meshed ring.

## The palette is realistic, and the globe is not tinted

Asked for on 2026-09-27: the ground in the colours it has in aerial and
satellite imagery. Two styled palettes were tried first and withdrawn — the pale
paper (grass `#D2DEC6`) and the game green (bc1dfdb, reverted in dc01bc5) — and
so was a stop between them. `palette`'s module note has the values.

Measured on the Deck at 14,000 km over Africa, from one launch per setting,
each variant changing only two numbers:

| land tint | deep-space density | Sahara | Congo | ocean |
|---|---|---|---|---|
| 0.75 | 0.40 (old) | (133,126,138) | (105,127,144) | (81,110,150) |
| 1.0  | 0.40 | (161,143,140) | (125,143,145) | (87,116,153) |
| 1.0  | 0.15 (kept) | (166,150,129) | (122,142,117) | (71,97,128) |
| 1.0  | 0.06 | (168,153,127) | (121,142,101) | (64,87,113) |

At the old settings, light scattered by the air outweighed the realistic land,
so the desert came out bluer than it was red. At 0.06 the disc reads as a flat
map with no air over it. At 0.15 the limb still glows, because its path through
the 2.5× shell is long.

"Land tint" is `globe::GLOBE_DEEP_ALBEDO`, and the history explains why the knob
still exists at 1.0. The paper basemap reflected ~0.8 where the real Earth
averages ~0.3, which put the sunlit hemisphere on the tonemapper's shoulder as a
flat pale ball; stopping the camera down barely darkens a shoulder (and dims the
limb and the stars with it), so the darkening went on the imagery instead — 0.55
against the paper palette, 0.75 for a while, 1.0 since this change. The ramp is
kept (`update_globe_albedo`, riding `space_factor`, with `GlobeSurfaceAlbedo`
read at tile spawn) so a brighter basemap source can be damped again. **Whatever
the deep value, it must be 1.0 at the tier boundary**: the globe replaces the
flat tiles exactly there, and a fixed tint stepped the map a stop darker the
instant it did.
