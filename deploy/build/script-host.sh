#!/bin/bash
# Build the script host — the process the simulator runs prim scripts in
# (tools/script-host, docs/notes/javascript-prim-scripts.md) — as one Linux
# x86_64 executable with Bun inside it:
#
#   deploy/build/script-host.sh  → target/script-host/cartopolis-script-host
#
# The simulator looks for it beside its own binary, so a server needs no Bun
# and no node_modules: the hosted sim's deploy publishes it next to
# `simulator`, the server download next to `cartopolis-server`. A Mac node
# cross-compiles the same executable (Bun fetches the Linux runtime once).
#
# The Bun inside is always Bun's own release build, fetched once into $TOOLS,
# never the machine's `bun`: on a Linux node that is Arch's, which asks for the
# node's glibc, and the first host built there wanted GLIBC_2.44 from a deploy
# host with 2.43 — it could not start, and the content pages went out without
# their body (2026-11-17). The release asks for 2.17; checked here as server.sh
# checks the simulator.
. "$(dirname "$0")/lib.sh"
command -v bun >/dev/null || { echo "the script host is built with Bun 1.4, and this machine has none" >&2; exit 1; }
GLIBC=${GLIBC:-2.38}
version=$(bun --version)
version=${version%%+*}
runtime="$TOOLS/bun-$version-linux-x64/bun"
if [ ! -x "$runtime" ]; then
    echo "fetching Bun $version's linux-x64 release" >&2
    tmp=$(mktemp -d)
    curl -fsSL -o "$tmp/bun.zip" "https://github.com/oven-sh/bun/releases/download/bun-v$version/bun-linux-x64.zip"
    # No unzip on the Linux node image; bsdtar reads zip, and is `tar` on a Mac.
    if command -v bsdtar >/dev/null; then bsdtar -xf "$tmp/bun.zip" -C "$tmp"; else tar -xf "$tmp/bun.zip" -C "$tmp"; fi
    mkdir -p "$(dirname "$runtime")"
    mv "$tmp/bun-linux-x64/bun" "$runtime"
    chmod +x "$runtime"
    rm -rf "$tmp"
fi
cd "$ROOT/tools/script-host"
bun install --frozen-lockfile
out="${CARGO_TARGET_DIR:-$ROOT/target}/script-host"
mkdir -p "$out"
bun build src/child.ts --compile --target=bun-linux-x64 --compile-executable-path="$runtime" --outfile "$out/cartopolis-script-host"
newest=$(objdump -T "$out/cartopolis-script-host" | grep -o 'GLIBC_2\.[0-9]*' | sort -uV | tail -1)
[ "$(printf '%s\n%s\n' "$newest" "GLIBC_$GLIBC" | sort -V | tail -1)" = "GLIBC_$GLIBC" ] \
    || { echo "$out/cartopolis-script-host asks for $newest, newer than $GLIBC" >&2; exit 1; }
echo "built $out/cartopolis-script-host ($(du -h "$out/cartopolis-script-host" | cut -f1)) with Bun $version, needs at most $newest"
