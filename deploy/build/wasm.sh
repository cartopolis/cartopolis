#!/bin/bash
# Build the wasm client into crates/cartopolis/dist — what `midas deploy wasm`
# publishes as https://cartopolis.org/viewer/. The steps of deploy-wasm.yml (now
# the fallback): threads and SIMD, std rebuilt for atomics, the module checked
# for shared memory, the large assets pre-compressed.
. "$(dirname "$0")/lib.sh"
wasm_bindgen >/dev/null
cd "$ROOT/crates/cartopolis"
RUSTUP_TOOLCHAIN=nightly \
CARGO_UNSTABLE_BUILD_STD=std,panic_abort \
RUSTFLAGS="--cfg=web_sys_unstable_apis \
 -C target-feature=+atomics,+bulk-memory,+mutable-globals,+simd128 \
 -C link-arg=--shared-memory -C link-arg=--import-memory -C link-arg=--max-memory=4294967296 \
 -C link-arg=--export=__heap_base -C link-arg=--export=__tls_base -C link-arg=--export=__wasm_init_tls \
 -C link-arg=--export=__tls_size -C link-arg=--export=__tls_align" \
    trunk build --cargo-profile web-release --dist dist --public-url /viewer/

# A module linked without shared memory loads and then cannot start a thread.
wasm=$(ls dist/*_bg.wasm)
# The glue is the script index.html loads. Not derived from the wasm's name:
# trunk 0.22 hashes the two separately (`cartopolis-4f2e…_bg.wasm` beside
# `cartopolis-e323….js`), which failed every wasm deploy from 2026-10-24.
js=$(grep -o 'cartopolis-[0-9a-f]*\.js' dist/index.html | sort -u)
[ "$(printf '%s\n' "$js" | grep -c .)" = 1 ] \
    || { echo "index.html does not name exactly one glue script: '$js'" >&2; exit 1; }
js="dist/$js"
for sym in wasm_thread_entry_point __wbindgen_thread_destroy; do
    grep -qa "$sym" "$wasm" || { echo "$sym missing: the module was linked without shared memory" >&2; exit 1; }
done
grep -qa "shared:true\|shared: true" "$js" || { echo "the JS glue does not construct a shared WebAssembly.Memory" >&2; exit 1; }
echo "threading: shared memory + thread transform confirmed"

cd dist
for f in *.wasm *.js; do
    [ -f "$f" ] || continue
    gzip -9 -k -f "$f"
    zstd -19 -T0 -q -f "$f" -o "$f.zst"
    brotli -q 11 -w 24 -k -f "$f"
done
echo "built $(du -sh . | cut -f1) in crates/cartopolis/dist"
