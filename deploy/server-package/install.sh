#!/bin/sh
# Install cartopolis-server on a Linux machine with systemd.
#
#   curl -fsSL https://cartopolis.org/server/install.sh | sh
#   curl -fsSL https://cartopolis.org/server/install.sh | sh -s maps.example.org
#   ./install.sh maps.example.org            from an unpacked download
#
# It only places the program: in /opt/cartopolis-server, owned by and run as the
# user `cartopolis` under systemd. Then it asks for the first admin account and
# adds the address to Caddy when Caddy is installed. Updates are the program's
# own: its Admin page offers a release the Cartopolis maintainer signed, and an
# admin installs it there (docs/mockups/release-approval). Running this again
# replaces the program and keeps the settings and data/.
#
# The whole script is in braces so that `curl | sh` runs nothing until all of
# it has arrived.
{
set -eu

DOWNLOAD=${CARTOPOLIS_DOWNLOAD:-https://cartopolis.org/server}
PKG=cartopolis-server-linux-x86_64
DIR=/opt/cartopolis-server

say() { printf '%s\n' "$*"; }
die() { printf 'cartopolis: %s\n' "$*" >&2; exit 1; }

address=${1:-}
case "$address" in -*) die "usage: install.sh [address]" ;; esac

[ "$(uname -s)" = Linux ] && [ "$(uname -m)" = x86_64 ] \
    || die "the server download is for Linux on x86_64; this is $(uname -sm)"
command -v systemctl >/dev/null || die "this installer needs systemd"

SUDO=""
if [ "$(id -u)" != 0 ]; then
    command -v sudo >/dev/null || die "run it as root: it installs a service"
    SUDO=sudo
fi

# A terminal to ask on, even when the script itself arrives on stdin.
has_tty() { (exec </dev/tty) 2>/dev/null; }

# --- The address, asked before anything is fetched ---------------------------

first=0
[ -f "$DIR/cartopolis.toml" ] || first=1
if [ "$first" = 1 ]; then
    if [ -z "$address" ]; then
        has_tty || die "give the server's address: … | sh -s maps.example.org"
        printf 'The address of this server, e.g. maps.example.org: '
        read -r address </dev/tty
    fi
    address=${address#https://}
    address=${address#http://}
    address=${address%%/*}
    [ -n "$address" ] || die "the server needs an address"
fi

# --- The package ---------------------------------------------------------------

work=$(mktemp -d)
trap 'rm -rf "$work"' EXIT
here=$(cd "$(dirname "$0")" 2>/dev/null && pwd || true)
if [ -n "$here" ] && [ -x "$here/cartopolis-server" ] && [ -d "$here/web" ]; then
    pkg=$here
else
    command -v curl >/dev/null || die "this installer needs curl"
    say "Downloading cartopolis-server …"
    sha=$(curl -fsSL "$DOWNLOAD/$PKG.tar.gz.sha256" || true)
    sha=${sha%% *}
    [ -n "$sha" ] || die "cannot reach $DOWNLOAD"
    curl -fsSL -o "$work/$PKG.tar.gz" "$DOWNLOAD/$PKG.tar.gz"
    [ "$(sha256sum "$work/$PKG.tar.gz" | cut -d' ' -f1)" = "$sha" ] \
        || die "the download does not match its checksum; nothing changed"
    tar -xzf "$work/$PKG.tar.gz" -C "$work"
    pkg=$work/$PKG
fi

# --- Install -------------------------------------------------------------------

id cartopolis >/dev/null 2>&1 \
    || $SUDO useradd --system --home-dir "$DIR" --shell /usr/sbin/nologin cartopolis
$SUDO systemctl stop cartopolis-server 2>/dev/null || true
$SUDO mkdir -p "$DIR/data"
$SUDO install -m 0755 "$pkg/cartopolis-server" "$DIR/cartopolis-server"
$SUDO rm -rf "$DIR/web"
$SUDO cp -r "$pkg/web" "$DIR/web"
if [ "$first" = 1 ]; then
    sed "s|maps.example.org|$address|" "$pkg/cartopolis.toml" | $SUDO tee "$DIR/cartopolis.toml" >/dev/null
fi
$SUDO install -m 0644 "$pkg/cartopolis-server.service" /etc/systemd/system/cartopolis-server.service
# The program replaces itself when an admin installs an update, so its
# directory is its own.
$SUDO chown -R cartopolis:cartopolis "$DIR"

# The first admin, asked for while nothing else runs.
admin_cmd="cd $DIR && sudo -u cartopolis ./cartopolis-server create-admin"
if [ ! -f "$DIR/data/world.db" ] && has_tty; then
    say ""
    say "The first admin account: it can make the map data, manage accounts and install updates."
    $SUDO sh -c "cd '$DIR' && runuser -u cartopolis -- ./cartopolis-server create-admin" </dev/tty \
        || say "No admin made; later: $admin_cmd"
fi

$SUDO systemctl daemon-reload
$SUDO systemctl enable --now cartopolis-server >/dev/null 2>&1

# --- HTTPS -----------------------------------------------------------------------

caddyfile=/etc/caddy/Caddyfile
caddy_done=""
if [ "$first" = 1 ] && [ -f "$caddyfile" ] && command -v caddy >/dev/null; then
    if grep -q "^$address[ {]" "$caddyfile"; then
        say "Caddy already serves $address; left as it is."
    else
        $SUDO cp "$caddyfile" "$caddyfile.bak-cartopolis"
        printf '\n%s {\n\treverse_proxy 127.0.0.1:4748\n}\n' "$address" | $SUDO tee -a "$caddyfile" >/dev/null
        $SUDO systemctl reload caddy
        say "Added $address to $caddyfile (the old one is $caddyfile.bak-cartopolis)."
    fi
    caddy_done=1
fi

address=${address:-$(sed -n 's|^public_base_url *= *"https\{0,1\}://\([^/"]*\).*|\1|p' "$DIR/cartopolis.toml")}
say ""
say "cartopolis-server is running: systemctl status cartopolis-server"
if [ "$first" = 1 ] && [ -z "$caddy_done" ]; then
    say "Put HTTPS in front of it. With Caddy (https://caddyserver.com), add to its Caddyfile:"
    say ""
    say "$address {"
    say "	reverse_proxy 127.0.0.1:4748"
    say "}"
    say ""
fi
say "Open https://$address, or sign in from the app with $address as the server."
say "Updates appear on its Admin page; an admin installs them there."
say "An admin account, now or later: $admin_cmd"
exit 0
}
