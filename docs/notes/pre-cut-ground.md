---
topic: pre-cut-ground
triggers: [ground_sheets.rs, surveyed_ground.rs, "pipeline/ground.rs", load_ground_sheets, GROUND_REVISION, PreCut, build_sheets, "ground/coverage.json", "ground/14", fingerprint, "pre-cut", precut, "sheets(atlas)", cut_ground_under_roads_and_paving, gap_fill, map_geometry.rs]
updated: 2026-10-21
---

# Atlas cuts the ground, and the client keeps every way back

A ground cell's flat sheets — landuse fills, surveyed paving, crop parcels, road
ribbons and the gap fill — are cut into one partition before they are draped on
the terrain. That cut was 3.5–4.5 s of a 7–9 s street-level cell on the Steam
Deck (2026-10-21); the other half is the drape, which needs the client's own
terrain field and stays on the client. Atlas's `ground` dataset now runs the cut
per cell and ships it as `ground/14/{x}/{y}.bin` (+ `.gz`, `ground/coverage.json`).

## Why this is not the pipeline that was reverted on 2026-09-19

`b9055279` shipped prepared ground and `6eddb533` took it out the same day,
because triangles have no topology: routing, street names, traffic, trees,
furniture and crossings all need the tile's *lines*, so shipping meshes instead
of the tile breaks them. This ships meshes **in addition to** the tile. The
client still fetches and parses the vector tile, the paving, the parcels and the
decks for everything else; only the sheets are replaced, and only when the file
was cut from exactly what the client has. Every path back to the old behaviour
is kept: no coverage, no file, a 404, a decode error, another `GROUND_REVISION`,
another input fingerprint — each is a cell the client cuts itself.

## One function, two callers

`cartopolis_geo::ground_sheets::build_sheets` is the cut, lifted out of
`map_geometry`'s cell job with its order and its comments. The client calls it
when there is no usable file; atlas calls it to write one. The surveyed inputs'
parsing moved beside it (`cartopolis_geo::surveyed_ground`, re-exported from
`systems::surveyed`, `systems::crops`, `systems::terrain_tiles`), so atlas parses
the paving, roof measurements, parcels and decks with the client's code.

- **Fingerprint.** `ground_sheets::fingerprint` hashes the tile's bytes and the
  parsed inputs *in lat/lng*, before either side's `tile_m` has touched them, so
  the two agree bit for bit when their inputs do. A tile re-imported upstream, a
  re-extracted paving file, or a device cache older than the host is a mismatch.
- **Revision.** `GROUND_REVISION` is a hash of what `build_sheets` builds on the
  fixture with synthetic paving and parcels; the test prints the new value. A
  change to anything in the cut moves it, the hosts' files stop matching, and
  **atlas's `ground` must run again** for the speed-up to come back.
- **Frame.** Atlas cuts at the cell's own `tile_m`; the client's comes from its
  anchor and differs by ~0.03 % (~0.45 m across a cell). `PreCut::sheets` scales
  x and z. The partition ships whole, roads included, so it stays a partition.

## The file, and why it is not raw `f32`

A raw dense street cell was **35 MB (7.5 MB gzipped)** — nine of them would have
added ~67 MB to a first visit. The cut writes triangle soups (exactly 3.00
vertices a triangle over a Groningen street cell), and the client must get
exactly those meshes back, so `encode` stores each distinct vertex once
(positions quantised to 0.25 mm, zigzag-varint deltas; colours and rungs as a
palette with runs), then a high-water-mark reference per vertex, and no indices
for a soup. Dense street cell: **5.8 MB raw, 3.8 MB gzipped**; Groningen's 25
cells around the centre 55 MB gzipped. Snapping is deterministic, so a vertex two
sheets share snaps to one place and the partition stays closed.

Still a lot for a slow mobile link: the file is fetched in the cell's IO phase,
so a link slower than the client's own cut makes that cell *later*. Not seen on
anything measured yet; a timeout on `load_ground_sheets` is the lever if it is.

## Measured

`cargo perf --only descent-low` (40 km → 40 m over the Grote Markt in 9 s) on
`mac-2` (M5 Pro VM), web-release, warm caches, two runs each, through a loopback
stand-in for 3dbag.cartopolis.org that served atlas's output for `/ground/` and
redirected the rest (`NO_GROUND=1` for the other half):

| after arrival | client cuts | atlas pre-cut |
|---|---|---|
| ground in view complete | 4.29 / 4.21 s | 2.35 / 2.39 s |
| everything quiet | 13.5 / 13.9 s | 7.28 / 7.44 s |
| worker time | 36 / 38 s | 18 / 17 s |

All nine cells logged `sheets(atlas)` (the stage takes 0–1 ms against 1–2.6 s).
The settled pictures differed by mean 0.09 % — between the two client-cut runs'
0.06 % and the two pre-cut runs' 0.13 % — and only at walkers and birds. Atlas:
25 cells in 29 s on the same machine.
