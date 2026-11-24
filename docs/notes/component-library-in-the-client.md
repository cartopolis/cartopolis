---
topic: component-library-in-the-client
triggers: [mithril_bevy, crates/mithril_bevy, lib_settings.rs, systems::lib_settings, ui_probe.rs, CARTO_UI_LIB, CARTO_UI_PROBE, LibScreens, SettingsState::lib, "ui-probe", UiPointer, overlay_owns_pointer, FontMap, drive_ui, DriveUi, with_host, "replace_children", UiTransform]
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
  `EguiManager::set_overlay_owns_pointer(UiPointer::owns_pointer())`, set by
  the screen's system for the next frame.

Captures: `--shots docs/phone-ui-shots.toml --only settings` with
`CARTO_UI_LIB=settings` (`CARTO_UI_LIB_PAGE=general` for a page), and
`CARTO_UI_LIB_HTML=<path>` writes the same panel as HTML for the browser
beside it (the study's `fase3b-settings.png`).

Not yet: the other Settings pages, placeholder text in inputs, the select
popup, the slider, scrolling by touch drag, cartopolis' own `Tokens`.
