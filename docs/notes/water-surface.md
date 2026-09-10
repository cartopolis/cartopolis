---
topic: water-surface
triggers: [water.wgsl, water.rs, "water looks", waves, ocean, sea, foam, whitecaps,
  glitter, "specular aliasing", Cox-Munk, "RMS slope", octave, LEAN, Toksvig,
  moire, corduroy, reflectance, F0, shore field, canal, "sea state", WaterSeaState,
  water_octaves]
updated: 2026-09-10
---

# The water surface: what is measured, and what only looked measured

`systems::water` + `assets/shaders/water.wgsl`. The polygons are flat — earcut
gives a canal two triangles — so every wave lives in the *normal*, and the
surface is an `ExtendedMaterial` over `StandardMaterial` so the sun, shadows, fog
and the atmosphere's environment map reach it for free.

## Four numbers that come from outside this repo

* **Slope.** Cox & Munk's 1954 sun-glitter survey: slope variance is
  `0.003 + 0.00512·U`, `U` in m/s. Dead calm 0.055 RMS, light breeze 0.136,
  Beaufort 6 0.254. The hand-picked steepness it replaced rendered ≈0.012 and
  ≈0.11 — half the measured sea at the top and **four times too flat** at the
  bottom, which is most of the water in a canal city.
* **Reflectance.** Bevy's `reflectance` is not F0: `F0 = 0.16 · reflectance²`.
  Water is `((n−1)/(n+1))²` at n = 1.333 → **0.0204**. The 0.25 that stood here
  aimed at 0.02 and landed on 0.01, so the surface reflected half the sky that
  water does — and the reflection is very nearly all of what water is.
* **Absorption.** Extinction runs ≈0.45 / 0.08 / 0.03 per metre for R/G/B, which
  is why a shallow edge goes pale, then turquoise, then green, then blue. One
  scalar tint cannot produce that progression.
* **Per-octave slope.** `SLOPE_MS_A/B/C` is the mean square of
  `d/dφ mix(s², s⁵, chop)`, integrated numerically. It converts octave amplitudes
  into a real slope variance, so it is carried identically on both sides.

## The split, and why it is a choice

Cox & Munk measured the *whole* surface including capillaries; these octaves span
0.4 m to 26 m. `RESOLVED_SLOPE_VARIANCE` (a quarter) puts that share in the normal
and the rest arrives as base roughness, by the same `α² += σ²` route the distance
fade uses — so they cannot double-count, and a test pins the total.

The upper bound is real; what sets the number below it is that **a sum of eight
sines cannot carry a full sea.** Handed the whole 0.254, open water over the
Paterswoldsemeer rendered as corduroy crossed by a standing bullseye of
interference lobes — worse than the flat sheet it replaced. Getting past that
needs a directional spectrum with many waves per wavenumber, which is an FFT,
which is a different surface.

Two causes, and the second was always there:

* **Every octave was in phase at `p == 0`** — the map anchor, which is where the
  camera is. Eight waves agreeing at one point is a focus, and it travelled with
  the anchor. It is in the pre-2026-09-10 renders too. `OCTAVE_PHASE` scatters the
  zero crossings.
* **The crest wobble was a lens.** At ±1.35 rad a crest wanders a fifth of its own
  wavelength and neighbouring octaves focus into ellipse-shaped caustics. Under a
  radian now.

## Fresnel had to point the other way

The albedo used to be *brightened* toward full at grazing angles. Backwards: light
that is not reflected is light that went in, so the body colour is worth `1 − F`
and nothing else. A flat chalky wash over exactly the part of the surface where
water stops having a colour of its own and becomes a picture of the sky was the
most computer-generated thing here, and it sat on top of the reflection.

Both this and the F0 fix are scaled so the **head-on tone is bit-identical** —
that is the number `vector_tiles::palette::WATER` is matched against
(`WATER_ALBEDO`), and only the grazing end moves.

Checked rather than assumed, because `WATER_ALBEDO` asks for it: the ring-water /
painted-water step at `53.07,5.34` from 12 km straight down, noon, went from
**14.9, 14.1, 11.9** to **13.1, 13.0, 11.3** — a shade narrower. Note that
`WATER_ALBEDO`'s own table records 8, 9, 9 for the same pair; that was a different
viewpoint and hour, and the gap between the two readings is the drift its last
paragraph predicts. **Compare a seam against the same view, never against a
remembered number.**

## Aliasing is answered with roughness, not with flatness

The fade returns the variance of exactly the octaves it retired into the GGX width
(LEAN/Toksvig). The sun's glitter path falls out of it instead of a hard dot; the
octave sum can be normalised to a physical slope because the scale is no longer
doubling as an aliasing control; and calm water gets a 0.22 sheen rather than a
0.04 mirror, which is most of the difference between water and blue plastic.

## The term with no ceiling (measured 2026-09-10)

Crest scattering is added **after** `apply_pbr_lighting`, so nothing downstream
holds it in range. Camera 40 m up, 5° above the Paterswoldsemeer, into the sun:

| `surface.y` at full sea | result |
|---|---|
| 0.55 | a lattice of blown-out cyan crests across the lake |
| 0.09 | the brightest water pulled 18 levels toward green |
| 0.045 | what ships |

It also has to be gated on a **low sun** (`SCATTER_PEAK_ELEVATION`): light crosses
the *thickness* of a crest, and the first version gated only on "the sun is up",
so it ran at full strength at noon when a wave is opaque. The number errs toward
invisible on purpose — too faint reads as the term missing, too strong reads as a
bug, and its magnitude is the one thing here a software rasteriser cannot settle.
**Worth a workstation's eye before it is raised.**

## Cost

Two independent things: how many octaves run, and what one costs.

**How many.** The loop breaks at the first fully-faded octave and closes the rest
of the variance as a geometric tail. Octaves evaluated, against the old
five-with-the-body-switched-off:

| distance | calm (6 m base) | Beaufort 6 (26 m base) |
|---|---|---|
| 25 m | 8 | 8 |
| 200 m | **5** | 8 |
| 800 m | 3 | **5** |
| 5 km | 0 | 2 |

The near band costs more and the far field less, crossing over at 200 m in a calm.
`Budget::water_octaves` (8/6/4) is the knob; `octave_norms` renormalises for the
count so a lower rung renders the *same* sea rather than a flatter one.

**What one costs.** Transcendentals per octave went from ~10 to 3, with no change
to the picture: the crest sharpening was `pow(s, mix(2,5,chop))` for a shape that
is a blend of `s²` and `s⁵` and is literally that blend now (three multiplies off
`s²` give both powers and both derivatives); the octave rotation's sine and cosine
are loop-invariant constants a compiler need not fold; and the phase speed advances
by a multiply rather than a reciprocal square root.

One trap in that: **the mean square of a blend is not the blend of the mean
squares.** A lerp between the endpoint constants is 8.5 % high mid-range — a 4 %
error in rendered slope — so the curve is a fitted quadratic, exact at both ends.

**Do not measure this by frame time in the agent container.** Several agents share
the box; the same view reported 1.5 s to 4.7 s a frame with the shader held
constant. The tables above are arithmetic, which is what this machine can settle.

## What is still missing, and cannot be cheap

**Nothing in the scene is reflected.** The environment map is the atmosphere, so
the water reflects the sky and follows the day/night cycle, but the quays and
houses along a canal are not in it. That is the largest remaining gap against a
viewer like Second Life's, and it needs either a second reflection pass or
deferred rendering for bevy's SSR. This client is forward-rendered on purpose.

Where there is **no environment map at all** — an adapter without
`FLOAT32_FILTERABLE` (`sky-without-an-atmosphere.md`), `CARTO_NO_ENVMAP`, sky
lighting off — the standard path has no reflection to give, so the shader stands
in with the `DistanceFog` haze colour under `#ifndef ENVIRONMENT_MAP`, weighted by
the same `reflected` the body colour was taken out of. Not compiled at all when
there *is* an environment map.

## Verifying a change here

Colour is not evidence on this container's rasteriser
(`headless-shots-software-renderer.md`) and water is mostly colour. What a capture
*can* settle is relative: the same view before and after. Structure — where foam
lands, whether a glitter path exists, whether the far field is a sheen or noise,
whether there is a bullseye in the lake — transfers; absolute tone does not. The
views used: a canal at `53.21263,6.57016` from 25 m, the Paterswoldsemeer at
`53.1830,6.5490` from 140 m, and 40 m at `53.1880,6.5560` looking into a 15:00
sun. None is in `docs/shots.toml`; none is a README image.
