---
topic: flight-at-speed
triggers: [flight stutters, avatar bobs in flight, terrain lift, flying is jerky, walking is jerky, movement wobble, prediction jitter, avatar rubber-bands, flight speed, fly faster, climb rate, follow camera lags, avatar off screen, snap threshold, reconciliation, extrapolation, snapshot gap, nagle, nodelay, udp, teleport sound, teleport animation, arrival, systems/player/avatar.rs, systems/player/free_camera.rs, systems/net/network.rs, core/src/motion.rs, cartopolis_core::motion, systems::interiors, systems/nav/geo_nav.rs, systems::diagnostics, simulator/src/lib.rs, NetPredictionHealth, ONLINE_RECONCILE_RATE, ONLINE_MAX_EXTRAPOLATION, online_snap_error, fly_altitude_factor, airborne_lift]
updated: 2026-10-17
---

# Why fast flight juddered, and what the fixes rest on

Reported 2026-08-25 as "the avatar flight is stuttery", **again on 2026-08-26
after those fixes shipped** — from a phone, signed in to the hosted sim, which is
the case the first round never touched — and **again on 2026-09-11, this time
with "same for walking, but less obviously noticeable"** beside it. Seven causes
now, none of them in the flight controls themselves. Recording it because most of
them are constants that look correct until you multiply them by the altitude
ramp, and anyone re-tuning from first principles would restore them.

Sections 1–3 are the first round (client-side smoothing and the camera).
Sections 4–5 are the second (the snapshot stream that feeds all of it).
Section 7 is the third, and it is the one that also explains the walk: it is
about the *shape* of the correction rather than about any threshold, which is
why it shows at 1.8 m/s as well as at 640. Section 6 is why the answer was not
UDP.

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
seconds that nobody wrote down.** `driver::GPS_TELEPORT_M` is the other side of
the coin and is a genuine distance, deliberately — check which kind you have
before tuning either. `ONLINE_MAX_EXTRAPOLATION` was named here as already being
a genuine time and therefore safe; it was a genuine time and it was still wrong,
for the different reason section 4 gives.

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

## 4. The client stopped believing itself after 150 ms

This is the one the second report was about, and it is the one that only shows up
on a phone talking to a real server.

`smooth_online_avatar_display` eases the predicted display toward the last
authoritative pose *extrapolated by its own age*. `ONLINE_MAX_EXTRAPOLATION` caps
that age. Past the cap the target stands still while the display keeps
integrating, so within `1 / ONLINE_RECONCILE_RATE` the ease brakes the avatar to a
**dead stop**, and it stays stopped until the next snapshot lands and it lurches
forward. The braking is deliberate — an avatar nobody has confirmed for a while
should stop rather than fly on — but the cap decides how often an ordinary event
triggers it, and the cost is proportional to speed. At a walk it is 20 cm and
invisible. At 250 m/s it is the stutter.

The cap was 0.15 s: three server ticks, which assumes the only thing between the
two ends is a healthy local network. Two ordinary events beat it several times a
minute on a handset and neither is a fault:

- **The mobile link.** Inter-arrival on cellular spikes to a few hundred
  milliseconds, and everything shares one ordered stream (chat, prims, voice), so
  one retransmission delays every snapshot queued behind it.
- **Our own frame.** `network::apply_network_snapshot` drains the mailbox once per
  Bevy frame, so a 200 ms hitch — a tile upload burst during fast flight, i.e.
  exactly when this matters — ages every queued snapshot by the length of the
  hitch before any of them is read. From inside the smoothing function that is
  indistinguishable from a network fault, and it isn't one.

Now 0.5 s, which covers both and still parks a genuinely dead link inside a
second. It stays under `ONLINE_SNAP_SECONDS` on purpose: a server that has really
stopped agreeing with us has to surface as divergence and then as a snap, not be
extrapolated away.

**Waiting longer is safe here for a reason that is easy to miss.** The
extrapolation runs on `predicted_velocity` — the client's own live input — not on
anything the snapshot carried. A longer window means trusting our own keys for
longer, not coasting on a stale reading of somebody else's. That is why this
number can be measured in half-seconds where a dead-reckoning-from-the-server
design could not afford it.

`AvatarState::snapshot_age` is stored uncapped now and clamped at the one place
that extrapolates (`avatar::extrapolated_target`). Clamping it at the source cost
the only measurement of how late a snapshot actually was — the moment the number
is worth reading is the moment a stored clamp stops being able to say it.

## 5. Nagle's algorithm was on, on every socket

`TCP_NODELAY` was set nowhere in the tree: not on the client's TCP connect, not on
its WebSocket, not on the server's accept loop, not on the axum listener the
deployed `wss://` clients all arrive through.

Nagle holds a small write until the previous one is acknowledged, to save headers
on bulk transfers made of many tiny writes. Nothing on this wire is that — an
intent is one complete message the sim wants now, a snapshot is one complete
message the prediction wants now. What the delay buys instead, on a link with real
round-trip time, is intents leaving in clumps and snapshots arriving in clumps,
which is precisely the late-snapshot pattern section 4 is about. On loopback it is
invisible, which is why it survived this long.

Four sites, all best-effort (a socket that refuses the option is still a working
socket): `network::set_nodelay` for both client carriers, the simulator's accept
loop, and `axum::serve::ListenerExt::tap_io` for the `/ws` listener.

## 7. The correction was stiff enough to turn arrival times into motion

Reported a third time, 2026-09-11: flight still not smooth, and **walking too,
less obviously**. That pairing is the diagnosis. Sections 1–5 are all things
that scale with speed and therefore say nothing about a 1.8 m/s walk; a fault
that shows at both ends of a 350× speed range is proportional to speed, which
means it is in the part of the loop that multiplies by it.

Two causes, and the brief was explicit: **the client being smooth matters more
than the client agreeing with the server.**

### The reconcile target is noisy, and a stiff ease passes the noise on

Section 4 made the target a moving one — the last snapshot extrapolated by its
own age — which is what stopped the display being dragged backwards between
ticks. What it cannot do is make the target *smooth*. The target is rebuilt
whenever a snapshot lands, and snapshots do not land on a metronome: a snapshot
40 ms late moves the target by 40 ms of travel before the display has moved at
all. That is 7 cm at a walk and 10 m in flight at altitude, and both are
correct — nothing has diverged, the news is just late.

At `ONLINE_RECONCILE_RATE` = 9/s that lateness was taken out of the picture
within a frame or two of arriving, which means it *became* motion: a correction
of `9 · 0.07 ≈ 0.6 m/s` against a 1.8 m/s walk, twenty times a second. Visible
as a wobble in the walk; at altitude the same arithmetic is hundreds of metres
per second and it is the judder.

Reconciliation now acts on a **low-passed error** rather than on the raw one
(`avatar::reconcile_display`, `ONLINE_ERROR_FILTER_RATE` = 7/s over
`ONLINE_RECONCILE_RATE` = 3.5/s). The pair is a second-order response, damping
ratio ~0.7, corner ~0.8 Hz. On top of it the correction speed is capped at a
quarter of the avatar's own pace (`ONLINE_CORRECTION_FRACTION`), so whatever
survives the filter reads as the motion being slightly off rather than as a tug.

Measured 2026-09-11 over a steady journey against a sim that agrees completely,
with the snapshots arriving ±40 ms either side of an 80 ms latency — the worst a
single frame's travel deviated from the nominal:

| | old (one pole, 9/s) | now |
|---|---|---|
| per-frame travel wobble | **30.5 %** | **2.3 %** |

The number is the *same at every speed*, which is the point and is why the
report named walking as well as flight: the error is proportional to speed, so
as a fraction of the travel it is a constant. Sections 1–5 all scale with speed
and none of them could have explained a wobble at 1.8 m/s.

**The filter does that work alone — the cap never enters it.** Also measured:
the largest error arrival noise opens is 6 cm at a walk and 22 m at the top of
the flight ramp, against a knee (`correction speed / rate`, where the cap starts
to bite) of 57 cm and 46 m. So the cap is free to be set by the *other* end,
which is how fast a genuine desync closes: at `ONLINE_CORRECTION_FLOOR` = 2 m/s
a 3 m disagreement closes in 1.4 s with the worst frame at 1.5× walking pace
(1 m/s took 2.8 s; 3 m/s reached twice walking pace). `avatar.rs`'s
`arrival_jitter_does_not_reach_the_picture` asserts both halves, and
`the_old_stiff_correction_is_what_the_wobble_was` keeps the A/B runnable so the
two rates are not just numbers to round off later.

Two things deliberately bypass both, because they are the cases where the answer
must *arrive* rather than be eased into: a snap (past `online_snap_error`, i.e.
a teleport or a server correction) and a stall (past
`ONLINE_MAX_EXTRAPOLATION`, where the target stands still on purpose so an
unconfirmed avatar brakes). Filtering the brake would have taken it from a third
of a second to a second and a half, and a stall is by definition not jitter.

**Correcting section 1 while we are here.** Its table reads the standing error
as `speed / ONLINE_RECONCILE_RATE`, which was true when the target was a
staircase. Once the target is extrapolated it advances at the same velocity the
display integrates, so the error converges on *zero* and what the display
actually trails the world by is the latency the extrapolation cannot see — the
rate no longer sets it. That is why lowering the rate by a factor of 2.6 costs
accuracy only in transients, and why `ONLINE_SNAP_SECONDS` still had to go from
0.6 to 1.0: the snap threshold has to stay clear of the *transient*, which the
gentler ease leaves standing for longer.

### Flight speed stepped at snapshot rate, because the altitude came from the sim

`send_network_intent` derived `motion::fly_altitude_factor` from
`AvatarState::position` — the authoritative height, which only advances at the
sim's 20 Hz tick. Flight rides altitude, so during a climb the *whole* velocity
was re-scaled twenty times a second in steps. At a kilometre up each snapshot
moves the height by ~10 m, which is an 8 % jump in horizontal speed as well as
vertical, and the follow camera leads by that same velocity (section 2), so the
view lurched with it.

It reads the **predicted** height now — the display, with `terrain_lift` and
`floor_lift` taken back off, which is the flat frame the sim actually simulates.
The authoritative height was chosen to stop a high flight rubber-banding and it
does not do that: reconciliation compares positions, and a factor derived from a
height that is itself reconciled converges on the sim's answer anyway,
continuously instead of in twenty steps a second. The residual is a systematic
1–2 % over-prediction during a sustained climb (the client's height leads the
server's, and the ramp is exponential in height), which is metres against a snap
threshold of hundreds.

The same expression fixed a second bug beside it: the gravity arc subtracted
only `terrain_lift`, so standing on an upper storey predicted a fall from the
height of that floor on every frame, which the reconciler then undid — for as
long as you stood indoors above the ground floor.

## 8. The ground was still under a flier's feet

Reported a fourth time, 2026-10-08, after all of the above. Sections 1–7 are
about *time* — when the news arrives and how hard it is acted on. This one is
about *height*, and none of them could have reached it.

The sim is flat; the measured ground is added on the display side
(`AvatarState::terrain_lift`, plus `floor_lift` for storeys and tunnel cuts).
That lift was the ground height under the avatar **on every frame at any
altitude**. Flying over Groningen at 300 m the avatar dropped two metres for each
canal, rose for each dike, and — since the half-metre tier — shivered over
kerbs, at whatever rate flight carried it across them: 100 m/s crosses a 30 m
canal in a third of a second. The follow camera aims at the avatar, so the whole
view shook with it. Being proportional to relief rather than to speed, it shows
over the city and not over the polder, which is a pattern nobody would read as
"the terrain".

`avatar::airborne_lift`: exact up to 1.5 m above the sim's ground (a jump's
apex is ~1.1 m, so a hop still lands on the kerb), then easing toward the ground
at a rate falling geometrically from 25/s to 0.3/s by 25 m. A long rise of the
land is still followed; a ditch is not (≤ 0.2 m, tested). A descent sweeps the
rate back up, so the lift is exact before the feet arrive.

The storey state machine (`interiors::step_floor_lift`) is held above 2 m for a
related reason: stepped at altitude it dropped the floor into every tunnel cut
the flight crossed, and — because getting out of a cut is refused except by the
ramp — by that rule left it there (read from the code, not reproduced), so
the next landing anywhere would be seven metres underground.

## 6. Why not UDP

Asked directly, and worth writing down because it will be asked again.

Nothing in sections 4 or 5 is caused by ordering or by retransmission. The avatar
stalls because the client refused to extrapolate, and it would stall identically
over UDP. Nagle is a socket option, not a protocol.

The costs are real and the benefit is unmeasured. A browser cannot open a UDP
socket at all, so it would be a **third** carrier beside TCP and WebSocket rather
than a replacement — and the whole reason `net::handle_connection` is generic over
`FrameTransport` is to avoid a second copy of the connection loop, let alone a
third. The same connection carries sign-in, chat, prims and voice, all of which
want ordering and delivery, so it would mean splitting into a reliable channel and
an unreliable one: a protocol project, not a socket change.

Revisit it if `NetPredictionHealth` (below) shows large gaps *with* nodelay on and
a generous window — that would be genuine loss, which is the thing UDP actually
addresses.

## The instrument: `NetPredictionHealth`

Both rounds of this were diagnosed by reading constants, because nothing measured
the thing being complained about. `resources::NetPredictionHealth` now does:
snapshots applied, the last and worst gap between them, and — the number that
matters — the share of predicted frames spent **parked** past the extrapolation
window.

It goes into the shareable diagnostics report (☰ ▸ the report `systems::dev::diagnostics`
composes) rather than a log line, for that module's own reason: `logcat` needs a
cable, which is what the person holding the failing device does not have.

Read it as a fork. `parked` near zero during a stuttering flight means this is not
a network problem at all and the `perf:` block above it is where to look. Anything
above a percent or two means it is. Nothing else in the report tells those two
apart and they need opposite fixes.

## Teleport arrivals

Unrelated cause, same session. A teleport is the only movement in this world with
no travel in it — the avatar is at one place on one frame and a hundred kilometres
away on the next, and nothing on screen distinguishes that from the map having
been redrawn around a figure that never moved.

- The arrival **scales**, it does not dissolve. The avatar bodies carry
  opaque materials; fading them means a second material per part and a place
  in the transparent sort, for an effect that is over in 0.45 s. A scale is one
  transform on a holder `update_avatar_model_pose` already writes every frame.
- It is published as a **sequence counter** (`AvatarState::teleport_sequence`),
  not a one-frame flag, for the reason `EmoteState` carries one: two jumps to the
  same place are two events, and the sound and the pose read the edge in different
  phases of the schedule.
- Bumped in `geo_nav::apply_teleport`, not at the call sites — a teleport queued
  before the world anchor exists stays pending for as long as it takes, and a
  sound fired when it was *queued* would play over an empty map minutes early.
- **The default is to announce.** `Teleport::silent` is set at the sites where a
  whoosh would be a lie about who acted: the startup location restore, a scripted
  `--shot` capture, the globe-descent re-anchor, the device-fix jumps in
  `locate`/`driver` (which fire whenever the GPS reports a big enough step), and
  `navigation::frame_route`, the autofit that frames a route the moment it
  solves. That last one is the test of the rule: it is a *camera* move, which is
  why its own caller already refuses to run it in Follow.

  Getting the default the other way round would be worse than it looks. An
  announce-only-where-asked rule fails silently — a new jump site simply never
  makes a sound, and nobody files that. Announce-by-default fails loudly, at the
  one place a reviewer is looking.
