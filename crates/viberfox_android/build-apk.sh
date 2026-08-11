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
#   VERSION_CODE       monotonic integer               )  see the version block
#   VERSION_NAME       what the user sees              )  below
set -euo pipefail

repo_root=$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)
cd "$repo_root"

ABI=${ABI:-arm64-v8a}
# `android-release`, not `release`: fat LTO over one codegen unit cannot link this
# on a 22 GB box — see the profile's comment in the workspace Cargo.toml.
PROFILE=${PROFILE:-android-release}
MIN_SDK=${MIN_SDK:-28}
PACKAGE=eu.garage44.viberfox

# --- version ---------------------------------------------------------------
#
# `versionCode` is the *only* thing Android compares when deciding whether one
# APK updates another, and until this block existed it was the literal `1` in
# AndroidManifest.xml. Every build ever published therefore claimed to be
# version 1: no update semantics at all, and nothing on the device (Settings ▸
# Apps ▸ Viberfox) that could say which build was installed.
#
# It must be a monotonically increasing integer, and the two sources here are
# chosen so that CI's is always the larger:
#
#   * a local build gets the commit count, a few hundred;
#   * `deploy-android.yml` passes `10000 + the workflow run number`.
#
# That ordering is deliberate — a phone refuses a *lower* code as a downgrade, so
# the published build must always be able to install over one someone built by
# hand, and not the other way round. Note the commit count is only meaningful in
# a full clone: `actions/checkout` fetches depth 1, where `rev-list --count` is
# 1, which is the other reason CI passes the value explicitly rather than letting
# this default fire.
#
# `versionName` is free text for humans, so it carries what a bug report needs:
# the workspace version and the commit it was built from. The workspace version
# is read out of the root Cargo.toml rather than repeated here, because a version
# written down twice is a version that disagrees with itself.
workspace_version=$(sed -n '/^\[workspace\.package\]/,/^\[/ s/^version = "\(.*\)"/\1/p' Cargo.toml | head -1)
[ -n "$workspace_version" ] || { echo "error: no version in [workspace.package] of Cargo.toml" >&2; exit 2; }
short_sha=$(git rev-parse --short=8 HEAD 2>/dev/null || echo unknown)

VERSION_CODE=${VERSION_CODE:-$(git rev-list --count HEAD 2>/dev/null || echo 1)}
VERSION_NAME=${VERSION_NAME:-"$workspace_version+$short_sha"}
case "$VERSION_CODE" in
    ''|*[!0-9]*) echo "error: VERSION_CODE must be a positive integer, got '$VERSION_CODE'" >&2; exit 2 ;;
esac
[ "$VERSION_CODE" -gt 0 ] || { echo "error: VERSION_CODE must be > 0" >&2; exit 2; }

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
mkdir -p "$work/res" "$out"

# Well-formedness of every XML we are about to hand aapt2, checked *before* the
# thirteen-minute Rust build rather than after it.
#
# This is here because it has already happened: a `--version-code` written inside
# an XML comment (a double hyphen is illegal there) and a comment placed among an
# element's attributes (a node cannot live in an attribute list) both shipped, and
# both were found by aapt2 at the very end of a cold `android-release` build that
# had just compiled the entire Bevy stack. aapt2's message — "not well-formed
# (invalid token)" with a line and column — is perfectly good; it just arrives
# sixteen minutes too late to be cheap.
#
# Python is used rather than xmllint because the CI image is known to have it and
# `libxml2-utils` is not something this script should require. Failing here costs
# a second.
echo "==> checking the XML is well-formed"
if command -v python3 >/dev/null 2>&1; then
    python3 - "crates/viberfox_android" <<'PYEOF'
import pathlib, sys, xml.etree.ElementTree as ET

root = pathlib.Path(sys.argv[1])
bad = []
for path in sorted(list(root.glob("AndroidManifest.xml")) + list((root / "res").rglob("*.xml"))):
    try:
        ET.parse(path)
    except ET.ParseError as exc:
        bad.append(f"{path}: {exc}")
if bad:
    print("XML is not well-formed; aapt2 would reject this after the whole build:")
    for line in bad:
        print(f"  {line}")
    sys.exit(1)
PYEOF
else
    echo "    (no python3; skipping — aapt2 will catch it at the end instead)" >&2
fi

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

echo "==> resources"
# The hand-written half: the theme that repaints the cold-start black screen, the
# palette it uses, and the adaptive-icon XML. Copied rather than compiled in place
# because the generated icon buckets below have to land in the same res tree.
cp -r crates/viberfox_android/res/. "$work/res/"

# The bitmap half is *generated*, and from exactly one file:
# assets/icon/viberfox.png is the source of truth for the app's icon on every
# platform (build.rs embeds it into the Windows .exe, desktop_integration installs
# it into the XDG icon dirs). Committing five resized copies of it here would be
# five copies that drift.
#
# It used to be one 256 px PNG dropped into a density-less `res/mipmap/`, which
# Android reads as the *mdpi baseline* — so every phone made in the last decade
# rescaled a 48 dp-nominal image up to its own density, and every launcher with a
# legacy icon treated it to the white-blob shrink. Hence two outputs per bucket:
#
#   icon.png             the legacy launcher icon, 48/72/96/144/192 px
#   icon_foreground.png  the adaptive icon's foreground — the same glyph centred
#                        on a transparent canvas 1.5× as wide, because an
#                        adaptive layer is 108 dp of which only the middle 72 dp
#                        survives the launcher's mask
#
# **A step may only use a tool the image already has** (docs/guides/development.md),
# and this one is not in a position to insist: `viberfox-ci:latest` is built by
# hand in another repo and nothing here can assert what is in it. So the resizer
# is chosen at run time from three, in descending order of quality, and the last
# of them is the Python standard library — no Pillow, no ImageMagick, nothing to
# install. If even that is missing the build does not fail; it ships today's
# single-density icon and says so, because a launcher icon is not worth failing a
# release for.
icon_src=assets/icon/viberfox.png
if   command -v magick  >/dev/null 2>&1; then icon_tool=magick
elif command -v convert >/dev/null 2>&1; then icon_tool=convert
elif command -v python3 >/dev/null 2>&1; then icon_tool=python3
else icon_tool=none
fi

# Pure-stdlib PNG rescale: decode, unfilter, box-average in premultiplied alpha,
# centre on a transparent canvas, re-encode. ~1 s for the whole set on a 256 px
# source, and measured within 1% RMSE of ImageMagick's Lanczos on this image.
# Deliberately narrow — it accepts the one file it is ever pointed at (8-bit,
# non-interlaced, RGB or RGBA) and exits with a reason for anything else.
if [ "$icon_tool" = python3 ]; then
    cat > "$work/png-resize.py" <<'PY'
import struct, sys, zlib

src, dst, glyph, canvas = sys.argv[1], sys.argv[2], int(sys.argv[3]), int(sys.argv[4])

raw = open(src, "rb").read()
if raw[:8] != b"\x89PNG\r\n\x1a\x0a":
    sys.exit("png-resize: %s is not a PNG" % src)
idat, pos, w, h, chans = b"", 8, 0, 0, 0
while pos < len(raw):
    ln, typ = struct.unpack(">I", raw[pos:pos + 4])[0], raw[pos + 4:pos + 8]
    body = raw[pos + 8:pos + 8 + ln]
    pos += 12 + ln
    if typ == b"IHDR":
        w, h, depth, colour, _, _, interlace = struct.unpack(">IIBBBBB", body)
        if depth != 8 or interlace != 0 or colour not in (2, 6):
            sys.exit("png-resize: need an 8-bit non-interlaced RGB/RGBA PNG")
        chans = 3 if colour == 2 else 4
    elif typ == b"IDAT":
        idat += body
    elif typ == b"IEND":
        break

# Undo the per-scanline filters (PNG spec 9.2). Nothing clever, just the five.
data, stride, out, prev = zlib.decompress(idat), w * chans, bytearray(), bytearray(w * chans)
p = 0
for _ in range(h):
    f, line, p = data[p], bytearray(data[p + 1:p + 1 + stride]), p + 1 + stride
    for i in range(stride):
        a = line[i - chans] if i >= chans else 0
        b = prev[i]
        c = prev[i - chans] if i >= chans else 0
        if f == 1:
            line[i] = (line[i] + a) & 255
        elif f == 2:
            line[i] = (line[i] + b) & 255
        elif f == 3:
            line[i] = (line[i] + ((a + b) >> 1)) & 255
        elif f == 4:
            pp = a + b - c
            pa, pb, pc = abs(pp - a), abs(pp - b), abs(pp - c)
            line[i] = (line[i] + (a if pa <= pb and pa <= pc else b if pb <= pc else c)) & 255
        elif f != 0:
            sys.exit("png-resize: unknown filter %d" % f)
    out += line
    prev = line

# Box-average down to `glyph`, in premultiplied alpha — averaging straight RGBA
# pulls the colour of fully transparent texels into the edge and leaves a halo.
scaled = bytearray(glyph * glyph * 4)
for y in range(glyph):
    y0, y1 = y * h // glyph, max(y * h // glyph + 1, (y + 1) * h // glyph)
    for x in range(glyph):
        x0, x1 = x * w // glyph, max(x * w // glyph + 1, (x + 1) * w // glyph)
        r = g = b = al = n = 0
        for sy in range(y0, y1):
            row = sy * stride
            for sx in range(x0, x1):
                i = row + sx * chans
                a = out[i + 3] if chans == 4 else 255
                r += out[i] * a
                g += out[i + 1] * a
                b += out[i + 2] * a
                al += a
                n += 1
        o = (y * glyph + x) * 4
        scaled[o + 3] = al // n
        if al:
            scaled[o], scaled[o + 1], scaled[o + 2] = r // al, g // al, b // al

# Centre it on a transparent square — that padding is the adaptive icon's 108/72
# safe zone, and it is the whole reason this cannot be a plain resize.
off = (canvas - glyph) // 2
rows = bytearray()
for y in range(canvas):
    rows.append(0)  # filter 0: none. Deflate does the work.
    if off <= y < off + glyph:
        rows += bytes(off * 4) + scaled[(y - off) * glyph * 4:(y - off + 1) * glyph * 4]
        rows += bytes((canvas - glyph - off) * 4)
    else:
        rows += bytes(canvas * 4)


def chunk(typ, body):
    return struct.pack(">I", len(body)) + typ + body + struct.pack(">I", zlib.crc32(typ + body) & 0xFFFFFFFF)


open(dst, "wb").write(
    b"\x89PNG\r\n\x1a\x0a"
    + chunk(b"IHDR", struct.pack(">IIBBBBB", canvas, canvas, 8, 6, 0, 0, 0))
    + chunk(b"IDAT", zlib.compress(bytes(rows), 9))
    + chunk(b"IEND", b"")
)
PY
fi

# resize_icon <glyph px> <canvas px> <out>: the glyph scaled to the first size and
# centred on a transparent square of the second (equal sizes = a plain resize).
resize_icon() {
    case "$icon_tool" in
        magick|convert)
            "$icon_tool" "$icon_src" -filter Lanczos -resize "${1}x${1}" \
                -background none -gravity center -extent "${2}x${2}" \
                -strip "PNG32:$3" ;;
        python3)
            python3 "$work/png-resize.py" "$icon_src" "$3" "$1" "$2" ;;
    esac
}

if [ "$icon_tool" = none ]; then
    echo "    warning: no magick/convert/python3 — shipping one density-less 256 px icon," >&2
    echo "             which Android will read as mdpi and rescale on every real phone." >&2
    mkdir -p "$work/res/mipmap"
    cp "$icon_src" "$work/res/mipmap/icon.png"
    cp "$icon_src" "$work/res/mipmap/icon_foreground.png"
else
    for bucket in mdpi:48 hdpi:72 xhdpi:96 xxhdpi:144 xxxhdpi:192; do
        density=${bucket%%:*}
        px=${bucket##*:}
        mkdir -p "$work/res/mipmap-$density"
        resize_icon "$px" "$px"             "$work/res/mipmap-$density/icon.png"
        resize_icon "$px" "$((px * 3 / 2))" "$work/res/mipmap-$density/icon_foreground.png"
    done
    echo "    icons: 5 density buckets + adaptive foregrounds, via $icon_tool"
fi

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
    --version-code "$VERSION_CODE" \
    --version-name "$VERSION_NAME" \
    "$work/res.zip"
echo "    version $VERSION_NAME (code $VERSION_CODE)"

# The licences ride into the APK's own `assets/`, beside the map data and the
# models they are the notice for. Four files, ~30 KB: the dual MIT/Apache choice
# and, more to the point, LICENSE-THIRD-PARTY.md — the ODbL, CC BY 4.0 and OFL
# notices that are a *condition* of shipping OpenStreetMap data, 3DBAG buildings
# and Plus Jakarta Sans at all. Added with `zip` rather than a second `-A`
# directory: it is the same move the native lib below uses, and it avoids either
# writing into the repo's `assets/` tree or copying 60 MB of glTF to a staging
# one just to add four text files next to it.
mkdir -p "$work/assets"
cp LICENSE LICENSE-MIT LICENSE-APACHE LICENSE-THIRD-PARTY.md "$work/assets/"
(cd "$work" && zip -q -r unaligned.apk assets)

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
    # Checked here rather than left to apksigner, because in CI the usual cause is
    # a bind mount that never happened: the Forgejo runner *silently drops* a
    # volume missing from its `valid_volumes` (a warning in the runner's own log,
    # nothing in the job's), so the directory is simply absent and the only other
    # symptom would be a Java stack trace about a file.
    for f in "$ANDROID_KEY" "$ANDROID_CERT"; do
        [ -f "$f" ] || { echo "error: signing material $f does not exist (mount not applied?)" >&2; exit 2; }
    done
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
echo "==> $out/viberfox-$ABI.apk  ($(du -h "$out/viberfox-$ABI.apk" | cut -f1), $VERSION_NAME / code $VERSION_CODE)"
echo "    adb install -r $out/viberfox-$ABI.apk"
echo "    adb shell am start -n $PACKAGE/android.app.NativeActivity"
echo "    adb logcat -s viberfox:V RustStdoutStderr:V bevy:V"
