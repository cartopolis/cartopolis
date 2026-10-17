---
topic: makehuman-avatars
triggers: [crates/human, systems::human_avatar, human_avatar.rs, human-pack, default.humanpack, assets/models/avatar/human, CARTO_HUMAN_AVATAR, MakeHuman, MPFB, UAL]
updated: 2026-10-17
---

# Realistic avatars are MakeHuman bodies built at runtime

The Quaternius characters (`avatar-composition.md`) are stylised, and a style
is the most anyone can choose. The replacement being trialled is a person
**built in the client** from MakeHuman's data: the base mesh, the shape
targets the sliders blend, the `game_engine` rig and its weights, and clothes
and hair as fitting rules. `crates/human` does the arithmetic (Bevy-free);
`systems::human_avatar` turns the result into joints, skinned meshes and clips.
It is behind `CARTO_HUMAN_AVATAR=1`, for the local avatar only.

## Why MakeHuman, and what lost (2026-10-17)

- **MakeHuman / MPFB 2** — realistic, parametric, and everything it exports and
  ships as data is CC0 (`mpfb2/LICENSE.md` §C; the system assets pack says so per
  file). Only the add-on's *code* is GPL, so the formats are implemented here
  from their docs, never by translating MPFB's Python.
- **Ready Player Me** — shut down 2026-01-31 after Netflix bought it.
- **VRoid / VRM** — customisable but anime-styled.
- **MetaHuman, Character Creator, SMPL-X** — licences rule them out (Unreal-only,
  per-seat runtime terms, non-commercial research).
- **Anny** (NAVER LABS, Apache 2.0) — MakeHuman-derived bodies for every age,
  body only. A later option for children's bodies, not a replacement.
- **Generating bodies from code alone** — gives a mannequin, not a person; the
  anatomy *is* the data. Animation is the part code can usefully generate
  (foot placement, leaning, looking at someone), on top of clips.

MPFB's maintainer advised exactly this shape for a Bevy game
(makehumancommunity/mpfb2#381): parse the native files rather than push 1 200
mutually exclusive shape keys through glTF.

## The pack

`human-pack` (`crates/human/src/bin`, feature `pack`) reads the sources and
writes `assets/models/avatar/human/default.humanpack` (postcard) plus the
textures beside it. Sources: mpfb2's `src/mpfb/data`, the MakeHuman system
assets zip (`files2.makehumancommunity.org/asset_packs/makehuman_system_assets/`),
and Quaternius's Universal Animation Library 1 Standard from OpenGameArt
(`opengameart.org/sites/default/files/universal_animation_librarystandard.zip`,
CC0) — its `Godot/AnimationLibrary_Godot_Standard.glb`.

**A pack carries only the targets of the bodies it was built for.** Every macro
target would be ~350 near-full-mesh files (~80 MB as floats), so the first pack
carries the default man's and woman's eight. Sliders need either a smaller
encoding or fetching a body's targets on demand; that is the open question for
the chooser, and `HumanModel::missing_targets` is how a body asked of the wrong
pack says so.

Measured 2026-10-17 on mac-1: the pack is 3.5 MB, the nine 2048² PNG textures
19 MB (too big for a phone; downscale and compress before this ships). The
default man is 1.73 m, 18 700 vertices and 33 000 triangles across body, eyes,
brows, lashes, hair, outfit and shoes — the Quaternius Adventurer file is about
20 000 vertices. `build` took about 10 ms in the test profile.

## Facts the code depends on

- **Base mesh**: 19 158 vertices in decimetres, +Y up, facing +Z (Bevy's
  handedness — scale by 0.1, no axis change). Vertices `0..13380` are skin; the
  rest are helpers (125 joint cubes, eyes, teeth, tights, hair) that are shaped
  with the body and never drawn. Faces using only skin vertices are the body.
- **The rig JSON has its bones at the top level**, not under `"bones"` as MPFB's
  docs say; alphabetical, so it is sorted parents-first on read. game_engine
  uses only `CUBE` joints (the mean of a joint cube's 8 vertices) plus one
  `MEAN`, and no offsets.
- **Rolls and the T-pose are Blender's** (Z up, `vec_roll_to_mat3`, Euler XYZ in
  bone space), so `rig.rs` builds bone frames in Blender's frame and converts
  once; a bone's local frame is the same in both.
- **A garment `.obj` is not in its fitted position** (authored on other bodies;
  the eyes are 0.84 m off). Only its faces and UVs are used; positions always
  come from the `.mhclo` fitting.
- **UAL1's glb uses Rigify names** (`DEF-upper_arm.L`) on the same skeleton as
  UAL2's Unreal names; `pack::library_bone` maps both onto the rig's. UAL1 is
  the one with the locomotion (idle, walk, jog, sprint, jump, swim, sit). It has
  no wave and no kick; `human_avatar::CLIP_ORDER` names what stands in.
- **The library's rest is a T-pose and MakeHuman's an A-pose**, with different
  bone rolls, so clips cannot be copied channel for channel. They are stored as
  each bone's world rotation relative to the library's T-pose, and replayed
  relative to MakeHuman's own T-pose (`poses/game_engine_fk/t-pose.json`).
  `the_shipped_clips_play_like_people` is the guard: idle hands hang by the
  sides, and a walking ankle reaches the ground.
