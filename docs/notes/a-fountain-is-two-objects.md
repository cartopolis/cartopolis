---
topic: map
triggers: [furniture.rs, FurnitureKind, over_water, FOUNTAIN_JET, world_places.rs, "amenity=fountain", placeholder models, "why is there a basin in the pond"]
updated: 2026-09-16
---

# `amenity=fountain` is two different objects, and the water tells them apart

One surveyed kind, two things on the ground: a stone basin standing on a square,
and a jet in the middle of a pond. The register cannot distinguish them, because
from directly above they are the same point with the same tag — and this client
drew the basin for both.

What that looked like at the study pond in Groningen: a 3 m ring of masonry
with a 0.4 m rim, laid at the waterline of the pond, with a solid collider and a
proximity prompt offering to walk up to it, about 40 m out into the water. The
fountain the user was looking for **was** being drawn, from data we already had
in two independent sources — OSM's `pois` layer and the municipality's own
register, which even supplies the name "Fontein <square>". It was drawn as
the wrong object.

## Why a flag and not a second `FurnitureKind`

`FurnitureInstance::over_water` is set from the tile's own water polygons in
`extract_furniture` and `place_surveyed`, and read by `parts_for` and by the
client's collider pass.

A second kind was the obvious shape and is wrong three times over. The vocabulary
is **closed**, pinned by a test, hand-synced with the atlas console's dropdown, and — the
one that decides it — reachable by an operator through
`FurnitureKind::slug`: somebody who uploads a fountain model and hangs it on the
`fountain` slug means *the fountains*, not the ones that happen to be on paving.
A jet and a basin should stay one kind to all three of those, and differ only in
the form they are drawn as.

## The test is exact and is asked of one kind only

An even-odd point-in-polygon walk over the tile's own `WATER_LAYERS`, holes
counted as land (a pond with an island in it has ground on the island).

`vector_tiles::water_shore_field` answers a richer question — distance to the
bank — and is the right instrument for shading a canal, at 1056² per cell. It is
the wrong shape here: a cell holds two or three fountains and several hundred
benches, so the question is asked per *object*, and only of the kind whose answer
changes anything. A bench does not become a different bench by standing near
water.

## Both halves of the prop treatment come off, not just the form

A collider out in open water is an invisible wall in the middle of a pond, and a
prompt offers to walk up to something nobody can reach. The bank is what stops
you, and the bank is drawn. `world_places::insert_cell` gives an afloat fountain
radius 0 and no `Interactable`.

`moorings.rs` deliberately leaves `over_water` false on its boats even though a
moored boat is on water by definition: the flag drives the fountain's form and
the collider pass, and a hull is meant to stop you.

## An operator's model stops at the water's edge

`fountain` is already in `cartopolis_core::placeholders`, so an operator can hang
an uploaded glTF on it with `PUT /api/admin/library/placeholders/fountain` and no
code changes at all. `FurnitureInstance::takes_placeholder_model` is what stops
that putting the basin back in the pond: a model on that slug is a thing with a
base, placed with a yaw, and whoever uploaded it was picturing a square.

The second half matters as much as the first. A masked kind is left *unmeshed*
precisely because a model is going to stand where it was — so an instance that
refuses a model has to keep its own geometry, or setting one fountain model
deletes every jet in the country.

The cost, stated: there is no way to give the jets a model of their own. That is
the right way round — a missing feature beats a stone basin floating in a lake.

## What it still is not

A still column of water. Nothing in the furniture group animates — a cell is one
merged mesh with one shared material, which is what makes a city's worth of
street objects affordable at all. A plume that moves is `systems::particles`,
which is a formula rather than a simulation and would suit this well; hanging one
on a fountain is unbuilt.
