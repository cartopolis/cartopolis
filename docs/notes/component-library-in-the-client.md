---
topic: component-library-in-the-client
triggers: [mithril_bevy, ellipsis.rs, images.rs, app_tokens, lib_chats.rs, systems::lib_chats, lib_search.rs, systems::lib_search, SearchView, bridge_input, overlay_owns_keyboard, crates/mithril_bevy, lib_settings.rs, systems::lib_settings, ui_probe.rs, CARTO_UI_LIB, CARTO_UI_PROBE, LibScreens, SettingsState::lib, "ui-probe", UiPointer, overlay_owns_pointer, FontMap, drive_ui, DriveUi, with_host, "replace_children", UiTransform]
updated: 2026-11-24
---

# The component library draws beside egui: how a screen moves over

`docs/mockups/component-library` is the study; this is what holds in the code
while egui and the library (`mithril`, the Rust edition in `jeroen/mithril`,
rendered by `crates/mithril_bevy`) share the client. Both are behind the
`ui-probe` feature until a screen ships on the library.

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

## A screen on the library, beside egui

`CARTO_UI_LIB=settings` puts Settings on the library (`systems::lib_settings`,
2026-11-24). The shape every further screen follows:

- **egui keeps the slot, the library draws in it.** egui's panel system
  returns early (`SettingsState::lib`) but still shows an *empty, unframed*
  `responsive_right_panel`, so the dock, the pins and `map_rect` move aside as
  for any panel and a click in that strip is egui's — not the world's. The
  library's panel is absolute over the same strip. Without this the dock sat
  over the panel's header on the desktop.
- **The library's system owns the panel's life.** `PostUpdate` before
  `DriveUi`: `RightPanel::Settings` appears → mount; disappears → `host.clear()`;
  its ✕ sets a `Cell` the system reads and clears `RightPanel`.
- **State is bridged by a frame-by-frame diff**, not by callbacks: a signal
  whose value moved since the last frame writes the preference; a preference
  that moved writes the signal; `AppPrefs` is assigned only when something
  differs, because its `is_changed()` gates re-run work (the same rule egui's
  panel keeps with its `edited` copy).
- **A world click under the library's panel yields** through
  `EguiManager::set_overlay_owns_pointer(UiPointer::owns_pointer())`, set
  for every library screen at once by `lib_host::bridge_input`; and each
  screen mounts under its own key (`lib_host::mount_keyed`), so a card over
  the map and a panel stand side by side and clear independently.

Captures: `--shots docs/phone-ui-shots.toml --only settings` with
`CARTO_UI_LIB=settings` (`CARTO_UI_LIB_PAGE=general|graphics|world` for a
page), and `CARTO_UI_LIB_HTML=<path>` writes the same panel as HTML for the
browser beside it (the study's `fase3b-settings.png`).

## The second screen: Layers

`systems::lib_layers` follows the Settings shape exactly — `DataLayers::lib`
makes egui's `render_data_layers_panel` keep an empty strip, the library's
system owns `RightPanel::Layers`, every switch is a signal diffed against
the resource each frame. Two things differ. The resource is written
through `bypass_change_detection` and `set_changed` only when a field
moved, because `DataLayers::is_changed` restarts streaming work. And the
panel is drawn as the mockup had it, not as egui built it: a `TileGrid`
of `Tile`s, `IconTabs` for the categories, `Chip`s for the kinds, choice
tiles for the basemap — components added to the library for it. A
category can be asked for by name (`CARTO_UI_LIB_PAGE=city`, `&page=city`).

## The third screen: Search

`systems::lib_search` (`CARTO_UI_LIB=search`, `&panel=search`) is the
first whose field drives the world. Two rules came out of it:

- **One set of methods, two editions.** What the field and the rows do is
  `GeoNavState`'s (`query_edited`, `submit_query`, `search_list`, `pick`,
  `go_home`), called by egui's `search_ui` and the library's screen alike,
  so the two cannot drift. Move a panel's logic onto its state before
  drawing it a second time.
- **The camera side stays with egui's system.** The view actions (Set
  Home, Copy link, Share) need the camera, the store and the share sheet,
  which egui's `render_search_panel` already holds; with `nav.lib` on it
  draws only its empty strip, publishes `GeoNavState::lib_view` (where the
  camera is, what is possible) and runs the action the library hands back.

A library text field with the keyboard holds the game keys off
(`lib_host::bridge_input` → `EguiManager::set_overlay_owns_keyboard`,
read by `ui_owns_keyboard`); before it, typing a Settings password walked
the avatar. A field takes the keyboard on mount with `autofocus` (the
renderer action `@focus`), only when asked (Ctrl+L, `--ui search`), so a
tap on the phone does not throw the keyboard over the recents.

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
`data-theme` from the app's setting, not the page's. Switched on by the
page's query: `?ui=lib`, `&panel=settings` opens it, `&page=graphics`
on a page. The Bevy-only parts of the system (camera, fonts, UI scale,
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
takes, where the resources, the connection and egui's clipboard are; a
value shown (the meter, the token list, an error) is a signal the system
mirrors each frame when it differs.

Not yet: the camera test (a live picture; the library has no image node),
the General page's receiver, storage and editor-access sections, and
cartopolis' own `Tokens` (phase 4).
- **An `<img>`** whose `src` is a `data:` URL (`data:image/jpeg;base64,…`)
  is decoded once and shown as an `ImageNode` (`images.rs`); the browser
  shows the same tag as it is. A screen hands over thumbnails this way,
  scaled down first, since the URL sits in the view and in its HTML dump.
- **`text-overflow: ellipsis`**, which Bevy's text lacks
  (`ellipsis.rs`): the line stays one line, asks for its whole text's
  width (`Node::width`, measured while the whole text shows), may shrink
  to `min-width: 0`, and is cut with "…" after `UiSystems::Layout`. Sized
  by the cut text instead, every box around it that sizes to its content
  shrank with the cut and kept it cut; a first measurement before the
  faces loaded was too narrow, so the fit starts over once, ~45 frames
  in. A line that still reads cut where egui's did not usually has
  less room: measure it (`MITHRIL_BEVY_DUMP=1`) before blaming the cut.
- **`@scroll-end`** (a renderer action) scrolls a scroll area to its end;
  `settle_scroll` brings the asked-for `f32::MAX` back to the laid-out
  end after the layout, or a drag would start from infinity.

## Type scale: the app's, not the site's

The library's screens in the app draw with `theme::app_tokens()` — the
site's `tokens()` on egui's sizes (`SIZE_BODY` 14, `SIZE_SUB` 12.5,
`SIZE_TITLE` 17); screen headers are 19 px (`SIZE_HEADING`), card and
sub-page titles 17. On the site's 15/13 every screen read a size larger
than its egui twin and titles wrapped (2026-11-25). The site's
`theme.css` stays on `tokens()`.
