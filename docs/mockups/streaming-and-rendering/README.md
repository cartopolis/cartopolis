---
topic: streaming-and-rendering
triggers: ["render study", "renderer architecture", "knipperen", "flikkeren", "hapert", "zoom is slow", "zoom stutters", "traffic hangs", "verkeer hangt", "metaverse", "entity count", MAX_DESPAWNS_PER_FRAME, "despawn budget", "time budget", "one tree", GLOBE_DRAW_ALT, GLOBE_SHOW_ALT, "detail band", "retear for detail", DriveNetwork, holds, building_fade.rs, "frame budget", "pipeline warmup", "shadow cache", "vertex displacement"]
updated: 2026-11-22
---

# Streaming en rendering: waar het knippert, hangt en hapert, en wat er daarna kan

Een studie, geen code. Gevraagd 2026-11-22: "Als ik in- en uitzoom zie ik af en
toe nog geknipper, verkeer laadt moeilijk en blijft wat hangen en het zoomen is
nog niet helemaal soepel. Doe een uitgebreid onderzoek naar de renderer en
architectuur en wees creatief in het vinden van oplossingen. Vorige agents hebben
ook het een en ander kunnen vinden, maar we zitten nu een beetje aan de top van
mogelijkheden lijkt het. Maak een studie zodat we de metaverse verder kunnen
bouwen."

Het bouwt op `docs/notes/render-performance-survey.md` (tien lenzen, de
negatieve resultaten), `upload-bursts.md`, `near-ground-layers-at-altitude.md`,
`level-of-detail-tile-tree.md` (de ene-boom-richting van 2026-09-23),
`tile-queuing.md`, `streaming-order-and-residency.md` en `ambient-traffic.md`.
Wat die noteren wordt hier niet herhaald; wat hier staat is wat zij niet zagen,
omdat elk ervan een *stilstaand* beeld meet en de drie klachten alle drie over
*beweging* gaan.

Alles hieronder is op 2026-11-22 gemeten of uit `main` (d377b421) gelezen, met
regelnummers. §1 tot §6 beschrijven de toestand vóór de wijzigingen van die
middag; §7a zegt wat daarvan gebouwd is en wat het mat.

## 0. Samenvatting

De drie klachten hebben één gemeenschappelijke wortel en een paar eigen
oorzaken.

**De wortel: de wereld streamt in hoogtebanden, en een band wisselt door alles
weg te gooien.** Een zoom van 40 km naar de straat kruist zes harde drempels
(50 km, 10 km, 3 km, 2 km, 500 m, 250 m); bij vier ervan wordt oude inhoud
*eerst* verwijderd en pas seconden later vervangen. De grondring (`map_geometry`)
is het ergst: elke bandwissel gooit alle negen cellen weg, inclusief het
wegennet waar het verkeer op rijdt, en bouwt ze opnieuw. Dat is tegelijk het
knipperen (de straat verdwijnt tot een vage kaart en komt terug), het verkeer
dat "opnieuw moet laden" (zijn wegen zijn weg; het druppelt daarna met 24 per
frame terug, buiten beeld), en een deel van het haperen (de rebuild is
secondenlang werk en de oude cel verdwijnt met 4 entiteiten per frame).

**Gemeten op mac-2** (Apple M5 Pro VM, ontwikkelprofiel, 1280×800, warme cache;
richting is betrouwbaar, milliseconden zijn van deze machine):

| zoom | gemiddeld frame | slechtste frame | grondring weggegooid | alles rustig na aankomst |
|---|---|---|---|---|
| 40 km → 150 m in 8 s | 71 ms (14 fps) | 964 ms | 1× (9 cellen) + 1× bij aankomst | 28,5 s |
| 40 km → 40 m in 9 s | 46 ms | 384 ms | 1× (9 cellen) | 25,2 s |
| 150 m → 200 km in 10 s | 57 ms | 134 ms | 2× (9 + 0 cellen) | 25,4 s |

Een film van de afdaling (15 beelden/s) laat geen enkele eenmalige "pop" van
één frame zien in 274 beeldparen. Wat het wel laat zien, en wat een mens als
knipperen leest: twee *aanhoudende* omslagen. Bij ~2 km slaat de hele kaart in
één beeldinterval om van helder naar grijs-wazig (de globe-overdracht,
`GLOBE_DRAW_ALT`, grootste sprong van de film: 18,8 % van de pixels); bij 243 m
verdwijnt het straatdetail dat al stond en blijft seconden de bruine basiskaart
over (de detailwissel, `surface ring retear for detail`).

**Eigen oorzaken per klacht**, kort:

- *Knipperen*: de bandwissels zonder vasthouden (§3), de ontbrekende hysterese
  op 50 km en 2 km, het reliëf dat in hoogte springt vóór zijn mesh er is, en
  een bug waardoor het grove gebouwblok al verdwijnt bij het *eerste* stuk van
  de echte LoD2.2-schil (telefoon en web).
- *Verkeer*: zijn wegennet leeft in de grondcel en sterft met elke bandwissel;
  het centreert op de camera en niet op het kijkpunt (in Earth-modus ligt de
  schijf van 260 m onder of achter de camera); het keert terug met 24 per frame
  en na de eerste helft alleen buiten beeld; het bevriest zichtbaar als de
  camera boven een cel staat die nog niet geland is (§4).
- *Haperen*: op desktop is er géén byteplafond per frame (25 MB in één frame
  midden in de zoom, 600 MB bij de start); een grondcel is ~485 mesh-entiteiten
  en negen cellen ~4 300 schaduwwerpers, terwijl de renderdraad per entiteit
  betaalt; weggooien gaat met 4 per frame, zodat de straatring van 200 km
  hoogte nog 30 s lang getekend wordt; vijf materialen worden élk frame opnieuw
  voorbereid door een `get_mut` vóór de vergelijking; pipelines compileren bij
  het eerste gebruik; de zoom-glide is lineair in afstand en de desktop pacet
  met een `sleep` naast de verversing (§5).

**Wat te doen**, in volgorde van opbrengst per moeite (§7 heeft de details):

1. Houd vast tot het nieuwe staat: geen `clear()` bij een bandwissel, de oude
   ring blijft getekend tot de nieuwe cel geland is. Dit raakt alle drie de
   klachten.
2. Haal het verkeersnet uit de grondcel: een lichte eigen job op de tegel, een
   eigen ring, centrering op het kijkpunt, plaatsing in beeld bij aankomst.
3. Een tijdsbudget per frame op de hoofddraad naast het byteboek: landen,
   spawnen én despawnen tot het budget op is, in milliseconden, niet in
   aantallen; en een byteplafond op desktop.
4. Minder entiteiten per cel (485 → tientallen): één mesh per cel per
   materiaal; het terrein uit de vertices (verplaatsing in de vertex-shader),
   waarmee de hele klasse "rebuild bij terreinrevisie" verdwijnt.
5. Hysterese op 50 km en 2 km, en een opwarmronde voor alle pipelines.
6. De grote stap: één boom voor alles (§6.1), waarin banden niet meer bestaan.

## 1. Hoe het gemeten is, en wat dat waard is

- **Machine**: `mac-2`, Apple M5 Pro in een midas-VM met paravirtuele GPU, gedeeld
  met een tweede VM. Ontwikkelprofiel (debug-asserties aan, eigen crates op
  `opt-level 1`), desktop-rung, `CARTO_FPS_CAP=0`, 1280×800, warme cache,
  één vlucht per scenario. Frame-milliseconden zijn hier een eigenschap van de
  doos en het profiel; de *verhoudingen* binnen één avond, de tellingen
  (entiteiten, draws, bytes, retears, seconden tot rustig) en de volgorde van
  gebeurtenissen in het log zijn betrouwbaar.
- **Scenario's**: `docs/perf.toml`'s `descent` (40 km → 150 m in 8 s),
  `descent-low` (→ 40 m in 9 s) en `climb` (150 m → 200 km in 10 s), via
  `cargo run -p cartopolis -- --perf --only descent,climb,descent-low`. De zoom
  wordt gestuurd als een gelijkmatig gedraaid wiel en wacht nergens op.
- **Film**: `CARTO_PERF_FILM=15` op `descent-low`: 275 van 353 beeldslots
  (de gaten zijn trage frames; het filmen zelf vertraagt, dus de frametijden van
  die run tellen niet). Per opeenvolgend paar is het gemiddelde absolute
  pixelverschil berekend (`compare -metric MAE`), en per drietal of een beeld
  méér verschilt van beide buren dan die van elkaar — de definitie van een pop
  van één frame.
- **Census**: de `render cost` / `mesh owners`-regels die de client elke 5 s
  logt (`render_census`), met entiteiten per markercomponent.
- **Niet gemeten**: de telefoon (OnePlus 15; niet verbonden tijdens deze
  sessie) en de browser. De telefoon heeft een eigen rung met plafonds die de
  desktop niet heeft, dus een deel van §5 geldt daar minder, en een deel (het
  blok-bug in §3.6) juist alleen daar.

## 2. De architectuur op één blad

Zoals het nu staat. Elke rij is een eigen streamer met een eigen ring, eigen
drempels en eigen plafonds; er is geen gedeelde planner behalve de
werkerwachtrij (`platform::workers`, geordend op urgentie) en het byteboek
(`stream_ledger`, alleen waar een rung een plafond heeft).

| laag | eenheid en ring | hoogte aan / uit | wat een wissel doet | per frame |
|---|---|---|---|---|
| Basiskaart (`map_stream`) | beeldfout-gestuurde tegelboom, ≤424 tegels | tot 50 km fijn; erboven alleen de wortels (`GLOBE_SHOW_ALT`, geen hysterese) | ouder blijft tot vier kinderen klaar zijn (goed); boven 50 km alles fijns weg, eronder ~340 tegels in één frame terug | 8 uploads, 12 despawns, 2 reliëfmeshes, 16 krommingsmeshes |
| Grondcellen (`map_geometry`) | 3×3 z14-cellen (~1,5 km) rond het kijkpunt | vlakke grond 250 m aan / 500 m uit; meubilair tot 450/500 m; bomen tot ~2/2,5 km | **hele ring `clear()`**, inclusief wegen, labels, plaatsen, gras, tunnels; nieuwe cellen pas als de zoom vertraagt (`SWEEP_RATE`) | 2 cellen landen, 4 entiteiten despawnen |
| LoD2.2 (`lod22`) | 5×5 cellen, binnen 1 cel schillen, daarbuiten blokken | tot 10 km (`BUILDINGS_TOP_ALT`, harde stap) | per cel; bij terreinrevisie eerst weg, dan opnieuw | 1 cel landen, 2 despawnen |
| OSM-gebouwen (`osm_buildings`) | ver patch + blokken | detail bevroren boven 3 km | patch-wissel overlapt oud en nieuw | 6 despawns |
| Globe (`globe`) | quadtree, ≤320 bladeren | getekend vanaf 2 km (`GLOBE_DRAW_ALT`, geen hysterese) | oud blijft tot alles wat eroverheen ligt geladen is (goed) | selectie elke 0,25 s, 8 builds |
| Terrein (`terrain`, `terrain_tiles`) | veld van 6,4/9,6 km, volgt de camera per 1,6 km | fijne laag bevroren boven 800 m | elke cel met gewijzigde hoogte-digest wordt herbouwd (eerst weg) | 1 bake |
| Straatdetail, meubilair, bomen | per cel, in de grondcel-job | fades 150–450 m, 2–2,5 km | mee met de grondring | — |
| Verkeer (`traffic`) | vaste pools (180 auto's, 220 voetgangers, 70 fietsers, 12 fatbikes), schijf van 260 m **rond de camera** | verborgen boven 700 m | leeft op `DriveNetwork`, dat met de grondring wordt gewist | 24 plaatsingen, Rapier 1–8 substappen |
| Labels, plaatsnamen, pins | egui-overlay | banden 60/150/400/800/1 200 m; namen 1,5–250 km | — | 4 rasters; elk frame een volledige egui-hertekening tijdens de zoom |

De hoofddraad landt resultaten, spawnt en despawnt; de renderdraad extraheert
elke entiteit elk frame. `render-performance-survey.md` mat op de Deck ~4,2 µs
per entiteit per frame aan renderdraad-CPU, onafhankelijk van pixels, draws of
schaduwen. Dat getal is de sleutel tot §5.2.

## 3. Klacht 1: knipperen bij in- en uitzoomen

### 3.1 Wat de film laat zien

![de detailwissel op 243 m](descent-243m-retear.jpg)

Zeven beelden rond 250 m (slots 110, 111, 114, 117, 118, 119, 121; de gaten
zijn trage frames van 485 en 529 ms). Links staat straatdetail — grijze
rijbanen, stoepranden — van twee cellen die na de wissel op 674 m (bomen aan)
net geland waren. Dan de wissel op 243 m (vlakke grond en meubilair aan):
`surfaces.clear()`, en alleen de bruine basiskaart blijft over, bomen en
gebouwen staan erop. Zo blijft het tot 5,2 s na aankomst (`view_ground_s`),
en tot 18–25 s voor de hele ring. Dit is precies wat "het knippert als ik
inzoom" beschrijft: detail dat er was, weg is, en terugkomt.

![de omslag bij 2 km](descent-2km-step.jpg)

Vier beelden rond 2–3 km (slots 54, 55, 58, 60). Tussen 54 en 55 — één
beeldinterval — wordt de kaart grijs en wazig en verschijnen tientallen
plaatsnamen. Dat is `GLOBE_DRAW_ALT` (`globe.rs:1043`): de globe wordt
getekend, de atmosfeer wisselt van `LookupTexture` naar `Raymarched` met
16 → 8 samples (`globe.rs:1435-1446`), het nabije vlak springt van 0,1 m naar
`alt × 0,004` (`free_camera.rs:1697-1712`). Geen hysterese, geen overgang.

Geen enkel beeld verschilt méér van beide buren dan die van elkaar: er zijn
**geen pops van één frame** in deze run. Op 15 beelden/s met gaten zou een
pop van één frame op 60 fps driekwart van de keren gemist worden; dit zegt
dus "niet gevonden", niet "bestaat niet". De aanhoudende omslagen zijn wél
hard bewijs.

### 3.2 De oorzaken, gerangschikt

Wat ze gemeen hebben: **oude inhoud wordt weggegooid voordat de nieuwe
getekend wordt.** De tegelboom en de globe-quadtree doen dat goed (ouder
blijft tot de kinderen er zijn); de vectorgrond, de LoD2.2-schillen en de
hoogtedrempels doen het niet.

1. **De grondring bij een bandwissel** (`map_geometry.rs:3555-3578`):
   `surfaces.clear()` leegt ook `complete` (3258), waardoor de basiskaart
   eronder in hetzelfde frame weer zichtbaar wordt terwijl de oude platen er
   nog 3 cm boven liggen — het bekende bandenspel (`map_stream.rs:1807-1813`)
   — en de oude entiteiten met 4 per frame verdwijnen (162). Tijdens het vegen
   wordt niets nieuws gebouwd (`SWEEP_RATE`, 1078; `held_for_sweep`, 1389),
   dus het gat duurt minstens tot de zoom stopt plus een celbouw. De drempels
   zelf hebben hysterese (250/500, ×1,25); het vasthouden ontbreekt.
2. **Reliëf-achterstand** (`map_stream.rs:1189, 1270-1278`): een tegel krijgt
   zijn hoogte meteen (`want_y` snapt), maar de bijbehorende mesh komt met 2 per
   frame; een nieuw zichtbare fijne tegel is een vlak kwadraat op `level_y` tot
   zijn reliëfmesh er is, terwijl zijn ouder (die wél reliëf had) al verborgen
   is. De grond springt onder gebouwen en wegen. Een koude clipmap is ~83
   frames van gevormd (1078-1084).
3. **50 km zonder hysterese**: `in_space` (`globe.rs:563-565`) is een harde
   boolean; alles met `HideInSpace` en alle tegels, gebouwen en grond wisselen
   in één frame (`data_layers.rs:1646-1675`), de tegelboom gaat boven 50 km
   naar z10 (`map_stream.rs:1920-1924`) en spawnt eronder ~340 tegels in één
   frame terug. De camera zelf heeft wel hysterese (55/48 km); de wereld niet.
   Een zoom die rond 50 km hangt, trasht de hele clipmap.
4. **2 km zonder hysterese** (hierboven, de film).
5. **Terreinrevisie: eerst weg, dan opnieuw.** `retire_cell`
   (`map_geometry.rs:3314-3334`) en `lod22::rebuild_cell` (1060-1087) halen de
   oude geometrie weg en bouwen daarna; voor 3DBAG-cellen komt er geen blok
   terug (`osm_buildings.rs:1649-1656`). Vuurt elke 1,6 km over gegradeerde
   grond (`terrain.rs:357`) en bij Far→Near-promotie (`lod22.rs:1380-1392`).
6. **Het blok verdwijnt bij het eerste stuk schil** (bug): `lod22.holds()` is
   `cells.contains_key` (799-801) en de cel wordt ingeschreven bij het eerste
   toegelaten stuk (1842), terwijl het commentaar op 1856-1861 "pas als alle
   stukken staan" belooft; `osm_buildings.rs:1908-1912` ruimt het blok op zodra
   `holds()` waar is. Alleen waar het byteboek een cel in stukken over frames
   verdeelt: telefoon en browser. Daar valt een gebouw dus even weg terwijl
   zijn schil in plakken binnenkomt.
7. **Eén frame achterlopende oorsprong**: `track_grid_origin`
   (`ground_detail.rs:127-138`) draait in de Stream-fase, de camera wisselt van
   10 km-cel in de Camera-fase erna; de lod-blend-ring, het gronddetail en de
   gebouwfade tekenen één frame met de oude oorsprong, een hele cel ernaast.
8. **Herselectie-bijwerkingen in de tegelboom**: de lod-blend-ring wordt bij
   elke herselectie opnieuw afgeleid (`map_stream.rs:1005, 1015-1022`), de
   geschilderde tweeling bestaat alleen op z13 (`tile_loader.rs:272`), en de
   cap- en frustumtest hebben geen hysterese (1658, 1677).

Kleiner: `BUILDINGS_TOP_ALT` (10 km) is bewust een stap; een her-anker laat
oude tegels met 12 per frame verdwijnen terwijl de nieuwe piramide laadt;
stoeprandschaduwen schakelen zonder hysterese (`kerb_shadows.rs:124-137`).

### 3.3 Wat het bestaande instrument niet ziet

`--flicker` fotografeert een *stilstaande, uitgezette* scène tweemaal 5 cm uit
elkaar en telt gewisselde pixels: alleen z-gevechten. Alles in deze paragraaf
is tijdsafhankelijk en valt erbuiten. De filmmethode van §1 (verschil per paar,
pop per drietal) is de maat die het wél ziet; §7.8 stelt voor haar in de suite
op te nemen.

## 4. Klacht 2: verkeer laadt moeilijk en blijft hangen

### 4.1 De keten, van bron tot scherm

```
Shortbread-tegel ──► "surfaces"-werkerjob (zware baan, NEAR)
   extract_drive_lines + keep_walkers_off          map_geometry.rs:2945, 2626
        │  wacht op: dekking ≤3 s · eerst de cellen in beeld · niets tijdens het vegen
        ▼
 landen: 2 cellen/frame, en 0 zolang de vorige cel nog meshes
         door stream_ledger::admit moet                 :3800, :3222
        │  weggegooid als gebouwd op een oude terreinrevisie   :3833
        ▼
 DriveNetwork.insert_cell (revision++)                    :3868
        │  ◄── gewist door: bandwissel (250/450/500/2000/2500 m),
        │       terrein/BGT-retear, her-anker, ring verlaten    :3565-3578
        ▼
 close_car_free_lines → ride_the_tracks: hele ring, hoofddraad, per revisie
        ▼
 drive_traffic: verborgen >700 m · net.near() ×4 rond de CAMERA
   · ≤24 plaatsingen/frame over alle pools · na de helft alleen buiten beeld
   · voetgangers/fietsers onzichtbaar tot hun crowd-body (FAR-werker) er is
        ▼
 collide_traffic: Rapier, 1–8 substappen → Transform/GridCell → render
```

Zestien wachtpunten; de simulator en het netwerk spelen geen rol — het verkeer
wordt per client verzonnen en er gaat niets over de draad.

### 4.2 De oorzaken, gerangschikt

1. **Een zoom wist het wegennet.** `stores.roads.clear()` zit in de
   bandwissel (`map_geometry.rs:3565-3578`), hoewel rijlijnen niets van bomen,
   meubilair of vlakke grond afhangen. Uitzoomen voorbij 500 m haalt alle
   verkeer weg, terwijl het tot 700 m zichtbaar hoort te zijn; terug onder
   450 m en 250 m wist het nog eens één of twee keer. Daarna wacht het verkeer
   op een complete celbouw (3,6 s koud per cel; de grond in beeld na 5–11 s,
   de ring na 18–28 s in de meting van §0). Dit is de klacht "laadt moeilijk".
2. **Het vult daarna druppelsgewijs.** `PLACE_PER_FRAME = 24`
   (`traffic.rs:1144`), gedeeld over alle 482 agenten; na een halve pool mag
   een agent alleen nog achter de camera of voorbij 0,8× de straal verschijnen
   (`:3209-3221`, `unseen_m`); elke plaatsing moet `physics.is_clear` halen.
   De straat in beeld vult tot de helft en de rest sijpelt van buiten binnen.
   Bewust zo gebouwd tegen het inpoppen (`ambient-traffic.md`); het ontbreekt
   aan een uitzondering voor "de pool is net gewist": dán hoort hij in beeld
   te vullen, zoals een koude start doet.
3. **Centrering op de camera, niet op het kijkpunt.** `cam =
   anchored_xz(cam_cell, cam_tf.translation)` (`:2261`), terwijl de kaartring
   in Earth-modus het orbit-focus neemt (`free_camera.rs:1465`) — de regel uit
   CLAUDE.md. Schuin uitgezoomd ligt de schijf van 260 m onder of achter de
   camera en zijn de straten in beeld leeg; elke zoomstap verschuift de camera
   en recyclet agenten over de 390 m-grens, terug door oorzaak 2.
4. **Zichtbare agenten bevriezen.** Is het net niet leeg maar ligt geen lijn
   binnen de straal, dan keert `drive_traffic` terug zonder te verbergen of te
   bewegen (`:2308-2310`): boven een nog niet gelande cel, water of een park
   staan de geplaatste agenten stil waar ze stonden. Dit is "blijft hangen".
5. **Trage frames maken het verkeer schokkerig, en het verkeer maakt de
   frames trager.** Rapier neemt tot 8 substappen van 1/30 s bij een groot
   `dt` (`traffic_physics.rs:130-131, 655`) over 482 lichamen; `net.near` loopt
   vier keer per frame over élk punt van élke lijn in de 3×3 ring zonder
   boxtest (`traffic.rs:670-684`, terwijl `drivable_near` die wel heeft);
   `ride_the_tracks` draait over de hele ring bij elke celmutatie (`:777-778`).
   Bevy's virtuele klok klemt bovendien `dt` op 0,25 s, dus in een frame van
   een seconde legt het verkeer een kwart seconde af: het "hangt".
6. **Live bussen staan stil tussen twee fixes** (alleen met Live vehicles
   aan): de interpolatie klemt na 15 s (`transit_vehicles.rs:245-247`), `t0`
   is het moment van inlezen, de volgende poll begint 15 s na de start van de
   vorige en de parse draait op FAR-urgentie achter de celjobs van de zoom.
7. **Geparkeerde auto's en voetgangers komen laat**: de eerste met de
   meubilair-retear onder 450 m, de tweede pas als hun body (FAR-werker) is
   gebouwd — eenmalig per start.

## 5. Klacht 3: zoomen is niet soepel

### 5.1 Wat er gebeurde tijdens de afdaling van 40 km naar 150 m

| moment | frame | wat het log erbij zegt |
|---|---|---|
| start, 43 km | 1 076 ms | upload-piek **613 MB** in één frame bij de koude vulling: desktop heeft geen plafond |
| 1 913 m | — | `surface ring retear for detail`, 9 cellen (bomen aan) |
| 1 710 m | 447 ms | meshes 998 |
| 1 313 m | 964 ms | **25 MB** upload in dit ene frame; meshes 1 001 → 1 279 |
| 681 m | 299 ms | |
| 397 m | 483 ms | |
| 141 m (aankomst) | — | tweede retear (vlakke grond + meubilair aan) |
| +11 s | 336 ms | meshes 2 441; grond in beeld compleet |
| +12 s | 385 ms | meshes 2 993 |
| +28,5 s | | alles rustig; `street_detail` als laatste |

Gemiddeld 71 ms per frame tijdens de 8 s zoom, tegen 19 ms op de plek ervoor.
Entiteiten: 3 946 bij de start, 4 352 tijdens de zoom, **9 198** op de straat.

### 5.2 De oorzaken, gerangschikt

1. **Geen byteplafond op desktop.** `upload_bytes_per_frame: None` op de
   desktop- en balanced-rung (`quality.rs:645, 743`); `admit` zegt dan meteen ja
   (`stream_ledger.rs:78-80`). Alleen de aantallen-caps gelden: 8 tegels met
   tweeling, 8 globe-tegels, 1 LoD2.2-cel (alle stukken tegelijk), de OSM-patch
   en de hele wachtrij van `map_geometry` — 2 cellen per frame (3800-3805), en
   een straatcel is ~170 MB (`near-ground-layers-at-altitude.md`). De 25 MB- en
   613 MB-frames hierboven zijn daarvan het bewijs. De telefoon heeft 4 MB, de
   browser 16 MB; de desktop heeft niets omdat hij nooit een device verloor —
   maar een frame van 964 ms is óók een verlies.
2. **Een grondcel is ~485 mesh-entiteiten, en de renderdraad betaalt per
   entiteit.** Census op 299 m: `MapSurface = 756 MB / 4 361 entiteiten` over 9
   cellen, 4 387 draws, **4 321 schaduwwerpers**. Met ~4 µs per entiteit per
   frame (de Deck-meting) is dat alleen al ~17 ms renderdraad, drie
   schaduwcascades nog daargelaten. De tessellators geven "één mesh per
   oppervlak" terug (`map_geometry.rs:1527-1534`) en de stukken zijn gemiddeld
   173 KB — ver onder het 4 MB-plafond; het aantal komt van de indeling per
   oppervlak, groep en rung, niet van het byteplafond. De GPU-kant is
   onverschillig (batching); de CPU-kant van de renderdraad niet
   (`write_binned_instance_buffers`, extractie, zichtbaarheid: alle per
   entiteit).
3. **Weggooien gaat met 4 per frame, en wat wacht wordt getekend.** De climb
   laat het zien: op 200 km hoogte, 30 s na vertrek, staan er nog
   `MapSurface = 2 157 entiteiten`, `draws 2 879`, `shadow_casters 2 531`;
   de reeks 4 041 → 3 765 → 3 057 → 2 157 → 1 241 → 277 is precies 4 per frame
   bij 9 fps. De despawn-caps zijn geschreven tegen de device-verlies-burst van
   *uploads* (`MAX_DESPAWNS_PER_FRAME`, `map_geometry.rs:162`; `lod22.rs:92`:
   2; tegels 12), maar een despawn uploadt niets: hij kost juist zolang hij
   *niet* gebeurt, elk frame opnieuw, per entiteit. Met 485 entiteiten per cel
   is de cap de verkeerde eenheid.
4. **Vijf materialen worden elk frame opnieuw voorbereid.**
   `material_modified_per_frame` leest 6,3–6,8 in elk venster waar verre
   gebouwen staan (climb load 6,28; descent-low descent 6,83) en 0,02 waar ze
   weg zijn. `update_building_fade` (`building_fade.rs:153-165`) neemt
   `get_mut` *vóór* de vergelijking; Bevy markeert het asset dan al gewijzigd.
   Het commentaar erbij beweert het tegendeel. Dezelfde regel als
   `render-performance-survey.md`'s "lees met `get`, vergelijk, dan `get_mut`".
5. **Camera-gestuurde materiaalschrijfacties.** `refresh_tile_lod_blend`
   draait zodra de gewenste set, de oorsprong of het tegelaantal beweegt
   (`map_stream.rs:1005`), en de pose-sleutel beweegt per 4,4 % hoogte
   (1963-1974) — dus bijna elk zoomframe; `lod` hangt af van het centrum en
   het bereik van elk niveau, die met de camera meebewegen (1047-1061). Een
   horizontale celgrens van `big_space` (10 km) herschrijft élk grondmateriaal
   (`ground_detail.rs:85-95`); in de ruimte kruist een radiale zoom die grens
   bijna elk frame. De maat: ~8 ms per 65 materialen op een M5 Pro
   (`street_labels.rs:710-715`).
6. **Pipelines compileren bij eerste gebruik.** `depth_bias` zit in de
   pipeline-sleutel (`bevy_pbr pbr_material.rs:1278`): elf globe-niveaus,
   zestien `GroundLayer`-rungen, elke `SurfaceGroup`, plus de
   atmosfeerwissel op 2 km. 340–380 ms per frame op de Deck als de straat-
   groepen voor het eerst verschijnen (`near-ground-layers-at-altitude.md`);
   de twee frames van 336/385 ms bij aankomst (§5.1) hebben die signatuur.
7. **De glide en de pacing.** De Earth-zoom glijdt lineair in afstand
   (`free_camera.rs:3299-3302`: `old + (target − old) × (1 − e^(−6·dt))`),
   terwijl de attach-modus ernaast in log-ruimte glijdt en uitlegt waarom
   (2632-2637): lineair legt een uitzoom bijna alles in het eerste frame af en
   elk wielklikje is een snelheidsstap. Op desktop is `PresentMode::AutoNoVsync`
   (`lib.rs:698-702`) met een `sleep`-begrenzer op 60 fps
   (`frame_pacing.rs:607-636`): een klok los van de verversing, op een 120/144 Hz
   paneel afwisselend 2 en 3 ververs-perioden per frame — judder bij elke
   gelijkmatige beweging, los van alle streaming.
8. **Herafleiding per frame in de tegelboom**: `select_tiles` (HashMaps, heap,
   sort over ≤424), `reprice_queue`, de 424-lus en de stale-scan
   (`map_stream.rs:1963-2265`), elk zoomframe; de globe-stalefilter is
   O(geladen × gewenst) elke 0,25 s (`globe.rs:2306-2316`) en bouwt tot 8
   bolpatches per frame op de hoofddraad.
9. **egui elk frame van 1,5 tot 250 km**: `place_names` projecteert elke
   plaats in ≤9×9 tegels met een O(n²) overlaptest, en `WorldAnchored` vraagt
   een volledige hertekening zodra iets beweegt — dus elk zoomframe.
10. **Her-anker in een pauze**: `recentre_anchor` (`geo_nav.rs:2072-2099`)
    herbouwt de hele wereld als het focus >10 km van het anker ligt, boven
    500 m, na 0,4 s "stil" — een adempauze tussen twee wielstoten telt als stil.
11. **Werkers zijn overtekend op desktop**: `cores − 1` tegeldraden op normale
    prioriteit naast Bevy's io (16–24), async-compute (45 %) en compute-pools
    (`lib.rs:241-245, 596-666`); alleen Android verlaagt hun prioriteit
    (`workers.rs:99-110`).

Kleiner: de 5 s-census loopt elke 5 s over elke mesh en `log_mesh_owners`
neemt `&World` (een barrière elk frame); stoeprandschaduwen wisselen in één
frame van archetype; de projectie wordt in de band 2–50 km elk frame tweemaal
geschreven; `ScatteringMedium` wordt 30–40 keer herbouwd over een klim.

## 6. De grotere stappen: waar de metaverse-schaal vandaan komt

De paragrafen hiervoor zijn reparaties. Dit zijn de verbouwingen, elk met wat
ze kosten, wat ze teruggeven en waar ze op rusten. Ze zijn creatief bedoeld en
in volgorde van verwachte waarde gezet; §7 trekt ze samen tot een pad.

### 6.1 Eén boom voor alles, en geen banden meer

`level-of-detail-tile-tree.md` sprak dit op 2026-09-23 af en bouwde stap 1
(tegels), 2 (terrein) en 4 (gebouwblokken); stap 3 (de globe erin, het anker
eruit) en de grondcellen zelf staan nog buiten de boom. Alles in §3 en §4.1
komt uit die rest: zes hoogtedrempels, elk een eigen "alles weg, alles terug".

Wat het wordt: elke laag kiest per *tegel* zijn niveau op beeldfout, nooit op
camerahoogte; een tegel wisselt van niveau pas als zijn vervanger geüpload en
getekend is (de regel die `map_stream` en `globe` al hebben, voor de grond,
de gebouwen en het meubilair ook); een cel is "een tegel met meer lagen", en
een laag die op deze afstand niets toevoegt (meubilair boven 450 m) is een
laag die niet geselecteerd wordt — geen band, geen retear, geen `clear()`.
De vier retears van een afdaling worden nul, het verkeer verliest nooit zijn
wegen, en het knipperen van §3.2.1 heeft geen mechanisme meer.

Kosten: de grondcel wordt een tegel met een ouder, dus de cel-job moet per
niveau kunnen leveren (z14 zoals nu; z15/z16 voor de straat zou de 170 MB per
cel in vieren of zestienen delen — en dat is óók de weg naar minder residency
op straatniveau, waar het 320 MB-plafond nu maar één cel toelaat). Het anker
blijft voorlopig (het "~35 lezers in 17 bestanden"-argument uit de notitie
staat nog).

### 6.2 Het terrein uit de vertices: verplaatsing in de vertex-shader

Elke rebuild-op-terreinrevisie — grondcellen, LoD2.2, blokken, tegels — bestaat
omdat de hoogte in de vertex gebakken wordt. `one-pass-cells.md` noemde de
splitsing al als "de richting die het echt één pass maakt": alles wat duur is
(parse, earcut, wegen snijden, paving, kust, gras, bomen) hangt niet van de
hoogte af; alleen de Y en de onderverdeling.

Wat het wordt: het hoogteveld als textuur (het is al een 513²-rooster per
tegel, `terrain_tile::DENSE_GRID`), gesampled in de vertex-shader van alles wat
op de grond staat; een cel wordt eenmaal getessellateerd op een vaste
onderverdeling (de refinement is dan een eigenschap van de mesh, niet van de
hoogten) en krijgt zijn reliëf per frame van de GPU. Een nieuw veld is een
textuur-upload van ~0,5 MB per tegel en nul rebuilds. Gebouwen krijgen één
"voetanker" per instantie of per vertex (de hoogte van het voetafdrukpunt),
zodat een schil als geheel mee stijgt en niet vervormt. De `TerrainField::at`
op de CPU blijft voor collision, verkeer en avatar.

Dit verwijdert §3.2.2, §3.2.5 en de "built on an old terrain revision"-weggooi
uit §4.1 in één beweging, maakt de atlas-voorgesneden grond (`pre-cut-ground.md`)
tot een *complete* vooraf gebouwde cel (want het enige dat de client nog deed,
de drape, is weg), en opent de deur naar §6.6. Kosten: een shader per grondgroep
(de WESL-regels), de kustveld- en watervlakken die een hoogte per cel nemen
(`rijkswaterstaat-water-levels.md` legt uit hoe strak die stapel is), en de
bruggen/tunnels waarvan de dekken juist *niet* het veld volgen — die houden hun
eigen hoogte als attribuut. De 8 mm-coplanaire stapel (`the-ground-stack.md`)
blijft in de shader even strak, want alle lagen samplen hetzelfde veld.

### 6.3 Minder entiteiten, niet minder driehoeken

`render-performance-survey.md` stelde vast dat op een echte GPU de frame het
entiteitenaantal volgt en niet de draws, pixels of schaduwen; §5.2.2 laat zien
dat de straat 9 200 entiteiten is, waarvan 4 361 grondplaten. De GPU is daar
allang klaar mee (batching, indirect draws); de renderdraad en de hoofddraad
niet.

Wat het wordt: per cel één mesh per (materiaal, groep) in plaats van één per
oppervlak — tientallen in plaats van ~485; de bytes blijven gelijk, de draws
dalen weinig (ze zijn al gebatcht), de extractie-, zichtbaarheids- en
bind-group-kosten vallen met een factor tien tot twintig. De splitsing op
4 MB (`TERRAIN_REFINE_EXPANSION`) blijft als bovengrens; ze is nu bijna nooit
bindend. Verder: één bindless-materiaal of texture-array voor de tegels (224–424
eigen `GroundMaterial`s nu; op Metal kost elke niet-bindless materiaal per
entiteit kernel-rondjes, zie de globe-meting: 373 → 107 batch-sets), en de
schaduwwerpers van de grond uit: 4 321 werpers op straatniveau terwijl paving
"casts: no" hoort te zijn (`SurfaceGroup`), een telling die eerst verklaard moet
worden (de census telt entiteiten zonder `NotShadowCaster`; welke groepen dat
zijn, zegt hij niet — §7.8).

### 6.4 Schaduw als cache

De stad staat stil en de zon draait 15° per uur; Bevy tekent elk frame elke
cascade opnieuw. Op de Deck kostte dat bij `city` niets (de meting van
2026-09-22), op de M5 Pro bij `street` 25 van 46 ms (cascade-kosten, niet
werper-kosten), op de telefoon onbekend. Wat het wordt, waar het loont: de
twee buitenste cascades elke N frames (of bij ≥0,5° zon) hertekenen, de
binnenste elk frame; bewegende dingen (verkeer, avatars) alleen in de
binnenste. Bevy 0.20 heeft er geen API voor; het is een eigen render-graph-node
in plaats van `bevy_light`'s schaduwpass, zoals dit project al wgpu,
`bevy_shader` en `big_space` vendort. Doen na een telefoonmeting, niet ervoor:
de Deck zegt nul, de Mac zegt de helft.

### 6.5 Een tijdsbudget per frame, naast het byteboek

`stream_ledger` bewaakt bytes; niets bewaakt *tijd*. Alle caps zijn aantallen
(2 cellen, 8 tegels, 4 despawns, 24 plaatsingen) gekozen op een machine, en
een aantal is de verkeerde eenheid voor dingen die drie ordes van grootte in
kosten verschillen (§3.2.1: een cel van 485 entiteiten tegen een cel van 20).

Wat het wordt: één hoofddraad-budget per frame in milliseconden (b.v. 4 ms
op desktop, 2 ms op de telefoon, gemeten met `Instant` op native en
`performance.now()` op het web), waaruit landen, spawnen, despawnen en
plaatsen putten tot het op is, in urgentievolgorde (de wachtrij van
`platform::workers` heeft die al). Een trage frame krijgt daardoor minder
werk, niet meer — het omgekeerde van nu, waar `dt` groot wordt en Rapier acht
substappen neemt. Despawns gaan het eerst (ze maken ruimte en kosten het
minst), landen daarna. Bevy's eigen `RenderAssetBytesPerFrame` blijft uit
(`upload-bursts.md` zegt waarom); dit zit vóór het aanmaken van assets, waar
het byteboek ook zit. En een byteplafond op desktop van 32–64 MB, want een
frame van een seconde is geen verlies van een device maar wel van de
gebruiker.

### 6.6 Cellen als bestanden, niet als jobs

Atlas snijdt de grond al voor (`pre-cut-ground.md`: celbouw 4,3 → 2,4 s,
werkertijd gehalveerd), gradeert het terrein en bouwt de gebouwblokken. Met
§6.2 is de client-helft van een cel nog: parse, meubilair, bomen, labels,
verkeerslijnen — allemaal functies van de tegel plus registers, zonder
apparaatafhankelijke invoer. Wat het wordt: atlas bouwt de *hele* cel per
niveau (glTF-achtig, gekwantiseerd, per laag een mesh per materiaal, met de
vectorlijnen ernaast voor routing, labels en verkeer), de client doet alleen
fetch → upload. De werkerpool krimpt tot het decoderen; de koude start wordt
bandbreedte-gebonden in plaats van CPU-gebonden; een cel kost op elke machine
hetzelfde; en het web en de telefoon krijgen dezelfde cel als de desktop. De
prijs is wat `pre-cut-ground.md` al meet: bytes op een trage mobiele link (een
dichte straatcel is 1,9 MB Brotli na de stoeprand-fix; met meubilair en bomen
erbij meer), en een "GROUND_REVISION"-discipline voor de hele cel.

### 6.7 Het verkeer als eigen laag met eigen bron

De rijlijnen zijn een pure functie van de tegel (`extract_drive_lines`), ~3 ms
tegen 3,6 s voor de celjob. Wat het wordt: een eigen lichte job per tegel op
NEAR, een eigen ring van radius 1 rond het kijkpunt (`stream_xz_at`), nooit
gewist door een bandwissel; een rooster voor `near()` (6 m-rooster bestaat al
voor voetgangers); plaatsing in beeld zolang de pool onder de helft is of net
gewist; de vroege `return` op `:2308` verbergt; Rapier's substappen gebonden
aan het tijdsbudget van §6.5 (liever één stap van `dt` dan acht die het frame
verdubbelen). Later: dezelfde lijnen uit atlas (§6.6) als `traffic/14/x/y.bin`.
Verder weg, en een ontwerpkeuze die niet hier valt: verkeer dat spelers
*delen* — nu verzint elke client zijn eigen stad, wat goed genoeg is voor "leeft
deze straat" en te weinig voor "wij staan samen bij dezelfde bus".

### 6.8 Pipelines opwarmen

Elke combinatie (materiaal × vertexlayout × `depth_bias`-rung × atmosfeer-modus)
compileert bij eerste gebruik, 50–300 ms op Adreno, 340–380 ms op de Deck, en
geometrie zonder pipeline wordt stilletjes overgeslagen. Wat het wordt: een
opwarmscène bij de start — één driehoek per combinatie, een kilometer onder de
grond, één frame — en de gecomponeerde WGSL blijft al bewaard
(`shader_store.rs`). De lijst combinaties is eindig en opsombaar (de zestien
rungen, de groepen, elf globe-niveaus, twee atmosfeer-modi); `pipeline_health.rs`
logt ze al (`?pipeline_log`). Dit verwijdert §5.2.6 en de "fade die zijn
pipeline moest afwachten" uit `near-ground-layers-at-altitude.md`.

### 6.9 Vloeiend zoomen als eigenschap van de camera

Los van alle streaming: glijd in log-afstand met een kritisch gedempte veer
(één gedeelde `glide` voor Earth, attach en de rail, zoals de rail al vliegt met
versnelling en plafonds, `zoom-ladder.md`); desktop op `AutoVsync` waar dat kan
(of de begrenzer uit de paneelfrequentie afleiden, zoals Android via
`Cadence` al doet); en de hysterese-band op 50 km en 2 km voor de wereld (de
camera heeft 55/48 km al), met een overgang in plaats van een stap voor de
atmosfeer (`space_factor`-crossfade, de regel uit CLAUDE.md, toegepast op de
LookupTexture→Raymarched-wissel).

## 7. Het pad: stappen die elk alleen landen

Elke stap heeft een maat die een machine kan beoordelen (waarde 6 van
`docs/direction.md`), en de volgorde is opbrengst per moeite. De getallen zijn
de maat van §0/§5.1, gemeten op dezelfde node vóór en na.

| # | stap | raakt | maat | moeite |
|---|---|---|---|---|
| 1 | **Vasthouden bij een bandwissel**: geen `clear()`; de oude cellen blijven getekend en `complete` tot de nieuwe geland zijn; `stores.roads` overleeft de wissel | §3.2.1, §4.2.1 | `ground_retears` blijft, maar `view_ground_s` bij aankomst en de film tonen geen bruine basiskaart meer; verkeer verdwijnt niet in het log | klein–middel: `map_geometry` kent al `retire_cell` per cel en `built_rev`; de wissel wordt "herbouw per cel, vervang bij landen" |
| 2 | **Verkeer: kijkpunt, in beeld vullen, verbergen bij lege straal, boxtest in `near`** | §4.2.2–5 | `traffic_cars` placed én een telling *in frustum* (nieuw veld); `why`-regels van `CARTO_TRAFFIC_OVERLAPS` | klein |
| 3 | **`get` vóór `get_mut` in `building_fade`** | §5.2.4 | `material_modified_per_frame` 6,8 → ~0 in stille vensters | triviaal |
| 4 | **Byteplafond op desktop (32–64 MB) en despawns in bytes of tijd, niet in aantallen** | §5.2.1, §5.2.3 | `upload_peak_kb` ≤ plafond; de climb-census leeg binnen 2 s na 500 m; slechtste frame van de descent | klein — gebouwd 2026-11-22: de despawns wel, het plafond gemeten en niet genomen (§7a) |
| 5 | **Tijdsbudget per frame op de hoofddraad** (§6.5) | §5 breed, §4.2.5 | `frame_ms_max` van descent/climb; `stalls` | middel: één module, zes aanroepers |
| 6 | **Hysterese-banden op 50 km en 2 km, atmosfeer-crossfade** | §3.2.3–4 | film: geen omslag >10 % in één interval buiten de camerabeweging | klein–middel |
| 7 | **Pipelines opwarmen** (§6.8) | §5.2.6 | `pipelines building` 0 na de start; geen frame >200 ms bij aankomst op de straat | middel |
| 8 | **Meetlat**: film-verschil als suite-kolom (`pops`, `max_step`), entiteiten per marker in `--dump-state`, schaduwwerpers per `SurfaceGroup`, een frustum-telling voor verkeer | §3.3, §6.3 | de kolommen bestaan | klein |
| 9 | **Eén mesh per cel per materiaal** (§6.3) | §5.2.2 | `MapSurface`-entiteiten 4 361 → <300; `street` steady frame | middel |
| 10 | **Zoom-glide en pacing** (§6.9) | §5.2.7 | `camera pacing`-regels: `applied_cv` tijdens een gescripte zoom | klein |
| 11 | **Het blok blijft tot de laatste plak** (`holds()` op compleet) | §3.2.6 | een `--flicker`-achtige telling op de telefoon-rung met `CARTO_UPLOAD_KB=4096` | triviaal |
| 12 | **Terrein in de vertex-shader** (§6.2) | §3.2.2, §3.2.5, alle rebuilds | `surface ring retear` en `lod22 ring retear` met `terrain_rev` verdwijnen; koude start in seconden | groot |
| 13 | **Eén boom voor alles** (§6.1) | alles | de zes drempels bestaan niet meer; `ground_retears` is per definitie 0 | groot, in stappen: grond op z14 als boomknoop eerst |
| 14 | **Cellen uit atlas** (§6.6), **schaduwcache** (§6.4, na telefoonmeting) | koude start, straat-frame | werkertijd per cel; `street` GPU-wait | groot |

Stap 1 tot 4 zijn samen een paar dagen (gebouwd dezelfde dag, §7a) en nemen
de drie klachten voor het grootste deel weg; 5 tot 11 maken het robuust en
meetbaar; 12 tot 14 zijn de
verbouwing waarop een metaverse kan staan — een wereld die per tegel streamt,
op elk apparaat dezelfde cel laadt, nooit iets weggooit voordat het vervangen
is, en per frame nooit meer doet dan het frame toelaat.

## 7a. Gebouwd op 2026-11-22: stappen 1 tot 4

Dezelfde middag, zes commits op `session/s811`, gemeten met dezelfde
scenario's op dezelfde node (ontwikkelprofiel, 1280×800, warme cache; vóór
één vlucht, ná twee, mediaan en [bereik]):

| | vóór | ná |
|---|---|---|
| afdaling 40 km → 150 m: alles rustig na aankomst | 28,5 s | 16–18 s |
| idem: grond in beeld compleet | 11,1 s | 5,5–6,4 s |
| idem: slechtste frame tijdens de zoom | 965 ms | 433 [133..733] ms |
| afdaling → 40 m: alles rustig na aankomst | 25,2 s | 16,3 s |
| klim 150 m → 200 km: gemiddeld frame tijdens de klim | 56,6 ms | 40 [35..46] ms |
| idem: alles rustig na aankomst | 25,4 s | 1,8 s |
| idem: slechtste frame na aankomst | 230 ms | 58 ms |
| draws tijdens de afdaling | 661 | ~2 200 |

- **Stap 1, vasthouden** (`map_geometry::rebuild_in_place`): een bandwissel
  die detail *toevoegt* markeert de cellen als verouderd en vraagt ze
  opnieuw; de oude cel blijft getekend en `complete` tot de laatste mesh van
  de nieuwe op de GPU staat, dan wisselt hij (`finish_swap`). Elke
  `insert_cell` van de stores is een vervanging, dus het wegennet blijft. Een
  wissel die alleen detail *wegneemt* (klimmen, een laag uit) ruimt meteen op
  zoals voorheen: de eerste versie hield ook op de klim vast en kwam op 200 km
  aan met 3 907 draws straat. Een teruggetrokken entiteit gaat het frame zelf
  van elk beeld af (`RenderLayers::none()`; `Visibility::Hidden` wordt door de
  globe elk frame teruggedraaid). De prijs staat in de laatste rij: tijdens de
  zoom wordt de oude ring getekend in plaats van weggegooid.
- **Stap 2, verkeer**: middelpunt op `stream_xz_at` (het oog alleen nog voor
  "uit het zicht", het detailniveau en het ongezien vertrekken), verbergen
  als geen weg binnen bereik is, en de boxtest in `near()`.
- **Stap 3**: `get` vóór `get_mut` in `building_fade`.
- **Stap 4, half**: een despawn-achterstand loopt binnen 30 frames leeg,
  hoe groot ook (`platform::stream::DRAIN_WITHIN_FRAMES`); dat is de klim-rij
  hierboven. **Het desktop-byteplafond is gemeten en niet genomen**: 64 MB per
  frame tegen geen plafond, twee vluchten elk, bewoog het slechtste frame van
  de afdaling niet in zijn voordeel (560 tegen 433 ms) en het gemiddelde
  evenmin; §5.2.1's redenering hield op deze machine niet. De eerste toelating
  van een frame gaat altijd heel door, dus de grote frames bij de start
  (900–1 100 ms) raakt een plafond toch niet; wat die nodig hebben is het
  tijdsbudget van §6.5.

Niet veranderd: de 2 km-omslag (§3.2.4), het gemiddelde frame van de
afdaling zelf (71 vóór, 55–83 ná, binnen de spreiding van deze VM) en alles
vanaf stap 5.

## 8. Wat niet gemeten is, en hoe wel

- **De telefoon.** Het toestel was niet verbonden. De `perf`- en
  `diagnostics`-tools van de midas-koppeling meten het in de hand; de
  `descent`-scenario's zijn op Android niet bereikbaar (geen CLI), dus daar
  blijft het: `debug.carto.fps_log 1` en een zoom met de hand. De
  balanced-rung heeft het 4 MB-plafond en `surface_jobs 2`, dus §5.2.1 geldt
  daar niet en §5.2.2–3 des te meer (de renderdraad deelt één thermisch budget
  met alles).
- **De browser.** Eén thread; `wasm-performance.md` meet een vlucht met
  `cdp_probe --drive-script flight`. Stappen 1, 4, 5 en 9 raken daar het hardst.
- **Pops van één frame op 60 fps.** De film op 15 beelden/s mist ze
  driekwart van de tijd; `--video-fps 60` wacht op de wereld en ziet ze dus
  niet. Een echte maat is een per-frame-verschil in de renderer zelf (het
  vorige frame als textuur, één compute-pass, één getal in `ShotMetrics`), die
  ook op het toestel draait.
- **Het schaduwwerper-aantal op de grond** (4 321): per `SurfaceGroup`
  uitsplitsen voordat iemand §6.4 bouwt.
