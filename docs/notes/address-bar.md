---
topic: ui
triggers: [ui.rs, geo_nav.rs, dock.rs, chat.rs, "top bar", "menu bar", "address bar", "search bar", "hamburger", "bottom bar", dock, logo, "suggestion", "map controls", compass]
updated: 2026-08-10
---

# The address bar

The map's search and navigation chrome (`ui::render_menu_bar`,
`geo_nav::nav_bar_ui`). Three shapes have been tried; this is what each one cost,
so the fourth attempt starts from the reasons rather than the pictures.

## Rejected: the full-width strip

The first version was a strip spanning the viewport, in the manner of a
desktop-app menu bar. It carries roughly 400 pt of controls, so on any window
wider than that most of the band existed only to hold a background — an opaque
46 pt slab across the top of the map, permanently, for nothing. It also forced
every future control to be placed relative to a *bar*, not to a corner.

Note the mechanical trap that kept it wide after the strip was abandoned:
`egui::MenuBar::ui` ends with `ui.set_min_size(vec2(ui.available_width(), …))`.
That is correct for a bar that spans a window and exactly wrong for a card that
sizes to its content — inside an `Area` it stretched the card ~230 pt past the
last button, which reads as a layout bug and isn't one. A card uses a plain
`ui.horizontal`; `MenuBar`'s other job (a shared config so hovering slides
between open menus) is worth nothing with a single menu in the row.

## Rejected: the hover-reveal card

The second version shrank to a card and hid itself, dropping in when the pointer
grazed the top ~6 pt of the window. Two problems, both fatal:

- **It cannot be discovered.** Nothing is drawn, so there is nothing to aim at.
  The only way to learn the gesture is to be told it.
- **The top edge of the window is not free real estate.** It is the route to the
  title bar, the window controls and, on a tiling desktop, the neighbouring
  window. The bar fired on trips that had nothing to do with it.

It also needed a keep-alive zone that grew to cover the card *and* its suggestion
dropdown, because the dropdown hangs below the bar and reaching for a suggestion
would otherwise dismiss the thing that owns it. That plumbing is a symptom: a
surface whose visibility is a function of pointer position needs a model of
everywhere the pointer might legitimately be.

## What it is now: the mark is the switch

One always-visible app mark in the top-left corner. Clicking it unfolds the rest
of the card to the right; clicking it again folds it back. The logo **does not
move** between the two states — the folded card keeps the open one's margins and
drops only its paint — so the bar reads as growing out of the mark rather than
appearing beside it. `Ctrl+L` unfolds and focuses the field, as in a browser.

The folded card draws no frame of its own: the icon asset is already a rounded
teal card, and a second one around it only muddies the mark.

Consequences worth knowing:

- The corner is now **permanently occupied**, so the compass and the locate
  button under it start below the mark (`ui::MAP_CONTROLS_TOP`, read by
  `hud::render_map_controls`). Before this, the bar and the compass shared the
  corner and the bar simply covered it whenever it was up.
- The mark appears in every scripted capture, README images included. `--hide-ui`
  still drops it.
- Driver mode suppresses the whole thing, mark included: it owns the screen and
  puts its manoeuvre card in this exact corner.

## Where a control goes

The chrome has three homes now, and the rule that sorts them is *how often you
touch it while looking at the map*:

- **The dock** (`dock.rs`, round buttons centred at the bottom) — the four places
  you navigate between: Chat, Layers, Route, Places. One at a time.
- **The address bar** — anything about *where you are*: history, Home, search,
  and the camera mode. The camera picker sat in the bottom bar for as long as
  this strip auto-hid and could not be relied on; that reason is gone.
- **The `☰` menu** — things you visit rather than flip: Appearance, the
  Assistant, developer tools, Exit.

**The dock is not unconditionally at the bottom** (2026-08-10). On a phone the
address bar is typed into with `systems::onscreen_keyboard`
(`docs/notes/touch-input.md`), which docks a slab to the same edge — so the dock
stands down entirely while the keyboard is up, the same `Area`-over-`Area`
reasoning driver mode already uses. Anything else that claims the bottom edge has
to make the same arrangement, and "it is only an overlay" is not an exemption: an
`Area` left underneath is still interactive and eats the taps meant for the keys.

The bottom bar this replaced held all three kinds at once, plus the nearby-chat
composer, in one wide strip of labelled text buttons. The composer now lives in
the Chat panel, where the scope combo box beside it turned out to be redundant
with the panel's own channel tabs.

Two mechanical notes. **Icon glyphs must be probed, not chosen**: that bar shipped
`🧭` for Route, `🤖` for the Assistant and `🧍` for Appearance, and all three drew
as empty boxes for their entire life, because nothing in the pipeline complains —
`theme::first_drawable` exists so an icon can't be a guess. And **the dock is not
session-bound**: the old bar lived inside `chat::render_chat_panel`, behind its
`OnlineSession` guard, so `--offline` had no way to open Layers at all. Only the
Chat entry needs a server, and only it is hidden without one.

## The suggestion rows carry a distance, and that is the point

Nominatim answers with one flattened `display_name` — "Amsterdam, North Holland,
Netherlands". Rendered as a single truncated line, the eight Amsterdams a public
index returns are eight identical rows: everything that tells them apart lives
past the first comma, which is exactly what truncation eats. So a hit is split at
parse time into name / kind / address context (`GeoSuggestion::from_nominatim`)
and drawn on two lines, with the great-circle distance and compass bearing from
the current camera position leading the second.

Two things about that:

- **Haversine, not the equirectangular approximation** `driver::metres_between`
  uses. That one measures tens of metres between route vertices, where the flat
  approximation is sub-millimetre; this one routinely measures thousands of
  kilometres, where it is out by hundreds.
- **The direction is a letter, not an arrow.** egui's bundled font stack has none
  of `← → ↑ ↓ ↖ ↗ ↘ ↙` (see `theme::first_drawable`), so an arrow glyph would be
  a column of empty boxes. "147 km SW" always renders.

The bearing is the great-circle initial bearing, so a hit in the American Midwest
reads NW from the Netherlands rather than W. That is correct and occasionally
surprising.

## The address bar is the one piece of chrome a shot cannot ask for

Every other panel is a boolean a scripted run can set. This one is unfolded by a
click, and a headless run has no pointer — so `--ui search` seeds a query through
`GeoNavState::seed_search` instead, which both unfolds the bar (the field takes
focus) and fills the dropdown. `VIBE_SEARCH_QUERY` overrides the demo query. A
capture of an empty field would document nothing.
