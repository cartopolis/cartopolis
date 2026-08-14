---
topic: ui
triggers: [ui.rs, geo_nav.rs, dock.rs, hud.rs, chat.rs, splash.rs, theme.rs, "top bar", "menu bar", "address bar", "search bar", "main menu", "hamburger", "bottom bar", dock, logo, mark, wordmark, "suggestion", "map controls", compass, "status hud"]
updated: 2026-08-14
---

# The map chrome

Where the search, the menu and the readouts live, and what the four shapes tried
so far each cost. (This was `address-bar.md` while the answer was a bar.)

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

## Rejected: the mark in the corner is the switch

The third version put one always-visible app mark in the top-left corner, which
unfolded the rest of the card to the right and folded it back. It fixed both of
the hover card's problems and lasted until it met a phone held in one hand.

What it cost:

- **The top-left corner is the far corner.** The two controls reached most often
  in this app are search and the menu, and both sat at the end of a diagonal from
  the thumb. The dock had the near edge and was carrying four panel toggles.
- **The card folded half of itself away below ~460 pt of viewport** — the history
  buttons and the camera picker moved into the `☰` menu, and the field took what
  was left. So the phone already had a different menu from the desktop, and every
  control had to be written twice, once labelled and once as a glyph.
- **The dropdown had nowhere to go.** Suggestions hang under the field; the
  on-screen keyboard (`docs/notes/touch-input.md`) docks a slab over the bottom
  half of the screen. Typing a search on a phone put the answers behind the keys.

## What it is now: the dock owns the chrome

Five discs along the bottom edge (`dock.rs`), reading outwards from the map:

| | |
|---|---|
| 🔍 Search | a panel (`geo_nav::render_search_panel`), field and hits, `Ctrl+L` |
| 💬 Chat | only with a session |
| ⚑ Route | the planner |
| ★ Places | saved places — **and where a place is saved**, "+ Place" |
| the mark | the main menu (`ui::render_main_menu`) |

Layers went the other way, to a disc in the **top-left corner** above the compass
(`dock::reserved_top`, read by `hud::render_map_controls`). It is the map's own
settings rather than a destination, which is the same thing the compass, the
locate button and the ⓘ toggle under it are.

The mark disc is the app's own icon, so the way into "everything else" is the
logo — the affordance the corner card had, on the edge a thumb reaches. The card
it opens is anchored to the bottom centre rather than to the disc, because the
dock's width depends on whether there is a session (Chat comes and goes) and a
card measured from the disc would move under a menu that should not move.

**One menu on every viewport.** Everything the compact layout used to fold away
lives in it permanently: Back / Forward / Home, the camera mode, Set Home, Copy
location, Appearance, the Assistant, Settings, About, the account, developer
tools, Exit. A menu whose contents depend on the window width is two menus.

### Consequences worth knowing

- **The status readout is off by default** (`hud::StatusHud`). It is an
  instrument panel — clock, position, altitude, the sun and weather switchers —
  and the two *verbs* it carried have gone where their results appear: ☆ into the
  Places panel, Set Home and Copy location into the main menu. The ⓘ disc at the
  foot of the top-left column brings it back. The column is measured, not pinned:
  the locate button above it only exists when there is an avatar.
- **`dock::reserved_top` / `reserved_bottom` are the two edges** everything else
  anchors against. A second copy of that arithmetic is how the scale bar and the
  thumbstick drift when the dock grows an entry.
- **The dock is not unconditionally at the bottom.** The on-screen keyboard docks
  a slab to the same edge, so the dock stands down entirely while it is up — the
  same `Area`-over-`Area` reasoning driver mode already uses. "It is only an
  overlay" is not an exemption: an `Area` left underneath is still interactive and
  eats the taps meant for the keys.
- **A compact panel is the whole screen**, and egui paints floating `Area`s above
  panels with no z-order that says otherwise. So the discs, the compass column and
  the menu all drop while one is open on a narrow viewport. On a wide one the
  panel docks to the right and never reaches those corners.
- Driver mode suppresses all of it: it owns the screen and puts its manoeuvre
  card in the top-left corner.

## The mark and the wordmark are drawn, not shipped

`theme::paint_mark` is a geometry table in fractions of a radius — a dark disc,
four streets, an accent avenue on the diagonal, an amber marker — and
`theme::paint_wordmark` splits the name in two tones (cream `carto`, accent
`polis`), which is how the account GUI writes it (`jeroen/cartopy`'s
`.app-header .wordmark`). Three reasons it is not a PNG:

- It is wanted at 84 pt on the splash and 20 pt on a dock disc. egui has no mip
  chain, so a 256 px image sampled down to a 20 pt button shimmers as the window
  moves under it.
- The icon PNG is a **Git LFS** object, and the failure mode of a clone without
  LFS is a 130-byte pointer file that decodes to nothing. The splash, the sign-in
  card and the dock each carried a fallback for that; now nothing inside the app
  depends on the file at all. It is still the OS window icon and the desktop
  entry, where a file is what the platform wants.
- `index.html`'s loader owns the first several seconds on the web and is drawn to
  match the in-app splash. The same shape is inline SVG there — no request, no
  404, nothing to arrive late. **Keep the two in step**; they are one screen
  handed over mid-load.

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

In a panel the list is **inline**, not an `Area` under the field, and it does not
close when the field loses focus. A dropdown over the map has to disappear the
moment attention leaves it; a panel has no such duty, and the list is what the
panel is for. It comes down when a hit is picked, when the query is retyped, or
with the panel.

## Two pieces of chrome a shot cannot ask for

Every panel is a boolean a scripted run can set, and `--ui` sets them. Two are
not quite booleans:

- `--ui search` opens the Search panel *and* seeds a query through
  `GeoNavState::seed_search` (`CARTO_SEARCH_QUERY` overrides it). A capture of an
  empty field would document nothing.
- `--ui hud` switches the status readout on, since it is off by default now. Any
  older manifest that expected it in frame has to say so.
