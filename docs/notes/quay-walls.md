---
topic: quay-walls
triggers: [quay wall, kademuur, canal edge, "the canal has no walls", "the street sags into the canal", lower quay, Terrace, terraces, lower_quay, parapet, TERRACE_APRON_M, wet_the_aprons, Ramp, ramps, QUAY_BRICK, pipeline/lower_quay.rs, land_at, sunk_at, bank_flags, quay_walls, QuayWall, water level, WET, BANK, systems/map/terrain.rs, systems/map/map_geometry.rs, water_levels.rs, Banks, Banked, "oever", ondersteunendwaterdeel, "sloping bank", pond, vijver]
updated: 2026-11-22
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

## A lower quay is measured, not resolved (2026-11-05)

Gorechtkade by the Linnaeusplein in Groningen has a walk along the water at the
foot of the retaining wall — benches, a jetty, steps up at the end — and the map
drew it as pavement at the top of the wall (photographed beside the app's view
from the same pose, session s609). AHN at 0.5 m reads the walk at −0.85 m NAP
and the street behind the wall at +0.7 to +1.0; the walk is 1.8 m wide on
average (`wegdeel` `G0014.404f925c4b580015e053660013ac6945`, 111 m²). Every
reading above got it wrong in the same way: the walk's samples are BANK, so
`land_at` replaced them with the street, and at 3.1 m between samples no field
could have held a step two metres from the water anyway. Atlas's graded tile
does keep part of the dip (−0.3 to −0.6 m on the few samples that land on the
walk) — the ground was not the problem, the resolution was.

So the walk's height travels as data instead. Atlas measures every paved surface
that runs along a quay wall (`pipeline/lower_quay.rs`: within 0.5 m of a
`kademuur`/`damwand`/`walbescherming` outline) against AHN: one flat level
inside it, and ground at least 0.8 m above that level just outside part of its
outline, is a lower quay, and the paving file carries `terraces` — the surface's
rings and that level. A key of its own, so an older client still reads the file.
The client lays every flat triangle whose centroid is on a terrace at its level,
with the vertices on its edge split from the ground beside it, so the wall's
step is a step (`map_geometry::split_terraces`); the ground sheets already cut
along the walk's outline because it is surveyed paving. Two walls follow from it:

* `quay_walls` stops a canal wall at the walk, not the street, where a terrace
  is on the edge or a metre out of the water — the map's water and the
  register's walk do not share an outline;
* a surveyed quay line with a terrace on one side and water on the other holds
  up the walk, so its top drops to the walk (`build_line_meshes_footed`'s
  `drop`); the one under the street keeps its top at the street.

**The map's water stops short of the register's walk.** OSM's water polygon
ends one to four metres off the walk's water edge, and the strip between was
ground at the street's level: a ledge along the water with a canal wall in
front of it, seen from the water as a brown face up to the street. Laid at the
walk's level it was still ground — a grey quay where the aerial shows the canal
reaching the walk ("I don't see the water"). So atlas marks which of a walk's
edges face the water (no AHN ground 1 m out, or only a margin no higher than
the walk with none 2.5 m out), and the client moves every flat triangle within
`TERRACE_APRON_M` (4 m) of such an edge, and nearest to it, into a water mesh
of its own (`wet_the_aprons`). Two rules here were learnt the hard way: not on
the walk's level alone, or the landing at the walk's end by the steps counts as
water and sank the street beside the steps into a pit; and the *nearest* edge,
not any in reach, because the walk is narrower than the reach and the street
behind the wall stood up as a slope of "water" against it.

**The steps are a slope.** The flights at either end are surveyed paving
(`voetpad op trap`) and were drawn flat at the street's level. Atlas measures a
flight that adjoins a walk (10th and 90th percentile of AHN inside it: −0.67 →
+0.78 and −0.50 → +0.82 m NAP here) and ships it as `ramps`; the client lays it
from its foot at the walk's outline to its top at the flight's far end
(`Ramp`). A slope, not treads — the register has no risers.

**The walls are brick** (`palette::QUAY_BRICK`): the register gives no material,
and grey read as concrete beside photographs of a red brick wall.

**The wall stands above the street.** AHN's surface model puts its top at +1.2
to +1.9 m NAP over a street at +0.8 — on the walk, above eye height, which is
what the photographs show and the first fix did not: the register's 3D height
(1.62 m) is the face below the street. Atlas measures the part above the street
along the wall's centreline (median of the highest return within 0.4 m every
half metre, so a tree over the wall does not count; 0.38 m here, 0.71 m on its
length by the steps) and ships it as a line of its own, `parapet` (appended to
`LINE_KINDS`, so an older client drops it), stood on the ground at its line
and never footed. Only walls along a lower quay are measured.

Not done: `TerrainSnapshot::at` still reads the field, so the avatar, a tree or
an OSM bench placed on a lower quay stand on the field (between the walk and the
street) and the shot harness's `on_ground` counts from it too. The surveyed
paving is binned by its first vertex and dropped by a cell it leaves, and so is
a terrace.

## A pond has a slope, not a wall (2026-11-22)

Everything above assumed every edge of the water is a quay. The Gorechtvijvers
by the Linnaeusplein are ponds in a park: AHN reads the lawn at +0.75 m NAP,
falling over 8–10 m to the water at −0.85 m on both long sides, roughly 1 : 6,
and the BGT surveys no `kademuur`, `damwand` or `walbescherming` anywhere along
them. The map drew them as canals — the lawn level to the edge (`land_at`), a
brick wall 1.6 m down to the water, and boats moored along it — reported from
the phone with "the slope and the grass are wrong and there are no boats in
that water".

What tells a pond from a canal is the register, not the tile. The shortbread
tile calls both `kind: water`, and the half-width test the moorings use passes a
25 m pond as a canal. The BGT draws the water to the waterline (`waterdeel`
`watervlakte`, `vijver` here) and the grass between it and the top of the slope
as its own object: `ondersteunendwaterdeel` `oever, slootkant`, ~9.5 m wide on
each side of these ponds. The inner city's canals have `kademuur` and no
`oever`; the two did not overlap in the three places checked.

So atlas ships each `oever` as a `banks` entry of the paving file (rings only,
simplified to 0.5 m — a rural square kilometre has some 130 of them, one per
ditch side, 11 000 vertices before simplifying), written to every cell it
touches. The client (`map_geometry::Banks`) gives each outline edge that has
registered water just outside it that water's level, and lays any ground point
on a bank at `water + (land − water) · d_water / (d_water + d_top)`; an edge
shared with the next bank is neither, so two banks meeting across a slope make
no ridge. The strip between the register's waterline and the map's water, out
to `BANK_APRON_M`, is the water's level, but only straight out from a waterline
edge — never round its end, the trap the lower quay's apron fell into. It is a
`GroundField` wrapper (`Banked`), so the refinement sees the slope and gives it
vertices even on a handset's 12.5 m field. It is never drawn under the basemap
(`sunk_at`). `quay_walls` tops a wall on a bank at the slope where the map's
water stops, a lip of centimetres (`BANK_LIP_M`), and the moorings skip any
point within 3 m of a bank (`canal-moorings.md`).

A bank with no registered water level in reach is left as before: there is no
level for its foot. Not done: `TerrainSnapshot::at` (the avatar, trees, the
shot harness's `on_ground`) still reads the field on a bank, not the slope.
