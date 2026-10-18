# Fonts

**Plus Jakarta Sans** (SemiBold and Regular) — SIL Open Font License 1.1, by Tokotype.
<https://fonts.google.com/specimen/Plus+Jakarta+Sans>

Used for the boot splash wordmark, in both halves of it: embedded into the
binary by `systems::splash` (`include_bytes!`) and served to the browser by
`index.html`'s `@font-face`. egui reads TTF/OTF only — not woff2 — which is why
the TTF is the file kept here rather than the smaller web format.

The Regular is the body face of every panel (`theme::BODY_TTF`), from
<https://github.com/tokotype/PlusJakartaSans> (`fonts/ttf`).

The OFL permits redistribution bundled with software; keep this attribution
alongside the file if it moves.

**Material Symbols Rounded** (`MaterialSymbolsRounded-subset.ttf`) — Apache
License 2.0, by Google. <https://fonts.google.com/icons>

Every icon the UI draws. Embedded by `systems::icons`, which is also the table
of which icons exist: each row names a codepoint and Google's name for it.

This file is **generated**, not downloaded — do not replace it with the upstream
release. `tools/subset-icon-font.py` instances the 15 MB variable font at one
style (`wght` 500, `FILL` 0, `GRAD` 0, `opsz` 24) and subsets it to the ~80
glyphs the table names, which is what makes it 16 kB and reasonable to ship in
an APK. Add an icon by adding a row to the table and re-running the script;
`icons::tests::every_icon_has_a_glyph` fails if the two fall out of step.

Apache-2.0 requires the licence and attribution travel with the font: keep this
notice, and note the file is a modified (subset) copy.

**Noto Emoji** (`NotoEmoji-Regular.ttf`) — SIL Open Font License 1.1, by Google.
<https://fonts.google.com/noto/specimen/Noto+Emoji>

The one-colour emoji face at the end of every egui stack (`theme::EMOJI_TTF`),
so chat emoji draw instead of `◻`: egui's bundled Noto Emoji stops at U+1F6C5.
Google Fonts ships it as a variable font (`NotoEmoji[wght].ttf`, google/fonts
`ofl/notoemoji`); this file is that instanced at `wght` 400 with fontTools
(`fonttools varLib.instancer NotoEmoji[wght].ttf wght=400`), 0.9 MB.
