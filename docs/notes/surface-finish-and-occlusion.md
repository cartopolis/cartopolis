---
topic: surface-finish-and-occlusion
triggers: [ScreenSpaceAmbientOcclusion, ssao, "ambient occlusion", Fxaa, Msaa,
  main_pass_msaa, wants_edge_filter, VolumetricFog, FogVolume, VolumetricLight,
  light_shafts.rs, wet_surfaces.rs, "light shafts", "god rays", "wet road", "rain",
  metallic_roughness_texture, road_material_image, "roughness", "reflectance",
  GLASS_ROUGHNESS, building_windows.wgsl, "looks flat", "looks like a model"]
updated: 2026-09-04
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
something nobody would see. And `handset_guarded` forces `light_shafts: false` on
Android whatever rung was chosen — a per-pixel shadow-sampled march on a tiled
mobile GPU is a property of the machine, not a judgement about the picture, which
is that function's test for what belongs in it.

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

So `setup_camera` refuses `VolumetricFog` when the adapter is a CPU rasteriser
(`Capabilities::software_renderer`, the same answer that already clamps the
sample count), and `light_shafts::setup_light_shafts` refuses the fog volume for
the same reason — a volume with no pass to consume it is not free, since bevy
extracts and uniform-buffers every `FogVolume` in the world.

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
