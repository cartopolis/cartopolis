---
topic: buildings
triggers: [interiors.rs, doors, entrance, verblijfsobject, Footprint, with_doors, nearest_edge_point, place_doors_on_facades, party wall, hollow, storeys, stairs, floor_lift, InteriorFloors, shell_datum_height, cell_cache_key, systems::interiors, building_facts.rs, collision.rs, building_design.rs]
updated: 2026-10-17
---

# Doors, and what is behind them

A national survey gives a building's shell: correct outside, solid, and sealed.
Three things had to arrive before you could walk into one — where the door is, an
opening in the collider, and something on the other side.

## Where the door is: nobody publishes it, so it has to be derived

**There is no door dataset, here or anywhere.** Checked 2026-08-30:

- The **BAG** registers every addressable unit with a **point**, required only to
  lie inside the pand. It is the closest thing to a national door position there
  is, and it is not one — over the Groningen centre cell it sits a median **1.8 m
  in from the nearest wall**. It is a point in a room.
- The **BGT** has 27 object types (wegdeel, pand, scheiding, straatmeubilair, …).
  None of them is a door or an entrance.
- **3DBAG** is geometry only, derived from the same pand footprint.
- **OpenStreetMap's `entrance=*`** nodes are genuinely surveyed doors and there
  are **137 of them** in a 2.2 × 2.6 km box over central Groningen, against ~8,000
  buildings and ~18,000 addresses. Not something a layer can be built on.

So the register's point is the only input, and a doorway has to be *derived* from
it. It rides on the **building's** record rather than in the heavy per-cell places
file (2 MB) the address readout reads — the mesh lane must not pull that to learn
where a doorway is. One point is a dozen bytes; the file it rides in is the one
that was 0.5 MB.

## The party wall, and why "nearest wall" is the wrong rule

The obvious derivation — put the door on whichever wall is nearest the register's
point — is what shipped first, and it fails on the ordinary Dutch terrace. The
wall nearest a unit's point is frequently the **party wall** it shares with the
house next door, so the panel is drawn inside the neighbour, where the
neighbour's own shell hides it completely. Measured over 1,289 doors in the
Groningen centre cell:

| | nearest wall | nearest wall with nobody behind it |
|---|---|---|
| door is on a wall you can see | 59 % | **99 %** |
| collider actually opens | 17 % | by construction |

That last row is the second half of the same bug, and it is the one that made a
door you *could* see still not a door you could walk through:
`Footprint::in_a_doorway` is a 1.1 m radius around the point, and from 1.8 m
inside the building that circle never reaches any wall at all.

`building_facts::place_doors_on_facades` runs the corrected rule as the cell is
parsed, so one answer reaches all three consumers — the drawn panel, the collider
gap, and the doorway cut out of the room's wall. It takes the nearest wall whose
outside is not inside another building in the same cell and puts the point **on**
it, clamped clear of the corners. The neighbours it can see are only the ones in
the same cell file, so a party wall crossing a cell boundary can still bury a
door; that is a thin strip of a 6 km cell and it fails there the way it did
before rather than worse.

**A variant that also weighed each wall against the street lost.** The BGT's
footway, driveway and paved surfaces are already streamed, and scoring each
candidate wall by its distance to one is the principled version of "which side
faces the street". Measured on the same 1,289 doors, it put 95.2 % of them on a
wall with public space within 20 m, against 93.6 % for the cheap rule — 1.6
points, for a 1.2 MB per-cell dependency inside the mesh job and a rule that
stops working the moment you leave BGT coverage. Rejecting the party wall had
already bought the street.

Note what that measure *cannot* tell you, because it looks like the obvious one
to check: run it against the **original** rule and it also answers 94 %. A party
wall's outward probe lands inside the neighbour and is still a few metres from
the pavement, so "is there public space out here" is blind to exactly the failure
being fixed. Only the neighbour test sees it.

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

- **A storey at a time, never the full height in one volume.** A full-height empty
  volume would be a sixteen-metre shaft with a front door, which is not a thing.
  This started as *one* storey, because the register says how many units a
  building holds and never where its floors are; the shell's own geometry does
  say how much building there is to divide, and "Storeys, and where the count
  comes from" below is how that is now read. Each storey is still a room at a
  nominal ceiling, and what is above the top one is the shell, unchanged.
- **The walls are inset** from the outline, so they stand inside the shell's own
  walls rather than in the same plane, where the two would z-fight the length of
  every room. The inset is an *edge* offset with a corner join, not a vertex push
  along the bisector — pushing vertices collapses a narrow canal house into a bow
  tie.
- **The doorway is cut out of the room's wall at the same points the collider
  opens at.** They have to be the same points: a room whose wall crossed its own
  door would seal the avatar in behind a surface they had just walked through.

Nothing here is double-sided. Walls face in, floor up, ceiling down — but that
hid only the walls. **An interior is drawn only for the building the avatar is
in, or the one opened with Look inside** (2026-10-07); every other building near
the avatar keeps its floors, flights and wall colliders and draws nothing. Facing
inward was not enough because the rooms come from the register's outline and the
storey count from the shell's 70th-percentile roof, and neither stays inside the
shell: on the Grote Markt a flight against an outline wall stood outside a facade
the shell draws further in, and the upper slabs of a building with a low wing came
up through its roof. From inside, the building is glass (`building_select`), so
there is no shell left for the interior to poke through.

## A footprint is solid or hollow, and a wall is crossed, not entered

Opening the collider at the door was only half of letting somebody in, and the
missing half was invisible for as long as nobody walked past the threshold.
`collision::Footprint` was one shape of thing — a region the avatar may not be
*in* — which a point test answers and an ejector enforces. A doorway punched a
1.1 m disc out of that region and left the rest of the floor a no-go area, so a
step further in was refused in both axes and the avatar stood in its own doorway
unable to move. It read as the avatar freezing against something invisible,
because it was.

So there are two kinds now, and the kind is decided where the room is:

- **Solid** — a mass we have nothing but a shell for. Unchanged: a point test,
  and anything that ends up inside is pushed back out. Walking into a bare shell
  is not a room, it is a hole in the world with the street showing through it
  (the walls are back-face culled).
- **Hollow** — a building with a door *and* a room behind it. Only its walls
  stop the avatar; the inside is somewhere to be, and nothing ejects from it.

`interiors::can_hold_a_room` is the one place that is decided, and `lod22` asks
it against the same outline `interiors` will build the room from. Two answers
derived separately would drift into exactly the failure above, in the other
direction: a door opening onto nothing.

**A wall is tested by crossing, never by sampling.** A wall has no thickness —
it is the ring itself — so there is no volume for a point test to land in, and a
sampled test would miss it on any frame long enough to step over it: the avatar
would walk out through the back of the house on a slow frame and not on a fast
one. It is also what the online path needs, and that one is not a matter of
frame rate at all: it probes `COLLIDE_PROBE_M` (0.6 m) ahead of the avatar, which
from just outside a facade lands well inside the room, so "is that point solid?"
answers no every single time.

**The storey counts as ground for the airborne test.** Both movement paths skip
collision for an avatar off the ground — the footprints are 2D, so applying them
to a flight would divert it around a building it is passing over. Offline the
transform *is* the avatar's state, so `floor_lift` is in it, and a first floor
stands 3.2 m up: without the lift in the ground reference every avatar upstairs
read as flying and the walls of every upper storey stopped being walls. Online
the sim position stays flat and the lift is a display offset, so that path never
had the bug — which is the kind of asymmetry to check both sides of.

## What a door does *not* do

- **It is not cut into the wall.** The panel stands 4 cm proud of the brick: the
  shell arrives as a finished triangle soup and a hole would mean re-triangulating
  around it. A panel cannot leave a crack, and reads as a doorway at any distance
  this is visible from.
- **Its height is a constraint, not a taste.** `building_windows.wesl` paints no
  window below `FIRST_SILL_M` (2.2 m), so a 2.1 m door sits in the one band of the
  facade the window shader leaves alone. A taller door would come out with panes
  across it.
- **The collider's opening is generous** (1.1 m in every direction from the
  point). The point now lies *in* the wall, so the radius straddles it and opens
  it from both sides — but its position *along* the wall is still only derived
  from the unit's own, so the doorway sits within a metre or so of the real leaf
  rather than on it, and a tight gap would be a door you have to hunt for.

## LoD3 needs none of this

An imported IFC model brings its own doors: `IfcDoor` is imported and
`lod3::is_solid_class` excludes it from collision, so a modelled doorway is
already open. All of the above is for the shells.

## A published fix that nobody could see

The door work landed on 2026-08-30 and an APK carrying it was installed on
2026-09-02. There were still no doors, and the app was not at fault.

`building_facts::cell_cache_key` had **no format version in it**, and the cache
has no revalidation — a hit is served and the host is never asked. The BAG cells
gained `doors` and `fp` when the extractor was re-run on 2026-08-29, so any
client that had already visited a city held a copy of that city from before, and
would hold it forever. Both fields are `#[serde(default)]`, so nothing failed:
the cell parsed, every building came out with no entrance and no outline,
`lod22` drew no door, `interiors` built no room, and the map looked exactly as it
always had. Updating the app could not touch it. Reinstalling would have.

`lod22`'s key already carried a `v2` for precisely this, and its own comment
records the same bug happening there with the mangled ids. The BAG key now
carries one too. **The rule is that a cache key with no version is a bug the
moment the file it names can gain a field**, and the tell is always the same
shape: the new code is provably right, the deployed data is provably right, and
the two never meet.

## Storeys, and where the count comes from

A room used to be the ground floor and nothing else, because "the register says
how many units a building holds, never where its floors are". That is still true
of the register. It is not true of the *geometry*: a LoD2.2 vertex carries its
height above the building's own base, so the shell says how much building there
is to divide, and `lod22::shell_datum_height` measures it as the cell is parsed.

**Not the maximum vertex height, which is the ridge.** A pitched Dutch house
carries three or four metres of roof over its top ceiling, so dividing the ridge
by a storey invents a floor in the rafters. The datum is the 70th percentile of
the *roof* surface's own vertices — the same number 3DBAG publishes as
`b3_h_dak_70p`, and it publishes it because it approximates the eaves on a
pitched roof and is exact on a flat one. Wall vertices are excluded because they
run from the ground up, so including them drags the percentile down by however
much wall the building has, i.e. by its height.

**The obvious alternative lost on a measurement.** The BAG ships each unit's
floor area and how many units a building holds, so `units × area / footprint` is
a storey count that needs no geometry at all and no new plumbing. Over the 6,342
buildings in Groningen's centre cell where both answers exist, the two agree on
**32.5 %** of buildings, and the area estimate is systematically lower — the
register's `gebruiksoppervlakte` is *usable* area, which excludes the walls, the
stairwell and the low half of an attic. That is a bias, and it could be
corrected. What cannot be corrected is that floor area does not know how tall
the building is: the stack has to fit **under the roof**, and only the height
can promise that. `storeys_for` therefore floors the datum, and
`no_storey_ever_reaches_above_the_shell_that_holds_it` walks every shell height
from 3 to 30 m to pin it.

The join runs the other way from every other fact in this area: the BAG cell
carries the outline and the doors, and the *height* comes from the LoD2.2 lane,
which is the only one that ever sees it. A building whose shell has not landed
yet reads as one storey — the old behaviour — and gains its floors on the
rebuild the growing facts index triggers anyway.

## The stairs, and why a floor nobody can reach is not a floor

Stacking rooms is the easy half and on its own it is a lie: the upper storeys
would be visible through the stairwell and reachable only by flying. So a
building gets its extra storeys **only if a flight fits in it**, and one that
does not keeps its ground floor whatever its height says.

The flight is straight and stands against the longest wall. Both are what let
the same footprint stack on every storey: a flight that wandered would need its
own well on each floor and its own answer for where the one above starts. Where
it sits on that wall is fixed — centred — rather than chosen, because a
staircase that moved when the avatar walked fifteen metres would be a
poltergeist. It is checked against the ring on **both** axes, because a corridor
has a wall long enough for the run and no depth at all to put the flight in, and
the length test alone passes it.

The well is a **bite out of the boundary, not an island in the slab**. That is
the reason for putting the flight against a wall and not merely a consequence:
an interior hole would have to be a second ring strictly inside the first, and a
well that stops five centimetres short of the wall leaves a nib of floor hanging
over it. `notch_ring` walks the outline, detours around the flight when it
reaches the wall it is on, and carries on — which keeps the result a simple
polygon.

That did retire the fan triangulation. A fan about the centroid is right on the
near-convex outline a footprint usually is, and a notched slab is exactly the
shape it gets wrong — and it gets it wrong by **filling the well back in**, so
the stairs arrive under a lid while the metric, the collider and the geometry all
still say the hole is there. It is earcut now, which the client already had.

**Superseded in the cut, not in the placement** (building designs, 2026-10):
`notch_ring` is gone. `BuildingDesign::compile` cuts every well out of the slab
by polygon difference (`building_design::subtract`, over `i_overlay`), which
yields polygons *with holes* — and earcut takes holes, so the lid failure above
cannot come back that way either. That is what lets a flight an owner draws
stand anywhere in a room. `building_design::generate` still puts the generated
flight against the longest wall, centred, for the poltergeist reason above.

## Standing on the first floor

`InteriorFloors` is a **ground level, not a collider** (`InteriorFloors::support`
answers it, from where the avatar already stands). `BuildingColliders`
answers "may I move here" in two dimensions; a stack of floors poses the other
question, "how high is the ground", and answering it with more 2D polygons cannot
work.

Three things are load-bearing.

- **A surface more than `STEP_UP_M` above you cannot pick you up.** The ceiling
  over your head and the storey above it are both surfaces directly above you.
  Only the flight rises *continuously* out of the floor you are standing on, so
  only the flight can take you up — which is what makes the stairs the way up
  rather than decoration over a lift.
- **The flight supports you as a ramp, not as sixteen treads.** The treads are
  drawn because a ramp does not read as a staircase, but walking one riser at a
  time is a bob at every step and a foot through every tread.
- **It enters where the terrain does, and for the same reason.** The sim holds no
  building geometry and never will, so the wire, the prediction and the
  reconciliation all stay on the flat plane it actually simulates, and only what
  you see and walk on stands on the storey. `AvatarState::floor_lift` is a
  separate field from `terrain_lift` rather than one summed number because they
  are *found* differently: the terrain is a field sampled anywhere, and this is
  the answer to "may I be raised onto that", which depends on where you already
  are.

Stepping off a landing is a **fall**, and the avatar's own `vertical_velocity`
cannot express one: in the sim's frame the avatar is standing on the ground the
whole way down, so it is the ground that drops. `floor_lift` therefore falls
under the same gravity as everything else and rises instantly, which is the
difference between a step and a drop stated once.

## What the storeys do not do yet

- **The flight is not solid from its open side.** Where the ramp is more than a
  step above you, you are not lifted onto it — so between roughly a tenth and a
  half of the run you can walk into the treads rather than under them. Past that
  the flight is over your head and walking under the stairs is correct. The
  soffit is drawn for that reason: without it the flight is invisible from below
  and the storey above shows straight through it.
- **The rooms are still bare.** No internal partitions, no furniture, no light of
  their own — a storey is one volume the size of the building.
- **Nothing has run on a phone.** The support field is CPU state and costs
  nothing, but the triangle count is several times what a single-storey set cost
  and the layer has never been measured on a handset.
