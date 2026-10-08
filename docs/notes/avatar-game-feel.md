---
topic: avatar-game-feel
triggers: [avatar feels sluggish, camera feels awkward, recentre, auto-rotate camera, camera behind avatar, banking, framing, foot slide, ice skating, walk speed, sprint, run too fast, gait, stride phase, jump, gravity, camera collision, emote, interact, footsteps, footstep sound, audio, alsa, wish magnitude, throttle]
updated: 2026-10-08
---

# What made the avatar feel wrong, and what the fix rests on

Measured 2026-08-09 while reworking Follow mode. The complaint was "sluggish and
unrealistic"; the input path turned out to be fine, and every actual cause was
somewhere else. Recording it because four of the five would be re-introduced by
anyone tuning this from first principles.

## The avatar was never laggy — the feet were wrong

Input reaches the display in one frame: a key press sets `predicted_velocity` and
`smooth_online_avatar_display` integrates it the same frame. What read as
sluggishness was **foot-slide**, and it was arithmetic:

- `WALK_SPEED` was **8.0 m/s** and it was the only speed. That is a world-class
  sprint (Bolt's average over 100 m is 10.4 m/s), used for a stroll down a canal.
- The `Run` clip is **0.8 s per cycle** = 2.5 steps/s ≈ **4 m/s** of real stride,
  and it was played at a hard-coded `set_speed(1.0)`.

So the model's feet moved at half the speed of the ground, permanently. Clip
durations, for anyone re-deriving the constants
(`jq '.animations[].name' assets/models/avatar/characters/Man_Casual.gltf`, then
the sampler `max` inputs):

```
Idle 1.667 s   Run 0.800 s   Walk 1.333 s   Wave 1.667 s   Interact 1.267 s   Roll 1.333 s
```

`Walk` is clip **22** and was never loaded at all; the client only ever knew
Idle (4) and Run (16).

## The throttle is a magnitude, which is why gaits needed no protocol change

`SimWorld::apply_intent` derives speed from the **magnitude** of the wish vector,
not from a speed field. So walk / jog / sprint / flight are four magnitudes the
client picks, and the sim needs no concept of a gait. Both ends call
`cartopolis_core::motion::sim_speed_for_wish`, so prediction cannot drift from
authority by transcription.

**This is the lever to reach for first** for anything that changes how fast the
avatar moves. It cost nothing on the wire. Jump and emotes did *not* fit through
it (see below) and cost protocol 13.

Altitude-scaled flight (2026-08-21) did not fit through it either, for the
mirror reason: the magnitude encoding **saturates at `MAX_SPEED`**, so a wish
can never ask for the 640 m/s a high flight is worth. It rides
`motion::fly_altitude_factor` instead — a multiplier derived from the avatar's
`y`, which is state both ends already share — applied at *integration* in all
three motion sites. Nothing changed on the wire, but the sim's `step` changed,
so **the deployed simulator must be rebuilt** or an online client predicts fast
flight the authority refuses and rubber-bands above ~120 m.

## `cartopolis_core::motion` exists because three copies could not be kept in step

The fall/climb/speed constants were duplicated in `cartopolis_simulator::state`,
`network::send_network_intent` (prediction) and `avatar::handle_avatar_movement`
(offline), each with a comment asking the next person to update the others. They
are one module now. **Do not re-inline them**: a mismatch between the first two
is invisible except as rubber-banding, and between the first and third it means
the same key produces a different world depending on how you launched.

## Jump could not be client-side, and gravity was not an acceleration

`SimWorld::step` clamps `y >= 0` and falls anything not flying, so a purely
predicted jump is reconciled straight into the floor — jump needs the sim, hence
protocol 13 (`ClientIntent::jump`, granted only from the ground so holding the
bit cannot climb). While there, the descent stopped being a **constant 12 m/s**
and became a real arc: it had no acceleration in it at all, so stepping off a
kerb and falling off a tower began identically.

`GRAVITY` is 22 m/s², well above Earth's. Real gravity gives a jump that clears a
useful height only by hanging in the air for over a second, which reads as the
moon.

## The camera collides in 2D, and only low down

The follow boom is swept against `BuildingColliders::segment_fraction`. The
footprints **carry no height**, so the test is gated on the camera being below
`CAMERA_COLLIDE_MAX_Y` (15 m): above a plausible eaves line a 2D hit means "there
is a building over there", not "the view is blocked", and acting on it reels the
camera in every time the avatar walks past a block. If building heights ever
reach the collider index, that gate is the thing to delete.

## Two tile layers were being decoded and dropped

`pois` (467 features in the Groningen z14 fixture) and `addresses` (6,211) were
listed as used for "nothing" in `vector-tile-unused-layers.md`. They are read now
by `systems::places`, out of the *same* `TileData` parse `map_geometry` already
does — no extra request, no extra decode. That is what the proximity prompt and
the house-number readout are built from.

Two things found while doing it:

- **A tile carries a buffer**, so a legitimately-decoded POI can sit a few metres
  *outside* its own cell (the fixture has one at x = −2.98 m in a 611 m cell).
  Asserting containment fails; clamping would be the wrong fix, because the
  coordinate is right and the cell simply is not all the tile holds.
- **House numbers are not all strings.** `addresses.housenumber` arrives as any
  of MVT's numeric value types as well as `String`; reading only `Value::String`
  silently drops most of them.

## Audio is a cargo feature because of a system header

`bevy_audio` pulls `alsa-sys`, whose build script hard-fails without the ALSA
**development headers** (`libasound2-dev`). That is a new system prerequisite for
every Linux build, including headless ones that will never open a device. So it
is the `audio` feature — in `default`, off for hosts that can't build it:

```bash
cargo check -p cartopolis --no-default-features   # the agent container, CI
```

The container has no root and cannot install the headers, so **the playback
systems in `systems::soundscape` have never been compiled there**. The synthesis
and the volume/step-rate maths were deliberately split out as pure functions so
they still compile and are still tested without the backend; the ECS wiring is
the part that needs a host with the headers.

Everything it plays is synthesised into WAV bytes at startup. That is not only to
avoid the LFS trap (`.gitattributes` tracks audio by extension, and a missing LFS
object is the documented silent-failure mode) — it is also why the loop had to be
cross-faded into itself: a click every two seconds is the most noticeable
artefact a looping ambience can have.

## The ladder was still one rung too high (2026-08-21)

Reported as "movement is not fluent, it should not be stuttery; when walking
slowly it should use a walk animation, not run; the run is a bit too fast". All
three are the same number.

`JOG_SPEED` (the **unmodified** gait — press W and nothing else) was 4.2 m/s,
and `GAIT_CLIP_SWITCH` was 2.4. So the everyday gait was above the switch:

- the `Walk` clip loaded in the fix above was reachable **only by holding Alt**,
  which is not a key a desktop compositor reliably delivers — several claim it
  for window drag or menu focus before winit sees it. In practice the walk clip
  had no user;
- and a start from rest crossed idle → walk → run inside 0.23 s, paying **two**
  180 ms `GAIT_FADE` cross-fades to travel in a straight line. Stopping paid two
  more. That is the "stutter" — it is at every start and stop, which is where
  the eye is.

The ladder is now stroll 1.1 (Alt) / **walk 1.8 (default)** / run 6.0 (Shift),
with the switch at 3.0 ± 0.5. A walk never reaches the switch, so an ordinary
start costs one transition; the second only happens when Shift is actually held.
6 m/s also puts the run clip at 1.5× instead of on `CLIP_SPEED_MAX` — the old
8 m/s sprint was clamped, i.e. sliding its feet, which is the very thing the
rate-matching was introduced to remove. `locomotion`'s suite pins both relations
(`the_unmodified_gait_is_a_walk`, `no_gait_plays_its_clip_at_the_clamp`), because
they are relations *between* constants that live in different sections of one
file and either can be tuned alone.

### A gait change has to carry the stride phase

The remaining transition is still a splice unless somebody says otherwise:
`AnimationTransitions::play` starts the incoming clip at seek time **zero**,
which in a locomotion clip is feet-together. Cross-fading a mid-stride pose into
that runs the legs through a position they never occupy, for 180 ms, every time.

`avatar::apply_pose` now reads the outgoing clip's seek time, divides by its own
cycle length and seeks the incoming clip to the same *fraction* of its cycle.
This is only sound because both clips are exactly **two footfalls** long
(`WALK_CLIP_SECS` 1.333, `RUN_CLIP_SECS` 0.8) — the normalized phase means the
same thing in each, so left foot fades into left foot. Idle, flight and the
emotes have no stride and are still started from zero, which is correct: idle
*is* feet-together.

### The footsteps were a metronome

"The related sound is a bit cheap" was two separate things, and the timbre was
the smaller of them.

- **Pattern.** Two samples, strictly alternated, at an exactly periodic rate.
  The ear locks onto that in about four steps — the earlier note claiming it
  "stops noticing at two" was wrong. Four variants now, with a ±6 % playback-rate
  and ±12 % level nudge per step, derived deterministically from the stride
  phase (`step_wobble`) so the cycle never lines up with the sample cycle.
- **Timing.** The stride counter was reset to `0.0` on firing rather than having
  1.0 subtracted, so every footfall was quantised to a frame boundary and up to
  a whole frame of phase was discarded each step — a few percent of the interval
  at a run, varying with the frame time. That is heard as sloppiness, not as
  variation. The remainder is carried now; a hitch past two whole strides still
  resets rather than paying back a burst.
- **Timbre.** One filtered noise burst with one envelope is a *tap*. A footfall
  is an impact, the weight arriving a few milliseconds behind it, and the sole
  still making noise as the body rolls over it. `synth::footstep` is three parts
  now — click, body (two decaying sines at 82/131 Hz, below anything the noise
  carries, so it costs nothing in the zero-crossing budget), and a scuff delayed
  18 ms at a quarter level. Two new tests: the clip must have energy under
  200 Hz, and must not stop dead in its last third.

## Keys that moved

`E` was the flight climb key and is now **interact** — it is the near-universal
"use" binding and a climb key is the cheaper of the two to move. Climb is `R`.
`Space` is **jump** on the press and **take off** if held past 0.25 s; a jump that
waits to see whether a hold is coming is not a jump. `F` remains the unambiguous
flight toggle. `Shift` runs and `Alt` strolls — both were free in Follow. Note
neither is needed for the ordinary case: the unmodified gait is the walk.

## The follow camera, against the published rules (2026-10-08)

Reported as "movement and camera position feel awkward". Checked against
Cinemachine's Third Person Follow and its recentring, Unreal's SpringArm, and
John Nesky's "50 Game Camera Mistakes" (GDC 2014, the Journey camera). The rig
already did most of what those ask — frame-rate-independent eases, collision in
fast and out slow, a speed-coupled FOV, a landing dip. Four things it did not:

- **It never came round behind the avatar.** Facing follows travel, and the
  camera stayed wherever it was last dragged, so a turn left the avatar walking
  across the screen until somebody dragged it back. `free_camera::recentre_turn`
  swings it after **1 s** without a hand on it (Cinemachine's wait), ramped in
  over 0.6 s, at 1.2/s on foot and 2.5/s in flight, scaled by pace, and faded to
  nothing for travel toward the lens — or pressing S spins the view round.
- **Recentring alone would make A walk a circle** — the camera turns, the keys
  are read against the camera, the avatar turns to follow (Nesky #18–19). So
  the camera totals what it turned *by itself* (`CameraState::auto_yaw`) and
  `avatar::steer_azimuth` takes that back out of the frame the held input is
  read in, until the input is released or changes by ~20°. A hand on the camera
  still steers. This also retires the "holding A walks a circle" caveat of
  follow-and-rotate (`avatar-locomotion.md`), which accrues into the same total.
- **It rode every jump** (Nesky #44). `step_pivot` holds the pivot height while
  on foot and off the ground, follows a fall once it drops 0.4 m below, and
  eases back on landing.
- **The avatar was dead centre and the boom was the same at 640 m/s as at a
  walk.** The aim rises 8 % of the boom above the chest, which puts the avatar
  in the lower middle; the boom stretches up to 1.9× between a run and 80 m/s
  (`pullback_for_speed`) — distance and FOV as one curve on speed (Nesky
  #14–16). `distance` keeps the wheel's choice; the stretch is applied on top.

Flight also **banks** now: roll ∝ the facing's turn rate, capped at ~35°
(`avatar::FLIGHT_BANK_*`). Not replicated, like the lean.

None of this has been felt on a device yet; every number above is a starting
point taken from the sources, not a tuning.
