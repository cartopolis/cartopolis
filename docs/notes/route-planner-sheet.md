---
topic: ui
triggers: [navigation.rs, "route panel", "route planner", "bottom sheet", "detent", place_pins.rs, place_card.rs, egui_manager.rs, "responsive_right_panel"]
updated: 2026-08-23
---

# The route planner is a bottom sheet

Planning a route used to open `RightPanel::Route`, and on a compact viewport a
right panel **is** the viewport (`egui_manager::responsive_right_panel`). So the
one act the planner exists for — "route to there" — covered the one thing it
produces: the map with the line on it. You could read a turn list; you could not
see the turn. That is the same sentence
[chat-without-a-panel.md](chat-without-a-panel.md) opens with, and the same
answer: the planner is a floating `Area` anchored to the bottom edge now, over a
map that stays live — the shape every phone map app converged on.

## Two detents, and neither is a preference

`navigation::SheetDetent` is `Peek` (mode chips, summary, actions) or `Full`
(those plus the endpoint fields and the turn list, in one scrolling body). What
matters is that the sheet moves **by itself**, in `effective_detent`:

- **A solved route collapses it to `Peek`**, written where the solve lands in
  `drive_route_solver`. The answer is on the map; the thing that asked the
  question gets out from in front of it.
- **No route forces `Full`.** There is nothing to collapse *to* — with no
  destination the fields are the entire content, and a planner peeking at a blank
  summary is a control that has hidden its only input.
- **An armed map pick forces `Peek`**, over either of the above: the next thing
  you have to do is click the map, and the sheet is on the map. The "Click the
  map to set the destination…" line therefore lives *outside* the collapsing
  body, or the instruction would vanish with the collapse it causes.

The grabber reads its drag **per frame** rather than accumulating from a drag
origin. The answer is one bit, so the first frame past the threshold has already
said everything a longer drag could; the alternative is cross-frame state for a
two-state control.

## Three things a floating sheet owes that a docked panel did not

A docked panel takes width out of the shared root `Ui`, and half this client's
layout is written against what is left of it. An `Area` takes nothing, so each of
these had to be paid by hand:

**The place pins had to learn about it.** They are floating `Area`s at the same
`Order`, drawn *later* in the frame, which is exactly what wins egui's hit test
within an order (`place_pins`' module note). A pin under the sheet is an
invisible teardrop eating the taps meant for Start — the trap the dock and the
on-screen keyboard are already subtracted for. `RouteState::sheet_rect` publishes
last frame's rect for the same reason `dock::reserved_bottom` is public.

**The suggestion dropdown had to learn which way to open.** It hung off
`field_rect.left_bottom()`, which from a bottom sheet is off the bottom of the
screen: the suggestions for the field the whole panel is about would have been
invisible. It pivots on the field's top edge when the field is in the lower half
(`Area::pivot`), which needs no height — egui grows the box away from the pivot.

**The place card had to hide.** It is anchored centre-bottom too, so on a desktop
window the two would have been drawn on top of each other. Hidden rather than
closed: "Route to" is a verb on that place, and it is still the subject when the
planner goes away.

## The keyboard lift costs one frame, and that is affordable here

`sheet_bottom` lifts the sheet clear of the on-screen keyboard when
`OnScreenKeyboard::is_open()`, which is a frame behind the focus that opened it.
So the first frame after tapping a field, `onscreen_keyboard::layout` still sees
the field down at the bottom edge and puts that slab on the **top** edge; the
next frame the sheet has moved up and the keys drop back. `chat_overlay`'s
composer cannot take that frame — it never lifts, so the flip would stick for as
long as the field holds focus, which is why that module *predicts* the height
instead. A surface that does lift self-corrects, so it uses the simpler,
non-flickering trigger (the keyboard holds its target across the frame egui takes
focus away; `text_edit_focused()` does not).

## Rejected: a shorter right panel

The obvious smaller change was to stop `responsive_right_panel` filling the
compact viewport — dock it to, say, 70 % and leave a strip of map. It loses on
both axes it was meant to win. The strip that is left is a *column* down one
side, and a route is a line across a city: the useful thing to leave visible is
the middle of the screen, not a slice of its edge. And the panel is still a
panel, so it still suppresses the dock, the map controls and the HUD wholesale
while it is up, for a planner that mostly wants to show one number and a button.

## Not verified

The sheet has been shot headlessly at 412×915 and 1280×800 (`--ui route`), which
is geometry and placement — trustworthy on lavapipe
([headless-shots-software-renderer.md](headless-shots-software-renderer.md)).
The one-frame keyboard flip above has **not** been watched on a device or in the
emulator; it is reasoned from `onscreen_keyboard::layout`, not measured.
