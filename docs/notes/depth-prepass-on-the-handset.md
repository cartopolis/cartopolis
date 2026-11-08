---
topic: rendering
triggers: [quality.rs, free_camera.rs, building_windows.rs, ground_detail.rs, building_fade.rs, road_depth.rs, grass.rs, water.rs, globe_ocean.rs, sky_dome.rs, data_layers.rs, enable_prepass, keep_holes_out_of_view_prepass, DepthPrepass, CARTO_DEPTH_PREPASS, "depth prepass", "prepass", "overdraw", "fill", "phone is slow", "handset frame rate"]
updated: 2026-11-08
---

# A handset draws depth first, and which materials stay out of it

Since 2026-11-08 the camera carries `DepthPrepass` when
`quality::Budget::depth_prepass` says so: on a handset (`handset_guarded`, the
phone rung), nowhere else. `CARTO_DEPTH_PREPASS=0|1` overrides it for one launch
(`debug.carto.depth_prepass` on Android). It is decided once, at the camera
spawn, like `Hdr`.

## Why: the phone's frame was three shaded fragments a pixel

Measured on the OnePlus 15 itself (build `aa11172c`, Balanced, its own settings,
still over Groningen at 120 m, 10 s windows, 2026-11-07): 30.2 ms a frame; render
scale 0.75 → 17.4 ms, 0.5 → 15.1 ms; bloom off and sky lighting off moved
nothing. The frame is fill.

Counted on `vps-1` (lavapipe, `--features perf`, `CARTO_QUALITY=android`, the
phone's 1272×2772, 150 m over the Oude Ebbingestraat looking north, 2026-11-08):

| | main pass fragments | main pass | depth prepass | frame (lavapipe) |
|---|---|---|---|---|
| no prepass | 10.62 M | 1218 ms | — | 1428 ms |
| prepass | **3.34 M** | 634 ms | 217 ms | 1051 ms |

3.5 M pixels, so the main pass went from 3.0 shaded fragments a pixel to 0.95
(the sky is the rest). Bevy bins `Opaque3d` by pipeline and material, not by
distance, so without a prepass nothing draws front to back and early-Z rejects
little. Leaving out ground detail, ground meshes, the building fade or the LOD
blend one at a time each moved the count by under 6 %: the overdraw is the city
drawn over itself, not one layer.

**Not yet measured on the phone.** lavapipe counts fragments exactly as an Adreno
would, but an Adreno has its own hidden-surface removal (LRZ) that may already
have taken some of this, and the prepass costs a second geometry pass there too.
The milliseconds above are a CPU rasteriser's. The A/B on the device is
`debug.carto.depth_prepass` 0 against 1.

On the Steam Deck at street level the frame is geometry and shadows, not fill
(`render-performance-survey.md`, 2026-10-20) — which is why the other rungs do not
take it.

## The rule every material follows

A prepass writes depth with bevy's own depth-only shaders. Anything whose main
pass puts its surface somewhere else, or cuts holes in it, would have depth
written where nothing is drawn, and everything behind it rejected. So:

- **Out (`enable_prepass() -> false`)** — drawn by the main pass alone, still
  depth-tested and depth-writing there, so the picture is unchanged and only the
  saving on its own pixels is lost:
  - `BuildingFade`, `GrassExtension`, `RoadDepthExtension` — move vertices or depth
    in their own vertex stage, which the prepass does not run.
  - `DepthAlways` (map pins) — ignores the depth test in the main pass.
  - `SkyDomeMaterial` — writes no depth in the main pass.
  - `WaterExtension`, `GlobeOcean` — not audited; small shares of a city frame.
- **In, except the variant that cuts holes** — `BuildingWindows` (the cutaway
  and the screen-door netting) and `GroundDetailExtension` (`see_through`, the
  sewer view). `keep_holes_out_of_view_prepass` turns the depth write off in the
  camera's prepass for those variants only.
- **In** — `StandardMaterial` and everything else opaque, and the crowd, which
  already had a matching `prepass_vertex_shader`. Transparent materials never
  enter a prepass.

A new material with a custom vertex stage, a `discard`, or a depth trick must
join one of the first two lists. Nothing checks this; the symptom of forgetting
is a hole where the material should be, on the phone only.

**The shadow passes are bevy's prepass pipeline too**, and go through the same
`specialize` hooks with a label containing `prepass`. They are told from the
camera's by the key's projection bits: a light's shadow view always sets one,
the camera's prepass key sets none (`VIEW_PROJECTION_NONSTANDARD`). Testing for
the camera's *perspective* bit — the first version — matched nothing, and the
sewer view lost its pipes; the A/B capture with `--sewers` is what caught it.
Shadows are unchanged; `enable_prepass` does not touch them (`enable_shadows`
does).

## Seen, and not seen

Captures with and without on `vps-1` at the phone's size: 150 m and 3 km
identical but for water ripples, traffic and windmill blades (14 k and 80 pixels
of 3.5 M and 0.9 M); street level identical once settled. Not seen: the inside
of a building (the cutaway), and anything on an Adreno, where a depth that
differs by one bit between the two passes would z-fight — bevy marks no position
`@invariant`.
