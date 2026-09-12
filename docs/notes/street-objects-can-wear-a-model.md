---
topic: content
triggers: [placeholder_models.rs, PLACEHOLDER_KINDS, FurnitureMask, "street objects", lamppost, "placeholder model", DetailWants, "V18__placeholder_models", "/api/library/placeholders"]
updated: 2026-09-11
---

# Replacing the boxes a street is made of

`cartopolis_geo::furniture` draws a lamppost as a column, a bracket and a head —
six boxes, merged with every other object in the cell into one mesh with one
material. That is what makes 651 lampposts on one Groningen page affordable, and
it is why they look like six boxes.

Nobody placed any of them. They come from the BGT, from a city's own register,
from Shortbread's `pois` layer, so there is nothing in the world to select and
improve, and the whole apparatus that lets a person put a model on a prim —
inventory, selection, the edit dialog — cannot reach them.

So the statement is made once, about the world:

```
PUT    /api/admin/library/placeholders/lamp   {"asset": "<hash>", "scale": 1.0, "yaw_offset": 0}
DELETE /api/admin/library/placeholders/lamp
GET    /api/library/placeholders
```

and every client draws that model at every lamppost.

## World-wide, not per player

A per-user override would be a preference, and two people standing on the same
street disagreeing about whether there is a bench there is not a preference — it
is two different worlds, in a simulator whose whole job is that there is one. It
would also need the client to hold models the server has never heard of, which
is a second asset store with no moderation on it.

The cost of the choice is that an operator is the only one who can do it, and
that is the intended shape: it is the same class of act as curating the library.

## The procedural form is skipped on the worker, not hidden afterwards

The set of replaced forms is a `FurnitureMask` — a `u16`, one bit per
`FurnitureKind` — and it rides in `map_geometry::DetailWants`, which is compared
every frame and retears every live cell when it changes. Three things follow, and
each is the reason it is there rather than somewhere more obvious:

- An overridden lamppost is **never meshed**. Hiding it after the fact would pay
  the extraction, the merge and the upload for geometry nobody sees, which is the
  exact mistake `DetailWants` was written to stop (see its own note: turning a
  layer off used to save the draw call and nothing else).
- Changing the table **rebuilds the streets**, for free, through machinery that
  already existed for the layer toggles.
- `DetailWants` is `Copy` and goes to a worker, so the set cannot be a `HashSet`.

`build_furniture_meshes_except` is the filter, and it allocates nothing when the
mask is empty — which is every server that has set no placeholders, i.e. all of
them until somebody does.

## Standing them up is bounded per frame

A dense cell is hundreds of instances and a teleport lands several cells at once.
`MAX_SPAWNS_PER_FRAME` is 64; the rest queue. This is the
[upload-bursts](upload-bursts.md) rule applied to *entities* rather than bytes:
the meshes behind a placeholder are uploaded once, for the first instance, and
what the other 650 cost is spawn work on the frame loop.

The queue is keyed by cell and dropped with it (`forget_cell`, `clear_queue`),
because a cell that arrives and leaves while the queue is long would otherwise
stand its objects up afterwards, into a cell that no longer exists.

## What it does not change

**The collider and the prompt are unchanged.** `WorldPlaces` still receives every
instance, so a bench is still solid and still offers to be sat on whether it is
six boxes or somebody's model. A placeholder is a statement about how a thing is
*drawn*; a model is not consulted for its shape, which is the same deal a prim
wearing one gets (it is picked against a unit box).

**No per-instance variation.** The forms carry a `luma` and a `paint` so a car
park is not one identical grey; a model brings its own materials and this has no
business tinting them.

**The vocabulary is closed.** `cartopolis_core::placeholders::PLACEHOLDER_KINDS`
is the list, `FurnitureKind::slug` maps onto it, and a test in `cartopolis_geo`
pins the two equal. The server refuses an unknown slug — the same rule the shot
manifest and `--expect` follow, and for the same reason: a setting that quietly
does nothing is worse than one that was refused. cartopy holds a third copy for
its dropdown, which makes this the **seventh** hand-synced seam between the two
repositories.

## Unverified

Nothing here has been seen on a screen. A scripted capture is offline and a
placeholder table comes from a session, so `--shot` cannot photograph one without
a local override the harness does not have; the geometry half —
`build_furniture_meshes_except`, the mask, the queue — is unit-tested, and the
spawn path is not. The obvious next step if this needs proving headlessly is a
`--placeholder kind=<file.glb>` flag reading a local file through the ordinary
`AssetServer`.
