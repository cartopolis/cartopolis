---
topic: ui-input
triggers: [ui_input.rs, systems::ui_input, UiInput, UiMetrics, map_dragged, track_map_drag, lib_host.rs, systems::lib_host, bridge_input, measure_map_room, UiPointer, place_pins.rs, systems/nav/locate.rs, picking.rs, map_area, map_rect, data-reserves-map, RESERVES_MAP, owns_click, wants_pointer, ui_owns_keyboard, window_to_ui, set_modal, "dead clicks", "ignores clicks", "game keys", "egui-shared-root-ui"]
updated: 2026-11-26
---

# Who owns the input, and how big the screen is: `UiInput` and `UiMetrics`

Every question a system asks about the interface — is this click the world's,
may the game take this key, where is the map, how big is a UI point — goes
through two plain `Send` resources in `systems::ui_input`, read as `Res<…>` on
every target: `UiInput` and `UiMetrics`. Nothing outside the interface reads
`mithril_bevy`'s state or the window's scale for these. Both answer from **the
last frame**: the library's verdicts come from its pointer and its layout, and
a node is not laid out in the frame that makes it.

## The click: `owns_click`

`UiInput::owns_click(touch_click, cursor)` is the gate for a *world click*, and
`picking::prim_picking` and `building_select::pick_building` must share it, or
`picking::PrimClick` goes stale. A library screen under the pointer owns the
click outright (`mithril_bevy::UiPointer`, which `lib_host::bridge_input` copies
in for the next frame). **A finger's click is also the interface's when it
lands outside `map_rect`**: a brisk tap has already lifted by the time its
click reaches picking, so the pointer's hover cannot be trusted and where it
landed decides (`touch-input.md`). `wants_pointer()` is the pointer-over-a-
screen half alone, what the camera's drags ask through
`TouchGestures::ui_blocked`. `map_dragged()` is a press that went down where
`owns_click` was false and has since moved more than 6 window points
(`ui_input::track_map_drag`, `PreUpdate` after the touch recogniser); a sheet
over the map steps aside for it (the scenario lens).

A node despawned under the pointer — a card closed by its ✕ — never sees the
pointer leave; `bridge_input` drops dead entities from `UiPointer::over`, or
that card held every world click after it.

## The keyboard: `ui_owns_keyboard`

The game keys yield only to **real text entry** — a library field with the
focus (`InputFocus` on an `EditableText`) — plus a modal (the sign-in card,
`set_modal`), the prim edit dialog and a held Ctrl or Super (Cmd on macOS, or
Cmd+W walks the avatar). Never to "some widget has the focus": egui's
`egui_wants_keyboard_input()` was that, so a button that kept the focus after a
click swallowed WASD and Space until someone clicked empty space. The modal is
in the list because a card with the user's attention otherwise leaves WASD
walking the avatar behind it.

## The map's free area: `map_rect` and `data-reserves-map`

`UiInput::map_rect` is the screen less every library element marked
`data-reserves-map` (`RESERVES_MAP`, valued `right`, `left`, `top` or
`bottom` — the edge it is cut from, `ui_input::free_map`), measured after
`bevy_ui`'s layout by `lib_host::measure_map_room` (`measure_map_room_dom` on
the web), in window points. **A panel or sheet that takes room from the map
carries the attribute on its root**, or the dock, the search bar, the round
buttons, the pins and the place names stand under it. `place_pins::map_area`
takes that and also subtracts what reserves nothing in the layout: the
on-screen keyboard's slab, the dock's bands, the route sheet and a lens. Every
map layer keeps to `map_area`.

## Size and scale: `UiMetrics`

`ui_scale`, `viewport_pts`, `compact` (under `COMPACT_WIDTH_PTS`) and
`window_to_ui`, from the window or a `--shot`'s `HeadlessSurface`. A UI point
is not a window point once the scale is above 1: a position from
`Window::cursor_position`, `TouchInput` or `Camera::world_to_viewport` goes
through `window_to_ui` before it places anything, or at 1.5× a mark near the
far edge is a third of a screen out and reads as a projection bug
(`touch-input.md`).

## What went with egui (2026-11-26)

Until the library replaced it, every egui panel drew into one root `Ui`, so
draw order decided egui's hit test (the panel-chain tuple in `plugin.rs`),
floating `Area`s painted over panels, `EguiManager` was non-send on wasm
(`EguiRef` / `EguiMut`), and a drag source laid over a row stole its buttons'
presses. None of it applies any more; this note's history as
`egui-shared-root-ui.md` has the detail.
