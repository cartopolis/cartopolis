---
topic: touch-input
triggers: [touch.rs, ui_input.rs, lib_map_osd.rs, lib_keyboard.rs, free_camera.rs, onscreen_keyboard.rs, map_osd.rs, index.html, "touch", "pinch", "mobile", "phone", "tablet", "gesture", "keyboard", "on-screen controls", "thumbstick", "long press", "double tap", "double_tap", "Touches", "iter_just_released", "tap does nothing", "android tap", "selection handles", picking.rs, gizmo.rs, owns_click, ui_blocked, UiClaimRegions, claim_mouse, CameraInput, touch-action, ui_scale, window_to_ui]
updated: 2026-11-26
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
| The component library (screens, buttons, fields) | Bevy's picking, through `mithril_bevy`'s observers | a pointer per finger |
| World clicks (picking, gizmo, route pick, zoom-to-cursor) | `touch::emulate_mouse` | the settled `Touches` set |
| Camera gestures (pinch, twist, tilt) | `touch::recognize` → `TouchGestures` | the settled `Touches` set |

"Settled" is `Touches::pressed` **plus the fingers that ended inside this frame**
(`fingers_this_frame`), which is not a detail — the bare level loses a fast tap
whole, and that is the section further down.

The world's two cannot use a per-pointer stream: a pinch is a relationship
between two fingers at one instant, which only `Touches` has. (Until
2026-11-26 the UI row was egui's own bridge, `egui_manager::pointer_events`,
over the ordered `WindowEvent` stream.)

The gestures are read per camera mode inside `free_camera::camera_controls`, which
was already at Bevy's 16-parameter cap, so the touch state came in grouped with
the rest of the frame's input as one `CameraInput` `SystemParam` rather than as a
17th parameter.

## Why the world side emulates a mouse instead of growing a touch API

Every world click consumer already gates itself on
`UiInput` and already reads `Window::cursor_position()`. Give
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

## A finger's click is judged by where it landed (fixed 2026-09-25)

The emulated button reaches `ButtonInput` a frame after the finger's events,
so a brisk tap — `Started` in one frame's batch, `Ended` in the next, the
ordinary tap on a handset — reaches `picking::prim_picking` and
`building_select::pick_building` after the finger has lifted, when the UI's
hover state no longer says where it was. A gate that asked "is the UI under
the pointer now" answered wrongly over the bare map, which kept the building
card shut. So `UiInput::owns_click` decides a finger's click by *where it
landed*: over a library screen, or outside `UiInput::map_rect`, it is the
interface's (`ui-input.md`). Both consumers ask it, or `PrimClick` goes stale.

(The egui predicate before it, `egui_wants_pointer_input()`, was only right
while a button was down: this client's hand-built root `Ui` left egui claiming
every point with no button held. It went with egui on 2026-11-26.)

## `Touch::delta()` is stale on a quiet frame

`bevy_input`'s `touch_screen_input_system` only refreshes `previous_position`
when the frame carried touch *events* (a deliberate upstream choice —
bevyengine/bevy#12442). Hold a finger still for one frame and `delta()` hands
back the previous frame's delta a second time, which reads as the map continuing
to slide under a motionless finger. `touch.rs` therefore keeps its own
last-seen positions and never calls `delta()`.

## `Touches` is a level, and a brisk tap never appears in it (fixed 2026-09-22)

`Touches::iter()` yields `pressed`, and `bevy_input` removes an id from
`pressed` the instant it processes that id's `Ended` event. So a frame whose
event batch carries a `Started` **and** an `Ended` for one finger leaves nothing
behind but a `just_released` entry — and `recognize_touch_gestures`, which polls
the level once per frame, saw no finger at all. No press, no click, no verdict,
nothing in any log to say a hand had touched the glass.

**This is why a tap did nothing on Android while every panel still worked.** The
bridges did not read the same thing: egui's read the ordered `TouchInput`
*stream* and so always saw both edges, while the world side polled the
resource. The asymmetry was the bug, and the device only
decided how often it fired — a 100 ms tap spans six 60 Hz frames and is
unmissable on a desktop, but Android delivers touch at 120 Hz or better into a
frame that may be 40 ms long, so on a handset drawing a city the tap lands
entirely between two samples. The *second* tap of a double tap is precisely the
brisk one, which is how the building card became unreachable on a phone
(`docs/notes/selecting-a-building.md`) while the double tap itself was working
exactly as designed.

`fingers_this_frame` reinstates such a finger for one frame. Deliberately less
than synthesising a whole press/release pair: the state machine downstream
already separates a press from its release by a frame (`deferred_release`), so
the finger is adopted and the `Left` press written now, it is absent next frame,
and the ordinary lift path writes the release and records the tap. Conditioned
on the finger being **new** — a touch that was live last frame and released this
one is an ordinary lift, and extending that would delay every release on every
platform to fix a case none of them has.

The general rule this leaves behind: **anything that has to see both edges of a
gesture reads the event stream, not the resource.** A level can only answer what
is true at the moment it is sampled, and the whole of a fast tap is over before
the next sample.

## The browser claims the gestures first

`touch-action: none` on the canvas is load-bearing, and it is **not** redundant
with winit's `preventDefault`: the compositor consults `touch-action` before
dispatching the event at all. Without it a one-finger drag scrolls the page,
two fingers zoom the page, and a double tap zooms — and no `TouchInput` ever
reaches Rust to explain the inertness. `overscroll-behavior: none` on
`html, body` is the same story for pull-to-refresh, and
`-webkit-touch-callout: none` stops the selection callout from covering the map
on the long press that is our context menu.

`touch.rs`'s long press → `MouseButton::Right` is the *world* context menu.
(Inside egui's panels a long touch also fired `clicked()`, so a row with a
context menu had to guard with `!long_touched()` — `place-photos.md`; gone with
egui, 2026-11-26. A library control that grows a long-press menu decides the
same question: the hold must not also act as a tap.)

## Gesture vocabulary, and where it came from

Phone-map convention, not the mouse's:

| Gesture | Earth surface | Earth space | Follow | Fly |
|---|---|---|---|---|
| one finger | pan | spin the globe | orbit | look |
| pinch | zoom about the fingers | zoom toward the fingers | follow distance | — |
| pinch, moving | pan while zooming | spin the globe while zooming | — | — |
| twist | yaw | compass heading | azimuth | — |
| two fingers side by side, sliding up/down | tilt | — | tilt | — |

A twist is 1:1 in radians — turn the fingers 90° and the view turns 90°. Drags
stay in points and keep the mouse's sensitivity constants, since `TouchInput`
and `CursorMoved` both report logical points.

**A pair is a tilt or a map gesture, decided once (`touch::PairLock`).** Until
2026-11-22 the two-finger centroid's vertical motion always tilted, its
horizontal motion was dropped, and the surface pinch zoomed about the middle of
the screen — so a pinch could not be aimed ("tijdens het pinchen is het niet
mogelijk om de richting van de zoom bij te sturen"). Now, after the pair has
moved 10 points (`PAIR_LOCK_PTS`), it is a **tilt** if the fingers lie side by
side and slid vertically together without spreading or turning, and otherwise
the **map gesture**: pinch, twist and centroid pan together. Nothing is reported
while deciding, and the held-back motion is handed over whole at the verdict. On
the surface the pinch's glide is anchored on the ground under the point between
the fingers (`CameraState::pinch_anchor`, an exact homothety of the rig); in
space the centroid is the cursor, so the existing zoom-toward-cursor and the
globe grab already follow it. The twist still turns about the screen's middle,
not about the fingers, so a twist pivoted on one finger drifts a little.

**Fly's column is empty on purpose, and it used to be full.** Pinch was W/S,
the two-finger drag was A/D + E/Q, and only the **sign** of each was read — so
the smallest deliberate spread was a full-speed press of W, held for as long as
the fingers stayed down. It flew, in the sense that the camera moved; it was not
steerable. Worse, it gave the one gesture whose meaning is settled across every
map application there is (pinch = zoom) a different meaning in one mode. The
verbs moved to on-screen controls instead (`systems::map_osd`, below) and the
gesture arms were deleted rather than left as a second way to do it.

## A tap is the cheapest thing a hand can do by accident

The emulated `Left` press fires on the frame a finger **lands**, and it has to:
a tap is over in a few frames, and a bridge that waited for the lift would lose
every short one. The consequence is that the first frame of a pan, of a pinch
and of a long press is also a click — so every consumer that acts on
`just_pressed(Left)` acts on all four.

That was tolerable while the click consumers were small and reversible (pick a
prim, drop a route pin). `building_select::pick_building` is not: it opens a
panel over the map and lays glass on a building, and on a phone it went off
under the thumb at the start of every pan. Reported 2026-09-13 as "the building
info is too easy to open on touch".

So `TouchGestures` answers two more questions about the press `ButtonInput`
reports this frame — [`touch_click()`](../../crates/cartopolis/src/systems/player/touch.rs)
("was it a finger's?") and `double_tap()` ("…and was it the second of a pair?")
— and the building pick asks for both. Four things about it:

- **It is per press, not per session.** `ever()` already latches "has this
  session been touched" and is the wrong instrument here: a touchscreen laptop
  driven with a mouse would be judged by the finger's timings and the finger's
  44 pt slop whenever it was the mouse doing the clicking. Each press is asked
  in the way the device that made it can answer.
- **Both verdicts are shifted a frame.** The press is *written* in
  `emulate_mouse` and reaches `ButtonInput` on the next frame (that ordering is
  the module's own, and deliberate), so the answer has to arrive **with** the
  click and not before it. `recognize_touch_gestures` shifts `pending_*` into
  view at the top of the frame; getting this wrong would gate on the previous
  press and is invisible in any single-frame test.
- **A single tap is inert, not a deselect.** The accident works in both
  directions — a card dismissed by the first frame of a pan is the same
  annoyance — and the card has a ✕.
- **A drag, a long press, or a spent pair is not half of a double tap**, so
  tap-drag-tap and a finger drumming on the glass both fire nothing. The window
  (`DOUBLE_TAP_SECS` = 0.4 s) sits under `LONG_PRESS_SECS` = 0.55 s on purpose:
  the two verbs must not be reachable by the same pair of timings.

**Desktop was given the same verb a day later**, and the reason is worth
recording because the first fix got it wrong: the bullet above used to say a
cursor "has a hover and a still hand to say *this one*". It does — and it is
still holding the same button down to orbit the camera, so a desktop drag over
the city begins with exactly the `just_pressed` a tap does, and the card was
opening on it. The mechanism is not shared, though, and should not be:
`building_select` measures the cursor's pair itself, in twenty lines, because a
cursor does not lift and come back — it sits where the first click left it, so a
6 pt slop between successive *press* positions rejects a drag with nothing
tracking the drag. That shortcut is unavailable to a finger, which is the whole
reason `Press` carries `strayed` and `long_press_sent`.
`docs/notes/selecting-a-building.md` has the pair as one decision.

Nothing else asks for the double tap yet. It is a gate to reach for when a
click consumer *opens* something over the map, not a new default.

## The on-screen controls take their input from Bevy, not the UI (2026-11-25)

Drawn by the component library (`lib_map_osd`), the controls are worked by
`map_osd` (`lib_pair`, `lib_stick`, `lib_jump`, `lib_hand`, `lib_rail`) from
Bevy's input, against the rects it publishes — no widget hears them. Fingers were already `TouchGestures` claims; a mouse press on a
control is claimed the same way (`touch::claim_mouse`, rects in
`UiClaimRegions::pointer`): taken out of `ButtonInput<MouseButton>` before
picking, the gizmo or the camera drag see it, so a drag on the zoom rail no
longer also turns the map. The library's own nodes for the controls stay
non-listening on purpose: a listening node makes the library's single pointer
the world's click gate, and a thumb resting on the stick would swallow every
tap on the map beside it. A long press on the hand disc is `BodyControls`'
`LongPress` (touch's `LONG_PRESS_SECS` / `HOLD_SLOP_PTS`), and its lift is not
also a tap.

## The on-screen controls, and why Earth is the touch camera

`systems::map_osd` draws what a finger has no key for. Three clusters, chosen by
camera mode:

- **Fly** — a proportional thumbstick bottom-left (forward/back + strafe) and a
  climb/descend pair bottom-right. Left thumb translates, right thumb changes
  height, which is where the thumbs already are on a two-handed phone.
- **Follow** — the same stick, walking the *avatar*; the same bottom-right pair,
  zooming the camera; and a jump disc stacked above it (a jump/flight pair until
  avatar flight was removed, 2026-11-14).
- **Earth** — the same bottom-right pair, zooming. Redundant with the pinch by
  design: one-handed is how a phone is held most of the time.

**Both edges anchor to the bottom.** The pair was `RIGHT_CENTER` until
2026-08-15 — halfway up the window, with the jump/flight pair in the corner
below it: two clusters of identical discs on one edge at two unrelated heights,
the busier of them out of a one-handed thumb's reach, and overlapping the scale
bar that was anchored to the same corner. Anchored to the bottom, the right edge
is one column that grows upwards and the pair keeps a single position across
every mode. The scale bar moved to the bottom-left and sits above whatever
`map_osd::bottom_left_reserved` says the stick claims.

**Since 2026-11-10 the right edge has two owners**: the map's column of round
buttons (Layers, the view mode, Locate, Camera) comes down from under the search
bar to 268 pt, and these controls grow up from the World | Community switch. In
Follow the face and hand discs stand beside the jump disc, one column in
(2026-11-11; in tiers above it they came within 8 pt of the column), so on a
600 pt phone the stack stops at 334 pt. The gap between that inner column and the
jump disc is a group gap, so their miss bands do not meet. The locate disc's caption
sits to its left rather than under it
(`map_osd::the_right_edge_controls_clear_the_column_of_round_buttons`).

**The stick floats** (2026-11-14, after the owner found the phone's controls
awkward). A thumb landing anywhere in a band `STICK_FLOAT_WELLS` (2.2) wells
wide — at most 45 % of the screen — takes the stick, and the well stands under
it (`map_osd::stick_origin`), as phone games do it. The fixed 108-pt well had to
be hit without looking, and a miss was a map finger: the thumb meant to walk
turned the camera. The band has the well's height, so nothing above the stick
(the scale bar, the chat transcript) moved; the cost is that a tap in the band
is the stick's and not the map's. At rest, and for a mouse, the well stands at
the band's left end.

The stick's *magnitude* is kept, which is the whole difference from the gestures
it replaced: `camera_controls` derives a throttle from `move_direction.length()`
(saturating at 1, so a key is exactly as fast as it always was) and a half-push
is half the speed.

**Follow's stick is a throttle over the gait ladder, and it has to be.** The
keyboard picks a rung with a held modifier — Alt strolls, Shift runs, neither
walks (`systems::locomotion`) — and a touchscreen has neither key, so a phone
that mapped the stick onto one gait would have exactly one speed. The push
carries the choice instead: `locomotion::speed_for_throttle` slides between the
same `STROLL_SPEED` and `RUN_SPEED` the keys reach, so neither input can ask for
a speed the other cannot. Two consequences worth knowing. The stick's deadzone
means the *slowest* push is already a stroll rather than a creep — that is
the deadzone's cost, paid where it is visible rather than hidden in a curve. And
the keys win outright when both are live (a desktop touchscreen has both), or a
thumb resting inside the deadzone would quietly cap a run.

The wish goes into `OsdInput::walk`, which `avatar::handle_avatar_movement`
consumes — *not* `camera_controls`. That is what makes it work identically in
every run mode: `handle_avatar_movement` owns the one speed ramp, and
`network::send_network_intent` consumes its output rather than re-deriving one,
so the phone's throttle reaches an authoritative sim through the same path a key
does. It also means this is the one control read a frame late: `drive_map_osd`
runs after the driver tick, which runs after the avatar has moved.

It hides itself while a right panel, the keyboard or driver mode owns the
screen, and it reports a *wish* into a resource rather than moving the camera,
because `camera_controls` already owns the accelerate/glide curve, the
altitude-scaled speed and the ground clamp.

**Earth mode is now what the app opens in** (`free_camera::seed_start_view`),
about 1.5 km over the map anchor. Follow at eye level is the right camera for
the thing you do second and the wrong one for the thing everyone does first,
which is work out where they are — and on a touchscreen it was worse than wrong,
because Follow's only verbs were an orbit drag and a zoom, so the opening screen
offered no way to travel and no horizon to travel by. Follow can be walked now,
which removes half of that argument; the other half — that "where am I" is the
first question and a chest-height camera cannot answer it — is untouched, so the
opening view stays Earth.

## A hand has several fingers; the UI's gate is one boolean (fixed 2026-08-16)

Holding the stick froze the view: while a thumb steered it, the UI "had the
pointer", and the world's gate is one boolean for the whole screen, so every
camera gesture stood down — for a finger nowhere near a control. (egui also
had a single pointer, so the second finger never reached a widget; that half
went with egui.) The stick is the one control you *hold* continuously, so on a
phone it made walking and looking mutually exclusive.

**The glass is partitioned, not arbitrated.** `map_osd` publishes the rects it
laid out into `touch::UiClaimRegions`; `recognize_touch_gestures` claims any
finger that **lands** inside one and hides it from the gesture recogniser and the
emulated mouse both. What is left is the map's, so a thumb on the stick plus a
finger on the map is a one-finger drag (it used to be read as half of a pinch,
which is why the view also *jittered* if you managed to move at all). The claim
is decided once, at landing, and held until the finger lifts — a thumb shoved
past the stick's rim is asking for full throttle, not handing itself to the
camera, and a map drag that wanders into the corner is not swallowed by the well.

Two consequences to keep in step:

- **`TouchGestures::ui_blocked(ui_input.wants_pointer())` is what the camera
  asks**, never the UI's verdict directly. It is the same verdict except when a
  claimed finger is down *beside* an unclaimed one, the case a single verdict
  answers for the wrong finger. New world-side touch consumers should use it.
- **The controls read fingers, not widgets.** A rate hit-tests
  `TouchGestures::ui_held` against its own rect, an edge hit-tests
  `ui_released`, and the stick keeps its finger by id; a mouse press on a
  control is claimed the same way (`touch::claim_mouse`). The library's nodes
  for the controls stay non-listening (above).

The regions are read one frame late (drawn in `Update`, consumed in `PreUpdate`).
They are edge-anchored and do not move, so the only stale frame is the one a
control appears or disappears on. Publishing from the draw is still the right way
round: a second copy of the anchor arithmetic in `touch.rs` is how a hit area and
the picture it belongs to drift apart.

**The height is the guideline; the width is a negotiation.** A fingertip
contact patch is taller than it is wide, so a row of small actions trades
horizontal padding for row height — the bargain the on-screen keyboard makes
when ten key columns will not fit in 393 dp. (egui's `Button::small()`, which
zeroed the vertical padding and shrank a control to its glyph, ~21 dp, is gone
with egui, 2026-11-26.)

## The UI is sharp on a phone and still too small to hit

Reported from a Fairphone 4 on Chromium, 2026-08-09: "the ui widgets are very
small/hard to read". This is **not** a DPI bug, and looking for one wastes the
afternoon. The UI already rendered at the display's scale factor (on the web
`devicePixelRatio`), so text was at full device resolution. The layout was
simply built for a mouse: a point on the web is a CSS pixel (1/96 in), so
egui's 40×24 pt `interact_size` was a **6.3 mm** touch target where the
platform guidelines ask for ~10 mm.

The mechanism is `UiMetrics::ui_scale`, a magnification *on top of* the display
scale (the library's `UiScale`), set by `?ui_scale=` / `CARTO_UI_SCALE` or
defaulted per-platform by `phone_ui_scale` on `quality::Tier::Phone`.

**But on the web the default is 1.0, because the render budget got there first.**
This is the part to read before reaching for the knob. `perf/wasm-phone-render-budget`
(#27, landed the same day) sizes `#stage` to viewport ÷ k and scales it back up
by k with a CSS transform, where k is `dpr / 1.5` on the phone tier. winit
measures the *untransformed* box, so Bevy's logical viewport shrinks by k and
every UI point is painted across k CSS pixels. On the reported Fairphone 4
(dpr 2.75) k ≈ 1.83, which takes that 24 pt row from 6.3 mm to **11.6 mm**
without anyone touching the UI. Two independent magnifications is 17 mm, and the
viewport lands at 143 pt against a 240 pt panel floor.

So the two changes overlap, and the overlap is only visible if you know the
canvas is a transformed element. Native Android has no such transform and a
point there is a dp (1/160 in), i.e. **3.8 mm** — which is why the default is
1.5 on that platform and 1.0 on the web. Three things are worth keeping:

**Scale the whole UI, not the style.** Multiplying the theme's font sizes and
spacing was rejected because it only reaches what the theme owns; hand-sized
controls (the dock discs, the pins, the driver rail) stay the size they were.
One scale over every point is uniform by construction.

**It forks UI points from window points, and that is the whole hazard.** They
were the same number everywhere before this existed, so any code holding a
position from `Window::cursor_position`, `TouchInput::position` or
`Camera::world_to_viewport` and handing it to the UI was silently correct. Such
positions go through `UiMetrics::window_to_ui`. The failure mode is nasty
precisely because it is proportional to distance from the top-left corner: at
1.5× a pin near the origin looks fine and one at the far edge is a third of a
screen out, so it reads as a projection bug.

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

**A compact panel *is* the viewport** (fixed 2026-08-13). egui's side panel
floored a compact panel at 240 pt, and on the web phone tier (214 pt through
k ≈ 1.83) that docked a panel 26 pt wider than its screen. The instinct is to
add a `min` beside the `max`; there is nothing for a floor to protect — a
100 pt window gets a 100 pt panel, useless but honest.

## The client draws its own keyboard

A finger has no keys. On Android there is no IME to fall back on at all
(`android-build.md` — NativeActivity produces no text events), and a phone
browser's IME is unreliable over a canvas, so the client draws its own
(`systems::lib_keyboard`, driven by `onscreen_keyboard`; how the keys reach a
library field without taking its focus is `component-library-in-the-client.md`).
It rises only while a text field genuinely holds the focus — never while a
button merely does, the predicate that once swallowed the avatar's movement keys
(`ui-input.md`).

Two consequences of the finger, recorded so they are not "simplified" away: a
key types as its finger lifts and keys off the finger rather than a click,
because a finger that drifts a few points produces a drag; and **key height
meets the 48 dp guideline while key width cannot** — ten columns at 48 dp needs
480 dp against a 393 dp handset. Height is a hard floor computed from the real
scale (`ui_scale`, which on Android *is* density). That trade is the one every
shipping phone keyboard makes.

Until 2026-11-26 the keys were an egui `Area` and most of this section was egui:
focus surrendered by every key tap (`restore_focus`), `move_to_top` demoting a
reopened keyboard below the sign-in card's backdrop for the rest of the run, and
`text_touch`'s caret and selection handles and the password eye. Those went with
egui's fields; the select-all key and the arrows are what a finger has to edit
a selection with now.

## Not covered

- **Marquee select** needs Ctrl, which a finger has no way to hold. Unreachable
  on touch by construction; not worth a gesture.
- **Clipboard keys** on the on-screen keyboard. Ctrl+V has no finger equivalent.
- **Caret handles** on the library's fields: a tap cannot place the caret
  between two particular characters.
- **Three or more fingers** are deliberately inert rather than guessed at.
  Nothing wants a three-finger verb, and picking two of the three would make the
  reading depend on which fingers happened to be lowest-id.
- **Verification on real glass.** Everything here is unit-tested at the
  recogniser, and it cannot be exercised end-to-end in the
  container (no GPU, no touchscreen). The gesture *feel* — sensitivities,
  deadzones, the twist sign — has never been checked on a device.
