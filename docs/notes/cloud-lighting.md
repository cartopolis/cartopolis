---
triggers:
  - working on assets/shaders/clouds.wgsl or `update_clouds` / `CloudMaterial`
  - "the clouds look flat / painted / wrong at sunset"
  - considering raymarched or volumetric clouds
  - changing the sun colour, the fog colour, or `utils::sun`
  - adding a cloud layer, or wondering why there are two
---

# Cloud lighting: why the deck is a scattering model, not a shaded surface

The clouds were rewritten on 2026-08-08 from a Lambert-shaded noise field to a
single-scattering model with a phase function. The geometry did not change — it
is still one static camera-centred shell projecting view rays onto flat planes —
and neither did the noise. **All of the difference is in the lighting**, and this
note is why, because the shader can only say what it does.

## The governing fact: don't reach for a raymarch

The prior art worth studying here is the generation of real-time skies that were
admired specifically for how their clouds caught the light. Reading their shipping
shaders, the striking thing is how little of it was cloud *geometry*: several were
projected 2D noise planes, architecturally the same thing we have, and at least
one shipped that way after its authors had working volumetrics in hand.

**Everything that makes a cloud read as cloud is affordable in a fragment
shader.** A projected plane with a scattering model beats a volumetric with a
surface model, at a fraction of the cost. That is the load-bearing conclusion of
the whole exercise, and it is the reason to be suspicious of any future proposal
that starts by adding a march through a 3D density field.

## The five mechanisms, and why each is here

1. **Cloud colour must be an output of the sky model.** If the deck carries its
   own palette it will drift from the sky, and ours did: `update_clouds` used to
   hand-author `lit_color` / `shadow_color` as CPU-side lerps sitting next to
   Bevy's physical atmosphere. They now come from `utils::sun`, which is also what
   `sky::sun_light` and `free_camera::distance_fog` are built from, and the haze
   colour is read straight off the camera's own `DistanceFog`.

   **"Built the same way as" is not the same as "is", and the gap was visible**
   (2026-08-16). The ambient term survived that clean-up as its own constant —
   `sky_day = (0.38, 0.44, 0.55)`, assembled out of the right ingredients, in the
   right shape, next to a comment saying it therefore agreed with the fog. It did
   not: the fog colour is ~(0.46, 0.52, 0.65) linear, and after the shader's
   `SKY_FLOOR` an opaque core landed at ~0.15 — under a third of the horizon it
   was supposed to recede into. On a fully overcast day that is the entire sky, so
   the phone drew a mid-grey lid butted against a near-white horizon band, with a
   hard step between them. Both values live in the same post-exposure linear space
   (bevy's `apply_fog` mixes toward `DistanceFog::color` *after* exposure, and
   `horizonhaze.wgsl` paints it un-exposed to match), so there was no unit
   mismatch to explain it — just two different greys. The deck's ambient is now
   the camera's fog colour itself, floored at a night value (see below).

   The one place cloud and fog must *not* agree is night: a cloud base over a city
   catches its glow from below, and matching the fog's near-black night colour
   makes the deck darker than the sky behind it — black holes in the star field.
   `CLOUD_NIGHT_FLOOR` is that exception, and now the only one.

   **The sky floor has to ride coverage.** A lone cumulus has a dark base: it is
   optically deep, lit from above, over ground that bounces little back. A solid
   overcast has a bright one — it is thinner, and it *is* the sky, with no
   brighter sky beside it to be dark against. One constant cannot be both, and the
   old single 0.35 was the cumulus answer applied to overcast. `cloud_sky_floor`
   ramps it over the top of the coverage range (`smoothstep(0.55, 1.0)`), so 30 %
   cover looks like 10 % cover and only a genuinely closed sky changes character.
   It is decided on the Rust side and shipped in the previously-unused alpha of
   `haze_color`: it is a lighting decision, and the shader is where shape lives.

2. **The sun must be extincted to the deck's altitude, not the ground's.**
   `utils::sun::sun_transmittance_at` applies the barometric factor `exp(-h/H)`,
   H = 8500 m. This is why a sunset deck catches pink and gold while the city
   under it has gone blue — and it is a *colour* difference more than a brightness
   one, because Rayleigh strips blue first and there is less of it left to strip up
   there. Unit-tested (`the_sun_is_brighter_and_bluer_above_the_haze`). The usual
   real-time shortcut is a flat "clouds are above half the air" fudge factor; the
   barometric term costs the same and is right.

3. **The silver lining is a view-dependent phase function.** A Henyey-Greenstein
   dual lobe (`g = 0.8` forward, `-0.3` back). **This is the biggest single
   contributor to the look.** The old shader had no view-dependent term at all;
   `dot(gradient_normal, sun)` is a Lambertian *surface*, and a Lambertian surface
   is precisely what a cloud is not.

4. **Self-shadowing is a march toward the sun**, along `sun.xz / sun.y × deck
   thickness`. The cheap version of this — one tap offset by the sun's horizontal
   direction alone — has a trap worth not inheriting: the offset, and with it all
   self-shadowing, goes to zero as the sun approaches the zenith. Dividing by
   `sun.y` makes the shadow *lengthen* as the sun drops, which is the behaviour
   that sells a low sun.

5. **Two decks**, at different heights and different wind speeds, so they slide
   past each other both when the camera translates and when nothing moves at all.
   One field can never have parallax.

Plus two terms from the volumetric-cloud literature, both ALU-only:

- **Beer-Powder** (Schneider & Vos, SIGGRAPH 2015). The powder factor darkens thin
  edges, which is what makes a cumulus read as solid rather than as fog. It is
  weighted *off* when looking toward the sun — there those edges are the brightest
  thing in the sky, and powder would cancel the silver lining exactly.
- **Multi-octave scattering** (Wrenninge et al. 2013; Hillaire 2016). Three
  evaluations with geometrically decaying extinction, energy and anisotropy. Costs
  three multiplies and no extra samples. Without it a thick cloud base goes black;
  with it, luminous grey.

And **aerial perspective**: Bevy composites its own over opaque geometry only, so
an alpha-blended shell never receives any. The deck now extincts toward the
camera's fog colour — the same colour the horizon-haze shell paints. Before, it
kept full contrast to the last visible pixel and then had its alpha faded to
nothing, which reads as the sky *ending*.

## Rejected, with reasons

- **Sampling Bevy's atmosphere transmittance LUT in the cloud shader.** This is
  possible, and worth knowing about: `bevy_pbr::mesh_view_bindings` exposes
  `atmosphere_transmittance_texture`, its sampler and the `Atmosphere` storage
  buffer at `@group(0) @binding(31..33)` under `#ifdef ATMOSPHERE`, which
  `render/mesh.rs` pushes for the mesh pipeline a `Material` uses;
  `bruneton_functions::transmittance_lut_r_mu_to_uv` turns `(r, mu)` into the UV.
  It was not taken because the analytic `sun_transmittance_at` is one CPU
  evaluation per frame instead of a per-pixel fetch, needs no `#ifdef` fallback
  for `CARTO_NO_ATMOSPHERE`, and — the deciding reason — keeps the clouds
  consistent with *our* sun light and *our* fog colour, which are also
  `utils::sun`. Matching Bevy's sky exactly while mismatching our own sun would
  have been the worse of the two consistencies. Revisit if the scene's lighting
  ever moves onto the LUTs wholesale.
- **The sky-view and aerial-view LUTs.** Not available: they live in the
  atmosphere pass's own `@group(0)`, not the view bind group. Only transmittance
  is reachable from a material.
- **Perlin-Worley noise** for the base shape, which is the textbook answer and does
  look better. An F1 cellular lookup is nine hashes per octave and this shader is
  fill-bound on the iGPU. The first two octaves are folded into *billow* noise
  (`1 − |2v − 1|`) instead — one sample, and it puts a crease at every zero
  crossing, which is most of what the cauliflower edge of a cumulus is.
- **Raymarching.** See the top of this note.

## Constraints that shape the code

- **Detail must fade with slant range.** A pixel's footprint on the cloud plane
  grows without bound toward the horizon; the high octaves alias into shimmer
  there. A muted octave contributes its *mean* (0.5) rather than dropping out, so
  the field's average — and therefore the coverage threshold, and therefore how
  much sky is filled — does not shift with range. This is what let the grazing cut
  narrow from `smoothstep(0.02, 0.20)` to `(0.006, 0.030)`; the old wide fade was
  hiding the aliasing, at the cost of the sky visibly ending.
- **`DistanceFog` is a uniform medium, which is wrong looking up.** At the default
  density its 7 km visibility would put ~30 % fog colour on a cloud straight
  overhead, because it charges the full vertical path at ground density. The haze
  *path* is bounded to `HAZE_LAYER_M / dir.y` instead of the density being scaled:
  real haze sits in the boundary layer, so a ray leaves it after a kilometre. This
  keeps the zenith clean while still burying the horizon, where the same slab is a
  30 km traverse.
- **The night ambient is deliberately brighter and warmer than the fog's night
  colour.** A cloud base over a city catches its glow from below. Matching the fog
  exactly would make the deck darker than the sky behind it and turn it into black
  holes in the star field — the one place cloud and fog should *not* agree.
- **Nothing here conserves energy.** The forward HG lobe runs to ~45× isotropic at
  `g = 0.8` and is clamped (`MAX_PHASE`); `CLOUD_SUN_GAIN` / `CLOUD_AMBIENT_GAIN`
  set the absolute level. These three are the tuning knobs, and they are art
  constants standing in for normalisation we do not do. The *shape* of the
  lighting does not depend on them.
- **The shell must stay one static mesh.** Per-frame geometry churn is what loses
  the Intel iGPU (see CLAUDE.md). Everything above is fragment work.

## Verifying a change here

`cargo shots` / `--shot` with `--time` is the A/B: the whole point of the model is
that it responds to sun elevation and azimuth, so sweep `--time 6 / 12 / 18` at a
fixed `--clouds`, and sweep the *heading* too — the phase function means looking
toward and away from the sun should not look the same.

Two limits. Colour, exposure and lighting are **not evidence on the CI software
rasteriser** (`headless-shots-software-renderer.md`), and this model changes
nothing else — so a green CI run says nothing about whether it looks right. And
the WGSL is compiled at runtime, so nothing in `cargo check` or `cargo test`
touches it. It can be validated offline, which is worth doing before handing a
shader change to a workstation:

```bash
cargo install naga-cli   # once
{ printf 'struct VertexOutput { @builtin(position) position: vec4<f32>,\n@location(0) world_position: vec4<f32> };\nstruct View { world_position: vec3<f32> };\n@group(0) @binding(0) var<uniform> view: View;\n'
  grep -v '^#import' assets/shaders/clouds.wgsl; } > /tmp/c.wgsl
naga /tmp/c.wgsl
```

That stubs the two `bevy_pbr` imports, so it validates our code — types, control
flow, swizzles — but not the import surface. It catches the errors that would
otherwise show up as a black sky on someone else's machine.

Note that `rustfmt` is **not installable via rustup in the container** —
`/usr/local/rustup` is root-owned and the component download fails. Fetch the
matching component tarball from `static.rust-lang.org` and run the binary with
`LD_LIBRARY_PATH` pointed at the toolchain's `lib/`.

## Sources

- Schneider & Vos, *The Real-Time Volumetric Cloudscapes of Horizon Zero Dawn*,
  SIGGRAPH 2015 — Beer-Powder, the weather-texture model
- Wrenninge et al., *Oz: The Great and Volumetric*, SIGGRAPH 2013; Hillaire,
  *Physically Based Sky, Atmosphere and Cloud Rendering*, SIGGRAPH 2016 —
  multi-octave scattering
- Henyey & Greenstein, *Diffuse radiation in the galaxy*, 1941 — the phase function
