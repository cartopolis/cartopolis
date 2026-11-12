Cartopolis server — Linux x86_64

What you need: a Linux machine with systemd, a domain name pointed at it, and
ports 80 and 443 open for HTTPS. Caddy (https://caddyserver.com) is the easiest
way to get HTTPS; install it first and the installer sets it up for you.

Install, or upgrade to this version, from this folder:

    ./install.sh maps.example.org

or without downloading anything first:

    curl -fsSL https://cartopolis.org/server/install.sh | sh

It asks for an admin account the first time, starts the server, and adds the
address to Caddy when Caddy is installed. Then open https://maps.example.org in
a browser, or sign in from the Cartopolis app with maps.example.org as the
server.

It updates itself once a day from cartopolis.org, keeping a copy of the previous
version and its database in /opt/cartopolis-server/previous/, and goes back to
it when a new version does not start. Turn that off with:

    sudo systemctl disable --now cartopolis-server-update.timer

Where things are:
    /opt/cartopolis-server/cartopolis.toml   the settings
    /opt/cartopolis-server/data/             the world, the accounts and the map data
    journalctl -u cartopolis-server          what it logs

The map data for your area (buildings, search, terrain) is made by the server
itself: sign in as the admin and open Admin > Data. A whole country takes hours
and several GB of memory while it runs.

Licences: LICENSE, LICENSE-MIT, LICENSE-APACHE; the data sources' terms are in
LICENSE-THIRD-PARTY.md.
