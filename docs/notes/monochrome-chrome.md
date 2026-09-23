---
topic: ui
triggers: [theme.rs, palette, colour, color, accent, orange, brand, glass, translucent, "panel fill", contrast, WCAG, "index.html", splash, monochrome]
updated: 2026-09-10
---

# The chrome is monochrome glass, and the logo is not

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

## The glass, the blur, and the one number that is not a taste knob

`GLASS` is `SURFACE` at 84 %, and it is the panel and window fill. Alongside the
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
GUI, the atlas console in `crates/atlas/console`, carries the wordmark too.
