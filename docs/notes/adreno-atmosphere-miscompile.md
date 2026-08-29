---
triggers:
  - "the sky is black on the phone / on the OnePlus / on Adreno"
  - working on `systems::atmosphere_patch` or `assets/shaders/patched/`
  - bumping Bevy (the patched shader must be re-diffed against upstream)
  - considering vendoring or patching a bevy_pbr shader
  - "driver_denies_atmosphere", the sky deny list, SkyPreference::Full
  - proposing another shader rewrite for the Adreno black sky
updated: 2026-08-29
---

# The Adreno 840 black sky: a per-inline-site miscompile, and why we stopped

Since 2026-08-19 the OnePlus 15 (Adreno 840, driver 0842.x, shader compiler
E031.50.19.27, Vulkan 1.4.295, LineageOS 23.2) renders bevy's atmosphere as a
black sky with a full-strength white sun disc and correct city lighting. As of
2026-08-29 the resolution is **`free_camera::driver_denies_atmosphere`**:
Automatic sky picks the analytic dome on this adapter, and
`SkyPreference::Full` (Settings ▸ Graphics ▸ Sky) deliberately bypasses the
deny so a future driver can be re-tested with one settings flip. This note is
the complete elimination record — eight hypotheses, five of them killed by an
on-device probe that painted its diagnosis into the sky — so nobody proposes
rewrite number nine without reading what happened to one through eight.

## What was eliminated, in order (each confirmed on the device)

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

## The conclusion

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

## Standing machinery this left behind

- `atmosphere_patch` (main) still swaps the patched `functions.wgsl` with the
  #152 hardening — arithmetic-identical everywhere, verified continuously by
  `cargo shots`; re-diff against upstream on every Bevy bump (a unit test
  pins the hunks).
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
- **Qualcomm**: the minimal reproducer is `max_atmosphere_distance` returning
  `<= 0` for inside-the-shell rays in specific inline sites, compiler
  E031.50.19.27 — the probe branch is the test harness. Other projects report
  unrelated miscompiles on this same driver (dusklight #750/#846), so a driver
  fix may simply arrive; re-test via Settings ▸ Graphics ▸ Sky ▸ Full.
