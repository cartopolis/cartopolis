---
topic: touch-input
triggers: [touch.rs, egui_manager.rs, free_camera.rs, onscreen_keyboard.rs, map_osd.rs, index.html, "touch", "pinch", "mobile", "phone", "tablet", "gesture", "keyboard", "on-screen controls", "thumbstick", "text_edit_focused"]
updated: 2026-08-13
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
| pinch | zoom | zoom | follow distance | — |
| twist | yaw | compass heading | azimuth | — |
| two-finger vertical | tilt | — | — | — |

A twist is 1:1 in radians — turn the fingers 90° and the view turns 90°. Drags
stay in points and keep the mouse's sensitivity constants, since `TouchInput`
and `CursorMoved` both report logical points.

**Only the vertical component of the two-finger drag is read.** A twist that
isn't perfectly centred moves the centroid sideways, so taking the horizontal
component too would double-count every rotation as a pan.

**Fly's column is empty on purpose, and it used to be full.** Pinch was W/S,
the two-finger drag was A/D + E/Q, and only the **sign** of each was read — so
the smallest deliberate spread was a full-speed press of W, held for as long as
the fingers stayed down. It flew, in the sense that the camera moved; it was not
steerable. Worse, it gave the one gesture whose meaning is settled across every
map application there is (pinch = zoom) a different meaning in one mode. The
verbs moved to on-screen controls instead (`systems::map_osd`, below) and the
gesture arms were deleted rather than left as a second way to do it.

## The on-screen controls, and why Earth is the touch camera

`systems::map_osd` draws what a finger has no key for. Two clusters, chosen by
camera mode:

- **Fly** — a proportional thumbstick bottom-left (forward/back + strafe) and a
  climb/descend pair on the right. Left thumb translates, right thumb changes
  height, which is where the thumbs already are on a two-handed phone.
- **Earth / Follow** — a zoom pair on the right. Redundant with the pinch by
  design: one-handed is how a phone is held most of the time.

The stick's *magnitude* is kept, which is the whole difference from the gestures
it replaced: `camera_controls` derives a throttle from `move_direction.length()`
(saturating at 1, so a key is exactly as fast as it always was) and a half-push
is half the speed.

Three things it inherits from elsewhere in this tree rather than inventing:
it draws floating `Area`s and is therefore out of the Phase 7b chain, like the
dock and the HUD; it hides itself while a right panel, the keyboard or driver
mode owns the screen, because an `Area` that is merely invisible still eats taps
(the dock's rule verbatim); and it reports a *wish* into a resource rather than
moving the camera, because `camera_controls` already owns the accelerate/glide
curve, the altitude-scaled speed and the ground clamp.

**Earth mode is now what the app opens in** (`free_camera::seed_start_view`),
about 1.5 km over the map anchor. Follow at eye level is the right camera for
the thing you do second and the wrong one for the thing everyone does first,
which is work out where they are — and on a touchscreen it was worse than wrong,
because Follow's only verbs are an orbit drag and a zoom, so the opening screen
offered no way to travel and no horizon to travel by.

## Selecting text with no pointer

Selecting text is a press-drag-release across the glyphs, and on this client
that gesture is spoken for twice over: the touch bridge holds the emulated left
button down for the whole gesture, and a finger held still on a field for half a
second is the long press that opens the world context menu. So "replace what is
in this field" — most of the editing anyone does in an address bar — had no
gesture at all, and the alternative was one backspace per character.

Two keys close it, both routed into egui's *own* handlers rather than a second
implementation:

- **Select-all** emits `Key::A` with `Modifiers::COMMAND`, which is the chord
  `text_selection::cursor_range::on_key_press` already answers.
- **Shift + an arrow** extends the selection, because egui distinguishes an
  arrow carrying `shift` from one that does not. Shift is deliberately *not*
  consumed by an arrow (it is by a character): a selection is grown one press at
  a time, and a one-shot shift would have to be re-armed for every letter.

## The UI is sharp on a phone and still too small to hit

Reported from a Fairphone 4 on Chromium, 2026-08-09: "the ui widgets are very
small/hard to read". This is **not** a DPI bug, and looking for one wastes the
afternoon. `egui_surface` already feeds egui the display's scale factor (on the
web that is `devicePixelRatio`), so text is rendered at full device resolution.
The layout is simply built for a mouse: a point on the web is a CSS pixel
(1/96 in), so `theme::style`'s `interact_size` of 40×24 pt is a **6.3 mm** touch
target where the platform guidelines ask for ~10 mm.

The mechanism is `EguiManager::ui_scale`, a magnification *on top of* the display
scale, set by `?ui_scale=` / `CARTO_UI_SCALE` or defaulted per-platform by
`phone_ui_scale` on `quality::Tier::Phone`.

**But on the web the default is 1.0, because the render budget got there first.**
This is the part to read before reaching for the knob. `perf/wasm-phone-render-budget`
(#27, landed the same day) sizes `#stage` to viewport ÷ k and scales it back up
by k with a CSS transform, where k is `dpr / 1.5` on the phone tier. winit
measures the *untransformed* box, so Bevy's logical viewport shrinks by k and
every egui point is painted across k CSS pixels. On the reported Fairphone 4
(dpr 2.75) k ≈ 1.83, which takes that 24 pt row from 6.3 mm to **11.6 mm**
without anyone touching the UI. Two independent magnifications is 17 mm, and the
viewport lands at 143 pt against a 240 pt panel floor.

So the two changes overlap, and the overlap is only visible if you know the
canvas is a transformed element. Native Android has no such transform and a
point there is a dp (1/160 in), i.e. **3.8 mm** — which is why the default is
1.5 on that platform and 1.0 on the web. Four things are worth keeping:

**Scaling points-per-pixel, not the style.** The alternative — multiplying the
theme's font sizes and spacing — was rejected because it only reaches what the
theme owns. The dock discs, `theme::close_button`, the place pins, the HUD boxes
and the driver rail all carry their own point sizes, so a style-only scale grows
the text and leaves every hand-drawn control the size it was. Scaling ppp is
uniform by construction.

**It is bounded from above by the panels, not by the finger.** The scale
multiplies the widgets by dividing the viewport, and `responsive_right_panel`
floors a compact panel at 240 pt — so the ceiling is wherever the viewport meets
that floor, which on the web phone tier is *already close* before any
magnification (a 393 pt handset presents 214 pt through k ≈ 1.83, so the floor
is doing the wrong thing there on its own — see below).

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

**One detector, borrowed rather than rebuilt.** The tier comes from
`crate::quality`, which `index.html` decides before the module is even fetched
(`wasm-performance.md` explains why the browser has to be the one to decide: the
canvas backing store is set from CSS, so the render scale is a page-layout fact
Rust cannot reach). An earlier draft of this asked `(pointer: coarse)` from Rust
instead, which was a second detector answering nearly the same question — and
two detectors is exactly how the UI ends up finger-sized on a machine rendering
at desktop resolution.

The borrowed answer is not free: `quality` asks for a coarse pointer **and** a
viewport under 900 px, so a **large tablet is `Desktop` and gets mouse-sized
targets** despite being driven by a finger. Wrong for that device, right for a
touchscreen laptop, and `CARTO_UI_SCALE` is the cover until someone has a
tablet to test on. If tablets start mattering, the fix is to publish pointer
coarseness from `index.html` as its own attribute beside `data-quality` — not to
re-add a media query here.

## The compact panel floor inverted on the web phone tier (fixed 2026-08-13)

`responsive_right_panel` used to give a compact panel
`exact_size(viewport_w.max(240.0))` — a floor written so a very narrow window
could not produce a degenerate sliver. On the web phone tier the viewport *is*
narrower than the floor (214 pt through k ≈ 1.83 on a Fairphone 4), so the floor
docked a panel **26 pt wider than the screen it sat in**: the exact failure it
was added to prevent, on the one device class the compact layout exists for.

The fix is `egui_manager::compact_panel_width`, and the reasoning is worth
keeping because the instinct is to add a `min` beside the `max`: a compact panel
*is* the viewport by definition, so there was nothing for a floor to protect. A
100 pt window gets a 100 pt panel — useless but honest — and the only genuinely
degenerate input is a viewport that is not a width at all (zero before the first
surface arrives, or NaN), which is what the remaining fallback covers.

## The client draws its own keyboard, and focus is most of the work

A finger has no keys. On Android there is no IME to fall back on at all
(`android-build.md` — NativeActivity produces no text events), and a phone
browser's IME is unreliable over a canvas, so `systems::onscreen_keyboard` draws
a QWERTY slab in a floating `Area` whenever `ctx.text_edit_focused()` says a
field genuinely holds focus. Not `egui_wants_keyboard_input()`, which is
`memory.focused().is_some()` and true for any focused button — the same wrong
predicate that once swallowed the avatar's movement keys.

Two things about it are not obvious and neither is discoverable from the
symptom:

**Tapping a key destroys the condition that shows the keyboard.** egui's default
is `SurrenderFocusOn::Clicks`, so any click that is not on the field drops the
field's focus — and every key is such a click. So `restore_focus` is the bulk of
the system, and it must **re-lock the `EventFilter` as well as calling
`request_focus`**: `request_focus` installs a default all-false filter, which
`Focus::begin_pass` reads before any widget runs, so without the re-lock the
first arrow key moves focus to the next widget instead of moving the caret.

**That forces the system to run after every system that can draw a text field.**
Registered earlier, the surrender happens *after* the restore and undoes it: the
keyboard types one character into nothing and then hides itself. This is the same
class of ordering constraint as `place_pins` below — and for the same reason,
`place_pins` subtracts the keyboard slab from its cull rect, since a pin left
under the keys is still an interactive `Area` that eats taps meant for them.

Two consequences of the finger, recorded so they are not "simplified" away:
dismissal keys off the pointer rather than `clicked()`, because a finger that
drifts a few points produces a drag and a `clicked()` test would close the
keyboard mid-word; and **key height meets the 48 dp guideline while key width
cannot** — ten columns at 48 dp needs 480 dp against a 393 dp handset. Height is
a hard floor computed from the real scale (`ui_scale`, which on Android *is*
density). That trade is the one every shipping phone keyboard makes, because a
fingertip contact patch is taller than it is wide.

Unverified on glass, like everything else here.

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
