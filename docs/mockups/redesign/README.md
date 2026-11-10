---
topic: redesign
triggers: [theme.rs, dock.rs, ui.rs, glass.rs, hud.rs, action_ring.rs, place_card.rs, building_card.rs, map_osd.rs, chats.rs, RightPanel, "navigation", "tab bar", menu, glass]
updated: 2026-11-08
---

# UI: het thema en de navigatie, opnieuw

Een voorstel, geen code. v1 hieronder gevraagd op 2026-11-07: "de ui/ux is onduidelijk. ik kan de helft
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

## v2 · twee aparte vragen

Gevraagd 2026-11-08, na v1. De persoon zei dat het om twee aparte vragen gaat:

- **Vraag 1, het thema.** De ronde knoppen waren niet slecht; het glas was slecht
  uitgevoerd en deed de web-UI na. Gevraagd zijn de kleuren van de midas-web-UI, en
  een UI die minder krap is.
- **Vraag 2, de navigatie.** Die voelt niet intuïtief. Er moet een duidelijk onderscheid
  komen tussen de wereld en de community. In v1 stonden community-dingen als knoppen in
  de wereld ("Say hi", "Post here"), zonder dat duidelijk was hoe die twee samenhangen.
  Ook namen de benoemde kaartknoppen van v1 te veel ruimte.

v2 is getekend op de echte maat van de app: 275 pt breed, zo breed als de foto's van
vandaag ernaast. v1 was getekend op 390 pt en leek daardoor ruimer dan eerlijk is.

### Vraag 1 · midas' kleuren, met ruimte

De indeling blijft zoals ze nu is. Wat verandert: de kleuren uit midas'
`web/src/theme.css`, de schaduwen uit midas' `app.css`, effen vlakken in plaats van glas,
ronde knoppen van 48 pt, en regels van 56 pt. Licht en donker volgen de bestaande
instelling Light (Auto / Light / Dark).

![theme](v2-theme.png)

### Vraag 2 · World en Community uit elkaar

- **Twee ruimtes.** De wereld is de kaart met ronde knoppen. Community is een effen
  pagina met lijsten.
- **Eén schakelaar.** Onderaan staat World | Community, in beide ruimtes op dezelfde plek.
- **Je gezicht.** Rechtsboven, in beide ruimtes; het vervangt het ☰-menu.
- **Drie bruggen ertussen, en geen andere:**
  - ① markers op de kaart openen in Community;
  - ② "Show in world" gaat de andere kant op, met daarna "‹ Back";
  - ③ "Share" stuurt een plek naar een gesprek of community.

![world and community](v2-world-community.png)

Dezelfde schermen in midas' donkere kleuren. Ze komen uit hetzelfde bestand, met
`v2-world-community.html?dark`:

![world and community, dark](v2-world-community-dark.png)

### Inloggen en je profiel, licht en donker

Gevraagd 2026-11-08 bij v2. De ingang is je gezicht rechtsboven: zonder account
is dat een grijs rondje. Inloggen gebruikt velden zoals midas, met het label erboven.
Een ander account (Mastodon, Codeberg …) staat onder "or with". Je profiel heeft het kopje
"How others see you". Iemand anders' profiel heeft Message, Call en "Show in world" ②.

![account](v2-account.png)

Nog niets besloten. Vervolg op 2026-11-10 in `fluid-ui/`: v2's indeling als lagenmodel, de beweging erbij, en waarom egui hapert.

## v1 (vervangen door v2)
### Wat er nu misgaat, de regels en de indeling

![structure](v1-structure.png)

### Telefoon, scherm voor scherm

![phone](v1-phone.png)

### Desktop

![desktop](v1-desktop.png)
