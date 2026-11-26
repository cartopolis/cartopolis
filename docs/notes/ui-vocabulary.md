---
topic: ui
triggers: [systems/gui/theme, ui_theme.rs, dismiss.rs, palette.rs, tokens.rs, UiTheme, ui_theme, Rgba8, "Appearance", map_disc, round_button, "design system", "light theme", "dark theme", contrast, tofu, glyph, emoji, icon, accessibility, a11y, WCAG, escape, "empty state", spinner, disabled, confirm, "touch target", systems::theme, app_tokens, RightPanel, phone-ui-shots]
updated: 2026-11-26
---

# The UI vocabulary, and the things that keep breaking it

`systems::theme` is the design system: one palette in two themes, the type
and spacing scales, and the tokens the component library's screens draw with.
This note holds what a reader of that module cannot derive from it — the
measurements it was tuned against and the rules whose violations are invisible
until someone runs the app.

Measurements dated 2026-08-14 unless marked, from a ten-lens sweep of the
client. Where a number is now a unit test, the test is named, because the test
is the thing that stays true.

## `theme` is the only place a chrome colour comes from

UI colour, spacing and type live in `systems::theme` and nowhere else; colours
are `theme::Rgba8`. A screen takes its chrome from `theme::p()` (the palette in
force) and the library's tokens (`theme::app_tokens()`, from `theme/tokens.rs`,
the same table `web_theme.rs` writes the site's `theme.css` from). A fresh
colour literal in a screen is how the chrome drifted before: the HUD, the stats
readout and the bottom bar were once three hand-rolled boxes with two alphas,
two radii and three margins. A palette for *content* — people, categories, the
scenario inks — is a named const in its module.

**Two palettes since 2026-11-10: the colours are `theme::p().field`, not
constants.** `theme::Palette` has one field per chrome role, `theme::DARK`
holds the look as it was (value for value, pinned by
`the_dark_palette_is_the_look_it_was`) and `theme::LIGHT` the midas hub's light
tokens in the same roles. Read the palette where you paint — never into a
`const`, `static` or cached field, which keeps the other theme's colour after a
flip. What switches it is **its own setting**, `AppPrefs::ui_theme` (Settings ▸
General ▸ Appearance: Auto / Light / Dark; `systems::ui_theme`, in
`ClientPhase::Input`, through `theme::set_light`): Auto is light while the real
sun is above the horizon at the map, whatever the sky is set to. Until then it
followed the scene's light, so a dark UI made the map night.
`CARTO_UI_THEME=light|dark` forces one for a capture. Brand and data colours
(`BRAND`, the council, the map labels, the call buttons, the scenario inks)
stay `const`.

**A screen dark in both themes draws in `theme::DARK`'s colours rather than
mounting dark** — the splash, the call and camera screens, the script editor's
code box, anything on a `LABEL_CHIP`. The host's dark switch is one for every
screen, so a screen that mounted dark turned every other screen dark with it
(`lib_camera`, 2026-11-25; `component-library-in-the-client.md`).

**Roles worth knowing.** `ink` is the deepest background (wells, a segmented
track) and `on_accent` the ink on an accent plate; `on_status` is the ink on a
warning or error plate and `accent_word` the accent-coloured word on a soft
tag. **The map's round buttons have their own four roles** (2026-11-10):
`map_fill`, `map_ink`, `map_rim` and `map_mark` — plate, glyph and rim of the
round buttons, the search pill, the World | Community track and the zoom
rail's thumb. In the light theme they are white with the deep accent teal
`#17696A`: the person's "the light buttons have no teal theme at all" was
black glyphs on white discs (`the_map_buttons_wear_the_teal_in_both_themes`).

A panel's name and glyph are `RightPanel::label()` and `RightPanel::icon()`, so
the dock disc, the menu entry and the header read one table.

**The palette is deep teal, not the logo's slate and orange** — since
2026-10-10, read off Android's keyboard in its teal dark theme. The logo's
orange (`theme::BRAND`, `#FD6012`) stays with the mark, the wordmark and the
badges. `theme`'s module note records the circle it took to get here (slate +
orange → monochrome on 2026-09-10, `monochrome-chrome.md` → charcoal and ember
→ teal) so nobody walks it again.

**Panels are styled after the midas hub's, on the person's word.** On
2026-11-08 a glass pass was taken back as "a bad implementation of glass, and a
try to mimic the web UI": solid cards on an `ink` page, the hub's `#2A3536`
hairline (`panel_rim`), and more room (rows 28 / 40 pt). One place
deliberately departs from the hub: a control's hairline is `line`, not the
hub's quieter border, which fails the 3:1 control boundary below. Midas
screenshots for comparison are in midas's `docs/screens/`.

## The controls are the web's

Decided 2026-11-22, shown egui's Search panel beside the light web build:
"those buttons and input fields are really badly styled — is egui hard to
handle?" egui had one look per state for every widget type and no selectors, so
a focused field wore the pressed button's plate. The answer then was a widget
set drawn to the web component library's measurements; since 2026-11-26 it is
the library itself (`@garage44/mithril`'s components, drawn by `mithril_bevy`),
so the app and cartopolis.org are one design by construction. Judge a screen
against the web page beside it, both at phone width (`docs/phone-ui-shots.toml`,
1.5×). Two places where the *web* departs from its own library and the app
follows the web: a segmented control's chosen option is a teal plate
(`depth.css`), and the current nav item an accent-soft bar. Type sizes are the
app's, not the site's (`theme::app_tokens`, `component-library-in-the-client.md`).

## The palette must be measured on every surface, not on `surface`

Contrast was originally checked against the panel fill and the accent fills.
That is the easy half. **The overlay's background is not in the theme at all**:
the imagery is a map, pale beige land on near-white paper, so a translucent box
over it lands three steps lighter than the panel its colours were picked
against. At the old 84 % alpha the dim text measured 4.12, the accent text 4.11
and the error 3.59 — under 4.5 over ordinary daytime map. (Every surface has
been solid since 2026-11-08, which removed that composite; the rule stands for
anything translucent.)

`text_clears_every_surface_it_can_land_on` pins the matrix. **Add a colour, add
its row**; a colour tested only against `surface` has been tested against the
one background it will not always have. Two further rules, both tests:

- **A control has to be findable before its label matters** (WCAG 1.4.11, 3:1).
  A button at rest is `surface_raised` on `surface`, 1.24:1 of fill difference,
  so the hairline is the only channel identifying it
  (`a_control_is_distinguishable_from_the_surface_it_sits_on`).
- **Prose does not recompute.** Three ratios written in the theme were wrong and
  a fourth was found by writing the test that checks them
  (`the_documented_ratios_are_the_measured_ones`).

**Rejected: stopping down the camera instead of fixing the colours.** The
near-white map sits on the tonemapper's shoulder, where exposure barely darkens
it but dims the limb and the stars. Contrast between chrome and its fill is not
a lighting problem.

## A tooltip is a sentence addressed to nobody on a touchscreen

A hover text never appears under a finger, and every disabled-state
explanation in the client once was one — including the one explaining a
control disabled during world load, i.e. the first thirty seconds a phone user
spends in the app. The reason a control cannot act is drawn under it on the
touch tier. A long press advertises itself nowhere either, which is why a row
with more actions carries a ⋯ menu.

**Unit conversion for this codebase:** Android is 1.5 dp/pt (`phone_ui_scale`),
the web phone tier ≈1.83 (dpr 2.75 ÷ `WEB_PHONE_MAX_DPR`). Android binds, so a
control needs **≥ 32 pt** to clear `MIN_TOUCH_TARGET_DP`. Documented phone
viewports are 274 pt (Android) and ≈214 pt (web) — test against both; the web
one is narrower and is the one layout tests keep forgetting.

## One dismissal ladder, and adding a surface means adding a rung

`systems::dismiss` owns Escape and Android Back. **One press dismisses exactly
one thing, the innermost**, and the order lives in one enum (`Rung`) whose
variant order *is* the specification. Per-panel handlers are what it replaced:
four independent reads of one key with no order between them closed two things
per press.

**A page inside a panel is a rung, and so is each card on the map** (since
2026-11-04). The place, building and person cards were once not on it, so Back
with only a card up found nothing open and sent the app to the background. A
page with a back chevron is `Rung::PanelPage`, before `RightPanel`; the cards
come after the panel, since on a phone the panel covers them. The one such page
now is a chat's World view (`ChatsView::lib_world`). **A new library page with
its own chevron has to be added to that rung**, or Back skips it.

## Where the vocabulary is thin

- **There is no toast, snackbar or undo anywhere.** The bottom edge belongs to
  the dock and, on a phone, to the on-screen keyboard. The idiom that works is a
  1.5 s glyph swap on the control itself (Search's Set Home / Copy link) for a
  synchronous act whose result is off-screen, and a status line inside the
  owning panel for anything that can fail or take time. Destructive actions arm
  in place for the same reason.
- **`map_enabled` is not the parent of the toggles indented under it.** It
  reaches the raster clipmap only; Trees, Street furniture, Road textures and
  Water shading act on the *vector* surfaces. Street labels is the one genuine
  child.

Until 2026-11-26 this note also held egui's traps — `theme::install` before the
first pass, `RichText::strong` taking the pressed stroke, bundled fonts with no
emoji (`icon-font.md` keeps the glyph story), frames that do not wrap,
`Button::small()`, `back_title`'s close handshake, and the Escape egui's
`Memory` ate. They went with egui.
