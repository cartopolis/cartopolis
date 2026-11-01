---
topic: lenses
triggers: [data_layers.rs, scenario.rs, Figures, "Layers panel", lens, lenses, "scenario chrome", render_scenario_chrome, DataLayers::scenario, "bottom sheet", portrait]
updated: 2026-11-01
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

Nothing decided yet. Open: two figures at once (split screen), the button's
name and icon, a community's own lenses on the shelf.
