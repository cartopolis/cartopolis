---
topic: buildings
triggers: [building_design.rs, building_designs.rs, designs.rs, "building design", "floor plan", "floor-plan editor", "/building/", buildings.rs, building_owners, "own a building", interiors.rs, IFC, BIM]
updated: 2026-10-05
---

# Building designs, and who may draw one

A building's inside is a **design**: storeys, walls carrying their doors and
windows, named rooms and straight flights, in metres east and north of an origin
(`cartopolis_geo::building_design`). Every building has one. A building somebody
owns and has drawn uses the design they saved; every other building gets the one
`generate` lays out from the BAG outline, its entrances and the shell's height,
which is the interior the client used to build by hand. One compiler turns either
into the mesh, the floors and stair ramps the avatar stands on, and the inner
walls it is stopped by — so a drawn building cannot behave differently from a
generated one.

## Where each half lives, and why it is split

| | where | why there |
|---|---|---|
| who owns a building | simulator, `building_owners` (V25) | ownership is an account fact, and the accounts are the simulator's |
| the design | atlas, `data_dir/designs/{pand}.json` + `history/` | it is map data the client streams beside the shells |
| what clients fetch | atlas's site tree, `designs/14/{x}/{y}.json` + `coverage.json` | served by Caddy like every other building tree |
| the editor | web UI, `/building/{pand}` | 2D precision work, and it ships on every push to `main` |

Atlas asks the simulator `GET /api/buildings/{pand}/access` before a save and
believes the answer, as it already did for "is this an admin". An admin may edit
any building.

**One building, one owner; one owner, several buildings** (`max_buildings_per_account`,
default 10). A block of flats therefore has one owner. The old rule was one
*address* per account; on 2026-10-05 the hosted server held one such claim.

## Why the structure follows IFC and the geometry does not

The document keeps IFC's spatial tree (storey → room, walls carrying openings) and
an id on every part, which is what lets a model be re-imported as an update and a
drawn design be exported. IFC's geometry is extrusions and boolean cuts and needs
a kernel to evaluate; a wall here is a line, a thickness and a height, which an
editor can draw and a phone can mesh. An IFC import (not built yet) belongs in
atlas, converting into this document.

Checked 2026-10-05: there is **no openly licensed IFC of any real Groningen
building**. The Schependomlaan model (Nijmegen, CC BY 4.0, 4 storeys, 100 spaces)
is the best real Dutch test file; its footprint matches Soendastraat 11–13.

## Freshness: the one building tree that changes while the app runs

Every other tree is rewritten by an extraction run. A design is somebody's work,
saved a minute ago. So the client polls `designs/coverage.json` every 30 s and the
manifest carries a **stamp per cell** that is part of the cell's cache key: a
cached cell is good for ever under its stamp, and an edit is a new stamp. The
stamp is written when the cell is, never derived from the designs in it — derived,
removing the newest design would move it *back* to a stamp a client may have
cached with that design still inside.

`lod22` bakes a designed building's entrances into its facade and collider, so it
rebuilds only the cells around a changed design (`neighbourhood_revision`), never
the ring.

## Two register traps the generator hit on the first real building

Prinsenstraat 24–26 (pand 0014100010938796) is where both were found:

- **An outline can contain another building.** This one sits wholly inside a
  365 m² outline from 2000. Counted as a neighbour it made every wall a party wall
  and the starting design lost both front doors. A neighbour containing the
  building's centre is not a party wall.
- **A point exactly on an edge is answered by rounding.** The flight's corners on
  its wall were nudged *along* the wall before the inside test, so the building
  got its stairs (and its upper storeys) or not depending on where the frame's
  origin was. They are nudged toward the flight's middle now.

## What is not done

- **The outside.** A drawn design changes the inside; the 3DBAG shell is still
  what the street sees. Replacing a shell needs the LoD3 path
  ([importing-a-building-model.md](importing-a-building-model.md)), whose
  floors and stairs are still not walkable.
- **IFC import into a design**, and export back out.
- **Rooms are labels.** Their names and areas show in the editor; the game does
  not draw or announce them yet.
- **The floor picker and cutaway in the game**, and an "Edit building" button on
  the building card.
- **Visitors.** Anybody may walk into any building; there is no private setting.
