---
topic: headless-shots-software-renderer
triggers: [shot_harness.rs, "--shot", "cargo shots", lavapipe, llvmpipe, Msaa, headless, "no GPU", container, CI, "capture never completed", software renderer]
updated: 2026-08-04
---

# `--shot` on a software renderer

Measured 2026-08-04 in the `viberfox-agent` container (Ubuntu 24.04, no
`/dev/dri`, no `DISPLAY`) against Mesa 25.2.8 **lavapipe** — the CPU Vulkan
driver. Nothing here is about a GPU workstation, where `cargo shots` behaves as
it always did.

## It works, and it needs no root

`libvulkan1` + `mesa-vulkan-drivers` + deps come to 14 packages / 234 MB. With no
root, `apt-get download` into a temp dir and `dpkg -x` into `~/vulkan-prefix` is
enough — the lavapipe ICD's `library_path` is *relative*, so the loader finds the
driver from `LD_LIBRARY_PATH` and no system path is touched:

    LD_LIBRARY_PATH=$PREFIX/usr/lib/x86_64-linux-gnu
    VK_ICD_FILENAMES=$PREFIX/usr/share/vulkan/icd.d/lvp_icd.json
    WGPU_BACKEND=vulkan

`vulkaninfo` then reports `llvmpipe (LLVM 20.1.2)`, `PHYSICAL_DEVICE_TYPE_CPU`,
Vulkan 1.4.318. The harness was already headless by construction (no window, no
winit, offscreen render target), so nothing else had to change.

## What it costs

Both at 640×360, `debug` profile, `CARGO_BUILD_JOBS=6`:

| view | wall clock | settle |
|---|---|---|
| 180 km orbit, straight down | **22.7 s** | settled in 6.7 s, 250 ms/frame |
| 400 m oblique over a city | **~5 min** | *never settles* — see below |

Orbit-and-above is cheap enough to run per-branch. Ground level is triage, not a
gate.

## What it cannot tell you

llvmpipe is a different rasteriser. Geometry, placement, culling, visibility and
UI layout are the same picture as a GPU's; **the tone-mapped sky is not**. A
colour, exposure, bloom or lighting judgement made against a software capture is
not evidence — `ShotMetrics::software_renderer` is in every sink so a shot says
which it was. The sun/atmosphere/albedo tuning still belongs on a workstation.

It also cannot see the `VK_ERROR_DEVICE_LOST` class at all: those are Intel iGPU
command-buffer timeouts, and a CPU rasteriser has no such failure mode. The
per-frame streaming caps stay a rule enforced by reading, not by testing.

## A capture that fires while the map is still streaming never completes

**Open, not fixed.** Below the `--wait` cap at ground level the capture fires,
the world keeps rendering *healthily* — 194 further frames at ~400 ms, 591 draws,
206 meshes, 433 images, no validation errors — and the `ScreenshotCaptured`
observer never runs. Reproduced at `--wait 5` and `--wait 30`; both die on the
60 s readback grace with no PNG. The same viewpoint at `--wait 120` *did* save,
which fits the readback starving only while texture uploads are continuous
(`awaiting_upload` was 81 when it hung, ~0 when it worked).

Consequence, and the reason this is worth knowing before trusting a capture:
**on a software renderer you can only reliably capture a scene that settled.**
The `--wait` timeout path — the one that exists to photograph a half-built world
— is the path that doesn't produce an image. Whether this reproduces on a real
GPU is untested.

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
