---
topic: rendering
triggers: [world_to_viewport, GlobalTransform, place_pins.rs, street_labels.rs, gizmo.rs, data_layers.rs, "overlay wobble", "pin jitter"]
updated: 2026-11-26
---

# Projecting world points to the screen from `Update`

Anything that paints an egui overlay onto a world position — saved-place pins, the
POI card, the gizmo handles — reads the camera and calls `Camera::world_to_viewport`.
Three things about that are non-obvious; the first two show up as the same symptom: the
overlay **wobbles or swims while the camera moves**, then sits perfectly still the
moment it stops.

## 1. `GlobalTransform` is one frame stale in `Update`

big_space propagates in `PostUpdate` (`BigSpaceSystems::PropagateHighPrecision`,
inside `TransformSystems::Propagate`) — same as Bevy's own transform plugin, whose
docs say outright to run projection systems *after* propagation. Our UI systems all
run in `Update`, after `free_camera::camera_controls` has written the camera's
`Transform`. So the `Transform` they see is this frame's and the `GlobalTransform`
is the one the *previous* frame rendered with.

The translation error cancels (the usual idiom projects `cam_gt.translation() +
delta`, and `world_to_viewport` subtracts the same translation straight back). The
**rotation error does not**. An orbit drag at 90°/s is 1.5° of stale view per frame;
over a ~60° vertical fov that is tens of pixels of overlay sliding against a map
that is drawn correctly. It is invisible in a still frame and in a screenshot, which
is why it survived so long.

Fix: rebuild the basis from this frame's `Transform` instead of reading
`GlobalTransform`. For the camera this is exact, not an approximation — the camera
is the floating origin, and `Grid::global_transform` subtracts the origin's own cell
and rotation, so for that one entity `GlobalTransform == Transform`. Then project
camera-relative deltas through a translation-zero basis. Costs one quaternion copy
per frame. (Moving the systems to `PostUpdate` was not on the table while they drew
into egui's shared root `Ui`; since 2026-11-26 the library draws the map's layers in
`PostUpdate` from what `Update` placed, and the placing still projects this way.)

`place_pins::place_map_pins` does this, as do the plans, the names, a lens's marks
and the device arrow. `data_layers::render_poi_popup`,
`data_layers::handle_poi_click` and `systems::gizmo` still project through
`GlobalTransform` and carry the same one-frame lag (for `handle_poi_click` it is a
click-accuracy error, not a visual one).

## 2. egui rounds every `Area` position to a physical pixel

(Sections 2 and 3 are egui's. The map's layers left egui on 2026-11-26 — the
library draws them, `lib_pins` and its siblings — so they hold for whatever is
still painted by egui at a projected point.)

`containers::area::round_area_position` snaps a floating `Area`'s top-left to a whole
physical pixel, so widgets inside a dragged window don't shimmer against each other.
Correct for chrome; wrong for a marker pinned to the ground, which can then only step
a pixel at a time while the map under it slides smoothly. `fixed_pos` does not opt
out of it.

Fix: keep the `Area` (it is what makes egui own the hover/click, so the world-space
click handlers correctly leave a click on the overlay alone) but **paint from the
exact projected position**, not from the rect the `Area` allocated. The rounded rect
still senses the click; half a pixel between the hit area and the drawn shape is not
something a pointer can resolve.

## 3. A frame with no input re-shows last frame's overlay

Different symptom — the pins stand still for a few frames and then jump — and not
a projection error at all: `end_egui_frame` keeps last frame's drawing when egui
saw no input, and a held joystick or a following camera is no input. Every
projected position went into an `egui_manager::WorldAnchored`, which asked for the
redraw when one moved (`docs/notes/egui-quiet-frame-staleness.md`); it went with
the last egui overlay, and the library redraws every frame. Anything that
moves the thing pinned (`avatar::smooth_remote_avatars` for the people markers and
chat balloons) is ordered before the overlay, or it trails on the frames the
scheduler runs it second.

The first two are avoided outright by anything that can be world geometry instead:
the street names went that way (`docs/notes/map-labels.md`) and neither the stale
basis nor the pixel rounding applies to them, because nothing projects them.
