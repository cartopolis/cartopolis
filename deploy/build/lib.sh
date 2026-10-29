# Shared by the deploy builds in this directory. Sourced, not run.
#
# Tools a build fetches for itself live outside the checkout, so a checkout
# reset to a commit (which every deploy and warm does) keeps them: in the
# node's /work volume on a midas node, in the user's cache elsewhere.
set -euo pipefail
ROOT=$(git rev-parse --show-toplevel)
if [ -d /work ] && [ -w /work ]; then
    TOOLS=/work/.tools
else
    TOOLS=${XDG_CACHE_HOME:-$HOME/.cache}/cartopolis-tools
fi
mkdir -p "$TOOLS/bin"
export PATH="$TOOLS/bin:$PATH"

# The wasm-bindgen CLI at exactly the version Cargo.lock names: anything else
# refuses the module (or, worse, writes glue that does not match it). The
# release binary, as the CI image installs it, rather than a cargo install —
# the build machine's own: the wasm and web deploys build on a Mac node.
wasm_bindgen() {
    local want dir triple
    want=$(awk '/^name = "wasm-bindgen"$/ { found = 1; next }
                found && /^version/ { gsub(/[",]/, "", $3); print $3; exit }' "$ROOT/Cargo.lock")
    case "$(uname -sm)" in
        "Darwin arm64") triple=aarch64-apple-darwin ;;
        *) triple=x86_64-unknown-linux-musl ;;
    esac
    dir="$TOOLS/wasm-bindgen-$want-$triple"
    if [ ! -x "$dir/wasm-bindgen" ]; then
        echo "fetching wasm-bindgen $want for $triple" >&2
        mkdir -p "$dir"
        curl -sSfL "https://github.com/wasm-bindgen/wasm-bindgen/releases/download/$want/wasm-bindgen-$want-$triple.tar.gz" \
            | tar -xz -C "$dir" --strip-components=1
    fi
    ln -sf "$dir/wasm-bindgen" "$TOOLS/bin/wasm-bindgen"
    echo "$dir/wasm-bindgen"
}

# Trunk, for the wasm client: the Linux node image carries it, a Mac node
# installs it once into $TOOLS.
trunk_cli() {
    command -v trunk >/dev/null || cargo install --locked --quiet --root "$TOOLS" trunk
}

# The Android NDK, as ANDROID_NDK_HOME (exported): the one already set, the
# machine's at /opt/android-ndk, or r27c fetched once into $TOOLS for this host —
# a Mac node has none mounted. Used by apk.sh and the landing gate's Android
# check.
android_ndk() {
    local host tmp
    case "$(uname -s)" in Darwin) host=darwin ;; *) host=linux ;; esac
    if [ -z "${ANDROID_NDK_HOME:-}" ]; then
        if [ -d /opt/android-ndk ]; then
            ANDROID_NDK_HOME=/opt/android-ndk
        else
            ANDROID_NDK_HOME="$TOOLS/android-ndk-r27c"
            if [ ! -x "$ANDROID_NDK_HOME/ndk-build" ]; then
                echo "fetching Android NDK r27c for $host" >&2
                tmp=$(mktemp -d)
                if [ "$host" = darwin ]; then
                    # For macOS the NDK is only published as a disk image.
                    curl -fsSL -o "$tmp/ndk.dmg" https://dl.google.com/android/repository/android-ndk-r27c-darwin.dmg
                    hdiutil attach -quiet -nobrowse -readonly -mountpoint "$tmp/mnt" "$tmp/ndk.dmg"
                    rm -rf "$ANDROID_NDK_HOME" && cp -R "$tmp"/mnt/*.app/Contents/NDK "$ANDROID_NDK_HOME"
                    hdiutil detach -quiet "$tmp/mnt"
                else
                    curl -fsSL -o "$tmp/ndk.zip" https://dl.google.com/android/repository/android-ndk-r27c-linux.zip
                    unzip -q "$tmp/ndk.zip" -d "$tmp" && rm -rf "$ANDROID_NDK_HOME" && mv "$tmp/android-ndk-r27c" "$ANDROID_NDK_HOME"
                fi
                rm -rf "$tmp"
            fi
        fi
    fi
    export ANDROID_NDK_HOME
}
