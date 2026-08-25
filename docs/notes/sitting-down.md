---
topic: sitting-down
triggers: [sit, bench, seat, picnic table, sit at bench, seated pose, authored clip, AnimationClip at runtime, street furniture, interact prompt, protocol 19, seated on the wire]
updated: 2026-08-25
---

# Sitting is an authored clip and a display offset

"Sit at Bench" appeared as soon as `systems::world_places` started reading the
`pois` layer, and for a while it did nothing you could see: `interact::act` fired
the one-shot `Interact` gesture that every other prompt fires, wrote a line of
text, and left the avatar standing beside the bench. Two separate things were
missing, and neither is the obvious one.

## There is no sit clip, and there cannot be one without an asset

The Quaternius "Ultimate Modular" set embeds the same 24 clips in every character
file and `docs/notes/avatar-locomotion.md` lists them: no sit, no crouch, no
kneel. The UAL library cannot lend one for the reason that note gives (different
rig, different bone names, nothing binds).

Authoring one means shipping a glTF, which means an LFS object with the silent
failure mode `CLAUDE.md` documents — and it means authoring it **twice**, because
the men's and the women's rigs are not the same skeleton: same overall height,
but the women's legs are ~5 cm longer and the torso correspondingly shorter.

So it is synthesised, the way `systems::soundscape` synthesises its WAVs.
`systems::sit_pose` builds an `AnimationClip` at spawn time out of eleven constant
curves — the pelvis, and three bones per leg — and pushes it into the same
`AnimationGraph` as the loaded ones, so it cross-fades, phase-locks across a mixed
avatar's holders and is chosen by `PoseClip` exactly like `Walk` or `Wave`.

**The pose is solved, not tabulated.** What is measured off the glTFs is the rig:
bind-pose joint positions, the two bone lengths, and each leg bone's bind
orientation (needed because a bone's *roll* is not implied by where it points, and
getting it wrong turns the thigh mesh inside out). What is computed is where the
joints go — hips at seat height, ankles on the ground one thigh-length in front,
knee from the two-link solution between them. That is what makes one piece of code
fit both rigs, and it is why the tests can assert "the hips are on the seat and the
feet are on the pavement" rather than comparing quaternions to themselves.

Three things about the rig are load-bearing and none of them are guessable:

- **The pelvis carries a 27° yaw in every clip**, and the `Torso` above it carries
  the opposite twist. Setting the pelvis to identity — the obvious choice, since
  the *bind* pose's pelvis is identity — leaves that counter-twist uncancelled and
  turns the shoulders 27° off the way the avatar is facing. The consequence is that
  the two hips are staggered a few centimetres front to back, so each foot is
  placed relative to *its own* hip; a shared foot line rakes one shin forward and
  the other back by that much.
- **The feet hang off `Root`, not off the shins.** There is no ankle in the chain,
  so bending the knee does not carry the foot with it and the pose has to place
  both. `a_seated_avatar_sits_on_the_bench_and_stands_on_the_ground` checks that
  the shin's own rotation ends where the foot was put.
- **Only the legs and the pelvis are keyed.** Everything above holds whatever the
  outgoing clip left it in, which is the idle's relaxed upper body. Keying the
  shoulders would mean keying the whole spine chain to keep them square.

## A seat is a display offset, not a position

The second half is that the avatar cannot *stand* where it has to sit. The bench
is a solid prop (`world_places` gives every form a circle collider), so collision
stops the avatar ~0.6 m short of the slat. And online the sim owns the avatar's
translation and has never heard of a bench — it holds no map data and never will —
so writing the seat into the position would be overwritten by the next snapshot,
and writing it locally would fight reconciliation for as long as the player sat
there.

So `resources::Seated` carries a world-XZ *offset* measured once, at the moment of
sitting, and `avatar::update_avatar_model_pose` applies it to the model holders —
the same place, and for the same reason, as the flight lean. Same shape as
`terrain_lift`. The follow camera adds it too (`AvatarState::seat_shift`), or it
spends the sit framing the pavement beside the bench.

The offset eases over `SEAT_SLIDE_SECS`, matched to `SIT_FADE`, so the body
finishes arriving as it finishes sitting. That is what `Seated::rising` is for: the
seat outlives the decision to get up by however long the slide takes, and
`is_seated()` — not `seated.is_some()` — is the question to ask before blocking
movement or choosing a pose.

## What ends a sit

Any movement input, a jump, a take-off, leaving Follow mode, and a teleport. The
teleport is the one that is not merely tidy: the offset points at a bench in the
city being left, so it is cleared outright rather than eased.

## On the wire (protocol 19)

`ClientIntent::seated` and `AvatarStateDto::seated` carry the seat as
`Option<[f32; 2]>` — **the offset, not the place**, which is what makes it
portable at all. Each client measured the bench in the frame its own map is
anchored in, and those frames differ by a translation; a delta survives that, a
position does not. The facing needs no field: a sitter turns to face out of the
seat and that is already the replicated `yaw`.

Sitting is a **level**, not an edge like the jump and the emote beside it. It is
resent in every snapshot for as long as the sit lasts, so an avatar that comes
into range mid-sit is drawn sitting — the same property `EMOTE_REPLICATE_SECS`
buys for a wave, without a timer, because a sit has no natural length.

**The blend is not replicated.** The wire carries the whole offset and each viewer
eases onto it off its own clock, so a sit costs one field per snapshot rather than
an animation sampled twenty times a second.

**The sim polices it, because this is the one field that separates where an avatar
*is* from where it is *drawn*.** It has no map and cannot check that there is a
bench there, so it does the two things that do not need one: it bounds the offset
(`MAX_SEAT_OFFSET_M`, and refuses a non-finite one) and it drops the seat the
moment the same intent asks to move, fly or jump. Without the second rule, sitting
is a way to walk around displaced from your own position — invisible to anything
reasoning about proximity, since the position it reasons about stays honest.
`a_seat_is_bounded_and_does_not_survive_moving` pins both.

## What it does not do

- **The seat is replicated; flight still is not.** A remote avatar takes the seat
  offset and the seated pose, but never the forward lean of flight — that has no
  wire field, and `update_avatar_model_pose` gives it only to the local avatar.
- **Only benches and picnic tables.** `furniture::seat` answers for exactly the two
  forms whose prompt already reads "Sit at …". A shelter has a slat in it and does
  not offer one, because its prompt says "Use".
- **The pose is built for one seat height.** Every slat this client can sit on is
  0.48 m; `a_seat_is_the_top_of_a_slat_that_is_really_meshed` is what keeps that
  true, and it is the assertion to look at first if a re-measured bench ever puts
  the avatar's hips through the timber.

## Seeing it

`--ui sit` seats the avatar on a synthetic bench half a metre ahead of it, so a
headless capture can photograph the pose without a streamed city having to put a
real bench where the camera is:

```bash
CARTO_TILE_URL="https://tiles.cartopolis.org/tiles/osm/{z}/{x}/{y}" \
cargo run -p cartopolis -- --shot /tmp/sit.png --shot-mode avatar \
    --at 53.2194,6.5665,4 --look=-10,110 --ui sit --size 700x800
```

It runs every frame and after the teleport, unlike the rest of the `--ui` seeding,
because the teleport clears the seat.

**`--shot-mode avatar` did not work at all until this landed**, which is what made
the pose so hard to look at. Follow is not on offer offline
(`free_camera::follow_available` wants a session) and every scripted run *is*
offline, so `camera_mode_toggle` dropped the camera to Fly on the frame after the
teleport put it in Follow: the mode the harness documents as "the right mode for
checking avatars" quietly produced a free-camera view of wherever the avatar was
not, and — once this feature existed — stood the seated avatar back up on the way
past. `shot_harness::hold_scripted_camera_mode` re-asserts the mode every frame
for the length of the run. Nothing about the interactive rule changed; offline
still has no Follow.
