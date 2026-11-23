# The Rust UI landscape, read on 2026-11-23

Facts for the component-library study in `README.md`: what each candidate
is, as of this date, with the source. Versions and dates come from the
crates.io API unless another source is named. Nothing here was run; it was
read.

**The one finding that frames everything:** no Rust toolkit today lets one
component render both to the browser DOM and to a GPU canvas. Xilem is the
only stack with both backends (`xilem` → Masonry/Vello, `xilem_web` → DOM),
and it shares the reactive `View` machinery, not the widgets — a component is
written twice ([next-100-guis](https://linebender.org/blog/next-100-guis/),
2026-11-21). Every "web + native" story below is either one renderer on a
canvas (losing the browser's text, IME, soft keyboard and screen readers) or
two element vocabularies behind one reactive layer.

## Dioxus, Blitz, bedina

- Dioxus 0.7.10 (2026-07-30) stable; 0.8.0-alpha.3 (2026-10-16). MIT/Apache.
  Roughly one minor a year, each breaking (`dioxus_elements` → `html::*`,
  const builder API, hashed templates in 0.8). `rsx!` is brace syntax, not
  angle brackets; it compiles to a `'static Template` (static roots plus
  paths to dynamic holes) that a renderer interprets — the one macro that is
  renderer-agnostic by construction. Reactivity is **component-level**: a
  component re-runs when a signal it read changes, then the template's
  dynamic parts are diffed; `#[derive(Store)]` gives field-level
  subscriptions. [releases](https://github.com/dioxuslabs/dioxus/releases),
  [reactivity](https://dioxuslabs.com/learn/0.7/essentials/basics/reactivity).
- A renderer implements `WriteMutations`, a 15-method stack machine
  (`load_template`, `set_attribute`, `set_node_text`, `insert_nodes_after`,
  …) and feeds events back in HTML shape
  ([docs](https://docs.rs/dioxus-core/latest/dioxus_core/trait.WriteMutations.html)).
  The trait changes at each minor. JMS55's `bevy_dioxus` (mutations →
  bevy_ui entities) stopped 2024-04-05.
- **Blitz** 0.3.0-beta.4 (2026-10-16): Stylo 0.24 (Servo's CSS engine,
  **MPL-2.0**, one version per binary, ~40 s of its own build), Taffy, Parley,
  AccessKit, `anyrender` over Vello compute / `vello_gpu` / `vello_cpu` /
  Skia. README: "beta … many bugs and missing features". CSS status page
  lists media queries, container queries and custom properties as
  unsupported while the README lists them as supported — untested
  contradiction ([status](https://blitz.is/status/css)). IME composition
  reached Dioxus only in Nov 2026; keyboard scrolling missing; one user
  measured 18–24 ms frames on scroll; ~12 MB stripped binary (2024 slide).
  wasm builds switched to WebGL2 on 2026-11-09 (PR #1471).
- **bedina** (DioxusLabs, Nico Burns): the first-party "Blitz inside Bevy"
  example, upgraded 2026-10-15 to Bevy 0.20, Dioxus 0.8.0-alpha.2, Blitz
  0.3.0-beta.3, wgpu 30. Blitz paints a `vello::Scene` into a wgpu texture
  on Bevy's device, shown on a `Mesh2d` quad; Bevy mouse and key events are
  converted and handed to the document. 19 files, "examples rather than a
  library"; scale factor 1.0 hard-coded, light scheme, no touch, no HTTP
  assets ([repo](https://github.com/DioxusLabs/bedina)).
- Costs reported by users: a 22-minute wasm build of one dashboard crate,
  210 KB gzip for a dashboard (Leptos 86 KB)
  ([2026-10-26](https://dev.to/hauju/one-year-of-dioxus-fullstack-in-production-the-good-the-painful-2oh8),
  [2026-08-29](https://www.pistack.xyz/posts/2026-08-29-dioxus-vs-yew-vs-leptos-rust-wasm-frameworks-comparison/)).
- Freya dropped Dioxus in 0.4 (2026-07-16): own hooks, Skia, desktop only.

## Leptos, Yew, Sycamore, reactive cores

- Leptos 0.8.23 / 0.9.0-rc1 (2026-10-31), MIT; one maintainer writes ~10×
  the commits of the next. `view!` (rstml) expands to builder calls
  (`div().child(…).on(ev::click, …)`); the static tree's types are why
  compile times grow (`erase_components` halves debug builds, +7 % wasm).
  Fine-grained: `reactive_graph` 0.2.16, Send+Sync arena signals,
  `LocalStorage` for `!Send`, async `Effect` through a global `any_spawner`
  executor, `ImmediateEffect` for synchronous work, `poll_local()` to flush
  from a loop ([issue #2822](https://github.com/leptos-rs/leptos/issues/2822)).
  SSR with streaming and islands.
- **tachys's renderer generic was removed before 0.7** ("catastrophic
  compile times"); `pub type Rndr = dom::Dom;` is fixed, `web-sys` is an
  unconditional dependency. Retargeting means a fork with a `cfg` swap —
  which DioxusLabs did on 2026-10-27 to put Leptos on Blitz, a DOM-shaped
  target ([PR](https://github.com/DioxusLabs/leptos/pull/1)); no
  layout-owning renderer has ever sat behind the trait.
- Yew 0.23.0 (2026-03-10) after a 26-month gap; VDOM over `web_sys`, no
  renderer trait, maintenance commits only. Not a fit for a native target.
- Sycamore 0.9.4 / 0.10.0-beta.1 (2026-11-09), one maintainer; synchronous
  thread-local fine-grained core; the `ViewNode` seam picks DOM or SSR by
  cfg, no third backend exists.
- Standalone cores: `reactive_graph` (active, Send+Sync, global executor),
  `sycamore-reactive` and `floem_reactive` (synchronous, batch per frame,
  thread-local; floem's unreleased since 2024-11), `dioxus-signals` (needs
  dioxus-core's runtime), `futures-signals` (lossy, async). Under wasm
  `+atomics` every DOM handle is `!Send`
  ([wasm-bindgen #955](https://github.com/rustwasm/wasm-bindgen/pull/955)).
- For a UI fed by ECS data that changes every frame, fine-grained signals
  fit best: write ECS → signals once a frame, flush once, only bound leaves
  update. VDOM and Xilem-style rebuild pay per re-rendered subtree; egui
  pays for the whole UI every frame.

## Linebender: Xilem, Masonry, Vello, Parley

- `xilem`/`masonry*` 0.4.0 on **2025-10-29** and nothing since (13 months),
  Apache-2.0 only, "alpha-quality"; `main` has a new two-pass layout
  protocol (PR #1560, 2026-01-16, bespoke, not Taffy), the `imaging`
  renderer abstraction pinned to a git rev, MSRV 1.96, and **`masonry_winit`
  does not build for wasm32** (issue #1848, open since 2026-09-06). Android
  demo-grade. Widgets: label, prose, text_input, button, checkbox, slider,
  progress, flex/grid/zstack/portal/virtual_scroll (+ switch, radio,
  collapse, tooltip on main); no menus, combobox, table, dialog.
- `masonry_core` has **no winit and no wgpu dependency**: `RenderRoot` takes
  pointer/text/accesskit events and returns layers to paint plus an
  AccessKit `TreeUpdate` and IME signals — the right shape for a host, but
  the two attempts to host it elsewhere (PR #879, `space_survival`, 2025)
  were closed unmerged and `texture_render` is undocumented.
- **Vello is three renderers** ([README](https://github.com/linebender/vello)):
  `vello_cpu` 0.4.0 (2026-11-11, "most mature", used by egui ≥0.34 and
  Servo), `vello_gpu` 0.4.0 (renamed from `vello_hybrid` 2026-10-02; CPU
  strips, GPU vertex/fragment only, **no compute, native WebGL2**; "roughly
  beta"; Graphite switched to it 2026-10-25), and the compute `vello` 0.12
  now under `research/`. Glyphs through `glifo` with hinting. `bevy_vello`
  0.14 (2026-08-21) targets Bevy 0.19 and the compute renderer; no 0.20 PR.
- Parley 0.13.0 (2026-11-12): HarfRust shaping, ICU4X, `PlainEditor` with
  IME compose hooks, no rich editor, no undo. Adopters: Masonry, Slint,
  Floem, **Bevy 0.19+**, Blitz, GPUI-CE. AccessKit 0.25.2: Windows, macOS,
  AT-SPI, Android, iOS — **no web adapter**.
- Text quality: egui 0.34 (2026-03-26) replaced ab_glyph with skrifa +
  vello_cpu and turned on hinting; 0.35 added subpixel binning and a
  hinting target; 0.37 colour emoji and system glyph fallback
  ([changelog](https://raw.githubusercontent.com/emilk/egui/main/CHANGELOG.md)).
  No stack in Rust does LCD subpixel AA; the browser remains the reference
  on low-DPI phones. No measured phone comparison exists.

## iced, GPUI, Slint, Floem, others

- iced 0.14.0 (2025-12-07), MIT, one reviewer by policy. Has the one
  maintained recipe for drawing into an existing wgpu device and texture
  (`examples/integration`), own layout engine, cosmic-text. No
  accessibility (#552 since 2020, community PR closed 2026-03-14), no
  Android (#302 since 2020), web = wgpu on a canvas with open IME bugs.
- GPUI 0.2.2 on crates.io (2025-10-22); Zed closed outside PRs 2025-12-12,
  GPUI-CE forked. Owns the window and swapchain on every platform — cannot
  draw into a Bevy texture; web platform (2026-02-26) shipped without text,
  IME, clipboard, a11y; no Android.
- Slint 1.19 (2026-11-16): own DSL, GPL-3 or royalty-free (badge required)
  or paid; web is a canvas "not recommended for general-purpose web apps".
  Floem: unreleased since 2024-11, no AccessKit. Makepad 1.0 (2025-05):
  shader DSL. Ribir, Cushy: stalled.
- Taffy 0.15.1 (2026-10-25), MIT, is the layout engine that crossed
  framework lines: Bevy UI, Blitz, GPUI, Floem, Servo (grid).

## Bevy 0.20's own UI

- The client enables none of `bevy_ui`, `bevy_text`, `bevy_ui_render`,
  `bevy_picking`, `bevy_input_focus`; the lockfile already pins taffy 0.14,
  parley 0.11.1, swash, skrifa, accesskit 0.24.
- `bevy_ui`: `Node` + `ComputedNode`, Taffy flex/grid/block, `Val` with
  px/%/vw/vh/em/rem, `UiScale`, `BorderRadius`, `BoxShadow`, gradients,
  per-side borders, `Outline`, `ImageNode`, `TextShadow`, `Overflow` with
  `ScrollPosition`, `GlobalZIndex`, `UiTargetCamera` (UI to a texture),
  SDF anti-aliasing independent of MSAA, `UiMaterial` for custom shaders.
  **Touch drag-to-scroll and inertia are not built in** (issue #18923, open).
- `bevy_text`: parley since 0.19 (PR #22879, 2026-02-11), hinting on;
  **subpixel positioning was lost in the migration and restored only on
  `main` 2026-11-17 for 0.21** (PR #26010) — 0.20 text is blurrier than
  0.18 or 0.21. Font fallback via fontique; system fonts behind
  `system_font_discovery` with **no web backend** (ship fonts). Atlases are
  never freed in 0.20.
- `EditableText` (0.19, parley `PlainEditor`): typing, selection, clipboard,
  IME preedit/commit, multiline; placeholder, undo and masking
  unimplemented; `bevy_ui_widgets::TextInput` is the widget. IME on Android
  through winit's `set_ime_allowed` (soft keyboard); **web unsupported** —
  a canvas field on mobile web gets no keyboard without a DOM `<input>`.
- `bevy_ui_widgets` (default since 0.19): headless `Button`, `Checkbox`,
  `RadioGroup`, `Slider`, `Scrollbar`, `ScrollArea`, `TextInput`,
  `MenuButton`/`MenuPopup`, `Popover`, `Tab`/`TabList`, `Dialog`/
  `ModalDialog`, `ListBox`; state `Hovered`/`Pressed`/`Checked`; events
  `Activate`, `ValueChange<T>`. `bevy_feathers`: "deliberately not intended
  for your game's UI" — fork it; `UiTheme`/`ThemeToken`/`SurfaceLevel`.
- `bevy_picking` observers (`On<PointerClick>`, drag, scroll) with bubbling,
  `PointerId::Touch`; `InputFocus`, tab and directional navigation;
  `bevy_a11y` → AccessKit on desktop, Android only with GameActivity
  (this APK uses NativeActivity).
- **BSN** shipped in 0.19 (PR #23413): `bsn!` templates, patches, props,
  inline observers; `.bsn` files and hot reload in the 0.21 cycle;
  **reactivity explicitly not upstream and not scheduled** (sixth-birthday
  post, 2026-08-10). Layout is whole-tree per frame in 0.20; **incremental
  layout merged 2026-10-14 for 0.21** (PR #25653, ~75 % faster static).
- Third-party reactive layers: of ten checked, six are dead or frozen
  (sickle_ui, cobweb_ui archived 2026-01-14, bevy_dioxus, quill, ecss,
  simple_text_input). Alive: **bevy_flair 0.9.0** (2026-10-14, Bevy 0.20;
  CSS selectors, `var()`, `@media`, transitions, keyframes onto `Node`s),
  **haalka 0.7.1** (Bevy 0.18; FRP signals via jonmo), bevy_lunex (own
  layout), bevy_immediate. The pattern every one of them ends in: signal or
  diff → `Commands` on `Node` entities.
