#!/usr/bin/env bash
# Serve the WebAssembly build with real threads (ADR-033).
#
# Three things have to line up, and all three are easy to get subtly wrong:
#
#  1. `+atomics` alone does NOT give shared memory — the module links with a
#     private heap per worker, which looks like threading and isn't. The
#     `--shared-memory`/`--import-memory`/`--max-memory` trio is what makes every
#     worker import the *same* heap. Verify, don't assume: the memory section
#     should read `flags=0x3 ... SHARED=YES`.
#  2. wasm-bindgen's thread transform injects a thread id and TLS setup, so it
#     needs those symbols exported by name — `__heap_base`, `__tls_base`,
#     `__wasm_init_tls`, `__tls_size`, `__tls_align`. Missing one fails the build
#     with "failed to prepare module for threading", naming only the first.
#  3. `-Z build-std` rebuilds std with atomics; std as shipped has none. It lives
#     in CARGO_UNSTABLE_BUILD_STD rather than `.cargo/config.toml` so the *native*
#     build stays on stable and never rebuilds std.
#
# Cross-origin isolation (COOP/COEP, set in Trunk.toml) is the browser-side half:
# without it `SharedArrayBuffer` is unavailable and the workers cannot start.
#
# Note each RUSTFLAGS change invalidates the whole build graph — edit with care.
set -euo pipefail

export PATH="$HOME/.cargo/bin:$HOME/.wasm-toolchain/bin:$PATH"
export RUSTUP_TOOLCHAIN=nightly
# `+simd128` is carried here rather than inherited: setting RUSTFLAGS in the
# environment *replaces* `.cargo/config.toml`'s `[target.wasm32-unknown-unknown]
# rustflags` wholesale rather than appending to it, so the SIMD baseline set there
# would silently vanish from this build — leaving tiny-skia's tile raster fully
# scalar, which is precisely the work the worker pool exists to speed up.
# `simd_baseline.rs` turns that into a compile error instead of a slow mystery.
export RUSTFLAGS="-C target-feature=+atomics,+bulk-memory,+mutable-globals,+simd128 \
-C link-arg=--shared-memory \
-C link-arg=--import-memory \
-C link-arg=--max-memory=4294967296 \
-C link-arg=--export=__heap_base \
-C link-arg=--export=__tls_base \
-C link-arg=--export=__wasm_init_tls \
-C link-arg=--export=__tls_size \
-C link-arg=--export=__tls_align"
export CARGO_UNSTABLE_BUILD_STD="std,panic_abort"

cd "$(dirname "$0")"

# `dev-bevy`, never plain `dev`. The dev profile builds every dependency at
# `[profile.dev.package."*"] opt-level = 0`, and on the web that is not a slow
# build — it is an unusable one: WebGL2 exposes no storage buffers, so Bevy falls
# back to CPU light clustering and CPU batching preprocessing, and running those
# unoptimised over a few hundred draws cost ~10 s per frame on real hardware.
# `dev-bevy` puts dependencies at opt-level 3 and leaves our own crate cheap to
# rebuild. Not `--release`: that pulls `wasm-opt` from GitHub (and fat LTO), which
# is a long detour for a dev loop.
exec trunk "${@:-serve}" --cargo-profile "${PROFILE:-dev-bevy}" \
    --port "${PORT:-8195}" --address 127.0.0.1
