---
topic: fluid-ui
triggers: [theme.rs, egui_manager.rs, dismiss.rs, dock.rs, ui.rs, action_ring.rs, place_card.rs, building_card.rs, onscreen_keyboard.rs, frame_pacing.rs, "systems/nav/navigation.rs", scenario.rs, chats.rs, RightPanel, animate_bool, animation_time, request_repaint, "page stack", "bottom sheet", sheet, transition, animation, motion, "reduced motion", navigation, "World | Community"]
updated: 2026-11-10
---

# Vloeiend: de indeling, de beweging, en waarom het hapert

Een voorstel, geen code. Gevraagd 2026-11-10: "we willen dat egui veel vloeiender
werkt, betere ux heeft met animaties, beter design en een logische indeling qua
navigatie. Maak een ontwerp."

Het bouwt op wat er al ligt. Het **thema** (vraag 1 van `redesign/`: midas' kleuren,
licht en donker, effen panelen, ronde knoppen met hun woord erin) is sinds
2026-11-10 op `main` en wordt hier niet opnieuw getekend. De **indeling** is die
van `redesign/` v2 (World en Community uit elkaar, je gezicht rechtsboven, drie
bruggen), hier voor het eerst als lagenmodel uitgewerkt, zodat elke overgang één
regel kan hebben. Wat nieuw is: de **beweging** (tijden, curves, één beweging per
laag, de kaartjes met drie rusthoogten, de vinger die een kaartje en een pagina
sleept) en het blad over **waarom egui nu hapert**, gelezen uit de code met
regelnummers, met een plan in zes stappen die elk alleen landen.

Alles op de drie bladen is **nieuw**, behalve de "Zo is het nu"-foto's onderaan.

## Blad 1 · De indeling

Zes lagen: de ruimte (World of Community), het chroom, kaartjes over de kaart,
pagina's (nooit dieper dan drie), bedekkingen, het toetsenbord. Een kaartje rust
op drie hoogten (kijkje · half · vol) en de kaart blijft eronder bewegen. Op
desktop zijn laag 2 en 3 één paneel van 400 pt naast de kaart. De 33 schermen
van `docs/phone-ui-shots.toml` staan in een tabel met hun ruimte, laag en het
aantal tikken vanaf de kaart.

![de indeling](v1-indeling.png)

## Blad 2 · De beweging

Vijf tijden en twee curves, letterlijk die van de midas-hub (`web/src/app.css`:
120 / 200 / 360 ms, `ease-out` en `spring`; "gaan" is 240 ms en "omgeving" 600 ms
erbij), zodat app en web-UI hetzelfde aanvoelen. Eén beweging per laag, vier
dingen die nooit bewegen, en vier filmstroken op de maat van de app: een
kaartje dat opkomt, een pagina die komt, wisselen van ruimte, een knop en een
marker. De instelling Motion: Normal / Reduced volgt het systeem en nooit de
framerate.

![de beweging](v1-beweging.png)

## Blad 3 · Waarom het hapert, en wat er verandert

Gelezen uit `main` (035fe5af), niet gemeten; meten is stap 0. Negen oorzaken,
elk met regelnummers en wat het wordt. De drie die het meest doen:

1. **Sluiten is altijd een knip.** egui vervaagt een `Area` wel in (0,2 s) maar
   nooit uit, en het repo tekent zelf geen enkel vertrek; alleen het paneel
   schuift (egui's `show_collapsible`, de standaard 0,2 s).
2. **Een stil scherm halveert het tempo.** `frame_pacing` zakt na 2 s zonder
   camera, toets of vinger naar de helft, en een lopende UI-animatie telt niet
   als bezig: een kaartje dat opkomt over een stilstaande kaart loopt op 30 fps.
3. **Tijd-animaties bevriezen op stille frames.** De splash-fade, de inlogkaart,
   de chat-fades en de GPS-zoekveeg lezen de klok en vragen geen repaint.

Verder: `predicted_dt` staat nooit (egui rekent met 1/60 s, een telefoon haalt
25–40 ms), JPEG-decode en opslag op de UI-thread, negen eigen paginastapels,
vegen als drempel in plaats van beweging, een toetsenbord dat bedekt en niets
schuift, en de egui-pass als tweede volledige pass over het eindbeeld.
(Oorzaak 9 is intussen gemeten: sessie s663 ziet op mac-2 ~0,3 ms per frame
voor `egui_pass`, 2026-11-10. Samenvoegen met upscaling is daarmee geen
prioriteit; de telefoon moet nog gemeten worden.)

## Wat er gebouwd is

- **Stap 1** (51a3b6d0): `systems::motion`, de instelling Motion, komen en
  gaan voor de kaartjes en de ring, 360 / 240 ms voor de panelen, de
  druk-schaal op ronde knoppen, de frame-pacing die een lopende animatie als
  bezig telt, `predicted_dt`, en de vier klok-fades die om hertekening vragen.
  De dock en het menu wachten op sessie s651, die beide hertekent.
- **Stap 2** (8fa8164d): `motion::grabber` / `motion::follow`: de plek- en
  gebouwkaart volgen de vinger en rusten op twee en drie hoogten; de
  routekaart en het lensblad nemen dezelfde grabber.
- **Stap 3** (67a14abc, 3a9a357a): `motion::page_turn` in Settings,
  Communities, Calendar, Admin en Contacts: een pagina komt van rechts over
  een ouder die een derde wijkt en donkerder wordt, en gaat zo terug. Geen
  gedeelde `PageStack` (het ontwerp noemde er een): elk paneel houdt zijn
  eigen paginastaat en geeft een sleutel en een diepte mee; dat was genoeg
  voor de overgang en raakte de dismiss-ladder niet. Chats en You wachten op
  s651.
- **Stap 5, deels** (82306ef2, en de regel erna): het toetsenbord komt omhoog
  van zijn rand (alleen het beeld; de tikvlakken liggen meteen waar ze
  rusten), een nieuwe marker popt, een nieuw bericht stijgt 6 pt. Nog niet:
  rijen die glijden, schimrijen bij laden, licht-naar-donker in 600 ms.
- **Open**: de dock en het menu (na s651), stap 4 is s651's werk, stap 0 en
  6 (meten op de OnePlus 15) zodra een build met dit werk op het toestel
  staat.

Wat het wordt: `theme::motion` als de ene plek voor tijd en curve (zoals `theme`
dat voor kleur is), een dunne laag over `ctx.animate_*` zodat egui zelf weet
wanneer het moet hertekenen; `motion::presence` voor komen én gaan;
`motion::Sheet` met drie rusten en de vinger erin; één `PageStack`; de
stille-frame-overslag en `WorldAnchored` blijven.

Zes stappen, elk landt alleen en laat iets zien: (1) motion, komen en gaan,
druk; (2) het kaartje; (3) pagina's; (4) de twee ruimtes, dat is `redesign/` v2;
(5) markers, lijsten, toetsenbord, licht naar donker; (6) meten, en de pass.

![vloeiend](v1-vloeiend.png)

## Zo is het nu

Gefotografeerd van `main` (035fe5af) met `docs/phone-ui-shots.toml` op schaal
1,5, op `vps-1` (CPU-renderer: de kaart eronder zegt niets over kleur of
licht; de UI is egui en klopt wel). Dit is de stand mét het thema van
2026-11-10, dus de foto's in `redesign/` (van 0a7e2a97) zijn ouder.

![zo is het nu](today.jpg)

Nog niets besloten.
