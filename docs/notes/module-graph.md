---
topic: module-graph
triggers: [crate split, circular dependency, systems/, resources.rs, hub module, crate naming, core crate]
updated: 2026-08-06
---

# The client's modules are coupled by state, not by behaviour

Measured 2026-08-06 over `crates/cartopolis/src/systems/*.rs` (50 files) by
extracting every `crate::systems::<m>` / `super::<m>::` reference into an edge
list, then computing in-degree and mutual pairs.

## The shape

> **Correction, 2026-08-13.** Re-derived over 57 modules: **18** mutual pairs, not
> 17 — but the pair count is the wrong metric and always was. The blocker is that
> **31 of the 57 modules form a single strongly-connected component**, so no member
> of it can be extracted at any granularity, and unpicking "17 pairs" would not
> have touched it. New mutual edges since 2026-08-06 include `avatar↔user_store`,
> `data_layers↔user_store`, `session↔user_store`, `collision↔data_layers`,
> `driver↔navigation`, `geo_nav↔navigation`, `globe↔osm_buildings` and
> `globe↔tile_loader`. The hubs moved too: `free_camera` fell from in-degree 25 to
> 4 (the `FreeCamera` marker move worked), and the top two are now `egui_manager`
> (21) and `theme` (20) — **both outside every cycle**. So the graph's centre of
> gravity is egui, not Bevy, which is the same conclusion
> `client-build-cost.md` reaches from the compile side.

**26 mutual dependency cycles** existed between modules when this was written.
2026-08-06 took it to 17: the `geo` extraction removed four, fixing the tile
pipeline's layering removed two more (see below), and moving the 23 marker
components back to `components.rs` removed three. Rust crates cannot be
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
| `free_camera` | 25 | `FreeCamera` (19), `WORLD_CELL_EDGE` (10) — `FreeCamera` is a *unit struct*, and moving it to `components.rs` on 2026-08-06 un-hubbed the module |
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

## What did work: markers, and correcting a layering inversion

Two things came out cheaply and are worth distinguishing from the state-crate
attempt above.

**The 23 marker components.** Zero-sized `#[derive(Component)] pub struct X;`
tags with no fields and no behaviour, declared across 17 files next to whichever
system spawned them. `components.rs` already existed for them. Moving them
removed three cycles at zero risk, because a marker has no state, no ordering
and no representation to get wrong. The instructive one is `FreeCamera`: the
single highest-in-degree type in the client, named by 25 modules, is a unit
struct — a quarter of the module graph pointed at the camera module to write
`With<FreeCamera>`.

**The tile pipeline was genuinely inverted**, not merely misfiled.
`tile_source` owns bytes and `tile_loader` owns images, but the URL vocabulary
(`format_osm_tile_url`, `tile_source_kind`) lived in `tile_loader`, so the byte
layer imported upward to decide what to fetch. Those moved to `core::world`
beside `TileKey` and `DEFAULT_TILE_URL_TEMPLATE`; `build_mip_chain` (pure) went
to `cartopolis_geo::mip` and `image_from_mip_chain` (Bevy) to `tile_loader`. The
pipeline now reads one way: core -> tile_source -> tile_loader -> rendering.

The distinction that matters when picking the next one: both of these moved
things that have **no runtime semantics** — types with no state, and pure
functions. The remaining cycles mostly do not.

## This codebase orders by change detection, not by schedule position

Worth knowing before proposing ordering edges for freshness, because the
measurement looks alarming and the conclusion is the opposite.

`MapStream`'s anchor is written by two systems and read by 23, and 17 of those 23
have **no ordering relation to `apply_reanchor`** — on a re-anchor frame Bevy is
free to run them either side of it. That reads like a latent bug. It is not.

`apply_reanchor` moves the anchor *and* retires the tiles in one system, so a
reader that runs before it sees old-anchor-plus-old-world and is self-consistent;
one that runs after sees new-and-new. Neither is wrong. And every consumer that
builds geometry compares generations rather than assuming freshness —
`osm_buildings`, `lod22`, `map_geometry` and `data_layers` call
`stream.sync(anchor_generation)`, `weather` keeps `fetched_gen`, `prim_geo` holds
a `Local` copy of the last anchor, `navigation` keys its cache on
`(route.generation, anchor_generation, terrain.revision)`. The same shape as
`TerrainField::revision`.

So the 17 `.after()` edges were rejected: they constrain the schedule for no
behavioural gain, and two of the candidates (`apply_teleport`,
`restore_start_location`) sit *upstream* of `apply_reanchor` and would have made
the graph cyclic — a startup panic.

The corollary is where the real risk lives: not in ordering, but in a consumer
that forgets to compare a generation, or in the publisher failing to bump one.
`MapAnchor` shipped with exactly that fault for one commit — `apply_reanchor`
stopped republishing and nothing failed, because no reader checks freshness.

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

**It is the lib target that shadows, not the package** — an earlier version of
this note said the *package* had to be renamed, which is wrong. Verified
2026-08-06: a package named `core` whose `[lib] name` is something else compiles
and its dependents resolve `use core::f32::consts::PI` to the sysroot as normal.
So `cargo run -p core` was available all along.

It was not taken. The workspace uses `cartopolis_core`, `cartopolis_geo`,
`cartopolis_simulator` and `cartopolis`, because a package whose name and lib name
disagree is a wrinkle every reader has to look up, and prefixing everything
removes the question instead of answering it. The crate names barely appear in
code anyway — `cartopolis_geo` is named twice in the whole tree, since
`systems/mod.rs` re-exports its modules flat and call sites write
`systems::vector_tiles`.

Cargo does not require directory and package names to match, so the directories
stay short: `crates/core`, `crates/geo`, `crates/simulator`. `crates/big_space`
keeps its upstream name — it is a vendored fork kept diff-comparable, and
renaming it would work against the re-sync it exists for. The simulator's *binary*
also stays `simulator`: it is what a user runs, and never appears in a dependency
graph or a `-p` flag.
