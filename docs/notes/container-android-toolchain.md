---
topic: container-android-toolchain
triggers: [aarch64-linux-android, RUSTUP_HOME, ANDROID_NDK_HOME, CC_aarch64_linux_android, blake3_neon, "rustup target add", "cannot check android", "permission denied rustup", "no ndk", crates/cartopolis_android, AndroidManifest.xml, simd_baseline.rs, cartopolis_android]
updated: 2026-10-17
---

# The agent container cannot check the Android target out of the box

`cargo check -p cartopolis_android --target aarch64-linux-android` is the CI gate
(`docs/notes/android-build.md`), and in `cartopolis-agent` it fails twice before it
ever compiles anything. Both failures read as configuration mistakes and neither
is; they cost an afternoon on 2026-08-10 and will cost it again, because the
container is rebuilt from an image that has neither piece.

## Two things are missing, and one of them cannot simply be added

**The target is not installed**, and `rustup target add aarch64-linux-android`
does not work: the toolchain lives at `/usr/local/rustup`, which is **root-owned
and not writable by `agent`**. The failure is a permission error on a path
nobody was thinking about, several lines into rustup's output.

The workaround is to give rustup a home it owns:

```bash
cp -a /usr/local/rustup ~/rustup            # ~1.7 GB
RUSTUP_HOME=~/rustup rustup target add aarch64-linux-android
```

Every later command needs `RUSTUP_HOME=~/rustup` too, or it resolves back to the
shared toolchain and reports the target as missing again. Note this copy is *not*
the sccache hazard `sccache-parallel-builds.md` documents — `RUSTUP_HOME` is not
hashed into the cache key; `CARGO_TARGET_DIR` is, and must be left alone.

**There is no NDK**, and `rustup target add` alone is not enough anyway. `blake3`
(under `bevy_asset`) compiles `blake3_neon.c` for every little-endian aarch64
target and a build script's `cc` runs at *check* time, so the lane needs a cross
C compiler even though it never links. r27c, matching CI:

```bash
curl -fsSL -o /tmp/ndk.zip \
  https://dl.google.com/android/repository/android-ndk-r27c-linux.zip
unzip -q /tmp/ndk.zip -d /tmp/ndk && mv /tmp/ndk/android-ndk-r27c ~/android-ndk
```

~2.0 GB unpacked. Then the two variables that are the whole difference between
this and a native check — `cargo-ndk` is not needed, because a check does not
link and the linker it would configure is never invoked:

```bash
NDK_BIN=~/android-ndk/toolchains/llvm/prebuilt/linux-x86_64/bin
export CC_aarch64_linux_android="$NDK_BIN/aarch64-linux-android28-clang"
export AR_aarch64_linux_android="$NDK_BIN/llvm-ar"
```

**`28` is the API level and must equal `minSdkVersion`** in
`crates/cartopolis_android/AndroidManifest.xml`. It is not a version to bump for
convenience — see `android-build.md`.

## What this costs, and the two things not to do about it

~3.7 GB of container disk for a lane that produces no artifact. That is the price
of the gate, and it is worth paying, because `cartopolis_android`'s dependencies are
target-gated: in a plain `cargo check --workspace` the crate is *empty* and proves
nothing about itself.

Do **not** reach for blake3's `no_neon` feature to skip the NDK. It removes the C
compiler requirement and also turns off SIMD in the shipped app, which is the
exact trade `crates/cartopolis/src/simd_baseline.rs` exists to refuse.

Do **not** write either path into `.cargo/config.toml` or a committed script.
They are facts about one container, not about the repo; CI resolves its own NDK
at `/opt/android-ndk` and does its own `rustup target add` as root. Baking the
NDK into `cartopolis-ci:latest` is the real fix and is only undone because that
image lives in another repo.
