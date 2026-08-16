---
topic: render-performance-survey
triggers: [ShotMetrics, render_census, "draw call", "overdraw", "clipmap", "SIMD", big_space, "frame time", "profiling", "perf", "framerate", "fps", occluded_by_finer, "tier", quality.rs]
updated: 2026-08-16
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
for ~10–30×. The same is true of `collision::blocked`, which still has no spatial
index despite CLAUDE.md calling it one: a uniform grid is ~100× where SIMD offers
4–8×.

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

### The unused MVT layers are not decoded

`docs/notes/vector-tile-unused-layers.md` says the geometry cell "decodes all of
it". That stopped being true when `TileData::features` became lazy per layer:
`place_labels` and `street_labels_points` are never decoded at all, and every
"loop over all layers" site filters before touching `features()`. The note's
conclusions hold; its cost model does not.

The real duplicate is `PAINT_ORDER` listing `buildings` and `streets` **twice**.
The decode is memoised; the path building and the bounds scan are not.

## Open, with the reason they are still open

- **The overzoom fan-out parses one z14 body up to 64 times.**
  `TilePayload::decode` parses per sub-tile, and `map_geometry`, both `pois`
  layer-defs, the globe and routing each parse the same body again — on the order
  of 65–70 parses per anchor, each a full `bytes.to_vec()` plus a fresh prost
  decode. This is the largest single number in the pipeline.
  **Blocked on a type change:** `TileData` holds `std::cell::OnceCell`, which is
  not `Sync`, so it cannot be shared across worker threads at all until that
  becomes a `OnceLock`. A weak `HashMap<TileKey, Weak<TileData>>` beside the byte
  cache is the shape; it must stay *weak*, or a globe fill pins hundreds of
  12.5 MiB readers (the size `docs/notes/allocation-budgets.md` measured, which is
  the number `tile-raster-cost.md` said this was blocked on).
- **`osm_buildings` blocks io-pool threads with `std::thread::sleep`** on three
  paths inside `async fn`s. `async-io` is in the graph only transitively; making
  it a direct dependency, with the reactor thread that implies on Android, is a
  decision to take on its own.
- **Android frame pacing.** `fps_cap: None` leaves pacing to FIFO, which paces to
  *the panel* — so a 90/120 Hz handset renders 2× the frames a 30-tuned scene
  needs and is then thermally throttled below what a cap would have held. The ADPF
  deadline in `report_frame_to_scheduler` is hardcoded to 16.6 ms for the same
  reason. `ANativeWindow_setFrameRate` is reachable through the `dlopen`/`dlsym`
  machinery `platform::android::perf_hint` already has.
- **egui runs at `pixels_per_point` ≈ 4.1 on a phone** (2.75 display scale ×
  1.5 `ui_scale`) while the 3D world runs at 0.5, and it is a *second*
  full-resolution pass over the swapchain with `LoadOp::Load` — a full tile-memory
  load and store of the 1080×2340 output, every frame. Clamping ppp is contained;
  merging the pass into `upscaling` is the larger win and the riskier change.
  **Do not touch `ui_scale`** — every dp-based layout assumption reads it.
- **Nothing prewarms the pipeline cache**, and `StandardMaterial::depth_bias` is
  part of the specialization key, so each of the ~25 depth-bias rungs is a
  distinct shader compile. Adreno compiles run 50–300 ms apiece and geometry whose
  pipeline is not resident is *silently skipped*. Costs time-to-a-complete-picture
  after launch and after every teleport, not steady-state frames — and `--shot`
  waits for settle, so the harness hides exactly this.

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

## The engine constraint behind the draw-count work

`bevy_render`'s `gpu_preprocessing` excludes **Adreno ≤ 730 (except 720) and Mali
drivers below 48** from GPU preprocessing entirely — `GpuPreprocessingMode::None`.
The Fairphone 4's Adreno 619 is in that bucket. So on the target device there is
no indirect draw, no GPU culling and no cross-mesh batching: every mesh is a
CPU-issued draw with its own bind group. That is why `draws` is a first-class
metric here rather than a proxy for triangles, and why it is a main-thread number
as much as a GPU one.
