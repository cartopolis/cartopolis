---
triggers:
  - working on crates/viewer (`carto-view`), or adding anything to it
  - "the harness shows a crack / a hole / a tear the client does not"
  - considering reimplementing a client algorithm in the harness rather than sharing it
  - deciding what belongs in `cartopolis_geo` versus `cartopolis`
  - reading a `--relief` A/B, or a picture the harness produced
---

# The harness must share the client's geometry, or it invents faults

`carto-view` exists to answer "what does this ground actually look like" in
seconds instead of the six minutes a client capture costs. It deliberately does
**not** depend on `cartopolis` — that dependency would rebuild it whenever the
thing under development changed, and a fault in shared rendering code would then
appear identically in both, which is the one case a harness has to be able to
tell apart.

That independence has a price, and on 2026-09-19 it was paid in a whole session.

## What happened

`refine_for_terrain` lived in the client. The harness therefore bent whatever
triangles earcut emitted. Earcut answers a land parcel with two of them, so an
unrefined 100 m edge draws a **straight chord** across whatever the ground does
between its ends — while the road ribbon beside it, which has a vertex every few
metres, follows the ground. The two gapped by metres.

The pictures showed cracks down every corridor in Groningen. Three separate
"faults" were chased as map bugs before the cause was found, and all three were
the instrument.

**The tell was that the gaps scaled linearly with `--relief`** — invisible at 2 m,
wide open at 25 m. That is what a chord does. A real geometric fault does not
care how tall the hills are.

The fix was to move the refinement into `cartopolis_geo::terrain_refine`, behind
a `GroundField` trait carrying the only two questions the algorithm ever asks of
a field. One implementation, not two: a second red-green refinement would have
its own T-junction behaviour, and a harness whose artifacts differ from the
client's is worse than no harness at all.

## The rule

**Anything that decides the shape of the ground belongs in `cartopolis_geo`, and
the harness calls it.** Tessellation, the depth ladder, the road stitch and now
the terrain refinement are all shared for this reason. If a step is missing from
the harness the pictures are not "approximately right" — they are wrong in a way
that looks exactly like the bug you are hunting.

Reimplementing a step in the harness "just to see" is the thing not to do. It
will drift, and it will drift silently, because nothing compares the two.

## The same rule, twice more (2026-09-19, later)

Both were things the client draws and the harness did not, and both produced a
fault-shaped absence rather than an obvious blank:

- **Every bridge was thrown away.** `tessellate_roads` returns a deck's
  carriageways in a separate `on_deck` list, tagged with their deck and still at
  `y = 0`, because the height is measured from AHN some frames later. The harness
  read `roads.ground` and dropped the rest, and drew no deck slabs either — 19
  decks and 10 road meshes in one central Groningen cell. What that leaves is a
  clean square-ended gap in a continuous road, which is exactly what a real
  tessellation fault looks like; an hour went on one over the railway before the
  instrument was suspected. There is no AHN here, so `--deck-height` is a flat
  lift: honest for *is it continuous, where are the joins*, and not evidence about
  any real bridge's height.
- **Roads were painted one flat tone.** A flat colour cannot show a joint, a
  marking's registration round a curve, or whether a slip road's texture runs the
  way the road does. `road_texture` is in `cartopolis_geo`, so the harness now
  draws the client's own surface — with the client's mip chain and 8x anisotropy,
  because without those a road at range aliases into a shimmer that reads as a
  registration fault.

**`--flicker` measures depth only with `--plain`.** Measured over a Groningen
motorway: about **100** changed pixels textured against **0** plain, and the mip
chain moved that by five. Textured it reports surface *and* sampler; plain it
reports geometry, which is where a coplanar fault lives. With that rule, 20
street-level viewpoints across five cells on 20 m of synthetic relief report zero.

## Two smaller versions of the same mistake, same day

Both were reference geometry rather than map geometry, and both read as holes:

- **The checkered underlay was a flat plane at `-30 m`.** With `--relief 25` the
  field bottoms out near −34 m, so the reference plane stood *through* the ground
  and painted checkerboard across intact surfaces. It hangs below the field's own
  floor now.
- **`--at`'s altitude is absolute.** Terrain rises under a fixed camera, so a
  relief capture frames less ground than its flat counterpart and every
  difference between the pair reads as geometry. `--agl` measures from the ground
  instead, and a `--relief` A/B is not trustworthy without it.

## What `--flicker` is and is not for

It was added the same day, and the first thing it established was a negative.
Measured over Groningen at 25 m relief: **7 pixels with the fills-under-roads cut
and 7 without**, and **0** on flat ground.

So the harness has no flicker floor at all — unlike the client's, where traffic
and pedestrians animate between the pair and the threshold has to sit above them.
And the cut is not a depth-tie fix: the road and the fill beneath it sit on
different rungs of `ground_stack`, so the bias already separates them. What that
cut removes is one sheet standing *through* another on a gradient, which is
deterministic under a camera nudge and therefore invisible to a flicker test.

Reach for `--flicker` for coplanar surfaces on the *same* rung. For anything the
ladder already orders, it will report zero whether the geometry is right or not.

## What the harness is blind to: inside the road band

It shares the client's geometry and the client's depth **ladder**, and it does not
share the client's depth **shader**. Each surface group is offset by
`GroundLayer::y_offset()` and that is all — the per-vertex rung
(`vector_tiles::road_rung` / `road_casing_rung`, drawn by
`assets/shaders/road_depth.wgsl`) is not applied.

That matters for exactly one region of the stack, because the road sheet is a
single `GroundLayer`. `ground_stack.rs` says it outright: *"Roads do not get a
rung each"* — a carriageway, its own kerb casing and a flyover over both are
ordered *within* the band by that per-vertex rung, inside `ROAD_BAND_ULPS` of
reserved room.

So:

- `--check` is still right about the road band. It measures **geometry** — which
  patch of ground two groups both cover — and that is a fact about the tiles, not
  about how they are drawn.
- A **picture** is not. Where `--check` reports `RoadCasing(X)` and `Road(X)`
  sharing ground (2.3–3.0 % of the casing over central Groningen, and the only
  overlap left anywhere in the six cells surveyed on 2026-09-19), the harness has
  no way to draw them in the right order and the client does. Do not read a
  z-fight between a kerb and its own tarmac off a `carto-view` capture.

That residual is expected, not a fault: a kerb band is the slab a carriageway is
laid on, `road_casing_rung` puts it at the foot of its class's block and
`road_rung` puts the tarmac above it, and the doc on the former says so — *"it
lies under its own tarmac"*. `road_stitch` removes the overlaps the ladder cannot
arbitrate; this is one it can.
