---
topic: texture-lifetime
triggers: [images.rs, ImageCache, IDLE_FRAMES, CAP_BYTES, LiveImages, "has been destroyed", "Validation Error", Queue::submit, render_halt.rs, "frozen app", "halted renderer", "egui-texture-lifetime", free_texture, photo_viewer]
updated: 2026-11-26
---

# A texture cannot be freed in the frame that recorded work on it

**Destroying a GPU resource is not a drop, and a recorded-but-unsubmitted
command buffer is a live user of it.** `wgpu::Texture::destroy` invalidates the
resource at once, including for command buffers already *recorded* and not yet
*submitted*, and wgpu reports it at submit:

```
Validation Error
  In Queue::submit
    In a draw command, kind: Draw
      Texture with 'egui_texid_Managed(3)' label has been destroyed
```

`render_halt.rs::handle_render_error` halts the renderer on `Validation` (by
design — validation errors recur), so the app keeps running with the picture
frozen at its last frame and nothing else in the log. On a phone that is
indistinguishable from a hang. Not a device loss: the device was healthy and the
command buffer was wrong.

## How it was found (2026-08-16, Adreno)

Opening a saved place's photo on a phone and closing it froze the app for the
rest of its life. egui retired a texture on the `end_pass` of the frame its last
handle was dropped in, and that same pass had just drawn it — dropping the
handle was the consequence of the ✕ the pass had drawn. `egui_wgpu`'s
`free_texture` destroyed it before the render graph submitted. The fix queued the
frees and drained them at the top of the next pass, past `Queue::submit`, as
egui-wgpu's own winit painter does. egui left the client on 2026-11-26; the rule
stayed.

## Where it holds now

- **The library's pictures.** `mithril_bevy`'s `ImageCache` (`images.rs`)
  decodes each `data:` source once and frees what no mounted `<img>` has named
  for `IDLE_FRAMES`, or the least recently used unshown ones past `CAP_BYTES`.
  An evicted image's asset leaves `Assets<Image>` **one sweep after** its entry,
  never in the frame that may have drawn it. Keep that lag if the cache is
  reworked: the photo viewer, the chats and the cards show multi-megabyte
  pictures for seconds and give them back, which is exactly the case that froze
  the phone.
- **`bevy:` live images** (`LiveImages`: the camera, a call's video) are the
  app's and not cached; whoever replaces one owns the same rule.
- **Anything in the render world** that explicitly destroys (rather than drops)
  a texture or buffer has to keep it past the frame that recorded work on it.
