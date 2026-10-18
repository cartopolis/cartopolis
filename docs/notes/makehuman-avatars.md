---
topic: makehuman-avatars
triggers: [crates/human, AvatarLook, SetAvatarLook, AvatarLookOf, RemoteLooks, looks.rs, V38__avatar_looks.sql, net/look.rs, systems::human_avatar, human_avatar.rs, avatar_designer.rs, HumanLook, CARTO_HUMAN_LOOK, aim_height, human-pack, default.humanpack, assets/models/avatar/human, mocap.rs, rig.rs, hands.rs, level_gaze, HumanTextureLoader, avatar.jpg, PACK_VERSION, ProxyRef, MakeHuman, MPFB, 100STYLE, CMU, BVH]
updated: 2026-10-18
---

# Realistic avatars are MakeHuman bodies built at runtime

Every avatar is a person **built in the client** from MakeHuman's data: the
base mesh, the shape targets the sliders blend, the `game_engine` rig and its
weights, and clothes and hair as fitting rules. `crates/human` does the
arithmetic (Bevy-free); `systems::human_avatar` turns the result into joints,
skinned meshes and clips. It replaced the stylised Quaternius characters
(`retired/quaternius-avatars.md`), whose only choice was a costume, on
2026-10-18; there is no switch back.

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
and motion capture: 100STYLE's Neutral style (`zenodo.org/records/8127870`,
CC BY 4.0) for walk, run and idle, and the CMU database's cgspeed BVH
(`github.com/una-dinosauria/cmu-mocap`, free for all uses but not for resale)
for the gestures and the seat. The cuts — which take, which frames — are
listed in `human-pack`.

**The pack carries every macro target** (`TargetSet::Macros`), so the sliders
can make any adult. That is most of it; one body needs about 30 of the 516, so
fetching a body's targets on demand is the next step if it must get smaller.
`HumanModel::missing_targets` is how a body asked of a pack without them says so.

**Pack 7 nearly halved it losslessly (2026-10-18): 13.9 MB to 8.0 MB.** Measured
field by field before: the 786 targets 10.26 MB, the clothing references 2.85
MB, the mesh 0.49, the clips 0.19, the weights 0.11. The targets became
differenced columns (`Target`'s doc has the table — 5.13 MB, same numbers);
the clothing references whole steps instead of `f32`s (`ProxyRef`). **Trap:
MakeHuman's reference weights go past 2** (up to 5.1, in a bob haircut; every
pair of shoes is 3.5), so an `i16` at 1/16384 clipped them, and the clipped
shoes stood a grey spike out behind the legs. They are `i32`, which postcard
writes in the same three bytes. The textures went from 9.9 MB to 7.5 MB by
storing the grey (neutral) ones as one channel, plus alpha where they cut out.

Measured 2026-10-17 on mac-1: the pack was 11.1 MB (13.6 MB from 2026-10-18 with the wardrobe and the face, plus 9.9 MB of textures) — ten clips and all 516 macro targets from age 18 up (the "child" ones blend in below 25; only the baby ones are left out), stored as half-millimetre steps and deflated, where floats were 70 MB —, the nine 2048² PNG textures
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
- **100STYLE is Xsens, 60 fps, centimetres; CMU's conversion 120 fps in
  0.056 m units.** Both rest in a T-pose facing +Z with the left at +X, like
  MakeHuman. Both name `LeftShoulder`, and mean different bones by it (Xsens's
  upper arm, CMU's clavicle); `mocap::map_bones` tells them apart by whether the
  skeleton has a `LeftCollar`. **CMU subjects 141 and 143 were captured at
  60 fps though their files say 120** (a jump in them falls at four times
  gravity), so their cuts set the rate. Frame 0 of every CMU file is a T-pose
  the conversion added.
- **Raw capture is cut, not used**: a loop is the stretch whose ends match best
  (`mocap::find_loop`), its remaining mismatch spread over it, its travel
  subtracted and kept as the clip's speed. A take turns at the walls of the
  capture room, and a turn on the spot can match its ends as well as a stride,
  so a travelling loop must also keep the take's usual pace in a straight line
  (before that, `Rushed`'s walk came out at 0.2 m/s).
- **The gaits were chosen by speed** (`human-pack --probe`, 2026-10-17, on the
  default man): 100STYLE's Neutral walk is 0.86 m/s, and the game walks at
  1.8 — twice its cadence. `Rushed` is 1.65 m/s on a 0.9 s cycle and plays at
  1.1×. Every 100STYLE run is a jog (2.3 m/s at best, `Rushed`) and the game
  runs at 6 m/s; CMU 09_01 is a real run, 3.46 m/s on 0.73 s, played at 1.7×.
- - **A capture rests in a T-pose and MakeHuman in an A-pose**, with different
  bone rolls, so clips cannot be copied channel for channel. They are stored as
  each bone's world rotation relative to the take's T-pose, and replayed
  relative to MakeHuman's own T-pose (`poses/game_engine_fk/t-pose.json`) —
  **with each of its bones first turned to point the way the take's does**
  (`rig::aligned_tpose`). A capture skeleton's spine and neck stand dead
  straight at rest and MakeHuman's lean forward; without the turn the take's
  own neck lean was added to a neck that had it, and every clip looked at its
  feet. Each clip is then stood on the ground by its lowest ankle
  (`rig::ground`), so no capture's floor height matters.
  `the_shipped_clips_play_like_people` is the guard: idle hands hang by the
  sides, and a walking ankle reaches the ground.

## The designer

The Appearance panel is `systems::avatar_designer` (`docs/mockups/avatar-designer`).
A look is `cartopolis_core::AvatarLook` (the client calls it `HumanLook`): whole percents,
RGB bytes and names, so it compares exactly and travels in about 150 bytes. While the panel is open
the avatar wears the draft (`HumanDraft`) and is rebuilt at most every 0.15 s
as it changes — each rebuild re-uploads about 1.5 MB of meshes — and closing it
unsaved puts the saved look back.

Three things it does so the avatar can be seen while it is shaped:
- **The model turns to the camera** (`update_avatar_model_pose`), which in
  Follow is behind it. A turn of the model only: the heading the sim holds and
  the camera stay where they were.
- **On a phone it is a sheet over the bottom half** (`egui_manager::bottom_sheet`),
  not the right panel, which on a compact screen is the whole screen.
- **The camera aims near the feet while the sheet is up**
  (`CameraState::aim_height`, -0.3 m), tilting the view so the whole body stands
  above it. 0.25 m put the legs under the sheet. Not saved.

`CARTO_HUMAN_LOOK="body_shape=0,weight=90,age=70"` starts an undesigned avatar
with those sliders, for a capture, which cannot drag one. It takes the wardrobe
and face too (`hair=afro01,top=150.40.40,face:nose/…=80`).

## Everyone sees everyone's look (protocol 42)

A client sends `SetAvatarLook` on every new connection and whenever its saved
look changes (never while it has not designed one: then nothing is kept and
everyone, itself included, draws the default body). The server checks it is
reasonable (`AvatarLook::is_reasonable`: sliders in range, age 18–90, short
names, a bounded face list — not whether the names exist, which is the client
pack's business), keeps it with the account (`accounts.look`, postcard bytes,
migration V38), wears it in the world and broadcasts `AvatarLookOf { avatar_id,
look }` on the world channel. A client that connects is handed every online
look before the snapshot that brings the avatars, and its own saved look is
worn and broadcast without it sending anything. Only the avatar id travels,
never the account: who an avatar is stays something a client asks.

On the client a look lives in `RemoteLooks` by sim id, not on `RemoteAvatar`,
because it arrives before the avatar's snapshot. A look that changes after a
model is built rebuilds it; the same look again (it is re-sent on every
connection) does not. **At most one MakeHuman body is built a frame**, so a
crowd coming into view is built over a few frames rather than in one hitch.

Protocol 42, message kinds 121 and 122 and migration V38 were reserved through
`midas reserve` on 2026-10-18, after the meetings work took 41, 111–120 and V37.

## The wardrobe, the skins and the face (2026-10-18)

From MakeHuman's CC0 system assets: 10 hair styles, 5 brows, 7 outfits, 5
shoes, a hat, 6 eye colours, and all 18 skins (African, Asian and Caucasian,
young, middle-aged and old, female and male — a look picks the family, its age
and body shape pick the skin). **Left out on purpose: every outfit printed with
MakeHuman's logo** (female_casualsuit01/02, male_casualsuit02/04/06, the T-shirt
the first trial wore): the assets are free, the trademark is not ours to put
on everyone's shirt.

**Recolouring needs neutral textures** (`human::textures`): a tint multiplies,
so over an authored texture it can only darken. Hair, brows, lashes and outfits
are stored as their luminance over the texture's bright end, in grey, with the
colour that gives the authored look back recorded per region; any colour,
lighter ones included, is then a tint. **An outfit is split into a top and a
bottom** by texture island — a suit is one mesh, but its shirt and trousers are
separate islands — and an island whose middle is below the hip joint is the
bottom. Splitting by skin weight first put a pair of overalls, bib and all, on top.

**Textures went from 19 MB to 9.9 MB while tripling the wardrobe**: 1024 px for
skins and garments, 512 for hair and brows, 256 for eyes, JPEG unless the alpha
really cuts something out. MakeHuman marks most garments transparent whether or
not they are, which made every outfit a 1.5 MB PNG; the pack now looks at the
alpha. The client needed Bevy's `jpeg` feature, whose decoder the `image` crate
already built.

**On the GPU they are block-compressed, with mips** (`HumanTextureLoader`,
reusing the map tiles' encoder): the default man's seven textures went from
16.0 MB of RGBA8 to 5.3 MB. What routes a file to that loader rather than
Bevy's is its **inner extension**: every pack texture is `*.avatar.jpg` or
`*.avatar.png` (`textures::TEXTURE_EXTENSION`), and Bevy tries a file's full
extension before its last one. A texture written as plain `.jpg` still loads —
uncompressed, with no error to say so.

**The face is MakeHuman's modifiers**, carried whole (about 110 controls, 270
targets, 0.1 MB of the pack); the designer offers about thirty. A modifier is a
pair of target sets by sign, a left-and-right one moving both sides; a
one-sided one (`head-oval`) goes 0 to 1. Face values are stored as a sparse
list, so an untouched face costs nothing.

## Rejected: Quaternius's animation library (2026-10-17)

The first retarget used Quaternius's Universal Animation Library. Quaternius
moved to a licence forbidding redistribution on 2026-08-28, and
`LICENSE-THIRD-PARTY.md` bars anything downloaded from them since; the March
2025 CC0 release on OpenGameArt was arguably exempt, but motion capture was the
better answer anyway — real motion suits a realistic body better than
hand-keyed clips — and no commit on the branch ever carried a pack made from
the library. Mixamo (no redistribution), Bandai Namco's set and LAFAN1
(non-commercial) and SFU's (no commercial products) were ruled out on licence.

## What code adds on top of the capture (2026-10-18)

- **The gaze.** Retargeted as captured, the face looked 20–25° down at idle
  and in the gestures and 42° down in the walk (the run was level) — measured
  per clip as the head's turn from rest applied to forward. `rig::level_gaze`
  turns each upright clip's head so its average is 6° below level; the
  capture's motion about that average stays. Clips whose head pitches over
  more than 40° (the roll, the fall) keep their gaze.
- **The neck** is halfway between the chest and the head, in every clip. CMU's
  skeleton has two neck joints to the rig's one; through the lower one the
  gestures held the rig's neck ~55° back and the head forward against it, and
  the wave's head lay on its shoulder until the neck was derived instead.
- **The fingers.** Neither database records them (100STYLE stops at the wrist,
  CMU has a stub), so `hands` curls them per clip — relaxed at idle, a loose
  fist running, a fist to punch, open to wave, closing at the end of the reach
  — with a little movement through each cycle. The bend direction comes from
  the rest hand's own curl, so no bone roll had to be worked out; a test
  checks that a fist brings the fingertips toward the thumb's root.

Known gaps: 100STYLE is all locomotion (CMU supplies the gestures), no faint in
CMU (a slip-and-fall stands in), the waving hand is the only one that opens on
purpose (both hands share the clip's grip), and the `--clip` sheet's first tile
sometimes catches the city before it has loaded.
