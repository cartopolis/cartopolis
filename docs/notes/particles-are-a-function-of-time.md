---
triggers:
  - working on `systems::particles`, `assets/shaders/particles.wesl`, or `cartopolis_core::particles`
  - "why don't the particles collide / why can't I move one"
  - adding an effect (smoke, spray, sparks, weather on a prim)
  - reaching for an off-the-shelf Bevy particle crate (`bevy_hanabi`, …)
  - an emitter is missing, or a fire is drawn in the wrong place
  - a burst plays once and never again, or plays every frame
updated: 2026-08-29
---

# A particle here is a formula, not a simulation

`cartopolis_core::ParticleEffect` is fourteen numbers on a prim. It travels in
every `WorldSnapshot`, it is one JSON column in SQLite, `particles` writes it
and the client hands it to a shader. **No particle exists anywhere as data**:
`assets/shaders/particles.wesl` computes where each one is, how big, what colour
and whether it is alive, from those numbers, the clock and the particle's own
index. After the shared quad pool is built at startup, an emitter uploads
nothing.

## The rejected option, and why

The obvious implementation is a `Vec<Particle>` stepped each frame into a
rewritten vertex buffer. That is a per-frame mesh upload per emitter, which is
exactly the churn that loses the device on the hardware this ships to — the Intel
iGPU entry in `upload-bursts.md`, which is also about the same failure measured
in bytes. A hundred fires would be a hundred vertex buffers rewritten sixty times
a second, and no per-frame cap fixes that because the writes *are* the feature.

The second reason is the wire. A CPU-simulated system that looked the same on two
screens would have to replicate particle state at the tick rate; a formula
replicates fourteen floats once and then never again.

`systems::grass` reached this conclusion first and this follows its pattern
exactly: one shared lattice mesh, a small uniform block, and a vertex shader that
turns a *slot* into a thing.

## What that costs, plainly

Nothing collides. Nothing can be individually addressed, moved or killed. Two
clients agree only because they evaluate the same closed form from the same
numbers — there is no authority over an individual particle and there never will
be one. For fire and for an explosion that is the whole of what is wanted; for
anything that has to interact with the world it is the wrong tool and a different
one should be built beside it rather than by bending this.

## Four things that are load-bearing

**A burst is a level with a sequence.** An explosion is one-shot, but a one-shot
*message* is missed by anyone whose snapshot lands a tick late and there is no
reliable channel here for a thing that happens once. So `burst: true` is carried
for as long as the prim lives, and `sequence` is what tells a client this is a
*second* blast rather than the same one re-described. Diffing on the effect
itself replays nothing; replaying on every snapshot restarts the bang twenty
times a second. `EmoteState::sequence` documents the identical trap.

**The emitter is a child entity, and only its translation is used.** Under
`big_space` a child inherits the prim's `GridCell` and floating-origin
propagation for free, which is what makes an emitter follow a prim a script is
flying across the map with nothing in `systems::particles` knowing that prims
move. The shader takes column 3 of the model matrix and nothing else, so the
prim's rotation and scale never reach the flame — a flattened box does not get a
flattened fire.

**Frustum culling is off, and it has to be.** The quads are placed metres away
from the mesh's own vertices, in the vertex stage, so the derived AABB describes
the wrong volume: culled correctly, an emitter disappears while its particles are
still on screen. `ParticleEffect::reach_m` knows the right radius and there is no
component in Bevy 0.19 that takes one, so the honest answer is not to cull this
handful of entities.

**`AlphaMode::Add` is premultiplied blending**, i.e. `src·1 + dst·(1−srcAlpha)`.
The fragment outputs an alpha of *zero* to reach pure `src + dst`. An alpha of
one there would be an ordinary over-blend and the fire would occlude what is
behind it.

## Measured, and unverified

`MAX_PARTICLES` is 256 and the pool is allocated once at that size; an emitter
asking for fewer draws the same mesh and collapses the surplus quads to a point,
because trimming the index count per emitter would be a mesh per distinct count —
the upload this design exists to avoid.

**Nothing about how it *looks* has been verified.** The container renders on
Mesa lavapipe, where colour, exposure, bloom and blending are not evidence
(`headless-shots-software-renderer.md`). What a headless capture proves is the
geometry half — that the pipeline built, that the emitter exists and that its
quads are in frame — and that is what `particle_emitters` / `particle_quads` in
`--dump-state` report and what `--ui fire` exists to photograph. Whether it reads
as fire is a question for a workstation.
