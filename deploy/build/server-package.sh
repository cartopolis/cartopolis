#!/bin/bash
# Build the server download: cartopolis-server, the web UI it serves, and what
# installs them (deploy/server-package/). Writes
# dist/cartopolis-server-linux-x86_64.tar.gz and its .sha256.
#
#   deploy/build/server-package.sh
#
# The binary is server.sh's (linked against an older glibc, so it runs on the
# Ubuntu and Debian releases people still have), the web UI web.sh's.
. "$(dirname "$0")/lib.sh"
"$ROOT/deploy/build/server.sh" cartopolis_server cartopolis-server release
"$ROOT/deploy/build/web.sh"

cd "$ROOT"
PKG=cartopolis-server-linux-x86_64
# Assembled outside dist/: release.yml uploads every file in dist/, and a
# directory there fails the upload.
stage=$(mktemp -d)
trap 'rm -rf "$stage"' EXIT
mkdir -p "$stage/$PKG" dist
cp "${CARGO_TARGET_DIR:-target}/x86_64-unknown-linux-gnu/release/cartopolis-server" "$stage/$PKG/"
cp -r crates/atlas/web/dist "$stage/$PKG/web"
cp deploy/server-package/* "$stage/$PKG/"
cp LICENSE LICENSE-MIT LICENSE-APACHE LICENSE-THIRD-PARTY.md "$stage/$PKG/"
chmod +x "$stage/$PKG/install.sh" "$stage/$PKG/cartopolis-server"
tar -czf "dist/$PKG.tar.gz" -C "$stage" "$PKG"
( cd dist && sha256sum "$PKG.tar.gz" > "$PKG.tar.gz.sha256" )
echo "built dist/$PKG.tar.gz ($(du -h "dist/$PKG.tar.gz" | cut -f1))"
