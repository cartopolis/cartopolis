---
topic: retired/avatar-flight
triggers: ["avatar flight", "fly the avatar", "flying avatar", "take off", "toggle flight", is_flying, fly_vertical, FLY_SPEED, fly_altitude_factor, step_fly_vertical, ClientIntent.flying, flight-at-speed, core/src/motion.rs, systems/player/avatar.rs, systems/gui/map_osd.rs]
updated: 2026-11-14
---

# Retired: avatar flight

From protocol 12 (2026-07) to 2026-11-14 the avatar could fly. The owner removed
it on 2026-11-14: **flying is unrealistic**, and the avatar is a person — it
walks, runs and jumps (and later swims). The camera's own modes are untouched:
the Fly (Free) camera, Earth, and zooming out to the globe all still fly; this
was only ever about the *body*.

## What it was

- **Keys.** `F` toggled flight; `Space` jumped on the press and took off if held
  past 0.25 s (`FLIGHT_HOLD_SECS`), a tap in the air landed; `R`/`Q` climbed and
  descended (`E` until it became interact). On a phone, a flight toggle (a
  filled dart, `Mark::Wings`) under the jump disc, and the right-edge zoom pair
  turned into a climb pair while flying.
- **Speeds.** 32 m/s horizontal (`motion::MAX_SPEED`, a wish magnitude of 4),
  20 m/s climb ramped at 40 m/s² (`step_fly_vertical`), and both multiplied by
  `fly_altitude_factor` — altitude / 120 m, up to 20×, so 640 m/s at ~2.4 km.
  Accel/decel 70/80 m/s². The heading arced at 3.2 rad/s in the air
  (`advance_move_dir`) instead of taking the wish whole.
- **Pose.** No clip has flight, so it was posed: a ~60° forward lean about the
  chest while moving, a bank of up to ~35° into turns, upright to hover.
- **Sound.** The wind peaked at flight speed rather than at a run.

## What removing it changed

- **The wire did not move.** `ClientIntent::flying`, `fly_up` and `fly_down`
  are still on it — removing a field is a protocol revision — and documented as
  ignored. Clients send `false`. The sim never reads them; an old client that
  flies is fallen by the sim like anyone else.
- **`motion::MAX_SPEED` came down from 32 to 8 m/s** (= `SPEED_SCALE`, a wish
  magnitude of 1). The encoding did not change, so an old client's walk and run
  mean what they did, and its flight-speed wish clamps to 8 m/s. Nothing else
  drove the avatar faster through intents: every gait and the on-screen stick
  top out at a 6 m/s run, agent and route walks use gaits, and a GPS follow
  carries the avatar outside the intent (`carried_speed`, `Teleport`).
- **The follow camera's pull-back with speed** (boom up to 1.9× between a run
  and 80 m/s) went with it — nothing on foot reached it.

## Lessons that outlived it

Flight is why the online prediction is shaped the way it is: a snap threshold
that is a duration, the camera leading by the velocity, a half-second
extrapolation window, `TCP_NODELAY`, a low-passed correction, a lift that
ignores the ground under a body high up. Those still hold and are in
[../avatar-prediction-smoothing.md](../avatar-prediction-smoothing.md). Two
that were flight's alone:

- **A vertical rate on a boolean key must ramp.** The climb was a step on the
  key edge, which multiplied by the altitude factor was hundreds of metres per
  second of discontinuity. The ramp was the one part of the vertical model the
  sim had to *reproduce* rather than receive, since the wire carried two bits.
- **"Fly faster" meant time-to-altitude, not top speed.** At 12 m/s it took
  ~40 s to climb into the part of the ramp worth being in; the cap was left at
  20× because past ~640 m/s the map streams for a viewpoint that outruns it.

## If it is asked for again

It was removed on purpose, by the owner, as unrealistic; propose it as a
decision, not as a fix. A vehicle or a ride that moves the avatar fast would
need `MAX_SPEED` raised and would meet every lesson above again.
