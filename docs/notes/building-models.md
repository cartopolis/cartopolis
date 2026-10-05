---
topic: building models for landmarks
triggers: [models.rs, "pipeline/models", "lod3/coverage.json", Euroborg, stadium, landmark, LANDMARKS, "building model", "generated model", dsm_05m]
updated: 2026-10-05
---

# Generated building models for landmarks

Some buildings come out of 3DBAG in a shape that is wrong in a way that matters.
The Euroborg (FC Groningen's stadium, BAG `0014100010953806`) is the first: the
register's outline is one 16-point polygon with no hole for the pitch, and the
LoD2.2 reconstruction fills it — one flat-topped block 23 m high over 34,323 m²
(3DBAG's own `b3_volume_lod22` is 782,706 m³, which only adds up with the pitch
filled in). The football pack's ball would be inside it.

atlas's `models` dataset (`crates/atlas/src/pipeline/models.rs`) writes a model
into the `lod3` tree, which the client already streams in place of a shell
(`systems::lod3`, `docs/notes/importing-a-building-model.md`). Nothing changed in
the client.

## What is measured and what is assumed

Measured, 2026-10-05:

- **The bowl, from AHN's half-metre surface model** (`dsm_05m`), one section
  every 2.5° from the centre spot. Grass at 0.90 m NAP. The roof starts ~10 m
  past the pitch (62 m from the centre behind the goals, 44.5 m beside the
  touchlines) at 25 m above the grass, and falls to 19–21 m at the outer wall.
  The west (main) stand runs out to 112–117 m, the east to 84 m.
- **The footprint**, from the BAG outline (PDOK WFS): the rays stop there.
- **The pitch**, from OpenStreetMap way 185043967 (106.5 × 68 m).

Assumed, because the laser cannot see under a roof — every one is a named
constant at the top of the module: a 7 m apron, a 1.2 m front wall, 0.85 m
rows, 3 m headroom under the roof, a 3 m concourse behind the back row, and four
8 m gates at the corners so an avatar can walk in (the real stadium has stairs
there). The 6 m ring the surface model shows outside the north, east and south
walls is **not** in the model: it lies outside this building's outline, i.e. it
is other buildings, which 3DBAG keeps drawing.

## Two things that are load-bearing

- **A model must be filed in the cell that draws its shell.** The client skips a
  shell only when the lod3 file *of the cell drawing it* lists the pand, and a
  cell draws a building when its first vertex is inside it. The Euroborg sits on
  the boundary between z14 cells 8491 and 8492; filed by its centre it would be
  drawn inside the block it replaces. `owner_cell` reads the published lod22
  cells to find the right one, so the dataset runs after `lod22`.
- **The stand is one solid element per slice.** The client gives every
  `IfcWall` element a convex-hull collider from below ground to its top; a
  single bowl element would hull to a disc over the pitch. Slices keep the pitch
  open and the gates walkable. The roof is `IfcRoof`, which gets no collider.

## Limits

The architecture is not modelled — the Euroborg's facade cladding, the tiered
main stand, colour (the client paints a model with the building's dated wall
and roof palette). The server's physics does not see any of it (it simulates
only prims), so the football pack's ball is kept on the pitch by prims.
