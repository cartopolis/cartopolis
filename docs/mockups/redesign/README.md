---
topic: redesign
triggers: [theme.rs, dock.rs, ui.rs, glass.rs, hud.rs, action_ring.rs, place_card.rs, building_card.rs, map_osd.rs, chats.rs, RightPanel, "navigation", "tab bar", menu, glass]
updated: 2026-11-07
---

# Opnieuw ingedeeld: vier tabs, benoemde knoppen, dichte panelen

Een voorstel, geen code. Gevraagd op 2026-11-07: "de ui/ux is onduidelijk. ik kan de helft
niet terugvinden en de ui elementen zijn lelijk. dat glas effect werkt niet, de menu items
zijn te krap, functies van buttons zijn niet duidelijk, navigatiestructuur is onduidelijk."

Het blad *Wat er nu misgaat* is gefotografeerd van `main` (0a7e2a97) met
`docs/phone-ui-shots.toml`, op schaal 1,5 op mac-1. Al het andere is **nieuw**.

Dit bouwt voort op richting C uit `ui-directions/`, die op 1 november gekozen werd maar
nooit is gebouwd. Sindsdien kwam alleen het glas van v3 in de app (d07fbe25), en dat is
precies wat nu niet werkt. Vijf regels:

1. **Vier vaste tabs**, Map · Chats · Calendar · You. Ze blijven altijd staan; het ☰-menu,
   de dock, de Community | World-schakelaar en de rondjes linksboven vervallen.
2. **Elke knop heeft een naam.** Alleen ✕ en ‹ mogen zonder, en ‹ zegt waar je heen gaat.
3. **Eén hoofdactie per scherm.** De rest zijn benoemde tegels of omlijnde knoppen.
   Weggooien is rood en vraagt eerst.
4. **Maten voor een vinger:** regels van 60 pt (nu 24 of 32), knoppen van 48 pt,
   tekst van 16 pt (nu 14 pt, met labels in de ring van 9,5 pt).
5. **Dichte panelen.** Het glas gaat weg: het vervaagt de kaart, maakt hem 1,69 keer
   zo verzadigd en laat hem door het paneel schijnen. Wat er op een drukke stad
   overblijft is een bruine vlek. Ronde knoppen waren alleen getint, en op Android en
   het web is het glas niet uit te zetten.

Op het eerste blad staat ook een tabel met waar elke functie van nu heen gaat.
Elf functies hadden maar één verstopte ingang: Layers, Lens, Calendar, Admin, Script,
Models, Plans, Back/Home, emotes, gezichten en Share diagnostics. Afsluiten op desktop
wordt een regel onder You.

Nog niets besloten.

## Wat er nu misgaat, de regels en de indeling

![structure](v1-structure.png)

## Telefoon, scherm voor scherm

![phone](v1-phone.png)

## Desktop

![desktop](v1-desktop.png)
