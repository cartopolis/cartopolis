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
. "$(dirname "$0")/lib.sh"
command -v bun >/dev/null || { echo "the script host is built with Bun 1.4, and this machine has none" >&2; exit 1; }
cd "$ROOT/tools/script-host"
bun install --frozen-lockfile
out="${CARGO_TARGET_DIR:-$ROOT/target}/script-host"
mkdir -p "$out"
bun build src/child.ts --compile --target=bun-linux-x64 --outfile "$out/cartopolis-script-host"
echo "built $out/cartopolis-script-host ($(du -h "$out/cartopolis-script-host" | cut -f1)) with Bun $(bun --version)"
