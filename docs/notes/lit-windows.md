---
topic: lit-windows
triggers: [building_windows.rs, building_windows.wesl, lod22.rs, osm_buildings.rs,
  "lit windows", "windows at night", "night", "3dbag windows", "facade", sky.rs,
  update_window_glow, WindowGlow]
updated: 2026-08-26
---

# The 3DBAG buildings had no windows, and the reason was a despawn

Two lanes draw buildings. `osm_buildings` extrudes OSM footprints and has built
real emissive window quads for a long time (`WindowGlow`, `push_wall_windows`,
`sky::update_window_glow`). `lod22` streams 3DBAG LoD2.2 meshes and had none.

That would be a small gap except for one line in `osm_buildings`: over any cell
`lod22_covers()` reports, it **despawns its own detail patch and never refetches
it**, because the real meshes are the only buildings there. 3DBAG covers the
Netherlands. So the window feature existed, was correct, and was never once
visible in the country the app is built around — measured 2026-08-26 with a
`--time 22` capture over Groningen that came back completely black except for
street labels and a few lamps.

`systems::building_windows` is the missing half, written for the lane that has to
serve it. `docs/notes/parked-vehicles.md` is the sibling case: content the map
data already implied and nothing drew.

## Why a shader and not quads

A z14 LoD2.2 cell is thousands of buildings and ~240k triangles. Four floors of
window quads on each is tens of megabytes of extra mesh per cell, on the lane that
already owns the largest assets in the app and the documented device-loss churn
(`docs/notes/upload-bursts.md`). Nothing is stored: the pattern is computed per
fragment from the wall's own normal and position, so it costs no vertex, no
upload and no residency — and it reaches every building rather than the patch
around the camera that the quad path was limited to.

This is the same call `systems::grass` states as a rule ("generated, not
scattered") and `globe_ocean` makes for the ocean ("a BRDF split, not water").

## The lattice has to divide the floating origin

The shader has no stable per-vertex anchor to work from. `water.wesl` uses UV0
in anchor metres and `grass.wesl` reconstructs one from `MeshTag`; neither is
available here, because `lod22` dedupes its vertices and a shared corner vertex
belongs to two walls with **different tangents**, so no per-vertex coordinate can
serve both.

So it reads `world_position` — which jumps by a whole `WORLD_CELL_EDGE`
(10,000 m) when big_space recentres — and the lattice is chosen to divide that
jump exactly:

| lattice | pitch | steps per 10 km | hash period | periods |
|---|---|---|---|---|
| windows across | 2.5 m | 4,000 | 32 | 125 |
| windows up | 3.125 m | 3,200 | 32 | 100 |
| occupancy block | 25 m | 400 | 16 | 25 |

Both the index step **and** its hash period have to come out whole, or the
pattern changes when you fly 10 km — every window in the city relighting at once,
with nothing in any log. `building_windows::tests` pins both rows; do not change
a pitch without redoing that arithmetic.

## Two hashes, not one

A per-window hash alone gives every facade in the city the same fraction of lit
windows. The first tuned capture showed what that looks like: correct, and
unmistakably decoration rather than a city. Real streets have whole dark
buildings standing between the lit ones.

So a second, coarser hash on a 25 m block — roughly one building's footprint,
taken on horizontal position only so all four walls of a building agree — decides
how *lively* the block is, and the window hash is tested against that. A dead
block lights about one window in twelve, the liveliest about half. **Neither end
is 0 or 1**: a wholly black building reads as missing, and a wholly lit one is
the thing this exists to prevent.

## Measured

- **Emissive gain 1.6 → 0.62** (2026-08-26). At 1.6 the city lit to the horizon
  like a stadium and bloom smeared every facade into one sheet.
- The **night factor must not be gated on `dev.is_changed()`**, which is what
  every other consumer of the clock does and what this did first. The clock
  settles during startup; the shared material is created *lazily*, on the first
  LoD2.2 cell that spawns, seconds later. Gated, the material was born holding
  zero and nothing ever wrote to it — a city that stays dark at midnight with no
  error and a healthy building count. `sky::update_window_glow` avoids the same
  trap for the other lane with a second `Added<WindowGlow>` query; a shared
  uniform has no equivalent, so it compares the value instead.

## What the streaks on a gable wall are, and are not

Night captures show thin vertical light streaks on walls seen nearly edge-on.
**They are correct projection, not an artefact**, and two rounds of captures were
spent before that was measured rather than guessed: at 40 m a 1.55 m window is
~30 px tall, and at ~10° incidence its 1.35 m width projects to ~4 px. A 4×30 px
sliver is what a real window looks like from there.

Two fixes were attempted against them and neither is the cause. Both were kept
anyway, on their own merits, and their comments say so:

- The **degenerate-normal guard** (`horiz_len > 1e-3`). A triangle soup from real
  survey data does contain zero-area triangles; `normalize` of a zero normal is
  NaN, and every comparison against NaN is false, so such a fragment would slip
  past the wall test into a garbage tangent. Real and silent — just not this.
- The **converge-to-the-mean blur**. Genuinely sub-pixel windows would crawl as
  the camera moves, which no still capture can show. Note it converges to the
  pattern's mean rather than fading out: the sub-pixel windows *are* the lit
  horizon, and fading them would black out the far half of the city.

## Known limits

- **Floors sit on each building's own base** since 2026-10-20 (the base rides in
  two spare vertex bytes — `surface-finish-and-occlusion.md`). The OSM lane
  carries no base and still sits on a global height lattice, which on Dutch
  relief puts a ground-floor band a metre or two out.
- **A wall is identified by its normal**, not by the source's roof/wall flag. The
  flag exists in the 3DBAG payload but is not carried into the mesh, and putting
  it there would cost a vertex attribute on the biggest geometry lane we have.
- **The daylight half is deliberate.** Panes darken the facade slightly by day
  (`DAY_TINT`), which is what a window looks like from outside and what stopped
  the buildings being blank slabs. It is also how the layer gets checked at all —
  a night-only feature is invisible for sixteen hours of the clock.
- **`osm_buildings`' quad path is untouched** and still runs outside 3DBAG
  coverage. The two use the same ramp and the same warm hue on purpose, so a
  street straddling a coverage edge does not change colour across it.
