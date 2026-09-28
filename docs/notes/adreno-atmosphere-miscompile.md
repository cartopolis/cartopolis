---
triggers:
  - "the sky is black on the phone / on the OnePlus / on Adreno"
  - working on `systems::atmosphere_patch` or `assets/shaders/patched/`
  - bumping Bevy (the patched shader must be re-diffed against upstream)
  - considering vendoring or patching a bevy_pbr shader
  - "driver_denies_atmosphere", the sky deny list, SkyPreference::Full
  - proposing another shader rewrite for the Adreno black sky
  - passing a uniform or storage struct by value in WGSL (a whole-struct load)
  - writing a probe that "verifies" an input against a range
updated: 2026-09-28
---

# The Adreno 840 black sky

Since 2026-08-19 the OnePlus 15 (Adreno 840, driver 0842.x, shader compiler
E031.50.19.27, Vulkan 1.4.295, LineageOS 23.2) renders bevy's atmosphere as a
black sky with a full-strength white sun disc and correct city lighting. Since
2026-08-29 **`free_camera::driver_denies_atmosphere`** makes Automatic sky pick
the analytic dome on this adapter; `SkyPreference::Full` (Settings ▸ Graphics
▸ Sky) bypasses the deny, so the atmosphere can be re-tested with one settings
flip and a restart.

**Status, 2026-09-28: the 2026-08-29 conclusion below is withdrawn, pending one
device test.** Re-read, the probe rounds do not show a compiler miscompiling
identical arithmetic. Their readings are exactly what the shader produces when
it places the camera *above the atmosphere*, where a black sky is the correct
answer — and bevy's shader has one place that would put it there on a driver
that mishandles a by-value struct.
The first section says why; the elimination record after it is kept as the
history it is.

## What the probes actually measured (2026-09-28)

Probe v4/v5 (below) lit band 6 with bands 1–5 dark. Two of those dark bands
were weaker than they read:

- **Band 2** ("camera radius wildly outside the shell") only fired below half
  the inner radius or above *twice* the outer radius. A camera just above the
  top of the air passes it.
- **Band 4** ("the intersector misses for the live ray") only ran when
  `r < atmosphere.outer_radius`. For a camera at or above the top of the air
  it never ran at all — "its arm passed whenever evaluated" (v2) was literally
  true.

With the camera 1 m above the shell, every upward ray has zero air to cross,
so `t_max <= 0` is the *right* answer, in every formulation — which is why four
rewrites of `max_atmosphere_distance` all "failed", and why the v5 integrator,
recomputing the limit itself from its own camera position, got the same zero.
The alternative reading needs the compiler to evaluate one expression on one
input two ways inside one function (band 4's `live_roots.y > 0` against v5's
`top_l.y <= 0`); this one needs no miscompile of arithmetic at all. It also
explains the fact the August work kept circling: the fault keys on the camera
radius — broken inside the shell, right from orbit.

**What lifts the camera.** Upstream's `get_view_position()` ends in
`clamp_to_surface(atmosphere, pos)`, passing the whole `atmosphere` uniform by
value — one load of the entire struct:

```wgsl
struct Atmosphere {          // byte offset
    ground_albedo: vec3<f32>,  // 0
    inner_radius: f32,         // 12, in the vec3's padding
    outer_radius: f32,         // 16
    world_to_atmosphere: mat4x4<f32>, // 32
}
```

A driver that lays the loaded value out with the `vec3` taking 16 bytes reads
`inner_radius` from byte 16 — the *outer* radius — so the clamp's
`min_radius = inner_radius + 1 m` lifts every camera inside the air to 1 m
above its top. The same copy is passed to `distance_to_top_atmosphere_boundary`
(its `outer_radius` from byte 20 is padding, zero, so every upward ray has zero
distance: the transmittance LUT integrates nothing, T = 1, a sun disc that
never reddens) and to `transmittance_lut_r_mu_to_uv`. Every symptom on record
follows, and nothing the probes read contradicts it.

Why a driver might: wgpu emits **SPIR-V 1.6** on any Vulkan 1.3+ device, and
naga uses the buffer's explicitly-laid-out struct type for the by-value
parameter too. From SPIR-V 1.5, Vulkan forbids explicit layout on
function-local types (spirv-val's check for `VUID-StandaloneSpirv-None-10684`,
SPIRV-Tools #6792), so a compiler may assume a function-local struct has its own
natural layout. Adreno 8xx compilers are on record mis-optimising buffer loads
(dusklight #750, fixed by changing the *shape* of storage loads with no root
cause found; bevy #24926, joint matrices read wrong from storage buffers on
Adreno 730/650). That makes the mechanism plausible; it is not proof, and
nothing here has a real Adreno.

**What changed.** Round 2 of `atmosphere_patch`: no atmosphere pass loads the
`atmosphere` uniform whole any more — `get_view_position` inlines the clamp,
and the transmittance-LUT and top-of-atmosphere helpers read the fields through
the uniform (`distance_to_top_of_atmosphere`, `transmittance_lut_uv`). Same
values on a correct driver; `cargo shots` keeps verifying that.

**The test.** Settings ▸ Graphics ▸ Sky ▸ Full atmosphere, restart the app,
look at a daytime sky. Blue: remove the 840 from `driver_denies_atmosphere`.
Black: this explanation is wrong too — and the next probe arm is the one the
old probe lacked, `r >= outer_radius` at the integrator, painted.

**Still loading the struct whole, outside the sky passes.** `pbr_lighting.wgsl`
(`let atmosphere = view_bindings::atmosphere;`, a storage buffer) and
`volumetric_fog.wgsl` pass it to `clamp_to_surface`,
`calculate_visible_sun_ratio` and `transmittance_lut_r_mu_to_uv` to dim the sun
on lit surfaces. If the device confirms the mechanism, that path is suspect too
— it would sample the transmittance LUT at NaN coordinates — but it cannot be
reached from `functions.wgsl`, and patching it means carrying a copy of
`pbr_lighting.wgsl`, so it waits for evidence.

**The lesson for the next probe.** A range check is not a verification. Band 2
accepted a radius off by the thickness of the whole atmosphere, and band 4
verified nothing on the pixels that mattered because its guard excluded them.
Check an input against its *expected value* (here: the camera altitude the app
put there, to the metre), and never let an arm's precondition skip the case
under test silently.

## The 2026-08 record

What follows was written on 2026-08-29. Items 1–4 stand. From 5 on, every
"verified healthy" rests on the range checks the section above takes apart.

### What was eliminated, in order (each confirmed on the device)

1. **Missing `FLOAT32_FILTERABLE`** — the 840 advertises it; `sky chosen`
   logs `Atmosphere`.
2. **The inner-radius f32 cliff** — black with the 4.5 km margin and without.
3. **Control-flow shapes in the integrator** (PR #152: branchless segment
   selection, u32 loop counter, guarded horizon sqrts) — no change.
4. **Medium LUT texels flushed at reduced filter precision** — the texels were
   baked 2^16 larger with the shader dividing it back out (`MEDIUM_TEXEL_SCALE`
   on the `fix/adreno-medium-texel-scale` branch); no change. The visible sun
   disc through a scaled-up medium also proves whichever shader ran saw an
   effectively-zero medium — the read location was right, the mechanism wrong.
5. **Probe v1** (`diag/adreno-sky-probe`, APK ba1e7e6c): the settings uniform,
   the lights list and a literal-uv medium sample are all **healthy** on the
   device while the sky integrates to zero.
6. **Probe v2/v3** (ed53146, 6fda282): walked the march-limit chain bottom-up.
   Atmosphere radii sane, camera radius sane, the ray-sphere intersector
   correct on hardcoded and live outer-sphere rays — yet the caller's
   `t_max <= 0`. Rebuilding the Bruneton helpers on the intersector (01f32c3)
   changed nothing.
7. **Probe v4** (05f13fc, positional band readout — hue judgement had failed
   us; v3's blown-out red and pink tonemapped identically): band 6. Every
   ingredient of `max_atmosphere_distance` verified healthy **inside
   `raymarch_atmosphere`'s own context**, including the inner-sphere
   intersection and — via the probe's own bright-band-over-dim-wash paint —
   `step` and `mix` themselves. The caller's result from the same ingredients:
   still `<= 0`.
8. **Probe v5** (3314d15): `max_atmosphere_distance` reduced to add/mul/min/
   max alone (no bool, no `f32(bool)`, no select, no step, no mix), and the
   integrator recomputing the limit from its in-context-verified ingredients,
   substituting any implausible caller value. **Still band 6.** The
   substitution itself ran through the verified context and still produced
   `<= 0` — at which point four structurally unrelated formulations had failed
   in one inline position while the identical arithmetic verified correct two
   inline sites away in the same entry point.

### The conclusion (withdrawn 2026-09-28, see the top)

The driver miscompiles **by inline site**, not by construct. The same
expression graph is correct in one function's inlining and collapses in
another's, across every source-level shape tried. An application cannot dodge
that: every rewrite just re-rolls the inliner's dice, each roll costs a
build-install-look round trip on a physical phone, and a "fix" that works
would be one driver update away from breaking again. Five instrumented rounds
is where the expected value went negative; the deny-list entry is the
engineering answer.

The probe machinery is preserved on `diag/adreno-sky-probe` (bands 1–9 with
`SKY_PROBE` in the patched `functions.wgsl`, verified to render a normal sky
on lavapipe): if anyone resumes this — say, on a new driver — the harness that
names the failing stage in one screenshot already exists.

## Standing machinery

- `atmosphere_patch` (main) swaps the patched `functions.wgsl`: the #152
  hardening plus, since 2026-09-28, the no-whole-struct-load hunks —
  arithmetic-identical everywhere, verified continuously by `cargo shots`;
  re-diff against upstream on every Bevy bump (unit tests pin the hunks and
  that no code in the module passes the uniform by value).
- `driver_denies_atmosphere` (this change): one entry per **measurement**,
  never a family — the repo already learned that lesson once with the deleted
  `gpu_affords_atmosphere` table. Add a line only with a dated on-device
  probe result recorded here; remove one only after `SkyPreference::Full`
  shows the atmosphere working on that adapter.
- The `fix/adreno-medium-texel-scale` branch holds the (unmerged, correctness-
  neutral) texel rescale, which protects any future device whose filter path
  narrows to fp16 — worth revisiting if bevy #25335-class devices matter.

## Upstream reports worth filing

- **bevy**: #25335 covers devices *without* `FLOAT32_FILTERABLE`; the 840
  advertises it and loses the atmosphere to a driver miscompile instead. The
  useful artifacts: the elimination record above, and the observation that
  per-metre medium coefficients sit entirely below fp16-normal (every
  reference implementation stores per-km) — an upstream bake-side scale
  immunises a class of mobile filter paths.
- **bevy, if the device confirms round 2**: the atmosphere shaders pass the
  `atmosphere` uniform by value in three places a field read would do, and
  `pbr_lighting.wgsl` copies the storage-buffer struct into a local. Reading
  fields is free and removes the dependency on how a driver lays out a copied
  struct.
- **naga / Qualcomm, same condition**: a minimal reproducer is a uniform struct
  `{vec3, f32, f32}` passed by value to a function that returns the `f32` at
  byte 12, run on compiler E031.50.19.27.
- (Written 2026-08-29, before the re-reading:) **Qualcomm**: the minimal
  reproducer is `max_atmosphere_distance` returning `<= 0` for
  inside-the-shell rays in specific inline sites — the probe branch is the
  test harness. Other projects report miscompiles on this same driver
  (dusklight #750/#846), so a driver fix may simply arrive; re-test via
  Settings ▸ Graphics ▸ Sky ▸ Full.
