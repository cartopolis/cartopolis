---
topic: cold-start-bytes
triggers: ["utils/cache.rs", MAX_CACHE_BYTES, sweep_cache_root, "disk cache", ground_sheets.rs, paving_edges.rs, ground_bytes, ground_stages, "pipeline/ground.rs", "pipeline/buildings.rs", formats/osm_buildings.rs, osm_buildings.rs, terrain_tile.rs, terrain_tiles.rs, "cold start", "first visit", "bytes on the wire", "data over the line", "loads slowly", encode_bin, ".bin.gz", precompressed]
updated: 2026-11-22
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

## The encoding is at its floor; the kerb bands were a third of the triangles

Measured 2026-11-21 on the same cell, format 3 as served (2.43 MB Brotli), by
decoding it and re-encoding each stream on its own
(`cargo run -p cartopolis_atlas --example ground_bytes -- <url>`):

| stream | Brotli q10 | share |
|---|---|---|
| positions (460 k distinct vertices) | 1.59 MB | 66 % |
| references (1.37 M) | 0.74 MB | 30 % |
| colours, UVs, rungs | 0.10 MB | 4 % |

What was tried on the encoding, and lost:

- **Brotli q11, 16 MB window**: −0.02 %.
- **Positions at 1 mm**: −0.2 % after Brotli — the quarter-millimetre bits
  carry no information Brotli was not already finding. **1 cm**: −15 %, but
  61 k triangles (13 %) collapse or flip, which is the sliver geometry the
  earlier note meant.
- **Vertices in Morton order**: positions −30 %, but the references lose the
  high-water trick (+95 %) and the colour runs break up: net **+15 %**.
- **Predicting a vertex from the others of its triangle**: +2 %.

So the bytes follow the vertex and triangle counts, and those were the lever.
The cell had 476 k triangles of which 93 k were under 10 cm² and 103 k under
**1 mm wide**; building the cell stage by stage
(`--example ground_stages -- 8490 5319`) put 96 k of those in the paving as it
left `paving_edges::refine`, against 574 for the same surfaces with no bands.
The kerb and worn runs were polylines with a vertex every `PIECE_M` down a
straight edge, and the stroke in `split` made each of them a vertex on the
band's inner side, exactly on the line between its neighbours: 151 k of the
397 k ring vertices after `refine` sat within 1 mm of their chord, against 704
of the survey's 135 k. A run now keeps only the ring's own vertices and its two
ends. The cell: 476 k → 340 k triangles, 2.43 → **1.92 MB** Brotli (−21 %);
the paving 372 k → 235 k triangles, its hairlines 93 k → 8 k. What is left under
a millimetre is mostly the cut under the roads (D in the stages output) and the
landuse cut, a few thousand each.

Dropping every triangle under 1 mm wide from the *old* file would have saved
13 % — but a dropped triangle is a hole with nothing under it, so that is not a
lever; removing the vertices that made them is.

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

## The other JSON layers travel packed (2026-11-21)

The BAG facts and places, the BGT paving and objects, the roof colours and the
registers were seven-decimal JSON — already short, unlike the far blocks — and
what Brotli left on the table was the numbers. Estimated first by splitting the
decoded documents into a coordinate-delta stream, an id stream and the text
skeleton (64–85 % of the JSON's Brotli size per layer), then built as **one
transform for every JSON cell file** rather than a codec per layer:
`formats::packed` takes every two-element array of ≤7-decimal numbers out of
the text as zigzag varint deltas and leaves the token `-0.0` in its place.
atlas writes `x.pj` beside every `x.json` it writes (`Ctx::write_json`,
`write_doc`), `atlas pack-json --out site` does it for a tree extracted before;
the client asks for the `.pj` first through `platform::packed_json` (one latch
per host, as `lod22` has for its binary), unpacks it to the same JSON text off
the frame and stores that under the layer's old cache key, so no parser and no
key changed. Measured on the dense cell, as served (Brotli q10):

| layer | JSON | packed | |
|---|---|---|---|
| `bag/…json` | 316 KB | 225 KB | 71 % |
| `bag/…places.json` | 148 KB | 128 KB | 86 % |
| `bgt/…surfaces.json` | 444 KB | 321 KB | 72 % |
| `bgt/…json` | 25 KB | 17 KB | 68 % |
| `roofs/…json` | 375 KB | 315 KB | 84 % |
| `registers/…json` | 197 KB | 147 KB | 74 % |

1.5 MB → 1.15 MB for the cell (−23 %), about 2.5 MB over a first visit's
rings. Packing a cell takes 160–220 ms in atlas, unpacking 6–17 ms in the
client. A document that already holds the token, or that would not come back
as the same values, is served as JSON only; a string's bytes are copied as
bytes, which is what keeps `Curaçaostraat` whole.

## What a visit keeps on disk (2026-11-21)

The same street-level visit leaves **~300 MB** in the disk cache, against the
55 MB it fetched compressed: what is stored is the decoded body. One visit to
the Grote Markt at 40 m (`mac-2`, a fresh `CARTO_CACHE_DIR`): finished basemap
tiles 79 MB (`rendered*`, PNG then; since 2026-11-22 the BC3 chain, ~0.3× the bytes, `keeping-what-was-expensive.md`), the JSON layers 111 MB (roofs 38, BGT
34, BAG 32, registers 6), vector tiles 30, ground 22, terrain 15. The sweep
caps the tree at 512 MiB (`utils::cache::MAX_CACHE_BYTES`), so that was under
two places.

Two changes, native only (the web build's store is IndexedDB, bounded by the
browser, and inflating there would be on the frame's thread):

- **Cache entries ending `.json` are deflated on disk** (`platform::storage`,
  level 1, over 8 KiB), told apart from older plain entries by the gzip
  magic. The visit: 293 → **200 MB**, the JSON layers 111 → ~40 MB. A warm
  start was no slower (39.4 s).
- **The sweep evicts the least recently *used*, not the least recently
  fetched.** A cache read sets the file's atime (`utils::cache::touch`) and the
  sweep orders by the later of atime and mtime. The mtime is left alone: it is
  what `storage::age_secs` reads for a feed's staleness. Before, the city a
  person opened every day sorted as old as the day it was first fetched.

## What is left, largest first

1. The ground file, ~13.5 MB for nine street cells after the kerb-band fix (was
   21.9 at format 2, ~17 at format 3).
2. The other JSON layers, ~10 MB together, now travel packed (above, −23 %);
   what is left in them is their text — keys, ids, names — and the roofs'
   per-surface colour objects.
3. The vector tiles are VersaTiles' own encoding and not ours to change.
