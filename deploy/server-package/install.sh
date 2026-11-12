#!/bin/sh
# Install, upgrade or update cartopolis-server on a Linux machine with systemd.
#
#   curl -fsSL https://cartopolis.org/server/install.sh | sh
#   curl -fsSL https://cartopolis.org/server/install.sh | sh -s maps.example.org
#   ./install.sh maps.example.org            from an unpacked download
#
# It puts the server in /opt/cartopolis-server, runs it under systemd as the
# user `cartopolis`, asks for the first admin account, and adds the address to
# Caddy when Caddy is installed. Run again to upgrade: the settings and data/
# are kept.
#
# It also installs a daily timer that runs `install.sh --update`: when the
# download at $CARTOPOLIS_DOWNLOAD is not the one installed, it fetches it, backs
# the database up, installs it, and puts the old one back if the new one does
# not stay up. `--no-auto-update` leaves the timer out (and turns it off).
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

update=0 auto_update=1 address=""
for arg in "$@"; do
    case "$arg" in
        --update) update=1 ;;
        --no-auto-update) auto_update=0 ;;
        -h | --help) sed -n '2,17p' "$0" 2>/dev/null | sed 's/^# \{0,1\}//'; exit 0 ;;
        -*) die "unknown option $arg" ;;
        *) address=$arg ;;
    esac
done

[ "$(uname -s)" = Linux ] && [ "$(uname -m)" = x86_64 ] \
    || die "the server download is for Linux on x86_64; this is $(uname -sm)"
command -v systemctl >/dev/null || die "this installer needs systemd"
command -v curl >/dev/null || die "this installer needs curl"

SUDO=""
if [ "$(id -u)" != 0 ]; then
    command -v sudo >/dev/null || die "run it as root: it installs a service"
    SUDO=sudo
fi

# A terminal to ask on, even when the script itself arrives on stdin.
has_tty() { (exec </dev/tty) 2>/dev/null; }

# --- The address, asked before anything is fetched ---------------------------------

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

# --- Which package -----------------------------------------------------------

work=$(mktemp -d)
trap 'rm -rf "$work"' EXIT
here=$(cd "$(dirname "$0")" 2>/dev/null && pwd || true)
if [ "$update" = 0 ] && [ -n "$here" ] && [ -x "$here/cartopolis-server" ] && [ -d "$here/web" ]; then
    pkg=$here
    sha=local
else
    sha=$(curl -fsSL "$DOWNLOAD/$PKG.tar.gz.sha256" || true)
    sha=${sha%% *}
    [ -n "$sha" ] || die "cannot reach $DOWNLOAD"
    if [ "$update" = 1 ] && [ "$sha" = "$(cat "$DIR/installed.sha256" 2>/dev/null)" ]; then
        say "cartopolis-server is up to date."
        exit 0
    fi
    if [ "$update" = 1 ] && [ "$sha" = "$(cat "$DIR/failed.sha256" 2>/dev/null)" ]; then
        say "The newest cartopolis-server did not start here before; waiting for the next one."
        exit 0
    fi
    say "Downloading cartopolis-server …"
    curl -fsSL -o "$work/$PKG.tar.gz" "$DOWNLOAD/$PKG.tar.gz"
    got=$(sha256sum "$work/$PKG.tar.gz" | cut -d' ' -f1)
    [ "$got" = "$sha" ] || die "the download does not match its checksum; nothing changed"
    tar -xzf "$work/$PKG.tar.gz" -C "$work"
    pkg=$work/$PKG
fi

# --- Install -------------------------------------------------------------------

id cartopolis >/dev/null 2>&1 \
    || $SUDO useradd --system --home-dir "$DIR" --shell /usr/sbin/nologin cartopolis
$SUDO mkdir -p "$DIR/data"
$SUDO systemctl stop cartopolis-server 2>/dev/null || true

# What an update puts back when the new version does not stay up.
rm_previous() { $SUDO rm -rf "$DIR/previous"; }
if [ -x "$DIR/cartopolis-server" ]; then
    rm_previous
    $SUDO mkdir -p "$DIR/previous"
    $SUDO cp -a "$DIR/cartopolis-server" "$DIR/web" "$DIR/previous/"
    for f in world.db world.db-wal world.db-shm; do
        if [ -f "$DIR/data/$f" ]; then $SUDO cp -a "$DIR/data/$f" "$DIR/previous/"; fi
    done
fi

$SUDO install -m 0755 "$pkg/cartopolis-server" "$DIR/cartopolis-server"
$SUDO install -m 0755 "$pkg/install.sh" "$DIR/install.sh"
$SUDO rm -rf "$DIR/web"
$SUDO cp -r "$pkg/web" "$DIR/web"
if [ "$first" = 1 ]; then
    sed "s|maps.example.org|$address|" "$pkg/cartopolis.toml" | $SUDO tee "$DIR/cartopolis.toml" >/dev/null
fi
for unit in cartopolis-server.service cartopolis-server-update.service cartopolis-server-update.timer; do
    $SUDO install -m 0644 "$pkg/$unit" "/etc/systemd/system/$unit"
done
$SUDO chown -R cartopolis:cartopolis "$DIR/data"
was=$(cat "$DIR/installed.sha256" 2>/dev/null || true)
printf '%s\n' "$sha" | $SUDO tee "$DIR/installed.sha256" >/dev/null

# The first admin, asked for while nothing else runs.
admin_cmd="cd $DIR && sudo -u cartopolis ./cartopolis-server create-admin"
if [ "$update" = 0 ] && [ ! -f "$DIR/data/world.db" ] && has_tty; then
    say ""
    say "The first admin account: it can make the map data and manage accounts."
    $SUDO sh -c "cd '$DIR' && runuser -u cartopolis -- ./cartopolis-server create-admin" </dev/tty \
        || say "No admin made; later: $admin_cmd"
fi

$SUDO systemctl daemon-reload
$SUDO systemctl enable --now cartopolis-server >/dev/null 2>&1
if [ "$auto_update" = 1 ]; then
    $SUDO systemctl enable --now cartopolis-server-update.timer >/dev/null 2>&1
elif [ "$update" = 0 ]; then
    $SUDO systemctl disable --now cartopolis-server-update.timer >/dev/null 2>&1 || true
fi

# An update that does not stay up goes back to what ran before it.
# Asked over HTTP, not `systemctl is-active`: a server that crashes on start is
# restarted every few seconds, and reads as active in between.
answers() {
    listen=$(sed -n 's/^http_listen *= *"\(.*\)"/\1/p' "$DIR/cartopolis.toml")
    i=0
    while [ "$i" -lt 30 ]; do
        curl -fs -o /dev/null "http://${listen:-127.0.0.1:4748}/api/services" && return 0
        sleep 2
        i=$((i + 1))
    done
    return 1
}
if [ "$update" = 1 ] && [ -d "$DIR/previous" ]; then
    if ! answers; then
        say "The new version did not stay up; going back to the previous one."
        $SUDO systemctl stop cartopolis-server || true
        $SUDO cp -a "$DIR/previous/cartopolis-server" "$DIR/cartopolis-server"
        $SUDO rm -rf "$DIR/web" && $SUDO cp -a "$DIR/previous/web" "$DIR/web"
        $SUDO rm -f "$DIR/data/world.db-wal" "$DIR/data/world.db-shm"
        for f in world.db world.db-wal world.db-shm; do
            if [ -f "$DIR/previous/$f" ]; then $SUDO cp -a "$DIR/previous/$f" "$DIR/data/"; fi
        done
        printf '%s\n' "$was" | $SUDO tee "$DIR/installed.sha256" >/dev/null
        printf '%s\n' "$sha" | $SUDO tee "$DIR/failed.sha256" >/dev/null
        $SUDO systemctl start cartopolis-server
        exit 1
    fi
    say "cartopolis-server updated."
    exit 0
fi

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
if [ "$auto_update" = 1 ]; then
    say "It updates itself daily; turn that off with: sudo systemctl disable --now cartopolis-server-update.timer"
fi
say "An admin account, now or later: $admin_cmd"
exit 0
}
