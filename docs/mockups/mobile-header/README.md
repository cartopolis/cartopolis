---
topic: mobile-header
triggers: [home.css, "home-top", "home-seg", "crates/atlas/web", phone, mobile]
updated: 2026-11-01
---

# The web page's top bar on a phone: mockup

The bar as it was on a phone (the logo's tile, a filled teal switch, Admin in
gold, a boxed theme switch: three blocks in three colours), and three ways to
make it calmer, in the web UI's light palette (`crates/atlas/web/src/css/theme.css`).

## v1

![v1](v1.png)

## Decided (2026-11-01)

**B, one quiet bar**: the mark, the three places as words with the open one
underlined in the accent, every word in one colour, and the account. A wide
screen keeps its switch; only `max-width: 760px` changes (`home.css`).
A (tabs at the bottom) and C (the title opens the choice) were not taken.
