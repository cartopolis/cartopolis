//! Compile-time guard on the SIMD baseline the build is supposed to have.
//!
//! Nothing in cartopolis writes intrinsics. The SIMD it runs on is SIMD that
//! already exists in its dependencies and in LLVM's vectoriser, switched on by
//! `target_feature` at compile time — most visibly `tiny-skia`, whose entire
//! raster pipeline is written against `f32x8`/`u32x8` wrappers that lower to
//! real 256-bit AVX only under `target_feature = "avx"`, and collapse to a pair
//! of SSE2 registers (or, on wasm without `simd128`, to *scalar* code) without
//! it. Every streamed vector tile goes through that pipeline.
//!
//! Which makes the flags in `.cargo/config.toml` load-bearing, and makes their
//! failure mode bad: they are not merged with the environment, they are
//! *replaced* by it. Any `RUSTFLAGS` in the shell or the CI job silently
//! discards the whole `[target.*] rustflags` entry, and the build still
//! succeeds — just several times slower at rasterising tiles, with nothing
//! anywhere to say so. The same goes for adding a target triple to the build
//! without adding it to that file.
//!
//! So assert it. A failure here is a build-configuration bug with an obvious
//! fix, which is strictly better than shipping a quietly scalar renderer.
//!
//! Escape hatch: `--features scalar-fallback` for a deliberately unaccelerated
//! build (bisecting a codegen bug, or a host that genuinely predates AVX2).

// x86: `x86-64-v3` — AVX2 + FMA, i.e. Haswell (2013) / Excavator (2015) onward.
// `avx2` implies `avx`, which is the feature tiny-skia's `f32x8` actually tests.
#[cfg(all(
    not(feature = "scalar-fallback"),
    any(target_arch = "x86", target_arch = "x86_64"),
    not(target_feature = "avx2")
))]
compile_error!(
    "x86 build without AVX2. `.cargo/config.toml` sets `-C target-cpu=x86-64-v3` for this \
     target — if you set RUSTFLAGS, it replaced that entry rather than adding to it. Re-add \
     the flag, or build with `--features scalar-fallback` if you meant it."
);

// wasm: `simd128`. Not on by default, and the only one of these where its
// absence costs a full 4x rather than a widening — the browser has no worker
// threads to hide a scalar rasteriser behind. Baseline in Chrome 91+,
// Firefox 89+ and Safari 16.4+.
#[cfg(all(
    not(feature = "scalar-fallback"),
    target_arch = "wasm32",
    not(target_feature = "simd128")
))]
compile_error!(
    "wasm32 build without simd128, which leaves tiny-skia fully scalar. `.cargo/config.toml` \
     sets `-C target-feature=+simd128` for this target — if you set RUSTFLAGS, it replaced \
     that entry. Re-add the flag, or build with `--features scalar-fallback` if you meant it."
);

// aarch64 needs no guard: NEON is part of the architecture, so tiny-skia's
// `target_feature = "neon"` arm is live on every Apple-silicon and ARM64 Linux
// build without anyone opting in.
