---
topic: checking-the-web-build
triggers: [wasm32, "web build", trunk, deploy-web.yml, build-std, wasm_thread, web_sys_unstable_apis, libsqlite3-sys, "cargo check --target", rustup target, "no wasm toolchain"]
updated: 2026-08-10
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

- **`--no-default-features`.** The default `solo` feature pulls
  `cartopolis_simulator` → `rusqlite` → `libsqlite3-sys`, whose bundled C cannot
  find `stdio.h` for wasm. The error is a fatal C-preprocessor error inside
  sqlite, which reads like a broken toolchain and is really a feature flag.
  `crates/cartopolis/index.html` passes `data-cargo-no-default-features` for this
  reason — the web client is Phase-1 Viewer with no sim (ADR-031/033).
- **Nightly.** `wasm_thread` opens with
  `#![cfg_attr(target_arch = "wasm32", feature(stdarch_wasm_atomic_wait))]`, so
  stable rejects it with E0554 pointing at someone else's crate.
- **`-Z build-std` and the atomics target-features.** The shipped `std` has no
  atomics; `platform::workers` runs real threads as Web Workers against shared
  memory. Needs the `rust-src` component (present on nightly here).
- **`--cfg=web_sys_unstable_apis`.** web-sys gates the Geolocation family behind
  it, and without it `platform::location` cannot see `GeolocationPosition`,
  cascading into ~99 errors across driver mode that name none of this.
  `deploy-web.yml` failed on every push until 2026-08-06 for exactly this.
- **A separate `CARGO_TARGET_DIR`.** These RUSTFLAGS differ from the native
  build's, so sharing the default target dir invalidates every other agent's
  cache on this box — several sessions build into one directory here.

The link-arg group in `deploy-web.yml` (`--shared-memory`, the `__tls_*`
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

## `audio` is a default feature, so the web build has none

`index.html` builds with no feature list at all, and `audio` is in `default`. The
deployed browser client therefore contains neither `bevy_audio` nor
`platform::audio`'s web arm, and `AudioInput::available()` is `false` there.
Browser voice needs that feature enabled for wasm first — a deliberate change,
since it pulls Bevy's web audio path into a build kept small on purpose.
