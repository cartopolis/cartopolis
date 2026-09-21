---
topic: the-city-stops-at-a-line
triggers: [FAR_CELL_RADIUS, far_cell_radius, "building LOD", "buildings stop", "hard edge", "the city ends", osm_buildings far cells, lod22_radius, "fade buildings", prepass_vertex_shader, MaterialExtension specialize]
updated: 2026-09-21
---

# The buildings end on a cell edge, and the obvious fix has a prepass in it

Three tiers draw buildings and the outermost one simply stops. `lod22` draws real
LoD2.2 shells to `lod22_radius()` (~3.5 km), `osm_buildings` draws a simplified
extrusion from there to `far_cell_radius()` (~5 km), and beyond that the ground is
the flat drawn map with the buildings *printed on it*. Those two do not look
alike, and the boundary is a **straight line across the view**: a city on one
side, a map on the other. Photographed over Groningen at 600 m on 2026-09-21 it is
the most visible LOD discontinuity in the client — more than the clipmap's, which
`map_stream::refresh_tile_lod_blend` now softens.

## The design that is right, and is not in the tree

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

## Why it was built and then reverted (2026-09-21)

**Ambient occlusion is on by default** — `AppPrefs::ambient_occlusion_on()` falls
back to `caps.compute_shaders` — and it pulls in `DepthPrepass` + `NormalPrepass`.
That is the same fact `road_depth::specialize` carries a long warning about, and it
is easy to read that warning as being only about the *layout* trap. It is not.

A `MaterialExtension` that moves vertices in its main-pass vertex shader and leaves
`prepass_vertex_shader` at bevy's default has **two pipelines drawing the same
geometry in two different places**. The prepass writes depth for the building at
full height; the main pass draws it short. Everything behind the building's missing
upper half was depth-rejected against a silhouette that is no longer drawn, and the
occlusion and normal buffers describe geometry that is not on screen.

*Reasoned from the pipeline, not observed* — the branch was reverted before a
capture, so the exact artifact is a prediction. The prediction is not the point;
the point is that the design is not complete without the prepass half, and shipping
it to find out is how this client gets a halted renderer.

## What a correct version costs

A `prepass_vertex_shader` applying the identical transform. That means a copy of
bevy's prepass vertex stage with one line changed, carrying its `#ifdef`s for
motion vectors, normals, deferred, skinning and morph targets — and therefore a
file that rots silently on every bevy bump, which is the maintenance this tree
already pays once for the vendored `big_space`. Worth doing; worth doing
deliberately, with the shader diffed against upstream at each upgrade the way
`bevy_compat.rs` is.

**The cheap alternative is to move the line, not soften it** — raise
`FAR_CELL_RADIUS` so the boundary sits out where the haze has taken over. It costs
streaming and residency on the tier that can least afford it, it does not remove
the discontinuity, and `remove-the-conflict` is the standing preference here. It is
recorded so it is a decision rather than an oversight.
