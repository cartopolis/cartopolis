---
topic: avatar-prediction-smoothing
triggers: [walking is jerky, movement wobble, prediction jitter, avatar rubber-bands, follow camera lags, avatar off screen, snap threshold, reconciliation, extrapolation, snapshot gap, nagle, nodelay, udp, teleport sound, teleport animation, arrival, terrain lift, systems/player/avatar.rs, systems/player/free_camera.rs, systems/net/network.rs, core/src/motion.rs, cartopolis_core::motion, systems::interiors, systems/nav/geo_nav.rs, systems::diagnostics, simulator/src/lib.rs, NetPredictionHealth, ONLINE_RECONCILE_RATE, ONLINE_MAX_EXTRAPOLATION, online_snap_error, FOLLOW_LEAD_MAX, airborne_lift]
updated: 2026-11-14
---

# Why the online avatar moves smoothly, and what that rests on

The online avatar is drawn where the client *predicts* it, eased toward what the
sim last confirmed. Getting that smooth took four reports (2026-08-25, -08-26,
-09-11, -10-08), all filed against avatar **flight**, which reached 640 m/s and
made every speed-proportional fault 350× bigger than at a walk. Flight was
removed on 2026-11-14 (`retired/avatar-flight.md`); the avatar now never goes
faster than `motion::MAX_SPEED` (8 m/s). The fixes stay, because each one is
the right shape at any speed and section 6 was measured at a walk too. This was
`flight-at-speed.md`; the flight-only parts are in the retired note.

## 1. The snap threshold is a duration, not a distance

Past `online_snap_error`, `smooth_online_avatar_display` hard-sets the display
to authority instead of easing (a teleport, a server correction). It was a flat
8 m. Reconciliation's own lag is proportional to speed, so a fixed distance is
a speed-dependent *time*: at flight speed the ordinary steady state crossed it,
and the display alternated between easing and snapping from frame to frame —
the judder, with nothing wrong. Now `max(8 m, speed × ONLINE_SNAP_SECONDS)`.
At today's speeds the floor binds; keep the rule a duration so the next fast
thing (a vehicle, a ride) does not bring the judder back.

**A threshold in metres on a quantity that scales with speed is a threshold in
seconds that nobody wrote down.** `driver::GPS_TELEPORT_M` is the other side of
the coin and is a genuine distance, deliberately — check which kind you have
before tuning either.

## 2. The follow camera leads by the velocity

A first-order lerp at rate `k` trails a target moving at `v` by `v / k` metres.
`free_camera` leads the rig by exactly that (`avatar_state.velocity /
FOLLOW_RATE`, capped at `FOLLOW_LEAD_MAX`), which cancels the lag at any speed
instead of stiffening the ease — a rate high enough for flight made a walk feel
welded to the camera. Only the **eye** leads; `look_at` still aims at the
avatar. The cap is for *discontinuities* (a teleport, a snap, a session ending
can leave a velocity that never happened), and `AvatarState::velocity` is zeroed
when Follow is left for the same reason. Driver mode is exempt: its rig aims
ahead by its own amount (`CameraState::drive_lookahead`).

## 3. The client believes itself for half a second

The reconcile target is the last snapshot extrapolated by its own age, capped at
`ONLINE_MAX_EXTRAPOLATION`. Past the cap the target stands still and the ease
brakes the avatar to a halt — deliberate for a dead link. The cap was 0.15 s and
two ordinary events beat it several times a minute on a phone: cellular
inter-arrival spikes (one ordered stream carries chat, prims and voice too), and
**our own frame** — `apply_network_snapshot` drains once per frame, so a 200 ms
hitch ages every queued snapshot by 200 ms. Now 0.5 s, still under
`ONLINE_SNAP_SECONDS` so a server that really disagrees surfaces as divergence
and then a snap.

Waiting longer is safe because the extrapolation runs on `predicted_velocity` —
the client's own live input — not on anything the snapshot carried.
`AvatarState::snapshot_age` is stored uncapped and clamped only where it
extrapolates (`avatar::extrapolated_target`), so the diagnostics can still say
how late a snapshot really was.

## 4. TCP_NODELAY on every socket

Nagle was on everywhere: intents left in clumps and snapshots arrived in clumps,
which is the late-snapshot pattern of section 3. Invisible on loopback. Four
sites, all best-effort: `network::set_nodelay` for both client carriers, the
simulator's accept loop, and `axum::serve::ListenerExt::tap_io` for `/ws`.

## 5. Why not UDP

Nothing in sections 3 or 4 is caused by ordering or retransmission; Nagle is a
socket option. A browser cannot open a UDP socket, so it would be a *third*
carrier beside TCP and WebSocket (`net::handle_connection` is generic over
`FrameTransport` to avoid a second copy), and the connection carries sign-in,
chat, prims and voice, which want ordering — a protocol project. Revisit only if
`NetPredictionHealth` shows large gaps *with* nodelay on and a generous window.

## 6. The correction acts on a low-passed error

Reported 2026-09-11 for flight **and walking**: a fault at both ends of a 350×
speed range is in the part of the loop that multiplies by speed. Snapshots do
not land on a metronome; a stiff correction (one pole at 9/s) turned each late
arrival into motion. `avatar::reconcile_display` now filters the error
(`ONLINE_ERROR_FILTER_RATE` 7/s over `ONLINE_RECONCILE_RATE` 3.5/s — damping
~0.7, corner ~0.8 Hz) and caps the correction at a quarter of the avatar's pace
(`ONLINE_CORRECTION_FRACTION`, floor `ONLINE_CORRECTION_FLOOR` 2 m/s).

Measured 2026-09-11, steady journey, sim in full agreement, snapshots ±40 ms
around 80 ms latency — worst per-frame travel deviation: **30.5 %** old, **2.3 %**
new, the same at every speed. The filter does it alone: arrival noise opens 6 cm
at a walk against a 57 cm knee. The floor sets how fast a real desync closes
(3 m in 1.4 s). Tests: `arrival_jitter_does_not_reach_the_picture`,
`the_old_stiff_correction_is_what_the_wobble_was`. A snap and a stall bypass
both, because there the answer must *arrive*.

The prediction reads the **predicted** height (display minus `terrain_lift` and
`floor_lift`), not the 20 Hz authoritative one, which is a staircase; it also
fixed a phantom fall predicted on every upper storey.

## 7. The ground under an avatar high up is not under its feet

The sim is flat; relief is added on the display (`terrain_lift`, `floor_lift`).
`avatar::airborne_lift` follows the ground exactly up to 1.5 m (a jump's apex is
~1.1 m) and eases above that, so a body high over the city does not drop two
metres per canal. With flight gone only a fall from a placement gets there. The
storey state machine is held above 2 m (`FLOOR_FREEZE_ABOVE_M`) so it cannot
drop the floor into a tunnel cut passed over in the air.

## The instrument: `NetPredictionHealth`

Snapshots applied, the last and worst gap, and the share of predicted frames
**parked** past the extrapolation window — in the shareable diagnostics report
(`systems::dev::diagnostics`), because `logcat` needs a cable. Read it as a
fork: `parked` near zero during a stutter means it is not the network (look at
`perf:`); above a percent or two means it is.

## Teleport arrivals

A teleport has no travel in it, so nothing on screen distinguishes it from the
map being redrawn around a figure that never moved; the arrival says a body went
somewhere.

- It **scales** (`avatar::body_pose`, `teleport_scale`), it does not dissolve:
  fading opaque bodies means a second material per part and a place in the
  transparent sort, for 0.45 s. A scale is one transform on a holder
  `update_avatar_model_pose` already writes.
- It is published as a **sequence counter** (`AvatarState::teleport_sequence`),
  so two jumps to one place are two events.
- Bumped in `geo_nav::apply_teleport`, not at call sites — a teleport queued
  before the anchor exists would otherwise whoosh over an empty map.
- **The default is to announce.** `Teleport::silent` is set where a whoosh would
  lie about who acted: the startup restore, a scripted `--shot`, the globe
  re-anchor, the device-fix jumps in `locate`/`driver`, and
  `navigation::frame_route`. Announce-only-where-asked fails silently (a new
  site never makes a sound); announce-by-default fails where a reviewer looks.
