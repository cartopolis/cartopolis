#!/bin/bash
# Build the server download: cartopolis-server, the web UI it serves, and what
# installs and updates them (deploy/server-package/). Writes dist/server/:
#
#   cartopolis-server-linux-x86_64.tar.gz  (+ .sha256)
#   install.sh                             what `curl … | sh` runs
#
# which `midas deploy server` publishes as https://cartopolis.org/server/. An
# installed server's daily update compares that .sha256 with its own.
#
# The binary is server.sh's (linked against an older glibc, so it runs on the
# Ubuntu and Debian releases people still have), the web UI web.sh's.
. "$(dirname "$0")/lib.sh"
"$ROOT/deploy/build/server.sh" cartopolis_server cartopolis-server release
"$ROOT/deploy/build/web.sh"

cd "$ROOT"
PKG=cartopolis-server-linux-x86_64
out=dist/server
stage=$(mktemp -d)
trap 'rm -rf "$stage"' EXIT
rm -rf "$out" && mkdir -p "$stage/$PKG" "$out"
cp "${CARGO_TARGET_DIR:-target}/x86_64-unknown-linux-gnu/release/cartopolis-server" "$stage/$PKG/"
cp -r crates/atlas/web/dist "$stage/$PKG/web"
cp deploy/server-package/* "$stage/$PKG/"
cp LICENSE LICENSE-MIT LICENSE-APACHE LICENSE-THIRD-PARTY.md "$stage/$PKG/"
chmod +x "$stage/$PKG/install.sh" "$stage/$PKG/cartopolis-server"
tar -czf "$out/$PKG.tar.gz" -C "$stage" "$PKG"
( cd "$out" && sha256sum "$PKG.tar.gz" > "$PKG.tar.gz.sha256" )
cp deploy/server-package/install.sh "$out/install.sh"
echo "built $out/$PKG.tar.gz ($(du -h "$out/$PKG.tar.gz" | cut -f1))"
