---
topic: render-scale
triggers: [render_scale.rs, systems::render_scale, MainPassResolutionOverride, RenderScale, apply_render_scale, make_the_blit_filter, physical_target_size, physical_viewport_size, ExtractedView, main_pass_viewport, upscaling, BlitPipeline, ANativeWindow_setBuffersGeometry, "render scale", "map in the corner"]
updated: 2026-10-17
---

# A render scale has to shrink the target, not the pass

The phone tier draws the 3D world at a fraction of the screen (1/2 by default,
a Settings slider since then) and blits it back up under a full-resolution egui.
`systems::render_scale`'s module doc has the mechanics; this note keeps the two
routes that lost, because both look like the obvious answer.

## `MainPassResolutionOverride` does not upscale

It is the component Bevy names for exactly this, and it was the first
implementation (2026-08-13). It put the map **in the corner of the phone**: the
world in the top-left ~70 % of the screen, clear colour down the right and along
the bottom, the chrome floating over nothing — and nothing logged, because
nothing was wrong.

The override shrinks the *pass*, not the *target*. Every 3D node scissors itself
into the top-left of a main texture still allocated at full size, and Bevy's
`upscaling` node then blits the **whole** texture — unrendered margin included —
over the output with a fixed 0..1 fullscreen triangle (`blit.wgsl`). Upstream's
only consumer of the override is DLSS, which owns a node that reads the reduced
region; without an upscaler in the graph it is a scissor and nothing more.

So `apply_render_scale` shrinks the **target** instead, in the render world:
`ExtractedCamera::physical_target_size`, `physical_viewport_size` and
`ExtractedView::viewport`. Three rules come with it:

- **In `RenderSystems::PrepareAssets`**, after extraction and before
  `prepare_view_targets` allocates the main texture from those sizes — any later
  and nothing is scaled.
- **Every frame**, because `extract_cameras` re-inserts both components; a
  one-shot write is overwritten before the first pass reads it.
- **All three or none.** `view.viewport` is what `view_transformations.wgsl` and
  the light-cluster factors divide the fragment coordinate by; scaling the target
  without it puts screen-space effects on a different grid from the pass.

The **main-world `Camera` is deliberately untouched**: `world_to_viewport`
(the place pins), egui's layout and `touch.rs`'s finger mapping all read it, and
all three are right because the picture still covers the whole window. Do not
"fix" the light clustering by scaling it — `cluster_factors` is computed in the
render world from the rewritten `ExtractedView`, so it already lines up (verified
against bevy_pbr 0.19; re-check on a Bevy bump).

The query is on `ExtractedCamera`, not a main-world marker: a render-world query
for `FreeCamera` compiles and matches zero entities, and on a change whose only
symptom is frame rate a silent no-op is the worst failure there is.

## `ANativeWindow_setBuffersGeometry` scales the chrome too

The NDK route (a smaller buffer, scaled by the display composer for free) was
rejected because it scales **everything**, including the UI and the street names,
and because it fights winit for the surface geometry, which would have to be
re-applied on every `SurfaceCreated` and kept consistent with the scale factor
`touch.rs` maps through — a mismatch there is taps landing in the wrong place.

## The blit had to filter

Bevy's blit binds a non-filtering sampler, so at a fractional scale it duplicated roughly
every third row and column in a cadence that crawled under camera motion; 1/2 was
chosen partly because it was the only clean value. `make_the_blit_filter` swaps
the blit pipeline's sampler for a linear one at `RenderStartup`, which is what
made a slider possible. At exactly 1.0 nothing is written, so `cargo shots` cannot
move. The web build never needed any of this: its scale is the canvas backing
store, set from CSS in `index.html` before wgpu sees the canvas.
