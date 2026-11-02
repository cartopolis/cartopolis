---
topic: lenses
triggers: [data_layers.rs, scenario.rs, Figures, "Layers panel", lens, lenses, "scenario chrome", render_scenario_chrome, DataLayers::scenario, "bottom sheet", portrait]
updated: 2026-11-02
---

# Lenses: reading the map, apart from how it looks

## v1 — proposed 2026-11-01

![v1](v1.png)

Asked for 2026-11-01: choosing a statistics layer is impractical in portrait,
the Figures tiles toggle like checkboxes but behave as a radio, and the Layers
panel is getting too busy — with scenarios, visualisations and simulations
still to come, they no longer fit under Layers.

The proposal: Layers keeps the switches about how the world looks; everything
with a source and a figure becomes a **lens**, one open at a time, behind its
own disc under Layers in the top-left column (the dock is full at six). On a
phone in portrait the lens is a bottom sheet with three heights — a strip above
the dock (figure, scale, value here), half (the figures as radio rows, each
showing its value at the crosshair), full (the shelf of lenses, grouped by the
question people bring, filtered to the place in view). Tapping a place turns
the list round into the place's card, every figure against town and country;
tapping a row switches the map. Landscape and desktop: the same content as a
left panel, with the clock and futures at its top when the lens has time. The
scenario's title card, states card and clock bar fold into that one surface;
the mode frame round the screen stays.

Agreed 2026-11-01 ("build it as drawn") and built the same day, in
`systems::scenario` (the sheet) and `systems::dock` (the disc). Built
differently from the drawing: the place card is the place under the
crosshair, opened from the strip's place row (a tap on the map still opens
the ring of actions); the shelf has no search or Here filter while it holds
one lens; the disc wears the ordinary engaged plate rather than the mode's
colour (the chrome is monochrome, `monochrome-chrome.md`); "Its town",
"Compare…" and "Talk about it" are not built. Still open: two figures at
once (split screen), a community's own lenses on the shelf.

## v2 — agreed 2026-11-02

![v2](v2.png)

Asked 2026-11-02: once a lens is picked its sheet stays up over half a
phone's map; "we want maximum room to see the map; the key may sit behind an
icon". On a phone in portrait the lens rests as one row — mode, figure, its
value under the crosshair, and a palette button that opens the key (the
strip of v1: source and year, scale, place row). A pick off the shelf or the
figures, or a drag on the map, drops the sheet to that row, and the scale
card under the figures goes, the key holding it. Agreed as drawn the same
day. Landscape and desktop keep the v1 column.

## v3 — agreed 2026-11-02

![v3](v3.png)

Asked against v2 the same day: "why can't a lens be on, and its dialog be
clicked away? The lens icon can say whether a lens is on." On and shown
become two things. A lens on is the coloured map, the mode's frame and the
lit disc, with no sheet and no strip; the disc raises the sheet, which holds
the open figure's key over the figures, and ✕, the disc, a pick or a drag on
the map hide it again. "Lens off" turns it off. The same on every screen.
v2's one-row strip and palette were not kept.
