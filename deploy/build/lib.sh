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
# release binary, as the CI image installs it, rather than a cargo install.
wasm_bindgen() {
    local want dir
    want=$(awk '/^name = "wasm-bindgen"$/ { found = 1; next }
                found && /^version/ { gsub(/[",]/, "", $3); print $3; exit }' "$ROOT/Cargo.lock")
    dir="$TOOLS/wasm-bindgen-$want"
    if [ ! -x "$dir/wasm-bindgen" ]; then
        echo "fetching wasm-bindgen $want" >&2
        mkdir -p "$dir"
        curl -sSfL "https://github.com/wasm-bindgen/wasm-bindgen/releases/download/$want/wasm-bindgen-$want-x86_64-unknown-linux-musl.tar.gz" \
            | tar -xz -C "$dir" --strip-components=1
    fi
    ln -sf "$dir/wasm-bindgen" "$TOOLS/bin/wasm-bindgen"
    echo "$dir/wasm-bindgen"
}
