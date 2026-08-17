---
topic: ui
triggers: [theme.rs, dismiss.rs, "design system", palette, contrast, tofu, glyph, emoji, icon, accessibility, a11y, WCAG, escape, "empty state", spinner, disabled, confirm, "touch target"]
updated: 2026-08-17
---

# The UI vocabulary, and the four things that keep breaking it

`systems::theme` is the design system: one palette, four scales, and a set of
widgets. This note holds the facts that a reader of that file cannot derive from
it — the measurements it was tuned against, the font it is drawn with, and the
three rules whose violations are invisible until someone runs the app.

All measurements dated 2026-08-14, taken from a ten-lens sweep of the client.
Method for each is given inline. Where a number is now a unit test, the test is
named, because the test is the thing that stays true.

## egui's bundled fonts have **no pictographic emoji at all**

Measured by probing `Fonts::has_glyphs` against the stack egui 0.35 installs for
`FontFamily::Proportional`, which is `["Ubuntu-Light", "NotoEmoji-Regular",
"emoji-icon-font"]`. Between them those cover the BMP symbol blocks and
essentially nothing in Miscellaneous Symbols and Pictographs.

Drawable (the useful ones, probed, not assumed):

```
★ ☆ ☉ ☐ ☘ ☠ ☢ ☣ ☤ ☥ ☦ ☩ ☪ ☬ ☭ ☮ ☯ ☰ ☸ ♀ ♂ ♔ ♕ ♖ ♗ ♘ ♙ ♚ ♛ ♜ ♝ ♞ ♟
♡ ♩ ♪ ♫ ♬ ♾ ⚐ ⚑ ⚒ ⚔ ⚕ ⚖ ⚘ ⚙ ⚛ ⚜ ⚰ ⛤ ⛧ ⛨ ⛩ ⛱ ✚ ✝ ✟ ✠ ✡ ✪ ✱ ✿ ❀ ❥ € • ○ ◎ ■ ▣
```

Tofu — and this is the surprising part, because they are the obvious choices:

```
🍴 ☕ 🛍 🛒 🛏 🏨 🎭 🎓 🚉 ⚽ ⛪ 📍 🚗 🚲 🚶 🔍 🗺 💬 📷 🗑 🤖 🧭 → ← ↑ ↓ ✕ ✓ ⋮ ●
```

Two consequences, and the second is the one that actually bites:

- **Prefer `theme::first_drawable` to picking a glyph.** It takes a preference
  ladder and returns the first the font has, so an icon degrades to a worse icon
  and finally to a letter, instead of to a box. Every icon in the *client* goes
  through it.
- **A call site with no egui `Context` cannot run a ladder**, so its glyph has to
  be right when it is written. `PoiCategory::glyph` and `TravelMode::icon` in
  `cartopolis_geo` are the two such tables, and both shipped as tofu — ten of
  twelve POI categories drew an empty box in the interact prompt. The guard,
  `theme::tests::no_painted_glyph_is_tofu`, walked `cartopolis/src` only; the
  hole was the crate boundary, not the rule. It scans `crates/geo/src` now.
  `cartopolis_geo` cannot depend on egui, so it cannot host that test — the test
  has to come to it, and any third crate that paints a glyph needs adding to the
  same walk.

`KNOWN_TOFU` is the excuse list. An entry there is a claim that the codepoint is
retained intent with no call site; it is not a way to ship a box.

## The palette must be measured on every surface, not on `SURFACE`

Contrast was originally checked against the panel fill and the accent fills. That
is the easy half. Two things put a colour somewhere else without anyone choosing
it:

- **egui's own widget table.** `weak_text_color` inside an *open* menu button is
  `TEXT_DIM` on `SURFACE_HOVER` — at rest, not as a transient a mouse passes
  through. That pair measured 4.40:1.
- **`OVERLAY`'s background is not in `theme.rs` at all.** The imagery is a map,
  not a satellite photo — pale beige land on near-white paper, which is the same
  fact `globe::GLOBE_DEEP_ALBEDO` exists to deal with. So the composite an
  overlay box actually lands on is `SURFACE` over *white*, three steps lighter
  than the panel its colours were picked against. At the old 84% alpha that is
  `#404B51`, on which `TEXT_DIM` measured 4.12, `ACCENT_TEXT` 4.11 and `ERROR`
  3.59 — the HUD's coordinates, every value in an overlay and the driver's
  failure text, all under 4.5 over ordinary daytime map.

`theme::tests::text_clears_every_surface_it_can_land_on` pins the whole matrix,
including the translucent composites over a bright and a dark basemap. **Add a
colour, add its row**; a colour tested only against `SURFACE` has been tested
against the one background it will not always have.

Two further rules the same sweep established, both now tests:

- **A control has to be findable before its label matters** (WCAG 1.4.11, 3:1).
  A button at rest is `SURFACE_RAISED` on `SURFACE`, which is **1.24:1** of fill
  difference — so the hairline is the only channel identifying the control, and
  `LINE` was 1.92:1. What had been carrying the UI is that panels are dense
  enough for position to do the work; a control on its own had no boundary.
  `a_control_is_distinguishable_from_the_surface_it_sits_on`.
- **Prose does not recompute.** Three ratios written in `theme.rs` were wrong,
  and a fourth was found by writing the test that checks them: `INK` on `ACCENT`
  was given as 5.9 in one place and 6.0 in another, the alternative it argues
  against ("only 3.1:1") is *white*'s ratio and not `CREAM`'s — cream on the
  full accent is 2.67, under the 3:1 floor the argument rests on — and `TEXT` on
  `SURFACE` had been documented as 11.9 since the file was created when it is
  11.85. `the_documented_ratios_are_the_measured_ones` makes a stale paragraph a
  failing test.

### Rejected: stopping down the camera instead of fixing the colours

Considered and dropped for the overlay contrast problem specifically. It is the
same argument the globe albedo note makes: the near-white map sits on the
tonemapper's shoulder, where exposure barely darkens the surface but does dim
the limb and the stars. Contrast between chrome and its own fill is not a
lighting problem and cannot be fixed by a lighting knob.

## A tooltip is a sentence addressed to nobody on a touchscreen

`on_disabled_hover_text` never appears under a finger. Every disabled-state
explanation in the client was one — `bookmarks` even records the fact in a
comment about long-press hints and then uses one anyway, to explain a control
disabled during world load, i.e. in the first thirty seconds a phone user ever
spends in the app.

So the reason a control cannot act goes through `theme::action_enabled_because`,
which draws a note under the control on the touch tier and a tooltip under a
pointer. **This is a class, not a site**: the answer has to be changeable in one
place, which means it has to be in one place.

The same argument produced `theme::kebab_button` earlier (a long press
advertises itself nowhere) and is why `theme::action_button` exists at all:
`egui::Button::small()` is not a smaller button — it zeroes the vertical padding
*and* skips the `interact_size` floor, so the widget collapses to its glyph, at
about 20 dp against a 48 dp guideline.

**Unit conversion for this codebase:** Android is 1.5 dp/pt (`phone_ui_scale`),
the web phone tier ≈1.83 (dpr 2.75 ÷ `WEB_PHONE_MAX_DPR`). Android binds, so a
control needs **≥ 32 pt** to clear `MIN_TOUCH_TARGET_DP`. Documented phone
viewports are 274 pt (Android) and ≈214 pt (web) — test against both; the web
one is narrower and is the one layout tests keep forgetting.

## One dismissal ladder, and adding a surface means adding a rung

`systems::dismiss` owns Escape and Android Back. The rule is that **one press
dismisses exactly one thing, the innermost**, and the order lives in one enum
whose variant order *is* the specification.

Why it cannot go back to being per-panel handlers: the Bevy→egui key bridge
forwards a key only while a text field has focus, which is right for activation
keys (Space would otherwise fire the last-clicked widget from across the map) and
was applied to every key. That made three of the four `Key::Escape` handlers in
the client unreachable code — and the moment Escape *is* forwarded, four
independent reads of one key with no order between them close two things per
press. Escape and Tab are now the two keys exempt from that gate.

One subtlety that is not obvious and will bite anyone reordering the ladder:
**egui's `Memory` consumes Escape to clear widget focus whatever this code
decides.** So "a text field had focus when the frame began" is a rung the ladder
*declines* to act on. Without it, one press blurs the search box and closes the
panel the box lives in.

## A menu row is not a button

`theme::menu_entry` is the app menu's row: full width, an icon column, a hover
highlight, the accent when what it opens is open — and no frame. It exists
because the menu was `ui.button` in a `top_down_justified` layout, which egui
draws as a full-width plate; a dozen of those read as a form of primary actions
rather than as a list, and the entries that *were* `selectable_label` drew nothing
at all, so one card carried two vocabularies and neither said which entries were
lit. The rank ladder now reads: `primary_button` (one per surface) → `action_button`
→ `chip` (a switcher segment, which is a plate on purpose) → `menu_entry` → plain
label.

## Where the vocabulary is thin

Recorded so the next person does not re-derive it:

- **There is no toast, snackbar or undo anywhere**, and a toast is the wrong
  shape here — a floating `Area` paints over panels (see `map-chrome.md`), the
  bottom edge belongs to the dock and, on a phone, to the on-screen keyboard.
  The idiom that works is a 1.5 s glyph swap on the control itself (the Search
  panel's Set Home / Copy link) for a synchronous act whose result is off-screen, and
  a status line inside the owning panel for anything that can fail or take time.
  Destructive actions arm in place (`theme::confirm_button`) for the same reason.
- **The `Layers` panel's order is declaration order, not a decision** — the
  `LAYER_DEFS` loop runs first because that const was the module's original
  subject, and Buildings, Map and Height were appended below it. Two layers that
  are off by default own the first screen on a phone.
- **`map_enabled` is not the parent of the toggles indented under it.** It
  reaches the raster clipmap only; Trees, Street furniture, Road textures and
  Water shading act on the *vector* surfaces, which it does not touch. Street
  labels is the one genuine child. The panel draws a hierarchy the code does not
  have.
