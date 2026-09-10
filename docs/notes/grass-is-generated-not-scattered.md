---
triggers:
  - working on `systems::grass`, `cartopolis_geo::turf`, or `assets/shaders/grass.wgsl`
  - "why isn't grass scattered like the trees are"
  - adding another dense near-field layer (flowers, gravel, crowds, litter)
  - reaching for an off-the-shelf Bevy grass crate
  - the lawn is bare, or a seam appears along a patch boundary
  - bumping Bevy, or anything that moves `MeshTag` / `MaterialExtension`
updated: 2026-08-21
---

# Grass is generated on the GPU, and the reason is churn

`systems::vegetation` scatters trees on the CPU, merges them into one mesh per
cell and uploads them. `systems::grass` does none of that: no blade exists on the
CPU at any point, and after startup the layer uploads nothing at all. This note
is why, because "do it like the trees" is the obvious thing to reach for and it
is the thing that was measured and rejected.

## The rejected option, with numbers

Grass differs from trees on both axes at once, in opposite directions. A tree is
legible from two kilometres up and a z14 cell holds at most `MAX_TREES_PER_CELL`
(6,000) of them. A blade of grass is illegible past about thirty metres and needs
roughly **ten per square metre** to read as grass rather than as a scatter of
weeds. So the same design gives:

| | trees | grass, scattered the same way |
|---|---|---|
| instances in view | ~6,000 per cell, over 1.5 km | ~20,000, over a 26 m disc |
| bytes per instance | ~340 (18-vertex ball + trunk) | ~316 (5-vertex blade) |
| vertex buffer | ~2 MB per cell, built once | **~6 MB, rebuilt as the camera walks** |

The last cell is the whole problem. A tree cell is built when it streams in and
then sits there; a grass disc follows the camera, so it is *continuous* churn for
as long as the avatar is moving. That is the exact pattern that loses the device
(`docs/notes/upload-bursts.md`), and unlike a streamer it cannot be fixed by a
per-frame cap — a cap spreads churn out, it does not remove it. There is no
budget at which "rebuild six megabytes of vertex buffer every twenty metres,
forever" becomes free.

## What replaced it

Three fixed things and one small moving one:

* **One mesh.** `turf::blade_lattice` — a patch's worth of blade *slots*, five
  vertices each carrying only which slot and which corner they are. Every patch
  in the world draws this one handle. ~170 KB, uploaded once.
* **One material.** Rewritten when the wind moves (a few times an hour) and when
  the mask moves. Not per cell, unlike water's — grass has no per-cell texture.
* **A fixed pool of patch entities** that slides along a world-aligned lattice
  under the camera. Crossing a lattice line moves a `Transform` and rewrites a
  `MeshTag`. Nothing spawns, nothing despawns, no mesh is rebuilt.
* **A moving coverage mask** — 384² `Rg8Unorm`, 96 m of ground at 25 cm a texel,
  ~288 KB. Red is "grass may grow here", green is the ground height. Rasterised
  on a worker when the camera walks out of it, which is every twenty-odd metres.

`assets/shaders/grass.wgsl` turns a slot into a blade: hash the slot's world
position for jitter, yaw, height, lean and tone; sample the mask for existence
and ground height; bend in the wind; shrink back into the ground with distance.

## The far ring (LOD), and why its seam is statistical

One dense pool ended at 26 m on a visible line — dense lawn on one side, flat
`palette::GRASS` paint on the other. The fix (2026-08-21) is a second `Tier`:
16 m patches on a 64² lattice (16 blades/m² against the near pool's ~200), each
blade a shade taller and three times wider so ground coverage doesn't collapse
with the count — Ghost of Tsushima's "drop three blades in four at the LOD
switch, widen the survivors" made into a ring. It retires, staggered, across
44–80 m, where a 17 cm blade is under two pixels: the handover to paint happens
where a blade stops being legible, not where the eye can find a circle.

The two rings never meet on a line. A far blade *arrives* (stands up out of the
ground) across exactly the band the near blades retire in, and both draw their
per-blade cut from the same hash lane — so the expected blade count ramps
smoothly from the near density to the far one through the band, and there is no
LOD switch anywhere, only two overlapping staggers. The `range2` uniform is the
arrive band; the near tier's sits below zero, which reads as "always arrived".
An unarrived blade costs its vertex invocations but rasterises nothing: height
zero collapses all five vertices onto one line, and zero-area triangles emit no
fragments.

The far ring is full-tier only, and the mask grew to feed it: 640² texels over
208 m (0.325 m/texel, against 0.25 before) on the full rung, unchanged on the
reduced one. One mask serves both rings — far blades need coverage, not more
coverage *accuracy*, and a second coarser mask would double the rebuild
machinery to save ~600 KB.

## Four things that are load-bearing and do not look it

**The hash input must be the anchor-metre frame, not `world_position`.** Under
`big_space` the render origin jumps a whole grid cell as the camera moves, and a
hash against it would reshuffle every blade in sight at the instant it jumped.
The frame is reconstructed from the patch's `MeshTag`, packed as two signed
16-bit patch indices — which is also why it is not taken from the floating
origin's grid cell: the tag depends on nothing outside the entity. `water.wgsl`
documents the same trap for its wave phase and solves it with UV0, which is not
available here because the mesh is shared by every patch.

**The hash input is quantised to millimetres first.** A slot is arithmetic
(`patch origin + lattice offset`), so the same ground reached from two different
patch origins can differ in the last bit. Unquantised, that last bit is a
different blade, and the result is a visible seam along every patch boundary.

**Nothing anywhere in this layer is transparent, and nothing may become so.** Both
ramps — the distance retire and the altitude fade — are *height* ramps: a blade
shrinks into the ground rather than fading out. Alpha on tens of thousands of
triangles buys a sort order and a pass to hide something already sub-pixel, which
is the same argument `scatter`'s module doc makes for opaque canopies. It is also
what lets the two ramps compose without a seam, since they multiply.

**A blade that is culled must collapse to a *point*, and every term that spreads
its vertices apart has to carry the `live` factor — not just the height.** A
fixed pool means the vertex stage runs for every slot whether or not a blade
stands there, and the ratio is not close: measured over a canal in Groningen on
2026-09-09, 1,511,680 slots against 16 live blades. Zeroing the height alone left
the other 1.5 million as flat hairlines lying in the ground plane — zero area in
exact arithmetic, and in practice enough of them catch a pixel centre to scatter
single-pixel specks over the whole near view. They are invisible over pale paving
and obvious over water, so they were reported from a phone as speckles on the
canals rather than as anything wrong with the lawn; and because which degenerate
triangle lands on a pixel centre is a property of the projection, they re-roll on
every camera move, which reads as crawling. `assets/shaders/particles.wgsl` had
the same shape of problem to solve and already solved it the same way (`size`
carries `alive`, so a dead particle's four corners coincide) — any future pooled
layer owes the same.

## Why not an off-the-shelf crate

`bevy_procedural_grass` and its siblings are standalone `Material`s with their own
pipelines. That is fine in a demo scene and wrong here: this map's ground is lit
by a physical sun, four shadow cascades, aerial-perspective fog and the
atmosphere's environment map, and a lawn lit by none of them reads as a decal.
`GrassExtension` is a `MaterialExtension` with a **vertex shader only** — the
blade is built here and handed to `StandardMaterial` to be lit exactly like the
ground it stands on. It also means the layer follows the day/night cycle and the
weather with nothing in this module to say about either.

The technique itself is borrowed, and borrowed from the right place: shader-side
blade generation with a density map, per-blade staggered LOD and view-space
thickening are what Ghost of Tsushima's GDC talk describes and what the Unity and
Godot ports of it do. What is not borrowed is the delivery — a compute shader
laying out an instance buffer per frame, which would need a compute path on three
backends to save work this scene does not spend.

## What is measured and what is not

Measured here on 2026-08-20, lavapipe, headless: nothing. The geometry, placement
and visibility half of a `--shot` is trustworthy on the software rasteriser and
`grass_patches` / `grass_slots` are in `--dump-state` for exactly that; **colour,
exposure and lighting are not** (`docs/notes/headless-shots-software-renderer.md`),
so how a lawn actually *looks* lit is unverified and belongs on a workstation.

Not run on a phone. The layer is behind `quality::Budget::detail_layers`, so a
handset starts with it off, and its cost there — tens of thousands of vertex
invocations per frame against a tiler's binning — is a guess until someone
measures it on the Adreno 840.

Two things the data cannot currently express, both worth doing and neither a bug:
the mask has one coverage channel, so a meadow and a mown verge get the same
28 cm blade; and there is no interaction, so an avatar walks through a lawn
without parting it.
