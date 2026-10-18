---
topic: avatar-locomotion
triggers: [avatar animation, fly animation, flying, jump, rig, facing, strafe, turn, WASD, gait, walk clip, systems/player/avatar.rs, systems::locomotion, simulator/src/state.rs, facing_yaw, advance_facing, display_yaw, update_avatar_model_pose, protocol 12, ClientIntent.flying, ClipGait, CLIP_ORDER]
updated: 2026-10-18
---

# The avatar clip set has no flight

Every avatar plays the clips the MakeHuman pack carries, cut from motion capture
(`makehuman-avatars.md` has the takes and how they were cut). The list is
complete:

```
idle, walk, run, wave, interact, roll, punch, kick, faint, sit
```

No jump, no fall, no hover, no swim — nothing airborne at all. A new one is a
cut in `human-pack` from a take that has it (CMU has jumps; neither database has
flight or swimming), retargeted by the same code, so adding a gait is a pack
rebuild, not new client code.

So flight is posed, not animated: `avatar::update_avatar_model_pose` pitches the
model holders forward about chest height while the avatar is flying *and* moving,
and stands them up again to hover or land. It plays `Idle` throughout. This is
also why hovering and gliding look different at all — the clip is the same one.
The lean goes on the **holders**, never on the avatar entity: that transform is
the authoritative position and yaw, and a pose written into it is a position the
sim never agreed to.

## Flight is explicit on the wire (protocol 12)

`ClientIntent::flying` says whether the avatar is flying. The vertical keys (`R`
up — it was `E` until `E` became interact — and `Q` down) do nothing unless it
is, and `SimWorld::step` falls an avatar that is not. Before the flag the sim
could not tell a flier from anyone else and applied no gravity at all, so
switching flight off in mid-air left the avatar hanging where it was, forever.
The fall and climb rates the sim, the prediction and the offline path integrate
are one module now, `cartopolis_core::motion` (`avatar-game-feel.md` says why
they must stay one).

The teleport arrival added 2026-08-25 is posed by the same system for a related
reason — see [flight-at-speed.md](flight-at-speed.md), which also has why fast
flight juddered and why none of the causes were in the flight controls.

**The jump added in 2026-08 is posed for the same reason**, at a much smaller
angle (`JUMP_TUCK`, ~11°) and driven by the *signed* vertical velocity, so the
rise and the fall do not look identical — which is the give-away that an
"airborne" pose is really one frozen frame.

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
