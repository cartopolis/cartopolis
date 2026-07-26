# Fonts

**Plus Jakarta Sans** (SemiBold) — SIL Open Font License 1.1, by Tokotype.
<https://fonts.google.com/specimen/Plus+Jakarta+Sans>

Used for the boot splash wordmark, in both halves of it: embedded into the
binary by `systems::splash` (`include_bytes!`) and served to the browser by
`index.html`'s `@font-face`. egui reads TTF/OTF only — not woff2 — which is why
the TTF is the file kept here rather than the smaller web format.

The OFL permits redistribution bundled with software; keep this attribution
alongside the file if it moves.
