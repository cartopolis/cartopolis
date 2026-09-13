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

* **The Netherlands is not flat, and neither are its cities** — but you cannot see it
  in a *range*. Groningen's standard deviation is 1.8 m over 6.4 km and its 38 m range
  is a handful of outliers (dredged pits, a landfill), and this note used to conclude
  from that "a capture over a Dutch city will look almost identical with the layer on
  and off — that is the ground, not a bug". **That conclusion was wrong** and it stood
  for six weeks. The Hondsrug rises 7 m through the centre of this box and falls to
  −0.7 m a kilometre west; what was missing was not the relief but the *gradient*, and
  the gradient is a function of the sample spacing, which the range cannot see. See
  "25 m had no slopes in it" below. The Veluwe and the dunes are still where the
  feature is most obvious, but a Dutch city is no longer a place where nothing shows.
* **The two rows that were read as "50 m loses almost nothing".** 128² and 256² over
  the same box do agree to within a couple of metres everywhere except the noisy
  tails — and that is a statement about *heights*, which was then used to argue against
  going finer. Slopes behave the other way round: each halving of the spacing roughly
  doubles the median facet tilt, all the way down to AHN's own grid. Compare heights
  when you are asking "is the ground in the right place" and slopes when you are asking
  "can anyone see it".
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

## Why the whole thing is one request, baked once, on a worker

A consumer asks for a height thousands of times per mesh build, so the field cannot be
a cache of per-question patches like `systems::height`'s. One `HeightRequest` per map
anchor (2049², 12.3 MB, 4.5 s measured) is baked into a regular grid in the anchored
world-XZ frame; after that every lookup is bilinear arithmetic with no I/O.

The bake runs on `platform::workers` and lands through `terrain::apply_coarse_terrain`.
It did not used to: at 25 m the field was 65,536 samples and ran inline. At 3.125 m it
is four million — a `wgs84_to_rd` polynomial each, a distance transform to close the
holes, and a min pyramid over the result — which is a long frame, and a long frame is
the documented way to lose the device on the hardware this has to run on.

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

## A quad per level, not a quad per sample (changed 2026-09-13)

Every mesh that stands on the field used to size its subdivision off the field's own
sampling: `refine_for_terrain` split edges longer than 24 m against a 25 m field, and
`map_stream::relief_divisions` gave a basemap tile `ceil(tile_m / 25)` divisions.
The rule read as obviously right — a vertex per measurement, no more and no less — and
it had a second job nobody wrote down: relieved clipmap levels are stacked only 0.4 m
apart, so a level that approximated the ground more crudely than the one above it would
poke through it on any slope, and making *every* level exact was what made that
impossible.

It is also what pinned the field at 25 m, and what kept Groningen flat. See the section
below.

**What replaced it.** A clipmap level is twice the size and twice as far out as the one
above it, so every level's tiles subtend about the same solid angle: a *constant*
division count per tile is what puts a constant number of pixels on a quad. The
world-space rule spent its triangles the other way round — at 25 m a 2,500 m tile got
100 divisions and 20k triangles for ground kilometres away, while the tile underfoot
got 8. `relief_divisions` is now `min(32, tile_m / TERRAIN_SAMPLE_M)`: constant on every
level a city streams, and bounded below by the field so a small tile never invents
vertices between measurements.

**And the poke-through moved to where it belongs**, into what the mesh samples.
`displaced_grid` reads `TerrainSnapshot::floor_at` at its own quad size — a *lower
envelope* of the field, min-filtered over ±2 quads, built as a 2× min-reduction pyramid
in `terrain::build_floors`. The property, and it is exact rather than tuned: the
interpolated value anywhere in a mesh cell is at most the largest of the cell's four
corner values, and each of those is a minimum over a box containing everything that
cell touches — so the drawn surface is **everywhere at or below the measured ground**,
at any relief, and a coarse level can no more rise through a fine one than a flat plane
could. The 0.4 m stacking is margin now instead of the whole guarantee.
`a_coarse_quad_never_draws_above_the_measured_ground` pins it, checking between the
vertices as well as on them, because linear interpolation is what a mesh actually draws.

A **fixed** division count is still a trap, and the upper bound is what stands against
it. It was tripped once (fixed 2026-07-31): `RELIEF_DIVISIONS = 48` was chosen for a
~1.5 km tile, but the clipmap's finest zoom follows altitude — over a Dutch city it is
z17, a **184 m** tile — and the same constant went to all four levels under
`RELIEF_MAX_TILE_M`. That put 4,608 triangles on a tile the field described with 7
samples across.

Measured on the Groningen anchor (`--offline`, settled, 60 m):

| | basemap tris | shadow tris | idle fps |
|---|---|---|---|
| flat (no `--height`) | 3 k | 1.07 M | — |
| relief, fixed 48 | **942 k** | 2.96 M | 24–27, with 12 s bands at **10–11** |
| relief, per-sample @ 25 m + no cast | **373 k** | 0.79 M¹ | 35–45, no slow bands |

¹ excluding `lod22`, which loaded more cells in the second run.

373 k is the ceiling the constant is sized against: ~166 tiles qualify for relief over a
Dutch city, and 2·32² triangles each is ~340 k.

The relief tiles are also `NotShadowCaster`: four levels of ground redrawn into every
cascade measured as ~2.5× the mesh's own cost, for self-shadowing the field can barely
express. The trade is deliberate — a hill no longer darkens the valley behind it at a
low sun. Over Dutch relief (±2 m) that is invisible; if the Veluwe ever needs it, it
belongs to the finest level alone, not to all four.

**This is a UI bug as much as a rendering one.** The egui bridge spends one pass per
pointer-button event (`egui_manager::take_pass_events`) and hit-tests against the
previous pass's rects, so a click costs three passes; egui discards a press→release
that spans more than `max_click_duration` (0.8 s). At 10–11 fps with hitches, panel
checkboxes and close buttons stop responding while the camera still feels fine. Any
change that halves the frame rate shows up there first.

## 25 m had no slopes in it, and that is why the field is 3.125 m (measured 2026-09-13)

The measurement at the top of this note says Groningen's relief is 38 m over the box
with a standard deviation of 1.8 m, and concludes "a capture over a Dutch city will look
almost identical with the layer on and off — that is the ground, not a bug". **The
conclusion was wrong**, and the reason is that relief is a range and what the eye reads
is a *gradient*.

The Hondsrug rises 7 m through the centre of the Groningen box and falls to −0.7 m a
kilometre west. The 25 m field carried all of it, over 256 samples, so no facet ever
tilted more than half a degree. Method: PDOK `dtm_05m` over the field's own 6.4 km box,
resampled, then the Lambert term of each facet's normal against a 30° sun, relative to
flat ground.

| field spacing | median slope | p90 slope | shading, p05 – p95 |
|---|---|---|---|
| 25 m | 0.46° | 1.96° | 0.96 – 1.04 |
| 12.5 m | 0.72° | 3.50° | 0.92 – 1.07 |
| 6.25 m | 1.07° | 6.68° | 0.86 – 1.13 |
| 3.125 m | 1.34° | 10.98° | 0.77 – 1.19 |

±4 % is below what anyone can see, over textured map imagery, at any time of day. So
the ground was measured, displaced, normal-mapped and lit correctly, and was
indistinguishable from a plane. `terrain_relief_m` could not show it either — it is the
range over the whole box, and it barely moves between those rows, which is why
`terrain_sample_m` is now in `--dump-state` beside it.

**What 25 m cost, and what it bought.** It bought one 220 kB request and a 65,536-sample
bake cheap enough to run inline on the main thread. 3.125 m is 2049² — 12.3 MB on the
wire in 4.5 s, 17 MB resident, four million samples — so the bake moved onto
`platform::workers` (`terrain::apply_coarse_terrain` installs it), the hole fill became
a distance in metres rather than a count of dilations, and `MAX_GRID` in `height.rs` went
to 4096. 2049 and not 2048 so the *intervals* are a power of two and the floor pyramid
halves exactly.

Three things were considered and are worth recording:

* **Half a metre, AHN's own grid, over the whole box.** 12800², 625 MB, 144 requests.
  That resolution is what the fine tier follows the camera to reach, for consumers that
  ask about a point rather than build a mesh.
* **A finer camera-centred mid tier instead of a finer field.** Rejected: the fine tier
  re-centres every 96 m of walking, and anything that *meshes* off a moving tier retears
  its geometry every 96 m and changes shape at the rim as you walk. The field is
  anchor-centred and re-bakes only on a re-anchor.
* **Leaving the clipmap alone and only refining the vector surfaces.** Tempting, because
  in a city the paving and roads cover the basemap almost everywhere. But the two would
  then disagree about where the ground is by more than the depth ladder can absorb, and
  the tile would poke through the street.

## What decides whether a street has a gradient is `MAX_GROUND_EDGE_M`

Worth separating from the field's own resolution, because they are different questions
with different costs. In a city the ground you look at is **vector surfaces** — paving,
carriageways, verges, squares — and the basemap tile under them is covered almost
everywhere. So `MAX_GROUND_EDGE_M` is what the street's shading comes from, and
`TERRAIN_SAMPLE_M` only has to be fine enough that those vertices land on measurements
rather than between them.

It is **12.5 m**, four field samples per edge, and the number was chosen against a
measured upload burst rather than against the picture. Two samples per edge (6.25 m) is
what the field can carry and is what the finest clipmap quad already is, so it looked
obvious. Measured over Groningen 2026-09-13, `--height` on against off at the same
settled viewpoint (53.2194, 6.5665, 260 m):

| | `upload_peak_asset_kb` | `gpu_resident_kb` | pixels differing >16/255 |
|---|---|---|---|
| height off | 8 MB | 1,244 MB | — |
| height on, 6.25 m edges | **37 MB** | 1,666 MB | 36.9 % |

37 MB is *one mesh* handed to the driver in a single frame. A `map_geometry` cell is a
z14 tile, ~1471 m across, and a fully covered one refined to 6.25 m is a six-figure
triangle count merged into one asset per group. That is exactly the burst
`lod22::MAX_CELL_MESH_BYTES` exists to cut up, and the ground groups have no
equivalent — bevy's upload budget writes one **whole** asset per frame, so
`Budget::upload_bytes_per_frame` cannot spread a single huge one however low it is set.

So the geometry stops at 12.5 m and everything below belongs to the layer that costs no
triangles (see below). Splitting the surface meshes by byte budget the way `lod22` does
is what would buy the finer half back, and is the thing to do first if this number is
ever wanted lower.

One related saving fell out of the same rule: `TerrainSnapshot::ground_edge_m` returns
infinity when the field is absent, so `refine_for_terrain` returns after one scan. With
the Height layer off the old constant still cut every ground polygon in the city at
24 m, for a displacement of exactly zero.

`MAX_REFINE_PASSES` is still 5, so one enormous polygon stops at 1500/32 ≈ 46 m rather
than tessellating the frame away — a big park is still coarse, and that is the other
bound to raise if it ever matters.

## A capture can photograph flat ground and call itself settled (fixed 2026-09-13)

The relief meshes are built `MAX_RELIEF_BUILDS_PER_FRAME` (2) at a time — the documented
guard against an upload burst — and ~166 tiles qualify over a city, so a cold clipmap is
about 83 frames from being shaped. Nothing in the settle test knew that. `terrain_ready`
said true, every tile stream went quiet, and the shutter fired on a plane.

Measured on the Posbank at `--wait 20`: `settled=false`, `ktris_total=15k`, and a
photograph of flat ground over 98 m of measured relief. The same viewpoint at
`--settle 6 --wait 200` settles with `busy=[]` and draws the Veluwezoom's slopes.

`map_stream::ReliefBacklog` counts the tiles still owed a relief mesh and `relief` is in
the harness's busy list beside `terrain`. The two are different questions — "has the
field arrived" and "is the ground drawn with it yet" — and only the second decides what
the picture shows.

## Sub-metre is a shading problem, not a geometry one (built 2026-09-13)

3.125 m is what gives a street a gradient. It is not what gives it a kerb, and the
two are different problems with different answers.

AHN's own grid is half a metre and the fine tier already fetches it — 1024² over a
512 m box following the camera. What cannot follow is the geometry: `map_geometry`
builds its ground surfaces per **z14 cell, ~1471 m across**, so half-metre vertices
over one cell is about nine million quads, rebuilt on every re-anchor. Raising the
clipmap instead does not help either, because in a city the vector surfaces cover the
basemap tile almost everywhere — a 40 m capture over the Groningen centre is entirely
rooftops, with no basemap visible at all.

What sub-metre relief *is*, though, is 5–30 cm of amplitude: a kerb, the camber of a
carriageway, the lip of a quay, a rut across a verge. All of that is **shading, not
silhouette** — the same observation `water.wgsl` is built on, where a canal is two
triangles and every wave lives in the normal.

So `systems::ground_detail` puts it in the normal:

* `terrain::encode_slope` turns the tier into an `Rg8Snorm` slope texture on the
  worker that already baked it.
* It holds a **residual** — the slope minus a box blur of itself at
  `MAX_GROUND_EDGE_M` — because the mesh underneath already carries the coarse
  gradient in its vertices, and adding the raw slope would apply every gradient twice.
* `GroundDetailExtension` over `StandardMaterial` perturbs the normal by it
  (`shaders/ground_detail.wgsl`), so the sun, the shadows, the fog and the atmosphere's
  environment map all still reach the ground exactly as before.
* Its uniform is the box's centre in **render** space, rewritten when the box
  re-centres or the floating origin jumps. Unlike `building_windows`, which rewrites
  its ring every frame against a single material, there are hundreds of ground
  materials — so this one is written on change only, a few times a minute on foot at
  worst.
* `params.w` is 0 with the layer off, outside the Netherlands or before the tier
  lands, which makes the shader the plain PBR path plus one comparison.
* `CARTO_NO_GROUND_DETAIL=1` strips it, and `ground_detail_ready` /
  `ground_detail_sample_m` are in `--dump-state`. Both exist because this is a
  *shading* term and colour and lighting are not evidence on the container's software
  rasteriser — the only honest A/B is the same scene with the term removed.

Measured over the Veluwe at 45 m, settled, the same viewpoint with the switch on and
off: `ground_detail_ready` true / false, `ground_detail_sample_m` 0.50 / 0.00, and
14.0 % of pixels differ (5.7 % by more than 4/255).

**It is on the clipmap tiles only.** The vector ground surfaces — paving,
carriageways, verges, squares, which are what you look at in a city — and the road
groups still take the plain path. Extending it there means converting
`map_geometry`'s shared per-group material map and the four consumers that repaint it
(`vegetation`, `wet_surfaces`, `street_furniture`, `street_detail`), and adding the
same three bindings to `road_depth.wgsl`. The volumetric groups must be kept *out* of
it: a tree's normals describe the tree, and giving a canopy the ground's sub-metre
slope is the mistake `stand_on_terrain` already refuses to make. `refresh_ground_detail`
reserves a negative `params.w` for that opt-out.

## Still flat, deliberately or otherwise

* The **globe tier** — from orbit, 100 m of Veluwe is far below a pixel.
* **Coarse clipmap levels**: only tiles at or under `RELIEF_MAX_TILE_M` get relief
  geometry. The rest are *sunk* below the field's lowest point instead, because the
  pyramid stacks levels 0.4 m apart and an eight-metre hollow would otherwise fill
  with blurry z10 imagery from the level underneath.
* Everything **outside the Netherlands**. `HeightLayer::covers` is AHN's own envelope;
  elsewhere the field never bakes and every consumer behaves exactly as it did before
  this module existed.
