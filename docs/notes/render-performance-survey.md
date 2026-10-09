---
topic: render-performance-survey
triggers: [ShotMetrics, render_census, "draw call", "overdraw", "clipmap", "SIMD", big_space, "frame time", "profiling", "perf", "framerate", "fps", occluded_by_finer, "tier", quality.rs, Adreno, "gpu preprocessing", bindless, "task pool", ComputeTaskPool, CARTO_NO_CLOUDS, globe_clouds, "clouds", "slow at altitude", "fps drops when zoomed", fbm3, "shadows", "shadows are slow", ShadowFilteringMethod, NotShadowCaster, NoFrustumCulling, shadow_casters, shadow_distance_m, shadow_cascades, CascadeShadowConfigBuilder, CARTO_NO_SHADOWS, "moon shadow"]
updated: 2026-09-14
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

## Correction: the 2026-09-22 Deck runs had three effects switched off (2026-09-30)

**Every `deck-1` figure dated 2026-09-22 below was measured with shadows, light
shafts and sky lighting all off.** Those runs set no `CARTO_DATA_DIR`, so the
client read the Deck bot's own `user.json`, which has all three switched off. The
mechanisms below still hold. The absolute figures describe that configuration,
not the default. In particular, "`CARTO_NO_SHADOWS` moves nothing" was measured
with shadows already off.

Re-run 2026-09-30 at `636c6c5` with a fresh data directory (same setup otherwise,
every steady window `quiet=true`, one run per row; repeats agreed within 0.3 %):

| city, steady | frame | `submit_pending_command_buffers` |
|---|---|---|
| defaults (desktop tier) | 29.9 ms | 12.6 ms |
| sky lighting off | **12.4 ms** | 2.4 ms |
| shadows off (light shafts go with them) | 25.3 ms | 6.7 ms |
| light shafts off | 26.1 ms | 9.8 ms |
| sun rays, bloom, anti-aliasing, or `CARTO_NO_CLOUDS` | ~30 ms each | — |

Orbit: 25.3 ms with defaults, 10.4 ms with sky lighting off. The same comparison
at `917a2f6`, the 2026-09-22 commit, gives 31.4 ms against 12.1 ms, so no commit
made this slower: it was always the default cost.

**Sky lighting (`AtmosphereEnvironmentMapLight`) is about 17.5 ms of a 30 ms
frame.** Bevy re-renders the sky cubemap and re-filters it into the diffuse and
specular maps every frame. It is GPU time that the render thread waits for inside
`submit_pending_command_buffers`. **None of it appears in `RenderDiagnosticsPlugin`'s
pass list**: bevy_pbr opens `lightprobe_*` spans, and they never reach
`DiagnosticsStore`. The shadow passes are also missing from that list. So the
"measured GPU passes" column cannot see the largest cost in the frame.
`systems::sky_bake` re-bakes only when the sun, the moon or the camera's place
in the atmosphere has moved.

## On a real GPU the frame is none of the things this note was optimising (2026-09-22)

Everything above was measured on the container's CPU rasteriser, where the table
says not to read a millisecond. `deck-1` is an AMD Van Gogh with Vulkan
timestamps, so `elapsed_gpu` there is a real GPU number and the whole suite was
re-run on it. Dev profile, default 640×360, tiles from
`tiles.cartopolis.org`, every steady window `quiet=true`.

| scenario | frame | fps | draws | entities | measured GPU passes | **unaccounted** |
|---|---|---|---|---|---|---|
| orbit (180 km) | 8.95 ms | 112 | 149 | 1109 | 4.11 ms | 4.84 ms (54 %) |
| low-orbit | 7.82 ms | 128 | 106 | 985 | 4.03 ms | 3.79 ms (49 %) |
| city (3 km) | 14.41 ms | 69 | 522 | 2295 | 4.00 ms | 9.73 ms (72 %) |

**Measured GPU pass time is the same 4 ms in all three.** Three scenes differing
by 5× in draw calls cost an identical amount of instrumented GPU work, while the
unaccounted remainder tracks the scene. Whatever sets a frame here is not in a
pass the instrument can see.

### Three candidates were eliminated, each by an A/B on that hardware

- **Fill.** A resolution sweep of `city` at 640×360 / 1280×720 / 1920×1080 —
  1×, 4×, 9× the pixels, and `fragment_invocations` duly 0.57 M / 2.2 M / 4.9 M —
  moved the frame 13.79 → 15.32 → 23.58 ms. The unaccounted remainder was 9.82 /
  9.13 / 15.19 ms: a **floor of about 10 ms that nine times the pixels barely
  touches**.
- **The shadow filter.** `CARTO_SHADOW_FILTER=fast` against `soft` cut the opaque
  pass from 2.385 to 1.759 ms — a real, repeatable 0.63 ms, 26 % of that pass —
  and the frame went 13.77 → 13.87 ms, i.e. nowhere. **0.6 ms of GPU pass time was
  removed and no frame time came back.**
- **Shadows entirely.** `CARTO_NO_SHADOWS=1` on `city`: 13.73 → 13.94 ms, with
  `draws` unchanged at 522. The sun is the only casting light (the moon has
  `shadow_maps_enabled: false` unconditionally), so this is the whole shadow cost,
  caster passes included, and it is **not on the critical path at all**.

That last one is worth stating plainly because this note's "not done, and it is
the big one" is shadow-map caching. On this hardware, at these scenarios, caching
every cascade perfectly would buy nothing. It may still be the big one on an
Adreno, which has a fraction of the fill rate and where the note's phone figures
were never taken — but it is not what a Deck frame is made of, and the claim
should not be repeated without a device measurement behind it.

### What the remainder does correlate with

| scenario | unaccounted | entities | µs per entity |
|---|---|---|---|
| orbit | 4.84 ms | 1109 | 4.36 |
| low-orbit | 3.79 ms | 985 | 3.85 |
| city | 9.73 ms | 2295 | 4.24 |

**Not draws, not pixels, not shadows — entity count**, at a near-constant
~4.2 µs each. Beside it, `process_cpu_percent` is flat at 21–23 % across all
three while the frame varies by a factor of two: on eight cores that is roughly
two busy threads and six idle, which is what a single-threaded bottleneck looks
like rather than a machine out of capacity.

Three points is a correlation and not a cause, and it is deliberately not written
up here as one. The honest statement is that the cost scales with how many
entities the main world holds, and that nothing in the render passes explains a
frame.

### The per-system profiler cannot be reached, and fails silently

`bevy/trace` and `bevy/debug` are both already in the `perf` feature, so the spans
exist and their names resolve. Collecting them is the problem: **`bevy/trace_chrome`
lives inside `LogPlugin`, and `lib.rs` disables `LogPlugin` on desktop** in favour
of this tree's own `tracing_subscriber`. Building with the feature therefore
succeeds, runs, and writes no trace file at all — no warning, no error, one full
client rebuild to discover.

Reaching those spans means installing a chrome layer on *our* subscriber under the
`perf` feature, which needs `tracing-chrome` as a direct dependency (it is not in
`Cargo.lock` today, not even transitively). That is the next step for anyone
continuing this, and it is the step that turns the correlation above into a name.

### The missing 10 ms is CPU work on the render thread (answered 2026-09-22)

The per-system profiler below settled it. Per-system spans, split into 100-frame
buckets so a streaming cost cannot be read as a frame cost — `city` on `deck-1`,
`Main::run_main` counted once per frame:

| system | streaming (frames 200–299) | **settled (frames 1600–1699)** |
|---|---|---|
| `bevy_render::run_render_schedule` | 46.8 ms/frame | **14.6 ms/frame** |
| `bevy_render::renderer::render_system` | 13.3 | 6.8 |
| `bevy_app::main_schedule::Main::run_main` | 192.0 | **5.9** |
| `bevy_core_pipeline::schedule::camera_driver` | 5.8 | 4.0 |
| `write_binned_instance_buffers<Opaque3d, MeshPipeline>` | 4.5 | 2.4 |
| `submit_pending_command_buffers` | 6.4 | 2.3 |
| `prepare_preprocess_bind_groups` | 3.1 | 1.6 |
| `main_opaque_pass_3d` | — | 0.9 |
| `map_geometry::update_map_surfaces` | **170.4** | **absent from the top 12** |

**The render schedule is the critical path and the main world is not.** Settled,
`run_render_schedule` is 14.6 ms against a 14.4 ms frame, while the whole main-world
schedule is 5.9 ms — the two run in parallel under bevy's pipelined rendering, so
the frame is the render thread's and the main world has headroom to spare.

**And the GPU passes are 4.0 ms of that 14.6.** The remaining ~10.6 ms is
render-thread *CPU* work — extraction, specialization, batching, bind-group and
buffer preparation — which is the same ~10 ms the table at the top of this section
could not account for. Every earlier negative result follows from it at once:
resolution did not matter because this work is resolution-independent, shadows did
not matter because removing a caster pass does not remove the preparation around
it, the cost tracked entity count because extraction and batching are per-entity,
and process CPU sat flat near two busy threads because exactly two are on the
path.

Two things in that settled column are worth someone's attention, and neither is a
pass:

- **`camera_driver` at 3.6 ms/frame settled**, second only to `render_system`.
- **Instance-buffer and batching preparation at ~4.2 ms/frame together** —
  `write_binned_instance_buffers` 2.36, `prepare_preprocess_bind_groups` 1.52,
  `batch_and_prepare_binned_render_phase` 0.34. All three scale with the number
  of batched draws, of which this viewpoint has 522, so **draw count is the lever
  on the render thread's CPU** even though it is not the lever on the GPU passes.

  An earlier draft of this section called that 2.4 ms row "pipeline
  specialization" and it is not. The chrome trace truncated the type name at 58
  characters and `Opaque3d, MeshPipeline` was read as a specialization key; the
  in-harness counter prints the whole path and it is
  `bevy_render::batching::gpu_preprocessing::write_binned_instance_buffers`.
  Nothing here measures specialization, and this note's pipeline-cache entry is
  neither supported nor contradicted by it.

**`update_map_surfaces` is a streaming cost and not a frame cost**, which is what
the whole-run total of 53.7 s could not distinguish and what this split exists to
say. One caveat travels with every row: this workspace's own crates build at
`opt-level = 1` and dependencies at `3`, so our systems and bevy's are not on
equal terms in this table.

### The first measured win off all this: cull the clipmap (2026-09-22)

The clipmap carried `NoFrustumCulling` with the reason "flat tiles + big_space
rebasing confuse frustum culling", so every tile in the pyramid was submitted to
the main pass whether or not it was on screen. big_space was not the cause. The
cause is that `stream.quads` caches one quad mesh per zoom and reuses it, and
bevy's `calculate_bounds` gives an `Aabb` only to an entity that has none, only
while it can see the mesh's positions — for a stashed, reused handle that is the
first spawn and no other. **An entity with no `Aabb` is culled exactly as little
as one that opted out**, so the component was describing the symptom.

Stating the bound at the spawn site, `deck-1`, all three scenarios:

| scenario | draws | frame |
|---|---|---|
| orbit (180 km) | 149 → **149** | 8.95 → 9.5 ms |
| low-orbit | 106 → **106** | 7.82 → 8.7 ms |
| **city (3 km)** | 522 → **331** | 14.41 → **11.2 ms** |

**Every figure in this section was measured before the branch was rebased**, and
`main` moved underneath it during the day — `634be61` deleted the depth prepass
and `4a214b2` changed the basemap cross-fade, both in the render path. The
*mechanism* is unaffected, since a tile with no `Aabb` cannot be culled whatever
else changed, but the exact saving on today's `main` has not been re-measured.
Treat the ratios as sound and the absolute milliseconds as of that branch.

**City: 191 fewer draws (−37 %) and 3.2 ms (−22 %)**, 69 fps to 89. The two orbit
scenarios are *unchanged*, which is the correctness check rather than a
disappointment: from 180 km looking straight down the whole pyramid is in frame,
so a correct culler removes nothing, and a drop there would have meant ground was
being deleted. The settled capture shows continuous ground to the horizon.

The saving lands exactly where the per-system table said it would, which is what
makes this a confirmation rather than a coincidence:

| settled, ms/frame | before | after |
|---|---|---|
| `run_render_schedule` | 13.5 | 9.94 |
| `render_system` | 6.36 | 4.85 |
| `camera_driver` | 3.59 | 2.72 |
| `write_binned_instance_buffers` | 2.36 | 1.11 |
| `prepare_preprocess_bind_groups` | 1.52 | 0.80 |
| `Main::run_main` | 5.03 | **5.47** |

**The main world got slower and that is the trade**: it now runs the frustum test
it was skipping. Where the render thread dominates, as at `city`, buying 3.6 ms
there for 0.4 ms here is overwhelmingly worth it. Where it does not, the trade is
thinner — both orbit scenarios came back 0.5–0.9 ms slower on identical draw
counts, which is at the edge of the ~0.7 ms run-to-run spread this machine shows
and is the one number here worth re-running before trusting.

### The prepass was missing because it had been deleted

Answered while rebasing, not by measurement: `634be61 refactor(view): remove
ambient occlusion, and the prepass it was the only user of` landed on `main`
during this work. So "no prepass appears in any scenario's pass list" is simply
true — there is no prepass — and the 3.0×-overdraw finding below describes a
build that no longer exists.

Worth keeping the sequence straight, because the measurement was taken on a
branch that still *contained* the prepass code and still reported nothing for it.
That is not resolved by the deletion and remains the open half: either it was
already inert on that branch, or a pass can run here without
`RenderDiagnosticsPlugin` reporting it. The second would matter, since the whole
2026-09-22 investigation began from a 10 ms hole in the pass list.

### And the prepass was missing from the diagnostics

This note records, as a standing finding, that at `orbit` "the **depth prepass**
runs 3.0× overdraw while the opaque pass runs exactly 1.0×". **No prepass appears
in `RenderDiagnosticsPlugin`'s output at `orbit` or anywhere else now** — the pass
list is the same seven entries in every scenario, with no prepass and no shadow
pass among them. `perf::gpu_passes` does not filter; it reports what the store
holds. Whether the prepass was turned off since, or is running unrecorded, is not
established here — but the 3.0× figure cannot currently be reproduced, and
something that renders is going unmeasured either way.

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

### …and sharing the rest of that duplicate was measured and rejected (2026-09-14)

The obvious next step is to memoise the other two per-layer things the second pass
redoes: `geometry_bounds`, which walks every coordinate of every feature, and the
`streets` paint order, which is a filter plus a sort whose comparator calls
`street_style` and two flag probes — O(n log n) attribute lookups, rebuilt for the
casing pass and again for the fill pass. It was built (a `HashMap` per concern
keyed on `Layer::layer_index`, the sort decorated so the key is read once), it is
exactly value-preserving, and `culling_off_screen_features_changes_no_pixels` and
the allocation budgets both pass with it in.

**It buys nothing measurable.** Paired `bench_overzoom_cull` runs, release, this
container, per tile:

| factor 2, cull | run 1 | run 2 | run 3 |
|---|---|---|---|
| before | 44.66 ms | 40.84 ms | 38.53 ms |
| after | 36.96 ms | 42.56 ms | 40.46 ms |

The first pair looked like −17 % and was an ordering artefact — that run's *before*
is simply the slowest of the three befores. Runs 2 and 3 put the memoised version
marginally behind. The arithmetic agrees and is the part to keep: the duplicated
bounds scan is a linear pass over ~50k coordinates, order **50 µs**, against a tile
that costs 18–45 ms. It is a tenth of a percent of the thing it sits inside.

So the duplicate is real and it is **not where the time is** — the time is
tiny-skia's scan conversion, as `tile-raster-cost.md` has said throughout. Two
hash maps and a memo protocol in the hottest loop in the tree is not worth a
saving under the noise floor. Reverted. Re-open only with a profile that puts
`geometry_bounds` somewhere visible.

**The path building genuinely cannot be shared** either, which is worth stating so
it is not tried: the two passes over a layer build *different* paths from the same
features — a fill against an outline, a casing stroke against a fill stroke — and
the batching runs are keyed on colour and width, so there is nothing common to
keep.

## Open, with the reason they are still open

- **`osm_buildings` blocks io-pool threads with `std::thread::sleep`** on two
  paths inside `async fn`s — the retry backoff and `pace_fallback`, both inside
  `overpass_query`, both spanning attempts and endpoints within a single call so
  a dispatcher cannot express them. `async-io` is in the graph only transitively;
  making it a direct dependency, with the reactor thread that implies on Android,
  is a decision to take on its own.

  It was three paths per *module* until 2026-09-14, and those were a different
  thing: `osm_buildings`, `street_detail` and `tall_structures` each paced their
  own per-cell Overpass queries by sleeping 800 ms on the pool thread *after*
  their own request. The spacing is the dispatcher's job — `data_layers` had
  already moved it there — so each stream's `Policy` now carries
  `overpass_pace().1` and the sleeps are gone. Two things came with it. The
  spacing had to be `cfg`-ed away on wasm, which cannot block, so the web build
  had **no** pacing against a public instance at all; it has it now, from the same
  declaration. And the parking was never only its own stream's problem — a
  `IoTaskPool` thread held for 800 ms a cell is one every other fetch in the app
  queues behind.
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

## The space-tier cloud shell: measured at last, and it is the frame

Reported 2026-08-23 from the OnePlus 15: frame rate swings hard with altitude —
84 fps with the globe small on screen, 24 fps with it filling the screen — which
reads as a streaming or residency fault. **It is not one**, and the developer
panel is what settles it. At 7191 km, `space · factor 1.00`:

    28 fps · 35.6 ms   draws 122 · 1040 entities   triangles 7 978
    buildings 0 · 3DBAG 0 · map tiles 0 · globe 0
    upload 0 kB now · pipelines 45 ready, 0 building

122 draws and 8k triangles with nothing streaming and nothing compiling, at
35.6 ms. No streaming cap can account for a cost that survives that; it is
per-pixel. (The `worst` figures — 52 and 90 ms in the same session — are a
different question and *are* plausibly the upload path: 12.4 MB peak against a
4 MB/frame ceiling.)

The candidate is `shaders/globe_clouds.wesl`. Per fragment it runs **nine
6-octave `fbm3` fields** (~54 3D value-noise lookups) plus two domain warps and a
Coriolis curl, on a shell at planet radius — so from low orbit it covers every
pixel of a 2772×1272 native-scale HDR target.

**Two things made it unmeasurable, which is the part worth remembering.**
Coverage (`params.x`) is not consulted until line 204, after every `fbm3`, and
the only `discard` is at 242 — so there is no cheap value to set it to. And the
value it *is* set to is the `GLOBE_CLOUD_COVER` constant blended with live GFS,
deliberately not the weather slider (one met.no point should not overcast the
planet), so neither `--clouds 0` nor Settings ▸ World moved it. A first A/B duly
returned 303.8 ms against 293.2 ms and looked like an exoneration; it was the
flag never reaching the shader. Hence `CARTO_NO_CLOUDS` and the Settings ▸ World
checkbox, which hide both shells — the ground deck already dropped itself when it
carried no cloud, and the globe copy was missing that guard.

**What is still not established is whether the clouds are the cost**, and
lavapipe cannot establish it. Triplicate at 1000 km, `CARTO_QUALITY=android`,
480×1040, medians: baseline 261.6 ms, `CARTO_NO_CLOUDS` 223.0, `NO_ATMOSPHERE`
252.5 — but repeats of one config spanned 218–319 ms, and an earlier sweep had
`CARTO_NO_SHADOWS` landing 48 ms *slower* than baseline. The within-group spread
exceeds every between-group difference. A CPU rasteriser also flatters this test
in the wrong direction specifically: an ALU-heavy noise shader is far more
expensive relative to everything else on lavapipe than on an Adreno, so even a
clean number here would not transfer. This is the `frame_ms` row of the table at
the top of this note doing exactly what it says it does.

### Settled 2026-09-09 on real hardware

A Steam Deck (AMD Van Gogh, RADV, Mesa 26.2.2) was added as a second runner, and
it answers what lavapipe could not. `cargo perf --only orbit`, quiet steady
windows, `CARTO_NO_CLOUDS` against baseline:

| size | config | fps | GPU total | transparent pass | fragment invocations |
|---|---|---|---|---|---|
| 640×360 | base | 106.9 | 5.19 ms | **3.21 ms** | 231 039 |
| 640×360 | no clouds | 115.4 | 2.08 ms | 0.03 ms | 0 |
| 1280×720 | base | **52.2** | 13.22 ms | **9.39 ms** | 922 840 |
| 1280×720 | no clouds | **84.5** | 3.85 ms | 0.03 ms | 0 |

**The globe cloud shell is the transparent pass, and at 1280×720 it is 71 % of the
GPU frame and the difference between 52 and 85 fps.** The atmosphere and the haze
are noise beside it: `CARTO_NO_ATMOSPHERE` measured *+0.36* ms and `CARTO_NO_HAZE`
−0.10 ms, both inside the run-to-run spread.

Three things make this trustworthy where the 2026-08-23 attempt was not. The
windows report `quiet=true` — nothing streaming, no worker jobs, no mesh churn.
Fragment counts repeat to the unit across configurations. And the machine was idle
(load 0.1–2.4) rather than shared.

**It is fill-bound, not overdraw.** Overdraw on that pass is exactly **1.00×** —
one fragment per pixel, nothing wasted — and the cost scales with resolution
(3.11 → 9.37 ms for 4× the pixels, a 3.0× rise; invocations scale exactly 4×). So
the problem is not how much is drawn, it is what each fragment costs:
`globe_clouds.wesl` runs **nine 6-octave `fbm3` fields, i.e. 54 `vnoise3` calls and
432 hash lookups, per pixel**.

**Do not extrapolate the millisecond figure to an Adreno.** This shader is
dominated by 3-D value-noise lookups, which is exactly where GPU architectures
diverge most. What does transfer is the *shape*: fill-bound, 1.00× overdraw, and
scaling with pixel count — so a handset at ~2772×1272 pays it over ~3.8× the pixels
of the 1280×720 row above.

### And it was made ~50 % cheaper, with the floor measured

`globe_clouds.wesl` ran nine 6-octave `fbm3` fields per fragment. Six of them are
*warps* — their output is scaled by 2.2 and added to a sample coordinate, so their
top octaves displace the sample by a fraction of a noise cell. Those six, and the
wisp (whose own comment calls it "gentle, LARGE-scale… low frequency and shallow"),
lost octaves; the density and ridge crests that decide what you see kept all six.

54 `vnoise3` calls per fragment became 27. Measured end to end at 1280×720, both
windows `quiet`, on an idle machine:

| | before | after |
|---|---|---|
| cloud pass | 9.38 ms | **4.81 ms** (−48.7 %) |
| GPU frame | 13.22 ms | **8.14 ms** (−38.4 %) |
| frame rate | 54.6 fps | **69.5 fps** (+27.4 %) |

**Where the floor is, and why.** A ladder on the warp octaves, whole-Earth capture
with `CARTO_CLOUD_DATA_SYNC=1` so the frame carries real GFS cloud:

| warp octaves | cloud pass | difference from 3 |
|---|---|---|
| 3 | 5.90 ms | reference |
| 2 | 5.05 ms | mean 1.10/255 — clean |
| 1 | 4.02 ms | mean 2.77/255 — **visibly blocky** |

At one octave the warp is too coarse to decorrelate the noise lattice, and the
axis-aligned grid that `M3` exists to hide shows through as rectangular,
stair-stepped patches — the "hexagonal/blocky blobs" `M3`'s own comment describes.
So the last 1.0 ms is refused: **two octaves is the floor**, not three, and not one.

Two lessons for the next person here. A per-pixel budget is not one number — the
nine fields in this shader had wildly different value per octave, and cutting the
six that only displace a coordinate cost nothing visible. And a `--shot` on real
hardware is now part of the argument: colour is not evidence on lavapipe, so this
trade could not have been judged from this container at all.

The device settles it, with no rebuild once an APK carries the flag:

```
adb shell setprop debug.carto.no_clouds 1
adb shell am force-stop eu.garage44.cartopolis
```

One unrelated thing the same readout shows: **6,998 of 7,978 triangles (88%) go
into shadow cascades at 7191 km**, where no shadow can be resolved. Cheap in
absolute terms — 8k triangles — but it is three passes over invisible geometry
and nothing gates cascade submission on the tier.

### No frame-rate governor on the clouds (decided 2026-09-28)

From 2026-09-09 `systems::adaptive_quality` hid both cloud shells when the frame
interval stayed over budget for 4 s, and brought them back only after 20 s under
70 % of it. On the OnePlus 15 that meant **no clouds, ever**: the first seconds of
every launch are loading frames (43–48 ms on a Steam Deck at the phone's
resolution, where the settled scene then ran at ~15 ms), and the interval of a
device paced to 60 fps cannot fall below 16.7 ms, so the 11.7 ms needed to take
them back was unreachable. It also ran during scripted captures, so every slow
`--shot` photographed a cloudless sky whatever `--clouds` said. The user's
decision was that clouds are **on or off, and not dependent on fps**, so the
governor was removed rather than repaired — it governed nothing else. What a
cloudy frame costs on a handset is now visible as a frame rate, and the levers
for it are the cloud shaders themselves (`CLOUDS_LOW`, the octave floor above),
not a switch that takes the clouds away.

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

## Where the shadows go (2026-09-11)

Reported as "enabling shadows makes the viewer very slow". The *caster* side had
been tuned hard and repeatedly — the terrain relief does not cast, `lod22` casts
only from the camera's own cell (`SHADOW_RADIUS = 0`), the moon light is
despawned by day, the map is 1024² below the desktop rung — and three things had
never been looked at at all.

### `NoFrustumCulling` is also "submit me to every cascade"

This is the one worth carrying forward, because the component's name says nothing
about it. `bevy_light::check_dir_light_mesh_visibility` tests each caster against
each cascade frustum — *unless* the entity carries `NoFrustumCulling` or has no
`Aabb`, in which case it goes into every cascade of every light with no test.

Four things in this tree carried that component and no `NotShadowCaster`, and one
of them is the whole ground:

| entity | why it was unculled | what it cast |
|---|---|---|
| `StreamTile` (the clipmap) | flat quads + big_space rebasing | a ground plane, onto itself |
| the route ribbon | same | a line lying *in* the road surface |
| the driver arrow | same | a navigation mark |
| particle emitters | the quads are placed in the vertex shader, so the AABB is the wrong volume | nothing coherent — the shadow pass runs bevy's own vertex shader over a lattice whose vertices are all at the origin |

The tiles are the expensive one: the pyramid is a few hundred quads (the 424
figure in CLAUDE.md's `handset_guarded` table is the count of tile *rasters* held,
which is the same set), each covering its whole cascade, and *including the ones
behind the camera*. They now carry
`NotShadowCaster` from spawn. The comment that used to justify it on the relief
path — "the ground does not cast" — was right about relief and had quietly never
applied to the flat case, which is every tile most of the time.

### Measured

Lavapipe, 480×270, Groningen from 3 km at −30°/35°, `CARTO_QUALITY=balanced`,
noon, clear, dry, both runs settled. The only difference between them is the four
`NotShadowCaster` insertions; everything else is the same binary and the same
scene, which the identical draw and triangle totals confirm.

| | before | after |
|---|---|---|
| `draws` | 488 | 488 |
| `tris_total` | 1688 | 1688 |
| `tris_shadow` | 1368 | **0** |
| `shadow_casters` | 158 | **44** |

**114 of 158 casters, 72 %, were things that cannot cast a meaningful shadow** —
and every one of them was going into every cascade of every light with no
frustum test. At this viewpoint the route, the driver arrow and the particle
emitters are all absent, so the whole difference is the ground.

**`tris_shadow` alone is not enough, though it is not blind here either.** The
first draft of this section claimed the metric could not see the change at all,
on the reasoning that the clipmap is uploaded `RENDER_WORLD`-only and counts zero
triangles from the main world. The measurement says otherwise: it moved 1368 → 0,
because the relief and curvature grids `map_stream` builds *are* main-world
meshes. What the reasoning got right is the direction — 1688 counted triangles
for a 488-draw scene means most of the geometry (the 3DBAG shells, the globe)
contributes nothing to either column, so a caster can still be invisible to
`tris_shadow` while costing a pass. Read both. `RenderCounts::shadow_casters` is
the entity count, and it reaches the `shot state` line, `--csv`, `--dump-state`,
`--expect`, the dev panel and the shareable report.

### The filter was never set, so every tier took bevy's most expensive one

Everything above bounds what the shadow *pass* draws. Nothing bounded what the
*main* pass reads: `ShadowFilteringMethod` appeared nowhere in the tree, so every
rung took the `Gaussian` default — Castaño's nine taps approximating a 5×5
kernel, per shadowed fragment, per light. At a handset's 2772×1272 with the sun
and the moon both up that is the largest single shadow term in the frame, and no
amount of caster work touches it.

`Budget::shadow_filter` now answers it: `Gaussian` on the desktop rung,
`Hardware2x2` (one hardware PCF tap) on balanced and phone. Balanced is the rung
Android resolves to, and this is the purest example of that rung's stated brief —
a shadow's *placement* is its shape, and the filter decides only how soft the edge
is. `Temporal` is not reachable from a rung and should not become so: it is the
right answer *with* TAA, and `surface-finish-and-occlusion.md` records why this
codebase cannot run TAA.

### The moon was a second shadow-casting light, all night

Same illuminance argument `light_shafts` already makes about the same light and
settles the same way — "a second volumetric light doubles the march's shadow work
for something nobody would see" — except that a shaft is an optional effect and a
shadow map is not. `spawn_moon_light` sets `shadow_maps_enabled: false`
unconditionally now. Bevy answers a light with that off by clearing its cascade
visible-entity lists outright, so it is a saving in the caster pass and not only
in the fragment lookup.

Its `CascadeShadowConfig` stays, identical to the sun's: the panic in bevy_light
is about the two configs matching and does not care which of them is casting.

### The cascades reached past anything that casts

`shadow_distance_m` was 3 km (desktop) and 1.5 km (balanced) while the casters are
bounded by `lod22`'s one z14 cell, ~1.5 km across at this latitude, plus whatever
a low sun throws off it. The outer cascade covered a band where by construction
nothing casts, and was paid for twice — as a pass, and as a share of the texel
budget spent on ground nothing can darken. Now 1.5 km and 1 km, at the same
cascade counts, so every cascade gets about twice the texel density: cheaper and
sharper at once.

**This is also the only distance LOD the casters have, deliberately.** A
per-streamer shadow radius on the trees and the street furniture was drafted and
dropped: bevy already culls every caster against every cascade frustum, so a
second mechanism would be two answers to one question — and this repo's rule is
to remove the overlap rather than arbitrate it.

### Not done — and on a Deck it would buy nothing (qualified 2026-09-22)

**Nothing caches a shadow map.** The city is static and the sun moves 15°/hour,
and bevy re-renders every cascade every frame regardless. Re-rendering the outer
cascades only every N frames is the real structural win here and bevy 0.19 has no
API for it — it is engine work, not a setting.

**This was written from the software rasteriser and the hardware A/B contradicts
it.** `CARTO_NO_SHADOWS=1` on `deck-1` moves `city` by 13.73 → 13.94 ms, i.e. not
at all — so on that GPU the ceiling for perfectly caching every cascade is zero.
See the 2026-09-22 section near the top. The idea may still hold on a handset,
where fill is scarce and none of these figures were taken; it is no longer
evidence-backed on a desktop-class GPU, and "the big one" should not be repeated
until a device says so.

### What is still unmeasured

The caster counts above are measured. **No millisecond figure is**, and none
should be taken from this container: frame times here are a CPU rasteriser's and
the table at the top of this note says not to gate on them — an earlier sweep had
`CARTO_NO_SHADOWS` landing 48 ms *slower* than baseline. Nothing above should be
read as a frame-rate claim.

The filter, the moon and the cascade reach have no caster-count signature at all,
so they are untested by the A/B above; they rest on reading `bevy_light` and the
call sites. Splitting the shadow cost between the caster pass and the
per-fragment lookup needs `cargo perf --features perf` on the `deck-gpu` runner,
which is how the cloud shell was settled — and `CARTO_SHADOW_FILTER` exists so
that run needs no rebuild.

One practical note for whoever does it: five of six capture attempts on this
container were killed by the OOM killer with eight other sessions compiling. The
3 km viewpoint at 480×270 is what fits; a 100 m street view did not.
