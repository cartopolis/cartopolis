---
topic: water-surface
triggers: [water.wgsl, water.rs, "water looks", waves, ocean, sea, foam, whitecaps,
  glitter, "specular aliasing", Cox-Munk, "RMS slope", octave, LEAN, Toksvig,
  moire, corduroy, shore field, canal, "sea state", WaterSeaState, water_octaves]
updated: 2026-09-10
---

# What the water surface is, and what it is measured against

`systems::water` + `assets/shaders/water.wgsl`. The polygons are flat — earcut
gives a canal two triangles — so every wave lives in the *normal*, and the
surface is an `ExtendedMaterial` over `StandardMaterial` so the sun, the shadows,
the fog and the atmosphere's environment map reach it for free.

## The measurement, and the split it forces

`rms_slope` is Cox & Munk's 1954 sun-glitter survey: the variance of sea-surface
slope is `0.003 + 0.00512·U`, `U` in m/s. Dead calm 0.055, a light breeze
(3 m/s) 0.136, Beaufort 6 0.254.

That is the whole surface, down to the capillary ripples that carry most of a
sea's slope variance. These octaves span roughly 0.4 m to 26 m, so they can only
be a *part* of it — and `RESOLVED_SLOPE_VARIANCE` (a quarter) is where the split
is made. The rest is not dropped: it becomes base roughness, by the same
`α² += σ²` route the distance fade already uses, so the two cannot double-count.
`the_split_conserves_the_measured_variance` pins that.

**The split is chosen; the total is measured.** The upper bound is real, and what
sets the number below it is the artefact in the next section.

## What a sum of eight sines cannot do (measured 2026-09-10)

Handed the *whole* 0.254 of Beaufort 6, open water over the Paterswoldsemeer
rendered as corduroy crossed by a standing bullseye of interference lobes — worse
than the flat sheet it replaced. Two separate causes, and the second is a bug
that was always there and only became visible once the sea was steep enough to
show it:

* **Every octave was in phase at `p == 0`**, which is the map anchor, which is
  where the camera is. Eight waves agreeing at one point is a focus, and it
  travelled with the anchor. `OCTAVE_PHASE` scatters the zero crossings.
* **The crest wobble was a lens.** At ±1.35 rad a crest wanders by a fifth of its
  own wavelength, and where two octaves' wanderings agreed the sum focused into
  ellipse-shaped caustics. It is under a radian now — enough to bend a straight
  crest, not enough to fold one over another.

With both fixed and a quarter of the variance in the normal, the same view is a
fine irregular ripple field. Anything approaching the full variance is not
reachable by tuning: it needs a directional spectrum with many waves per
wavenumber, which is an FFT, which is a different surface.

## The term with no ceiling (measured 2026-09-10)

Crest scattering — light entering the back of a wave and leaving the front — is
added **after** `apply_pbr_lighting`, so nothing downstream holds it in range and
its own numbers have to. Two captures over the Paterswoldsemeer, camera 40 m up
and 5° above the water looking straight into a mid-afternoon sun, bound it:

| effective albedo (`surface.y` at full sea) | result |
|---|---|
| 0.55 | a lattice of blown-out cyan crests over the whole lake |
| 0.09 | the brightest water pulled 18 levels toward green |
| 0.045 | what is in the tree |

It also has to be gated on a **low sun** (`SCATTER_PEAK_ELEVATION`): light has to
cross the thickness of a crest to come out the far side, and the first version
gated only on "the sun is up", so it ran at full strength at noon, when a wave is
lit from over the top and is opaque.

The number that ships errs toward invisible on purpose. Too faint reads as the
term not being there; too strong reads as a bug — and **its magnitude is the one
thing on this surface a software rasteriser genuinely cannot settle.** Worth a
workstation's eye before it is raised.

## Why the sea could not be steep before at all

Sub-pixel ripples alias. The old shader answered that with a tuned lump of extra
roughness (`surface.y * (1 - detail)`) and by keeping the surface flat enough
that there was not much to alias — the second of which is why calm water read as
plastic sheeting.

The fade now returns the **variance of exactly the octaves it retired**, folded
into the GGX width the way LEAN/Toksvig mapping does. Three things follow:

* the sun's glitter path falls out of it, instead of a hard dot near the camera;
* the octave sum can be normalised to a physical RMS slope at all, because the
  scale is no longer doing double duty as an aliasing control;
* and calm water gets a *sheen* rather than a mirror — perceptual roughness 0.22
  at dead calm against the old flat 0.04, which is most of the difference between
  water and a blue plastic sheet.

## Cost — where it went up, and where it went down

The loop **breaks** at the first fully-faded octave and closes the remaining
slope variance as a geometric tail in one line. Wavenumber only rises, so the
early exit is exact rather than an approximation. Octaves actually evaluated per
fragment, against the old five-with-the-body-switched-off:

| view distance | calm (6 m base) | Beaufort 6 (26 m base) |
|---|---|---|
| 25 m | 8 | 8 |
| 100 m | 6 | 8 |
| 200 m | **5** | 8 |
| 800 m | 3 | **5** |
| 5 km | 0 | 2 |

So it is not free, and the shape matters: **the near band costs up to 60 % more
and the far field costs less**, with the crossover at 200 m in a calm and 800 m
in a gale. A canal view has a little water close up; an open-water view has most
of its fill beyond the crossover. `Budget::water_octaves` (8 / 6 / 4) is the knob
if that trade is wrong on a given device.

`octave_norms` renormalises for the count, so **a lower rung renders the same
sea**; it stops resolving the small stuff sooner and takes it as roughness
instead. A rung that changed the slope would show a phone a different ocean from
a workstation's, on the hardware nobody is testing on.

**Do not try to measure this in the agent container by frame time.** Several
agents share the box, and the same view across five runs on 2026-09-10 reported
1.5 s to 4.7 s a frame with the shader held constant — the signal is entirely
other people's builds. The table above is arithmetic on the fade, which is what
can be checked here; a real timing belongs on a quiet machine, through
`cargo perf`.

## What is still missing, and cannot be cheap

**Nothing in the scene is reflected.** The environment map is the atmosphere, so
the water reflects the *sky* and follows the day/night cycle, but a bridge, a
quay and the houses along a canal are not in it. That is the largest remaining
gap against a viewer like Second Life's, and it is the expensive one: it needs
either a second reflection pass over the scene or deferred rendering for bevy's
screen-space reflections, and this client is forward-rendered on purpose.

**On a view with no environment map at all** — an adapter without
`FLOAT32_FILTERABLE` (`sky-without-an-atmosphere.md`), `CARTO_NO_ENVMAP`, or sky
lighting off in Settings — the standard path has *no* reflection to give. The
shader stands in with the `DistanceFog` haze colour under
`#ifndef ENVIRONMENT_MAP`, weighted by Schlick against water's F0 of 0.02: a
horizon tone, not a sky, which is close to right because a reflected ray off
near-flat water is near-horizontal. It is not compiled at all when there *is* an
environment map, or it would be a second reflection over the real one.

## Verifying a change here

Colour and lighting are not evidence on this container's software rasteriser
(`headless-shots-software-renderer.md`), and water is almost entirely colour and
lighting. What a capture here *can* settle is relative: the same view before and
after, on the same renderer. Structure — where foam lands, whether a glitter path
exists, whether the far field is a sheen or a field of noise, whether there is a
bullseye in the middle of the lake — transfers. Absolute tone does not, and
belongs on a workstation. The two views used above are a canal at
`53.21263,6.57016` from 25 m and the Paterswoldsemeer at `53.1830,6.5490` from
140 m; neither is in `docs/shots.toml`, because neither is a README image.
