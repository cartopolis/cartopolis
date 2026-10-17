---
topic: egui-shared-root-ui
triggers: [egui_manager.rs, systems::egui_manager, systems/gui/plugin.rs, place_pins.rs, systems/nav/locate.rs, hud.rs, bookmarks.rs, chat_overlay.rs, systems/player/interact.rs, "panel-chain", "panel chain", ui_mut, available_rect_before_wrap, map_area, EguiRef, EguiMut, "Res<EguiManager>", NonSend, dnd_drag_source, rename_field, rename_row, TextEdit, ui_owns_keyboard, text_edit_focused, egui_wants_keyboard_input, egui_wants_pointer_input, owns_click, TexturesDelta, TextureUpdates, run_ui_unpainted, "dead clicks", "needs several clicks", "ignores clicks", egui_pass, upscaling, Core3d]
updated: 2026-10-17
---

# Every panel draws into one root `Ui`, and egui's hit test is decided by draw order

This client does not call `Context::run`. `egui_manager::begin_egui_frame` opens
the pass with `begin_pass` and builds one root `Ui`; every panel system borrows it
through `EguiManager::ui_mut()` and `end_egui_frame` closes it. The traps below
all follow from that, and none of them logs anything: the symptom is a control
that ignores clicks, or a key that does nothing.

The bridge is our own — an `EguiPlugin` over `egui-wgpu`, modelled on
`bevy_egui` — and the root `Ui` exists because egui 0.35 shows panels inside a
`Ui`, not the `Context`. Its draw is the `egui_pass` **system** in Bevy's `Core3d`
render schedule (Bevy 0.19's schedule-based render graph replaced the
`ViewNode`/render-graph API it used to be), ordered **after `upscaling`**, onto
the final single-sampled sRGB output. Drawing before upscaling, on the
ping-ponged HDR main texture, made the UI flicker and read too bright.
`egui_manager.rs`'s module header still says "before `upscaling`"; the
registration (`egui_pass.after(upscaling)`) is what runs.

## Panels must be totally ordered, or the one underneath goes deaf

All root-`Ui` panels share egui's single `Background` layer. Where Bevy is free to
run two of them in either order, on the frames one composes first its widgets sit
*over* the other's and win the hit test. The covered checkbox then reports
`contains_pointer() == false` while its rect and its clip both contain the
pointer and `layer_id_at` still answers `Background` — the contest is *within* a
layer, so no layer-level diagnostic sees it. It reads as "the checkbox needs
several clicks" (each press is a coin flip) and never reproduces in a
single-panel test. `chat` and `appearance` were left unordered when the bottom bar
grew them (fixed 2026-07-31); an overlay that also draws a root-`Ui` panel is not
exempt for being an overlay.

Since 2026-08-06 the order is **one `.chain()` tuple** in `systems/gui/plugin.rs`
between the `panel-chain:begin` / `panel-chain:end` markers — two chained halves,
`ClientPhase::Input` then `ClientPhase::Present`, still one total order — instead
of a dozen `.after()` links over two `add_systems` calls. **Add a panel by putting
it in that tuple.** `egui_manager::every_root_ui_panel_is_in_the_chain` reads the
sources and fails when a function that takes `EguiMut` and calls `ui_mut()` (or
`egui_manager::right_panel(`) is not named in the block. The order costs no
parallelism: every member takes the manager mutably, so Bevy runs them one at a
time anyway. A system that draws only its own floating `Area`s, and does not read
the map rect below, needs no place in it and can take `EguiRef`, because an `Area`'s z-order lives in egui's memory, not
in registration order — and that demotion is what lets it leave the `EguiMut`
clique that serialises Update (`render-performance-survey.md`).

## Floating `Area`s paint over panels, so a world-anchored one culls itself

egui paints every floating `Area` above the panels — even `Order::Background`.
The HUD's compass and locate button avoid it by not drawing while a
`RightPanel` is open (`hud::render_map_controls`). Pins cannot disappear, so
`place_pins::render_place_pins` culls every pin against `place_pins::map_area`:
the root `Ui`'s `available_rect_before_wrap()` — the space the panels *left*, which
is the map — minus the floating `Area`s that reserve nothing in it (the on-screen
keyboard's slab, the dock's top and bottom bands, the route sheet). That is the
map only once every panel has taken its width, so the pins sit at the **tail** of
the chain, with `locate::render_locate_arrow` after them on the same rule; the
interaction prompt and the chat bubbles cull the same way and sit near the end.
Run one earlier and it sees the whole viewport and paints over the Places list.

**Cull, do not merely clip.** A pin clipped out with `set_clip_rect` is still an
interactive `Area`, and still takes the click meant for the panel behind it. The
same holds inside one `Order`: the pins draw last at the dock's `Order::Middle`,
so before the dock bands were subtracted a pin near the bottom edge of a phone
(dock ~240 pt of a 274 pt viewport) swallowed taps on Search and the app menu.

## Inside a row, the widget registered last wins the press

`Ui::dnd_drag_source` lays a `Sense::drag()`-only widget over the whole row
*after* its contents. egui marks a drag-only widget dragged the instant the
pointer goes down — nothing else senses the press, so there is no threshold — and
re-draws the contents into an `Order::Tooltip` layer whose `Response` is empty. In
the Places panel every ✏ and 🗑 started a drag and lost its click, and an inline
rename `TextEdit` (which senses drag to select text) lost the press, so the field
never held the keyboard. `bookmarks` therefore uses no drag source at all: the
drag lives on the row's name, `click_and_drag` (sensing both, egui waits to see
which it is), with `Response::dnd_set_drag_payload` and a hand-painted
`drag_ghost`; `clicking_a_row_acts_on_it_and_starts_no_drag` guards it. A row
being renamed is drawn by `rename_row` as a plain row, and `rename_field` gives
its `TextEdit` an explicit `Id` — the auto-id shifts when the row swaps its label
for the field, and a field whose id changes between frames loses focus on the
frame it gains it. The opposite arrangement works: a `UiBuilder::sense` scope
registers its own widget *before* its contents and updates it in place, so a
button inside it is on top (`geo_nav`'s `the_route_button_is_clickable_inside_the_row`).

A long press inside egui also fires `clicked()`: `touch-input.md`.

## Who owns the pointer, and who owns the keyboard

`egui_wants_pointer_input()` answers correctly only while a pointer button is
down, because egui never learns this root `Ui`'s leftover rect —
`touch-input.md` has the mechanism and why it cannot be fixed from here. A new
world click gates on `EguiManager::owns_click`.

The keyboard gate is `EguiManager::ui_owns_keyboard`, and it tests
`ctx.text_edit_focused()`, **never `egui_wants_keyboard_input()`**. The latter is
`memory.focused().is_some()` — true whenever *any* widget holds focus — so a
button or selectable in the avatar chooser kept focus after a click and silently
swallowed WASD **and** the Space/Shift fly keys until somebody clicked empty
space. Only real text entry yields the keyboard (plus a modal, the prim edit
dialog, or a held Ctrl/Super chord).

## Reaching the manager: `EguiRef` / `EguiMut`, never `Res<EguiManager>`

egui 0.36 holds dropped files as `Arc<dyn DroppedFile>` without `Send + Sync` on
wasm with atomics (a `web_sys::File` is not thread-safe there), so the whole
`egui::Context` — and `EguiManager` with it — cannot be a `Resource` in the web
build. It is a non-send resource there, and the two aliases resolve to
`Res`/`ResMut` or `NonSend`/`NonSendMut` per target so no system carries a `cfg`.
**A new `Res<EguiManager>` compiles on the desktop and breaks only the web
check.** An `unsafe impl Send` was not taken: it would assert something about
Bevy's executor that a later Bevy is free to change.

egui 0.36 also debug-asserts that a `TexturesDelta` is never dropped unapplied,
which assumes one painter consuming each delta once. Here a delta is cloned into
the render world every extract and retired by generation (`APPLIED_GENERATION`),
so the bridge's `TextureUpdates` wrapper clears itself on drop; a test that runs a
`Context` with no painter goes through `UnpaintedPasses::run_ui_unpainted` /
`end_pass_unpainted`, which clear the texture half of the output. When a texture
may actually be *freed* is `egui-texture-lifetime.md`.
