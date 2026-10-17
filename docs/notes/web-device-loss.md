---
topic: web-device-loss
triggers: [DeviceLost, wasm, webgpu, "blue screen", "device lost", reload, sessionStorage,
           "GPU process", dawn, "gpu_preprocessing", "halt card", render_halt.rs,
           carto-halt-last, remember_halt, render_setup.rs, cartopolis/index.html,
           halt_action, HALT_RELOAD_MAX]
updated: 2026-10-17
---

# On the web a lost device is weather, and the client reloads instead of halting

The deployed web client on a OnePlus 15 (Adreno 840, Android Chrome) died within
about a minute of boot on 2026-08-23: `DeviceLost` →
`handle_render_error` → `StopRendering`, which on the web meant a permanently
blue page — the loader had already removed itself, the halt is terminal, and the
explanation was in a console a phone cannot open. The boot log, read off the
device, showed an upload-burst warning immediately before the loss.

## What the message means

`A valid external Instance reference no longer exists.` is **Dawn's own text for
the browser tearing the device down** — GPU process crash or restart, instance
dropped — not a translated `VK_ERROR_DEVICE_LOST` from our workload. Chrome
restarts its GPU process under memory pressure, on driver resets and on plain
crashes (Android Adreno drivers being a chronic source), and both the WebGPU
spec and Chrome's guidance say the application's job is to expect `device.lost`
and get a new device. Bevy cannot re-request a device mid-run; the page-level
equivalent is a reload, and that is what the client now does.

## The policy (`render_halt.rs::exit_on_halt`, wasm arm)

The native halt policy's whole rationale is Vulkan: a stranded swapchain
semaphore, pool-thread aborts, an `ANativeWindow` mid-teardown. None of it
exists on the webgpu backend (no wgpu-core underneath), so the web keeps nothing
by staying down. A halt therefore **reloads the page**, with three deliberate
edges:

- **Capped at `HALT_RELOAD_MAX` (2) per streak**, counted in `sessionStorage` —
  a loss that recurs deterministically at startup must not reload forever, and
  each retry costs the full module download. The spent streak shows a plain DOM
  card (`halt_card`) whose retry is an `<a href="">`, so it works after the wasm
  runtime is dead.
- **`HALT_STREAK_RESET_S` (120 s) of uptime makes the next loss a fresh streak**,
  or two losses an hour apart would eventually strand a long session on the card.
- **No storage → no reload at all**: an uncountable loop is never started.

The boot recorder in `index.html` reads the counter back and prints
`recovering from a lost graphics device (reload N)`, so a recurring loss looks
like what it is rather than a loader that runs twice. The decision is a pure
function with native tests (`halt_action_tests`) — the wasm lane compiles but
never executes, so policy gated behind `#[cfg(wasm)]` would be policy nothing
tests.

## Leads chased and closed (2026-08-23)

- **The draw path is not the native/web difference.** The plausible story —
  native Android caps Bevy to `PreprocessingOnly`
  (`GpuPreprocessingOverridePlugin`) while the web build gets the full
  GPU-culling path on the same GPU — is false: wgpu 29's webgpu backend maps
  **no `IMMEDIATES`** feature (`backend/webgpu.rs`, `FEATURES_MAPPING`), bevy's
  `Culling` mode requires it, so every browser lands on `PreprocessingOnly`
  anyway ("Some GPU preprocessing are limited on this device"). Also structural
  and worth knowing: bevy's Android GPU denylist parses `AdapterInfo.name`,
  which is **empty** on `BrowserWebGpu` — upstream device heuristics can never
  fire on the web.
- **The upload ceiling is live on the web** (phone rung, 4096 kB/frame), so the
  burst guard was not missing there either.
- What actually took the device on the phone is therefore not identifiable from
  the page — one string is all a page gets — which is exactly why the fix is
  resilience rather than a workload tweak.

The container's browser harness **cannot reproduce or diagnose any device
loss**: headless-Chromium-on-SwiftShader loses the device on any page that
presents to a canvas, twenty-line clear-and-submit pages included
([checking-the-web-build.md](checking-the-web-build.md)). What it *can* verify
is this policy's plumbing: its guaranteed loss exercises the reload → counter →
card path end to end.

## The card names its cause (2026-10-17)

The card used to say "lost its graphics device" for **every** halt — a
validation failure or an out-of-memory refusal as much as a real
`device.lost` — and the browser's own text was in a console two reloads had
already cleared. So a report of the card was one bit. `remember_halt` keeps
`"{kind}: {description}"` in sessionStorage (`carto-halt-last`) across the
reloads, the card prints it, and `index.html`'s recovery line repeats it.
**Ask for that line before reasoning about a reported loss.**

The report that prompted it — a Mac, "crashes when flying higher", the card
after both reloads — came before the line existed, so its cause is not known.
What the climb was holding is in
[near-ground-layers-at-altitude.md](near-ground-layers-at-altitude.md).

