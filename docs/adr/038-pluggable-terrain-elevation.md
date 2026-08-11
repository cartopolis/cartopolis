# ADR-038: Pluggable Terrain Elevation (Height-Field Sources, AHN First)

---
**Metadata:**
- **ID**: ADR-038
- **Status**: Proposed
- **Date**: 2026-07-23
- **Tags**: [terrain, elevation, dem, ahn, pdok, map, vector, rendering, geometry, collision, providers, dependency, ops]
- **Decision Type**: architecture_pattern
- **Aligns with**: [G-04, G-02, P-03]
- **Roadmap tier**: P1
- **Doc resolution**: R2
- **Related**: [ADR-035, ADR-030, ADR-027, ADR-022, ADR-020, ADR-019, ADR-025, ADR-034, ADR-033, ADR-014]
- **Supersedes**: []
- **Superseded By**: []
---

## Context

**Problem**: The map tier is flat. ADR-035 moved the basemap from raster photographs to
tessellated vector geometry (`systems/vector_tiles.rs`, `systems/map_geometry.rs`), but every
surface it produces sits at `y = 0` with a millimetre stacking offset per `SurfaceGroup` to keep
the layers from z-fighting. A canal is a blue polygon painted on a plane; a dijk is invisible; a
grass verge has no camber. In a world whose point is being *situated* (G-04), the ground is the
one surface that never behaves like ground.

Two smaller problems ride along:

1. **3DBAG building heights are already absolute NAP** (ADR-027). Drawing them on a plane at
   `y = 0` is not merely "missing relief" — it is a latent datum inconsistency between two
   datasets that are meant to agree.
2. **Ground height is assumed, never queried.** `geo_nav::AVATAR_GROUND_Y`, the gravity clamp in
   `avatar::handle_avatar_movement` and `cartopolis_simulator::state`'s spawn `y` each hard-code a constant.
   Nothing in the codebase can answer "how high is the ground here?".

**What resolution this needs.** The features wanted — canal banks, ditch profiles, dijk slopes,
the crown of a road — are 3–15 m wide. A global 30 m DEM cannot express them (a 6 m canal is a
fifth of a pixel), and Copernicus GLO-30 is additionally a *surface* model containing trees and
rooftops. Sub-metre bare-earth data is not a nicety here; it is the feature.

**But sub-metre bare-earth data is national, not global.** It exists because a country flew
LiDAR over itself. The Netherlands has AHN; other regions have their own, with different
formats, resolutions, licenses, vertical datums and update cadences — and most of the planet has
nothing better than a coarse global DSM. Any design that treats "the DEM" as a singular thing
will be wrong the first time we leave NL. **The elevation source has to be a plug, and having no
plug at all has to be the ordinary case, not the error case.**

**Requirements**:
- Sub-metre bare-earth elevation where a region provides it; graceful, seam-free degradation to
  coarse data or to flat where it doesn't.
- Adding a region must not require client changes — new source, new coverage, new format,
  configuration only.
- **No source at all is the default and must stay the tested path.**
- The *whole* ground — water, landuse, roads — displaced consistently, no cracks or z-fighting
  between adjacent surfaces on a slope.
- One vertical datum across sources, and shared with 3DBAG's building heights.
- Per-frame bounded streaming and a triangle budget that survives the iGPU (the documented
  `VK_ERROR_DEVICE_LOST` pattern, CLAUDE.md).
- Ground height becomes a queryable function for collision, geo-nav and avatar placement.
- Self-hosted and same-origin per ADR-034/035; no third party in the per-tile runtime path.

## Decision

**Terrain is a registry of interchangeable height-field sources, normalised at ingest to one
canonical tile format and datum, sampled by the renderer into a single welded ground
triangulation per cell. AHN is the first source; flat is the default.**

The client knows nothing about AHN, PDOK, LiDAR or the Netherlands. It knows how to sample a
height tile.

### 1. `TerrainField` — the runtime contract (ADR-030 substitutable provider)

One resource, two operations:

- `height(lat, lng) -> Option<f32>` — canonical metres (below), bilinear within a tile.
- `coverage(cell) -> Option<&TerrainSource>` — which source, if any, owns this cell.

`None` means *no data*, and every consumer falls back to the flat plane by construction rather
than by special case. `None` is not degraded mode; for most of the planet it is the answer.

### 2. `TerrainSource` — the descriptor, declared in configuration

A source is a **config entry** (ADR-014 layering), not a code path:

| Field | Purpose |
|---|---|
| `id`, `name` | identity |
| `url_template` | `z/x/y` into our own origin |
| `coverage` | bboxes, or a low-zoom coverage bitmap for ragged extents |
| `native_resolution_m` | drives Steiner-point density (below) and max useful zoom |
| `max_zoom` | depth of that source's pyramid |
| `priority` | which wins where two sources overlap |
| `kind` | bare-earth (DTM) vs surface (DSM) — affects whether buildings double up |
| `attribution`, `license` | rendered in the credits; non-optional |

Per cell the client picks the highest-priority covering source, or none. **Adding a region is a
manifest entry plus an ingest run.** No client release, no code.

### 3. Canonical encoding and datum — normalise at ingest, not at runtime

The runtime speaks exactly one format. Everything heterogeneous is absorbed by the offline step.

- **Encoding**: Terrarium-encoded PNG/WebP, Web Mercator `z/x/y`. Chosen for boring reasons — it
  is an image, so it rides the existing `tile_loader` fetch/cache path and
  `image::load_from_memory` decode; it is pure Rust and wasm-safe with no C dependency
  (ADR-033); and its 1/256 m quantisation (~4 mm) is far below any source's real accuracy.
- **Vertical datum**: **orthometric metres above EGM2008**. Each source declares its *native*
  datum (NAP, NN2000, NAVD88, ellipsoidal, …) and ingest converts through a geoid grid. This is
  the part that must not be NL-shaped: NAP is one country's local realisation, and hard-coding it
  into the contract would make the second source a rewrite instead of a config entry.
  **3DBAG's NAP building heights get the same conversion** (ADR-027), so buildings and terrain
  still share a datum — just not a Dutch one.
- **Horizontal**: reprojected at ingest. Note this matters more than it used to — a naive
  RD→WGS84 approximation is ~0.5 m off, irrelevant against a 30 m DEM and very relevant against
  0.5 m AHN. Use a proper RDNAPTRANS-grade transform per source.

The ingest tool is a set of **input adapters** — COG GeoTIFF, ASCII grid, LAZ point cloud, or
passthrough for a source already published as Terrarium — emitting the canonical pyramid. A new
region needs an adapter only if it brings a format we haven't met.

### 4. The first source: AHN (Netherlands)

AHN is published by PDOK as **CC0**, bare-earth DTM at 0.5 m, in Cloud Optimized GeoTIFF tiles
of 5 × 6.25 km, bulk-downloadable over ATOM (`service.pdok.nl/rws/ahn/atom/dtm_05m.xml`). AHN5
is the 2026 release and replaces AHN4 in place. We carry it to z16–z17, where a texel is ~0.5 m
at NL latitudes — public Terrarium sets stop at z15, which is why we generate our own.

Served from garage44 behind the same Caddy as 3DBAG, Nominatim and the MVT tiles: the ADR-027
pattern again. We do **not** call PDOK per tile at runtime — same reasoning as ADR-035.

A coarse global source (Copernicus GLO-30 via Mapterhorn/AWS Terrarium) is an obvious **second**
entry for mountains and coastlines. It is a manifest entry at lower priority, not a design
change, and it is explicitly not what delivers this ADR's goal.

### 5. Water: enforced at ingest, not carved at runtime

LiDAR gets no return from water. In a DTM every canal, ditch and pond is a void filled by
interpolation, so the sampled surface is roughly the *water level*, flat — banks real and sharp,
channel a hole. This is true of every LiDAR-derived product.

The height field is therefore simply **wrong** over water, and the place to fix a wrong height
field is the height field. At ingest we rasterise the water polygons into the DEM and lower them,
leaving the banks — standard DEM hydro-enforcement. The runtime then does one thing: sample the
field and displace the mesh. **There is no water case in the renderer.**

Two consequences to accept:

- It **couples the elevation pipeline to a vector water source**. AHN refreshes in years, OSM in
  days, so a canal dug since the last burn renders flat until re-ingest. That is a new re-ingest
  trigger.
- It does **nothing where there is no DEM** — a flat field has no raster to burn. Outside
  coverage, water is flat. Accepted: the alternative is a runtime carving path that exists solely
  for uncovered regions, and that is not worth a permanent special case in the renderer.

The one genuinely water-specific thing that remains is the **water surface level** — a
single-valued height field cannot hold both bed and surface at the same point. That is one number
per water body applied to the existing `SurfaceGroup::Water` mesh, which sits above the enforced
bed. Small and contained.

### 6. One welded ground TIN per cell

This is the part that is not "sample a DEM per vertex", and the reason the ADR is R2.

Displacing each `SurfaceGroup`'s tessellation independently **cracks the map**. Coincident
*vertices* land at the same height; coincident *edges* do not. A road polygon's edge may be one
long segment where the grass beside it has a vertex halfway along — identical on flat ground, and
on a slope the straight edge and the two-segment edge diverge, opening a gap and a z-fighting
seam along every land boundary in the scene.

So `tessellate_surfaces` changes from *N independent triangulations* to **one constrained
Delaunay triangulation per cell over all ground polygons together**, with height-field samples
inserted as Steiner points, then split into per-`SurfaceGroup` meshes by which source polygon each
triangle fell in. Watertight by construction.

**Steiner density comes from the source descriptor**, not a constant: a 0.5 m source earns dense
sampling, a 30 m source must not generate 0.5 m grids, and a `None` cell generates no interior
points at all and reduces exactly to today's flat tessellation. This is what keeps a
mixed-coverage world inside one triangle budget.

Two things fall out, both good:

- **The `SurfaceGroup::y_offset()` / `depth_bias()` millimetre stack retires** for ground
  surfaces. With one shared triangulation nothing is coplanar and nothing overlaps — each
  triangle belongs to exactly one group. Hand-tuned biases become a structural guarantee.
- **Buildings stand on the terrain**, sampling the same field at the footprint.

Roads are the awkward case: Shortbread serves them as *lines*, stroked into ribbons, and a ribbon
is hard to weld into a TIN. Interim: displace the ribbon by the same field and accept a small
mismatch at kerbs.

### 7. Seams between sources

Where a fine source meets a coarse one, or any source meets `None`, the height steps. Blend over
a feather band at the coverage boundary, driven by the descriptor's declared accuracy. The
`None` boundary is the sharpest case — real elevation dropping to a plane — and is the one to
test first, because it is the boundary most users will actually see.

### Downstream: better vector sources per region (noted, not decided)

The same pluggability question applies one layer up. PDOK serves **BGT** as MVT over an OGC API
(`https://api.pdok.nl/lv/bgt/ogc/v1`): 20 cm national topography with roads as *polygons*
(`wegdeel`) rather than lines, which would fix the road-ribbon problem properly. It is a natural
fit for the TIN and for regional substitution generally.

**Not decided here** — it splits the vector source by region, doubles the styling surface
(`PAINT_ORDER` is Shortbread-shaped), and lands mid-flight in ADR-035's workstream. Revisit once
the TIN exists and can be judged against a real source.

## Rationale

**Primary Reasoning**:

1. **Sub-metre bare-earth data is inherently national**, so the source must be a plug and "no
   source" must be ordinary. Designing around a single DEM would be wrong the first time we leave
   the Netherlands.
2. **Normalising at ingest keeps the runtime small.** One encoding, one datum, one sampling path.
   Format, projection and datum diversity is absorbed by an offline tool where it costs nothing
   per frame and nothing in wasm binary size.
3. **Cracks are the real engineering problem**, not fetching a DEM. Any design that displaces
   polygon groups independently fails on the first slope; deciding the welded-TIN structure now
   avoids building the wrong version first.
4. **Water depth is a data defect, not a rendering feature.** Fixing it at ingest removes a whole
   special case from the renderer, which is worth more than the uncovered-region case it gives up.
5. **It simplifies as much as it complicates**: the tuned millimetre offset stack is replaced by a
   structural guarantee, and three hard-coded ground constants become one queryable function.
6. **It fixes an existing datum inconsistency** between 3DBAG's absolute heights and a ground
   plane at zero.
7. **It follows the established shape** — preprocess offline, serve static from garage44 behind
   the same Caddy, decode in pure Rust on the tile worker pool.

**Alternatives Considered**:

| Alternative | Pros | Cons | Rejected Because |
|------------|------|------|------------------|
| **Hard-code AHN as "the" DEM** | Simplest possible; ships fastest | Every other region is then a rewrite; NAP leaks into the runtime contract | The requirement is explicitly that other regions plug in their own or nothing |
| **Global DEM only** (GLO-30 / Mapterhorn / AWS Terrarium) | Worldwide; no per-region ops; PMTiles ready-made | 30 m posts, and a **DSM** — trees and roofs, not bare earth | Cannot represent a 6 m canal or a verge slope at all. Kept as a **low-priority registry entry**, not the answer |
| **Kadaster 3D Basisvoorziening — CityJSON terrain** (BGT polygons already triangulated with AHN heights) | Effectively this ADR's output, pre-built and classified; solves cracks and roads for us | Bulk download *op aanvraag*; current jaargang only; annual cadence; NL-only; license unconfirmed; a pipeline we don't control — and a per-region format the plug model exists to avoid | A dependency to take on rather than a pipeline we own. **Kept as the validation reference** to diff our TIN against |
| **3D Basisvoorziening — Quantized Mesh DTM** | Ready-made LOD terrain pyramid, well-specified | Same access constraints; a *separate* terrain surface our polygons must then be draped onto, reintroducing welding at another layer | Doesn't remove the hard part; adds a second geometry source |
| **PDOK WCS / COG reads at runtime** | No preprocessing | Third party in the per-tile path; RD projection and COG range reads on the client; contradicts ADR-034/035 | Same reasons ADR-035 self-hosts the MVT tiles |
| **Vertex-shader displacement from a height texture** | No CPU tessellation change; cheap | Needs dense uniform subdivision anyway to have vertices to move; leaves cracks untouched; gives collision and geo-nav nothing (they need a CPU query) | Solves the easy half and none of the hard half |
| **Runtime water carving from vector polygons** | Works with no DEM, so canals appear worldwide | A permanent water special case in the renderer, existing only for uncovered regions; duplicates what ingest already does where there *is* a DEM | Fix the height field at ingest instead; uncovered regions get flat water |
| **Stay flat** | Zero work, zero risk | Ground never behaves like ground; 3DBAG datum mismatch persists | This is the request |

## Consequences

**Positive**:
- Canal banks, dijks, cuttings and grass slopes are visible, lit and shaded by the existing sun
  and atmosphere — ADR-035's "geometry, not wallpaper" extended to the third axis.
- **Regions are pluggable**: another country's LiDAR is a manifest entry plus an ingest run;
  no source is the default and needs no configuration at all.
- One welded TIN per cell: no cracks, no z-fighting, and the `y_offset`/`depth_bias` stack
  retires.
- No water case in the renderer.
- Buildings and terrain share one datum, and it is not a national one.
- Ground height becomes a real query for collision, geo-nav, avatar spawn and camera.
- Self-hosted, same-origin, pure-Rust decode, wasm-safe; no new runtime dependency.

**Negative**:
- **Triangle budget.** A z14 cell at 53° N is ~1.46 km; a uniform 0.5 m grid is ~8.5 M vertices
  per cell — the documented device-loss pattern with extra steps. *Mitigation*: Steiner density
  is derived from the source's declared resolution, the CDT is adaptive, and the existing
  `MAX_BUILDS_PER_FRAME` / `MAX_DESPAWNS_PER_FRAME` caps apply. A per-cell triangle ceiling is a
  hard implementation requirement.
- **Seams at every coverage boundary**, most visibly where real terrain meets flat.
  *Mitigation*: feather band sized from declared accuracy; test the `None` boundary first.
- **Mixed DTM/DSM sources behave differently under buildings** — a DSM already contains the
  rooftops we then extrude on top. *Mitigation*: the `kind` field exists so a DSM source can be
  flagged and its building bases handled (or the source demoted).
- **Hydro-enforcement couples the elevation pipeline to a vector water source**, and adds a
  re-ingest trigger on vector refresh. *Mitigation*: cadence mismatch shows as a flat canal, not
  a broken one.
- **Water depth is invented.** Plausible, not surveyed; it will not match a bathymetric chart.
  Unavoidable — no LiDAR product has it either.
- **The sim has no terrain.** `cartopolis_simulator` stays authoritative for avatar position with no height
  field, so ground-following is client-side prediction — the split already documented for
  building collision (`systems/collision.rs`). *Mitigation*: reuse that pattern.
- **Another preprocessing pipeline and dataset to host**, per region. *Mitigation*: same tooling
  shape, same Caddy; AHN's refresh cadence is years.
- **Attribution is per source and mandatory**, and now varies by where the camera is.
- **Three hard-coded ground constants must change together** (`geo_nav::AVATAR_GROUND_Y`, the
  gravity clamp in `avatar::handle_avatar_movement`, `cartopolis_simulator::state`'s spawn `y`) — the trio
  CLAUDE.md already flags as easy to miss.
- **Roads mismatch at kerbs** until a polygon road source or a road-aware weld lands.

**Phasing** (each step independently shippable and visible):

| Phase | Content | Depends on |
|---|---|---|
| **1** | `TerrainField` + `TerrainSource` registry with **only** the flat source — proves the no-data path and changes nothing visually | — |
| **2** | AHN ingest → canonical pyramid on garage44; displace existing per-group tessellations and building bases | 1 |
| **3** | Hydro-enforcement in ingest; water surface level per body | 2 |
| **4** | Welded CDT per cell; retire the `y_offset`/`depth_bias` stack | 2 |
| **5** | Ground-height query wired into collision, geo-nav, avatar spawn | 2 |
| **6** *(optional)* | Coarse global source as a second registry entry; per-region vector sources (BGT) | 4 |

Phase 1 first is deliberate: the substitutable path is the one that must not rot, and building it
before any real data exists is the only way to be sure the flat case is a first-class path rather
than a fallback nobody exercises.

## Related

- [ADR-035](./035-self-hosted-map-tiles.md): This is its Z axis. The tessellation, styling and
  streaming modified here are introduced there; the "self-host, preprocess offline, style
  client-side" stance is inherited wholesale.
- [ADR-030](./030-platform-capability-model.md): `TerrainField` is a substitutable provider and
  the registry is the capability-model pattern applied to data coverage rather than to platform.
- [ADR-027](./027-3dbag-buildings.md): The hosting precedent, *and* the datum counterpart — its
  NAP heights are converted by the same ingest rule.
- [ADR-022](./022-osm-building-import.md): OSM footprints gain a ground height instead of an
  assumed zero.
- [ADR-020](./020-map-tier-and-sim-tier-world-model.md): The map tier's surface stops being a
  plane; the tier split is unchanged.
- [ADR-019](./019-planet-scale-floating-origin.md): Canonical heights are absolute and sampled in
  `f64`, then expressed in the floating-origin frame like everything else.
- [ADR-014](./014-runtime-configuration-and-operations.md): The source registry is configuration,
  which is what makes a new region a config change.
- [ADR-034](./034-public-data-relay.md): Height tiles are served locally, not proxied.
- [ADR-033](./033-build-targets-and-wasm-packaging.md): Terrarium PNG/WebP keeps the decode path
  pure Rust and wasm-safe.
- [ADR-025](./025-planetary-globe-and-space-tier.md): Unaffected today — terrain is a map-tier
  feature; a coarse global source would be the first thing the globe tier could use.
