---
topic: parked-vehicles
triggers: [parking.rs, furniture.rs, "parked car", "parked cars", "bicycle parking",
  "sites layer", "car park", "vehicles", "more lively", "inhabited", world_places.rs,
  park_in_bays, parking bays, parkeervakken]
updated: 2026-10-19
---

# The car parks are filled, and the cars are inferred

`cartopolis_geo::parking` (2026-08-26) fills the `sites` layer's `parking` and
`bicycle_parking` polygons with parked vehicles. It rides `map_geometry`'s
existing fetch, parse, worker and per-cell batching, joins the street furniture's
`SurfaceGroup::Furniture` mesh and material, and answers to the same **Street
furniture** toggle — so collision matches what is drawn, which is the rule
`collision::sync_prop_colliders` exists for.

Before this, `sites` (84 features in the Groningen z14 fixture, 56 of them
`parking`) was one flat grey fill and nothing else.

## The claim it makes, and the one it refuses

OSM surveys the **lot**, never the cars in it. The polygon is real, its
orientation is real, and its capacity is implied by its area; every individual
vehicle is invented. So:

- The fill has to be right about what the data does say — where the tarmac is,
  which way the bays run, how many fit — and must not pretend to be right about
  anything else.
- **Vehicles are not interactables.** `world_places::CellPlaces` keeps them in
  their own field: the collider index takes them, the proximity prompt does not.
  A prompt saying "Parked car" would be this client naming a thing it made up.
  `prop_name` and `icons::furniture` still carry arms for both kinds, written as
  what they *would* say rather than as an `unreachable!`, so making one
  promptable later is a decision and not a panic.

That is the same line the tree scatter and the fill palette already draw — an
unstyled polygon beats a wrongly styled one.

## Four decisions in the layout

- **Rows follow the lot's longest edge** (`scatter::dominant_angle`, the function
  the orchards already use). A car park is bays perpendicular to an aisle, and the
  aisle runs the long way. Squaring the bays to Mercator north instead puts a
  diagonal grid of cars inside every kerb line on the map.
- **Every third row is empty.** Two back-to-back rows plus an aisle is a 16 m
  module, which is what a real lot is built on and what stops a fill reading as a
  raft of metal. The rows of a pair face away from each other, toward their own
  aisle. The row index is `rem_euclid`, not `%` — a lot straddling the lattice
  origin has negative rows, and `-1 % 3` is `-1`, which would put the aisle on a
  different row on each side of it.
- **Some bays are empty** (30% for cars, 20% for bikes). A full car park is as
  artificial as an empty one. The vacancy is hashed off the cell seed, so a lot
  that streams out and back is parked identically — a car that moves is a car
  somebody watched move.
- **The lattice bound is the tile's diagonal, not its edge.** The indices are in
  the lot's own rotated frame, and a square rotated 45° reaches √2 times as far as
  its side; handing `bucket_range` the tile edge clipped the far corner of a
  diagonally-aligned lot and parked no cars in it.

The access road is kept clear with `scatter::RoadMask` for the reason the tree
scatter needs it: a lot's entrance is *interior* to the polygon, because
Shortbread draws the carriageway on top rather than punching a hole, so no
boundary inset can see it.

## Measured, and what bounds it

**2026-08-26, Groningen z14 fixture** (`parking::tests::survey_the_fixture`):
531 vehicles — 215 cars, 316 bikes — costing **3.5 MB** of mesh. A car is seven
boxes (4.40 × 1.72 × 1.45 m, body clear of the ground so the wheels reach under
it), a bike is four.

The seventh box is a **body-coloured roof over the glass**, and it is worth its
17%: with the greenhouse as the topmost part, a capture from 25 m showed every car
as an identical dark grey rectangle — from above, which is where this camera
spends most of its time, the whole per-instance paint scheme was a thin skirt
under the windows. Glazing you can see through the *side* of reads as a car; a
tinted sunroof over the entire cabin does not.

Every other producer in this crate is bounded by a *survey*: there are as many
benches as OSM recorded. A lot fill is bounded by the ground it covers, so the
count is the thing that has to be capped, and `MAX_VEHICLES_PER_CELL` (900) is
close to a working limit rather than the pure headroom
`MAX_FURNITURE_PER_CELL` is. Hitting it costs whole lots at the end of the layer —
an empty car park beside a full one is what that looks like — and it is logged.

This is why **`build_furniture_meshes` now takes a `max_bytes`** and cuts, as
`build_vegetation_meshes` does. One mesh per cell was safe while the group's whole
vocabulary was surveyed points; an area fill is what made the canopies the largest
asset in the app, and it would have done the same here. The cut is taken between
objects, so no car loses its wheels. Note the ceiling bounds the *rate*, not the
total: `docs/notes/upload-bursts.md` on why residency is the other half.

## Most of them are street-side strips, not lots

Worth knowing before tuning the aisle module: in a residential tile the `parking`
polygons are mostly **kerbside parking strips** — long shapes a bay or two wide,
running along the street — rather than the open lots the layout was designed for.
So what the fill mostly produces is cars parked down residential streets, and the
two-rows-and-an-aisle module rarely comes into play there. That is a better result
for the goal than a car park would have been, and it is the reason the inset and
the road keep-out matter more than the module does.

## Where a city surveys its bays, the bays win (2026-10-19)

Groningen registers every public parking bay as its own polygon (34,994; OSM has
1,741), served by atlas's `registers` dataset. `park_in_bays` puts one car in each
— a different and stronger claim than the lot fill, since only *whether this bay is
taken* is invented — and `extract_parking_around` leaves out every OSM lot that
contains a surveyed bay's centre, so a lot is never parked twice. Lots with no
surveyed bay in them (private car parks) are filled as before. The numbers and the
bay sizes are in `municipal-registers.md`.

## Known limits, none worth a request to fix

- **A lot straddling a cell boundary is filled twice**, from two independently
  clipped halves on two lattices that do not line up. The boundary inset leaves a
  couple of metres either side, so the seam reads as an aisle. z14 cells are 611 m
  at NL latitudes, so it is rare.
- **The tile cannot tell a surface car park from a multi-storey.**
  `amenity=parking` on a *parkeergarage* gets a ground-level fill inside the
  building drawn over it. Testing it against the `buildings` layer was considered
  and rejected on cost: that layer is 8,552 features and ~75% of a tile's
  triangles, it is never otherwise decoded on this path (heights come from 3DBAG),
  and building a keep-out mask over it would be a new per-cell tessellation for a
  case the building itself mostly hides.
- **Vehicles fade on the furniture ramp** (`FURNITURE_FADE_START` 150 m →
  `FURNITURE_FADE_END` 450 m), which was tuned for a 1.8 m bench and is early for
  a 4.4 m car. One group means one material means one fade; splitting the group to
  give cars their own band would cost a draw call per cell to buy a pop nobody has
  complained about yet.

## Colour

Bodies are painted per instance out of `CAR_PAINTS` / `BIKE_PAINTS` through the
new `Tone::Paint`, which reads `FurnitureInstance::paint` instead of the tone
table — the one part of the furniture scheme that varies *within* a form. It has
to work that way because the group has a single shared material: the variety is
carried by the vertex colours, exactly as the canopy's two tones are.

The palette is muted on purpose and is the one place this layer visibly departs
from reality. Real traffic is the most saturated thing in a street; this map is
pale cartographic paper (`systems::vegetation`), and a car park in real car
colours is instantly the loudest object on screen. Silver, white and graphite are
most of any Dutch lot anyway, with two desaturated hues to break up the grey and
nothing brighter.
