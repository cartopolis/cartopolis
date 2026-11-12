#!/bin/bash
# Build the server download: cartopolis-server, the web UI it serves, and what
# installs them (deploy/server-package/). Writes dist/server/:
#
#   cartopolis-server-linux-x86_64.tar.gz  (+ .sha256)
#   install.sh                             what `curl … | sh` runs
#   release.json                           what the owner signs (release.json.sig)
#
# for https://cartopolis.org/server/. An installed server reads release.json
# every six hours and offers a higher, owner-signed version on its Admin page
# (crates/server/src/update.rs).
#
# The binary is server.sh's (linked against an older glibc, so it runs on the
# Ubuntu and Debian releases people still have), the web UI web.sh's.
. "$(dirname "$0")/lib.sh"
# The version code, as the APK's (deploy/build/apk.sh): only ever rises along
# main. The program carries it, and refuses a release that is not higher.
CARTOPOLIS_BUILD_CODE=${CARTOPOLIS_BUILD_CODE:-$(( 20000 + $(git -C "$ROOT" rev-list --count HEAD) ))}
CARTOPOLIS_BUILD_NAME="$CARTOPOLIS_BUILD_CODE ($(git -C "$ROOT" rev-parse --short HEAD))"
export CARTOPOLIS_BUILD_CODE CARTOPOLIS_BUILD_NAME
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
# What the owner signs (docs/mockups/release-approval): the version and the
# package's size and checksum. Unsigned here; a server installs nothing until
# release.json.sig beside it is the owner's.
size=$(wc -c < "$out/$PKG.tar.gz" | tr -d ' ')
sum=$(cut -d' ' -f1 "$out/$PKG.tar.gz.sha256")
printf '{"code":%s,"name":"%s","published":"%s","changes":[],"server":{"file":"%s.tar.gz","sha256":"%s","size":%s}}\n' \
    "$CARTOPOLIS_BUILD_CODE" "$CARTOPOLIS_BUILD_NAME" "$(date -u +%Y-%m-%dT%H:%M:%SZ)" "$PKG" "$sum" "$size" \
    > "$out/release.json"
echo "built $out/$PKG.tar.gz ($(du -h "$out/$PKG.tar.gz" | cut -f1))"
