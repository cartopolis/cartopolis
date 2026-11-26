---
topic: pinnacle
triggers: ["pinnacle", pinnacle-store.sh, target_os = "pinnacle", aarch64-unknown-pinnacle, crates/cartopolis/Cargo.toml, platform/photo.rs, platform/model_file.rs, platform/gnss_receiver.rs, utils/clipboard.rs, face_tracking.rs, log_ring.rs, blake3, ring, rustls]
updated: 2026-11-26
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
patch.crates-io…`. This repo's manifest stays as it is everywhere else.
`cargo xtask cartopolis <checkout of this repo>` there builds the client
(release) for Pinnacle; it leaves `wgpu` to this repo's own copy
(`crates/wgpu`, whose build script turns the Vulkan backend on for
Pinnacle too: its Vulkan is Pinnacle's own driver, through QEMU's Venus).

As of 2026-11-25 the client **builds and links** for Pinnacle (20 minutes
on a Mac node, cold): a program of 141 MiB, 106 MiB of it loaded (86 MiB
code, 20 MiB read-only data), no debug info. It does not run yet: Pinnacle's
kernel loads a program whole into one allocation (26 MiB already failed) and
its QEMU guest has 256 MiB of memory. wgpu itself runs there (a window drawn
by the host's GPU: Pinnacle's `cargo xtask wgpu-demo`).

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

From the Pinnacle repo, with its std and ports: `cargo xtask cartopolis` (a
build), or `cargo check` of this
workspace for `std/aarch64-unknown-pinnacle.json` with `-Zbuild-std` and the
ports' patches (Pinnacle's `xtask` has the flags). Nothing in this repo's gate
checks it yet, so a desktop-only crate added under plain `not(wasm32)` breaks
Pinnacle silently, as it once broke Android.

## Where its files are

Pinnacle installs the client as an app, `system:/apps/cartopolis/program`
with its `assets/` beside it, and an app may write only in its own folder,
`app:` (the kernel keeps that in the person's data). So on Pinnacle
`ASSET_ROOT` is the relative `assets`, which Bevy reads from the program's
folder (`current_exe`), and `app_data_root` is `app:/data`. Two traps, both
silent but for a log line: the build machine's absolute path baked in found
no shader, so every custom material drew nothing and the map was bare ground
under haze (`pipeline_health`: "could not be loaded"); and an absolute
`system:/…` root does no better, because Rust's `Path` takes `system:` for a
relative name and Bevy joined it onto the program's folder a second time.

`cargo xtask cartopolis <checkout>` in Pinnacle runs it; `PINNACLE_CLOCK`
fixes the time of day, and the haze over the city in those shots is the live
weather, which an offline `--shot` does not fetch.

## In Pinnacle's Store

`midas deploy pinnacle-store` publishes the client as an app in Pinnacle's
hosted Store, https://pinnacle-os.eu/store/cartopolis/, which every
Pinnacle system disk lists (ARM only). `deploy/build/pinnacle-store.sh`
builds it on a Mac node with a checkout of Pinnacle's main of its own
(`$TOOLS/pinnacle`), as an **unsigned** package; the deploy host signs it
there with the Store's key, which no node ever sees, and checks every file
against its hash before it replaces what is published. The version is the
build number (20000 + commits), as the APK's: a device offers Update when it
differs. Pinnacle's `docs/store.md` has the format. Not after landings: a
cold build is 20 minutes on a Mac node, so it is deployed when asked.
