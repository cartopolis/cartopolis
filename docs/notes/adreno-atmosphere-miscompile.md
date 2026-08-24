---
triggers:
  - "the sky is black on the phone / on the OnePlus / on Adreno"
  - working on `systems::atmosphere_patch` or `assets/shaders/patched/`
  - bumping Bevy (the patched shader must be re-diffed against upstream)
  - reading `globe::earth_inner_radius` or the radius-cliff comments
  - considering vendoring or patching a bevy_pbr shader
updated: 2026-08-24
---

# The Adreno 840 black sky, and the patched atmosphere shader

Since 2026-08-19 the OnePlus 15 (Adreno 840, driver 512.842.x) has rendered a
black sky at ground level with a correct sun disc, correct city lighting and a
correct atmospheric limb from orbit. Two shipped hypotheses failed on the
device before this one: the `FLOAT32_FILTERABLE` gate (the 840 has the
feature; `sky chosen` says `Atmosphere`) and the inner-radius f32 cliff
(`earth_inner_radius`'s 4.5 km margin was the Android default and the sky was
black with it in force — the A/B switch shipped 2026-08-23 confirmed black in
both positions). This note is the diagnosis that survived elimination and the
shape of the fix, so nobody re-walks the three weeks.

## The elimination

Fixed points, each load-bearing:

- **The sun disc renders.** `render_sky.wgsl` computes it in the same fragment
  as the sky and adds `sun_radiance * transmittance`. A visible disc proves the
  pass runs, the depth read is right (`depth == 0.0` branch taken), the
  transmittance LUT is sane, the view transforms and exposure are sane, and the
  result is not NaN (NaN swallows the disc too). The measured state is exactly
  `inscattering ≈ 0, transmittance ≈ 1`.
- **Forced ground raymarch is also black** (`CARTO_GROUND_RAYMARCH`, from the
  Settings ▸ Graphics ▸ Developer switches, 2026-08-24). That bisects less than
  it looks — the sky-view LUT compute pass *is* a call to
  `raymarch_atmosphere` — but it does prove the fault is inside the shared
  integrator, not in the sky-view UV parameterization that the earlier
  radius-cliff theory blamed.
- **Ground and orbit run identical code on this device.** The handset's tier
  (`Budget::balanced`) has `space_raymarch: false`, so both tiers use
  `LookupTexture`; and the app's density ramp keeps the optical column constant
  across the altitude bands. The only input that differs is the camera radius.
  Broken at `r ≈ inner_radius`, correct at orbit ⇒ the fault keys on `r`.
- **The handset's own tier renders a blue sky on lavapipe**
  (`CARTO_QUALITY=android`, container, 2026-08-24) — no app-side setting on
  that rung blacks the sky. It is the driver.

What is r-dependent inside `raymarch_atmosphere` and produces exactly
`{inscattering: 0, transmittance: 1}`: `get_raymarch_segment` handing back an
empty segment, or the march loop running zero times. Ground takes the
inside-the-atmosphere arm of an `if`/`else if`/`else` chain that merges
straight into `for (var s = 0.0; s < sample_count; s += 1.0)` — a float
counter against a non-constant float bound; orbit takes the outside arm.
Qualcomm's compiler has a documented history with precisely that shape
(branches converging into a loop miscompile: ggml-org/llama.cpp#5186; Blender
disabled SPIR-V optimization wholesale for Qualcomm, blender/blender#129775),
and this same phone's driver has confirmed geometry miscompiles in other
projects. No competing hypothesis explains all four fixed points:
"medium-LUT texels flush to zero" (bevy #25335 — real on devices *without*
`FLOAT32_FILTERABLE`) predicts a black limb from orbit too, and every
LUT-parameterization NaN dies on the raymarch result.

## The fix

`systems::atmosphere_patch` swaps the loaded
`bevy_pbr::atmosphere::functions` `Shader` asset for
`assets/shaders/patched/bevy_pbr_atmosphere_functions.wgsl` — a byte copy of
upstream 0.19.0 with three hunks, each tagged `PATCHED (cartopolis)`:
`get_raymarch_segment` rewritten as `select`s (no branch merge), the march
loop counted in `u32` against `ceil(sample_count)` (trip count and arithmetic
unchanged on a correct compiler), and `sqrt(max(r*r − inner², 0.0))` guards on
both sky-view horizon terms (the guard `bruneton_functions.wgsl` always had;
belt-and-braces in case the on-device A/Bs never actually arrived).

Load-bearing details:

- **The swap is an asset replacement, not an embedded-registry insert.**
  `load_shader_library!` registers *and* async-loads the module inside
  `AtmospherePlugin::build`, so a registry insert from any later plugin races
  the load. Replacing the `Assets<Shader>` entry fires `Modified`, which
  `PipelineCache::extract_shaders` answers by re-composing every dependent
  pipeline — the shader-hot-reload path, race-free.
- **It runs on every platform.** The patch is arithmetic-identical on a
  correct compiler, so lavapipe and desktop render through it too — every
  `cargo shots` run verifies it, instead of it being a phone-only branch
  nobody can see working. Verified 2026-08-24: patched and stock ground shots
  have identical top-band statistics on lavapipe, and a deliberately-reddened
  probe build confirmed the injected bytes are what the atmosphere compiles.
- **On a Bevy bump, re-diff the patched file against the new upstream
  `functions.wgsl`** and re-apply the three hunks. A unit test pins the hunks'
  presence (`atmosphere_patch::tests`), so a forgotten re-apply fails the
  suite rather than silently shipping a stock copy under the patched name.

## Status and the honest limit

**Not yet confirmed on the device.** No harness here has a real Adreno — the
VPS emulator renders on SwiftShader, which cannot reproduce a Qualcomm
compiler bug. The diagnosis is elimination plus precedent, not a captured
miscompile. If the next device run still shows a black sky, the surviving
suspects are (in order): the same class of miscompile in a function the patch
did not restructure (`ray_sphere_intersect`, `sample_local_inscattering`), and
the `Rgba32Float` medium LUTs filtering wrong *despite* the advertised
feature — for which the workaround is a `Nearest` medium sampler or the
kilometre-unit rescale (`Atmosphere` docs bless a scaled `GlobalTransform`;
radii in km move every texel ×1000 out of fp16-subnormal range). Both are in
the 2026-08-24 research trail (PR the fix landed in) with sources.

The upstream report worth filing: bevy #25335 currently reads as "devices
that refuse `FLOAT32_FILTERABLE`"; the 840 is a device that *advertises* it
and still renders a black sky, and the unguarded horizon `sqrt` in
`functions.wgsl` is a one-line upstream fix regardless of which hypothesis
holds.
