# The web side today: the fork, atlas web, midas, podium (inventory, 2026-11-23)

Read from the four checkouts for the component-library study in `README.md`.
Line counts are `wc -l`. Nothing was built, so bundle sizes are the notes'.

## 1. `@garage44/mithril` 3.12.2 (`jeroen/mithril`, HEAD 5ef10eb3)

Atlas and midas pin exactly this commit as a tarball. One runtime dependency
(`htm`). ~12,100 lines of library, 55 test files (14,524 lines, ~782 tests).

| Area | Lines |
|---|---|
| `src/components/*.ts` (core 193, elements 611, fields 411, shell 319, index 75) | 1,609 |
| `src/components/css/` (tokens 107, base 52, elements 915, fields 624, shell 524) | 2,232 |
| `src/render/` (render.ts 1,537, hydration_debug 396, vnode 197, hyperscript 153, …) | 2,377 |
| `src/state/` (signal 529, state 1,062, store 553) | 2,144 |
| `src/ssr/` | 1,420 |
| `src/router/` | 1,110 |
| `src/validation/` | 417 |

**Components** (`src/components/index.ts`), all `MithrilComponent` classes
written with `m()`, not TSX:

- *core*: `Model<T> {value}`, `BaseAttrs {id, class, disabled}`,
  `FieldAttrs {model, label, help, error, validation, required, onchange}`,
  `FieldComponent`, `field()` wrapper, `Field` (`layout: stack|inline`),
  `cx`, `uid`, `normalizeOptions`.
- *fields*: `FieldText` (type, placeholder, autocomplete, prefix, suffix,
  lazy), `FieldTextarea` (rows, autosize, maxlength), `FieldNumber` (min,
  max, step, unit, plain), `FieldCheckbox` (indeterminate), `FieldSwitch`,
  `FieldSelect<T>` (native `<select>`, values by option index),
  `FieldRadio<T>` (inline), `FieldSegmented<T>` (size sm|md), `FieldSlider`
  (format), `FieldTags` (suggestions; the array is replaced, not mutated).
- *elements*: `Tone = neutral|accent|success|warning|danger|info`, `Size =
  sm|md|lg`; `Button` (variant primary|secondary|ghost|danger, size, type,
  loading, href, icon, iconOnly, title), `ButtonGroup` (attached), `Badge`
  (tone, dot), `Spinner`, `Progress`, `Alert` (tone, title, ondismiss),
  `CloseButton`, `Card` (title, subtitle, actions, footer, flush), `Table<R>`
  (columns with sortable, rows, rowKey, `sort` model, empty, onrowclick),
  `Tabs<K>` (model, tabs, variant line|pill), `Collapse` (model, title,
  actions), `Dialog` (model, title, footer, size, modal — native `<dialog>`
  with `showModal()`, a bottom sheet below 560 px), `Toaster` + `toast()`,
  `ThemeSwitch` + `applyTheme()`/`storedTheme()`.
- *shell*: `Shell` (nav, navHeader/Footer, navOpen/navWidth models, header,
  footer, context, contextTitle/Footer, contextOpen/contextWidth models),
  `ShellMenuButton`, `NavGroup`, `NavItem` (href, active, icon, badge,
  muted). A size container: ≥1100 px three columns, 720–1099 context over
  main, <720 drawers; `NARROW` duplicated in `shell.css` and `shell.ts`.

**The `model` convention** (`docs/notes/components.md`): every stateful
component takes `model: {value}` (`state.$key` or `signal()`); the view reads
it, input writes it; dialog open, active tab, sort and the toast list are
models too. Rejected: `value`+`oninput` pairs, the `[object,'key']` tuple, a
`validate` callback.

**CSS**: tokens on `:root, [data-theme]`, every colour a `light-dark()` pair;
groups brand (`--m-accent*`, `--m-on-accent`), surfaces (`--m-bg`,
`--m-surface(-raised|-sunken)`, `--m-hover`, `--m-pressed`), text and lines
(`--m-text(-strong|-dim|-faint)`, `--m-border(-strong)`, `--m-control-bg`),
status (info/success/warning/danger + `-soft`), effects (`--m-focus`,
`--m-backdrop`, `--m-shadow-color`), type (`--m-font`, `--m-font-mono`,
sizes 15/13/17, line height 1.5), shape (radius 4/8/12/full), spacing
(`--m-space-1..6` = 4/8/12/16/24/32), `--m-control-height` 38 / sm 30 / lg
46 (44 / 36 under `pointer: coarse`), shadows, `--m-ease`, `--m-duration`
140 ms (0 under reduced motion). 31 BEM blocks with an `m-` prefix; variants
as `data-variant` / `data-size` / `data-tone`.

**Signals and state**: `signal()`, `computed()` (lazy, notifies on the
clean→dirty edge, no equality check), `effect()`, `state()` (deep proxy,
function properties become computeds, `state.$key` is the raw signal,
replace whole objects), `watch()`, `Store` (localStorage / sessionStorage /
cookie / server session). Redraws coalesce per microtask; a component
redrawn >10× in one task is limited to once per frame; a nested component
whose view is one element redraws in place. Rejected: `@preact/signals-core`,
SolidJS-style direct DOM writes with per-vnode tracking.

**SSR**: `@garage44/mithril/server` → `renderToString`, Bun-only server
helpers, state in `#__SSR_STATE__`; positional hydration, mismatches
replaced. Rejected: clear-and-rerender, strict matching, a runtime-neutral
SSR layer. **Router**: `m.route` with `RouteResolver`; **validation**: DOM-free
chainable rules, so a server can run them.

## 2. cartopolis atlas web (`crates/atlas/web`)

| | Lines |
|---|---|
| `src/**/*.tsx` (39 files) | 15,894 |
| `src/**/*.ts` (not tests) | 10,429 |
| `src/**/*.css` (27 files) | 9,271 |
| top-level build / mock / browser tests | 4,104 |

`pages/` 20 folders, 13,713 code + 7,366 CSS (`chat.tsx` 4,785 + `chat.css`
3,983; `building.tsx` 1,295; `posts.tsx` 1,130; `calendar.tsx` 928); `cms/`
893 + 614; `worker/` 1,772; `lib/` 48 files, 7,284; `types.ts` 981. 17 routes
(`main.tsx:104-123`), admin with 13 tabs in the library's `Shell`.

**Library use**: `Button` 280 (ghost 97, primary 52, danger 15), `FieldText`
104, `Card` 59, `Badge` 30, `Alert` 28, `FieldCheckbox` 22, `FieldSegmented`
20, `FieldSelect` 18, `FieldNumber` 12, `FieldSwitch` 6, `Field` 5,
`ButtonGroup`/`Dialog`/`Table` 4 each, `ThemeSwitch`/`FieldTextarea` 3,
`Shell`/`Collapse`/`Spinner`/`FieldRadio` 2, the rest 1. Never used:
`FieldSlider`, `FieldTags`, `Tabs`, `Toaster`. Beside the library: 45
`MithrilComponent` classes and ~37 function components of its own; raw
`<button` 82, `<details` 8, `<table` 4, `<input` 2; charts (`stats/charts.tsx`
320), the calendar, the floor-plan editor.

**CSS beyond the library**: `theme.css` (59 lines, **generated** from the
client's palette) overrides `--m-*` and adds `--c-*` (brand, glass, rim,
highlight, shade, glow, accent-top/bottom, map); `depth.css` (508 lines)
restyles library classes directly (`.m-button`, `.m-input`, `.m-segmented`,
`.m-nav-item`, …) — this is where the segmented option gets its accent plate;
112 raw hex colours in page CSS (mostly `chat.css` 29, `stats.css` 19).

**CMS pages** (`src/cms/`): built three ways (browser `pages-<hash>.js`,
`pages-ssr.js` for Bun, `pages.html`); the simulator renders a visit in the
script host's Bun with a 2 s budget (`pages/render.rs`); the browser mounts
over the HTML from `#cms-data`; views never read `location`, storage, the
clock or the theme; 11 registered components in `registry.json`, checked by
Rust (`pages/jsx.rs` parses the page JSX, `check.rs`); languages
`page_languages` (default nl, en), paths `/{lang}/{slug}`.

**Chat core**: `crates/web_chat` wraps `cartopolis_chat::Engine` (3,797
lines) as wasm-bindgen `ChatCore`, 66 `pub fn`, byte-shaped (`receive`,
`next_frame`, `take_events` JSON, `take_state`); the worker (1,772 lines TS)
calls 48 of them through a hand-typed `ChatCoreApi`; SharedWorker
`'cartopolis-chat'` with a fixed script name, Web Lock, IndexedDB. Calls:
`lib/call.ts` 1,291 + `call-worker-<hash>.js` from the client crate.

**World frame** (`lib/world-frame.ts`, 82 lines): WebGPU adapter check,
`cartopolis.ready` / `cartopolis.session {token}` same-origin, places via
`FRAMED_PLACE_MESSAGE`. **Browser tests**: five CDP scripts against a real
simulator, no npm packages.

**TS that duplicates Rust**: `lib/calendar.ts` 371, `lib/meeting.ts` 771
(`crates/core/src/meeting.rs`), `lib/rooms.ts` 249, `lib/posts.ts` 75,
`lib/design.ts` 513 (`BuildingDesign` byte for byte), `types.ts`
(`AccountRow`, `PlaceholderModel`), `worker/protocol.ts` (`event_json`).

## 3. midas and podium

**midas** (`midas/web`): 13,864 lines TSX (34 files; `sessions.tsx` 3,227),
7,421 TS, 9,602 CSS (`app.css` 9,537). Same mithril pin. Library use: Button
130, Card 61, FieldText 42, Badge 26, FieldSelect 15, Dialog 10, Alert 9,
FieldSegmented/FieldSwitch/Table 5, … ; 42 own classes and ~38 function
components (charts, fold, pips, confirm, icons, nodeline); raw `<button` 75.
No SSR, own History router (`lib/route.ts`), live data over a WebSocket
(`/v1/stream`). **The server is Rust**: `build.rs` embeds `web/dist/` with
`include_bytes!`, fixed file names, font inlined as base64. Its
`web/src/theme.css` is a **copy of cartopolis's generated `theme.css`** at
c719d86b (2026-10-21) and has drifted in four values; cartopolis's
`theme::LIGHT` in turn says it took midas's light tokens role for role
(`palette.rs:11-13`) — a circle.

**podium**: does **not** use the library. Vanilla TypeScript with direct DOM
(`src/app.ts` 1,612; 2,845 lines in `src/`), own tokens (oklch, Inter, radius
14), `Bun.serve` with an HTML import, no SSR, WebRTC to a realtime API.

## 4. The shared-style seam today

Generated: `web_theme.rs` (378 lines, test-only) writes `--m-*` colours as
`light-dark()` pairs (dark half `theme::DARK` with the `WEB` accent, light
half the `L_*` constants), the `--c-*` tokens, `--m-font`, three radii.
**Not** generated: spacing, control heights, the other tokens. Copied by the
build: fonts, icons, the call worker. By hand: the control metrics table in
`theme/controls.rs:1-34` (38/44 px, radius 8, padding 16/12, 1 px border,
3 px ring, small 30/36 → `control_height()` 40/40 under touch), the web→egui
component table (`Button primary → primary_button_sized`, `ghost →
ghost_button`, `.m-input → text_field`, `Field → labelled`, `FieldCheckbox →
checkbox`, `FieldSegmented → segmented`, `NavItem → list_row/menu_entry`),
midas's `theme.css`, `pages.html`'s background hex.

## 5. Constraints a replacement must respect

1. CMS SSR with identical client takeover (Bun process, 2 s, no `location`
   / storage / clock / theme in views; page JSX checked in Rust).
2. SharedWorker chat: fixed `chat-worker.js` name, `Worker` fallback, Web
   Lock, IndexedDB; state written before frames are sent.
3. Hashed names + Caddy `immutable`; `.br`/`.gz` beside every asset; a
   deploy replaces the directory.
4. Mock mode (`bun run mock`, `mock.ts` 1,783 lines typed against
   `types.ts`; `?mock-signin`, `?mock-tab`, `?mock-chat`).
5. The `types.ts` mirror test (`web_mirror.rs`).
6. The frame's postMessage protocol and the WebGPU check.
7. i18n of content pages; the app's own words stay English.
8. Sizes (from notes, unverified): app ~442 KB, pages ~79 KB, chat engine
   2.9 MB wasm (735 KB gzipped at 2.5 MB in 2026-10), framed viewer 100.3 MB
   (15.3 MB brotli).
9. Pathname routing with Caddy falling back to `index.html`; `/`, `/nl/*`,
   `/en/*`, `/p/*`, `/mcp`, `/api/pages*` go to the simulator.
10. `light-dark()` tokens with a `data-theme` pin applied pre-paint.
11. midas embeds its page in the Rust binary with fixed names.
12. podium is a Bun server app outside the library.
