---
topic: wasm-performance
triggers: [platform/workers.rs, quality.rs, index.html, lod22.rs, osm_buildings.rs, IoTaskPool, cdp_probe, "wasm", "web build", "phone", "mobile", "render scale", "devicePixelRatio"]
updated: 2026-08-09
---

# What actually costs frames on the web build

The standing rule — **on wasm `IoTaskPool` runs on the main thread**, because
Bevy disables `multi_threaded` there, so parse and mesh work must go to
`platform::workers` — is documented where it bites, at `lod22.rs:349`. This note
holds only what the code cannot.

## The measurement

2026-07-26, `tools/cdp_probe.ts --gpu --profile` against the real adapter
(`vendor=intel architecture=xe-lpg`, Meteor Lake Arc, Mesa 26.1.5) — **not**
SwiftShader, which headless Chromium falls back to silently and which makes any
frame number meaningless.

Before: **median 1–2 fps, 500–2000 ms frames.** Cause was `serde_json` on the
main thread — ~32% of total samples, ~46% of non-idle, led by
`drop_glue::<serde_json::Value>` (14.2%) and `Value as Deserialize::deserialize`
(10.6% + 4.9% visitor).

After moving parse+mesh to `platform::workers` (`1ba27b0`, `11f8d7e`, `9a9b67d`):
**median 52 fps, 16–20 ms frames**, and `serde_json` is absent from the
main-thread profile.

## Ruled out — do not re-test these first

A/B'd on the real GPU, all within noise of baseline: **atmosphere, env-map IBL,
haze, clouds, rain**. GPU load is implausible as a cause at ~520 draws / 1.5 M
triangles on this device. Shadows were never tested and are ~79% of submitted
triangles — `?no_shadows` exists to check that one.

## Still true, still costing

Parsing is still into **untyped `serde_json::Value`** rather than derived structs
(`lod22.rs:459`, `osm_buildings.rs:108` and `:1845`), so dropping the tree costs
more than building it. That cost merely moved off the main thread; it was not
removed. Typing these is the next win if 3DBAG streaming latency matters.

check: `grep -q "serde_json::Value" crates/viberfox/src/systems/map/lod22.rs`

Overpass's validate-before-persist survived the move via a derived
`OverpassRemark` struct (serde skips `elements` without allocating). Its
200-status `remark` error channel must still be rejected before caching —
a 200 response is not a success there.

## The phone tier (2026-08-09)

The measurement above was taken on a workstation GPU, and everything it ruled
out was ruled out *there*. A handset is a different machine in three ways at
once, and `crate::quality` exists to hold the one answer the rest of the client
reads:

* **Fill.** A Fairphone 4 reports `devicePixelRatio` ≈ 2.75, so a canvas laid
  across its 393 CSS-px viewport renders **1080×2340 real pixels** — with HDR,
  an atmosphere, a tonemapper and (until this) bloom over every one of them.
  That is ~2.5 Mpx on an Adreno 619.
* **One thread.** `bevy_tasks` disables its multi-threaded pool on wasm, so
  every system, extract, queue and batch step is serial. Draw *count* costs as
  much as draw cost.
* **Thermals.** 8 cores in `available_parallelism`, of which 6 are efficiency
  cores sharing an envelope with the render thread and the compositor.

`?quality=phone` / `VIBERFOX_QUALITY=phone` forces the tier anywhere, which is
how it gets profiled on a machine that can profile it; `?quality=high` forces it
off. Auto-detection is coarse-pointer **and** a viewport under 900 px — either
alone catches a touchscreen laptop or a narrow desktop window.

### Render scale is a page-layout problem, not a renderer setting

This is the part that is not discoverable from Rust, and the reason the detector
lives in `index.html` rather than in `quality.rs`.

On the web **the client does not choose its render resolution**. wgpu's surface
`configure` writes `canvas.width/height` from the window's physical size
(`wgpu/src/backend/webgpu.rs`), and that size is whatever winit's
`ResizeObserver` measured — it observes with `ResizeObserverBoxOptions::
DevicePixelContentBox`, i.e. CSS content box × DPR
(`winit/src/platform_impl/web/web_sys/resize_scaling.rs`).

Two consequences, both verified in the dependency sources rather than reasoned
about:

* **`Window::resolution.set_scale_factor_override` does not work here.** winit's
  web `request_inner_size` converts the requested physical size back to logical
  and sets the canvas **CSS** size, returning `None`
  (`platform_impl/web/window.rs`). Asking for fewer physical pixels shrinks the
  visible canvas instead of the buffer behind it.
* **A CSS transform does work**, because a transform is a paint operation and
  the device-pixel *content box* is a layout box. So `#stage` is sized to
  viewport ÷ k and scaled back up by k: the canvas covers the screen, the
  backing store is 1/k² of it. Use `transform`, never `zoom` — `zoom` is a
  layout property in the browsers that have it, so it would change the measured
  box and cancel itself out.

**Pointer coordinates survive the transform** — winit takes them from
`offsetX/offsetY` (`web_sys/event.rs`), which the browser reports in the
element's own untransformed space, the same space the backing store is in. Note
`Canvas::position()` *does* use `getBoundingClientRect`, which is
transform-affected; nothing on the input path reads it, and anything new that
does will be wrong by exactly k.

### What is in the phone budget, and why each is there

| Setting | Phone | Reason |
|---|---|---|
| Render scale | ≤1.5 DPR, ≤1.3 Mpx | ~30% of the pixels on a 2.75× screen |
| Shadows | off (1024² if re-enabled) | shadow casters were ~79% of submitted triangles |
| Bloom | `intensity = 0.0` | skips ~13 full-screen passes; the node early-returns on exactly 0 |
| Sky IBL | off | a cubemap + mip chain re-baked every frame |
| Trees, furniture, labels, textured roads, water shader | off | per-cell geometry the single thread meshes *and* draws |
| Clipmap | 424 → **224** tiles | over half the ground draw list and the streaming behind it |
| Globe leaf tiles | 320 → 160 | ~450 draws of planet, from one thread |
| Per-frame build/upload/despawn budgets | halved | these bound the worst-case hitch, not the throughput |
| CPU workers | ≤3 | see thermals above |

The atmosphere deliberately **stays on**: it is the sky, it A/B'd within noise
above, and the alternative is a void.

### Not measured

Nothing here has been run on the device. It was written against the constraints,
and the container it was written in has no GPU and no browser — `cdp_probe`
cannot run there at all, and `--shot` needs a Vulkan driver it does not have. So
the table above is a set of predictions with mechanisms, not results.

The one number that would settle it is a `tools/cdp_probe.ts --gpu --profile`
run, or a frame counter on the handset, **before and after**, at the same place
and altitude. Until someone has that, treat "phone tier holds 30 fps" as
untested — and in particular do not assume the ordering in the table is the
ordering of the wins. Render scale is first on reasoning about fill; if the
device turns out to be main-thread-bound instead, the clipmap radius and the
draw-count rows are the ones doing the work and the resolution could be given
back.
