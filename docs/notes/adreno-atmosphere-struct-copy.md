---
triggers:
  - "the sky is black on the phone / on the OnePlus / on Adreno"
  - "buildings too bright on the phone", "no sunset colour on buildings"
  - working on `systems::atmosphere_patch` or `assets/shaders/patched/`
  - bumping Bevy (the patched shaders must be re-copied; a debug build panics until they are)
  - considering vendoring or patching a bevy_pbr shader
  - passing a uniform or storage struct by value in WGSL (a whole-struct load)
  - writing a probe that "verifies" an input against a range
  - a GPU-specific rendering bug that "must be a compiler miscompile"
updated: 2026-09-28
---

# The Adreno 840 black sky: bevy copies a struct the driver reads wrong

From 2026-08-19 to 2026-09-28 the OnePlus 15 (Adreno 840, driver 0842.x,
shader compiler E031.50.19.27, Vulkan 1.4.295) drew bevy's atmosphere as a black
sky with a full-strength white sun disc over a normally lit city. For a month it
was blamed on the driver miscompiling arithmetic, and the phone was put on a
deny list that gave it the fallback dome (`systems::sky_dome`). The cause was
elsewhere, and the fix is on our side.

## The cause (confirmed on the device, 2026-09-28)

bevy_pbr 0.19 hands its `Atmosphere` binding to helper functions **by value** —
`clamp_to_surface(atmosphere, pos)`, `transmittance_lut_r_mu_to_uv(atmosphere,
r, mu)`, `distance_to_top_atmosphere_boundary(atmosphere, r, mu)`,
`calculate_visible_sun_ratio(atmosphere, ..)` — and naga compiles each such call
into one load of the entire struct:

```wgsl
struct Atmosphere {          // byte offset
    ground_albedo: vec3<f32>,  // 0
    inner_radius: f32,         // 12, in the vec3's padding
    outer_radius: f32,         // 16
    world_to_atmosphere: mat4x4<f32>, // 32
}
```

The Adreno 840 reads the copy wrong. Everything on record fits the copy's
`inner_radius` coming out as the *outer* radius (what a layout taking the vec3 as
16 bytes would read), and `outer_radius` as zero:

- `get_view_position` ends in `clamp_to_surface`, whose `min_radius = inner_radius
  + 1 m` then lifts every camera inside the air to 1 m above its top. From there
  no upward ray crosses any air: **black sky**, transmittance 1, a sun disc at
  full strength. From orbit the clamp does not fire, which is why the fault
  "keyed on the camera radius alone".
- The transmittance lookup table integrates along `distance_to_top..`, which with
  an outer radius of zero is zero for every upward ray: T = 1 everywhere, **a sun
  that never reddens** at sunset.
- `transmittance_lut_r_mu_to_uv` computes `sqrt(outer² − inner²)` from the copy —
  a NaN coordinate, and the speckle and flashes reported over the map on
  2026-08-19.
- The same copies dim the sun on every lit surface (`pbr_lighting.wgsl`) and in
  the light shafts (`volumetric_fog.wgsl`), so with the sky fixed the city came
  out **too bright** and would not go orange at sunset — the second report, same
  day.

Faking exactly that misread on lavapipe reproduces the phone: a black sky over a
lit city with a pale band at the horizon, and, with the August probe built in, the
probe's own verdict (below). The first shipped fix for it turned the phone's sky
blue.

Why this driver, when desktop GPUs and lavapipe are fine, is not established.
wgpu emits SPIR-V 1.6 on any Vulkan 1.3+ device, naga reuses the buffer's
explicitly laid-out struct type for the by-value value, and from SPIR-V 1.5
Vulkan forbids explicit layout on function-local types (spirv-val's check for
`VUID-StandaloneSpirv-None-10684`, SPIRV-Tools #6792) — a plausible place for a
compiler to assume its own layout. Adreno 8xx compilers are on record
mis-optimising buffer reads elsewhere too (dusklight #750, bevy #24926).

## The fix: never load the struct whole

`systems::atmosphere_patch` swaps three of bevy_pbr's shaders for byte copies
with tagged hunks (`PATCHED (cartopolis)`) that read the fields through the
binding instead — the same values on any driver that copies structs correctly,
which the headless captures and the CI smokes check continuously:

1. `bevy_pbr::atmosphere::functions` — every atmosphere pass (the sky, its lookup
   tables, the sky-reflection cubemap). Also guards the sky-view horizon `sqrt`
   against a negative, as bevy's sibling expression always has.
2. `bevy_pbr::lighting` — the air's dimming of sunlight on lit surfaces.
3. `volumetric_fog.wgsl` — the same for the light shafts.

**On a Bevy bump**, each copy has to be re-made from the new file and its hunks
re-applied. The module records the FNV-1a hash of each stock file it was copied
from and refuses to swap a stock shader that no longer matches — a debug build
panics with instructions, so `cargo shots` and the CI smokes fail at once; a
release build logs an error and runs bevy's own shader. Swapping a copy of an old
file into a new renderer would otherwise be silent. `ssr` and `deferred` reach the
sun dimming through `bevy_pbr::lighting`, so they are covered; nothing else in
bevy_pbr 0.19.1 passes the struct by value.

Upstream deserves the report: three field reads instead of a copy would remove
the need for all of this. Not filed yet.

## Why it took a month: the probe verified nothing where it mattered

Five on-device probe builds (2026-08-27..29, `diag/adreno-sky-probe`) painted
the first failing check into the sky. The last two lit band 6 — "the march limit
is ≤ 0" — with bands 1–5 dark, and were read as every ingredient of that limit
being verified healthy while the arithmetic built from them failed: a compiler
miscompiling identical expressions by inline site. Four rewrites of the
expression all "failed", which looked like proof.

Two of the dark bands were weaker than they read:

- **Band 2** ("camera radius wildly outside the shell") only fired below half the
  inner radius or above *twice* the outer one. A camera 1 m above the top passes.
- **Band 4** ("the intersector misses for the live ray") only ran when
  `r < outer_radius`. With the camera above the air it never ran at all.

With the camera above the air, `t_max ≤ 0` is the *correct* answer to every
rewrite, which is why they all failed. The lesson for the next probe: a range
check is not a verification — check an input against the value it should have
(here, the camera altitude the app set, to the metre) — and never let an arm's
precondition silently skip the case under test.

Eliminated before that, and still right: missing `FLOAT32_FILTERABLE` (the 840
has it); the medium's texels flushed by reduced-precision filtering (baking them
2^16 larger changed nothing, `fix/adreno-medium-texel-scale`). The workarounds
built for the wrong diagnosis were removed with the fix: the deny list, the
handset's separate planet radius for the atmosphere (it bought a faint ring below
the horizon for a precision problem that was never the cause), the
`CARTO_GROUND_RAYMARCH` / `CARTO_ATMO_*_RADIUS` switches, and the control-flow
rewrites in the patched `functions.wgsl`.
