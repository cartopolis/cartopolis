#!/bin/bash
# Build the Android app into target/android/publish — what `midas deploy apk`
# publishes as https://cartopolis.org/apk/. The steps of deploy-android.yml (now
# the fallback), but **not signed for release**: the release key never leaves
# the deploy host, which re-signs the APK there and fills in the page's size and
# sha256 (`prepare` in the hub's deploy recipe). Here it is signed with a debug
# key, which only proves it can be.
#
# The version code is 20000 + the number of commits on the branch: it must only
# grow (Android refuses an update with a lower one), and the workflow's codes
# were 10000 + its run number, which never reached 20000.
. "$(dirname "$0")/lib.sh"
# What it needs, fetched once into $TOOLS where a node lacks it — on a Linux
# node only the SDK pieces (the image has a JDK and the machine mounts an NDK);
# on a Mac node all of it, for macOS.
case "$(uname -s)" in
    Darwin) host=darwin bt_zip=build-tools_r34-macosx.zip ;;
    *)      host=linux  bt_zip=build-tools_r34-linux.zip ;;
esac
sdk="$TOOLS/android-sdk"
if [ ! -x "$sdk/build-tools/34.0.0/aapt2" ] || [ ! -f "$sdk/platforms/android-34/android.jar" ]; then
    echo "fetching the Android SDK pieces (build-tools 34, platform 34) for $host" >&2
    tmp=$(mktemp -d)
    curl -fsSL -o "$tmp/bt.zip" "https://dl.google.com/android/repository/$bt_zip"
    curl -fsSL -o "$tmp/pl.zip" https://dl.google.com/android/repository/platform-34-ext8_r01.zip
    unzip -q "$tmp/bt.zip" -d "$tmp/bt" && unzip -q "$tmp/pl.zip" -d "$tmp/pl"
    rm -rf "$sdk/build-tools/34.0.0" "$sdk/platforms/android-34"
    mkdir -p "$sdk/build-tools" "$sdk/platforms"
    mv "$tmp"/bt/* "$sdk/build-tools/34.0.0" && mv "$tmp/pl/android-34" "$sdk/platforms/"
    rm -rf "$tmp"
fi
android_ndk
if [ -z "${JAVA_HOME:-}" ]; then
    if [ "$host" = darwin ]; then
        JAVA_HOME="$TOOLS/jdk-17/Contents/Home"
        if [ ! -x "$JAVA_HOME/bin/javac" ]; then
            echo "fetching a JDK 17 (Temurin) for macOS" >&2
            rm -rf "$TOOLS/jdk-17" && mkdir -p "$TOOLS/jdk-17"
            curl -fsSL "https://api.adoptium.net/v3/binary/latest/17/ga/mac/$(uname -m | sed 's/arm64/aarch64/')/jdk/hotspot/normal/eclipse" \
                | tar -xz -C "$TOOLS/jdk-17" --strip-components=1
        fi
    else
        JAVA_HOME=/usr/lib/jvm/java-17-openjdk
    fi
fi
export JAVA_HOME ANDROID_SDK_HOME="$sdk"
case " $(rustup target list --installed | tr '\n' ' ') " in
    *" aarch64-linux-android "*) ;;
    *) rustup target add aarch64-linux-android ;;
esac
command -v cargo-ndk >/dev/null || cargo install --locked --root "$TOOLS" cargo-ndk
export VERSION_CODE=$(( 20000 + $(git -C "$ROOT" rev-list --count HEAD) ))
cd "$ROOT"
crates/cartopolis_android/build-apk.sh

apk="${CARGO_TARGET_DIR:-target}/android/cartopolis-arm64-v8a.apk"
ndk_bin=
for d in "$ANDROID_NDK_HOME"/toolchains/llvm/prebuilt/*/bin; do [ -d "$d" ] && { ndk_bin=$d; break; }; done
aapt2="$sdk/build-tools/34.0.0/aapt2"
# What deploy-android.yml checks before it publishes: each is an APK that
# installs and then cannot start.
# Captured first, then matched: under pipefail, `nm | grep -q` fails when grep
# stops reading at the first match and nm dies of the closed pipe.
lib=$(mktemp)
unzip -p "$apk" lib/arm64-v8a/libcartopolis_android.so > "$lib"
syms=$("$ndk_bin/llvm-nm" --dynamic --defined-only "$lib")
case "$syms" in *android_main*) ;; *) echo "android_main is not exported: the APK would install and fail to start" >&2; exit 1 ;; esac
listing=$(unzip -l "$apk")
case "$listing" in *classes.dex*) ;; *) echo "no classes.dex: javac/d8 did not run" >&2; exit 1 ;; esac
manifest=$("$aapt2" dump xmltree --file AndroidManifest.xml "$apk")
case "$manifest" in *"hasCode(0x0101000c)=false"*) echo "classes.dex is packaged but android:hasCode is false" >&2; exit 1 ;; esac
dynamic=$("$ndk_bin/llvm-readelf" -d "$lib")
case "$dynamic" in *libc++_shared*) echo "the .so needs libc++_shared.so, which the APK does not ship" >&2; exit 1 ;; esac
headers=$("$ndk_bin/llvm-readelf" --program-headers "$lib")
align=0
# Hex to number in the shell, not with gawk's strtonum: macOS's awk has none.
for a in $(printf '%s\n' "$headers" | awk '$1 == "LOAD" { print $NF }'); do
    v=$((a))
    if [ "$align" = 0 ] || [ "$v" -lt "$align" ]; then align=$v; fi
done
[ "$align" -ge 16384 ] || { echo "LOAD segments aligned to $align bytes, under the 16384 Pixel 8+ require" >&2; exit 1; }
rm -f "$lib"

out="${CARGO_TARGET_DIR:-target}/android/publish"
rm -rf "$out" && mkdir -p "$out"
cp "$apk" "$out/cartopolis.apk"
git rev-parse HEAD > "$out/cartopolis.apk.commit"
badging=$("$aapt2" dump badging "$apk")
badging=${badging%%$'\n'*}
vname=$(printf '%s' "$badging" | sed -n "s/.*versionName='\([^']*\)'.*/\1/p")
vcode=$(printf '%s' "$badging" | sed -n "s/.*versionCode='\([^']*\)'.*/\1/p")
sed -e "s|@VERSION_NAME@|$vname|g" -e "s|@VERSION_CODE@|$vcode|g" \
    -e "s|@COMMIT@|$(git rev-parse --short=8 HEAD)|g" -e "s|@BUILT@|$(date -u '+%Y-%m-%d %H:%M UTC')|g" \
    "$ROOT/deploy/build/apk-index.html" > "$out/index.html"
echo "staged $vname (code $vcode) in $out — the deploy host signs it"
