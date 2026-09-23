---
topic: buildings
triggers: [building_select.rs, building_card.rs, BuildingSelection, ghost, in_ghost, ghost_ring, set_indoors, PrimClick, ShellMeasure, shell_stats, roof area, volume, "click a building", "double click", "double tap", "touch", "tap does nothing on android", "fingers_this_frame", "see through walls", "avatar inside a building", bagviewer]
updated: 2026-09-22
---

# Selecting a building, and seeing into one

Three things arrived together because they are one question — *which building is
this?* — asked by a cursor, by a card, and by the avatar's own feet.

## The pick is a ray against footprints, not against the mesh

A LoD2.2 cell is thousands of buildings merged into a handful of meshes
(`lod22::MAX_CELL_MESH_BYTES` cuts a cell into byte-bounded pieces, not into
buildings), so there is **no per-building entity to hit**, and making one is not
affordable on the lane that already owns the largest assets in the app.

What there is: the register's own outline per building, already streamed for the
collider, and now a measured height. `building_select::ray_prism` intersects the
ray with that outline extruded to the ridge. It is exact for a prism rather than
approximate for a box — the y slab gives an interval in `t`, each ring edge the
ray crosses gives a crossing in `t`, and the answer is the first point inside
both. The only case it gets wrong is a ray through the notch under a roof
overhang, which is centimetres.

Brute force over the loaded facts, on the frame of a click. Same stance
`picking::prim_picking` takes and the same reason: tens of thousands of cheap
rejections once, against a hit test every frame forever. The broad phase is the
ray's crossing of the ground plane — `BuildingFacts::near` around the midpoint of
that segment, widened by `PICK_CORRIDOR_M`.

**Two consumers now want the same left click**, and the prim layer wins. That
needed `picking::PrimClick`, and the shape of it is the interesting part: it
records whether the *ray* found a prim, on every press `prim_picking` evaluates,
hit or miss. Not what `prim_picking` then decided to do — a **scripted prim is
worked rather than selected**, so "is a prim selected?" answers `false` for a
press that was very much spent. And not a frame stamp either: the presses
`prim_picking` skips (egui has the pointer, no cursor, no camera) are exactly the
presses `pick_building` skips, so a value that is only written on a processed
press cannot go stale in a way anybody can reach.

Adding one parameter to `prim_picking` put it over **Bevy's 16-parameter cap**,
which is a compile error that reads as "`prim_picking` is not a system set". The
camera-cell / anchor / session trio is a `SystemParam` now (`PrimPickWorld`) — the
same wall `free_camera::camera_controls` hit when touch input arrived.

## It takes two presses, on both devices

The first press is not a choice on either device, because on both it is *also*
the first frame of a camera move.

On touch the emulated mouse presses `Left` on the frame a finger **lands**
(`systems::touch`, and it has to — a tap can be over in two frames). So can the
first frame of a pan, of a pinch and of a long press, and all four reach
`pick_building` as one indistinguishable `just_pressed`. Reported 2026-09-13 as
"too easy to open on touch"; fixed by asking a finger for a **double tap**
(`TouchGestures::touch_click` + `TouchGestures::double_tap`).

Desktop was left on one click in that fix, on the reasoning that a cursor has a
hover and a still hand to say "this one". It does — and it is still holding the
same button down to orbit: `free_camera` rotates in Follow and Fly and pans in
Earth on a held `Left`, so every drag over the city begins with a `just_pressed`
here too, and this system sees only the press. By the time the drag has revealed
itself as a drag, the card is open over the map and the glass is on a building
nobody chose. Same accident, same answer (2026-09-14): **a double click**.

Four things about the shape of that:

- **Each device is asked in the way it can answer.** A finger goes through the
  gesture recogniser, which knows what else the hand was doing;
  `touch::TouchGestures` carries the timings and the frame-shift trap
  (`docs/notes/touch-input.md`). A cursor is measured in `building_select` itself
  against `DOUBLE_CLICK_SECS` / `DOUBLE_CLICK_SLOP_PTS`, because a mouse press is
  only ever visible on the frame it happens and *where* it happened is all a
  double click is made of. The gate picks the branch per **press**, never per
  session, so a touchscreen laptop driven with a mouse gets the mouse's rule.
- **The cursor needs no drag tracking to reject a drag**, which is why its half
  is twenty lines and the finger's is a state machine. A cursor does not lift and
  come back: it is sitting where the first click left it. Press, orbit 200 px,
  release, press — and the second press is 200 px from the first, well outside
  the 6 pt slop. The finger's 44 pt slop cannot lean on that, hence `Press`'s
  explicit `strayed` and `long_press_sent`.
- **A single press is inert, not a deselect.** The accident runs both ways: a
  card dismissed by the first frame of an orbit is the same annoyance. Deselect
  is the card's ✕, a double press on empty ground, or a re-anchor.
- **Nothing else here changed.** A scripted prim is still worked on one press and
  the context menu is still the long press or the right button — a building is
  the only click consumer that *opens* something over the map, which is what
  earns it the extra press.

### …and on Android it then opened nothing at all (fixed 2026-09-22)

Reported as "tap doesn't work on android", and the gate above was not the cause:
`touch.double_tap()` was correct and simply never became true, because the tap
before it had never been seen. `recognize_touch_gestures` polled
`Touches::iter()` — a *level* that no longer contains a finger whose `Ended`
arrived in the same frame's event batch — and a handset drawing a city runs
frames long enough that a brisk tap lands entirely between two samples. The
second tap of a double tap is exactly the brisk one.

Two things worth carrying away. The symptom pointed at the newest code and the
fault was in the oldest: this gate shipped on 2026-09-14 and the polling has been
there since touch arrived, working everywhere anyone had tried it. And it was
invisible from a desktop **by construction** — a 100 ms tap spans six 60 Hz
frames, so no amount of clicking here could reproduce it.
`docs/notes/touch-input.md` has the mechanism and the rule it leaves behind.

## Roof area and volume are measured, not fetched

The obvious way to put "Totale Dakoppervlak" and "Volume" on the card is the way
3DBAG offers them: `b3_opp_dak_plat + b3_opp_dak_schuin` and `b3_volume_lod22`
are attributes on the WFS, so ship them from atlas's extractor. That is a
change on both sides of a hand-synced format, a pipeline re-run for every city,
and a `cell_cache_key` bump — to carry two numbers that are **functions of
triangles the client already holds**.

`lod22::shell_stats` measures them in the pass that was already walking the soup
for the storey datum. Nothing new is requested, nothing is re-run, and it works
on a cell already in the cache.

Two details are load-bearing:

- **The volume sum needs no floor.** A LoD2.2 solid is the surface above its own
  base and carries no ground face. The divergence-theorem sum over triangles is
  unaffected by a missing face exactly when that face's plane passes through the
  origin — and the base plane is `y = 0`.
- **It has to be recentred first.** The vertices are metres from the *map
  anchor*, so at kilometre offsets the triple product of a 10 m building is the
  difference of two enormous numbers and f32 has nothing left. The sum is taken
  about the building's own first vertex, in f64.

`ShellMeasure::top_m` is a third number and is deliberately not `datum_m`: the
datum is the eaves (storeys stack *under* a roof — see
[doors-and-rooms.md](doors-and-rooms.md)), and a highlight drawn to the eaves
leaves a pitched roof sticking out of it unselected.

## The glass: a prism, because the shell cannot be tinted

The whole 3DBAG lane shares **one opaque material**, which is what lets
`building_windows` light every window in the city by writing a single uniform. So
a per-building tint on the shell itself cannot be translucent at all, and giving
one building its own material means recovering its triangles from a merged
buffer.

Both the highlight and the ghost are therefore one translucent mesh extruded from
the register's outline — the same builder, drawn twice with different heights and
tints. It is also what the reference implementations do: 3dgroningen.nl draws a
red translucent box over the selected pand.

- **Selected** → glass over the shell, to the ridge, inflated
  `HIGHLIGHT_INFLATE_M` so it is not coplanar with the walls it covers. The
  offset is an *edge* offset with a corner join, not a vertex push along the
  bisector — pushing vertices collapses a narrow canal house into a bow tie, the
  same rule `interiors::inset_ring` states.
- **Occupied** → glass where the walls were, to the eaves, not inflated, because
  the walls it replaces have been taken out of the pass and there is nothing left
  to fight.

Outward normals are found by **probing** (step 5 cm off the edge, test whether
that lands inside the ring) rather than derived from the signed area. That is the
bug [doors-and-rooms.md](doors-and-rooms.md) records twice, and the test here
runs both windings for the same reason.

## The glass must not be frustum-culled, and the metric is what said so

The shell's vertices are **anchored-world metres** while its transform carries the
grid cell's offset, exactly as `interiors` builds its rooms. Bevy derives the AABB
from the mesh in its own local space, so the volume it tests is not the volume the
mesh occupies, and the frustum answers *no* from directly overhead — the one
viewpoint from which a highlight is most obviously meant to be visible.
`NoFrustumCulling`, for the reason `systems::particles` sets it.

What is worth keeping is not the fix but how it was found. The first capture
showed no glass, and a picture cannot tell "never built" from "not in shot" —
which is the trap `doors-and-rooms.md` records costing an afternoon to camera
angles. `building_shell_meshes` / `building_shell_rendered` were added *before*
guessing, and answered `1 / 0` on the next run: built, not rendered, therefore
visibility and not placement. Two numbers, one capture, no camera hunting.

They still earn their keep afterwards, because the failure has a third mode:
`1 / 1` with nothing on screen is placement or alpha, and that is what a prism
falling back to its 3.2 m default height looks like when the shell measurement has
not landed.

## Seeing in: the walls go, the roof stays

`building_windows.wgsl` gained an `in_ghost` test — an even-odd polygon test
against a ring carried in a uniform — and discards a fragment that is both inside
it and on a **wall**. `is_wall` was already computed, for the windows.

**Only the walls.** A building whose roof went too would be a hole in the street
with a floor in it; the roof standing over a glass shell is what a cutaway looks
like.

A uniform rather than a per-vertex building key, on this module's own standing
rule: four bytes a vertex on the largest geometry in the app, carried forever, to
serve a state that is off almost all of the time. The ring is rewritten **every
frame in render space**, because under `big_space` the origin jumps a whole
`WORLD_CELL_EDGE` as the camera crosses a grid cell — the trap the window
lattice's whole note is about. One material, 32 floats, once a frame.

Two guards that are not optional:

- **The bounding box is the broad phase.** Without it every wall fragment in the
  city walks a 32-edge polygon while anything is ghosted.
- **A ring longer than the shader holds is decimated, not truncated.** A
  truncated ring is an *open* polygon and even-odd on one is noise: half the
  building's walls would flicker between drawn and discarded as the camera moved.
  Dropping every other point keeps it closed and moves the boundary by
  centimetres.

## Which building you are in, and why only the glass needs to know

`InteriorFloors::building_at` names the building the avatar is standing in. It
feeds the see-through shell and nothing else.

It very nearly fed collision too. The doorway used to be unusable — the gap is a
`DOOR_HALF_WIDTH` radius around a point *in* the wall, so an avatar that crossed
the wall was outside the gap and inside the ring a metre later, where `slide`
stopped it dead and `eject_dir` walked it back out — and the fix drafted here was
to except the building you are in from your own collider. `main` landed a better
one first: `collision::Footprint` now distinguishes a **hollow** building (one
with a room in it) from a solid one, `contains` is unconditionally false for a
hollow footprint, and `crosses_wall` stops the avatar at the wall instead. That
needs no state, no name and no system — and it gives the follow camera's boom the
indoor behaviour for free, because `segment_fraction`'s `blocked(from)` guard
simply stops firing inside a building you are legitimately in.

The lesson worth keeping is the one about where the answer lives: "which building
is this?" is a rendering question here, and the moment it looked like a collision
question too, the collision half wanted to be solved without ever asking it.

Two things `building_at` does still owe the glass:

- **It tests the building's outline, not the room's inset walls.** The two
  disagree over a 22 cm band, and that band is the doorway. Asking the inset ring
  would put the glass up a step after you were through the wall.
- **It answers only for a building with a room in it**, which is what makes it
  safe to dissolve walls on: a sealed shell is never reported, so its walls are
  never opened onto an interior it does not have.

## What it does not do

- **Outside the Netherlands there is nothing to select.** The pick, the card and
  the glass all key off the BAG's outline and the BAG's facts. The OSM detail
  patch draws buildings everywhere and carries neither.
- **No hover state.** A press selects; nothing highlights under the cursor. On a
  phone there is no hover to have, and a desktop-only affordance for the primary
  interaction is how two behaviours drift apart.
- **The card cannot say who owns it.** Cadastral *ownership* is not open data —
  [kadaster-parcels.md](kadaster-parcels.md) records the check. Everything on the
  card is from the BAG, the monuments register, or the geometry itself.
- **Nothing here has run on a phone.** The polygon test is per-fragment and
  guarded by a bounding box, but "guarded" is an argument, not a measurement, and
  an Adreno has not seen it.
