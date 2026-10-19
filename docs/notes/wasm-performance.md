---
topic: wasm-performance
triggers: [platform/workers.rs, crates/wgpu, crates/bevy_shader, shader_store.rs, wgsl-v1.bin, keep-profile, map_wgt_features, pipeline_log, height.rs, debug_switches.rs, pipeline_health.rs, quality.rs, index.html, lod22.rs, osm_buildings.rs, ground_detail.rs, wet_surfaces.rs, refresh_tile_lod_blend, "get_mut", "Assets<GroundMaterial>", IoTaskPool, cdp_probe, callgrind, "wasm", "web build", "phone", "mobile", "render scale", "devicePixelRatio", "hitch", "hang"]
updated: 2026-10-19
---

# What actually costs frames on the web build

The standing rule — **on wasm `IoTaskPool` runs on the main thread**, because
Bevy disables `multi_threaded` there, so parse and mesh work must go to
`platform::workers` — is documented where it bites, at `lod22.rs:349`. This note
holds only what the code cannot.

## The measurement

2026-07-26, `tools/cdp_probe.ts --gpu --profile` against the real adapter
(`vendor=intel architecture=xe-lpg`, Meteor Lake Arc, Mesa 26.1.5) — **not**
SwiftShader, which headless Chromium falls back to silently and which makes any
frame number meaningless.

Before: **median 1–2 fps, 500–2000 ms frames.** Cause was `serde_json` on the
main thread — ~32% of total samples, ~46% of non-idle, led by
`drop_glue::<serde_json::Value>` (14.2%) and `Value as Deserialize::deserialize`
(10.6% + 4.9% visitor).

After moving parse+mesh to `platform::workers` (`1ba27b0`, `11f8d7e`, `9a9b67d`):
**median 52 fps, 16–20 ms frames**, and `serde_json` is absent from the
main-thread profile.

## Ruled out — do not re-test these first

A/B'd on the real GPU, all within noise of baseline: **atmosphere, env-map IBL,
haze, clouds, rain**. GPU load is implausible as a cause at ~520 draws / 1.5 M
triangles on this device. Shadows were never tested and are ~79% of submitted
triangles — `?no_shadows` exists to check that one.

## Still true, still costing

Parsing is **half fixed** (restated 2026-08-10). `lod22.rs` is typed now — its
`parse_cell` uses a derived `Deserialize` — so the 3DBAG path no longer boxes
every coordinate of a ~4 MB cell. What is still an untyped `serde_json::Value`
walk is the OSM building path: `osm_buildings.rs`'s `fetch_coverage` and
`parse_buildings`, the latter over a multi-megabyte Overpass response. (Named,
not line-numbered: the line numbers written here on 2026-08-06 had drifted by
fourteen within four days — the `check:` grep below is the durable pointer.)
Dropping that tree still costs more than building it, and that cost moved off the
main thread rather than going away. Typing `parse_buildings` is the remaining win.

check: `grep -qE 'serde_json::from_str::<serde_json::Value>|: serde_json::Value = serde_json::from_str' crates/cartopolis/src/systems/map/osm_buildings.rs`

The previous check pointed at `lod22.rs` and had become a **false green**: after
that file was typed, its only remaining match was the comment explaining that it
is *not* a `Value` walk, so the grep kept passing while the fact it guarded had
already changed. Hence the pattern above matches a parse *call*, not a mention.

Overpass's validate-before-persist survived the move via a derived
`OverpassRemark` struct (serde skips `elements` without allocating). Its
200-status `remark` error channel must still be rejected before caching —
a 200 response is not a success there.

## Where a load's hitches went (2026-10-16)

Measured with `tools/cdp_probe.ts --gpu --profile-from 0 --profile-out`, a
release build kept unstripped (`CARGO_PROFILE_WEB_RELEASE_STRIP=none`, so the
profile has function names), in Chrome for Testing on a Mac node's Apple GPU
(how to set that up is in the probe's header and in
[checking-the-web-build.md](checking-the-web-build.md)). One 75 s load over
Groningen per run, two runs per side, alternated:

| | deployed code | fixed |
|---|---|---|
| main-thread busy time | 32.5 s, 42.8 s | 24.4 s, 22.9 s |
| of which in stretches ≥100 ms with no idle | 15.8 s, 26.2 s | 5.0 s, 4.5 s |
| re-preparing ground materials | 6.6 s, 8.5 s | 26 ms, 24 ms |
| a basemap tile's future, on the main thread | 1.8 s, 2.2 s | 0.3 s, 0.1 s |

**The big one was material churn, and it is a rule, not a one-off.** `get_mut`
on an `Assets<M>` marks the asset changed the moment it is dereferenced
mutably — assigning a field the value it already has counts — and a changed
material is re-prepared: its bind groups rebuilt from scratch. Three systems
(`ground_detail::refresh_ground_detail`, `map_stream::refresh_tile_lod_blend`,
`wet_surfaces::apply_wet_surfaces`) re-ran whenever the *number* of materials
changed, so that a newcomer gets the current value — which is right — and then
wrote *every* material unconditionally — which re-prepared the whole clipmap and
every ground surface once per tile landing, all through a load. **Read first,
write only what differs.** `building_fade` and `sewers` already did.

Re-preparing was so expensive on the web for a second reason: almost all of it was
`wgpu::backend::webgpu::map_wgt_features`. wgpu's webgpu backend answers
`Device::features()` by asking the browser about each of ~60 feature names, every
call, marshalling a string each way, and bevy's `AsBindGroup` asks it several times
per material prepare. It is not cached anywhere in wgpu 30. With the churn gone it is
~0.4 s a load, which did not justify vendoring wgpu to cache it; if a new hot path
calls `RenderDevice::features()` per item, it is worth reconsidering. *(2026-10-19: one did, and
wgpu is vendored — next section.)*

The rest was CPU work running *between awaits* in futures on the `IoTaskPool` —
on the web, the main thread. `workers::compute` existed for exactly this, and three
places still did not use it: `surveyed::raster_survey` (a projection per vertex and a
formatted key per area, then freeing two parsed files of megabytes), municipal
register parses, and `tzf_rs::DefaultFinder::new` (250 ms, in a system). The
shape to look for: anything after an `.await` in a future that loops over a parsed
file, or a large value whose **drop** happens there.

**Still costing, and upstream:** bevy compiles each new pipeline's WESL to WGSL on
the main thread — ~1 s a load, and the single worst remaining frame (~600 ms on
the M5) — and its mesh slab allocator copies vertex data there (~0.5 s). The first
frame after start is ~0.9 s of browser and bevy start-up.

**A freeze in the container's browser is the container's.** Headless Chrome there
presents on lavapipe; its GPU process spends 7–8 s inside single WebGPU flushes,
the page blocks on it (`GpuChannel::WaitForGetOffsetInRange` in a `--trace`), and
neither the debugger nor the profiler can reach the main thread meanwhile. On the
Mac's GPU the same build never froze. Use a node with a GPU for anything about
time.

## Moving the camera, and the wgpu fork (2026-10-19)

A load with the camera standing still under-reports what a person feels: a
settled scene streams nothing. `cdp_probe --drive 25` repeats a fixed script from
25 s on (pan, turn, zoom in and back out, pan), so runs compare. Same setup as
above: release build kept unstripped, Chrome for Testing on `mac-1`'s Apple GPU,
1600×900, 100 s, Groningen.

Without the fixes, a 75 s drive held ~53 fps on that machine (60 is vsync), with
100–700 ms freezes most seconds and the odd 1.5 s one. Each of these was found
inside those freezes and moved:

- **`map_wgt_features` was the largest single item: 3–8 s a run.** The paragraph
  above said to vendor wgpu once a hot path called `features()` per item, and
  material preparation is that path: tiles land all through a drive, each with a
  ground material, and each prepare asks several times. bevy also asks the
  *adapter* every frame (`prepare_prepass_textures` and
  `prepare_mip_generator_pipelines` through `get_texture_format_features`).
  `crates/wgpu` is wgpu 30.0.1 with `WebDevice::features`/`limits` and
  `WebAdapter::features` cached (every change marked `cartopolis:`), patched in
  with `[patch.crates-io]`. Preparing ground materials went from 4.0 s to 0.9 s a
  run.
- **`water_common.wesl` and `cloud_field.wesl` were each registered twice**: once
  by the shader loader, resolving `super::…` imports, and once by hand. Bevy then
  recompiled every pipeline built against the first copy
  ([shaders-are-wesl.md](shaders-are-wesl.md)).
- **The AHN height patch's TIFF inflate ran in a future**, so on the main thread:
  0.06–1 s a run, and `height::decode_patch` moves it to a worker. **A tile retired
  while its fetch was in flight dropped its payload in the future**, and that can be
  the last reference to its source's `Survey`. It is dropped on a worker now.
- `debug_switches::query_value` re-parsed `location.search` on every call, and some
  switches are read every frame. It now parses the query once.

Two runs each side, alternated:

| per 100 s run | before | after |
|---|---|---|
| main-thread busy | 47.2 s, 51.4 s | 38.8 s, 41.1 s |
| in stretches ≥100 ms with no idle | 13.7 s, 21.2 s | 9.0 s, 11.1 s |
| mean fps while driving | 53.2, 54.6 | 55.5, 53.9 |

The fps barely moves because this machine is vsync-bound. The busy time is the
headroom a slower machine would spend.

**This instrument is noisy, and it shows how noisy.** The Mac mini hosts both
Mac nodes, and while another session renders on the other one the host's load
average sits at 7–8 on 6 cores. V8's sampler then misses ticks: 12–30 s of each
100 s profile is gaps between samples, not code. A run-to-run spread of ±20 % on the
same build is normal under that load. Look at `ioreg -r -c IOAccelerator` and
`uptime` before trusting one run, alternate the builds, and run each at least twice.

**Still costing, in order:**

1. **Shader composing on a first visit, ~0.4 s a run.** `?pipeline_log` logs each
   new pipeline with its shaders and defs (`pipeline_health.rs`). One run built 69
   pipelines from 56 distinct (shader, defs) pairs. Most mid-drive ones are
   `mesh.wesl` + `ground_detail.wesl` again for a new vertex layout: ground-detail
   meshes arrive with and without `VERTEX_COLORS` and `VERTEX_UVS`, three
   combinations each composed separately. Giving every ground-detail mesh one
   layout would remove two of them. The rest of what this item used to cost is
   gone (next section).
2. **The render schedule itself**: `render_system`, `camera_driver` and extraction
   are ~40 % of busy time while driving. They scale with draws and entities, and on
   the web they share the one thread with everything else.
3. **One 790 ms `writeTexture`** inside `GpuImage::prepare_asset`, seen once,
   during a frame that also queued 10–14 MB of uploads. `upload_meter` warns about
   those frames. Which image it was is not established.

## Composed shaders are kept, and composed without WESL's checks (2026-10-19)

`crates/bevy_shader` is bevy_shader 0.20.0 with two changes, both marked
`cartopolis:`:

- **A release build composes without WESL's own validation and source map.**
  Profiled, they were over half of composing: `check_cycles` 0.76 s and
  `validate_wgsl` 0.52 s of 2.3 s in one run. They only make an error message
  better, and the renderer validates the WGSL either way. A debug build keeps them.
- **Composed WGSL is kept between runs** by `shader_store.rs`, one blob in
  `platform::storage`'s cache (`shaders/wgsl-v1.bin`, ~1.6 MB for a Groningen
  session). An entry is keyed by the main module's source, defs and constants. It
  lists every module it was composed from, by asset path and source hash, and is
  only used while all of them are unchanged. So a deploy that changes a shader
  recomposes it, and nothing has to be cleared by hand.

Main-thread time composing WESL over one 100 s drive, same probe as above
(sampler gaps capped at 3 ms, so these read low against the earlier table):

| | composing |
|---|---|
| before | 612 ms |
| first visit | 409–441 ms |
| second visit | 8–11 ms |

On a second visit the time inside ≥100 ms freezes was 3.4–4.2 s, against 9.3 s
for the build before. Most of that difference is not the shaders: the browser
also keeps its compiled copy of the wasm module and the app's tile caches. The
sampler gaps that made every first-visit profile noisy (12–30 s of 100) fell to
~2 s on a second visit, so much of that noise was the browser compiling the
157 MB module and not the shared host.

Two things to know when working on shaders:

- **A WESL mistake in a release build surfaces as the renderer's WGSL error**,
  naming mangled identifiers. Reproduce it in a debug build for WESL's own message.
- **`cdp_probe --keep-profile` is a returning visitor.** Without it every probe
  run is a first visit, because the browser profile is deleted. The probe kills the
  browser at the end, and a save from the last seconds of a run can be lost with
  it. That costs the next run one recompose, not a wrong shader.

## The phone tier (2026-08-09)

The measurement above was taken on a workstation GPU, and everything it ruled
out was ruled out *there*. A handset is a different machine in three ways at
once, and `crate::quality` exists to hold the one answer the rest of the client
reads:

* **Fill.** A Fairphone 4 reports `devicePixelRatio` ≈ 2.75, so a canvas laid
  across its 393 CSS-px viewport renders **1080×2340 real pixels** — with HDR,
  an atmosphere, a tonemapper and (until this) bloom over every one of them.
  That is ~2.5 Mpx on an Adreno 619.
* **One thread.** `bevy_tasks` disables its multi-threaded pool on wasm, so
  every system, extract, queue and batch step is serial. Draw *count* costs as
  much as draw cost.
* **Thermals.** 8 cores in `available_parallelism`, of which 6 are efficiency
  cores sharing an envelope with the render thread and the compositor.

`?quality=phone` / `CARTO_QUALITY=phone` forces the tier on any platform that
*has* a query string or an environment — which is how it gets profiled on a
machine that can profile it, and which excludes Android (see below);
`?quality=high` forces it off. Auto-detection has two arms, not one: on the web
it is coarse-pointer **and** a viewport under 900 px (either alone catches a
touchscreen laptop or a narrow desktop window), decided in `index.html`; on
Android `quality::auto()` simply *is* the phone budget, because there is nothing
to detect.

### Render scale is a page-layout problem, not a renderer setting

This is the part that is not discoverable from Rust, and the reason the detector
lives in `index.html` rather than in `quality.rs`.

On the web **the client does not choose its render resolution**. wgpu's surface
`configure` writes `canvas.width/height` from the window's physical size
(`wgpu/src/backend/webgpu.rs`), and that size is whatever winit's
`ResizeObserver` measured — it observes with `ResizeObserverBoxOptions::
DevicePixelContentBox`, i.e. CSS content box × DPR
(`winit/src/platform_impl/web/web_sys/resize_scaling.rs`).

Two consequences, both verified in the dependency sources rather than reasoned
about:

* **`Window::resolution.set_scale_factor_override` does not work here.** winit's
  web `request_inner_size` converts the requested physical size back to logical
  and sets the canvas **CSS** size, returning `None`
  (`platform_impl/web/window.rs`). Asking for fewer physical pixels shrinks the
  visible canvas instead of the buffer behind it.
* **A CSS transform does work**, because a transform is a paint operation and
  the device-pixel *content box* is a layout box. So `#stage` is sized to
  viewport ÷ k and scaled back up by k: the canvas covers the screen, the
  backing store is 1/k² of it. Use `transform`, never `zoom` — `zoom` is a
  layout property in the browsers that have it, so it would change the measured
  box and cancel itself out.

**Pointer coordinates survive the transform** — winit takes them from
`offsetX/offsetY` (`web_sys/event.rs`), which the browser reports in the
element's own untransformed space, the same space the backing store is in. Note
`Canvas::position()` *does* use `getBoundingClientRect`, which is
transform-affected; nothing on the input path reads it, and anything new that
does will be wrong by exactly k.

### What is in the phone budget, and why each is there

| Setting | Phone | Reason |
|---|---|---|
| Render scale | ≤1.5 DPR, ≤1.3 Mpx | ~30% of the pixels on a 2.75× screen |
| Shadows | off (1024² if re-enabled) | shadow casters were ~79% of submitted triangles |
| Bloom | `intensity = 0.0` | skips ~13 full-screen passes; the node early-returns on exactly 0 |
| Sky IBL | off | a cubemap + mip chain re-baked every frame |
| Trees, furniture, labels, textured roads, water shader | off | per-cell geometry the single thread meshes *and* draws |
| Clipmap | 424 → **224** tiles | over half the ground draw list and the streaming behind it |
| Globe leaf tiles | 320 → 160 | ~450 draws of planet, from one thread |
| Per-frame build/upload/despawn budgets | halved | these bound the worst-case hitch, not the throughput |
| CPU workers | ≤3 | see thermals above |
| MSAA | **off** | correctness, not cost: only 1× and 4× are guaranteed in Vulkan's `framebufferColorSampleCounts`, and a sample count the device rejects fails `main_texture_sampled` and renders *nothing* |
| Atmosphere LUTs | every axis halved | ~4 M ray-march steps per frame at Bevy's defaults, rebuilt every frame; sizes, not sample counts, because starving the integration bands instead |
| Space-tier raymarching | off | |
| Tile raster edge | 512² → **256²** | ~310 MB of clipmap texture against ~78 MB — [tile-raster-cost.md](tile-raster-cost.md) |

The atmosphere deliberately **stays on**: it is the sky, it A/B'd within noise
above, and the alternative is a void.

**The table is the tier**, so a knob that has no `Budget` field is a knob the
phone does not get. That is the rule the four rows added on 2026-08-10 exist to
enforce, and it is not a style preference: a zygote-launched NativeActivity
process has no settable environment and no `?query=`, so on the one platform the
phone tier was written for, "force it with a raw env var" is not an answer to
anything.

*Corrected 2026-08-16.* This used to say **every** env-var escape hatch is
unreachable on Android, naming `CARTO_FPS_CAP`, `CARTO_NO_SPACE_RAYMARCH` and
`CARTO_QUALITY`. That is true only of switches read through raw `std::env::var`.
Anything going through `crate::env_flag` / `env_value` maps to a
`debug.carto.<name>` **system property** on Android, which is the one namespace
`adb shell setprop` may write on a production build without root — so those *are*
reachable on a shipped APK with no rebuild. `CARTO_QUALITY` and
`CARTO_NO_SPACE_RAYMARCH` were always on that path; `CARTO_FPS_CAP` was the one
switch still bypassing it, and was moved onto it on 2026-08-16. The rule the
paragraph exists to defend is unchanged — a tier decision belongs in `Budget`,
not behind a debugging knob — but "unreachable" was the wrong reason for it, and
it hid the fact that a real device can be instrumented from `adb` alone
(`docs/notes/render-performance-survey.md`).

### Not measured

Nothing here has been run on the device. It was written against the constraints,
and the container it was written in has no GPU and no browser — `cdp_probe`
cannot run there at all, and `--shot` needs a Vulkan driver it does not have. So
the table above is a set of predictions with mechanisms, not results.

The one number that would settle it is a `tools/cdp_probe.ts --gpu --profile`
run, or a frame counter on the handset, **before and after**, at the same place
and altitude. Until someone has that, treat "phone tier holds 30 fps" as
untested — and in particular do not assume the ordering in the table is the
ordering of the wins. Render scale is first on reasoning about fill; if the
device turns out to be main-thread-bound instead, the clipmap radius and the
draw-count rows are the ones doing the work and the resolution could be given
back.
