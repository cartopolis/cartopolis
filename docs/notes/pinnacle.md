---
topic: pinnacle
triggers: ["pinnacle", target_os = "pinnacle", aarch64-unknown-pinnacle, crates/cartopolis/Cargo.toml, platform/photo.rs, platform/model_file.rs, platform/gnss_receiver.rs, utils/clipboard.rs, face_tracking.rs, log_ring.rs, blake3, ring, rustls]
updated: 2026-11-24
---

# Cartopolis on Pinnacle

Pinnacle (jeroen/pinnacle) is the owner's own operating system, written in
Rust from the kernel up, with **no C anywhere**. The aim is that this client
runs on it. Rust knows it as `aarch64-unknown-pinnacle`, `target_os =
"pinnacle"`, with a std of Pinnacle's own (`std/` in that repo).

## How the client is built for it

Not from this repo: Pinnacle's repo holds the crates from crates.io that
needed a Pinnacle branch (`ports/` there: getrandom, mio, tokio, winit, ring,
bevy_asset and more), and switches them in for one build with `--config
patch.crates-io…`. This repo's manifest stays as it is everywhere else. As of
2026-11-24 the whole client `cargo check`s for Pinnacle; it does not yet run,
because Pinnacle has no GPU API for wgpu (Vulkan via QEMU's Venus is the
chosen route).

## What this repo does for it

Pinnacle is a **fourth kind of platform**, beside desktop, web and Android,
and it answers "native" to `not(wasm32)` and "not Android" to
`not(target_os = "android")` — the trap `android-build.md` describes, one
level further. So a desktop-only crate is gated
`all(not(wasm32), not(android), not(target_os = "pinnacle"))`, and each
per-platform `mod imp` chain has a Pinnacle arm:

| What | On Pinnacle | Why |
|---|---|---|
| File dialog (`rfd`), photo picker | Hidden (`AVAILABLE = false`) | No file dialog, no camera driver yet |
| Clipboard (`arboard`) | Within the app only | The display server's shared clipboard is not wired to winit yet |
| GNSS receiver over a serial port | No ports | No serial ports for programs yet |
| Face tracking (`cartopolis_face`) | Never built; a picture fed is let go | `tract`'s matrix kernels are C and assembly |
| `blake3` (Bevy's asset hashes) | Its `pure` feature | Its default build compiles C and assembly |
| rustls' crypto | `ring`, as Pinnacle's port has it | ring's own is C and assembly; the port is Rust on Pinnacle's cryptography and refuses signing keys (a client needs none) and P-256/P-384 key agreement (X25519 is offered first) |

`log_ring`'s reader and the face tap's fields are unused there (the MCP server
is Unix-only), hence their `allow(dead_code)` on Pinnacle.

## Checking a change against it

From the Pinnacle repo, with its std and ports: `cargo check` of this
workspace for `std/aarch64-unknown-pinnacle.json` with `-Zbuild-std` and the
ports' patches (Pinnacle's `xtask` has the flags). Nothing in this repo's gate
checks it yet, so a desktop-only crate added under plain `not(wasm32)` breaks
Pinnacle silently, as it once broke Android.
