---
topic: headless-shots-software-renderer
triggers: [shot_harness.rs, "--shot", "--expect", "cargo shots", lavapipe, llvmpipe, Msaa, headless, "no GPU", container, CI, ci.yml, "scene smoke", "capture never completed", software renderer, AsyncComputeTaskPool, quiesce, "black image", workers.rs, platform::workers, set_paused]
updated: 2026-10-17
---

# `--shot` on a software renderer

Measured 2026-08-04 in the `cartopolis-agent` container (Ubuntu 24.04, no
`/dev/dri`, no `DISPLAY`) against Mesa 25.2.8 **lavapipe** — the CPU Vulkan
driver. Nothing here is about a GPU workstation, where `cargo shots` behaves as
it always did.

## It is in the image now

`libvulkan1` + `mesa-vulkan-drivers` + `vulkan-tools` are installed in
**`cartopolis-ci:latest`** (`~/build/cartopolis-ci-image/Dockerfile` on the VPS), which
`cartopolis-agent` is built `FROM` — so every CI job and every agent container has a
Vulkan adapter with no setup at all. `WGPU_BACKEND=vulkan` is set in the image; the
build asserts `vulkaninfo --summary | grep -q llvmpipe`, so a broken driver stack
fails the image build instead of showing up later as a job that renders nothing.

`VK_ICD_FILENAMES` is deliberately **not** pinned. `mesa-vulkan-drivers` ships every
Mesa ICD and only lavapipe initialises on a host with no `/dev/dri`, so the loader
already picks it — pinning would force software rendering if the runner ever moved
to hardware.

The predecessor is worth remembering as a technique, since it needs no root:
`apt-get download` into a temp dir and `dpkg -x` into `~/vulkan-prefix`, because the
lavapipe ICD's `library_path` is *relative* and the loader will find the driver off
`LD_LIBRARY_PATH` without touching a system path:

    LD_LIBRARY_PATH=$PREFIX/usr/lib/x86_64-linux-gnu
    VK_ICD_FILENAMES=$PREFIX/usr/share/vulkan/icd.d/lvp_icd.json
    WGPU_BACKEND=vulkan

`vulkaninfo` then reports `llvmpipe (LLVM 20.1.2)`, `PHYSICAL_DEVICE_TYPE_CPU`,
Vulkan 1.4.318. What made it worth promoting into the image: 234 MB in a container's
writable layer is re-done by hand after every `docker compose up` that recreates the
container, and it is invisible to CI. The harness was already headless by
construction (no window, no winit, offscreen render target), so nothing else had to
change.

## What it costs

Both at 640×360, `debug` profile, `CARGO_BUILD_JOBS=6`:

| view | wall clock | settle |
|---|---|---|
| 180 km orbit, straight down | **22.7 s** | settled in 6.7 s, 250 ms/frame |
| 400 m oblique over a city | **~5 min** | *never settles* — see below |

Orbit-and-above is cheap enough to run per-branch. Ground level is triage, not a
gate.

**It is a gate now** (2026-08-06). `build-test` in `.forgejo/workflows/ci.yml`
ends with an orbit capture asserting `settled==true` and `globe_rendered>0` via
the harness's `--expect` flag, which exits non-zero when a metric does not hold.
It is a step in that job rather than a job of its own so it reuses the dependency
graph the tests just compiled — a separate job pays a second cold Bevy build
against its own cache, and the runner's capacity is 2. This is the only check in
CI that renders anything: everything else proves the code compiles and its units
behave, which a black screen does too.

What it may assert is bounded by the section below — geometry, placement, culling
and visibility, never colour or lighting. Ground level is still not a gate, and
raising `--wait` does not change that; it fails slower.

## What it cannot tell you

llvmpipe is a different rasteriser. Geometry, placement, culling, visibility and
UI layout are the same picture as a GPU's; **the tone-mapped sky is not**. A
colour, exposure, bloom or lighting judgement made against a software capture is
not evidence — `ShotMetrics::software_renderer` is in every sink so a shot says
which it was. The sun/atmosphere/albedo tuning still belongs on a workstation.

It also cannot see the `VK_ERROR_DEVICE_LOST` class at all: those are Intel iGPU
command-buffer timeouts, and a CPU rasteriser has no such failure mode. The
per-frame streaming caps stay a rule enforced by reading, not by testing.

## Two captures of one viewpoint are not the same picture (2026-09-21)

**The camera is placed relative to measured ground, and the measurement settles
differently each run**, so two `--shot` runs at one `--at` can differ by a few
pixels of camera height. Over a city at a grazing angle that is a *large* per-pixel
difference: measured on the Deck, two runs of the same viewpoint with a shading
term switched between them differed by a mean of 26.5/255 over the near field, and
rolling one image down **four pixels** took that to 6.95. The shading term under
test was worth a small fraction of either number.

So an image-difference A/B of a shading term is, by default, a measurement of the
camera. Three ways out, in order of preference:

* **Visualise the term itself.** A temporary `return` in the fragment shader that
  paints the weight — not the shaded result — answers "where does this apply and
  how strongly" from *one* capture, with no pair to align. This is what settled
  the clipmap cross-fade after two wrong readings of a difference image.
* **Assert a number, not a picture** — the `--dump-state`/`--expect` route, which
  is why every layer here has a countable field beside its visible one.
* **Align before differencing**, and report the offset you removed. A pair whose
  best alignment is not (0, 0) is a pair that was not the same viewpoint, and the
  residual is the most that can honestly be attributed to the change.

The cheap tell is to difference two captures that had the *same* settings: if that
"noise floor" is not near zero, nothing in the real comparison is readable. Ours
was not — and the run used as the reference was the cache-filling one, which makes
it worse, not better.

## The shutter and the map share one thread pool

**Fixed 2026-08-06.** This was the note's open bug — a capture firing below the
`--wait` cap at ground level, the world rendering *healthily* for 194 further
frames, and `ScreenshotCaptured` never running. The cause is not the renderer at
all, which is why it looked so much like one:

* `platform::workers::submit` **was** `AsyncComputeTaskPool` on native (until
  2026-08-14 — see below), and our jobs
  are blocking closures — a full MVT rasterise, ~120 ms each
  ([`tile-raster-cost.md`](tile-raster-cost.md)) — so each owns a pool thread
  outright with no yield point.
* `bevy_render`'s `collect_screenshots` spawns the readback future onto **the same
  pool** and detaches it.

A teleport queues hundreds of jobs; the pool is 3 threads wide on an 8-core host
(`min(12, 8 × 0.45)`); the streamers keep refilling it. The readback future is
FIFO'd behind all of them and may never be polled. Nothing logs, because nothing is
wrong — the frames really are healthy, and the queue really is being served.

The fix is [`workers::set_paused`]: once the settle test has returned its verdict
the harness stops admitting jobs, waits for `workers::outstanding()` to reach zero
(capped by `QUIESCE_MAX_S`), and only then fires. Everything the streamers would do
after the verdict is invisible in that frame's image anyway. Pausing **parks** jobs
rather than dropping them — the streamers de-duplicate on "a fetch is in flight", so
a discarded decode is never re-queued and the tile would be missing for every later
shot in a `--shots` run.

**The tile jobs have had their own pool since 2026-08-14** ("carto tile worker",
`platform::workers`' module header). The same starvation turned out to hit
everything else on `AsyncComputeTaskPool`, and the worst victim was not the
shutter: Bevy spawns **pipeline compilation** there on Android
(`create_pipeline_task`), so after launch and after every teleport each shader
compile waited behind the rasterise backlog and the geometry that needed it was
silently skipped by every pass — a half-drawn, stuttering map rather than a stall.
The general rule is the one that forced the split: **blocking CPU work goes
through `platform::workers`, never onto `AsyncComputeTaskPool`**, which Bevy uses
for its own readbacks and compiles. `set_paused` stayed, for a reason the split
does not touch: the harness has to know our work is *finished*, not merely running
elsewhere, because a decode landing between the settle verdict and the shutter
changes the picture.

**`set_paused` is no longer the harness's alone** (2026-08-10). Android's
`quiesce_while_backgrounded` pauses the pool on `AppLifecycle::WillSuspend` and
un-pauses it on `WillResume | Running`, for battery. That is the right behaviour
on a phone and it makes the flag a *shared* one: any writer that sets `false`
while the harness is quiescing re-admits the streamers and hands back exactly the
"black scene with only the chrome, exit 0" above. Whether Bevy emits a `Running`
lifecycle message on the headless desktop path — which would trip this on every
CI capture — is **not verified here**; it needs a machine that can render. If a
third writer is ever added, the flag has to become owned rather than global.

### The failure has a second face, and it is worse

Reproduced on a GPU workstation by squeezing the pool to one thread (the mechanism
is starvation, not software rasterisation — so it does **not** need lavapipe). At
`--wait 5` over Groningen at 400 m:

| | image | `shot state` |
|---|---|---|
| before | **black scene, chrome only** — 22 KB | `settled=false busy="surfaces\|lod22"`, exit 0 |
| after | the city — 400 KB | `settled=false busy="surfaces"`, exit 0 |

So the pool starvation does not always withhold the PNG; sometimes it writes a
*wrong* one and reports success, which is strictly harder to catch. At 400 m the
ground is vector `surfaces` geometry, not raster imagery, and that geometry was
still sitting on the worker queue. Cost of the fix in that run: 3.3 s.

`--shot` retries the shutter **once** if the grace expires, which also covers an
independent trap: a `Screenshot` gets exactly one frame. `extract_screenshots`
inserts `Capturing` and its own query is `Without<Capturing>`, while
`prepare_screenshots` rebuilds its prepared set every frame — so if that single
frame's prepare skips the entity (render-target image not in `RenderAssets`,
pipeline not yet compiled) the request is dropped silently and no later frame
reconsiders it.

## `--wait` and `--settle` are virtual seconds, and the clock is clamped

Not fixed, and worth knowing before you reach for a bigger number. Both are
accumulated from `Res<Time>`'s per-frame delta, and Bevy clamps
`Time<Virtual>::max_delta` to **0.25 s**. So a run whose frames take 1.6 s advances
the harness clock at ~1/6 wall-clock speed: measured on the one-thread A/B above,
`settled_in="5.0s"` took **33 s** of real time. On a GPU workstation frames are
under 0.25 s and the two coincide exactly, which is why the flags read as honest
there.

Consequences: `--wait 45` in `docs/shots.toml` is up to several minutes on a
software renderer (this is what "~5 min, never settles" in the table above actually
was), and the `--shot` help calling `--wait` a "hard cap … in seconds" is wrong off
a fast machine. Making the harness use real time would be a small change and would
make the flags mean what they say — but it would also make every CI capture *less*
complete for the same flag values, so it is a deliberate trade rather than an
oversight.

## Two things that made the first failure unreadable

Both fixed, both worth remembering as a shape.

`Msaa::Sample2` is invalid on lavapipe (it allows 1 and 4, exactly as WebGPU
does). The failure is silent in the way `free_camera.rs` already documents for
the web: nothing renders, then a flood of `CommandQueue has un-applied commands`
naming only the symptom. `--shot` exited **0** in 3.5 s with no PNG.

It was hard to find because the fallback log filter began with `off`, which
silenced `bevy_render::error_handler` — the component that reports a validation
failure *and* writes the `AppExit` ending the run. The wgpu targets were at
`warn` and said nothing, because the error never came from them.

The general lesson, since the harness exists to be read by something that cannot
look at the screen: **a run that dies for a render reason has to say so, and has
to exit non-zero.** `app.run()`'s `AppExit` was being dropped, so every failure
— including `capture never completed` — reported success to the shell. Anything
built on top (a CI job, an agent checking its own work) would have been green
forever.
