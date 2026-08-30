---
topic: 3d-basisvoorziening
triggers: [3D basisvoorziening, basisbestand_gebouwen, basisvoorziening-3d, Kadaster 3D,
  "3D Tiles", CityJSON, LoD3, "national LoD3", quantized mesh, roofer, "3d.kadaster.nl",
  lod22.rs, lod3.rs, hoogtestatistieken]
updated: 2026-08-30
---

# The Kadaster's 3D basisvoorziening: checked and not taken

The frontier row promised "national LoD3, in place of one hand-imported model".
**There is no LoD3 in it.** The buildings collection is 3DBAG — the same dataset
this client already streams — delivered a different way, and its ceiling is
LoD 2.2. Checked 2026-08-30 read-only against the live service.

## The interface

`https://api.pdok.nl/kadaster/3d-basisvoorziening/ogc/v1`, an OGC API, keyless.
Eight collections in two access shapes:

| Collection | What it is | Shape |
|---|---|---|
| `gebouwen` | buildings, "LoD 2.2, LoD 1.3 where AHN is missing" (its own description) | 3D Tiles 1.1, `.glb` leaves |
| `terreinen` | BGT ground surfaces draped on AHN | 3D Tiles 1.1 |
| `basisbestand_gebouwen` | the same buildings as CityJSON | sheet index → zip |
| `basisbestand_gebouwen_terreinen` | ground, water, roads (and, before 2025, buildings) | sheet index → zip |
| `hoogtestatistieken_gebouwen` | 2D footprints with height attributes, GeoPackage | sheet index → zip |
| `digitaalterreinmodel` | AHN6 DTM, Quantized Mesh for Cesium | tileset + a 12,228,609,219 B national zip |
| `digitaaloppervlaktemodel_20cm` / `_8cm` | DSM point clouds, LASZip | sheet index → zip |

The five `/items` endpoints are a **sheet index, not a feature service**: each
item is a polygon carrying `download_link` and `download_size_bytes`. There is no
per-building and no per-bbox feature query anywhere in the API. Since 2021 the
sheets are 2 km RD blocks (`232000_582000`); before that, 1:25,000 map sheets.
The download host answers **302** to `3d.kadaster.nl`, so `curl` without `-L`
gets a 5-byte body that reads as an empty dataset.

**Licence: CC BY 4.0**, stated by the API's own landing page as a `rel: "license"`
link to `creativecommons.org/licenses/by/4.0/deed.nl`. Redistributing a cached
extract is allowed, and it is the same licence the client already carries for
3DBAG (`crates/cartopolis/src/resources.rs:100`). **The licence was never the
obstacle.**

## The finding that settles it: this is 3DBAG, and 2.2 is the ceiling

`buildings_2025_232000_582000.zip` (Groningen centre), 15,737,752 B →
98,070,614 B of CityJSON 2.0. 29,325 objects — 14,665 `Building` + 14,660
`BuildingPart` — and 539,829 vertices. Every geometry in the file:

```
('Building',     'MultiSurface', lod '0'  ) × 14665     ← footprint
('BuildingPart', 'Solid',        lod '1.2') × 14660
('BuildingPart', 'Solid',        lod '1.3') × 14660
('BuildingPart', 'Solid',        lod '2.2') × 14660
```

Semantic surfaces are `WallSurface` 87,960, `RoofSurface` 77,161,
`GroundSurface` 43,980, **and nothing else**. No openings, no windows, no doors,
no facade detail.

The attributes name the producer: `identificatie: NL.IMBAG.Pand.…`,
`oorspronkelijkBouwjaar`, and 33 `rf_*` fields — `rf_pc_source: "AHN6"`,
`rf_pc_year: 2025`, `rf_roof_type`, `rf_ridgelines`, `rf_rmse_lod22`. Those are
**roofer** reconstruction attributes, i.e. the TU Delft 3DBAG pipeline, whose
stable output is forwarded to the Kadaster.

So this is the product already wired in.
`crates/cartopolis/src/systems/map/lod22.rs:1` is "3DBAG **LoD2.2** real building
meshes"; `docs/guides/server.md:485` routes `lod22/` from "3DBAG CityJSON
LoD2.2"; `docs/adr/027-3dbag-buildings.md:46` describes exactly this parse —
LoD2.2 `Solid`, drop the `GroundSurface`, triangulate Wall/Roof. The cell format
already carries the roof/wall split (`lod22.rs:7`) and the identificatie is
already the join key (`building_facts.rs:11`). Taking it would substitute one
delivery of one dataset for another delivery of the same dataset — **value 4,
"add, don't replace"** — and **value 6** agrees: no `--dump-state` count moves.

**And there is no LoD3 to be had elsewhere either.** A national Dutch LoD3 does
not exist; enriching LoD2 with facade openings from oblique imagery is current
research, not a published register. `lod3.rs:1`, the tier that streams one
hand-imported IFC model, has nothing to be replaced by.

## The other collections, each against what is already wired

- **`basisbestand_gebouwen_terreinen`** — the BGT draped on AHN. The 2025
  Groningen block is 14,100,639 B → 77,715,868 B, 32,647 objects, 656,457
  vertices: `LandUse` 21,341, `Road` 6,390, `PlantCover` 4,477,
  `OtherConstruction` 341, `WaterBody` 53, `Bridge` 45, **every one at LoD 1.2**,
  from `Wegdeel_vlak`, `OnbegroeidTerreindeel_vlak`, `Waterdeel_vlak` and their
  siblings. The client already streams those polygons
  (`crates/geo/src/paving.rs`, `systems/map/surveyed.rs`) and already lifts them
  on a measured AHN field. The z here is the *same AHN*, sampled at BGT vertices
  instead of on the client's 25 m grid (`systems/map/terrain.rs:63`, `:69`,
  `:76`) — median internal relief per `LandUse` polygon 0.15 m, p90 0.52 m. A
  resolution knob on `height.rs`, not a new source.
- **`hoogtestatistieken_gebouwen`** — footprints plus height statistics,
  joinable on `bagpandid`. The client has the geometry itself and the register
  facts beside it (`building_facts.rs`).
- **`digitaalterreinmodel`** — AHN6 DTM as Quantized Mesh, for Cesium. The client
  reads AHN directly from the WCS (`systems/map/height.rs:113`, `:114` —
  `dtm_05m` / `dsm_05m`).
- **DSM 8 cm / 20 cm** — raw LASZip point clouds. No consumer in this tree.
- **`gebouwen` / `terreinen` 3D Tiles** — visualisation deliveries of the two
  CityJSON collections, most-recent vintage only. A sampled leaf
  (`t/4/64/128.glb`) is 1,124,404 B, untextured, and requires
  `EXT_meshopt_compression` + `KHR_mesh_quantization` + `EXT_structural_metadata`
  — none of which bevy's glTF loader reads — so taking this shape means writing
  a 3D Tiles loader to arrive at meshes the client already has.

## Two pointers worth keeping

- **The combined file carries bridge decks with a real z.** 45
  `Overbruggingsdeel_vlak` objects in the Groningen block, one viaduct running
  0.96 → 6.62 m. `docs/direction.md` says the separate BGT bridge-decks ticket is
  blocked on "a deck height the register does not carry" — this is a place that
  height exists. It is still **not** a reason to take the source:
  [bridge-heights.md](bridge-heights.md) already measures the same clearance from
  AHN's own DSM/DTM pair, and the constraint there was never the height but the
  flat approach roads.
- **The 2025 combined block no longer carries buildings**, despite the
  collection description promising them. There are no `Building` or
  `BuildingPart` objects at all; the 14,867 `Pand_vlak` records are present as
  flat LoD 1.2 `LandUse` patches (median internal relief 0.15 m) with an empty
  `identificatiebag`. The 2022 block does carry them — 44,824,991 B →
  212,832,673 B, 14,367 `Building` + 14,263 `BuildingPart`, LoD ceiling 2.2
  again. Anyone reading the description and fetching the current vintage will
  find the buildings missing and it will look like a bug. (In the same file, the
  `PlantCover` `Solid`s are not canopy volumes either — 720 of the 4,477, median
  height 0.17 m, p90 0.41 m; they are `Vegetatieobject_vlak` ground patches.)

## How every number above was measured

All read-only, all run 2026-08-30 from the agent container.

```bash
# The eight collections, and the licence link on the landing page.
curl -s 'https://api.pdok.nl/kadaster/3d-basisvoorziening/ogc/v1/collections?f=json'
curl -s 'https://api.pdok.nl/kadaster/3d-basisvoorziening/ogc/v1?f=json' \
  | python3 -c 'import json,sys;print([l for l in json.load(sys.stdin)["links"] if l["rel"]=="license"])'

# The sheet index over Groningen: download links and sizes, not features.
curl -s 'https://api.pdok.nl/kadaster/3d-basisvoorziening/ogc/v1/collections/basisbestand_gebouwen/items?f=json&limit=200&bbox=6.55,53.21,6.58,53.23'

# The blocks themselves. -L is required: the download host answers 302.
curl -sL -o geb.zip   'https://download.pdok.nl/kadaster/basisvoorziening-3d/v1_0/2025/gebouwen/buildings_2025_232000_582000.zip'
curl -sL -o vol25.zip 'https://download.pdok.nl/kadaster/basisvoorziening-3d/v1_0/2025/volledig/volledig_2025_232000_582000.zip'
curl -sL -o vol22.zip 'https://download.pdok.nl/kadaster/basisvoorziening-3d/v1_0/2022/volledig/volledig_2022_232000_582000.zip'
```

Every tally is one pass over the decompressed CityJSON:

```python
import zipfile, json
from collections import Counter
z = zipfile.ZipFile('geb.zip')
cj = json.loads(z.read(z.infolist()[0].filename))        # decompressed size = infolist()[0].file_size
objs = cj['CityObjects']
Counter(o['type'] for o in objs.values())                # object types
Counter((o['type'], g['type'], str(g.get('lod')))        # the LoD tally
        for o in objs.values() for g in o.get('geometry', []))
Counter(s['type'] for o in objs.values() for g in o.get('geometry', [])
        for s in (g.get('semantics') or {}).get('surfaces', []))
Counter(k for o in objs.values() for k in o.get('attributes', {}))   # the rf_* block
Counter(o['attributes'].get('tabelnaam') for o in objs.values())     # BGT source tables
```

Internal relief per polygon is `max(z) − min(z)` over the vertex indices its
boundaries reference, with `z = vertices[i][2] * transform['scale'][2] +
transform['translate'][2]` (CityJSON stores quantised integers). The 3D Tiles
leaf is `GET <collection>/3dtiles?f=json`, then walk `root.children[*].content.uri`
and fetch one `.glb`; its glTF JSON chunk starts at byte 20.
