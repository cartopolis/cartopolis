---
topic: render-performance-survey
triggers: [ShotMetrics, render_census, "draw call", "overdraw", "clipmap", "SIMD", big_space, "frame time", "profiling", "perf", "framerate", "fps", occluded_by_finer, "tier", quality.rs, Adreno, "gpu preprocessing", bindless, "task pool", ComputeTaskPool]
updated: 2026-08-18
---

# Where the frames go, and where they do not

A ten-lens survey of the client's render cost, 2026-08-16. Most of what follows
is **negative results** — directions that look obviously worth taking and are
not. Those are the expensive part to rediscover, so they are the part written
down. What was found *and* fixed is in the commits; this note holds the standing
facts around them.

## The instrument, and what it may be trusted for

`ShotMetrics` carries `draws`, `tris_total`, `tris_shadow`, the four per-streamer
triangle splits, `tiles_occluded`, `worker_outstanding`, `frame_ms` and
`frame_ms_max`. `systems::render_census` computes the first group for both the
shot harness and `log_render_cost`'s five-second line.

**The split that matters is workload versus machine.**

| Metric | Trustworthy in this container | Gate on it in CI |
|---|---|---|
| `draws`, `tris_*`, `tiles_occluded` | **Yes** — a property of what the client *selected*. Culling, LOD and visibility run the same code on lavapipe as on an Adreno | **Yes**, as an absolute ceiling |
| mesh/image/material asset counts | Yes | Leak-shaped rather than cost-shaped |
| Allocation counts (`crates/geo/src/perf_budgets.rs`) | Yes — bit-identical over three runs | **Already gated** |
| `worker_outstanding` | Timing-dependent | No — report only |
| `frame_ms`, `frame_ms_max` | Weak relative signal *within one run on one machine* | **No** |
| `elapsed_gpu` from `RenderDiagnosticsPlugin` | **No** — CPU rasteriser time wearing a GPU label | **No.** This is theatre |

Two limits are structural and documented at the fields. Triangle counts read **0**
for anything uploaded `RENDER_WORLD`-only — tiles, globe, transit — because the
main world no longer holds the geometry, so on the globe tier `draws` is the only
column carrying meaning. And `worker_outstanding` is sampled at the settle
*verdict*, not the shutter, because the harness quiesces the pool between them.

`frame_ms` was clipped at both ends until 2026-08-16: it accumulated `Res<Time>`,
whose delta Bevy clamps at `max_delta` (0.25 s), so on any renderer slower than
four frames a second it read exactly 250.0 for every scene, while `frame_limiter`
floored it at the tier's cap from the other side.

### Reference measurements

Lavapipe, 640×360, Groningen, dev profile, 2026-08-16. Frame times are the CPU
rasteriser's and are **not** comparable to a device; the counts are exact.

| Viewpoint | draws | tiles_occluded | frame_ms |
|---|---|---|---|
| 2000 km orbit, straight down | 94 | 0 | 352 |
| 3 km, −30° | 367 (was 401) | 34 | 1082 |
| 150 m, −20° | 653 | 70 | 971 |
| 3 km, −30°, `CARTO_QUALITY=phone` | 209 | — | 491 |

## Negative results — do not re-open these first

### SIMD is already done, and the remaining candidates are not vectorisable

`.cargo/config.toml` is a *measured* baseline document, not boilerplate:
`target-cpu=x86-64-v3` on every x86 lane and `+simd128` on wasm32, with ~30% off
the mask raster and ~2× off the shore-field tail recorded against it. Three of the
four sites that look most promising were already vectorised and carry their
numbers: `squared_distance_transform` (~3×, bit-identical), `mip::halve_triangle`
(~2×), `paint_water_alpha_by_colour` (~2.5×).

Availability needs no work either. NEON is baseline on aarch64 — the Android lane
correctly sets no RUSTFLAGS — and `simd128` is repeated in every wasm lane because
RUSTFLAGS *replaces* rather than merges the config entry. glam 0.32 selects its
NEON/SSE2/simd128 arms automatically and neither `scalar-math` nor `core-simd` is
enabled anywhere.

Two consequences worth knowing. `x86-64-v3` is a **baseline assumption, not
dispatch** — shipped desktop binaries already require Haswell — so further x86
widening must be runtime-dispatched or stay at v3. And `Vec3A` is not the win it
looks like: mesh attributes must stay packed `[f32; 3]` for upload, so converting
would add a repack costing more than it saves.

**Not vectorisable, with the reason:** earcut (pointer-chasing ear clipping), MVT
varint decode (serially dependent by construction), `clip_ring` (data-dependent
output length), the routing graph's `finish`/snap/A\* (hash probes and heap
operations), `lower_envelope`'s stack machine. tiny-skia's scan conversion — the
121 ms — is inside the dependency and *is* already SIMD.

Where a loop genuinely was hot, the fix was algorithmic, not SIMD:
`terrain::bake` hoisted anchor-invariant trig and exploited Mercator separability
for ~10–30×.

### The building colliders do not need a spatial index

`collision::blocked` is a linear scan over every footprint and CLAUDE.md calls the
structure a "footprint index", which it is not. Both look like an obvious ~100×.
Do the arithmetic before building one: `blocked` is called **at most ~5 times per
frame** (`slide` does up to four, `steer_velocity` a couple), each footprint costs
an AABB test that the first compare usually rejects, and the worst-case patch is
6,776 buildings — so the whole per-frame cost is on the order of **70 µs**.

A uniform grid has to be rebuilt whenever a source changes, and 3DBAG cells arrive
several times a second while streaming. Rebuilding ~6,800 footprints into their
overlapping cells is ~0.5–1 ms, i.e. the index would **cost more on the frames it
is rebuilt than it saves on the frames it is queried**. It becomes worth building
only if the query rate rises by an order of magnitude — several colliding agents,
or a per-vertex query — and the honest trigger is a measurement, not the shape of
the loop.

### big_space is not the bottleneck

~800–1000 entities, all flat children of exactly one `Grid`; no nesting anywhere
in the client. High-precision propagation is order **tens of microseconds**. The
vendored fork's divergence from Bevy 0.19's dirty-tree propagation is upstream's
deliberate choice, inherited, and costs nothing here: the only deep low-precision
subtrees are avatar skeletons, which animate every frame, so nothing would be
skipped.

Upstream's post-0.10 perf work does **not** transfer. `Stationary` and the
`GridDirtyTick` in `2f6f4fc` are *per grid*, and this app's single grid contains
the camera and the avatar, so it is dirty every frame by construction; the 6×
figure in `17549ac` is from a many-grid scene. Only `961cc0c`'s two `set_if_neq`
lines are worth cherry-picking. A specialised replacement of the general machinery
would save microseconds and cost the upstream relationship — the machinery is
cheap here *precisely because* the hierarchy is flat.

**`free_camera.rs`'s comment claiming that any camera `Transform` change forces an
O(scene) `GlobalTransform` recompute is false.** `LocalFloatingOrigin::compute_all`
seeds from the origin's *cell*, so moving the camera a metre recomputes one
transform. The write-back guard is still worth keeping — it stops the camera's own
`Changed<Transform>` firing, which a great many systems key off — but whatever
frame-time win was once measured came from somewhere else.

`BigSpaceValidationPlugin` walks every entity every frame as an *exclusive* system
under `debug_assertions`, estimated at 1–3 ms. It is `cfg`-disabled in release,
android-release and web-release, so it costs a shipped build nothing, and at
~1000 ms/frame on lavapipe it cannot be measured here. Left alone deliberately;
settling it needs a GPU workstation or a `trace_chrome` capture.

### The ECS side is disciplined

No missing `is_changed()` throttle was found, and no unconditional `ResMut` deref
defeating an existing one. `.chain()` is used exactly twice and both are correct.
`theme.rs` is not a per-frame cost (`install` runs once from `EguiManager::default`;
~850 of its 2318 lines are tests). There is no `FixedUpdate` anywhere, so there is
no timestep death spiral.

The real ceiling is **parallelism**: 28 systems take `ResMut<EguiManager>` and ~33
take `ResMut<Assets<…>>`. Those two disjoint cliques cover ~61 of ~150 Update
systems, so everything that does real work is at most 2-way parallel. On a 4-core
phone that leaves two cores idle for most of Update. Most clique members early-out,
so the realised cost concentrates in the streamers — but the ceiling is structural
and no amount of `.after()` tuning moves it.

**And until 2026-08-18 the realised figure was 1-way, for an unrelated reason.**
`ComputeTaskPool` *is* Bevy's parallel system executor, and its width is not
configured anywhere — `create_default_pools` sizes io, then async_compute, then
compute, each from the last one's remainder, and `get_number_of_threads` applies
`min(remaining)` **before** `clamp(min_threads, max_threads)`. On the 8 cores an
Android device reports, `lib.rs`'s override resolved to io = 4 (its floor beating
its 0.30 share), async_compute = 4 (0.45 × 8 rounded up, taking the rest), and
compute = `min(8, 0)` clamped up to its default floor of **1**. Measured by
reading `/proc/<pid>/task/*/comm` on an 8-core host: 1 compute thread before the
fix, 3 after.

Two things worth keeping from that. It was invisible for months because the io
and async widths were measured on a 22-core box, where the same policies give
io=7, async=10, compute=5 and the failure cannot occur — a pool width that is
*derived* rather than declared needs a device with the awkward core count to
falsify it. And it is why the diagnostics report now prints all three widths: the
next person to move a percentage here has no other way to see what they did on
hardware they do not own.

### Android frame pacing is fixed (2026-08-16)

`fps_cap: None` on the phone meant "do not run a `thread::sleep` limiter on top of
`AutoVsync`", which is right, and was being read as "run as fast as the panel
will go", which is not the same instruction. FIFO paces to *the display*, so a
90/120 Hz handset rendered a 30 fps-budgeted scene at 90/120 and was thermally
throttled for it — and the ADPF deadline was hardcoded to `1e9/60` regardless, so
on those same devices every frame was reported comfortably on time and the
governor never had cause to boost.

`Budget::target_fps` is the number to tell the *compositor*, distinct from
`fps_cap`, and `platform::android::set_frame_rate` delivers it through
`ANativeWindow_setFrameRate` — `dlsym`ed from `libandroid.so` for the same reason
ADPF is, since an API-29 handset is not misconfigured. It is a hint: the platform
may pick a multiple, or ignore it on a device with no seamless mode switching, so
it logs what it got. Retried until `native_window()` exists, because that is
`None` for the first few seconds after `android_main`.

**Unverifiable here.** The emulator is fixed at 60 Hz, so this needs a real
90/120 Hz device — `adb shell dumpsys SurfaceFlinger | grep -i refresh` plus
battery temperature.

### …and it came undone, and is fixed again with an idle throttle (2026-08-21)

Reported from the OnePlus 15: fast enough, extremely hot. It had regressed.
`report_frame_to_scheduler` was passing a hardcoded `0.0` to
`ANativeWindow_setFrameRate` — the API's documented "the app will accept the
system's choice of display frame rate" — so `Budget::target_fps` reached nothing
but the ADPF seed and a 120 Hz LTPO panel got exactly what it asked for.

The argument for the `0.0` was not silly and is worth keeping straight, because
half of it is still true. It ran: 60 is not a property of any display, it is a
property of the desktop the tier table was written on, and this codebase cannot
*read* the panel's rate — winit's Android backend answers `None` for the refresh
rate and fabricates a 60 in `video_modes`. All correct. What it got wrong is that
handing the choice back is not neutral when the choice is "as fast as possible":
the compositor optimises for smoothness, and nobody in that conversation
represents the battery. `target_fps` is not a claim about the panel — it is a
statement about how much work the tier is willing to do per second, which *is*
something we know. And the panel's rate is measured now anyway (`Cadence`), so
ADPF's deadline never depended on the constant.

**The rung test could not have caught it**, and that is the reusable part.
`quality::every_rung_asks_the_compositor_for_a_rate` asserts what `target_fps()`
returns; the bug was that nothing passed the return value on. Same shape as the
two demotions already recorded in `quality.rs` — a test that describes a preset
rather than a call site. The resolution is a plain function now
(`frame_pacing::resolve_frame_rate`), compiled on every target, with the
assertion on it.

On top of that, an **idle throttle**: `frame_pacing::present_rate` asks for half
the rate (floored at 30) after `IDLE_AFTER_S` of nothing happening — no input
held or just released, camera not moved, driver mode off, no bytes uploaded this
frame and no tile job outstanding. The streamers are in that list because the
per-frame build budgets are *per frame*, so a cold map filling in while you sit
still would otherwise take twice as long to arrive.

Two things about the mechanism. **The compositor is the lever, not a sleep**: a
`thread::sleep` on top of FIFO is the second clock `frame_limiter` exists to
refuse, whereas `setFrameRate` is the *same* clock — FIFO then blocks on acquire
for the longer period and the whole loop throttles with it. And the rate is one
resource (`PresentationRate`) with two consumers, so the desktop limiter gets the
idle drop from the same decision rather than a parallel one that can drift.

**The deliberate trade:** the world keeps animating while idle — sun, waves, a
remote avatar walking past — and it animates at the idle rate. Hence a floor of
30 rather than something lower.

**Unverified.** Same wall as the 2026-08-16 entry: the emulator is fixed at
60 Hz, so neither the mode switch nor the heat can be measured here. On the
device, `debug.carto.fps_cap` A/Bs it without a rebuild (`0` = uncapped, which
also disables the idle throttle so a headroom measurement is not measuring
itself), and the transitions are on `debug` in logcat. `IDLE_AFTER_S = 2.0` is a
guess and is the first number to move if the drop is noticeable.

### The repeat parses are collapsed (2026-08-16)

`tile_loader::parse_shared` keys a `Weak<TileData>` cache on the **source** tile,
which is the ancestor when overzooming — so the finest clipmap ring's 81 z17
tiles share **4** parses rather than doing 81 (pinned by a test). The globe,
`map_geometry`, both tile-sourced data layers and the routing corridor go through
the same cache, so ground being streamed by several consumers at once is parsed
once between them.

It stays `Weak` on purpose: a fully-decoded dense z14 tile peaks at ~12.5 MiB and
the globe can want hundreds at once, so anything strong would be a memory policy
rather than a cache. That number, from `allocation-budgets.md`, is what
`tile-raster-cost.md` said this was blocked on.

The enabling change was `TileData`'s per-layer memo moving from
`std::cell::OnceCell` to `OnceLock`, i.e. from `Send` to `Send + Sync`. A
compile-time assertion in `crates/geo` holds it there, because `cartopolis_geo` is
single-threaded and would otherwise test green while breaking the cache.

**Not visible in `settled_s` on this container**, and that is expected: lavapipe
spends ~1 s per frame rasterising, so a cold ground fill is bounded by frames
drawn, not by tiles decoded. The saving is in worker time and allocation traffic.

### The unused MVT layers are not decoded

`docs/notes/vector-tile-unused-layers.md` says the geometry cell "decodes all of
it". That stopped being true when `TileData::features` became lazy per layer:
`place_labels` and `street_labels_points` are never decoded at all, and every
"loop over all layers" site filters before touching `features()`. The note's
conclusions hold; its cost model does not.

The real duplicate is `PAINT_ORDER` listing `buildings` and `streets` **twice**.
The decode is memoised; the path building and the bounds scan are not.

## Open, with the reason they are still open

- **`osm_buildings` blocks io-pool threads with `std::thread::sleep`** on three
  paths inside `async fn`s. `async-io` is in the graph only transitively; making
  it a direct dependency, with the reactor thread that implies on Android, is a
  decision to take on its own.
- **egui is a second full-resolution pass over the swapchain** with `LoadOp::Load`
  — a full tile-memory load and store of the 1080×2340 output, every frame, purely
  to composite the UI. Merging it into the `upscaling` pass removes the round trip
  and is the real win here. It means replacing Bevy's `upscaling` node with a local
  one that blits and then hands the open `RenderPass` to `egui_wgpu`;
  `render_scale::make_the_blit_filter` shows `BlitPipeline` is reachable from
  `RenderStartup`, so the pieces exist. The failure mode is a wgpu validation
  error, and this tree already knows what that costs — a silent mid-stream quit.

  **Clamping `pixels_per_point` is *not* the cheap version of this**, which is
  worth stating because it looks like it. egui's ppp is the layout unit and the
  rasterisation density at once: lowering it from 4.1 to 2.0 does not render the
  same UI more cheaply, it makes every element physically smaller — i.e. it pays
  for fill with touch-target size, on the platform where touch targets matter
  most. Raising `ui_scale` to compensate puts the number straight back, because
  ppp *is* `display_scale × ui_scale`. There is no "lay out large, rasterise
  small" mode short of rendering egui to a smaller target and upscaling it, which
  is exactly what `render_scale` deliberately does not do to the chrome so that
  text stays crisp.
- **Nothing prewarms the pipeline cache**, and `StandardMaterial::depth_bias` is
  part of the specialization key, so each of the ~25 depth-bias rungs is a
  distinct shader compile. Adreno compiles run 50–300 ms apiece and geometry whose
  pipeline is not resident is *silently skipped*. Costs time-to-a-complete-picture
  after launch and after every teleport, not steady-state frames — and `--shot`
  waits for settle, so the harness hides exactly this.

## A trap in the instrument itself

**`--frames` sweeps only place the first viewpoint.** Every capture after the
first reports `alt_m=0` with `space_factor` and `globe_albedo` as `NaN`
(reproduced 2026-08-16 on a 6-frame 60 km → 3000 km climb, and confirmed
pre-existing by re-running against a clean tree). The likely cause is that each
waypoint is a teleport, a teleport re-anchors, `setup_globe` rebuilds, and the
settle test waits on the tile streams but not on the globe being placed — so the
shutter fires while `GlobeState` is still empty.

This matters more than it looks, because `--frames` + `--csv` is precisely the
tool CLAUDE.md documents for catching *a hard cut in a ramp* — the one thing a
single still cannot show. Until it is fixed, verify a ramp with separate `--shot`
runs at several altitudes, which does work:

    400 km   space_factor 0.039   globe_albedo 0.983
    1500 km  space_factor 0.487   globe_albedo 0.781
    3000 km  space_factor 1.000   globe_albedo 0.550

## What a phone cannot be measured on here

Lavapipe is a CPU rasteriser: geometry, placement, culling and visibility are
trustworthy, colour and lighting are not (`headless-shots-software-renderer.md`).
The VPS Android emulator renders on SwiftShader and **aborts after ~9 s**
(`egui-wgpu: Tried to update a texture that has not been allocated yet`), so it
cannot hold a steady frame at all — it settles startup order, asset loading,
lifecycle, memory and panics, not performance. `simpleperf` is worse than useless
under `ndk-translation`, which executes translated x86_64 and profiles the
translator.

**The minimum real-device loop needs no rebuild.** `env_flag` on Android reads
`debug.carto.*` system properties, the one namespace `adb shell setprop` may write
on a production build without root:

```
adb shell setprop debug.carto.fps_log 1
adb shell setprop debug.carto.render_cost 1
adb shell am force-stop eu.garage44.cartopolis
adb shell monkey -p eu.garage44.cartopolis -c android.intent.category.LAUNCHER 1
adb logcat -s cartopolis:V | grep -E 'frame stats|render cost'
```

That gives per-second fps/jitter and the five-second draws/triangle census off a
shipped APK. `wasm-performance.md`'s claim that "every env-var escape hatch is
unreachable on Android" is true only of switches read through raw
`std::env::var`; anything going through `env_flag`/`env_value` is reachable, and
`CARTO_FPS_CAP` was moved onto that path on 2026-08-16.

## The engine constraint behind the draw-count work — **lapsed 2026-08-18**

`bevy_render`'s `gpu_preprocessing` excludes **Adreno ≤ 730 (except 720) and Mali
drivers below 48** from GPU preprocessing entirely — `GpuPreprocessingMode::None`.
The Fairphone 4's Adreno 619 was in that bucket, so on the then-target device
there was no indirect draw, no GPU culling and no cross-mesh batching: every mesh
was a CPU-issued draw with its own bind group. That is why `draws` became a
first-class metric here rather than a proxy for triangles, and why it was a
main-thread number as much as a GPU one.

**The filter is unchanged; the device is not.** `bevy_render/src/lib.rs:531-545`
strips the literal prefix `"Adreno (TM) "` and parses the digits, and the test at
`batching/gpu_preprocessing.rs:1333` is `model != 720 && model <= 730`. An Adreno
840 fails it and lands in the *supported* branch — so GPU culling, indirect draw,
cross-mesh batching and GPU bin unpacking are all live on the current target, and
with them the Bevy 0.19 rendering wins that are downstream of them. Note the
filter also fails open: if Qualcomm changes the string, `strip_prefix` returns
`None` and the device is treated as supported.

Two consequences for anyone reading the draw-count sections above:

- `draws` is still worth measuring, but it is no longer *the* number. On a part
  that batches, what matters is how many distinct **batch sets** the draws fall
  into — `pipeline + material bind group + mesh asset` — which is why the
  material-sharing work (one handle per building lane, one per surface group)
  buys more than the draw count alone suggests.
- Whether it collapses further depends on **bindless**, which `StandardMaterial`
  declares (`#[bindless(index_table(range(0..31)))]`) and which engages only if
  the driver reports `TEXTURE_BINDING_ARRAY | BUFFER_BINDING_ARRAY` and enough
  binding-array sampler slots. If it is live, the 224 per-tile materials and the
  160 per-label materials stop mattering on their own. That is a log line
  (`RUST_LOG=bevy_render=debug`, "Configured wgpu adapter Features"), not a
  guess, and it should be read before anyone builds a tile atlas or a texture
  array to solve a problem the driver may already have solved.

## The target device, and what it voids

Until 2026-08-18 the Android target was a **Fairphone 4** — Adreno 619, 512 KiB
of on-chip tile memory, ~17 GB/s, 2 performance + 6 efficiency cores. It is now a
**OnePlus 15**: Snapdragon 8 Elite Gen 5, **Adreno 840**, **18 MB** of tile memory
(Qualcomm's own a8xx kernel enablement series states the figure), ~85 GB/s
LPDDR5X, and 2 prime + 6 performance Oryon cores with no efficiency cores at all.
Older phones are explicitly out of scope.

Three arguments in `quality.rs` were built on the old numbers and do not survive
the change, which is why `auto()` now puts Android on `Budget::balanced`:

- **The tile budget that ruled out `Hdr`.** At the panel's 2772×1272, RGBA8 +
  D32 is 2 bins and `Rgba16Float` + D32 is 3, against 39 and 58 on the 619. HDR
  is affordable, and it also moves tonemapping out of every mesh fragment back
  into one pass.
- **The fill scarcity that halved the render scale.** Still a real cost — tile
  memory does not make fragments free — but against ~5× the bandwidth and a far
  faster GPU, and a point-sampled 2× upscale was the most visible quality cost in
  the app.
- **The 2-big-6-little thermal envelope** behind `worker_cap: Some(3)`. There are
  no little cores.

**One field does not come up with the rung**, and it is the one this device has
actually broken: `upload_bytes_per_frame`. It is `None` on every rung above the
phone, on the sound reasoning that those rungs describe machines whose uploads
cross a much wider bus and none of which has lost a device to streaming — but a
handset on the balanced rung is neither case, and this handset lost its Vulkan
device to a cold-start burst *while on the phone budget*. So `auto()` takes
`balanced` and keeps the 4 MB ceiling explicitly. Note the same change raises the
pressure on it: 512 px tile rasters are 4× the texture bytes of 256, and
`radius_bias: 0` nearly doubles the tile count. With the ceiling kept that is
fill latency; without it, it is the burst again.

One prediction made from a desk was **wrong**, and it is worth recording as
wrong: FP32 linear filtering was assumed to be a standing Adreno architectural
gap, so the flagship was expected to lack `FLOAT32_FILTERABLE` and to keep the
analytic dome. The device reports the feature. It was the *tier*, not the
adapter, that was vetoing the sky.

The fix for that was itself a vendor-family generalisation — a table of GPU model
numbers, `gpu_affords_atmosphere` — and it survived one day before being deleted
along with `Budget::sky_dome` (2026-08-18). The sky is now decided by asking the
adapter, through `quality::Capabilities`. So the lesson landed twice in two days
and is worth stating flatly: **a vendor-family generalisation is not a
measurement**, this file should not have carried one, and neither should the fix
for it. Where a capability can be queried, query it; where it cannot, let a person
choose a rung.

Two of the chipset's headline features are
structurally unreachable rather than merely unused: Bevy 0.19 contains no
reference to `EXPERIMENTAL_MESH_SHADER` (wgpu 29 exposes it; the engine has no
draw path), and the explicit 18 MB tile heap is `VK_QCOM_tile_memory_heap`, which
wgpu 29 does not support at all — the memory is reachable only as implicit GMEM.

**Measurement is harder on this device, not easier.** `dumpsys gfxinfo` is
useless for a Vulkan NativeActivity surface, AGI's whitelist does not include it,
and there is no workstation in the loop — so the shareable report
(`systems::dev::diagnostics`) is the primary instrument, and since 2026-08-18 it
carries frame time, draws, the triangle split and the three task-pool widths for
that reason.
