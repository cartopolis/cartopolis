---
topic: surface-finish-and-occlusion
triggers: [ScreenSpaceAmbientOcclusion, ssao, "ambient occlusion", Fxaa, Msaa,
  main_pass_msaa, wants_edge_filter, VolumetricFog, FogVolume, VolumetricLight,
  light_shafts.rs, wet_surfaces.rs, "light shafts", "god rays", "wet road", "rain",
  metallic_roughness_texture, road_material_image, "roughness", "reflectance",
  GLASS_ROUGHNESS, building_windows.wgsl, "looks flat", "looks like a model",
  density_factor, "the world went dark", "too dark", "everything is dark",
  ContactShadows, "contact shadow", wall_base_tone, wall_shade, ground_texture,
  ground_finish_texture, WALL_FINISH_RANGE, "buildings look like they are floating",
  "the ground is flat", "paving has no texture"]
updated: 2026-09-11
---

# Why the city read as a model, and the four things that were missing

Measured on 2026-09-04, before any of this landed: the client used **no ambient
occlusion, no normal or roughness maps, no volumetric fog and no
anti-aliasing beyond a 2× main-pass sample count**, and `metallic` appeared in
exactly one file (`water.rs`). Every building was `Color::WHITE` with a vertex
colour at `perceptual_roughness: 0.95`; every ground surface was a base colour at
0.95; the roads were the only textured thing in the scene and their texture was
colour only.

That is a scene where **every surface in the world has the same finish** and
**nothing is darker where it meets anything else**. Those two facts are most of
what separates a render from a photograph, and neither is about geometry — the
geometry was already surveyed.

## These two are capability settings, not tier settings

They were tier fields first (`Budget::ambient_occlusion`, `Budget::light_shafts`)
and that was wrong. A tier is a guess about the *class* of machine, resolved
before a renderer exists — which is the same mistake `gpu_affords_atmosphere`
made and `free_camera::setup_camera`'s sky block spends thirty lines explaining.
Both of these have a real capability behind them that the device can be asked
about directly:

- **Ambient occlusion** is a compute pass, so it needs a backend with a compute
  stage (`Capabilities::compute_shaders`, read off
  `limits().max_compute_workgroup_size_x`, which is 0 on WebGL2).
- **Light shafts** need an adapter that rasterises in hardware and a shadow map
  to cut the shafts out of.

`AppPrefs::ambient_occlusion` / `light_shafts` are `Option<bool>` on the same
pattern as `shadows` and `sky_lighting`, except that `None` resolves against the
*adapter* rather than against `quality::budget()`. An explicit `Some(true)` still
has to pass the capability test — asking for a pass the backend cannot run does
not make it appear, exactly as `SkyPreference::Full` still has to pass
`FLOAT32_FILTERABLE`.

Both apply **immediately**, from Settings ▸ Graphics.
`systems::render_features::apply_render_features` owns adding and removing every
component involved, and the camera spawn deliberately inserts none of them — two
places that can disagree about one component is how a setting comes to read "on"
over a scene that does not have it. That live toggle is safe here and is not safe
for the sky: an `Atmosphere` cannot be removed without tearing down GPU resources
still in flight, while an occlusion pass and a fog volume are pipeline state that
bevy respecialises on the next frame.

One trap in the removal: `ScreenSpaceAmbientOcclusion` carries
`#[require(DepthPrepass, NormalPrepass)]`, and **bevy does not remove a required
component when the requiring one goes** — so switching occlusion off has to drop
both prepasses by hand, or the run keeps paying for two passes nothing reads.

## Ambient occlusion costs the multisampling, and that is not optional

`ScreenSpaceAmbientOcclusion` reads a depth and a normal prepass, neither of
which is multisampled, so bevy requires `Msaa::Off` and **warns and carries on**
if it does not get it. A rung that asked for both would silently get the samples
and no occlusion.

So the three answers to "what sample count does the main pass get" are resolved
in one place, `quality::Budget::main_pass_msaa`: the tier's own choice, the
web-and-software-adapter restriction to 1 or 4, and ambient occlusion. And
because switching AO on takes the desktop rung's 2× away, the same run gains
`Fxaa` — `Budget::wants_edge_filter` is true exactly when something took the
samples away, so a rung that never had any (the phone) does not quietly gain a
full-screen pass.

**FXAA and not TAA**, and this is a property of this codebase rather than a
preference: `big_space` re-anchors the whole coordinate frame as the camera
moves, and a history buffer indexed by last frame's motion vectors is precisely
what that breaks. The `bevy_anti_alias` feature is enabled for FXAA alone; SMAA
would additionally need `smaa_luts` back.

## The prepass broke a pipeline that had been fine for a year

Ambient occlusion requires `DepthPrepass` + `NormalPrepass`, which this client
had never had. **Bevy runs a material's `specialize` hook for the prepass as well
as for the main pass** — `PrepassPipelineSpecializer` calls `user_specialize`,
and bevy's own source comments that this is "a bit risky because it's possible to
change something that would break the prepass but be fine in the main pass".

`RoadDepthExtension::specialize` replaces `descriptor.vertex.buffers` with the
layout its own vertex shader wants (position, normal, uv, and the depth rung at
location 8). Applied to the prepass — which compiles *bevy's* vertex shader,
since `prepass_vertex_shader` is deliberately left at the default — that leaves
bevy's shader with an input nothing supplies. The result is `RenderPipeline with
'pbr_prepass_pipeline' label is invalid` at `Queue::submit`, which
`lib.rs::handle_render_error` halts the renderer on: the app draws nothing for
the rest of the process.

So **any `specialize` that rewrites the vertex layout must return early for the
prepass**, matched on `descriptor.label` containing "prepass". The mesh key
carries the prepass bits in both calls (the main pass is told which prepasses
exist), so the key cannot tell them apart.

The other custom vertex shader, `grass.wgsl`, is safe by accident and it is worth
knowing why: a blade's five lattice vertices are all at the same point, so
without the vertex stage that expands them every triangle is degenerate and the
prepass rasterises nothing at all.

## Light shafts need three components, and two of them render nothing alone

The camera's `VolumetricFog` turns the pass on. A directional light's
`VolumetricLight` says which light casts shafts. A `FogVolume` is the only place
either of them has any effect. Miss the third and the pass runs and draws
nothing, at full cost.

The volume follows the camera (`systems::light_shafts`) rather than covering the
world, because the march spreads `step_count` samples over the *volume*: a box
big enough to hold the streamed map would sample every few hundred metres and the
fog would come out in slabs. 600 m at 48 steps is ~12 m per sample. **If the
volume grows, the step count has to grow with it** — there is a test on that
ratio.

It is placed from the camera's `GlobalTransform`, the same trick
`sky::update_star_sky` uses for the star shell: the camera is the floating
origin, so its global translation is ~0 whatever grid cell it is in.

Only the sun gets `VolumetricLight`. The moon's illuminance is four orders of
magnitude down and a second volumetric light doubles the march's shadow work for
something nobody would see.

## A fog volume subtracts as well as adds, and that is how the shafts went dark

Reported 2026-09-10: turning light shafts on made the whole world dark. It was
not the shafts, it was the air they are marched through.

Bevy's march is Beer's law twice. It adds the sunlight scattered *into* the ray —
the shafts, the thing the feature is named after — and along the same ray it
attenuates the background by `exp(-density × (absorption + scattering) ×
distance)`. The second half is the one that gets forgotten, and it is scaled by
the volume's own metres, so the same `density_factor` means something completely
different in a 5 m room and in a 600 m box.

The numbers, because the size of the mistake is the point:

| | background surviving |
|---|---|
| shipped value (0.024 after the haze multiplier), at 100 m | **19 %** |
| shipped value, at 300 m — the volume's half-edge | **0.7 %** |
| now, default weather, at 300 m | 95 % |
| now, default weather, across a 20 m street | 99.6 % |
| now, heaviest fog *and* heaviest rain, at 300 m | 65 % |

Two things caused it and either alone would have been survivable. Bevy's default
`density_factor` of 0.1 was taken as a starting point when it is meant for a
volume a few metres across. And `DevPanelState::fog_density` was read as if it ran
`0..0.02` — that is the scale of a `FogFalloff` *coefficient*, not of the control,
which is the Weather slider in Settings ▸ World and runs `0..1` with a default of
0.15. So the haze multiplier sat clamped at its ceiling in every weather,
including "clear", and no slider position ever looked different from any other.

The fix is not a smaller number, it is a different unit. `light_shafts` states how
much of the background the air may swallow over the volume's half-edge
(`CLEAR_EXTINCTION`, `RAIN_EXTINCTION` — fractions, reviewable on sight) and
inverts Beer's law to get the `density_factor` bevy wants (~3 × 10⁻⁴, reviewable
by nobody). Four tests hold it: the extinction constants mean what they say, the
volume passes at least 60 % of the background in the worst weather, rain is
thicker than clear air, and the haze knob does not saturate in the first tenth of
its travel.

**This is not checkable in the container.** Light shafts are refused on a software
adapter (see below), so the fog volume is never even spawned on lavapipe. The
arithmetic above is the evidence, plus the tests; a picture needs the `deck-gpu`
runner.

There is **no Android special case**, deliberately. A handset will resolve
Automatic to on and a shadowed per-pixel march at panel resolution will cost it —
Settings ▸ Graphics is where that gets turned off, by the person holding the
device, which is the whole reason these two are reachable from the phone at all.
If a measurement on a real handset says Automatic should answer differently
there, that belongs in `AppPrefs::light_shafts_on` as a *measured* refusal beside
the software-adapter one, not as a tier field.

## Roughness is now three multiplied things, and they must stay multiplicative

For a road, the finish comes from:

1. `RoadClass::perceptual_roughness` — the class's level (0.78 tarmac, 0.95 grit).
2. `RoadTexture::surface` — the generated metallic-roughness map, which *varies*
   that across the carriageway: polished wheel paths, smoother paint, a metallic
   rail head. Bevy multiplies the texture's green channel into the scalar.
3. `systems::wet_surfaces` — rain, which lowers the scalar in (1).

Because (2) multiplies (1) and (3) writes (1), rain automatically wets the wheel
paths more than the crown, and none of the three had to know about the others.
Break that by writing an absolute roughness anywhere in the chain and the other
two stop composing.

`MIN_ROUGHNESS` in `road_texture.rs` is the floor that exists because of this
stacking: a texel allowed to reach 0 would be a perfect mirror on a wet night,
i.e. a bug visible in one weather at one time of day.

The road map is `Rgba8Unorm`, **not** `Rgba8UnormSrgb`. These are coefficients,
not colour; the sRGB decode would bend a stored 0.62 to 0.34.

## Wet surfaces must ramp from a remembered dry value

`wet_surfaces` recomputes from a stored `DryLook` rather than scaling the live
value, because scaling compounds — a shower that lasted a minute would leave the
roads black for the rest of the session — and because `systems::vegetation`
already writes `base_color` on some of the same handles.

The cache is keyed by `AssetId`, not by `SurfaceGroup`: a group's handle is
recreated on every re-anchor, and a material rebuilt mid-shower is created *dry*
by its own recipe, so a group-keyed cache would ramp it from the previous
handle's already-wetted values. Dead ids are dropped each pass.

Only hard surfaces get wet (`hard_surface`). Wet foliage darkens without becoming
glossy, and the vegetation groups' colour belongs to `systems::vegetation`'s
altitude ramp — two systems writing one field is the bug.

**There are no puddles and should not be until the data supports them.** A puddle
is a mask of where water collects, and nothing streamed says where the ground is
low. Noise puts standing water on a camber and up a kerb.

## Glass is the facade half

`building_windows.wgsl` now writes `perceptual_roughness` and `reflectance` per
fragment, blended by `pane` — the *distance-blurred* coverage, not the raw
`inside` test. That matters for the same reason the emissive uses it: at range the
windows have to converge on the wall's finish, or the far half of the city is a
grid of sparkling glints.

## What is still flat

Recorded so the next person does not have to re-measure it:

- **Paving, `Built` and every other ground group carry no texture at all** — one
  vertex colour at 0.95. They are the largest remaining flat area in a street
  scene. A texture there needs UVs on those meshes, which is a real change.
- **There are no normal maps anywhere.** Bevy needs `ATTRIBUTE_TANGENT` for them,
  and none of the streamed meshes carry tangents. Everything above varies
  *roughness*, which needs no tangent — that is why it was done first, not
  because it is the better effect.
- No screen-space reflections, no contact shadows (`bevy_pbr` has both), no
  parallax, no decals.

## Volumetric fog is refused on a software adapter, and ambient occlusion is not

Measured 2026-09-04 in this container (Mesa lavapipe, `--shots --only city`):

- Phone rung, neither effect: the capture finished in about 100 seconds.
- Balanced rung, ambient occlusion on: **2,760 ms per frame**, capture finished
  in about five and a half minutes, `settled=true`, PNG written.
- Desktop rung, ambient occlusion *and* light shafts: did not finish in ten
  minutes.

So `AppPrefs::light_shafts_on` refuses when the adapter is a CPU rasteriser
(`Capabilities::software_renderer`, the same answer that already clamps the
sample count), and refuses an explicit `Some(true)` with it. `render_features`
then spawns no fog volume either — a volume with no pass to consume it is not
free, since bevy extracts and uniform-buffers every `FogVolume` in the world.

Ambient occlusion is deliberately **kept** there, at three seconds a frame. It is
what compiles the prepass, and the prepass is the thing most likely to break —
had it been skipped in the container, the pipeline failure above would have
reached a real GPU instead.

## None of this can be judged in the container

Colour, exposure and lighting are not evidence on lavapipe
(`docs/notes/headless-shots-software-renderer.md`), and every change here is a
lighting change. A `--shot` run proves the app still starts, still streams and
still draws the same geometry; it cannot show you whether the city looks better.
That is a workstation job.

## What stopped being flat on 2026-09-11

Four of the things the section above listed as still flat, done in the order that
costs least.

### The 3DBAG buildings had no ground contact, and the OSM ones always had

`osm_buildings::wall_base_tone` has existed for as long as that lane has: the
bottom row of wall vertices carries a darker tone that eases back to the wall
colour at the eaves, a baked contact shadow. `lod22` — the lane that draws the
**actual** city over the Netherlands, and the one whose buildings you are
standing among — never called it. Every 3DBAG building was one flat tone from
pavement to eaves.

The curve is now `wall_shade(wall_h, y)` and both lanes evaluate it, which is the
point: two lanes drawing the same city on two recipes is how they come to
disagree in a way nobody notices for a year. The OSM lane gets it from
interpolation across a base→eaves quad; the 3DBAG lane has to evaluate it per
vertex, because a LoD2.2 shell is a triangle soup with no rows.

It is **baked on the worker that builds the cell**, so it costs nothing in a
frame, and it is there on every rung and every adapter — a handset, WebGL2,
anyone who has switched the occlusion pass off. It is not a substitute for real
occlusion and does not try to be: it knows about the ground and nothing else, so
it says nothing about the alley between two blocks. It is the cue that stops an
extrusion looking like it is hovering, which is the thing you see first.

`SPAN_M`'s strange-looking role is worth keeping straight: the gradient runs the
whole height of the wall by construction, so the *strength* is scaled down on a
tall building rather than the span being clamped. Without that, a tower spreads a
ground-contact effect over thirty metres of facade.

### The wall between the windows was one roughness, everywhere

`building_windows.wgsl` had already given the panes their own finish, and the
argument for it applies just as well to what surrounds them: one
`perceptual_roughness: 0.95` on every square metre of every building in the
country means a whole street catches a low sun identically. `WALL_FINISH_RANGE`
varies it ±0.05 per lattice cell — about a window bay, which is the scale real
facades change finish at.

It costs **no new hash**. It reuses `r`, the cell's own draw, decorrelated from
the lit-window decision by one multiply and a `fract`. This is the largest
geometry lane in the app, and the question the whole module answers is what a
facade can have for free. It fades out on `blur` like everything else there —
a per-cell value that survives below a pixel is a sparkle crawling across the far
half of the city.

### The ground between the roads has a finish now

`cartopolis_geo::ground_texture` generates one small tiling metallic-roughness
map, and `Paving`, `Built` and `Sand` sample it. **Roughness only** — the colour
of the ground is the map's palette, argued over and reverted once already for
being too saturated, and roughness varies how a surface catches the sun without
touching a tone. It is the same reason this note gives for doing roughness before
normal maps.

Three things make it cheap. One image for every group that takes it, since the
map says nothing about colour and the groups differ in nothing else. No new UVs:
UV0 on an area surface is already **metres from the map anchor**, so a repeat
every `GROUND_FINISH_M` is a `uv_transform` scale. And it *multiplies* the
group's scalar rather than replacing it, so it slots into the three-term
roughness stack this note already requires — `wet_surfaces` writes the scalar,
this varies it, and neither knows about the other.

The groups left out each have a reason, listed at `wants_ground_finish`. The
short version: roads have their own and a better one, water is a shader, the
vegetation groups have colour systems writing the same material, and the object
groups' UV0 is not the anchor-metre projection this is scaled against.

### Contact shadows, riding the occlusion setting

`bevy_pbr::ContactShadows` (screen-space, `linear_steps: 8` against bevy's 16,
over a 30 cm ray) is on whenever ambient occlusion is on **and** shadows are on.
Two gates, each honest for its half.

It has no preference of its own on purpose. It is `#[require(DepthPrepass)]`, and
the depth prepass exists in this app only because the occlusion pass asked for
it — so a third checkbox would be one that silently turns a second full-screen
pass on, which is not something a Graphics tab should be able to hide. They are
also the same cue ("nothing is darker where it meets anything else"), and this is
the half a screen-space occlusion pass is worst at: at the contact its own radius
has the least to work with. The shadow gate is the plainer one — these are
shadows, and somebody who turned shadows off to get their frames back has not
asked for a ray march instead.

The removal order matters and is the same trap the note records for
`ScreenSpaceAmbientOcclusion`'s required components: contact shadows come off
*before* the prepass they read, in the same flush.

**And they are refused on a software adapter**, beside volumetric fog and for the
same reason: a screen-space march is per-fragment work, and a CPU rasteriser is
where per-fragment work is least affordable.

Note the difference from ambient occlusion, which is deliberately *kept* here:
that one compiles the depth and normal prepasses, which are the things most
likely to break a pipeline, and paying three seconds a frame for it is how such
a failure gets caught in this container instead of on a phone. Contact shadows
compile nothing the occlusion pass has not already compiled, so keeping them
buys no coverage.

**This was first written up with a measurement it does not have, and the
correction is the useful part.** The street flicker smoke failed on the landing
gate at 901 s with `platform::watchdog` dumping every thread asleep on a futex,
and that was read as contact shadows stalling the capture. It was not: 901 s is
`verify-branch`'s own `timeout 900` plus a second, the step simply ran out of
budget, and the same step failed identically with contact shadows refused. The
watchdog output is what a *slow* process looks like, not what a stalled one
looks like, and the two are indistinguishable from that dump alone. The refusal
above stands on the light-shafts precedent, not on this. (That gate is retired —
[retired/flicker-gate.md](retired/flicker-gate.md) — and this is one of the two
false findings it produced.)

Unlike the other two gates this one is not overridable, because it is not a
judgement about the picture: asking a rasteriser with no fragment budget for more
fragment work does not produce a slower picture, it produces no picture.

### Still flat

- **No normal maps.** Unchanged, and for the unchanged reason: bevy needs
  `ATTRIBUTE_TANGENT` and none of the streamed meshes carry tangents.
- **No screen-space reflections, no parallax, no decals.**
- The ground finish is a *level*, not a pattern. There are still no paving slabs,
  no kerb detail and no road-to-pavement joint beyond the depth rung.
