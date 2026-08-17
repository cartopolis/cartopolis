---
topic: ui
triggers: [icons.rs, theme.rs, "icon", "glyph", "tofu", "empty box", "material symbols", "first_drawable", subset-icon-font.py]
updated: 2026-08-16
---

# The icons are a font we ship, not characters we hope the font has

Until 2026-08-16 every icon in the client was a Unicode character picked out of
whatever egui's bundled stack — `Ubuntu-Light`, `NotoEmoji-Regular`,
`emoji-icon-font` — happened to cover, chosen at runtime through a preference
ladder (`theme::first_drawable(ctx, &["🔍", "⌕", "S"])`) that fell back to a
letter when the glyph was absent. `systems::icons` replaces that with a subset of
**Material Symbols Rounded**, bundled in the binary.

## Why the ladder had to go

It was doing its job: after `no_painted_glyph_is_tofu` landed, nothing in the app
drew an empty box. What it could not do is produce an *icon set*. The ladder
optimises each glyph independently against a font stack that was never designed
as one, and the survivors were a set of near-misses standing in for meanings they
did not have:

| meant | drew |
|---|---|
| restaurant | `⚔` crossed swords |
| café | `⛱` beach umbrella |
| shop | `⚖` scales |
| hotel | `♜` chess rook |
| school | `⚛` atom |
| station | `☸` dharma wheel |
| route panel | `⚑` flag (the compass rose was missing) |
| travel modes | *nothing* — the tabs were words, because `🚗 🚲 🚶` were all tofu |

They also came from three different faces, so weight, optical size and metrics
changed from icon to icon; and since the ladder resolves against whatever fonts
are actually loaded, the chrome could differ between platforms.

## What replaced it

`crates/cartopolis/src/systems/gui/icons.rs` is a table of `(constant,
codepoint, upstream name)` and is the **single source of truth**:

- `tools/subset-icon-font.py` parses it, verifies every codepoint against
  Google's published `.codepoints` manifest (a mistyped `\u{…}` would otherwise
  ship a plausible but *different* icon, and nothing downstream could tell),
  instances the variable font at one style and subsets it to those glyphs.
- `theme::fonts` appends the face to **every** family's fallback list rather than
  giving it a family of its own. It can't shadow anything: all its codepoints are
  private-use. That is what lets a label be `format!("{}  Copy", icons::COPY)` —
  one string, laid out once, in whichever family the widget already uses.
- `icons::tests::every_icon_has_a_glyph` fails if the table names an icon the
  committed subset lacks, which is the one way these two files can drift.

**Measured 2026-08-16:** upstream's `MaterialSymbolsRounded[FILL,GRAD,opsz,wght].ttf`
is 15.0 MB. Instanced at `wght` 500 / `FILL` 0 / `GRAD` 0 / `opsz` 24 and subset
to the 84 icons in the table it is **16.9 kB** — which is what makes an icon font
something you can `include_bytes!` into an APK at all. The instance is static on
purpose: the variable axes cost more than they buy for a set drawn at two sizes.

## The optical fit is two numbers

Material Symbols draws on a full em square (advance 1.0 em, ink from −0.10 to
0.88 em, `unitsPerEm` 960, ascent 1.10 em). Dropped into a text run at the same
point size as the text, an icon therefore reads a size too large and sits a touch
high, *and* it raises the row height of any line it appears in — epaint takes a
row's height from the tallest font in it, so a button with an icon in its label
would otherwise be taller than the identical button without one. `FontTweak {
scale: 1.15, y_offset_factor: 0.03 }` in `theme::fonts` is the whole correction,
and it was **set against rendered frames** (`--shot --ui menu`, `--ui route`), not
derived: the arithmetic answer — scale it down until the em square matches the
text's cap height — gave 0.9, and at 0.9 every icon read as timid beside the
label it belongs to, because Material draws an icon to be *bigger* than the type
around it (its own guidance is 1.2–1.5×). 1.15 is as far up as it goes before the
ascent starts adding row height, i.e. before a button with an icon in its label
grows taller than the identical button without one. Re-check both ends against a
shot if the body face or the type scale ever moves.

## `theme::first_drawable` went with the ladders

It was deleted rather than kept for a rainy day, because the workspace denies
`unused` and a helper nobody calls does not compile. What it was working around
is still true, though, of the *other* kind of non-Latin-1 character — a symbol
inside a sentence, a currency mark, the on-screen keyboard's `€` — which still
comes from egui's own stack, whose coverage remains both thinner than it looks
and wider than people guess. `theme::tests::no_painted_glyph_is_tofu` still walks
both crates' sources and holds every hard-coded glyph to that; its advice is now
"take it from `systems::icons`, or use something Latin-1". If a preference ladder
is ever wanted again, `git log` has it.

## Picking an icon: the dock is the hard case

An icon in this app is drawn at two sizes, and the small one decides. In the dock
a 20 pt glyph sits in a 44 pt disc with a map behind it, and detail that reads in
a menu row turns to noise there: `route` — Material's winding road, the obvious
name for a route planner — collapsed into an anonymous squiggle, and the panel
now wears `directions` (an arrow on a signpost) for that reason. When two names
fit, take the one with fewer strokes and one clear silhouette.

Two more traps in the same family. The subset is instanced at `FILL` 0, so
**there are no solid shapes in it at all** — `circle`, `fiber_manual_record` and
`square` all come out hollow, which is why the layers legend's colour key is a
ring (`icons::DOT`) and not a disc. And Material's name is not always its
picture: `toys` is what you reach for to mean a playground and it draws a toy
*car*, which on a map means a road.

## Two things that stay out of the icon set

- **`PICKED_PREFIX`** (`navigation.rs`), the `· ` in front of a map-picked
  endpoint, is *data*: `drive_route_geocoding` compares against it to decide
  whether a name may be overwritten. A stored sentinel is not chrome and does not
  become a picture.
- **The close button and the kebab menu** (`theme::close_button`,
  `theme::kebab_button`) are still painted from line segments. They predate the
  font and were built that way because `✕` was tofu — but they land on the pixel
  grid at any scale and at any DPI, which a hinted glyph at 12 pt does not, so
  there is nothing to gain by converting them.

## Licence

Apache-2.0 (Google). The obligation travels with the file: the notice lives in
`assets/fonts/README.md`, which also records that the shipped copy is *modified*
(instanced and subset). Adding an icon does not change any of that — but
replacing the file with an upstream release would, and would also undo the
subsetting the size measurement above depends on.
