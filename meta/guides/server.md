# Self-hosted geo services (Overpass + Nominatim)

Operational guide for the `example.com` server: a Docker Compose stack running a
local [Overpass API](https://dev.overpass-api.de/) and
[Nominatim](https://nominatim.org/) instance, each seeded with the **Netherlands**
OSM extract, behind a [Caddy](https://caddyserver.com/) reverse proxy with
automatic TLS.

The viberfox client streams building footprints and POI layers from Overpass
(`overpass_query` in `crates/viberfox/src/systems/osm_buildings.rs`). The public
`overpass-api.de` endpoint rate-limits (429/504) under the client's streaming
load, so we run our own instance and point the client at it with
`VIBE_OVERPASS_URL`. Nominatim provides geocoding / reverse-geocoding for the
same region.

> **Why Docker.** The upstream images (`wiktorn/overpass-api`,
> `mediagis/nominatim`) bundle the import *and* the daily diff-update mechanism,
> so the whole deployment — and its maintenance — reduces to a few environment
> variables. The host OS only has to run Docker.

## Host

| | |
|---|---|
| Provider | OVH VPS-4 (KVM) |
| OS | Ubuntu 26.04 LTS |
| CPU / RAM | 8 vCores / 22 GB |
| System disk | `/dev/sda` 200 GB → `/` |
| Extra volume | `/dev/sdb` 200 GB → `/data` (ext4) |
| Domain | `example.com` (apex, `www`, `overpass` → the VPS) |
| Public IPs | `<VPS_IPV4>` / `<VPS_IPV6>` |

Everything data-heavy (Docker images, both databases) lives on the **`/data`**
volume, keeping the OS disk lean. The Nominatim import cannot fill the root
filesystem.

## Provisioning (what to order)

Order an OVH VPS with these choices (each has a one-line rationale; adjust for
your region and data):

- **Server — OVH VPS-4** (8 vCores, 24 GB RAM, 200 GB NVMe, ~€20/mo): NVMe and
  RAM both matter for the Nominatim import, and this fits Overpass + Nominatim for
  one country with headroom.
- **Extra storage — the 200 GB block volume**: mounted at `/data` (step 1) to keep
  the geo data off the OS disk. 200 GB is comfortable for the Netherlands; size up
  only to go multi-country. You can attach more later.
- **Location — nearest your users**: for NL, Germany (Limburg) or France
  (Gravelines). The workload isn't latency-sensitive, so this is just a tiebreaker.
- **OS — Ubuntu 26.04 LTS**: long support, `apt`, no SELinux friction with Docker.
  The app runs in containers, so optimise the host for stability, not fresh packages.
- **DNS Anycast add-on — skip it**: a small-audience API gains nothing from it.
- **Domain — register one** (here `example.com`): complete the registry's
  contact-validation email. Manage DNS under `OVH Manager → Web Cloud → Domain
  names`; records are in step 3.

**First access.** The delivery email (or `OVH Manager → …→ VPS`) gives the IP and
credentials. SSH in as the distro user (`ssh ubuntu@<IP>`) or with the emailed
root password; the Manager's **KVM console** is the fallback if SSH fails. Then
harden before anything else:

```bash
sudo apt-get update && sudo apt-get upgrade -y
# add your key (from your laptop):  ssh-copy-id ubuntu@<IP>
# then in /etc/ssh/sshd_config set: PasswordAuthentication no; sudo systemctl restart ssh
```

## Layout

```
/data
├── docker/                 # Docker data-root (images + named volumes)
└── stack/
    ├── docker-compose.yml   # overpass + nominatim + caddy
    ├── Caddyfile            # reverse proxy + TLS
    └── .env                 # NOMINATIM_PASSWORD (secret, chmod 600, not in git)
```

Named volumes (`overpass_db`, `nominatim_data`, `nominatim_flatnode`,
`caddy_data`, `caddy_config`) all resolve under `/data/docker/volumes`.

## Executing the steps

Run steps 1–5 in order over SSH as the `ubuntu` user (which has passwordless
`sudo`). After each step, confirm its **✓ check** before moving on. Re-running a
step is safe (idempotent) **except** the `mkfs` in step 1, which erases the volume.

**Fill in these values first and substitute them everywhere below — including
inside the `docker-compose.yml` and `Caddyfile`:**

| Placeholder | Meaning | Where to get it |
|-------------|---------|-----------------|
| `<VPS_IPV4>` / `<VPS_IPV6>` | the VPS public IPs | delivery email / `OVH Manager → …→ VPS` |
| `example.com` | your domain (apex + `www`, `overpass` subdomains) | your registrar |
| `you@example.com` | ACME contact for Let's Encrypt | your email |
| region extract | Geofabrik `.osm.pbf` + `…-updates/` URL | [download.geofabrik.de](https://download.geofabrik.de) (here: `europe/netherlands`) |

Two things are **not** pure SSH commands and act as gates:

- **Step 3 (DNS)** is done in the OVH panel (or via the OVH API) by a human. It is
  a hard gate: do not start Caddy (step 5) until the names resolve, or Let's
  Encrypt fails.
- **The imports (step 5)** take time — Overpass ~20–60 min, Nominatim ~1–3 h. Wait
  for the container to report `healthy` before using the endpoint; the wait
  command is in step 5.

## 1. Extra volume → `/data`

The 200 GB volume ships blank. **First confirm the target device** — the extra
volume is the one with no filesystem and no mountpoint (here `/dev/sdb`; it may
differ). `mkfs` **erases** whatever it points at, so never run it against the
system disk (`/dev/sda*`, mounted at `/`):

```bash
lsblk -o NAME,SIZE,FSTYPE,MOUNTPOINT   # pick the blank disk: no FSTYPE, no MOUNTPOINT
DEV=/dev/sdb                            # <-- set to the confirmed device
```

Then format and mount it by UUID (with `nofail` so a missing volume never blocks
boot):

```bash
sudo mkfs.ext4 -L geodata "$DEV"
UUID=$(sudo blkid -s UUID -o value "$DEV")
sudo mkdir -p /data
echo "UUID=$UUID /data ext4 defaults,discard,nofail 0 2" | sudo tee -a /etc/fstab
sudo mount -a
df -h /data
```

**✓ check:** `df -h /data` shows the ~200 GB volume mounted at `/data`.

## 2. Docker (data-root on `/data`)

Ubuntu's own packages avoid the "Docker's apt repo has no dist for 26.04 yet"
problem on a fresh release:

```bash
sudo apt-get update
sudo apt-get install -y docker.io docker-compose-v2

# point the data-root at the big volume BEFORE pulling anything
sudo systemctl stop docker docker.socket
sudo mkdir -p /data/docker
echo '{ "data-root": "/data/docker" }' | sudo tee /etc/docker/daemon.json
sudo systemctl start docker

sudo usermod -aG docker ubuntu
sudo docker info | grep 'Docker Root Dir'   # -> /data/docker
```

The group membership only applies to a **new** login shell. Open a fresh SSH
session before the later `docker` commands, or prefix them with `sudo` in this one.

**✓ check:** `Docker Root Dir` prints `/data/docker`.

## 3. DNS

**Manual step (OVH panel or API).** In the OVH DNS zone (`Web Cloud → Domain
names → example.com → DNS Zone`), point the names at the VPS. `@` and `www`
replace OVH's parking records; `overpass` is new:

| Subdomain | Type | Target |
|-----------|------|--------|
| `@`        | A    | `<VPS_IPV4>` |
| `@`        | AAAA | `<VPS_IPV6>` |
| `www`      | A / AAAA | same |
| `overpass` | A / AAAA | same |

Verify before starting Caddy (Let's Encrypt needs the names to resolve):

```bash
dig +short overpass.example.com     # must print <VPS_IPV4> before continuing
```

**✓ check (hard gate):** `dig` returns your `<VPS_IPV4>` for `overpass`, apex and
`www`. Do not start Caddy until it does.

> Nominatim's subdomain is **not** created yet — see step 6.

## 4. The stack

`/data/stack/docker-compose.yml` and `/data/stack/Caddyfile` are the whole
deployment. Generate the Nominatim password once:

```bash
sudo mkdir -p /data/stack && sudo chown "$USER" /data/stack
echo "NOMINATIM_PASSWORD=$(openssl rand -hex 24)" > /data/stack/.env
chmod 600 /data/stack/.env
```

### `docker-compose.yml`

Key choices, encoded as env vars:

- **Overpass** — `OVERPASS_META=no` (bbox reads only), `OVERPASS_MODE=init`
  (download + import on first boot, then serve + auto-update). Geofabrik dropped
  the `.osm.bz2` the image expects, so `OVERPASS_PLANET_PREPROCESS` converts the
  `.pbf` to `.bz2` in-place with `osmium` (bundled in the image). Diffs come from
  `OVERPASS_DIFF_URL`, applied daily (`OVERPASS_UPDATE_SLEEP=86400`).
- **Nominatim** — imports `PBF_URL`, keeps up to date from `REPLICATION_URL`.
  `IMPORT_STYLE=address` is the lean style (address/POI geocoding, faster
  import). Postgres 16 data lives at `/var/lib/postgresql/16/main`; the flatnode
  file is persisted so diff updates work.
- **Caddy** — terminates TLS, proxies `overpass.example.com` → `overpass:80`.

```yaml
name: geo

services:
  overpass:
    image: wiktorn/overpass-api:v0.7.62.9
    container_name: overpass
    restart: unless-stopped
    environment:
      OVERPASS_META: "no"
      OVERPASS_MODE: init
      OVERPASS_PLANET_URL: https://download.geofabrik.de/europe/netherlands-latest.osm.pbf
      OVERPASS_PLANET_PREPROCESS: 'mv /db/planet.osm.bz2 /db/planet.osm.pbf && osmium cat -o /db/planet.osm.bz2 /db/planet.osm.pbf && rm /db/planet.osm.pbf'
      OVERPASS_DIFF_URL: https://download.geofabrik.de/europe/netherlands-updates/
      OVERPASS_RULES_LOAD: "10"
      OVERPASS_UPDATE_SLEEP: "86400"
    volumes:
      - overpass_db:/db
    ports:
      - "127.0.0.1:12345:80"
    networks: [geonet]

  nominatim:
    image: mediagis/nominatim:5.3.2
    container_name: nominatim
    restart: unless-stopped
    environment:
      PBF_URL: https://download.geofabrik.de/europe/netherlands-latest.osm.pbf
      REPLICATION_URL: https://download.geofabrik.de/europe/netherlands-updates/
      IMPORT_STYLE: address
      NOMINATIM_PASSWORD: ${NOMINATIM_PASSWORD}
    volumes:
      - nominatim_data:/var/lib/postgresql/16/main
      - nominatim_flatnode:/nominatim/flatnode
    shm_size: 2g
    ports:
      - "127.0.0.1:8080:8080"
    networks: [geonet]

  caddy:
    image: caddy:2
    container_name: caddy
    restart: unless-stopped
    ports:
      - "80:80"
      - "443:443"
    volumes:
      - ./Caddyfile:/etc/caddy/Caddyfile:ro
      - caddy_data:/data
      - caddy_config:/config
    networks: [geonet]

networks:
  geonet:

volumes:
  overpass_db:
  nominatim_data:
  nominatim_flatnode:
  caddy_data:
  caddy_config:
```

### `Caddyfile`

```caddy
{
	email you@example.com
}

overpass.example.com {
	reverse_proxy overpass:80
}

# nominatim.example.com {
# 	reverse_proxy nominatim:8080
# }

example.com, www.example.com {
	header Content-Type "text/plain; charset=utf-8"
	respond "example.com — self-hosted geo services.

Overpass API : https://overpass.example.com/api/interpreter
" 200
}
```

### Write the two files

Save the two blocks above to `/data/stack/`, **substituting your domain and email
first**. Use a **quoted** heredoc (`<<'EOF'`) so `${NOMINATIM_PASSWORD}`, the
quotes, and the `&&` in the preprocess command are written literally instead of
being expanded by the shell:

```bash
cat > /data/stack/docker-compose.yml <<'EOF'
# ...paste the docker-compose.yml block above, verbatim...
EOF

cat > /data/stack/Caddyfile <<'EOF'
# ...paste the Caddyfile block above, with your domain + email...
EOF

cd /data/stack && docker compose config --quiet && echo "compose OK"
```

**✓ check:** `docker compose config` reports no errors (`compose OK`), and
`ls /data/stack` shows `.env`, `docker-compose.yml`, `Caddyfile`.

## 5. Start it — long imports in `screen`

The two imports run far longer than an SSH session (Overpass ~20–60 min,
Nominatim ~1–3 h). The work happens **inside the containers** (managed by the
Docker daemon, so it survives disconnects); we just watch the logs from a
[`screen`](https://www.gnu.org/software/screen/) session so progress is easy to
follow and reattach to.

```bash
cd /data/stack

# Overpass: start + follow
docker compose up -d overpass
screen -dmS overpass bash -c 'cd /data/stack && docker compose logs -f overpass'

# Nominatim: start + follow
docker compose up -d nominatim
screen -dmS nominatim bash -c 'cd /data/stack && docker compose logs -f nominatim'

# Caddy: obtains Let's Encrypt certs immediately (independent of the imports)
docker compose up -d caddy
```

Follow / detach:

```bash
screen -ls                 # list sessions
screen -r overpass         # attach   (detach again: Ctrl-A then D)
docker compose ps          # container + health status
```

While importing, `https://overpass.example.com/api/interpreter` returns **502**
(backend not ready). Block until the container's health flips to `healthy`:

```bash
# waits until Overpass has finished importing:
until [ "$(sudo docker inspect -f '{{.State.Health.Status}}' overpass)" = healthy ]; do
  sleep 30; echo 'overpass still importing…'
done
echo 'overpass ready'
```

Nominatim has no healthcheck; it is ready when a local query returns results (the
smoke test in step 6). Then verify end-to-end:

```bash
# apex landing page (works as soon as Caddy has its cert)
curl -s https://example.com

# Overpass through the public TLS endpoint:
curl 'https://overpass.example.com/api/interpreter' \
  --data 'data=[out:json][timeout:25];way["building"](52.09,5.11,52.10,5.12);out geom;'
```

**✓ check:** the Overpass query returns JSON containing `elements` (no 502).

## 6. Enable Nominatim publicly (after import)

Nominatim listens on `127.0.0.1:8080` only. To expose it:

1. Add DNS records `nominatim` → `<VPS_IPV4>` / `<VPS_IPV6>`
   (same as `overpass`).
2. Uncomment the `nominatim.example.com` block in the `Caddyfile`.
3. Reload Caddy — it fetches the cert on demand:

```bash
docker compose exec caddy caddy reload --config /etc/caddy/Caddyfile
```

Local smoke test meanwhile:

```bash
curl 'http://127.0.0.1:8080/search?q=Dam,Amsterdam&format=jsonv2'
curl 'http://127.0.0.1:8080/reverse?lat=52.373&lon=4.892&format=jsonv2'
```

## 7. Point the client at it

```bash
VIBE_OVERPASS_URL=https://overpass.example.com/api/interpreter cargo run -p viberfox
```

Against a dedicated instance you can also lower `OVERPASS_THROTTLE`
(`osm_buildings.rs`, the 800 ms politeness delay) for faster streaming.

## Maintenance

Routine upkeep is mostly automatic — the containers self-update from Geofabrik
diffs. What's left is occasional and change-driven:

- **Diff updates** — built in. Overpass applies daily
  (`OVERPASS_UPDATE_SLEEP`); Nominatim replicates from `REPLICATION_URL`. Nothing
  to schedule.
- **Restart / boot** — `restart: unless-stopped` brings everything back after a
  reboot. Verify with `docker compose ps`.
- **Image upgrade** — bump the pinned tag in `docker-compose.yml`, then
  `docker compose pull <svc> && docker compose up -d <svc>`. A **major**
  Nominatim bump may change the Postgres version (and thus the volume path
  `/var/lib/postgresql/NN/main`) — check the new image before upgrading, and
  expect a re-import.
- **Re-import from scratch** (e.g. corruption, or switching region): stop the
  service, remove its volume, start again.
  ```bash
  docker compose rm -sf overpass && docker volume rm geo_overpass_db
  docker compose up -d overpass
  ```
- **Backups** — the OSM data is reproducible from Geofabrik, so it needs no
  backup; only `/data/stack/` (compose, Caddyfile, `.env`) is worth keeping. Copy
  it off-box.
- **Alerting (optional)** — for "did a container die / did the diff update
  stall", add a systemd `OnFailure=` notifier or a small healthcheck→webhook.
  An LLM/n8n triage layer on top is optional and not required for upkeep.

## Troubleshooting

Isolate the layer:

- **`docker compose ps` shows a container restarting / unhealthy** →
  `docker compose logs <svc>`. Overpass init failing usually means the
  pbf→bz2 preprocess or the download failed (check the Geofabrik URL resolves).
- **Local query works but the public URL 502s** → backend still importing, or
  Caddy can't reach the service name. `docker compose logs caddy`; confirm both
  are on the `geonet` network (`docker network inspect geo_geonet`).
- **Caddy can't get a cert** → the DNS name must resolve to this box and ports
  80/443 must be reachable (`sudo ss -tlnp | grep -E ':(80|443)'`). Caddy retries
  automatically; watch `docker compose logs -f caddy`.
- **Disk filling during a Nominatim re-import** → it's on `/data` (`df -h /data`),
  isolated from `/`.

## Security notes

- Overpass is currently an **open** endpoint. For a country extract that's low
  risk, but to prevent abuse add a Caddy `basic_auth` or an IP allowlist to the
  `overpass.example.com` block (the client would then send credentials via
  `VIBE_OVERPASS_URL`).
- No host firewall is enabled. If you add `ufw`, allow `22/80/443` **before**
  `ufw enable` to avoid locking yourself out; OVH's edge anti-DDoS is separate.
- `.env` holds the Nominatim DB password — keep it `chmod 600` and out of git.
