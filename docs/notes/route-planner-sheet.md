---
topic: ui
triggers: [navigation.rs, lib_route.rs, "route panel", "route planner", "bottom sheet", "detent", place_pins.rs, place_card.rs]
updated: 2026-11-26
---

# The route planner is a bottom sheet

Planning a route used to open `RightPanel::Route`, and on a compact viewport a
right panel **is** the viewport. So the
one act the planner exists for — "route to there" — covered the one thing it
produces: the map with the line on it. You could read a turn list; you could not
see the turn. That is the same sentence
[chat-without-a-panel.md](chat-without-a-panel.md) opens with, and the same
answer: the planner is a sheet anchored to the bottom edge now (`lib_route.rs`),
over a map that stays live — the shape every phone map app converged on.

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

The grabber is a tap that flips the detent. (Until 2026-11-26 egui's read its
drag per frame: the answer is one bit, so the first frame past the threshold
said everything a longer drag could.)

## Three things a floating sheet owes that a docked panel did not

A docked panel reserves its side of the map (`data-reserves-map`, which
`UiInput::map_rect` subtracts). A floating sheet reserves nothing, so each of
these had to be paid by hand:

**The place pins had to learn about it.** A pin under the sheet is an
invisible teardrop eating the taps meant for Start — the trap the dock and the
on-screen keyboard are already subtracted for. `RouteState::sheet_rect` publishes
last frame's rect, which `place_pins::map_area` takes off the map.

**The suggestions had to stay on screen.** egui's dropdown hung under the field,
off the bottom of the screen from a bottom sheet; the library draws one list
inside the sheet, under the card.

**The place card had to hide.** It is anchored centre-bottom too, so on a desktop
window the two would have been drawn on top of each other. Hidden rather than
closed: "Route to" is a verb on that place, and it is still the subject when the
planner goes away.

## The keyboard lift

`sheet_bottom` lifts the sheet clear of the on-screen keyboard by its reach
(`onscreen_keyboard::KeyboardInset`, zero while the keys are down or at the top).
The keys open at the bottom and decide their edge only after the screens have
lifted (`onscreen_keyboard`'s module note), so the lift does not make them flip.

## The peek has to say where the route goes

Collapsed, the sheet showed the duration the mode chip above it already showed and
nothing else: the destination is in the body the solve folds away, so a route on
screen did not say where it went or when you would be there. The summary carries
the arrival clock beside the distance and "to <destination>" under them (2026-11-02).
A start that this device cannot offer ("Needs a GPS position") is a dim note in the
button's place, not a disabled button — faded to half contrast over the glass it
read as a rendering fault.

## The two ends are one card, and "My position" is the avatar (2026-11-02)

The endpoints were two form rows carrying five outlined buttons (use my position,
pick on the map, clear, on each end, and swap), which on a phone left the
destination a field narrower than the word in it; the person called it ugly. It is
one card now (`endpoint_card`): a rail down the left, ring for the start and dot
for the destination, the places beside it, one swap for the pair, a small ✕ inside
a field that holds something. The ways to fill an end without typing are chips
under the card ("My position", "Choose on map"), drawn only for the end still
open. A failure is said under the card, on an error plate, not above it.

In egui the chips and the failure were cut off the sheet's bottom on a phone (a
two-line column reserved at zero height; 150 pt charged for a summary not drawn,
leaving the body 113 pt of a 610 pt viewport). The failure is drawn outside the
scrolling body, beside the action row, so it is never the thing that scrolls away.

"My position" used to be the camera, filled once and kept. So it stayed wherever
the camera was the first time the sheet opened: after the map had been at Winsum,
"My position → Winsum" refused as "the same place" from Groningen. It is the
avatar's position when there is an avatar, and a start still reading "My
position" follows the avatar once it is 30 m out (`RouteState::follow_here`). The
camera answers only offline, and does not follow there, or every pan of the map
to look at the route would re-solve it. A too-close refusal under "My position"
now reads "You are already at …", with the distance.

## Rejected: a shorter right panel

The obvious smaller change was to stop the right panel filling the
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
The keyboard lift has **not** been watched on a device or in the emulator.
