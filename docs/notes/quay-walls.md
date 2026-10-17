---
topic: quay-walls
triggers: [quay wall, kademuur, canal edge, "the canal has no walls", "the street sags into the canal", land_at, sunk_at, bank_flags, quay_walls, QuayWall, water level, WET, BANK, systems/map/terrain.rs, systems/map/map_geometry.rs, water_levels.rs]
updated: 2026-09-26
---

# A canal is drawn as two surfaces and a wall, not one sheet

The register gives every Dutch canal a measured level ([rijkswaterstaat-water-levels.md](rijkswaterstaat-water-levels.md)),
and the terrain field is carved to it. What the field cannot hold is the *step*: it
is sampled every 3.125 m on a desktop and 12.5 m on a handset, so between the last
sample on the street and the first in the canal bilinear interpolation draws a
ramp that wide. Everything read off the field sagged into it — the street ran
downhill into the water, and the water, clamped above the basemap, climbed up to
meet it. On a handset every inner canal was a smeared slope with no edge
(reported 2026-09-25: "the inner canals don't have walls").

## Three readings of one field

`terrain::bank_flags` marks each sample **WET** (inside a registered ring — the
strict test the carve itself uses) or **BANK** (within one spacing of a wet one).
From that, `TerrainSnapshot` answers three questions:

| reading | used by | at a quay |
|---|---|---|
| `at` | everything else (buildings, avatar, trees, tunnels, decks) | the carved field, as before |
| `land_at` | every ground surface in `map_geometry::stand_on_terrain` | wet and bank samples replaced by the mean of the dry ones within two spacings: the street stays level to the edge |
| `sunk_at` | the water sheet, the basemap (`floor_at` and its envelopes) | bank samples sunk to the canal beside them: the canal side reaches the bed at the edge |

`map_geometry::quay_walls` then stands a vertical face along every outline edge of
the placed water, from `QUAY_WALL_FOOT_M` under the surface to `land_at`.

**One spacing of bank, not zero**, because a graded field is carved on atlas's
lattice and resampled onto this one — the first sample outside a ring can carry
part of the carve, and on a handset's thinned tiles that is a whole atlas sample.

**`sunk_at` feeds the basemap for a reason that is easy to undo by accident.** The
water must stay above the basemap under it, or the opaque clipmap hides it. The
finest basemap level reads the field exactly, so with the water at its registered
level and the basemap on `at`, the basemap's ramp out of the carve would poke up
through the water along every bank. Both read `sunk_at`, so both reach the bed at
the same place. `sunk_at` is never above `at`, so the lower-envelope guarantee in
`build_floors` still holds.

## Where there is no wall

An outline edge gets nothing when it lies on the tile border (the canal carries on
in the next cell), when there is more of the cell's water just outside it (two
water polygons meeting), or when the street is within `QUAY_WALL_MIN_M` (0.3 m) of
the water — a ditch in a field, a lake shore, or a canal too narrow for the field
to have caught a sample in it. That last case is the handset's for most of the
Diepenring's narrow arms: no sample lands in the canal, the water rises to the
ground as it always did, and there is no step to close. It degrades to the old
picture rather than to a broken one.

## Not done

`at` is unchanged for everything that is not a ground surface, so a building or a
tree standing within one spacing of a canal still reads the carved field. The
surveyed walls *on* the quay (`walls-have-a-measured-height.md`) are a separate
layer and stand on `at` too.
