#!/bin/bash
# Build the atlas web UI into crates/atlas/web/dist — what `midas deploy web`
# publishes as https://cartopolis.org/. The steps of deploy-web.yml (now the
# fallback): the typechecks and the tests are part of the build, so a red one
# publishes nothing.
. "$(dirname "$0")/lib.sh"
WASM_BINDGEN=$(wasm_bindgen)
export WASM_BINDGEN
cd "$ROOT/crates/atlas/web"
bun install --frozen-lockfile
RUSTUP_TOOLCHAIN=nightly bun run build:chat-core
bun x tsc --noEmit
bun x tsc --noEmit -p tsconfig.worker.json
bun test
# The call files the page imports (src/lib/call.ts): SFrame against the
# native vectors, and the codec choice.
bun test ../../cartopolis/web
bun run build.ts --require-chat
echo "built $(du -sh dist | cut -f1) in crates/atlas/web/dist"
