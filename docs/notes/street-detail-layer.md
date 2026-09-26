---
triggers: street detail, overpass features, surveyed trees, street lamps, traffic
  signals, zebra, barriers, piers, wind turbines, windmills, building parts
---

# The Overpass street-detail layer

`systems::street_detail` + `systems::tall_structures` (2026-08-21) stream the
close-range and landmark-scale things the vector tiles do not carry: surveyed
trees (`natural=tree`/`tree_row`), street lamps, traffic signals, zebra paint,
barriers (wall/fence/hedge/retaining/guard rail), piers/lock gates/weirs, a few
props the tile `pois` layer lacks (bicycle racks, billboards, bollards), and —
on a coarse z11 ring out to ~±30 km — wind turbines, windmills, masts, towers,
chimneys, water towers and cranes. `osm_buildings` additionally fetches
`building:part` ways and drops an outline's own extrusion where parts cover
half its area (detail tier only; the far tier keeps outlines and skips parts).

**The tall ring is pre-cut by atlas where it has run** (2026-09-26): its `tall`
dataset asks the same z11 query (`cartopolis_geo::tall::query`) ahead of time
and writes the answers to `3dbag.cartopolis.org/tall/11/{x}/{y}.json`, and the
client reads a covered cell from there under the same cache key
(`where-the-map-data-is-built.md`). Everything else here is still live Overpass.

Standing facts, several of which a diff will not explain:

- **The street-detail data cannot ride the building cell query.** 3DBAG-covered
  cells never reach Overpass at all (`fetch_cell_text` short-circuits to the
  static host), so anything appended to that query would silently not exist in
  exactly the covered Dutch cities the client is pointed at. Hence a separate
  query family with its own cache namespace (`cache/features/v1/…`,
  `cache/features/tall/v1/…`). The building cache moved to `buildings/v2/…`
  when the query grew `building:part` — an old cached cell would otherwise
  lack parts forever, because the cache key carries no query hash.
- **Zebra/signal orientation comes from the query, not from guessing.**
  `way(bn.sig)` / `way(bn.zeb)` return the highway ways *containing* each
  node, and the node id's position in the way gives the exact road direction
  and a width estimate. A crossing node mapped only on the footway is handled
  by rotating 90° (the footway is the walking direction). Nearest-road
  distance search was never needed.
- **Zebra stripes are elongated along the walking direction** and repeat along
  the road axis (five bars, 0.9 m pitch) — look at a photo before "fixing"
  this; both orientations look plausible in code.
- **Signals are six materials, not per-entity state.** Every signal lamp ball
  is meshed into one of six per-cell meshes — (phase group A|B) × (red, amber,
  green) — and `update_signal_cycle` writes six emissives on phase
  transitions. Group = quantised approach bearing, so opposing arms alternate
  like a real junction. Cost is constant in the number of signals.
- **The lamp point lights are a fixed pool, not per-lamp entities.** They are
  the first clustered lights in the scene; `ForceCpuClusteringPlugin` notes
  CPU clustering is cheap only "for a handful of clustered lights", and on the
  web it is the most expensive thing in the frame. `lamp_pool_size()` (48
  desktop / 16 balanced / 0 phone) is a structural cap: `update_lamp_lights`
  re-aims pool entities at the nearest lamps at night. Lamp *glow* (emissive
  heads) is separate and unpooled — every head glows, only lights are capped.
- **Surveyed trees ride the scatter pipeline on purpose** — they become
  `TreeInstance`s, share the Canopy/Trunk materials via
  `MapSurfaces::group_material`, and carry `VegetationSurface`, so the
  altitude fade and the Trees toggle treat them exactly like scattered trees.
  No dedup against the scatter is attempted: overlap exists only inside
  parks/forest polygons, and reads as "more trees", not as an artefact.
- **Guard rails draw but do not collide** (`BarrierKind::solid()`): shin-high
  steel is stepped over everywhere in real life, and a knee-high invisible
  wall along every N-road would be worse than no rail. Other barriers publish
  thin rectangle footprints per segment into `BuildingColliders`' feature
  channel, dropped with their cell. Building parts raised ≥ 2.2 m (archways,
  skybridges) are likewise excluded from colliders — the mapper deliberately
  opened that passage.
- **Turbine yaw is the prevailing south-westerly and never re-aims.** The live
  wind drives only rotor *speed* (`spin_rotors`); re-posing every nacelle when
  the observation veers would rotate the world for a number nobody checks.
  Becalmed turbines still idle slowly — a stopped wind farm reads as broken.
- **Piers/weirs/lock gates take one terrain level for the whole structure**
  (the field under their first vertex): a deck is straight, and over water the
  DTM is roughly the surface. Flat regions get the old y = 0 behaviour because
  `TerrainSnapshot::at` answers 0 there.
- **Fades:** small props (lamps' posts, signals, racks, paint) fade on the
  street-furniture band (150–450 m) by walking the shared vertex-colour
  material toward grey and hiding past the end; linear features (barriers,
  piers) and lamp heads use a longer band ending at the trees' 2 km. Tall
  structures never fade — being visible from afar is their entire point; they
  hide with everything else in space (`HideInSpace`).
- The whole layer is one Data Layers toggle (**Map ▸ Street detail**), seeded
  from `Budget::detail_layers` (so off on the phone rung), persisted in
  `LayerPrefs`, stripped by driver mode's minimal map, and torn down through
  the bounded despawn queue on the off-edge like the Buildings master toggle.
  `--dump-state` carries `prop_meshes`; `busy` gains `street_detail` and
  `tall_structures`, so a settle-gated capture waits for them.
