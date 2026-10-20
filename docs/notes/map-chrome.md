---
topic: ui
triggers: [ui.rs, geo_nav.rs, dock.rs, hud.rs, chat.rs, splash.rs, theme.rs, "top bar", "menu bar", "address bar", "search bar", "main menu", "hamburger", "bottom bar", dock, logo, mark, wordmark, "suggestion", "map controls", compass, "status hud", egui::Area, "size-to-content", set_width, "box grows", "stretches across the screen"]
updated: 2026-10-20
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

## What it is now: the dock owns the chrome

Up to six discs along the bottom edge (`dock.rs`), left to right:

| | |
|---|---|
| 🔍 Search | a panel (`geo_nav::render_search_panel`), field and hits, `Ctrl+L` |
| 💬 Chat | only with a session |
| view mode | Follow → Fly → Earth, one tap a step (`dock::mode_disc`) |
| 📷 Camera | orange; not on the web, which has no camera (`camera_screen`) |
| ★ Places | saved places — **and where a place is saved**, "+ Place" |
| the mark | the main menu (`ui::render_main_menu`) |

Layers went the other way, to a disc in the **top-left corner** above the compass
(`dock::reserved_top`, read by `hud::render_map_controls`). It is the map's own
settings rather than a destination, which is the same thing the compass and the
locate button under it are.

The menu disc closes the row. It was the app's own **mark** at first — the way
into "everything else" being the logo, which is the affordance the corner card
had — and that was wrong for one reason: in a row where every other disc is a
picture of what it does, a logo is decoration, and nothing about it said there
was a menu behind it. It carries `icons::MENU` since 2026-08-17; the mark is
still the splash's and the sign-in card's, where a brand belongs. The card it
opens is anchored to the bottom centre rather than to the disc, because the dock's
width depends on whether there is a session (Chat comes and goes) and a card
measured from the disc would move under a menu that should not move.

**One menu on every viewport**, and it holds what has nowhere else to be: the
account (first, and a single **Log out** once there is a session), the camera
mode, Appearance, the Assistant, Settings, About, developer tools, diagnostics,
Exit. A menu whose contents depend on the window width is two menus.

**Three entries are session-bound**: Nearby chat, Models and — since
2026-09-10 — Appearance. The first two are obvious (there is nobody to have said
anything, and an inventory belongs to an account). Appearance is the arguable
one, and it costs something real: an offline avatar is on screen wearing whatever
`user.json` last remembered, and this hides the only way to change a body you can
still see. The argument for it anyway is that an appearance is a thing other
people look at — that is what it is *for* — so alone on the map the panel is a
wardrobe with no occasion. Nothing is lost; the saved choice comes back with the
session that gives it a point.

**What left it, and why.** Back / Forward / Home and Set Home / Copy link went to
the **Search panel** — a history belongs beside the field you navigate with, and
both view actions name the place that field just took you to. The menu had become
half a navigator, two taps from the address bar it was a history of.

**The entries are list rows, not buttons** (`theme::menu_entry`). Justified
`ui.button`s draw a full-width plate each, so the card read as a stack of primary
actions — with the four `selectable_label` panel entries between them drawing no
frame at all, i.e. two visual vocabularies in one card and neither of them saying
which entries were *on*. A row highlights on hover and carries its state in the
accent. The one deliberate plate left is the camera switcher, which is three
`theme::chip`s on one line: it is a switcher, chips are what this app's switchers
look like, and three list rows for it pushed Exit below the fold.

### Consequences worth knowing

- **The status readout is gone** (2026-09-10), and so is the performance
  overlay. Both were anchored boxes over the map that duplicated a panel: the
  readout's position, altitude and street address are the Developer tools' View
  tab, its sun and weather chips are Settings ▸ World, and the overlay's frame
  rate and entity count are the Frame tab, which says considerably more (worst
  frame, jitter, triangles by producer, upload bytes, residency). Both existed
  because the panel holding those numbers was once behind a keyboard chord —
  which stopped being true when Developer tools got a menu entry, and neither
  removal was made until somebody noticed the reason had expired. The
  top-left column is compass and locate only now; it is measured, not pinned,
  since the locate button only exists when there is an avatar.
- **`dock::reserved_top` / `reserved_bottom` are the two edges** everything else
  anchors against. A second copy of that arithmetic is how the scale bar and the
  thumbstick drift when the dock grows an entry.
- **Place pins cull against `reserved_top`/`reserved_bottom` too.** They are the
  one overlay drawn *after* everything else, at the same `Order::Middle` the dock
  uses, so within that order a pin raised later wins egui's hit test. On a phone
  the dock is ~240 pt of a 274 pt viewport, so a saved place near the bottom edge
  was taking the taps meant for Search and the app menu. Same reasoning as the
  keyboard subtraction beside it; the two edges were simply never added.
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
  card in the top-left corner. Note this held only for the chrome *written* for
  driver mode — the place pins and the interact prompt came from elsewhere and
  never learned about it, so teardrops kept drawing over the navigation map (and
  a pin's click is a `Teleport`) and driving past a bench popped "Sit at Bench"
  into the speed pill's band. Anything new that draws over the map has to opt
  out explicitly; there is no ambient "driver mode is on" that hides an `Area`.
- **The dock's spacing is responsive, its disc size is not.** Signed in the row
  is five discs = 240 pt, against ≈214 pt on the web phone tier — it overflowed
  symmetrically, losing the Search disc and the mark, which are the two the
  layout exists to place. `dock::disc_spacing` closes the gaps instead of
  shrinking the targets. A sixth entry does not fit the web tier at any spacing, and the
  answer then is that Chat folds into the mark's menu on the narrow tier.
  The web never has the camera, so its widest row is still five; a phone at
  274 pt takes the sixth at 7 pt gaps. The mode disc spent 2026-10-18 in the
  top right corner to make room, and was not found there.

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
