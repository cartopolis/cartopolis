# Preparing map data with `carto-pipeline`

The client can derive the whole map from raw sources on its own, and does
everywhere this has not been run. That is expensive and it is the *same* answer
on every device: measured on one capture over Groningen, cutting the ground
surfaces into shape was 45.9 seconds of the client's 78 seconds of map work.

`carto-pipeline` does that work once, on a server. It lives in this repository
rather than in [`cartopy`](https://code.garage44.eu/jeroen/cartopy) because it
calls `cartopolis_geo` — the client's own code — so what the server builds and
what the client would have built come out of the same functions and cannot drift.
cartopy keeps the operator console and runs this as a command.

## Building it

```bash
cargo build -p cartopolis_pipeline --release
# → $CARGO_TARGET_DIR/release/carto-pipeline
```

No game engine and no graphics driver: it builds and runs on a headless server.

## The `ground` stage

Builds the landuse fills, the road ribbons and their kerbs for every map cell in a
box, cuts them into a partition so no two of them cover the same ground, and writes
one file per cell.

```bash
carto-pipeline ground \
    --bbox 6.50,53.19,6.62,53.25 \
    --tile-url 'https://tiles.cartopolis.org/tiles/osm/{z}/{x}/{y}' \
    --out /data/map/ground
```

| flag | |
|---|---|
| `--bbox W,S,E,N` | the area, in degrees. Checked: inside-out boxes and anything past Web Mercator's ±85.05° are refused rather than silently producing nothing. |
| `--zoom` | which cells to build. Default 14, which is what the client streams its ground from. |
| `--tile-url` | vector tile source, `{z}/{x}/{y}` template. |
| `--tile-dir` | tiles already on disk as `{z}/{x}/{y}.mvt`, instead of a URL. Use this to reproduce a run exactly, or to work with no server at all. |
| `--out` | where cells are written: `{z}/{x}/{y}.cgnd` under here. |
| `--jobs` | cells built at once. Defaults to the machine's parallelism. Each job holds a tile and a cell's meshes, so memory scales with it — a dense cell's paving is measured in megabytes. |
| `--force` | rebuild cells that are already there. |

**A run resumes.** Without `--force`, a cell whose file this build can *read* is
left alone — the header is checked, not the filename, so a half-written file from
an interrupted run and a file from an older format are both rebuilt. Stopping a
city halfway and starting again costs only what it had not reached.

**A failed cell does not stop the run.** A city is thousands of cells and one
unreachable tile must not cost the rest. Each failure is reported as it happens and
the command still exits non-zero at the end, so a scheduled build cannot report
success on a half-built city.

## Reading the output while it runs

One JSON object per line on standard output, one per unit of work. Logs go to
standard error, so the two never mix.

```json
{"event":"start","stage":"ground","cells":48,"jobs":8}
{"event":"cell","z":14,"x":8489,"y":5321,"groups":[["Built",1],["Grass",1],["Road(Major)",1]],"triangles":41267,"bytes":1983204,"millis":976}
{"event":"skipped","z":14,"x":8490,"y":5321}
{"event":"failed","z":14,"x":8491,"y":5321,"error":"fetching https://…: status 502"}
{"event":"done","stage":"ground","cells":48,"failed":1,"millis":11204}
```

A console can drive a progress bar off `start`'s `cells`, show each cell as it
lands, and surface `failed` lines to the operator without parsing a log.

From a shell:

```bash
carto-pipeline ground … | jq -r 'select(.event=="failed") | "\(.z)/\(.x)/\(.y) \(.error)"'
```

## What it writes

`{z}/{x}/{y}.cgnd` — one map cell's finished surfaces. The format is
`cartopolis_geo::ground_file`, which is where it is specified; the short version:

* Fixed-width little-endian throughout. Reading it is a bounds check and a copy,
  never a parse. That is the point — a format needing work to decode would give
  back the cost this exists to remove.
* Nothing is compressed inside the file. The transport already compresses, and a
  self-compressing file cannot be served straight off a static host.
* Each surface group travels as an explicitly assigned code, never a Rust
  discriminant. The surface order *is* the depth ladder and gets variants inserted
  into it by design, so a file riding the discriminant would have its meaning
  silently rewritten by a change the ladder invites.
* A reader refuses anything it does not fully understand — wrong version, unknown
  flag, unknown group, truncated file, an index pointing past its own vertices —
  rather than repairing it. The client can always fall back to building the ground
  itself, so a refusal costs a little time and a misread costs a wrong map.

Serve the directory as static files. It needs no application in front of it.

## Where this is going

The ground stage is the first and the largest. Still to move off the client:

* **The height survey, with the roads flattened into it.** The reason the stage
  order starts here rather than there is measured above, but the height work is
  what closes the surface tearing on sloped ground — see
  `docs/notes/the-ground-stack.md`.
* The surveyed pavements, the 3D buildings, the building records and the crop
  parcels, which are TypeScript in cartopy today.

**The output is an override, never a requirement.** Where a prepared cell exists
the client uses it; where one does not, the client builds the ground exactly as it
always has. That is what keeps the map working outside the Netherlands, and what
makes a bad pipeline run degrade to the current behaviour instead of a hole in the
map.
