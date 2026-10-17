---
topic: buildings
triggers: [building_design.rs, building_designs.rs, designs.rs, "building design", "floor plan", "floor-plan editor", "/building/", buildings.rs, building_owners, "own a building", interiors.rs, IFC, BIM, lod22_facade.rs, cutaway.rs, crates/atlas/src/designs.rs, crates/atlas/web/src/components/pages/building, DesignToWorld, with_design_doors]
updated: 2026-10-17
---

# Building designs, and who may draw one

A building's inside is a **design**: storeys, walls carrying their doors and
windows, named rooms and straight flights, in metres east and north of an origin
(`cartopolis_geo::building_design`). Every building has one. A building somebody
owns and has drawn uses the design they saved; every other building gets the one
`generate` lays out from the BAG outline, its entrances and the shell's height,
which is the interior the client used to build by hand. One compiler
(`BuildingDesign::compile`) turns either into the mesh, the floors and stair
ramps the avatar stands on, and the inner walls it is stopped by — so a drawn
building cannot behave differently from a generated one. A flight's well is cut
out of the slab by polygon difference, so a drawn flight may stand anywhere; the
generated one still stands against a wall ([doors-and-rooms.md](doors-and-rooms.md)).

**The design's frame is a mirror of the world's.** A design is metres east and
north; the world runs +z south. `interiors::DesignToWorld` carries a design
across and, being a reflection, reverses every triangle's winding on the way —
miss that and every floor is lit from beneath.

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

**A designed building's entrances come from its design**
(`building_designs::with_design_doors`), not from the register's points, so the
facade door, the collider opening and the room's doorway move together when an
owner moves a door. `lod22` bakes them into its facade and collider, so it
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

## Seeing in: the cutaway, and why the roof has to go

Inside a building the roof comes off with the walls and everything above the
avatar's storey stops being drawn (`systems::cutaway`, `compile_cut`). Removing
only the walls — what `building_select` did first — left the follow camera,
which is always above and behind the avatar, looking at the underside of the
roof; culling the ceiling as well only moved the problem to the floor of the
storey above. Four captures over Prinsenstraat 24–26 on 2026-10-05 showed the
avatar inside and nothing legible. What is cut is the mesh alone: floors, stairs
and walls stay whole, so an avatar upstairs does not fall because somebody is
looking at the ground floor.

The building card's Look inside does the same from outside, and the floor picker
lists the storeys by the names in the design, so the game and the editor use one
vocabulary: the picker says "In the Keuken" because the owner drew a room called
that, and its Floor plan button opens the same building in the editor.

## The facade is the design's outer walls

A saved design's outer walls are the building's facade (`lod22_facade`): the
surveyed walls are left out and drawn again along the register outline, up to
the height the survey measured along each edge (so a gable keeps its point and
meets the surveyed roof, which stays), with the design's doors and windows cut
through as real openings. The painted window lattice is switched off for those
walls — it guesses windows from the era, and a guess in a different place from
the window drawn would show the street a pane the room does not have. So the
editor's starting layout carries windows on its outer walls
(`with_facade_windows`): saved as-is, a design without them would turn the
building into a blank brick box.

## Authoring: draw walls, the rooms find themselves

The editor has no room tool. A room is whatever the walls enclose, found by
`lib/rooms` (split at crossings and T-junctions, then the planar face walk), and
a person names one by tapping it; a name follows its room when walls move.
Walls' ends and doors drag directly, an outer wall takes "space N windows along
it" and "same windows on the floors above" in one tap each, and the keyboard
has V/W/D/N/S, Esc, Delete and Ctrl+Z.

## What is not done

- **The massing.** A design changes the inside and the facade's openings; the
  building's shape and roof are still the survey's. Replacing those is the
  exterior-override work (an admin-only record in atlas, the LoD3 path), agreed
  with the session building it on 2026-10-05.
- **IFC import into a design**, and export back out.
- **Rooms are labels.** The game names the room you stand in; it does not tint
  or draw them.
- **Visitors.** Anybody may walk into any building; there is no private setting.
