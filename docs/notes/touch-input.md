---
topic: touch-input
triggers: [touch.rs, egui_manager.rs, free_camera.rs, index.html, "touch", "pinch", "mobile", "phone", "tablet", "gesture"]
updated: 2026-08-09
---

# Touch is a separate input stream, not a flavour of the mouse

The thing that makes this non-obvious: **a touch pointer produces no mouse events
at all**, so a client that reads only `ButtonInput<MouseButton>` and
`CursorMoved` is not *degraded* on a phone, it is completely inert — no camera,
no buttons, no panels, and nothing in the log to say why. That was the state of
the web build until 2026-08-06.

The mechanism is upstream and not ours to change. winit's web backend
(`platform_impl/web/event_loop/window_target.rs`) registers separate callbacks
for mouse and touch pointers; only the mouse one emits `CursorMoved` +
`MouseInput`. A `pointerType == "touch"` pointer goes out as `WindowEvent::Touch`
and nothing else. Bevy passes that through unchanged as `TouchInput`, and its
`Touches` resource is the only place a finger ever appears. The same split
applies natively — the web is just where it is guaranteed to be hit, because
that is where the phones are.

## Three bridges, because there are three consumers

Not one: each consumer needs a different shape of the same events, and merging
them was tried on paper and abandoned.

| Consumer | Bridge | Reads |
|---|---|---|
| egui (panels, buttons, fields) | `egui_manager::pointer_events` | the ordered `WindowEvent` stream |
| World clicks (picking, gizmo, route pick, zoom-to-cursor) | `touch::emulate_mouse` | the settled `Touches` set |
| Camera gestures (pinch, twist, tilt) | `touch::recognize` → `TouchGestures` | the settled `Touches` set |

egui cannot share the other two's source: it needs the events **in stream
order**, because a press must carry the position it had at its own point in the
batch (that is the whole of `take_pass_events`, and it predates touch). The
other two cannot use the stream: a pinch is a relationship between two fingers
at one instant, which the event stream does not have — only `Touches` does.

## Why the world side emulates a mouse instead of growing a touch API

Every world click consumer already gates itself on
`egui_wants_pointer_input()` and already reads `Window::cursor_position()`. Give
the primary finger the left button and the cursor, and picking, the gizmo drag,
the route pick, the layers pick and the globe's zoom-toward-cursor all work with
no edit at any of those sites. The alternative — a `TouchGestures::tap` field
plus a branch in each consumer — is five files of duplicated gating that can
drift apart, to express something the mouse already expresses.

Two consequences worth knowing:

- The emulated button arrives **one frame after** the cursor moved with the
  finger, because the recogniser runs after `InputSystems` (which is what fills
  `Touches`). Invisible in practice, and the alternative is reading the raw event
  stream and duplicating the pair bookkeeping.
- Press and release are never written in the same frame. See below — this one is
  not a nicety.

## `egui_wants_pointer_input()` is only meaningful while a button is down

This is the sharp edge, and it long predates touch.

`Context::is_pointer_over_egui()` resolves the Background layer — which egui
registers over the **whole screen** (`context.rs`: "Ensure we register the
background area so panels and background ui can catch clicks") — by asking
whether the pointer is outside `root_ui_available_rect`. Only `Context::run`
sets that field, and this client drives egui with `begin_pass`/`end_pass` and a
hand-built root `Ui` instead. So the field is never set,
`is_pointer_over_egui()` is **unconditionally true**, and

```
egui_wants_pointer_input() == is_using_pointer() || !any_pointer_down()
```

collapses to `is_using_pointer()` while a button is held and to `true` when none
is. Every call site happens to consult it on a press or during a drag, so it has
always answered correctly — by luck of the calling convention, not by being
right.

Touch has to respect that. It is why the egui bridge holds a pointer button down
for the whole gesture (so the camera's gate keeps working), and why a tap that
starts *and* finishes inside one frame — not rare on the web, where a frame can
run 100 ms — must not deliver `just_pressed` on a frame where egui has already
seen the lift: the gate would report "egui wants it" and swallow the tap,
intermittently and only on slow frames.

Fixing the root cause (setting `root_ui_available_rect` from `end_egui_frame`)
is a one-line change and was deliberately **not** made here: it silently alters
the answer at six call sites that currently work, which is not something to
bundle into an input feature. Worth doing on its own, with the click behaviour of
each site checked.

## `Touch::delta()` is stale on a quiet frame

`bevy_input`'s `touch_screen_input_system` only refreshes `previous_position`
when the frame carried touch *events* (a deliberate upstream choice —
bevyengine/bevy#12442). Hold a finger still for one frame and `delta()` hands
back the previous frame's delta a second time, which reads as the map continuing
to slide under a motionless finger. `touch.rs` therefore keeps its own
last-seen positions and never calls `delta()`.

## The browser claims the gestures first

`touch-action: none` on the canvas is load-bearing, and it is **not** redundant
with winit's `preventDefault`: the compositor consults `touch-action` before
dispatching the event at all. Without it a one-finger drag scrolls the page,
two fingers zoom the page, and a double tap zooms — and no `TouchInput` ever
reaches Rust to explain the inertness. `overscroll-behavior: none` on
`html, body` is the same story for pull-to-refresh, and
`-webkit-touch-callout: none` stops the selection callout from covering the map
on the long press that is our context menu.

## Gesture vocabulary, and where it came from

Phone-map convention (Google Maps, Organic Maps), not the mouse's:

| Gesture | Earth surface | Earth space | Follow | Fly |
|---|---|---|---|---|
| one finger | pan | spin the globe | orbit | look |
| pinch | zoom | zoom | follow distance | forward/back |
| twist | yaw | compass heading | azimuth | — |
| two-finger vertical | tilt | — | — | rise/fall |

A twist is 1:1 in radians — turn the fingers 90° and the view turns 90°. Drags
stay in points and keep the mouse's sensitivity constants, since `TouchInput`
and `CursorMoved` both report logical points.

**Only the vertical component of the two-finger drag is read.** A twist that
isn't perfectly centred moves the centroid sideways, so taking the horizontal
component too would double-count every rotation as a pan.

## The UI is sharp on a phone and still too small to hit

Reported from a Fairphone 4 on Chromium, 2026-08-09: "the ui widgets are very
small/hard to read". This is **not** a DPI bug, and looking for one wastes the
afternoon. `egui_surface` already feeds egui the display's scale factor (on the
web that is `devicePixelRatio`), so text is rendered at full device resolution.
The layout is simply built for a mouse: a point on the web is a CSS pixel
(1/96 in), so `theme::style`'s `interact_size` of 40×24 pt is a **6.3 mm** touch
target where the platform guidelines ask for ~10 mm.

The fix is `EguiManager::ui_scale`, a magnification *on top of* the display
scale, defaulting to `TOUCH_UI_SCALE` (1.5×) where the browser answers
`(pointer: coarse)` and overridable per-device with `?ui_scale=1.3` /
`VIBERFOX_UI_SCALE`. Four things about it are worth keeping:

**Scaling points-per-pixel, not the style.** The alternative — multiplying the
theme's font sizes and spacing — was rejected because it only reaches what the
theme owns. The dock discs, `theme::close_button`, the place pins, the HUD boxes
and the driver rail all carry their own point sizes, so a style-only scale grows
the text and leaves every hand-drawn control the size it was. Scaling ppp is
uniform by construction.

**1.5 is bounded from above by the panels, not by the finger.** The scale
multiplies the widgets by dividing the viewport: a 393 pt phone becomes 262 pt,
and `responsive_right_panel` floors a compact panel at 240 pt. 2× would dock a
panel wider than the screen it sits in.

**It forks egui points from window points, and that is the whole hazard.** They
were the same number everywhere before this existed, so any code holding a
position from `Window::cursor_position`, `TouchInput::position` or
`Camera::world_to_viewport` and handing it to egui was silently correct. Four
such places existed — the context menu, the place pins, the POI popup and the
marquee rect — and all four are now routed through `EguiManager::window_to_ui`.
The failure mode is nasty precisely because it is proportional to distance from
the top-left corner: at 1.5× a pin near the origin looks fine and one at the far
edge is a third of a screen out, so it reads as a projection bug. `boot_splash`
was a fifth, sizing its backdrop from `Window::width()`.

**Asked of the browser, not inferred from the first touch.** `(pointer: coarse)`
is answerable before the first frame is laid out; waiting for a `TouchInput`
would relay the entire UI out from under whatever finger triggered it. Android
is a native target and answers `true` unconditionally — one more of the
`cfg(not(wasm32))` seams `android-build.md` warns about.

## Not covered

- **Marquee select** needs Ctrl, which a finger has no way to hold. Unreachable
  on touch by construction; not worth a gesture.
- **Three or more fingers** are deliberately inert rather than guessed at.
  Nothing wants a three-finger verb, and picking two of the three would make the
  reading depend on which fingers happened to be lowest-id.
- **Verification on real glass.** Everything here is unit-tested at the
  recogniser and the egui bridge, and neither can be exercised end-to-end in the
  container (no GPU, no touchscreen). The gesture *feel* — sensitivities,
  deadzones, the twist sign — has never been checked on a device.
