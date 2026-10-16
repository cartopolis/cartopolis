---
topic: checking-the-web-build
triggers: [wasm32, "web build", trunk, deploy-wasm.yml, build-std, wasm_thread, web_sys_unstable_apis, libsqlite3-sys, "cargo check --target", rustup target, "no wasm toolchain", cdp_probe, "headless chromium", SwiftShader, callgrind, "real GPU", mac-1, mac-2]
updated: 2026-10-16
---

# The web build can be checked locally, but not with `cargo check --target`

The agent container gained `wasm32-unknown-unknown` on 2026-08-10, so wasm code no
longer has to ship uncompiled. Three separate things make the obvious command
wrong, and each fails with an error that names something other than the cause.

## The command

```bash
RUSTUP_TOOLCHAIN=nightly \
CARGO_UNSTABLE_BUILD_STD=std,panic_abort \
CARGO_TARGET_DIR=/home/agent/cargo-target/wasmcheck \
RUSTFLAGS="--cfg=web_sys_unstable_apis -C target-feature=+atomics,+bulk-memory,+mutable-globals,+simd128" \
cargo check -p cartopolis --target wasm32-unknown-unknown \
    --no-default-features --features audio
```

`--features audio` is part of the shipped configuration since 2026-08-15, not an
extra: `index.html` carries `data-cargo-features="audio"`. Leave it off and the
check silently covers a build nobody deploys — which is how the browser came to
ship with no soundscape at all ([audio-off-platform.md](audio-off-platform.md)).

## Why each part is load-bearing

- **`--no-default-features`.** Historically this dropped the `solo` feature
  (removed 2026-08-21), whose `cartopolis_simulator` → `rusqlite` →
  `libsqlite3-sys` chain could not compile for wasm — the error was a fatal
  C-preprocessor error inside sqlite that read like a broken toolchain.
  `crates/cartopolis/index.html` still passes `data-cargo-no-default-features`
  (plus `audio` named back), and the check mirrors the real build's flags so the
  two cannot drift.
- **Nightly.** `wasm_thread` opens with
  `#![cfg_attr(target_arch = "wasm32", feature(stdarch_wasm_atomic_wait))]`, so
  stable rejects it with E0554 pointing at someone else's crate.
- **`-Z build-std` and the atomics target-features.** The shipped `std` has no
  atomics; `platform::workers` runs real threads as Web Workers against shared
  memory. Needs the `rust-src` component (present on nightly here).
- **`--cfg=web_sys_unstable_apis`.** web-sys gates the Geolocation family behind
  it, and without it `platform::location` cannot see `GeolocationPosition`,
  cascading into ~99 errors across driver mode that name none of this.
  `deploy-wasm.yml` failed on every push until 2026-08-06 for exactly this.
- **A separate `CARGO_TARGET_DIR`.** These RUSTFLAGS differ from the native
  build's, so sharing the default target dir invalidates every other agent's
  cache on this box — several sessions build into one directory here.

The link-arg group in `deploy-wasm.yml` (`--shared-memory`, the `__tls_*`
exports) is only needed to *link*; `cargo check` does not need it.

## Two standing gotchas

**The install is not permanent.** `rustup target add` was run as root inside the
running container (`/usr/local/rustup` is root-owned and there is no in-container
sudo — the permission failure is reported as "cleaning up cached downloads: No
such file or directory", which looks like a corrupt cache and is not). It is lost
whenever `cartopolis-agent` is recreated from its image. The durable fix is a line
in that image's Dockerfile, which lives in another repo — the same situation
[android-build.md](android-build.md) describes for the NDK.

**A green `cargo check` is not a working web client.** It does not link, does not
run `wasm-bindgen`, and cannot execute anything. It catches missing web-sys
features, wrong API shapes and cfg mistakes — which is most of what goes wrong in
code that has never been compiled — and nothing about browser behaviour.

The sharpest case so far, and the reason that sentence is not a formality:
`platform::watchdog` took its clock from `std::time::Instant`. On
`wasm32-unknown-unknown` `Instant::now()` is std's `unsupported` stub and
*panics* — it compiles everywhere and runs nowhere. The watcher thread is
native-only, but `beat()` is a system in `Last` on every target, so the deployed
browser client died at the end of its first frame, before anything was drawn,
with `time not implemented on this platform` and a location naming std rather
than any file of ours. It shipped in the 2026-08-21 deploy and was found on
2026-08-23 by disassembling the published module. `b2f97bc` had already gated
that module's native-only constants and statics "so wasm builds again" — which
is exactly the trap: gating until it compiles answers the compiler, not the
browser. **Every clock in this tree is `web_time::Instant` or
`bevy_platform::time::Instant`** (natively both are re-exports of std's, so
there is no native cost and no reason to reach for `std::time` at all); a fresh
`use std::time::Instant` in code a wasm target can reach is a crash, not a lint.

## The browser harness cannot diagnose a device loss

`tools/cdp_probe.ts` under headless Chromium on SwiftShader **loses the WebGPU
device on any page that presents to a canvas** — measured 2026-08-23 with a
twenty-line page whose whole body is `configure()` + clear + `submit()`, no
allocations: `device.lost` fires within a second (`reason: "destroyed"`, or
`unknown` with `A valid external Instance reference no longer exists`). A
control page rendering to an offscreen texture with a readback is never lost and
returns correct pixels, so it is canvas presentation, not the adapter or the
workload. This cost a session: five probe runs against the deployed client
"reproduced" a `DeviceLost` that was entirely the harness's own.

So: before attributing any device loss seen in the probe to the app, run the
trivial canvas page; and treat everything downstream of a probe-side loss — the
halted renderer, the blank canvas, the follow-on ECS panic — as void. What the
probe remains good for: console output, Rust panics, request accounting, adapter
identity, how far startup got — and the reload-recovery path itself
([web-device-loss.md](web-device-loss.md)), which its guaranteed loss exercises
end to end.

## `audio` is a default feature, so the web build has none

`index.html` builds with no feature list at all, and `audio` is in `default`. The
deployed browser client therefore contains neither `bevy_audio` nor
`platform::audio`'s web arm, and `AudioInput::available()` is `false` there.
Browser voice needs that feature enabled for wasm first — a deliberate change,
since it pulls Bevy's web audio path into a build kept small on purpose.

## Timing it needs a real GPU, and the Mac nodes have one (2026-10-16)

Nothing about time measured in the container's browser means anything: it
presents on lavapipe, and its GPU process blocks the page for seconds at a time
([wasm-performance.md](wasm-performance.md)). A Mac node (`midas take --node
mac-2`) is a macOS VM with an Apple GPU, and Chrome there gets a real adapter
(`vendor=apple`). What it took, all in the node's home, nothing system-wide:

- **Chrome for Testing** (mac-arm64 zip, unzipped under `~/cft`) and a
  `~/bin/chromium` shim that execs it — `cdp_probe` spawns `chromium` by name and
  drops its Linux-only Vulkan flags on macOS.
- **Bun** (`curl -fsSL https://bun.sh/install | bash`), because the probe has to
  run *on* the node: nodes refuse port forwarding, so DevTools cannot be reached
  from here.
- **The build itself can be made there too**, in about five minutes cold:
  `rustup component add rust-src --toolchain nightly`, plus the
  `aarch64-apple-darwin` release binaries of trunk and of wasm-bindgen at the
  version `Cargo.lock` names, then `deploy/build/wasm.sh`'s `trunk build` line
  with its RUSTFLAGS. That is what to do when the Linux node is held.
- `tools/serve_web_build.ts` serves the result the way the site does.

Then `bun tools/cdp_probe.ts --url http://127.0.0.1:8297/viewer/?fps_log --gpu
--profile-from 0 --profile-out /tmp/run` writes `run.cpuprofile` (Chrome DevTools,
speedscope) and `run.callgrind` (kcachegrind/qcachegrind). Build with
`CARGO_PROFILE_WEB_RELEASE_STRIP=none` or every frame is `wasm-function[N]`.
`--trace` records every process, which is the only view into a page blocked on
the GPU process; the probe also pauses a main thread that stops answering and
prints its stack.
