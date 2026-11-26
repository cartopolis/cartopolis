---
topic: component-library-in-the-client
triggers: [mithril_bevy, lib_keyboard.rs, systems::lib_keyboard, KeyEdits, data-keep-focus, KeyboardInset, LibCamera, follow_world_target, ellipsis.rs, images.rs, app_tokens, lib_chats.rs, systems::lib_chats, lib_search.rs, systems::lib_search, SearchView, bridge_input, overlay_owns_keyboard, crates/mithril_bevy, lib_settings.rs, systems::lib_settings, ui_probe.rs, CARTO_UI_LIB, CARTO_UI_PROBE, LibScreens, SettingsState::lib, "ui-probe", UiPointer, overlay_owns_pointer, FontMap, drive_ui, DriveUi, with_host, "replace_children", UiTransform]
updated: 2026-11-26
---

# The component library is the client's interface

`docs/mockups/component-library` is the study; this is what holds in the code.
The library is `mithril` (the Rust edition in `jeroen/mithril`), drawn by
`crates/mithril_bevy` on `bevy_ui` natively (feature `ui-probe`, a default
and named in the APK's) and as DOM on the web (`ui-dom`). Every screen is a
`systems::lib_*` module; `LibScreens` turns them all on whenever the build has
a backend. **egui left the client on 2026-11-26** (screens moved over from
2026-11-24, the bridge deleted last); the sections below keep, dated, what the
move taught. Input and size questions are `ui-input.md`'s.

Two traps every port met: a node is not laid out in the frame that makes it,
so content rebuilt while it ticks is blank on every frame it is rebuilt — a
ticking value goes into a signal that rewrites its existing text, and only a
change of the rows themselves rebuilds them (`lib_dev_panel`'s readouts,
`lib_call`'s timer: a capture of either was blank or empty while they rebuilt
whole, and a throttle like `READOUT_PERIOD_S` only made it rarer); and the host's dark switch is one
for every screen, so a screen dark in both themes draws in `theme::DARK`'s
colours rather than mounting dark (`lib_camera`).

## One frame of the library

`mithril_bevy::drive_ui` runs in `PostUpdate` (set `DriveUi`): it hands the
frame's pointer and keyboard events to the tree's handlers, drains the
operations the tree queued (a signal write anywhere → an effect → operations),
applies them to `bevy_ui` entities, restyles what changed through the
library's cascade (`mithril::style::Stylist`, the same `css!` rules `gen-css`
writes for the browser) and syncs text inputs. The host (renderer, mounted
trees, handlers) is **thread-local** — signals and handlers are `!Send`, and
Bevy 0.20 has no non-send `World` resources — so a system that mounts takes
`NonSendMarker` and reaches it with `mithril_bevy::with_host`.

Tokens: the plugin takes `theme::tokens()`, cartopolis' own `Tokens`
(`theme/tokens.rs`) — the one table `web_theme.rs` writes the web's
`theme.css` from, so the app's library screens and the site share a
palette by construction. Change a colour there and both move; the web
theme test fails until `theme.css` is regenerated.

Fonts: the cascade's families map onto `FontMap` (`regular`, `semibold` from
weight ≥ 550, `mono`, and `icons` for a family naming "Symbols"), so an icon is
a glyph of `icons::FONT` in a span with `font-family: 'Material Symbols
Rounded'`, on the web the same with an `@font-face`.

## Two silent ways to a 0×0 tree (Bevy 0.20)

Both left every node under the root at size zero at (0, 0), with every style
computed correctly, and nothing in any log. `MITHRIL_BEVY_DUMP=1` logs the tree
with each node's size; that is how they were found.

- **Never `remove::<UiTransform>` from a node.** `ui_layout_system`'s node
  query requires it; a node without it ends the layout walk, and its whole
  subtree stays unlaid out. Write `UiTransform::IDENTITY` instead.
- **Never `remove::<Children>()` + `add_children()` in one frame.** The layout
  syncs changed child lists and then clears every parent whose `Children` was
  *removed* that frame (`RemovedComponents<Children>`), so the parent ends the
  frame childless in the layout tree, and nothing re-syncs it until its
  children change again. `replace_children(&all)` edits in place.

## The shape of a screen

Settings was the first (`systems::lib_settings`, 2026-11-24); every screen
since has the same shape:

- **The screen's system owns its life.** `PostUpdate` before `DriveUi`:
  `RightPanel::Settings` appears → mount; disappears → clear; its ✕ sets a
  `Cell` the system reads and clears `RightPanel`. Each screen mounts under
  its own key (`lib_host::mount_keyed`), so a card over the map and a panel
  stand side by side and clear independently.
- **State is bridged by a frame-by-frame diff**, not by callbacks: a signal
  whose value moved since the last frame writes the preference; a preference
  that moved writes the signal; `AppPrefs` is assigned only when something
  differs, because its `is_changed()` gates re-run work. `DataLayers`
  (`lib_layers`) goes further: written through `bypass_change_detection` and
  `set_changed` only when a field moved, because its `is_changed` restarts
  streaming work.
- **The logic lives on the state, not in the view.** What Search's field and
  rows do is `GeoNavState`'s (`query_edited`, `submit_query`, `search_list`,
  …); a screen whose actions need the camera or the store publishes a view
  (`lib_view`) and runs the action the library hands back.
- **A panel that takes room from the map marks its root `data-reserves-map`**
  (`ui-input.md`), and the pointer and keyboard verdicts reach the world
  through `lib_host::bridge_input` → `UiInput`.

A field takes the keyboard on mount with `autofocus` (the renderer action
`@focus`), only when asked (Ctrl+L, `--ui search`), so a tap on the phone does
not throw the keyboard over the recents. Captures: `--shots
docs/phone-ui-shots.toml --only settings` (`CARTO_UI_LIB_PAGE=general|graphics|world`
for a page, `&page=` on the web), and `CARTO_UI_LIB_HTML=<path>` writes the
panel as HTML for the browser beside it.

## The same screen as DOM, in the web build

On the web the client is only the world inside the site's frame
(`web-client-is-the-world.md`), and its chrome should look like the site.
`systems::lib_host` is the one seam: a screen's system calls `mount` and
`clear`, and the lane decides — `ui-probe` draws through `mithril_bevy`,
`ui-dom` (wasm) through `mithril::dom` into a full-window `<div
id="cartopolis-ui">` over the canvas, `pointer-events: none` except where a
screen is, with the library's stylesheets written into a `<style>` from
`theme::tokens()` the first time something mounts (no file is copied; the
page's own baseline `base.css` is left out). The container carries
`data-theme` from the app's setting, not the page's. The page's query
can open a screen: `&panel=settings`, `&page=graphics` on a page. The Bevy-only parts of the system (camera, fonts, UI scale,
the pointer's ownership) are a `BevyBackend` parameter behind `ui-probe`;
as DOM the panel sits over the canvas and a click never reaches the world.

## What the renderer draws that the browser does natively

The sheet reaches these through pseudo-elements or the platform, which the
cascade does not model; `tree.rs`'s `decorations` draws them from the
computed colours, so a change of token reaches both:

- **A range input** is `bevy_ui_widgets::Slider` (headless, `TrackClick::Snap`)
  over three decorations: the track, the filled part, and a thumb marked
  `SliderThumb` at `left: pct%` pulled back by `pct` of its own width, which
  is the travel the widget measures a drag against. Its `ValueChange<f32>`
  is the element's `input` event; the model's value comes back through
  `SliderValue` in `sync_inputs`.
- **A select** hides its `<option>`s and shows the chosen label as a text
  decoration; a click raises the renderer's own `@select`, and
  `popup::open_select` mounts a backdrop and a menu (`.m-select__menu`,
  rules in the library's `fields.rs`) under the control through the host,
  so they are styled and clicked like any node. A choice raises the
  select's `change` with `selected_index` through `UiHost::internal`,
  delivered in the same frame.
- **A placeholder** is a text decoration in `--m-text-faint`, shown while
  the editor's text is empty — the typed text, not the model, so it goes
  at the first key.
- **A finger's drag** scrolls a `ScrollArea` (the widgets' handles only the
  wheel): a `PointerDrag` observer moves `ScrollPosition` by the delta.

Every Settings page is on the library since 2026-11-24. An action the page
asks for — the chime, the microphone, a deletion, an unblock, a token made
or revoked, a copy — is a `Cell` in `Actions` the view sets and the system
takes, where the resources, the connection and the clipboard
(`utils::clipboard`) are; a value shown (the meter, the token list, an
error) is a signal the system mirrors each frame when it differs.

- **An `<img>`** whose `src` is a `data:` URL (`data:image/jpeg;base64,…`)
  is decoded into an asset shared by every node with that source and shown
  as an `ImageNode` (`images.rs`); the browser shows the same tag as it
  is. **The cache frees what nothing shows**: an image no mounted `<img>`
  has named for `IDLE_FRAMES` (120) frames is evicted, and while the
  decoded pixels pass `CAP_BYTES` (48 MB) unshown ones go earlier, least
  recently used first; a shown one is never evicted. The asset leaves
  `Assets<Image>` one sweep after its entry, never in the frame that may
  have drawn it (`texture-lifetime.md`); shown again, it decodes
  again. So screens may show pictures — chats, avatars, community
  pictures, post photos — as `data:` thumbnails scaled to the size they
  draw, since the URL sits in the view and in its HTML dump. `bevy:NAME`
  live images (`LiveImages`) are the app's and not cached.
- **`text-overflow: ellipsis`**, which Bevy's text lacks
  (`ellipsis.rs`): the line stays one line, asks for its whole text's
  width (`Node::width`, measured while the whole text shows), may shrink
  to `min-width: 0`, and is cut with "…" after `UiSystems::Layout`. Sized
  by the cut text instead, every box around it that sizes to its content
  shrank with the cut and kept it cut; a first measurement before the
  faces loaded was too narrow, so the fit starts over once, ~45 frames
  in. A line that reads cut where it should fit usually has
  less room: measure it (`MITHRIL_BEVY_DUMP=1`) before blaming the cut.
- **`@scroll-end`** (a renderer action) scrolls a scroll area to its end;
  `settle_scroll` brings the asked-for `f32::MAX` back to the laid-out
  end after the layout, or a drag would start from infinity.

## Type scale: the app's, not the site's

The library's screens in the app draw with `theme::app_tokens()` — the
site's `tokens()` on the app's sizes (`SIZE_BODY` 14, `SIZE_SUB` 12.5,
`SIZE_TITLE` 17); screen headers are 19 px (`SIZE_HEADING`), card and
sub-page titles 17. On the site's 15/13 every screen read a size larger
than the egui screen it replaced and titles wrapped (2026-11-25). The site's
`theme.css` stays on `tokens()`.

## The library draws on a camera of its own

`lib_host::prepare_bevy` spawns `LibCamera`, a `Camera2d` at order 100 with
no clear and no MSAA, and makes it the `UiCamera`, so the interface draws
after the world's upscale at full resolution (on the world's camera
`bevy_ui`'s pass sits before the upscale; while egui lived, its buttons lay
over the library's cards there, 2026-11-25). `follow_world_target` copies
the world camera's `RenderTarget` onto it, since a capture moves that onto
an image. The web build has no `LibCamera` (the screens are DOM).

The "…" fit waits for a new measure after each cut (`Ellipsis::cut_from`):
a text's `TextLayoutInfo` lags its change by a frame, and cutting by the
stale width again took long lines down to "1…".

## The on-screen keyboard

`systems::lib_keyboard` draws the keys where the platform wants them at all
(Android, a phone browser, `CARTO_OSK=1`; only a `ui-probe` build).
`onscreen_keyboard::drive_onscreen_keyboard` (`Update`) raises them while a
library field has the focus (`UiInput::overlay_owns_keyboard`), follows the
fingers on them from Bevy's `Touches` — a key types as its finger lifts,
backspace on landing and then repeating while held — and publishes
`onscreen_keyboard::KeyboardInset` (how far up from the bottom, in the
library's px); a library screen with a field at its foot lifts by it. The
keys flip to the top when they would cover their field, but that is
decided once, `SETTLE_FRAMES` after the focus arrives: decided every frame,
a composer lifted by the inset is clear, the keys drop back, it is covered,
and they flip again. A click on a key types only with no finger behind it
(the MCP server's `press`).

Into a library field the keys go through `mithril_bevy::KeyEdits`, applied
at the start of `drive_ui` to the focused `EditableText` (`queue_edit`)
with a `keydown` per key, so `onkeydown` (Enter) fires as for a physical
key. **A key must not take the focus it types into**: a press on a control
takes the focus (`events.rs`), and `PointerFocusPlugin`'s `AcquireFocus`
bubbles to the window and clears it. An element with `data-keep-focus`
does neither.
