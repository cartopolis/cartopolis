---
topic: building overrides — a building's outside replaced by a model
triggers: [overrides.rs, models.rs, "pipeline/models", "lod3/coverage.json", "/api/pipeline/overrides", Euroborg, stadium, landmark, "building model", "generated model", building_override, no_interior, dsm_05m]
updated: 2026-10-06
---

# Building overrides

A building's surveyed shell can be replaced by a model: an uploaded GLB, or a
model atlas generates (the Euroborg's stadium). It is set **per building**, by
the BAG identificatie the shell already carries, and **deleting it brings the
survey's building back** — nothing about that building was changed, the client
just stops skipping it.

## Where it lives, and why there

A folder per building in atlas's data directory (`crates/atlas/src/overrides.rs`):

```text
overrides/<pand>/override.json   name, source, placement, collision, interior, author, revision
overrides/<pand>/model.glb       the uploaded model, when the source is one
overrides/<pand>/compiled.json   what it compiles to, cached
overrides/<pand>/history/<rev>/  every earlier override.json and model.glb
overrides/.deleted/<pand>.<ms>/  a deleted override, kept
```

Considered and rejected: a table in the simulator (the sim draws no buildings
and must not grow a second copy of the map's data), the published `lod3/` tree
itself (that is output; an operator's input mixed into it is lost on the next
full run), and a git repository of models (nobody uploading a building should
need one). The folder is the source of truth, the served `lod3/14/{x}/{y}.json`
cells are compiled from it, and the `models` dataset recompiles every override
on a full run.

The Euroborg was a hard-coded list until 2026-10-06. It is now seeded once into
that folder (marker file `overrides/.seeded`), so deleting it in the web UI
sticks.

## The routes and the permission

`/api/pipeline/overrides` (list), `/overrides/{pand}` (GET, PUT an edit, DELETE),
`/overrides/{pand}/model` (POST the GLB as the raw body). All of them need the
`building_override` permission (`docs/notes/permissions.md`). The web UI's
**Overrides** tab is the front end for them.

A save recompiles that one building and rewrites only its cell, plus the
manifest. Two writes happen in the same request, so a save is serialised by a
lock in the store.

## How a viewer sees the change: stamps

`lod3/coverage.json` lists every cell as `[x, y, stamp]`, where the stamp only
ever goes up (`crate::stamped`, which the floor-plan designs use too). The client
polls the manifest every 30 s (`systems::lod3`). It puts the stamp in the cell's
cache key and URL, and folds it into `lod22`'s per-cell rebuild revision
(`live_rev`). So an edit refetches one cell and rebuilds one cell, and a cell
fetched once stays good under its stamp. A manifest from before stamps (`[x, y]`)
reads as stamp 0.

The manifest also lists `no_interior` — overrides set to `interior: none`. For
those, `interiors` generates no rooms, because a stadium has a pitch where the
rooms would go.

## What a model may be

A single self-contained **GLB**, under the same byte, triangle and node limits as
the simulator's model store (`cartopolis_core::model_limits`). It uses glTF's
axes: +Y up, −Z the front, one unit a metre.

Placement:
- By default the footprint's centre goes on the centre of the register outline,
  and the model's lowest point goes on the ground.
- Heading (front to north at 0°), scale, lift and an explicit lat/lng override
  those defaults.

Each primitive's base colour becomes the part's `rgb`, and textures are dropped.
The served format is triangles with one colour per part.

Imported models get no painted windows (`building_windows::WINDOWLESS_UV`).

Collision options:
- `parts`: every part gets a hull.
- `walls`: only parts named wall, column or pillar get a hull, so doorways stay
  open.
- `none`: nothing gets a hull.

## Two things that are load-bearing

- **A model must be filed in the cell that draws its shell.** The client skips a
  shell only when the lod3 file *of the cell drawing it* lists the pand. A cell
  draws a building when the building's first vertex is inside that cell. The
  Euroborg sits on the boundary between z14 cells 8491 and 8492; filed by its
  centre, it would be drawn inside the block it replaces. `models::owner_cell`
  reads the published lod22 cells to find the right one, so a building with no
  published lod22 cell is filed by its outline instead (`cell_found: false` in
  the save's summary).
- **A stand is one solid element per slice.** The client gives every `IfcWall`
  element a convex-hull collider from below ground to its top, so a single bowl
  element would hull to a disc over the pitch. Slices keep the pitch open and the
  gates walkable. The roof is `IfcRoof`, which gets no collider.

## The Euroborg, measured

3DBAG draws the Euroborg (BAG `0014100010953806`) as one flat-topped block 23 m
high over 34,323 m². The register's outline has no hole for the pitch.

Measured on 2026-10-05:
- **The bowl**, from AHN's half-metre surface model (`dsm_05m`), with one section
  every 2.5° from the centre spot.
  - The grass is at 0.90 m NAP.
  - The roof starts about 10 m past the pitch, 25 m above the grass, and falls to
    19–21 m at the outer wall.
- **The footprint**, from the BAG outline.
- **The pitch**, from OSM way 185043967 (106.5 × 68 m).
  - It is striped, with the FIFA markings.
  - The pitch corners are part of the override (`Source::Stadium`), so they can
    be re-aimed without a code change.

Assumed, because the laser cannot see under a roof (each is a named constant in
`models.rs`):
- a 7 m apron;
- 0.85 m rows;
- 3 m headroom under the roof;
- four 8 m gates at the corners.

The 6 m ring outside the north, east and south walls belongs to other buildings,
which 3DBAG keeps drawing.

The server's physics does not see any of this, because it simulates only prims.
So the football pack's ball is kept on the pitch by prims.
