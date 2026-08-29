---
topic: pipeline
triggers: [3dbag_extract.py, 3dbag_lod22.py, bag_attributes.py, bgt_objects.py, dikes.py, cartopy, pipeline, extractor, coverage.json, PDOK, 3DBAG, BAG, BGT, proj4, pyproj, earcut, RD, EPSG:28992]
updated: 2026-08-29
---

# The map's Dutch layers are built in the other repository

`tools/3dbag_extract.py`, `tools/3dbag_lod22.py`, `tools/bag_attributes.py`,
`tools/bgt_objects.py` and `tools/dikes.py` are gone from this tree. They are
TypeScript in
[`jeroen/cartopy`](https://code.garage44.eu/jeroen/cartopy) under `server/`, run
by an admin console rather than by four ssh commands per city.

The move was for the console, and the console was the whole point: an operator
wants to see what a box is currently serving, press a button and watch a log, and
none of that fits a script somebody scps to a VPS. `docs/guides/server.md` §8 has
the operational half.

## What did *not* move, and why

`tools/ifc_lod3.py` and `tools/ifc_demo_building.py` stay Python. They need
`ifcopenshell`, which has no equivalent in JS worth having, and they are a manual
one-off per building rather than a pipeline — a person supplies the model, the
identificatie and the georeferencing on the command line.

`gen_stars.py`, `id_presets.py` and `subset-icon-font.py` were never server-side
at all: they generate artifacts that are committed here (a star catalogue, a Rust
table, a subset font), they run on a workstation, and two of them need Python
libraries with no JS counterpart.

## The seam this created, which is worse than the one it removed

**The consumers of these formats are in this repository.** `lod22.rs`,
`building_facts.rs`, `surveyed.rs`, `osm_buildings.rs` and `dikes.rs` each parse
one of them,
and the producer is now in a repository with a different language and a different
CI. Nothing checks that the two agree — it is the `src/types.ts` problem cartopy's
README already describes, four times over and much wider.

The mitigation is `server/pipeline/format.test.ts` over there: each format pinned
against trimmed captures of a real run, so a renamed key fails a test instead of
reaching a client as a layer that silently draws nothing. It is not a compiler.
**Change a wire format here and change it there**, and read that test file first —
it is the closest thing to a specification either side has.

## The port was checked against the Python, not assumed equivalent

Measured 2026-08-29, over a 1 km square of Groningen
(`--bbox 233000 582000 234000 583000`), running both implementations against the
live sources and diffing every output file:

- **`buildings`, `bag`, `bgt`, `dikes`: structurally identical** to a tolerance
  of 1e-6, across all 40 files. Same file set, same ids, same tags, same counts — 3,126
  buildings, 7,586 addresses, 140 berths, 180 monuments, 261 boundaries, 3,258
  paved surfaces on both sides. Vertex positions agreed to 0.02 mm.
- **`lod22`: the same surface, a different triangulation.** 8,695 buildings on
  both sides, same ids, same vertex counts; the JS `earcut` and the Python
  `mapbox_earcut` binding pick different diagonals across the same polygon, so
  the index arrays legitimately differ. 3 buildings of 8,695 differed by one
  *wall* triangle (roof counts identical), and a handful of vertices differed by
  ~7 mm — a last-digit disagreement at the 7th decimal place.

That last one is the reason a byte-for-byte comparison is the wrong test for this
pipeline and a structural one is the right one.

`dikes` and the paving materials were ported later than the rest — they landed on
`main` while the port was in review, inside the very Python it replaces — and were
checked the same way against the same square: 12 files, no differences at all.
That is the failure mode to watch for here. A branch that removes a producer can
be revert-clean in git and still lose a feature somebody added to it in the
meantime, because the loss is a *deletion* that merges without a conflict on every
file but one.

## The projection is the load-bearing part, and it agrees for a reason

Every coordinate in every dataset goes through one RD (EPSG:28992) → WGS84
transform, and a projection that is subtly wrong does not fail: it writes a
complete, well-formed city half a metre to the left, and nothing downstream can
tell.

proj4js was checked against the `pyproj` in `/data/3dbag/venv` on the VPS — the
one that produced the tiles now being served. They agree to ~3e-10 degrees, about
0.02 mm. **They agree because neither has the NSGI grid.** PROJ on that box has no
`nl_nsgi_rdtrans2018.tif`, so it falls back to the seven-parameter Helmert, which
is what proj4js does unconditionally.

Install that grid next to PROJ and the two stop agreeing: PROJ would move to a
sub-centimetre grid shift and proj4js would not, so the same area extracted by
each would differ by a couple of decimetres. Nothing on the map would look wrong.
The only thing that would say so is the fixture test.

## One covered area per dataset, still

Each extractor rewrites the whole `coverage.json` for its dataset from the run
that just happened, so extracting a second city **replaces** the first rather than
adding to it — the first city's cell files stay on disk and the client stops being
told they are there.

This was inherited from the Python, where it was hidden behind an ssh command
somebody typed deliberately. A console with eight cities in a dropdown makes it a
button, so the console warns before doing it, and the API answers a start request
with the list of datasets it would displace. Merging coverage across areas is the
obvious fix and has not been made: the manifests would have to be read back and
unioned, and a cell listed in a manifest whose files were deleted is worse than a
cell that is not listed.
