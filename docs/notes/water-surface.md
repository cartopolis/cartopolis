---
topic: water-surface
triggers: [water.wgsl, water.rs, "water looks", waves, ocean, sea, foam, whitecaps,
  glitter, "specular aliasing", Cox-Munk, "RMS slope", octave, LEAN, Toksvig,
  moire, corduroy, reflectance, F0, shore field, bank field, canal, "sea state",
  WaterSeaState, "shadow around the shore", "rim of shadow",
  water_octaves, sky_ibl, "water looks grey", "no environment map", shallows,
  transparency, "ring edge", far_water, water_common, "hole in the water",
  "painted water", RENDERED_FORMAT]
updated: 2026-09-29
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

Checked rather than assumed, because `WATER_ALBEDO` asks for it. The ring-water /
painted-water step at `53.07,5.34` from 12 km straight down, at noon:

| | R, G, B |
|---|---|
| before this work, on lavapipe | 14.9, 14.1, 11.9 |
| after, on lavapipe | 13.1, 13.0, 11.3 |
| **after, on the Steam Deck** | **6.4, 8.7, 7.9** |

Two readings of the same thing, and the third is the one that counts. The change
narrows the seam slightly — that comparison is like-for-like and holds on either
machine — but the *absolute* width does not transfer at all: lavapipe puts it near
14 and real hardware near 8, which is where `WATER_ALBEDO`'s own table has always
had it. **The palette match is intact, and it took a GPU to say so.** Compare a
seam against the same view on the same machine, never against a remembered
number.

## Aliasing is answered with roughness, not with flatness

The fade returns the variance of exactly the octaves it retired into the GGX width
(LEAN/Toksvig). The sun's glitter path falls out of it instead of a hard dot; the
octave sum can be normalised to a physical slope because the scale is no longer
doubling as an aliasing control; and calm water gets a 0.22 sheen rather than a
0.04 mirror, which is most of the difference between water and blue plastic.

## Crest scattering is a multiplier, and it took three goes to learn why

An additive radiance was scaled by the sun's `color · illuminance · exposure`, and
this scene lifts exposure at a low sun faster than it extinguishes the beam — so
the term **grew as the sun set**, which is backwards, and put a lattice of
blown-out cyan crests across the Paterswoldsemeer. Three readings, all 2026-09-10:

| form | strength | result |
|---|---|---|
| additive | 0.55 | a neon lattice across the whole lake |
| additive | 0.09 | brightest water pulled 18 levels toward green |
| additive | 0.045 | *appeared* fine — see below |
| relative, capped at 30 % | — | a sun glitter path |

**The third row is the one to remember.** It looked tame, and it was measured in a
view aimed 100° away from the sun, so it never fired the term at all. Re-aimed at
the sun the same numbers blew out exactly as before. A verification that does not
exercise the thing it verifies is worse than none, because it is filed as
evidence.

The fix is not a smaller number, it is a bounded form: a fraction of the light
already there (`SCATTER_MAX`, 30 %) says "a crest is brighter and greener than the
water beside it", which is true in any exposure and cannot blow out in any of
them. The mask is a ramp rather than a narrow band for the same class of reason —
a hard threshold on a smooth field draws the wave field's own diamond lattice.

It is still gated on a **low sun** (`SCATTER_PEAK_ELEVATION`): light crosses the
*thickness* of a crest, and the first version gated only on "the sun is up", so it
ran at full strength at noon when a wave is lit from over the top and opaque.

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

## How to A/B this surface at all

Two `gpu shots` runs of the same preset are **not** the same frame. The waves are
a function of `globals.time`, which is wall clock, so every capture lands on a
different phase and a raw image diff of two runs reports ~14 % of pixels changed
before anything has been altered. Half of that is the wave texture and it is
signed noise, not a finding.

So measure over **water pixels only** (`b − r > 18` separates this surface from
the pale map behind it) and take a **mean**, which the phase noise averages out
of, rather than a maximum or a pixel count. The numbers in the bank-field section
below were taken that way over the `edge-lake` and `edge-canal` presets: what one
term is worth is the mean luminance difference between a run with it and a run
with it neutralised, measured over the water in the frame.

## The bank is in the mirror, without a reflection pass

Nothing in the scene is *actually* reflected — the environment map is the
atmosphere, so the water reflects the sky and nothing else, and real reflections
need a second pass or deferred rendering for bevy's SSR. This client is
forward-rendered on purpose.

The dominant term is not the detail of the reflection, though. It is that the
mirror is **dark where it is full of quay and bright where it is full of sky**.
The reflected ray leaves horizontally away from the camera and climbs at the
angle the view descends, so it clears a bank-side building after
`height · cot(elevation)`; probing a distance field that far along it answers the
question.

**Which field it probes is the whole of it, and the first version got it wrong.**
It read the *shore* field — distance to the bank — and the shore field can only
ever answer "this is land". So the term asserted that a storey or two of quay
stands on every shoreline there is, which over a city canal is true and over a
lake with a mown bank is not, and over the lake it drew a rim of shadow hugging
the shore. It was argued at the time that being view-dependent was what stopped
it being that rim: water by the near bank probes across the water and sees sky,
water by the far bank probes onto the land behind it. That is true and it is not
enough. From a few hundred metres up the camera direction barely changes across
the frame, so "the far bank" is the same side of every water body at once, and a
band on one consistent side of every shore is exactly what a cast shadow looks
like — which is how it was reported.

Measured on deck-gpu, 2026-09-11, as the mean over the frame's water of
neutralising the term (see the A/B section above):

| preset | the term as it was | after the bank field | removed |
|---|---|---|---|
| `edge-lake`  (open banks, one marina) | +3.62 | +0.27 | **93 %** |
| `edge-canal` (Groningen, built both sides) | +5.10 | +1.83 | 64 % |

That split is the whole point, and the 64 % is not a shortfall: `edge-canal`
looks down at 30°, where the ray is 9 m up only 16 m back, and a Groningen quay
routinely has a road before the houses. What survives is the water in front of
the buildings, which is what a reflection of them would be.

So there is a second, coarser field now — `ShoreField::bank`,
[`BANK_FIELD_PX`] = 256 against the shore field's 1024, 64 KiB a cell against
1 MiB — seeded from the tile's **buildings** layer, and that is what the probe
reads. A quay darkens the water in front of it; a lake bank with nothing on it
does not darken anything, which is the honest answer and also the one that ends
the rim.

Buildings only, deliberately. The `land` layer's wood and park polygons are
canopy *areas* whose edge is where the trees begin, a treeline dims a reflection
rather than stopping it, and seeding the transform with them would put most of
the rim straight back. 256 texels is ~5.9 m at z14, which is finer than the
smallest building in the tile and far finer than the 12 m the term softens its
own answer over — the shore field needs its 1.5 m to resolve a 10 m canal, and
this one is read at a single point.

Looking straight down the reach goes to zero and the term switches itself off
either way — correct, because straight down the mirror holds the zenith.

It dims **F0**, not the lit colour: a building blocks the sun's image as surely as
it blocks the sky's, and scaling `reflectance` is the only way to reach a specular
`apply_pbr_lighting` owns (`F0 = 0.16 · reflectance²`, hence a square root). And a
probe leaving the cell would read a clamped edge texel — an answer about the wrong
place, and a seam along every cell boundary — so off the edge it does not apply.

One texture sample and about ten instructions.

The reach is `height · cot(elevation)` and so grows without bound as the view
flattens — 31 m at 16° above the water, 129 m at 4°, half a kilometre at 1° — so
`BANK_REACH_MAX_M` caps it. Set by the shallowest angle the test views use, not by
taste: at 45 m the grazing lake frames were truncated to a third of their real
reach, which is exactly where a far shore belongs in the mirror.

## Real reflections: the two routes, and why neither was taken

Everything about that reflection except its brightness is missing: a quay reads as
a dark band rather than as a quay, and nothing moving is in it at all. Both ways
to fix that properly were costed on 2026-09-10 and **both were declined** — not
because they do not work, but because the approximation above already carries the
part of the effect the eye reads.

**Bevy's own `ScreenSpaceReflections` is not one of the routes.** It requires
deferred rendering and a `DeferredPrepass` (`bevy_pbr::ssr`, and its own doc
comment says so); this client is forward-rendered on purpose.

* **Screen-space, by hand.** A material returning `reads_view_transmission_texture
  = true` is handed `view_transmission_texture`, a snapshot of the scene as drawn
  just before it — the buildings, quays and trees. March the depth buffer in the
  reflected direction, sample that, fall back to the environment map on a miss.
  Costs a full-screen copy plus the march. Two things make it more than a shader
  change: it moves water from the opaque pass into `Transmissive3d`, which is the
  coplanar ground ordering this tree has been bitten by repeatedly, and it needs a
  `DepthPrepass`, which **this client no longer has at all** — ambient occlusion
  was the only thing that asked for one and was removed on 2026-09-21
  (`surface-finish-and-occlusion.md`). Adding one back is now its own decision
  with its own consequences, not a component to insert: see
  `the-city-stops-at-a-line.md`, whose building fade depends on there being no
  second pipeline drawing each mesh.
* **Planar.** A second camera mirrored about the water plane rendering into a
  quarter-resolution image, sampled by screen UV. Reflects off-screen geometry,
  which screen-space cannot, and needs no pass move — but it is a second scene
  traversal every frame.

If this is revisited, screen-space is the better trade for a city viewer: the
reflection you actually want is the far side of a canal, and that is nearly always
on screen.

Where there is **no environment map at all** — an adapter without
`FLOAT32_FILTERABLE` (`sky-without-an-atmosphere.md`), `CARTO_NO_ENVMAP`, sky
lighting off — the standard path has no reflection to give, so the shader stands
in with the `DistanceFog` haze colour under `#ifndef ENVIRONMENT_MAP`, weighted by
the same `reflected` the body colour was taken out of. Not compiled at all when
there *is* an environment map.

**That branch is what Android draws, not an edge case.** Android resolves to the
`balanced` rung, and `balanced` has `sky_ibl: false` (the cubemap is re-baked
every frame), so no environment map — and neither does the laptop. Reflecting one
haze colour at every angle made the reflection independent of the wave normal:
the only thing left to show a wave was the fresnel weight, and a phone
screenshot of the Paterswoldsemeer (2026-09-26) was uniform grey corduroy under a
blue sky. The reflected ray now looks up a two-stop sky — haze at the horizon,
`SKY_ZENITH_TINT` of it overhead, `sqrt(elevation)` between, drifting to the
cosine-weighted mean with roughness. The zenith is derived from the haze, so the
clock and the dusk tint come free; only cloud cover is sent (`wind.w`, from
`sky::overcast`), because an overcast sky has no blue in it.

Bevy's *ambient* reflects too (`EnvBRDFApprox` in `ambient.wgsl`, one colour from
every direction, raised to 4000 lux on these tiers for exactly this sheen), and
that flat third of the reflection washed the gradient out. The shader recomputes
it with bevy's own functions and removes it **in proportion to
`sky_t × clear`**. Both limits were measured on the Deck at the `balanced` rung:
removed everywhere, an overcast lake went dark navy under a grey sky and a dusk
lake went darker than its own horizon, because under cloud the ambient *is* the
sky and at the horizon it is the brightness the haze colour alone is short of.
Scaled, mean water colour over a fixed crop:

| view | before | after |
|---|---|---|
| `sunpath` (clear, 15:00) | 101, 107, 107 | 91, 100, 105 |
| `canal` (clear, 11:00) | 66, 81, 88 | 55, 74, 86 |
| overcast, same view | 149, 160, 167 | 149, 160, 167 |
| `lowsun` (17:12) | 199, 162, 128 | 198, 160, 125 |
| `ringedge` (12 km, the palette seam) | 76, 105, 132 | 76, 105, 132 |

The sky, the fresnel and the ambient give-way live in `water_common.wgsl`
(`cartopolis::water_common`), registered as a shader library from
`WaterSeaState::from_world` and imported by both water shaders below.

## The clouds are in the mirror (2026-09-29)

Neither sky the surface reflects has any clouds: the environment map is baked
from bevy's atmosphere, the stand-in above is built from the haze colour, and the
deck is a shell drawn over the sky *after* both. So a lake under a broken sky
reflected a clear one. Each water fragment now finds the deck again along its
reflected ray (`cloud_field::mirrored`, the field and the lighting the shell
itself uses) and puts it in front of whichever sky the reflection holds, at the
weight that sky went in with (`water_common::with_clouds`): with an environment
map, the map's own radiance and bevy's split-sum recomputed from the same inputs;
without one, the stand-in at `reflected × mirror × (1 − foam)`. A cloudless
fragment is left exactly as it was.

Three things were not obvious:

* **The flat map's water past the ring has to do it too**, or a lake's clouds stop
  at the ring's edge — and at grazing angles, where the reflection is strongest,
  most of the visible lake is past it. So every ground material binds the deck's
  buffer (108; the water binds it at 109), and both sides blur by the same
  far-field roughness.
* **That buffer lives at a fixed asset id** (`sky::CLOUD_FIELD_BUFFER`). A storage
  buffer that does not exist is not an empty binding: the material never
  prepares. The ground's materials are built with `..default()` in a dozen places,
  and `Handle::default()` names no asset, so one missed site would be a hole in
  the map. The ground and road extensions' `Default` names the fixed id instead.
* **A rough mirror reflects a cone**, and a cone twenty kilometres long spans whole
  clouds. The deck is sampled with the octaves finer than `2·t·α` muted and, past
  one base octave, its edges widened about their midpoint — so a far, rough lake
  shows soft blotches that average to the cover, not cut-outs.
* **Only the long waves steer the clouds; the short ones blur them.** With the
  full wave normal, the first cut drew the Paterswoldsemeer in a 3.5 m/s breeze
  as grain (Deck, 2026-09-29): a ripple tilts the reflected ray a tenth of a
  radian, which kilometres up is a different cloud, so neighbouring pixels
  flipped between cloud and sky. The sky's own reflection gets away with that
  normal because the sky barely changes over a tenth of a radian. The cloud ray
  now bends on the two long octaves alone (`Waves::grad_lo`) and the short
  octaves' slope variance joins its roughness. Blurring by each pixel's footprint
  on the deck (`fwidth` of where the ray lands) was tried first, alone, and
  changed nothing visible: the footprint of a random sample is still random. It
  is kept for what it is right for — the horizon, and a swell bending rays apart.

**What it costs, measured** (Steam Deck, 1600×900, same build with and without
the two mirror blocks swapped in as runtime shader assets, three alternating
runs each, 2026-09-29):

| view | with | without | cost |
|---|---|---|---|
| Paterswoldsemeer from 30 m, grazing | 17.3 ms | 14.9 ms | **+2.5 ms** |
| the same lake from 150 m | 15.5 ms | 14.3 ms | +1.2 ms |
| the city at sunset, little water | 12.9 ms | 12.7 ms | none |

The first cut, at the shell's full precision, cost about 3.5 ms; the mirror now
runs at most four of the shell's octaves (muted, not dropped — see
`cloud_field::MIRROR_MAX_OCTAVES`), no domain warp, a one-tap sun march and two
scattering octaves, and is not looked up at all where the water reflects under
3 % (`water_common::MIRROR_CLOUD_MIN`, steep views). **The tap is the one saving
that failed:** replacing it with the column's own depth made every cloud fully
self-shadowed, so the reflected deck came out as dark as the sky around it and
the reflection vanished while measuring cheaper. More of the cost than the
arithmetic predicts, and the fact that either mirror alone cost nearly as much as
both in single runs, suggest register pressure in two already-heavy shaders
rather than the noise itself — unproven. The phone tier runs the shell's cheap
counts, read from a flag in the buffer because the water pipeline has no
`CLOUDS_LOW` def. Only the low deck: the cirrus reflection at water's few per
cent is below what a wave does to the picture. **Not yet seen on a phone.**

## Shallows show their bed, without a transparent pass

Inland water was opaque with a brightened edge, so a bank never read as seen
through water. Distance from the bank stands in for depth (`BED_SLOPE` 0.12 m/m,
capped at `BED_MAX_DEPTH_M`), and a silt-and-sand bed shows through by
Beer–Lambert along a down-and-up path through the refracted view ray. The
extinction is **inland**, not ocean: 0.80 / 0.42 / 0.62 per metre, because algae
and peat take the blue out nearly as fast as the red, so a Dutch bank shelves
through olive, not turquoise. Canals are never more than a few metres from a bank
and so read murky olive-brown — asked about and kept, 2026-09-26. Real blending
was rejected for the reasons in *Real reflections* below: the transmissive pass
and a depth prepass. Deep water is untouched to the level, so the far field below
still matches.

## The ring's edge: the tiles are lit as the surface's far field

Past the streamed ring the only water was the clipmap's *painted* colour, matched
to the lit surface by hand at one time of day. Now `TilePayload::decode` packs the
water mask into a vector tile's alpha as **land** coverage (255 = land, so an
image source or a tile without a mask reads as land and is untouched), and
`ground_detail.wgsl` lights those texels with the surface's deep body colour, its
roughness with every octave retired (`α² = surface.x⁴ + wave.x²`, which a test
checks against the shader's own series) and its reflectance, through the same
`water_common` fresnel and sky. `WaterSeaState::far_water` supplies them, at
spawn and whenever `update_water_waves` rewrites the sea. The disk cache key
carries `RENDERED_FORMAT` so a tile drawn before the mask is not read back as
all land.

Three things this needed:

* **Water texels are level.** A relief tile's normal follows the terrain at its
  own quad size, and beside the Afsluitdijk that spreads the dike's slope over
  tens of metres of open water. Matte paint never showed it; glossy water drew a
  dark band on one side of the dike and a bright one on the other.
* **The surface's waves fade out before the edge** (`RING_FADE_M`, 500 m inside
  it). The distance cut is divided by the fade, so at the edge every octave is
  retired and what is left *is* the tile's far field. The shore terms (shelter,
  bed, bank mirror) fade with it. The ring is found in the shader from the cell's
  own corner and size; `RING_RADIUS_CELLS` is held to `map_geometry::CELL_RADIUS`
  by a test.
* Measured at 150 m over the IJsselmeer: the colour step across the edge went
  from 40–50 levels to about 2.

## Water that is there, under the map

Most "holes" in the surface are not missing geometry. A per-cell comparison of
`tessellate_surfaces`' water area against the tile's mask agrees to the third
decimal everywhere it was checked; the surface is drawn **under a clipmap tile**.
The discriminating test is to discard the tile's water texels
(`if water > 0.5 { discard; }` in `ground_detail.wgsl`): the hole turns into
surface.

* **Unregistered water** was flat at the median of its outline, and AHN has no
  return off open water, so the field over it is filled from the banks — which
  beside a dike rises metres above that median. The relief tiles follow the field
  and stood through the sheet: square patches of basemap in the IJsselmeer. Fixed
  by clamping to the ground drawn under it, the rule registered water already had
  (`map_geometry::water_surface`).
* **Still open, 2026-09-26:** small blotches near the Paterswoldsemeer's shore
  show the same symptom *with* registered levels (`water_rings` 2924). Not missing
  geometry; unexplained. With the lit far field they are nearly invisible, which
  is also what makes them hard to find — check with the discard test, not by eye.
  Note that above `FLAT_GROUND_OFF_M` (500 m) the flat ground, water included, is
  never meshed at all, by design.

## Aim a test view with arithmetic

`--look`'s yaw is measured **anticlockwise from north** — yaw 0 faces north, yaw
90 faces west — so a compass azimuth `a` is `yaw = 360 - a`. The sun's own azimuth
and elevation come from `utils::sun::sun_direction(hours, decl)` at
`OBSERVER_LATITUDE_DEG` (52°), which is what the time-of-day slider drives:
15:00 is azimuth 231.8° and 25.8° up, 17:12 is 260.5° and 7.4° up, and 18:36 —
which reads like sunset — is already **5.5° below the horizon** and renders night.

Getting that backwards is how the first version of `docs/water-shots.toml` aimed
both of its "into the sun" views at empty sky, on a machine that costs twenty
minutes a run, and then passed. Compute the number; do not eyeball it.

## Verifying a change here

Colour is not evidence on this container's rasteriser
(`headless-shots-software-renderer.md`) and water is mostly colour. What a capture
*can* settle is relative: the same view before and after. Structure — where foam
lands, whether a glitter path exists, whether the far field is a sheen or noise,
whether there is a bullseye in the lake — transfers; absolute tone does not. The
views used: a canal at `53.21263,6.57016` from 25 m, the Paterswoldsemeer at
`53.1830,6.5490` from 140 m, and 40 m at `53.1880,6.5560` looking into a 15:00
sun. None is in `docs/shots.toml`; none is a README image.
