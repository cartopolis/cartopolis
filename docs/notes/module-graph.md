---
topic: module-graph
triggers: [crate split, circular dependency, systems/, resources.rs, hub module, crate naming, core crate]
updated: 2026-08-06
---

# The client's modules are coupled by state, not by behaviour

Measured 2026-08-06 over `crates/viberfox/src/systems/*.rs` (50 files) by
extracting every `crate::systems::<m>` / `super::<m>::` reference into an edge
list, then computing in-degree and mutual pairs.

## The shape

**26 mutual dependency cycles** exist between modules. Rust crates cannot be
cyclic, so each one is a hard blocker for any crate split — this is the reason a
split cannot simply be done, and it is worth knowing before proposing one again.

But the cycles are not behavioural. Dumping both directions of each shows the
same pattern almost everywhere — **a type or constant one way, a function the
other**, and in several cases data in *both* directions:

| Cycle | One way | The other |
|---|---|---|
| `avatar ↔ collision` | `BuildingColliders` (type) | `AIRBORNE_EPS` (const) |
| `data_layers ↔ terrain` | `TerrainField`, `TerrainSnapshot` | `DataLayers` |
| `globe ↔ map_stream` | `MapStream`, `StreamTile` | `GlobeState`, `HideInSpace` |
| `lod22 ↔ osm_buildings` | `OsmBuildings`, `CACHE_ZOOM` | `BACKFACE_CULL`, `LOD22_RADIUS` |
| `driver ↔ hud` | `HUD_WIDTH`, `render_map_controls` | `DriverState`, `MapOrientation` |
| `rendering ↔ tile_loader` | `RegionTileTexture` | `build_mip_chain` |
| `vector_tiles ↔ road_texture` | `RoadClass` | `render_mvt`, `tessellate_roads` |

## Why: the hubs are imported for one type each

The highest in-degree modules are not depended on for their logic. They are
depended on because a shared state type happens to be declared in the same file
as the systems that drive it:

| Hub | In-degree | What importers actually name |
|---|---|---|
| `free_camera` | 25 | `FreeCamera` (19), `WORLD_CELL_EDGE` (10) |
| `map_stream` | 24 | `MapStream` (26) |
| `globe` | 20 | `GlobeState` (34), `HideInSpace` (12) |
| `data_layers` | 17 | `DataLayers` (18) |
| `egui_manager` | 16 | `EguiManager` (17) |

`resources.rs` (28 types) and `components.rs` (10 types) were meant to be the
state layer, but **33 of the 50 system files also declare `Resource` /
`Component` types**, and `systems/` carries 136 public structs in total. The
layer exists; it was abandoned partway.

The consequence is that the dependency graph does not describe the design. It
describes *where types were filed*. Hoisting shared types and constants out of
`systems/` breaks most of the 26 cycles mechanically, without changing any
system's behaviour — that is the cheap part of any future split, and the reason
the mesh looks worse than it is.

## A crate named `core` is not available

Verified 2026-08-06 with a throwaway workspace, not reasoned from docs. A
package named `core` builds fine on its own, but its lib target shadows the
sysroot `core` in **every dependent crate**:

```
error[E0433]: cannot find `f32` in `core`
 --> src/lib.rs:1:11
  |
1 | use core::f32::consts::PI;
```

Any `core::` path in a dependent breaks, including paths that macros expand to.
`simulator` has no such conflict. If `crates/core/` is wanted as a directory
name, the package must be named something else (`viberfox_core`) — Cargo does
not require the directory and package names to match.
