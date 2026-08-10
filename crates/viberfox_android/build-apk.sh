#!/usr/bin/env bash
#
# Build an installable Viberfox APK, with no Gradle and no Java source.
#
#   crates/viberfox_android/build-apk.sh              # arm64, android-release
#   ABI=x86_64 PROFILE=dev ./build-apk.sh             # emulator, faster build
#
# The output is a signed APK at target/android/viberfox-<abi>.apk, ready for
# `adb install -r`. By default it is signed with a debug key generated here on
# first run — enough for a device to accept it, not for distribution. The
# `deploy-android` job hands it the published key instead (see the Optional block
# below and docs/notes/android-build.md).
#
# Required in the environment (or discovered under $ANDROID_HOME):
#   ANDROID_NDK_HOME   NDK r27+                        (compiler, linker, sysroot)
#   ANDROID_SDK_HOME   an SDK root with platforms/ and build-tools/
#   JAVA_HOME          a JDK 17+                       (apksigner and keytool are
#                                                       Java programs; nothing here
#                                                       compiles Java)
#
# Optional:
#   ANDROID_KEY        PKCS#8 DER private key          )  sign with *this* identity
#   ANDROID_CERT       matching X.509 PEM certificate  )  instead of the debug key
#   STRIP=0            keep the .so's symbol table (it is ~100 MB of the APK)
set -euo pipefail

repo_root=$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)
cd "$repo_root"

ABI=${ABI:-arm64-v8a}
# `android-release`, not `release`: fat LTO over one codegen unit cannot link this
# on a 22 GB box — see the profile's comment in the workspace Cargo.toml.
PROFILE=${PROFILE:-android-release}
MIN_SDK=${MIN_SDK:-28}
PACKAGE=eu.garage44.viberfox

# `arm64-v8a` is what a phone is; `x86_64` is what an emulator on a dev box is.
# cargo-ndk speaks ABI names, cargo speaks target triples, and the APK's lib/ dir
# is keyed by ABI — so all three appear here and they have to agree.
case "$ABI" in
    arm64-v8a)   TRIPLE=aarch64-linux-android ;;
    armeabi-v7a) TRIPLE=armv7-linux-androideabi ;;
    x86_64)      TRIPLE=x86_64-linux-android ;;
    *) echo "error: unsupported ABI '$ABI' (arm64-v8a | armeabi-v7a | x86_64)" >&2; exit 2 ;;
esac

: "${ANDROID_SDK_HOME:=${ANDROID_HOME:-${ANDROID_SDK_ROOT:-}}}"
: "${ANDROID_NDK_HOME:=${ANDROID_NDK_ROOT:-${ANDROID_SDK_HOME:+$ANDROID_SDK_HOME/ndk}}}"

need() {
    [ -n "${!1:-}" ] || { echo "error: \$$1 is not set — see the header of $0" >&2; exit 2; }
    [ -e "${!1}" ]   || { echo "error: \$$1 points at ${!1}, which does not exist" >&2; exit 2; }
}
need ANDROID_SDK_HOME
need ANDROID_NDK_HOME
need JAVA_HOME

# Newest build-tools and newest platform, rather than a pinned pair. The two are
# independent of `MIN_SDK` (that is a manifest attribute, not a toolchain choice),
# and pinning them here would mean this script rots every time the CI image's SDK
# moves. `sort -V | tail -1` because build-tools versions are dotted, so a plain
# lexical sort puts 9.0.0 after 34.0.0.
build_tools=$(ls -d "$ANDROID_SDK_HOME"/build-tools/*/ 2>/dev/null | sort -V | tail -1)
android_jar=$(ls -d "$ANDROID_SDK_HOME"/platforms/android-*/android.jar 2>/dev/null | sort -V | tail -1)
[ -n "$build_tools" ] || { echo "error: no build-tools under $ANDROID_SDK_HOME" >&2; exit 2; }
[ -n "$android_jar" ] || { echo "error: no platforms/android-*/android.jar under $ANDROID_SDK_HOME" >&2; exit 2; }
AAPT2="${build_tools}aapt2"
ZIPALIGN="${build_tools}zipalign"
APKSIGNER="${build_tools}apksigner"
KEYTOOL="$JAVA_HOME/bin/keytool"
export PATH="$JAVA_HOME/bin:$PATH"   # apksigner is a shell script that runs `java`

# Not a bare `target/`: this repo's containers set CARGO_TARGET_DIR to one shared
# directory (docs/notes/sccache-parallel-builds.md), so `target/` is where the
# artifacts are *not*. Everything below is written beside them either way.
target_dir=${CARGO_TARGET_DIR:-target}
out="$target_dir/android"
work="$out/$ABI"
rm -rf "$work"
mkdir -p "$work/res/mipmap" "$out"

echo "==> cargo ndk ($ABI / $TRIPLE, profile $PROFILE)"
# `--no-default-features` drops `solo`: Phase-1 Android is the offline viewer, so
# there is no in-process simulator on the phone and therefore no bundled SQLite to
# cross-compile. `--platform` is the *native API level* the NDK links against and
# must match the manifest's minSdkVersion, or the .so imports symbols the oldest
# supported device does not have and the dynamic linker fails the load at launch.
cargo ndk -t "$ABI" --platform "$MIN_SDK" build \
    -p viberfox_android --no-default-features --profile "$PROFILE"

# Cargo's output directory is named after the profile except for `dev`, which
# lands in `debug/`. One special case, and getting it wrong looks like the build
# silently producing nothing.
artifact_dir=$PROFILE
[ "$PROFILE" = dev ] && artifact_dir=debug
so="$target_dir/$TRIPLE/$artifact_dir/libviberfox_android.so"
[ -f "$so" ] || { echo "error: cargo produced no $so" >&2; exit 1; }

# The launcher icon is generated, not committed: assets/icon/viberfox.png is the
# one source of truth for the app's icon across every platform (it is also what
# build.rs embeds into the Windows .exe and what desktop_integration installs into
# the XDG icon dirs). A second copy under res/ would be a copy that drifts.
cp assets/icon/viberfox.png "$work/res/mipmap/icon.png"

echo "==> aapt2 (resources + manifest + assets)"
"$AAPT2" compile --dir "$work/res" -o "$work/res.zip"
# `-A assets` is the whole asset story: aapt2 copies that tree into the APK's
# `assets/`, and on Android Bevy's AssetPlugin.file_path is ignored in favour of
# AndroidAssetReader, which reads paths straight out of it via the framework
# AssetManager. So the repo's assets/ layout is the device's, unchanged.
"$AAPT2" link \
    -o "$work/unaligned.apk" \
    -I "$android_jar" \
    --manifest crates/viberfox_android/AndroidManifest.xml \
    -A assets \
    --min-sdk-version "$MIN_SDK" \
    "$work/res.zip"

echo "==> native lib"
mkdir -p "$work/lib/$ABI"
cp "$so" "$work/lib/$ABI/"
# Strip the *copy*, never cargo's artifact — a stripped .so in the target dir would
# make the next incremental build's freshness check lie. `--strip-unneeded` drops
# the symbol table and the debug sections and keeps `.dynsym`, which is the only
# one that matters here: `android_main` is looked up by name from C and nothing in
# Rust references it, so this is the step that would silently produce an APK that
# installs and does not start. The deploy job re-asserts the symbol afterwards.
if [ "${STRIP:-1}" != 0 ]; then
    llvm_strip="$ANDROID_NDK_HOME/toolchains/llvm/prebuilt/linux-x86_64/bin/llvm-strip"
    if [ -x "$llvm_strip" ]; then
        before=$(du -h "$work/lib/$ABI/libviberfox_android.so" | cut -f1)
        "$llvm_strip" --strip-unneeded "$work/lib/$ABI/libviberfox_android.so"
        echo "    stripped $before -> $(du -h "$work/lib/$ABI/libviberfox_android.so" | cut -f1)"
    else
        echo "    warning: no llvm-strip at $llvm_strip; shipping the .so unstripped" >&2
    fi
fi
# `zip` from inside $work so the entry path is exactly `lib/<abi>/lib*.so`, which
# is where the framework looks. Compressed (rather than stored + page-aligned)
# because the manifest sets extractNativeLibs="true" — the installer unpacks the
# library to the app's private dir, so nothing ever mmaps it out of the APK and
# the alignment that would require does not apply.
(cd "$work" && zip -q -r unaligned.apk "lib/$ABI")

echo "==> sign"
# Which identity signs this matters more than "signed at all" suggests: Android
# keys an installed app to its signing certificate and refuses an update from a
# different one (INSTALL_FAILED_UPDATE_INCOMPATIBLE — the user has to uninstall
# first, losing the tile cache). So a build that is handed a key uses it, and only
# a build with no key falls back to a keystore generated here, which is per
# machine and therefore incompatible with everyone else's.
if [ -n "${ANDROID_KEY:-}" ]; then
    [ -n "${ANDROID_CERT:-}" ] || { echo "error: \$ANDROID_KEY set without \$ANDROID_CERT" >&2; exit 2; }
    # --key/--cert rather than --ks: openssl can produce this pair and keytool
    # cannot be asked for one, which keeps the key's home (the deploy host) free
    # of a JDK it has no other use for.
    sign_args=(--key "$ANDROID_KEY" --cert "$ANDROID_CERT")
else
    ks="$out/debug.keystore"
    if [ ! -f "$ks" ]; then
        echo "    generating a debug keystore (first run)"
        "$KEYTOOL" -genkeypair -v -keystore "$ks" -storepass android -keypass android \
            -alias viberfox-debug -keyalg RSA -keysize 2048 -validity 10950 \
            -dname "CN=Viberfox Debug, O=garage44, C=NL" >/dev/null
    fi
    sign_args=(--ks "$ks" --ks-pass pass:android --key-pass pass:android
               --ks-key-alias viberfox-debug)
fi
"$ZIPALIGN" -f -p 4 "$work/unaligned.apk" "$work/aligned.apk"
# v2 signing (and up) covers the whole archive, so it must come *after* zipalign —
# align an already-signed APK and the signature no longer matches the bytes.
"$APKSIGNER" sign "${sign_args[@]}" \
    --min-sdk-version "$MIN_SDK" \
    --out "$out/viberfox-$ABI.apk" "$work/aligned.apk"
"$APKSIGNER" verify --min-sdk-version "$MIN_SDK" -v "$out/viberfox-$ABI.apk"

echo
echo "==> $out/viberfox-$ABI.apk  ($(du -h "$out/viberfox-$ABI.apk" | cut -f1))"
echo "    adb install -r $out/viberfox-$ABI.apk"
echo "    adb shell am start -n $PACKAGE/android.app.NativeActivity"
echo "    adb logcat -s viberfox:V RustStdoutStderr:V bevy:V"
