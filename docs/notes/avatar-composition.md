---
topic: avatar-composition
triggers: [systems/player/avatar.rs, assets/models/avatar, systems/nav/geo_nav.rs, simulator/src/state.rs, core/src/motion.rs, systems::sit_pose, CHARACTER_STYLES, AvatarAppearance, SlotMask, resolve_avatar_holders, sync_avatar_animations, bind_external_animations, is_weapon_node, play_avatar_animation_when_ready, holder, GROUND_HEIGHT, AVATAR_GROUND_Y, GROUND_Y, standing height, avatar floats, mixed avatar, avatar chooser]
updated: 2026-10-17
---

# An avatar is one character file per slot, phase-locked

The avatars are Quaternius "Ultimate Modular" characters (CC0). Each style is a
**self-contained glTF** — buffers embedded as base64 data URIs, solid-colour
materials, no external textures — under `assets/models/avatar/characters/`
(`Man_<x>.gltf`, `Woman_<x>.gltf`; `avatar::CHARACTER_STYLES` is the catalog, and
adding a style is a row there plus the file). Every file embeds the **same 24
clips in the same order** (`avatar-locomotion.md` lists them), which is why the
clip indices are constants (`CHAR_IDLE` 4, `CHAR_WALK` 22, `CHAR_RUN` 16, the
emotes in `EMOTE_CLIPS`) — and why a mixed avatar can be phase-locked at all.

Each file carries four slot meshes named by suffix — `*_Head` (face, hair,
headwear), `*_Body` (the Top slot), `*_Legs` (or `*_Pants`), `*_Feet` — plus an
optional accessory (`Backpack`, only the Adventurer's) and in some styles a
**weapon** (`Pistol`, `Sword`, …). `classify_slot` reads the suffix;
`is_weapon_node` hides weapons unconditionally, mask or no mask: no business
avatar walks the Grote Markt with a gun.

## Mixing slots is several skeletons, kept in step

`AvatarAppearance` is per slot (`{sex, head, top, legs, feet, accessory}`).
`resolve_avatar_holders` groups the chosen slots by character file: one **holder**
per distinct file, each a child entity of the avatar with a `SlotMask`. A preset
(every slot one style) collapses to a single holder. Each holder is its own scene
with its own `WorldInstanceReady` observer (`play_avatar_animation_when_ready`),
so it binds only its own skeleton, hides the meshes outside its mask, and starts
its embedded clips ("Case 1").

A mixed avatar is therefore several skeletons and several `AnimationPlayer`s that
started on different frames. Each holder gets its own `AnimationGraph`, built
from its own file in the same order, so the node indices agree and the one
representative `AvatarAnimations` on the avatar entity drives every holder's
player. That alone does not stop them drifting: `sync_avatar_animations` copies
the first holder's seek times onto the rest every frame, so a head from one style
and a body from another move as one body.

If the glTFs are missing (a clone without Git LFS) `resolve_avatar_holders`
returns nothing and the avatar is invisible; the spawn logs `holders = 0` to tell
that apart from a model that loaded and was masked away.

## Case 2 is kept for a rig that brings no clips

`bind_external_animations` is the fallback for a scene with no embedded clips:
it roots an `AnimationPlayer` on the `Armature` node and hashes each bone's name
path into the `AnimationTargetId` `bevy_gltf` would have assigned, so clips from
a *separate* glTF on the same skeleton bind and play. Nothing exercises it today
— every character embeds its clips — and it cannot lend these characters a
foreign clip set, because targets are hashed from bone *names* and a different
rig shares none (`avatar-locomotion.md`, on the UAL library).

## The direction this replaced (before 2026-07-22)

Avatars were Quaternius base bodies plus outfit glTFs, with no clips of their own
and every animation bound through Case 2 from the separate `UAL1_Standard.glb`.
The outfit exports were not uniform — one needed the base body under it, one
showed the base's bare back through partial clothing, one was self-complete and
pushed the base's torso out through its skin — and the **head and skull were
welded into the base body's full-body mesh** (only a thin `Face` surface was
separate). So a head could not be grafted onto an outfit: it was the whole base
body or nothing. Per-slot meshes in self-contained files are what removed that.

## Standing height: origin at the feet, and two numbers for the ground

The humanoids are authored **origin-at-feet**. The fox before them had its origin
at its centre, and its offsets survived it: 0.45 (ground + half a fox) in three
places and `+1.0` on the sim's spawn floated the humanoids. The teleport one was
the hard one to see, because it re-plants the avatar on every geo-nav and at
startup.

What the code holds today is **two** values, not one:

- **0.0** — `motion::GROUND_Y`. The sim spawns at `Vec3::ZERO` and clamps to it
  (`SimWorld::step`), the online prediction falls to it, and since a fix
  `driver::placement` writes it for a GPS step.
- **0.05** — `avatar::GROUND_HEIGHT`, the old region tile's top surface. The
  offline gravity clamp stands on it; `collision`'s snap-out backstop reuses the
  `pub` constant rather than a copy; `geo_nav::AVATAR_GROUND_Y` and
  `shot_harness`'s `GROUND_Y` are literal copies of it.

The disagreement has already cost one bug: GPS placement wrote 0.05 while the
sim and the prediction stood at zero, so every fix put the avatar 5 cm up and the
prediction reported a landing — a 6 m/s touchdown several times a second at
120 Hz, thud and camera dip included, for as long as the phone moved
(`driver::placement`'s doc). `geo_nav::place_avatar` still sends 0.05 to the sim
with an online teleport (`NetMessage::Teleport`), so each online jump arrives
5 cm up and lands once.

The follow camera pivots and aims at `FOLLOW_EYE_HEIGHT` (1.5 m, the chest of a
1.8 m model) above the feet; first person uses `FIRST_PERSON_EYE_HEIGHT` (1.62 m,
the eyes), and the third-person aim rises a further `FRAMING_RISE` of the boom
(`avatar-game-feel.md`).
