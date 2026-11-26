---
topic: rendering
triggers: [world_to_viewport, GlobalTransform, place_pins.rs, street_labels.rs, gizmo.rs, data_layers.rs, "overlay wobble", "pin jitter"]
updated: 2026-11-26
---

# Projecting world points to the screen from `Update`

Anything that places an overlay onto a world position — saved-place pins, the
POI card, the gizmo handles — reads the camera and calls `Camera::world_to_viewport`.
The trap shows up as the overlay **wobbling or swimming while the camera moves**,
then sitting perfectly still the moment it stops.

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
and the device arrow. `data_layers::drive_poi_card`,
`data_layers::handle_poi_click` and `systems::gizmo` still project through
`GlobalTransform` and carry the same one-frame lag (for `handle_poi_click` it is a
click-accuracy error, not a visual one).

## 2. Order the mover before the overlay

Anything that moves the thing pinned (`avatar::smooth_remote_avatars` for the
people markers and chat balloons) is ordered before the system that places the
overlay, or it trails on the frames the scheduler runs it second.

Until 2026-11-26 egui added two more: it rounded every `Area` to a physical pixel
(a pin stepped while the map slid), and it re-showed last frame's drawing on a
frame with no input (`retired/egui-quiet-frame-staleness.md`). The library draws
every frame at the placed position.

The stale basis is avoided outright by anything that can be world geometry instead:
the street names went that way (`docs/notes/map-labels.md`), and nothing
projects them.
