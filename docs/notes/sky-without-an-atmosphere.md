---
triggers:
  - "the sky is black on the phone / on this device"
  - working on `systems::sky_dome`, `assets/shaders/sky_dome.wesl`, or `SkyMode`
  - touching the `FLOAT32_FILTERABLE` gate in `free_camera::setup_camera`
  - bumping Bevy, or reading `bevy_pbr::medium` / `ScatteringMedium`
  - considering vendoring or patching `bevy_pbr`
  - "why are there two skies"
  - choosing a sky from Settings ▸ Graphics (`AppPrefs::sky`)
  - files `sky_dome.rs`, `sky_dome.wesl`, `render_features.rs`, `free_camera.rs`, `sky.rs`
updated: 2026-10-17
---

# The sky on a GPU that cannot run bevy's atmosphere

The Fairphone 4 (Adreno 619) renders no sky at all, and has since 2026-08-12.
That was the *fix* for something worse — the app quitting three seconds into
launch — and it left a black dome over a lit city at noon. Since 2026-08-15 that
device draws `systems::sky_dome` instead. This note is the part that outlives
both diffs: why the atmosphere cannot run there, and what the thing standing in
for it can and cannot do.

## Why the atmosphere is refused, precisely

Bevy 0.19 moved the scattering coefficients out of the atmosphere's own code and
into a `ScatteringMedium` asset. `bevy_pbr::medium` bakes each medium into two
lookup textures — a density LUT and a scattering LUT — and both are
**`TextureFormat::Rgba32Float`**, read through a sampler with
`mag_filter: FilterMode::Linear`. wgpu allows a filtering sampler on a 32-bit
float texture only when the adapter reports **`FLOAT32_FILTERABLE`**.

Desktop GPUs offer it. Mesa's lavapipe offers it (so CI and `cargo shots` see the
real sky). A great many mobile GPUs do not, and the first phone this client ever
ran on is one of them.

Two details worth pinning, because both have already been got wrong once:

- **It is the medium's LUTs, not the atmosphere's four LUTs.** The commit that
  added the gate (`0c901b7`) named the transmittance / multiscattering / sky-view
  / render-sky LUTs as the `Rgba32Float` ones. They are all `Rgba16Float`
  (`bevy_pbr/src/atmosphere/resources.rs`), and 16-bit float filtering is core
  wgpu. The gate was right and its stated reason was not, which matters the
  moment someone tries to fix it upstream and goes looking in the wrong file.
- **There is no way to work around it from this side.** The format, the sampler
  and the bind-group layout (`filterable: true`) are all bevy's; the medium is
  not optional, and `ScatteringMediumSampler`'s inner `Sampler` is private, so it
  cannot be swapped for a non-filtering one. Fixing the *atmosphere* means
  changing bevy: 16-bit LUTs, or a `Nearest` sampler and a manual bilinear fetch
  where the feature is absent. That is an upstream change worth making and is not
  a thing to vendor `bevy_pbr` for — the crate is fifty thousand lines and would
  have to be re-forked on every Bevy bump.

`AtmospherePlugin` has its own two refusals in `finish()` — no compute shaders,
or `Rgba16Float` without `STORAGE_BINDING` — which merely log a warning and skip
the plugin. Those are *not* what happens here: our own gate fires first. If a
device ever hits one of those instead, the symptom is the same black sky and
`SkyMode` will still say `Atmosphere`, which is the one case the Settings ▸
Graphics readout would lie about — it reports the sky that was *chosen*, and a
plugin that skipped itself after the choice is invisible to it.

## What the fallback is

`systems::sky_dome`: one camera-centred opaque shell and ~20 instructions of
per-pixel arithmetic. Single scattering through a uniform slab, per channel,
using the Rayleigh and aerosol optical depths from `utils::sun` — the same
numbers the sun light, the fog colour and the cloud deck are built from. No
compute pass, no LUT, no float32 texture, no storage buffer: there is nothing in
it for an adapter to refuse.

Three decisions in it are load-bearing:

- **It scatters the light from 10 km up, not from the ground** (`SKY_SCATTER_ALT_M`,
  via `sun::sun_transmittance_at`). At dusk the ground is in shadow while the air
  above it is still in full sun — that is *why* the sky stays bright for half an
  hour after sunset. Using the ground's own beam gives a black sky over a lit
  city. Same argument as `docs/notes/cloud-lighting.md` makes for the deck.
- **It has an explicit twilight and night floor** because it has no multiple
  scattering. A single-scattering sky goes black the instant the sun crosses the
  horizon, and `sky_daylight` (nautical twilight, −12°) plus the night floor is
  what covers for the light the model cannot compute. This is the same gap the
  horizon-haze shell exists to paper over for the *real* atmosphere.
- **Altitude comes from the barometric column, not a fade constant.** The
  scattering coefficients are scaled by the air remaining above the camera, so
  the sky thins as you climb because there is less of it, and the only tuned
  numbers are where the last few thousandths are taken to zero
  (`SKY_DOME_FADE_ALT` → `SKY_DOME_MAX_ALT`, both below `GLOBE_SHOW_ALT`).

It is opaque and it writes no depth. Opaque because the star field, the moon, the
haze and the cloud shell are all camera-centred and therefore all tie in the
transparent phase's distance sort — the dome must not be entered into that
lottery, and the opaque phase composites before all of it. No depth write because
the shell is farther than anything in the flat tier, so the depth *test* alone
keeps it behind the world, and `SKY_DOME_RADIUS` is chosen against
`GROUND_DISC_RADIUS` for the same reason (a smaller shell puts sky in front of
the last 10 km of ground).

## What it deliberately is not

No multiple scattering, no aerial perspective on geometry (`DistanceFog` still
does that), and **no planet**: it is a flat-earth sky for the flat tier, hidden
before the globe appears. Carrying it into space would mean a curved-atmosphere
model, and at that point the honest move is to fix bevy's instead.

It is also *not* what `CARTO_NO_ATMOSPHERE` selects. That flag still means **no
sky at all**, because it is a bisecting switch and one that quietly substituted a
different sky would bisect nothing. `CARTO_SKY_DOME=1` is the other direction: it
forces the fallback on hardware that could run the real thing. Since the render
toggles left the developer panel, the flag is the *only* way to get
`SkyMode::None` (`SkyMode`'s own doc comment still names a dev-panel toggle).

**Since 2026-08-19 that direction is also a setting** — Settings ▸ Graphics ▸ Sky
(Automatic / Full atmosphere / Simple), stored as `AppPrefs::sky` and read once by
`setup_camera` beside the capability test. The env flag stays, and the two are not
redundant: a flag is per-launch and reaches a scripted capture, while the setting
persists and is the only one of the pair a phone can reach at all. Note the
asymmetry the code enforces — asking for `Simple` always works, asking for `Full`
still has to pass `FLOAT32_FILTERABLE`, because the alternative to refusing is the
bind-group validation failure that takes the whole app down. The tab reports which
sky the run actually got, and why, for the same reason this file exists: on the
platform where it matters there is no log to read.

**Since 2026-09-29 the choice is no longer once per launch.**
`render_features::apply_sky` resolves `SkyMode::resolve(prefs.sky)` every frame —
the one answer the camera, the planet, the dome and the Graphics tab read — and
adds or removes the camera's atmosphere components to match, ending in a re-sync
of the camera's render entity (`the-atmosphere-from-the-street-to-orbit.md` has
why). `setup_camera` still decides two things for good: the capability answers
(`quality::init_capabilities`) and `Hdr`, which the atmosphere requires — so a
run launched without `Hdr` keeps the dome until it restarts, and the tab says so.

## The absent component is a load-bearing fact about the rest of the scene

Choosing the dome does not only change the sky. `free_camera::setup_camera`
gates the camera's `AtmosphereSettings` on the same answer — because that
component carries `#[require(Hdr)]` and the doubled main-texture bandwidth is
most of what the fallback exists to avoid — so a `SkyMode::Dome` run is also a
run where **that component is not on the camera at all**.

On 2026-08-16 that deleted the planet on Android. `globe::update_globe_visibility`
is the sole writer of the globe surface's `Visibility` and of the `HideInSpace`
props, and it asked for `&mut AtmosphereSettings` in its camera query — it began
life as the atmosphere-mode switch and grew the visibility writes later. With
nothing matching, it returned before touching a single `Visibility`: the globe
streamed all 228 of its tiles, built every mesh, and drew none of them. What the
phone showed from orbit was the 150 km ground disc (a flat grey circle over the
anchor, normally hidden in space by that same system) with the planet cloud shell
floating above a world that was not there — read at the time as "no tiles, and
the clouds look weird". `setup_camera` had a comment asserting the system "no-ops
without it, which is what made dropping it a safe web workaround"; that was true
when it was written and stopped being true without anything saying so.

The general rule: **a component this gate withholds may be read by other systems,
but never required by them.** `Option<&mut AtmosphereSettings>` in the query,
with the atmosphere work in an `if let`. A required component is a silent
whole-system opt-out, and it opts out exactly the tier nobody is looking at.

The gate against a repeat is the phone-tier arm of the headless scene smoke
(`.forgejo/workflows/ci.yml`), which asserts the same `globe_rendered>0` the
desktop arm does under `CARTO_QUALITY=phone`. It is the same assertion twice on
purpose: what is being checked is not a new property but that the checked one
still holds on the unwatched tier.
## Losing the atmosphere loses the *lighting*, not just the sky

The section above is the same shape of fault one layer over: there, a system
*required* a component the gate withholds; here, nothing at all replaced what
that component was doing.

Found 2026-08-16, from the first Fairphone screenshots of a working sky. The dome
replaces what the atmosphere **drew**. Nothing replaced what it **lit with**.

`AtmosphereEnvironmentMapLight` bakes the sky into an IBL cubemap, and on the
tiers that carry it that is the scene's whole diffuse fill — `AmbientLight` beside
it is a trim, which is exactly what `AMBIENT_DAY_BRIGHTNESS = 900` says it is and
why it is so small. On a `SkyMode::Dome` run there is no atmosphere to bake from,
so the env map is absent and that 900 lux is not a trim on the fill, it *is* the
fill. Two tiers reach the same place by the other route: `quality::Budget::sky_ibl`
is false on the laptop and the phone whether or not the adapter could have run the
atmosphere.

What it looks like, both visible in the screenshots:

- a facade turned away from the sun goes to near-black, because the only thing
  that was ever going to light it was the sky;
- **water renders as a black hole in the map.** `WATER_ALBEDO` is `#0F2935` at
  roughness 0.06 — a mirror, correctly, since almost everything you see looking at
  water is reflected. With no cubemap there is nothing to reflect. `water.rs`'s own
  module header names the environment map as the thing its sea reflects; that
  sentence is simply false on half the tiers.

The fix is `AMBIENT_DAY_BRIGHTNESS_NO_IBL`, chosen by integrating this dome's own
model over the hemisphere (~16 klux at noon, the right order for a clear sky) and
discounting it to a quarter, because `AmbientLight` is unoccluded and omni-
directional where the IBL it stands in for has a dark ground half. It puts the
water back for a reason worth knowing: bevy's ambient is **not diffuse-only** —
`ambient.wgsl` runs `EnvBRDFApprox` for the specular lobe too, so raising it
restores the grazing-angle sheen a water surface is mostly made of, with no
cubemap and no per-cell material churn.

A real procedural cubemap generated from this dome would be better still —
it would have a sun in it, and a ground half — and is the obvious next step if
the flat fill reads wrong. It was not needed to stop the water being black.

## Verification status

The maths is unit-tested (`systems::map::sky_dome::tests`) and the WGSL was
validated with naga against bevy's binding layout. **Nothing here has been seen
on a screen by the agent that wrote it** — this container has no GPU, and the
brightness calibration in particular (radiance = lux/π, taken through
`view.exposure` the way `horizonhaze.wesl` does) is arithmetic that agrees with
the scene's physical exposure on paper. The knobs to reach for if it reads wrong
are `NIGHT_SKY_LUMINANCE` (night too dark or too milky), `SUN_DISC_GAIN`, and
`sky_daylight`'s bounds (dusk ending too early or lingering).
