---
topic: avatar-game-feel
triggers: [avatar feels sluggish, foot slide, ice skating, walk speed, sprint, gait, jump, gravity, camera collision, emote, interact, footsteps, audio, alsa, wish magnitude, throttle]
updated: 2026-08-09
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
cargo check -p cartopolis --no-default-features --features solo   # the agent container, CI
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

## Keys that moved

`E` was the flight climb key and is now **interact** — it is the near-universal
"use" binding and a climb key is the cheaper of the two to move. Climb is `R`.
`Space` is **jump** on the press and **take off** if held past 0.25 s; a jump that
waits to see whether a hold is coming is not a jump. `F` remains the unambiguous
flight toggle. `Shift` sprints and `Alt` strolls — both were free in Follow.
