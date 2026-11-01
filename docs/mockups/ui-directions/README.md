---
topic: ui-directions
triggers: [theme.rs, dock.rs, "design system", palette, accent, "map chrome", RightPanel, place_card.rs, data_layers.rs, communities.rs]
updated: 2026-11-01
---

# Drie richtingen voor de UI

Een voorstel, geen code. Gevraagd 2026-11-01 ("verbeter het design van de ui van
cartopolis, stel een paar richtingen voor met mockups"). Alles op de bladen A–C
is **nieuw**; het blad *Zo is het nu* is gefotografeerd van `main` (d0103e24)
met `docs/phone-ui-shots.toml` op schaal 1,5 en een desktopvariant op 1600×900.

De richtingen lopen op in hoe ver ze gaan:

- **A · Rustig glas** — dezelfde teal, dezelfde dock; drie knopgewichten, één
  accent (oranje alleen logo en live), kaartbediening in één capsule, labels in
  de dock, een zwevend paneel op desktop, plaatskaart als sheet.
- **B · Atlas** — licht papier en inkt overdag, met legenda-stippellijnen,
  kleine kapitalen, windroos en schaalstok; 's avonds dezelfde layout in donker
  glas, gekozen door de zon boven de plek.
- **C · Hier en nu** — een andere opbouw: een vaste tabbalk (Map · Chats ·
  Calendar · You) en op de kaart altijd een sheet over de plek in beeld: wie er
  is, wat er vandaag gebeurt, wat er gezegd is.

A en C sluiten elkaar niet uit (C's structuur met A's knopgewichten), en B's
"zon kiest licht of donker" kan op elk van beide.

Nog niets besloten.

## Zo is het nu

![today](v1-today.png)

## A · Rustig glas

![A](v1-a-quiet-glass.png)

## B · Atlas

![B](v1-b-atlas.png)

## C · Hier en nu

![C](v1-c-here-and-now.png)
