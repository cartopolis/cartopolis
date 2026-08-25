---
topic: flight-at-speed
triggers: [flight stutters, flying is jerky, avatar rubber-bands, flight speed, fly faster, climb rate, follow camera lags, avatar off screen, snap threshold, reconciliation, teleport sound, teleport animation, arrival]
updated: 2026-08-25
---

# Why fast flight juddered, and what the fixes rest on

Reported 2026-08-25 as "the avatar flight is stuttery". Three separate causes,
none of them in the flight controls themselves. Recording it because two of the
three are constants that look correct until you multiply them by the altitude
ramp, and anyone re-tuning from first principles would restore them.

**Flight is the one thing in this client that is fast.** `motion::MAX_SPEED` is
32 m/s, and `motion::fly_altitude_factor` multiplies it by up to
`FLY_ALT_MAX_FACTOR` (20) — 640 m/s at ~2.4 km up. Every constant tuned against a
1.8 m/s walk is being asked to hold at three hundred and fifty times that speed.
That is the whole story below.

## 1. The reconciliation snap threshold was a fixed distance

`avatar::ONLINE_SNAP_ERROR` was a flat 8 m: past that, `smooth_online_avatar_display`
hard-sets the display to authority instead of easing toward it, on the theory
that only a teleport or a server correction moves an avatar that far.

But reconciliation has a **standing error of its own**, and it is proportional to
speed. The display integrates `predicted_velocity` every frame; the target is the
last snapshot extrapolated by its age, capped at `ONLINE_MAX_EXTRAPOLATION`
(0.15 s). Whatever latency that cap does not cover, the target trails by, and the
ease (`ONLINE_RECONCILE_RATE`, 9/s) settles at `speed / 9` metres behind:

| gait | speed | standing error | vs. an 8 m threshold |
|---|---|---|---|
| walk | 1.8 m/s | 0.20 m | fine |
| run | 6 m/s | 0.67 m | fine |
| flight, ground level | 32 m/s | 3.6 m | fine |
| flight, 400 m up | 107 m/s | 11.9 m | **snaps** |
| flight, 2.4 km up | 640 m/s | 71 m | **snaps every frame** |

Around the crossing point (~72 m/s, i.e. ~270 m of altitude) the error hovers on
the threshold and the display alternates between easing and snapping from frame
to frame. *That alternation is the judder.* Nothing is wrong when it happens —
the prediction is correct, the connection is healthy.

So the threshold is a distance with a **duration** in it:
`online_snap_error(speed) = max(8 m, speed × 0.6 s)`. Six tenths of a second is
far more than reconciliation's own lag at any speed and far less than any
teleport, which moves the avatar to another part of the world.

**A threshold in metres on a quantity that scales with speed is a threshold in
seconds that nobody wrote down.** The same trap is waiting in
`ONLINE_MAX_EXTRAPOLATION` (a genuine time, deliberately) and in
`driver::GPS_TELEPORT_M` (a genuine distance, deliberately) — check which kind
you have before tuning either.

## 2. The follow camera trailed further than its boom could reach

A first-order lerp at rate `k` trails a target moving at `v` by `v / k` metres,
for as long as the motion lasts. The follow rig ran at 12/s against a boom capped
at `AVATAR_CAM_MAX` (25 m):

| speed | trail at 12/s | vs. a 25 m boom |
|---|---|---|
| walk | 0.15 m | invisible |
| run | 0.5 m | invisible |
| flight, ground level | 2.7 m | noticeable |
| flight, 2.4 km up | 53 m | **avatar is off the back of the frame** |

`free_camera` now leads the rig by exactly that much
(`avatar_state.velocity / FOLLOW_RATE`, capped at `FOLLOW_LEAD_MAX`), which
cancels the lag at any speed instead of stiffening the ease — a rate high enough
to keep up at 640 m/s would make a walk feel welded to the camera.

Two things are load-bearing. Only the **eye** leads; `look_at` still aims at the
avatar's real position, so the framing moves and the subject does not. And the
cap exists for *discontinuities*, not for speed: a teleport, a reconciliation
snap or the frame a session ends can leave a velocity that never happened.
`AvatarState::velocity` is zeroed when Follow is left for the same reason —
re-entering Follow snaps the rig onto its target, and a stale lead would place
that snap tens of metres off.

Driver mode is exempt, as it is from the speed-coupled FOV: its rig is written
from the map orientation every frame and already aims ahead by a deliberate
amount (`CameraState::drive_lookahead`).

## 3. The vertical thrust was a step, and the heading had no turn rate

Neither is stutter in the reconciliation sense; both are discontinuities in the
world velocity, which is what they look like.

- **Climb** assigned `FLY_VERTICAL_SPEED` on the frame the key went down and zero
  on the frame it came up. Times `fly_altitude_factor`, that is a
  hundreds-of-metres-per-second step. `motion::step_fly_vertical` ramps it, in all
  three motion sites. The wire carries two booleans, so this ramp is the one part
  of the vertical model the sim has to *reproduce* rather than receive; it runs
  per 20 Hz tick there and per frame on the client, which differ by well under a
  metre during the ~0.5 s a thrust change takes.
- **Heading** took the wish direction whole, every frame. On the ground that is
  right — a walk is footfalls, and turning just changes where the next one goes.
  In flight it rotates the velocity vector ninety degrees between two frames.
  `avatar::advance_move_dir` arcs it instead, at `FLIGHT_TURN_RATE`. Deliberately
  faster than the *facing* eases, so the avatar is never seen flying sideways.

## What "fly faster" actually meant

The ask alongside the stutter was to accelerate faster and cover more ground.
Altitude is the mechanism (see `fly_altitude_factor`) and it was not the ceiling
that was wrong, it was the time to reach it: at the old 12 m/s climb, getting
from the ground into the part of the ramp worth being in took **~40 s**. Raising
`FLY_VERTICAL_SPEED` to 20 m/s takes that to ~24 s; `FLY_ACCEL`/`FLY_DECEL` went
70/80 so a course change costs under half a second rather than two thirds.

`FLY_ALT_REF` was left at 120 m on purpose — it is deliberately the same number
as `free_camera::FREE_SPEED_ALT_REF`, so avatar flight and the Fly camera agree
about how fast the ground should slide past at a given height. `FLY_ALT_MAX_FACTOR`
was left at 20 for the reason its own doc gives: past ~640 m/s the map streams
for a viewpoint that outruns it, and that is a *streaming* limit, so raising it
would trade the stutter this note is about for a worse one.

## Teleport arrivals

Unrelated cause, same session. A teleport is the only movement in this world with
no travel in it — the avatar is at one place on one frame and a hundred kilometres
away on the next, and nothing on screen distinguishes that from the map having
been redrawn around a figure that never moved.

- The arrival **scales**, it does not dissolve. The Quaternius characters carry
  plain opaque materials; fading them means a second material per slot and a place
  in the transparent sort, for an effect that is over in 0.45 s. A scale is one
  transform on a holder `update_avatar_flight_pose` already writes every frame.
- It is published as a **sequence counter** (`AvatarState::teleport_sequence`),
  not a one-frame flag, for the reason `EmoteState` carries one: two jumps to the
  same place are two events, and the sound and the pose read the edge in different
  phases of the schedule.
- Bumped in `geo_nav::apply_teleport`, not at the call sites — a teleport queued
  before the world anchor exists stays pending for as long as it takes, and a
  sound fired when it was *queued* would play over an empty map minutes early.
- **The default is to announce.** `Teleport::silent` is set at the sites where a
  whoosh would be a lie about who acted: the startup location restore, a scripted
  `--shot` capture, the globe-descent re-anchor, and the device-fix jumps in
  `locate`/`driver`, which fire whenever the GPS reports a big enough step.
