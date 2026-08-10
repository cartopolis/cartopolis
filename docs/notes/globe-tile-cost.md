---
topic: globe-tile-cost
triggers: [globe.rs, update_globe_tiles, qt_segs, GLOBE_TILE_PX, QT_LIFT_PER_Z, QT_MAX_TILES, tile_px, "globe is slow", "globe artifacts", "flat globe", "tile grid", "zoom out"]
updated: 2026-08-10
---

# What the globe quadtree actually costs, and the three ways it showed

Derived 2026-08-08 from the constants in `globe.rs` plus a replica of
`update_globe_tiles`' selection loop (`QT_SPLIT_PX = 340`, `QT_MAX_Z = 12`,
`QT_MAX_TILES = 320`, 1080p, camera looking straight down). **Arithmetic and a
simulation, not a rendered measurement** — this container has no GPU. Every
number below is falsifiable with one `--shot` sweep and should be re-taken by
whoever next has one.

## The resident set peaks just above the tier boundary, not in deep space

| Altitude | Leaves | Total desired | At 512²/tile |
|---|---|---|---|
| 60 km | 294 | 463 | 648 MB |
| 400 km | 285 | 442 | 619 MB |
| 1,000 km | 323 | **494** | 692 MB |
| 3,000 km | 184 | 289 | 405 MB |
| 35,000 km | 12 | 80 | 112 MB |

The intuition that "deep space is the expensive case" is backwards: at 35,000 km
the whole planet is 80 tiles. The load is concentrated in 60 km–1,000 km, which
is exactly the band a globe→street zoom passes through.

`QT_MAX_TILES` is **binding** across that band (294–323 leaves against a cap of
320). The cap stops refinement mid-traversal, so which regions reach z11 and
which stop at z7 depends on `stack.pop()` order rather than on anything visual.
Raising it raises everything in the table with it.

## Three costs, one cause

**Memory.** 512² RGBA + mips is ~1.4 MB/tile, and `RenderAssetUsages::default()`
is `MAIN_WORLD | RENDER_WORLD` — so each tile was resident *twice*, once on the
GPU and once in `Assets<Image>`. Nothing reads a tile's pixels back.
`tile_loader::TILE_TEXTURE_USAGE` is now `RENDER_WORLD` alone.

**The default bites meshes exactly the same way, and that took a second pass to
find** (2026-08-10). Fixing it for tile *textures* left every generated mesh at
`RenderAssetUsages::default()` — the globe's own tile patches and backstop sphere
(`GLOBE_MESH_USAGE`, `qt_segs(z)²` vertices plus indices apiece) and the OSM
building meshes among them, none of which is ever read back either. So the rule
is the asset kind, not the asset: **anything this client generates and only
draws wants `RENDER_WORLD` alone**, and `default()` is the wrong default for all
of it. Consequently the ~1.3 GB "across both worlds" figure below is a
pre-2026-08-10 number and now overstates the build.

**Fill latency.** Every one of those tiles goes through `render_mvt` — the ~121 ms
of tiny-skia scan conversion in [tile-raster-cost.md](tile-raster-cost.md) — plus
`ocean_mask` and a mip chain, on the `platform::workers` pool it shares with the
flat clipmap. At ~460 tiles and a 3-thread pool that is ~12 s to fill the globe.

**Oversampling was the multiplier on both.** `QT_SPLIT_PX = 340` means a leaf is
only ever 170–340 px on screen, so 512² was 2–3× oversampled per axis. The
`DecodedTile` doc comment still said "a globe tile is 256²" — true of the raster
basemap the globe was written against, and silently falsified when the vector
switch routed globe tiles through `vector_tiles::TILE_PX`. `GLOBE_TILE_PX = 256`
restores it; `render_mvt_sized` carries the size, and scales the pixel-denominated
stroke widths with it (`stroke_widths_scale_with_the_raster_size`).

**This conclusion took a release to reach the flat clipmap, which is the larger
consumer** (2026-08-10). The clipmap's 224 tiles are ~310 MB of texture at 512²
against ~78 MB at 256², and it now takes its edge from `quality::Budget::tile_px`
— 512 on a desktop, 256 on the phone tier — through the same `render_mvt_sized`
this section introduced. So `GLOBE_TILE_PX` is no longer the only place the
question is answered; see [tile-raster-cost.md](tile-raster-cost.md) for the flat
side's arithmetic and for why 256² is payable at street level and would not be on
a tier where the imagery *is* the map.

## The ancestor pyramid was permanent, not a loading fallback

`update_globe_tiles` expands every leaf with its full ancestor chain so an
uncached level degrades one step at a time instead of dropping to the z3 floor.
That is right. What was wrong is that the chain was unconditional, so ancestors
never left `desired` — and the stale-despawn filter only reclaims what has. At
low orbit ~170 of ~460 live tiles were coarse levels completely hidden under
their own descendants, each holding a texture, a mesh, a material and a draw call.

Ancestors are now only wanted while the leaf under them has yet to arrive. The
existing stale filter then reclaims them with no new despawn path, and its
"everything overlapping is loaded" guard already prevents a hole.

## Why the globe read as flat panels on a fast zoom-out

`QT_SEGS` was a constant 12 segments per axis at every zoom. A z2 tile spans 90°
of longitude, so its facets were 7.5° — **830 km across, sagging 14 km inside the
sphere**. From just above the tier boundary that is not a sphere; it is a handful
of enormous flat plates meeting at angles. It surfaces on a *fast* zoom-out
specifically, because that is when the coarse floor is all that has arrived.

`qt_segs(z)` sizes the grid from the tile's own angular span to hold sag under
`QT_MAX_SAG_M` (500 m): 64 segments at z2, 32 at z3, 16 at z4, the `QT_SEGS`
minimum from z5 down. Only 16 tiles ever exist at z2 and 64 at z3, so the extra
vertices are bounded and the fine tiles — the overwhelming majority of the live
set — pay nothing (`fine_tiles_keep_the_minimum_grid`).

**The flat clipmap is a different thing and is still genuinely flat.** It is a
tangent plane: `map_stream::clipmap` gives each level a constant `y` with no
curvature term at all, and at the z10 ring's ~165 km extent the true surface is
~530 m below it at the edge. That is invisible from the altitudes the flat tier
is drawn at, and curving it is not a local change — the avatar plane, collision
footprints, buildings, water and the route line all sit on y ≈ 0.

## The depth separation had no altitude range

`QT_LIFT_PER_Z = 4 m` between zoom levels is a **world-space** separation, and
the space tier's depth range is enormous (`free_camera` sets `near = altitude ×
0.01`, `far` past the far limb). With reverse-Z f32 the quantum at the surface is
roughly `d²/near · 2⁻²⁴`:

| Altitude | near | quantum | 4 m resolves? |
|---|---|---|---|
| 60 km | 600 m | 0.36 m | yes |
| 1,000 km | 10 km | 5.9 m | no |
| 3,000 km | 30 km | 18 m | no |
| 10,000 km | 50 km | 119 m | no — the whole ladder is one quantum |

So above ~700 km every rung of the stack z-fought every other. Raising the lift
cannot fix it: the metres needed at 10,000 km are a visible step against the flat
tiles at the boundary. `QT_DEPTH_BIAS_PER_Z` uses `StandardMaterial::depth_bias`
instead, which becomes wgpu's `DepthBiasState::constant` and is denominated in
*depth-buffer* units — one rung per zoom, correct at every altitude, no world-space
cost. `ExtendedMaterial` forwards `depth_bias` to its base, which is what makes
this reachable from `GlobeSurfaceMaterial` at all.

## Not established: the black artifacts

Reported as "weird black artifacts when flying high with a lot of tiles loading".
The two mechanisms above are both consistent with *patchy, view-dependent*
corruption and neither obviously produces black. Two candidates remain untested:
GPU memory pressure (~1.3 GB across both worlds at low orbit, on hardware where
`VK_ERROR_DEVICE_LOST` is already documented), and a decode path that was failing
without saying so — `spawn_globe_tile` discarded both its fetch and decode errors,
leaving the aggregate `globe tile fetches failed` count as the only evidence, which
reads as a rate limit whatever the real cause. Both now log.

A third candidate is **the mip chain is built in the wrong colour space**, and it
is the one that specifically worsens with altitude. `mip::build_mip_chain`
averages `Rgba8UnormSrgb` bytes directly — sRGB-encoded values, with no
linearise-average-re-encode. Because the encoding is concave, the mean of encoded
values is always *below* the encoded mean of light: averaging a dark road
(L ≈ 0.05) with pale paper (L ≈ 0.8) gives 0.58 where the correct answer is 0.68,
about 14% dark. Deeper mips are what a minified tile samples, and the globe
minifies hard — so the map darkens as it recedes, which is exactly the reported
shape even though it is a dimming rather than literal black.

Not fixed here, for two reasons. It would change **every mip in the game** —
streamed tiles, globe tiles, prim textures — and none of it can be looked at from
this container. And `mips_match_the_image_crate` deliberately pins the filter to
`image::imageops::resize`, which does not linearise either, so the test's premise
has to be revisited in the same change rather than quietly broken by it.

The bisect, for whoever has a GPU:

```bash
# does it track altitude (depth) or tile count (memory)?
cargo run -p viberfox -- --shot /tmp/s.png --at 53.2,6.5,60000 \
    --at 53.2,6.5,10000000 --frames 12 --look=-90,0 --csv /tmp/s.csv
VIBERFOX_NO_OCEAN=1 cargo run -p viberfox -- --shot /tmp/a.png \
    --at 53.2,6.5,1000000 --look=-90,0 --dump-state /tmp/a.json
```

`ShotMetrics` now carries `globe_tiles_loaded`, `globe_tiles_desired`,
`globe_texture_mb` and `globe_tile_px`, so that sweep separates the first two
candidates without a rebuild: artifacts tracking `globe_texture_mb` are memory,
artifacts tracking `alt_m` while it stays flat are depth. Before those columns
existed the run could not tell them apart, which is why this section says
"candidates" and not "cause". The colour-space one shows as a monotone darkening
across the whole sweep rather than an onset at some altitude.

If the artifacts appear between the 700 km and 1,500 km frames and nowhere below,
the depth fix above was the cause and this section can be deleted.

## Not measured: any of it, on a GPU

Every figure here is arithmetic. What none of it establishes is whether
`ShotMetrics::settled_s` actually moves — the same gap
[tile-raster-cost.md](tile-raster-cost.md) records for the parse deduplication,
and for the same reason. A `--frames` sweep from 35,000 km to street level with
`--csv` is the one command that would settle it.

## Beware the shared target dir when checking any of this

Verifying this work kept producing "cannot find function `render_mvt_sized` in
module `vector_tiles`" for code that was correct and had compiled a minute
earlier. Cause: other worktrees building concurrently into the container's one
`CARGO_TARGET_DIR`, leaving several `libviberfox_geo-*.rmeta` in
`debug/deps/` — only one of which held the new symbol. `cargo` linked whichever
won. `CARGO_TARGET_DIR=/tmp/... cargo test …` for a trustworthy answer; a
compile error that contradicts the source is this, not the source. The related
trap from the other direction is in
[sccache-parallel-builds.md](sccache-parallel-builds.md).
