---
topic: egui-quiet-frame-staleness
triggers: [egui_manager.rs, end_egui_frame, WorldAnchored, place_pins.rs, EguiRenderOutput, force_next_write, keep_egui_fresh, repaint_delay, request_repaint, shot_harness.rs, "--shot", "cargo shots", "stale UI", "frozen HUD", "compass does not move", quiet frame, tessellate]
updated: 2026-10-17
---

# egui's repaint signal does not know about the ECS

`egui_manager::end_egui_frame` skips the tessellate → clone → upload chain when
egui reports `repaint_delay == Duration::MAX`, and lets `egui_pass` redraw the
last-written primitives against the fresh colour target. That skip is worth
keeping — without it a completely static menu bar reallocated and re-uploaded its
whole geometry sixty times a second — but it rests on a premise that is only
sometimes true:

> egui says nothing changed, therefore this frame's shapes equal last frame's.

**`repaint_delay` answers a narrower question than that.** It means "no input
reached me, no animation is running and nobody called `request_repaint`". It says
nothing about a shape list built from `Res<GlobeState>`, a `Transform`, a clock,
or a streaming tile count — because egui has never heard of any of them. So an
overlay whose content comes from the world can go on painting an arbitrarily old
frame while the world moves under it, and *nothing anywhere logs a thing*: the
geometry is valid, the render pass succeeds, the picture is simply from before.

## How it was found (2026-08-15)

A `--shot-mode earth` capture from 14,000 km carried a scale bar reading
**"200 m"**. 200 m is the bar for an altitude near 1.5 km; `hud::render_map_controls`
suppresses the bar entirely above `GLOBE_SHOW_ALT`, and a probe confirmed it had
correctly drawn nothing for the whole 20-second settle. The geometry in the PNG
was from **frame 2 of the run**, where the camera was still at 1478 m on its way
out — the last frame before egui went quiet. Disabling the skip removed the bar;
that is the A/B.

The failure needs a run that *stops* changing, which is precisely what `--settle`
is for. It is therefore worst exactly where it is least visible: a scripted
capture, whose whole purpose is a picture nobody watched being taken.

## What is fixed, and what is not

**Scripted captures are fixed.** `shot_harness::keep_egui_fresh` sets
`force_next_write` every frame for the lifetime of a `ShotHarness`, so a shot run
never takes the skip. It costs one tessellation per frame in a run that is
already spending ~250 ms a frame on lavapipe. **Do not remove it** as a
redundant-looking `force_next_write`: without it every `--shot`, every
`cargo shots` README image and every `--expect` gate is a picture of an unknown
earlier frame.

**The live app still has the property**, and it is not obviously fixable for
free. Anything read off the world and drawn into the overlay can freeze on a
quiet frame — the HUD clock, the coordinate readout, the altitude, the compass
needle while the camera glides or a movement key is *held* (a held key produces
no repeated winit events, so egui sees no input at all). Three options; the
second is taken for the map overlays (below):

- **Tessellate and compare.** Correct and general, and it pays the cost the skip
  exists to avoid.
- **`ctx.request_repaint()` from each ECS-derived overlay when its inputs
  changed** — cheap and targeted, but it is a rule every future overlay has to
  remember, and forgetting it fails silently in this same way.
- **Drop the skip.** Measure first: it was added against a real profile.

## The map pins (2026-10-17)

The first complaint: "when moving around, the pins are not kept in place on all
rendered frames." Walking with a finger held still on the stick, the follow camera
catching up and a GPS glide are all frames with no events, so the pins stood still
on them and jumped on the next frame that carried a touch.

The targeted option is taken for the world-anchored overlays: `place_pins`
(saved places, chat points, posts, people), `chat_overlay::render_chat_bubbles` and
`locate::render_locate_arrow` record every screen position they paint in an
`egui_manager::WorldAnchored`, which requests a repaint on the frames one moved,
appeared or went, and on no others. A still map with pins on it keeps the skip.
**A new overlay painted at a projected world point needs one too**; the POI card
(`data_layers::render_poi_popup`) and the gizmo still have none. `animating()` is
not a substitute: a quarter-second redraw is a stutter against a map drawn at
sixty. The HUD readouts listed above are still open.
