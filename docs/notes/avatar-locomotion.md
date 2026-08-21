---
topic: avatar-locomotion
triggers: [avatar animation, fly animation, flying, jump, clip index, Quaternius, UAL, rig, facing, strafe, turn, WASD, gait, walk clip]
updated: 2026-08-09
---

# The avatar clip set has no flight, and no library can lend it one

The avatars are Quaternius "Ultimate Modular" characters. Every character glTF
embeds the **same 24 clips**, and the list is complete:

```
Death, Gun_Shoot, HitRecieve, HitRecieve_2, Idle, Idle_Gun, Idle_Gun_Pointing,
Idle_Gun_Shoot, Idle_Neutral, Idle_Sword, Interact, Kick_Left, Kick_Right,
Punch_Left, Punch_Right, Roll, Run, Run_Back, Run_Left, Run_Right, Run_Shoot,
Sword_Slash, Walk, Wave
```

No jump, no fall, no hover, no swim — nothing airborne at all. (Verify with
`jq -r '.animations[].name' assets/models/avatar/characters/Man_Casual.gltf`.)

**The UAL library cannot fill the gap.** `assets/models/avatar/anims/UAL1_Standard.glb`
does have `Jump_Loop`, `Swim_Fwd_Loop` and `Swim_Idle_Loop`, and the client even
has the machinery to bind clips from a separate file onto a skeleton
(`avatar::bind_external_animations`, "Case 2"). It still can't be used here: UAL
is authored for the Superhero base bodies, whose rig uses Unreal-style bone names
(`neck_01`, `upperarm_l`, `hand_l`), while the Modular characters use Quaternius's
own (`Neck`, `UpperArm.L`, `Wrist.L`). Animation targets are hashed from the bone
**name path**, so not one channel binds. Retargeting between the two rigs, or
authoring a flight clip, is the only way to get a real one.

So flight is posed, not animated: `avatar::update_avatar_flight_pose` pitches the
model holders forward about chest height while the avatar is flying *and* moving,
and stands them up again to hover or land. It plays `Idle` throughout. This is
also why hovering and gliding look different at all — the clip is the same one.

**The jump added in 2026-08 is posed for the same reason**, at a much smaller
angle (`JUMP_TUCK`, ~11°) and driven by the *signed* vertical velocity, so the
rise and the fall do not look identical — which is the give-away that an
"airborne" pose is really one frozen frame.

Anything that adds a gait (a crouch, a swim) is picking from the 24 above, or
bringing its own asset.

## The clips are played at the speed the avatar is actually moving

Since 2026-08-09 the locomotion clip is chosen by ground speed and its playback
rate is `ground_speed / clip_reference_speed` (`systems::locomotion`). `Walk`
(clip 22) is loaded now; it never was before, so `Run` was played at every speed
and at a fixed 1.0×. The reference speeds are derived from the cycle durations —
`Walk` 1.333 s ≈ 1.35 m/s, `Run` 0.8 s ≈ 4.0 m/s — and
`docs/notes/avatar-game-feel.md` has the full table and the reasoning.

## Facing follows travel, and that is deliberate

The avatar turns toward its **wish direction**, so A/D turn it a quarter turn and
S brings it about; only the turn *rate* is smoothed
(`avatar::advance_facing`). It used to be pinned to camera-forward in every
frame, which made A/D a sideways moonwalk. The strafe clips (`Run_Left`,
`Run_Right`, `Run_Back`) are the other way this could have gone — the set has
them — and they were not taken: strafing everywhere reads as a shooter, not as a
person walking through a city, and it makes the replicated `display_yaw`
carry the camera's orientation rather than the avatar's.

Consequence worth knowing: with follow-and-rotate armed (the GPS button, pressed
while in Follow) the camera chases the avatar's heading while the input stays
camera-relative, so **holding A walks a circle**. That is inherent to the pair,
not a bug in either.
