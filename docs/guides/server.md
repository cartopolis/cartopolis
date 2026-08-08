# Self-hosted geo services (Overpass + Nominatim + 3DBAG)

Operational guide for the `example.com` server: a Docker Compose stack running a
local [Overpass API](https://dev.overpass-api.de/) and
[Nominatim](https://nominatim.org/) instance, each seeded with the **Netherlands**
OSM extract, behind a [Caddy](https://caddyserver.com/) reverse proxy with
automatic TLS.

The viberfox client streams building footprints and POI layers from Overpass
(`overpass_query` in `crates/viberfox/src/systems/map/osm_buildings.rs`). The public
`overpass-api.de` endpoint rate-limits (429/504) under the client's streaming
load, so we run our own instance and point the client at it with
`VIBE_OVERPASS_URL`. Nominatim provides geocoding / reverse-geocoding for the
same region.

> **Why Docker.** The upstream images (`wiktorn/overpass-api`,
> `mediagis/nominatim`) bundle the import *and* a diff-update mechanism, so the
> whole deployment — and its maintenance — reduces to a few environment
> variables. The host OS only has to run Docker. (We use the updates for
> Overpass and deliberately not for Nominatim — see
> [Static vs. updating](#static-vs-updating).)

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
  the geo data off the OS disk. Measured, Netherlands: Nominatim ~23 GB, Overpass
  ~10 GB — but a Nominatim flatnode file adds **~105 GB** if you keep diff
  updates, since it is sized by the global node-ID space rather than the extract.
  Running frozen (as we do) is what makes 200 GB comfortable; size up
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
/data                       # OVH block volume (network-attached)
├── docker/                 # Docker data-root (images + named volumes)
└── stack/
    ├── docker-compose.yml   # overpass + nominatim + caddy
    ├── Caddyfile            # reverse proxy + TLS
    └── .env                 # NOMINATIM_PASSWORD (secret, chmod 600, not in git)

/srv/nominatim/pgdata       # root NVMe — Nominatim's PGDATA, bind-mounted
```

Named volumes (`overpass_db`, `caddy_data`, `caddy_config`) all resolve under
`/data/docker/volumes`.

**Nominatim's database is the exception, and deliberately so.** `/data` is OVH's
*additional* volume, which is network-attached: measured at ~240 random 8 KB IOPS
against ~8,000 on the root NVMe. A geocoding search touches thousands of random
pages, so a cold street-and-city lookup took 4–10 s there and tripped Nominatim's
own 10 s timeout (HTTP 503, `Query took too long to process.`), while the same
statement served from cache took 15 ms. Docker's data-root lives on `/data`, so a
*named* volume cannot be moved off it — the PGDATA is a bind mount to
`/srv/nominatim/pgdata` on the root disk instead. See
[docs/notes/geocoding-latency.md](../notes/geocoding-latency.md) for the numbers
and for the several plausible causes that turned out not to be it.

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
- **Nominatim** — imports `PBF_URL`. `IMPORT_STYLE=address` is the lean style
  (address/POI geocoding, faster import). Postgres 16 data lives at
  `/var/lib/postgresql/16/main`. **`FREEZE=true`: no diff updates** — see
  [Static vs. updating](#static-vs-updating) before changing that, because the
  flatnode file it needs costs ~105 GB.
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
      IMPORT_STYLE: address
      NOMINATIM_PASSWORD: ${NOMINATIM_PASSWORD}
      # Static, query-only. Drops the replication block in the image's
      # /app/start.sh entirely — including an unconditional
      # `nominatim replication --init` that otherwise runs on EVERY boot even
      # with UPDATE_MODE unset. See "Static vs. updating" below.
      FREEZE: "true"
      # Query-instance tuning. The image's defaults are import defaults: 2 GB of
      # shared_buffers cannot hold search_name (3.5 GB) + its GIN indexes, and
      # every miss then costs a random read off a 240-IOPS volume. 10 GB of
      # maintenance_work_mem is likewise only useful during the import.
      POSTGRES_SHARED_BUFFERS: 8GB
      POSTGRES_MAINTENANCE_WORK_MEM: 1GB
      POSTGRES_EFFECTIVE_CACHE_SIZE: 18GB
      # Runs `nominatim admin --warm` before serving, so the first real user is
      # not the one who pays for a cold cache after a restart.
      WARMUP_ON_STARTUP: "true"
    volumes:
      # Bind mount, not a named volume: Docker's data-root is on the slow /data
      # block volume. See "Layout" above.
      - /srv/nominatim/pgdata:/var/lib/postgresql/16/main
      # No nominatim_flatnode mount: the node cache is only read by osm2pgsql
      # during import and diff updates, never by queries, and it is ~105 GB.
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

> **Run the two imports one at a time, not concurrently.** Both write to the same
> `/data` volume; run together they saturate the disk and each crawls (Overpass's
> bzip2 passes and Nominatim's `osm2pgsql` compete for I/O). Import Overpass to
> `healthy` **first**, then start Nominatim.

```bash
cd /data/stack

# Caddy first — obtains Let's Encrypt certs (needs step 3 DNS to resolve):
docker compose up -d caddy

# Overpass import — run it ALONE:
docker compose up -d overpass
screen -dmS overpass bash -c 'cd /data/stack && docker compose logs -f overpass'
```

Wait for Overpass to go `healthy` (the loop below), **then** start Nominatim:

```bash
docker compose up -d nominatim
screen -dmS nominatim bash -c 'cd /data/stack && docker compose logs -f nominatim'
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

> **If Overpass stays `unhealthy` after the import** and every query returns an
> HTML page with `open64: 13 Permission denied /db/db/osm3s_osm_base`, the `/db`
> volume directory is `drwx------` (owner-only) so the web process (`www-data`)
> can't traverse to the dispatcher socket. Fix once — it persists on the volume,
> and a restart won't re-import (init is skipped once `/db/init_done` exists):
> ```bash
> docker exec -u root overpass chmod o+rx /db
> docker restart overpass
> ```

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
3. Apply it by **recreating** Caddy — not `caddy reload`. The `Caddyfile` is
   bind-mounted as a single file; an editor or `sed -i` writes a *new inode*, which
   the running container doesn't see, so `reload` reports "config is unchanged".
   Recreating re-reads the file and fetches the cert:

```bash
docker compose up -d --force-recreate caddy
```

Local smoke test meanwhile:

```bash
curl 'http://127.0.0.1:8080/search?q=Dam,Amsterdam&format=jsonv2'
curl 'http://127.0.0.1:8080/reverse?lat=52.373&lon=4.892&format=jsonv2'
```

## 7. Point the client at it

The client (`overpass_query` in `crates/viberfox/src/systems/map/osm_buildings.rs`)
**defaults** to the self-hosted endpoint (`DEFAULT_OVERPASS_URL`), so a plain run
already uses it:

```bash
cargo run -p viberfox
```

Override with `VIBE_OVERPASS_URL` to point elsewhere — e.g. back to the public API:

```bash
VIBE_OVERPASS_URL=https://overpass-api.de/api/interpreter cargo run -p viberfox
```

Against a dedicated instance you can also lower `OVERPASS_THROTTLE`
(`osm_buildings.rs`, the 800 ms politeness delay) for faster streaming.

**Nominatim** is wired the same way: geo-search (`geo_nav.rs`) and bookmark
reverse-geocoding (`bookmarks.rs`) both use `nominatim_base_url()`, which defaults
to `DEFAULT_NOMINATIM_URL` and is overridable with `VIBE_NOMINATIM_URL` (e.g.
`https://nominatim.openstreetmap.org`).

## 8. 3DBAG buildings (self-hosted static tiles)

For the Netherlands we serve **3DBAG** building data (real BAG heights + ground
level) instead of OSM footprints with guessed heights. The live 3DBAG API is
slow, so — like Overpass — we host it ourselves. Unlike Overpass it is **static,
download-once** data: no database, no diffs, just preprocessed files behind
Caddy. Currently the **city of Groningen** is covered (90k buildings, ~44 MB).

**Pieces on the box** (all under `/data/3dbag/`, owned by `ubuntu`):

```
/data/3dbag
├── venv/                 # python venv with pyproj (RD -> WGS84 reprojection)
├── bin/3dbag_extract.py  # deployed copy of tools/3dbag_extract.py
└── site/                 # OUTPUT, bind-mounted read-only into caddy
    ├── coverage.json     # which z14 cells this host serves (client routing)
    └── buildings/14/{x}/{y}.json   # per-cell Overpass-shaped buildings
```

The extract script (`tools/3dbag_extract.py` in the repo) pages the 3DBAG WFS
`BAG3D:lod12` layer over an RD/EPSG:28992 bbox, reprojects footprints to WGS84,
derives `height`/`roof:shape`/`roof:height` tags from the BAG attributes
(`b3_h_maaiveld`, `b3_h_70p`, `b3_h_nok`, `b3_dak_type`), and bins each building
into the client's zoom-14 CACHE_ZOOM cell. Output is **Overpass-shaped JSON**, so
the client's existing `parse_buildings` consumes it unchanged — only the *source
URL* differs per covered cell.

**One-time setup** (already done on this box):

```bash
sudo mkdir -p /data/3dbag/{bin,site} && sudo chown -R ubuntu:ubuntu /data/3dbag
sudo apt-get install -y python3.14-venv
cd /data/3dbag && python3 -m venv venv && ./venv/bin/pip install -q pyproj
```

**Generate / refresh a city** (idempotent — rewrites `site/`, no Caddy change):

```bash
# from the repo, redeploy the script if it changed:
scp tools/3dbag_extract.py ubuntu@garage44.eu:/data/3dbag/bin/

# on the box (default bbox = Groningen city; ~40 s):
cd /data/3dbag && ./venv/bin/python bin/3dbag_extract.py --out site --name groningen
# other area: --name <city> --bbox <xmin ymin xmax ymax>   (EPSG:28992 / RD New)
```

**LoD2.2 real roofs** (`tools/3dbag_lod22.py`) — the higher-detail tier: instead
of footprints, it parses 3DBAG's **CityJSON** tiles, triangulates each building's
real LoD2.2 roof/wall surfaces, reprojects, and emits per-cell meshes to
`site/lod22/{14}/{x}/{y}.json` (+ `site/lod22/coverage.json`). Needs `numpy` +
`mapbox_earcut` in the venv:

```bash
cd /data/3dbag && ./venv/bin/pip install -q numpy mapbox_earcut
# ~3-4 min for Groningen; writes into the already-served site/ tree:
./venv/bin/python bin/3dbag_lod22.py --out site --name groningen
```

The client's `lod22.rs` streams these for covered cells (real roofs); the LoD1.2
footprints above remain the fallback tier. Groningen LoD2.2 is ~114 MB / 101k
buildings / 112 cells.

**Serving** — the `caddy` service bind-mounts `/data/3dbag/site:/srv/3dbag:ro`
and the `Caddyfile` has:

```caddy
3dbag.garage44.eu {
	root * /srv/3dbag
	encode gzip
	file_server {
		precompressed gzip
	}
	header Access-Control-Allow-Origin "*"
	header /coverage.json Cache-Control "public, max-age=3600"
	header /buildings/* Cache-Control "public, max-age=86400"
}
```

Adding the bind mount needs a **recreate** (not reload):
`docker compose up -d --force-recreate caddy`. A DNS record
`3dbag` → `<VPS_IPV4>`/`<VPS_IPV6>` must exist (Let's Encrypt).

Smoke test: `curl --compressed https://3dbag.garage44.eu/coverage.json`.

**Client wiring:** the client fetches covered z14 cells from
`VIBE_3DBAG_URL` (default `https://3dbag.garage44.eu`) and falls back to Overpass
(OSM) on a 404 — see `osm_buildings.rs`. Attribution
(`coverage.json.attribution`, CC BY 4.0) is shown in the client.

## 9. VersaTiles vector basemap (self-hosted)

The client's **default basemap** (`DEFAULT_TILE_URL_TEMPLATE`, and the `VIBE_TILE_URL`
the CI scene smoke pins) is a self-hosted mirror of the VersaTiles planet build —
Shortbread-schema MVT, every zoom, worldwide. Routing needs a *vector* source, so
this is not just a cosmetic basemap: against a raster template the planner refuses
to plan.

Like 3DBAG it is **static, download-once** data — but as a single 62 GB archive
rather than a file tree, served by the `versatiles` binary rather than by Caddy.

**The archive.** One file, `/srv/versatiles/osm.versatiles`:

```bash
# 62 GB over ~35 min at 40 MB/s. Run it in `screen`, not nohup, and use a single
# `wget -c` writer — a second one racing the same path corrupts the archive.
screen -S tiles
cd /srv/versatiles
wget -c https://download.versatiles.org/osm.versatiles 2>&1 | tee download.log
```

Budget for it: the box's root volume is 193 GB and this is a third of it. Refreshing
to a newer planet means re-downloading the whole archive — there are no diffs — so
keep enough headroom to hold the new one *beside* the old if you want a swap with no
downtime.

**The service.** The `latest-scratch` image has no shell, hence the explicit
`entrypoint`; the `[osm]` prefix names the source, which is what puts `osm` in the
tile path. No published ports — it is reachable only through Caddy on `geonet`:

```yaml
  tiles:
    image: ghcr.io/versatiles-org/versatiles:latest-scratch
    container_name: tiles
    restart: unless-stopped
    entrypoint: /app/versatiles
    command: serve -i 0.0.0.0 -p 8080 "[osm]/data/osm.versatiles"
    volumes:
      - /srv/versatiles/osm.versatiles:/data/osm.versatiles:ro
    networks: [geonet]
```

**Serving** — a plain proxy, because the `versatiles` server already sets CORS (`*`)
and a long `Cache-Control` itself:

```caddy
tiles.garage44.eu {
	reverse_proxy tiles:8080
}
```

A DNS record `tiles` → `<VPS_IPV4>`/`<VPS_IPV6>` must exist for Let's Encrypt.

Smoke test — a tile is a gzipped protobuf, so check the status and size, not the body:

```bash
curl -sI https://tiles.garage44.eu/tiles/osm/14/8490/5319 | head -3
```

**Client wiring:** `https://tiles.garage44.eu/tiles/osm/{z}/{x}/{y}` is the default;
`VIBE_TILE_URL` overrides it and `VIBE_TILE_FORMAT` forces `vector|raster` when a
template's kind can't be inferred. A scripted run (`--shot` / `cargo shots`) never
sees the server handshake that would supply one, which is why `docs/shots.toml` pins
`tile_url` — the built-in fallback is the *raster* basemap.

## 10. PDOK AHN height proxy (web build only)

The height layer (`systems::height`) reads AHN elevation rasters straight from
`https://service.pdok.nl` on native. The browser build can't: PDOK is a third party
and its CORS rules are not ours to negotiate, so the wasm client asks *its own
origin* for `/up/ahn/…` (`http::upstream_url`) and Caddy forwards it. `Trunk.toml`
provides the same path as a `[[proxy]]` in dev, so one code path works in both.

This block lives on the web client's site, beside the identical `/up/nomads/*` proxy
the globe's cloud cover needs:

```caddy
handle_path /up/ahn/* {
	reverse_proxy https://service.pdok.nl {
		header_up Host service.pdok.nl
		header_up User-Agent "viberfox/0.2 (https://code.garage44.eu/jeroen/viberfox; jvanveen@protonmail.com)"
	}
}
```

The `User-Agent` is not decoration: a browser `fetch` silently drops the header, and
putting it back here is what lets PDOK see who is calling a public service they run
for free. Nothing is cached or stored on our side — the layer is off by default in
the client for the same reason.

## Maintenance

Routine upkeep is mostly automatic. What's left is occasional and change-driven.

**Diff updates** — Overpass applies them daily (`OVERPASS_UPDATE_SLEEP`), nothing
to schedule. **Nominatim does not update**: it runs frozen, on purpose.

### Static vs. updating

Nominatim runs **static** (`FREEZE=true`, no flatnode volume). Reversing that is
a full re-import, so the trade is worth understanding before you change it.

**What the flatnode file is.** A node cache osm2pgsql writes during import,
sized by the *global* OSM node-ID space rather than the extract — so a
Netherlands-only import still produced **105 GB**, over half of `/data`. It is
read only by osm2pgsql, during import and while applying diffs. **Queries never
touch it**; they hit Postgres. A frozen instance therefore has no reader for it
at all.

Measured on this host (2026-07-30): deleting it freed 105 GB and cost ~30 s of
downtime. Search, reverse and the public route all returned 200 immediately
after, and again after a forced restart with the file absent.

**Why `FREEZE=true` is required, not cosmetic.** The image's `/app/start.sh`
runs `nominatim replication --init` *unconditionally* whenever `REPLICATION_URL`
is set and `FREEZE` is not — outside the `UPDATE_MODE` branches, so it fires on
every boot even when no updater is configured. `FREEZE=true` drops that whole
block. Without it, a restart eventually reaches for a file that is no longer
there. Confirm after a restart:

```bash
docker logs --since 5m nominatim | grep -i replication   # expect: replicationpid=0
```

**What you give up.** Incremental updates, permanently, without re-importing.
Geocoding results are a snapshot of the extract's import date. For viberfox that
is acceptable: place names and addresses move slowly, and the client's failure
mode is a missing search result, not incorrect data.

**To re-enable updates** (~105 GB and a 1–3 h re-import):

1. Drop `FREEZE`, restore `REPLICATION_URL` and the `nominatim_flatnode` volume
   plus its `/nominatim/flatnode` mount
2. Set `UPDATE_MODE=continuous` — without it the container still never updates,
   which is exactly how this instance sat "configured for replication" for
   weeks while doing nothing
3. Empty `/srv/nominatim/pgdata` and re-import: the flatnode cannot be
   reconstructed from an existing database

**Check whether an instance is actually updating** (not merely configured to):

```bash
docker exec nominatim psql -U nominatim -d nominatim -tAc 'select * from import_status;'
stat -c %y /data/docker/volumes/geo_nominatim_flatnode/_data/flatnode.file
```

An empty `import_status` and a flatnode mtime equal to the import date mean
replication never ran.

Remaining upkeep:

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
- **Every Overpass query returns an HTML `open64: … Permission denied
  /db/db/osm3s_osm_base` page (and the container is `unhealthy`)** → the `/db`
  volume dir isn't traversable by the `www-data` web process; `docker exec -u root
  overpass chmod o+rx /db` (see step 5).
- **Local query works but the public URL 502s** → backend still importing, or
  Caddy can't reach the service name. `docker compose logs caddy`; confirm both
  are on the `geonet` network (`docker network inspect geo_geonet`).
- **Caddy can't get a cert** → the DNS name must resolve to this box and ports
  80/443 must be reachable (`sudo ss -tlnp | grep -E ':(80|443)'`). Caddy retries
  automatically; watch `docker compose logs -f caddy`.
- **Disk filling during a Nominatim re-import** → it's on `/data` (`df -h /data`),
  isolated from `/`.
- **Both imports started at once and each crawls** → they starve each other on the
  shared disk. Freeze one so the other finishes, then resume:
  `docker pause nominatim` … (wait for Overpass `healthy`) … `docker unpause nominatim`.
  `pause` is a safe cgroup freeze — no data loss.
- **Overpass sits on `osmium fileinfo` / init for a long time** → normal: it fully
  decompresses the ~2 GB `.bz2` (twice — once for the timestamp, once to build the
  DB). It clears on its own; running it alone (above) is much faster.

## Security notes

- Overpass is currently an **open** endpoint. For a country extract that's low
  risk, but to prevent abuse add a Caddy `basic_auth` or an IP allowlist to the
  `overpass.example.com` block (the client would then send credentials via
  `VIBE_OVERPASS_URL`).
- No host firewall is enabled. If you add `ufw`, allow `22/80/443` **before**
  `ufw enable` to avoid locking yourself out; OVH's edge anti-DDoS is separate.
- `.env` holds the Nominatim DB password — keep it `chmod 600` and out of git.
