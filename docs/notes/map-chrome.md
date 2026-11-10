---
topic: ui
triggers: [ui.rs, geo_nav.rs, dock.rs, hud.rs, welcome.rs, MainMenu, "face sheet", "World | Community", "space switch", chat.rs, splash.rs, theme.rs, "top bar", "menu bar", "address bar", "search bar", "main menu", "hamburger", "bottom bar", dock, logo, mark, wordmark, "suggestion", "map controls", compass, "status hud", egui::Area, "size-to-content", set_width, "box grows", "stretches across the screen"]
updated: 2026-11-10
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

That is one case of the general rule: **an anchored, size-to-content `Area` must
pin its content width.** Anything inside that reaches for the *available* width —
a `right_to_left` row, `vertical_centered`, a wrapping label — grows the box a
little each frame, because this frame's size becomes next frame's available width,
and the feedback stretches it across the screen. The old status HUD fixed it with
`HUD_WIDTH` and a `hud_box_does_not_grow` test (both went with the readout,
2026-09-10); driver mode's first bottom bar hit it again and swallowed the lower
third of the window in a few frames, which is why every box in `driver.rs` sets its
own width ([navigation-display.md](navigation-display.md)).

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

## What it is now: two spaces, one switch, your face (2026-11-10)

The person approved the v2 navigation (`docs/mockups/redesign`, 2026-11-08,
built 2026-11-10): the app is two spaces, **World** (the map with round
buttons on it) and **Community** (chats, groups, the calendar — the
`RightPanel::Chat` page, the whole screen on a phone and a side panel on a
desktop). The dock below had mixed the two in one row, and "the navigation is
not intuitive" was about that. All of it is `dock.rs`:

| | |
|---|---|
| bottom, centred | **World \| Community**, always — signed out too, in both spaces, in the same place. A 48 pt pill (`theme::p().surface`, the chosen side the accent plate with white ink, the unread count a gold `p().gold`/`on_gold` badge on Community) |
| top | a **"Search places" pill** that opens the Search panel (`Ctrl+L` still does), anchored at the map's top right, at most 420 pt; **your face** at its end |
| right column, under the bar | Layers, the view mode (Follow → Fly → Earth, its word under it), Locate (drawn by `hud` at the slot `dock::right_column` gives it, its caption to its left), Camera where there is one |
| left column, under the bar | the compass, the lens, the light disc, and Say in a session (`dock::left_slot`) |

**Your face replaced the ☰ menu** and opens the same sheet from the search
bar and from the Community page's top right (`ui::render_main_menu`; one
face, not two — on a desktop with Community open the bar drops its own). It
is a bottom sheet on a phone and a 320 pt card under the face on a desktop:
who you are (signed out a grey person, "Not signed in", and Sign in…), Your
profile and Share link (the Community's You page), then list rows (`theme::
list_row`, 50 pt): Avatar, Saved places, Models · Settings, Admin, About ·
Developer tools and Share diagnostics with the developer setting · Exit on the
desktop, and Sign out… last, in red, arming before it acts. Saved places lost
its dock disc to it: the places are yours, not the map's. Community left it:
that is the switch.

**Signed out, Community is not a dead end.** It is the card that says what
signing in is for (`welcome::Welcome::community`, the first-launch card), with
Sign in, Create account and "Not now, just the map"; signing in from it opens
the Community page. Back and Escape put it away as a rung of their own
(`dismiss::Rung::SignedOutCommunity`, last).

**On a phone the switch stays over Community** — it is the way back — and the
page leaves `reserved_bottom` free under it; inside a chat it stands down, since
the composer has the bottom edge. Over every other full-screen panel the map's
chrome drops, as before.

**It is in the panel chain now, at its tail** (`render_dock`, where the old top
switch was). On a desktop a side panel takes the right of the window, and the
bar and the switch have to sit over the map that is left — which is the root
`Ui`'s leftover rect, known only once every panel has drawn. That puts the
faces *after* the sheet they open in the frame, so a tap on a face is a tap
outside the sheet when the sheet tests for one: `ui::MainMenu::face` is last
frame's face rect, which the sheet leaves to the face to answer. (The old order
— the dock before the menu, and a `toggled` flag — had the same problem the
other way round.)

**Measured fits** (unit tests, the phone being 274 × 600 pt — the shots' 412 ×
900 px at 1.5 — and the web phone 214 pt wide): the switch and the bar take the
viewport less the margins, so both fit by construction
(`dock::the_switch_and_the_bar_fit_a_phone`); the right column ends at 268 pt
with Locate and Camera, and the right edge's touch stack grows up from the
switch to 276 pt in Follow
(`map_osd::the_right_edge_controls_clear_the_column_of_round_buttons`). To get
there **the hand button moved beside the face disc** instead of above it (a
fourth tier reached into the column), and **the locate caption moved to the
left of its disc** instead of under it. Earth's zoom rail
stands over the switch on a phone (top at 300 pt) and **hangs under the column
on a desktop** (`MapChrome::right_bottom`, last frame's); centred on the edge,
it met the column on a short window. The floor picker hangs there too.

Everything else that sat at the top edge moved under the bar
(`dock::reserved_top`): the location-sharing chip, the update pill, the
camera-attach chip, the lens's column, the Midas link button (a second column,
left of the right one) and the community filter chip (truncated to the room
between the two columns). The walk guide's turn card keeps the top: the bar
stands down while a walk is guided, as it does in a chat's World view, whose
header has the top.

### Consequences worth knowing

- **`dock::reserved_top` / `reserved_bottom` are the two edges** everything
  else anchors against: the bar's band (68 pt) and the switch's (60 pt, it was
  74 with the dock's words). A second copy of that arithmetic is how the scale
  bar and the thumbstick drift.
- **Place pins cull against both**, being the one overlay drawn after
  everything else at the chrome's `Order::Middle`: a pin raised later wins
  egui's hit test, so a saved place near an edge would take the taps meant for
  the switch or the search bar.
- **The keyboard stands the switch and the bar down**: it docks a slab to the
  bottom edge (or flips to the top), and an `Area` left underneath is still
  interactive and eats the taps meant for the keys.
- **A compact panel is the whole screen**, and egui paints floating `Area`s
  above panels with no z-order that says otherwise; so the bar and both columns
  drop while any panel is open on a phone. The columns also drop with a side
  panel on a desktop, as the compass always has — the panel docks to the edge
  they stand on.
- Driver mode and the camera suppress all of it. Anything new that draws over
  the map has to opt out of driver mode explicitly; there is no ambient "driver
  mode is on" that hides an `Area`.

## Superseded: the dock owns the chrome (2026-08 → 2026-11-10)

Up to six discs along the bottom edge, left to right, each with its word under
it: Search, Say, the view mode, Camera, Saved, Menu; Layers and the lens in the
top-left corner above the compass; a Community | World switch at the top of the
map, signed in only. What it taught, and what the face's sheet kept:

- **Words under discs.** Bare glyphs left the view-mode disc and Saved for a
  person to guess (`docs/mockups/ux-review`, 2026-11-04). The mode disc keeps
  its word; the other buttons in the column are the mockup's bare discs, named
  for `ui_tree` and with a tooltip.
- **The menu disc was the app's mark at first**, and that was wrong: in a row of
  pictures-of-what-they-do a logo reads as decoration. It carried `icons::MENU`
  from 2026-08-17; your face is the third answer and the mockup's.
- **One menu on every viewport**, holding what has nowhere else to be. A menu
  whose contents depend on the window width is two menus. The sheet keeps it.
- **Session-bound entries**: Avatar and Models only in a session (an
  appearance is a thing other people look at; an inventory belongs to an
  account). Back / Forward / Home, Set Home and Copy link left the menu for the
  Search panel, beside the field they navigate from.
- **Entries are list rows, not buttons**: justified `ui.button`s drew a
  full-width plate each and read as a form of primary actions.
- **Spacing, not size, gave way on the web phone**: five discs were 240 pt
  against ≈214, and the row overflowed symmetrically, losing the two discs the
  layout existed to place. One switch of two sides has no such arithmetic.
- **The mode disc spent 2026-10-18 in the top right corner and was not found
  there.** It is back on the right, now under the search bar in a column of
  map buttons — the place every phone map has taught.

## The mark and the wordmark are drawn, not shipped

`theme::paint_mark` is a geometry table in the launcher icon's own 512-unit
square — the brand pin on Groningen's street plan, as in
`assets/icon/cartopolis.svg`, the table and the icons all written by
`tools/brand-mark.ts` (the isometric tower cluster before it was retired
2026-10-10) — and
`theme::paint_wordmark` splits the name in two tones (cream `carto`, accent
`polis`), which is how the account GUI writes it (the atlas web UI's
`.app-header .wordmark`, in `crates/atlas/web`). Three reasons it is not a PNG:

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

There are now **three** copies of that shape — the icon SVG, `paint_mark`, and
the loader's inline SVG — and the app's two are a deliberate reduction of the
first: a flat tile where the icon has a gradient, one motif per facade where the
icon collages a different map fragment onto every tower, and a disc-over-triangle
pin instead of the teardrop path. Painting the full art from egui primitives
would cost several hundred shapes on the frame the splash has least to spare,
and none of the detail survives the 44 px the sign-in card draws it at. What is
*not* reduced is the silhouette, the placement and the palette, which is what a
mark is recognised by. `cargo test -p cartopolis write_mark_preview -- --ignored`
rasterises exactly what egui tessellates, at the four sizes the mark is used at,
which is how the reduction was checked against the icon rather than guessed at.

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
- `--ui profiler` and `--ui switches` open Developer tools *on a named tab*. A
  tab is not reachable by opening the window alone, so `--ui dev` alone
  photographs whichever tab the enum defaults to — the same trap `--ui graphics`
  exists to avoid for Settings. `--ui hud` and `--ui stats` are gone with the two
  overlays they opened.
