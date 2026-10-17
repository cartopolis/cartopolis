---
topic: navigation
triggers: [navigation.rs, systems::navigation, "route line", "route ribbon", ribbon, ribbon_faces_up, bevel_join_never_spikes, ROUTE_Y, RouteLine, RouteCasing, RoutePin, route_width_scale, ROUTE_WIDTH_PER_ALT, CONNECTOR_HALF_WIDTH, frame_route, FRAME_MAX_ALT, route_meshes, route_rendered, "Fit button", "giant arrowhead", mitre, miter]
updated: 2026-10-17
---

# The route line

The drawn route is a baked ribbon mesh lying in the road surface
(`navigation.rs`), not an overlay. Almost everything that went wrong with it was
the line being correct, present and invisible, and nothing logging why.

## It is a coplanar-ground problem, not a mesh problem

Two independent failures hid the first version completely, and neither logged
anything:

- **The triangles were wound downward**, so the whole ribbon was backface-culled.
  `ribbon_faces_up` pins the winding, and the material is `cull_mode: None` as well
  so that winding can never hide it again — a flat ribbon has no meaningful back.
- **At `ROUTE_Y = 0.12` the line lost the depth fight with the ground** and
  rendered as horizontal bands: with `near = 0` and a 200 km far plane the depth
  buffer cannot resolve a tenth of a metre at street range. It sits at 0.5 m now,
  but the height is only a margin; what actually settles it is the depth rung it
  takes from `ground_stack::GroundLayer::RouteLine` (with `RouteCasing` and
  `RoutePin` beside it), above every ground sheet. Those rungs used to be bare
  numbers written in this file (18, 20, 22, 22) on a ladder whose other end moved —
  [the-ground-stack.md](the-ground-stack.md) has what that cost.

**How to tell the cases apart.** `ShotMetrics` carries `route_meshes` and
`route_rendered`, so "not spawned" and "spawned but not rendered" are one
`--dump-state` apart. When both are non-zero and still nothing is visible, widen
the line and colour it magenta for one capture: that separates "placed wrong" from
"depth-fighting" in a single frame.

## Bevel joins, not mitre

A mitre offsets each vertex along the bisector by `1/cos(θ/2)`, which runs to
infinity at a sharp turn — and the noded graph emits sub-metre segments
([the-routing-graph.md](the-routing-graph.md)), on which even a clamped mitre shot a
spike clear across a junction (the "giant arrowhead"). `ribbon` draws each segment
as its own butt-ended quad from its own perpendicular and fills the wedge at each
interior vertex with corner triangles. That cannot spike (no vertex is further than
the half-width from the centreline) and cannot pinch, at any angle or segment
length; `bevel_join_never_spikes` holds it. Heights are per point because that is
the only way a bridge deck reaches the mesh ([grade-separation.md](grade-separation.md)).

## Full width only along the road

A route's points run `[start pin, road node, …, road node, end pin]`. Drawing the
full-width ribbon from the pin put a fat line across a canal whenever the pin sat
on a quay and the nearest *drivable* node was on the far bank. The road path is
drawn at full width; each pin-to-road gap is a thin connector
(`CONNECTOR_HALF_WIDTH`, 1 m; none at all under `CONNECTOR_MIN_M`, 4 m).

## The width rides the camera altitude, in power-of-two bands

Six metres of world geometry is a comfortable fifteen pixels at 1 km up and a
*third of a pixel* at 30 km, so planning a cross-town drive drew a bare map with
two pins on it and nothing between them — correct, present, invisible. Every map
app draws its route in screen space; this one cannot, because the line lies in the
road surface and takes the sun, the terrain and the depth ladder with it. So the
metres are scaled instead (`route_width_scale`: ~0.0017 × altitude, i.e. about 8 px
of a phone screen, reaching the base width at ~1.9 km, capped at 64×).

The scale is **stepped in powers of two**, and the band is in the mesh's redraw
key. A width that tracked altitude continuously would re-triangulate the whole
route on every frame of a zoom — the per-frame upload churn that loses a mobile GPU
([upload-bursts.md](upload-bursts.md)). A doubling is a rebuild every few seconds of
a fast fly-out, and rounding keeps the drawn width within √2 of the ideal.

The other end has a band too: below 40 m (only the street-level camera is that low)
the line is a footpath — 0.125× the width, 0.15 m up — because from behind a
walking avatar the 6.4 m band and 14 m start disc covered a whole square and the
avatar in it (2026-10-06).

## It stays until it is cleared

The line used to be gated on the Route panel being open, so you could not plan a
route and then look at it, and closing the planner erased it. It is drawn until the
route is cleared; `Clear` and an endpoint edit both bump `RouteState::generation`,
which is what takes it away. Stripping the map is driver mode's job, not
planning's ([navigation-display.md](navigation-display.md)).

## Solving a route frames it

There was a "Fit" button: one tap standing between the user and the thing they
had just asked for. `drive_route_solver` now frames the route when the solve lands
(`frame_route`, a silent `Teleport`), with three exceptions that are the point:

- **Not in Follow.** The camera is glued to the avatar there, so a framing teleport
  would bring the avatar with it — and planning a route is not a request to be moved
  fifteen kilometres.
- **Not under the shot harness.** `--at` states where a picture is taken from; a
  solve that moved the camera would re-aim every `--ui route` preset and make the
  metrics report a place nobody asked for.
- **Once per pair of endpoints**, not once per solve. Switching travel mode
  re-solves, and yanking back a camera the user has since panned every time they
  tap Bike is the over-eagerness the button avoided by being a button.

The framing ceiling, `FRAME_MAX_ALT`, is 0.9 × `GLOBE_SHOW_ALT`, deliberately under
the space tier: the route carries `HideInSpace` with the rest of the ground detail,
and the old 60 km ceiling flew any route over ~65 km to a viewpoint where the line
is not drawn at all. Past the ceiling the framing shows the middle of a long route —
some of the line beats none of it.
