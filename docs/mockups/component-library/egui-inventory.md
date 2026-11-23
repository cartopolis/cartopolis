# What the client gets from egui today (inventory, 2026-11-23)

Read from this worktree (`main` at d0103e24) for the component-library study in
`README.md`. Counts are `grep -rE` over `crates/cartopolis/src` and include test
code unless noted. This is the list a replacement has to answer, item by item.

## 1. Size

| What | Count |
|---|---|
| `.rs` files in `systems/gui` (incl. `theme/` and three `tests.rs`) | 70 |
| of which panels, systems, bridge, helpers (outside `theme/`, not tests) | 56 |
| lines in `systems/gui` | 78,012 (75,134 without the `tests.rs` files) |
| lines in `systems/gui/theme/` (12 files) | 5,902 |
| `systems/gui/egui_manager.rs` | 1,708 lines (+ 898 of tests) |
| `.rs` files in `systems/dev` | 44 files, 34,776 lines; 10 name egui |
| screens in `docs/phone-ui-shots.toml` | 41 |
| `egui::` references crate-wide | 4,847 across 118 files |
| systems taking `EguiMut` / `EguiRef` | 68 / 67 |

Largest GUI files: `chats.rs` 5,318 · `settings.rs` 2,959 · `data_layers.rs`
2,885 · `camera_screen.rs` 2,823 · `ui.rs` 2,658 · `calendar.rs` 2,590 ·
`scenario.rs` 2,569 · `call_screen.rs` 2,529 · `community_posts.rs` 2,410 ·
`contacts.rs` 2,395 · `map_osd.rs` 2,335 · `communities.rs` 2,325 ·
`chat_overlay.rs` 2,186 · `bookmarks.rs` 2,179 · `design_room.rs` 2,093.

**A lot of egui lives outside `systems/gui`**: 43 files in `nav/`, `player/`,
`map/`, `net/` name it. The heavy ones: `nav/navigation.rs` (route sheet, 102
mentions, 3,083 lines), `nav/driver.rs` (76), `nav/geo_nav.rs` (search panel,
41), `player/free_camera.rs` (35), `nav/walk_guide.rs` (30), `player/picking.rs`
(25), `nav/locate.rs` (24), `map/satellites.rs` (22), `player/touch.rs` (20).

## 2. The bridge (`systems/gui/egui_manager.rs`)

Our own `EguiPlugin` over `egui-wgpu` 0.36 (`default-features = false`),
modelled on bevy_egui; `begin_pass`/`end_pass` with one shared root `Ui`, never
`Context::run`.

- **Lifecycle** (`:810-866`): `begin_egui_frame` in PreUpdate after
  `InputSystems`; panels in Update through `ui_mut()` (`:687`) or
  `ctx()`/`ctx_mut()` for `Area`s (`:509-531`); `end_egui_frame` in PostUpdate;
  `ExtractResourcePlugin<EguiRenderOutput>` to the render world (`:759-777`);
  `egui_pass` a system in `Core3d`, `.after(upscaling)`.
- **Root `Ui`** (`:1307-1317`): full viewport on `LayerId::background()`,
  rebuilt every frame, shared by every panel. Draw order decides hit-testing
  (hence the one `.chain()`); the leftover `available_rect_before_wrap()` is
  kept as `map_rect` (`:1387-1390`). egui never learns the root's available
  rect (`pass_state_mut` is `pub(crate)`), so `egui_wants_pointer_input()` is
  only right while a button is down (`touch-input.md:69-123`).
- **Pointer and touch** (`pointer_events` `:946-1042`): reads the ordered
  `WindowEvent` stream; `take_pass_events` (`:1104-1112`) cuts a pass's batch
  after its first button event so a click followed by a flick is not lost;
  every finger becomes `Event::Touch`, the first finger owns egui's single
  pointer; `heal_lost_touch` (`:1074-1093`) releases a finger Bevy forgot
  (Android camera intent). Positions are divided by `ui_scale` (`:953`);
  `window_to_ui` (`:622`) is the same conversion for overlays.
- **Keyboard** (`:1182-1262`): keys forwarded only while
  `ctx.text_edit_focused()`, except Escape and Tab; Accel+C/X/V bridged to
  `utils::clipboard`; text from `KeyboardInput.text`. **No IME bridge at
  all**: no `Ime` event is handled anywhere, so no composition or pre-edit.
  Injected events (on-screen keyboard, MCP `press`) are appended next frame
  (`inject_events` `:610`).
- **`RawInput`** (`:1283-1305`): `screen_rect` from the window or
  `HeadlessSurface`; `predicted_dt` measured and clamped (`:1296`);
  `native_pixels_per_point` = display scale × `ui_scale`.
- **Quiet frames** (`:1402-1443`, `egui-quiet-frame-staleness.md`): when
  egui's repaint delay has not run out and nothing changed, tessellate,
  extract and upload are skipped and the pass redraws resident buffers.
  Anything drawn from ECS state freezes unless it asks: `animating()`
  (`:551`), `WorldAnchored` (`:461-503`, repaint only when a projected point
  moved), `force_next_write` (shot harness, screenshot suppression).
- **Texture deltas** (`egui-texture-lifetime.md`): cloned into the render
  world, carried forward until `APPLIED_GENERATION` (`:1601`) says they
  landed (Android has no view for ~5 s); frees deferred past submit
  (`:1575-1577`, `:1666-1668`); `TextureUpdates` clears itself on drop.
- **Render pass** (`:1508-1669`): separate pass with `LoadOp::Load` onto the
  final sRGB output; skips cameras with `order < 0`; renderer re-created on
  format or sample-count change; re-uploads only on `generation` change.
- **wasm non-send** (`:39-56`, `:788-804`): `EguiManager` and the renderer
  are `NonSend` on wasm (egui 0.36's `DroppedFile`, WebGL handles);
  `EguiRef`/`EguiMut` resolve per target.
- **Scale** (`:153-209`): `ui_scale` 1.5 on the Android phone tier, 1.0 on
  the web, `CARTO_UI_SCALE` / `?ui_scale` 0.5–3; `compact()` = width < 640 pt
  (`:125`); `responsive_right_panel`, `right_panel`, `bottom_sheet`
  (`:244-365`) are the shared scaffolding. egui has no media queries: every
  panel branches on `compact()` each frame.

## 3. Capabilities used

| Capability | Where used | What a replacement must provide |
|---|---|---|
| **Text editing** | `TextEdit::` 64 sites / 28 files; `theme::text_field(` 55 / 19; `password_field` 5; `hint_text` 48; `char_limit` 8; `TextEditState`/`CCursor` 8 / 4 (text_touch, onscreen_keyboard, geo_nav, bookmarks); explicit field `Id`s (`bookmarks::rename_field`) | Single- and multi-line editing: caret, selection, drag-select (scroll first, select once focused), undo/redo, select-all, cut/copy/paste through our clipboard, hint text, masking, char limits, stable focus identity, programmatic focus with event filters (10 sites), the galley and settled cursor range in the same pass |
| **IME** | none bridged | Today: plain text through `KeyboardInput.text`, nothing for composition. A replacement should do better (the web backend gets it free) |
| **On-screen keyboard** (`onscreen_keyboard.rs`, 1,289 + 742 test lines) | QWERTY slab in an `Area` on `Order::Tooltip`; Android always, web only on the phone tier (`:90-121`); layers, shift, arrows extending the selection; injects events for the next frame; `restore_focus` after every tap; `field_makes_room` (`:452`) | Key injection into the focused field, focus that survives tapping a non-field widget, a top-most layer, the keyboard's rect published so others make room, 48 dp key floor — or a platform IME path (Android NativeActivity has none) |
| **Touch text handles** (`text_touch.rs`, 199) | Teardrop caret/selection handles from the same pass's galley; `password_field` reveal eye in temp memory | Text layout geometry (glyph positions, row rects) and selection control from outside the widget |
| **Scroll areas** | `ScrollArea::` 53 / 25; `stick_to_bottom` 5; `scroll_to_*` 1; solid bars (`theme/style.rs:166`); kinetic fling on `predicted_dt` | Vertical and horizontal scroll with touch drag and inertia, stick-to-bottom, scroll-to-rect, bars that reserve their own column, no stealing of drag-to-select |
| **Panels** | `egui::Panel::` 12 / 7 (right, bottom, `show_collapsible` slide); `RightPanel` slot enum; 11+ right-panel systems | Docked panels that report the leftover rect (the map), resizable on desktop, full width on compact, slide in/out |
| **Floating layers** | `egui::Area::` 92 / 39; `egui::Window::` 8 / 7; `egui::Modal` 1; `Order::Foreground` 57, `Middle` 18, `Background` 9, `Tooltip` 8; `move_to_top` 13; `interactable(false)` 9 | Layered overlays with explicit order classes, hit-testing by layer (`layer_id_at` in `press_over_ui`, `:1358`), non-interactive readouts, modals with backdrop, a z-order that does not demote transient layers (`touch-input.md:566-627`) |
| **World-anchored overlays** | `Camera::world_to_viewport` 26 / 15 then `window_to_ui` 12 / 7: place pins (culled against `place_pins::map_area`), chat bubbles, place names, plans, scenario areas, locate arrow, person markers, POI popup, gizmo handles, design room overlay, satellites; `WorldAnchored` in 8 files; camera basis from this frame's `Transform` (`screen-overlay-projection.md`); sub-pixel tips; at the tail of the chain | Per-frame placement at projected points with sub-pixel precision; culling (not clipping) against the free map rect; a repaint trigger tied to world motion; ordering after the camera update |
| **Interaction prompt** (`player/interact.rs:992-1012`) | `Area` at bottom-centre of the leftover map rect | Screen-anchored overlay relative to the map rect |
| **Custom painting** | `painter()`/`layer_painter` 216 / 51; `Shape::` 159 / 31; `circle_*` 128 / 34; `egui::Mesh` 9 / 6. Heaviest: `scenario.rs` 64, `map_osd.rs` 54 (thumbstick, rate rails), `call_screen.rs` 44 (gradient mesh `:456`), `chats.rs` 33, `hud.rs` 26 (compass, light disc, scale bar), `cosmos.rs` 22 (Moon phase, Milky Way meshes), `place_pins.rs` 15 (teardrops), `discs.rs`, `faces.rs` (textured fan `:105-112`), `contacts.rs:2130` (QR), `theme/brand.rs` (mark, wordmark from a geometry table), `close_button`/`kebab_button` | An immediate 2D vector API: circles, rounded rects, segments, convex paths, triangle meshes with per-vertex colour and UV into a texture, clip rects (16), layered painters, text at arbitrary positions (~98 / 27), galley measurement (`layout_no_wrap`/`fonts` 56 / 26) |
| **Images / textures** | `TextureHandle` 33 / 10; `load_texture` 13 / 8; `egui::Image` 38 / 28: place photos and thumbnails, community pictures, faces, chat pictures, texture-library thumbnails, call video frames (`TextureHandle::set` per frame), camera viewfinder and result, settings camera preview. No `register_native_texture`, `TextureId::User` or `PaintCallback` | CPU-RGBA upload with in-place update at video rate, linear filtering, deferred free, images in layouts and in meshes (UV). Decoding happens on the UI thread in several places |
| **3D previews** | none as textures (avatar designer is a sheet over the live avatar) | nothing |
| **Fonts / icon font** | `theme/metrics.rs:12-106`: `BRAND_TTF`, `BODY_TTF`, `EMOJI_TTF` (one-colour Noto Emoji), `icons::FONT` (Material Symbols Rounded subset, 16.9 kB, 132 `\u{…}` literals) appended to every family with `FontTweak`; icons are private-use codepoints inside strings; `FontId`/`FontFamily` 190 / 42; `RichText` 545 / 52; `LayoutJob` 15 / 11 | Fallback chains per family, named families, per-font scale and baseline tweak, glyph coverage queries (tofu test), fonts installed before the first frame |
| **Drag & drop** | `bookmarks.rs` only: `click_and_drag` on the row name, `dnd_set_drag_payload`, hand-painted ghost (`egui-shared-root-ui.md:76-93`) | Drag payloads between rows, click and drag discriminated on one widget |
| **Drags / gestures** | `Sense::drag`/`click_and_drag`/`drag_delta` 31 / 13 (sheets, zoom rail, thumbstick, handles, sliders); `Sense::` 197 / 46; `interact(` 51 / 23 | Raw drag deltas and velocity per widget. egui has one pointer; two fingers go round it through `touch::UiClaimRegions` |
| **Long press / menus** | `context_menu(` 6; `long_touched` 4; `secondary_clicked` 3; `menu_button`/`Popup` 8 / 4; `ComboBox` 6 / 4 | Long press = secondary click on touch with the click suppressible; anchored popups; dropdowns |
| **Tooltips** | `on_hover_*` 142 / 42; `theme::action_enabled_because` draws an inline note on touch | Hover tooltips; an inline alternative on touch |
| **Animation** | `motion.rs` (781 lines) is the only caller of `animate_*` (rule `every_animation_goes_through_motion`): `presence`/`linger`/`enter`/`fade`/`pop`/`press`/`toggle`/`value`, `grabber`/`follow` sheets, `page_turn`; five durations, `ease_out`/`spring`, reduced motion; `motion::` 207 uses; `request_repaint` 31 / 14 | Time-based tweens keyed by id that keep the frame alive only while running; enter **and** leave; reduced motion |
| **Dismiss ladder** (`dismiss.rs`, 618) | `enum Rung` of 21 variants in order (Keyboard, TextFocus, CallScreen, Camera, …, RightPanel, PersonCard, PlaceCard, BuildingCard, …); `handle_escape` (`:371`), `handle_back_gesture` (`:583`); TextFocus rung exists because egui's `Memory` eats Escape; pages register via `theme::page_back` | One back stack every surface registers in; "was a field focused at frame start"; Escape that blurs without also closing |
| **Focus / keyboard gating** | `ui_owns_keyboard` (`:704-719`, 18 / 11); `owns_click` (`:645-650`, 10 / 5); `map_dragged`; `set_modal` | "Is a field focused", "did this press land on UI" by position, "did this drag start on the map" |
| **Accessibility** | No platform output. AccessKit only for the MCP `ui_tree` (`dev/mcp/screen.rs:939-952`); `theme::named(…)` 89 sites, `widget_info` ~17, `// unnamed:` 7 | A semantic tree (role, label, enabled, toggled, value) for every interactive widget incl. hand-painted ones, joinable to screen rects |
| **Screenshots / MCP driving** (`dev/mcp/screen.rs`, 1,386) | `ui_tree` from `accesskit_update` in an egui `Plugin` `output_hook` joined with `WidgetRects` by `Id`; `tap_point` via `layer_id_at`; `press` injects pointer sequences via `input_hook`; refs are 6 digits folded from the egui `Id`; `--hide-ui` via `suppress_egui_for_screenshot` | Stable widget ids across frames, a per-frame list of widget rects with semantics, an input-injection hook ahead of hit-testing, "which layer is on top at p" |
| **Script editor** (`script_editor.rs` 488, `js_highlight.rs` 453) | `TextEdit::multiline(...).code_editor().layouter(...)` with a hand-written JS lexer → `LayoutJob` | A multiline editor with per-span colouring, monospace, tab handling, large buffers |
| **Dev panels** | `dev_panel.rs`: one `egui::Window` with tabs, scroll, labels; no plots, no tables (`Grid` 41 / 29 crate-wide) | Window, tabs, grid, labels, scroll |
| **Splash** (`splash.rs`, 878) | Painted mark and wordmark from frame 1, fade with `request_repaint`; matches `index.html`'s loader | Fonts at construction; a full-screen painted layer on the first frame |
| **Call / camera screens** | video tiles as per-frame `TextureHandle::set`, cropped via UV, gradient meshes; viewfinder texture, polaroid frame | Streaming texture updates, UV cropping, full-screen layers |
| **Layer tiles panel** (`layer_tiles.rs`, 485) | painted category discs, whole-row hit areas | Custom rows with whole-row hit areas |
| **Design room / avatar designer** | overlay at projected points; `Slider` ×3, `CollapsingHeader` ×2; bottom sheet | Sliders, collapsibles, bottom sheet |
| **DragValue / Slider** | `DragValue` 21 in `ui.rs` (prim edit dialog) + 1; `Slider` 14 (settings ×9, designer ×3, scenario ×2) | Numeric drag-fields with ranges; sliders |
| **Other widgets** | `checkbox(` 52 / 18, `selectable_*` 16 / 8, `CollapsingHeader` 5, `ComboBox` 6, progress/spinner 38 / 19, `open_url` 43 / 12, `Grid` 41 / 29, `hovered()` 61 / 29, cursor icons 31 / 16, `data_mut`/temp memory 45 / 11 | Checkbox/switch, segmented, collapsible, dropdown, progress, link, grid, hover, cursors, per-id temp state |
| **Theme widget set** (`theme/*.rs`, 2,981 `theme::` uses / 77 files) | `controls.rs` (text_field, labelled, switch, ghost/danger/icon buttons), `frames.rs` (overlay_frame, primary/secondary/round buttons, cards), `header.rs` (panel_header, close/kebab/action buttons, menu entries, chip), `states.rs` (section, list_row, back_title, page_back, segmented, empty_state, busy, inline_error, confirm_button, checkbox, password_field, wrap_before, panel close), `palette.rs`, `style.rs`. 24 raw `egui::Button` sites / 13 files remain | The same components, with per-widget styling (egui shares one `WidgetVisuals` per state across widget types) |
| **web_theme generation** (`web_theme.rs`, test-only) | `tokens()` maps theme constants to `--c-*`/`--m-*` custom properties; `css()` writes `crates/atlas/web/src/css/theme.css`; three tests | Theme tokens as plain data |
| **Responsive layout** | `compact()` 25 / 20; `ui_scale()`/`display_scale()` 21 / 12; `dp_per_point`, `min_touch_target_pts`, `touch_layout` | Viewport-width branching, dp-correct touch sizes |
| **Clipboard** | `utils/clipboard.rs`: arboard; Android copy via JNI, paste `None`; web write-only | Copy/paste routed through the client's module |

## 4. Rules and tests tied to egui

`source_rules.rs` (465 lines): `egui_is_reached_through_egui_ref_and_egui_mut`,
`game_keys_yield_only_to_a_focused_text_field`, `hand_drawn_controls_are_named`
(a clickable rect needs `theme::named` or `widget_info`),
`every_animation_goes_through_motion`, `text_fields_are_the_themes`,
`buttons_are_the_themes`, `checkboxes_are_the_themes` (the last three exempt
`EGUI_OWN_FILES` `:367-378`).

Bridge tests (`egui_manager/tests.rs`, 25): input ordering and touch
synthesis (8), clicks through the root `Ui` (5), repaint (2), robustness (3),
scale (7), injection (1), and **`every_root_ui_panel_is_in_the_chain`**
(`:803`), which reads the sources for `EguiMut` + `ui_mut()` functions and
requires them between the `panel-chain` markers in `gui/plugin.rs:516-649`.
Chain order: main_menu → search → layers → bookmarks → route_sheet [→ chats,
admin] → script → model_library → avatar_chooser → settings → about →
interaction_prompt [→ chat_bubbles, chat_chip, chat_sheet, chat_world] →
dock [→ person_card] → scenario_map → place_names → plans → place_pins →
locate_arrow.

Theme tests (`theme/tests.rs`, 21): the contrast matrix
(`text_clears_every_surface_it_can_land_on`), control boundary 3:1, documented
ratios, palette pins, brand geometry, fonts on the first frame, tofu scan
(`cartopolis/src` and `crates/geo/src`), type/radius scales, touch sizes.
Plus `icons.rs` (2), `web_theme` (3), `onscreen_keyboard/tests.rs` (23, a
headless `egui::Context` harness), `mcp/screen.rs` tests, and
`UnpaintedPasses::run_ui_unpainted` used by 34 widget-level click tests in 16
files.

Phone shots (`docs/phone-ui-shots.toml`, 41 screens at 412×900, 1.5×): need
`HeadlessSurface`, `keep_egui_fresh`, `--hide-ui`, and `--ui` names that open
panels. An equivalent needs headless offscreen rendering, a forced repaint per
frame, and programmatic opening of each screen.

## 5. Platform specifics

- **Android**: no IME (NativeActivity), so the on-screen keyboard is
  unconditional; `phone_ui_scale()` 1.5, touch floor 48 dp, viewport 274 pt;
  Back = `KeyCode::BrowserBack` → `dismiss`; no view for ~5 s at start;
  camera intent drops touch-ups; clipboard copy by JNI, paste `None`.
- **Web**: manager and renderer non-send; DPR folded into the canvas
  (`WEB_PHONE_MAX_DPR` 1.5, viewport ≈214 pt); on-screen keyboard only on the
  phone tier because the browser IME resizes the visual viewport under the
  canvas; `touch-action: none` etc. load-bearing; clipboard write-only; WebGPU
  required. **Since 2026-11-22 the wasm build is only the world**
  (`--no-default-features --features audio`): access_tokens, admin, bridges,
  calendar, call_screen, camera_screen, chat_overlay, chat_people, chats,
  communities, community_posts, contacts, external_signin, login_screen are
  compiled out. What remains: menu/face sheet, search, layers, Places, route
  sheet, script editor, model library, avatar chooser/designer, settings,
  about, interaction prompt, dock, scenario map, place names, plans, pins,
  locate arrow, HUD, OSD, zoom rail, cosmos, design room, place/building
  cards, floor picker, facade check, action ring, welcome, splash, app
  update, faces, discs, on-screen keyboard, text handles, dismiss, motion,
  theme, icons, driver UI, dev tools.
- **Desktop**: mouse path valid while a button is held; resizable panels;
  Super on macOS; arboard; no IME composition; `CARTO_OSK=1` to test the
  phone text stack.

## 6. What is painful today

From `ui-vocabulary.md` "The controls are the web's" (2026-11-22): one
`WidgetVisuals` per state for all widget types; no per-widget styling or
selectors; measure-after-place layout; floating scroll bars; `Button::small()`
collapses to ~20 dp; `horizontal_wrapped` does not wrap frames; no tooltips on
touch; no toast idiom.

From `docs/mockups/fluid-ui/README.md` "waarom egui hapert" (2026-11-10), nine
causes: closing is always a cut (egui fades in, never out); a still screen
halves the frame rate; clock animations freeze on quiet frames; `predicted_dt`
unset (since fixed); decode and storage on the UI thread; nine page-stack
enums; swipe as threshold not motion; a keyboard that covers and moves
nothing; the UI pass as a second full pass with 28 `ResMut<EguiManager>`
systems serialising Update. Steps 1–3 and part of 5 of its six-step plan are
built; 0, 4 and 6 are open.

Performance: the `EguiMut` clique (`render-performance-survey.md:806-812`)
leaves Update at most 2-way parallel; the only measured number is
`egui_pass` ≈ 0.3 ms/frame on mac-2 (s663, 2026-11-10); phone cost and
tessellation cost unmeasured.

Other stated limits: `is_pointer_over_egui()` unfixable from outside;
`move_to_top` a flag in a stable sort; one pointer; `dnd_drag_source` steals
presses; shifting auto-ids drop focus; `SurrenderFocusOn::Clicks` forces
`restore_focus`; `Memory` eats Escape; a long touch also clicks; anchored
size-to-content `Area`s grow; no mip chain (brand mark painted, not a PNG);
`WidgetInfo` labels only in debug builds; `DroppedFile` forces non-send on
wasm; bundled fonts lacked emoji and arrows; `RichText::strong()` coloured by
the pressed-button stroke.
