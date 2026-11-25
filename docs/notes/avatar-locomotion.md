---
topic: avatar-locomotion
triggers: [avatar animation, jump, rig, facing, strafe, turn, WASD, gait, walk clip, systems/player/avatar.rs, systems::locomotion, simulator/src/state.rs, facing_yaw, advance_facing, display_yaw, update_avatar_model_pose, ClientIntent.flying, ClipGait, CLIP_ORDER, "avatar flight", flying, swim, swimming, swim_surface_at, ground_or_water, TREAD_DEPTH, "--clip swim"]
updated: 2026-11-15
---

# The avatar walks, runs and jumps — and does not fly

The avatar is a person: it strolls, walks, runs, jumps and swims. **It does
not fly.** Avatar flight existed from protocol 12 until the
owner removed it on 2026-11-14 as unrealistic; what it was and what it taught
are in [retired/avatar-flight.md](retired/avatar-flight.md). Since 2026-11-25
the Fly camera does not fly either; the avatar goes fast **in a car**
([driving.md](driving.md)). Earth and the zoom out to the globe still move the
camera, and none of that is about the avatar.

check: `! grep -rqw is_flying crates/cartopolis/src crates/simulator/src`

`ClientIntent::flying`, `fly_up` and `fly_down` are still on the wire, ignored
by the sim and sent `false` by clients: removing them would be a protocol
revision. The sim falls every avatar (`SimWorld::step` runs `motion::step_fall`
for all of them), so an old client that still flies is simply brought down, and
`motion::MAX_SPEED` (8 m/s since the removal) clamps its flight-speed wish.

## The clip set has no jump

Every avatar plays the clips the MakeHuman pack carries, cut from motion capture
(`makehuman-avatars.md` has the takes and how they were cut). The list is
complete:

```
idle, walk, run, wave, interact, roll, punch, kick, faint, sit, swim,
handshake, highfive, throw, climb, dangle
```

The greetings and the throw were appended (2026-11-15) so no earlier index in
`human_avatar::CLIP_ORDER` moved; the emotes are therefore not one run of
it, and `human_avatar::EMOTE_CLIPS` says where each sits. The greetings are
made with somebody (`paired-gestures.md`). `climb` and `dangle` came after
them the same day: climbing onto low walls and sitting on edges with the legs
hanging, a display floor and a holder lift like swimming
(`climbing-and-edges.md`).

No jump and no fall. A new one is a cut in `human-pack` from a take that has it
(CMU has jumps), retargeted by the same code, so adding a gait is a pack
rebuild, not new client code.

So a jump is **posed**, not animated: `avatar::update_avatar_model_pose` pitches
the model holders about chest height (`JUMP_TUCK`, ~11°) driven by the *signed*
vertical velocity, so the rise and the fall do not look identical — the
give-away that an "airborne" pose is one frozen frame. It plays `Idle`
throughout. The tuck goes on the **holders**, never on the avatar entity: that
transform is the authoritative position and yaw, and a pose written into it is
a position the sim never agreed to. The teleport arrival (a scale out of a thin
column, `avatar::body_pose`) is written by the same system for the same reason
([avatar-prediction-smoothing.md](avatar-prediction-smoothing.md)).

## Swimming is a ground under the water and a pose on the holders (2026-11-14)

Asked for by the owner: "an avatar in water must swim, not walk over it". It
walked over it because `TerrainField::ground_at` over a canal is the *land* the
quays are stood on (`quay-walls.md`), i.e. street height.

- **Where.** `TerrainField::swim_surface_at`: strictly inside a registered water
  body (`WaterLevelField::level_at`, not `water_surface_at`'s three-metre join
  tolerance), and not on a bridge deck at or above the surface. A register's
  ring can be a metre wider than the drawn water beside a lawn, so a swim can
  start that much early there.
- **How deep.** `avatar::ground_or_water` makes the ground in water the surface
  less `TREAD_DEPTH` (1.5 m), the feet of someone treading water: the idle,
  head out to the chin. Like the terrain it is a display lift on every viewer's
  own map; the sim knows nothing of water and nothing went on the wire. The
  lift moves at most `WATER_SINK_RATE`/`WATER_CLIMB_RATE` while in or entering
  or leaving water, so off a quay is a drop and out is a climb (the `climb`
  clip is for walls, and quay walls are not colliders).
- **The stroke.** The `swim` clip is CMU 125_01's breaststroke, captured lying
  on a stool, so it goes nowhere and its pace is chosen (`SWIM_CLIP_SPEED`).
  `mocap::heading_of` takes the spine as a lying body's heading. Grounded by its
  lowest ankle the head is 0.16 m over the feet, so `avatar::swim_lie` lifts the
  holders by `SWIM_LIFT` to bring the head to the surface and tips them 0.3 rad
  feet-down about the head: level, the kick drew the shins up through the water
  at every stroke (the stool's air, not water). Eased over `SWIM_LIE_SECS`, with
  the clip's `SWIM_FADE`.
- **Pace.** Every gait swims `SWIM_SPEED` (0.9 m/s), a run `SWIM_FAST_SPEED`
  (1.4), built and shed at `SWIM_ACCEL`/`SWIM_DECEL`, far softer than on foot.
  No jump in water; no footsteps.
- **Splashes and sound** (2026-11-16, `water-splashes.md`): going in splashes
  by the entry speed (the drop from the frame before, and the pace), each
  stroke throws a ripple and a sound at `SWIM_STROKE_PHASE` of the clip read
  off the animation player, treading water rings, and a lapping loop replaces
  the footsteps; the landing thud is not played in water. Other swimmers are
  splashed from each viewer's own map. A `--clip walk` path that goes into the
  water swims there, which is how the drop off a quay is filmed.

`--clip swim` photographs the stroke; the Noorderhaven at 53.22005,6.5587 is in
the water (the skill's canal-side point at 53.2194,6.5665 is not).

## The feet are placed on the ground under each of them (2026-11-16)

The clips are flat-floor captures and the body stands on the ground under its
origin, so on a slope a planted foot was up to 14 cm in or over the drawn ground (a park's
rampart, 34 %).
Each leg is now solved over the clip so its foot lands on its own ground, the
pelvis lowered for a planted foot below the body — on the ground only, not
swimming, sitting, climbing or in an emote. [foot-placement.md](foot-placement.md)
has where it was measured, the rule for the pelvis, and what it does not do.

## The clips are played at the speed the avatar is actually moving

Since 2026-08-09 the locomotion clip is chosen by ground speed and its playback
rate is `ground_speed / clip_reference_speed` (`systems::locomotion`). Each
captured walk and run carries its own reference speed — the travel the pack
subtracted when it cut the loop — and its cycle (`ClipGait`, read from the pack
in `human_avatar`): the walk is 1.65 m/s on 0.9 s, the run 3.46 m/s on 0.73 s.
`ClipGait::CHARACTER_WALK`/`CHARACTER_RUN` (the Quaternius clips' 1.333 s and
0.8 s) are only the fallback for a pack without them. `avatar-game-feel.md` has
the reasoning.

## Facing follows travel, and that is deliberate

The avatar turns toward its **wish direction**, so A/D turn it a quarter turn and
S brings it about; only the turn *rate* is smoothed
(`avatar::advance_facing`). `AvatarState::facing_yaw` is the single facing: the
render transform, the replicated `display_yaw` and the follow-and-rotate camera
all read it, so there is no second heading to fall out of step with it. It used
to be pinned to camera-forward in every frame, which made A/D a sideways
moonwalk. Strafe clips are the other way this could have gone — the Quaternius
set had them — and they were not taken: strafing everywhere reads as a shooter, not as a
person walking through a city, and it makes the replicated `display_yaw`
carry the camera's orientation rather than the avatar's.

With follow-and-rotate armed (the GPS button, pressed while in Follow) the camera
chases the avatar's heading while the input stays camera-relative, which used to
make **holding A walk a circle**. Since 2026-10-08 the held input is read against
the camera minus what it turned by itself (`avatar::steer_azimuth`), so it walks
straight and the camera comes round behind it — `avatar-game-feel.md` has why.
