---
topic: ui
triggers: [theme.rs, palette, colour, color, accent, orange, brand, glass, translucent, "panel fill", contrast, WCAG, "index.html", splash, monochrome]
updated: 2026-11-10
---

# The chrome was monochrome glass (now solid), and the logo is not

> **The palette half of this note is history.** The monochrome ramp and pale
> accent gave way on 2026-10-04 (charcoal and orange) and again on 2026-10-10 to
> a deep teal with a saturated teal accent — the pale accent read as one more
> grey on every selected control. `theme.rs`'s module note has the sequence and
> `ui-vocabulary.md` the current rule. What below still holds: the brand orange
> scoped to the logo, the monotonic state ladder, the glass and its alpha floor,
> the two-ring focus, and `index.html`'s loader tones.

Until 2026-09-10 the UI was a slate-blue ramp with an orange accent, taken from
the logo. It is a graphite ramp with a pale accent now, translucent and blurred
behind, and the two sentences worth remembering are: **the chrome carries one
hue at very low saturation**, and **the brand orange still exists, scoped to the
logo** (`theme::BRAND` — the mark's roofs and pin, the wordmark tail,
`index.html`'s loader bar; nothing else).

**Monochrome means one hue, not none.** The ramp was neutral grey for half a day
and read flat: a colourless panel over a coloured map is a dialog pasted onto a
photograph, and glass has never been colourless — the faint cast it gives what
you see through it is most of what identifies the material. Everything in the
chrome is hue 197° (blue-teal) at 6–14 % saturation, applied at constant HSL
lightness so no contrast ratio moved by more than 0.5.

## Why the hue went

Not taste, and not "flat is modern". The chrome sat over a basemap that is
itself full of colour — green parks, blue water, beige paper, and on the aerial
layer a photograph — and two saturated families competed for the same eye. An
accent means "look here" only if nothing else in the frame is shouting; over a
city at z17 the orange was one more coloured thing among thousands, and it had
also spread: by the end it was on the dock discs, the compass needle, the chat
names, the frame-time readout, the microphone meter and a card that meant
"rerouting". A colour that appears everywhere is not an accent, it is a tint.

What replaced it is **lightness**, which the map cannot take away. The state
ladder is monotonic and `theme::tests::the_state_ladder_rises` pins it:

| state | fill | rim |
|---|---|---|
| rest | `SURFACE_RAISED` | `LINE` (solid, ≥3:1 — a *control* boundary) |
| hover | `SURFACE_HOVER` | `ACCENT` |
| pressed | `ACCENT_MUTED` | `ACCENT_RING` |
| engaged (panel open, flying, locked) | `ACCENT` — a **light plate** | `ACCENT_RING`, or `ACCENT_BRIGHT` while *held* |

The engaged step **inverts** rather than tinting: a light plate with `INK` on it.
That is not decoration, it is the only treatment that survives a monochrome
palette — "on" and "off" cannot be two greys a few percent apart on a control
that floats over arbitrary imagery.

## What the palette lost, and what took the work over

- **The second accent (`AMBER`) is gone.** It meant "held" (north-up lock,
  caps lock) in some places and "caution" (clipping meter, unsaved script, a
  frame over 33 ms) in others — two meanings on one constant, which only worked
  while the colour was doing the explaining. Caution went to `WARN`, which is
  what it always was; held went to `ACCENT_BRIGHT` **plus weight** — the compass
  lock draws a second 2 pt ring over the disc's own 1.5 pt one, and caps lock
  wears the engaged plate where a one-shot shift only gets the pressed fill.
- **`ACCENT_TEXT` and `ACCENT_BRIGHT` now hold the same value** (pure white). One
  is type, one is a stroke; they answer to different tests and are free to move
  apart again. This is deliberate, not a leftover.
- **`WARN` and `ERROR` kept their colour.** Caution and fault are statements
  about the world rather than decoration, and a grey warning is not read as one.
  They are the only coloured tones left in the chrome.
- **Road signage was never in scope** and still isn't: `driver::DRIVE_BLUE`,
  `SIGN_RED`, `ARRIVE_GREEN`, `REROUTE_AMBER` and the route's green/red endpoints
  are wayfinding convention. The same exemption covers `DEFAULT_PIN_COLOR`, which
  stayed orange because a pin is *map content* — it has to be found against parks
  and paper, and grey is what a map already has plenty of.

## The glass is gone (2026-11-08)

**Every surface is solid now**, and `systems::glass`, the blur behind the
panes, is deleted. The person's words: "the round buttons weren't bad; it's
just a bad implementation of glass, and a try to mimic the theming of the web
UI". What they saw, on a dense city (`docs/mockups/redesign/v1-structure.png`):
the ×1.69 saturation lift turned the blurred roofs into a brown wash behind
the type, and the round controls were tinted but never blurred, so two kinds
of "glass" sat side by side.

What replaced it is the midas hub's layering, value for value, at one
darkness: `PANEL`, `GLASS` and `OVERLAY` are all the hub's dark teal card (`#192627`, measured off its question card; `INK` "looked black" and `#0B1918` had too little colour) under a
soft shadow, cards inside a panel are `SURFACE` wells a step darker, as the hub's options are, and every rim is the hub's
border (`PANEL_RIM`, `#2A3536`). Floating cards were `SURFACE` for one round
and the person saw them as "panels lighter than others; they must be dark". The
discs are flat plates under one shadow: the sheen and the accent glow went
with the glass. The web's `--c-glass` was **not** changed: `web_theme.rs`
holds it at 92 %, because the ask was about the app.

Do not bring a blur back to answer "the map should show through". That was
the argument for every step below, and each step ended at a darker slab or a
brown one. If the map has to be seen, move the panel off it (the desktop
panels already sit beside the map).

The rest of this section is the history of the glass, kept so the next
person who wants it knows what it cost.

## History: the glass, the blur, and the one number that was not a taste knob

`GLASS` is `SURFACE` at 92 %, and it is the panel and window fill (84 % until
2026-11-01; see "The midas hub's tones" below). Alongside the
translucency: a white-at-low-alpha rim (`OVERLAY_LINE`) rather than a drawn
border, wider radii (`RADIUS` 6→8, `RADIUS_LG` 10→14), a softer and further
shadow — and a real backdrop blur (`systems::glass`).

**The blur is what bought the transparency, and it only works in that order.**
egui has no backdrop-blur anything, so the first version had none, and it could
not afford to be thin: at 93 % the roof edges and tree crowns of Groningen read
straight through the Settings panel's body copy. A blur reduces detail to a
wash, which is the one thing a fill cannot do at any alpha, so with one behind
it the panel went to 84 % — noticeably see-through — and stayed legible.

How it is built is in `systems::glass`'s own module note; the two facts worth
having here are that it is **not load-bearing** (no backdrop registered means
flat fills, i.e. the look before it existed) and that it hangs a textured twin
under every rectangle painted in a glass fill, so it covers panels, windows,
menus and floating readouts without any of them knowing it exists.

Later facts (2026-10-31). **The blur runs only on frames whose geometry holds
glass** (`EguiRenderOutput::glass`): until then its two quarter-size passes ran
every frame, behind an opaque full-screen panel on a phone too, although
`theme::PANEL`'s note already claimed the panels did not pay for it.

**Over a blur a pane is a clear rim round a dark core**, not one tint
(`glass::core_shape`, `theme::GLASS_RIM` / `GLASS_CORE`, a 6 pt rim). The person
looking at the 84 % panes said the glass was not visible, and the arithmetic
agrees: wherever type can sit the pane must be dark over white paper, and at
that darkness a blurred city shows at a sixth of itself. Two ways round it
were tried that day and **both lost**:

- *Dim the backdrop, thin the tint.* Over white the result is the same, so the
  city shows at about the same sixth — the slab only got darker.
- *Compress the backdrop's brightness with a curve* (paper pressed to the floor,
  the city keeping its spread). A sunlit city is itself bright, so it was
  pressed flat too: a darker slab again.

The rim is where the glass is seen: no type reaches it (every glass frame's
inner margin is ≥ 8 pt), so it can be nearly clear, and the blurred city shows
through it at five sixths, the edge of a thick sheet. The core, inset, carries
the darkness, and `the_glass_core_is_no_lighter_than_the_flat_pane` holds core
+ rim + sheen to `GLASS`'s worst case. With the rim go a sheen (a texture
fading from the top, `glass::sheen_image`), a lit top line, a hairline on the
core's edge and a saturation lift in the blur pass (×1.69, about the luminance,
so no ratio moves). The dock became one such pane (`dock::bar_pad`), its discs
glyphs on it (`theme::bar_button`).

**The alpha is load-bearing in both directions, and only one of them is a
test.** Downward: a translucent panel's real background is not in `theme.rs` at
all — it is the basemap, and the basemap is pale beige land on near-white paper.
`text_clears_every_surface_it_can_land_on` composites every text tone through
`GLASS` over white and measures it, so a panel made prettier by being more
transparent fails there. This is the same trap `OVERLAY`'s alpha was raised for
on 2026-08-14, one surface later.

At 84 % `TEXT_DIM` measures 5.06:1 against white paper and **4.40:1 one step
further** (alpha 205), so this is a floor with one step of margin, not a
preference. Note what the test cannot know: it composites against *unblurred*
white, which is the honest worst case, since no blur makes a white page darker.

Upward, nothing measures it and a rendered city is the only instrument — that is
how the pre-blur value was chosen (93 % passed every ratio and still showed the
city through the body copy). Check a change here against a shot of a dense city,
not against the table.

## The midas hub's tones (2026-11-01)

The midas hub's page (`jeroen/midas`, `web/src/theme.css`) is a copy of the
web's generated `theme.css`, so its colours *are* these constants; what the
person saw as "midas's panel tones" was how it lays them out in dark mode:
layers of solid `SURFACE` cards on an `INK` page, each with a soft shadow, and
`ACCENT_RING` on `ACCENT_MUTED` tags. The person put it as "the colours of the
panel layers, and the depth", and kept the orange as an accent. In both halves
at once:

- `GLASS`, `OVERLAY` 84 → 92 %, `GLASS_CORE` 87 → 97 %: at 84 % a card over a
  bright map came out a thin mid grey beside the hub's teal near-black. The rim
  stays clear, so the blur is still seen. The web's `--c-glass-soft` (dark)
  went from `SURFACE_RAISED` at 55 % to the same `GLASS` alpha of `SURFACE`.
- Tags: `contacts::fact` and the web's toned `.m-badge` are the hub's flat
  teal tag; a neutral web badge keeps its rim, the only thing marking it on a
  card.
- Layers: `PANEL` is `INK`, the hub's page (it was a step lighter, so a
  `SURFACE` card in a panel stood off it by less than a step), and the cards
  in it — `list_group`, chat, community and contact rows (`chats::ROW_CARD`,
  which was white at 4 %) — are `SURFACE` under `theme::CARD_SHADOW`, the
  hub's `--lift-2` as one egui shadow. The web's cards take the same lift, and
  its dark `--m-shadow-color` went from 45 to 60 % black.
- Turning the Layers panel's brand-orange "on" teal was tried and taken back
  the same day: the orange is wanted there as an accent.

When midas's copy of `theme.css` is refreshed it picks up the token changes;
the rest is layout, and lives in each repository.

## A light theme beside the dark one (2026-11-10)

The person asked for "the midas web UI's colours", light and dark. The dark
theme is the palette above, unchanged (`theme::DARK`); `theme::LIGHT` is the
hub's light tokens role for role (page `#F1F5F5`, white cards, text `#1F2829`,
strong `#0F1416`, dim `#546568`, hairline `#DAE3E3`, accent-soft `#D9F0EE`,
warning `#855700`, danger `#B3361E`). Three values move off the hub's, each
for a 3:1 or 4.5:1 that a `theme` test holds in both themes:

- **The accent is `#34938E` on the light theme, not `#5CC6C0`.** The hub's
  accent is 2.0:1 on white, so an engaged plate, a slider's fill and the hover
  ring identified nothing against the light surfaces. Same hue, deeper; the
  hub's own value is what a plate lifts to under the pointer (`accent_lift`).
- `line` is the hub's strong border a step darker (`#788B8E`): `#84979A` is
  2.8:1 on the page.
- `accent_ring` / `accent_pressed` are `#318C87`, so the pressed tag's rim
  clears 3:1 and the plate's ink 4.5:1.

The shadows scale by `Palette::shadow` (0.35 on the light theme, the hub's
faint lift), and the focus ring's two rings swap: the ink is the light half
there, the strong text the dark one. Switching: `ui-vocabulary.md`.

## The focus ring is two rings

`paint_focus_ring` draws `INK` under `CREAM`. It is drawn *outside* the control,
so its background is whatever is behind — map at any brightness, a panel, or the
light plate of the control it encircles — and no single tone clears 3:1 on all
of those. The monochrome palette makes this worse rather than better: there is no
saturated colour left to fall back on. The assertion is not "the ring clears
3:1" but "**at least one half of it does, on every background it can land on**".

## Where else the palette is written down

`crates/cartopolis/index.html` — the loader is the same screen handed over
mid-load, so its five tones are the five constants and drift there is invisible
from Rust. Only one pair is unit-tested (the wordmark orange,
`the_wordmark_is_one_orange_on_both_splashes`); the rest is a person. The account
GUI, the atlas web UI in `crates/atlas/web`, carries the wordmark too.
