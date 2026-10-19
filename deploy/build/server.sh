#!/bin/bash
# Build a server binary — the simulator or atlas — for the deploy host.
#
#   deploy/build/server.sh <package> <binary> <profile>
#
# A workshop node runs Arch and the deploy host Ubuntu, and a binary linked on the
# node asks for the node's glibc: the first simulator built this way wanted
# GLIBC_2.44 from a host with 2.43, and the deploy's own smoke test refused it.
# So it is linked by cargo-zigbuild against an older glibc, $GLIBC — 2.38, what
# the binaries the CI image built on Ubuntu 24.04 asked for — and checked here
# for anything newer. The binary lands in target/x86_64-unknown-linux-gnu/<profile>/.
. "$(dirname "$0")/lib.sh"
pkg=$1 bin=$2 profile=$3
GLIBC=${GLIBC:-2.38}
if ! command -v zig >/dev/null; then
    # The node image carries zig; a node on an older image fetches it once.
    if [ ! -x "$TOOLS/zig/zig" ]; then
        mkdir -p "$TOOLS/zig"
        curl -fsSL https://ziglang.org/download/0.16.0/zig-x86_64-linux-0.16.0.tar.xz \
            | tar -xJ -C "$TOOLS/zig" --strip-components=1
    fi
    export PATH="$TOOLS/zig:$PATH"
fi
command -v cargo-zigbuild >/dev/null || cargo install --locked --root "$TOOLS" cargo-zigbuild
cd "$ROOT"
cargo zigbuild --locked -j 4 -p "$pkg" --bin "$bin" --profile "$profile" --target "x86_64-unknown-linux-gnu.$GLIBC"
out="${CARGO_TARGET_DIR:-target}/x86_64-unknown-linux-gnu/$profile/$bin"
syms=$(objdump -T "$out")
newest=$(printf '%s\n' "$syms" | grep -o 'GLIBC_2\.[0-9]*' | sort -uV | tail -1)
[ "$(printf '%s\n%s\n' "$newest" "GLIBC_$GLIBC" | sort -V | tail -1)" = "GLIBC_$GLIBC" ] \
    || { echo "$out asks for $newest, newer than $GLIBC" >&2; exit 1; }
echo "built $out ($(du -h "$out" | cut -f1)), needs at most $newest"
