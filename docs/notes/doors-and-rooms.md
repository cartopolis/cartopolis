---
topic: buildings
triggers: [interiors.rs, doors, entrance, verblijfsobject, Footprint, with_doors, nearest_edge_point, hollow]
updated: 2026-08-29
---

# Doors, and what is behind them

A national survey gives a building's shell: correct outside, solid, and sealed.
Three things had to arrive before you could walk into one — where the door is, an
opening in the collider, and something on the other side.

## Where the door is: the register already knew

The BAG registers every addressable unit with its own **point**, at or near the
unit's entrance. That is the closest thing to a national door-position dataset
that exists, and it was already being streamed for the address readout.

What it needed was to arrive in the *right file*. The addresses live in the heavy
per-cell places file (2 MB) that only the readout reads; the mesh lane must not
pull that to learn where a doorway is. So each unit's point is also pushed onto
its **building's** record, in the small file `lod22` already fetches. One point is
a dozen bytes; the file it rides in is the one that was 0.5 MB.

## The outline, which fixed an older approximation too

A 3DBAG building arrives as a triangle soup, and the only footprint recoverable
from one is its **convex hull**. That is what the collider has always been —
right for a rectangle, and wrong for every L-shaped building, whose inner corner
it fills in with a wall nobody can walk through and nothing is drawn on.

The BAG holds the real ring, so the BAG extractor now ships it, simplified at
10 cm (about 11 vertices down to 9 — these rings were never the dense ones). The
hull is the fallback for a building the register has no outline for. The same
outline then says where a wall *is*, which is what a door has to stand in and what
a room is built from.

## Two bugs of the same shape, and the lesson

**Every door in the city was drawn inside its own wall.** The outward normal was
derived from the ring's signed area — the tidy answer, and the wrong way round in
a frame where +z runs south. The failure is worth remembering because of how it
presented: the door count read **113,710** and the street showed none. A capture
cannot tell "there are no doors" from "I am not looking at one", so the first
hour went on camera angles.

The fix is not a corrected sign. It is to stop deriving the answer and **ask**:
step 5 cm off the wall and test whether that point is inside the ring. That is
true on any winding and any shape, where a rule reasoned about one winding is
true only on that one.

The room walls had the identical assumption, and would have failed the identical
silent way — room built, metric counting it, and standing inside you see straight
out through the walls. `inward_normal` probes for the same reason, and both are
pinned by a test that runs the ring both ways round.

## The room

`systems::interiors` is a **near-camera layer**, and that is what makes it
affordable. A room is only ever seen from inside it or through its own doorway, so
there is no reason to hold one for a city: rooms stand for the buildings within
70 m and are rebuilt when the avatar has moved 15 m. Fifty buildings at a few
dozen triangles each is ~1,500 triangles standing at once, against the ~240,000 a
single LoD2.2 cell carries.

Three things are load-bearing:

- **One storey, not the full height.** The register says how many units a building
  holds, never where its floors are. A room is the ground floor at a nominal
  ceiling; above that ceiling is the shell, unchanged. A full-height empty volume
  would be a sixteen-metre shaft with a front door, which is not a thing.
- **The walls are inset** from the outline, so they stand inside the shell's own
  walls rather than in the same plane, where the two would z-fight the length of
  every room. The inset is an *edge* offset with a corner join, not a vertex push
  along the bisector — pushing vertices collapses a narrow canal house into a bow
  tie.
- **The doorway is cut out of the room's wall at the same points the collider
  opens at.** They have to be the same points: a room whose wall crossed its own
  door would seal the avatar in behind a surface they had just walked through.

Nothing here is double-sided. Walls face in, floor up, ceiling down — so from
outside a room is invisible and costs no fill, which is its state for all but a
few seconds of a session.

## What a door does *not* do

- **It is not cut into the wall.** The panel stands 4 cm proud of the brick: the
  shell arrives as a finished triangle soup and a hole would mean re-triangulating
  around it. A panel cannot leave a crack, and reads as a doorway at any distance
  this is visible from.
- **Its height is a constraint, not a taste.** `building_windows.wgsl` paints no
  window below `FIRST_SILL_M` (2.2 m), so a 2.1 m door sits in the one band of the
  facade the window shader leaves alone. A taller door would come out with panes
  across it.
- **The collider's opening is generous** (1.1 m either side of the register's
  point). That point is the *unit's* position rather than the doorway's, so it
  sits near the entrance rather than exactly in it; a tight gap would be a door
  you have to hunt for.

## LoD3 needs none of this

An imported IFC model brings its own doors: `IfcDoor` is imported and
`lod3::is_solid_class` excludes it from collision, so a modelled doorway is
already open. All of the above is for the shells.
