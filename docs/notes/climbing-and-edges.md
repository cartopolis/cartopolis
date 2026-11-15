---
topic: climbing-and-edges
triggers: [climb, pose_for, SeatFit, seat_fit, feet_ground, QuayEdge, quay_edges_of, quay_near, set_quay_cell, climbing, "climb up", ledge, ledge_under, ledge_ahead, ledge_at, ledge_edge, as_ledge, standable, CLIMB_MIN_M, CLIMB_MAX_M, LEDGE_FOOTING_M, systems/player/climb.rs, systems::climb, Edges, EdgeSeat, seat_on_edge, find_edge_seat, dangle, "--clip climb", "--clip dangle", "--ui ledge", "--ui edgesit", sit on a wall, quay edge, remote_ledge_step, wants_climb]
updated: 2026-11-15
---

# Climbing onto low walls, and sitting on edges

Asked for by the owner as "klimmen en zitten" (2026-11-15): climbing onto
walls and edges, and sitting in more places than benches. Built the way
swimming was (`avatar-locomotion.md`): a display floor and a pose on the
model holders, every viewer on its own map. **Neither the sim nor the wire
changed** — no protocol revision, no new message kind.

## What can be climbed, and why

Only what collides and says how high it is: the street-detail barriers
(`street_detail.rs`, Overpass/atlas `street` cells), whose collider is the
drawn wall from the ground to its top (`height-aware-collision.md`).

- **Ledges are `wall`, `city_wall` and `retaining_wall`**
  (`BarrierKind::standable` → `Footprint::as_ledge`): masonry with a coping
  to stand on. **Not** a hedge (it gives), a fence (a rail on posts, 6 cm
  across) or a guard rail (not solid at all).
- **Climbable by height**: the top `CLIMB_MIN_M` (0.35 m, `Body::STEP` —
  lower is stepped over) to `CLIMB_MAX_M` (1.3 m, chest height on the default
  man) over the soles. The height is **the one drawn**: the tag when the way
  has one, else the form's (`BarrierKind::form`). So a retaining wall, drawn
  at 1.2 m, is climbable, and an untagged garden wall, drawn at 1.5 m, is not.
  Measured over central Groningen's street cells (2026-11-15): 167 walls, 19
  with a height (12 between 0.3 and 1.2 m), 77 retaining walls, none tagged.
  A retaining wall at the water is the quay and is not drawn or collided
  (`by_the_water`), so it is not climbed either.
- **The surveyed BGT walls (`surveyed.rs`, measured heights, LineKind::Wall /
  Parapet) are not colliders** and so are not climbed: they are walked
  through today. Making them solid is the next step if the owner wants walls
  with measured heights to climb; it changes what blocks everyone.
- **Quay walls are not colliders either** (`quay-walls.md`); a quay is sat
  on along the drawn wall's top edge (sitting, below).

Being on top is the client's: `BuildingColliders::ledge_under` is a roof to
`interiors::Footing` beside `roof_under`, so a climber stands on the top,
walks along it, and walking off the edge is the fall off a roof
(`floor_drop_speed`). `LEDGE_FOOTING_M` (0.12 m) keeps a foot on a 0.3 m top
while the body's centre is just past it. Everybody else is kept out of the
wall as before: a footprint blocks a body whose soles are below its top less
a step, and the climber's soles are on it. A wall top is only stood on from
above — `ledge_under`'s reach is a step over the soles — so a jump that
clears a wall can also land on it, as on a roof.

## The climb

E (or the prompt's "Climb up") facing a climbable wall within
`CLIMB_REACH_M` (1.0 m — where walking into one stops, `COLLIDE_PROBE_M`, and
a little more), or **Space while walking into one** (then the climb, not a
jump). `climb::Climb` carries the avatar from where it stands to a landing on
the top over the clip's 1.49 s, the wall let through (offline the transform
moves; online the wish does, steered with the soles counted on the top, and
`eject_avatar_from_buildings` stands down). When it ends the floor is the top.
Movement input waits until then.

The clip is CMU 15_02 (makehuman-avatars.md): a knee up onto a 0.39 m step,
0.365 m up on the default man. **The model is lifted the rest of the way**
(`climb_lift`: the wall's height less the clip's step, smoothstepped over the
climb), so a 1.2 m wall is a step and a 0.84 m lift. It reads as a big step
up for walls to about 0.7 m and increasingly as a float above that. CMU has
no hoist onto a waist-high wall; its other climbs are stairs and playground
frames with the hands on an overhead bar. The climb fades out at once
(`apply_pose`): it ends standing on its step and the idle stands on the
floor just raised, and a cross-fade would sink the body by the step.

**Other viewers** cannot be told a climb started (that would be a wire
field), so they infer it: an avatar inside a ledge footprint, near the
ground, is on top of it, because collision keeps everybody but a climber out
of a wall (`ledge_at`, `avatar::remote_ledge_step`). The first frame one
arrives there with the wall climbable from the ground starts the climb clip
on that viewer's clock, and the top is its ground from the end of it; off the
edge it drops at 4 m/s. A remote climb is therefore seen from when it reaches
the wall — about half way through its owner's — and late by the link. A
player whose own client draws no walls (street detail off, as on the phone
rung by default) walks through them; one arriving inside a wall faster than
`REMOTE_CLIMB_MAX_SPEED` (1 m/s — a climb carries its avatar at under 0.9) is
taken to be passing through and stays on the ground. One strolling through
slower than that is still drawn climbing it.

## Sitting on edges

"Sit" is offered (`climb::find_edge_seat`):

1. standing on a wall top: at its nearest edge with a drop of
   `EDGE_DROP_M` (0.35 m) beyond it, trying the way the avatar faces, then
   either side, then behind;
2. on the ground facing a wall 0.35–0.9 m high within reach: on its near
   edge, facing back out;
3. on the ground facing the water, within 1.5 m of a drawn quay wall with
   `EDGE_DROP_M` or more below its top: on the wall's top edge, facing out.

It is the bench's seat (`sitting-down.md`): an XZ offset the wire already
carries (protocol 19), with a **lift** (`Seated::lift`) and a clip chosen so
that **no foot ever goes below the ground it sits above** (the owner,
2026-11-15: "de avatar zit met zijn benen in de grond. dat kan niet").
`climb::pose_for` decides it from the seat's height and the *highest*
ground under where the feet come down (0.2–0.65 m out from the seat, a
drawn quay wall crossed on the way counting at its top), on the sitter's
own legs (`SeatFit`, measured on every body as it is built:
`cartopolis_human::build::seat_fit`):

- the legs hang (`dangle`, CMU 14_29, the seat on the edge) only where the
  hanging soles at their lowest stay 2 cm above that ground — on the default
  man a drop of about 0.64 m or more;
- otherwise the feet are planted (`sit`): the seat on the edge, or, where
  that would put the soles into the ground, the hips raised until the feet
  stand on it. Between the two (an edge too high for planted feet and too low
  for hanging ones) the feet are planted and stand a little off the ground.

`no_seat_puts_a_foot_under_the_ground` poses and skins each chosen clip, every
frame, at the chosen lift for edges 0.3–2 m high and five body heights, and
holds the lowest sole within 2 cm of the ground or above it.

**What put the feet in the ground:** the dangle's seat was measured at 0.45
m over its ground when it is 0.62 m. The measure took the lowest skin under
the hips, and the swinging calves pass back under the stool; bounded to a
third of the hips' height under the joint (`build::seat_depth`) it is the
seat. With 0.45 the clip was lowered 0.17 m too far, and legs chosen to hang
from any edge 0.42 m up.

**Every viewer measures the edge itself** (`climb::seat_on_edge`) from the
seat point (position + offset) and the facing (the sitter's replicated yaw):
on a ledge, or on a drawn quay wall's top with `EDGE_DROP_M` below it, it
is an edge, and the lift and the clip follow; otherwise it is a bench and
draws as one. The local avatar measures with the same function, so
a viewer with the same map draws the same seat. A viewer with street detail
off has no walls and draws a wall sitter as on a bench.

**The lift is re-measured every frame, never kept** (`Seated::edge`,
`climb::remeasure` in `track_edges`). It is a difference between the edge's
height and the root the sitter is drawn at, and both move after it sits: a
capture seats the avatar the frame it lands, and the first captures
(2026-11-15) drew a sitter 1.2–1.4 m over the grass beside the wall and over
the pavement beside a parked car. Logged at the Noorderhaven: seated with its
root at 0.0 while the ground under it was −4.76 m, so the lift measured then
would have put the seat 4.76 m below the quay once the avatar landed. Offline
a seated avatar also never came down at all — the seated branch of
`handle_avatar_movement` returned before gravity — and now stands on its
ground and floor. `an_edge_seat_is_drawn_on_the_wall_whatever_the_root_was`
pins both: the seat point on the wall, the drawn seat at its top.

**A quay seat is on the drawn quay wall** (`collision::QuayEdge`). The
surface streamer reads each cell's quay walls' top edges off the meshes
`map_geometry::quay_walls` built (`quay_edges_of`) into `BuildingColliders`
(`set_quay_cell`, dropped with the cell); "Sit" at a quay is the nearest
drawn top edge within `QUAY_REACH_M` (1.5 m) of an avatar facing the water,
the hips 5 cm behind it, facing out over the water. No drawn wall, no quay
seat. Where the map draws a strip of quay between two water polygons the
seat goes out to the last wall (`outer_quay`), and a wall crossed under the
feet refuses the seat (`Surroundings::feet_ground`). It was found from the register's water (`swim_surface_at`) until
2026-11-15, and on the Noorderhaven's north quay that ring reaches a metre
under the street: the seat sat behind the drawn wall and the legs went
through it. Logged after the change: both Noorderhaven seats 5.0 cm from the
drawn edge, at its top. The seat is not refused for having the register's
water under it, for the same reason.

## What lost

- **Telling the sim** (a climbed flag, the seat's height on the wire, a
  `Emote::Climb`): the sim has no map to check any of it against, the seat
  already shows a client-measured offset is enough, and every viewer can
  measure the wall. A wire field would have been protocol 50 for something
  each viewer already knows.
- **A dangle flag on the wire**: the drop under the seat decides it on every
  viewer.
- **Making the clip's own rise do all of it** (cutting it to the wall's
  height): one clip, many walls; the lift is the difference.
- **Stairs (13_35–38, 14_21–23)** for the climb: three 0.2 m steps in three
  seconds read as stairs that are not there. **01_06–08** for the dangle: the
  hands hold an overhead bar throughout.

## Seeing it

`--clip climb` and `--clip dangle` photograph the clips. `--ui ledge` stands
the avatar on the wall it was put on (`--at` on the wall's OSM line) and
`--ui edgesit` sits it on the nearest edge the prompt would offer
(`shot_harness::seat_scripted_avatar`). A 0.7 m brick wall (OSM way
1358344358, `height=0.7`, `source:height=estimate`) runs along
53.21855,6.55977 in Groningen.
