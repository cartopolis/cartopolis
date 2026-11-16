---
topic: water-splashes
triggers: [splashes.rs, systems::splashes, WaterFx, water_effects, "Splashes and spray", SWIM_STROKE_PHASE, swim_phase, assets/sounds/water, particles.wesl, ParticleMaterial::rings, "splash", "fountain jet", "fountain sound"]
updated: 2026-11-16
---

# Water reacts: splashes, strokes, ripples and fountain jets

Asked for by the owner on 2026-11-16 ("Water en fonteinen"): the avatar swam
since 2026-11-14 (`avatar-locomotion.md`), but going in, swimming and the
fountains made no splash and no sound.

## What happens when

| Moment | Drawn | Heard |
|---|---|---|
| A body goes into the water | fine streaked drops, a white crown, a foam patch on the surface and two or three faint ripples, all sized by the entry speed | `splash-big-*` (size ≥ 0.45) or `splash-small-*`, louder with size |
| A swimmer's stroke | a small throw of drops and two faint ripples ahead of the feet, where the head and hands meet the water | `stroke-*` |
| Treading water (in it, not moving) | gentle continuous rings at the body | — |
| Swimming at all (own avatar) | — | `tread.ogg` loop, eased in; footsteps are off and the landing thud is not played |
| Another swimmer near | the same, from this viewer's map | `tread.ogg` by distance, strokes and splashes by distance |
| A fountain jet (`amenity=fountain` in open water) | a plume of streaked drops from the column's top and faint ripples at its foot, for the six jets nearest the camera within 300 m | `fountain.ogg` loop by distance to the nearest jet (full within 4 m, gone at 60 m) |
| A swimmer reaches a jet (within 1 m) | a medium splash, once on the way in | a small splash |

Nothing went on the wire. Who is in the water is each viewer's own reading
(`AvatarState::swimming`, `RemoteAvatar::swimming`), as the swimming itself is.

## How big a splash is

`entry_speed` is the speed of a fall from where the feet were the frame
*before* the water was reached, under `motion::GRAVITY` (22 m/s², the avatar's
own fall offline), combined with the speed across the ground. The frame before
matters: the lift into the water eases (`WATER_SINK_RATE`), and a capture flown
frame by frame has already put the feet at the water by the time the flag
flips. `splash_size` maps 1 m/s (a wade) to 0 and 8 m/s to 1; a walk off a
1.5 m quay is 8.2 m/s, i.e. the biggest; running down a bank into a pond is
about a third. The drop is clamped to 6 m so a teleport into the water is not
a meteor — it does still splash, which reads as arriving in the water.

## A stroke is timed by the arms

`cartopolis_human::build::SWIM_STROKE_PHASE` (0.95) is where in the `swim`
loop the hands shoot forward through the surface, measured on the shipped pack
as the frame of the hands' fastest forward travel
(`the_stroke_splashes_where_the_hands_shoot_forward`, 2026-11-16: phase 0.953;
the hands are also at their highest there). `AvatarAnimations::swim_phase`
reads the phase off the animation player that is playing the stroke — its
seek time over the clip's own length (`swim_secs`) — so the splash follows a
re-cut pack, a playback-rate change and a pinned capture alike. A rebuilt pack
whose stroke moves fails that test rather than splashing at the wrong moment.

## Drawn with the particle shader, plus two shapes

`systems::particles`' closed form and quad pool, unchanged in principle
(`particles-are-a-function-of-time.md`). Two shapes are added, chosen by the
spare `sizing.w`; a prim's effect never sets it, so neither is on the wire.

- **Rings** (`ParticleMaterial::rings`, 1): each particle a ring lying on the
  emitter's level, growing at its speed. The band widens with the radius
  (+0.35 × radius) and the ring fades as (1 − t)² from the start, so a ripple
  softens and thins out as it spreads, as a real one does.
- **Droplets** (`ParticleMaterial::droplets`, 2): each particle its own size
  (0.35–1.65× by a hash), drawn as a streak along its motion across the view
  (a 1/25 s exposure's travel) and brighter as it leaves the water.

**What it is meant to look like, and how it first did not** (owner review,
2026-11-16): the first rings were crisp thin white circles that read as UI
drawn on the water, and the spray was round soft blobs of one size that read
as snow. The reference photos (CC0, Wikimedia Commons: a girl jumping into the
sea at Röe Sandvik; ripples on a foggy lake) show a splash as a dense white
crown with fine streaked drops flung off it and white water on the surface,
and ripples as a faint brightening of the reflection. So a body going in is
now four bursts: fine drops, a lower and thicker crown, two or three faint
ripples (alpha ≤ 0.3, a test holds it), and a patch of foam — a "ring" whose
band is wider than it is, i.e. a soft disc, gone in a second or two. A stroke
is fine drops and two ripples. Judged on mac-1's GPU, which is evidence for
the shape and not for exact colour.

- **Bursts re-arm a fixed set of 40 emitters** (two a stroke, four a body
  going in) in turn — a material rewrite, never a spawn. A burst past its
  life is hidden. Each is one draw of the shared pool.
- **A moving emitter drags its particles**: they are placed from the emitter's
  *current* origin. So continuous rings are only for a body treading water;
  a moving swimmer leaves a burst at each stroke, where it happened — a wake.
- The rings ride 10 cm over the registered surface. **Beside a quay wall the
  half of a ring towards the wall is still cut off** (Noorderhaven,
  2026-11-16, mac-1, at 3, 6 and 10 cm alike): the drawn water there rises
  towards the wall above the registered level, so it is the water's shape,
  not the lift. In open water and on the Noorderplantsoen pond the rings are
  whole. Unsolved; the cure is in the water near a wall, or a depth bias on
  the rings, not a higher lift.
- All of it follows **Settings ▸ Graphics ▸ Splashes and spray**
  (`AppPrefs::water_effects`, on by default) and nothing else — never the frame
  rate. A jet's plume also follows the furniture layer, since it belongs to
  the column that layer draws.

## Sounds

CC0 recordings, Freesound HQ previews, cut and credited as the city sounds
were (`city-soundscape.md`, `assets/sounds/LICENSE.md`). Levels follow
`AppPrefs::effects_volume`; heard only while the camera follows the avatar,
with the avatar as the ear, as the ambience is. At most three one-shots start
in a frame.

`RUST_LOG=cartopolis::splashes=debug` logs every splash found (drop, speed,
size), every sound started (take, volume, and the animation clock, which is a
`--video-fps` movie's own time) and the two loops' levels four times a second
— enough to rebuild the soundtrack of a capture, which is how it was checked:
nobody can listen in the agent container.

## Not done

- A plaza basin fountain (on paving) has a collider at 2 m and cannot be
  walked into; it gets no spray and no sound — the register cannot say whether
  it is running.
- Nothing persists: no wet avatar, no drips after climbing out.
