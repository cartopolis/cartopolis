---
topic: performance-suite
triggers: ["cargo perf", perf.toml, "performance suite", "battery", "power", "hot",
  "slow", "profiling", "fragment shader invocations", "overdraw", "gpu time",
  RenderDiagnosticsPlugin, MeshAllocatorDiagnosticPlugin, SystemInformationDiagnosticsPlugin,
  "residency", "resident_kb", quiet, "steady window", "perf feature", "--repeat",
  median, spread]
updated: 2026-09-27
---

# Measuring what the client costs, and what a measurement here is worth

`cargo perf` flies the viewpoints in `docs/perf.toml`, settles and captures each
through the existing `--shot` harness, and measures two windows per scenario. The
report is JSON.

```bash
cargo perf                      # every scenario → /tmp/perf.json
cargo perf --only orbit         # a subset
cargo perf --features perf      # + the GPU passes and allocation columns
CARTO_QUALITY=phone cargo perf --only city   # the handset's own code branches
```

## Why this exists rather than another timer

`docs/notes/render-performance-survey.md` is the survey of where this client's
cost is; `crates/geo/src/perf.rs` is the allocation-budget answer for the tile
pipeline. Between them they say the thing this suite is built on: **every previous
attempt to measure this tree was a wall-clock timer and every one was reverted.** A
timer cannot survive a shared CI VM, and one loose enough not to flake is loose
enough to catch nothing. What survived, in `crates/geo`, was counting *work*.

The client had no equivalent. `ShotMetrics` describes a settled world in
exhaustive detail and carries one performance number, `frame_ms`, which the survey
note says must never be gated on.

## What comes from Bevy, and what had to be written

Almost everything comes from Bevy, through one `DiagnosticsStore`:

| plugin | what it answers |
|---|---|
| `FrameTimeDiagnosticsPlugin` | `fps`, `frame_time` |
| `SystemInformationDiagnosticsPlugin` | `process/cpu_usage`, `process/mem_usage` — **both percentages** |
| `EntityCountDiagnosticsPlugin` | `entity_count` |
| `MeshAllocatorDiagnosticPlugin` | slabs, slab bytes, mesh allocations |
| `RenderDiagnosticsPlugin` | per render pass: `elapsed_gpu`, `elapsed_cpu`, pipeline statistics |

An earlier draft hand-rolled a `/proc` reader for process CPU and memory and its
own frame-time statistics. Both were a second implementation of something the
engine maintains and both were deleted. What is genuinely ours is *when* to
measure, *what a scenario is*, and the residency split.

**None of these plugins asks anything new of the device.** `WgpuSettings` defaults
to `Functionality` priority, under which `initialize_renderer` already enables
every feature the adapter reports — so they cannot fail device creation, and on
hardware without the queries the columns come back absent rather than zero.

## The column that transfers

`fragment_shader_invocations` is a **count** of how many times the fragment shader
ran — fill and overdraw measured rather than timed. A count is the same arithmetic
on this container's software rasteriser as on an Adreno, which makes it the only
GPU-side number here that survives the trip to a handset. Timestamp queries and
pipeline statistics are Vulkan and DX12 only: that covers this container and
Android, and excludes Metal and the web.

Everything else on the GPU side is host-dependent. `elapsed_gpu` is a real GPU
timestamp on Vulkan and DX12 and **CPU time wearing a GPU label everywhere else**,
including here, where "the GPU" is llvmpipe. Structure yes, magnitude no.

### A point sample of the fill counter is not reproducible

Three runs of one unchanged configuration reported opaque counts of 160 836,
218 436 and 218 436 — two values differing by *exactly one full screen*. An A/B of
the sky shells run against single readings therefore measured that flip rather
than the shells, and produced a clean-looking table saying that turning the clouds
*off* increased fill. It was withdrawn.

So the window samples every frame and reports `frag_min`, `frag_max` and the
sample count. `frag_min != frag_max` means the scene has more than one steady
state, and that differencing two runs is meaningless until that is understood.

### And neither is a point sample of anything else: `--repeat N`

The same argument holds for every other column *across launches* — identical
builds have measured 25 % apart (below), and the render survey's within-group
spread was wider than every between-group difference it was after. So
`cargo perf --repeat N` flies each selected scenario N times in one launch and
the report carries no bare numbers: each `(scenario, phase)` window is one
`WindowSummary`, and every column in it is `{runs, min, median, max}`. The
individual flights stay in the `perf window` log lines; a `perf summary` line per
aggregated window gives the median and range of frame time and fps.

Decided, so nobody has to re-derive them (2026-09-27):

- **Round-robin, not blocked** (`a b a b`, not `a a b b`). The drift worth
  exposing is the shared box; blocked repeats put a slow patch inside one
  scenario's samples, where it reads as the scene.
- **An even count's median is the mean of the middle two** — the ordinary
  definition, so a median recomputed by hand from the log lines matches the JSON.
- **A column absent in some flights aggregates over the rest and says so**
  (`Stat::runs`); absent in all of them, it is absent. Never zero.
- `--repeat 1` is the default and reports `min == median == max`; its PNG keeps
  the name `perf-<scenario>.png`, while repeats write `perf-<scenario>-r<n>.png`.

**Caveat: a repeat's `load` window is not a cold one.** The tiles are already in
the process's memory cache and on disk, so repeats 2..N re-stream from warm and
`load` medians describe a re-teleport, not a cold start. `steady` is unaffected,
and is what the flag is for.

## The two windows, and why `quiet` exists

* **`load`** — teleport to settle verdict: the streaming burst.
* **`steady`** — frames after the shutter, with the world *meant* to be idle.

**A steady window's whole claim is that nothing is happening, and at ground level
that claim is often false.** The settle test can pass while the vector-tile
streamer is still trickling — `CLAUDE.md` says the same of `cargo shots`. Measured
on `city`: a "steady" window carrying 6 new meshes and ~10 MB of uploads *per
frame*. Read as an idle number that is simply wrong, and it is what made Bevy's
mesh allocator look like a defect when it was doing real work on real arriving
meshes.

`Window::quiet` therefore tests it — no uploads, no worker jobs, no mesh churn —
and a steady window that fails it warns. **`orbit` is the scenario that reliably
goes quiet**; `city` does not, on this container.

## Where the gigabyte is

Residency is what a driver refuses when it refuses an allocation, and it is the
number that matches this project's device-loss reports. It is reported split by
what it is made of, because "reduce residency" is not a change anyone can make
while the total is one number. City, 3 km over Groningen, settled:

| rung | resident | textures | geometry | Bevy mesh slabs | draws |
|---|---|---|---|---|---|
| desktop | 954 MB | 410 | 544 | 742 | 969 |
| phone | 370 MB | 78 | 292 | 392 | 250 |

The totals reproduce the 991 MB → 371 MB that `CLAUDE.md` records from an
independent measurement a year earlier, which is the best available evidence that
this instrument measures what it claims.

**Geometry is the majority, and on the handset rung it is nearly all of it.** The
guard cuts textures by 81 % (`tile_px` → 256) and geometry by only 46 %
(`radius_bias`). So 79 % of handset residency is geometry, and the texture lever is
nearly spent. Anything further has to come out of how many cells are held — which
is `radius_bias`, the one knob with no user control.

**And about a third of mesh memory is not mesh.** Bevy's allocator reports 742 MB
of slabs holding 544 MB on desktop, 392 holding 292 on the phone rung — ~35 % in
both. The cause is named: `SlabAllocatorSettings::growth_factor`, 1.5 by default.
That is ~100 MB on a handset. **It is not a free win**: lowering it buys the memory
back by reallocating more often, and a reallocation is a buffer copy, i.e. exactly
the upload burst `docs/notes/upload-bursts.md` records as having cost this project
a device. Worth measuring before it is worth doing.

## What it has found, and what it has withdrawn

Found, and standing:

* Residency is mostly geometry, and on a handset the texture knob is spent.
* ~35 % of mesh GPU memory is slab overhead.
* At `orbit`, quiet, 320×180: the **depth prepass** runs 3.0× overdraw while the
  opaque pass runs exactly 1.0× — which is a prepass working as intended, eating
  the overdraw so the expensive pass does not. Bloom is the largest single GPU
  cost in the frame in both windows.

Withdrawn, each caught by this suite rather than by review:

* A street-lamp scan reported as the sixth costliest system at 98.9 ms/frame. Under
  *processor* time it is under 0.09 ms — the 98.9 was the machine, not the work. An
  early-out was written for it and reverted; a fix with no measurable effect is a
  line to maintain bought with nothing.
* Bevy's mesh allocator at 21 % of the CPU frame "in a settled window". The window
  was not settled.
* The sky-shell A/B above.

Only one measured optimisation has come out of all this: deleting the road hum,
0.26 ms of processor per frame. That is the honest score.

## What this container cannot settle

Lavapipe is a CPU rasteriser: geometry, placement, culling and counts are
trustworthy, timings are not. The machine is also shared, and identical builds have
been measured 25 % apart. Prefer, in order: counts (fill, allocations, draws,
residency), then processor time on one quiet machine, then never wall clock.

**Unverified on a phone.** The suite has never run on Android — there is no CLI
there (`Cli::parse()` deliberately does not run), so reaching it needs the same
`debug.carto.*` property route the other switches use, and nobody has built that.
