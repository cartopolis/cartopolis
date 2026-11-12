#!/bin/sh
# Install or upgrade cartopolis-server on a Linux machine with systemd.
#
#   sudo ./install.sh maps.example.org
#
# Run it from the unpacked package. It puts the program in /opt/cartopolis-server,
# runs it as the user `cartopolis`, asks for the first admin account, and adds
# the address to Caddy when Caddy is installed. Run again with a newer package to
# upgrade: the settings and data/ are kept.
set -eu

address=${1:-}
case "$address" in
    "" | -*)
        echo "usage: sudo ./install.sh <address>    e.g. sudo ./install.sh maps.example.org" >&2
        exit 2 ;;
esac
address=${address#https://}
address=${address%%/*}
[ "$(id -u)" = 0 ] || { echo "run it with sudo: it installs a service" >&2; exit 1; }
here=$(cd "$(dirname "$0")" && pwd)
dir=/opt/cartopolis-server

id cartopolis >/dev/null 2>&1 \
    || useradd --system --home-dir "$dir" --shell /usr/sbin/nologin cartopolis
mkdir -p "$dir/data"
systemctl stop cartopolis-server 2>/dev/null || true
install -m 0755 "$here/cartopolis-server" "$dir/cartopolis-server"
rm -rf "$dir/web"
cp -r "$here/web" "$dir/web"
if [ ! -f "$dir/cartopolis.toml" ]; then
    sed "s|maps.example.org|$address|" "$here/cartopolis.toml" > "$dir/cartopolis.toml"
fi
chown -R cartopolis:cartopolis "$dir/data"
install -m 0644 "$here/cartopolis-server.service" /etc/systemd/system/cartopolis-server.service

# The first admin, asked for while nothing else runs. Skipped when no one is at
# the keyboard; the command below makes one later.
admin_cmd="cd $dir && sudo -u cartopolis ./cartopolis-server create-admin"
if [ -t 0 ] && [ ! -f "$dir/data/world.db" ]; then
    echo
    echo "The first admin account: it can run the map-data jobs and manage accounts."
    (cd "$dir" && runuser -u cartopolis -- ./cartopolis-server create-admin) \
        || echo "No admin made; later: $admin_cmd"
fi

systemctl daemon-reload
systemctl enable --now cartopolis-server

caddyfile=/etc/caddy/Caddyfile
block="$address {
	reverse_proxy 127.0.0.1:4748
}"
if [ -f "$caddyfile" ] && command -v caddy >/dev/null; then
    if grep -q "^$address[ {]" "$caddyfile"; then
        echo "Caddy already serves $address; left as it is."
    else
        cp "$caddyfile" "$caddyfile.bak-cartopolis"
        printf '\n%s\n' "$block" >> "$caddyfile"
        systemctl reload caddy
        echo "Added $address to $caddyfile (the old one is $caddyfile.bak-cartopolis)."
    fi
    caddy_done=1
fi

echo
echo "cartopolis-server is running: systemctl status cartopolis-server"
if [ -z "${caddy_done:-}" ]; then
    echo "Put HTTPS in front of it. With Caddy (https://caddyserver.com), add to its Caddyfile:"
    echo
    echo "$block"
    echo
fi
echo "Open https://$address, or sign in from the app with $address as the server."
echo "An admin account, now or later: $admin_cmd"
