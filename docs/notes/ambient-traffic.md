---
topic: ambient-traffic
triggers: [traffic.rs, "ambient traffic", "cars driving", "moving cars", "traffic",
  DriveNetwork, drive_traffic, oneway_reverse, "streets layer", data_layers.rs]
updated: 2026-08-26
---

# Cars that drive, off the streets layer we already parse

`cartopolis_geo::traffic` + `systems::traffic` (2026-08-26). The first thing in
this tree that **moves on its own**: everything else streamed from the map is
placed and then stands still, and a city of stationary objects is most of what
makes a streamed map read as a model rather than a place. (Live transit vehicles
move too, but they are a real feed of real buses and only exist where OVapi has
one.)

It costs no request and no extra decode — the drivable centrelines come off the
same `streets` layer the road ribbons, the routing graph and the street labels
are already built from. Sibling notes: `docs/notes/parked-vehicles.md` and
`docs/notes/lit-windows.md`.

## `oneway` and `oneway_reverse` are different answers

Shortbread carries both. `vector_tiles::street_style` **ORs them together**, and
is right to: a painted road is the same width whichever way it runs. Traffic
cannot do that — the two send a car in opposite directions, and collapsing them
would run every `oneway_reverse` street's traffic backwards. `traffic::flow_from`
is a pure three-way function with a test on it for exactly this reason.

## Nothing may be built per frame

The documented device-loss failure in this tree is mass mesh churn
(`docs/notes/upload-bursts.md`), and a moving object is the obvious way to
reintroduce it. So:

- There is exactly **one** car mesh, built once at startup from the same
  `furniture::form::CAR` parts the parked cars use — a car waiting at a kerb and
  a car driving past it are the same object.
- A car is an entity with a `Transform`. Driving it writes that transform and
  nothing else. **After startup this layer uploads nothing, ever.**
- The pool is **fixed** and cars are **recycled**, never spawned or despawned on
  the move. A car that runs out of range is given a new road, not deleted.

`systems::grass` reached the same shape from the same constraint and states it as
a rule. The one thing that differs: a shared mesh cannot carry a per-instance
tint, so the paints are a handful of shared materials rather than vertex colours —
which is also what lets bevy batch 180 cars into a few draws.

## Two bugs, both silent, both found by looking at a capture

- **The pool was never created.** `setup_traffic` parents the cars to the
  `BigSpace` root, which `free_camera::setup_camera` spawns — and it was
  registered in the same `Startup` tuple with no ordering, so it ran first, found
  no root and returned. No cars, no error, nothing in the log. It is
  `.after(setup_camera)` now, as `setup_sky_dome` in that same tuple already was,
  and the unreachable arm **logs a warning** instead of returning in silence.
- **One car in view.** Placing uniformly over every candidate road spreads the
  pool across every service alley and back lane in the ring. The count went
  96 → 180 and placement now takes three draws and keeps the fastest road —
  `drive_speed` is already this codebase's encoding of road importance, so it is
  the natural weight.

## What this deliberately is not

No car-to-car collision, no obeying the traffic signals that already cycle
(`systems::street_detail`), no give-way, no queueing, no avatar interaction.
Every one is a simulation cost landing on every frame, and none is what the layer
is for: the question it answers is "is this street alive", settled at a glance
from a range where two cars overlapping for a moment is invisible. Recorded here
rather than hidden — a car driving through a red light is a known limit, not a
surprise.

## Its own toggle

**Map ▸ Traffic**, not the street-furniture one, because it is the only layer
here that moves: the first thing somebody who finds the motion distracting will
look for, and the first thing to switch off when judging anything else in a
still. It rides `Budget::detail_layers`, so a phone starts without it.

`LayerPrefs::traffic` uses the same `serde(default)` helper as its neighbours, so
a `user.json` written before this existed reads as the tier's default rather than
as off — an absent key is "not stated", not "the user turned it off".

## Verified, and what is not

Verified from stills over Groningen: cars appear, sit on the carriageway, face
along it, keep to the right-hand lane, and carry the parked cars' paint palette.

**Not verified**: that they drive *smoothly*, that the junction hand-off is
seamless, or that nothing pops when a car is recycled. A still cannot show any of
it, and this container has no way to watch a moving frame. The hand-off logic
excludes the road just left — without that a car at a dead end turns round and
drives back down it forever — but that path has been reasoned about, not watched.
