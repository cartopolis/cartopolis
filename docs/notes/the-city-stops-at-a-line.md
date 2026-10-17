---
topic: the-city-stops-at-a-line
triggers: [FAR_CELL_RADIUS, far_cell_radius, "building LOD", "buildings stop", "hard edge", "the city ends", osm_buildings far cells, lod22_radius, "fade buildings", prepass_vertex_shader, MaterialExtension specialize, systems::building_fade, building_fade.wesl, systems/map/osm_buildings.rs, systems/map/lod22.rs]
updated: 2026-09-21
---

# The buildings used to end on a cell edge, and the fix needed a prepass out of the way

**Built 2026-09-21**, after ambient occlusion was removed and took the depth
prepass with it (`surface-finish-and-occlusion.md`). The section below on why it
could not be done is kept, because the constraint returns the moment anything
reintroduces a prepass — and `systems::building_fade` is the code.

Three tiers draw buildings and the outermost one used to simply stop. `lod22`
draws real LoD2.2 shells to `lod22_radius()` (~3.5 km), `osm_buildings` draws a
simplified extrusion from there to `far_cell_radius()` (~5 km), and beyond that
the ground is the flat drawn map with the buildings *printed on it*. Those two do
not look alike, and the boundary was a **straight line across the view**: a city
on one side, a map on the other. Photographed over Groningen at 600 m on
2026-09-21 it was the most visible LOD discontinuity in the client — more than the
clipmap's, which `map_stream::refresh_tile_lod_blend` softens.

## The design

A **height ramp over the outermost ring**: scale each building about its own base
so it goes down into the map instead of vanishing at an edge. Three things make it
the right shape rather than a compromise:

* **The endpoint is already drawn.** A building at zero height *is* its printed
  footprint, which is exactly what takes over beyond the ring. It fades into its
  replacement, not into nothing. Nothing else available here has that property.
* **Not transparency.** `systems::grass` sets this tree's rule and both of its
  retires are height ramps. An alpha-blended building leaves the opaque pass,
  sorts against the ground it stands on, and pays a per-pixel depth decision at
  the one distance where there are thousands of them.
* **Chebyshev in cells, not metres.** The ring it must reach zero at is a *square*
  of cells. A radial ramp finishes early along the axes and leaves the four
  corners standing at full height against the boundary — the original hard line,
  kept exactly where it reads worst. Same reasoning as the clipmap cross-fade.

It needs one piece of plumbing: a vertex must know **its own building's base**.
`MeshData::set_base` folds the ground height into the vertex Y, where a shader
cannot recover it, so the far tier would carry it as a mesh attribute — opt-in per
buffer, because the detail patch and the LoD2.2 shells never fade and four bytes a
vertex is ~2.9 MB of graphics memory on one 720k-vertex cell, on the tier with the
least of it. And the far *windows* must take the same material: a window is a quad
standing proud of a wall that is being lowered, and on the shared material it
would hang in the air over a building sinking beneath it.

## Why it could not be done until the prepass went (2026-09-21)

**Ambient occlusion was on by default** — `AppPrefs::ambient_occlusion_on()` fell
back to `caps.compute_shaders` — and it pulled in `DepthPrepass` + `NormalPrepass`.
That is the same fact `road_depth::specialize` carries a long warning about, and it
is easy to read that warning as being only about the *layout* trap. It is not.

A `MaterialExtension` that moves vertices in its main-pass vertex shader and leaves
`prepass_vertex_shader` at bevy's default has **two pipelines drawing the same
geometry in two different places**. The prepass writes depth for the building at
full height; the main pass draws it short. Everything behind the building's missing
upper half would be depth-rejected against a silhouette that is not drawn, and the
occlusion and normal buffers would describe geometry that is not on screen.

*Reasoned from the pipeline, never observed* — the first attempt was reverted
before a capture. The prediction is not the point; the point is that the design is
not complete without the prepass half.

## What a prepass would cost this feature, if one comes back

A `prepass_vertex_shader` applying the identical transform: a copy of bevy's
prepass vertex stage with one line changed, carrying its `#ifdef`s for motion
vectors, normals, deferred, skinning and morph targets — and therefore a file that
rots silently on every bevy bump, which is the maintenance this tree already pays
once for the vendored `big_space`. `BuildingFade::specialize` keeps a prepass
guard so the failure is a building drawn unfaded rather than a halted renderer,
but the *correctness* half is that shader and there is no way round it.

**The cheap alternative was to move the line, not soften it** — raise
`FAR_CELL_RADIUS` so the boundary sits out where the haze has taken over. It costs
streaming and residency on the tier that can least afford it, it does not remove
the discontinuity, and `remove-the-conflict` is the standing preference here. It
is recorded so it is a decision rather than an oversight.
