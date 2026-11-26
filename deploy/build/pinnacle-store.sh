#!/bin/bash
# Build the client for Pinnacle (jeroen/pinnacle, the owner's own operating
# system) as a package source for its Store — what `midas deploy
# pinnacle-store` publishes as https://pinnacle-os.eu/store/cartopolis/.
# Writes dist/pinnacle-store/:
#
#   catalog                      the Store's line for the app
#   packages/cartopolis.ppa      the package, **unsigned**: the deploy host
#                                signs it into cartopolis.ppk with the Store's
#                                key, which never leaves it (`prepare` in the
#                                hub's deploy recipe)
#   blobs/<sha256>               the program and every asset, by hash
#
# Pinnacle's own `cargo xtask package-cartopolis` does the building (its std,
# its ports of crates.io crates; docs/notes/pinnacle.md), from a checkout of
# Pinnacle's main kept in $TOOLS, or $PINNACLE where set. Its docs/store.md
# has the format and how a device checks it.
#
# The version is the build number the APK and the server carry (20000 + the
# commits on the branch): the Store offers an update when it differs.
. "$(dirname "$0")/lib.sh"
CARTOPOLIS_BUILD_CODE=${CARTOPOLIS_BUILD_CODE:-$(( 20000 + $(git -C "$ROOT" rev-list --count HEAD) ))}
if [ -z "${PINNACLE:-}" ]; then
    # Its own checkout, apart from the one sessions send to this node
    # (/work/repos/jeroen/pinnacle), fetched from where this one came from:
    # on a midas node the hub's mirror, `<host>:jeroen/pinnacle.git`.
    PINNACLE="$TOOLS/pinnacle"
    origin=$(git -C "$ROOT" remote get-url origin)
    url="${origin%:*}:jeroen/pinnacle.git"
    case "$origin" in
        https://*|http://*) url="${origin%/*/*}/jeroen/pinnacle.git" ;;
    esac
    [ -d "$PINNACLE/.git" ] || git init --quiet "$PINNACLE"
    git -C "$PINNACLE" remote set-url origin "$url" 2>/dev/null || git -C "$PINNACLE" remote add origin "$url"
    git -C "$PINNACLE" fetch --quiet origin main
    git -C "$PINNACLE" checkout --quiet --force --detach FETCH_HEAD
fi
echo "Pinnacle at $(git -C "$PINNACLE" rev-parse --short HEAD), Cartopolis $CARTOPOLIS_BUILD_CODE"
out="$ROOT/dist/pinnacle-store"
(cd "$PINNACLE" && cargo xtask package-cartopolis "$ROOT" "$out" "$CARTOPOLIS_BUILD_CODE")
# Its lockfile, which the build moved to Pinnacle's port of wgpu-hal, as it was.
git -C "$ROOT" checkout --quiet -- Cargo.lock
test -s "$out/catalog" && test -s "$out/packages/cartopolis.ppa"
echo "built $out ($(du -sh "$out" | cut -f1))"
