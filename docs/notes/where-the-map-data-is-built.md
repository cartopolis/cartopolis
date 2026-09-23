---
topic: pipeline
triggers: [brp.ts, crops.rs, 3dbag_extract.py, 3dbag_lod22.py, bag_attributes.py, bgt_objects.py, dikes.py, atlas, pipeline, extractor, coverage.json, PDOK, 3DBAG, BAG, BGT, proj4, pyproj, earcut, RD, EPSG:28992]
updated: 2026-09-23
---

# The map's Dutch layers are built by atlas

The extractors that turn the Dutch open registers into the per-cell files the
client streams live in `crates/atlas/src/pipeline/`: `water`, `buildings`,
`lod22`, `bag`, `dikes` and `bgt`, plus `bv3d` (the Kadaster's 3D
basisvoorziening, which `water` and `bgt` both read). They write
`<dataset>/14/{x}/{y}.json` and a `coverage.json` per dataset into
`/data/3dbag/site`, which Caddy serves as `3dbag.cartopolis.org`.

They have been ported twice. They started as Python in `tools/`, run by four ssh
commands per city. In 2026-08 they became TypeScript in a separate repository, behind
an admin console, because an operator wants to see what a box is serving, press a
button and watch a log. In 2026-09 they became Rust in atlas
(`atlas.md`), and the console moved with them to `crates/atlas/console`. The
formats did not change either time.

Two ways in, one code path: the console's Data tab (`/api/pipeline/*`,
`crates/atlas/src/console.rs`, one job at a time) and a shell:

```bash
atlas run --out /tmp/site --area groningen --datasets buildings,bag
atlas run --out /tmp/site --name delfzijl --bbox 253000 592000 258000 597000 --datasets bgt
```

`--bbox` is RD New (EPSG:28992), the grid these registers publish on, not degrees.

## What did *not* move, and why

`tools/ifc_lod3.py` and `tools/ifc_demo_building.py` stay Python. They need
`ifcopenshell`, which has no Rust equivalent worth having, and they are a manual
one-off per building rather than a pipeline — a person supplies the model, the
identificatie and the georeferencing on the command line.

`gen_stars.py`, `id_presets.py` and `subset-icon-font.py` were never server-side
at all: they generate artifacts committed here and run on a workstation.

## The seam is shorter now, and still there

**The producers and the consumers are in one repository**, but not in one crate
and not on one type. Atlas writes each format through `serde_json::Value`, and
`lod22.rs`, `building_facts.rs`, `surveyed.rs`, `osm_buildings.rs`, `dikes.rs`
and `water_levels.rs` in the client each parse their own. A renamed key still
compiles on both sides and arrives as a layer that draws nothing. What changed is
that one commit can now change both sides, and one gate tests both. Moving each
format onto a shared type in `cartopolis_geo` is the obvious next step and has
not been taken.

The TypeScript pipeline pinned the formats with `server/pipeline/format.test.ts` against captures
of a real run. That test was not ported: it existed because the parsers were in
another repository, and each client module already pins its shape in its own
tests. **Change a wire format in atlas and change its parser in the same
commit.**

**The seam runs in the other direction too.** `systems::crops` parses a format
whose producer does not exist: the TypeScript pipeline never had a BRP extractor, so the crop
layer draws nothing anywhere until somebody writes one in `pipeline/`
(`crop-parcels.md`). That is the *safe* direction — a client that can read a form
the host never sends is inert, while an extractor that emits a form no client
knows is content silently thrown away.

## Each port was checked against the one before, not assumed equivalent

**Python → TypeScript**, 2026-08-29, over a 1 km square of Groningen
(`233000 582000 234000 583000`): `buildings`, `bag`, `bgt` and `dikes`
structurally identical to 1e-6 across all files. `lod22` was the same surface
with a different triangulation: 8,695 buildings on both sides, same ids, same
vertex counts, 3 buildings differing by one wall triangle, because the JS
`earcut` and Python's `mapbox_earcut` pick different diagonals.

**TypeScript → Rust**, 2026-09-23, over `233000 581000 234000 582000`, both
implementations run against the live sources and every file diffed:

- `buildings` (2,494 buildings), `bag` (2,516 buildings, 7,793 addresses),
  `dikes` and `water`: identical.
- `bgt`: 7 of 9 files identical; the other 2 differ in 3–22 paving coordinates by
  at most 6e-7° (the 7th decimal place, ~5 cm at worst, mostly one unit in the
  last place). The cause is most likely V8's `Math.cos` and libm's `cos`
  rounding the last bit differently. Porting V8's degree-to-radian arithmetic
  exactly did not change it.
- `lod22`: same buildings, ids and vertex counts; `earcutr` picks different
  diagonals again.

A byte-for-byte comparison is the wrong test for this pipeline and a structural
one is the right one. And the failure mode to watch in a port is a producer
feature added *while the port was in review*: `dikes` and the paving materials
landed inside the Python the first port was replacing, and a deletion merges
without a conflict.

## The projection is the load-bearing part, and it agrees for a reason

Every coordinate in every dataset goes through one RD → WGS84 transform
(`pipeline/rd.rs`), and a projection that is subtly wrong does not fail: it writes
a complete, well-formed city half a metre to the left.

`rd.rs` is proj4js's `sterea` + Bessel + seven-parameter Helmert ported line for
line, and agrees with proj4js to under 1e-11° at 49 reference points. proj4js in
turn agreed with the `pyproj` in `/data/3dbag/venv` to ~3e-10° (0.02 mm) — **because
neither has the NSGI grid.** PROJ on that box has no
`nl_nsgi_rdtrans2018.tif` and falls back to the same Helmert. Install that grid
and PROJ would move by a couple of decimetres while `rd.rs` would not. Nothing on
the map would look wrong.

## One covered area per dataset, still

Each extractor rewrites the whole `coverage.json` for its dataset from the run
that just happened, so extracting a second city **replaces** the first rather than
adding to it — the first city's cell files stay on disk and the client stops being
told they are there. The console warns before doing it, and `POST
/api/pipeline/jobs` answers with the datasets it would displace (`replaces`).
Merging coverage across areas is the obvious fix and has not been made: the
manifests would have to be read back and unioned, and a cell listed in a manifest
whose files were deleted is worse than a cell that is not listed.
