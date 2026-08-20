---
topic: device-loss-strands-the-swapchain
triggers: [DeviceLost, VK_ERROR_DEVICE_LOST, handle_render_error, abandon_lost_surface, exit_on_halt, StopRendering, SwapchainAcquireSemaphore, SurfaceTexture, release_resources, discard_texture, check_is_valid, WindowSurfaces, SIGABRT, tombstone, wgpu upgrade, "still in use by a SurfaceTexture"]
updated: 2026-08-20
---

# After a device loss nobody can release the acquired swapchain image

An **external constraint**, not a bug of ours, and three attempts were spent
finding that out. On wgpu 29 (and on trunk past 30.0.0) a `VK_ERROR_DEVICE_LOST`
permanently strands the swapchain image that was acquired for the frame it
happened in. Anything that destroys that swapchain afterwards aborts the process.

```text
// wgpu-core-29.0.4/src/present.rs — Surface::discard()
let device = &present.device;
device.check_is_valid()?;                      // Err(DeviceError::Lost)
let texture = present.acquired_texture.take()… // never reached
```

Dropping a `SurfaceTexture` that was never presented calls `discard`. `discard`
refuses to run on a lost device, so the raw texture — and the
`Arc<Mutex<SwapchainAcquireSemaphore>>` clone it carries — stays parked in
`present.acquired_texture` for the life of the process. The teardown then hits

```text
Arc::into_inner(semaphore)
    .expect("Trying to destroy a SwapchainAcquireSemaphore that is still in use by a SurfaceTexture")
```

in `wgpu-hal`'s `NativeSwapchain::release_resources`. On Android that is a SIGABRT
the instant the app is backgrounded — `extract_windows` drops the surface when
`bevy_winit` takes the window's `RawHandleWrapper` away.

Two details close off the obvious escapes. `NativeSwapchain::discard_texture` is
still `// TODO: Current implementation no-ops`, and the `check_is_valid` gate is
**still present on wgpu trunk after 30.0.0** — so this is not a version to upgrade
out of, and `bevy_render 0.19` pins `wgpu = "29.0.3"` anyway. wgpu 30's changelog
fixes only the sibling case (a `SurfaceTexture` dropped during *panic unwind*).

## What lost, and why

- **`RenderErrorPolicy::Recover`** (2026-08-17): built a second wgpu instance and
  asked for a surface on an `ANativeWindow` the old one still held —
  `ERROR_NATIVE_WINDOW_IN_USE_KHR`, abort.
- **Dropping the old surface first** (2026-08-18): tripped the semaphore assert
  one layer down. Abort.
- **`release_halted_frame`** (2026-08-19): cleared every holder *bevy* has —
  `ExtractedWindows::swap_chain_texture`, the `ViewTarget`s,
  `ViewTargetAttachments`. Reasoned correctly about the render world and shipped;
  the device aborted with the same message from a build containing it. The holder
  was never ours, and no amount of clearing on our side can reach it.

The surviving move is not to release anything: **leak `WindowSurfaces`** so no
`wgpu::Surface` is ever dropped and `release_resources` is never reached
(`abandon_lost_surface`). Holding a few Vulkan objects that can no longer be
destroyed, in a process that is halted and about to end, costs nothing.

## The other way a halted process dies

A lost device also makes the next `Buffer::get_mapped_range` a validation failure
in every render system, and some of those run on `AsyncComputeTaskPool` threads
that no error handler can reach. Two of the four tombstones from the night of
2026-08-19 are that abort rather than the swapchain one. It cannot be prevented
from here, only outrun — which is why Android now leaves promptly
(`exit_on_halt`) instead of holding a frozen screen and waiting to see which
abort arrives first.

## The number the guards were not watching

Groningen at 150 m, `--shot-mode earth`, 900×1900, 2026-08-20, from
`--dump-state`'s new `vram_allocated_kb` / `vram_reserved_kb`
(`wgpu::Device::generate_allocator_report`):

| rung | our asset count | wgpu in use | reserved from the driver |
|---|---|---|---|
| `android` | 364 MB | 483 MB | **664 MB** |
| `desktop` | 736 MB | 996 MB | 1228 MB |

`gpu_resident_kb` counts asset bytes this client creates. It is machine-independent
and that is why it is the gate — but it is **not what a driver refuses on**. Between
it and the driver sit the render targets, shadow maps, atmosphere LUTs and pipelines
it never counted, and then the allocator's own slack. On the handset rung those add
up to 1.8×: the memory-guard series took `resident` from 991 MB to 371 MB and read
that as the ask, while the ask was closer to 660.

The 1.37× slack is not the catastrophic over-reservation
[bevyengine/bevy#24722] describes, so on this evidence the allocator regression is
a term rather than the whole story. **Measured on lavapipe**, whose render targets
and allocator behaviour are its own; the ratio is indicative and the phone's own
figure now arrives with every halt report (`gpu_memory_census`).

## Upstream

[bevyengine/bevy#24722](https://github.com/bevyengine/bevy/issues/24722) reports
this exact panic on *desktop* Vulkan, alongside OOM and device-lost errors,
bisected to wgpu 28's switch from `gpu-alloc` to `gpu-allocator`; DX12 and Metal
are unaffected and `WGPU_BACKEND=dx12` is the reporter's workaround. Relaxing the
`check_is_valid` gate looks like a small upstream fix and nothing here can test
it. Worth watching before writing more guards on this side: if that allocator
regression is a real term in *our* device losses, the streaming budgets are not
where the answer is.

Related: `allocation-budgets.md` (what the client asks the GPU for),
`android-build.md` (the halt policy and the crash reporter).

**Nothing on this page was measured here.** No harness in this container loses a
Vulkan device — lavapipe does not, and the VPS emulator is SwiftShader. The
mechanism is quoted from the wgpu sources in the cargo registry; the outcome needs
a phone, and the test is a tombstone that no longer arrives.
