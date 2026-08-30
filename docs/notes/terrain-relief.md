---
triggers:
  - working on systems/map/terrain.rs, or anything that places geometry on the ground
  - "why is the terrain flat here" / "why doesn't the relief go further out"
  - considering a different height source, resolution, or extent
  - adding a new thing that stands on the map (a marker, a vehicle, a mesh)
---

# Terrain relief: what AHN actually holds, and the shape the field takes from it

`systems::terrain` turns the client's flat `y = 0` plane into measured ground. The
numbers below are why it is shaped the way it is; the code says what it does.

## How much relief is there, really (measured 2026-07-31)

Method: PDOK's AHN WCS (`dtm_05m`, EPSG:28992 GeoTIFF) queried directly over a
**6.4 km box** — the field's own extent — at 128² (50 m/sample) and 256² (25 m).
Statistics over the finite samples only.

| Place (RD centre) | relief (max−min) | sd | coverage |
|---|---|---|---|
| Groningen centre (233500, 582000) | 37.9 m | 1.8 | 98 % |
| Groningen ring south (234000, 579500) | 21.7 m | 2.0 | 93 % |
| Dunes @ Bergen NH (105000, 517000) | 40.4 m | 7.2 | 79 % |
| Veluwe @ Posbank (197000, 449000) | **101.9 m** | 25.2 | 100 % |
| Vaals / Limburg (197500, 307500) | 182.2 m | 39.9 | **41 %** |
| Flevoland polder (165000, 500000) | 20.9 m | 1.2 | 99 % |

Three things follow, and all three are load-bearing:

* **The Netherlands is not flat, but its *cities* nearly are.** Groningen's standard
  deviation is 1.8 m over 6.4 km; its 38 m range is a handful of outliers (dredged
  pits, a landfill). So a capture over a Dutch city will look almost identical with
  the layer on and off — that is the ground, not a bug, and it is why the
  `terrain_relief_m` column exists in the `--shot` metrics. The Veluwe and the dunes
  are where the feature is visible, and where it should be checked.
* **50 m sampling loses almost nothing at this extent.** 128² and 256² over the same
  box agree to within a couple of metres everywhere except the noisy tails. 25 m was
  kept because it costs one 256 KB request either way, but there is no case for going
  finer at 6.4 km — a dike is 10 m wide and would need 2 m sampling, i.e. 3200², i.e.
  a 40 MB answer per anchor.
* **No-data is the normal case, not the error case.** Coverage is never 100 % in
  practice: the DTM has bridges and buildings *cut out of it* by construction, the
  sea has no measurement, and a box near a border is mostly another country (Vaals is
  41 % because it sits on the tripoint). A consumer that treats no-data as zero digs
  a pit under every viaduct.

## The vertical datum is the anchor, not NAP

Samples are stored relative to the ground under the map anchor. Absolute metres above
NAP was rejected: it would drop the whole world 322 m below the camera in Vaals and
lift it in the polder, for a picture that is *identical* — what the eye reads as
relief is the difference between here and there, never the offset both share. Keeping
the anchor at `y = 0` also means every existing placement in the client stays exactly
right at the middle of the map, which is where the user is.

## The field is finite, so it tapers rather than ends

6.4 km of measured ground sits inside a clipmap that runs to the horizon, and the
world outside AHN is flat by definition. The samples are therefore multiplied by a
smoothstep that reaches **exactly zero at the boundary**, baked in rather than applied
at lookup, so `at()` outside the box returns 0 and the two meet with no step.

The cost, stated plainly: a hillside crossing the boundary is flattened toward the
anchor's level over the last ~600 m. Alternatives considered and rejected:

* **No taper.** The measured edge meets flat ground at whatever height it happens to
  be — a cliff, and one that *moves with the camera* as the anchor re-snaps. Worse in
  every way.
* **A bigger box.** Delays the problem at linear cost in bytes and quadratic cost in
  bake time, and never removes it: the edge exists at any size.
* **Absolute heights out to AHN's own envelope.** That is a 300 km × 200 km field —
  three orders of magnitude more data than one anchor needs, to serve ground the
  camera cannot see.

## Why the whole thing is one request, baked once

A consumer asks for a height thousands of times per mesh build, so the field cannot be
a cache of per-question patches like `systems::height`'s. One `HeightRequest` per map
anchor (256², ~256 KB) is baked into a regular grid in the anchored world-XZ frame;
after that every lookup is bilinear arithmetic with no I/O.

Baking resamples through `world → lat/lng → RD` per grid point rather than mapping the
RD box onto the world box directly. RD is rotated against Web Mercator by the meridian
convergence (~1° over NL) and scaled slightly differently: the axis-aligned shortcut
puts the corners of a 6.4 km field ~80 m out, which is a hill in the wrong place.

## The field arrives *after* the anchor, so every consumer needs a revision

`at()` returns 0 while the request is in flight, which is the same answer it gives
outside the Netherlands and with the layer off — that uniformity is what lets a
consumer add the lift unconditionally. The price is that every streamer builds its
first cells on flat ground and must retear when the real field lands a second or
two later. `TerrainField::revision` is that signal, and a consumer holds the
revision it built against beside its cell keys.

**The failure is silent and one-sided**, which is why this is written down: the
consumers that *were* wired sink or rise together and look right, and the one that
was not stays where it was while the ground moves under it. Street labels
(2026-08-10) were exactly that — the road surfaces beneath them were displaced,
the names were not, and with the Height layer on the text sank into the road. A
new thing standing on the ground is not done until it carries the revision.

## The physics stays flat; only the picture stands on the ground

`simulator` is authoritative for the avatar and holds **no map data at all** — the
terrain streams only on the client, so the sim cannot know about it. Rather than teach
it (a protocol change, for data it has no way to obtain), the terrain is added on the
display side: `AvatarState::position` stays on the flat plane the sim simulates,
`display_position` stands on the measured ground, and the difference is
`terrain_lift`. The lift is stripped before each reconciliation — a lifted position
easing toward a flat target is dragged back into the hillside every frame, and the two
fight for as long as you stand on a slope.

Two consequences worth knowing:

* Building collision (`systems::collision`) is 2D footprints in world XZ and is
  unaffected — correctly, since a footprint is the same polygon at any height.
* Offline (`--offline` / `--shot`) the avatar's transform *is* its state, so there the
  terrain enters as the ground level itself (the gravity clamp), not as a display
  offset. Lifting the transform after the fact would read as airborne on the next
  frame and gravity would take it straight back down.

## Earcut's answer cannot follow a hill

MVT polygons are triangulated with as few triangles as the outline allows — a field or
a lake is two. Two triangles cannot follow terrain, they can only be tilted by it, so
ground surfaces are refined before displacement (`refine_for_terrain`).

The refinement is **per edge, not per triangle**: an edge longer than the threshold is
split once, its midpoint is shared by both triangles that own it, and each triangle is
re-cut by how many of its edges were split. Deciding per triangle ("this one is big,
subdivide it") leaves a T-junction wherever a split triangle meets an unsplit
neighbour — and a T-junction on a slope is a crack you can see the sky through.

Not everything is refined, and the exceptions are about kind, not cost:

* **Water is level.** One height for the whole *cell*, not the whole body — every
  water polygon in a tile is merged into one `SurfaceMesh` before the levelling
  sees it, so a hillside pond and the canal below share the cell's median height,
  and a river crossing a cell edge can step against its own continuation.
  Per-body levelling would need the tessellator to keep bodies apart (or tag
  their vertex ranges); until then this is the honest description. Displacing a
  lake per-vertex would tip it on its side, which is why the median exists at all.
  That level is also **the quay, not the water**: AHN is no-data over a canal and
  the banks are dilated inward to fill it. Putting the real surface there was
  checked and not taken — the drop is metres and the ground stack cannot hold it
  ([rijkswaterstaat-water-levels.md](rijkswaterstaat-water-levels.md)).
* **A bridge deck is straight.** Same treatment as water, for the opposite reason: the
  ground dipping into a valley is exactly what a viaduct does *not* do.
* **Buildings step, they do not shear.** One height per footprint. 3DBAG vertices are
  heights above the building's own base, so per-vertex displacement racks the walls
  out of plumb.
* **Trees keep their own normals.** A canopy's normals describe the tree, not the hill.

## One quad per sample — and never a constant

Every mesh that stands on the field sizes its subdivision off the field's own 25 m
sampling (`TERRAIN_SAMPLE_M`): `refine_for_terrain` splits edges longer than 24 m, and
`map_stream::relief_divisions` gives a basemap tile `ceil(tile_m / 25)` divisions.
Finer buys nothing — it interpolates measurements that were never taken — and coarser
is worse than it looks, because relieved clipmap levels are stacked only 0.4 m apart:
a level that approximates the ground more crudely than the one above it pokes through
it on any slope.

A **fixed** division count is the trap, and it was tripped once (fixed 2026-07-31).
`RELIEF_DIVISIONS = 48` was chosen for a ~1.5 km tile, but the clipmap's finest zoom
follows altitude — over a Dutch city it is z17, a **184 m** tile — and the same
constant was applied to all four levels under `RELIEF_MAX_TILE_M`. That put 4,608
triangles on a tile the field describes with 7 samples across.

Measured on the Groningen anchor (`--offline`, settled, 60 m), before → after:

| | basemap tris | shadow tris | idle fps |
|---|---|---|---|
| flat (no `--height`) | 3 k | 1.07 M | — |
| relief, fixed 48 | **942 k** | 2.96 M | 24–27, with 12 s bands at **10–11** |
| relief, per-sample + no cast | **373 k** | 0.79 M¹ | 35–45, no slow bands |

¹ excluding `lod22`, which loaded more cells in the second run.

The relief tiles are also `NotShadowCaster`: four levels of ground redrawn into every
cascade measured as ~2.5× the mesh's own cost, for self-shadowing a 25 m field can
barely express. The trade is deliberate — a hill no longer darkens the valley behind
it at a low sun. Over Dutch relief (±2 m) that is invisible; if the Veluwe ever needs
it, it belongs to the finest level alone, not to all four.

**This is a UI bug as much as a rendering one.** The egui bridge spends one pass per
pointer-button event (`egui_manager::take_pass_events`) and hit-tests against the
previous pass's rects, so a click costs three passes; egui discards a press→release
that spans more than `max_click_duration` (0.8 s). At 10–11 fps with hitches, panel
checkboxes and close buttons stop responding while the camera still feels fine. Any
change that halves the frame rate shows up there first.

## Still flat, deliberately or otherwise

* The **globe tier** — from orbit, 100 m of Veluwe is far below a pixel.
* **Coarse clipmap levels**: only tiles at or under `RELIEF_MAX_TILE_M` get relief
  geometry. The rest are *sunk* below the field's lowest point instead, because the
  pyramid stacks levels 0.4 m apart and an eight-metre hollow would otherwise fill
  with blurry z10 imagery from the level underneath.
* Everything **outside the Netherlands**. `HeightLayer::covers` is AHN's own envelope;
  elsewhere the field never bakes and every consumer behaves exactly as it did before
  this module existed.
