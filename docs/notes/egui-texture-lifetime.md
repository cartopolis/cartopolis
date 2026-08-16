---
topic: egui-texture-lifetime
triggers: [egui_manager.rs, egui_pass, free_texture, EguiWgpuRenderer, textures_delta, load_texture, TextureHandle, "has been destroyed", "Validation Error", Queue::submit, photo_viewer, bookmarks.rs, frozen app, halted renderer]
updated: 2026-08-16
---

# A texture egui retires cannot be destroyed in the pass that drew it

`egui_wgpu::Renderer::free_texture` calls `wgpu::Texture::destroy`. That is not a
refcount drop — it invalidates the resource immediately, including for command
buffers that have already been *recorded* and not yet *submitted*. wgpu reports it
at submit time:

```
Validation Error
  In Queue::submit
    In a draw command, kind: Draw
      Texture with 'egui_texid_Managed(3)' label has been destroyed
```

`lib.rs::handle_render_error` halts the renderer on `Validation` (by design —
validation errors recur), so the app keeps running with the picture frozen at the
last frame it drew. Nothing else is logged. On a phone that is indistinguishable
from a hang.

## Why the frames line up badly

egui puts a texture in `TexturesDelta::free` on the `end_pass` of the frame its
last `TextureHandle` was dropped in. **The shapes of that same pass can still
reference it** — and routinely do, because dropping the handle is usually the
consequence of a widget the pass has just drawn:

```rust
egui::Window::new(&view.name).open(&mut open).show(ctx, |ui| {
    ui.add(egui::Image::new(SizedTexture::new(texture.id(), …)));  // draws texture 3
});
if !open {
    bookmarks.viewing = None;                                      // retires texture 3
}
```

That is the saved-place photo viewer (`systems::bookmarks::photo_viewer`), and it
is how the bug was found: on a phone (2026-08-16, Adreno) opening a place's photo
and closing it again froze the app for the rest of its life.

## The fix, and the rule it generalises to

Frees are **queued**, not applied, at the bottom of `egui_pass`, and drained at
the top of the *next* one — by which time Bevy's render graph has submitted the
command buffers that referenced them. egui-wgpu's own winit painter has exactly
this constraint and solves it the same way; its comment is explicit that the free
must follow `Queue::submit`. Ours cannot free after submit in the same system
because the render graph owns the submit, so "the top of the next pass" is the
equivalent point.

The general rule for anything in the render world: **destroying a GPU resource is
not a drop, and a recorded-but-unsubmitted command buffer is a live user of it.**
If a system explicitly destroys (rather than drops) a texture or buffer, it has to
outlive the frame that recorded work against it.

## What this is not

- Not the same as [`egui-quiet-frame-staleness`](egui-quiet-frame-staleness.md).
  That note is about *geometry* being retained across quiet frames; this one is
  about a *texture* being destroyed too early. They meet only in `egui_pass`.
- Not a device-loss problem, so `handle_render_error`'s `DeviceLost` recovery
  never applied: the device was healthy and the command buffer was wrong.
- Not visible on the desktop in ordinary use, because nothing there retires a
  texture very often — the font atlas is allocated once and the place thumbnails
  live for the run. The photo viewer is the first thing in this client that
  allocates a multi-megabyte texture and gives it back a few seconds later.
