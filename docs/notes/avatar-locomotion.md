---
topic: avatar-locomotion
triggers: [avatar animation, jump, rig, facing, strafe, turn, WASD, gait, walk clip, systems/player/avatar.rs, systems::locomotion, simulator/src/state.rs, facing_yaw, advance_facing, display_yaw, update_avatar_model_pose, ClientIntent.flying, ClipGait, CLIP_ORDER, "avatar flight", flying]
updated: 2026-11-14
---

# The avatar walks, runs and jumps — and does not fly

The avatar is a person: it strolls, walks, runs and jumps (swimming is the next
gait). **It does not fly.** Avatar flight existed from protocol 12 until the
owner removed it on 2026-11-14 as unrealistic; what it was and what it taught
are in [retired/avatar-flight.md](retired/avatar-flight.md). The camera still
flies — the Fly (Free) camera, Earth, the zoom out to the globe — and none of
that is about the avatar.

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
idle, walk, run, wave, interact, roll, punch, kick, faint, sit
```

No jump, no fall, no swim. A new one is a cut in `human-pack` from a take that
has it (CMU has jumps; neither database has swimming), retargeted by the same
code, so adding a gait is a pack rebuild, not new client code.

So a jump is **posed**, not animated: `avatar::update_avatar_model_pose` pitches
the model holders about chest height (`JUMP_TUCK`, ~11°) driven by the *signed*
vertical velocity, so the rise and the fall do not look identical — the
give-away that an "airborne" pose is one frozen frame. It plays `Idle`
throughout. The tuck goes on the **holders**, never on the avatar entity: that
transform is the authoritative position and yaw, and a pose written into it is
a position the sim never agreed to. The teleport arrival (a scale out of a thin
column, `avatar::body_pose`) is written by the same system for the same reason
([avatar-prediction-smoothing.md](avatar-prediction-smoothing.md)).

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
