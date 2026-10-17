---
triggers:
  - working on assets/shaders/clouds.wesl, cloud_field.wesl or `update_clouds` / `CloudMaterial` / `CloudField`
  - "the clouds look flat / painted / wrong at sunset"
  - considering raymarched or volumetric clouds
  - changing the sun colour, the fog colour, or `utils::sun`
  - adding a cloud layer, or wondering why there are two
  - how fast or which way the clouds move
  - cloud shadows on the ground (see cloud-shadows.md)
updated: 2026-10-17
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

**But a plane has no sides, and that was the limit (2026-10-17).** With the
lighting done, the deck still looked like brush strokes: toward the horizon every
cloud foreshortened to a sliver, the far deck was a stack of stripes, and nothing
had a top to light. The shell now draws the low deck **in relief** — the same 2D
field read as a height map, a flat base with a dome over it, which the view ray
climbs in eight steps and two secant refinements (see "The low deck in relief"
below). That is a march, but through a height field, not a density field: one
coarse 3-octave fbm a step, stopping at the first cloud, lit once. On the M5 Pro
node at 1920×1080 the phone tier went 5.8 → 6.0 ms with clouds on; the desktop
tier was inside the run-to-run noise. The conclusion above stands for the
*lighting*; it was never an argument for flat geometry.

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
   `horizonhaze.wesl` paints it un-exposed to match), so there was no unit
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

   **With bevy's atmosphere running, the colour is the atmosphere's own**
   (2026-09-29, `cloud_field::deck_sunlight`): its transmittance table, read at
   the deck's altitude toward the sun, times `utils::sun`'s disc occlusion and the
   gain (`params3.x`). Before, a Full-sky sunset put a `utils::sun`-coloured deck
   in front of bevy's differently-coloured sky. `utils::sun` is still the answer
   wherever there is no atmosphere (the dome sky, `CARTO_NO_ATMOSPHERE`), and the
   radii are read through the binding a field at a time — the Adreno 840 misreads
   a by-value copy of that struct (`adreno-atmosphere-struct-copy.md`).

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

## The deck moves with the weather's wind (2026-09-29)

It used to drift at a fixed 126 m/s from one direction, whatever the weather said.
It now takes MET's 10 m wind — speed *and* direction — scaled and veered to the
deck's height (`sky::deck_wind`: twice the speed, 20° clockwise) and sped up four
times (`CLOUD_TIME_LAPSE`), because at its true speed a deck 2.5 km up barely
visibly moves. The drift is **integrated on the CPU** (`CloudField`) and handed
to the shader in metres: velocity × clock in the shader would jump the whole
field every time the wind changed. The velocity eases over 8 s, so a new sample
turns the clouds rather than snapping them. The grass and the waves read the same
surface wind (`weather::surface_wind`), so all three move one way; without a
sample it is a 3 m/s south-westerly.

## The water mirrors this deck

`water.wesl` and the flat map's water past the ring (`ground_detail.wesl`) find the
deck along their reflected ray, through the same `cloud_field.wesl` the shell
draws from, reading its uniforms out of one shared buffer. That is why the field,
its octave counts, its sun march and its lighting live in the shared module
rather than in `clouds.wesl`: two copies would drift, and a lake would mirror
clouds that are not overhead. `water-surface.md` has the rest.

Plus two terms from the volumetric-cloud literature, both ALU-only:

- **Beer-Powder** (Schneider & Vos, SIGGRAPH 2015). The powder factor darkens thin
  edges, which is what makes a cumulus read as solid rather than as fog. It is
  weighted *off* when looking toward the sun — there those edges are the brightest
  thing in the sky, and powder would cancel the silver lining exactly.
- **Multi-octave scattering** (Wrenninge et al. 2013; Hillaire 2016). Three
  evaluations with geometrically decaying extinction, energy and anisotropy. Costs
  three multiplies and no extra samples. Without it a thick cloud base goes black;
  with it, luminous grey — but only just: every octave still decays as
  exp(−depth), so a core got almost nothing from it, which is the next term.

## Diffusion through the column (2026-10-17)

The octaves above are the first few bounces. Light that has scattered dozens of
times diffuses through a cloud, and diffusion falls off as 1/(1 + (1−g)τ), not
exponentially. Without it the deck's cores lived on the ambient floor alone: grey
bases with a hard step behind a lit rim, and at sunset a muddy brown. Frozen
Fractal (*Around the World 34: A pretty sky*, 2026) hit the same thing and
measured it with a Monte Carlo tracer: real multiple scattering makes cloud
bottoms brighter and the lighting smoother, and the old look was "metallic".

`cloud_field::low_deck_light` (shared by the shell and the mirror) adds the
*diffuse* part of the Eddington two-stream transmittance of a conservative slab —
everything but what still travels near the incoming direction, which the phase
octaves already draw — for two sources:

- **the sun**, at its own incidence `sun.y`;
- **the clear sky above the deck** — Rayleigh blue (`SKY_ABOVE_TINT`), as bright
  as the camera's horizon (`params3.z` = fog luminance × `CLOUD_SKY_ABOVE_GAIN`),
  at a hemisphere's mean incidence. This is what tints a base blue by day, and
  under a low sun mixes the red beam and the blue sky into mauve. Frozen Fractal
  got it by bending the bases' normals up into Godot's sky light; a plane has no
  normals, so here it goes through the slab. It is taken from the fog rather than
  `daylight` because exposure follows the sky: by the clock, the skylight faded
  at dusk while the sky on screen stayed bright, and the bases went brown.

The column's depth for diffusion is `DIFFUSE_DEPTH_LOW` = 30 per unit of
coverage — a real cumulus is 30–100 — and not `OPTICAL_DEPTH_LOW` (7), which is
tuned as the view ray's opacity and the march's shadow. With that light in, the
sky floors came down (0.35 → 0.2 scattered, 0.85 → 0.55 overcast), because part
of what they stood in for is now modelled. The sun's diffused part is weighted
by `sun.y`, as a beam spread over a slanted top is: without it a closed deck
under a 16 h sun glowed orange. At noon an opaque cumulus core lands at 0.63×
the horizon haze and a closed overcast at 0.98×
(`print_noon_core_bases`, an ignored test, prints both; the two tests beside it
mirror the shader's constants and must move with them).

On its own, on the old flat deck, this was a small change — the person steering
said they could barely see it. It earns its place in the relief, where `above`
(the cloud over the point seen) differs from `column` (the cloud's thickness):
a crown has little above it and is lit, a base has all of it and is grey.

**After sunset the base is the lit face.** The deck's horizon dips √(2h/R) —
1.6° at 2.5 km — below the street's, so it keeps the sun for minutes after the
city has lost it (`utils::sun::sun_disc_at`), and then the sun is *under* it:
what a viewer below sees is what the column reflects (1 − T of the same slab).
That is the red glow on a deck's underside just after sunset.

## The deck's sunlight follows eye adaptation (2026-10-17)

The deck's sun term is written in the screen's units at the day exposure, and
the camera opens up toward dusk and night by up to 4.5 stops
(`sky::eye_adaptation_stops`: the night and twilight lifts). The sky and the fog
colour followed; the deck's lit side did not, so a sunset cloud's lit side was
that many stops too dark — the muddy brown decks of the evening. `update_clouds`
now multiplies the sun term by 2^stops. The overcast lift is left out on
purpose: it pays for the beam the overcast model takes off the *ground*, which
the deck, being that overcast, never lost.

## The low deck in relief (2026-10-17)

`cloud_field.wesl`'s relief section and the shell's march. In short:

- **A height map, not a sheet.** A column stands from the deck's flat base to
  `relief_column` of its thickness; the surface is where the field reaches
  `relief_floor(h) = thresh + span·h²` — quadratic, so a cloud's sides climb
  steeply off the base and round over at the top. Linear stood clouds up as
  cones. The view ray climbs from base to top in `RELIEF_STEPS` (8; 4 on the
  phone) on a 3-octave field (2 on the phone), stops at the first step inside,
  and places the crossing by two secant steps. With one linear interpolation
  the step that first lands inside changes from pixel to pixel across a side,
  and the side was drawn as contour lines.
- **Opacity and thickness are read a step inside** (`READ_DEPTH`), with the full
  octaves: at the crossing the coarse field is exactly at the threshold, half in
  and half out, which made every side half transparent.
- **The edge is narrow** (`RELIEF_SOFT` 0.05 of the field, against the sheet's
  0.30). The sheet's wide ramp is most of why its clouds read as smears.
- **Opacity is the cloud crossed, not the edge alone.** Beer's law on the
  column's thickness over the ray's slant (`VIEW_DEPTH_LOW`): a fringe or a
  small puff lets the sky through, a core hides the sun. Five times deeper
  toward a closed deck, whose thinnest places must still hide the disc. The
  first relief had every cloud solid to its last pixel, and that read wrong.
- **The field is cauliflower, not billows** (`relief_field`). The old billow fold
  (`1 − |2v − 1|`) is round valleys under knife-edge ridges; stood up as a height
  map the ridges were paper folds, and rounding them off left smooth blobs the
  person steering called "too liquid". Its twin `|2v − 1|` is round humps with
  cusped creases between — a heap of puffs — so the masses (first two octaves)
  are plain noise, the middle octaves puffs, and the last plain grain. The march
  reads four octaves so the puffs are in the silhouette, and the domain warp
  went 0.28 → 0.12: its curls were the other half of the liquid look.
- **Cover is a computed quantile, not `1 − cover`.** The field is narrow and off
  centre, so `1 − cover` put four fifths of the sky under a "45 %" deck once the
  edges were crisp. `low_threshold` holds the field's quantiles for both tiers,
  computed by a TypeScript mirror of `relief_field` over 600,000 points,
  counting what the relief draws. It moves if the field does. (Counting cloud
  pixels in zenith shots was tried first and is worthless: a 5 km patch holds a
  handful of clouds, and the dark zenith sky failed a colour test for "sky".)
- **Overcast gets cells.** At full cover every point is cloud, so thickness is
  measured from `RELIEF_COLUMN_FLOOR` (the field's ~90 % quantile) rather than
  from the threshold, linearly from `RELIEF_OVERCAST_MIN` up, which leaves
  thinner, brighter places between thick, dark cells. A thin place of no
  thickness at all drew as a hard-edged dark blotch (the powder term darkens
  what it takes for a thin edge).
  The cloud opacity is now 1.0, not 0.92: at 0.92 the sun disc shone through a
  closed deck as a white dot.
- **The mirror keeps the sheet**, but takes the same field, threshold and edge,
  so a lake reflects as much cloud as the sky has.

Before/after sets for all of this were rendered on the M5 Pro node (Groningen,
40 m up, headings S, N and WNW, 12–17.4 h, covers 0.25–1.0).

And **aerial perspective**: Bevy composites its own over opaque geometry only, so
an alpha-blended shell never receives any. The deck now extincts toward the
camera's fog colour — the same colour the horizon-haze shell paints. Before, it
kept full contrast to the last visible pixel and then had its alpha faded to
nothing, which reads as the sky *ending*.

## Rejected, with reasons

- **Sampling Bevy's atmosphere transmittance LUT in the cloud shader** — rejected
  on 2026-08-08, **taken on 2026-09-29** (mechanism 2 above). It was declined for
  keeping the clouds consistent with *our* sun light and fog colour, which are
  `utils::sun`: matching bevy's sky while mismatching our own sun looked like the
  worse of the two consistencies. What changed is that the Adreno 840 can now run
  bevy's atmosphere (`adreno-atmosphere-struct-copy.md`), so on every tier that
  shows Full sky the deck sits *in* bevy's sky and the mismatch was the one on
  screen. The ambient and the haze are still the fog's; only the beam's colour
  moved. `bevy_pbr::mesh_view_bindings` exposes the table, its sampler and the
  `Atmosphere` buffer at `@group(0) @binding(31..33)` under `#ifdef ATMOSPHERE`,
  which `render/mesh.rs` pushes for the mesh pipeline a `Material` uses.
- **The sky-view and aerial-view LUTs.** Not available: they live in the
  atmosphere pass's own `@group(0)`, not the view bind group. Only transmittance
  is reachable from a material.
- **Perlin-Worley noise** for the base shape, which is the textbook answer and does
  look better. An F1 cellular lookup is nine hashes per octave and this shader is
  fill-bound on the iGPU. The first two octaves are folded into *billow* noise
  (`1 − |2v − 1|`) instead — one sample, and it puts a crease at every zero
  crossing, which is most of what the cauliflower edge of a cumulus is.
- **Raymarching a 3D density field.** See the top of this note; the relief march
  through a 2D height field is the step that was taken instead.

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
  the Intel iGPU (see `upload-bursts.md`). Everything above is fragment work.

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
  grep -v '^#import' assets/shaders/clouds.wesl; } > /tmp/c.wgsl
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
