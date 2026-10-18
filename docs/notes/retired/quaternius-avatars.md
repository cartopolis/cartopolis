---
topic: retired/quaternius-avatars
triggers: [Quaternius, Ultimate Modular, UAL, UAL1_Standard, CHARACTER_STYLES, SlotMask, resolve_avatar_holders, sync_avatar_animations, bind_external_animations, is_weapon_node, play_avatar_animation_when_ready, respawn_avatar_on_appearance_change, sit_pose, avatar/characters, mixed avatar, slot chooser]
updated: 2026-10-18
---

# Retired: the Quaternius characters

From 2026-07-22 to 2026-10-18 every avatar was a Quaternius "Ultimate Modular"
character (CC0 when obtained): stylised, low-poly, and the only choice a player
had was a costume. They were replaced by MakeHuman bodies the client builds
from sliders, animated with motion capture (`makehuman-avatars.md`), and the
62 MB of glTFs under `assets/models/avatar/characters/` were deleted with the
code that drew them. What follows is what they were, so nobody re-derives it.

## Do not bring them back

Quaternius moved off CC0 on 2026-08-28 to the "Quaternius Asset License v1.0",
which permits using an asset in a product and **forbids redistributing the
files themselves** — which is what a public repository of `.gltf` files is. The
copies that were here predated the change and were covered by its "not
retroactive" clause, but anything downloaded from Quaternius since is not, and
must not be added to `assets/` (`.gitignore` still lists the old hand-fetched
folders for that reason).

## One character file per slot, phase-locked

Each style was a self-contained glTF (embedded buffers, solid-colour materials)
carrying four slot meshes — `*_Head` (face, hair, headwear), `*_Body`, `*_Legs`,
`*_Feet` — an optional accessory, and in some styles a weapon, which was hidden
unconditionally. `AvatarAppearance` held a style per slot; `resolve_avatar_holders`
made one child **holder** per distinct file, each with a `SlotMask` hiding the
meshes outside it, and `sync_avatar_animations` copied the first holder's seek
times onto the rest every frame so a head from one style and a body from another
moved as one body.

Every file embedded the **same 24 clips in the same order**, which is what made
mixing possible at all:

```
Death, Gun_Shoot, HitRecieve, HitRecieve_2, Idle, Idle_Gun, Idle_Gun_Pointing,
Idle_Gun_Shoot, Idle_Neutral, Idle_Sword, Interact, Kick_Left, Kick_Right,
Punch_Left, Punch_Right, Roll, Run, Run_Back, Run_Left, Run_Right, Run_Shoot,
Sword_Slash, Walk, Wave
```

No jump, no fall, no flight, no sit. The Universal Animation Library could not
lend one: it was authored for the Superhero base bodies, whose bones carry
Unreal-style names (`neck_01`, `upperarm_l`), while the Modular characters used
Quaternius's own (`Neck`, `UpperArm.L`), and Bevy hashes animation targets from
the bone **name path**, so not one channel bound.

## The direction before that (until 2026-07-22)

Base bodies plus outfit glTFs, with every clip bound from `UAL1_Standard.glb`.
The outfits were not uniform — one needed the body under it, one showed its bare
back through the clothes, one pushed the body's torso out through its skin — and
the head was welded into the body's full-body mesh, so it could not be swapped.
Per-slot meshes in self-contained files were what removed that.

## The synthesised sit

With no sit clip in the set, `systems::sit_pose` built one at spawn time from
eleven constant curves (pelvis and three bones per leg), **solved** from the
rig — hips at seat height, ankles one thigh-length ahead, knee from the two-link
solution — because the men's and women's rigs differed (the women's legs ~5 cm
longer). Its traps: the pelvis carried a 27° yaw in every clip that the torso
cancelled, so an identity pelvis turned the shoulders; and the feet hung off
`Root`, not the shins, so the pose had to place them. The MakeHuman pack carries
a captured sit instead (`sitting-down.md`).
