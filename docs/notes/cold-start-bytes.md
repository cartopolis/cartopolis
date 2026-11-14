---
topic: cold-start-bytes
triggers: [ground_sheets.rs, "pipeline/ground.rs", "pipeline/buildings.rs", formats/osm_buildings.rs, osm_buildings.rs, terrain_tile.rs, terrain_tiles.rs, "cold start", "first visit", "bytes on the wire", "data over the line", "loads slowly", encode_bin, ".bin.gz", precompressed]
updated: 2026-11-14
---

# What a first visit downloads, and where the bytes can still go

Measured 2026-11-14 against the live hosts (3dbag.cartopolis.org, cartopolis.org,
tiles.cartopolis.org), around the Grote Markt (z14 8490/5319), asking with
`Accept-Encoding: br, gzip` as the client does and summing `size_download`
over each layer's ring. The rings are the code's (`FAR_CELL_RADIUS`, the ground
ring, `bag`'s radius 2, the 6.4 km terrain field), read from the source, not
counted from a running client — a `--shot` with an empty `CARTO_CACHE_DIR`
reports `fetch_mb` per route and is the check on these sums.

| layer | files | on the wire |
|---|---|---|
| pre-cut ground (`ground/…bin`, street level only) | 9 | **21.9 MB** |
| terrain tiles (`terrain/14/…?grid=513`) | 42 | 7.3 MB |
| LoD2.2 shells (`lod22/…bin`) | 9 | 4.8 MB |
| far LoD1.2 blocks (`buildings/…json`, ring 3 less the LoD2.2 ring) | 40 | 4.4 MB |
| BAG facts + places (`bag/…json`, `.places.json`) | 25 + 25 | 4.0 MB |
| vector tiles, z14 (`tiles/osm/14/…`) | 81 | 3.8 MB |
| BGT paving (`bgt/…surfaces.json`) | 9 | 2.6 MB |
| roof colours and gardens (`roofs/…json`) | 9 | 2.3 MB |
| AHN fine tier (`/height?…grid=1024`, float GeoTIFF) | 1 | 1.4 MB |
| registers, water, street detail, roads, BGT objects | | ~2.3 MB |

About **55 MB** at street level, and about **33 MB** above 500 m, where the
ground file is not asked for. All of it is cached; only the first visit to a
place pays. The paving and roof files are also fetched under nearby basemap
tiles, so their ring reaches ~45 cells (6.4 and 6.0 MB).

## The ground file was a quarter colour noise

One dense cell (8490/5319, format 2): 5.5 MB raw, 3.14 MB Brotli, 477k
triangles of which 373k are paving. Of its bytes, positions were 45 %, the
per-vertex references 28 % and **colours 25 %** — the paving's colour palette
held 49,137 `f32` entries that were 4,244 colours once their last bits were
dropped, so neither the palette nor its runs repeated. Format 3 writes colours in
1/1024 steps (`ground_sheets::COLOUR_STEP`): that cell goes to 2.49 MB Brotli
(−21 %), colours from 1.4 MB to 74 KB.

What was tried on the rest, re-encoding the decoded cell (Brotli q10):

- **Coarser positions are not worth it.** 1 mm saved 0.3 %, 1 cm 23 % of the
  position stream; 1 cm is also coarser than the cut's own sliver geometry. Kept
  at 0.25 mm.
- **Splitting into per-field streams** gained 0.2 %.
- **Shipping outlines instead of triangles** would drop the references (0.71 MB
  of 2.49 after Brotli) and nothing else, since the paving's triangles are close
  to one per vertex already — and it hands the triangulation back to the client.
  Not done.

## Terrain tiles do not compress further

A 513² tile is 526 KB of `u16` and goes out gzipped by atlas at 170–205 KB.
A planar predictor (left + up − up-left) made it **larger** after gzip or zstd
(169 → 220 KB): the AHN surface is noise at the 1 cm step, and a predictor
turns the noise into wider residuals. Coarser steps barely help (5 cm −13 %,
10 cm −20 %). The lever left is fewer samples far from the camera, which is a
change to the field, not to the encoding.

## The far blocks were JSON with fifteen digits a coordinate

`buildings/14/{x}/{y}.json` writes every point as
`{"lat":53.22630817120953,"lon":6.547981787831522}`. The densest cell is 4.18 MB
and 621 KB after Brotli. `formats::osm_buildings::encode_bin` (`.bin` beside the
JSON, 10⁻⁷° grid, tags as an interned string table) is 537 KB raw and
**236 KB after Brotli q10** for that cell — 62 % less (encoded by the codec
itself from the live JSON, 2026-11-14). The client asks for the
binary first and latches to the JSON on a host without it, as for LoD2.2.

## What is left, largest first

1. The ground file, still ~17 MB for nine street cells after format 3.
2. The other JSON layers — BAG places, paving, roofs, registers, ~10 MB together —
   are full-precision decimal text like the far blocks were, and a binary of each
   would likely halve them; not measured per layer.
3. The vector tiles are VersaTiles' own encoding and not ours to change.
