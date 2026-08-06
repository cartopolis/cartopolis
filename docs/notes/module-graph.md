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

**26 mutual dependency cycles** exist between modules (22 after the `geo` crate
extraction on 2026-08-06 removed four). Rust crates cannot be
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
describes *where types were filed*.

That made hoisting them look like the cheap fix. It is not — see the next
section, which is the record of trying it. What the in-degree table above really
identifies is not five portable types but five *God-objects*, three of which fuse
shared state with the streaming machinery that maintains it.

## The hub types cannot simply be moved to a state layer — attempted 2026-08-06

The obvious fix for the cycles is to hoist the shared types into a layer nothing
depends back on. Tried it; it does not work as a move, and the reason is worth
knowing before someone budgets for it again.

**The hub "types" are not state. They are streamers wearing a `Resource` hat.**
Counting the fields that carry machinery — a `CellStream`, a `HashMap` keyed to
`Entity`, an asset `Handle`, a task or channel:

| Type | Fields | Of which machinery |
|---|---|---|
| `GlobeState` | 8 | **6** |
| `DataLayers` | 8 | **5** |
| `MapStream` | 9 | **5** |
| `DriverState` | 10 | 0 |
| `BuildingColliders` | 2 | 0 |
| `TerrainField` | 2 | 0 |

`GlobeState` reads as the cleanest of the three from its first few fields and its
`impl` — which is how the first attempt picked it — but it carries
`CellStream<TileKey, TileFetch>` and `HashMap<TileKey, Entity>`, and its methods
call `free_camera::WORLD_CELL_EDGE` and `utils::sun::smoothstep`. Moving it
produced 33 errors and had to be reverted. **Check a struct's fields, not just
its `impl`, before calling it portable.**

So the three highest-in-degree types are precisely the three that cannot move.
The cheap half — `DriverState`, `BuildingColliders`, `TerrainField` and friends —
is genuinely portable but only breaks two or three of the 22 cycles, because most
cycles have one of the big three on the other side.

The precondition is therefore a **split, not a move**: each hub has to be divided
into the state half everyone reads and the machinery half one system owns, the
same operation `vegetation` needed (870 pure lines / 130 ECS). Until that is
done, a state crate buys little. That is three non-trivial refactors ahead of the
one the ticket describes, and it is why phase 2 stalled here rather than
shipping.

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
